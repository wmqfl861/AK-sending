//! TEST_PLAN T03（IPv6 环回形态，L2）：外网 IPv4/IPv6 可直连 —— 双端认证
//! 通过，原件摘要一致。
//!
//! 如实边界：本文件验证的是**IPv6 环回（::1）**上的完整引擎传输与路径
//! 判定，属于 T03 的地址族补充形态。环回 ::1 **不等于**真实公网 IPv6
//! 直连——没有 NAT64/DSCP/防火墙/前缀授权等真实网络因素，本文件的成功
//! 不能被引用为"跨网络 IPv6 直连已验证"。真实双机/跨网段验证仍属 T03
//! 的未覆盖部分（见 TEST_PLAN 自动化覆盖映射）。
//!
//! 三个断言层：
//! 1. L1 决策：`classify(::1) == LocalDirect`（回环按定义本地直连）。
//! 2. L2 通道：双方各绑 `[::1]:0`，QUIC+TLS 握手成功，两侧
//!    `DirectChannel::path()` 均为 LocalDirect，指纹互钉。
//! 3. L2 引擎：`SendSpec.bind` 与 `ReceiveSpec.bind` 都设为
//!    `[::1]:0`，引擎传输完成、字节一致、接收端实际绑定地址确为 ::1
//!    （证明整条数据路径真的跑在 IPv6 环回上，而非回退到 127.0.0.1）。
#![forbid(unsafe_code)]

mod common;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use ak_core::handle::ConflictPolicy;
use ak_core::policy::PathKind;
use ak_identity::DeviceKeyPair;
use ak_net::DirectEndpoint;

use common::{
    assert_no_part_files, assert_trees_identical, try_run_transfer_opts, write_file, TransferOpts,
};

fn v6_loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
}

/// Both L2 tests below bind QUIC endpoints on `[::1]:0`; hosts without an
/// IPv6 loopback (some CI containers) must skip with the reason printed,
/// not panic (CR-P3-01).
fn ipv6_loopback_or_skip() -> bool {
    match std::net::UdpSocket::bind(v6_loopback(0)) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping: [::1] UDP bind unavailable (no IPv6 loopback on this host?): {e}");
            false
        }
    }
}

/// L1 decision layer: loopback is local-direct by definition, with or without
/// any interface knowledge; fe80::/10 link-local is local-direct too.
#[test]
fn ipv6_loopback_and_link_local_classify_local_direct() {
    assert_eq!(
        ak_net::classify::classify(IpAddr::V6(Ipv6Addr::LOCALHOST), &[]),
        PathKind::LocalDirect
    );
    assert_eq!(
        ak_net::classify::classify(
            IpAddr::V6(Ipv6Addr::from([0xfe80, 0, 0, 0, 1, 1, 1, 1])),
            &[]
        ),
        PathKind::LocalDirect
    );
    assert!(ak_net::classify::is_ipv6_link_local(Ipv6Addr::from([
        0xfe80, 0, 0, 0, 0, 0, 0, 9
    ])));
}

/// L2 channel layer: both endpoints bind `[::1]:0`, the QUIC+TLS handshake
/// succeeds over IPv6 loopback, identity pins hold, and both sides classify
/// the path LocalDirect (loopback family, not a v4 fallback).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipv6_loopback_channel_handshake_pins_identity_and_reports_local_direct() {
    if !ipv6_loopback_or_skip() {
        return;
    }
    let server_key = DeviceKeyPair::generate();
    let client_key = DeviceKeyPair::generate();
    let server_bundle = ak_identity::tls::make_self_signed(&server_key);
    let client_bundle = ak_identity::tls::make_self_signed(&client_key);

    let server = DirectEndpoint::bind(v6_loopback(0), &server_bundle)
        .await
        .expect("[::1]:0 server bind (Windows IPv6 loopback must be available)");
    let client = DirectEndpoint::bind(v6_loopback(0), &client_bundle)
        .await
        .expect("[::1]:0 client bind");

    let server_addr = server.local_addr().expect("server local addr");
    assert_eq!(server_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_ne!(server_addr.port(), 0);
    let client_addr = client.local_addr().expect("client local addr");
    assert_eq!(client_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));

    let accept_task = tokio::spawn(async move { server.accept(&|_| true).await });
    let client_channel = client
        .connect(server_addr, &server_bundle.fingerprint)
        .await
        .expect("QUIC+TLS handshake over [::1]");
    let server_channel = accept_task
        .await
        .expect("accept task panicked")
        .expect("server accept over [::1]");

    // Identity pins in both directions over the v6 path.
    assert_eq!(
        client_channel.peer_fingerprint(),
        &server_bundle.fingerprint
    );
    assert_eq!(
        server_channel.peer_fingerprint(),
        &client_bundle.fingerprint
    );
    // Loopback (v6 family) classifies as a local direct path on both sides.
    assert_eq!(client_channel.path(), PathKind::LocalDirect);
    assert_eq!(server_channel.path(), PathKind::LocalDirect);
}

/// L2 engine layer: a full engine transfer with both binds on `[::1]:0`.
/// Completes, byte-identical, no residue, and the receiver's real bound
/// address is an IPv6 loopback one — the whole data path rode ::1.
#[tokio::test]
async fn ipv6_loopback_engine_transfer_is_byte_identical() {
    if !ipv6_loopback_or_skip() {
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t03-v6-src");
    write_file(
        &src.join("readme.txt"),
        b"t03 ipv6 loopback: both engine binds are [::1]:0",
    );
    write_file(
        &src.join("blob/512k.bin"),
        &common::Lcg::new(0x3003_0000_0000_0001).bytes(512 * 1024),
    );
    write_file(
        &src.join("blob/empty-dir-marker.txt"),
        &common::Lcg::new(0x3003_0000_0000_0002).bytes(3 * 1024),
    );
    let dst = tempfile::tempdir().unwrap();

    let opts = TransferOpts {
        send_bind: v6_loopback(0),
        recv_bind: v6_loopback(0),
        ..TransferOpts::default()
    };
    let raw = try_run_transfer_opts(&src, dst.path(), 16, ConflictPolicy::Fail, 120, opts).await;
    let send_report = raw.send.expect("send over [::1] must succeed");
    let recv_report = raw.recv.expect("receive over [::1] must succeed");

    // The data path really was IPv6 loopback end to end.
    assert_eq!(raw.recv_local_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert!(
        raw.recv_local_addr.port() != 0,
        "receiver must report a concrete ephemeral port"
    );

    assert_eq!(
        send_report.files,
        common::snapshot_tree(&src).entry_count(),
        "Report.files counts manifest entries (files + dirs)"
    );
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(send_report.resumed_chunks, 0);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
}

// ---------------------------------------------------------------------------
// CR-P1-02 改法第 1 条 (REVIEW §4.1): the v1 TASK-CODE path over a
// dual-stack receiver with IPv6 candidates. Everything above drives the
// engine layer directly; the addition below drives the real product path
// — `ak-cli registry-server` + `ak-cli task code-recv` + `ak-cli task
// join-send` subprocesses — and proves (1) the receiver binds dual-stack
// and its DERIVED advertisement carries the IPv6 loopback (fe80 never
// rides), (2) a full task transfer completes through that path, and
// (3) with [::1] leading the candidate list (`--advertise ::1`, the
// unchanged manual-advertise escape hatch) the QUIC transfer genuinely
// rides the IPv6 leg ("connected via [::1]"). Same-host IPv6 loopback is
// still ::1, not a cross-network IPv6 claim — that boundary is the file
// header's.
// ---------------------------------------------------------------------------

use std::io::{BufRead, BufReader, Read, Write as IoWrite};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/ak-itests has a workspace root two levels up")
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

/// Path of the `ak-cli` executable (which also provides the
/// `registry-server` subcommand), building it once if needed.
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind to pick a free port")
        .local_addr()
        .expect("free port")
        .port()
}

/// Lines a piped stream produced so far (fed by a reader thread so a full
/// pipe can never block the child).
#[derive(Clone, Default)]
struct LineLog(Arc<Mutex<Vec<String>>>);

impl LineLog {
    fn spawn_reader(stream: impl Read + Send + 'static) -> Self {
        let log = LineLog::default();
        let sink = Arc::clone(&log.0);
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                match line {
                    Ok(text) => {
                        let mut lines = sink.lock().unwrap_or_else(|p| p.into_inner());
                        lines.push(text);
                    }
                    Err(_) => break,
                }
            }
        });
        log
    }

    fn snapshot(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// First line containing `needle`, polling until it appears.
    fn wait_for(&self, needle: &str, timeout: std::time::Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            for line in self.snapshot() {
                if line.contains(needle) {
                    return line;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no line containing {needle:?} within {timeout:?}; output so far:\n{}",
                self.snapshot().join("\n")
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// A resident CLI child with captured stdout/stderr and a WRITABLE stdin
/// (the receive consent prompt reads real terminal input).
struct Proc {
    child: Child,
    stdout: LineLog,
    stderr: LineLog,
    stdin: Option<ChildStdin>,
}

impl Proc {
    fn spawn(tag: &str, args: &[String]) -> Proc {
        let cli = cli_exe();
        let mut child = Command::new(&cli)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {tag} ({:?} {:?}): {err}", cli, args));
        let stdout = LineLog::spawn_reader(child.stdout.take().expect("stdout piped"));
        let stderr = LineLog::spawn_reader(child.stderr.take().expect("stderr piped"));
        let stdin = child.stdin.take();
        Proc {
            child,
            stdout,
            stderr,
            stdin,
        }
    }

    fn report(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            self.stdout.snapshot().join("\n"),
            self.stderr.snapshot().join("\n")
        )
    }

    /// Answer the consent prompt. The prompt itself is a `print!` without
    /// a newline (so it never forms a readable line) and the CLI DISCARDS
    /// input older than the question — so feed a fresh `y` every 100 ms
    /// from the moment the (newline-terminated) offer line appeared until
    /// the receiver moves on; a `y` that lands after the question started
    /// is genuinely fresh and accepts.
    fn consent_yes(&mut self, done_needles: &[&str], budget: std::time::Duration) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if let Some(stdin) = self.stdin.as_mut() {
                if writeln!(stdin, "y").and_then(|_| stdin.flush()).is_err() {
                    break; // the child is gone
                }
            }
            let log = self.stdout.snapshot().join("\n");
            if done_needles.iter().any(|needle| log.contains(needle)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn wait_exit_ok(&mut self, timeout: std::time::Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try wait on cli child") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "cli child exited with {status}; {}",
                        self.report()
                    );
                    return;
                }
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "cli child did not exit within {timeout:?}; {}",
                        self.report()
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }

    fn kill(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The real v2 registry as a subprocess (`ak-cli registry-server`), same
/// shape as the invite-pairing itests.
struct RegistryProc {
    /// Owning the process keeps the registry alive (killed on drop).
    #[allow(dead_code)]
    proc: Proc,
    addr: SocketAddr,
    fp_hex: String,
}

impl RegistryProc {
    fn spawn(tag: &str, server_dir: &std::path::Path, db: &std::path::Path) -> RegistryProc {
        let port = free_port().to_string();
        let args: Vec<String> = [
            "registry-server",
            "--port",
            &port,
            "--db-path",
            &db.to_string_lossy(),
            "--id-dir",
            &server_dir.to_string_lossy(),
        ]
        .iter()
        .map(|text| text.to_string())
        .collect();
        let proc = Proc::spawn(tag, &args);
        let line = proc.stdout.wait_for(
            "registry id (pin with --registry-fp): ",
            std::time::Duration::from_secs(60),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id is the last token")
            .to_ascii_lowercase();
        assert_eq!(fp_hex.len(), 64, "registry id line: {line}");
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("registry addr");
        RegistryProc { proc, addr, fp_hex }
    }
}

/// One end-to-end task-code chain: a `task code-recv` process (optionally
/// with extra `--advertise` IPs) plus a `task join-send` run, through one
/// shared registry. Returns the receiver's `receiver bound` and
/// `advertising` lines, both sides' captured stdout, for the caller's
/// assertions.
fn run_task_code_chain(
    tag: &str,
    registry: &RegistryProc,
    root: &std::path::Path,
    advertise: &[&str],
) -> (String, String, String, String) {
    let id_recv = root.join(format!("{tag}-id-recv"));
    let id_send = root.join(format!("{tag}-id-send"));
    let out = root.join(format!("{tag}-out"));
    let src = root.join(format!("{tag}-src"));
    for dir in [&id_recv, &id_send, &out, &src] {
        std::fs::create_dir_all(dir).expect("create chain dir");
    }
    common::write_file(
        &src.join("payload.bin"),
        &common::Lcg::new(0xC0DE_0000_0000_0001).bytes(256 * 1024),
    );

    let mut recv_args: Vec<String> = vec![
        "--id-dir".into(),
        id_recv.to_string_lossy().into_owned(),
        "task".into(),
        "code-recv".into(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
        "--server".into(),
        registry.addr.to_string(),
        "--registry-fp".into(),
        registry.fp_hex.clone(),
    ];
    for ip in advertise {
        recv_args.push("--advertise".into());
        recv_args.push((*ip).into());
    }
    let mut recv = Proc::spawn(&format!("{tag}-recv"), &recv_args);
    let advertised = recv
        .stdout
        .wait_for("advertising", std::time::Duration::from_secs(60));
    let bound = recv
        .stdout
        .wait_for("receiver bound", std::time::Duration::from_secs(10));

    let code_line = recv
        .stdout
        .wait_for("AKI1-", std::time::Duration::from_secs(60));
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {code_line}"));

    let send_args: Vec<String> = [
        "--id-dir".to_string(),
        id_send.to_string_lossy().into_owned(),
        "task".to_string(),
        "join-send".to_string(),
        code,
        src.to_string_lossy().into_owned(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .iter()
    .map(|text| text.to_string())
    .collect();
    let mut sender = Proc::spawn(&format!("{tag}-send"), &send_args);

    // The receiver announces the offer (a newline-terminated line), THEN
    // asks the consent question; keep feeding a fresh `y` until the
    // transfer visibly moves on.
    recv.stdout.wait_for(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );

    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));
    let sender_lines = sender.stdout.snapshot().join("\n");
    let recv_lines = recv.stdout.snapshot().join("\n");

    // The payload landed byte-identical through the task-code path.
    let landed = std::fs::read(out.join("payload.bin")).expect("received payload");
    let expected = std::fs::read(src.join("payload.bin")).expect("source payload");
    assert_eq!(landed, expected, "payload bytes differ ({tag})");
    (bound, advertised, recv_lines, sender_lines)
}

/// The v1 task-code path over the dual-stack receiver (CR-P1-02 改法第 1
/// 条): derived candidates carry [::1] (never fe80::/10), the receiver
/// binds [::] dual-stack, a full registry-paired transfer completes — and
/// with [::1] leading the list the transfer names the IPv6 leg as the
/// path that carried it. Skips with a printed reason where IPv6 (or a
/// clearable V6ONLY) is unavailable.
#[test]
fn v1_task_code_path_transfers_over_dual_stack_receiver_with_ipv6_candidates() {
    if !ipv6_loopback_or_skip() {
        return;
    }
    // The receiver must be ABLE to bind dual-stack ([::] with V6ONLY
    // cleared); [::1] alone is not proof of that (some stacks accept the
    // loopback bind but refuse the flag).
    let probe = match ak_net::holepunch::bind_dual_stack_udp(0) {
        Ok(socket) => socket,
        Err(reason) => {
            eprintln!("skipping: dual-stack UDP bind unavailable ({reason})");
            return;
        }
    };
    drop(probe);

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-task",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );

    // Chain 1 — derived advertisement: both loopback families present,
    // no link-local, dual-stack bind reported, full transfer completes.
    let (bound, advertised, _recv_log, sender_log) =
        run_task_code_chain("t03-derived", &registry, tmp.path(), &[]);
    assert!(
        bound.contains("[::]"),
        "the receiver must report the dual-stack wildcard bind: {bound}"
    );
    assert!(
        bound.contains("dual-stack"),
        "the receiver must name the bind dual-stack: {bound}"
    );
    assert!(
        advertised.contains("[::1]") && advertised.contains("127.0.0.1"),
        "the derived candidates must carry both loopback families: {advertised}"
    );
    assert!(
        !advertised.contains("fe80"),
        "link-local IPv6 must never ride the candidate list: {advertised}"
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the task-code transfer must complete: {sender_log}"
    );

    // Chain 2 — the IPv6 leg carries the payload: `--advertise ::1`
    // (the unchanged manual escape hatch) leads the list, so the winning
    // candidate is named and it is the v6 loopback.
    let (_bound2, _advertised2, _recv_log2, sender_log2) =
        run_task_code_chain("t03-v6lead", &registry, tmp.path(), &["::1"]);
    assert!(
        sender_log2.contains("[task] connected via [::1]:"),
        "the transfer must name the IPv6 loopback as the carrying path: {sender_log2}"
    );
    assert!(
        sender_log2.contains("[task] done:"),
        "the v6-led task-code transfer must complete: {sender_log2}"
    );
}

// ---------------------------------------------------------------------------
// CR-P1-02 改法第 2 条 (REVIEW §4.1): STUN server-reflexive candidates on
// the REAL product path. A minimal RFC5389 binding responder (the
// ak-signal STUN codec on a std thread) runs in the test process; the
// `task code-recv` subprocess is pointed at it with `--stun`. Three
// assertion layers:
//
// 1. Same-socket proof: a TRUTHFUL responder (echoes the observed source)
//    reflects the receiver's own socket address, so the `stun reflexive`
//    line's port MUST equal the `receiver bound` line's port — a query on
//    any throwaway socket would reflect a different ephemeral port.
// 2. Wire shape: the reflexive address rides the ADVERTISED candidate
//    list (the endpoint blob): the truthful loopback reflection dedups
//    against the derived 127.0.0.1 candidate (appears exactly once), and
//    a DISTINCT reflection (fixed 198.51.100.9:55555) is inserted after
//    the v4 interface block, before the 127.0.0.1 entry.
// 3. Fail-open: a silent server costs at most the bounded budget (2 s),
//    the receiver still binds, local candidates still advertise, and the
//    failure is logged.
// Same-host loopback STUN is not a real NAT traversal claim — no NAT sits
// on the path — that boundary stays the file header's.
// ---------------------------------------------------------------------------

/// One loopback STUN binding responder. `mapped = None` answers with the
/// OBSERVED source (truthful); `Some(addr)` answers with a fixed
/// reflection (the distinct-candidate case).
struct StunResponder {
    addr: SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl StunResponder {
    fn spawn(mapped: Option<SocketAddr>) -> StunResponder {
        let socket = match std::net::UdpSocket::bind("127.0.0.1:0") {
            Ok(socket) => socket,
            Err(error) => {
                panic!("cannot bind the test STUN responder on 127.0.0.1: {error}");
            }
        };
        let addr = socket.local_addr().expect("responder local addr");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("t03-stun-responder".to_string())
            .spawn(move || {
                socket
                    .set_read_timeout(Some(std::time::Duration::from_millis(100)))
                    .expect("responder read timeout");
                let mut buffer = [0u8; 128];
                loop {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let (len, from) = match socket.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(_) => continue, // timeout tick: re-check the flag
                    };
                    let Ok(transaction) = ak_signal::stun::parse_binding_request(&buffer[..len])
                    else {
                        continue;
                    };
                    let reply =
                        ak_signal::stun::binding_response(transaction, mapped.unwrap_or(from));
                    let _ = socket.send_to(&reply, from);
                }
            })
            .expect("spawn responder thread");
        StunResponder { addr, stop }
    }
}

impl Drop for StunResponder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Spawn one `task code-recv` with an explicit `--stun` and return the
/// process (the caller asserts on the stdout lines and drives or kills
/// it). Skips with a printed reason when loopback UDP is unavailable.
fn spawn_recv_with_stun(
    tag: &str,
    registry: &RegistryProc,
    root: &std::path::Path,
    stun: &str,
) -> Proc {
    let id_recv = root.join(format!("{tag}-id-recv"));
    let out = root.join(format!("{tag}-out"));
    for dir in [&id_recv, &out] {
        std::fs::create_dir_all(dir).expect("create recv dir");
    }
    let args: Vec<String> = [
        "--id-dir".to_string(),
        id_recv.to_string_lossy().into_owned(),
        "task".to_string(),
        "code-recv".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
        "--stun".to_string(),
        stun.to_string(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .to_vec();
    Proc::spawn(tag, &args)
}

/// The same-socket + wire-shape proof over the real CLI receiver: the
/// truthful responder's reflection carries the receiver socket's own
/// port, the advertisement dedups it against the derived loopback
/// candidate, and a full task-code transfer still completes through the
/// STUN-configured receiver.
#[test]
fn stun_reflexive_rides_the_cli_receiver_socket() {
    // Loopback UDP must be bindable for both the responder and the
    // receiver under test.
    match std::net::UdpSocket::bind("127.0.0.1:0") {
        Ok(probe) => drop(probe),
        Err(error) => {
            eprintln!("skipping: 127.0.0.1 UDP bind unavailable: {error}");
            return;
        }
    }
    let responder = StunResponder::spawn(None); // truthful echo of the source

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-stun",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let mut recv = spawn_recv_with_stun(
        "t03-stun-recv",
        &registry,
        tmp.path(),
        &responder.addr.to_string(),
    );
    let bound = recv
        .stdout
        .wait_for("receiver bound", std::time::Duration::from_secs(10));
    let reflexive_line = recv
        .stdout
        .wait_for("stun reflexive", std::time::Duration::from_secs(10));

    // (1) THE same-socket proof: the reflected port equals the bound
    // port — the query ran on the very socket QUIC then adopted (a
    // throwaway query socket would reflect its own ephemeral port).
    let reflexive_addr_text = reflexive_line
        .split("[task] stun reflexive ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("reflexive address in the line: {reflexive_line}"));
    let reflexive: SocketAddr = reflexive_addr_text
        .parse()
        .unwrap_or_else(|_| panic!("reflexive ip:port: {reflexive_addr_text}"));
    let bound_port = bound
        .split("receiver bound ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or_else(|| panic!("bound addr token in the line: {bound}"));
    assert_eq!(
        reflexive.port(),
        bound_port,
        "the STUN query must run on the socket QUIC adopts: {reflexive_line} vs {bound}"
    );
    assert_eq!(
        reflexive.ip(),
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "the truthful loopback responder reflects the local source: {reflexive_line}"
    );

    // (2) Wire shape: the reflexive address rides the advertisement, and
    // it DEDUPS against the derived 127.0.0.1 candidate (same address:
    // exactly one occurrence — the no-NAT skip rule). The needle carries
    // the "[task] " prefix: the CLI's STUN-FAILURE line also mentions
    // "advertising local candidates only".
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(10));
    let reflexive_text = format!("127.0.0.1:{}", reflexive.port());
    let occurrences = advertised
        .split_whitespace()
        .filter(|token| *token == reflexive_text)
        .count();
    assert_eq!(
        occurrences, 1,
        "the loopback reflection is the derived loopback candidate — deduped to one entry: {advertised}"
    );

    // (3) A full task-code transfer through the STUN-configured receiver.
    let code_line = recv
        .stdout
        .wait_for("AKI1-", std::time::Duration::from_secs(60));
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {code_line}"));
    let src = tmp.path().join("t03-stun-src");
    std::fs::create_dir_all(&src).expect("src dir");
    write_file(
        &src.join("payload.bin"),
        &common::Lcg::new(0x57D0_0000_0000_0001).bytes(256 * 1024),
    );
    let send_args: Vec<String> = [
        "--id-dir".to_string(),
        tmp.path()
            .join("t03-stun-id-send")
            .to_string_lossy()
            .into_owned(),
        "task".to_string(),
        "join-send".to_string(),
        code,
        src.to_string_lossy().into_owned(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .to_vec();
    let mut sender = Proc::spawn("t03-stun-send", &send_args);
    recv.stdout.wait_for(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );
    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));
    let landed = std::fs::read(tmp.path().join("t03-stun-recv-out").join("payload.bin"))
        .expect("received payload through the STUN-configured receiver");
    assert_eq!(
        landed,
        std::fs::read(src.join("payload.bin")).expect("source payload"),
        "payload bytes differ through the STUN-configured receiver"
    );
}

/// A DISTINCT reflection (the responder answers with a fixed public
/// address) is inserted into the advertisement after the v4 interface
/// block, before the 127.0.0.1 entry — the documented blob position.
#[test]
fn distinct_stun_reflection_leads_the_non_lan_candidates() {
    let fixed: SocketAddr = "198.51.100.9:55555".parse().unwrap();
    let responder = StunResponder::spawn(Some(fixed));
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-stun2",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let recv = spawn_recv_with_stun(
        "t03-stun2-recv",
        &registry,
        tmp.path(),
        &responder.addr.to_string(),
    );
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(15));
    let reflexive_at = advertised
        .find("198.51.100.9:55555")
        .unwrap_or_else(|| panic!("the distinct reflection must ride the blob: {advertised}"));
    let loopback_at = advertised.find("127.0.0.1:").unwrap_or_else(|| {
        panic!("the derived loopback candidate must ride the blob: {advertised}")
    });
    assert!(
        reflexive_at < loopback_at,
        "the reflexive candidate inserts BEFORE the loopback tail: {advertised}"
    );
}

/// Fail-open: a configured-but-silent STUN server costs at most the
/// bounded budget (2 s query + startup), the receiver still binds, local
/// candidates still advertise, and the bounded failure is logged.
#[test]
fn dead_stun_server_fails_open_within_the_budget() {
    // A bound-but-silent socket: packets land, nothing answers — the
    // deterministic timeout path on every OS (a dead port would fast-fail
    // via ICMP/ConnectionReset on Windows only).
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the silent STUN socket");
    let silent_addr = silent.local_addr().expect("silent STUN addr");
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-stun3",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let started = Instant::now();
    let recv = spawn_recv_with_stun(
        "t03-stun3-recv",
        &registry,
        tmp.path(),
        &silent_addr.to_string(),
    );
    // The bound line arriving at all proves the bind wait (10 s in the
    // child) was never hit; the explicit time bound pins the 2 s budget.
    let bound = recv
        .stdout
        .wait_for("receiver bound", std::time::Duration::from_secs(10));
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "fail-open must stay inside the budget (query budget 2 s): {elapsed:?}; {bound}"
    );
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(10));
    assert!(
        advertised.contains("127.0.0.1:"),
        "local candidates still advertise on STUN failure: {advertised}"
    );
    let failure = recv
        .stdout
        .wait_for("stun query failed", std::time::Duration::from_secs(5));
    assert!(
        !failure.contains("panicked"),
        "the failure line is bounded diagnostics, not a crash: {failure}"
    );
}

// ---------------------------------------------------------------------------
// CR-P1-02 改法第 3 条 (REVIEW §4.1): the send-time simultaneous-probe leg
// on the REAL product path. The sender's connect phase now opens with one
// probe round (one dual-stack socket, 1 s window) fired at EVERY advertised
// candidate; an observed answering address leads the QUIC dial order under
// a bounded 2 s first-attempt budget, and the static LAN-first order is
// the always-available fallback. Two assertion layers over real
// `registry-server` + `task code-recv` + `task join-send` subprocesses:
//
// 1. Fallback: the candidate list carries an address that ANSWERS PROBES
//    but holds NO QUIC listener (a probe-echoing fake, injected through
//    the unchanged `--stun` reflexive slot by a test STUN responder that
//    reflects the fake's fixed address). The sender's probe round must
//    observe it and dial it FIRST; the bounded attempt must fail over to
//    the REAL receiver and the transfer must complete with "connected
//    via" naming the receiver's own bound port.
// 2. Honest observation: in the plain derived chain the probe round
//    observes one of the receiver's REAL advertised candidates (the echo
//    responder's port — the only advertised socket that speaks the probe
//    protocol; the QUIC socket does not, an honest boundary of today's
//    product path: no single advertised address both answers probes and
//    accepts QUIC, so "connected via <observed>" is structurally
//    unreachable on this path — see the C3 report's ak-net proposal).
//
// Same-host loopback is not a NAT traversal claim — no NAT sits on the
// path — that boundary stays the file header's.
// ---------------------------------------------------------------------------

/// One UDP socket that answers ak-net hole-punch probe packets (any
/// nonce, with the all-zero reply nonce) and swallows everything else:
/// an advertised candidate that answers probes but holds NO QUIC
/// listener.
struct ProbeEchoFake {
    addr: SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl ProbeEchoFake {
    fn spawn() -> ProbeEchoFake {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the probe-echo fake");
        let addr = socket.local_addr().expect("fake local addr");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("t03-probe-echo-fake".to_string())
            .spawn(move || {
                socket
                    .set_read_timeout(Some(std::time::Duration::from_millis(100)))
                    .expect("fake read timeout");
                let mut buffer = [0u8; 1500];
                loop {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let (len, from) = match socket.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(_) => continue, // timeout tick: re-check the flag
                    };
                    if ak_net::holepunch::is_probe_packet(&buffer[..len]) {
                        // Echo with the all-zero nonce (the echo
                        // responder's reply shape — still a probe packet
                        // the sender's round counts as an observation).
                        let reply = ak_net::holepunch::probe_packet(&[0u8; 16]);
                        let _ = socket.send_to(&reply, from);
                    }
                    // Everything else (QUIC handshakes included) is
                    // dropped: this address never accepts QUIC.
                }
            })
            .expect("spawn the probe-echo fake thread");
        ProbeEchoFake { addr, stop }
    }
}

impl Drop for ProbeEchoFake {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The "[task] connected via <ip:port>" winner of one sender run.
fn connected_via_addr(sender_log: &str) -> SocketAddr {
    let line = sender_log
        .lines()
        .find(|line| line.contains("[task] connected via "))
        .unwrap_or_else(|| panic!("no connected-via line in the sender log:\n{sender_log}"));
    line.split("[task] connected via ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("connected-via address in the line: {line}"))
}

/// The port the receiver's "receiver bound [::]:<port>" line reports.
fn receiver_bound_port(bound_line: &str) -> u16 {
    bound_line
        .split("receiver bound ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or_else(|| panic!("bound addr token in the line: {bound_line}"))
}

/// The address the sender's probe-round line observed (the observed head
/// that led the dial order).
fn probe_observed_addr(sender_log: &str) -> SocketAddr {
    let line = sender_log
        .lines()
        .find(|line| line.contains("[task] probe round observed ") && !line.contains("nothing"))
        .unwrap_or_else(|| {
            panic!("no probe-round observation line in the sender log:\n{sender_log}")
        });
    line.split("[task] probe round observed ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("observed address in the line: {line}"))
}

/// Fallback layer: the advertised list carries a probe-answering fake
/// (injected as the receiver's STUN-reflexive candidate by a responder
/// that reflects the fake's fixed address). The sender's probe round
/// observes the fake and dials it FIRST; the bounded attempt fails (no
/// QUIC listener there) and the dial falls through to the REAL receiver
/// — the transfer completes and names the receiver's own bound port.
#[test]
fn probe_answered_non_quic_candidate_falls_back_to_the_real_receiver() {
    // The fake probe-echo endpoint, plus the STUN responder that reflects
    // its fixed address into the receiver's candidate list.
    let fake = ProbeEchoFake::spawn();
    let stun = StunResponder::spawn(Some(fake.addr));

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-punch",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let mut recv = spawn_recv_with_stun(
        "t03-punch-recv",
        &registry,
        tmp.path(),
        &stun.addr.to_string(),
    );
    let bound = recv
        .stdout
        .wait_for("receiver bound", std::time::Duration::from_secs(10));
    let real_port = receiver_bound_port(&bound);
    // The fake genuinely rides the advertised blob (the reflexive slot):
    // the sender's probe round probed it because it was advertised.
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(10));
    let fake_text = fake.addr.to_string();
    assert!(
        advertised.contains(&fake_text),
        "the probe-echo fake must ride the advertised candidates: {advertised}"
    );

    // The real transfer chain against that receiver.
    let code_line = recv
        .stdout
        .wait_for("AKI1-", std::time::Duration::from_secs(60));
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {code_line}"));
    let src = tmp.path().join("t03-punch-src");
    std::fs::create_dir_all(&src).expect("src dir");
    write_file(
        &src.join("payload.bin"),
        &common::Lcg::new(0x9999_0000_0000_0001).bytes(256 * 1024),
    );
    let send_args: Vec<String> = [
        "--id-dir".to_string(),
        tmp.path()
            .join("t03-punch-id-send")
            .to_string_lossy()
            .into_owned(),
        "task".to_string(),
        "join-send".to_string(),
        code,
        src.to_string_lossy().into_owned(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .to_vec();
    let mut sender = Proc::spawn("t03-punch-send", &send_args);
    recv.stdout.wait_for(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );
    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));
    let sender_log = sender.stdout.snapshot().join("\n");

    // (1) The probe round ran and observed the FAKE (a candidate that
    // answers probes but no QUIC) as the dial-leading head.
    let observed = probe_observed_addr(&sender_log);
    assert_eq!(
        observed, fake.addr,
        "the probe round must observe the probe-answering fake first: {sender_log}"
    );

    // (2) The observed-first attempt cannot carry QUIC; the fallback
    // completes the transfer via the REAL receiver's bound port.
    let winner = connected_via_addr(&sender_log);
    assert_ne!(
        winner.port(),
        fake.addr.port(),
        "the fake holds no QUIC listener — it must not carry the task: {sender_log}"
    );
    assert_eq!(
        winner.port(),
        real_port,
        "the fallback must land on the receiver's real bound port: {sender_log}"
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the task-code transfer must complete through the fallback: {sender_log}"
    );

    // (3) The payload is byte-identical through the observed-first path.
    let landed = std::fs::read(tmp.path().join("t03-punch-recv-out").join("payload.bin"))
        .expect("received payload through the probe-fallback chain");
    assert_eq!(
        landed,
        std::fs::read(src.join("payload.bin")).expect("source payload"),
        "payload bytes differ through the probe-fallback chain"
    );
}

/// Honest-observation layer (the plain derived chain): the probe round
/// observes one of the receiver's REAL advertised candidates — the echo
/// responder's port, the only advertised socket that speaks the probe
/// protocol (the QUIC socket does not) — the observed-first attempt
/// fails over within its bounded budget, and the transfer names the
/// receiver's bound port. This pins that the send-time round genuinely
/// runs against the real product path's own candidates.
#[test]
fn probe_round_observes_the_real_advertised_candidates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-obs",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let (bound, advertised, _recv_log, sender_log) =
        run_task_code_chain("t03-obs", &registry, tmp.path(), &[]);
    let real_port = receiver_bound_port(&bound);

    // The observed head is one of the receiver's genuinely ADVERTISED
    // addresses (it appears in the advertising line), and it is the echo
    // responder's port — not the QUIC port (the honest boundary: no
    // advertised address both answers probes and accepts QUIC today).
    let observed = probe_observed_addr(&sender_log);
    let observed_text = observed.to_string();
    assert!(
        advertised
            .split_whitespace()
            .any(|token| token == observed_text),
        "the probe round must observe a genuinely advertised candidate: {observed_text} not in {advertised}"
    );
    assert_ne!(
        observed.port(),
        real_port,
        "the probe-answering candidate is the echo responder's port, never the QUIC port: {sender_log}"
    );

    // The observed-first attempt fails over to the real receiver.
    let winner = connected_via_addr(&sender_log);
    assert_eq!(
        winner.port(),
        real_port,
        "the transfer must ride the receiver's real bound port: {sender_log}"
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the plain chain still completes: {sender_log}"
    );
}
