//! Resident pairing listener (contract §4 "常驻监听", B6).
//!
//! One loop consumes [`PairingEvent`]s from the injected transport and,
//! for every completed pairing ([`PairingEvent::PairReady`]), submits an
//! inbound receive task into the **same scheduler** the local submissions
//! use — the pairing relation legitimizes transfers from that peer, so
//! the group's code policy applies to both directions (contract §2).
//!
//! The listener has an explicit stop signal and a joinable handle (the
//! B6 validation gate: stop + join, no detached accept). Inbound
//! submissions refused with `QUEUE_FULL` are counted and surfaced as
//! events; they never block or kill the loop (contract §7).
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::runtime::actor::Command;
use crate::runtime::executor::TransferExecutor;
use crate::runtime::transport::{PairingEvent, PairingTransport};
use crate::runtime::types::{
    ListenerView, PairingPlan, SessionEvent, TransferDirection, TransferSpec,
};
use crate::scheduler::key::CodeGroupKey;

/// Shared listener counters (read by actor snapshots).
#[derive(Debug, Default)]
pub struct ListenerStats {
    running: AtomicBool,
    inbound_submitted: AtomicU64,
    inbound_rejected_queue_full: AtomicU64,
}

impl ListenerStats {
    /// Snapshot the counters.
    pub fn view(&self, enabled: bool) -> ListenerView {
        ListenerView {
            enabled,
            running: self.running.load(Ordering::Relaxed),
            inbound_submitted: self.inbound_submitted.load(Ordering::Relaxed),
            inbound_rejected_queue_full: self.inbound_rejected_queue_full.load(Ordering::Relaxed),
        }
    }
}

/// Handle to the resident listener: explicit stop + join.
#[derive(Debug)]
pub struct RuntimeListener {
    join: Option<JoinHandle<()>>,
    stop: watch::Sender<bool>,
    stats: Arc<ListenerStats>,
}

impl RuntimeListener {
    /// Signal the loop to stop (idempotent).
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// Whether the loop is still running.
    pub fn is_running(&self) -> bool {
        self.stats.running.load(Ordering::Relaxed)
    }

    /// Counter snapshot.
    pub fn stats(&self) -> ListenerView {
        self.stats.view(true)
    }

    /// Join the loop (waits for the current event to be processed).
    pub async fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }

    /// Spawn the listener loop over the injected transport.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        transport: Arc<dyn PairingTransport>,
        cmd: mpsc::Sender<Command>,
        events: broadcast::Sender<SessionEvent>,
        default_executor: Arc<dyn TransferExecutor>,
        stats: Arc<ListenerStats>,
        stop: watch::Sender<bool>,
    ) -> Self {
        let stop_rx = stop.subscribe();
        let join = tokio::spawn(run_listener(
            transport,
            cmd,
            events,
            default_executor,
            stats.clone(),
            stop_rx,
        ));
        RuntimeListener {
            join: Some(join),
            stop,
            stats,
        }
    }
}

/// The listener loop body (see the module docs).
async fn run_listener(
    transport: Arc<dyn PairingTransport>,
    cmd: mpsc::Sender<Command>,
    events: broadcast::Sender<SessionEvent>,
    default_executor: Arc<dyn TransferExecutor>,
    stats: Arc<ListenerStats>,
    mut stop: watch::Receiver<bool>,
) {
    stats.running.store(true, Ordering::Relaxed);
    loop {
        if *stop.borrow() {
            break;
        }
        let event = tokio::select! {
            changed = stop.changed() => {
                if changed.is_ok() && *stop.borrow() {
                    break;
                }
                continue;
            }
            event = transport.next_pairing_event() => event,
        };
        let event = match event {
            Ok(event) => event,
            // Transport closed: nothing more can arrive; leave the loop
            // (stop+join contract keeps this observable).
            Err(_) => break,
        };
        match event {
            PairingEvent::PairReady {
                registry_id,
                code_id,
                pairing_id,
                peer,
            } => {
                // Newly paired peer: submit an inbound receive task into
                // the same scheduler (default routing, contract §2).
                let spec = TransferSpec {
                    group_key: CodeGroupKey::new(registry_id, code_id),
                    pairing: PairingPlan::Ready { pairing_id },
                    executor: default_executor.clone(),
                    direction: TransferDirection::Receive,
                    label: format!("inbound-{}", peer.to_hex()),
                    task_id: None,
                };
                let (reply_tx, reply_rx) = oneshot::channel();
                if cmd
                    .send(Command::Submit {
                        spec,
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                match reply_rx.await {
                    Ok(Ok(task)) => {
                        stats.inbound_submitted.fetch_add(1, Ordering::Relaxed);
                        let _ = events.send(SessionEvent::InboundSubmitted { task });
                    }
                    Ok(Err(reason)) => {
                        if reason == crate::runtime::error::SessionError::QueueFull {
                            stats
                                .inbound_rejected_queue_full
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        let _ = events.send(SessionEvent::InboundRejected { reason });
                    }
                    Err(_) => break,
                }
            }
            PairingEvent::Disconnected => {
                let _ = events.send(SessionEvent::TransportDisconnected);
            }
            PairingEvent::Reconnected { replayed } => {
                let _ = events.send(SessionEvent::TransportReconnected { replayed });
            }
            PairingEvent::RedeemOffered {
                code_id,
                pairing_id,
                peer,
            } => {
                let _ = events.send(SessionEvent::RedeemOffered {
                    code_id,
                    pairing_id,
                    peer,
                });
            }
            PairingEvent::PairAborted { pairing_id, .. } => {
                let _ = events.send(SessionEvent::PairAborted { pairing_id });
            }
        }
    }
    stats.running.store(false, Ordering::Relaxed);
}
