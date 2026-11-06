//! Runtime data types: transfer specs, configuration, snapshots, events.
use ak_core::error::ErrorCode;
use std::fmt;
use std::sync::Arc;

use crate::runtime::clock::RuntimeClock;
use crate::runtime::error::SessionError;
use crate::runtime::executor::TransferExecutor;
use crate::runtime::transport::{PairingId, PairingTransport, PeerFingerprint, RedeemPayload};
use crate::scheduler::config::SchedulerPolicy;
use crate::scheduler::key::{CodeGroupKey, RegistryId, TaskId};
use crate::scheduler::state::{SchedulerEvent, TaskState};

/// Frozen broadcast bound for runtime events (contract §7: a slow UI must
/// never grow memory without bound; the oldest events are dropped and the
/// receiver observes a `Lagged` warning).
pub const EVENT_CAPACITY: usize = 1024;

/// How many terminal task records the runtime keeps resident (P2-06).
///
/// A resident daemon (one receive loop iteration per executor) used to
/// retain every task's working spec — executor handle included — forever,
/// so memory and the snapshot scan grew with the number of finished,
/// refused, timed-out or errored receives. Terminal tasks now release
/// their working spec/executor immediately and keep only a small slim
/// record (terminal state, error summary, timestamps, identity and a
/// bounded label — a few hundred bytes each). 256 keeps a full page of
/// recent history for snapshot/UI recall at well under a megabyte while
/// bounding the resident set; anything older is dropped, because durable
/// history belongs to persistence, not to memory. Retryable tasks are
/// never terminal and always keep their full working spec.
pub const MAX_TERMINAL_HISTORY: usize = 256;

/// Stable task instance identity (contract §11 `transfer_id`).
pub type TransferId = TaskId;

/// Bound and sanitize a label coming from a submission (96 chars: enough
/// for the `inbound-` prefix plus a full 64-hex-char peer fingerprint).
fn sanitize_label(label: &str) -> String {
    let mut out: String = label
        .chars()
        .take(96)
        .map(|c| match c as u32 {
            0x00..=0x1F | 0x7F..=0x9F => '?',
            _ => c,
        })
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Which side of the transfer this device is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferDirection {
    /// This device sends to a paired peer.
    Send,
    /// This device receives from a paired peer (inbound submission by the
    /// [`RuntimeListener`](crate::runtime::RuntimeListener)).
    Receive,
}

/// How the task resolves its pairing relation before opening the data
/// session.
#[derive(Debug, Clone)]
pub enum PairingPlan {
    /// The pairing relation is already `Ready`; go straight to the direct
    /// path.
    Ready {
        /// The established relation.
        pairing_id: PairingId,
    },
    /// Redeem a code and finish the pairing transcript first.
    Redeem {
        /// The fixed redeem request (A3 `InviteRedeem` shape).
        request: RedeemPayload,
    },
    /// No signaling-plane round-trip at all (wave 24): the task runs a
    /// local bind/accept (the daemon's resident receive loop) and goes
    /// straight from admission to its executor. The driver reports the
    /// `WaitingDirect`/`AwaitingAdmission` phases and skips
    /// [`PairingTransport::open_session`](crate::runtime::PairingTransport::open_session):
    /// a resident listener has no signaling session to open toward the
    /// still-unknown connecting peer.
    Local,
}

/// One submitted transfer task.
///
/// Per the B6 brief the spec carries the executor handle: each task names
/// the [`TransferExecutor`] that will run it once admitted, so the runtime
/// stays engine-agnostic (wave 21 binds the real `ak-engine` adapter).
#[derive(Clone)]
pub struct TransferSpec {
    /// Scheduling group (`(registry_id, code_id)`; one code one owner).
    pub group_key: CodeGroupKey,
    /// Pairing resolution plan.
    pub pairing: PairingPlan,
    /// Executor that runs the task once admitted.
    pub executor: Arc<dyn TransferExecutor>,
    /// Send or receive side.
    pub direction: TransferDirection,
    /// Bounded display label (sanitized on submission).
    pub label: String,
    /// Explicit task identity; `None` lets the runtime assign one.
    pub task_id: Option<TaskId>,
}

impl TransferSpec {
    /// Build a send-side spec with an already-established pairing.
    pub fn send_ready(
        group_key: CodeGroupKey,
        pairing_id: PairingId,
        executor: Arc<dyn TransferExecutor>,
    ) -> Self {
        TransferSpec {
            group_key,
            pairing: PairingPlan::Ready { pairing_id },
            executor,
            direction: TransferDirection::Send,
            label: String::new(),
            task_id: None,
        }
    }

    /// Sanitized copy of the label (as stored by the runtime).
    pub fn sanitized_label(&self) -> String {
        sanitize_label(&self.label)
    }
}

impl fmt::Debug for TransferSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransferSpec")
            .field("group_key", &self.group_key)
            .field("pairing", &self.pairing)
            .field("direction", &self.direction)
            .field("label", &self.label)
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

/// Failure-retry budget and backoff schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts allowed per task (the first try plus retries);
    /// exhausting it parks the task until a user `retry`.
    pub max_attempts: u32,
    /// Backoff before the first retry.
    pub initial_backoff_ms: u64,
    /// Backoff ceiling.
    pub max_backoff_ms: u64,
    /// Exponential factor between consecutive backoffs.
    pub multiplier: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 8000,
            multiplier: 2,
        }
    }
}

impl RetryPolicy {
    /// Backoff after the `failure_count`-th failure (1-based),
    /// exponential and capped.
    pub fn backoff_ms(&self, failure_count: u32) -> u64 {
        let mut delay = self.initial_backoff_ms;
        for _ in 1..failure_count {
            delay = delay.saturating_mul(u64::from(self.multiplier.max(1)));
            if delay >= self.max_backoff_ms {
                return self.max_backoff_ms;
            }
        }
        delay.min(self.max_backoff_ms)
    }
}

/// Configuration for [`SessionRuntime::start`](crate::runtime::SessionRuntime).
pub struct RuntimeConfig {
    /// Initial scheduling policy (may be switched later via
    /// `SessionHandle::set_policy`).
    pub policy: SchedulerPolicy,
    /// Injected clock (Fake in tests, `RealClock` in production).
    pub clock: Arc<dyn RuntimeClock>,
    /// Injected signaling/pairing transport.
    pub transport: Arc<dyn PairingTransport>,
    /// Executor used for inbound (listener-submitted) tasks.
    pub default_executor: Arc<dyn TransferExecutor>,
    /// This device's registry identity (groups inbound code relations).
    pub registry_id: RegistryId,
    /// Broadcast bound for runtime events (default/maximum 1024).
    pub event_capacity: usize,
    /// Retry budget for failed tasks.
    pub retry: RetryPolicy,
    /// How long shutdown waits for in-flight tasks before forcing
    /// cancellation.
    pub shutdown_timeout_ms: u64,
    /// Whether to run the resident pairing listener.
    pub listener_enabled: bool,
}

impl RuntimeConfig {
    /// Build a config with the contract defaults (1024-event broadcast,
    /// 3-attempt retry budget, 5 s shutdown timeout, listener on).
    pub fn new(
        policy: SchedulerPolicy,
        clock: Arc<dyn RuntimeClock>,
        transport: Arc<dyn PairingTransport>,
        default_executor: Arc<dyn TransferExecutor>,
        registry_id: RegistryId,
    ) -> Self {
        RuntimeConfig {
            policy,
            clock,
            transport,
            default_executor,
            registry_id,
            event_capacity: EVENT_CAPACITY,
            retry: RetryPolicy::default(),
            shutdown_timeout_ms: 5000,
            listener_enabled: true,
        }
    }

    /// Clamp the event capacity into 1..=1024.
    pub(crate) fn clamped_event_capacity(&self) -> usize {
        self.event_capacity.clamp(1, EVENT_CAPACITY)
    }
}

/// Why a task sits parked outside the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParkReason {
    /// The retry budget was exhausted; only a user `retry` restarts it.
    RetryExhausted,
    /// A retry wanted to re-queue but the local queue was full; the actor
    /// requeues automatically once capacity frees.
    QueueFull,
}

/// Result of [`SessionHandle::cancel`](crate::runtime::SessionHandle::cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReceipt {
    /// The task sat in the queue; removed without ever connecting
    /// (contract §10).
    RemovedQueued,
    /// The task was admitted; the executor was told to stop and the task
    /// reaches terminal `Cancelled` when it reports back.
    RunningCancellationRequested,
}

/// How [`SessionHandle::shutdown`](crate::runtime::SessionHandle::shutdown)
/// treats in-flight work (contract §10 "stop listener / close runtime").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownMode {
    /// Let in-flight tasks finish naturally (within the timeout), then
    /// cancel everything left.
    Drain,
    /// Cancel in-flight tasks immediately.
    Cancel,
}

/// Aggregate result of a shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownSummary {
    /// Requested mode.
    pub mode: ShutdownMode,
    /// Tasks that reached `Completed`.
    pub completed: u64,
    /// Tasks that reached `Cancelled` (including the shutdown's own
    /// cancellation of residuals).
    pub cancelled: u64,
    /// Tasks still parked when the runtime stopped.
    pub parked: u64,
    /// Whether the shutdown timeout forced the completion.
    pub timed_out: bool,
    /// Whether the resident listener was stopped and joined.
    pub listener_stopped: bool,
}

/// What a snapshot query selects.
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotQuery {
    /// Restrict to one task (`None` = all).
    pub task: Option<TaskId>,
    /// Include terminal tasks in the listing.
    pub include_terminal: bool,
}

impl SnapshotQuery {
    /// Everything, terminal included.
    pub fn all() -> Self {
        SnapshotQuery {
            task: None,
            include_terminal: true,
        }
    }

    /// One task, terminal included.
    pub fn one(task: TaskId) -> Self {
        SnapshotQuery {
            task: Some(task),
            include_terminal: true,
        }
    }
}

/// Runtime view of one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    /// Task identity.
    pub task: TransferId,
    /// Scheduling group.
    pub group: CodeGroupKey,
    /// Contract §9 state.
    pub state: TaskState,
    /// Send or receive.
    pub direction: TransferDirection,
    /// Bounded display label.
    pub label: String,
    /// Attempts started so far (first try + retries).
    pub attempts: u32,
    /// Last stable error, if any.
    pub last_error: Option<ErrorCode>,
    /// Cancellation was requested and is being finalized.
    pub cancel_requested: bool,
    /// Why the task is parked, when it is.
    pub parked: Option<ParkReason>,
    /// When the task reached its terminal state (runtime clock, `None`
    /// while it can still run). Kept by the bounded terminal history
    /// (P2-06) so recent finished tasks stay time-addressable.
    pub finished_at_ms: Option<u64>,
}

/// Resident-listener counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListenerView {
    /// Whether the listener was enabled for this runtime.
    pub enabled: bool,
    /// Whether the listener loop is currently running.
    pub running: bool,
    /// Inbound tasks accepted into the scheduler.
    pub inbound_submitted: u64,
    /// Inbound tasks refused with `QUEUE_FULL`.
    pub inbound_rejected_queue_full: u64,
}

/// Point-in-time view of the whole runtime (contract §4 `snapshot`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    /// Current policy.
    pub policy: SchedulerPolicy,
    /// Monotonic policy revision.
    pub policy_revision: u64,
    /// Whether a stricter policy is converging (contract §8 draining).
    pub draining: bool,
    /// Admitted (permit-holding) tasks.
    pub active_tasks: u32,
    /// Queued tasks.
    pub queued_tasks: usize,
    /// Live permits (permit-leak check: equals `active_tasks`).
    pub live_permits: usize,
    /// Whether a shutdown is in progress.
    pub shutting_down: bool,
    /// Per-task views (sorted by task id).
    pub tasks: Vec<TaskView>,
    /// Listener counters.
    pub listener: ListenerView,
}

/// One observable runtime event on the broadcast stream.
///
/// Scheduler transitions are wrapped from [`SchedulerEvent`]; the runtime
/// adds its own orchestration events. The channel is bounded
/// ([`EVENT_CAPACITY`]); a slow consumer misses the oldest events and is
/// told through `broadcast::RecvError::Lagged` (contract §7).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// A scheduler-domain event (task queued/admitted/started/finished,
    /// policy changes, draining).
    Scheduler(SchedulerEvent),
    /// A failed task will retry after a backoff.
    BackoffScheduled {
        /// The task.
        task: TransferId,
        /// The attempt that just failed.
        attempt: u32,
        /// When the retry fires (clock ms).
        until_ms: u64,
    },
    /// A backed-off task re-entered the queue for its next attempt.
    Retrying {
        /// The task.
        task: TransferId,
        /// The attempt about to start (1-based).
        attempt: u32,
    },
    /// A task was parked (retry budget exhausted or queue full on retry).
    TaskParked {
        /// The task.
        task: TransferId,
        /// Why.
        reason: ParkReason,
    },
    /// The listener accepted an inbound submission from a paired peer.
    InboundSubmitted {
        /// The task.
        task: TransferId,
    },
    /// The listener's inbound submission was refused.
    InboundRejected {
        /// The refusal reason (typically `QUEUE_FULL`).
        reason: SessionError,
    },
    /// The signaling connection dropped.
    TransportDisconnected,
    /// The signaling connection is back; `replayed` pairing events follow.
    TransportReconnected {
        /// Number of replayed events following this marker.
        replayed: u16,
    },
    /// A redeemer answered one of this device's codes.
    RedeemOffered {
        /// The code being redeemed.
        code_id: crate::scheduler::key::CodeId,
        /// The pairing transaction.
        pairing_id: PairingId,
        /// The redeeming peer.
        peer: PeerFingerprint,
    },
    /// A pairing transaction aborted.
    PairAborted {
        /// The aborted transaction.
        pairing_id: PairingId,
    },
    /// A shutdown began (no new admissions; listener stopping).
    ShutdownStarted {
        /// The chosen mode.
        mode: ShutdownMode,
    },
    /// The shutdown completed.
    ShutdownComplete {
        /// Tasks that reached `Completed`.
        completed: u64,
        /// Tasks that reached `Cancelled`.
        cancelled: u64,
        /// Whether the timeout forced completion.
        timed_out: bool,
    },
}
