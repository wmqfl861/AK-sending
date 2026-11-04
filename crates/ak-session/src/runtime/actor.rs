//! The scheduler actor: one task serially owning
//! [`SchedulerCore`](crate::scheduler::SchedulerCore) (contract §5 /
//! B0 integration point 1).
//!
//! Everything that mutates scheduler state happens inside this loop:
//! external commands arrive on a bounded `mpsc`, executor/progress reports
//! and backoff timers are fed back through the same channel, so the core
//! has a single writer and needs no locks. Network I/O and file work live
//! outside the loop (driver tasks + the injected executors); the actor
//! only transitions state.
//!
//! Per command the actor pumps admission (inside the core) and then runs
//! [`SchedulerActor::after_mutation`]: broadcast drained core events on
//! the bounded event channel, spawn a task driver for every new admission
//! (suppressed once a shutdown began — "stop new admissions"), requeue
//! tasks parked by a queue-full retry, and check shutdown convergence.
use ak_core::error::ErrorCode;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::runtime::clock::RuntimeClock;
use crate::runtime::error::SessionError;
use crate::runtime::executor::{ExecutorReport, ProgressSink};
use crate::runtime::listener::ListenerStats;
use crate::runtime::transport::{PairingConfirmation, PairingTransport};
use crate::runtime::types::{
    CancelReceipt, PairingPlan, ParkReason, RetryPolicy, SessionEvent, SessionSnapshot,
    ShutdownMode, ShutdownSummary, SnapshotQuery, TaskView, TransferId, TransferSpec,
};
use crate::scheduler::config::SchedulerPolicy;
use crate::scheduler::core::{FinishResult, PolicySwitch, SchedulerCore};
use crate::scheduler::key::{TaskHandle, TaskId};
use crate::scheduler::queue::QueueError;
use crate::scheduler::state::{FinishOutcome, SchedulerEvent, TaskPhase, TaskState};

/// One command for the actor loop (external calls, executor reports and
/// internal timers all funnel through here).
///
/// Deliberately unboxed (same policy as the ak-signal `CtrlMessage`):
/// commands are short-lived, the channel is bounded at 256 and the big
/// `Submit` variant is dominated by the frozen 184-byte redeem payload.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Submit a transfer task.
    Submit {
        /// The task specification.
        spec: TransferSpec,
        /// Where the assigned transfer id (or error) is answered.
        reply: oneshot::Sender<Result<TransferId, SessionError>>,
    },
    /// Cancel a task (queued removal or running cancellation).
    Cancel {
        /// The task.
        task: TaskId,
        /// Receipt answer.
        reply: oneshot::Sender<Result<CancelReceipt, SessionError>>,
    },
    /// Switch the scheduling policy at runtime.
    SetPolicy {
        /// The new policy.
        policy: SchedulerPolicy,
        /// Switch result answer.
        reply: oneshot::Sender<Result<PolicySwitch, SessionError>>,
    },
    /// Manually restart a parked task.
    Retry {
        /// The task.
        task: TaskId,
        /// Answer.
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    /// Read a snapshot.
    Snapshot {
        /// Query filters.
        query: SnapshotQuery,
        /// Snapshot answer.
        reply: oneshot::Sender<SessionSnapshot>,
    },
    /// Begin shutdown (drain or cancel).
    Shutdown {
        /// Chosen mode.
        mode: ShutdownMode,
        /// Summary answer.
        reply: oneshot::Sender<Result<ShutdownSummary, SessionError>>,
    },
    /// A progress report from a task driver / executor.
    Report(ExecutorReport),
    /// A backoff timer fired for a task.
    BackoffElapsed {
        /// The task waiting out its backoff.
        task: TaskId,
    },
    /// The shutdown deadline fired.
    ShutdownDeadlineReached,
}

/// Runtime bookkeeping for one submitted task.
struct TaskEntry {
    spec: TransferSpec,
    attempts: u32,
    cancel_requested: bool,
    parked: Option<ParkReason>,
    last_error: Option<ErrorCode>,
}

/// In-progress shutdown.
struct ShutdownProgress {
    mode: ShutdownMode,
    reply: oneshot::Sender<Result<ShutdownSummary, SessionError>>,
    timed_out: bool,
    replied: bool,
}

/// The scheduler actor (see the module docs).
pub(crate) struct SchedulerActor {
    core: SchedulerCore,
    clock: Arc<dyn RuntimeClock>,
    transport: Arc<dyn PairingTransport>,
    cmd_tx: mpsc::Sender<Command>,
    events: broadcast::Sender<SessionEvent>,
    listener_stop: watch::Sender<bool>,
    listener_stats: Arc<ListenerStats>,
    listener_enabled: bool,
    entries: HashMap<TaskId, TaskEntry>,
    /// Tasks with a spawned driver that has not reported a finish yet.
    inflight: HashSet<TaskId>,
    /// Tasks holding their permit while waiting out a retry backoff.
    backoff_waiters: HashSet<TaskId>,
    shutdown: Option<ShutdownProgress>,
    finalized: bool,
    next_task_id: u64,
    retry: RetryPolicy,
    shutdown_timeout_ms: u64,
}

impl SchedulerActor {
    /// Assemble the actor around a configured core and its feedback
    /// channels.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        core: SchedulerCore,
        clock: Arc<dyn RuntimeClock>,
        transport: Arc<dyn PairingTransport>,
        cmd_tx: mpsc::Sender<Command>,
        events: broadcast::Sender<SessionEvent>,
        listener_stop: watch::Sender<bool>,
        listener_stats: Arc<ListenerStats>,
        listener_enabled: bool,
        retry: RetryPolicy,
        shutdown_timeout_ms: u64,
    ) -> Self {
        SchedulerActor {
            core,
            clock,
            transport,
            cmd_tx,
            events,
            listener_stop,
            listener_stats,
            listener_enabled,
            entries: HashMap::new(),
            inflight: HashSet::new(),
            backoff_waiters: HashSet::new(),
            shutdown: None,
            finalized: false,
            next_task_id: 0,
            retry,
            shutdown_timeout_ms,
        }
    }

    /// Run until a shutdown finalizes (or every handle is dropped).
    pub(crate) async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            self.handle(cmd);
            if self.finalized {
                break;
            }
        }
        let _ = self.listener_stop.send(true);
        self.flush_events();
        if let Some(mut progress) = self.shutdown.take() {
            if !progress.replied {
                progress.replied = true;
                let summary = self.build_summary(progress.mode, progress.timed_out);
                let _ = progress.reply.send(Ok(summary));
            }
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Submit { spec, reply } => self.on_submit(spec, reply),
            Command::Cancel { task, reply } => self.on_cancel(task, reply),
            Command::SetPolicy { policy, reply } => self.on_set_policy(policy, reply),
            Command::Retry { task, reply } => self.on_retry(task, reply),
            Command::Snapshot { query, reply } => {
                let snapshot = self.build_snapshot(&query);
                let _ = reply.send(snapshot);
            }
            Command::Shutdown { mode, reply } => self.on_shutdown(mode, reply),
            Command::Report(report) => self.on_report(report),
            Command::BackoffElapsed { task } => self.on_backoff_elapsed(task),
            Command::ShutdownDeadlineReached => self.on_shutdown_deadline(),
        }
    }

    // ---- Command handlers --------------------------------------------

    fn on_submit(
        &mut self,
        spec: TransferSpec,
        reply: oneshot::Sender<Result<TransferId, SessionError>>,
    ) {
        if self.shutdown.is_some() {
            let _ = reply.send(Err(SessionError::ShuttingDown));
            return;
        }
        let task_id = match spec.task_id {
            Some(id) => id,
            None => {
                // Auto ids skip past explicitly-provided ids so the two
                // namespaces never collide (real transfer ids are random
                // and persistent; the counter form serves the Fake wave).
                loop {
                    self.next_task_id += 1;
                    let candidate = TaskId::from_counter(self.next_task_id);
                    if !self.entries.contains_key(&candidate) {
                        break candidate;
                    }
                }
            }
        };
        let mut spec = spec;
        spec.label = spec.sanitized_label();
        let handle = TaskHandle::new(task_id);
        let group = spec.group_key;
        match self.core.enqueue(handle, group) {
            Err(QueueError::QueueFull) => {
                // Contract §7: refuse without scanning, without a
                // connection and without touching queued tasks.
                let _ = reply.send(Err(SessionError::QueueFull));
                return;
            }
            Err(QueueError::DuplicateTask) => {
                let _ = reply.send(Err(SessionError::DuplicateTask));
                return;
            }
            Ok(()) => {
                self.entries.insert(
                    task_id,
                    TaskEntry {
                        spec,
                        attempts: 0,
                        cancel_requested: false,
                        parked: None,
                        last_error: None,
                    },
                );
                let _ = reply.send(Ok(task_id));
            }
        }
        self.after_mutation();
    }

    fn on_cancel(
        &mut self,
        task: TaskId,
        reply: oneshot::Sender<Result<CancelReceipt, SessionError>>,
    ) {
        let answer = self.cancel_task(&task);
        let _ = reply.send(answer);
        self.after_mutation();
    }

    /// Cancel one task; contract §10 layering.
    fn cancel_task(&mut self, task: &TaskId) -> Result<CancelReceipt, SessionError> {
        if self.shutdown.is_some() {
            return Err(SessionError::ShuttingDown);
        }
        let handle = TaskHandle::new(*task);
        let Some(state) = self.core.task_state(&handle) else {
            return Err(SessionError::TaskNotFound);
        };
        if state.is_terminal() {
            return Err(SessionError::NotActive);
        }
        let Some(entry) = self.entries.get_mut(task) else {
            return Err(SessionError::TaskNotFound);
        };
        let holds_permit = self.core.permit_of(&handle).is_some();
        if holds_permit {
            // Running (or admitted / in backoff): request the stop, let
            // the executor's finish report finalize the terminal state.
            entry.cancel_requested = true;
            let _ = self.core.report_phase(&handle, TaskPhase::CancelRequested);
            if self.backoff_waiters.contains(task) {
                // No executor attempt is running; finalize immediately.
                self.backoff_waiters.remove(task);
                let _ = self.core.cancel(&handle);
            } else {
                entry.spec.executor.cancel(&handle);
            }
            Ok(CancelReceipt::RunningCancellationRequested)
        } else {
            // Queued, paused or parked: removed without ever connecting.
            let _ = self.core.cancel(&handle);
            Ok(CancelReceipt::RemovedQueued)
        }
    }

    fn on_set_policy(
        &mut self,
        policy: SchedulerPolicy,
        reply: oneshot::Sender<Result<PolicySwitch, SessionError>>,
    ) {
        if self.shutdown.is_some() {
            let _ = reply.send(Err(SessionError::ShuttingDown));
            return;
        }
        let switch = self.core.set_policy(policy);
        let _ = reply.send(Ok(switch));
        self.after_mutation();
    }

    fn on_retry(&mut self, task: TaskId, reply: oneshot::Sender<Result<(), SessionError>>) {
        if self.shutdown.is_some() {
            let _ = reply.send(Err(SessionError::ShuttingDown));
            return;
        }
        let handle = TaskHandle::new(task);
        let Some(entry) = self.entries.get_mut(&task) else {
            let _ = reply.send(Err(SessionError::TaskNotFound));
            return;
        };
        if entry.parked.is_none() {
            let _ = reply.send(Err(SessionError::NotActive));
            return;
        }
        entry.attempts = 0;
        entry.parked = None;
        entry.cancel_requested = false;
        entry.last_error = None;
        let result = match self.core.requeue(&handle) {
            Ok(true) => Ok(()),
            Ok(false) => Err(SessionError::NotActive),
            Err(QueueError::QueueFull) => Err(SessionError::QueueFull),
            Err(QueueError::DuplicateTask) => Err(SessionError::DuplicateTask),
        };
        let _ = reply.send(result);
        self.after_mutation();
    }

    fn on_shutdown(
        &mut self,
        mode: ShutdownMode,
        reply: oneshot::Sender<Result<ShutdownSummary, SessionError>>,
    ) {
        if self.shutdown.is_some() {
            let _ = reply.send(Err(SessionError::AlreadyShuttingDown));
            return;
        }
        self.shutdown = Some(ShutdownProgress {
            mode,
            reply,
            timed_out: false,
            replied: false,
        });
        self.broadcast(SessionEvent::ShutdownStarted { mode });
        // Stop the resident listener (stop + join from the handle).
        let _ = self.listener_stop.send(true);
        if mode == ShutdownMode::Cancel {
            // Stop everything we can right now; in-flight tasks finalize
            // through their executor's finish reports.
            let tasks: Vec<TaskId> = self.entries.keys().copied().collect();
            for task in tasks {
                let handle = TaskHandle::new(task);
                let Some(state) = self.core.task_state(&handle) else {
                    continue;
                };
                if state.is_terminal() {
                    continue;
                }
                if self.inflight.contains(&task) {
                    if let Some(entry) = self.entries.get_mut(&task) {
                        entry.cancel_requested = true;
                        let _ = self.core.report_phase(&handle, TaskPhase::CancelRequested);
                        entry.spec.executor.cancel(&handle);
                    }
                } else {
                    self.backoff_waiters.remove(&task);
                    let _ = self.core.cancel(&handle);
                }
            }
        }
        // Deadline watchdog on the injected clock.
        let deadline = self.clock.now_ms().saturating_add(self.shutdown_timeout_ms);
        let clock = Arc::clone(&self.clock);
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            clock.sleep_until(deadline).await;
            let _ = cmd_tx.send(Command::ShutdownDeadlineReached).await;
        });
        self.after_mutation();
    }

    fn on_shutdown_deadline(&mut self) {
        if let Some(progress) = self.shutdown.as_mut() {
            progress.timed_out = true;
        }
        self.finalize_shutdown();
    }

    fn on_report(&mut self, report: ExecutorReport) {
        match report {
            ExecutorReport::Phase { task, phase } => {
                let _ = self.core.report_phase(&task, phase);
            }
            ExecutorReport::Finished { task, outcome } => self.on_finished(&task, outcome),
        }
        self.after_mutation();
    }

    fn on_finished(&mut self, task: &TaskHandle, outcome: FinishOutcome) {
        let id = task.task_id();
        self.inflight.remove(&id);
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        match outcome {
            FinishOutcome::Completed => {
                // From `CancelRequested` this still wins (contract §9/§10:
                // the persisted final commit takes precedence).
                let _ = self.core.finish(task, FinishOutcome::Completed);
            }
            FinishOutcome::Failed { error } => {
                entry.last_error = Some(error);
                if entry.cancel_requested
                    || self.core.task_state(task) == Some(TaskState::CancelRequested)
                {
                    let _ = self.core.cancel(task);
                    return;
                }
                entry.attempts += 1;
                if self.shutdown.is_some() {
                    // No point backing off; record and let finalize clean
                    // up (the core requeue keeps it queryable meanwhile).
                    let _ = self.core.finish(task, FinishOutcome::Failed { error });
                    return;
                }
                if entry.attempts < self.retry.max_attempts {
                    // Backoff-first: hold the report until the timer
                    // fires, then finish-as-failed requeues for the next
                    // attempt (B0: backoff timing is the runtime's call).
                    let attempt = entry.attempts;
                    let deadline = self
                        .clock
                        .now_ms()
                        .saturating_add(self.retry.backoff_ms(attempt));
                    self.backoff_waiters.insert(id);
                    self.broadcast(SessionEvent::BackoffScheduled {
                        task: id,
                        attempt,
                        until_ms: deadline,
                    });
                    let clock = Arc::clone(&self.clock);
                    let cmd_tx = self.cmd_tx.clone();
                    tokio::spawn(async move {
                        clock.sleep_until(deadline).await;
                        let _ = cmd_tx.send(Command::BackoffElapsed { task: id }).await;
                    });
                } else {
                    // Retry budget exhausted: park the task (permit
                    // released, checkpoint kept) until a user retry.
                    let _ = self.core.pause(task);
                    entry.parked = Some(ParkReason::RetryExhausted);
                    self.broadcast(SessionEvent::TaskParked {
                        task: id,
                        reason: ParkReason::RetryExhausted,
                    });
                }
            }
        }
    }

    fn on_backoff_elapsed(&mut self, task: TaskId) {
        self.backoff_waiters.remove(&task);
        let handle = TaskHandle::new(task);
        let Some(state) = self.core.task_state(&handle) else {
            return;
        };
        if state.is_terminal() {
            return;
        }
        let Some(entry) = self.entries.get_mut(&task) else {
            return;
        };
        if entry.cancel_requested || self.shutdown.is_some() {
            let _ = self.core.cancel(&handle);
            self.after_mutation();
            return;
        }
        let error = entry.last_error.unwrap_or(ErrorCode::Internal);
        let attempt = entry.attempts + 1;
        let result = self.core.finish(&handle, FinishOutcome::Failed { error });
        let mut parked_event = None;
        if let Some(FinishResult::FailedParkedQueueFull) = result {
            entry.parked = Some(ParkReason::QueueFull);
            parked_event = Some(SessionEvent::TaskParked {
                task,
                reason: ParkReason::QueueFull,
            });
        }
        self.broadcast(SessionEvent::Retrying { task, attempt });
        if let Some(event) = parked_event {
            self.broadcast(event);
        }
        self.after_mutation();
    }

    // ---- Shared post-mutation pipeline --------------------------------

    /// Common tail of every mutating command: broadcast core events, spawn
    /// drivers for new admissions, requeue capacity-parked tasks, check
    /// shutdown convergence.
    fn after_mutation(&mut self) {
        self.flush_events();
        self.requeue_capacity_parked();
        self.check_shutdown_progress();
    }

    /// Drain core events: broadcast each, spawn a driver per admission
    /// (never once a shutdown began — that is the "stop new admissions"
    /// half of draining).
    fn flush_events(&mut self) {
        for event in self.core.take_events() {
            self.broadcast(SessionEvent::Scheduler(event.clone()));
            if let SchedulerEvent::TaskAdmitted { task, .. } = event {
                if self.shutdown.is_none() {
                    self.spawn_driver(task.task_id());
                }
            }
        }
    }

    /// Tasks parked because their retry requeue hit a full queue come back
    /// once capacity frees (B0: "call `requeue` once capacity frees").
    fn requeue_capacity_parked(&mut self) {
        if self.shutdown.is_some() {
            return;
        }
        loop {
            let candidate = self.entries.iter().find_map(|(id, entry)| {
                (entry.parked == Some(ParkReason::QueueFull)).then_some(*id)
            });
            let Some(task) = candidate else {
                return;
            };
            let handle = TaskHandle::new(task);
            match self.core.requeue(&handle) {
                Ok(true) => {
                    if let Some(entry) = self.entries.get_mut(&task) {
                        entry.parked = None;
                    }
                }
                // Still full (or no longer eligible) — retry on the next
                // capacity change instead of spinning here.
                _ => return,
            }
            self.flush_events();
        }
    }

    /// Shutdown converges when no driver is in flight (drain mode waits
    /// for natural finishes; cancel mode for the cancellation reports).
    fn check_shutdown_progress(&mut self) {
        if self.shutdown.is_none() || self.finalized || !self.inflight.is_empty() {
            return;
        }
        self.finalize_shutdown();
    }

    /// Cancel every non-terminal task, emit the final events and answer
    /// the shutdown caller.
    fn finalize_shutdown(&mut self) {
        if self.finalized {
            return;
        }
        let tasks: Vec<TaskId> = self.entries.keys().copied().collect();
        for task in tasks {
            let handle = TaskHandle::new(task);
            if let Some(state) = self.core.task_state(&handle) {
                if !state.is_terminal() {
                    let _ = self.core.cancel(&handle);
                }
            }
        }
        self.backoff_waiters.clear();
        self.flush_events();
        self.finalized = true;
        if let Some(mut progress) = self.shutdown.take() {
            if !progress.replied {
                progress.replied = true;
                let summary = self.build_summary(progress.mode, progress.timed_out);
                self.broadcast(SessionEvent::ShutdownComplete {
                    completed: summary.completed,
                    cancelled: summary.cancelled,
                    timed_out: summary.timed_out,
                });
                let _ = progress.reply.send(Ok(summary));
            }
        }
    }

    // ---- Task driver ---------------------------------------------------

    /// Drive one admitted task through pairing resolution and into its
    /// executor. Runs outside the actor loop; every observation flows back
    /// through the command channel.
    fn spawn_driver(&mut self, task: TaskId) {
        let Some(entry) = self.entries.get(&task) else {
            return;
        };
        let spec = entry.spec.clone();
        self.inflight.insert(task);
        let transport = Arc::clone(&self.transport);
        let sink = ProgressSink::new(self.cmd_tx.clone());
        let handle = TaskHandle::new(task);
        tokio::spawn(async move {
            let pairing_id = match spec.pairing.clone() {
                PairingPlan::Ready { pairing_id } => pairing_id,
                PairingPlan::Redeem { request } => {
                    // WaitingPairing → redeem → confirm (A3 transcript;
                    // wave 21 fills the crypto from the pairing module).
                    let accepted = match transport.redeem(request).await {
                        Ok(accepted) => accepted,
                        Err(err) => {
                            sink.finished(
                                handle,
                                FinishOutcome::Failed {
                                    error: err.error_code(),
                                },
                            )
                            .await;
                            return;
                        }
                    };
                    let confirmation = PairingConfirmation {
                        pairing_id: accepted.pairing_id,
                        transcript_digest: [0u8; 32],
                        proof: [0u8; 96],
                    };
                    match transport.confirm_pairing(confirmation).await {
                        Ok(ready) => ready.pairing_id,
                        Err(err) => {
                            sink.finished(
                                handle,
                                FinishOutcome::Failed {
                                    error: err.error_code(),
                                },
                            )
                            .await;
                            return;
                        }
                    }
                }
            };
            sink.phase(handle, TaskPhase::WaitingDirect).await;
            match transport
                .open_session(pairing_id, spec.group_key.code_id)
                .await
            {
                Ok(_) => {}
                Err(err) => {
                    sink.finished(
                        handle,
                        FinishOutcome::Failed {
                            error: err.error_code(),
                        },
                    )
                    .await;
                    return;
                }
            }
            sink.phase(handle, TaskPhase::AwaitingAdmission).await;
            // Wave 21: the real TaskOpen/TaskAdmission handshake (B2/B3)
            // happens here, then the engine on-channel executor runs.
            if let Err(err) = spec.executor.enqueue(handle, spec.clone(), sink.clone()) {
                sink.finished(handle, FinishOutcome::Failed { error: err.code })
                    .await;
            }
        });
    }

    // ---- Introspection -------------------------------------------------

    fn broadcast(&self, event: SessionEvent) {
        // Bounded channel: a slow consumer drops the oldest events and is
        // notified via `RecvError::Lagged` (contract §7); the actor never
        // blocks on observers.
        let _ = self.events.send(event);
    }

    fn build_summary(&self, mode: ShutdownMode, timed_out: bool) -> ShutdownSummary {
        let mut completed = 0;
        let mut cancelled = 0;
        let mut parked = 0;
        for task in self.entries.keys() {
            match self.core.task_state(&TaskHandle::new(*task)) {
                Some(TaskState::Completed) => completed += 1,
                Some(TaskState::Cancelled) => cancelled += 1,
                Some(TaskState::Paused) | Some(TaskState::Queued) => parked += 1,
                _ => {}
            }
        }
        ShutdownSummary {
            mode,
            completed,
            cancelled,
            parked,
            timed_out,
            listener_stopped: true,
        }
    }

    fn build_snapshot(&self, query: &SnapshotQuery) -> SessionSnapshot {
        let mut tasks: Vec<TaskView> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                let handle = TaskHandle::new(*id);
                let state = self.core.task_state(&handle)?;
                if !query.include_terminal && state.is_terminal() {
                    return None;
                }
                if let Some(wanted) = query.task {
                    if wanted != *id {
                        return None;
                    }
                }
                Some(TaskView {
                    task: *id,
                    group: entry.spec.group_key,
                    state,
                    direction: entry.spec.direction,
                    label: entry.spec.label.clone(),
                    attempts: entry.attempts,
                    last_error: self.core.last_error(&handle).or(entry.last_error),
                    cancel_requested: entry.cancel_requested,
                    parked: entry.parked,
                })
            })
            .collect();
        tasks.sort_by_key(|view| view.task);
        SessionSnapshot {
            policy: self.core.policy(),
            policy_revision: self.core.policy_revision(),
            draining: self.core.is_draining(),
            active_tasks: self.core.active_total(),
            queued_tasks: self.core.queued_len(),
            live_permits: self.core.live_permits(),
            shutting_down: self.shutdown.is_some(),
            tasks,
            listener: self.listener_stats.view(self.listener_enabled),
        }
    }
}
