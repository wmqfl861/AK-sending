//! Two-end scheduler-mode observables (wave 24, review 2026-11-05
//! "最值得做的改进" #2 / "两端不同调度策略"): what the SESSION RUNTIME actually
//! enforces when the two ends of a pairing disagree about concurrency —
//! every scheduling group below is the `(registry_id, code_id)` of a REAL
//! invite code claimed and redeemed through the real v2 registry, and
//! every claim is proven by the fake executor's interval arithmetic on
//! the injected clock (contract §12), never by sleeping.
//!
//! * **Intersection tightens the loose end**: A serial/serial vs B
//!   parallel/parallel — `SchedulerPolicy::intersect` yields serial/serial
//!   with the minimum caps, AND the runtime running the intersected policy
//!   over two real code groups executes all four tasks strictly
//!   back-to-back (zero interval overlap).
//! * **Mid-flight parallel → serial switch**: two long tasks already
//!   overlapping keep running to completion (the switch reports draining,
//!   never cancels in-flight work); tasks submitted after the switch wait
//!   for the drain and then run strictly serially — proven by the
//!   interval ordering `[t3, t4) after every in-flight task ended`.
//! * **Hot code vs cold code**: one group with five queued tasks cannot
//!   starve a one-task group — group round-robin serves the cold group's
//!   task before the hot group's third slot (interval ordering), and
//!   everything completes.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ak_identity::DeviceKeyPair;
use ak_session::pairing::{PairingOrchestrator, RegistryClient, BOOK_NAME};
use ak_session::runtime::{
    CodeGroupKey, CodeId, FakeClock, FakePairingTransport, FakeTransferExecutor, PairingId,
    RegistryId, RuntimeConfig, SchedulerPolicy, SessionHandle, SessionRuntime, SessionSnapshot,
    SnapshotQuery, TaskId, TaskState, TransferSpec,
};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode};
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::Limits;
use tokio::net::TcpListener;
use tokio::sync::watch;

// ---- real-registry harness --------------------------------------------------

struct TestRegistry {
    addr: std::net::SocketAddr,
    registry_id: [u8; 32],
    shutdown: watch::Sender<bool>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn spawn_registry() -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind v2 port");
    let addr = listener.local_addr().expect("local addr");
    let store = RegistryStore::open_in_memory().expect("open registry store");
    let (shutdown, shutdown_rx) = watch::channel(false);
    let key = registry_key.clone();
    tokio::spawn(async move {
        let _ = serve_v2(listener, &key, store, Limits::default(), shutdown_rx).await;
    });
    TestRegistry {
        addr,
        registry_id: registry_key.public_key(),
        shutdown,
    }
}

/// One in-process device with a real registry client and orchestrator.
struct Device {
    key: DeviceKeyPair,
    orch: PairingOrchestrator,
    dir: PathBuf,
}

async fn connect_device(registry: &TestRegistry, name: &str, dir: PathBuf) -> Device {
    std::fs::create_dir_all(&dir).expect("create device dir");
    let key = DeviceKeyPair::generate();
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, name)
        .await
        .expect("device connects");
    let orch = PairingOrchestrator::with_defaults(client.clone(), key.clone(), &dir)
        .expect("open orchestrator");
    Device { key, orch, dir }
}

/// One real pairing round: owner claims a long-lived code (resident owner
/// loop), redeemer redeems to `PairReady`. Returns the code id and the
/// verified pairing id of the completed transaction. (The registry handle
/// is unused here: both devices already hold their real clients; it
/// documents that the transaction ran on that registry.)
async fn real_pairing(
    _registry: &TestRegistry,
    owner: &Device,
    redeemer: &Device,
) -> ([u8; 16], [u8; 16]) {
    let invite = owner.orch.create_invite(0).await.expect("claim invite");
    owner.orch.spawn_owner_loop();
    let verified = redeemer
        .orch
        .redeem(&invite.display)
        .await
        .expect("redeem reaches PairReady");
    let owner_hex = owner.key.fingerprint().to_hex();
    let redeemer_hex = redeemer.key.fingerprint().to_hex();
    assert!(
        std::fs::read_to_string(owner.dir.join(BOOK_NAME))
            .is_ok_and(|text| text.contains(&redeemer_hex)),
        "owner book records redeemer"
    );
    assert!(
        std::fs::read_to_string(redeemer.dir.join(BOOK_NAME))
            .is_ok_and(|text| text.contains(&owner_hex)),
        "redeemer book records owner"
    );
    (invite.code_id, verified.pairing_id())
}

// ---- runtime harness ----------------------------------------------------------

struct Sched {
    clock: Arc<FakeClock>,
    executor: Arc<FakeTransferExecutor>,
    handle: SessionHandle,
}

fn start_sched(registry_id: [u8; 32], policy: SchedulerPolicy) -> Sched {
    let clock = Arc::new(FakeClock::new(1000));
    let transport = Arc::new(FakePairingTransport::new(true));
    let executor = Arc::new(FakeTransferExecutor::new(clock.clone()));
    let config = RuntimeConfig::new(
        policy,
        clock.clone(),
        transport,
        executor.clone(),
        RegistryId::from_bytes(registry_id),
    );
    let handle = SessionRuntime::start(config).expect("runtime starts");
    Sched {
        clock,
        executor,
        handle,
    }
}

fn policy(same: SameCodeMode, cross: CrossCodeMode) -> SchedulerPolicy {
    // Caps wide enough that the modes, not the ceilings, decide.
    SchedulerPolicy::new(same, cross, 4, 4, 64).expect("valid policy")
}

/// A send task with an explicit id under a real code group and a real
/// pairing id (the pairing itself completed on the real plane).
fn send_task(
    n: u64,
    group: CodeGroupKey,
    pairing_id: [u8; 16],
    executor: &Arc<FakeTransferExecutor>,
) -> TransferSpec {
    let mut spec =
        TransferSpec::send_ready(group, PairingId::from_bytes(pairing_id), executor.clone());
    spec.task_id = Some(TaskId::from_counter(n));
    spec
}

/// Advance the fake clock (10 ms steps, small real yields) until every
/// task is terminal — and every task must have COMPLETED.
async fn run_until_terminal(sched: &Sched, ctx: &str) -> SessionSnapshot {
    let mut waited = 0u64;
    loop {
        let snap = sched
            .handle
            .snapshot(SnapshotQuery::all())
            .await
            .expect("snapshot");
        let all_terminal =
            !snap.tasks.is_empty() && snap.tasks.iter().all(|t| t.state.is_terminal());
        if all_terminal {
            assert!(
                snap.tasks.iter().all(|t| t.state == TaskState::Completed),
                "{ctx}: every task must complete, states: {:?}",
                snap.tasks.iter().map(|t| t.state).collect::<Vec<_>>()
            );
            return snap;
        }
        assert!(
            waited < 10_000,
            "{ctx}: tasks not terminal after {waited} fake ms; snapshot: {snap:?}"
        );
        sched.clock.advance_ms(10);
        waited += 10;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Advance until `n` tasks hold permits and are `Transferring` (the
/// in-flight state the draining switch must respect).
async fn advance_until_transferring(sched: &Sched, count: u32, ctx: &str) {
    let mut waited = 0u64;
    loop {
        let snap = sched
            .handle
            .snapshot(SnapshotQuery::all())
            .await
            .expect("snapshot");
        let transferring = snap
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Transferring)
            .count();
        if transferring >= count as usize {
            return;
        }
        assert!(
            waited < 10_000,
            "{ctx}: only {transferring}/{count} transferring after {waited} fake ms; snapshot: {snap:?}"
        );
        sched.clock.advance_ms(10);
        waited += 10;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Finished `[start, end)` interval of one task (exactly one attempt).
fn interval_of(sched: &Sched, task: u64) -> (u64, u64) {
    let id = TaskId::from_counter(task);
    let intervals = sched.executor.intervals_of(id);
    assert_eq!(
        intervals.len(),
        1,
        "task {task} ran exactly once: {intervals:?}"
    );
    let interval = intervals[0];
    (interval.start_ms, interval.end_ms.expect("interval closed"))
}

/// The listed tasks pairwise never overlap (interval arithmetic).
fn assert_zero_overlap(sched: &Sched, tasks: &[u64], ctx: &str) {
    let mut spans: Vec<(u64, u64)> = tasks.iter().map(|t| interval_of(sched, *t)).collect();
    spans.sort();
    for pair in spans.windows(2) {
        assert!(
            pair[1].0 >= pair[0].1,
            "{ctx}: intervals overlap: [{},{}) vs [{},{}) in {spans:?}",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1
        );
    }
}

/// Every listed task's interval overlaps every other's.
fn assert_all_overlap(sched: &Sched, tasks: &[u64], ctx: &str) {
    for a in tasks {
        for b in tasks {
            if a != b {
                let (a_start, a_end) = interval_of(sched, *a);
                let (b_start, b_end) = interval_of(sched, *b);
                assert!(
                    a_start < b_end && b_start < a_end,
                    "{ctx}: tasks {a} and {b} do not overlap: [{a_start},{a_end}) vs [{b_start},{b_end})"
                );
            }
        }
    }
}

// ---- tests --------------------------------------------------------------------

/// A is serial/serial (tight), B is parallel/parallel (loose): the frozen
/// intersection rule makes serial win on both dimensions and takes the
/// minimum caps — and the runtime running the INTERSECTED policy over two
/// REAL code groups (two tasks each) executes all four tasks strictly
/// back-to-back: B's loose end is tightened, observable as zero interval
/// overlap across both groups.
#[tokio::test]
async fn intersect_tightens_the_loose_end_and_the_runtime_enforces_it() {
    let registry = spawn_registry().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner1 = connect_device(&registry, "owner-1", tmp.path().join("o1")).await;
    let owner2 = connect_device(&registry, "owner-2", tmp.path().join("o2")).await;
    let redeemer = connect_device(&registry, "redeemer", tmp.path().join("r")).await;
    let (code1, pairing1) = real_pairing(&registry, &owner1, &redeemer).await;
    let (code2, pairing2) = real_pairing(&registry, &owner2, &redeemer).await;
    assert_ne!(code1, code2, "two distinct real codes");
    let group1 = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code1),
    );
    let group2 = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code2),
    );

    // The policy arithmetic: strict wins, minimums taken, symmetric.
    let strict = policy(SameCodeMode::Serial, CrossCodeMode::Serial);
    let loose = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel);
    let eff = strict.intersect(&loose);
    assert_eq!(eff.same_code_mode, SameCodeMode::Serial);
    assert_eq!(eff.cross_code_mode, CrossCodeMode::Serial);
    assert_eq!(
        eff,
        strict.intersect(&loose),
        "intersection is deterministic"
    );
    assert_eq!(
        loose.intersect(&strict),
        eff,
        "intersection is symmetric in the modes (B's loose end is tightened)"
    );
    let limits = eff.effective_limits();
    assert_eq!(limits.max_tasks_per_group, 1, "serial same-code = 1");
    assert_eq!(limits.max_active_groups, 1, "serial cross-code = 1");

    // Behavioral proof under the intersected policy: all four tasks across
    // the two real groups run strictly one after another.
    let sched = start_sched(registry.registry_id, eff);
    for (n, group, pairing_id) in [
        (1u64, group1, pairing1),
        (2, group2, pairing2),
        (3, group1, pairing1),
        (4, group2, pairing2),
    ] {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue");
    }
    let snap = run_until_terminal(&sched, "intersected serial/serial").await;
    assert_eq!(snap.tasks.len(), 4);
    assert_eq!(sched.executor.start_count(), 4, "each task ran once");
    assert_zero_overlap(&sched, &[1, 2, 3, 4], "the intersected policy serializes");
}

/// A live parallel → serial switch (the `daemon --same-code serial` shape,
/// tightened here to both dimensions): the two in-flight tasks keep their
/// overlap and complete (draining, never cancelled); tasks submitted after
/// the switch wait out the drain and then run strictly serially — the
/// interval proof that new work honors the tighter envelope.
#[tokio::test]
async fn parallel_to_serial_switch_drains_inflight_and_serializes_new_tasks() {
    let registry = spawn_registry().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner = connect_device(&registry, "owner-a", tmp.path().join("a")).await;
    let redeemer = connect_device(&registry, "redeemer-b", tmp.path().join("b")).await;
    let (code_id, pairing_id) = real_pairing(&registry, &owner, &redeemer).await;
    let group = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code_id),
    );

    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Parallel, CrossCodeMode::Parallel),
    );
    // Two LONG in-flight tasks (4000 fake ms of transfer each).
    sched.executor.set_script(
        TaskId::from_counter(1),
        ak_session::runtime::FakeScript::Complete {
            scan_ms: 50,
            transfer_ms: 4000,
        },
    );
    sched.executor.set_script(
        TaskId::from_counter(2),
        ak_session::runtime::FakeScript::Complete {
            scan_ms: 50,
            transfer_ms: 4000,
        },
    );
    for n in 1..=2u64 {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue in-flight pair");
    }
    advance_until_transferring(&sched, 2, "both long tasks running in parallel").await;

    // The live switch to serial/serial: the revision moves and the active
    // set (2) exceeds the new envelope (1) — draining.
    let switch = sched
        .handle
        .set_policy(policy(SameCodeMode::Serial, CrossCodeMode::Serial))
        .await
        .expect("set_policy");
    assert!(switch.revision >= 1, "a real switch happened: {switch:?}");
    assert!(
        switch.draining,
        "two in-flight tasks over a serial envelope drain: {switch:?}"
    );

    // New submissions while draining: accepted, queued behind the drain.
    for n in 3..=4u64 {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue behind the drain");
    }
    let snap = run_until_terminal(&sched, "drain then serial").await;
    assert_eq!(snap.tasks.len(), 4);

    // In-flight pair: kept its overlap and completed.
    assert_all_overlap(&sched, &[1, 2], "the switch never cancels in-flight work");
    // Post-switch pair: strictly serial, and strictly AFTER the drain.
    assert_zero_overlap(&sched, &[3, 4], "new tasks honor the serial envelope");
    let (end1, _) = interval_of(&sched, 1);
    let (_, end2) = interval_of(&sched, 2);
    let drain_end = end1.max(end2);
    let (start3, _) = interval_of(&sched, 3);
    let (start4, _) = interval_of(&sched, 4);
    assert!(
        start3 >= drain_end && start4 >= drain_end,
        "the new tasks waited out the drain: [s3={start3}, s4={start4}) vs drain end {drain_end}"
    );
}

/// Fairness across groups: a hot code with five queued tasks cannot
/// starve a cold code's single task — group round-robin serves the cold
/// group before the hot group's third slot, observable as the cold task's
/// interval starting strictly before the hot group's third task. Every
/// task completes exactly once.
#[tokio::test]
async fn hot_code_group_does_not_starve_the_cold_code_group() {
    let registry = spawn_registry().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner1 = connect_device(&registry, "owner-1", tmp.path().join("o1")).await;
    let owner2 = connect_device(&registry, "owner-2", tmp.path().join("o2")).await;
    let redeemer = connect_device(&registry, "redeemer", tmp.path().join("r")).await;
    let (hot_code, hot_pairing) = real_pairing(&registry, &owner1, &redeemer).await;
    let (cold_code, cold_pairing) = real_pairing(&registry, &owner2, &redeemer).await;
    let hot = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(hot_code),
    );
    let cold = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(cold_code),
    );

    // Fully serialized runtime (one group active at a time): the worst
    // case for a cold group arriving behind a hot backlog.
    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Serial),
    );
    // The whole hot backlog is queued BEFORE the cold task arrives.
    for n in 1..=5u64 {
        sched
            .handle
            .enqueue(send_task(n, hot, hot_pairing, &sched.executor))
            .await
            .expect("enqueue hot task");
    }
    sched
        .handle
        .enqueue(send_task(9, cold, cold_pairing, &sched.executor))
        .await
        .expect("enqueue cold task");

    let snap = run_until_terminal(&sched, "hot vs cold round-robin").await;
    assert_eq!(snap.tasks.len(), 6);
    assert_eq!(sched.executor.start_count(), 6, "each task ran once");

    // Round-robin proof by intervals: the cold task ran before the hot
    // group's third slot (within two group turns of its arrival), and the
    // whole run stayed serialized.
    let (cold_start, _) = interval_of(&sched, 9);
    let (hot3_start, _) = interval_of(&sched, 3);
    assert!(
        cold_start < hot3_start,
        "the cold group was served before the hot group's third task: cold@{cold_start} vs hot3@{hot3_start}"
    );
    assert_zero_overlap(&sched, &[1, 2, 3, 4, 5, 9], "serial execution throughout");
}
