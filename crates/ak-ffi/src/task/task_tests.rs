use super::*;
// C02: the C exports are `unsafe extern "C" fn` now; this module is
// `#![forbid(unsafe_code)]` (whole file), so the audited one-line
// unsafe calls live in `task_ext::testabi` and are imported here under
// the SAME names — the call sites below are unchanged and still drive
// the real exported shapes, just through the test-only adapters.
use crate::task_ext::testabi::{
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_pairing_op_result,
    ak_pairing_poll_event, ak_task_consent, ak_task_create_code, ak_task_disconnect, ak_task_join,
    ak_task_list, ak_task_send, ak_task_server_info, ak_task_set_event_notify,
    ak_task_set_save_dir, ak_task_start,
};
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const CAP: usize = 8192;
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

// ---- the real v2 registry (dedicated thread; the FFI engine keeps
// its own embedded runtime, so the two never nest) ------------------

/// The real v2 registry on a dedicated thread (current-thread tokio
/// runtime), same shape as `tests/ffi_pairing.rs`.
struct TestRegistry {
    addr: SocketAddr,
    registry_id: [u8; 32],
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestRegistry {
    fn spawn() -> TestRegistry {
        Self::spawn_with_revision(None)
    }

    /// G6b: the same registry with an explicit build revision (the
    /// `serve_v2_with_revision` path production injects — the
    /// `ak_task_server_info` op's answer source).
    fn spawn_with_revision(revision: Option<&'static str>) -> TestRegistry {
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
                let server = ak_signal::server_v2::engine::serve_v2_with_revision(
                    listener,
                    &key,
                    store,
                    ak_signal::server_v2::Limits::default(),
                    revision,
                    stop_rx,
                );
                tokio::select! {
                    _ = server => {}
                    _ = shutdown_rx => {
                        let _ = stop_tx.send(true);
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

// ---- a REAL minimal RFC5389 binding responder on loopback -------------
// (CR-P1-02 改法第 2 条 product tests): the ak-signal STUN codec on a
// std thread — the same wire format `bind_receiver_socket` dials, so
// the tests exercise the real client against a real responder.

/// One loopback STUN responder. `mapped = None` answers with the
/// OBSERVED source address (a truthful reflection: on loopback the
/// reflexive address is the querier's own local socket address, which
// is exactly what the same-socket assertions need); `Some(addr)`
/// answers with a fixed address (the distinct-reflexive case).
struct TestStun {
    addr: SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl TestStun {
    fn spawn(mapped: Option<SocketAddr>) -> TestStun {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind test STUN");
        let addr = socket.local_addr().expect("test STUN addr");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .expect("STUN responder read timeout");
            let mut buf = [0u8; 128];
            loop {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(received) => received,
                    Err(_) => continue, // timeout tick: re-check the flag
                };
                let Ok(transaction) = ak_signal::stun::parse_binding_request(&buf[..len]) else {
                    continue;
                };
                let reply = ak_signal::stun::binding_response(transaction, mapped.unwrap_or(from));
                let _ = socket.send_to(&reply, from);
            }
        });
        TestStun { addr, stop }
    }
}

impl Drop for TestStun {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

fn cbuf(cap: usize) -> Vec<c_char> {
    vec![0 as c_char; cap]
}

// `c_char` is `u8` on aarch64 (where our unix clippy probe runs) but `i8`
// on other mainstream targets, so the cast below is required on half the
// platforms and an identity on the other half.
#[allow(clippy::unnecessary_cast)]
fn text_of(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8(bytes).expect("FFI output is UTF-8")
}

fn last_error_of(engine: *mut AkEngine) -> String {
    let mut buf = cbuf(1024);
    let len = ak_engine_last_error(engine, buf.as_mut_ptr(), buf.len());
    assert!(len >= 0, "last_error must be readable");
    text_of(&buf[..len as usize])
}

fn spec_for(id_dir: &std::path::Path) -> String {
    // A syntactically valid registry pin (32 bytes of 0x11); only the
    // validation paths use this spec (no live registry, no op runs).
    format!(
        "{{\"spec_version\":1,\"registry\":\"127.0.0.1:7200\",\
         \"registry_id\":\"{}\",\"id_dir\":\"{}\"}}",
        "11".repeat(32),
        id_dir.to_string_lossy().replace('\\', "/"),
    )
}

/// The spec of a runtime whose ops pair through a LIVE registry.
fn registry_spec(registry: &TestRegistry, id_dir: &std::path::Path) -> String {
    format!(
        "{{\"spec_version\":1,\"registry\":\"{}\",\
         \"registry_id\":\"{}\",\"id_dir\":\"{}\"}}",
        registry.addr,
        registry.id_hex(),
        id_dir.to_string_lossy().replace('\\', "/"),
    )
}

fn start_task(engine: *mut AkEngine, id_dir: &std::path::Path) -> [u8; 16] {
    let spec = CString::new(spec_for(id_dir)).expect("spec CString");
    let mut runtime_id = [0u8; 16];
    let code = ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr());
    let detail = last_error_of(engine);
    assert_eq!(code, 0, "ak_task_start failed: {detail}");
    runtime_id
}

fn start_task_on(
    engine: *mut AkEngine,
    registry: &TestRegistry,
    id_dir: &std::path::Path,
) -> [u8; 16] {
    let spec = CString::new(registry_spec(registry, id_dir)).expect("spec CString");
    let mut runtime_id = [0u8; 16];
    let code = ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr());
    let detail = last_error_of(engine);
    assert_eq!(code, 0, "ak_task_start failed: {detail}");
    runtime_id
}

/// Poll `ak_pairing_poll_event` until one event whose body contains
/// `needle` arrives (draining older events into a log).
fn wait_event(engine: *mut AkEngine, runtime_id: &[u8; 16], needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        let mut buf = cbuf(CAP);
        let code = ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len());
        if code == 1 {
            let line = text_of(&buf);
            if line.contains(needle) {
                return line;
            }
            seen.push(line);
        } else {
            assert!(code == 0, "poll_event failed: {code}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; saw {seen:?}"
        );
        std::thread::sleep(POLL);
    }
}

/// Poll `ak_pairing_op_result` until the op settles; returns
/// `(ok, json)`.
fn wait_op(engine: *mut AkEngine, runtime_id: &[u8; 16], op_id: &[u8; 16]) -> (bool, String) {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut buf = cbuf(CAP);
        let code = ak_pairing_op_result(
            engine,
            runtime_id.as_ptr(),
            op_id.as_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
        );
        match code {
            0 => {}
            1 | 2 => return (code == 1, text_of(&buf)),
            other => panic!("op_result failed: {other} ({})", last_error_of(engine)),
        }
        assert!(Instant::now() < deadline, "timed out waiting for the op");
        std::thread::sleep(POLL);
    }
}

fn create_code(
    engine: *mut AkEngine,
    runtime_id: &[u8; 16],
    spec: &str,
) -> ([u8; 16], bool, String) {
    let c_spec = CString::new(spec).expect("spec CString");
    let mut op_id = [0u8; 16];
    let code = ak_task_create_code(
        engine,
        runtime_id.as_ptr(),
        c_spec.as_ptr(),
        op_id.as_mut_ptr(),
    );
    assert_eq!(
        code,
        0,
        "ak_task_create_code not accepted: {}",
        last_error_of(engine)
    );
    let (ok, json) = wait_op(engine, runtime_id, &op_id);
    (op_id, ok, json)
}

fn code_of(json: &str) -> String {
    json.split("\"code\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("code in result")
        .to_string()
}

fn conn_of(json: &str) -> [u8; 16] {
    let hex = json
        .split("\"conn\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("conn id");
    hex16_id(hex)
}

fn hex16_id(hex: &str) -> [u8; 16] {
    let mut id = [0u8; 16];
    for (index, byte) in hex.as_bytes().chunks(2).enumerate() {
        id[index] = u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap();
    }
    id
}

fn join_code(engine: *mut AkEngine, runtime_id: &[u8; 16], code_text: &str) -> (bool, String) {
    join_code_as(engine, runtime_id, code_text, None)
}

/// Join with an explicit joiner-role hint (the cross-device path —
/// the code's declaration is unreadable before the redeem).
fn join_code_as(
    engine: *mut AkEngine,
    runtime_id: &[u8; 16],
    code_text: &str,
    role: Option<&str>,
) -> (bool, String) {
    let role_field = role
        .map(|text| format!(",\"role\":\"{text}\""))
        .unwrap_or_default();
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"code\":\"{code_text}\"{role_field}}}"
    ))
    .expect("spec CString");
    let mut op_id = [0u8; 16];
    let status = ak_task_join(
        engine,
        runtime_id.as_ptr(),
        spec.as_ptr(),
        op_id.as_mut_ptr(),
    );
    assert_eq!(
        status,
        0,
        "ak_task_join not accepted: {}",
        last_error_of(engine)
    );
    wait_op(engine, runtime_id, &op_id)
}

// -- ak_task_start ------------------------------------------------------

#[test]
fn task_start_accepts_and_emits_runtime_ready() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    assert_ne!(runtime_id, [0u8; 16]);
    let event = wait_event(engine, &runtime_id, "runtime_ready");
    assert!(
        event.contains("\"runtime\":"),
        "runtime id rides the event: {event}"
    );
    ak_engine_free(engine);
}

#[test]
fn task_start_validates_arguments_before_side_effects() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let good = CString::new(spec_for(dir.path())).expect("spec");
    let mut out = [0u8; 16];
    // Null output pointer: refused BEFORE parsing (no runtime minted).
    assert_eq!(
        ak_task_start(engine, good.as_ptr(), std::ptr::null_mut()),
        -1
    );
    // Null spec.
    assert_eq!(
        ak_task_start(engine, std::ptr::null(), out.as_mut_ptr()),
        -1
    );
    // Malformed JSON.
    let bad = CString::new("{nope").expect("bad");
    assert_eq!(ak_task_start(engine, bad.as_ptr(), out.as_mut_ptr()), -2);
    assert!(
        !last_error_of(engine).is_empty(),
        "the parse detail lands in last_error"
    );
    // Missing registry.
    let no_reg = CString::new(format!(
        "{{\"id_dir\":\"{}\"}}",
        dir.path().to_string_lossy().replace('\\', "/")
    ))
    .expect("spec");
    assert_eq!(ak_task_start(engine, no_reg.as_ptr(), out.as_mut_ptr()), -2);
    assert!(last_error_of(engine).contains("registry"));
    // Bad registry pin (not 64 hex).
    let bad_pin = CString::new(format!(
        "{{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"zz\",\"id_dir\":\"{}\"}}",
        dir.path().to_string_lossy().replace('\\', "/")
    ))
    .expect("spec");
    assert_eq!(
        ak_task_start(engine, bad_pin.as_ptr(), out.as_mut_ptr()),
        -2
    );
    assert!(last_error_of(engine).contains("registry_id"));
    // Empty id_dir.
    let no_dir =
        CString::new("{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"11\",\"id_dir\":\"\"}")
            .expect("spec");
    assert_eq!(ak_task_start(engine, no_dir.as_ptr(), out.as_mut_ptr()), -2);
    // CR-P1-02 改法第 2 条: a present-but-malformed stun_addr is a
    // validation failure (absent/empty stays legal — fail-open, no
    // query).
    let bad_stun = CString::new(format!(
        "{{\"registry\":\"127.0.0.1:7200\",\"registry_id\":\"{}\",\
         \"id_dir\":\"{}\",\"stun_addr\":\"not-an-address\"}}",
        "11".repeat(32),
        dir.path().to_string_lossy().replace('\\', "/"),
    ))
    .expect("spec");
    assert_eq!(
        ak_task_start(engine, bad_stun.as_ptr(), out.as_mut_ptr()),
        -2
    );
    assert!(last_error_of(engine).contains("stun_addr"));
    // No runtime was registered by any failed call: poll says -4.
    assert_eq!(
        ak_pairing_poll_event(engine, out.as_ptr(), cbuf(CAP).as_mut_ptr(), CAP),
        -4
    );
    ak_engine_free(engine);
}

// -- ak_task_server_info (G6b) ------------------------------------------

/// G6b positive: the op asks the LIVE registry (via the runtime's
/// shared client) and resolves OK with the server's configured build
/// revision — read through the SHARED `ak_pairing_op_result` channel,
/// like every task op.
#[test]
fn server_info_op_reports_the_registry_revision() {
    let registry = TestRegistry::spawn_with_revision(Some("0.1.0+g6btest1"));
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    let mut op_id = [0u8; 16];
    let code = ak_task_server_info(engine, runtime_id.as_ptr(), op_id.as_mut_ptr());
    let detail = last_error_of(engine);
    assert_eq!(code, 0, "ak_task_server_info failed: {detail}");
    let (ok, json) = wait_op(engine, &runtime_id, &op_id);
    assert!(
        ok,
        "the revision query never fails against a live server: {json}"
    );
    assert_eq!(
        json, "{\"op\":\"task_server_info\",\"revision\":\"0.1.0+g6btest1\"}",
        "the configured build revision rides the op result"
    );
    // The repeat is cheap and answers the SAME cached fact (the
    // per-epoch cache of `RegistryClient::server_info`).
    let mut op2 = [0u8; 16];
    let code2 = ak_task_server_info(engine, runtime_id.as_ptr(), op2.as_mut_ptr());
    assert_eq!(code2, 0, "repeat query accepted: {}", last_error_of(engine));
    let (ok2, json2) = wait_op(engine, &runtime_id, &op2);
    assert!(ok2, "{json2}");
    assert_eq!(
        json2, "{\"op\":\"task_server_info\",\"revision\":\"0.1.0+g6btest1\"}",
        "the repeat answers the same cached revision"
    );
    ak_engine_free(engine);
}

/// G6b degrade + negative: pointer discipline first, unknown runtime
/// `-4`, and an unreachable registry degrades the op to `revision:null`
/// — never an error verdict (the `server_info` degrade contract).
#[test]
fn server_info_op_degrades_and_validates() {
    let engine = ak_engine_new();
    let mut op_id = [0u8; 16];
    // Null output pointer: refused before anything is registered.
    assert_eq!(
        ak_task_server_info(engine, [0u8; 16].as_ptr(), std::ptr::null_mut()),
        -1
    );
    // Unknown runtime.
    assert_eq!(
        ak_task_server_info(engine, [0u8; 16].as_ptr(), op_id.as_mut_ptr()),
        -4
    );
    // A runtime whose registry is unreachable (nothing listens at
    // 127.0.0.1:7200): the op is ACCEPTED and then FAILS with a
    // readable error — a stack that could not even be established is
    // an honest op failure, NOT the `revision:null` degrade (which is
    // reserved for a reachable server that lacks/failed the query).
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    let mut op2 = [0u8; 16];
    let code = ak_task_server_info(engine, runtime_id.as_ptr(), op2.as_mut_ptr());
    assert_eq!(code, 0, "accepted: {}", last_error_of(engine));
    let (ok, json) = wait_op(engine, &runtime_id, &op2);
    assert!(
        !ok,
        "an unreachable registry is an op failure, not a null answer: {json}"
    );
    assert!(
        json.contains("\"op\":\"task_server_info\"") && json.contains("\"error\":"),
        "the failure names the op and carries a reason: {json}"
    );
    ak_engine_free(engine);
}

// -- ak_task_create_code ------------------------------------------------

#[test]
fn create_code_sender_role_op_roundtrip() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    let (_, ok, json) = create_code(
        engine,
        &runtime_id,
        "{\"spec_version\":1,\"role\":\"sender\"}",
    );
    assert!(ok, "create must succeed at the real registry: {json}");
    assert!(json.contains("\"op\":\"task_create_code\""), "{json}");
    // The registry mints the invite display form (AKI1-…).
    assert!(
        json.contains("\"code\":\"AKI1-"),
        "registry display code: {json}"
    );
    assert!(json.contains("\"role\":\"sender\""), "{json}");
    assert!(json.contains("\"code_id\":\""), "{json}");
    assert!(json.contains("\"endpoint\":"), "{json}");
    let event = wait_event(engine, &runtime_id, "op_done");
    assert!(event.contains("\"op\":\"task_create_code\""), "{event}");
    ak_engine_free(engine);
}

#[test]
fn create_code_receiver_role_binds_a_real_endpoint() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    let (_, ok, json) = create_code(
        engine,
        &runtime_id,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "receiver-role create must bind a real socket: {json}");
    // The endpoint is a real ip:port (the derivation rewrote the
    // wildcard bind to interface addresses).
    assert!(json.contains("\"endpoint\":\""), "{json}");
    ak_engine_free(engine);
}

/// Batch G6 (R36-P2-04, the FFI half): the task runtime participates
/// in the per-identity control-connection lock — a SECOND runtime on
/// the same id_dir (same engine, same PID) fails its first registry
/// op loudly instead of silently kicking the first runtime's control
/// connection, and the lease releases at teardown (the lock file
/// disappears with the engine).
#[test]
fn second_runtime_on_the_same_id_dir_fails_its_first_op_loudly() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let first = start_task_on(engine, &registry, dir.path());
    let (_, ok, json) = create_code(engine, &first, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok, "the first runtime's create must succeed: {json}");
    // The first runtime's stack now holds the identity's lock.
    let lock_file = dir.path().join("registry-connection.lock");
    assert!(lock_file.exists(), "the lock file exists while held");

    // A second runtime on the SAME id_dir: start is accepted (local
    // bootstrap only — no network yet), but its first registry op
    // must refuse loudly.
    let second = start_task_on(engine, &registry, dir.path());
    let (_, ok, json) = create_code(engine, &second, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(!ok, "the second runtime's op must fail: {json}");
    assert!(
        json.contains("control-connection lock"),
        "the failure names the lock: {json}"
    );
    assert!(
        json.contains("second runtime"),
        "the failure names the same-process second-runtime situation: {json}"
    );

    // Teardown releases: the engine drop releases every stack's lease,
    // so a later run (or a CLI task command) can claim the identity.
    ak_engine_free(engine);
    assert!(
        !lock_file.exists(),
        "the lock file is gone after the engine teardown"
    );
}

#[test]
fn create_code_validates_before_side_effects() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    let mut op_id = [0u8; 16];
    // Unknown role value: -2 at the call, no op minted.
    let bad_role = CString::new("{\"role\":\"both\"}").expect("spec");
    assert_eq!(
        ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            bad_role.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -2
    );
    assert!(last_error_of(engine).contains("role"));
    // Missing role.
    let no_role = CString::new("{}").expect("spec");
    assert_eq!(
        ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            no_role.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -2
    );
    // Bad candidate shape (not ip:port).
    let bad_cand =
        CString::new("{\"role\":\"sender\",\"candidates\":[\"not-an-addr\"]}").expect("spec");
    assert_eq!(
        ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            bad_cand.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -2
    );
    // Unknown runtime: -4.
    assert_eq!(
        ak_task_create_code(
            engine,
            [9u8; 16].as_ptr(),
            bad_role.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -4
    );
    // Null op_id_out: -1.
    let good = CString::new("{\"role\":\"sender\"}").expect("spec");
    assert_eq!(
        ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            good.as_ptr(),
            std::ptr::null_mut()
        ),
        -1
    );
    ak_engine_free(engine);
}

#[test]
fn create_code_request_id_is_idempotent() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    let spec = "{\"spec_version\":1,\"role\":\"sender\",\
                \"request_id\":\"a1b2c3d4e5f60718293a4b5c6d7e8f90\"}";
    let (op_one, ok_one, json_one) = create_code(engine, &runtime_id, spec);
    assert!(ok_one, "{json_one}");
    let (op_two, ok_two, json_two) = create_code(engine, &runtime_id, spec);
    assert!(ok_two, "{json_two}");
    // Exact replay: the SAME op id and the SAME code — never a second
    // registry claim, never a second code.
    assert_eq!(op_one, op_two, "replay must answer the original op");
    assert_eq!(json_one, json_two);
    // Same request_id, different spec: -2 before any side effect.
    let conflict = CString::new(
        "{\"spec_version\":1,\"role\":\"receiver\",\
          \"request_id\":\"a1b2c3d4e5f60718293a4b5c6d7e8f90\"}",
    )
    .expect("spec");
    let mut op_id = [0u8; 16];
    assert_eq!(
        ak_task_create_code(
            engine,
            runtime_id.as_ptr(),
            conflict.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -2
    );
    ak_engine_free(engine);
}

// -- ak_task_join -------------------------------------------------------

#[test]
fn join_links_both_sides_and_lists_the_connection() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine, &registry, dir_a.path());
    let joiner = start_task_on(engine, &registry, dir_b.path());
    let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok, "{json}");
    let code = code_of(&json);
    // Separator tolerance: the UI's normalized (hyphen-less) form of
    // the SAME code joins the SAME registration.
    let stripped = code.split('-').collect::<String>();
    let stripped_lower = stripped.to_lowercase();
    let (join_ok, join_json) = join_code(engine, &joiner, &stripped_lower);
    assert!(
        join_ok,
        "join must succeed through the registry: {join_json}"
    );
    assert!(join_json.contains("\"op\":\"task_join\""), "{join_json}");
    assert!(
        join_json.contains("\"role\":\"receiver\""),
        "joiner gets the opposite role: {join_json}"
    );
    assert!(join_json.contains("\"conn\":\""), "{join_json}");
    // The real probe answered by the owner's echo responder.
    assert!(join_json.contains("\"probe\":\"verified\""), "{join_json}");
    // Bilateral `connected` events (the owner's from its owner loop).
    let on_owner = wait_event(engine, &owner, "\"event\":\"connected\"");
    let on_joiner = wait_event(engine, &joiner, "\"event\":\"connected\"");
    assert!(on_owner.contains("\"role\":\"sender\""), "{on_owner}");
    assert!(on_joiner.contains("\"role\":\"receiver\""), "{on_joiner}");
    // Probe event on the joiner: the real verdict.
    let probe = wait_event(engine, &joiner, "probe_result");
    assert!(probe.contains("\"reachable\":true"), "{probe}");
    // Both lists show the connection (the probe runs INSIDE the join
    // op, so both sides have settled by the time the op answered).
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    let owner_list = text_of(&buf);
    assert!(
        owner_list.contains("\"state\":\"connected\""),
        "{owner_list}"
    );
    buf.iter_mut().for_each(|slot| *slot = 0);
    assert_eq!(
        ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    let joiner_list = text_of(&buf);
    assert!(
        joiner_list.contains("\"state\":\"connected\""),
        "{joiner_list}"
    );
    assert!(joiner_list.contains("\"code\":\""), "{joiner_list}");
    ak_engine_free(engine);
}

#[test]
fn join_unknown_code_fails_the_op() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    // A registry-shaped code nobody ever claimed (8 groups of 4).
    let (ok, json) = join_code(
        engine,
        &runtime_id,
        "AKI1-0000-0000-0000-0000-0000-0000-0000-0000",
    );
    assert!(!ok, "unknown code must fail the op: {json}");
    // The registry never reveals whether a code exists; the joiner's
    // honest wording covers unknown/expired/revoked alike.
    assert!(json.contains("unknown task code"), "{json}");
    // Missing code field: -2 at the call.
    let no_code = CString::new("{}").expect("spec");
    let mut op_id = [0u8; 16];
    assert_eq!(
        ak_task_join(
            engine,
            runtime_id.as_ptr(),
            no_code.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -2
    );
    ak_engine_free(engine);
}

// -- ak_task_list / -3 lossless -----------------------------------------

#[test]
fn task_list_buffer_retry_is_lossless() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    let mut big = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()),
        1
    );
    let full = text_of(&big);
    // Tiny buffer: -3, nothing consumed, the message names the size.
    let mut tiny = cbuf(8);
    assert_eq!(
        ak_task_list(engine, runtime_id.as_ptr(), tiny.as_mut_ptr(), tiny.len()),
        -3
    );
    let detail = last_error_of(engine);
    assert!(detail.contains("buffer too small"), "{detail}");
    // Retry with the full capacity: the SAME JSON.
    big.iter_mut().for_each(|slot| *slot = 0);
    assert_eq!(
        ak_task_list(engine, runtime_id.as_ptr(), big.as_mut_ptr(), big.len()),
        1
    );
    assert_eq!(text_of(&big), full);
    // Unknown runtime: -4.
    assert_eq!(
        ak_task_list(engine, [9u8; 16].as_ptr(), big.as_mut_ptr(), big.len()),
        -4
    );
    ak_engine_free(engine);
}

// -- ak_task_set_save_dir ----------------------------------------------

#[test]
fn set_save_dir_creates_and_reflects_in_list() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    let inbox = dir.path().join("inbox");
    let path = CString::new(inbox.to_string_lossy().replace('\\', "/")).expect("path CString");
    assert_eq!(
        ak_task_set_save_dir(engine, runtime_id.as_ptr(), path.as_ptr()),
        0
    );
    assert!(inbox.is_dir(), "the save dir is created eagerly");
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    let list = text_of(&buf);
    assert!(list.contains("inbox"), "{list}");
    // Empty path: -2. Unknown runtime: -4.
    let empty = CString::new("").expect("empty");
    assert_eq!(
        ak_task_set_save_dir(engine, runtime_id.as_ptr(), empty.as_ptr()),
        -2
    );
    assert_eq!(
        ak_task_set_save_dir(engine, [7u8; 16].as_ptr(), path.as_ptr()),
        -4
    );
    ak_engine_free(engine);
}

// -- ak_task_disconnect -------------------------------------------------

#[test]
fn disconnect_notifies_both_sides() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine, &registry, dir_a.path());
    let joiner = start_task_on(engine, &registry, dir_b.path());
    let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code(engine, &joiner, &code);
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    assert_eq!(
        ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()),
        0
    );
    // Local side: reason local; peer side: reason peer (the
    // same-engine instant cache notify).
    let local = wait_event(engine, &joiner, "disconnected");
    assert!(local.contains("\"reason\":\"local\""), "{local}");
    let peer = wait_event(engine, &owner, "disconnected");
    assert!(peer.contains("\"reason\":\"peer\""), "{peer}");
    // Both lists now report the connection disconnected.
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
    // Idempotent: a second disconnect is a silent AK_OK; an unknown
    // conn id is -4.
    assert_eq!(
        ak_task_disconnect(engine, joiner.as_ptr(), conn.as_ptr()),
        0
    );
    assert_eq!(
        ak_task_disconnect(engine, joiner.as_ptr(), [5u8; 16].as_ptr()),
        -4
    );
    ak_engine_free(engine);
}

// -- ak_task_send + ak_task_consent (real bytes end to end) -------------

/// The full v1 pairing story in one process: receiver-role code on A
/// (real resident receiver bound, consent gate armed), B joins as the
/// sender, B sends a real file, A's user accepts the incoming request,
/// the bytes land in A's save folder, both sides see `done`.
#[test]
fn send_with_accepted_consent_delivers_real_bytes() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine, &registry, dir_a.path());
    let joiner = start_task_on(engine, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()),
        0
    );

    let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
    assert!(ok, "receiver-role create (real bind): {json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code(engine, &joiner, &code);
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);

    // A real payload.
    let payload = b"ak-task-plane e2e payload".repeat(64);
    std::fs::write(dir_b.path().join("e2e.bin"), &payload).expect("source file");
    let paths = CString::new(json_paths(&dir_b.path().join("e2e.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0,
        "send accepted: {}",
        last_error_of(engine)
    );

    // The receiver's consent gate fires: answer it from this thread.
    let request = wait_event(engine, &owner, "incoming_request");
    assert!(request.contains("\"files\":1"), "{request}");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 1), 0);

    // The send op completes with the report.
    let (send_ok, send_json) = wait_op(engine, &joiner, &send_op);
    assert!(send_ok, "send must succeed after consent: {send_json}");
    assert!(send_json.contains("\"op\":\"task_send\""), "{send_json}");
    assert!(send_json.contains("\"files\":1"), "{send_json}");
    // The receiver's done event + the delivered bytes.
    let done = wait_event(engine, &owner, "\"event\":\"done\"");
    assert!(done.contains("\"conn\":\""), "{done}");
    let delivered = find_file(&inbox, "e2e.bin").expect("the file landed in the save dir");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    // batchF G2 (CR-P3-06): the sender's pre-dial probe round
    // reported exactly one `probe_observed` line — the loopback echo
    // responder confirms the head candidate, so the kind is
    // `candidate` (the observed-first dial still lands the transfer).
    // Polled BEFORE the joiner's `done` drain: wait_event discards
    // non-matching queued lines, and this event was queued first.
    let observed = wait_event(engine, &joiner, "\"event\":\"probe_observed\"");
    assert_eq!(
        conn_of(&observed),
        conn_of(&join_json),
        "the line correlates with the connection: {observed}"
    );
    assert!(observed.contains("\"phase\":\"send\""), "{observed}");
    assert!(
        observed.contains("\"kind\":\"candidate\""),
        "the loopback echo responder confirms the head: {observed}"
    );
    assert!(observed.contains("\"observed\":\""), "{observed}");
    // The sender-side done event too.
    let _ = wait_event(engine, &joiner, "\"event\":\"done\"");
    ak_engine_free(engine);
}

/// The decline path: consent(false) refuses the transfer over the wire
/// (DECLINED semantics), the send op fails, NOTHING lands on disk.
#[test]
fn declined_consent_refuses_over_the_wire() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine, &registry, dir_a.path());
    let joiner = start_task_on(engine, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine, owner.as_ptr(), save_path.as_ptr()),
        0
    );
    let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code(engine, &joiner, &code);
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    std::fs::write(dir_b.path().join("no.bin"), b"must not land").unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("no.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0
    );
    let request = wait_event(engine, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 0), 0);
    let (send_ok, send_json) = wait_op(engine, &joiner, &send_op);
    assert!(
        !send_ok,
        "a declined transfer must fail the op: {send_json}"
    );
    assert!(
        find_file(&inbox, "no.bin").is_none(),
        "nothing may land on a decline"
    );
    // A decided request cannot be decided again: -4.
    assert_eq!(ak_task_consent(engine, owner.as_ptr(), job.as_ptr(), 1), -4);
    ak_engine_free(engine);
}

#[test]
fn send_validates_runtime_and_connection() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    let paths = CString::new("{\"paths\":[\"C:/x\"]}").unwrap();
    let mut op_id = [0u8; 16];
    // Unknown connection: -4 with the conn wording.
    assert_eq!(
        ak_task_send(
            engine,
            runtime_id.as_ptr(),
            [3u8; 16].as_ptr(),
            paths.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -4
    );
    assert!(last_error_of(engine).contains("unknown connection"));
    // Unknown runtime: -4 (checked before the connection).
    assert_eq!(
        ak_task_send(
            engine,
            [3u8; 16].as_ptr(),
            [3u8; 16].as_ptr(),
            paths.as_ptr(),
            op_id.as_mut_ptr()
        ),
        -4
    );
    // Empty paths array: accepted runtime + a real connection needed —
    // exercised in send_from_the_receiver_side_fails_the_op instead;
    // here the -2 path for a malformed paths value on an unknown conn
    // stays ordered after the -4s above (validation order: runtime,
    // conn, spec).
    ak_engine_free(engine);
}

/// Only the sender side may send: a receiver-side runtime's send op
/// fails with the honest wording (async failure, not a sync -2).
#[test]
fn send_from_the_receiver_side_fails_the_op() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine, &registry, dir_a.path());
    let joiner = start_task_on(engine, &registry, dir_b.path());
    let (_, ok, json) = create_code(engine, &owner, "{\"spec_version\":1,\"role\":\"receiver\"}");
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code(engine, &joiner, &code);
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    // The OWNER holds the receiver-side conn entry — it arrives with
    // its owner-loop completion, so wait for that first.
    let _ = wait_event(engine, &owner, "\"event\":\"connected\"");
    let paths = CString::new(json_paths(std::path::Path::new("C:/nowhere/none.bin"))).unwrap();
    let mut op_id = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine,
            owner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            op_id.as_mut_ptr()
        ),
        0
    );
    let (send_ok, send_json) = wait_op(engine, &owner, &op_id);
    assert!(!send_ok, "{send_json}");
    assert!(send_json.contains("only the sender side"), "{send_json}");
    ak_engine_free(engine);
}

// -- consent bookkeeping -------------------------------------------------

#[test]
fn consent_unknown_request_is_not_found() {
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task(engine, dir.path());
    assert_eq!(
        ak_task_consent(engine, runtime_id.as_ptr(), [4u8; 16].as_ptr(), 1),
        -4
    );
    assert_eq!(
        ak_task_consent(engine, [4u8; 16].as_ptr(), [4u8; 16].as_ptr(), 1),
        -4
    );
    ak_engine_free(engine);
}

/// Teardown flips `stopped`: the consent registration declines every
/// later offer immediately (the waiter-first teardown of engine drop).
/// A live runtime without a fingerprint-matching connection declines
/// too (P1-04: attribution fails closed, no event surfaces). Built on
/// the safe layer with an OWNED engine value, so no raw pointer is
/// ever dereferenced (this module is `forbid(unsafe_code)`).
#[test]
fn consent_registration_declines_after_teardown_and_for_strangers() {
    let rt = {
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
        let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
        let clone = Arc::clone(&rt);
        drop(engine); // teardown runs here (waiters first, then owners)
        clone
    };
    let info = ConsentInfo {
        job_id: [0u8; 16],
        root_name: "late.bin".to_string(),
        file_count: 1,
        total_bytes: 1,
        sender_fingerprint: Fingerprint::from_bytes([7u8; 32]),
    };
    let started = Instant::now();
    // A stopped runtime: immediate decline, nothing registered.
    assert!(matches!(
        register_consent(
            &rt,
            &[0u8; 16],
            &info,
            std::path::Path::new("D:/unused"),
            PENDING_CONSENT_WINDOW
        ),
        ConsentAnswer::Now(false),
    ));
    // A live runtime without the paired connection of that sender:
    // also an immediate decline (fail closed — the offer belongs to
    // no connection of this code, so the user is never asked).
    let (engine, rt_live, code_id) = {
        let engine = AkEngine::new().expect("engine");
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
        let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
        (engine, rt, [0u8; 16])
    };
    let mut events = 0;
    assert!(matches!(
        register_consent(
            &rt_live,
            &code_id,
            &info,
            std::path::Path::new("D:/unused"),
            PENDING_CONSENT_WINDOW
        ),
        ConsentAnswer::Now(false),
    ));
    if let Ok(queue) = rt_live.events.lock() {
        events = queue.len();
    }
    assert_eq!(events, 1, "only runtime_ready is queued: {events}");
    drop(engine);
    assert!(started.elapsed() < Duration::from_secs(2));
}

// -- engine teardown with an armed receiver ------------------------------

/// A resident receiver parked in accept must not hang the engine drop
/// (the P1-04-style waiter-first teardown; the registry stack's client
/// close is part of the same bounded convergence).
#[test]
fn engine_drop_converges_with_a_parked_receiver() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = start_task_on(engine, &registry, dir.path());
    let (_, ok, json) = create_code(
        engine,
        &runtime_id,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let started = Instant::now();
    ak_engine_free(engine); // would hang if the parked accept leaked
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "engine drop must converge quickly"
    );
}

// -- CR-P2-11: the mirrored candidate/reflexive/observed-order unit
// battery moved to ak-session `pairing::task_net::tests` (one shared
// set now); the STUN product-path tests below stay here — they drive
// the real C-ABI surface.

// -- CR-P1-02 改法第 2 条: the STUN reflexive leg, product path ---------

/// The receiver's STUN query runs ON THE SAME SOCKET QUIC THEN OWNS,
/// observable as port equality: a truthful loopback responder reflects
/// the OBSERVED source, so the event's `reflexive` port must equal the
/// `addr` (bound) port — a throwaway query socket would reflect a
/// different ephemeral port and fail this. The reflexive loopback
/// dedups against the derived 127.0.0.1 candidate (same address), so
/// the advertisement carries it exactly once (the CLI itest pins the
/// wire-side of that).
#[test]
fn stun_reflexive_rides_the_receiver_socket() {
    let registry = TestRegistry::spawn();
    let stun = TestStun::spawn(None); // truthful echo of the source
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"registry\":\"{}\",\
         \"registry_id\":\"{}\",\"id_dir\":\"{}\",\"stun_addr\":\"{}\"}}",
        registry.addr,
        registry.id_hex(),
        dir.path().to_string_lossy().replace('\\', "/"),
        stun.addr,
    ))
    .expect("spec CString");
    let mut runtime_id = [0u8; 16];
    assert_eq!(
        ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()),
        0,
        "{}",
        last_error_of(engine)
    );
    let (_, ok, json) = create_code(
        engine,
        &runtime_id,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "the receiver code must claim: {json}");
    let event = wait_event(engine, &runtime_id, "receiver_bind");
    // Success shape: a reflexive address, no failure reason.
    let reflexive = event
        .split("\"reflexive\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string();
    assert!(
        !reflexive.is_empty(),
        "the reflexive address must ride the event: {event}"
    );
    assert!(
        event.contains("\"stun_error\":null"),
        "a successful query reports no error: {event}"
    );
    // THE same-socket proof: the reflected port equals the bound
    // port (the responder truthfully echoed the socket's own source).
    let reflexive_addr: SocketAddr = reflexive.parse().expect("reflexive ip:port");
    let bound = event
        .split("\"addr\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|text| text.parse::<SocketAddr>().ok())
        .expect("bound addr in the event");
    assert_eq!(
        reflexive_addr.port(),
        bound.port(),
        "the STUN query must run on the socket QUIC adopts: {event}"
    );
    assert_eq!(
        reflexive_addr.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "the truthful loopback responder reflects the local source: {event}"
    );
    ak_engine_free(engine);
}

/// Fail-open (改法第 2 条): a configured-but-silent STUN server costs at
/// most STUN_QUERY_BUDGET, the receiver still binds, the code still
/// claims, and the bounded failure rides the `receiver_bind` event
/// with NO reflexive candidate.
#[test]
fn stun_failure_fails_open_within_the_budget() {
    let registry = TestRegistry::spawn();
    // A bound-but-silent socket: packets land, nothing answers — the
    // deterministic timeout path on every OS (a dead port would
    // fast-fail via ICMP/ConnectionReset on Windows only).
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("silent STUN bind");
    let silent_addr = silent.local_addr().expect("silent STUN addr");
    let engine = ak_engine_new();
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"registry\":\"{}\",\
         \"registry_id\":\"{}\",\"id_dir\":\"{}\",\"stun_addr\":\"{}\"}}",
        registry.addr,
        registry.id_hex(),
        dir.path().to_string_lossy().replace('\\', "/"),
        silent_addr,
    ))
    .expect("spec CString");
    let mut runtime_id = [0u8; 16];
    assert_eq!(
        ak_task_start(engine, spec.as_ptr(), runtime_id.as_mut_ptr()),
        0,
        "{}",
        last_error_of(engine)
    );
    let started = Instant::now();
    let (_, ok, json) = create_code(
        engine,
        &runtime_id,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    let elapsed = started.elapsed();
    assert!(ok, "a dead STUN server must not fail the code: {json}");
    // The bounded budget: the 2 s query + the registry claim must stay
    // far inside the 10 s receiver-bind wait (which would have failed
    // the op outright at its deadline).
    assert!(
        elapsed < Duration::from_secs(8),
        "fail-open must be prompt (STUN_QUERY_BUDGET is 2 s): {elapsed:?}"
    );
    let event = wait_event(engine, &runtime_id, "receiver_bind");
    assert!(
        event.contains("\"reflexive\":null"),
        "no reflexive candidate on failure: {event}"
    );
    assert!(
        event.contains("\"addr\":\""),
        "the receiver still bound: {event}"
    );
    assert!(
        !event.contains("\"stun_error\":null"),
        "the bounded failure reason rides the event: {event}"
    );
    ak_engine_free(engine);
}

// -- the registry relay (the three formerly-#[ignore] contracts) ---------

/// Codes relay ACROSS ENGINES through the real registry: engine A
/// claims its code (endpoint blob, max_redeems = 1) and engine B — a
/// completely separate engine handle, task runtime and registry
/// client — joins it there. The join drives the frozen pairing
/// transcript to `PairReady`; the owner's side completes through its
/// resident owner loop (`connected` on A, too). This replaces the
/// engine-local-era positive test that asserted the cross-engine
/// join FAILS (`unknown task code`) — with the relay it is the
/// success path.
#[test]
fn task_codes_relay_across_engines_via_the_registry() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let (_, ok, json) = create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok, "the owner claims at the real registry: {json}");
    let code = code_of(&json);
    // Cross-engine: the engine-local cache cannot help — the join
    // must succeed through the registry relay alone.
    let (join_ok, join_json) = join_code(engine_b, &joiner, &code);
    assert!(
        join_ok,
        "the registry relay must carry the code across engines: {join_json}"
    );
    assert!(join_json.contains("\"op\":\"task_join\""), "{join_json}");
    assert!(join_json.contains("\"role\":\"receiver\""), "{join_json}");
    // The owner learns the pairing through its owner loop: the same
    // connection id (the pairing transaction id) and its own role.
    let on_owner = wait_event(engine_a, &owner, "\"event\":\"connected\"");
    assert!(on_owner.contains("\"role\":\"sender\""), "{on_owner}");
    assert_eq!(conn_of(&on_owner), conn_of(&join_json), "shared conn id");
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// The probe reports REAL reachability: with derived candidates each
/// side runs a probe echo responder on an advertised port, so the
/// joiner's probe over the owner's candidates confirms a live path
/// (`reachable:true` with the answering address, `probe:"verified"`);
/// with EXPLICIT caller-chosen candidates (verbatim advertisement, no
/// echo port) a dead address reports every candidate failed
/// (`reachable:false`, one reason per candidate, `probe:"all_failed"`).
#[test]
fn probe_result_reports_real_reachability() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());

    // Live leg: derived candidates (real receiver/echo ports).
    let (_, ok, json) = create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok, "{json}");
    let live_code = code_of(&json);
    let (live_ok, live_json) = join_code(engine_b, &joiner, &live_code);
    assert!(live_ok, "{live_json}");
    assert!(
        live_json.contains("\"probe\":\"verified\""),
        "the echo responder answers the real probe: {live_json}"
    );
    let probe = wait_event(engine_b, &joiner, "probe_result");
    assert!(probe.contains("\"reachable\":true"), "{probe}");
    assert!(
        probe.contains("\"endpoint\":\""),
        "the confirmed address rides the event: {probe}"
    );

    // Dead leg: the caller advertises exactly one closed port.
    let (_, ok_two, json_two) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"sender\",\"candidates\":[\"127.0.0.1:1\"]}",
    );
    assert!(ok_two, "{json_two}");
    let dead_code = code_of(&json_two);
    let (dead_ok, dead_json) = join_code(engine_b, &joiner, &dead_code);
    assert!(dead_ok, "the pairing itself stands: {dead_json}");
    assert!(
        dead_json.contains("\"probe\":\"all_failed\""),
        "a silent candidate reports every candidate failed: {dead_json}"
    );
    let dead_probe = wait_event(engine_b, &joiner, "\"reachable\":false");
    assert!(
        dead_probe.contains("\"candidate\":\"127.0.0.1:1\""),
        "{dead_probe}"
    );
    assert!(dead_probe.contains("\"reason\":"), "{dead_probe}");
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// Unit shape of the `probe_observed` line (batchF G2): the three
/// `kind` verdicts of `observed_dial_head`'s rules and the counted
/// fields — pure formatting, no sockets.
#[test]
fn probe_observed_event_renders_the_three_kinds() {
    let conn_id = [7u8; 16];
    let candidate: SocketAddr = "192.168.1.9:52000".parse().expect("candidate");
    let outside: SocketAddr = "203.0.113.4:9".parse().expect("outside source");
    let mut report = ak_net::holepunch::PunchReport {
        reachable: vec![candidate],
        peer_observed_from: Some(outside),
        packets_sent: 3,
        packets_received: 2,
        elapsed: Duration::from_millis(1200),
    };
    let candidate_line = probe_observed_event(&conn_id, Some(candidate), &report);
    assert!(
        candidate_line.contains("\"kind\":\"candidate\""),
        "{candidate_line}"
    );
    assert!(
        candidate_line.contains("\"observed\":\"192.168.1.9:52000\""),
        "{candidate_line}"
    );
    assert!(
        candidate_line.contains("\"phase\":\"send\""),
        "{candidate_line}"
    );
    assert!(
        candidate_line.contains("\"reachable\":1"),
        "{candidate_line}"
    );
    assert!(
        candidate_line.contains("\"packets_in\":2"),
        "{candidate_line}"
    );
    assert!(
        candidate_line.contains("\"elapsed_ms\":1200"),
        "{candidate_line}"
    );

    // Rule 2 head (a peer-probe source outside the candidate list).
    let observed_line = probe_observed_event(&conn_id, Some(outside), &report);
    assert!(
        observed_line.contains("\"kind\":\"observed\""),
        "{observed_line}"
    );
    assert!(
        observed_line.contains("\"observed\":\"203.0.113.4:9\""),
        "{observed_line}"
    );

    // Nothing observed: still exactly one bounded line, `null` +
    // `none` (the fail-open shape).
    report.reachable.clear();
    report.peer_observed_from = None;
    report.packets_received = 0;
    let none_line = probe_observed_event(&conn_id, None, &report);
    assert!(none_line.contains("\"observed\":null"), "{none_line}");
    assert!(none_line.contains("\"kind\":\"none\""), "{none_line}");
}

/// A remote disconnect reaches the peer's runtime: engine A owns a
/// receiver-role code (real resident receiver, consent gate armed),
/// engine B joins as the sender and starts a real send; A's user
/// ACCEPTS the offer and B disconnects the moment the transfer
/// begins. B's abort drops its QUIC endpoint (the CONNECTION_CLOSE
/// reaches A mid-transfer), A's receive fails with the peer gone,
/// ak-session reports the task `ByPeer`, and the event pump surfaces
/// `disconnected`/`peer` on A's poll channel (the cross-engine path —
/// the engine-local cache cannot carry it).
#[test]
fn remote_disconnect_reaches_the_peer_device() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    // Cross-engine: the joiner's side is the mirror of the owner's
    // receiver declaration — declared explicitly on the join spec.
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    assert!(join_json.contains("\"role\":\"sender\""), "{join_json}");
    let conn = conn_of(&join_json);
    // A's connection entry arrives with its owner-loop completion.
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

    // A real, sizeable send: the offer parks in A's consent gate.
    let payload = b"remote-disconnect probe payload".repeat(4096);
    std::fs::write(dir_b.path().join("bye.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("bye.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    // A accepts; the transfer begins; B disconnects immediately after
    // (the mid-transfer data-plane departure is what the peer can
    // actually observe — a mid-OFFER close is swallowed by the consent
    // gate's own 120 s window, which declines rather than erroring).
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    assert_eq!(
        ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
        0
    );
    let local = wait_event(engine_b, &joiner, "disconnected");
    assert!(local.contains("\"reason\":\"local\""), "{local}");

    // A's data plane sees the peer go: ByPeer -> the pump -> A's poll.
    let remote = wait_event(engine_a, &owner, "disconnected");
    assert!(
        remote.contains("\"reason\":\"peer\""),
        "the peer device must learn the disconnect: {remote}"
    );
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

// -- wave-29 review findings (P1-04, P2-01..07, P2-12) ------------------

/// Poll `ak_pairing_poll_event` for `ms` and FAIL if any event whose
/// body contains `needle` arrives (the absence assertions of the
/// stranger-dial tests: a refused offer must never reach the user).
fn assert_no_event_within(engine: *mut AkEngine, runtime_id: &[u8; 16], needle: &str, ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        let mut buf = cbuf(CAP);
        let code = ak_pairing_poll_event(engine, runtime_id.as_ptr(), buf.as_mut_ptr(), buf.len());
        if code == 1 {
            let line = text_of(&buf);
            assert!(
                !line.contains(needle),
                "unexpected event {needle:?}: {line}"
            );
        } else {
            assert!(code == 0, "poll_event failed: {code}");
        }
        std::thread::sleep(POLL);
    }
}

/// The first advertised endpoint of a create result (the receiver's
/// real port for a receiver-role code).
fn endpoint_of(json: &str) -> SocketAddr {
    json.split("\"endpoint\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("endpoint in result")
        .parse()
        .expect("endpoint is ip:port")
}

fn code_id_of(json: &str) -> [u8; 16] {
    let hex = json
        .split("\"code_id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("code id in result");
    hex16_id(hex)
}

/// A REAL engine send from a STRANGER device (wave-29 P1-04): a fresh
/// device key that knows both the target endpoint and the owner's
/// fingerprint (the worst case — the TLS handshake completes), yet is
/// not the paired peer of the task. Returns the failure text; a
/// stranger transfer that SUCCEEDS fails the test.
fn stranger_send_fails(
    endpoint: SocketAddr,
    expect_peer: &Fingerprint,
    payload: &std::path::Path,
) -> String {
    let key = DeviceKeyPair::generate();
    let mut spec = SendSpec::new(
        payload.to_path_buf(),
        endpoint,
        *expect_peer,
        make_self_signed(&key),
    );
    spec.bind = if endpoint.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    spec.idle_timeout = Duration::from_secs(5);
    let stranger = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("stranger runtime");
    let attempt = stranger.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), send(spec, &|_| {})).await
    });
    match attempt {
        Ok(Ok(report)) => panic!("the stranger's transfer succeeded: {report:?}"),
        Ok(Err(error)) => format!("{error}"),
        Err(_) => "stranger send timed out".to_string(),
    }
}

/// A stranger dial whose TLS handshake FAILS MID-WAY (wave-30 P1-04
/// residual): the dialer pins a fingerprint the listener does NOT
/// hold, so the client aborts right after the server certificate
/// arrives — the SERVER side of the accept then reports a
/// Transport/PeerIdentity failure, the per-connection error class the
/// accept loop must survive. Returns the failure text; a transfer
/// that SUCCEEDS fails the test.
fn wrong_pin_send_fails(endpoint: SocketAddr, payload: &std::path::Path) -> String {
    let key = DeviceKeyPair::generate();
    let wrong_pin = key.fingerprint(); // nobody listening holds this key
    let mut spec = SendSpec::new(
        payload.to_path_buf(),
        endpoint,
        wrong_pin,
        make_self_signed(&key),
    );
    spec.bind = if endpoint.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    spec.idle_timeout = Duration::from_secs(5);
    let stranger = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("stranger runtime");
    let attempt = stranger.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), send(spec, &|_| {})).await
    });
    match attempt {
        Ok(Ok(report)) => panic!("a wrong-pin transfer must not succeed: {report:?}"),
        Ok(Err(error)) => format!("{error}"),
        Err(_) => "wrong-pin send timed out".to_string(),
    }
}

/// Safe-layer op wait for the OWNED-engine tests (no raw pointers).
fn wait_op_owned(engine: &AkEngine, runtime_id: &[u8; 16], op_id: &[u8; 16]) -> (bool, String) {
    let deadline = Instant::now() + WAIT;
    loop {
        let snapshot =
            crate::task::task_op_result(engine, runtime_id, op_id).expect("task op read");
        if let OpResult::Finished { ok, json } = snapshot {
            return (ok, json);
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the owned op"
        );
        std::thread::sleep(POLL);
    }
}

/// Fire one safe-layer `task_create_code_start` from a spawned thread
/// (the concurrency tests; the raw C handle is not `Send`, and the
/// atomicity under test lives in the safe layer's gate anyway).
fn threaded_create(
    engine: Arc<AkEngine>,
    runtime_id: [u8; 16],
    spec: String,
) -> mpsc::Receiver<Result<[u8; 16], FfiError>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = task_create_code_start(&engine, &runtime_id, &spec);
        tx.send(result).expect("test channel");
    });
    rx
}

/// P1-04: the receiver's admission is bound to the completed pairing's
/// verified fingerprint. Before B joins, a stranger's dial (C — a
/// fresh device key that knows A's endpoint AND fingerprint, so its
/// TLS handshake completes) is refused at the admission gate: no
/// `incoming_request`, zero disk writes, and the listener survives.
/// After B joins, the SAME stranger is refused again (fingerprint
/// mismatch now), and B's legitimate send still succeeds on the very
/// same listener — a stranger never consumes the receiver.
#[test]
fn stranger_dials_never_reach_the_user_or_consume_the_receiver() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );

    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "receiver-role create (real bind): {json}");
    let code = code_of(&json);
    let endpoint = endpoint_of(&json);
    // The stranger "knows" A's fingerprint: A's device key is on disk.
    let (owner_key, _) =
        DeviceKeyPair::load_or_create(&dir_a.path().join("device.key")).expect("A key");
    let owner_fp = owner_key.fingerprint();
    std::fs::write(dir_a.path().join("stranger.bin"), b"stranger payload").unwrap();

    // 1) Pre-pairing: C's completed-TLS dial is refused — no consent
    //    prompt, nothing on disk, receiver not consumed.
    let failure = stranger_send_fails(endpoint, &owner_fp, &dir_a.path().join("stranger.bin"));
    assert!(
        !failure.is_empty(),
        "the pre-pairing stranger dial must fail: {failure}"
    );
    assert_no_event_within(engine_a, &owner, "incoming_request", 1500);
    assert!(find_file(&inbox, "stranger.bin").is_none());

    // 2) B joins as the sender; A's connection entry arrives with the
    //    owner-loop completion (synchronous bookkeeping).
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "the legitimate join must succeed: {join_json}");
    let conn = conn_of(&join_json);
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

    // 3) Post-pairing: the same stranger is refused AGAIN — its
    //    fingerprint is not the pairing's verified peer.
    let failure_two = stranger_send_fails(endpoint, &owner_fp, &dir_a.path().join("stranger.bin"));
    assert!(
        !failure_two.is_empty(),
        "the post-pairing stranger dial must fail: {failure_two}"
    );
    assert_no_event_within(engine_a, &owner, "incoming_request", 1500);
    assert!(
        find_file(&inbox, "stranger.bin").is_none(),
        "a stranger's offer writes nothing to disk"
    );

    // 4) B's legitimate send on the SAME listener succeeds, and the
    //    prompt is attributed to B's connection.
    let payload = b"legitimate bytes after stranger attempts".repeat(32);
    std::fs::write(dir_b.path().join("legit.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("legit.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0,
        "B's send accepted: {}",
        last_error_of(engine_b)
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    assert!(
        request.contains(&format!("\"conn\":\"{}\"", core::hex_encode(&conn))),
        "the prompt is attributed to the fingerprint-matched conn: {request}"
    );
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
    assert!(
        send_ok,
        "B still succeeds after the stranger attempts: {send_json}"
    );
    let delivered = find_file(&inbox, "legit.bin").expect("B's file landed");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-01: the request gate is one atomic acceptance — two threads
/// racing the SAME request get the SAME op id (exactly one code is
/// claimed), and the same `request_id` under a DIFFERENT operation is
/// a `-2` conflict, never a cross-operation replay hit.
#[test]
fn request_id_acceptance_is_atomic_and_domain_scoped() {
    let registry = TestRegistry::spawn();
    let engine = Arc::new(AkEngine::new().expect("engine"));
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
    let request_id = "b1b2c3d4e5f60718293a4b5c6d7e8f90";
    let spec =
        format!("{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{request_id}\"}}");
    let racer_one = threaded_create(Arc::clone(&engine), runtime_id, spec.clone());
    let racer_two = threaded_create(Arc::clone(&engine), runtime_id, spec.clone());
    let op_one = racer_one
        .recv()
        .expect("racer one answered")
        .expect("accepted");
    let op_two = racer_two
        .recv()
        .expect("racer two answered")
        .expect("accepted");
    assert_eq!(
        op_one, op_two,
        "both racers must observe the ONE atomically accepted op"
    );
    let (ok, json) = wait_op_owned(&engine, &runtime_id, &op_one);
    assert!(ok, "{json}");
    assert!(
        json.contains("\"code\":\"AKI1-"),
        "exactly one claim: {json}"
    );

    // Cross-operation domain: the same request id under task_join is
    // a conflict (-2), not a replay of the create's op.
    let join_spec = format!(
        "{{\"spec_version\":1,\"code\":\"AKI1-0000-0000-0000-0000-0000-0000-0000-0000\",\
          \"request_id\":\"{request_id}\"}}"
    );
    let error = task_join_start(&engine, &runtime_id, &join_spec).expect_err("conflict");
    assert_eq!(error.code, crate::core::AK_ERR_INVALID);
    assert!(
        error.message.contains("different spec"),
        "{}",
        error.message
    );
}

/// P2-01 (second half): an ACCEPTED send request's answer stays
/// retrievable after the connection later disconnected — the replay
/// returns the ORIGINAL op id instead of a state rejection.
#[test]
fn accepted_send_request_replays_its_answer_after_disconnect() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);

    let payload = b"replayable request".repeat(16);
    std::fs::write(dir_b.path().join("re.bin"), &payload).unwrap();
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"paths\":[\"{}\"],\
          \"request_id\":\"c1c2c3d4e5f60718293a4b5c6d7e8f90\"}}",
        dir_b
            .path()
            .join("re.bin")
            .to_string_lossy()
            .replace('\\', "/"),
    ))
    .unwrap();
    let mut op_one = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            spec.as_ptr(),
            op_one.as_mut_ptr()
        ),
        0
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &op_one);
    assert!(send_ok, "{send_json}");

    // The connection goes away; the ORIGINAL request still answers.
    assert_eq!(
        ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
        0
    );
    let mut op_two = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            spec.as_ptr(),
            op_two.as_mut_ptr()
        ),
        0,
        "the replay is answered, not state-rejected: {}",
        last_error_of(engine_b)
    );
    assert_eq!(op_one, op_two, "the replay returns the ORIGINAL op");
    let (replay_ok, replay_json) = wait_op(engine_b, &joiner, &op_two);
    assert!(replay_ok, "{replay_json}");
    assert_eq!(replay_json, send_json, "the original answer, verbatim");
    // A FRESH request on the severed connection is still refused.
    let fresh = CString::new(json_paths(&dir_b.path().join("re.bin"))).unwrap();
    let mut op_three = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            fresh.as_ptr(),
            op_three.as_mut_ptr()
        ),
        -2
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-02: two concurrent FIRST ops on one runtime initialize the
/// registry stack exactly once — both succeed and the surviving stack
/// keeps serving later ops (no stranded, replaced control client).
#[test]
fn concurrent_first_ops_initialize_the_stack_once_and_it_stays_usable() {
    let registry = TestRegistry::spawn();
    let engine = Arc::new(AkEngine::new().expect("engine"));
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let runtime_id = task_start(&engine, &registry_spec(&registry, dir_a.path())).expect("rt");
    let spec_one = format!(
        "{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{:032x}\"}}",
        0u32
    );
    let spec_two = format!(
        "{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{:032x}\"}}",
        1u32
    );
    let racer_one = threaded_create(Arc::clone(&engine), runtime_id, spec_one);
    let racer_two = threaded_create(Arc::clone(&engine), runtime_id, spec_two);
    let op_one = racer_one
        .recv()
        .expect("racer one answered")
        .expect("accepted");
    let op_two = racer_two
        .recv()
        .expect("racer two answered")
        .expect("accepted");
    assert_ne!(op_one, op_two, "distinct requests, distinct ops");
    let mut codes = Vec::new();
    for op in [&op_one, &op_two] {
        let (ok, json) = wait_op_owned(&engine, &runtime_id, op);
        assert!(ok, "both concurrent first ops must succeed: {json}");
        codes.push(code_of(&json));
    }
    // The stack the race left behind still serves real work: a second
    // runtime on the same engine joins one of the codes through it.
    let joiner = task_start(&engine, &registry_spec(&registry, dir_b.path())).expect("rt2");
    let join_op = task_join_start(
        &engine,
        &joiner,
        &format!("{{\"spec_version\":1,\"code\":\"{}\"}}", codes[0]),
    )
    .expect("join accepted");
    let (join_ok, join_json) = wait_op_owned(&engine, &joiner, &join_op);
    assert!(join_ok, "the surviving stack is usable: {join_json}");
}

/// P2-03: an IDLE disconnect (no transfer whose QUIC drop the peer
/// would observe) still reaches the paired peer: B's disconnect dials
/// A's advertised receiver port and hands it the TASK-CLOSE frame; A
/// ends the pairing ByPeer (`disconnected`/`peer`).
#[test]
fn idle_disconnect_reaches_the_paired_peer() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    // A's connection entry arrives with its owner-loop completion.
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");
    // IDLE: no send ever happens. B walks away.
    assert_eq!(
        ak_task_disconnect(engine_b, joiner.as_ptr(), conn.as_ptr()),
        0
    );
    let remote = wait_event(engine_a, &owner, "disconnected");
    assert!(
        remote.contains("\"reason\":\"peer\""),
        "the idle peer must learn the disconnect: {remote}"
    );
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(text_of(&buf).contains("\"state\":\"disconnected\""));
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-04: a generated-but-never-joined code is cancellable through
/// `ak_task_disconnect` by its `code_id`: the registry row revokes
/// (a later join answers "unknown task code"), the receiver/echo
/// listeners are released, `code_cancelled` is emitted, and repeats
/// are idempotent. Runs on an OWNED engine (safe-layer calls only).
#[test]
fn unjoined_code_is_cancellable_through_disconnect() {
    let registry = TestRegistry::spawn();
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = registry_spec(&registry, dir.path());
    let runtime = task_start(&engine, &spec).expect("runtime");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let op = task_create_code_start(
        &engine,
        &runtime,
        "{\"spec_version\":1,\"role\":\"receiver\",\
          \"request_id\":\"d1d2d3d4e5f60718293a4b5c6d7e8f90\"}",
    )
    .expect("create accepted");
    let (ok, json) = wait_op_owned(&engine, &runtime, &op);
    assert!(ok, "{json}");
    let code_id = code_id_of(&json);
    let code = code_of(&json);
    assert!(
        rt.spawned
            .lock()
            .map(|m| m.contains_key(&code_id))
            .unwrap_or(false),
        "the receiver/echo listeners are registered under the code id"
    );

    // Cancel by code id (the FFI disconnect covers the un-joined state).
    task_disconnect(&engine, &runtime, &code_id).expect("cancel");
    let events: Vec<String> = rt
        .events
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    assert!(
        events
            .iter()
            .any(|line| line.contains("code_cancelled")
                && line.contains(&core::hex_encode(&code_id))),
        "the cancel is observable: {events:?}"
    );
    assert!(
        !rt.spawned
            .lock()
            .map(|m| m.contains_key(&code_id))
            .unwrap_or(true),
        "the listeners are released"
    );
    // Idempotent repeat: a silent AK_OK.
    task_disconnect(&engine, &runtime, &code_id).expect("idempotent cancel");

    // The registry row is gone: a later join answers "unknown task code".
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let spec_b = registry_spec(&registry, dir_b.path());
    let runtime_b = task_start(&engine, &spec_b).expect("second runtime");
    let join_op = task_join_start(
        &engine,
        &runtime_b,
        &format!("{{\"spec_version\":1,\"code\":\"{code}\"}}"),
    )
    .expect("join accepted");
    let (join_ok, join_json) = wait_op_owned(&engine, &runtime_b, &join_op);
    assert!(!join_ok, "a cancelled code cannot be joined: {join_json}");
    assert!(join_json.contains("unknown task code"), "{join_json}");

    // An id that is neither a connection nor one of this runtime's
    // codes stays the frozen -4.
    let error = task_disconnect(&engine, &runtime, &[9u8; 16]).expect_err("unknown id");
    assert_eq!(error.code, crate::core::AK_ERR_NOT_FOUND);
}

/// P2-05: a DECLINED offer keeps the listener alive (the sender may
/// retry on the same code), and a COMPLETED transfer converges BOTH
/// sides' entries to the terminal `disconnected` — a follow-up send
/// on the dead connection is refused up front.
#[test]
fn declined_offer_keeps_the_listener_and_done_converges_both_sides() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

    // Attempt 1: declined.
    let payload = b"retry me".repeat(64);
    std::fs::write(dir_b.path().join("retry.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("retry.bin"))).unwrap();
    let mut op_one = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            op_one.as_mut_ptr()
        ),
        0
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("job"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 0),
        0
    );
    let (declined_ok, _) = wait_op(engine_b, &joiner, &op_one);
    assert!(!declined_ok, "the declined attempt fails the op");
    // The listener (and the connection) survived the decline. Poll
    // the owner's list until its pairing-time probe SETTLES first:
    // since CR-P1-07 the joiner's join returns after its (faster,
    // early-exit) adopted-socket round, so the owner's 1.5 s
    // probe-with-retry can still be mid-window here — `probing` is a
    // transient state, never a verdict.
    let deadline = Instant::now() + WAIT;
    let settled = loop {
        let mut buf = cbuf(CAP);
        assert_eq!(
            ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
            1
        );
        let text = text_of(&buf);
        if !text.contains("\"state\":\"probing\"") {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the owner's pairing-time probe never settled: {text}"
        );
        std::thread::sleep(POLL);
    };
    assert!(
        settled.contains("\"state\":\"connected\""),
        "a decline is not a task terminal state: {settled}"
    );

    // Attempt 2 (a fresh op on the SAME connection): accepted, lands.
    let mut op_two = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            op_two.as_mut_ptr()
        ),
        0,
        "the retry is accepted: {}",
        last_error_of(engine_b)
    );
    let request_two = wait_event(engine_a, &owner, "incoming_request");
    let job_two = hex16_id(
        request_two
            .split("\"job\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("job"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job_two.as_ptr(), 1),
        0
    );
    let (retry_ok, retry_json) = wait_op(engine_b, &joiner, &op_two);
    assert!(retry_ok, "the retry after a decline succeeds: {retry_json}");
    let delivered = find_file(&inbox, "retry.bin").expect("the retry landed");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);

    // Done is the single-transfer terminal state on BOTH sides.
    let _ = wait_event(engine_a, &owner, "\"event\":\"done\"");
    // Wave-30 P2-05 residual: the converge line's reason IS `done` —
    // a decidable terminal state for the Dart table to agree on.
    let done_line = wait_event(engine_a, &owner, "\"reason\":\"done\"");
    assert!(
        done_line.contains("\"event\":\"disconnected\""),
        "{done_line}"
    );
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        text_of(&buf).contains("\"state\":\"disconnected\""),
        "the receiver converges: {}",
        text_of(&buf)
    );
    buf.iter_mut().for_each(|slot| *slot = 0);
    assert_eq!(
        ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        text_of(&buf).contains("\"state\":\"disconnected\""),
        "the sender converges: {}",
        text_of(&buf)
    );
    // A third send on the terminal connection is refused up front.
    let mut op_three = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            op_three.as_mut_ptr()
        ),
        -2
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

fn synthetic_sender(engine: &AkEngine) -> (tempfile::TempDir, Arc<TaskRt>, TaskConn) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = task_start(engine, &spec_for(dir.path())).unwrap();
    let rt = task_runtime_of(engine, &runtime).unwrap();
    let conn = TaskConn {
        conn_id: [71; 16],
        code_id: [72; 16],
        role: TaskRole::Sender,
        state: ConnState::Connected,
        candidates: vec!["127.0.0.1:9".parse().unwrap()],
        peer_endpoint: None,
        peer_fp: None,
        code: "synthetic".to_string(),
    };
    rt.conns.lock().unwrap().insert(conn.conn_id, conn.clone());
    ensure_sender_lifetime(&rt, &conn);
    (dir, rt, conn)
}

/// Uses the same admission + lease settlement as the production
/// launcher, but barriers replace the engine I/O to force ordering.
fn synthetic_send(rt: &Arc<TaskRt>, conn: &TaskConn, id: [u8; 16]) -> SendLease {
    let mut lives = rt.sender_lifetimes.lock().unwrap();
    let entry = Arc::new(OpEntry {
        outcome: StdMutex::new(OpOutcome::Running { op: "task_send" }),
        task: StdMutex::new(None),
    });
    rt.ops.lock().unwrap().insert(id, Arc::clone(&entry));
    lives.get_mut(&conn.conn_id).unwrap().admit(id);
    SendLease {
        rt: Arc::downgrade(rt),
        conn_id: conn.conn_id,
        op_id: id,
        entry,
        settled: false,
    }
}

/// Catches the old B-counts-two/A-has-old-epoch dead end. Exactly the
/// last Finished publication restores idle, in either completion order.
#[test]
fn reverse_send_failures_restore_idle_holding() {
    let engine = AkEngine::new().unwrap();
    let (_dir, rt, conn) = synthetic_sender(&engine);
    for order in [[1u8, 0], [0, 1]] {
        let mut first = synthetic_send(&rt, &conn, [73; 16]);
        let mut second = synthetic_send(&rt, &conn, [74; 16]);
        let deadline = rt.sender_lifetimes.lock().unwrap()[&conn.conn_id].deadline;
        let mut state = rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
            .state
            .subscribe();
        state.borrow_and_update();
        engine.runtime().block_on(async {
            let (a_tx, a_rx) = oneshot::channel();
            let (b_tx, b_rx) = oneshot::channel();
            let a = tokio::spawn(async move {
                a_rx.await.unwrap();
                first.finish(Some((false, "A failed".into())));
            });
            let b = tokio::spawn(async move {
                b_rx.await.unwrap();
                second.finish(Some((false, "B failed".into())));
            });
            if order[0] == 1 {
                b_tx.send(()).unwrap();
                b.await.unwrap();
                assert!(
                    !state.has_changed().unwrap(),
                    "not the last send: keeper remains paused"
                );
                assert_eq!(
                    rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
                        .active
                        .len(),
                    1
                );
                a_tx.send(()).unwrap();
                a.await.unwrap();
            } else {
                a_tx.send(()).unwrap();
                a.await.unwrap();
                assert!(!state.has_changed().unwrap());
                b_tx.send(()).unwrap();
                b.await.unwrap();
            }
        });
        assert!(state.has_changed().unwrap());
        assert_eq!(*state.borrow_and_update(), SenderKeepaliveState::Idle);
        assert!(
            !state.has_changed().unwrap(),
            "one idle transition, no extra restart"
        );
        let lives = rt.sender_lifetimes.lock().unwrap();
        assert!(lives[&conn.conn_id].active.is_empty());
        assert_eq!(
            lives[&conn.conn_id].deadline, deadline,
            "budget never resets"
        );
        for id in [[73; 16], [74; 16]] {
            assert!(matches!(
                *rt.ops.lock().unwrap()[&id].outcome.lock().unwrap(),
                OpOutcome::Finished { .. }
            ));
        }
    }
    rt.teardown(engine.runtime());
}

/// The timer survives zero targets/no socket. An admitted send can
/// span the deadline; only its final failed settlement ends pairing.
#[test]
fn resource_free_pairing_deadline_and_active_deferral() {
    let engine = AkEngine::new().unwrap();
    let (_dir, rt, conn) = synthetic_sender(&engine);
    let timer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    for variant in 0..3 {
        for busy in [false, true] {
            rt.conns
                .lock()
                .unwrap()
                .get_mut(&conn.conn_id)
                .unwrap()
                .state = ConnState::Connected;
            rt.events.lock().unwrap().clear();
            timer.block_on(async {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                rt.sender_lifetimes
                    .lock()
                    .unwrap()
                    .insert(conn.conn_id, SenderLifetime::new(deadline));
                let mut lease = busy.then(|| synthetic_send(&rt, &conn, [75; 16]));
                let state = rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
                    .state
                    .subscribe();
                let socket =
                    (variant != 0).then(|| std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
                let candidates = match variant {
                    0 => vec![],
                    1 => vec!["127.0.0.1:9".parse().unwrap()],
                    _ => vec!["[2001:db8::1]:9".parse().unwrap()],
                };
                let weak = Arc::downgrade(&rt);
                let conn_id = conn.conn_id;
                let keeper = tokio::spawn(async move {
                    sender_pairing_keeper(
                        socket.as_ref(),
                        &candidates,
                        deadline,
                        state,
                        |_| {},
                        || {
                            expire_idle_sender(&weak.upgrade().unwrap(), &conn_id);
                        },
                    )
                    .await
                });
                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_secs(3)).await;
                tokio::task::yield_now().await;
                if let Some(lease) = &mut lease {
                    assert_eq!(
                        rt.conns.lock().unwrap()[&conn.conn_id].state,
                        ConnState::Connected
                    );
                    assert!(
                        !keeper.is_finished(),
                        "active at deadline: no terminal verdict"
                    );
                    assert!(!rt
                        .events
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|line| line.contains("idle_expired")));
                    lease.finish(Some((false, "failed after deadline".into())));
                }
                let report = keeper.await.unwrap();
                assert_eq!(report.keepalive_packets, 0);
                assert_eq!(
                    rt.conns.lock().unwrap()[&conn.conn_id].state,
                    ConnState::Disconnected
                );
                let events = rt.events.lock().unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|line| line.contains("\"reason\":\"idle_expired\""))
                        .count(),
                    1
                );
                if busy {
                    let finished = events
                        .iter()
                        .position(|line| line.contains("op_failed"))
                        .unwrap();
                    let expired = events
                        .iter()
                        .position(|line| line.contains("idle_expired"))
                        .unwrap();
                    assert!(
                        finished < expired,
                        "Finished event is published before expiry"
                    );
                }
                drop(events);
                expire_idle_sender(&rt, &conn.conn_id);
                assert_eq!(
                    rt.events
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|line| line.contains("idle_expired"))
                        .count(),
                    1
                );
            });
        }
    }
    rt.teardown(engine.runtime());
}

/// A lease survives until the cancelled future is actually dropped,
/// including cancellation before the spawned future's first poll.
#[test]
fn cancelled_send_releases_once_without_resurrecting_terminal_pairing() {
    let engine = AkEngine::new().unwrap();
    let (_dir, rt, conn) = synthetic_sender(&engine);
    let lease = synthetic_send(&rt, &conn, [76; 16]);
    engine.runtime().block_on(async {
        let task = tokio::spawn(async move {
            let _lease = lease;
            std::future::pending::<()>().await;
        });
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    });
    assert!(rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
        .active
        .is_empty());
    assert_eq!(
        *rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
            .state
            .borrow(),
        SenderKeepaliveState::Idle
    );
    let lease = synthetic_send(&rt, &conn, [77; 16]);
    rt.push_disconnected_once(&conn.conn_id, "local");
    drop(lease);
    assert_eq!(
        *rt.sender_lifetimes.lock().unwrap()[&conn.conn_id]
            .state
            .borrow(),
        SenderKeepaliveState::Closed
    );
    assert!(!rt
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains("idle_expired")));
    rt.teardown(engine.runtime());
}

/// P2-06: the save folder is resolved when a transfer STARTS, not when
/// the code was generated — a `set_save_dir` after the code (but
/// before the offer) applies to the incoming task.
#[test]
fn save_dir_change_applies_to_the_next_received_task() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    // Create the code FIRST (the receiver captures the DEFAULT dir),
    // then change the setting — the change must govern the task.
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let inbox = dir_a.path().join("later-inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );

    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    let payload = b"saved in the changed folder".repeat(32);
    std::fs::write(dir_b.path().join("moved.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("moved.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("job"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
    assert!(send_ok, "{send_json}");
    let delivered =
        find_file(&inbox, "moved.bin").expect("the file lands in the folder current at task start");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    assert!(
        find_file(&dir_a.path().join("downloads"), "moved.bin").is_none(),
        "the stale creation-time folder must not receive the task"
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-07: two pending consents on ONE engine (two receivers, two
/// paired senders) park nothing on the two Tokio workers — a third
/// registry op on the same engine completes while both humans have
/// not decided, and both transfers then land once accepted. (Under
/// the retired Condvar gate this test starves: both workers block,
/// the third op cannot run.)
#[test]
fn pending_consents_do_not_starve_the_engine_runtime() {
    let registry = TestRegistry::spawn();
    let engine = ak_engine_new(); // A1, A2 (receivers) + A3
    let engine_f = ak_engine_new(); // B1, B2 (senders)
    let mut receivers = Vec::new();
    let mut senders = Vec::new();
    let mut codes = Vec::new();
    for index in 0..2 {
        let dir = tempfile::tempdir().expect("receiver tempdir");
        let inbox = dir.path().join(format!("inbox{index}"));
        let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
        let runtime = start_task_on(engine, &registry, dir.path());
        assert_eq!(
            ak_task_set_save_dir(engine, runtime.as_ptr(), save_path.as_ptr()),
            0
        );
        let (_, ok, json) = create_code(
            engine,
            &runtime,
            "{\"spec_version\":1,\"role\":\"receiver\"}",
        );
        assert!(ok, "{json}");
        codes.push((code_of(&json), dir, inbox));
        receivers.push(runtime);
    }
    for (code, _, _) in &codes {
        let dir = tempfile::tempdir().expect("sender tempdir");
        let runtime = start_task_on(engine_f, &registry, dir.path());
        let (join_ok, join_json) = join_code_as(engine_f, &runtime, code, Some("sender"));
        assert!(join_ok, "{join_json}");
        senders.push((runtime, conn_of(&join_json), dir));
    }

    // Both senders fire; both receivers park at their consent gates.
    let payload = b"concurrent consent payload".repeat(64);
    let mut ops = Vec::new();
    for (runtime, conn, dir) in &senders {
        std::fs::write(dir.path().join("cc.bin"), &payload).unwrap();
        let paths = CString::new(json_paths(&dir.path().join("cc.bin"))).unwrap();
        let mut op = [0u8; 16];
        assert_eq!(
            ak_task_send(
                engine_f,
                runtime.as_ptr(),
                conn.as_ptr(),
                paths.as_ptr(),
                op.as_mut_ptr()
            ),
            0
        );
        ops.push(op);
    }
    // Both receivers park at their consent gates; capture the job ids.
    let mut jobs = Vec::new();
    for runtime in &receivers {
        let request = wait_event(engine, runtime, "incoming_request");
        jobs.push(hex16_id(
            request
                .split("\"job\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .expect("job"),
        ));
    }

    // THE P2-07 ASSERTION: with both consents still pending, a third
    // REAL op on the same two-worker engine completes promptly.
    let dir3 = tempfile::tempdir().expect("tempdir 3");
    let third = start_task_on(engine, &registry, dir3.path());
    let started = Instant::now();
    let (_, third_ok, third_json) =
        create_code(engine, &third, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(third_ok, "the runtime is not starved: {third_json}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the third op completed while both consents were pending"
    );

    // Both pending gates then resolve and both transfers land.
    for (index, runtime) in receivers.iter().enumerate() {
        assert_eq!(
            ak_task_consent(engine, runtime.as_ptr(), jobs[index].as_ptr(), 1),
            0
        );
        let (send_ok, send_json) = wait_op(engine_f, &senders[index].0, &ops[index]);
        assert!(send_ok, "transfer {index} lands: {send_json}");
        assert!(
            find_file(&codes[index].2, "cc.bin").is_some(),
            "transfer {index} bytes landed"
        );
    }
    ak_engine_free(engine);
    ak_engine_free(engine_f);
}

/// P2-12: the probe echo responder answers a genuine probe (with the
/// fixed all-zero reply nonce), NEVER answers a reply (its own or
/// another responder's — the reflection loop killer), and keeps
/// answering fresh probes afterwards.
#[test]
fn echo_responder_answers_probes_but_never_replies_to_replies() {
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &spec_for(dir.path())).expect("runtime");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    // The responder's wildcard bind reports 0.0.0.0:port on Windows;
    // dial it through loopback (exactly what local_candidates does).
    let bound_one = engine
        .runtime()
        .block_on(async { spawn_echo_responder(&rt, [1u8; 16]) })
        .expect("responder one");
    let addr_one = SocketAddr::from(([127, 0, 0, 1], bound_one.port()));
    let bound_two = engine
        .runtime()
        .block_on(async { spawn_echo_responder(&rt, [2u8; 16]) })
        .expect("responder two");
    let addr_two = SocketAddr::from(([127, 0, 0, 1], bound_two.port()));
    let probe_socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe socket");
    probe_socket
        .set_read_timeout(Some(Duration::from_millis(400)))
        .expect("read timeout");

    // A genuine probe is answered; the reply carries the zero nonce.
    let nonce = [0xA5u8; 16];
    probe_socket
        .send_to(&ak_net::holepunch::probe_packet(&nonce), addr_one)
        .expect("probe sent");
    let mut buffer = [0u8; 1500];
    let (len, from) = probe_socket
        .recv_from(&mut buffer)
        .expect("a reply arrives");
    assert_eq!(len, 24, "the reply is a probe-shaped packet");
    assert_eq!(
        &buffer[8..len],
        &[0u8; 16][..],
        "the reply nonce is the marker"
    );
    assert_eq!(from, addr_one);
    let reply = buffer[..len].to_vec();

    // The responder never answers its OWN reply (a reply is not a probe).
    probe_socket.send_to(&reply, addr_one).expect("reply sent");
    assert!(
        probe_socket.recv_from(&mut buffer).is_err(),
        "a responder must never answer a reply"
    );

    // Two responders fed a reply stay silent — no mutual answering loop.
    probe_socket
        .send_to(&reply, addr_two)
        .expect("cross reply sent");
    assert!(
        probe_socket.recv_from(&mut buffer).is_err(),
        "a second responder must never answer a reply either"
    );

    // Fresh probes still work: neither responder was consumed.
    probe_socket
        .send_to(&ak_net::holepunch::probe_packet(&[0x5Au8; 16]), addr_two)
        .expect("fresh probe");
    let (len, _) = probe_socket
        .recv_from(&mut buffer)
        .expect("responder two answers");
    assert_eq!(len, 24);
}

// -- wave-30 review findings (P1-04/05 residuals, P2-08/09/11
//    residuals, R29-P2-03) -----------------------------------------------

/// P1-05 (production wiring): the FFI's own owner loop runs behind the
/// pre-commit redeem-offer gate. A CROSS-ENGINE join that declares the
/// SAME direction as the code (explicitly, or through the FFI's
/// no-hint default on a receiver-role code) is refused BEFORE the
/// server's commit — the join op fails, no `connected` event fires on
/// the owner, and the code's single redeem is NOT consumed: a
/// correctly-directed join of the very same code still pairs.
#[test]
fn production_owner_loop_refuses_same_direction_joins_before_commit() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());

    // Phase 1: an EXPLICIT same-direction join is refused pre-commit.
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "receiver-role create: {json}");
    let code = code_of(&json);
    let (wrong_ok, wrong_json) = join_code_as(engine_b, &joiner, &code, Some("receiver"));
    assert!(
        !wrong_ok,
        "a receiver-role join of a receiver-role code must be refused: {wrong_json}"
    );
    assert!(
        wrong_json.contains("unknown task code"),
        "the pre-commit gate refuses with the unified unavailable answer: {wrong_json}"
    );
    assert_no_event_within(engine_a, &owner, "\"event\":\"connected\"", 1500);
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        !text_of(&buf).contains("\"conn\""),
        "the refused joiner holds no connection: {}",
        text_of(&buf)
    );
    // The single redeem survived the refusal: a complementary join of
    // the SAME code pairs (a consumed code would answer exhausted).
    let (right_ok, right_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(right_ok, "the code must still pair: {right_json}");

    // Phase 2: the FFI's no-hint DEFAULT direction (receiver) on a
    // FRESH receiver-role code is equally refused pre-commit, and the
    // code stays usable for an explicit complementary join.
    let (_, ok_two, json_two) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok_two, "{json_two}");
    let code_two = code_of(&json_two);
    let (default_ok, default_json) = join_code(engine_b, &joiner, &code_two);
    assert!(
        !default_ok,
        "the no-role default (receiver) must be refused on a receiver code: {default_json}"
    );
    let dir_c = tempfile::tempdir().expect("tempdir c");
    let joiner_two = start_task_on(engine_b, &registry, dir_c.path());
    let (fixed_ok, fixed_json) = join_code_as(engine_b, &joiner_two, &code_two, Some("sender"));
    assert!(fixed_ok, "the second code still pairs: {fixed_json}");
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-08: the registry CONTROL connection is not a data-plane verdict.
/// With the whole registry gone, the paired tasks stay live (no
/// `disconnected` event, `ak_task_list` keeps the connection) and a
/// real send still delivers bytes — the data plane never depended on
/// the registry.
#[test]
fn registry_connection_loss_keeps_paired_tasks_and_sends_working() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");
    // Let both probes settle, then take the ENTIRE registry away.
    let _ = wait_event(engine_a, &owner, "probe_result");
    drop(registry);

    // No kill event: the tasks outlive the control plane.
    assert_no_event_within(engine_a, &owner, "disconnected", 1500);
    assert_no_event_within(engine_b, &joiner, "disconnected", 1500);
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        text_of(&buf).contains("\"state\":\"connected\""),
        "the paired task outlives the registry: {}",
        text_of(&buf)
    );

    // A real send over the (registry-free) data plane still lands.
    let payload = b"data plane without a registry".repeat(32);
    std::fs::write(dir_b.path().join("noreg.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("noreg.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0,
        "the send is accepted after the registry died: {}",
        last_error_of(engine_b)
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
    assert!(send_ok, "the transfer completes: {send_json}");
    let delivered = find_file(&inbox, "noreg.bin").expect("the bytes landed");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P1-04 residual: ONE connection's failed handshake (a stranger whose
/// TLS the server side rejects — wrong pin, so the client aborts
/// mid-handshake) does NOT end the resident receiver: the bounded
/// `handshake_failed` event fires and the legitimate paired sender
/// still completes a transfer on the very same listener.
#[test]
fn failed_stranger_handshake_does_not_end_the_listener() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let inbox = dir_a.path().join("inbox");
    let save_path = CString::new(inbox.to_string_lossy().replace('\\', "/")).unwrap();
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), save_path.as_ptr()),
        0
    );
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let endpoint = endpoint_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

    // The stranger pins the WRONG fingerprint: the client aborts the
    // TLS handshake after the server certificate arrives, so the
    // server side of the accept fails (Transport/PeerIdentity) — the
    // exact error class the old loop treated as "endpoint dead".
    std::fs::write(dir_a.path().join("midhandshake.bin"), b"stranger").unwrap();
    let failure = wrong_pin_send_fails(endpoint, &dir_a.path().join("midhandshake.bin"));
    assert!(
        !failure.is_empty(),
        "the wrong-pin dial must fail: {failure}"
    );
    // The failure is recorded and the loop CONTINUES listening.
    let recorded = wait_event(engine_a, &owner, "handshake_failed");
    assert!(
        recorded.contains(&core::hex_encode(&code_id_of(&json))),
        "the bounded failure names the code: {recorded}"
    );

    // The legitimate sender completes on the very same listener.
    let payload = b"legitimate after a failed stranger handshake".repeat(16);
    std::fs::write(dir_b.path().join("after.bin"), &payload).unwrap();
    let paths = CString::new(json_paths(&dir_b.path().join("after.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0,
        "the send is accepted: {}",
        last_error_of(engine_b)
    );
    let request = wait_event(engine_a, &owner, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
    assert!(
        send_ok,
        "the listener survived the handshake failure: {send_json}"
    );
    let delivered = find_file(&inbox, "after.bin").expect("the file landed");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// R29-P2-03 (deterministic half): the op id a request reserves and
/// the RUNNING op entry are published ATOMICICALLY — polling the id
/// while the launcher has not even run yet answers `running`, never
/// the `-4` of an unregistered op.
#[test]
fn reserved_op_id_is_pollable_before_the_launcher_runs() {
    let registry = TestRegistry::spawn();
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let args = crate::core::json::parse(
        "{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"e1e2e3d4e5f60718293a4b5c6d7e8f90\"}",
    )
    .expect("args");
    // The gate reserves AND registers — then we STOP between the gate
    // and the launcher (the report's exact interleaving).
    let reserved = match gate_request(&rt, "task_create_code", None, false, &args, None)
        .expect("gate accepts")
    {
        RequestGate::Fresh(reserved) => reserved,
        _other => panic!("fresh request, got a different gate outcome"),
    };
    let snapshot = task_op_result(&engine, &runtime, &reserved.op_id).expect("pollable");
    match snapshot {
        OpResult::Running => {}
        _other => panic!("the reserved op must read running, got a settled or missing op"),
    }
    // The reservation launches exactly once and completes normally.
    let op_id = launch_task_op(
        &engine,
        &rt,
        TaskOpSpec::CreateCode {
            role: TaskRole::Sender,
            candidates: Vec::new(),
            ttl_secs: TASK_CODE_TTL_SECS,
        },
        Some(reserved),
    )
    .expect("launch");
    let (ok, json) = wait_op_owned(&engine, &runtime, &op_id);
    assert!(ok, "{json}");
    // The replay of the same request answers the SAME op.
    match gate_request(&rt, "task_create_code", None, false, &args, None).expect("replay") {
        RequestGate::Replay(replayed) => assert_eq!(replayed, op_id),
        _other => panic!("replay, got a different gate outcome"),
    }
}

/// R29-P2-03 (race half): a concurrent retry that replays a just-
/// reserved request polls the op IMMEDIATELY — running or finished,
/// never `-4` — and the whole race starts exactly one op.
#[test]
fn concurrent_retry_polls_a_registered_op_never_minus_four() {
    let registry = TestRegistry::spawn();
    let engine = Arc::new(AkEngine::new().expect("engine"));
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &registry_spec(&registry, dir.path())).expect("rt");
    let request_id = "f1f2f3d4e5f60718293a4b5c6d7e8f90";
    let spec =
        format!("{{\"spec_version\":1,\"role\":\"sender\",\"request_id\":\"{request_id}\"}}");
    let racer = threaded_create(Arc::clone(&engine), runtime, spec.clone());
    // From THIS thread: retry the same request concurrently — the
    // gate answers the racer's op id (fresh or replay, one atomic
    // acceptance) — and poll that id at once, before the racer's
    // launcher (let alone its op body) necessarily ran.
    let op_id = task_create_code_start(&engine, &runtime, &spec)
        .expect("same-spec retry is accepted (fresh or replay)");
    // The immediate poll must never observe an unregistered op.
    match task_op_result(&engine, &runtime, &op_id).expect("poll") {
        OpResult::Running | OpResult::Finished { .. } => {}
    }
    // Exactly one op for the whole race: the racer's answer is the
    // same id and it completes once.
    let racer_op = racer.recv().expect("racer answered").expect("accepted");
    assert_eq!(racer_op, op_id, "one atomically accepted op");
    let (ok, json) = wait_op_owned(&engine, &runtime, &op_id);
    assert!(ok, "{json}");
    assert!(
        json.contains("\"code\":\"AKI1-"),
        "exactly one claim: {json}"
    );
}

/// P2-09 residual: the connection settle is HONEST. A joiner that
/// advertises only a dead candidate leaves the OWNER's entry
/// `unreachable` in `ak_task_list` (matching its `probe_result`
/// event), while the joiner's own probe of the owner's live endpoint
/// settles `connected`.
#[test]
fn all_failed_probe_settles_unreachable_not_connected() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    // Explicit candidates are advertised verbatim (no echo responder,
    // no real port): the owner's probe of 127.0.0.1:1 is honestly
    // dead, and the settle must SAY so.
    let spec = CString::new(format!(
        "{{\"spec_version\":1,\"code\":\"{code}\",\"role\":\"sender\",\
          \"candidates\":[\"127.0.0.1:1\"]}}"
    ))
    .expect("spec CString");
    let mut op_id = [0u8; 16];
    assert_eq!(
        ak_task_join(engine_b, joiner.as_ptr(), spec.as_ptr(), op_id.as_mut_ptr()),
        0,
        "join accepted: {}",
        last_error_of(engine_b)
    );
    let (join_ok, join_json) = wait_op(engine_b, &joiner, &op_id);
    assert!(join_ok, "{join_json}");
    // The joiner's own probe of the owner's live endpoint succeeds...
    assert!(
        join_json.contains("\"probe\":\"verified\""),
        "the joiner's probe verdict rides the result: {join_json}"
    );
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_b, joiner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        text_of(&buf).contains("\"state\":\"connected\""),
        "the joiner settles connected: {}",
        text_of(&buf)
    );
    // ...while the owner's probe of the dead candidate settles
    // unreachable — never a fake connected.
    let probe = wait_event(engine_a, &owner, "probe_result");
    assert!(probe.contains("\"reachable\":false"), "{probe}");
    let mut buf = cbuf(CAP);
    assert_eq!(
        ak_task_list(engine_a, owner.as_ptr(), buf.as_mut_ptr(), buf.len()),
        1
    );
    assert!(
        text_of(&buf).contains("\"state\":\"unreachable\""),
        "ak_task_list agrees with the probe verdict: {}",
        text_of(&buf)
    );
    assert!(
        !text_of(&buf).contains("\"state\":\"connected\""),
        "a dead candidate must not settle connected: {}",
        text_of(&buf)
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 1): the sender's reflexive
/// opener fires ONCE at PAIRING COMPLETION — both entry points. The
/// JOINING sender sees its `reflexive_opener` event while the join op
/// is settling (the shared pairing-time opener/keepalive task spawned
/// right after the redeem), and the OWNER of a sender-role code sees
/// its own from `owner_side_probe`. The event carries the opener's
/// packet count (host-dependent — 0 only without an adopted socket or
/// non-loopback candidates).
#[test]
fn reflexive_opener_event_fires_at_pairing_on_both_sender_paths() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());

    // Joiner-sender path: the owner holds a receiver-role code, the
    // joiner joins as the SENDER with derived candidates (an adopted
    // dial socket is bound before the redeem).
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "join accepted: {join_json}");
    let opener = wait_event(engine_b, &joiner, "reflexive_opener");
    assert!(
        opener.contains("\"packets\":"),
        "the pairing-time opener's count rides the event: {opener}"
    );

    // Owner-sender path: the owner of a sender-role code runs its
    // opener from owner_side_probe when the joiner's redeem completes.
    let (_, ok_two, json_two) =
        create_code(engine_a, &owner, "{\"spec_version\":1,\"role\":\"sender\"}");
    assert!(ok_two, "{json_two}");
    let code_two = code_of(&json_two);
    let (join_two_ok, join_two_json) = join_code_as(engine_b, &joiner, &code_two, None);
    assert!(join_two_ok, "second join accepted: {join_two_json}");
    let owner_opener = wait_event(engine_a, &owner, "reflexive_opener");
    assert!(
        owner_opener.contains("\"packets\":"),
        "the owner-side pairing-time opener fired: {owner_opener}"
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// P2-11 residual: a pending receive-consent request ends VISIBLY —
/// the 120 s window's auto-decline emits `consent_expired`, a
/// disconnect of the attributed connection resolves the request and
/// emits `request_closed`/`disconnected`, and decided requests answer
/// the frozen `-4`.
#[test]
fn consent_requests_close_visibly_on_expiry_and_disconnect() {
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let code_id = [7u8; 16];
    let conn_id = [9u8; 16];
    let sender_fp = DeviceKeyPair::generate().fingerprint();
    {
        let mut conns = rt.conns.lock().expect("conn table");
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: TaskRole::Receiver,
                state: ConnState::Connected,
                candidates: Vec::new(),
                peer_endpoint: None,
                peer_fp: Some(sender_fp),
                code_id,
                code: "AKI1-TEST".into(),
            },
        );
    }
    let info = ConsentInfo {
        job_id: [0u8; 16],
        root_name: "expiry.bin".into(),
        file_count: 1,
        total_bytes: 16,
        sender_fingerprint: sender_fp,
    };
    let rt_for_close = Arc::clone(&rt);
    let rt_in = Arc::clone(&rt);
    let bound_dir = dir.path().join("bound-inbox");
    let bound_save_json = quoted(&bound_dir.to_string_lossy());
    let (expired_declined, closed_declined) = engine.runtime().block_on(async move {
        // Short window: the auto-decline fires while we await.
        let ConsentAnswer::Pending(expiry_rx) = register_consent(
            &rt_in,
            &code_id,
            &info,
            &bound_dir,
            Duration::from_millis(300),
        ) else {
            panic!("the attributed offer must register as pending");
        };
        // Long window: only the connection's disconnect resolves it.
        let ConsentAnswer::Pending(closed_rx) =
            register_consent(&rt_in, &code_id, &info, &bound_dir, Duration::from_secs(60))
        else {
            panic!("the second offer must register as pending");
        };
        let expired = expiry_rx.await.unwrap_or(false);
        // The disconnect drives the still-pending request to its end.
        rt_for_close.decline_pending_of_conn(&conn_id);
        let closed = closed_rx.await.unwrap_or(false);
        (expired, closed)
    });
    assert!(!expired_declined, "the window auto-declines");
    assert!(!closed_declined, "the disconnect declines");
    // Both terminal events are observable, attributed to the conn.
    let events: Vec<String> = rt
        .events
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    let conn_hex = core::hex_encode(&conn_id);
    let expired_line = events
        .iter()
        .find(|line| line.contains("consent_expired") && line.contains(&conn_hex))
        .expect("the expiry is a visible request-terminal event");
    let expired_job = hex16_id(
        expired_line
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert!(
        events.iter().any(|line| line.contains("request_closed")
            && line.contains("\"reason\":\"disconnected\"")
            && line.contains(&conn_hex)),
        "the disconnect closes its pending request visibly: {events:?}"
    );
    // Wave-31 P2-06: the consent event carries the transfer's BOUND
    // save dir — the one binding, not a fresh read of the setting.
    assert!(
        events
            .iter()
            .any(|line| line.contains("\"event\":\"incoming_request\"")
                && line.contains(&format!("\"saveDir\":{bound_save_json}"))),
        "the consent event carries the bound save dir: {events:?}"
    );
    // Decided requests answer the frozen -4.
    let decided = task_consent(&engine, &runtime, &expired_job, true);
    assert!(decided.is_err(), "the expired request is already decided");
    assert_eq!(
        decided.err().map(|e| e.code),
        Some(crate::core::AK_ERR_NOT_FOUND)
    );
}

// -- wave-31 review findings (R30-P2-02, R30-P2-03, P2-06 residual) ----

/// Poll an OWNED runtime's event queue until one event whose body
/// contains `needle` arrives (draining older events like `wait_event`).
fn wait_owned_event(rt: &Arc<TaskRt>, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let popped = rt
            .events
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front());
        if let Some(line) = popped {
            if line.contains(needle) {
                return line;
            }
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}"
        );
        std::thread::sleep(POLL);
    }
}

/// R30-P2-02: both sides of a pairing share the SAME code id — the
/// join result and BOTH `connected` events (the owner's from its
/// resident owner loop, the joiner's from its own bookkeeping) carry
/// the create result's registry code id, so a joiner that typed the
/// code (and never saw the create result) can fill its conn→code
/// mapping.
#[test]
fn connected_events_and_join_result_carry_the_shared_code_id() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let code_id_hex = core::hex_encode(&code_id_of(&json));
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    assert!(
        join_json.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
        "the join result carries the redeemed code's id: {join_json}"
    );
    let on_owner = wait_event(engine_a, &owner, "\"event\":\"connected\"");
    assert!(
        on_owner.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
        "the owner's connected event carries the shared code id: {on_owner}"
    );
    let on_joiner = wait_event(engine_b, &joiner, "\"event\":\"connected\"");
    assert!(
        on_joiner.contains(&format!("\"code_id\":\"{code_id_hex}\"")),
        "the joiner's connected event carries the shared code id: {on_joiner}"
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

/// R30-P2-02 (the progress-key migration): receive progress rows ride
/// under the CONNECTION id — the one correlation key both sides of a
/// pairing hold — exactly like the sender's rows; no receive row may
/// surface under the code id (the joiner's Dart side maps progress by
/// conn id alone and would never see a code-id row).
#[test]
fn receive_progress_rows_ride_under_the_connection_id() {
    let registry = TestRegistry::spawn();
    let engine_a = Arc::new(AkEngine::new().expect("engine a"));
    let engine_b = Arc::new(AkEngine::new().expect("engine b"));
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = task_start(&engine_a, &registry_spec(&registry, dir_a.path())).expect("rt");
    let rt_a = task_runtime_of(&engine_a, &owner).expect("task runtime");
    let joiner = task_start(&engine_b, &registry_spec(&registry, dir_b.path())).expect("rt2");
    let create_op = task_create_code_start(
        &engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    )
    .expect("create accepted");
    let (ok, json) = wait_op_owned(&engine_a, &owner, &create_op);
    assert!(ok, "{json}");
    let code = code_of(&json);
    let code_id_hex = core::hex_encode(&code_id_of(&json));
    let join_op = task_join_start(
        &engine_b,
        &joiner,
        &format!("{{\"spec_version\":1,\"code\":\"{code}\",\"role\":\"sender\"}}"),
    )
    .expect("join accepted");
    let (join_ok, join_json) = wait_op_owned(&engine_b, &joiner, &join_op);
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    // The owner's connection entry (its receiver admission is bound to
    // it) arrives with the owner-loop completion.
    let _ = wait_owned_event(&rt_a, "\"event\":\"connected\"");

    let payload = b"progress-key migration payload".repeat(64);
    std::fs::write(dir_b.path().join("job.bin"), &payload).expect("source file");
    let send_op = task_send_start(
        &engine_b,
        &joiner,
        &conn,
        &json_paths(&dir_b.path().join("job.bin")),
    )
    .expect("send accepted");
    let request = wait_owned_event(&rt_a, "incoming_request");
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    task_consent(&engine_a, &owner, &job, true).expect("consent");
    let (send_ok, send_json) = wait_op_owned(&engine_b, &joiner, &send_op);
    assert!(send_ok, "the transfer completes: {send_json}");
    let _ = wait_owned_event(&rt_a, "\"event\":\"done\"");

    // The RECEIVER's progress rows: keyed by the conn id, never the
    // code id (rows survive completion in the engine-wide queue).
    let conn_hex = core::hex_encode(&conn);
    let recv_rows: Vec<String> = rt_a
        .engine_progress
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    assert!(
        recv_rows
            .iter()
            .any(|row| row.contains(&format!("\"job\":\"{conn_hex}\""))
                && row.contains("\"event\":\"accepted\"")),
        "receive progress rides under the connection id: {recv_rows:?}"
    );
    assert!(
        recv_rows
            .iter()
            .all(|row| !row.contains(&format!("\"job\":\"{code_id_hex}\""))),
        "no receive row may ride under the code id: {recv_rows:?}"
    );
    // The SENDER's rows on its own engine share the SAME key.
    let rt_b = task_runtime_of(&engine_b, &joiner).expect("joiner runtime");
    let send_rows: Vec<String> = rt_b
        .engine_progress
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    assert!(
        send_rows
            .iter()
            .any(|row| row.contains(&format!("\"job\":\"{conn_hex}\""))),
        "send progress keeps the connection-id key: {send_rows:?}"
    );
    // Client UI U1 (ffi-v2 §8.4 U1 revision): the task-plane rows
    // carry the engine payload fields — the GUI's byte progress reads
    // the totals and the per-file sizes from them.
    let total = payload.len();
    assert!(
        recv_rows.iter().any(|row| row.contains(&format!(
            "\"job\":\"{conn_hex}\",\"event\":\"accepted\",\"files\":1,\"bytes\":{total}}}"
        ))),
        "the receiver's accepted row carries files/bytes: {recv_rows:?}"
    );
    assert!(
        send_rows.iter().any(|row| row.starts_with(&format!(
            "{{\"job\":\"{conn_hex}\",\"event\":\"file_started\",\"file\":0,\"path\":"
        )) && row.ends_with(&format!(",\"bytes\":{total}}}"))),
        "the sender's file_started row carries file/path/bytes: {send_rows:?}"
    );
    assert!(
        send_rows
            .iter()
            .chain(recv_rows.iter())
            .all(|row| !row.contains("\"conn\":")),
        "task-plane rows never carry the engine-level conn index: {send_rows:?}"
    );
}

/// R30-P2-03 (deterministic half): an offer registered on an
/// already-closed connection — the report's interleaving with the
/// disconnect's conns flip landed first — is refused inside the
/// registration's critical section: NO `incoming_request` surfaces,
/// nothing stays pending, and any consent for the never-published
/// request answers the frozen `-4`.
#[test]
fn register_consent_on_a_closed_connection_publishes_nothing() {
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let code_id = [7u8; 16];
    let conn_id = [9u8; 16];
    let sender_fp = DeviceKeyPair::generate().fingerprint();
    {
        let mut conns = rt.conns.lock().expect("conn table");
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: TaskRole::Receiver,
                state: ConnState::Connected,
                candidates: Vec::new(),
                peer_endpoint: None,
                peer_fp: Some(sender_fp),
                code_id,
                code: "AKI1-TEST".into(),
            },
        );
    }
    // The disconnect's first half (flip + `disconnected` line) and its
    // pending decline BOTH complete before the registration runs.
    rt.push_disconnected_once(&conn_id, "local");
    rt.decline_pending_of_conn(&conn_id);
    let info = ConsentInfo {
        job_id: [0u8; 16],
        root_name: "late.bin".into(),
        file_count: 1,
        total_bytes: 16,
        sender_fingerprint: sender_fp,
    };
    let reg_rt = Arc::clone(&rt);
    let answer = engine.runtime().block_on(async move {
        register_consent(
            &reg_rt,
            &code_id,
            &info,
            std::path::Path::new("D:/unused"),
            Duration::from_secs(60),
        )
    });
    assert!(
        matches!(answer, ConsentAnswer::Now(false)),
        "an offer on a closed connection is refused on the spot"
    );
    let events: Vec<String> = rt
        .events
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    assert!(
        !events.iter().any(|line| line.contains("incoming_request")),
        "no incoming_request may surface for a dead request: {events:?}"
    );
    assert!(
        rt.pending
            .lock()
            .map(|pending| pending.is_empty())
            .unwrap_or(false),
        "nothing stays pending"
    );
    let decided = task_consent(&engine, &runtime, &[1u8; 16], true);
    assert_eq!(
        decided.err().map(|error| error.code),
        Some(crate::core::AK_ERR_NOT_FOUND)
    );
}

/// CR-P3-02: the pure preflight is compiled to a verdict only on
/// Windows — the platform-neutral rules stay the only gate elsewhere.
/// CR-P3-02: on a Windows receiver, an offer whose root name cannot
/// exist here is refused at the consent registration — BEFORE any
/// `incoming_request` surfaces, before anything is pending, before
/// the engine could answer Accept or write a single file — and the
/// refusal surfaces as a readable `offer_refused` event. On other
/// platforms the same offer registers normally (the name is legal
/// there), pinning the platform split.
#[test]
fn windows_receiver_refuses_unsavable_root_name_before_registration() {
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let code_id = [7u8; 16];
    let conn_id = [9u8; 16];
    let sender_fp = DeviceKeyPair::generate().fingerprint();
    {
        let mut conns = rt.conns.lock().expect("conn table");
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: TaskRole::Receiver,
                state: ConnState::Connected,
                candidates: Vec::new(),
                peer_endpoint: None,
                peer_fp: Some(sender_fp),
                code_id,
                code: "AKI1-TEST".into(),
            },
        );
    }
    let info = ConsentInfo {
        job_id: [0u8; 16],
        root_name: "what?now.bin".into(),
        file_count: 1,
        total_bytes: 16,
        sender_fingerprint: sender_fp,
    };
    let reg_rt = Arc::clone(&rt);
    let answer = engine.runtime().block_on(async move {
        register_consent(
            &reg_rt,
            &code_id,
            &info,
            std::path::Path::new("D:/unused"),
            Duration::from_secs(60),
        )
    });
    let events: Vec<String> = rt
        .events
        .lock()
        .map(|queue| queue.iter().cloned().collect())
        .unwrap_or_default();
    if cfg!(windows) {
        assert!(
            matches!(answer, ConsentAnswer::Now(false)),
            "a Windows receiver must refuse the offer on the spot"
        );
        let refusal = events
            .iter()
            .find(|line| line.contains("\"event\":\"offer_refused\""))
            .unwrap_or_else(|| panic!("no offer_refused event: {events:?}"));
        assert!(
            refusal.contains(&core::hex_encode(&conn_id)),
            "the refusal is attributed to the connection: {refusal}"
        );
        assert!(
            refusal.contains("what?now.bin") && refusal.contains("cannot save"),
            "the refusal names the offender readably: {refusal}"
        );
        assert!(
            !events.iter().any(|line| line.contains("incoming_request")),
            "no incoming_request banner may surface: {events:?}"
        );
        assert!(
            rt.pending
                .lock()
                .map(|pending| pending.is_empty())
                .unwrap_or(false),
            "nothing stays pending — no consent to answer, no Accept"
        );
    } else {
        assert!(
            matches!(answer, ConsentAnswer::Pending(_)),
            "a non-Windows receiver registers the offer normally"
        );
        assert!(
            events.iter().any(|line| line.contains("incoming_request")),
            "the offer reaches the user on platforms where the name is legal: {events:?}"
        );
    }
}

/// R30-P2-03 (race half): a registration racing a disconnect of its
/// connection — many real thread interleavings of the exact window
/// (pending insert vs. flip + decline) — can never leave a published
/// `incoming_request` without its `request_closed` landing LATER in
/// the same window, and never leaves a pending entry behind.
#[test]
fn disconnect_register_race_never_surfaces_a_late_incoming() {
    let engine = Arc::new(AkEngine::new().expect("engine"));
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = task_start(&engine, &spec_for(dir.path())).expect("rt");
    let rt = task_runtime_of(&engine, &runtime).expect("task runtime");
    let code_id = [7u8; 16];
    let conn_id = [9u8; 16];
    let sender_fp = DeviceKeyPair::generate().fingerprint();
    {
        let mut conns = rt.conns.lock().expect("conn table");
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: TaskRole::Receiver,
                state: ConnState::Connected,
                candidates: Vec::new(),
                peer_endpoint: None,
                peer_fp: Some(sender_fp),
                code_id,
                code: "AKI1-TEST".into(),
            },
        );
    }
    let info = ConsentInfo {
        job_id: [0u8; 16],
        root_name: "race.bin".into(),
        file_count: 1,
        total_bytes: 16,
        sender_fingerprint: sender_fp,
    };
    for _round in 0..64 {
        // Re-arm the connection (the previous round severed it).
        rt.conns
            .lock()
            .expect("conn table")
            .get_mut(&conn_id)
            .expect("conn entry")
            .state = ConnState::Connected;
        let base = rt.events.lock().expect("event queue").len();
        let reg_rt = Arc::clone(&rt);
        let reg_engine = Arc::clone(&engine);
        let reg_info = info.clone();
        let close_rt = Arc::clone(&rt);
        let registrar = thread::spawn(move || {
            reg_engine.runtime().block_on(async move {
                matches!(
                    register_consent(
                        &reg_rt,
                        &code_id,
                        &reg_info,
                        std::path::Path::new("D:/unused"),
                        Duration::from_millis(25),
                    ),
                    ConsentAnswer::Now(false)
                )
            })
        });
        let closer = thread::spawn(move || {
            // `task_disconnect`'s exact order: flip + `disconnected`
            // line first, then the pending decline of the connection.
            close_rt.push_disconnected_once(&conn_id, "local");
            close_rt.decline_pending_of_conn(&conn_id);
        });
        let _refused = registrar.join().expect("registrar answered");
        closer.join().expect("closer answered");
        let events: Vec<String> = rt
            .events
            .lock()
            .expect("event queue")
            .iter()
            .skip(base)
            .cloned()
            .collect();
        for (index, line) in events.iter().enumerate() {
            if line.contains("\"event\":\"incoming_request\"") {
                let job_hex = line
                    .split("\"job\":\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .expect("job id");
                // R31-P3-02: EITHER terminal is legal — the
                // disconnect-driven request_closed OR the 25 ms
                // consent window's consent_expired (the race may let
                // the expiry task win before the closer thread gets
                // scheduled; both are correct request terminals that
                // can only appear AFTER the incoming was published).
                assert!(
                    events[index + 1..]
                        .iter()
                        .any(|later| (later.contains("\"event\":\"request_closed\"")
                            || later.contains("\"event\":\"consent_expired\""))
                            && later.contains(&format!("\"job\":\"{job_hex}\""))),
                    "every published request reaches a terminal (closed or expired) LATER: {events:?}"
                );
            }
        }
        assert!(
            rt.pending.lock().expect("pending table").is_empty(),
            "no request survives the round"
        );
    }
}

/// P2-06 residual (wave-31): the save folder is ONE immutable binding
/// per transfer — resolved when the connection is accepted, carried
/// identically by the `incoming_request` event, the engine's actual
/// writes and the `done` event. Changing the setting DURING the
/// pending-consent window cannot skew them: the in-flight transfer
/// lands in its bound folder and `done` reports exactly that.
#[test]
fn save_dir_binding_is_immutable_across_the_consent_window() {
    let registry = TestRegistry::spawn();
    let engine_a = ak_engine_new();
    let engine_b = ak_engine_new();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner = start_task_on(engine_a, &registry, dir_a.path());
    let joiner = start_task_on(engine_b, &registry, dir_b.path());
    let alpha = dir_a.path().join("alpha");
    let alpha_text = alpha.to_string_lossy().replace('\\', "/");
    let beta = dir_a.path().join("beta");
    let beta_text = beta.to_string_lossy().replace('\\', "/");
    let alpha_path = CString::new(alpha_text.clone()).expect("path CString");
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), alpha_path.as_ptr()),
        0
    );

    let (_, ok, json) = create_code(
        engine_a,
        &owner,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    );
    assert!(ok, "{json}");
    let code = code_of(&json);
    let (join_ok, join_json) = join_code_as(engine_b, &joiner, &code, Some("sender"));
    assert!(join_ok, "{join_json}");
    let conn = conn_of(&join_json);
    let _ = wait_event(engine_a, &owner, "\"event\":\"connected\"");

    let payload = b"immutable binding payload".repeat(64);
    std::fs::write(dir_b.path().join("bound.bin"), &payload).expect("source file");
    let paths = CString::new(json_paths(&dir_b.path().join("bound.bin"))).unwrap();
    let mut send_op = [0u8; 16];
    assert_eq!(
        ak_task_send(
            engine_b,
            joiner.as_ptr(),
            conn.as_ptr(),
            paths.as_ptr(),
            send_op.as_mut_ptr()
        ),
        0
    );
    // The banner's saveDir is the transfer's binding (alpha) — not a
    // fresh read of the setting.
    let request = wait_event(engine_a, &owner, "incoming_request");
    assert!(
        request.contains(&format!("\"saveDir\":\"{alpha_text}\"")),
        "the consent event shows the bound target: {request}"
    );
    // The user changes the setting DURING the pending window.
    let beta_path = CString::new(beta_text.clone()).expect("path CString");
    assert_eq!(
        ak_task_set_save_dir(engine_a, owner.as_ptr(), beta_path.as_ptr()),
        0
    );
    let job = hex16_id(
        request
            .split("\"job\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("job id"),
    );
    assert_eq!(
        ak_task_consent(engine_a, owner.as_ptr(), job.as_ptr(), 1),
        0
    );
    let (send_ok, send_json) = wait_op(engine_b, &joiner, &send_op);
    assert!(send_ok, "{send_json}");

    // The in-flight transfer landed in its BOUND folder and `done`
    // reports exactly that — the actual disk result, not the newer
    // setting.
    let delivered = find_file(&alpha, "bound.bin").expect("the file lands in the bound folder");
    assert_eq!(std::fs::read(&delivered).unwrap(), payload);
    assert!(
        find_file(&beta, "bound.bin").is_none(),
        "a mid-pending setting change cannot move the in-flight transfer"
    );
    let done = wait_event(engine_a, &owner, "\"event\":\"done\"");
    assert!(
        done.contains(&format!("\"saveDir\":\"{alpha_text}\"")),
        "done reports the actual landing dir: {done}"
    );
    assert!(
        !done.contains(&format!("\"saveDir\":\"{beta_text}\"")),
        "done never reports a dir the transfer did not use: {done}"
    );
    ak_engine_free(engine_a);
    ak_engine_free(engine_b);
}

// -- CR-P2-05 (2026-11-12): the native event doorbell ----------------------
//
// Tests run in PARALLEL threads, so each test below owns its own
// callback + counters — no shared statics, no cross-test races.

/// Test A's callback: counts invocations and records the ctx token
/// of the last call (a sentinel address; never dereferenced).
static A_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static A_CTX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn notify_a(ctx: *mut std::os::raw::c_void) {
    A_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    A_CTX.store(ctx as usize, std::sync::atomic::Ordering::SeqCst);
}

/// Test B's two callbacks (proving a re-registration REPLACES the
/// old callback rather than adding a second one).
static B1_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static B2_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn notify_b1(_ctx: *mut std::os::raw::c_void) {
    B1_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn notify_b2(_ctx: *mut std::os::raw::c_void) {
    B2_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Test C's callback (cross-thread producers).
static C_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn notify_c(_ctx: *mut std::os::raw::c_void) {
    C_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Registered → every queued task event rings the doorbell with the
/// caller's ctx verbatim; cleared (`notify = None`) → zero rings and
/// the pure-polling queue behavior is unchanged (the doorbell never
/// consumes an event). Built on the SAFE layer with an owned engine
/// value, like the consent-teardown test above (no raw pointer is
/// ever dereferenced in this `forbid(unsafe_code)` module).
#[test]
fn event_notify_fires_on_queued_events_and_silences_after_clear() {
    A_HITS.store(0, std::sync::atomic::Ordering::SeqCst);
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
    let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
    // The runtime_ready of task_start queued BEFORE the registration
    // and must NOT have rung; the queue itself still holds it.
    assert_eq!(A_HITS.load(Ordering::SeqCst), 0);
    assert_eq!(rt.events.lock().expect("events").len(), 1);
    let sentinel = 0xdead_beef_usize;
    task_set_event_notify(&engine, &runtime_id, Some((notify_a, sentinel))).expect("register");
    // Registering alone must not ring either (doorbell ≠ payload).
    assert_eq!(A_HITS.load(Ordering::SeqCst), 0);
    rt.push_event("{\"event\":\"probe_result\"}".to_string());
    assert_eq!(A_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(A_CTX.load(Ordering::SeqCst), sentinel);
    rt.push_event("{\"event\":\"done\"}".to_string());
    assert_eq!(A_HITS.load(Ordering::SeqCst), 2, "one ring per event");
    // Clearing: returns only after the last fire — no more rings.
    task_set_event_notify(&engine, &runtime_id, None).expect("clear");
    rt.push_event("{\"event\":\"disconnected\"}".to_string());
    assert_eq!(
        A_HITS.load(Ordering::SeqCst),
        2,
        "cleared doorbell never rings"
    );
    // Every queued line is still POLLABLE — the doorbell never
    // consumed anything (poll stays authoritative): runtime_ready +
    // the three pushes.
    assert_eq!(rt.events.lock().expect("events").len(), 4);
    // Teardown (engine drop) also silences a doorbell someone left
    // registered: the Arc-cloned runtime outlives the engine value,
    // and its later pushes must not ring a dead embedder callback.
    task_set_event_notify(&engine, &runtime_id, Some((notify_a, 0)))
        .expect("re-register before drop");
    let survivor = Arc::clone(&rt);
    drop(engine); // teardown clears the slot first
    survivor.push_event("{}".to_string());
    assert_eq!(
        A_HITS.load(Ordering::SeqCst),
        2,
        "teardown silenced the doorbell (engine_free may not fire)"
    );
}

/// A REPLACING registration swaps the callback; a NULL ctx with a
/// live fn is legal (opaque token); unknown runtime `-4`; NULL
/// runtime pointer `-1` — all through the C EXPORT adapter, with the
/// safe layer confirming the swap's effect. The engine VALUE backs
/// both views (the exports take `*mut AkEngine`; a shared-borrow
/// cast is sound because every export reads the handle as `&`).
#[test]
fn event_notify_export_validates_and_replaces() {
    B1_HITS.store(0, std::sync::atomic::Ordering::SeqCst);
    B2_HITS.store(0, std::sync::atomic::Ordering::SeqCst);
    let engine = AkEngine::new().expect("engine");
    let raw = &engine as *const AkEngine as *mut AkEngine;
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
    let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
    assert_eq!(
        ak_task_set_event_notify(
            raw,
            [9u8; 16].as_ptr(),
            Some(notify_b1),
            std::ptr::null_mut()
        ),
        -4,
        "unknown runtime id"
    );
    assert_eq!(
        ak_task_set_event_notify(raw, std::ptr::null(), Some(notify_b1), std::ptr::null_mut()),
        -1,
        "null runtime pointer"
    );
    assert_eq!(
        ak_task_set_event_notify(
            raw,
            runtime_id.as_ptr(),
            Some(notify_b1),
            std::ptr::null_mut()
        ),
        0,
        "register via the export"
    );
    rt.push_event("{}".to_string());
    assert_eq!(B1_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(B2_HITS.load(Ordering::SeqCst), 0);
    assert_eq!(
        ak_task_set_event_notify(
            raw,
            runtime_id.as_ptr(),
            Some(notify_b2),
            std::ptr::null_mut()
        ),
        0
    );
    rt.push_event("{}".to_string());
    assert_eq!(B1_HITS.load(Ordering::SeqCst), 1, "replaced, not added");
    assert_eq!(B2_HITS.load(Ordering::SeqCst), 1, "NULL ctx passes through");
    assert_eq!(
        ak_task_set_event_notify(raw, runtime_id.as_ptr(), None, std::ptr::null_mut()),
        0,
        "clear via the export"
    );
    rt.push_event("{}".to_string());
    assert_eq!(B2_HITS.load(Ordering::SeqCst), 1, "cleared = silent");
    drop(engine);
}

/// The fire happens on the PRODUCING thread and never blocks it into
/// a deadlock: a producer thread pushes 64 events (ringing on ITS
/// thread under the fire lock) while this thread clears — the clear
/// waits for the in-flight fire, and afterwards no ring may happen
/// even though the producer's Arc<TaskRt> still exists.
#[test]
fn event_notify_crosses_threads_and_clear_waits_for_inflight_fire() {
    C_HITS.store(0, std::sync::atomic::Ordering::SeqCst);
    let engine = AkEngine::new().expect("engine");
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime_id = task_start(&engine, &spec_for(dir.path())).expect("runtime");
    task_set_event_notify(&engine, &runtime_id, Some((notify_c, 0))).expect("register");
    let rt = task_runtime_of(&engine, &runtime_id).expect("task runtime");
    let producer = thread::spawn(move || {
        for _ in 0..64 {
            rt.push_event("{\"event\":\"chunk_sent\"}".to_string());
        }
    });
    producer
        .join()
        .expect("producer must not deadlock under the fire lock");
    assert_eq!(C_HITS.load(Ordering::SeqCst), 64);
    task_set_event_notify(&engine, &runtime_id, None).expect("clear");
    let survivor = task_runtime_of(&engine, &runtime_id).expect("task runtime");
    survivor.push_event("{}".to_string());
    assert_eq!(
        C_HITS.load(Ordering::SeqCst),
        64,
        "no ring after the clear returned"
    );
    drop(engine);
}

// -- helpers --------------------------------------------------------------

fn json_paths(path: &std::path::Path) -> String {
    format!(
        "{{\"spec_version\":1,\"paths\":[\"{}\"]}}",
        path.to_string_lossy().replace('\\', "/")
    )
}

fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(hit) = walk(&path, name) {
                    return Some(hit);
                }
            } else if path.file_name().map(|n| n == name).unwrap_or(false) {
                return Some(path);
            }
        }
        None
    }
    walk(root, name)
}

// -- W1 (batch W, 2026-11-22, V-N2): post-connect failure convergence ------

/// The W1 scenario state (see [`run_silent_peer_send`]).
struct SilentPeerScenario {
    /// The receiver-side engine, held alive (silent) for the scenario's
    /// whole lifetime — never read directly.
    _owner: AkEngine,
    sender: AkEngine,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    sender_runtime: [u8; 16],
    conn: [u8; 16],
    send_op: [u8; 16],
    send_answer: (bool, String),
    started: Instant,
}

/// W1 (batch W, 2026-11-22, V-N2): pair a receiver-owner with a
/// sender-joiner over a live registry, shorten ONLY the sender runtime's
/// engine idle budget (the per-runtime test knob — production keeps
/// `TASK_SEND_IDLE`, 150 s), then start a real send whose peer goes
/// SILENT after the QUIC connection is established: the offer parks on
/// the receiver's consent gate and is never answered, and no close frame
/// is ever sent — the exact wire shape of the Android freezer incident
/// (the sender's connection died of its idle budget while the process
/// was frozen). Resolves when the send op settles; the receiver engine
/// stays alive and silent throughout.
fn run_silent_peer_send(idle: Duration) -> SilentPeerScenario {
    let registry = TestRegistry::spawn();
    let owner = AkEngine::new().expect("owner engine");
    let sender = AkEngine::new().expect("sender engine");
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let owner_runtime =
        task_start(&owner, &registry_spec(&registry, dir_a.path())).expect("owner runtime");
    let sender_runtime =
        task_start(&sender, &registry_spec(&registry, dir_b.path())).expect("sender runtime");
    let create_op = task_create_code_start(
        &owner,
        &owner_runtime,
        "{\"spec_version\":1,\"role\":\"receiver\"}",
    )
    .expect("create accepted");
    let (ok, json) = wait_op_owned(&owner, &owner_runtime, &create_op);
    assert!(ok, "receiver-role create (real bind): {json}");
    let code = code_of(&json);
    let join_op = task_join_start(
        &sender,
        &sender_runtime,
        &format!("{{\"spec_version\":1,\"code\":\"{code}\",\"role\":\"sender\"}}"),
    )
    .expect("join accepted");
    let (join_ok, join_json) = wait_op_owned(&sender, &sender_runtime, &join_op);
    assert!(join_ok, "sender-side join: {join_json}");
    let conn = conn_of(&join_json);
    // W1: the per-runtime TEST idle. The silent peer's wait must exhaust
    // it inside the CI budget; no production constant changes and no
    // other test's runtime is affected.
    *task_runtime_of(&sender, &sender_runtime)
        .expect("sender TaskRt")
        .send_idle_timeout
        .lock()
        .expect("send idle slot") = idle;
    let payload = dir_b.path().join("w1-silent.bin");
    std::fs::write(&payload, b"w1 silent-peer payload".repeat(64)).expect("source file");
    let started = Instant::now();
    let send_op = task_send_start(&sender, &sender_runtime, &conn, &json_paths(&payload))
        .expect("send accepted");
    // The receiver's consent gate fires; NEVER answer it — the peer is
    // silent from the offer on (no close frame, no decline).
    let send_answer = wait_op_owned(&sender, &sender_runtime, &send_op);
    SilentPeerScenario {
        _owner: owner,
        sender,
        _dirs: (dir_a, dir_b),
        sender_runtime,
        conn,
        send_op,
        send_answer,
        started,
    }
}

/// W1 (batch W, 2026-11-22, V-N2): the peer endpoint goes SILENT after
/// the QUIC connection is established (no close frame — the freezer
/// shape). The sender's send op dies on the engine idle budget, and the
/// connection must end with `disconnected`/`conn_lost` within the
/// (test-shortened) idle cap plus margin — NOT `idle_expired` (the
/// 10-minute pairing budget), and never a silently-`connected` zombie
/// row: the pre-fix behavior the V2-b walkthrough measured (connected at
/// t=121 s, the send failed on thaw, no terminal event until
/// `idle_expired` at t=719.6 s — 598 s of zombie).
#[test]
fn silent_peer_after_connect_ends_the_connection_with_conn_lost() {
    let idle = Duration::from_secs(2);
    let scene = run_silent_peer_send(idle);
    assert!(
        !scene.send_answer.0,
        "the send must fail against the silent peer: {}",
        scene.send_answer.1
    );
    assert!(
        scene.send_answer.1.contains("after connecting"),
        "a post-connect failure, not a dial failure: {}",
        scene.send_answer.1
    );
    let rt = task_runtime_of(&scene.sender, &scene.sender_runtime).expect("sender TaskRt");
    let line = wait_owned_event(&rt, "\"event\":\"disconnected\"");
    assert!(
        line.contains("\"reason\":\"conn_lost\""),
        "the honest terminal reason is conn_lost, not the pairing budget: {line}"
    );
    assert_eq!(conn_of(&line), scene.conn, "attributed to the connection");
    // Bounded by the idle cap plus margin (CI headroom) — not the
    // 10-minute pairing budget the zombie row used to wait out.
    assert!(
        scene.started.elapsed() < idle + Duration::from_secs(30),
        "the terminal verdict must land within the idle cap plus margin"
    );
    // The engine's own snapshot is honestly terminal (no zombie row).
    let snapshot = task_list(&scene.sender, &scene.sender_runtime).expect("task list");
    let conn_hex = core::hex_encode(&scene.conn);
    assert!(
        snapshot.contains(&format!("\"conn\":\"{conn_hex}\"")),
        "the row is still listed: {snapshot}"
    );
    assert!(
        snapshot.contains("\"state\":\"disconnected\""),
        "the row is terminal in the engine snapshot: {snapshot}"
    );
    // The pairing budget NEVER fired for this connection (the settle
    // wait also guarantees the op_failed announcement is already queued,
    // so the drained set below is the complete story).
    wait_owned_event(&rt, "\"event\":\"op_failed\"");
    let queue: Vec<String> = rt
        .events
        .lock()
        .map(|events| events.iter().cloned().collect())
        .unwrap_or_default();
    assert!(
        !queue
            .iter()
            .any(|line| line.contains("idle_expired") && line.contains(&conn_hex)),
        "idle_expired stays reserved for the pairing budget: {queue:?}"
    );
}

/// W1 (batch W): pin the EVENT PAIR of a post-connect failure — the
/// connection's terminal `disconnected`/`conn_lost` line AND the op's
/// own `op_failed` announcement are BOTH emitted; the disconnected line
/// arrives first here (the close precedes the op's settle publication;
/// same-frame arrival would be equally legal — the contract demands
/// both exist, ffi-v2 §8.3 批次 W 增补).
#[test]
fn post_connect_failure_emits_both_conn_lost_and_op_failed() {
    let scene = run_silent_peer_send(Duration::from_secs(2));
    assert!(
        !scene.send_answer.0,
        "the send must fail against the silent peer: {}",
        scene.send_answer.1
    );
    let rt = task_runtime_of(&scene.sender, &scene.sender_runtime).expect("sender TaskRt");
    let conn_hex = core::hex_encode(&scene.conn);
    let op_hex = core::hex_encode(&scene.send_op);
    let is_conn_lost = |line: &String| {
        line.contains("\"event\":\"disconnected\"")
            && line.contains(&format!("\"conn\":\"{conn_hex}\""))
            && line.contains("\"reason\":\"conn_lost\"")
    };
    let is_op_failed = |line: &String| {
        line.contains("\"event\":\"op_failed\"")
            && line.contains(&format!("\"op_id\":\"{op_hex}\""))
    };
    // Drain until BOTH lines are observable (bounded like every helper).
    let mut lines: Vec<String> = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        while let Some(line) = rt
            .events
            .lock()
            .ok()
            .and_then(|mut events| events.pop_front())
        {
            lines.push(line);
        }
        if lines.iter().any(is_conn_lost) && lines.iter().any(is_op_failed) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both events must arrive; saw {lines:?}"
        );
        std::thread::sleep(POLL);
    }
    let disconnected_at = lines.iter().position(is_conn_lost).expect("drained above");
    let failed_at = lines.iter().position(is_op_failed).expect("drained above");
    assert!(
        disconnected_at <= failed_at,
        "disconnected lands at (or before) op_failed — both exist either way: {lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|line| is_conn_lost(line)).count(),
        1,
        "the connection's terminal line fires exactly once: {lines:?}"
    );
}
