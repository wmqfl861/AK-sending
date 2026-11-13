//! Public runtime API (contract §4, seven signatures):
//!
//! ```text
//! SessionRuntime::start(config)      -> Result<SessionHandle, SessionError>
//! SessionHandle::enqueue(spec)       -> Result<TransferId,   SessionError>
//! SessionHandle::set_policy(policy)  -> Result<PolicySwitch, SessionError>
//! SessionHandle::cancel(transfer_id) -> Result<CancelReceipt, SessionError>
//! SessionHandle::retry(transfer_id)  -> Result<(),           SessionError>
//! SessionHandle::snapshot(query)     -> Result<SessionSnapshot, SessionError>
//! SessionHandle::shutdown(mode)      -> Result<ShutdownSummary, SessionError>
//! ```
//!
//! The methods are `async` (commands round-trip the scheduler actor);
//! wave-21 FFI/IPC wrappers bridge them to the frozen C ABI/NDAPI shapes.
//! `set_policy` returns the full [`PolicySwitch`] (revision + draining +
//! backfill count — the contract's `PolicyRevision` plus the facts the UI
//! must show); `cancel`/`shutdown` return the receipt/summary the B6
//! brief requires.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::runtime::actor::{Command, SchedulerActor};
use crate::runtime::error::SessionError;
use crate::runtime::listener::{ListenerStats, RuntimeListener};
use crate::runtime::types::{
    CancelReceipt, RuntimeConfig, SessionEvent, SessionSnapshot, ShutdownMode, ShutdownSummary,
    SnapshotQuery, TransferId, TransferSpec,
};
use crate::scheduler::config::SchedulerPolicy;
use crate::scheduler::core::PolicySwitch;

/// Bounded capacity of the actor command channel.
const COMMAND_CHANNEL_CAPACITY: usize = 256;
/// Real-time cap for joining the actor/listener after a shutdown reply.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Handle to a running session runtime (cloneable; every clone talks to
/// the same scheduler actor).
#[derive(Debug)]
pub struct SessionHandle {
    cmd: mpsc::Sender<Command>,
    events: broadcast::Sender<SessionEvent>,
    listener_stop: watch::Sender<bool>,
    listener: Mutex<Option<RuntimeListener>>,
    actor: Mutex<Option<JoinHandle<()>>>,
    listener_stats: Arc<ListenerStats>,
}

impl Clone for SessionHandle {
    fn clone(&self) -> Self {
        SessionHandle {
            cmd: self.cmd.clone(),
            events: self.events.clone(),
            listener_stop: self.listener_stop.clone(),
            listener: Mutex::new(None),
            actor: Mutex::new(None),
            listener_stats: Arc::clone(&self.listener_stats),
        }
    }
}

impl SessionHandle {
    /// Subscribe to the bounded runtime event stream (capacity 1024).
    ///
    /// A slow consumer misses the oldest events and observes
    /// `broadcast::RecvError::Lagged(n)` — the contract §7 backpressure
    /// warning; the actor is never blocked by subscribers.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// Submit a transfer task; returns its stable transfer id.
    ///
    /// `QUEUE_FULL` is answered synchronously: nothing was scanned, no
    /// connection was created (contract §7).
    pub async fn enqueue(&self, spec: TransferSpec) -> Result<TransferId, SessionError> {
        self.request(|reply| Command::Submit { spec, reply })
            .await?
    }

    /// Switch the scheduling policy at runtime (contract §8).
    pub async fn set_policy(&self, policy: SchedulerPolicy) -> Result<PolicySwitch, SessionError> {
        self.request(|reply| Command::SetPolicy { policy, reply })
            .await?
    }

    /// Cancel a task: queued tasks are removed without ever connecting;
    /// running tasks get an executor cancellation and finalize to
    /// `Cancelled` when it reports back (contract §10).
    pub async fn cancel(&self, task: TransferId) -> Result<CancelReceipt, SessionError> {
        self.request(|reply| Command::Cancel { task, reply })
            .await?
    }

    /// Restart a parked task (retry budget exhausted / queue-full park).
    pub async fn retry(&self, task: TransferId) -> Result<(), SessionError> {
        self.request(|reply| Command::Retry { task, reply }).await?
    }

    /// Read a runtime snapshot.
    pub async fn snapshot(&self, query: SnapshotQuery) -> Result<SessionSnapshot, SessionError> {
        self.request(|reply| Command::Snapshot { query, reply })
            .await
    }

    /// Shut the runtime down (stop new admissions, wait in-flight with a
    /// timeout, cancel residuals) and join the actor and the listener.
    pub async fn shutdown(&self, mode: ShutdownMode) -> Result<ShutdownSummary, SessionError> {
        let summary = self
            .request(|reply| Command::Shutdown { mode, reply })
            .await??;
        let _ = self.listener_stop.send(true);
        // Take the join handles out of their mutexes before awaiting, so
        // no lock guard is ever held across an await point.
        let actor = self.actor.lock().expect("handle lock").take();
        if let Some(actor) = actor {
            let _ = tokio::time::timeout(JOIN_TIMEOUT, actor).await;
        }
        let listener = self.listener.lock().expect("handle lock").take();
        if let Some(mut listener) = listener {
            listener.stop();
            let _ = tokio::time::timeout(JOIN_TIMEOUT, listener.join()).await;
        }
        Ok(summary)
    }

    /// Resident-listener counters without a full snapshot.
    pub fn listener_stats(&self) -> crate::runtime::types::ListenerView {
        self.listener_stats.view(true)
    }

    async fn request<T>(
        &self,
        make_command: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, SessionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd
            .send(make_command(reply_tx))
            .await
            .map_err(|_| SessionError::RuntimeStopped)?;
        reply_rx.await.map_err(|_| SessionError::RuntimeStopped)
    }
}

/// Entry point: spawn a session runtime from injected dependencies.
pub struct SessionRuntime;

impl SessionRuntime {
    /// Start the scheduler actor (and, when configured, the resident
    /// pairing listener) under the current tokio runtime.
    pub fn start(mut config: RuntimeConfig) -> Result<SessionHandle, SessionError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(SessionError::NotInRuntime);
        }
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(COMMAND_CHANNEL_CAPACITY);
        let (events_tx, _events_rx) =
            broadcast::channel::<SessionEvent>(config.clamped_event_capacity());
        let (listener_stop, listener_stop_rx) = watch::channel(false);
        let listener_stats = Arc::new(ListenerStats::default());

        let actor = SchedulerActor::new(
            crate::scheduler::core::SchedulerCore::new(config.policy),
            Arc::clone(&config.clock),
            Arc::clone(&config.transport),
            cmd_tx.clone(),
            events_tx.clone(),
            listener_stop.clone(),
            Arc::clone(&listener_stats),
            config.listener_enabled,
            config.retry,
            config.shutdown_timeout_ms,
            config.on_attempt_settled.take(),
        );
        let actor_join = tokio::spawn(actor.run(cmd_rx));

        let listener = if config.listener_enabled {
            Some(RuntimeListener::spawn(
                Arc::clone(&config.transport),
                cmd_tx.clone(),
                events_tx.clone(),
                Arc::clone(&config.default_executor),
                Arc::clone(&listener_stats),
                listener_stop.clone(),
            ))
        } else {
            drop(listener_stop_rx);
            None
        };

        Ok(SessionHandle {
            cmd: cmd_tx,
            events: events_tx,
            listener_stop,
            listener: Mutex::new(listener),
            actor: Mutex::new(Some(actor_join)),
            listener_stats,
        })
    }
}
