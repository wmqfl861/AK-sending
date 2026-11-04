//! Shutdown semantics (contract §10 "close runtime" / B6 gates): stop new
//! admissions, wait in-flight with a timeout, cancel residuals, join the
//! actor and the listener, and return a faithful summary.
mod common;

use ak_session::runtime::{FakeScript, SessionError, ShutdownMode, ShutdownSummary, SnapshotQuery};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode};
use common::{advance_while, env_with, env_with_config, group, policy, send_spec};

/// Drain mode: the in-flight task finishes naturally; the task admitted
/// after shutdown began never reaches the engine and is cancelled; the
/// runtime rejects further submissions and joins everything.
#[tokio::test]
async fn shutdown_drain_completes_inflight_cancels_residual() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
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

    let summary = advance_while(&env, env.handle.shutdown(ShutdownMode::Drain))
        .await
        .expect("drain shutdown");

    assert_eq!(summary.mode, ShutdownMode::Drain);
    assert!(!summary.timed_out);
    assert_eq!(summary.completed, 1, "in-flight task finished: {summary:?}");
    assert_eq!(
        summary.cancelled, 1,
        "queued residual cancelled: {summary:?}"
    );
    assert!(summary.listener_stopped);
    assert_eq!(env.executor.start_count(), 1, "t2 never entered the engine");
    let snap_tail = env.handle.snapshot(SnapshotQuery::all()).await;
    assert!(matches!(snap_tail, Err(SessionError::RuntimeStopped)));
    let _ = (t1, t2);
}

/// Cancel mode: in-flight tasks are stopped through the executor, queued
/// tasks are removed, everything converges to `Cancelled`.
#[tokio::test]
async fn shutdown_cancel_stops_everything() {
    let env = env_with(policy(SameCodeMode::Serial, CrossCodeMode::Serial, 1, 1, 8)).await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let code_a = group(1);
    let t1 = env
        .handle
        .enqueue(send_spec(1, code_a, &env.executor))
        .await
        .unwrap();
    let t2 = env
        .handle
        .enqueue(send_spec(2, group(2), &env.executor))
        .await
        .unwrap();

    let summary = advance_while(&env, env.handle.shutdown(ShutdownMode::Cancel))
        .await
        .expect("cancel shutdown");

    assert!(
        !summary.timed_out,
        "cancel mode needs no timeout: {summary:?}"
    );
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.cancelled, 2, "running and queued both cancelled");
    assert_eq!(
        env.executor.cancel_calls(),
        vec![t1],
        "engine stop requested once"
    );
    let t1_interval = env.executor.intervals_of(t1)[0];
    assert!(t1_interval.end_ms.is_some(), "running interval closed");
    assert_eq!(
        env.executor.intervals_of(t2).len(),
        0,
        "queued never started"
    );
    let _ = t2;
}

/// The shutdown deadline (Fake clock) forces completion when in-flight
/// tasks refuse to finish; residuals are still cancelled and counted.
#[tokio::test]
async fn shutdown_timeout_forces_completion() {
    let env = env_with_config(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64),
        |config| config.shutdown_timeout_ms = 50,
    )
    .await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    let _hanging = env
        .handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();

    let summary = advance_while(&env, env.handle.shutdown(ShutdownMode::Drain))
        .await
        .expect("forced shutdown");

    assert!(
        summary.timed_out,
        "deadline must force the drain: {summary:?}"
    );
    assert_eq!(summary.cancelled, 1);
    assert_eq!(summary.completed, 0);
    assert!(
        env.clock.now_ms() >= 1050,
        "fake clock passed the deadline (start 1000 + 50)"
    );
}

/// A second shutdown while one is in flight is refused; the first summary
/// stays authoritative.
#[tokio::test]
async fn second_shutdown_is_rejected() {
    let env = env_with_config(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64),
        |config| config.shutdown_timeout_ms = 1_500,
    )
    .await;
    env.executor
        .set_default_script(FakeScript::NeverFinish { scan_ms: 20 });
    env.handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();

    let handle2 = env.handle.clone();
    // The second shutdown is sent after the first has registered.
    let second = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        handle2.shutdown(ShutdownMode::Cancel).await
    });
    let summary: ShutdownSummary = advance_while(&env, env.handle.shutdown(ShutdownMode::Drain))
        .await
        .expect("first shutdown succeeds");
    let second = second.await.expect("join second attempt");
    match second {
        Err(SessionError::AlreadyShuttingDown | SessionError::RuntimeStopped) => {}
        Err(other) => panic!("unexpected second-shutdown error: {other:?}"),
        Ok(summary) => panic!("second shutdown must not succeed: {summary:?}"),
    }
    assert!(summary.timed_out || summary.cancelled >= 1);
}
