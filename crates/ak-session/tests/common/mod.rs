//! Shared fixtures for the `runtime_*` integration tests.
//!
//! Everything runs on a [`FakeClock`]: the executor records per-attempt
//! `[start_ms, end_ms)` intervals in the fake time domain, so serial vs
//! overlapping execution is proven by interval arithmetic (contract §12),
//! never by sleeping real time.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use ak_session::runtime::{
    FakeClock, FakePairingTransport, FakeTransferExecutor, PairingId, PeerFingerprint,
    RuntimeConfig, SessionHandle, SessionRuntime, SessionSnapshot, SnapshotQuery, TaskInterval,
    TaskView, TransferSpec,
};
use ak_session::scheduler::{
    CodeGroupKey, CodeId, CrossCodeMode, RegistryId, SameCodeMode, SchedulerPolicy, TaskId,
    TaskState,
};

pub const REGISTRY_BYTE: u8 = 7;

/// This device's registry identity.
pub fn registry() -> RegistryId {
    RegistryId::from_bytes([REGISTRY_BYTE; 32])
}

/// Group key number `n`.
pub fn group(n: u8) -> CodeGroupKey {
    CodeGroupKey::new(registry(), CodeId::from_bytes([n; 16]))
}

/// Pairing id number `n`.
pub fn pairing(n: u8) -> PairingId {
    PairingId::from_bytes([n; 16])
}

/// Peer fingerprint number `n`.
pub fn peer(n: u8) -> PeerFingerprint {
    PeerFingerprint::from_bytes([n; 32])
}

/// Validated policy shorthand.
pub fn policy(
    same: SameCodeMode,
    cross: CrossCodeMode,
    per_code: u32,
    connections: u32,
    queued: u32,
) -> SchedulerPolicy {
    SchedulerPolicy::new(same, cross, per_code, connections, queued).unwrap()
}

/// A running runtime plus its injected fakes.
pub struct Env {
    pub clock: Arc<FakeClock>,
    pub transport: Arc<FakePairingTransport>,
    pub executor: Arc<FakeTransferExecutor>,
    pub handle: SessionHandle,
}

/// Start a runtime under `policy` with an online transport, a fake
/// executor (default `Complete { 20, 80 }`) and the contract defaults.
pub async fn env_with(policy: SchedulerPolicy) -> Env {
    env_with_config(policy, |_| {}).await
}

/// Like [`env_with`], with a hook to tune the
/// [`RuntimeConfig`](ak_session::runtime::RuntimeConfig) (retry budget,
/// shutdown timeout, listener toggle) before start.
pub async fn env_with_config(
    policy: SchedulerPolicy,
    tune: impl FnOnce(&mut RuntimeConfig),
) -> Env {
    let clock = Arc::new(FakeClock::new(1000));
    let transport = Arc::new(FakePairingTransport::new(true));
    let executor = Arc::new(FakeTransferExecutor::new(clock.clone()));
    let mut config = RuntimeConfig::new(
        policy,
        clock.clone(),
        transport.clone(),
        executor.clone(),
        registry(),
    );
    tune(&mut config);
    let handle = SessionRuntime::start(config).expect("runtime starts inside tokio");
    Env {
        clock,
        transport,
        executor,
        handle,
    }
}

/// A send-side spec with an already-established pairing and an explicit
/// task id (so per-task scripts can be pinned before submission).
pub fn send_spec(
    task: u64,
    group: CodeGroupKey,
    executor: &Arc<FakeTransferExecutor>,
) -> TransferSpec {
    let mut spec = TransferSpec::send_ready(group, pairing((task % 200) as u8), executor.clone());
    spec.task_id = Some(TaskId::from_counter(task));
    spec
}

/// Advance the fake clock in 10 ms steps (with a small real-time yield so
/// drivers and the actor get polled) until `cond` holds on a fresh
/// snapshot; panics with the last snapshot after `max_fake_ms`.
pub async fn run_until(
    env: &Env,
    mut cond: impl FnMut(&SessionSnapshot) -> bool,
    max_fake_ms: u64,
) -> SessionSnapshot {
    let mut waited = 0u64;
    loop {
        let snap = env
            .handle
            .snapshot(SnapshotQuery::all())
            .await
            .expect("snapshot");
        if cond(&snap) {
            return snap;
        }
        assert!(
            waited < max_fake_ms,
            "condition unmet after {waited} fake ms; snapshot: {snap:?}"
        );
        env.clock.advance_ms(10);
        waited += 10;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Whether every task reached a terminal state.
pub fn all_terminal(snap: &SessionSnapshot) -> bool {
    !snap.tasks.is_empty() && snap.tasks.iter().all(|t| t.state.is_terminal())
}

/// State of one task (panics when absent).
pub fn state_of(snap: &SessionSnapshot, task: TaskId) -> TaskState {
    view_of(snap, task).state
}

/// View of one task (panics when absent).
pub fn view_of(snap: &SessionSnapshot, task: TaskId) -> &TaskView {
    snap.tasks
        .iter()
        .find(|t| t.task == task)
        .unwrap_or_else(|| panic!("task {task:?} missing from snapshot"))
}

/// All finished intervals as `(task, start, end)`, sorted by start.
pub fn intervals_sorted(executor: &FakeTransferExecutor) -> Vec<(TaskId, u64, u64)> {
    let mut rows: Vec<(TaskId, u64, u64)> = executor
        .intervals()
        .into_iter()
        .map(|i| {
            (
                i.task,
                i.start_ms,
                i.end_ms
                    .expect("interval finished; call after run_until terminal"),
            )
        })
        .collect();
    rows.sort_by_key(|(_, start, _)| *start);
    rows
}

/// Assert the intervals are strictly non-overlapping (serial execution),
/// proving it by interval arithmetic rather than final success only.
pub fn assert_serial(executor: &FakeTransferExecutor, ctx: &str) {
    let rows = intervals_sorted(executor);
    assert!(
        rows.len() >= 2,
        "{ctx}: need at least two intervals, got {:?}",
        executor.intervals()
    );
    for pair in rows.windows(2) {
        let (_, start_a, end_a) = pair[0];
        let (_, start_b, end_b) = pair[1];
        assert!(
            start_b >= end_a,
            "{ctx}: intervals overlap: [{start_a},{end_a}) vs [{start_b},{end_b}) in {rows:?}"
        );
    }
}

/// Assert every pair of intervals overlaps (parallel execution).
pub fn assert_all_overlap(executor: &FakeTransferExecutor, ctx: &str) {
    let intervals: Vec<TaskInterval> = executor
        .intervals()
        .into_iter()
        .map(|mut i| {
            i.end_ms = Some(i.end_ms.expect("interval finished"));
            i
        })
        .collect();
    assert!(intervals.len() >= 2, "{ctx}: need at least two intervals");
    for a in &intervals {
        for b in &intervals {
            if a.task != b.task {
                assert!(
                    a.overlaps(b),
                    "{ctx}: intervals of {:?} and {:?} do not overlap: {a:?} vs {b:?}",
                    a.task,
                    b.task
                );
            }
        }
    }
}

/// Drive the fake clock forward while a bounded future (usually
/// `shutdown`) runs to completion. Coarse 25 ms steps: this helper only
/// needs to cross deadlines, not to observe fine-grained states.
pub async fn advance_while<F: std::future::Future>(env: &Env, fut: F) -> F::Output {
    tokio::select! {
        output = fut => output,
        _ = async {
            loop {
                env.clock.advance_ms(25);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        } => unreachable!("clock driver never completes"),
    }
}
