//! Command-line prototype for AK-sending (single binary, many verbs).
//!
//! Verbs: `keygen`, `fingerprint`, `pair`, `receive`, `send`, `discover`,
//! `jobs`, `signal-server`, `probe`, `bench`, `doctor`. The binary never
//! talks to any file-carrying server; servers started here are
//! coordination-only (ADR-001).
//!
//! Pairing model: the local peer book (`peers.book` in the id directory)
//! holds fingerprint/name pairs. `receive` defaults to accepting only paired
//! fingerprints; a successful `send` records the receiver trust-on-first-use.
#![forbid(unsafe_code)]

mod doctor;
mod errors;

use std::io::Write as _;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use ak_core::handle::ConflictPolicy;
use ak_engine::{EngineError, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::tls::TlsBundle;
use ak_identity::{DeviceKeyPair, Fingerprint, PeerBook, PeerRecord};

#[derive(Debug, Parser)]
#[command(
    name = "ak-cli",
    version,
    about = "AK-sending direct-only prototype",
    after_help = "Typical flow:\n  1. ak-cli pair code                 (on the receiving device)\n  2. ak-cli receive --port 7100       (accepts paired peers only)\n  3. ak-cli send <path> [more paths...] --to <ip:7100> --fp <fingerprint>\n\nFingerprints accept the full 64 hex characters or a unique 8-63 character\nprefix of a peer already in peers.book. Multiple send paths travel in one\njob; each keeps its own top-level name on the receiver (paths with the same\nname must be sent in separate runs).\nAll transfers are direct device-to-device; no server ever carries file data."
)]
struct Cli {
    /// Identity directory holding device.key, peers.book, checkpoints
    /// (default: <home>/.ak-sending).
    #[arg(long, global = true)]
    id_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or show the device key, fingerprint, and pairing code.
    Keygen,
    /// Print the current fingerprint and pairing code (same as keygen on an
    /// existing identity directory).
    Fingerprint,
    /// Manage the peer book: paired device fingerprints (peers.book).
    Pair {
        #[command(subcommand)]
        cmd: PairCommand,
    },
    /// Receive transfers into a directory. By default keeps serving one job
    /// after another until Ctrl-C; --once handles exactly one job.
    Receive {
        /// Destination directory (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// UDP port to listen on (0 = auto).
        #[arg(long, default_value_t = 7100)]
        port: u16,
        /// Accept policy: omitted = only fingerprints in peers.book;
        /// 'any' = accept every peer (warning: unauthenticated);
        /// 64-hex fingerprint (or unique 8-63 character peers.book prefix)
        /// = pin that one peer;
        /// 6-digit pairing code = accept the peer whose code matches.
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Handle exactly one transfer, then exit (default: loop until Ctrl-C).
        #[arg(long, default_value_t = false)]
        once: bool,
        /// LAN-only mode: refuse non-local paths (no internet direct transfer).
        #[arg(long, default_value_t = false)]
        no_internet: bool,
        /// Idle timeout in seconds: abort when no frame arrives within this
        /// budget (idle timeout, not total run time). Applies to every loop
        /// iteration.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
    /// Send files and/or directories to a peer. The first path is the
    /// primary root; further paths join the same job, each keeping its own
    /// top-level name on the receiver (root names must be unique per run).
    /// On success the peer's fingerprint is recorded in peers.book (trust on
    /// first use).
    Send {
        /// Files/directories to send: the first is the primary root, any
        /// further paths are extra roots in the same job.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Peer address ip:port.
        #[arg(long)]
        to: SocketAddr,
        /// Peer fingerprint pinned for this transfer: 64 hex characters, or
        /// a unique 8-63 character prefix of a peer already in peers.book.
        /// Get it from the peer device via `ak-cli pair code`.
        #[arg(long)]
        fp: String,
        /// Local UDP port (0 = auto).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Overwrite policy on the receiver side: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Transfer files on parallel QUIC streams when the peer supports it.
        #[arg(long, default_value_t = false)]
        multistream: bool,
        /// LAN-only mode: refuse non-local paths (no internet direct transfer).
        #[arg(long, default_value_t = false)]
        no_internet: bool,
        /// Idle timeout in seconds: abort when no frame arrives within this
        /// budget (idle timeout, not total run time).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
    /// Discover peers on the local network via mDNS (bounded browse) and show
    /// pairing status for each candidate.
    Discover {
        /// Seconds to browse before reporting.
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Emit one machine-readable JSON array instead of the text table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show or clean transfer job checkpoints (resumable state records in
    /// <id-dir>/checkpoints). Part files (.akpart) are never touched.
    Jobs {
        #[command(subcommand)]
        cmd: Option<JobsCommand>,
    },
    /// Environment self-check for two-machine LAN debugging (REVIEW 4.3):
    /// identity, local networks, UDP bind, mDNS, optional STUN, disk space.
    /// Every probe is timeout-bounded; failures are reported (ok/warn/skip),
    /// never fatal.
    Doctor {
        /// STUN server ip:port to query (3s timeout). Omitted = skip the
        /// check (the default; LAN debugging does not need it).
        #[arg(long)]
        stun: Option<SocketAddr>,
        /// mDNS advertise+browse window in seconds (best effort).
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=30))]
        mdns_seconds: u64,
        /// Emit machine-readable JSON instead of the text report.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Run the coordination server (signaling + STUN; no file routes).
    SignalServer {
        #[arg(long, default_value_t = 7100)]
        tcp_port: u16,
        #[arg(long, default_value_t = 7101)]
        stun_port: u16,
    },
    /// Public direct-connect probe: register, exchange candidates, punch, QUIC.
    Probe {
        #[arg(long)]
        server: SocketAddr,
        /// Peer device id (64-hex fingerprint) to punch toward.
        #[arg(long)]
        peer: String,
        /// Skip the STUN mapping query.
        #[arg(long, default_value_t = false)]
        no_stun: bool,
    },
    /// In-process loopback benchmark: generate files, then push them through
    /// the real engine over 127.0.0.1 QUIC (evidence generator; no server
    /// carries file data). Default runs a single-stream and a multistream
    /// round and compares them.
    Bench {
        /// Total bytes to transfer, split evenly across --files files.
        #[arg(long, default_value_t = 268_435_456, value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
        /// Number of source files the data is split across.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..))]
        files: u32,
        /// Chunk size in KiB (1..=8192, the engine decode budget).
        #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=8192))]
        chunk_kb: u32,
        /// Select the multistream round with --once; without --once both
        /// rounds always run and this flag has no extra effect.
        #[arg(long, default_value_t = false)]
        multistream: bool,
        /// Run exactly one round (single-stream by default, multistream with
        /// --multistream) instead of both plus a comparison.
        #[arg(long, default_value_t = false)]
        once: bool,
        /// Emit machine-readable JSON instead of the text report.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PairCommand {
    /// Add (or rename) a paired peer in peers.book.
    Add {
        /// Peer fingerprint, 64 hex characters (see `pair code` on the peer).
        fingerprint: String,
        /// Friendly local name for the peer book entry.
        #[arg(long)]
        name: Option<String>,
    },
    /// List paired peers: name, short id, full fingerprint, paired time.
    List,
    /// Remove a paired peer from peers.book.
    Remove {
        /// Peer fingerprint: 64 hex characters, or a unique 8-63 character
        /// prefix of a peers.book entry.
        fingerprint: String,
    },
    /// Print this device's fingerprint and pairing code so the peer side can
    /// pair with this device.
    Code,
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    /// List checkpointed jobs, most recently updated first.
    List,
    /// Delete checkpoint record files (.akjob). Part files (.akpart) survive
    /// so interrupted transfers can still resume.
    Clean {
        /// Delete every checkpoint record, finished or not.
        #[arg(long, default_value_t = false)]
        all: bool,
        /// Delete only records of finished transfers (state completed).
        #[arg(long, default_value_t = false)]
        completed: bool,
    },
}

fn id_dir(cli: &Cli) -> PathBuf {
    let base = cli.id_dir.clone().unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".ak-sending")
    });
    std::fs::create_dir_all(&base).ok();
    base
}

fn parse_fingerprint(text: &str) -> Result<Fingerprint> {
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
fn resolve_fingerprint_arg(text: &str, book_path: &Path) -> Result<Fingerprint> {
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

fn now_unix_ms() -> u64 {
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
fn upsert_peer(book: &mut PeerBook, fingerprint: &Fingerprint, name: Option<&str>) -> bool {
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
fn pair_add(id: &Path, fingerprint_text: &str, name: Option<&str>) -> Result<(Fingerprint, bool)> {
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
fn pair_remove(id: &Path, fingerprint_text: &str) -> Result<Fingerprint> {
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
fn record_peer_after_send(id: &Path, fingerprint: &Fingerprint) -> Result<bool> {
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
// Receive accept policy (--expect and the paired-peers default)
// ---------------------------------------------------------------------------

/// Accept policy for `receive`. The book policy re-reads peers.book on every
/// connection so `pair add` takes effect without restarting the loop.
#[derive(Debug, Clone)]
enum ExpectPolicy {
    Any,
    Pinned(Fingerprint),
    PairingCode(String),
    Book(PathBuf),
}

impl ExpectPolicy {
    fn parse(text: &str, book_path: &Path) -> Result<Self> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(ExpectPolicy::Any);
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

    fn allows(&self, peer: &Fingerprint) -> bool {
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
    fn authorize(&self) -> Box<dyn Fn(&Fingerprint) -> bool + Send + Sync> {
        let policy = self.clone();
        Box::new(move |peer| policy.allows(peer))
    }
}

/// Resolve the receive accept policy. With --expect omitted, peers.book must
/// hold at least one peer; otherwise the user must authorize explicitly.
fn resolve_expect(text: Option<&str>, book_path: &Path) -> Result<ExpectPolicy> {
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

// ---------------------------------------------------------------------------
// Job checkpoints (read-only view of the AKJOB1 files the engine writes)
// ---------------------------------------------------------------------------

/// The display-relevant slice of one `*.akjob` checkpoint record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobEntry {
    job_hex: String,
    direction: &'static str,
    root: String,
    peer_short: Option<String>,
    total_files: u64,
    total_bytes: u64,
    state: String,
    updated_unix_ms: u64,
}

fn job_field<'a>(lines: &mut impl Iterator<Item = &'a str>, key: &str) -> Option<&'a str> {
    lines.next()?.strip_prefix(key)?.strip_prefix(' ')
}

/// Parse one AKJOB1 record (the format ak-store's FileCheckpointStore
/// writes). Unparsable input yields None; callers skip such files.
fn parse_job_record(text: &str) -> Option<JobEntry> {
    let mut lines = text.lines();
    if lines.next()? != "AKJOB1" {
        return None;
    }
    let job_hex = job_field(&mut lines, "job")?.to_owned();
    if hex_to_bytes(&job_hex).ok()?.len() != 16 {
        return None;
    }
    let direction = match job_field(&mut lines, "dir")? {
        "send" => "send",
        "receive" => "receive",
        _ => return None,
    };
    let root_hex = job_field(&mut lines, "root")?;
    let root = String::from_utf8(hex_to_bytes(root_hex).ok()?).ok()?;
    let peer = job_field(&mut lines, "peer")?;
    let peer_short = if peer == "-" {
        None
    } else {
        Fingerprint::from_hex(peer)?;
        Some(peer[..8].to_owned())
    };
    job_field(&mut lines, "chunk")?;
    let total = job_field(&mut lines, "total")?;
    let mut total_parts = total.split(' ');
    let total_files: u64 = total_parts.next()?.parse().ok()?;
    let total_bytes: u64 = total_parts.next()?.parse().ok()?;
    let state = job_field(&mut lines, "state")?.to_owned();
    job_field(&mut lines, "created")?;
    let updated_unix_ms: u64 = job_field(&mut lines, "updated")?.parse().ok()?;
    for line in lines {
        if !line.starts_with("file ") {
            return None;
        }
    }
    Some(JobEntry {
        job_hex,
        direction,
        root,
        peer_short,
        total_files,
        total_bytes,
        state,
        updated_unix_ms,
    })
}

/// All parsable `*.akjob` files under `dir`, newest first, with their paths.
fn list_job_files(dir: &Path) -> Vec<(PathBuf, JobEntry)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(record) = parse_job_record(&text) {
            out.push((path, record));
        }
    }
    out.sort_by_key(|(_, record)| std::cmp::Reverse(record.updated_unix_ms));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanScope {
    All,
    Completed,
}

/// Delete checkpoint record files per scope. Returns (removed, kept).
/// Part files (.akpart) are never touched, so interrupted transfers stay
/// resumable even after `--all`.
fn clean_job_records(dir: &Path, scope: CleanScope) -> Result<(usize, usize)> {
    let mut removed = 0usize;
    let mut kept = 0usize;
    match scope {
        CleanScope::All => {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((0, 0));
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("cannot read {}", dir.display()))
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
        CleanScope::Completed => {
            for (path, record) in list_job_files(dir) {
                if record.state != "completed" {
                    kept += 1;
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => kept += 1,
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
    }
    Ok((removed, kept))
}

// ---------------------------------------------------------------------------
// Small display helpers
// ---------------------------------------------------------------------------

/// Whole days since epoch -> civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

fn format_utc_ms(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Two-line fingerprint display used everywhere: short 8 first, full second.
fn fingerprint_lines(label: &str, fingerprint: &Fingerprint) -> String {
    format!(
        "{label}: {}\n{label} (full): {}",
        fingerprint.short_id(),
        fingerprint.to_hex()
    )
}

fn peer_display_name(name: &str) -> &str {
    if name.is_empty() {
        "(unnamed)"
    } else {
        name
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Escape a string for embedding in a JSON string literal.
pub(crate) fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => out.push(other),
        }
    }
    out
}

fn source_name(source: ak_discovery::CandidateSource) -> &'static str {
    match source {
        ak_discovery::CandidateSource::Manual => "manual",
        ak_discovery::CandidateSource::Signaling => "signaling",
        ak_discovery::CandidateSource::Mdns => "mdns",
    }
}

/// The fixed text printed for the probe SameEgress hint.
const SAME_EGRESS_NOTICE: &str = "same egress detected: prefer LAN candidates first";

/// One-shot guard for repeated events that should be announced exactly once:
/// [`OnceNotice::take`] returns the notice text the first time and None for
/// every later hit.
#[derive(Debug, Default)]
struct OnceNotice {
    shown: bool,
}

impl OnceNotice {
    fn new() -> Self {
        OnceNotice { shown: false }
    }

    fn take(&mut self) -> Option<&'static str> {
        if self.shown {
            None
        } else {
            self.shown = true;
            Some(SAME_EGRESS_NOTICE)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared command plumbing
// ---------------------------------------------------------------------------

fn conflict_policy(text: &str) -> Result<ConflictPolicy> {
    match text {
        "error" => Ok(ConflictPolicy::Fail),
        "rename" => Ok(ConflictPolicy::Rename),
        other => bail!("unknown conflict policy {other:?}; use error|rename"),
    }
}

/// Map the `--timeout` flag (seconds) onto the engine idle timeout. The flag
/// is an idle budget, not a total run time; clap already rejects 0.
fn idle_timeout_from_secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn checkpoint_dir(id: &Path) -> PathBuf {
    id.join("checkpoints")
}

fn load_identity(id: &Path) -> Result<(DeviceKeyPair, TlsBundle)> {
    let (key, created) = DeviceKeyPair::load_or_create(&id.join("device.key"))
        .with_context(|| format!("cannot load device key in {}", id.display()))?;
    if created {
        eprintln!(
            "created new device key in {}",
            id.join("device.key").display()
        );
    }
    let bundle = ak_identity::tls::make_self_signed(&key);
    Ok((key, bundle))
}

fn print_identity(key: &DeviceKeyPair) {
    let fingerprint = key.fingerprint();
    println!("device id: {}", hex_of(&key.public_key()));
    println!("{}", fingerprint_lines("fingerprint", &fingerprint));
    println!("pairing code: {}", ak_identity::pairing_code(&fingerprint));
}

fn print_progress(direction: &str, progress: &Progress) {
    match progress {
        Progress::Connected { peer } => println!("[{direction}] connected; peer {peer}"),
        Progress::Offered { files, bytes } => {
            println!(
                "[{direction}] offered {files} entries, {}",
                human_bytes(*bytes)
            )
        }
        Progress::Accepted { files, bytes } => {
            println!(
                "[{direction}] accepted {files} entries, {}",
                human_bytes(*bytes)
            )
        }
        Progress::Resuming { chunks } => {
            println!("[{direction}] resuming after {chunks} verified chunks")
        }
        Progress::FileStarted { path, bytes, .. } => {
            println!("[{direction}] {path} ({})", human_bytes(*bytes))
        }
        Progress::FileCommitted { path, .. } => println!("[{direction}] committed {path}"),
        Progress::Paused => println!("[{direction}] paused by peer; holding (no fallback exists)"),
        Progress::Resumed => println!("[{direction}] resumed"),
        Progress::FinalAck { verified } => println!("[{direction}] final ack verified={verified}"),
        Progress::Done => println!("[{direction}] done"),
        Progress::ManifestReceived => println!("[{direction}] manifest received"),
        _ => {}
    }
}

/// Terminal width the byte-progress line is clipped/padded to (80-col
/// friendly; the padding also erases leftovers of a previous, longer line).
const PROGRESS_LINE_WIDTH: usize = 79;

/// Clip text to `width` display columns on a char boundary, marking the cut
/// with "..." when the width allows it.
fn clip_line(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width >= 3 {
        let mut clipped: String = text.chars().take(width - 3).collect();
        clipped.push_str("...");
        clipped
    } else {
        text.chars().take(width).collect()
    }
}

/// Byte-level progress renders as one `\r`-refreshed line instead of one
/// println per event (the engine emits BytesAcked every 16 acks, which used
/// to flood the terminal). Whole-line events (Done, file-level, ...) first
/// erase an open progress line so log lines stay readable.
struct ProgressLine {
    direction: &'static str,
    open: std::sync::atomic::AtomicBool,
}

impl ProgressLine {
    fn new(direction: &'static str) -> Self {
        ProgressLine {
            direction,
            open: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn show(&self, progress: &Progress) {
        if let Progress::BytesAcked {
            file_index,
            cumulative_bytes,
            ..
        } = progress
        {
            let text = clip_line(
                &format!(
                    "[{}] file {file_index}: {} acknowledged",
                    self.direction,
                    human_bytes(*cumulative_bytes)
                ),
                PROGRESS_LINE_WIDTH,
            );
            let pad = " ".repeat(PROGRESS_LINE_WIDTH.saturating_sub(text.chars().count()));
            print!("\r{text}{pad}");
            let _ = std::io::stdout().flush();
            self.open.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        // Whole-line event: clear any open progress line, keep it whole.
        if self.open.swap(false, std::sync::atomic::Ordering::Relaxed) {
            print!("\r{}\r", " ".repeat(PROGRESS_LINE_WIDTH));
        }
        print_progress(self.direction, progress);
    }
}

fn progress_printer(direction: &'static str) -> Arc<dyn Fn(Progress) + Send + Sync> {
    let line = Arc::new(ProgressLine::new(direction));
    Arc::new(move |progress: Progress| {
        if std::env::var_os("AK_QUIET").is_none() {
            line.show(&progress);
        }
    })
}

fn short_of(peer: &Option<String>) -> String {
    peer.as_deref()
        .map(|hex| hex.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "?".to_owned())
}

/// One-line root listing for the summary: `roots: a, b, c` (primary first,
/// then extras in add order). None when the report carries no roots.
fn roots_line(report: &Report) -> Option<String> {
    if report.roots.is_empty() {
        None
    } else {
        Some(format!("roots: {}", report.roots.join(", ")))
    }
}

fn report_summary(report: &Report) {
    println!(
        "job {} complete: {} entries, {}, resumed {} chunks, peer {}",
        report
            .job_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        report.files,
        human_bytes(report.bytes),
        report.resumed_chunks,
        short_of(&report.peer_fingerprint)
    );
    if let Some(line) = roots_line(report) {
        println!("{line}");
    }
}

// ---------------------------------------------------------------------------
// bench: in-process loopback throughput measurement (evidence generator)
// ---------------------------------------------------------------------------

/// Fixed PRNG seed: identical bench runs generate identical bytes, so
/// numbers are comparable across runs and machines.
const BENCH_SEED: u64 = 0x00C0_FFEE_1234_5678;
/// Printed with every bench result so nobody mistakes loopback for a NIC.
const BENCH_DISCLAIMER: &str = "loopback throughput; real-NIC measurement needs two machines";
/// Idle budget for the bench engine runs; generous because a bench must
/// measure throughput, not trigger timeouts on a busy machine.
const BENCH_IDLE: Duration = Duration::from_secs(120);

/// Deterministic xorshift64 filler (Marsaglia). Fast and dependency-free;
/// NOT cryptographic -- it only needs pseudo-random bytes to exercise the
/// hashing, framing and QUIC paths.
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    /// A zero state would freeze the generator; map it to a fixed nonzero
    /// seed so every input produces a working stream.
    fn new(seed: u64) -> Self {
        Xorshift64 {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Fill `buf` with little-endian words of the generator stream; a
    /// trailing partial word takes the low bytes.
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

/// Split `total` bytes into exactly `files` sizes: even quotient, remainder
/// spread one byte at a time over the first files. Always sums to `total`.
fn bench_file_sizes(total: u64, files: u32) -> Vec<u64> {
    let base = total / u64::from(files);
    let mut rem = total % u64::from(files);
    (0..files)
        .map(|_| {
            if rem > 0 {
                rem -= 1;
                base + 1
            } else {
                base
            }
        })
        .collect()
}

/// Best-effort scoped temp directory for bench artifacts (source tree,
/// per-round destinations and checkpoints). The tempfile crate is a
/// dev-dependency of this crate and cannot be used here, hence the manual
/// guard.
struct BenchTempDir(PathBuf);

impl BenchTempDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "ak-cli-bench-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("cannot create bench dir {}", path.display()))?;
        Ok(BenchTempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for BenchTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stream `size` bytes of PRNG output into one file in 1 MiB steps (the
/// default 256 MiB must never be materialized in memory as a whole).
fn write_bench_file(path: &Path, size: u64, prng: &mut Xorshift64) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let step = remaining.min(buf.len() as u64) as usize;
        prng.fill(&mut buf[..step]);
        file.write_all(&buf[..step])?;
        remaining -= step as u64;
    }
    Ok(())
}

/// Generate the flat bench source tree (bench-0000.bin, ...). Per-file seeds
/// are distinct so files do not share prefixes.
fn write_bench_source(src: &Path, sizes: &[u64]) -> Result<()> {
    std::fs::create_dir_all(src).with_context(|| format!("cannot create {}", src.display()))?;
    for (index, size) in sizes.iter().enumerate() {
        let path = src.join(format!("bench-{index:04}.bin"));
        let seed = BENCH_SEED
            ^ (index as u64)
                .wrapping_add(1)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut prng = Xorshift64::new(seed);
        write_bench_file(&path, *size, &mut prng)?;
    }
    Ok(())
}

fn bench_quiet_progress(_: Progress) {}

/// One completed bench round. `duration` is the engine's own Report.duration
/// (send side), so the measurement covers exactly what the engine measured.
#[derive(Debug, Clone)]
struct BenchRound {
    mode: &'static str,
    duration: Duration,
    bytes: u64,
}

/// Throughput of a round in decimal MB/s and binary GiB/s.
fn throughput(bytes: u64, duration: Duration) -> (f64, f64) {
    let secs = duration.as_secs_f64().max(1e-9);
    (
        bytes as f64 / 1_000_000.0 / secs,
        bytes as f64 / 1_073_741_824.0 / secs,
    )
}

/// Run one full sender->receiver transfer of `src` through the real engine
/// over loopback QUIC (fresh key pair, destination and checkpoint dirs per
/// round so nothing resumes from a previous round). Both sides run inside
/// this process; the data path is still the full QUIC + TLS + engine stack.
async fn bench_round(
    src: &Path,
    dst: &Path,
    checkpoints: &Path,
    chunk_kb: u32,
    multistream: bool,
) -> Result<BenchRound> {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();
    let chunk = NonZeroU32::new(chunk_kb.saturating_mul(1024))
        .context("chunk size must be at least 1 KiB")?;

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = chunk;
    recv_spec.checkpoint_dir = checkpoints.to_path_buf();
    recv_spec.device_name = "ak-bench".into();
    recv_spec.idle_timeout = BENCH_IDLE;
    // Loopback still pins both identities: only our own sender may connect.
    let sender_fp = sender_key.fingerprint();
    recv_spec.authorize = Box::new(move |fp: &Fingerprint| *fp == sender_fp);

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move {
        ak_engine::receive_with_addr(recv_spec, &bench_quiet_progress, addr_tx).await
    });
    let addr = addr_rx
        .await
        .context("bench receiver failed to bind its loopback port")?;

    let mut send_spec = SendSpec::new(src.to_path_buf(), addr, receiver_fp, sender_bundle);
    send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    send_spec.chunk_size = chunk;
    send_spec.checkpoint_dir = checkpoints.to_path_buf();
    send_spec.device_name = "ak-bench".into();
    send_spec.idle_timeout = BENCH_IDLE;
    send_spec.multistream = multistream;
    let report = ak_engine::send(send_spec, &bench_quiet_progress)
        .await
        .context("bench send failed")?;
    let recv_report = receiver
        .await
        .context("bench receiver task failed")?
        .context("bench receive failed")?;

    // A Report already implies whole-digest verification on the receive
    // side; the byte totals must also agree between both ends.
    if report.bytes != recv_report.bytes {
        bail!(
            "bench integrity mismatch: sent {} bytes but received {}",
            report.bytes,
            recv_report.bytes
        );
    }
    Ok(BenchRound {
        mode: if multistream { "multistream" } else { "single" },
        duration: report.duration,
        bytes: report.bytes,
    })
}

/// Machine-readable bench result (reuses the JSON string escaper).
fn bench_json(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    rounds: &[BenchRound],
    ratio: Option<f64>,
) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"bytes\": {bytes},\n"));
    out.push_str(&format!("  \"files\": {files},\n"));
    out.push_str(&format!("  \"chunk_kb\": {chunk_kb},\n"));
    out.push_str("  \"rounds\": [\n");
    for (index, round) in rounds.iter().enumerate() {
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        let comma = if index + 1 < rounds.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"mode\": \"{}\", \"duration_ms\": {:.3}, \"bytes\": {}, \
             \"mb_per_s\": {:.3}, \"gib_per_s\": {:.6}}}{}\n",
            json_escape(round.mode),
            round.duration.as_secs_f64() * 1000.0,
            round.bytes,
            mb_per_s,
            gib_per_s,
            comma
        ));
    }
    out.push_str("  ],\n");
    match ratio {
        Some(ratio) => out.push_str(&format!("  \"ratio\": {ratio:.4},\n")),
        None => out.push_str("  \"ratio\": null,\n"),
    }
    out.push_str(&format!(
        "  \"note\": \"{}\"\n}}",
        json_escape(BENCH_DISCLAIMER)
    ));
    out
}

async fn bench_command(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    multistream: bool,
    once: bool,
    json: bool,
) -> Result<()> {
    // Default: both rounds plus a comparison. --once runs exactly one round
    // (single-stream, or multistream when --multistream is also given).
    let run_single = !once || !multistream;
    let run_multi = !once || multistream;
    let mut plan: Vec<(&'static str, bool)> = Vec::new();
    if run_single {
        plan.push(("single-stream", false));
    }
    if run_multi {
        plan.push(("multistream", true));
    }

    let dir = BenchTempDir::new()?;
    let src = dir.path().join("src");
    let sizes = bench_file_sizes(bytes, files);
    eprintln!(
        "ak-cli bench: {} across {} files, chunk {chunk_kb} KiB; \
         generating deterministic source into {}",
        human_bytes(bytes),
        sizes.len(),
        src.display()
    );
    write_bench_source(&src, &sizes)?;

    let total = plan.len();
    let mut rounds: Vec<BenchRound> = Vec::new();
    for (index, (label, round_multistream)) in plan.iter().enumerate() {
        let dst = dir.path().join(format!("dst-{index}"));
        let checkpoints = dir.path().join(format!("ckpt-{index}"));
        std::fs::create_dir_all(&dst)
            .with_context(|| format!("cannot create {}", dst.display()))?;
        std::fs::create_dir_all(&checkpoints)
            .with_context(|| format!("cannot create {}", checkpoints.display()))?;
        eprintln!("round {}/{total} ({label}): running...", index + 1);
        let round = bench_round(&src, &dst, &checkpoints, chunk_kb, *round_multistream).await?;
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        eprintln!(
            "round {}/{total} ({label}): {} in {:.3} s -> {:.2} MB/s ({:.4} GiB/s)",
            index + 1,
            human_bytes(round.bytes),
            round.duration.as_secs_f64(),
            mb_per_s,
            gib_per_s
        );
        rounds.push(round);
    }

    // Compare only when both modes ran (rounds[0]=single, rounds[1]=multi).
    let ratio = if rounds.len() == 2 {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        Some(multi_mb / single_mb.max(1e-9))
    } else {
        None
    };

    if json {
        println!("{}", bench_json(bytes, files, chunk_kb, &rounds, ratio));
        return Ok(());
    }
    if let Some(ratio) = ratio {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        println!(
            "comparison: multistream {ratio:.2}x single-stream \
             ({multi_mb:.2} vs {single_mb:.2} MB/s)"
        );
    }
    println!("note: {BENCH_DISCLAIMER}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn pair_command(id: &Path, cmd: PairCommand) -> Result<()> {
    match cmd {
        PairCommand::Add { fingerprint, name } => {
            let (fp, added) = pair_add(id, &fingerprint, name.as_deref())?;
            let shown = name
                .as_deref()
                .map(str::to_owned)
                .unwrap_or_else(|| "(unnamed)".to_owned());
            if added {
                println!("paired {shown} ({})", fp.short_id());
            } else {
                println!(
                    "updated {shown} ({}) -- entry already existed",
                    fp.short_id()
                );
            }
            println!("fingerprint (full): {}", fp.to_hex());
            println!(
                "that peer's pairing code must be {} -- verify it on the peer device",
                ak_identity::pairing_code(&fp)
            );
            Ok(())
        }
        PairCommand::List => {
            let path = peer_book_path(id);
            let book = load_peer_book(&path)?;
            let count = book.iter().count();
            if count == 0 {
                println!("no paired peers yet; use `pair add <peer-fingerprint> --name NAME`");
                return Ok(());
            }
            println!("{count} paired peer(s) in {}", path.display());
            for peer in book.iter() {
                println!(
                    "{}  {}  paired {}",
                    peer_display_name(&peer.name),
                    peer.fingerprint.short_id(),
                    format_utc_ms(peer.paired_unix_ms)
                );
                println!("  fingerprint (full): {}", peer.fingerprint.to_hex());
            }
            Ok(())
        }
        PairCommand::Remove { fingerprint } => {
            let fp = pair_remove(id, &fingerprint)?;
            println!(
                "unpaired {} (removed from {})",
                fp.short_id(),
                peer_book_path(id).display()
            );
            Ok(())
        }
        PairCommand::Code => {
            let (key, _) = load_identity(id)?;
            print_identity(&key);
            Ok(())
        }
    }
}

fn jobs_command(id: &Path, cmd: Option<JobsCommand>) -> Result<()> {
    let dir = checkpoint_dir(id);
    match cmd {
        None | Some(JobsCommand::List) => {
            let records = list_job_files(&dir);
            if records.is_empty() {
                println!("no job checkpoints in {}", dir.display());
                return Ok(());
            }
            println!("{} job checkpoint(s) in {}", records.len(), dir.display());
            for (_, record) in &records {
                println!(
                    "{} {} {} ({} files, {}) updated {}",
                    record.job_hex,
                    record.direction,
                    record.state,
                    record.total_files,
                    human_bytes(record.total_bytes),
                    format_utc_ms(record.updated_unix_ms)
                );
                println!(
                    "  root {} peer {}",
                    record.root,
                    record.peer_short.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        Some(JobsCommand::Clean { all, completed }) => {
            if all && completed {
                bail!("choose one of --all or --completed, not both");
            }
            let scope = if all {
                CleanScope::All
            } else if completed {
                CleanScope::Completed
            } else {
                bail!(
                    "jobs clean needs --all or --completed (part files .akpart are never touched)"
                );
            };
            let (removed, kept) = clean_job_records(&dir, scope)?;
            println!(
                "removed {removed} checkpoint record(s), {kept} kept, in {}; \
                 .akpart part files are untouched",
                dir.display()
            );
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_command(
    id: &Path,
    out: PathBuf,
    port: u16,
    expect: Option<String>,
    conflict: String,
    once: bool,
    no_internet: bool,
    timeout: u64,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let fp = key.fingerprint();
    let policy = resolve_expect(expect.as_deref(), &peer_book_path(id))?;
    let conflict = conflict_policy(&conflict)?;
    let checkpoints = checkpoint_dir(id);
    let device_name = whoami();
    let idle = idle_timeout_from_secs(timeout);

    match &policy {
        ExpectPolicy::Any => println!(
            "warning: --expect any accepts EVERY peer; anyone who can reach \
             this port may send files"
        ),
        ExpectPolicy::Pinned(pinned) => println!(
            "accept policy: only peer {} (its pairing code must be {})",
            pinned.short_id(),
            ak_identity::pairing_code(pinned)
        ),
        ExpectPolicy::PairingCode(code) => {
            println!("accept policy: only the peer presenting pairing code {code}")
        }
        ExpectPolicy::Book(path) => match PeerBook::load(path) {
            Ok(book) => println!(
                "accept policy: paired peers only ({} in {})",
                book.iter().count(),
                path.display()
            ),
            Err(_) => println!("accept policy: paired peers only ({})", path.display()),
        },
    }
    println!(
        "note: Windows may prompt to allow ak-cli through the firewall on \
         first listen; granting it only opens this direct transfer port"
    );

    let mut first = true;
    // The previous iteration's QUIC endpoint releases its UDP socket
    // asynchronously after drop, so an immediate rebind can hit AddrInUse.
    // Retry briefly instead of failing the whole loop.
    const MAX_REBIND_RETRIES: u32 = 50;
    let mut rebind_retries = 0u32;
    loop {
        let mut spec = ReceiveSpec::new(out.clone(), bundle.clone());
        spec.bind = SocketAddr::from(([0, 0, 0, 0], port));
        spec.conflict = conflict;
        spec.checkpoint_dir = checkpoints.clone();
        spec.device_name = device_name.clone();
        spec.idle_timeout = idle;
        spec.policy.allow_internet = !no_internet;
        spec.authorize = policy.authorize();
        let printer = progress_printer("recv");
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await },
            );
        if first {
            let addr = addr_rx.await.context("receiver failed to bind")?;
            println!("receiving into {}", out.display());
            println!("address: {addr}");
            println!("{}", fingerprint_lines("fingerprint", &fp));
            println!("pairing code: {}", ak_identity::pairing_code(&fp));
            println!("waiting for sender (Ctrl-C stops)...");
            first = false;
        } else {
            // Bind errors surface through the task result below.
            let _ = addr_rx.await;
        }
        let outcome = tokio::select! {
            joined = receiver => joined,
            _ = tokio::signal::ctrl_c() => {
                println!("ctrl-c: stopped receiving (partial files stay resumable)");
                return Ok(());
            }
        };
        match outcome {
            Ok(Ok(report)) => {
                report_summary(&report);
                if once {
                    return Ok(());
                }
                rebind_retries = 0;
                println!("waiting for next sender (Ctrl-C stops)...");
            }
            Ok(Err(EngineError::Connect(ak_net::ConnectError::Unauthorized))) if !once => {
                println!("rejected a peer outside the accept policy; still waiting (Ctrl-C stops)");
            }
            Ok(Err(EngineError::Io(err)))
                if !once
                    && err.kind() == std::io::ErrorKind::AddrInUse
                    && rebind_retries < MAX_REBIND_RETRIES =>
            {
                rebind_retries += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(join) => bail!("receiver task failed: {join}"),
        }
    }
}

/// Canonicalize the send paths: the first is the primary root, the rest
/// become extra roots in the order given (the engine enforces unique root
/// names across the whole job).
fn send_roots(paths: &[PathBuf]) -> Result<(PathBuf, Vec<PathBuf>)> {
    let Some(first) = paths.first() else {
        bail!("send needs at least one file or directory");
    };
    let primary = first
        .canonicalize()
        .with_context(|| first.display().to_string())?;
    let extras = paths[1..]
        .iter()
        .map(|path| {
            path.canonicalize()
                .with_context(|| path.display().to_string())
        })
        .collect::<Result<Vec<PathBuf>>>()?;
    Ok((primary, extras))
}

/// The engine's duplicate-root refusal as an io error, so the CLI can reword
/// it into a friendly hint. Any other InvalidInput (e.g. an unusable root
/// name) passes through untouched.
fn duplicate_roots_error(err: &EngineError) -> Option<&std::io::Error> {
    match err {
        EngineError::Io(io)
            if io.kind() == std::io::ErrorKind::InvalidInput
                && io.to_string().starts_with("duplicate transfer root names") =>
        {
            Some(io)
        }
        _ => None,
    }
}

// Options arrive flat from clap; grouping them into a struct here would only
// mirror the enum variant one-to-one (same allowance as the engine drivers).
#[allow(clippy::too_many_arguments)]
async fn send_command(
    id: &Path,
    paths: Vec<PathBuf>,
    to: SocketAddr,
    fp_text: String,
    port: u16,
    conflict: String,
    multistream: bool,
    no_internet: bool,
    timeout: u64,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let pinned = resolve_fingerprint_arg(&fp_text, &peer_book_path(id))?;
    let (root, extras) = send_roots(&paths)?;
    println!(
        "sending {} path(s) to peer {} (its pairing code must be {})",
        1 + extras.len(),
        pinned.short_id(),
        ak_identity::pairing_code(&pinned)
    );
    let mut spec = SendSpec::new(root, to, pinned, bundle);
    for extra in extras {
        spec.add_root(extra);
    }
    spec.bind = SocketAddr::from(([0, 0, 0, 0], port));
    spec.conflict = conflict_policy(&conflict)?;
    spec.checkpoint_dir = checkpoint_dir(id);
    spec.device_name = whoami();
    spec.idle_timeout = idle_timeout_from_secs(timeout);
    spec.multistream = multistream;
    spec.policy.allow_internet = !no_internet;
    let _ = key;
    let printer = progress_printer("send");
    let report = match ak_engine::send(spec, &*printer).await {
        Ok(report) => report,
        Err(err) => {
            if let Some(io) = duplicate_roots_error(&err) {
                bail!("two paths have the same name: please send them in separate runs ({io})");
            }
            // Friendly copy for the common failure shapes; the hint rides as
            // an anyhow context so the original error chain stays visible.
            if let Some(hint) = errors::friendly_send_hint(&err) {
                return Err(anyhow::Error::from(err).context(hint));
            }
            return Err(err.into());
        }
    };
    report_summary(&report);
    // TOFU: remember a successfully reached receiver.
    if record_peer_after_send(id, &pinned)? {
        println!(
            "recorded peer {} in {} (trust on first use; its pairing code is {})",
            pinned.short_id(),
            peer_book_path(id).display(),
            ak_identity::pairing_code(&pinned)
        );
    } else {
        println!("peer {} already paired", pinned.short_id());
    }
    Ok(())
}

async fn discover_command(id: &Path, seconds: u64, json: bool) -> Result<()> {
    let (key, _) = load_identity(id)?;
    let fp = key.fingerprint();
    let book = load_peer_book(&peer_book_path(id))?;
    let mut mdns = ak_discovery::MdnsDiscovery::new().context("mDNS daemon unavailable")?;
    mdns.advertise(7100, &fp, &whoami())
        .context("mDNS advertise failed (multicast may be blocked)")?;
    let nets: Vec<_> = ak_net::classify::local_nets()?
        .into_iter()
        .map(|net| ak_discovery::ak_net_shim::Net {
            addr: net.addr,
            prefix_len: net.prefix_len,
        })
        .collect();
    let found = mdns.browse(Duration::from_secs(seconds))?;
    let ranked = ak_discovery::rank(found, &nets);
    if json {
        println!("{}", discover_json(&ranked, &book));
        return Ok(());
    }
    println!("address\tfingerprint\tpairing\tsource");
    for candidate in ranked {
        let short = candidate
            .fingerprint
            .map(|f| f.short_id())
            .unwrap_or_else(|| "-".to_owned());
        let pairing = match &candidate.fingerprint {
            Some(peer_fp) => match book.get(peer_fp) {
                Some(record) if !record.name.is_empty() => record.name.clone(),
                Some(_) => "paired".to_owned(),
                None => "unpaired".to_owned(),
            },
            None => "no-fingerprint".to_owned(),
        };
        println!(
            "{}\t{short}\t{pairing}\t{}",
            candidate.addr,
            source_name(candidate.source)
        );
    }
    Ok(())
}

/// Machine-readable JSON array of ranked discovery candidates.
/// `name` is the peers.book name when paired, else null.
fn discover_json(candidates: &[ak_discovery::PeerCandidate], book: &PeerBook) -> String {
    let mut out = String::from("[\n");
    for (index, candidate) in candidates.iter().enumerate() {
        let fingerprint = match &candidate.fingerprint {
            Some(fp) => format!("\"{}\"", fp.to_hex()),
            None => "null".to_owned(),
        };
        let name = match &candidate.fingerprint {
            Some(fp) => book.get(fp).map(|record| record.name.clone()),
            None => None,
        };
        let name_json = match name {
            Some(name) if !name.is_empty() => format!("\"{}\"", json_escape(&name)),
            _ => "null".to_owned(),
        };
        let paired = candidate
            .fingerprint
            .is_some_and(|fp| book.get(&fp).is_some());
        let comma = if index + 1 < candidates.len() {
            ","
        } else {
            ""
        };
        out.push_str(&format!(
            "  {{\"addr\": \"{}\", \"fingerprint\": {fingerprint}, \"paired\": {paired}, \
             \"name\": {name_json}, \"source\": \"{}\"}}{comma}\n",
            candidate.addr,
            source_name(candidate.source)
        ));
    }
    out.push(']');
    out
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let id = id_dir(&cli);
    match cli.command {
        Command::Keygen => {
            let (key, _) = load_identity(&id)?;
            print_identity(&key);
            Ok(())
        }
        Command::Fingerprint => {
            let (key, _) = load_identity(&id)?;
            print_identity(&key);
            Ok(())
        }
        Command::Pair { cmd } => pair_command(&id, cmd),
        Command::Receive {
            out,
            port,
            expect,
            conflict,
            once,
            no_internet,
            timeout,
        } => {
            receive_command(&id, out, port, expect, conflict, once, no_internet, timeout).await
        }
        Command::Send {
            paths,
            to,
            fp,
            port,
            conflict,
            multistream,
            no_internet,
            timeout,
        } => {
            send_command(&id, paths, to, fp, port, conflict, multistream, no_internet, timeout)
                .await
        }
        Command::Discover { seconds, json } => discover_command(&id, seconds, json).await,
        Command::Jobs { cmd } => jobs_command(&id, cmd),
        Command::Doctor {
            stun,
            mdns_seconds,
            json,
        } => doctor::doctor_command(&id, stun, mdns_seconds, json).await,
        Command::SignalServer {
            tcp_port,
            stun_port,
        } => {
            let config = ak_signal::server::Config {
                tcp: SocketAddr::from(([0, 0, 0, 0], tcp_port)),
                stun_udp: SocketAddr::from(([0, 0, 0, 0], stun_port)),
                ..ak_signal::server::Config::default()
            };
            println!(
                "signaling on {} (STUN {}); no file routes exist",
                config.tcp, config.stun_udp
            );
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let server =
                tokio::spawn(async move { ak_signal::server::run(config, shutdown_rx).await });
            tokio::signal::ctrl_c().await.ok();
            let _ = shutdown_tx.send(true);
            server.await??;
            Ok(())
        }
        Command::Probe {
            server,
            peer,
            no_stun,
        } => probe_command(&id, server, peer, no_stun).await,
        Command::Bench {
            bytes,
            files,
            chunk_kb,
            multistream,
            once,
            json,
        } => bench_command(bytes, files, chunk_kb, multistream, once, json).await,
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "ak-device".into())
}

async fn probe_command(id: &Path, server: SocketAddr, peer: String, no_stun: bool) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let peer_id_hex = peer.trim().to_ascii_lowercase();
    let target: [u8; 32] = {
        let bytes = hex_to_bytes(&peer_id_hex).context("peer must be 64 hex characters")?;
        let mut id = [0u8; 32];
        if bytes.len() != 32 {
            bail!("peer fingerprint must be 64 hex characters");
        }
        id.copy_from_slice(&bytes);
        id
    };
    println!("device id: {}", hex_of(&key.public_key()));
    println!("{}", fingerprint_lines("fingerprint", &key.fingerprint()));

    // 1. Bind the transfer UDP socket first: every mapped/public candidate
    //    must belong to the socket we would actually transfer on.
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let local = socket.local_addr()?;
    println!("transfer socket: {local}");

    // 2. Local candidates: real interface addresses plus loopback (never the
    //    unspecified 0.0.0.0 bind address, which is not dialable), then the
    //    STUN mapping via the same socket (coordination plane, no data).
    let mut candidates: Vec<SocketAddr> = ak_net::classify::local_nets()?
        .into_iter()
        .map(|net| SocketAddr::new(net.addr, local.port()))
        .collect();
    candidates.push(SocketAddr::from(([127, 0, 0, 1], local.port())));
    if !no_stun {
        let stun_addr = SocketAddr::new(server.ip(), server.port() + 1);
        match tokio::time::timeout(
            Duration::from_secs(6),
            ak_signal::stun::query_on(&socket, stun_addr, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(mapped)) => {
                println!("STUN mapped address: {mapped}");
                candidates.push(mapped);
            }
            Ok(Err(error)) => println!("STUN query failed: {error}"),
            Err(_) => println!("STUN query timed out"),
        }
    }

    // 3. Signaling: register candidates and open a session toward the peer.
    let client = ak_signal::client::SignalClient::connect(server, &key, &whoami())
        .await
        .context("signaling connect failed")?;
    candidates.sort();
    candidates.dedup();
    client.register(&candidates).await?;
    println!("registered {} candidates", candidates.len());
    client.connect_peer(target).await?;

    // 4. Exchange signals until we see the peer's candidates, then punch.
    let mut peer_candidates: Vec<SocketAddr> = Vec::new();
    let mut reachable: Option<SocketAddr> = None;
    let mut same_egress = OnceNotice::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    'signal: while Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next()).await??;
        match message {
            ak_signal::codec::CtrlMessage::SessionOpened { session, peer } => {
                println!("session open with peer {}", hex_of(&peer));
                client
                    .send_signal(
                        session,
                        ak_signal::codec::PeerSignal::Candidates(candidates.clone()),
                    )
                    .await?;
            }
            ak_signal::codec::CtrlMessage::Signal { signal, .. } => match signal {
                ak_signal::codec::PeerSignal::Candidates(list) => {
                    peer_candidates = list;
                    println!("peer candidates: {peer_candidates:?}");
                    let report = ak_net::holepunch::probe(
                        &socket,
                        &peer_candidates,
                        Duration::from_millis(200),
                        Duration::from_secs(5),
                    )
                    .await?;
                    println!(
                        "punch: sent {} recv {} reachable {:?} from {:?}",
                        report.packets_sent,
                        report.packets_received,
                        report.reachable,
                        report.peer_observed_from
                    );
                    if let Some(first) = report.reachable.first() {
                        reachable = Some(*first);
                        break 'signal;
                    }
                }
                ak_signal::codec::PeerSignal::SameEgress => {
                    // The server may repeat the hint; announce it once.
                    if let Some(text) = same_egress.take() {
                        println!("{text}");
                    }
                }
                other => println!("signal: {other:?}"),
            },
            other => println!("server: {other:?}"),
        }
    }
    if let Some(target_addr) = reachable {
        // 5. Real verification: QUIC handshake with pinned identity on the
        // SAME socket that punched (keeps NAT pinholes valid). Role is
        // assigned deterministically from the two device ids so both sides
        // agree without extra signaling: smaller key connects.
        let our_key = key.public_key();
        let pinned = Fingerprint::from_public_key(&target);
        let std_socket = socket.into_std()?;
        let endpoint = ak_net::DirectEndpoint::bind_with_socket(std_socket, &bundle).await?;
        let handshake = if our_key < target {
            println!("role: client, dialing {target_addr}");
            endpoint.connect(target_addr, &pinned).await
        } else {
            println!("role: server, waiting on the punched path");
            let authorize = move |fp: &Fingerprint| fp == &pinned;
            match tokio::time::timeout(Duration::from_secs(15), endpoint.accept(&authorize)).await {
                Ok(result) => result,
                Err(_) => Err(ak_net::ConnectError::Timeout(Duration::from_secs(15))),
            }
        };
        match handshake {
            Ok(channel) => {
                println!(
                    "QUIC handshake OK path={:?} peer={}",
                    channel.path(),
                    channel.peer_fingerprint().to_hex()
                );
            }
            Err(error) => println!("QUIC handshake failed after punch: {error}"),
        }
    } else if peer_candidates.is_empty() {
        println!(
            "no peer candidates received; peer may be offline or behind an unreachable network"
        );
    } else {
        println!("no candidate answered probes; this network pair may not support direct connections (no relay fallback exists)");
    }
    let _ = client.close().await;
    println!("probe finished (direct-only: no relay fallback exists)");
    Ok(())
}

fn hex_to_bytes(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("odd-length hex string");
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0])
            .with_context(|| format!("invalid hex digit '{}'", pair[0] as char))?;
        let low = hex_digit(pair[1])
            .with_context(|| format!("invalid hex digit '{}'", pair[1] as char))?;
        out.push(high * 16 + low);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::DeviceKeyPair;
    use ak_store::{
        CheckpointStore, Direction, FileCheckpoint, FileCheckpointStore,
        TaskRecord as StoredTaskRecord,
    };

    /// Unique temp directory helper (tests may use expect/unwrap).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ak-cli-tests-{tag}-{}-{}",
                std::process::id(),
                now_unix_ms()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn random_fp() -> Fingerprint {
        DeviceKeyPair::generate().fingerprint()
    }

    #[test]
    fn hex_helpers_round_trip() {
        assert_eq!(hex_to_bytes("00ff10").unwrap(), vec![0, 255, 16]);
        assert!(hex_to_bytes("0").is_err());
        assert!(hex_to_bytes("zz").is_err());
    }

    #[test]
    fn conflict_policy_parses_known_values() {
        assert_eq!(conflict_policy("error").unwrap(), ConflictPolicy::Fail);
        assert_eq!(conflict_policy("rename").unwrap(), ConflictPolicy::Rename);
        assert!(conflict_policy("overwrite").is_err());
    }

    #[test]
    fn fingerprint_parser_rejects_junk() {
        assert!(parse_fingerprint("not-a-fingerprint").is_err());
        assert!(parse_fingerprint(&"x".repeat(64)).is_err());
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.00 GiB");
    }

    // -- expect policy ------------------------------------------------------

    #[test]
    fn expect_policy_parses_all_forms() {
        let dir = TempDir::new("expect-forms");
        let book_path = dir.path().join("peers.book");
        let fp = random_fp();
        assert!(matches!(
            ExpectPolicy::parse("any", &book_path).unwrap(),
            ExpectPolicy::Any
        ));
        assert!(matches!(
            ExpectPolicy::parse("ANY", &book_path).unwrap(),
            ExpectPolicy::Any
        ));
        assert!(matches!(
            ExpectPolicy::parse(&fp.to_hex(), &book_path).unwrap(),
            ExpectPolicy::Pinned(parsed) if parsed == fp
        ));
        assert!(matches!(
            ExpectPolicy::parse("123-456", &book_path).unwrap(),
            ExpectPolicy::PairingCode(code) if code == "123-456"
        ));
        assert!(matches!(
            ExpectPolicy::parse(" 123 456 ", &book_path).unwrap(),
            ExpectPolicy::PairingCode(_)
        ));
        // Separator placement is lenient (ak-identity filters separators);
        // "123-45-6" means the same code as "123456".
        assert!(matches!(
            ExpectPolicy::parse("123-45-6", &book_path).unwrap(),
            ExpectPolicy::PairingCode(code) if code == "123-45-6"
        ));
        for junk in ["junk", "", "12345", "1234567", "12a456", &"x".repeat(63)] {
            assert!(
                ExpectPolicy::parse(junk, &book_path).is_err(),
                "unexpectedly accepted {junk:?}"
            );
        }
    }

    #[test]
    fn expect_policy_allows_only_matching_peers() {
        let dir = TempDir::new("expect-allows");
        let book_path = dir.path().join("peers.book");
        let pinned = random_fp();
        let stranger = random_fp();
        let policy = ExpectPolicy::parse(&pinned.to_hex(), &book_path).unwrap();
        assert!(policy.allows(&pinned));
        assert!(!policy.allows(&stranger));

        let any = ExpectPolicy::parse("any", &book_path).unwrap();
        assert!(any.allows(&pinned) && any.allows(&stranger));

        let code = ak_identity::pairing_code(&pinned);
        let policy = ExpectPolicy::parse(&code, &book_path).unwrap();
        assert!(policy.allows(&pinned));
        assert!(!policy.allows(&stranger));
    }

    #[test]
    fn expect_policy_completes_unique_book_prefix() {
        let dir = TempDir::new("expect-prefix");
        let book_path = dir.path().join("peers.book");
        let pinned = random_fp();
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &pinned, Some("laptop"));
        book.save(&book_path).unwrap();

        // A unique prefix pins the same peer as the full fingerprint.
        let policy = ExpectPolicy::parse(&pinned.to_hex()[..12], &book_path).unwrap();
        assert!(matches!(
            policy,
            ExpectPolicy::Pinned(parsed) if parsed == pinned
        ));
        assert!(policy.allows(&pinned));
        assert!(!policy.allows(&random_fp()));
    }

    #[test]
    fn book_policy_rereads_peers_book_per_call() {
        let dir = TempDir::new("book-policy");
        let path = dir.path().join("peers.book");
        let peer = random_fp();
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &peer, Some("laptop"));
        book.save(&path).unwrap();

        let policy = ExpectPolicy::Book(path.clone());
        assert!(policy.allows(&peer));
        assert!(!policy.allows(&random_fp()));

        // Removing the peer takes effect without rebuilding the policy.
        let mut book = PeerBook::load(&path).unwrap();
        assert!(book.remove(&peer));
        book.save(&path).unwrap();
        assert!(!policy.allows(&peer));

        // A missing book denies everyone rather than opening up.
        let missing = ExpectPolicy::Book(dir.path().join("nope.book"));
        assert!(!missing.allows(&peer));
    }

    #[test]
    fn resolve_expect_defaults_to_book_and_requires_a_peer() {
        let dir = TempDir::new("resolve-expect");
        let book_path = peer_book_path(dir.path());

        // Missing or empty book: explicit authorization is required.
        assert!(resolve_expect(None, &book_path).is_err());
        PeerBook::default().save(&book_path).unwrap();
        assert!(resolve_expect(None, &book_path).is_err());

        let mut book = PeerBook::default();
        let peer = random_fp();
        upsert_peer(&mut book, &peer, Some("phone"));
        book.save(&book_path).unwrap();
        let policy = resolve_expect(None, &book_path).unwrap();
        assert!(policy.allows(&peer));
        assert!(!policy.allows(&random_fp()));

        // Explicit --expect overrides the book default.
        assert!(matches!(
            resolve_expect(Some("any"), &book_path).unwrap(),
            ExpectPolicy::Any
        ));
    }

    // -- fingerprint prefix resolution ---------------------------------------

    /// Deterministic fingerprint with a controlled hex head (first 4 bytes)
    /// and a distinguishing tail byte, for prefix-collision tests.
    fn fp_with_head(head: [u8; 4], tail: u8) -> Fingerprint {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&head);
        bytes[31] = tail;
        Fingerprint::from_bytes(bytes)
    }

    #[test]
    fn resolve_fingerprint_arg_completes_unique_prefix() {
        let dir = TempDir::new("fp-unique");
        let book_path = dir.path().join("peers.book");
        let a = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x11); // hex head abcdef01
        let b = fp_with_head([0x12, 0x34, 0x56, 0x78], 0x22); // hex head 12345678
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &a, Some("alpha"));
        upsert_peer(&mut book, &b, Some("beta"));
        book.save(&book_path).unwrap();

        // Minimum length (8), longer, and uppercase input all complete.
        assert_eq!(resolve_fingerprint_arg("abcdef01", &book_path).unwrap(), a);
        assert_eq!(
            resolve_fingerprint_arg(&a.to_hex()[..16], &book_path).unwrap(),
            a
        );
        assert_eq!(
            resolve_fingerprint_arg(&b.to_hex()[..20].to_uppercase(), &book_path).unwrap(),
            b
        );
        // Whitespace is trimmed like everywhere else.
        assert_eq!(
            resolve_fingerprint_arg(" 12345678 ", &book_path).unwrap(),
            b
        );
    }

    #[test]
    fn resolve_fingerprint_arg_rejects_ambiguous_and_unknown_prefixes() {
        let dir = TempDir::new("fp-ambiguous");
        let book_path = dir.path().join("peers.book");
        let a = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x11);
        let b = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x33); // same head
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &a, Some("alpha"));
        upsert_peer(&mut book, &b, Some("beta"));
        book.save(&book_path).unwrap();

        let ambiguous = resolve_fingerprint_arg("abcdef01", &book_path)
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("ambiguous"), "{ambiguous}");
        assert!(ambiguous.contains("matches 2 peers"), "{ambiguous}");
        // A longer prefix disambiguates again (tails differ at hex char 62+).
        assert_eq!(
            resolve_fingerprint_arg(&a.to_hex()[..63], &book_path).unwrap(),
            a
        );

        let unknown = resolve_fingerprint_arg("ffffffffff", &book_path)
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("not found in the peer book"), "{unknown}");
        assert!(
            unknown.contains("needs the full 64-hex fingerprint"),
            "{unknown}"
        );
    }

    #[test]
    fn resolve_fingerprint_arg_full_value_skips_the_book() {
        let dir = TempDir::new("fp-full");
        // No peers.book exists in this directory at all.
        let book_path = dir.path().join("peers.book");
        let fp = random_fp();
        assert_eq!(
            resolve_fingerprint_arg(&fp.to_hex(), &book_path).unwrap(),
            fp
        );
        assert!(resolve_fingerprint_arg(&fp.to_hex()[..63], &book_path).is_err());

        // Malformed shapes keep their explicit errors.
        for bad in ["nope", "", "abcd", &"a".repeat(65), &"z".repeat(64)] {
            assert!(
                resolve_fingerprint_arg(bad, &book_path).is_err(),
                "unexpectedly accepted {bad:?}"
            );
        }
        let short = resolve_fingerprint_arg("abcdef0", &book_path)
            .unwrap_err()
            .to_string();
        assert!(short.contains("8 to 63"), "{short}");
    }

    #[test]
    fn pair_remove_accepts_unique_prefix() {
        let dir = TempDir::new("pair-rm-prefix");
        let fp = random_fp();
        pair_add(dir.path(), &fp.to_hex(), Some("laptop")).unwrap();
        // A unique 8-char prefix removes the entry.
        let removed = pair_remove(dir.path(), &fp.to_hex()[..8]).unwrap();
        assert_eq!(removed, fp);
        // Afterwards the prefix matches nothing.
        let err = pair_remove(dir.path(), &fp.to_hex()[..8])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found in the peer book"), "{err}");
    }

    // -- multi-path send ------------------------------------------------------

    #[test]
    fn send_roots_keeps_primary_and_extra_order() {
        let dir = TempDir::new("send-roots");
        let file1 = dir.path().join("file1.bin");
        let file2 = dir.path().join("file2.bin");
        let sub = dir.path().join("dir1");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&file1, b"one").unwrap();
        std::fs::write(&file2, b"two").unwrap();

        let (primary, extras) = send_roots(&[file1.clone(), file2.clone(), sub.clone()]).unwrap();
        assert_eq!(primary, file1.canonicalize().unwrap());
        assert_eq!(
            extras,
            vec![file2.canonicalize().unwrap(), sub.canonicalize().unwrap()]
        );

        // Single path: primary only, no extras. No path: explicit error.
        let (primary, extras) = send_roots(&[file1]).unwrap();
        assert!(extras.is_empty());
        assert_eq!(
            primary,
            dir.path().join("file1.bin").canonicalize().unwrap()
        );
        assert!(send_roots(&[]).is_err());
        // A missing path fails naming the path.
        let err = send_roots(&[dir.path().join("missing.bin")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing.bin"), "{err}");
    }

    #[test]
    fn send_spec_extra_roots_follow_add_root_order() {
        let dir = TempDir::new("send-spec-order");
        let file1 = dir.path().join("file1.bin");
        let file2 = dir.path().join("file2.bin");
        let sub = dir.path().join("dir1");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&file1, b"1").unwrap();
        std::fs::write(&file2, b"2").unwrap();

        let (primary, extras) = send_roots(&[file1, file2, sub]).unwrap();
        let key = DeviceKeyPair::generate();
        let bundle = ak_identity::tls::make_self_signed(&key);
        let mut spec = SendSpec::new(
            primary.clone(),
            "127.0.0.1:7100".parse().unwrap(),
            key.fingerprint(),
            bundle,
        );
        for extra in extras {
            spec.add_root(extra);
        }
        assert_eq!(spec.source_root, primary);
        assert_eq!(spec.extra_roots.len(), 2);
        // add_root order must survive: file2 first, then dir1.
        assert!(spec.extra_roots[0].ends_with("file2.bin"));
        assert!(spec.extra_roots[1].ends_with("dir1"));
    }

    #[test]
    fn send_args_accept_multiple_paths_in_order() {
        let fp = "a".repeat(64);
        let cli = cli_of(&[
            "send",
            "file1.bin",
            "file2.bin",
            "dir1",
            "--to",
            "127.0.0.1:7100",
            "--fp",
            &fp,
        ]);
        let Command::Send { paths, .. } = cli.command else {
            panic!("expected send subcommand");
        };
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names, vec!["file1.bin", "file2.bin", "dir1"]);
    }

    #[test]
    fn duplicate_roots_error_is_recognized_for_a_friendly_hint() {
        let dup = EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "duplicate transfer root names: [photos, photos]",
        ));
        let io = duplicate_roots_error(&dup).expect("duplicate recognized");
        assert!(io.to_string().contains("photos"));

        // Other InvalidInput messages and other kinds are not misdetected.
        let bad_name = EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "root name not usable as a transfer directory name: \"com1\"",
        ));
        assert!(duplicate_roots_error(&bad_name).is_none());
        let not_found = EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "duplicate transfer root names: [x]",
        ));
        assert!(duplicate_roots_error(&not_found).is_none());
    }

    #[test]
    fn roots_line_joins_report_roots_and_hides_when_empty() {
        let report = |roots: Vec<&str>| Report {
            job_id: [0u8; 16],
            files: 3,
            bytes: 10,
            duration: Duration::from_millis(1),
            resumed_chunks: 0,
            peer_fingerprint: Some("ab".repeat(32)),
            roots: roots.into_iter().map(str::to_owned).collect(),
        };
        assert_eq!(roots_line(&report(Vec::new())), None);
        assert_eq!(
            roots_line(&report(vec!["file1.bin", "file2.bin", "dir1"])),
            Some("roots: file1.bin, file2.bin, dir1".to_owned())
        );
    }

    // -- probe same-egress notice ---------------------------------------------

    #[test]
    fn same_egress_notice_fires_exactly_once() {
        let mut notice = OnceNotice::new();
        assert_eq!(notice.take(), Some(SAME_EGRESS_NOTICE));
        assert_eq!(notice.take(), None);
        assert_eq!(notice.take(), None);
    }

    // -- pair book round trip ------------------------------------------------

    #[test]
    fn pair_add_list_remove_round_trip() {
        let dir = TempDir::new("pair-roundtrip");
        let fp = random_fp();

        let (parsed, added) = pair_add(dir.path(), &fp.to_hex(), Some("laptop")).unwrap();
        assert_eq!(parsed, fp);
        assert!(added);
        let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
        assert_eq!(book.iter().count(), 1);
        assert_eq!(book.get(&fp).map(|r| r.name.as_str()), Some("laptop"));

        // Re-adding updates the name instead of duplicating the entry.
        let (_, added) = pair_add(dir.path(), &fp.to_hex(), Some("deck")).unwrap();
        assert!(!added);
        let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
        assert_eq!(book.iter().count(), 1);
        assert_eq!(book.get(&fp).map(|r| r.name.as_str()), Some("deck"));

        let removed = pair_remove(dir.path(), &fp.to_hex()).unwrap();
        assert_eq!(removed, fp);
        assert_eq!(
            PeerBook::load(&peer_book_path(dir.path()))
                .unwrap()
                .iter()
                .count(),
            0
        );
        // Removing an unknown fingerprint is an error, not a silent no-op.
        assert!(pair_remove(dir.path(), &fp.to_hex()).is_err());
        // Malformed fingerprints never reach the book.
        assert!(pair_add(dir.path(), "nope", None).is_err());
    }

    #[test]
    fn sender_tofu_preserves_existing_names() {
        let dir = TempDir::new("tofu");
        let known = random_fp();
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &known, Some("alice"));
        book.save(&peer_book_path(dir.path())).unwrap();

        // Known peer: no change, no new record.
        assert!(!record_peer_after_send(dir.path(), &known).unwrap());
        let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
        assert_eq!(book.iter().count(), 1);
        assert_eq!(book.get(&known).map(|r| r.name.as_str()), Some("alice"));

        // Unknown peer: added with an empty name for later renaming.
        let stranger = random_fp();
        assert!(record_peer_after_send(dir.path(), &stranger).unwrap());
        let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
        assert_eq!(book.iter().count(), 2);
        assert_eq!(
            book.get(&stranger).map(|r| r.name.as_str()),
            Some("") // renamed via peer_display_name in the UI
        );
    }

    // -- jobs ----------------------------------------------------------------

    fn stored_record(job_byte: u8, state: &str) -> StoredTaskRecord {
        StoredTaskRecord {
            job_id: [job_byte; 16],
            direction: Direction::Send,
            root: PathBuf::from(r"D:\send dir\photos"),
            peer_fingerprint: Some(Fingerprint::from_bytes([9u8; 32])),
            chunk_size: 262_144,
            total_files: 3,
            total_bytes: 10_000_000,
            state: state.to_owned(),
            created_unix_ms: 1_730_000_000_000,
            updated_unix_ms: 1_730_000_500_000,
            files: vec![FileCheckpoint {
                file_index: 0,
                size: 4_000,
                verified_prefix_chunks: 2,
            }],
        }
    }

    #[test]
    fn jobs_list_parses_file_checkpoint_store_output() {
        let dir = TempDir::new("jobs-list");
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.save(&stored_record(7, "completed")).unwrap();
        store.save(&stored_record(8, "transferring")).unwrap();

        let listed = list_job_files(dir.path());
        assert_eq!(listed.len(), 2);
        let entry = listed
            .iter()
            .find(|(_, r)| r.job_hex == "07".repeat(16))
            .map(|(_, r)| r.clone())
            .expect("completed record listed");
        assert_eq!(entry.direction, "send");
        assert_eq!(entry.root, r"D:\send dir\photos");
        assert_eq!(entry.peer_short.as_deref(), Some("09090909"));
        assert_eq!(entry.total_files, 3);
        assert_eq!(entry.total_bytes, 10_000_000);
        assert_eq!(entry.state, "completed");
        assert_eq!(entry.updated_unix_ms, 1_730_000_500_000);

        // Corrupt and foreign files are skipped, never fatal.
        std::fs::write(
            dir.path().join("beef00112233445566778899aabbccdd.akjob"),
            b"junk",
        )
        .unwrap();
        std::fs::write(dir.path().join("keep.akpart"), b"parts").unwrap();
        assert_eq!(list_job_files(dir.path()).len(), 2);
    }

    #[test]
    fn parse_job_record_rejects_tampered_lines() {
        let dir = TempDir::new("jobs-tamper");
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.save(&stored_record(1, "transferring")).unwrap();
        let original =
            std::fs::read_to_string(dir.path().join(format!("{}.akjob", "01".repeat(16)))).unwrap();
        assert!(parse_job_record(&original).is_some());

        let variants = [
            original.replacen("AKJOB1", "AKJOB2", 1),
            original.replacen("state transferring", "state", 1),
            original.replacen("dir send", "dir sideways", 1),
            original.replacen("root ", "roots ", 1),
            format!("{original}extra junk\n"),
        ];
        for variant in &variants {
            assert!(
                parse_job_record(variant).is_none(),
                "tampered record parsed: {variant:?}"
            );
        }
    }

    #[test]
    fn jobs_clean_filters_by_scope_and_keeps_part_files() {
        let dir = TempDir::new("jobs-clean");
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.save(&stored_record(1, "completed")).unwrap();
        store.save(&stored_record(2, "transferring")).unwrap();
        // A corrupt record file still counts for --all but not --completed.
        std::fs::write(
            dir.path().join("beef00112233445566778899aabbccdd.akjob"),
            b"junk",
        )
        .unwrap();
        // Part files must survive every clean scope.
        std::fs::write(dir.path().join("0001.akpart"), b"partial data").unwrap();

        let (removed, kept) = clean_job_records(dir.path(), CleanScope::Completed).unwrap();
        assert_eq!((removed, kept), (1, 1));
        assert!(dir.path().join("0001.akpart").exists());
        assert!(dir
            .path()
            .join(format!("{}.akjob", "02".repeat(16)))
            .exists());
        assert!(!dir
            .path()
            .join(format!("{}.akjob", "01".repeat(16)))
            .exists());

        let (removed, kept) = clean_job_records(dir.path(), CleanScope::All).unwrap();
        assert_eq!(removed, 2); // remaining valid record + corrupt file
        assert_eq!(kept, 0);
        assert!(dir.path().join("0001.akpart").exists());
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .count(),
            1
        );

        // Cleaning a missing directory is a no-op, not an error.
        let (removed, kept) = clean_job_records(&dir.path().join("nope"), CleanScope::All).unwrap();
        assert_eq!((removed, kept), (0, 0));
    }

    // -- display helpers -----------------------------------------------------

    #[test]
    fn civil_dates_and_utc_formatting_match_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_666), (2026, 8, 1));
        assert_eq!(civil_from_days(20_759), (2026, 11, 2));
        assert_eq!(format_utc_ms(0), "1970-01-01 00:00:00 UTC");
        // 2026-11-02 00:00:00 UTC = 1793577600 s; plus 3661 s = 01:01:01.
        let day = 20_759 * 86_400;
        assert_eq!(
            format_utc_ms((day as u64 + 3661) * 1000),
            "2026-11-02 01:01:01 UTC"
        );
    }

    #[test]
    fn fingerprint_two_line_display_has_short_and_full() {
        let fp = random_fp();
        let text = fingerprint_lines("fingerprint", &fp);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format!("fingerprint: {}", fp.short_id()));
        assert_eq!(lines[1], format!("fingerprint (full): {}", fp.to_hex()));
        assert_eq!(peer_display_name(""), "(unnamed)");
        assert_eq!(peer_display_name("laptop"), "laptop");
    }

    #[test]
    fn json_escape_handles_quotes_controls_and_unicode() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(json_escape("line\nnext\ttab"), "line\\nnext\\ttab");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
        assert_eq!(json_escape("设备"), "设备");
    }

    #[test]
    fn discover_json_marks_paired_candidates() {
        let book_path_dir = TempDir::new("discover-json");
        let peer = random_fp();
        let mut book = PeerBook::default();
        upsert_peer(&mut book, &peer, Some("Alice \"Lap\"top"));
        book.save(&book_path_dir.path().join("peers.book")).unwrap();
        let book = PeerBook::load(&book_path_dir.path().join("peers.book")).unwrap();

        let candidates = vec![
            ak_discovery::PeerCandidate {
                addr: "192.168.1.9:7100".parse().expect("addr"),
                fingerprint: Some(peer),
                source: ak_discovery::CandidateSource::Mdns,
                device_name: None,
            },
            ak_discovery::PeerCandidate {
                addr: "192.168.1.10:7100".parse().expect("addr"),
                fingerprint: Some(random_fp()),
                source: ak_discovery::CandidateSource::Mdns,
                device_name: None,
            },
            ak_discovery::PeerCandidate {
                addr: "192.168.1.11:7100".parse().expect("addr"),
                fingerprint: None,
                source: ak_discovery::CandidateSource::Manual,
                device_name: None,
            },
        ];
        let json = discover_json(&candidates, &book);
        assert!(json.starts_with("[\n") && json.ends_with(']'));
        assert!(json.contains(&format!("\"fingerprint\": \"{}\"", peer.to_hex())));
        assert!(json.contains("\"paired\": true"));
        assert!(json.contains("\"name\": \"Alice \\\"Lap\\\"top\""));
        assert!(json.contains("\"paired\": false"));
        assert!(json.contains("\"fingerprint\": null"));
        assert!(json.contains("\"source\": \"mdns\""));
        assert!(json.contains("\"source\": \"manual\""));
        assert_eq!(json.matches('\n').count(), 4); // no trailing junk
    }

    // -- bench ---------------------------------------------------------------

    /// Parse a Cli from argv (tests may use expect/unwrap).
    fn cli_of(args: &[&str]) -> Cli {
        let mut argv = vec!["ak-cli"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).expect("argv must parse")
    }

    #[test]
    fn bench_args_defaults_and_overrides() {
        let cli = cli_of(&["bench"]);
        let Command::Bench {
            bytes,
            files,
            chunk_kb,
            multistream,
            once,
            json,
        } = cli.command
        else {
            panic!("expected bench subcommand");
        };
        assert_eq!((bytes, files, chunk_kb), (268_435_456, 8, 1024));
        assert!(!multistream && !once && !json);

        let cli = cli_of(&[
            "bench",
            "--bytes",
            "1000",
            "--files",
            "4",
            "--chunk-kb",
            "64",
            "--multistream",
            "--once",
            "--json",
        ]);
        let Command::Bench {
            bytes,
            files,
            chunk_kb,
            multistream,
            once,
            json,
        } = cli.command
        else {
            panic!("expected bench subcommand");
        };
        assert_eq!((bytes, files, chunk_kb), (1000, 4, 64));
        assert!(multistream && once && json);
    }

    #[test]
    fn bench_args_reject_out_of_range_values() {
        for bad in [
            vec!["bench", "--bytes", "0"],
            vec!["bench", "--files", "0"],
            vec!["bench", "--chunk-kb", "0"],
            vec!["bench", "--chunk-kb", "8193"], // above the engine decode budget
        ] {
            let mut argv = vec!["ak-cli"];
            argv.extend_from_slice(&bad);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "unexpectedly accepted {bad:?}"
            );
        }
    }

    #[test]
    fn doctor_args_defaults_overrides_and_ranges() {
        // Defaults: STUN skipped, 3s mDNS window, text output.
        let cli = cli_of(&["doctor"]);
        let Command::Doctor {
            stun,
            mdns_seconds,
            json,
        } = cli.command
        else {
            panic!("expected doctor subcommand");
        };
        assert_eq!(stun, None);
        assert_eq!(mdns_seconds, 3);
        assert!(!json);

        // Explicit server, window and JSON output.
        let cli = cli_of(&[
            "doctor",
            "--stun",
            "127.0.0.1:3478",
            "--mdns-seconds",
            "1",
            "--json",
        ]);
        let Command::Doctor {
            stun,
            mdns_seconds,
            json,
        } = cli.command
        else {
            panic!("expected doctor subcommand");
        };
        assert_eq!(stun, Some("127.0.0.1:3478".parse().expect("stun addr")));
        assert_eq!(mdns_seconds, 1);
        assert!(json);

        // The mDNS window is bounded on both ends; malformed servers reject.
        for bad in [
            vec!["doctor", "--mdns-seconds", "0"],
            vec!["doctor", "--mdns-seconds", "31"],
            vec!["doctor", "--stun", "not-an-addr"],
            vec!["doctor", "--stun", "127.0.0.1"],
        ] {
            let mut argv = vec!["ak-cli"];
            argv.extend_from_slice(&bad);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "unexpectedly accepted {bad:?}"
            );
        }
    }

    #[test]
    fn timeout_flag_maps_to_idle_timeout_on_both_verbs() {
        // send: default 30, explicit override 5.
        let fp = "a".repeat(64);
        let send_args = |extra: &[&str]| -> Vec<String> {
            let mut argv: Vec<String> = vec![
                "ak-cli".into(),
                "send".into(),
                "x.bin".into(),
                "--to".into(),
                "127.0.0.1:7100".into(),
                "--fp".into(),
                fp.clone(),
            ];
            argv.extend(extra.iter().map(|arg| String::from(*arg)));
            argv
        };
        let cli = Cli::try_parse_from(send_args(&[])).unwrap();
        let Command::Send { timeout, .. } = cli.command else {
            panic!("expected send subcommand");
        };
        assert_eq!(timeout, 30);
        assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(30));

        let cli = Cli::try_parse_from(send_args(&["--timeout", "5"])).unwrap();
        let Command::Send { timeout, .. } = cli.command else {
            panic!("expected send subcommand");
        };
        assert_eq!(timeout, 5);
        assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(5));

        // receive: same default and override.
        let recv_args = |extra: &[&str]| -> Vec<String> {
            let mut argv: Vec<String> = vec!["ak-cli".into(), "receive".into()];
            argv.extend(extra.iter().map(|arg| String::from(*arg)));
            argv
        };
        let cli = Cli::try_parse_from(recv_args(&[])).unwrap();
        let Command::Receive { timeout, .. } = cli.command else {
            panic!("expected receive subcommand");
        };
        assert_eq!(timeout, 30);
        let cli = Cli::try_parse_from(recv_args(&["--timeout", "17"])).unwrap();
        let Command::Receive { timeout, .. } = cli.command else {
            panic!("expected receive subcommand");
        };
        assert_eq!(timeout, 17);
        assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(17));

        // Zero is an idle budget of nothing; rejected for both verbs.
        assert!(Cli::try_parse_from(send_args(&["--timeout", "0"])).is_err());
        assert!(Cli::try_parse_from(recv_args(&["--timeout", "0"])).is_err());
    }

    #[test]
    fn xorshift_filler_is_deterministic_and_distinct() {
        // Same seed, same stream.
        let mut a = Xorshift64::new(42);
        let mut b = Xorshift64::new(42);
        let (mut buf_a, mut buf_b, mut buf_c) = ([0u8; 20], [0u8; 20], [0u8; 20]);
        a.fill(&mut buf_a);
        b.fill(&mut buf_b);
        Xorshift64::new(43).fill(&mut buf_c);
        assert_eq!(buf_a, buf_b);
        assert_ne!(buf_a, buf_c);
        assert!(buf_a.iter().any(|byte| *byte != 0), "degenerate stream");

        // Zero seed recovers instead of freezing.
        let mut zero = Xorshift64::new(0);
        let mut buf_zero = [0u8; 8];
        zero.fill(&mut buf_zero);
        assert!(buf_zero.iter().any(|byte| *byte != 0), "zero seed froze");

        // Word-stream continuity: one 16-byte fill equals two 8-byte fills.
        let mut once = Xorshift64::new(7);
        let mut twice = Xorshift64::new(7);
        let (mut whole, mut left, mut right) = ([0u8; 16], [0u8; 8], [0u8; 8]);
        once.fill(&mut whole);
        twice.fill(&mut left);
        twice.fill(&mut right);
        assert_eq!(&whole[..8], &left);
        assert_eq!(&whole[8..], &right);
    }

    #[test]
    fn bench_file_sizes_split_evenly_and_sum_to_total() {
        assert_eq!(bench_file_sizes(10, 3), vec![4, 3, 3]);
        assert_eq!(bench_file_sizes(8, 8), vec![1; 8]);
        assert_eq!(bench_file_sizes(3, 5), vec![1, 1, 1, 0, 0]);
        for (total, files) in [(1u64, 1u32), (1_000_001, 7), (268_435_456, 8)] {
            let sizes = bench_file_sizes(total, files);
            assert_eq!(sizes.len(), files as usize);
            assert_eq!(sizes.iter().sum::<u64>(), total);
        }
    }

    #[test]
    fn bench_json_has_machine_readable_shape() {
        let rounds = vec![
            BenchRound {
                mode: "single",
                duration: Duration::from_millis(2000),
                bytes: 100_000_000,
            },
            BenchRound {
                mode: "multistream",
                duration: Duration::from_millis(1000),
                bytes: 100_000_000,
            },
        ];
        let json = bench_json(100_000_000, 8, 1024, &rounds, Some(2.0));
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"bytes\": 100000000"));
        assert!(json.contains("\"files\": 8"));
        assert!(json.contains("\"chunk_kb\": 1024"));
        assert!(json.contains("\"mode\": \"single\""));
        assert!(json.contains("\"mode\": \"multistream\""));
        assert!(json.contains("\"duration_ms\": 2000.000"));
        assert!(json.contains("\"mb_per_s\": 50.000"));
        assert!(json.contains("\"gib_per_s\": 0.046566"));
        assert!(json.contains("\"ratio\": 2.0000"));
        assert!(json.contains(BENCH_DISCLAIMER));
        assert!(!json.contains("\"ratio\": null"));

        let one = vec![BenchRound {
            mode: "single",
            duration: Duration::from_millis(500),
            bytes: 1000,
        }];
        let json = bench_json(1000, 1, 1, &one, None);
        assert!(json.contains("\"ratio\": null"));
        assert!(!json.contains("inf") && !json.contains("NaN"));
    }

    #[test]
    fn clip_line_truncates_on_char_boundary_within_width() {
        assert_eq!(clip_line("short", 10), "short");
        let clipped = clip_line(&"x".repeat(100), 79);
        assert_eq!(clipped.chars().count(), 79);
        assert!(clipped.ends_with("..."));
        // Unicode path characters never split a char.
        let clipped = clip_line(&"设".repeat(100), 79);
        assert_eq!(clipped.chars().count(), 79);
        // Tiny widths degrade to a plain truncation, never wider than width.
        assert_eq!(clip_line("abc", 2), "ab");
    }
}
