//! Command-line prototype for AK-sending (single binary, many verbs).
//!
//! Verbs: `keygen`, `fingerprint`, `pair`, `receive`, `send`, `discover`,
//! `jobs`, `signal-server`, `probe`. The binary never talks to any
//! file-carrying server; servers started here are coordination-only (ADR-001).
//!
//! Pairing model: the local peer book (`peers.book` in the id directory)
//! holds fingerprint/name pairs. `receive` defaults to accepting only paired
//! fingerprints; a successful `send` records the receiver trust-on-first-use.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
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
    after_help = "Typical flow:\n  1. ak-cli pair code                 (on the receiving device)\n  2. ak-cli receive --port 7100       (accepts paired peers only)\n  3. ak-cli send <path> --to <ip:7100> --fp <64-hex fingerprint>\n\nAll transfers are direct device-to-device; no server ever carries file data."
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
        /// 64-hex fingerprint = pin that one peer;
        /// 6-digit pairing code = accept the peer whose code matches.
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Handle exactly one transfer, then exit (default: loop until Ctrl-C).
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    /// Send one file or one directory to a peer. On success the peer's
    /// fingerprint is recorded in peers.book (trust on first use).
    Send {
        /// Files or one directory to send.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Peer address ip:port.
        #[arg(long)]
        to: SocketAddr,
        /// Peer fingerprint (64 hex characters), pinned for this transfer.
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
        /// Peer fingerprint, 64 hex characters.
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

// ---------------------------------------------------------------------------
// Peer book helpers
// ---------------------------------------------------------------------------

fn peer_book_path(id: &Path) -> PathBuf {
    id.join("peers.book")
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Load peers.book; a missing file means "no peers yet", corruption is fatal.
fn load_peer_book(path: &Path) -> Result<PeerBook> {
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

/// `pair remove`: drop the fingerprint; error when it was never paired.
fn pair_remove(id: &Path, fingerprint_text: &str) -> Result<Fingerprint> {
    let fingerprint = parse_fingerprint(fingerprint_text)?;
    let path = peer_book_path(id);
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
    fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(ExpectPolicy::Any);
        }
        if trimmed.len() == 64 {
            let pinned = Fingerprint::from_hex(trimmed)
                .context("invalid --expect fingerprint (expected 64 hex characters)")?;
            return Ok(ExpectPolicy::Pinned(pinned));
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
        bail!(
            "invalid --expect {trimmed:?}: use 'any', a 64-hex fingerprint, \
             or a 6-digit pairing code"
        )
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
        Some(text) => ExpectPolicy::parse(text),
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

fn human_bytes(bytes: u64) -> String {
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
fn json_escape(text: &str) -> String {
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
        Progress::BytesAcked { cumulative_bytes, .. } => println!(
            "[{direction}] progress: {} acknowledged",
            human_bytes(*cumulative_bytes)
        ),
        Progress::FinalAck { verified } => println!("[{direction}] final ack verified={verified}"),
        Progress::Done => println!("[{direction}] done"),
        Progress::ManifestReceived => println!("[{direction}] manifest received"),
        _ => {}
    }
}

fn progress_printer(direction: &'static str) -> Arc<dyn Fn(Progress) + Send + Sync> {
    Arc::new(move |progress: Progress| {
        if std::env::var_os("AK_QUIET").is_none() {
            print_progress(direction, &progress);
        }
    })
}

fn short_of(peer: &Option<String>) -> String {
    peer.as_deref()
        .map(|hex| hex.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "?".to_owned())
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
                println!("updated {shown} ({}) -- entry already existed", fp.short_id());
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
                bail!("jobs clean needs --all or --completed (part files .akpart are never touched)");
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

async fn receive_command(
    id: &Path,
    out: PathBuf,
    port: u16,
    expect: Option<String>,
    conflict: String,
    once: bool,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let fp = key.fingerprint();
    let policy = resolve_expect(expect.as_deref(), &peer_book_path(id))?;
    let conflict = conflict_policy(&conflict)?;
    let checkpoints = checkpoint_dir(id);
    let device_name = whoami();

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
        spec.authorize = policy.authorize();
        let printer = progress_printer("recv");
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(async move {
            ak_engine::receive_with_addr(spec, &*printer, addr_tx).await
        });
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
            Ok(Err(EngineError::Connect(ak_net::ConnectError::Unauthorized)))
                if !once =>
            {
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

async fn send_command(
    id: &Path,
    paths: Vec<PathBuf>,
    to: SocketAddr,
    fp_text: String,
    port: u16,
    conflict: String,
    multistream: bool,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let pinned = parse_fingerprint(&fp_text)?;
    let Some(first) = paths.first() else {
        bail!("send needs exactly one file or one directory per run in this prototype");
    };
    let root = first
        .canonicalize()
        .with_context(|| first.display().to_string())?;
    println!(
        "sending to peer {} (its pairing code must be {})",
        pinned.short_id(),
        ak_identity::pairing_code(&pinned)
    );
    let mut spec = SendSpec::new(root, to, pinned, bundle);
    spec.bind = SocketAddr::from(([0, 0, 0, 0], port));
    spec.conflict = conflict_policy(&conflict)?;
    spec.checkpoint_dir = checkpoint_dir(id);
    spec.device_name = whoami();
    spec.multistream = multistream;
    let _ = key;
    let printer = progress_printer("send");
    let report = ak_engine::send(spec, &*printer).await?;
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
        } => receive_command(&id, out, port, expect, conflict, once).await,
        Command::Send {
            paths,
            to,
            fp,
            port,
            conflict,
            multistream,
        } => send_command(&id, paths, to, fp, port, conflict, multistream).await,
        Command::Discover { seconds, json } => discover_command(&id, seconds, json).await,
        Command::Jobs { cmd } => jobs_command(&id, cmd),
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
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn whoami() -> String {
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
        let fp = random_fp();
        assert!(matches!(
            ExpectPolicy::parse("any").unwrap(),
            ExpectPolicy::Any
        ));
        assert!(matches!(
            ExpectPolicy::parse("ANY").unwrap(),
            ExpectPolicy::Any
        ));
        assert!(matches!(
            ExpectPolicy::parse(&fp.to_hex()).unwrap(),
            ExpectPolicy::Pinned(parsed) if parsed == fp
        ));
        assert!(matches!(
            ExpectPolicy::parse("123-456").unwrap(),
            ExpectPolicy::PairingCode(code) if code == "123-456"
        ));
        assert!(matches!(
            ExpectPolicy::parse(" 123 456 ").unwrap(),
            ExpectPolicy::PairingCode(_)
        ));
        // Separator placement is lenient (ak-identity filters separators);
        // "123-45-6" means the same code as "123456".
        assert!(matches!(
            ExpectPolicy::parse("123-45-6").unwrap(),
            ExpectPolicy::PairingCode(code) if code == "123-45-6"
        ));
        for junk in ["junk", "", "12345", "1234567", "12a456", &"x".repeat(63)] {
            assert!(
                ExpectPolicy::parse(junk).is_err(),
                "unexpectedly accepted {junk:?}"
            );
        }
    }

    #[test]
    fn expect_policy_allows_only_matching_peers() {
        let pinned = random_fp();
        let stranger = random_fp();
        let policy = ExpectPolicy::parse(&pinned.to_hex()).unwrap();
        assert!(policy.allows(&pinned));
        assert!(!policy.allows(&stranger));

        let any = ExpectPolicy::parse("any").unwrap();
        assert!(any.allows(&pinned) && any.allows(&stranger));

        let code = ak_identity::pairing_code(&pinned);
        let policy = ExpectPolicy::parse(&code).unwrap();
        assert!(policy.allows(&pinned));
        assert!(!policy.allows(&stranger));
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
        std::fs::write(dir.path().join("beef00112233445566778899aabbccdd.akjob"), b"junk").unwrap();
        std::fs::write(dir.path().join("keep.akpart"), b"parts").unwrap();
        assert_eq!(list_job_files(dir.path()).len(), 2);
    }

    #[test]
    fn parse_job_record_rejects_tampered_lines() {
        let dir = TempDir::new("jobs-tamper");
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.save(&stored_record(1, "transferring")).unwrap();
        let original = std::fs::read_to_string(
            dir.path().join(format!("{}.akjob", "01".repeat(16))),
        )
        .unwrap();
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
        std::fs::write(dir.path().join("beef00112233445566778899aabbccdd.akjob"), b"junk").unwrap();
        // Part files must survive every clean scope.
        std::fs::write(dir.path().join("0001.akpart"), b"partial data").unwrap();

        let (removed, kept) = clean_job_records(dir.path(), CleanScope::Completed).unwrap();
        assert_eq!((removed, kept), (1, 1));
        assert!(dir.path().join("0001.akpart").exists());
        assert!(dir.path().join(format!("{}.akjob", "02".repeat(16))).exists());
        assert!(!dir.path().join(format!("{}.akjob", "01".repeat(16))).exists());

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
        let (removed, kept) =
            clean_job_records(&dir.path().join("nope"), CleanScope::All).unwrap();
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
            },
            ak_discovery::PeerCandidate {
                addr: "192.168.1.10:7100".parse().expect("addr"),
                fingerprint: Some(random_fp()),
                source: ak_discovery::CandidateSource::Mdns,
            },
            ak_discovery::PeerCandidate {
                addr: "192.168.1.11:7100".parse().expect("addr"),
                fingerprint: None,
                source: ak_discovery::CandidateSource::Manual,
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
}
