//! End-to-end tests of the B8 FFI surface (`ak_pairing_*` / `ak_sched_*`,
//! wave 22). Every test drives the library ONLY through the exported C ABI
//! functions against a REAL v2 registry (pinned TLS 1.3 + in-memory
//! SQLite, `ak-signal` `server-persistence`) — the same server class the
//! ak-session pairing e2e suite boots — and, for the scheduler tests,
//! REAL `ak-engine` loopback transfers of real bytes.
//!
//! Sequencing note that the tests make explicit: `ak_sched_start` must
//! precede the redeem whose transcript the scheduler's transport later
//! opens a session for (`RealTransport` caches the `PairCommit` facts from
//! the live pairing plane), so the harness starts the scheduler runtime
//! before driving the redeem op.
//!
//! Fixture-only direct library use (temp files, a random invite code to
//! cancel, the registry spawn) stays outside the ABI, mirroring the
//! conventions of `tests/ffi.rs`.

use std::ffi::CString;
use std::fs;
use std::net::UdpSocket;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ak_ffi::{
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_identity_info, ak_job_result,
    ak_pairing_cancel_op, ak_pairing_create_start, ak_pairing_list_start, ak_pairing_op_result,
    ak_pairing_poll_event, ak_pairing_redeem_start, ak_pairing_revoke_start, ak_pairing_start,
    ak_pairing_stop_start, ak_poll_progress, ak_receive_start, ak_sched_cancel, ak_sched_enqueue,
    ak_sched_listen_start, ak_sched_poll_event, ak_sched_retry, ak_sched_set_policy,
    ak_sched_shutdown_start, ak_sched_snapshot, ak_sched_start, ak_sched_task_result,
    ak_send_start, ak_version, AkEngine, AK_DONE, AK_ERR_BUFFER, AK_ERR_INVALID, AK_ERR_NOT_FOUND,
    AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK, AK_PENDING,
};

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn cbuf_to_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8(bytes).expect("FFI output is UTF-8")
}

fn jstr(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn path_jstr(path: &Path) -> String {
    jstr(&path.to_string_lossy())
}

fn field_str(text: &str, key: &str) -> String {
    let pat = format!("\"{key}\":\"");
    let start = text
        .find(&pat)
        .unwrap_or_else(|| panic!("key {key} in {text}"))
        + pat.len();
    let end = start + text[start..].find('"').expect("string value end");
    text[start..end].to_string()
}

fn field_u64(text: &str, key: &str) -> u64 {
    let pat = format!("\"{key}\":");
    let start = text
        .find(&pat)
        .unwrap_or_else(|| panic!("key {key} in {text}"))
        + pat.len();
    let end = start
        + text[start..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len() - start);
    text[start..end]
        .parse()
        .unwrap_or_else(|_| panic!("digits of {key} in {text}"))
}

fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind(("127.0.0.1", 0))
        .expect("reserve udp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

/// The real v2 registry on a dedicated thread (current-thread tokio
/// runtime; the FFI's own `block_on` calls happen on the TEST thread, so
/// the two never nest).
struct TestRegistry {
    addr: std::net::SocketAddr,
    registry_id: [u8; 32],
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestRegistry {
    fn spawn() -> TestRegistry {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("registry runtime");
            rt.block_on(async move {
                let key = ak_identity::DeviceKeyPair::generate();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind registry port");
                let addr = listener.local_addr().expect("registry addr");
                let store = ak_signal::server_v2::store::RegistryStore::open_in_memory()
                    .expect("in-memory registry store");
                let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                ready_tx
                    .send((addr, key.public_key()))
                    .expect("test is waiting");
                let server = ak_signal::server_v2::engine::serve_v2(
                    listener,
                    &key,
                    store,
                    ak_signal::server_v2::Limits::default(),
                    stop_rx,
                );
                tokio::select! {
                    _ = server => {}
                    _ = shutdown_rx => {
                        let _ = stop_tx.send(true);
                        // Give the server a beat to observe the stop flag.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            });
        });
        let (addr, registry_id) = ready_rx.recv().expect("registry boots");
        TestRegistry {
            addr,
            registry_id,
            shutdown: Some(shutdown_tx),
        }
    }

    fn id_hex(&self) -> String {
        self.registry_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// One pairing runtime via the ABI, waiting for `runtime_ready`.
fn start_runtime(engine: *mut AkEngine, registry: &TestRegistry, id_dir: &Path) -> [u8; 16] {
    let spec = CString::new(format!(
        "{{\"registry\":\"{}\",\"registry_id\":{},\"id_dir\":{},\"device_name\":\"ffi-device\"}}",
        registry.addr,
        jstr(&registry.id_hex()),
        path_jstr(id_dir),
    ))
    .unwrap();
    let mut runtime_id = [0u8; 16];
    let code = unsafe { ak_pairing_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()) };
    assert_eq!(code, AK_OK, "ak_pairing_start");
    wait_event(engine, &runtime_id, "runtime_ready", 15);
    runtime_id
}

/// Poll one runtime's events until an event with `name` shows up.
fn wait_event(engine: *mut AkEngine, runtime_id: &[u8; 16], name: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut buf = [0 as c_char; 4096];
    loop {
        let code = unsafe {
            ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len())
        };
        assert!(code == AK_EVENT || code == AK_PENDING, "poll code {code}");
        if code == AK_EVENT {
            let line = cbuf_to_string(&buf);
            if line.contains(&format!("\"event\":\"{name}\"")) {
                return line;
            }
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "event {name} never arrived on runtime {}",
            hex16(runtime_id)
        );
        thread::sleep(Duration::from_millis(20));
    }
}

enum Op<'a> {
    Create(&'a str),
    Redeem(&'a str),
    List(&'a str),
    Revoke(&'a str),
}

/// Start one op through the ABI and poll its result to a terminal state.
fn run_op(engine: *mut AkEngine, runtime_id: &[u8; 16], op: Op) -> (i32, String) {
    type Launch = unsafe extern "C" fn(*mut AkEngine, *const u8, *const c_char, *mut u8) -> i32;
    let (spec, launch): (CString, Launch) = match op {
        Op::Create(spec) => (CString::new(spec).unwrap(), ak_pairing_create_start),
        Op::Redeem(spec) => (CString::new(spec).unwrap(), ak_pairing_redeem_start),
        Op::List(spec) => (CString::new(spec).unwrap(), ak_pairing_list_start),
        Op::Revoke(spec) => (CString::new(spec).unwrap(), ak_pairing_revoke_start),
    };
    let mut op_id = [0u8; 16];
    let code = unsafe {
        launch(
            engine,
            runtime_id.as_ptr(),
            spec.as_ptr(),
            op_id.as_mut_ptr(),
        )
    };
    assert_eq!(code, AK_OK, "op start");
    let mut buf = [0 as c_char; 8192];
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let code = unsafe {
            ak_pairing_op_result(
                engine,
                runtime_id.as_ptr(),
                op_id.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(
            code == AK_PENDING || code == AK_DONE || code == AK_FAILED,
            "op_result {code}"
        );
        if code == AK_DONE || code == AK_FAILED {
            return (code, cbuf_to_string(&buf));
        }
        assert!(Instant::now() < deadline, "op never finished");
        thread::sleep(Duration::from_millis(15));
    }
}

/// One paired fixture: registry + two runtimes (owner `b`, redeemer `a`)
/// + one created invite on `b`.
struct Pair {
    _registry: TestRegistry,
    engine: *mut AkEngine,
    a: [u8; 16],
    b: [u8; 16],
    a_dir: PathBuf,
    b_dir: PathBuf,
    invite_code: String,
    code_id: String,
}

fn paired_fixture() -> Pair {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let a_dir = tempfile::tempdir().expect("tempdir a").keep();
    let b_dir = tempfile::tempdir().expect("tempdir b").keep();
    let a = start_runtime(engine, &registry, &a_dir);
    let b = start_runtime(engine, &registry, &b_dir);
    Pair {
        _registry: registry,
        engine,
        a,
        b,
        a_dir,
        b_dir,
        invite_code: String::new(),
        code_id: String::new(),
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        // Best effort: forget the engine only after freeing it once.
        unsafe { ak_engine_free(self.engine) };
    }
}

/// `pair.a`/`pair.b` are raw pointers re-read after moves; keep them in a
/// small trait object to satisfy borrow rules in helpers below.
fn create_invite_on(pair: &mut Pair) {
    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::Create("{\"spec_version\":1,\"ttl_secs\":0}"),
    );
    assert_eq!(code, AK_DONE, "create op: {json}");
    assert!(json.starts_with("{\"op\":\"create\""), "{json}");
    pair.invite_code = field_str(&json, "code");
    pair.code_id = field_str(&json, "code_id");
    assert!(pair.invite_code.starts_with("AKI1"), "{json}");
}

// ---------------------------------------------------------------------------
// ak_pairing_start / events / argument discipline.
// ---------------------------------------------------------------------------

#[test]
fn pairing_start_ready_event_and_unknown_runtime_poll() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let runtime_id = start_runtime(engine, &registry, &dir);

    // The ready event was consumed by the helper; a fresh poll is empty.
    let mut buf = [0 as c_char; 256];
    let code =
        unsafe { ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    assert_eq!(code, AK_PENDING);

    // Unknown runtime: every runtime-keyed call answers -4.
    let bogus = [9u8; 16];
    assert_eq!(
        unsafe { ak_pairing_poll_event(engine, bogus.as_ptr(), buf.as_mut_ptr(), buf.len()) },
        AK_ERR_NOT_FOUND
    );
    let mut op_buf = [0 as c_char; 64];
    assert_eq!(
        unsafe {
            ak_pairing_op_result(
                engine,
                bogus.as_ptr(),
                bogus.as_ptr(),
                op_buf.as_mut_ptr(),
                op_buf.len(),
            )
        },
        AK_ERR_NOT_FOUND
    );
    assert_eq!(
        unsafe { ak_pairing_cancel_op(engine, bogus.as_ptr(), bogus.as_ptr()) },
        AK_ERR_NOT_FOUND
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn pairing_start_validates_arguments_before_side_effects() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let mut runtime_id = [0u8; 16];

    // Null spec / null output pointer: -1, nothing registered.
    let spec = CString::new("{}").unwrap();
    assert_eq!(
        unsafe { ak_pairing_start(engine, std::ptr::null(), runtime_id.as_mut_ptr()) },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe { ak_pairing_start(engine, spec.as_ptr(), std::ptr::null_mut()) },
        AK_ERR_NULL
    );

    // Missing registry / bad registry_id: -2 (validation before IO).
    let missing = CString::new(format!("{{\"id_dir\":{}}}", path_jstr(&dir))).unwrap();
    assert_eq!(
        unsafe { ak_pairing_start(engine, missing.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_ERR_INVALID
    );
    let bad_pin = CString::new(format!(
        "{{\"registry\":\"127.0.0.1:1\",\"registry_id\":\"abc\",\"id_dir\":{}}}",
        path_jstr(&dir)
    ))
    .unwrap();
    assert_eq!(
        unsafe { ak_pairing_start(engine, bad_pin.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_ERR_INVALID
    );

    // Unreachable registry: accepted (background connect), then the
    // failure arrives as an event, and later ops fail against it.
    let dead = CString::new(format!(
        "{{\"registry\":\"127.0.0.1:1\",\"registry_id\":{},\"id_dir\":{},\"redeem_window_ms\":1000}}",
        jstr(&[0x11u8; 32].iter().map(|b| format!("{b:02x}")).collect::<String>()),
        path_jstr(&dir),
    ))
    .unwrap();
    assert_eq!(
        unsafe { ak_pairing_start(engine, dead.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_OK
    );
    let failed = wait_event(engine, &runtime_id, "runtime_failed", 20);
    assert!(failed.contains("runtime_failed"), "{failed}");

    // A sched bind against the dead runtime is refused before it starts.
    let sched = CString::new("{}").unwrap();
    assert_eq!(
        unsafe { ak_sched_start(engine, sched.as_ptr()) },
        AK_ERR_INVALID,
        "sched_start needs a connected runtime"
    );

    unsafe { ak_engine_free(engine) };
}

// ---------------------------------------------------------------------------
// Ops against the real registry.
// ---------------------------------------------------------------------------

#[test]
fn create_invite_op_roundtrip_with_real_registry() {
    let mut pair = paired_fixture();
    create_invite_on(&mut pair);

    // Same create replayed with the SAME request id answers the same code
    // (server idempotency; ffi-v2 §2 "新操作使用 request_id 幂等").
    let request_id: String = (0u8..16).map(|i| format!("{:02x}", i + 1)).collect();
    let first = run_op(
        pair.engine,
        &pair.b,
        Op::Create(&format!(
            "{{\"spec_version\":1,\"request_id\":\"{request_id}\",\"ttl_secs\":0}}"
        )),
    );
    let second = run_op(
        pair.engine,
        &pair.b,
        Op::Create(&format!(
            "{{\"spec_version\":1,\"request_id\":\"{request_id}\",\"ttl_secs\":0}}"
        )),
    );
    assert_eq!(first.0, AK_DONE);
    assert_eq!(second.0, AK_DONE);
    assert_eq!(
        field_str(&first.1, "code"),
        field_str(&second.1, "code"),
        "the same request id replays the same code: {} vs {}",
        first.1,
        second.1
    );
}

#[test]
fn redeem_full_flow_writes_both_peers_books() {
    let mut pair = paired_fixture();
    create_invite_on(&mut pair);

    let (code, json) = run_op(
        pair.engine,
        &pair.a,
        Op::Redeem(&format!(
            "{{\"spec_version\":1,\"invite_code\":{}}}",
            jstr(&pair.invite_code)
        )),
    );
    assert_eq!(code, AK_DONE, "redeem op: {json}");
    assert!(json.contains("\"op\":\"redeem\""), "{json}");
    let pairing_id = field_str(&json, "pairing_id");
    assert_eq!(pairing_id.len(), 32, "{json}");

    // The redeem op drove the whole transcript: both devices durably
    // paired (the redeemer applied at commit time; the owner loop applied
    // on its own side). Verify the books on disk directly.
    let peers_a = std::fs::read_to_string(pair.a_dir.join("peers.book")).expect("a book");
    let peers_b = std::fs::read_to_string(pair.b_dir.join("peers.book")).expect("b book");
    let (key_b, _) =
        ak_identity::DeviceKeyPair::load_or_create(&pair.b_dir.join("device.key")).unwrap();
    let (key_a, _) =
        ak_identity::DeviceKeyPair::load_or_create(&pair.a_dir.join("device.key")).unwrap();
    assert!(
        peers_a.contains(&key_b.fingerprint().to_hex()),
        "redeemer book must pin the owner: {peers_a}"
    );
    assert!(
        peers_b.contains(&key_a.fingerprint().to_hex()),
        "owner book must pin the redeemer: {peers_b}"
    );
}

#[test]
fn op_result_buffer_retry_is_lossless() {
    let mut pair = paired_fixture();
    create_invite_on(&mut pair);

    // The stored create result survives a -3 unharmed, repeatedly.
    let spec = CString::new("{\"spec_version\":1,\"ttl_secs\":0}").unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_pairing_create_start(
                pair.engine,
                pair.b.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_OK
    );
    let mut big = [0 as c_char; 8192];
    let (code, json) = loop {
        let code = unsafe {
            ak_pairing_op_result(
                pair.engine,
                pair.b.as_ptr(),
                op_id.as_ptr(),
                big.as_mut_ptr(),
                big.len(),
            )
        };
        if code == AK_DONE || code == AK_FAILED {
            break (code, cbuf_to_string(&big));
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(code, AK_DONE);
    let code_value = field_str(&json, "code");

    let mut tiny = [0 as c_char; 4];
    for _ in 0..3 {
        assert_eq!(
            unsafe {
                ak_pairing_op_result(
                    pair.engine,
                    pair.b.as_ptr(),
                    op_id.as_ptr(),
                    tiny.as_mut_ptr(),
                    tiny.len(),
                )
            },
            AK_ERR_BUFFER,
            "a too-small buffer must be -3"
        );
    }
    // Lossless retry: same op, same bytes, and a re-read stays identical —
    // no second invite code was generated.
    let mut again = [0 as c_char; 8192];
    assert_eq!(
        unsafe {
            ak_pairing_op_result(
                pair.engine,
                pair.b.as_ptr(),
                op_id.as_ptr(),
                again.as_mut_ptr(),
                again.len(),
            )
        },
        AK_DONE
    );
    let again_json = cbuf_to_string(&again);
    assert_eq!(again_json, json, "the stored op result is stable");
    assert_eq!(field_str(&again_json, "code"), code_value);
}

#[test]
fn poll_event_buffer_pushback_keeps_the_event() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let runtime_id = [7u8; 16]; // replaced by the real id below
    let _ = runtime_id;
    let spec = CString::new(format!(
        "{{\"registry\":\"{}\",\"registry_id\":{},\"id_dir\":{}}}",
        registry.addr,
        jstr(&registry.id_hex()),
        path_jstr(&dir),
    ))
    .unwrap();
    let mut runtime_id = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_OK
    );

    // Wait for the ready event to exist, then prove -3 does not consume it.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut tiny = [0 as c_char; 2];
    loop {
        assert!(Instant::now() < deadline, "ready event never arrived");
        let code = unsafe {
            ak_pairing_poll_event(engine, runtime_id.as_ptr(), tiny.as_mut_ptr(), tiny.len())
        };
        if code == AK_PENDING {
            // Not queued yet (still connecting).
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        assert_eq!(code, AK_ERR_BUFFER, "a queued event must not fit 2 bytes");
        let mut big = [0 as c_char; 4096];
        let code = unsafe {
            ak_pairing_poll_event(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len())
        };
        if code == AK_EVENT {
            let line = cbuf_to_string(&big);
            assert!(line.contains("runtime_ready"), "{line}");
            break;
        }
        assert_eq!(code, AK_PENDING);
        thread::sleep(Duration::from_millis(20));
    }
    // The event was consumed exactly once by the successful poll.
    let mut big = [0 as c_char; 4096];
    assert_eq!(
        unsafe { ak_pairing_poll_event(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()) },
        AK_PENDING
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn cancel_op_stamps_cancelled_and_unknown_op_is_not_found() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let spec = CString::new(format!(
        "{{\"registry\":\"{}\",\"registry_id\":{},\"id_dir\":{},\"redeem_window_ms\":1000}}",
        registry.addr,
        jstr(&registry.id_hex()),
        path_jstr(&dir),
    ))
    .unwrap();
    let mut runtime_id = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_OK
    );
    wait_event(engine, &runtime_id, "runtime_ready", 15);

    // A well-formed but never-registered code: the server answers
    // InviteRedeemPending and the flow waits out its window — long enough
    // to cancel in flight.
    let unknown = ak_identity::invite::InviteCode::generate().to_display();
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"invite_code\":{}}}",
        jstr(&unknown)
    ))
    .unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_pairing_redeem_start(
                engine,
                runtime_id.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_OK
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        unsafe { ak_pairing_cancel_op(engine, runtime_id.as_ptr(), op_id.as_ptr()) },
        AK_OK
    );
    let mut buf = [0 as c_char; 2048];
    let deadline = Instant::now() + Duration::from_secs(5);
    let json = loop {
        let code = unsafe {
            ak_pairing_op_result(
                engine,
                runtime_id.as_ptr(),
                op_id.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(Instant::now() < deadline, "cancelled op never resolved");
        if code == AK_FAILED {
            break cbuf_to_string(&buf);
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(json.contains("cancelled by caller"), "{json}");

    let bogus = [9u8; 16];
    assert_eq!(
        unsafe { ak_pairing_cancel_op(engine, runtime_id.as_ptr(), bogus.as_ptr()) },
        AK_ERR_NOT_FOUND
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn list_and_revoke_roundtrip() {
    let mut pair = paired_fixture();
    create_invite_on(&mut pair);

    // Active list shows the code.
    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::List("{\"spec_version\":1,\"limit\":32,\"state_filter\":\"all\"}"),
    );
    assert_eq!(code, AK_DONE, "{json}");
    assert!(json.contains(&pair.code_id), "{json}");
    assert!(json.contains("\"state\":\"active\""), "{json}");

    // Revoke it; the revoked filter shows the tombstone.
    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::Revoke(&format!(
            "{{\"spec_version\":1,\"code_id\":{}}}",
            jstr(&pair.code_id)
        )),
    );
    assert_eq!(code, AK_DONE, "revoke op: {json}");
    assert!(json.contains("\"op\":\"revoke\""), "{json}");

    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::List("{\"spec_version\":1,\"state_filter\":\"revoked\"}"),
    );
    assert_eq!(code, AK_DONE, "{json}");
    assert!(json.contains(&pair.code_id), "{json}");
    assert!(json.contains("\"state\":\"revoked\""), "{json}");

    // Revoking an unknown code fails as an op (local secret check).
    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::Revoke("{\"spec_version\":1,\"code_id\":\"00000000000000000000000000000000\"}"),
    );
    assert_eq!(code, AK_FAILED, "{json}");
    assert!(json.contains("unknown invite code"), "{json}");
}

// ---------------------------------------------------------------------------
// Scheduler.
// ---------------------------------------------------------------------------

fn snapshot_text(engine: *mut AkEngine) -> (i32, String) {
    let mut buf = [0 as c_char; 16384];
    let code = unsafe { ak_sched_snapshot(engine, buf.as_mut_ptr(), buf.len()) };
    (code, cbuf_to_string(&buf))
}

/// Poll the snapshot until `pred` holds; returns the last snapshot text.
fn snapshot_until(engine: *mut AkEngine, mut pred: impl FnMut(&str) -> bool, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let (code, json) = snapshot_text(engine);
        assert_eq!(code, AK_DONE, "snapshot: {json}");
        if pred(&json) {
            return json;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot condition unmet: {json}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn count_state(snapshot: &str, state: &str) -> usize {
    snapshot.matches(&format!("\"state\":\"{state}\"")).count()
}

/// Drain every scheduler event line (bounded).
fn drain_sched_events(engine: *mut AkEngine) -> Vec<String> {
    let mut lines = Vec::new();
    let mut buf = [0 as c_char; 4096];
    loop {
        let code = unsafe { ak_sched_poll_event(engine, buf.as_mut_ptr(), buf.len()) };
        assert!(code == AK_EVENT || code == AK_PENDING, "sched poll {code}");
        if code == AK_PENDING {
            return lines;
        }
        lines.push(cbuf_to_string(&buf));
    }
}

struct TransferFixture {
    pair: Pair,
    dst: PathBuf,
    src: Vec<PathBuf>,
    recv_job: [u8; 16],
}

/// A paired fixture with the scheduler started on the redeemer (sender)
/// BEFORE the redeem (its transport must observe the transcript), `groups`
/// applied pairings (distinct code groups), a real concurrent receiver
/// (the owner device, `receiver_jobs` connections), and `files` source
/// files enqueued round-robin over the groups.
#[allow(clippy::too_many_arguments)]
fn transfer_fixture(
    same: &str,
    cross: &str,
    per_code: u32,
    files: usize,
    file_mb: usize,
    groups: usize,
    receiver_jobs: usize,
) -> TransferFixture {
    let mut pair = paired_fixture();
    // Scheduler first: its transport must observe the redeem's PairCommit.
    let sched_spec = CString::new(format!(
        "{{\"runtime_id\":\"{}\",\"same_code\":\"{same}\",\"cross_code\":\"{cross}\",\
         \"max_active_tasks_per_code\":{per_code},\"max_active_code_groups\":4,\
         \"max_queued_tasks\":64}}",
        hex16(&pair.a),
    ))
    .unwrap();
    assert_eq!(
        unsafe { ak_sched_start(pair.engine, sched_spec.as_ptr()) },
        AK_OK
    );

    // One applied pairing per group: the sender redeems `groups` distinct
    // invites of the owner (distinct code groups for the cross-code mode).
    let mut pairing_ids = Vec::new();
    for _ in 0..groups {
        create_invite_on(&mut pair);
        let (code, json) = run_op(
            pair.engine,
            &pair.a,
            Op::Redeem(&format!(
                "{{\"spec_version\":1,\"invite_code\":{}}}",
                jstr(&pair.invite_code)
            )),
        );
        assert_eq!(code, AK_DONE, "redeem: {json}");
        pairing_ids.push(field_str(&json, "pairing_id"));
    }

    // The sender's fingerprint (the receiver pins it).
    let mut buf = [0 as c_char; 2048];
    let id_str = CString::new(pair.a_dir.to_string_lossy().into_owned()).unwrap();
    assert_eq!(
        unsafe { ak_identity_info(pair.engine, id_str.as_ptr(), buf.as_mut_ptr(), buf.len()) },
        AK_DONE
    );
    let sender_fp = field_str(&cbuf_to_string(&buf), "fingerprint");

    // Receiver: the owner device, serving `receiver_jobs` connections.
    let dst = tempfile::tempdir().expect("dst tempdir").keep();
    let port = free_udp_port();
    let recv_spec = CString::new(format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":{},\"id_dir\":{},\
         \"once_jobs\":{receiver_jobs},\"timeout_secs\":120}}",
        path_jstr(&dst),
        jstr(&sender_fp),
        path_jstr(&pair.b_dir),
    ))
    .unwrap();
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe { ak_receive_start(pair.engine, recv_spec.as_ptr(), recv_job.as_mut_ptr()) },
        AK_OK
    );

    // Source files: distinct names, deterministic bytes, `file_mb` each.
    let src_dir = tempfile::tempdir().expect("src tempdir").keep();
    let mut src = Vec::new();
    let mut pattern = 0x2545F4914F6CDD1Du64;
    for index in 0..files {
        let mut bytes = Vec::with_capacity(file_mb << 20);
        while bytes.len() < (file_mb << 20) {
            pattern ^= pattern << 13;
            pattern ^= pattern >> 7;
            pattern ^= pattern << 17;
            bytes.extend_from_slice(&pattern.to_le_bytes());
        }
        let path = src_dir.join(format!("task-{index}.bin"));
        fs::write(&path, &bytes).expect("write source");
        src.push(path);
    }

    // Enqueue one task per source file through the ABI, round-robin over
    // the pairing groups.
    for (index, path) in src.iter().enumerate() {
        let pairing_id = &pairing_ids[index % pairing_ids.len()];
        let spec = CString::new(format!(
            "{{\"pairing_id\":\"{pairing_id}\",\"source_roots\":[{}],\
             \"peer\":\"127.0.0.1:{port}\",\"timeout_secs\":60}}",
            path_jstr(path),
        ))
        .unwrap();
        let mut task_id = [0u8; 16];
        assert_eq!(
            unsafe { ak_sched_enqueue(pair.engine, spec.as_ptr(), task_id.as_mut_ptr()) },
            AK_OK,
            "enqueue {}",
            path.display()
        );
    }

    TransferFixture {
        pair,
        dst,
        src,
        recv_job,
    }
}

impl TransferFixture {
    /// Wait for every task to complete; returns (max observed active, final
    /// snapshot).
    fn wait_all_completed(&self, tasks: usize) -> (u64, String) {
        let mut max_active = 0u64;
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let (code, json) = snapshot_text(self.pair.engine);
            assert_eq!(code, AK_DONE, "{json}");
            max_active = max_active.max(field_u64(&json, "active_tasks"));
            if count_state(&json, "Completed") == tasks
                && count_state(&json, "Failed") == 0
                && count_state(&json, "Cancelled") == 0
            {
                // Let the receiver finish its side before checking files.
                let mut buf = [0 as c_char; 8192];
                let recv_deadline = Instant::now() + Duration::from_secs(60);
                loop {
                    let code = unsafe {
                        ak_job_result(
                            self.pair.engine,
                            self.recv_job.as_ptr(),
                            buf.as_mut_ptr(),
                            buf.len(),
                        )
                    };
                    if code == AK_DONE || code == AK_FAILED {
                        return (max_active, json);
                    }
                    assert!(
                        Instant::now() < recv_deadline,
                        "receiver never finished: {}",
                        cbuf_to_string(&buf)
                    );
                    thread::sleep(Duration::from_millis(20));
                }
            }
            assert!(
                Instant::now() < deadline,
                "tasks did not complete; snapshot: {json}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn assert_files_delivered(&self) {
        for path in &self.src {
            let got = self.dst.join(
                path.file_name()
                    .expect("source file name")
                    .to_string_lossy()
                    .into_owned(),
            );
            let want = fs::read(path).expect("source readable");
            let got_bytes = fs::read(&got).unwrap_or_else(|error| {
                panic!("delivered file {} missing: {error}", got.display())
            });
            assert_eq!(want, got_bytes, "bytes differ for {}", got.display());
        }
    }
}

#[test]
fn sched_requires_runtime_and_single_start() {
    let engine = ak_engine_new();
    // No pairing runtime at all: -4 on every scheduler call.
    let spec = CString::new("{}").unwrap();
    assert_eq!(
        unsafe { ak_sched_start(engine, spec.as_ptr()) },
        AK_ERR_NOT_FOUND
    );
    let mut buf = [0 as c_char; 256];
    assert_eq!(
        unsafe { ak_sched_snapshot(engine, buf.as_mut_ptr(), buf.len()) },
        AK_ERR_NOT_FOUND
    );
    assert_eq!(
        unsafe { ak_sched_poll_event(engine, buf.as_mut_ptr(), buf.len()) },
        AK_ERR_NOT_FOUND
    );
    let policy = CString::new("{\"same_code\":\"serial\"}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(engine, policy.as_ptr()) },
        AK_ERR_NOT_FOUND
    );
    assert_eq!(
        unsafe { ak_sched_cancel(engine, [0u8; 16].as_ptr()) },
        AK_ERR_NOT_FOUND
    );
    let enqueue =
        CString::new("{\"pairing_id\":\"00\",\"source_roots\":[\"/x\"],\"peer\":\"127.0.0.1:1\"}")
            .unwrap();
    let mut task_id = [0u8; 16];
    assert_eq!(
        unsafe { ak_sched_enqueue(engine, enqueue.as_ptr(), task_id.as_mut_ptr()) },
        AK_ERR_NOT_FOUND
    );
    unsafe { ak_engine_free(engine) };
}

#[test]
fn sched_snapshot_and_set_policy_fields() {
    let fixture = transfer_fixture("serial", "parallel", 4, 1, 1, 1, 1);
    let engine = fixture.pair.engine;

    let (code, json) = snapshot_text(engine);
    assert_eq!(code, AK_DONE, "{json}");
    assert!(json.contains("\"same_code\":\"serial\""), "{json}");
    assert!(json.contains("\"cross_code\":\"parallel\""), "{json}");
    assert_eq!(field_u64(&json, "policy_revision"), 1, "{json}");
    assert!(json.contains("\"tasks\":["), "{json}");

    // Switch to parallel cross-code; the revision bumps.
    let policy = CString::new("{\"cross_code\":\"serial\"}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(engine, policy.as_ptr()) },
        AK_OK
    );
    let json = snapshot_until(engine, |json| field_u64(json, "policy_revision") == 2, 10);
    assert!(json.contains("\"cross_code\":\"serial\""), "{json}");

    // A bad policy word is -2 and changes nothing.
    let bad = CString::new("{\"same_code\":\"diagonal\"}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(engine, bad.as_ptr()) },
        AK_ERR_INVALID
    );
    let (_, json) = snapshot_text(engine);
    assert_eq!(field_u64(&json, "policy_revision"), 2, "{json}");

    // Let the fixture's single transfer finish, then drain the scheduler
    // events: the full task lifecycle must be observable.
    snapshot_until(engine, |json| count_state(json, "Completed") == 1, 120);
    let lines = drain_sched_events(engine);
    assert!(
        lines.iter().any(|line| line.contains("task_queued")),
        "no task_queued event in {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"outcome\":\"completed\"")),
        "no completed task event in {lines:?}"
    );
}

#[test]
fn sched_serial_same_code_never_runs_two() {
    let fixture = transfer_fixture("serial", "parallel", 4, 3, 6, 1, 3);
    let (max_active, snapshot) = fixture.wait_all_completed(3);
    assert_eq!(
        max_active, 1,
        "same-code serial must run exactly one task at a time (snapshot tail: {snapshot})"
    );
    fixture.assert_files_delivered();
}

#[test]
fn sched_parallel_same_code_overlaps() {
    let fixture = transfer_fixture("parallel", "parallel", 4, 3, 6, 1, 3);
    let (max_active, _snapshot) = fixture.wait_all_completed(3);
    assert!(
        max_active >= 2,
        "same-code parallel must run tasks concurrently (max active observed: {max_active})"
    );
    fixture.assert_files_delivered();
}

#[test]
fn sched_cross_code_serial_bounds_active_groups() {
    // Two independent pairings (two code groups), two tasks each: under
    // cross-code serial only ONE group may hold active tasks at any
    // moment, while the tasks inside that group run in parallel.
    let fixture = transfer_fixture("parallel", "serial", 2, 4, 6, 2, 4);
    let engine = fixture.pair.engine;

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saw_two_active = false;
    loop {
        let (code, json) = snapshot_text(engine);
        assert_eq!(code, AK_DONE, "{json}");
        if count_state(&json, "Completed") == 4 {
            break;
        }
        assert!(Instant::now() < deadline, "tasks did not complete: {json}");
        if field_u64(&json, "active_tasks") >= 2 {
            saw_two_active = true;
            // The active tasks (transfer phases) must all share one group.
            let active_groups = active_groups(&json);
            assert!(
                active_groups.len() <= 1,
                "cross-code serial runs one group at a time, saw {active_groups:?} in {json}"
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        saw_two_active,
        "the within-group parallel pair must actually overlap"
    );
    fixture.assert_files_delivered();
}

/// Distinct code groups holding tasks in an active-transfer phase. The
/// task JSON layout is `{"id":..,"state":..,"group":..}`, so the group
/// value follows the state field inside the same task object.
fn active_groups(snapshot: &str) -> Vec<String> {
    let mut groups = Vec::new();
    for state in ["Scanning", "Transferring", "Verifying", "Committing"] {
        let needle = format!("\"state\":\"{state}\"");
        let mut rest = snapshot;
        while let Some(at) = rest.find(&needle) {
            let after = &rest[at + needle.len()..];
            let group_start = after
                .find("\"group\":\"")
                .map(|pos| pos + "\"group\":\"".len())
                .expect("group after the state field");
            let tail = &after[group_start..];
            let group = &tail[..tail.find('"').expect("group value end")];
            if !groups.iter().any(|g: &String| g == group) {
                groups.push(group.to_string());
            }
            rest = tail;
        }
    }
    groups
}

#[test]
fn sched_cancel_removes_a_queued_task() {
    // Serial same-code with one long task: tasks 2 and 3 sit queued while
    // task 1 transfers; cancelling a queued task removes it without ever
    // connecting, the others still complete.
    let fixture = transfer_fixture("serial", "parallel", 1, 3, 8, 1, 2);
    let engine = fixture.pair.engine;

    // Wait until exactly one task runs and two wait in the queue.
    let snapshot = snapshot_until(
        engine,
        |json| field_u64(json, "active_tasks") == 1 && field_u64(json, "queued_tasks") == 2,
        30,
    );
    // Find a queued task id from the snapshot (first "Queued" occurrence).
    let needle = "\"state\":\"Queued\"";
    let at = snapshot.find(needle).expect("a queued task in {snapshot}");
    let id_start = snapshot[..at]
        .rfind("\"id\":\"")
        .map(|pos| pos + "\"id\":\"".len())
        .expect("task id before the state field");
    let id_end = id_start + snapshot[id_start..].find('"').unwrap();
    let queued_id_hex = snapshot[id_start..id_end].to_string();
    let mut queued_id = [0u8; 16];
    for (index, byte) in queued_id_hex.as_bytes().chunks(2).enumerate() {
        queued_id[index] =
            u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).expect("hex id");
    }

    assert_eq!(
        unsafe { ak_sched_cancel(engine, queued_id.as_ptr()) },
        AK_OK
    );

    let final_snapshot = snapshot_until(
        engine,
        |json| count_state(json, "Completed") == 2 && count_state(json, "Cancelled") == 1,
        180,
    );
    assert!(final_snapshot.contains(&queued_id_hex), "{final_snapshot}");

    // The receiver still gets the two surviving files.
    let mut recv_buf = [0 as c_char; 8192];
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let code = unsafe {
            ak_job_result(
                engine,
                fixture.recv_job.as_ptr(),
                recv_buf.as_mut_ptr(),
                recv_buf.len(),
            )
        };
        if code == AK_DONE || code == AK_FAILED {
            break;
        }
        assert!(Instant::now() < deadline, "receiver never finished");
        thread::sleep(Duration::from_millis(20));
    }
    let delivered = fixture
        .src
        .iter()
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            fixture.dst.join(name).exists()
        })
        .count();
    assert_eq!(delivered, 2, "exactly the two uncancelled files arrive");

    // Cancelling the already-terminal task is -4.
    assert_eq!(
        unsafe { ak_sched_cancel(engine, queued_id.as_ptr()) },
        AK_ERR_NOT_FOUND
    );
}

#[test]
fn sched_transfer_progress_flows_into_engine_progress_queue() {
    let fixture = transfer_fixture("serial", "parallel", 2, 1, 2, 1, 1);
    let _ = fixture.wait_all_completed(1);
    fixture.assert_files_delivered();

    // Per-task transfer progress surfaced through ak_poll_progress with
    // the task id as the job field (documented B8 wiring).
    let mut lines = Vec::new();
    let mut buf = [0 as c_char; 8192];
    loop {
        let code = unsafe { ak_poll_progress(fixture.pair.engine, buf.as_mut_ptr(), buf.len()) };
        if code == AK_PENDING {
            break;
        }
        assert_eq!(code, AK_EVENT);
        lines.push(cbuf_to_string(&buf));
    }
    assert!(
        lines.iter().any(|line| line.contains("\"event\":\"done\"")),
        "engine progress of the scheduled task in {lines:?}"
    );
}

#[test]
fn sched_enqueue_resolves_relations_locally_only() {
    let fixture = transfer_fixture("serial", "parallel", 2, 1, 1, 1, 1);
    let engine = fixture.pair.engine;

    // Unknown pairing: -4, nothing submitted; the message is readable
    // immediately (every later FFI call clears the buffer).
    let spec = CString::new(
        "{\"pairing_id\":\"11223344556677889900aabbccddeeff\",\"source_roots\":[\"/x\"],\
         \"peer\":\"127.0.0.1:1\"}",
    )
    .unwrap();
    let mut task_id = [0u8; 16];
    assert_eq!(
        unsafe { ak_sched_enqueue(engine, spec.as_ptr(), task_id.as_mut_ptr()) },
        AK_ERR_NOT_FOUND
    );
    let last = last_error_text(engine);
    assert!(
        last.contains("is not in this runtime's journal"),
        "the -4 carries a journal-resolution message: {last}"
    );

    // Bad peer address / bad pairing hex: -2.
    let bad_peer = CString::new(
        "{\"pairing_id\":\"11223344556677889900aabbccddeeff\",\"source_roots\":[\"/x\"],\
         \"peer\":\"not-an-address\"}",
    )
    .unwrap();
    assert_eq!(
        unsafe { ak_sched_enqueue(engine, bad_peer.as_ptr(), task_id.as_mut_ptr()) },
        AK_ERR_INVALID
    );
    let bad_hex =
        CString::new("{\"pairing_id\":\"zz\",\"source_roots\":[\"/x\"],\"peer\":\"127.0.0.1:1\"}")
            .unwrap();
    assert_eq!(
        unsafe { ak_sched_enqueue(engine, bad_hex.as_ptr(), task_id.as_mut_ptr()) },
        AK_ERR_INVALID
    );
}

fn last_error_text(engine: *mut AkEngine) -> String {
    let mut buf = [0 as c_char; 2048];
    let code = unsafe { ak_engine_last_error(engine, buf.as_mut_ptr(), buf.len()) };
    assert!(code >= 0, "last_error itself failed: {code}");
    cbuf_to_string(&buf)
}

// ---------------------------------------------------------------------------
// Wave 23 (`sched_ext`): the five exports completing the frozen ffi-v2 §1
// surface — ak_sched_listen_start (the contract's name for the scheduler
// start), ak_sched_retry, ak_sched_task_result, ak_sched_shutdown_start,
// ak_pairing_stop_start.
// ---------------------------------------------------------------------------

/// Poll one op's result to terminal state by op id (`run_op` mints its own
/// op; the wave-23 exports hand the op id out directly).
fn wait_op_result(engine: *mut AkEngine, runtime_id: &[u8; 16], op_id: &[u8; 16]) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let code = unsafe {
            ak_pairing_op_result(
                engine,
                runtime_id.as_ptr(),
                op_id.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(
            code == AK_PENDING || code == AK_DONE || code == AK_FAILED,
            "op_result {code}"
        );
        if code == AK_DONE || code == AK_FAILED {
            return (code, cbuf_to_string(&buf));
        }
        assert!(Instant::now() < deadline, "op never finished");
        thread::sleep(Duration::from_millis(15));
    }
}

/// Poll the scheduler event queue until one event named `name` shows up.
fn wait_sched_event(engine: *mut AkEngine, name: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut buf = [0 as c_char; 4096];
    loop {
        let code = unsafe { ak_sched_poll_event(engine, buf.as_mut_ptr(), buf.len()) };
        assert!(code == AK_EVENT || code == AK_PENDING, "sched poll {code}");
        if code == AK_EVENT {
            let line = cbuf_to_string(&buf);
            if line.contains(&format!("\"event\":\"{name}\"")) {
                return line;
            }
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "sched event {name} never arrived"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// The first `tasks[].id` of a snapshot JSON, as 32 hex chars.
fn first_task_id_hex(snapshot: &str) -> String {
    snapshot
        .split("\"id\":\"")
        .nth(1)
        .expect("a task id in the snapshot")
        .chars()
        .take(32)
        .collect()
}

/// Decode 32 hex characters into one 16-byte task id.
fn task_id_from_hex(hex: &str) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (index, slot) in id.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex task id");
    }
    id
}

#[test]
fn sched_listen_start_binds_named_runtime_and_reports_listener_scope() {
    let pair = paired_fixture();
    // The contract name binds the scheduler to the NAMED runtime even
    // when the spec itself names none (the id argument is injected).
    let spec = CString::new("{\"spec_version\":1,\"same_code\":\"serial\"}").unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_sched_listen_start(
                pair.engine,
                pair.a.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_OK
    );
    // The op is immediately terminal and honest about the wave-22 listener
    // scope cut.
    let (code, json) = wait_op_result(pair.engine, &pair.a, &op_id);
    assert_eq!(code, AK_DONE, "{json}");
    assert!(json.starts_with("{\"op\":\"listen_start\""), "{json}");
    assert!(json.contains("\"listener_enabled\":false"), "{json}");
    assert_eq!(field_str(&json, "runtime"), hex16(&pair.a), "{json}");

    // The scheduler really runs under the contract name: snapshot answers.
    let (code, snapshot) = snapshot_text(pair.engine);
    assert_eq!(code, AK_DONE, "{snapshot}");
    assert!(snapshot.contains("\"policy\""), "{snapshot}");

    // One scheduler per engine: a second listen_start is -2.
    assert_eq!(
        unsafe {
            ak_sched_listen_start(
                pair.engine,
                pair.a.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_ERR_INVALID
    );
    // A spec naming a DIFFERENT runtime than the id argument is -2 before
    // anything is rebound.
    let conflict = CString::new(format!("{{\"runtime_id\":\"{}\"}}", hex16(&pair.b))).unwrap();
    assert_eq!(
        unsafe {
            ak_sched_listen_start(
                pair.engine,
                pair.a.as_ptr(),
                conflict.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_ERR_INVALID
    );
    // The sched calls that take a runtime id must name the BOUND runtime.
    assert_eq!(
        unsafe { ak_sched_retry(pair.engine, pair.b.as_ptr(), [0x55; 16].as_ptr()) },
        AK_ERR_NOT_FOUND
    );
    // Unknown task on the correct binding: -4 as well.
    assert_eq!(
        unsafe { ak_sched_retry(pair.engine, pair.a.as_ptr(), [0x55; 16].as_ptr()) },
        AK_ERR_NOT_FOUND
    );
}

#[test]
fn sched_task_result_tracks_a_real_task_and_retry_rejects_nonparked() {
    let fixture = transfer_fixture("serial", "parallel", 1, 1, 1, 1, 1);
    let (max_active, snapshot) = fixture.wait_all_completed(1);
    assert!(
        max_active <= 1,
        "serial same-code mode: {max_active} active"
    );
    fixture.assert_files_delivered();

    let task_hex = first_task_id_hex(&snapshot);
    let task = task_id_from_hex(&task_hex);
    let engine = fixture.pair.engine;
    let runtime = fixture.pair.a;
    let mut buf = [0 as c_char; 4096];

    // AK_DONE with the snapshot-shaped task object, terminal state visible.
    assert_eq!(
        unsafe {
            ak_sched_task_result(
                engine,
                runtime.as_ptr(),
                task.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        },
        AK_DONE
    );
    let json = cbuf_to_string(&buf);
    assert!(json.contains("\"state\":\"Completed\""), "{json}");
    assert!(json.contains(&format!("\"id\":\"{task_hex}\"")), "{json}");

    // A too-small buffer is -3 and the retry is lossless.
    let mut tiny = [0 as c_char; 8];
    assert_eq!(
        unsafe {
            ak_sched_task_result(
                engine,
                runtime.as_ptr(),
                task.as_ptr(),
                tiny.as_mut_ptr(),
                tiny.len(),
            )
        },
        AK_ERR_BUFFER
    );
    assert_eq!(
        unsafe {
            ak_sched_task_result(
                engine,
                runtime.as_ptr(),
                task.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        },
        AK_DONE
    );
    assert_eq!(
        cbuf_to_string(&buf),
        json,
        "the -3 retry must re-read the same view"
    );

    // retry only restarts PARKED tasks: a completed task is -4.
    assert_eq!(
        unsafe { ak_sched_retry(engine, runtime.as_ptr(), task.as_ptr()) },
        AK_ERR_NOT_FOUND
    );
    // Unknown task id: -4.
    assert_eq!(
        unsafe {
            ak_sched_task_result(
                engine,
                runtime.as_ptr(),
                [0x77; 16].as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        },
        AK_ERR_NOT_FOUND
    );
}

#[test]
fn sched_shutdown_and_pairing_stop_lifecycle() {
    let pair = paired_fixture();
    let spec = CString::new("{\"spec_version\":1}").unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_sched_listen_start(
                pair.engine,
                pair.a.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_OK
    );

    // Shutdown is ACCEPTED immediately; the summary arrives via the op.
    let mut shut_op = [0u8; 16];
    let shut_spec = CString::new("{\"mode\":\"cancel\"}").unwrap();
    assert_eq!(
        unsafe {
            ak_sched_shutdown_start(
                pair.engine,
                pair.a.as_ptr(),
                shut_spec.as_ptr(),
                shut_op.as_mut_ptr(),
            )
        },
        AK_OK
    );
    let (code, json) = wait_op_result(pair.engine, &pair.a, &shut_op);
    assert_eq!(code, AK_DONE, "{json}");
    assert!(json.starts_with("{\"op\":\"shutdown\""), "{json}");
    assert!(json.contains("\"mode\":\"cancel\""), "{json}");
    // The frozen shutdown events flowed into the scheduler event queue.
    assert!(wait_sched_event(pair.engine, "shutdown_started", 5).contains("\"mode\":\"cancel\""));
    assert!(wait_sched_event(pair.engine, "shutdown_complete", 5).contains("shutdown_complete"));

    // Stopping one pairing runtime does not touch the other: b still runs.
    let mut stop_op = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_stop_start(pair.engine, pair.b.as_ptr(), stop_op.as_mut_ptr()) },
        AK_OK
    );
    let (code, stopped) = wait_op_result(pair.engine, &pair.b, &stop_op);
    assert_eq!(code, AK_DONE, "{stopped}");
    assert!(stopped.starts_with("{\"op\":\"stop\""), "{stopped}");
    assert!(stopped.contains(&hex16(&pair.b)), "{stopped}");
    // The runtime_stopped event fired, and later ops on the stopped
    // runtime fail fast with the stop reason.
    assert!(wait_event(pair.engine, &pair.b, "runtime_stopped", 5).contains(&hex16(&pair.b)));
    let (code, json) = run_op(
        pair.engine,
        &pair.b,
        Op::Create("{\"spec_version\":1,\"ttl_secs\":0}"),
    );
    assert_eq!(code, AK_FAILED, "{json}");
    assert!(json.contains("stopped by caller"), "{json}");
    // The runtime entry stays readable after the stop, so a second stop is
    // accepted and idempotent (tearing down an already-torn-down runtime
    // is a no-op that re-records the terminal op).
    assert_eq!(
        unsafe { ak_pairing_stop_start(pair.engine, pair.b.as_ptr(), stop_op.as_mut_ptr()) },
        AK_OK
    );
}

// ---------------------------------------------------------------------------
// Wave 26: P1-04 stop-race, P2-05 policy caps, P2-07 enqueue idempotency,
// P2-08 sync error propagation.
// ---------------------------------------------------------------------------

/// A one-shot TCP proxy in front of the real registry whose first
/// connection is held back until `release` fires: the pairing TLS
/// handshake cannot complete while the gate is closed, so a runtime
/// started through the proxy stays in `Starting` for exactly as long as
/// the test wants (the P1-04 window).
struct GatedRegistryProxy {
    addr: std::net::SocketAddr,
    release: mpsc::Sender<()>,
}

fn gated_registry_proxy(real: std::net::SocketAddr) -> GatedRegistryProxy {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("proxy bind");
    let addr = listener.local_addr().expect("proxy addr");
    let (release, gate) = mpsc::channel::<()>();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(client) = stream else { break };
            let Ok(upstream) = std::net::TcpStream::connect(real) else {
                continue;
            };
            // Hold EVERYTHING back until the gate opens.
            if gate.recv().is_err() {
                break;
            }
            let Ok(mut up_read) = upstream.try_clone() else {
                break;
            };
            let Ok(mut client_read) = client.try_clone() else {
                break;
            };
            let Ok(mut client_write) = client.try_clone() else {
                break;
            };
            let Ok(mut up_write) = upstream.try_clone() else {
                break;
            };
            thread::spawn(move || {
                let _ = std::io::copy(&mut client_read, &mut up_write);
            });
            let _ = std::io::copy(&mut up_read, &mut client_write);
        }
    });
    GatedRegistryProxy { addr, release }
}

/// P1-04: stopping a pairing runtime WHILE its background connect is
/// still in flight must be final — the delayed connection completing
/// afterwards can neither install the live client nor flip the phase to
/// Ready nor emit `runtime_ready`. With the pre-fix code the connect task
/// was unregistered (unabortable) and installed unconditionally, so the
/// runtime came back online after a successful stop.
#[test]
fn pairing_stop_during_starting_prevents_late_connect_resurrection() {
    let registry = TestRegistry::spawn();
    let proxy = gated_registry_proxy(registry.addr);
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let spec = CString::new(format!(
        "{{\"registry\":\"{}\",\"registry_id\":{},\"id_dir\":{},\"device_name\":\"ffi-device\"}}",
        proxy.addr,
        jstr(&registry.id_hex()),
        path_jstr(&dir),
    ))
    .unwrap();
    let mut runtime_id = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()) },
        AK_OK
    );

    // The connect is gated, so the runtime is Starting: stop it NOW,
    // before the TLS handshake ever completes.
    let mut stop_op = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_stop_start(engine, runtime_id.as_ptr(), stop_op.as_mut_ptr()) },
        AK_OK
    );
    let (code, stopped) = wait_op_result(engine, &runtime_id, &stop_op);
    assert_eq!(code, AK_DONE, "{stopped}");
    assert!(stopped.starts_with("{\"op\":\"stop\""), "{stopped}");
    assert!(wait_event(engine, &runtime_id, "runtime_stopped", 5).contains("runtime_stopped"));

    // NOW the delayed connect completes on the wire. The resurrection
    // window is open: give it real time to misbehave in.
    proxy.release.send(()).expect("gate opens");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut buf = [0 as c_char; 512];
        let code = unsafe {
            ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len())
        };
        assert!(code == AK_EVENT || code == AK_PENDING, "poll code {code}");
        if code == AK_EVENT {
            let line = cbuf_to_string(&buf);
            assert!(
                !line.contains("runtime_ready"),
                "a stopped runtime came back online: {line}"
            );
        }
        thread::sleep(Duration::from_millis(50));
    }

    // New ops fail fast with the frozen stop reason.
    let (code, json) = run_op(
        engine,
        &runtime_id,
        Op::Create("{\"spec_version\":1,\"ttl_secs\":0}"),
    );
    assert_eq!(code, AK_FAILED, "{json}");
    assert!(json.contains("stopped by caller"), "{json}");

    // A second stop stays accepted and idempotent.
    let mut again = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_stop_start(engine, runtime_id.as_ptr(), again.as_mut_ptr()) },
        AK_OK
    );
    unsafe { ak_engine_free(engine) };
}

/// P1-04 (doc-contradiction half): accepted op results stay readable
/// after the runtime was stopped — `sched_ext` documents exactly that;
/// the old teardown wiped the op table (`ops.clear()`), answering `-4`.
#[test]
fn pairing_stop_keeps_finished_op_results_readable() {
    let pair = paired_fixture();
    let spec = CString::new("{\"spec_version\":1,\"ttl_secs\":0}").unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_pairing_create_start(
                pair.engine,
                pair.b.as_ptr(),
                spec.as_ptr(),
                op_id.as_mut_ptr(),
            )
        },
        AK_OK
    );
    let (code, before) = wait_op_result(pair.engine, &pair.b, &op_id);
    assert_eq!(code, AK_DONE, "{before}");
    let code_text = field_str(&before, "code");

    let mut stop_op = [0u8; 16];
    assert_eq!(
        unsafe { ak_pairing_stop_start(pair.engine, pair.b.as_ptr(), stop_op.as_mut_ptr()) },
        AK_OK
    );
    let (code, stopped) = wait_op_result(pair.engine, &pair.b, &stop_op);
    assert_eq!(code, AK_DONE, "{stopped}");

    let mut buf = [0 as c_char; 8192];
    assert_eq!(
        unsafe {
            ak_pairing_op_result(
                pair.engine,
                pair.b.as_ptr(),
                op_id.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        },
        AK_DONE,
        "a finished op must survive the stop of its runtime"
    );
    let after = cbuf_to_string(&buf);
    assert!(after.contains(&code_text), "{after}");
}

/// P2-05: the three wave-24 caps (max_active_tasks,
/// max_queued_tasks_per_code, max_pending_admissions) flow through the
/// FFI into the live policy and back out of the snapshot; a field outside
/// the frozen set is `-2` (never accepted-then-ignored); range and
/// coupling violations are `-2`.
#[test]
fn sched_policy_wave24_caps_apply_and_unknown_fields_reject() {
    let pair = paired_fixture();
    let spec = CString::new(format!(
        "{{\"runtime_id\":\"{}\",\"same_code\":\"parallel\",\"cross_code\":\"parallel\",\
         \"max_active_tasks_per_code\":2,\"max_active_code_groups\":3,\"max_queued_tasks\":40,\
         \"max_active_tasks\":5,\"max_queued_tasks_per_code\":6,\"max_pending_admissions\":7}}",
        hex16(&pair.a),
    ))
    .unwrap();
    assert_eq!(unsafe { ak_sched_start(pair.engine, spec.as_ptr()) }, AK_OK);

    let (code, json) = snapshot_text(pair.engine);
    assert_eq!(code, AK_DONE, "{json}");
    assert_eq!(field_u64(&json, "max_active_tasks"), 5, "{json}");
    assert_eq!(field_u64(&json, "max_queued_tasks_per_code"), 6, "{json}");
    assert_eq!(field_u64(&json, "max_pending_admissions"), 7, "{json}");
    assert_eq!(field_u64(&json, "max_active_tasks_per_code"), 2, "{json}");
    assert_eq!(field_u64(&json, "max_active_code_groups"), 3, "{json}");
    assert_eq!(field_u64(&json, "max_queued_tasks"), 40, "{json}");

    // Dynamic switch of the wave-24 caps takes effect.
    let policy = CString::new(
        "{\"max_active_tasks\":1,\"max_queued_tasks_per_code\":2,\"max_pending_admissions\":3}",
    )
    .unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(pair.engine, policy.as_ptr()) },
        AK_OK
    );
    let json = snapshot_until(
        pair.engine,
        |json| field_u64(json, "max_active_tasks") == 1,
        10,
    );
    assert_eq!(field_u64(&json, "max_queued_tasks_per_code"), 2, "{json}");
    assert_eq!(field_u64(&json, "max_pending_admissions"), 3, "{json}");

    // Unknown field: explicit -2 with a naming message (P2-05's "不能成功
    // 后按另一套值执行"), and nothing changed.
    let unknown = CString::new("{\"max_tasks\":1}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(pair.engine, unknown.as_ptr()) },
        AK_ERR_INVALID
    );
    let last = last_error_text(pair.engine);
    assert!(last.contains("unknown policy field"), "{last}");
    let (_, json) = snapshot_text(pair.engine);
    assert_eq!(field_u64(&json, "max_active_tasks"), 1, "{json}");

    // Out of the frozen range: -2.
    let big = CString::new("{\"max_active_tasks\":65}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(pair.engine, big.as_ptr()) },
        AK_ERR_INVALID
    );

    // Contract §3 coupling: an EXPLICIT per-code queue bound above the
    // global bound is rejected.
    let coupled = CString::new("{\"max_queued_tasks\":8,\"max_queued_tasks_per_code\":9}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(pair.engine, coupled.as_ptr()) },
        AK_ERR_INVALID
    );

    // A DEFAULTED per-code bound above a shrunk global bound clamps down.
    let clamp = CString::new("{\"max_queued_tasks\":8}").unwrap();
    assert_eq!(
        unsafe { ak_sched_set_policy(pair.engine, clamp.as_ptr()) },
        AK_OK
    );
    let json = snapshot_until(
        pair.engine,
        |json| field_u64(json, "max_queued_tasks") == 8,
        10,
    );
    assert_eq!(field_u64(&json, "max_queued_tasks_per_code"), 8, "{json}");
}

/// P2-07: `ak_sched_enqueue` honors the ffi-v2 §2 `request_id`
/// idempotency — a replay of the same request returns the ORIGINAL task
/// id (no second task, dedup before any side effect); the same
/// request_id with a different spec is `-2`; without a request_id every
/// call accepts a fresh task.
#[test]
fn sched_enqueue_request_id_is_idempotent() {
    let mut pair = paired_fixture();
    let sched_spec = CString::new(format!(
        "{{\"runtime_id\":\"{}\",\"same_code\":\"serial\"}}",
        hex16(&pair.a)
    ))
    .unwrap();
    assert_eq!(
        unsafe { ak_sched_start(pair.engine, sched_spec.as_ptr()) },
        AK_OK
    );
    create_invite_on(&mut pair);
    let (code, json) = run_op(
        pair.engine,
        &pair.a,
        Op::Redeem(&format!(
            "{{\"spec_version\":1,\"invite_code\":{}}}",
            jstr(&pair.invite_code)
        )),
    );
    assert_eq!(code, AK_DONE, "redeem: {json}");
    let pairing_id = field_str(&json, "pairing_id");

    let src = tempfile::tempdir().expect("src tempdir").keep();
    let file_a = src.join("a.bin");
    let file_b = src.join("b.bin");
    fs::write(&file_a, b"payload-a").expect("write a");
    fs::write(&file_b, b"payload-b").expect("write b");
    let request_id = "0123456789abcdef0123456789abcdef";

    let spec_a = format!(
        "{{\"spec_version\":1,\"request_id\":\"{request_id}\",\"pairing_id\":\"{pairing_id}\",\
         \"source_roots\":[{}],\"peer\":\"127.0.0.1:1\"}}",
        path_jstr(&file_a),
    );
    // The SAME request with members in a different order — the canonical
    // form must treat it as identical.
    let spec_a_reordered = format!(
        "{{\"peer\":\"127.0.0.1:1\",\"source_roots\":[{}],\
         \"pairing_id\":\"{pairing_id}\",\"request_id\":\"{request_id}\",\"spec_version\":1}}",
        path_jstr(&file_a),
    );

    let mut first = [0u8; 16];
    let spec = CString::new(spec_a).unwrap();
    assert_eq!(
        unsafe { ak_sched_enqueue(pair.engine, spec.as_ptr(), first.as_mut_ptr()) },
        AK_OK
    );
    let mut replay = [0u8; 16];
    let spec = CString::new(spec_a_reordered).unwrap();
    assert_eq!(
        unsafe { ak_sched_enqueue(pair.engine, spec.as_ptr(), replay.as_mut_ptr()) },
        AK_OK
    );
    assert_eq!(
        first, replay,
        "the same request_id must replay the original task id"
    );

    // Same request_id, DIFFERENT spec: -2, named reason, no new task.
    let spec_conflict = CString::new(format!(
        "{{\"spec_version\":1,\"request_id\":\"{request_id}\",\"pairing_id\":\"{pairing_id}\",\
         \"source_roots\":[{}],\"peer\":\"127.0.0.1:1\"}}",
        path_jstr(&file_b),
    ))
    .unwrap();
    let mut conflict = [0u8; 16];
    assert_eq!(
        unsafe { ak_sched_enqueue(pair.engine, spec_conflict.as_ptr(), conflict.as_mut_ptr()) },
        AK_ERR_INVALID
    );
    let last = last_error_text(pair.engine);
    assert!(last.contains("different spec"), "{last}");

    // Without a request_id no dedup happens: a fresh, distinct task.
    let spec_fresh = CString::new(format!(
        "{{\"spec_version\":1,\"pairing_id\":\"{pairing_id}\",\"source_roots\":[{}],\
         \"peer\":\"127.0.0.1:1\"}}",
        path_jstr(&file_a),
    ))
    .unwrap();
    let mut fresh = [0u8; 16];
    assert_eq!(
        unsafe { ak_sched_enqueue(pair.engine, spec_fresh.as_ptr(), fresh.as_mut_ptr()) },
        AK_OK
    );
    assert_ne!(first, fresh, "an unkeyed enqueue is a new task");

    // Exactly two tasks exist: the replay created nothing.
    let json = snapshot_until(
        pair.engine,
        |json| json.matches("\"state\":\"").count() == 2,
        15,
    );
    assert!(json.contains(&hex16(&first)), "{json}");
    assert!(json.contains(&hex16(&fresh)), "{json}");
}

/// P2-08 (Rust side): every synchronous failure path records a
/// thread-local last-error message — the status code alone is never the
/// whole answer. Representative -1/-2/-3/-4 paths.
#[test]
fn synchronous_failure_paths_leave_last_error_messages() {
    let engine = ak_engine_new();

    // -1: null output pointer (argument validation).
    let spec = CString::new("{}").unwrap();
    assert_eq!(
        unsafe { ak_sched_enqueue(engine, spec.as_ptr(), std::ptr::null_mut()) },
        AK_ERR_NULL
    );
    assert!(!last_error_text(engine).is_empty(), "-1 needs a message");

    // -2: malformed spec JSON.
    let bad = CString::new("{not json").unwrap();
    let mut job = [0u8; 16];
    assert_eq!(
        unsafe { ak_send_start(engine, bad.as_ptr(), job.as_mut_ptr()) },
        AK_ERR_INVALID
    );
    assert!(!last_error_text(engine).is_empty(), "-2 needs a message");

    // -3: caller buffer too small names the needed size.
    let mut tiny = [0 as c_char; 2];
    assert_eq!(
        unsafe { ak_version(tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let last = last_error_text(engine);
    assert!(
        last.contains("need"),
        "the -3 message names the size: {last}"
    );

    // -4: no scheduler runtime on this engine.
    let mut buf = [0 as c_char; 256];
    assert_eq!(
        unsafe { ak_sched_snapshot(engine, buf.as_mut_ptr(), buf.len()) },
        AK_ERR_NOT_FOUND
    );
    assert!(!last_error_text(engine).is_empty(), "-4 needs a message");

    unsafe { ak_engine_free(engine) };
}
