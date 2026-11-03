//! Optional SQLite backend for task checkpoints (BACKLOG P1-03).
//!
//! Semantically equivalent to the default [`FileCheckpointStore`]: same
//! [`CheckpointStore`] trait, same validation rules (root must be UTF-8,
//! state must match `[A-Za-z0-9_-]+`, strict decoding), atomic `save`, `list`
//! sorted by job id and skipping corrupt rows, `remove` reporting presence.
//!
//! Which backend when:
//!
//! - **File backend** (default, no feature flag): the LAN prototype default.
//!   Zero non-Rust build dependencies, one human-readable `AKJOB1` file per
//!   job, trivial to inspect and copy.
//! - **SQLite backend** (this module, `sqlite` feature): for the mobile apps
//!   (P0-06) and hosts running many concurrent jobs, where a single database
//!   file with WAL and `busy_timeout` handles multiple connections better
//!   than per-file renames, and where a bundled C SQLite build is acceptable.
//!
//! Deliberate differences from the file backend (both benign for the engine,
//! which addresses files by index):
//!
//! - `files` are returned ordered by `file_index`; the file backend preserves
//!   insertion order.
//! - Duplicate `file_index` values within one record are rejected (primary
//!   key on the child table); the file backend tolerates them.
//! - `u64` fields above `i64::MAX` are rejected on save (SQLite INTEGER
//!   range); the file backend stores arbitrary `u64` as decimal text.
//!
//! PROTOCOL ordering note: same rule as the file backend applies — this
//! module only provides the atomic save (transaction commit); any completion
//! marker must be written strictly after final verification and this commit.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ak_identity::Fingerprint;
use rusqlite::{params, Connection, Row, TransactionBehavior};

use crate::{
    invalid_data, valid_state, CheckpointStore, Direction, FileCheckpoint, JobIdBytes, TaskRecord,
};

/// `direction` column code for [`Direction::Send`].
const DIRECTION_SEND: i64 = 0;
/// `direction` column code for [`Direction::Receive`].
const DIRECTION_RECEIVE: i64 = 1;

/// `peer_fp` column stores the raw fingerprint bytes or NULL for "no peer".
const PEER_FP_LEN: usize = 32;

/// How long a competing writer waits before failing. WAL plus IMMEDIATE
/// write transactions keep contention short.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS jobs (
    job_id      BLOB PRIMARY KEY,
    direction   INTEGER NOT NULL CHECK (direction IN (0, 1)),
    root        TEXT NOT NULL,
    peer_fp     BLOB CHECK (peer_fp IS NULL OR length(peer_fp) = 32),
    chunk_size  INTEGER NOT NULL,
    total_files INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL,
    state       TEXT NOT NULL,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS job_files (
    job_id     BLOB NOT NULL,
    file_index INTEGER NOT NULL,
    size       INTEGER NOT NULL,
    prefix     INTEGER NOT NULL,
    PRIMARY KEY (job_id, file_index)
) WITHOUT ROWID;";

/// SQLite-backed checkpoint store: one database file, a `jobs` table plus a
/// `job_files` child table, WAL journal, `busy_timeout`, and an atomic
/// delete-then-insert `save` inside one transaction.
pub struct SqliteCheckpointStore {
    conn: Mutex<Connection>,
}

impl SqliteCheckpointStore {
    /// Create or open the database file at `path` (missing parent directories
    /// are created, mirroring [`crate::FileCheckpointStore::open`]), enable
    /// WAL and `busy_timeout`, and create the tables if absent.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path).map_err(sqlite_err)?;
        // `PRAGMA journal_mode` returns the resulting mode row, so it must be
        // read as a query; execute()/pragma_update() would reject the row.
        let _mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(sqlite_err)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA).map_err(sqlite_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl CheckpointStore for SqliteCheckpointStore {
    /// Persist atomically: one IMMEDIATE transaction deletes any previous
    /// rows for the job, validates the record, and re-inserts it. Validation
    /// runs after the deletes on purpose — a rejected record rolls the
    /// deletes back, so the previous checkpoint (if any) stays intact and a
    /// failed save can never leave a half-written job behind.
    ///
    /// PROTOCOL ordering note: the commit below is the save acknowledgement;
    /// any "completed" marker must be written strictly after it.
    fn save(&self, record: &TaskRecord) -> io::Result<()> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_err)?;
        tx.execute(
            "DELETE FROM job_files WHERE job_id = ?1",
            [record.job_id.as_slice()],
        )
        .map_err(sqlite_err)?;
        tx.execute(
            "DELETE FROM jobs WHERE job_id = ?1",
            [record.job_id.as_slice()],
        )
        .map_err(sqlite_err)?;

        let root = record
            .root
            .to_str()
            .ok_or_else(|| invalid_data("root path is not valid UTF-8"))?;
        if !valid_state(&record.state) {
            return Err(invalid_data(
                "state must match [A-Za-z0-9_-]+ and be non-empty",
            ));
        }
        let direction = match record.direction {
            Direction::Send => DIRECTION_SEND,
            Direction::Receive => DIRECTION_RECEIVE,
        };
        let peer = record
            .peer_fingerprint
            .as_ref()
            .map(|fp| fp.as_bytes().as_slice());
        let created = to_i64(record.created_unix_ms, "created time")?;
        let updated = to_i64(record.updated_unix_ms, "updated time")?;
        let total_files = to_i64(record.total_files, "total files")?;
        let total_bytes = to_i64(record.total_bytes, "total bytes")?;

        tx.execute(
            "INSERT INTO jobs (job_id, direction, root, peer_fp, chunk_size, total_files, \
             total_bytes, state, created, updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.job_id.as_slice(),
                direction,
                root,
                peer,
                i64::from(record.chunk_size),
                total_files,
                total_bytes,
                record.state.as_str(),
                created,
                updated,
            ],
        )
        .map_err(sqlite_err)?;

        for file in &record.files {
            tx.execute(
                "INSERT INTO job_files (job_id, file_index, size, prefix) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.job_id.as_slice(),
                    i64::from(file.file_index),
                    to_i64(file.size, "file size")?,
                    to_i64(file.verified_prefix_chunks, "file verified prefix")?,
                ],
            )
            .map_err(sqlite_err)?;
        }

        tx.commit().map_err(sqlite_err)?;
        Ok(())
    }

    /// Strict load: an absent job is `None`; a row that exists but fails
    /// validation (same rules as the file parser) is an error.
    fn load(&self, job_id: &JobIdBytes) -> io::Result<Option<TaskRecord>> {
        let conn = lock_conn(&self.conn);
        let row = conn.query_row(
            "SELECT direction, root, peer_fp, chunk_size, total_files, total_bytes, state, \
             created, updated FROM jobs WHERE job_id = ?1",
            [job_id.as_slice()],
            |row| read_job_row_at(row, 0),
        );
        let job = match row {
            Ok(job) => job,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(sqlite_err(e)),
        };
        let files = read_files(&conn, job_id)?;
        Ok(Some(decode_job(job_id, job, files)?))
    }

    /// Lists all valid records sorted by job id. Corrupt or unreadable rows
    /// are skipped; they never fail the whole listing (same rule as the file
    /// backend skipping unreadable `.akjob` files).
    fn list(&self) -> io::Result<Vec<TaskRecord>> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT job_id, direction, root, peer_fp, chunk_size, total_files, \
                 total_bytes, state, created, updated FROM jobs ORDER BY job_id",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, read_job_row_at(row, 1)?))
            })
            .map_err(sqlite_err)?;
        let mut records = Vec::new();
        for row in rows {
            let Ok((job_id_bytes, job)) = row else {
                continue;
            };
            let Ok(job_id) = JobIdBytes::try_from(job_id_bytes.as_slice()) else {
                continue;
            };
            let Ok(files) = read_files(&conn, &job_id) else {
                continue;
            };
            let Ok(record) = decode_job(&job_id, job, files) else {
                continue;
            };
            records.push(record);
        }
        Ok(records)
    }

    /// Deletes the job row and its file rows in one transaction; returns
    /// false if no job row existed.
    fn remove(&self, job_id: &JobIdBytes) -> io::Result<bool> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_err)?;
        tx.execute(
            "DELETE FROM job_files WHERE job_id = ?1",
            [job_id.as_slice()],
        )
        .map_err(sqlite_err)?;
        let removed = tx
            .execute("DELETE FROM jobs WHERE job_id = ?1", [job_id.as_slice()])
            .map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)?;
        Ok(removed == 1)
    }
}

/// Lock the connection, recovering from poisoning: a panic mid-transaction
/// still rolls back on drop, so the connection stays consistent.
fn lock_conn(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sqlite_err(error: rusqlite::Error) -> io::Error {
    io::Error::other(error)
}

/// SQLite INTEGER is signed 64-bit; values above `i64::MAX` cannot round-trip
/// and are rejected up front.
fn to_i64(value: u64, what: &str) -> io::Result<i64> {
    i64::try_from(value)
        .map_err(|_| invalid_data(format!("{what} {value} exceeds SQLite INTEGER range")))
}

/// Reject negative values read back from the database.
fn u64_or_invalid(value: i64, what: &str) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid_data(what))
}

/// Owned copy of the nine non-key `jobs` columns (direction..updated);
/// see [`read_job_row_at`] for the two SELECT layouts.
struct JobRow {
    direction: i64,
    root: String,
    peer_fp: Option<Vec<u8>>,
    chunk_size: i64,
    total_files: i64,
    total_bytes: i64,
    state: String,
    created: i64,
    updated: i64,
}

/// Read the nine job columns starting at `start` (0 for the `load` query,
/// 1 for the `list` query whose first column is `job_id`).
fn read_job_row_at(row: &Row<'_>, start: usize) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        direction: row.get(start)?,
        root: row.get(start + 1)?,
        peer_fp: row.get(start + 2)?,
        chunk_size: row.get(start + 3)?,
        total_files: row.get(start + 4)?,
        total_bytes: row.get(start + 5)?,
        state: row.get(start + 6)?,
        created: row.get(start + 7)?,
        updated: row.get(start + 8)?,
    })
}

/// Read the child rows of one job, ordered by file index (see module docs on
/// the ordering difference to the file backend).
fn read_files(conn: &Connection, job_id: &JobIdBytes) -> io::Result<Vec<FileCheckpoint>> {
    let mut stmt = conn
        .prepare(
            "SELECT file_index, size, prefix FROM job_files WHERE job_id = ?1 \
             ORDER BY file_index",
        )
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([job_id.as_slice()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(sqlite_err)?;
    let mut files = Vec::new();
    for row in rows {
        let (index, size, prefix) = row.map_err(sqlite_err)?;
        files.push(FileCheckpoint {
            file_index: u32::try_from(index)
                .map_err(|_| invalid_data("file index out of u32 range"))?,
            size: u64_or_invalid(size, "negative file size")?,
            verified_prefix_chunks: u64_or_invalid(prefix, "negative verified prefix")?,
        });
    }
    Ok(files)
}

/// Validate a row against the same rules as the strict file parser and build
/// the record. `job_id` comes from the caller (the key it queried with).
fn decode_job(
    job_id: &JobIdBytes,
    row: JobRow,
    files: Vec<FileCheckpoint>,
) -> io::Result<TaskRecord> {
    let direction = match row.direction {
        DIRECTION_SEND => Direction::Send,
        DIRECTION_RECEIVE => Direction::Receive,
        other => return Err(invalid_data(format!("bad direction code {other}"))),
    };
    let peer_fingerprint = match row.peer_fp {
        None => None,
        Some(bytes) => {
            if bytes.len() != PEER_FP_LEN {
                return Err(invalid_data(format!(
                    "peer fingerprint must be {PEER_FP_LEN} bytes, got {}",
                    bytes.len()
                )));
            }
            let mut fp = [0u8; PEER_FP_LEN];
            fp.copy_from_slice(&bytes);
            Some(Fingerprint::from_bytes(fp))
        }
    };
    if !valid_state(&row.state) {
        return Err(invalid_data(
            "state must match [A-Za-z0-9_-]+ and be non-empty",
        ));
    }
    Ok(TaskRecord {
        job_id: *job_id,
        direction,
        root: PathBuf::from(row.root),
        peer_fingerprint,
        chunk_size: u32::try_from(row.chunk_size)
            .map_err(|_| invalid_data("chunk size out of u32 range"))?,
        total_files: u64_or_invalid(row.total_files, "negative total files")?,
        total_bytes: u64_or_invalid(row.total_bytes, "negative total bytes")?,
        state: row.state,
        created_unix_ms: u64_or_invalid(row.created, "negative created time")?,
        updated_unix_ms: u64_or_invalid(row.updated, "negative updated time")?,
        files,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::hex_encode;

    fn db_under(dir: &Path) -> PathBuf {
        dir.join("jobs.db")
    }

    fn store(db: &Path) -> SqliteCheckpointStore {
        SqliteCheckpointStore::open(db).expect("open store")
    }

    fn job(byte: u8) -> JobIdBytes {
        [byte; 16]
    }

    fn fp(byte: u8) -> Fingerprint {
        Fingerprint::from_bytes([byte; 32])
    }

    fn sample(job_id: JobIdBytes, direction: Direction, peer: Option<Fingerprint>) -> TaskRecord {
        TaskRecord {
            job_id,
            direction,
            root: PathBuf::from(r"D:\data\send dir"),
            peer_fingerprint: peer,
            chunk_size: 262_144,
            total_files: 3,
            total_bytes: 10_000_000,
            state: "in_progress-1".to_string(),
            created_unix_ms: 1_730_000_000_000,
            updated_unix_ms: 1_730_000_500_000,
            files: vec![
                FileCheckpoint {
                    file_index: 0,
                    size: 4_000_000,
                    verified_prefix_chunks: 15,
                },
                FileCheckpoint {
                    file_index: 1,
                    size: 4_000_000,
                    verified_prefix_chunks: 0,
                },
                FileCheckpoint {
                    file_index: 2,
                    size: 2_000_000,
                    verified_prefix_chunks: 2,
                },
            ],
        }
    }

    /// Apply raw SQL through a second connection to corrupt or inspect the
    /// database behind the store's back.
    fn raw(db: &Path, sql: &str) {
        Connection::open(db)
            .expect("open raw connection")
            .execute_batch(sql)
            .expect("raw sql");
    }

    fn scalar(db: &Path, sql: &str) -> i64 {
        Connection::open(db)
            .expect("open raw connection")
            .query_row(sql, [], |row| row.get(0))
            .expect("scalar query")
    }

    #[test]
    fn save_load_roundtrip_with_peer_and_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(&db_under(dir.path()));
        let record = sample(job(7), Direction::Send, Some(fp(9)));
        s.save(&record).expect("save");
        let loaded = s.load(&job(7)).expect("load").expect("record present");
        assert_eq!(loaded, record);
        assert_eq!(loaded.verified_chunks(), 17);
    }

    #[test]
    fn save_load_roundtrip_receive_no_peer_no_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(&db_under(dir.path()));
        let mut record = sample(job(1), Direction::Receive, None);
        record.files.clear();
        s.save(&record).expect("save");
        assert_eq!(s.load(&job(1)).expect("load"), Some(record));
    }

    #[test]
    fn load_missing_job_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(&db_under(dir.path()));
        assert_eq!(s.load(&job(42)).expect("load"), None);
    }

    #[test]
    fn list_is_sorted_and_skips_corrupt_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let s = store(&db);
        for byte in [9u8, 1, 5] {
            s.save(&sample(job(byte), Direction::Send, None))
                .expect("save");
        }
        // Wrong-length job_id, non-UTF-8 root blob, and a negative counter
        // must all be skipped, mirroring the file backend's skip rules.
        raw(
            &db,
            "INSERT INTO jobs (job_id, direction, root, peer_fp, chunk_size, total_files, \
             total_bytes, state, created, updated) \
             VALUES (x'00', 0, 'C:/r', NULL, 4096, 0, 0, 'ok', 0, 0); \
             INSERT INTO jobs (job_id, direction, root, peer_fp, chunk_size, total_files, \
             total_bytes, state, created, updated) \
             VALUES (x'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', 0, x'8081', NULL, 4096, 0, 0, 'ok', 0, 0); \
             INSERT INTO jobs (job_id, direction, root, peer_fp, chunk_size, total_files, \
             total_bytes, state, created, updated) \
             VALUES (x'cccccccccccccccccccccccccccccccc', 0, 'C:/r', NULL, 4096, -1, 0, 'ok', 0, 0);",
        );

        let listed = s.list().expect("list");
        let ids: Vec<JobIdBytes> = listed.iter().map(|r| r.job_id).collect();
        assert_eq!(ids, vec![job(1), job(5), job(9)]);
    }

    #[test]
    fn remove_reports_presence_and_cleans_child_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let s = store(&db);
        s.save(&sample(job(3), Direction::Send, None))
            .expect("save");
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM job_files"), 3);
        assert!(s.remove(&job(3)).expect("remove"));
        assert!(!s.remove(&job(3)).expect("remove again"));
        assert_eq!(s.load(&job(3)).expect("load"), None);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM job_files"), 0);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM jobs"), 0);
    }

    #[test]
    fn concurrent_save_same_job_across_connections_stays_consistent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let a = std::sync::Arc::new(store(&db));
        // A second independent connection exercises WAL + busy_timeout.
        let b = SqliteCheckpointStore::open(&db).expect("open second store");

        let mut rec_a = sample(job(4), Direction::Send, None);
        rec_a.state = "writing-a".to_string();
        let mut rec_b = rec_a.clone();
        rec_b.state = "writing-b".to_string();
        rec_b.files.pop(); // differing file counts prove no cross-transaction mixing

        let sa = std::sync::Arc::clone(&a);
        let ra = rec_a.clone();
        let handle_a = std::thread::spawn(move || sa.save(&ra));
        let rb = rec_b.clone();
        let handle_b = std::thread::spawn(move || b.save(&rb));
        handle_a.join().expect("thread a").expect("save a");
        handle_b.join().expect("thread b").expect("save b");

        let loaded = a.load(&job(4)).expect("load").expect("present");
        assert!(
            loaded == rec_a || loaded == rec_b,
            "torn record: {loaded:?}"
        );
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM jobs"), 1);
    }

    #[test]
    fn unicode_and_space_paths_roundtrip() {
        let outer = tempfile::tempdir().expect("tempdir");
        // Chinese + space directory and file name; open() must create it.
        let db = outer.path().join("检查点 目录").join("任务 库.db");
        let s = store(&db);
        let mut record = sample(job(11), Direction::Receive, Some(fp(2)));
        record.root = PathBuf::from("E:/接收 文件/新 文件夹");
        s.save(&record).expect("save");
        assert_eq!(s.load(&job(11)).expect("load"), Some(record.clone()));
        assert_eq!(s.list().expect("list"), vec![record]);
    }

    #[test]
    fn reopen_preserves_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let record = sample(job(6), Direction::Send, Some(fp(8)));
        {
            let s = store(&db);
            s.save(&record).expect("save");
        }
        // Restart recovery: a fresh open on the same file sees the record.
        let s = SqliteCheckpointStore::open(&db).expect("reopen");
        assert_eq!(s.load(&job(6)).expect("load"), Some(record.clone()));
        assert_eq!(s.list().expect("list"), vec![record]);
    }

    #[test]
    fn corrupt_rows_fail_load_strictly() {
        let variants: Vec<(&str, &str)> = vec![
            ("bad state charset", "UPDATE jobs SET state = 'has space'"),
            ("root not utf-8", "UPDATE jobs SET root = x'80ff'"),
            ("negative total bytes", "UPDATE jobs SET total_bytes = -1"),
            (
                "chunk size over u32",
                "UPDATE jobs SET chunk_size = 4294967296",
            ),
            ("negative created", "UPDATE jobs SET created = -1"),
            ("negative file size", "UPDATE job_files SET size = -1"),
            (
                "file index over u32",
                "UPDATE job_files SET file_index = 4294967296 WHERE file_index = 0",
            ),
        ];
        for (what, sql) in variants {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = db_under(dir.path());
            let s = store(&db);
            s.save(&sample(job(21), Direction::Send, Some(fp(3))))
                .expect("save");
            raw(&db, sql);
            assert!(s.load(&job(21)).is_err(), "expected failure for {what}");
            assert!(
                s.list().expect("list").is_empty(),
                "expected skip for {what}"
            );
        }
    }

    #[test]
    fn save_rejects_invalid_state_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let s = store(&db);
        let mut record = sample(job(31), Direction::Send, None);
        record.state = "not allowed!".to_string();
        assert!(
            s.save(&record).is_err(),
            "state with spaces must be rejected"
        );
        assert_eq!(s.load(&job(31)).expect("load"), None);
        assert!(s.list().expect("list").is_empty());
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM jobs"), 0);
        assert_eq!(scalar(&db, "SELECT COUNT(*) FROM job_files"), 0);
    }

    #[test]
    fn failed_save_mid_transaction_preserves_previous_checkpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let s = store(&db);
        let good = sample(job(5), Direction::Send, Some(fp(1)));
        s.save(&good).expect("seed");

        // Invalid state fails after the transaction already ran its deletes;
        // the rollback must leave the previous checkpoint untouched.
        let mut bad_state = sample(job(5), Direction::Send, None);
        bad_state.state = "not allowed!".to_string();
        assert!(s.save(&bad_state).is_err());
        assert_eq!(s.load(&job(5)).expect("load"), Some(good.clone()));

        // A duplicate file index violates the job_files primary key
        // mid-transaction; same rollback guarantee.
        let mut dup = sample(job(5), Direction::Send, None);
        dup.files[1].file_index = dup.files[0].file_index;
        assert!(
            s.save(&dup).is_err(),
            "duplicate file index must be rejected"
        );
        assert_eq!(s.load(&job(5)).expect("load"), Some(good.clone()));
        assert_eq!(s.list().expect("list"), vec![good]);
    }

    #[cfg(windows)]
    #[test]
    fn save_rejects_non_utf8_root() {
        use std::os::windows::ffi::OsStringExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(&db_under(dir.path()));
        let mut record = sample(job(41), Direction::Send, None);
        // Unpaired surrogate: representable in OsString, not in UTF-8.
        record.root = PathBuf::from(std::ffi::OsString::from_wide(&[0xD800]));
        assert!(s.save(&record).is_err(), "non-UTF-8 root must be rejected");
        assert!(s.list().expect("list").is_empty());
    }

    #[test]
    fn saved_rows_match_expected_schema_encoding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db_under(dir.path());
        let s = store(&db);
        let record = sample(job(77), Direction::Receive, Some(fp(4)));
        s.save(&record).expect("save");
        // Spot-check the on-disk encoding: BLOB keys and fingerprint, peer
        // NULL vs 32 bytes, and the child-row count.
        assert_eq!(
            scalar(
                &db,
                &format!(
                    "SELECT COUNT(*) FROM jobs WHERE job_id = x'{}' AND direction = 1 \
                     AND root = 'D:\\data\\send dir' AND peer_fp = x'{}' \
                     AND chunk_size = 262144 AND total_files = 3 AND total_bytes = 10000000 \
                     AND state = 'in_progress-1' AND created = 1730000000000 \
                     AND updated = 1730000500000",
                    hex_encode(&job(77)),
                    hex_encode(fp(4).as_bytes()),
                )
            ),
            1
        );
        assert_eq!(
            scalar(
                &db,
                &format!(
                    "SELECT COUNT(*) FROM job_files WHERE job_id = x'{}' AND file_index = 0 \
                     AND size = 4000000 AND prefix = 15",
                    hex_encode(&job(77))
                )
            ),
            1
        );
    }
}
