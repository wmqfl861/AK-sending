//! Invite-pairing integration tests, CLI layer (wave 22): the **complete
//! product chain driven by real `ak-cli` processes** — a registry-server
//! subprocess, `invite create` / `invite list` / `invite revoke`
//! subprocesses, resident `daemon --pairing` (owner) and
//! `daemon --invite-code` (redeemer) subprocesses over real TLS ports and
//! the real filesystem, and a final `send` subprocess whose payload must
//! land byte-identical on the other side.
//!
//! Layering: `crates/ak-session/tests/pairing_e2e.rs` proves the pairing
//! transaction in-process (client + orchestrator against a spawned
//! registry task); the in-crate `daemon` smoke test proves the daemon
//! core in-process. These tests cross the remaining boundaries — process
//! spawn/exit codes, stdout contracts (human text and the `--json-lines`
//! NDJSON stream), the persisted `registry.pin` / `invites.store` /
//! `peers.book` files shared between independent processes, and the real
//! UDP transfer plane between two daemon processes.
//!
//! The binary is built with `cargo build -p ak-cli` into the shared
//! workspace target dir on first use (cargo sets `CARGO` for test
//! processes; `$CARGO_TARGET_DIR` is honored when present).
#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---- CLI binary location / build -------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/ak-itests has a workspace root two levels up")
        .to_path_buf()
}

fn cargo_bin() -> PathBuf {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return PathBuf::from(cargo);
    }
    let fallback = std::env::var_os("USERPROFILE")
        .map(|home| {
            PathBuf::from(home).join(".cargo").join(if cfg!(windows) {
                "bin\\cargo.exe"
            } else {
                "bin/cargo"
            })
        })
        .filter(|path| path.exists());
    fallback.unwrap_or_else(|| PathBuf::from("cargo"))
}

/// Path of the `ak-cli` executable, building it once if needed.
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
        // Always ask cargo for a freshness check so a stale binary can
        // never make these tests assert against old code.
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

// ---- process plumbing -------------------------------------------------------

/// Lines a piped stream has produced so far (fed by a reader thread so
/// the child can never block on a full pipe).
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
                        let mut lines =
                            sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
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
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn joined(&self) -> String {
        self.snapshot().join("\n")
    }
}

/// A resident CLI child process with captured stdout/stderr.
struct Proc {
    child: Child,
    stdout: LineLog,
    stderr: LineLog,
}

impl Proc {
    fn spawn(tag: &str, args: &[&str]) -> Proc {
        let cli = cli_exe();
        let mut child = Command::new(&cli)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {tag} ({:?} {:?}): {err}", cli, args));
        let stdout = LineLog::spawn_reader(child.stdout.take().expect("stdout piped"));
        let stderr = LineLog::spawn_reader(child.stderr.take().expect("stderr piped"));
        Proc {
            child,
            stdout,
            stderr,
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Result of one one-shot CLI run.
struct RunOutcome {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run `ak-cli <args>` to completion (killed and failed after `timeout`).
fn run_cli(args: &[&str], timeout: Duration) -> RunOutcome {
    let cli = cli_exe();
    let mut child = Command::new(&cli)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn ({:?} {:?}): {err}", cli, args));
    let stdout_handle = {
        let pipe = child.stdout.take().expect("stdout piped");
        std::thread::spawn(move || read_all(pipe))
    };
    let stderr_handle = {
        let pipe = child.stderr.take().expect("stderr piped");
        std::thread::spawn(move || read_all(pipe))
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait on cli child") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    RunOutcome {
        success: status.is_some_and(|status| status.success()),
        stdout: stdout_handle.join().expect("stdout reader"),
        stderr: stderr_handle.join().expect("stderr reader"),
    }
}

fn read_all(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind a probe socket")
        .local_addr()
        .expect("probe local addr")
        .port()
}

// ---- fixtures ---------------------------------------------------------------

/// Deterministic payload (xorshift64 filler, not cryptographic — it only
/// needs pseudo-random bytes for byte-identity checks).
fn payload(len: usize) -> Vec<u8> {
    let mut state = 0x00C0_FFEE_1234_5678u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// The registry-server subprocess: a real v2 TLS registry with its own
/// SQLite database and its pin printed on stdout.
struct RegistryProc {
    /// Owning the process keeps the server alive (and killed on drop).
    #[allow(dead_code)]
    proc: Proc,
    addr: SocketAddr,
    fp_hex: String,
}

impl RegistryProc {
    fn spawn(tag: &str, server_dir: &Path, db: &Path) -> RegistryProc {
        let port = free_port();
        let port_text = port.to_string();
        let db_text = db.to_string_lossy().into_owned();
        let id_text = server_dir.to_string_lossy().into_owned();
        let args = [
            "registry-server",
            "--port",
            &port_text,
            "--db-path",
            &db_text,
            "--id-dir",
            &id_text,
        ];
        let proc = Proc::spawn(tag, &args);
        let line = proc.stdout.wait_for(
            "registry id (pin with --registry-fp): ",
            Duration::from_secs(30),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id is the last token")
            .to_ascii_lowercase();
        assert_eq!(fp_hex.len(), 64, "registry id line: {line}");
        RegistryProc {
            proc,
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            fp_hex,
        }
    }
}

/// Everything the `invite create` subprocess reports, parsed from stdout.
#[derive(Debug, Clone)]
struct CreatedCode {
    display: String,
    code_id: String,
}

fn parse_created_code(stdout: &str) -> CreatedCode {
    let display = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("AKI1-"))
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("no AKI1- code line in:\n{stdout}"));
    let code_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("code id "))
        .and_then(|rest| rest.split(' ').next().map(str::to_owned))
        .unwrap_or_else(|| panic!("no code id line in:\n{stdout}"));
    assert_eq!(code_id.len(), 32, "code id is 32 hex chars: {code_id}");
    CreatedCode { display, code_id }
}

/// One paired owner/redeemer chain across four processes: registry-server,
/// owner `daemon --pairing`, redeemer `daemon --invite-code` (paired and
/// listening), both against fresh identity directories.
struct PairedChain {
    tmp: tempfile::TempDir,
    /// Owning the registry keeps the subprocess alive for the chain.
    #[allow(dead_code)]
    registry: RegistryProc,
    a_dir: PathBuf,
    b_dir: PathBuf,
    b_out: PathBuf,
    a_daemon: Proc,
    b_daemon: Proc,
    #[allow(dead_code)]
    code: CreatedCode,
    a_fp: String,
    b_fp: String,
    b_port: u16,
}

impl PairedChain {
    fn spawn(tag: &str) -> PairedChain {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server_dir = tmp.path().join("server");
        let a_dir = tmp.path().join("owner-a");
        let b_dir = tmp.path().join("redeemer-b");
        let a_out = tmp.path().join("out-a");
        let b_out = tmp.path().join("out-b");
        for dir in [&server_dir, &a_dir, &b_dir, &a_out, &b_out] {
            std::fs::create_dir_all(dir).expect("create dir");
        }
        let registry = RegistryProc::spawn(tag, &server_dir, &tmp.path().join("registry.db"));
        let addr = registry.addr.to_string();
        let fp = registry.fp_hex.clone();

        // Owner creates one long-lived invite code (flags persist the pin).
        let created = run_cli(
            &[
                "invite",
                "create",
                "--long-lived",
                "--id-dir",
                &a_dir.to_string_lossy(),
                "--server",
                &addr,
                "--registry-fp",
                &fp,
            ],
            Duration::from_secs(30),
        );
        assert!(created.success, "invite create failed:\n{}", created.stderr);
        let code = parse_created_code(&created.stdout);

        // Owner daemon: resident pairing answer service (the book is empty
        // at startup, so the receive admission must be widened explicitly).
        let a_daemon = Proc::spawn(
            &format!("{tag}-owner-daemon"),
            &[
                "daemon",
                "--id-dir",
                &a_dir.to_string_lossy(),
                "--out",
                &a_out.to_string_lossy(),
                "--expect",
                "any",
                "--json-lines",
                "--pairing",
                "--name",
                &format!("owner-{tag}"),
                "--server",
                &addr,
                "--registry-fp",
                &fp,
            ],
        );
        a_daemon.stderr.wait_for(
            "pairing service: answering redeems",
            Duration::from_secs(30),
        );
        let a_listening = a_daemon
            .stdout
            .wait_for("\"type\":\"listening\"", Duration::from_secs(30));
        let a_fp = json_field(&a_listening, "fingerprint").expect("a fingerprint");

        // Redeemer daemon: redeem at startup, then resident receive.
        let b_daemon = Proc::spawn(
            &format!("{tag}-redeemer-daemon"),
            &[
                "daemon",
                "--id-dir",
                &b_dir.to_string_lossy(),
                "--out",
                &b_out.to_string_lossy(),
                "--json-lines",
                "--invite-code",
                &code.display,
                "--name",
                &format!("redeemer-{tag}"),
                "--server",
                &addr,
                "--registry-fp",
                &fp,
            ],
        );
        // The listening event only fires after the redeem committed the
        // owner into peers.book, so it doubles as the pairing barrier.
        let b_listening = b_daemon
            .stdout
            .wait_for("\"type\":\"listening\"", Duration::from_secs(60));
        let b_port: u16 = json_field(&b_listening, "port")
            .expect("b port")
            .parse()
            .expect("b port number");
        let b_fp = json_field(&b_listening, "fingerprint").expect("b fingerprint");

        PairedChain {
            tmp,
            registry,
            a_dir,
            b_dir,
            b_out,
            a_daemon,
            b_daemon,
            code,
            a_fp,
            b_fp,
            b_port,
        }
    }

    fn send_to_b(&self, path: &Path, fp_arg: &str) -> RunOutcome {
        run_cli(
            &[
                "send",
                &path.to_string_lossy(),
                "--to",
                &format!("127.0.0.1:{}", self.b_port),
                "--fp",
                fp_arg,
                "--id-dir",
                &self.a_dir.to_string_lossy(),
                "--timeout",
                "30",
            ],
            Duration::from_secs(60),
        )
    }
}

/// Extract `"field":"value"` or `"field":number` from a JSON line (the
/// daemon's NDJSON events have a fixed, flat shape).
fn json_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(value) = rest.strip_prefix('"') {
        let end = value.find('"')?;
        Some(value[..end].to_owned())
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].to_owned())
    }
}

/// Both trust books cross-record the other side, byte-level (the
/// fingerprint hex appears in the file itself, not just via a parser).
fn assert_books_cross_record(a_dir: &Path, a_fp: &str, b_dir: &Path, b_fp: &str) {
    let a_book = std::fs::read_to_string(a_dir.join("peers.book")).expect("a peers.book");
    let b_book = std::fs::read_to_string(b_dir.join("peers.book")).expect("b peers.book");
    assert!(a_book.starts_with("AKPEERS1\n"), "a book header: {a_book}");
    assert!(b_book.starts_with("AKPEERS1\n"), "b book header: {b_book}");
    assert!(
        a_book.contains(&b_fp.to_ascii_lowercase()),
        "owner book must record the redeemer byte-for-byte: {a_book}"
    );
    assert!(
        b_book.contains(&a_fp.to_ascii_lowercase()),
        "redeemer book must record the owner byte-for-byte: {b_book}"
    );
}

// ---- tests ------------------------------------------------------------------

/// The full product chain: registry-server → `invite create` (code
/// printed) → owner `daemon --pairing` → redeemer `daemon --invite-code`
/// → `send` from a fifth process → byte-identical file on the redeemer,
/// `job_done` NDJSON on its stdout, and both `peers.book` files
/// cross-recording the other device.
#[test]
fn full_chain_registry_create_redeem_daemons_send_and_byte_identical_files() {
    let chain = PairedChain::spawn("full");

    // The owner's pairing service observed the redeemer (stderr note in
    // --json-lines mode).
    chain
        .a_daemon
        .stderr
        .wait_for("paired with redeemer", Duration::from_secs(30));

    let src = chain.tmp.path().join("full-src.bin");
    let bytes = payload(256 * 1024);
    std::fs::write(&src, &bytes).expect("write source file");

    let sent = chain.send_to_b(&src, &chain.b_fp);
    assert!(
        sent.success,
        "send failed:\n{}\n{}",
        sent.stdout, sent.stderr
    );

    // The daemon reports the finished job as one NDJSON event.
    let done = chain
        .b_daemon
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    assert_eq!(
        json_field(&done, "bytes")
            .expect("bytes field")
            .parse::<u64>()
            .expect("bytes number"),
        bytes.len() as u64,
        "job_done byte count: {done}"
    );
    assert_eq!(
        json_field(&done, "files")
            .expect("files field")
            .parse::<u64>()
            .expect("files number"),
        1,
        "job_done file count: {done}"
    );
    assert_eq!(
        json_field(&done, "peer").expect("peer"),
        chain.a_fp,
        "job_done names the sender fingerprint"
    );

    let received = std::fs::read(chain.b_out.join("full-src.bin")).expect("received file");
    assert_eq!(received, bytes, "the payload must be byte-identical");

    assert_books_cross_record(&chain.a_dir, &chain.a_fp, &chain.b_dir, &chain.b_fp);
}

/// `invite redeem --once`: pairs, handles exactly one transfer, and then
/// the process itself exits zero — the receive address and fingerprint
/// are parsed from its human stdout contract.
#[test]
fn invite_redeem_once_receives_one_job_then_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server_dir = tmp.path().join("server");
    let a_dir = tmp.path().join("owner-a");
    let b_dir = tmp.path().join("redeemer-b");
    let a_out = tmp.path().join("out-a");
    let b_out = tmp.path().join("out-b");
    for dir in [&server_dir, &a_dir, &b_dir, &a_out, &b_out] {
        std::fs::create_dir_all(dir).expect("create dir");
    }
    let registry = RegistryProc::spawn("once", &server_dir, &tmp.path().join("registry.db"));
    let addr = registry.addr.to_string();
    let fp = registry.fp_hex.clone();

    let created = run_cli(
        &[
            "invite",
            "create",
            "--long-lived",
            "--id-dir",
            &a_dir.to_string_lossy(),
            "--server",
            &addr,
            "--registry-fp",
            &fp,
        ],
        Duration::from_secs(30),
    );
    assert!(created.success, "invite create failed:\n{}", created.stderr);
    let code = parse_created_code(&created.stdout);

    let a_daemon = Proc::spawn(
        "once-owner-daemon",
        &[
            "daemon",
            "--id-dir",
            &a_dir.to_string_lossy(),
            "--out",
            &a_out.to_string_lossy(),
            "--expect",
            "any",
            "--json-lines",
            "--pairing",
            "--name",
            "once-owner",
            "--server",
            &addr,
            "--registry-fp",
            &fp,
        ],
    );
    a_daemon.stderr.wait_for(
        "pairing service: answering redeems",
        Duration::from_secs(30),
    );
    // The owner's true fingerprint comes from the owner daemon's own
    // listening event. (`invite redeem`'s "owner (full):" line is
    // defective: it prints the REDEEMER's (own) fingerprint under the
    // owner label — verified below against this value.)
    let a_listening = a_daemon
        .stdout
        .wait_for("\"type\":\"listening\"", Duration::from_secs(30));
    let a_fp = json_field(&a_listening, "fingerprint")
        .expect("a fingerprint")
        .to_ascii_lowercase();

    let mut b_proc = Proc::spawn(
        "once-redeemer",
        &[
            "invite",
            "redeem",
            &code.display,
            "--once",
            "--out",
            &b_out.to_string_lossy(),
            "--id-dir",
            &b_dir.to_string_lossy(),
            "--server",
            &addr,
            "--registry-fp",
            &fp,
            "--timeout",
            "30",
        ],
    );
    let listening = b_proc
        .stdout
        .wait_for("receiving one transfer into", Duration::from_secs(60));
    let port_text = listening
        .split("listening on ")
        .nth(1)
        .expect("listening addr on the line")
        .trim_end_matches(')');
    let port: u16 = port_text
        .rsplit(':')
        .next()
        .expect("port of the addr")
        .parse()
        .expect("port number");
    let b_fp = b_proc
        .stdout
        .wait_for("fingerprint (full): ", Duration::from_secs(10))
        .split(": ")
        .nth(1)
        .expect("hex fingerprint")
        .trim()
        .to_ascii_lowercase();
    assert_eq!(b_fp.len(), 64, "full fingerprint: {b_fp}");
    // The "owner (full):" line the redeem prints carries the redeemer's
    // OWN fingerprint under the owner label (ak-cli defect, see module
    // notes): it equals this device's own "fingerprint (full)" line, not
    // the owner's.
    let owner_line = b_proc
        .stdout
        .wait_for("owner (full): ", Duration::from_secs(10));
    let after_marker = owner_line
        .split("owner (full): ")
        .nth(1)
        .expect("text after the marker");
    assert!(after_marker.len() >= 64, "marker content: {owner_line}");
    let printed_owner = after_marker[..64].to_ascii_lowercase();
    assert!(
        printed_owner.chars().all(|c| c.is_ascii_hexdigit()),
        "64 hex chars follow the marker: {owner_line}"
    );
    assert_eq!(
        printed_owner, a_fp,
        "the owner line carries the code owner's fingerprint (bug fixed)"
    );
    assert_ne!(
        printed_owner, b_fp,
        "and not the local (redeemer) fingerprint"
    );

    let src = tmp.path().join("once-src.bin");
    let bytes = payload(128 * 1024);
    std::fs::write(&src, &bytes).expect("write source file");
    let sent = run_cli(
        &[
            "send",
            &src.to_string_lossy(),
            "--to",
            &format!("127.0.0.1:{port}"),
            "--fp",
            &b_fp,
            "--id-dir",
            &a_dir.to_string_lossy(),
            "--timeout",
            "30",
        ],
        Duration::from_secs(60),
    );
    assert!(
        sent.success,
        "send failed:\n{}\n{}",
        sent.stdout, sent.stderr
    );

    // --once: the redeemer exits by itself, with success.
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = b_proc.child.try_wait().expect("try_wait redeemer") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "redeemer --once never exited; stdout:\n{}",
            b_proc.stdout.joined()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.success(),
        "redeemer --once exit {status}; stdout:\n{}\nstderr:\n{}",
        b_proc.stdout.joined(),
        b_proc.stderr.joined()
    );
    let b_stdout = b_proc.stdout.joined();
    assert!(b_stdout.contains("job "), "report summary line: {b_stdout}");
    assert!(
        b_stdout.contains("complete"),
        "report summary line: {b_stdout}"
    );
    let received = std::fs::read(b_out.join("once-src.bin")).expect("received file");
    assert_eq!(received, bytes, "the payload must be byte-identical");
    assert_books_cross_record(&a_dir, &a_fp, &b_dir, &b_fp);
}

/// Cross-process state: `--server/--registry-fp` persist the pin once,
/// later invocations reuse it; `invite list` shows real states and the
/// masked hint; `invite revoke` flips the state and forgets the local
/// secret; a foreign identity cannot revoke someone else's code; a
/// malformed code id is refused locally.
#[test]
fn registry_pin_and_invite_lifecycle_across_processes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server_dir = tmp.path().join("server");
    let a_dir = tmp.path().join("owner-a");
    let b_dir = tmp.path().join("stranger-b");
    for dir in [&server_dir, &a_dir, &b_dir] {
        std::fs::create_dir_all(dir).expect("create dir");
    }
    let registry = RegistryProc::spawn("pin", &server_dir, &tmp.path().join("registry.db"));
    let addr = registry.addr.to_string();
    let fp = registry.fp_hex.clone();

    // First call carries the flags and persists the pin.
    let first = run_cli(
        &[
            "invite",
            "create",
            "--long-lived",
            "--id-dir",
            &a_dir.to_string_lossy(),
            "--server",
            &addr,
            "--registry-fp",
            &fp,
        ],
        Duration::from_secs(30),
    );
    assert!(first.success, "create with flags:\n{}", first.stderr);
    let code1 = parse_created_code(&first.stdout);
    // The pin file exists and names the registry (bearer of the endpoint).
    let pin = std::fs::read_to_string(a_dir.join("registry.pin")).expect("registry.pin");
    assert!(pin.starts_with("AKREG1 "), "pin header: {pin}");
    assert!(pin.contains(&addr), "pin names the endpoint: {pin}");
    assert!(
        pin.contains(&fp.to_ascii_lowercase()),
        "pin names the id: {pin}"
    );

    // Later calls need no flags (the pin is reused across processes).
    let second = run_cli(
        &[
            "invite",
            "create",
            "--ttl-secs",
            "600",
            "--id-dir",
            &a_dir.to_string_lossy(),
        ],
        Duration::from_secs(30),
    );
    assert!(second.success, "create via saved pin:\n{}", second.stderr);
    let code2 = parse_created_code(&second.stdout);
    assert_ne!(code1.code_id, code2.code_id);

    let list = run_cli(
        &["invite", "list", "--id-dir", &a_dir.to_string_lossy()],
        Duration::from_secs(30),
    );
    assert!(list.success, "list via saved pin:\n{}", list.stderr);
    for line in list.stdout.lines() {
        if line.starts_with(&code1.code_id) || line.starts_with(&code2.code_id) {
            assert!(line.contains("active"), "fresh codes are active: {line}");
            assert!(
                line.contains("AKI1-"),
                "the local secret yields a masked hint: {line}"
            );
        }
    }
    assert!(
        list.stdout.contains(&code1.code_id) && list.stdout.contains(&code2.code_id),
        "list shows both codes:\n{}",
        list.stdout
    );

    // Revoke the second code; the state flips and the secret is forgotten
    // (the hint disappears from later lists).
    let revoke = run_cli(
        &[
            "invite",
            "revoke",
            &code2.code_id,
            "--id-dir",
            &a_dir.to_string_lossy(),
        ],
        Duration::from_secs(30),
    );
    assert!(revoke.success, "revoke:\n{}", revoke.stderr);
    let list_after = run_cli(
        &["invite", "list", "--id-dir", &a_dir.to_string_lossy()],
        Duration::from_secs(30),
    );
    assert!(
        list_after.success,
        "list after revoke: {}",
        list_after.stderr
    );
    let revoked_line = list_after
        .stdout
        .lines()
        .find(|line| line.starts_with(&code2.code_id))
        .expect("revoked code still listed (tombstone)");
    assert!(
        revoked_line.contains("revoked"),
        "state flipped: {revoked_line}"
    );
    let active_line = list_after
        .stdout
        .lines()
        .find(|line| line.starts_with(&code1.code_id))
        .expect("first code still listed");
    assert!(
        active_line.contains("active"),
        "other code untouched: {active_line}"
    );
    assert!(
        !revoked_line.contains("AKI1-"),
        "the forgotten secret no longer yields a hint: {revoked_line}"
    );
    assert!(
        active_line.contains("AKI1-"),
        "held secret keeps its hint: {active_line}"
    );
    // The bearer secret really left the local store.
    let store = std::fs::read_to_string(a_dir.join("invites.store")).expect("invites.store");
    assert!(
        !store.contains(&code2.display),
        "revoked secret forgotten: {store}"
    );
    assert!(store.contains(&code1.display), "held secret kept: {store}");

    // A malformed code id is refused locally, without touching the server.
    let bad_shape = run_cli(
        &[
            "invite",
            "revoke",
            "nothex",
            "--id-dir",
            &a_dir.to_string_lossy(),
        ],
        Duration::from_secs(30),
    );
    assert!(!bad_shape.success, "malformed code id must fail");
    assert!(
        bad_shape.stderr.contains("32 hex characters"),
        "error names the expected shape: {}",
        bad_shape.stderr
    );

    // A foreign identity cannot revoke the owner's code: it sees neither a
    // local secret nor a list entry.
    let foreign = run_cli(
        &[
            "invite",
            "revoke",
            &code1.code_id,
            "--id-dir",
            &b_dir.to_string_lossy(),
            "--server",
            &addr,
            "--registry-fp",
            &fp,
        ],
        Duration::from_secs(30),
    );
    assert!(
        !foreign.success,
        "a stranger must not revoke a foreign code"
    );
    assert!(
        foreign
            .stderr
            .contains("not in the local store nor the registry list"),
        "foreign revoke error: {}",
        foreign.stderr
    );
    // And the code survives the attempt.
    let list_final = run_cli(
        &["invite", "list", "--id-dir", &a_dir.to_string_lossy()],
        Duration::from_secs(30),
    );
    assert!(list_final.stdout.contains(&code1.code_id));
    assert!(
        list_final
            .stdout
            .lines()
            .any(|line| line.starts_with(&code1.code_id) && line.contains("active")),
        "the code is still active after the foreign attempt:\n{}",
        list_final.stdout
    );
}

/// After a paired chain, `send --fp` accepts a unique 8-hex **prefix**:
/// the pairing-committed peers.book of the sender (written by a different
/// process — the owner daemon) resolves it. The file lands byte-identical
/// and the sender keeps the existing pairing (no duplicate book entry).
#[test]
fn send_after_pairing_resolves_fingerprint_prefix_from_paired_book() {
    let chain = PairedChain::spawn("prefix");
    let book = std::fs::read_to_string(chain.a_dir.join("peers.book")).expect("a peers.book");

    let src = chain.tmp.path().join("prefix-src.bin");
    let bytes = payload(64 * 1024);
    std::fs::write(&src, &bytes).expect("write source file");

    let prefix = chain.b_fp[..8].to_owned();
    let sent = chain.send_to_b(&src, &prefix);
    assert!(
        sent.success,
        "send with prefix {prefix} failed:\n{}\n{}",
        sent.stdout, sent.stderr
    );
    assert!(
        sent.stdout.contains("sending 1 path(s) to peer"),
        "send reports the resolved peer:\n{}",
        sent.stdout
    );

    let done = chain
        .b_daemon
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    assert_eq!(
        json_field(&done, "bytes")
            .expect("bytes field")
            .parse::<u64>()
            .expect("bytes number"),
        bytes.len() as u64
    );
    let received = std::fs::read(chain.b_out.join("prefix-src.bin")).expect("received file");
    assert_eq!(received, bytes, "the payload must be byte-identical");

    // Exactly one entry for the redeemer existed before the send and the
    // TOFU path must not have duplicated it.
    let book_after = std::fs::read_to_string(chain.a_dir.join("peers.book")).expect("book");
    let hits = book_after
        .lines()
        .filter(|line| line.contains(&chain.b_fp))
        .count();
    let hits_before = book
        .lines()
        .filter(|line| line.contains(&chain.b_fp))
        .count();
    assert_eq!(hits, hits_before, "no duplicate peer entries after send");
    assert_eq!(hits, 1, "exactly one redeemer entry: {book_after}");
}
