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
//! Cancellation: the engine kernel has no cooperative cancel handle, so
//! `cancel` aborts the spawned engine task (part files and checkpoints
//! stay behind, resumable exactly like a peer drop) and reports
//! `Failed { CANCELLED }` on its behalf.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use ak_core::error::ErrorCode;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::runtime::error::EngineStartError;
use crate::runtime::executor::{ProgressSink, TransferExecutor};
use crate::runtime::types::{TransferDirection, TransferSpec};
use crate::scheduler::key::{TaskHandle, TaskId};
use crate::scheduler::state::{FinishOutcome, TaskPhase};

/// How many finished outcomes stay queryable (per task id, latest wins).
const MAX_REMEMBERED_OUTCOMES: usize = 8;

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

/// Running per-task bookkeeping (for `cancel`).
struct RunningTask {
    sink: ProgressSink,
    abort: AbortHandle,
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
        // callback into the driver task through an unbounded channel; the
        // only event forwarded today is `Transferring` (on `Connected`),
        // so the channel cannot grow without bound.
        let (phase_tx, mut phase_rx) = mpsc::unbounded_channel::<TaskPhase>();
        let observer = self.observer.clone();
        let progress = move |progress: ak_engine::Progress| {
            if let Some(observer) = observer.as_ref() {
                observer(progress.clone());
            }
            if matches!(progress, ak_engine::Progress::Connected { .. }) {
                let _ = phase_tx.send(TaskPhase::Transferring);
            }
        };

        let task_sink = sink.clone();
        let shared_state = Arc::clone(&self.state);
        let engine_task = tokio::spawn(async move {
            task_sink.phase(task, TaskPhase::Scanning).await;
            let engine_fut = ak_engine::receive_with_addr(engine_spec, &progress, addr_tx);
            tokio::pin!(engine_fut);
            let mut transferring_reported = false;
            let result = loop {
                tokio::select! {
                    phase = phase_rx.recv() => {
                        if let Some(phase) = phase {
                            if phase == TaskPhase::Transferring {
                                transferring_reported = true;
                            }
                            task_sink.phase(task, phase).await;
                        }
                    }
                    outcome = &mut engine_fut => break outcome,
                }
            };
            // Record the outcome in a lexically-scoped block: a std
            // MutexGuard must never sit across an await point, and the
            // block boundary makes that provable for the Send check.
            let failure_code = {
                let mut state = shared_state.lock().expect("real executor lock");
                state.running.remove(&id);
                match result {
                    Ok(report) => {
                        Self::record_outcome(&mut state, id, RealOutcome::Completed { report });
                        None
                    }
                    Err(err) => {
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
        });

        let abort = engine_task.abort_handle();
        self.state
            .lock()
            .expect("real executor lock")
            .running
            .insert(id, RunningTask { sink, abort });
        Ok(())
    }

    fn cancel(&self, task: &TaskHandle) {
        let id = task.task_id();
        let entry = self
            .state
            .lock()
            .expect("real executor lock")
            .running
            .remove(&id);
        let Some(entry) = entry else {
            return;
        };
        // Abort the engine kernel (part files/checkpoints stay resumable,
        // exactly like a peer drop) and report the cancellation on its
        // behalf — the aborted task can no longer report itself.
        entry.abort.abort();
        let sink = entry.sink;
        tokio::spawn(async move {
            sink.finished(
                TaskHandle::new(id),
                FinishOutcome::Failed {
                    error: ErrorCode::Cancelled,
                },
            )
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::actor::{test_command_channel, Command};
    use crate::runtime::executor::ExecutorReport;
    use crate::runtime::types::PairingPlan;
    use crate::scheduler::key::{CodeGroupKey, CodeId, RegistryId};
    use std::time::Duration;

    fn group() -> CodeGroupKey {
        CodeGroupKey::new(RegistryId::from_bytes([7; 32]), CodeId::from_bytes([9; 16]))
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
