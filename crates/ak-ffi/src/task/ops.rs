//! Task ops: validation, idempotency, the launcher and the public
//! `ak_task_*` entry points.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// Ops: validation, idempotency, launcher.
// ---------------------------------------------------------------------------

/// The validated body of one task op. All `-2` validation happens BEFORE
/// an op id exists.
pub(super) enum TaskOpSpec {
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
pub(super) enum RequestGate {
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
pub(super) struct ReservedOp {
    pub(super) op_id: [u8; 16],
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
pub(super) fn gate_request(
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
pub(super) fn launch_task_op(
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
    // X5 (batch X, 2026-11-22, ffi-v2 §8.3 批次 X 增补): the row carries
    // the peer's SELF-REPORTED device name and the VERIFIED fingerprint's
    // first 8 hex — the name is display-only (never an identity proof),
    // the short code is the identity anchor the UI annotates it with.
    // Both render `null` when the record lacks the fact (additive: older
    // readers ignore unknown keys).
    let peer_name = conn
        .peer_name
        .as_deref()
        .map_or_else(|| "null".to_string(), quoted);
    let peer_fp_short = conn
        .peer_fp
        .as_ref()
        .map_or_else(|| "null".to_string(), |fp| quoted(&fp.short_id()));
    format!(
        "{{\"conn\":\"{}\",\"role\":{},\"state\":{},\"peer_endpoint\":{endpoint},\"code\":{},\"peer_name\":{peer_name},\"peer_fp_short\":{peer_fp_short}}}",
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
