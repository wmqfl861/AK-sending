//! The scheduler core: one deterministic state machine over queue, policy,
//! admission and permits.
//!
//! [`SchedulerCore`] is the pure domain half of the "scheduler actor" of
//! contract §5: it owns no channels, no clocks and no I/O. The B6 runtime
//! wraps it, forwards external commands as method calls, drains
//! [`SchedulerEvent`]s with [`SchedulerCore::take_events`], and executes the
//! admitted tasks against the engine.
//!
//! Core invariants after every public mutating call:
//!
//! * every admitted task holds exactly one live permit;
//! * `active_total` equals the number of live permits and the sum of the
//!   per-group active counts;
//! * no queued task is admissible under the current policy (the core pumps
//!   after every mutation), except while it is being drained or a group is
//!   blocked by a serial constraint;
//! * the queue never silently drops or reorders tasks (cancellation is the
//!   only removal).
use std::collections::HashMap;

use ak_core::error::ErrorCode;

use crate::scheduler::admission::{self, ActiveGroups, ActiveTasks, AdmissionDecision};
use crate::scheduler::config::SchedulerPolicy;
use crate::scheduler::key::{CodeGroupKey, Permit, PermitId, TaskHandle, TaskId};
use crate::scheduler::queue::{FairQueue, QueueError};
use crate::scheduler::state::{FinishOutcome, SchedulerEvent, TaskPhase, TaskState};

/// Per-task bookkeeping.
#[derive(Debug, Clone)]
struct TaskRecord {
    state: TaskState,
    group: CodeGroupKey,
    enqueue_seq: u64,
    last_error: Option<ErrorCode>,
    started_reported: bool,
}

/// Result of [`SchedulerCore::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishResult {
    /// Terminal success recorded.
    Completed,
    /// Failure recorded and the task re-entered the queue for a later
    /// attempt (retry budget/backoff are the runtime's concern).
    FailedRequeued,
    /// Failure recorded; the queue was full so the task is parked
    /// (`Queued` state, not in the queue). Call
    /// [`SchedulerCore::requeue`] once capacity frees.
    FailedParkedQueueFull,
}

/// Result of [`SchedulerCore::cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The task was sitting in the queue; removed without ever connecting.
    RemovedQueued,
    /// The task held a permit; the permit was released, the slot returned,
    /// and queued work was admitted to backfill.
    CancelledRunning,
    /// Unknown task id or already terminal; nothing changed.
    NotActive,
}

/// Result of [`SchedulerCore::set_policy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicySwitch {
    /// The new policy revision.
    pub revision: u64,
    /// Whether the switch put the scheduler into draining (the active set
    /// exceeds the new envelope; in-flight tasks converge naturally).
    pub draining: bool,
    /// How many queued tasks were admitted immediately by the switch
    /// (`serial → parallel` backfills at once).
    pub admitted: usize,
}

/// Contract §9 transition table for runtime-reported phases (anything not
/// listed is rejected; `CancelRequested` is accepted from every
/// non-terminal state).
fn phase_allowed(from: TaskState, phase: TaskPhase) -> bool {
    use TaskPhase as P;
    use TaskState as S;
    if from.is_terminal() {
        return false;
    }
    if phase == P::CancelRequested {
        return true;
    }
    matches!(
        (from, phase),
        (S::WaitingPairing, P::WaitingDirect)
            | (S::WaitingDirect, P::AwaitingAdmission)
            | (S::AwaitingAdmission, P::Scanning)
            | (S::Scanning, P::Transferring | P::WaitingPermission)
            | (S::Transferring, P::Verifying | P::WaitingDirect)
            | (S::Verifying, P::Committing)
    )
}

/// Deterministic scheduler state machine (see the module docs).
#[derive(Debug)]
pub struct SchedulerCore {
    policy: SchedulerPolicy,
    policy_revision: u64,
    enqueue_seq_counter: u64,
    permit_seq: u64,
    queue: FairQueue,
    records: HashMap<TaskId, TaskRecord>,
    active_per_group: HashMap<CodeGroupKey, u32>,
    active_total: u32,
    permits: HashMap<TaskId, Permit>,
    events: Vec<SchedulerEvent>,
}

impl SchedulerCore {
    /// Build a core under `policy` (revision starts at 1).
    pub fn new(policy: SchedulerPolicy) -> Self {
        let queue = FairQueue::new(policy.max_queued_tasks as usize);
        SchedulerCore {
            policy,
            policy_revision: 1,
            enqueue_seq_counter: 0,
            permit_seq: 0,
            queue,
            records: HashMap::new(),
            active_per_group: HashMap::new(),
            active_total: 0,
            permits: HashMap::new(),
            events: Vec::new(),
        }
    }

    // ---- Queries ------------------------------------------------------

    /// Current policy.
    pub fn policy(&self) -> SchedulerPolicy {
        self.policy
    }

    /// Monotonic policy revision (starts at 1, bumped by
    /// [`SchedulerCore::set_policy`]).
    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    /// Whether the active set exceeds the current policy envelope
    /// (`parallel → serial` draining).
    pub fn is_draining(&self) -> bool {
        self.exceeds_envelope()
    }

    /// Current state of one task.
    pub fn task_state(&self, task: &TaskHandle) -> Option<TaskState> {
        self.records.get(&task.task_id()).map(|rec| rec.state)
    }

    /// The group a task belongs to.
    pub fn task_group(&self, task: &TaskHandle) -> Option<CodeGroupKey> {
        self.records.get(&task.task_id()).map(|rec| rec.group)
    }

    /// Last stable error code reported for the task
    /// (`last_stable_error_code` in the persistence schema).
    pub fn last_error(&self, task: &TaskHandle) -> Option<ErrorCode> {
        self.records
            .get(&task.task_id())
            .and_then(|rec| rec.last_error)
    }

    /// Monotonic enqueue sequence (updated on every (re)queue; used for
    /// stable persistence ordering).
    pub fn enqueue_seq(&self, task: &TaskHandle) -> Option<u64> {
        self.records.get(&task.task_id()).map(|rec| rec.enqueue_seq)
    }

    /// Total admitted (permit-holding) tasks.
    pub fn active_total(&self) -> u32 {
        self.active_total
    }

    /// Distinct active code groups.
    pub fn active_group_count(&self) -> usize {
        self.active_per_group.len()
    }

    /// Admitted tasks of one group.
    pub fn active_in_group(&self, group: &CodeGroupKey) -> u32 {
        self.active_per_group.get(group).copied().unwrap_or(0)
    }

    /// Number of queued tasks.
    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }

    /// Ready-ring order of the queued groups (fair-rotation snapshot).
    pub fn queue_ready_order(&self) -> Vec<CodeGroupKey> {
        self.queue.ready_order()
    }

    /// Whether the task is currently sitting in the queue.
    pub fn task_in_queue(&self, task: &TaskHandle) -> bool {
        self.queue.contains(&task.task_id())
    }

    /// Number of live (granted) permits; always equals
    /// [`SchedulerCore::active_total`]. A drop after finish/cancel is the
    /// permit-leak check.
    pub fn live_permits(&self) -> usize {
        self.permits.len()
    }

    /// The live permit id of an admitted task.
    pub fn permit_of(&self, task: &TaskHandle) -> Option<PermitId> {
        self.permits.get(&task.task_id()).map(Permit::id)
    }

    /// Drain and clear the pending event log, in order of occurrence.
    pub fn take_events(&mut self) -> Vec<SchedulerEvent> {
        std::mem::take(&mut self.events)
    }

    // ---- Commands -----------------------------------------------------

    /// Submit a task for `group`.
    ///
    /// Rejects with [`QueueError::QueueFull`] immediately and without
    /// scanning (contract §7), leaving the queue and every existing task
    /// untouched. On success the task is queued, a `TaskQueued` event is
    /// logged and admission is pumped.
    pub fn enqueue(&mut self, task: TaskHandle, group: CodeGroupKey) -> Result<(), QueueError> {
        let id = task.task_id();
        if self.records.contains_key(&id) {
            return Err(QueueError::DuplicateTask);
        }
        self.queue.enqueue(&group, id)?;
        self.enqueue_seq_counter += 1;
        self.records.insert(
            id,
            TaskRecord {
                state: TaskState::Queued,
                group,
                enqueue_seq: self.enqueue_seq_counter,
                last_error: None,
                started_reported: false,
            },
        );
        self.events.push(SchedulerEvent::TaskQueued { task, group });
        self.pump();
        Ok(())
    }

    /// Try to admit queued tasks under the current policy until nothing
    /// admissible remains. Returns the number of admissions.
    ///
    /// Groups are visited in ready-ring order; within a group, tasks are
    /// admitted while [`admission::decide`] says `Admit` (same-group
    /// parallelism fills up to the cap). A denied head task is put back at
    /// the exact same position, so denial never reorders the queue.
    pub fn pump(&mut self) -> usize {
        let order = self.queue.ready_order();
        let mut admitted = 0;
        for group in order {
            while let Some(task) = self.queue.pop_for(&group) {
                let decision = self.decide_for(&group);
                match decision {
                    AdmissionDecision::Admit => {
                        self.grant(task, &group);
                        admitted += 1;
                    }
                    AdmissionDecision::Deny(_) => {
                        self.queue.requeue_front(&group, task);
                        break;
                    }
                }
            }
        }
        admitted
    }

    /// Pure admission probe for one group against the live active set
    /// (useful for the runtime to answer remote `TaskOpen` requests).
    pub fn decide_for(&self, group: &CodeGroupKey) -> AdmissionDecision {
        let active_groups = ActiveGroups::from_keys(self.active_per_group.keys().copied());
        let active_tasks = ActiveTasks::from_counts(
            self.active_per_group
                .iter()
                .map(|(key, count)| (*key, *count)),
        );
        admission::decide(&self.policy, &active_groups, &active_tasks, group)
    }

    /// Report a finished task.
    ///
    /// `Completed` requires the task to be admitted, or to sit in
    /// `CancelRequested` (contract §9: the final commit may win the race
    /// against a requested cancellation); it then becomes the terminal
    /// `Completed` state. `Failed` releases the slot, records the stable
    /// error code and re-queues the task (or parks it when the queue is
    /// full); retry budget and backoff are decided by the runtime *before*
    /// reporting here. Returns `None` for unknown or terminal tasks.
    pub fn finish(&mut self, task: &TaskHandle, outcome: FinishOutcome) -> Option<FinishResult> {
        let id = task.task_id();
        let (state, group) = {
            let rec = self.records.get(&id)?;
            if rec.state.is_terminal() {
                return None;
            }
            (rec.state, rec.group)
        };
        match outcome {
            FinishOutcome::Completed => {
                let held = self.release_active(id);
                if !held && state != TaskState::CancelRequested {
                    return None; // a queued/paused task cannot complete
                }
                self.records
                    .get_mut(&id)
                    .expect("record checked above")
                    .state = TaskState::Completed;
                self.events.push(SchedulerEvent::TaskFinished {
                    task: *task,
                    group,
                    outcome,
                });
                self.pump();
                Some(FinishResult::Completed)
            }
            FinishOutcome::Failed { error } => {
                self.release_active(id);
                if let Some(rec) = self.records.get_mut(&id) {
                    rec.last_error = Some(error);
                }
                let parked = if self.queue.contains(&id) {
                    false // already queued (e.g. cancel-requested queue item)
                } else {
                    matches!(self.queue.enqueue(&group, id), Err(QueueError::QueueFull))
                };
                self.records
                    .get_mut(&id)
                    .expect("record checked above")
                    .state = TaskState::Queued;
                self.events.push(SchedulerEvent::TaskFinished {
                    task: *task,
                    group,
                    outcome: FinishOutcome::Failed { error },
                });
                self.pump();
                Some(if parked {
                    FinishResult::FailedParkedQueueFull
                } else {
                    FinishResult::FailedRequeued
                })
            }
        }
    }

    /// Cancel a task (contract §10 layering):
    ///
    /// * queued → removed from the queue without ever connecting;
    /// * admitted → permit released, slot returned, queued work backfills;
    /// * unknown/terminal → no-op.
    pub fn cancel(&mut self, task: &TaskHandle) -> CancelOutcome {
        let id = task.task_id();
        let group = match self.records.get(&id) {
            Some(rec) if !rec.state.is_terminal() => rec.group,
            _ => return CancelOutcome::NotActive,
        };
        if self.release_active(id) {
            self.records
                .get_mut(&id)
                .expect("record checked above")
                .state = TaskState::Cancelled;
            self.events.push(SchedulerEvent::TaskCancelled {
                task: *task,
                group,
                was_running: true,
            });
            self.pump();
            CancelOutcome::CancelledRunning
        } else {
            self.queue.cancel(&id);
            self.records
                .get_mut(&id)
                .expect("record checked above")
                .state = TaskState::Cancelled;
            self.events.push(SchedulerEvent::TaskCancelled {
                task: *task,
                group,
                was_running: false,
            });
            CancelOutcome::RemovedQueued
        }
    }

    /// User pause of an admitted task: releases the scheduling permit (a
    /// paused task must not monopolize a serial group, contract §8) and
    /// parks the task in `Paused`; resume via [`SchedulerCore::requeue`].
    pub fn pause(&mut self, task: &TaskHandle) -> bool {
        let id = task.task_id();
        let eligible = match self.records.get(&id) {
            Some(rec) => !rec.state.is_terminal() && rec.state != TaskState::CancelRequested,
            None => false,
        };
        if !eligible || !self.release_active(id) {
            return false;
        }
        self.records
            .get_mut(&id)
            .expect("record checked above")
            .state = TaskState::Paused;
        self.pump();
        true
    }

    /// Release the slot of a task stuck waiting for a direct path or a
    /// remote permit (contract §7) while keeping its current state; the
    /// runtime calls [`SchedulerCore::requeue`] to bring it back through
    /// admission. Only valid in a waiting phase.
    pub fn yield_slot(&mut self, task: &TaskHandle) -> bool {
        let id = task.task_id();
        let eligible = match self.records.get(&id) {
            Some(rec) => rec.state.is_waiting_phase(),
            None => false,
        };
        if !eligible || !self.release_active(id) {
            return false;
        }
        self.pump();
        true
    }

    /// Bring a parked task (paused, yielded while waiting, parked after a
    /// queue-full failure, or waiting for re-authorization) back into the
    /// queue; the pump then re-admits it under the current policy
    /// (contract §9 resume/recovery edges into `Queued`).
    pub fn requeue(&mut self, task: &TaskHandle) -> Result<bool, QueueError> {
        let id = task.task_id();
        let eligible = match self.records.get(&id) {
            Some(rec) => {
                !rec.state.is_terminal()
                    && rec.state != TaskState::CancelRequested
                    && !self.queue.contains(&id)
            }
            None => false,
        };
        let holds_permit = self.permits.contains_key(&id);
        if !eligible || holds_permit {
            return Ok(false);
        }
        let group = self.records.get(&id).expect("record checked above").group;
        self.queue.enqueue(&group, id)?;
        self.enqueue_seq_counter += 1;
        let rec = self.records.get_mut(&id).expect("record checked above");
        rec.state = TaskState::Queued;
        rec.enqueue_seq = self.enqueue_seq_counter;
        self.events
            .push(SchedulerEvent::TaskQueued { task: *task, group });
        self.pump();
        Ok(true)
    }

    /// Runtime progress report; validates the contract §9 edges.
    ///
    /// Progress phases require the task to hold its permit (a yielded or
    /// paused task must requeue and re-admit first); `CancelRequested` may
    /// additionally be reported for queued tasks. The first
    /// transfer-service phase (`Scanning`/`Transferring`/`Verifying`/
    /// `Committing`) emits `TaskStarted`. Returns whether the report was
    /// accepted.
    pub fn report_phase(&mut self, task: &TaskHandle, phase: TaskPhase) -> bool {
        let id = task.task_id();
        let from = match self.records.get(&id) {
            Some(rec) => rec.state,
            None => return false,
        };
        if !phase_allowed(from, phase) {
            return false;
        }
        if phase != TaskPhase::CancelRequested && !self.permits.contains_key(&id) {
            return false;
        }
        let rec = self.records.get_mut(&id).expect("record checked above");
        rec.state = phase.as_state();
        if phase.is_active_transfer() && !rec.started_reported {
            rec.started_reported = true;
            self.events
                .push(SchedulerEvent::TaskStarted { task: *task });
        }
        true
    }

    /// Switch the policy at runtime (contract §8):
    ///
    /// * `parallel → serial` with an over-limit active set enters draining
    ///   — `PolicyChanged{draining:true}` plus a `Draining` event are
    ///   emitted, in-flight tasks are untouched and converge naturally,
    ///   and `decide` keeps serializing new admissions;
    /// * `serial → parallel` backfills immediately: queued tasks are
    ///   admitted up to the new caps before this call returns.
    pub fn set_policy(&mut self, policy: SchedulerPolicy) -> PolicySwitch {
        self.policy = policy;
        self.queue.set_capacity(policy.max_queued_tasks as usize);
        self.policy_revision += 1;
        let draining = self.exceeds_envelope();
        self.events.push(SchedulerEvent::PolicyChanged {
            revision: self.policy_revision,
            draining,
        });
        if draining {
            self.events.push(SchedulerEvent::Draining {
                active_tasks: self.active_total,
            });
        }
        let admitted = self.pump();
        PolicySwitch {
            revision: self.policy_revision,
            draining,
            admitted,
        }
    }

    // ---- Internals ----------------------------------------------------

    /// Grant a permit to `task` for `group`, marking it admitted.
    fn grant(&mut self, task: TaskId, group: &CodeGroupKey) {
        self.permit_seq += 1;
        let permit_id = PermitId::from_counter(self.permit_seq);
        self.permits
            .insert(task, Permit::new_granted(permit_id, task, *group));
        *self.active_per_group.entry(*group).or_insert(0) += 1;
        self.active_total += 1;
        let rec = self
            .records
            .get_mut(&task)
            .expect("admitted task has a record");
        rec.state = TaskState::WaitingPairing;
        rec.started_reported = false;
        self.events.push(SchedulerEvent::TaskAdmitted {
            task: TaskHandle::new(task),
            group: *group,
            permit: permit_id,
        });
    }

    /// Return the permit of `task` (and its group slot). Returns whether a
    /// permit was actually held.
    fn release_active(&mut self, task: TaskId) -> bool {
        if self.permits.remove(&task).is_none() {
            return false;
        }
        let group = self
            .records
            .get(&task)
            .expect("permitted task has a record")
            .group;
        match self.active_per_group.get_mut(&group) {
            Some(count) => {
                *count -= 1;
                if *count == 0 {
                    self.active_per_group.remove(&group);
                }
            }
            // Counts are removed exactly when they reach zero, so a group
            // with a live permit always has an entry.
            None => unreachable!("active group must exist while its tasks hold permits"),
        }
        self.active_total -= 1;
        true
    }

    /// Whether the active set exceeds the effective envelope of the
    /// current policy (draining condition).
    fn exceeds_envelope(&self) -> bool {
        let limits = self.policy.effective_limits();
        self.active_per_group
            .values()
            .any(|count| *count > limits.max_tasks_per_group)
            || self.active_per_group.len() > limits.max_active_groups as usize
    }
}
