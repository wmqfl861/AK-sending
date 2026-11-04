//! Runtime-facing error taxonomy.
//!
//! `QUEUE_FULL` is the frozen wire-facing rejection of contract §7: the
//! enqueue is refused without scanning and without creating a connection.
//! Dynamic detail strings are bounded and stripped of control characters
//! before being embedded (same policy as the scheduler config parser).
use ak_core::error::ErrorCode;

/// Bound and sanitize an input-derived string for display.
fn sanitize(detail: &str) -> String {
    let mut out: String = detail
        .chars()
        .take(80)
        .map(|c| match c as u32 {
            0x00..=0x1F | 0x7F..=0x9F => '?',
            _ => c,
        })
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Why a runtime call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// The local queue is at capacity (`QUEUE_FULL`, contract §7): nothing
    /// was scanned, no connection was created, no queued task was dropped.
    #[error("QUEUE_FULL")]
    QueueFull,
    /// The task id is already known to this runtime.
    #[error("duplicate task")]
    DuplicateTask,
    /// No task with this id exists in this runtime.
    #[error("task not found")]
    TaskNotFound,
    /// The task exists but cannot take this action (e.g. terminal, or not
    /// parked for a retry).
    #[error("task not active for this operation")]
    NotActive,
    /// The runtime received the command after shutdown began.
    #[error("runtime is shutting down")]
    ShuttingDown,
    /// A second shutdown was requested while one is in progress.
    #[error("shutdown already in progress")]
    AlreadyShuttingDown,
    /// The runtime actor has exited; the handle can no longer be used.
    #[error("runtime stopped")]
    RuntimeStopped,
    /// `SessionRuntime::start` was called outside a tokio runtime.
    #[error("session runtime requires a tokio runtime")]
    NotInRuntime,
    /// The submitted spec is malformed.
    #[error("invalid spec: {detail}")]
    InvalidSpec {
        /// Bounded sanitized description.
        detail: String,
    },
}

impl SessionError {
    /// Build [`SessionError::InvalidSpec`] with a sanitized detail.
    pub fn invalid_spec(detail: &str) -> Self {
        SessionError::InvalidSpec {
            detail: sanitize(detail),
        }
    }
}

/// Why an executor refused to start a task (mapped to the stable error
/// code recorded by the scheduler on the failure path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStartError {
    /// Stable error code carried into `FinishOutcome::Failed`.
    pub code: ErrorCode,
    /// Bounded sanitized detail for logs/diagnostics.
    pub detail: String,
}

impl EngineStartError {
    /// Create with a sanitized detail.
    pub fn new(code: ErrorCode, detail: &str) -> Self {
        EngineStartError {
            code,
            detail: sanitize(detail),
        }
    }
}

impl std::fmt::Display for EngineStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "engine start failed: {} ({})", self.code, self.detail)
    }
}

impl std::error::Error for EngineStartError {}
