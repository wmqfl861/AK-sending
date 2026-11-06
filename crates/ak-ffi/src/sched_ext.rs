//! B8 (wave 23): the five remaining `ffi-v2.md` §1 exports —
//! `ak_pairing_stop_start`, `ak_sched_listen_start`, `ak_sched_retry`,
//! `ak_sched_task_result`, `ak_sched_shutdown_start` — completing the frozen
//! 18-signature v2 table (`ak_pairing_*` 9 + `ak_sched_*` 9). Together with
//! the wave-22 legacy alias `ak_sched_start` (shim.rs) the actual export
//! count is 19, the form the 2026-11-05 wave-26 ABI ruling froze
//! (`ak_sched_start` and `ak_sched_listen_start` coexist).
//!
//! # Why a second shim file
//!
//! [`crate::shim`] and [`crate::pairing`] are under concurrent ownership in
//! this wave, so this file carries its own pointer adapters and safe layer
//! under EXACTLY the same discipline: `unsafe` blocks live only behind the
//! extern "C" entry points below, each carrying a `SAFETY:` comment covering
//! non-nullness, lifetime, and UTF-8 validation, and every line of logic
//! lives in the `forbid`-by-construction safe functions above them (no
//! `unsafe` keyword appears outside the pointer helpers). The four small
//! pointer helpers mirror `shim.rs`'s private ones verbatim — they cannot
//! be shared without editing that frozen file.
//!
//! # Symbol naming (task: 统一 ak_sched 符号命名)
//!
//! The frozen contract names the scheduler entry point
//! `ak_sched_listen_start` and defines no bare `ak_sched_start`; wave 22
//! shipped the implementation under `ak_sched_start` (shim.rs). This file
//! exports the contract name as an independent `#[no_mangle]` wrapper (a
//! `pub use` re-export cannot carry `#[no_mangle]`) delegating to the same
//! safe-layer `sched_start`, so `llvm-nm -D` sees the contract symbol and
//! the Android build's symbol gate passes.
//!
//! # Signatures
//!
//! The five exports use the frozen `ffi-v2.md` §1 signatures (the wave-22
//! `ak_sched_*` exports dropped the `runtime_id` parameter; these do not —
//! the contract document is the interoperability authority the Dart
//! bindings code against). Where a scheduler call names a `runtime_id`, it
//! must be the pairing runtime the scheduler is BOUND to (the one its
//! spec selected at start); any other id is `-4`.
//!
//! # What is real and what is stubbed (honest scope)
//!
//! * `ak_pairing_stop_start` — REAL: aborts the runtime's ops and owner
//!   loop, closes the control client (best-effort `close_notify`), flips
//!   the connect phase to a terminal "stopped by caller" state, clears the
//!   engine's active-runtime pointer when it named this runtime, and
//!   records an immediately-finished `stop` op readable through
//!   `ak_pairing_op_result`.
//! * `ak_sched_listen_start` — REAL delegation: binds the real session
//!   runtime exactly like `ak_sched_start` (the contract's name for the
//!   same operation), injecting the caller's `runtime_id` into the spec
//!   when the spec does not name one. The RESIDENT INBOUND LISTENER
//!   remains the documented wave-22 scope cut (`listener_enabled` is
//!   forced off until the on-channel receive executor lands), so the op
//!   result honestly reports `"listener_enabled":false`.
//! * `ak_sched_retry` — REAL: the session runtime's `retry` command
//!   (parked-task restart, contract `sched retry <task-id>`).
//! * `ak_sched_task_result` — REAL: single-task snapshot query
//!   (`SnapshotQuery::one`), rendered in the same object shape as the
//!   `tasks[]` entries of `ak_sched_snapshot`.
//! * `ak_sched_shutdown_start` — REAL: spawns the session runtime's
//!   shutdown (drain or cancel) on the embedded tokio runtime; the op
//!   resolves with the `ShutdownSummary` and the frozen
//!   `shutdown_started`/`shutdown_complete` scheduler events flow through
//!   `ak_sched_poll_event`. Known limitation: the engine keeps its one
//!   scheduler slot for its lifetime (the dead runtime cannot be swapped
//!   for a fresh `ak_sched_start` on the same engine); restarting the
//!   scheduler needs a new engine handle.
//!
//! Nothing here returns a NOT_IMPLEMENTED code — every symbol has a real
//! implementation; the only functional gap (the resident inbound listener)
//! is inherited from wave 22 and is reported per call, not hidden.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ak_identity::pairing_txn::random_id16;
use ak_session::runtime::{
    ParkReason, SessionError, ShutdownMode, ShutdownSummary, SnapshotQuery, TaskView,
    TransferDirection,
};
use ak_session::scheduler::key::TaskId;

use crate::core::{
    self, guarded, AkEngine, FfiError, AK_DONE, AK_ERR_BUFFER, AK_ERR_INVALID, AK_ERR_NULL, AK_OK,
};
use crate::pairing::{OpEntry, OpOutcome, PairingRt, SchedRt};

/// Byte length of every id crossing the boundary (runtime/op/task ids).
const ID_LEN: usize = 16;

/// Same synchronous scheduler round-trip budget as `pairing.rs` (in-memory
/// command-channel answers, never network).
const SCHED_ROUNDTRIP: Duration = Duration::from_secs(5);

// ===========================================================================
// Safe layer (no `unsafe` below this line until the shim section).
// ===========================================================================

fn quoted(text: &str) -> String {
    crate::core::json::quoted(text)
}

/// Lock the engine's v2 state (`-5` on poisoning, same wording).
fn v2_locked(
    engine: &AkEngine,
) -> Result<std::sync::MutexGuard<'_, crate::pairing::V2State>, FfiError> {
    engine
        .v2
        .lock()
        .map_err(|_| FfiError::internal("v2 state lock poisoned"))
}

/// Look one pairing runtime up by id (same `-4` wording as `pairing.rs`).
fn runtime_of(engine: &AkEngine, runtime_id: &[u8; ID_LEN]) -> Result<Arc<PairingRt>, FfiError> {
    v2_locked(engine)?
        .runtimes
        .get(runtime_id)
        .cloned()
        .ok_or_else(|| {
            FfiError::not_found(format!("unknown runtime {}", core::hex_encode(runtime_id)))
        })
}

/// The engine's single scheduler runtime (`-4` when none was started).
fn sched_of(engine: &AkEngine) -> Result<Arc<SchedRt>, FfiError> {
    v2_locked(engine)?
        .sched
        .clone()
        .ok_or_else(|| FfiError::not_found("no scheduler runtime: call ak_sched_start first"))
}

/// The scheduler runtime, additionally checking that `runtime_id` is the
/// pairing runtime it is bound to (the contract passes a runtime id on
/// every sched call; a mismatch is `-4`, never silently rebound).
fn sched_bound_to(engine: &AkEngine, runtime_id: &[u8; ID_LEN]) -> Result<Arc<SchedRt>, FfiError> {
    let sched = sched_of(engine)?;
    if sched.runtime_id != *runtime_id {
        return Err(FfiError::not_found(format!(
            "the scheduler runtime is bound to {}, not {}",
            core::hex_encode(&sched.runtime_id),
            core::hex_encode(runtime_id),
        )));
    }
    Ok(sched)
}

/// Push one pairing event line into the runtime's bounded queue.
fn push_rt_event(rt: &PairingRt, line: String) {
    if let Ok(mut queue) = rt.events.lock() {
        core::push_capped(&mut queue, line);
    }
}

/// Register an ALREADY-FINISHED op on one runtime and announce it with the
/// frozen `op_done`/`op_failed` pairing event. Used by the stop and
/// listen_start exports, whose bodies complete synchronously; the op entry
/// makes the outcome readable through `ak_pairing_op_result` like any op.
fn register_finished_op(
    rt: &PairingRt,
    op: &'static str,
    ok: bool,
    json: String,
) -> Result<[u8; ID_LEN], FfiError> {
    let op_id = random_id16();
    let entry = Arc::new(OpEntry {
        outcome: StdMutex::new(OpOutcome::Finished { ok, json }),
        task: StdMutex::new(None),
    });
    let mut ops = rt
        .ops
        .lock()
        .map_err(|_| FfiError::internal("op table lock poisoned"))?;
    ops.insert(op_id, entry);
    drop(ops);
    push_rt_event(
        rt,
        format!(
            "{{\"event\":\"{}\",\"op_id\":\"{}\",\"op\":{}}}",
            if ok { "op_done" } else { "op_failed" },
            core::hex_encode(&op_id),
            quoted(op),
        ),
    );
    Ok(op_id)
}

/// Map a session-runtime error onto the ABI codes (same policy as
/// `pairing.rs`: unknown/terminal tasks are `-4`, spec/queue/shutdown-state
/// rejections are `-2`, everything else is `-5`).
fn map_session_error(error: SessionError) -> FfiError {
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

/// Run one bounded command round-trip against the scheduler actor.
fn sched_roundtrip<T>(
    engine: &AkEngine,
    call: impl std::future::Future<Output = Result<T, SessionError>>,
) -> Result<T, FfiError> {
    engine.runtime().block_on(async {
        match tokio::time::timeout(SCHED_ROUNDTRIP, call).await {
            Ok(result) => result.map_err(map_session_error),
            Err(_) => Err(FfiError::internal("scheduler round-trip timed out")),
        }
    })
}

// ---------------------------------------------------------------------------
// ak_pairing_stop_start (safe body).
// ---------------------------------------------------------------------------

/// Stop one pairing runtime: atomically enter the terminal "stopped"
/// phase (under the runtime's startup lock), then cancel/await the
/// startup subtasks — the background connect included — abort the op
/// tasks, and close the control client. The runtime entry STAYS
/// registered and accepted op results (this stop op included, and any op
/// that finished before the stop) remain readable via
/// `ak_pairing_op_result`; every later op on this runtime fails fast
/// with "stopped by caller". A connect that settles after the stop
/// returns can never install its live halves or emit `runtime_ready`
/// (P1-04; ffi-v2 §2 stop semantics).
pub fn pairing_stop(
    engine: &AkEngine,
    runtime_id: &[u8; ID_LEN],
) -> Result<[u8; ID_LEN], FfiError> {
    let rt = runtime_of(engine, runtime_id)?;
    // Teardown enters the terminal phase FIRST (inside, under the startup
    // lock), then cancels/awaits the startup subtasks. Local work only —
    // task aborts plus a spawned best-effort close_notify; no network
    // wait inside the synchronous call.
    rt.teardown(engine.runtime());
    push_rt_event(
        &rt,
        format!(
            "{{\"event\":\"runtime_stopped\",\"runtime\":\"{}\"}}",
            core::hex_encode(runtime_id),
        ),
    );
    let op_id = register_finished_op(
        &rt,
        "stop",
        true,
        format!(
            "{{\"op\":\"stop\",\"runtime\":{}}}",
            quoted(&core::hex_encode(runtime_id)),
        ),
    )?;
    // The stopped runtime must not stay the implicit bind target of a
    // later `ak_sched_start` spec that names no runtime.
    let mut v2 = v2_locked(engine)?;
    if v2.active == Some(*runtime_id) {
        v2.active = None;
    }
    Ok(op_id)
}

// ---------------------------------------------------------------------------
// ak_sched_listen_start (safe body) — the contract name of ak_sched_start.
// ---------------------------------------------------------------------------

/// Re-render a parsed JSON value (only invoked on parser-accepted input, so
/// strings are already control-character free and `Num` tokens are the
/// original lexemes — the output is valid JSON by construction).
fn render_json_value(value: &crate::core::json::Json) -> String {
    use crate::core::json::Json;
    match value {
        Json::Null => "null".to_string(),
        Json::Bool(true) => "true".to_string(),
        Json::Bool(false) => "false".to_string(),
        Json::Num(token) => token.clone(),
        Json::Str(text) => quoted(text),
        Json::Arr(items) => {
            let mut out = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&render_json_value(item));
            }
            out.push(']');
            out
        }
        Json::Obj(members) => {
            let mut out = String::from("{");
            for (index, (key, item)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&quoted(key));
                out.push(':');
                out.push_str(&render_json_value(item));
            }
            out.push('}');
            out
        }
    }
}

/// `ak_sched_listen_start`: bind the scheduler to the NAMED pairing runtime
/// (the caller's `runtime_id` argument wins unless the spec names the same
/// id explicitly; a conflicting spec id is `-2`), then record the
/// immediately-finished op. Returns the op id.
pub fn sched_listen_start(
    engine: &AkEngine,
    runtime_id: &[u8; ID_LEN],
    spec: &str,
) -> Result<[u8; ID_LEN], FfiError> {
    use crate::core::json::Json;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    if !matches!(args, Json::Obj(_)) {
        return Err(FfiError::invalid(
            "the scheduler spec must be a JSON object",
        ));
    }
    let runtime_hex = core::hex_encode(runtime_id);
    let effective_spec = match args.get("runtime_id") {
        None | Some(Json::Null) => {
            // Inject the caller's id: the underlying sched_start reads the
            // (optional) "runtime_id" field; get() takes the LAST member,
            // so appending wins over any earlier null.
            let mut members = args.members().to_vec();
            members.push(("runtime_id".to_string(), Json::Str(runtime_hex.clone())));
            let mut out = String::from("{");
            for (index, (key, value)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&quoted(key));
                out.push(':');
                out.push_str(&render_json_value(value));
            }
            out.push('}');
            out
        }
        Some(Json::Str(text)) if text == &runtime_hex => spec.to_string(),
        Some(Json::Str(other)) => {
            return Err(FfiError::invalid(format!(
                "field \"runtime_id\" ({other}) does not match the runtime_id argument ({runtime_hex})"
            )));
        }
        Some(_) => return Err(FfiError::invalid("field \"runtime_id\" must be a string")),
    };
    // Same validation/side-effect order as ak_sched_start: a bad spec or an
    // unconnected runtime fails BEFORE any op id exists.
    crate::pairing::sched_start(engine, &effective_spec)?;
    let rt = runtime_of(engine, runtime_id)?;
    // Honest scope note in the result: the resident inbound listener stays
    // off until the on-channel receive executor lands (wave-22 cut).
    register_finished_op(
        &rt,
        "listen_start",
        true,
        format!(
            "{{\"op\":\"listen_start\",\"runtime\":{},\"listener_enabled\":false}}",
            quoted(&runtime_hex),
        ),
    )
}

// ---------------------------------------------------------------------------
// ak_sched_retry (safe body).
// ---------------------------------------------------------------------------

/// `ak_sched_retry`: restart one PARKED task (contract `sched retry
/// <task-id>`). Unknown, non-existent, or not-currently-parked tasks are
/// `-4` (`TaskNotFound`/`NotActive`).
pub fn sched_retry(
    engine: &AkEngine,
    runtime_id: &[u8; ID_LEN],
    task_id: &[u8; ID_LEN],
) -> Result<(), FfiError> {
    let sched = sched_bound_to(engine, runtime_id)?;
    sched_roundtrip(engine, sched.handle.retry(TaskId::from_bytes(*task_id)))
}

// ---------------------------------------------------------------------------
// ak_sched_task_result (safe body).
// ---------------------------------------------------------------------------

fn park_reason(reason: &ParkReason) -> &'static str {
    match reason {
        ParkReason::RetryExhausted => "retry_exhausted",
        ParkReason::QueueFull => "queue_full",
    }
}

/// Render one [`TaskView`] in the exact object shape of the
/// `ak_sched_snapshot` `tasks[]` entries (schema in the crate header), so
/// callers parse one shape everywhere.
fn render_task_view(task: &TaskView) -> String {
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

/// `ak_sched_task_result`: the single-task view (terminal tasks included —
/// this is the contract §5 "终态必须能通过任务查询补取" channel). An unknown
/// task id is `-4`.
pub fn sched_task_result(
    engine: &AkEngine,
    runtime_id: &[u8; ID_LEN],
    task_id: &[u8; ID_LEN],
) -> Result<String, FfiError> {
    let sched = sched_bound_to(engine, runtime_id)?;
    let snapshot = sched_roundtrip(
        engine,
        sched
            .handle
            .snapshot(SnapshotQuery::one(TaskId::from_bytes(*task_id))),
    )?;
    let task = snapshot
        .tasks
        .iter()
        .find(|view| view.task.as_bytes() == task_id)
        .ok_or_else(|| {
            FfiError::not_found(format!("unknown task {}", core::hex_encode(task_id)))
        })?;
    Ok(render_task_view(task))
}

// ---------------------------------------------------------------------------
// ak_sched_shutdown_start (safe body).
// ---------------------------------------------------------------------------

fn shutdown_mode_str(mode: ShutdownMode) -> &'static str {
    match mode {
        ShutdownMode::Drain => "drain",
        ShutdownMode::Cancel => "cancel",
    }
}

fn shutdown_summary_json(summary: &ShutdownSummary) -> String {
    format!(
        "{{\"op\":\"shutdown\",\"mode\":{},\"completed\":{},\"cancelled\":{},\"parked\":{},\"timed_out\":{},\"listener_stopped\":{}}}",
        quoted(shutdown_mode_str(summary.mode)),
        summary.completed,
        summary.cancelled,
        summary.parked,
        summary.timed_out,
        summary.listener_stopped,
    )
}

/// Parse the shutdown spec: `{"spec_version":1,"mode":"drain"|"cancel"}`
/// (mode optional, default `cancel` — the teardown default; unknown fields
/// ignored, same policy as every other spec).
fn parse_shutdown_spec(spec: &str) -> Result<ShutdownMode, FfiError> {
    use crate::core::json::Json;
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    match args.get("mode") {
        None | Some(Json::Null) => Ok(ShutdownMode::Cancel),
        Some(Json::Str(text)) => match text.as_str() {
            "drain" => Ok(ShutdownMode::Drain),
            "cancel" => Ok(ShutdownMode::Cancel),
            other => Err(FfiError::invalid(format!(
                "field \"mode\" must be drain or cancel, got {other:?}"
            ))),
        },
        Some(_) => Err(FfiError::invalid("field \"mode\" must be a string")),
    }
}

/// `ak_sched_shutdown_start`: accept the shutdown (the op id is minted and
/// returned immediately — no drain wait inside the synchronous call) and
/// drive it on the embedded runtime. The op resolves with the summary; the
/// frozen `shutdown_started`/`shutdown_complete` scheduler events flow
/// through `ak_sched_poll_event` as always.
pub fn sched_shutdown_start(
    engine: &AkEngine,
    runtime_id: &[u8; ID_LEN],
    spec: &str,
) -> Result<[u8; ID_LEN], FfiError> {
    // Validation precedes every side effect (ffi-v2 §2): a bad spec never
    // mints an op, let alone begins a shutdown.
    let mode = parse_shutdown_spec(spec)?;
    let sched = sched_bound_to(engine, runtime_id)?;
    let rt = runtime_of(engine, runtime_id)?;

    let op_id = random_id16();
    let entry = Arc::new(OpEntry {
        outcome: StdMutex::new(OpOutcome::Running { op: "shutdown" }),
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
        let handle = sched.handle.clone();
        let op_id_hex = core::hex_encode(&op_id);
        engine.runtime().spawn(async move {
            let (ok, json) = match handle.shutdown(mode).await {
                Ok(summary) => (true, shutdown_summary_json(&summary)),
                Err(error) => (
                    false,
                    format!(
                        "{{\"op\":\"shutdown\",\"error\":{}}}",
                        quoted(&error.to_string())
                    ),
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
            push_rt_event(
                &rt,
                format!(
                    "{{\"event\":\"{}\",\"op_id\":\"{op_id_hex}\",\"op\":\"shutdown\"}}",
                    if ok { "op_done" } else { "op_failed" },
                ),
            );
        })
    };
    if let Ok(mut slot) = entry.task.lock() {
        *slot = Some(task);
    }
    Ok(op_id)
}

// ===========================================================================
// C ABI shim (the ONLY `unsafe` in this file; mirrors shim.rs discipline).
// ===========================================================================

/// Why a caller-buffer write failed; `need` includes the NUL terminator.
enum WriteError {
    NullBuffer,
    TooSmall { need: usize, capacity: usize },
}

/// Write `text` plus a NUL into `buf[0..cap]`. No side effects on error.
///
/// # Safety
///
/// SAFETY: when returning an error, nothing is written; on success, `buf`
/// must be valid for writes of `cap` bytes for the duration of the call
/// and `text.len() + 1 <= cap` is verified before any write happens, so
/// the write stays in bounds by construction.
unsafe fn write_cstr(buf: *mut c_char, cap: usize, text: &str) -> Result<usize, WriteError> {
    if buf.is_null() {
        return Err(WriteError::NullBuffer);
    }
    let need = text.len() + 1;
    if cap < need {
        return Err(WriteError::TooSmall {
            need,
            capacity: cap,
        });
    }
    // SAFETY: buf is non-null and caller-valid for `cap` bytes; `need <= cap`
    // was just verified, so both the payload copy and the NUL write at
    // buf[text.len()] are within the caller's allocation.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buf.cast::<u8>(), text.len());
        *buf.cast::<u8>().add(text.len()) = 0;
    }
    Ok(text.len())
}

/// Read a NUL-terminated UTF-8 C string.
///
/// # Safety
///
/// SAFETY: `ptr` must be non-null and point to a NUL-terminated sequence of
/// bytes that stays valid and unmodified for the duration of `'a` (in
/// practice: the FFI call); `CStr::from_ptr` stops at the first NUL, so the
/// borrow never reads past the terminator. UTF-8 validity is checked before
/// the string is handed on.
unsafe fn read_cstr<'a>(ptr: *const c_char) -> Result<&'a str, FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: AK_ERR_NULL,
            message: "string argument is null".to_string(),
        });
    }
    // SAFETY: caller guarantees non-null + NUL termination (C contract);
    // the borrow lives only for the duration of this FFI call.
    let raw = unsafe { std::ffi::CStr::from_ptr(ptr) };
    raw.to_str().map_err(|_| FfiError {
        code: AK_ERR_INVALID,
        message: "string argument is not valid UTF-8".to_string(),
    })
}

/// Read one 16-byte id.
///
/// # Safety
///
/// SAFETY: `ptr` must be non-null and readable for 16 bytes for the
/// duration of the call; the copy leaves the source untouched.
unsafe fn read_id(ptr: *const u8) -> Result<[u8; ID_LEN], FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: AK_ERR_NULL,
            message: "id argument is null".to_string(),
        });
    }
    let mut id = [0u8; ID_LEN];
    // SAFETY: ptr is non-null and caller-valid for 16 readable bytes.
    unsafe { std::ptr::copy_nonoverlapping(ptr, id.as_mut_ptr(), ID_LEN) };
    Ok(id)
}

/// Write one 16-byte id.
///
/// # Safety
///
/// SAFETY: `ptr` must be non-null and writable for 16 bytes for the
/// duration of the call; exactly 16 bytes are written.
unsafe fn write_id(ptr: *mut u8, id: &[u8; ID_LEN]) -> Result<(), FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: AK_ERR_NULL,
            message: "id_out argument is null".to_string(),
        });
    }
    // SAFETY: ptr is non-null and caller-valid for 16 writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), ptr, ID_LEN) };
    Ok(())
}

/// Record an error return: message to the thread-local, code to the caller.
fn fail(error: FfiError) -> i32 {
    core::set_last_error(error.message);
    error.code
}

/// Record the retryable buffer-too-small condition.
fn fail_buffer(need: usize, capacity: usize, what: &str) -> i32 {
    core::set_last_error(format!(
        "buffer too small for {what}: need {need} bytes including the NUL terminator \
         (capacity {capacity}); retry with a larger buffer"
    ));
    AK_ERR_BUFFER
}

/// Resolve the engine handle or fail with `-1` (same contract as
/// `shim.rs`'s `engine_or_fail`).
///
/// # Safety
///
/// SAFETY: the raw handle comes from `ak_engine_new`; the caller keeps it
/// alive, un-freed and un-concurrent for the duration of this call, so the
/// shared borrow cannot dangle or alias a mutation. The unbounded lifetime
/// is the one `engine.as_ref()` infers at every shim call site; the C
/// contract (single serialized caller per handle) justifies it.
unsafe fn engine_or_fail<'a>(engine: *const AkEngine) -> Option<&'a AkEngine> {
    unsafe { engine.as_ref() }
}

/// `ak_pairing_stop_start` (ffi-v2 §1): stop one pairing runtime. Returns
/// `AK_OK` with a fresh 16-byte `op_id` written whose result (immediately
/// terminal) is readable via `ak_pairing_op_result`: `{"op":"stop",
/// "runtime":"<32hex>"}`. The runtime stops synchronously (op aborts,
/// owner-loop abort, spawned best-effort client close) — no network wait
/// happens inside the call; a `runtime_stopped` pairing event follows.
///
/// Fails with `-1` (null engine/runtime/op pointer), or `-4` for an unknown
/// runtime id. A stopped runtime stays readable (op results included); new
/// ops on it fail with "stopped by caller".
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` must be non-null and readable
/// for 16 bytes; `op_id_out` must be non-null and writable for 16 bytes
/// (both checked BEFORE any side effect — a failed call never stops
/// anything); the engine handle contract is that of `ak_send_start`
/// (live, not freed concurrently, calls serialized by the caller).
#[no_mangle]
pub extern "C" fn ak_pairing_stop_start(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine_or_fail(engine) }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // Output pointer first: a failed start must never leave side effects
    // behind (ffi-v2 §2).
    if op_id_out.is_null() {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "op_id_out argument is null".to_string(),
        });
    }
    // SAFETY: read_id verifies non-null and copies exactly 16 bytes.
    let runtime_id = match unsafe { read_id(runtime_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match guarded(|| pairing_stop(engine, &runtime_id)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly the 16
            // documented bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// `ak_sched_listen_start` (ffi-v2 §1) — the contract's name for the
/// scheduler start: bind the real session runtime to the pairing runtime
/// named by `runtime_id` (injected into the policy spec when the spec names
/// none; a conflicting spec id is `-2`). Returns `AK_OK` with a fresh
/// 16-byte `op_id` whose result is `{"op":"listen_start","runtime":
/// "<32hex>","listener_enabled":false}` — the resident inbound listener is
/// the documented wave-22 scope cut and stays off this revision.
///
/// Same statuses as `ak_sched_start` (`-2` bad spec / not-yet-connected
/// runtime / second scheduler, `-4` no or unknown runtime), plus `-1` for
/// null pointers.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; `spec_json` non-null, NUL-terminated, valid UTF-8, readable up
/// to its NUL; `op_id_out` non-null and writable for 16 bytes (all checked
/// before any side effect). Engine handle as `ak_send_start`.
#[no_mangle]
pub extern "C" fn ak_sched_listen_start(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    spec_json: *const c_char,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine_or_fail(engine) }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    if op_id_out.is_null() {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "op_id_out argument is null".to_string(),
        });
    }
    // SAFETY: read_id verifies non-null and copies exactly 16 bytes.
    let runtime_id = match unsafe { read_id(runtime_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    // SAFETY: read_cstr verifies non-null and UTF-8; the borrow is confined
    // to this call.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| sched_listen_start(engine, &runtime_id, spec)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly 16
            // bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// `ak_sched_retry` (ffi-v2 §1): restart one PARKED task of the scheduler
/// bound to `runtime_id`. `AK_OK` on acceptance; `-4` for an unknown
/// runtime/task binding, a task this runtime does not know, or a task that
/// is not currently parked; `-1` for null pointers.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` and `task_id` non-null and
/// readable for 16 bytes each (checked before any side effect). Engine
/// handle as `ak_send_start`.
#[no_mangle]
pub extern "C" fn ak_sched_retry(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    task_id: *const u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine_or_fail(engine) }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_id verifies non-null and copies exactly 16 bytes.
    let runtime_id = match unsafe { read_id(runtime_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    // SAFETY: same contract as runtime_id.
    let task_id = match unsafe { read_id(task_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match guarded(|| sched_retry(engine, &runtime_id, &task_id)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// `ak_sched_task_result` (ffi-v2 §1): write one task's current view
/// (UTF-8 + NUL) into `buf` — the same object shape as the
/// `ak_sched_snapshot` `tasks[]` entries, terminal states included, so a
/// missed progress event can always be recovered (ffi-v2 §5).
///
/// Returns `AK_DONE` (1) when the JSON was written, `AK_ERR_NOT_FOUND`
/// (`-4`) for an unknown runtime/task binding or task id, `AK_ERR_BUFFER`
/// (`-3`) when `cap` is too small (nothing is consumed — retry losslessly
/// with a larger cap; the last-error message names the needed size),
/// `AK_ERR_NULL` (`-1`) for null pointer arguments.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id`/`task_id` non-null and readable
/// for 16 bytes each; when non-null, `buf` writable for `cap` bytes, not
/// modified concurrently; `cap` must not overstate the real allocation.
/// Engine handle as `ak_send_start`.
#[no_mangle]
pub extern "C" fn ak_sched_task_result(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    task_id: *const u8,
    buf: *mut c_char,
    cap: usize,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine_or_fail(engine) }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_id verifies non-null and copies exactly 16 bytes.
    let runtime_id = match unsafe { read_id(runtime_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    // SAFETY: same contract as runtime_id.
    let task_id = match unsafe { read_id(task_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match guarded(|| sched_task_result(engine, &runtime_id, &task_id)) {
        Ok(json) => match unsafe { write_cstr(buf, cap, &json) } {
            // SAFETY: write_cstr checks null/bounds itself and writes only
            // within the caller-provided capacity.
            Ok(_) => AK_DONE,
            Err(WriteError::TooSmall { need, capacity }) => {
                fail_buffer(need, capacity, "the task result JSON")
            }
            Err(WriteError::NullBuffer) => {
                core::set_last_error("buf argument is null");
                AK_ERR_NULL
            }
        },
        Err(error) => fail(error),
    }
}

/// `ak_sched_shutdown_start` (ffi-v2 §1): begin the scheduler shutdown
/// (`{"spec_version":1,"mode":"drain"|"cancel"}`, mode default `cancel`).
/// Returns `AK_OK` with a fresh 16-byte `op_id` written IMMEDIATELY — the
/// drain/cancel itself runs on the embedded runtime; its result
/// (`{"op":"shutdown","mode":...,"completed":N,...}`) arrives via
/// `ak_pairing_op_result`, and the frozen `shutdown_started` /
/// `shutdown_complete` events via `ak_sched_poll_event`.
///
/// `-2` for a bad spec or an already-shutting-down runtime, `-4` for an
/// unknown runtime/task binding or no scheduler, `-1` for null pointers.
/// After `shutdown_complete` the scheduler slot stays occupied by the dead
/// runtime for the engine's lifetime (restart on a fresh engine).
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; `spec_json` non-null, NUL-terminated, valid UTF-8; `op_id_out`
/// non-null and writable for 16 bytes (all checked before any side
/// effect — a rejected shutdown never begins). Engine handle as
/// `ak_send_start`.
#[no_mangle]
pub extern "C" fn ak_sched_shutdown_start(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    spec_json: *const c_char,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine_or_fail(engine) }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    if op_id_out.is_null() {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "op_id_out argument is null".to_string(),
        });
    }
    // SAFETY: read_id verifies non-null and copies exactly 16 bytes.
    let runtime_id = match unsafe { read_id(runtime_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    // SAFETY: read_cstr verifies non-null and UTF-8; the borrow is
    // confined to this call.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| sched_shutdown_start(engine, &runtime_id, spec)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly the 16
            // documented bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// The wave-23 completion of the frozen v2 export surface, asserted at
/// test time (the Android symbol gate checks the .so; this checks the
/// rlib's Rust-visible names stay exported after refactors).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ak_engine_free, ak_engine_new, AK_ERR_NOT_FOUND};

    #[test]
    fn frozen_export_names_exist() {
        // Type-level presence checks: referencing the functions through a
        // fn pointer proves the (no_mangle) items exist with the expected
        // arities.
        let stops: unsafe extern "C" fn(*mut AkEngine, *const u8, *mut u8) -> i32 =
            ak_pairing_stop_start;
        let listen: unsafe extern "C" fn(*mut AkEngine, *const u8, *const c_char, *mut u8) -> i32 =
            ak_sched_listen_start;
        let retry: unsafe extern "C" fn(*mut AkEngine, *const u8, *const u8) -> i32 =
            ak_sched_retry;
        let result: unsafe extern "C" fn(
            *mut AkEngine,
            *const u8,
            *const u8,
            *mut c_char,
            usize,
        ) -> i32 = ak_sched_task_result;
        let shutdown: unsafe extern "C" fn(
            *mut AkEngine,
            *const u8,
            *const c_char,
            *mut u8,
        ) -> i32 = ak_sched_shutdown_start;
        let _ = (stops, listen, retry, result, shutdown);
    }

    #[test]
    fn argument_validation_precedes_side_effects() {
        let null_id: *const u8 = std::ptr::null();
        let null_out: *mut u8 = std::ptr::null_mut();
        // Null engine → -1 on every export; nothing is dereferenced.
        assert_eq!(
            ak_pairing_stop_start(std::ptr::null_mut(), null_id, null_out),
            AK_ERR_NULL
        );
        assert_eq!(
            ak_sched_listen_start(std::ptr::null_mut(), null_id, std::ptr::null(), null_out,),
            AK_ERR_NULL
        );
        assert_eq!(
            ak_sched_retry(std::ptr::null_mut(), null_id, null_id),
            AK_ERR_NULL
        );
        assert_eq!(
            ak_sched_task_result(
                std::ptr::null_mut(),
                null_id,
                null_id,
                std::ptr::null_mut(),
                0,
            ),
            AK_ERR_NULL
        );
        assert_eq!(
            ak_sched_shutdown_start(std::ptr::null_mut(), null_id, std::ptr::null(), null_out),
            AK_ERR_NULL
        );

        // Live engine, null id/out pointers → -1 before anything happens.
        let engine = ak_engine_new();
        assert!(!engine.is_null());
        assert_eq!(
            ak_pairing_stop_start(engine, null_id, null_out),
            AK_ERR_NULL
        );
        assert_eq!(ak_sched_retry(engine, null_id, null_id), AK_ERR_NULL);
        assert_eq!(
            ak_sched_task_result(engine, null_id, null_id, std::ptr::null_mut(), 8),
            AK_ERR_NULL
        );
        let spec = std::ffi::CString::new("{\"mode\":\"cancel\"}").expect("spec");
        assert_eq!(
            ak_sched_listen_start(engine, null_id, spec.as_ptr(), null_out),
            AK_ERR_NULL
        );
        assert_eq!(
            ak_sched_shutdown_start(engine, null_id, spec.as_ptr(), null_out),
            AK_ERR_NULL
        );
        ak_engine_free(engine);
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let engine = ak_engine_new();
        assert!(!engine.is_null());
        let runtime = [0x11u8; 16];
        let task = [0x22u8; 16];
        let mut op_out = [0u8; 16];
        let mut buf = [0 as c_char; 256];
        let spec = std::ffi::CString::new("{\"mode\":\"cancel\"}").expect("spec");
        assert_eq!(
            ak_pairing_stop_start(engine, runtime.as_ptr(), op_out.as_mut_ptr()),
            AK_ERR_NOT_FOUND
        );
        // No scheduler runtime at all → -4 before the ids are even used.
        assert_eq!(
            ak_sched_retry(engine, runtime.as_ptr(), task.as_ptr()),
            AK_ERR_NOT_FOUND
        );
        assert_eq!(
            ak_sched_task_result(
                engine,
                runtime.as_ptr(),
                task.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            ),
            AK_ERR_NOT_FOUND
        );
        assert_eq!(
            ak_sched_shutdown_start(engine, runtime.as_ptr(), spec.as_ptr(), op_out.as_mut_ptr()),
            AK_ERR_NOT_FOUND
        );
        // listen_start: unknown runtime with a valid spec → -4 (the spec's
        // runtime binding resolves against the registry of started
        // runtimes), and a spec naming a DIFFERENT id than the argument is
        // a -2 mismatch even when neither runtime exists.
        let listen_spec = std::ffi::CString::new("{\"spec_version\":1}").expect("spec");
        assert_eq!(
            ak_sched_listen_start(
                engine,
                runtime.as_ptr(),
                listen_spec.as_ptr(),
                op_out.as_mut_ptr(),
            ),
            AK_ERR_NOT_FOUND
        );
        let conflicting = std::ffi::CString::new(
            "{\"spec_version\":1,\"runtime_id\":\"33333333333333333333333333333333\"}",
        )
        .expect("spec");
        assert_eq!(
            ak_sched_listen_start(
                engine,
                runtime.as_ptr(),
                conflicting.as_ptr(),
                op_out.as_mut_ptr(),
            ),
            AK_ERR_INVALID
        );
        ak_engine_free(engine);
    }
}
