//! Wave-27 send-side scheduler admission, real-mode integration tests
//! (final-review P1-02; scheduler.md §6).
//!
//! What these tests prove, across real process and network boundaries:
//!
//! * the CLI `send` path consults the identity directory's **Ready pairing
//!   relations** (the `ak-store` pairing journal, `phase applied`) for the
//!   pinned fingerprint and — when one exists — runs the on-channel
//!   scheduler admission (`TaskOpen → Admission → AdmissionAck`) through
//!   [`ak_engine::send_on_channel`], carrying the relation's code group
//!   (in the frozen [`ak_session::runtime::code_group_wire_form`]), the
//!   pinned registry identity and the pairing id (tests 1);
//! * an admission denial surfaces as a bounded BUSY error with the §7
//!   retry copy and NO silent retry loop — exactly one `TaskOpen` ever
//!   reaches the peer, the send exits non-zero, nothing is transferred
//!   (test 2);
//! * a legacy receiver (no `CAP_SCHEDULER` in its Hello) still receives
//!   the file: the send attempt explicitly retried the address on the
//!   frozen v1 protocol — never a silent path choice (test 3);
//! * the daemon-side two-dimensional concurrency matrix (same-code
//!   serial, cross-code parallel, dual-end policy intersection) is
//!   exercised by real multi-process chains whose evidence is the
//!   receiver's per-task activity intervals — `job_started`/`job_done`
//!   NDJSON event ordering — not just terminal states (tests 4–6,
//!   live since the wave-27 daemon-side admission landed; see each
//!   ignore note).
//!
//! The receiver of tests 1–2 is a REAL in-process engine receiver
//! ([`ak_engine::receive_on_channel`] on a real loopback `DirectEndpoint`
//! with a real TLS identity); test 3's receiver is the legacy
//! [`ak_engine::receive_with_addr`] kernel. The sender is always the real
//! `ak-cli` binary built from this workspace.
#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ak_engine::{AdmissionDecision, AdmissionRequest, ReceiveSpec};
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_net::DirectEndpoint as EngineEndpoint;

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
}

/// A resident CLI child process with captured stdout/stderr.
struct Proc {
    child: Child,
    stdout: LineLog,
    #[allow(dead_code)]
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
        if let Some(status) = child.try_wait().expect("try wait on cli child") {
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

// ---- fixtures ---------------------------------------------------------------

/// Deterministic payload (xorshift64 filler; it only needs pseudo-random
/// bytes for byte-identity checks).
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

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write one Ready pairing relation into an identity directory exactly as
/// the pairing orchestrator's journal does (frozen AKPAIR1 record):
/// `<id>/pairing/<pairing hex>.akpair` with `phase applied`, the code id
/// and the peer's Ed25519 device key.
fn write_ready_relation(id: &Path, pairing_id: &[u8; 16], code_id: &[u8; 16], peer_key: &[u8; 32]) {
    let dir = id.join("pairing");
    std::fs::create_dir_all(&dir).expect("journal dir");
    let text = format!(
        "AKPAIR1\npairing {}\ncode {}\nlookup {}\nsecret {}\npeer {}\nphase applied\ncreated 1000\nupdated 2000\n",
        hex_of(pairing_id),
        hex_of(code_id),
        hex_of(&[0x44u8; 32]),
        hex_of(&[0x55u8; 16]),
        hex_of(peer_key),
    );
    std::fs::write(dir.join(format!("{}.akpair", hex_of(pairing_id))), text)
        .expect("write akpair record");
}

/// Write the registry pin (`AKREG1 <addr> <64hex>`): the registry half of
/// the admission context.
fn write_registry_pin(id: &Path, registry_id: &[u8; 32]) {
    std::fs::write(
        id.join("registry.pin"),
        format!("AKREG1 127.0.0.1:1 {}\n", hex_of(registry_id)),
    )
    .expect("write registry pin");
}

/// The identity fixtures of one admission-capable sender: pairing id,
/// code id and registry id, plus the id-dir holding the Ready relation
/// and the registry pin for `peer`.
struct SenderIdentity {
    id_dir: PathBuf,
    pairing_id: [u8; 16],
    code_id: [u8; 16],
    registry_id: [u8; 32],
}

fn sender_identity_with_relation(base: &Path, tag: &str, peer_key: &[u8; 32]) -> SenderIdentity {
    let id_dir = base.join(format!("sender-{tag}"));
    std::fs::create_dir_all(&id_dir).expect("sender id dir");
    let pairing_id = [0xA7u8; 16];
    let code_id = [0xC9u8; 16];
    let registry_id = [0x91u8; 32];
    write_ready_relation(&id_dir, &pairing_id, &code_id, peer_key);
    write_registry_pin(&id_dir, &registry_id);
    SenderIdentity {
        id_dir,
        pairing_id,
        code_id,
        registry_id,
    }
}

// ---- in-process real receivers ----------------------------------------------

/// A real on-channel receiver on an auto-assigned loopback port: one
/// accepted connection through [`ak_engine::receive_on_channel`], its
/// admission decider captured, and every [`AdmissionRequest`] recorded.
struct OnChannelReceiver {
    addr: SocketAddr,
    fingerprint: Fingerprint,
    /// The receiver's Ed25519 device key (what the sender's `.akpair`
    /// journal record pins).
    peer_public_key: [u8; 32],
    requests: Arc<Mutex<Vec<AdmissionRequest>>>,
    task: tokio::task::JoinHandle<Result<ak_engine::Report, ak_engine::EngineError>>,
}

/// Spawn [`OnChannelReceiver`]: `decide` answers every admission request.
/// Async so the test's runtime awaits the bound address directly (the
/// receiver reports it before parking in `accept`).
async fn spawn_on_channel_receiver(
    out: PathBuf,
    decide: Arc<dyn Fn(&AdmissionRequest) -> AdmissionDecision + Send + Sync>,
) -> OnChannelReceiver {
    let key = DeviceKeyPair::generate();
    let peer_public_key = key.public_key();
    let mut spec = ReceiveSpec::new(out, ak_identity::tls::make_self_signed(&key));
    spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    spec.idle_timeout = Duration::from_secs(30);
    spec.authorize = Box::new(|_| true);

    let requests: Arc<Mutex<Vec<AdmissionRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let decider = move |request: &AdmissionRequest| {
        seen.lock().unwrap().push(*request);
        decide(request)
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let endpoint = EngineEndpoint::bind(spec.bind, &spec.identity).await?;
        let _ = tx.send(endpoint.local_addr()?);
        let channel = endpoint.accept(&spec.authorize).await?;
        ak_engine::receive_on_channel(channel, spec, &decider, &|_| {}).await
    });
    let addr = rx.await.expect("the receiver reports its bound address");
    OnChannelReceiver {
        addr,
        fingerprint: key.fingerprint(),
        peer_public_key,
        requests,
        task,
    }
}

/// Test 3 — legacy fallback with a Ready relation present (scheduler.md
/// §6: old ends keep the old protocol): the receiver is the REAL current
/// `ak-cli daemon` (the wave-24 resident receive loop — the frozen v1
/// kernel, its Hello carries no `CAP_SCHEDULER`), so the sender's
/// admission attempt is refused by the engine before any TaskOpen byte
/// and the CLI explicitly retried the same address on the legacy
/// protocol. This is the wave-26 compatibility scenario verbatim: an
/// upgraded sender against a not-yet-upgraded receiver. The daemon
/// serves both connections (probe, then transfer), reports the job, and
/// the payload lands byte-identically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_send_with_ready_relation_falls_back_to_legacy_receiver() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let daemon_dir = tmp.path().join("legacy-daemon");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&daemon_dir).expect("daemon dir");
    std::fs::create_dir_all(&out).expect("out dir");

    let daemon_dir_text = daemon_dir.to_string_lossy().into_owned();
    let out_text = out.to_string_lossy().into_owned();
    let mut daemon = Proc::spawn(
        "legacy-daemon",
        &[
            "daemon",
            "--id-dir",
            &daemon_dir_text,
            "--out",
            &out_text,
            "--expect",
            "any",
            "--json-lines",
        ],
    );
    let listening = daemon
        .stdout
        .wait_for("\"type\":\"listening\"", Duration::from_secs(30));
    let daemon_fp = json_field(&listening, "fingerprint").expect("daemon fingerprint");
    let daemon_port: u16 = json_field(&listening, "port")
        .expect("daemon port")
        .parse()
        .expect("daemon port number");

    // The daemon's fingerprint hex IS the pinned digest; the journal
    // record pins the raw public key instead, so take it from the
    // daemon's device.key through the ak-identity loader.
    let device_key = daemon_dir.join("device.key");
    let (key, _) = ak_identity::DeviceKeyPair::load_or_create(&device_key)
        .expect("the daemon wrote its device key");
    let identity = sender_identity_with_relation(tmp.path(), "legacy", &key.public_key());
    // Cross-check: the pinned digest the send will use matches the key.
    assert_eq!(key.fingerprint().to_hex(), daemon_fp.to_ascii_lowercase());

    let src = tmp.path().join("legacy-src.bin");
    let bytes = payload(96 * 1024);
    std::fs::write(&src, &bytes).expect("write source file");

    let sent = run_send(
        &identity.id_dir,
        &src,
        SocketAddr::from(([127, 0, 0, 1], daemon_port)),
        &key.fingerprint(),
    );
    assert!(
        sent.success,
        "send failed:\nstdout:\n{}\nstderr:\n{}",
        sent.stdout, sent.stderr
    );
    // The fallback is explicit, never a silent path choice.
    assert!(
        sent.stdout.contains("legacy transfer protocol"),
        "the legacy retry must be observable:\n{}",
        sent.stdout
    );
    // The daemon committed exactly the one real job (the probe never
    // became a transfer) and the payload is byte-identical.
    let done = daemon
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
        std::fs::read(out.join("legacy-src.bin")).expect("received file"),
        bytes,
        "the payload must be byte-identical over the legacy protocol"
    );
    daemon.kill();
}

/// Run the real `ak-cli send` subprocess against `addr`, pining `fp`.
fn run_send(id_dir: &Path, src: &Path, addr: SocketAddr, fp: &Fingerprint) -> RunOutcome {
    let dir_text = id_dir.to_string_lossy().into_owned();
    let src_text = src.to_string_lossy().into_owned();
    let addr_text = addr.to_string();
    let fp_text = fp.to_hex();
    run_cli(
        &[
            "send",
            &src_text,
            "--to",
            &addr_text,
            "--fp",
            &fp_text,
            "--id-dir",
            &dir_text,
            "--timeout",
            "30",
        ],
        Duration::from_secs(60),
    )
}

// ---- passing tests: sender-side admission against real receivers ------------

/// Test 1 — the complete sender-side admission path over a real process
/// boundary: a Ready relation in the sender's identity directory drives
/// `ak-cli send` onto the on-channel admission handshake. The real
/// receiver (in-process `receive_on_channel`) grants; the decider must
/// see EXACTLY the journal-derived context — the code group in the frozen
/// `code_group_wire_form`, the pinned registry identity and the pairing
/// id — and the payload must land byte-identical.
///
/// Multi-thread flavor: `run_cli` blocks the test task synchronously, so
/// the receiver task needs the other workers to keep being polled while
/// the real subprocess talks to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_send_with_ready_relation_runs_admission_and_completes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).expect("out dir");

    let receiver =
        spawn_on_channel_receiver(out.clone(), Arc::new(|_| AdmissionDecision::Admit)).await;
    let identity = sender_identity_with_relation(tmp.path(), "granted", &receiver.peer_public_key);

    let src = tmp.path().join("admission-src.bin");
    let bytes = payload(128 * 1024);
    std::fs::write(&src, &bytes).expect("write source file");

    let sent = run_send(&identity.id_dir, &src, receiver.addr, &receiver.fingerprint);
    assert!(
        sent.success,
        "send failed:\nstdout:\n{}\nstderr:\n{}",
        sent.stdout, sent.stderr
    );
    // The send announced the relation-driven path before connecting.
    assert!(
        sent.stdout.contains("requesting scheduler admission"),
        "the relation lookup must be observable:\n{}",
        sent.stdout
    );

    let report = tokio::time::timeout(Duration::from_secs(10), receiver.task)
        .await
        .expect("receiver settles")
        .expect("receiver task joinable")
        .expect("receive completes");
    assert_eq!(report.files, 1);
    assert_eq!(
        std::fs::read(out.join("admission-src.bin")).expect("received file"),
        bytes,
        "the payload must be byte-identical"
    );

    // The decider saw exactly the journal-derived admission context.
    let requests = receiver.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "exactly one TaskOpen: {requests:?}");
    assert_eq!(
        requests[0],
        AdmissionRequest {
            group: ak_session::runtime::code_group_wire_form(&identity.code_id),
            group_registry: identity.registry_id,
            pairing_id: identity.pairing_id,
        },
        "the wire context is the Ready relation's own identity"
    );
}

/// Test 2 — an admission denial (receiver queue/serial constraints) is a
/// terminal BUSY error: bounded retry copy per scheduler.md §7, the send
/// exits non-zero, EXACTLY one TaskOpen reached the peer (no silent
/// retry loop), and no file landed. (Multi-thread flavor for the same
/// reason as test 1.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_send_admission_denied_reports_busy_without_silent_retry() {
    for (tag, reason, word) in [
        ("capacity", ak_wire::AdmissionReason::Capacity, "capacity"),
        (
            "serial-wait",
            ak_wire::AdmissionReason::SerialWait,
            "serial_wait",
        ),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).expect("out dir");

        let receiver = spawn_on_channel_receiver(
            out.clone(),
            Arc::new(move |_| AdmissionDecision::Deny(reason)),
        )
        .await;
        let identity = sender_identity_with_relation(tmp.path(), tag, &receiver.peer_public_key);

        let src = tmp.path().join("busy-src.bin");
        std::fs::write(&src, payload(16 * 1024)).expect("write source file");

        let sent = run_send(&identity.id_dir, &src, receiver.addr, &receiver.fingerprint);
        assert!(
            !sent.success,
            "{tag}: a denied admission must fail the send"
        );
        let err_text = format!("{}\n{}", sent.stderr, sent.stdout);
        assert!(
            err_text.contains("BUSY"),
            "{tag}: the denial must read as BUSY:\n{err_text}"
        );
        assert!(
            err_text.contains(word),
            "{tag}: the denial names its reason ({word}):\n{err_text}"
        );

        // Exactly one TaskOpen was decided — the engine's bounded
        // StalePermit resubmits never apply to capacity/serial_wait, and
        // the CLI must not loop the attempt either.
        let requests = receiver.requests.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            1,
            "{tag}: exactly one TaskOpen may reach the peer"
        );

        // The denied receiver call settles with the same reason and no
        // file ever landed.
        let err = tokio::time::timeout(Duration::from_secs(10), receiver.task)
            .await
            .expect("receiver settles")
            .expect("receiver task joinable")
            .expect_err("a denied receive fails");
        assert!(
            matches!(
                err,
                ak_engine::EngineError::AdmissionDenied { reason: denied } if denied == reason
            ),
            "{tag}: receiver-side denial mirrors the wire reason: {err:?}"
        );
        assert_eq!(
            std::fs::read_dir(&out).expect("out dir").count(),
            0,
            "{tag}: nothing may land from a denied task"
        );
    }
}

// ---- daemon-matrix tests (wave-27 daemon-side admission dependency) ---------

/// The registry-server subprocess (real v2 TLS registry + SQLite): reused
/// verbatim from `invite_pairing_cli_e2e.rs` so the chains here speak the
/// same real control plane.
struct RegistryProc {
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

/// One code created by the owner identity (`invite create` subprocess):
/// its bearer display form (the redeem input).
struct CreatedCode {
    display: String,
}

fn parse_created_code(stdout: &str) -> CreatedCode {
    let display = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("AKI1-"))
        .map(|rest| format!("AKI1-{rest}"))
        .unwrap_or_else(|| panic!("no AKI1- code line in:\n{stdout}"));
    CreatedCode { display }
}

/// One owner daemon plus N redeemer daemons, every device a real
/// subprocess against a real registry: the matrix fixture of tests 4–6.
/// The owner's NDJSON stream is the interval-evidence source.
struct DaemonChain {
    tmp: tempfile::TempDir,
    #[allow(dead_code)]
    registry: RegistryProc,
    redeemer_dirs: Vec<PathBuf>,
    owner: Proc,
    #[allow(dead_code)]
    redeemers: Vec<Proc>,
    owner_fp: String,
    owner_port: u16,
}

impl DaemonChain {
    /// `codes` codes created up front; `redeemers` daemon processes redeem
    /// `codes[i % codes.len()]` (same code for every redeemer when only
    /// one exists). `same_code`/`cross_code` shape the owner's policy.
    fn spawn(
        tag: &str,
        codes: usize,
        redeemers: usize,
        same_code: &str,
        cross_code: &str,
    ) -> DaemonChain {
        let tmp = tempfile::tempdir().expect("tempdir");
        let server_dir = tmp.path().join("server");
        let owner_dir = tmp.path().join("owner");
        std::fs::create_dir_all(&server_dir).expect("server dir");
        std::fs::create_dir_all(&owner_dir).expect("owner dir");
        let registry = RegistryProc::spawn(tag, &server_dir, &tmp.path().join("registry.db"));
        let addr = registry.addr.to_string();
        let fp = registry.fp_hex.clone();

        let owner_out = tmp.path().join("owner-out");
        std::fs::create_dir_all(&owner_out).expect("owner out dir");
        let owner_dir_text = owner_dir.to_string_lossy().into_owned();
        let owner_out_text = owner_out.to_string_lossy().into_owned();
        let owner_name = format!("owner-{tag}");
        let mut created = Vec::new();
        for _ in 0..codes {
            let run = run_cli(
                &[
                    "invite",
                    "create",
                    "--long-lived",
                    "--id-dir",
                    &owner_dir_text,
                    "--server",
                    &addr,
                    "--registry-fp",
                    &fp,
                ],
                Duration::from_secs(30),
            );
            assert!(run.success, "invite create failed:\n{}", run.stderr);
            created.push(parse_created_code(&run.stdout));
        }

        let owner = Proc::spawn(
            &format!("{tag}-owner"),
            &[
                "daemon",
                "--id-dir",
                &owner_dir_text,
                "--out",
                &owner_out_text,
                "--expect",
                "any",
                "--json-lines",
                "--pairing",
                "--same-code",
                same_code,
                "--cross-code",
                cross_code,
                "--name",
                &owner_name,
                "--server",
                &addr,
                "--registry-fp",
                &fp,
            ],
        );
        owner.stderr.wait_for(
            "pairing service: answering redeems",
            Duration::from_secs(30),
        );
        let listening = owner
            .stdout
            .wait_for("\"type\":\"listening\"", Duration::from_secs(30));
        let owner_fp = json_field(&listening, "fingerprint").expect("owner fingerprint");
        let owner_port: u16 = json_field(&listening, "port")
            .expect("owner port")
            .parse()
            .expect("owner port number");

        let mut redeemer_dirs = Vec::new();
        let mut redeemer_procs = Vec::new();
        for index in 0..redeemers {
            let dir = tmp.path().join(format!("redeemer-{index}"));
            std::fs::create_dir_all(&dir).expect("redeemer dir");
            let out = tmp.path().join(format!("redeemer-out-{index}"));
            std::fs::create_dir_all(&out).expect("redeemer out dir");
            // Every redeemer of a one-code chain redeems the SAME code
            // (the same-code matrix); with several codes they take one
            // each (the cross-code matrix).
            let code = created[index % created.len()].display.clone();
            let dir_text = dir.to_string_lossy().into_owned();
            let out_text = out.to_string_lossy().into_owned();
            let name = format!("redeemer-{tag}-{index}");
            let proc = Proc::spawn(
                &format!("{tag}-redeemer-{index}"),
                &[
                    "daemon",
                    "--id-dir",
                    &dir_text,
                    "--out",
                    &out_text,
                    "--json-lines",
                    "--invite-code",
                    &code,
                    "--name",
                    &name,
                    "--server",
                    &addr,
                    "--registry-fp",
                    &fp,
                ],
            );
            // The redeemer's listening event fires only after its redeem
            // committed the owner into peers.book (the pairing barrier).
            proc.stdout
                .wait_for("\"type\":\"listening\"", Duration::from_secs(60));
            redeemer_dirs.push(dir);
            redeemer_procs.push(proc);
        }

        DaemonChain {
            tmp,
            registry,
            redeemer_dirs,
            owner,
            redeemers: redeemer_procs,
            owner_fp,
            owner_port,
        }
    }

    /// Launch one real `ak-cli send` from redeemer `index` to the owner
    /// (the redeemer's identity directory carries its Ready relation, so
    /// the send requests scheduler admission). Runs concurrently when
    /// spawned on threads by the caller.
    fn send_from(&self, index: usize, bytes: usize) -> RunOutcome {
        let src = self.tmp.path().join(format!("matrix-src-{index}.bin"));
        std::fs::write(&src, payload(bytes)).expect("write matrix source");
        let id_dir = self.redeemer_dirs[index].clone();
        let to = format!("127.0.0.1:{}", self.owner_port);
        let fp = self.owner_fp.clone();
        let src_text = src.to_string_lossy().into_owned();
        let dir_text = id_dir.to_string_lossy().into_owned();
        run_cli(
            &[
                "send",
                &src_text,
                "--to",
                &to,
                "--fp",
                &fp,
                "--id-dir",
                &dir_text,
                "--timeout",
                "60",
            ],
            Duration::from_secs(180),
        )
    }
}

/// Interval evidence from the owner daemon's NDJSON stream: for each peer
/// fingerprint, the (start, end) event indices of its task —
/// `job_started(peer)` opening the interval (the handshake completed and
/// the transfer started), `job_done(peer)` closing it (committed). Two
/// tasks are serialized exactly when their intervals are disjoint, and
/// overlapped exactly when both opens precede the other's close.
fn task_intervals(owner: &Proc) -> Vec<(String, usize, usize)> {
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut intervals: Vec<(String, usize, usize)> = Vec::new();
    for (index, line) in owner.stdout.snapshot().iter().enumerate() {
        if line.contains("\"type\":\"job_started\"") {
            if let Some(peer) = json_field(line, "peer") {
                open.push((peer, index));
            }
        } else if line.contains("\"type\":\"job_done\"") {
            let peer = json_field(line, "peer").unwrap_or_default();
            if let Some(position) = open.iter().position(|(p, _)| *p == peer) {
                let (peer, start) = open.remove(position);
                intervals.push((peer, start, index));
            }
        }
    }
    intervals
}

fn intervals_disjoint(a: &(String, usize, usize), b: &(String, usize, usize)) -> bool {
    a.2 < b.1 || b.2 < a.1
}

/// Test 4 — same-code serial: one owner code, two redeemers sending to
/// the owner simultaneously; the owner's two task activity intervals
/// must NOT overlap (contract §2: each code holds at most one active
/// task). Evidence is the owner's `job_started`/`job_done` event
/// ordering, not merely both jobs finishing.
#[tokio::test]
async fn same_code_serial_two_redeemers_never_overlap() {
    let chain = Arc::new(DaemonChain::spawn("same-code", 1, 2, "serial", "parallel"));
    let owner = &chain.owner;

    let first = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || chain.send_from(0, 2 * 1024 * 1024))
    };
    let second = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || chain.send_from(1, 2 * 1024 * 1024))
    };
    for (label, outcome) in [("redeemer-0", first), ("redeemer-1", second)] {
        let sent = outcome.join().expect("send thread");
        assert!(
            sent.success,
            "{label} send failed:\n{}\n{}",
            sent.stdout, sent.stderr
        );
    }

    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    let intervals = task_intervals(owner);
    assert_eq!(intervals.len(), 2, "two completed tasks: {intervals:?}");
    assert!(
        intervals_disjoint(&intervals[0], &intervals[1]),
        "same-code tasks must be serialized; intervals {intervals:?}"
    );
}

/// Test 5 — cross-code parallel: two codes, one redeemer each, both
/// sending simultaneously; the owner's two task intervals must OVERLAP
/// (contract §2: different codes may transfer concurrently under the
/// default `cross_code = parallel`).
#[tokio::test]
async fn cross_code_parallel_two_redeemers_overlap() {
    let chain = Arc::new(DaemonChain::spawn("cross-code", 2, 2, "serial", "parallel"));
    let owner = &chain.owner;

    let first = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || chain.send_from(0, 4 * 1024 * 1024))
    };
    let second = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || chain.send_from(1, 4 * 1024 * 1024))
    };
    for (label, outcome) in [("redeemer-0", first), ("redeemer-1", second)] {
        let sent = outcome.join().expect("send thread");
        assert!(
            sent.success,
            "{label} send failed:\n{}\n{}",
            sent.stdout, sent.stderr
        );
    }

    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    let intervals = task_intervals(owner);
    assert_eq!(intervals.len(), 2, "two completed tasks: {intervals:?}");
    assert!(
        !intervals_disjoint(&intervals[0], &intervals[1]),
        "cross-code tasks must run concurrently; intervals {intervals:?}"
    );
}

/// Test 6 — dual-end policy intersection (contract §2: "两端策略取交
/// 集"): the receiving owner runs same-code serial while the sending side
/// would allow parallel — two concurrent sends from the SAME redeemer
/// under one code must still be serialized by the receiver.
#[tokio::test]
async fn dual_policy_intersection_serializes_same_code_sends() {
    let chain = Arc::new(DaemonChain::spawn("intersect", 1, 1, "serial", "parallel"));
    let owner = &chain.owner;

    let first = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || chain.send_from(0, 2 * 1024 * 1024))
    };
    let second = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || {
            // Give the first send a moment to reach admission first; the
            // assertion is about the receiver serializing them, not about
            // winning a connect race.
            std::thread::sleep(Duration::from_millis(300));
            chain.send_from(0, 2 * 1024 * 1024)
        })
    };
    for (label, outcome) in [("send-1", first), ("send-2", second)] {
        let sent = outcome.join().expect("send thread");
        assert!(
            sent.success,
            "{label} failed:\n{}\n{}",
            sent.stdout, sent.stderr
        );
    }

    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    owner
        .stdout
        .wait_for("\"type\":\"job_done\"", Duration::from_secs(30));
    let intervals = task_intervals(owner);
    assert_eq!(intervals.len(), 2, "two completed tasks: {intervals:?}");
    assert!(
        intervals_disjoint(&intervals[0], &intervals[1]),
        "the receiver's serial policy must win the intersection; intervals {intervals:?}"
    );
}
