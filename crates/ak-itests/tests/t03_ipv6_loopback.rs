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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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
#[derive(Clone)]
enum Watched {
    /// Still running (or not watched at all).
    Running,
    Exited(std::process::ExitStatus),
    /// The watcher could not keep polling the child: its end is unknown.
    Unwatched(String),
}

/// One line read off a pipe and the moment it was read.
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

/// Punctuation that may sit in front of a secret inside one token; it is
/// kept in front of what replaces the secret.
const OPENERS: [char; 8] = ['"', '\'', '(', '[', '{', '<', '（', '“'];

/// What ends a path or a name inside one token: what follows it there is
/// other text.
const CLAUSE_CLOSERS: [char; 8] = [',', ';', ')', ']', '}', '=', '）', '＝'];

/// Punctuation after a secret, kept so the line still reads.
const TRAILING: [char; 11] = ['"', '\'', ',', ';', ')', ']', '}', ':', '.', '!', '?'];

/// The words after which a bare file name (`in device.key`) or a quoted
/// name (`under "photos"`) is a name on disk, not a word of the message.
const INTRODUCES_A_NAME: [&str; 15] = [
    "in", "at", "into", "under", "dir", "path", "file", "db", "key", "open", "read", "write",
    "save", "load", "create",
];

/// `true` for text that is a filesystem path: absolute (a drive letter,
/// a UNC root, a `/` root), or relative with a separator (`..\x`,
/// `src/main.rs`, a URL; not prose such as `Y/n`).
fn looks_like_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let absolute = (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || text.starts_with(r"\\")
        || (text.starts_with('/') && text[1..].contains('/'));
    let from_here = ["./", "../", ".\\", "..\\"]
        .iter()
        .any(|start| text.starts_with(start));
    let last = text.rsplit(['\\', '/']).next().unwrap_or(text);
    absolute
        || from_here
        || text.contains('\\')
        || text.contains("://")
        || (text.contains('/') && is_bare_file_name(last))
}

/// A file name with no separator in it (`device.key`): a dot and a short
/// extension that is not all digits (`256.00` is a number).
fn is_bare_file_name(text: &str) -> bool {
    let Some((base, extension)) = text.rsplit_once('.') else {
        return false;
    };
    let short = !extension.is_empty()
        && extension.len() <= 4
        && extension.bytes().all(|b| b.is_ascii_alphanumeric());
    let numbered = extension.bytes().all(|b| b.is_ascii_digit());
    !base.is_empty() && short && !numbered
}

/// An address as text; a zone id (`fe80::1%12`) is left out of the parse.
fn parse_host(text: &str) -> Option<IpAddr> {
    let bare = text.split('%').next()?;
    if let Ok(v6) = bare.parse::<Ipv6Addr>() {
        return Some(IpAddr::V6(v6));
    }
    bare.parse::<Ipv4Addr>().ok().map(IpAddr::V4)
}

/// Loopback and unspecified addresses, and loopback in its v4-mapped
/// form, stay in a report.
fn address_stays(address: IpAddr) -> bool {
    let local = |v4: Ipv4Addr| v4.is_loopback() || v4.is_unspecified();
    match address {
        IpAddr::V4(v4) => local(v4),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.to_ipv4_mapped().is_some_and(local)
        }
    }
}

/// What replaces an address that is not kept: `<addr>`, with its port
/// when the text has one (`[v6]:port`, `v4:port`; a bare v6 has none).
fn hidden_address(address: &str) -> String {
    let port = match address.strip_prefix('[') {
        Some(after) => after
            .split_once(']')
            .and_then(|(_, tail)| tail.strip_prefix(':')),
        None => address
            .rsplit_once(':')
            .filter(|(host, _)| !host.contains(':'))
            .map(|(_, port)| port),
    };
    match port {
        Some(port) => format!("<addr>:{port}"),
        None => "<addr>".to_string(),
    }
}

/// An IP address at the start of `text`: how many bytes it takes, and
/// whether it is hidden. `None` when `text` does not start with one.
fn address_prefix(text: &str) -> Option<(usize, bool)> {
    // `[v6]:port`: the bracket belongs to the address.
    if let Some(after) = text.strip_prefix('[') {
        let end = after.find(']')?;
        let address = parse_host(&after[..end])?;
        if !address.is_ipv6() {
            return None;
        }
        let digits = after[end + 1..].strip_prefix(':').unwrap_or("");
        let port = digits.bytes().take_while(|b| b.is_ascii_digit()).count();
        let len = 2 + end + if port > 0 { 1 + port } else { 0 };
        return Some((len, !address_stays(address)));
    }
    let len = text
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '%'))
        .count();
    let candidate = text.get(..len)?;
    let (host, port) = match candidate.rsplit_once(':') {
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (host, Some(port))
        }
        _ => (candidate, None),
    };
    let address = parse_host(host)?;
    if port.is_some() && !address.is_ipv4() {
        return None;
    }
    Some((len, !address_stays(address)))
}

/// A task code in every form the parser takes (any case, grouped or
/// not), from `AKI1` on: how many bytes it takes.
fn task_code_len(text: &str) -> Option<usize> {
    let body = text.get(4..)?;
    let run = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .count();
    let code = body
        .get(..run)
        .is_some_and(|code| match code.strip_prefix('-') {
            Some(groups) => {
                groups.split('-').count() == 8 && groups.split('-').all(|group| group.len() == 4)
            }
            None => code.len() == 32 && code.chars().all(|c| c.is_ascii_alphanumeric()),
        });
    code.then_some(4 + run)
}

/// One stretch of text that is neither a path nor an address, with the
/// task code and hex runs of 32 or more (a registry fingerprint, a
/// pairing id) hidden wherever they start.
fn redact_plain(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let code_at = (0..rest.len().saturating_sub(3))
            .find(|&at| rest.as_bytes()[at..at + 4].eq_ignore_ascii_case(b"AKI1"));
        let hex_at = rest.find(|c: char| c.is_ascii_hexdigit());
        let code_at = match (code_at, hex_at) {
            (Some(at), None) => Some(at),
            (Some(at), Some(hex)) if at <= hex => Some(at),
            _ => None,
        };
        match (code_at, hex_at) {
            (Some(at), _) => {
                if let Some(len) = task_code_len(&rest[at..]) {
                    shown.push_str(&rest[..at]);
                    shown.push_str("AKI1-<code>");
                    rest = &rest[at + len..];
                } else {
                    // Not a code: this `AKI1` stays; look further on.
                    shown.push_str(&rest[..at + 4]);
                    rest = &rest[at + 4..];
                }
            }
            (_, Some(at)) => {
                shown.push_str(&rest[..at]);
                let run = rest[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
                if run >= 32 {
                    shown.push_str("<hex>");
                } else {
                    shown.push_str(&rest[at..at + run]);
                }
                rest = &rest[at + run..];
            }
            (None, None) => {
                shown.push_str(rest);
                rest = "";
            }
        }
    }
    shown
}

fn strip_trailing(text: &str) -> (&str, &str) {
    let stem_len = text.trim_end_matches(|c: char| TRAILING.contains(&c)).len();
    text.split_at(stem_len)
}

fn clause_end(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(_, c)| CLAUSE_CLOSERS.contains(c))
        .map(|(at, _)| at)
}

/// How a secret runs on over the words that follow it in the line.
#[derive(Clone, Copy)]
enum RunOn {
    /// A quoted path or name: the words up to its closing quote.
    Quoted(char),
    /// An unquoted path with spaces: the words up to one that ends a
    /// clause, starts a flag or a bracket, or has an `=`.
    Unquoted,
}

enum TokenPiece {
    Text(String),
    RunsOn { shown: String, run_on: RunOn },
}

impl TokenPiece {
    fn into_parts(self) -> (String, Option<RunOn>) {
        match self {
            TokenPiece::Text(text) => (text, None),
            TokenPiece::RunsOn { shown, run_on } => (shown, Some(run_on)),
        }
    }
}

/// The secret that starts at `rest`: a token, or what follows an
/// opening quote or bracket inside one; `before` is what precedes it
/// there, and `previous` the word shown before the token. `None` when
/// `rest` does not start with one.
fn secret_at(rest: &str, before: &str, previous: &str) -> Option<TokenPiece> {
    // An address, first with its own bracket.
    if let Some((len, hidden)) = address_prefix(rest) {
        let head = &rest[..len];
        let shown = if hidden {
            hidden_address(head)
        } else {
            head.to_string()
        };
        return Some(TokenPiece::Text(format!(
            "{before}{shown}{}",
            redact_plain(&rest[len..])
        )));
    }
    let lead_len = rest.chars().take_while(|c| OPENERS.contains(c)).count();
    let (lead, body) = rest.split_at(lead_len);
    if !lead.is_empty() {
        if let Some((len, hidden)) = address_prefix(body) {
            let head = &body[..len];
            let shown = if hidden {
                hidden_address(head)
            } else {
                head.to_string()
            };
            return Some(TokenPiece::Text(format!(
                "{before}{lead}{shown}{}",
                redact_plain(&body[len..])
            )));
        }
    }
    // `key=value` and `--flag=value`: the value is where a secret can be.
    if let Some((key, value)) = body.split_once('=') {
        if let Some(piece) = secret_at(value, &format!("{before}{lead}{key}="), "") {
            return Some(piece);
        }
    }
    let quote = lead.chars().rev().find(|c| *c == '"' || *c == '\'');
    let (stem, tail) = strip_trailing(body);
    let (stem, after) = match clause_end(stem) {
        Some(at) => (&stem[..at], &stem[at..]),
        None => (stem, ""),
    };
    let placeholder = if looks_like_path(stem) {
        "<path>"
    } else if INTRODUCES_A_NAME.contains(&previous) && is_bare_file_name(stem) {
        return Some(TokenPiece::Text(format!(
            "{before}{lead}<path>{after}{tail}"
        )));
    } else if INTRODUCES_A_NAME.contains(&previous) && quote.is_some() {
        "<name>"
    } else {
        return None;
    };
    let mut shown = format!("{before}{lead}{placeholder}");
    if let Some(quote) = quote {
        // Quoted: the same word may already close it.
        if !after.is_empty() || tail.starts_with(quote) {
            shown.push_str(after);
            shown.push_str(tail);
            return Some(TokenPiece::Text(shown));
        }
        return Some(TokenPiece::RunsOn {
            shown,
            run_on: RunOn::Quoted(quote),
        });
    }
    shown.push_str(after);
    // Unquoted: a path whose last part has no extension may have spaces
    // after it (`C:\Users\John Doe`); one that ends in a file name, or is
    // followed by punctuation, does not.
    let complete = is_bare_file_name(stem.rsplit(['\\', '/']).next().unwrap_or(stem));
    if complete || !tail.is_empty() {
        shown.push_str(tail);
        return Some(TokenPiece::Text(shown));
    }
    Some(TokenPiece::RunsOn {
        shown,
        run_on: RunOn::Unquoted,
    })
}

/// What replaces a token: a secret starts at its first character or
/// right after an opening quote or bracket in it; anything else is text
/// with codes and hex runs hidden.
fn open_token(token: &str, previous: &str) -> TokenPiece {
    let after_openers = token
        .char_indices()
        .filter(|(_, c)| OPENERS.contains(c))
        .map(|(at, c)| at + c.len_utf8());
    for at in std::iter::once(0usize).chain(after_openers) {
        if let Some(piece) = secret_at(&token[at..], &token[..at], previous) {
            return piece;
        }
    }
    TokenPiece::Text(redact_plain(token))
}

/// Where an unquoted path's run on ends inside `token`: at a word that
/// starts a flag or a bracket (unless the word still carries a path
/// separator, as `C:\Program Files (x86)\x` does), or at a clause closer
/// or an `=` in it.
fn run_on_end(token: &str) -> Option<usize> {
    if let Some(first) = token.chars().next() {
        if (first == '-' || OPENERS.contains(&first)) && !token.contains(['\\', '/']) {
            return Some(0);
        }
    }
    let (at, closer) = token
        .char_indices()
        .find(|(_, c)| CLAUSE_CLOSERS.contains(c))?;
    let after = &token[at + closer.len_utf8()..];
    // `(x86)\Common`: a closer inside a word that still carries the path.
    if after.contains(['\\', '/']) {
        None
    } else {
        Some(at)
    }
}

fn redact_line(line: &str) -> String {
    let mut shown = String::with_capacity(line.len());
    let mut run_on: Option<RunOn> = None;
    let mut previous = String::new();
    for token in line.split_whitespace() {
        let in_run_on = run_on.is_some();
        let (piece, runs_on) = match run_on {
            // The rest of a quoted path or name: absorbed up to its
            // closing quote.
            Some(RunOn::Quoted(quote)) => match token.find(quote) {
                Some(at) => {
                    let after = at + quote.len_utf8();
                    (format!("{quote}{}", redact_plain(&token[after..])), None)
                }
                None => continue,
            },
            // A word of an unquoted path with spaces.
            Some(RunOn::Unquoted) => match run_on_end(token) {
                Some(0) => open_token(token, &previous).into_parts(),
                Some(at) => (redact_plain(&token[at..]), None),
                // `path: cause`: the path ends with the word before the colon.
                None if token.ends_with(':') => (":".to_string(), None),
                None => continue,
            },
            None => open_token(token, &previous).into_parts(),
        };
        run_on = runs_on;
        // The word that ends a path: its closing quote, clause closer or
        // colon belongs to the placeholder, without a space before it.
        let closes =
            in_run_on && piece.starts_with(['"', '\'', ',', ';', ')', ']', '}', ':', '）']);
        if !shown.is_empty() && !closes {
            shown.push(' ');
        }
        shown.push_str(&piece);
        previous = piece
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_ascii_lowercase();
    }
    shown
}

/// The reportable form of captured text. The callers that parse a line
/// (the code, the bound port, the candidates) keep the raw text; this is
/// only for what a failure prints. Hidden: the task code in every form
/// the parser takes; hex runs of 32 or more (the registry fingerprint, a
/// pairing id); paths and file names, quoted or not, spaces included;
/// the source root name the receiver prints; addresses that are not
/// loopback (their ports stay). A path never runs on over the words that
/// follow it into the next line, and a clause closer, a colon or a
/// closing quote after it ends it, so the cause after it stays.
fn redact(text: &str) -> String {
    text.split('\n')
        .map(redact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The subcommand words a spawn failure may name.
const SUBCOMMANDS: [&str; 6] = [
    "task",
    "code-recv",
    "code-send",
    "join-send",
    "join-recv",
    "registry-server",
];

/// What a spawn failure may say about a child's arguments: long flags by
/// name (`--key=value` as `--key=<value>`) and the subcommand words as
/// they are; every other value hidden.
fn argv_summary(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let name = match arg.split_once('=') {
                Some((flag, _)) => flag,
                None => arg.as_str(),
            };
            if name.starts_with("--") || SUBCOMMANDS.contains(&name) {
                if name.len() == arg.len() {
                    arg.clone()
                } else {
                    format!("{name}=<value>")
                }
            } else {
                "<value>".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    /// What the watcher concluded so far.
    fn watched(&self) -> Watched {
        self.exit
            .as_ref()
            .map(|watched| watched.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or(Watched::Running)
    }

    /// The raw lines so far, for the callers that parse them.
    fn snapshot(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lines
            .iter()
            .map(|line| line.text.clone())
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
        let own = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .lines
            .first()
            .map(|line| line.at_ms);
        let err = self.stderr.as_ref().and_then(|state| {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .lines
                .first()
                .map(|line| line.at_ms)
        });
        match (own, err) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        }
    }

    /// Both pipes ended (closed or on a read error).
    fn pipes_ended(&self) -> bool {
        let own = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .end
            .is_some();
        let err = self
            .stderr
            .as_ref()
            .map(|state| {
                state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .end
                    .is_some()
            })
            .unwrap_or(true);
        own && err
    }

    /// The process is over and did not end well: a non-zero exit, or its
    /// pipes closed after an error line — whichever the watcher and the
    /// reader threads happen to report first, the same ending decides the
    /// same way. It cannot produce the line a wait is looking for, so the
    /// wait stops and reports now.
    fn ended_with_error(&self) -> bool {
        match self.watched() {
            Watched::Exited(status) => !status.success(),
            Watched::Running | Watched::Unwatched(_) => self.pipes_ended() && self.has_error_line(),
        }
    }

    /// `Error:` or a panic somewhere in this log or its stderr.
    fn has_error_line(&self) -> bool {
        let owns = |state: &Arc<Mutex<LogState>>| {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .lines
                .iter()
                .any(|line| line.text.contains("Error:") || line.text.contains("panicked at"))
        };
        owns(&self.state) || self.stderr.as_ref().map(owns).unwrap_or(false)
    }

    /// The lines of this log and of its stderr, redacted, each with
    /// milliseconds since `origin` and the stream it came from; then how
    /// each pipe ended and the exit status (still-open pipes last).
    fn timeline(&self, origin: u128) -> Vec<String> {
        let mut rows: Vec<(u128, String)> = Vec::new();
        let streams = [(Some(&self.state), "out"), (self.stderr.as_ref(), "err")];
        for (state, tag) in streams {
            let Some(state) = state else { continue };
            let state = state.lock().unwrap_or_else(|p| p.into_inner());
            for line in &state.lines {
                rows.push((
                    line.at_ms,
                    format!(
                        "[+{:>7} ms] {tag}: {}",
                        line.at_ms.saturating_sub(origin),
                        redact(&line.text)
                    ),
                ));
            }
            match &state.end {
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
        match self.watched() {
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

    /// This stream so far plus the process's stderr when attached, as one
    /// timeline counted from the earliest line. The last lines of a
    /// process that already exited are waited for (see [`settle`]).
    fn report(&self) -> String {
        settle(&[self]);
        let origin = self.earliest_ms().unwrap_or_else(since_origin_ms);
        self.timeline(origin).join("\n")
    }

    /// First line containing `needle`, polling until it appears.
    fn wait_for(&self, needle: &str, timeout: std::time::Duration) -> String {
        self.wait_for_reporting(needle, timeout, &[])
    }

    /// [`Self::wait_for`], with `others`' output (stderr included) in the
    /// timeout report too: a wait on one process often runs out because
    /// of another (2026-11-20 review of batch Q: a Windows CI run timed
    /// out waiting for the receiver's offer line, and the report held only
    /// the receiver's side). Q-N3 (2026-11-27): the wait also stops as soon
    /// as this process or one of `others` is over and did not end well —
    /// the registry is still alive then, so its log and a read-only
    /// snapshot of its database go into the same report.
    fn wait_for_reporting(
        &self,
        needle: &str,
        timeout: std::time::Duration,
        others: &[(&str, &LineLog)],
    ) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            for line in self.snapshot() {
                if line.contains(needle) {
                    return line;
                }
            }
            let self_ended = self.ended_with_error();
            let other_ended = others
                .iter()
                .find(|(_, other)| other.ended_with_error())
                .map(|(name, _)| *name);
            if Instant::now() >= deadline || self_ended || other_ended.is_some() {
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
                let mut report = format!("no line containing {needle:?} ({why})");
                let mut logs: Vec<(&str, &LineLog)> =
                    vec![(self.name.as_deref().unwrap_or("this process"), self)];
                logs.extend_from_slice(others);
                report.push_str(&failure_report(&logs));
                panic!("{report}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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

/// One failure report for every process of the chain: a timeline counted
/// from the first line any of them wrote, then the registry database as it
/// is right now (when a registry was started on this thread).
fn failure_report(logs: &[(&str, &LineLog)]) -> String {
    let watched: Vec<&LineLog> = logs.iter().map(|(_, log)| *log).collect();
    settle(&watched);
    let origin = logs
        .iter()
        .filter_map(|(_, log)| log.earliest_ms())
        .min()
        .unwrap_or_else(since_origin_ms);
    let mut report = String::new();
    for (name, log) in logs {
        report.push_str(&format!(
            "\n--- {name} (milliseconds since the first line of this report):\n"
        ));
        report.push_str(&log.timeline(origin).join("\n"));
    }
    report.push_str(&format!(
        "\n--- registry database, read-only snapshot at +{} ms:\n{}",
        since_origin_ms().saturating_sub(origin),
        registry_snapshot()
    ));
    report
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
    assert_eq!(
        reflexive.ip(),
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "the truthful loopback responder reflects the local source: {}",
        redact(reflexive_line.as_ref())
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
    let observed_text = observed.to_string();
    assert!(
        advertised
            .split_whitespace()
            .any(|token| token == observed_text),
        "the observed head must be a genuinely advertised candidate: {} not in {}",
        redact(observed_text.as_ref()),
        redact(advertised.as_ref())
    );
    assert!(
        observed == fake.addr || observed.port() == real_port,
        "the head is the probe-answering fake or the punch-revealed QUIC candidate: {observed} (fake {}, real port {real_port}): {}",
        fake.addr
    , redact(&sender_log));

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
    assert_eq!(
        reflexive.ip(),
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "the truthful responder reflects the sender's own source: {}",
        redact(reflexive_line.as_ref())
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
    let punch_target = match std::net::UdpSocket::bind("127.0.0.9:0") {
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
    // sender's dial — read while the transfer runs).
    punch_target
        .set_read_timeout(Some(std::time::Duration::from_millis(500)))
        .expect("punch target timeout");
    // Pairing + punch window + transfer slack.
    let punch_deadline = Instant::now() + std::time::Duration::from_secs(30);
    // Q-N3 (2026-11-27): the first CI failure of this test said only that
    // no matching packet arrived. The report now also says what the 30 s
    // did see, on the same clock as the three processes' timelines, and
    // (review 20) what the receiver's own punch loop says it sent to the
    // watched target.
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
    let mut watch = PunchWatch::default();
    // The joiner's exit status when it ended before a matching packet, and
    // when that was seen.
    let mut sender_exit: Option<String> = None;
    let mut joiner_gone: Option<Instant> = None;
    let mut buffer = [0u8; 1500];
    while Instant::now() < punch_deadline && !watch.from_quic_port {
        // The joiner ending (any status) means the transfer is over. The
        // receiver's punch loop, started at pairing, may still be inside
        // its window: read what is already queued first (review 20: a
        // matching packet can sit in the queue when the exit is first
        // seen), then keep reading only for as long as that window lasts.
        if sender_exit.is_none() {
            if let Some(status) = sender.exit_status() {
                sender_exit = Some(status.to_string());
                watch.drain(&punch_target, quic_port, origin);
                joiner_gone = Some(Instant::now());
            }
        }
        if joiner_gone.is_some_and(|seen| seen.elapsed() >= punch_grace()) {
            break;
        }
        match punch_target.recv_from(&mut buffer) {
            Ok((len, from)) => {
                if watch.saw(&buffer[..len], from, quic_port, origin) {
                    break;
                }
            }
            // Interrupted (e.g. on Unix): the read is retried inside the
            // same 30 s budget, not counted and not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                watch.read_timeouts += 1;
            }
            Err(error) => {
                watch.read_error = Some(error.to_string());
                break;
            }
        }
    }
    if let Some((len, probe)) = watch.bad_packet {
        assert!(
            probe,
            "the punch packet is probe-shaped: {len} bytes{}",
            failure_report(&chain)
        );
        assert!(
            len <= 64,
            "the punch packet is small: {len} bytes{}",
            failure_report(&chain)
        );
    }
    let receiver_log = recv.stdout.snapshot().join("\n");
    assert!(
        watch.from_quic_port,
        "the receiver's punch must arrive from its bound QUIC port {quic_port}: \
         watching from +{watching_from_ms} ms, probe-shaped packets seen {} \
         (from other source ports {}), first at +{:?} ms, last at +{:?} ms, \
         read timeouts {}, sender exit status {sender_exit:?}, read error {:?}; \
         receiver pairing line seen: {}; receiver punch loop: {}{}",
        watch.packets,
        watch.other_source_port,
        watch.first_packet_ms,
        watch.last_packet_ms,
        watch.read_timeouts,
        watch.read_error,
        receiver_log.contains("[task] paired; peer "),
        punch_loop_record(&recv.stderr.snapshot().join("\n"), punch_addr.port()),
        failure_report(&chain)
    );

    // The receiver's punch loop ends with the first connection or with its
    // window, whichever comes first; its line is taken before the transfer
    // goes on. On a fast machine the receiver can otherwise finish and exit
    // inside one tick of the loop, and the detached loop's report is never
    // written (CI run 46054926242 failed exactly there, on both platforms).
    recv.stderr.wait_for_reporting(
        "[pair-diag] receiver punch loop ended: ",
        std::time::Duration::from_secs(10),
        &[
            ("receiver", &recv.stdout),
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
    let recv_log = recv.stdout.snapshot().join("\n");
    assert!(
        recv_log.contains("[task] punching"),
        "the receiver must report its punch loop: {}",
        redact(&recv_log)
    );
    // The receiver's own record agrees with the socket: the watched port
    // was one of its selected targets, and it sent to that target (the
    // line was waited for above, so it is on hand).
    let record = punch_loop_record(&recv.stderr.snapshot().join("\n"), punch_addr.port());
    assert!(
        record.contains("first sent on tick "),
        "the receiver's punch loop must record a send to the watched port: {record}"
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
    eprintln!(
        "t03 punch: a probe-shaped packet from the receiver's QUIC port {quic_port} reached the \
         watched target at +{} ms ({} probe-shaped packets seen, {} from other source ports); \
         receiver punch loop: {record}; both children exited 0; payload {} bytes identical",
        watch
            .first_packet_ms
            .map_or("?".to_string(), |ms| ms.to_string()),
        watch.packets,
        watch.other_source_port,
        landed.len()
    );
}

/// How long after the joiner is gone the watched socket is still read: the
/// receiver's punch window, plus the delayed start of a host with no
/// low-TTL send. All of it inside the test's own 30 s.
fn punch_grace() -> std::time::Duration {
    ak_session::pairing::task_net::PUNCH_WINDOW + ak_session::pairing::task_net::PUNCH_LOW_TTL_PHASE
}

/// What the watched socket of the punch test saw (Q-N3, review 20): the
/// numbers a failure needs, kept apart from the processes' logs.
#[derive(Default)]
struct PunchWatch {
    /// A probe-shaped packet from the receiver's bound QUIC port arrived.
    from_quic_port: bool,
    packets: usize,
    other_source_port: usize,
    first_packet_ms: Option<u128>,
    last_packet_ms: Option<u128>,
    read_timeouts: usize,
    read_error: Option<String>,
    /// A packet that is not what this socket is for: its length and
    /// whether it was probe-shaped.
    bad_packet: Option<(usize, bool)>,
}

impl PunchWatch {
    /// Account for one datagram read off the watched socket. `true` when
    /// it is not a punch packet: the caller stops and reports.
    fn saw(&mut self, payload: &[u8], from: SocketAddr, quic_port: u16, origin: u128) -> bool {
        let probe = ak_net::holepunch::is_probe_packet(payload);
        if !probe || payload.len() > 64 {
            self.bad_packet = Some((payload.len(), probe));
            return true;
        }
        self.packets += 1;
        let at = since_origin_ms().saturating_sub(origin);
        self.first_packet_ms.get_or_insert(at);
        self.last_packet_ms = Some(at);
        if from.port() == quic_port {
            self.from_quic_port = true;
        } else {
            // The pairing-time probe (ak-session's throwaway socket) also
            // lands here.
            self.other_source_port += 1;
        }
        false
    }

    /// Read and account for every datagram already queued on `socket`,
    /// without blocking; the socket's read timeout is set back after.
    fn drain(&mut self, socket: &std::net::UdpSocket, quic_port: u16, origin: u128) {
        let timeout = socket.read_timeout().ok().flatten();
        let _ = socket.set_read_timeout(Some(std::time::Duration::ZERO));
        let mut buffer = [0u8; 1500];
        while !self.from_quic_port && self.bad_packet.is_none() {
            match socket.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    if self.saw(&buffer[..len], from, quic_port, origin) {
                        break;
                    }
                }
                // Nothing more is queued (or the socket is done): the
                // errors of a non-blocking read carry no information.
                Err(_) => break,
            }
        }
        let _ = socket.set_read_timeout(timeout);
    }
}

/// What the receiver's `[pair-diag] receiver punch loop ended:` line says
/// about the port the test watches (review 20): whether that target was
/// selected at all, and on which tick the loop first sent to it — a total
/// over all targets cannot tell. The line carries ports and counts only.
fn punch_loop_record(receiver_log: &str, watched_port: u16) -> String {
    let Some(line) = receiver_log
        .lines()
        .find(|line| line.contains("[pair-diag] receiver punch loop ended: "))
    else {
        return "no end line yet (the loop starts once pairing completes and ends \
                with its window or the first connection)"
            .to_string();
    };
    let said = line
        .split("[pair-diag] receiver punch loop ended: ")
        .nth(1)
        .unwrap_or("");
    let selected = said
        .split("selected [")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .unwrap_or("");
    let entries: Vec<&str> = selected
        .split(", ")
        .filter(|entry| !entry.is_empty())
        .collect();
    let watched_port = watched_port.to_string();
    let target = match entries
        .iter()
        .position(|entry| entry.split('@').next() == Some(watched_port.as_str()))
    {
        Some(index) => {
            let (tick, packets) = entries[index]
                .rsplit_once('@')
                .and_then(|(_, counts)| counts.split_once('/'))
                .unwrap_or(("?", "?"));
            format!(
                "the watched port is target {} of {}, first sent on tick {tick}, {packets} packet(s)",
                index + 1,
                entries.len()
            )
        }
        None => format!(
            "the watched port is not among the selected target(s) [{}]",
            selected
        ),
    };
    format!("{said}; for the watched port: {target}")
}
