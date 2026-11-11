//! Transactional `peers.book` writes for pairing results (plan A.5 §5).
//!
//! [`PeerBook::save`] atomically replaces one local file, but a pairing
//! commit is a read–modify–write of that file plus replay protection. This
//! module adds the missing transactional entry point,
//! [`crate::peer_store::PeerBookStore::commit_paired_peer`]:
//!
//! * the read/merge/save sequence runs under one process-wide lock, so
//!   concurrent commits (CLI `add`, FFI `ak_peer_*`, automatic pairing)
//!   serialize instead of clobbering each other;
//! * a small pairing journal (`<book>.pairings`, `pairing_id →
//!   fingerprint`) makes commits idempotent per pairing transaction:
//!   re-committing the same `pairing_id` for the same fingerprint is a
//!   no-op success, re-committing it for a *different* fingerprint is a
//!   conflict, and committing a new `pairing_id` for a known fingerprint is
//!   an explicit update;
//! * a corrupt book or journal is an error, never silently treated as an
//!   empty file to overwrite;
//! * on update, the locally set name and the first pairing timestamp are
//!   preserved by an explicit rule.
//!
//! The write order is book first, journal second: a crash between the two
//! only turns the retry into a benign re-commit (upsert of identical
//! data). A crash after an idempotent `AlreadyCommitted` never resurrects a
//! peer the user removed manually — the entry point returns without
//! touching the book.
//!
//! Scope notes (deliberate, plan A.5 §5): the lock is process-local; two
//! *processes* writing the same book last-writer-wins at whole-file
//! granularity via the existing atomic rename. Full Prepared/Commit/Ready
//! journaling, tombstones against delayed commits, and trust versioning
//! belong to the A5 `ak-store` pairing journal and the A6 orchestration
//! that calls this entry point.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::pairing_txn::PairingId;
use crate::{atomic_write, hex_lower, hex_value, Fingerprint, PeerBook, PeerRecord};

/// Serializes the read–merge–save critical section across all threads of
/// this process, regardless of which book path they target. A single lock
/// is intentional: peer-book commits are rare, tiny, and must never
/// deadlock on per-path lock ordering.
static BOOK_TXN_LOCK: Mutex<()> = Mutex::new(());

/// Header line of the `<book>.pairings` journal.
const PAIRING_JOURNAL_MAGIC: &str = "AKPAIRTXN1";

/// Why a pairing commit failed.
#[derive(Debug, thiserror::Error)]
pub enum PeerBookTxnError {
    /// I/O or format error while reading/writing the book or journal.
    /// Corrupt files fail closed here instead of being overwritten.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The pairing id was already committed for a different fingerprint;
    /// one pairing transaction can only ever land on one peer.
    #[error("pairing id is already committed for a different peer fingerprint")]
    PairingIdConflict,
}

/// Result of a [`crate::peer_store::PeerBookStore::commit_paired_peer`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The fingerprint was new; a book entry was created.
    Committed,
    /// The fingerprint was already paired; the entry was updated under a
    /// new pairing transaction (first pairing time kept).
    Updated,
    /// This exact pairing id had already been committed for the same
    /// fingerprint; nothing was written.
    AlreadyCommitted,
}

/// Transactional entry point for writing pairing results into a local
/// `peers.book`. Stateless; cheap to construct wherever needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerBookStore;

impl PeerBookStore {
    pub fn new() -> Self {
        PeerBookStore
    }

    /// Atomically record one paired peer under a specific pairing
    /// transaction (plan A.5 §5).
    ///
    /// * Same `expected_pairing_id` re-committed for the same fingerprint:
    ///   [`CommitOutcome::AlreadyCommitted`], no writes (idempotent replay
    ///   protection — also what keeps a delayed duplicate from
    ///   resurrecting a manually removed peer).
    /// * Same `expected_pairing_id` for a different fingerprint:
    ///   [`PeerBookTxnError::PairingIdConflict`], no writes.
    /// * New `expected_pairing_id` for a known fingerprint:
    ///   [`CommitOutcome::Updated`] — the old journal entry is replaced,
    ///   the locally set name and first pairing timestamp are preserved,
    ///   an empty incoming name never wipes the local name.
    /// * New fingerprint: [`CommitOutcome::Committed`], book created from
    ///   scratch if the file does not exist yet.
    ///
    /// A missing book file means "fresh device"; a corrupt book or
    /// journal is returned as [`PeerBookTxnError::Io`] and is never
    /// overwritten.
    pub fn commit_paired_peer(
        &self,
        book_path: &Path,
        peer: PeerRecord,
        expected_pairing_id: &PairingId,
    ) -> Result<CommitOutcome, PeerBookTxnError> {
        let _guard = BOOK_TXN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let journal_path = journal_path_for(book_path);
        let mut journal = PairingJournal::load(&journal_path)?;

        // Replay protection first: an already-committed pairing id is a
        // no-op for the same fingerprint and a hard conflict otherwise.
        if let Some(entry) = journal.entries.get(expected_pairing_id) {
            if entry.fingerprint == peer.fingerprint {
                return Ok(CommitOutcome::AlreadyCommitted);
            }
            return Err(PeerBookTxnError::PairingIdConflict);
        }

        // Missing book = fresh device; corrupt book = fail closed.
        let mut book = match PeerBook::load(book_path) {
            Ok(book) => book,
            Err(err) if err.kind() == io::ErrorKind::NotFound => PeerBook::default(),
            Err(err) => return Err(err.into()),
        };

        // Explicit update rules: keep the first pairing timestamp, and let
        // an empty incoming name preserve the local one.
        let existing = book.get(&peer.fingerprint).cloned();
        let existed = existing.is_some();
        let mut record = peer;
        if let Some(existing) = existing {
            if record.name.is_empty() {
                record.name = existing.name.clone();
            }
            record.paired_unix_ms = existing.paired_unix_ms;
        }
        let outcome = if existed {
            CommitOutcome::Updated
        } else {
            CommitOutcome::Committed
        };

        let fingerprint = record.fingerprint;
        book.upsert(record);
        // Book first, journal second: a crash between the two turns the
        // retry into a benign re-commit of identical data.
        book.save(book_path)?;
        journal.commit(*expected_pairing_id, fingerprint, now_unix_ms());
        journal.save(&journal_path)?;
        Ok(outcome)
    }
}

/// `<book_path>.pairings`, next to the book itself.
fn journal_path_for(book_path: &Path) -> PathBuf {
    let mut name = book_path.as_os_str().to_os_string();
    name.push(".pairings");
    PathBuf::from(name)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse exactly `2 * N` hex characters into an `[u8; N]` via the crate's
/// shared hex decoder.
fn parse_hex_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_value(bytes[i * 2])?;
        let lo = hex_value(bytes[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

/// One journal line: what a committed pairing transaction landed on.
#[derive(Debug, Clone, Copy)]
struct JournalEntry {
    fingerprint: Fingerprint,
    committed_unix_ms: u64,
}

/// The `<book>.pairings` sidecar: latest committed pairing id per peer.
/// At most one entry per fingerprint, so the file grows with the book,
/// not with pairing history.
#[derive(Debug, Default)]
struct PairingJournal {
    entries: BTreeMap<PairingId, JournalEntry>,
}

impl PairingJournal {
    /// Load the journal; a missing file is an empty journal. Anything
    /// malformed is InvalidData (fail closed), with no field content in
    /// the error.
    fn load(path: &Path) -> io::Result<Self> {
        let invalid = |reason: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {reason}", path.display()),
            )
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(PairingJournal::default())
            }
            Err(err) => return Err(err),
        };
        let mut lines = text.lines();
        let header = lines
            .next()
            .ok_or_else(|| invalid("pairing journal is empty"))?;
        if header.trim_end_matches('\r') != PAIRING_JOURNAL_MAGIC {
            return Err(invalid("unexpected pairing journal header"));
        }
        let mut entries = BTreeMap::new();
        for line in lines {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let (id_hex, fp_hex, ts) =
                match (fields.next(), fields.next(), fields.next(), fields.next()) {
                    (Some(id), Some(fp), Some(ts), None) => (id, fp, ts),
                    _ => {
                        return Err(invalid(
                            "expected pairing id, fingerprint, timestamp fields",
                        ))
                    }
                };
            let pairing_id: PairingId = parse_hex_fixed(id_hex)
                .ok_or_else(|| invalid("pairing id must be 32 hex characters"))?;
            let fingerprint = Fingerprint::from_hex(fp_hex)
                .ok_or_else(|| invalid("fingerprint must be 64 hex characters"))?;
            let committed_unix_ms: u64 = ts
                .parse()
                .map_err(|_| invalid("timestamp must be an unsigned integer"))?;
            entries.insert(
                pairing_id,
                JournalEntry {
                    fingerprint,
                    committed_unix_ms,
                },
            );
        }
        Ok(PairingJournal { entries })
    }

    /// Record a commit: one fingerprint carries at most its latest
    /// pairing id, so an update replaces the previous entry.
    fn commit(&mut self, pairing_id: PairingId, fingerprint: Fingerprint, committed_unix_ms: u64) {
        self.entries
            .retain(|_, entry| entry.fingerprint != fingerprint);
        self.entries.insert(
            pairing_id,
            JournalEntry {
                fingerprint,
                committed_unix_ms,
            },
        );
    }

    /// Persist atomically (temp file + fsync + rename) in deterministic
    /// pairing-id order.
    fn save(&self, path: &Path) -> io::Result<()> {
        let mut out =
            String::with_capacity(PAIRING_JOURNAL_MAGIC.len() + 1 + self.entries.len() * 114);
        out.push_str(PAIRING_JOURNAL_MAGIC);
        out.push('\n');
        for (pairing_id, entry) in &self.entries {
            out.push_str(&hex_lower(pairing_id));
            out.push('\t');
            out.push_str(&entry.fingerprint.to_hex());
            out.push('\t');
            out.push_str(&entry.committed_unix_ms.to_string());
            out.push('\n');
        }
        atomic_write(path, out.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::fs;
    use std::sync::Arc;

    fn peer_record(name: &str, paired_unix_ms: u64) -> PeerRecord {
        PeerRecord {
            fingerprint: Fingerprint::from_bytes([0x77; 32]),
            name: name.to_owned(),
            paired_unix_ms,
        }
    }

    fn other_peer_record(name: &str) -> PeerRecord {
        PeerRecord {
            fingerprint: Fingerprint::from_bytes([0x88; 32]),
            name: name.to_owned(),
            paired_unix_ms: 9,
        }
    }

    fn pid(byte: u8) -> PairingId {
        [byte; 16]
    }

    #[test]
    fn commit_into_missing_book_creates_book_and_journal() {
        let dir = TempDir::new("fresh-book");
        let book_path = dir.path().join("peers.book");
        let store = PeerBookStore::new();

        let outcome = store
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap();
        assert_eq!(outcome, CommitOutcome::Committed);

        let book = PeerBook::load(&book_path).unwrap();
        assert_eq!(book.iter().count(), 1);
        assert_eq!(
            book.get(&Fingerprint::from_bytes([0x77; 32]))
                .map(|record| record.name.as_str()),
            Some("Laptop")
        );
        let journal_text = fs::read_to_string(journal_path_for(&book_path)).unwrap();
        assert!(journal_text.starts_with("AKPAIRTXN1\n"));
        assert!(journal_text.contains(&Fingerprint::from_bytes([0x77; 32]).to_hex()));
    }

    #[test]
    fn recommit_with_same_pairing_id_is_idempotent() {
        let dir = TempDir::new("idempotent");
        let book_path = dir.path().join("peers.book");
        let store = PeerBookStore::new();

        store
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap();
        let book_before = fs::read_to_string(&book_path).unwrap();
        let journal_before = fs::read_to_string(journal_path_for(&book_path)).unwrap();

        // Same pairing id, same fingerprint — even with a changed name and
        // timestamp the commit is a no-op.
        let outcome = store
            .commit_paired_peer(&book_path, peer_record("Attacker", 9_999), &pid(1))
            .unwrap();
        assert_eq!(outcome, CommitOutcome::AlreadyCommitted);

        assert_eq!(fs::read_to_string(&book_path).unwrap(), book_before);
        assert_eq!(
            fs::read_to_string(journal_path_for(&book_path)).unwrap(),
            journal_before
        );
    }

    #[test]
    fn different_pairing_id_same_fingerprint_is_update() {
        let dir = TempDir::new("update");
        let book_path = dir.path().join("peers.book");
        let store = PeerBookStore::new();

        store
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap();
        let outcome = store
            .commit_paired_peer(&book_path, peer_record("Laptop Pro", 5_000), &pid(2))
            .unwrap();
        assert_eq!(outcome, CommitOutcome::Updated);

        // One book entry; the journal holds only the latest pairing id.
        let book = PeerBook::load(&book_path).unwrap();
        assert_eq!(book.iter().count(), 1);
        let journal_text = fs::read_to_string(journal_path_for(&book_path)).unwrap();
        assert!(journal_text.contains(&hex_lower(&pid(2))));
        assert!(!journal_text.contains(&hex_lower(&pid(1))));

        // And the old pairing id can be replayed as a fresh update.
        let outcome = store
            .commit_paired_peer(&book_path, peer_record("Laptop Pro", 5_000), &pid(1))
            .unwrap();
        assert_eq!(outcome, CommitOutcome::Updated);
    }

    #[test]
    fn update_preserves_first_pairing_time_and_local_name() {
        let dir = TempDir::new("preserve");
        let book_path = dir.path().join("peers.book");
        let store = PeerBookStore::new();

        store
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap();
        // Re-pair under a new transaction with an empty name and a later
        // timestamp: both local fields win.
        store
            .commit_paired_peer(&book_path, peer_record("", 9_999), &pid(2))
            .unwrap();

        let book = PeerBook::load(&book_path).unwrap();
        let record = book.get(&Fingerprint::from_bytes([0x77; 32])).unwrap();
        assert_eq!(record.name, "Laptop");
        assert_eq!(record.paired_unix_ms, 1_000);
    }

    #[test]
    fn same_pairing_id_other_fingerprint_is_conflict() {
        let dir = TempDir::new("conflict");
        let book_path = dir.path().join("peers.book");
        let store = PeerBookStore::new();

        store
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap();
        let book_before = fs::read_to_string(&book_path).unwrap();

        let err = store
            .commit_paired_peer(&book_path, other_peer_record("Impostor"), &pid(1))
            .unwrap_err();
        assert!(matches!(err, PeerBookTxnError::PairingIdConflict));

        // Nothing was written for the impostor.
        assert_eq!(fs::read_to_string(&book_path).unwrap(), book_before);
        let book = PeerBook::load(&book_path).unwrap();
        assert_eq!(book.iter().count(), 1);
        assert!(book.get(&Fingerprint::from_bytes([0x88; 32])).is_none());
    }

    #[test]
    fn corrupt_book_is_rejected_not_overwritten() {
        let dir = TempDir::new("corrupt-book");
        let book_path = dir.path().join("peers.book");
        fs::write(&book_path, "definitely not a peer book\n").unwrap();

        let err = PeerBookStore::new()
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap_err();
        match err {
            PeerBookTxnError::Io(io_err) => assert_eq!(io_err.kind(), io::ErrorKind::InvalidData),
            other => panic!("expected Io, got {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(&book_path).unwrap(),
            "definitely not a peer book\n"
        );
        assert!(!journal_path_for(&book_path).exists());
    }

    #[test]
    fn corrupt_journal_fails_closed() {
        let dir = TempDir::new("corrupt-journal");
        let book_path = dir.path().join("peers.book");
        fs::write(journal_path_for(&book_path), "AKPAIRTXN1\ngarbage line\n").unwrap();

        let err = PeerBookStore::new()
            .commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            .unwrap_err();
        match err {
            PeerBookTxnError::Io(io_err) => assert_eq!(io_err.kind(), io::ErrorKind::InvalidData),
            other => panic!("expected Io, got {other:?}"),
        }
        // The book was never created: the commit refused to proceed with
        // unusable replay-protection state.
        assert!(!book_path.exists());
    }

    #[test]
    fn concurrent_updates_same_fingerprint_serialize() {
        let dir = TempDir::new("concurrent-update");
        let book_path = Arc::new(dir.path().join("peers.book"));
        let store = Arc::new(PeerBookStore::new());

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (pairing_id, name) in [(pid(1), "A-side"), (pid(2), "B-side")] {
            let book_path = Arc::clone(&book_path);
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.commit_paired_peer(&book_path, peer_record(name, 1_000), &pairing_id)
            }));
        }
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread panicked").expect("io error"))
            .collect();

        // Serialized: exactly one fresh commit, exactly one update.
        assert_eq!(outcomes.len(), 2);
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == CommitOutcome::Committed)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == CommitOutcome::Updated)
                .count(),
            1
        );

        // The book holds exactly one entry; the journal exactly one pairing
        // id, whichever thread committed last.
        let book = PeerBook::load(&book_path).unwrap();
        assert_eq!(book.iter().count(), 1);
        let journal = fs::read_to_string(journal_path_for(&book_path)).unwrap();
        let entry_count = journal.lines().count() - 1;
        assert_eq!(entry_count, 1, "journal: {journal}");
        assert!(journal.contains(&hex_lower(&pid(1))) ^ journal.contains(&hex_lower(&pid(2))));

        // No temp files left behind.
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["peers.book".to_owned(), "peers.book.pairings".to_owned()]
        );
    }

    #[test]
    fn concurrent_same_pairing_id_commits_exactly_once() {
        let dir = TempDir::new("concurrent-once");
        let book_path = Arc::new(dir.path().join("peers.book"));
        let store = Arc::new(PeerBookStore::new());

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let book_path = Arc::clone(&book_path);
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.commit_paired_peer(&book_path, peer_record("Laptop", 1_000), &pid(1))
            }));
        }
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread panicked").expect("io error"))
            .collect();

        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == CommitOutcome::Committed)
                .count(),
            1,
            "outcomes: {outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == CommitOutcome::AlreadyCommitted)
                .count(),
            1,
            "outcomes: {outcomes:?}"
        );
        assert_eq!(PeerBook::load(&book_path).unwrap().iter().count(), 1);
    }
}
