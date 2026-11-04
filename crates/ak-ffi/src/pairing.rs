//! B8 (wave 22): the safe layer behind the `ak_pairing_*` and `ak_sched_*`
//! C ABI exports — pairing orchestration and the transfer scheduler, both
//! embedded in the [`AkEngine`](crate::AkEngine) handle.
//!
//! Everything here is compiled with `forbid(unsafe_code)` like the rest of
//! the core; the pointer shim in [`crate::shim`] adapts it to C. No network
//! wait ever happens inside a synchronous entry point: `ak_pairing_start`
//! accepts the runtime and connects in the background (the phase becomes
//! observable through the runtime's event queue), and every control
//! operation is an *op* — a spawned task whose result is polled with
//! `ak_pairing_op_result` (contract `docs/contracts/ffi-v2.md` §2: "`start`
//! 返回“已受理”，在线、完成、撤销同步等由结果确认").
//!
//! # Composition (public ak-session surface only, nothing private)
//!
//! * one [`PairingRt`] per `ak_pairing_start`: a real
//!   [`RegistryClient`] over pinned TLS (ALPN `ak-signal/2`, exporter
//!   `AK control hello exporter`, raw-key pin — the wave-22 unified
//!   constants), a [`PairingOrchestrator`] (journal + trust book under
//!   `id_dir`), the resident owner loop, an op table and an event queue;
//! * at most one [`SchedRt`] per engine: a real
//!   [`SessionRuntime`](ak_session::runtime::SessionRuntime) whose
//!   `PairingTransport` ([`FfiPairingTransport`]) shares the runtime's
//!   control client ("调度与常驻接收使用同一个 runtime", ffi-v2 §1) and whose
//!   executor ([`FfiSendExecutor`]) runs REAL `ak-engine` transfers per
//!   admitted task, reporting the contract §9 phases back into the actor.
//!
//! Known scope cut, documented: the on-channel `TaskOpen`/`TaskAdmission`
//! wire handshake (`ak_engine::send_on_channel`) and the session-relayed
//! data path remain the adapter work ak-session's actor notes as wave-22+;
//! the executor here runs the frozen self-connecting
//! [`ak_engine::send`](ak_engine::send) body per task — a real QUIC/TLS
//! transfer of real bytes, scheduled by the real runtime. The wire-level
//! admission handshake follows when the on-channel executor lands.
//!
//! # Idempotency and `-3` discipline (ffi-v2 §2/§3)
//!
//! Op starts validate and parse the whole spec BEFORE any side effect (an
//! op id is only minted for a fully valid request); `ak_pairing_op_result`
//! reads the stored terminal JSON without consuming it, so an
//! `AK_ERR_BUFFER` retry is a lossless re-read of the SAME answer — a
//! create op never generates a second invite code. Events are only popped
//! when the caller buffer actually fits; a `-3` pushes the line back to
//! the front.
#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ak_core::error::ErrorCode;
use ak_engine::SendSpec;
use ak_identity::pairing_txn::random_id16;
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_session::pairing::{ClientConfig, OrchestratorConfig, PairingOrchestrator, RegistryClient};
use ak_session::runtime::{
    CodeId as RtCodeId, EngineStartError, InviteClaimRequest, InviteClaimed, OpenSessionInfo,
    PairingConfirmation, PairingError, PairingEvent, PairingFuture, PairingId as RtPairingId,
    PairingPlan, PairingReadyInfo, PairingTransport, ParkReason, ProgressSink, RealClock,
    RedeemAccepted, RedeemPayload, RuntimeClock, RuntimeConfig, SessionEvent, SessionHandle,
    SessionRuntime, ShutdownMode, SnapshotQuery, TaskView, TransferDirection, TransferExecutor,
    TransferSpec,
};
use ak_session::scheduler::config::{CrossCodeMode, SameCodeMode, SchedulerPolicy};
use ak_session::scheduler::key::{CodeGroupKey, CodeId, RegistryId, TaskHandle, TaskId};
use ak_session::scheduler::state::{FinishOutcome, SchedulerEvent, TaskPhase};
use ak_signal::protocol_v2::{InviteState, StateFilter, CURSOR_START};
use ak_store::PairingJournal;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::core::{self, FfiError};
use crate::AkEngine;

/// How long a synchronous scheduler round-trip (enqueue/cancel/snapshot —
/// in-memory command-channel answers, never network) may take before the
/// FFI call gives up with `AK_ERR_INTERNAL`. Generous on purpose: the only
/// realistic slow case is a wedged actor, which is exactly what the bound
/// exists to surface.
const SCHED_ROUNDTRIP: Duration = Duration::from_secs(5);

/// Bounded wait for the background registry connect of `ak_pairing_start`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Cap of one runtime's event queue (same budget as the progress queue).
const MAX_QUEUED_EVENTS: usize = 65_536;

// ---------------------------------------------------------------------------
// Engine-side state.
// ---------------------------------------------------------------------------

/// Everything `ak_pairing_*`/`ak_sched_*` hangs off one engine handle.
#[derive(Default)]
pub(crate) struct V2State {
    /// One entry per `ak_pairing_start`, keyed by runtime id.
    pub runtimes: HashMap<[u8; 16], Arc<PairingRt>>,
    /// The runtime `ak_sched_start` binds to when its spec names none: the
    /// most recently started one ("调度与常驻接收使用同一个 runtime").
    pub active: Option<[u8; 16]>,
    /// The single scheduler runtime (`ak_sched_start`).
    pub sched: Option<Arc<SchedRt>>,
}

/// Connection phase of one pairing runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtPhase {
    /// The background connect of `ak_pairing_start` is still running.
    Starting,
    /// The control client is live; ops may run.
    Ready,
    /// The connect failed permanently; the message is the bounded
    /// diagnostic ops surface.
    Failed(String),
}

/// The connected halves, installed exactly once by the background task.
#[derive(Clone)]
pub(crate) struct RtLive {
    client: RegistryClient,
    orch: PairingOrchestrator,
}

/// One pairing runtime (the `ak_pairing_start` artefact).
pub(crate) struct PairingRt {
    /// Runtime id crossing the ABI.
    pub(crate) runtime_id: [u8; 16],
    /// Identity directory: `device.key`, `peers.book`, `pairing/`,
    /// `checkpoints/`.
    pub(crate) id_dir: PathBuf,
    /// The device key loaded from `<id_dir>/device.key`.
    pub(crate) key: DeviceKeyPair,
    /// Fingerprint of [`PairingRt::key`] (rendered into results).
    pub(crate) device_fp: Fingerprint,
    /// Registry endpoint and pinned raw public key.
    pub(crate) registry_addr: std::net::SocketAddr,
    pub(crate) registry_id: [u8; 32],
    /// Connect phase; `Starting` until the background task settles.
    pub(crate) phase: watch::Sender<RtPhase>,
    /// The live client + orchestrator (`None` until `Ready`).
    pub(crate) live: StdMutex<Option<RtLive>>,
    /// The resident owner loop handle (abortable teardown).
    pub(crate) owner_loop: StdMutex<Option<JoinHandle<()>>>,
    /// Op table: op id → entry (entries stay readable after completion —
    /// the same forever-readable semantics as the job table).
    pub(crate) ops: StdMutex<HashMap<[u8; 16], Arc<OpEntry>>>,
    /// Pairing-plane event lines (schema in the crate header).
    pub(crate) events: Arc<StdMutex<VecDeque<String>>>,
}

impl PairingRt {
    /// Best-effort teardown on engine drop: abort the op tasks and the
    /// owner loop, then ask the client to close (polite close_notify).
    pub(crate) fn teardown(&self, runtime: &tokio::runtime::Runtime) {
        if let Ok(mut ops) = self.ops.lock() {
            for entry in ops.values() {
                if let Ok(mut slot) = entry.task.lock() {
                    if let Some(task) = slot.take() {
                        task.abort();
                    }
                }
            }
            ops.clear();
        }
        let owner = self.owner_loop.lock().ok().and_then(|mut slot| slot.take());
        if let Some(owner) = owner {
            owner.abort();
        }
        let live = self.live.lock().ok().and_then(|mut slot| slot.take());
        if let Some(live) = live {
            let client = live.client;
            runtime.spawn(async move {
                client.close().await;
            });
        }
    }

    fn push_event(&self, line: String) {
        if let Ok(mut queue) = self.events.lock() {
            push_capped(&mut queue, line);
        }
    }

    /// Wait (asynchronously) for the background connect to settle.
    async fn await_ready(&self) -> Result<(), String> {
        let mut rx = self.phase.subscribe();
        loop {
            match &*rx.borrow() {
                RtPhase::Ready => return Ok(()),
                RtPhase::Failed(message) => return Err(message.clone()),
                RtPhase::Starting => {}
            }
            if rx.changed().await.is_err() {
                return Err("pairing runtime state source closed".to_string());
            }
        }
    }
}

/// Terminal state of one op.
pub(crate) enum OpOutcome {
    Running { op: &'static str },
    Finished { ok: bool, json: String },
}

/// One async control operation (create/redeem/list/revoke).
pub(crate) struct OpEntry {
    pub outcome: StdMutex<OpOutcome>,
    /// The op task; taken (and aborted) by cancel/teardown.
    pub task: StdMutex<Option<JoinHandle<()>>>,
}

/// Result snapshot for `ak_pairing_op_result`.
pub(crate) enum OpResult {
    Running,
    Finished { ok: bool, json: String },
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn push_capped(queue: &mut VecDeque<String>, line: String) {
    if queue.len() < MAX_QUEUED_EVENTS {
        queue.push_back(line);
    }
}

fn quoted(text: &str) -> String {
    crate::core::json::quoted(text)
}

fn type_error(key: &str, wanted: &str) -> FfiError {
    FfiError::invalid(format!("field \"{key}\" must be {wanted}"))
}

/// Parse exactly 32 hex characters into 16 bytes (ids crossing the ABI).
fn hex16(text: &str, what: &str) -> Result<[u8; 16], FfiError> {
    let mut out = [0u8; 16];
    decode_hex_into(text, &mut out, what)?;
    Ok(out)
}

/// Parse exactly 64 hex characters into 32 bytes.
fn hex32(text: &str, what: &str) -> Result<[u8; 32], FfiError> {
    let mut out = [0u8; 32];
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

/// Look one runtime up by id (`-4` with the ABI's not-found wording).
fn runtime_of(engine: &AkEngine, runtime_id: &[u8; 16]) -> Result<Arc<PairingRt>, FfiError> {
    let v2 = engine
        .v2
        .lock()
        .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
    v2.runtimes.get(runtime_id).cloned().ok_or_else(|| {
        FfiError::not_found(format!("unknown runtime {}", core::hex_encode(runtime_id)))
    })
}

// ---------------------------------------------------------------------------
// Field readers (same policy as the send/receive spec readers in core).
// ---------------------------------------------------------------------------

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

fn field_bool(args: &crate::core::json::Json, key: &str) -> Result<Option<bool>, FfiError> {
    match args.get(key) {
        None | Some(crate::core::json::Json::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| type_error(key, "boolean")),
    }
}

// ---------------------------------------------------------------------------
// ak_pairing_start.
// ---------------------------------------------------------------------------

/// Start one pairing runtime: parse + validate the spec, load the device
/// key, register the runtime, and connect the control client in the
/// background. Returns the runtime id. Spec schema: see the crate header.
pub fn pairing_start(engine: &AkEngine, spec: &str) -> Result<[u8; 16], FfiError> {
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let registry_text = field_str(&args, "registry", true)?
        .ok_or_else(|| FfiError::invalid("missing \"registry\""))?;
    let registry_addr: std::net::SocketAddr = registry_text.parse().map_err(|_| {
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
    let device_name =
        field_str(&args, "device_name", false)?.unwrap_or_else(|| "ak-ffi".to_string());
    if device_name.chars().count() > 64 {
        return Err(FfiError::invalid(
            "field \"device_name\" must be at most 64 characters",
        ));
    }
    let owner_loop = field_bool(&args, "owner_loop")?.unwrap_or(true);
    let redeem_window_ms = field_u64(&args, "redeem_window_ms")?.unwrap_or(90_000);
    if !(1_000..=119_000).contains(&redeem_window_ms) {
        return Err(FfiError::invalid(
            "field \"redeem_window_ms\" must be between 1000 and 119000",
        ));
    }

    // Local, synchronous identity bootstrap (disk under id_dir, no network).
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
    let (phase, _) = watch::channel(RtPhase::Starting);

    let rt = Arc::new(PairingRt {
        runtime_id,
        id_dir: dir,
        key,
        device_fp,
        registry_addr,
        registry_id,
        phase,
        live: StdMutex::new(None),
        owner_loop: StdMutex::new(None),
        ops: StdMutex::new(HashMap::new()),
        events: Arc::new(StdMutex::new(VecDeque::new())),
    });
    {
        let mut v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        v2.runtimes.insert(runtime_id, Arc::clone(&rt));
        v2.active = Some(runtime_id);
    }

    // Background connect: no network wait inside this synchronous call
    // (ffi-v2 §2). Ops started meanwhile park on `await_ready`.
    engine.runtime().spawn(async move {
        let connect = RegistryClient::connect_with_config(
            rt.registry_addr,
            rt.registry_id,
            &rt.key,
            &device_name,
            ClientConfig::default(),
        );
        let client = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => return fail_start(&rt, &error.to_string()),
            Err(_) => return fail_start(&rt, "registry connect timed out"),
        };
        let config = OrchestratorConfig {
            redeem_window_ms,
            ..OrchestratorConfig::default()
        };
        let orch =
            match PairingOrchestrator::open(client.clone(), rt.key.clone(), &rt.id_dir, config) {
                Ok(orch) => orch,
                Err(error) => {
                    return fail_start(&rt, &format!("cannot open pairing state: {error}"));
                }
            };
        if owner_loop {
            let loop_handle = orch.spawn_owner_loop();
            if let Ok(mut slot) = rt.owner_loop.lock() {
                *slot = Some(loop_handle);
            }
        }
        if let Ok(mut slot) = rt.live.lock() {
            *slot = Some(RtLive {
                client,
                orch: orch.clone(),
            });
        }
        rt.phase.send_replace(RtPhase::Ready);
        rt.push_event(format!(
            "{{\"event\":\"runtime_ready\",\"runtime\":\"{}\"}}",
            core::hex_encode(&rt.runtime_id)
        ));
    });
    Ok(runtime_id)
}

fn fail_start(rt: &PairingRt, message: &str) {
    rt.phase.send_replace(RtPhase::Failed(message.to_string()));
    rt.push_event(format!(
        "{{\"event\":\"runtime_failed\",\"runtime\":\"{}\",\"error\":{}}}",
        core::hex_encode(&rt.runtime_id),
        quoted(message),
    ));
}

// ---------------------------------------------------------------------------
// Ops: create / redeem / list / revoke.
// ---------------------------------------------------------------------------

/// The parsed, fully validated body of one op (validation precedes any
/// side effect — an op id is only minted for a valid request).
enum OpSpec {
    Create {
        request_id: [u8; 16],
        ttl_secs: u32,
    },
    Redeem {
        invite_code: String,
    },
    List {
        cursor: [u8; 16],
        limit: u8,
        state_filter: StateFilter,
    },
    Revoke {
        code_id: [u8; 16],
    },
}

impl OpSpec {
    fn op(&self) -> &'static str {
        match self {
            OpSpec::Create { .. } => "create",
            OpSpec::Redeem { .. } => "redeem",
            OpSpec::List { .. } => "list",
            OpSpec::Revoke { .. } => "revoke",
        }
    }
}

/// Parse one op spec (all `-2` validation happens before any side effect).
fn parse_op_spec(op: &str, spec: &str) -> Result<OpSpec, FfiError> {
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let request_id = match field_str(&args, "request_id", false)? {
        Some(text) => hex16(&text, "field \"request_id\"")?,
        None => random_id16(),
    };
    match op {
        "create" => {
            let ttl_secs = field_u64(&args, "ttl_secs")?.unwrap_or(0);
            if ttl_secs > u64::from(u32::MAX) {
                return Err(FfiError::invalid("field \"ttl_secs\" is too large"));
            }
            Ok(OpSpec::Create {
                request_id,
                ttl_secs: ttl_secs as u32,
            })
        }
        "redeem" => {
            let invite_code = field_str(&args, "invite_code", true)?
                .ok_or_else(|| FfiError::invalid("missing \"invite_code\""))?;
            if invite_code.is_empty() {
                return Err(FfiError::invalid("field \"invite_code\" must be non-empty"));
            }
            Ok(OpSpec::Redeem { invite_code })
        }
        "list" => {
            let cursor = match field_str(&args, "cursor", false)? {
                Some(text) if !text.is_empty() => hex16(&text, "field \"cursor\"")?,
                _ => CURSOR_START,
            };
            let limit = field_u64(&args, "limit")?.unwrap_or(32);
            if !(1..=32).contains(&limit) {
                return Err(FfiError::invalid(
                    "field \"limit\" must be between 1 and 32",
                ));
            }
            let filter_text =
                field_str(&args, "state_filter", false)?.unwrap_or_else(|| "active".to_string());
            let state_filter = match filter_text.as_str() {
                "active" => StateFilter::ACTIVE,
                "revoked" => StateFilter::REVOKED,
                "expired" => StateFilter::EXPIRED,
                "all" => StateFilter::ALL,
                other => {
                    return Err(FfiError::invalid(format!(
                        "field \"state_filter\" must be active|revoked|expired|all, got {other:?}"
                    )));
                }
            };
            Ok(OpSpec::List {
                cursor,
                limit: limit as u8,
                state_filter,
            })
        }
        "revoke" => {
            let code_hex = field_str(&args, "code_id", true)?
                .ok_or_else(|| FfiError::invalid("missing \"code_id\""))?;
            Ok(OpSpec::Revoke {
                code_id: hex16(&code_hex, "field \"code_id\"")?,
            })
        }
        other => Err(FfiError::internal(format!("unknown op {other:?}"))),
    }
}

/// Shared op launcher: validate the spec, register the op, spawn the body
/// on the embedded runtime. The body first awaits the runtime's connect
/// phase, then runs the orchestrator/client call and stamps the terminal
/// JSON (kept for lossless repeated reads).
fn launch_op(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_spec: OpSpec,
) -> Result<[u8; 16], FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    let op = op_spec.op();
    let op_id = random_id16();
    let entry = Arc::new(OpEntry {
        outcome: StdMutex::new(OpOutcome::Running { op }),
        task: StdMutex::new(None),
    });
    {
        let mut ops = rt
            .ops
            .lock()
            .map_err(|_| FfiError::internal("op table lock poisoned"))?;
        ops.insert(op_id, Arc::clone(&entry));
    }
    let task = {
        let entry = Arc::clone(&entry);
        engine.runtime().spawn(async move {
            let outcome = run_op(&rt, op_spec).await;
            let (ok, json) = match outcome {
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
            rt.push_event(if ok {
                format!(
                    "{{\"event\":\"op_done\",\"op_id\":\"{}\",\"op\":{}}}",
                    core::hex_encode(&op_id),
                    quoted(op),
                )
            } else {
                format!(
                    "{{\"event\":\"op_failed\",\"op_id\":\"{}\",\"op\":{}}}",
                    core::hex_encode(&op_id),
                    quoted(op),
                )
            });
        })
    };
    if let Ok(mut slot) = entry.task.lock() {
        *slot = Some(task);
    }
    Ok(op_id)
}

/// One op body: await the connect phase, then run the operation.
async fn run_op(rt: &PairingRt, op_spec: OpSpec) -> Result<String, String> {
    if let Err(message) = rt.await_ready().await {
        return Err(format!("pairing runtime not connected: {message}"));
    }
    let (client, orch) = {
        let live = rt
            .live
            .lock()
            .map_err(|_| "runtime state lock poisoned".to_string())?;
        let live = live.as_ref().ok_or("runtime is not connected")?;
        (live.client.clone(), live.orch.clone())
    };
    match op_spec {
        OpSpec::Create {
            request_id,
            ttl_secs,
        } => {
            let created = orch
                .create_invite_with_request_id(request_id, ttl_secs)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "{{\"op\":\"create\",\"code\":{},\"hint\":{},\"code_id\":\"{}\",\"expires_ms\":{},\"revision\":{}}}",
                quoted(&created.display),
                quoted(&created.hint),
                core::hex_encode(&created.code_id),
                created.expires_ms,
                created.revision,
            ))
        }
        OpSpec::Redeem { invite_code } => {
            let verified = orch
                .redeem(&invite_code)
                .await
                .map_err(|error| error.to_string())?;
            let record = verified.record();
            let me = client.device_id();
            let peer_device = if record.transcript.owner_device_id == me {
                record.transcript.redeemer_device_id
            } else {
                record.transcript.owner_device_id
            };
            let peer_fp = Fingerprint::from_public_key(&peer_device);
            Ok(format!(
                "{{\"op\":\"redeem\",\"pairing_id\":\"{}\",\"code_id\":\"{}\",\"peer\":{},\"peer_device\":\"{}\",\"book\":{}}}",
                core::hex_encode(&verified.pairing_id()),
                core::hex_encode(&record.transcript.code_id),
                quoted(&peer_fp.to_hex()),
                core::hex_encode(&peer_device),
                quoted(&rt.id_dir.join("peers.book").to_string_lossy()),
            ))
        }
        OpSpec::List {
            cursor,
            limit,
            state_filter,
        } => {
            let (next_cursor, entries) = client
                .invite_list(cursor, limit, state_filter)
                .await
                .map_err(|error| error.to_string())?;
            let mut lines = String::from("[");
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    lines.push(',');
                }
                lines.push_str(&format!(
                    "{{\"code_id\":\"{}\",\"state\":{},\"created_ms\":{},\"expires_ms\":{},\"revision\":{}}}",
                    core::hex_encode(&entry.code_id),
                    quoted(invite_state(&entry.state)),
                    entry.created_ms,
                    entry.expires_ms,
                    entry.revision,
                ));
            }
            lines.push(']');
            Ok(format!(
                "{{\"op\":\"list\",\"next_cursor\":\"{}\",\"entries\":{lines}}}",
                core::hex_encode(&next_cursor),
            ))
        }
        OpSpec::Revoke { code_id } => {
            let revoked = orch
                .revoke(code_id)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "{{\"op\":\"revoke\",\"code_id\":\"{}\",\"revision\":{},\"revoked_ms\":{}}}",
                core::hex_encode(&revoked.code_id),
                revoked.revision,
                revoked.revoked_ms,
            ))
        }
    }
}

fn invite_state(state: &InviteState) -> &'static str {
    match state {
        InviteState::Active => "active",
        InviteState::Revoked => "revoked",
        InviteState::Expired => "expired",
    }
}

/// Start a create op (spec schema in the crate header).
pub fn pairing_create_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let op_spec = parse_op_spec("create", spec)?;
    launch_op(engine, runtime_id, op_spec)
}

/// Start a redeem op.
pub fn pairing_redeem_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let op_spec = parse_op_spec("redeem", spec)?;
    launch_op(engine, runtime_id, op_spec)
}

/// Start a list op.
pub fn pairing_list_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    query: &str,
) -> Result<[u8; 16], FfiError> {
    let op_spec = parse_op_spec("list", query)?;
    launch_op(engine, runtime_id, op_spec)
}

/// Start a revoke op.
pub fn pairing_revoke_start(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> Result<[u8; 16], FfiError> {
    let op_spec = parse_op_spec("revoke", spec)?;
    launch_op(engine, runtime_id, op_spec)
}

/// Read one op's terminal state. Reading consumes nothing: repeated calls
/// return the same JSON until the runtime goes away.
pub fn pairing_op_result(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<OpResult, FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    let ops = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("op table lock poisoned"))?;
    let entry = ops.get(op_id).ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown op {} on runtime {}",
            core::hex_encode(op_id),
            core::hex_encode(runtime_id),
        ))
    })?;
    let outcome = entry
        .outcome
        .lock()
        .map_err(|_| FfiError::internal("op outcome lock poisoned"))?;
    Ok(match &*outcome {
        OpOutcome::Running { .. } => OpResult::Running,
        OpOutcome::Finished { ok, json } => OpResult::Finished {
            ok: *ok,
            json: json.clone(),
        },
    })
}

/// Pop the oldest pairing event line (`None` when the queue is empty).
pub fn pairing_poll_event(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
) -> Result<Option<String>, FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    let mut queue = rt
        .events
        .lock()
        .map_err(|_| FfiError::internal("event queue lock poisoned"))?;
    Ok(queue.pop_front())
}

/// Push one event line back to the front (the shim does this after an
/// `AK_ERR_BUFFER`, so a short caller buffer never loses an event).
pub fn pairing_push_event_front(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    line: String,
) -> Result<(), FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    let mut queue = rt
        .events
        .lock()
        .map_err(|_| FfiError::internal("event queue lock poisoned"))?;
    queue.push_front(line);
    Ok(())
}

/// Cancel one op: abort the task and stamp a terminal failure. An op that
/// already finished is left untouched.
pub fn pairing_cancel_op(
    engine: &AkEngine,
    runtime_id: &[u8; 16],
    op_id: &[u8; 16],
) -> Result<(), FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    let ops = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("op table lock poisoned"))?;
    let entry = ops.get(op_id).ok_or_else(|| {
        FfiError::not_found(format!(
            "unknown op {} on runtime {}",
            core::hex_encode(op_id),
            core::hex_encode(runtime_id),
        ))
    })?;
    let task = entry
        .task
        .lock()
        .map_err(|_| FfiError::internal("op task lock poisoned"))?
        .take();
    if let Some(task) = task {
        task.abort();
    }
    let mut outcome = entry
        .outcome
        .lock()
        .map_err(|_| FfiError::internal("op outcome lock poisoned"))?;
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
// Scheduler: ak_sched_*.
// ---------------------------------------------------------------------------

/// The single scheduler runtime of one engine (see the module docs).
pub(crate) struct SchedRt {
    /// The pairing runtime the scheduler shares its client with.
    pub runtime_id: [u8; 16],
    pub id_dir: PathBuf,
    pub handle: SessionHandle,
    pub executor: Arc<FfiSendExecutor>,
    pub(crate) events: Arc<StdMutex<VecDeque<String>>>,
    /// Event pump task handle (broadcast → bounded queue).
    pump: StdMutex<Option<JoinHandle<()>>>,
}

impl SchedRt {
    pub(crate) fn teardown(&self, runtime: &tokio::runtime::Runtime) {
        self.executor.abort_all();
        let pump = self.pump.lock().ok().and_then(|mut slot| slot.take());
        if let Some(pump) = pump {
            pump.abort();
        }
        // Bounded best-effort shutdown; dropping the handle afterwards
        // closes the command channel, which also ends the actor.
        let handle = self.handle.clone();
        let _ = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(2),
                handle.shutdown(ShutdownMode::Cancel),
            )
            .await
        });
    }
}

/// The real per-task transfer executor: runs `ak_engine::send` for every
/// admitted task, reports the contract §9 phases through the
/// [`ProgressSink`], and forwards engine progress into the engine's
/// progress queue (visible via `ak_poll_progress` with the task id as the
/// `job` field).
///
/// Bookkeeping entries (cancel signals, join handles) live for the
/// engine's lifetime — the same forever-visible semantics as the job
/// table; task ids are fresh CSPRNG draws, so a stale entry can never be
/// mistaken for a live task.
pub(crate) struct FfiSendExecutor {
    handle: tokio::runtime::Handle,
    /// Engine send specs registered by `sched_enqueue`, keyed by task id
    /// and consumed by the task body.
    specs: StdMutex<HashMap<[u8; 16], SendSpec>>,
    /// Cancellation signals of started tasks.
    cancels: StdMutex<HashMap<[u8; 16], watch::Sender<bool>>>,
    /// Join handles of started tasks (teardown aborts them).
    tasks: StdMutex<HashMap<[u8; 16], JoinHandle<()>>>,
    /// The engine-wide progress queue (`ak_poll_progress`).
    progress: Arc<StdMutex<VecDeque<String>>>,
}

impl FfiSendExecutor {
    fn abort_all(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        if let Ok(mut cancels) = self.cancels.lock() {
            for (_, cancel) in cancels.drain() {
                let _ = cancel.send(true);
            }
        }
    }
}

impl TransferExecutor for FfiSendExecutor {
    fn enqueue(
        &self,
        task: TaskHandle,
        _spec: TransferSpec,
        sink: ProgressSink,
    ) -> Result<(), EngineStartError> {
        let id = *task.task_id().as_bytes();
        let engine_spec = self
            .specs
            .lock()
            .ok()
            .and_then(|mut specs| specs.remove(&id))
            .ok_or_else(|| {
                EngineStartError::new(
                    ErrorCode::Internal,
                    "no engine send spec registered for the task",
                )
            })?;
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.insert(id, cancel_tx);
        }
        let progress = Arc::clone(&self.progress);
        let task_hex = core::hex_encode(&id);
        let join = self.handle.spawn(async move {
            sink.phase(task, TaskPhase::Scanning).await;
            sink.phase(task, TaskPhase::Transferring).await;
            let callback = move |event: ak_engine::Progress| {
                core::push_event(&progress, &task_hex, None, &event);
            };
            // Dropping the send future on cancel aborts the transfer and
            // releases its endpoint; the report then finalizes the task.
            let outcome = tokio::select! {
                _ = cancel_rx.changed(), if *cancel_rx.borrow() => {
                    FinishOutcome::Failed { error: ErrorCode::Cancelled }
                }
                report = ak_engine::send(engine_spec, &callback) => {
                    match report {
                        Ok(_) => FinishOutcome::Completed,
                        Err(error) => FinishOutcome::Failed {
                            error: engine_error_code(&error),
                        },
                    }
                }
            };
            if matches!(outcome, FinishOutcome::Completed) {
                sink.phase(task, TaskPhase::Verifying).await;
                sink.phase(task, TaskPhase::Committing).await;
            }
            sink.finished(task, outcome).await;
        });
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(id, join);
        }
        Ok(())
    }

    fn cancel(&self, task: &TaskHandle) {
        let id = *task.task_id().as_bytes();
        if let Ok(mut cancels) = self.cancels.lock() {
            if let Some(cancel) = cancels.remove(&id) {
                let _ = cancel.send(true);
            }
        }
    }
}

/// The B8 transport of the scheduler runtime: one [`PairingTransport`]
/// over the runtime's shared [`RegistryClient`].
///
/// Scope, documented honestly: the invite/redeem transcript flows of the
/// trait are REFUSED here — they are the orchestrator's transactional
/// business (the `ak_pairing_*` ops own the journal, the trust book and
/// the invite secrets), so scheduler tasks must arrive with an
/// already-applied pairing (`PairingPlan::Ready`), which is exactly what
/// [`sched_enqueue`] submits. `open_session` enforces the relation check
/// this revision can drive end to end: the pairing must exist in the
/// local journal under the SAME code group with phase `applied` — the
/// local mirror of the registry's `relation_exists` gate. The registry
/// session relay itself (`ConnectRequest` → `SessionOpened`) cannot be
/// driven through `RegistryClient` yet: its dispatcher never routes
/// `SessionOpened` to the `connect_peer` waiter (a wave-21 gap in
/// `ak-session`'s `pairing/client.rs` that crate's owner must fix), so
/// the FFI gates locally until then. The data transfer is REAL
/// regardless — `ak-engine` QUIC end to end.
struct FfiPairingTransport {
    client: RegistryClient,
    journal_dir: PathBuf,
}

impl PairingTransport for FfiPairingTransport {
    fn claim_invite(&self, _claim: InviteClaimRequest) -> PairingFuture<'_, InviteClaimed> {
        Box::pin(async {
            Err(PairingError::rejected(
                "invite registration runs through the ak_pairing_* ops, not the scheduler",
            ))
        })
    }

    fn redeem(&self, _request: RedeemPayload) -> PairingFuture<'_, RedeemAccepted> {
        Box::pin(async {
            Err(PairingError::rejected(
                "pairing transcripts run through the ak_pairing_* ops, not the scheduler",
            ))
        })
    }

    fn confirm_pairing(&self, _c: PairingConfirmation) -> PairingFuture<'_, PairingReadyInfo> {
        Box::pin(async {
            Err(PairingError::rejected(
                "pairing transcripts run through the ak_pairing_* ops, not the scheduler",
            ))
        })
    }

    fn open_session(
        &self,
        pairing_id: RtPairingId,
        code_id: RtCodeId,
    ) -> PairingFuture<'_, OpenSessionInfo> {
        let dir = self.journal_dir.join("pairing");
        let want_pairing = *pairing_id.as_bytes();
        let want_code = *code_id.as_bytes();
        Box::pin(async move {
            let journal = PairingJournal::open(&dir)
                .map_err(|error| PairingError::rejected(&format!("journal: {error}")))?;
            let records = journal
                .load_all()
                .map_err(|error| PairingError::rejected(&format!("journal: {error}")))?;
            let record = records
                .iter()
                .find(|record| record.pairing_id == want_pairing)
                .ok_or_else(|| {
                    PairingError::rejected("no such pairing in this device's journal")
                })?;
            if record.code_id != want_code {
                return Err(PairingError::rejected(
                    "pairing belongs to a different code group",
                ));
            }
            if record.phase != ak_store::PairingPhase::Applied {
                return Err(PairingError::rejected(
                    "pairing is not applied on this device yet",
                ));
            }
            Ok(OpenSessionInfo {
                session_id: random_id16(),
                expires_ms: now_unix_ms().saturating_add(120_000),
            })
        })
    }

    fn next_pairing_event(&self) -> PairingFuture<'_, PairingEvent> {
        // The resident listener is disabled for this runtime (the
        // orchestrator's owner loop answers pairing offers); park forever
        // so a hypothetical listener would block instead of spinning.
        Box::pin(std::future::pending())
    }

    fn is_connected(&self) -> bool {
        self.client.is_connected()
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// Best-effort stable-code mapping of an engine failure for the
/// scheduler's `FinishOutcome::Failed` record.
fn engine_error_code(error: &ak_engine::EngineError) -> ErrorCode {
    use ak_engine::EngineError;
    match error {
        EngineError::Rejected(code) | EngineError::PeerError { code, .. } => *code,
        EngineError::Connect(_) | EngineError::PeerClosed | EngineError::IdleTimeout(_) => {
            ErrorCode::DirectUnavailable
        }
        EngineError::SourceChanged => ErrorCode::SourceChanged,
        EngineError::Storage(_) | EngineError::Checkpoint(_) => ErrorCode::StorageError,
        EngineError::StorageFull { .. } => ErrorCode::StorageFull,
        EngineError::VerificationFailed => ErrorCode::HashMismatch,
        EngineError::RouteForbidden(_) => ErrorCode::RouteForbidden,
        EngineError::Cancelled => ErrorCode::Cancelled,
        _ => ErrorCode::Internal,
    }
}

/// `ak_sched_start`: bind the real session runtime to one pairing runtime
/// (spec schema in the crate header). Requires the pairing runtime to be
/// connected — poll the `runtime_ready` event first.
pub fn sched_start(engine: &AkEngine, spec: &str) -> Result<(), FfiError> {
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let runtime_id = match field_str(&args, "runtime_id", false)? {
        Some(text) => hex16(&text, "field \"runtime_id\"")?,
        None => {
            let v2 = engine
                .v2
                .lock()
                .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
            v2.active.ok_or_else(|| {
                FfiError::not_found("no pairing runtime: call ak_pairing_start first")
            })?
        }
    };
    let rt = runtime_of(engine, &runtime_id)?;
    let policy = parse_policy(&args)?;

    let ready = {
        let phase = rt.phase.subscribe();
        let borrowed = phase.borrow();
        matches!(&*borrowed, RtPhase::Ready)
    };
    if !ready {
        return Err(FfiError::invalid(
            "the pairing runtime is not connected yet (poll the runtime_ready event first)",
        ));
    }
    let client = {
        let live = rt
            .live
            .lock()
            .map_err(|_| FfiError::internal("runtime state lock poisoned"))?;
        live.as_ref()
            .map(|live| live.client.clone())
            .ok_or_else(|| FfiError::invalid("the pairing runtime is not connected"))?
    };

    {
        let v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        if v2.sched.is_some() {
            return Err(FfiError::invalid(
                "a scheduler runtime is already started on this engine",
            ));
        }
    }

    let executor = Arc::new(FfiSendExecutor {
        handle: engine.runtime().handle().clone(),
        specs: StdMutex::new(HashMap::new()),
        cancels: StdMutex::new(HashMap::new()),
        tasks: StdMutex::new(HashMap::new()),
        progress: engine.events_queue(),
    });
    let sched = engine.runtime().block_on(async {
        // The B8 transport shares the runtime's live control client; its
        // open_session gates on the local journal (see FfiPairingTransport
        // for the documented wave-21 client-gap rationale).
        let transport = Arc::new(FfiPairingTransport {
            client: client.clone(),
            journal_dir: rt.id_dir.clone(),
        });
        let clock: Arc<dyn RuntimeClock> = Arc::new(RealClock::new());
        let mut config = RuntimeConfig::new(
            policy,
            clock,
            transport,
            executor.clone(),
            RegistryId::from_bytes(client.registry_id()),
        );
        // The orchestrator's resident owner loop already answers pairing
        // offers transactionally; the runtime listener's inbound receive
        // submission stays disabled until the on-channel receive executor
        // lands (documented scope cut).
        config.listener_enabled = false;
        let handle = SessionRuntime::start(config).map_err(|error| {
            FfiError::internal(format!("cannot start scheduler runtime: {error}"))
        })?;
        let events: Arc<StdMutex<VecDeque<String>>> = Arc::new(StdMutex::new(VecDeque::new()));
        let mut subscription = handle.subscribe();
        let queue = Arc::clone(&events);
        let pump = tokio::spawn(async move {
            loop {
                match subscription.recv().await {
                    Ok(event) => {
                        if let Ok(mut queue) = queue.lock() {
                            push_capped(&mut queue, sched_event_json(&event));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok::<SchedRt, FfiError>(SchedRt {
            runtime_id,
            id_dir: rt.id_dir.clone(),
            handle,
            executor,
            events,
            pump: StdMutex::new(Some(pump)),
        })
    })?;

    let mut v2 = engine
        .v2
        .lock()
        .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
    if v2.sched.is_some() {
        return Err(FfiError::invalid(
            "a scheduler runtime is already started on this engine",
        ));
    }
    v2.sched = Some(Arc::new(sched));
    Ok(())
}

/// Parse a [`SchedulerPolicy`] from the shared field set (used by
/// `ak_sched_start` and `ak_sched_set_policy`).
fn parse_policy(args: &crate::core::json::Json) -> Result<SchedulerPolicy, FfiError> {
    let same = match field_str(args, "same_code", false)?.as_deref() {
        None | Some("serial") => SameCodeMode::Serial,
        Some("parallel") => SameCodeMode::Parallel,
        Some(other) => {
            return Err(FfiError::invalid(format!(
                "field \"same_code\" must be serial or parallel, got {other:?}"
            )));
        }
    };
    let cross = match field_str(args, "cross_code", false)?.as_deref() {
        None | Some("parallel") => CrossCodeMode::Parallel,
        Some("serial") => CrossCodeMode::Serial,
        Some(other) => {
            return Err(FfiError::invalid(format!(
                "field \"cross_code\" must be serial or parallel, got {other:?}"
            )));
        }
    };
    let defaults = SchedulerPolicy::default();
    let per_code = field_u64(args, "max_active_tasks_per_code")?
        .unwrap_or(u64::from(defaults.max_active_tasks_per_code));
    let groups = field_u64(args, "max_active_code_groups")?
        .unwrap_or(u64::from(defaults.max_active_code_connections));
    let queued =
        field_u64(args, "max_queued_tasks")?.unwrap_or(u64::from(defaults.max_queued_tasks));
    if per_code > u64::from(u16::MAX)
        || groups > u64::from(u16::MAX)
        || queued > u64::from(u16::MAX)
    {
        return Err(FfiError::invalid("scheduler policy limits are too large"));
    }
    SchedulerPolicy::new(same, cross, per_code as u32, groups as u32, queued as u32)
        .map_err(|error| FfiError::invalid(format!("invalid scheduler policy: {error}")))
}

fn sched_of(engine: &AkEngine) -> Result<Arc<SchedRt>, FfiError> {
    let v2 = engine
        .v2
        .lock()
        .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
    v2.sched
        .clone()
        .ok_or_else(|| FfiError::not_found("no scheduler runtime: call ak_sched_start first"))
}

/// `ak_sched_enqueue`: resolve the pairing relation locally, register the
/// engine send spec, submit the task. Returns the task id.
pub fn sched_enqueue(engine: &AkEngine, spec: &str) -> Result<[u8; 16], FfiError> {
    let sched = sched_of(engine)?;
    let rt = runtime_of(engine, &sched.runtime_id)?;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let pairing_hex = field_str(&args, "pairing_id", true)?
        .ok_or_else(|| FfiError::invalid("missing \"pairing_id\""))?;
    let pairing_id = hex16(&pairing_hex, "field \"pairing_id\"")?;
    let roots = match args.get("source_roots") {
        None | Some(crate::core::json::Json::Null) => {
            return Err(FfiError::invalid("missing required field \"source_roots\""));
        }
        Some(value) => value
            .as_str_array()
            .ok_or_else(|| type_error("source_roots", "an array of strings"))?,
    };
    if roots.is_empty() {
        return Err(FfiError::invalid(
            "field \"source_roots\" must not be empty",
        ));
    }
    if roots.iter().any(|root| root.is_empty()) {
        return Err(FfiError::invalid(
            "entries of \"source_roots\" must be non-empty paths",
        ));
    }
    let peer_text =
        field_str(&args, "peer", true)?.ok_or_else(|| FfiError::invalid("missing \"peer\""))?;
    let peer: std::net::SocketAddr = peer_text.parse().map_err(|_| {
        FfiError::invalid(format!(
            "field \"peer\" is not an ip:port address: {peer_text:?}"
        ))
    })?;

    // Resolve the pairing relation from this runtime's journal: the code
    // group and the verified peer key. The UI cannot override either
    // (ffi-v2 §4: "code_id 与真实对端从已保存的 pairing_id 关系解析").
    let journal = PairingJournal::open(&sched.id_dir.join("pairing"))
        .map_err(|error| FfiError::invalid(format!("cannot open the pairing journal: {error}")))?;
    let records = journal
        .load_all()
        .map_err(|error| FfiError::invalid(format!("cannot read the pairing journal: {error}")))?;
    let record = records
        .iter()
        .find(|record| record.pairing_id == pairing_id)
        .ok_or_else(|| {
            FfiError::not_found(format!(
                "pairing {} is not in this runtime's journal",
                core::hex_encode(&pairing_id)
            ))
        })?;
    let peer_public_key = record.peer_public_key.ok_or_else(|| {
        FfiError::invalid(format!(
            "pairing {} has no verified peer key yet",
            core::hex_encode(&pairing_id)
        ))
    })?;
    let pinned = Fingerprint::from_public_key(&peer_public_key);
    let group_key = CodeGroupKey::new(
        RegistryId::from_bytes(rt.registry_id),
        CodeId::from_bytes(record.code_id),
    );

    // Build the engine spec the executor will run.
    let bundle = make_self_signed(&rt.key);
    let mut engine_spec = SendSpec::new(PathBuf::from(&roots[0]), peer, pinned, bundle);
    for extra in &roots[1..] {
        engine_spec.add_root(PathBuf::from(extra));
    }
    engine_spec.checkpoint_dir = sched.id_dir.join("checkpoints");
    engine_spec.device_name = format!("ak-ffi-{}", rt.device_fp.short_id());
    if let Some(text) = field_str(&args, "conflict", false)? {
        engine_spec.conflict = parse_conflict(&text)?;
    }
    if let Some(multistream) = field_bool(&args, "multistream")? {
        engine_spec.multistream = multistream;
    }
    if let Some(no_internet) = field_bool(&args, "no_internet")? {
        engine_spec.policy = ak_core::policy::DirectPolicy {
            allow_internet: !no_internet,
            ..ak_core::policy::DirectPolicy::default()
        };
    }
    if let Some(secs) = field_u64(&args, "timeout_secs")? {
        engine_spec.idle_timeout = parse_timeout(secs)?;
    }

    let task_id = random_id16();
    sched
        .executor
        .specs
        .lock()
        .map_err(|_| FfiError::internal("executor lock poisoned"))?
        .insert(task_id, engine_spec);
    let label = std::path::Path::new(&roots[0])
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| roots[0].clone());
    let transfer = TransferSpec {
        group_key,
        pairing: PairingPlan::Ready {
            pairing_id: RtPairingId::from_bytes(pairing_id),
        },
        executor: sched.executor.clone(),
        direction: TransferDirection::Send,
        label,
        task_id: Some(TaskId::from_bytes(task_id)),
    };
    let submitted = engine.runtime().block_on(async {
        match tokio::time::timeout(SCHED_ROUNDTRIP, sched.handle.enqueue(transfer)).await {
            Ok(result) => result.map_err(map_session_error),
            Err(_) => Err(FfiError::internal("scheduler round-trip timed out")),
        }
    });
    match submitted {
        Ok(id) => {
            debug_assert_eq!(*id.as_bytes(), task_id);
            Ok(task_id)
        }
        Err(error) => {
            // Nothing was admitted: forget the registered spec too.
            if let Ok(mut specs) = sched.executor.specs.lock() {
                specs.remove(&task_id);
            }
            Err(error)
        }
    }
}

/// `ak_sched_set_policy`.
pub fn sched_set_policy(engine: &AkEngine, policy_json: &str) -> Result<(), FfiError> {
    let sched = sched_of(engine)?;
    let args = crate::core::json::parse(policy_json).map_err(FfiError::invalid)?;
    let policy = parse_policy(&args)?;
    let switched = engine.runtime().block_on(async {
        match tokio::time::timeout(SCHED_ROUNDTRIP, sched.handle.set_policy(policy)).await {
            Ok(result) => result.map_err(map_session_error),
            Err(_) => Err(FfiError::internal("scheduler round-trip timed out")),
        }
    });
    switched?;
    Ok(())
}

/// `ak_sched_cancel`.
pub fn sched_cancel(engine: &AkEngine, task_id: &[u8; 16]) -> Result<(), FfiError> {
    let sched = sched_of(engine)?;
    let receipt = engine.runtime().block_on(async {
        match tokio::time::timeout(
            SCHED_ROUNDTRIP,
            sched.handle.cancel(TaskId::from_bytes(*task_id)),
        )
        .await
        {
            Ok(result) => result.map_err(map_session_error),
            Err(_) => Err(FfiError::internal("scheduler round-trip timed out")),
        }
    });
    receipt?;
    Ok(())
}

/// `ak_sched_snapshot`: the whole runtime as one JSON document.
pub fn sched_snapshot(engine: &AkEngine) -> Result<String, FfiError> {
    let sched = sched_of(engine)?;
    let snapshot = engine.runtime().block_on(async {
        match tokio::time::timeout(SCHED_ROUNDTRIP, sched.handle.snapshot(SnapshotQuery::all()))
            .await
        {
            Ok(result) => result.map_err(map_session_error),
            Err(_) => Err(FfiError::internal("scheduler round-trip timed out")),
        }
    })?;
    render_snapshot(&snapshot)
}

/// `ak_sched_poll_event`: pop the oldest scheduler event line.
pub fn sched_poll_event(engine: &AkEngine) -> Result<Option<String>, FfiError> {
    let sched = sched_of(engine)?;
    let mut queue = sched
        .events
        .lock()
        .map_err(|_| FfiError::internal("event queue lock poisoned"))?;
    Ok(queue.pop_front())
}

/// Push one scheduler event back (after an `AK_ERR_BUFFER`).
pub fn sched_push_event_front(engine: &AkEngine, line: String) -> Result<(), FfiError> {
    let sched = sched_of(engine)?;
    let mut queue = sched
        .events
        .lock()
        .map_err(|_| FfiError::internal("event queue lock poisoned"))?;
    queue.push_front(line);
    Ok(())
}

fn map_session_error(error: ak_session::runtime::SessionError) -> FfiError {
    use ak_session::runtime::SessionError;
    match error {
        SessionError::TaskNotFound | SessionError::NotActive => {
            FfiError::not_found(error.to_string())
        }
        SessionError::QueueFull
        | SessionError::DuplicateTask
        | SessionError::ShuttingDown
        | SessionError::AlreadyShuttingDown
        | SessionError::InvalidSpec { .. }
        | SessionError::NotInRuntime => FfiError::invalid(error.to_string()),
        other => FfiError::internal(other.to_string()),
    }
}

fn render_snapshot(snapshot: &ak_session::runtime::SessionSnapshot) -> Result<String, FfiError> {
    let mut out = String::with_capacity(256);
    out.push_str(&format!(
        "{{\"policy\":{{\"same_code\":{},\"cross_code\":{},\"max_active_tasks_per_code\":{},\"max_active_code_groups\":{},\"max_queued_tasks\":{}}}",
        quoted(snapshot.policy.same_code_mode.as_str()),
        quoted(snapshot.policy.cross_code_mode.as_str()),
        snapshot.policy.max_active_tasks_per_code,
        snapshot.policy.max_active_code_connections,
        snapshot.policy.max_queued_tasks,
    ));
    out.push_str(&format!(
        ",\"policy_revision\":{},\"draining\":{},\"active_tasks\":{},\"queued_tasks\":{},\"shutting_down\":{}",
        snapshot.policy_revision, snapshot.draining, snapshot.active_tasks, snapshot.queued_tasks, snapshot.shutting_down,
    ));
    out.push_str(&format!(
        ",\"listener\":{{\"enabled\":{},\"running\":{},\"inbound_submitted\":{},\"inbound_rejected_queue_full\":{}}}",
        snapshot.listener.enabled,
        snapshot.listener.running,
        snapshot.listener.inbound_submitted,
        snapshot.listener.inbound_rejected_queue_full,
    ));
    out.push_str(",\"tasks\":[");
    for (index, task) in snapshot.tasks.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&render_task(task));
    }
    out.push_str("]}");
    Ok(out)
}

fn render_task(task: &TaskView) -> String {
    let last_error = match &task.last_error {
        Some(code) => quoted(&code.to_string()),
        None => "null".to_string(),
    };
    let parked = match &task.parked {
        Some(reason) => quoted(park_reason(reason)),
        None => "null".to_string(),
    };
    format!(
        "{{\"id\":\"{}\",\"state\":{},\"group\":\"{}\",\"registry\":\"{}\",\"direction\":\"{}\",\"label\":{},\"attempts\":{},\"last_error\":{last_error},\"cancel_requested\":{},\"parked\":{parked}}}",
        core::hex_encode(task.task.as_bytes()),
        quoted(task.state.as_str()),
        task.group.code_id.to_hex(),
        task.group.registry_id.to_hex(),
        match task.direction {
            TransferDirection::Send => "send",
            TransferDirection::Receive => "receive",
        },
        quoted(&task.label),
        task.attempts,
        task.cancel_requested,
    )
}

fn park_reason(reason: &ParkReason) -> &'static str {
    match reason {
        ParkReason::RetryExhausted => "retry_exhausted",
        ParkReason::QueueFull => "queue_full",
    }
}

/// Render one [`SessionEvent`] as a compact event line (schema in the
/// crate header).
fn sched_event_json(event: &SessionEvent) -> String {
    match event {
        SessionEvent::Scheduler(inner) => scheduler_event_json(inner),
        SessionEvent::BackoffScheduled {
            task,
            attempt,
            until_ms,
        } => format!(
            "{{\"event\":\"backoff_scheduled\",\"task\":\"{}\",\"attempt\":{attempt},\"until_ms\":{until_ms}}}",
            core::hex_encode(task.as_bytes()),
        ),
        SessionEvent::Retrying { task, attempt } => format!(
            "{{\"event\":\"retrying\",\"task\":\"{}\",\"attempt\":{attempt}}}",
            core::hex_encode(task.as_bytes()),
        ),
        SessionEvent::TaskParked { task, reason } => format!(
            "{{\"event\":\"task_parked\",\"task\":\"{}\",\"reason\":{}}}",
            core::hex_encode(task.as_bytes()),
            quoted(park_reason(reason)),
        ),
        SessionEvent::InboundSubmitted { task } => format!(
            "{{\"event\":\"inbound_submitted\",\"task\":\"{}\"}}",
            core::hex_encode(task.as_bytes()),
        ),
        SessionEvent::InboundRejected { reason } => format!(
            "{{\"event\":\"inbound_rejected\",\"reason\":{}}}",
            quoted(&reason.to_string()),
        ),
        SessionEvent::TransportDisconnected => {
            "{\"event\":\"transport_disconnected\"}".to_string()
        }
        SessionEvent::TransportReconnected { replayed } => {
            format!("{{\"event\":\"transport_reconnected\",\"replayed\":{replayed}}}")
        }
        SessionEvent::RedeemOffered {
            code_id,
            pairing_id,
            peer,
        } => format!(
            "{{\"event\":\"redeem_offered\",\"code_id\":\"{}\",\"pairing_id\":\"{}\",\"peer\":\"{}\"}}",
            code_id.to_hex(),
            core::hex_encode(pairing_id.as_bytes()),
            peer.to_hex(),
        ),
        SessionEvent::PairAborted { pairing_id } => format!(
            "{{\"event\":\"pair_aborted\",\"pairing_id\":\"{}\"}}",
            core::hex_encode(pairing_id.as_bytes()),
        ),
        SessionEvent::ShutdownStarted { mode } => format!(
            "{{\"event\":\"shutdown_started\",\"mode\":{}}}",
            quoted(match mode {
                ShutdownMode::Drain => "drain",
                ShutdownMode::Cancel => "cancel",
            }),
        ),
        SessionEvent::ShutdownComplete {
            completed,
            cancelled,
            timed_out,
        } => format!(
            "{{\"event\":\"shutdown_complete\",\"completed\":{completed},\"cancelled\":{cancelled},\"timed_out\":{timed_out}}}",
        ),
    }
}

fn scheduler_event_json(event: &SchedulerEvent) -> String {
    match event {
        SchedulerEvent::TaskQueued { task, group } => format!(
            "{{\"event\":\"task_queued\",\"task\":\"{}\",\"group\":\"{}\"}}",
            core::hex_encode(task.task_id().as_bytes()),
            group.code_id.to_hex(),
        ),
        SchedulerEvent::TaskAdmitted { task, group, .. } => format!(
            "{{\"event\":\"task_admitted\",\"task\":\"{}\",\"group\":\"{}\"}}",
            core::hex_encode(task.task_id().as_bytes()),
            group.code_id.to_hex(),
        ),
        SchedulerEvent::TaskStarted { task } => format!(
            "{{\"event\":\"task_started\",\"task\":\"{}\"}}",
            core::hex_encode(task.task_id().as_bytes()),
        ),
        SchedulerEvent::TaskFinished {
            task,
            group,
            outcome,
        } => {
            let (outcome_name, error) = match outcome {
                FinishOutcome::Completed => ("completed", String::new()),
                FinishOutcome::Failed { error } => (
                    "failed",
                    format!(",\"error\":{}", quoted(&error.to_string())),
                ),
            };
            format!(
                "{{\"event\":\"task_finished\",\"task\":\"{}\",\"group\":\"{}\",\"outcome\":\"{outcome_name}\"{error}}}",
                core::hex_encode(task.task_id().as_bytes()),
                group.code_id.to_hex(),
            )
        }
        SchedulerEvent::TaskCancelled {
            task,
            group,
            was_running,
        } => format!(
            "{{\"event\":\"task_cancelled\",\"task\":\"{}\",\"group\":\"{}\",\"was_running\":{was_running}}}",
            core::hex_encode(task.task_id().as_bytes()),
            group.code_id.to_hex(),
        ),
        SchedulerEvent::PolicyChanged { revision, draining } => {
            format!("{{\"event\":\"policy_changed\",\"revision\":{revision},\"draining\":{draining}}}")
        }
        SchedulerEvent::Draining { active_tasks } => {
            format!("{{\"event\":\"draining\",\"active_tasks\":{active_tasks}}}")
        }
    }
}

fn parse_conflict(text: &str) -> Result<ak_core::handle::ConflictPolicy, FfiError> {
    match text {
        "fail" => Ok(ak_core::handle::ConflictPolicy::Fail),
        "rename" => Ok(ak_core::handle::ConflictPolicy::Rename),
        other => Err(FfiError::invalid(format!(
            "field \"conflict\" must be \"fail\" or \"rename\", got {other:?}"
        ))),
    }
}

fn parse_timeout(secs: u64) -> Result<Duration, FfiError> {
    if !(1..=86_400).contains(&secs) {
        return Err(FfiError::invalid(
            "field \"timeout_secs\" must be between 1 and 86400",
        ));
    }
    Ok(Duration::from_secs(secs))
}
