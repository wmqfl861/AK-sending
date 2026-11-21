//! Sender lifetimes, send leases and the adopted-socket probe.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;

/// One lifetime per paired sender, including degraded/no-socket pairings.
pub(super) struct SenderLifetime {
    pub(super) deadline: tokio::time::Instant,
    pub(super) active: HashSet<[u8; 16]>,
    pub(super) terminal: bool,
    pub(super) state: tokio::sync::watch::Sender<SenderKeepaliveState>,
    keeper: Option<JoinHandle<()>>,
}

impl SenderLifetime {
    pub(super) fn new(deadline: tokio::time::Instant) -> Self {
        let (state, _) = tokio::sync::watch::channel(SenderKeepaliveState::Idle);
        Self {
            deadline,
            active: HashSet::new(),
            terminal: false,
            state,
            keeper: None,
        }
    }

    pub(super) fn admit(&mut self, op_id: [u8; 16]) {
        self.active.insert(op_id);
        self.state.send_replace(SenderKeepaliveState::Active);
    }

    pub(super) fn close(&mut self) {
        self.terminal = true;
        self.state.send_replace(SenderKeepaliveState::Closed);
        if let Some(task) = self.keeper.take() {
            task.abort();
        }
    }
}

impl Drop for SenderLifetime {
    fn drop(&mut self) {
        self.close();
    }
}

/// Install the deadline before any send can be admitted. Also called by
/// the keeper hook, which is idempotent (no second opener on catch-up).
pub(super) fn ensure_sender_lifetime(rt: &TaskRt, conn: &TaskConn) {
    if conn.role == TaskRole::Sender {
        if let Ok(mut lifetimes) = rt.sender_lifetimes.lock() {
            lifetimes.entry(conn.conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
    }
}

pub(super) fn spawn_sender_opener(
    rt: &Arc<TaskRt>,
    code_id: [u8; 16],
    receiver_candidates: &[SocketAddr],
) {
    let Some(conn) = rt.conn_of_code(&code_id) else {
        return;
    };
    ensure_sender_lifetime(rt, &conn);
    let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
        return;
    };
    let Some(life) = lifetimes.get_mut(&conn.conn_id) else {
        return;
    };
    if life.terminal || life.keeper.is_some() || rt.is_stopped() {
        return;
    }
    let socket = rt.hole_sockets.lock().ok().and_then(|holes| {
        holes
            .get(&code_id)
            .and_then(|hole| hole.socket.try_clone().ok())
    });
    let deadline = life.deadline;
    let state = life.state.subscribe();
    let candidates = order_candidates(receiver_candidates.to_vec());
    // The resident task never strongly owns TaskRt; dropping the runtime
    // closes/aborts the keeper and releases its socket without an Arc ring.
    let opener_rt = Arc::downgrade(rt);
    let expiry_rt = Arc::downgrade(rt);
    let conn_id = conn.conn_id;
    life.keeper = Some(rt.executor.spawn(async move {
        sender_pairing_keeper(
            socket.as_ref(),
            &candidates,
            deadline,
            state,
            |packets| {
                if let Some(rt) = opener_rt.upgrade() {
                    if !rt.is_stopped() {
                        rt.push_event(format!(
                            "{{\"event\":\"reflexive_opener\",\"packets\":{packets}}}"
                        ));
                    }
                }
            },
            || {
                if let Some(rt) = expiry_rt.upgrade() {
                    expire_idle_sender(&rt, &conn_id);
                }
            },
        )
        .await;
    }));
}

pub(super) fn expire_idle_sender(rt: &Arc<TaskRt>, conn_id: &[u8; 16]) {
    let expired = {
        let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
            return;
        };
        rt.reconcile_sender_locked(&mut lifetimes, conn_id)
    };
    finish_idle_expiry(rt, expired);
}

/// Terminal state/event were decided under the gate. Network cleanup is
/// outside it; idle expiry never aborts a live send because active is empty.
pub(super) fn finish_idle_expiry(rt: &Arc<TaskRt>, expired: Option<TaskConn>) {
    let Some(conn) = expired else {
        return;
    };
    rt.decline_pending_of_conn(&conn.conn_id);
    if let Ok(table) = rt.codes.lock() {
        if let Some(entry) = table.values().find(|entry| entry.code_id == conn.code_id) {
            let peer = if entry.owner.runtime_id == rt.runtime_id {
                entry.joiner.clone()
            } else {
                Some(Arc::clone(&entry.owner))
            };
            if let Some(peer) = peer {
                peer.push_disconnected_once(&conn.conn_id, "peer");
            }
        }
    }
    if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
        rt.executor.spawn(async move {
            let _ = stack.pairing.disconnect(conn.code_id).await;
        });
    }
}

/// Owns exactly one admission. Constructed before spawn so cancellation
/// before first poll also releases it. The awaited send future is created
/// after this guard enters the task, and is dropped before the guard.
pub(super) struct SendLease {
    pub(super) rt: Weak<TaskRt>,
    pub(super) conn_id: [u8; 16],
    pub(super) op_id: [u8; 16],
    pub(super) entry: Arc<OpEntry>,
    pub(super) settled: bool,
}

impl SendLease {
    pub(super) fn finish(&mut self, result: Option<(bool, String)>) {
        if self.settled {
            return;
        }
        self.settled = true;
        let Some(rt) = self.rt.upgrade() else {
            return;
        };
        let expired = {
            let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
                return;
            };
            // Publication precedes releasing admission, in the same
            // critical section. Duplicate/cancelled settlement is silent.
            if let Ok(mut outcome) = self.entry.outcome.lock() {
                if matches!(&*outcome, OpOutcome::Running { .. }) {
                    let (ok, json) = result.unwrap_or_else(|| {
                        (
                            false,
                            "{\"op\":\"task_send\",\"error\":\"cancelled by caller\"}".to_string(),
                        )
                    });
                    *outcome = OpOutcome::Finished { ok, json };
                    rt.push_event(format!(
                        "{{\"event\":\"{}\",\"op_id\":\"{}\",\"op\":\"task_send\"}}",
                        if ok { "op_done" } else { "op_failed" },
                        core::hex_encode(&self.op_id),
                    ));
                }
            }
            let removed = lifetimes
                .get_mut(&self.conn_id)
                .is_some_and(|life| life.active.remove(&self.op_id));
            if removed {
                rt.reconcile_sender_locked(&mut lifetimes, &self.conn_id)
            } else {
                None
            }
        };
        finish_idle_expiry(&rt, expired);
    }
}

impl Drop for SendLease {
    fn drop(&mut self) {
        self.finish(None);
    }
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 2): the SENDER's pairing-time
/// connectivity test on the ADOPTED dial socket — one
/// [`probe_dial_round_on`] round over a `try_clone` of the stashed socket
/// (the exact round the send phase runs), folded into the pairing plane's
/// [`ProbeOutcome`] vocabulary. The legacy throwaway-socket test
/// (`probe_with_retry`) could STRUCTURALLY never pass through a
/// port-restricted-cone NAT — its mapping was not the QUIC socket's — and
/// its false `unreachable` used to gate the GUI's send button; on the
/// adopted socket the round observes the peer's own punch packets
/// arriving on the real mapping, so the verdict is meaningful. The
/// [`SENDER_DIAL_PAUSE`] wait keeps the opener's packets the adopted
/// socket's FIRST outbound (the CR-P1-06 ordering); without a stash (bind
/// failed at create/join) the throwaway round stays the honest fallback.
pub(super) async fn adopted_socket_probe(
    rt: &TaskRt,
    code_id: &[u8; 16],
    candidates: &[SocketAddr],
) -> ProbeOutcome {
    let ordered = order_candidates(candidates.to_vec());
    tokio::time::sleep(SENDER_DIAL_PAUSE).await;
    let adopted = rt
        .hole_sockets
        .lock()
        .ok()
        .and_then(|holes| holes.get(code_id).and_then(|e| e.socket.try_clone().ok()));
    let plan = match adopted {
        Some(socket) => {
            probe_dial_round_on(socket, ordered.len(), ordered.clone(), |_, _, _| {}).await
        }
        None => probe_dial_round(ordered.clone(), |_, _, _| {}).await,
    };
    round_probe_outcome(&ordered, &plan)
}
