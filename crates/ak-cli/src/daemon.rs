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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonEvent<'a> {
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
/// [`RealTransport`] over the pinned v2 registry client, the real
/// [`RealTransferExecutor`] receive kernel as the inbound executor, and
/// the initial [`SchedulerPolicy`] from the flags. The daemon's own
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
async fn start_wired_runtime(
    id: &Path,
    settings: &crate::invite::RegistrySettings,
    policy: ak_session::runtime::SchedulerPolicy,
    default_executor: Arc<ak_session::runtime::RealTransferExecutor>,
) -> Result<WiredRuntime> {
    use ak_session::runtime::{
        RealClock, RetryPolicy, RuntimeClock, RuntimeConfig, SessionRuntime, TransferExecutor,
    };
    let (key, _) = crate::load_identity(id)?;
    let client = crate::invite::connect_registry(settings, &key).await?;
    let transport = ak_session::pairing::RealTransport::new(client);
    let clock: Arc<dyn RuntimeClock> = Arc::new(RealClock::new());
    let mut config = RuntimeConfig::new(
        policy,
        clock,
        Arc::new(transport) as Arc<dyn ak_session::runtime::PairingTransport>,
        default_executor as Arc<dyn TransferExecutor>,
        ak_session::runtime::RegistryId::from_bytes(settings.registry_id),
    );
    // The daemon loop is the resident receive path; the runtime listener
    // would submit competing receive tasks on the same port.
    config.listener_enabled = false;
    // One attempt per enqueued receive: the loop itself rebinds.
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

/// Best-effort session shutdown at daemon exit: cancel in-flight work,
/// join the actor (bounded by the handle's own join timeout, plus an
/// outer bound so a wedged engine abort cannot delay the exit beyond a
/// second here).
async fn stop_wired_runtime(handle: &ak_session::runtime::SessionHandle) {
    use ak_session::runtime::ShutdownMode;
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        handle.shutdown(ShutdownMode::Cancel),
    )
    .await;
}

/// How one receive iteration failed (mirrors the direct loop's error
/// matching so both paths share the response arms).
enum JobFailure {
    /// A peer outside the accept policy was rejected.
    Unauthorized,
    /// The bind lost a port race (the loop retries with backoff).
    AddrInUse,
    /// Anything else; `message` is bounded sanitized text.
    Other { message: String },
}

/// One iteration of the wired receive loop: enqueue a fresh real receive
/// task into the session runtime and watch its lifecycle on the event
/// stream. Returns the submit result (the task id) plus the armed
/// executor to read the outcome from once the task settles.
async fn enqueue_wired_receive(
    wired: &WiredRuntime,
    spec: ak_engine::ReceiveSpec,
    printer: Arc<dyn Fn(ak_engine::Progress) + Send + Sync>,
    addr_tx: tokio::sync::oneshot::Sender<SocketAddr>,
) -> (
    Result<ak_session::runtime::TransferId, String>,
    Arc<ak_session::runtime::RealTransferExecutor>,
) {
    use ak_session::runtime::real_executor::RealTransferExecutor;
    use ak_session::runtime::{
        CodeGroupKey, CodeId, PairingPlan, RegistryId, TransferDirection, TransferExecutor,
        TransferSpec,
    };
    let executor = Arc::new(RealTransferExecutor::receive(
        spec,
        Some(addr_tx),
        Some(printer),
    ));
    // The resident listener's anonymous group: every iteration shares the
    // pinned registry identity; the all-zero code id is the local sentinel
    // for "the loop's own group" (it never reaches the wire on the Local
    // plan, and it keeps every iteration inside one code group so the
    // same-code policy governs the whole loop as one code).
    let group = CodeGroupKey::new(
        RegistryId::from_bytes(wired.registry_id),
        CodeId::from_bytes([0u8; 16]),
    );
    let submit = wired.handle.enqueue(TransferSpec {
        group_key: group,
        pairing: PairingPlan::Local,
        executor: executor.clone() as Arc<dyn TransferExecutor>,
        direction: TransferDirection::Receive,
        label: "daemon-receive".to_owned(),
        task_id: None,
    });
    let submitted = submit
        .await
        .map_err(|err| format!("cannot enqueue the receive task: {err}"));
    (submitted, executor)
}

/// Classify a settled wired receive from the executor's outcome record.
fn classify_wired_outcome(
    executor: &ak_session::runtime::RealTransferExecutor,
    task: &ak_session::runtime::TransferId,
) -> Result<ak_engine::Report, JobFailure> {
    use ak_session::runtime::real_executor::RealOutcome;
    match executor.outcome(task) {
        Some(RealOutcome::Completed { report }) => Ok(report),
        Some(RealOutcome::Failed(failure)) => Err(classify_real_failure(&failure)),
        None => Err(JobFailure::Other {
            message: "the receive task ended without an engine outcome".to_owned(),
        }),
    }
}

/// Map the executor's stable failure classification onto the loop's
/// failure kinds (the same distinctions the direct path's `EngineError`
/// match makes).
fn classify_real_failure(failure: &ak_session::runtime::RealFailure) -> JobFailure {
    use ak_core::error::ErrorCode;
    match failure.code {
        ErrorCode::AuthFailed => JobFailure::Unauthorized,
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
/// the single allowed failure parks — or cancelled), then classify.
async fn watch_wired_task(
    events: &mut tokio::sync::broadcast::Receiver<ak_session::runtime::SessionEvent>,
    executor: &ak_session::runtime::RealTransferExecutor,
    task: ak_session::runtime::TransferId,
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
    classify_wired_outcome(executor, &task)
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

    // Wave 21: owner-side pairing service (`--pairing`). Resident: spawned,
    // stopped by the shared shutdown flag.
    if opts.pairing {
        let settings = match registry {
            Some(settings) => settings,
            None => crate::invite::resolve_registry(id, opts.server, opts.registry_fp.clone())?,
        };
        let (key, _) = crate::load_identity(id)?;
        let client = crate::invite::connect_registry(&settings, &key).await?;
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
        tokio::spawn(crate::invite::owner_pairing_loop(
            client,
            key,
            id_dir,
            shutdown.clone(),
            on_paired,
        ));
    }

    // Wave 21: `--invite-code`: redeem at startup, pair with the owner,
    // then keep receiving (the owner lands in peers.book, so the book
    // policy below admits it). In wired mode the registry settings were
    // resolved once above.
    if let Some(code) = &opts.invite_code {
        let settings = registry
            .as_ref()
            .expect("invite-code implies the wired registry settings");
        let verified = crate::invite::redeem_and_pair(id, code, settings).await?;
        let peer = verified.owner_fingerprint(); // the code owner is the peer
        out.note("paired with the code owner:");
        out.note(&crate::fingerprint_lines("owner", &peer));
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
    // the resident receive path.
    let wired_runtime = if wired_mode {
        let settings = registry.as_ref().expect("checked above");
        let policy =
            scheduler_policy_from_flags(opts.same_code.as_deref(), opts.cross_code.as_deref())?;
        let default_executor = Arc::new(ak_session::runtime::RealTransferExecutor::receive(
            config.iteration_spec(),
            None,
            None,
        ));
        let wired = start_wired_runtime(id, settings, policy, default_executor).await?;
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

    // The receive kernel's rebind retry budget (the previous iteration's
    // QUIC endpoint releases its UDP socket asynchronously after drop).
    const MAX_REBIND_RETRIES: u32 = 50;
    let mut rebind_retries = 0u32;
    let mut first = true;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let spec = config.iteration_spec();
        let connected_at = Arc::new(Mutex::new(None::<Instant>));
        let printer = daemon_progress_printer(out.json_lines, connected_at.clone());
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();

        // Wave 24 wired branch: the iteration runs inside the session
        // runtime (enqueue → admission → real executor → engine).
        if let Some(wired) = wired_runtime.as_ref() {
            // Subscribe BEFORE the submit: a broadcast receiver only sees
            // messages sent after it existed, and a lightning-fast task
            // must not be able to finish before the watcher is armed.
            let mut events = wired.handle.subscribe();
            let (submitted, executor) = enqueue_wired_receive(wired, spec, printer, addr_tx).await;
            let task = match submitted {
                Ok(task) => task,
                Err(message) => {
                    out.event(&DaemonEvent::Error { message: &message });
                    out.text(&format!("{message}; still listening (Ctrl-C stops)"));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
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
                let _ = addr_rx.await;
            }
            let watcher = watch_wired_task(&mut events, &executor, task);
            tokio::pin!(watcher);
            let outcome = tokio::select! {
                result = &mut watcher => result,
                _ = shutdown.changed() => {
                    if let Some(discovery) = mdns.as_mut() {
                        discovery.stop_advertise();
                    }
                    out.event(&DaemonEvent::Stopped { reason: "shutdown" });
                    out.text("daemon stopped (mDNS announcement withdrawn; partial files stay resumable)");
                    stop_wired_runtime(&wired.handle).await;
                    return Ok(());
                }
            };
            match outcome {
                Ok(report) => {
                    rebind_retries = 0;
                    emit_job_done(&out, &connected_at, &report);
                    out.text("waiting for next sender (Ctrl-C stops)...");
                }
                Err(JobFailure::Unauthorized) => {
                    out.event(&DaemonEvent::Error {
                        message: "rejected a peer outside the accept policy",
                    });
                    out.text(
                        "rejected a peer outside the accept policy; still waiting (Ctrl-C stops)",
                    );
                }
                Err(JobFailure::AddrInUse) if rebind_retries < MAX_REBIND_RETRIES => {
                    rebind_retries += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(failure) => {
                    let message = match &failure {
                        JobFailure::AddrInUse => {
                            "bind failed: address already in use (retry budget exhausted)"
                                .to_owned()
                        }
                        JobFailure::Other { message } => message.clone(),
                        JobFailure::Unauthorized => {
                            "rejected a peer outside the accept policy".to_owned()
                        }
                    };
                    out.event(&DaemonEvent::Error { message: &message });
                    out.text(&format!(
                        "job failed: {message}; still listening (Ctrl-C stops)"
                    ));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            continue;
        }

        // Direct branch (no registry/scheduler flags): the engine loop as
        // it always ran.
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
                emit_job_done(&out, &connected_at, &report);
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
    if let Some(wired) = &wired_runtime {
        stop_wired_runtime(&wired.handle).await;
    }
    Ok(())
}

/// Emit the job-done NDJSON event / text summary shared by both loop
/// branches. Duration spans `job_started` (handshake) to completion; if
/// no `Connected` event was seen, the engine's own report duration is
/// used.
fn emit_job_done(out: &DaemonOut, connected_at: &Arc<Mutex<Option<Instant>>>, report: &Report) {
    if out.json_lines {
        let started = connected_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let duration_ms = started
            .map(|at| at.elapsed().as_millis())
            .unwrap_or_else(|| report.duration.as_millis());
        out.event(&DaemonEvent::JobDone {
            id: &job_id_hex(report),
            files: report.files,
            bytes: report.bytes,
            duration_ms: u64::try_from(duration_ms).unwrap_or(u64::MAX),
            resumed: report.resumed_chunks,
            peer: report.peer_fingerprint.as_deref(),
        });
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
        let wired = start_wired_runtime(
            &dir,
            &registry.settings,
            scheduler_policy_from_flags(Some("parallel"), Some("serial")).unwrap(),
            Arc::new(default_executor),
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
        let wired = start_wired_runtime(
            &dir,
            &registry.settings,
            scheduler_policy_from_flags(Some("parallel"), None).unwrap(),
            Arc::new(default_executor),
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

        // 2. `invite create` on the owner side.
        let first =
            crate::invite::invite_create(&dir_a, crate::invite::TtlChoice::Secs(600), &settings)
                .await
                .expect("invite create");
        assert!(first.display.starts_with("AKI1-"), "{}", first.display);
        assert_eq!(
            crate::invite::InviteStore::open(&dir_a)
                .load()
                .unwrap()
                .len(),
            1,
            "the bearer secret persists for the owner loop"
        );

        // 3. Owner pairing answer online (`daemon --pairing` core).
        let (key_a, _) = crate::load_identity(&dir_a).unwrap();
        let client_a = crate::invite::connect_registry(&settings, &key_a)
            .await
            .expect("owner registry client");
        let (owner_stop, owner_stop_rx) = tokio::sync::watch::channel(false);
        let paired: Arc<Mutex<Vec<ak_identity::Fingerprint>>> = Arc::new(Mutex::new(Vec::new()));
        let paired_seen = Arc::clone(&paired);
        let on_paired = std::sync::Arc::new(move |peer: ak_identity::Fingerprint| {
            paired_seen.lock().unwrap().push(peer);
        });
        let owner_dir = dir_a.clone();
        let owner_loop = tokio::spawn(crate::invite::owner_pairing_loop(
            client_a,
            key_a.clone(),
            owner_dir,
            owner_stop_rx,
            on_paired,
        ));

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
        assert_eq!(
            *paired.lock().unwrap(),
            vec![fp_b],
            "owner observed the pairing"
        );

        // 5. `daemon --invite-code` on a SECOND code of the same owner:
        // redeem at startup, then resident receive.
        let second =
            crate::invite::invite_create(&dir_a, crate::invite::TtlChoice::Secs(600), &settings)
                .await
                .expect("second invite");
        let out_b = tmp.path().join("out-b");
        std::fs::create_dir_all(&out_b).unwrap();
        let daemon_port = pick_free_port().unwrap();
        let registry_fp_hex: String = registry_key
            .public_key()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
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
            registry_fp: Some(registry_fp_hex),
        };
        let (daemon_stop, daemon_stop_rx) = tokio::sync::watch::channel(false);
        let daemon_dir = dir_b.clone();
        let daemon =
            tokio::spawn(
                async move { daemon_core(&daemon_dir, daemon_opts, daemon_stop_rx).await },
            );

        // 6. Wait for the second pairing (owner side observes it), then
        // send one real file into the daemon's listener.
        let deadline = Instant::now() + Duration::from_secs(30);
        while paired.lock().unwrap().len() < 2 {
            assert!(
                Instant::now() < deadline,
                "second pairing never completed (daemon finished early: {})",
                daemon.is_finished()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await; // bind settles

        let src = tmp.path().join("smoke-src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..1_500_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(src.join("journey.bin"), &payload).unwrap();
        let identity_a =
            ak_identity::tls::make_self_signed(&crate::load_identity(&dir_a).unwrap().0);
        let mut send_spec = ak_engine::SendSpec::new(
            src.clone(),
            SocketAddr::from(([127, 0, 0, 1], daemon_port)),
            fp_b,
            identity_a,
        );
        send_spec.checkpoint_dir = tmp.path().join("checkpoints-a");
        send_spec.idle_timeout = Duration::from_secs(10);

        let report = loop {
            match ak_engine::send(send_spec.clone(), &|_| {}).await {
                Ok(report) => break report,
                Err(ak_engine::EngineError::Connect(_)) => {
                    // The daemon is still redeeming/binding; retry briefly.
                    assert!(
                        Instant::now() < deadline,
                        "daemon never became reachable for the send"
                    );
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                Err(other) => panic!("send failed: {other:?}"),
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
        let _ = owner_stop.send(true);
        owner_loop.abort();
        let _ = registry_stop.send(true);
    }
}
