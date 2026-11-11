//! Pure scheduler domain model (wave 19, B0).
//!
//! Composition of the pieces (contract `docs/contracts/scheduler.md`):
//!
//! * [`crate::scheduler::config::SchedulerPolicy`] — two-dimensional concurrency modes and
//!   caps, with strict JSON parsing/validation;
//! * [`crate::scheduler::key`] — grouping keys, task handles, permits;
//! * [`crate::scheduler::queue::FairQueue`] — bounded FIFO with group round-robin fairness
//!   and O(1) queue-full rejection;
//! * [`crate::scheduler::admission::decide`] — the pure four-mode admission decision;
//! * [`crate::scheduler::state`] — the contract §9 task states and scheduler events;
//! * [`crate::scheduler::core::SchedulerCore`] — the deterministic state machine gluing them
//!   together, driven by the B6 runtime actor.
//!
//! No networking, no clocks, no I/O: everything here is testable with
//! plain data.
pub mod admission;
pub mod config;
pub mod core;
pub mod key;
pub mod queue;
pub mod state;

pub use admission::{ActiveGroups, ActiveTasks, AdmissionDecision, DenyReason};
pub use config::{
    CrossCodeMode, EffectiveLimits, PolicyError, SameCodeMode, SchedulerPolicy,
    DEFAULT_MAX_ACTIVE_TASKS, DEFAULT_MAX_PENDING_ADMISSIONS, DEFAULT_MAX_QUEUED_TASKS_PER_CODE,
    MAX_ACTIVE_CODE_CONNECTIONS, MAX_ACTIVE_TASKS, MAX_ACTIVE_TASKS_PER_CODE,
    MAX_PENDING_ADMISSIONS, MAX_QUEUED_TASKS, MAX_QUEUED_TASKS_PER_CODE,
};
pub use core::{CancelOutcome, FinishResult, PolicySwitch, SchedulerCore};
pub use key::{
    CodeGroupKey, CodeId, Permit, PermitId, PermitState, RegistryId, TaskHandle, TaskId,
};
pub use queue::{FairQueue, QueueError};
pub use state::{FinishOutcome, SchedulerEvent, TaskPhase, TaskState};
