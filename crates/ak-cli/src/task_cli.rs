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
//! CR-P1-05 (review 2026-11-11 §4.5): both connectivity directions ride
//! THE socket that carries QUIC. The sender verbs (`code-send`,
//! `join-send`) carry `--stun` (production default, `""` disables): the
//! future-QUIC dial socket is bound and same-socket STUN-queried BEFORE
//! the claim/redeem, the reflexive address rides this side's endpoint
//! blob, and the send phase probes AND dials from that exact socket
//! (`probe_dial_round_on` + `SendSpec::adopt_socket`). The receiver verbs
//! additionally PUNCH: a send-only `try_clone` of the QUIC listener
//! socket sends probe-shaped packets to every advertised sender
//! candidate (150 ms cadence, ≤4 s window, stopped by the first accepted
//! connection) so a port-restricted-cone NAT on the receiver side
//! admits the sender's QUIC Initial.
//!
//! Registry resolution follows the `invite` family exactly
//! (`--server`/`--registry-fp` persisted into `<id-dir>/registry.pin`,
//! later invocations reuse the pin). Progress reuses the engine progress
//! printers (`[send]`/`[recv]` lines). CLI is one-shot: there is no
//! request-id surface; Ctrl-C is a LOCAL disconnect (revoke the code +
//! best-effort close-notify), and a peer/network departure prints an
//! explicit reason and exits non-zero.
#![forbid(unsafe_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use ak_engine::{
    receive_accepted_consent, send, ConsentAnswer, ConsentInfo, EngineError, Progress, ReceiveSpec,
    Report, SendSpec, TransferConsent, DECLINED_WIRE_CODE,
};
use ak_identity::pairing_txn::random_id16;
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{
    ClientConfig, OrchestratorConfig, PairingOrchestrator, PeerEndpoint, ProbeOutcome,
    RegistryClient, TaskDirection, TaskDisconnectReason, TaskError, TaskEvent, TaskPaired,
    TaskPairing, TaskPairingConfig, TASK_CODE_TTL_SECS,
};
// CR-P2-11 (review 2026-11-11 §4.5): the v1 connectivity plumbing this
// file used to duplicate against crates/ak-ffi/src/task.rs lives in ONE
// shared module now — ak-session `pairing::task_net` (a pure move: same
// bodies, same tunables, same rationale docs; the mirrored unit tests
// moved with them). This adapter keeps only the CLI-side wiring — stdout
// lines instead of events, the one-shot session state. The shared names
// are imported directly so the moved call sites read unchanged.
use ak_session::pairing::task_net::{
    bind_echo_responder_socket, bind_receiver_socket, blob_candidates, deliver_close_notify,
    drive_with_connect_budget, echo_responder_loop, handshake_failure_word,
    insert_reflexive_candidate, local_candidates, order_candidates, prefers_lan, probe_dial_round,
    probe_dial_round_on, probe_with_retry, punch_candidates, sender_reflexive_opener,
    staggered_dial_pass, windows_root_name_refusal, AttemptVerdict, ConnectMarks, PUNCH_INTERVAL,
    PUNCH_WINDOW, RECEIVER_BIND_TIMEOUT, REGISTRY_CONNECT_TIMEOUT, TASK_CLOSE_MESSAGE,
    TASK_CONNECT_PASSES, TASK_PROBE_OPTIONS, TASK_REDEEM_WINDOW_MS, TASK_SEND_IDLE,
    TASK_SETTLE_PAUSE,
};
// CR-P1-05: the shared sender-socket bind (aliased — the Session method
// of the same role wraps it with this adapter's stdout lines).
use ak_session::pairing::task_net::bind_sender_socket as task_net_bind_sender_socket;

use crate::invite::RegistrySettings;

// ---------------------------------------------------------------------------
// Tunables. CR-P2-11 (review 2026-11-11 §4.5): the connectivity tunables
// this file used to mirror against crates/ak-ffi/src/task.rs moved to the
// shared ak-session `pairing::task_net` module (imported above); only the
// CLI-only seams stay here.
// ---------------------------------------------------------------------------

/// Offer-level consent window: the terminal prompt waits this long, then
/// auto-DECLINES (never auto-accepts). 30 s per the frozen v1 model; the
/// field is injectable so tests exercise the expiry on a short window.
pub(crate) const CONSENT_WINDOW: Duration = Duration::from_secs(30);

/// Rate bound of the echo responder's "probe observed from …" diagnostic
/// line (CR-P1-02 改法第 3 条): a genuinely new source is named at most
/// this often, so spoofed source churn cannot flood stdout.
const ECHO_OBSERVE_LOG_WINDOW: Duration = Duration::from_secs(5);

/// Default STUN server of the `--stun` verbs (CR-P1-02 改法第 2 条 for
/// the receiver, CR-P1-05 for the sender — "STUN 地址与注册中心一起配置"):
/// the production coordination server's UDP STUN endpoint (docs/STATUS:
/// registry 7200 TLS + legacy signal 7100/7101 on 122.51.121.24). Active
/// by default so a NAT'd receiver publishes its reflexive candidate and a
/// NAT'd sender's dial socket gets its mapping with zero flags;
/// `--stun ""` explicitly disables the query (fail-open). MUST move to a
/// domain name when CR-P3-03 (single hostname for the coordination
/// server) lands — a hard-coded IP cannot survive a server migration.
const DEFAULT_STUN_ADDR: &str = "122.51.121.24:7101";

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
        /// STUN server ip:port for the SENDER's server-reflexive candidate
        /// (CR-P1-05): queried ON the future-QUIC dial socket before the
        /// code is claimed (the socket's NAT mapping survives into the
        /// QUIC dial, and the reflected public address rides this side's
        /// endpoint blob so the paired receiver can punch it). Default:
        /// the production coordination server; pass an EMPTY value ("")
        /// to disable the query.
        #[arg(long, default_value = DEFAULT_STUN_ADDR)]
        stun: String,
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
        /// STUN server ip:port for the receiver's server-reflexive
        /// candidate (CR-P1-02): queried ON the receiver's own UDP socket
        /// (the NAT mapping survives into the QUIC listener) and the
        /// reflected public address is advertised after the interface
        /// IPv4 block. Default: the production coordination server; pass
        /// an EMPTY value ("") to disable the query.
        #[arg(long, default_value = DEFAULT_STUN_ADDR)]
        stun: String,
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
        /// STUN server ip:port for the SENDER's server-reflexive candidate
        /// (CR-P1-05): queried ON the future-QUIC dial socket BEFORE the
        /// redeem (the blob the redeem registers carries the reflexive
        /// address; the socket's NAT mapping survives into the QUIC dial
        /// — the probe round and every dial attempt leave from it).
        /// Default: the production coordination server; pass an EMPTY
        /// value ("") to disable the query.
        #[arg(long, default_value = DEFAULT_STUN_ADDR)]
        stun: String,
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
        /// STUN server ip:port for the receiver's server-reflexive
        /// candidate (CR-P1-02): queried ON the receiver's own UDP socket
        /// (the NAT mapping survives into the QUIC listener) and the
        /// reflected public address is advertised after the interface
        /// IPv4 block. Default: the production coordination server; pass
        /// an EMPTY value ("") to disable the query.
        #[arg(long, default_value = DEFAULT_STUN_ADDR)]
        stun: String,
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
            stun,
            server,
            registry_fp,
        } => {
            let ttl = resolve_ttl(ttl_secs)?;
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, ttl);
            cfg.advertise = advertise;
            cfg.stun = resolve_stun(&stun)?;
            check_send_paths(&paths)?;
            run_and_report(run_code_send(cfg, TaskHooks::production(), paths)).await
        }
        TaskCommand::CodeRecv {
            out,
            ttl_secs,
            advertise,
            stun,
            server,
            registry_fp,
        } => {
            let ttl = resolve_ttl(ttl_secs)?;
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, ttl);
            cfg.advertise = advertise;
            cfg.stun = resolve_stun(&stun)?;
            let cfg = cfg.with_out_dir(out)?;
            run_and_report(run_code_recv(cfg, TaskHooks::production())).await
        }
        TaskCommand::JoinSend {
            code,
            paths,
            advertise,
            stun,
            server,
            registry_fp,
        } => {
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, TASK_CODE_TTL_SECS);
            cfg.advertise = advertise;
            cfg.stun = resolve_stun(&stun)?;
            check_send_paths(&paths)?;
            run_and_report(run_join_send(cfg, TaskHooks::production(), code, paths)).await
        }
        TaskCommand::JoinRecv {
            code,
            out,
            advertise,
            stun,
            server,
            registry_fp,
        } => {
            let registry = crate::invite::resolve_registry(id, server, registry_fp)?;
            let mut cfg = TaskRunConfig::new(id.to_path_buf(), registry, TASK_CODE_TTL_SECS);
            cfg.advertise = advertise;
            cfg.stun = resolve_stun(&stun)?;
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

/// `--stun` resolution of the verbs that carry it (CR-P1-02 改法第 2 条
/// receiver side; CR-P1-05 sender side): the
/// clap default already carries the production endpoint, so the common
/// path is a valid address; an explicit EMPTY value is the operator's
/// opt-out (no query, fail-open — the same absent/empty semantics as the
/// FFI spec's `stun_addr`), and anything non-empty but unparseable fails
/// the run before any code is claimed.
fn resolve_stun(text: &str) -> Result<Option<SocketAddr>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<SocketAddr>()
        .map(Some)
        .with_context(|| format!("--stun is not an ip:port address: {trimmed:?}"))
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
    /// STUN server of the receiver verbs' `--stun` (CR-P1-02 改法第 2
    /// 条): queried ON the receiver's own UDP socket, so the reflected
    /// address rides the advertised candidates and the NAT mapping
    /// survives into the QUIC listener. CR-P1-05 (review 2026-11-11
    /// §4.5): the SENDER verbs carry it too — queried ON the future-QUIC
    /// dial socket before the claim/redeem, so the reflexive address
    /// rides the sender's own blob and the dial leaves from the mapped
    /// port. `None` = no query, fail-open — the DEFAULT of embedded
    /// tests (production verbs resolve `--stun`, empty string disables).
    pub(crate) stun: Option<SocketAddr>,
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
            stun: None,
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
/// (journal + trust book under `id_dir`), one `TaskPairing`, the
/// connection keeper (wave 35, common ak-session component — 20 s ping +
/// capped-backoff reconnect, R36-P1-01/R36-P1-02: without it the registry
/// v2 server cuts an idle owner after 30 s of inbound silence while the
/// CLI still displays a live task code), plus the task-aware owner loop
/// behind the pre-commit redeem-offer gate (code-* verbs only — a joiner
/// answers no redeems). All four task commands share exactly one stack
/// per run.
struct TaskStack {
    pairing: TaskPairing,
    owner_loop: Option<tokio::task::JoinHandle<()>>,
    keeper: Option<tokio::task::JoinHandle<()>>,
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
    /// Most recent probe source OBSERVED by this session's echo responder
    /// (CR-P1-02 改法第 3 条), normalized to plain-family form — the
    /// receiver side's raw hole-punch observation, consumed by
    /// [`close_notify_peer`] (the receiver's one outbound dial role). One
    /// session is one task run, so the slot is per-pairing scoped by
    /// construction.
    echo_observed: Arc<StdMutex<Option<SocketAddr>>>,
    /// The connection lock heartbeat refresher (R36-P2-04).
    lock_heartbeat: Option<tokio::task::JoinHandle<()>>,
    /// Stops the connection keeper at teardown (the keeper additionally
    /// ends itself when the client reaches Closed).
    keeper_stop: tokio::sync::watch::Sender<bool>,
    /// This run's hold on the identity's control connection; released
    /// (file removed) when the session ends or the build failed midway.
    lock: Option<ConnectionLock>,
    code_id: Option<[u8; 16]>,
    link: Option<TaskLink>,
    /// CR-P1-05 (review 2026-11-11 §4.5): the SENDER flow's adopted
    /// future-QUIC dial socket — bound (and same-socket STUN-queried)
    /// BEFORE the claim/redeem, kept here until the send phase: the
    /// probe round runs on a `try_clone` of it and every engine dial
    /// attempt adopts it, so the NAT mapping the STUN query opened
    /// carries into the QUIC Initial. `None` on receiver flows and when
    /// the bind failed (fail-open: the engine self-binds as before).
    sender_socket: Option<std::net::UdpSocket>,
    /// The pairing-completion publisher (the owner loop holds a clone; the
    /// joiner flows publish through [`Session::set_link`]).
    link_tx: tokio::sync::watch::Sender<Option<TaskLink>>,
}

/// Build the run's registry stack: real pinned-TLS client, orchestrator
/// with the task redeem window, `TaskPairing`, the connection keeper on
/// the shared client, and — for code owners — the owner loop behind the
/// redeem-offer gate (a same-direction join is vetoed BEFORE the server
/// commits, so the code's single redeem survives the refusal).
async fn open_session(cfg: &TaskRunConfig, owner: bool) -> Result<Session> {
    // R36-P2-04 (CLI side): one live control connection per identity per
    // machine. The registry REPLACES an older same-device connection, so
    // a second local process on the same id-dir would silently kick this
    // one (and be kicked back); refuse loudly instead. The guard releases
    // itself if anything below fails.
    let lock = acquire_connection_lock(&cfg.id_dir)?;
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
    // The connection keeper (R36-P1-01): keepalive pings inside the
    // server's 30 s silence budget + reconnect with capped backoff +
    // black-hole recovery, so a code owner waiting on "the peer to join"
    // stays online for the whole code TTL. The common ak-session
    // component — the same implementation the daemon uses.
    let (keeper_stop, keeper_stop_rx) = tokio::sync::watch::channel(false);
    let keeper = pairing
        .orchestrator()
        .client()
        .spawn_connection_keeper(keeper_stop_rx);
    let lock_heartbeat = spawn_connection_lock_heartbeat(&lock);
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
            keeper: Some(keeper),
            link_rx,
        },
        events: pairing.subscribe(),
        receiver: None,
        echo: None,
        echo_observed: Arc::new(StdMutex::new(None)),
        lock_heartbeat: Some(lock_heartbeat),
        keeper_stop,
        lock: Some(lock),
        code_id: None,
        link: None,
        sender_socket: None,
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
    /// CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): bind
    /// the SENDER flow's future-QUIC dial socket — dual-stack first, the
    /// optional STUN reflexive query ON THAT SAME SOCKET (same 2 s
    /// fail-open budget as the receiver) — stash it for the send phase
    /// ([`Session::sender_socket`]) and return `(bound, reflexive)`.
    /// The caller MUST insert the reflexive address into its derived
    /// candidate list BEFORE the claim/redeem (the registered blob is
    /// the paired receiver's punch target). A bind failure fails open
    /// to the historical port-0 advertisement + engine self-bind (the
    /// bounded reason prints; the sender, unlike the receiver, does not
    /// NEED a pre-bound socket to function).
    async fn bind_sender_socket(
        &mut self,
        stun: Option<SocketAddr>,
    ) -> Result<(SocketAddr, Option<SocketAddr>)> {
        let sock = match task_net_bind_sender_socket(stun).await {
            Ok(sock) => sock,
            Err(reason) => {
                println!("[task] sender socket bind failed ({reason}); dialing without adoption");
                return Ok((SocketAddr::from(([0, 0, 0, 0], 0)), None));
            }
        };
        if let Some(reason) = &sock.bind_fallback {
            println!(
                "[task] dual-stack bind unavailable ({reason}); \
                 falling back to the IPv4-only bind"
            );
        }
        match sock.reflexive {
            Some(reflexive) => println!(
                "[task] stun reflexive {reflexive} (queried on the sender socket; \
                 the QUIC dial adopts this socket)"
            ),
            None if sock.stun_error.is_some() => println!(
                // Deliberately avoids the word "advertising" (the itests
                // grep for it to find the candidate list lines).
                "[task] stun query failed ({}); local candidates only",
                sock.stun_error.as_deref().unwrap_or_default()
            ),
            None => {} // no server configured: nothing to report
        }
        let bound = sock
            .socket
            .local_addr()
            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        self.sender_socket = Some(sock.socket);
        Ok((bound, sock.reflexive))
    }

    /// Bind the probe echo responder (derived candidates append its port
    /// last; see `blob_candidates`): the shared socket bind + loop live in
    /// `task_net` (dual-stack first, CR-P1-02 改法第 1 条 — the responder
    /// must answer probes aimed at the advertised IPv6 candidates too;
    /// hosts without a clearable V6ONLY fall back to the historical v4
    /// wildcard socket). Since 改法第 3 条 the responder also OBSERVES:
    /// every genuine probe's source lands in [`Session::echo_observed`],
    /// and the CLI-side "probe observed from …" stdout line is injected
    /// as the loop's observation callback — rate-limited here so spoofed
    /// source churn cannot flood stdout (the print site stays in the CLI:
    /// the itests grep these strings).
    fn bind_echo_responder(&mut self) -> Result<SocketAddr> {
        let (socket, addr) = bind_echo_responder_socket().map_err(anyhow::Error::msg)?;
        let observed = Arc::clone(&self.echo_observed);
        let mut last_logged: Option<(SocketAddr, Instant)> = None;
        self.echo = Some(tokio::spawn(async move {
            echo_responder_loop(socket, observed, move |source_plain| {
                let now = Instant::now();
                let log_it = match last_logged {
                    Some((logged_at_addr, logged_at)) => {
                        logged_at_addr != source_plain
                            && now.duration_since(logged_at) >= ECHO_OBSERVE_LOG_WINDOW
                    }
                    None => true,
                };
                if log_it {
                    println!("[task] probe observed from {source_plain}");
                    last_logged = Some((source_plain, now));
                }
            })
            .await;
        }));
        Ok(addr)
    }

    /// Bind the resident receiver: ONE wildcard endpoint serving a LOOP of
    /// connections until the task ends. Admission binds the pairing: the
    /// accept `authorize` admits ONLY the transcript-verified peer
    /// fingerprint, so a stranger can neither deliver an offer nor consume
    /// the receiver; handshake failures are per-connection (keep
    /// listening), a DECLINED offer ends only that connection. Returns
    /// the bound address TOGETHER with the STUN server-reflexive address
    /// observed on the same socket (`None` when no `--stun` was resolved
    /// or the query failed open — 改法第 2 条).
    async fn bind_task_receiver(
        &mut self,
        cfg: &TaskRunConfig,
    ) -> Result<(
        (SocketAddr, Option<SocketAddr>),
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
            stun: cfg.stun,
        });
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(4);
        let slot = Arc::clone(&code_slot);
        self.receiver = Some(tokio::spawn(async move {
            run_task_receiver(ctx, slot, addr_tx, events_tx).await;
        }));
        match tokio::time::timeout(RECEIVER_BIND_TIMEOUT, addr_rx).await {
            Ok(Ok(bound_and_reflexive)) => Ok((bound_and_reflexive, events_rx, code_slot)),
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
        if let Some(task) = self.lock_heartbeat.take() {
            task.abort();
        }
        if !completed {
            if let Some(link) = self.link.clone() {
                // 改法第 3 条: hand the echo responder's most recent
                // observed probe source to the close-notify dial (it
                // leads the order — see close_notify_peer).
                let observed = self
                    .echo_observed
                    .lock()
                    .map(|slot| *slot)
                    .unwrap_or_else(|poisoned| *poisoned.into_inner());
                close_notify_peer(&self.key, &link, observed).await;
            }
        }
        if let Some(task) = self.stack.owner_loop.take() {
            task.abort();
        }
        if let Some(task) = self.stack.keeper.take() {
            let _ = self.keeper_stop.send(true);
            task.abort();
        }
        if let Some(code_id) = self.code_id {
            let pairing = self.stack.pairing.clone();
            let _ = tokio::time::timeout(Duration::from_secs(3), pairing.disconnect(code_id)).await;
        }
        let client = self.stack.pairing.orchestrator().client().clone();
        let _ = tokio::time::timeout(Duration::from_secs(1), client.close()).await;
        // Release the identity's control connection (the last Arc of the
        // terminal input source drops with the receiver's ctx; the
        // detached stdin thread dies with the process).
        if let Some(lock) = self.lock.take() {
            drop(lock);
        }
    }
}

// ---------------------------------------------------------------------------
// The per-identity control-connection lock (R36-P2-04, CLI side)
// ---------------------------------------------------------------------------

/// Lock file under `id_dir` marking which local process holds this
/// identity's registry control connection (`pid` + heartbeat `unix-ms`).
const CONNECTION_LOCK_FILE: &str = "registry-connection.lock";
/// While holding, the session refreshes the heartbeat at this cadence.
const CONNECTION_LOCK_REFRESH: Duration = Duration::from_secs(5);
/// A heartbeat older than this means the holder crashed (or suspended):
/// the lock is breakable by the next run. Generous versus the refresh
/// cadence, short versus a user's patience.
const CONNECTION_LOCK_STALE: Duration = Duration::from_secs(15);

/// One parsed lock file (`pid heartbeat-ms`).
pub(crate) struct LockHolder {
    pid: u32,
    beat_ms: u64,
}

pub(crate) fn parse_lock_holder(text: &str) -> Option<LockHolder> {
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let beat_ms = parts.next()?.parse().ok()?;
    Some(LockHolder { pid, beat_ms })
}

pub(crate) fn write_lock_file(path: &Path) {
    let _ = std::fs::write(
        path,
        format!("{}\n{}\n", std::process::id(), crate::now_unix_ms()),
    );
}

/// The run's hold on the identity's control connection: acquired before
/// the registry dial, released (file removed) on drop — including the
/// failure paths inside [`open_session`].
#[derive(Debug)]
pub(crate) struct ConnectionLock {
    path: PathBuf,
}

/// Claim the identity's control connection for THIS process
/// ([`CONNECTION_LOCK_FILE`]). A live holder (fresh heartbeat, different
/// PID) refuses loudly — the registry would otherwise let the two
/// processes silently kick each other's connection forever. A stale lock
/// (crashed/suspended holder, heartbeat past [`CONNECTION_LOCK_STALE`])
/// or our own PID (same process) is broken/refreshed. Best-effort by
/// design: no atomic file locking, only the documented pid+heartbeat
/// protocol.
pub(crate) fn acquire_connection_lock(id_dir: &Path) -> Result<ConnectionLock> {
    let path = id_dir.join(CONNECTION_LOCK_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Some(holder) = parse_lock_holder(&text) {
            let age = crate::now_unix_ms().saturating_sub(holder.beat_ms);
            if holder.pid != std::process::id() && age < CONNECTION_LOCK_STALE.as_millis() as u64 {
                bail!(
                    "this identity is already holding a control connection (process {}); \
                     reuse that process or pass a different --id-dir",
                    holder.pid
                );
            }
        }
        // Unparseable, our own PID, or stale: breakable below.
    }
    write_lock_file(&path);
    Ok(ConnectionLock { path })
}

/// Keep the heartbeat fresh while the session lives, so a later run on
/// the same id-dir sees a LIVE holder (a crashed process stops
/// refreshing and its lock goes stale).
pub(crate) fn spawn_connection_lock_heartbeat(
    lock: &ConnectionLock,
) -> tokio::task::JoinHandle<()> {
    let path = lock.path.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CONNECTION_LOCK_REFRESH).await;
            write_lock_file(&path);
        }
    })
}

impl Drop for ConnectionLock {
    fn drop(&mut self) {
        // Remove the file only when it still names THIS process: a run
        // that broke our stale lock (we suspended past the stale window)
        // owns it now and must not be unlocked by our exit.
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if parse_lock_holder(&text).is_some_and(|holder| holder.pid == std::process::id()) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
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
    /// `--stun` of the receiver verbs (改法第 2 条): queried ON the
    /// receiver's own socket before it is handed to QUIC; `None` skips
    /// the query (fail-open).
    stun: Option<SocketAddr>,
}

/// The resident receiver loop (wave-29 P1-04/P2-05 semantics, the FFI
/// shape): one bound port, a loop of connections, fingerprint-bound
/// admission, per-connection consent, decline keeps listening, done is
/// terminal.
async fn run_task_receiver(
    ctx: Arc<ReceiverCtx>,
    code_slot: Arc<StdMutex<[u8; 16]>>,
    first_addr: tokio::sync::oneshot::Sender<(SocketAddr, Option<SocketAddr>)>,
    events: tokio::sync::mpsc::Sender<ReceiverEvent>,
) {
    let identity = make_self_signed(&ctx.key);
    // CR-P1-02 改法第 1 条 + 第 2 条: the socket is created explicitly —
    // dual-stack `[::]` first (C1; v4 wildcard fallback for hosts without
    // IPv6, reported on stdout as before) — the optional STUN reflexive
    // query runs ON IT (same socket, so the NAT mapping survives), and
    // only then is it handed to QUIC via `bind_with_socket`. Every
    // optional step fails open and says so on one bounded stdout line.
    let sock = match bind_receiver_socket(ctx.stun).await {
        Ok(sock) => sock,
        Err(reason) => {
            println!("[task] receiver socket bind failed ({reason})");
            return; // the dropped oneshot fails the bind wait
        }
    };
    if let Some(reason) = &sock.bind_fallback {
        println!(
            "[task] dual-stack bind unavailable ({reason}); \
             falling back to the IPv4-only bind"
        );
    }
    match sock.reflexive {
        Some(reflexive) => {
            println!("[task] stun reflexive {reflexive} (queried on the receiver socket)")
        }
        None if sock.stun_error.is_some() => println!(
            // Deliberately avoids the word "advertising" (the itests grep
            // for it to find the real candidate list line).
            "[task] stun query failed ({}); local candidates only",
            sock.stun_error.as_deref().unwrap_or_default()
        ),
        None => {} // no server configured: nothing to report
    }
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 3 条, receiver half): keep a
    // SEND-ONLY `try_clone` of the QUIC socket (duplicate handle, same
    // local port) BEFORE it moves into the endpoint — the punch task
    // below sends the peer-opening packets FROM the QUIC socket's port,
    // so a port-restricted-cone NAT on this side admits the sender's
    // QUIC Initial. The clone never receives (it would compete with
    // quinn for the socket queue).
    let punch_clone = sock.socket.try_clone().ok();
    let endpoint = match ak_net::DirectEndpoint::bind_with_socket(sock.socket, &identity).await {
        Ok(endpoint) => endpoint,
        Err(_) => return, // the dropped oneshot fails the bind wait
    };
    // The punch task: waits for THIS side's pairing (the session's link
    // watch flips when `set_link` runs), then fires bounded punches at
    // the SENDER's advertised candidates — concurrently with the
    // sender's dial, every PUNCH_INTERVAL for at most PUNCH_WINDOW,
    // stopped early by the first accepted data-plane connection (the
    // flag below). Bounded and detached: it never blocks the accept
    // loop, and the window ends it even if nothing else does.
    let punch_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(punch_socket) = punch_clone {
        let mut punch_link_rx = ctx.link_rx.clone();
        let punch_stop_loop = Arc::clone(&punch_stop);
        tokio::spawn(async move {
            loop {
                let link_now = punch_link_rx.borrow_and_update().clone();
                if let Some(link) = link_now {
                    let candidates = link.peer.candidates.clone();
                    println!(
                        "[task] punching {} advertised sender candidate(s) every {} ms for up to {} s",
                        candidates.len(),
                        PUNCH_INTERVAL.as_millis(),
                        PUNCH_WINDOW.as_secs(),
                    );
                    punch_candidates(punch_socket, &candidates, &punch_stop_loop, |_| {}).await;
                    return;
                }
                if punch_link_rx.changed().await.is_err() {
                    return; // the session's link publisher is gone
                }
            }
        });
    }
    let bound = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let _ = first_addr.send((bound, sock.reflexive));
    println!(
        "[task] receiver bound {} ({})",
        bound,
        // Tracked at bind time (C1 read this off the endpoint; adopted
        // sockets always report is_dual_stack() == false — see
        // ReceiverSocket::dual_stack).
        if sock.dual_stack {
            "dual-stack: IPv6 and IPv4 peers"
        } else {
            "IPv4-only"
        }
    );
    let printer = crate::progress_printer("recv");
    // The session's ONE managed terminal input source (R36-P2-01): a
    // single detached reader thread serves every consent prompt of this
    // run; expired prompts' answers are discarded by
    // [`TerminalInput::answer`], never left to answer the next offer.
    let terminal = match ctx.consent {
        ConsentInput::Terminal => Some(TerminalInput::start()),
        _ => None,
    };
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
        // CR-P1-05: the FIRST successful data-plane connection stops the
        // punch loop early — from here on the QUIC connection itself
        // keeps the mapping hot.
        punch_stop.store(true, std::sync::atomic::Ordering::Relaxed);
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
            let consent_terminal = terminal.clone();
            Arc::new(move |info: &ConsentInfo| {
                let Some(link) = link_rx.borrow().clone() else {
                    return ConsentAnswer::Now(false);
                };
                if link.peer_fp != info.sender_fingerprint {
                    return ConsentAnswer::Now(false);
                }
                // CR-P3-02 (review 2026-11-11), mirroring the FFI gate:
                // on a Windows receiver, refuse an offer whose root name
                // cannot exist here BEFORE the prompt — no question asked,
                // no Accept answered, nothing written; the peer hears the
                // standard DECLINED and the reason is printed locally.
                if let Some(reason) = windows_root_name_refusal(&info.root_name) {
                    println!("[task] offer refused before accept: {reason}; still listening");
                    return ConsentAnswer::Now(false);
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                spawn_consent_decider(&ctx, info, &consent_out_dir, consent_terminal.clone(), tx);
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
                let _ = events
                    .send(ReceiverEvent::PeerGone(
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

// ---------------------------------------------------------------------------
// The session terminal input source (R36-P1-04 / R36-P2-01)
// ---------------------------------------------------------------------------

/// One stdin read outcome as the reader thread reports it. EOF (`Ok(0)`)
/// and I/O failures are NEVER "an empty answer" — a prompt that could not
/// read a decision declines.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StdinLine {
    /// A line was read (`Ok(n > 0)`); `complete` = it ended in a real
    /// newline (a final fragment at EOF does not).
    Line { text: String, complete: bool },
    /// End of input: closed stdin, an empty pipe, a finished heredoc.
    Eof,
    /// The read failed (bounded reason string).
    Io(String),
}

impl StdinLine {
    /// The consent verdict of one read (R36-P1-04): EOF, I/O failure, an
    /// incomplete final line (input ended without Enter) and unrecognized
    /// text all DECLINE; only a complete line that is empty (a genuine
    /// Enter), `y` or `yes` accepts.
    fn consent_decision(&self) -> bool {
        match self {
            StdinLine::Eof | StdinLine::Io(_) => false,
            StdinLine::Line { text, complete } => {
                if !complete {
                    return false; // no real newline was ever read
                }
                matches!(text.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
            }
        }
    }
}

/// One line as the reader delivers it, stamped when it became available
/// (the freshness boundary of a consent prompt: input older than the
/// prompt answered a DEAD prompt).
struct TerminalLine {
    at: Instant,
    kind: StdinLine,
}

/// The session-managed terminal input source (R36-P2-01): ONE plain OS
/// thread reads stdin for the whole session — not one unowned blocking
/// read per Offer. Each consent prompt ([`TerminalInput::answer`])
/// discards input that predates it (the late answer to an EXPIRED
/// prompt, or stray input), so a stale line can never silently accept
/// the next offer, and no per-Offer reader tasks are left behind.
///
/// The reader is deliberately a detached OS thread rather than a Tokio
/// blocking task: a parked `read_line` cannot be cancelled, and a
/// blocking-pool task would hold the runtime (and thus process exit)
/// open after the command already printed its summary; a detached
/// thread simply dies with the process. When the session ends, the
/// channel receiver drops, the thread's next send fails and it exits.
struct TerminalInput {
    lines: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<TerminalLine>>,
}

impl TerminalInput {
    /// Start the session's one stdin reader thread. If the thread cannot
    /// be spawned the source fails CLOSED: the failed spawn drops the
    /// closure (and with it the sender), the channel then reports
    /// "closed" and every prompt declines instead of silently waiting
    /// out its window.
    fn start() -> Arc<TerminalInput> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let _ = std::thread::Builder::new()
            .name("ak-task-stdin".to_string())
            .spawn(move || Self::read_loop(tx));
        Arc::new(TerminalInput {
            lines: tokio::sync::Mutex::new(rx),
        })
    }

    /// The reader body: stdin lines → channel, forever. EOF and I/O
    /// errors are delivered once (so the pending prompt declines with the
    /// honest reason), then the thread ends.
    fn read_loop(tx: tokio::sync::mpsc::Sender<TerminalLine>) {
        let stdin = std::io::stdin();
        loop {
            let mut text = String::new();
            let kind = match stdin.read_line(&mut text) {
                Ok(0) => StdinLine::Eof,
                Ok(_) => StdinLine::Line {
                    complete: text.ends_with('\n'),
                    text,
                },
                Err(error) => StdinLine::Io(error.to_string()),
            };
            let terminal = matches!(kind, StdinLine::Eof | StdinLine::Io(_));
            let line = TerminalLine {
                at: Instant::now(),
                kind,
            };
            if tx.blocking_send(line).is_err() {
                return; // the session ended
            }
            if terminal {
                return; // stdin is finished for good
            }
        }
    }

    /// Ask the CURRENT consent question and wait one window:
    ///
    /// * input already queued BEFORE this question (the late answer to an
    ///   expired prompt, stray keystrokes) is discarded — it can never
    ///   answer this offer (R36-P2-01);
    /// * EOF, I/O failure, an incomplete final line and an expired window
    ///   all DECLINE (R36-P1-04);
    /// * only a genuinely read Enter (an empty complete line), `y` or
    ///   `yes` accepts.
    async fn answer(&self, window: Duration) -> bool {
        let started = Instant::now();
        let mut lines = self.lines.lock().await;
        while lines.try_recv().is_ok() {
            // Answered a dead prompt (or arrived before this one): drop.
        }
        let deadline = started + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                print_expiry_note(window);
                return false;
            }
            let line = match tokio::time::timeout(remaining, lines.recv()).await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    println!("[task] the terminal input closed: declined");
                    return false;
                }
                Err(_elapsed) => {
                    print_expiry_note(window);
                    return false;
                }
            };
            if line.at < started {
                // Read completed before this prompt existed (it was parked
                // in the sender when the prompt started): a dead prompt's
                // answer — drop it and keep waiting.
                continue;
            }
            let verdict = line.kind.consent_decision();
            if !verdict {
                match &line.kind {
                    StdinLine::Eof => {
                        println!("[task] stdin closed before an answer: declined");
                    }
                    StdinLine::Io(reason) => {
                        println!("[task] cannot read the terminal ({reason}): declined");
                    }
                    StdinLine::Line {
                        complete: false, ..
                    } => {
                        println!("[task] input ended without Enter: declined");
                    }
                    StdinLine::Line { text, .. }
                        if !matches!(text.trim().to_ascii_lowercase().as_str(), "n" | "no") =>
                    {
                        println!("[task] unrecognized answer {:?}: declined", text.trim());
                    }
                    _ => {} // an explicit n/no declines silently
                }
            }
            return verdict;
        }
    }
}

/// The inline-prompt expiry note (a fresh line first, because the prompt
/// has no trailing newline).
fn print_expiry_note(window: Duration) {
    println!();
    println!("[task] no answer within {}s: declined", window.as_secs());
}

/// Print the consent banner and decide: the terminal branch asks the
/// session's managed input source ([`TerminalInput::answer`] declines on
/// EOF/I/O/expiry — never auto-accepts); the scripted branches keep the
/// engine's pending path exercised.
fn spawn_consent_decider(
    ctx: &ReceiverCtx,
    info: &ConsentInfo,
    out_dir: &Path,
    terminal: Option<Arc<TerminalInput>>,
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
                let decision = match terminal {
                    Some(source) => source.answer(window).await,
                    None => {
                        // No managed source (the reader thread could not
                        // be spawned): fail closed.
                        println!("[task] the terminal input is unavailable: declined");
                        false
                    }
                };
                let _ = decide.send(decision);
            }
        }
    })
}

// CR-P2-11: candidate enumeration/ordering, reflexive insertion and
// the observed-dial-order helpers (local_candidates, filter_and_order_,
// insert_reflexive_, order_candidates, observed_dial_head,
// observed_first_order) moved to ak-session `pairing::task_net`.

/// Assemble the endpoint-blob candidate list: the dial candidates (the
/// receiver's real port first), then the probe echo port, deduped, capped,
/// and shrunk until the blob fits the wire bound.
/// Prepend the operator's `--advertise` IPs (paired with `port`) to a
/// built blob, deduped (wave-32: NAT escape hatch — see TaskRunConfig).
fn with_advertised(
    blob: Vec<SocketAddr>,
    advertise: &[std::net::IpAddr],
    port: u16,
) -> Vec<SocketAddr> {
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
    // The prepended entries can push a v6-heavy list over the 512-byte
    // wire bound (CR-P1-02 leg 1 grew realistic blob sizes): shrink until
    // it fits — the same belt-and-braces loop as `blob_candidates`, and
    // the caps keep it off the common path.
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
/// and revoked all answer PAIR_UNAVAILABLE) — the anyhow adapter over the
/// shared `task_net::task_error_message` vocabulary (CR-P2-11; the
/// underlying `TaskError` rides the chain so `--debug` printing keeps the
/// full cause).
fn task_error_message(error: TaskError) -> anyhow::Error {
    let text = ak_session::pairing::task_net::task_error_message(&error);
    anyhow::Error::new(error).context(text)
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// Run the honest reachability probe and print its verdict (an all-failed
/// probe does not abort: it proves UDP liveness, not QUIC dialability).
async fn probe_and_print(pairing: &TaskPairing, link: &TaskLink) {
    let outcome = probe_with_retry(pairing, &link.peer).await;
    match &outcome {
        ProbeOutcome::Success { address } => {
            let lan = if prefers_lan(address) {
                "lan"
            } else {
                "internet"
            };
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

/// One task send: run the shared candidate dial pass
/// (`task_net::staggered_dial_pass`, CR-P3-09: reflexive/observed
/// addresses strictly first — 保序私网候选在慢路径 — Happy-Eyeballs
/// stagger on the self-bound path, strictly sequential on the adopted
/// socket) under the split connect budget; a post-connect failure fails
/// the send (another address cannot fix it); a DECLINED answer is
/// surfaced verbatim; purely connect-level failure of the whole list
/// gets [`TASK_CONNECT_PASSES`] passes (the just-paired peer's admission
/// bookkeeping can trail the pairing by a beat). Since CR-P1-02 改法第 3
/// 条 the connect phase OPENS with one simultaneous-probe round: when it
/// observes an answering address, that address leads the dial order
/// under the bounded observed-candidate budget; with no observation the
/// round is invisible and the static order dials exactly as before.
/// CR-P1-05: when the sender flow stashed an ADOPTED dial socket, the
/// round runs on a `try_clone` of IT ([`probe_dial_round_on`]) and every
/// dial attempt adopts the same socket — the review's core rule (审核改法
/// 第 4 条: probing AND QUIC on one socket); the legacy throwaway round
/// covers the no-stash fallback byte for byte. CR-P1-06 改法 2: the
/// phase is PRECEDED by [`sender_reflexive_opener`] — see there (the
/// caller's `send_phase`).
async fn drive_task_send(
    pairing: &TaskPairing,
    key: &DeviceKeyPair,
    id_dir: &Path,
    link: &TaskLink,
    paths: &[PathBuf],
    adopted: Option<&std::net::UdpSocket>,
) -> Result<Report> {
    let static_order = order_candidates(link.peer.candidates.clone());
    if static_order.is_empty() {
        bail!("the paired peer advertised no dial candidates");
    }
    // 改法第 3 条 + CR-P1-05: probe round first (≤ the shared
    // PROBE_DIAL_WINDOW before the first dial attempt), observed address
    // leads when any. The CLI-side stdout line rides the shared round's
    // observation callback (deliberately avoids the words "advertising"
    // and "connected via" — the itests grep those for the candidate list
    // and the transfer verdict).
    let probe_observe = |head: Option<SocketAddr>,
                         report: &ak_net::holepunch::PunchReport,
                         probe_count: usize| {
        match head {
        Some(head) => println!(
            "[task] probe round observed {head} ({} of {probe_count} candidate(s) answered, {} peer probe packet(s) in); dialing the observed address first",
            report.reachable.len(),
            report.packets_received,
        ),
        None => println!(
            "[task] probe round observed nothing ({probe_count} candidate(s) probed in {} ms); dialing the reflexive/public candidates first",
            report.elapsed.as_millis(),
        ),
    }
    };
    let dial = match adopted.map(|socket| socket.try_clone().ok()) {
        Some(Some(round_socket)) => {
            probe_dial_round_on(
                round_socket,
                static_order.len(),
                static_order,
                probe_observe,
            )
            .await
        }
        _ => probe_dial_round(static_order, probe_observe).await,
    };
    let printer = crate::progress_printer("send");
    let send_ctx = SendCtx {
        key,
        id_dir,
        adopted,
    };
    let mut failures: Vec<(SocketAddr, String)> = Vec::new();
    for pass in 0..TASK_CONNECT_PASSES {
        if pass > 0 {
            tokio::time::sleep(TASK_SETTLE_PAUSE).await;
            failures.clear();
        }
        let outcome = staggered_dial_pass(
            &dial.order,
            dial.head,
            adopted.is_some(),
            |addr, marks, budget| {
                attempt_send(
                    &send_ctx,
                    addr,
                    link.peer_fp,
                    paths,
                    &printer,
                    budget,
                    marks,
                )
            },
            |outcome| match outcome {
                Attempt::Done(_) => AttemptVerdict::Done,
                Attempt::Declined => AttemptVerdict::Fatal,
                // The peer's admission trailed the pairing (wave-29
                // race): the connection died before any protocol byte —
                // retryable like a connect failure.
                Attempt::FailedAfterConnect {
                    nothing_offered: true,
                    reason,
                    ..
                } => AttemptVerdict::Retryable(reason.clone()),
                Attempt::FailedAfterConnect { .. } => AttemptVerdict::Fatal,
                Attempt::ConnectFailed(reason) => AttemptVerdict::Retryable(reason.clone()),
            },
        )
        .await;
        if let Some((addr, Attempt::Done(report))) = outcome.done {
            // Which candidate actually carried the task (the FFI
            // result carries the same fact as its `peer` field) —
            // with IPv6 in the mix (CR-P1-02 leg 1) the family of
            // the winning path is exactly what an operator needs
            // to see.
            println!("[task] connected via {addr}");
            return Ok(report);
        }
        match outcome.fatal {
            Some((_, Attempt::Declined)) => {
                bail!("the receiving side declined the transfer")
            }
            Some((
                addr,
                Attempt::FailedAfterConnect {
                    reason, peer_gone, ..
                },
            )) => {
                if peer_gone {
                    pairing.report_peer_disconnect(link.code_id);
                }
                bail!("transfer failed after connecting via {addr}: {reason}");
            }
            _ => {}
        }
        failures = outcome.failures;
    }
    let detail = failures
        .iter()
        .map(|(addr, reason)| format!("{addr}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ");
    bail!("no candidate accepted the connection ({detail})")
}

/// The per-send context shared by every candidate attempt of one task
/// send (CR-P1-05 grouping): the device key + checkpoint dir of the run,
/// plus the sender's ADOPTED dial socket when the flow stashed one (the
/// QUIC Initial then leaves from the socket the same-socket STUN query
/// and probe round mapped).
struct SendCtx<'a> {
    key: &'a DeviceKeyPair,
    id_dir: &'a Path,
    adopted: Option<&'a std::net::UdpSocket>,
}

/// One candidate attempt: build the send spec (family-matched bind, task
/// idle budget covering the consent window), drive it under the connect
/// budget, and classify the outcome.
///
/// CR-P3-09: `marks` is the CALLER's (the shared `staggered_dial_pass`'s)
/// [`ConnectMarks`] for this attempt — the pass reads its `connected()`
/// flag as the Happy-Eyeballs winner signal, so the marks cannot live
/// inside this function.
async fn attempt_send(
    ctx: &SendCtx<'_>,
    addr: SocketAddr,
    pinned: Fingerprint,
    paths: &[PathBuf],
    printer: &Arc<dyn Fn(Progress) + Send + Sync>,
    connect_budget: Duration,
    marks: Arc<ConnectMarks>,
) -> Attempt {
    let mut spec = SendSpec::new(paths[0].clone(), addr, pinned, make_self_signed(ctx.key));
    for extra in &paths[1..] {
        spec.add_root(extra.clone());
    }
    spec.bind = if addr.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    if let Some(socket) = ctx.adopted {
        if let Ok(handle) = socket.try_clone() {
            spec.adopt_socket(handle);
        }
    }
    spec.checkpoint_dir = ctx.id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", ctx.key.fingerprint().short_id());
    spec.idle_timeout = TASK_SEND_IDLE;

    let progress = {
        let send_marks = Arc::clone(&marks);
        let printer = Arc::clone(printer);
        move |event: Progress| {
            send_marks.mark(&event);
            printer(event);
        }
    };
    let send = send(spec, &progress);
    tokio::pin!(send);
    // The caller's budget: the full task budget for static candidates,
    // the bounded observed-candidate budget for the leading
    // observed/reflexive candidate (改法第 3 条 — see the constant).
    let result = match drive_with_connect_budget(&marks, send.as_mut(), connect_budget).await {
        Ok(result) => result,
        Err(()) => {
            return Attempt::ConnectFailed(format!(
                "timeout after {}s (no QUIC handshake)",
                connect_budget.as_secs()
            ))
        }
    };
    let after_connect = marks.connected();
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
                    nothing_offered: !marks.offered(),
                    reason,
                }
            } else {
                Attempt::ConnectFailed(reason)
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
/// the departure instead of staying "connected" — the CLI adapter over
/// the shared `task_net::deliver_close_notify` core (best effort and
/// bounded; CR-P1-02 改法第 3 条's observed-source lead is decided inside
/// the core — see its docs).
async fn close_notify_peer(key: &DeviceKeyPair, link: &TaskLink, observed: Option<SocketAddr>) {
    deliver_close_notify(key, link.peer.candidates.clone(), link.peer_fp, observed).await;
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
    println!(
        "[task] paired; peer {endpoint} (task {})",
        hex_of(&link.code_id)
    );
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Name what this RECEIVER-side run advertises (CR-P1-02 改法第 1 条: with
/// IPv6 in the candidate list the family mix must be visible when
/// diagnosing cross-NAT reachability — one bounded line, nothing hidden).
fn print_advertised(blob: &[SocketAddr]) {
    let list = blob
        .iter()
        .map(SocketAddr::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    println!("[task] advertising {} candidate(s): {list}", blob.len());
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
        // CR-P1-05: same as join-send — the sender-role code binds the
        // future-QUIC dial socket first (same-socket STUN), the blob
        // carries the reflexive address + the dial socket's real port,
        // and the send phase dials from this exact socket.
        let (sender_bound, sender_reflexive) = session.bind_sender_socket(cfg.stun).await?;
        let echo_addr = session.bind_echo_responder()?;
        let dial = insert_reflexive_candidate(local_candidates(sender_bound), sender_reflexive);
        let blob = with_advertised(
            blob_candidates(dial, Vec::new(), local_candidates(echo_addr), ""),
            &cfg.advertise,
            sender_bound.port(),
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
        // CR-P1-06 改法 2: the opener runs BEFORE the pairing-plane
        // reachability report (which takes ≥ its 1.5 s probe window, up to
        // ~3.3 s with the retry) — the review's "配对后短暂等待，然后立即
        // 从收养 socket 向接收端反射地址发包" pins the adopted socket's
        // first outbound to ~SENDER_DIAL_PAUSE after pairing, well inside
        // the receiver's 1 s low-TTL punch phase; deferring it past the
        // report would let the receiver's phase-2 normal-TTL punches reach
        // the sender's NAT first and re-create the conntrack tuple
        // conflict this fix removes.
        let opened = sender_reflexive_opener(
            session.sender_socket.as_ref(),
            &order_candidates(link.peer.candidates.clone()),
        )
        .await;
        // Zero sent with an adopted socket + non-empty candidates is the
        // run-43304434327 failure fingerprint (filter too strict): make
        // it visible, not a silent no-op.
        println!("[task] reflexive opener sent {opened} packet(s)");
        probe_and_print(&session.stack.pairing, link).await;
        drive_task_send(
            &session.stack.pairing,
            &session.key,
            id_dir,
            link,
            paths,
            // CR-P1-05: the sender flow's adopted dial socket (None on
            // receiver flows and bind failure — the legacy path).
            session.sender_socket.as_ref(),
        )
        .await
    };
    Ok(tokio::select! {
        report = flow => report,
        _ = wait_cancelled(&hooks.cancel) => return Ok(Err(TaskCancelled.into())),
    })
}
/// it, wait for the joiner, probe, then serve receives behind the consent
/// prompt until one transfer completes (or the peer departs).
pub(crate) async fn run_code_recv(cfg: TaskRunConfig, mut hooks: TaskHooks) -> Result<TaskOutcome> {
    let started = Instant::now();
    let mut session = open_session(&cfg, true).await?;
    let flow = async {
        let ((bound, reflexive), mut receiver, code_slot) =
            session.bind_task_receiver(&cfg).await?;
        let echo_addr = session.bind_echo_responder()?;
        // CR-P1-02 改法第 2 条: the receiver's STUN server-reflexive
        // address rides the derived dial candidates (inserted right after
        // the v4 interface block — see the helper).
        let blob = with_advertised(
            blob_candidates(
                insert_reflexive_candidate(local_candidates(bound), reflexive),
                Vec::new(),
                local_candidates(echo_addr),
                "",
            ),
            &cfg.advertise,
            bound.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        print_advertised(&blob);
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
        // CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): the
        // future-QUIC dial socket is bound and same-socket STUN-queried
        // BEFORE the redeem — the blob the redeem registers carries the
        // reflexive address and the socket's real port (the paired
        // receiver's punch target), and the send phase below dials FROM
        // this exact socket.
        let (sender_bound, sender_reflexive) = session.bind_sender_socket(cfg.stun).await?;
        let echo_addr = session.bind_echo_responder()?;
        let dial = insert_reflexive_candidate(local_candidates(sender_bound), sender_reflexive);
        let blob = with_advertised(
            blob_candidates(dial, Vec::new(), local_candidates(echo_addr), ""),
            &cfg.advertise,
            sender_bound.port(),
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
        let ((bound, reflexive), mut receiver, code_slot) =
            session.bind_task_receiver(&cfg).await?;
        let echo_addr = session.bind_echo_responder()?;
        // 改法第 2 条, the joiner-receiver mirror of code-recv: the
        // socket-reflexive address rides the derived dial candidates.
        let blob = with_advertised(
            blob_candidates(
                insert_reflexive_candidate(local_candidates(bound), reflexive),
                Vec::new(),
                local_candidates(echo_addr),
                "",
            ),
            &cfg.advertise,
            bound.port(),
        );
        if blob.is_empty() {
            bail!("no dialable candidate to advertise");
        }
        print_advertised(&blob);
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

    fn sender_cfg(id: &tempfile::TempDir, registry: &TestRegistry, ttl: u32) -> TaskRunConfig {
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
        let sent = run_join_send(cfg_join, hooks(None, None, &no_cancel()), code, vec![src])
            .await
            .expect("join-send completes");
        assert_eq!(sent.files, 1);
        assert_eq!(sent.bytes, 256_000);

        let received = owner.await.expect("owner task").expect("code-recv ok");
        assert_eq!(received.files, 1);
        assert_eq!(received.bytes, 256_000);
        assert_eq!(
            blake3::hash(&std::fs::read(out_a.path().join("only.txt")).unwrap()),
            hash
        );
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
        let error = run_join_send(cfg_join, hooks(None, None, &no_cancel()), code, vec![src])
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
        let error = run_join_send(cfg_join, hooks(None, None, &no_cancel()), code, vec![src])
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
        assert!(
            owner_result.is_ok(),
            "the owner's send completes: {owner_result:?}"
        );
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

    /// `--advertise` entries lead the blob, and a v6-heavy mix still fits
    /// the 512-byte wire bound after the shrink loop (the loop pops the
    /// tail: the advertised head survives, the least-significant derived
    /// entry gives way). Pins the CLI-only `with_advertised` seam over
    /// the shared `task_net` blob helpers.
    #[test]
    fn advertised_blobs_shrink_to_the_wire_bound() {
        use ak_session::pairing::task_net::MAX_DIAL_CANDIDATES;
        // A dial list that alone fits the bound: eight full-length global
        // v6 candidates (~49 bytes each encoded).
        let dial: Vec<SocketAddr> = (1..=8u16)
            .map(|i| {
                let v6 = std::net::Ipv6Addr::from([0x2606, 0x4700 + i, i, i, i, i, i, i]);
                SocketAddr::new(std::net::IpAddr::V6(v6), 40_000 + i)
            })
            .collect();
        assert_eq!(dial.len(), MAX_DIAL_CANDIDATES);
        // Nine more advertised full-length v6 entries push the combined
        // list past 16 candidates AND past 512 bytes — the shrink must
        // keep the head (the operator's explicit addresses lead) and pop
        // the tail until the blob encodes.
        let advertise: Vec<std::net::IpAddr> = (1..=9u16)
            .map(|i| {
                std::net::IpAddr::V6(std::net::Ipv6Addr::from([
                    0x2a00,
                    0x1450 + i,
                    i,
                    i,
                    i,
                    i,
                    i,
                    i,
                ]))
            })
            .collect();
        let out = with_advertised(
            blob_candidates(dial, Vec::new(), Vec::new(), ""),
            &advertise,
            41_000,
        );
        assert!(
            out.len() > 1 && out.len() < 16,
            "the shrink kept a bounded tail: {out:?}"
        );
        assert_eq!(
            out[0],
            SocketAddr::new(advertise[0], 41_000),
            "the operator's advertised head survives the shrink"
        );
        let endpoint = PeerEndpoint {
            candidates: out,
            name: String::new(),
            direction: TaskDirection::Send,
        };
        assert!(ak_session::pairing::encode_endpoint_blob(&endpoint).is_ok());
    }

    /// `--stun` resolution: the empty value is the explicit opt-out, a
    /// valid ip:port resolves, junk fails with a readable reason.
    #[test]
    fn stun_flag_resolution_empty_disables_and_junk_fails() {
        assert_eq!(resolve_stun("").expect("empty disables"), None);
        assert_eq!(resolve_stun("  ").expect("blank disables"), None);
        assert_eq!(
            resolve_stun("127.0.0.1:7101").expect("valid resolves"),
            Some("127.0.0.1:7101".parse().unwrap())
        );
        let error = resolve_stun("not-an-address").expect_err("junk must fail");
        assert!(error.to_string().contains("--stun"), "{error}");
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

    // ---- R36-P1-04: the consent stdin contract --------------------------
    // EOF, I/O errors and truncated input DECLINE; only a genuinely read
    // newline (empty line), `y` or `yes` accepts.

    impl TerminalInput {
        /// Tests: build the source from a caller-fed channel (no stdin
        /// reader thread involved).
        fn from_receiver(rx: tokio::sync::mpsc::Receiver<TerminalLine>) -> Arc<TerminalInput> {
            Arc::new(TerminalInput {
                lines: tokio::sync::Mutex::new(rx),
            })
        }
    }

    fn stdin_line(text: &str) -> StdinLine {
        StdinLine::Line {
            text: text.to_string(),
            complete: text.ends_with('\n'),
        }
    }

    #[test]
    fn consent_verdicts_pin_the_stdin_contract() {
        // A genuine Enter (a complete empty line) accepts — the default Y.
        assert!(stdin_line("\n").consent_decision());
        assert!(stdin_line("   \n").consent_decision());
        // Explicit yes/no.
        assert!(stdin_line("y\n").consent_decision());
        assert!(stdin_line("Y\n").consent_decision());
        assert!(stdin_line("yes\r\n").consent_decision());
        assert!(!stdin_line("n\n").consent_decision());
        assert!(!stdin_line(" No \n").consent_decision());
        assert!(!stdin_line("maybe\n").consent_decision());
        // R36-P1-04: EOF and I/O failure are NOT an answer — decline.
        assert!(!StdinLine::Eof.consent_decision());
        assert!(!StdinLine::Io("broken pipe".to_string()).consent_decision());
        // Input that ended WITHOUT a newline (a truncated pipe) never
        // counts as Enter — decline even for `y`.
        assert!(!StdinLine::Line {
            text: "y".to_string(),
            complete: false,
        }
        .consent_decision());
        assert!(!StdinLine::Line {
            text: String::new(),
            complete: false,
        }
        .consent_decision());
    }

    #[tokio::test]
    async fn terminal_input_answers_fresh_input_and_expires() {
        let (tx, rx) = tokio::sync::mpsc::channel::<TerminalLine>(8);
        let input = TerminalInput::from_receiver(rx);

        // A fresh `y` line answers the prompt.
        let asker = {
            let input = Arc::clone(&input);
            tokio::spawn(async move { input.answer(Duration::from_secs(2)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(TerminalLine {
            at: Instant::now(),
            kind: stdin_line("y\n"),
        })
        .await
        .expect("feed a fresh answer");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), asker)
                .await
                .expect("bounded")
                .expect("asker alive"),
            "a fresh y accepts"
        );

        // A fresh EOF declines (never "empty = yes").
        tx.send(TerminalLine {
            at: Instant::now(),
            kind: StdinLine::Eof,
        })
        .await
        .expect("feed EOF");
        let verdict = input.answer(Duration::from_secs(2)).await;
        assert!(!verdict, "EOF declines");

        // The window expires with no input at all.
        let started = Instant::now();
        let verdict = input.answer(Duration::from_millis(150)).await;
        assert!(!verdict, "an expired window declines");
        assert!(
            started.elapsed() >= Duration::from_millis(140),
            "the window was actually waited"
        );
    }

    /// R36-P2-01: the answer to an EXPIRED prompt (input that predates
    /// the next question) is discarded — it must never silently accept
    /// the next offer.
    #[tokio::test]
    async fn terminal_input_discards_expired_prompts_answers() {
        let (tx, rx) = tokio::sync::mpsc::channel::<TerminalLine>(8);
        let input = TerminalInput::from_receiver(rx);

        // A queued `y` that predates the question…
        tx.send(TerminalLine {
            at: Instant::now() - Duration::from_secs(1),
            kind: stdin_line("y\n"),
        })
        .await
        .expect("queue a stale answer");
        // …and a fresh-looking `y` parked in the reader thread (read
        // completed before the prompt started, delivered after the drain).
        tx.send(TerminalLine {
            at: Instant::now() - Duration::from_millis(1),
            kind: stdin_line("y\n"),
        })
        .await
        .expect("queue a second stale answer");

        // The next prompt declines: both stale answers were discarded and
        // the (short) window expires.
        let started = Instant::now();
        let verdict = input.answer(Duration::from_millis(200)).await;
        assert!(!verdict, "stale answers never accept the next offer");
        assert!(
            started.elapsed() >= Duration::from_millis(190),
            "the full window was waited after discarding"
        );

        // The channel is now empty: an Enter typed DURING the next prompt
        // (sent after it started — pre-prompt input would be drained as
        // stale by design) accepts.
        let asker = {
            let input = Arc::clone(&input);
            tokio::spawn(async move { input.answer(Duration::from_secs(2)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(TerminalLine {
            at: Instant::now(),
            kind: stdin_line("\n"),
        })
        .await
        .expect("feed a fresh Enter");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), asker)
                .await
                .expect("bounded")
                .expect("asker alive"),
            "a fresh Enter accepts after the stale ones were dropped"
        );
    }

    // ---- R36-P2-04: the per-identity connection lock --------------------

    fn write_holder_lock(id_dir: &Path, pid: u32, age: Duration) {
        let beat = crate::now_unix_ms().saturating_sub(age.as_millis() as u64);
        std::fs::write(
            id_dir.join(CONNECTION_LOCK_FILE),
            format!("{pid}\n{beat}\n"),
        )
        .expect("write the holder's lock");
    }

    #[test]
    fn connection_lock_refuses_a_live_second_holder() {
        let id_dir = identity_dir("lk-live");
        write_holder_lock(id_dir.path(), 424_242, Duration::from_secs(1));
        let error = acquire_connection_lock(id_dir.path())
            .expect_err("a live holder must refuse a second process");
        let text = format!("{error:#}");
        assert!(
            text.contains("424242"),
            "the error names the holding process: {text}"
        );
        assert!(
            text.contains("--id-dir"),
            "the error tells the user how to proceed: {text}"
        );
    }

    #[test]
    fn connection_lock_breaks_stale_and_own_locks_and_releases() {
        let id_dir = identity_dir("lk-stale");

        // A crashed holder (heartbeat far past the stale window) is
        // breakable.
        write_holder_lock(id_dir.path(), 424_242, CONNECTION_LOCK_STALE * 4);
        let lock = acquire_connection_lock(id_dir.path()).expect("stale lock is breakable");
        let text = std::fs::read_to_string(id_dir.path().join(CONNECTION_LOCK_FILE)).unwrap();
        assert!(
            text.starts_with(&format!("{}\n", std::process::id())),
            "the broken lock now names THIS process: {text:?}"
        );
        drop(lock);
        assert!(
            !id_dir.path().join(CONNECTION_LOCK_FILE).exists(),
            "drop removes the lock we own"
        );

        // A fresh lock naming OUR OWN pid never refuses us (sequential
        // runs of the same process).
        write_holder_lock(id_dir.path(), std::process::id(), Duration::ZERO);
        let lock = acquire_connection_lock(id_dir.path()).expect("own pid is not a conflict");
        drop(lock);
        assert!(!id_dir.path().join(CONNECTION_LOCK_FILE).exists());
    }

    /// R36-P1-01 (the real validation, loopback + a real v2 registry
    /// fixture): the code owner idles FAR past the registry server's 30 s
    /// inbound-silence cut — the connection keeper's 20 s pings keep the
    /// session online, so a joiner arriving after 65 s still pairs and
    /// transfers. Without a keeper the owner goes offline at ~30 s and the
    /// join fails with "unknown task code (owner offline)".
    #[tokio::test]
    async fn keeper_keeps_an_idle_code_owner_online_past_the_silence_cut() {
        let registry = TestRegistry::spawn();
        let id_owner = identity_dir("idle-owner");
        let id_joiner = identity_dir("idle-joiner");
        let out_owner = identity_dir("idle-out");
        let src = id_joiner.path().join("late.bin");
        let hash = write_payload(&src, 64_000);

        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(run_code_recv(
            receiver_cfg(
                &id_owner,
                &registry,
                &out_owner,
                ConsentInput::Scripted(true),
                CONSENT_WINDOW,
            ),
            hooks(Some(code_tx), None, &no_cancel()),
        ));
        let code = code_rx.await.expect("code is displayed");

        // Idle the owner past TWO server silence cuts (30 s each): the
        // keeper's 20 s pings must keep the control session alive.
        tokio::time::sleep(Duration::from_secs(65)).await;

        let sent = run_join_send(
            sender_cfg(&id_joiner, &registry, 600),
            hooks(None, None, &no_cancel()),
            code,
            vec![src],
        )
        .await
        .expect("a joiner arriving after the idle window still pairs");
        assert_eq!(sent.files, 1);
        assert_eq!(sent.bytes, 64_000);
        let received = owner.await.expect("owner task").expect("code-recv ok");
        assert_eq!(received.files, 1);
        assert_eq!(
            blake3::hash(&std::fs::read(out_owner.path().join("late.bin")).unwrap()),
            hash
        );
    }
}
