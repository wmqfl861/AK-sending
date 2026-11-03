//! C ABI shim: the ONLY place in this crate (and, by the repo's rule, in
//! the repository) where `unsafe` is allowed. Every function below is a
//! thin pointer adapter; all logic lives in [`crate::core`] (compiled with
//! `forbid(unsafe_code)`). Each `unsafe` block carries a `SAFETY:` comment
//! covering pointer non-nullness, lifetime, and (for inputs) UTF-8
//! validation before any engine call.
//!
//! Calling conventions (the frozen contract):
//!
//! - Handles: [`ak_engine_new`] returns an owned `*mut AkEngine`;
//!   [`ak_engine_free`] is its only matching destructor. Calls on one
//!   handle must be serialized by the caller (one UI thread / isolate).
//! - Strings IN: NUL-terminated UTF-8 `*const c_char`; invalid UTF-8 or a
//!   missing NUL is an error return, never a panic.
//! - Strings OUT: caller-provided `*mut c_char` buffer plus capacity in
//!   bytes; the shim writes the payload plus one NUL and never writes
//!   beyond `cap`. On `AK_ERR_BUFFER` nothing was consumed — retry with a
//!   larger buffer (the required size is in the last-error message).
//! - Job ids: opaque 16-byte values; copy them, do not interpret them.
//! - Status codes: `0` ok (plus `1`/`2` meanings per function), negative
//!   values are errors with a message in the thread-local last-error
//!   buffer (see [`ak_engine_last_error`]).
//!
//! `clippy::not_unsafe_ptr_arg_deref` is allowed module-wide: dereferencing
//! caller-provided pointers under a documented C contract is this module's
//! entire purpose. The Rust-side concern behind the lint (a safe Rust fn
//! handing out UB to Rust callers passing bogus pointers) does not apply to
//! the C/FFI frontends this ABI serves; every entry point re-validates
//! null/capacity/UTF-8 before touching a pointer and never panics across
//! the boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;

use crate::core::{
    self, guarded, AkEngine, FfiError, JobState, AK_DONE, AK_ERR_BUFFER, AK_ERR_INVALID,
    AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK, AK_PENDING,
};

/// Byte length of every job id crossing the boundary.
const JOB_ID_LEN: usize = 16;

/// Why a caller-buffer write failed; `need` is the full required size
/// including the NUL terminator.
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
/// SAFETY: `ptr` must be non-null and point to a NUL-terminated sequence
/// of bytes that stays valid and unmodified for the duration of `'a` (in
/// practice: the FFI call); `CStr::from_ptr` stops at the first NUL, so
/// the borrow never reads past the terminator. UTF-8 validity is checked
/// before the string is handed on.
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

/// Read one 16-byte job id.
///
/// # Safety
///
/// SAFETY: `ptr` must be non-null and readable for 16 bytes for the
/// duration of the call; the copy leaves the source untouched.
unsafe fn read_job(ptr: *const u8) -> Result<[u8; JOB_ID_LEN], FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: AK_ERR_NULL,
            message: "job argument is null".to_string(),
        });
    }
    let mut job = [0u8; JOB_ID_LEN];
    // SAFETY: ptr is non-null and caller-valid for 16 readable bytes.
    unsafe { std::ptr::copy_nonoverlapping(ptr, job.as_mut_ptr(), JOB_ID_LEN) };
    Ok(job)
}

/// Write one 16-byte job id.
///
/// # Safety
///
/// SAFETY: `ptr` must be non-null and writable for 16 bytes for the
/// duration of the call; exactly 16 bytes are written.
unsafe fn write_job(ptr: *mut u8, job: &[u8; JOB_ID_LEN]) -> Result<(), FfiError> {
    if ptr.is_null() {
        return Err(FfiError {
            code: AK_ERR_NULL,
            message: "job_out argument is null".to_string(),
        });
    }
    // SAFETY: ptr is non-null and caller-valid for 16 writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(job.as_ptr(), ptr, JOB_ID_LEN) };
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

/// Create a new engine handle: an embedded 2-worker tokio runtime plus a
/// fresh job table and progress queue.
///
/// Returns an owned `*mut AkEngine`, or null on failure (see the
/// thread-local last error on the SAME thread; the message is only set on
/// the thread that observed the failure).
#[no_mangle]
pub extern "C" fn ak_engine_new() -> *mut AkEngine {
    core::clear_last_error();
    let build = || {
        AkEngine::new().ok_or_else(|| FfiError::internal("cannot build the embedded tokio runtime"))
    };
    match guarded(build) {
        Ok(engine) => Box::into_raw(Box::new(engine)),
        Err(error) => {
            fail(error);
            std::ptr::null_mut()
        }
    }
}

/// Destroy an engine handle returned by [`ak_engine_new`]. Frees the
/// runtime and aborts every still-running job first. Passing null is a
/// no-op. The handle must not be used after this call.
#[no_mangle]
pub extern "C" fn ak_engine_free(engine: *mut AkEngine) {
    core::clear_last_error();
    if engine.is_null() {
        return;
    }
    // SAFETY: `engine` is null-checked above; otherwise it is the unique
    // handle from ak_engine_new, not used concurrently or after this call,
    // and `Box::from_raw` is the matching deallocation for `Box::into_raw`.
    unsafe { drop(Box::from_raw(engine)) };
}

/// Copy the thread-local last-error message (UTF-8 + NUL) into `buf`.
///
/// `engine` is reserved for future per-engine error contexts and is not
/// dereferenced; it may be null. The message describes the most recent
/// call on THIS thread and is cleared at the start of every other call,
/// so read it immediately after a failure return.
///
/// Returns the message byte length (excluding the NUL), `AK_ERR_NULL`
/// when `buf` is null, or `AK_ERR_BUFFER` when `cap` is too small — the
/// stored message is NOT modified or truncated, so the retry is lossless.
#[no_mangle]
pub extern "C" fn ak_engine_last_error(
    engine: *const AkEngine,
    buf: *mut c_char,
    cap: usize,
) -> i32 {
    let _ = engine;
    let text = core::last_error_text();
    // SAFETY: write_cstr checks null/bounds itself and writes only within
    // the caller-provided capacity.
    match unsafe { write_cstr(buf, cap, &text) } {
        Ok(len) => len as i32,
        Err(WriteError::NullBuffer) => AK_ERR_NULL,
        Err(WriteError::TooSmall { .. }) => AK_ERR_BUFFER,
    }
}

/// Start a send job from a JSON spec (schema in the crate header).
///
/// `spec_json` is a NUL-terminated UTF-8 string. On success (`AK_OK`) the
/// fresh 16-byte job id is written to `job_out` (exactly 16 bytes). On
/// failure a negative code is returned and nothing is written.
#[no_mangle]
pub extern "C" fn ak_send_start(
    engine: *mut AkEngine,
    spec_json: *const c_char,
    job_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: the raw handle comes from ak_engine_new; the caller keeps it
    // alive and unused-by-free for the duration of this call, so the
    // shared borrow below cannot dangle or alias a mutation.
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_cstr verifies non-null and UTF-8; the borrow is confined
    // to this call, matching the caller's buffer lifetime.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(spec) => spec,
        Err(error) => return fail(error),
    };
    match guarded(|| engine.start_send(spec)) {
        Ok(job) => match unsafe { write_job(job_out, &job) } {
            // SAFETY: write_job verifies non-null and writes exactly the 16
            // documented bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// Start a receive job from a JSON spec (schema in the crate header),
/// same buffer conventions as [`ak_send_start`].
#[no_mangle]
pub extern "C" fn ak_receive_start(
    engine: *mut AkEngine,
    spec_json: *const c_char,
    job_out: *mut u8,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_cstr verifies non-null and UTF-8 before use.
    let spec = match unsafe { read_cstr(spec_json) } {
        Ok(spec) => spec,
        Err(error) => return fail(error),
    };
    match guarded(|| engine.start_receive(spec)) {
        Ok(job) => match unsafe { write_job(job_out, &job) } {
            // SAFETY: write_job verifies non-null and writes exactly 16
            // bytes within the caller's buffer.
            Ok(()) => AK_OK,
            Err(error) => fail(error),
        },
        Err(error) => fail(error),
    }
}

/// Pop the oldest pending progress event and write it as one compact JSON
/// line (UTF-8 + NUL) into `buf`.
///
/// Returns `AK_EVENT` (1) when a line was written, `AK_PENDING` (0) when
/// the queue is empty, `AK_ERR_BUFFER` when `cap` is too small — the event
/// stays queued, nothing is lost, and the last-error message names the
/// needed size — or `AK_ERR_NULL`/`AK_ERR_INTERNAL` on bad arguments.
#[no_mangle]
pub extern "C" fn ak_poll_progress(engine: *mut AkEngine, buf: *mut c_char, cap: usize) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    let line = match guarded(|| engine.take_progress()) {
        Ok(Some(line)) => line,
        Ok(None) => return AK_PENDING,
        Err(error) => return fail(error),
    };
    // SAFETY: write_cstr checks null/bounds itself and writes only within
    // the caller-provided capacity.
    match unsafe { write_cstr(buf, cap, &line) } {
        Ok(_) => AK_EVENT,
        Err(WriteError::TooSmall { need, capacity }) => {
            if let Err(error) = engine.push_front_progress(line) {
                return fail(error);
            }
            fail_buffer(need, capacity, "a progress event")
        }
        Err(WriteError::NullBuffer) => {
            if let Err(error) = engine.push_front_progress(line) {
                return fail(error);
            }
            core::set_last_error("buf argument is null");
            AK_ERR_NULL
        }
    }
}

/// Fetch one job's terminal state.
///
/// Returns `AK_DONE` (1) with the report JSON in `buf` on success,
/// `AK_PENDING` (0) while the job runs (`buf` untouched),
/// `AK_FAILED` (2) with an error JSON in `buf` on failure,
/// `AK_ERR_BUFFER` when `cap` is too small (retry losslessly with a
/// larger buffer), `AK_ERR_NOT_FOUND` for an unknown job id.
#[no_mangle]
pub extern "C" fn ak_job_result(
    engine: *mut AkEngine,
    job: *const u8,
    buf: *mut c_char,
    cap: usize,
) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_job verifies non-null and copies exactly the 16
    // documented bytes from the caller's buffer.
    let job = match unsafe { read_job(job) } {
        Ok(job) => job,
        Err(error) => return fail(error),
    };
    match guarded(|| engine.job_result(&job)) {
        Ok(JobState::Running) => AK_PENDING,
        Ok(JobState::Finished { ok, json }) => {
            // SAFETY: write_cstr checks null/bounds itself and writes only
            // within the caller-provided capacity.
            match unsafe { write_cstr(buf, cap, &json) } {
                Ok(_) if ok => AK_DONE,
                Ok(_) => AK_FAILED,
                Err(WriteError::TooSmall { need, capacity }) => {
                    fail_buffer(need, capacity, "the job result JSON")
                }
                Err(WriteError::NullBuffer) => {
                    core::set_last_error("buf argument is null");
                    AK_ERR_NULL
                }
            }
        }
        Err(error) => fail(error),
    }
}

/// Abort a running job. A job that already finished (either way) is left
/// untouched; an aborted job reports `AK_FAILED` with
/// `"cancelled by caller"` and never emits a trailing `done` event.
///
/// Returns `AK_OK`, or `AK_ERR_NOT_FOUND` for an unknown job id.
#[no_mangle]
pub extern "C" fn ak_job_cancel(engine: *mut AkEngine, job: *const u8) -> i32 {
    core::clear_last_error();
    // SAFETY: same unique-handle contract as ak_send_start.
    let Some(engine) = (unsafe { engine.as_ref() }) else {
        return fail(FfiError {
            code: AK_ERR_NULL,
            message: "engine handle is null".to_string(),
        });
    };
    // SAFETY: read_job verifies non-null and copies exactly 16 bytes.
    let job = match unsafe { read_job(job) } {
        Ok(job) => job,
        Err(error) => return fail(error),
    };
    match guarded(|| engine.job_cancel(&job)) {
        Ok(()) => AK_OK,
        Err(error) => fail(error),
    }
}

/// Write the library version string (UTF-8 + NUL) into `buf`, e.g.
/// `ak-ffi 1 (wire v1, caps 0x0007)`. Returns the written byte length
/// (excluding the NUL), `AK_ERR_NULL`, or `AK_ERR_BUFFER`.
#[no_mangle]
pub extern "C" fn ak_version(buf: *mut c_char, cap: usize) -> i32 {
    core::clear_last_error();
    let text = core::version_string();
    // SAFETY: write_cstr checks null/bounds itself and writes only within
    // the caller-provided capacity.
    match unsafe { write_cstr(buf, cap, &text) } {
        Ok(len) => len as i32,
        Err(WriteError::TooSmall { need, capacity }) => {
            fail_buffer(need, capacity, "the version string")
        }
        Err(WriteError::NullBuffer) => {
            core::set_last_error("buf argument is null");
            AK_ERR_NULL
        }
    }
}
