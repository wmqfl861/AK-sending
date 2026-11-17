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
    // CR-P1-02 改法第 2 条 ("STUN 地址与注册中心一起配置"): the optional
    // STUN endpoint the receiver's socket-reflexive query dials. The Rust
    // layer stays DEFAULT-FREE on purpose — the registry endpoint itself
    // is resolved by the Dart layer (env / --dart-define) and passed in
    // the spec, and the STUN address rides the exact same resolution
    // path (contract ffi-v2 §8.1); an absent/empty value means NO query
    // (fail-open: derived local candidates still advertise, and
    // same-subnet/LAN joins are unaffected).
    let stun_addr = match field_str(&args, "stun_addr", false)? {
        Some(text) if !text.trim().is_empty() => {
            Some(text.trim().parse::<SocketAddr>().map_err(|_| {
                FfiError::invalid(format!(
                    "field \"stun_addr\" is not an ip:port address: {text:?}"
                ))
            })?)
        }
        _ => None,
    };
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
        stun_addr,
        save_dir: StdMutex::new(PathBuf::from(save_dir_text)),
        ops: StdMutex::new(HashMap::new()),
        events: Arc::new(StdMutex::new(VecDeque::new())),
        conns: StdMutex::new(HashMap::new()),
        pending: StdMutex::new(HashMap::new()),
        spawned: StdMutex::new(HashMap::new()),
        echo_observed: Arc::new(StdMutex::new(None)),
        stopped: StdMutex::new(false),
        request_ids: StdMutex::new(HashMap::new()),
        stack: StdMutex::new(None),
        stack_init: tokio::sync::Mutex::new(()),
        conn_ops: StdMutex::new(HashMap::new()),
        codes,
        engine_progress,
        notify: StdMutex::new(None),
        hole_sockets: StdMutex::new(HashMap::new()),
        sender_lifetimes: StdMutex::new(HashMap::new()),
        executor: engine.runtime().handle().clone(),
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

// CR-P2-11: candidate enumeration/ordering, reflexive insertion and the
// observed-dial-order helpers (local_candidates, filter_and_order_,
// insert_reflexive_, order_candidates, observed_dial_head,
// observed_first_order) moved to ak-session `pairing::task_net`.

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
    // Batch G6 (R36-P2-04 client half): claim the identity's
    // control-connection lock BEFORE the registry dial — the registry
    // replaces older same-device connections, so without this a CLI task
    // run or a daemon on the same id-dir (or a second task runtime in
    // this same engine) would silently kick this stack's connection and
    // be kicked back by its keeper forever. The lease (guard + heartbeat)
    // releases itself on every failure path below and otherwise moves
    // into the stack, holding until teardown.
    let connection_lock =
        ak_session::pairing::ConnectionLockLease::acquire(&rt.id_dir).map_err(|error| {
            format!(
                "cannot claim the identity's control-connection lock under {}: {error}",
                rt.id_dir.display()
            )
        })?;
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
        connection_lock: StdMutex::new(Some(connection_lock)),
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
        let (keeper_stop_tx, keeper_stop_rx) = tokio::sync::watch::channel(false);
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
        let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
            return;
        };
        if role == TaskRole::Sender {
            lifetimes.entry(conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
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
    if let Some(conn) = rt.conn_of_code(&code_id) {
        ensure_sender_lifetime(rt, &conn);
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
///
/// CR-P1-07 (review 2026-11-13 §4.4 改法 1/2): the sender role now runs
/// its PAIRING-TIME opener + mapping keepalive here (the same shared
/// `task_net::sender_pairing_keeper` the CLI runs — identical timing), and
/// its connectivity test rides the ADOPTED dial socket
/// ([`adopted_socket_probe`]) instead of a throwaway socket. A receiver
/// owner keeps the CR-P1-05 punch plus the pairing-plane probe: its QUIC
/// socket is already owned by quinn, so a receive round on a clone would
/// compete for the socket queue.
async fn owner_side_probe(rt: &Arc<TaskRt>, stack: &Arc<TaskStack>, paired: TaskPaired) {
    let conn_id = paired.pairing_id;
    let role = TaskRole::of_direction(paired.direction);
    // CR-P1-05, receiver half (the create-code entry point): the pairing
    // just completed and the SENDER's advertised candidates are known —
    // punch them from the resident receiver's QUIC-socket clone,
    // concurrently with the sender's dial. Owners of sender-role codes
    // run their own opener instead (their peer is the receiver).
    match role {
        TaskRole::Receiver => {
            spawn_receiver_punch(rt, paired.code_id, &paired.peer.candidates);
        }
        TaskRole::Sender => {
            spawn_sender_opener(rt, paired.code_id, &paired.peer.candidates);
        }
    }
    let outcome = if role == TaskRole::Sender {
        adopted_socket_probe(rt, &paired.code_id, &paired.peer.candidates).await
    } else {
        let endpoint_probe = PeerEndpoint {
            candidates: paired.peer.candidates.clone(),
            name: paired.peer.name.clone(),
            direction: paired.peer.direction,
        };
        probe_with_retry(&stack.pairing, &endpoint_probe).await
    };
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

/// Render the `probe_observed` event line of one send-time probe round
/// (batchF G2 / CR-P3-06 diagnostics; contract ffi-v2 §8.3, prose
/// registration beside `sender_bind`): exactly ONE bounded line per
/// round — the observed peer address, its candidate kind and the dial
/// phase — whether or not anything was observed (the fail-open shape
/// mirrors the round itself). `kind` follows
/// [`task_net::observed_dial_head`]'s two rules: a head inside
/// `report.reachable` is an ADVERTISED candidate the round
/// echo-confirmed (`candidate`); any other head is the raw source of a
/// PEER probe from outside the candidate list (`observed`); no head at
/// all is `none`.
fn probe_observed_event(
    conn_id: &[u8; 16],
    head: Option<SocketAddr>,
    report: &ak_net::holepunch::PunchReport,
) -> String {
    let kind = match head {
        Some(head) if report.reachable.contains(&head) => "candidate",
        Some(_) => "observed",
        None => "none",
    };
    format!(
        "{{\"event\":\"probe_observed\",\"conn\":\"{}\",\"observed\":{},\"kind\":\"{kind}\",\"phase\":\"send\",\"reachable\":{},\"packets_in\":{},\"elapsed_ms\":{}}}",
        core::hex_encode(conn_id),
        head.map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
        report.reachable.len(),
        report.packets_received,
        report.elapsed.as_millis(),
    )
}

/// Bind the probe echo responder (the shared loop lives in
/// `task_net::echo_responder_loop`): a small UDP socket that answers
/// ak-net hole-punch probe packets from any of this device's advertised
/// addresses, so the peer's `probe_peer` genuinely confirms two-way
/// reachability (the QUIC receiver itself never speaks the probe
/// exchange). Registered under `key` in the spawned table (teardown /
/// disconnect abort it).
///
/// Since CR-P1-02 改法第 3 条 the responder also OBSERVES: every genuine
/// probe's source lands in [`TaskRt::echo_observed`] (most-recent, one
/// slot, normalized to plain-family form) — the receiver side's raw
/// hole-punch observation, consumed by [`close_notify_peer`] for its
/// one outbound dial role. The QUIC accept itself is already bound to
/// the receiver's own socket and needs no dial, so the observation is
/// deliberately NOT plumbed into the accept path — see the struct's
/// honest-scoping note.
fn spawn_echo_responder(rt: &Arc<TaskRt>, key: [u8; 16]) -> Result<SocketAddr, String> {
    let (socket, addr) = ak_session::pairing::task_net::bind_echo_responder_socket()?;
    let observed_slot = Arc::clone(&rt.echo_observed);
    let task = tokio::spawn(async move {
        // The FFI side reports nothing per observation (the event set is
        // contract-frozen) — the observation slot is the whole surface.
        ak_session::pairing::task_net::echo_responder_loop(socket, observed_slot, |_| {}).await;
    });
    if let Ok(mut spawned) = rt.spawned.lock() {
        spawned.entry(key).or_default().push(task);
    }
    Ok(addr)
}

/// Abort and forget the resident tasks registered under `from` and
/// re-register them under the registry-assigned `code_id` (the bind
/// happens before the claim, under a local placeholder id). CR-P1-05:
/// the code's hole socket (stashed under the placeholder by the resident
/// receiver) re-keys with them.
fn rekey_spawned(rt: &TaskRt, from: [u8; 16], code_id: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&from) {
            spawned.entry(code_id).or_default().extend(tasks);
        }
    }
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        if let Some(socket) = holes.remove(&from) {
            holes.insert(code_id, socket);
        }
    }
}

/// Abort the resident tasks registered under a placeholder id (the op
/// failed after binding — nothing may leak past the failed op). CR-P1-05:
/// the placeholder's hole socket goes with them (its punch stop flips so
/// a spawned punch loop ends too).
fn abort_spawned(rt: &TaskRt, key: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&key) {
            for task in tasks {
                task.abort();
            }
        }
    }
    drop_hole_socket(rt, &key);
}

/// Release UDP resources without owning the pairing's policy deadline.
fn drop_hole_socket(rt: &TaskRt, code_id: &[u8; 16]) {
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        if let Some(socket) = holes.remove(code_id) {
            socket.punch_stop.store(true, Ordering::Relaxed);
        }
    }
}

/// CR-P1-05: stash one code's hole socket (sender: the adopted dial
/// socket; receiver: the send-only QUIC-socket clone).
fn stash_hole_socket(rt: &TaskRt, code_id: [u8; 16], socket: std::net::UdpSocket) {
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        holes.insert(
            code_id,
            HoleSocket {
                socket,
                punch_stop: Arc::new(AtomicBool::new(false)),
            },
        );
    }
}

/// CR-P1-05, receiver half: spawn the punch loop toward the paired
/// SENDER's advertised candidates from the receiver's stashed QUIC-socket
/// clone. Called the moment the pairing completes (both entry points —
/// the owner-loop completion and the joiner's redeem return) so the
/// punches overlap the sender's dial. No clone stashed (e.g. an older
/// receiver bind path) is an honest no-op; the loop itself is bounded by
/// [`PUNCH_WINDOW`] and stopped early by the accept path's flag.
fn spawn_receiver_punch(rt: &Arc<TaskRt>, code_id: [u8; 16], sender_candidates: &[SocketAddr]) {
    let (socket, stop) = {
        let Ok(holes) = rt.hole_sockets.lock() else {
            return;
        };
        let Some(entry) = holes.get(&code_id) else {
            return;
        };
        match entry.socket.try_clone() {
            Ok(clone) => (clone, Arc::clone(&entry.punch_stop)),
            Err(_) => return,
        }
    };
    let candidates = sender_candidates.to_vec();
    tokio::spawn(async move {
        ak_session::pairing::task_net::punch_candidates(socket, &candidates, &stop, |_| {}).await;
    });
}

/// One lifetime per paired sender, including degraded/no-socket pairings.
struct SenderLifetime {
    deadline: tokio::time::Instant,
    active: HashSet<[u8; 16]>,
    terminal: bool,
    state: tokio::sync::watch::Sender<SenderKeepaliveState>,
    keeper: Option<JoinHandle<()>>,
}

impl SenderLifetime {
    fn new(deadline: tokio::time::Instant) -> Self {
        let (state, _) = tokio::sync::watch::channel(SenderKeepaliveState::Idle);
        Self {
            deadline,
            active: HashSet::new(),
            terminal: false,
            state,
            keeper: None,
        }
    }

    fn admit(&mut self, op_id: [u8; 16]) {
        self.active.insert(op_id);
        self.state.send_replace(SenderKeepaliveState::Active);
    }

    fn close(&mut self) {
        self.terminal = true;
        self.state.send_replace(SenderKeepaliveState::Closed);
        if let Some(task) = self.keeper.take() {
            task.abort();
        }
    }
}

impl Drop for SenderLifetime {
    fn drop(&mut self) {
        self.close();
    }
}

/// Install the deadline before any send can be admitted. Also called by
/// the keeper hook, which is idempotent (no second opener on catch-up).
fn ensure_sender_lifetime(rt: &TaskRt, conn: &TaskConn) {
    if conn.role == TaskRole::Sender {
        if let Ok(mut lifetimes) = rt.sender_lifetimes.lock() {
            lifetimes.entry(conn.conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
    }
}

fn spawn_sender_opener(rt: &Arc<TaskRt>, code_id: [u8; 16], receiver_candidates: &[SocketAddr]) {
    let Some(conn) = rt.conn_of_code(&code_id) else {
        return;
    };
    ensure_sender_lifetime(rt, &conn);
    let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
        return;
    };
    let Some(life) = lifetimes.get_mut(&conn.conn_id) else {
        return;
    };
    if life.terminal || life.keeper.is_some() || rt.is_stopped() {
        return;
    }
    let socket = rt.hole_sockets.lock().ok().and_then(|holes| {
        holes
            .get(&code_id)
            .and_then(|hole| hole.socket.try_clone().ok())
    });
    let deadline = life.deadline;
    let state = life.state.subscribe();
    let candidates = order_candidates(receiver_candidates.to_vec());
    // The resident task never strongly owns TaskRt; dropping the runtime
    // closes/aborts the keeper and releases its socket without an Arc ring.
    let opener_rt = Arc::downgrade(rt);
    let expiry_rt = Arc::downgrade(rt);
    let conn_id = conn.conn_id;
    life.keeper = Some(rt.executor.spawn(async move {
        sender_pairing_keeper(
            socket.as_ref(),
            &candidates,
            deadline,
            state,
            |packets| {
                if let Some(rt) = opener_rt.upgrade() {
                    if !rt.is_stopped() {
                        rt.push_event(format!(
                            "{{\"event\":\"reflexive_opener\",\"packets\":{packets}}}"
                        ));
                    }
                }
            },
            || {
                if let Some(rt) = expiry_rt.upgrade() {
                    expire_idle_sender(&rt, &conn_id);
                }
            },
        )
        .await;
    }));
}

fn expire_idle_sender(rt: &Arc<TaskRt>, conn_id: &[u8; 16]) {
    let expired = {
        let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
            return;
        };
        rt.reconcile_sender_locked(&mut lifetimes, conn_id)
    };
    finish_idle_expiry(rt, expired);
}

/// Terminal state/event were decided under the gate. Network cleanup is
/// outside it; idle expiry never aborts a live send because active is empty.
fn finish_idle_expiry(rt: &Arc<TaskRt>, expired: Option<TaskConn>) {
    let Some(conn) = expired else {
        return;
    };
    rt.decline_pending_of_conn(&conn.conn_id);
    if let Ok(table) = rt.codes.lock() {
        if let Some(entry) = table.values().find(|entry| entry.code_id == conn.code_id) {
            let peer = if entry.owner.runtime_id == rt.runtime_id {
                entry.joiner.clone()
            } else {
                Some(Arc::clone(&entry.owner))
            };
            if let Some(peer) = peer {
                peer.push_disconnected_once(&conn.conn_id, "peer");
            }
        }
    }
    if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
        rt.executor.spawn(async move {
            let _ = stack.pairing.disconnect(conn.code_id).await;
        });
    }
}

/// Owns exactly one admission. Constructed before spawn so cancellation
/// before first poll also releases it. The awaited send future is created
/// after this guard enters the task, and is dropped before the guard.
struct SendLease {
    rt: Weak<TaskRt>,
    conn_id: [u8; 16],
    op_id: [u8; 16],
    entry: Arc<OpEntry>,
    settled: bool,
}

impl SendLease {
    fn finish(&mut self, result: Option<(bool, String)>) {
        if self.settled {
            return;
        }
        self.settled = true;
        let Some(rt) = self.rt.upgrade() else {
            return;
        };
        let expired = {
            let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
                return;
            };
            // Publication precedes releasing admission, in the same
            // critical section. Duplicate/cancelled settlement is silent.
            if let Ok(mut outcome) = self.entry.outcome.lock() {
                if matches!(&*outcome, OpOutcome::Running { .. }) {
                    let (ok, json) = result.unwrap_or_else(|| {
                        (
                            false,
                            "{\"op\":\"task_send\",\"error\":\"cancelled by caller\"}".to_string(),
                        )
                    });
                    *outcome = OpOutcome::Finished { ok, json };
                    rt.push_event(format!(
                        "{{\"event\":\"{}\",\"op_id\":\"{}\",\"op\":\"task_send\"}}",
                        if ok { "op_done" } else { "op_failed" },
                        core::hex_encode(&self.op_id),
                    ));
                }
            }
            let removed = lifetimes
                .get_mut(&self.conn_id)
                .is_some_and(|life| life.active.remove(&self.op_id));
            if removed {
                rt.reconcile_sender_locked(&mut lifetimes, &self.conn_id)
            } else {
                None
            }
        };
        finish_idle_expiry(&rt, expired);
    }
}

impl Drop for SendLease {
    fn drop(&mut self) {
        self.finish(None);
    }
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 2): the SENDER's pairing-time
/// connectivity test on the ADOPTED dial socket — one
/// [`probe_dial_round_on`] round over a `try_clone` of the stashed socket
/// (the exact round the send phase runs), folded into the pairing plane's
/// [`ProbeOutcome`] vocabulary. The legacy throwaway-socket test
/// (`probe_with_retry`) could STRUCTURALLY never pass through a
/// port-restricted-cone NAT — its mapping was not the QUIC socket's — and
/// its false `unreachable` used to gate the GUI's send button; on the
/// adopted socket the round observes the peer's own punch packets
/// arriving on the real mapping, so the verdict is meaningful. The
/// [`SENDER_DIAL_PAUSE`] wait keeps the opener's packets the adopted
/// socket's FIRST outbound (the CR-P1-06 ordering); without a stash (bind
/// failed at create/join) the throwaway round stays the honest fallback.
async fn adopted_socket_probe(
    rt: &TaskRt,
    code_id: &[u8; 16],
    candidates: &[SocketAddr],
) -> ProbeOutcome {
    let ordered = order_candidates(candidates.to_vec());
    tokio::time::sleep(SENDER_DIAL_PAUSE).await;
    let adopted = rt
        .hole_sockets
        .lock()
        .ok()
        .and_then(|holes| holes.get(code_id).and_then(|e| e.socket.try_clone().ok()));
    let plan = match adopted {
        Some(socket) => {
            probe_dial_round_on(socket, ordered.len(), ordered.clone(), |_, _, _| {}).await
        }
        None => probe_dial_round(ordered.clone(), |_, _, _| {}).await,
    };
    round_probe_outcome(&ordered, &plan)
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
    /// G6 follow-through (batch G6b, 2026-11-13): ask the registry for
    /// its build revision — the GUI's "关于" fact. A DIAGNOSTIC query,
    /// never a transfer: no idempotency key of its own (the Rust side
    /// caches per connection epoch and keeps one query in flight, so
    /// repeats are cheap), and `None` answers as `"revision":null`
    /// instead of an error (the `RegistryClient::server_info`
    /// degrade-to-None contract rides unchanged).
    ServerInfo,
}

impl TaskOpSpec {
    fn op(&self) -> &'static str {
        match self {
            TaskOpSpec::CreateCode { .. } => "task_create_code",
            TaskOpSpec::Join { .. } => "task_join",
            TaskOpSpec::Send { .. } => "task_send",
            TaskOpSpec::ServerInfo => "task_server_info",
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
    lifetime: Option<&SenderLifetime>,
) -> Result<RequestGate, FfiError> {
    let Some(request_text) = field_str(args, "request_id", false)? else {
        if enforce_live_conn {
            // No dedupe key: still refuse a send on a severed connection
            // before any op exists.
            ensure_conn_live(rt, &conn.expect("send gate carries a conn id"))?;
            if rt.is_stopped()
                || lifetime.is_some_and(|life| {
                    life.terminal || tokio::time::Instant::now() >= life.deadline
                })
            {
                return Err(FfiError::invalid(
                    "the pairing idle budget expired or runtime stopped",
                ));
            }
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
                if rt.is_stopped()
                    || lifetime.is_some_and(|life| {
                        life.terminal || tokio::time::Instant::now() >= life.deadline
                    })
                {
                    return Err(FfiError::invalid(
                        "the pairing idle budget expired or runtime stopped",
                    ));
                }
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
    launch_task_op_inner(engine, rt, spec, reserved, None)
}

fn launch_task_op_inner(
    engine: &AkEngine,
    rt: &Arc<TaskRt>,
    spec: TaskOpSpec,
    reserved: Option<ReservedOp>,
    lifetimes: Option<&mut HashMap<[u8; 16], SenderLifetime>>,
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
    let lease = if let TaskOpSpec::Send { conn_id, .. } = &spec {
        let lifetimes = lifetimes.expect("send launcher owns the admission gate");
        if let Some(life) = lifetimes.get_mut(conn_id) {
            life.admit(op_id);
        }
        rt.track_conn_op(*conn_id, op_id);
        Some(SendLease {
            rt: Arc::downgrade(rt),
            conn_id: *conn_id,
            op_id,
            entry: Arc::clone(&entry),
            settled: false,
        })
    } else {
        None
    };
    let task = {
        let entry = Arc::clone(&entry);
        let rt = Arc::clone(rt);
        engine.runtime().spawn(async move {
            let mut lease = lease;
            let (ok, json) = match run_task_op(&rt, spec).await {
                Ok(json) => (true, json),
                Err(message) => (
                    false,
                    format!("{{\"op\":{},\"error\":{}}}", quoted(op), quoted(&message)),
                ),
            };
            if let Some(lease) = &mut lease {
                lease.finish(Some((ok, json)));
                return;
            }
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
    let reserved = match gate_request(&rt, "task_create_code", None, false, &args, None)? {
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
    let reserved = match gate_request(&rt, "task_join", None, false, &args, None)? {
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
    let mut lifetimes = rt
        .sender_lifetimes
        .lock()
        .map_err(|_| FfiError::internal("sender lifecycle lock poisoned"))?;
    let expired = rt.reconcile_sender_locked(&mut lifetimes, conn_id);
    // The entire acceptance (including task registration) is protected
    // against expiry/close. Replays never acquire another send lease.
    let result = (|| {
        let reserved = match gate_request(
            &rt,
            "task_send",
            Some(*conn_id),
            true,
            &args,
            lifetimes.get(conn_id),
        )? {
            RequestGate::Replay(op_id) => return Ok(op_id),
            RequestGate::Fresh(reserved) => Some(reserved),
            RequestGate::NoId => None,
        };
        launch_task_op_inner(
            engine,
            &rt,
            TaskOpSpec::Send {
                conn_id: *conn_id,
                paths,
            },
            reserved,
            Some(&mut lifetimes),
        )
    })();
    drop(lifetimes);
    finish_idle_expiry(&rt, expired);
    result
}

/// `ak_task_server_info` (G6 follow-through, batch G6b 2026-11-13): accept
/// one registry-revision query on the task runtime — the GUI's「关于」fact.
/// No spec, no idempotency key: the Rust side gates on the server's
/// advertised capability bit, keeps one query in flight and caches per
/// connection epoch, so repeated calls are cheap. The op resolves OK with
/// `{"op":"task_server_info","revision":"<build>"}` or
/// `"revision":null` (old server / failed query / degraded — NEVER an
/// error); it only fails when the runtime cannot reach its registry stack
/// at all (not started / stopped / dial failed).
pub fn task_server_info_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    launch_task_op(engine, &rt, TaskOpSpec::ServerInfo, None)
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
    let Some(conn) = rt
        .conns
        .lock()
        .ok()
        .and_then(|conns| conns.get(conn_id).cloned())
    else {
        return cancel_unjoined_code(engine, &rt, conn_id);
    };
    if rt.push_disconnected_once(conn_id, "local").is_none() {
        return Ok(());
    }
    let code_id = conn.code_id;
    // Wake this connection's parked consents (P2-07: a disconnect must
    // resolve its pending offers, not wait out the 120 s window).
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
    // CR-P1-05: release the code's hole socket (the sender's adopted
    // dial socket / the receiver's punch clone) and stop its punch loop.
    drop_hole_socket(&rt, &code_id);
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
    // CR-P1-05: the cancelled code's hole socket goes with the listeners.
    drop_hole_socket(rt, code_id);
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

/// The authenticated idle-close notification (wave-29 P2-03), the FFI
/// adapter: hand the shared `task_net::deliver_close_notify` core this
/// runtime's key, the connection's advertised candidates, the verified
/// peer fingerprint and the echo responder's most recent observed probe
/// source (CR-P1-02 改法第 3 条 — the observation leads the dial order;
/// see the shared core for the full rationale).
async fn close_notify_peer(rt: &Arc<TaskRt>, conn: &TaskConn) {
    let Some(pinned) = conn.peer_fp else {
        return;
    };
    let observed = rt
        .echo_observed
        .lock()
        .map(|slot| *slot)
        .unwrap_or_else(|poisoned| *poisoned.into_inner());
    deliver_close_notify(&rt.key, conn.candidates.clone(), pinned, observed).await;
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
/// (the entry is gone the moment it was decided, auto-declined at the 120 s
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
        TaskOpSpec::ServerInfo => run_server_info(rt).await,
    }
}

/// The `task_server_info` op body (G6 follow-through, batch G6b): ask the
/// runtime's shared registry client for the serving registry's build
/// revision (protocol-v2.md §6a). The whole G6 degrade contract rides
/// unchanged: an OLD server (no capability bit) is never asked, a failed
/// query never faults the control connection, and both surface as
/// `"revision":null` — the op itself only FAILS when the registry stack
/// could not be established at all (not started / stopped / dial failed),
/// exactly like every other op's `ensure_stack` gate.
async fn run_server_info(rt: &Arc<TaskRt>) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let revision = stack.pairing.orchestrator().client().server_info().await;
    Ok(format!(
        "{{\"op\":\"task_server_info\",\"revision\":{}}}",
        revision
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
    ))
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
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): a
    // sender-role code binds THE future-QUIC dial socket now — before
    // the claim, because the registered blob carries the reflexive
    // address — STUN-queries ON IT (same fail-open 2 s budget), and the
    // socket is stashed for the send op (probe round + every dial
    // attempt leave from this port). Without a STUN server the behavior
    // is byte-identical to the old port-0 advertisement plus a bound
    // socket for the dial.
    let mut sender_sock: Option<(std::net::UdpSocket, Option<SocketAddr>)> = None;
    if role == TaskRole::Sender {
        match bind_sender_socket(rt.stun_addr).await {
            Ok(sock) => {
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":{},\"dual_stack\":{},\"fallback_reason\":{},\"reflexive\":{},\"stun_error\":{}}}",
                    quoted(&sock.socket.local_addr().map(|addr| addr.to_string()).unwrap_or_default()),
                    sock.dual_stack,
                    sock.bind_fallback.as_deref().map_or_else(|| "null".to_string(), quoted),
                    sock.reflexive.as_ref().map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
                    sock.stun_error.as_deref().map_or_else(|| "null".to_string(), quoted),
                ));
                sender_sock = Some((sock.socket, sock.reflexive));
            }
            Err(message) => {
                // Fail-open: the send path falls back to the engine's
                // own bind (the old behavior) — the bounded reason rides
                // the sender_bind event's shape.
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":\"null\",\"dual_stack\":false,\"fallback_reason\":{},\"reflexive\":\"null\",\"stun_error\":\"null\"}}",
                    quoted(&message),
                ));
            }
        }
    }
    if !explicit {
        let derived = if role == TaskRole::Receiver {
            let ((bound, reflexive), slot) = match bind_resident_receiver(rt, placeholder).await {
                Ok(bound) => bound,
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            };
            code_slot = Some(slot);
            // CR-P1-02 改法第 2 条: the STUN server-reflexive address of
            // the receiver's own socket rides the derived list (inserted
            // right after the v4 interface block — see the helper).
            insert_reflexive_candidate(local_candidates(bound), reflexive)
        } else {
            // CR-P1-05: the sender-role blob carries the DIAL socket's
            // real port (the paired receiver's punch loop needs a
            // dialable target — port-0 "no listener" markers defeat it)
            // plus the socket's reflexive address when STUN answered.
            let (socket, reflexive) = sender_sock
                .as_ref()
                .map(|(socket, reflexive)| {
                    (
                        socket
                            .local_addr()
                            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0))),
                        *reflexive,
                    )
                })
                .unwrap_or((SocketAddr::from(([0, 0, 0, 0], 0)), None));
            insert_reflexive_candidate(local_candidates(socket), reflexive)
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
            task_error_message(&error)
        })?;
    rekey_spawned(rt, placeholder, code.invite.code_id);
    // CR-P1-05: the sender's adopted dial socket lands under the REAL
    // code id (the send op looks it up by the connection's code id).
    if let Some((socket, _)) = sender_sock {
        stash_hole_socket(rt, code.invite.code_id, socket);
    }
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
/// first bound address (plus the STUN server-reflexive address observed
/// on the same socket, 改法第 2 条 — `None` when no server was configured
/// or the query failed open). The task registers under `key` in the
/// spawned table (teardown / disconnect abort it); the returned slot
/// carries the registry code id to the receiver once it is known.
async fn bind_resident_receiver(
    rt: &Arc<TaskRt>,
    key: [u8; 16],
) -> Result<((SocketAddr, Option<SocketAddr>), ReceiverCodeSlot), String> {
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
        Ok(Ok(bound_and_reflexive)) => Ok((bound_and_reflexive, code_slot)),
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
    first_addr: oneshot::Sender<(SocketAddr, Option<SocketAddr>)>,
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
    // CR-P1-02 改法第 1 条 + 第 2 条: the socket is created explicitly —
    // dual-stack `[::]` first (C1; v4 wildcard fallback for hosts without
    // IPv6) — the optional STUN reflexive query runs ON IT (same socket,
    // so the NAT mapping survives), and only then is it handed to QUIC
    // via `bind_with_socket`. Every optional step fails open; the outcome
    // rides one bounded `receiver_bind` event (the reflexive address and
    // the bounded STUN failure reason extend C1's shape — the Dart pump
    // ignores unknown fields).
    let sock = match bind_receiver_socket(rt.stun_addr).await {
        Ok(sock) => sock,
        Err(_) => return, // the dropped oneshot fails the create/join op
    };
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 3 条): keep a SEND-ONLY
    // `try_clone` of the very socket QUIC is about to adopt (duplicate
    // handle, same local port) for the receiver-side hole punch — the
    // punch task spawned at pairing completion sends the peer-opening
    // packets FROM the QUIC socket's port, so a port-restricted-cone NAT
    // on this side admits the sender's QUIC Initial. The clone NEVER
    // receives (it would compete with quinn for the socket queue).
    // Stashed under the CURRENT code id — the placeholder until the
    // claim/redeem fills the slot; [`rekey_spawned`] moves it with the
    // resident tasks.
    let punch_clone = sock.socket.try_clone().ok();
    let endpoint = match ak_net::DirectEndpoint::bind_with_socket(sock.socket, &identity).await {
        Ok(endpoint) => endpoint,
        Err(_) => return, // the dropped oneshot fails the create/join op
    };
    if let Some(clone) = punch_clone {
        stash_hole_socket(&rt, current_code(), clone);
    }
    let bound = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let _ = first_addr.send((bound, sock.reflexive));
    rt.push_event(format!(
        "{{\"event\":\"receiver_bind\",\"addr\":{},\"dual_stack\":{},\"fallback_reason\":{},\"reflexive\":{},\"stun_error\":{}}}",
        quoted(&bound.to_string()),
        // Tracked at bind time: adopted-socket endpoints always report
        // is_dual_stack() == false (ak-net cannot portably re-query
        // V6ONLY) — see ReceiverSocket::dual_stack.
        sock.dual_stack,
        sock.bind_fallback
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
        sock.reflexive
            .as_ref()
            .map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
        sock.stun_error
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
    ));
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
        // Client UI U1 (ffi-v2 §8.4 U1 revision): the task plane renders
        // the SAME payload fields as the engine-level lines (`files`/
        // `bytes`/`file`/`chunk`/`path`/`verified`) — the GUI's live
        // progress needs the byte counts, not just the event name.
        core::push_event(&progress_queue, &job, None, &event);
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
        // CR-P1-05: the FIRST successful data-plane connection stops the
        // receiver punch loop early (the review's early-stop rule) —
        // from here on the QUIC connection itself keeps the mapping hot.
        if let Ok(holes) = rt.hole_sockets.lock() {
            if let Some(entry) = holes.get(&code_id) {
                entry.punch_stop.store(true, Ordering::Relaxed);
            }
        }
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
                // The user (or the 120 s window) declined: the pending
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
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): the
    // joining SENDER binds THE future-QUIC dial socket BEFORE the redeem
    // — the blob the redeem registers must carry the reflexive address
    // (the paired receiver's punch loop aims at it) and the socket's
    // real port. STUN runs ON IT under the same fail-open budget; the
    // socket is stashed under the code id once the redeem returns.
    let mut sender_sock: Option<(std::net::UdpSocket, Option<SocketAddr>)> = None;
    if my_role == TaskRole::Sender {
        match bind_sender_socket(rt.stun_addr).await {
            Ok(sock) => {
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":{},\"dual_stack\":{},\"fallback_reason\":{},\"reflexive\":{},\"stun_error\":{}}}",
                    quoted(&sock.socket.local_addr().map(|addr| addr.to_string()).unwrap_or_default()),
                    sock.dual_stack,
                    sock.bind_fallback.as_deref().map_or_else(|| "null".to_string(), quoted),
                    sock.reflexive.as_ref().map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
                    sock.stun_error.as_deref().map_or_else(|| "null".to_string(), quoted),
                ));
                sender_sock = Some((sock.socket, sock.reflexive));
            }
            Err(message) => {
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":\"null\",\"dual_stack\":false,\"fallback_reason\":{},\"reflexive\":\"null\",\"stun_error\":\"null\"}}",
                    quoted(&message),
                ));
            }
        }
    }
    if !explicit {
        dial = if my_role == TaskRole::Receiver {
            match bind_resident_receiver(rt, placeholder).await {
                Ok(((bound, reflexive), slot)) => {
                    code_slot = Some(slot);
                    // 改法第 2 条, the joiner-receiver mirror of the
                    // create path: the socket-reflexive address rides the
                    // derived candidates.
                    insert_reflexive_candidate(local_candidates(bound), reflexive)
                }
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            }
        } else {
            // CR-P1-05: the joining sender's blob carries the dial
            // socket's real port + reflexive address (same shape as the
            // create-side sender path above).
            let (socket, reflexive) = sender_sock
                .as_ref()
                .map(|(socket, reflexive)| {
                    (
                        socket
                            .local_addr()
                            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0))),
                        *reflexive,
                    )
                })
                .unwrap_or((SocketAddr::from(([0, 0, 0, 0], 0)), None));
            insert_reflexive_candidate(local_candidates(socket), reflexive)
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
            return Err(task_error_message(&error));
        }
    };
    let code_id = paired.code_id;
    let conn_id = paired.pairing_id;
    let dial_candidates = paired.peer.candidates.clone();
    let peer_fp = paired.verified.owner_fingerprint();
    rekey_spawned(rt, placeholder, code_id);
    // CR-P1-05: the joining sender's adopted dial socket lands under the
    // real code id (the send op adopts it); the joining RECEIVER punches
    // toward the paired sender's advertised candidates from its stashed
    // QUIC-socket clone — concurrent with the sender's dial, bounded by
    // PUNCH_WINDOW, stopped by the accept path's flag. CR-P1-07: the
    // joining SENDER runs its pairing-time opener + mapping keepalive now
    // (the same shared core as the CLI) instead of at send time — the
    // GUI's send may come arbitrarily late.
    if let Some((socket, _)) = sender_sock {
        stash_hole_socket(rt, code_id, socket);
    }
    if let Some(slot) = code_slot {
        if let Ok(mut value) = slot.lock() {
            *value = code_id;
        }
    }
    // Publish the connection and its policy deadline under the same gate.
    {
        let mut lifetimes = rt
            .sender_lifetimes
            .lock()
            .map_err(|_| "sender lifecycle lock poisoned".to_string())?;
        if my_role == TaskRole::Sender {
            lifetimes.entry(conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
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
    match my_role {
        TaskRole::Receiver => {
            spawn_receiver_punch(rt, code_id, &dial_candidates);
        }
        TaskRole::Sender => {
            spawn_sender_opener(rt, code_id, &dial_candidates);
        }
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
    // The REAL reachability probe; the settle is honest (P2-09):
    // all-failed leaves the entry `unreachable`, matching the join
    // result's `probe` verdict. CR-P1-07 改法 2: a SENDER's test rides the
    // ADOPTED dial socket (meaningful through a port-restricted-cone NAT,
    // and the GUI no longer treats its failure as terminal); a RECEIVER
    // keeps the pairing-plane probe (quinn owns its socket).
    let outcome = if my_role == TaskRole::Sender {
        adopted_socket_probe(rt, &code_id, &dial_candidates).await
    } else {
        probe_with_retry(&stack.pairing, &paired.peer).await
    };
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

/// One send op: run the shared candidate dial pass
/// (`task_net::staggered_dial_pass`, CR-P3-09: reflexive/observed
/// addresses strictly first, Happy-Eyeballs stagger on the self-bound
/// path, sequential on the adopted socket) under the split connect
/// budget; a post-connect failure fails the op (another address cannot
/// fix it); success emits `done` and completes the op. Purely
/// connect-level failure of the WHOLE list gets [`TASK_CONNECT_PASSES`]
/// passes (the just-paired peer's admission bookkeeping can trail the
/// joiner by a beat — see the constant). Since CR-P1-02 改法第 3 条 the
/// connect phase OPENS with one simultaneous-probe round
/// ([`probe_dial_round`]): when it observes an answering address, that
/// address leads the dial order under the bounded observed-candidate
/// budget; with no observation the round is invisible and the static
/// order dials exactly as before. Since CR-P1-07 the send-time
/// reflexive opener is GONE — the opener + mapping keepalive run at
/// PAIRING COMPLETION ([`spawn_sender_opener`]); the admission gate pauses
/// keeper packets while any send owns the connection.
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
    let static_order = order_candidates(conn.candidates.clone());
    if static_order.is_empty() {
        return Err(format!(
            "connection {} carries no dial candidates",
            core::hex_encode(&conn_id)
        ));
    }
    // CR-P1-05: when this sender stashed an adopted dial socket at
    // join/create time, the probe round runs ON a `try_clone` of IT (a
    // duplicate handle of the same OS socket — same local port, same NAT
    // mapping) and every dial attempt below adopts the same socket; the
    // review's core rule (审核改法第 4 条: the probing AND the QUIC dial
    // ride one socket). No stash (bind failed, or an old runtime): the
    // legacy throwaway round, byte-identical to the pre-CR path.
    //
    // Admission, not this attempt, owns keeper activity. No sender-side
    // stop flag or epoch lives in the socket stash.
    let adopted = rt
        .hole_sockets
        .lock()
        .ok()
        .and_then(|holes| holes.get(&conn.code_id)?.socket.try_clone().ok());
    // batchF G2: the round's observation rides one bounded
    // `probe_observed` event per send (the emission pattern of
    // `sender_bind` — a plain `push_event`, unknown-name-tolerated on
    // the Dart side); the round itself early-exits on the first
    // confirmed head since CR-P3-06 (task_net's probe_dial_round_on).
    let dial = match adopted.as_ref().map(|socket| socket.try_clone().ok()) {
        Some(Some(round_socket)) => {
            probe_dial_round_on(
                round_socket,
                static_order.len(),
                static_order,
                |head, report, _| {
                    rt.push_event(probe_observed_event(&conn_id, head, report));
                },
            )
            .await
        }
        _ => {
            probe_dial_round(static_order, |head, report, _| {
                rt.push_event(probe_observed_event(&conn_id, head, report));
            })
            .await
        }
    };
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
        // CR-P3-09: the per-candidate dial loop lives in the shared
        // `staggered_dial_pass` (reflexive/observed first, Happy-Eyeballs
        // stagger on the self-bound path, strictly sequential on the
        // adopted socket — concurrent QUIC endpoints on handles of one OS
        // socket would compete for its receive queue).
        let outcome = staggered_dial_pass(
            &dial.order,
            dial.head,
            adopted.is_some(),
            |addr, marks, budget| {
                attempt_task_send(
                    rt,
                    &conn,
                    addr,
                    &paths,
                    pinned,
                    budget,
                    adopted.as_ref(),
                    marks,
                )
            },
            |outcome| match outcome {
                AttemptOutcome::Done(_) => AttemptVerdict::Done,
                AttemptOutcome::FailedAfterConnect { .. } => AttemptVerdict::Fatal,
                AttemptOutcome::ConnectFailed(reason) => AttemptVerdict::Retryable(reason.clone()),
            },
        )
        .await;
        if let Some((addr, AttemptOutcome::Done(report))) = outcome.done {
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
        if let Some((addr, failure)) = outcome.fatal {
            if let AttemptOutcome::FailedAfterConnect { reason, peer_gone } = failure {
                if peer_gone {
                    // The peer's data plane is gone — the pairing is
                    // ending ByPeer through the event pump; a resumed
                    // keepalive would only hold a dead mapping.
                    report_peer_gone(rt, &conn);
                }
                return Err(format!(
                    "transfer failed after connecting via {addr}: {reason}"
                ));
            }
            unreachable!("the FFI verdict maps only FailedAfterConnect to Fatal");
        }
        failures = outcome.failures;
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
/// `Connecting` report, the caller's `connect_budget` from it (normally
/// [`TASK_CONNECT_BUDGET`]; the leading observed/reflexive candidate
/// dials under [`OBSERVED_DIAL_BUDGET`] — the shared pass's hot-mapping
/// rule), no deadline once
/// connected. Post-connect failures are distinguishable so the fallback
/// loop stops instead of retrying a protocol error on another address.
///
/// CR-P1-05: `adopted` (the sender's stashed dial socket, when any) is
/// adopted into the spec — the QUIC Initial leaves from the socket the
/// same-socket STUN query and probe round mapped. The bind fallback
/// stays family-matched for the no-adoption path.
///
/// CR-P3-09: `marks` is the CALLER's (the shared `staggered_dial_pass`'s)
/// [`ConnectMarks`] for this attempt — the pass reads its `connected()`
/// flag as the Happy-Eyeballs winner signal, so the marks cannot live
/// inside this function. (The extra argument pushes the count past
/// clippy's default — bundling the borrow-only FFI context into a struct
/// would be churn for one call site; the shape mirrors the CLI
/// adapter's `SendCtx` grouping where it pays for itself.)
#[allow(clippy::too_many_arguments)]
async fn attempt_task_send(
    rt: &Arc<TaskRt>,
    conn: &TaskConn,
    addr: SocketAddr,
    paths: &[String],
    pinned: Fingerprint,
    connect_budget: Duration,
    adopted: Option<&std::net::UdpSocket>,
    marks: Arc<ConnectMarks>,
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
    // Family-matched wildcard bind (the engine fast-fails mismatches);
    // ignored when the adopted socket carries the dial.
    spec.bind = if addr.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    if let Some(socket) = adopted {
        if let Ok(handle) = socket.try_clone() {
            spec.adopt_socket(handle);
        }
    }
    spec.checkpoint_dir = rt.id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
    spec.idle_timeout = TASK_SEND_IDLE;

    let progress_queue = Arc::clone(&rt.engine_progress);
    let conn_hex = core::hex_encode(&conn.conn_id);
    let send_marks = Arc::clone(&marks);
    let progress = move |event: Progress| {
        send_marks.mark(&event);
        // Client UI U1: the engine payload fields ride along (ffi-v2 §8.4).
        core::push_event(&progress_queue, &conn_hex, None, &event);
    };
    let send = send(spec, &progress);
    tokio::pin!(send);
    // The caller's budget: [`TASK_CONNECT_BUDGET`] for static candidates,
    // [`OBSERVED_DIAL_BUDGET`] for the leading observed/reflexive
    // candidate (see the constant).
    let result = match drive_with_connect_budget(&marks, send.as_mut(), connect_budget).await {
        Ok(result) => result,
        Err(()) => {
            return AttemptOutcome::ConnectFailed(format!(
                "timeout after {}s (no QUIC handshake)",
                connect_budget.as_secs(),
            ))
        }
    };
    let after_connect = marks.connected();
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

/// CR-P2-05 (review 2026-11-11, 改法第 1 条后半): `ak_task_set_event_notify`
/// safe half — register or clear ONE task runtime's event doorbell.
///
/// `Some((func, ctx))` stores the callback (it fires on the thread that
/// queues each subsequent task event; see [`TaskEventNotify`]); `None`
/// CLEARS it and — because [`TaskRt::ring_event_notify`] fires under the
/// slot lock — returns only after an in-flight invocation completed, so
/// the caller can safely destroy the callback right after this call.
/// An unknown runtime is the plane's usual `-4`. The context token is an
/// opaque address Rust never dereferences; `0` (a NULL `void*`) is fine.
pub fn task_set_event_notify(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    notify: Option<(extern "C" fn(ctx: *mut std::os::raw::c_void), usize)>,
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let mut slot = rt
        .notify
        .lock()
        .map_err(|_| FfiError::internal("task notify slot lock poisoned"))?;
    *slot = notify.map(|(func, ctx_addr)| TaskEventNotify { func, ctx_addr });
    Ok(())
}

/// `ak_pairing_cancel_op` task-plane half: cancel a TASK op (abort +
/// terminal "cancelled by caller"). Strict like the op read (C11):
/// the caller routed on the plane, errors propagate as-is.
pub(crate) fn task_cancel_op(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let entry = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("task op table lock poisoned"))?
        .get(op_id)
        .cloned()
        .ok_or_else(|| FfiError::not_found("unknown task op"))?;
    let task = {
        let _gate = rt
            .sender_lifetimes
            .lock()
            .map_err(|_| FfiError::internal("sender lifecycle lock poisoned"))?;
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
        entry
            .task
            .lock()
            .map_err(|_| FfiError::internal("task op lock poisoned"))?
            .take()
    };
    if let Some(task) = task {
        task.abort();
    }
    // SendLease drops only after the cancelled future's UDP owner is gone.

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
mod task_tests;
