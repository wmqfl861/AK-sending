//! Task-lifecycle orchestration: happy-path phases, failure backoff
//! (Fake-clock driven), retry-cap parking with manual retry, and the
//! cancel layering of contract §10 (queued removal vs running stop).
mod common;

use ak_core::error::ErrorCode;
use ak_session::runtime::{CancelReceipt, FakeScript, RetryPolicy, SessionEvent};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode, TaskId, TaskState};
use common::{
    all_terminal, env_with, env_with_config, group, policy, run_until, send_spec, state_of,
};

/// Runtime with a custom retry policy.
async fn env_with_retry(
    policy: ak_session::scheduler::SchedulerPolicy,
    retry: RetryPolicy,
) -> common::Env {
    env_with_config(policy, |config| config.retry = retry).await
}

/// A submitted task walks the whole §9 happy path and completes once.
#[tokio::test]
async fn submit_walks_phases_to_completion() {
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

    let snap = run_until(&env, all_terminal, 2_000).await;
    let view = common::view_of(&snap, task);
    assert_eq!(view.state, TaskState::Completed);
    assert_eq!(view.attempts, 0, "no failures, no retries");
    assert_eq!(view.last_error, None);
    assert_eq!(view.direction, ak_session::runtime::TransferDirection::Send);
    assert_eq!(env.executor.start_count(), 1);
    assert_eq!(snap.policy_revision, 1);

    let mut started_events = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            SessionEvent::Scheduler(ak_session::scheduler::SchedulerEvent::TaskStarted { .. })
        ) {
            started_events += 1;
        }
    }
    assert_eq!(started_events, 1, "TaskStarted exactly once");
}

/// A failing task backs off on the fake clock, retries and completes.
#[tokio::test]
async fn failure_backoff_retries_then_completes() {
    let retry = RetryPolicy {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 1_000,
        multiplier: 2,
    };
    let env = env_with_retry(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64),
        retry,
    )
    .await;
    let task = TaskId::from_counter(7);
    env.executor.set_script(
        task,
        FakeScript::Fail {
            scan_ms: 10,
            error: ErrorCode::DirectUnavailable,
            times: 1,
            then_transfer_ms: 50,
        },
    );
    let mut spec = send_spec(7, group(1), &env.executor);
    spec.task_id = Some(task);
    let submitted = env.handle.enqueue(spec).await.unwrap();
    assert_eq!(submitted, task);

    // First attempt fails, a backoff is scheduled (100 ms on the fake
    // clock) and the task is NOT restarted before it elapses.
    let snap = run_until(
        &env,
        |s| common::view_of(s, task).attempts == 1 && !state_of(s, task).is_terminal(),
        1_000,
    )
    .await;
    assert_eq!(env.executor.start_count(), 1, "no retry before the backoff");
    assert_eq!(
        common::view_of(&snap, task).last_error,
        Some(ErrorCode::DirectUnavailable)
    );

    let snap = run_until(&env, |s| state_of(s, task) == TaskState::Completed, 3_000).await;
    assert_eq!(env.executor.start_count(), 2, "exactly one retry");
    assert_eq!(state_of(&snap, task), TaskState::Completed);
}

/// After the retry budget is exhausted the task is parked (slot released)
/// and only a manual `retry` restarts it.
#[tokio::test]
async fn retry_cap_parks_task_until_manual_retry() {
    let retry = RetryPolicy {
        max_attempts: 2,
        initial_backoff_ms: 50,
        max_backoff_ms: 200,
        multiplier: 2,
    };
    let env = env_with_retry(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64),
        retry,
    )
    .await;
    let task = TaskId::from_counter(9);
    env.executor.set_script(
        task,
        FakeScript::Fail {
            scan_ms: 10,
            error: ErrorCode::HashMismatch,
            times: 10,
            then_transfer_ms: 50,
        },
    );
    let mut spec = send_spec(9, group(1), &env.executor);
    spec.task_id = Some(task);
    env.handle.enqueue(spec).await.unwrap();

    let snap = run_until(&env, |s| state_of(s, task) == TaskState::Paused, 3_000).await;
    assert_eq!(
        env.executor.start_count(),
        2,
        "budget of two attempts respected"
    );
    let view = common::view_of(&snap, task);
    assert_eq!(view.state, TaskState::Paused, "parked, not terminal");
    assert_eq!(
        view.parked,
        Some(ak_session::runtime::ParkReason::RetryExhausted)
    );
    assert_eq!(view.last_error, Some(ErrorCode::HashMismatch));
    assert_eq!(snap.active_tasks, 0, "parking released the slot");
    assert_eq!(snap.live_permits, 0, "no permit leak");

    // Manual retry resets the budget: the still-failing task gets its two
    // attempts again and parks once more.
    env.handle.retry(task).await.expect("manual retry");
    run_until(&env, |s| state_of(s, task) == TaskState::Paused, 3_000).await;
    assert_eq!(
        env.executor.start_count(),
        4,
        "two more attempts after the retry reset"
    );
}

/// Cancelling a queued task removes it without ever connecting.
#[tokio::test]
async fn cancel_queued_task_removes_without_connecting() {
    let env = env_with(policy(SameCodeMode::Serial, CrossCodeMode::Serial, 1, 1, 8)).await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let code = group(1);
    let _running = env
        .handle
        .enqueue(send_spec(1, code, &env.executor))
        .await
        .unwrap();
    let queued = env
        .handle
        .enqueue(send_spec(2, code, &env.executor))
        .await
        .unwrap();

    let receipt = env.handle.cancel(queued).await.expect("cancel queued");
    assert_eq!(receipt, CancelReceipt::RemovedQueued);

    let snap = run_until(&env, |s| state_of(s, queued) == TaskState::Cancelled, 500).await;
    assert_eq!(state_of(&snap, queued), TaskState::Cancelled);
    assert_eq!(snap.queued_tasks, 0);
    assert_eq!(env.executor.start_count(), 1, "queued task never started");
    assert_eq!(env.executor.intervals_of(queued).len(), 0);
    assert!(
        env.executor.cancel_calls().is_empty(),
        "no engine stop needed"
    );
    assert_eq!(
        env.transport.open_calls().len(),
        1,
        "no connection for the queued task"
    );
    // Cancelling a terminal task is refused.
    assert_eq!(
        env.handle.cancel(queued).await.unwrap_err(),
        ak_session::runtime::SessionError::NotActive
    );
}

/// Cancelling a running task stops the engine, waits for its report and
/// releases the permit (contract §10 "cancel active task").
#[tokio::test]
async fn cancel_running_task_stops_engine_and_finalizes() {
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
    let task = env
        .handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();
    // Let it reach Transferring so the recorded interval is non-degenerate.
    run_until(&env, |s| state_of(s, task) == TaskState::Transferring, 500).await;

    let receipt = env.handle.cancel(task).await.expect("cancel running");
    assert_eq!(receipt, CancelReceipt::RunningCancellationRequested);

    // Briefly CancelRequested, then terminal Cancelled once the executor
    // reports the stop.
    let snap = run_until(&env, |s| state_of(s, task) == TaskState::Cancelled, 2_000).await;
    assert_eq!(state_of(&snap, task), TaskState::Cancelled);
    assert_eq!(
        env.executor.cancel_calls(),
        vec![task],
        "engine was told to stop"
    );
    let interval = env.executor.intervals_of(task)[0];
    let end = interval.end_ms.expect("interval closed on cancel");
    assert!(end > interval.start_ms, "task ran before being stopped");
    assert_eq!(snap.live_permits, 0, "permit released");
    assert_eq!(snap.active_tasks, 0);

    // The freed slot serves the next submission immediately.
    let next = env
        .handle
        .enqueue(send_spec(2, group(1), &env.executor))
        .await;
    assert!(next.is_ok());
}

/// CR-P2-15: the `on_attempt_settled` hook fires strictly BEFORE the
/// scheduler core applies the task's terminal transition — before the
/// task's own `TaskFinished` is broadcast and before the released slot
/// admits the queued same-code successor. This is the ordering the
/// daemon's `job_done`-before-`job_started` guarantee rests on: the
/// actor is the single writer of the core, so the hook running inside
/// `on_finished` precedes the slot release by construction, not by
/// scheduling luck. The probe subscribes before any task is submitted
/// and drains synchronously inside the hook; the events asserted absent
/// are the exact consequences of the release.
#[tokio::test]
async fn attempt_settled_hook_precedes_slot_release_and_events() {
    use std::sync::{Arc, Mutex};

    use ak_session::runtime::RuntimeConfig;
    use ak_session::scheduler::SchedulerEvent;

    let first = TaskId::from_counter(1);
    let successor = TaskId::from_counter(2);
    let probe: Arc<Mutex<Option<tokio::sync::broadcast::Receiver<SessionEvent>>>> =
        Arc::new(Mutex::new(None));
    let fired: Arc<Mutex<Vec<TaskId>>> = Arc::new(Mutex::new(Vec::new()));
    let (hook_probe, hook_fired) = (Arc::clone(&probe), Arc::clone(&fired));
    let env = env_with_config(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 1, 4, 64),
        |config: &mut RuntimeConfig| {
            config.on_attempt_settled = Some(Arc::new(move |task| {
                let current = task.task_id();
                hook_fired.lock().unwrap().push(current);
                let mut violations: Vec<&'static str> = Vec::new();
                let guard = &mut *hook_probe.lock().unwrap();
                if let Some(rx) = guard.as_mut() {
                    loop {
                        match rx.try_recv() {
                            Ok(SessionEvent::Scheduler(SchedulerEvent::TaskFinished {
                                task,
                                ..
                            })) if task.task_id() == current => violations
                                .push("the task's own TaskFinished was broadcast before the hook"),
                            Ok(SessionEvent::Scheduler(SchedulerEvent::TaskAdmitted {
                                task,
                                ..
                            })) if current == first && task.task_id() == successor => violations
                                .push(
                                    "the freed slot's next admission was broadcast before the hook",
                                ),
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                            Err(_) => break,
                        }
                    }
                }
                assert!(
                    violations.is_empty(),
                    "events beat the settled hook: {violations:?}"
                );
            }));
        },
    )
    .await;
    *probe.lock().unwrap() = Some(env.handle.subscribe());

    // Same group under same-code serial: the successor stays queued until
    // the first task's slot release — exactly the release the hook must
    // precede.
    env.handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();
    env.handle
        .enqueue(send_spec(2, group(1), &env.executor))
        .await
        .unwrap();

    let snap = run_until(&env, all_terminal, 5_000).await;
    assert!(
        snap.tasks.iter().all(|t| t.state == TaskState::Completed),
        "both tasks complete"
    );
    assert_eq!(
        *fired.lock().unwrap(),
        vec![first, successor],
        "the hook fired exactly once per settled task, in settle order"
    );
}
