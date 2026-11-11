//! The scheduler actor: one task serially owning
//! [`SchedulerCore`] (contract §5 /
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
//! `SchedulerActor::after_mutation`: broadcast drained core events on
//! the bounded event channel, spawn a task driver for every new admission
//! (suppressed once a shutdown began — "stop new admissions"), requeue
//! tasks parked by a queue-full retry, and check shutdown convergence.
use ak_core::error::ErrorCode;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::runtime::clock::RuntimeClock;
use crate::runtime::error::SessionError;
use crate::runtime::executor::{ExecutorReport, ExecutorWork, ProgressSink};
use crate::runtime::listener::ListenerStats;
use crate::runtime::transport::{PairingConfirmation, PairingTransport};
use crate::runtime::types::{
    CancelReceipt, PairingPlan, ParkReason, RetryPolicy, SessionEvent, SessionSnapshot,
    ShutdownMode, ShutdownSummary, SnapshotQuery, TaskView, TransferDirection, TransferId,
    TransferSpec, MAX_TERMINAL_HISTORY,
};
use crate::scheduler::config::SchedulerPolicy;
use crate::scheduler::core::{CancelOutcome, FinishResult, PolicySwitch, SchedulerCore};
use crate::scheduler::key::{CodeGroupKey, TaskHandle, TaskId};
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

/// Runtime bookkeeping for one submitted task (P2-06).
///
/// A terminal task's working spec — executor handle included — is
/// released immediately; only a small bounded record stays for
/// snapshot/history recall. Non-terminal tasks (queued, active, backed
/// off, parked, retryable) always keep their full working spec: drivers,
/// retries and executor cancellation need it.
enum TaskEntry {
    /// Working state: the task may still run. The spec is boxed: the
    /// variant must stay comparable in size to the slim `Finished`
    /// record (clippy::large_enum_variant), and one heap allocation per
    /// live task is nothing against the executor work it describes.
    Active {
        /// The submitted spec (drives retries and carries the executor).
        spec: Box<TransferSpec>,
        attempts: u32,
        cancel_requested: bool,
        parked: Option<ParkReason>,
        last_error: Option<ErrorCode>,
    },
    /// Bounded terminal record; see [`MAX_TERMINAL_HISTORY`].
    Finished(FinishedTask),
}

/// The slim record kept for a terminal task (P2-06): identity and display
/// fields for snapshot recall, the last stable error, and when it ended.
/// No executor, no engine spec, no progress state.
struct FinishedTask {
    group: CodeGroupKey,
    direction: TransferDirection,
    label: String,
    attempts: u32,
    last_error: Option<ErrorCode>,
    cancelled: bool,
    finished_at_ms: u64,
}

/// In-progress shutdown.
struct ShutdownProgress {
    mode: ShutdownMode,
    reply: oneshot::Sender<Result<ShutdownSummary, SessionError>>,
    timed_out: bool,
    replied: bool,
}

/// How long the actor waits for one aborted driver task to join before
/// giving up on it (abort takes effect at the next `await` point, so this
/// is a generous safety bound, not the expected duration).
const DRIVER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Graceful bound for joining one executor's engine work during shutdown
/// (P1-03). 12 s deliberately exceeds ak-engine's 10 s FinalAck
/// tail-hold (a finished receive holds its channel up to 10 s so the
/// sender can observe the final ack): a task whose completion is already
/// persisted is allowed to finish that hold rather than be aborted
/// mid-commit. Only after this may the shutdown summary go out.
const ENGINE_WORK_GRACE: Duration = Duration::from_secs(12);
/// Hard bound for the abort escalation after [`ENGINE_WORK_GRACE`]
/// expires (an aborted task unwinds at its next `await` point).
const ENGINE_WORK_HARD: Duration = Duration::from_secs(2);

/// Build a real actor command channel for in-crate tests (the same
/// bounded kind the runtime owns), so executors can be driven through a
/// genuine [`ProgressSink`](crate::runtime::ProgressSink).
#[cfg(test)]
pub(crate) fn test_command_channel() -> (mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    mpsc::channel(64)
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
    /// Termination order of the bounded terminal history (P2-06), for
    /// oldest-first eviction beyond [`MAX_TERMINAL_HISTORY`].
    finished_order: VecDeque<TaskId>,
    /// Cumulative terminal transitions (P2-06): terminal entries may be
    /// trimmed from `entries`, so shutdown summaries count transitions
    /// instead of rescanning history.
    completed_total: u64,
    cancelled_total: u64,
    /// Tasks with a spawned driver that has not reported a finish yet.
    inflight: HashSet<TaskId>,
    /// Join handles of the spawned task drivers. Kept so shutdown can
    /// abort and join every driver before the shutdown caller is answered
    /// (P1-3: previously the handle was dropped on spawn and a driver
    /// stuck in pairing/session I/O outlived the runtime).
    drivers: HashMap<TaskId, JoinHandle<()>>,
    /// Engine work handles taken from executors during finalize (P1-03):
    /// joined in [`SchedulerActor::run`] before the shutdown reply, so no
    /// summary claims the runtime stopped while a real engine still held
    /// its port or wrote files.
    pending_engine_work: Vec<ExecutorWork>,
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
            finished_order: VecDeque::new(),
            completed_total: 0,
            cancelled_total: 0,
            inflight: HashSet::new(),
            drivers: HashMap::new(),
            pending_engine_work: Vec::new(),
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
        // P1-03: join the still-running real engine work BEFORE the
        // shutdown caller is answered. `finalize_shutdown` already asked
        // every executor to stop; a cancelled engine reports its outcome
        // on its way out (strictly after dropping the kernel), so the
        // join below proves the ports are released and the writes stopped
        // before any summary can claim the runtime is down.
        self.join_engine_work().await;
        // P1-3: abort and join every remaining driver BEFORE the shutdown
        // caller is answered, so a summary is a statement about a runtime
        // whose engine work has actually stopped.
        self.abort_and_join_drivers().await;
        // Reports that arrived while joining (a cancelled engine task
        // sends its terminal report right before exiting) are processed
        // here — with the persisted outcome winning over the requested
        // cancellation, exactly as in the live loop.
        while let Ok(cmd) = rx.try_recv() {
            self.handle(cmd);
        }
        // Defensive close-out (P1-03): anything whose engine died without
        // delivering its report still goes terminal here, so no permit or
        // slot survives the runtime.
        self.terminalize_unreported();
        self.flush_events();
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
                    TaskEntry::Active {
                        spec: Box::new(spec),
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
        if !matches!(self.entries.get(task), Some(TaskEntry::Active { .. })) {
            return Err(SessionError::TaskNotFound);
        }
        let holds_permit = self.core.permit_of(&handle).is_some();
        if holds_permit {
            // Running (or admitted / in backoff): request the stop, let
            // the executor's finish report finalize the terminal state.
            if self.backoff_waiters.contains(task) {
                // No executor attempt is running; finalize immediately.
                if let Some(TaskEntry::Active {
                    cancel_requested, ..
                }) = self.entries.get_mut(task)
                {
                    *cancel_requested = true;
                }
                let _ = self.core.report_phase(&handle, TaskPhase::CancelRequested);
                self.backoff_waiters.remove(task);
                self.cancel_terminal(&handle);
            } else {
                self.request_running_cancel(task);
            }
            Ok(CancelReceipt::RunningCancellationRequested)
        } else {
            // Queued, paused or parked: removed without ever connecting.
            self.cancel_terminal(&handle);
            Ok(CancelReceipt::RemovedQueued)
        }
    }

    /// Hand a running (permit-holding) task's stop to its executor
    /// (P1-03): flag the intent, publish `CancelRequested`, then let the
    /// executor wind the real engine down — its finish report, sent
    /// strictly after the engine work exited, finalizes the terminal
    /// state (a persisted completion wins over the request).
    fn request_running_cancel(&mut self, task: &TaskId) {
        let handle = TaskHandle::new(*task);
        let executor = match self.entries.get_mut(task) {
            Some(TaskEntry::Active {
                cancel_requested,
                spec,
                ..
            }) => {
                *cancel_requested = true;
                let _ = self.core.report_phase(&handle, TaskPhase::CancelRequested);
                Arc::clone(&spec.executor)
            }
            _ => return,
        };
        executor.cancel(&handle);
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
        let TaskEntry::Active {
            attempts,
            parked,
            cancel_requested,
            last_error,
            ..
        } = entry
        else {
            // Terminal record (P2-06): nothing to restart. Anything
            // already evicted from the bounded history answered
            // `TaskNotFound` above.
            let _ = reply.send(Err(SessionError::NotActive));
            return;
        };
        if parked.is_none() {
            let _ = reply.send(Err(SessionError::NotActive));
            return;
        }
        *attempts = 0;
        *parked = None;
        *cancel_requested = false;
        *last_error = None;
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
            // through their executor's finish reports (sent strictly after
            // the real engine work exited — P1-03).
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
                    self.request_running_cancel(&task);
                } else {
                    self.backoff_waiters.remove(&task);
                    self.cancel_terminal(&handle);
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
        // The driver ends right after handing the task to its executor;
        // its join handle is no longer interesting (aborting it later
        // would be a no-op).
        self.drivers.remove(&id);
        if matches!(self.entries.get(&id), Some(TaskEntry::Finished(_))) {
            // Terminal (or duplicate) report: the first persisted outcome
            // wins (P1-03 — never rewrite a terminal state).
            return;
        }
        match outcome {
            FinishOutcome::Completed => {
                // From `CancelRequested` this still wins (contract §9/§10:
                // the persisted final commit takes precedence).
                self.complete_terminal(task);
            }
            FinishOutcome::Failed { error } => {
                let cancel_requested = {
                    let Some(TaskEntry::Active {
                        cancel_requested,
                        last_error,
                        ..
                    }) = self.entries.get_mut(&id)
                    else {
                        return;
                    };
                    *last_error = Some(error);
                    *cancel_requested
                } || self.core.task_state(task)
                    == Some(TaskState::CancelRequested);
                if cancel_requested {
                    self.cancel_terminal(task);
                    return;
                }
                let attempts = {
                    let Some(TaskEntry::Active { attempts, .. }) = self.entries.get_mut(&id) else {
                        return;
                    };
                    *attempts += 1;
                    *attempts
                };
                if self.shutdown.is_some() {
                    // No point backing off; record and let finalize clean
                    // up (the core requeue keeps it queryable meanwhile).
                    let _ = self.core.finish(task, FinishOutcome::Failed { error });
                    return;
                }
                if attempts < self.retry.max_attempts {
                    // Backoff-first: hold the report until the timer
                    // fires, then finish-as-failed requeues for the next
                    // attempt (B0: backoff timing is the runtime's call).
                    let deadline = self
                        .clock
                        .now_ms()
                        .saturating_add(self.retry.backoff_ms(attempts));
                    self.backoff_waiters.insert(id);
                    self.broadcast(SessionEvent::BackoffScheduled {
                        task: id,
                        attempt: attempts,
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
                    if let Some(TaskEntry::Active { parked, .. }) = self.entries.get_mut(&id) {
                        *parked = Some(ParkReason::RetryExhausted);
                    }
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
        let Some(TaskEntry::Active {
            attempts,
            cancel_requested,
            last_error,
            ..
        }) = self.entries.get_mut(&task)
        else {
            return;
        };
        let cancel_requested = *cancel_requested;
        let error = (*last_error).unwrap_or(ErrorCode::Internal);
        let attempt = *attempts + 1;
        if cancel_requested || self.shutdown.is_some() {
            self.cancel_terminal(&handle);
            self.after_mutation();
            return;
        }
        let result = self.core.finish(&handle, FinishOutcome::Failed { error });
        let mut parked_event = None;
        if let Some(FinishResult::FailedParkedQueueFull) = result {
            if let Some(TaskEntry::Active { parked, .. }) = self.entries.get_mut(&task) {
                *parked = Some(ParkReason::QueueFull);
            }
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
            let candidate = self.entries.iter().find_map(|(id, entry)| match entry {
                TaskEntry::Active {
                    parked: Some(ParkReason::QueueFull),
                    ..
                } => Some(*id),
                _ => None,
            });
            let Some(task) = candidate else {
                return;
            };
            let handle = TaskHandle::new(task);
            match self.core.requeue(&handle) {
                Ok(true) => {
                    if let Some(TaskEntry::Active { parked, .. }) = self.entries.get_mut(&task) {
                        *parked = None;
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

    /// Stop every non-terminal task and mark the runtime finalized
    /// (P1-03).
    ///
    /// Tasks without running engine work (queued, parked, backed off) go
    /// terminal right here. Running tasks are stopped through their
    /// executor and their joinable work handles are collected; the actual
    /// terminal transition — and with it the permit release — happens
    /// only after that work joined and its persisted outcome was
    /// processed in [`SchedulerActor::run`]. The shutdown reply is never
    /// sent while a real engine may still hold its port or write files.
    fn finalize_shutdown(&mut self) {
        if self.finalized {
            return;
        }
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
                self.request_running_cancel(&task);
                // Take the joinable work handle: `run` joins it before
                // the shutdown reply (P1-03).
                if let Some(TaskEntry::Active { spec, .. }) = self.entries.get(&task) {
                    if let Some(work) = spec.executor.take_work(&handle) {
                        self.pending_engine_work.push(work);
                    }
                }
            } else {
                self.backoff_waiters.remove(&task);
                self.cancel_terminal(&handle);
            }
        }
        self.backoff_waiters.clear();
        self.flush_events();
        self.finalized = true;
    }

    /// Join every executor work handle taken during finalize (P1-03),
    /// concurrently and bounded: the graceful bound deliberately exceeds
    /// the engine's 10 s FinalAck tail-hold so a receive whose completion
    /// is already persisted can deliver its final ack instead of being
    /// aborted mid-commit. `ShutdownComplete` is only sent after this.
    async fn join_engine_work(&mut self) {
        let joins: Vec<JoinHandle<()>> = std::mem::take(&mut self.pending_engine_work)
            .into_iter()
            .map(|work| {
                tokio::spawn(async move {
                    work.join_within(ENGINE_WORK_GRACE, ENGINE_WORK_HARD).await;
                })
            })
            .collect();
        let overall = ENGINE_WORK_GRACE + ENGINE_WORK_HARD + DRIVER_JOIN_TIMEOUT;
        for mut join in joins {
            let _ = tokio::time::timeout(overall, &mut join).await;
        }
    }

    /// Defensive close-out (P1-03): after the engine joins and the report
    /// drain, force any task that never delivered its terminal report
    /// into `Cancelled`, so no permit or slot survives the runtime.
    fn terminalize_unreported(&mut self) {
        let tasks: Vec<TaskId> = self.entries.keys().copied().collect();
        for task in tasks {
            let handle = TaskHandle::new(task);
            if let Some(state) = self.core.task_state(&handle) {
                if !state.is_terminal() {
                    self.cancel_terminal(&handle);
                }
            }
        }
        self.backoff_waiters.clear();
        self.flush_events();
    }

    /// Abort every still-tracked driver task and join it (bounded per
    /// task). Called once from [`SchedulerActor::run`] after the loop
    /// exits — a finished driver joins immediately; a stuck one is
    /// cancelled at its current `await` point, which also drops any
    /// in-flight pairing/session future it holds.
    async fn abort_and_join_drivers(&mut self) {
        let drivers: Vec<JoinHandle<()>> = self.drivers.drain().map(|(_, driver)| driver).collect();
        for mut driver in drivers {
            driver.abort();
            let _ = tokio::time::timeout(DRIVER_JOIN_TIMEOUT, &mut driver).await;
        }
    }

    // ---- Terminal transitions (P2-06) ----------------------------------

    /// Record terminal success through the core (a final commit that won
    /// the race against a requested cancellation still wins, contract
    /// §9/§10) and keep the bounded terminal record.
    fn complete_terminal(&mut self, handle: &TaskHandle) {
        if matches!(
            self.core.finish(handle, FinishOutcome::Completed),
            Some(FinishResult::Completed)
        ) {
            self.completed_total += 1;
            self.retain_finished(handle.task_id(), false);
        }
    }

    /// Cancel through the core; when the task actually terminal-cancels,
    /// keep the bounded terminal record and bump the cumulative counter
    /// so shutdown summaries stay faithful although terminal entries get
    /// trimmed (P2-06).
    fn cancel_terminal(&mut self, handle: &TaskHandle) -> CancelOutcome {
        let outcome = self.core.cancel(handle);
        if !matches!(outcome, CancelOutcome::NotActive) {
            self.cancelled_total += 1;
            self.retain_finished(handle.task_id(), true);
        }
        outcome
    }

    /// Convert an active entry into its bounded slim terminal record and
    /// evict the oldest history beyond [`MAX_TERMINAL_HISTORY`] (P2-06).
    /// The working spec — and with it the executor handle — is released
    /// here; already-finished entries are left as they are.
    fn retain_finished(&mut self, task: TaskId, cancelled: bool) {
        let Some(entry) = self.entries.get_mut(&task) else {
            return;
        };
        let TaskEntry::Active {
            spec,
            attempts,
            last_error,
            ..
        } = entry
        else {
            return;
        };
        let finished = FinishedTask {
            group: spec.group_key,
            direction: spec.direction,
            label: std::mem::take(&mut spec.label),
            attempts: *attempts,
            last_error: *last_error,
            cancelled,
            finished_at_ms: self.clock.now_ms(),
        };
        *entry = TaskEntry::Finished(finished);
        self.finished_order.push_back(task);
        while self.finished_order.len() > MAX_TERMINAL_HISTORY {
            let evict = self
                .finished_order
                .pop_front()
                .expect("history nonempty while over the cap");
            self.entries.remove(&evict);
        }
    }

    // ---- Task driver ---------------------------------------------------

    /// Drive one admitted task through pairing resolution and into its
    /// executor. Runs outside the actor loop; every observation flows back
    /// through the command channel.
    fn spawn_driver(&mut self, task: TaskId) {
        let Some(TaskEntry::Active { spec, .. }) = self.entries.get(&task) else {
            return;
        };
        let spec = (**spec).clone();
        self.inflight.insert(task);
        let transport = Arc::clone(&self.transport);
        let sink = ProgressSink::new(self.cmd_tx.clone());
        let handle = TaskHandle::new(task);
        let driver = tokio::spawn(async move {
            // Pairing resolution: `Local` skips the signaling plane
            // entirely (a resident bind/accept listener has no session to
            // open toward a still-unknown peer); `Ready` has its relation;
            // `Redeem` finishes the A3 transcript first.
            let resolved = match spec.pairing.clone() {
                PairingPlan::Local => None,
                PairingPlan::Ready { pairing_id } => Some(pairing_id),
                PairingPlan::Redeem { request } => {
                    // WaitingPairing → redeem → confirm (A3 transcript).
                    // Wave 21 delivered the real transcript crypto in
                    // `crate::pairing` (PairingOrchestrator): it owns the
                    // invite secret, journal and trust book, so flows that
                    // must reach `PairReady` are orchestrated there and
                    // submitted as `PairingPlan::Ready`. The transport-level
                    // confirm below still drives the frames the trait
                    // models (real adapter: proof + prepared + commit).
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
                        Ok(ready) => Some(ready.pairing_id),
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
            if let Some(pairing_id) = resolved {
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
            }
            sink.phase(handle, TaskPhase::AwaitingAdmission).await;
            // Wave 21 wiring status: the real `PairingTransport` adapter
            // (`crate::pairing::RealTransport`) plugs into this runtime via
            // `RuntimeConfig::transport`, so everything before this point
            // (redeem/confirm/events) runs against the real registry. The
            // TaskOpen/TaskAdmission handshake (B2/B3) and the ak-engine
            // on-channel executor binding remain wave-22 work; the injected
            // executor still receives admitted tasks here unchanged.
            if let Err(err) = spec.executor.enqueue(handle, spec.clone(), sink.clone()) {
                sink.finished(handle, FinishOutcome::Failed { error: err.code })
                    .await;
            }
        });
        self.drivers.insert(task, driver);
    }

    // ---- Introspection -------------------------------------------------

    fn broadcast(&self, event: SessionEvent) {
        // Bounded channel: a slow consumer drops the oldest events and is
        // notified via `RecvError::Lagged` (contract §7); the actor never
        // blocks on observers.
        let _ = self.events.send(event);
    }

    fn build_summary(&self, mode: ShutdownMode, timed_out: bool) -> ShutdownSummary {
        // Terminal tasks may already be evicted from the bounded history
        // (P2-06), so the summary counts cumulative terminal transitions
        // instead of rescanning entries; parked/queued leftovers are by
        // definition non-terminal and therefore still resident.
        let mut parked = 0;
        for id in self.entries.keys() {
            match self.core.task_state(&TaskHandle::new(*id)) {
                Some(TaskState::Paused) | Some(TaskState::Queued) => parked += 1,
                _ => {}
            }
        }
        ShutdownSummary {
            mode,
            completed: self.completed_total,
            cancelled: self.cancelled_total,
            parked,
            timed_out,
            listener_stopped: true,
        }
    }

    fn build_snapshot(&self, query: &SnapshotQuery) -> SessionSnapshot {
        // P2-06: the scan is bounded by live tasks plus the capped
        // terminal history — a long-running runtime no longer walks every
        // task it ever ran.
        let mut tasks: Vec<TaskView> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                let view = match entry {
                    TaskEntry::Active {
                        spec,
                        attempts,
                        cancel_requested,
                        parked,
                        last_error,
                    } => {
                        let handle = TaskHandle::new(*id);
                        let state = self.core.task_state(&handle)?;
                        TaskView {
                            task: *id,
                            group: spec.group_key,
                            state,
                            direction: spec.direction,
                            label: spec.label.clone(),
                            attempts: *attempts,
                            last_error: self.core.last_error(&handle).or(*last_error),
                            cancel_requested: *cancel_requested,
                            parked: *parked,
                            finished_at_ms: None,
                        }
                    }
                    TaskEntry::Finished(finished) => TaskView {
                        task: *id,
                        group: finished.group,
                        state: if finished.cancelled {
                            TaskState::Cancelled
                        } else {
                            TaskState::Completed
                        },
                        direction: finished.direction,
                        label: finished.label.clone(),
                        attempts: finished.attempts,
                        last_error: finished.last_error,
                        cancel_requested: false,
                        parked: None,
                        finished_at_ms: Some(finished.finished_at_ms),
                    },
                };
                if !query.include_terminal && view.state.is_terminal() {
                    return None;
                }
                if let Some(wanted) = query.task {
                    if wanted != *id {
                        return None;
                    }
                }
                Some(view)
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

#[cfg(test)]
mod tests {
    //! P1-3 shutdown coverage that lives with the actor: a shutdown whose
    //! deadline expires while a driver is wedged in pairing I/O must
    //! abort and JOIN that driver before the shutdown summary is
    //! answered. The wedge is observed through a transport whose
    //! `open_session` future never resolves and flips a flag when it is
    //! dropped — exactly what aborting the parked driver does.

    use super::*;
    use crate::runtime::clock::RealClock;
    use crate::runtime::executor::FakeTransferExecutor;
    use crate::runtime::transport::{
        InviteClaimRequest, InviteClaimed, OpenSessionInfo, PairingError, PairingEvent,
        PairingFuture, PairingId, PairingReadyInfo, RedeemAccepted, RedeemPayload,
    };
    use crate::runtime::types::{PairingPlan, RuntimeConfig, ShutdownMode, TransferDirection};
    use crate::runtime::{
        RegistryId, RuntimeClock, SessionHandle, SessionRuntime, TransferExecutor,
    };
    use crate::scheduler::key::CodeGroupKey;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    /// `open_session` future that never resolves; sets `dropped` when the
    /// driver awaiting it is aborted.
    struct HungOpenSession {
        dropped: Arc<AtomicBool>,
    }

    impl Future for HungOpenSession {
        type Output = Result<OpenSessionInfo, PairingError>;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for HungOpenSession {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct HungTransport {
        opens: AtomicUsize,
        dropped: Arc<AtomicBool>,
    }

    impl PairingTransport for HungTransport {
        fn claim_invite(&self, _claim: InviteClaimRequest) -> PairingFuture<'_, InviteClaimed> {
            Box::pin(async { Err(PairingError::rejected("unused")) })
        }

        fn redeem(&self, _request: RedeemPayload) -> PairingFuture<'_, RedeemAccepted> {
            Box::pin(async { Err(PairingError::rejected("unused")) })
        }

        fn confirm_pairing(
            &self,
            _confirmation: PairingConfirmation,
        ) -> PairingFuture<'_, PairingReadyInfo> {
            Box::pin(async { Err(PairingError::rejected("unused")) })
        }

        fn open_session(
            &self,
            _pairing_id: PairingId,
            _code_id: crate::scheduler::key::CodeId,
        ) -> PairingFuture<'_, OpenSessionInfo> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Box::pin(HungOpenSession {
                dropped: Arc::clone(&self.dropped),
            })
        }

        fn next_pairing_event(&self) -> PairingFuture<'_, PairingEvent> {
            Box::pin(std::future::pending())
        }

        fn is_connected(&self) -> bool {
            true
        }
    }

    fn wedged_runtime() -> (SessionHandle, Arc<HungTransport>) {
        let transport = Arc::new(HungTransport::default());
        let clock = RealClock::shared();
        let executor = Arc::new(FakeTransferExecutor::new(Arc::clone(&clock)));
        let mut config = RuntimeConfig::new(
            SchedulerPolicy::default(),
            clock,
            Arc::clone(&transport) as Arc<dyn PairingTransport>,
            executor as Arc<dyn TransferExecutor>,
            RegistryId::from_bytes([0x11; 32]),
        );
        config.listener_enabled = false;
        config.shutdown_timeout_ms = 150;
        let handle = SessionRuntime::start(config).expect("runtime starts");
        (handle, transport)
    }

    /// The shutdown timeout aborts (and joins) the wedged driver BEFORE
    /// the summary reply is sent: at the moment the caller is answered,
    /// the hung `open_session` future is already dropped.
    #[tokio::test]
    async fn shutdown_timeout_aborts_and_joins_stuck_drivers_before_answering() {
        let (handle, transport) = wedged_runtime();
        let group = CodeGroupKey::new(
            RegistryId::from_bytes([0x11; 32]),
            crate::scheduler::key::CodeId::from_bytes([0x22; 16]),
        );
        let executor = Arc::new(FakeTransferExecutor::new(RealClock::shared()));
        let spec = TransferSpec {
            group_key: group,
            pairing: PairingPlan::Ready {
                pairing_id: PairingId::from_bytes([0x33; 16]),
            },
            executor: executor as Arc<dyn TransferExecutor>,
            direction: TransferDirection::Send,
            label: "wedged".to_owned(),
            task_id: None,
        };
        handle.enqueue(spec).await.expect("task submitted");

        // The driver reaches (and wedges in) open_session.
        let deadline = Duration::from_secs(5);
        tokio::time::timeout(deadline, async {
            while transport.opens.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("driver reached open_session");
        assert!(!transport.dropped.load(Ordering::SeqCst));

        let summary = tokio::time::timeout(deadline, handle.shutdown(ShutdownMode::Cancel))
            .await
            .expect("shutdown answers within the deadline")
            .expect("shutdown succeeds");
        assert!(summary.timed_out, "wedged driver forces the timeout path");
        // The join happened before the reply: the hung future is gone.
        assert!(
            transport.dropped.load(Ordering::SeqCst),
            "the wedged driver was aborted and joined before the summary"
        );
    }

    /// Drain-mode shutdown after natural completion still answers with a
    /// summary (the new reply path in `run`).
    #[tokio::test]
    async fn drain_shutdown_after_completion_answers_through_the_new_reply_path() {
        let clock = RealClock::shared();
        let executor = Arc::new(FakeTransferExecutor::new(Arc::clone(&clock)));
        let mut config = RuntimeConfig::new(
            SchedulerPolicy::default(),
            clock,
            Arc::new(crate::runtime::transport::FakePairingTransport::new(true))
                as Arc<dyn PairingTransport>,
            Arc::clone(&executor) as Arc<dyn TransferExecutor>,
            RegistryId::from_bytes([0x11; 32]),
        );
        config.listener_enabled = false;
        config.shutdown_timeout_ms = 5_000;
        let handle = SessionRuntime::start(config).expect("runtime starts");

        let group = CodeGroupKey::new(
            RegistryId::from_bytes([0x11; 32]),
            crate::scheduler::key::CodeId::from_bytes([0x22; 16]),
        );
        let spec = TransferSpec {
            group_key: group,
            pairing: PairingPlan::Ready {
                pairing_id: PairingId::from_bytes([0x44; 16]),
            },
            executor: executor as Arc<dyn TransferExecutor>,
            direction: TransferDirection::Send,
            label: "quick".to_owned(),
            task_id: None,
        };
        handle.enqueue(spec).await.expect("task submitted");
        // Default fake script completes in ~100 ms of real time.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let summary =
            tokio::time::timeout(Duration::from_secs(5), handle.shutdown(ShutdownMode::Drain))
                .await
                .expect("shutdown answers")
                .expect("shutdown succeeds");
        assert!(!summary.timed_out);
        assert_eq!(summary.completed, 1);
    }

    /// P2-06: a terminal task releases its working spec — and with it the
    /// executor handle. A Drop-counting executor proves the runtime lets
    /// go once the task is terminal (only the slim bounded record stays).
    struct DroppingExecutor {
        dropped: Arc<AtomicBool>,
    }

    impl TransferExecutor for DroppingExecutor {
        fn enqueue(
            &self,
            task: TaskHandle,
            _spec: TransferSpec,
            sink: crate::runtime::executor::ProgressSink,
        ) -> Result<(), crate::runtime::error::EngineStartError> {
            tokio::spawn(async move {
                sink.phase(task, TaskPhase::Scanning).await;
                sink.phase(task, TaskPhase::Transferring).await;
                sink.phase(task, TaskPhase::Verifying).await;
                sink.phase(task, TaskPhase::Committing).await;
                sink.finished(task, FinishOutcome::Completed).await;
            });
            Ok(())
        }

        fn cancel(&self, _task: &TaskHandle) {}
    }

    impl Drop for DroppingExecutor {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn terminal_task_releases_its_executor() {
        let clock = RealClock::shared();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut config = RuntimeConfig::new(
            SchedulerPolicy::default(),
            Arc::clone(&clock) as Arc<dyn RuntimeClock>,
            Arc::new(crate::runtime::transport::FakePairingTransport::new(true))
                as Arc<dyn PairingTransport>,
            // The default executor is unrelated (listener off); only the
            // task spec below owns the DroppingExecutor.
            Arc::new(FakeTransferExecutor::new(Arc::clone(&clock))) as Arc<dyn TransferExecutor>,
            RegistryId::from_bytes([0x11; 32]),
        );
        config.listener_enabled = false;
        let handle = SessionRuntime::start(config).expect("runtime starts");

        let group = CodeGroupKey::new(
            RegistryId::from_bytes([0x11; 32]),
            crate::scheduler::key::CodeId::from_bytes([0x55; 16]),
        );
        let spec = TransferSpec {
            group_key: group,
            pairing: PairingPlan::Local,
            executor: Arc::new(DroppingExecutor {
                dropped: Arc::clone(&dropped),
            }) as Arc<dyn TransferExecutor>,
            direction: TransferDirection::Send,
            label: "drop-me".to_owned(),
            task_id: None,
        };
        let task = handle.enqueue(spec).await.expect("task submitted");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snap = handle
                .snapshot(crate::runtime::SnapshotQuery::one(task))
                .await
                .expect("snapshot");
            if snap.tasks.iter().any(|view| view.state.is_terminal()) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "task never completed");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The runtime and every driver have let go of the executor: the
        // only Arc was the spec's, released with the terminal transition.
        assert!(
            dropped.load(Ordering::SeqCst),
            "the terminal task's executor was released"
        );
        let _ = handle.shutdown(ShutdownMode::Drain).await;
    }
}
