//! `ak-cli daemon`: resident receive mode.
//!
//! One process, three long-running concerns: the device identity (shared
//! with `receive` via [`crate::ReceiveLoopConfig`], so admission semantics
//! can never drift between the two verbs), an mDNS advertisement with the
//! fingerprint TXT record (the same record `discover` reads), and the
//! receive loop. Unlike `receive`, a failed or timed-out job does not end
//! the daemon -- the failure is reported and the loop keeps listening
//! until Ctrl-C, which withdraws the mDNS announcement and exits 0.
//!
//! Two receive shapes (wave 27):
//!
//! * **wired** (any of `--same-code`/`--cross-code`/`--invite-code`/
//!   `--registry-fp`): the session-runtime path. The endpoint binds ONCE
//!   and accepts connections concurrently; every accepted connection
//!   becomes a handler task that runs the scheduler handshake split. A
//!   scheduler-capable sender is validated against the local Ready
//!   pairing relations ([`ak_session::runtime::AdmissionValidator`]) and enters the scheduler
//!   under the REAL code group of that relation (`PairingPlan::Ready`),
//!   so `same_code_mode`/`cross_code_mode` govern real groups. A legacy
//!   peer (no `CAP_SCHEDULER`, e.g. the previous `send --fp`) keeps the
//!   frozen single-job protocol on the same channel under the anonymous
//!   legacy group (`PairingPlan::Local`), so its concurrency stays policy
//!   visible too.
//! * **direct** (no scheduler flags): the engine loop exactly as it always
//!   ran -- bind, accept one job at a time, rebind (compatibility gate).
//!
//! `--json-lines` turns stdout into an NDJSON event stream
//! ([`DaemonEvent`]) for GUI/script consumers; human notes move to stderr
//! so stdout stays parseable line by line.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use ak_engine::{EngineError, Progress, ReceiveSpec, Report};
use ak_session::runtime::RealOutcome;

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
    /// Wave 21: redeem this invite code at startup, then keep receiving.
    pub(crate) invite_code: Option<String>,
    /// Wave 21: run the owner-side pairing answer service.
    pub(crate) pairing: bool,
    /// Wave 21: scheduler policy pushed via `SessionHandle::set_policy`.
    pub(crate) same_code: Option<String>,
    pub(crate) cross_code: Option<String>,
    /// Registry endpoint (with `registry_fp`); otherwise the saved pin.
    pub(crate) server: Option<std::net::SocketAddr>,
    pub(crate) registry_fp: Option<String>,
}

/// One NDJSON event of the `daemon --json-lines` stream. Field names and
/// the `"type"` values are a script-facing contract: extend, never rename.
/// Contract scope (CR-P3-08): this stream is the CLI's stdout format, NOT
/// the FFI ABI — `docs/contracts/ffi-v2.md` freezes the FFI event polling
/// model (§5) and the daemon FLAG surface (§6), not these lines, so the
/// additive extensions below land directly in code. Extension rule: new
/// fields are APPENDED after the existing ones so prefix parsers
/// (`scripts/cross_device_test.sh` greps the `job_done` field prefix up to
/// `duration_ms`) keep working byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonEvent<'a> {
    /// Startup progress (run-43312035496 diagnosis finding 2): before the
    /// first bind the daemon's stdout carries literally nothing and every
    /// earlier failure exits via stderr — an extend-only progress event
    /// per phase makes startup stalls/early-exits visible in json-lines
    /// mode without touching any frozen shape.
    Starting { phase: &'a str },
    /// Emitted once per daemon run after the first successful bind: the
    /// concrete listen address, this device's fingerprint and name, whether
    /// the mDNS announcement went out, and the wave-25 `candidates` list
    /// (every dialable local interface address carrying the listen port --
    /// what a sender should feed `--to` on a multi-NIC host).
    Listening {
        addr: &'a str,
        port: u16,
        fingerprint: &'a str,
        name: &'a str,
        mdns: bool,
        candidates: &'a [String],
    },
    /// A sender completed the handshake (its verified fingerprint in hex);
    /// the transfer itself starts now. `id` (CR-P3-08) is the task id the
    /// daemon can know BEFORE the transfer: the wired scheduler path's
    /// `TaskOpen.job_id`, i.e. the sender's scheduler task identity (pairs
    /// with the sender's `sched` snapshot, not with [`DaemonEvent::JobDone`]'s
    /// `id`, which is the content id the Offer carries — the engine emits
    /// `Connected` before reading the Offer, so no content id can exist at
    /// this point; e716a61's finding). `None` (rendered `null`) on the
    /// legacy/direct paths, which send no TaskOpen. External consumers pair
    /// `job_started`/`job_done` of one job by `peer` — since CR-P3-08 both
    /// events always carry it.
    JobStarted { peer: &'a str, id: Option<&'a str> },
    /// One job finished and committed; the daemon keeps listening.
    /// `peer` (CR-P3-08) is always filled when the daemon observed one:
    /// the report's own field first, the `job_started` capture as the
    /// fallback (`null` only if the transfer completed without a single
    /// `Connected` observation, which cannot happen on a real path).
    JobDone {
        id: &'a str,
        files: u64,
        bytes: u64,
        duration_ms: u64,
        resumed: u64,
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
        // Starting renders before the match so the frozen arms stay
        // byte-identical (extend-only).
        if let DaemonEvent::Starting { phase } = self {
            return format!(
                "{{\"type\":\"starting\",\"phase\":\"{}\"}}",
                crate::json_escape(phase)
            );
        }
        match self {
            // Rendered above; the arm exists only for exhaustiveness.
            DaemonEvent::Starting { .. } => unreachable!("Starting handled above"),
            DaemonEvent::Listening {
                addr,
                port,
                fingerprint,
                name,
                mdns,
                candidates,
            } => format!(
                "{{\"type\":\"listening\",\"addr\":\"{}\",\"port\":{port},\
                 \"fingerprint\":\"{}\",\"name\":\"{}\",\"mdns\":{mdns},\
                 \"candidates\":{}}}",
                crate::json_escape(addr),
                crate::json_escape(fingerprint),
                crate::json_escape(name),
                crate::json_string_array(candidates)
            ),
            DaemonEvent::JobStarted { peer, id } => {
                // CR-P3-08: `id` APPENDED after `peer` (extend-only — the
                // frozen prefix stays byte-identical).
                let id_json = match id {
                    Some(id) => format!("\"{}\"", crate::json_escape(id)),
                    None => "null".to_owned(),
                };
                format!(
                    "{{\"type\":\"job_started\",\"peer\":\"{}\",\"id\":{id_json}}}",
                    crate::json_escape(peer)
                )
            }
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
/// mode everything is human copy on stdout. `Copy`: every concurrent
/// connection handler owns one (printing is line-atomic through
/// `println!`'s stdout lock).
#[derive(Clone, Copy)]
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

/// 32-hex rendering of a 16-byte job/task id (the wire `Offer.job_id` of a
/// finished report, same rendering as `report_summary`, and the
/// `TaskOpen.job_id` of a started scheduler task).
fn job_id_hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the daemon observed at `job_started` (CR-P3-08): the start instant
/// (`job_done.duration_ms` spans started→done) and the verified peer
/// fingerprint — the fallback that keeps `job_done.peer` filled even when
/// the report's own peer field is empty.
struct JobStart {
    at: Instant,
    peer: String,
}

/// Progress adapter for the daemon: `Connected` marks the job start (and
/// becomes a `job_started` event in NDJSON mode); text mode reuses the
/// `receive` single-line renderer, NDJSON mode drops the rest so stdout
/// stays event-only. `start_slot` receives the started-at instant and peer
/// (see [`JobStart`]); `start_id` is the task id the daemon already knows
/// when the printer is armed — the wired scheduler path's `TaskOpen.job_id`
/// — or `None` on paths whose first identity arrives only with the Offer.
fn daemon_progress_printer(
    json_lines: bool,
    start_slot: Arc<Mutex<Option<JobStart>>>,
    start_id: Option<String>,
) -> Arc<dyn Fn(Progress) + Send + Sync> {
    let line = Arc::new(ProgressLine::new("recv"));
    Arc::new(move |progress: Progress| {
        if let Progress::Connected { peer } = &progress {
            *start_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(JobStart {
                at: Instant::now(),
                peer: peer.clone(),
            });
            if json_lines {
                emit_line(
                    &DaemonEvent::JobStarted {
                        peer,
                        id: start_id.as_deref(),
                    }
                    .to_json_line(),
                );
                return;
            }
        }
        if !json_lines && std::env::var_os("AK_QUIET").is_none() {
            line.show(&progress);
        }
    })
}

/// Map the `--same-code`/`--cross-code` flag words onto the scheduler
/// policy (pure; unit-tested). Omitted flags keep the contract defaults
/// (same-code serial, cross-code parallel, and the wave-24 caps
/// 16/16/16).
pub(crate) fn scheduler_policy_from_flags(
    same_code: Option<&str>,
    cross_code: Option<&str>,
) -> Result<ak_session::runtime::SchedulerPolicy> {
    use ak_session::runtime::SchedulerPolicy;
    let same = ak_session::scheduler::SameCodeMode::parse(same_code.unwrap_or("serial"))
        .context("--same-code must be serial or parallel")?;
    let cross = ak_session::scheduler::CrossCodeMode::parse(cross_code.unwrap_or("parallel"))
        .context("--cross-code must be serial or parallel")?;
    let policy = SchedulerPolicy {
        same_code_mode: same,
        cross_code_mode: cross,
        ..SchedulerPolicy::default()
    };
    policy
        .validate()
        .context("the scheduler policy from --same-code/--cross-code is invalid")?;
    Ok(policy)
}

/// The daemon's wired session runtime (wave 24): the real
/// [`ak_session::pairing::RealTransport`] over the pinned v2 registry client, the real
/// [`ak_session::runtime::RealTransferExecutor`] receive kernel as the inbound executor, and
/// the initial [`ak_session::runtime::SchedulerPolicy`] from the flags. The daemon's own
/// receive loop is the resident consumer of pairing events, so the
/// runtime's listener stays off (it would double-bind the receive port);
/// retries are disabled (`max_attempts = 1`) because the daemon loop
/// itself is the retry: it re-enqueues a fresh receive task per
/// iteration.
struct WiredRuntime {
    handle: ak_session::runtime::SessionHandle,
    /// The pinned registry identity (the scheduler group's registry half).
    registry_id: [u8; 32],
}

/// Start the wired runtime (see [`WiredRuntime`]). `default_executor` is
/// the never-used listener slot (the listener is disabled).
/// `terminals` is the CR-P2-15 terminal-print registry: it becomes the
/// runtime's `on_attempt_settled` hook, so every wired job's
/// `job_done`/`error` line prints from INSIDE the actor's finish
/// handling — strictly before the scheduler releases the task's slot,
/// hence before the task admitted into that slot can print anything.
/// CR-P3-17: only the hook's TERMINAL invocations print (a retryable
/// failure attempt settles non-terminal and must not end the job's
/// event stream), so the guarantee no longer rests on the daemon's
/// single-attempt budget.
async fn start_wired_runtime(
    client: ak_session::pairing::RegistryClient,
    settings: &crate::invite::RegistrySettings,
    policy: ak_session::runtime::SchedulerPolicy,
    default_executor: Arc<ak_session::runtime::RealTransferExecutor>,
    terminals: Arc<JobTerminals>,
) -> Result<WiredRuntime> {
    use ak_session::runtime::{
        RealClock, RetryPolicy, RuntimeClock, RuntimeConfig, SessionRuntime, TransferExecutor,
    };
    // Wave 27: the caller hands in THE daemon's one shared registry client.
    // The v2 registry keeps exactly ONE live connection per device (a new
    // Hello kicks the previous), so a second connection here used to kick
    // the pairing service's connection off — every redeem offer then went
    // to a plane nobody read (the silent no-pairing failure).
    let transport = ak_session::pairing::RealTransport::new(client);
    let clock: Arc<dyn RuntimeClock> = Arc::new(RealClock::new());
    let mut config = RuntimeConfig::new(
        policy,
        clock,
        Arc::new(transport) as Arc<dyn ak_session::runtime::PairingTransport>,
        default_executor as Arc<dyn TransferExecutor>,
        ak_session::runtime::RegistryId::from_bytes(settings.registry_id),
    );
    // CR-P2-15/CR-P3-17: the settled hook prints the job's terminal line
    // before the slot release, and only on the task's TERMINAL settlement
    // — a retryable failure settles non-terminal and leaves the registry
    // entry for the eventual terminal outcome. The handler-side fallback
    // (see [`wired_connection_handler`]) covers terminal paths that never
    // reach an executor finish report.
    config.on_attempt_settled = Some(Arc::new(
        move |task: &ak_session::scheduler::key::TaskHandle,
              _outcome: &ak_session::runtime::FinishOutcome,
              terminal: bool| {
            if !terminal {
                // A retryable attempt settled; the job is not over yet.
                return;
            }
            let id = task.task_id();
            if let Some(context) = terminals.withdraw(&id) {
                context.print(&id);
            }
        },
    ));
    // The daemon loop is the resident receive path; the runtime listener
    // would submit competing receive tasks on the same port.
    config.listener_enabled = false;
    // One attempt per enqueued receive: the loop itself rebinds. The
    // terminal print above no longer depends on this budget (CR-P3-17);
    // it stays because re-binding a fresh receive per iteration is the
    // daemon's own retry shape.
    config.retry = RetryPolicy {
        max_attempts: 1,
        initial_backoff_ms: 0,
        max_backoff_ms: 0,
        multiplier: 1,
    };
    let handle = SessionRuntime::start(config).context("cannot start the scheduler runtime")?;
    Ok(WiredRuntime {
        handle,
        registry_id: settings.registry_id,
    })
}

/// Best-effort session shutdown at daemon exit (wave 27): DRAIN in-flight
/// work first — the actor waits (bounded by its own `shutdown_timeout_ms`)
/// for the running receives to finish naturally, force-cancels residuals
/// and JOINS every real engine work task before answering (wave-26 P1-03
/// join semantics), so when this returns no engine still holds a port or
/// writes files. The outer bound keeps a wedged engine abort from delaying
/// the exit beyond two seconds here.
async fn stop_wired_runtime(handle: &ak_session::runtime::SessionHandle) {
    use ak_session::runtime::ShutdownMode;
    let _ =
        tokio::time::timeout(Duration::from_secs(2), handle.shutdown(ShutdownMode::Drain)).await;
}

/// How one receive task failed (mirrors the direct loop's error matching
/// so both paths share the response arms).
enum JobFailure {
    /// A peer outside the accept policy was rejected.
    Unauthorized,
    /// The bind lost a port race (the direct loop retries with backoff).
    AddrInUse,
    /// The task was cancelled (daemon shutdown): no NDJSON event — the
    /// `stopped` event is the closing line.
    Cancelled,
    /// Anything else; `message` is bounded sanitized text.
    Other { message: String },
}

/// Map the executor's stable failure classification onto the loop's
/// failure kinds (the same distinctions the direct path's `EngineError`
/// match makes).
fn classify_real_failure(failure: &ak_session::runtime::RealFailure) -> JobFailure {
    use ak_core::error::ErrorCode;
    match failure.code {
        ErrorCode::AuthFailed => JobFailure::Unauthorized,
        ErrorCode::Cancelled => JobFailure::Cancelled,
        ErrorCode::StorageError if failure.io_kind == Some(std::io::ErrorKind::AddrInUse) => {
            JobFailure::AddrInUse
        }
        _ => JobFailure::Other {
            message: failure.message.clone(),
        },
    }
}

/// Watch the runtime event stream until `task` reaches a terminal-for-
/// the-loop scheduler event (finished, parked — retries are disabled so
/// the single allowed failure parks — or cancelled), then classify the
/// outcome through `outcome_of` (each wiring passes its executor's
/// query).
async fn watch_wired_task(
    events: &mut tokio::sync::broadcast::Receiver<ak_session::runtime::SessionEvent>,
    task: ak_session::runtime::TransferId,
    outcome_of: impl FnOnce(&ak_session::runtime::TransferId) -> Option<RealOutcome>,
) -> Result<ak_engine::Report, JobFailure> {
    use ak_session::runtime::SessionEvent;
    use ak_session::scheduler::SchedulerEvent;
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        let mine = match &event {
            SessionEvent::Scheduler(SchedulerEvent::TaskFinished { task: t, .. })
            | SessionEvent::Scheduler(SchedulerEvent::TaskCancelled { task: t, .. }) => {
                t.task_id() == task
            }
            // Parking is a runtime-level event (the scheduler core's
            // `pause` emits nothing); with retries disabled it is the
            // terminal state of a failed receive.
            SessionEvent::TaskParked { task: t, .. } => *t == task,
            _ => false,
        };
        if mine {
            break;
        }
    }
    match outcome_of(&task) {
        Some(RealOutcome::Completed { report }) => Ok(report),
        Some(RealOutcome::Failed(failure)) => Err(classify_real_failure(&failure)),
        None => Err(JobFailure::Other {
            message: "the receive task ended without an engine outcome".to_owned(),
        }),
    }
}

/// `ak-cli daemon`: resident receive mode (mDNS advertise + receive loop +
/// event output). See the module docs; Ctrl-C withdraws the announcement
/// and exits 0.
pub(crate) async fn daemon_command(id: &Path, opts: DaemonOptions) -> Result<()> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        // Ctrl-C flips the shared shutdown flag; the core loop observes it
        // alongside everything else.
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(true);
    });
    daemon_core(id, opts, shutdown_rx).await
}

// ===========================================================================
// Wave-27 parallel-agent API (final review P1-02) — ONE integration surface.
//
// The entries below are owned by the parallel engine/session agents and
// land in the same wave; every reference in this file goes through the
// wrappers in this block, so reconciling the final shapes is a handful of
// lines here instead of a diff across the file:
//
//   ak-engine (landed)
//     channel_scheduler_handshake(&mut ch, &ReceiveSpec)
//         -> Result<SchedulerHandshake, EngineError>
//       SchedulerHandshake::Legacy { peer_capabilities: u16 }
//       SchedulerHandshake::Scheduler { open: TaskOpenAdmission }
//   ak-session (landed)
//     ChannelReceiveExecutor::new(ch, ReceiveSpec, open, observer)
//       + claim()/peer_fingerprint()/outcome() accessors
//     runtime::admission::AdmissionValidator::open(id_dir, registry_id)
//       + resolve(peer, &TaskOpenClaim) -> Result<ResolvedAdmission, ..>
//
//   STILL-LANDING (compile fails here until the engine agent exposes it —
//   expected per the wave brief): the LEGACY continuation
//     receive_job(ch, ReceiveSpec, peer_capabilities, progress)
//   — the frozen single-job body from Offer to FinalAck on the
//   Hello-exchanged stream. This requires the engine's Legacy arm to hand
//   the Hello-exchanged stream back (today `channel_scheduler_handshake`
//   drops it, which would strand a legacy peer); the wrapper below is the
//   single call site to reconcile.
// ===========================================================================

/// The handshake split outcome in daemon terms (see the block comment
/// above): a legacy peer, or a scheduler-capable sender whose `TaskOpen`
/// admission context the engine already decoded.
enum WiredHandshake {
    /// Peer without `CAP_SCHEDULER`: keep the frozen single-job protocol
    /// on the handed-back Hello-exchanged stream.
    Legacy {
        peer_capabilities: u16,
        stream: ak_engine::HelloStream,
    },
    /// Scheduler-capable sender: `open` is its `TaskOpen` admission
    /// context (claimed group + registry + pairing relation) with the
    /// live Hello-exchanged protocol stream retained inside.
    Scheduler { open: ak_engine::TaskOpenAdmission },
}

/// Run the scheduler handshake split on one accepted channel (wave-27
/// engine API): identity re-check (`spec.authorize`), policy gate, the
/// Hello exchange, and — for a scheduler-capable peer — its `TaskOpen`.
async fn wired_handshake(
    channel: &mut ak_net::DirectChannel,
    spec: &ReceiveSpec,
) -> Result<WiredHandshake, EngineError> {
    match ak_engine::channel_scheduler_handshake(channel, spec).await {
        Ok(ak_engine::SchedulerHandshake::Legacy {
            peer_capabilities,
            stream,
        }) => Ok(WiredHandshake::Legacy {
            peer_capabilities,
            stream,
        }),
        Ok(ak_engine::SchedulerHandshake::Scheduler { open }) => {
            Ok(WiredHandshake::Scheduler { open })
        }
        Err(err) => Err(err),
    }
}

/// Continue one legacy (no `CAP_SCHEDULER`) receive on the already
/// handshook channel: the frozen single-job body from Offer to FinalAck
/// on the SAME Hello-exchanged stream (the integrator-side engine entry
/// landed as `receive_after_hello`; it re-runs the authorize/policy
/// defenses and emits exactly one `Connected`, which the daemon printer
/// turns into the `job_started` event).
async fn wired_legacy_body(
    channel: ak_net::DirectChannel,
    spec: ReceiveSpec,
    hello: ak_engine::HelloStream,
    peer_capabilities: u16,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    ak_engine::receive_after_hello(channel, spec, hello, peer_capabilities, progress).await
}

/// The anonymous legacy code group (wave 24 sentinel kept verbatim): all
/// legacy connections of one daemon share it, so the same-code policy
/// governs them as one code — policy stays visible on the compatibility
/// path. It never reaches the wire (the Local plan carries no TaskOpen).
fn legacy_group(registry_id: [u8; 32]) -> ak_session::runtime::CodeGroupKey {
    ak_session::runtime::CodeGroupKey::new(
        ak_session::runtime::RegistryId::from_bytes(registry_id),
        ak_session::runtime::CodeId::from_bytes([0u8; 16]),
    )
}

// ---------------------------------------------------------------------------
// Legacy channel executor
// ---------------------------------------------------------------------------

/// Grace given to the engine kernel when a cancellation races the final
/// commit (mirrors the runtime's real-executor bound, P1-03): between the
/// terminal `completed` checkpoint save and the `FinalAck { verified:
/// true }` progress event the kernel is a few frame writes from a fully
/// persisted completion; 250 ms lets that finish (reporting `Completed`)
/// while an ordinary mid-transfer cancel stays prompt.
const LEGACY_FINAL_COMMIT_GRACE: Duration = Duration::from_millis(250);

/// Classify an engine failure onto the stable scheduler error code (the
/// same mapping `ak-session`'s real executor applies; duplicated here
/// because the engine taxonomy is public only through this file's use).
fn legacy_error_code(err: &EngineError) -> (ak_core::error::ErrorCode, Option<std::io::ErrorKind>) {
    use ak_core::error::ErrorCode;
    match err {
        EngineError::Connect(ak_net::ConnectError::Unauthorized) => (ErrorCode::AuthFailed, None),
        EngineError::Connect(_) => (ErrorCode::DirectUnavailable, None),
        EngineError::Stream(_) | EngineError::PeerClosed | EngineError::IdleTimeout(_) => {
            (ErrorCode::DirectUnavailable, None)
        }
        EngineError::Rejected(code) | EngineError::PeerError { code, .. } => (*code, None),
        EngineError::SourceChanged => (ErrorCode::SourceChanged, None),
        EngineError::Storage(_) | EngineError::Checkpoint(_) => (ErrorCode::StorageError, None),
        EngineError::StorageFull { .. } => (ErrorCode::StorageFull, None),
        EngineError::Io(io) => (ErrorCode::StorageError, Some(io.kind())),
        EngineError::VerificationFailed => (ErrorCode::HashMismatch, None),
        EngineError::Cancelled => (ErrorCode::Cancelled, None),
        EngineError::RouteForbidden(_) => (ErrorCode::RouteForbidden, None),
        EngineError::AdmissionDenied { .. } => (ErrorCode::ProtocolUnsupported, None),
        // Wave 28: the receiving user declined through a TransferConsent
        // hook; the peer already saw the DECLINED wire frame (mapped onto
        // LocalPermissionDenied by the engine), the scheduler code mirrors
        // it (same mapping as ak-session's real executor).
        EngineError::ConsentDeclined { .. } => (ErrorCode::LocalPermissionDenied, None),
        EngineError::InvalidAdmissionCtx { .. } | EngineError::PeerMismatch { .. } => {
            (ErrorCode::PeerMismatch, None)
        }
    }
}

/// One running legacy task (cancel flag + joinable work, P1-03 shape).
struct LegacyRunning {
    cancel: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct LegacyState {
    running: HashMap<ak_session::runtime::TransferId, LegacyRunning>,
    outcomes: HashMap<ak_session::runtime::TransferId, RealOutcome>,
}

/// One-shot legacy receive executor (wave 27): runs the frozen single-job
/// receive body on an ALREADY-ACCEPTED, ALREADY-HANDSHOOK channel — the
/// daemon's compatibility path for peers without `CAP_SCHEDULER` (e.g.
/// the previous `send --fp`). Same contract surface as the runtime's
/// `RealTransferExecutor`: one channel per instance, cooperative cancel
/// that drops the engine kernel (closing the channel and stopping every
/// staged write) before reporting, persisted-completion adoption when a
/// cancel races the final commit, joinable work for shutdown, and a
/// queryable outcome.
struct LegacyChannelExecutor {
    channel: Mutex<Option<ak_net::DirectChannel>>,
    hello: Mutex<Option<ak_engine::HelloStream>>,
    peer_capabilities: u16,
    spec: Mutex<Option<ReceiveSpec>>,
    observer: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
    state: Arc<Mutex<LegacyState>>,
}

impl LegacyChannelExecutor {
    /// Arm the executor with the accepted channel, its Hello-exchanged
    /// stream (handed back by the handshake's Legacy arm), the
    /// capabilities the handshake observed, the connection's engine spec
    /// and an optional progress observer (the daemon printer rides here).
    fn new(
        channel: ak_net::DirectChannel,
        hello: ak_engine::HelloStream,
        peer_capabilities: u16,
        spec: ReceiveSpec,
        observer: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
    ) -> Self {
        LegacyChannelExecutor {
            channel: Mutex::new(Some(channel)),
            hello: Mutex::new(Some(hello)),
            peer_capabilities,
            spec: Mutex::new(Some(spec)),
            observer,
            state: Arc::new(Mutex::new(LegacyState::default())),
        }
    }

    /// The outcome of the task's attempt once it settled (`None` while
    /// running).
    fn outcome(&self, task: &ak_session::runtime::TransferId) -> Option<RealOutcome> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .outcomes
            .get(task)
            .cloned()
    }
}

impl ak_session::runtime::TransferExecutor for LegacyChannelExecutor {
    fn enqueue(
        &self,
        task: ak_session::scheduler::key::TaskHandle,
        spec: ak_session::runtime::TransferSpec,
        sink: ak_session::runtime::ProgressSink,
    ) -> Result<(), ak_session::runtime::EngineStartError> {
        use ak_session::runtime::{EngineStartError, TransferDirection};
        if spec.direction != TransferDirection::Receive {
            return Err(EngineStartError::new(
                ak_core::error::ErrorCode::Internal,
                "the legacy channel executor is receive-only",
            ));
        }
        let id = task.task_id();
        let channel = self
            .channel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| {
                EngineStartError::new(
                    ak_core::error::ErrorCode::Internal,
                    "this legacy executor already ran its one channel",
                )
            })?;
        let hello = self
            .hello
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .expect("the Hello stream is consumed together with the channel");
        let engine_spec = self
            .spec
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .expect("the spec is consumed together with the channel");

        let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        // "The completion is persisted": the engine saves the terminal
        // `completed` checkpoint strictly before emitting
        // `FinalAck { verified: true }` — the flag a cancellation must
        // lose against (P1-03).
        let final_ack = Arc::new(AtomicBool::new(false));
        let observer = self.observer.clone();
        let progress_final_ack = Arc::clone(&final_ack);
        let progress = move |progress: Progress| {
            if let Some(observer) = observer.as_ref() {
                observer(progress.clone());
            }
            match progress {
                Progress::Connected { .. } => {
                    let _ = phase_tx.send(ak_session::scheduler::state::TaskPhase::Transferring);
                }
                Progress::FinalAck { verified: true } => {
                    progress_final_ack.store(true, Ordering::Release);
                }
                _ => {}
            }
        };

        let peer_caps = self.peer_capabilities;
        let task_sink = sink.clone();
        let shared_state = Arc::clone(&self.state);
        let work = tokio::spawn(async move {
            use ak_session::scheduler::state::{FinishOutcome, TaskPhase};
            task_sink.phase(task, TaskPhase::Scanning).await;
            let mut engine_fut = Box::pin(wired_legacy_body(
                channel,
                engine_spec,
                hello,
                peer_caps,
                &progress,
            ));
            let mut transferring_reported = false;
            let result = loop {
                tokio::select! {
                    biased;
                    phase = phase_rx.recv() => {
                        if let Some(phase) = phase {
                            if phase == TaskPhase::Transferring {
                                transferring_reported = true;
                            }
                            task_sink.phase(task, phase).await;
                        }
                    }
                    outcome = &mut engine_fut => break Some(outcome),
                    _ = cancel_rx.changed() => {
                        if !*cancel_rx.borrow_and_update() {
                            continue;
                        }
                        if final_ack.load(Ordering::Acquire) {
                            // Persisted completion: let the kernel finish.
                            break Some((&mut engine_fut).await);
                        }
                        match tokio::time::timeout(
                            LEGACY_FINAL_COMMIT_GRACE,
                            &mut engine_fut,
                        )
                        .await
                        {
                            Ok(outcome) => break Some(outcome),
                            Err(_grace_elapsed) => break None,
                        }
                    }
                }
            };
            // Drop the kernel BEFORE recording and reporting (P1-03): the
            // channel closes and staged writes stop here.
            drop(engine_fut);
            let failure_code = {
                let mut state = shared_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match result {
                    Some(Ok(report)) => {
                        state.outcomes.insert(id, RealOutcome::Completed { report });
                        None
                    }
                    Some(Err(err)) => {
                        let (code, io_kind) = legacy_error_code(&err);
                        state.outcomes.insert(
                            id,
                            RealOutcome::Failed(ak_session::runtime::RealFailure {
                                code,
                                io_kind,
                                message: err.to_string().chars().take(160).collect(),
                            }),
                        );
                        Some(code)
                    }
                    None => {
                        state.outcomes.insert(
                            id,
                            RealOutcome::Failed(ak_session::runtime::RealFailure {
                                code: ak_core::error::ErrorCode::Cancelled,
                                io_kind: None,
                                message: "cancelled: the engine kernel was dropped".to_owned(),
                            }),
                        );
                        Some(ak_core::error::ErrorCode::Cancelled)
                    }
                }
            };
            match failure_code {
                None => {
                    if !transferring_reported {
                        task_sink.phase(task, TaskPhase::Transferring).await;
                    }
                    task_sink.phase(task, TaskPhase::Verifying).await;
                    task_sink.phase(task, TaskPhase::Committing).await;
                    task_sink.finished(task, FinishOutcome::Completed).await;
                }
                Some(code) => {
                    task_sink
                        .finished(task, FinishOutcome::Failed { error: code })
                        .await;
                }
            }
            shared_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .running
                .remove(&id);
        });

        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running
            .insert(
                id,
                LegacyRunning {
                    cancel: cancel_tx,
                    join: work,
                },
            );
        Ok(())
    }

    fn cancel(&self, task: &ak_session::scheduler::key::TaskHandle) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = state.running.get(&task.task_id()) {
            let _ = running.cancel.send(true);
        }
    }

    fn take_work(
        &self,
        task: &ak_session::scheduler::key::TaskHandle,
    ) -> Option<ak_session::runtime::ExecutorWork> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running
            .remove(&task.task_id())
            .map(|running| ak_session::runtime::ExecutorWork::new(running.join))
    }
}

// ---------------------------------------------------------------------------
// Wired accept loop (wave 27)
// ---------------------------------------------------------------------------

/// The receive-loop kernel's rebind retry budget: the previous process's
/// QUIC endpoint releases its UDP socket asynchronously after drop, so a
/// fresh bind may lose a short port race (the wired path needs this only
/// for its ONE initial bind; the direct loop rebinds per iteration).
const MAX_REBIND_RETRIES: u32 = 50;

/// The per-connection slice of [`ReceiveLoopConfig`] the wired handlers
/// need (everything but `bind`, which the resident endpoint already
/// consumed). `Clone`: every accepted connection builds one fresh engine
/// spec from it — including a fresh `authorize` closure, so the engine's
/// per-entry identity re-check re-reads peers.book exactly like the
/// accept loop's own check.
#[derive(Clone)]
struct WiredJobTemplate {
    out_dir: PathBuf,
    identity: ak_identity::tls::TlsBundle,
    device_name: String,
    conflict: ak_core::handle::ConflictPolicy,
    checkpoint_dir: PathBuf,
    idle_timeout: Duration,
    allow_internet: bool,
    policy: crate::ExpectPolicy,
}

impl WiredJobTemplate {
    fn of(config: &ReceiveLoopConfig) -> Self {
        WiredJobTemplate {
            out_dir: config.out.clone(),
            identity: config.bundle.clone(),
            device_name: config.device_name.clone(),
            conflict: config.conflict,
            checkpoint_dir: config.checkpoints.clone(),
            idle_timeout: config.idle,
            allow_internet: config.allow_internet,
            policy: config.policy.clone(),
        }
    }

    /// One connection's engine receive spec. `bind` is unused on the
    /// channel path (the channel exists); `authorize` is re-armed from
    /// the accept policy because both wave-27 engine entries re-run the
    /// identity check themselves (defense in depth); the destination,
    /// conflict policy, checkpoints, idle budget, name and route gate all
    /// apply exactly as in `receive_with_addr`.
    fn spec(&self) -> ReceiveSpec {
        let mut spec = ReceiveSpec::new(self.out_dir.clone(), self.identity.clone());
        spec.conflict = self.conflict;
        spec.checkpoint_dir = self.checkpoint_dir.clone();
        spec.device_name = self.device_name.clone();
        spec.idle_timeout = self.idle_timeout;
        spec.policy.allow_internet = self.allow_internet;
        spec.authorize = self.policy.authorize();
        spec
    }
}

/// The executor of one wired connection, whichever branch the handshake
/// split took. Both variants expose the same outcome query the task
/// watcher needs. `Clone` (cheap Arc clones): the connection handler and
/// the CR-P2-15 terminal-print registry each keep a copy.
#[derive(Clone)]
enum WiredExecutor {
    /// Legacy peer: the daemon's channel executor (this file).
    Legacy(Arc<LegacyChannelExecutor>),
    /// Scheduler peer: the session channel executor (wave-27 session API).
    Channel(Arc<ak_session::runtime::ChannelReceiveExecutor>),
}

impl WiredExecutor {
    fn transfer(&self) -> Arc<dyn ak_session::runtime::TransferExecutor> {
        match self {
            WiredExecutor::Legacy(executor) => Arc::clone(executor) as Arc<_>,
            WiredExecutor::Channel(executor) => Arc::clone(executor) as Arc<_>,
        }
    }

    fn outcome(&self, task: &ak_session::runtime::TransferId) -> Option<RealOutcome> {
        match self {
            WiredExecutor::Legacy(executor) => executor.outcome(task),
            // Same queryable-outcome contract as the runtime's real
            // executor (wave-27 session API).
            WiredExecutor::Channel(executor) => executor.outcome(task),
        }
    }
}

/// Everything one accepted connection's handler owns.
struct WiredConnection {
    handle: ak_session::runtime::SessionHandle,
    validator: Arc<ak_session::runtime::admission::AdmissionValidator>,
    template: WiredJobTemplate,
    out: DaemonOut,
    registry_id: [u8; 32],
    shutdown: tokio::sync::watch::Receiver<bool>,
    channel: ak_net::DirectChannel,
    /// CR-P2-15: the daemon-wide terminal-print registry (see
    /// [`JobTerminals`]).
    terminals: Arc<JobTerminals>,
}

/// CR-P2-15: one job's terminal print context — everything needed to
/// print the job's final NDJSON event (or text-mode report summary) the
/// moment the runtime reports the task's terminal settlement. Registered
/// under the task id BEFORE the task is submitted, consumed exactly
/// once: normally by the runtime's `on_attempt_settled` hook on its
/// TERMINAL invocation (CR-P3-17: retryable attempts settle non-terminal
/// and do not consume it), which runs strictly before the scheduler
/// releases the task's slot (see [`start_wired_runtime`]), so the
/// `job_done`/`error` line can never land after the next task admitted
/// into that slot prints its `job_started`.
struct JobTerminal {
    out: DaemonOut,
    start_slot: Arc<Mutex<Option<JobStart>>>,
    executor: WiredExecutor,
}

impl JobTerminal {
    /// Print the terminal line of one job: the `job_done` event (or
    /// report summary in text mode) on success, the matching `error`
    /// event on failure. A shutdown cancellation prints nothing — the
    /// closing `stopped` event is the report. Byte-identical to the
    /// event lines the handler used to print after the task watcher
    /// returned; only the TIMING moved (before slot release).
    fn print(&self, task: &ak_session::runtime::TransferId) {
        match self.executor.outcome(task) {
            Some(RealOutcome::Completed { report }) => {
                emit_job_done(&self.out, &self.start_slot, &report);
            }
            Some(RealOutcome::Failed(failure)) => match classify_real_failure(&failure) {
                JobFailure::Unauthorized => self.out.event(&DaemonEvent::Error {
                    message: "rejected a peer outside the accept policy",
                }),
                JobFailure::Cancelled => {}
                JobFailure::AddrInUse => self.out.event(&DaemonEvent::Error {
                    message: "bind failed: address already in use (retry budget exhausted)",
                }),
                JobFailure::Other { message } => {
                    self.out.event(&DaemonEvent::Error { message: &message });
                }
            },
            None => self.out.event(&DaemonEvent::Error {
                message: "the receive task ended without an engine outcome",
            }),
        }
    }
}

/// CR-P2-15: the wired daemon's terminal-print registry. Every connection
/// handler registers its [`JobTerminal`] under the task id it
/// pre-assigned ([`fresh_wired_task_id`]) BEFORE submitting the task;
/// the runtime's settled hook withdraws and prints it inside the actor's
/// finish handling on the task's TERMINAL settlement — before the slot
/// release. A handler that observes the task's end through a path that
/// never fires a terminal hook invocation (a queued task cancelled at
/// shutdown, a failure the shutdown close-out terminalizes, a closed
/// event stream) finds its entry still present after the task watcher
/// returns and prints the terminal line itself, so every job's terminal
/// line prints exactly once.
#[derive(Default)]
struct JobTerminals {
    entries: Mutex<HashMap<ak_session::runtime::TransferId, Arc<JobTerminal>>>,
}

impl JobTerminals {
    fn register(&self, task: ak_session::runtime::TransferId, terminal: Arc<JobTerminal>) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task, terminal);
    }

    /// Remove and return one task's terminal context; `None` means
    /// somebody else already withdrew (and printed) it.
    fn withdraw(&self, task: &ak_session::runtime::TransferId) -> Option<Arc<JobTerminal>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(task)
    }
}

/// Pre-assigned wired task id (CR-P2-15): the terminal context must be
/// registrable BEFORE the task is submitted, so the daemon assigns the id
/// itself instead of taking the runtime's. The marker byte keeps this
/// namespace clear of the runtime's auto ids (`from_counter` form, whose
/// high eight bytes are zero); a millisecond timestamp plus a
/// process-local counter keeps ids unique within the daemon's runtime.
fn fresh_wired_task_id() -> ak_session::runtime::TransferId {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    bytes[0] = 0xD1;
    bytes[1..9].copy_from_slice(&crate::now_unix_ms().to_be_bytes());
    bytes[9..].copy_from_slice(&seq.to_be_bytes()[1..]);
    ak_session::runtime::TransferId::from_bytes(bytes)
}

/// Serve exactly one accepted connection (spawned per connection by the
/// wired accept loop): run the scheduler handshake split, then either the
/// legacy compatibility receive or the validated scheduler receive — both
/// inside the session runtime, so the policy flags govern real groups —
/// and emit the per-task NDJSON events (`job_started` comes from the
/// engine's `Connected` progress through the daemon printer; `job_done`
/// or `error` from the CR-P2-15 settled hook's TERMINAL invocation, which
/// prints BEFORE the scheduler releases the task's slot).
async fn wired_connection_handler(connection: WiredConnection) {
    let WiredConnection {
        handle,
        validator,
        template,
        out,
        registry_id,
        mut shutdown,
        mut channel,
        terminals,
    } = connection;
    let peer = channel.peer_fingerprint().to_hex();
    let start_slot = Arc::new(Mutex::new(None::<JobStart>));
    let spec = template.spec();

    // Handshake split. Shutdown only races this phase: once a task is
    // enqueued the runtime's drain/cancel settles it (and joins the
    // engine work) before the daemon exits, so the watch below needs no
    // shutdown arm.
    let split = tokio::select! {
        split = wired_handshake(&mut channel, &spec) => split,
        _ = shutdown.changed() => {
            channel.close(0, b"daemon stopping");
            return;
        }
    };
    let (executor, group, pairing, label) = match split {
        Err(error) => {
            out.event(&DaemonEvent::Error {
                message: &error.to_string(),
            });
            out.text(&format!(
                "job failed: {error}; still listening (Ctrl-C stops)"
            ));
            return;
        }
        Ok(WiredHandshake::Legacy {
            peer_capabilities,
            stream,
        }) => {
            // Compatibility path: the frozen single-job protocol on this
            // channel (resumed on the handed-back Hello stream), inside
            // the anonymous legacy group so the same-code policy keeps
            // governing legacy concurrency. No TaskOpen exists here, so
            // the `job_started` event carries no id (CR-P3-08).
            (
                WiredExecutor::Legacy(Arc::new(LegacyChannelExecutor::new(
                    channel,
                    stream,
                    peer_capabilities,
                    spec,
                    Some(daemon_progress_printer(
                        out.json_lines,
                        start_slot.clone(),
                        None,
                    )),
                ))),
                legacy_group(registry_id),
                ak_session::runtime::PairingPlan::Local,
                "daemon-legacy-receive".to_owned(),
            )
        }
        Ok(WiredHandshake::Scheduler { open }) => {
            // CR-P3-08: the TaskOpen's job id is the one task identity
            // that exists BEFORE the transfer starts; it rides the
            // printer as `job_started.id` (the sender's scheduler task
            // identity — `job_done.id` is the later, different content
            // id; see the event docs).
            let start_id = job_id_hex(&open.job_id());
            // The channel executor owns the channel + the wire TaskOpen;
            // its accessors feed the relation check (wave-27 session API:
            // the group key may only come from a validated Ready
            // relation, never from the claim bytes).
            let channel_executor = ak_session::runtime::ChannelReceiveExecutor::new(
                channel,
                spec,
                open,
                Some(daemon_progress_printer(
                    out.json_lines,
                    start_slot.clone(),
                    Some(start_id),
                )),
            );
            let (claim, channel_peer) = match (
                channel_executor.claim(),
                channel_executor.peer_fingerprint(),
            ) {
                (Some(claim), Some(channel_peer)) => (claim, channel_peer),
                _ => {
                    // Cannot happen for a freshly built executor; fail
                    // closed anyway.
                    let message = "scheduler admission lost its channel before validation";
                    out.event(&DaemonEvent::Error { message });
                    out.text(&format!("{message}; still listening (Ctrl-C stops)"));
                    return;
                }
            };
            match validator.resolve(&channel_peer, &claim) {
                Ok(resolved) => (
                    WiredExecutor::Channel(Arc::new(channel_executor)),
                    resolved.group,
                    // Local, not Ready: the actor's Ready path opens a
                    // SIGNALING session toward the peer before the
                    // executor — meaningful for outbound sends, wrong for
                    // an inbound channel that already exists. `Local`
                    // goes straight from admission to the executor; the
                    // REAL validated group key above is what the policy
                    // governs (the plan carries no grouping). The pairing
                    // id rides the label for snapshot visibility.
                    ak_session::runtime::PairingPlan::Local,
                    format!("daemon-receive p{}", &resolved.pairing_id.to_hex()[..16]),
                ),
                Err(reject) => {
                    // Wire semantics: the refused admission closes the
                    // channel (dropping the executor drops it); the
                    // reason is reported, the daemon lives on.
                    drop(channel_executor);
                    let message =
                        format!("rejected a scheduler admission from peer {peer}: {reject}");
                    out.event(&DaemonEvent::Error { message: &message });
                    out.text(&format!("{message}; still listening (Ctrl-C stops)"));
                    return;
                }
            }
        }
    };

    // Subscribe BEFORE the submit: a broadcast receiver only sees messages
    // sent after it existed, and a lightning-fast task must not be able to
    // finish before the watcher is armed.
    let mut events = handle.subscribe();
    // CR-P2-15: pre-assign the task id and register the terminal print
    // context BEFORE submitting — the runtime's settled hook (whose
    // terminal invocation runs strictly before the scheduler releases
    // this task's slot) can then print this job's final line with no
    // registration race, keeping `job_done` ahead of the next same-code
    // task's `job_started` on the event stream no matter how the tokio
    // workers interleave.
    let task_id = fresh_wired_task_id();
    terminals.register(
        task_id,
        Arc::new(JobTerminal {
            out,
            start_slot: Arc::clone(&start_slot),
            executor: executor.clone(),
        }),
    );
    let task = match handle
        .enqueue(ak_session::runtime::TransferSpec {
            group_key: group,
            pairing,
            executor: executor.transfer(),
            direction: ak_session::runtime::TransferDirection::Receive,
            label,
            task_id: Some(task_id),
        })
        .await
    {
        Ok(task) => task,
        Err(ak_session::runtime::SessionError::QueueFull) => {
            // Contract §7: the local queue is full — refuse THIS
            // connection (capacity), never block or disturb others.
            terminals.withdraw(&task_id);
            let message = "queue full: connection refused (capacity)".to_owned();
            out.event(&DaemonEvent::Error { message: &message });
            out.text(&format!("{message}; still listening (Ctrl-C stops)"));
            return;
        }
        Err(err) => {
            terminals.withdraw(&task_id);
            let message = format!("cannot enqueue the receive task: {err}");
            out.event(&DaemonEvent::Error { message: &message });
            out.text(&format!("{message}; still listening (Ctrl-C stops)"));
            return;
        }
    };
    let outcome = watch_wired_task(&mut events, task, |task| executor.outcome(task)).await;
    // CR-P2-15 fallback: terminal paths that never fire a terminal hook
    // invocation (a queued task cancelled at shutdown, a failure the
    // shutdown close-out terminalizes, a closed event stream) find their
    // entry still registered here and print it themselves. Such a task
    // never started a transfer, so nothing it printed can invert the
    // started/done order of a real job. When the hook already printed,
    // the entry is gone and this is a no-op: every job's terminal line
    // prints exactly once.
    if let Some(terminal) = terminals.withdraw(&task_id) {
        terminal.print(&task_id);
    }
    // The terminal EVENT (or report summary) printed above; only the
    // human follow-up lines remain here.
    match outcome {
        Ok(_) => {
            out.text("waiting for next sender (Ctrl-C stops)...");
        }
        Err(JobFailure::Unauthorized) => {
            out.text("rejected a peer outside the accept policy; still listening (Ctrl-C stops)");
        }
        Err(JobFailure::Cancelled) => {
            // Shutdown cancellation: the closing `stopped` event is the
            // report; no per-task error line on the event stream.
            out.text("task cancelled (daemon stopping)");
        }
        Err(JobFailure::AddrInUse) => {
            out.text("bind failed: address already in use (retry budget exhausted); still listening (Ctrl-C stops)");
        }
        Err(JobFailure::Other { message }) => {
            out.text(&format!("{message}; still listening (Ctrl-C stops)"));
        }
    }
}

/// The wired daemon's resident state (wave 27): one bound endpoint, the
/// session runtime, the admission validator over this identity's pairing
/// journal, and the output/mDNS wiring the shutdown path must close.
struct WiredDaemon {
    wired: WiredRuntime,
    id_dir: PathBuf,
    config: ReceiveLoopConfig,
    out: DaemonOut,
    fingerprint: ak_identity::Fingerprint,
    name: String,
    mdns: Option<ak_discovery::MdnsDiscovery>,
    mdns_ok: bool,
    candidates_json: Vec<String>,
    candidate_addrs: Vec<crate::AddressCandidate>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    /// CR-P2-15: the terminal-print registry shared with the runtime's
    /// settled hook (one per daemon; every connection handler registers
    /// its job here).
    terminals: Arc<JobTerminals>,
}

impl WiredDaemon {
    /// The wired receive path: bind once (rebind backoff only on the
    /// initial bind), announce, then accept connections concurrently —
    /// one handler task each — until shutdown; on shutdown drain/cancel
    /// the runtime, join every handler, then emit the closing event.
    async fn run(mut self) -> Result<()> {
        let endpoint = self.bind_once().await?;
        let addr = endpoint
            .local_addr()
            .with_context(|| format!("cannot read the bound address of {}", self.config.listen))?;
        self.announce(addr);
        let validator = Arc::new(
            ak_session::runtime::admission::AdmissionValidator::open(
                &self.id_dir,
                ak_session::runtime::RegistryId::from_bytes(self.wired.registry_id),
            )
            .context("cannot open the pairing journal for admission validation")?,
        );
        let template = WiredJobTemplate::of(&self.config);

        let mut handlers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        loop {
            // A fresh spec per accept: the authorize callback re-reads
            // peers.book, so `pair add` takes effect without a restart
            // (the same rule the direct loop applies per iteration).
            let accept_spec = self.config.iteration_spec();
            tokio::select! {
                accepted = endpoint.accept(&accept_spec.authorize) => {
                    match accepted {
                        Ok(channel) => {
                            handlers.push(tokio::spawn(wired_connection_handler(
                                WiredConnection {
                                    handle: self.wired.handle.clone(),
                                    validator: Arc::clone(&validator),
                                    template: template.clone(),
                                    out: self.out,
                                    registry_id: self.wired.registry_id,
                                    shutdown: self.shutdown.clone(),
                                    channel,
                                    terminals: Arc::clone(&self.terminals),
                                },
                            )));
                            // Reap exited handlers so the join set stays
                            // bounded on a long-lived daemon.
                            handlers.retain(|task| !task.is_finished());
                        }
                        Err(ak_net::ConnectError::Unauthorized) => {
                            self.out.event(&DaemonEvent::Error {
                                message: "rejected a peer outside the accept policy",
                            });
                            self.out.text(
                                "rejected a peer outside the accept policy; still waiting (Ctrl-C stops)",
                            );
                        }
                        Err(err) => {
                            // The endpoint itself is gone (or the accept
                            // machinery failed): the resident listener
                            // cannot continue.
                            let message = format!("accept failed: {err}");
                            self.out.event(&DaemonEvent::Error { message: &message });
                            self.out.text(&format!("{message}; stopping the listener"));
                            break;
                        }
                    }
                }
                _ = self.shutdown.changed() => break,
            }
        }

        // Shutdown (contract §10): stop accepting, withdraw the mDNS
        // announcement, drain-or-cancel every in-flight task (the runtime
        // joins all real engine work before answering), let the handlers
        // report their settled outcomes, and only then close the event
        // stream — `stopped` remains the last NDJSON line.
        if let Some(discovery) = self.mdns.as_mut() {
            discovery.stop_advertise();
        }
        stop_wired_runtime(&self.wired.handle).await;
        for task in handlers.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
        self.out.event(&DaemonEvent::Stopped { reason: "shutdown" });
        self.out
            .text("daemon stopped (mDNS announcement withdrawn; partial files stay resumable)");
        Ok(())
    }

    /// Bind the one wired endpoint. The wave-24 rebind backoff survives
    /// HERE (and only here): the previous process's endpoint releases its
    /// UDP socket asynchronously, so the first bind may lose a short port
    /// race. Once bound, the endpoint serves every connection for the
    /// daemon's whole life — the per-iteration rebind is gone.
    async fn bind_once(&self) -> Result<ak_net::DirectEndpoint> {
        let mut retries = 0u32;
        loop {
            match ak_net::DirectEndpoint::bind(self.config.listen, &self.config.bundle).await {
                Ok(endpoint) => return Ok(endpoint),
                Err(err)
                    if err.kind() == std::io::ErrorKind::AddrInUse
                        && retries < MAX_REBIND_RETRIES =>
                {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(err) => {
                    bail!("daemon cannot bind {}: {err}", self.config.listen)
                }
            }
        }
    }

    /// The once-per-run listening announcement (event + human block),
    /// identical in shape to the direct loop's first iteration.
    fn announce(&self, addr: SocketAddr) {
        self.out.event(&DaemonEvent::Listening {
            addr: &addr.to_string(),
            port: addr.port(),
            fingerprint: &self.fingerprint.to_hex(),
            name: &self.name,
            mdns: self.mdns_ok,
            candidates: &self.candidates_json,
        });
        self.out.text(&format!(
            "daemon listening on {addr} into {} (name {})",
            self.config.out.display(),
            self.name
        ));
        for line in crate::address_candidates_lines(&self.candidate_addrs) {
            self.out.text(&line);
        }
        self.out
            .text(&crate::fingerprint_lines("fingerprint", &self.fingerprint));
        self.out.text(&format!(
            "pairing code: {}",
            ak_identity::pairing_code(&self.fingerprint)
        ));
        self.out.text("waiting for sender (Ctrl-C stops)...");
    }
}

/// The daemon body, shutdown-driven (the command wrapper only wires
/// Ctrl-C into the channel). Split so tests can drive a full daemon run
/// and stop it deterministically.
pub(crate) async fn daemon_core(
    id: &Path,
    opts: DaemonOptions,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let out = DaemonOut::new(opts.json_lines);
    let name = daemon_device_name(opts.name.as_deref())?;

    // Wave 24: any of the scheduler/pairing flags switches the daemon onto
    // the WIRED session runtime — real registry transport
    // (`RealTransport`) plus the real engine receive kernel behind the
    // scheduler (`RealTransferExecutor`), with the receive loop entering
    // through `SessionHandle::enqueue`. Without them the direct engine
    // loop below runs byte-for-byte as before (compatibility gate).
    let wired_mode = opts.same_code.is_some()
        || opts.cross_code.is_some()
        || opts.invite_code.is_some()
        || opts.registry_fp.is_some();
    let registry = if wired_mode {
        Some(crate::invite::resolve_registry(
            id,
            opts.server,
            opts.registry_fp.clone(),
        )?)
    } else {
        None
    };

    // Wave 21: `--invite-code`: redeem at startup, pair with the owner,
    // then keep receiving (the owner lands in peers.book, so the book
    // policy below admits it). MUST run BEFORE the shared client below
    // connects: the redeem opens its own short-lived same-device
    // connection, and under the registry's one-connection-per-device rule
    // the two connections would kick each other for as long as the redeem
    // flow runs (the wave-27 keeper reconnects the shared client, which
    // kills the in-flight redeem — the connection war the smoke test
    // caught). Redeem first, close, then connect the shared client once.
    if let Some(code) = &opts.invite_code {
        let settings = registry
            .as_ref()
            .expect("invite-code implies the wired registry settings");
        // run-43312035496: the redeem transaction can legitimately run
        // ~90-105 s (window + waits) under load; without a progress line
        // the json-lines stdout stays empty through the whole phase.
        if opts.json_lines {
            emit_line(&DaemonEvent::Starting { phase: "redeem" }.to_json_line());
        }
        let verified = crate::invite::redeem_and_pair(id, code, settings).await?;
        if opts.json_lines {
            emit_line(&DaemonEvent::Starting { phase: "paired" }.to_json_line());
        }
        let peer = verified.owner_fingerprint(); // the code owner is the peer
        out.note("paired with the code owner:");
        out.note(&crate::fingerprint_lines("owner", &peer));
    }

    // Wave 27: THE daemon's one registry connection, shared by everything
    // resident (the --pairing orchestrator's owner loop AND the wired
    // runtime's RealTransport). The v2 registry keeps exactly one live
    // connection per device — a second connect used to kick the first,
    // silently rerouting every redeem offer to a plane nobody read.
    //
    // R36-P2-04 / batch G6: the daemon participates in the SAME
    // per-identity control-connection lock as the CLI task commands and
    // the FFI task runtime (ak-session `pairing::connection_lock`). The
    // lease — guard PLUS heartbeat — lives for the WHOLE daemon run:
    // the original code dropped both at the end of the `if` block below
    // while the shared client lived on (the audit's C5 finding: the file
    // vanished immediately, a detached heartbeat kept re-stamping it),
    // so a CLI task run against this id-dir would refuse for a moment
    // and then silently kick the daemon's connection after all. Held
    // here, the lock releases on EVERY exit path of `run` — wired and
    // direct branches alike — via the lease's Drop (heartbeat aborted
    // first, file removed after).
    let mut _registry_lock: Option<ak_session::pairing::ConnectionLockLease> = None;
    let shared_registry = if wired_mode || opts.pairing {
        _registry_lock = Some(
            ak_session::pairing::ConnectionLockLease::acquire(id).map_err(|error| {
                anyhow::Error::new(error)
                    .context("cannot claim the identity's control-connection lock")
            })?,
        );
        let settings = match registry.as_ref() {
            Some(settings) => *settings,
            None => crate::invite::resolve_registry(id, opts.server, opts.registry_fp.clone())?,
        };
        let (key, _) = crate::load_identity(id)?;
        let client = crate::invite::connect_registry(&settings, &key).await?;
        Some((settings, key, client))
    } else {
        None
    };

    // Wave 21: owner-side pairing service (`--pairing`). Resident: spawned,
    // stopped by the shared shutdown flag.
    if opts.pairing {
        let (settings, key, client) = shared_registry
            .as_ref()
            .expect("--pairing implies the shared registry connection");
        let (key, client) = (key.clone(), client.clone());
        out.note(&format!(
            "pairing service: answering redeems for codes in {} (registry {})",
            id.join(crate::invite::INVITE_STORE_FILE).display(),
            settings.server
        ));
        let json_lines = opts.json_lines;
        let id_dir = id.to_path_buf();
        let on_paired = std::sync::Arc::new(move |peer: ak_identity::Fingerprint| {
            if json_lines {
                eprintln!("paired with redeemer {}", peer.to_hex());
            } else {
                println!(
                    "pairing: paired with redeemer {} ({})",
                    peer.short_id(),
                    peer.to_hex()
                );
            }
        });
        // Wave 27: the owner answers through the ORCHESTRATOR's owner loop
        // (the wave-21 `invite::owner_pairing_loop` only wrote peers.book,
        // never the pairing journal — so wave-27's admission validator,
        // whose fact source is the journal's Applied records, would reject
        // every scheduler-capable sender this daemon paired with). Codes
        // created in earlier runs live in `<id-dir>/invites.store`; the
        // orchestrator's secret slots open empty, so every LIVE persisted
        // code is adopted back before the loop starts answering.
        let orchestrator =
            ak_session::pairing::PairingOrchestrator::with_defaults(client, key.clone(), &id_dir)?;
        let now_ms = crate::now_unix_ms();
        for held in crate::invite::InviteStore::open(&id_dir).load()? {
            if !held.live(now_ms) {
                continue; // expired codes are not answered (same rule the
                          // old loop applied per offer)
            }
            let code = held.code()?;
            orchestrator.adopt_persisted_code(&code, held.code_id, held.revision, held.expires_ms);
        }
        // Codes created (or revoked) by OTHER processes while this daemon
        // runs: a LIVE per-offer backfill over the persisted store — the
        // same freshness semantics the wave-21 loop had. `invite create`
        // in a second process is the normal flow (the cross-device script
        // starts the owner daemon before creating its code).
        {
            let store_dir = id_dir.clone();
            let source: ak_session::pairing::PersistedCodeSource =
                std::sync::Arc::new(move |code_id: &[u8; 16]| {
                    let held = crate::invite::InviteStore::open(&store_dir)
                        .find(code_id)
                        .ok()
                        .flatten()?;
                    if !held.live(crate::now_unix_ms()) {
                        return None; // expired: not answered
                    }
                    let code = held.code().ok()?;
                    Some((code, held.revision, held.expires_ms))
                });
            orchestrator.set_persisted_code_source(source);
        }
        orchestrator.spawn_owner_loop_notifying(Some(on_paired));
    }

    // Wave 27: connection keeper for the SHARED registry client. The v2
    // registry keeps one live connection per device, so any same-device
    // process (`invite create`, a second daemon, a probe) KICKS this
    // connection; the client surfaces Disconnected but never re-establishes
    // on its own, which used to leave the owner loop deaf (its plane
    // received nothing until a restart). Wave 35/R36: the common ak-session
    // keeper component — 20 s keepalive pings (inside the server's 30 s
    // frame-silence cut, so an idle daemon also stays online), reconnect
    // on Disconnected with a CAPPED backoff between failed batches
    // (R36-P1-02: a failed batch ends retryable-Disconnected, never
    // wedged in Connecting), and black-hole recovery after repeated ping
    // timeouts. The server's Hello catch-up re-pushes pending redeem
    // offers onto the still-live plane subscription. The keeper ends on
    // the shared shutdown channel.
    if let Some((_settings, _key, client)) = shared_registry.as_ref() {
        let client = client.clone();
        let shutdown_keeper = shutdown.clone();
        client.spawn_connection_keeper(shutdown_keeper);
    }

    let port = if opts.port == 0 {
        pick_free_port()?
    } else {
        opts.port
    };
    let listen = SocketAddr::from(([0, 0, 0, 0], port));
    // Wave 25: the addresses a sender can actually dial on this host (every
    // non-loopback, non-link-local IPv4 interface address carrying the
    // fixed port). Computed once -- the port never changes across loop
    // iterations -- and shared by the human candidates block and the
    // NDJSON `listening` event. Empty when enumeration fails or no IPv4
    // interface exists (the block is a hint; the bind address is reported
    // regardless).
    let candidate_addrs = crate::address_candidates(port);
    let candidates_json = crate::address_candidates_json(&candidate_addrs);
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

    // Wave 24: start the wired session runtime (flags → policy; pinned
    // registry → real transport; the loop below enqueues real receive
    // tasks per iteration). The `default_executor` slot is never used —
    // the resident listener is disabled because the daemon loop itself is
    // the resident receive path. `wired_terminals` (CR-P2-15) is the
    // terminal-print registry the runtime's settled hook and every
    // connection handler share.
    let wired_terminals = Arc::new(JobTerminals::default());
    let wired_runtime = if wired_mode {
        let settings = registry.as_ref().expect("checked above");
        let policy =
            scheduler_policy_from_flags(opts.same_code.as_deref(), opts.cross_code.as_deref())?;
        let default_executor = Arc::new(ak_session::runtime::RealTransferExecutor::receive(
            config.iteration_spec(),
            None,
            None,
        ));
        let shared_client = shared_registry
            .as_ref()
            .expect("wired mode implies the shared registry connection")
            .2
            .clone();
        let wired = start_wired_runtime(
            shared_client,
            settings,
            policy,
            default_executor,
            Arc::clone(&wired_terminals),
        )
        .await?;
        let switch = wired
            .handle
            .set_policy(policy)
            .await
            .context("set_policy failed")?;
        out.note(&format!(
            "scheduler runtime: registry {} (wired); same-code {}, cross-code {}, caps \
             active/queued-per-code/pending {}/{}/{} (revision {}, draining {}, admitted {})",
            settings.server,
            policy.same_code_mode.as_str(),
            policy.cross_code_mode.as_str(),
            policy.max_active_tasks,
            policy.max_queued_tasks_per_code,
            policy.max_pending_admissions,
            switch.revision,
            switch.draining,
            switch.admitted,
        ));
        Some(wired)
    } else {
        None
    };

    // Advertise the same TXT record `discover` reads (fp + name), on the
    // fixed port. mDNS is a discovery hint, not authorization and not a
    // hard dependency: when multicast is unavailable the daemon degrades
    // to manual `--to` connections (REVIEW 4.3).
    //
    // Wave 25 verification (no mDNS code change needed): advertise() uses
    // `enable_addr_auto()`, so the mDNS daemon registers EVERY host
    // interface address (real NIC and VPN alike) as A/AAAA records -- the
    // TXT record carries only `fp` and `name`, never an IP, so there is no
    // VPN-priority problem inside TXT. The browsing side (`discover`)
    // receives every resolved address and `ak_discovery::rank` sorts
    // same-LAN candidates first, mirroring the send fallback's LAN-first
    // order.
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

    // Wave 27 wired branch: hand everything to the resident wired daemon
    // — one bind, a concurrent accept loop (one handler per connection,
    // real code-group scheduling, legacy compatibility) and the drain+
    // join shutdown. This returns only at shutdown.
    if let Some(wired) = wired_runtime {
        let daemon = WiredDaemon {
            wired,
            id_dir: id.to_path_buf(),
            config,
            out,
            fingerprint: fp,
            name,
            mdns,
            mdns_ok,
            candidates_json,
            candidate_addrs,
            shutdown,
            terminals: wired_terminals,
        };
        return daemon.run().await;
    }

    // Direct branch (no registry/scheduler flags): the engine loop as it
    // always ran — bind, accept ONE job at a time, run it to completion,
    // rebind (the compatibility gate). The rebind budget covers the
    // previous iteration's endpoint releasing its UDP socket
    // asynchronously after drop.
    let mut rebind_retries = 0u32;
    let mut first = true;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let spec = config.iteration_spec();
        let start_slot = Arc::new(Mutex::new(None::<JobStart>));
        // Direct path: no TaskOpen precedes the transfer, so the
        // `job_started` event carries no id (CR-P3-08) — the first job
        // identity arrives only with the Offer, after `Connected`.
        let printer = daemon_progress_printer(out.json_lines, start_slot.clone(), None);
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
                candidates: &candidates_json,
            });
            out.text(&format!(
                "daemon listening on {addr} into {} (name {name})",
                config.out.display()
            ));
            for line in crate::address_candidates_lines(&candidate_addrs) {
                out.text(&line);
            }
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
            _ = shutdown.changed() => {
                if let Some(discovery) = mdns.as_mut() {
                    discovery.stop_advertise();
                }
                out.event(&DaemonEvent::Stopped { reason: "shutdown" });
                out.text("daemon stopped (mDNS announcement withdrawn; partial files stay resumable)");
                return Ok(());
            }
        };
        match outcome {
            Ok(Ok(report)) => {
                rebind_retries = 0;
                emit_job_done(&out, &start_slot, &report);
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
    Ok(())
}

/// Render the `job_done` NDJSON line (pure, CR-P3-08; unit-tested). The
/// pairing-relevant fields: `id` is the report's content id, and `peer` is
/// the report's own field first with the `job_started` capture as the
/// fallback — so started/done of one job always carry the same peer and
/// external consumers can pair them reliably.
fn job_done_line(started: Option<&JobStart>, report: &Report) -> String {
    let id = job_id_hex(&report.job_id);
    let duration_ms = match started {
        Some(start) => start.at.elapsed().as_millis(),
        None => report.duration.as_millis(),
    };
    let peer = report
        .peer_fingerprint
        .as_deref()
        .or_else(|| started.map(|start| start.peer.as_str()));
    DaemonEvent::JobDone {
        id: &id,
        files: report.files,
        bytes: report.bytes,
        duration_ms: u64::try_from(duration_ms).unwrap_or(u64::MAX),
        resumed: report.resumed_chunks,
        peer,
    }
    .to_json_line()
}

/// Emit the job-done NDJSON event / text summary shared by both receive
/// paths (the wired connection handlers and the direct loop). Duration
/// spans `job_started` (handshake) to completion; if no `Connected` event
/// was seen, the engine's own report duration is used.
fn emit_job_done(out: &DaemonOut, start_slot: &Arc<Mutex<Option<JobStart>>>, report: &Report) {
    if out.json_lines {
        let started = start_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        emit_line(&job_done_line(started.as_ref(), report));
    } else {
        crate::report_summary(report);
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
    /// Parse CLI args into the daemon options (the full flag surface, wave
    /// 21 fields included).
    fn parse_daemon(args: &[&str]) -> DaemonOptions {
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
                invite_code,
                pairing,
                same_code,
                cross_code,
                server,
                registry_fp,
            } => DaemonOptions {
                out,
                port,
                expect,
                conflict,
                timeout,
                name,
                json_lines,
                invite_code,
                pairing,
                same_code,
                cross_code,
                server,
                registry_fp,
            },
            other => panic!("expected daemon, got {other:?}"),
        }
    }

    #[test]
    fn daemon_flags_parse_with_defaults() {
        let opts = parse_daemon(&["daemon"]);
        assert_eq!(opts.out, PathBuf::from("."));
        assert_eq!(opts.port, 0);
        assert_eq!(opts.expect, None);
        assert_eq!(opts.conflict, "error");
        assert_eq!(opts.timeout, 30);
        assert_eq!(opts.name, None);
        assert!(!opts.json_lines);
        assert_eq!(opts.invite_code, None);
        assert!(!opts.pairing);
        assert_eq!(opts.same_code, None);
        assert_eq!(opts.cross_code, None);
        assert_eq!(opts.server, None);
        assert_eq!(opts.registry_fp, None);
    }

    /// CR-P2-15: pre-assigned wired task ids carry the daemon-namespace
    /// marker byte (the runtime's auto ids are `from_counter` form, high
    /// eight bytes zero) and never repeat within the process.
    #[test]
    fn wired_task_ids_are_marked_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1_000 {
            let id = fresh_wired_task_id();
            assert_eq!(id.as_bytes()[0], 0xD1, "daemon namespace marker byte");
            assert!(seen.insert(id), "task ids never repeat within the process");
        }
    }

    #[test]
    fn daemon_flags_parse_every_override() {
        let opts = parse_daemon(&[
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
            "--invite-code",
            "AKI1-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH",
            "--pairing",
            "--same-code",
            "parallel",
            "--cross-code",
            "serial",
            "--server",
            "127.0.0.1:7200",
            "--registry-fp",
            &"ab".repeat(32),
        ]);
        assert_eq!(opts.out, PathBuf::from("D:/inbox"));
        assert_eq!(opts.port, 7555);
        assert_eq!(opts.expect.as_deref(), Some("any"));
        assert_eq!(opts.conflict, "rename");
        assert_eq!(opts.timeout, 9);
        assert_eq!(opts.name.as_deref(), Some("Alice's laptop"));
        assert!(opts.json_lines);
        assert_eq!(
            opts.invite_code.as_deref(),
            Some("AKI1-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH")
        );
        assert!(opts.pairing);
        assert_eq!(opts.same_code.as_deref(), Some("parallel"));
        assert_eq!(opts.cross_code.as_deref(), Some("serial"));
        assert_eq!(opts.server, Some("127.0.0.1:7200".parse().unwrap()));
        assert_eq!(opts.registry_fp.as_deref(), Some(&"ab".repeat(32)[..]));
    }

    #[test]
    fn daemon_rejects_bad_policy_words_and_half_registry_flags() {
        // Bad concurrency words are refused at parse time.
        assert!(crate::Cli::try_parse_from(["ak-cli", "daemon", "--same-code", "fast"]).is_err());
        assert!(crate::Cli::try_parse_from(["ak-cli", "daemon", "--cross-code", "lazy"]).is_err());
        // Registry flags must come as a pair.
        assert!(
            crate::Cli::try_parse_from(["ak-cli", "daemon", "--server", "127.0.0.1:7200"]).is_err()
        );
        assert!(crate::Cli::try_parse_from([
            "ak-cli",
            "daemon",
            "--registry-fp",
            &"ab".repeat(32),
        ])
        .is_err());
    }

    #[test]
    fn daemon_events_render_exact_single_line_ndjson() {
        let fp_hex = "ab".repeat(32);
        // Wave 25 candidates: real interface addresses a sender can dial.
        let candidates = vec![
            "103.236.80.135:53112".to_owned(),
            "192.168.1.100:53112".to_owned(),
        ];
        let no_candidates: Vec<String> = Vec::new();
        let events = [
            DaemonEvent::Listening {
                addr: "0.0.0.0:53112",
                port: 53112,
                fingerprint: &fp_hex,
                name: "Alice",
                mdns: true,
                candidates: &candidates,
            }
            .to_json_line(),
            DaemonEvent::Listening {
                addr: "[::]:7100",
                port: 7100,
                fingerprint: &fp_hex,
                name: "Bob",
                mdns: false,
                candidates: &no_candidates,
            }
            .to_json_line(),
            DaemonEvent::JobStarted {
                peer: &fp_hex,
                id: None,
            }
            .to_json_line(),
            DaemonEvent::JobStarted {
                peer: &fp_hex,
                id: Some(&"cd".repeat(16)),
            }
            .to_json_line(),
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
                 \"fingerprint\":\"{fp_hex}\",\"name\":\"Alice\",\"mdns\":true,\
                 \"candidates\":[\"103.236.80.135:53112\", \"192.168.1.100:53112\"]}}"
            )
        );
        assert_eq!(
            events[1],
            format!(
                "{{\"type\":\"listening\",\"addr\":\"[::]:7100\",\"port\":7100,\
                 \"fingerprint\":\"{fp_hex}\",\"name\":\"Bob\",\"mdns\":false,\
                 \"candidates\":[]}}"
            )
        );
        assert_eq!(
            events[2],
            format!("{{\"type\":\"job_started\",\"peer\":\"{fp_hex}\",\"id\":null}}")
        );
        assert_eq!(
            events[3],
            format!(
                "{{\"type\":\"job_started\",\"peer\":\"{fp_hex}\",\"id\":\"{}\"}}",
                "cd".repeat(16)
            )
        );
        assert_eq!(
            events[4],
            format!(
                "{{\"type\":\"job_done\",\"id\":\"{}\",\"files\":3,\"bytes\":4096,\
                 \"duration_ms\":1523,\"resumed\":7,\"peer\":\"{fp_hex}\"}}",
                "09af".repeat(8)
            )
        );
        assert_eq!(
            events[5],
            format!(
                "{{\"type\":\"job_done\",\"id\":\"{}\",\"files\":1,\"bytes\":0,\
                 \"duration_ms\":0,\"resumed\":0,\"peer\":null}}",
                "09af".repeat(8)
            )
        );
        assert_eq!(
            events[6],
            "{\"type\":\"error\",\"message\":\"idle timeout\"}"
        );
        assert_eq!(events[7], "{\"type\":\"stopped\",\"reason\":\"ctrl-c\"}");
        for line in events {
            assert!(!line.contains('\n'), "NDJSON line broke: {line}");
            assert!(!line.contains('\r'), "NDJSON line broke: {line}");
        }
    }

    /// CR-P3-08: `job_started`/`job_done` of ONE job must be pairable by
    /// an external NDJSON consumer. Two invariants, driven through the
    /// real emit machinery (the printer closure's `Connected` capture and
    /// the pure `job_done_line`):
    ///
    /// * **peer**: started and done carry the SAME peer string even when
    ///   the report's own peer field is empty — the `job_started`
    ///   capture is the fallback. This is the pairing key that always
    ///   exists (a content id cannot: the engine emits `Connected` before
    ///   reading the Offer, so no `job_done.id`-namespace id exists at
    ///   start time — e716a61's finding).
    /// * **id (format level)**: when started and done carry the same id
    ///   string, both render it byte-identically (`"id":"<hex>"`), so a
    ///   consumer pairing by id matches exactly; `null` stays `null`.
    #[test]
    fn daemon_started_and_done_events_pair_by_peer_and_id() {
        let sender_fp = "ab".repeat(32);
        let task_id = [0xcdu8; 16];
        let report = Report {
            job_id: [0x09u8; 16],
            files: 1,
            bytes: 10,
            duration: Duration::from_millis(2),
            resumed_chunks: 0,
            // The regression shape: a completion path that lost the peer.
            peer_fingerprint: None,
            roots: Vec::new(),
        };

        // The real start capture: the printer's `Connected` arm fills the
        // shared slot with the instant AND the peer (it also prints the
        // started line — the render is asserted via the same event below).
        let start_slot = Arc::new(Mutex::new(None::<JobStart>));
        let printer =
            daemon_progress_printer(true, Arc::clone(&start_slot), Some(job_id_hex(&task_id)));
        printer(Progress::Connected {
            peer: sender_fp.clone(),
        });
        let started = start_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .expect("Connected captured the job start");
        assert_eq!(started.peer, sender_fp, "the capture holds the peer");

        // The started line the printer emitted (same construction), and
        // the done line for a peer-less report: done falls back to the
        // captured peer, so the events pair.
        let started_line = DaemonEvent::JobStarted {
            peer: &started.peer,
            id: Some(&job_id_hex(&task_id)),
        }
        .to_json_line();
        let done_line = job_done_line(Some(&started), &report);
        assert!(started_line.contains(&format!("\"peer\":\"{sender_fp}\"")));
        assert!(
            done_line.contains(&format!("\"peer\":\"{sender_fp}\"")),
            "done.peer fell back to the started capture: {done_line}"
        );

        // Id pairing at the format level: the same id string renders
        // byte-identically in both event shapes (and null stays null).
        let same_id = job_id_hex(&report.job_id);
        let started_with_done_id = DaemonEvent::JobStarted {
            peer: &sender_fp,
            id: Some(&same_id),
        }
        .to_json_line();
        assert!(started_with_done_id.contains(&format!("\"id\":\"{same_id}\"")));
        assert!(done_line.contains(&format!("\"id\":\"{same_id}\"")));
        let started_null = DaemonEvent::JobStarted {
            peer: &sender_fp,
            id: None,
        }
        .to_json_line();
        assert!(started_null.contains("\"id\":null"));
        assert!(done_line.contains(&format!("\"id\":\"{}\"", job_id_hex(&report.job_id))));

        // Without any Connected capture (a done whose start was never
        // observed), the report's own peer still renders when present.
        let mut with_peer = report.clone();
        with_peer.peer_fingerprint = Some(sender_fp.clone());
        assert!(job_done_line(None, &with_peer).contains(&format!("\"peer\":\"{sender_fp}\"")));
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
            candidates: &["192.168.1.100:1".to_owned()],
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

    // ---- wave 21: policy flags + the real loopback pairing smoke ------

    #[test]
    fn policy_flags_map_onto_the_scheduler_policy() {
        use ak_session::runtime::SchedulerPolicy;
        use ak_session::scheduler::{CrossCodeMode, SameCodeMode};

        // Omitted flags keep the contract defaults — including the wave-24
        // caps (16/16/16).
        let defaults = scheduler_policy_from_flags(None, None).expect("defaults");
        assert_eq!(defaults.same_code_mode, SameCodeMode::Serial);
        assert_eq!(defaults.cross_code_mode, CrossCodeMode::Parallel);
        assert_eq!(defaults.max_active_tasks, 16);
        assert_eq!(defaults.max_queued_tasks_per_code, 16);
        assert_eq!(defaults.max_pending_admissions, 16);

        let flipped =
            scheduler_policy_from_flags(Some("parallel"), Some("serial")).expect("flipped");
        assert_eq!(flipped.same_code_mode, SameCodeMode::Parallel);
        assert_eq!(flipped.cross_code_mode, CrossCodeMode::Serial);
        assert_eq!(
            flipped,
            SchedulerPolicy {
                same_code_mode: SameCodeMode::Parallel,
                cross_code_mode: CrossCodeMode::Serial,
                ..SchedulerPolicy::default()
            }
        );

        // Unknown words are refused (clap already rejects them at parse
        // time; the mapper is the second lock for programmatic callers).
        assert!(scheduler_policy_from_flags(Some("fast"), None).is_err());
        assert!(scheduler_policy_from_flags(None, Some("lazy")).is_err());
    }

    #[tokio::test]
    async fn set_policy_pushes_the_flags_through_the_session_runtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        let registry = TestRegistry::start(tmp.path()).await;
        let default_executor = ak_session::runtime::RealTransferExecutor::receive(
            loopback_receive_spec(&tmp.path().join("unused-out"), throwaway_identity()),
            None,
            None,
        );
        let (key, _) = crate::load_identity(&dir).unwrap();
        let client = crate::invite::connect_registry(&registry.settings, &key)
            .await
            .unwrap();
        let wired = start_wired_runtime(
            client,
            &registry.settings,
            scheduler_policy_from_flags(Some("parallel"), Some("serial")).unwrap(),
            Arc::new(default_executor),
            Arc::new(JobTerminals::default()),
        )
        .await
        .expect("wired runtime starts against the real registry transport");
        let switch = wired
            .handle
            .set_policy(scheduler_policy_from_flags(Some("parallel"), Some("serial")).unwrap())
            .await
            .expect("the live set_policy round trip applies");
        assert!(switch.revision >= 1, "a switch happened: {switch:?}");
        // The wave-24 caps flow into the runtime's policy.
        let snapshot = wired
            .handle
            .snapshot(ak_session::runtime::SnapshotQuery::all())
            .await
            .unwrap();
        assert_eq!(snapshot.policy.max_active_tasks, 16);
        assert_eq!(snapshot.policy.max_queued_tasks_per_code, 16);
        assert_eq!(snapshot.policy.max_pending_admissions, 16);
        stop_wired_runtime(&wired.handle).await;
        registry.stop().await;
    }

    /// The wave-24 wired receive path through the FULL session stack: a
    /// real registry (TLS transport up), a real `RealTransferExecutor`
    /// receive armed on a loopback port, submitted through
    /// `SessionHandle::enqueue` (admission → driver → executor), fed by a
    /// real legacy `ak_engine::send`. This is the daemon's wired
    /// iteration in miniature (`--invite-code`/flags mode).
    #[tokio::test]
    async fn wired_enqueue_completes_a_real_transfer_through_the_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let registry = TestRegistry::start(tmp.path()).await;

        let default_executor = ak_session::runtime::RealTransferExecutor::receive(
            loopback_receive_spec(&tmp.path().join("default-out"), throwaway_identity()),
            None,
            None,
        );
        let (key, _) = crate::load_identity(&dir).unwrap();
        let client = crate::invite::connect_registry(&registry.settings, &key)
            .await
            .unwrap();
        let wired = start_wired_runtime(
            client,
            &registry.settings,
            scheduler_policy_from_flags(Some("parallel"), None).unwrap(),
            Arc::new(default_executor),
            Arc::new(JobTerminals::default()),
        )
        .await
        .expect("wired runtime starts");

        // The iteration's receive task: a fresh one-shot executor bound to
        // an auto-assigned loopback port with the daemon identity,
        // exactly like the daemon loop builds it.
        let (key, identity) = crate::load_identity(&dir).unwrap();
        let receiver_fp = key.fingerprint();
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let executor = Arc::new(ak_session::runtime::RealTransferExecutor::receive(
            loopback_receive_spec(&out, identity),
            Some(addr_tx),
            None,
        ));
        let group = ak_session::runtime::CodeGroupKey::new(
            ak_session::runtime::RegistryId::from_bytes(registry.settings.registry_id),
            ak_session::runtime::CodeId::from_bytes([0u8; 16]),
        );
        let task = wired
            .handle
            .enqueue(ak_session::runtime::TransferSpec {
                group_key: group,
                pairing: ak_session::runtime::PairingPlan::Local,
                executor: executor.clone() as Arc<dyn ak_session::runtime::TransferExecutor>,
                direction: ak_session::runtime::TransferDirection::Receive,
                label: "wired-loopback".to_owned(),
                task_id: None,
            })
            .await
            .expect("the receive task enters the scheduler");

        let bound = tokio::time::timeout(Duration::from_secs(10), addr_rx)
            .await
            .expect("bound in time")
            .expect("addr delivered");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("wired.bin"), &payload).unwrap();
        let sender_key = ak_identity::DeviceKeyPair::generate();
        let sender_identity = ak_identity::tls::make_self_signed(&sender_key);
        let mut send_spec = ak_engine::SendSpec::new(src, bound, receiver_fp, sender_identity);
        send_spec.idle_timeout = Duration::from_secs(20);
        let report =
            tokio::time::timeout(Duration::from_secs(30), ak_engine::send(send_spec, &|_| {}))
                .await
                .expect("send finishes")
                .expect("send succeeds");
        assert_eq!(report.files, 1);

        // The task reaches terminal Completed in the runtime and the file
        // landed byte-identical.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = wired
                .handle
                .snapshot(ak_session::runtime::SnapshotQuery::one(task))
                .await
                .unwrap();
            if let Some(view) = snapshot.tasks.first() {
                if view.state == ak_session::scheduler::TaskState::Completed {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the wired task never completed");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        match executor.outcome(&task) {
            Some(ak_session::runtime::RealOutcome::Completed { report }) => {
                assert_eq!(report.bytes, payload.len() as u64);
            }
            other => panic!("expected a completed outcome, got {other:?}"),
        }
        assert_eq!(std::fs::read(out.join("wired.bin")).unwrap(), payload);

        stop_wired_runtime(&wired.handle).await;
        registry.stop().await;
    }

    // ------------------------------------------------------------------
    // Wave 27: concurrent wired receive — real code groups, real policy
    // (final-review P1-02). The fixtures below drive the REAL wired
    // machinery: a real v2 registry, the daemon identity as the code
    // owner (its `owner_pairing_loop` answers every redeem, so the
    // daemon's pairing journal holds the Ready relations the senders'
    // `TaskOpen`s claim), the real wired session runtime with the policy
    // under test, the real admission validator, and the real per-
    // connection handler on a loopback `DirectEndpoint`. Senders are real
    // `send_on_channel` scheduler senders; concurrency is proven by task
    // activity intervals (contract §12), observed on the sender as
    // `Offered` (fires strictly after the receiver granted admission —
    // the TaskOpen handshake blocks until then) to `Done` (final ack).
    // ------------------------------------------------------------------

    /// The sender-side activity window of one scheduler task.
    #[derive(Clone, Copy, Debug)]
    struct SenderInterval {
        offered: Instant,
        done: Instant,
    }

    /// Shared slots a spawned sender's progress callback fills.
    #[derive(Clone, Default)]
    struct IntervalWatch {
        offered: Arc<Mutex<Option<Instant>>>,
        done: Arc<Mutex<Option<Instant>>>,
    }

    impl IntervalWatch {
        /// Block until the sender's `Offered` arrives (it was admitted).
        async fn wait_offered(&self, deadline: Instant) -> Instant {
            loop {
                if let Some(at) = *self.offered.lock().unwrap() {
                    return at;
                }
                assert!(
                    Instant::now() < deadline,
                    "the sender was never admitted (no Offered progress)"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// The completed interval (both ends observed).
        fn interval(&self) -> SenderInterval {
            SenderInterval {
                offered: self.offered.lock().unwrap().expect("offered observed"),
                done: self.done.lock().unwrap().expect("done observed"),
            }
        }
    }

    /// Everything one wired-concurrency test needs, on real planes.
    struct WiredScaffold {
        tmp: tempfile::TempDir,
        registry: TestRegistry,
        daemon_fp: ak_identity::Fingerprint,
        daemon_bundle: ak_identity::tls::TlsBundle,
        out_dir: PathBuf,
        handle: ak_session::runtime::SessionHandle,
        registry_id: [u8; 32],
        validator: Arc<ak_session::runtime::admission::AdmissionValidator>,
        template: WiredJobTemplate,
        /// CR-P2-15: the terminal-print registry the runtime hook and the
        /// spawned handlers share (the daemon's own wiring, in miniature).
        terminals: Arc<JobTerminals>,
    }

    impl WiredScaffold {
        /// Deterministic teardown of every resident piece.
        async fn teardown(self) {
            stop_wired_runtime(&self.handle).await;
            self.registry.stop().await;
        }
    }

    /// The pairing phase of a wired test: the registry, the daemon
    /// identity and the daemon-owned codes, with the ORCHESTRATOR owner
    /// loop answering redeems (the journal-writing path — the daemon's
    /// pairing journal is exactly what `AdmissionValidator::resolve`
    /// checks, and `crate::invite::owner_pairing_loop` only writes
    /// `peers.book`).
    ///
    /// Phase discipline (the v2 registry keeps ONE live connection per
    /// device — a newer Hello replaces the previous): this phase owns the
    /// daemon's single registry connection; [`WiredPairing::start_runtime`]
    /// releases it and only then starts the wired runtime.
    struct WiredPairing {
        tmp: tempfile::TempDir,
        registry: TestRegistry,
        daemon_dir: PathBuf,
        daemon_bundle: ak_identity::tls::TlsBundle,
        daemon_fp: ak_identity::Fingerprint,
        out_dir: PathBuf,
        codes: Vec<([u8; 16], String)>,
        _orchestrator: ak_session::pairing::PairingOrchestrator,
        _client: ak_session::pairing::RegistryClient,
        owner_loop: tokio::task::JoinHandle<()>,
    }

    /// Registry + daemon identity + `codes` fresh codes + the owner loop
    /// answering (phase 1).
    async fn wired_pairing_setup(codes: usize) -> WiredPairing {
        let _ = env_logger::builder()
            .filter_level(log::LevelFilter::Info)
            .is_test(true)
            .try_init();
        let tmp = tempfile::tempdir().expect("tempdir");
        let daemon_dir = tmp.path().join("daemon");
        std::fs::create_dir_all(&daemon_dir).unwrap();
        let out_dir = tmp.path().join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let registry = TestRegistry::start(tmp.path()).await;

        let (daemon_key, daemon_bundle) = crate::load_identity(&daemon_dir).unwrap();
        let daemon_fp = daemon_key.fingerprint();
        let client = crate::invite::connect_registry(&registry.settings, &daemon_key)
            .await
            .expect("daemon registry client");
        let orchestrator = ak_session::pairing::PairingOrchestrator::with_defaults(
            client.clone(),
            daemon_key.clone(),
            &daemon_dir,
        )
        .expect("the owner orchestrator opens");
        let mut created = Vec::new();
        for _ in 0..codes {
            let invite = orchestrator
                .create_invite(600)
                .await
                .expect("invite create");
            created.push((invite.code_id, invite.display.clone()));
        }
        let owner_loop = orchestrator.spawn_owner_loop();
        WiredPairing {
            tmp,
            registry,
            daemon_dir,
            daemon_bundle,
            daemon_fp,
            out_dir,
            codes: created,
            _orchestrator: orchestrator,
            _client: client,
            owner_loop,
        }
    }

    impl WiredPairing {
        /// Phase 2: the pairings are done — stop the owner loop, release
        /// the daemon's registry connection and start the wired runtime
        /// (its transport takes the device's single connection slot), the
        /// admission validator and the per-connection job template.
        async fn start_runtime(
            self,
            policy: ak_session::runtime::SchedulerPolicy,
        ) -> WiredScaffold {
            self.owner_loop.abort();
            let default_executor = ak_session::runtime::RealTransferExecutor::receive(
                loopback_receive_spec(&self.tmp.path().join("unused-out"), throwaway_identity()),
                None,
                None,
            );
            let (key, _) = crate::load_identity(&self.daemon_dir).unwrap();
            let client = crate::invite::connect_registry(&self.registry.settings, &key)
                .await
                .unwrap();
            let terminals = Arc::new(JobTerminals::default());
            let wired = start_wired_runtime(
                client,
                &self.registry.settings,
                policy,
                Arc::new(default_executor),
                Arc::clone(&terminals),
            )
            .await
            .expect("wired runtime starts");
            let validator = Arc::new(
                ak_session::runtime::admission::AdmissionValidator::open(
                    &self.daemon_dir,
                    ak_session::runtime::RegistryId::from_bytes(self.registry.settings.registry_id),
                )
                .expect("validator opens over the daemon journal"),
            );
            let template = WiredJobTemplate::of(
                &ReceiveLoopConfig::resolve(
                    &self.daemon_dir,
                    self.out_dir.clone(),
                    SocketAddr::from(([127, 0, 0, 1], 0)),
                    Some("any"),
                    "error",
                    false,
                    30,
                    "wired-test".to_owned(),
                )
                .expect("template config resolves"),
            );
            WiredScaffold {
                registry_id: wired.registry_id,
                handle: wired.handle,
                tmp: self.tmp,
                registry: self.registry,
                daemon_fp: self.daemon_fp,
                daemon_bundle: self.daemon_bundle,
                out_dir: self.out_dir,
                validator,
                template,
                terminals,
            }
        }
    }

    /// One payload directory with a single deterministic file of `size`
    /// bytes (the engine sends directories).
    fn payload_dir(root: &Path, name: &str, size: usize) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join(format!("{name}.bin")), &payload).unwrap();
        dir
    }

    /// Redeem one invite code from a fresh sender identity, returning the
    /// sender's directory and the pairing id of its Ready relation.
    async fn redeem_sender(
        root: &Path,
        settings: &crate::invite::RegistrySettings,
        name: &str,
        code_display: &str,
    ) -> (PathBuf, [u8; 16]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let verified = crate::invite::redeem_and_pair(&dir, code_display, settings)
            .await
            .expect("sender redeems the code");
        (dir, verified.pairing_id())
    }

    /// The wired accept loop in miniature: accept `connections`
    /// connections, one real handler task each (the same
    /// `wired_connection_handler` the daemon spawns).
    fn spawn_wired_acceptor(
        endpoint: ak_net::DirectEndpoint,
        handle: ak_session::runtime::SessionHandle,
        validator: Arc<ak_session::runtime::admission::AdmissionValidator>,
        template: WiredJobTemplate,
        registry_id: [u8; 32],
        terminals: Arc<JobTerminals>,
        connections: usize,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // The shutdown flag lives as long as the acceptor; every
            // handler holds a receiver clone.
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            for _ in 0..connections {
                if let Ok(channel) = endpoint.accept(&|_: &ak_identity::Fingerprint| true).await {
                    tokio::spawn(wired_connection_handler(WiredConnection {
                        handle: handle.clone(),
                        validator: Arc::clone(&validator),
                        template: template.clone(),
                        out: DaemonOut::new(false),
                        registry_id,
                        shutdown: shutdown_rx.clone(),
                        channel,
                        terminals: Arc::clone(&terminals),
                    }));
                }
            }
            // The endpoint and the shutdown flag must outlive every
            // accepted connection (the real daemon keeps both for its
            // whole life — dropping the watch sender would fire every
            // handler's shutdown arm and close its channel): park until
            // the test runtime goes away.
            let _keep_alive = (shutdown_tx, endpoint);
            std::future::pending::<()>().await;
        })
    }

    /// A real scheduler sender: fresh identity, connect, `TaskOpen`
    /// carrying the relation's code group (frozen 32-byte wire form) and
    /// pairing id, then the real on-channel send. `Offered`/`Done`
    /// progress lands in the returned watch (the admitted interval).
    #[allow(clippy::too_many_arguments)]
    fn spawn_scheduler_sender(
        settings: &crate::invite::RegistrySettings,
        daemon_addr: SocketAddr,
        daemon_fp: &ak_identity::Fingerprint,
        sender_dir: &Path,
        src: &Path,
        code_id: [u8; 16],
        pairing_id: [u8; 16],
    ) -> (
        tokio::task::JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>>,
        IntervalWatch,
    ) {
        let watch = IntervalWatch::default();
        let progress_watch = watch.clone();
        let settings = *settings;
        let daemon_fp = *daemon_fp;
        let sender_dir = sender_dir.to_path_buf();
        let src = src.to_path_buf();
        let join = tokio::spawn(async move {
            let (key, _) = crate::load_identity(&sender_dir).unwrap();
            let identity = ak_identity::tls::make_self_signed(&key);
            let endpoint =
                ak_net::DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
                    .await
                    .unwrap();
            let channel = endpoint
                .connect(daemon_addr, &daemon_fp)
                .await
                .expect("the scheduler sender connects");
            // Distinct non-zero task/permit ids derived from the relation
            // (two senders of one code must not collide on the wire ids).
            let mut job_id = pairing_id;
            job_id[0] ^= 0x5a;
            let mut permit_id = pairing_id;
            permit_id[1] ^= 0xa5;
            let ctx = ak_engine::AdmissionCtx::new(
                job_id,
                permit_id,
                ak_session::runtime::admission::code_group_wire_form(&code_id),
                settings.registry_id,
                pairing_id,
            )
            .expect("a structurally valid admission context");
            let mut spec = ak_engine::SendSpec::new(src, daemon_addr, daemon_fp, identity);
            spec.idle_timeout = Duration::from_secs(30);
            ak_engine::send_on_channel(channel, spec, ctx, &move |progress| match progress {
                ak_engine::Progress::Offered { .. } => {
                    *progress_watch.offered.lock().unwrap() = Some(Instant::now());
                }
                ak_engine::Progress::Done => {
                    *progress_watch.done.lock().unwrap() = Some(Instant::now());
                }
                _ => {}
            })
            .await
        });
        (join, watch)
    }

    /// Bind the scaffold's daemon endpoint and start the acceptor for
    /// `connections` handlers; returns the dialable address.
    async fn wired_listener(
        scaffold: &WiredScaffold,
        connections: usize,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let endpoint = ak_net::DirectEndpoint::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            &scaffold.daemon_bundle,
        )
        .await
        .expect("the daemon endpoint binds");
        let addr = endpoint.local_addr().unwrap();
        let acceptor = spawn_wired_acceptor(
            endpoint,
            scaffold.handle.clone(),
            Arc::clone(&scaffold.validator),
            scaffold.template.clone(),
            scaffold.registry_id,
            Arc::clone(&scaffold.terminals),
            connections,
        );
        (addr, acceptor)
    }

    /// Await one spawned sender to a successful report (bounded).
    async fn await_sender(
        join: tokio::task::JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>>,
        label: &str,
    ) -> ak_engine::Report {
        tokio::time::timeout(Duration::from_secs(60), join)
            .await
            .expect("the send finishes")
            .expect("the sender task joins")
            .unwrap_or_else(|err| panic!("{label} send failed: {err:?}"))
    }

    /// Default policy (same-code serial, cross-code parallel): two
    /// scheduler senders of ONE code; the second is only admitted after
    /// the first finished — their admitted intervals never overlap
    /// (contract §2/§12, the P1-02 core case).
    #[tokio::test]
    async fn wired_same_code_tasks_never_overlap_by_default() {
        let pairing = wired_pairing_setup(1).await;
        let (code_id, code_display) = pairing.codes[0].clone();
        let (dir_a, pair_a) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-a",
            &code_display,
        )
        .await;
        let (dir_b, pair_b) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-b",
            &code_display,
        )
        .await;
        let scaffold = pairing
            .start_runtime(scheduler_policy_from_flags(None, None).unwrap())
            .await;

        let (addr, _acceptor) = wired_listener(&scaffold, 2).await;
        let src_a = payload_dir(scaffold.tmp.path(), "src-a", 6 * 1024 * 1024);
        let src_b = payload_dir(scaffold.tmp.path(), "src-b", 1024 * 1024);
        let (join_a, watch_a) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_a,
            &src_a,
            code_id,
            pair_a,
        );
        // The first task is admitted and transferring before the second
        // even connects (contract §12: the interval evidence must come
        // from real sequencing, not accidental starts).
        watch_a
            .wait_offered(Instant::now() + Duration::from_secs(30))
            .await;
        let (join_b, watch_b) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_b,
            &src_b,
            code_id,
            pair_b,
        );
        let report_a = await_sender(join_a, "first").await;
        let report_b = await_sender(join_b, "second").await;
        assert_eq!(report_a.files, 1);
        assert_eq!(report_b.files, 1);

        let first = watch_a.interval();
        let second = watch_b.interval();
        assert!(
            second.offered >= first.done,
            "same-code tasks overlapped under the default serial policy: \
             first {first:?}, second {second:?}"
        );
        assert!(
            scaffold.out_dir.join("src-a.bin").exists()
                && scaffold.out_dir.join("src-b.bin").exists(),
            "both payloads must land"
        );
        scaffold.teardown().await;
    }

    /// Default policy: two senders of TWO DIFFERENT codes run
    /// concurrently — the second is admitted while the first still
    /// transfers (overlapping intervals).
    #[tokio::test]
    async fn wired_cross_code_tasks_overlap_by_default() {
        let pairing = wired_pairing_setup(2).await;
        let (code_a, display_a) = pairing.codes[0].clone();
        let (code_b, display_b) = pairing.codes[1].clone();
        let (dir_a, pair_a) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-a",
            &display_a,
        )
        .await;
        let (dir_b, pair_b) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-b",
            &display_b,
        )
        .await;
        let scaffold = pairing
            .start_runtime(scheduler_policy_from_flags(None, None).unwrap())
            .await;

        let (addr, _acceptor) = wired_listener(&scaffold, 2).await;
        let src_a = payload_dir(scaffold.tmp.path(), "src-a", 16 * 1024 * 1024);
        let src_b = payload_dir(scaffold.tmp.path(), "src-b", 256 * 1024);
        let (join_a, watch_a) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_a,
            &src_a,
            code_a,
            pair_a,
        );
        watch_a
            .wait_offered(Instant::now() + Duration::from_secs(30))
            .await;
        let (join_b, watch_b) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_b,
            &src_b,
            code_b,
            pair_b,
        );
        let report_a = await_sender(join_a, "first-code").await;
        let report_b = await_sender(join_b, "second-code").await;
        assert_eq!(report_a.files, 1);
        assert_eq!(report_b.files, 1);

        let first = watch_a.interval();
        let second = watch_b.interval();
        assert!(
            second.offered < first.done,
            "cross-code tasks did not overlap under the default parallel \
             policy: first {first:?}, second {second:?}"
        );
        assert!(
            scaffold.out_dir.join("src-a.bin").exists()
                && scaffold.out_dir.join("src-b.bin").exists(),
            "both payloads must land"
        );
        scaffold.teardown().await;
    }

    /// The `--same-code parallel` flag reaches the REAL group keys: the
    /// same one-code setup as the serial test, but the policy admits both
    /// at once (overlapping intervals) — the four-mode matrix on the real
    /// path, not just in the scheduler model.
    #[tokio::test]
    async fn wired_same_code_parallel_flag_overlaps_real_groups() {
        let pairing = wired_pairing_setup(1).await;
        let (code_id, code_display) = pairing.codes[0].clone();
        let (dir_a, pair_a) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-a",
            &code_display,
        )
        .await;
        let (dir_b, pair_b) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-b",
            &code_display,
        )
        .await;
        let scaffold = pairing
            .start_runtime(scheduler_policy_from_flags(Some("parallel"), None).unwrap())
            .await;

        let (addr, _acceptor) = wired_listener(&scaffold, 2).await;
        let src_a = payload_dir(scaffold.tmp.path(), "src-a", 16 * 1024 * 1024);
        let src_b = payload_dir(scaffold.tmp.path(), "src-b", 256 * 1024);
        let (join_a, watch_a) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_a,
            &src_a,
            code_id,
            pair_a,
        );
        watch_a
            .wait_offered(Instant::now() + Duration::from_secs(30))
            .await;
        let (join_b, watch_b) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &dir_b,
            &src_b,
            code_id,
            pair_b,
        );
        let report_a = await_sender(join_a, "parallel-first").await;
        let report_b = await_sender(join_b, "parallel-second").await;
        assert_eq!(report_a.files, 1);
        assert_eq!(report_b.files, 1);

        let first = watch_a.interval();
        let second = watch_b.interval();
        assert!(
            second.offered < first.done,
            "--same-code parallel did not overlap the real code group: \
             first {first:?}, second {second:?}"
        );
        scaffold.teardown().await;
    }

    /// A scheduler sender whose `TaskOpen` claims a relation the daemon
    /// never stored is refused (contract §6: no self-reported `code_id`
    /// into a queue): its connection dies, and the daemon machinery
    /// serves the very next (legitimate) sender unchanged.
    #[tokio::test]
    async fn wired_admission_rejection_closes_the_connection_and_daemon_survives() {
        let pairing = wired_pairing_setup(1).await;
        let (code_id, code_display) = pairing.codes[0].clone();
        let (good_dir, good_pair) = redeem_sender(
            pairing.tmp.path(),
            &pairing.registry.settings,
            "sender-good",
            &code_display,
        )
        .await;
        let scaffold = pairing
            .start_runtime(scheduler_policy_from_flags(None, None).unwrap())
            .await;

        let (addr, _acceptor) = wired_listener(&scaffold, 2).await;
        // The attacker: a well-formed TaskOpen of the SAME code group but
        // a pairing id no local relation backs.
        let bad_dir = scaffold.tmp.path().join("sender-bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        let src_bad = payload_dir(scaffold.tmp.path(), "src-bad", 64 * 1024);
        let (bad_join, _bad_watch) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &bad_dir,
            &src_bad,
            code_id,
            [0x99; 16],
        );
        let bad = tokio::time::timeout(Duration::from_secs(30), bad_join)
            .await
            .expect("the refused sender settles")
            .expect("join")
            .expect_err("the unbacked claim must be refused");
        assert!(
            !scaffold.out_dir.join("src-bad.bin").exists(),
            "nothing may land from the refused task (error {bad:?})"
        );

        // The daemon lives: the legitimate relation completes through the
        // same handler machinery.
        let src_good = payload_dir(scaffold.tmp.path(), "src-good", 256 * 1024);
        let (good_join, _good_watch) = spawn_scheduler_sender(
            &scaffold.registry.settings,
            addr,
            &scaffold.daemon_fp,
            &good_dir,
            &src_good,
            code_id,
            good_pair,
        );
        let report = await_sender(good_join, "good-after-reject").await;
        assert_eq!(report.files, 1);
        assert!(
            scaffold.out_dir.join("src-good.bin").exists(),
            "the legitimate payload lands after the rejection"
        );
        scaffold.teardown().await;
    }

    /// A pure LEGACY peer (no `CAP_SCHEDULER`, no `TaskOpen` — the frozen
    /// `send` kernel, i.e. the pre-wave-27 `send --fp`) completes one
    /// receive through the wired daemon's compatibility path (the
    /// anonymous legacy group + the frozen single-job body on the same
    /// channel).
    #[tokio::test]
    async fn wired_legacy_peer_still_completes_one_receive() {
        let pairing = wired_pairing_setup(0).await;
        let scaffold = pairing
            .start_runtime(scheduler_policy_from_flags(None, None).unwrap())
            .await;
        let (addr, _acceptor) = wired_listener(&scaffold, 1).await;

        let sender_dir = scaffold.tmp.path().join("legacy-sender");
        std::fs::create_dir_all(&sender_dir).unwrap();
        let (_, identity) = crate::load_identity(&sender_dir).unwrap();
        let src = payload_dir(scaffold.tmp.path(), "src-legacy", 512 * 1024);
        let mut spec = ak_engine::SendSpec::new(src, addr, scaffold.daemon_fp, identity);
        spec.idle_timeout = Duration::from_secs(30);
        let report = tokio::time::timeout(Duration::from_secs(60), ak_engine::send(spec, &|_| {}))
            .await
            .expect("the legacy send settles")
            .expect("the legacy send succeeds");
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, 512 * 1024_u64);
        assert!(
            scaffold.out_dir.join("src-legacy.bin").exists(),
            "the legacy payload lands through the wired compatibility path"
        );
        scaffold.teardown().await;
    }

    /// A real v2 test registry (TLS + SQLite) shared by the wired tests.
    struct TestRegistry {
        settings: crate::invite::RegistrySettings,
        stop_tx: tokio::sync::watch::Sender<bool>,
        join: tokio::task::JoinHandle<()>,
    }

    impl TestRegistry {
        async fn start(root: &Path) -> Self {
            use ak_signal::server_v2::{engine::serve_v2, store::RegistryStore, Limits};
            let key = ak_identity::DeviceKeyPair::generate();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let store = RegistryStore::open(&root.join("registry.db")).unwrap();
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let server_key = key.clone();
            let join = tokio::spawn(async move {
                let _ = serve_v2(listener, &server_key, store, Limits::default(), stop_rx).await;
            });
            TestRegistry {
                settings: crate::invite::RegistrySettings {
                    server: addr,
                    registry_id: key.public_key(),
                },
                stop_tx,
                join,
            }
        }

        async fn stop(self) {
            let _ = self.stop_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(5), self.join).await;
        }
    }

    /// A loopback receive spec on an auto-assigned port (accept anyone),
    /// presenting `identity` so a test sender can pin its fingerprint.
    fn loopback_receive_spec(
        out: &Path,
        identity: ak_identity::tls::TlsBundle,
    ) -> ak_engine::ReceiveSpec {
        let mut spec = ak_engine::ReceiveSpec::new(out.to_path_buf(), identity);
        spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        spec.idle_timeout = Duration::from_secs(20);
        spec.authorize = Box::new(|_| true);
        spec
    }

    /// A throwaway TLS identity for the never-used default-executor slot.
    fn throwaway_identity() -> ak_identity::tls::TlsBundle {
        ak_identity::tls::make_self_signed(&ak_identity::DeviceKeyPair::generate())
    }

    /// The full wave-21 journey on one machine, all real planes: v2
    /// registry (TLS + SQLite), `invite create`, the owner pairing answer
    /// (`--pairing` core), `daemon --invite-code` (redeem at startup +
    /// resident receive), and one real engine send into the daemon —
    /// both `peers.book` files end up holding the other side.
    #[tokio::test]
    async fn invite_pairing_daemon_smoke_one_send_one_receive() {
        use ak_signal::server_v2::{engine::serve_v2, store::RegistryStore, Limits};
        use tokio::net::TcpListener;

        let _ = env_logger::builder()
            .filter_level(log::LevelFilter::Info)
            .is_test(true)
            .try_init();

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_a = tmp.path().join("owner");
        let dir_b = tmp.path().join("redeemer");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        // 1. Real v2 registry over loopback TLS + SQLite.
        let registry_key = ak_identity::DeviceKeyPair::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let registry_addr = listener.local_addr().unwrap();
        let store = RegistryStore::open(&tmp.path().join("registry.db")).unwrap();
        let (registry_stop, registry_stop_rx) = tokio::sync::watch::channel(false);
        let server_key = registry_key.clone();
        tokio::spawn(async move {
            let _ = serve_v2(
                listener,
                &server_key,
                store,
                Limits::default(),
                registry_stop_rx,
            )
            .await;
        });
        let settings = crate::invite::RegistrySettings {
            server: registry_addr,
            registry_id: registry_key.public_key(),
        };

        // 2. `invite create` on the owner side — both codes of this
        //    journey up front, created through the ORCHESTRATOR (wave 27):
        //    its resident owner loop answers redeems through the
        //    journal-writing path, so the owner side ends up holding the
        //    Ready pairing relation the later scheduler send claims (the
        //    `invite.rs` answer loop only writes peers.book). The bearer
        //    secrets persist exactly like the CLI wrapper does.
        let registry_fp_hex: String = registry_key
            .public_key()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let (key_a, _) = crate::load_identity(&dir_a).unwrap();
        let client_a = crate::invite::connect_registry(&settings, &key_a)
            .await
            .expect("owner registry client");
        let orchestrator_a = ak_session::pairing::PairingOrchestrator::with_defaults(
            client_a.clone(),
            key_a.clone(),
            &dir_a,
        )
        .expect("the owner orchestrator opens");
        let invite_store = crate::invite::InviteStore::open(&dir_a);
        let mut invites = Vec::new();
        for _ in 0..2 {
            let created = orchestrator_a
                .create_invite(600)
                .await
                .expect("invite create");
            invite_store
                .add(&crate::invite::StoredInvite {
                    code_id: created.code_id,
                    expires_ms: created.expires_ms,
                    revision: created.revision,
                    display: created.display.clone(),
                })
                .expect("the bearer secret persists");
            invites.push(created);
        }
        let first = &invites[0];
        let second = &invites[1];
        assert!(first.display.starts_with("AKI1-"), "{}", first.display);
        assert_eq!(
            invite_store.load().unwrap().len(),
            2,
            "the bearer secrets persist for the owner loop"
        );

        // 3. Owner pairing answer online (the orchestrator's resident
        // owner loop; the registry keeps one live connection per device,
        // so this client is the owner's single connection).
        let owner_loop = orchestrator_a.spawn_owner_loop();
        // The owner's registry pin (what the scheduler send's admission
        // context reads its `group_registry` from).
        crate::invite::resolve_registry(&dir_a, Some(registry_addr), Some(registry_fp_hex.clone()))
            .expect("persist the owner's registry pin");

        // 4. Redeem on the other side: peers.book records both directions.
        let verified = crate::invite::redeem_and_pair(&dir_b, &first.display, &settings)
            .await
            .expect("redeem pairs with the owner");
        let fp_a = key_a.fingerprint();
        let (key_b, _) = crate::load_identity(&dir_b).unwrap();
        let fp_b = key_b.fingerprint();
        assert_eq!(verified.owner_fingerprint(), fp_a);
        let book_a = std::fs::read_to_string(crate::peer_book_path(&dir_a)).unwrap();
        let book_b = std::fs::read_to_string(crate::peer_book_path(&dir_b)).unwrap();
        assert!(
            book_a.contains(&fp_b.to_hex()),
            "owner book records redeemer: {book_a}"
        );
        assert!(
            book_b.contains(&fp_a.to_hex()),
            "redeemer book records owner: {book_b}"
        );
        let first_relation = crate::ready_relation_for(&dir_a, &fp_b)
            .expect("relation lookup")
            .expect("the owner observed the pairing (journal Applied)");
        assert_eq!(
            first_relation.code_id, first.code_id,
            "the first relation belongs to the first code"
        );

        // 5. `daemon --invite-code` on a SECOND code of the same owner:
        // redeem at startup, then resident receive.
        let out_b = tmp.path().join("out-b");
        std::fs::create_dir_all(&out_b).unwrap();
        let daemon_port = pick_free_port().unwrap();
        let daemon_opts = DaemonOptions {
            out: out_b.clone(),
            port: daemon_port,
            expect: None,
            conflict: "error".into(),
            timeout: 30,
            name: Some("smoke-redeemer".into()),
            json_lines: true,
            invite_code: Some(second.display.clone()),
            pairing: false,
            same_code: Some("parallel".into()),
            cross_code: Some("serial".into()),
            server: Some(registry_addr),
            registry_fp: Some(registry_fp_hex.clone()),
        };
        let (daemon_stop, daemon_stop_rx) = tokio::sync::watch::channel(false);
        let daemon_dir = dir_b.clone();
        let daemon =
            tokio::spawn(
                async move { daemon_core(&daemon_dir, daemon_opts, daemon_stop_rx).await },
            );

        // 6. Wait for the second pairing (the owner's journal reaches
        // Applied for the second code), then send one real file into the
        // daemon's listener.
        let deadline = Instant::now() + Duration::from_secs(30);
        let relation = loop {
            if let Ok(Some(relation)) = crate::ready_relation_for(&dir_a, &fp_b) {
                if relation.code_id == second.code_id {
                    break relation;
                }
            }
            assert!(
                Instant::now() < deadline,
                "second pairing never completed (daemon finished early: {})",
                daemon.is_finished()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        tokio::time::sleep(Duration::from_millis(500)).await; // bind settles

        // Wave 27: the send rides the SCHEDULER admission path (the
        // equivalent migration of the legacy one-shot send): the owner
        // side holds a Ready relation for the daemon from the second
        // redeem, so the real `send_on_channel` TaskOpen reaches the
        // daemon's new accept loop, gets validated against the daemon's
        // journal and runs under the real code group.

        let src = tmp.path().join("smoke-src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..1_500_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(src.join("journey.bin"), &payload).unwrap();

        let report = loop {
            let (join, _watch) = spawn_scheduler_sender(
                &settings,
                SocketAddr::from(([127, 0, 0, 1], daemon_port)),
                &fp_b,
                &dir_a,
                &src,
                relation.code_id,
                relation.pairing_id,
            );
            match tokio::time::timeout(Duration::from_secs(30), join)
                .await
                .expect("the sender joins")
            {
                Ok(Ok(report)) => break report,
                Ok(Err(ak_engine::EngineError::Connect(_))) => {
                    // The daemon is still redeeming/binding; retry briefly.
                    assert!(
                        Instant::now() < deadline,
                        "daemon never became reachable for the send"
                    );
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                Ok(Err(other)) => panic!("scheduler send failed: {other:?}"),
                Err(join) => panic!("sender task failed: {join}"),
            }
        };
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, payload.len() as u64);
        assert_eq!(
            std::fs::read(out_b.join("journey.bin")).unwrap(),
            payload,
            "一发一收: the daemon received byte-identical content"
        );

        // 7. Deterministic shutdown of every resident piece.
        let _ = daemon_stop.send(true);
        daemon.await.unwrap().expect("daemon exits cleanly");
        owner_loop.abort();
        let _ = registry_stop.send(true);
    }
}
