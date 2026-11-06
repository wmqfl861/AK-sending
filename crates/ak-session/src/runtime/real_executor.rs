//! Real engine adapter: [`RealTransferExecutor`] (wave 24).
//!
//! This is the production binding of the injected
//! [`TransferExecutor`](crate::runtime::TransferExecutor) trait: one
//! admitted task runs one real `ak-engine` transfer and reports the
//! contract §9 phases plus the terminal outcome back through the
//! [`ProgressSink`](crate::runtime::ProgressSink).
//!
//! **Entry-point choice (recorded per the wave-24 brief):** the adapter
//! wraps the legacy [`ak_engine::receive_with_addr`] bind-accept-receive
//! kernel, NOT the wave-21 on-channel pair
//! (`send_on_channel`/`receive_on_channel`). Reasons:
//!
//! * the on-channel entries require the runtime to own an
//!   already-connected direct channel plus a full `AdmissionCtx`/decider
//!   wiring (the B2/B3 `TaskOpen` handshake) — the actor's driver still
//!   dispatches executors without that handshake, so an on-channel
//!   binding would be a wire mismatch, not a working path;
//! * a receive-side on-channel binding refuses every legacy sender
//!   (`PROTOCOL_UNSUPPORTED`, by design on that entry), breaking the
//!   compatibility requirement that the daemon keeps accepting the
//!   existing direct senders;
//! * `receive_with_addr` is exactly the kernel the daemon loop ran before
//!   wave 24, so behavior (idle budgets, staging, checkpoints, authorize
//!   callback) is unchanged — only the ownership moved into the
//!   scheduler.
//!
//! Phase mapping: `Scanning` is reported when the engine call starts
//! (bind + accept), `Transferring` when the engine reports `Connected`
//! (the handshake completed and file bytes may move), and
//! `Verifying`/`Committing` when the engine call returns successfully —
//! the legacy kernel verifies and commits before returning, so those two
//! phases are reported back-to-back at completion. Failures report
//! `Finished { Failed { code } }` with a stable
//! [`ErrorCode`](ak_core::error::ErrorCode) classification.
//!
//! Cancellation (P1-03): the engine kernel has no cooperative cancel
//! handle, so the spawned work task races the kernel future against a
//! cancel flag. On cancellation it drops the kernel future — releasing
//! the bound port and stopping staged writes — and only then records and
//! reports `Failed { CANCELLED }` (part files and checkpoints stay
//! behind, resumable exactly like a peer drop). A cancellation that races
//! the final commit loses against the persisted state: when the engine
//! already saved its terminal `completed` checkpoint (observed as the
//! `FinalAck { verified: true }` progress, which the kernel emits strictly
//! after that save and before its bounded FinalAck tail-hold), the work
//! task lets the kernel finish and reports `Completed`. The work task's
//! `JoinHandle` stays joinable through [`TransferExecutor::take_work`] so
//! shutdown can prove the engine exited before answering.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ak_core::error::ErrorCode;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::runtime::error::EngineStartError;
use crate::runtime::executor::{ExecutorWork, ProgressSink, TransferExecutor};
use crate::runtime::types::{TransferDirection, TransferSpec};
use crate::scheduler::key::{TaskHandle, TaskId};
use crate::scheduler::state::{FinishOutcome, TaskPhase};

/// How many finished outcomes stay queryable (per task id, latest wins).
const MAX_REMEMBERED_OUTCOMES: usize = 8;

/// Grace given to the engine kernel when a cancellation races the final
/// commit (P1-03). Between the terminal `completed` checkpoint save and
/// the `FinalAck { verified: true }` progress event the kernel is a few
/// quick frame writes away from a fully persisted completion; 250 ms lets
/// that finish (and report `Completed`) while keeping an ordinary
/// mid-transfer cancel prompt — the drop itself is what stops the port
/// and the writes, so the bound only delays the synthesized report.
const FINAL_COMMIT_GRACE: Duration = Duration::from_millis(250);

/// The classified failure of one real transfer attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealFailure {
    /// Stable scheduler error code (what `FinishOutcome::Failed` carried).
    pub code: ErrorCode,
    /// The raw io error kind, when the engine failure was an io error
    /// (the daemon's bind-retry logic keys on `AddrInUse`).
    pub io_kind: Option<std::io::ErrorKind>,
    /// Bounded human-readable description.
    pub message: String,
}

/// The outcome of one real transfer, queryable after the task reported
/// `Finished` into the runtime.
#[derive(Debug, Clone)]
pub enum RealOutcome {
    /// The engine returned a report: the transfer completed, every file
    /// verified and committed.
    Completed {
        /// The engine's own final report.
        report: ak_engine::Report,
    },
    /// The engine call failed (classification inside).
    Failed(RealFailure),
}

/// Running per-task bookkeeping (for `cancel` and `take_work`).
struct RunningTask {
    /// Cooperative cancellation flag (set by `cancel`).
    cancel: watch::Sender<bool>,
    /// The joinable engine-work task (P1-03: cancelable AND awaitable —
    /// handed to the runtime by `take_work` so shutdown can join the real
    /// engine before it answers).
    join: JoinHandle<()>,
}

#[derive(Default)]
struct RealState {
    running: HashMap<TaskId, RunningTask>,
    outcomes: HashMap<TaskId, RealOutcome>,
    outcome_order: Vec<TaskId>,
}

/// One-shot real receive executor: wraps exactly one
/// `ak_engine::receive_with_addr` call.
///
/// The daemon constructs one per receive-loop iteration (the engine
/// `ReceiveSpec` — bind address, identity, accept policy — belongs to that
/// iteration), passes it as the task's `TransferSpec::executor`, and reads
/// [`RealTransferExecutor::outcome`] when the runtime reports the task
/// finished. A second `enqueue` on the same instance fails fast; a send
/// direction is refused.
pub struct RealTransferExecutor {
    spec: Mutex<Option<ak_engine::ReceiveSpec>>,
    addr_tx: Mutex<Option<oneshot::Sender<SocketAddr>>>,
    observer: Option<Arc<dyn Fn(ak_engine::Progress) + Send + Sync>>,
    state: Arc<Mutex<RealState>>,
}

impl std::fmt::Debug for RealTransferExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The engine spec and observer are not Debug; report only the
        // facts a log reader needs.
        let armed = self.spec.lock().map(|spec| spec.is_some()).unwrap_or(false);
        f.debug_struct("RealTransferExecutor")
            .field("armed_spec", &armed)
            .field("observer", &self.observer.is_some())
            .finish_non_exhaustive()
    }
}

/// Classify an engine failure onto the stable scheduler error code.
fn stable_code(err: &ak_engine::EngineError) -> (ErrorCode, Option<std::io::ErrorKind>) {
    use ak_engine::EngineError;
    match err {
        EngineError::Connect(ak_net::ConnectError::Unauthorized) => (ErrorCode::AuthFailed, None),
        EngineError::Connect(_) => (ErrorCode::DirectUnavailable, None),
        EngineError::Stream(_) | EngineError::PeerClosed | EngineError::IdleTimeout(_) => {
            (ErrorCode::DirectUnavailable, None)
        }
        EngineError::Rejected(code) | EngineError::PeerError { code, .. } => (*code, None),
        EngineError::SourceChanged => (ErrorCode::SourceChanged, None),
        EngineError::Storage(_) | EngineError::Checkpoint(_) => (ErrorCode::StorageError, None),
        EngineError::StorageFull { .. } => (ErrorCode::StorageFull, None),
        EngineError::Io(io) => (ErrorCode::StorageError, Some(io.kind())),
        EngineError::VerificationFailed => (ErrorCode::HashMismatch, None),
        EngineError::Cancelled => (ErrorCode::Cancelled, None),
        EngineError::RouteForbidden(_) => (ErrorCode::RouteForbidden, None),
        EngineError::AdmissionDenied { .. } => (ErrorCode::ProtocolUnsupported, None),
        EngineError::InvalidAdmissionCtx { .. } | EngineError::PeerMismatch { .. } => {
            (ErrorCode::PeerMismatch, None)
        }
    }
}

/// Bound an error message for storage/reporting.
fn bounded(message: String) -> String {
    message.chars().take(160).collect()
}

impl RealTransferExecutor {
    /// Build the executor for one receive iteration.
    ///
    /// * `spec` — the fully-resolved engine receive spec (bind address,
    ///   identity, accept policy, destination, checkpoints);
    /// * `addr_tx` — receives the concrete bound address before the first
    ///   accept (what `receive_with_addr` hands its caller); optional;
    /// * `observer` — optional progress callback forwarded every engine
    ///   progress event (the daemon's NDJSON/line printer rides here).
    pub fn receive(
        spec: ak_engine::ReceiveSpec,
        addr_tx: Option<oneshot::Sender<SocketAddr>>,
        observer: Option<Arc<dyn Fn(ak_engine::Progress) + Send + Sync>>,
    ) -> Self {
        RealTransferExecutor {
            spec: Mutex::new(Some(spec)),
            addr_tx: Mutex::new(addr_tx),
            observer,
            state: Arc::new(Mutex::new(RealState::default())),
        }
    }

    /// The outcome of the task's attempt, once it finished. `None` while
    /// the transfer is running (or the executor was never enqueued).
    pub fn outcome(&self, task: &TaskId) -> Option<RealOutcome> {
        self.state
            .lock()
            .expect("real executor lock")
            .outcomes
            .get(task)
            .cloned()
    }

    /// Whether the engine call has settled (any outcome recorded).
    pub fn is_settled(&self, task: &TaskId) -> bool {
        self.state
            .lock()
            .expect("real executor lock")
            .outcomes
            .contains_key(task)
    }

    fn record_outcome(state: &mut RealState, task: TaskId, outcome: RealOutcome) {
        if state.outcomes.insert(task, outcome).is_none() {
            state.outcome_order.push(task);
            if state.outcome_order.len() > MAX_REMEMBERED_OUTCOMES {
                let evict = state.outcome_order.remove(0);
                state.outcomes.remove(&evict);
            }
        }
    }
}

impl TransferExecutor for RealTransferExecutor {
    fn enqueue(
        &self,
        task: TaskHandle,
        spec: TransferSpec,
        sink: ProgressSink,
    ) -> Result<(), EngineStartError> {
        if spec.direction != TransferDirection::Receive {
            return Err(EngineStartError::new(
                ErrorCode::Internal,
                "the real executor is receive-only in wave 24",
            ));
        }
        let id = task.task_id();
        let engine_spec = self
            .spec
            .lock()
            .expect("real executor lock")
            .take()
            .ok_or_else(|| {
                EngineStartError::new(
                    ErrorCode::Internal,
                    "this real executor already ran its one receive spec",
                )
            })?;
        let addr_tx = self
            .addr_tx
            .lock()
            .expect("real executor lock")
            .take()
            .unwrap_or_else(|| oneshot::channel::<SocketAddr>().0);

        // Phase events cross from the engine's synchronous progress
        // callback into the work task through an unbounded channel; the
        // only event forwarded today is `Transferring` (on `Connected`),
        // so the channel cannot grow without bound.
        let (phase_tx, mut phase_rx) = mpsc::unbounded_channel::<TaskPhase>();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        // "The completion is persisted": ak-engine saves the terminal
        // `completed` checkpoint strictly before emitting
        // `FinalAck { verified: true }` and before its bounded FinalAck
        // tail-hold, so this flag is exactly the signal a cancellation
        // must lose against (P1-03).
        let final_ack = Arc::new(AtomicBool::new(false));
        let observer = self.observer.clone();
        let progress_final_ack = Arc::clone(&final_ack);
        let progress = move |progress: ak_engine::Progress| {
            if let Some(observer) = observer.as_ref() {
                observer(progress.clone());
            }
            match progress {
                ak_engine::Progress::Connected { .. } => {
                    let _ = phase_tx.send(TaskPhase::Transferring);
                }
                ak_engine::Progress::FinalAck { verified: true } => {
                    progress_final_ack.store(true, Ordering::Release);
                }
                _ => {}
            }
        };

        let task_sink = sink.clone();
        let shared_state = Arc::clone(&self.state);
        let engine_task = tokio::spawn(async move {
            let mut cancel_rx = cancel_rx;
            task_sink.phase(task, TaskPhase::Scanning).await;
            let mut engine_fut =
                Box::pin(ak_engine::receive_with_addr(engine_spec, &progress, addr_tx));
            let mut transferring_reported = false;
            let result = loop {
                tokio::select! {
                    biased;
                    phase = phase_rx.recv() => {
                        if let Some(phase) = phase {
                            if phase == TaskPhase::Transferring {
                                transferring_reported = true;
                            }
                            task_sink.phase(task, phase).await;
                        }
                    }
                    outcome = &mut engine_fut => break Some(outcome),
                    _ = cancel_rx.changed() => {
                        if !*cancel_rx.borrow_and_update() {
                            continue;
                        }
                        if final_ack.load(Ordering::Acquire) {
                            // Persisted completion: let the kernel finish
                            // (its FinalAck tail-hold is bounded by the
                            // engine itself) and report the real outcome —
                            // never Cancelled (P1-03).
                            break Some((&mut engine_fut).await);
                        }
                        // Final-commit race grace: the completion may be
                        // a few frame writes from persisting; give it a
                        // short bounded chance before dropping the kernel.
                        match tokio::time::timeout(FINAL_COMMIT_GRACE, &mut engine_fut).await {
                            Ok(outcome) => break Some(outcome),
                            Err(_grace_elapsed) => break None,
                        }
                    }
                }
            };
            // Drop the kernel BEFORE recording and reporting (P1-03): the
            // bound port is released and staged writes stop here, so by
            // the time the runtime can observe this task's outcome the
            // real engine work is already off the wire. Part files and
            // checkpoints stay behind, resumable exactly like a peer drop.
            drop(engine_fut);
            // Record the outcome in a lexically-scoped block: a std
            // MutexGuard must never sit across an await point, and the
            // block boundary makes that provable for the Send check.
            let failure_code = {
                let mut state = shared_state.lock().expect("real executor lock");
                match result {
                    Some(Ok(report)) => {
                        Self::record_outcome(&mut state, id, RealOutcome::Completed { report });
                        None
                    }
                    Some(Err(err)) => {
                        let (code, io_kind) = stable_code(&err);
                        Self::record_outcome(
                            &mut state,
                            id,
                            RealOutcome::Failed(RealFailure {
                                code,
                                io_kind,
                                message: bounded(err.to_string()),
                            }),
                        );
                        Some(code)
                    }
                    None => {
                        // Dropped on cancellation: the work task reports
                        // the synthesized cancellation on the kernel's
                        // behalf, strictly after the kernel exited.
                        Self::record_outcome(
                            &mut state,
                            id,
                            RealOutcome::Failed(RealFailure {
                                code: ErrorCode::Cancelled,
                                io_kind: None,
                                message: "cancelled: the engine kernel was dropped".to_owned(),
                            }),
                        );
                        Some(ErrorCode::Cancelled)
                    }
                }
            };
            match failure_code {
                None => {
                    if !transferring_reported {
                        // Defensive: `Connected` is always reported by the
                        // receive kernel, but a phase report must never be
                        // silently missing on the success path.
                        task_sink.phase(task, TaskPhase::Transferring).await;
                    }
                    task_sink.phase(task, TaskPhase::Verifying).await;
                    task_sink.phase(task, TaskPhase::Committing).await;
                    task_sink.finished(task, FinishOutcome::Completed).await;
                }
                Some(code) => {
                    task_sink
                        .finished(task, FinishOutcome::Failed { error: code })
                        .await;
                }
            }
            // Self-deregister last (P1-03): `take_work` may hand out the
            // join handle up to this point; after the removal the work is
            // done by definition.
            shared_state
                .lock()
                .expect("real executor lock")
                .running
                .remove(&id);
        });

        self.state
            .lock()
            .expect("real executor lock")
            .running
            .insert(id, RunningTask {
                cancel: cancel_tx,
                join: engine_task,
            });
        Ok(())
    }

    fn cancel(&self, task: &TaskHandle) {
        // Cooperative stop (P1-03): the work task observes the flag at its
        // current await point, drops the engine kernel (releasing the
        // bound port, stopping writes) and only then records and reports
        // its outcome. An already-persisted completion is adopted by the
        // work task itself — never overwritten with Cancelled.
        let state = self.state.lock().expect("real executor lock");
        if let Some(running) = state.running.get(&task.task_id()) {
            let _ = running.cancel.send(true);
        }
    }

    fn take_work(&self, task: &TaskHandle) -> Option<ExecutorWork> {
        self.state
            .lock()
            .expect("real executor lock")
            .running
            .remove(&task.task_id())
            .map(|running| ExecutorWork::new(running.join))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::actor::{test_command_channel, Command};
    use crate::runtime::executor::ExecutorReport;
    use crate::runtime::types::PairingPlan;
    use crate::runtime::{
        FakePairingTransport, FakeTransferExecutor, PairingTransport, RealClock, RuntimeClock,
        RuntimeConfig, SessionRuntime, ShutdownMode, SnapshotQuery, TaskState,
    };
    use crate::scheduler::config::SchedulerPolicy;
    use crate::scheduler::key::{CodeGroupKey, CodeId, RegistryId};
    use std::time::{Duration, Instant};

    fn group() -> CodeGroupKey {
        CodeGroupKey::new(RegistryId::from_bytes([7; 32]), CodeId::from_bytes([9; 16]))
    }

    /// A real receive spec bound to an auto-assigned loopback port with a
    /// generous idle budget (the tests cancel/shutdown on purpose).
    fn loopback_spec(out: &std::path::Path) -> ak_engine::ReceiveSpec {
        let key_b = ak_identity::DeviceKeyPair::generate();
        let identity_b = ak_identity::tls::make_self_signed(&key_b);
        let mut spec = ak_engine::ReceiveSpec::new(out.to_owned(), identity_b);
        spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        spec.idle_timeout = Duration::from_secs(20);
        spec.authorize = Box::new(|_| true);
        spec
    }

    /// A runtime with the real clock and a real receive executor armed for
    /// `PairingPlan::Local` (the daemon's resident receive shape), plus
    /// the port it bound and the task id it runs under.
    async fn real_receive_runtime(
        out: &std::path::Path,
        shutdown_timeout_ms: u64,
    ) -> (
        crate::runtime::SessionHandle,
        Arc<RealTransferExecutor>,
        crate::runtime::TransferId,
        SocketAddr,
    ) {
        let (addr_tx, addr_rx) = oneshot::channel();
        let executor = Arc::new(RealTransferExecutor::receive(
            loopback_spec(out),
            Some(addr_tx),
            None,
        ));
        let clock = RealClock::shared();
        let mut config = RuntimeConfig::new(
            SchedulerPolicy::default(),
            Arc::clone(&clock) as Arc<dyn RuntimeClock>,
            Arc::new(FakePairingTransport::new(true)) as Arc<dyn PairingTransport>,
            Arc::new(FakeTransferExecutor::new(clock)) as Arc<dyn TransferExecutor>,
            RegistryId::from_bytes([0x11; 32]),
        );
        config.listener_enabled = false;
        config.shutdown_timeout_ms = shutdown_timeout_ms;
        let handle = SessionRuntime::start(config).expect("runtime starts");
        let task = handle
            .enqueue(TransferSpec {
                group_key: group(),
                pairing: PairingPlan::Local,
                executor: executor.clone() as Arc<dyn TransferExecutor>,
                direction: TransferDirection::Receive,
                label: "real-accept".to_owned(),
                task_id: None,
            })
            .await
            .expect("real receive task submitted");
        let bound = tokio::time::timeout(Duration::from_secs(10), addr_rx)
            .await
            .expect("the engine binds its address")
            .expect("addr delivered");
        (handle, executor, task, bound)
    }

    /// P1-03: a Drain shutdown whose deadline expires while the real
    /// engine sits in `accept` must stop AND join that engine before the
    /// summary is answered — when `shutdown` returns, the UDP port is
    /// already free (a fresh bind succeeds) and the task carries a real
    /// recorded cancellation outcome, not a synthesized-after-the-fact
    /// state on a still-running engine.
    #[tokio::test]
    async fn drain_timeout_releases_the_real_bound_port_before_answering() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (handle, executor, task, bound) =
            real_receive_runtime(&out, 400).await;

        let summary = tokio::time::timeout(
            Duration::from_secs(15),
            handle.shutdown(ShutdownMode::Drain),
        )
        .await
        .expect("shutdown answers once the engine joined")
        .expect("shutdown succeeds");
        assert!(summary.timed_out, "the accept never finishes on its own");
        assert_eq!(summary.cancelled, 1);
        assert_eq!(summary.completed, 0);

        // The engine kernel exited before the summary: rebinding the
        // exact bound address must succeed immediately (no sleep, no
        // retry — the join in the actor's shutdown path is the guarantee).
        std::net::UdpSocket::bind(bound)
            .expect("the real engine released its bound port before the summary");
        assert!(
            executor.is_settled(&task),
            "the engine work recorded its outcome"
        );
        match executor.outcome(&task) {
            Some(RealOutcome::Failed(failure)) => {
                assert_eq!(failure.code, ErrorCode::Cancelled);
            }
            other => panic!("expected a cancelled outcome, got {other:?}"),
        }
    }

    /// P1-03: cancelling a running real receive joins the engine work.
    /// The work handle is taken right after the (cooperative) cancel and
    /// joined: it resolves only after the kernel dropped (port free) and
    /// the outcome was recorded — and only then does the work task send
    /// its synthesized cancellation report.
    #[tokio::test]
    async fn cancel_running_real_task_joins_work_and_releases_the_port() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (addr_tx, addr_rx) = oneshot::channel();
        let executor = Arc::new(RealTransferExecutor::receive(
            loopback_spec(&out),
            Some(addr_tx),
            None,
        ));
        let (cmd_tx, mut cmd_rx) = test_command_channel();
        let handle = TaskHandle::new(TaskId::from_counter(31));
        executor
            .enqueue(
                handle,
                TransferSpec {
                    group_key: group(),
                    pairing: PairingPlan::Local,
                    executor: executor.clone() as Arc<dyn TransferExecutor>,
                    direction: TransferDirection::Receive,
                    label: "cancel-join".to_owned(),
                    task_id: None,
                },
                ProgressSink::new(cmd_tx),
            )
            .expect("enqueue starts the engine task");
        let bound = tokio::time::timeout(Duration::from_secs(10), addr_rx)
            .await
            .expect("bound in time")
            .expect("addr delivered");

        // Stop and take the work handle in one synchronous stretch (no
        // await between the calls, so the engine cannot have exited).
        executor.cancel(&handle);
        let work = executor
            .take_work(&handle)
            .expect("the running engine work is joinable");
        work.join_within(Duration::from_secs(5), Duration::from_secs(5))
            .await;

        // Joined ⇒ the kernel dropped (port free) and the outcome is
        // recorded; the report follows strictly after the exit.
        assert!(executor.is_settled(&handle.task_id()));
        match executor.outcome(&handle.task_id()) {
            Some(RealOutcome::Failed(failure)) => {
                assert_eq!(failure.code, ErrorCode::Cancelled);
            }
            other => panic!("expected a cancelled outcome, got {other:?}"),
        }
        std::net::UdpSocket::bind(bound)
            .expect("the real engine released its bound port on cancel");
        let mut cancelled_report = None;
        let reported = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Command::Report(ExecutorReport::Finished { outcome, .. }) = cmd {
                    cancelled_report = Some(outcome);
                    break;
                }
            }
        })
        .await;
        assert!(reported.is_ok(), "the work task reported its cancellation");
        assert_eq!(
            cancelled_report,
            Some(FinishOutcome::Failed {
                error: ErrorCode::Cancelled
            })
        );
    }

    /// P1-03 (runtime path): a user cancel of a running real receive
    /// finalizes to `Cancelled` strictly from the executor's report —
    /// which the engine task sends only after dropping the kernel. When
    /// the snapshot says `Cancelled`, the port is already free.
    #[tokio::test]
    async fn runtime_cancel_of_real_receive_finalizes_cancelled_and_frees_the_port() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (handle, executor, task, bound) = real_receive_runtime(&out, 10_000).await;

        let receipt = handle.cancel(task).await.expect("cancel the task");
        assert_eq!(receipt, crate::runtime::CancelReceipt::RunningCancellationRequested);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snap = handle
                .snapshot(SnapshotQuery::one(task))
                .await
                .expect("snapshot");
            if snap
                .tasks
                .iter()
                .any(|view| view.state == TaskState::Cancelled)
            {
                break;
            }
            assert!(Instant::now() < deadline, "task never reached Cancelled");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Cancelled is only reachable via the executor's report, sent
        // strictly after the kernel dropped: the port must rebind now.
        std::net::UdpSocket::bind(bound)
            .expect("the real engine released its bound port before Cancelled");
        match executor.outcome(&task) {
            Some(RealOutcome::Failed(failure)) => {
                assert_eq!(failure.code, ErrorCode::Cancelled);
            }
            other => panic!("expected a cancelled outcome, got {other:?}"),
        }
        let _ = handle.shutdown(ShutdownMode::Drain).await;
    }

    /// P1-03: a cancellation racing the final commit must not override a
    /// persisted completion. The observer fires `cancel` exactly when the
    /// engine emits `FinalAck { verified: true }` — strictly after the
    /// terminal `completed` checkpoint was saved, while the kernel is
    /// still finishing its FinalAck tail. The task must end `Completed`
    /// with the real report, never `Cancelled`.
    #[tokio::test]
    async fn cancel_racing_the_final_commit_keeps_the_persisted_completion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 199) as u8).collect();
        std::fs::write(src.join("payload.bin"), &payload).unwrap();

        // The observer needs the executor before it exists; bridge the
        // cancel through a slot filled right after construction.
        type CancelSlot = Arc<Mutex<Option<(Arc<RealTransferExecutor>, TaskHandle)>>>;
        let cancel_slot: CancelSlot = Arc::new(Mutex::new(None));
        let observer = {
            let cancel_slot = Arc::clone(&cancel_slot);
            Arc::new(move |progress: ak_engine::Progress| {
                if matches!(progress, ak_engine::Progress::FinalAck { verified: true }) {
                    if let Some((executor, handle)) = cancel_slot.lock().unwrap().as_ref() {
                        executor.cancel(handle);
                    }
                }
            }) as Arc<dyn Fn(ak_engine::Progress) + Send + Sync>
        };
        let (addr_tx, addr_rx) = oneshot::channel();
        let spec = loopback_spec(&out);
        let pinned = spec.identity.fingerprint;
        let executor = Arc::new(RealTransferExecutor::receive(
            spec,
            Some(addr_tx),
            Some(observer),
        ));
        let (cmd_tx, mut cmd_rx) = test_command_channel();
        let sink = ProgressSink::new(cmd_tx);
        let handle = TaskHandle::new(TaskId::from_counter(21));
        *cancel_slot.lock().unwrap() = Some((executor.clone(), handle));
        executor
            .enqueue(
                handle,
                TransferSpec {
                    group_key: group(),
                    pairing: PairingPlan::Local,
                    executor: executor.clone() as Arc<dyn TransferExecutor>,
                    direction: TransferDirection::Receive,
                    label: "race".to_owned(),
                    task_id: None,
                },
                sink,
            )
            .expect("enqueue starts the engine task");

        let bound = tokio::time::timeout(Duration::from_secs(10), addr_rx)
            .await
            .expect("bound in time")
            .expect("addr delivered");
        let key_a = ak_identity::DeviceKeyPair::generate();
        let identity_a = ak_identity::tls::make_self_signed(&key_a);
        let mut send_spec = ak_engine::SendSpec::new(src, bound, pinned, identity_a);
        send_spec.idle_timeout = Duration::from_secs(20);
        let report = tokio::time::timeout(
            Duration::from_secs(30),
            ak_engine::send(send_spec, &|_| {}),
        )
        .await
        .expect("send finishes")
        .expect("send succeeds");
        assert_eq!(report.files, 1);

        // The cancellation fired mid-completion; the persisted completion
        // must win: terminal `Completed`, never `Failed { Cancelled }`.
        let mut finished = None;
        let observed = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Command::Report(ExecutorReport::Finished { outcome, .. }) = cmd {
                    finished = Some(outcome);
                    break;
                }
            }
        })
        .await;
        assert!(observed.is_ok(), "the executor reported its finish");
        assert_eq!(finished, Some(FinishOutcome::Completed));
        match executor.outcome(&handle.task_id()) {
            Some(RealOutcome::Completed { report }) => assert_eq!(report.files, 1),
            other => panic!("expected the persisted completion, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(out.join("payload.bin")).unwrap(),
            payload,
            "the committed bytes are identical"
        );
    }

    /// End-to-end on one machine: a real executor (the real engine
    /// receive kernel on an auto-assigned loopback port) driven through
    /// the `TransferExecutor` trait, fed by a real legacy
    /// `ak_engine::send`. Asserts the phase walk, the terminal report and
    /// the committed bytes.
    #[tokio::test]
    async fn real_receive_executor_completes_a_loopback_transfer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let key_b = ak_identity::DeviceKeyPair::generate();
        let identity_b = ak_identity::tls::make_self_signed(&key_b);
        let mut spec = ak_engine::ReceiveSpec::new(out.clone(), identity_b);
        spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        spec.idle_timeout = Duration::from_secs(20);
        spec.authorize = Box::new(|_| true);

        let (addr_tx, addr_rx) = oneshot::channel();
        let executor = Arc::new(RealTransferExecutor::receive(spec, Some(addr_tx), None));

        // A command channel standing in for the actor, so the sink reports
        // are observable.
        let (cmd_tx, mut cmd_rx) = test_command_channel();
        let sink = ProgressSink::new(cmd_tx);
        let handle = TaskHandle::new(TaskId::from_counter(1));
        let transfer_spec = TransferSpec {
            group_key: group(),
            pairing: PairingPlan::Local,
            executor: executor.clone() as Arc<dyn TransferExecutor>,
            direction: TransferDirection::Receive,
            label: "loopback".to_owned(),
            task_id: None,
        };
        executor
            .enqueue(handle, transfer_spec, sink)
            .expect("enqueue starts the engine task");

        // The engine bound its address; feed one real send into it.
        let bound = tokio::time::timeout(Duration::from_secs(10), addr_rx)
            .await
            .expect("bound in time")
            .expect("addr delivered");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 199) as u8).collect();
        std::fs::write(src.join("payload.bin"), &payload).unwrap();
        let key_a = ak_identity::DeviceKeyPair::generate();
        let identity_a = ak_identity::tls::make_self_signed(&key_a);
        let mut send_spec = ak_engine::SendSpec::new(src, bound, key_b.fingerprint(), identity_a);
        send_spec.idle_timeout = Duration::from_secs(20);
        let report =
            tokio::time::timeout(Duration::from_secs(30), ak_engine::send(send_spec, &|_| {}))
                .await
                .expect("send finishes")
                .expect("send succeeds");
        assert_eq!(report.files, 1);

        // The executor finished and reported the full phase walk plus the
        // terminal outcome, in order.
        let mut seen_phases = Vec::new();
        let mut finished = None;
        let observed = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Command::Report(ExecutorReport::Phase { phase, .. }) => seen_phases.push(phase),
                    Command::Report(ExecutorReport::Finished { outcome, .. }) => {
                        finished = Some(outcome);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await;
        assert!(observed.is_ok(), "the executor reported its finish");
        assert_eq!(
            seen_phases,
            vec![
                TaskPhase::Scanning,
                TaskPhase::Transferring,
                TaskPhase::Verifying,
                TaskPhase::Committing,
            ],
            "full contract §9 walk"
        );
        assert_eq!(finished, Some(FinishOutcome::Completed));

        // The outcome is queryable and carries the real report.
        let id = handle.task_id();
        assert!(executor.is_settled(&id));
        match executor.outcome(&id) {
            Some(RealOutcome::Completed { report }) => {
                assert_eq!(report.files, 1);
                assert_eq!(report.bytes, payload.len() as u64);
            }
            other => panic!("expected a completed outcome, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(out.join("payload.bin")).unwrap(),
            payload,
            "the committed bytes are identical"
        );

        // Single-shot: a second enqueue of the same instance fails fast
        // (the one engine spec was consumed).
        let (cmd_tx2, _cmd_rx2) = test_command_channel();
        let err = executor.enqueue(
            handle,
            TransferSpec {
                group_key: group(),
                pairing: PairingPlan::Local,
                executor: executor.clone() as Arc<dyn TransferExecutor>,
                direction: TransferDirection::Receive,
                label: String::new(),
                task_id: None,
            },
            ProgressSink::new(cmd_tx2),
        );
        assert!(err.is_err(), "the one-shot spec is consumed");
    }

    #[test]
    fn send_direction_is_refused() {
        let identity = ak_identity::tls::make_self_signed(&ak_identity::DeviceKeyPair::generate());
        let spec = ak_engine::ReceiveSpec::new(std::path::PathBuf::from("."), identity);
        let executor = Arc::new(RealTransferExecutor::receive(spec, None, None));
        let (cmd_tx, _cmd_rx) = test_command_channel();
        let err = executor.enqueue(
            TaskHandle::new(TaskId::from_counter(9)),
            TransferSpec {
                group_key: group(),
                pairing: PairingPlan::Local,
                executor: executor.clone() as Arc<dyn TransferExecutor>,
                direction: TransferDirection::Send,
                label: String::new(),
                task_id: None,
            },
            ProgressSink::new(cmd_tx),
        );
        assert!(err.is_err());
    }

    #[test]
    fn engine_errors_map_onto_stable_codes() {
        use ak_engine::EngineError;
        let unauthorized = EngineError::Connect(ak_net::ConnectError::Unauthorized);
        assert_eq!(stable_code(&unauthorized).0, ErrorCode::AuthFailed);
        let io_addr_in_use = EngineError::Io(std::io::Error::from(std::io::ErrorKind::AddrInUse));
        let (code, kind) = stable_code(&io_addr_in_use);
        assert_eq!(code, ErrorCode::StorageError);
        assert_eq!(kind, Some(std::io::ErrorKind::AddrInUse));
        assert_eq!(
            stable_code(&EngineError::VerificationFailed).0,
            ErrorCode::HashMismatch
        );
        assert_eq!(
            stable_code(&EngineError::Rejected(ErrorCode::TargetRevoked)).0,
            ErrorCode::TargetRevoked
        );
    }
}
