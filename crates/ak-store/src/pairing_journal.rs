//! Local pairing-transaction journal (pairing-code plan A.3 / B.6.4).
//!
//! One file per pairing transaction, recording the local side of the
//! `pending -> committed -> applied` lifecycle (the server's
//! `pairing_transactions` mirror the same phases). The journal is what lets
//! a device that crashed mid-pairing resume or report a transaction instead
//! of silently losing it.
//!
//! Secret handling (B.6.4): the invite-code secret (`secret20`) and its
//! derived `auth_key` live only in the in-memory runtime. The journal
//! stores a `secret_ref` — an opaque reference id the runtime assigns to
//! its in-memory secret slot — never the secret itself, never `auth_key`,
//! and never the plain invite code. After a restart the reference can only
//! be resolved while the runtime still holds the secret; a `pending` entry
//! whose reference is dead cannot proceed without re-entering the code.
//! `committed`/`applied` entries never need the secret again.
//!
//! File format (version header `AKPAIR1`, one record per file, LF line
//! endings, fixed field order, nothing after the last line):
//!
//! ```text
//! AKPAIR1
//! pairing <32 hex>
//! code <32 hex>
//! lookup <64 hex>
//! secret <32 hex>
//! peer <64 hex or ->
//! phase pending|committed|applied
//! created <unix ms u64>
//! updated <unix ms u64>
//! ```
//!
//! Semantics:
//!
//! - **Atomic writes**: same temp+fsync+rename discipline as the checkpoint
//!   store (see [`atomic_write`]). A crash mid-write leaves a `.tmp` residue
//!   that loading ignores; a published record is always a complete file.
//! - **Monotonic phases**: `pending < committed < applied`. Phases only move
//!   forward. `applied` is terminal and never regresses; replaying a stale
//!   `committed` mark against an `applied` record is an error, not a
//!   rewrite. A forward jump `pending -> applied` is allowed: recovery may
//!   learn from a status reply that the server already finished.
//! - **Fail-closed loading**: a `.akpair` file that fails to parse —
//!   corrupt, truncated, bad hex, name/content mismatch, or a malformed
//!   file name — makes `load`/`load_all` return `Err`. Bad records are
//!   never silently skipped: a journal that lies is worse than a journal
//!   that refuses.
//! - **Idempotent recovery**: replaying the same `record_pending`
//!   (identical identity fields) returns the stored record without
//!   rewriting, whatever phase it reached; replaying the same phase mark is
//!   a no-op. The same `pairing_id` with *different* identity fields is
//!   rejected (the local mirror of the server's REQUEST_CONFLICT rule).
//! - **Peer key**: the peer's Ed25519 public key is optional at `pending`
//!   (the owner records a transaction before any redeemer appears) and can
//!   be filled in once. Once set it can never be changed — a different key
//!   for the same `pairing_id` is the "malicious server swaps keys" case
//!   and fails closed.
//!
//! This module also hosts the low-level helpers (hex codec, strict line
//! cursor, atomic write) shared with [`crate::queue`]; they are `pub(crate)`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{invalid_data, now_unix_ms};

/// Pairing transaction id (`pairing_id[16]` in the frozen transcript).
pub type PairingIdBytes = [u8; 16];
/// Invite code id (`code_id[16]`); groups tasks sharing one code.
pub type CodeIdBytes = [u8; 16];
/// Domain-separated lookup key (`lookup_digest[32]`); NOT a credential.
pub type LookupDigestBytes = [u8; 32];
/// Opaque reference to the in-memory secret slot; never secret material.
pub type SecretRefBytes = [u8; 16];
/// Peer Ed25519 public key (the device identity, not the fingerprint).
pub type PeerPublicKeyBytes = [u8; 32];

/// Journal phase of one pairing transaction; the derive order
/// (`Pending < Committed < Applied`) is the monotonic progress order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PairingPhase {
    /// Recorded locally, transaction not yet committed anywhere.
    Pending,
    /// Server (and both endpoints) durably committed the transcript.
    Committed,
    /// Local side wrote the finished relation into its trust book.
    Applied,
}

impl PairingPhase {
    /// Stable disk word.
    pub fn as_str(self) -> &'static str {
        match self {
            PairingPhase::Pending => "pending",
            PairingPhase::Committed => "committed",
            PairingPhase::Applied => "applied",
        }
    }

    fn parse(word: &str) -> io::Result<Self> {
        match word {
            "pending" => Ok(PairingPhase::Pending),
            "committed" => Ok(PairingPhase::Committed),
            "applied" => Ok(PairingPhase::Applied),
            other => Err(invalid_data(format!("bad pairing phase '{other}'"))),
        }
    }
}

/// Everything needed to open (or idempotently re-open) a journal entry.
/// The identity fields are immutable for the entry's life; only the peer
/// key (learned later) and the phase may advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPairing {
    pub pairing_id: PairingIdBytes,
    pub code_id: CodeIdBytes,
    pub lookup_digest: LookupDigestBytes,
    pub secret_ref: SecretRefBytes,
    /// `None` until the counterparty is known; fill via
    /// [`PairingJournal::set_peer_public_key`].
    pub peer_public_key: Option<PeerPublicKeyBytes>,
}

/// One persisted pairing-transaction entry. `pairing_id`, `code_id`,
/// `lookup_digest`, and `secret_ref` are immutable; `phase`,
/// `peer_public_key`, and `updated_unix_ms` advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub pairing_id: PairingIdBytes,
    pub code_id: CodeIdBytes,
    pub lookup_digest: LookupDigestBytes,
    pub secret_ref: SecretRefBytes,
    pub peer_public_key: Option<PeerPublicKeyBytes>,
    pub phase: PairingPhase,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
}

impl PairingRecord {
    /// True when `init` carries exactly this entry's identity fields (the
    /// idempotent-replay test; phase and timestamps are not compared).
    fn matches_init(&self, init: &PendingPairing) -> bool {
        self.pairing_id == init.pairing_id
            && self.code_id == init.code_id
            && self.lookup_digest == init.lookup_digest
            && self.secret_ref == init.secret_ref
            && self.peer_public_key == init.peer_public_key
    }
}

/// Directory of `<pairing hex>.akpair` files.
pub struct PairingJournal {
    dir: PathBuf,
}

const HEADER: &str = "AKPAIR1";
const EXTENSION: &str = "akpair";

impl PairingJournal {
    /// Open (creating if needed) the journal directory. A dedicated
    /// directory is recommended; any non-`.akpair` files in it are ignored.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    fn path_for(&self, pairing_id: &PairingIdBytes) -> PathBuf {
        self.dir
            .join(format!("{}.{}", hex_encode(pairing_id), EXTENSION))
    }

    /// Record a new `pending` transaction. Idempotent: replaying an init
    /// whose identity fields match the stored entry returns the stored
    /// record unchanged (no rewrite, no phase reset). A replay with any
    /// differing field is rejected — the same `pairing_id` can never name
    /// two different transactions.
    pub fn record_pending(&self, init: &PendingPairing) -> io::Result<PairingRecord> {
        if let Some(existing) = self.load(&init.pairing_id)? {
            if existing.matches_init(init) {
                return Ok(existing);
            }
            return Err(invalid_data(format!(
                "pairing {} replayed with different identity fields",
                hex_encode(&init.pairing_id)
            )));
        }
        let now = now_unix_ms();
        let record = PairingRecord {
            pairing_id: init.pairing_id,
            code_id: init.code_id,
            lookup_digest: init.lookup_digest,
            secret_ref: init.secret_ref,
            peer_public_key: init.peer_public_key,
            phase: PairingPhase::Pending,
            created_unix_ms: now,
            updated_unix_ms: now,
        };
        self.persist(&record)?;
        Ok(record)
    }

    /// Advance `pending -> committed` (the local mirror of a durable
    /// commit outcome). `committed -> committed` is an idempotent no-op;
    /// marking an already-`applied` entry is an error (stale replay), never
    /// a regression. Missing id: `NotFound`.
    pub fn mark_committed(&self, pairing_id: &PairingIdBytes) -> io::Result<PairingRecord> {
        self.advance_to(pairing_id, PairingPhase::Committed)
    }

    /// Advance to `applied` (local trust book updated). From `committed`
    /// this is the normal step; from `pending` the forward jump is allowed
    /// for recovery (a status reply says the transaction already finished);
    /// `applied -> applied` is an idempotent no-op. Nothing can leave
    /// `applied`. Missing id: `NotFound`.
    pub fn mark_applied(&self, pairing_id: &PairingIdBytes) -> io::Result<PairingRecord> {
        self.advance_to(pairing_id, PairingPhase::Applied)
    }

    /// Fill in the peer public key once (the owner side learns the
    /// redeemer at `InviteRedeemOffered`). Setting the same key again is a
    /// no-op; replacing a known key with a different one fails closed and
    /// leaves the stored entry untouched.
    pub fn set_peer_public_key(
        &self,
        pairing_id: &PairingIdBytes,
        key: &PeerPublicKeyBytes,
    ) -> io::Result<PairingRecord> {
        let mut record = self.required(pairing_id)?;
        match record.peer_public_key {
            Some(existing) if &existing == key => return Ok(record),
            Some(existing) => {
                return Err(invalid_data(format!(
                    "pairing {} peer key already pinned as {} and cannot change to {}",
                    hex_encode(pairing_id),
                    hex_encode(&existing),
                    hex_encode(key)
                )));
            }
            None => {}
        }
        record.peer_public_key = Some(*key);
        record.updated_unix_ms = now_unix_ms();
        self.persist(&record)?;
        Ok(record)
    }

    /// Strict load: an absent id is `None`; a present-but-corrupt file is
    /// `Err`.
    pub fn load(&self, pairing_id: &PairingIdBytes) -> io::Result<Option<PairingRecord>> {
        let path = self.path_for(pairing_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let record = decode_record(&text)?;
        if &record.pairing_id != pairing_id {
            return Err(invalid_data(
                "pairing journal id does not match its file name",
            ));
        }
        Ok(Some(record))
    }

    /// All entries sorted by pairing id, **fail-closed**: any `.akpair`
    /// file that cannot be read and strictly parsed (including a malformed
    /// file name) is an error, never a silently skipped record. `.tmp`
    /// residues from interrupted atomic writes and unrelated extensions
    /// are ignored.
    pub fn load_all(&self) -> io::Result<Vec<PairingRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            // A non-UTF-8 name cannot end in the ASCII ".akpair".
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // A temp-residue name ends in ".tmp", never in ".akpair".
            let Some(stem) = strip_extension(name, EXTENSION) else {
                continue;
            };
            let name_id = hex_decode_fixed::<16>(stem).ok_or_else(|| {
                invalid_data(format!("malformed pairing journal file name '{name}'"))
            })?;
            let text = fs::read_to_string(&path)?;
            let record = decode_record(&text)?;
            if record.pairing_id != name_id {
                return Err(invalid_data(format!(
                    "pairing journal id does not match its file name '{name}'"
                )));
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.pairing_id);
        Ok(records)
    }

    /// Entries for one invite code (`code_id`), sorted by pairing id.
    pub fn find_by_code(&self, code_id: &CodeIdBytes) -> io::Result<Vec<PairingRecord>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|record| &record.code_id == code_id)
            .collect())
    }

    /// Delete one entry (pruning an applied transaction once the trust
    /// book holds the relation). Returns false if absent. Pruning is the
    /// caller's policy; the journal itself never removes on load.
    pub fn remove(&self, pairing_id: &PairingIdBytes) -> io::Result<bool> {
        match fs::remove_file(self.path_for(pairing_id)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Shared phase-advance rules of `mark_committed`/`mark_applied`.
    fn advance_to(
        &self,
        pairing_id: &PairingIdBytes,
        target: PairingPhase,
    ) -> io::Result<PairingRecord> {
        let mut record = self.required(pairing_id)?;
        if record.phase == target {
            return Ok(record); // idempotent replay: no rewrite, no side effect
        }
        if record.phase > target {
            return Err(invalid_data(format!(
                "pairing {} is already {} and cannot go back to {}",
                hex_encode(pairing_id),
                record.phase.as_str(),
                target.as_str()
            )));
        }
        record.phase = target;
        record.updated_unix_ms = now_unix_ms();
        self.persist(&record)?;
        Ok(record)
    }

    fn required(&self, pairing_id: &PairingIdBytes) -> io::Result<PairingRecord> {
        self.load(pairing_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("pairing journal has no entry {}", hex_encode(pairing_id)),
            )
        })
    }

    /// Encode and atomically replace the entry's file.
    fn persist(&self, record: &PairingRecord) -> io::Result<()> {
        atomic_write(
            &self.dir,
            &hex_encode(&record.pairing_id),
            EXTENSION,
            encode_record(record).as_bytes(),
        )
    }
}

fn encode_record(record: &PairingRecord) -> String {
    let mut out = String::with_capacity(288);
    out.push_str(HEADER);
    out.push('\n');
    out.push_str("pairing ");
    out.push_str(&hex_encode(&record.pairing_id));
    out.push('\n');
    out.push_str("code ");
    out.push_str(&hex_encode(&record.code_id));
    out.push('\n');
    out.push_str("lookup ");
    out.push_str(&hex_encode(&record.lookup_digest));
    out.push('\n');
    out.push_str("secret ");
    out.push_str(&hex_encode(&record.secret_ref));
    out.push('\n');
    out.push_str("peer ");
    match &record.peer_public_key {
        Some(key) => out.push_str(&hex_encode(key)),
        None => out.push('-'),
    }
    out.push('\n');
    out.push_str("phase ");
    out.push_str(record.phase.as_str());
    out.push('\n');
    out.push_str("created ");
    out.push_str(&record.created_unix_ms.to_string());
    out.push('\n');
    out.push_str("updated ");
    out.push_str(&record.updated_unix_ms.to_string());
    out.push('\n');
    out
}

fn decode_record(text: &str) -> io::Result<PairingRecord> {
    let mut lines = StrictLines::new(text, HEADER)?;
    let pairing_id = hex_field::<16>(&mut lines, "pairing")?;
    let code_id = hex_field::<16>(&mut lines, "code")?;
    let lookup_digest = hex_field::<32>(&mut lines, "lookup")?;
    let secret_ref = hex_field::<16>(&mut lines, "secret")?;
    let peer_public_key = hex_field_opt::<32>(&mut lines, "peer")?;
    let phase = PairingPhase::parse(lines.field("phase")?)?;
    let created_unix_ms = parse_num(Some(lines.field("created")?), "created time", "created")?;
    let updated_unix_ms = parse_num(Some(lines.field("updated")?), "updated time", "updated")?;
    lines.finish()?;
    Ok(PairingRecord {
        pairing_id,
        code_id,
        lookup_digest,
        secret_ref,
        peer_public_key,
        phase,
        created_unix_ms,
        updated_unix_ms,
    })
}

// ---------------------------------------------------------------------------
// Low-level helpers shared by this module and `crate::queue`. They mirror
// the private helpers in the crate root (lib.rs) without modifying it.
// ---------------------------------------------------------------------------

/// Distinguishes concurrent temp files within one process (same scheme as
/// the checkpoint store: pid + monotonic seq).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write and fsync so the rename below can never publish a torn file.
pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Rename `tmp` over `final_path`, removing a pre-existing target first.
///
/// On Windows, rename over an existing file can fail, so the old file is
/// removed and the rename retried. That remove-then-rename gap is not
/// atomic: a crash in between leaves no final file at all. This mirrors
/// the checkpoint store's accepted trade-off — a missing entry only
/// degrades to redoing the pairing step, never to a false "applied" state,
/// because phases only move forward through [`PairingJournal::persist`].
pub(crate) fn rename_replace(tmp: &Path, final_path: &Path) -> io::Result<()> {
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

/// Atomic durable write of `<dir>/<stem>.<ext>`: write a unique
/// `<stem>.<pid>.<seq>.<ext>.tmp` in the same directory, fsync, then rename
/// over the final name; best-effort temp cleanup on failure. Concurrent
/// writers never share a temp path.
pub(crate) fn atomic_write(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> io::Result<()> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{stem}.{}.{seq}.{ext}.tmp", std::process::id()));
    let final_path = dir.join(format!("{stem}.{ext}"));
    let result = write_durable(&tmp, bytes).and_then(|()| rename_replace(&tmp, &final_path));
    if result.is_err() {
        // Best-effort cleanup so a failed save leaves no temp residue.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Strict line cursor over a fixed-order `<key> <value>` record; mirrors
/// the private parser in the crate root.
pub(crate) struct StrictLines<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> StrictLines<'a> {
    /// Split, tolerate exactly the one trailing newline every save writes,
    /// and require `header` as the first line.
    pub(crate) fn new(text: &'a str, header: &str) -> io::Result<Self> {
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        let first = lines
            .first()
            .copied()
            .ok_or_else(|| invalid_data("empty record file"))?;
        if first != header {
            return Err(invalid_data(format!(
                "unknown record version '{}'",
                preview(first)
            )));
        }
        Ok(Self { lines, pos: 1 })
    }

    /// Consume the next line, requiring it to be `<key> <value>`.
    pub(crate) fn field(&mut self, key: &str) -> io::Result<&'a str> {
        let line = self
            .lines
            .get(self.pos)
            .ok_or_else(|| invalid_data(format!("record ended early, expected '{key}' line")))?;
        let Some(value) = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(' '))
        else {
            return Err(invalid_data(format!(
                "expected '{key}' line, got '{}'",
                preview(line)
            )));
        };
        self.pos += 1;
        Ok(value)
    }

    /// Every line must have been consumed; anything left is unknown.
    pub(crate) fn finish(&self) -> io::Result<()> {
        match self.lines.get(self.pos) {
            None => Ok(()),
            Some(line) => Err(invalid_data(format!("unknown line '{}'", preview(line)))),
        }
    }
}

/// Char-boundary-safe snippet for error messages.
pub(crate) fn preview(line: &str) -> String {
    line.chars().take(48).collect()
}

/// Parse a strictly numeric field (used by both stores).
pub(crate) fn parse_num<T: std::str::FromStr>(
    raw: Option<&str>,
    what: &str,
    line: &str,
) -> io::Result<T> {
    let text =
        raw.ok_or_else(|| invalid_data(format!("missing {what} on line '{}'", preview(line))))?;
    text.parse::<T>()
        .map_err(|_| invalid_data(format!("bad {what} '{text}' on line '{}'", preview(line))))
}

/// Lowercase hex codec (kept local, like the crate-root one).
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
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

/// Decode exactly `N*2` hex chars into fixed bytes; written lowercase,
/// uppercase tolerated on read (same rule as the crate root).
pub(crate) fn hex_decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
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

/// Consume a `<key> <hex>` field of fixed width.
pub(crate) fn hex_field<const N: usize>(
    lines: &mut StrictLines<'_>,
    key: &str,
) -> io::Result<[u8; N]> {
    let raw = lines.field(key)?;
    hex_decode_fixed::<N>(raw)
        .ok_or_else(|| invalid_data(format!("bad {key} hex (expected {} hex chars)", N * 2)))
}

/// Consume a `<key> <hex|->` optional field of fixed width.
pub(crate) fn hex_field_opt<const N: usize>(
    lines: &mut StrictLines<'_>,
    key: &str,
) -> io::Result<Option<[u8; N]>> {
    let raw = lines.field(key)?;
    if raw == "-" {
        return Ok(None);
    }
    hex_decode_fixed::<N>(raw)
        .map(Some)
        .ok_or_else(|| invalid_data(format!("bad {key} hex (expected {} hex chars)", N * 2)))
}

/// If `name` is `<stem>.<ext>` return `stem`, else `None`. Temp-residue
/// names end in `.tmp`, never in a real extension, so callers skip them
/// before this check matters.
fn strip_extension<'a>(name: &'a str, ext: &str) -> Option<&'a str> {
    name.strip_suffix(&format!(".{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(dir: &Path) -> PairingJournal {
        PairingJournal::open(dir).expect("open journal")
    }

    fn init(byte: u8) -> PendingPairing {
        PendingPairing {
            pairing_id: [byte; 16],
            code_id: [byte + 1; 16],
            lookup_digest: [byte + 2; 32],
            secret_ref: [byte + 3; 16],
            peer_public_key: None,
        }
    }

    fn entry_path(dir: &Path, id: &PairingIdBytes) -> PathBuf {
        dir.join(format!("{}.{}", hex_encode(id), EXTENSION))
    }

    fn file_bytes(dir: &Path, id: &PairingIdBytes) -> Vec<u8> {
        fs::read(entry_path(dir, id)).expect("read entry")
    }

    fn akpair_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("read_dir")
            .filter(|e| {
                e.as_ref()
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(EXTENSION)
            })
            .count()
    }

    #[test]
    fn record_pending_roundtrip_and_file_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        let mut start = init(7);
        start.peer_public_key = Some([0xB1; 32]);
        j.record_pending(&start).expect("record");
        let loaded = j.load(&[7; 16]).expect("load").expect("present");
        assert_eq!(loaded.phase, PairingPhase::Pending);
        assert_eq!(loaded.created_unix_ms, loaded.updated_unix_ms);
        assert!(entry_path(dir.path(), &[7; 16]).is_file());
        assert_eq!(
            file_bytes(dir.path(), &[7; 16]),
            encode_record(&loaded).as_bytes()
        );
    }

    #[test]
    fn replay_is_idempotent_across_every_phase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        let start = init(3);
        j.record_pending(&start).expect("record");
        j.mark_committed(&[3; 16]).expect("commit");
        let before = file_bytes(dir.path(), &[3; 16]);

        // Same init replayed after commit: stored record, no rewrite.
        let replayed = j.record_pending(&start).expect("idempotent replay");
        assert_eq!(replayed.phase, PairingPhase::Committed);
        assert_eq!(file_bytes(dir.path(), &[3; 16]), before);

        j.mark_applied(&[3; 16]).expect("apply");
        let replayed = j.record_pending(&start).expect("idempotent replay");
        assert_eq!(replayed.phase, PairingPhase::Applied);
        assert_eq!(akpair_count(dir.path()), 1);
    }

    #[test]
    fn replay_with_different_identity_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(5)).expect("record");
        let before = file_bytes(dir.path(), &[5; 16]);

        let mut other = init(5);
        other.secret_ref = [0xEE; 16];
        assert!(j.record_pending(&other).is_err(), "secret_ref change");
        let mut other = init(5);
        other.code_id = [0xCC; 16];
        assert!(j.record_pending(&other).is_err(), "code_id change");
        let mut other = init(5);
        other.peer_public_key = Some([0xAA; 32]);
        assert!(j.record_pending(&other).is_err(), "peer fill-in via replay");

        assert_eq!(file_bytes(dir.path(), &[5; 16]), before, "entry untouched");
    }

    #[test]
    fn phases_advance_monotonically_with_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        let pending = j.record_pending(&init(9)).expect("record");
        let committed = j.mark_committed(&[9; 16]).expect("commit");
        assert_eq!(committed.phase, PairingPhase::Committed);
        assert_eq!(committed.created_unix_ms, pending.created_unix_ms);
        assert!(committed.updated_unix_ms >= pending.updated_unix_ms);
        let applied = j.mark_applied(&[9; 16]).expect("apply");
        assert_eq!(applied.phase, PairingPhase::Applied);

        // The same marks again: the regression is rejected, the idempotent
        // replay is a no-op, and neither touches the file.
        let before = file_bytes(dir.path(), &[9; 16]);
        let err = j.mark_committed(&[9; 16]).expect_err("regression");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(file_bytes(dir.path(), &[9; 16]), before);
        let again = j.mark_applied(&[9; 16]).expect("idempotent");
        assert_eq!(again.phase, PairingPhase::Applied);
        assert_eq!(file_bytes(dir.path(), &[9; 16]), before);
    }

    #[test]
    fn applied_never_regresses_even_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(11)).expect("record");
        // Documented recovery jump: applied can be learned without a local
        // commit step.
        j.mark_applied(&[11; 16])
            .expect("forward jump pending->applied");
        assert!(j.mark_committed(&[11; 16]).is_err());
        assert_eq!(
            j.load(&[11; 16]).expect("load").expect("present").phase,
            PairingPhase::Applied
        );
    }

    #[test]
    fn mark_missing_entry_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        let err = j.mark_committed(&[21; 16]).expect_err("missing");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn peer_key_is_set_once_and_never_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(13)).expect("record");
        let filled = j.set_peer_public_key(&[13; 16], &[0x5A; 32]).expect("fill");
        assert_eq!(filled.peer_public_key, Some([0x5A; 32]));
        let pinned = file_bytes(dir.path(), &[13; 16]);
        // Same key again: no-op, no rewrite.
        j.set_peer_public_key(&[13; 16], &[0x5A; 32])
            .expect("same key");
        assert_eq!(file_bytes(dir.path(), &[13; 16]), pinned);
        // Different key: rejected, entry unchanged.
        assert!(
            j.set_peer_public_key(&[13; 16], &[0xC3; 32]).is_err(),
            "peer key swap must fail closed"
        );
        assert_eq!(file_bytes(dir.path(), &[13; 16]), pinned);
    }

    #[test]
    fn crash_before_rename_keeps_previous_phase_visible() {
        // "Changed half" crash: the committed text sits in a temp file but
        // the rename never happened. Readers must still see the old phase,
        // and replaying the mark must then complete atomically.
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(17)).expect("record");
        let mut staged = j.load(&[17; 16]).expect("load").expect("present");
        staged.phase = PairingPhase::Committed;
        staged.updated_unix_ms += 1;
        let tmp = dir.path().join(format!(
            "{}.{}.0.{EXTENSION}.tmp",
            hex_encode(&[17; 16]),
            std::process::id()
        ));
        fs::write(&tmp, encode_record(&staged).as_bytes()).expect("write tmp");

        assert_eq!(
            j.load(&[17; 16]).expect("load").expect("present").phase,
            PairingPhase::Pending
        );
        assert_eq!(j.load_all().expect("load_all").len(), 1);
        let committed = j.mark_committed(&[17; 16]).expect("commit after recovery");
        assert_eq!(committed.phase, PairingPhase::Committed);
    }

    #[test]
    fn crash_in_windows_rename_gap_recovers_by_redo() {
        // "Changed half" crash inside rename_replace: the old final file is
        // gone and the new one was never renamed in. The entry is simply
        // absent; re-recording the same transaction is allowed and fresh.
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(19)).expect("record");
        fs::remove_file(entry_path(dir.path(), &[19; 16])).expect("remove final");
        fs::write(
            dir.path().join("residue.akpair.tmp"),
            b"AKPAIR1\npairing 19",
        )
        .expect("tmp");

        assert_eq!(j.load(&[19; 16]).expect("load"), None);
        assert_eq!(j.load_all().expect("load_all"), Vec::new());
        let redo = j.record_pending(&init(19)).expect("re-record");
        assert_eq!(redo.phase, PairingPhase::Pending);
    }

    #[test]
    fn crash_mid_write_temp_residue_is_ignored() {
        // "Written half" crash: a half-written temp file must never surface
        // as a record, and unrelated files are not journal business.
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(23)).expect("record");
        fs::write(dir.path().join("aaaa.akpair.tmp"), b"AKPAI").expect("half tmp");
        fs::write(dir.path().join("notes.txt"), b"unrelated").expect("unrelated");
        fs::write(dir.path().join("1111.akjob"), b"checkpoint lives here").expect("foreign");

        let all = j.load_all().expect("load_all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].pairing_id, [23; 16]);
    }

    #[test]
    fn corrupt_records_fail_closed_not_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(29)).expect("record");
        let original = String::from_utf8(file_bytes(dir.path(), &[29; 16])).expect("utf8");

        let variants: Vec<(&str, String)> = vec![
            (
                "bad header",
                format!(
                    "AKPAIR2\n{}",
                    original.lines().skip(1).collect::<Vec<_>>().join("\n")
                ),
            ),
            ("unknown line", format!("{original}extra junk\n")),
            (
                "missing phase line",
                original
                    .lines()
                    .filter(|l| !l.starts_with("phase "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                "bad phase word",
                original.replacen("phase pending", "phase reverted", 1),
            ),
            (
                "bad lookup hex",
                original.replacen(&hex_encode(&[31; 32]), "zz", 1),
            ),
            (
                "truncated",
                original
                    .split("updated ")
                    .next()
                    .expect("split")
                    .to_string(),
            ),
        ];
        for (what, text) in variants {
            fs::write(entry_path(dir.path(), &[29; 16]), text.as_bytes()).expect("write");
            assert!(j.load(&[29; 16]).is_err(), "load must fail for {what}");
            assert!(
                j.load_all().is_err(),
                "load_all must fail closed for {what}"
            );
        }

        // A malformed file name owning the extension fails closed too.
        fs::write(dir.path().join("nothex.akpair"), original.as_bytes()).expect("write");
        assert!(j.load_all().is_err(), "malformed name must fail closed");
        fs::remove_file(dir.path().join("nothex.akpair")).expect("remove");

        // A valid record stored under another id's file name is a mismatch.
        fs::write(entry_path(dir.path(), &[77; 16]), original.as_bytes()).expect("write");
        assert!(
            j.load(&[77; 16]).is_err(),
            "name/content mismatch must fail closed"
        );
        assert!(j.load_all().is_err());
    }

    #[test]
    fn find_by_code_filters_and_sorts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        let mut a = init(41);
        a.code_id = [0x99; 16];
        let mut b = init(37);
        b.code_id = [0x99; 16];
        let c = init(43);
        j.record_pending(&a).expect("a");
        j.record_pending(&b).expect("b");
        j.record_pending(&c).expect("c");

        let found = j.find_by_code(&[0x99; 16]).expect("find");
        let ids: Vec<PairingIdBytes> = found.iter().map(|r| r.pairing_id).collect();
        assert_eq!(ids, vec![[37; 16], [41; 16]]);
    }

    #[test]
    fn remove_reports_presence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = journal(dir.path());
        j.record_pending(&init(47)).expect("record");
        assert!(j.remove(&[47; 16]).expect("remove"));
        assert!(!j.remove(&[47; 16]).expect("remove again"));
        assert_eq!(j.load(&[47; 16]).expect("load"), None);
    }
}
