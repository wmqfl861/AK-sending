//! Fingerprints, the peer book, pair export / import and the receive
//! `--expect` policy.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

/// The identity directory `id_dir` resolves to, WITHOUT creating it (the
/// read-only `registry-id` must not leave a directory behind).
pub(super) fn id_dir_path(cli: &Cli) -> PathBuf {
    cli.id_dir.clone().unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".ak-sending")
    })
}

pub(super) fn parse_fingerprint(text: &str) -> Result<Fingerprint> {
    let trimmed = text.trim();
    if trimmed.len() == 64 {
        Fingerprint::from_hex(trimmed).context("fingerprint must be 64 hex characters")
    } else {
        bail!(
            "unknown fingerprint format: expected 64 hex characters \
             (print it on the peer device with `ak-cli pair code`)"
        )
    }
}

/// One accepted fingerprint-argument shape for [`resolve_fingerprint_arg`].
const FP_MIN_PREFIX: usize = 8;

/// Resolve a fingerprint argument shared by `send --fp`, `pair remove` and
/// `receive --expect`: a full 64-hex value parses directly (no book access
/// needed); an 8..63 hex-character prefix must match exactly one peers.book
/// entry and is completed to that entry. Zero matches, ambiguity and
/// malformed input are explicit errors.
pub(super) fn resolve_fingerprint_arg(text: &str, book_path: &Path) -> Result<Fingerprint> {
    let trimmed = text.trim();
    let is_hex = !trimmed.is_empty() && trimmed.bytes().all(|b: u8| b.is_ascii_hexdigit());
    if !is_hex {
        bail!(
            "unknown fingerprint format: expected 64 hex characters \
             (print it on the peer device with `ak-cli pair code`)"
        );
    }
    if trimmed.len() == 64 {
        return Fingerprint::from_hex(trimmed).context("fingerprint must be 64 hex characters");
    }
    if (FP_MIN_PREFIX..64).contains(&trimmed.len()) {
        let prefix = trimmed.to_ascii_lowercase();
        let book = load_peer_book(book_path)?;
        let hits: Vec<&PeerRecord> = book
            .iter()
            .filter(|record| record.fingerprint.to_hex().starts_with(&prefix))
            .collect();
        return match hits.as_slice() {
            [only] => Ok(only.fingerprint),
            [] => bail!(
                "fingerprint prefix {prefix} not found in the peer book {}; the sending \
                 side needs the full 64-hex fingerprint (see `ak-cli pair list`)",
                book_path.display()
            ),
            many => bail!(
                "fingerprint prefix {prefix} is ambiguous: matches {} peers ({}); add more \
                 hex characters or use the full 64-hex fingerprint",
                many.len(),
                many.iter()
                    .map(|record| record.fingerprint.short_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    bail!(
        "fingerprint must be the full 64 hex characters or a unique prefix of \
         {FP_MIN_PREFIX} to 63 characters (got {})",
        trimmed.len()
    );
}

// ---------------------------------------------------------------------------
// Peer book helpers
// ---------------------------------------------------------------------------

pub(crate) fn peer_book_path(id: &Path) -> PathBuf {
    id.join("peers.book")
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Load peers.book; a missing file means "no peers yet", corruption is fatal.
pub(crate) fn load_peer_book(path: &Path) -> Result<PeerBook> {
    match PeerBook::load(path) {
        Ok(book) => Ok(book),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(PeerBook::default()),
        Err(err) => Err(err).with_context(|| format!("cannot load peer book {}", path.display())),
    }
}

/// Insert a peer or update its name. Returns true when newly added; an
/// existing entry keeps its original paired time unless renamed.
pub(super) fn upsert_peer(
    book: &mut PeerBook,
    fingerprint: &Fingerprint,
    name: Option<&str>,
) -> bool {
    if let Some(existing) = book.get(fingerprint) {
        if let Some(name) = name {
            book.upsert(PeerRecord {
                fingerprint: *fingerprint,
                name: name.to_owned(),
                paired_unix_ms: existing.paired_unix_ms,
            });
        }
        return false;
    }
    book.upsert(PeerRecord {
        fingerprint: *fingerprint,
        name: name.unwrap_or("").to_owned(),
        paired_unix_ms: now_unix_ms(),
    });
    true
}

/// `pair add`: persist the fingerprint, return it plus whether it is new.
pub(super) fn pair_add(
    id: &Path,
    fingerprint_text: &str,
    name: Option<&str>,
) -> Result<(Fingerprint, bool)> {
    let fingerprint = parse_fingerprint(fingerprint_text)?;
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let added = upsert_peer(&mut book, &fingerprint, name);
    book.save(&path)
        .with_context(|| format!("cannot save {}", path.display()))?;
    Ok((fingerprint, added))
}

/// `pair remove`: drop the fingerprint (full 64-hex or a unique peers.book
/// prefix); error when it was never paired.
pub(super) fn pair_remove(id: &Path, fingerprint_text: &str) -> Result<Fingerprint> {
    let path = peer_book_path(id);
    let fingerprint = resolve_fingerprint_arg(fingerprint_text, &path)?;
    let mut book = load_peer_book(&path)?;
    if !book.remove(&fingerprint) {
        bail!(
            "fingerprint {} is not in {}; nothing removed (see `pair list`)",
            fingerprint.short_id(),
            path.display()
        );
    }
    book.save(&path)
        .with_context(|| format!("cannot save {}", path.display()))?;
    Ok(fingerprint)
}

/// After a successful send, record the peer trust-on-first-use. Returns true
/// when the book gained a new entry (existing entries keep their name).
pub(super) fn record_peer_after_send(id: &Path, fingerprint: &Fingerprint) -> Result<bool> {
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let added = upsert_peer(&mut book, fingerprint, None);
    if added {
        book.save(&path)
            .with_context(|| format!("cannot save {}", path.display()))?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// pair export / import (AKPAIR1 public-only pairing block)
// ---------------------------------------------------------------------------

/// First line of every exported pairing block (format version tag).
const PAIR_EXPORT_MAGIC: &str = "AKPAIR1";

/// The PUBLIC content of one AKPAIR1 block: who exported it (informational
/// on import; never merged into anything) plus the peer entries to merge.
/// SECURITY RED LINE (AGENTS.md): this format carries only public values --
/// device id, fingerprint, pairing code, peer book entries. The device.key
/// file (the 32-byte private seed) must never appear in it; tests assert a
/// sentinel seed's hex is absent from every export.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedPairBlock {
    /// Exporting device's public key, 64 hex characters.
    device: Option<String>,
    /// Exporting device's fingerprint, 64 hex characters.
    fingerprint: Option<String>,
    /// Exporting device's pairing code, "ddd-ddd".
    code: Option<String>,
    /// Valid peer lines, already deduped in-block (newer paired time wins).
    peers: Vec<PeerRecord>,
}

/// `pair export`: render this device's public pairing data as an AKPAIR1
/// block. Everything in the output is already shown by `pair code` and
/// `pair list`; no private material is read besides loading the key pair
/// whose public half becomes the `device` line.
pub(super) fn pair_export_text(id: &Path) -> Result<String> {
    let (key, _) = load_identity(id)?;
    let fingerprint = key.fingerprint();
    let book = load_peer_book(&peer_book_path(id))?;
    let mut out = String::from(PAIR_EXPORT_MAGIC);
    out.push('\n');
    out.push_str(&format!("device {}\n", hex_of(&key.public_key())));
    out.push_str(&format!("fingerprint {}\n", fingerprint.to_hex()));
    out.push_str(&format!(
        "code {}\n",
        ak_identity::pairing_code(&fingerprint)
    ));
    for peer in book.iter() {
        out.push_str(&format!(
            "peer {} {} {}\n",
            peer.fingerprint.to_hex(),
            peer.paired_unix_ms,
            export_name(&peer.name)
        ));
    }
    Ok(out)
}

/// Tab/newline bytes would break the one-line-per-record format, so they
/// become spaces exactly like ak-identity does when saving peers.book.
fn export_name(name: &str) -> String {
    name.chars()
        .map(|c| matches!(c, '\t' | '\n' | '\r').then_some(' ').unwrap_or(c))
        .collect()
}

/// Upsert `record` by fingerprint keeping the NEWER paired time; an exact
/// tie keeps the existing entry, so importing a block twice never rewrites
/// the book. The whole winning record (name included) replaces the loser.
fn upsert_newer(book: &mut PeerBook, record: PeerRecord) {
    match book.get(&record.fingerprint) {
        Some(existing) if existing.paired_unix_ms >= record.paired_unix_ms => {}
        _ => book.upsert(record),
    }
}

/// Parse one `peer <fingerprint> <paired_unix_ms> [name...]` line. The name
/// is everything after the single space following the timestamp (it may be
/// empty or contain spaces); names carrying tab/newline bytes can never have
/// come from our exporter and are rejected.
fn parse_peer_line(rest: &str) -> Option<PeerRecord> {
    let mut parts = rest.splitn(3, ' ');
    let fingerprint = Fingerprint::from_hex(parts.next()?)?;
    let paired_unix_ms: u64 = parts.next()?.parse().ok()?;
    let name = parts.next().unwrap_or("");
    if name.contains('\t') || name.contains('\n') || name.contains('\r') {
        return None;
    }
    Some(PeerRecord {
        fingerprint,
        name: name.to_owned(),
        paired_unix_ms,
    })
}

/// Pairing codes are always "ddd-ddd"; anything else cannot be a code line.
fn pairing_code_shape_ok(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 7
        && bytes[..3].iter().all(|b| b.is_ascii_digit())
        && bytes[3] == b'-'
        && bytes[4..].iter().all(|b| b.is_ascii_digit())
}

/// Parse an AKPAIR1 block. The first line must be the magic (anything else
/// is a fatal error: the block is not ours to interpret); every later line
/// is validated individually, with malformed lines counted (never fatal) so
/// a partially corrupted block still merges its intact records.
fn parse_pair_block(text: &str) -> Result<(ParsedPairBlock, usize)> {
    let mut lines = text.lines();
    match lines.next().map(|line| line.trim_end_matches('\r')) {
        Some(PAIR_EXPORT_MAGIC) => {}
        Some(other) => bail!(
            "not an AKPAIR1 pairing block (first line is {other:?}); generate it with \
             `ak-cli pair export` on the source device"
        ),
        None => bail!("empty input: expected an AKPAIR1 pairing block"),
    }

    let mut block = ParsedPairBlock::default();
    let mut staged = PeerBook::default();
    let mut bad = 0usize;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue; // tolerate blank lines and trailing newline noise
        }
        if let Some(rest) = line.strip_prefix("device ") {
            let value = rest.trim();
            if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
                block.device = Some(value.to_ascii_lowercase());
            } else {
                bad += 1;
            }
        } else if let Some(rest) = line.strip_prefix("fingerprint ") {
            match Fingerprint::from_hex(rest.trim()) {
                Some(_) => block.fingerprint = Some(rest.trim().to_ascii_lowercase()),
                None => bad += 1,
            }
        } else if let Some(rest) = line.strip_prefix("code ") {
            let value = rest.trim();
            if pairing_code_shape_ok(value) {
                block.code = Some(value.to_owned());
            } else {
                bad += 1;
            }
        } else if let Some(rest) = line.strip_prefix("peer ") {
            match parse_peer_line(rest) {
                Some(record) => upsert_newer(&mut staged, record),
                None => bad += 1,
            }
        } else {
            bad += 1;
        }
    }
    block.peers = staged.iter().cloned().collect();
    Ok((block, bad))
}

/// What one `pair import` changed, for both the user report and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PairImportOutcome {
    /// Distinct valid peer records the block carried (after in-block dedup).
    pub(super) peers: usize,
    pub(super) added: usize,
    pub(super) updated: usize,
    pub(super) unchanged: usize,
    /// Malformed non-header lines skipped.
    pub(super) bad: usize,
    /// Short id (first 8 hex) of the exporting device, when the block says.
    pub(super) source_short: Option<String>,
}

/// `pair import`: validate the block, merge its peers into peers.book by
/// fingerprint keeping the newer paired time, and report what happened. The
/// book is rewritten only when something actually changed.
pub(super) fn pair_import_text(id: &Path, text: &str) -> Result<PairImportOutcome> {
    let (block, bad) = parse_pair_block(text)?;
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let mut outcome = PairImportOutcome {
        bad,
        source_short: block.device.as_ref().map(|hex| hex[..8].to_owned()),
        ..PairImportOutcome::default()
    };
    for record in block.peers {
        outcome.peers += 1;
        match book.get(&record.fingerprint) {
            None => {
                book.upsert(record);
                outcome.added += 1;
            }
            Some(existing) if existing.paired_unix_ms >= record.paired_unix_ms => {
                outcome.unchanged += 1;
            }
            Some(_) => {
                book.upsert(record);
                outcome.updated += 1;
            }
        }
    }
    if outcome.added + outcome.updated > 0 {
        book.save(&path)
            .with_context(|| format!("cannot save {}", path.display()))?;
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Receive accept policy (--expect and the paired-peers default)
// ---------------------------------------------------------------------------

/// Accept policy for `receive`. The book policy re-reads peers.book on every
/// connection so `pair add` takes effect without restarting the loop.
#[derive(Debug, Clone)]
pub(super) enum ExpectPolicy {
    Any,
    Pinned(Fingerprint),
    PairingCode(String),
    Book(PathBuf),
}

impl ExpectPolicy {
    pub(super) fn parse(text: &str, book_path: &Path) -> Result<Self> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(ExpectPolicy::Any);
        }
        // Explicit `book` (ffi-v2 §6 note: keep the omitted-default
        // semantics but accept the word callers copy from the contract).
        if trimmed.eq_ignore_ascii_case("book") {
            return Ok(ExpectPolicy::Book(book_path.to_path_buf()));
        }
        let all_code_chars = trimmed
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'-' | b' '));
        let digits: String = trimmed
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .collect();
        if all_code_chars && digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(ExpectPolicy::PairingCode(trimmed.to_owned()));
        }
        // Full 64-hex values and unique peers.book prefixes share one path.
        let pinned = resolve_fingerprint_arg(trimmed, book_path)
            .with_context(|| format!("invalid --expect {trimmed:?}"))?;
        Ok(ExpectPolicy::Pinned(pinned))
    }

    pub(super) fn allows(&self, peer: &Fingerprint) -> bool {
        match self {
            ExpectPolicy::Any => true,
            ExpectPolicy::Pinned(pinned) => peer == pinned,
            ExpectPolicy::PairingCode(code) => ak_identity::verify_pairing_code(peer, code),
            ExpectPolicy::Book(path) => PeerBook::load(path)
                .map(|book| book.get(peer).is_some())
                .unwrap_or(false),
        }
    }

    /// Engine-facing consent callback: one shared evaluation path, cloned per
    /// loop iteration so book edits apply to later connections.
    pub(super) fn authorize(&self) -> Box<dyn Fn(&Fingerprint) -> bool + Send + Sync> {
        let policy = self.clone();
        Box::new(move |peer| policy.allows(peer))
    }
}

/// Resolve the receive accept policy. With --expect omitted, peers.book must
/// hold at least one peer; otherwise the user must authorize explicitly.
pub(super) fn resolve_expect(text: Option<&str>, book_path: &Path) -> Result<ExpectPolicy> {
    match text {
        Some(text) => ExpectPolicy::parse(text, book_path),
        None => {
            let book = load_peer_book(book_path)?;
            if book.iter().next().is_none() {
                bail!(
                    "no paired peers in {}: pair first with \
                     `ak-cli pair add <peer-fingerprint> --name NAME` (print it on the peer \
                     device with `ak-cli pair code`), or authorize this receive explicitly \
                     with `--expect <fingerprint|any>`",
                    book_path.display()
                );
            }
            Ok(ExpectPolicy::Book(book_path.to_path_buf()))
        }
    }
}
