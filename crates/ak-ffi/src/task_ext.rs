//! B8 (wave 28): the eight `ak_task_*` C ABI exports of the v1
//! minimal-pairing task plane — the safe layer lives in [`crate::task`].
//!
//! Same dual-file discipline as [`crate::sched_ext`] (wave 23): this file
//! holds ONLY pointer adapters, each `unsafe` block carrying a `SAFETY:`
//! comment covering non-nullness, lifetime, and UTF-8 validation; every
//! line of logic lives in the `forbid(unsafe_code)` safe layer. The four
//! small pointer helpers mirror `shim.rs`'s private ones verbatim — they
//! cannot be shared without editing that frozen file.
//!
//! # Signatures (frozen ffi-v2.md task-plane table)
//!
//! ```text
//! ak_task_start(engine, spec_json, runtime_id_out[16])
//! ak_task_create_code(engine, runtime_id, role_and_cands_json, op_id_out[16])
//! ak_task_join(engine, runtime_id, code_and_cands_json, op_id_out[16])
//! ak_task_list(engine, runtime_id, buf, cap)
//! ak_task_disconnect(engine, runtime_id, task_id[16])
//! ak_task_send(engine, runtime_id, conn_id[16], paths_json, op_id_out[16])
//! ak_task_set_save_dir(engine, runtime_id, path)
//! ak_task_consent(engine, runtime_id, job_id[16], accept)
//! ak_task_set_event_notify(engine, runtime_id, notify|NULL, ctx)  // 2026-11-12
//! ak_task_server_info(engine, runtime_id, op_id_out[16])          // 2026-11-13
//! ```
//!
//! Op RESULTS and EVENTS do not add symbols: they flow through the
//! existing `ak_pairing_op_result` / `ak_pairing_poll_event` /
//! `ak_pairing_cancel_op` exports (shim.rs), which route to the task
//! table when the runtime id names a task runtime (one `runtime_plane`
//! table pass decides which plane owns the id) — the wave-28 brief's
//! "结果经 poll_event/op_result 取 / 接收提示断开事件走现有
//! poll_event 通道扩展事件类型" ruling. The one addition,
//! `ak_task_set_event_notify` (CR-P2-05 改法第 1 条后半, 2026-11-12),
//! adds NO event/result channel: it registers a DOORBELL — a plain C
//! callback fired when a task event is queued — while the poll remains
//! the single authoritative channel (`src/task.rs`'s `TaskEventNotify`
//! documents the thread/lifetime contract; ffi-v2.md §8.6 the ABI).
//!
//! Nothing here returns a NOT_IMPLEMENTED code; the registry relay,
//! real probes and remote-disconnect events are fully wired at the
//! safe layer (`src/task.rs` — see the module docs for the final
//! wave-28 shape).

use std::os::raw::{c_char, c_void};

use crate::core::{self, guarded, AkEngine, FfiError, AK_DONE, AK_ERR_BUFFER, AK_ERR_NULL, AK_OK};
use crate::task;

/// Byte length of every id crossing the boundary (runtime/op/conn/job).
const ID_LEN: usize = 16;

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
/// SAFETY: `ptr` must be non-null and point to a sequence of bytes that
/// contains a NUL within the caller's allocation and stays valid and
/// unmodified for the duration of `'a` (in practice: the FFI call).
/// `CStr::from_ptr` scans forward for the FIRST NUL — it has no length,
/// so it CANNOT detect a missing terminator: a pointer into memory with
/// no NUL anywhere ahead reads past the allocation (undefined behavior,
/// not an error return). NUL termination is therefore a strict caller
/// precondition, not a validated property. UTF-8 validity of the bytes up
/// to the terminator IS checked before the string is handed on.
unsafe fn read_cstr<'a>(ptr: *const c_char) -> Result<&'a str, FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: crate::core::AK_ERR_NULL,
            message: "string argument is null".to_string(),
        });
    }
    // SAFETY: caller guarantees non-null + NUL termination (C contract);
    // the borrow lives only for the duration of this FFI call.
    let raw = unsafe { std::ffi::CStr::from_ptr(ptr) };
    raw.to_str().map_err(|_| FfiError {
        code: crate::core::AK_ERR_INVALID,
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
            code: crate::core::AK_ERR_NULL,
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
            code: crate::core::AK_ERR_NULL,
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

/// Resolve the engine handle or fail with `-1`.
// SAFETY discipline identical to shim.rs's `engine_or_fail`.
fn engine_or_fail<'a>(engine: *const AkEngine) -> Option<&'a AkEngine> {
    // SAFETY: the raw handle comes from ak_engine_new; the caller keeps it
    // alive, un-freed and un-concurrent for the duration of this call, so
    // the shared borrow cannot dangle or alias a mutation (the same C
    // contract every shim export relies on).
    unsafe { engine.as_ref() }
}

/// `ak_task_start`: validate the spec, bootstrap the identity + default
/// save folder, register the runtime. `AK_OK` with the fresh 16-byte
/// `runtime_id` written; the `runtime_ready` event follows in the task
/// event queue (read via `ak_pairing_poll_event`).
///
/// # Safety
///
/// SAFETY (caller contract): `spec_json` must be non-null, NUL-terminated,
/// valid UTF-8, readable up to its NUL for the duration of the call;
/// `runtime_id_out` must be non-null and writable for 16 bytes (checked
/// BEFORE the spec is parsed or anything is registered). The engine handle
/// contract is that of `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_start(
    engine: *mut AkEngine,
    spec_json: *const c_char,
    runtime_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // Output pointer first: a failed start must never leave side effects
    // behind (the F6 discipline of every start export).
    if runtime_id_out.is_null() {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "runtime_id_out argument is null".to_string(),
        });
    }
    // SAFETY: read_cstr verifies non-null and UTF-8; the borrow is
    // confined to this call.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| task::task_start(engine, spec)) {
        Ok(runtime_id) => match unsafe { write_id(runtime_id_out, &runtime_id) } {
            // SAFETY: write_id verifies non-null and writes exactly the 16
            // documented bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// The safe-layer entry every task op start delegates to.
type TaskOpLauncher = fn(&AkEngine, &[u8; 16], &str) -> Result<[u8; 16], FfiError>;

/// Shared shape of the two op starts (`create_code`, `join`): validate
/// every pointer argument before parsing, then mint the op id.
fn task_op_start(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    spec_json: *const c_char,
    op_id_out: *mut u8,
    launch: TaskOpLauncher,
) -> i32 {
    let Some(engine) = engine_or_fail(engine) else {
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
    // SAFETY: read_cstr verifies non-null and UTF-8.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| launch(engine, &runtime_id, spec)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly 16
            // bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// `ak_task_create_code` (async op; result via `ak_pairing_op_result`).
/// Spec: `{"role":"sender|receiver","candidates":["ip:port",...],
/// "ttl_secs":0,"request_id":"<32hex>"}`.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; `spec_json` non-null, NUL-terminated, valid UTF-8; `op_id_out`
/// non-null and writable for 16 bytes (all checked before any side
/// effect). Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_create_code(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    role_and_cands_json: *const c_char,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    task_op_start(
        engine,
        runtime_id,
        role_and_cands_json,
        op_id_out,
        task::task_create_code_start,
    )
}

/// `ak_task_join` (async op; result via `ak_pairing_op_result`). Spec:
/// `{"code":"AKT1-...","candidates":[...],"request_id":"<32hex>"}`.
///
/// # Safety
///
/// SAFETY (caller contract): as [`ak_task_create_code`].
#[no_mangle]
pub unsafe extern "C" fn ak_task_join(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    code_and_cands_json: *const c_char,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    task_op_start(
        engine,
        runtime_id,
        code_and_cands_json,
        op_id_out,
        task::task_join_start,
    )
}

/// `ak_task_send` (async op; result via `ak_pairing_op_result`). Spec:
/// `{"paths":["<本地路径>",...],"request_id":"<32hex>"}`; `conn_id` is the
/// 16-byte connection id of `ak_task_list`/the join result.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id`/`conn_id` non-null and readable
/// for 16 bytes; `paths_json` non-null, NUL-terminated, valid UTF-8;
/// `op_id_out` non-null and writable for 16 bytes (all checked before any
/// side effect). Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_send(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    conn_id: *const u8,
    paths_json: *const c_char,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    // SAFETY: same contract as runtime_id.
    let conn_id = match unsafe { read_id(conn_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    // SAFETY: read_cstr verifies non-null and UTF-8.
    let spec = match unsafe { read_cstr(paths_json) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| task::task_send_start(engine, &runtime_id, &conn_id, spec)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly 16
            // bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// `ak_task_list`: write the connection snapshot JSON (UTF-8 + NUL) into
/// `buf` — `AK_DONE` (1), `AK_ERR_BUFFER` when it does not fit (nothing
/// is consumed — retry with a larger `cap`), `AK_ERR_NOT_FOUND` for an
/// unknown task runtime.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; when non-null, `buf` writable for `cap` bytes, not modified
/// concurrently; `cap` must not overstate the real allocation.
#[no_mangle]
pub unsafe extern "C" fn ak_task_list(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    buf: *mut c_char,
    cap: usize,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    match guarded(|| task::task_list(engine, &runtime_id)) {
        Ok(json) => match unsafe { write_cstr(buf, cap, &json) } {
            // SAFETY: write_cstr checks null/bounds itself and writes only
            // within the caller-provided capacity.
            Ok(_) => AK_DONE,
            Err(WriteError::TooSmall { need, capacity }) => {
                fail_buffer(need, capacity, "the task connection snapshot JSON")
            }
            Err(WriteError::NullBuffer) => {
                core::set_last_error("buf argument is null");
                AK_ERR_NULL
            }
        },
        Err(error) => fail(error),
    }
}

/// `ak_task_disconnect`: sever one connection — or cancel a task code that
/// was generated but never joined (the id then names the create result's
/// `code_id`; wave-29 P2-04). Idempotent (an already-severed connection
/// and an already-cancelled code are silent `AK_OK`s);
/// `AK_ERR_NOT_FOUND` for an id that is neither, or an unknown runtime.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id`/`task_id` non-null and readable
/// for 16 bytes. Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_disconnect(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    task_id: *const u8,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    let conn_id = match unsafe { read_id(task_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match guarded(|| task::task_disconnect(engine, &runtime_id, &conn_id)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// `ak_task_set_save_dir`: the one default save folder (created eagerly;
/// `AK_ERR_INVALID` for an empty path or an unusable directory).
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; `path` non-null, NUL-terminated, valid UTF-8, readable up to
/// its NUL for the duration of the call. Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_set_save_dir(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    path: *const c_char,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    // SAFETY: read_cstr verifies non-null and UTF-8.
    let path = match unsafe { read_cstr(path) } {
        Ok(text) => text,
        Err(error) => return fail(error),
    };
    match guarded(|| task::task_set_save_dir(engine, &runtime_id, path)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// `ak_task_consent`: decide one pending `incoming_request`. `accept` is
/// the C-truthy form (`0` = decline, non-zero = accept). `AK_OK`, or
/// `AK_ERR_NOT_FOUND` for an unknown or already-decided request (the 30 s
/// window auto-declines on its own).
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id`/`job_id` non-null and readable
/// for 16 bytes. Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_consent(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    job_id: *const u8,
    accept: i32,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    let job_id = match unsafe { read_id(job_id) } {
        Ok(id) => id,
        Err(error) => return fail(error),
    };
    match guarded(|| task::task_consent(engine, &runtime_id, &job_id, accept != 0)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// `ak_task_set_event_notify` (CR-P2-05 改法第 1 条后半, 2026-11-12):
/// register or clear ONE task runtime's event DOORBELL. `notify` NULL =
/// clear (returns only after an in-flight invocation completed — the
/// caller may then destroy the callback); non-NULL = the callback Rust
/// invokes (with `ctx` verbatim) on the thread that queues each
/// subsequent task event. The poll (`ak_pairing_poll_event`) stays the
/// single authoritative channel — the doorbell carries NO payload.
/// `AK_OK`; `-1` null engine/runtime pointer; `-4` unknown runtime.
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes. `notify` (when non-NULL) must point to a valid C function of
/// the documented signature that does not unwind, does not block, and
/// does not re-enter this library — Rust calls it from arbitrary engine
/// threads while holding the runtime's notify slot (the unregister
/// lifetime guarantee). `ctx` is opaque to Rust (never dereferenced;
/// NULL allowed) and must stay valid — or at least stay a meaningful
/// token — until the next `ak_task_set_event_notify` on this runtime or
/// the engine's teardown. Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_set_event_notify(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    notify: Option<extern "C" fn(ctx: *mut c_void)>,
    ctx: *mut c_void,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
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
    let registration = notify.map(|func| (func, ctx as usize));
    match guarded(|| task::task_set_event_notify(engine, &runtime_id, registration)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// `ak_task_server_info` (G6 follow-through, batch G6b 2026-11-13;
/// ffi-v2.md §8.7): accept one ASYNC registry-revision query on the task
/// runtime — `AK_OK` with the 16-byte `op_id` written; the result reads
/// through the existing `ak_pairing_op_result` as
/// `{"op":"task_server_info","revision":"<build>"}` (`"revision":null` =
/// old server / failed query / degraded, NEVER an error). No spec, no
/// idempotency key: the Rust side caches per connection epoch and keeps
/// one query in flight, so repeats are cheap. `-1` null pointers; `-4`
/// unknown runtime. The network wait lives in the op body on the engine
/// runtime — nothing blocks in this synchronous call (ADR-003).
///
/// # Safety
///
/// SAFETY (caller contract): `runtime_id` non-null and readable for 16
/// bytes; `op_id_out` non-null and writable for 16 bytes (checked before
/// anything is registered). Engine handle as `ak_send_start`.
#[no_mangle]
pub unsafe extern "C" fn ak_task_server_info(
    engine: *mut AkEngine,
    runtime_id: *const u8,
    op_id_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    let Some(engine) = engine_or_fail(engine) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // Output pointer first: a failed query must never leave side effects
    // behind (the F6 discipline of every start export).
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
    match guarded(|| task::task_server_info_start(engine, &runtime_id)) {
        Ok(op_id) => match unsafe { write_id(op_id_out, &op_id) } {
            // SAFETY: write_id verifies non-null and writes exactly the 16
            // documented bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod abi_shape_tests {
    //! P0-01-style guard: pin every `ak_task_*` export's name AND full call
    //! shape (arity plus parameter types). Any drift — an added or removed
    //! parameter, a changed pointer type — fails COMPILATION here, the
    //! parameter-shape half of the guarantee `scripts/check_contracts.py`
    //! check 2 gives for the symbol set.

    use super::*;

    #[test]
    fn frozen_task_abi_shapes_are_pinned() {
        let _: unsafe extern "C" fn(*mut AkEngine, *const c_char, *mut u8) -> i32 = ak_task_start;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *const c_char, *mut u8) -> i32 =
            ak_task_create_code;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *const c_char, *mut u8) -> i32 =
            ak_task_join;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *mut c_char, usize) -> i32 =
            ak_task_list;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *const u8) -> i32 =
            ak_task_disconnect;
        let _: unsafe extern "C" fn(
            *mut AkEngine,
            *const u8,
            *const u8,
            *const c_char,
            *mut u8,
        ) -> i32 = ak_task_send;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *const c_char) -> i32 =
            ak_task_set_save_dir;
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *const u8, i32) -> i32 =
            ak_task_consent;
        // CR-P2-05 (2026-11-12): the doorbell registration export — the
        // one task-plane ADDITION (a nullable C fn pointer + an opaque
        // context token; ffi-v2.md §8.6).
        let _: unsafe extern "C" fn(
            *mut AkEngine,
            *const u8,
            Option<extern "C" fn(*mut c_void)>,
            *mut c_void,
        ) -> i32 = ak_task_set_event_notify;
        // G6 follow-through (batch G6b, 2026-11-13): the registry-revision
        // query export — the second task-plane ADDITION (a spec-less op
        // start; ffi-v2.md §8.7).
        let _: unsafe extern "C" fn(*mut AkEngine, *const u8, *mut u8) -> i32 = ak_task_server_info;
    }
}

/// C02 fall-out for `task.rs`'s `#![forbid(unsafe_code)]` test module: the
/// exports are `unsafe extern "C" fn` now, and that module cannot spell
/// the `unsafe` blocks itself. Each adapter below is the audited one-line
/// call — every argument the tests pass is constructed right there (live
/// engine handles from `ak_engine_new`, 16-byte id arrays,
/// NUL-terminated `CString`s, buffers sized to their capacities) — and
/// nothing outside `#[cfg(test)]` can reach them.
#[cfg(test)]
pub(crate) mod testabi {
    use std::os::raw::{c_char, c_void};

    use crate::core::AkEngine;
    // The real exports, renamed so the same-name safe adapters below can
    // mirror their call shapes one-to-one.
    use crate::shim::{
        ak_engine_free as ffi_engine_free, ak_engine_last_error as ffi_engine_last_error,
        ak_pairing_op_result as ffi_pairing_op_result,
        ak_pairing_poll_event as ffi_pairing_poll_event,
    };
    use crate::task_ext::{
        ak_task_consent as ffi_task_consent, ak_task_create_code as ffi_task_create_code,
        ak_task_disconnect as ffi_task_disconnect, ak_task_join as ffi_task_join,
        ak_task_list as ffi_task_list, ak_task_send as ffi_task_send,
        ak_task_server_info as ffi_task_server_info,
        ak_task_set_event_notify as ffi_task_set_event_notify,
        ak_task_set_save_dir as ffi_task_set_save_dir, ak_task_start as ffi_task_start,
    };

    /// Safe as-is (no pointer arguments; C02 only made pointer-taking
    /// exports unsafe).
    pub(crate) use crate::shim::ak_engine_new;

    pub(crate) fn ak_engine_free(engine: *mut AkEngine) {
        // SAFETY: `engine` is null or the live, non-concurrently-used
        // handle `ak_engine_new` handed the test on this thread.
        unsafe { ffi_engine_free(engine) }
    }

    pub(crate) fn ak_engine_last_error(
        engine: *const AkEngine,
        buf: *mut c_char,
        cap: usize,
    ) -> i32 {
        // SAFETY: `buf` is the test's own buffer, writable for `cap`
        // bytes; the engine pointer is never dereferenced.
        unsafe { ffi_engine_last_error(engine, buf, cap) }
    }

    pub(crate) fn ak_pairing_op_result(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        op_id: *const u8,
        buf: *mut c_char,
        cap: usize,
    ) -> i32 {
        // SAFETY: both ids are the test's 16-byte arrays; `buf` is
        // writable for `cap` bytes and not touched concurrently.
        unsafe { ffi_pairing_op_result(engine, runtime_id, op_id, buf, cap) }
    }

    pub(crate) fn ak_pairing_poll_event(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        buf: *mut c_char,
        cap: usize,
    ) -> i32 {
        // SAFETY: the id is the test's 16-byte array; `buf` is writable
        // for `cap` bytes and not touched concurrently.
        unsafe { ffi_pairing_poll_event(engine, runtime_id, buf, cap) }
    }

    pub(crate) fn ak_task_start(
        engine: *mut AkEngine,
        spec_json: *const c_char,
        runtime_id_out: *mut u8,
    ) -> i32 {
        // SAFETY: `spec_json` is a NUL-terminated `CString` alive for the
        // call; `runtime_id_out` is a 16-byte test array.
        unsafe { ffi_task_start(engine, spec_json, runtime_id_out) }
    }

    pub(crate) fn ak_task_create_code(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        role_and_cands_json: *const c_char,
        op_id_out: *mut u8,
    ) -> i32 {
        // SAFETY: 16-byte id array, NUL-terminated spec, 16-byte out.
        unsafe { ffi_task_create_code(engine, runtime_id, role_and_cands_json, op_id_out) }
    }

    pub(crate) fn ak_task_join(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        code_and_cands_json: *const c_char,
        op_id_out: *mut u8,
    ) -> i32 {
        // SAFETY: 16-byte id array, NUL-terminated spec, 16-byte out.
        unsafe { ffi_task_join(engine, runtime_id, code_and_cands_json, op_id_out) }
    }

    pub(crate) fn ak_task_send(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        conn_id: *const u8,
        paths_json: *const c_char,
        op_id_out: *mut u8,
    ) -> i32 {
        // SAFETY: 16-byte id arrays, NUL-terminated spec, 16-byte out.
        unsafe { ffi_task_send(engine, runtime_id, conn_id, paths_json, op_id_out) }
    }

    pub(crate) fn ak_task_list(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        buf: *mut c_char,
        cap: usize,
    ) -> i32 {
        // SAFETY: 16-byte id array; `buf` writable for `cap` bytes.
        unsafe { ffi_task_list(engine, runtime_id, buf, cap) }
    }

    pub(crate) fn ak_task_disconnect(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        task_id: *const u8,
    ) -> i32 {
        // SAFETY: both ids are the test's 16-byte arrays.
        unsafe { ffi_task_disconnect(engine, runtime_id, task_id) }
    }

    pub(crate) fn ak_task_set_save_dir(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        path: *const c_char,
    ) -> i32 {
        // SAFETY: 16-byte id array; NUL-terminated path `CString`.
        unsafe { ffi_task_set_save_dir(engine, runtime_id, path) }
    }

    pub(crate) fn ak_task_consent(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        job_id: *const u8,
        accept: i32,
    ) -> i32 {
        // SAFETY: both ids are the test's 16-byte arrays.
        unsafe { ffi_task_consent(engine, runtime_id, job_id, accept) }
    }

    pub(crate) fn ak_task_set_event_notify(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        notify: Option<extern "C" fn(ctx: *mut c_void)>,
        ctx: *mut c_void,
    ) -> i32 {
        // SAFETY: the id is the test's 16-byte array; `notify` (when
        // given) is the test's own well-formed counter fn; `ctx` is a
        // sentinel address the counter records verbatim (never
        // dereferenced).
        unsafe { ffi_task_set_event_notify(engine, runtime_id, notify, ctx) }
    }

    pub(crate) fn ak_task_server_info(
        engine: *mut AkEngine,
        runtime_id: *const u8,
        op_id_out: *mut u8,
    ) -> i32 {
        // SAFETY: the id is the test's 16-byte array; `op_id_out` is a
        // 16-byte test array.
        unsafe { ffi_task_server_info(engine, runtime_id, op_id_out) }
    }
}
