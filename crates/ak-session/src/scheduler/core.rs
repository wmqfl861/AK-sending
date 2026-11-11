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
//!   after every mutation), except while it is being drained, a group is
//!   blocked by a serial constraint, or the cross-code serial batch gate
//!   defers a next-batch append (contract §8 finite batches);
//! * the queue never silently drops or reorders tasks (cancellation is the
//!   only removal).
use std::collections::HashMap;

use ak_core::error::ErrorCode;

use crate::scheduler::admission::{self, ActiveGroups, ActiveTasks, AdmissionDecision, DenyReason};
use crate::scheduler::config::{CrossCodeMode, SchedulerPolicy};
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

/// One cross-code serial batch (contract §8: finite batches).
///
/// While cross-code serial mode selects a group, the batch fixes which of
/// that group's tasks belong to the current turn:
///
/// * while `open` (from the group's first admission until its first slot
///   release — a batch task finishing, being cancelled, paused or yielding
///   its slot), `cutoff` tracks the enqueue counter, so the tasks filling
///   the group's parallelism, including appends that pile up behind the
///   cap, still belong to this batch;
/// * the first slot release closes the selection by freezing `cutoff`:
///   tasks of the batch group appended (or requeued) afterwards carry a
///   higher enqueue sequence and belong to the *next* batch.
///
/// A batch lives exactly as long as its group holds at least one active
/// task. When the last one releases, the batch ends and the ready ring
/// rotates to the next group — a hot code that keeps appending cannot
/// occupy the single cross-code serial slot forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossSerialBatch {
    /// The group owning this turn.
    group: CodeGroupKey,
    /// Highest enqueue sequence still belonging to this batch.
    cutoff: u64,
    /// Whether the selection window is still absorbing new appends.
    open: bool,
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
    /// Tasks holding a permit while still in a pre-transfer-service state
    /// (wave-24 `max_pending_admissions` bound; kept exact against
    /// `TaskRecord::state` by grant/release/report_phase).
    pending_admissions: u32,
    /// The live cross-code serial batch, if any (contract §8 finite
    /// batches; see [`CrossSerialBatch`]). `None` outside cross-code serial
    /// mode or between batches.
    cross_serial_batch: Option<CrossSerialBatch>,
    events: Vec<SchedulerEvent>,
}

impl SchedulerCore {
    /// Build a core under `policy` (revision starts at 1). The queue picks
    /// up both bounds of the policy: the global `max_queued_tasks` and the
    /// wave-24 per-code `max_queued_tasks_per_code`.
    pub fn new(policy: SchedulerPolicy) -> Self {
        let queue = FairQueue::with_caps(
            policy.max_queued_tasks as usize,
            policy.max_queued_tasks_per_code,
        );
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
            pending_admissions: 0,
            cross_serial_batch: None,
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

    /// Admitted tasks still in a pre-transfer-service state (the wave-24
    /// `max_pending_admissions` bound; see
    /// [`TaskState::is_pending_admission`]).
    pub fn pending_admissions(&self) -> u32 {
        self.pending_admissions
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
    ///
    /// Wave 24: an otherwise-admissible task is additionally held back
    /// (denied `WaitingCapacity`, left in the queue) while
    /// [`SchedulerCore::pending_admissions`] sits at the policy's
    /// `max_pending_admissions` bound — at most that many admission
    /// handshakes may be in flight at once, so the runtime never spawns a
    /// new task driver beyond the bound (the driver of a held-back task is
    /// simply not dispatched; it is dispatched by the next pump once a
    /// pending handshake completes or is released).
    ///
    /// Cross-code serial finite batches (contract §8): while a batch is
    /// live, an admissible task of the batch group whose enqueue sequence
    /// exceeds the batch cutoff is denied `WaitingSerialMode` — it belongs
    /// to the group's next batch. Every denial of the live batch group
    /// defers that group to the *back* of the ready ring (it already holds
    /// the turn; waiting groups must stay ahead of it), so when the batch
    /// ends the next group in rotation is selected instead of the hot
    /// group's appends.
    pub fn pump(&mut self) -> usize {
        self.refresh_cross_serial_batch();
        let order = self.queue.ready_order();
        let mut admitted = 0;
        for group in order {
            while let Some(task) = self.queue.pop_for(&group) {
                let mut decision = self.decide_for(&group);
                if decision.is_admitted()
                    && self.pending_admissions >= self.policy.max_pending_admissions as u32
                {
                    decision = AdmissionDecision::Deny(DenyReason::WaitingCapacity);
                }
                if decision.is_admitted() && self.batch_defers(&group, &task) {
                    decision = AdmissionDecision::Deny(DenyReason::WaitingSerialMode);
                }
                match decision {
                    AdmissionDecision::Admit => {
                        self.grant(task, &group);
                        admitted += 1;
                    }
                    AdmissionDecision::Deny(_) => {
                        if self.holds_live_batch(&group) {
                            self.queue.requeue_defer(&group, task);
                        } else {
                            self.queue.requeue_front(&group, task);
                        }
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
    ///
    /// Wave 24: a report that moves the task out of its admission handshake
    /// frees one `max_pending_admissions` slot and pumps immediately, so
    /// queued tasks are dispatched without waiting for the next external
    /// mutation (a report that moves the task back into a handshake phase
    /// — e.g. `Transferring → WaitingDirect` on a dropped path — re-takes
    /// the slot).
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
        let to = phase.as_state();
        let freed_pending_slot = from.is_pending_admission() && !to.is_pending_admission();
        let took_pending_slot = !from.is_pending_admission() && to.is_pending_admission();
        let rec = self.records.get_mut(&id).expect("record checked above");
        rec.state = to;
        if phase.is_active_transfer() && !rec.started_reported {
            rec.started_reported = true;
            self.events
                .push(SchedulerEvent::TaskStarted { task: *task });
        }
        if freed_pending_slot {
            self.pending_admissions = self.pending_admissions.saturating_sub(1);
            self.pump();
        } else if took_pending_slot {
            self.pending_admissions += 1;
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
        self.queue.set_capacity(policy.max_queued_tasks as usize);
        self.queue
            .set_per_group_capacity(policy.max_queued_tasks_per_code);
        self.policy = policy;
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
        // Selecting a group under cross-code serial opens its finite batch
        // (contract §8): everything of this group queued so far joins the
        // turn; appends arriving after the first slot release are next
        // batch (see [`CrossSerialBatch`]).
        if self.policy.cross_code_mode == CrossCodeMode::Serial && self.cross_serial_batch.is_none()
        {
            self.cross_serial_batch = Some(CrossSerialBatch {
                group: *group,
                cutoff: self.enqueue_seq_counter,
                open: true,
            });
        }
        self.permit_seq += 1;
        let permit_id = PermitId::from_counter(self.permit_seq);
        self.permits
            .insert(task, Permit::new_granted(permit_id, task, *group));
        *self.active_per_group.entry(*group).or_insert(0) += 1;
        self.active_total += 1;
        // A fresh admission starts its handshake (WaitingPairing), so it
        // occupies one pending-admission slot until it reports into the
        // transfer service.
        self.pending_admissions += 1;
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
        let rec = self
            .records
            .get(&task)
            .expect("permitted task has a record");
        let group = rec.group;
        // A task released while still in its handshake frees the
        // pending-admission slot it held.
        if rec.state.is_pending_admission() {
            self.pending_admissions = self.pending_admissions.saturating_sub(1);
        }
        // The first slot release of a batch task closes the cross-code
        // serial batch's selection window (contract §8 finite batches):
        // appends arriving from here on belong to the next batch.
        if let Some(batch) = self.cross_serial_batch.as_mut() {
            if batch.open && batch.group == group {
                batch.open = false;
            }
        }
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

    /// Reconcile the cross-code serial batch with reality at the top of
    /// every pump: a batch ends when its group no longer holds an active
    /// task (or the policy left cross-code serial mode), and an open batch
    /// keeps its cutoff level with the enqueue counter so appends of the
    /// batch group still join the turn while the selection window is open.
    fn refresh_cross_serial_batch(&mut self) {
        let Some(batch) = self.cross_serial_batch else {
            return;
        };
        let live = self.policy.cross_code_mode == CrossCodeMode::Serial
            && self.active_per_group.contains_key(&batch.group);
        if !live {
            self.cross_serial_batch = None;
        } else if batch.open && batch.cutoff != self.enqueue_seq_counter {
            self.cross_serial_batch = Some(CrossSerialBatch {
                cutoff: self.enqueue_seq_counter,
                ..batch
            });
        }
    }

    /// Whether `task` of `group` is an admissible task of the live batch
    /// group whose enqueue sequence places it in the *next* batch (the
    /// cross-code serial finite-batch gate; `false` in every other case).
    fn batch_defers(&self, group: &CodeGroupKey, task: &TaskId) -> bool {
        match self.cross_serial_batch {
            Some(batch) if batch.group == *group => self
                .records
                .get(task)
                .is_none_or(|rec| rec.enqueue_seq > batch.cutoff),
            _ => false,
        }
    }

    /// Whether `group` is the group of the live cross-code serial batch
    /// (its denials defer it to the back of the ready ring — see
    /// [`SchedulerCore::pump`]).
    fn holds_live_batch(&self, group: &CodeGroupKey) -> bool {
        self.cross_serial_batch
            .is_some_and(|batch| batch.group == *group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::config::{CrossCodeMode, SameCodeMode};
    use crate::scheduler::key::{CodeId, RegistryId};

    fn group(n: u8) -> CodeGroupKey {
        CodeGroupKey::new(RegistryId::from_bytes([n; 32]), CodeId::from_bytes([n; 16]))
    }

    fn task(n: u64) -> TaskHandle {
        TaskHandle::new(TaskId::from_counter(n))
    }

    /// Parallel-everything policy with individually settable wave-24 caps.
    fn wide_policy(
        max_active_tasks: usize,
        max_pending_admissions: usize,
        max_queued_tasks_per_code: usize,
    ) -> SchedulerPolicy {
        SchedulerPolicy {
            same_code_mode: SameCodeMode::Parallel,
            cross_code_mode: CrossCodeMode::Parallel,
            max_active_tasks_per_code: 16,
            max_active_code_connections: 16,
            max_queued_tasks: 256,
            max_active_tasks,
            max_queued_tasks_per_code,
            max_pending_admissions,
        }
    }

    /// Drive one admitted task through the handshake into the transfer
    /// service (freeing its pending-admission slot).
    fn enter_transfer_service(core: &mut SchedulerCore, task: &TaskHandle) {
        assert!(core.report_phase(task, TaskPhase::WaitingDirect));
        assert!(core.report_phase(task, TaskPhase::AwaitingAdmission));
        assert!(core.report_phase(task, TaskPhase::Scanning));
    }

    /// Wave 24 pending-admissions bound: with `max_pending_admissions = 2`
    /// only two of four ready tasks (distinct groups, so no other cap is in
    /// the way) are admitted; the moment one reports into the transfer
    /// service the pump dispatches the next.
    #[test]
    fn pending_admissions_bound_holds_and_dispatches() {
        let mut core = SchedulerCore::new(wide_policy(64, 2, 256));
        for n in 1..=4u64 {
            core.enqueue(task(n), group(n as u8)).unwrap();
        }
        assert_eq!(core.active_total(), 2, "only two handshakes in flight");
        assert_eq!(core.pending_admissions(), 2);
        assert_eq!(core.queued_len(), 2);
        // The held-back tasks stay queued (both groups still in the ring —
        // the relative rotation of two denied groups is the pump's
        // round-robin artifact, only membership matters here).
        let mut ready = core.queue_ready_order();
        ready.sort();
        let mut expect = vec![group(3), group(4)];
        expect.sort();
        assert_eq!(ready, expect);

        // Task 1 enters the transfer service: its slot frees and the pump
        // dispatches one held-back task (3 or 4 — which one depends on the
        // round-robin rotation of the two denied groups) without any other
        // mutation.
        enter_transfer_service(&mut core, &task(1));
        assert_eq!(core.pending_admissions(), 2, "freed slot refilled");
        assert_eq!(core.active_total(), 3);
        assert_eq!(core.queued_len(), 1);
        let candidates = [task(3), task(4)];
        let dispatched = candidates
            .iter()
            .find(|t| core.task_state(t) == Some(TaskState::WaitingPairing))
            .expect("one held-back task was dispatched");
        let held_back = if dispatched == &task(3) {
            task(4)
        } else {
            task(3)
        };
        assert_eq!(core.task_state(&held_back), Some(TaskState::Queued));
        assert_eq!(core.task_state(&task(1)), Some(TaskState::Scanning));

        // Task 2 finishes while still pending (handshake failed): its slot
        // frees the same way and the other held-back task dispatches.
        let result = core.finish(
            &task(2),
            FinishOutcome::Failed {
                error: ErrorCode::DirectUnavailable,
            },
        );
        assert_eq!(result, Some(FinishResult::FailedRequeued));
        assert_eq!(core.active_total(), 3);
        assert_eq!(
            core.task_state(&held_back),
            Some(TaskState::WaitingPairing),
            "the second held-back task dispatched on the freed slot"
        );
        assert_eq!(core.queued_len(), 1, "task 2 rejoined the queue behind it");
    }

    /// A waiting-phase task cancelled mid-handshake also returns its
    /// pending-admission slot — and the freed slot backfills immediately.
    #[test]
    fn cancelling_a_pending_handshake_frees_the_slot() {
        let mut core = SchedulerCore::new(wide_policy(64, 1, 256));
        core.enqueue(task(1), group(1)).unwrap();
        core.enqueue(task(2), group(2)).unwrap();
        assert_eq!(core.pending_admissions(), 1);
        assert_eq!(core.queued_len(), 1);

        assert_eq!(core.cancel(&task(1)), CancelOutcome::CancelledRunning);
        assert_eq!(core.task_state(&task(1)), Some(TaskState::Cancelled));
        assert_eq!(
            core.task_state(&task(2)),
            Some(TaskState::WaitingPairing),
            "the freed slot backfilled task 2"
        );
        assert_eq!(core.active_total(), 1);
        assert_eq!(core.pending_admissions(), 1);
    }

    /// `Transferring → WaitingDirect` re-enters the handshake and re-takes
    /// a pending slot; the re-entry cannot bypass the bound retroactively
    /// (it held a permit throughout), but the counter must track it.
    #[test]
    fn re_entering_the_handshake_re_takes_the_slot() {
        let mut core = SchedulerCore::new(wide_policy(64, 2, 256));
        core.enqueue(task(1), group(1)).unwrap();
        enter_transfer_service(&mut core, &task(1));
        assert!(core.report_phase(&task(1), TaskPhase::Transferring));
        assert_eq!(core.pending_admissions(), 0);
        assert!(core.report_phase(&task(1), TaskPhase::WaitingDirect));
        assert_eq!(
            core.pending_admissions(),
            1,
            "path re-established: handshake again"
        );
    }

    /// The per-code queue bound flows from the policy into the core's
    /// queue, on construction and through `set_policy`. Both dimensions
    /// serial keeps the extra tasks queued (only one admits), which is
    /// what the bound governs.
    #[test]
    fn core_queue_honors_the_per_code_bound() {
        let mut policy = wide_policy(64, 64, 2);
        policy.same_code_mode = SameCodeMode::Serial;
        policy.cross_code_mode = CrossCodeMode::Serial;
        let mut core = SchedulerCore::new(policy);
        let hot = group(1);
        core.enqueue(task(1), hot).unwrap(); // admitted (serial: 1 active)
        core.enqueue(task(2), hot).unwrap(); // queued
        core.enqueue(task(3), hot).unwrap(); // queued — the per-code bound (2)
        assert_eq!(
            core.enqueue(task(4), hot),
            Err(QueueError::QueueFull),
            "third hot-group QUEUED item refused at the per-code bound"
        );
        // A different code still has room.
        core.enqueue(task(5), group(2)).unwrap();
        assert_eq!(core.queued_len(), 3);

        // Raising the bound through a policy switch admits one more queued
        // item of the hot group.
        let mut raised = wide_policy(64, 64, 8);
        raised.same_code_mode = SameCodeMode::Serial;
        raised.cross_code_mode = CrossCodeMode::Serial;
        core.set_policy(raised);
        core.enqueue(task(4), hot).unwrap();
        assert_eq!(core.queued_len(), 4);
        // And lowering it below the live backlog keeps everything queued.
        let mut lowered = wide_policy(64, 64, 1);
        lowered.same_code_mode = SameCodeMode::Serial;
        lowered.cross_code_mode = CrossCodeMode::Serial;
        core.set_policy(lowered);
        assert_eq!(core.queued_len(), 4, "no task dropped by the shrink");
        assert_eq!(
            core.enqueue(task(6), hot),
            Err(QueueError::QueueFull),
            "the shrunk bound holds for new enqueues"
        );
    }
}
