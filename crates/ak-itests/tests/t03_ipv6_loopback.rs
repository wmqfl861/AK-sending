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
