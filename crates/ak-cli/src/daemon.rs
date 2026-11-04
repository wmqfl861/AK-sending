//! `ak-cli daemon`: resident receive mode.
//!
//! One process, three long-running concerns: the device identity (shared
//! with `receive` via [`crate::ReceiveLoopConfig`], so admission semantics
//! can never drift between the two verbs), an mDNS advertisement with the
//! fingerprint TXT record (the same record `discover` reads), and the
//! sequential receive loop (the `receive` kernel: bind, accept one job,
//! run it to completion, rebind). Unlike `receive`, a failed or timed-out
//! job does not end the daemon -- the failure is reported and the loop
//! keeps listening until Ctrl-C, which withdraws the mDNS announcement and
//! exits 0.
//!
//! `--json-lines` turns stdout into an NDJSON event stream
//! ([`DaemonEvent`]) for GUI/script consumers; human notes move to stderr
//! so stdout stays parseable line by line.
#![forbid(unsafe_code)]

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use ak_engine::{EngineError, Progress, Report};

use crate::{ProgressLine, ReceiveLoopConfig, FIREWALL_NOTE};

/// Flags of the `daemon` subcommand (clap parses them in `main.rs`; they
/// arrive here grouped so the command signature stays two-argument).
pub(crate) struct DaemonOptions {
    pub(crate) out: PathBuf,
    /// 0 = auto-assign a free port before advertising.
    pub(crate) port: u16,
    pub(crate) expect: Option<String>,
    pub(crate) conflict: String,
    pub(crate) timeout: u64,
    pub(crate) name: Option<String>,
    pub(crate) json_lines: bool,
}

/// One NDJSON event of the `daemon --json-lines` stream. Field names and
/// the `"type"` values are a script-facing contract: extend, never rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonEvent<'a> {
    /// Emitted once per daemon run after the first successful bind: the
    /// concrete listen address, this device's fingerprint and name, and
    /// whether the mDNS announcement went out.
    Listening {
        addr: &'a str,
        port: u16,
        fingerprint: &'a str,
        name: &'a str,
        mdns: bool,
    },
    /// A sender completed the handshake (its verified fingerprint in hex);
    /// the transfer itself starts now.
    JobStarted { peer: &'a str },
    /// One job finished and committed; the daemon keeps listening.
    JobDone {
        id: &'a str,
        files: u64,
        bytes: u64,
        duration_ms: u64,
        resumed: u64,
        /// Sender fingerprint (full hex) when the report carries it.
        peer: Option<&'a str>,
    },
    /// A recoverable failure (rejected peer, job error); the daemon keeps
    /// listening.
    Error { message: &'a str },
    /// Graceful shutdown (Ctrl-C); no further events follow.
    Stopped { reason: &'a str },
}

impl DaemonEvent<'_> {
    /// Render as exactly one NDJSON line (no trailing newline). All
    /// caller-controlled strings go through `json_escape`, so the output
    /// can never break line discipline.
    pub(crate) fn to_json_line(&self) -> String {
        match self {
            DaemonEvent::Listening {
                addr,
                port,
                fingerprint,
                name,
                mdns,
            } => format!(
                "{{\"type\":\"listening\",\"addr\":\"{}\",\"port\":{port},\
                 \"fingerprint\":\"{}\",\"name\":\"{}\",\"mdns\":{mdns}}}",
                crate::json_escape(addr),
                crate::json_escape(fingerprint),
                crate::json_escape(name)
            ),
            DaemonEvent::JobStarted { peer } => format!(
                "{{\"type\":\"job_started\",\"peer\":\"{}\"}}",
                crate::json_escape(peer)
            ),
            DaemonEvent::JobDone {
                id,
                files,
                bytes,
                duration_ms,
                resumed,
                peer,
            } => {
                let peer_json = match peer {
                    Some(hex) => format!("\"{}\"", crate::json_escape(hex)),
                    None => "null".to_owned(),
                };
                format!(
                    "{{\"type\":\"job_done\",\"id\":\"{id}\",\"files\":{files},\
                     \"bytes\":{bytes},\"duration_ms\":{duration_ms},\"resumed\":{resumed},\
                     \"peer\":{peer_json}}}"
                )
            }
            DaemonEvent::Error { message } => format!(
                "{{\"type\":\"error\",\"message\":\"{}\"}}",
                crate::json_escape(message)
            ),
            DaemonEvent::Stopped { reason } => format!(
                "{{\"type\":\"stopped\",\"reason\":\"{}\"}}",
                crate::json_escape(reason)
            ),
        }
    }
}

/// Print one line to stdout and flush, so pipe consumers see each NDJSON
/// event as soon as it happens.
fn emit_line(text: &str) {
    println!("{text}");
    let _ = std::io::stdout().flush();
}

/// Output routing shared by every daemon print: with `--json-lines`
/// stdout carries ONLY NDJSON events, human notes move to stderr; in text
/// mode everything is human copy on stdout.
struct DaemonOut {
    json_lines: bool,
}

impl DaemonOut {
    fn new(json_lines: bool) -> Self {
        DaemonOut { json_lines }
    }

    /// One NDJSON event; acts only with `--json-lines` (the text modes
    /// print their own richer copy at the same call sites).
    fn event(&self, event: &DaemonEvent<'_>) {
        if self.json_lines {
            emit_line(&event.to_json_line());
        }
    }

    /// A human line; acts only in text mode.
    fn text(&self, line: &str) {
        if !self.json_lines {
            println!("{line}");
        }
    }

    /// An informational note for both audiences: stdout in text mode,
    /// stderr with `--json-lines` (stdout must stay pure NDJSON there).
    fn note(&self, line: &str) {
        if self.json_lines {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

/// Resolve the daemon device name: `--name` or the local user name (what
/// `receive` reports). The flag boundary enforces the budgets the mDNS TXT
/// record and the wire hello already impose: non-empty after trim, no
/// control characters, at most 64 bytes -- failing here gives a clearer
/// error than a silent sanitize inside the stack.
pub(crate) fn daemon_device_name(name: Option<&str>) -> Result<String> {
    let raw = match name {
        Some(given) => given.to_owned(),
        None => crate::whoami(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("--name cannot be empty or only whitespace");
    }
    if trimmed.len() > 64 {
        bail!(
            "--name is {} bytes; the limit is 64 (mDNS TXT budget)",
            trimmed.len()
        );
    }
    if trimmed.chars().any(char::is_control) {
        bail!("--name cannot contain control characters");
    }
    Ok(trimmed.to_owned())
}

/// Pick a free UDP port with a throwaway `0.0.0.0:0` bind, so the port is
/// known BEFORE the mDNS announcement and stays fixed for the daemon's
/// whole life (each receive iteration rebinds it). A losing race against
/// another socket grabbing the same port surfaces as AddrInUse inside the
/// loop and rides the existing rebind retries.
fn pick_free_port() -> Result<u16> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").context("cannot probe a free UDP port")?;
    let port = probe
        .local_addr()
        .context("cannot read the probed UDP port")?
        .port();
    Ok(port)
}

/// 32-hex job id of a finished report (same rendering as `report_summary`).
fn job_id_hex(report: &Report) -> String {
    report.job_id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Progress adapter for the daemon: `Connected` marks the job start (and
/// becomes a `job_started` event in NDJSON mode); text mode reuses the
/// `receive` single-line renderer, NDJSON mode drops the rest so stdout
/// stays event-only.
fn daemon_progress_printer(
    json_lines: bool,
    connected_at: Arc<Mutex<Option<Instant>>>,
) -> Arc<dyn Fn(Progress) + Send + Sync> {
    let line = Arc::new(ProgressLine::new("recv"));
    Arc::new(move |progress: Progress| {
        if let Progress::Connected { peer } = &progress {
            *connected_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
            if json_lines {
                emit_line(&DaemonEvent::JobStarted { peer }.to_json_line());
                return;
            }
        }
        if !json_lines && std::env::var_os("AK_QUIET").is_none() {
            line.show(&progress);
        }
    })
}

/// `ak-cli daemon`: resident receive mode (mDNS advertise + receive loop +
/// event output). See the module docs; Ctrl-C withdraws the announcement
/// and exits 0.
pub(crate) async fn daemon_command(id: &Path, opts: DaemonOptions) -> Result<()> {
    let out = DaemonOut::new(opts.json_lines);
    let name = daemon_device_name(opts.name.as_deref())?;
    let port = if opts.port == 0 {
        pick_free_port()?
    } else {
        opts.port
    };
    let listen = SocketAddr::from(([0, 0, 0, 0], port));
    // The same identity/admission/checkpoint wiring `receive` resolves:
    // one config, two verbs.
    let config = ReceiveLoopConfig::resolve(
        id,
        opts.out.clone(),
        listen,
        opts.expect.as_deref(),
        &opts.conflict,
        false,
        opts.timeout,
        name.clone(),
    )?;
    let fp = config.fingerprint;
    for line in crate::expect_policy_lines(&config.policy) {
        out.note(&line);
    }
    out.note(FIREWALL_NOTE);

    // Advertise the same TXT record `discover` reads (fp + name), on the
    // fixed port. mDNS is a discovery hint, not authorization and not a
    // hard dependency: when multicast is unavailable the daemon degrades
    // to manual `--to` connections (REVIEW 4.3).
    let mut mdns: Option<ak_discovery::MdnsDiscovery> = None;
    let mut mdns_ok = false;
    match ak_discovery::MdnsDiscovery::new() {
        Ok(mut discovery) => match discovery.advertise(port, &fp, &name) {
            Ok(()) => {
                mdns_ok = true;
                mdns = Some(discovery);
                out.note(&format!(
                    "mDNS: advertised {} instance {} on port {port} (name {name})",
                    ak_discovery::SERVICE_TYPE,
                    fp.short_id()
                ));
            }
            Err(err) => out.note(&format!(
                "mDNS: advertise failed ({err}); senders can still connect manually with --to"
            )),
        },
        Err(err) => out.note(&format!(
            "mDNS: daemon unavailable ({err}); senders can still connect manually with --to"
        )),
    }

    // The receive kernel's rebind retry budget (the previous iteration's
    // QUIC endpoint releases its UDP socket asynchronously after drop).
    const MAX_REBIND_RETRIES: u32 = 50;
    let mut rebind_retries = 0u32;
    let mut first = true;
    loop {
        let spec = config.iteration_spec();
        let connected_at = Arc::new(Mutex::new(None::<Instant>));
        let printer = daemon_progress_printer(out.json_lines, connected_at.clone());
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await },
            );
        if first {
            let addr = addr_rx.await.context("receiver failed to bind")?;
            out.event(&DaemonEvent::Listening {
                addr: &addr.to_string(),
                port: addr.port(),
                fingerprint: &fp.to_hex(),
                name: &name,
                mdns: mdns_ok,
            });
            out.text(&format!(
                "daemon listening on {addr} into {} (name {name})",
                config.out.display()
            ));
            out.text(&crate::fingerprint_lines("fingerprint", &fp));
            out.text(&format!("pairing code: {}", ak_identity::pairing_code(&fp)));
            out.text("waiting for sender (Ctrl-C stops)...");
            first = false;
        } else {
            // Bind errors surface through the task result below.
            let _ = addr_rx.await;
        }
        let outcome = tokio::select! {
            joined = receiver => joined,
            _ = tokio::signal::ctrl_c() => {
                if let Some(discovery) = mdns.as_mut() {
                    discovery.stop_advertise();
                }
                out.event(&DaemonEvent::Stopped { reason: "ctrl-c" });
                out.text("ctrl-c: daemon stopped (mDNS announcement withdrawn; partial files stay resumable)");
                return Ok(());
            }
        };
        match outcome {
            Ok(Ok(report)) => {
                rebind_retries = 0;
                if out.json_lines {
                    // Duration spans job_started (handshake) to completion;
                    // if no Connected event was seen, fall back to the
                    // engine's own report duration.
                    let started = connected_at
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take();
                    let duration_ms = started
                        .map(|at| at.elapsed().as_millis())
                        .unwrap_or_else(|| report.duration.as_millis());
                    out.event(&DaemonEvent::JobDone {
                        id: &job_id_hex(&report),
                        files: report.files,
                        bytes: report.bytes,
                        duration_ms: u64::try_from(duration_ms).unwrap_or(u64::MAX),
                        resumed: report.resumed_chunks,
                        peer: report.peer_fingerprint.as_deref(),
                    });
                } else {
                    crate::report_summary(&report);
                }
                out.text("waiting for next sender (Ctrl-C stops)...");
            }
            Ok(Err(EngineError::Connect(ak_net::ConnectError::Unauthorized))) => {
                out.event(&DaemonEvent::Error {
                    message: "rejected a peer outside the accept policy",
                });
                out.text("rejected a peer outside the accept policy; still waiting (Ctrl-C stops)");
            }
            Ok(Err(EngineError::Io(err)))
                if err.kind() == std::io::ErrorKind::AddrInUse
                    && rebind_retries < MAX_REBIND_RETRIES =>
            {
                rebind_retries += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            // A failed job does not end the daemon (unlike `receive`):
            // report it and keep listening. The brief pause prevents a
            // hot error loop on a permanently broken environment.
            Ok(Err(error)) => {
                out.event(&DaemonEvent::Error {
                    message: &error.to_string(),
                });
                out.text(&format!(
                    "job failed: {error}; still listening (Ctrl-C stops)"
                ));
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(join) => bail!("receiver task failed: {join}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::{DeviceKeyPair, PeerBook, PeerRecord};
    use clap::Parser as _;

    /// Unique temp directory (tests may use expect/unwrap).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ak-cli-daemon-tests-{tag}-{}-{}",
                std::process::id(),
                crate::now_unix_ms()
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

    /// Parse CLI args and pull out the daemon variant's fields.
    fn parse_daemon(
        args: &[&str],
    ) -> (
        PathBuf,
        u16,
        Option<String>,
        String,
        u64,
        Option<String>,
        bool,
    ) {
        let cli = crate::Cli::try_parse_from(std::iter::once("ak-cli").chain(args.iter().copied()))
            .expect("cli parses");
        match cli.command {
            crate::Command::Daemon {
                out,
                port,
                expect,
                conflict,
                timeout,
                name,
                json_lines,
            } => (out, port, expect, conflict, timeout, name, json_lines),
            other => panic!("expected daemon, got {other:?}"),
        }
    }

    #[test]
    fn daemon_flags_parse_with_defaults() {
        let (out, port, expect, conflict, timeout, name, json_lines) = parse_daemon(&["daemon"]);
        assert_eq!(out, PathBuf::from("."));
        assert_eq!(port, 0);
        assert_eq!(expect, None);
        assert_eq!(conflict, "error");
        assert_eq!(timeout, 30);
        assert_eq!(name, None);
        assert!(!json_lines);
    }

    #[test]
    fn daemon_flags_parse_every_override() {
        let (out, port, expect, conflict, timeout, name, json_lines) = parse_daemon(&[
            "daemon",
            "--out",
            "D:/inbox",
            "--port",
            "7555",
            "--expect",
            "any",
            "--conflict",
            "rename",
            "--timeout",
            "9",
            "--name",
            "Alice's laptop",
            "--json-lines",
        ]);
        assert_eq!(out, PathBuf::from("D:/inbox"));
        assert_eq!(port, 7555);
        assert_eq!(expect.as_deref(), Some("any"));
        assert_eq!(conflict, "rename");
        assert_eq!(timeout, 9);
        assert_eq!(name.as_deref(), Some("Alice's laptop"));
        assert!(json_lines);
    }

    #[test]
    fn daemon_events_render_exact_single_line_ndjson() {
        let fp_hex = "ab".repeat(32);
        let events = [
            DaemonEvent::Listening {
                addr: "0.0.0.0:53112",
                port: 53112,
                fingerprint: &fp_hex,
                name: "Alice",
                mdns: true,
            }
            .to_json_line(),
            DaemonEvent::Listening {
                addr: "[::]:7100",
                port: 7100,
                fingerprint: &fp_hex,
                name: "Bob",
                mdns: false,
            }
            .to_json_line(),
            DaemonEvent::JobStarted { peer: &fp_hex }.to_json_line(),
            DaemonEvent::JobDone {
                id: &"09af".repeat(8),
                files: 3,
                bytes: 4096,
                duration_ms: 1523,
                resumed: 7,
                peer: Some(&fp_hex),
            }
            .to_json_line(),
            DaemonEvent::JobDone {
                id: &"09af".repeat(8),
                files: 1,
                bytes: 0,
                duration_ms: 0,
                resumed: 0,
                peer: None,
            }
            .to_json_line(),
            DaemonEvent::Error {
                message: "idle timeout",
            }
            .to_json_line(),
            DaemonEvent::Stopped { reason: "ctrl-c" }.to_json_line(),
        ];
        assert_eq!(
            events[0],
            format!(
                "{{\"type\":\"listening\",\"addr\":\"0.0.0.0:53112\",\"port\":53112,\
                 \"fingerprint\":\"{fp_hex}\",\"name\":\"Alice\",\"mdns\":true}}"
            )
        );
        assert_eq!(
            events[1],
            format!(
                "{{\"type\":\"listening\",\"addr\":\"[::]:7100\",\"port\":7100,\
                 \"fingerprint\":\"{fp_hex}\",\"name\":\"Bob\",\"mdns\":false}}"
            )
        );
        assert_eq!(
            events[2],
            format!("{{\"type\":\"job_started\",\"peer\":\"{fp_hex}\"}}")
        );
        assert_eq!(
            events[3],
            format!(
                "{{\"type\":\"job_done\",\"id\":\"{}\",\"files\":3,\"bytes\":4096,\
                 \"duration_ms\":1523,\"resumed\":7,\"peer\":\"{fp_hex}\"}}",
                "09af".repeat(8)
            )
        );
        assert_eq!(
            events[4],
            format!(
                "{{\"type\":\"job_done\",\"id\":\"{}\",\"files\":1,\"bytes\":0,\
                 \"duration_ms\":0,\"resumed\":0,\"peer\":null}}",
                "09af".repeat(8)
            )
        );
        assert_eq!(
            events[5],
            "{\"type\":\"error\",\"message\":\"idle timeout\"}"
        );
        assert_eq!(events[6], "{\"type\":\"stopped\",\"reason\":\"ctrl-c\"}");
        for line in events {
            assert!(!line.contains('\n'), "NDJSON line broke: {line}");
            assert!(!line.contains('\r'), "NDJSON line broke: {line}");
        }
    }

    #[test]
    fn daemon_event_strings_are_json_escaped() {
        let line = DaemonEvent::Error {
            message: "boom \"quoted\"\nsecond line\ttab\u{1}",
        }
        .to_json_line();
        // One physical line with every special byte escaped.
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains("\\\"quoted\\\""), "quotes escaped in {line}");
        assert!(line.contains("\\n"), "newline escaped in {line}");
        assert!(line.contains("\\t"), "tab escaped in {line}");
        assert!(line.contains("\\u0001"), "control escaped in {line}");
        let listening = DaemonEvent::Listening {
            addr: "0.0.0.0:1",
            port: 1,
            fingerprint: "00",
            name: "A\\lice\"x",
            mdns: false,
        }
        .to_json_line();
        assert!(
            listening.contains("\"name\":\"A\\\\lice\\\"x\""),
            "in {listening}"
        );
    }

    #[test]
    fn daemon_device_name_validates_and_defaults_to_whoami() {
        assert_eq!(daemon_device_name(None).unwrap(), crate::whoami());
        assert_eq!(
            daemon_device_name(Some("  Alice's laptop ")).unwrap(),
            "Alice's laptop"
        );
        // Exactly 64 bytes passes, one more fails (mDNS TXT budget).
        assert_eq!(
            daemon_device_name(Some(&"n".repeat(64))).unwrap(),
            "n".repeat(64)
        );
        assert!(daemon_device_name(Some(&"n".repeat(65))).is_err());
        assert!(daemon_device_name(Some("")).is_err());
        assert!(daemon_device_name(Some("   ")).is_err());
        assert!(daemon_device_name(Some("Bad\u{7}Name")).is_err());
        assert!(daemon_device_name(Some("row1\nrow2")).is_err());
        // Multi-byte names count bytes, not chars, and pass under the cap.
        assert_eq!(daemon_device_name(Some("手机-1234")).unwrap(), "手机-1234");
    }

    #[test]
    fn daemon_and_receive_resolve_identical_identity_and_admission() {
        let dir = TempDir::new("shared-admission");
        let peer = DeviceKeyPair::generate().fingerprint();
        let book_path = dir.path().join("peers.book");
        let mut book = PeerBook::default();
        book.upsert(PeerRecord {
            fingerprint: peer,
            name: "sender".to_owned(),
            paired_unix_ms: 1,
        });
        book.save(&book_path).expect("save book");

        let listen: SocketAddr = "0.0.0.0:7100".parse().unwrap();
        // `receive` resolves with the local user name, `daemon` with --name:
        // everything else comes from the same flags.
        let recv = ReceiveLoopConfig::resolve(
            dir.path(),
            PathBuf::from("out"),
            listen,
            None,
            "error",
            false,
            30,
            crate::whoami(),
        )
        .expect("receive config");
        let daemon = ReceiveLoopConfig::resolve(
            dir.path(),
            PathBuf::from("out"),
            listen,
            None,
            "error",
            false,
            30,
            "Alice".to_owned(),
        )
        .expect("daemon config");

        // One identity directory means one identity for both verbs.
        assert_eq!(recv.fingerprint, daemon.fingerprint);
        assert_eq!(recv.checkpoints, daemon.checkpoints);
        assert_eq!(recv.listen, daemon.listen);
        assert_eq!(recv.idle, daemon.idle);
        assert_eq!(recv.conflict, daemon.conflict);

        let recv_spec = recv.iteration_spec();
        let daemon_spec = daemon.iteration_spec();
        assert_eq!(recv_spec.bind, daemon_spec.bind);
        assert_eq!(recv_spec.conflict, daemon_spec.conflict);
        assert_eq!(recv_spec.idle_timeout, daemon_spec.idle_timeout);
        assert_eq!(recv_spec.checkpoint_dir, daemon_spec.checkpoint_dir);
        assert_eq!(
            recv_spec.policy.allow_internet,
            daemon_spec.policy.allow_internet
        );
        assert_eq!(recv_spec.device_name, crate::whoami());
        assert_eq!(daemon_spec.device_name, "Alice");

        // Admission behaves identically: the paired peer passes on both
        // specs, a stranger fails on both (book policy re-reads the file).
        assert!((recv_spec.authorize)(&peer));
        assert!((daemon_spec.authorize)(&peer));
        let stranger = DeviceKeyPair::generate().fingerprint();
        assert!(!(recv_spec.authorize)(&stranger));
        assert!(!(daemon_spec.authorize)(&stranger));
    }

    #[test]
    fn daemon_rejects_bad_conflict_and_expect_like_receive() {
        let dir = TempDir::new("flag-parity");
        let listen: SocketAddr = "0.0.0.0:0".parse().unwrap();
        // resolve() is the shared path, so both verbs reject the same junk.
        // --expect any clears the (empty) book check so the conflict error
        // is the one that surfaces.
        let err = match ReceiveLoopConfig::resolve(
            dir.path(),
            PathBuf::from("out"),
            listen,
            Some("any"),
            "overwrite",
            false,
            30,
            "Alice".to_owned(),
        ) {
            Ok(_) => panic!("bad conflict policy must fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown conflict policy"));
        let err = match ReceiveLoopConfig::resolve(
            dir.path(),
            PathBuf::from("out"),
            listen,
            Some("not-hex"),
            "error",
            false,
            30,
            "Alice".to_owned(),
        ) {
            Ok(_) => panic!("bad --expect must fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("invalid --expect"),
            "error {err} does not name --expect"
        );
    }
}
