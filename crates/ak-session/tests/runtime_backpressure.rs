//! Backpressure rules of contract §7: bounded queue rejections never
//! create connections, and a slow event consumer lags (drop-oldest)
//! without ever blocking the scheduler actor.
mod common;

use ak_session::runtime::{FakeScript, SessionError, SessionEvent, SnapshotQuery};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode, TaskState};
use common::{all_terminal, env_with, group, policy, run_until, send_spec};

/// A full local queue answers `QUEUE_FULL` synchronously: nothing was
/// scanned, no pairing session was opened, no connection was created and
/// no queued task was dropped.
#[tokio::test]
async fn queue_full_rejects_without_connecting() {
    let env = env_with(policy(SameCodeMode::Serial, CrossCodeMode::Serial, 1, 1, 2)).await;
    // One hanging task keeps the single active-code-group slot busy.
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let blocked = group(1);
    env.handle
        .enqueue(send_spec(1, blocked, &env.executor))
        .await
        .expect("first task admitted");

    // Two more tasks of other codes wait in the queue (cross-serial).
    let t2 = env
        .handle
        .enqueue(send_spec(2, group(2), &env.executor))
        .await
        .unwrap();
    let t3 = env
        .handle
        .enqueue(send_spec(3, group(3), &env.executor))
        .await
        .unwrap();
    let queued_before = env
        .handle
        .snapshot(SnapshotQuery::all())
        .await
        .unwrap()
        .queued_tasks;
    assert_eq!(queued_before, 2);

    // The queue is at capacity: refuse the next submission outright.
    let refused = send_spec(4, group(2), &env.executor);
    let err = env.handle.enqueue(refused).await.unwrap_err();
    assert_eq!(err, SessionError::QueueFull);

    // …without ever touching the executor or the transport for it.
    assert_eq!(
        env.executor.start_count(),
        1,
        "only the admitted task started"
    );
    assert_eq!(
        env.transport.open_calls().len(),
        1,
        "no session for queued/refused tasks"
    );
    assert!(env.executor.cancel_calls().is_empty());
    assert_eq!(env.executor.intervals_of(t2).len(), 0);
    assert_eq!(env.executor.intervals_of(t3).len(), 0);

    // Nothing was dropped to make room.
    let snap = env.handle.snapshot(SnapshotQuery::all()).await.unwrap();
    assert_eq!(snap.queued_tasks, 2);
    assert_eq!(snap.active_tasks, 1);
    assert_eq!(
        common::state_of(&snap, t2),
        TaskState::Queued,
        "queued task untouched by the refusal"
    );
}

/// A subscriber that stops reading lags once more than 1024 events are
/// broadcast (drop-oldest, contract §7) — and the actor keeps working.
#[tokio::test]
async fn slow_consumer_lags_without_blocking_the_actor() {
    let env = env_with(policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        16,
        16,
        512,
    ))
    .await;
    let mut slow = env.handle.subscribe();

    // 300 tasks × (Queued + Admitted + Started + Finished) > 1024 events.
    let code = group(1);
    for n in 1..=300u64 {
        env.handle
            .enqueue(send_spec(n, code, &env.executor))
            .await
            .expect("enqueue");
    }
    run_until(&env, all_terminal, 30_000).await;

    // The slow consumer is told it missed events (the warning)…
    match slow.try_recv() {
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(missed)) => {
            assert!(
                missed >= 1,
                "expected at least one missed event, got {missed}"
            );
        }
        other => panic!("expected Lagged, got {other:?}"),
    }

    // …and the actor never blocked: a new submission and snapshot work.
    let extra = env
        .handle
        .enqueue(send_spec(999, group(2), &env.executor))
        .await;
    assert!(extra.is_ok(), "actor still accepts commands: {extra:?}");
    let snap = env
        .handle
        .snapshot(SnapshotQuery::all())
        .await
        .expect("snapshot after lag");
    // P2-06: the resident set is bounded — the runtime saturated its
    // capped terminal history (it tracked every task; only the oldest
    // finished records were evicted) and still lists the fresh task.
    assert!(
        snap.tasks.len() >= ak_session::runtime::MAX_TERMINAL_HISTORY,
        "the bounded history is saturated and the actor kept working, got {} entries",
        snap.tasks.len()
    );
    let fresh = ak_session::scheduler::TaskId::from_counter(999);
    assert!(
        snap.tasks.iter().any(|view| view.task == fresh),
        "the post-lag submission is listed: {:?}",
        snap.tasks.iter().map(|view| view.task).collect::<Vec<_>>()
    );
}

/// A fast subscriber observes the scheduler lifecycle events in order.
#[tokio::test]
async fn fast_subscriber_sees_ordered_lifecycle_events() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    let mut rx = env.handle.subscribe();
    let task = env
        .handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();
    run_until(&env, all_terminal, 2_000).await;

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    use ak_session::scheduler::{SchedulerEvent, TaskHandle};
    let target = TaskHandle::new(task);
    let idx = |phase: &str| {
        events
            .iter()
            .position(|event| match event {
                SessionEvent::Scheduler(SchedulerEvent::TaskQueued { task, .. }) => {
                    phase == "queued" && *task == target
                }
                SessionEvent::Scheduler(SchedulerEvent::TaskAdmitted { task, .. }) => {
                    phase == "admitted" && *task == target
                }
                SessionEvent::Scheduler(SchedulerEvent::TaskStarted { task }) => {
                    phase == "started" && *task == target
                }
                SessionEvent::Scheduler(SchedulerEvent::TaskFinished { task, .. }) => {
                    phase == "finished" && *task == target
                }
                _ => false,
            })
            .unwrap_or_else(|| panic!("{phase} event for {target:?} missing; got {events:?}"))
    };
    let (i_queued, i_admitted, i_started, i_finished) = (
        idx("queued"),
        idx("admitted"),
        idx("started"),
        idx("finished"),
    );
    assert!(i_queued < i_admitted, "queued must precede admitted");
    assert!(i_admitted < i_started, "admitted must precede started");
    assert!(i_started < i_finished, "started must precede finished");
}
