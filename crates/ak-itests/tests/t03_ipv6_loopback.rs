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

use common::report::{argv_summary, redact};

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

/// The port a `registry-server --port 0` bound, from its `v2 pairing
/// registry on tls <addr> (db …)` line. The server prints that line only
/// once it listens (2026-11-19 review), so no port is picked here and
/// bound later by another process, and no client starts too early.
fn announced_registry_port(line: &str) -> u16 {
    line.split("v2 pairing registry on tls ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .filter(|port| *port != 0)
        .unwrap_or_else(|| panic!("bound address in the registry line: {}", redact(line)))
}

/// Lines a piped stream produced so far, each stamped with the moment the
/// reader thread read it, fed by a reader thread so a full pipe can never
/// block the child. A stdout log also holds its process's stderr and its
/// exit status (from a watcher thread), so a timed-out wait reports all
/// three (2026-11-19 review: a CLI that fails before its first stdout line
/// says why only on stderr).
///
/// Q-N3 recurrence (2026-11-27): a report shows every line of every process
/// of the chain with milliseconds since the first of those lines, how each
/// pipe ended (closed by the writer, or a read error) and each process's
/// exit status, and never prints a task code, a registry fingerprint, a
/// pairing id, a filesystem path or an address that is not loopback.
#[derive(Clone, Default)]
struct LineLog {
    state: Arc<Mutex<LogState>>,
    stderr: Option<Arc<Mutex<LogState>>>,
    /// The process tag, for a report's heading.
    name: Option<String>,
    /// What the watcher thread concluded about the process's end.
    exit: Option<Arc<Mutex<Watched>>>,
}

/// What the watcher of a child process knows about how it ended.
#[derive(Clone, Default)]
enum Watched {
    /// Still running (or not watched at all).
    #[default]
    Running,
    Exited(std::process::ExitStatus),
    /// The watcher could not keep polling the child: its end is unknown.
    Unwatched(String),
}

/// One of a process's two pipes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

/// One line read off a pipe and the moment it was read.
#[derive(Clone)]
struct TimedLine {
    at_ms: u128,
    text: String,
}

#[derive(Default)]
struct LogState {
    lines: Vec<TimedLine>,
    /// How the pipe ended and when: `None` for the error means the writer
    /// closed it; `None` for the whole field means it is still open.
    end: Option<(u128, Option<String>)>,
}

/// The test binary's monotonic clock: every captured line is stamped with
/// milliseconds since the first process was spawned (tests in one binary
/// share it; each report then counts from its own first line).
fn since_origin_ms() -> u128 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis()
}

/// Q-N3 (reviews 19 and 20): what a report may show, on synthetic
/// lines — the secrets vanish and the cause of a fault stays.
#[test]
fn report_redaction_hides_codes_fingerprints_ids_paths_and_foreign_addresses() {
    // The task code: grouped, lowercase, ungrouped, inside `key=value`.
    assert_eq!(
        redact("join with 'AKI1-RXBM-2ZJG-GPSV-VAXH-NJ9P-CHGK-DSFZ-4V05' now"),
        "join with 'AKI1-<code>' now"
    );
    assert_eq!(
        redact("join with aki1-rxbm-2zjg-gpsv-vaxh-nj9p-chgk-dsfz-4v05 now"),
        "join with AKI1-<code> now"
    );
    assert_eq!(
        redact("code=AKI1RXBM2ZJGGPSVVAXHNJ9PCHGKDSFZ4V05;"),
        "code=AKI1-<code>;"
    );
    // A shorter run after the prefix is something else and stays.
    assert_eq!(redact("prefix AKI1-XXXX stays"), "prefix AKI1-XXXX stays");

    // The registry fingerprint, a pairing id, quotes and parentheses kept.
    assert_eq!(
        redact("fp \"3553befe995cc81c0557d05e1b99cc785810538eddd27d80b3a4be5f81768efc\""),
        "fp \"<hex>\""
    );
    assert_eq!(
        redact("owner pairing flow failed (9053fa41d556827c7bc97bc2cfe93505): x"),
        "owner pairing flow failed (<hex>): x"
    );
    // The receiver's paired line: the peer stays when it is loopback, the
    // pairing id after it does not.
    assert_eq!(
        redact("[task] paired; peer 127.0.0.1:53421 (task 9053fa41d556827c7bc97bc2cfe93505)"),
        "[task] paired; peer 127.0.0.1:53421 (task <hex>)"
    );
    assert_eq!(
        redact("pairing=9053fa41d556827c7bc97bc2cfe93505"),
        "pairing=<hex>"
    );
    // A short device prefix, times and small numbers stay.
    assert_eq!(
        redact("v2 device c805e392 online 14:40:23 0.29 s"),
        "v2 device c805e392 online 14:40:23 0.29 s"
    );
    assert_eq!(
        redact("version 0.1.0 (256.00 KiB) at 14:40:23.5"),
        "version 0.1.0 (256.00 KiB) at 14:40:23.5"
    );

    // A quoted path with spaces (a `Debug` argv): the closing quote may
    // carry the punctuation after it.
    assert_eq!(
        redact("\"--id-dir\" \"C:\\Users\\John Doe\\t03 x\\device.key\", \"task\" \"code-recv\""),
        "\"--id-dir\" \"<path>\", \"task\" \"code-recv\""
    );
    assert_eq!(
        redact("open \"C:\\lab\\a b\", cause=timeout"),
        "open \"<path>\", cause=timeout"
    );
    assert_eq!(
        redact("open \"C:\\lab\\a b\": cause=timeout"),
        "open \"<path>\": cause=timeout"
    );
    // An unquoted path with spaces, a longer one, and a relative one.
    assert_eq!(
        redact("created new device key in C:\\Users\\John Doe\\device.key"),
        "created new device key in <path>"
    );
    assert_eq!(
        redact("created new device key in C:\\Users\\A B C D E F G H\\device.key"),
        "created new device key in <path>"
    );
    assert_eq!(
        redact("created new device key in ..\\id dir two\\device.key"),
        "created new device key in <path>"
    );
    assert_eq!(
        redact("cannot read C:\\Program Files (x86)\\Common Files\\x\\device.key: missing"),
        "cannot read <path>: missing"
    );
    // The path may end inside its word or right before a flag.
    assert_eq!(
        redact("on tls 10.1.0.43:54091 (db C:\\Users\\RUNNER~1\\AppData\\registry.db); no file routes exist"),
        "on tls <addr>:54091 (db <path>); no file routes exist"
    );
    assert_eq!(
        redact("--id-dir=C:\\x y\\id --registry-fp=3553befe995cc81c0557d05e1b99cc785810538eddd27d80b3a4be5f81768efc --server 10.1.0.43:54091"),
        "--id-dir=<path> --registry-fp=<hex> --server <addr>:54091"
    );
    assert_eq!(
        redact("wrote the journal to src/main.rs and continued"),
        "wrote the journal to <path> and continued"
    );
    // A bare file name and a quoted name after a word that introduces one.
    assert_eq!(
        redact("cannot load device key in device.key: missing"),
        "cannot load device key in <path>: missing"
    );
    assert_eq!(
        redact("[task] the peer wants to send 1 file(s) (256.00 KiB under \"zz-diag-send-src\") into C:\\Users\\RUNNER~1\\AppData\\Local\\Temp"),
        "[task] the peer wants to send 1 file(s) (256.00 KiB under \"<name>\") into <path>"
    );
    assert_eq!(
        redact("(256.00 KiB under \"My Photos 2024\") into C:\\x"),
        "(256.00 KiB under \"<name>\") into <path>"
    );
    // Prose with a slash or a dot is not a path.
    assert_eq!(
        redact("accept? [Y/n] read/write and/or target/debug 0.29 s"),
        "accept? [Y/n] read/write and/or target/debug 0.29 s"
    );

    // The cause stays: a path never runs on into the next line.
    assert_eq!(
        redact("Error: cannot load device key in C:\\lab\\a b\\id\n\nCaused by:\n    No such file or directory (os error 2)"),
        "Error: cannot load device key in <path>\n\nCaused by:\nNo such file or directory (os error 2)"
    );
    assert_eq!(
        redact("in C:\\lab\\a b: The system cannot find the file specified. (os error 2)"),
        "in <path>: The system cannot find the file specified. (os error 2)"
    );
    assert_eq!(
        redact("in C:\\lab\\a b (os error 2)"),
        "in <path> (os error 2)"
    );

    // Addresses: loopback and unspecified stay with their port; others
    // keep only the port — v4, v6, bracketed with a port, with a zone id,
    // quoted, and inside `key=value`.
    assert_eq!(
        redact("bound [::1]:63049 via 127.0.0.9:51845"),
        "bound [::1]:63049 via 127.0.0.9:51845"
    );
    assert_eq!(
        redact("receiver bound [::]:53421 on 0.0.0.0:53421 via ::ffff:127.0.0.1"),
        "receiver bound [::]:53421 on 0.0.0.0:53421 via ::ffff:127.0.0.1"
    );
    assert_eq!(
        redact("stun reflexive 172.206.23.55:33858 (queried)"),
        "stun reflexive <addr>:33858 (queried)"
    );
    assert_eq!(
        redact("candidate fe80::1 and 10.1.0.62:62070"),
        "candidate <addr> and <addr>:62070"
    );
    assert_eq!(
        redact("via [2001:db8::7]:443 and [2001:db8::7]"),
        "via <addr>:443 and <addr>"
    );
    assert_eq!(
        redact("on fe80::1%12 (link-local) from '10.1.0.43:54091' peer=10.1.0.43:54091"),
        "on <addr> (link-local) from '<addr>:54091' peer=<addr>:54091"
    );

    // A spawn failure names flags and subcommands, never values: a flag
    // with a value shows the flag's name only.
    assert_eq!(
        argv_summary(&[
            "--id-dir".to_string(),
            "C:\\x y\\id".to_string(),
            "task".to_string(),
            "join-send".to_string(),
            "AKI1-RXBM-2ZJG-GPSV-VAXH-NJ9P-CHGK-DSFZ-4V05".to_string(),
            "C:\\p".to_string(),
            "--registry-fp=3553befe995cc81c0557d05e1b99cc785810538eddd27d80b3a4be5f81768efc"
                .to_string(),
            "-v".to_string(),
            "photos".to_string(),
        ]),
        "--id-dir <value> task join-send <value> <value> --registry-fp=<value> <value> <value>"
    );
}

/// A synthetic task code in both forms the parser takes (the symbols are
/// from the Crockford alphabet, as a real one's are).
fn synthetic_code() -> (&'static str, &'static str) {
    (
        "AKI1-0Z9Y-0Z9Y-0Z9Y-0Z9Y-0Z9Y-0Z9Y-0Z9Y-0Z9Y",
        "AKI10Z9Y0Z9Y0Z9Y0Z9Y0Z9Y0Z9Y0Z9Y0Z9Y",
    )
}

/// The text a closure panicked with.
fn panic_text(run: impl FnOnce()) -> String {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .expect_err("the assertion must fail");
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    panic!("the panic carried neither a String nor a &str");
}

/// Plan of 2026-11-28 (after review 21), section A: the word shapes the
/// redaction missed. One table, and every input is redacted on its own, so
/// one run reports every leak, swallowed cause or second panic.
#[test]
fn redaction_covers_the_word_shapes_review_21_found() {
    let (code, ungrouped) = synthetic_code();
    let long_hex = "f".repeat(32);
    let cases: Vec<(String, String)> = vec![
        ("（说明）".into(), "（说明）".into()),
        ("（192.0.2.7:41）".into(), "（<addr>:41）".into()),
        ("under “Alpha,Beta”".into(), "under “<name>”".into()),
        (
            "no candidate accepted the connection (192.0.2.7:41: permission denied)".into(),
            "no candidate accepted the connection (<addr>:41: permission denied)".into(),
        ),
        (format!("{code}q"), "AKI1-<code>q".into()),
        (format!("{ungrouped}-suffix"), "AKI1-<code>-suffix".into()),
        (
            format!("{}abc", code.to_ascii_lowercase()),
            "AKI1-<code>abc".into(),
        ),
        (format!("face{code}"), "faceAKI1-<code>".into()),
        (format!("{long_hex}{code}"), "<hex>AKI1-<code>".into()),
        ("under \"Alpha,Beta\"".into(), "under \"<name>\"".into()),
        (
            "under \"Alpha, Beta\", failed".into(),
            "under \"<name>\", failed".into(),
        ),
        ("open \"/lab/Alpha,Beta\"".into(), "open \"<path>\"".into()),
        ("in /private".into(), "in <path>".into()),
        (
            "in /var/cache/x permission denied".into(),
            "in <path> permission denied".into(),
        ),
        (
            "in /lab/a b Permission denied (os error 13)".into(),
            "in <path> Permission denied (os error 13)".into(),
        ),
        (
            "in /lab/a permission denied/leaf".into(),
            "in <path>".into(),
        ),
        ("in /lab/a unclassified words".into(), "in <path>".into()),
    ];
    let mut wrong = Vec::new();
    for (input, expected) in &cases {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| redact(input))) {
            Ok(actual) if actual == *expected => {}
            Ok(actual) => wrong.push(format!("{input:?}: expected {expected:?}, got {actual:?}")),
            Err(_) => wrong.push(format!("{input:?}: panicked")),
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// Plan of 2026-11-28, section A: a failing assertion on an external
/// address shows the reason and nothing raw — neither the address nor the
/// `left` / `right` an `assert_eq!` adds by itself.
#[test]
fn a_failing_assertion_on_an_external_address_shows_nothing_raw() {
    let external_v4: SocketAddr = "192.0.2.7:41".parse().unwrap();
    let external_v6: SocketAddr = "[2001:db8::7]:41".parse().unwrap();
    let fake: SocketAddr = "127.0.0.9:53422".parse().unwrap();
    let advertised = "[task] advertising 2 candidate(s): 127.0.0.1:53421 [::1]:53421";
    let sender_log = "[send] trying 1/2: 192.0.2.7:41...\n[send] connected; peer 127.0.0.1:53421";
    let reflexive_line = "[task] stun reflexive 192.0.2.7:41 (queried on the sender socket)";
    let texts = [
        panic_text(|| assert_observed_head_is_advertised(advertised, external_v4)),
        panic_text(|| assert_observed_head_is_advertised(advertised, external_v6)),
        panic_text(|| assert_observed_head_is_fake_or_real(external_v4, fake, 53421, sender_log)),
        panic_text(|| {
            assert_reflexive_is_loopback(external_v4, reflexive_line, "the reflexive address")
        }),
        panic_text(|| {
            assert_reflexive_is_loopback(external_v6, reflexive_line, "the reflexive address")
        }),
    ];
    for text in &texts {
        for raw in ["192.0.2.7", "2001:db8::7", "left:", "right:"] {
            assert!(!text.contains(raw), "the panic text shows {raw:?}:\n{text}");
        }
    }
    assert!(texts[0].contains("genuinely advertised"), "{}", texts[0]);
    assert!(
        texts[2].contains("fake or the punch-revealed"),
        "{}",
        texts[2]
    );
    assert!(texts[3].contains("the reflexive address"), "{}", texts[3]);
}

/// Plan of 2026-11-28, section A: the whole panic text of test 1's UDP
/// failure on a synthetic report — the secrets are gone, the cause stays,
/// and the report itself survives the wide punctuation the CLI prints.
#[test]
fn the_udp_failure_report_hides_secrets_and_keeps_the_cause() {
    let (code, _) = synthetic_code();
    let fingerprint = "9f".repeat(32);
    let receiver = LineLog::synthetic(
        "receiver",
        &[
            format!("[task] paired; peer 192.0.2.7:53421 (task {fingerprint})"),
            format!("the peer joins with: ak-cli task join-send '{code}' <PATHS...>"),
        ],
        &[
            "created new device key in C:\\Users\\Public User\\t03 x\\device.key".to_string(),
            "无法加载/新建 device.key（C:\\Users\\Public User\\t03 x\\device.key）：系统找不到指定的文件。 (os error 2)".to_string(),
        ],
        false,
        Watched::Running,
    );
    let sender = LineLog::synthetic(
        "sender",
        &[],
        &["[task] joining the task code at the registry...".to_string()],
        false,
        Watched::Running,
    );
    let registry = LineLog::synthetic(
        "registry",
        &[],
        &[
            format!("v2 registry server listening on tls 192.0.2.7:54364 (registry {fingerprint})"),
            "（192.0.2.7:41）".to_string(),
        ],
        false,
        Watched::Running,
    );
    let chain = [
        ("receiver", &receiver),
        ("sender", &sender),
        ("registry", &registry),
    ];
    let joiner_exit = JoinerExit {
        at_ms: since_origin_ms(),
        status: "exit code: 1".to_string(),
    };
    let text = panic_text(|| {
        assert_punch_arrived(
            &PunchWatch::default(),
            53421,
            317,
            since_origin_ms() + 30_000,
            Some(&joiner_exit),
            64169,
            "receiver",
            &chain,
        )
    });
    for raw in [
        code,
        fingerprint.as_str(),
        "Public User",
        "192.0.2.7",
        "char boundary",
    ] {
        assert!(!text.contains(raw), "the report shows {raw:?}:\n{text}");
    }
    for kept in [
        "系统找不到指定的文件",
        "(os error 2)",
        "watching from +317 ms",
        "no end line yet",
    ] {
        assert!(text.contains(kept), "the report lost {kept:?}:\n{text}");
    }
    for placeholder in ["<addr>:53421", "<path>", "AKI1-<code>", "<hex>"] {
        assert!(
            text.contains(placeholder),
            "the report lacks {placeholder:?}:\n{text}"
        );
    }
    // Each process once.
    for heading in ["--- receiver (", "--- sender (", "--- registry ("] {
        assert_eq!(text.matches(heading).count(), 1, "{heading} in:\n{text}");
    }
}

/// How long a report waits for the last lines of a process that already
/// exited: they can still be in the pipes when its exit is first seen
/// (review 20), and the first error is often the last line.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1000);

/// Wait, at most [`SETTLE`], until every process here that already exited
/// has nothing left coming out of its pipes. A process still running is
/// not waited for.
fn settle(logs: &[&LineLog]) {
    let until = Instant::now() + SETTLE;
    while Instant::now() < until {
        let pending = logs
            .iter()
            .any(|log| log.exit().is_some() && !log.pipes_ended());
        if !pending {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `logs` with every process once: a log is left out when one of its
/// pipes is a pipe of a log already kept (the bare stderr log of a process
/// whose stdout log is listed). A report shows each process once, however
/// the wait that produced it was phrased.
fn unique_processes<'a>(logs: &[(&'a str, &'a LineLog)]) -> Vec<(&'a str, &'a LineLog)> {
    fn pipes(log: &LineLog) -> impl Iterator<Item = &Arc<Mutex<LogState>>> {
        std::iter::once(&log.state).chain(log.stderr.as_ref())
    }
    let mut kept: Vec<(&str, &LineLog)> = Vec::new();
    for &(name, log) in logs {
        let same = |state: &Arc<Mutex<LogState>>| {
            kept.iter()
                .any(|(_, seen)| pipes(seen).any(|pipe| Arc::ptr_eq(pipe, state)))
        };
        if !pipes(log).any(same) {
            kept.push((name, log));
        }
    }
    kept
}

/// One process as a report froze it.
struct FrozenProcess {
    name: String,
    out: Vec<TimedLine>,
    err: Vec<TimedLine>,
    /// How each pipe ended and when: the error is `None` when the writer
    /// closed the pipe, the whole field `None` while it is still open.
    out_end: Option<(u128, Option<String>)>,
    err_end: Option<(u128, Option<String>)>,
    exit: Watched,
}

/// Every process of a chain, taken once for one report (plan of 2026-11-28,
/// after review 21): the last lines of processes that already exited are
/// waited for, then every pipe, every exit status and the common origin
/// are frozen, and the registry database is sampled once. A summary, the
/// details and whatever a test decides all read this one copy, so they
/// cannot disagree. Lines that only arrived during the wait explain a
/// failure; they never turn it into a success.
struct Frozen {
    origin: u128,
    processes: Vec<FrozenProcess>,
    snapshot: String,
    snapshot_at_ms: u128,
}

fn freeze(logs: &[(&str, &LineLog)]) -> Frozen {
    let processes = unique_processes(logs);
    settle(&processes.iter().map(|(_, log)| *log).collect::<Vec<_>>());
    let frozen: Vec<FrozenProcess> = processes
        .iter()
        .map(|(name, log)| {
            let out = log
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let err = log.stderr.as_ref().map(|state| {
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            FrozenProcess {
                name: name.to_string(),
                out: out.lines.clone(),
                err: err
                    .as_ref()
                    .map(|err| err.lines.clone())
                    .unwrap_or_default(),
                out_end: out.end.clone(),
                err_end: err.as_ref().and_then(|err| err.end.clone()),
                exit: log.watched(),
            }
        })
        .collect();
    let origin = frozen
        .iter()
        .flat_map(|process| [&process.out, &process.err])
        .filter_map(|lines| lines.first())
        .map(|line| line.at_ms)
        .min()
        .unwrap_or_else(since_origin_ms);
    Frozen {
        origin,
        processes: frozen,
        snapshot: registry_snapshot(),
        snapshot_at_ms: since_origin_ms(),
    }
}

impl Frozen {
    fn process(&self, name: &str) -> Option<&FrozenProcess> {
        self.processes.iter().find(|process| process.name == name)
    }

    /// The report's text: each process once, as a timeline of both pipes
    /// counted from the first line any of them wrote, then the registry
    /// database with the moment it was sampled.
    fn render(&self) -> String {
        let mut report = String::new();
        for process in &self.processes {
            report.push_str(&format!(
                "\n--- {} (milliseconds since the first line of this report):\n",
                process.name
            ));
            report.push_str(&process.timeline(self.origin).join("\n"));
        }
        report.push_str(&format!(
            "\n--- registry database, read-only snapshot at +{} ms:\n{}",
            self.snapshot_at_ms.saturating_sub(self.origin),
            self.snapshot
        ));
        report
    }
}

impl FrozenProcess {
    /// The lines of both pipes, redacted, each with milliseconds since
    /// `origin` and the stream it came from; then how each pipe ended and
    /// the exit status (still-open pipes last).
    fn timeline(&self, origin: u128) -> Vec<String> {
        let mut rows: Vec<(u128, String)> = Vec::new();
        for (state, ended, tag) in [
            (&self.out, &self.out_end, "out"),
            (&self.err, &self.err_end, "err"),
        ] {
            for line in state {
                rows.push((
                    line.at_ms,
                    format!(
                        "[+{:>7} ms] {tag}: {}",
                        line.at_ms.saturating_sub(origin),
                        redact(&line.text)
                    ),
                ));
            }
            match ended {
                Some((at, None)) => rows.push((
                    *at,
                    format!(
                        "[+{:>7} ms] {tag}: <pipe closed by the writer>",
                        at.saturating_sub(origin)
                    ),
                )),
                Some((at, Some(error))) => rows.push((
                    *at,
                    format!(
                        "[+{:>7} ms] {tag}: <pipe lost on a read error: {error}>",
                        at.saturating_sub(origin)
                    ),
                )),
                None => rows.push((u128::MAX, format!("[+      ? ms] {tag}: <pipe still open>"))),
            }
        }
        match &self.exit {
            Watched::Exited(status) if status.success() => {
                rows.push((u128::MAX, format!("exit status: {status}")))
            }
            Watched::Exited(status) => rows.push((u128::MAX - 1, format!("exit status: {status}"))),
            Watched::Unwatched(error) => rows.push((
                u128::MAX - 1,
                format!("exit status: unknown; the watcher could not poll the child: {error}"),
            )),
            Watched::Running => rows.push((
                u128::MAX,
                "exit status: <process still running>".to_string(),
            )),
        }
        rows.sort_by_key(|(at, _)| *at);
        rows.into_iter().map(|(_, text)| text).collect()
    }

    fn out_text(&self) -> String {
        Self::text_of(&self.out)
    }

    fn err_text(&self) -> String {
        Self::text_of(&self.err)
    }

    fn text_of(lines: &[TimedLine]) -> String {
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl LineLog {
    fn spawn_reader(stream: impl Read + Send + 'static) -> Self {
        let log = LineLog::default();
        let sink = Arc::clone(&log.state);
        std::thread::spawn(move || {
            let mut read_error = None;
            for line in BufReader::new(stream).lines() {
                match line {
                    Ok(text) => {
                        let at_ms = since_origin_ms();
                        let mut state = sink.lock().unwrap_or_else(|p| p.into_inner());
                        state.lines.push(TimedLine { at_ms, text });
                    }
                    Err(error) => {
                        read_error = Some(error.to_string());
                        break;
                    }
                }
            }
            // The pipe ended: closed by the writer, or on a read error.
            let mut state = sink.lock().unwrap_or_else(|p| p.into_inner());
            state.end = Some((since_origin_ms(), read_error));
        });
        log
    }

    /// Report `stderr` (the same process's other stream) on a timeout.
    fn with_stderr(mut self, stderr: &LineLog) -> Self {
        self.stderr = Some(Arc::clone(&stderr.state));
        self
    }

    fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    fn watched_by(mut self, exit: &Arc<Mutex<Watched>>) -> Self {
        self.exit = Some(Arc::clone(exit));
        self
    }

    /// A log that no pipe feeds: the caller states its lines, whether its
    /// pipes ended, and what the watcher concluded — for synthetic reports.
    fn synthetic(
        name: &str,
        out: &[String],
        err: &[String],
        ended: bool,
        exit: Watched,
    ) -> LineLog {
        let now = since_origin_ms();
        let state = |lines: &[String]| -> Arc<Mutex<LogState>> {
            Arc::new(Mutex::new(LogState {
                lines: lines
                    .iter()
                    .map(|text| TimedLine {
                        at_ms: now,
                        text: text.clone(),
                    })
                    .collect(),
                end: ended.then_some((now, None)),
            }))
        };
        LineLog {
            state: state(out),
            stderr: Some(state(err)),
            name: Some(name.to_string()),
            exit: Some(Arc::new(Mutex::new(exit))),
        }
    }

    /// What the watcher concluded so far.
    fn watched(&self) -> Watched {
        self.exit
            .as_ref()
            .map(|watched| watched.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or(Watched::Running)
    }

    /// The lines of `stream` so far, for the callers that parse them.
    fn lines_of(&self, stream: Stream) -> Vec<TimedLine> {
        let state = match stream {
            Stream::Stdout => &self.state,
            Stream::Stderr => self.stderr.as_ref().unwrap_or(&self.state),
        };
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lines
            .clone()
    }

    /// This stream's raw lines so far.
    fn snapshot(&self) -> Vec<String> {
        self.lines_of(Stream::Stdout)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// The process's exit status once the watcher saw it.
    fn exit(&self) -> Option<std::process::ExitStatus> {
        match self.watched() {
            Watched::Exited(status) => Some(status),
            _ => None,
        }
    }

    /// When the first line of this log or its stderr was read.
    fn earliest_ms(&self) -> Option<u128> {
        [Stream::Stdout, Stream::Stderr]
            .into_iter()
            .filter_map(|stream| self.lines_of(stream).first().map(|line| line.at_ms))
            .min()
    }

    /// Both pipes ended (closed or on a read error).
    fn pipes_ended(&self) -> bool {
        let ended = |state: &Arc<Mutex<LogState>>| {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .end
                .is_some()
        };
        let err = self.stderr.as_ref().map(ended).unwrap_or(true);
        ended(&self.state) && err
    }

    /// The process is over and did not end well: it exited unsuccessfully,
    /// or both its pipes ended after an error line — one condition or the
    /// other, whichever the watcher and the reader threads report first.
    /// An exit status of 0 does not settle it by itself: a process can
    /// print its error and exit 0. It cannot produce the line a wait is
    /// looking for, so the wait stops and reports now.
    fn ended_with_error(&self) -> bool {
        let exited_badly = matches!(self.watched(), Watched::Exited(status) if !status.success());
        exited_badly || (self.pipes_ended() && self.has_error_line())
    }

    /// `Error:` or a panic somewhere in this log or its stderr.
    fn has_error_line(&self) -> bool {
        let has = |state: &Arc<Mutex<LogState>>| {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .lines
                .iter()
                .any(|line| line.text.contains("Error:") || line.text.contains("panicked at"))
        };
        has(&self.state) || self.stderr.as_ref().is_some_and(has)
    }

    /// This process as one timeline counted from its earliest line. The
    /// last lines of a process that already exited are waited for (see
    /// [`settle`]).
    fn report(&self) -> String {
        freeze(&[(self.name.as_deref().unwrap_or("this process"), self)]).render()
    }

    /// First line containing `needle`, polling until it appears.
    fn wait_for(&self, needle: &str, timeout: std::time::Duration) -> String {
        self.wait_for_stream(Stream::Stdout, needle, timeout, &[])
    }

    /// [`Self::wait_for`], with `others`' output (stderr included) in the
    /// timeout report too: a wait on one process often runs out because
    /// of another (2026-11-20 review of batch Q: a Windows CI run timed
    /// out waiting for the receiver's offer line, and the report held only
    /// the receiver's side).
    fn wait_for_reporting(
        &self,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        self.wait_for_stream(Stream::Stdout, needle, timeout, others)
    }

    /// [`Self::wait_for`] on one stream of this process. The stream only
    /// decides where the line is looked for; the report, when the wait
    /// gives up, always comes from this process — both pipes, one exit
    /// status — and from `others`, each process once. The wait stops as
    /// soon as this process or one of `others` is over and did not end
    /// well (Q-N3, 2026-11-27): the registry is still alive then, so its
    /// log and a read-only snapshot of its database go into the report.
    /// A line that was read after the deadline shows in that report and
    /// does not count as the line having come in time.
    fn wait_for_stream(
        &self,
        stream: Stream,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        let deadline = Instant::now() + timeout;
        let deadline_ms = since_origin_ms() + timeout.as_millis();
        loop {
            if let Some(line) = self
                .lines_of(stream)
                .into_iter()
                .find(|line| line.at_ms <= deadline_ms && line.text.contains(needle))
            {
                return line.text;
            }
            let self_ended = self.ended_with_error();
            let other_ended = others
                .iter()
                .find(|(_, other)| other.ended_with_error())
                .map(|(name, _)| *name);
            let now = Instant::now();
            if now >= deadline || self_ended || other_ended.is_some() {
                let mut who: Vec<String> = Vec::new();
                if self_ended {
                    who.push(self.name.as_deref().unwrap_or("this process").to_string());
                }
                if let Some(name) = other_ended {
                    who.push(name.to_string());
                }
                let why = if who.is_empty() {
                    format!("waited the full {timeout:?}")
                } else {
                    format!(
                        "stopped early: {} had already ended without success",
                        who.join(" and ")
                    )
                };
                let stream = match stream {
                    Stream::Stdout => "stdout",
                    Stream::Stderr => "stderr",
                };
                let mut logs: Vec<(&str, &LineLog)> =
                    vec![(self.name.as_deref().unwrap_or("this process"), self)];
                logs.extend_from_slice(others);
                panic!(
                    "no line of the {stream} containing {needle:?} ({why}){}",
                    freeze(&logs).render()
                );
            }
            std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(50)));
        }
    }
}

thread_local! {
    /// The database of the registry this test thread started last, for the
    /// snapshot in a failure report.
    static REGISTRY_DB: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// `scripts/t03_pairing_snapshot.py` on the registry database, or the reason
/// it was not taken. The interpreter is `AK_T03_PYTHON` when set, else
/// `python` / `python3` from the PATH.
fn registry_snapshot() -> String {
    let Some(db) = REGISTRY_DB.with(|slot| slot.borrow().clone()) else {
        return "not taken: no registry database was registered on this thread".into();
    };
    let python: std::ffi::OsString = std::env::var_os("AK_T03_PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let script = workspace_root()
        .join("scripts")
        .join("t03_pairing_snapshot.py");
    let started = Instant::now();
    match Command::new(&python)
        .arg("-I")
        .arg(&script)
        .arg(&db)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(output) => format!(
            "not taken: exit status {} after {:?}; stderr: {}",
            output.status,
            started.elapsed(),
            redact(&String::from_utf8_lossy(&output.stderr))
        ),
        Err(error) => format!("not taken: cannot start the interpreter ({error})"),
    }
}

/// What every CLI child of a chain runs with (Q-N3, 2026-11-27): the
/// endpoints' pairing trace on stderr (`AK_PAIR_DIAG` turns it on inside
/// `ak-cli task`; `RUST_LOG` keeps everything else at warnings).
const PAIR_DIAG_ENV: &[(&str, &str)] = &[
    ("AK_PAIR_DIAG", "1"),
    ("RUST_LOG", "warn,ak_pair_diag=info"),
];

/// A resident CLI child with captured stdout/stderr and a WRITABLE stdin
/// (the receive consent prompt reads real terminal input). A watcher
/// thread records its exit status, so a wait can tell "ended with status
/// N" from "closed its pipes" (Q-N3).
struct Proc {
    /// Behind a lock: the watcher thread polls it, `kill` ends it.
    child: Arc<Mutex<Child>>,
    stdout: LineLog,
    #[allow(dead_code)]
    stderr: LineLog,
    stdin: Option<ChildStdin>,
}

impl Proc {
    fn spawn(tag: &str, args: &[String]) -> Proc {
        Proc::spawn_with_env(tag, args, PAIR_DIAG_ENV)
    }

    /// [`Self::spawn`] with extra environment variables for the child.
    fn spawn_with_env(tag: &str, args: &[String], env: &[(&str, &str)]) -> Proc {
        let cli = cli_exe();
        let mut command = Command::new(&cli);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {tag} ({}): {err}", argv_summary(args)));
        let child = Arc::new(Mutex::new(child));
        let stderr = LineLog::spawn_reader(
            child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .stderr
                .take()
                .expect("stderr piped"),
        )
        .named(&format!("{tag} (stderr)"));
        let exit: Arc<Mutex<Watched>> = Arc::new(Mutex::new(Watched::Running));
        let stdout = LineLog::spawn_reader(
            child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .stdout
                .take()
                .expect("stdout piped"),
        )
        .with_stderr(&stderr)
        .named(tag)
        .watched_by(&exit);
        let stdin = child.lock().unwrap_or_else(|p| p.into_inner()).stdin.take();
        let watched = Arc::clone(&child);
        let tag = tag.to_string();
        std::thread::spawn(move || loop {
            let status = {
                let mut child = watched.lock().unwrap_or_else(|p| p.into_inner());
                child.try_wait()
            };
            match status {
                Ok(Some(status)) => {
                    *exit.lock().unwrap_or_else(|p| p.into_inner()) = Watched::Exited(status);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(error) => {
                    eprintln!("t03: cannot poll the child {tag}: {error}");
                    // The report says the end is unknown, not "still running".
                    *exit.lock().unwrap_or_else(|p| p.into_inner()) =
                        Watched::Unwatched(error.to_string());
                    break;
                }
            }
        });
        Proc {
            child,
            stdout,
            stderr,
            stdin,
        }
    }

    /// The exit status once the watcher saw it.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.stdout.exit()
    }

    /// Wait for a line of one stream of this process; the report, when
    /// the wait gives up, is this process's (both pipes, one exit status)
    /// and `others`'. See [`LineLog::wait_for_stream`].
    fn wait_for_line(
        &self,
        stream: Stream,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        self.stdout.wait_for_stream(stream, needle, timeout, others)
    }

    /// This process's stdout and stderr as one timeline.
    fn report(&self) -> String {
        let mut text = format!("{}:", self.stdout.name.as_deref().unwrap_or("process"));
        for line in self.stdout.report().split('\n') {
            text.push('\n');
            text.push_str(line);
        }
        text
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

    /// Wait until the child exited, and it exited successfully. Its last
    /// lines are read in first: they can still be in the pipes when the
    /// exit is first seen, and the caller reads the log right after this.
    fn wait_exit_ok(&mut self, timeout: std::time::Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.exit_status() {
                settle(&[&self.stdout]);
                assert!(
                    status.success(),
                    "cli child exited with {status}; {}",
                    self.report()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "cli child did not exit within {timeout:?}; {}",
                self.report()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn kill(&mut self) {
        self.stdin.take();
        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        let _ = child.kill();
        let _ = child.wait();
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
        let args: Vec<String> = [
            "registry-server",
            "--port",
            "0",
            "--db-path",
            &db.to_string_lossy(),
            "--id-dir",
            &server_dir.to_string_lossy(),
        ]
        .iter()
        .map(|text| text.to_string())
        .collect();
        // Q-N3 (2026-11-27): the registry's DEBUG lines say which branch
        // did what (a ghost redeem expiring, a delivery that went
        // nowhere); everything else stays at INFO.
        let proc = Proc::spawn_with_env(
            tag,
            &args,
            &[("RUST_LOG", "info,ak_signal::server_v2=debug")],
        );
        REGISTRY_DB.with(|slot| *slot.borrow_mut() = Some(db.to_path_buf()));
        let listening = proc.stdout.wait_for(
            "v2 pairing registry on tls ",
            std::time::Duration::from_secs(60),
        );
        let port = announced_registry_port(&listening);
        let line = proc.stdout.wait_for(
            "registry id (pin with --registry-fp): ",
            std::time::Duration::from_secs(60),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id is the last token")
            .to_ascii_lowercase();
        assert_eq!(
            fp_hex.len(),
            64,
            "registry id line: {}",
            redact(line.as_ref())
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        RegistryProc { proc, addr, fp_hex }
    }
}

/// One end-to-end task-code chain: a `task code-recv` process (optionally
/// with extra `--advertise` IPs) plus a `task join-send` run, through one
/// shared registry. `consent_delay` is a REAL wait between the offer
/// banner and the accept answer (E2/G4's delayed-consent scenario passes
/// ~90 s; the immediate chains pass `Duration::ZERO`). Returns the
/// receiver's `receiver bound` and `advertising` lines, both sides'
/// captured stdout, for the caller's assertions.
fn run_task_code_chain(
    tag: &str,
    registry: &RegistryProc,
    root: &std::path::Path,
    advertise: &[&str],
    consent_delay: std::time::Duration,
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
    let advertised = recv.stdout.wait_for_reporting(
        "advertising",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let bound = recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );

    let code_line = recv.stdout.wait_for_reporting(
        "AKI1-",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {}", redact(code_line.as_ref())));

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
    // asks the consent question; wait out the requested delay (a real
    // user reading the banner for ~90 s in the E2/G4 scenario), then
    // keep feeding a fresh `y` until the transfer visibly moves on.
    recv.stdout.wait_for_reporting(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );
    if !consent_delay.is_zero() {
        std::thread::sleep(consent_delay);
    }
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
    // For a single-case run of a test on this chain (`--exact --nocapture`).
    eprintln!(
        "t03 chain {tag}: both children exited 0; payload {} bytes identical",
        landed.len()
    );
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
    let (bound, advertised, _recv_log, sender_log) = run_task_code_chain(
        "t03-derived",
        &registry,
        tmp.path(),
        &[],
        std::time::Duration::ZERO,
    );
    assert!(
        bound.contains("[::]"),
        "the receiver must report the dual-stack wildcard bind: {}",
        redact(bound.as_ref())
    );
    assert!(
        bound.contains("dual-stack"),
        "the receiver must name the bind dual-stack: {}",
        redact(bound.as_ref())
    );
    assert!(
        advertised.contains("[::1]") && advertised.contains("127.0.0.1"),
        "the derived candidates must carry both loopback families: {}",
        redact(advertised.as_ref())
    );
    assert!(
        !advertised.contains("fe80"),
        "link-local IPv6 must never ride the candidate list: {}",
        redact(advertised.as_ref())
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the task-code transfer must complete: {}",
        redact(&sender_log)
    );

    // Chain 2 — the IPv6 leg carries the payload: `--advertise ::1`
    // (the unchanged manual escape hatch) leads the list, so the winning
    // candidate is named and it is the v6 loopback.
    let (_bound2, _advertised2, _recv_log2, sender_log2) = run_task_code_chain(
        "t03-v6lead",
        &registry,
        tmp.path(),
        &["::1"],
        std::time::Duration::ZERO,
    );
    assert!(
        sender_log2.contains("[task] connected via [::1]:"),
        "the transfer must name the IPv6 loopback as the carrying path: {}",
        redact(&sender_log2)
    );
    assert!(
        sender_log2.contains("[task] done:"),
        "the v6-led task-code transfer must complete: {}",
        redact(&sender_log2)
    );
}

/// E2/G4 acceptance (review 2026-11-12 §6, batch-F F5/G4): the consent
/// window is 120 s, so a receiver who only answers ~90 s after the offer
/// still completes the transfer — the QUIC data plane must survive the
/// wait (CR-P2-04's keep-alive 10 s / idle 60 s) and the sender's
/// engine-level idle budget (`TASK_SEND_IDLE`, 150 s) must not cut the
/// parked send first.
///
/// This is a REAL-TIME test: it genuinely waits 90 s between the offer
/// banner and the `y` answer (the same "real wait" pattern as
/// ak-cli's keeper test; tokio time control cannot reach into the two
/// child processes). Wall time ≈ 95–105 s — CI timeouts must budget for
/// it. The window text assertion also pins the production 120 s value
/// through the product path (the prompt prints it).
#[test]
fn v1_task_code_path_accepts_90s_after_the_offer_and_still_completes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-late",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );

    let started = Instant::now();
    let (_bound, _advertised, recv_log, sender_log) = run_task_code_chain(
        "t03-late",
        &registry,
        tmp.path(),
        &[],
        // The acceptance scenario's "~90 s": comfortably past every
        // 30 s-era deadline (the old window would have declined at
        // 30 s), comfortably inside the new 120 s window.
        std::time::Duration::from_secs(90),
    );
    let elapsed = started.elapsed();

    // The wait really happened (a bug that answered instantly would
    // fail this) and stayed inside the window.
    assert!(
        elapsed >= std::time::Duration::from_secs(90),
        "the delayed consent must have actually waited: {elapsed:?}"
    );

    // The prompt (a `print!` fragment flushed into the first
    // newline-terminated line after acceptance) names the production
    // window — 120 s, not the frozen v1 model's 30 s.
    assert!(
        recv_log.contains("declines automatically after 120s"),
        "the consent prompt must carry the 120 s window: {}",
        redact(&recv_log)
    );

    // Both sides converged to success and the bytes are identical (the
    // chain asserts exit codes and byte equality itself; these pin the
    // visible completion lines).
    assert!(
        sender_log.contains("[task] done:"),
        "the sender must finish after the late accept: {}",
        redact(&sender_log)
    );
    assert!(
        recv_log.contains("received 1 file(s)"),
        "the receiver must complete the transfer: {}",
        redact(&recv_log)
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
    let bound = recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );
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
        .unwrap_or_else(|| {
            panic!(
                "reflexive address in the line: {}",
                redact(reflexive_line.as_ref())
            )
        });
    let reflexive: SocketAddr = reflexive_addr_text
        .parse()
        .unwrap_or_else(|_| panic!("reflexive ip:port: {}", redact(reflexive_addr_text)));
    let bound_port = bound
        .split("receiver bound ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or_else(|| panic!("bound addr token in the line: {}", redact(bound.as_ref())));
    assert_eq!(
        reflexive.port(),
        bound_port,
        "the STUN query must run on the socket QUIC adopts: {} vs {}",
        redact(reflexive_line.as_ref()),
        redact(bound.as_ref())
    );
    assert_reflexive_is_loopback(
        reflexive,
        &reflexive_line,
        "the truthful loopback responder reflects the local source",
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
        occurrences,
        1,
        "the loopback reflection is the derived loopback candidate — deduped to one entry: {}",
        redact(advertised.as_ref())
    );

    // (3) A full task-code transfer through the STUN-configured receiver.
    let code_line = recv.stdout.wait_for_reporting(
        "AKI1-",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {}", redact(code_line.as_ref())));
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
    recv.stdout.wait_for_reporting(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
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
    let reflexive_at = advertised.find("198.51.100.9:55555").unwrap_or_else(|| {
        panic!(
            "the distinct reflection must ride the blob: {}",
            redact(advertised.as_ref())
        )
    });
    let loopback_at = advertised.find("127.0.0.1:").unwrap_or_else(|| {
        panic!(
            "the derived loopback candidate must ride the blob: {}",
            redact(advertised.as_ref())
        )
    });
    assert!(
        reflexive_at < loopback_at,
        "the reflexive candidate inserts BEFORE the loopback tail: {}",
        redact(advertised.as_ref())
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
    let bound = recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "fail-open must stay inside the budget (query budget 2 s): {elapsed:?}; {}",
        redact(bound.as_ref())
    );
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(10));
    assert!(
        advertised.contains("127.0.0.1:"),
        "local candidates still advertise on STUN failure: {}",
        redact(advertised.as_ref())
    );
    let failure = recv
        .stdout
        .wait_for("stun query failed", std::time::Duration::from_secs(5));
    assert!(
        !failure.contains("panicked"),
        "the failure line is bounded diagnostics, not a crash: {}",
        redact(&failure)
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
        .unwrap_or_else(|| {
            panic!(
                "no connected-via line in the sender log:\n{}",
                redact(sender_log)
            )
        });
    line.split("[task] connected via ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("connected-via address in the line: {}", redact(line)))
}

/// The port the receiver's "receiver bound [::]:<port>" line reports.
fn receiver_bound_port(bound_line: &str) -> u16 {
    bound_line
        .split("receiver bound ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or_else(|| panic!("bound addr token in the line: {}", redact(bound_line)))
}

/// The address the sender's probe-round line observed (the observed head
/// that led the dial order).
fn probe_observed_addr(sender_log: &str) -> SocketAddr {
    let line = sender_log
        .lines()
        .find(|line| line.contains("[task] probe round observed ") && !line.contains("nothing"))
        .unwrap_or_else(|| {
            panic!(
                "no probe-round observation line in the sender log:\n{}",
                redact(sender_log)
            )
        });
    line.split("[task] probe round observed ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("observed address in the line: {}", redact(line)))
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
    let bound = recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );
    let real_port = receiver_bound_port(&bound);
    // The fake genuinely rides the advertised blob (the reflexive slot):
    // the sender's probe round probed it because it was advertised.
    let advertised = recv
        .stdout
        .wait_for("[task] advertising", std::time::Duration::from_secs(10));
    let fake_text = fake.addr.to_string();
    assert!(
        advertised.contains(&fake_text),
        "the probe-echo fake must ride the advertised candidates: {}",
        redact(advertised.as_ref())
    );

    // The real transfer chain against that receiver.
    let code_line = recv.stdout.wait_for_reporting(
        "AKI1-",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {}", redact(code_line.as_ref())));
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
    recv.stdout.wait_for_reporting(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );
    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));
    let sender_log = sender.stdout.snapshot().join("\n");

    // (1) The probe round ran and observed a dial-leading head. CR-P1-05
    // changed which candidate that is: the receiver's punch loop now
    // sends probe-shaped packets FROM the receiver's own QUIC socket, so
    // the receiver's QUIC-port candidate genuinely "answers" the round
    // and — preceding the fake in the static LAN-first order — leads the
    // dial (straight to the right socket). A head on the fake remains
    // legal (the pre-punch shape, or a punch that missed the window);
    // both must be ADVERTISED addresses.
    let observed = probe_observed_addr(&sender_log);
    assert_observed_head_is_advertised(&advertised, observed);
    assert_observed_head_is_fake_or_real(observed, fake.addr, real_port, &sender_log);

    // (2) The observed-first attempt cannot carry QUIC; the fallback
    // completes the transfer via the REAL receiver's bound port.
    let winner = connected_via_addr(&sender_log);
    assert_ne!(
        winner.port(),
        fake.addr.port(),
        "the fake holds no QUIC listener — it must not carry the task: {}",
        redact(&sender_log)
    );
    assert_eq!(
        winner.port(),
        real_port,
        "the fallback must land on the receiver's real bound port: {}",
        redact(&sender_log)
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the task-code transfer must complete through the fallback: {}",
        redact(&sender_log)
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
    let (bound, advertised, _recv_log, sender_log) = run_task_code_chain(
        "t03-obs",
        &registry,
        tmp.path(),
        &[],
        std::time::Duration::ZERO,
    );
    let real_port = receiver_bound_port(&bound);

    // The observed head is one of the receiver's genuinely ADVERTISED
    // addresses (it appears in the advertising line). CR-P1-05 supersedes
    // the old boundary note ("the probe-answering candidate is the echo
    // responder's port, never the QUIC port"): the receiver's punch loop
    // now sends probe-shaped packets FROM the QUIC socket, so the QUIC
    // candidate answers too — and when it leads, the observed-first dial
    // connects straight to the right socket (the winner check below).
    let observed = probe_observed_addr(&sender_log);
    let observed_text = observed.to_string();
    assert!(
        advertised
            .split_whitespace()
            .any(|token| token == observed_text),
        "the probe round must observe a genuinely advertised candidate: {} not in {}",
        redact(observed_text.as_ref()),
        redact(advertised.as_ref())
    );

    // The observed-first attempt fails over to the real receiver.
    let winner = connected_via_addr(&sender_log);
    assert_eq!(
        winner.port(),
        real_port,
        "the transfer must ride the receiver's real bound port: {}",
        redact(&sender_log)
    );
    assert!(
        sender_log.contains("[task] done:"),
        "the plain chain still completes: {}",
        redact(&sender_log)
    );
    // What the run proved, for a single-case run of this test (`--exact
    // --nocapture`): loopback ports only.
    eprintln!(
        "t03 probe round: the observed head {} is one of the {} advertised candidates; the \
         transfer rode the receiver's bound port {real_port}; the sender printed its done line; \
         payload bytes are checked by the chain",
        redact(observed_text.as_ref()),
        advertised
            .split("[task] advertising ")
            .nth(1)
            .and_then(|rest| rest.split(" candidate").next())
            .unwrap_or("?")
    );
}

// ---------------------------------------------------------------------------
// CR-P1-05 (review 2026-11-11 §4.5): the SENDER's same-socket lifecycle and
// the RECEIVER's hole punch, over the REAL product path. Two proofs:
//
// 1. Same-socket (sender): a truthful responder reflects the join-sender's
//    reflexive address, and the receiver's "paired; peer" line names the
//    ADVERTISED blob's port — the two ports must be EQUAL: the STUN query
//    ran on the very socket whose port the blob advertises and the
//    engine's dial adopts (a throwaway query socket would reflect its own
//    ephemeral port, and the blob would carry a different one).
// 2. Punch (receiver): a DISTINCT responder reflects a fixed address whose
//    port is a PLAIN UdpSocket this test owns; that address rides the
//    join-sender's advertised blob, and the RECEIVER's punch loop — fired
//    from the QUIC socket's clone — puts probe-shaped packets into the
//    test socket FROM the receiver's bound QUIC port. Same-host loopback
//    is not a NAT traversal claim (no NAT sits on the path); these tests
//    pin the wiring, not the traversal.
// ---------------------------------------------------------------------------

/// An address for an assertion's text: loopback as it is, anything else
/// with only its port.
fn shown_address(address: SocketAddr) -> String {
    redact(&address.to_string())
}

/// The reflexive address a truthful (same-host) responder reports is
/// loopback. An assertion as a function, so that a synthetic failing input
/// can show its whole panic text (review 21): a boolean comparison, since
/// an `assert_eq!` on the address would print both sides by itself.
fn assert_reflexive_is_loopback(reflexive: SocketAddr, line: &str, what: &str) {
    assert!(
        reflexive.ip() == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "{what}: the reflexive address is not loopback: {}",
        redact(line)
    );
}

/// The probe round's observed head is one of the advertised candidates
/// (an assertion as a function, see [`assert_reflexive_is_loopback`]).
fn assert_observed_head_is_advertised(advertised: &str, observed: SocketAddr) {
    let observed_text = observed.to_string();
    assert!(
        advertised
            .split_whitespace()
            .any(|token| token == observed_text),
        "the observed head must be a genuinely advertised candidate: {} not in {}",
        shown_address(observed),
        redact(advertised)
    );
}

/// The observed head is the probe-answering fake or the receiver's real
/// QUIC port (an assertion as a function, see [`assert_reflexive_is_loopback`]).
fn assert_observed_head_is_fake_or_real(
    observed: SocketAddr,
    fake_addr: SocketAddr,
    real_port: u16,
    sender_log: &str,
) {
    assert!(
        observed == fake_addr || observed.port() == real_port,
        "the head is the probe-answering fake or the punch-revealed QUIC candidate: {} (fake {}, real port {real_port}): {}",
        shown_address(observed),
        shown_address(fake_addr),
        redact(sender_log)
    );
}

/// Test 1's assertion that the receiver's punch arrived inside its budget,
/// as a function: its failure text comes from [`punch_failure`].
#[allow(clippy::too_many_arguments)] // what the failure text names
fn assert_punch_arrived(
    watch: &PunchWatch,
    quic_port: u16,
    watching_from_ms: u128,
    deadline_ms: u128,
    joiner_exit: Option<&JoinerExit>,
    watched_port: u16,
    receiver: &str,
    chain: &[(&str, &LineLog)],
) {
    assert!(
        watch.matched(),
        "{}",
        punch_failure(
            watch,
            quic_port,
            watching_from_ms,
            deadline_ms,
            joiner_exit,
            watched_port,
            receiver,
            chain
        )
    );
}

/// How long `at_ms` is after the report's origin, as text.
fn since_origin(origin: u128, at_ms: Option<u128>) -> String {
    at_ms.map_or("?".to_string(), |at| at.saturating_sub(origin).to_string())
}

#[allow(clippy::too_many_arguments)] // what the failure text names
fn punch_failure(
    watch: &PunchWatch,
    quic_port: u16,
    watching_from_ms: u128,
    deadline_ms: u128,
    joiner_exit: Option<&JoinerExit>,
    watched_port: u16,
    receiver: &str,
    chain: &[(&str, &LineLog)],
) -> String {
    // Frozen once: the summary and the details read the same copy (plan of
    // 2026-11-28, section B).
    let frozen = freeze(chain);
    let (receiver_out, receiver_err, receiver_exit) = frozen
        .process(receiver)
        .map(|process| (process.out_text(), process.err_text(), process.exit.clone()))
        .unwrap_or_default();
    let record = punch_loop_record(&receiver_err, watched_port);
    let receiver_exit = match receiver_exit {
        Watched::Exited(status) => format!("exited with {status}"),
        Watched::Running => "still running".to_string(),
        Watched::Unwatched(_) => "unknown".to_string(),
    };
    let joiner = joiner_exit
        .map(|exit| {
            format!(
                "{} at +{} ms",
                exit.status,
                exit.at_ms.saturating_sub(frozen.origin)
            )
        })
        .unwrap_or_else(|| "not seen while watching".to_string());
    format!(
        "the receiver's punch must arrive from its bound QUIC port {quic_port} before +{} ms \
         (watching from +{watching_from_ms} ms): probe-shaped packets seen {} \
         ({} from other source ports), the first from any source at +{} ms, \
         the first from the QUIC port at +{} ms, the last at +{} ms{}; \
         {} read(s) found nothing queued, {} interrupted, \
         {} datagram(s) that were not a probe packet, read error {:?}; \
         joiner exit status {joiner}; receiver punch loop start line seen: {}, \
         receiver {receiver_exit}; receiver punch loop: {}{}",
        deadline_ms.saturating_sub(frozen.origin),
        watch.probes,
        watch.other_source_probes,
        since_origin(frozen.origin, watch.first_probe_ms),
        since_origin(frozen.origin, watch.first_match_ms),
        since_origin(frozen.origin, watch.last_probe_ms),
        watch
            .late_match_ms
            .map(|at| format!(
                ", a matching packet first read at +{} ms, after the deadline",
                at.saturating_sub(frozen.origin)
            ))
            .unwrap_or_default(),
        watch.empty_reads,
        watch.interrupted,
        watch.bad_packets.len(),
        watch.read_error,
        receiver_out.contains("[task] punching"),
        record.describe(),
        frozen.render()
    )
}

/// Extract the address of the sender's "stun reflexive …" line (the
/// join-sender's own reflection).
fn sender_stun_reflexive_addr(sender_log: &str) -> SocketAddr {
    let line = sender_log
        .lines()
        .find(|line| line.contains("[task] stun reflexive "))
        .unwrap_or_else(|| {
            panic!(
                "no stun reflexive line in the sender log:\n{}",
                redact(sender_log)
            )
        });
    line.split("[task] stun reflexive ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse().ok())
        .unwrap_or_else(|| panic!("reflexive address in the line: {}", redact(line)))
}

/// One `task code-recv` child with STUN disabled (the receiver side is a
/// bystander in the CR-P1-05 sender tests).
fn spawn_recv_quiet_stun(tag: &str, registry: &RegistryProc, root: &std::path::Path) -> Proc {
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
        String::new(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .to_vec();
    Proc::spawn(tag, &args)
}

/// The join-send child with an explicit `--stun` (CR-P1-05: the sender
/// verbs carry the flag now) plus a payload directory of `tag`'s own.
fn spawn_join_send_with_stun(
    tag: &str,
    registry: &RegistryProc,
    root: &std::path::Path,
    code: &str,
    stun: &str,
) -> (Proc, PathBuf) {
    let id_send = root.join(format!("{tag}-id-send"));
    let src = root.join(format!("{tag}-src"));
    std::fs::create_dir_all(&id_send).expect("sender id dir");
    std::fs::create_dir_all(&src).expect("sender src dir");
    write_file(
        &src.join("payload.bin"),
        &common::Lcg::new(0xC5C5_0000_0000_0005).bytes(256 * 1024),
    );
    let args: Vec<String> = [
        "--id-dir".to_string(),
        id_send.to_string_lossy().into_owned(),
        "task".to_string(),
        "join-send".to_string(),
        code.to_string(),
        src.to_string_lossy().into_owned(),
        "--stun".to_string(),
        stun.to_string(),
        "--server".to_string(),
        registry.addr.to_string(),
        "--registry-fp".to_string(),
        registry.fp_hex.clone(),
    ]
    .to_vec();
    (Proc::spawn(tag, &args), src)
}

/// Proof 1: the sender's STUN query runs on THE socket the engine dial
/// adopts — the truthful reflection's port equals the port the sender's
/// ADVERTISED blob carries (the receiver's "paired; peer" line).
#[test]
fn sender_stun_and_probe_round_share_the_adopted_dial_socket() {
    // Loopback UDP must be bindable for the responder and both children.
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
        "t03-cr5a",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let mut recv = spawn_recv_quiet_stun("t03-cr5a-recv", &registry, tmp.path());
    recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );
    let code_line = recv.stdout.wait_for_reporting(
        "AKI1-",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {}", redact(code_line.as_ref())));

    let (mut sender, src) = spawn_join_send_with_stun(
        "t03-cr5a-send",
        &registry,
        tmp.path(),
        &code,
        &responder.addr.to_string(),
    );
    let reflexive_line = sender
        .stdout
        .wait_for("stun reflexive", std::time::Duration::from_secs(10));
    let reflexive = sender_stun_reflexive_addr(&reflexive_line);
    assert_reflexive_is_loopback(
        reflexive,
        &reflexive_line,
        "the truthful responder reflects the sender's own source",
    );

    // Drive the transfer to completion (offer + consent + done).
    recv.stdout.wait_for_reporting(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );
    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));

    // THE same-socket proof, wire shape: the sender's ADVERTISED blob
    // (what the receiver's "paired; peer …" line names) carries the very
    // port the truthful STUN reflection reported — the query ran on the
    // socket whose port the blob advertises and the engine dial adopts
    // (a throwaway query socket would reflect its own ephemeral port,
    // and the blob would carry a different one). The unit suite pins
    // the probe-round leg of the same rule
    // (`probe_round_on_leaves_from_the_caller_owned_port`).
    let recv_log = recv.stdout.snapshot().join("\n");
    let paired_line = recv_log
        .lines()
        .find(|line| line.contains("[task] paired; peer "))
        .unwrap_or_else(|| panic!("no paired line in the receiver log:\n{}", redact(&recv_log)));
    let advertised_port = paired_line
        .split("[task] paired; peer ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse::<SocketAddr>().ok())
        .map(|addr| addr.port())
        .unwrap_or_else(|| panic!("peer endpoint token in the line: {}", redact(paired_line)));
    assert_eq!(
        reflexive.port(),
        advertised_port,
        "the STUN query must run on the socket the blob advertises (and the dial adopts): {} vs {}",
        redact(reflexive_line.as_ref()),
        redact(paired_line)
    );
    let sender_log = sender.stdout.snapshot().join("\n");
    assert!(
        sender_log.contains("[task] done:"),
        "the transfer completes through the adopted dial socket: {}",
        redact(&sender_log)
    );
    let landed = std::fs::read(tmp.path().join("t03-cr5a-recv-out").join("payload.bin"))
        .expect("received payload");
    assert_eq!(
        landed,
        std::fs::read(src.join("payload.bin")).expect("source payload"),
        "payload bytes differ through the adopted dial socket"
    );
}

/// Proof 2: the receiver's punch loop sends from the QUIC socket's port to
/// the sender's ADVERTISED candidates — observable as probe-shaped packets
/// arriving at a test-owned socket bound at the sender's reflected
/// (distinct) address, sourced from the receiver's bound QUIC port.
#[test]
fn receiver_punch_reaches_the_sender_advertised_reflexive_port() {
    // The test-owned socket at a LOOPBACK address no interface enumerates
    // (127.0.0.9 is not 127.0.0.1, so the reflection never dedups away) —
    // the stand-in for the sender's NAT-mapped public endpoint.
    let mut punch_target = match std::net::UdpSocket::bind("127.0.0.9:0") {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("skipping: 127.0.0.9 UDP bind unavailable: {error}");
            return;
        }
    };
    let punch_addr = punch_target.local_addr().expect("punch target addr");
    let responder = StunResponder::spawn(Some(punch_addr)); // fixed distinct reflection

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = RegistryProc::spawn(
        "t03-cr5b",
        &tmp.path().join("server"),
        &tmp.path().join("registry.db"),
    );
    let mut recv = spawn_recv_quiet_stun("t03-cr5b-recv", &registry, tmp.path());
    let bound = recv.stdout.wait_for_reporting(
        "receiver bound",
        std::time::Duration::from_secs(10),
        &[("registry", &registry.proc.stdout)],
    );
    let quic_port = receiver_bound_port(&bound);
    let code_line = recv.stdout.wait_for_reporting(
        "AKI1-",
        std::time::Duration::from_secs(60),
        &[("registry", &registry.proc.stdout)],
    );
    let code = code_line
        .trim()
        .strip_prefix("AKI1-")
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("not the display line: {}", redact(code_line.as_ref())));

    let (mut sender, src) = spawn_join_send_with_stun(
        "t03-cr5b-send",
        &registry,
        tmp.path(),
        &code,
        &responder.addr.to_string(),
    );
    // The distinct reflection rides the sender's own reflexive line (and
    // with it, the advertised blob). The receiver and the registry go into
    // the report too: review 20 found a run that stopped here with only
    // the joiner's side.
    sender.stdout.wait_for_reporting(
        &format!("stun reflexive {punch_addr}"),
        std::time::Duration::from_secs(10),
        &[
            ("receiver", &recv.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );

    // THE punch observable: a probe-shaped packet from the receiver's
    // bound QUIC port arrives at the advertised address within the punch
    // window (the loop fires at pairing completion, concurrently with the
    // sender's dial — read while the transfer runs). The 30 s start here,
    // right after the sender's STUN line was read (plan of 2026-11-28,
    // section C): what they saw, the receiver's own record and the three
    // processes come from one frozen copy.
    let watch = {
        let chain = [
            ("receiver", &recv.stdout),
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ];
        let origin = chain
            .iter()
            .filter_map(|(_, log)| log.earliest_ms())
            .min()
            .unwrap_or_else(since_origin_ms);
        let watching_from_ms = since_origin_ms().saturating_sub(origin);
        let deadline_ms = since_origin_ms() + 30_000;
        let mut watch = PunchWatch::default();
        let joiner = &mut || sender.exit_status().map(|status| status.to_string());
        let (ended, joiner_exit) = watch_punch(
            &mut punch_target,
            quic_port,
            deadline_ms,
            punch_grace().as_millis(),
            joiner,
            &mut watch,
        );
        if ended != WatchEnd::Matched {
            assert_punch_arrived(
                &watch,
                quic_port,
                watching_from_ms,
                deadline_ms,
                joiner_exit.as_ref(),
                punch_addr.port(),
                "receiver",
                &chain,
            );
        }
        watch
    };

    // The receiver's punch loop ends with the first connection or with its
    // window, whichever comes first; its line is taken before the transfer
    // goes on. What CI run 46054926242 (head b0ea728) showed is that, at
    // the assertion back then, the parent had not read that line yet. Two
    // explanations fit — the receiver finished and exited before the
    // loop's next tick wrote it, or the line was written and the parent
    // had not drained the pipe yet — and the change covers both, so the
    // green runs since do not tell which it was.
    recv.wait_for_line(
        Stream::Stderr,
        "[pair-diag] receiver punch loop ended: ",
        std::time::Duration::from_secs(10),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );

    // The transfer still completes (the junk candidate costs the sender
    // nothing — it dials the RECEIVER's list, not its own).
    recv.stdout.wait_for_reporting(
        "the peer wants to send",
        std::time::Duration::from_secs(120),
        &[
            ("sender", &sender.stdout),
            ("registry", &registry.proc.stdout),
        ],
    );
    recv.consent_yes(
        &["[recv]", "task complete", "declined"],
        std::time::Duration::from_secs(40),
    );
    sender.wait_exit_ok(std::time::Duration::from_secs(180));
    recv.wait_exit_ok(std::time::Duration::from_secs(60));
    // The receiver's own record agrees with the socket: the watched port
    // was one of its selected targets, and it sent to that target.
    let chain = [
        ("receiver", &recv.stdout),
        ("sender", &sender.stdout),
        ("registry", &registry.proc.stdout),
    ];
    let frozen = freeze(&chain);
    let receiver = frozen
        .process("receiver")
        .expect("the receiver is one of the chain");
    let recv_log = receiver.out_text();
    assert!(
        recv_log.contains("[task] punching"),
        "the receiver must report its punch loop: {}",
        redact(&recv_log)
    );
    let record = punch_loop_record(&receiver.err_text(), punch_addr.port());
    assert!(
        matches!(
            record,
            PunchRecord::Sent {
                tick: 1..,
                packets: 1..,
                ..
            }
        ),
        "the receiver's punch loop must record a send to the watched port: {}",
        record.describe()
    );
    let landed = std::fs::read(tmp.path().join("t03-cr5b-recv-out").join("payload.bin"))
        .expect("received payload");
    let expected = std::fs::read(src.join("payload.bin")).expect("source payload");
    assert_eq!(
        landed, expected,
        "payload bytes differ through the punched path"
    );
    // What the run proved, for a single-case run of this test (`--exact
    // --nocapture`): a skipped run prints `skipping:` and none of this.
    // The time is that of the first packet from the QUIC port, not of the
    // first packet from anywhere.
    eprintln!(
        "t03 punch: a probe-shaped packet from the receiver's QUIC port {quic_port} reached the \
         watched target at +{} ms (the first probe-shaped packet from any source at +{} ms; \
         {} seen, {} from other source ports); receiver punch loop: {}; both children exited 0; \
         payload {} bytes identical",
        since_origin(frozen.origin, watch.first_match_ms),
        since_origin(frozen.origin, watch.first_probe_ms),
        watch.probes,
        watch.other_source_probes,
        record.describe(),
        landed.len()
    );
}

/// How long after the joiner is gone the watched socket is still read: the
/// receiver's punch window, plus the delayed start of a host with no
/// low-TTL send. All of it inside the test's own 30 s.
fn punch_grace() -> std::time::Duration {
    ak_session::pairing::task_net::PUNCH_WINDOW + ak_session::pairing::task_net::PUNCH_LOW_TTL_PHASE
}

/// How often the watched socket is asked again when nothing was queued.
const PUNCH_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// The largest datagram the punch test reads.
const PUNCH_BUFFER: usize = 1500;

/// What the punch test reads datagrams with: the real socket, or a
/// scripted one with a clock of its own in the unit tests below.
trait DatagramSource {
    fn set_nonblocking(&mut self, on: bool) -> std::io::Result<()>;
    fn recv_from(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;
    /// The test's monotonic clock, in milliseconds.
    fn now_ms(&self) -> u128;
    /// How long the socket is left alone before it is asked again.
    fn wait_a_little(&mut self, at_most: std::time::Duration);
}

impl DatagramSource for std::net::UdpSocket {
    fn set_nonblocking(&mut self, on: bool) -> std::io::Result<()> {
        std::net::UdpSocket::set_nonblocking(self, on)
    }

    fn recv_from(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        std::net::UdpSocket::recv_from(self, buffer)
    }

    fn now_ms(&self) -> u128 {
        since_origin_ms()
    }

    fn wait_a_little(&mut self, at_most: std::time::Duration) {
        std::thread::sleep(at_most);
    }
}

/// What the watched socket saw of the receiver's punch. Times are the
/// moments the parent read the datagrams, on the report's clock.
#[derive(Default)]
struct PunchWatch {
    /// The first probe-shaped packet from any source.
    first_probe_ms: Option<u128>,
    /// The first probe-shaped packet from the receiver's bound QUIC port,
    /// read before the deadline.
    first_match_ms: Option<u128>,
    /// A matching packet first read at or after the deadline: seen, but no
    /// hit inside the budget.
    late_match_ms: Option<u128>,
    /// The last probe-shaped packet from any source.
    last_probe_ms: Option<u128>,
    /// Probe-shaped packets from any source.
    probes: usize,
    /// …of them from a source port other than the QUIC port.
    other_source_probes: usize,
    /// Datagrams that are not what this socket is for (not probe-shaped,
    /// or longer than 64 bytes): length and whether probe-shaped.
    bad_packets: Vec<(usize, bool)>,
    /// Reads that found nothing queued.
    empty_reads: usize,
    /// Reads interrupted and retried inside the same budget.
    interrupted: usize,
    /// The first read error that is neither of those, as text.
    read_error: Option<String>,
}

impl PunchWatch {
    fn matched(&self) -> bool {
        self.first_match_ms.is_some()
    }

    /// Account for one datagram read at `at_ms`. `true` when the reading
    /// stops: a match, or a datagram that is not what this socket is for.
    fn saw(&mut self, payload: &[u8], from: SocketAddr, quic_port: u16, at_ms: u128) -> bool {
        let probe = ak_net::holepunch::is_probe_packet(payload);
        if !probe || payload.len() > 64 {
            self.bad_packets.push((payload.len(), probe));
            return true;
        }
        self.probes += 1;
        self.first_probe_ms.get_or_insert(at_ms);
        self.last_probe_ms = Some(at_ms);
        if from.port() == quic_port {
            self.first_match_ms.get_or_insert(at_ms);
            true
        } else {
            self.other_source_probes += 1;
            false
        }
    }
}

/// How the watching ended.
#[derive(Debug, PartialEq, Eq)]
enum WatchEnd {
    /// A probe-shaped packet from the receiver's bound QUIC port, read
    /// before the deadline.
    Matched,
    /// A datagram that is not what this socket is for.
    BadPacket,
    /// The deadline passed: 30 s after the sender's STUN line, or the
    /// punch grace after the joiner's exit, whichever came first.
    Deadline,
    /// The socket could not be switched to non-blocking reads.
    Source(String),
    /// A read failed for a reason other than "nothing queued".
    ReadError,
}

/// The joiner's exit, as the loop first saw it.
struct JoinerExit {
    at_ms: u128,
    status: String,
}

/// Read the watched socket until a probe-shaped packet from `quic_port`
/// arrives, a datagram is not what the socket is for, a read fails, or the
/// deadline passes — `deadline_ms`, shortened to `grace_ms` after the
/// joiner's exit (`joiner_exit`, asked before every read) is first seen.
/// The socket is switched to non-blocking reads once, and that is checked.
/// Before every read the remaining budget is checked, so a packet that is
/// only read at or after the deadline is never a hit; a match, a bad
/// datagram or a hard read error ends the reading at once (plan of
/// 2026-11-28, section C).
fn watch_punch(
    source: &mut impl DatagramSource,
    quic_port: u16,
    deadline_ms: u128,
    grace_ms: u128,
    joiner_exit: &mut dyn FnMut() -> Option<String>,
    watch: &mut PunchWatch,
) -> (WatchEnd, Option<JoinerExit>) {
    if let Err(error) = source.set_nonblocking(true) {
        return (WatchEnd::Source(error.to_string()), None);
    }
    let mut deadline_ms = deadline_ms;
    let mut exit: Option<JoinerExit> = None;
    let mut buffer = [0u8; PUNCH_BUFFER];
    loop {
        let now = source.now_ms();
        if now >= deadline_ms {
            return (WatchEnd::Deadline, exit);
        }
        if exit.is_none() {
            if let Some(status) = joiner_exit() {
                // The joiner ending (any status) means the transfer is
                // over. The receiver's punch loop, started at pairing, may
                // still be inside its window: the socket keeps being read
                // for that long, and no longer.
                deadline_ms = deadline_ms.min(now + grace_ms);
                exit = Some(JoinerExit { at_ms: now, status });
            }
        }
        match source.recv_from(&mut buffer) {
            Ok((len, from)) => {
                let at_ms = source.now_ms();
                if watch.saw(&buffer[..len], from, quic_port, at_ms) {
                    if let Some(at) = watch.first_match_ms {
                        if at >= deadline_ms {
                            // Read only after the deadline: not a hit.
                            watch.first_match_ms = None;
                            watch.late_match_ms = Some(at);
                            return (WatchEnd::Deadline, exit);
                        }
                    }
                    let ended = if watch.matched() {
                        WatchEnd::Matched
                    } else {
                        WatchEnd::BadPacket
                    };
                    return (ended, exit);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => watch.interrupted += 1,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                watch.empty_reads += 1;
                let left = std::time::Duration::from_millis((deadline_ms - now) as u64);
                source.wait_a_little(PUNCH_POLL.min(left));
            }
            Err(error) => {
                watch.read_error = Some(error.to_string());
                return (WatchEnd::ReadError, exit);
            }
        }
    }
}

/// What the receiver's `[pair-diag] receiver punch loop ended:` line says
/// about the watched port. Only `Sent` proves that the loop sent to that
/// port; everything else names what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PunchRecord {
    /// The loop has not ended yet, or never ran: no end line.
    NoEndLine,
    /// The line is there; the watched port is not among the selected
    /// targets (the list may be empty).
    NotSelected,
    /// Selected, and no send to it succeeded (`@0/0`).
    NeverSent,
    /// Selected, but the evidence does not decide: the port is that of
    /// several targets, or a count is missing, zero on one side only, not
    /// a number, or out of range.
    Undecided(String),
    /// Selected alone: target `index` of `of`, first sent on tick `tick`
    /// (1-based), `packets` sent in all.
    Sent {
        index: usize,
        of: usize,
        tick: u32,
        packets: u32,
    },
}

impl PunchRecord {
    fn describe(&self) -> String {
        match self {
            PunchRecord::NoEndLine => "no end line yet (the loop starts once pairing completes \
                 and ends with its window or the first connection)"
                .to_string(),
            PunchRecord::NotSelected => {
                "the watched port is not among the selected target(s)".to_string()
            }
            PunchRecord::NeverSent => {
                "the watched port is a selected target, but no send to it succeeded (@0/0)"
                    .to_string()
            }
            PunchRecord::Undecided(why) => {
                format!("the evidence does not decide: {why}")
            }
            PunchRecord::Sent {
                index,
                of,
                tick,
                packets,
            } => format!(
                "the watched port is target {index} of {of}, first sent on tick {tick}, \
                 {packets} packet(s)"
            ),
        }
    }
}

/// The part of the receiver's end line after `selected [` and before `];`.
fn punch_loop_record(receiver_err: &str, watched_port: u16) -> PunchRecord {
    const HEAD: &str = "[pair-diag] receiver punch loop ended: ";
    let Some(said) = receiver_err
        .lines()
        .filter_map(|line| line.split_once(HEAD).map(|(_, said)| said))
        .next_back()
    else {
        return PunchRecord::NoEndLine;
    };
    let selected = said
        .split("selected [")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .unwrap_or_default();
    let entries: Vec<&str> = selected
        .split(", ")
        .filter(|entry| !entry.is_empty())
        .collect();
    let watched_port = watched_port.to_string();
    let watched: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|entry| entry.split('@').next() == Some(watched_port.as_str()))
        .collect();
    if watched.is_empty() {
        return PunchRecord::NotSelected;
    }
    if watched.len() > 1 {
        return PunchRecord::Undecided(format!(
            "the watched port is the port of {} of the {} selected target(s) \
             [{selected}], so one target cannot be named",
            watched.len(),
            entries.len()
        ));
    }
    let entry = watched[0];
    let undecided = |why: &str| {
        PunchRecord::Undecided(format!("the watched port is selected [{entry}], but {why}"))
    };
    let Some((tick, packets)) = entry
        .split_once('@')
        .and_then(|(_, counts)| counts.split_once('/'))
    else {
        return undecided("a count is missing");
    };
    let (Ok(tick), Ok(packets)) = (tick.parse::<u32>(), packets.parse::<u32>()) else {
        return undecided("a count is not a number or is out of range");
    };
    match (tick, packets) {
        (0, 0) => PunchRecord::NeverSent,
        (tick, packets) if tick > 0 && packets > 0 => PunchRecord::Sent {
            index: entries
                .iter()
                .position(|other| *other == entry)
                .unwrap_or(0)
                + 1,
            of: entries.len(),
            tick,
            packets,
        },
        _ => undecided("one count is zero and the other is not"),
    }
}

/// An exit status as a real child produced it, for synthetic logs.
fn exit_status(code: i32) -> std::process::ExitStatus {
    let mut command = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    if cfg!(windows) {
        command.args(["/C", "exit", &code.to_string()]);
    } else {
        command.args(["-c", &format!("exit {code}")]);
    }
    command
        .status()
        .expect("run a child that exits with the code")
}

/// Plan of 2026-11-28 (after review 21), section B: the same ending
/// decides the same way, whichever thread reports first.
#[test]
fn ending_with_error_does_not_depend_on_which_thread_reports_first() {
    const ERROR: &str = "Error: something failed";
    // The exit status 0 first, then the error line and the pipes' end.
    let a = LineLog::synthetic("p", &[], &[], false, Watched::Exited(exit_status(0)));
    assert!(!a.ended_with_error());
    a.state.lock().unwrap().end = Some((since_origin_ms(), None));
    a.stderr
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .lines
        .push(TimedLine {
            at_ms: since_origin_ms(),
            text: ERROR.to_string(),
        });
    // One pipe still open while the status says 0: not enough.
    assert!(!a.ended_with_error());
    a.stderr.as_ref().unwrap().lock().unwrap().end = Some((since_origin_ms(), None));
    assert!(a.ended_with_error());
    // The error line and both pipes' end first, then the exit status 0.
    let b = LineLog::synthetic("p", &[], &[ERROR.to_string()], true, Watched::Running);
    assert!(b.ended_with_error());
    *b.exit.as_ref().unwrap().lock().unwrap() = Watched::Exited(exit_status(0));
    assert!(b.ended_with_error());
    // An unsuccessful exit needs no error line.
    let c = LineLog::synthetic("p", &[], &[], false, Watched::Exited(exit_status(3)));
    assert!(c.ended_with_error());
    // A clean exit is not an error.
    let d = LineLog::synthetic(
        "p",
        &["all good".to_string()],
        &[],
        true,
        Watched::Exited(exit_status(0)),
    );
    assert!(!d.ended_with_error());
    // An error line with one pipe still open, while the process runs, is
    // not enough.
    let e = LineLog::synthetic("p", &[], &[ERROR.to_string()], false, Watched::Running);
    assert!(!e.ended_with_error());
    // An unknown end goes by the pipes and the error line alone.
    let f = LineLog::synthetic(
        "p",
        &[],
        &[ERROR.to_string()],
        true,
        Watched::Unwatched("x".to_string()),
    );
    assert!(f.ended_with_error());
    let g = LineLog::synthetic("p", &[], &[], true, Watched::Unwatched("x".to_string()));
    assert!(!g.ended_with_error());
}

/// Plan of 2026-11-28, section B: a wait on one stream reports every
/// process once — both pipes, one exit status — and a line read after the
/// deadline explains the failure without ending it.
#[test]
fn a_wait_on_one_stream_reports_each_process_once() {
    let receiver = LineLog::synthetic(
        "receiver",
        &["the same needle on stdout".to_string()],
        &[],
        false,
        Watched::Running,
    );
    let sender = LineLog::synthetic("sender", &[], &[], false, Watched::Running);
    let registry = LineLog::synthetic("registry", &[], &[], false, Watched::Running);
    let others = [
        ("sender", &sender),
        ("registry", &registry),
        // The same process a second time, and its bare stderr log: neither
        // gets a second heading.
        ("receiver", &receiver),
        (
            "receiver again",
            &LineLog {
                state: Arc::clone(receiver.stderr.as_ref().unwrap()),
                stderr: None,
                name: None,
                exit: None,
            },
        ),
    ];
    let text = panic_text(|| {
        receiver.wait_for_stream(
            Stream::Stderr,
            "the same needle",
            std::time::Duration::from_millis(40),
            &others,
        );
    });
    for heading in ["--- receiver (", "--- sender (", "--- registry ("] {
        assert_eq!(text.matches(heading).count(), 1, "{heading} in:\n{text}");
    }
    assert_eq!(text.matches("exit status:").count(), 3, "{text}");
    assert!(text.contains("out: the same needle on stdout"), "{text}");
    assert!(!text.contains("err: the same needle"), "{text}");

    // Read after the deadline: shown in the report, not returned.
    let late = LineLog::synthetic(
        "p",
        &["came late".to_string()],
        &[],
        false,
        Watched::Running,
    );
    late.state.lock().unwrap().lines[0].at_ms = since_origin_ms() + 60_000;
    let text = panic_text(|| {
        late.wait_for_stream(
            Stream::Stdout,
            "came late",
            std::time::Duration::from_millis(40),
            &[],
        );
    });
    assert!(
        text.contains("no line of the stdout containing \"came late\""),
        "{text}"
    );
    assert!(text.contains("came late"), "{text}");
}

/// Plan of 2026-11-28, section B: a report waits for the last lines of a
/// process that already exited, and everything it says reads that one
/// copy. A line that arrives after the copy was taken is in nothing.
#[test]
fn a_report_reads_the_last_lines_of_an_exited_process_once() {
    const END_LINE: &str = "[pair-diag] receiver punch loop ended: 2 candidate(s), 2 considered, \
         selected [41@2/3]; 6 packet(s) sent (0 low-TTL), 3 tick(s), stopped early: false, \
         start delayed: false";
    let receiver = LineLog::synthetic(
        "receiver",
        &["[task] punching".to_string()],
        &[],
        false,
        Watched::Exited(exit_status(1)),
    );
    receiver.state.lock().unwrap().end = Some((since_origin_ms(), None));
    let stderr = Arc::clone(receiver.stderr.as_ref().unwrap());
    // The last line is still in the pipe: it arrives 150 ms after the exit
    // was seen, and with it the pipe's end.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut state = stderr.lock().unwrap();
        state.lines.push(TimedLine {
            at_ms: since_origin_ms(),
            text: END_LINE.to_string(),
        });
        state.end = Some((since_origin_ms(), None));
    });
    let sender = LineLog::synthetic("sender", &[], &[], true, Watched::Exited(exit_status(1)));
    let chain = [("receiver", &receiver), ("sender", &sender)];
    let watch = PunchWatch::default();
    let text = panic_text(|| {
        assert_punch_arrived(&watch, 53421, 300, 30_300, None, 41, "receiver", &chain)
    });
    // In the summary …
    assert!(text.contains("first sent on tick 2, 3 packet(s)"), "{text}");
    // … and in the details, once.
    assert_eq!(text.matches(END_LINE).count(), 1, "{text}");
    // A line that arrives after the copy was taken is in neither.
    let frozen = freeze(&chain);
    let taken = frozen.process("receiver").expect("the receiver is frozen");
    assert!(taken.err_text().contains(END_LINE));
    assert_eq!(
        punch_loop_record(&taken.err_text(), 41).describe(),
        "the watched port is target 1 of 1, first sent on tick 2, 3 packet(s)"
    );
    let before = frozen.render();
    receiver
        .stderr
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .lines
        .push(TimedLine {
            at_ms: since_origin_ms(),
            text: "written after the copy".to_string(),
        });
    assert!(!frozen.render().contains("written after the copy"));
    assert!(!before.contains("written after the copy"));
}

/// One read of a scripted watched socket.
enum Step {
    /// A datagram from `from_port`, read `after_ms` later: probe-shaped or
    /// not.
    Packet {
        after_ms: u128,
        from_port: u16,
        probe: bool,
    },
    Interrupted,
    /// A read error of this kind, other than "nothing queued".
    Error(std::io::ErrorKind),
}

/// A watched socket that answers from a script: each read returns the next
/// step and moves the clock by the step's delay; an empty script answers
/// `WouldBlock`, and waiting moves the clock too.
struct ScriptedSocket {
    steps: std::collections::VecDeque<Step>,
    clock_ms: u128,
    reads: usize,
    refuses_nonblocking: bool,
}

impl ScriptedSocket {
    fn new(steps: Vec<Step>) -> Self {
        ScriptedSocket {
            steps: steps.into(),
            clock_ms: 0,
            reads: 0,
            refuses_nonblocking: false,
        }
    }
}

impl DatagramSource for ScriptedSocket {
    fn set_nonblocking(&mut self, _on: bool) -> std::io::Result<()> {
        if self.refuses_nonblocking {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        } else {
            Ok(())
        }
    }

    fn recv_from(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        self.reads += 1;
        match self.steps.pop_front() {
            Some(Step::Packet {
                after_ms,
                from_port,
                probe,
            }) => {
                self.clock_ms += after_ms;
                let packet = if probe {
                    ak_net::holepunch::probe_packet(&[0x5A; 16])
                } else {
                    vec![0u8; 200]
                };
                buffer[..packet.len()].copy_from_slice(&packet);
                Ok((
                    packet.len(),
                    SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), from_port),
                ))
            }
            Some(Step::Interrupted) => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
            Some(Step::Error(kind)) => Err(std::io::Error::from(kind)),
            None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        }
    }

    fn now_ms(&self) -> u128 {
        self.clock_ms
    }

    fn wait_a_little(&mut self, at_most: std::time::Duration) {
        self.clock_ms += at_most.as_millis();
    }
}

fn probe(after_ms: u128, from_port: u16) -> Step {
    Step::Packet {
        after_ms,
        from_port,
        probe: true,
    }
}

/// Plan of 2026-11-28, section C: how many reads the loop makes on a
/// scripted socket with a clock of its own. `run` returns how it ended,
/// the joiner's exit status as the loop saw it, the number of reads, the
/// clock when the loop returned, and what was seen.
#[test]
fn the_punch_loop_reads_only_what_its_budget_allows() {
    const QUIC: u16 = 53421;
    let run = |steps: Vec<Step>, deadline_ms: u128, grace_ms: u128, joiner: Option<&str>| {
        let mut socket = ScriptedSocket::new(steps);
        let mut watch = PunchWatch::default();
        let mut exit_status = || joiner.map(str::to_string);
        let (ended, exit) = watch_punch(
            &mut socket,
            QUIC,
            deadline_ms,
            grace_ms,
            &mut exit_status,
            &mut watch,
        );
        (
            ended,
            exit.map(|exit| exit.status),
            socket.reads,
            socket.clock_ms,
            watch,
        )
    };
    // Another source at +3 ms, the match at +9 ms: two reads; the times
    // are those of the first packet from anywhere and of the match.
    let (ended, exit, reads, _, watch) =
        run(vec![probe(3, 54000), probe(6, QUIC)], 30_000, 5_000, None);
    assert_eq!((ended, exit, reads), (WatchEnd::Matched, None, 2));
    assert_eq!(
        (
            watch.first_probe_ms,
            watch.first_match_ms,
            watch.last_probe_ms,
            watch.other_source_probes
        ),
        (Some(3), Some(9), Some(9), 1)
    );
    // A datagram that is not a probe packet: one read, no match.
    let (ended, _, reads, _, watch) = run(
        vec![
            Step::Packet {
                after_ms: 3,
                from_port: QUIC,
                probe: false,
            },
            probe(6, QUIC),
        ],
        30_000,
        5_000,
        None,
    );
    assert_eq!((ended, reads), (WatchEnd::BadPacket, 1));
    assert!(!watch.matched());
    assert_eq!(watch.bad_packets, vec![(200, false)]);
    // The deadline already passed: not one read.
    let (ended, _, reads, _, _) = run(vec![probe(1, QUIC)], 0, 5_000, None);
    assert_eq!((ended, reads), (WatchEnd::Deadline, 0));
    // A match already queued when the joiner's exit is seen: read.
    let (ended, exit, reads, _, watch) =
        run(vec![probe(0, QUIC)], 30_000, 5_000, Some("exit code: 0"));
    assert_eq!(
        (ended, exit, reads, watch.matched()),
        (WatchEnd::Matched, Some("exit code: 0".to_string()), 1, true)
    );
    // An empty queue: asked every 10 ms until the deadline, never matched.
    let (ended, _, reads, clock, watch) = run(vec![], 100, 5_000, None);
    assert_eq!((ended, reads, clock), (WatchEnd::Deadline, 10, 100));
    assert_eq!(watch.empty_reads, 10);
    assert!(!watch.matched());
    // Near the deadline: another source every millisecond does not keep
    // the loop reading past it.
    let near: Vec<Step> = (0..20).map(|_| probe(1, 54000)).collect();
    let (ended, _, reads, _, watch) = run(near, 10, 5_000, None);
    assert_eq!((ended, reads), (WatchEnd::Deadline, 10));
    assert_eq!(watch.other_source_probes, 10);
    // Interrupted reads are retried inside the same budget.
    let (ended, _, reads, _, watch) = run(
        vec![Step::Interrupted, Step::Interrupted, probe(1, QUIC)],
        30_000,
        5_000,
        None,
    );
    assert_eq!(ended, WatchEnd::Matched);
    assert_eq!(
        (reads, watch.interrupted, watch.first_match_ms),
        (3, 2, Some(1))
    );
    // The joiner's exit shortens the deadline to the grace.
    let (ended, exit, reads, clock, _) = run(
        vec![
            probe(1_000, 54000),
            probe(1_000, 54000),
            probe(1_000, 54000),
            probe(1_000, QUIC),
        ],
        30_000,
        3_000,
        Some("exit code: 1"),
    );
    assert_eq!(ended, WatchEnd::Deadline);
    assert_eq!(exit.as_deref(), Some("exit code: 1"));
    assert_eq!((reads, clock), (3, 3_000));
    // A match read at or after the deadline is not a hit.
    let (ended, _, reads, _, watch) = run(
        vec![probe(5_000, 54000), probe(2_000, QUIC)],
        6_000,
        5_000,
        None,
    );
    assert_eq!(ended, WatchEnd::Deadline);
    assert_eq!(
        (reads, watch.matched(), watch.late_match_ms),
        (2, false, Some(7_000))
    );
    // The socket cannot be switched: nothing is read.
    let mut socket = ScriptedSocket::new(vec![probe(1, QUIC)]);
    socket.refuses_nonblocking = true;
    let mut watch = PunchWatch::default();
    let mut never = || None;
    let (ended, _) = watch_punch(&mut socket, QUIC, 30_000, 5_000, &mut never, &mut watch);
    assert!(matches!(ended, WatchEnd::Source(_)));
    assert_eq!(socket.reads, 0);
    // A hard read error ends the reading at once.
    let (ended, _, reads, _, watch) = run(
        vec![
            Step::Error(std::io::ErrorKind::ConnectionReset),
            probe(1, QUIC),
        ],
        30_000,
        5_000,
        None,
    );
    assert_eq!((ended, reads), (WatchEnd::ReadError, 1));
    assert!(watch.read_error.is_some());
}

/// Plan of 2026-11-28, section C: the real API — a datagram queued before
/// the loop starts is read by the first non-blocking read.
#[test]
fn a_queued_probe_is_read_from_the_real_socket() {
    let mut watched = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the watched socket");
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the sending socket");
    sender
        .send_to(
            &ak_net::holepunch::probe_packet(&[0x07; 16]),
            watched.local_addr().expect("watched address"),
        )
        .expect("queue the datagram");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut watch = PunchWatch::default();
    let mut never = || None;
    let (ended, _) = watch_punch(
        &mut watched,
        sender.local_addr().expect("sender address").port(),
        since_origin_ms() + 2_000,
        100,
        &mut never,
        &mut watch,
    );
    assert_eq!(ended, WatchEnd::Matched);
    assert_eq!((watch.probes, watch.empty_reads), (1, 0));
}

/// Plan of 2026-11-28, section C: the receiver's record decides only for a
/// target that is the only one with the watched port, and only with both
/// counts positive.
#[test]
fn the_punch_record_decides_only_on_a_unique_target_with_positive_counts() {
    let said = |selected: &str| {
        format!(
            "[pair-diag] receiver punch loop ended: 9 candidate(s), 8 considered, \
             selected [{selected}]; 80 packet(s) sent (48 low-TTL), 10 tick(s), \
             stopped early: true, start delayed: false"
        )
    };
    assert_eq!(punch_loop_record("", 41), PunchRecord::NoEndLine);
    assert_eq!(
        punch_loop_record("some other line\nmore", 41),
        PunchRecord::NoEndLine
    );
    assert_eq!(punch_loop_record(&said(""), 41), PunchRecord::NotSelected);
    assert_eq!(
        punch_loop_record(&said("40@1/2, 42@1/2"), 41),
        PunchRecord::NotSelected
    );
    let sent = punch_loop_record(&said("40@1/2, 41@2/3, 42@1/1"), 41);
    assert_eq!(
        sent,
        PunchRecord::Sent {
            index: 2,
            of: 3,
            tick: 2,
            packets: 3
        }
    );
    assert_eq!(
        sent.describe(),
        "the watched port is target 2 of 3, first sent on tick 2, 3 packet(s)"
    );
    // The last end line is the one that counts.
    assert_eq!(
        punch_loop_record(&format!("{}\n{}", said("40@1/2"), said("41@2/3")), 41),
        PunchRecord::Sent {
            index: 1,
            of: 1,
            tick: 2,
            packets: 3
        }
    );
    assert_eq!(
        punch_loop_record(&said("41@0/0"), 41),
        PunchRecord::NeverSent
    );
    for undecidable in [
        "41@0/3",
        "41@2/0",
        "41@2",
        "41@x/3",
        "41@2/x",
        "41@2/4294967296",
        "41@4294967296/1",
        "41@-1/1",
        "41@1/1, 41@2/3",
    ] {
        let record = punch_loop_record(&said(undecidable), 41);
        assert!(
            matches!(record, PunchRecord::Undecided(_)),
            "{undecidable:?} gave {record:?}"
        );
    }
}
