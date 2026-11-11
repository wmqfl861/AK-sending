//! On-channel receive executor for daemon inbound tasks (wave 27,
//! final-review P1-02).
//!
//! Where [`RealTransferExecutor`](crate::runtime::RealTransferExecutor)
//! wraps the legacy *bind-accept-receive* kernel (one UDP port, an
//! anonymous all-zero group, one task per loop iteration),
//! [`ChannelReceiveExecutor`] wraps the scheduler-admitted form of an
//! **already accepted** [`DirectChannel`]: the daemon accepted the
//! connection, ran the wave-27 engine scheduler handshake
//! (`ak_engine::channel_scheduler_handshake`), obtained the sender's wire
//! `TaskOpen`, validated it against the local Ready relations
//! ([`AdmissionValidator`](crate::runtime::AdmissionValidator)) and now
//! binds the connection into a
//! [`TransferSpec`](crate::runtime::TransferSpec) whose group key came
//! from the *relation*, not the claim (contract §6). When the scheduler
//! admits that task, the executor's `run` completes the admission reply
//! and the whole receive body through the frozen wave-27 engine entry:
//!
//! ```text
//! ak_engine::receive_on_channel_preadmitted(ch, spec, open, progress)
//! ```
//!
//! Cancellation/shutdown follow the wave-26 `ExecutorWork` discipline
//! (P1-03, byte-for-byte the
//! [`RealTransferExecutor`](crate::runtime::RealTransferExecutor) pattern):
//! the work task races the engine future against a cooperative cancel
//! flag. On cancellation it drops the engine future first — closing the
//! accepted QUIC channel and stopping staged writes — and only then
//! records and reports `Failed { CANCELLED }`; part files and checkpoints
//! stay behind, resumable exactly like a peer drop. A cancellation that
//! races the final commit loses against the persisted state: once the
//! engine emitted `FinalAck { verified: true }` (strictly after its
//! terminal `completed` checkpoint save) — before the cancel *or during
//! the post-cancel grace window* — the work task lets the kernel finish
//! and reports `Completed`, never `Cancelled`. The work `JoinHandle`
//! stays joinable through [`TransferExecutor::take_work`] so shutdown can
//! prove the engine exited before answering.
//!
//! **Engine-symbol note (wave-27 parallel build):** this file is the only
//! ak-session compilation unit that references the two frozen engine
//! entries above. If the engine crate has not landed them yet, the
//! crate's compile errors must point at those two references (and nowhere
//! else); the rest of ak-session — including the admission validator and
//! its tests — stays green.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ak_core::error::ErrorCode;
use ak_net::DirectChannel;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::runtime::admission::TaskOpenClaim;
use crate::runtime::error::EngineStartError;
use crate::runtime::executor::{ExecutorWork, ProgressSink, TransferExecutor};
use crate::runtime::real_executor::{stable_code, RealFailure, RealOutcome};
use crate::runtime::transport::{PairingId, PeerFingerprint};
use crate::runtime::types::{TransferDirection, TransferSpec};
use crate::scheduler::key::{TaskHandle, TaskId};
use crate::scheduler::state::{FinishOutcome, TaskPhase};

/// How many finished outcomes stay queryable (per task id, latest wins);
/// mirrors the real-executor bound.
const MAX_REMEMBERED_OUTCOMES: usize = 8;

/// Grace given to the engine when a cancellation races the final commit
/// (the same bound and rationale as the real executor's
/// `FINAL_COMMIT_GRACE`).
const FINAL_COMMIT_GRACE: Duration = Duration::from_millis(250);

/// Running per-task bookkeeping (for `cancel` and `take_work`).
struct RunningTask {
    /// Cooperative cancellation flag (set by `cancel`).
    cancel: watch::Sender<bool>,
    /// The joinable engine-work task (P1-03).
    join: JoinHandle<()>,
}

#[derive(Default)]
struct ChannelState {
    running: HashMap<TaskId, RunningTask>,
    outcomes: HashMap<TaskId, RealOutcome>,
    outcome_order: Vec<TaskId>,
}

/// One-shot on-channel receive executor: wraps exactly one
/// `receive_on_channel_preadmitted` call on one already-accepted,
/// already-handshaked direct channel.
///
/// The daemon constructs one per accepted scheduler-capable connection
/// (`channel_scheduler_handshake` answered `Scheduler { open }`), passes
/// it as the task's `TransferSpec::executor` next to the group key and
/// pairing id the [`AdmissionValidator`](crate::runtime::AdmissionValidator)
/// resolved, and reads [`ChannelReceiveExecutor::outcome`] once the
/// runtime reports the task finished. A second `enqueue` on the same
/// instance fails fast (the one channel was consumed); a send direction is
/// refused.
pub struct ChannelReceiveExecutor {
    /// The accepted channel, until `enqueue` moves it into the work task.
    channel: Mutex<Option<DirectChannel>>,
    /// The receive material for the engine call (destination, conflict
    /// policy, checkpoints, idle budget; `bind`/`authorize` are unused by
    /// the preadmitted entry, exactly as in `receive_on_channel`).
    spec: Mutex<Option<ak_engine::ReceiveSpec>>,
    /// The wire `TaskOpen` the sender opened this task with; handed back
    /// to the engine so the admission reply names the sender's task and
    /// permit.
    open: Mutex<Option<ak_engine::TaskOpenAdmission>>,
    /// Optional progress callback forwarded every engine progress event.
    observer: Option<Arc<dyn Fn(ak_engine::Progress) + Send + Sync>>,
    state: Arc<Mutex<ChannelState>>,
}

impl std::fmt::Debug for ChannelReceiveExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The engine spec and observer are not Debug; report only the
        // facts a log reader needs.
        let armed = self
            .channel
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false);
        f.debug_struct("ChannelReceiveExecutor")
            .field("armed_channel", &armed)
            .field("observer", &self.observer.is_some())
            .finish_non_exhaustive()
    }
}

impl From<&ak_engine::TaskOpenAdmission> for TaskOpenClaim {
    fn from(open: &ak_engine::TaskOpenAdmission) -> Self {
        TaskOpenClaim::new(
            open.job_id(),
            open.permit_id(),
            open.group(),
            open.group_registry(),
            open.pairing_id(),
            open.direction(),
        )
    }
}

impl ChannelReceiveExecutor {
    /// Build the executor for one accepted connection.
    ///
    /// * `channel` — the accepted, scheduler-handshaked direct channel
    ///   (`channel_scheduler_handshake` already returned `Scheduler`);
    /// * `spec` — the engine receive material (destination dir, conflict
    ///   policy, checkpoints, idle timeout; `bind`/`authorize` unused);
    /// * `open` — the engine-decoded `TaskOpen` of this task (the
    ///   admission reply and the per-task staging tree key);
    /// * `observer` — optional progress callback (the daemon's
    ///   NDJSON/line printer rides here).
    pub fn new(
        channel: DirectChannel,
        spec: ak_engine::ReceiveSpec,
        open: ak_engine::TaskOpenAdmission,
        observer: Option<Arc<dyn Fn(ak_engine::Progress) + Send + Sync>>,
    ) -> Self {
        ChannelReceiveExecutor {
            channel: Mutex::new(Some(channel)),
            spec: Mutex::new(Some(spec)),
            open: Mutex::new(Some(open)),
            observer,
            state: Arc::new(Mutex::new(ChannelState::default())),
        }
    }

    /// The wire claim of the held `TaskOpen` (what
    /// [`AdmissionValidator::resolve`](crate::runtime::AdmissionValidator::resolve)
    /// checks), until the executor is consumed by `enqueue`.
    pub fn claim(&self) -> Option<TaskOpenClaim> {
        self.open
            .lock()
            .expect("channel executor lock")
            .as_ref()
            .map(TaskOpenClaim::from)
    }

    /// The verified peer fingerprint of the held channel, until `enqueue`
    /// consumes it — the identity half of the relation check.
    pub fn peer_fingerprint(&self) -> Option<PeerFingerprint> {
        self.channel
            .lock()
            .expect("channel executor lock")
            .as_ref()
            .map(|channel| PeerFingerprint::from_bytes(*channel.peer_fingerprint().as_bytes()))
    }

    /// The pairing id the held `TaskOpen` claims (convenience for building
    /// `PairingPlan::Ready` alongside a resolved group).
    pub fn claimed_pairing_id(&self) -> Option<PairingId> {
        self.claim()
            .map(|claim| PairingId::from_bytes(claim.pairing_id))
    }

    /// The outcome of the task's attempt, once it finished. `None` while
    /// the transfer is running (or the executor was never enqueued).
    pub fn outcome(&self, task: &TaskId) -> Option<RealOutcome> {
        self.state
            .lock()
            .expect("channel executor lock")
            .outcomes
            .get(task)
            .cloned()
    }

    /// Whether the engine call has settled (any outcome recorded).
    pub fn is_settled(&self, task: &TaskId) -> bool {
        self.state
            .lock()
            .expect("channel executor lock")
            .outcomes
            .contains_key(task)
    }

    fn record_outcome(state: &mut ChannelState, task: TaskId, outcome: RealOutcome) {
        if state.outcomes.insert(task, outcome).is_none() {
            state.outcome_order.push(task);
            if state.outcome_order.len() > MAX_REMEMBERED_OUTCOMES {
                let evict = state.outcome_order.remove(0);
                state.outcomes.remove(&evict);
            }
        }
    }
}

impl TransferExecutor for ChannelReceiveExecutor {
    fn enqueue(
        &self,
        task: TaskHandle,
        spec: TransferSpec,
        sink: ProgressSink,
    ) -> Result<(), EngineStartError> {
        if spec.direction != TransferDirection::Receive {
            return Err(EngineStartError::new(
                ErrorCode::Internal,
                "the channel receive executor is receive-only",
            ));
        }
        let id = task.task_id();
        // Consume the one-shot triple under one lock stretch per slot so a
        // racing second enqueue can never observe a half-taken set.
        let channel = self
            .channel
            .lock()
            .expect("channel executor lock")
            .take()
            .ok_or_else(|| {
                EngineStartError::new(
                    ErrorCode::Internal,
                    "this channel executor already ran its one accepted connection",
                )
            })?;
        let engine_spec = self
            .spec
            .lock()
            .expect("channel executor lock")
            .take()
            .ok_or_else(|| {
                EngineStartError::new(
                    ErrorCode::Internal,
                    "this channel executor already ran its one receive spec",
                )
            })?;
        let open = self
            .open
            .lock()
            .expect("channel executor lock")
            .take()
            .ok_or_else(|| {
                EngineStartError::new(
                    ErrorCode::Internal,
                    "this channel executor already ran its one task open",
                )
            })?;

        // Phase events cross from the engine's synchronous progress
        // callback into the work task through an unbounded channel; the
        // only events forwarded today are `Transferring` (on `Connected`)
        // and the persisted-completion latch (`FinalAck`), so the channel
        // cannot grow without bound.
        let (phase_tx, mut phase_rx) = mpsc::unbounded_channel::<TaskPhase>();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        // "The completion is persisted": the engine saves the terminal
        // `completed` checkpoint strictly before emitting
        // `FinalAck { verified: true }`, so this flag is exactly the signal
        // a cancellation must lose against (P1-03).
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
            let mut engine_fut = Box::pin(async move {
                ak_engine::receive_on_channel_preadmitted(channel, engine_spec, open, &progress)
                    .await
            });
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
                            // Persisted completion: let the engine finish
                            // and report the real outcome — never Cancelled
                            // (P1-03).
                            break Some((&mut engine_fut).await);
                        }
                        // Final-commit race grace: the completion may be a
                        // few frame writes from persisting; give it a short
                        // bounded chance before dropping the engine work.
                        match tokio::time::timeout(FINAL_COMMIT_GRACE, &mut engine_fut).await {
                            Ok(outcome) => break Some(outcome),
                            Err(_grace_elapsed) => {
                                if final_ack.load(Ordering::Acquire) {
                                    // The completion persisted *during* the
                                    // grace window (the engine saves its
                                    // terminal `completed` checkpoint and
                                    // emits `FinalAck { verified: true }`
                                    // before its bounded tail-hold return):
                                    // adopt it — a persisted completion is
                                    // never reported as Cancelled, even when
                                    // the cancel fired first.
                                    break Some((&mut engine_fut).await);
                                }
                                break None;
                            }
                        }
                    }
                }
            };
            // Drop the engine BEFORE recording and reporting (P1-03): the
            // accepted channel is closed and staged writes stop here, so by
            // the time the runtime can observe this task's outcome the real
            // engine work is already off the wire. Part files and
            // checkpoints stay behind, resumable exactly like a peer drop.
            drop(engine_fut);
            // Record the outcome in a lexically-scoped block: a std
            // MutexGuard must never sit across an await point, and the
            // block boundary makes that provable for the Send check.
            let failure_code = {
                let mut state = shared_state.lock().expect("channel executor lock");
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
                        // Dropped on cancellation: the work task reports the
                        // synthesized cancellation on the engine's behalf,
                        // strictly after the engine exited.
                        Self::record_outcome(
                            &mut state,
                            id,
                            RealOutcome::Failed(RealFailure {
                                code: ErrorCode::Cancelled,
                                io_kind: None,
                                message: "cancelled: the engine future was dropped".to_owned(),
                            }),
                        );
                        Some(ErrorCode::Cancelled)
                    }
                }
            };
            match failure_code {
                None => {
                    if !transferring_reported {
                        // Defensive: `Connected` is reported by the receive
                        // kernel, but a phase report must never be silently
                        // missing on the success path.
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
                .expect("channel executor lock")
                .running
                .remove(&id);
        });

        self.state
            .lock()
            .expect("channel executor lock")
            .running
            .insert(
                id,
                RunningTask {
                    cancel: cancel_tx,
                    join: engine_task,
                },
            );
        Ok(())
    }

    fn cancel(&self, task: &TaskHandle) {
        // Cooperative stop (P1-03): the work task observes the flag at its
        // current await point, drops the engine work (closing the accepted
        // channel, stopping writes) and only then records and reports its
        // outcome. An already-persisted completion is adopted by the work
        // task itself — never overwritten with Cancelled.
        let state = self.state.lock().expect("channel executor lock");
        if let Some(running) = state.running.get(&task.task_id()) {
            let _ = running.cancel.send(true);
        }
    }

    fn take_work(&self, task: &TaskHandle) -> Option<ExecutorWork> {
        self.state
            .lock()
            .expect("channel executor lock")
            .running
            .remove(&task.task_id())
            .map(|running| ExecutorWork::new(running.join))
    }
}

/// Bound an error message for storage/reporting (same policy as the real
/// executor).
fn bounded(message: String) -> String {
    message.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::actor::{test_command_channel, Command};
    use crate::runtime::admission::{code_group_wire_form, AdmissionValidator, ResolvedAdmission};
    use crate::runtime::executor::ExecutorReport;
    use crate::runtime::types::PairingPlan;
    use crate::scheduler::key::{CodeGroupKey, CodeId, RegistryId};
    use ak_identity::DeviceKeyPair;
    use ak_net::DirectEndpoint;
    use ak_store::{PairingJournal, PairingPhase, PendingPairing};
    use std::net::SocketAddr;

    /// The frozen wave-27 handshake entry, isolated so a signature drift
    /// on the engine side lands on this one call.
    async fn scheduler_handshake(
        channel: &mut DirectChannel,
        spec: &ak_engine::ReceiveSpec,
    ) -> Result<ak_engine::SchedulerHandshake, ak_engine::EngineError> {
        ak_engine::channel_scheduler_handshake(channel, spec).await
    }

    /// Seed one Ready relation (peer = `peer_key`) into a journal under
    /// `id_dir` and return the validator over it.
    fn seeded_validator(
        id_dir: &std::path::Path,
        registry: [u8; 32],
        pairing_id: [u8; 16],
        code_id: [u8; 16],
        peer_key: &[u8; 32],
    ) -> AdmissionValidator {
        let journal = PairingJournal::open(&id_dir.join(crate::pairing::JOURNAL_SUBDIR))
            .expect("journal opens");
        journal
            .record_pending(&PendingPairing {
                pairing_id,
                code_id,
                lookup_digest: [0x44; 32],
                secret_ref: [0x55; 16],
                peer_public_key: Some(*peer_key),
            })
            .expect("pending recorded");
        journal.mark_committed(&pairing_id).expect("committed");
        journal.mark_applied(&pairing_id).expect("applied");
        assert_eq!(
            journal
                .load(&pairing_id)
                .expect("load")
                .expect("present")
                .phase,
            PairingPhase::Applied
        );
        AdmissionValidator::over_journal(journal, RegistryId::from_bytes(registry))
    }

    /// A receive spec for the preadmitted entry (bind/authorize unused).
    fn on_channel_recv_spec(
        out: &std::path::Path,
        identity: ak_identity::tls::TlsBundle,
    ) -> ak_engine::ReceiveSpec {
        let mut spec = ak_engine::ReceiveSpec::new(out.to_owned(), identity);
        spec.idle_timeout = Duration::from_secs(20);
        spec
    }

    /// One loopback receiver fixture: a bound endpoint plus the relation
    /// (validator) the connecting sender will claim membership of. The
    /// fixture must outlive the transfer: its `DirectEndpoint` owns the
    /// accepted connection's transport, and dropping a quinn endpoint
    /// terminates the connections it accepted.
    struct LoopbackFixture {
        tmp: tempfile::TempDir,
        validator: AdmissionValidator,
        receiver_key: DeviceKeyPair,
        endpoint: DirectEndpoint,
        addr: SocketAddr,
        registry: [u8; 32],
        pairing_id: [u8; 16],
        code_id: [u8; 16],
        sender_key: DeviceKeyPair,
    }

    async fn loopback_fixture(pairing_id: [u8; 16], code_id: [u8; 16]) -> LoopbackFixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
            .await
            .expect("receiver binds");
        let addr = endpoint.local_addr().expect("local addr");
        let sender_key = DeviceKeyPair::generate();
        let registry = [0x11; 32];
        let validator = seeded_validator(
            tmp.path(),
            registry,
            pairing_id,
            code_id,
            &sender_key.public_key(),
        );
        LoopbackFixture {
            tmp,
            validator,
            receiver_key,
            endpoint,
            addr,
            registry,
            pairing_id,
            code_id,
            sender_key,
        }
    }

    impl LoopbackFixture {
        fn recv_spec(&self) -> ak_engine::ReceiveSpec {
            on_channel_recv_spec(
                &self.tmp.path().join("out"),
                ak_identity::tls::make_self_signed(&self.receiver_key),
            )
        }

        /// Spawn the sender side of the same relation: a real wire
        /// `TaskOpen` through `send_on_channel`.
        fn spawn_sender(
            &self,
            src: std::path::PathBuf,
            task_id: [u8; 16],
        ) -> tokio::task::JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>> {
            let identity = ak_identity::tls::make_self_signed(&self.sender_key);
            let mut send_spec =
                ak_engine::SendSpec::new(src, self.addr, self.receiver_key.fingerprint(), identity);
            send_spec.idle_timeout = Duration::from_secs(20);
            let admission = ak_engine::AdmissionCtx::new(
                task_id,
                [0x5C; 16],
                code_group_wire_form(&self.code_id),
                self.registry,
                self.pairing_id,
            )
            .expect("admission ctx");
            tokio::spawn(async move {
                let dialer = DirectEndpoint::bind(
                    SocketAddr::from(([127, 0, 0, 1], 0)),
                    &send_spec.identity,
                )
                .await
                .expect("dialer binds");
                let channel = dialer
                    .connect(send_spec.peer, &send_spec.pinned)
                    .await
                    .expect("dialer connects");
                ak_engine::send_on_channel(channel, send_spec, admission, &|_| {}).await
            })
        }

        /// Daemon-side accept + handshake + relation validation: returns
        /// the armed executor and the resolved admission. Borrows `self`
        /// on purpose (see the fixture docs).
        async fn accept_and_admit(
            &self,
        ) -> (
            Arc<ChannelReceiveExecutor>,
            ResolvedAdmission,
            PeerFingerprint,
        ) {
            self.accept_and_admit_observed(None).await
        }

        /// [`LoopbackFixture::accept_and_admit`] with a progress observer
        /// on the executor (the cancel test gates its cancel on real chunk
        /// progress).
        async fn accept_and_admit_observed(
            &self,
            observer: Option<Arc<dyn Fn(ak_engine::Progress) + Send + Sync>>,
        ) -> (
            Arc<ChannelReceiveExecutor>,
            ResolvedAdmission,
            PeerFingerprint,
        ) {
            let spec = self.recv_spec();
            let mut channel = self.endpoint.accept(&|_| true).await.expect("accept");
            let open = match scheduler_handshake(&mut channel, &spec)
                .await
                .expect("handshake")
            {
                ak_engine::SchedulerHandshake::Scheduler { open } => open,
                ak_engine::SchedulerHandshake::Legacy { .. } => {
                    panic!("a scheduler-capable sender must not degrade to legacy")
                }
            };
            let executor = Arc::new(ChannelReceiveExecutor::new(channel, spec, open, observer));
            let claim = executor.claim().expect("claim before enqueue");
            let peer = executor.peer_fingerprint().expect("peer before enqueue");
            let resolved = self
                .validator
                .resolve(&peer, &claim)
                .expect("relation admits");
            (executor, resolved, peer)
        }
    }

    /// End-to-end on loopback: a real scheduler-capable sender
    /// (`send_on_channel` with a real wire `TaskOpen` built from the same
    /// relation the receiver validates) against the on-channel executor
    /// driven through the `TransferExecutor` trait. Asserts the resolved
    /// group, the phase walk, the terminal report and the committed bytes.
    #[tokio::test]
    async fn channel_executor_completes_an_admitted_loopback_receive() {
        let fixture = loopback_fixture([0xA1; 16], [0xC3; 16]).await;
        let out = fixture.tmp.path().join("out");
        let src = fixture.tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 199) as u8).collect();
        std::fs::write(src.join("payload.bin"), &payload).unwrap();

        // Daemon side: accept, handshake, validate, build the executor.
        // The sender dials concurrently; the accept is already pending.
        let sender = fixture.spawn_sender(src.clone(), [0x42; 16]);
        let (executor, resolved, _peer) = fixture.accept_and_admit().await;
        assert_eq!(
            resolved.group,
            CodeGroupKey::new(
                RegistryId::from_bytes([0x11; 32]),
                CodeId::from_bytes([0xC3; 16])
            ),
            "the group comes from the relation, not the claim bytes"
        );
        assert_eq!(resolved.pairing_id.as_bytes(), &[0xA1; 16]);

        // Drive the executor through the trait on a command channel
        // standing in for the actor.
        let (cmd_tx, mut cmd_rx) = test_command_channel();
        let handle = TaskHandle::new(TaskId::from_counter(7));
        executor
            .enqueue(
                handle,
                TransferSpec {
                    group_key: resolved.group,
                    pairing: PairingPlan::Ready {
                        pairing_id: resolved.pairing_id,
                    },
                    executor: executor.clone() as Arc<dyn TransferExecutor>,
                    direction: TransferDirection::Receive,
                    label: "admitted-loopback".to_owned(),
                    task_id: None,
                },
                ProgressSink::new(cmd_tx),
            )
            .expect("enqueue starts the engine task");

        let send_report = tokio::time::timeout(Duration::from_secs(30), sender)
            .await
            .expect("send settles")
            .expect("sender task")
            .expect("send succeeds");
        assert_eq!(send_report.files, 1);
        assert_eq!(send_report.bytes, payload.len() as u64);

        // The executor reported the full contract §9 walk and Completed.
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
        match executor.outcome(&handle.task_id()) {
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
        // (the one channel was consumed).
        let (cmd_tx2, _cmd_rx2) = test_command_channel();
        let err = executor.enqueue(
            handle,
            TransferSpec {
                group_key: resolved.group,
                pairing: PairingPlan::Ready {
                    pairing_id: resolved.pairing_id,
                },
                executor: executor.clone() as Arc<dyn TransferExecutor>,
                direction: TransferDirection::Receive,
                label: String::new(),
                task_id: None,
            },
            ProgressSink::new(cmd_tx2),
        );
        assert!(err.is_err(), "the one-shot channel is consumed");
    }

    /// Cancellation semantics (P1-03 discipline): cancelling a running
    /// admitted receive drops the engine work first — the accepted channel
    /// closes, which the real sender observes as a hard failure — and only
    /// then records and reports `Failed { CANCELLED }`. The cancel is
    /// gated on observed chunk progress (second chunk of a 48-chunk job),
    /// so it demonstrably lands mid-transfer with the final commit far
    /// outside the 250 ms grace window.
    #[tokio::test]
    async fn cancel_running_channel_task_drops_the_engine_first() {
        let fixture = loopback_fixture([0xB2; 16], [0xD4; 16]).await;
        let src = fixture.tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // 48 MiB = 48 default chunks; cancel after chunk 2 leaves ~46 MiB
        // in flight — orders of magnitude beyond the grace window even on
        // a fast loopback.
        let payload: Vec<u8> = (0..48_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("payload.bin"), &payload).unwrap();

        // Cancel exactly when the second chunk arrived: real bytes moved,
        // the bulk of the job is still ahead.
        let chunks_seen = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let observer = {
            let chunks_seen = Arc::clone(&chunks_seen);
            Arc::new(move |progress: ak_engine::Progress| {
                if matches!(progress, ak_engine::Progress::ChunkReceived { .. }) {
                    chunks_seen.fetch_add(1, Ordering::Relaxed);
                }
            }) as Arc<dyn Fn(ak_engine::Progress) + Send + Sync>
        };
        let sender = fixture.spawn_sender(src.clone(), [0x42; 16]);
        let (executor, resolved, _peer) = fixture.accept_and_admit_observed(Some(observer)).await;

        let (cmd_tx, mut cmd_rx) = test_command_channel();
        let handle = TaskHandle::new(TaskId::from_counter(11));
        executor
            .enqueue(
                handle,
                TransferSpec {
                    group_key: resolved.group,
                    pairing: PairingPlan::Ready {
                        pairing_id: resolved.pairing_id,
                    },
                    executor: executor.clone() as Arc<dyn TransferExecutor>,
                    direction: TransferDirection::Receive,
                    label: "cancel-mid-transfer".to_owned(),
                    task_id: None,
                },
                ProgressSink::new(cmd_tx),
            )
            .expect("enqueue starts the engine task");

        // Wait for the Transferring report (Connected: admission granted,
        // offer accepted), then for the second chunk to arrive.
        let mut transferring = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while (!transferring || chunks_seen.load(Ordering::Relaxed) < 2)
            && std::time::Instant::now() < deadline
        {
            assert!(
                !executor.is_settled(&handle.task_id()),
                "the transfer must still be running when the cancel lands"
            );
            match tokio::time::timeout(Duration::from_millis(50), cmd_rx.recv()).await {
                Ok(Some(Command::Report(ExecutorReport::Phase {
                    phase: TaskPhase::Transferring,
                    ..
                }))) => transferring = true,
                Ok(Some(Command::Report(ExecutorReport::Finished { .. }))) => {
                    panic!("the transfer finished before the cancel could land")
                }
                Ok(Some(_)) | Ok(None) | Err(_) => {}
            }
        }
        assert!(transferring, "the transfer started");
        assert!(
            chunks_seen.load(Ordering::Relaxed) >= 2,
            "real file bytes moved before the cancel"
        );

        // Stop and take the work handle in one synchronous stretch.
        executor.cancel(&handle);
        let work = executor
            .take_work(&handle)
            .expect("the running engine work is joinable");
        work.join_within(Duration::from_secs(5), Duration::from_secs(5))
            .await;

        // Joined ⇒ the engine exited; the outcome is recorded Cancelled
        // and the report follows strictly after the exit.
        assert!(executor.is_settled(&handle.task_id()));
        match executor.outcome(&handle.task_id()) {
            Some(RealOutcome::Failed(failure)) => {
                assert_eq!(failure.code, ErrorCode::Cancelled);
            }
            other => panic!("expected a cancelled outcome, got {other:?}"),
        }
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

        // The dropped engine closed the accepted channel: the real sender
        // cannot complete (never a successful Report on a cancelled
        // receive).
        let sender_outcome = tokio::time::timeout(Duration::from_secs(20), sender)
            .await
            .expect("sender settles once the channel dropped")
            .expect("the sender task exits cleanly");
        assert!(
            sender_outcome.is_err(),
            "the sender must fail after the receiver dropped the engine work, \
             but it reported {sender_outcome:?}"
        );
    }

    /// The executor refuses a send-direction spec (it exists for daemon
    /// inbound receives only). The refusal fires before any engine call,
    /// so the connected sender never gets an admission answer — the test
    /// aborts it instead of waiting out the idle budget.
    #[tokio::test]
    async fn send_direction_is_refused() {
        let fixture = loopback_fixture([0xE5; 16], [0xF6; 16]).await;
        // A real source must exist: the sender scans it before the Hello,
        // and the handshake must complete so the executor is armed with a
        // genuine wire TaskOpen.
        let src = fixture.tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f.bin"), b"refuse-me").unwrap();
        let sender = fixture.spawn_sender(src, [0x42; 16]);
        let (executor, resolved, _peer) = fixture.accept_and_admit().await;
        let (cmd_tx, _cmd_rx) = test_command_channel();
        let err = executor.enqueue(
            TaskHandle::new(TaskId::from_counter(3)),
            TransferSpec {
                group_key: resolved.group,
                pairing: PairingPlan::Ready {
                    pairing_id: resolved.pairing_id,
                },
                executor: executor.clone() as Arc<dyn TransferExecutor>,
                direction: TransferDirection::Send,
                label: String::new(),
                task_id: None,
            },
            ProgressSink::new(cmd_tx),
        );
        assert!(err.is_err(), "a send-direction spec is refused");
        // The executor stays armed (nothing was consumed): its claim is
        // still readable for the caller's diagnostics.
        assert!(executor.claim().is_some());
        sender.abort();
    }
}
