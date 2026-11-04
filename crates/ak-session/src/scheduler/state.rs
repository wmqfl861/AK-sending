//! Task lifecycle states and scheduler events.
//!
//! [`TaskState`] is exactly the frozen state set of contract §9 (its
//! mermaid diagram); terminal states are `Completed` and `Cancelled`. The
//! scheduler core manages `Queued`/`Paused`/`Cancelled`/`Completed`
//! directly; the runtime reports progress through the remaining states via
//! [`SchedulerCore::report_phase`](crate::scheduler::core::SchedulerCore::report_phase),
//! whose transition table mirrors the contract edges (including
//! `CancelRequested → Completed` when the final commit won the race).
use ak_core::error::ErrorCode;

use crate::scheduler::key::{CodeGroupKey, TaskHandle};

/// Lifecycle state of one task (contract §9, frozen vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Waiting in the local queue for admission.
    Queued,
    /// Admitted; resolving the pairing relationship.
    WaitingPairing,
    /// Admitted or reconnecting; waiting for a usable direct path
    /// (checkpoints are kept; a long wait must not occupy a transfer slot).
    WaitingDirect,
    /// Waiting for the remote end's admission decision (`TaskOpen`/
    /// `TaskAdmission` handshake over the direct channel).
    AwaitingAdmission,
    /// Scanning sources after both ends granted the permit.
    Scanning,
    /// File bytes are moving over the direct channel.
    Transferring,
    /// Verifying hashes and metadata fidelity.
    Verifying,
    /// Final target commit (staging → destination).
    Committing,
    /// Terminal success; persisted only after verification and commit.
    Completed,
    /// Source authorization lost (e.g. Android permission revoked);
    /// re-queues after re-authorization.
    WaitingPermission,
    /// User pause: slot released, checkpoint kept, re-queues on resume.
    Paused,
    /// Cancellation requested; the runtime is stopping subtasks and waits
    /// for them to exit.
    CancelRequested,
    /// Terminal cancellation.
    Cancelled,
}

impl TaskState {
    /// Stable state name, matching the contract vocabulary used by
    /// persistence (restore rules in `storage-schema.md` §7).
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Queued => "Queued",
            TaskState::WaitingPairing => "WaitingPairing",
            TaskState::WaitingDirect => "WaitingDirect",
            TaskState::AwaitingAdmission => "AwaitingAdmission",
            TaskState::Scanning => "Scanning",
            TaskState::Transferring => "Transferring",
            TaskState::Verifying => "Verifying",
            TaskState::Committing => "Committing",
            TaskState::Completed => "Completed",
            TaskState::WaitingPermission => "WaitingPermission",
            TaskState::Paused => "Paused",
            TaskState::CancelRequested => "CancelRequested",
            TaskState::Cancelled => "Cancelled",
        }
    }

    /// Terminal states (`Completed`, `Cancelled`); they never restart
    /// within the same task instance (contract §9).
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Completed | TaskState::Cancelled)
    }

    /// States in which a task holds no permit and is neither queued nor
    /// paused — the runtime may `yield_slot`/`requeue` from here
    /// (contract §7: waiting for a direct path or a remote permit must not
    /// occupy a transfer slot long-term).
    pub fn is_waiting_phase(&self) -> bool {
        matches!(
            self,
            TaskState::WaitingPairing
                | TaskState::WaitingDirect
                | TaskState::AwaitingAdmission
                | TaskState::WaitingPermission
        )
    }

    /// States that occupy transfer service (permit held).
    pub fn is_active_transfer(&self) -> bool {
        matches!(
            self,
            TaskState::Scanning
                | TaskState::Transferring
                | TaskState::Verifying
                | TaskState::Committing
        )
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Runtime-reported progress phase. Maps onto [`TaskState`]; the scheduler
/// validates transitions against the contract diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskPhase {
    /// Pairing resolution started.
    WaitingPairing,
    /// Direct path being established (or re-established after a drop).
    WaitingDirect,
    /// Remote admission handshake in flight.
    AwaitingAdmission,
    /// Source scan started (first transfer-service activity).
    Scanning,
    /// File transfer started.
    Transferring,
    /// Verification started.
    Verifying,
    /// Final commit started.
    Committing,
    /// Source authorization lost (e.g. Android permission).
    WaitingPermission,
    /// Cancellation requested; subtasks are stopping.
    CancelRequested,
}

impl TaskPhase {
    /// The matching task state.
    pub fn as_state(&self) -> TaskState {
        match self {
            TaskPhase::WaitingPairing => TaskState::WaitingPairing,
            TaskPhase::WaitingDirect => TaskState::WaitingDirect,
            TaskPhase::AwaitingAdmission => TaskState::AwaitingAdmission,
            TaskPhase::Scanning => TaskState::Scanning,
            TaskPhase::Transferring => TaskState::Transferring,
            TaskPhase::Verifying => TaskState::Verifying,
            TaskPhase::Committing => TaskState::Committing,
            TaskPhase::WaitingPermission => TaskState::WaitingPermission,
            TaskPhase::CancelRequested => TaskState::CancelRequested,
        }
    }

    /// Whether the phase occupies transfer service (drives the
    /// `TaskStarted` event on first report).
    pub fn is_active_transfer(&self) -> bool {
        matches!(
            self,
            TaskPhase::Scanning
                | TaskPhase::Transferring
                | TaskPhase::Verifying
                | TaskPhase::Committing
        )
    }
}

/// How an active task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishOutcome {
    /// Success: persisted after verification and commit.
    Completed,
    /// Failure with a stable error code. The scheduler releases the slot
    /// and re-queues the task per the contract diagram
    /// (`AwaitingAdmission → Queued`); retry budget and backoff timing are
    /// the runtime's decisions, taken before reporting here.
    Failed {
        /// Stable wire error code, kept as the record's
        /// `last_stable_error_code`.
        error: ErrorCode,
    },
}

/// One observable scheduler event, in order of occurrence.
///
/// The runtime (B6) drains these with `take_events` and forwards them to
/// UI/FFI; progress events may be coalesced but terminal outcomes are
/// always emitted exactly once per task (contract §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    /// A task entered the local queue.
    TaskQueued {
        /// The task.
        task: TaskHandle,
        /// The code group it belongs to.
        group: CodeGroupKey,
    },
    /// Admission granted: the task now holds a permit and one active slot
    /// of its group.
    TaskAdmitted {
        /// The task.
        task: TaskHandle,
        /// The code group.
        group: CodeGroupKey,
        /// The granted local permit.
        permit: crate::scheduler::key::PermitId,
    },
    /// The task's transfer-service activity began (first
    /// `Scanning`/`Transferring`/`Verifying`/`Committing` phase report).
    TaskStarted {
        /// The task.
        task: TaskHandle,
    },
    /// The task left the active set with a terminal or retry outcome.
    TaskFinished {
        /// The task.
        task: TaskHandle,
        /// The code group.
        group: CodeGroupKey,
        /// How it ended.
        outcome: FinishOutcome,
    },
    /// The task was cancelled (from the queue or while active).
    TaskCancelled {
        /// The task.
        task: TaskHandle,
        /// The code group.
        group: CodeGroupKey,
        /// `true` when the task held a permit (was admitted); `false` when
        /// it was cancelled straight out of the queue.
        was_running: bool,
    },
    /// The policy changed; `draining` says the active set currently exceeds
    /// the new (stricter) envelope.
    PolicyChanged {
        /// Monotonic revision, starting at 1 and bumped on every change.
        revision: u64,
        /// Whether the scheduler entered draining due to this change.
        draining: bool,
    },
    /// Draining started (parallel → serial switch with an over-limit active
    /// set): in-flight tasks keep running and converge naturally.
    Draining {
        /// Active tasks at the moment draining began.
        active_tasks: u32,
    },
}
