//! Invite-pairing integration tests, scheduler layer (wave 22): the
//! two-dimensional concurrency semantics of the session runtime **keyed
//! by codes that went through the real pairing plane**.
//!
//! Layering: `crates/ak-session/tests/runtime_modes.rs` proves the
//! serial/parallel matrix with synthetic group keys and `scheduler_core`
//! proves the core in isolation. These tests cross the boundary that
//! matters for the invite feature: every `CodeGroupKey` below is the
//! `(registry_id, code_id)` of a **real** invite code, claimed on a real
//! v2 registry (TLS + SQLite) and redeemed through the full frozen
//! transcript (`InviteRedeem → PairChallenge → PairProof → PairPrepared →
//! PairCommit → PairApplied → PairReady`) with a resident owner loop, and
//! every `PairingPlan::Ready` id is a real verified pairing id. The
//! executor stays the deterministic fake so serial vs overlapping
//! execution is proven by interval arithmetic (contract §12), never by
//! sleeping.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
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

async fn spawn_registry(db: &Path) -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind v2 port");
    let addr = listener.local_addr().expect("local addr");
    let store = if db.as_os_str() == ":memory:" {
        RegistryStore::open_in_memory()
    } else {
        RegistryStore::open(db)
    }
    .expect("open registry store");
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

impl Device {
    fn book_path(&self) -> PathBuf {
        self.dir.join(BOOK_NAME)
    }

    fn book_contains(&self, hex: &str) -> bool {
        std::fs::read_to_string(self.book_path())
            .map(|text| text.contains(hex))
            .unwrap_or(false)
    }
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

/// One real pairing round: `owner` claims a long-lived code and keeps a
/// resident owner loop; `redeemer` redeems it to `PairReady`. Returns the
/// code id and the verified pairing id of the completed transaction.
/// (The registry handle is unused here: both devices already hold their
/// real clients; it documents that the transaction ran on that registry.)
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
        owner.book_contains(&redeemer_hex),
        "owner book records redeemer"
    );
    assert!(
        redeemer.book_contains(&owner_hex),
        "redeemer book records owner"
    );
    (invite.code_id, verified.pairing_id())
}

// ---- runtime harness --------------------------------------------------------

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
/// pairing id (the pairing itself already completed on the real plane).
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
/// task is terminal.
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

/// Finished `[start, end)` interval of one task.
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

// ---- tests ------------------------------------------------------------------

/// Default policy is same-code serial: three tasks submitted under one
/// REAL invite code execute strictly back to back — zero time-interval
/// overlap, proven by the executor's interval arithmetic.
#[tokio::test]
async fn same_code_three_tasks_serial_zero_overlap_under_real_code() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let owner_dir = tempfile::tempdir().expect("tempdir");
    let redeemer_dir = tempfile::tempdir().expect("tempdir");
    let owner = connect_device(&registry, "owner-a", owner_dir.path().to_path_buf()).await;
    let redeemer = connect_device(&registry, "redeemer-b", redeemer_dir.path().to_path_buf()).await;
    let (code_id, pairing_id) = real_pairing(&registry, &owner, &redeemer).await;

    // The scheduling group is the real (registry, code) pair.
    let group = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code_id),
    );
    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel),
    );
    for n in 1..=3u64 {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue");
    }
    let snap = run_until_terminal(&sched, "same-code serial").await;
    assert_eq!(snap.tasks.len(), 3);
    assert_eq!(sched.executor.start_count(), 3, "each task ran once");
    assert_zero_overlap(&sched, &[1, 2, 3], "same-code serial by default");
}

/// Switching the policy to same-code parallel (the `daemon --same-code
/// parallel` path pushes exactly this switch) unlocks concurrency: three
/// MORE tasks under the SAME real code now overlap.
#[tokio::test]
async fn policy_switch_serial_to_parallel_same_code_overlaps() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let owner_dir = tempfile::tempdir().expect("tempdir");
    let redeemer_dir = tempfile::tempdir().expect("tempdir");
    let owner = connect_device(&registry, "owner-a", owner_dir.path().to_path_buf()).await;
    let redeemer = connect_device(&registry, "redeemer-b", redeemer_dir.path().to_path_buf()).await;
    let (code_id, pairing_id) = real_pairing(&registry, &owner, &redeemer).await;
    let group = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code_id),
    );

    // Phase 1: default serial — three tasks run back to back.
    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel),
    );
    for n in 1..=3u64 {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue serial round");
    }
    run_until_terminal(&sched, "serial round").await;
    assert_zero_overlap(&sched, &[1, 2, 3], "serial before the switch");

    // Phase 2: the live policy switch (SessionHandle::set_policy is what
    // the daemon flag drives) — the revision must move and the next three
    // tasks of the same code group overlap.
    let switch = sched
        .handle
        .set_policy(policy(SameCodeMode::Parallel, CrossCodeMode::Parallel))
        .await
        .expect("set_policy");
    assert!(switch.revision >= 1, "a real switch happened: {switch:?}");
    for n in 11..=13u64 {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue parallel round");
    }
    run_until_terminal(&sched, "parallel round").await;
    assert_all_overlap(
        &sched,
        &[11, 12, 13],
        "same code after switching to parallel",
    );
}

/// Multiple codes at once, default policy: two REAL codes redeemed by the
/// same device from two different owners run cross-code in parallel, while
/// each code's own tasks stay serial inside the group.
#[tokio::test]
async fn two_real_codes_run_cross_code_parallel_by_default() {
    let registry = spawn_registry(Path::new(":memory:")).await;
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

    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel),
    );
    // Two tasks per code: serial within a group, but the groups
    // themselves must interleave.
    for (n, group, pairing_id) in [
        (1u64, group1, pairing1),
        (2, group1, pairing1),
        (3, group2, pairing2),
        (4, group2, pairing2),
    ] {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue");
    }
    let snap = run_until_terminal(&sched, "cross-code parallel").await;
    assert_eq!(snap.tasks.len(), 4);

    // Within each real code group: strictly serial.
    assert_zero_overlap(&sched, &[1, 2], "code 1 group serial");
    assert_zero_overlap(&sched, &[3, 4], "code 2 group serial");
    // Across the two groups: at least one pair overlaps (with both groups
    // live from the start, the first task of each must overlap).
    let (s1, e1) = interval_of(&sched, 1);
    let (s3, e3) = interval_of(&sched, 3);
    assert!(
        s1 < e3 && s3 < e1,
        "the two code groups ran in parallel: [{s1},{e1}) vs [{s3},{e3})"
    );
}

/// The same two-code setup with cross-code switched to serial: one code
/// group at a time — ALL four intervals become strictly non-overlapping.
#[tokio::test]
async fn cross_code_switch_to_serial_serializes_the_real_groups() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner1 = connect_device(&registry, "owner-1", tmp.path().join("o1")).await;
    let owner2 = connect_device(&registry, "owner-2", tmp.path().join("o2")).await;
    let redeemer = connect_device(&registry, "redeemer", tmp.path().join("r")).await;

    let (code1, pairing1) = real_pairing(&registry, &owner1, &redeemer).await;
    let (code2, pairing2) = real_pairing(&registry, &owner2, &redeemer).await;
    let group1 = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code1),
    );
    let group2 = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(code2),
    );

    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Serial),
    );
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
    run_until_terminal(&sched, "cross-code serial").await;
    // Cross-code serial serializes everything, across groups included.
    assert_zero_overlap(&sched, &[1, 2, 3, 4], "cross-code serial after the switch");
}

/// Group semantics with two redeemers: B and C both redeem the SAME real
/// code (the contract's A↔B and A↔C case). Both transactions are distinct
/// pairings, but the scheduler groups by code: tasks carrying either
/// pairing id under that one code still serialize.
#[tokio::test]
async fn same_code_two_redeemers_share_one_serial_group() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner = connect_device(&registry, "owner-a", tmp.path().join("a")).await;
    let redeemer_b = connect_device(&registry, "redeemer-b", tmp.path().join("b")).await;
    let redeemer_c = connect_device(&registry, "redeemer-c", tmp.path().join("c")).await;

    let invite = owner.orch.create_invite(0).await.expect("claim invite");
    owner.orch.spawn_owner_loop();
    let pairing_b = redeemer_b
        .orch
        .redeem(&invite.display)
        .await
        .expect("b redeems")
        .pairing_id();
    let pairing_c = redeemer_c
        .orch
        .redeem(&invite.display)
        .await
        .expect("c redeems")
        .pairing_id();
    assert_ne!(pairing_b, pairing_c, "two distinct pairing transactions");
    // The owner's book holds both redeemers (A↔B and A↔C), and B/C never
    // learned about each other.
    let b_hex = redeemer_b.key.fingerprint().to_hex();
    let c_hex = redeemer_c.key.fingerprint().to_hex();
    assert!(owner.book_contains(&b_hex) && owner.book_contains(&c_hex));
    let b_book = std::fs::read_to_string(redeemer_b.dir.join(BOOK_NAME)).expect("b book");
    assert!(
        !b_book.contains(&c_hex),
        "redeemers of one code must not pair with each other: {b_book}"
    );

    let group = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(invite.code_id),
    );
    let sched = start_sched(
        registry.registry_id,
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel),
    );
    for (n, pairing_id) in [
        (1u64, pairing_b),
        (2, pairing_b),
        (3, pairing_c),
        (4, pairing_c),
    ] {
        sched
            .handle
            .enqueue(send_task(n, group, pairing_id, &sched.executor))
            .await
            .expect("enqueue");
    }
    run_until_terminal(&sched, "one code, two redeemers").await;
    // One code, one group: every interval serial regardless of which
    // redeemer's pairing the task carries.
    assert_zero_overlap(&sched, &[1, 2, 3, 4], "one code groups both redeemers");
}
