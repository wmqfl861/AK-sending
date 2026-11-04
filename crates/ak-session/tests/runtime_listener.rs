//! Resident listener (B6): pairing-completion events from the injected
//! transport become inbound receive tasks in the same scheduler; a full
//! queue refuses them with a warning instead of blocking; events buffered
//! while the transport was offline are replayed after reconnect, in order.
mod common;

use std::time::Duration;

use ak_session::runtime::{
    FakeScript, PairingEvent, SessionEvent, SnapshotQuery, TransferDirection,
};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode, TaskState};
use common::{env_with, group, pairing, peer, policy, registry, run_until};

fn pair_ready(code: u8, peer_byte: u8) -> PairingEvent {
    PairingEvent::PairReady {
        registry_id: registry(),
        code_id: group(code).code_id,
        pairing_id: pairing(peer_byte),
        peer: peer(peer_byte),
    }
}

/// A completed pairing submits one inbound receive task into the same
/// scheduler (same group key as the code that produced the relation).
#[tokio::test]
async fn pairready_submits_inbound_task_into_same_scheduler() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    env.transport.push_event(pair_ready(5, 0xAB));

    let snap = run_until(
        &env,
        |s| s.tasks.len() == 1 && s.tasks[0].state == TaskState::Completed,
        2_000,
    )
    .await;
    let view = &snap.tasks[0];
    assert_eq!(view.direction, TransferDirection::Receive);
    assert_eq!(view.group, group(5));
    assert!(view.label.contains(&peer(0xAB).to_hex()));
    assert_eq!(snap.listener.inbound_submitted, 1);
    assert_eq!(snap.listener.inbound_rejected_queue_full, 0);
    assert!(snap.listener.running);
}

/// An inbound submission against a full queue is refused, counted and
/// surfaced as an event — the listener loop survives (contract §7).
#[tokio::test]
async fn inbound_submission_on_full_queue_is_refused_with_event() {
    let env = env_with(policy(SameCodeMode::Serial, CrossCodeMode::Serial, 1, 1, 1)).await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let mut rx = env.handle.subscribe();
    // Occupy the single active group and the single queue slot.
    env.handle
        .enqueue(common::send_spec(1, group(1), &env.executor))
        .await
        .unwrap();
    env.transport.push_event(pair_ready(2, 0x21)); // fills the queue
    let snap = run_until(&env, |s| s.listener.inbound_submitted == 1, 500).await;
    assert_eq!(snap.queued_tasks, 1, "first inbound task queued");

    env.transport.push_event(pair_ready(3, 0x22)); // queue full now
    let snap = run_until(&env, |s| s.listener.inbound_rejected_queue_full == 1, 500).await;
    assert_eq!(snap.listener.inbound_rejected_queue_full, 1);
    assert_eq!(snap.listener.inbound_submitted, 1, "no double submit");

    let mut saw_rejection = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, SessionEvent::InboundRejected { .. }) {
            saw_rejection = true;
        }
    }
    assert!(saw_rejection, "InboundRejected event broadcast");
    assert!(snap.listener.running, "listener loop survives the refusal");
}

/// Events pushed while the transport is offline are buffered; after
/// reconnect they are replayed in order (after a Reconnected marker).
#[tokio::test]
async fn reconnect_replays_buffered_pairing_events_in_order() {
    let env = env_with(policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    let mut rx = env.handle.subscribe();

    // Offline: two pairings complete elsewhere; nothing may be submitted.
    env.transport.set_online(false);
    env.transport.push_event(pair_ready(1, 0x51));
    env.transport.push_event(pair_ready(2, 0x52));
    for _ in 0..20 {
        env.clock.advance_ms(10);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let snap = env.handle.snapshot(SnapshotQuery::all()).await.unwrap();
    assert!(snap.tasks.is_empty(), "offline transport delivers nothing");

    // Reconnect: buffered events replay in push order.
    env.transport.set_online(true);
    let snap = run_until(&env, |s| s.tasks.len() == 2, 1_000).await;
    assert_eq!(snap.listener.inbound_submitted, 2, "both replayed");
    let first = &snap.tasks[0];
    let second = &snap.tasks[1];
    assert!(first.task < second.task, "replay preserves order");
    assert!(first.label.contains(&peer(0x51).to_hex()));
    assert!(second.label.contains(&peer(0x52).to_hex()));

    let mut marker = None;
    while let Ok(event) = rx.try_recv() {
        if let SessionEvent::TransportReconnected { replayed } = event {
            marker = Some(replayed);
        }
    }
    assert_eq!(marker, Some(2), "reconnect marker carries the replay count");
}
