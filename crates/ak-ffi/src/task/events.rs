//! The op-result / event-channel extension of the pairing exports.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;

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
