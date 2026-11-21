//! v2 sessions: connect, announce, signal and cancel (contract §10).
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- v2 sessions (contract §10) ------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_connect(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    target: DeviceId,
    pairing_id: PairingId,
    code_id: CodeId,
    connection_id: [u8; 16],
) {
    if target == me {
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "self connect"));
        return;
    }
    // v2 sessions are keyed by the verified triple: a completed pairing
    // relation must exist for exactly this (pairing_id, code_id) and this
    // endpoint pair.
    let verified = shared
        .store
        .relation_exists(&pairing_id, &code_id, &me, &target)
        .unwrap_or(false);
    if !verified {
        deliver_msg(
            out,
            pair_error(
                [0u8; 16],
                ErrorScope::Connection,
                connection_id,
                PAIR_UNAVAILABLE,
                0,
            ),
        );
        return;
    }
    let mut inner = shared.lock();
    if !inner.conns.contains_key(&target) {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_UNKNOWN_DEVICE, "target device not online"),
        );
        return;
    }
    let triple = (pairing_id, code_id, connection_id);
    // Idempotent: an existing session for the same triple is re-announced.
    if let Some(existing) = inner.session_by_triple.get(&triple) {
        if let Some(entry) = inner.sessions.get(existing) {
            let entry = entry.clone();
            let session = *existing;
            drop(inner);
            announce_session(shared, &entry, session, me, target);
            return;
        }
    }
    let my_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&me))
        .count();
    let target_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&target))
        .count();
    if my_count >= shared.limits.max_sessions_per_device
        || target_count >= shared.limits.max_sessions_per_device
    {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_SESSION_LIMIT, "session limit reached"),
        );
        return;
    }
    let mut session_id = random_id16();
    for _ in 0..8 {
        if !inner.sessions.contains_key(&session_id) {
            break;
        }
        session_id = random_id16();
    }
    let entry = SessionEntry {
        triple,
        endpoints: [me, target],
        expires_at: Instant::now() + shared.limits.session_ttl,
    };
    inner.sessions.insert(session_id, entry.clone());
    inner.session_by_triple.insert(triple, session_id);
    drop(inner);
    announce_session(shared, &entry, session_id, me, target);
}

fn announce_session(
    shared: &Shared,
    session_entry: &SessionEntry,
    session: SessionId,
    me: DeviceId,
    target: DeviceId,
) {
    let expires_ms = now_ms() + shared.limits.session_ttl.as_millis() as u64;
    let (pairing_id, code_id, connection_id) = session_entry.triple;
    for (device, peer) in [(me, target), (target, me)] {
        shared.deliver(
            &device,
            CtrlMessage::SessionOpened {
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            },
        );
    }
    // SameEgress hint: both endpoints' control-plane peer IPs known and
    // identical (ordering hint only, never a trust claim).
    let inner = shared.lock();
    let me_ip = inner.conns.get(&me).map(|entry| entry.peer_ip);
    let target_ip = inner.conns.get(&target).map(|entry| entry.peer_ip);
    drop(inner);
    if crate::util::same_egress(me_ip, target_ip) {
        for device in [me, target] {
            shared.deliver(
                &device,
                CtrlMessage::Signal {
                    session,
                    signal: crate::protocol_v2::PeerSignal::SameEgress,
                },
            );
        }
    }
}

pub(super) fn handle_signal(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    session: SessionId,
    signal: crate::protocol_v2::PeerSignal,
) {
    let inner = shared.lock();
    let Some(entry) = inner.sessions.get(&session) else {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    };
    if !entry.endpoints.contains(&me) {
        drop(inner);
        // Unknown and non-member sessions look identical (no oracle).
        deliver_msg(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    }
    let other = if entry.endpoints[0] == me {
        entry.endpoints[1]
    } else {
        entry.endpoints[0]
    };
    drop(inner);
    // Verbatim relay of one bounded, structured signal.
    shared.deliver(&other, CtrlMessage::Signal { session, signal });
}

pub(super) fn handle_cancel(shared: &Shared, me: DeviceId, session: SessionId) {
    let mut inner = shared.lock();
    let mine = inner
        .sessions
        .get(&session)
        .map(|entry| entry.endpoints.contains(&me))
        .unwrap_or(false);
    if mine {
        if let Some(entry) = inner.sessions.remove(&session) {
            inner.session_by_triple.remove(&entry.triple);
        }
    }
    // Cancelling an unknown session is idempotent, not an error.
}
