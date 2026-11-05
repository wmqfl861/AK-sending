//! Cancel- and shutdown-observable tests (wave 24, review 2026-11-05
//! "最值得做的改进" #2): what the disk, the processes and the sockets look
//! like AFTER an interruption — no hidden writes, no zombie ports, no
//! collateral damage to sibling work. All engine scenarios are real QUIC
//! loopback transfers; the daemon scenario runs real `ak-cli` processes;
//! the drain scenario drives the real session runtime over a REAL pairing
//! with the injected clock.
//!
//! * **Mid-transfer cancel freezes the destination**: aborting both ends
//!   mid-flight leaves the destination tree byte-for-byte frozen — two
//!   snapshots 500 ms apart are identical, nothing new appears, and the
//!   byte count at cancel is provably below the payload (genuinely
//!   mid-flight).
//! * **Daemon killed mid-receive**: after the daemon process dies, the
//!   out-dir scan is frozen across a 500 ms window (no new files, no new
//!   parts) and the daemon never reported a job completion.
//! * **Scheduler drain shutdown**: the in-flight task COMPLETES (drain
//!   waits within its timeout, `timed_out == false`), and submissions
//!   after the shutdown are refused.
//! * **Cancel one of two transfers**: the victim's tree freezes while the
//!   survivor keeps growing and finishes byte-identical to its source.
//! * **Port release**: the receiver's UDP port and the registry's TCP
//!   port both bind again immediately after the abort/shutdown.
#![forbid(unsafe_code)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ak_core::handle::ConflictPolicy;
use ak_engine::{receive_with_addr, send, ReceiveSpec, SendSpec};
use ak_identity::DeviceKeyPair;
use ak_session::pairing::{PairingOrchestrator, RegistryClient};
use ak_session::runtime::{
    CodeGroupKey, CodeId, FakeClock, FakePairingTransport, FakeScript, FakeTransferExecutor,
    PairingId, RegistryId, RuntimeConfig, SchedulerPolicy, SessionError, SessionRuntime,
    ShutdownMode, SnapshotQuery, TaskId, TaskState, TransferSpec,
};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode};
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::Limits;
use tokio::net::TcpListener as TokioListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

// ---- engine loopback plumbing (abortable) -------------------------------------

/// One live loopback transfer: both endpoint tasks are abortable handles
/// so a test can "pull the plug" mid-flight.
struct LiveRun {
    send: JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>>,
    recv: JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>>,
    addr: SocketAddr,
    _send_ckpt: tempfile::TempDir,
    _recv_ckpt: tempfile::TempDir,
}

async fn spawn_transfer(src: &Path, dst: &Path, chunk_size: u32, in_flight_window: u32) -> LiveRun {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        // Peer placeholder; replaced by the receiver's reported address.
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_key.fingerprint(),
        sender_bundle,
    );
    send_spec.device_name = "cancel-itests-sender".into();
    send_spec.chunk_size = NonZeroU32::new(chunk_size).expect("chunk size");
    send_spec.in_flight_window = in_flight_window;
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.device_name = "cancel-itests-receiver".into();
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.conflict = ConflictPolicy::Fail;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let recv = tokio::spawn(async move { receive_with_addr(recv_spec, &|_| {}, addr_tx).await });
    let addr = addr_rx.await.expect("receiver must report its address");
    send_spec.peer = addr;
    let send = tokio::spawn(async move { send(send_spec, &|_| {}).await });

    LiveRun {
        send,
        recv,
        addr,
        _send_ckpt: send_ckpt,
        _recv_ckpt: recv_ckpt,
    }
}

impl LiveRun {
    /// Abort both endpoint tasks and wait until both futures are dropped
    /// (their sockets and files closed).
    async fn pull_the_plug(self) {
        self.recv.abort();
        let _ = self.recv.await;
        self.send.abort();
        let _ = self.send.await;
    }
}

/// Recursive name → byte-size snapshot of everything under `root` (the
/// staging tree included — it is part of the destination observable).
fn tree_snapshot(root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for child in entries {
            let Ok(child) = child else {
                continue;
            };
            let name = child.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = child.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push((path, rel));
            } else {
                out.insert(rel, meta.len());
            }
        }
    }
    out
}

fn tree_bytes(root: &Path) -> u64 {
    tree_snapshot(root).values().sum()
}

/// Poll the tree until it holds at least `threshold` payload bytes.
async fn wait_until_bytes(root: &Path, threshold: u64, what: &str) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let bytes = tree_bytes(root);
        if bytes >= threshold {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: tree never reached {threshold} bytes (at {bytes}) under {}",
            root.display()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Write a deterministic multi-MiB payload (1 MiB pattern blocks from the
/// shared LCG so the file writes stay fast).
fn write_payload(path: &Path, total_bytes: u64, seed: u64) {
    let mut lcg = common::Lcg::new(seed);
    let block = lcg.bytes(1024 * 1024);
    let mut file = fs::File::create(path).expect("create payload");
    let mut written = 0u64;
    while written < total_bytes {
        let take = (total_bytes - written).min(block.len() as u64) as usize;
        file.write_all(&block[..take]).expect("write payload block");
        written += take as u64;
    }
    file.sync_all().expect("flush payload");
}

// ---- 1: mid-transfer cancel freezes the destination ---------------------------

/// A >16 MiB transfer interrupted mid-flight (both endpoint futures
/// aborted — the process-death model): the destination tree is FROZEN
/// after the cancel — two full snapshots taken 500 ms apart are identical
/// (same names, same sizes) — and the byte count at the cancel is provably
/// below the payload, so the interruption genuinely caught it running.
/// No final file ever appeared at the destination root.
#[tokio::test]
async fn mid_transfer_cancel_freezes_the_destination_tree() {
    let base = tempfile::tempdir().expect("tempdir");
    let src = base.path().join("big-cancel.bin");
    const PAYLOAD: u64 = 32 * 1024 * 1024; // 32 MiB > 16 MiB
    write_payload(&src, PAYLOAD, 0xC0FFEE);

    let dst = tempfile::tempdir().expect("dst tempdir");
    // Window 1 + 64 KiB chunks: the sender marches one chunk at a time,
    // giving the poll loop a real window to land mid-flight.
    let run = spawn_transfer(&src, dst.path(), 64 * 1024, 1).await;

    let at_cancel = wait_until_bytes(dst.path(), 512 * 1024, "mid-flight progress").await;
    assert!(
        at_cancel < PAYLOAD,
        "the transfer must still be running at the cancel ({at_cancel} of {PAYLOAD})"
    );
    run.pull_the_plug().await;

    // Frozen: two snapshots 500 ms apart are identical.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snap1 = tree_snapshot(dst.path());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snap2 = tree_snapshot(dst.path());
    assert_eq!(
        snap1, snap2,
        "the destination tree froze at the cancel: {snap1:?} vs {snap2:?}"
    );

    // Nothing was COMMITTED to the destination: no final file exists (the
    // interrupted partial — wherever this engine path keeps it, beside the
    // final name or in the task's staging tree — never took the final
    // name), and the partial itself survives inside the frozen tree,
    // still resumable.
    assert!(
        !dst.path().join("big-cancel.bin").exists(),
        "a cancelled transfer must not commit its final file; tree: {snap2:?}"
    );
    assert!(
        snap2.keys().any(|name| name.ends_with(".akpart")),
        "the interrupted part survives for resume: {:?}",
        snap2.keys().collect::<Vec<_>>()
    );
}

// ---- 2: daemon killed mid-receive ----------------------------------------------

/// The product daemon process killed mid-receive: after the kill, a 500 ms
/// window adds no file, no part and no byte anywhere under the out dir
/// (two scans identical), and the daemon's own event stream never claimed
/// the job completed.
#[tokio::test]
async fn daemon_killed_mid_receive_leaves_a_frozen_out_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a_dir = tmp.path().join("daemon-id");
    let out_dir = tmp.path().join("out");
    fs::create_dir_all(&a_dir).expect("id dir");
    fs::create_dir_all(&out_dir).expect("out dir");

    let mut daemon = Proc::spawn(&[
        "daemon",
        "--id-dir",
        &a_dir.to_string_lossy(),
        "--out",
        &out_dir.to_string_lossy(),
        "--expect",
        "any",
        "--json-lines",
        "--name",
        "freeze-daemon",
    ]);
    let listening = daemon.wait_stdout("\"type\":\"listening\"", Duration::from_secs(30));
    let port: u16 = json_number(&listening, "port")
        .and_then(|p| u16::try_from(p).ok())
        .expect("daemon port");
    let fp = json_string(&listening, "fingerprint").expect("daemon fingerprint");

    // A big send through the real CLI sender subprocess.
    let payload = tmp.path().join("daemon-payload.bin");
    write_payload(&payload, 24 * 1024 * 1024, 0x0DAE_8000);
    let sender_dir = tmp.path().join("sender-id");
    fs::create_dir_all(&sender_dir).expect("sender id dir");
    let sender = Proc::spawn(&[
        "send",
        &payload.to_string_lossy(),
        "--to",
        &format!("127.0.0.1:{port}"),
        "--fp",
        &fp,
        "--id-dir",
        &sender_dir.to_string_lossy(),
        "--timeout",
        "120",
    ]);

    let at_kill = wait_until_bytes(&out_dir, 512 * 1024, "daemon receive progress").await;
    assert!(
        at_kill < 24 * 1024 * 1024,
        "the receive must still be running at the kill ({at_kill})"
    );
    daemon.kill();

    // Frozen out dir: no new files, parts, or bytes across 500 ms.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snap1 = tree_snapshot(&out_dir);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snap2 = tree_snapshot(&out_dir);
    assert_eq!(
        snap1, snap2,
        "no new file/part/byte after the daemon died: {snap1:?} vs {snap2:?}"
    );

    // The daemon never reported the job as done (it died mid-receive).
    assert!(
        !daemon.saw_stdout("\"type\":\"job_done\""),
        "a killed daemon must not have claimed completion; stdout so far: {:?}",
        daemon_lines(&daemon)
    );
    drop(daemon);
    drop(sender); // kill-on-drop reaps the sender process
}

fn daemon_lines(proc: &Proc) -> Vec<String> {
    proc.stdout
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

// ---- 3: scheduler drain shutdown -----------------------------------------------

/// Drain-mode shutdown on the real session runtime (a REAL pairing group
/// from the real registry): the one in-flight task runs to completion
/// inside the shutdown window (`completed == 1`, `cancelled == 0`,
/// `timed_out == false`), and a submission after the shutdown is refused.
#[tokio::test]
async fn drain_shutdown_completes_inflight_and_rejects_new_submissions() {
    // One real pairing for the scheduling group.
    let registry = spawn_test_registry().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let owner = connect_device(&registry, "owner-a", tmp.path().join("a")).await;
    let redeemer = connect_device(&registry, "redeemer-b", tmp.path().join("b")).await;
    let invite = owner.orch.create_invite(0).await.expect("claim invite");
    owner.orch.spawn_owner_loop();
    let verified = redeemer
        .orch
        .redeem(&invite.display)
        .await
        .expect("real pairing");
    let group = CodeGroupKey::new(
        RegistryId::from_bytes(registry.registry_id),
        CodeId::from_bytes(invite.code_id),
    );

    let clock = Arc::new(FakeClock::new(1000));
    let transport = Arc::new(FakePairingTransport::new(true));
    let executor = Arc::new(FakeTransferExecutor::new(clock.clone()));
    let mut config = RuntimeConfig::new(
        SchedulerPolicy::new(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64).unwrap(),
        clock.clone(),
        transport,
        executor.clone(),
        RegistryId::from_bytes(registry.registry_id),
    );
    config.shutdown_timeout_ms = 30_000;
    let handle = SessionRuntime::start(config).expect("runtime starts");

    // One long in-flight task (3000 fake ms of transfer).
    executor.set_script(
        TaskId::from_counter(1),
        FakeScript::Complete {
            scan_ms: 100,
            transfer_ms: 3000,
        },
    );
    let mut spec = TransferSpec::send_ready(
        group,
        PairingId::from_bytes(verified.pairing_id()),
        executor.clone(),
    );
    spec.task_id = Some(TaskId::from_counter(1));
    handle.enqueue(spec).await.expect("enqueue the victim");

    // Let it reach Transferring.
    let mut waited = 0u64;
    loop {
        let snap = handle
            .snapshot(SnapshotQuery::all())
            .await
            .expect("snapshot");
        if snap
            .tasks
            .iter()
            .any(|t| t.state == TaskState::Transferring)
        {
            break;
        }
        assert!(waited < 10_000, "task never started transferring: {snap:?}");
        clock.advance_ms(10);
        waited += 10;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // Drain shutdown while the clock keeps ticking (the injected-clock
    // discipline: the deadline watchdog and the task both progress on
    // explicit advances).
    let shutdown_clock = Arc::clone(&clock);
    let ticker = tokio::spawn(async move {
        loop {
            shutdown_clock.advance_ms(100);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let summary = tokio::time::timeout(
        Duration::from_secs(15),
        handle.shutdown(ShutdownMode::Drain),
    )
    .await
    .expect("drain shutdown converges")
    .expect("shutdown ok");
    ticker.abort();

    assert_eq!(summary.mode, ShutdownMode::Drain);
    assert_eq!(
        summary.completed, 1,
        "the in-flight task finished: {summary:?}"
    );
    assert_eq!(
        summary.cancelled, 0,
        "drain cancels nothing within its window"
    );
    assert!(!summary.timed_out, "the deadline never forced the finish");
    assert!(summary.listener_stopped, "the resident listener was joined");

    // New submissions after the shutdown are refused — the actor is gone.
    let mut late = TransferSpec::send_ready(
        group,
        PairingId::from_bytes(verified.pairing_id()),
        executor.clone(),
    );
    late.task_id = Some(TaskId::from_counter(2));
    let refused = handle.enqueue(late).await;
    assert!(
        matches!(
            refused,
            Err(SessionError::RuntimeStopped) | Err(SessionError::ShuttingDown)
        ),
        "post-shutdown submissions must be refused, got {refused:?}"
    );
}

// ---- 4: cancel one of two transfers ---------------------------------------------

/// Two independent transfers; the first is cancelled mid-flight while the
/// second must be untouched: the victim's tree freezes (two snapshots
/// 500 ms apart identical), the survivor's byte count at that moment is
/// below its final size (it kept growing) and it completes byte-identical
/// to its source with no part residue.
#[tokio::test]
async fn cancelling_one_of_two_transfers_leaves_the_survivor_untouched() {
    let base = tempfile::tempdir().expect("tempdir");
    let src_dir_a = base.path().join("victim-src");
    let src_dir_b = base.path().join("survivor-src");
    fs::create_dir(&src_dir_a).expect("src a dir");
    fs::create_dir(&src_dir_b).expect("src b dir");
    const PAYLOAD: u64 = 16 * 1024 * 1024; // 16 MiB each
    write_payload(&src_dir_a.join("victim.bin"), PAYLOAD, 0xA11CE);
    write_payload(&src_dir_b.join("survivor.bin"), PAYLOAD, 0x5B5B5B);

    let dst_a = tempfile::tempdir().expect("dst a");
    let dst_b = tempfile::tempdir().expect("dst b");
    let victim = spawn_transfer(&src_dir_a, dst_a.path(), 64 * 1024, 1).await;
    let survivor = spawn_transfer(&src_dir_b, dst_b.path(), 64 * 1024, 2).await;

    // Both genuinely running before the cancel.
    let a_at_cancel = wait_until_bytes(dst_a.path(), 256 * 1024, "victim progress").await;
    let b_at_cancel = wait_until_bytes(dst_b.path(), 256 * 1024, "survivor progress").await;
    assert!(a_at_cancel < PAYLOAD && b_at_cancel < PAYLOAD);

    // Cancel ONLY the victim.
    victim.pull_the_plug().await;

    // The victim's tree freezes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let frozen1 = tree_snapshot(dst_a.path());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let frozen2 = tree_snapshot(dst_a.path());
    assert_eq!(frozen1, frozen2, "the cancelled transfer's tree froze");

    // The survivor kept growing past its cancel-moment bytes and finished.
    let (recv_report, send_report) = tokio::time::timeout(Duration::from_secs(180), async {
        (survivor.recv.await, survivor.send.await)
    })
    .await
    .expect("survivor completes within the budget");
    let send_report = send_report.expect("survivor sender task").expect("send ok");
    let recv_report = recv_report
        .expect("survivor receiver task")
        .expect("receive ok");
    assert_eq!(send_report.bytes, PAYLOAD);
    assert_eq!(recv_report.bytes, PAYLOAD);
    let final_bytes = tree_bytes(dst_b.path());
    assert!(
        final_bytes > b_at_cancel,
        "the survivor kept transferring after the victim was cancelled ({final_bytes} vs {b_at_cancel})"
    );
    common::assert_trees_identical(&src_dir_b, dst_b.path());
    common::assert_no_part_files(dst_b.path());
}

// ---- 5: port release ------------------------------------------------------------

/// Interruptions do not leak sockets: after the receiver's tasks are
/// aborted, its UDP port binds again IMMEDIATELY (first try), and after
/// the registry server shuts down, its TCP port binds again within a
/// short bounded window (the async listener task needs a tick to observe
/// the shutdown).
#[tokio::test]
async fn ports_bind_again_immediately_after_cancel_and_shutdown() {
    // Engine receiver UDP port: abort → the endpoint future drops → the
    // socket closes synchronously → the very next bind succeeds.
    let base = tempfile::tempdir().expect("tempdir");
    let src = base.path().join("tiny.bin");
    fs::write(&src, b"port-release-probe").expect("write probe file");
    let dst = tempfile::tempdir().expect("dst");
    let run = spawn_transfer(&src, dst.path(), 64 * 1024, 4).await;
    let port = run.addr.port();
    run.pull_the_plug().await;
    let rebound = UdpSocket::bind(("127.0.0.1", port)).expect("UDP port rebinds immediately");
    drop(rebound);

    // Registry TCP port: in-process serve_v2 shutdown → listener dropped.
    let registry = spawn_test_registry().await;
    let tcp_port = registry.addr.port();
    registry.shutdown.send(true).expect("send shutdown");
    drop(registry);
    let mut bound = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while bound.is_none() {
        match TcpListener::bind(("127.0.0.1", tcp_port)) {
            Ok(listener) => bound = Some(listener),
            Err(err) => {
                assert!(
                    Instant::now() < deadline,
                    "registry TCP port {tcp_port} never became bindable: {err}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    drop(bound);
}

// ---- pairing harness for the drain test -----------------------------------------

struct TestRegistry {
    addr: SocketAddr,
    registry_id: [u8; 32],
    shutdown: watch::Sender<bool>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn spawn_test_registry() -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TokioListener::bind("127.0.0.1:0")
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

struct Device {
    orch: PairingOrchestrator,
}

async fn connect_device(registry: &TestRegistry, name: &str, dir: PathBuf) -> Device {
    std::fs::create_dir_all(&dir).expect("create device dir");
    let key = DeviceKeyPair::generate();
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, name)
        .await
        .expect("device connects");
    let orch = PairingOrchestrator::with_defaults(client, key, &dir).expect("open orchestrator");
    Device { orch }
}

// ---- CLI plumbing (daemon / send as real processes) ------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root two levels up")
        .to_path_buf()
}

fn cargo_bin() -> PathBuf {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return PathBuf::from(cargo);
    }
    std::env::var_os("USERPROFILE")
        .map(|home| {
            PathBuf::from(home).join(".cargo").join(if cfg!(windows) {
                "bin\\cargo.exe"
            } else {
                "bin/cargo"
            })
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn cli_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let root = workspace_root();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let exe = target_dir.join("debug").join(if cfg!(windows) {
            "ak-cli.exe"
        } else {
            "ak-cli"
        });
        let output = Command::new(cargo_bin())
            .args(["build", "-p", "ak-cli"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|err| panic!("cannot run cargo build -p ak-cli: {err}"));
        assert!(
            output.status.success(),
            "cargo build -p ak-cli failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(exe.exists(), "cargo build produced no {}", exe.display());
        exe
    })
    .clone()
}

/// A resident CLI child (killed on drop), stdout captured line-wise.
struct Proc {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
}

impl Proc {
    fn spawn(args: &[&str]) -> Proc {
        let cli = cli_exe();
        let mut child = Command::new(&cli)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("spawn ({:?} {:?}): {err}", cli, args));
        let stdout = pipe_lines(child.stdout.take().expect("stdout piped"));
        // Drain stderr into the void so a chatty child can never block.
        let _ = pipe_lines(child.stderr.take().expect("stderr piped"));
        Proc { child, stdout }
    }

    fn wait_stdout(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let lines = self
                .stdout
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            for line in lines {
                if line.contains(needle) {
                    return line;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no stdout line containing {needle:?} within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether a captured stdout line contains `needle` (no waiting).
    fn saw_stdout(&self, needle: &str) -> bool {
        self.stdout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|line| line.contains(needle))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pipe_lines(stream: impl Read + Send + 'static) -> Arc<Mutex<Vec<String>>> {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&sink);
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(text) => writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(text),
                Err(_) => break,
            }
        }
    });
    sink
}

fn json_string(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn json_number(line: &str, field: &str) -> Option<u64> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
