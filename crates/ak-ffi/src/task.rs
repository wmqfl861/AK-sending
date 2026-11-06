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
//! the SAME op id and the SAME answer; same id + different spec → `-2`),
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
//!   progress lines flow into it under the conn/code id as the `job`
//!   field, exactly like the scheduler executor's per-task lines.
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
//! (`spawn_owner_loop_verified` + `complete_owner_pairing`), so its
//! `connected` event, connection entry, verified peer key and probe all
//! come from the registry-relayed pairing, not from local state.
//!
//! * **Peer identity** — the joiner pins the owner's device fingerprint
//!   from `TaskPaired::verified.owner_fingerprint()` (the relayed,
//!   transcript-verified key), and the owner the redeemer's. The engine
//!   `send` keeps using it as the TLS pin.
//! * **Probing** — after a pairing both sides run ak-session
//!   `probe_peer` over the peer's advertised candidates (LAN-first) and
//!   emit `probe_result`; the join result's `probe` field is the honest
//!   verdict (`verified` / `all_failed`; `unverified` only when no probe
//!   could run). Probes are answered by the probe echo responder each
//!   side binds alongside its endpoint advertisement (below).
//! * **Remote disconnect** — `TaskPairing::disconnect` revokes the code
//!   at the registry (a pending redeem of a revoked code aborts), and the
//!   [`TaskEvent`] stream (`TaskPairing::subscribe`) is pumped into the
//!   task event queue: `TaskDisconnected{ByPeer}` → `disconnected` with
//!   reason `peer` (the resident receiver reporting the peer's data-plane
//!   departure, or a send that died with `PeerClosed`), `Network` → the
//!   frozen `receiver_failed` wording. A `local` line is pushed directly
//!   by `ak_task_disconnect`, so the pump never repeats it.
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
//!   `max_redeems = 1`, TTL). A RECEIVER-role creation additionally binds
//!   a REAL `ak-engine` resident receiver (`receive_with_addr_consent`,
//!   wildcard bind, sequential serve loop) and derives the code's dial
//!   candidates from the machine's interfaces.
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
//!   with the ak-engine `TransferConsent` hook (wave 28): every incoming
//!   offer mints a pending request id, emits an `incoming_request` event,
//!   and the transfer is held until `ak_task_consent` decides or the
//!   frozen 30 s window auto-declines. The peer is told over the wire
//!   (DECLINED semantics); on a decline nothing is written to disk.
//! * `ak_task_list` / `ak_task_disconnect` / `ak_task_set_save_dir` /
//!   `ak_task_consent` — real state, real bilateral `disconnected` events
//!   (registry revoke + event pump; a same-engine peer additionally gets
//!   the instant cache notification).
#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

use ak_engine::{
    receive_with_addr_consent, send, ConsentInfo, EngineError, Progress, ReceiveSpec, Report,
    SendSpec, TransferConsent,
};
use ak_identity::pairing_txn::{random_id16, VerifiedPairing};
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{
    ClientConfig, OrchestratorConfig, PairFlowError, PairingOrchestrator, PeerEndpoint,
    ProbeOptions, ProbeOutcome, RegistryClient, TaskDirection, TaskDisconnectReason, TaskError,
    TaskEvent, TaskPairing, TaskPairingConfig, TaskPaired, TASK_CODE_TTL_SECS,
};
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
    /// running (the entry flips to `connected` when the probe settles —
    /// whichever way it settles; the verdict itself rides the
    /// `probe_result` event and the join result).
    Probing,
    Connected,
    Disconnected,
}

impl ConnState {
    fn as_str(self) -> &'static str {
        match self {
            ConnState::Probing => "probing",
            ConnState::Connected => "connected",
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

/// One pending receive-consent request (held until decided or the 30 s
/// window auto-declines). The human-readable summary (`conn`, root, files,
/// bytes) rides the `incoming_request` event; the held state itself is
/// just the undecided/decided flag.
#[derive(Debug, Clone)]
struct PendingConsent {
    decision: Option<bool>,
}

/// One accepted op's idempotency record: the minted op id plus the
/// canonical spec the request was accepted for.
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
}

/// The per-runtime ak-session task-pairing stack: one registry client,
/// one orchestrator (journal + trust book under `id_dir`), one
/// `TaskPairing` on top, plus the resident tasks that keep it alive — the
/// task-aware owner loop (answers redeems of this device's codes and
/// completes pairings), the network watcher (registry loss → Network task
/// ends) and the event pump ([`TaskEvent`]s → task event lines).
struct TaskStack {
    pairing: TaskPairing,
    owner_loop: StdMutex<Option<JoinHandle<()>>>,
    network_watcher: StdMutex<Option<JoinHandle<()>>>,
    event_pump: StdMutex<Option<JoinHandle<()>>>,
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
        if let Ok(mut slot) = self.network_watcher.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
        if let Ok(mut slot) = self.event_pump.lock() {
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
    pending: StdMutex<HashMap<[u8; 16], PendingConsent>>,
    /// Wakes blocked consent gates (paired with [`TaskRt::pending`]).
    consent_signal: Condvar,
    /// Resident-receiver + probe-echo tasks (aborted by teardown),
    /// keyed by code id (a local placeholder id until the registry
    /// assigns the real one). One code owns SEVERAL tasks — the receiver
    /// and the echo responder — so each key holds its handles as a set.
    spawned: StdMutex<HashMap<[u8; 16], Vec<JoinHandle<()>>>>,
    /// Flipped by teardown; every consent gate checks it first.
    stopped: StdMutex<bool>,
    /// request_id idempotency: request id → (op id, canonical spec).
    request_ids: StdMutex<HashMap<[u8; 16], TaskRequestRecord>>,
    /// The lazily built ak-session stack (first create/join op).
    stack: StdMutex<Option<Arc<TaskStack>>>,
    /// In-flight op ids per connection (a disconnect aborts them).
    conn_ops: StdMutex<HashMap<[u8; 16], Vec<[u8; 16]>>>,
    /// The engine-shared task-code cache (module doc: engine sharing
    /// without engine borrows).
    codes: Arc<TaskCodeTable>,
    /// The engine-wide progress queue (`ak_poll_progress`).
    engine_progress: Arc<StdMutex<VecDeque<String>>>,
}

impl TaskRt {
    /// Teardown (engine drop): invalidate waiters FIRST — a consent gate
    /// blocked on the condvar cannot be interrupted by `abort()`, so every
    /// pending request gets an explicit decline and a wakeup before any
    /// task is aborted. Order matters for the same reason the wave-26
    /// P1-05 Dart sequence does: waiters, then owners.
    pub(crate) fn teardown(&self, runtime: &tokio::runtime::Runtime) {
        {
            let mut stopped = match self.stopped.lock() {
                Ok(slot) => slot,
                Err(_) => return,
            };
            *stopped = true;
        }
        {
            // Wake every blocked consent gate with an explicit decline.
            let mut pending = match self.pending.lock() {
                Ok(slot) => slot,
                Err(_) => return,
            };
            for entry in pending.values_mut() {
                if entry.decision.is_none() {
                    entry.decision = Some(false);
                }
            }
            drop(pending);
            self.consent_signal.notify_all();
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
        // above), then close the control client bounded — an open client
        // would otherwise hold the TCP connection past the engine drop.
        let stack = self
            .stack
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(stack) = stack {
            stack.abort_tasks();
            let client = stack.pairing.orchestrator().client().clone();
            let _ = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), client.close()).await
            });
        }
        // Drop this runtime's cached code registrations: it breaks the
        // table↔runtime Arc cycle AND un-advertises codes whose owner is
        // going away (the registry rows expire on their own TTL; the
        // cache only mirrors them for same-engine lookups).
        if let Ok(mut table) = self.codes.lock() {
            table.retain(|_, entry| entry.owner.runtime_id != self.runtime_id);
        }
        // Bounded settle window: the woken gates return at their next
        // instruction; aborted tasks land at their next await point. The
        // empty future keeps the timeout on the runtime's own clock.
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

    /// Register one pending consent, emit `incoming_request`, and hold
    /// until decided or `window` elapses (then auto-decline). Runs INSIDE
    /// the engine's receive body — the blocking wait IS the consent gate
    /// (single-connection v1; the embedded runtime keeps the other worker
    /// free for the peer's send).
    fn gate_consent(&self, code_id: &[u8; 16], info: &ConsentInfo, window: Duration) -> bool {
        if self.is_stopped() {
            return false;
        }
        let job = random_id16();
        let conn = self.conn_of_code(code_id);
        {
            let mut pending = match self.pending.lock() {
                Ok(slot) => slot,
                Err(_) => return false,
            };
            pending.insert(job, PendingConsent { decision: None });
        }
        let mut line = String::with_capacity(160);
        line.push_str(&format!(
            "{{\"event\":\"incoming_request\",\"job\":\"{}\"",
            core::hex_encode(&job),
        ));
        if let Some(conn) = &conn {
            line.push_str(&format!(",\"conn\":\"{}\"", core::hex_encode(&conn.conn_id)));
        }
        line.push_str(&format!(
            ",\"root\":{},\"files\":{},\"bytes\":{}}}",
            crate::core::json::quoted(&info.root_name),
            info.file_count,
            info.total_bytes,
        ));
        self.push_event(line);
        self.await_decision(job, window)
    }

    /// Block until `ak_task_consent` decides `job`, the runtime stops, or
    /// `window` elapses (auto-decline). The entry is always removed.
    fn await_decision(&self, job: [u8; 16], window: Duration) -> bool {
        let deadline = std::time::Instant::now() + window;
        let mut pending = match self.pending.lock() {
            Ok(slot) => slot,
            Err(_) => return false,
        };
        loop {
            if self.is_stopped() {
                pending.remove(&job);
                self.consent_signal.notify_all();
                return false;
            }
            match pending.get(&job) {
                Some(entry) => {
                    if let Some(decision) = entry.decision {
                        pending.remove(&job);
                        return decision;
                    }
                }
                None => return false, // removed elsewhere: declined
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                pending.remove(&job);
                self.consent_signal.notify_all();
                return false; // the frozen 30 s auto-decline
            }
            let (guard, _) = self
                .consent_signal
                .wait_timeout(pending, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending = guard;
        }
    }
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
    let id_dir = field_str(&args, "id_dir", true)?
        .ok_or_else(|| FfiError::invalid("missing \"id_dir\""))?;
    if id_dir.is_empty() {
        return Err(FfiError::invalid("field \"id_dir\" must be non-empty"));
    }
    let device_name = field_str(&args, "device_name", false)?.unwrap_or_else(|| "ak-task".into());
    if device_name.chars().count() > 64 {
        return Err(FfiError::invalid(
            "field \"device_name\" must be at most 64 characters",
        ));
    }
    let trimmed_dir = id_dir.trim_end_matches(['/','\\']);
    let save_dir_text = field_str(&args, "save_dir", false)?
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format!("{trimmed_dir}/downloads"));
    // The save folder is created eagerly: ak-engine's LocalSaveTarget
    // requires an existing directory, so a missing default would fail the
    // receiver-role code creation later (better to fail at start).
    std::fs::create_dir_all(&save_dir_text).map_err(|error| {
        FfiError::invalid(format!(
            "cannot create save dir {save_dir_text}: {error}"
        ))
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
        consent_signal: Condvar::new(),
        spawned: StdMutex::new(HashMap::new()),
        stopped: StdMutex::new(false),
        request_ids: StdMutex::new(HashMap::new()),
        stack: StdMutex::new(None),
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
/// on top, and its three resident tasks — the task-aware owner loop, the
/// network watcher and the event pump. Runs inside op bodies, i.e. on the
/// engine's embedded runtime (the client's reader tasks spawn there).
async fn ensure_stack(rt: &Arc<TaskRt>) -> Result<Arc<TaskStack>, String> {
    if let Some(stack) = rt
        .stack
        .lock()
        .map_err(|_| "task stack lock poisoned".to_string())?
        .clone()
    {
        return Ok(stack);
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
        network_watcher: StdMutex::new(None),
        event_pump: StdMutex::new(None),
    });
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
        if let Ok(paired) = owner_stack.pairing.complete_owner_pairing(&verified, direction) {
            let rt = Arc::clone(&owner_rt);
            let stack = Arc::clone(&owner_stack);
            tokio::spawn(async move {
                owner_side_completion(&rt, &stack, code_id, paired).await;
            });
        }
    });
    let owner_loop = stack
        .pairing
        .orchestrator()
        .spawn_owner_loop_verified(Some(on_paired));
    let watcher = stack.pairing.spawn_network_watcher();
    let pump = spawn_task_event_pump(Arc::clone(rt), &stack);
    {
        let mut slot = rt
            .stack
            .lock()
            .map_err(|_| "task stack lock poisoned".to_string())?;
        if let Some(existing) = slot.as_ref() {
            // A racing op built the stack first: keep theirs, abort ours
            // (dropping a JoinHandle does NOT stop a tokio task).
            for task in [owner_loop, watcher, pump] {
                task.abort();
            }
            return Ok(Arc::clone(existing));
        }
        if let Ok(mut handle) = stack.owner_loop.lock() {
            *handle = Some(owner_loop);
        }
        if let Ok(mut handle) = stack.network_watcher.lock() {
            *handle = Some(watcher);
        }
        if let Ok(mut handle) = stack.event_pump.lock() {
            *handle = Some(pump);
        }
        *slot = Some(Arc::clone(&stack));
    }
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
                    // departure, `receiver_failed` for the link-loss end.
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

/// The owner side of a completed registry pairing: the connection entry
/// (conn id = the pairing transaction's id, identical on both sides),
/// the `connected` event, then the real reachability probe.
async fn owner_side_completion(
    rt: &Arc<TaskRt>,
    stack: &Arc<TaskStack>,
    code_id: [u8; 16],
    paired: TaskPaired,
) {
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
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"role\":{},\"peer_endpoint\":{endpoint},\"code\":{}}}",
        core::hex_encode(&conn_id),
        quoted(role.as_str()),
        quoted(&display),
    ));
    let endpoint_probe = PeerEndpoint {
        candidates: candidates.clone(),
        name: paired.peer.name.clone(),
        direction: paired.peer.direction,
    };
    let outcome = probe_with_retry(&stack.pairing, &endpoint_probe).await;
    if let Ok(mut conns) = rt.conns.lock() {
        if let Some(conn) = conns.get_mut(&conn_id) {
            if conn.state == ConnState::Probing {
                conn.state = ConnState::Connected;
            }
        }
    }
    let (line, _verdict) = probe_result_line(&conn_id, &outcome);
    rt.push_event(line);
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
        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((len, source)) => {
                    if ak_net::holepunch::is_probe_packet(&buffer[..len]) {
                        // Echo a probe packet back — exactly what the peer
                        // side of a paired task observes while probing.
                        let _ = socket
                            .send_to(&ak_net::holepunch::probe_packet(&[0u8; 16]), source)
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

/// Resolve the request_id idempotency key: `Ok(Some(op_id))` on an exact
/// replay (the caller returns the ORIGINAL op id, no side effects),
/// `Err(-2)` on a request-id collision with a different spec, `Ok(None)`
/// when the key is fresh or absent. Runs BEFORE any side effect.
fn replay_check(rt: &TaskRt, args: &crate::core::json::Json) -> Result<Option<[u8; 16]>, FfiError> {
    let Some(request_text) = field_str(args, "request_id", false)? else {
        return Ok(None);
    };
    let request_id = hex16(&request_text, "field \"request_id\"")?;
    let canonical = canonical_spec_json(args);
    let registry = rt
        .request_ids
        .lock()
        .map_err(|_| FfiError::internal("task request-id registry lock poisoned"))?;
    match registry.get(&request_id) {
        Some((op_id, previous)) if *previous == canonical => Ok(Some(*op_id)),
        Some(_) => Err(FfiError::invalid(format!(
            "request_id {} was already accepted with a different spec",
            core::hex_encode(&request_id),
        ))),
        None => Ok(None),
    }
}

/// Record the idempotency key AFTER the op was accepted (replays answer
/// the same op id forever).
fn record_request_id(rt: &TaskRt, args: &crate::core::json::Json, op_id: [u8; 16]) {
    let Ok(Some(request_text)) = field_str(args, "request_id", false) else {
        return;
    };
    let Ok(request_id) = hex16(&request_text, "field \"request_id\"") else {
        return;
    };
    if let Ok(mut registry) = rt.request_ids.lock() {
        registry
            .entry(request_id)
            .or_insert((op_id, canonical_spec_json(args)));
    }
}

/// Shared op launcher (the pairing plane's `launch_op`, task-plane
/// shape): register the entry, spawn the body on the embedded runtime,
/// stamp the terminal JSON (kept for lossless repeated reads), announce
/// `op_done`/`op_failed` in the task event queue. The body owns ONLY
/// `Arc` clones — never an engine borrow (module doc).
fn launch_task_op(
    engine: &AkEngine,
    rt: &Arc<TaskRt>,
    spec: TaskOpSpec,
    args_json: &crate::core::json::Json,
) -> Result<[u8; 16], FfiError> {
    let op = spec.op();
    let op_id = random_id16();
    // Sends are abortable by a disconnect of their connection.
    if let TaskOpSpec::Send { conn_id, .. } = &spec {
        rt.track_conn_op(*conn_id, op_id);
    }
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
    record_request_id(rt, args_json, op_id);
    Ok(op_id)
}

// ---------------------------------------------------------------------------
// ak_task_create_code / ak_task_join / ak_task_send (acceptance).
// ---------------------------------------------------------------------------

/// `ak_task_create_code`: claim a task code at the REGISTRY under the
/// chosen role (endpoint blob + `max_redeems = 1`; one code pairs once).
/// A RECEIVER-role creation binds the resident receiver first; the op
/// completes with the registry's display code only after the bind and the
/// claim both succeeded.
pub fn task_create_code_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let role_text = field_str(&args, "role", true)?
        .ok_or_else(|| FfiError::invalid("missing \"role\""))?;
    let role = TaskRole::parse(&role_text)?;
    let candidates = parse_candidates(&args)?;
    let ttl_secs = field_u64(&args, "ttl_secs")?.unwrap_or(0);
    if ttl_secs > u64::from(u32::MAX) {
        return Err(FfiError::invalid("field \"ttl_secs\" is too large"));
    }
    if let Some(replay) = replay_check(&rt, &args)? {
        return Ok(replay);
    }
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::CreateCode {
            role,
            candidates,
            ttl_secs: ttl_secs as u32,
        },
        &args,
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
    let code = field_str(&args, "code", true)?
        .ok_or_else(|| FfiError::invalid("missing \"code\""))?;
    if code.trim().is_empty() {
        return Err(FfiError::invalid("field \"code\" must be non-empty"));
    }
    let role_hint = match field_str(&args, "role", false)? {
        None => None,
        Some(text) => Some(TaskRole::parse(&text)?),
    };
    let candidates = parse_candidates(&args)?;
    if let Some(replay) = replay_check(&rt, &args)? {
        return Ok(replay);
    }
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::Join {
            code: normalize_code_key(&code),
            candidates,
            role_hint,
        },
        &args,
    )
}

/// `ak_task_send`: run a real engine send over the connection's candidate
/// endpoints (LAN-first, 10 s split connect budget, automatic fallback).
pub fn task_send_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    conn_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    {
        let conns = rt
            .conns
            .lock()
            .map_err(|_| FfiError::internal("task connection table lock poisoned"))?;
        let conn = conns.get(conn_id).ok_or_else(|| {
            FfiError::not_found(format!(
                "unknown connection {} on task runtime {}",
                core::hex_encode(conn_id),
                core::hex_encode(runtime_id),
            ))
        })?;
        if conn.state == ConnState::Disconnected {
            return Err(FfiError::invalid("the connection is disconnected"));
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
        return Err(FfiError::invalid("entries of \"paths\" must be non-empty paths"));
    }
    if let Some(replay) = replay_check(&rt, &args)? {
        return Ok(replay);
    }
    launch_task_op(
        engine,
        &rt,
        TaskOpSpec::Send {
            conn_id: *conn_id,
            paths,
        },
        &args,
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

/// `ak_task_disconnect`: sever one connection. The local side gets its
/// `disconnected`/`local` line immediately; the code is revoked AT THE
/// REGISTRY (a pending redeem of it aborts server-side, and the pair's
/// data-plane drop reaches the peer — its resident receiver reports the
/// departure, which the event pump surfaces as `disconnected`/`peer`);
/// a SAME-ENGINE peer additionally gets the instant cache notification.
/// In-flight send ops of the connection are aborted (their QUIC endpoint
/// dropping is what carries the close to the peer). Idempotent: an
/// already-severed connection is a silent `AK_OK`.
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
        let conn = conns.get_mut(conn_id).ok_or_else(|| {
            FfiError::not_found(format!(
                "unknown connection {} on task runtime {}",
                core::hex_encode(conn_id),
                core::hex_encode(runtime_id),
            ))
        })?;
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
    std::fs::create_dir_all(&dir)
        .map_err(|error| FfiError::invalid(format!("cannot create save dir {}: {error}", dir.display())))?;
    let mut save_dir = rt
        .save_dir
        .lock()
        .map_err(|_| FfiError::internal("save dir lock poisoned"))?;
    *save_dir = dir;
    Ok(())
}

/// `ak_task_consent`: decide one pending `incoming_request` (accept or
/// decline). An unknown or already-decided request is `-4`; the 30 s
/// window auto-declines on its own.
pub fn task_consent(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    job_id: &[u8; 16],
    accept: bool,
) -> Result<(), FfiError> {
    let rt = task_runtime_of(engine, runtime_id)?;
    let mut pending = rt
        .pending
        .lock()
        .map_err(|_| FfiError::internal("pending consent table lock poisoned"))?;
    let already = pending
        .get(job_id)
        .is_some_and(|entry| entry.decision.is_some());
    if already {
        return Err(FfiError::not_found(format!(
            "consent request {} was already decided",
            core::hex_encode(job_id),
        )));
    }
    let entry = pending.get_mut(job_id).ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown consent request {}",
            core::hex_encode(job_id),
        ))
    })?;
    entry.decision = Some(accept);
    drop(pending);
    rt.consent_signal.notify_all();
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

/// The resident receiver: bind once (wildcard), serve exactly ONE
/// connection, then end. Rationale: `receive_with_addr_consent` binds
/// internally, so a serve loop would rebind a DIFFERENT port per job and
/// strand the advertised candidates; and in the v1 model one code carries
/// one connection (the joiner's dial is the first and only completed
/// handshake — a stray dial cannot complete TLS against this endpoint's
/// certificate). Every job runs with the offer-level consent gate;
/// progress flows into the engine-wide queue under the code id, and a
/// completed job emits a `done` event carrying the connection of the
/// code (once joined). A DECLINED offer ends the receiver too (the
/// connection is closed at the Offer point; the sender learns over the
/// wire — a fresh code is the v1 answer for another attempt).
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
    let code_id = current_code();
    let out_dir = match rt.save_dir.lock() {
        Ok(dir) => dir.clone(),
        Err(_) => return,
    };
    // ak-engine's save target requires the directory to exist (created at
    // start/set time; re-created best-effort here in case it was removed).
    let _ = std::fs::create_dir_all(&out_dir);
    let mut spec = ReceiveSpec::new(out_dir, make_self_signed(&rt.key));
    spec.bind = SocketAddr::from(([0, 0, 0, 0], 0));
    spec.checkpoint_dir = rt.id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
    spec.idle_timeout = TASK_SEND_IDLE;
    // v1 zero-fingerprint admission: the channel accepts any peer; the
    // USER-facing gate is the offer-level consent below (the pairing's
    // verified key is pinned at SEND time, not at admission).
    spec.authorize = Box::new(|_| true);
    let consent: TransferConsent = {
        let rt = Arc::clone(&rt);
        let code_slot = Arc::clone(&code_slot);
        Arc::new(move |info: &ConsentInfo| {
            let code_id = code_slot
                .lock()
                .map(|slot| *slot)
                .unwrap_or_else(|poisoned| *poisoned.into_inner());
            rt.gate_consent(&code_id, info, PENDING_CONSENT_WINDOW)
        })
    };
    let progress_queue = Arc::clone(&rt.engine_progress);
    let code_hex = core::hex_encode(&code_id);
    let progress = move |event: Progress| {
        if let Ok(mut queue) = progress_queue.lock() {
            core::push_capped(
                &mut queue,
                format!(
                    "{{\"job\":{},\"event\":{}}}",
                    quoted(&code_hex),
                    quoted(core::progress_name(&event)),
                ),
            );
        }
    };
    let result = receive_with_addr_consent(spec, &progress, first_addr, Some(consent)).await;
    match result {
        Ok(report) => {
            let conn = rt.conn_of_code(&current_code());
            let mut line = String::with_capacity(128);
            line.push_str("{\"event\":\"done\"");
            if let Some(conn) = &conn {
                line.push_str(&format!(",\"conn\":\"{}\"", core::hex_encode(&conn.conn_id)));
            }
            line.push_str(&format!(
                ",\"files\":{},\"bytes\":{}}}",
                report.files, report.bytes,
            ));
            rt.push_event(line);
        }
        Err(EngineError::ConsentDeclined { .. }) => {
            // The user (or the 30 s window) declined: the pending entry is
            // resolved, the peer was told over the wire, nothing landed on
            // disk. The receiver ends (one code, one connection).
        }
        Err(_) => {
            // Channel-level failure (sender gave up mid-handshake, peer
            // disconnected mid-transfer, IO error). The receiver ends;
            // when the pairing had completed, this is the peer going away
            // — report it so the task ends as ByPeer through the event
            // pump (the cross-device `disconnected`/`peer` notification).
            let code_id = current_code();
            if rt.conn_of_code(&code_id).is_some() {
                if let Some(stack) = rt
                    .stack
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                {
                    stack.pairing.report_peer_disconnect(code_id);
                }
            }
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
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"code\":{}}}",
        core::hex_encode(&conn_id),
        quoted(my_role.as_str()),
        endpoint,
        quoted(code),
    ));
    // The REAL reachability probe over the peer's advertised candidates.
    let outcome = probe_with_retry(&stack.pairing, &paired.peer).await;
    let (probe_line, verdict) = probe_result_line(&conn_id, &outcome);
    if let Ok(mut conns) = rt.conns.lock() {
        if let Some(conn) = conns.get_mut(&conn_id) {
            if conn.state == ConnState::Probing {
                conn.state = ConnState::Connected;
            }
        }
    }
    rt.push_event(probe_line);
    Ok(format!(
        "{{\"op\":\"task_join\",\"conn\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"probe\":\"{verdict}\"}}",
        core::hex_encode(&conn_id),
        quoted(my_role.as_str()),
        endpoint,
    ))
}

/// One send op: iterate the connection's candidates LAN-first under the
/// split connect budget; a post-connect failure fails the op (another
/// address cannot fix it); success emits `done` and completes the op.
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
                return Err(format!("transfer failed after connecting via {addr}: {reason}"));
            }
            AttemptOutcome::ConnectFailed(reason) => {
                failures.push((*addr, reason));
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

/// `ak_pairing_op_result` fallback: read a TASK op's outcome. Returns
/// `Ok(None)` when the runtime id is not a task runtime (the caller keeps
/// the pairing plane's own error).
pub(crate) fn task_op_result_maybe(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<Option<OpResult>, FfiError> {
    let rt = match task_runtime_of(engine, runtime_id) {
        Ok(rt) => rt,
        Err(_) => return Ok(None),
    };
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
    Ok(Some(match &*outcome {
        OpOutcome::Running { .. } => OpResult::Running,
        OpOutcome::Finished { ok, json } => OpResult::Finished {
            ok: *ok,
            json: json.clone(),
        },
    }))
}

/// `ak_pairing_poll_event` fallback: pop a TASK runtime's event. Returns
/// the popped line plus the queue it came from (the shim pushes back to
/// THE queue on `-3`), or `Ok(None)` when the runtime id is not a task
/// runtime.
pub(crate) fn task_poll_event_maybe(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
) -> Result<Option<PolledTaskLine>, FfiError> {
    let rt = match task_runtime_of(engine, runtime_id) {
        Ok(rt) => rt,
        Err(_) => return Ok(None),
    };
    let queue = Arc::clone(&rt.events);
    let line = queue
        .lock()
        .map_err(|_| FfiError::internal("task event queue lock poisoned"))?
        .pop_front();
    Ok(Some((line, queue)))
}

/// `ak_pairing_poll_event` pushback half: push one event line back to a
/// TASK runtime's queue (after an `AK_ERR_BUFFER`). Returns `Ok(false)`
/// when the runtime id is not a task runtime.
pub(crate) fn task_push_event_front_maybe(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    line: String,
) -> Result<bool, FfiError> {
    let rt = match task_runtime_of(engine, runtime_id) {
        Ok(rt) => rt,
        Err(_) => return Ok(false),
    };
    let mut queue = rt
        .events
        .lock()
        .map_err(|_| FfiError::internal("task event queue lock poisoned"))?;
    queue.push_front(line);
    Ok(true)
}

/// `ak_pairing_cancel_op` fallback: cancel a TASK op (abort + terminal
/// "cancelled by caller"). Returns `Ok(false)` when the runtime id is not
/// a task runtime.
pub(crate) fn task_cancel_op_maybe(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<bool, FfiError> {
    let rt = match task_runtime_of(engine, runtime_id) {
        Ok(rt) => rt,
        Err(_) => return Ok(false),
    };
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
            json: format!("{{\"op\":{},\"error\":\"cancelled by caller\"}}", quoted(op)),
        };
    }
    Ok(true)
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
    use crate::shim::{
        ak_engine_free, ak_engine_last_error, ak_engine_new, ak_pairing_op_result,
        ak_pairing_poll_event,
    };
    use crate::task_ext::{
        ak_task_consent, ak_task_create_code, ak_task_disconnect, ak_task_join, ak_task_list,
        ak_task_send, ak_task_set_save_dir, ak_task_start,
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

    fn create_code(engine: *mut AkEngine, runtime_id: &[u8; 16], spec: &str) -> ([u8; 16], bool, String) {
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
        let status =
            ak_task_join(engine, runtime_id.as_ptr(), spec.as_ptr(), op_id.as_mut_ptr());
        assert_eq!(status, 0, "ak_task_join not accepted: {}", last_error_of(engine));
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
        assert!(event.contains("\"runtime\":"), "runtime id rides the event: {event}");
        ak_engine_free(engine);
    }

    #[test]
    fn task_start_validates_arguments_before_side_effects() {
        let engine = ak_engine_new();
        let dir = tempfile::tempdir().expect("tempdir");
        let good = CString::new(spec_for(dir.path())).expect("spec");
        let mut out = [0u8; 16];
        // Null output pointer: refused BEFORE parsing (no runtime minted).
        assert_eq!(ak_task_start(engine, good.as_ptr(), std::ptr::null_mut()), -1);
        // Null spec.
        assert_eq!(ak_task_start(engine, std::ptr::null(), out.as_mut_ptr()), -1);
        // Malformed JSON.
        let bad = CString::new("{nope").expect("bad");
        assert_eq!(ak_task_start(engine, bad.as_ptr(), out.as_mut_ptr()), -2);
        assert!(!last_error_of(engine).is_empty(), "the parse detail lands in last_error");
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
        assert_eq!(ak_task_start(engine, bad_pin.as_ptr(), out.as_mut_ptr()), -2);
        assert!(last_error_of(engine).contains("registry_id"));
        // Empty id_dir.
        let no_dir =
            CString::new("{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"11\",\"id_dir\":\"\"}")
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
        let (_, ok, json) =
            create_code(engine, &runtime_id, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "create must succeed at the real registry: {json}");
        assert!(json.contains("\"op\":\"task_create_code\""), "{json}");
        // The registry mints the invite display form (AKI1-…).
        assert!(json.contains("\"code\":\"AKI1-"), "registry display code: {json}");
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
        let (_, ok, json) =
            create_code(engine, &runtime_id, "{\"spec_version\":1,\"role\":\"receiver\"}");
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
        let (_, ok, json) =
            create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        // Separator tolerance: the UI's normalized (hyphen-less) form of
        // the SAME code joins the SAME registration.
        let stripped = code.split('-').collect::<String>();
        let stripped_lower = stripped.to_lowercase();
        let (join_ok, join_json) = join_code(engine, &joiner, &stripped_lower);
        assert!(join_ok, "join must succeed through the registry: {join_json}");
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
        assert_eq!(ak_task_list(engine, owner.as_ptr(), buf.as_mut_ptr(), buf.len()), 1);
        let owner_list = text_of(&buf);
        assert!(owner_list.contains("\"state\":\"connected\""), "{owner_list}");
        buf.iter_mut().for_each(|slot| *slot = 0);
        assert_eq!(ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()), 1);
        let joiner_list = text_of(&buf);
        assert!(joiner_list.contains("\"state\":\"connected\""), "{joiner_list}");
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
            ak_task_join(engine, runtime_id.as_ptr(), no_code.as_ptr(), op_id.as_mut_ptr()),
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
        assert_eq!(ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()), 1);
        let full = text_of(&big);
        // Tiny buffer: -3, nothing consumed, the message names the size.
        let mut tiny = cbuf(8);
        assert_eq!(ak_task_list(engine, runtime_id.as_ptr(), tiny.as_mut_ptr(), tiny.len()), -3);
        let detail = last_error_of(engine);
        assert!(detail.contains("buffer too small"), "{detail}");
        // Retry with the full capacity: the SAME JSON.
        big.iter_mut().for_each(|slot| *slot = 0);
        assert_eq!(ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()), 1);
        assert_eq!(text_of(&big), full);
        // Unknown runtime: -4.
        assert_eq!(ak_task_list(engine, [9u8; 16].as_ptr(), big.as_mut_ptr(), big.len()), -4);
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
        assert_eq!(ak_task_set_save_dir(engine, runtime_id.as_ptr(), path.as_ptr()), 0);
        assert!(inbox.is_dir(), "the save dir is created eagerly");
        let mut buf = cbuf(CAP);
        assert_eq!(ak_task_list(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len()), 1);
        let list = text_of(&buf);
        assert!(list.contains("inbox"), "{list}");
        // Empty path: -2. Unknown runtime: -4.
        let empty = CString::new("").expect("empty");
        assert_eq!(ak_task_set_save_dir(engine, runtime_id.as_ptr(), empty.as_ptr()), -2);
        assert_eq!(ak_task_set_save_dir(engine, [7u8; 16].as_ptr(), path.as_ptr()), -4);
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
        let (_, ok, json) =
            create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
        assert!(ok, "{json}");
        let code = code_of(&json);
        let (join_ok, join_json) = join_code(engine, &joiner, &code);
        assert!(join_ok, "{join_json}");
        let conn = conn_of(&join_json);
        assert_eq!(ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()), 0);
        // Local side: reason local; peer side: reason peer (the
        // same-engine instant cache notify).
        let local = wait_event(engine, &joiner, "disconnected");
        assert!(local.contains("\"reason\":\"local\""), "{local}");
        let peer = wait_event(engine, &owner, "disconnected");
        assert!(peer.contains("\"reason\":\"peer\""), "{peer}");
        // Both lists now report the connection disconnected.
        let mut buf = cbuf(CAP);
        assert_eq!(ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()), 1);
        assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
        // Idempotent: a second disconnect is a silent AK_OK; an unknown
        // conn id is -4.
        assert_eq!(ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()), 0);
        assert_eq!(ak_task_disconnect(engine, joiner.as_ptr(), [5u8; 16].as_ptr()), -4);
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
        assert_eq!(ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()), 0);

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
        let delivered =
            find_file(&inbox, "e2e.bin").expect("the file landed in the save dir");
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
        assert_eq!(ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()), 0);
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
        assert!(!send_ok, "a declined transfer must fail the op: {send_json}");
        assert!(find_file(&inbox, "no.bin").is_none(), "nothing may land on a decline");
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
        let paths =
            CString::new(json_paths(std::path::Path::new("C:/nowhere/none.bin"))).unwrap();
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
        assert_eq!(ak_task_consent(engine, runtime_id.as_ptr(), [4u8; 16].as_ptr(), 1), -4);
        assert_eq!(ak_task_consent(engine, [4u8; 16].as_ptr(), [4u8; 16].as_ptr(), 1), -4);
        ak_engine_free(engine);
    }

    /// Teardown flips `stopped`: the consent gate declines every later
    /// offer immediately (the waiter-first teardown of engine drop). Built
    /// on the safe layer with an OWNED engine value, so no raw pointer is
    /// ever dereferenced (this module is `forbid(unsafe_code)`).
    #[test]
    fn consent_gate_declines_after_teardown() {
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
        };
        let started = Instant::now();
        let decision = rt.gate_consent(&[0u8; 16], &info, PENDING_CONSENT_WINDOW);
        assert!(!decision, "a stopped runtime declines every offer");
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
        let (_, ok, json) =
            create_code(engine, &runtime_id, "{\"spec_version\":1,\"role\":\"receiver\"}");
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
        assert!(join_ok, "the registry relay must carry the code across engines: {join_json}");
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
        assert!(probe.contains("\"endpoint\":\""), "the confirmed address rides the event: {probe}");

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
        assert!(dead_probe.contains("\"candidate\":\"127.0.0.1:1\""), "{dead_probe}");
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
        assert_eq!(ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()), 0);
        let (_, ok, json) =
            create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
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
        assert_eq!(ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1), 0);
        assert_eq!(ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()), 0);
        let local = wait_event(engine_b, &joiner, "disconnected");
        assert!(local.contains("\"reason\":\"local\""), "{local}");

        // A's data plane sees the peer go: ByPeer -> the pump -> A's poll.
        let remote = wait_event(engine_a, &owner, "disconnected");
        assert!(
            remote.contains("\"reason\":\"peer\""),
            "the peer device must learn the disconnect: {remote}"
        );
        let mut buf = cbuf(CAP);
        assert_eq!(ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()), 1);
        assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
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
