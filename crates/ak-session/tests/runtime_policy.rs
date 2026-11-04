//! Runtime policy switches (contract §8): `serial → parallel` backfills
//! immediately; `parallel → serial` drains without dropping in-flight
//! tasks and converges naturally.
mod common;

use ak_session::runtime::{FakeScript, SessionEvent, SnapshotQuery};
use ak_session::scheduler::{
    CrossCodeMode, SameCodeMode, SchedulerEvent, SchedulerPolicy, TaskState,
};
use common::{all_terminal, env_with, group, policy, run_until, send_spec, state_of};

/// `serial → parallel`: queued tasks are admitted at once (interval
/// overlap proves the backfill actually started them concurrently).
#[tokio::test]
async fn serial_to_parallel_backfills_immediately() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    let code = group(1);
    for n in 1..=3u64 {
        env.handle
            .enqueue(send_spec(n, code, &env.executor))
            .await
            .expect("enqueue");
    }
    // One active, two queued under same-code serial.
    let before = env.handle.snapshot(SnapshotQuery::all()).await.unwrap();
    assert_eq!(before.active_tasks, 1);
    assert_eq!(before.queued_tasks, 2);

    let switch = env
        .handle
        .set_policy(policy(
            SameCodeMode::Parallel,
            CrossCodeMode::Parallel,
            4,
            4,
            64,
        ))
        .await
        .expect("switch");
    assert_eq!(switch.revision, 2);
    assert!(!switch.draining);
    assert_eq!(switch.admitted, 2, "serial→parallel backfills the queue");

    run_until(&env, all_terminal, 3_000).await;
    assert_eq!(env.executor.start_count(), 3);
    common::assert_all_overlap(&env.executor, "backfilled tasks overlap the running one");
}

/// `parallel → serial`: enters draining, in-flight tasks finish naturally
/// (never dropped), and the queued task starts only after they converge.
#[tokio::test]
async fn parallel_to_serial_drains_and_converges() {
    let env = env_with(policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        2,
        4,
        64,
    ))
    .await;
    env.executor.set_default_script(FakeScript::Complete {
        scan_ms: 20,
        transfer_ms: 280,
    });
    let code = group(1);
    let t1 = env
        .handle
        .enqueue(send_spec(1, code, &env.executor))
        .await
        .unwrap();
    let t2 = env
        .handle
        .enqueue(send_spec(2, code, &env.executor))
        .await
        .unwrap();
    let t3 = env
        .handle
        .enqueue(send_spec(3, code, &env.executor))
        .await
        .unwrap();
    let mut rx = env.handle.subscribe();

    let switch = env
        .handle
        .set_policy(policy(
            SameCodeMode::Serial,
            CrossCodeMode::Parallel,
            2,
            4,
            64,
        ))
        .await
        .expect("switch");
    assert!(switch.draining, "over-limit active set enters draining");
    assert_eq!(switch.admitted, 0);
    let right_after = env.handle.snapshot(SnapshotQuery::all()).await.unwrap();
    assert!(right_after.draining);
    assert_eq!(right_after.active_tasks, 2, "in-flight tasks keep running");
    assert_eq!(state_of(&right_after, t3), TaskState::Queued);

    // Both in-flight tasks complete naturally; t3 then converges.
    run_until(
        &env,
        |s| state_of(s, t1) == TaskState::Completed && state_of(s, t2) == TaskState::Completed,
        3_000,
    )
    .await;
    let snap = run_until(&env, all_terminal, 4_000).await;
    assert_eq!(state_of(&snap, t3), TaskState::Completed);
    let i1 = env.executor.intervals_of(t1)[0];
    let i2 = env.executor.intervals_of(t2)[0];
    let i3 = env.executor.intervals_of(t3)[0];
    let (end1, end2) = (i1.end_ms.unwrap(), i2.end_ms.unwrap());
    assert!(i3.start_ms >= end1, "t3 waited for t1: {i1:?} {i3:?}");
    assert!(i3.start_ms >= end2, "t3 waited for t2: {i2:?} {i3:?}");
    assert_eq!(
        env.executor.start_count(),
        3,
        "no task dropped by the switch"
    );

    let mut saw_draining = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            SessionEvent::Scheduler(SchedulerEvent::Draining { .. })
        ) {
            saw_draining = true;
        }
    }
    assert!(saw_draining, "Draining event broadcast");
}

/// Switching to an equivalent policy keeps ids, permits and queue intact
/// (no re-pairing, no checkpoint loss — contract §8).
#[tokio::test]
async fn policy_switch_keeps_task_identity_and_order() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let code = group(1);
    let t1 = env
        .handle
        .enqueue(send_spec(1, code, &env.executor))
        .await
        .unwrap();
    let t2 = env
        .handle
        .enqueue(send_spec(2, code, &env.executor))
        .await
        .unwrap();
    env.handle
        .set_policy(SchedulerPolicy::default())
        .await
        .unwrap();
    let snap = run_until(&env, |s| state_of(s, t1) == TaskState::Transferring, 500).await;
    assert_eq!(snap.active_tasks, 1);
    assert_eq!(snap.queued_tasks, 1);
    assert_eq!(state_of(&snap, t1), TaskState::Transferring);
    assert_eq!(state_of(&snap, t2), TaskState::Queued);
    assert_eq!(snap.policy_revision, 2);
}
