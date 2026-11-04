//! Injected engine dependency: [`TransferExecutor`].
//!
//! The runtime admits tasks, then hands each one to an executor: "enqueue
//! one task and report phase/finish". Reports flow back through a
//! [`ProgressSink`], which serializes them into the scheduler-actor
//! command channel — the actor is the single writer of
//! [`SchedulerCore`](crate::scheduler::SchedulerCore). The executor never
//! touches scheduler state itself. [`FakeTransferExecutor`] records
//! start/end clock intervals per attempt so tests can prove serial vs
//! overlapping execution (contract §12 / plan acceptance point 2).
use ak_core::error::ErrorCode;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

use crate::runtime::actor::Command;
use crate::runtime::clock::RuntimeClock;
use crate::runtime::error::EngineStartError;
use crate::runtime::types::TransferSpec;
use crate::scheduler::key::{TaskHandle, TaskId};
use crate::scheduler::state::{FinishOutcome, TaskPhase};

/// A progress report from a running task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorReport {
    /// The task reached a new phase (validated by the scheduler against
    /// the contract §9 edges).
    Phase {
        /// The task.
        task: TaskHandle,
        /// The phase.
        phase: TaskPhase,
    },
    /// The task ended; `Failed` drives the runtime's backoff/park/cancel
    /// paths, `Completed` is terminal success.
    Finished {
        /// The task.
        task: TaskHandle,
        /// How it ended.
        outcome: FinishOutcome,
    },
}

/// Handle an executor uses to report progress back into the runtime.
///
/// Cheap to clone; sends land on the bounded command channel owned by the
/// scheduler-actor loop, so executor progress can never deadlock the
/// actor.
#[derive(Clone)]
pub struct ProgressSink {
    tx: mpsc::Sender<Command>,
}

impl ProgressSink {
    /// Wrap a command-channel sender (runtime-internal constructor).
    pub(crate) fn new(tx: mpsc::Sender<Command>) -> Self {
        ProgressSink { tx }
    }

    /// Report a phase transition.
    pub async fn phase(&self, task: TaskHandle, phase: TaskPhase) {
        let _ = self
            .tx
            .send(Command::Report(ExecutorReport::Phase { task, phase }))
            .await;
    }

    /// Report the terminal (or failure) outcome.
    pub async fn finished(&self, task: TaskHandle, outcome: FinishOutcome) {
        let _ = self
            .tx
            .send(Command::Report(ExecutorReport::Finished { task, outcome }))
            .await;
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressSink")
    }
}

/// Injected engine dependency (wave 20: fake; wave 21: the `ak-engine`
/// on-channel executor adapter).
///
/// `enqueue` starts one admitted task and arranges all further reports
/// through `sink`; it must not block on I/O. `cancel` asks a previously
/// enqueued task to stop; the task then reports
/// `Finished { Failed { CANCELLED } }` — unless the final commit already
/// won the race (contract §10).
pub trait TransferExecutor: Send + Sync {
    /// Begin executing one admitted task.
    fn enqueue(
        &self,
        task: TaskHandle,
        spec: TransferSpec,
        sink: ProgressSink,
    ) -> Result<(), EngineStartError>;

    /// Request cancellation of a started task (no-op for unknown tasks).
    fn cancel(&self, task: &TaskHandle);
}

/// Script describing how the fake executes one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeScript {
    /// Walk the phases and complete after `scan_ms + transfer_ms`.
    Complete {
        /// Fake ms spent scanning.
        scan_ms: u64,
        /// Fake ms spent transferring.
        transfer_ms: u64,
    },
    /// Fail with `error` for the first `times` attempts, then complete
    /// (`then_transfer_ms` is the transfer duration of the successful
    /// attempt).
    Fail {
        /// Fake ms before the failure surfaces.
        scan_ms: u64,
        /// Stable error to fail with.
        error: ErrorCode,
        /// How many attempts fail.
        times: u32,
        /// Transfer duration of the eventual success.
        then_transfer_ms: u64,
    },
    /// Reach `Transferring` and hang until cancelled (shutdown/timeout
    /// tests).
    NeverFinish {
        /// Fake ms spent scanning first.
        scan_ms: u64,
    },
}

/// One recorded execution interval (fake-clock domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInterval {
    /// The task.
    pub task: TaskId,
    /// When the executor started the attempt.
    pub start_ms: u64,
    /// When the attempt ended (`None` while running).
    pub end_ms: Option<u64>,
}

impl TaskInterval {
    /// Whether two finished intervals overlap.
    pub fn overlaps(&self, other: &TaskInterval) -> bool {
        match (self.end_ms, other.end_ms) {
            (Some(end_a), Some(end_b)) => self.start_ms < end_b && other.start_ms < end_a,
            _ => false,
        }
    }
}

/// Per-attempt cancellation signal (`watch` so a cancel issued before the
/// driver parks is still observed — no lost wakeup).
struct TaskCtl {
    cancel_tx: watch::Sender<bool>,
}

impl TaskCtl {
    fn channel() -> (Arc<TaskCtl>, watch::Receiver<bool>) {
        let (cancel_tx, cancelled) = watch::channel(false);
        (Arc::new(TaskCtl { cancel_tx }), cancelled)
    }

    fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }
}

/// Sleep on the injected clock, waking early on cancellation.
/// Returns `false` when the task was cancelled.
async fn sleep_cancellable(
    clock: &Arc<dyn RuntimeClock>,
    cancelled: &mut watch::Receiver<bool>,
    deadline_ms: u64,
) -> bool {
    loop {
        if *cancelled.borrow() {
            return false;
        }
        if clock.now_ms() >= deadline_ms {
            return true;
        }
        tokio::select! {
            _ = clock.sleep_until(deadline_ms) => {
                return !*cancelled.borrow();
            }
            _ = cancelled.changed() => {
                if *cancelled.borrow() {
                    return false;
                }
            }
        }
    }
}

/// Record the end of the latest unfinished interval of `task`.
fn record_end(shared: &FakeShared, task: TaskId) {
    let now = shared.clock.now_ms();
    let mut state = shared.state.lock().expect("fake executor lock");
    if let Some(slot) = state
        .intervals
        .iter_mut()
        .rev()
        .find(|i| i.task == task && i.end_ms.is_none())
    {
        slot.end_ms = Some(now);
    }
}

#[derive(Default)]
struct FakeState {
    default: Option<FakeScript>,
    scripts: HashMap<TaskId, FakeScript>,
    intervals: Vec<TaskInterval>,
    starts: usize,
    cancel_calls: Vec<TaskId>,
    ctls: HashMap<TaskId, Arc<TaskCtl>>,
}

struct FakeShared {
    clock: Arc<dyn RuntimeClock>,
    state: Mutex<FakeState>,
}

/// Deterministic fake executor (see the module docs).
///
/// Every `enqueue` spawns a driver task that walks the contract §9 phases
/// on the injected clock; per-attempt `[start_ms, end_ms)` intervals are
/// recorded for serial/overlap assertions.
pub struct FakeTransferExecutor {
    shared: Arc<FakeShared>,
}

impl FakeTransferExecutor {
    /// Create a fake driven by `clock`; the default script is
    /// `Complete { scan_ms: 20, transfer_ms: 80 }`.
    pub fn new(clock: Arc<dyn RuntimeClock>) -> Self {
        FakeTransferExecutor {
            shared: Arc::new(FakeShared {
                clock,
                state: Mutex::new(FakeState {
                    default: Some(FakeScript::Complete {
                        scan_ms: 20,
                        transfer_ms: 80,
                    }),
                    ..FakeState::default()
                }),
            }),
        }
    }

    /// Override the script used for tasks without an explicit one.
    pub fn set_default_script(&self, script: FakeScript) {
        self.shared
            .state
            .lock()
            .expect("fake executor lock")
            .default = Some(script);
    }

    /// Pin a per-task script (selected on every attempt).
    pub fn set_script(&self, task: TaskId, script: FakeScript) {
        self.shared
            .state
            .lock()
            .expect("fake executor lock")
            .scripts
            .insert(task, script);
    }

    /// Recorded intervals, in attempt order.
    pub fn intervals(&self) -> Vec<TaskInterval> {
        self.shared
            .state
            .lock()
            .expect("fake executor lock")
            .intervals
            .clone()
    }

    /// Recorded intervals of one task, in attempt order.
    pub fn intervals_of(&self, task: TaskId) -> Vec<TaskInterval> {
        self.shared
            .state
            .lock()
            .expect("fake executor lock")
            .intervals
            .iter()
            .filter(|i| i.task == task)
            .copied()
            .collect()
    }

    /// Total attempts started.
    pub fn start_count(&self) -> usize {
        self.shared.state.lock().expect("fake executor lock").starts
    }

    /// Tasks the runtime asked to cancel, in order.
    pub fn cancel_calls(&self) -> Vec<TaskId> {
        self.shared
            .state
            .lock()
            .expect("fake executor lock")
            .cancel_calls
            .clone()
    }
}

impl TransferExecutor for FakeTransferExecutor {
    fn enqueue(
        &self,
        task: TaskHandle,
        _spec: TransferSpec,
        sink: ProgressSink,
    ) -> Result<(), EngineStartError> {
        let id = task.task_id();
        let (ctl, cancelled) = TaskCtl::channel();
        let (start_ms, scan_ms, transfer_ms, failure) =
            {
                let mut state = self.shared.state.lock().expect("fake executor lock");
                state.starts += 1;
                let attempt = state.intervals.iter().filter(|i| i.task == id).count() as u32 + 1;
                let script = state.scripts.get(&id).copied().or(state.default).unwrap_or(
                    FakeScript::Complete {
                        scan_ms: 20,
                        transfer_ms: 80,
                    },
                );
                let shaped = match script {
                    FakeScript::Complete {
                        scan_ms,
                        transfer_ms,
                    } => (scan_ms, transfer_ms, None),
                    FakeScript::Fail {
                        scan_ms,
                        error,
                        times,
                        then_transfer_ms,
                    } => {
                        if attempt <= times {
                            (scan_ms, 0, Some(error))
                        } else {
                            (scan_ms, then_transfer_ms, None)
                        }
                    }
                    FakeScript::NeverFinish { scan_ms } => (scan_ms, u64::MAX / 4, None),
                };
                let start_ms = self.shared.clock.now_ms();
                state.intervals.push(TaskInterval {
                    task: id,
                    start_ms,
                    end_ms: None,
                });
                state.ctls.insert(id, ctl);
                (start_ms, shaped.0, shaped.1, shaped.2)
            };
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let mut cancelled = cancelled;
            sink.phase(task, TaskPhase::Scanning).await;
            if !sleep_cancellable(&shared.clock, &mut cancelled, start_ms + scan_ms).await {
                record_end(&shared, id);
                sink.finished(
                    task,
                    FinishOutcome::Failed {
                        error: ErrorCode::Cancelled,
                    },
                )
                .await;
                return;
            }
            sink.phase(task, TaskPhase::Transferring).await;
            if !sleep_cancellable(
                &shared.clock,
                &mut cancelled,
                start_ms + scan_ms + transfer_ms,
            )
            .await
            {
                record_end(&shared, id);
                sink.finished(
                    task,
                    FinishOutcome::Failed {
                        error: ErrorCode::Cancelled,
                    },
                )
                .await;
                return;
            }
            sink.phase(task, TaskPhase::Verifying).await;
            sink.phase(task, TaskPhase::Committing).await;
            record_end(&shared, id);
            let outcome = match failure {
                Some(error) => FinishOutcome::Failed { error },
                None => FinishOutcome::Completed,
            };
            sink.finished(task, outcome).await;
        });
        Ok(())
    }

    fn cancel(&self, task: &TaskHandle) {
        let mut state = self.shared.state.lock().expect("fake executor lock");
        state.cancel_calls.push(task.task_id());
        if let Some(ctl) = state.ctls.get(&task.task_id()) {
            ctl.cancel();
        }
    }
}
