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
//! reachability of the peer host). Since CR-P1-02 改法第 1 条 the derived
//! list carries BOTH families — every interface IPv4, then the
//! globally-routable interface IPv6 (link-local excluded; see
//! `filter_and_order_candidates` for the full selection rule) — and the
//! resident receiver and echo responder bind DUAL-STACK ([::] with
//! V6ONLY cleared), so a v6 candidate is genuinely served and two NAT'd
//! IPv6-capable peers get a NAT-free path (a host without IPv6 falls
//! back to the historical v4-only binds, logged via `receiver_bind`).
//! Since CR-P1-02 改法第 2 条 the derived list additionally carries the
//! STUN server-reflexive address: the receiver's UDP socket is created
//! explicitly (`holepunch::bind_dual_stack_udp`, v4 wildcard fallback),
//! the optional `stun_addr` of the start spec is queried ON THAT SAME
//! SOCKET (bounded by [`STUN_QUERY_BUDGET`], fail-open), and only then is
//! the socket handed to QUIC via `DirectEndpoint::bind_with_socket` — so
//! the NAT mapping the query created stays valid for the QUIC listener
//! and the reflected public address is genuinely dialable (`bind_receiver_socket`
//! documents the full lifecycle; the symmetric-NAT residual is 改法第 4
//! 条's honest-limit case). Explicit `candidates` in the spec replace the
//! derived list verbatim (caller-knows-better), and then neither the
//! reflexive candidate nor an echo port is appended — the probe reports
//! exactly what the caller advertised.
//! Since CR-P1-02 改法第 3 条 the SENDER's connect phase opens with one
//! simultaneous-probe round (`probe_dial_round`): a dedicated dual-stack
//! socket fires `holepunch::probe` packets at every dial candidate for a
//! bounded 1 s window, and when the round observes an answering address
//! (the receiver's echo responder reply, or a still-overlapping peer
//! probe round's source) that observed address leads the QUIC dial order
//! under a reduced first-attempt budget — strictly additive, with the
//! static LAN-first order as the always-available fallback. The receiver
//! side consumes its own observations (the echo responder's most recent
//! observed probe source) only for its one outbound dial role, the
//! idle-close notify; the QUIC accept is socket-bound and needs no dial.
//! CR-P1-05 (review 2026-11-11 §4.5) completes the second direction on
//! THE socket that carries QUIC, both ways: the joining/creating SENDER
//! binds its future-QUIC dial socket before the redeem/claim
//! (`bind_sender_socket` — same lifecycle and fail-open budget as the
//! receiver bind; the `sender_bind` event mirrors `receiver_bind`), the
//! reflexive address rides the sender's OWN endpoint blob (the paired
//! receiver's punch target), the send-time probe round runs on a
//! `try_clone` of that socket (`probe_dial_round_on`), and every dial
//! attempt adopts it (`SendSpec::adopt_socket` +
//! `DirectEndpoint::dial_with_socket`) so the QUIC Initial leaves from
//! the port the STUN query mapped. The RECEIVER punches back: a
//! send-only `try_clone` of the QUIC listener socket (stashed under the
//! code id) feeds `punch_candidates` — probe-shaped packets to every
//! advertised sender candidate, every 150 ms for ≤4 s, concurrent with
//! the sender's dial, stopped by the first accepted connection — so a
//! port-restricted-cone NAT on the receiver side admits the Initial
//! (the one-way-punching defect the review closed).
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
//!   `ak_task_consent` decides, the 120 s window auto-declines
//!   (a timer task, not a blocked thread; the frozen v1 model said
//!   30 s — raised by acceptance E2/G4, review 2026-11-12 §6), the
//!   runtime tears down, or
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use ak_engine::{
    receive_accepted_consent, send, ConsentAnswer, ConsentInfo, EngineError, Progress, ReceiveSpec,
    Report, SendSpec, TransferConsent,
};
use ak_identity::pairing_txn::{random_id16, VerifiedPairing};
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{
    ClientConfig, OrchestratorConfig, PairingOrchestrator, PeerEndpoint, ProbeOutcome,
    RegistryClient, TaskDirection, TaskDisconnectReason, TaskEvent, TaskPaired, TaskPairing,
    TaskPairingConfig, TASK_CODE_TTL_SECS,
};
// CR-P2-11 (review 2026-11-11 §4.5): the v1 connectivity plumbing this
// file used to duplicate against crates/ak-cli/src/task_cli.rs lives in
// ONE shared module now — ak-session `pairing::task_net` (a pure move:
// same bodies, same tunables, same rationale docs; the mirrored unit
// tests moved with them). This adapter keeps only the FFI-side wiring —
// events instead of stdout lines, the task runtime's state. The shared
// names are imported directly so the moved call sites read unchanged.
use ak_session::pairing::task_net::{
    bind_receiver_socket, bind_sender_socket, blob_candidates, deliver_close_notify,
    drive_with_connect_budget, handshake_failure_word, insert_reflexive_candidate,
    local_candidates, one_line_reason, order_candidates, probe_dial_round, probe_dial_round_on,
    probe_with_retry, round_probe_outcome, sender_pairing_keeper, staggered_dial_pass,
    task_error_message, windows_root_name_refusal, AttemptVerdict, ConnectMarks,
    SenderKeepaliveState, OPENER_KEEPALIVE_BUDGET, RECEIVER_BIND_TIMEOUT, REGISTRY_CONNECT_TIMEOUT,
    SENDER_DIAL_PAUSE, TASK_CLOSE_MESSAGE, TASK_CONNECT_PASSES, TASK_PROBE_OPTIONS,
    TASK_REDEEM_WINDOW_MS, TASK_SEND_IDLE, TASK_SETTLE_PAUSE,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::core::{self, FfiError};
use crate::pairing::{OpEntry, OpOutcome, OpResult};
use crate::AkEngine;

mod events;
mod join_send;
mod lease;
mod ops;
mod runs;
mod stack;

pub use events::*;
use join_send::*;
use lease::*;
pub use ops::*;
use runs::*;
pub use stack::*;

/// Offer-level consent window: a pending `incoming_request` holds the
/// transfer until `ak_task_consent` decides or this window elapses (then
/// the request auto-declines — never auto-accepts). 120 s since the
/// acceptance decision E2 (review 2026-11-12 §6, batch-F G4): the frozen
/// v1 model's 30 s was too short for a phone user reading the banner,
/// and CR-P2-04's QUIC keep-alive (10 s) plus idle budget (60 s) now
/// hold the connection through the whole wait. The event carries the
/// live value (`windowSecs`) so the UI countdown never guesses.
pub(crate) const PENDING_CONSENT_WINDOW: Duration = Duration::from_secs(120);

// Every other tunable of the connectivity plane (connect budgets, probe
// round, STUN query, echo responder, close notify, registry/redeem waits)
// moved to the shared `task_net` module — see the import above.

/// Cap of one task runtime's event queue (same budget as every plane).
const MAX_QUEUED_EVENTS: usize = 65_536;

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
/// 120 s auto-decline timer, a disconnect of its connection or runtime
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
    /// Batch G6 (R36-P2-04, the client half): this stack's hold on the
    /// identity's per-identity control-connection lock — the SAME
    /// primitive the CLI task commands and the daemon use
    /// (`ak_session::pairing::connection_lock`). Acquired in
    /// [`ensure_stack`] BEFORE the registry dial; released in
    /// [`TaskRt::teardown`] AFTER the client closed (in-flight op bodies
    /// may still hold `Arc<TaskStack>` clones, so the release must not
    /// wait for the last Arc). A failed build releases via the stack's
    /// drop. A second task runtime on the same id-dir — in THIS process
    /// (same PID: refused as a second runtime) or in another process
    /// (live foreign holder) — fails its first op loudly instead of
    /// silently kicking this stack's control connection. One-shot
    /// registry verbs (`invite revoke`) deliberately do NOT take this
    /// lock; they rely on the server's replacement reason frame plus the
    /// keeper's replacement pacing.
    connection_lock: StdMutex<Option<ak_session::pairing::ConnectionLockLease>>,
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

/// CR-P2-05 (review 2026-11-11, 改法第 1 条后半): the registered native
/// event doorbell of one task runtime — a plain C callback Rust invokes
/// whenever a task-plane event line is QUEUED, so the embedder can stop
/// polling at a fixed cadence and drain (`ak_pairing_poll_event`) only
/// when something actually arrived.
///
/// Design choice (Option A of the review's two candidates, the one
/// implemented): a plain `extern "C" fn(ctx)` registered through the new
/// `ak_task_set_event_notify` export, paired with Dart's builtin
/// `NativeCallable.listener` (dart:ffi, no new dependencies). The
/// alternative — `Dart_PostCObject` via an embedded `dart_api_dl.c` —
/// was rejected: it ships ~200 lines of Dart SDK source inside ak-ffi
/// (licensing-sensitive), needs a `Dart_InitializeApiDL` bootstrap on the
/// embedder, and marshals events as `Dart_CObject` values, which is
/// exactly the frozen-shape/lifetime hazard the review warned about. The
/// doorbell carries NO payload — the poll stays the single authoritative
/// channel — so no frozen §8.2/§8.3 shape is touched, and an embedder
/// that never registers observes byte-identical pure-polling behavior.
///
/// Thread/lifetime contract (mirrored in ffi-v2.md §8.6):
///
/// * The callback fires on WHATEVER thread queued the event — the
///   engine's embedded tokio workers for async facts, or the FFI
///   caller's own thread when an export queues an event synchronously
///   (e.g. `ak_task_disconnect`). That is safe for
///   `NativeCallable.listener` (its trampoline only POSTS to the owning
///   isolate's event loop from any thread) and adds no latency and no
///   notifier thread; a fire-and-forget C callback of another embedder
///   must simply not block.
/// * The call happens UNDER the slot lock. That is deliberate: the
///   trampoline never blocks or re-enters this library, so contention is
///   bounded, while the lock gives the hard lifetime guarantee
///   `ak_task_set_notify(…, None, …)` (unregister) needs — it returns
///   only after an in-flight fire completed, and no later fire can
///   start, so the caller may then destroy the callback
///   (`NativeCallable.close`) without a use-after-free window.
/// * The wrapper itself cannot panic (a fn-pointer call with a usize —
///   no allocation, no indexing), and an `extern "C"` callee that
///   unwinds aborts the process by definition of the ABI, so nothing is
///   caught around the call: no panic can cross this boundary.
pub(crate) struct TaskEventNotify {
    /// The registered callback (a plain non-unwinding C function).
    func: extern "C" fn(ctx: *mut std::os::raw::c_void),
    /// The caller's opaque context token, stored as an address so the
    /// slot stays `Send` + `Sync` inside the shared runtime without a
    /// raw pointer (this module is `forbid(unsafe_code)`). Rust never
    /// dereferences it — it is passed back verbatim.
    ctx_addr: usize,
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
    /// Optional STUN server (CR-P1-02 改法第 2 条, "STUN 地址与注册中心
    /// 一起配置"): queried ON the receiver's own UDP socket before the
    /// socket is handed to QUIC, so the reflected address rides the
    /// endpoint blob as a dialable candidate. `None` (absent/empty
    /// `stun_addr` in the start spec) = no query, fail-open — local
    /// candidates still cover same-subnet/LAN joins.
    pub(crate) stun_addr: Option<SocketAddr>,
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
    /// Most recent probe source OBSERVED by this runtime's echo
    /// responder (CR-P1-02 改法第 3 条), in normalized plain-family form.
    /// A single-slot, most-recent-wins cache on purpose (bounded): the
    /// only consumer is [`close_notify_peer`] — the receiver side's one
    /// outbound dial role — which reads it opportunistically and dials
    /// the observed source ahead of the static candidates. Honest
    /// scoping: the slot is per RUNTIME, not per pairing, so with
    /// several codes over one runtime's life a stale observation from
    /// another pairing can lead that best-effort dial; the cost is one
    /// bounded failed attempt before the advertised list (the close
    /// notify is fire-and-forget, never a correctness gate). The task
    /// event set is contract-frozen (§8.3), so the FFI surface exposes
    /// no `probe_observed` event — see the C3 report's contract
    /// proposal.
    echo_observed: Arc<StdMutex<Option<SocketAddr>>>,
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
    /// CR-P2-05: the optional native event doorbell ([`TaskEventNotify`]
    /// has the full contract). `None` = pure polling, bit-identical to
    /// the pre-doorbell behavior.
    notify: StdMutex<Option<TaskEventNotify>>,
    /// CR-P1-05 (review 2026-11-11 §4.5): the per-code UDP hole sockets,
    /// keyed by registry code id (a local placeholder id until the
    /// claim/redeem assigns the real one — [`rekey_spawned`] re-keys
    /// this table with the spawned table). Sender side: the ADOPTED
    /// future-QUIC dial socket ([`bind_sender_socket`]'s outcome), read
    /// by the send op so the probe round AND every dial attempt use the
    /// mapping the same-socket STUN query opened. Receiver side: a
    /// send-only `try_clone` of the QUIC listener socket, read by the
    /// punch spawn so the receiver's own NAT admits the sender's QUIC
    /// Initial ([`punch_candidates`]). Entries are removed on disconnect
    /// / cancel; teardown drops the runtime (and the sockets) wholesale.
    hole_sockets: StdMutex<HashMap<[u8; 16], HoleSocket>>,
    /// Lock order: lifecycle -> request_ids / conns / op outcome / holes.
    /// No reverse nesting and no await while holding this gate. Admission,
    /// Finished publication + lease release, expiry and terminal close all
    /// serialize here; UDP resources are optional, lifecycle state is not.
    sender_lifetimes: StdMutex<HashMap<[u8; 16], SenderLifetime>>,
    executor: tokio::runtime::Handle,
}

/// CR-P1-05: one code's hole socket plus the shared early-stop flag of
/// its punch loop (the accept path sets the flag on the FIRST successful
/// data-plane connection — see [`punch_candidates`]; a sender-side entry
/// simply never has its flag set).
pub(crate) struct HoleSocket {
    /// Sender: the adopted dial socket. Receiver: the send-only clone of
    /// the QUIC listener socket.
    pub(crate) socket: std::net::UdpSocket,
    /// Early-stop of the receiver punch loop (see the field docs above).
    pub(crate) punch_stop: Arc<AtomicBool>,
}

impl TaskRt {
    /// Teardown (engine drop): invalidate waiters FIRST — every pending
    /// consent is resolved with an explicit decline by DROPPING its
    /// deciding half (the parked engine receive fails closed on the
    /// vanished oneshot, wave-29 P2-07: no worker is blocked, so there is
    /// no condvar to wake). Order matters for the same reason the
    /// wave-26 P1-05 Dart sequence does: waiters, then owners.
    pub(crate) fn teardown(&self, runtime: &tokio::runtime::Runtime) {
        // CR-P2-05: silence the doorbell FIRST — a runtime being torn
        // down (engine drop) must not fire the embedder's callback from
        // inside `ak_engine_free`, and the decline/closure events queued
        // below are for the poll channel only.
        if let Ok(mut slot) = self.notify.lock() {
            *slot = None;
        }
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
        if let Ok(mut lifetimes) = self.sender_lifetimes.lock() {
            for life in lifetimes.values_mut() {
                life.close();
            }
        }
        if let Ok(mut holes) = self.hole_sockets.lock() {
            for (_, socket) in holes.drain() {
                socket.punch_stop.store(true, Ordering::Relaxed);
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
            // Batch G6: release the identity's control-connection lock
            // only now — AFTER the client closed. In-flight op bodies can
            // still hold `Arc<TaskStack>` clones, so the lease is taken
            // explicitly instead of waiting for the last Arc to drop; its
            // Drop aborts the heartbeat first, then removes the lock file
            // (a fresh runtime or a CLI task run can claim the identity
            // immediately after).
            if let Some(lease) = stack
                .connection_lock
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            {
                drop(lease);
            }
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
        let queued = if let Ok(mut queue) = self.events.lock() {
            if queue.len() < MAX_QUEUED_EVENTS {
                queue.push_back(line);
                true
            } else {
                false
            }
        } else {
            false
        };
        // CR-P2-05: ring the doorbell only when a line actually landed —
        // a queue at its cap (or a poisoned lock) has nothing to drain,
        // and a dropped event must not wake the embedder for nothing.
        // The `-3` pushback path (`task_push_event_front`) never rings:
        // the event was already announced when it was first queued and
        // the poller is actively draining it.
        if queued {
            self.ring_event_notify();
        }
    }

    /// CR-P2-05: invoke the registered doorbell — the WHOLE native-push
    /// mechanism. Fire-and-forget under the slot lock ([`TaskEventNotify`]
    /// documents why the lock is held across the call and why the wrapper
    /// can neither panic nor unwind); no fire happens when nothing is
    /// registered, which keeps the pure-polling behavior bit-identical
    /// for embedders that never call `ak_task_set_event_notify`.
    fn ring_event_notify(&self) {
        if let Ok(slot) = self.notify.lock() {
            if let Some(notify) = slot.as_ref() {
                (notify.func)(notify.ctx_addr as *mut std::os::raw::c_void);
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
        let mut lifetimes = self.sender_lifetimes.lock().ok()?;
        self.close_connection_locked(&mut lifetimes, conn_id, reason)
            .map(|conn| conn.conn_id)
    }

    fn close_connection_locked(
        &self,
        lifetimes: &mut HashMap<[u8; 16], SenderLifetime>,
        conn_id: &[u8; 16],
        reason: &str,
    ) -> Option<TaskConn> {
        let conn = {
            let mut conns = self.conns.lock().ok()?;
            let conn = conns.get_mut(conn_id)?;
            if conn.state == ConnState::Disconnected {
                return None;
            }
            conn.state = ConnState::Disconnected;
            conn.clone()
        };
        if let Some(life) = lifetimes.get_mut(conn_id) {
            life.close();
        }
        drop_hole_socket(self, &conn.code_id);
        self.push_event(format!(
            "{{\"event\":\"disconnected\",\"conn\":\"{}\",\"reason\":{}}}",
            core::hex_encode(conn_id),
            quoted(reason),
        ));
        Some(conn)
    }

    /// Called only under the lifecycle gate. A deadline is a local
    /// policy limit, never evidence that an active QUIC path died.
    fn reconcile_sender_locked(
        &self,
        lifetimes: &mut HashMap<[u8; 16], SenderLifetime>,
        conn_id: &[u8; 16],
    ) -> Option<TaskConn> {
        let life = lifetimes.get_mut(conn_id)?;
        if life.terminal || !life.active.is_empty() {
            return None;
        }
        if tokio::time::Instant::now() >= life.deadline {
            return self.close_connection_locked(lifetimes, conn_id, "idle_expired");
        }
        if *life.state.borrow() != SenderKeepaliveState::Idle {
            life.state.send_replace(SenderKeepaliveState::Idle);
        }
        None
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
///
/// CR-P3-02 (review 2026-11-11): on a Windows receiver the attribution
/// is followed by a name preflight ([`windows_root_name_refusal`]) — an
/// offered root name that cannot exist on this filesystem is refused
/// BEFORE the request is registered, so no banner surfaces, no consent
/// is ever asked and nothing is written; the peer hears the standard
/// DECLINED.
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
        // CR-P3-02 (review 2026-11-11): a Windows receiver cannot create
        // files whose names carry `< > " | ? *` or use the extended
        // reserved device stems. The platform-neutral path validation
        // deliberately allows them (a Linux→Linux transfer is legal), so
        // the refusal is made per local platform exactly HERE — before
        // the request is registered (no `incoming_request` banner, no
        // pending entry) and before the engine could answer Accept. The
        // peer is told the standard DECLINED, nothing is ever written to
        // disk, and the refusal surfaces as its own bounded event so the
        // reason is readable instead of an I/O error at file-creation
        // time. Emitted while holding the pending lock, like the
        // `incoming_request` line below, keeping every consent-path
        // publication totally ordered by the one lock.
        if let Some(reason) = windows_root_name_refusal(&info.root_name) {
            rt.push_event(format!(
                "{{\"event\":\"offer_refused\",\"conn\":\"{}\",\"root\":{},\"reason\":{}}}",
                core::hex_encode(&conn.conn_id),
                quoted(&info.root_name),
                quoted(&reason),
            ));
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
        // E2/G4 (review 2026-11-12 §6): the event carries the window it
        // was registered with (`windowSecs`) so the UI countdown mirrors
        // the NATIVE deadline — the fact source per ffi-v2 §8.3 — instead
        // of hard-coding a guess that would drift on the next change.
        line.push_str(&format!(",\"windowSecs\":{}", window.as_secs()));
        line.push('}');
        rt.push_event(line);
    }
    // The 120 s auto-decline (E2/G4): a timer task, never a blocked thread.
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
// Tests: every ak_task_* export, one positive + one negative minimum,
// driven through the C ABI boundary like tests/ffi_pairing.rs (but inside
// src/ — the wave-28 file-ownership boundary). The op/event reads go
// through the SHARED ak_pairing_op_result / ak_pairing_poll_event
// exports on purpose: that sharing is the contract.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod task_tests;
