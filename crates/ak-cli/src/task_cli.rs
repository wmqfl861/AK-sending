//! `ak-cli task` (wave 32): the v1 minimal task-code model as CLI commands
//! — the "GUI 功能 CLI 化" half of the Linux-client story.
//!
//! The v1 model (wave 28..31, frozen): ONE random code = ONE task, exactly
//! two sides, codes are generated on demand, a disconnect ends the task,
//! zero configuration. This module rebuilds the `ak_task_*` FFI plane's
//! logic on the SAME public library surfaces (`ak-session` task pairing +
//! `ak-engine` transfers + `ak-net` endpoints) — ak-cli does NOT link
//! ak-ffi; `crates/ak-ffi/src/task.rs` is the reference implementation the
//! structure below deliberately mirrors (candidate shape, echo responder,
//! fingerprint-bound admission, consent window, close notification).
//!
//! Command family (frozen):
//!
//! * `task code-send <PATHS...>` — generate a SENDER-side code, print it,
//!   wait for the joiner, pair, probe, send the paths, report, disconnect.
//! * `task code-recv [--out DIR]` — generate a RECEIVER-side code, print
//!   it, wait, pair, probe, then serve receives behind the 30 s consent
//!   prompt ([Y/n], timeout declines); one completed task ends the run.
//! * `task join-send <CODE> <PATHS...>` — join someone's code as the
//!   sender: redeem (explicit direction), probe, send, disconnect.
//! * `task join-recv <CODE> [--out DIR]` — join as the receiver: redeem,
//!   probe, serve with the consent prompt, disconnect after one task.
//!
//! Registry resolution follows the `invite` family exactly
//! (`--server`/`--registry-fp` persisted into `<id-dir>/registry.pin`,
//! later invocations reuse the pin). Progress reuses the engine progress
//! printers (`[send]`/`[recv]` lines). CLI is one-shot: there is no
//! request-id surface; Ctrl-C is a LOCAL disconnect (revoke the code +
//! best-effort close-notify), and a peer/network departure prints an
//! explicit reason and exits non-zero.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use ak_engine::{
    send, receive_accepted_consent, ConsentAnswer, ConsentInfo, EngineError, Progress, ReceiveSpec,
    Report, SendSpec, TransferConsent, DECLINED_WIRE_CODE,
};
use ak_identity::pairing_txn::random_id16;
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{
    ClientConfig, OrchestratorConfig, PairFlowError, PairingOrchestrator, PeerEndpoint,
    ProbeOptions, ProbeOutcome, RegistryClient, TaskDirection, TaskDisconnectReason, TaskError,
    TaskEvent, TaskPairing, TaskPairingConfig, TaskPaired, TASK_CODE_TTL_SECS,
};
use ak_wire::{Frame, Message};

use crate::invite::RegistrySettings;

// ---------------------------------------------------------------------------
// Tunables (mirroring crates/ak-ffi/src/task.rs — same rationale per value).
// ---------------------------------------------------------------------------

/// Offer-level consent window: the terminal prompt waits this long, then
/// auto-DECLINES (never auto-accepts). 30 s per the frozen v1 model; the
/// field is injectable so tests exercise the expiry on a short window.
pub(crate) const CONSENT_WINDOW: Duration = Duration::from_secs(30);

/// Bounded wait for the registry connect of the task-pairing stack.
const REGISTRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Redeem window offered to the registry: how long the code owner has to
/// answer once a joiner typed the code (ak-session's 90 s default is the
/// long-lived invite UX, not the short-TTL task-code one).
const TASK_REDEEM_WINDOW_MS: u64 = 10_000;

/// Probe tunables: one round listens 1.5 s with probes every 150 ms; a
/// failed first round gets exactly one retry after a short stagger.
const TASK_PROBE_OPTIONS: ProbeOptions = ProbeOptions {
    interval: Duration::from_millis(150),
    window: Duration::from_millis(1_500),
};
const TASK_PROBE_RETRY_PAUSE: Duration = Duration::from_millis(300);

/// Bound for the resident receiver's first bind.
const RECEIVER_BIND_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-candidate connect budget of the send driver (split budget: the clock
/// starts at the engine's `Connecting` report, never during the source
/// scan, and stops once connected).
const TASK_CONNECT_BUDGET: Duration = Duration::from_secs(10);

/// Idle budget of a task send: must comfortably cover the receiver's
/// [`CONSENT_WINDOW`] plus slow-storage scans on the far end.
const TASK_SEND_IDLE: Duration = Duration::from_secs(120);

/// Whole-list connect passes: a send may fire the instant its pairing
/// returned while the peer's admission bookkeeping trails by a beat —
/// purely connect-level failures are retried after a short settle pause.
const TASK_CONNECT_PASSES: usize = 3;
const TASK_SETTLE_PAUSE: Duration = Duration::from_millis(300);
const ATTEMPT_PAUSE: Duration = Duration::from_millis(150);

/// Upper bounds of the endpoint blob's candidate list (the wire bound is
/// 16 candidates / 512 bytes; the caps keep the blob well under both).
const MAX_DIAL_CANDIDATES: usize = 8;
const MAX_ECHO_CANDIDATES: usize = 4;

/// Wire marker of the idle close notification: a disconnecting side with no
/// live transfer dials the peer's data endpoint and hands it exactly one
/// Error frame with this text, so the LISTENING peer learns the departure
/// instead of staying "connected" (wave-29 P2-03 shape).
const TASK_CLOSE_MESSAGE: &str = "TASK-CLOSE: paired device disconnected";
const TASK_CLOSE_CONNECT_BUDGET: Duration = Duration::from_secs(3);
const TASK_CLOSE_TOTAL_BUDGET: Duration = Duration::from_secs(6);

/// Anti-reflection constants of the probe echo responder (wave-29 P2-12):
/// replies carry the all-zero nonce and are never themselves answered;
/// each source endpoint gets a bounded reply budget per window.
const ECHO_REPLY_NONCE: [u8; 16] = [0u8; 16];
const ECHO_REPLY_BUDGET: u32 = 32;
const ECHO_REPLY_WINDOW: Duration = Duration::from_secs(1);
const ECHO_SOURCES_MAX: usize = 1024;

// ---------------------------------------------------------------------------
// clap surface
// ---------------------------------------------------------------------------

/// The `ak-cli task` subcommand family (wave 32). One code = one task;
/// every verb exits when ITS task ends (success, decline of the last
/// pending state, peer departure, Ctrl-C).
#[derive(Debug, clap::Subcommand)]
pub(crate) enum TaskCommand {
    /// Generate a sender-side task code and print it, wait for the peer to
    /// join, then send PATHS over the direct link and report. Registry
    /// flags follow `invite`: --server/--registry-fp given together persist
    /// the pin; later runs reuse it.
    CodeSend {
        /// Files/directories to send once the peer joins (>= 1).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Task-code lifetime in seconds (60..=2592000; default 600).
        #[arg(long)]
        ttl_secs: Option<u32>,
        /// Extra candidate IP to advertise (repeatable): an address NAT or
        /// 1:1-NAT hides from interface enumeration (e.g. the public IP of
        /// a cloud VM). Paired with this run's data/echo port.
        #[arg(long = "advertise")]
        advertise: Vec<std::net::IpAddr>,
        /// Registry endpoint ip:port (with --registry-fp; persisted).
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        /// Registry identity pin, 64 hex characters (with --server).
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Generate a receiver-side task code and print it, wait for the peer
    /// to join, then receive behind a 30 s [Y/n] consent prompt into --out.
    /// One completed transfer ends the run (single-task model).
    CodeRecv {
        /// Destination directory (default: current directory).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// Task-code lifetime in seconds (60..=2592000; default 600).
        #[arg(long)]
        ttl_secs: Option<u32>,
        /// Extra candidate IP to advertise (repeatable): an address NAT or
        /// 1:1-NAT hides from interface enumeration (e.g. the public IP of
        /// a cloud VM). Paired with this run's data/echo port.
        #[arg(long = "advertise")]
        advertise: Vec<std::net::IpAddr>,
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Join someone's task code as the SENDER: redeem (explicit direction),
    /// probe the peer, send PATHS, disconnect. Fails without burning the
    /// code when the code's owner declared the same direction.
    JoinSend {
        /// The task code in its canonical AKI1-... form.
        code: String,
        /// Files/directories to send (>= 1).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Extra candidate IP to advertise (repeatable): an address NAT or
        /// 1:1-NAT hides from interface enumeration (e.g. the public IP of
        /// a cloud VM). Paired with this run's data/echo port.
        #[arg(long = "advertise")]
        advertise: Vec<std::net::IpAddr>,
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Join someone's task code as the RECEIVER: redeem, probe, receive
    /// behind the 30 s consent prompt into --out, disconnect after one
    /// completed task.
    JoinRecv {
        /// The task code in its canonical AKI1-... form.
        code: String,
        /// Destination directory (default: current directory).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// Extra candidate IP to advertise (repeatable): an address NAT or
        /// 1:1-NAT hides from interface enumeration (e.g. the public IP of
        /// a cloud VM). Paired with this run's data/echo port.
        #[arg(long = "advertise")]
        advertise: Vec<std::net::IpAddr>,
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
}

/// The Ctrl-C marker of a locally interrupted task (the command layer maps
/// it to exit code 130; the disconnect itself already ran inside the
/// driver's teardown).
#[derive(Debug)]
pub(crate) struct TaskCancelled;

impl std::fmt::Display for TaskCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "interrupted (Ctrl-C): the task was disconnected locally")
    }
}

impl std::error::Error for TaskCancelled {}

/// `ak-cli task ...`: resolve shared settings, spawn the Ctrl-C watcher,
/// drive the one-shot task, print the summary.
pub(crate) async fn task_command(id: &Path, cmd: TaskCommand) -> Result<()> {
    match cmd {
        TaskCommand::CodeSend {
            paths,
            ttl_secs,
            advertise,
            server,
            registry_fp,
        } => {
            let ttl = resolve_ttl(ttl_secs)?;
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, ttl);
            cfg.advertise = advertise;
            check_send_paths(&paths)?;
            run_and_report(run_code_send(cfg, TaskHooks::production(), paths)).await
        }
        TaskCommand::CodeRecv {
            out,
            ttl_secs,
            advertise,
            server,
            registry_fp,
        } => {
            let ttl = resolve_ttl(ttl_secs)?;
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, ttl);
            cfg.advertise = advertise;
            let cfg = cfg.with_out_dir(out)?;
            run_and_report(run_code_recv(cfg, TaskHooks::production())).await
        }
        TaskCommand::JoinSend {
            code,
            paths,
            advertise,
            server,
            registry_fp,
        } => {
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, TASK_CODE_TTL_SECS);
            cfg.advertise = advertise;
            check_send_paths(&paths)?;
            run_and_report(run_join_send(cfg, TaskHooks::production(), code, paths)).await
        }
        TaskCommand::JoinRecv {
            code,
            out,
            advertise,
            server,
            registry_fp,
        } => {
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, TASK_CODE_TTL_SECS);
            cfg.advertise = advertise;
            let cfg = cfg.with_out_dir(out)?;
            run_and_report(run_join_recv(cfg, TaskHooks::production(), code)).await
        }
    }
}

/// Await one driver, print the outcome summary, and give Ctrl-C its
/// conventional exit code (130) after the teardown already ran.
async fn run_and_report(flow: impl Future<Output = Result<TaskOutcome>>) -> Result<()> {
    match flow.await {
        Ok(outcome) => {
            println!(
                "[task] done: {} file(s), {} in {:.1}s ({})",
                outcome.files,
                crate::human_bytes(outcome.bytes),
                outcome.elapsed.as_secs_f64(),
                outcome.detail
            );
            Ok(())
        }
        Err(error) => {
            if error.downcast_ref::<TaskCancelled>().is_some() {
                println!(
                    "[task] interrupted; the task code was revoked and the peer \
                     notified as far as it was reachable"
                );
                std::process::exit(130);
            }
            Err(error)
        }
    }
}

/// TTL resolution of the code-* verbs: absent/0 maps to the task default
/// (a v1 code never lingers forever), explicit values must respect the
/// server's invite bounds (the task plane rides the same rows).
fn resolve_ttl(secs: Option<u32>) -> Result<u32> {
    let raw = secs.unwrap_or(0);
    if raw == 0 {
        return Ok(TASK_CODE_TTL_SECS);
    }
    let (min, max) = (
        crate::invite::INVITE_TTL_MIN_SECS,
        crate::invite::INVITE_TTL_MAX_SECS,
    );
    if !(min..=max).contains(&raw) {
        bail!("--ttl-secs must be {min}..={max} (or omitted for the {TASK_CODE_TTL_SECS}s task default)");
    }
    Ok(raw)
}

/// Fail fast when a send verb got no readable sources (before any code is
/// generated or redeemed).
fn check_send_paths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if !path.exists() {
            bail!("path does not exist: {}", path.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Run configuration (test-injectable seams)
// ---------------------------------------------------------------------------

/// How the receive consent question is decided.
#[derive(Debug, Clone)]
pub(crate) enum ConsentInput {
    /// The real terminal prompt: `[Y/n]` within the window; empty/`y`/`yes`
    /// accepts, everything else (including the window elapsing) declines.
    Terminal,
    /// Tests: the scripted decision fires immediately (still through the
    /// engine's pending-consent path, never a bypass).
    #[cfg_attr(not(test), allow(dead_code))]
    Scripted(bool),
    /// Tests: no decision is ever sent; the window's expiry auto-declines
    /// (inject a short `consent_window` to keep the test fast).
    #[cfg_attr(not(test), allow(dead_code))]
    WindowTimeout,
}

/// Everything one task run needs; the consent seams exist so the embedded
/// tests can drive the decline/timeout branches deterministically.
#[derive(Debug, Clone)]
pub(crate) struct TaskRunConfig {
    pub(crate) id_dir: PathBuf,
    pub(crate) registry: RegistrySettings,
    /// Resolved code TTL (absent/0 already mapped to the task default).
    pub(crate) ttl_secs: u32,
    /// Receiver side: where transfers land (created eagerly).
    pub(crate) out_dir: PathBuf,
    pub(crate) consent_window: Duration,
    pub(crate) consent: ConsentInput,
    /// Operator-supplied extra candidate IPs (`--advertise`, repeatable):
    /// addresses NAT/1:1-NAT hides from interface enumeration (e.g. a
    /// cloud VM's public IP that is not on any interface). Each is paired
    /// with the run's data port (receiver runs) or the echo-responder
    /// port (sender runs) and PREPENDED to the advertised blob — the
    /// zero-config enumeration stays as the fallback tail.
    pub(crate) advertise: Vec<std::net::IpAddr>,
}

impl TaskRunConfig {
    pub(crate) fn new(id_dir: PathBuf, registry: RegistrySettings, ttl_secs: u32) -> Self {
        TaskRunConfig {
            id_dir,
            registry,
            ttl_secs,
            out_dir: PathBuf::from("."),
            consent_window: CONSENT_WINDOW,
            consent: ConsentInput::Terminal,
            advertise: Vec::new(),
        }
    }

    /// Set (and create) the receive directory; an unusable path fails now,
    /// not mid-task.
    pub(crate) fn with_out_dir(mut self, out: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&out)
            .with_context(|| format!("cannot create receive dir {}", out.display()))?;
        self.out_dir = out;
        Ok(self)
    }
}

/// Per-run hooks: the code hand-off (tests feed the joiner), the
/// pairing-completion signal (tests sequence the disconnect branch) and
/// the cancellation channel (production: Ctrl-C; tests: a watch they flip).
pub(crate) struct TaskHooks {
    pub(crate) code_out: Option<tokio::sync::oneshot::Sender<String>>,
    pub(crate) paired: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) cancel: tokio::sync::watch::Receiver<bool>,
}

impl TaskHooks {
    /// The production shape: no test channels, a watch the Ctrl-C task
    /// flips.
    fn production() -> Self {
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = cancel_tx.send(true);
            }
        });
        TaskHooks {
            code_out: None,
            paired: None,
            cancel: cancel_rx,
        }
    }

    fn fire_paired(&mut self) {
        if let Some(tx) = self.paired.take() {
            let _ = tx.send(());
        }
    }
}

/// What one finished task did (the command layer's summary line).
#[derive(Debug)]
pub(crate) struct TaskOutcome {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    pub(crate) elapsed: Duration,
    pub(crate) detail: String,
}

// ---------------------------------------------------------------------------
// Session state shared by the owner loop, the receiver loop and the driver
// ---------------------------------------------------------------------------

/// This device's side of the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Sender,
    Receiver,
}

impl Side {
    fn direction(self) -> TaskDirection {
        match self {
            Side::Sender => TaskDirection::Send,
            Side::Receiver => TaskDirection::Recv,
        }
    }
}

/// The one link of a completed pairing (conn id = the pairing transaction
/// id, identical on both sides — kept for reporting only).
#[derive(Debug, Clone)]
struct TaskLink {
    #[allow(dead_code)]
    conn_id: [u8; 16],
    code_id: [u8; 16],
    /// The peer as its endpoint blob advertised (probe input).
    peer: PeerEndpoint,
    /// The transcript-verified peer key to pin at dial/authorize time.
    peer_fp: Fingerprint,
}

impl TaskLink {
    /// From a completed pairing: each side pins the OTHER side's verified
    /// key (the owner verifies the redeemer; the joiner the owner).
    fn of(paired: &TaskPaired, owner_side: bool) -> Self {
        TaskLink {
            conn_id: paired.pairing_id,
            code_id: paired.code_id,
            peer: paired.peer.clone(),
            peer_fp: if owner_side {
                paired.verified.redeemer_fingerprint()
            } else {
                paired.verified.owner_fingerprint()
            },
        }
    }
}

/// The pairing-completion signal: a `watch` (not a `Notify`) so a
/// completion that lands between two awaits can never be missed.
type LinkWatch = tokio::sync::watch::Receiver<Option<TaskLink>>;

/// The registry task stack of one CLI run: one client, one orchestrator
/// (journal + trust book under `id_dir`), one `TaskPairing`, plus the
/// task-aware owner loop behind the pre-commit redeem-offer gate
/// (code-* verbs only — a joiner answers no redeems).
struct TaskStack {
    pairing: TaskPairing,
    owner_loop: Option<tokio::task::JoinHandle<()>>,
    link_rx: LinkWatch,
}

/// Everything one run owns; `finish` is the single teardown path (abort
/// waiters first, close-notify while idle, revoke the code, close the
/// control client).
struct Session {
    key: DeviceKeyPair,
    stack: TaskStack,
    events: tokio::sync::broadcast::Receiver<TaskEvent>,
    receiver: Option<tokio::task::JoinHandle<()>>,
    echo: Option<tokio::task::JoinHandle<()>>,
    code_id: Option<[u8; 16]>,
    link: Option<TaskLink>,
    /// The pairing-completion publisher (the owner loop holds a clone; the
    /// joiner flows publish through [`Session::set_link`]).
    link_tx: tokio::sync::watch::Sender<Option<TaskLink>>,
}

/// Build the run's registry stack: real pinned-TLS client, orchestrator
/// with the task redeem window, `TaskPairing`, and — for code owners — the
/// owner loop behind the redeem-offer gate (a same-direction join is
/// vetoed BEFORE the server commits, so the code's single redeem survives
/// the refusal).
async fn open_session(cfg: &TaskRunConfig, owner: bool) -> Result<Session> {
    let (key, _) = crate::load_identity(&cfg.id_dir)?;
    let name = crate::whoami();
    let connect = RegistryClient::connect_with_config(
        cfg.registry.server,
        cfg.registry.registry_id,
        &key,
        &name,
        ClientConfig::default(),
    );
    let client = match tokio::time::timeout(REGISTRY_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => bail!("cannot connect to the pairing registry: {error}"),
        Err(_) => bail!("registry connect timed out after {REGISTRY_CONNECT_TIMEOUT:?}"),
    };
    let orch = PairingOrchestrator::open(
        client,
        key.clone(),
        &cfg.id_dir,
        OrchestratorConfig {
            redeem_window_ms: TASK_REDEEM_WINDOW_MS,
            ..OrchestratorConfig::default()
        },
    )
    .with_context(|| {
        format!(
            "cannot open the task pairing journal under {}",
            cfg.id_dir.display()
        )
    })?;
    let pairing = TaskPairing::open(
        orch,
        TaskPairingConfig {
            ttl_secs: TASK_CODE_TTL_SECS,
            probe: TASK_PROBE_OPTIONS,
        },
    );
    let (link_tx, link_rx) = tokio::sync::watch::channel(None);
    let mut owner_handle = None;
    if owner {
        // The pre-commit gate: veto a same-direction or endpoint-less join
        // before the server commits (the code stays redeemable).
        pairing.install_redeem_offer_gate();
        let callback_pairing = pairing.clone();
        let callback_link_tx = link_tx.clone();
        let on_paired: Arc<dyn Fn(ak_session::pairing::VerifiedPairing) + Send + Sync> =
            Arc::new(move |verified| {
                let code_id = verified.record().transcript.code_id;
                let Some(direction) = callback_pairing
                    .active_tasks()
                    .into_iter()
                    .find(|task| task.code == code_id)
                    .map(|task| task.direction)
                else {
                    return; // not one of this run's task codes
                };
                if let Ok(paired) = callback_pairing.complete_owner_pairing(&verified, direction) {
                    let link = TaskLink::of(&paired, true);
                    let _ = callback_link_tx.send(Some(link));
                }
                // A refused completion (direction conflict defense in depth,
                // malformed blob): the registry gate already vetoed the
                // normal path; the honest surface is "no pairing happened".
            });
        owner_handle = Some(
            pairing
                .orchestrator()
                .spawn_owner_loop_verified(Some(on_paired)),
        );
    }
    Ok(Session {
        key,
        stack: TaskStack {
            pairing: pairing.clone(),
            owner_loop: owner_handle,
            link_rx,
        },
        events: pairing.subscribe(),
        receiver: None,
        echo: None,
        code_id: None,
        link: None,
        link_tx,
    })
}

impl Session {
    /// Record the completed pairing on this side: the driver's copy AND
    /// the watch the receiver admission and consent attribution read (a
    /// joiner's only publisher — the owner loop's callback holds its own
    /// clone of the sender).
    fn set_link(&mut self, link: TaskLink) {
        let _ = self.link_tx.send(Some(link.clone()));
        self.link = Some(link);
    }
}

impl Session {
    /// Bind the probe echo responder (derived candidates append its port
    /// last; see `blob_candidates`).
    fn bind_echo_responder(&mut self) -> Result<SocketAddr> {
        let std_socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .context("cannot bind the probe responder")?;
        std_socket
            .set_nonblocking(true)
            .context("cannot arm the probe responder")?;
        let addr = std_socket
            .local_addr()
            .context("probe responder bind address")?;
        let socket = tokio::net::UdpSocket::from_std(std_socket)
            .context("cannot register the probe responder")?;
        self.echo = Some(tokio::spawn(async move {
            echo_responder_loop(socket).await;
        }));
        Ok(addr)
    }

    /// Bind the resident receiver: ONE wildcard endpoint serving a LOOP of
    /// connections until the task ends. Admission binds the pairing: the
    /// accept `authorize` admits ONLY the transcript-verified peer
    /// fingerprint, so a stranger can neither deliver an offer nor consume
    /// the receiver; handshake failures are per-connection (keep
    /// listening), a DECLINED offer ends only that connection.
    async fn bind_task_receiver(
        &mut self,
        cfg: &TaskRunConfig,
    ) -> Result<(
        SocketAddr,
        tokio::sync::mpsc::Receiver<ReceiverEvent>,
        Arc<StdMutex<[u8; 16]>>,
    )> {
        let code_slot: Arc<StdMutex<[u8; 16]>> = Arc::new(StdMutex::new(random_id16()));
        let ctx = Arc::new(ReceiverCtx {
            key: self.key.clone(),
            id_dir: cfg.id_dir.clone(),
            out_dir: cfg.out_dir.clone(),
            consent_window: cfg.consent_window,
            consent: cfg.consent.clone(),
            device_name: format!("ak-task-{}", self.key.fingerprint().short_id()),
            link_rx: self.stack.link_rx.clone(),
            pairing: self.stack.pairing.clone(),
        });
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(4);
        let slot = Arc::clone(&code_slot);
        self.receiver = Some(tokio::spawn(async move {
            run_task_receiver(ctx, slot, addr_tx, events_tx).await;
        }));
        match tokio::time::timeout(RECEIVER_BIND_TIMEOUT, addr_rx).await {
            Ok(Ok(addr)) => Ok((addr, events_rx, code_slot)),
            Ok(Err(_)) => bail!("the resident receiver stopped before binding"),
            Err(_) => bail!("resident receiver bind timed out"),
        }
    }

    /// The single teardown: waiters before owners. A locally cancelled or
    /// failed run that had paired (and completed no transfer) additionally
    /// close-notifies the peer so its waiting side exits promptly.
    async fn finish(mut self, completed: bool) {
        if let Some(task) = self.receiver.take() {
            task.abort();
        }
        if let Some(task) = self.echo.take() {
            task.abort();
        }
        if !completed {
            if let Some(link) = self.link.clone() {
                close_notify_peer(&self.key, &link).await;
            }
        }
        if let Some(task) = self.stack.owner_loop.take() {
            task.abort();
        }
        if let Some(code_id) = self.code_id {
            let pairing = self.stack.pairing.clone();
            let _ = tokio::time::timeout(Duration::from_secs(3), pairing.disconnect(code_id)).await;
        }
        let client = self.stack.pairing.orchestrator().client().clone();
        let _ = tokio::time::timeout(Duration::from_secs(1), client.close()).await;
    }
}

/// What the resident receiver reported to the driver.
enum ReceiverEvent {
    /// One transfer completed: the single-transfer task's terminal state.
    Done { files: u64, bytes: u64 },
    /// The paired peer's data plane went away (TASK-CLOSE marker, channel
    /// failure or mid-transfer departure).
    PeerGone(&'static str),
}

struct ReceiverCtx {
    key: DeviceKeyPair,
    id_dir: PathBuf,
    out_dir: PathBuf,
    consent_window: Duration,
    consent: ConsentInput,
    device_name: String,
    link_rx: LinkWatch,
    /// To converge the local task state on a data-plane departure.
    pairing: TaskPairing,
}

/// The resident receiver loop (wave-29 P1-04/P2-05 semantics, the FFI
/// shape): one bound port, a loop of connections, fingerprint-bound
/// admission, per-connection consent, decline keeps listening, done is
/// terminal.
async fn run_task_receiver(
    ctx: Arc<ReceiverCtx>,
    code_slot: Arc<StdMutex<[u8; 16]>>,
    first_addr: tokio::sync::oneshot::Sender<SocketAddr>,
    events: tokio::sync::mpsc::Sender<ReceiverEvent>,
) {
    let identity = make_self_signed(&ctx.key);
    let endpoint = match ak_net::DirectEndpoint::bind(
        SocketAddr::from(([0, 0, 0, 0], 0)),
        &identity,
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(_) => return, // the dropped oneshot fails the bind wait
    };
    let bound = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let _ = first_addr.send(bound);
    let printer = crate::progress_printer("recv");
    let current_code = || -> [u8; 16] {
        code_slot
            .lock()
            .map(|slot| *slot)
            .unwrap_or_else(|poisoned| *poisoned.into_inner())
    };
    loop {
        // Admission binds the pairing: only the transcript-verified peer,
        // and only once the pairing completed (pre-pairing dials are
        // refused at the TLS gate and the loop keeps waiting).
        let authorize = |fp: &Fingerprint| -> bool {
            ctx.link_rx
                .borrow()
                .as_ref()
                .is_some_and(|link| link.peer_fp == *fp)
        };
        let channel = match endpoint.accept(&authorize).await {
            Ok(channel) => channel,
            Err(ak_net::ConnectError::Unauthorized) => continue,
            Err(ak_net::ConnectError::Io(err))
                if err.kind() == std::io::ErrorKind::NotConnected
                    && err.to_string().contains("endpoint closed") =>
            {
                return; // the endpoint itself is gone (teardown abort)
            }
            Err(failure) => {
                // One connection's handshake failed (a stranger's aborted
                // dial): per-connection fact, never a listener verdict.
                println!(
                    "[task] inbound handshake failed ({}); still listening",
                    handshake_failure_word(&failure)
                );
                continue;
            }
        };
        let out_dir = ctx.out_dir.clone();
        let _ = std::fs::create_dir_all(&out_dir);
        let mut spec = ReceiveSpec::new(out_dir.clone(), identity.clone());
        spec.checkpoint_dir = ctx.id_dir.join("checkpoints");
        spec.device_name = ctx.device_name.clone();
        spec.idle_timeout = TASK_SEND_IDLE;
        // The consent hook returns PENDING immediately (no worker is ever
        // parked on the decision) and attributes the offer by the verified
        // sender fingerprint — an unpaired stranger is refused silently.
        let consent: TransferConsent = {
            let ctx = Arc::clone(&ctx);
            let link_rx = ctx.link_rx.clone();
            let consent_out_dir = out_dir.clone();
            Arc::new(move |info: &ConsentInfo| {
                let Some(link) = link_rx.borrow().clone() else {
                    return ConsentAnswer::Now(false);
                };
                if link.peer_fp != info.sender_fingerprint {
                    return ConsentAnswer::Now(false);
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                spawn_consent_decider(&ctx, info, &consent_out_dir, tx);
                ConsentAnswer::Pending(rx)
            })
        };
        let result = receive_accepted_consent(channel, spec, &*printer, Some(consent)).await;
        match result {
            Ok(report) => {
                println!(
                    "[task] received {} file(s), {} into {}",
                    report.files,
                    crate::human_bytes(report.bytes),
                    out_dir.display()
                );
                let _ = events
                    .send(ReceiverEvent::Done {
                        files: report.files,
                        bytes: report.bytes,
                    })
                    .await;
                return; // single-transfer terminal state
            }
            Err(EngineError::ConsentDeclined { .. }) => {
                println!("[task] offer declined; still listening (the sender may retry)");
                continue;
            }
            Err(EngineError::PeerError { message, .. }) if message == TASK_CLOSE_MESSAGE => {
                ctx.pairing.report_peer_disconnect(current_code());
                let _ = events.send(ReceiverEvent::PeerGone(
                    "the peer disconnected (task closed by the peer)",
                ))
                .await;
                return;
            }
            Err(_) => {
                ctx.pairing.report_peer_disconnect(current_code());
                let _ = events
                    .send(ReceiverEvent::PeerGone(
                        "the peer disconnected during the transfer",
                    ))
                    .await;
                return;
            }
        }
    }
}

/// Bounded reason word of one failed accept — a fixed vocabulary, never
/// the raw transport text (hostile peers control parts of it).
fn handshake_failure_word(error: &ak_net::ConnectError) -> &'static str {
    match error {
        ak_net::ConnectError::PeerIdentity(_) => "peer certificate rejected",
        _ => "handshake transport failure",
    }
}

/// Print the consent banner and decide: the terminal branch reads one
/// stdin line on a blocking thread and races the window (an expired window
/// declines — never auto-accepts); the scripted branches keep the engine's
/// pending path exercised.
fn spawn_consent_decider(
    ctx: &ReceiverCtx,
    info: &ConsentInfo,
    out_dir: &Path,
    decide: tokio::sync::oneshot::Sender<bool>,
) -> tokio::task::JoinHandle<()> {
    let window = ctx.consent_window;
    let input = ctx.consent.clone();
    let files = info.file_count;
    let bytes = info.total_bytes;
    let root = info.root_name.clone();
    let out = out_dir.to_path_buf();
    tokio::spawn(async move {
        println!(
            "[task] the peer wants to send {files} file(s) ({} under {root:?}) into {}",
            crate::human_bytes(bytes),
            out.display()
        );
        match input {
            ConsentInput::Scripted(decision) => {
                let _ = decide.send(decision);
            }
            ConsentInput::WindowTimeout => {
                tokio::time::sleep(window).await;
                println!(
                    "[task] no decision within {} ms: declined",
                    window.as_millis()
                );
                let _ = decide.send(false);
            }
            ConsentInput::Terminal => {
                print!(
                    "[task] accept? [Y/n] (declines automatically after {}s): ",
                    window.as_secs()
                );
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                let decision = tokio::select! {
                    line = tokio::task::spawn_blocking(read_stdin_line) => {
                        let answer = line.unwrap_or_default().trim().to_ascii_lowercase();
                        match answer.as_str() {
                            "" | "y" | "yes" => true,
                            "n" | "no" => false,
                            other => {
                                println!("[task] unrecognized answer {other:?}: declined");
                                false
                            }
                        }
                    }
                    _ = tokio::time::sleep(window) => {
                        println!();
                        println!("[task] no answer within {}s: declined", window.as_secs());
                        false
                    }
                };
                let _ = decide.send(decision);
            }
        }
    })
}

/// One stdin line for the terminal consent prompt (runs on a blocking
/// thread; an unread line after a window expiry is harmless — the process
/// exits with the task).
fn read_stdin_line() -> String {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(_) => line,
        Err(_) => String::new(),
    }
}

/// The echo responder body: answer ak-net hole-punch probe packets from
/// any advertised address with the all-zero nonce (never answered itself),
/// under a per-source reply budget per window.
async fn echo_responder_loop(socket: tokio::net::UdpSocket) {
    let mut buffer = [0u8; 1500];
    let mut budgets: HashMap<SocketAddr, (Instant, u32)> = HashMap::new();
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((len, source)) => {
                if !ak_net::holepunch::is_probe_packet(&buffer[..len]) {
                    continue;
                }
                // A reply (all-zero nonce) is never answered again — the
                // loop-killer; a genuine probe's nonce never is all-zero.
                if buffer[8..len] == ECHO_REPLY_NONCE {
                    continue;
                }
                let now = Instant::now();
                let allowed = match budgets.get_mut(&source) {
                    Some(entry) => {
                        if now.duration_since(entry.0) >= ECHO_REPLY_WINDOW {
                            *entry = (now, 1);
                            true
                        } else if entry.1 < ECHO_REPLY_BUDGET {
                            entry.1 += 1;
                            true
                        } else {
                            false
                        }
                    }
                    None => {
                        if budgets.len() >= ECHO_SOURCES_MAX {
                            budgets.clear();
                        }
                        budgets.insert(source, (now, 1));
                        true
                    }
                };
                if allowed {
                    let _ = socket
                        .send_to(
                            &ak_net::holepunch::probe_packet(&ECHO_REPLY_NONCE),
                            source,
                        )
                        .await;
                }
            }
            Err(_) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Candidates (the ak-ffi shapes, verbatim semantics)
// ---------------------------------------------------------------------------

/// Every local interface address (v4) with `bound`'s port, loopback last.
fn local_candidates(bound: SocketAddr) -> Vec<SocketAddr> {
    let port = bound.port();
    let mut out = Vec::new();
    if let Ok(nets) = ak_net::classify::local_nets() {
        for net in nets {
            if let std::net::IpAddr::V4(v4) = net.addr {
                out.push(SocketAddr::from((v4, port)));
            }
        }
    }
    let loopback = SocketAddr::from(([127, 0, 0, 1], port));
    if !out.contains(&loopback) {
        out.push(loopback);
    }
    out
}

/// LAN preference (RFC1918 private IPv4, loopback v6): a routing hint,
/// never an authorization.
fn prefers_lan(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => ak_net::classify::is_ipv4_private(v4),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Dedupe (normalizing v4-mapped IPv6), then stable-sort LAN-first.
fn order_candidates(targets: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut ordered: Vec<SocketAddr> = Vec::with_capacity(targets.len());
    for addr in targets {
        let addr = ak_net::classify::expand_mapped(addr);
        if !ordered.contains(&addr) {
            ordered.push(addr);
        }
    }
    ordered.sort_by_key(|addr| !prefers_lan(addr));
    ordered
}

/// Assemble the endpoint-blob candidate list: the dial candidates (the
/// receiver's real port first), then the probe echo port, deduped, capped,
/// and shrunk until the blob fits the wire bound.
/// Prepend the operator's `--advertise` IPs (paired with `port`) to a
/// built blob, deduped (wave-32: NAT escape hatch — see TaskRunConfig).
fn with_advertised(blob: Vec<SocketAddr>, advertise: &[std::net::IpAddr], port: u16) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::with_capacity(blob.len() + advertise.len());
    for ip in advertise {
        let addr = SocketAddr::new(*ip, port);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    for addr in blob {
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    out.truncate(ak_session::pairing::ENDPOINT_BLOB_MAX_CANDIDATES);
    out
}

fn blob_candidates(dial: Vec<SocketAddr>, echo: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::with_capacity(dial.len() + echo.len());
    for addr in dial.into_iter().take(MAX_DIAL_CANDIDATES) {
        let addr = ak_net::classify::expand_mapped(addr);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    for addr in echo.into_iter().take(MAX_ECHO_CANDIDATES) {
        let addr = ak_net::classify::expand_mapped(addr);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    if out.len() > ak_session::pairing::ENDPOINT_BLOB_MAX_CANDIDATES {
        out.truncate(ak_session::pairing::ENDPOINT_BLOB_MAX_CANDIDATES);
    }
    while out.len() > 1 {
        let endpoint = PeerEndpoint {
            candidates: out.clone(),
            name: String::new(),
            direction: TaskDirection::Send,
        };
        if ak_session::pairing::encode_endpoint_blob(&endpoint).is_ok() {
            break;
        }
        out.pop();
    }
    out
}

/// Map an ak-session task-pairing failure onto bounded CLI error strings
/// (the registry never reveals whether a code exists — unknown, expired
/// and revoked all answer PAIR_UNAVAILABLE).
fn task_error_message(error: TaskError) -> anyhow::Error {
    let text = match &error {
        TaskError::Flow(PairFlowError::Rejected { code })
            if *code == ak_signal::protocol_v2::message::REDEEM_EXHAUSTED =>
        {
            "the task code was already joined (one code pairs exactly once)".to_string()
        }
        TaskError::Flow(PairFlowError::Unavailable { .. }) => {
            "unknown task code (or expired, revoked, owner offline)".to_string()
        }
        TaskError::Flow(PairFlowError::BadCode(cause)) => {
            format!("task code rejected: {cause}")
        }
        other => format!("registry task pairing failed: {other}"),
    };
    anyhow::Error::new(error).context(text)
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// One probe round with exactly one retry after a short stagger (the
/// peer's own probe may start a beat later on the slow path).
async fn probe_with_retry(pairing: &TaskPairing, peer: &PeerEndpoint) -> ProbeOutcome {
    let first = pairing.probe_peer(peer).await;
    if matches!(first, ProbeOutcome::Success { .. }) {
        return first;
    }
    tokio::time::sleep(TASK_PROBE_RETRY_PAUSE).await;
    let second = pairing.probe_peer(peer).await;
    if matches!(second, ProbeOutcome::Success { .. }) {
        second
    } else {
        first
    }
}

/// Run the honest reachability probe and print its verdict (an all-failed
/// probe does not abort: it proves UDP liveness, not QUIC dialability).
async fn probe_and_print(pairing: &TaskPairing, link: &TaskLink) {
    let outcome = probe_with_retry(pairing, &link.peer).await;
    match &outcome {
        ProbeOutcome::Success { address } => {
            let lan = if prefers_lan(address) { "lan" } else { "internet" };
            println!("[probe] peer reachable via {address} ({lan})");
        }
        ProbeOutcome::AllFailed { failures } => {
            for failure in failures {
                println!(
                    "[probe] {} unreachable ({})",
                    failure.candidate, failure.reason
                );
            }
            println!("[probe] no candidate answered; the transfer is still attempted");
        }
    }
}

// ---------------------------------------------------------------------------
// The send driver (split connect budget + LAN-first fallback)
// ---------------------------------------------------------------------------

/// What one candidate attempt concluded.
enum Attempt {
    Done(Report),
    /// The connection was established but the transfer failed.
    /// `nothing_offered` marks failures where the connection died BEFORE
    /// any protocol exchange (the just-paired peer's receiver admission
    /// can trail the pairing by a beat — its TLS gate closes our dial
    /// after the client-side handshake already succeeded). Those are the
    /// wave-29 P1-04 race, mirrored: retryable like a connect failure.
    FailedAfterConnect {
        reason: String,
        peer_gone: bool,
        nothing_offered: bool,
    },
    Declined,
    ConnectFailed(String),
}

/// One task send: iterate the peer's candidates LAN-first under the split
/// connect budget; a post-connect failure fails the send (another address
/// cannot fix it); a DECLINED answer is surfaced verbatim; purely
/// connect-level failure of the whole list gets [`TASK_CONNECT_PASSES`]
/// passes (the just-paired peer's admission bookkeeping can trail the
/// pairing by a beat).
async fn drive_task_send(
    pairing: &TaskPairing,
    key: &DeviceKeyPair,
    id_dir: &Path,
    link: &TaskLink,
    paths: &[PathBuf],
) -> Result<Report> {
    let ordered = order_candidates(link.peer.candidates.clone());
    if ordered.is_empty() {
        bail!("the paired peer advertised no dial candidates");
    }
    let printer = crate::progress_printer("send");
    let mut failures: Vec<(SocketAddr, String)> = Vec::new();
    for pass in 0..TASK_CONNECT_PASSES {
        if pass > 0 {
            tokio::time::sleep(TASK_SETTLE_PAUSE).await;
            failures.clear();
        }
        for (index, addr) in ordered.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(ATTEMPT_PAUSE).await;
            }
            match attempt_send(key, id_dir, *addr, link.peer_fp, paths, &printer).await {
                Attempt::Done(report) => return Ok(report),
                Attempt::Declined => {
                    bail!("the receiving side declined the transfer")
                }
                Attempt::FailedAfterConnect {
                    reason,
                    peer_gone,
                    nothing_offered,
                } => {
                    if nothing_offered {
                        // The peer's admission trailed the pairing: retry
                        // like a connect failure (a settle pause follows).
                        failures.push((*addr, reason));
                        continue;
                    }
                    if peer_gone {
                        pairing.report_peer_disconnect(link.code_id);
                    }
                    bail!("transfer failed after connecting via {addr}: {reason}");
                }
                Attempt::ConnectFailed(reason) => failures.push((*addr, reason)),
            }
        }
    }
    let detail = failures
        .iter()
        .map(|(addr, reason)| format!("{addr}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ");
    bail!("no candidate accepted the connection ({detail})")
}

/// One candidate attempt: build the send spec (family-matched bind, task
/// idle budget covering the consent window), drive it under the connect
/// budget, and classify the outcome.
async fn attempt_send(
    key: &DeviceKeyPair,
    id_dir: &Path,
    addr: SocketAddr,
    pinned: Fingerprint,
    paths: &[PathBuf],
    printer: &Arc<dyn Fn(Progress) + Send + Sync>,
) -> Attempt {
    let mut spec = SendSpec::new(
        paths[0].clone(),
        addr,
        pinned,
        make_self_signed(key),
    );
    for extra in &paths[1..] {
        spec.add_root(extra.clone());
    }
    spec.bind = if addr.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    spec.checkpoint_dir = id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", key.fingerprint().short_id());
    spec.idle_timeout = TASK_SEND_IDLE;

    let marks = Arc::new(ConnectMarks::new());
    let progress = {
        let marks = Arc::clone(&marks);
        let printer = Arc::clone(printer);
        move |event: Progress| {
            marks.mark(&event);
            printer(event);
        }
    };
    let send = send(spec, &progress);
    tokio::pin!(send);
    let result = match drive_with_connect_budget(&marks, send.as_mut(), TASK_CONNECT_BUDGET).await {
        Ok(result) => result,
        Err(()) => {
            return Attempt::ConnectFailed(format!(
                "timeout after {}s (no QUIC handshake)",
                TASK_CONNECT_BUDGET.as_secs()
            ))
        }
    };
    let after_connect = marks.connected.load(Ordering::Relaxed);
    match result {
        Ok(report) => Attempt::Done(report),
        Err(error) => {
            if matches!(
                &error,
                EngineError::PeerError { code, .. } if *code == DECLINED_WIRE_CODE
            ) {
                return Attempt::Declined;
            }
            let reason = crate::one_line_reason(&error);
            if after_connect {
                Attempt::FailedAfterConnect {
                    peer_gone: matches!(error, EngineError::PeerClosed),
                    nothing_offered: !marks.offered.load(Ordering::Relaxed),
                    reason,
                }
            } else {
                Attempt::ConnectFailed(reason)
            }
        }
    }
}

/// Connect-phase bookkeeping: the engine invokes its progress callback
/// synchronously inside the polled future, so the marks carry no race.
struct ConnectMarks {
    connecting_at: StdMutex<Option<tokio::time::Instant>>,
    connecting: tokio::sync::Notify,
    connected: AtomicBool,
    /// Whether the Offer ever went out (the retryable-close test of
    /// [`Attempt::FailedAfterConnect`]).
    offered: AtomicBool,
}

impl ConnectMarks {
    fn new() -> Self {
        Self {
            connecting_at: StdMutex::new(None),
            connecting: tokio::sync::Notify::new(),
            connected: AtomicBool::new(false),
            offered: AtomicBool::new(false),
        }
    }

    fn mark(&self, progress: &Progress) {
        match progress {
            Progress::Connecting => {
                if let Ok(mut slot) = self.connecting_at.lock() {
                    *slot = Some(tokio::time::Instant::now());
                }
                self.connecting.notify_one();
            }
            Progress::Connected { .. } => {
                self.connected.store(true, Ordering::Relaxed);
            }
            Progress::Offered { .. } | Progress::Accepted { .. } => {
                self.offered.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// The split-budget driver: unbudgeted preparation, budgeted
/// bind/connect/handshake from the engine's own `Connecting` report,
/// unbudgeted transfer once connected.
async fn drive_with_connect_budget<S>(
    marks: &ConnectMarks,
    mut send: std::pin::Pin<&mut S>,
    budget: Duration,
) -> Result<<S as Future>::Output, ()>
where
    S: Future + ?Sized,
{
    let connect_started = tokio::select! {
        _ = marks.connecting.notified() => {
            marks
                .connecting_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .expect("Connecting instant is recorded before notify")
        }
        done = send.as_mut() => return Ok(done),
    };
    match tokio::time::timeout_at(connect_started + budget, send.as_mut()).await {
        Ok(done) => Ok(done),
        Err(_deadline_elapsed) => {
            if marks.connected.load(Ordering::Relaxed) {
                Ok(send.await)
            } else {
                Err(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Idle close notification (wave-29 P2-03 shape)
// ---------------------------------------------------------------------------

/// Dial the paired peer's advertised data candidates (LAN-first,
/// family-matched, TLS pinned to the pairing's verified fingerprint) and
/// deliver exactly one TASK-CLOSE Error frame, so a LISTENING peer learns
/// the departure instead of staying "connected". Best effort and bounded:
/// an unreachable peer is an honest miss.
async fn close_notify_peer(key: &DeviceKeyPair, link: &TaskLink) {
    let identity = make_self_signed(key);
    let candidates = order_candidates(link.peer.candidates.clone());
    let mut remaining = TASK_CLOSE_TOTAL_BUDGET;
    for addr in candidates {
        if remaining.is_zero() {
            return;
        }
        let bind = if addr.is_ipv6() {
            SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::from(([0, 0, 0, 0], 0))
        };
        let Ok(endpoint) = ak_net::DirectEndpoint::bind(bind, &identity).await else {
            return;
        };
        let dial = tokio::time::timeout(
            TASK_CLOSE_CONNECT_BUDGET.min(remaining),
            endpoint.connect(addr, &link.peer_fp),
        )
        .await;
        remaining = remaining.saturating_sub(TASK_CLOSE_CONNECT_BUDGET);
        let Ok(Ok(channel)) = dial else {
            continue; // candidate down: try the next
        };
        if let Ok(mut stream) = channel.open_bi().await {
            let frame = Frame {
                version: ak_wire::VERSION,
                message: Message::Error {
                    code: ak_core::error::ErrorCode::Cancelled,
                    message: TASK_CLOSE_MESSAGE.to_string(),
                },
            };
            let _ = stream.send_frame(&frame).await;
            let _ = stream.finish_send();
        }
        channel.close(0, b"task closed");
        return; // one delivered notification is enough
    }
}

// ---------------------------------------------------------------------------
// Waiting helpers
// ---------------------------------------------------------------------------

/// Resolve when the cancellation channel flips true. A channel whose
/// sender went away WITHOUT cancelling is NOT a cancellation (a dropped
/// test sender means "nobody will cancel") — that arm then never fires
/// and the caller's other select arms keep bounding the wait.
async fn wait_cancelled(cancel: &tokio::sync::watch::Receiver<bool>) {
    let mut rx = cancel.clone();
    loop {
        match rx.changed().await {
            Ok(()) => {
                if *rx.borrow() {
                    return;
                }
            }
            Err(_sender_gone_without_cancel) => {
                std::future::pending::<()>().await;
            }
        }
    }
}

fn disconnect_reason_word(reason: TaskDisconnectReason) -> &'static str {
    match reason {
        TaskDisconnectReason::ByPeer => "the peer disconnected",
        TaskDisconnectReason::Network => "the data-plane link was lost",
        TaskDisconnectReason::Local => "disconnected locally",
    }
}

/// Wait for this side's pairing to complete (owner callback or join
/// return). Ends at the code's TTL, on a task event that ends the task, or
/// on Ctrl-C.
async fn wait_paired(
    link_rx: &mut LinkWatch,
    events: &mut tokio::sync::broadcast::Receiver<TaskEvent>,
    cancel: &tokio::sync::watch::Receiver<bool>,
    ttl_secs: u32,
) -> Result<TaskLink> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(ttl_secs));
    loop {
        if let Some(link) = link_rx.borrow_and_update().clone() {
            return Ok(link);
        }
        tokio::select! {
            changed = link_rx.changed() => {
                if changed.is_err() {
                    bail!("the task session closed before anyone joined");
                }
                continue;
            }
            event = events.recv() => match event {
                Ok(TaskEvent::TaskDisconnected {
                    reason: TaskDisconnectReason::Local,
                    ..
                }) => continue,
                Ok(TaskEvent::TaskDisconnected { reason, .. }) => {
                    bail!(
                        "the task ended before the peer joined ({})",
                        disconnect_reason_word(reason)
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => continue,
            },
            _ = wait_cancelled(cancel) => return Err(TaskCancelled.into()),
            _ = tokio::time::sleep_until(deadline) => {
                bail!("the task code expired after {ttl_secs}s before anyone joined");
            }
        }
    }
}

/// Wait for the resident receiver's verdict (one completed transfer or the
/// peer's departure), with the registry task events and Ctrl-C racing it.
async fn wait_receive(
    receiver: &mut tokio::sync::mpsc::Receiver<ReceiverEvent>,
    events: &mut tokio::sync::broadcast::Receiver<TaskEvent>,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<TaskOutcome> {
    loop {
        tokio::select! {
            verdict = receiver.recv() => {
                return match verdict {
                    Some(ReceiverEvent::Done { files, bytes }) => Ok(TaskOutcome {
                        files,
                        bytes,
                        elapsed: Duration::from_secs(0),
                        detail: "received into the requested directory".to_string(),
                    }),
                    Some(ReceiverEvent::PeerGone(why)) => bail!("{why}"),
                    None => bail!("the resident receiver stopped unexpectedly"),
                };
            }
            event = events.recv() => match event {
                Ok(TaskEvent::TaskDisconnected {
                    reason: TaskDisconnectReason::Local,
                    ..
                }) => continue,
                Ok(TaskEvent::TaskDisconnected { reason, .. }) => {
                    bail!("{}", disconnect_reason_word(reason));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => continue,
            },
            _ = wait_cancelled(cancel) => return Err(TaskCancelled.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// The four flows (each ends in Session::finish on every path)
// ---------------------------------------------------------------------------

/// The eye-catching code block: the full AKI1 code plus how the peer uses
/// it and a copy hint (the code is a bearer credential — the block says
/// so).
fn print_code_block(display: &str, role: &str, ttl_secs: u32) {
    let bar = "=".repeat(68);
    println!("{bar}");
    println!(" task code (role: {role}) — one code pairs exactly once");
    println!(" valid for {ttl_secs}s · treat it like a password");
    println!("{}", "-".repeat(68));
    println!("  {display}");
    println!("{bar}");
    match role {
        "send" => println!(" the peer joins with: ak-cli task join-recv '{display}'"),
        _ => println!(" the peer joins with: ak-cli task join-send '{display}' <PATHS...>"),
    }
}

/// Announce the pairing on this side (the conn id is the shared
/// correlation key; the endpoint is what a send would dial first).
fn print_paired(link: &TaskLink) {
    let endpoint = order_candidates(link.peer.candidates.clone())
        .first()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|| "no endpoint".to_string());
    println!("[task] paired; peer {endpoint} (task {})", hex_of(&link.code_id));
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `task code-send`: claim a sender-side code, print it, wait for the
/// joiner, probe, send the paths, report, disconnect.
pub(crate) async fn run_code_send(
    cfg: TaskRunConfig,
    mut hooks: TaskHooks,
    paths: Vec<PathBuf>,
) -> Result<TaskOutcome> {
    let started = Instant::now();
    let mut session = open_session(&cfg, true).await?;
    let flow = async {
        let echo_addr = session.bind_echo_responder()?;
        // A sender-role code still advertises the creator's endpoints
        // (port 0 marks "no listener", which a probe skips as undialable);
        // the echo port appended last is the genuinely reachable one.
        let dial = local_candidates(SocketAddr::from(([0, 0, 0, 0], 0)));
        let blob = with_advertised(
            blob_candidates(dial, local_candidates(echo_addr)),
            &cfg.advertise,
            echo_addr.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        let code = session
            .stack
            .pairing
            .create_task_code_with_ttl(Side::Sender.direction(), &blob, cfg.ttl_secs)
            .await
            .map_err(task_error_message)?;
        session.code_id = Some(code.invite.code_id);
        print_code_block(&code.invite.display, "send", cfg.ttl_secs);
        if let Some(tx) = hooks.code_out.take() {
            let _ = tx.send(code.invite.display.clone());
        }
        println!("[task] waiting for the peer to join (Ctrl-C cancels the code)...");
        let link = wait_paired(
            &mut session.stack.link_rx,
            &mut session.events,
            &hooks.cancel,
            cfg.ttl_secs,
        )
        .await?;
        session.set_link(link.clone());
        print_paired(&link);
        hooks.fire_paired();
        let report = send_phase(&session, &hooks, &cfg.id_dir, &link, &paths).await??;
        println!("[task] transfer complete; disconnecting (the code is revoked)");
        Ok(TaskOutcome {
            files: report.files,
            bytes: report.bytes,
            elapsed: started.elapsed(),
            detail: "sent over the direct link".to_string(),
        })
    }
    .await;
    session.finish(flow.is_ok()).await;
    flow
}

/// The post-pairing send phase (probe + transfer), cancellable: Ctrl-C (or
/// a test's cancel channel) aborts the phase before or mid-transfer and
/// the flow ends as a LOCAL stop — the teardown then close-notifies the
/// waiting peer so it exits with the explicit peer-departure reason.
async fn send_phase(
    session: &Session,
    hooks: &TaskHooks,
    id_dir: &Path,
    link: &TaskLink,
    paths: &[PathBuf],
) -> Result<Result<Report>> {
    let flow = async {
        probe_and_print(&session.stack.pairing, link).await;
        drive_task_send(&session.stack.pairing, &session.key, id_dir, link, paths).await
    };
    Ok(tokio::select! {
        report = flow => report,
        _ = wait_cancelled(&hooks.cancel) => return Ok(Err(TaskCancelled.into())),
    })
}
/// it, wait for the joiner, probe, then serve receives behind the consent
/// prompt until one transfer completes (or the peer departs).
pub(crate) async fn run_code_recv(
    cfg: TaskRunConfig,
    mut hooks: TaskHooks,
) -> Result<TaskOutcome> {
    let started = Instant::now();
    let mut session = open_session(&cfg, true).await?;
    let flow = async {
        let (bound, mut receiver, code_slot) = session.bind_task_receiver(&cfg).await?;
        let echo_addr = session.bind_echo_responder()?;
        let blob = with_advertised(
            blob_candidates(local_candidates(bound), local_candidates(echo_addr)),
            &cfg.advertise,
            bound.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        let code = session
            .stack
            .pairing
            .create_task_code_with_ttl(Side::Receiver.direction(), &blob, cfg.ttl_secs)
            .await
            .map_err(task_error_message)?;
        session.code_id = Some(code.invite.code_id);
        if let Ok(mut slot) = code_slot.lock() {
            *slot = code.invite.code_id;
        }
        print_code_block(&code.invite.display, "recv", cfg.ttl_secs);
        if let Some(tx) = hooks.code_out.take() {
            let _ = tx.send(code.invite.display.clone());
        }
        println!("[task] waiting for the peer to join (Ctrl-C cancels the code)...");
        let link = wait_paired(
            &mut session.stack.link_rx,
            &mut session.events,
            &hooks.cancel,
            cfg.ttl_secs,
        )
        .await?;
        session.set_link(link.clone());
        print_paired(&link);
        hooks.fire_paired();
        probe_and_print(&session.stack.pairing, &link).await;
        let mut outcome = wait_receive(&mut receiver, &mut session.events, &hooks.cancel).await?;
        outcome.elapsed = started.elapsed();
        println!("[task] task complete; disconnecting (the code is revoked)");
        Ok(outcome)
    }
    .await;
    session.finish(flow.is_ok()).await;
    flow
}

/// `task join-send <CODE> <PATHS...>`: redeem the typed code as the
/// SENDER (explicit direction — a same-direction code is refused before
/// the server commits, without burning it), probe, send, disconnect.
pub(crate) async fn run_join_send(
    cfg: TaskRunConfig,
    mut hooks: TaskHooks,
    code: String,
    paths: Vec<PathBuf>,
) -> Result<TaskOutcome> {
    let started = Instant::now();
    let mut session = open_session(&cfg, false).await?;
    let flow = async {
        let echo_addr = session.bind_echo_responder()?;
        let dial = local_candidates(SocketAddr::from(([0, 0, 0, 0], 0)));
        let blob = with_advertised(
            blob_candidates(dial, local_candidates(echo_addr)),
            &cfg.advertise,
            echo_addr.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        println!("[task] joining the task code at the registry...");
        let paired = session
            .stack
            .pairing
            .join_task_code(&code, &blob, Side::Sender.direction())
            .await
            .map_err(task_error_message)?;
        session.code_id = Some(paired.code_id);
        let link = TaskLink::of(&paired, false);
        session.set_link(link.clone());
        print_paired(&link);
        hooks.fire_paired();
        let report = send_phase(&session, &hooks, &cfg.id_dir, &link, &paths).await??;
        println!("[task] transfer complete; disconnecting (the code is revoked)");
        Ok(TaskOutcome {
            files: report.files,
            bytes: report.bytes,
            elapsed: started.elapsed(),
            detail: "sent over the direct link".to_string(),
        })
    }
    .await;
    session.finish(flow.is_ok()).await;
    flow
}

/// `task join-recv <CODE>`: bind the receiver, redeem the typed code as
/// the RECEIVER, probe, serve behind the consent prompt until one transfer
/// completes (or the peer departs).
pub(crate) async fn run_join_recv(
    cfg: TaskRunConfig,
    mut hooks: TaskHooks,
    code: String,
) -> Result<TaskOutcome> {
    let started = Instant::now();
    let mut session = open_session(&cfg, false).await?;
    let flow = async {
        let (bound, mut receiver, code_slot) = session.bind_task_receiver(&cfg).await?;
        let echo_addr = session.bind_echo_responder()?;
        let blob = with_advertised(
            blob_candidates(local_candidates(bound), local_candidates(echo_addr)),
            &cfg.advertise,
            bound.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        println!("[task] joining the task code at the registry...");
        let paired = session
            .stack
            .pairing
            .join_task_code(&code, &blob, Side::Receiver.direction())
            .await
            .map_err(task_error_message)?;
        session.code_id = Some(paired.code_id);
        if let Ok(mut slot) = code_slot.lock() {
            *slot = paired.code_id;
        }
        let link = TaskLink::of(&paired, false);
        session.set_link(link.clone());
        print_paired(&link);
        hooks.fire_paired();
        probe_and_print(&session.stack.pairing, &link).await;
        let mut outcome = wait_receive(&mut receiver, &mut session.events, &hooks.cancel).await?;
        outcome.elapsed = started.elapsed();
        println!("[task] task complete; disconnecting (the code is revoked)");
        Ok(outcome)
    }
    .await;
    session.finish(flow.is_ok()).await;
    flow
}

// ---------------------------------------------------------------------------
// Tests: the real plane (a live v2 registry, real pairings, real QUIC
// transfers over loopback) — the same shape as ak-ffi's task tests.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    const E2E_BUDGET: Duration = Duration::from_secs(120);

    // ---- the real v2 registry (dedicated thread; never nests the test
    // runtime) ----------------------------------------------------------

    struct TestRegistry {
        addr: SocketAddr,
        registry_id: [u8; 32],
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl TestRegistry {
        fn spawn() -> TestRegistry {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("registry runtime");
                rt.block_on(async move {
                    let key = DeviceKeyPair::generate();
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("bind registry port");
                    let addr = listener.local_addr().expect("registry addr");
                    let store = ak_signal::server_v2::store::RegistryStore::open_in_memory()
                        .expect("in-memory registry store");
                    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                    ready_tx
                        .send((addr, key.public_key()))
                        .expect("test is waiting");
                    let server = ak_signal::server_v2::engine::serve_v2(
                        listener,
                        &key,
                        store,
                        ak_signal::server_v2::Limits::default(),
                        stop_rx,
                    );
                    tokio::select! {
                        _ = server => {}
                        _ = shutdown_rx => {
                            let _ = stop_tx.send(true);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                });
            });
            let (addr, registry_id) = ready_rx.recv().expect("registry boots");
            TestRegistry {
                addr,
                registry_id,
                shutdown: Some(shutdown_tx),
            }
        }
    }

    impl Drop for TestRegistry {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    // ---- fixtures --------------------------------------------------------

    fn identity_dir(_tag: &str) -> tempfile::TempDir {
        tempfile::tempdir().expect("temp identity dir")
    }

    fn registry_settings(registry: &TestRegistry) -> RegistrySettings {
        RegistrySettings {
            server: registry.addr,
            registry_id: registry.registry_id,
        }
    }

    fn sender_cfg(
        id: &tempfile::TempDir,
        registry: &TestRegistry,
        ttl: u32,
    ) -> TaskRunConfig {
        TaskRunConfig::new(id.path().to_path_buf(), registry_settings(registry), ttl)
    }

    fn receiver_cfg(
        id: &tempfile::TempDir,
        registry: &TestRegistry,
        out: &tempfile::TempDir,
        consent: ConsentInput,
        window: Duration,
    ) -> TaskRunConfig {
        let mut cfg = TaskRunConfig::new(
            id.path().to_path_buf(),
            registry_settings(registry),
            TASK_CODE_TTL_SECS,
        )
        .with_out_dir(out.path().to_path_buf())
        .expect("out dir");
        cfg.consent = consent;
        cfg.consent_window = window;
        cfg
    }

    /// Write a deterministic payload and return its hash (blake3 — the
    /// same family the engine's verification uses; byte-identical proof).
    fn write_payload(path: &Path, size: usize) -> blake3::Hash {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("payload parent");
        }
        let mut content = Vec::with_capacity(size);
        let mut state = 0x243F6A8885A308D3u64;
        while content.len() < size {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            content.extend_from_slice(&state.to_le_bytes());
        }
        content.truncate(size);
        std::fs::write(path, &content).expect("write payload");
        blake3::hash(&content)
    }

    fn hooks(
        code_out: Option<tokio::sync::oneshot::Sender<String>>,
        paired: Option<tokio::sync::oneshot::Sender<()>>,
        cancel: &tokio::sync::watch::Receiver<bool>,
    ) -> TaskHooks {
        TaskHooks {
            code_out,
            paired,
            cancel: cancel.clone(),
        }
    }

    fn no_cancel() -> tokio::sync::watch::Receiver<bool> {
        tokio::sync::watch::channel(false).1
    }

    /// An error is the Ctrl-C marker.
    fn is_cancelled(result: &Result<TaskOutcome>) -> bool {
        result
            .as_ref()
            .err()
            .is_some_and(|error| error.downcast_ref::<TaskCancelled>().is_some())
    }

    fn dir_entry_count(path: &Path) -> usize {
        std::fs::read_dir(path)
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    // ---- 1. code-send + join-recv: the Y branch, real bytes both ways ---

    #[tokio::test]
    async fn code_send_join_recv_e2e_accept_branch() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("cs-a");
        let id_b = identity_dir("cs-b");
        let out_b = identity_dir("cs-out");
        let src1 = id_a.path().join("alpha.bin");
        let src2 = id_a.path().join("nested").join("beta.bin");
        let hash1 = write_payload(&src1, 700_000);
        let hash2 = write_payload(&src2, 300_000);

        // The code-SEND side owns the payloads; the join-RECV side answers
        // the consent prompt with Y (scripted) and lands them.
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(run_code_send(
            sender_cfg(&id_a, &registry, 600),
            hooks(Some(code_tx), None, &no_cancel()),
            vec![src1, src2],
        ));
        let code = code_rx.await.expect("code is displayed");
        assert!(code.starts_with("AKI1-"), "registry display form: {code}");

        let joined = run_join_recv(
            receiver_cfg(
                &id_b,
                &registry,
                &out_b,
                ConsentInput::Scripted(true),
                CONSENT_WINDOW,
            ),
            hooks(None, None, &no_cancel()),
            code,
        )
        .await
        .expect("join-recv completes one task");
        assert_eq!(joined.files, 2);
        assert_eq!(joined.bytes, 1_000_000);

        let sent = owner.await.expect("owner task").expect("code-send ok");
        assert_eq!(sent.files, 2);
        assert_eq!(sent.bytes, 1_000_000);

        // Multi-root sends keep every root under its root name (the
        // engine's frozen manifest layout), so the files land as
        // <root-name>/<file-name>.
        assert_eq!(
            blake3::hash(&std::fs::read(out_b.path().join("alpha.bin").join("alpha.bin")).unwrap()),
            hash1
        );
        assert_eq!(
            blake3::hash(&std::fs::read(out_b.path().join("beta.bin").join("beta.bin")).unwrap()),
            hash2
        );
    }

    // ---- 2. code-recv + join-send: the reverse direction e2e ------------

    #[tokio::test]
    async fn code_recv_join_send_e2e_reverse() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("cr-a");
        let id_b = identity_dir("cr-b");
        let out_a = identity_dir("cr-out");
        let src = id_b.path().join("only.txt");
        let hash = write_payload(&src, 256_000);

        let cfg_owner = receiver_cfg(
            &id_a,
            &registry,
            &out_a,
            ConsentInput::Scripted(true),
            CONSENT_WINDOW,
        );
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let cancel_a = no_cancel();
        let owner = tokio::spawn(run_code_recv(
            cfg_owner.clone(),
            hooks(Some(code_tx), None, &cancel_a),
        ));
        let code = code_rx.await.expect("code is displayed");

        let cfg_join = sender_cfg(&id_b, &registry, 600);
        let sent =
            run_join_send(cfg_join, hooks(None, None, &no_cancel()), code, vec![src])
                .await
                .expect("join-send completes");
        assert_eq!(sent.files, 1);
        assert_eq!(sent.bytes, 256_000);

        let received = owner.await.expect("owner task").expect("code-recv ok");
        assert_eq!(received.files, 1);
        assert_eq!(received.bytes, 256_000);
        assert_eq!(blake3::hash(&std::fs::read(out_a.path().join("only.txt")).unwrap()), hash);
    }

    // ---- 3. the n branch: zero files land, the sender errors clearly ---

    #[tokio::test]
    async fn decline_branch_zero_disk_and_clear_sender_error() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("dn-a");
        let id_b = identity_dir("dn-b");
        let out_a = identity_dir("dn-out");
        let src = id_b.path().join("never.bin");
        write_payload(&src, 1000);

        let cfg_owner = receiver_cfg(
            &id_a,
            &registry,
            &out_a,
            ConsentInput::Scripted(false),
            CONSENT_WINDOW,
        );
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let owner = tokio::spawn(run_code_recv(
            cfg_owner,
            hooks(Some(code_tx), None, &cancel_rx),
        ));
        let code = code_rx.await.expect("code is displayed");

        let cfg_join = sender_cfg(&id_b, &registry, 600);
        let error = run_join_send(
            cfg_join,
            hooks(None, None, &no_cancel()),
            code,
            vec![src],
        )
        .await
        .expect_err("a declined offer fails the sender");
        assert!(
            error.to_string().contains("declined"),
            "the sender sees the declination: {error}"
        );
        assert_eq!(dir_entry_count(out_a.path()), 0, "nothing lands on disk");

        // The receiver keeps listening after a decline (wave-30: the
        // sender may retry); cancelling it ends the run as a local stop.
        cancel_tx.send(true).expect("flip cancel");
        let owner_result = owner.await.expect("owner task");
        assert!(is_cancelled(&owner_result), "local stop: {owner_result:?}");
        assert_eq!(dir_entry_count(out_a.path()), 0, "still nothing on disk");
    }

    // ---- 4. the 30 s window (injected short): expiry declines ---------

    #[tokio::test]
    async fn consent_window_timeout_declines() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("to-a");
        let id_b = identity_dir("to-b");
        let out_a = identity_dir("to-out");
        let src = id_b.path().join("late.bin");
        write_payload(&src, 1000);

        let cfg_owner = receiver_cfg(
            &id_a,
            &registry,
            &out_a,
            ConsentInput::WindowTimeout,
            Duration::from_millis(700),
        );
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let owner = tokio::spawn(run_code_recv(
            cfg_owner,
            hooks(Some(code_tx), None, &cancel_rx),
        ));
        let code = code_rx.await.expect("code is displayed");

        let cfg_join = sender_cfg(&id_b, &registry, 600);
        let error = run_join_send(
            cfg_join,
            hooks(None, None, &no_cancel()),
            code,
            vec![src],
        )
        .await
        .expect_err("an expired window declines");
        assert!(
            error.to_string().contains("declined"),
            "declined by the window: {error}"
        );
        assert_eq!(dir_entry_count(out_a.path()), 0);

        cancel_tx.send(true).expect("flip cancel");
        let owner_result = owner.await.expect("owner task");
        assert!(is_cancelled(&owner_result));
    }

    // ---- 5. direction conflict: the gate refuses and the code survives -

    #[tokio::test]
    async fn direction_conflict_refused_and_code_still_redeemable() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("dc-a");
        let id_b = identity_dir("dc-b");
        let id_c = identity_dir("dc-c");
        let out_b = identity_dir("dc-out");
        let owner_src = id_a.path().join("owner.bin");
        let owner_hash = write_payload(&owner_src, 120_000);
        let conflict_src = id_c.path().join("payload.bin");
        write_payload(&conflict_src, 10_000);

        // A SENDER-side code...
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(run_code_send(
            sender_cfg(&id_a, &registry, 600),
            hooks(Some(code_tx), None, &no_cancel()),
            vec![owner_src],
        ));
        let code = code_rx.await.expect("code is displayed");

        // ...joined by ANOTHER sender: the pre-commit gate vetoes it and
        // the redeem does not burn the code.
        let conflict = run_join_send(
            sender_cfg(&id_c, &registry, 600),
            hooks(None, None, &no_cancel()),
            code.clone(),
            vec![conflict_src],
        )
        .await
        .expect_err("same-direction join is refused");
        assert!(
            conflict.to_string().contains("unknown task code"),
            "the unified unavailable answer: {conflict}"
        );

        // The same code still pairs exactly once — as the RECEIVER: the
        // OWNER's payload arrives, not the refused joiner's.
        let received = run_join_recv(
            receiver_cfg(
                &id_b,
                &registry,
                &out_b,
                ConsentInput::Scripted(true),
                CONSENT_WINDOW,
            ),
            hooks(None, None, &no_cancel()),
            code,
        )
        .await
        .expect("the code survived the refused join");
        assert_eq!(received.files, 1);
        assert_eq!(received.bytes, 120_000);
        let owner_result = owner.await.expect("owner task");
        assert!(owner_result.is_ok(), "the owner's send completes: {owner_result:?}");
        assert_eq!(
            blake3::hash(&std::fs::read(out_b.path().join("owner.bin")).expect("landed")),
            owner_hash,
            "the OWNER's bytes arrived, not the refused joiner's"
        );
    }

    // ---- 6. the peer departs: this side exits with an explicit reason --

    #[tokio::test]
    async fn peer_disconnect_ends_wait_with_clear_reason() {
        let registry = TestRegistry::spawn();
        let id_a = identity_dir("pd-a");
        let id_b = identity_dir("pd-b");
        let out_a = identity_dir("pd-out");
        let src = id_b.path().join("will-not-arrive.bin");
        write_payload(&src, 400_000);

        // The owner waits receiving; the joiner pairs as the sender, then
        // disconnects locally (Ctrl-C shape) mid send-phase — its teardown
        // dials the owner's receiver with the TASK-CLOSE marker.
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(run_code_recv(
            receiver_cfg(
                &id_a,
                &registry,
                &out_a,
                ConsentInput::Scripted(true),
                CONSENT_WINDOW,
            ),
            hooks(Some(code_tx), None, &no_cancel()),
        ));
        let code = code_rx.await.expect("code is displayed");

        let (paired_tx, paired_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let joiner = tokio::spawn(run_join_send(
            sender_cfg(&id_b, &registry, 600),
            hooks(None, Some(paired_tx), &cancel_rx),
            code,
            vec![src],
        ));
        paired_rx.await.expect("joiner paired");

        let owner_error = tokio::time::timeout(E2E_BUDGET, async {
            cancel_tx.send(true).expect("joiner cancels");
            let joiner_result = joiner.await.expect("joiner task");
            assert!(is_cancelled(&joiner_result), "joiner stopped locally");
            owner.await.expect("owner task")
        })
        .await
        .expect("bounded")
        .expect_err("the owner exits non-zero on the peer's departure");
        assert!(
            owner_error.to_string().to_lowercase().contains("peer"),
            "the reason names the peer: {owner_error}"
        );
        assert_eq!(dir_entry_count(out_a.path()), 0, "no bytes ever landed");
    }

    // ---- unit-level: ttl resolution mirrors the invite bounds ----------

    #[test]
    fn ttl_resolution_maps_default_and_enforces_bounds() {
        assert_eq!(resolve_ttl(None).unwrap(), TASK_CODE_TTL_SECS);
        assert_eq!(resolve_ttl(Some(0)).unwrap(), TASK_CODE_TTL_SECS);
        assert_eq!(resolve_ttl(Some(600)).unwrap(), 600);
        assert!(resolve_ttl(Some(59)).is_err());
        assert!(resolve_ttl(Some(2_592_001)).is_err());
    }

    #[test]
    fn candidate_helpers_match_the_ffi_shape() {
        // LAN-first ordering with dedupe and v4-mapped normalization.
        let ordered = order_candidates(vec![
            "8.8.8.8:1".parse().unwrap(),
            "[::ffff:192.168.1.5]:2".parse().unwrap(),
            "192.168.1.5:2".parse().unwrap(),
            "127.0.0.1:3".parse().unwrap(),
        ]);
        assert_eq!(
            ordered,
            vec![
                "192.168.1.5:2".parse::<SocketAddr>().unwrap(),
                "127.0.0.1:3".parse().unwrap(),
                "8.8.8.8:1".parse().unwrap(),
            ]
        );
        // The blob caps dial candidates at 8 and echo candidates at 4.
        let dial: Vec<SocketAddr> = (1..=12)
            .map(|i| SocketAddr::from(([192, 168, 0, i], 7000)))
            .collect();
        let blob = blob_candidates(dial, Vec::new());
        assert_eq!(blob.len(), MAX_DIAL_CANDIDATES);
        // Local candidates always include loopback and carry the port.
        let locals = local_candidates("0.0.0.0:7100".parse().unwrap());
        assert!(locals.contains(&"127.0.0.1:7100".parse().unwrap()));
        assert!(locals.iter().all(|addr| addr.port() == 7100));
    }

    // ---- unit-level: the e2e flows are bounded by construction ---------

    /// The full battery above must also hold under the shared deadline —
    /// pinning that a wedged plane cannot hang a user terminal forever is
    /// the drivers' own budgets' job; this pins the wait helpers at least.
    #[tokio::test]
    async fn wait_paired_expires_at_the_code_ttl() {
        let (_tx, link_rx) = tokio::sync::watch::channel::<Option<TaskLink>>(None);
        let (_, cancel) = tokio::sync::watch::channel(false);
        let (_, mut events) = tokio::sync::broadcast::channel::<TaskEvent>(8);
        let error = wait_paired(&mut link_rx.clone(), &mut events, &cancel, 1)
            .await
            .expect_err("nothing pairs");
        assert!(error.to_string().contains("expired"), "{error}");
    }
}
