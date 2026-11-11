//! Terminal-history bounding (P2-06): a resident runtime must not keep
//! one working entry — executor handle included — per finished task
//! forever. Terminal tasks release their working spec and keep only a
//! capped slim record, so the snapshot scan stays bounded no matter how
//! many receives completed, were refused, timed out or errored.
mod common;

use ak_session::runtime::{FakeScript, SessionError, SnapshotQuery, MAX_TERMINAL_HISTORY};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode, TaskState};
use common::{all_terminal, env_with, group, policy, run_until, send_spec};

/// Completing far more tasks than the history cap keeps the resident
/// entry set bounded: after 384 terminal tasks the snapshot lists at most
/// `MAX_TERMINAL_HISTORY` of them (oldest evicted first), and the live
/// counters converge to zero.
#[tokio::test]
async fn terminal_history_stays_bounded_after_many_completions() {
    let env = env_with(policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        4,
        4,
        512,
    ))
    .await;
    // 10 fake ms per attempt keeps the whole run (384 tasks) well inside
    // the fake-clock budget without changing any lifecycle semantics.
    env.executor.set_default_script(FakeScript::Complete {
        scan_ms: 5,
        transfer_ms: 5,
    });

    let batches = 6;
    let per_batch = 64;
    for batch in 0..batches {
        for i in 0..per_batch {
            let n = batch * per_batch + i + 1;
            env.handle
                .enqueue(send_spec(n, group(1), &env.executor))
                .await
                .expect("enqueue within the queue bound");
        }
        // The batch's tasks finish within the budget (4 parallel × 100
        // fake ms per attempt ⇒ ~1.6 fake seconds per batch); everything
        // resident from earlier batches is already terminal.
        run_until(&env, all_terminal, 5_000).await;
    }

    let snap = env
        .handle
        .snapshot(SnapshotQuery::all())
        .await
        .expect("snapshot");
    assert_eq!(snap.active_tasks, 0, "nothing is running");
    assert_eq!(snap.queued_tasks, 0, "nothing is queued");
    assert_eq!(snap.live_permits, 0, "no permit leak");
    assert!(
        snap.tasks.len() <= MAX_TERMINAL_HISTORY,
        "terminal history must stay bounded: {} entries retained (cap {})",
        snap.tasks.len(),
        MAX_TERMINAL_HISTORY,
    );

    // The retained records are faithful slim views: terminal, with a
    // completion timestamp, and the most recent batch fully present
    // (eviction is oldest-first).
    for view in &snap.tasks {
        assert_eq!(view.state, TaskState::Completed);
        assert!(
            view.finished_at_ms.is_some(),
            "terminal records are timestamped"
        );
        assert_eq!(view.last_error, None);
    }
    let last_batch_start = (batches - 1) * per_batch + 1;
    for n in last_batch_start..=batches * per_batch {
        let wanted = ak_session::scheduler::TaskId::from_counter(n);
        let present = snap.tasks.iter().any(|t| t.task == wanted);
        assert!(
            present,
            "the newest batch stays resident (task {n} missing among {} views)",
            snap.tasks.len()
        );
    }

    // Long-past tasks are queryable only through durable history, not
    // through this runtime's bounded memory.
    let first = ak_session::scheduler::TaskId::from_counter(1);
    let gone = env.handle.snapshot(SnapshotQuery::one(first)).await;
    match gone {
        Ok(snap) => assert!(
            snap.tasks.is_empty(),
            "the oldest task must have been evicted from the bounded history"
        ),
        Err(SessionError::RuntimeStopped) => panic!("runtime stopped early"),
        Err(other) => panic!("unexpected snapshot error: {other:?}"),
    }
}
