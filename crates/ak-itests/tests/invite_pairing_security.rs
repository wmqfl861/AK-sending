//! Invite-pairing integration tests, security layer (wave 22): the
//! negative paths of the pairing plane across its real boundaries.
//!
//! Covered, each against the real v2 registry (TLS + SQLite) and — where
//! the product path is the point — real `ak-cli` processes:
//!
//! * **Brute force**: 50 parse-valid but wrong codes from one device
//!   exhaust the frozen per-connection redeem budget (`RATE_LIMITED`
//!   0x2102 runs before the lookup, with a retry hint), and nothing is
//!   ever recorded locally for the failed attempts.
//! * **Enumeration**: the first response to a redeem — live code, ghost
//!   code, revoked code, self-owned code — has the same shape: an
//!   `InviteRedeemPending` echoing the caller's request id and deadline
//!   with a fresh random pairing id (contract §11: no existence oracle).
//! * **Revocation semantics**: a CLI-revoked code fails the next redeem
//!   with the unified `PAIR_UNAVAILABLE`; revoking a code AFTER a
//!   completed pairing never touches the already-committed trust — both
//!   `peers.book` files keep the peer and later sends still pass
//!   admission.
//! * **Fail-closed parsing**: malformed codes are rejected locally.
#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ak_identity::invite::InviteCode;
use ak_identity::pairing_txn::{random_id16, random_nonce32};
use ak_identity::DeviceKeyPair;
use ak_session::pairing::{
    redeem_request_mac, ClientError, OrchestratorConfig, PairFlowError, PairingOrchestrator,
    RegistryClient,
};
use ak_signal::protocol_v2::message::PAIR_UNAVAILABLE;
use ak_signal::protocol_v2::{RedeemRequest, RATE_LIMITED};
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::hello2;
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::Limits;
use tokio::net::TcpListener as TokioListener;
use tokio::sync::watch;

// ---- in-process registry ----------------------------------------------------

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

async fn spawn_registry() -> TestRegistry {
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

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A fully signed redeem request for `code` (the same construction the
/// orchestrator performs; the server checks the signature, never the MAC).
fn signed_redeem(
    key: &DeviceKeyPair,
    registry_id: &[u8; 32],
    code: &InviteCode,
    deadline: u64,
) -> RedeemRequest {
    let request_id = random_id16();
    let nonce = random_nonce32();
    let request_mac = redeem_request_mac(
        &code.auth_key(),
        registry_id,
        &code.lookup_digest(),
        &key.public_key(),
        &request_id,
        &nonce,
        deadline,
    );
    let signature = key.sign(&hello2::redeem_message(
        registry_id,
        &code.lookup_digest(),
        &key.public_key(),
        &request_id,
        &nonce,
        deadline,
        &request_mac,
    ));
    RedeemRequest {
        request_id,
        lookup_digest: code.lookup_digest(),
        redeemer_nonce: nonce,
        deadline_ms: deadline,
        request_mac,
        signature,
    }
}

/// A fast orchestrator (short redeem window) for negative redeems.
fn fast_orchestrator(
    client: RegistryClient,
    key: DeviceKeyPair,
    dir: &Path,
) -> PairingOrchestrator {
    PairingOrchestrator::open(
        client,
        key,
        dir,
        OrchestratorConfig {
            // Same slow-runner headroom as task_pairing (was 1.5s/5s).
            redeem_window_ms: 10_000,
            ready_wait_ms: 30_000,
            claim_conflict_retries: 8,
        },
    )
    .expect("open orchestrator")
}

/// Nothing may be recorded for a failed redeem: no trust book, and the
/// pairing journal (whose directory the orchestrator creates eagerly)
/// holds zero records.
fn assert_no_local_pairing_state(dir: &Path) {
    assert!(
        !dir.join("peers.book").exists(),
        "no peers.book may appear for failed redeems: {}",
        dir.display()
    );
    let journal = ak_store::PairingJournal::open(&dir.join("pairing")).expect("journal opens");
    let records = journal.load_all().expect("journal loads");
    assert!(
        records.is_empty(),
        "no pairing journal records may appear for failed redeems: {records:?}"
    );
}

// ---- CLI plumbing (registry-server / invite verbs as real processes) --------

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

struct RunOutcome {
    success: bool,
    stdout: String,
    stderr: String,
}

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
        if let Some(status) = child.try_wait().expect("try_wait") {
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
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

/// A resident CLI child (killed on drop).
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
        // Drain stderr too (into the void) so a chatty child can never
        // block on a full pipe.
        let _ = pipe_lines(child.stderr.take().expect("stderr piped"));
        Proc { child, stdout }
    }

    fn stdout_joined(&self) -> String {
        self.stdout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .join("\n")
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
                "no stdout line containing {needle:?} within {timeout:?}; got:\n{}",
                self.stdout_joined()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until at least `count` lines containing `needle` appeared.
    fn wait_stdout_count(&self, needle: &str, count: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let seen = self
                .stdout
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter(|line| line.contains(needle))
                .count();
            if seen >= count {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "only {seen}/{count} lines containing {needle:?} within {timeout:?}; got:\n{}",
                self.stdout_joined()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// The registry-server subprocess; its pin is parsed from stdout.
struct RegistryProc {
    /// Owning the process keeps the server alive (and killed on drop).
    #[allow(dead_code)]
    proc: Proc,
    addr: SocketAddr,
    fp_hex: String,
}

impl RegistryProc {
    fn spawn(server_dir: &Path, db: &Path) -> RegistryProc {
        let port = free_port();
        let proc = Proc::spawn(&[
            "registry-server",
            "--port",
            &port.to_string(),
            "--db-path",
            &db.to_string_lossy(),
            "--id-dir",
            &server_dir.to_string_lossy(),
        ]);
        let line = proc.wait_stdout(
            "registry id (pin with --registry-fp): ",
            Duration::from_secs(30),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id last token")
            .to_ascii_lowercase();
        assert_eq!(fp_hex.len(), 64, "registry id line: {line}");
        RegistryProc {
            proc,
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            fp_hex,
        }
    }

    /// The registry identity pin as raw bytes (for in-process clients).
    fn registry_id(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (slot, pair) in out.iter_mut().zip(self.fp_hex.as_bytes().chunks(2)) {
            *slot =
                u8::from_str_radix(std::str::from_utf8(pair).expect("hex"), 16).expect("hex digit");
        }
        out
    }
}

// ---- 1: brute force against the per-connection budget ------------------------

/// 50 parse-valid but never-registered codes from one device on one
/// control connection: the frozen per-connection budget (10/min) admits
/// the first 10 as `InviteRedeemPending` (identical shape, no oracle) and
/// answers every further attempt with `RATE_LIMITED` (0x2102) plus a
/// retry hint — and the failed brute force leaves zero local state.
#[tokio::test]
async fn fifty_wrong_codes_hit_the_per_connection_rate_limit() {
    let registry = spawn_registry().await;
    let b_dir = tempfile::tempdir().expect("tempdir");
    let key = DeviceKeyPair::generate();
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, "brute")
        .await
        .expect("connect");

    const ATTEMPTS: usize = 50;
    const BUDGET: usize = 10; // REDEEM_PER_CONN_PER_MIN (contract §11)
    let mut pending = 0usize;
    let mut rate_limited = 0usize;
    for attempt in 1..=ATTEMPTS {
        // Each attempt carries a fresh wrong code (160-bit entropy each),
        // a fresh request id and nonce — a genuine brute-force pattern.
        let ghost = InviteCode::generate();
        let request_id = random_id16();
        let deadline = now_unix_ms() + 3_000;
        let nonce = random_nonce32();
        let request_mac = redeem_request_mac(
            &ghost.auth_key(),
            &registry.registry_id,
            &ghost.lookup_digest(),
            &key.public_key(),
            &request_id,
            &nonce,
            deadline,
        );
        let signature = key.sign(&hello2::redeem_message(
            &registry.registry_id,
            &ghost.lookup_digest(),
            &key.public_key(),
            &request_id,
            &nonce,
            deadline,
            &request_mac,
        ));
        let ack = client
            .invite_redeem(RedeemRequest {
                request_id,
                lookup_digest: ghost.lookup_digest(),
                redeemer_nonce: nonce,
                deadline_ms: deadline,
                request_mac,
                signature,
            })
            .await;
        match ack {
            Ok(ack) => {
                pending += 1;
                assert!(pending <= BUDGET, "attempt {attempt} admitted over budget");
                // Same pending shape every time: our ids echoed, a fresh
                // random pairing id, our deadline echoed.
                assert_eq!(ack.request_id, request_id, "request id echoed");
                assert_eq!(ack.deadline_ms, deadline, "deadline echoed");
                assert_ne!(ack.pairing_id, [0u8; 16], "pairing id assigned");
            }
            Err(err) => {
                rate_limited += 1;
                assert!(
                    pending >= BUDGET,
                    "attempt {attempt} rejected before the budget drained: {err:?}"
                );
                match err {
                    ClientError::Pair {
                        code,
                        retry_after_ms,
                        ..
                    } => {
                        assert_eq!(code, RATE_LIMITED, "attempt {attempt}: {err}");
                        assert!(
                            retry_after_ms > 0,
                            "the limiter suggests a retry delay: {err}"
                        );
                    }
                    other => panic!("attempt {attempt}: expected RATE_LIMITED, got {other:?}"),
                }
            }
        }
    }
    assert_eq!(
        pending, BUDGET,
        "exactly the per-connection budget is admitted"
    );
    assert_eq!(
        rate_limited,
        ATTEMPTS - BUDGET,
        "every further attempt is rate limited"
    );
    assert_no_local_pairing_state(b_dir.path());
}

// ---- 2: no existence oracle in the first response ----------------------------

/// The first response to `InviteRedeem` cannot distinguish code states:
/// live (owner online), ghost, revoked, and self-owned codes all answer
/// `InviteRedeemPending` with the caller's request id and deadline echoed
/// and a fresh random pairing id — the unified failure only comes at the
/// caller-chosen deadline (contract §11).
#[tokio::test]
async fn redeem_first_response_indistinguishable_across_code_states() {
    let registry = spawn_registry().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let a_key = DeviceKeyPair::generate();
    let a_client = RegistryClient::connect(registry.addr, registry.registry_id, &a_key, "owner-a")
        .await
        .expect("owner connects");
    let a = PairingOrchestrator::with_defaults(a_client.clone(), a_key.clone(), tmp.path())
        .expect("owner orchestrator");

    let b_key = DeviceKeyPair::generate();
    let b_client =
        RegistryClient::connect(registry.addr, registry.registry_id, &b_key, "redeemer-b")
            .await
            .expect("redeemer connects");

    // Four code states, all redeemed by B with the same deadline.
    let live = a.create_invite(0).await.expect("live code");
    let revoked = a.create_invite(0).await.expect("code to revoke");
    a.revoke(revoked.code_id).await.expect("revoke");
    let self_owned = b_code_via_second_identity(&registry, &tmp).await;

    let deadline = now_unix_ms() + 3_000;
    let mut acks = Vec::new();
    for code in [&live, &revoked, &self_owned] {
        let parsed = InviteCode::parse(&code.display).expect("stored code parses");
        let request = signed_redeem(&b_key, &registry.registry_id, &parsed, deadline);
        let ack = b_client.invite_redeem(request).await;
        let ack = ack.expect("the first response is always Pending, never an error");
        acks.push((code.display.clone(), ack));
    }
    // A ghost code (never registered) joins the comparison.
    let ghost = InviteCode::generate();
    let request = signed_redeem(&b_key, &registry.registry_id, &ghost, deadline);
    let ghost_ack = b_client
        .invite_redeem(request)
        .await
        .expect("ghost code also answers Pending");

    // Identical shape for every state: our deadline echoed, a pairing id
    // assigned, and no error before the deadline.
    let mut pairing_ids = Vec::new();
    for (label, ack) in &acks {
        assert_eq!(ack.deadline_ms, deadline, "{label}: deadline echoed");
        assert_ne!(ack.pairing_id, [0u8; 16], "{label}: pairing id assigned");
        pairing_ids.push(ack.pairing_id);
    }
    assert_eq!(ghost_ack.deadline_ms, deadline, "ghost: deadline echoed");
    assert_ne!(
        ghost_ack.pairing_id, [0u8; 16],
        "ghost: pairing id assigned"
    );
    pairing_ids.push(ghost_ack.pairing_id);
    // The pairing ids are fresh randoms — no bit of them correlates with
    // the code's existence (all four distinct, none zero).
    let unique: std::collections::HashSet<[u8; 16]> = pairing_ids.into_iter().collect();
    assert_eq!(unique.len(), 4, "four distinct random pairing ids");

    // None of the four attempts wrote trust: the redeemer side of this
    // test is a raw client with no book at all.
    assert!(
        !tmp.path().join("b").join("peers.book").exists(),
        "no peers.book from Pending-only redeem attempts"
    );
}

/// B also owns one code itself (self-redeem must look like a ghost, not
/// like an error).
async fn b_code_via_second_identity(
    registry: &TestRegistry,
    tmp: &tempfile::TempDir,
) -> ak_session::pairing::CreatedInvite {
    let dir = tmp.path().join("b");
    std::fs::create_dir_all(&dir).expect("b dir");
    let key = DeviceKeyPair::generate();
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, "redeemer-b")
        .await
        .expect("b connects");
    let orch = PairingOrchestrator::with_defaults(client, key, &dir).expect("b orchestrator");
    orch.create_invite(0).await.expect("b claims a code")
}

// ---- 3: revocation blocks the next redeem ------------------------------------

/// A code revoked through the real CLI (`invite revoke` subprocess on the
/// shared identity directory) rejects the next redeem with the unified
/// `PAIR_UNAVAILABLE`, leaves the redeemer clean, and the owner sees the
/// revoked state through `invite list`.
#[tokio::test]
async fn cli_revoked_code_rejects_next_redeem_with_unified_unavailable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a_dir = tmp.path().join("owner-a");
    let b_dir = tmp.path().join("redeemer-b");
    std::fs::create_dir_all(&a_dir).expect("a dir");
    std::fs::create_dir_all(&b_dir).expect("b dir");
    let registry = RegistryProc::spawn(&tmp.path().join("server"), &tmp.path().join("reg.db"));
    let addr = registry.addr.to_string();
    let fp = registry.fp_hex.clone();

    // The owner identity lives on disk so the CLI subprocess and the
    // in-process orchestrator share one device key.
    let (a_key, _) = DeviceKeyPair::load_or_create(&a_dir.join("device.key")).expect("a key");
    let a_client =
        RegistryClient::connect(registry.addr, registry.registry_id(), &a_key, "owner-a")
            .await
            .expect("a connects");
    let a = PairingOrchestrator::with_defaults(a_client, a_key, &a_dir).expect("a orchestrator");
    let invite = a.create_invite(0).await.expect("claim");
    let code_id_hex: String = invite.code_id.iter().map(|b| format!("{b:02x}")).collect();

    // Revoke through the real CLI (this also persists the registry pin).
    let revoke = run_cli(
        &[
            "invite",
            "revoke",
            &code_id_hex,
            "--id-dir",
            &a_dir.to_string_lossy(),
            "--server",
            &addr,
            "--registry-fp",
            &fp,
        ],
        Duration::from_secs(30),
    );
    assert!(revoke.success, "cli revoke failed:\n{}", revoke.stderr);

    // The owner's own list shows the revoked state (tombstone kept).
    let list = run_cli(
        &["invite", "list", "--id-dir", &a_dir.to_string_lossy()],
        Duration::from_secs(30),
    );
    assert!(list.success, "cli list failed:\n{}", list.stderr);
    let line = list
        .stdout
        .lines()
        .find(|line| line.starts_with(&code_id_hex))
        .expect("revoked code listed");
    assert!(line.contains("revoked"), "state flipped: {line}");

    // The next redeem fails with the unified answer (fast window keeps
    // the deadline wait short).
    let b_key = DeviceKeyPair::generate();
    let b_client =
        RegistryClient::connect(registry.addr, registry.registry_id(), &b_key, "redeemer-b")
            .await
            .expect("b connects");
    let b = fast_orchestrator(b_client, b_key, &b_dir);
    let err = b
        .redeem(&invite.display)
        .await
        .expect_err("revoked code must fail");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected the unified PAIR_UNAVAILABLE, got {other:?}"),
    }
    assert_no_local_pairing_state(&b_dir);
}

// ---- 4: revoking after a completed pairing keeps the trust -------------------

/// Contract §8 ordering: once the pairing transaction committed, a later
/// revoke of the code does not remove the established trust. Proven on
/// the full product chain: paired daemons exchange one file byte-identical,
/// the code is revoked through the CLI, both `peers.book` files still
/// cross-record the peer, a second send still passes admission — and a
/// third device redeeming the revoked code fails unified.
#[tokio::test]
async fn revoke_after_pairing_preserves_books_and_admits_further_sends() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server_dir = tmp.path().join("server");
    let a_dir = tmp.path().join("owner-a");
    let b_dir = tmp.path().join("redeemer-b");
    let a_out = tmp.path().join("out-a");
    let b_out = tmp.path().join("out-b");
    let c_dir = tmp.path().join("stranger-c");
    for dir in [&server_dir, &a_dir, &b_dir, &a_out, &b_out, &c_dir] {
        std::fs::create_dir_all(dir).expect("create dir");
    }
    let registry = RegistryProc::spawn(&server_dir, &tmp.path().join("registry.db"));
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
    let code = created
        .stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("AKI1-"))
        .map(|rest| format!("AKI1-{rest}"))
        .expect("code printed");
    let code_id = created
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("code id "))
        .and_then(|rest| rest.split(' ').next().map(str::to_owned))
        .expect("code id printed");

    let a_daemon = Proc::spawn(&[
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
        "keep-owner",
        "--server",
        &addr,
        "--registry-fp",
        &fp,
    ]);
    let b_daemon = Proc::spawn(&[
        "daemon",
        "--id-dir",
        &b_dir.to_string_lossy(),
        "--out",
        &b_out.to_string_lossy(),
        "--json-lines",
        "--invite-code",
        &code,
        "--name",
        "keep-redeemer",
        "--server",
        &addr,
        "--registry-fp",
        &fp,
    ]);
    let b_listening = b_daemon.wait_stdout("\"type\":\"listening\"", Duration::from_secs(60));
    let b_port =
        u16::try_from(json_number(&b_listening, "port").expect("b port")).expect("port fits u16");
    let b_fp = json_string(&b_listening, "fingerprint").expect("b fingerprint");
    let a_listening = a_daemon.wait_stdout("\"type\":\"listening\"", Duration::from_secs(30));
    let a_fp = json_string(&a_listening, "fingerprint").expect("a fingerprint");

    // One real transfer through the paired chain.
    let payload: Vec<u8> = (0..96 * 1024u32).map(|i| (i % 251) as u8).collect();
    let src1 = tmp.path().join("before-revoke.bin");
    std::fs::write(&src1, &payload).expect("write file");
    let sent1 = run_cli(
        &[
            "send",
            &src1.to_string_lossy(),
            "--to",
            &format!("127.0.0.1:{b_port}"),
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
        sent1.success,
        "send before revoke failed:\n{}",
        sent1.stderr
    );
    b_daemon.wait_stdout_count("\"type\":\"job_done\"", 1, Duration::from_secs(30));
    assert_eq!(
        std::fs::read(b_out.join("before-revoke.bin")).expect("file one"),
        payload
    );

    // Revoke the code through the CLI while both daemons keep running.
    let revoke = run_cli(
        &[
            "invite",
            "revoke",
            &code_id,
            "--id-dir",
            &a_dir.to_string_lossy(),
        ],
        Duration::from_secs(30),
    );
    assert!(revoke.success, "revoke failed:\n{}", revoke.stderr);

    // Established trust survives: both books still cross-record, byte level.
    let a_book = std::fs::read_to_string(a_dir.join("peers.book")).expect("a book");
    let b_book = std::fs::read_to_string(b_dir.join("peers.book")).expect("b book");
    assert!(a_book.contains(&b_fp), "owner keeps the redeemer: {a_book}");
    assert!(b_book.contains(&a_fp), "redeemer keeps the owner: {b_book}");

    // A second send still passes admission (book policy, no code needed).
    let src2 = tmp.path().join("after-revoke.bin");
    let payload2: Vec<u8> = (0..48 * 1024u32).map(|i| (i % 249) as u8).collect();
    std::fs::write(&src2, &payload2).expect("write file");
    let sent2 = run_cli(
        &[
            "send",
            &src2.to_string_lossy(),
            "--to",
            &format!("127.0.0.1:{b_port}"),
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
        sent2.success,
        "send after revoke must still pass admission:\n{}\n{}",
        sent2.stdout, sent2.stderr
    );
    b_daemon.wait_stdout_count("\"type\":\"job_done\"", 2, Duration::from_secs(30));
    assert_eq!(
        std::fs::read(b_out.join("after-revoke.bin")).expect("file two"),
        payload2
    );

    // But the revoked code no longer pairs a third device: fast in-process
    // redeem on a fresh identity fails unified at its deadline.
    let c_key = DeviceKeyPair::generate();
    let c_client =
        RegistryClient::connect(registry.addr, registry.registry_id(), &c_key, "stranger-c")
            .await
            .expect("c connects");
    let c = fast_orchestrator(c_client, c_key, &c_dir);
    let err = c.redeem(&code).await.expect_err("revoked code must fail");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected PAIR_UNAVAILABLE, got {other:?}"),
    }
    assert_no_local_pairing_state(&c_dir);
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

// ---- 5: malformed codes fail closed ------------------------------------------

/// Malformed codes (wrong prefix, undefined symbol, wrong length) are
/// rejected by the CLI with a non-zero exit and the fail-closed parse
/// error — and no local pairing state appears.
#[test]
fn malformed_invite_codes_fail_closed_at_the_cli() {
    // A live registry is stood up only so the command cannot fail for
    // connectivity reasons before reaching the parser.
    let tmp = tempfile::tempdir().expect("tempdir");
    let server_dir = tmp.path().join("server");
    let b_dir = tmp.path().join("redeemer-b");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&server_dir).expect("server dir");
    std::fs::create_dir_all(&b_dir).expect("b dir");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let port = free_port();
    let server = Proc::spawn(&[
        "registry-server",
        "--port",
        &port.to_string(),
        "--db-path",
        &tmp.path().join("reg.db").to_string_lossy(),
        "--id-dir",
        &server_dir.to_string_lossy(),
    ]);
    let pin_line = server.wait_stdout(
        "registry id (pin with --registry-fp): ",
        Duration::from_secs(30),
    );
    let fp = pin_line.rsplit(' ').next().expect("hex").to_owned();
    let addr = format!("127.0.0.1:{port}");

    let malformed = [
        "NOTACODE-AAAA-BBBB",                           // wrong prefix
        "AKI1-UUUU-UUUU-UUUU-UUUU-UUUU-UUUU-UUUU-UUUU", // U is not a Crockford symbol
        "AKI1-AAAA",                                    // wrong length
    ];
    for code in malformed {
        let run = run_cli(
            &[
                "invite",
                "redeem",
                code,
                "--once",
                "--out",
                &out_dir.to_string_lossy(),
                "--id-dir",
                &b_dir.to_string_lossy(),
                "--server",
                &addr,
                "--registry-fp",
                &fp,
            ],
            Duration::from_secs(30),
        );
        assert!(!run.success, "malformed code {code} must fail");
        assert!(
            run.stderr.contains("invite code rejected"),
            "the parse error is reported fail-closed for {code}: {}",
            run.stderr
        );
    }
    assert_no_local_pairing_state(&b_dir);
    // The receiver never even bound: no file appeared.
    assert!(
        std::fs::read_dir(&out_dir).expect("out dir").count() == 0,
        "no files may appear for rejected codes"
    );
}
