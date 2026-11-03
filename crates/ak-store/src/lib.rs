//! Task checkpoint persistence.
//!
//! v1 is a line-based file store with atomic writes (write temp, fsync,
//! rename). An optional SQLite backend (BACKLOG P1-03, `sqlite` feature,
//! [`sqlite::SqliteCheckpointStore`]) implements the same trait with the same
//! semantics; the engine only sees [`CheckpointStore`].
//!
//! Rule from PROTOCOL: a completed marker is written strictly after final
//! verification and save commit, never before.
//!
//! File format (version header `AKJOB1`, one record per file, LF line
//! endings, fixed field order, then zero or more file lines):
//!
//! ```text
//! AKJOB1
//! job <32 hex>
//! dir send|receive
//! root <UTF-8 hex of the root path>
//! peer <64 hex fingerprint or ->
//! chunk <u32>
//! total <files u64> <bytes u64>
//! state <[A-Za-z0-9_-]+>
//! created <unix ms u64>
//! updated <unix ms u64>
//! file <index u32> <size u64> <verified prefix chunks u64>
//! ```
//!
//! Parsing is strict: an unknown line, a missing field, or bad hex makes the
//! whole record fail to load.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ak_identity::Fingerprint;

pub type JobIdBytes = [u8; 16];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

/// Per-file resume state: verified contiguous prefix, in chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileCheckpoint {
    pub file_index: u32,
    pub size: u64,
    pub verified_prefix_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub job_id: JobIdBytes,
    pub direction: Direction,
    /// Source root (send) or destination root (receive).
    pub root: PathBuf,
    pub peer_fingerprint: Option<Fingerprint>,
    pub chunk_size: u32,
    pub total_files: u64,
    pub total_bytes: u64,
    pub state: String,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub files: Vec<FileCheckpoint>,
}

impl TaskRecord {
    /// Sum of verified chunks across files (progress + resume accounting).
    pub fn verified_chunks(&self) -> u64 {
        self.files.iter().map(|f| f.verified_prefix_chunks).sum()
    }
}

pub trait CheckpointStore: Send + Sync {
    /// Persist atomically; readers never observe a torn record.
    fn save(&self, record: &TaskRecord) -> io::Result<()>;
    fn load(&self, job_id: &JobIdBytes) -> io::Result<Option<TaskRecord>>;
    fn list(&self) -> io::Result<Vec<TaskRecord>>;
    /// Remove after successful completion and commit; returns false if absent.
    fn remove(&self, job_id: &JobIdBytes) -> io::Result<bool>;
}

/// Optional SQLite backend (BACKLOG P1-03); semantics match the file backend.
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteCheckpointStore;

/// File-per-job store under a directory, files named `<job hex>.akjob`.
pub struct FileCheckpointStore {
    dir: PathBuf,
}

const HEADER: &str = "AKJOB1";
const EXTENSION: &str = "akjob";
const JOB_HEX_LEN: usize = 16 * 2;

/// Distinguishes concurrent temp files within one process.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl FileCheckpointStore {
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    fn path_for(&self, job_id: &JobIdBytes) -> PathBuf {
        self.dir
            .join(format!("{}.{}", hex_encode(job_id), EXTENSION))
    }
}

impl CheckpointStore for FileCheckpointStore {
    /// Persist atomically: write a temp file in the same directory, fsync it,
    /// then rename over the final name (see [`rename_replace`] for the
    /// Windows overwrite handling).
    ///
    /// PROTOCOL ordering note: this crate only provides the atomic write
    /// (temp file -> durable -> record -> acknowledge). The "completed"
    /// marker semantics live in the engine, which must write any completion
    /// marker strictly after final verification and this save's commit.
    fn save(&self, record: &TaskRecord) -> io::Result<()> {
        let text = encode_record(record)?;
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        // Unique per process and per call, so concurrent saves of the same
        // job never share a temp file. Extension `tmp` keeps it out of list().
        let tmp = self.dir.join(format!(
            "{}.{}.{}.{}.tmp",
            hex_encode(&record.job_id),
            std::process::id(),
            seq,
            EXTENSION
        ));
        let final_path = self.path_for(&record.job_id);
        let result =
            write_durable(&tmp, text.as_bytes()).and_then(|()| rename_replace(&tmp, &final_path));
        if result.is_err() {
            // Best-effort cleanup so a failed save leaves no temp residue.
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn load(&self, job_id: &JobIdBytes) -> io::Result<Option<TaskRecord>> {
        let path = self.path_for(job_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let record = decode_record(&text)?;
        if &record.job_id != job_id {
            return Err(invalid_data(
                "checkpoint job id does not match its file name",
            ));
        }
        Ok(Some(record))
    }

    /// Lists all parsable records sorted by job id. Corrupt or unreadable
    /// files are skipped; they never fail the whole listing.
    fn list(&self) -> io::Result<Vec<TaskRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue;
            }
            // Skip files whose name is not `<job hex>.akjob` to begin with.
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(name_job) = hex_decode_fixed::<16>(stem) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(record) = decode_record(&text) else {
                continue;
            };
            if record.job_id != name_job {
                continue;
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.job_id);
        Ok(records)
    }

    fn remove(&self, job_id: &JobIdBytes) -> io::Result<bool> {
        match fs::remove_file(self.path_for(job_id)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Current time in Unix milliseconds (single source for record timestamps).
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write and fsync so the rename below can never publish a torn file.
fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Rename `tmp` over `final_path`, removing a pre-existing target first.
///
/// On Windows, rename over an existing file can fail, so the old checkpoint
/// is removed and the rename retried. That remove-then-rename gap is not
/// atomic: a crash in between leaves no checkpoint file at all. This is
/// accepted on purpose -- the engine-side PROTOCOL rule ("completion marker
/// strictly after final verification") is what keeps the window safe, since
/// a missing checkpoint only degrades to re-transfer, never to a false
/// "completed" state.
fn rename_replace(tmp: &Path, final_path: &Path) -> io::Result<()> {
    if fs::rename(tmp, final_path).is_ok() {
        return Ok(());
    }
    match fs::remove_file(final_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::rename(tmp, final_path)
}

/// Invalid-data error shared by both backends (same kind the strict file
/// parser produces).
pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// State charset rule `[A-Za-z0-9_-]+`, shared by both backends.
pub(crate) fn valid_state(state: &str) -> bool {
    !state.is_empty()
        && state
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn encode_record(record: &TaskRecord) -> io::Result<String> {
    let root = record
        .root
        .to_str()
        .ok_or_else(|| invalid_data("root path is not valid UTF-8"))?;
    if !valid_state(&record.state) {
        return Err(invalid_data(
            "state must match [A-Za-z0-9_-]+ and be non-empty",
        ));
    }
    let mut out = String::with_capacity(192 + record.files.len() * 32);
    out.push_str(HEADER);
    out.push('\n');
    out.push_str("job ");
    out.push_str(&hex_encode(&record.job_id));
    out.push('\n');
    out.push_str("dir ");
    out.push_str(match record.direction {
        Direction::Send => "send",
        Direction::Receive => "receive",
    });
    out.push('\n');
    out.push_str("root ");
    out.push_str(&hex_encode(root.as_bytes()));
    out.push('\n');
    out.push_str("peer ");
    match &record.peer_fingerprint {
        Some(fp) => out.push_str(&hex_encode(fp.as_bytes())),
        None => out.push('-'),
    }
    out.push('\n');
    out.push_str("chunk ");
    out.push_str(&record.chunk_size.to_string());
    out.push('\n');
    out.push_str("total ");
    out.push_str(&record.total_files.to_string());
    out.push(' ');
    out.push_str(&record.total_bytes.to_string());
    out.push('\n');
    out.push_str("state ");
    out.push_str(&record.state);
    out.push('\n');
    out.push_str("created ");
    out.push_str(&record.created_unix_ms.to_string());
    out.push('\n');
    out.push_str("updated ");
    out.push_str(&record.updated_unix_ms.to_string());
    out.push('\n');
    for f in &record.files {
        out.push_str("file ");
        out.push_str(&f.file_index.to_string());
        out.push(' ');
        out.push_str(&f.size.to_string());
        out.push(' ');
        out.push_str(&f.verified_prefix_chunks.to_string());
        out.push('\n');
    }
    Ok(out)
}

/// Line cursor for strict, fixed-order parsing.
struct Lines<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut lines: Vec<&'a str> = text.split('\n').collect();
        // Tolerate exactly the trailing newline every save writes.
        if lines.last() == Some(&"") {
            lines.pop();
        }
        Self { lines, pos: 0 }
    }

    /// Consume the next line, requiring it to be `<key> <value>`.
    fn field(&mut self, key: &str) -> io::Result<&'a str> {
        let line = self
            .lines
            .get(self.pos)
            .ok_or_else(|| invalid_data(format!("record ended early, expected '{key}' line")))?;
        let Some(value) = strip_key(line, key) else {
            return Err(invalid_data(format!(
                "expected '{key}' line, got '{}'",
                preview(line)
            )));
        };
        self.pos += 1;
        Ok(value)
    }

    /// Consume and parse `file <index> <size> <prefix>` lines while present.
    fn files(&mut self) -> io::Result<Vec<FileCheckpoint>> {
        let mut files = Vec::new();
        while let Some(line) = self.lines.get(self.pos) {
            if !line.starts_with("file ") {
                break;
            }
            let rest = &line["file ".len()..];
            let mut parts = rest.split(' ');
            let index = parse_num(parts.next(), "file index", line)?;
            let size = parse_num(parts.next(), "file size", line)?;
            let prefix = parse_num(parts.next(), "file prefix", line)?;
            if parts.next().is_some() {
                return Err(invalid_data(format!(
                    "extra field on file line '{}'",
                    preview(line)
                )));
            }
            files.push(FileCheckpoint {
                file_index: index,
                size,
                verified_prefix_chunks: prefix,
            });
            self.pos += 1;
        }
        Ok(files)
    }

    /// Every line must have been consumed; anything left is unknown.
    fn finish(&self) -> io::Result<()> {
        match self.lines.get(self.pos) {
            None => Ok(()),
            Some(line) => Err(invalid_data(format!("unknown line '{}'", preview(line)))),
        }
    }
}

fn strip_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key)?.strip_prefix(' ')
}

/// Char-boundary-safe snippet for error messages (a corrupt file may hold
/// arbitrary bytes decoded as lossy text).
fn preview(line: &str) -> String {
    line.chars().take(48).collect()
}

fn parse_num<T: std::str::FromStr>(raw: Option<&str>, what: &str, line: &str) -> io::Result<T> {
    let text =
        raw.ok_or_else(|| invalid_data(format!("missing {what} on line '{}'", preview(line))))?;
    text.parse::<T>()
        .map_err(|_| invalid_data(format!("bad {what} '{text}' on line '{}'", preview(line))))
}

fn decode_record(text: &str) -> io::Result<TaskRecord> {
    let mut lines = Lines::new(text);
    let header = lines
        .lines
        .first()
        .copied()
        .ok_or_else(|| invalid_data("empty checkpoint file"))?;
    if header != HEADER {
        return Err(invalid_data(format!(
            "unknown checkpoint version '{}'",
            preview(header)
        )));
    }
    lines.pos = 1;

    let job_hex = lines.field("job")?;
    let job_id = hex_decode_fixed::<16>(job_hex).ok_or_else(|| {
        invalid_data(format!("bad job id hex (expected {JOB_HEX_LEN} hex chars)"))
    })?;

    let dir = lines.field("dir")?;
    let direction = match dir {
        "send" => Direction::Send,
        "receive" => Direction::Receive,
        other => return Err(invalid_data(format!("bad direction '{other}'"))),
    };

    let root_hex = lines.field("root")?;
    let root_bytes = hex_decode(root_hex).ok_or_else(|| invalid_data("bad root hex"))?;
    let root_str =
        String::from_utf8(root_bytes).map_err(|_| invalid_data("root is not valid UTF-8"))?;

    let peer = lines.field("peer")?;
    let peer_fingerprint = if peer == "-" {
        None
    } else {
        Some(
            hex_decode_fixed::<32>(peer)
                .map(Fingerprint::from_bytes)
                .ok_or_else(|| invalid_data("bad peer fingerprint hex"))?,
        )
    };

    let chunk_size = parse_num(Some(lines.field("chunk")?), "chunk size", "chunk")?;

    let total = lines.field("total")?;
    let mut total_parts = total.split(' ');
    let total_files = parse_num(total_parts.next(), "total files", "total")?;
    let total_bytes = parse_num(total_parts.next(), "total bytes", "total")?;
    if total_parts.next().is_some() {
        return Err(invalid_data("extra field on 'total' line"));
    }

    let state = lines.field("state")?.to_string();
    if !valid_state(&state) {
        return Err(invalid_data(
            "state must match [A-Za-z0-9_-]+ and be non-empty",
        ));
    }

    let created_unix_ms = parse_num(Some(lines.field("created")?), "created time", "created")?;
    let updated_unix_ms = parse_num(Some(lines.field("updated")?), "updated time", "updated")?;

    let files = lines.files()?;
    lines.finish()?;

    Ok(TaskRecord {
        job_id,
        direction,
        root: PathBuf::from(root_str),
        peer_fingerprint,
        chunk_size,
        total_files,
        total_bytes,
        state,
        created_unix_ms,
        updated_unix_ms,
        files,
    })
}

/// Lowercase hex codec, kept local so this crate never depends on the
/// hex form of [`Fingerprint`] being implemented elsewhere.
fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        out.push((hex_digit(pair[0])?) << 4 | (hex_digit(pair[1])?));
    }
    Some(out)
}

fn hex_decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (hex_digit(pair[0])?) << 4 | (hex_digit(pair[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store(dir: &Path) -> FileCheckpointStore {
        FileCheckpointStore::open(dir).expect("open store")
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

    #[test]
    fn save_load_roundtrip_with_peer_and_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let record = sample(job(7), Direction::Send, Some(fp(9)));
        s.save(&record).expect("save");
        let loaded = s.load(&job(7)).expect("load").expect("record present");
        assert_eq!(loaded, record);
        assert_eq!(loaded.verified_chunks(), 17);
    }

    #[test]
    fn save_load_roundtrip_receive_no_peer_no_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let mut record = sample(job(1), Direction::Receive, None);
        record.files.clear();
        s.save(&record).expect("save");
        assert_eq!(s.load(&job(1)).expect("load"), Some(record));
    }

    #[test]
    fn load_missing_job_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        assert_eq!(s.load(&job(42)).expect("load"), None);
    }

    #[test]
    fn list_is_sorted_and_skips_corrupt_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        for byte in [9u8, 1, 5] {
            s.save(&sample(job(byte), Direction::Send, None))
                .expect("save");
        }
        // A corrupt file, a bad-hex name, and a temp file must all be skipped.
        fs::write(dir.path().join("aaaa.akjob"), b"not a checkpoint").expect("write");
        fs::write(dir.path().join("zzzz.akjob"), b"AKJOB1\njob xyz\n").expect("write");
        fs::write(dir.path().join("3333.akjob.tmp"), b"partial").expect("write");

        let listed = s.list().expect("list");
        let ids: Vec<JobIdBytes> = listed.iter().map(|r| r.job_id).collect();
        assert_eq!(ids, vec![job(1), job(5), job(9)]);
    }

    #[test]
    fn remove_reports_presence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        s.save(&sample(job(3), Direction::Send, None))
            .expect("save");
        assert!(s.remove(&job(3)).expect("remove"));
        assert!(!s.remove(&job(3)).expect("remove again"));
        assert_eq!(s.load(&job(3)).expect("load"), None);
    }

    #[test]
    fn concurrent_save_same_job_leaves_no_temp_residue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = std::sync::Arc::new(store(dir.path()));
        let mut a = sample(job(4), Direction::Send, None);
        a.state = "writing-a".to_string();
        let mut b = a.clone();
        b.state = "writing-b".to_string();

        let (sa, sb) = (std::sync::Arc::clone(&s), std::sync::Arc::clone(&s));
        let handle_a = std::thread::spawn(move || sa.save(&a));
        let handle_b = std::thread::spawn(move || sb.save(&b));
        handle_a.join().expect("thread a").expect("save a");
        handle_b.join().expect("thread b").expect("save b");

        // The job must still load as exactly one of the two writers.
        let loaded = s.load(&job(4)).expect("load").expect("present");
        assert!(loaded.state == "writing-a" || loaded.state == "writing-b");
        for entry in fs::read_dir(dir.path()).expect("read_dir") {
            let name = entry.expect("entry").file_name();
            let name = name.to_string_lossy().to_string();
            assert!(!name.contains(".tmp"), "temp residue: {name}");
        }
    }

    #[test]
    fn unicode_and_space_paths_roundtrip() {
        let outer = tempfile::tempdir().expect("tempdir");
        let dir = outer.path().join("检查点 目录"); // Chinese + space
        let s = store(&dir);
        let mut record = sample(job(11), Direction::Receive, Some(fp(2)));
        record.root = PathBuf::from("E:/接收 文件/新 文件夹");
        s.save(&record).expect("save");
        assert_eq!(s.load(&job(11)).expect("load"), Some(record.clone()));
        assert_eq!(s.list().expect("list"), vec![record]);
    }

    #[test]
    fn corrupt_records_fail_load_strictly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let good = sample(job(21), Direction::Send, Some(fp(3)));
        s.save(&good).expect("save");
        let original =
            fs::read_to_string(dir.path().join(format!("{}.akjob", hex_encode(&job(21)))))
                .expect("read");

        let bad_variants: Vec<(&str, String)> = vec![
            (
                "bad header",
                format!(
                    "AKJOB2\n{}",
                    original.lines().skip(1).collect::<Vec<_>>().join("\n")
                ),
            ),
            ("unknown line", format!("{original}extra junk\n")),
            (
                "bad peer hex",
                original.replacen(&hex_encode(fp(3).as_bytes()), "zz", 1),
            ),
            (
                "missing state line",
                original
                    .lines()
                    .filter(|l| !l.starts_with("state "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                "bad state charset",
                original.replacen("state in_progress-1", "state has space", 1),
            ),
            (
                "truncated",
                original
                    .split("updated ")
                    .next()
                    .expect("split")
                    .to_string(),
            ),
            (
                "bad chunk number",
                original.replacen("chunk 262144", "chunk abc", 1),
            ),
        ];
        for (what, text) in bad_variants {
            fs::write(
                dir.path().join(format!("{}.akjob", hex_encode(&job(21)))),
                text.as_bytes(),
            )
            .expect("write");
            let result = s.load(&job(21));
            assert!(result.is_err(), "expected parse failure for {what}");
        }
    }

    #[test]
    fn save_rejects_invalid_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let mut record = sample(job(31), Direction::Send, None);
        record.state = "not allowed!".to_string();
        assert!(
            s.save(&record).is_err(),
            "state with spaces must be rejected"
        );
        // No residue after a rejected save either.
        let entries: Vec<_> = fs::read_dir(dir.path()).expect("read_dir").collect();
        assert!(entries.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn save_rejects_non_utf8_root() {
        use std::os::windows::ffi::OsStringExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = store(dir.path());
        let mut record = sample(job(41), Direction::Send, None);
        // Unpaired surrogate: representable in OsString, not in UTF-8.
        record.root = PathBuf::from(std::ffi::OsString::from_wide(&[0xD800]));
        assert!(s.save(&record).is_err(), "non-UTF-8 root must be rejected");
    }
}
