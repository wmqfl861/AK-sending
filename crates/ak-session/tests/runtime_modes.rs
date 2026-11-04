//! Four-mode concurrency matrix proven by activity time intervals
//! (contract §2/§12 and plan acceptance point 2: serial/overlap must be
//! shown by non-overlapping/overlapping `[start, end)` intervals, not just
//! by final success).
mod common;

use ak_session::scheduler::{CrossCodeMode, SameCodeMode};
use common::{
    all_terminal, assert_all_overlap, assert_serial, env_with, group, run_until, send_spec,
};

/// `serial`/`parallel` (default): one active task per code; codes run in
/// parallel. Three tasks of one code must execute back to back.
#[tokio::test]
async fn serial_same_code_executes_back_to_back() {
    let env = env_with(common::policy(
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
    let snap = run_until(&env, all_terminal, 2_000).await;
    assert_eq!(env.executor.start_count(), 3);
    assert_serial(&env.executor, "same-code serial");
    assert!(snap
        .tasks
        .iter()
        .all(|t| t.state == ak_session::scheduler::TaskState::Completed));
}

/// `parallel`/`parallel`: same-code tasks run concurrently up to the cap.
#[tokio::test]
async fn parallel_same_code_overlaps_up_to_cap() {
    let env = env_with(common::policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        3,
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
    run_until(&env, all_terminal, 2_000).await;
    assert_eq!(env.executor.start_count(), 3);
    assert_all_overlap(&env.executor, "same-code parallel");
}

/// `serial`/`serial`: one active code group at a time — tasks of two
/// different codes still execute strictly one after the other.
#[tokio::test]
async fn serial_cross_code_executes_one_group_at_a_time() {
    let env = env_with(common::policy(
        SameCodeMode::Serial,
        CrossCodeMode::Serial,
        4,
        4,
        64,
    ))
    .await;
    env.handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();
    env.handle
        .enqueue(send_spec(2, group(2), &env.executor))
        .await
        .unwrap();
    run_until(&env, all_terminal, 2_000).await;
    assert_eq!(env.executor.start_count(), 2);
    assert_serial(&env.executor, "cross-code serial");
}

/// `parallel`/`serial`: one active code group whose tasks overlap; another
/// code's task starts only after the whole group finishes.
#[tokio::test]
async fn parallel_same_serial_cross_overlaps_inside_group_only() {
    let env = env_with(common::policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Serial,
        2,
        1,
        64,
    ))
    .await;
    let code_a = group(1);
    let code_b = group(2);
    let t1 = env
        .handle
        .enqueue(send_spec(1, code_a, &env.executor))
        .await
        .unwrap();
    let t2 = env
        .handle
        .enqueue(send_spec(2, code_a, &env.executor))
        .await
        .unwrap();
    let t3 = env
        .handle
        .enqueue(send_spec(3, code_b, &env.executor))
        .await
        .unwrap();
    assert_ne!(t1, t2);

    let snap = run_until(&env, all_terminal, 3_000).await;
    assert_eq!(env.executor.start_count(), 3);

    // Inside group A the two intervals overlap…
    let a1 = env.executor.intervals_of(t1);
    let a2 = env.executor.intervals_of(t2);
    let b = env.executor.intervals_of(t3);
    let mut i1 = a1[0];
    i1.end_ms = Some(i1.end_ms.unwrap());
    let mut i2 = a2[0];
    i2.end_ms = Some(i2.end_ms.unwrap());
    assert!(
        i1.overlaps(&i2),
        "group A tasks must overlap: {i1:?} vs {i2:?}"
    );
    // …and group B starts only after both of them ended.
    let b_start = b[0].start_ms;
    assert!(b_start >= i1.end_ms.unwrap(), "b started before t1 ended");
    assert!(b_start >= i2.end_ms.unwrap(), "b started before t2 ended");
    assert!(snap
        .tasks
        .iter()
        .all(|t| t.state == ak_session::scheduler::TaskState::Completed));
}
