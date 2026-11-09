//! B8 (wave 28): the safe layer behind the `ak_task_*` C ABI exports — the
//! v1 minimal-pairing task plane ("极简三动作": generate a task code with a
//! chosen role / join by entering a code / connection list + disconnect;
//! after connecting the sender picks files, the receiver accepts or
//! declines; exactly one default save folder; zero IP/port/fingerprint
//! surfaced to the user).
//!
//! Everything here is compiled with `forbid(unsafe_code)`; the pointer
//! adapters live in [`crate::task_ext`]. The op/result discipline is the
//! frozen ffi-v2 §2/§3 one, reused verbatim: validation precedes any side
//! effect (an op id is only minted for a fully valid request),
//! `request_id` makes create/join/send idempotent (same id + same spec →
//! the SAME op id and the SAME answer; same id + different spec → `-2`;
//! wave-29 P2-01: the check/reserve/launch handoff is one atomic
//! acceptance under the request registry's lock, the canonical spec is
//! namespaced by op type + connection id, and an accepted request replays
//! its ORIGINAL answer even after the connection later disconnected),
//! `AK_ERR_BUFFER` never consumes (op results re-readable, popped events
//! pushed back), and results/events cross through the EXISTING
//! `ak_pairing_op_result` / `ak_pairing_poll_event` channels with the task
//! runtime id in place of a pairing runtime id (the plane extension the
//! wave-28 brief froze: "接收提示/断开事件走现有 poll_event 通道扩展事件
//! 类型").
//!
//! # Engine sharing without engine borrows
//!
//! The async op bodies run on the engine's embedded runtime but must not
//! borrow the engine (an FFI handle is caller-owned). The task plane
//! therefore shares exactly two engine-level artefacts by `Arc`:
//!
//! * the task-code table (created with the engine's v2 state, cloned into
//!   every task runtime) — the join/disconnect bookkeeping both sides of
//!   a pairing mutate;
//! * the engine-wide progress queue (`ak_poll_progress`) — send/receive
//!   progress lines flow into it under the CONNECTION id as the `job`
//!   field (wave-31 R30-P2-02: both directions — the pairing
//!   transaction's id is the one correlation key both sides hold),
//!   exactly like the scheduler executor's per-task lines.
//!
//! # The registry relay (wave-28 closeout: the ak-session task API)
//!
//! Task codes are REAL registry invites now: `create_code` claims through
//! ak-session `TaskPairing::create_task_code_with_ttl` (endpoint blob +
//! `max_redeems = 1`, so one code pairs exactly once) and `join` redeems
//! through `TaskPairing::join_task_code`, driving the frozen pairing
//! transcript to `PairReady` OVER THE REGISTRY — a join across engines,
//! processes and devices is the same code path as an in-process one. The
//! owner side runs the resident task-aware owner loop
//! (`spawn_owner_loop_verified` + `complete_owner_pairing`) BEHIND the
//! pre-commit redeem-offer gate (`install_redeem_offer_gate`, wave-30
//! P1-05: a same-direction or endpoint-less join is vetoed before the
//! server commits, so the code's single redeem is never burned by a
//! refused task), so its `connected` event, connection entry, verified
//! peer key and probe all come from the registry-relayed pairing, not
//! from local state.
//!
//! * **Peer identity** — the joiner pins the owner's device fingerprint
//!   from `TaskPaired::verified.owner_fingerprint()` (the relayed,
//!   transcript-verified key), and the owner the redeemer's. The engine
//!   `send` keeps using it as the TLS pin.
//! * **Receiver admission binds the pairing (wave-29 P1-04)** — the
//!   resident receiver's TLS `authorize` accepts ONLY the fingerprint of
//!   the connection entry the completed pairing registered. Before the
//!   pairing completes there is no entry, so every dial is refused at
//!   handshake and the listener keeps waiting; a stranger that somehow
//!   reaches the port can neither deliver an Offer nor consume the
//!   receiver. The offer-level consent is additionally ATTRIBUTED by the
//!   verified sender fingerprint ([`ConsentInfo::sender_fingerprint`]):
//!   an `incoming_request` belongs to the fingerprint-matching connection
//!   of that code, never merely to the port's own code id.
//! * **Probing** — after a pairing both sides run ak-session
//!   `probe_peer` over the peer's advertised candidates (LAN-first) and
//!   emit `probe_result`; the join result's `probe` field is the honest
//!   verdict (`verified` / `all_failed`; `unverified` only when no probe
//!   could run). Probes are answered by the probe echo responder each
//!   side binds alongside its endpoint advertisement (below); the
//!   responder is anti-reflection hardened (wave-29 P2-12: replies carry
//!   the all-zero nonce and are never themselves answered, plus a
//!   per-source reply budget, so two responders cannot ping-pong).
//! * **Remote disconnect** — `TaskPairing::disconnect` revokes the code
//!   at the registry (a pending redeem of a revoked code aborts), and the
//!   [`TaskEvent`] stream (`TaskPairing::subscribe`) is pumped into the
//!   task event queue: `TaskDisconnected{ByPeer}` → `disconnected` with
//!   reason `peer`, `Network` → the frozen `receiver_failed` wording (a
//!   genuine DATA-plane link loss — wave-30 P2-08: a registry/control
//!   disconnect produces no task event at all; the data plane is
//!   independent of the registry). A `local` line is pushed directly by
//!   `ak_task_disconnect`, so the pump never repeats it. While IDLE (no
//!   live transfer whose QUIC drop the peer would see) the disconnecting
//!   side additionally dials the paired peer's advertised data endpoint
//!   and hands it an authenticated TASK-CLOSE Error frame (wave-29
//!   P2-03), so the LISTENING peer learns `ByPeer` immediately instead
//!   of staying "connected"; a peer that never bound a data listener (a
//!   pure sender side) still learns honestly at its next send, which
//!   fails to connect.
//!
//! The endpoint blob each side registers carries its dial candidates:
//! the RECEIVER side's real resident-receiver port first, then — when the
//! candidates were derived locally — a dedicated probe-echo UDP port
//! appended last (a small responder that answers ak-net hole-punch probe
//! packets from any advertised address; the data-plane dial candidates
//! keep precedence, and the probe genuinely confirms bidirectional UDP
//! reachability of the peer host). Explicit `candidates` in the spec
//! replace the derived list verbatim (caller-knows-better), and then no
//! echo port is appended — the probe reports exactly what the caller
//! advertised.
//!
//! The engine-shared code table stays as a CACHE ONLY: same-engine
//! joins use it to mirror the code's declared role onto the joiner (the
//! join spec's optional `role` field says the same thing cross-device;
//! the default is `receiver`, the v1 primary story) and
//! `ak_task_disconnect` uses it to notify a same-engine peer instantly.
//! Cross-engine joins never consult it — the registry is the source of
//! truth.
//!
//! REAL today, end to end (the positive tests drive real pairings, real
//! probes and real bytes through the boundary against a live registry):
//!
//! * `ak_task_start` — local bootstrap: identity under `id_dir`, default
//!   save folder `<id_dir>/downloads`, registry endpoint + pin validated
//!   and stored (the Dart layer resolves them from environment/dart-define
//!   exactly like the invite plane's `AK_REGISTRY_ADDR`/`AK_REGISTRY_ID`).
//! * `ak_task_create_code` — a real registry claim (owner settings blob,
//!   `max_redeems = 1`, TTL: an absent or `0` `ttl_secs` maps to the
//!   task-code default [`TASK_CODE_TTL_SECS`], so v1 codes never linger
//!   as never-expiring registry rows — wave-29 P2-04). A RECEIVER-role
//!   creation additionally binds a REAL resident receiver (wildcard bind,
//!   serve loop) and derives the code's dial candidates from the
//!   machine's interfaces.
//! * `ak_task_join` — a real registry redeem: the full pairing transcript
//!   to `PairReady`, bilateral `connected` events (the owner's from its
//!   resident owner loop), the verified peer key, a real `probe_result`
//!   and — when the joiner is the receiver — a real resident receiver
//!   bind before the op completes.
//! * `ak_task_send` — a REAL `ak-engine` send over the connection's
//!   candidate endpoints, LAN-first ordered, with the split 10 s connect
//!   budget + automatic fallback of the wave-25/26 CLI driver (P1-01: the
//!   budget starts at the engine's `Connecting` report, never during the
//!   source scan, and stops applying once connected).
//! * Offer-level receive consent — the receiver's resident receiver runs
//!   with the ak-engine `TransferConsent` hook in its NON-BLOCKING shape
//!   (wave-29 P2-07: the callback registers the request and returns a
//!   pending oneshot immediately — no Tokio worker is ever parked on a
//!   human decision): every incoming offer mints a pending request id,
//!   emits an `incoming_request` event attributed to the fingerprint-
//!   matched connection, and the transfer is held until
//!   `ak_task_consent` decides, the frozen 30 s window auto-declines
//!   (a timer task, not a blocked thread), the runtime tears down, or
//!   the connection disconnects. The peer is told over the wire
//!   (DECLINED semantics); on a decline nothing is written to disk.
//!   Wave-31 R30-P2-03: the pending insert and the `incoming_request`
//!   publication are ONE critical section with the connection's
//!   liveness re-checked inside it — an offer registered on an
//!   already-closed connection is refused without any event, so a late
//!   `incoming_request` after `request_closed` is structurally
//!   impossible.
//! * The resident receiver (wave-29 P2-05/P2-06) — ONE endpoint (one
//!   advertised port) serves a LOOP of connections: unauthorized dials
//!   are rejected and the loop keeps listening (they never consume the
//!   receiver); a DECLINED offer ends only that connection, so the
//!   sender may retry on the same code; a COMPLETED transfer is the
//!   single-transfer task's terminal state (both sides converge their
//!   connection entries to `disconnected`). The save folder is resolved
//!   from the runtime's CURRENT setting each time a transfer starts, so
//!   `ak_task_set_save_dir` applies to every subsequent task; wave-31
//!   P2-06 makes that ONE immutable binding per transfer — the
//!   `incoming_request` event's `saveDir` and the `done` event's
//!   `saveDir` carry the very directory the engine's `ReceiveSpec`
//!   writes into (never a second, skewable read of the setting; a change
//!   during the pending-consent window applies to the NEXT request).
//! * `ak_task_list` / `ak_task_disconnect` / `ak_task_set_save_dir` /
//!   `ak_task_consent` — real state, real bilateral `disconnected` events
//!   (registry revoke + TASK-CLOSE + event pump; a same-engine peer
//!   additionally gets the instant cache notification).
//!   `ak_task_disconnect` is idempotent and also covers a code that was
//!   generated but never joined (wave-29 P2-04): disconnecting by the
//!   create result's `code_id` revokes the registry row, releases the
//!   receiver/echo listeners and emits `code_cancelled`.
#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ak_core::error::ErrorCode;
use ak_engine::{
    receive_accepted_consent, send, ConsentAnswer, ConsentInfo, EngineError, Progress, ReceiveSpec,
    Report, SendSpec, TransferConsent,
};
use ak_identity::pairing_txn::{random_id16, VerifiedPairing};
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{
    ClientConfig, OrchestratorConfig, PairFlowError, PairingOrchestrator, PeerEndpoint,
    ProbeOptions, ProbeOutcome, RegistryClient, TaskDirection, TaskDisconnectReason, TaskError,
    TaskEvent, TaskPaired, TaskPairing, TaskPairingConfig, TASK_CODE_TTL_SECS,
};
use ak_wire::{Frame, Message};
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;

use crate::core::{self, FfiError};
use crate::pairing::{OpEntry, OpOutcome, OpResult};
use crate::AkEngine;

/// Offer-level consent window: a pending `incoming_request` holds the
/// transfer until `ak_task_consent` decides or this window elapses (then
/// the request auto-declines — never auto-accepts). 30 s per the frozen
/// v1 model.
pub(crate) const PENDING_CONSENT_WINDOW: Duration = Duration::from_secs(30);

/// Per-candidate connect budget of `ak_task_send` (the wave-26 P1-01
/// value: cross-VPN QUIC+TLS handshakes measurably need more than 3 s;
/// LAN candidates still connect in milliseconds, so the larger budget
/// costs nothing on fast paths).
const TASK_CONNECT_BUDGET: Duration = Duration::from_secs(10);

/// Idle budget of a task-plane send: must comfortably cover the receiver's
/// [`PENDING_CONSENT_WINDOW`] (the sender's offer waits while the peer's
/// user decides) plus slow-storage source scans on the far end.
const TASK_SEND_IDLE: Duration = Duration::from_secs(120);

/// Brief pause between candidate attempts (the failed attempt's QUIC
/// endpoint releases its UDP socket asynchronously; the CLI driver uses
/// the same 150 ms).
const ATTEMPT_PAUSE: Duration = Duration::from_millis(150);

/// Whole-list connect passes of `ak_task_send` (wave-29 P1-04 follow-up):
/// a send may fire the instant its join op returned, while the paired
/// peer's owner loop is still a beat away from registering the connection
/// its receiver admission is bound to — its first dial can be REFUSED at
/// the TLS gate through no fault of the sender. Purely connect-level
/// failures (nothing ever connected) are retried after a short settle
/// pause; identity checks are unaffected — the peer still authorizes
/// exactly the pairing's verified fingerprint.
const TASK_CONNECT_PASSES: usize = 3;
const TASK_SETTLE_PAUSE: Duration = Duration::from_millis(300);

/// Bound for the resident receiver's first bind (surfaced as the code's
/// candidates). Binding a wildcard UDP socket is fast; the bound exists so
/// a wedged stack fails the create op instead of hanging it.
const RECEIVER_BIND_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap of one task runtime's event queue (same budget as every plane).
const MAX_QUEUED_EVENTS: usize = 65_536;

/// Bounded wait for the registry connect of the task-pairing stack (same
/// budget as the pairing plane's background connect).
const REGISTRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Redeem window offered to the registry by a task-plane join: how long
/// the code owner has to answer once a joiner typed the code. A live
/// owner loop answers in milliseconds; unknown/expired/revoked codes and
/// offline owners surface the unified "unavailable" answer at this
/// deadline (the ak-session default of 90 s is the long-lived invite
/// UX, not the short-TTL task-code one).
const TASK_REDEEM_WINDOW_MS: u64 = 10_000;

/// Probe tunables of the task plane: one probe round listens 1.5 s with
/// probes every 150 ms (ak-session's defaults); a failed first round gets
/// exactly one retry after a short stagger so the two sides' windows
/// overlap even when one side's pairing completion lands late.
const TASK_PROBE_OPTIONS: ProbeOptions = ProbeOptions {
    interval: Duration::from_millis(150),
    window: Duration::from_millis(1_500),
};
const TASK_PROBE_RETRY_PAUSE: Duration = Duration::from_millis(300);

/// Upper bounds of the endpoint blob's candidate list (the wire bound is
/// 16 candidates / 512 bytes; the plane stays well under both so the
/// shrink loop below is a belt-and-braces guard, never the common path).
const MAX_DIAL_CANDIDATES: usize = 8;
const MAX_ECHO_CANDIDATES: usize = 4;

/// Wire marker of the task-plane close notification (wave-29 P2-03): a
/// disconnecting side that has no live transfer (whose QUIC drop the peer
/// would observe) dials the paired peer's advertised data endpoint — TLS
/// pinned to the pairing's verified fingerprint — and hands it exactly one
/// Error frame with `ErrorCode::Cancelled` and this message before
/// closing. The peer's resident receiver recognizes the marker and ends
/// its pairing ByPeer (the event pump surfaces `disconnected`/`peer`).
/// Reusing the existing Error-frame semantics adds no wire vocabulary.
pub(crate) const TASK_CLOSE_MESSAGE: &str = "TASK-CLOSE: paired device disconnected";

/// Per-candidate and overall budget of the close-notify dial (best effort:
/// a peer behind a dead path simply is not reachable; the disconnect
/// itself already succeeded locally).
const TASK_CLOSE_CONNECT_BUDGET: Duration = Duration::from_secs(3);
const TASK_CLOSE_TOTAL_BUDGET: Duration = Duration::from_secs(6);

/// Anti-reflection constants of the probe echo responder (wave-29 P2-12):
/// replies carry the all-zero nonce (a real prober's nonce is
/// time/counter-derived and never all-zero) and are NEVER themselves
/// answered, and each source endpoint gets a bounded reply budget per
/// window so a spoofed flood cannot sustain CPU burn either.
const ECHO_REPLY_NONCE: [u8; 16] = [0u8; 16];
const ECHO_REPLY_BUDGET: u32 = 32;
const ECHO_REPLY_WINDOW: Duration = Duration::from_secs(1);
const ECHO_SOURCES_MAX: usize = 1024;

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// The engine-shared task-code table — a CACHE of this engine's own task
/// codes (see the module doc): same-engine role mirroring at join, the
/// same-engine instant peer notification at disconnect, and the owner's
/// display-code lookup. The REGISTRY is the source of truth for every
/// join; nothing here is authoritative across devices. Keyed by the
/// normalized display code.
pub(crate) type TaskCodeTable = StdMutex<HashMap<String, TaskCodeEntry>>;

/// Which side of the connection this runtime plays (the creator picks a
/// role when generating the code; the joiner gets the opposite one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskRole {
    Sender,
    Receiver,
}

impl TaskRole {
    fn as_str(self) -> &'static str {
        match self {
            TaskRole::Sender => "sender",
            TaskRole::Receiver => "receiver",
        }
    }

    fn parse(text: &str) -> Result<TaskRole, FfiError> {
        match text {
            "sender" => Ok(TaskRole::Sender),
            "receiver" => Ok(TaskRole::Receiver),
            other => Err(FfiError::invalid(format!(
                "field \"role\" must be sender or receiver, got {other:?}"
            ))),
        }
    }

    fn opposite(self) -> TaskRole {
        match self {
            TaskRole::Sender => TaskRole::Receiver,
            TaskRole::Receiver => TaskRole::Sender,
        }
    }

    /// The ak-session direction tag of this role.
    fn direction(self) -> TaskDirection {
        match self {
            TaskRole::Sender => TaskDirection::Send,
            TaskRole::Receiver => TaskDirection::Recv,
        }
    }

    fn of_direction(direction: TaskDirection) -> TaskRole {
        match direction {
            TaskDirection::Send => TaskRole::Sender,
            TaskDirection::Recv => TaskRole::Receiver,
        }
    }
}

/// Lifecycle of one connection entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    /// Paired over the registry, the `probe_peer` reachability test still
    /// running (the entry settles to `connected`/`unreachable` when the
    /// probe verdict arrives — wave-30 P2-09: the settle is HONEST, a
    /// total probe failure is not a `connected`; the verdict itself rides
    /// the `probe_result` event and the join result).
    Probing,
    Connected,
    /// Every advertised candidate failed the reachability probe
    /// (wave-30 P2-09): the pairing stands and a send may still be
    /// attempted (the probe proves UDP liveness of candidates, not
    /// dialability of the QUIC path), but `ak_task_list` reports the
    /// honest verdict instead of `connected`.
    Unreachable,
    Disconnected,
}

impl ConnState {
    fn as_str(self) -> &'static str {
        match self {
            ConnState::Probing => "probing",
            ConnState::Connected => "connected",
            ConnState::Unreachable => "unreachable",
            ConnState::Disconnected => "disconnected",
        }
    }
}

/// One connection of one task runtime (both sides of a pairing hold an
/// entry with the SAME conn id — the pairing transaction's id, which the
/// registry relayed to both, is the UI correlation key).
#[derive(Debug, Clone)]
pub(crate) struct TaskConn {
    pub conn_id: [u8; 16],
    role: TaskRole,
    state: ConnState,
    /// The peer's advertised candidates as delivered by the registry
    /// endpoint blob (registration order; the LAN-first ordering happens
    /// at send time).
    candidates: Vec<SocketAddr>,
    /// First candidate as the "peer endpoint" the list/op results report.
    peer_endpoint: Option<SocketAddr>,
    /// The verified peer key to pin when dialing — the transcript-verified
    /// device fingerprint ak-session relayed with the pairing.
    peer_fp: Option<Fingerprint>,
    code_id: [u8; 16],
    code: String,
}

/// One pending receive-consent request: the fingerprint-attributed
/// connection (whose disconnect resolves the request, wave-29 P2-07) plus
/// the deciding half of the oneshot the engine's receive body holds. The
/// request exists exactly until it is decided: `ak_task_consent`, the
/// 30 s auto-decline timer, a disconnect of its connection or runtime
/// teardown REMOVE the entry and resolve the channel (dropping the sender
/// half unparked would fail closed anyway — removing it keeps
/// `ak_task_consent` honestly `-4` afterwards).
struct PendingConsent {
    conn: Option<[u8; 16]>,
    sender: oneshot::Sender<bool>,
}

/// One accepted op's idempotency record: the minted op id plus the
/// DOMAIN-NAMESPACED canonical spec the request was accepted for
/// (wave-29 P2-01: `op|conn|canonical-json` — the same request id under a
/// different op type, a different connection or a different spec is a
/// conflict, never a false replay hit).
type TaskRequestRecord = ([u8; 16], String);

/// A popped task event plus the queue it came from (the shim pushes the
/// line back to THE queue on `-3`).
type PolledTaskLine = (Option<String>, Arc<StdMutex<VecDeque<String>>>);

/// One registered task code in the ENGINE-LOCAL CACHE (module doc: the
/// registry row is authoritative; this is the same-engine lookup layer).
pub(crate) struct TaskCodeEntry {
    pub code_id: [u8; 16],
    role: TaskRole,
    /// The display form as the registry minted it (`AKI1-…`).
    display: String,
    /// The owner runtime. The table is per engine and torn down with it,
    /// so the Arc cannot outlive the engine; teardown still removes this
    /// runtime's entries to break the table↔runtime Arc cycle deterministically.
    owner: Arc<TaskRt>,
    /// The joiner runtime once a SAME-ENGINE join redeemed the code
    /// (cross-device joiners are invisible here — by design).
    joiner: Option<Arc<TaskRt>>,
    /// The connection id once the code was paired (same-engine only).
    conn: Option<[u8; 16]>,
    /// Flipped when the un-joined code was cancelled through
    /// `ak_task_disconnect` (wave-29 P2-04): the registry row is revoked
    /// and the listener resources released; repeat disconnects of the
    /// same code id stay idempotent `AK_OK`s.
    cancelled: AtomicBool,
}

/// The per-runtime ak-session task-pairing stack: one registry client,
/// one orchestrator (journal + trust book under `id_dir`), one
/// `TaskPairing` on top, plus the resident tasks that keep it alive — the
/// task-aware owner loop (answers redeems of this device's codes and
/// completes pairings, behind the pre-commit redeem-offer gate) and the
/// event pump ([`TaskEvent`]s → task event lines). Wave 30 (review
/// 2026-11-07, P2-08): there is NO registry-connection watcher any more —
/// a control-plane disconnect is not a task verdict; the data planes of
/// paired tasks are independent of the registry.
struct TaskStack {
    pairing: TaskPairing,
    owner_loop: StdMutex<Option<JoinHandle<()>>>,
    event_pump: StdMutex<Option<JoinHandle<()>>>,
    /// Wave 34 (Android "unknown task code" after idle): the registry
    /// drops idle sessions at its 120 s SESSION_TTL, and a code owner
    /// sitting on "waiting for the peer" goes silently offline — the
    /// phone's join then gets 0x2101 PAIR_UNAVAILABLE ("owner offline")
    /// even though the GUI still shows a live code. This keeper does two
    /// things on the SHARED registry client: a keepalive ping well inside
    /// the TTL (so the session never idles out), and a reconnect whenever
    /// the client reports Disconnected (server restart, NAT drop, WiFi
    /// change) — pairing-plane subscriptions survive reconnection by
    /// design, so the owner loop keeps answering redeems across the gap.
    keeper: StdMutex<Option<JoinHandle<()>>>,
}

impl TaskStack {
    /// Abort the resident tasks (teardown; the runtime is still alive —
    /// closing the client afterwards unblocks their awaits).
    fn abort_tasks(&self) {
        if let Ok(mut slot) = self.owner_loop.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
        if let Ok(mut slot) = self.event_pump.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
        if let Ok(mut slot) = self.keeper.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }
}

/// One task runtime (the `ak_task_start` artefact), keyed by runtime id in
/// the engine's [`crate::pairing::V2State`] task table.
pub(crate) struct TaskRt {
    pub(crate) runtime_id: [u8; 16],
    pub(crate) id_dir: PathBuf,
    pub(crate) key: DeviceKeyPair,
    pub(crate) device_fp: Fingerprint,
    /// The wire-legal display name (rides the registry Hello and the
    /// endpoint blob the peer sees).
    pub(crate) device_name: String,
    /// Registry endpoint + pin the task-pairing stack dials.
    pub(crate) registry_addr: SocketAddr,
    pub(crate) registry_id: [u8; 32],
    /// The one default save folder (`ak_task_set_save_dir`).
    pub(crate) save_dir: StdMutex<PathBuf>,
    /// Op table — the SAME forever-readable semantics as the pairing
    /// plane (accepted results stay readable after a stop).
    pub(crate) ops: StdMutex<HashMap<[u8; 16], Arc<OpEntry>>>,
    /// Task-plane event lines (read through `ak_pairing_poll_event`).
    pub(crate) events: Arc<StdMutex<VecDeque<String>>>,
    /// Connections of this runtime (`ak_task_list`).
    pub(crate) conns: StdMutex<HashMap<[u8; 16], TaskConn>>,
    /// Pending receive consents keyed by request id (`ak_task_consent`).
    /// Each entry owns the deciding oneshot half; the engine's parked
    /// receive holds the other (wave-29 P2-07: nothing blocks a worker).
    pending: StdMutex<HashMap<[u8; 16], PendingConsent>>,
    /// Resident-receiver + probe-echo tasks (aborted by teardown),
    /// keyed by code id (a local placeholder id until the registry
    /// assigns the real one). One code owns SEVERAL tasks — the receiver
    /// and the echo responder — so each key holds its handles as a set.
    spawned: StdMutex<HashMap<[u8; 16], Vec<JoinHandle<()>>>>,
    /// Flipped by teardown; every consent gate checks it first.
    stopped: StdMutex<bool>,
    /// request_id idempotency: request id → (op id, canonical spec).
    request_ids: StdMutex<HashMap<[u8; 16], TaskRequestRecord>>,
    /// The lazily built ak-session stack (first create/join op). The
    /// build itself is serialized by [`TaskRt::stack_init`] (wave-29
    /// P2-02): exactly one registry login ever runs per runtime, so
    /// concurrent first ops can neither duplicate the client nor strand
    /// a replaced control connection.
    stack: StdMutex<Option<Arc<TaskStack>>>,
    /// One-at-a-time gate for the lazy stack build (P2-02).
    stack_init: tokio::sync::Mutex<()>,
    /// In-flight op ids per connection (a disconnect aborts them).
    conn_ops: StdMutex<HashMap<[u8; 16], Vec<[u8; 16]>>>,
    /// The engine-shared task-code cache (module doc: engine sharing
    /// without engine borrows).
    codes: Arc<TaskCodeTable>,
    /// The engine-wide progress queue (`ak_poll_progress`).
    engine_progress: Arc<StdMutex<VecDeque<String>>>,
}

impl TaskRt {
    /// Teardown (engine drop): invalidate waiters FIRST — every pending
    /// consent is resolved with an explicit decline by DROPPING its
    /// deciding half (the parked engine receive fails closed on the
    /// vanished oneshot, wave-29 P2-07: no worker is blocked, so there is
    /// no condvar to wake). Order matters for the same reason the
    /// wave-26 P1-05 Dart sequence does: waiters, then owners.
    pub(crate) fn teardown(&self, runtime: &tokio::runtime::Runtime) {
        {
            let mut stopped = match self.stopped.lock() {
                Ok(slot) => slot,
                Err(_) => return,
            };
            *stopped = true;
        }
        {
            // Resolve every pending consent with a declination: dropping
            // the sender half unparks the receive (fail-closed Err → the
            // engine declines) and leaves `ak_task_consent` honestly -4.
            if let Ok(mut pending) = self.pending.lock() {
                pending.clear();
            }
        }
        if let Ok(mut spawned) = self.spawned.lock() {
            for (_, tasks) in spawned.drain() {
                for task in tasks {
                    task.abort();
                }
            }
        }
        if let Ok(ops) = self.ops.lock() {
            for entry in ops.values() {
                if let Ok(mut slot) = entry.task.lock() {
                    if let Some(task) = slot.take() {
                        task.abort();
                    }
                }
            }
            // Entries stay: results remain readable post-stop (frozen).
        }
        // Take the registry stack down: abort the resident owner loop /
        // watcher / pump first (waiters before owners, same order as
        // above), then — wave-29 P2-04 — revoke this runtime's own
        // un-cancelled codes BEST-EFFORT and bounded (an app exit must
        // not leave active registry rows no local secret can answer for),
        // then close the control client bounded — an open client would
        // otherwise hold the TCP connection past the engine drop.
        let stack = self.stack.lock().ok().and_then(|mut slot| slot.take());
        if let Some(stack) = stack {
            stack.abort_tasks();
            let owned: Vec<[u8; 16]> = self
                .codes
                .lock()
                .map(|table| {
                    table
                        .values()
                        .filter(|entry| {
                            entry.owner.runtime_id == self.runtime_id
                                && !entry.cancelled.load(Ordering::Acquire)
                        })
                        .map(|entry| entry.code_id)
                        .collect()
                })
                .unwrap_or_default();
            let pairing = stack.pairing.clone();
            let _ = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), async {
                    for code_id in owned {
                        let _ = pairing.disconnect(code_id).await;
                    }
                })
                .await
            });
            let client = stack.pairing.orchestrator().client().clone();
            let _ = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), client.close()).await
            });
        }
        // Drop this runtime's cached code registrations: it breaks the
        // table↔runtime Arc cycle AND un-advertises codes whose owner is
        // going away (codes not revoked above simply expire on their TTL;
        // the cache only mirrors them for same-engine lookups).
        if let Ok(mut table) = self.codes.lock() {
            table.retain(|_, entry| entry.owner.runtime_id != self.runtime_id);
        }
        // Bounded settle window: aborted tasks land at their next await
        // point. The empty future keeps the timeout on the runtime's own
        // clock.
        let _ = runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(500), std::future::ready(())).await
        });
    }

    fn is_stopped(&self) -> bool {
        matches!(self.stopped.lock(), Ok(guard) if *guard)
    }

    fn push_event(&self, line: String) {
        if let Ok(mut queue) = self.events.lock() {
            if queue.len() < MAX_QUEUED_EVENTS {
                queue.push_back(line);
            }
        }
    }

    /// The connection entry for one code (a code carries at most one
    /// connection in v1).
    fn conn_of_code(&self, code_id: &[u8; 16]) -> Option<TaskConn> {
        let conns = self.conns.lock().ok()?;
        conns
            .values()
            .find(|conn| &conn.code_id == code_id)
            .cloned()
    }

    /// Flip one connection to `disconnected` and emit the event line —
    /// exactly once per connection (already-severed entries are silent).
    /// Returns the conn id when THIS call was the one that severed it.
    fn push_disconnected_once(&self, conn_id: &[u8; 16], reason: &str) -> Option<[u8; 16]> {
        let mut conns = self.conns.lock().ok()?;
        let conn = conns.get_mut(conn_id)?;
        if conn.state == ConnState::Disconnected {
            return None;
        }
        conn.state = ConnState::Disconnected;
        self.push_event(format!(
            "{{\"event\":\"disconnected\",\"conn\":\"{}\",\"reason\":{}}}",
            core::hex_encode(conn_id),
            quoted(reason),
        ));
        Some(*conn_id)
    }

    /// Record an in-flight op under its connection (a disconnect aborts
    /// it; the outcome is stamped so the op never reads `running` again).
    fn track_conn_op(&self, conn_id: [u8; 16], op_id: [u8; 16]) {
        if let Ok(mut map) = self.conn_ops.lock() {
            map.entry(conn_id).or_default().push(op_id);
        }
    }

    /// Abort every in-flight op of one connection and stamp a terminal
    /// outcome (the QUIC endpoint's drop is what tells the peer).
    fn abort_conn_ops(&self, conn_id: &[u8; 16]) {
        let op_ids = self
            .conn_ops
            .lock()
            .ok()
            .and_then(|mut map| map.remove(conn_id))
            .unwrap_or_default();
        let Ok(ops) = self.ops.lock() else {
            return;
        };
        for op_id in op_ids {
            let Some(entry) = ops.get(&op_id) else {
                continue;
            };
            if let Ok(mut slot) = entry.task.lock() {
                if let Some(task) = slot.take() {
                    task.abort();
                }
            }
            if let Ok(mut outcome) = entry.outcome.lock() {
                if matches!(&*outcome, OpOutcome::Running { .. }) {
                    *outcome = OpOutcome::Finished {
                        ok: false,
                        json: "{\"op\":\"task_send\",\"error\":\"connection disconnected\"}"
                            .to_string(),
                    };
                }
            }
        }
    }

    /// The connection one offer is attributed to (wave-29 P1-04): the
    /// entry of THIS code whose verified peer fingerprint equals the
    /// offer's actual TLS sender and which has not reached a terminal
    /// state. `None` means the sender is not this task's paired peer —
    /// the offer is refused without ever surfacing to the user.
    fn conn_for_sender(&self, code_id: &[u8; 16], sender: &Fingerprint) -> Option<TaskConn> {
        let conns = self.conns.lock().ok()?;
        conns
            .values()
            .find(|conn| {
                &conn.code_id == code_id
                    && conn.state != ConnState::Disconnected
                    && conn.peer_fp == Some(*sender)
            })
            .cloned()
    }

    /// Resolve every pending consent of one connection with a
    /// declination (a disconnect must wake its parked offers — the sender
    /// learns over the wire, the user's stale banner answers `-4`). Each
    /// resolution is a REQUEST-TERMINAL event (wave-30 P2-11 residual):
    /// `request_closed`/`disconnected` closes the banner at the same
    /// moment the request dies, not when a mirror timer guesses.
    fn decline_pending_of_conn(&self, conn_id: &[u8; 16]) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        let doomed: Vec<[u8; 16]> = pending
            .iter()
            .filter(|(_, entry)| entry.conn.as_ref() == Some(conn_id))
            .map(|(job, _)| *job)
            .collect();
        for job in doomed {
            if let Some(entry) = pending.remove(&job) {
                // The terminal event is queued BEFORE the oneshot resolves
                // (the same ordering the expiry timer uses): the request's
                // visible end and its resolution are never observably
                // apart.
                if let Some(conn) = entry.conn {
                    self.push_event(format!(
                        "{{\"event\":\"request_closed\",\"job\":\"{}\",\"conn\":\"{}\",\"reason\":\"disconnected\"}}",
                        core::hex_encode(&job),
                        core::hex_encode(&conn),
                    ));
                }
                let _ = entry.sender.send(false);
            }
        }
    }
}

/// Register one pending receive consent and hand back its pending half
/// (wave-29 P2-07: this returns IMMEDIATELY — the engine's receive body
/// awaits the oneshot, so no Tokio worker is ever parked on the user's
/// decision). The offer is first attributed by VERIFIED SENDER
/// FINGERPRINT (wave-29 P1-04): an offer that does not match the paired
/// connection of the listening code is refused on the spot (`Now(false)`)
/// — no `incoming_request` event, no attribution to an innocent
/// connection. The frozen `window` auto-decline runs as its own timer
/// task; a runtime teardown or a disconnect of the attributed connection
/// resolves the request by removing the entry.
///
/// `save_dir` is the transfer's BOUND target (wave-31 P2-06): the ONE
/// directory binding the resident receiver resolved when this
/// connection was accepted — the same value the engine's `ReceiveSpec`
/// writes into and the `done` event reports. The event never re-reads
/// the global setting, so the banner, the disk result and the terminal
/// event can never disagree.
///
/// Wave-31 R30-P2-03: the pending INSERT and the `incoming_request`
/// PUBLICATION are ONE critical section on the pending table, with the
/// connection's liveness re-checked INSIDE it (lock order: pending →
/// conns and pending → events, nested nowhere in reverse —
/// [`TaskRt::decline_pending_of_conn`] and the expiry timer take the
/// pending lock alone, and [`task_disconnect`] releases the conns lock
/// before its decline path runs). A disconnect that races the
/// registration therefore always lands on one side of the section:
/// either it severed the connection first — the re-check sees
/// `Disconnected`, the offer is refused on the spot and NO
/// `incoming_request` ever surfaces for the dead request — or the entry
/// is already inserted and published, in which case the decline's
/// `request_closed` (emitted under the SAME lock) can only land AFTER
/// the `incoming_request`. A late incoming after `request_closed` is
/// structurally impossible.
fn register_consent(
    rt: &Arc<TaskRt>,
    code_id: &[u8; 16],
    info: &ConsentInfo,
    save_dir: &std::path::Path,
    window: Duration,
) -> ConsentAnswer {
    if rt.is_stopped() {
        return ConsentAnswer::Now(false);
    }
    let Some(conn) = rt.conn_for_sender(code_id, &info.sender_fingerprint) else {
        // Not the paired peer of this code (pre-pairing dial that slipped
        // past an earlier authorize shape, or a terminal connection):
        // fail closed, silently — the user is never asked.
        return ConsentAnswer::Now(false);
    };
    let job = random_id16();
    let (sender, receiver) = oneshot::channel();
    {
        let Ok(mut pending) = rt.pending.lock() else {
            return ConsentAnswer::Now(false);
        };
        // Liveness re-check INSIDE the critical section (wave-31
        // R30-P2-03): the attribution above ran without the pending lock,
        // and a concurrent disconnect may have severed the connection in
        // that window. An offer on an already-closed connection is
        // refused here — no pending entry, no event, the parked receive
        // fails closed on the vanished oneshot.
        let still_live = rt
            .conns
            .lock()
            .map(|conns| {
                conns
                    .get(&conn.conn_id)
                    .is_some_and(|entry| entry.state != ConnState::Disconnected)
            })
            .unwrap_or(false);
        if !still_live {
            return ConsentAnswer::Now(false);
        }
        pending.insert(
            job,
            PendingConsent {
                conn: Some(conn.conn_id),
                sender,
            },
        );
        // Published while STILL holding the pending lock — the same
        // ordering discipline `decline_pending_of_conn` uses for
        // `request_closed`, so the two lines are totally ordered by the
        // one lock that guards the request's existence.
        let mut line = String::with_capacity(192);
        line.push_str(&format!(
            "{{\"event\":\"incoming_request\",\"job\":\"{}\",\"conn\":\"{}\"",
            core::hex_encode(&job),
            core::hex_encode(&conn.conn_id),
        ));
        // saveDir (wave-31 P2-06): the transfer's ONE binding — resolved
        // by the resident receiver when the connection was accepted, the
        // same directory the engine's ReceiveSpec writes into. Not a
        // fresh read of the global setting: the banner can only ever
        // show the target that is actually used.
        line.push_str(&format!(
            ",\"root\":{},\"files\":{},\"bytes\":{},\"saveDir\":{}",
            crate::core::json::quoted(&info.root_name),
            info.file_count,
            info.total_bytes,
            crate::core::json::quoted(&save_dir.to_string_lossy()),
        ));
        line.push('}');
        rt.push_event(line);
    }
    // The frozen 30 s auto-decline: a timer task, never a blocked thread.
    // Wave-30 P2-11 residual: the expiry is a REQUEST-TERMINAL event, not
    // just a silent resolution — the native source of truth the UI mirror
    // timer only guessed at (`consent_expired`).
    let timer_rt = Arc::clone(rt);
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        if let Some(entry) = timer_rt
            .pending
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&job))
        {
            // The terminal event is queued BEFORE the oneshot resolves:
            // whoever observes the resolution (a parked receive, a test)
            // can already read the request's terminal state.
            if let Some(conn) = entry.conn {
                timer_rt.push_event(format!(
                    "{{\"event\":\"consent_expired\",\"job\":\"{}\",\"conn\":\"{}\"}}",
                    core::hex_encode(&job),
                    core::hex_encode(&conn),
                ));
            }
            let _ = entry.sender.send(false);
        }
    });
    ConsentAnswer::Pending(receiver)
}

fn quoted(text: &str) -> String {
    crate::core::json::quoted(text)
}

fn type_error(key: &str, wanted: &str) -> FfiError {
    FfiError::invalid(format!("field \"{key}\" must be {wanted}"))
}

fn field_str(
    args: &crate::core::json::Json,
    key: &str,
    required: bool,
) -> Result<Option<String>, FfiError> {
    match args.get(key) {
        None | Some(crate::core::json::Json::Null) if required => Err(FfiError::invalid(format!(
            "missing required field \"{key}\""
        ))),
        None | Some(crate::core::json::Json::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| type_error(key, "string")),
    }
}

fn field_u64(args: &crate::core::json::Json, key: &str) -> Result<Option<u64>, FfiError> {
    match args.get(key) {
        None | Some(crate::core::json::Json::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| type_error(key, "unsigned integer")),
    }
}

/// Parse exactly 64 hex characters into 32 bytes.
fn hex32(text: &str, what: &str) -> Result<[u8; 32], FfiError> {
    let mut out = [0u8; 32];
    decode_hex_into(text, &mut out, what)?;
    Ok(out)
}

/// Parse exactly 32 hex characters into 16 bytes.
fn hex16(text: &str, what: &str) -> Result<[u8; 16], FfiError> {
    let mut out = [0u8; 16];
    decode_hex_into(text, &mut out, what)?;
    Ok(out)
}

fn decode_hex_into(text: &str, out: &mut [u8], what: &str) -> Result<(), FfiError> {
    let want = out.len() * 2;
    if text.len() != want {
        return Err(FfiError::invalid(format!(
            "{what} must be exactly {want} hex characters"
        )));
    }
    for (index, slot) in out.iter_mut().enumerate() {
        let hi = hex_digit(text.as_bytes()[index * 2], what)?;
        let lo = hex_digit(text.as_bytes()[index * 2 + 1], what)?;
        *slot = (hi << 4) | lo;
    }
    Ok(())
}

fn hex_digit(byte: u8, what: &str) -> Result<u8, FfiError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(FfiError::invalid(format!(
            "{what} contains a non-hex character"
        ))),
    }
}

/// The code-table key: uppercased, separators (`-`, spaces) stripped —
/// `AKT1-ABCD-1234-EF56`, `akt1 abcd 1234 ef56` and `AKT1ABCD1234EF56`
/// are the SAME code (the UI's join input may arrive in any of these
/// forms; the display form stays `AKT1-XXXX-XXXX-XXXX`).
fn normalize_code_key(code: &str) -> String {
    code.trim()
        .to_uppercase()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect()
}

/// Look one task runtime up by id (`-4` with the plane's wording).
pub(crate) fn task_runtime_of(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
) -> Result<Arc<TaskRt>, FfiError> {
    let v2 = engine
        .v2
        .lock()
        .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
    v2.tasks.get(runtime_id).cloned().ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown task runtime {}",
            core::hex_encode(runtime_id)
        ))
    })
}

// ---------------------------------------------------------------------------
// ak_task_start.
// ---------------------------------------------------------------------------

/// Start one task runtime. Registry fields mirror `ak_pairing_start` (the
/// Dart layer resolves them from `AK_REGISTRY_ADDR`/`AK_REGISTRY_ID` env
/// or the compiled `--dart-define` defaults, then hands them to the spec);
/// the default save folder is `<id_dir>/downloads`.
pub fn task_start(engine: &AkEngine, spec: &str) -> Result<[u8; 16], FfiError> {
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let registry_text = field_str(&args, "registry", true)?
        .ok_or_else(|| FfiError::invalid("missing \"registry\""))?;
    let registry_addr: SocketAddr = registry_text.parse().map_err(|_| {
        FfiError::invalid(format!(
            "field \"registry\" is not an ip:port address: {registry_text:?}"
        ))
    })?;
    let registry_hex = field_str(&args, "registry_id", true)?
        .ok_or_else(|| FfiError::invalid("missing \"registry_id\""))?;
    let registry_id = hex32(&registry_hex, "field \"registry_id\"")?;
    let id_dir =
        field_str(&args, "id_dir", true)?.ok_or_else(|| FfiError::invalid("missing \"id_dir\""))?;
    if id_dir.is_empty() {
        return Err(FfiError::invalid("field \"id_dir\" must be non-empty"));
    }
    let device_name = field_str(&args, "device_name", false)?.unwrap_or_else(|| "ak-task".into());
    if device_name.chars().count() > 64 {
        return Err(FfiError::invalid(
            "field \"device_name\" must be at most 64 characters",
        ));
    }
    let trimmed_dir = id_dir.trim_end_matches(['/', '\\']);
    let save_dir_text = field_str(&args, "save_dir", false)?
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format!("{trimmed_dir}/downloads"));
    // The save folder is created eagerly: ak-engine's LocalSaveTarget
    // requires an existing directory, so a missing default would fail the
    // receiver-role code creation later (better to fail at start).
    std::fs::create_dir_all(&save_dir_text).map_err(|error| {
        FfiError::invalid(format!("cannot create save dir {save_dir_text}: {error}"))
    })?;

    // Local, synchronous bootstrap (disk under id_dir, no network).
    let dir = PathBuf::from(&id_dir);
    std::fs::create_dir_all(&dir).map_err(|error| {
        FfiError::invalid(format!("cannot create id_dir {}: {error}", dir.display()))
    })?;
    let key_path = dir.join("device.key");
    let (key, _created) = DeviceKeyPair::load_or_create(&key_path).map_err(|error| {
        FfiError::invalid(format!(
            "cannot load device key at {}: {error}",
            key_path.display()
        ))
    })?;
    let device_fp = key.fingerprint();
    let runtime_id = random_id16();
    let (codes, engine_progress) = {
        let v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        (Arc::clone(&v2.task_codes), engine.events_queue())
    };
    let rt = Arc::new(TaskRt {
        runtime_id,
        id_dir: dir,
        key,
        device_fp,
        device_name,
        registry_addr,
        registry_id,
        save_dir: StdMutex::new(PathBuf::from(save_dir_text)),
        ops: StdMutex::new(HashMap::new()),
        events: Arc::new(StdMutex::new(VecDeque::new())),
        conns: StdMutex::new(HashMap::new()),
        pending: StdMutex::new(HashMap::new()),
        spawned: StdMutex::new(HashMap::new()),
        stopped: StdMutex::new(false),
        request_ids: StdMutex::new(HashMap::new()),
        stack: StdMutex::new(None),
        stack_init: tokio::sync::Mutex::new(()),
        conn_ops: StdMutex::new(HashMap::new()),
        codes,
        engine_progress,
    });
    {
        let mut v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        v2.tasks.insert(runtime_id, Arc::clone(&rt));
    }
    rt.push_event(format!(
        "{{\"event\":\"runtime_ready\",\"runtime\":\"{}\"}}",
        core::hex_encode(&runtime_id),
    ));
    Ok(runtime_id)
}

// ---------------------------------------------------------------------------
// Candidate endpoints (the wave-25 CLI derivation, task-plane variant).
// ---------------------------------------------------------------------------

/// Dialable local candidates for a wildcard bind: every interface IPv4
/// with the bound port, plus an explicit loopback entry (the CLI drops
/// loopback because same-host sends pass an explicit `--to`; the
/// zero-address task plane keeps it so a same-host join always has a
/// working candidate — `is_ipv4_private` counts loopback as private, so
/// it rides WITHIN the LAN group, insertion-ordered after the interface
/// addresses).
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

/// LAN preference for one candidate (RFC1918 private IPv4 — loopback
/// included by ak-net's `is_ipv4_private` — and loopback IPv6; a routing
/// hint, never an authorization — the CLI's `send_target_prefers_lan`).
fn prefers_lan(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => ak_net::classify::is_ipv4_private(v4),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Dedupe (normalizing v4-mapped IPv6 to plain IPv4), then stable-sort
/// LAN-first: within a group the registration order survives.
pub(crate) fn order_candidates(targets: Vec<SocketAddr>) -> Vec<SocketAddr> {
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

/// Parse the optional `candidates` field: an array of `ip:port` strings.
fn parse_candidates(args: &crate::core::json::Json) -> Result<Vec<SocketAddr>, FfiError> {
    match args.get("candidates") {
        None | Some(crate::core::json::Json::Null) => Ok(Vec::new()),
        Some(value) => {
            let texts = value
                .as_str_array()
                .ok_or_else(|| type_error("candidates", "an array of ip:port strings"))?;
            let mut out = Vec::with_capacity(texts.len());
            for text in &texts {
                let addr: SocketAddr = text.parse().map_err(|_| {
                    FfiError::invalid(format!(
                        "field \"candidates\" holds a non ip:port entry: {text:?}"
                    ))
                })?;
                out.push(addr);
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// The registry task-pairing stack (lazily built by the first create/join).
// ---------------------------------------------------------------------------

/// Build (or fetch) this runtime's [`TaskStack`]: a REAL v2 registry
/// client (pinned TLS, the runtime's device key and display name), the
/// orchestrator (journal + trust book under `id_dir`), the `TaskPairing`
/// on top, and its resident tasks — the task-aware owner loop (behind the
/// pre-commit redeem-offer gate, P1-05) and the event pump. Runs inside
/// op bodies, i.e. on the engine's embedded runtime (the client's reader
/// tasks spawn there).
///
/// The build is serialized by [`TaskRt::stack_init`] (wave-29 P2-02):
/// concurrent first ops await the gate and then find the finished stack,
/// so exactly ONE registry login ever runs — no duplicate device login
/// (the server's "new connection closes the old" would strand the saved
/// stack), no orphaned replacement client. A failed build leaves the slot
/// empty, so a later op retries cleanly.
async fn ensure_stack(rt: &Arc<TaskRt>) -> Result<Arc<TaskStack>, String> {
    if let Some(stack) = rt
        .stack
        .lock()
        .map_err(|_| "task stack lock poisoned".to_string())?
        .clone()
    {
        return Ok(stack);
    }
    if rt.is_stopped() {
        return Err("task runtime is stopped".to_string());
    }
    // One initializer at a time; the gate is released on both success and
    // failure (a failed login must be retryable by the next op).
    let init_gate = rt.stack_init.lock().await;
    if let Some(stack) = rt
        .stack
        .lock()
        .map_err(|_| "task stack lock poisoned".to_string())?
        .clone()
    {
        return Ok(stack);
    }
    if rt.is_stopped() {
        return Err("task runtime is stopped".to_string());
    }
    let connect = RegistryClient::connect_with_config(
        rt.registry_addr,
        rt.registry_id,
        &rt.key,
        &rt.device_name,
        ClientConfig::default(),
    );
    let client = match tokio::time::timeout(REGISTRY_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return Err(format!("registry connect failed: {error}")),
        Err(_) => return Err("registry connect timed out".to_string()),
    };
    let orch = PairingOrchestrator::open(
        client,
        rt.key.clone(),
        &rt.id_dir,
        OrchestratorConfig {
            // The v1 task window a code owner has to answer a redeem: a
            // live owner loop answers in milliseconds; an unknown code,
            // an offline owner or a revoked code surfaces the unified
            // "unavailable" answer at this deadline instead of hanging
            // the join op for the 90 s library default.
            redeem_window_ms: TASK_REDEEM_WINDOW_MS,
            ..OrchestratorConfig::default()
        },
    )
    .map_err(|error| {
        format!(
            "cannot open the task pairing journal under {}: {error}",
            rt.id_dir.display()
        )
    })?;
    let pairing = TaskPairing::open(
        orch,
        TaskPairingConfig {
            ttl_secs: TASK_CODE_TTL_SECS,
            probe: TASK_PROBE_OPTIONS,
        },
    );
    let stack = Arc::new(TaskStack {
        pairing,
        owner_loop: StdMutex::new(None),
        event_pump: StdMutex::new(None),
        keeper: StdMutex::new(None),
    });
    // Wave 36 (R36-P1-02): the shared connection keeper from ak-session
    // (RegistryClient::spawn_connection_keeper — the same component the
    // CLI task plane and the daemon use since this wave): 20s ping inside
    // the 30s FRAME_IO_DEADLINE, bounded-backoff reconnect on Disconnected
    // (a failed batch now terminal-publishes Disconnected, so recovery
    // continues across prolonged outages), 3-timeout black-hole recovery.
    // Shutdown rides the runtime-stopped watch: stop flips it, the keeper
    // exits; abort_tasks still aborts the handle for prompt teardown.
    {
        let client = stack.pairing.orchestrator().client().clone();
        // Shutdown: poll the runtime-stopped flag at the ping cadence —
        // abort_tasks() also aborts the handle on teardown, so the poll
        // is a cooperative fast path, not the only exit.
        let stop_rt = Arc::clone(rt);
        let (keeper_stop_tx, keeper_stop_rx) =
            tokio::sync::watch::channel(false);
        let _watcher = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                if stop_rt.is_stopped() {
                    let _ = keeper_stop_tx.send(true);
                    break;
                }
                interval.tick().await;
            }
        });
        let keeper = client.spawn_connection_keeper(keeper_stop_rx);
        if let Ok(mut slot) = stack.keeper.lock() {
            *slot = Some(keeper);
        }
    }
    // Wave 30 (review 2026-11-07, P1-05): the PRODUCTION owner loop runs
    // behind the same pre-commit redeem-offer gate the library's
    // `TaskPairing::spawn_owner_loop` installs. This stack deliberately
    // spawns `spawn_owner_loop_verified` directly (the synchronous
    // fingerprint registration below must run inside the completion
    // callback), so the gate is installed explicitly FIRST — without it a
    // wrong-direction or endpoint-less join completed the pairing and
    // burned the code's single redeem before the post-completion check
    // refused it.
    stack.pairing.install_redeem_offer_gate();
    // The task-aware owner loop: ak-session's own completion bookkeeping
    // (direction rule + active-task registry) PLUS the task plane's —
    // the connection entry, `connected` event and probe. `spawn_owner_
    // loop_verified` invokes the callback inside the runtime, so the
    // completion work can be spawned straight away.
    let owner_stack = Arc::clone(&stack);
    let owner_rt = Arc::clone(rt);
    let on_paired: Arc<dyn Fn(VerifiedPairing) + Send + Sync> = Arc::new(move |verified| {
        let code_id = verified.record().transcript.code_id;
        let my_direction = owner_stack
            .pairing
            .active_tasks()
            .into_iter()
            .find(|task| task.code == code_id)
            .map(|task| task.direction);
        let Some(direction) = my_direction else {
            return; // not one of this runtime's task codes
        };
        // Completion refused (direction conflict / malformed blob): the
        // pairing stands server-side but this side refuses to register
        // the task — no `connected` event is the honest surface
        // (ak-session logged the refusal on its own stream).
        if let Ok(paired) = owner_stack
            .pairing
            .complete_owner_pairing(&verified, direction)
        {
            // The connection entry and `connected` event are inserted
            // SYNCHRONOUSLY here, before any await: the resident
            // receiver's P1-04 admission is bound to this entry, and the
            // peer's join may already be returning on the other device —
            // its send can dial this endpoint within milliseconds.
            owner_side_bookkeeping(&owner_rt, code_id, &paired);
            let rt = Arc::clone(&owner_rt);
            let stack = Arc::clone(&owner_stack);
            tokio::spawn(async move {
                owner_side_probe(&rt, &stack, paired).await;
            });
        }
    });
    let owner_loop = stack
        .pairing
        .orchestrator()
        .spawn_owner_loop_verified(Some(on_paired));
    let pump = spawn_task_event_pump(Arc::clone(rt), &stack);
    let mut unbuilt: Option<[JoinHandle<()>; 2]> = Some([owner_loop, pump]);
    // Install under the stack lock, re-checking `stopped` INSIDE it: either
    // teardown took the slot first (we see `stopped` and abort what we
    // built) or we install first (teardown then finds and closes our
    // stack). No live registry client can slip past a teardown.
    {
        let mut slot = rt
            .stack
            .lock()
            .map_err(|_| "task stack lock poisoned".to_string())?;
        if !rt.is_stopped() {
            if let Some([owner_loop, pump]) = unbuilt.take() {
                if let Ok(mut handle) = stack.owner_loop.lock() {
                    *handle = Some(owner_loop);
                }
                if let Ok(mut handle) = stack.event_pump.lock() {
                    *handle = Some(pump);
                }
            }
            *slot = Some(Arc::clone(&stack));
        }
    }
    if let Some(tasks) = unbuilt {
        // Teardown ran mid-build: nothing may install a live registry
        // client past the engine's death — abort what we built.
        for task in tasks {
            task.abort();
        }
        let client = stack.pairing.orchestrator().client().clone();
        let _ = tokio::time::timeout(Duration::from_millis(200), client.close()).await;
        return Err("task runtime is stopped".to_string());
    }
    drop(init_gate);
    Ok(stack)
}

/// Pump the [`TaskEvent`] broadcast into the task event queue. `Local`
/// ends are skipped — `ak_task_disconnect` pushes that line directly, so
/// the pump repeating it would duplicate the event.
fn spawn_task_event_pump(rt: Arc<TaskRt>, stack: &Arc<TaskStack>) -> JoinHandle<()> {
    let mut events = stack.pairing.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(TaskEvent::TaskDisconnected { code, reason }) => {
                    if reason == TaskDisconnectReason::Local {
                        continue;
                    }
                    let Some(conn) = rt.conn_of_code(&code) else {
                        continue;
                    };
                    // The frozen ffi-v2 §8.3 vocabulary: `peer` for a peer
                    // departure, `receiver_failed` for a data-plane link
                    // loss. Wave 30 (review 2026-11-07, P2-08): a
                    // registry/control-connection drop no longer produces
                    // `Network` at all — ak-session's task layer only
                    // reports it for the task's OWN data plane now, so
                    // `receiver_failed` is always a genuine data-plane
                    // terminal verdict and the Dart terminal-reason table
                    // agrees with it.
                    let text = match reason {
                        TaskDisconnectReason::ByPeer => "peer",
                        _ => "receiver_failed",
                    };
                    rt.push_disconnected_once(&conn.conn_id, text);
                }
                // A slow consumer lagging the 64-slot ring just misses
                // events; the state flip already happened either way.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// The owner side of a completed registry pairing, synchronous half: the
/// connection entry (conn id = the pairing transaction's id, identical on
/// both sides) and the `connected` event. Runs inside the owner-loop
/// callback (see `ensure_stack`) so the entry exists before the peer's
/// returning join op can possibly dial this endpoint.
fn owner_side_bookkeeping(rt: &Arc<TaskRt>, code_id: [u8; 16], paired: &TaskPaired) {
    if rt.conn_of_code(&code_id).is_some() {
        return; // duplicate completion (Hello catch-up re-push): dedup
    }
    let role = TaskRole::of_direction(paired.direction);
    let conn_id = paired.pairing_id;
    let candidates = paired.peer.candidates.clone();
    let endpoint = order_candidates(candidates.clone())
        .first()
        .copied()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    let display = display_of_code(rt, &code_id);
    {
        let Ok(mut conns) = rt.conns.lock() else {
            return;
        };
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role,
                state: ConnState::Probing,
                candidates: candidates.clone(),
                peer_endpoint: order_candidates(candidates.clone()).first().copied(),
                peer_fp: Some(paired.verified.redeemer_fingerprint()),
                code_id,
                code: display.clone(),
            },
        );
    }
    rt.push_event(format!(
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{endpoint},\"code\":{}}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: both sides of the pairing share the SAME
        // code id — the joiner (which never saw the create result) fills
        // its conn→code mapping from this field.
        core::hex_encode(&code_id),
        quoted(role.as_str()),
        quoted(&display),
    ));
}

/// The owner side of a completed registry pairing, asynchronous half: the
/// real reachability probe over the peer's advertised candidates and the
/// connection's honest `probing → connected|unreachable` settle (wave-30
/// P2-09 residual: a total probe failure settles to `unreachable`, never
/// a fake `connected`).
async fn owner_side_probe(rt: &Arc<TaskRt>, stack: &Arc<TaskStack>, paired: TaskPaired) {
    let conn_id = paired.pairing_id;
    let endpoint_probe = PeerEndpoint {
        candidates: paired.peer.candidates.clone(),
        name: paired.peer.name.clone(),
        direction: paired.peer.direction,
    };
    let outcome = probe_with_retry(&stack.pairing, &endpoint_probe).await;
    settle_probe_state(rt, &conn_id, &outcome);
    let (line, _verdict) = probe_result_line(&conn_id, &outcome);
    rt.push_event(line);
}

/// Settle one connection's `probing` state by the probe's honest verdict
/// (wave-30 P2-09 residual): success → `connected`, total failure →
/// `unreachable`. A connection that already reached a terminal state
/// (`disconnected`) keeps it; `ak_task_list` and the join result agree
/// with the `probe_result` event.
fn settle_probe_state(rt: &Arc<TaskRt>, conn_id: &[u8; 16], outcome: &ProbeOutcome) {
    let reachable = matches!(outcome, ProbeOutcome::Success { .. });
    if let Ok(mut conns) = rt.conns.lock() {
        if let Some(conn) = conns.get_mut(conn_id) {
            if conn.state == ConnState::Probing {
                conn.state = if reachable {
                    ConnState::Connected
                } else {
                    ConnState::Unreachable
                };
            }
        }
    }
}

/// The owner's display form of one of its cached codes (fallback: the
/// code id hex — the registry display never travelled back on this path).
fn display_of_code(rt: &TaskRt, code_id: &[u8; 16]) -> String {
    rt.codes
        .lock()
        .ok()
        .and_then(|table| {
            table
                .values()
                .find(|entry| &entry.code_id == code_id)
                .map(|entry| entry.display.clone())
        })
        .unwrap_or_else(|| core::hex_encode(code_id))
}

/// One probe round, retried once after a short stagger (the peer's own
/// probe may start a beat later than ours on the slow path).
async fn probe_with_retry(pairing: &TaskPairing, endpoint: &PeerEndpoint) -> ProbeOutcome {
    let first = pairing.probe_peer(endpoint).await;
    if matches!(first, ProbeOutcome::Success { .. }) {
        return first;
    }
    tokio::time::sleep(TASK_PROBE_RETRY_PAUSE).await;
    let second = pairing.probe_peer(endpoint).await;
    if matches!(second, ProbeOutcome::Success { .. }) {
        second
    } else {
        first
    }
}

/// Render the `probe_result` event line + the join-result verdict of one
/// probe outcome (`reachable` carries the confirmed address, or false
/// with one bounded reason per candidate).
fn probe_result_line(conn_id: &[u8; 16], outcome: &ProbeOutcome) -> (String, &'static str) {
    match outcome {
        ProbeOutcome::Success { address } => (
            format!(
                "{{\"event\":\"probe_result\",\"conn\":\"{}\",\"endpoint\":{},\"reachable\":true}}",
                core::hex_encode(conn_id),
                quoted(&address.to_string()),
            ),
            "verified",
        ),
        ProbeOutcome::AllFailed { failures } => {
            let list = failures
                .iter()
                .map(|failure| {
                    format!(
                        "{{\"candidate\":{},\"reason\":{}}}",
                        quoted(&failure.candidate.to_string()),
                        quoted(failure.reason),
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            (
                format!(
                    "{{\"event\":\"probe_result\",\"conn\":\"{}\",\"endpoint\":null,\"reachable\":false,\"failures\":[{list}]}}",
                    core::hex_encode(conn_id),
                ),
                "all_failed",
            )
        }
    }
}

/// Map an ak-session task-pairing failure onto the task plane's bounded
/// op error strings (the registry never reveals whether a code exists —
/// unknown, expired and revoked all answer `PAIR_UNAVAILABLE`, so the
/// joiner's honest wording covers all three).
fn task_error_message(error: TaskError) -> String {
    match &error {
        TaskError::Flow(PairFlowError::Rejected { code })
            if *code == ak_signal::protocol_v2::message::REDEEM_EXHAUSTED =>
        {
            "the task code was already joined (one code pairs exactly once)".to_string()
        }
        // RATE_LIMITED (0x2102, pairing-flow.md §12): the device exhausted
        // its hourly invite-creation budget. Actionable copy beats the raw
        // code — the user learns it REFILLS and roughly when.
        TaskError::Flow(PairFlowError::Rejected { code })
            if *code == ak_signal::protocol_v2::message::RATE_LIMITED =>
        {
            "rate limited: too many codes created this hour; the budget refills within an hour — wait and retry".to_string()
        }
        TaskError::Flow(PairFlowError::Unavailable { .. }) => {
            "unknown task code (or expired, revoked, owner offline)".to_string()
        }
        TaskError::Flow(PairFlowError::BadCode(cause)) => {
            format!("task code rejected: {cause}")
        }
        other => format!("registry task pairing failed: {other}"),
    }
}

/// Bind the probe echo responder: a small UDP socket that answers
/// ak-net hole-punch probe packets from any of this device's advertised
/// addresses, so the peer's `probe_peer` genuinely confirms two-way
/// reachability (the QUIC receiver itself never speaks the probe
/// exchange). Registered under `key` in the spawned table (teardown /
/// disconnect abort it).
///
/// Anti-reflection (wave-29 review P2-12): replies carry the fixed
/// all-zero nonce and a reply is NEVER itself answered, so two reachable
/// responders cannot loop answering each other (a real prober's nonce is
/// time/counter-derived and never all-zero); each source endpoint
/// additionally gets [`ECHO_REPLY_BUDGET`] replies per
/// [`ECHO_REPLY_WINDOW`] (a genuine probe round sends ~7 packets/s), and
/// the source table is hard-capped, so even a spoofed flood cannot
/// sustain unbounded reply traffic or memory growth.
fn spawn_echo_responder(rt: &Arc<TaskRt>, key: [u8; 16]) -> Result<SocketAddr, String> {
    let std_socket = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|error| format!("cannot bind the probe responder: {error}"))?;
    std_socket
        .set_nonblocking(true)
        .map_err(|error| format!("cannot arm the probe responder: {error}"))?;
    let addr = std_socket
        .local_addr()
        .map_err(|error| format!("probe responder bind address: {error}"))?;
    let socket = tokio::net::UdpSocket::from_std(std_socket)
        .map_err(|error| format!("cannot register the probe responder: {error}"))?;
    let task = tokio::spawn(async move {
        let mut buffer = [0u8; 1500];
        let mut budgets: HashMap<SocketAddr, (std::time::Instant, u32)> = HashMap::new();
        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((len, source)) => {
                    if !ak_net::holepunch::is_probe_packet(&buffer[..len]) {
                        continue;
                    }
                    // A reply (all-zero nonce) is never answered again —
                    // the loop-killer. A genuine probe's nonce is
                    // time/counter-derived and cannot be all-zero.
                    if buffer[8..len] == ECHO_REPLY_NONCE {
                        continue;
                    }
                    let now = std::time::Instant::now();
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
                            .send_to(&ak_net::holepunch::probe_packet(&ECHO_REPLY_NONCE), source)
                            .await;
                    }
                }
                Err(_) => return,
            }
        }
    });
    if let Ok(mut spawned) = rt.spawned.lock() {
        spawned.entry(key).or_default().push(task);
    }
    Ok(addr)
}

/// Assemble the endpoint-blob candidate list of one side: the dial
/// candidates (real receiver port first / caller extras verbatim / the
/// machine's interfaces), then — in the derived path only — the probe
/// echo port, deduped, capped, and shrunk until the blob fits the wire
/// bound (a defensive loop; the caps keep it far below 512 bytes).
fn blob_candidates(
    dial: Vec<SocketAddr>,
    extras: Vec<SocketAddr>,
    echo: Vec<SocketAddr>,
    name: &str,
) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::with_capacity(dial.len() + extras.len() + echo.len());
    for addr in dial.into_iter().take(MAX_DIAL_CANDIDATES) {
        let addr = ak_net::classify::expand_mapped(addr);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    for addr in extras {
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
            name: name.to_string(),
            direction: TaskDirection::Send,
        };
        if ak_session::pairing::encode_endpoint_blob(&endpoint).is_ok() {
            break;
        }
        out.pop();
    }
    out
}

/// Abort and forget the resident tasks registered under `from` and
/// re-register them under the registry-assigned `code_id` (the bind
/// happens before the claim, under a local placeholder id).
fn rekey_spawned(rt: &TaskRt, from: [u8; 16], code_id: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&from) {
            spawned.entry(code_id).or_default().extend(tasks);
        }
    }
}

/// Abort the resident tasks registered under a placeholder id (the op
/// failed after binding — nothing may leak past the failed op).
fn abort_spawned(rt: &TaskRt, key: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&key) {
            for task in tasks {
                task.abort();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ops: validation, idempotency, launcher.
// ---------------------------------------------------------------------------

/// The validated body of one task op. All `-2` validation happens BEFORE
/// an op id exists.
enum TaskOpSpec {
    CreateCode {
        role: TaskRole,
        candidates: Vec<SocketAddr>,
        ttl_secs: u32,
    },
    Join {
        code: String,
        candidates: Vec<SocketAddr>,
        /// The joiner's own side, when the spec said it (cross-device the
        /// code's declaration is unreadable before the redeem, and a
        /// wrong guess burns the single redeem — so the spec may say it;
        /// same-engine codes mirror the cached declaration instead, and
        /// the v1 default is the receiver side).
        role_hint: Option<TaskRole>,
    },
    Send {
        conn_id: [u8; 16],
        paths: Vec<String>,
    },
}

impl TaskOpSpec {
    fn op(&self) -> &'static str {
        match self {
            TaskOpSpec::CreateCode { .. } => "task_create_code",
            TaskOpSpec::Join { .. } => "task_join",
            TaskOpSpec::Send { .. } => "task_send",
        }
    }
}

/// Canonical rendering of a parsed spec (sorted keys, recursively) — the
/// request_id digest form (same policy as `sched_enqueue`, P2-07).
fn canonical_spec_json(value: &crate::core::json::Json) -> String {
    use crate::core::json::Json;
    fn render_into(out: &mut String, value: &Json) {
        match value {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(token) => out.push_str(token),
            Json::Str(text) => out.push_str(&crate::core::json::quoted(text)),
            Json::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    render_into(out, item);
                }
                out.push(']');
            }
            Json::Obj(members) => {
                let mut sorted: Vec<&(String, Json)> = members.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                out.push('{');
                for (index, (key, item)) in sorted.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&crate::core::json::quoted(key));
                    out.push(':');
                    render_into(out, item);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    render_into(&mut out, value);
    out
}

/// The domain-namespaced canonical form of one request (wave-29 P2-01):
/// op type + connection id (sends) + the sorted-JSON spec. Two requests
/// collide only when they are the SAME operation for the SAME connection
/// with the SAME spec; anything else under the same `request_id` is a
/// conflict, never a false replay hit.
fn canonical_request(op: &str, conn: Option<[u8; 16]>, args: &crate::core::json::Json) -> String {
    match conn {
        Some(conn_id) => format!(
            "{op}|{}|{}",
            core::hex_encode(&conn_id),
            canonical_spec_json(args)
        ),
        None => format!("{op}|-|{}", canonical_spec_json(args)),
    }
}

/// The outcome of one idempotency gate pass.
enum RequestGate {
    /// Exact replay of an already-accepted request: the caller returns
    /// the ORIGINAL op id and answer — no new side effect, and none of
    /// the live-state re-validation below applies (a replay answers even
    /// after its connection later disconnected).
    Replay([u8; 16]),
    /// Fresh request; its op id is reserved AND its running op entry
    /// registered atomically with the check (wave-30 R29-P2-03): a
    /// concurrent retry that replays this op id can poll it immediately
    /// (`running`, never the `-4` of an unregistered op). The caller must
    /// launch exactly this reservation.
    Fresh(ReservedOp),
    /// The request carried no `request_id`: nothing to dedupe, the op id
    /// is minted by the launcher.
    NoId,
}

/// One atomically accepted request: the minted op id plus the RUNNING op
/// entry the gate already published in the op table (wave-30 R29-P2-03 —
/// reservation and registration share the request registry's critical
/// section, so no other thread can observe the id before the entry).
struct ReservedOp {
    op_id: [u8; 16],
    entry: Arc<OpEntry>,
}

/// The ONE atomic acceptance point of the task plane (wave-29 P2-01):
/// replay check, conflict rejection, (for sends) the live-connection
/// rule, the reservation of the minted op id AND the registration of its
/// RUNNING op entry (wave-30 R29-P2-03) all happen inside a single
/// critical section on the request registry, so two racing identical
/// requests can never both launch and a replayed op id is always already
/// pollable. `enforce_live_conn` (sends) refuses a FRESH request whose
/// connection is already disconnected — after the replay branch, so an
/// accepted request's answer stays retrievable forever. Lock order is
/// request_ids → conns and request_ids → ops; no other path takes either
/// pair together in reverse.
fn gate_request(
    rt: &TaskRt,
    op: &'static str,
    conn: Option<[u8; 16]>,
    enforce_live_conn: bool,
    args: &crate::core::json::Json,
) -> Result<RequestGate, FfiError> {
    let Some(request_text) = field_str(args, "request_id", false)? else {
        if enforce_live_conn {
            // No dedupe key: still refuse a send on a severed connection
            // before any op exists.
            ensure_conn_live(rt, &conn.expect("send gate carries a conn id"))?;
        }
        return Ok(RequestGate::NoId);
    };
    let request_id = hex16(&request_text, "field \"request_id\"")?;
    let canonical = canonical_request(op, conn, args);
    let mut registry = rt
        .request_ids
        .lock()
        .map_err(|_| FfiError::internal("task request-id registry lock poisoned"))?;
    match registry.get(&request_id) {
        Some((op_id, previous)) if *previous == canonical => Ok(RequestGate::Replay(*op_id)),
        Some(_) => Err(FfiError::invalid(format!(
            "request_id {} was already accepted with a different spec",
            core::hex_encode(&request_id),
        ))),
        None => {
            if enforce_live_conn {
                // Fresh requests only — replays above keep answering.
                ensure_conn_live(rt, &conn.expect("send gate carries a conn id"))?;
            }
            let op_id = random_id16();
            let entry = Arc::new(OpEntry {
                outcome: StdMutex::new(OpOutcome::Running { op }),
                task: StdMutex::new(None),
            });
            // Wave-30 R29-P2-03: the RUNNING entry is registered in the
            // SAME critical section that reserves the id (lock order
            // request_ids → ops, taken nowhere in reverse). A concurrent
            // retry can only replay an op id whose entry is already
            // pollable — the reserve/publish window that returned a
            // not-yet-existing op id (first poll `-4`) is closed.
            let mut ops = rt
                .ops
                .lock()
                .map_err(|_| FfiError::internal("task op table lock poisoned"))?;
            ops.insert(op_id, Arc::clone(&entry));
            drop(ops);
            registry.insert(request_id, (op_id, canonical));
            Ok(RequestGate::Fresh(ReservedOp { op_id, entry }))
        }
    }
}

/// The live-connection rule of a FRESH send: unknown → `-4`, already
/// severed → `-2` (nothing was reserved yet — refusal is side-effect
/// free). Caller holds the request-registry lock (lock order above).
fn ensure_conn_live(rt: &TaskRt, conn_id: &[u8; 16]) -> Result<(), FfiError> {
    let conns = rt
        .conns
        .lock()
        .map_err(|_| FfiError::internal("task connection table lock poisoned"))?;
    match conns.get(conn_id) {
        None => Err(FfiError::not_found(format!(
            "unknown connection {} on task runtime {}",
            core::hex_encode(conn_id),
            core::hex_encode(&rt.runtime_id),
        ))),
        Some(conn) if conn.state == ConnState::Disconnected => Err(FfiError::invalid(
            "the connection is disconnected".to_string(),
        )),
        Some(_) => Ok(()),
    }
}

/// Shared op launcher (the pairing plane's `launch_op`, task-plane
/// shape): register the entry, spawn the body on the embedded runtime,
/// stamp the terminal JSON (kept for lossless repeated reads), announce
/// `op_done`/`op_failed` in the task event queue. The body owns ONLY
/// `Arc` clones — never an engine borrow (module doc). `reserved` is the
/// idempotency gate's atomic reservation (id + already-registered RUNNING
/// entry, wave-30 R29-P2-03); `None` mints and registers a fresh entry
/// for requests without a `request_id`.
fn launch_task_op(
    engine: &AkEngine,
    rt: &Arc<TaskRt>,
    spec: TaskOpSpec,
    reserved: Option<ReservedOp>,
) -> Result<[u8; 16], FfiError> {
    let op = spec.op();
    let (op_id, entry) = match reserved {
        // Already registered (atomically with the request reservation).
        Some(reserved) => (reserved.op_id, reserved.entry),
        None => {
            let op_id = random_id16();
            let entry = Arc::new(OpEntry {
                outcome: StdMutex::new(OpOutcome::Running { op }),
                task: StdMutex::new(None),
            });
            {
                let mut ops = rt
                    .ops
                    .lock()
                    .map_err(|_| FfiError::internal("task op table lock poisoned"))?;
                ops.insert(op_id, Arc::clone(&entry));
            }
            (op_id, entry)
        }
    };
    // Sends are abortable by a disconnect of their connection.
    if let TaskOpSpec::Send { conn_id, .. } = &spec {
        rt.track_conn_op(*conn_id, op_id);
    }
    let task = {
        let entry = Arc::clone(&entry);
        let rt = Arc::clone(rt);
        engine.runtime().spawn(async move {
            let (ok, json) = match run_task_op(&rt, spec).await {
                Ok(json) => (true, json),
                Err(message) => (
                    false,
                    format!("{{\"op\":{},\"error\":{}}}", quoted(op), quoted(&message)),
                ),
            };
            if let Ok(mut slot) = entry.outcome.lock() {
                if matches!(&*slot, OpOutcome::Running { .. }) {
                    *slot = OpOutcome::Finished {
                        ok,
                        json: json.clone(),
                    };
                }
            }
            rt.push_event(format!(
                "{{\"event\":\"{}\",\"op_id\":\"{}\",\"op\":{}}}",
                if ok { "op_done" } else { "op_failed" },
                core::hex_encode(&op_id),
                quoted(op),
            ));
        })
    };
    if let Ok(mut slot) = entry.task.lock() {
        *slot = Some(task);
    }
    Ok(op_id)
}

// ---------------------------------------------------------------------------
// ak_task_create_code / ak_task_join / ak_task_send (acceptance).
// ---------------------------------------------------------------------------

/// `ak_task_create_code`: claim a task code at the REGISTRY under the
/// chosen role (endpoint blob + `max_redeems = 1`; one code pairs once).
/// A RECEIVER-role creation binds the resident receiver first; the op
/// completes with the registry's display code only after the bind and the
/// claim both succeeded. An absent or `0` `ttl_secs` maps to the task-code
/// default [`TASK_CODE_TTL_SECS`] (wave-29 P2-04: a v1 code never lingers
/// as a never-expiring registry row).
pub fn task_create_code_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let role_text =
        field_str(&args, "role", true)?.ok_or_else(|| FfiError::invalid("missing \"role\""))?;
    let role = TaskRole::parse(&role_text)?;
    let candidates = parse_candidates(&args)?;
    let ttl_secs = field_u64(&args, "ttl_secs")?.unwrap_or(0);
    if ttl_secs > u64::from(u32::MAX) {
        return Err(FfiError::invalid("field \"ttl_secs\" is too large"));
    }
    // P2-04: 0/absent = the ten-minute task-code default, never the
    // registry's "long-lived" semantics.
    let ttl_secs = if ttl_secs == 0 {
        TASK_CODE_TTL_SECS
    } else {
        ttl_secs as u32
    };
    let reserved = match gate_request(&rt, "task_create_code", None, false, &args)? {
        RequestGate::Replay(op_id) => return Ok(op_id),
        RequestGate::Fresh(reserved) => Some(reserved),
        RequestGate::NoId => None,
    };
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::CreateCode {
            role,
            candidates,
            ttl_secs,
        },
        reserved,
    )
}

/// `ak_task_join`: redeem a live task code AT THE REGISTRY (the frozen
/// pairing transcript to `PairReady`). The joiner's role is the opposite
/// of the code's declaration — resolved from the optional spec `role`
/// (the joiner's own side), else the engine cache (same-engine codes),
/// else the v1 default (`receiver`); a receiver-side joiner binds its
/// resident receiver before the op completes. The joiner gets its
/// `connected` + `probe_result` events; the owner's arrive from its
/// resident owner loop.
pub fn task_join_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let code =
        field_str(&args, "code", true)?.ok_or_else(|| FfiError::invalid("missing \"code\""))?;
    if code.trim().is_empty() {
        return Err(FfiError::invalid("field \"code\" must be non-empty"));
    }
    let role_hint = match field_str(&args, "role", false)? {
        None => None,
        Some(text) => Some(TaskRole::parse(&text)?),
    };
    let candidates = parse_candidates(&args)?;
    let reserved = match gate_request(&rt, "task_join", None, false, &args)? {
        RequestGate::Replay(op_id) => return Ok(op_id),
        RequestGate::Fresh(reserved) => Some(reserved),
        RequestGate::NoId => None,
    };
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::Join {
            code: normalize_code_key(&code),
            candidates,
            role_hint,
        },
        reserved,
    )
}

/// `ak_task_send`: run a real engine send over the connection's candidate
/// endpoints (LAN-first, 10 s split connect budget, automatic fallback).
/// The idempotency gate runs AFTER the spec validates but BEFORE the
/// live-connection rule, so an ACCEPTED request's answer stays retrievable
/// even after the connection later disconnected (wave-29 P2-01).
pub fn task_send_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    conn_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    {
        // Unknown connection refuses before anything else (-4, the frozen
        // validation order: runtime, conn, spec).
        let conns = rt
            .conns
            .lock()
            .map_err(|_| FfiError::internal("task connection table lock poisoned"))?;
        if !conns.contains_key(conn_id) {
            return Err(FfiError::not_found(format!(
                "unknown connection {} on task runtime {}",
                core::hex_encode(conn_id),
                core::hex_encode(runtime_id),
            )));
        }
    }
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let paths = match args.get("paths") {
        None | Some(crate::core::json::Json::Null) => {
            return Err(FfiError::invalid("missing required field \"paths\""));
        }
        Some(value) => value
            .as_str_array()
            .ok_or_else(|| type_error("paths", "an array of strings"))?,
    };
    if paths.is_empty() {
        return Err(FfiError::invalid("field \"paths\" must not be empty"));
    }
    if paths.iter().any(|path| path.is_empty()) {
        return Err(FfiError::invalid(
            "entries of \"paths\" must be non-empty paths",
        ));
    }
    // Replay answers first; a FRESH send additionally requires a live
    // connection (inside the gate's atomic acceptance).
    let reserved = match gate_request(&rt, "task_send", Some(*conn_id), true, &args)? {
        RequestGate::Replay(op_id) => return Ok(op_id),
        RequestGate::Fresh(reserved) => Some(reserved),
        RequestGate::NoId => None,
    };
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::Send {
            conn_id: *conn_id,
            paths,
        },
        reserved,
    )
}

// ---------------------------------------------------------------------------
// ak_task_list / disconnect / set_save_dir / consent.
// ---------------------------------------------------------------------------

/// `ak_task_list`: the connection snapshot plus the current save folder.
pub fn task_list(engine: &AkEngine, runtime_id: &[u8; 16]) -> Result<String, FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let conns = rt
        .conns
        .lock()
        .map_err(|_| FfiError::internal("task connection table lock poisoned"))?;
    let save_dir = rt
        .save_dir
        .lock()
        .map_err(|_| FfiError::internal("save dir lock poisoned"))?
        .clone();
    let mut out = String::with_capacity(192);
    out.push_str(&format!(
        "{{\"runtime\":\"{}\",\"save_dir\":{}",
        core::hex_encode(&rt.runtime_id),
        quoted(&save_dir.to_string_lossy()),
    ));
    out.push_str(",\"connections\":[");
    let mut sorted: Vec<&TaskConn> = conns.values().collect();
    sorted.sort_by_key(|conn| conn.conn_id);
    for (index, conn) in sorted.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&render_conn(conn));
    }
    out.push_str("]}");
    Ok(out)
}

fn render_conn(conn: &TaskConn) -> String {
    let endpoint = match conn.peer_endpoint {
        Some(addr) => quoted(&addr.to_string()),
        None => "null".to_string(),
    };
    format!(
        "{{\"conn\":\"{}\",\"role\":{},\"state\":{},\"peer_endpoint\":{endpoint},\"code\":{}}}",
        core::hex_encode(&conn.conn_id),
        quoted(conn.role.as_str()),
        quoted(conn.state.as_str()),
        quoted(&conn.code),
    )
}

/// `ak_task_disconnect`: sever one connection — or CANCEL a code that was
/// generated but never joined (wave-29 P2-04: the id argument then names
/// the create result's `code_id`; the registry row is revoked, the
/// receiver/echo listeners released, a `code_cancelled` event emitted).
///
/// For a connection: the local side gets its `disconnected`/`local` line
/// immediately; the code is revoked AT THE REGISTRY (a pending redeem of
/// it aborts server-side); in-flight send ops and the pending consents of
/// the connection are resolved; and — wave-29 P2-03 — while IDLE (no live
/// transfer whose QUIC drop the peer would observe) the disconnecting
/// side additionally dials the paired peer's advertised data endpoint and
/// hands it one authenticated TASK-CLOSE Error frame, so the LISTENING
/// peer ends the pairing ByPeer (`disconnected`/`peer`) right away
/// instead of staying "connected". A peer that never bound a data
/// listener (a pure sender side) cannot be reached this way and learns
/// honestly at its next send. A SAME-ENGINE peer additionally gets the
/// instant cache notification. Idempotent: an already-severed connection
/// — and an already-cancelled code — are silent `AK_OK`s.
pub fn task_disconnect(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    conn_id: &[u8; 16],
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let code_id = {
        let mut conns = rt
            .conns
            .lock()
            .map_err(|_| FfiError::internal("task connection table lock poisoned"))?;
        let Some(conn) = conns.get_mut(conn_id) else {
            drop(conns);
            return cancel_unjoined_code(engine, &rt, conn_id);
        };
        if conn.state == ConnState::Disconnected {
            return Ok(());
        }
        conn.state = ConnState::Disconnected;
        conn.code_id
    };
    rt.push_event(format!(
        "{{\"event\":\"disconnected\",\"conn\":\"{}\",\"reason\":\"local\"}}",
        core::hex_encode(conn_id),
    ));
    // Wake this connection's parked consents (P2-07: a disconnect must
    // resolve its pending offers, not wait out the 30 s window).
    rt.decline_pending_of_conn(conn_id);
    // Abort this connection's in-flight ops and resident tasks (the
    // spawned map holds the resident receiver + probe responder of the
    // code; dropping them closes the data plane toward the peer).
    rt.abort_conn_ops(conn_id);
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&code_id) {
            for task in tasks {
                task.abort();
            }
        }
    }
    // Same-engine instant notify through the cache (the registry path
    // below is the cross-device one).
    let stack = rt.stack.lock().ok().and_then(|slot| slot.clone());
    if let Ok(table) = rt.codes.lock() {
        if let Some(entry) = table.values().find(|entry| entry.code_id == code_id) {
            let peer = if entry.owner.runtime_id == rt.runtime_id {
                entry.joiner.clone()
            } else {
                Some(Arc::clone(&entry.owner))
            };
            if let Some(peer) = peer {
                peer.push_disconnected_once(conn_id, "peer");
            }
        }
    }
    // The idle close notification (P2-03): tell the paired peer over its
    // authenticated data channel before the code goes away.
    let notify_conn = rt.conn_of_code(&code_id);
    let notify_rt = Arc::clone(&rt);
    engine.runtime().spawn(async move {
        if let Some(conn) = notify_conn {
            close_notify_peer(&notify_rt, &conn).await;
        }
    });
    // The registry disconnect: revoke the code server-side (aborting any
    // pending redeem) and end the local task — ak-session emits
    // `TaskDisconnected{Local}`, which the pump skips (the direct line
    // above already said it); the important half is the revoke.
    if let Some(stack) = stack {
        let pairing = stack.pairing.clone();
        engine.runtime().spawn(async move {
            let _ = pairing.disconnect(code_id).await;
        });
    }
    Ok(())
}

/// The P2-04 cancel path of [`task_disconnect`]: `code_id` names one of
/// THIS runtime's generated codes that has no connection yet (nobody
/// joined). Revoke the registry row, release the receiver/echo listeners,
/// emit `code_cancelled`, and mark the cache entry cancelled so repeat
/// disconnects stay silent `AK_OK`s. A code that was already cancelled is
/// a silent success; anything else is the frozen `-4`.
fn cancel_unjoined_code(
    engine: &AkEngine,
    rt: &Arc<TaskRt>,
    code_id: &[u8; 16],
) -> Result<(), FfiError> {
    // The code was already joined (a live pairing exists for it): the
    // disconnect is the CONNECTION's, not the un-joined cancel — delegate
    // so the pairing's bookkeeping, notification and revoke all run.
    if let Some(conn) = rt.conn_of_code(code_id) {
        let runtime_id = rt.runtime_id;
        return task_disconnect(engine, &runtime_id, &conn.conn_id);
    }
    let stack = rt.stack.lock().ok().and_then(|slot| slot.clone());
    let already = {
        let Ok(table) = rt.codes.lock() else {
            return Err(FfiError::internal("task code cache lock poisoned"));
        };
        let Some(entry) = table.values().find(|entry| &entry.code_id == code_id) else {
            return Err(FfiError::not_found(format!(
                "unknown connection {} on task runtime {}",
                core::hex_encode(code_id),
                core::hex_encode(&rt.runtime_id),
            )));
        };
        if entry.owner.runtime_id != rt.runtime_id {
            // Another runtime's code: not this caller's to cancel.
            return Err(FfiError::not_found(format!(
                "unknown connection {} on task runtime {}",
                core::hex_encode(code_id),
                core::hex_encode(&rt.runtime_id),
            )));
        }
        entry.cancelled.load(Ordering::Acquire)
    };
    if already {
        return Ok(()); // idempotent repeat cancel
    }
    // Abort the code's resident receiver + echo responder (release the
    // listeners) and revoke at the registry.
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(code_id) {
            for task in tasks {
                task.abort();
            }
        }
    }
    if let Some(stack) = stack {
        let pairing = stack.pairing.clone();
        let revoke_id = *code_id;
        engine.runtime().spawn(async move {
            let _ = pairing.disconnect(revoke_id).await;
        });
    }
    rt.push_event(format!(
        "{{\"event\":\"code_cancelled\",\"code_id\":\"{}\"}}",
        core::hex_encode(code_id),
    ));
    // Mark cancelled only after the side effects are armed; the flag is
    // what makes repeat disconnects idempotent (the event/revoke above
    // fire exactly once per code).
    if let Ok(table) = rt.codes.lock() {
        if let Some(entry) = table.values().find(|entry| &entry.code_id == code_id) {
            entry.cancelled.store(true, Ordering::Release);
        }
    }
    Ok(())
}

/// The authenticated idle-close notification (wave-29 P2-03): dial the
/// paired peer's advertised data candidates (LAN-first, family-matched,
/// TLS pinned to the pairing's verified fingerprint — the peer's receiver
/// admission accepts exactly that key) and deliver ONE
/// `ErrorCode::Cancelled` + [`TASK_CLOSE_MESSAGE`] Error frame. The
/// peer's resident receiver recognizes the marker, ends its pairing
/// ByPeer and stops; an unreachable peer is an honest miss (its next
/// send fails to connect), never a failed disconnect.
async fn close_notify_peer(rt: &Arc<TaskRt>, conn: &TaskConn) {
    let Some(pinned) = conn.peer_fp else {
        return;
    };
    let identity = make_self_signed(&rt.key);
    let candidates = order_candidates(conn.candidates.clone());
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
            endpoint.connect(addr, &pinned),
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
                    code: ErrorCode::Cancelled,
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

/// `ak_task_set_save_dir`: the ONE default save folder. The directory is
/// created eagerly so an unusable path fails at setting time, not at the
/// next receive.
pub fn task_set_save_dir(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    path: &str,
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    if path.is_empty() {
        return Err(FfiError::invalid("path must be non-empty"));
    }
    let dir = PathBuf::from(path);
    std::fs::create_dir_all(&dir).map_err(|error| {
        FfiError::invalid(format!("cannot create save dir {}: {error}", dir.display()))
    })?;
    let mut save_dir = rt
        .save_dir
        .lock()
        .map_err(|_| FfiError::internal("save dir lock poisoned"))?;
    *save_dir = dir;
    Ok(())
}

/// `ak_task_consent`: decide one pending `incoming_request` (accept or
/// decline). The decision removes the entry and resolves the parked
/// receive through its oneshot; an unknown or already-decided request
/// (the entry is gone the moment it was decided, auto-declined at the 30 s
/// window, resolved by a disconnect, or removed at teardown) is `-4`.
pub fn task_consent(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    job_id: &[u8; 16],
    accept: bool,
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let entry = {
        let mut pending = rt
            .pending
            .lock()
            .map_err(|_| FfiError::internal("pending consent table lock poisoned"))?;
        pending.remove(job_id)
    };
    let Some(entry) = entry else {
        return Err(FfiError::not_found(format!(
            "consent request {} is unknown or already decided",
            core::hex_encode(job_id),
        )));
    };
    // The parked receive may already be gone (transfer aborted): the send
    // half fails silently, the request was moot either way.
    let _ = entry.sender.send(accept);
    Ok(())
}

// ---------------------------------------------------------------------------
// The op bodies (async, on the embedded runtime; Arc-owned state only).
// ---------------------------------------------------------------------------

async fn run_task_op(rt: &Arc<TaskRt>, spec: TaskOpSpec) -> Result<String, String> {
    match spec {
        TaskOpSpec::CreateCode {
            role,
            candidates,
            ttl_secs,
        } => run_create_code(rt, role, candidates, ttl_secs).await,
        TaskOpSpec::Join {
            code,
            candidates,
            role_hint,
        } => run_join(rt, &code, candidates, role_hint).await,
        TaskOpSpec::Send { conn_id, paths } => run_send(rt, conn_id, paths).await,
    }
}

/// Claim one task code at the registry. A RECEIVER-role creation binds
/// the resident receiver FIRST (real wildcard bind + sequential serve
/// loop with the offer-level consent gate) and its real port leads the
/// advertised candidates; the op fails without claiming anything if the
/// bind fails. Derived advertisements additionally carry the probe echo
/// port (module doc); explicit `candidates` in the spec are advertised
/// verbatim instead.
async fn run_create_code(
    rt: &Arc<TaskRt>,
    role: TaskRole,
    extras: Vec<SocketAddr>,
    ttl_secs: u32,
) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let placeholder = random_id16();
    let explicit = !extras.is_empty();
    let mut dial = extras;
    let mut echo: Vec<SocketAddr> = Vec::new();
    let mut code_slot: Option<ReceiverCodeSlot> = None;
    if !explicit {
        let derived = if role == TaskRole::Receiver {
            let (bound, slot) = match bind_resident_receiver(rt, placeholder).await {
                Ok(bound) => bound,
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            };
            code_slot = Some(slot);
            local_candidates(bound)
        } else {
            // A sender-role code still advertises the creator's endpoints
            // (informational for v1 — the reverse direction's dial targets
            // arrive with the joiner's registration; port 0 marks "no
            // listener", which the probe skips as undialable).
            local_candidates(SocketAddr::from(([0, 0, 0, 0], 0)))
        };
        dial = derived;
        let responder = match spawn_echo_responder(rt, placeholder) {
            Ok(responder) => responder,
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        };
        echo = local_candidates(responder);
    } else if role == TaskRole::Receiver {
        // The receiver must exist (and its bind must succeed) even when
        // the caller advertises other addresses — its real port just
        // stays off the wire list.
        match bind_resident_receiver(rt, placeholder).await {
            Ok((_, slot)) => code_slot = Some(slot),
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        }
    }
    let blob = blob_candidates(
        dial,
        Vec::new(), // extras already folded into `dial` above
        echo,
        &rt.device_name,
    );
    if blob.is_empty() {
        abort_spawned(rt, placeholder);
        return Err("no dialable candidate to advertise".to_string());
    }
    let code = stack
        .pairing
        .create_task_code_with_ttl(role.direction(), &blob, ttl_secs)
        .await
        .map_err(|error| {
            abort_spawned(rt, placeholder);
            task_error_message(error)
        })?;
    rekey_spawned(rt, placeholder, code.invite.code_id);
    if let Some(slot) = code_slot {
        if let Ok(mut value) = slot.lock() {
            *value = code.invite.code_id;
        }
    }
    // Cache the code engine-locally (same-engine role mirroring, instant
    // peer notify, the owner's display lookup — module doc).
    {
        let mut table = rt
            .codes
            .lock()
            .map_err(|_| "task code cache lock poisoned".to_string())?;
        table.insert(
            normalize_code_key(&code.invite.display),
            TaskCodeEntry {
                code_id: code.invite.code_id,
                role,
                display: code.invite.display.clone(),
                owner: Arc::clone(rt),
                joiner: None,
                conn: None,
                cancelled: AtomicBool::new(false),
            },
        );
    }
    let endpoint = order_candidates(blob)
        .first()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    Ok(format!(
        "{{\"op\":\"task_create_code\",\"code\":{},\"role\":{},\"code_id\":\"{}\",\"expires_ms\":{},\"endpoint\":{}}}",
        quoted(&code.invite.display),
        quoted(role.as_str()),
        core::hex_encode(&code.invite.code_id),
        code.invite.expires_ms,
        endpoint,
    ))
}

/// Spawn the resident receiver for one code and wait (bounded) for its
/// first bound address. The task registers under `spawned` for teardown.
/// The registry code id of one resident receiver, filled in when the
/// claim/redeem returns (the receiver is bound BEFORE the registry
/// assigns the id, under a local placeholder key).
type ReceiverCodeSlot = Arc<StdMutex<[u8; 16]>>;

/// Spawn the resident receiver for one code and wait (bounded) for its
/// first bound address. The task registers under `key` in the spawned
/// table (teardown / disconnect abort it); the returned slot carries the
/// registry code id to the receiver once it is known.
async fn bind_resident_receiver(
    rt: &Arc<TaskRt>,
    key: [u8; 16],
) -> Result<(SocketAddr, ReceiverCodeSlot), String> {
    let (addr_tx, addr_rx) = oneshot::channel();
    let code_slot: ReceiverCodeSlot = Arc::new(StdMutex::new(key));
    let task = {
        let rt = Arc::clone(rt);
        let addr_tx = addr_tx;
        let code_slot = Arc::clone(&code_slot);
        tokio::spawn(async move {
            run_resident_receiver(rt, code_slot, addr_tx).await;
        })
    };
    rt.spawned
        .lock()
        .map_err(|_| "spawned task table lock poisoned".to_string())?
        .entry(key)
        .or_default()
        .push(task);
    match tokio::time::timeout(RECEIVER_BIND_TIMEOUT, addr_rx).await {
        Ok(Ok(addr)) => Ok((addr, code_slot)),
        Ok(Err(_)) => Err("the resident receiver stopped before binding".to_string()),
        Err(_) => Err("resident receiver bind timed out".to_string()),
    }
}

/// The resident receiver (wave-29 P1-04/P2-05/P2-06): bind ONE wildcard
/// endpoint (one advertised port) and serve a LOOP of connections until
/// the task reaches a terminal state.
///
/// * **Admission binds the pairing (P1-04)** — the accept `authorize`
///   admits ONLY the fingerprint ak-session's completed pairing verified
///   for this code's connection, and only while that connection is live.
///   Before the pairing completes there is no entry, so EVERY dial is
///   refused at the TLS gate and the loop keeps waiting: a stranger can
///   neither deliver an Offer nor consume the receiver. The consent hook
///   re-checks the attribution independently (fingerprint, not port).
/// * **Handshake failures are per-connection (wave-30 P1-04 residual)** —
///   a dial whose TLS handshake fails on OUR side of the accept
///   (`Transport`/`PeerIdentity`: a stranger's aborted handshake or a
///   rejected certificate) is recorded as a bounded `handshake_failed`
///   event and the loop keeps listening; only the endpoint's own death
///   or a task terminal state ends the listener. A stranger cannot end
///   the receiver by dialing badly.
/// * **Decline keeps the listener (P2-05)** — a DECLINED offer ends only
///   that connection; the sender may retry on the same code (a fresh
///   `incoming_request` fires).
/// * **Done is the single-transfer terminal state (P2-05)** — a completed
///   transfer ends the task: both sides converge their connection entries
///   to `disconnected` (`done`).
/// * **Per-task save folder (P2-06)** — the out dir is resolved from the
///   runtime's CURRENT setting each time a transfer starts, so
///   `ak_task_set_save_dir` applies to every subsequent task.
/// * **Idle close notification (P2-03)** — a TASK-CLOSE Error frame from
///   the paired peer ends the pairing ByPeer (the pump surfaces
///   `disconnected`/`peer`).
async fn run_resident_receiver(
    rt: Arc<TaskRt>,
    code_slot: ReceiverCodeSlot,
    first_addr: oneshot::Sender<SocketAddr>,
) {
    // The CURRENT registry code id (the placeholder until the claim or
    // redeem that owns this receiver assigned the real one).
    let current_code = || -> [u8; 16] {
        code_slot
            .lock()
            .map(|slot| *slot)
            .unwrap_or_else(|poisoned| *poisoned.into_inner())
    };
    let identity = make_self_signed(&rt.key);
    let endpoint =
        match ak_net::DirectEndpoint::bind(SocketAddr::from(([0, 0, 0, 0], 0)), &identity).await {
            Ok(endpoint) => endpoint,
            Err(_) => return, // the dropped oneshot fails the create/join op
        };
    let bound = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let _ = first_addr.send(bound);
    // Progress lines ride under the CONNECTION id (wave-31 R30-P2-02):
    // the pairing transaction's id is the ONE correlation key both sides
    // of a pairing hold — the sender's rows already use it, and a joiner
    // that typed someone else's code never learns the code id at all, so
    // the Dart side maps receive progress by conn id alone. The current
    // code id (the registry id once the claim/redeem that owns this
    // receiver filled the slot) resolves the connection entry; entries
    // are never removed, so a started transfer always finds its conn.
    let progress_queue = Arc::clone(&rt.engine_progress);
    let progress_slot = Arc::clone(&code_slot);
    let progress_rt = Arc::clone(&rt);
    let progress = move |event: Progress| {
        let code_id = progress_slot
            .lock()
            .map(|slot| *slot)
            .unwrap_or_else(|poisoned| *poisoned.into_inner());
        let job = progress_rt
            .conn_of_code(&code_id)
            .map(|conn| core::hex_encode(&conn.conn_id))
            // Unreachable for a started transfer (consent already
            // required a fingerprint-matched, live connection entry);
            // keep a diagnosable code-id row rather than dropping the
            // event silently.
            .unwrap_or_else(|| core::hex_encode(&code_id));
        if let Ok(mut queue) = progress_queue.lock() {
            core::push_capped(
                &mut queue,
                format!(
                    "{{\"job\":{},\"event\":{}}}",
                    quoted(&job),
                    quoted(core::progress_name(&event)),
                ),
            );
        }
    };
    loop {
        if rt.is_stopped() {
            return;
        }
        let code_id = current_code();
        // The task already reached a terminal state (done, ByPeer,
        // receiver_failed, local disconnect): stop listening.
        if rt
            .conn_of_code(&code_id)
            .is_some_and(|conn| conn.state == ConnState::Disconnected)
        {
            return;
        }
        // P1-04 admission: only this pairing's verified peer, and only
        // while its connection is live. A refused dial closes the
        // stranger's connection and the loop keeps listening.
        let authorize = |fp: &Fingerprint| -> bool {
            let code_id = current_code();
            rt.conn_of_code(&code_id).is_some_and(|conn| {
                conn.state != ConnState::Disconnected && conn.peer_fp == Some(*fp)
            })
        };
        let channel = match endpoint.accept(&authorize).await {
            Ok(channel) => channel,
            Err(ak_net::ConnectError::Unauthorized) => continue,
            Err(ak_net::ConnectError::Io(err))
                if err.kind() == std::io::ErrorKind::NotConnected
                    && err.to_string().contains("endpoint closed") =>
            {
                // The ENDPOINT itself is gone (teardown/driver dropped):
                // the one honest end of the listener.
                return;
            }
            Err(failure) => {
                // Wave 30 (review 2026-11-07, P1-04 residual): ONE
                // connection's handshake failed (a stranger's aborted
                // dial — Transport — or a certificate the server side
                // rejected — PeerIdentity). That is a per-connection
                // fact, never a listener verdict: record the bounded
                // event and keep serving; the listener only ends with a
                // task terminal state above or the endpoint's own death.
                // Wave-31 (R30-P3-01): the field is `code_id` (a 32hex
                // task-code id STRING, no conn — an unpaired dial has no
                // attributable connection), matching the frozen
                // contract shape and the Dart typed parser.
                rt.push_event(format!(
                    "{{\"event\":\"handshake_failed\",\"code_id\":\"{}\",\"reason\":{}}}",
                    core::hex_encode(&current_code()),
                    quoted(handshake_failure_word(&failure)),
                ));
                continue;
            }
        };
        // P2-06 (wave-31): the save folder is resolved ONCE per transfer
        // start — the moment this connection is accepted — and that ONE
        // binding is what the engine's ReceiveSpec writes into, what the
        // consent event shows and what `done` reports. No second read of
        // the global setting can skew them: a `set_save_dir` during the
        // pending window applies to the NEXT request, never the
        // in-flight one (the immutable-binding semantics the wave-28
        // report blessed for this fix).
        let out_dir = match rt.save_dir.lock() {
            Ok(dir) => dir.clone(),
            Err(_) => return,
        };
        // ak-engine's save target requires the directory to exist
        // (created at start/set time; re-created best-effort here in case
        // it was removed).
        let _ = std::fs::create_dir_all(&out_dir);
        let mut spec = ReceiveSpec::new(out_dir.clone(), identity.clone());
        spec.checkpoint_dir = rt.id_dir.join("checkpoints");
        spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
        spec.idle_timeout = TASK_SEND_IDLE;
        // The non-blocking consent hook (P2-07): registers + returns
        // pending. Built PER CONNECTION so the event's saveDir is THIS
        // transfer's binding (wave-31 P2-06) — the loop serves one
        // connection at a time, so the capture cannot race a later one.
        let consent: TransferConsent = {
            let rt = Arc::clone(&rt);
            let code_slot = Arc::clone(&code_slot);
            let bound_dir = out_dir.clone();
            Arc::new(move |info: &ConsentInfo| {
                let code_id = code_slot
                    .lock()
                    .map(|slot| *slot)
                    .unwrap_or_else(|poisoned| *poisoned.into_inner());
                register_consent(&rt, &code_id, info, &bound_dir, PENDING_CONSENT_WINDOW)
            })
        };
        let result = receive_accepted_consent(channel, spec, &progress, Some(consent)).await;
        match result {
            Ok(report) => {
                // The single-transfer terminal state: report and converge.
                let conn = rt.conn_of_code(&current_code());
                let mut line = String::with_capacity(160);
                line.push_str("{\"event\":\"done\"");
                if let Some(conn) = &conn {
                    line.push_str(&format!(
                        ",\"conn\":\"{}\"",
                        core::hex_encode(&conn.conn_id)
                    ));
                }
                // saveDir (wave-31 P2-06): the ACTUAL landing directory —
                // the same binding the consent event showed, so the UI
                // can display the real result of the finished transfer.
                line.push_str(&format!(
                    ",\"saveDir\":{}",
                    quoted(&out_dir.to_string_lossy()),
                ));
                line.push_str(&format!(
                    ",\"files\":{},\"bytes\":{}}}",
                    report.files, report.bytes,
                ));
                rt.push_event(line);
                if let Some(conn) = conn {
                    rt.push_disconnected_once(&conn.conn_id, "done");
                }
                return;
            }
            Err(EngineError::ConsentDeclined { .. }) => {
                // The user (or the 30 s window) declined: the pending
                // entry is resolved, the peer was told over the wire,
                // nothing landed on disk. Only THIS connection ended —
                // keep listening, the sender may retry on the same code.
                continue;
            }
            Err(EngineError::PeerError { message, .. }) if message == TASK_CLOSE_MESSAGE => {
                // The paired peer's authenticated idle-close (P2-03):
                // end the pairing ByPeer through the event pump.
                report_peer_departure(&rt, &current_code());
                return;
            }
            Err(_) => {
                // Channel-level failure (sender gave up mid-handshake,
                // peer disconnected mid-transfer, IO error). The task is
                // over; when the pairing had completed, this is the peer
                // going away — report ByPeer through the event pump (the
                // cross-device `disconnected`/`peer` notification).
                report_peer_departure(&rt, &current_code());
                return;
            }
        }
    }
}

/// Report one pairing as ended ByPeer when it had completed (the receiver
/// observed its peer's data plane depart).
fn report_peer_departure(rt: &Arc<TaskRt>, code_id: &[u8; 16]) {
    if rt.conn_of_code(code_id).is_some() {
        if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
            stack.pairing.report_peer_disconnect(*code_id);
        }
    }
}

/// Bounded reason word of one failed accept (wave-30 P1-04 residual): a
/// fixed two-word vocabulary, never the raw transport text (hostile peers
/// control parts of it).
fn handshake_failure_word(error: &ak_net::ConnectError) -> &'static str {
    match error {
        ak_net::ConnectError::PeerIdentity(_) => "peer certificate rejected",
        _ => "handshake transport failure",
    }
}

/// Join one live code AT THE REGISTRY: resolve the joiner's side (spec
/// hint → engine-cache mirror → the v1 receiver default), bind the
/// joiner's resident receiver + probe echo responder when the candidates
/// are derived, drive the frozen pairing transcript to `PairReady`
/// through `join_task_code`, then book the connection (conn id = the
/// pairing transaction id, identical on both sides), emit `connected`,
/// run the REAL reachability probe and report its honest verdict.
async fn run_join(
    rt: &Arc<TaskRt>,
    code: &str,
    extras: Vec<SocketAddr>,
    role_hint: Option<TaskRole>,
) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let my_role = role_hint
        .or_else(|| {
            rt.codes
                .lock()
                .ok()
                .and_then(|table| table.get(code).map(|entry| entry.role.opposite()))
        })
        .unwrap_or(TaskRole::Receiver);
    let placeholder = random_id16();
    let explicit = !extras.is_empty();
    let mut dial = extras;
    let mut echo: Vec<SocketAddr> = Vec::new();
    let mut code_slot: Option<ReceiverCodeSlot> = None;
    if !explicit {
        dial = if my_role == TaskRole::Receiver {
            match bind_resident_receiver(rt, placeholder).await {
                Ok((bound, slot)) => {
                    code_slot = Some(slot);
                    local_candidates(bound)
                }
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            }
        } else {
            local_candidates(SocketAddr::from(([0, 0, 0, 0], 0)))
        };
        let responder = match spawn_echo_responder(rt, placeholder) {
            Ok(responder) => responder,
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        };
        echo = local_candidates(responder);
    }
    let blob = blob_candidates(dial, Vec::new(), echo, &rt.device_name);
    if blob.is_empty() {
        abort_spawned(rt, placeholder);
        return Err("no dialable candidate to advertise".to_string());
    }
    let paired = match stack
        .pairing
        .join_task_code(code, &blob, my_role.direction())
        .await
    {
        Ok(paired) => paired,
        Err(error) => {
            abort_spawned(rt, placeholder);
            return Err(task_error_message(error));
        }
    };
    let code_id = paired.code_id;
    let conn_id = paired.pairing_id;
    let dial_candidates = paired.peer.candidates.clone();
    let peer_fp = paired.verified.owner_fingerprint();
    rekey_spawned(rt, placeholder, code_id);
    if let Some(slot) = code_slot {
        if let Ok(mut value) = slot.lock() {
            *value = code_id;
        }
    }
    // Joiner side bookkeeping.
    {
        let mut conns = rt
            .conns
            .lock()
            .map_err(|_| "connection table lock poisoned".to_string())?;
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: my_role,
                state: ConnState::Probing,
                candidates: dial_candidates.clone(),
                peer_endpoint: order_candidates(dial_candidates.clone()).first().copied(),
                peer_fp: Some(peer_fp),
                code_id,
                code: code.to_string(),
            },
        );
    }
    // Same-engine cache flip: the owner runtime (if any) learns the
    // joiner + conn id for its instant disconnect notify.
    {
        if let Ok(mut table) = rt.codes.lock() {
            if let Some(entry) = table.get_mut(code) {
                entry.conn = Some(conn_id);
                entry.joiner = Some(Arc::clone(rt));
            }
        }
    }
    // The reported endpoint is the LAN-first first candidate — the same
    // address a send would dial first.
    let endpoint = order_candidates(dial_candidates.clone())
        .first()
        .copied()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    rt.push_event(format!(
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"code\":{}}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: the shared code id rides the joiner's
        // `connected` event too (same value as the owner's).
        core::hex_encode(&code_id),
        quoted(my_role.as_str()),
        endpoint,
        quoted(code),
    ));
    // The REAL reachability probe over the peer's advertised candidates;
    // the settle is honest (P2-09): all-failed leaves the entry
    // `unreachable`, matching the join result's `probe` verdict.
    let outcome = probe_with_retry(&stack.pairing, &paired.peer).await;
    let (probe_line, verdict) = probe_result_line(&conn_id, &outcome);
    settle_probe_state(rt, &conn_id, &outcome);
    rt.push_event(probe_line);
    Ok(format!(
        "{{\"op\":\"task_join\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"probe\":\"{verdict}\"}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: the join result carries the redeemed code's
        // registry id — a joiner that never saw the create result fills
        // its conn→code (and progress-correlation) mapping from here.
        core::hex_encode(&code_id),
        quoted(my_role.as_str()),
        endpoint,
    ))
}

/// One send op: iterate the connection's candidates LAN-first under the
/// split connect budget; a post-connect failure fails the op (another
/// address cannot fix it); success emits `done` and completes the op.
/// Purely connect-level failure of the WHOLE list gets
/// [`TASK_CONNECT_PASSES`] passes (the just-paired peer's admission
/// bookkeeping can trail the joiner by a beat — see the constant).
async fn run_send(
    rt: &Arc<TaskRt>,
    conn_id: [u8; 16],
    paths: Vec<String>,
) -> Result<String, String> {
    let conn = {
        let conns = rt
            .conns
            .lock()
            .map_err(|_| "connection table lock poisoned".to_string())?;
        conns
            .get(&conn_id)
            .cloned()
            .ok_or_else(|| format!("unknown connection {}", core::hex_encode(&conn_id)))?
    };
    if conn.role != TaskRole::Sender {
        return Err("only the sender side of a task connection may send".to_string());
    }
    let pinned = conn.peer_fp.ok_or_else(|| {
        "the peer key is unresolved (ak-session PeerEndpoint pending)".to_string()
    })?;
    let ordered = order_candidates(conn.candidates.clone());
    if ordered.is_empty() {
        return Err(format!(
            "connection {} carries no dial candidates",
            core::hex_encode(&conn_id)
        ));
    }
    let mut failures: Vec<(SocketAddr, String)> = Vec::new();
    for pass in 0..TASK_CONNECT_PASSES {
        if pass > 0 {
            // Stop retrying a connection that ended while we waited.
            let severed = rt
                .conns
                .lock()
                .map(|conns| {
                    conns
                        .get(&conn_id)
                        .is_none_or(|conn| conn.state == ConnState::Disconnected)
                })
                .unwrap_or(true);
            if severed {
                break;
            }
            tokio::time::sleep(TASK_SETTLE_PAUSE).await;
            failures.clear();
        }
        for (index, addr) in ordered.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(ATTEMPT_PAUSE).await;
            }
            match attempt_task_send(rt, &conn, *addr, &paths, pinned).await {
                AttemptOutcome::Done(report) => {
                    rt.push_event(format!(
                        "{{\"event\":\"done\",\"conn\":\"{}\",\"files\":{},\"bytes\":{}}}",
                        core::hex_encode(&conn_id),
                        report.files,
                        report.bytes,
                    ));
                    // The single-transfer task is complete: converge BOTH
                    // sides' entries (the receiver ends its loop and marks
                    // the same terminal state; wave-29 P2-05 — the UI never
                    // keeps offering a dead connection).
                    rt.push_disconnected_once(&conn_id, "done");
                    return Ok(format!(
                        "{{\"op\":\"task_send\",\"conn\":\"{}\",\"files\":{},\"bytes\":{},\"peer\":{}}}",
                        core::hex_encode(&conn_id),
                        report.files,
                        report.bytes,
                        quoted(&addr.to_string()),
                    ));
                }
                AttemptOutcome::FailedAfterConnect { reason, peer_gone } => {
                    if peer_gone {
                        report_peer_gone(rt, &conn);
                    }
                    return Err(format!(
                        "transfer failed after connecting via {addr}: {reason}"
                    ));
                }
                AttemptOutcome::ConnectFailed(reason) => {
                    failures.push((*addr, reason));
                }
            }
        }
    }
    let detail = failures
        .iter()
        .map(|(addr, reason)| format!("{addr}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("no candidate accepted the connection ({detail})"))
}

/// Report one pairing as ended ByPeer (the peer's data plane went away).
/// The event pump turns it into the `disconnected`/`peer` line.
fn report_peer_gone(rt: &Arc<TaskRt>, conn: &TaskConn) {
    if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
        stack.pairing.report_peer_disconnect(conn.code_id);
    }
}

enum AttemptOutcome {
    Done(Report),
    FailedAfterConnect {
        reason: String,
        /// The engine named the peer's departure (`PeerClosed`): the
        /// pairing is reported ByPeer after the op fails.
        peer_gone: bool,
    },
    ConnectFailed(String),
}

/// One candidate attempt under the split connect budget (the wave-25/26
/// CLI driver, P1-01 shape): no deadline before the engine's
/// `Connecting` report, [`TASK_CONNECT_BUDGET`] from it, no deadline once
/// connected. Post-connect failures are distinguishable so the fallback
/// loop stops instead of retrying a protocol error on another address.
async fn attempt_task_send(
    rt: &Arc<TaskRt>,
    conn: &TaskConn,
    addr: SocketAddr,
    paths: &[String],
    pinned: Fingerprint,
) -> AttemptOutcome {
    let mut spec = SendSpec::new(
        PathBuf::from(&paths[0]),
        addr,
        pinned,
        make_self_signed(&rt.key),
    );
    for extra in &paths[1..] {
        spec.add_root(PathBuf::from(extra));
    }
    // Family-matched wildcard bind (the engine fast-fails mismatches).
    spec.bind = if addr.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    spec.checkpoint_dir = rt.id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
    spec.idle_timeout = TASK_SEND_IDLE;

    let marks = Arc::new(ConnectMarks::new());
    let progress_queue = Arc::clone(&rt.engine_progress);
    let conn_hex = core::hex_encode(&conn.conn_id);
    let send_marks = Arc::clone(&marks);
    let progress = move |event: Progress| {
        send_marks.mark(&event);
        if let Ok(mut queue) = progress_queue.lock() {
            core::push_capped(
                &mut queue,
                format!(
                    "{{\"job\":{},\"event\":{}}}",
                    quoted(&conn_hex),
                    quoted(core::progress_name(&event)),
                ),
            );
        }
    };
    let send = send(spec, &progress);
    tokio::pin!(send);
    let result = match drive_with_connect_budget(&marks, send.as_mut(), TASK_CONNECT_BUDGET).await {
        Ok(result) => result,
        Err(()) => {
            return AttemptOutcome::ConnectFailed(format!(
                "timeout after {}s (no QUIC handshake)",
                TASK_CONNECT_BUDGET.as_secs(),
            ))
        }
    };
    let after_connect = marks.connected.load(Ordering::Relaxed);
    match result {
        Ok(report) => AttemptOutcome::Done(report),
        Err(error) => {
            let reason = one_line_reason(&error);
            if after_connect {
                AttemptOutcome::FailedAfterConnect {
                    peer_gone: matches!(error, EngineError::PeerClosed),
                    reason,
                }
            } else {
                AttemptOutcome::ConnectFailed(reason)
            }
        }
    }
}

/// Single-line rendering of an engine error (multi-line Display output
/// would garble the aggregate message).
fn one_line_reason(error: &EngineError) -> String {
    let text = error.to_string();
    let first = text.split(['\n', '\r']).next().unwrap_or("");
    first.chars().take(120).collect()
}

/// Connect-phase bookkeeping (the CLI's `ConnectPhaseMarks`): the engine
/// invokes its progress callback synchronously inside the polled future,
/// so the marks carry no cross-task race.
struct ConnectMarks {
    connecting_at: StdMutex<Option<tokio::time::Instant>>,
    connecting: Notify,
    connected: AtomicBool,
}

impl ConnectMarks {
    fn new() -> Self {
        Self {
            connecting_at: StdMutex::new(None),
            connecting: Notify::new(),
            connected: AtomicBool::new(false),
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
            _ => {}
        }
    }
}

/// The split-budget driver (P1-01): unbudgeted preparation, budgeted
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
// The op-result / event-channel extension of the pairing exports.
// ---------------------------------------------------------------------------

/// `ak_pairing_op_result` task-plane half: read a TASK op's outcome. The
/// shim has already routed on the plane (C11: one
/// `pairing::runtime_plane` pass decides), so a not-found here is the
/// honest `-4` (unknown op, or the runtime left the table between the two
/// locks) and internal errors propagate as-is — never masked by a
/// speculative pairing-plane lookup.
pub(crate) fn task_op_result(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<OpResult, FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let ops = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("task op table lock poisoned"))?;
    let entry = ops.get(op_id).ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown op {} on task runtime {}",
            core::hex_encode(op_id),
            core::hex_encode(runtime_id),
        ))
    })?;
    let outcome = entry
        .outcome
        .lock()
        .map_err(|_| FfiError::internal("task op outcome lock poisoned"))?;
    Ok(match &*outcome {
        OpOutcome::Running { .. } => OpResult::Running,
        OpOutcome::Finished { ok, json } => OpResult::Finished {
            ok: *ok,
            json: json.clone(),
        },
    })
}

/// `ak_pairing_poll_event` task-plane half: pop a TASK runtime's event.
/// Returns the popped line plus the queue it came from (the shim pushes
/// back to THE queue on `-3`); `Ok(None)` is a known runtime with an empty
/// queue — the caller has routed already (C11), so errors propagate
/// strictly.
pub(crate) fn task_poll_event(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
) -> Result<Option<PolledTaskLine>, FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let queue = Arc::clone(&rt.events);
    let line = queue
        .lock()
        .map_err(|_| FfiError::internal("task event queue lock poisoned"))?
        .pop_front();
    Ok(Some((line, queue)))
}

/// `ak_pairing_poll_event` pushback half: push one event line back to a
/// TASK runtime's queue (after an `AK_ERR_BUFFER`). Strict like the pop
/// (C11): the caller routed on the plane, errors propagate as-is.
pub(crate) fn task_push_event_front(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    line: String,
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let mut queue = rt
        .events
        .lock()
        .map_err(|_| FfiError::internal("task event queue lock poisoned"))?;
    queue.push_front(line);
    Ok(())
}

/// `ak_pairing_cancel_op` task-plane half: cancel a TASK op (abort +
/// terminal "cancelled by caller"). Strict like the op read (C11): the
/// caller routed on the plane, errors propagate as-is.
pub(crate) fn task_cancel_op(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let ops = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("task op table lock poisoned"))?;
    let entry = ops.get(op_id).ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown op {} on task runtime {}",
            core::hex_encode(op_id),
            core::hex_encode(runtime_id),
        ))
    })?;
    let task = entry
        .task
        .lock()
        .map_err(|_| FfiError::internal("task op task lock poisoned"))?
        .take();
    if let Some(task) = task {
        task.abort();
    }
    let mut outcome = entry
        .outcome
        .lock()
        .map_err(|_| FfiError::internal("task op outcome lock poisoned"))?;
    if let OpOutcome::Running { op } = &*outcome {
        *outcome = OpOutcome::Finished {
            ok: false,
            json: format!(
                "{{\"op\":{},\"error\":\"cancelled by caller\"}}",
                quoted(op)
            ),
        };
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests: every ak_task_* export, one positive + one negative minimum,
// driven through the C ABI boundary like tests/ffi_pairing.rs (but inside
// src/ — the wave-28 file-ownership boundary). The op/event reads go
// through the SHARED ak_pairing_op_result / ak_pairing_poll_event
// exports on purpose: that sharing is the contract.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod task_tests {
    use super::*;
    // C02: the C exports are `unsafe extern "C" fn` now; this module is
    // `#![forbid(unsafe_code)]` (whole file), so the audited one-line
    // unsafe calls live in `task_ext::testabi` and are imported here under
    // the SAME names — the call sites below are unchanged and still drive
    // the real exported shapes, just through the test-only adapters.
    use crate::task_ext::testabi::{
        ak_engine_free, ak_engine_last_error, ak_engine_new, ak_pairing_op_result,
        ak_pairing_poll_event, ak_task_consent, ak_task_create_code, ak_task_disconnect,
        ak_task_join, ak_task_list, ak_task_send, ak_task_set_save_dir, ak_task_start,
    };
    use std::ffi::CString;
    use std::os::raw::c_char;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const CAP: usize = 8192;
    const WAIT: Duration = Duration::from_secs(60);
    const POLL: Duration = Duration::from_millis(10);

    // ---- the real v2 registry (dedicated thread; the FFI engine keeps
    // its own embedded runtime, so the two never nest) ------------------

    /// The real v2 registry on a dedicated thread (current-thread tokio
    /// runtime), same shape as `tests/ffi_pairing.rs`.
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
                    let key = ak_identity::DeviceKeyPair::generate();
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

        fn id_hex(&self) -> String {
            self.registry_id
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
    }

    impl Drop for TestRegistry {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    fn cbuf(cap: usize) -> Vec<c_char> {
        vec![0 as c_char; cap]
    }

    fn text_of(buf: &[c_char]) -> String {
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte as u8)
            .collect();
        String::from_utf8(bytes).expect("FFI output is UTF-8")
    }

    fn last_error_of(engine: *mut AkEngine) -> String {
        let mut buf = cbuf(1024);
        let len = ak_engine_last_error(engine, buf.as_mut_ptr(), buf.len());
        assert!(len >= 0, "last_error must be readable");
        text_of(&buf[..len as usize])
    }

    fn spec_for(id_dir: &std::path::Path) -> String {
        // A syntactically valid registry pin (32 bytes of 0x11); only the
        // validation paths use this spec (no live registry, no op runs).
        format!(
            "{{\"spec_version\":1,\"registry\":\"127.0.0.1:7200\",\
             \"registry_id\":\"{}\",\"id_dir\":\"{}\"}}",
            "11".repeat(32),
            id_dir.to_string_lossy().replace('\\', "/"),
        )
    }

    /// The spec of a runtime whose ops pair through a LIVE registry.
    fn registry_spec(registry: &TestRegistry, id_dir: &std::path::Path) -> String {
        format!(
            "{{\"spec_version\":1,\"registry\":\"{}\",\
             \"registry_id\":\"{}\",\"id_dir\":\"{}\"}}",
            registry.addr,
            registry.id_hex(),
            id_dir.to_string_lossy().replace('\\', "/"),
        )
    }

    fn start_task(engine: *mut AkEngine, id_dir: &std::path::Path) -> [u8; 16] {
        let spec = CString::new(spec_for(id_dir)).expect("spec CString");
        let mut runtime_id = [0u8; 16];
        let code = ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr());
        let detail = last_error_of(engine);
        assert_eq!(code, 0, "ak_task_start failed: {detail}");
        runtime_id
    }

    fn start_task_on(
        engine: *mut AkEngine,
        registry: &TestRegistry,
        id_dir: &std::path::Path,
    ) -> [u8; 16] {
        let spec = CString::new(registry_spec(registry, id_dir)).expect("spec CString");
        let mut runtime_id = [0u8; 16];
        let code = ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr());
        let detail = last_error_of(engine);
        assert_eq!(code, 0, "ak_task_start failed: {detail}");
        runtime_id
    }

    /// Poll `ak_pairing_poll_event` until one event whose body contains
    /// `needle` arrives (draining older events into a log).
    fn wait_event(engine: *mut AkEngine, runtime_id: &[u8; 16], needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let mut buf = cbuf(CAP);
            let code =
                ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len());
            if code == 1 {
                let line = text_of(&buf);
                if line.contains(needle) {
                    return line;
                }
                seen.push(line);
            } else {
                assert!(code == 0, "poll_event failed: {code}");
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; saw {seen:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// Poll `ak_pairing_op_result` until the op settles; returns
    /// `(ok, json)`.
    fn wait_op(engine: *mut AkEngine, runtime_id: &[u8; 16], op_id: &[u8; 16]) -> (bool, String) {
        let deadline = Instant::now() + WAIT;
        loop {
            let mut buf = cbuf(CAP);
            let code = ak_pairing_op_result(
                engine,
                runtime_id.as_ptr(),
                op_id.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            );
            match code {
                0 => {}
                1 | 2 => return (code == 1, text_of(&buf)),
                other => panic!("op_result failed: {other} ({})", last_error_of(engine)),
            }
            assert!(Instant::now() < deadline, "timed out waiting for the op");
            std::thread::sleep(POLL);
        }
    }

    fn create_code(
        engine: *mut AkEngine,
        runtime_id: &[u8; 16],
        spec: &str,
    ) -> ([u8; 16], bool, String) {
        let c_spec = CString::new(spec).expect("spec CString");
        let mut op_id = [0u8; 16];
        let code = ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            c_spec.as_ptr(),
            op_id.as_mut_ptr(),
        );
        assert_eq!(
            code,
            0,
            "ak_task_create_code not accepted: {}",
            last_error_of(engine)
        );
        let (ok, json) = wait_op(engine, runtime_id, &op_id);
        (op_id, ok, json)
    }

    fn code_of(json: &str) -> String {
        json.split("\"code\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("code in result")
            .to_string()
    }

    fn conn_of(json: &str) -> [u8; 16] {
        let hex = json
            .split("\"conn\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("conn id");
        hex16_id(hex)
    }

    fn hex16_id(hex: &str) -> [u8; 16] {
        let mut id = [0u8; 16];
        for (index, byte) in hex.as_bytes().chunks(2).enumerate() {
            id[index] = u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap();
        }
        id
    }

    fn join_code(engine: *mut AkEngine, runtime_id: &[u8; 16], code_text: &str) -> (bool, String) {
        join_code_as(engine, runtime_id, code_text, None)
    }

    /// Join with an explicit joiner-role hint (the cross-device path —
    /// the code's declaration is unreadable before the redeem).
    fn join_code_as(
        engine: *mut AkEngine,
        runtime_id: &[u8; 16],
        code_text: &str,
        role: Option<&str>,
    ) -> (bool, String) {
        let role_field = role
            .map(|text| format!(",\"role\":\"{text}\""))
            .unwrap_or_default();
        let spec = CString::new(format!(
            "{{\"spec_version\":1,\"code\":\"{code_text}\"{role_field}}}"
        ))
        .expect("spec CString");
        let mut op_id = [0u8; 16];
        let status = ak_task_join(
            engine,
            runtime_id.as_ptr(),
            spec.as_ptr(),
            op_id.as_mut_ptr(),
        );
        assert_eq!(
            status,
            0,
            "ak_task_join not accepted: {}",
            last_error_of(engine)
        );
        wait_op(engine, runtime_id, &op_id)
    }

    // -- ak_task_start ------------------------------------------------------

    #[test]
    fn task_start_accepts_and_emits_runtime_ready() {
        let engine = ak_engine_new();
        assert!(!engine.is_null());
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        assert_ne!(runtime_id, [0u8; 16]);
        let event = wait_event(engine, &runtime_id, "runtime_ready");
        assert!(
            event.contains("\"runtime\":"),
            "runtime id rides the event: {event}"
        );
        ak_engine_free(engine);
    }

    #[test]
    fn task_start_validates_arguments_before_side_effects() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let good = CString::new(spec_for(dir.path())).expect("spec");
        let mut out = [0u8; 16];
        // Null output pointer: refused BEFORE parsing (no runtime minted).
        assert_eq!(
            ak_task_start(engine, good.as_ptr(), std::ptr::null_mut()),
            -1
        );
        // Null spec.
        assert_eq!(
            ak_task_start(engine, std::ptr::null(), out.as_mut_ptr()),
            -1
        );
        // Malformed JSON.
        let bad = CString::new("{nope").expect("bad");
        assert_eq!(ak_task_start(engine, bad.as_ptr(), out.as_mut_ptr()), -2);
        assert!(
            !last_error_of(engine).is_empty(),
            "the parse detail lands in last_error"
        );
        // Missing registry.
        let no_reg = CString::new(format!(
            "{{\"id_dir\":\"{}\"}}",
            dir.path().to_string_lossy().replace('\\', "/")
        ))
        .expect("spec");
        assert_eq!(ak_task_start(engine, no_reg.as_ptr(), out.as_mut_ptr()), -2);
        assert!(last_error_of(engine).contains("registry"));
        // Bad registry pin (not 64 hex).
        let bad_pin = CString::new(format!(
            "{{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"zz\",\"id_dir\":\"{}\"}}",
            dir.path().to_string_lossy().replace('\\', "/")
        ))
        .expect("spec");
        assert_eq!(
            ak_task_start(engine, bad_pin.as_ptr(), out.as_mut_ptr()),
            -2
        );
        assert!(last_error_of(engine).contains("registry_id"));
        // Empty id_dir.
        let no_dir = CString::new(
            "{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"11\",\"id_dir\":\"\"}",
        )
        .expect("spec");
        assert_eq!(ak_task_start(engine, no_dir.as_ptr(), out.as_mut_ptr()), -2);
        // No runtime was registered by any failed call: poll says -4.
        assert_eq!(
            ak_pairing_poll_event(engine, out.as_ptr(), cbuf(CAP).as_mut_ptr(), CAP),
            -4
        );
        ak_engine_free(engine);
    }

    // -- ak_task_create_code ------------------------------------------------

    #[test]
    fn create_code_sender_role_op_roundtrip() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task_on(engine, &registry, dir.path());
        let (_, ok, json) = create_code(
            engine,
            &runtime_id,
            "{\"spec_version\":1,\"role\":\"sender\"}",
        );
        assert!(ok, "create must succeed at the real registry: {json}");
        assert!(json.contains("\"op\":\"task_create_code\""), "{json}");
        // The registry mints the invite display form (AKI1-…).
        assert!(
            json.contains("\"code\":\"AKI1-"),
            "registry display code: {json}"
        );
        assert!(json.contains("\"role\":\"sender\""), "{json}");
        assert!(json.contains("\"code_id\":\""), "{json}");
        assert!(json.contains("\"endpoint\":"), "{json}");
        let event = wait_event(engine, &runtime_id, "op_done");
        assert!(event.contains("\"op\":\"task_create_code\""), "{event}");
        ak_engine_free(engine);
    }

    #[test]
    fn create_code_receiver_role_binds_a_real_endpoint() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task_on(engine, &registry, dir.path());
        let (_, ok, json) = create_code(
            engine,
            &runtime_id,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "receiver-role create must bind a real socket: {json}");
        // The endpoint is a real ip:port (the derivation rewrote the
        // wildcard bind to interface addresses).
        assert!(json.contains("\"endpoint\":\""), "{json}");
        ak_engine_free(engine);
    }

    #[test]
    fn create_code_validates_before_side_effects() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        let mut op_id = [0u8; 16];
        // Unknown role value: -2 at the call, no op minted.
        let bad_role = CString::new("{\"role\":\"both\"}").expect("spec");
        assert_eq!(
            ak_task_create_code(
                engine,
                runtime_id.as_ptr(),
                bad_role.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -2
        );
        assert!(last_error_of(engine).contains("role"));
        // Missing role.
        let no_role = CString::new("{}").expect("spec");
        assert_eq!(
            ak_task_create_code(
                engine,
                runtime_id.as_ptr(),
                no_role.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -2
        );
        // Bad candidate shape (not ip:port).
        let bad_cand =
            CString::new("{\"role\":\"sender\",\"candidates\":[\"not-an-addr\"]}").expect("spec");
        assert_eq!(
            ak_task_create_code(
                engine,
                runtime_id.as_ptr(),
                bad_cand.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -2
        );
        // Unknown runtime: -4.
        assert_eq!(
            ak_task_create_code(
                engine,
                [9u8; 16].as_ptr(),
                bad_role.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -4
        );
        // Null op_id_out: -1.
        let good = CString::new("{\"role\":\"sender\"}").expect("spec");
        assert_eq!(
            ak_task_create_code(
                engine,
                runtime_id.as_ptr(),
                good.as_ptr(),
                std::ptr::null_mut()
            ),
            -1
        );
        ak_engine_free(engine);
    }

    #[test]
    fn create_code_request_id_is_idempotent() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task_on(engine, &registry, dir.path());
        let spec = "{\"spec_version\":1,\"role\":\"sender\",\
                    \"request_id\":\"a1b2c3d4e5f60718293a4b5c6d7e8f90\"}";
        let (op_one, ok_one, json_one) = create_code(engine, &runtime_id, spec);
        assert!(ok_one, "{json_one}");
        let (op_two, ok_two, json_two) = create_code(engine, &runtime_id, spec);
        assert!(ok_two, "{json_two}");
        // Exact replay: the SAME op id and the SAME code — never a second
        // registry claim, never a second code.
        assert_eq!(op_one, op_two, "replay must answer the original op");
        assert_eq!(json_one, json_two);
        // Same request_id, different spec: -2 before any side effect.
        let conflict = CString::new(
            "{\"spec_version\":1,\"role\":\"receiver\",\
              \"request_id\":\"a1b2c3d4e5f60718293a4b5c6d7e8f90\"}",
        )
        .expect("spec");
        let mut op_id = [0u8; 16];
        assert_eq!(
            ak_task_create_code(
                engine,
                runtime_id.as_ptr(),
                conflict.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -2
        );
        ak_engine_free(engine);
    }

    // -- ak_task_join -------------------------------------------------------

    #[test]
    fn join_links_both_sides_and_lists_the_connection() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine, &registry, dir_a.path());
        let joiner = start_task_on(engine, &registry, dir_b.path());
        let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        // Separator tolerance: the UI's normalized (hyphen-less) form of
        // the SAME code joins the SAME registration.
        let stripped = code.split('-').collect::<String>();
        let stripped_lower = stripped.to_lowercase();
        let (join_ok, join_json) = join_code(engine, &joiner, &stripped_lower);
        assert!(
            join_ok,
            "join must succeed through the registry: {join_json}"
        );
        assert!(join_json.contains("\"op\":\"task_join\""), "{join_json}");
        assert!(
            join_json.contains("\"role\":\"receiver\""),
            "joiner gets the opposite role: {join_json}"
        );
        assert!(join_json.contains("\"conn\":\""), "{join_json}");
        // The real probe answered by the owner's echo responder.
        assert!(join_json.contains("\"probe\":\"verified\""), "{join_json}");
        // Bilateral `connected` events (the owner's from its owner loop).
        let on_owner = wait_event(engine, &owner, "\"event\":\"connected\"");
        let on_joiner = wait_event(engine, &joiner, "\"event\":\"connected\"");
        assert!(on_owner.contains("\"role\":\"sender\""), "{on_owner}");
        assert!(on_joiner.contains("\"role\":\"receiver\""), "{on_joiner}");
        // Probe event on the joiner: the real verdict.
        let probe = wait_event(engine, &joiner, "probe_result");
        assert!(probe.contains("\"reachable\":true"), "{probe}");
        // Both lists show the connection (the probe runs INSIDE the join
        // op, so both sides have settled by the time the op answered).
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        let owner_list = text_of(&buf);
        assert!(
            owner_list.contains("\"state\":\"connected\""),
            "{owner_list}"
        );
        buf.iter_mut().for_each(|slot| *slot = 0);
        assert_eq!(
            ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        let joiner_list = text_of(&buf);
        assert!(
            joiner_list.contains("\"state\":\"connected\""),
            "{joiner_list}"
        );
        assert!(joiner_list.contains("\"code\":\""), "{joiner_list}");
        ak_engine_free(engine);
    }

    #[test]
    fn join_unknown_code_fails_the_op() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task_on(engine, &registry, dir.path());
        // A registry-shaped code nobody ever claimed (8 groups of 4).
        let (ok, json) = join_code(
            engine,
            &runtime_id,
            "AKI1-0000-0000-0000-0000-0000-0000-0000-0000",
        );
        assert!(!ok, "unknown code must fail the op: {json}");
        // The registry never reveals whether a code exists; the joiner's
        // honest wording covers unknown/expired/revoked alike.
        assert!(json.contains("unknown task code"), "{json}");
        // Missing code field: -2 at the call.
        let no_code = CString::new("{}").expect("spec");
        let mut op_id = [0u8; 16];
        assert_eq!(
            ak_task_join(
                engine,
                runtime_id.as_ptr(),
                no_code.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -2
        );
        ak_engine_free(engine);
    }

    // -- ak_task_list / -3 lossless -----------------------------------------

    #[test]
    fn task_list_buffer_retry_is_lossless() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        let mut big = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()),
            1
        );
        let full = text_of(&big);
        // Tiny buffer: -3, nothing consumed, the message names the size.
        let mut tiny = cbuf(8);
        assert_eq!(
            ak_task_list(engine, runtime_id.as_ptr(), tiny.as_mut_ptr(), tiny.len()),
            -3
        );
        let detail = last_error_of(engine);
        assert!(detail.contains("buffer too small"), "{detail}");
        // Retry with the full capacity: the SAME JSON.
        big.iter_mut().for_each(|slot| *slot = 0);
        assert_eq!(
            ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()),
            1
        );
        assert_eq!(text_of(&big), full);
        // Unknown runtime: -4.
        assert_eq!(
            ak_task_list(engine, [9u8; 16].as_ptr(), big.as_mut_ptr(), big.len()),
            -4
        );
        ak_engine_free(engine);
    }

    // -- ak_task_set_save_dir ----------------------------------------------

    #[test]
    fn set_save_dir_creates_and_reflects_in_list() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        let inbox = dir.path().join("inbox");
        let path = CString::new(inbox.to_string_lossy().replace('\\', "/")).expect("path CString");
        assert_eq!(
            ak_task_set_save_dir(engine, runtime_id.as_ptr(), path.as_ptr()),
            0
        );
        assert!(inbox.is_dir(), "the save dir is created eagerly");
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        let list = text_of(&buf);
        assert!(list.contains("inbox"), "{list}");
        // Empty path: -2. Unknown runtime: -4.
        let empty = CString::new("").expect("empty");
        assert_eq!(
            ak_task_set_save_dir(engine, runtime_id.as_ptr(), empty.as_ptr()),
            -2
        );
        assert_eq!(
            ak_task_set_save_dir(engine, [7u8; 16].as_ptr(), path.as_ptr()),
            -4
        );
        ak_engine_free(engine);
    }

    // -- ak_task_disconnect -------------------------------------------------

    #[test]
    fn disconnect_notifies_both_sides() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine, &registry, dir_a.path());
        let joiner = start_task_on(engine, &registry, dir_b.path());
        let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code(engine, &joiner, &code);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        assert_eq!(
            ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()),
            0
        );
        // Local side: reason local; peer side: reason peer (the
        // same-engine instant cache notify).
        let local = wait_event(engine, &joiner, "disconnected");
        assert!(local.contains("\"reason\":\"local\""), "{local}");
        let peer = wait_event(engine, &owner, "disconnected");
        assert!(peer.contains("\"reason\":\"peer\""), "{peer}");
        // Both lists now report the connection disconnected.
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
        // Idempotent: a second disconnect is a silent AK_OK; an unknown
        // conn id is -4.
        assert_eq!(
            ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()),
            0
        );
        assert_eq!(
            ak_task_disconnect(engine, joiner.as_ptr(), [5u8; 16].as_ptr()),
            -4
        );
        ak_engine_free(engine);
    }

    // -- ak_task_send + ak_task_consent (real bytes end to end) -------------

    /// The full v1 pairing story in one process: receiver-role code on A
    /// (real resident receiver bound, consent gate armed), B joins as the
    /// sender, B sends a real file, A's user accepts the incoming request,
    /// the bytes land in A's save folder, both sides see `done`.
    #[test]
    fn send_with_accepted_consent_delivers_real_bytes() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine, &registry, dir_a.path());
        let joiner = start_task_on(engine, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()),
            0
        );

        let (_, ok, json) =
            create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
        assert!(ok, "receiver-role create (real bind): {json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code(engine, &joiner, &code);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);

        // A real payload.
        let payload = b"ak-task-plane e2e payload".repeat(64);
        std::fs::write(dir_b.path().join("e2e.bin"), &payload).expect("source file");
        let paths = CString::new(json_paths(&dir_b.path().join("e2e.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0,
            "send accepted: {}",
            last_error_of(engine)
        );

        // The receiver's consent gate fires: answer it from this thread.
        let request = wait_event(engine, &owner, "incoming_request");
        assert!(request.contains("\"files\":1"), "{request}");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 1), 0);

        // The send op completes with the report.
        let (send_ok, send_json) = wait_op(engine, &joiner, &send_op);
        assert!(send_ok, "send must succeed after consent: {send_json}");
        assert!(send_json.contains("\"op\":\"task_send\""), "{send_json}");
        assert!(send_json.contains("\"files\":1"), "{send_json}");
        // The receiver's done event + the delivered bytes.
        let done = wait_event(engine, &owner, "\"event\":\"done\"");
        assert!(done.contains("\"conn\":\""), "{done}");
        let delivered = find_file(&inbox, "e2e.bin").expect("the file landed in the save dir");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        // The sender-side done event too.
        let _ = wait_event(engine, &joiner, "\"event\":\"done\"");
        ak_engine_free(engine);
    }

    /// The decline path: consent(false) refuses the transfer over the wire
    /// (DECLINED semantics), the send op fails, NOTHING lands on disk.
    #[test]
    fn declined_consent_refuses_over_the_wire() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine, &registry, dir_a.path());
        let joiner = start_task_on(engine, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) =
            create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code(engine, &joiner, &code);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        std::fs::write(dir_b.path().join("no.bin"), b"must not land").unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("no.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0
        );
        let request = wait_event(engine, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 0), 0);
        let (send_ok, send_json) = wait_op(engine, &joiner, &send_op);
        assert!(
            !send_ok,
            "a declined transfer must fail the op: {send_json}"
        );
        assert!(
            find_file(&inbox, "no.bin").is_none(),
            "nothing may land on a decline"
        );
        // A decided request cannot be decided again: -4.
        assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 1), -4);
        ak_engine_free(engine);
    }

    #[test]
    fn send_validates_runtime_and_connection() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        let paths = CString::new("{\"paths\":[\"C:/x\"]}").unwrap();
        let mut op_id = [0u8; 16];
        // Unknown connection: -4 with the conn wording.
        assert_eq!(
            ak_task_send(
                engine,
                runtime_id.as_ptr(),
                [3u8; 16].as_ptr(),
                paths.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -4
        );
        assert!(last_error_of(engine).contains("unknown connection"));
        // Unknown runtime: -4 (checked before the connection).
        assert_eq!(
            ak_task_send(
                engine,
                [3u8; 16].as_ptr(),
                [3u8; 16].as_ptr(),
                paths.as_ptr(),
                op_id.as_mut_ptr()
            ),
            -4
        );
        // Empty paths array: accepted runtime + a real connection needed —
        // exercised in send_from_the_receiver_side_fails_the_op instead;
        // here the -2 path for a malformed paths value on an unknown conn
        // stays ordered after the -4s above (validation order: runtime,
        // conn, spec).
        ak_engine_free(engine);
    }

    /// Only the sender side may send: a receiver-side runtime's send op
    /// fails with the honest wording (async failure, not a sync -2).
    #[test]
    fn send_from_the_receiver_side_fails_the_op() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine, &registry, dir_a.path());
        let joiner = start_task_on(engine, &registry, dir_b.path());
        let (_, ok, json) =
            create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code(engine, &joiner, &code);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        // The OWNER holds the receiver-side conn entry — it arrives with
        // its owner-loop completion, so wait for that first.
        let _ = wait_event(engine, &owner, "\"event\":\"connected\"");
        let paths = CString::new(json_paths(std::path::Path::new("C:/nowhere/none.bin"))).unwrap();
        let mut op_id = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine,
                owner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                op_id.as_mut_ptr()
            ),
            0
        );
        let (send_ok, send_json) = wait_op(engine, &owner, &op_id);
        assert!(!send_ok, "{send_json}");
        assert!(send_json.contains("only the sender side"), "{send_json}");
        ak_engine_free(engine);
    }

    // -- consent bookkeeping -------------------------------------------------

    #[test]
    fn consent_unknown_request_is_not_found() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task(engine, dir.path());
        assert_eq!(
            ak_task_consent(engine, runtime_id.as_ptr(), [4u8; 16].as_ptr(), 1),
            -4
        );
        assert_eq!(
            ak_task_consent(engine, [4u8; 16].as_ptr(), [4u8; 16].as_ptr(), 1),
            -4
        );
        ak_engine_free(engine);
    }

    /// Teardown flips `stopped`: the consent registration declines every
    /// later offer immediately (the waiter-first teardown of engine drop).
    /// A live runtime without a fingerprint-matching connection declines
    /// too (P1-04: attribution fails closed, no event surfaces). Built on
    /// the safe layer with an OWNED engine value, so no raw pointer is
    /// ever dereferenced (this module is `forbid(unsafe_code)`).
    #[test]
    fn consent_registration_declines_after_teardown_and_for_strangers() {
        let rt = {
            let engine = AkEngine::new().expect("engine");
            let dir = tempfile::tempdir().expect("tempdir");
            let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
            let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
            let clone = Arc::clone(&rt);
            drop(engine); // teardown runs here (waiters first, then owners)
            clone
        };
        let info = ConsentInfo {
            job_id: [0u8; 16],
            root_name: "late.bin".to_string(),
            file_count: 1,
            total_bytes: 1,
            sender_fingerprint: Fingerprint::from_bytes([7u8; 32]),
        };
        let started = Instant::now();
        // A stopped runtime: immediate decline, nothing registered.
        assert!(matches!(
            register_consent(
                &rt,
                &[0u8; 16],
                &info,
                std::path::Path::new("D:/unused"),
                PENDING_CONSENT_WINDOW
            ),
            ConsentAnswer::Now(false),
        ));
        // A live runtime without the paired connection of that sender:
        // also an immediate decline (fail closed — the offer belongs to
        // no connection of this code, so the user is never asked).
        let (engine, rt_live, code_id) = {
            let engine = AkEngine::new().expect("engine");
            let dir = tempfile::tempdir().expect("tempdir");
            let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
            let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
            (engine, rt, [0u8; 16])
        };
        let mut events = 0;
        assert!(matches!(
            register_consent(
                &rt_live,
                &code_id,
                &info,
                std::path::Path::new("D:/unused"),
                PENDING_CONSENT_WINDOW
            ),
            ConsentAnswer::Now(false),
        ));
        if let Ok(queue) = rt_live.events.lock() {
            events = queue.len();
        }
        assert_eq!(events, 1, "only runtime_ready is queued: {events}");
        drop(engine);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    // -- engine teardown with an armed receiver ------------------------------

    /// A resident receiver parked in accept must not hang the engine drop
    /// (the P1-04-style waiter-first teardown; the registry stack's client
    /// close is part of the same bounded convergence).
    #[test]
    fn engine_drop_converges_with_a_parked_receiver() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = start_task_on(engine, &registry, dir.path());
        let (_, ok, json) = create_code(
            engine,
            &runtime_id,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let started = Instant::now();
        ak_engine_free(engine); // would hang if the parked accept leaked
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "engine drop must converge quickly"
        );
    }

    // -- candidate ordering ---------------------------------------------------

    #[test]
    fn candidates_order_lan_first_and_dedupe() {
        let lan: SocketAddr = "192.168.1.9:7000".parse().unwrap();
        let public: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:7000".parse().unwrap();
        // ak-net counts loopback as private, so loopback rides WITHIN the
        // LAN group (stable within equals: registration order).
        let ordered = order_candidates(vec![public, loopback, lan, lan]);
        assert_eq!(ordered, vec![loopback, lan, public]);
    }

    // -- the registry relay (the three formerly-#[ignore] contracts) ---------

    /// Codes relay ACROSS ENGINES through the real registry: engine A
    /// claims its code (endpoint blob, max_redeems = 1) and engine B — a
    /// completely separate engine handle, task runtime and registry
    /// client — joins it there. The join drives the frozen pairing
    /// transcript to `PairReady`; the owner's side completes through its
    /// resident owner loop (`connected` on A, too). This replaces the
    /// engine-local-era positive test that asserted the cross-engine
    /// join FAILS (`unknown task code`) — with the relay it is the
    /// success path.
    #[test]
    fn task_codes_relay_across_engines_via_the_registry() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let (_, ok, json) =
            create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "the owner claims at the real registry: {json}");
        let code = code_of(&json);
        // Cross-engine: the engine-local cache cannot help — the join
        // must succeed through the registry relay alone.
        let (join_ok, join_json) = join_code(engine_b, &joiner, &code);
        assert!(
            join_ok,
            "the registry relay must carry the code across engines: {join_json}"
        );
        assert!(join_json.contains("\"op\":\"task_join\""), "{join_json}");
        assert!(join_json.contains("\"role\":\"receiver\""), "{join_json}");
        // The owner learns the pairing through its owner loop: the same
        // connection id (the pairing transaction id) and its own role.
        let on_owner = wait_event(engine_a, &owner, "\"event\":\"connected\"");
        assert!(on_owner.contains("\"role\":\"sender\""), "{on_owner}");
        assert_eq!(conn_of(&on_owner), conn_of(&join_json), "shared conn id");
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// The probe reports REAL reachability: with derived candidates each
    /// side runs a probe echo responder on an advertised port, so the
    /// joiner's probe over the owner's candidates confirms a live path
    /// (`reachable:true` with the answering address, `probe:"verified"`);
    /// with EXPLICIT caller-chosen candidates (verbatim advertisement, no
    /// echo port) a dead address reports every candidate failed
    /// (`reachable:false`, one reason per candidate, `probe:"all_failed"`).
    #[test]
    fn probe_result_reports_real_reachability() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());

        // Live leg: derived candidates (real receiver/echo ports).
        let (_, ok, json) =
            create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "{json}");
        let live_code = code_of(&json);
        let (live_ok, live_json) = join_code(engine_b, &joiner, &live_code);
        assert!(live_ok, "{live_json}");
        assert!(
            live_json.contains("\"probe\":\"verified\""),
            "the echo responder answers the real probe: {live_json}"
        );
        let probe = wait_event(engine_b, &joiner, "probe_result");
        assert!(probe.contains("\"reachable\":true"), "{probe}");
        assert!(
            probe.contains("\"endpoint\":\""),
            "the confirmed address rides the event: {probe}"
        );

        // Dead leg: the caller advertises exactly one closed port.
        let (_, ok_two, json_two) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"sender\",\"candidates\":[\"127.0.0.1:1\"]}",
        );
        assert!(ok_two, "{json_two}");
        let dead_code = code_of(&json_two);
        let (dead_ok, dead_json) = join_code(engine_b, &joiner, &dead_code);
        assert!(dead_ok, "the pairing itself stands: {dead_json}");
        assert!(
            dead_json.contains("\"probe\":\"all_failed\""),
            "a silent candidate reports every candidate failed: {dead_json}"
        );
        let dead_probe = wait_event(engine_b, &joiner, "\"reachable\":false");
        assert!(
            dead_probe.contains("\"candidate\":\"127.0.0.1:1\""),
            "{dead_probe}"
        );
        assert!(dead_probe.contains("\"reason\":"), "{dead_probe}");
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// A remote disconnect reaches the peer's runtime: engine A owns a
    /// receiver-role code (real resident receiver, consent gate armed),
    /// engine B joins as the sender and starts a real send; A's user
    /// ACCEPTS the offer and B disconnects the moment the transfer
    /// begins. B's abort drops its QUIC endpoint (the CONNECTION_CLOSE
    /// reaches A mid-transfer), A's receive fails with the peer gone,
    /// ak-session reports the task `ByPeer`, and the event pump surfaces
    /// `disconnected`/`peer` on A's poll channel (the cross-engine path —
    /// the engine-local cache cannot carry it).
    #[test]
    fn remote_disconnect_reaches_the_peer_device() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        // Cross-engine: the joiner's side is the mirror of the owner's
        // receiver declaration — declared explicitly on the join spec.
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        assert!(join_json.contains("\"role\":\"sender\""), "{join_json}");
        let conn = conn_of(&join_json);
        // A's connection entry arrives with its owner-loop completion.
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

        // A real, sizeable send: the offer parks in A's consent gate.
        let payload = b"remote-disconnect probe payload".repeat(4096);
        std::fs::write(dir_b.path().join("bye.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("bye.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        // A accepts; the transfer begins; B disconnects immediately after
        // (the mid-transfer data-plane departure is what the peer can
        // actually observe — a mid-OFFER close is swallowed by the consent
        // gate's own 30 s window, which declines rather than erroring).
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        assert_eq!(
            ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
            0
        );
        let local = wait_event(engine_b, &joiner, "disconnected");
        assert!(local.contains("\"reason\":\"local\""), "{local}");

        // A's data plane sees the peer go: ByPeer -> the pump -> A's poll.
        let remote = wait_event(engine_a, &owner, "disconnected");
        assert!(
            remote.contains("\"reason\":\"peer\""),
            "the peer device must learn the disconnect: {remote}"
        );
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    // -- wave-29 review findings (P1-04, P2-01..07, P2-12) ------------------

    /// Poll `ak_pairing_poll_event` for `ms` and FAIL if any event whose
    /// body contains `needle` arrives (the absence assertions of the
    /// stranger-dial tests: a refused offer must never reach the user).
    fn assert_no_event_within(engine: *mut AkEngine, runtime_id: &[u8; 16], needle: &str, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            let mut buf = cbuf(CAP);
            let code =
                ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len());
            if code == 1 {
                let line = text_of(&buf);
                assert!(
                    !line.contains(needle),
                    "unexpected event {needle:?}: {line}"
                );
            } else {
                assert!(code == 0, "poll_event failed: {code}");
            }
            std::thread::sleep(POLL);
        }
    }

    /// The first advertised endpoint of a create result (the receiver's
    /// real port for a receiver-role code).
    fn endpoint_of(json: &str) -> SocketAddr {
        json.split("\"endpoint\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("endpoint in result")
            .parse()
            .expect("endpoint is ip:port")
    }

    fn code_id_of(json: &str) -> [u8; 16] {
        let hex = json
            .split("\"code_id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("code id in result");
        hex16_id(hex)
    }

    /// A REAL engine send from a STRANGER device (wave-29 P1-04): a fresh
    /// device key that knows both the target endpoint and the owner's
    /// fingerprint (the worst case — the TLS handshake completes), yet is
    /// not the paired peer of the task. Returns the failure text; a
    /// stranger transfer that SUCCEEDS fails the test.
    fn stranger_send_fails(
        endpoint: SocketAddr,
        expect_peer: &Fingerprint,
        payload: &std::path::Path,
    ) -> String {
        let key = DeviceKeyPair::generate();
        let mut spec = SendSpec::new(
            payload.to_path_buf(),
            endpoint,
            *expect_peer,
            make_self_signed(&key),
        );
        spec.bind = if endpoint.is_ipv6() {
            SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::from(([0, 0, 0, 0], 0))
        };
        spec.idle_timeout = Duration::from_secs(5);
        let stranger = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stranger runtime");
        let attempt = stranger.block_on(async {
            tokio::time::timeout(Duration::from_secs(15), send(spec, &|_| {})).await
        });
        match attempt {
            Ok(Ok(report)) => panic!("the stranger's transfer succeeded: {report:?}"),
            Ok(Err(error)) => format!("{error}"),
            Err(_) => "stranger send timed out".to_string(),
        }
    }

    /// A stranger dial whose TLS handshake FAILS MID-WAY (wave-30 P1-04
    /// residual): the dialer pins a fingerprint the listener does NOT
    /// hold, so the client aborts right after the server certificate
    /// arrives — the SERVER side of the accept then reports a
    /// Transport/PeerIdentity failure, the per-connection error class the
    /// accept loop must survive. Returns the failure text; a transfer
    /// that SUCCEEDS fails the test.
    fn wrong_pin_send_fails(endpoint: SocketAddr, payload: &std::path::Path) -> String {
        let key = DeviceKeyPair::generate();
        let wrong_pin = key.fingerprint(); // nobody listening holds this key
        let mut spec = SendSpec::new(
            payload.to_path_buf(),
            endpoint,
            wrong_pin,
            make_self_signed(&key),
        );
        spec.bind = if endpoint.is_ipv6() {
            SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::from(([0, 0, 0, 0], 0))
        };
        spec.idle_timeout = Duration::from_secs(5);
        let stranger = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stranger runtime");
        let attempt = stranger.block_on(async {
            tokio::time::timeout(Duration::from_secs(15), send(spec, &|_| {})).await
        });
        match attempt {
            Ok(Ok(report)) => panic!("a wrong-pin transfer must not succeed: {report:?}"),
            Ok(Err(error)) => format!("{error}"),
            Err(_) => "wrong-pin send timed out".to_string(),
        }
    }

    /// Safe-layer op wait for the OWNED-engine tests (no raw pointers).
    fn wait_op_owned(engine: &AkEngine, runtime_id: &[u8; 16], op_id: &[u8; 16]) -> (bool, String) {
        let deadline = Instant::now() + WAIT;
        loop {
            let snapshot =
                crate::task::task_op_result(engine, runtime_id, op_id).expect("task op read");
            if let OpResult::Finished { ok, json } = snapshot {
                return (ok, json);
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the owned op"
            );
            std::thread::sleep(POLL);
        }
    }

    /// Fire one safe-layer `task_create_code_start` from a spawned thread
    /// (the concurrency tests; the raw C handle is not `Send`, and the
    /// atomicity under test lives in the safe layer's gate anyway).
    fn threaded_create(
        engine: Arc<AkEngine>,
        runtime_id: [u8; 16],
        spec: String,
    ) -> mpsc::Receiver<Result<[u8; 16], FfiError>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = task_create_code_start(&engine, &runtime_id, &spec);
            tx.send(result).expect("test channel");
        });
        rx
    }

    /// P1-04: the receiver's admission is bound to the completed pairing's
    /// verified fingerprint. Before B joins, a stranger's dial (C — a
    /// fresh device key that knows A's endpoint AND fingerprint, so its
    /// TLS handshake completes) is refused at the admission gate: no
    /// `incoming_request`, zero disk writes, and the listener survives.
    /// After B joins, the SAME stranger is refused again (fingerprint
    /// mismatch now), and B's legitimate send still succeeds on the very
    /// same listener — a stranger never consumes the receiver.
    #[test]
    fn stranger_dials_never_reach_the_user_or_consume_the_receiver() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );

        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "receiver-role create (real bind): {json}");
        let code = code_of(&json);
        let endpoint = endpoint_of(&json);
        // The stranger "knows" A's fingerprint: A's device key is on disk.
        let (owner_key, _) =
            DeviceKeyPair::load_or_create(&dir_a.path().join("device.key")).expect("A key");
        let owner_fp = owner_key.fingerprint();
        std::fs::write(dir_a.path().join("stranger.bin"), b"stranger payload").unwrap();

        // 1) Pre-pairing: C's completed-TLS dial is refused — no consent
        //    prompt, nothing on disk, receiver not consumed.
        let failure = stranger_send_fails(endpoint, &owner_fp, &dir_a.path().join("stranger.bin"));
        assert!(
            !failure.is_empty(),
            "the pre-pairing stranger dial must fail: {failure}"
        );
        assert_no_event_within(engine_a, &owner, "incoming_request", 1500);
        assert!(find_file(&inbox, "stranger.bin").is_none());

        // 2) B joins as the sender; A's connection entry arrives with the
        //    owner-loop completion (synchronous bookkeeping).
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "the legitimate join must succeed: {join_json}");
        let conn = conn_of(&join_json);
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

        // 3) Post-pairing: the same stranger is refused AGAIN — its
        //    fingerprint is not the pairing's verified peer.
        let failure_two =
            stranger_send_fails(endpoint, &owner_fp, &dir_a.path().join("stranger.bin"));
        assert!(
            !failure_two.is_empty(),
            "the post-pairing stranger dial must fail: {failure_two}"
        );
        assert_no_event_within(engine_a, &owner, "incoming_request", 1500);
        assert!(
            find_file(&inbox, "stranger.bin").is_none(),
            "a stranger's offer writes nothing to disk"
        );

        // 4) B's legitimate send on the SAME listener succeeds, and the
        //    prompt is attributed to B's connection.
        let payload = b"legitimate bytes after stranger attempts".repeat(32);
        std::fs::write(dir_b.path().join("legit.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("legit.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0,
            "B's send accepted: {}",
            last_error_of(engine_b)
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        assert!(
            request.contains(&format!("\"conn\":\"{}\"", core::hex_encode(&conn))),
            "the prompt is attributed to the fingerprint-matched conn: {request}"
        );
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
        assert!(
            send_ok,
            "B still succeeds after the stranger attempts: {send_json}"
        );
        let delivered = find_file(&inbox, "legit.bin").expect("B's file landed");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-01: the request gate is one atomic acceptance — two threads
    /// racing the SAME request get the SAME op id (exactly one code is
    /// claimed), and the same `request_id` under a DIFFERENT operation is
    /// a `-2` conflict, never a cross-operation replay hit.
    #[test]
    fn request_id_acceptance_is_atomic_and_domain_scoped() {
        let registry = TestRegistry::spawn();
        let engine = Arc::new(AkEngine::new().expect("engine"));
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
        let request_id = "b1b2c3d4e5f60718293a4b5c6d7e8f90";
        let spec =
            format!("{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{request_id}\"}}");
        let racer_one = threaded_create(Arc::clone(&engine), runtime_id, spec.clone());
        let racer_two = threaded_create(Arc::clone(&engine), runtime_id, spec.clone());
        let op_one = racer_one
            .recv()
            .expect("racer one answered")
            .expect("accepted");
        let op_two = racer_two
            .recv()
            .expect("racer two answered")
            .expect("accepted");
        assert_eq!(
            op_one, op_two,
            "both racers must observe the ONE atomically accepted op"
        );
        let (ok, json) = wait_op_owned(&engine, &runtime_id, &op_one);
        assert!(ok, "{json}");
        assert!(
            json.contains("\"code\":\"AKI1-"),
            "exactly one claim: {json}"
        );

        // Cross-operation domain: the same request id under task_join is
        // a conflict (-2), not a replay of the create's op.
        let join_spec = format!(
            "{{\"spec_version\":1,\"code\":\"AKI1-0000-0000-0000-0000-0000-0000-0000-0000\",\
              \"request_id\":\"{request_id}\"}}"
        );
        let error = task_join_start(&engine, &runtime_id, &join_spec).expect_err("conflict");
        assert_eq!(error.code, crate::core::AK_ERR_INVALID);
        assert!(
            error.message.contains("different spec"),
            "{}",
            error.message
        );
    }

    /// P2-01 (second half): an ACCEPTED send request's answer stays
    /// retrievable after the connection later disconnected — the replay
    /// returns the ORIGINAL op id instead of a state rejection.
    #[test]
    fn accepted_send_request_replays_its_answer_after_disconnect() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);

        let payload = b"replayable request".repeat(16);
        std::fs::write(dir_b.path().join("re.bin"), &payload).unwrap();
        let spec = CString::new(format!(
            "{{\"spec_version\":1,\"paths\":[\"{}\"],\
              \"request_id\":\"c1c2c3d4e5f60718293a4b5c6d7e8f90\"}}",
            dir_b
                .path()
                .join("re.bin")
                .to_string_lossy()
                .replace('\\', "/"),
        ))
        .unwrap();
        let mut op_one = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                spec.as_ptr(),
                op_one.as_mut_ptr()
            ),
            0
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &op_one);
        assert!(send_ok, "{send_json}");

        // The connection goes away; the ORIGINAL request still answers.
        assert_eq!(
            ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
            0
        );
        let mut op_two = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                spec.as_ptr(),
                op_two.as_mut_ptr()
            ),
            0,
            "the replay is answered, not state-rejected: {}",
            last_error_of(engine_b)
        );
        assert_eq!(op_one, op_two, "the replay returns the ORIGINAL op");
        let (replay_ok, replay_json) = wait_op(engine_b, &joiner, &op_two);
        assert!(replay_ok, "{replay_json}");
        assert_eq!(replay_json, send_json, "the original answer, verbatim");
        // A FRESH request on the severed connection is still refused.
        let fresh = CString::new(json_paths(&dir_b.path().join("re.bin"))).unwrap();
        let mut op_three = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                fresh.as_ptr(),
                op_three.as_mut_ptr()
            ),
            -2
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-02: two concurrent FIRST ops on one runtime initialize the
    /// registry stack exactly once — both succeed and the surviving stack
    /// keeps serving later ops (no stranded, replaced control client).
    #[test]
    fn concurrent_first_ops_initialize_the_stack_once_and_it_stays_usable() {
        let registry = TestRegistry::spawn();
        let engine = Arc::new(AkEngine::new().expect("engine"));
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let runtime_id = task_start(&engine, &registry_spec(&registry, dir_a.path())).expect("rt");
        let spec_one = format!(
            "{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{:032x}\"}}",
            0u32
        );
        let spec_two = format!(
            "{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{:032x}\"}}",
            1u32
        );
        let racer_one = threaded_create(Arc::clone(&engine), runtime_id, spec_one);
        let racer_two = threaded_create(Arc::clone(&engine), runtime_id, spec_two);
        let op_one = racer_one
            .recv()
            .expect("racer one answered")
            .expect("accepted");
        let op_two = racer_two
            .recv()
            .expect("racer two answered")
            .expect("accepted");
        assert_ne!(op_one, op_two, "distinct requests, distinct ops");
        let mut codes = Vec::new();
        for op in [&op_one, &op_two] {
            let (ok, json) = wait_op_owned(&engine, &runtime_id, op);
            assert!(ok, "both concurrent first ops must succeed: {json}");
            codes.push(code_of(&json));
        }
        // The stack the race left behind still serves real work: a second
        // runtime on the same engine joins one of the codes through it.
        let joiner = task_start(&engine, &registry_spec(&registry, dir_b.path())).expect("rt2");
        let join_op = task_join_start(
            &engine,
            &joiner,
            &format!("{{\"spec_version\":1,\"code\":\"{}\"}}", codes[0]),
        )
        .expect("join accepted");
        let (join_ok, join_json) = wait_op_owned(&engine, &joiner, &join_op);
        assert!(join_ok, "the surviving stack is usable: {join_json}");
    }

    /// P2-03: an IDLE disconnect (no transfer whose QUIC drop the peer
    /// would observe) still reaches the paired peer: B's disconnect dials
    /// A's advertised receiver port and hands it the TASK-CLOSE frame; A
    /// ends the pairing ByPeer (`disconnected`/`peer`).
    #[test]
    fn idle_disconnect_reaches_the_paired_peer() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        // A's connection entry arrives with its owner-loop completion.
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");
        // IDLE: no send ever happens. B walks away.
        assert_eq!(
            ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
            0
        );
        let remote = wait_event(engine_a, &owner, "disconnected");
        assert!(
            remote.contains("\"reason\":\"peer\""),
            "the idle peer must learn the disconnect: {remote}"
        );
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-04: a generated-but-never-joined code is cancellable through
    /// `ak_task_disconnect` by its `code_id`: the registry row revokes
    /// (a later join answers "unknown task code"), the receiver/echo
    /// listeners are released, `code_cancelled` is emitted, and repeats
    /// are idempotent. Runs on an OWNED engine (safe-layer calls only).
    #[test]
    fn unjoined_code_is_cancellable_through_disconnect() {
        let registry = TestRegistry::spawn();
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = registry_spec(&registry, dir.path());
        let runtime = task_start(&engine, &spec).expect("runtime");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        let op = task_create_code_start(
            &engine,
            &runtime,
            "{\"spec_version\":1,\"role\":\"receiver\",\
              \"request_id\":\"d1d2d3d4e5f60718293a4b5c6d7e8f90\"}",
        )
        .expect("create accepted");
        let (ok, json) = wait_op_owned(&engine, &runtime, &op);
        assert!(ok, "{json}");
        let code_id = code_id_of(&json);
        let code = code_of(&json);
        assert!(
            rt.spawned
                .lock()
                .map(|m| m.contains_key(&code_id))
                .unwrap_or(false),
            "the receiver/echo listeners are registered under the code id"
        );

        // Cancel by code id (the FFI disconnect covers the un-joined state).
        task_disconnect(&engine, &runtime, &code_id).expect("cancel");
        let events: Vec<String> = rt
            .events
            .lock()
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        assert!(
            events.iter().any(|line| line.contains("code_cancelled")
                && line.contains(&core::hex_encode(&code_id))),
            "the cancel is observable: {events:?}"
        );
        assert!(
            !rt.spawned
                .lock()
                .map(|m| m.contains_key(&code_id))
                .unwrap_or(true),
            "the listeners are released"
        );
        // Idempotent repeat: a silent AK_OK.
        task_disconnect(&engine, &runtime, &code_id).expect("idempotent cancel");

        // The registry row is gone: a later join answers "unknown task code".
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let spec_b = registry_spec(&registry, dir_b.path());
        let runtime_b = task_start(&engine, &spec_b).expect("second runtime");
        let join_op = task_join_start(
            &engine,
            &runtime_b,
            &format!("{{\"spec_version\":1,\"code\":\"{code}\"}}"),
        )
        .expect("join accepted");
        let (join_ok, join_json) = wait_op_owned(&engine, &runtime_b, &join_op);
        assert!(!join_ok, "a cancelled code cannot be joined: {join_json}");
        assert!(join_json.contains("unknown task code"), "{join_json}");

        // An id that is neither a connection nor one of this runtime's
        // codes stays the frozen -4.
        let error = task_disconnect(&engine, &runtime, &[9u8; 16]).expect_err("unknown id");
        assert_eq!(error.code, crate::core::AK_ERR_NOT_FOUND);
    }

    /// P2-05: a DECLINED offer keeps the listener alive (the sender may
    /// retry on the same code), and a COMPLETED transfer converges BOTH
    /// sides' entries to the terminal `disconnected` — a follow-up send
    /// on the dead connection is refused up front.
    #[test]
    fn declined_offer_keeps_the_listener_and_done_converges_both_sides() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

        // Attempt 1: declined.
        let payload = b"retry me".repeat(64);
        std::fs::write(dir_b.path().join("retry.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("retry.bin"))).unwrap();
        let mut op_one = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                op_one.as_mut_ptr()
            ),
            0
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .expect("job"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 0),
            0
        );
        let (declined_ok, _) = wait_op(engine_b, &joiner, &op_one);
        assert!(!declined_ok, "the declined attempt fails the op");
        // The listener (and the connection) survived the decline.
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"connected\""),
            "a decline is not a task terminal state: {}",
            text_of(&buf)
        );

        // Attempt 2 (a fresh op on the SAME connection): accepted, lands.
        let mut op_two = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                op_two.as_mut_ptr()
            ),
            0,
            "the retry is accepted: {}",
            last_error_of(engine_b)
        );
        let request_two = wait_event(engine_a, &owner, "incoming_request");
        let job_two = hex16_id(
            request_two
                .split("\"job\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .expect("job"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job_two.as_ptr(), 1),
            0
        );
        let (retry_ok, retry_json) = wait_op(engine_b, &joiner, &op_two);
        assert!(retry_ok, "the retry after a decline succeeds: {retry_json}");
        let delivered = find_file(&inbox, "retry.bin").expect("the retry landed");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);

        // Done is the single-transfer terminal state on BOTH sides.
        let _ = wait_event(engine_a, &owner, "\"event\":\"done\"");
        // Wave-30 P2-05 residual: the converge line's reason IS `done` —
        // a decidable terminal state for the Dart table to agree on.
        let done_line = wait_event(engine_a, &owner, "\"reason\":\"done\"");
        assert!(
            done_line.contains("\"event\":\"disconnected\""),
            "{done_line}"
        );
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"disconnected\""),
            "the receiver converges: {}",
            text_of(&buf)
        );
        buf.iter_mut().for_each(|slot| *slot = 0);
        assert_eq!(
            ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"disconnected\""),
            "the sender converges: {}",
            text_of(&buf)
        );
        // A third send on the terminal connection is refused up front.
        let mut op_three = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                op_three.as_mut_ptr()
            ),
            -2
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-06: the save folder is resolved when a transfer STARTS, not when
    /// the code was generated — a `set_save_dir` after the code (but
    /// before the offer) applies to the incoming task.
    #[test]
    fn save_dir_change_applies_to_the_next_received_task() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        // Create the code FIRST (the receiver captures the DEFAULT dir),
        // then change the setting — the change must govern the task.
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let inbox = dir_a.path().join("later-inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );

        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        let payload = b"saved in the changed folder".repeat(32);
        std::fs::write(dir_b.path().join("moved.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("moved.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .expect("job"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
        assert!(send_ok, "{send_json}");
        let delivered = find_file(&inbox, "moved.bin")
            .expect("the file lands in the folder current at task start");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        assert!(
            find_file(&dir_a.path().join("downloads"), "moved.bin").is_none(),
            "the stale creation-time folder must not receive the task"
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-07: two pending consents on ONE engine (two receivers, two
    /// paired senders) park nothing on the two Tokio workers — a third
    /// registry op on the same engine completes while both humans have
    /// not decided, and both transfers then land once accepted. (Under
    /// the retired Condvar gate this test starves: both workers block,
    /// the third op cannot run.)
    #[test]
    fn pending_consents_do_not_starve_the_engine_runtime() {
        let registry = TestRegistry::spawn();
        let engine = ak_engine_new(); // A1, A2 (receivers) + A3
        let engine_f = ak_engine_new(); // B1, B2 (senders)
        let mut receivers = Vec::new();
        let mut senders = Vec::new();
        let mut codes = Vec::new();
        for index in 0..2 {
            let dir = tempfile::tempdir().expect("receiver tempdir");
            let inbox = dir.path().join(format!("inbox{index}"));
            let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
            let runtime = start_task_on(engine, &registry, dir.path());
            assert_eq!(
                ak_task_set_save_dir(engine, runtime.as_ptr(), save_path.as_ptr()),
                0
            );
            let (_, ok, json) = create_code(
                engine,
                &runtime,
                "{\"spec_version\":1,\"role\":\"receiver\"}",
            );
            assert!(ok, "{json}");
            codes.push((code_of(&json), dir, inbox));
            receivers.push(runtime);
        }
        for (code, _, _) in &codes {
            let dir = tempfile::tempdir().expect("sender tempdir");
            let runtime = start_task_on(engine_f, &registry, dir.path());
            let (join_ok, join_json) = join_code_as(engine_f, &runtime, code, Some("sender"));
            assert!(join_ok, "{join_json}");
            senders.push((runtime, conn_of(&join_json), dir));
        }

        // Both senders fire; both receivers park at their consent gates.
        let payload = b"concurrent consent payload".repeat(64);
        let mut ops = Vec::new();
        for (runtime, conn, dir) in &senders {
            std::fs::write(dir.path().join("cc.bin"), &payload).unwrap();
            let paths = CString::new(json_paths(&dir.path().join("cc.bin"))).unwrap();
            let mut op = [0u8; 16];
            assert_eq!(
                ak_task_send(
                    engine_f,
                    runtime.as_ptr(),
                    conn.as_ptr(),
                    paths.as_ptr(),
                    op.as_mut_ptr()
                ),
                0
            );
            ops.push(op);
        }
        // Both receivers park at their consent gates; capture the job ids.
        let mut jobs = Vec::new();
        for runtime in &receivers {
            let request = wait_event(engine, runtime, "incoming_request");
            jobs.push(hex16_id(
                request
                    .split("\"job\":\"")
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .expect("job"),
            ));
        }

        // THE P2-07 ASSERTION: with both consents still pending, a third
        // REAL op on the same two-worker engine completes promptly.
        let dir3 = tempfile::tempdir().expect("tempdir 3");
        let third = start_task_on(engine, &registry, dir3.path());
        let started = Instant::now();
        let (_, third_ok, third_json) =
            create_code(engine, &third, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(third_ok, "the runtime is not starved: {third_json}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the third op completed while both consents were pending"
        );

        // Both pending gates then resolve and both transfers land.
        for (index, runtime) in receivers.iter().enumerate() {
            assert_eq!(
                ak_task_consent(engine, runtime.as_ptr(), jobs[index].as_ptr(), 1),
                0
            );
            let (send_ok, send_json) = wait_op(engine_f, &senders[index].0, &ops[index]);
            assert!(send_ok, "transfer {index} lands: {send_json}");
            assert!(
                find_file(&codes[index].2, "cc.bin").is_some(),
                "transfer {index} bytes landed"
            );
        }
        ak_engine_free(engine);
        ak_engine_free(engine_f);
    }

    /// P2-12: the probe echo responder answers a genuine probe (with the
    /// fixed all-zero reply nonce), NEVER answers a reply (its own or
    /// another responder's — the reflection loop killer), and keeps
    /// answering fresh probes afterwards.
    #[test]
    fn echo_responder_answers_probes_but_never_replies_to_replies() {
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &spec_for(dir.path())).expect("runtime");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        // The responder's wildcard bind reports 0.0.0.0:port on Windows;
        // dial it through loopback (exactly what local_candidates does).
        let bound_one = engine
            .runtime()
            .block_on(async { spawn_echo_responder(&rt, [1u8; 16]) })
            .expect("responder one");
        let addr_one = SocketAddr::from(([127, 0, 0, 1], bound_one.port()));
        let bound_two = engine
            .runtime()
            .block_on(async { spawn_echo_responder(&rt, [2u8; 16]) })
            .expect("responder two");
        let addr_two = SocketAddr::from(([127, 0, 0, 1], bound_two.port()));
        let probe_socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe socket");
        probe_socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .expect("read timeout");

        // A genuine probe is answered; the reply carries the zero nonce.
        let nonce = [0xA5u8; 16];
        probe_socket
            .send_to(&ak_net::holepunch::probe_packet(&nonce), addr_one)
            .expect("probe sent");
        let mut buffer = [0u8; 1500];
        let (len, from) = probe_socket
            .recv_from(&mut buffer)
            .expect("a reply arrives");
        assert_eq!(len, 24, "the reply is a probe-shaped packet");
        assert_eq!(
            &buffer[8..len],
            &[0u8; 16][..],
            "the reply nonce is the marker"
        );
        assert_eq!(from, addr_one);
        let reply = buffer[..len].to_vec();

        // The responder never answers its OWN reply (a reply is not a probe).
        probe_socket.send_to(&reply, addr_one).expect("reply sent");
        assert!(
            probe_socket.recv_from(&mut buffer).is_err(),
            "a responder must never answer a reply"
        );

        // Two responders fed a reply stay silent — no mutual answering loop.
        probe_socket
            .send_to(&reply, addr_two)
            .expect("cross reply sent");
        assert!(
            probe_socket.recv_from(&mut buffer).is_err(),
            "a second responder must never answer a reply either"
        );

        // Fresh probes still work: neither responder was consumed.
        probe_socket
            .send_to(&ak_net::holepunch::probe_packet(&[0x5Au8; 16]), addr_two)
            .expect("fresh probe");
        let (len, _) = probe_socket
            .recv_from(&mut buffer)
            .expect("responder two answers");
        assert_eq!(len, 24);
    }

    // -- wave-30 review findings (P1-04/05 residuals, P2-08/09/11
    //    residuals, R29-P2-03) -----------------------------------------------

    /// P1-05 (production wiring): the FFI's own owner loop runs behind the
    /// pre-commit redeem-offer gate. A CROSS-ENGINE join that declares the
    /// SAME direction as the code (explicitly, or through the FFI's
    /// no-hint default on a receiver-role code) is refused BEFORE the
    /// server's commit — the join op fails, no `connected` event fires on
    /// the owner, and the code's single redeem is NOT consumed: a
    /// correctly-directed join of the very same code still pairs.
    #[test]
    fn production_owner_loop_refuses_same_direction_joins_before_commit() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());

        // Phase 1: an EXPLICIT same-direction join is refused pre-commit.
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "receiver-role create: {json}");
        let code = code_of(&json);
        let (wrong_ok, wrong_json) = join_code_as(engine_b, &joiner, &code, Some("receiver"));
        assert!(
            !wrong_ok,
            "a receiver-role join of a receiver-role code must be refused: {wrong_json}"
        );
        assert!(
            wrong_json.contains("unknown task code"),
            "the pre-commit gate refuses with the unified unavailable answer: {wrong_json}"
        );
        assert_no_event_within(engine_a, &owner, "\"event\":\"connected\"", 1500);
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            !text_of(&buf).contains("\"conn\""),
            "the refused joiner holds no connection: {}",
            text_of(&buf)
        );
        // The single redeem survived the refusal: a complementary join of
        // the SAME code pairs (a consumed code would answer exhausted).
        let (right_ok, right_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(right_ok, "the code must still pair: {right_json}");

        // Phase 2: the FFI's no-hint DEFAULT direction (receiver) on a
        // FRESH receiver-role code is equally refused pre-commit, and the
        // code stays usable for an explicit complementary join.
        let (_, ok_two, json_two) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok_two, "{json_two}");
        let code_two = code_of(&json_two);
        let (default_ok, default_json) = join_code(engine_b, &joiner, &code_two);
        assert!(
            !default_ok,
            "the no-role default (receiver) must be refused on a receiver code: {default_json}"
        );
        let dir_c = tempfile::tempdir().expect("tempdir c");
        let joiner_two = start_task_on(engine_b, &registry, dir_c.path());
        let (fixed_ok, fixed_json) = join_code_as(engine_b, &joiner_two, &code_two, Some("sender"));
        assert!(fixed_ok, "the second code still pairs: {fixed_json}");
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-08: the registry CONTROL connection is not a data-plane verdict.
    /// With the whole registry gone, the paired tasks stay live (no
    /// `disconnected` event, `ak_task_list` keeps the connection) and a
    /// real send still delivers bytes — the data plane never depended on
    /// the registry.
    #[test]
    fn registry_connection_loss_keeps_paired_tasks_and_sends_working() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");
        // Let both probes settle, then take the ENTIRE registry away.
        let _ = wait_event(engine_a, &owner, "probe_result");
        drop(registry);

        // No kill event: the tasks outlive the control plane.
        assert_no_event_within(engine_a, &owner, "disconnected", 1500);
        assert_no_event_within(engine_b, &joiner, "disconnected", 1500);
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"connected\""),
            "the paired task outlives the registry: {}",
            text_of(&buf)
        );

        // A real send over the (registry-free) data plane still lands.
        let payload = b"data plane without a registry".repeat(32);
        std::fs::write(dir_b.path().join("noreg.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("noreg.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0,
            "the send is accepted after the registry died: {}",
            last_error_of(engine_b)
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
        assert!(send_ok, "the transfer completes: {send_json}");
        let delivered = find_file(&inbox, "noreg.bin").expect("the bytes landed");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P1-04 residual: ONE connection's failed handshake (a stranger whose
    /// TLS the server side rejects — wrong pin, so the client aborts
    /// mid-handshake) does NOT end the resident receiver: the bounded
    /// `handshake_failed` event fires and the legitimate paired sender
    /// still completes a transfer on the very same listener.
    #[test]
    fn failed_stranger_handshake_does_not_end_the_listener() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let inbox = dir_a.path().join("inbox");
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let endpoint = endpoint_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

        // The stranger pins the WRONG fingerprint: the client aborts the
        // TLS handshake after the server certificate arrives, so the
        // server side of the accept fails (Transport/PeerIdentity) — the
        // exact error class the old loop treated as "endpoint dead".
        std::fs::write(dir_a.path().join("midhandshake.bin"), b"stranger").unwrap();
        let failure = wrong_pin_send_fails(endpoint, &dir_a.path().join("midhandshake.bin"));
        assert!(
            !failure.is_empty(),
            "the wrong-pin dial must fail: {failure}"
        );
        // The failure is recorded and the loop CONTINUES listening.
        let recorded = wait_event(engine_a, &owner, "handshake_failed");
        assert!(
            recorded.contains(&core::hex_encode(&code_id_of(&json))),
            "the bounded failure names the code: {recorded}"
        );

        // The legitimate sender completes on the very same listener.
        let payload = b"legitimate after a failed stranger handshake".repeat(16);
        std::fs::write(dir_b.path().join("after.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir_b.path().join("after.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0,
            "the send is accepted: {}",
            last_error_of(engine_b)
        );
        let request = wait_event(engine_a, &owner, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
        assert!(
            send_ok,
            "the listener survived the handshake failure: {send_json}"
        );
        let delivered = find_file(&inbox, "after.bin").expect("the file landed");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// R29-P2-03 (deterministic half): the op id a request reserves and
    /// the RUNNING op entry are published ATOMICICALLY — polling the id
    /// while the launcher has not even run yet answers `running`, never
    /// the `-4` of an unregistered op.
    #[test]
    fn reserved_op_id_is_pollable_before_the_launcher_runs() {
        let registry = TestRegistry::spawn();
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        let args = crate::core::json::parse(
            "{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"e1e2e3d4e5f60718293a4b5c6d7e8f90\"}",
        )
        .expect("args");
        // The gate reserves AND registers — then we STOP between the gate
        // and the launcher (the report's exact interleaving).
        let reserved = match gate_request(&rt, "task_create_code", None, false, &args)
            .expect("gate accepts")
        {
            RequestGate::Fresh(reserved) => reserved,
            _other => panic!("fresh request, got a different gate outcome"),
        };
        let snapshot = task_op_result(&engine, &runtime, &reserved.op_id).expect("pollable");
        match snapshot {
            OpResult::Running => {}
            _other => panic!("the reserved op must read running, got a settled or missing op"),
        }
        // The reservation launches exactly once and completes normally.
        let op_id = launch_task_op(
            &engine,
            &rt,
            TaskOpSpec::CreateCode {
                role: TaskRole::Sender,
                candidates: Vec::new(),
                ttl_secs: TASK_CODE_TTL_SECS,
            },
            Some(reserved),
        )
        .expect("launch");
        let (ok, json) = wait_op_owned(&engine, &runtime, &op_id);
        assert!(ok, "{json}");
        // The replay of the same request answers the SAME op.
        match gate_request(&rt, "task_create_code", None, false, &args).expect("replay") {
            RequestGate::Replay(replayed) => assert_eq!(replayed, op_id),
            _other => panic!("replay, got a different gate outcome"),
        }
    }

    /// R29-P2-03 (race half): a concurrent retry that replays a just-
    /// reserved request polls the op IMMEDIATELY — running or finished,
    /// never `-4` — and the whole race starts exactly one op.
    #[test]
    fn concurrent_retry_polls_a_registered_op_never_minus_four() {
        let registry = TestRegistry::spawn();
        let engine = Arc::new(AkEngine::new().expect("engine"));
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
        let request_id = "f1f2f3d4e5f60718293a4b5c6d7e8f90";
        let spec =
            format!("{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{request_id}\"}}");
        let racer = threaded_create(Arc::clone(&engine), runtime, spec.clone());
        // From THIS thread: retry the same request concurrently — the
        // gate answers the racer's op id (fresh or replay, one atomic
        // acceptance) — and poll that id at once, before the racer's
        // launcher (let alone its op body) necessarily ran.
        let op_id = task_create_code_start(&engine, &runtime, &spec)
            .expect("same-spec retry is accepted (fresh or replay)");
        // The immediate poll must never observe an unregistered op.
        match task_op_result(&engine, &runtime, &op_id).expect("poll") {
            OpResult::Running | OpResult::Finished { .. } => {}
        }
        // Exactly one op for the whole race: the racer's answer is the
        // same id and it completes once.
        let racer_op = racer.recv().expect("racer answered").expect("accepted");
        assert_eq!(racer_op, op_id, "one atomically accepted op");
        let (ok, json) = wait_op_owned(&engine, &runtime, &op_id);
        assert!(ok, "{json}");
        assert!(
            json.contains("\"code\":\"AKI1-"),
            "exactly one claim: {json}"
        );
    }

    /// P2-09 residual: the connection settle is HONEST. A joiner that
    /// advertises only a dead candidate leaves the OWNER's entry
    /// `unreachable` in `ak_task_list` (matching its `probe_result`
    /// event), while the joiner's own probe of the owner's live endpoint
    /// settles `connected`.
    #[test]
    fn all_failed_probe_settles_unreachable_not_connected() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        // Explicit candidates are advertised verbatim (no echo responder,
        // no real port): the owner's probe of 127.0.0.1:1 is honestly
        // dead, and the settle must SAY so.
        let spec = CString::new(format!(
            "{{\"spec_version\":1,\"code\":\"{code}\",\"role\":\"sender\",\
              \"candidates\":[\"127.0.0.1:1\"]}}"
        ))
        .expect("spec CString");
        let mut op_id = [0u8; 16];
        assert_eq!(
            ak_task_join(engine_b, joiner.as_ptr(), spec.as_ptr(), op_id.as_mut_ptr()),
            0,
            "join accepted: {}",
            last_error_of(engine_b)
        );
        let (join_ok, join_json) = wait_op(engine_b, &joiner, &op_id);
        assert!(join_ok, "{join_json}");
        // The joiner's own probe of the owner's live endpoint succeeds...
        assert!(
            join_json.contains("\"probe\":\"verified\""),
            "the joiner's probe verdict rides the result: {join_json}"
        );
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"connected\""),
            "the joiner settles connected: {}",
            text_of(&buf)
        );
        // ...while the owner's probe of the dead candidate settles
        // unreachable — never a fake connected.
        let probe = wait_event(engine_a, &owner, "probe_result");
        assert!(probe.contains("\"reachable\":false"), "{probe}");
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        assert!(
            text_of(&buf).contains("\"state\":\"unreachable\""),
            "ak_task_list agrees with the probe verdict: {}",
            text_of(&buf)
        );
        assert!(
            !text_of(&buf).contains("\"state\":\"connected\""),
            "a dead candidate must not settle connected: {}",
            text_of(&buf)
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// P2-11 residual: a pending receive-consent request ends VISIBLY —
    /// the 30 s window's auto-decline emits `consent_expired`, a
    /// disconnect of the attributed connection resolves the request and
    /// emits `request_closed`/`disconnected`, and decided requests answer
    /// the frozen `-4`.
    #[test]
    fn consent_requests_close_visibly_on_expiry_and_disconnect() {
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        let code_id = [7u8; 16];
        let conn_id = [9u8; 16];
        let sender_fp = DeviceKeyPair::generate().fingerprint();
        {
            let mut conns = rt.conns.lock().expect("conn table");
            conns.insert(
                conn_id,
                TaskConn {
                    conn_id,
                    role: TaskRole::Receiver,
                    state: ConnState::Connected,
                    candidates: Vec::new(),
                    peer_endpoint: None,
                    peer_fp: Some(sender_fp),
                    code_id,
                    code: "AKI1-TEST".into(),
                },
            );
        }
        let info = ConsentInfo {
            job_id: [0u8; 16],
            root_name: "expiry.bin".into(),
            file_count: 1,
            total_bytes: 16,
            sender_fingerprint: sender_fp,
        };
        let rt_for_close = Arc::clone(&rt);
        let rt_in = Arc::clone(&rt);
        let bound_dir = dir.path().join("bound-inbox");
        let bound_save_json = quoted(&bound_dir.to_string_lossy());
        let (expired_declined, closed_declined) = engine.runtime().block_on(async move {
            // Short window: the auto-decline fires while we await.
            let ConsentAnswer::Pending(expiry_rx) = register_consent(
                &rt_in,
                &code_id,
                &info,
                &bound_dir,
                Duration::from_millis(300),
            ) else {
                panic!("the attributed offer must register as pending");
            };
            // Long window: only the connection's disconnect resolves it.
            let ConsentAnswer::Pending(closed_rx) =
                register_consent(&rt_in, &code_id, &info, &bound_dir, Duration::from_secs(60))
            else {
                panic!("the second offer must register as pending");
            };
            let expired = expiry_rx.await.unwrap_or(false);
            // The disconnect drives the still-pending request to its end.
            rt_for_close.decline_pending_of_conn(&conn_id);
            let closed = closed_rx.await.unwrap_or(false);
            (expired, closed)
        });
        assert!(!expired_declined, "the window auto-declines");
        assert!(!closed_declined, "the disconnect declines");
        // Both terminal events are observable, attributed to the conn.
        let events: Vec<String> = rt
            .events
            .lock()
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        let conn_hex = core::hex_encode(&conn_id);
        let expired_line = events
            .iter()
            .find(|line| line.contains("consent_expired") && line.contains(&conn_hex))
            .expect("the expiry is a visible request-terminal event");
        let expired_job = hex16_id(
            expired_line
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert!(
            events.iter().any(|line| line.contains("request_closed")
                && line.contains("\"reason\":\"disconnected\"")
                && line.contains(&conn_hex)),
            "the disconnect closes its pending request visibly: {events:?}"
        );
        // Wave-31 P2-06: the consent event carries the transfer's BOUND
        // save dir — the one binding, not a fresh read of the setting.
        assert!(
            events
                .iter()
                .any(|line| line.contains("\"event\":\"incoming_request\"")
                    && line.contains(&format!("\"saveDir\":{bound_save_json}"))),
            "the consent event carries the bound save dir: {events:?}"
        );
        // Decided requests answer the frozen -4.
        let decided = task_consent(&engine, &runtime, &expired_job, true);
        assert!(decided.is_err(), "the expired request is already decided");
        assert_eq!(
            decided.err().map(|e| e.code),
            Some(crate::core::AK_ERR_NOT_FOUND)
        );
    }

    // -- wave-31 review findings (R30-P2-02, R30-P2-03, P2-06 residual) ----

    /// Poll an OWNED runtime's event queue until one event whose body
    /// contains `needle` arrives (draining older events like `wait_event`).
    fn wait_owned_event(rt: &Arc<TaskRt>, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let popped = rt
                .events
                .lock()
                .ok()
                .and_then(|mut queue| queue.pop_front());
            if let Some(line) = popped {
                if line.contains(needle) {
                    return line;
                }
                continue;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// R30-P2-02: both sides of a pairing share the SAME code id — the
    /// join result and BOTH `connected` events (the owner's from its
    /// resident owner loop, the joiner's from its own bookkeeping) carry
    /// the create result's registry code id, so a joiner that typed the
    /// code (and never saw the create result) can fill its conn→code
    /// mapping.
    #[test]
    fn connected_events_and_join_result_carry_the_shared_code_id() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let code_id_hex = core::hex_encode(&code_id_of(&json));
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        assert!(
            join_json.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
            "the join result carries the redeemed code's id: {join_json}"
        );
        let on_owner = wait_event(engine_a, &owner, "\"event\":\"connected\"");
        assert!(
            on_owner.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
            "the owner's connected event carries the shared code id: {on_owner}"
        );
        let on_joiner = wait_event(engine_b, &joiner, "\"event\":\"connected\"");
        assert!(
            on_joiner.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
            "the joiner's connected event carries the shared code id: {on_joiner}"
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    /// R30-P2-02 (the progress-key migration): receive progress rows ride
    /// under the CONNECTION id — the one correlation key both sides of a
    /// pairing hold — exactly like the sender's rows; no receive row may
    /// surface under the code id (the joiner's Dart side maps progress by
    /// conn id alone and would never see a code-id row).
    #[test]
    fn receive_progress_rows_ride_under_the_connection_id() {
        let registry = TestRegistry::spawn();
        let engine_a = Arc::new(AkEngine::new().expect("engine a"));
        let engine_b = Arc::new(AkEngine::new().expect("engine b"));
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = task_start(&engine_a, &registry_spec(&registry, dir_a.path())).expect("rt");
        let rt_a = task_runtime_of(&engine_a, &owner).expect("task runtime");
        let joiner = task_start(&engine_b, &registry_spec(&registry, dir_b.path())).expect("rt2");
        let create_op = task_create_code_start(
            &engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        )
        .expect("create accepted");
        let (ok, json) = wait_op_owned(&engine_a, &owner, &create_op);
        assert!(ok, "{json}");
        let code = code_of(&json);
        let code_id_hex = core::hex_encode(&code_id_of(&json));
        let join_op = task_join_start(
            &engine_b,
            &joiner,
            &format!("{{\"spec_version\":1,\"code\":\"{code}\",\"role\":\"sender\"}}"),
        )
        .expect("join accepted");
        let (join_ok, join_json) = wait_op_owned(&engine_b, &joiner, &join_op);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        // The owner's connection entry (its receiver admission is bound to
        // it) arrives with the owner-loop completion.
        let _ = wait_owned_event(&rt_a, "\"event\":\"connected\"");

        let payload = b"progress-key migration payload".repeat(64);
        std::fs::write(dir_b.path().join("job.bin"), &payload).expect("source file");
        let send_op = task_send_start(
            &engine_b,
            &joiner,
            &conn,
            &json_paths(&dir_b.path().join("job.bin")),
        )
        .expect("send accepted");
        let request = wait_owned_event(&rt_a, "incoming_request");
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        task_consent(&engine_a, &owner, &job, true).expect("consent");
        let (send_ok, send_json) = wait_op_owned(&engine_b, &joiner, &send_op);
        assert!(send_ok, "the transfer completes: {send_json}");
        let _ = wait_owned_event(&rt_a, "\"event\":\"done\"");

        // The RECEIVER's progress rows: keyed by the conn id, never the
        // code id (rows survive completion in the engine-wide queue).
        let conn_hex = core::hex_encode(&conn);
        let recv_rows: Vec<String> = rt_a
            .engine_progress
            .lock()
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        assert!(
            recv_rows
                .iter()
                .any(|row| row.contains(&format!("\"job\":\"{conn_hex}\""))
                    && row.contains("\"event\":\"accepted\"")),
            "receive progress rides under the connection id: {recv_rows:?}"
        );
        assert!(
            recv_rows
                .iter()
                .all(|row| !row.contains(&format!("\"job\":\"{code_id_hex}\""))),
            "no receive row may ride under the code id: {recv_rows:?}"
        );
        // The SENDER's rows on its own engine share the SAME key.
        let rt_b = task_runtime_of(&engine_b, &joiner).expect("joiner runtime");
        let send_rows: Vec<String> = rt_b
            .engine_progress
            .lock()
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        assert!(
            send_rows
                .iter()
                .any(|row| row.contains(&format!("\"job\":\"{conn_hex}\""))),
            "send progress keeps the connection-id key: {send_rows:?}"
        );
    }

    /// R30-P2-03 (deterministic half): an offer registered on an
    /// already-closed connection — the report's interleaving with the
    /// disconnect's conns flip landed first — is refused inside the
    /// registration's critical section: NO `incoming_request` surfaces,
    /// nothing stays pending, and any consent for the never-published
    /// request answers the frozen `-4`.
    #[test]
    fn register_consent_on_a_closed_connection_publishes_nothing() {
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        let code_id = [7u8; 16];
        let conn_id = [9u8; 16];
        let sender_fp = DeviceKeyPair::generate().fingerprint();
        {
            let mut conns = rt.conns.lock().expect("conn table");
            conns.insert(
                conn_id,
                TaskConn {
                    conn_id,
                    role: TaskRole::Receiver,
                    state: ConnState::Connected,
                    candidates: Vec::new(),
                    peer_endpoint: None,
                    peer_fp: Some(sender_fp),
                    code_id,
                    code: "AKI1-TEST".into(),
                },
            );
        }
        // The disconnect's first half (flip + `disconnected` line) and its
        // pending decline BOTH complete before the registration runs.
        rt.push_disconnected_once(&conn_id, "local");
        rt.decline_pending_of_conn(&conn_id);
        let info = ConsentInfo {
            job_id: [0u8; 16],
            root_name: "late.bin".into(),
            file_count: 1,
            total_bytes: 16,
            sender_fingerprint: sender_fp,
        };
        let reg_rt = Arc::clone(&rt);
        let answer = engine.runtime().block_on(async move {
            register_consent(
                &reg_rt,
                &code_id,
                &info,
                std::path::Path::new("D:/unused"),
                Duration::from_secs(60),
            )
        });
        assert!(
            matches!(answer, ConsentAnswer::Now(false)),
            "an offer on a closed connection is refused on the spot"
        );
        let events: Vec<String> = rt
            .events
            .lock()
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        assert!(
            !events.iter().any(|line| line.contains("incoming_request")),
            "no incoming_request may surface for a dead request: {events:?}"
        );
        assert!(
            rt.pending
                .lock()
                .map(|pending| pending.is_empty())
                .unwrap_or(false),
            "nothing stays pending"
        );
        let decided = task_consent(&engine, &runtime, &[1u8; 16], true);
        assert_eq!(
            decided.err().map(|error| error.code),
            Some(crate::core::AK_ERR_NOT_FOUND)
        );
    }

    /// R30-P2-03 (race half): a registration racing a disconnect of its
    /// connection — many real thread interleavings of the exact window
    /// (pending insert vs. flip + decline) — can never leave a published
    /// `incoming_request` without its `request_closed` landing LATER in
    /// the same window, and never leaves a pending entry behind.
    #[test]
    fn disconnect_register_race_never_surfaces_a_late_incoming() {
        let engine = Arc::new(AkEngine::new().expect("engine"));
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
        let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
        let code_id = [7u8; 16];
        let conn_id = [9u8; 16];
        let sender_fp = DeviceKeyPair::generate().fingerprint();
        {
            let mut conns = rt.conns.lock().expect("conn table");
            conns.insert(
                conn_id,
                TaskConn {
                    conn_id,
                    role: TaskRole::Receiver,
                    state: ConnState::Connected,
                    candidates: Vec::new(),
                    peer_endpoint: None,
                    peer_fp: Some(sender_fp),
                    code_id,
                    code: "AKI1-TEST".into(),
                },
            );
        }
        let info = ConsentInfo {
            job_id: [0u8; 16],
            root_name: "race.bin".into(),
            file_count: 1,
            total_bytes: 16,
            sender_fingerprint: sender_fp,
        };
        for _round in 0..64 {
            // Re-arm the connection (the previous round severed it).
            rt.conns
                .lock()
                .expect("conn table")
                .get_mut(&conn_id)
                .expect("conn entry")
                .state = ConnState::Connected;
            let base = rt.events.lock().expect("event queue").len();
            let reg_rt = Arc::clone(&rt);
            let reg_engine = Arc::clone(&engine);
            let reg_info = info.clone();
            let close_rt = Arc::clone(&rt);
            let registrar = thread::spawn(move || {
                reg_engine.runtime().block_on(async move {
                    matches!(
                        register_consent(
                            &reg_rt,
                            &code_id,
                            &reg_info,
                            std::path::Path::new("D:/unused"),
                            Duration::from_millis(25),
                        ),
                        ConsentAnswer::Now(false)
                    )
                })
            });
            let closer = thread::spawn(move || {
                // `task_disconnect`'s exact order: flip + `disconnected`
                // line first, then the pending decline of the connection.
                close_rt.push_disconnected_once(&conn_id, "local");
                close_rt.decline_pending_of_conn(&conn_id);
            });
            let _refused = registrar.join().expect("registrar answered");
            closer.join().expect("closer answered");
            let events: Vec<String> = rt
                .events
                .lock()
                .expect("event queue")
                .iter()
                .skip(base)
                .cloned()
                .collect();
            for (index, line) in events.iter().enumerate() {
                if line.contains("\"event\":\"incoming_request\"") {
                    let job_hex = line
                        .split("\"job\":\"")
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .expect("job id");
                    // R31-P3-02: EITHER terminal is legal — the
                    // disconnect-driven request_closed OR the 25 ms
                    // consent window's consent_expired (the race may let
                    // the expiry task win before the closer thread gets
                    // scheduled; both are correct request terminals that
                    // can only appear AFTER the incoming was published).
                    assert!(
                        events[index + 1..].iter().any(|later|
                            (later.contains("\"event\":\"request_closed\"")
                                || later.contains("\"event\":\"consent_expired\""))
                            && later.contains(&format!("\"job\":\"{job_hex}\""))),
                        "every published request reaches a terminal (closed or expired) LATER: {events:?}"
                    );
                }
            }
            assert!(
                rt.pending.lock().expect("pending table").is_empty(),
                "no request survives the round"
            );
        }
    }

    /// P2-06 residual (wave-31): the save folder is ONE immutable binding
    /// per transfer — resolved when the connection is accepted, carried
    /// identically by the `incoming_request` event, the engine's actual
    /// writes and the `done` event. Changing the setting DURING the
    /// pending-consent window cannot skew them: the in-flight transfer
    /// lands in its bound folder and `done` reports exactly that.
    #[test]
    fn save_dir_binding_is_immutable_across_the_consent_window() {
        let registry = TestRegistry::spawn();
        let engine_a = ak_engine_new();
        let engine_b = ak_engine_new();
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let owner = start_task_on(engine_a, &registry, dir_a.path());
        let joiner = start_task_on(engine_b, &registry, dir_b.path());
        let alpha = dir_a.path().join("alpha");
        let alpha_text = alpha.to_string_lossy().replace('\\', "/");
        let beta = dir_a.path().join("beta");
        let beta_text = beta.to_string_lossy().replace('\\', "/");
        let alpha_path = CString::new(alpha_text.clone()).expect("path CString");
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), alpha_path.as_ptr()),
            0
        );

        let (_, ok, json) = create_code(
            engine_a,
            &owner,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

        let payload = b"immutable binding payload".repeat(64);
        std::fs::write(dir_b.path().join("bound.bin"), &payload).expect("source file");
        let paths = CString::new(json_paths(&dir_b.path().join("bound.bin"))).unwrap();
        let mut send_op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_b,
                joiner.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                send_op.as_mut_ptr()
            ),
            0
        );
        // The banner's saveDir is the transfer's binding (alpha) — not a
        // fresh read of the setting.
        let request = wait_event(engine_a, &owner, "incoming_request");
        assert!(
            request.contains(&format!("\"saveDir\":\"{alpha_text}\"")),
            "the consent event shows the bound target: {request}"
        );
        // The user changes the setting DURING the pending window.
        let beta_path = CString::new(beta_text.clone()).expect("path CString");
        assert_eq!(
            ak_task_set_save_dir(engine_a, owner.as_ptr(), beta_path.as_ptr()),
            0
        );
        let job = hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("job id"),
        );
        assert_eq!(
            ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
        assert!(send_ok, "{send_json}");

        // The in-flight transfer landed in its BOUND folder and `done`
        // reports exactly that — the actual disk result, not the newer
        // setting.
        let delivered = find_file(&alpha, "bound.bin").expect("the file lands in the bound folder");
        assert_eq!(std::fs::read(&delivered).unwrap(), payload);
        assert!(
            find_file(&beta, "bound.bin").is_none(),
            "a mid-pending setting change cannot move the in-flight transfer"
        );
        let done = wait_event(engine_a, &owner, "\"event\":\"done\"");
        assert!(
            done.contains(&format!("\"saveDir\":\"{alpha_text}\"")),
            "done reports the actual landing dir: {done}"
        );
        assert!(
            !done.contains(&format!("\"saveDir\":\"{beta_text}\"")),
            "done never reports a dir the transfer did not use: {done}"
        );
        ak_engine_free(engine_a);
        ak_engine_free(engine_b);
    }

    // -- helpers --------------------------------------------------------------

    fn json_paths(path: &std::path::Path) -> String {
        format!(
            "{{\"spec_version\":1,\"paths\":[\"{}\"]}}",
            path.to_string_lossy().replace('\\', "/")
        )
    }

    fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        fn walk(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(hit) = walk(&path, name) {
                        return Some(hit);
                    }
                } else if path.file_name().map(|n| n == name).unwrap_or(false) {
                    return Some(path);
                }
            }
            None
        }
        walk(root, name)
    }
}
