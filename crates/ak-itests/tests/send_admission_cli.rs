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

use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ak_engine::{AdmissionDecision, AdmissionRequest, ReceiveSpec};
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_net::DirectEndpoint as EngineEndpoint;

mod common;

// The chain plumbing is the one the T03 test checks (plan of 2026-11-28,
// section F): both pipes and the exit status of every process, frozen
// once for a report, and the registry database sampled next to it.
use common::report::{
    argv_summary, live_report, mark_decision_cursor, redact, registry_snapshot_about, Proc,
    RegistryProc, Stream,
};

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
        .unwrap_or_else(|err| {
            let owned: Vec<String> = args.iter().map(|text| text.to_string()).collect();
            panic!("spawn ({}): {err}", argv_summary(&owned))
        });
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

/// One CLI child of this file's chains, with the pairing trace on (the
/// shared `Proc::spawn` does that for every child). It is registered for
/// the reports of this thread from its first moment.
fn spawn_proc(tag: &str, args: &[&str]) -> Proc {
    let owned: Vec<String> = args.iter().map(|text| text.to_string()).collect();
    Proc::spawn(tag, &owned)
}

fn read_all(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
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
    let mut daemon = spawn_proc(
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
        .unwrap_or_else(|| panic!("no AKI1- code line in:\n{}", redact(stdout)));
    CreatedCode { display }
}

// ---- calibration of the capture (plan of 2026-11-28, section F) -----------

/// The pairing the calibration asks the registry about: synthetic, not
/// zero, and in no database.
const CALIBRATION_PAIRING: [u8; 16] = [0xA5; 16];

/// What the calibration collected on one registry instance.
#[derive(Clone)]
struct Calibration {
    /// The registry's stderr when the calibration ended.
    registry_err: Vec<String>,
    /// What the calibration client took off the wire for the synthetic
    /// pairing, if it took anything.
    taken: Option<ak_signal::protocol_v2::CtrlMessage>,
    /// The calibration device's first four bytes, as the notice line names
    /// the device.
    device: [u8; 4],
    /// The snapshot's look at the control pairing.
    transactions: String,
    /// Why the calibration identity could not reach the registry, when it
    /// could not: then nothing else was collected. Carried back instead of
    /// a panic, so the verdict and the report come from the test's own
    /// thread, which holds the registry.
    unreached: Option<String>,
}

/// Which of the registry's two calibration lines are on its stderr: the
/// decision for the synthetic pairing, and the notice queued to the
/// calibration device.
fn calibration_lines(registry_err: &[String], device: &[u8; 4]) -> (bool, bool) {
    let prefix = hex_of(&CALIBRATION_PAIRING[..4]);
    let device = hex_of(device);
    let decision = registry_err.iter().any(|line| {
        line.contains(&format!(
            "answered unavailable: no transaction to prepare (pairing {prefix})"
        ))
    });
    let notice = registry_err.iter().any(|line| {
        line.contains(&format!("pairing {prefix} notice "))
            && line.contains(&format!(" to the requester {device}: "))
            && line.contains("; enqueued")
    });
    (decision, notice)
}

/// Whether the calibration proves that this registry's decisions are being
/// captured. Four things must be there: the registry's decision line for
/// the synthetic pairing, the notice queued to the calibration device,
/// the `unavailable` the calibration client took for that pairing, and
/// no row for it in the snapshot. Anything missing is red — also the lines
/// of another registry instance, which hold none of this one's.
fn calibration_verdict(calibration: &Calibration) -> Result<(), String> {
    use ak_signal::protocol_v2::{CtrlMessage, ErrorScope, PAIR_UNAVAILABLE};
    if let Some(reason) = &calibration.unreached {
        return Err(format!(
            "the calibration identity could not reach the registry: {reason}"
        ));
    }
    let (decision, notice) = calibration_lines(&calibration.registry_err, &calibration.device);
    let taken = matches!(
        calibration.taken,
        Some(CtrlMessage::PairError {
            scope: ErrorScope::Pairing,
            object_id,
            code: PAIR_UNAVAILABLE,
            ..
        }) if object_id == CALIBRATION_PAIRING
    );
    let no_row = calibration
        .transactions
        .contains("\"alias\": \"calibration\", \"matches\": 0, \"row\": null");
    let mut missing = Vec::new();
    if !decision {
        missing.push("the registry's decision line for the pairing");
    }
    if !notice {
        missing.push("the notice queued to the calibration device");
    }
    if !taken {
        missing.push("the `unavailable` the calibration client took");
    }
    if !no_row {
        missing.push("zero rows for the control pairing in the snapshot");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the calibration on this registry is missing: {}",
            missing.join(", ")
        ))
    }
}

/// Run the calibration on `registry`. A calibration identity subscribes to
/// the plane and sends a Prepared for a pairing that does not exist — the
/// registry's one certain exit for that — then the registry's stderr, the
/// client's answer and the snapshot are collected within one bounded
/// window. It proves only that this registry instance and this capture
/// path work; a natural attempt is aligned on its own lines.
async fn collect_calibration(registry: &RegistryProc) -> Calibration {
    let key = DeviceKeyPair::generate();
    let mut registry_id = [0u8; 32];
    for (index, slot) in registry_id.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&registry.fp_hex[index * 2..index * 2 + 2], 16)
            .expect("the registry's pin is hex");
    }
    let client = match ak_session::pairing::RegistryClient::connect(
        registry.addr,
        registry_id,
        &key,
        "calibration",
    )
    .await
    {
        Ok(client) => client,
        Err(error) => {
            // No panic here: this is the calibration's own thread, which
            // holds no process. The registry's lines are read through the
            // shared log, and the verdict is given on the test's thread.
            return Calibration {
                registry_err: registry
                    .proc
                    .stdout
                    .lines_of(Stream::Stderr)
                    .into_iter()
                    .map(|line| line.text)
                    .collect(),
                taken: None,
                device: key.public_key()[..4].try_into().expect("four bytes"),
                transactions: String::new(),
                unreached: Some(redact(&error.to_string())),
            };
        }
    };
    let mut plane = client.subscribe_plane();
    client
        .send_pair_prepared(CALIBRATION_PAIRING, [0x5A; 32], [0u8; 64])
        .await
        .expect("the calibration client sends its Prepared");
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut taken = None;
    let registry_err = loop {
        if taken.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Ok(Some(message)) = tokio::time::timeout(left, plane.recv()).await {
                taken = Some(message);
            }
        }
        let registry_err = registry
            .proc
            .stdout
            .lines_of(Stream::Stderr)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        // With all three parts in, the registry's stderr is read once more
        // after a short grace, so a line written right behind the notice
        // is not cut off.
        let (decision, notice) = calibration_lines(
            &registry_err,
            &key.public_key()[..4].try_into().expect("four bytes"),
        );
        if taken.is_some() && decision && notice {
            std::thread::sleep(Duration::from_millis(150));
            break registry
                .proc
                .stdout
                .lines_of(Stream::Stderr)
                .into_iter()
                .map(|line| line.text)
                .collect::<Vec<_>>();
        }
        if Instant::now() >= deadline {
            break registry_err;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let answered_ms = started.elapsed().as_millis();
    let transactions = registry_snapshot_about(
        &registry.db,
        &[(
            "calibration",
            CALIBRATION_PAIRING[..4].try_into().expect("four bytes"),
        )],
    );
    let looked_up_ms = started.elapsed().as_millis();
    client.close().await;
    // How long each part took, for the handoff's timings.
    eprintln!(
        "t03 calibration: the answer, the decision line and the notice were in after {answered_ms} ms; \
         the snapshot lookup ended at {looked_up_ms} ms; the calibration connection closed at {} ms",
        started.elapsed().as_millis()
    );
    let device: [u8; 4] = key.public_key()[..4].try_into().expect("four bytes");
    Calibration {
        registry_err,
        taken,
        device,
        transactions,
        unreached: None,
    }
}

/// Run the calibration on a thread of its own (the chain fixture is
/// synchronous) and demand that it passes.
fn calibrate(registry: &RegistryProc) {
    let calibration = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Runtime::new()
                    .expect("a runtime for the calibration")
                    .block_on(collect_calibration(registry))
            })
            .join()
            .expect("the calibration thread")
    });
    if let Err(missing) = calibration_verdict(&calibration) {
        panic!("{missing}\n{}", live_report());
    }
}

/// Plan of 2026-11-28, section F: the verdict is red whenever one of its
/// four parts is missing — among others with the lines of another registry
/// instance, which have none of this one's.
#[test]
fn the_calibration_is_red_without_any_of_its_four_parts() {
    use ak_signal::protocol_v2::{CtrlMessage, ErrorScope, PAIR_UNAVAILABLE};
    let prefix = hex_of(&CALIBRATION_PAIRING[..4]);
    let device = [0x11u8; 4];
    let decision = format!(
        "[.. DEBUG ak_signal::server_v2::engine::pairing] prepared from 1a2b3c4d \
         answered unavailable: no transaction to prepare (pairing {prefix})"
    );
    let notice = format!(
        "[.. DEBUG ak_signal::server_v2::engine] pairing {prefix} notice 1 to the requester {}: \
         prepared answered unavailable: no transaction on record; enqueued",
        hex_of(&device)
    );
    let taken = || {
        Some(CtrlMessage::PairError {
            request_id: [0u8; 16],
            scope: ErrorScope::Pairing,
            object_id: CALIBRATION_PAIRING,
            code: PAIR_UNAVAILABLE,
            retry_after_ms: 0,
        })
    };
    let whole = Calibration {
        registry_err: vec![decision.clone(), notice.clone()],
        taken: taken(),
        device,
        transactions: "{\"about\": [{\"alias\": \"calibration\", \"matches\": 0, \"row\": null}]}"
            .to_string(),
        unreached: None,
    };
    assert_eq!(calibration_verdict(&whole), Ok(()));
    let other_device = Calibration {
        device: [0x22u8; 4],
        ..whole.clone()
    };
    let mut other_pairing = whole.clone();
    other_pairing.taken = Some(CtrlMessage::PairError {
        request_id: [0u8; 16],
        scope: ErrorScope::Pairing,
        object_id: [0x5A; 16],
        code: PAIR_UNAVAILABLE,
        retry_after_ms: 0,
    });
    let broken = [
        (
            "no decision line",
            Calibration {
                registry_err: vec![notice.clone()],
                ..whole.clone()
            },
        ),
        (
            "no notice",
            Calibration {
                registry_err: vec![decision.clone()],
                ..whole.clone()
            },
        ),
        (
            "another instance's log",
            Calibration {
                registry_err: vec!["v2 registry server listening on tls 127.0.0.1:9".to_string()],
                ..whole.clone()
            },
        ),
        ("a notice to another device", other_device),
        (
            "the client took nothing",
            Calibration {
                taken: None,
                ..whole.clone()
            },
        ),
        ("the client took another pairing's answer", other_pairing),
        (
            "a dropped notice",
            Calibration {
                registry_err: vec![
                    decision,
                    notice.replace("enqueued", "dropped: the outbound queue is full"),
                ],
                ..whole.clone()
            },
        ),
        (
            "a row exists for the control pairing",
            Calibration {
                transactions:
                    "{\"about\": [{\"alias\": \"calibration\", \"matches\": 1, \"row\": {}}]}"
                        .to_string(),
                ..whole.clone()
            },
        ),
    ];
    for (name, calibration) in broken {
        assert!(
            calibration_verdict(&calibration).is_err(),
            "{name}: {:?}",
            calibration_verdict(&calibration)
        );
    }
    // An identity that could not reach the registry names that, and not
    // the three parts it never got to look for.
    let unreachable = calibration_verdict(&Calibration {
        unreached: Some("pinned TLS dial failed".to_string()),
        ..whole.clone()
    });
    assert_eq!(
        unreachable,
        Err(
            "the calibration identity could not reach the registry: pinned TLS dial failed"
                .to_string()
        )
    );
}

/// …and on a real registry whose DEBUG lines are not written: the decision
/// and the notice are two of those lines.
#[tokio::test]
async fn the_calibration_on_a_real_registry_is_red_without_its_debug_lines() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server_dir = tmp.path().join("server");
    std::fs::create_dir_all(&server_dir).expect("server dir");
    let registry = RegistryProc::spawn_with_log(
        "quiet-registry",
        &server_dir,
        &tmp.path().join("registry.db"),
        "info",
    );
    let calibration = collect_calibration(&registry).await;
    let verdict = calibration_verdict(&calibration);
    assert!(
        verdict.is_err(),
        "without the registry's DEBUG lines the calibration must be red; the registry said:\n{}",
        redact(&calibration.registry_err.join("\n"))
    );
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
        // Before any natural redeem: from here on, every report of this
        // thread shows what this registry decided, without the
        // calibration's own lines.
        calibrate(&registry);
        mark_decision_cursor();
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
            assert!(
                run.success,
                "invite create failed:\n{}\n{}",
                redact(&run.stderr),
                live_report()
            );
            created.push(parse_created_code(&run.stdout));
        }

        let owner = spawn_proc(
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
        owner.wait_for_line(
            Stream::Stderr,
            "pairing service: answering redeems",
            Duration::from_secs(30),
            &[],
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
            let proc = spawn_proc(
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
    /// spawned on threads by the caller. `file_tag` names the source file:
    /// concurrent sends from the SAME redeemer must pass distinct tags —
    /// two threads rewriting one `matrix-src-N.bin` (truncate + rewrite)
    /// race the running sender's chunk reads into
    /// `Error: source changed during transfer` (observed on CI and loaded
    /// hosts; the send CLI legitimately refuses a mid-transfer mutation).
    fn send_from(&self, index: usize, bytes: usize) -> RunOutcome {
        self.send_from_file(index, index, bytes)
    }

    /// `send_from` with an explicit source-file tag (redeemer index and
    /// file tag are independent: `dual_policy` sends twice from redeemer 0).
    fn send_from_file(&self, index: usize, file_tag: usize, bytes: usize) -> RunOutcome {
        let src = self.tmp.path().join(format!("matrix-src-{file_tag}.bin"));
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
/// Max simultaneously-in-flight jobs and total started jobs, from the RAW
/// event line order (+1 per job_started, -1 per job_done). Pairing-free by
/// design: job_done.peer is optional (always filled since CR-P3-08, but a
/// peer string is never a concurrency key) and job_started's id is the
/// scheduler task id while job_done's id is the content id — different
/// namespaces, so peer/id-based pairing is inherently ambiguous under
/// parallel completion (CI runs 43029286253/43069873077/43077330860 each
/// tripped a different ambiguity). Serial-vs-parallel semantics live in
/// the ORDER alone.
fn job_concurrency(owner: &Proc) -> (usize, usize) {
    let mut in_flight = 0usize;
    let mut max_in_flight = 0usize;
    let mut started = 0usize;
    for line in owner.stdout.snapshot() {
        if line.contains("\"type\":\"job_started\"") {
            started += 1;
            in_flight += 1;
            max_in_flight = max_in_flight.max(in_flight);
        } else if line.contains("\"type\":\"job_done\"") {
            in_flight = in_flight.saturating_sub(1);
        }
    }
    (max_in_flight, started)
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
            "{label} send failed:\n{}\n{}\n{}",
            redact(&sent.stdout),
            redact(&sent.stderr),
            live_report()
        );
    }

    owner
        .stdout
        .wait_count("\"type\":\"job_done\"", 2, Duration::from_secs(30));
    let (max_in_flight, started) = job_concurrency(owner);
    assert_eq!(started, 2, "two completed tasks");
    assert!(
        max_in_flight <= 1,
        "same-code tasks must be serialized (max in-flight {max_in_flight})"
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
            "{label} send failed:\n{}\n{}\n{}",
            redact(&sent.stdout),
            redact(&sent.stderr),
            live_report()
        );
    }

    owner
        .stdout
        .wait_count("\"type\":\"job_done\"", 2, Duration::from_secs(30));
    let (max_in_flight, started) = job_concurrency(owner);
    assert_eq!(started, 2, "two completed tasks");
    assert!(
        max_in_flight >= 2,
        "cross-code tasks must run concurrently (max in-flight {max_in_flight})"
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
            // winning a connect race. Distinct file tag: same redeemer,
            // but rewriting the first send's source mid-transfer would
            // (correctly) trip the sender's source-mutation check.
            std::thread::sleep(Duration::from_millis(300));
            chain.send_from_file(0, 1, 2 * 1024 * 1024)
        })
    };
    for (label, outcome) in [("send-1", first), ("send-2", second)] {
        let sent = outcome.join().expect("send thread");
        assert!(
            sent.success,
            "{label} failed:\n{}\n{}\n{}",
            redact(&sent.stdout),
            redact(&sent.stderr),
            live_report()
        );
    }

    owner
        .stdout
        .wait_count("\"type\":\"job_done\"", 2, Duration::from_secs(30));
    let (max_in_flight, started) = job_concurrency(owner);
    assert_eq!(started, 2, "two completed tasks");
    assert!(
        max_in_flight <= 1,
        "the receiver's serial policy must win the intersection (max in-flight {max_in_flight})"
    );
}
