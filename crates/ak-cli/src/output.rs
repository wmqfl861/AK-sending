//! Display helpers: dates, sizes, identities, progress lines and the job
//! summary.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// Small display helpers
// ---------------------------------------------------------------------------

/// Whole days since epoch -> civil date (Howard Hinnant's algorithm).
pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
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

pub(super) fn format_utc_ms(unix_ms: u64) -> String {
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
pub(crate) fn fingerprint_lines(label: &str, fingerprint: &Fingerprint) -> String {
    format!(
        "{label}: {}\n{label} (full): {}",
        fingerprint.short_id(),
        fingerprint.to_hex()
    )
}

pub(super) fn peer_display_name(name: &str) -> &str {
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

pub(super) fn source_name(source: ak_discovery::CandidateSource) -> &'static str {
    match source {
        ak_discovery::CandidateSource::Manual => "manual",
        ak_discovery::CandidateSource::Signaling => "signaling",
        ak_discovery::CandidateSource::Mdns => "mdns",
    }
}

/// The fixed text printed for the probe SameEgress hint.
// G5 / E1: the probe (legacy plaintext signal client) is feature-gated;
// its one-shot notice helper is gated with it.
#[cfg(feature = "legacy-plaintext-signal")]
pub(super) const SAME_EGRESS_NOTICE: &str = "same egress detected: prefer LAN candidates first";

/// One-shot guard for repeated events that should be announced exactly once:
/// [`OnceNotice::take`] returns the notice text the first time and None for
/// every later hit.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Default)]
pub(super) struct OnceNotice {
    shown: bool,
}

#[cfg(feature = "legacy-plaintext-signal")]
impl OnceNotice {
    pub(super) fn new() -> Self {
        OnceNotice { shown: false }
    }

    pub(super) fn take(&mut self) -> Option<&'static str> {
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

pub(super) fn conflict_policy(text: &str) -> Result<ConflictPolicy> {
    match text {
        "error" => Ok(ConflictPolicy::Fail),
        "rename" => Ok(ConflictPolicy::Rename),
        other => bail!("unknown conflict policy {other:?}; use error|rename"),
    }
}

/// Map the `--timeout` flag (seconds) onto the engine idle timeout. The flag
/// is an idle budget, not a total run time; clap already rejects 0.
pub(super) fn idle_timeout_from_secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

pub(super) fn checkpoint_dir(id: &Path) -> PathBuf {
    id.join("checkpoints")
}

pub(crate) fn load_identity(id: &Path) -> Result<(DeviceKeyPair, TlsBundle)> {
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

pub(super) fn print_identity(key: &DeviceKeyPair) {
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
pub(super) fn clip_line(text: &str, width: usize) -> String {
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
pub(crate) struct ProgressLine {
    direction: &'static str,
    open: std::sync::atomic::AtomicBool,
}

impl ProgressLine {
    pub(crate) fn new(direction: &'static str) -> Self {
        ProgressLine {
            direction,
            open: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn show(&self, progress: &Progress) {
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

pub(crate) fn progress_printer(direction: &'static str) -> Arc<dyn Fn(Progress) + Send + Sync> {
    let line = Arc::new(ProgressLine::new(direction));
    Arc::new(move |progress: Progress| {
        if std::env::var_os("AK_QUIET").is_none() {
            line.show(&progress);
        }
    })
}

/// Progress adapter for concurrent receive: job-prefixed whole-line events
/// only (per-job single-line refresh would garble when jobs interleave, so
/// byte-level ticks are skipped in this mode).
pub(super) fn progress_printer_multi() -> Arc<dyn Fn(u64, Progress) + Send + Sync> {
    Arc::new(move |job: u64, progress: Progress| {
        if std::env::var_os("AK_QUIET").is_some() {
            return;
        }
        if matches!(progress, Progress::BytesAcked { .. }) {
            return;
        }
        let direction = format!("recv#{job}");
        print_progress(&direction, &progress);
    })
}

fn short_of(peer: &Option<String>) -> String {
    peer.as_deref()
        .map(|hex| hex.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "?".to_owned())
}

/// One-line root listing for the summary: `roots: a, b, c` (primary first,
/// then extras in add order). None when the report carries no roots.
pub(super) fn roots_line(report: &Report) -> Option<String> {
    if report.roots.is_empty() {
        None
    } else {
        Some(format!("roots: {}", report.roots.join(", ")))
    }
}

pub(crate) fn report_summary(report: &Report) {
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
