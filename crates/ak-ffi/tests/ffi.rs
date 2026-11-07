//! End-to-end tests for the ak-ffi C ABI. Every test drives the library
//! ONLY through the exported `ak_*` functions (never `core` directly):
//! real loopback QUIC transfers crossing the C boundary, spec-JSON
//! validation, cancellation, buffer-capacity handling, two engines
//! running concurrently, identity inspection (`ak_identity_info`),
//! persisted-job listing (`ak_jobs_list`), paired-peer book management
//! (`ak_peer_list`/`ak_peer_add`/`ak_peer_remove`), and receive admission
//! by peer book (`expect: "book"`). Fixture preparation (writing source
//! files, pre-creating a receiver device key so its fingerprint is known)
//! uses plain `std`/`ak-identity` — the transfers themselves never bypass
//! the ABI.
//!
//! L2 shape: real engine + real QUIC on 127.0.0.1 (same claim level as
//! the ak-itests loopback scenarios), not a cross-machine claim.

use std::ffi::CString;
use std::fs;
use std::net::UdpSocket;
use std::os::raw::c_char;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use ak_ffi::{
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_identity_info, ak_job_cancel,
    ak_job_result, ak_jobs_list, ak_peer_add, ak_peer_list, ak_peer_remove, ak_poll_progress,
    ak_receive_start, ak_send_start, ak_version, AkEngine, AK_DONE, AK_ERR_BUFFER, AK_ERR_INTERNAL,
    AK_ERR_INVALID, AK_ERR_NOT_FOUND, AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK, AK_PENDING,
};

// ---------------------------------------------------------------------------
// Small helpers (safe conversions only; no unsafe needed in tests).
// ---------------------------------------------------------------------------

/// Deterministic xorshift byte stream so source trees are reproducible.
struct Pattern(u64);

impl Pattern {
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            out.push((self.0 >> 33) as u8);
        }
        out
    }
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).unwrap();
    fs::write(path, bytes).unwrap();
}

/// JSON string literal for a path (escapes backslashes for Windows).
fn jstr(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn path_jstr(path: &Path) -> String {
    jstr(&path.to_string_lossy())
}

/// Read a `c_char` buffer up to the first NUL as UTF-8 (safe conversion).
fn cbuf_to_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8(bytes).expect("FFI output is UTF-8")
}

fn last_error_text(engine: *mut AkEngine) -> String {
    let mut buf = [0 as c_char; 2048];
    let code = unsafe { ak_engine_last_error(engine, buf.as_mut_ptr(), buf.len()) };
    assert!(code >= 0, "last_error itself failed: {code}");
    cbuf_to_string(&buf)
}

fn poll_big(engine: *mut AkEngine) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = unsafe { ak_poll_progress(engine, buf.as_mut_ptr(), buf.len()) };
    (code, cbuf_to_string(&buf))
}

fn result_big(engine: *mut AkEngine, job: &[u8; 16]) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = unsafe { ak_job_result(engine, job.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    (code, cbuf_to_string(&buf))
}

/// Reserve then release an ephemeral loopback UDP port.
fn free_udp_port() -> u16 {
    UdpSocket::bind(("127.0.0.1", 0))
        .expect("reserve udp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

/// Pre-create the receiver identity (`<id_dir>/device.key`, the ak-cli
/// convention) so the test knows the fingerprint the sender must pin.
/// Fixture-only use of ak-identity; the transfer still runs over the ABI.
fn make_receiver_identity(id_dir: &Path) -> String {
    let key = ak_identity::DeviceKeyPair::generate();
    key.save(&id_dir.join("device.key")).expect("save key");
    key.fingerprint().to_hex()
}

fn event_name(line: &str) -> String {
    let key = "\"event\":\"";
    let start = line.find(key).expect("event key") + key.len();
    let end = start + line[start..].find('"').expect("event value end");
    line[start..end].to_string()
}

fn line_job(line: &str) -> String {
    let key = "\"job\":\"";
    let start = line.find(key).expect("job key") + key.len();
    let end = start + line[start..].find('"').expect("job value end");
    line[start..end].to_string()
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

fn field_str(text: &str, key: &str) -> String {
    let pat = format!("\"{key}\":\"");
    let start = text
        .find(&pat)
        .unwrap_or_else(|| panic!("key {key} in {text}"))
        + pat.len();
    let end = start + text[start..].find('"').expect("string value end");
    text[start..end].to_string()
}

fn hex16(job: &[u8; 16]) -> String {
    job.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn events_of(lines: &[String], job_hex: &str) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line_job(line) == job_hex)
        .map(|line| event_name(line))
        .collect()
}

/// Byte-exact tree comparison (dirs + files), first divergences reported.
fn assert_trees_identical(src: &Path, dst: &Path) {
    fn walk(root: &Path, prefix: &str, dirs: &mut Vec<String>, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(root).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                dirs.push(rel.clone());
                walk(&entry.path(), &rel, dirs, files);
            } else {
                files.push((rel, fs::read(entry.path()).expect("read file")));
            }
        }
    }
    let mut src_dirs = Vec::new();
    let mut src_files = Vec::new();
    let mut dst_dirs = Vec::new();
    let mut dst_files = Vec::new();
    walk(src, "", &mut src_dirs, &mut src_files);
    walk(dst, "", &mut dst_dirs, &mut dst_files);
    src_dirs.sort();
    dst_dirs.sort();
    src_files.sort_by(|a, b| a.0.cmp(&b.0));
    dst_files.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(src_dirs, dst_dirs, "directory structure differs");
    assert_eq!(
        src_files
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        dst_files
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        "file sets differ"
    );
    for ((name, want), (_, got)) in src_files.iter().zip(&dst_files) {
        assert_eq!(want, got, "file contents differ for {name}");
    }
}

// ---------------------------------------------------------------------------
// The shared through-the-ABI loopback runner.
// ---------------------------------------------------------------------------

struct LoopbackOutcome {
    send_job: [u8; 16],
    recv_job: [u8; 16],
    send_json: String,
    recv_json: String,
    lines: Vec<String>,
}

/// Start one receiver + one sender on `engine` via the C ABI and pump
/// events until both jobs finish. When `probe_small_buffers` is set the
/// runner additionally drives the `AK_ERR_BUFFER` retry path mid-run.
fn run_loopback(
    engine: *mut AkEngine,
    src: &Path,
    dst: &Path,
    recv_id_dir: &Path,
    recv_fp_hex: &str,
    probe_small_buffers: bool,
    extra_send_fields: &str,
) -> LoopbackOutcome {
    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"device_name\":\"ffi-recv\",\"timeout_secs\":30}}",
        path_jstr(dst),
        path_jstr(recv_id_dir)
    );
    let mut recv_job = [0u8; 16];
    let code = unsafe {
        ak_receive_start(
            engine,
            CString::new(recv_spec).unwrap().as_ptr(),
            recv_job.as_mut_ptr(),
        )
    };
    assert_eq!(code, AK_OK, "receive_start: {}", last_error_text(engine));

    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\
         \"fp_hex\":\"{recv_fp_hex}\",\"chunk_kb\":64,\
         \"device_name\":\"ffi-send\",\"timeout_secs\":30{extra_send_fields}}}",
        path_jstr(src)
    );
    let mut send_job = [0u8; 16];
    let code = unsafe {
        ak_send_start(
            engine,
            CString::new(send_spec).unwrap().as_ptr(),
            send_job.as_mut_ptr(),
        )
    };
    assert_eq!(code, AK_OK, "send_start: {}", last_error_text(engine));
    assert_ne!(send_job, [0u8; 16], "job ids must never be all-zero");
    assert_ne!(send_job, recv_job, "each job gets its own id");

    pump_jobs(engine, &recv_job, &send_job, probe_small_buffers)
}

/// Pump both started jobs to their terminal states through the ABI and
/// verify the buffer-retry path mid-run when asked. Panics when either
/// side fails; shared by `run_loopback` and the custom-spec tests.
fn pump_jobs(
    engine: *mut AkEngine,
    recv_job: &[u8; 16],
    send_job: &[u8; 16],
    probe_small_buffers: bool,
) -> LoopbackOutcome {
    let recv_job = *recv_job;
    let send_job = *send_job;
    let mut lines: Vec<String> = Vec::new();
    let mut send_json = String::new();
    let mut recv_json = String::new();
    let mut small_polled_ok = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if probe_small_buffers && !small_polled_ok {
            // Cap 4 can never hold a real event line; the pending event
            // must stay queued when this returns AK_ERR_BUFFER.
            let mut tiny = [0 as c_char; 4];
            let code = unsafe { ak_poll_progress(engine, tiny.as_mut_ptr(), tiny.len()) };
            if code == AK_ERR_BUFFER {
                let message = last_error_text(engine);
                assert!(
                    message.contains("buffer too small") && message.contains("need"),
                    "capacity hint in last_error: {message}"
                );
                small_polled_ok = true;
            } else {
                assert_eq!(code, AK_PENDING, "tiny poll: {}", last_error_text(engine));
            }
        }
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            assert!(!line.contains('\n'), "event lines are single-line: {line}");
            lines.push(line);
        } else {
            assert_eq!(code, AK_PENDING, "poll: {}", last_error_text(engine));
        }
        if send_json.is_empty() {
            match result_big(engine, &send_job) {
                (AK_FAILED, json) => panic!("sender failed through the ABI: {json}"),
                (AK_DONE, json) => send_json = json,
                _ => {}
            }
        }
        if recv_json.is_empty() {
            match result_big(engine, &recv_job) {
                (AK_FAILED, json) => panic!("receiver failed through the ABI: {json}"),
                (AK_DONE, json) => recv_json = json,
                _ => {}
            }
        }
        if !send_json.is_empty() && !recv_json.is_empty() {
            // Both terminal states are set; the final events (done) may
            // still be queued — drain before judging the streams.
            loop {
                let (code, line) = poll_big(engine);
                if code != AK_EVENT {
                    assert_eq!(code, AK_PENDING);
                    break;
                }
                lines.push(line);
            }
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!send_json.is_empty(), "sender did not finish within 90 s");
    assert!(!recv_json.is_empty(), "receiver did not finish within 90 s");
    if probe_small_buffers {
        assert!(small_polled_ok, "the tiny-buffer poll never saw an event");
        // Nothing was lost: the same run still delivered every event the
        // jobs produced, ending in `done` for both sides.
        // (checked by the callers via `lines`)
        let mut tiny = [0 as c_char; 8];
        let code =
            unsafe { ak_job_result(engine, send_job.as_ptr(), tiny.as_mut_ptr(), tiny.len()) };
        assert_eq!(code, AK_ERR_BUFFER, "result must refuse a tiny buffer");
        let message = last_error_text(engine);
        assert!(
            message.contains("buffer too small") && message.contains("retry"),
            "capacity hint in last_error: {message}"
        );
        let (code, json) = result_big(engine, &send_job);
        assert_eq!(code, AK_DONE, "retry with a large buffer: {json}");
        assert_eq!(json, send_json, "lossless retry returns the same report");
    }
    LoopbackOutcome {
        send_job,
        recv_job,
        send_json,
        recv_json,
        lines,
    }
}

/// Nested fixture tree: an empty dir, CRLF text, binary payloads, a deep
/// path, and a filename with spaces/semicolons. Returns
/// (payload bytes, manifest entry count: files + directories).
fn make_source_tree(base: &Path) -> (u64, u64) {
    let mut pattern = Pattern(0xFF11_0000_0000_0002);
    write_file(
        &base.join("notes-crlf.txt"),
        b"line one\r\nline two\r\nno final newline",
    );
    let binary = pattern.bytes(160 * 1024);
    write_file(&base.join("photos/img-0001.bin"), &binary);
    let tail = pattern.bytes(53 * 1024 + 7);
    write_file(&base.join("photos/raw/img-0002.bin"), &tail);
    fs::create_dir_all(base.join("photos/empty-album")).unwrap();
    write_file(
        &base.join("docs/deep/manual; v2 (draft).txt"),
        b"ffi loopback: byte-identical",
    );
    let bytes = (binary.len()
        + tail.len()
        + b"line one\r\nline two\r\nno final newline".len()
        + b"ffi loopback: byte-identical".len()) as u64;
    // 4 files + directories photos, photos/raw, photos/empty-album,
    // docs, docs/deep.
    (bytes, 9)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn version_new_free_last_error_roundtrip() {
    // Version: exact string, NUL termination, negative length semantics.
    let expected = "ak-ffi 1 (wire v1, caps 0x0007)";
    let mut buf = [0 as c_char; 128];
    let code = unsafe { ak_version(buf.as_mut_ptr(), buf.len()) };
    assert_eq!(code, expected.len() as i32);
    assert_eq!(cbuf_to_string(&buf), expected);
    assert_eq!(buf[code as usize], 0, "NUL terminator right after payload");

    // Too-small capacity: specific negative code, capacity hint, lossless.
    let mut tiny = [0 as c_char; 4];
    assert_eq!(
        unsafe { ak_version(tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let message = last_error_text(std::ptr::null_mut());
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );
    // Reading does not clear the message (other entry points do).
    let mut buf = [0 as c_char; 256];
    let code = unsafe { ak_engine_last_error(std::ptr::null(), buf.as_mut_ptr(), buf.len()) };
    assert_eq!(code as usize, message.len(), "full message written");
    assert_eq!(cbuf_to_string(&buf), message, "message survives reading");
    // Null buffer.
    assert_eq!(
        unsafe { ak_version(std::ptr::null_mut(), 128) },
        AK_ERR_NULL
    );

    // A fresh successful call clears the slot.
    let mut buf = [0 as c_char; 64];
    assert_eq!(
        unsafe { ak_version(buf.as_mut_ptr(), buf.len()) },
        expected.len() as i32
    );
    let code = unsafe { ak_engine_last_error(std::ptr::null(), buf.as_mut_ptr(), buf.len()) };
    assert_eq!(code, 0, "successful calls clear the last error");
    assert_eq!(cbuf_to_string(&buf), "");

    // Engine lifecycle.
    let engine = ak_engine_new();
    assert!(!engine.is_null(), "engine_new must return a handle");
    unsafe { ak_engine_free(std::ptr::null_mut()) }; // no-op, must not crash
    unsafe { ak_engine_free(engine) };
}

#[test]
fn send_receive_loopback_through_the_abi() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let id_dir = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(id_dir.path());
    assert_eq!(recv_fp.len(), 64);

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("ffi-source");
    let (total_bytes, total_entries) = make_source_tree(&src);
    let dst = tempfile::tempdir().unwrap();

    let outcome = run_loopback(engine, &src, dst.path(), id_dir.path(), &recv_fp, true, "");

    // Reports agree with each other and with the payload.
    let send_files = field_u64(&outcome.send_json, "files");
    let send_bytes = field_u64(&outcome.send_json, "bytes");
    assert_eq!(
        send_files, total_entries,
        "files + directories in the manifest"
    );
    assert_eq!(send_bytes, total_bytes);
    assert_eq!(field_u64(&outcome.recv_json, "files"), send_files);
    assert_eq!(field_u64(&outcome.recv_json, "bytes"), send_bytes);
    assert_eq!(
        field_str(&outcome.send_json, "wire_job"),
        field_str(&outcome.recv_json, "wire_job"),
        "both sides report the same protocol job id"
    );
    assert_eq!(field_str(&outcome.send_json, "peer"), recv_fp);
    assert_eq!(
        field_u64(&outcome.send_json, "resumed"),
        0,
        "fresh first transfer"
    );
    assert!(field_u64(&outcome.send_json, "duration_ms") > 0);
    assert!(
        outcome.send_json.contains("\"roots\":[\"ffi-source\"]"),
        "roots list the primary root name: {}",
        outcome.send_json
    );

    // Event streams: full ordered sequence per side, `done` last.
    let send_hex = hex16(&outcome.send_job);
    let recv_hex = hex16(&outcome.recv_job);
    let send_events = events_of(&outcome.lines, &send_hex);
    let recv_events = events_of(&outcome.lines, &recv_hex);
    assert_eq!(send_events.first().map(String::as_str), Some("connecting"));
    assert_eq!(send_events.last().map(String::as_str), Some("done"));
    assert_eq!(recv_events.first().map(String::as_str), Some("listening"));
    assert_eq!(recv_events.last().map(String::as_str), Some("done"));
    for anchor in ["connecting", "connected", "offered", "accepted", "done"] {
        assert!(
            send_events.iter().any(|name| name == anchor),
            "sender saw {anchor}"
        );
    }
    let position = |name: &str| {
        send_events
            .iter()
            .position(|event| event == name)
            .unwrap_or_else(|| panic!("sender sequence needs {name}"))
    };
    assert!(position("connecting") < position("connected"));
    assert!(position("connected") < position("offered"));
    assert!(position("offered") < position("accepted"));
    assert!(position("accepted") < position("done"));
    let listening = recv_events
        .iter()
        .position(|event| event == "listening")
        .expect("receiver emits listening once bound");
    let recv_connected = recv_events
        .iter()
        .position(|event| event == "connected")
        .expect("receiver connects");
    assert!(listening < recv_connected);
    assert!(recv_events.iter().any(|name| name == "manifest_received"));
    assert!(recv_events.iter().any(|name| name == "file_committed"));

    // Events reference only the two known jobs.
    for line in &outcome.lines {
        let job = line_job(line);
        assert!(
            job == send_hex || job == recv_hex,
            "stray job id {job} in event {line}"
        );
    }

    // The bytes that crossed the ABI are the bytes on disk.
    assert_trees_identical(&src, dst.path());
    assert!(
        !dst.path().join("nope").exists(),
        "destination is the expected tree"
    );

    // Unknown job ids are rejected, not guessed.
    let bogus = [9u8; 16];
    assert_eq!(
        unsafe { ak_job_result(engine, bogus.as_ptr(), [0 as c_char; 64].as_mut_ptr(), 64) },
        AK_ERR_NOT_FOUND
    );
    assert_eq!(
        unsafe { ak_job_cancel(engine, bogus.as_ptr()) },
        AK_ERR_NOT_FOUND
    );

    unsafe { ak_engine_free(engine) };
}

/// `ak_send_start` with error-message sanity, usable many times per test.
fn try_send(engine: *mut AkEngine, spec: &str, job: &mut [u8; 16]) -> i32 {
    let code = unsafe {
        ak_send_start(
            engine,
            CString::new(spec).unwrap().as_ptr(),
            job.as_mut_ptr(),
        )
    };
    if code < 0 {
        assert!(!last_error_text(engine).is_empty(), "errors set last_error");
    }
    code
}

fn try_receive(engine: *mut AkEngine, spec: &str, job: &mut [u8; 16]) -> i32 {
    let code = unsafe {
        ak_receive_start(
            engine,
            CString::new(spec).unwrap().as_ptr(),
            job.as_mut_ptr(),
        )
    };
    if code < 0 {
        assert!(!last_error_text(engine).is_empty(), "errors set last_error");
    }
    code
}

#[test]
fn bad_spec_json_is_rejected_with_clear_codes() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let fp = "ab".repeat(32);
    let src = tempfile::tempdir().unwrap();
    let src_field = path_jstr(src.path());
    let id_dir = tempfile::tempdir().unwrap();
    let id_field = path_jstr(id_dir.path());
    let out_dir = tempfile::tempdir().unwrap();
    let out_field = path_jstr(out_dir.path());
    let dead_port = free_udp_port();
    let mut job = [0u8; 16];

    // Malformed JSON / not an object / trailing garbage.
    assert_eq!(try_send(engine, "{nope", &mut job), AK_ERR_INVALID);
    assert_eq!(try_send(engine, "[1,2]", &mut job), AK_ERR_INVALID);
    assert_eq!(try_send(engine, "{} extra", &mut job), AK_ERR_INVALID);
    assert_eq!(try_send(engine, "", &mut job), AK_ERR_INVALID);
    assert_eq!(try_receive(engine, "{", &mut job), AK_ERR_INVALID);

    // Missing / mistyped required fields.
    let good = format!(
        "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\"fp_hex\":\"{fp}\"}}"
    );
    assert_eq!(
        try_send(engine, &good, &mut job),
        AK_OK,
        "baseline valid spec starts"
    );
    assert_eq!(
        try_send(
            engine,
            &format!("{{\"peer\":\"127.0.0.1:{dead_port}\",\"fp_hex\":\"{fp}\"}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("source_roots"));
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":\"not-an-array\",\"peer\":\"127.0.0.1:{dead_port}\",\"fp_hex\":\"{fp}\"}}"
            ),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert_eq!(
        try_send(
            engine,
            &format!("{{\"source_roots\":[{src_field}],\"peer\":123,\"fp_hex\":\"{fp}\"}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\"fp_hex\":\"{short}\"}}",
                short = "ab".repeat(31)
            ),
            &mut job
        ),
        AK_ERR_INVALID
    );

    // Numeric range / type errors.
    for bad_chunk in [
        "\"chunk_kb\":0",
        "\"chunk_kb\":8193",
        "\"chunk_kb\":\"64\"",
        "\"chunk_kb\":1.5",
        "\"chunk_kb\":-8",
    ] {
        assert_eq!(
            try_send(
                engine,
                &format!(
                    "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
                     \"fp_hex\":\"{fp}\",{bad_chunk}}}"
                ),
                &mut job
            ),
            AK_ERR_INVALID,
            "chunk field {bad_chunk}"
        );
    }
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
                 \"fp_hex\":\"{fp}\",\"multistream\":\"yes\"}}"
            ),
            &mut job
        ),
        AK_ERR_INVALID
    );

    // Control characters in values: raw and escaped forms both refused.
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
                 \"fp_hex\":\"{fp}\",\"device_name\":\"two\nlines\"}}"
            ),
            &mut job
        ),
        AK_ERR_INVALID,
        "raw newline inside a string value"
    );
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
                 \"fp_hex\":\"{fp}\",\"device_name\":\"beep\\u0007\"}}"
            ),
            &mut job
        ),
        AK_ERR_INVALID,
        "\\u0007 escape decodes to a control character"
    );
    assert_eq!(
        try_send(
            engine,
            &format!(
                "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
                 \"fp_hex\":\"{fp}\",\"device_name\":\"tab\\there\"}}"
            ),
            &mut job
        ),
        AK_ERR_INVALID,
        "\\t escape decodes to a control character"
    );
    assert_eq!(
        try_send(engine, r#"{"a":"\x41"}"#, &mut job),
        AK_ERR_INVALID,
        "invalid escape"
    );

    // Pathological input shapes.
    let nesting = format!("{}1{}", "[".repeat(40), "]".repeat(40));
    let deep = format!(
        "{{\"future\":{nesting},\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\"fp_hex\":\"{fp}\"}}"
    );
    assert_eq!(
        try_send(engine, &deep, &mut job),
        AK_ERR_INVALID,
        "nesting beyond 32 levels"
    );

    // Receive-side validation.
    assert_eq!(
        try_receive(engine, "{\"port\":1}", &mut job),
        AK_ERR_INVALID
    );
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"port\":0,\"once_jobs\":2}}"),
            &mut job
        ),
        AK_ERR_INVALID,
        "concurrent receive needs an explicit port"
    );
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"expect\":\"maybe\"}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"conflict\":\"overwrite\"}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"once_jobs\":0}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );

    // Null arguments.
    assert_eq!(
        unsafe { ak_send_start(engine, std::ptr::null(), job.as_mut_ptr()) },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe { ak_receive_start(engine, std::ptr::null(), job.as_mut_ptr()) },
        AK_ERR_NULL
    );
    let ok_spec = CString::new(good).unwrap();
    assert_eq!(
        unsafe { ak_send_start(engine, ok_spec.as_ptr(), std::ptr::null_mut()) },
        AK_ERR_NULL
    );

    // Invalid UTF-8 in the C string.
    let bad_utf8: [c_char; 3] = [0xC3u8 as c_char, 0x28u8 as c_char, 0];
    assert_eq!(
        unsafe { ak_send_start(engine, bad_utf8.as_ptr(), job.as_mut_ptr()) },
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("UTF-8"));

    // Unknown fields are ignored (forward compatibility): a spec full of
    // not-yet-known keys of every JSON type still starts, and the job it
    // created is then cancelled cleanly through the ABI.
    let unknown = format!(
        "{{\"future_int\":7,\"future_bool\":true,\"future_null\":null,\
          \"future_str\":\"x\",\"future_arr\":[1,\"two\",{{\"deep\":false}}],\
          \"future_obj\":{{\"nested\":{{\"more\":[true,null]}}}},\
          \"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
          \"fp_hex\":\"{fp}\",\"id_dir\":{id_field},\"timeout_secs\":5}}"
    );
    assert_eq!(
        try_send(engine, &unknown, &mut job),
        AK_OK,
        "unknown fields must not block a start"
    );
    assert_ne!(job, [0u8; 16]);
    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED, "cancelled job reports failure");
    assert!(json.contains("cancelled by caller"), "body: {json}");

    unsafe { ak_engine_free(engine) };
}

#[test]
fn cancel_mid_transfer_fails_without_fake_completion() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let id_dir = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(id_dir.path());

    // One big deterministic file so the cancel lands mid-chunk-phase.
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("bulk");
    let mut pattern = Pattern(0xCA11_0000_0000_0003);
    let block = pattern.bytes(1024 * 1024);
    let mut bulk = Vec::with_capacity(256 * 1024 * 1024);
    for _ in 0..256 {
        bulk.extend_from_slice(&block);
    }
    write_file(&src, &bulk);
    let dst = tempfile::tempdir().unwrap();

    let port = free_udp_port();
    // The aborted sender vanishes WITHOUT a wire close (its endpoint is
    // dropped mid-await), so the receiver's failure surfaces as the idle
    // timeout; keep it short so the test observes the failure quickly.
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\"timeout_secs\":10}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec).unwrap().as_ptr(),
                recv_job.as_mut_ptr(),
            )
        },
        AK_OK
    );
    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\"fp_hex\":\"{recv_fp}\",\
         \"chunk_kb\":64,\"timeout_secs\":60}}",
        path_jstr(&src)
    );
    let mut send_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_send_start(
                engine,
                CString::new(send_spec).unwrap().as_ptr(),
                send_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );
    let send_hex = hex16(&send_job);

    // Wait until chunks are actually flowing; fail loudly if the transfer
    // completes before a cancel could land (then the fixture is too small).
    let mut lines: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < deadline, "no chunk events within 60 s");
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            if line_job(&line) == send_hex && event_name(&line) == "chunk_sent" {
                lines.push(line);
                break;
            }
            lines.push(line);
        } else {
            let (code, json) = result_big(engine, &send_job);
            assert_eq!(
                code, AK_PENDING,
                "transfer finished before cancel could land: {json}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    // The job is genuinely mid-flight; abort it through the ABI.
    let (code, json) = result_big(engine, &send_job);
    assert_eq!(code, AK_PENDING, "still running before cancel ({json})");
    assert_eq!(unsafe { ak_job_cancel(engine, send_job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &send_job);
    assert_eq!(code, AK_FAILED, "cancelled job must read as failed");
    assert!(
        json.contains("cancelled by caller"),
        "failure body names the cancellation: {json}"
    );

    // The receiver observes the vanished peer and also fails.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut recv_failed = false;
    while Instant::now() < deadline {
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            lines.push(line);
        }
        let (code, json) = result_big(engine, &recv_job);
        if code == AK_FAILED {
            assert!(!json.is_empty(), "error body present: {json}");
            recv_failed = true;
            break;
        }
        assert_ne!(code, AK_DONE, "receiver must not fake success: {json}");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(recv_failed, "receiver failed after the sender vanished");

    // Drain everything left: no `done` anywhere — no fake completion.
    loop {
        let (code, line) = poll_big(engine);
        if code != AK_EVENT {
            assert_eq!(code, AK_PENDING);
            break;
        }
        lines.push(line);
    }
    for line in &lines {
        assert!(
            !line.contains("\"event\":\"done\""),
            "no done event may survive a cancelled run: {line}"
        );
    }

    unsafe { ak_engine_free(engine) };
}

// ---------------------------------------------------------------------------
// Wave-17 fixes: null job_out before side effects, required `expect`,
// cancel convergence (port rebind + quiet), DEL/C1 control characters.
// ---------------------------------------------------------------------------

/// Poll until the queue is empty, collecting every line; asserts PENDING.
fn drain_events(engine: *mut AkEngine, lines: &mut Vec<String>) {
    loop {
        let (code, line) = poll_big(engine);
        if code != AK_EVENT {
            assert_eq!(code, AK_PENDING, "drain: {}", last_error_text(engine));
            break;
        }
        lines.push(line);
    }
}

/// Poll until `job` emits `event` (other lines collected and returned);
/// panics with the collected lines when `timeout` elapses first.
fn wait_for_event(
    engine: *mut AkEngine,
    job: &[u8; 16],
    event: &str,
    timeout: Duration,
) -> Vec<String> {
    let job_hex = hex16(job);
    let mut others = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            assert!(!line.contains('\n'), "single-line events: {line}");
            if line_job(&line) == job_hex && event_name(&line) == event {
                return others;
            }
            others.push(line);
        } else {
            assert_eq!(code, AK_PENDING, "poll: {}", last_error_text(engine));
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("no {event} event for job {job_hex} within {timeout:?}; saw {others:?}");
}

#[test]
fn null_job_out_is_refused_before_any_task_starts() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    // A fully VALID send spec; the only defect is the null output pointer.
    let src = tempfile::tempdir().unwrap();
    write_file(&src.path().join("f.bin"), b"payload");
    let fp = "ab".repeat(32);
    let dead_port = free_udp_port();
    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{dead_port}\",\
         \"fp_hex\":\"{fp}\",\"timeout_secs\":30}}",
        path_jstr(src.path())
    );
    assert_eq!(
        unsafe {
            ak_send_start(
                engine,
                CString::new(send_spec).unwrap().as_ptr(),
                std::ptr::null_mut(),
            )
        },
        AK_ERR_NULL
    );
    let message = last_error_text(engine);
    assert!(
        message.contains("job_out"),
        "last_error names job_out: {message}"
    );

    // A fully VALID receive spec with a concrete port; same defect.
    let id_dir = tempfile::tempdir().unwrap();
    make_receiver_identity(id_dir.path());
    let dst = tempfile::tempdir().unwrap();
    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"timeout_secs\":30}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec.clone()).unwrap().as_ptr(),
                std::ptr::null_mut(),
            )
        },
        AK_ERR_NULL
    );
    assert!(last_error_text(engine).contains("job_out"));

    // No task may have been produced: over a window long enough for a real
    // receive to emit `listening` (bind is immediate) and a real send to
    // emit `connecting`, the queue stays empty.
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        let (code, line) = poll_big(engine);
        assert_eq!(code, AK_PENDING, "no event may exist, got: {line}");
        thread::sleep(Duration::from_millis(25));
    }

    // Contrast: the same spec with a real job_out DOES start and emit,
    // proving the quiet window above was the absence of tasks, not a dead
    // engine (the job table and queue still work).
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec).unwrap().as_ptr(),
                recv_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );
    wait_for_event(engine, &recv_job, "listening", Duration::from_secs(20));
    assert_eq!(unsafe { ak_job_cancel(engine, recv_job.as_ptr()) }, AK_OK);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn receive_spec_without_expect_is_rejected() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let dst = tempfile::tempdir().unwrap();
    let out_field = path_jstr(dst.path());
    let id_dir = tempfile::tempdir().unwrap();
    let id_field = path_jstr(id_dir.path());
    let mut job = [0u8; 16];

    // Missing or null expect: -2 with the exact required-field hint.
    for spec in [
        format!("{{\"out_dir\":{out_field},\"port\":0}}"),
        format!("{{\"out_dir\":{out_field},\"port\":0,\"expect\":null}}"),
    ] {
        assert_eq!(
            try_receive(engine, &spec, &mut job),
            AK_ERR_INVALID,
            "spec {spec}"
        );
        let message = last_error_text(engine);
        assert!(
            message.contains("expect required")
                && message.contains("any")
                && message.contains("book")
                && message.contains("pairing code"),
            "required-expect hint: {message}"
        );
        assert_eq!(job, [0u8; 16], "no job id on refusal");
        assert!(
            !dst.path().join("checkpoints").exists(),
            "refusal starts nothing on disk"
        );
    }

    // A present-but-empty expect is a VALUE error (the parse hint), still -2.
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"\"}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("expect"));

    // A wrong TYPE is a type error, not the required hint.
    assert_eq!(
        try_receive(
            engine,
            &format!("{{\"out_dir\":{out_field},\"port\":0,\"expect\":7}}"),
            &mut job
        ),
        AK_ERR_INVALID
    );

    // "any" must be EXPLICIT: with it the same spec starts (then cancels).
    let spec =
        format!("{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"any\",\"id_dir\":{id_field}}}");
    assert_eq!(try_receive(engine, &spec, &mut job), AK_OK);
    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED);
    assert!(json.contains("cancelled by caller"), "{json}");

    unsafe { ak_engine_free(engine) };
}

#[test]
fn cancelled_single_receive_port_rebinds_immediately() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    make_receiver_identity(id_dir.path());
    let dst = tempfile::tempdir().unwrap();
    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"timeout_secs\":30}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );

    let mut job = [0u8; 16];
    assert_eq!(try_receive(engine, &recv_spec, &mut job), AK_OK);
    wait_for_event(engine, &job, "listening", Duration::from_secs(20));
    let job_hex = hex16(&job);

    // Cancel through the ABI; the result flips to the cancelled failure.
    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED, "cancelled job reports failure");
    assert!(
        json.contains("cancelled by caller"),
        "failure body names the cancellation: {json}"
    );

    // Convergence: once cancel returned, the job never emits again. Drain
    // the pre-cancel backlog, then the queue must stay empty — and no
    // fake-completion `done` may ever have been queued.
    let mut lines = Vec::new();
    drain_events(engine, &mut lines);
    thread::sleep(Duration::from_millis(300));
    let (code, line) = poll_big(engine);
    assert_eq!(code, AK_PENDING, "quiet after cancel: {line}");
    assert!(
        !lines
            .iter()
            .any(|l| line_job(l) == job_hex && l.contains("\"event\":\"done\"")),
        "no done for a cancelled job: {lines:?}"
    );

    // THE point of the convergence fix: the same port rebinds IMMEDIATELY
    // (the endpoint was reaped with the aborted task, not at idle timeout).
    let mut job2 = [0u8; 16];
    assert_eq!(
        try_receive(engine, &recv_spec, &mut job2),
        AK_OK,
        "immediate rebind on port {port}: {}",
        last_error_text(engine)
    );
    wait_for_event(engine, &job2, "listening", Duration::from_secs(20));
    assert_eq!(unsafe { ak_job_cancel(engine, job2.as_ptr()) }, AK_OK);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn cancelled_concurrent_receive_releases_its_port() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    make_receiver_identity(id_dir.path());
    let dst = tempfile::tempdir().unwrap();
    let port = free_udp_port();

    // once_jobs=2 with no senders: ak-engine parks a detached accept task
    // owning the endpoint on this port (no stop channel exists for it).
    let spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"once_jobs\":2,\"timeout_secs\":60}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );
    let mut job = [0u8; 16];
    assert_eq!(try_receive(engine, &spec, &mut job), AK_OK);
    thread::sleep(Duration::from_millis(150));
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_PENDING, "still serving before cancel: {json}");

    // Cancel converges (abort + await, then the port is knocked so the
    // detached accept task drops the endpoint instead of holding it for
    // the full 60 s accept deadline).
    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED);
    assert!(json.contains("cancelled by caller"), "{json}");

    // The port must be rebindable IMMEDIATELY: a fresh single receive on
    // the same port starts and reports itself listening.
    let single = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"timeout_secs\":30}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );
    let mut job2 = [0u8; 16];
    assert_eq!(
        try_receive(engine, &single, &mut job2),
        AK_OK,
        "rebind on port {port}: {}",
        last_error_text(engine)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_listening = false;
    while Instant::now() < deadline {
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            if line_job(&line) == hex16(&job2) && event_name(&line) == "listening" {
                saw_listening = true;
                break;
            }
        } else {
            assert_eq!(code, AK_PENDING, "{}", last_error_text(engine));
        }
        // Surface the bind failure itself (e.g. AddrInUse) instead of a
        // bare listening timeout.
        let (code, json) = result_big(engine, &job2);
        if code == AK_FAILED {
            panic!("rebind receive on port {port} failed: {json}");
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        saw_listening,
        "the cancelled port rebinds and listens right away"
    );
    assert_eq!(unsafe { ak_job_cancel(engine, job2.as_ptr()) }, AK_OK);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn cancelled_send_job_is_cancelled_and_stays_quiet() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let src = tempfile::tempdir().unwrap();
    write_file(&src.path().join("f.bin"), b"x");
    let fp = "ab".repeat(32);
    let port = free_udp_port();
    let spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\"fp_hex\":\"{fp}\",\
         \"timeout_secs\":30}}",
        path_jstr(src.path())
    );

    // The peer never answers; the sender parks in its connect budget.
    let mut job = [0u8; 16];
    assert_eq!(try_send(engine, &spec, &mut job), AK_OK);
    wait_for_event(engine, &job, "connecting", Duration::from_secs(20));
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_PENDING, "still connecting before cancel: {json}");

    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED);
    assert!(json.contains("cancelled by caller"), "{json}");

    // After cancel returned, the aborted task is reaped: no further events.
    let mut lines = Vec::new();
    drain_events(engine, &mut lines);
    thread::sleep(Duration::from_millis(300));
    let (code, line) = poll_big(engine);
    assert_eq!(code, AK_PENDING, "quiet after cancel: {line}");
    assert!(
        !lines.iter().any(|l| l.contains("\"event\":\"done\"")),
        "no done for a cancelled job: {lines:?}"
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn del_and_c1_control_characters_are_refused() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let src = tempfile::tempdir().unwrap();
    write_file(&src.path().join("f.bin"), b"x");
    let fp = "ab".repeat(32);
    let dead_port = free_udp_port();
    let src_field = path_jstr(src.path());
    let mut job = [0u8; 16];

    let spec_with_name = |name: &str| {
        format!(
            "{{\"source_roots\":[{src_field}],\"peer\":\"127.0.0.1:{dead_port}\",\
             \"fp_hex\":\"{fp}\",\"device_name\":\"{name}\"}}"
        )
    };
    // Escaped DEL and C1 CSI are refused (the JSON escape layer).
    assert_eq!(
        try_send(engine, &spec_with_name("a\\u007fb"), &mut job),
        AK_ERR_INVALID,
        "escaped DEL U+007F"
    );
    assert_eq!(
        try_send(engine, &spec_with_name("a\\u009bb"), &mut job),
        AK_ERR_INVALID,
        "escaped CSI U+009B"
    );
    // Raw DEL byte, and raw C1 carried as UTF-8 bytes, are refused too.
    assert_eq!(
        try_send(engine, &spec_with_name("a\u{7f}b"), &mut job),
        AK_ERR_INVALID,
        "raw DEL byte"
    );
    assert_eq!(
        try_send(engine, &spec_with_name("a\u{9b}b"), &mut job),
        AK_ERR_INVALID,
        "raw C1 (two UTF-8 bytes)"
    );
    let message = last_error_text(engine);
    assert!(
        message.contains("control"),
        "the refusal names control characters: {message}"
    );

    // peer_add shares the rule (raw chars a native caller can pass).
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    for bad in ["a\u{7f}b", "a\u{9b}b", "a\u{85}b"] {
        let (code, body) = call_peer_add(engine, &path_text, &fp_hex(0x45), Some(bad));
        assert_eq!((code, body.as_str()), (AK_ERR_INVALID, ""), "name {bad:?}");
        assert!(last_error_text(engine).contains("control"));
    }

    // Boundary: U+00A0 (NBSP) is the first NON-control above C1 and passes.
    let (code, body) = call_peer_add(engine, &path_text, &fp_hex(0x44), Some("a\u{a0}b"));
    assert_eq!(
        (code, body.as_str()),
        (AK_DONE, "{\"peers\":1}"),
        "NBSP is not a control character"
    );

    // Only the boundary-clean peer was persisted.
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(
        array_elements(&list).len(),
        1,
        "refused adds paired nothing"
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn two_engines_run_loopbacks_without_cross_talk() {
    let threads: Vec<_> = (0..2)
        .map(|index| {
            thread::spawn(move || -> Result<(), String> {
                let engine = ak_engine_new();
                if engine.is_null() {
                    return Err("engine_new failed".to_string());
                }
                let id_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
                let recv_fp = make_receiver_identity(id_dir.path());
                let base = tempfile::tempdir().map_err(|e| e.to_string())?;
                let src = base.path().join(format!("engine{index}-source"));
                let (total_bytes, total_entries) = make_source_tree(&src);
                let dst = tempfile::tempdir().map_err(|e| e.to_string())?;

                let outcome =
                    run_loopback(engine, &src, dst.path(), id_dir.path(), &recv_fp, false, "");
                let send_hex = hex16(&outcome.send_job);
                let recv_hex = hex16(&outcome.recv_job);
                for line in &outcome.lines {
                    let job = line_job(line);
                    if job != send_hex && job != recv_hex {
                        return Err(format!("cross-talk: foreign job {job} in {line}"));
                    }
                }
                if field_u64(&outcome.send_json, "bytes") != total_bytes
                    || field_u64(&outcome.send_json, "files") != total_entries
                {
                    return Err("report count mismatch".to_string());
                }
                if field_str(&outcome.send_json, "wire_job")
                    != field_str(&outcome.recv_json, "wire_job")
                {
                    return Err("wire job mismatch between sides".to_string());
                }
                assert_trees_identical(&src, dst.path());
                unsafe { ak_engine_free(engine) };
                Ok(())
            })
        })
        .collect();
    for thread in threads {
        thread
            .join()
            .expect("thread must not panic")
            .expect("engine run");
    }
}

#[test]
fn concurrent_receive_aggregates_two_senders() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let id_dir = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(id_dir.path());
    let dst = tempfile::tempdir().unwrap();

    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"any\",\"id_dir\":{},\
         \"once_jobs\":2,\"timeout_secs\":60}}",
        path_jstr(dst.path()),
        path_jstr(id_dir.path())
    );
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec).unwrap().as_ptr(),
                recv_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );

    let mut send_jobs = Vec::new();
    let mut send_bytes_total = 0u64;
    for index in 0..2usize {
        let base = tempfile::tempdir().unwrap();
        // Single-root jobs land the root's CONTENTS at the destination top
        // level, so each sender needs its own file name (a shared name
        // would be a genuine ConflictPolicy::Fail conflict).
        let src = base.path().join(format!("serve{index}"));
        let mut pattern = Pattern(0x5E12_0000_0000_0000 + index as u64);
        let payload = pattern.bytes(96 * 1024 + index * 1000);
        write_file(&src.join(format!("payload{index}.bin")), &payload);
        send_bytes_total += payload.len() as u64;
        // Keep the tempdir alive for the transfer's duration by leaking into
        // a Vec that outlives the loop below.
        let spec = format!(
            "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\
             \"fp_hex\":\"{recv_fp}\",\"chunk_kb\":32,\"timeout_secs\":60}}",
            path_jstr(&src)
        );
        let mut job = [0u8; 16];
        assert_eq!(
            unsafe {
                ak_send_start(
                    engine,
                    CString::new(spec).unwrap().as_ptr(),
                    job.as_mut_ptr(),
                )
            },
            AK_OK,
            "{}",
            last_error_text(engine)
        );
        send_jobs.push((job, base));
    }

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut lines: Vec<String> = Vec::new();
    let mut recv_json = String::new();
    let mut collected: Vec<Option<String>> = vec![None; send_jobs.len()];
    while Instant::now() < deadline {
        let (code, line) = poll_big(engine);
        if code == AK_EVENT {
            lines.push(line);
        } else {
            assert_eq!(code, AK_PENDING, "{}", last_error_text(engine));
        }
        for (index, (job, _)) in send_jobs.iter().enumerate() {
            if collected[index].is_some() {
                continue;
            }
            let (code, json) = result_big(engine, job);
            match code {
                AK_FAILED => {
                    let (_, recv_json) = result_big(engine, &recv_job);
                    panic!("sender {index} failed: {json}\nreceiver state: {recv_json}")
                }
                AK_DONE => collected[index] = Some(json),
                AK_PENDING => {}
                other => panic!("sender {index} result code {other}: {json}"),
            }
        }
        if recv_json.is_empty() {
            let (code, json) = result_big(engine, &recv_job);
            match code {
                AK_FAILED => panic!("concurrent receive failed: {json}"),
                AK_DONE => recv_json = json,
                AK_PENDING => {}
                other => panic!("receive result code {other}: {json}"),
            }
        }
        if !recv_json.is_empty() && collected.iter().all(Option::is_some) {
            // Drain any events still queued behind the terminal states.
            loop {
                let (code, line) = poll_big(engine);
                if code != AK_EVENT {
                    assert_eq!(code, AK_PENDING);
                    break;
                }
                lines.push(line);
            }
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let send_jsons: Vec<String> = collected.into_iter().flatten().collect();
    assert!(!recv_json.is_empty(), "aggregate receive did not finish");
    assert_eq!(send_jsons.len(), 2, "both senders finished");

    assert_eq!(field_u64(&recv_json, "jobs"), 2);
    assert_eq!(field_u64(&recv_json, "files"), 2);
    assert_eq!(field_u64(&recv_json, "bytes"), send_bytes_total);
    let recv_hex = hex16(&recv_job);
    assert!(
        lines.iter().any(|line| line.contains("\"conn\":0")),
        "concurrent events carry connection indices"
    );
    assert!(lines.iter().all(|line| line_job(line) == recv_hex
        || send_jobs
            .iter()
            .any(|(job, _)| line_job(line) == hex16(job))));

    unsafe { ak_engine_free(engine) };
}

// ---------------------------------------------------------------------------
// ak_identity_info / ak_jobs_list / receive expect "book".
// ---------------------------------------------------------------------------

/// Drive `ak_identity_info` through the C ABI with a large buffer.
fn call_identity_info(engine: *mut AkEngine, id_dir: &str) -> (i32, String) {
    let mut buf = [0 as c_char; 4096];
    let code = unsafe {
        ak_identity_info(
            engine,
            CString::new(id_dir).unwrap().as_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    (code, cbuf_to_string(&buf))
}

/// Drive `ak_jobs_list` through the C ABI with a large buffer.
fn call_jobs_list(engine: *mut AkEngine, id_dir: &str) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = unsafe {
        ak_jobs_list(
            engine,
            CString::new(id_dir).unwrap().as_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    (code, cbuf_to_string(&buf))
}

/// The single element of a one-element JSON array (fails loudly otherwise).
/// Elements are objects, so their own `,` separators are fine — only the
/// `},{` boundary between two elements rules out "exactly one".
fn single_element(body: &str) -> String {
    assert!(
        body.starts_with('[') && body.ends_with(']'),
        "expected a JSON array: {body}"
    );
    let inner = &body[1..body.len() - 1];
    assert!(
        !inner.is_empty() && !inner.contains("},{"),
        "expected exactly one element: {body}"
    );
    inner.to_string()
}

#[test]
fn identity_info_creates_once_and_matches_independent_key_load() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    // The id_dir does not exist yet; the call must create it.
    let base = tempfile::tempdir().unwrap();
    let id_dir = base.path().join("fresh-id");
    assert!(!id_dir.exists());

    let (code, body) = call_identity_info(engine, &id_dir.to_string_lossy());
    assert_eq!(code, AK_DONE, "identity_info: {}", last_error_text(engine));
    let device_id = field_str(&body, "device_id");
    let fingerprint = field_str(&body, "fingerprint");
    let pairing = field_str(&body, "pairing_code");
    // Exact key set, order, and shapes.
    assert_eq!(
        body,
        format!(
            "{{\"device_id\":\"{device_id}\",\"fingerprint\":\"{fingerprint}\",\
             \"pairing_code\":\"{pairing}\",\"peers\":0,\"created\":true}}"
        )
    );
    assert_eq!(device_id.len(), 64);
    assert!(device_id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(pairing.len(), 7);
    assert_eq!(pairing.as_bytes()[3], b'-');
    assert!(pairing
        .bytes()
        .enumerate()
        .all(|(i, b)| { b.is_ascii_digit() || (i == 3 && b == b'-') }));
    assert_eq!(field_u64(&body, "peers"), 0);

    assert!(id_dir.is_dir(), "id_dir was created");
    assert_eq!(
        fs::metadata(id_dir.join("device.key")).unwrap().len(),
        32,
        "32-byte seed file"
    );

    // Second call: the key exists, created flips to false, identity stable.
    let (code, again) = call_identity_info(engine, &id_dir.to_string_lossy());
    assert_eq!(code, AK_DONE);
    assert_eq!(
        again,
        body.replace("\"created\":true", "\"created\":false"),
        "only the created flag changes"
    );

    // The reported identity matches an independent load of the same file
    // (fixture-side ak-identity use, like make_receiver_identity).
    let (key, created) =
        ak_identity::DeviceKeyPair::load_or_create(&id_dir.join("device.key")).unwrap();
    assert!(!created);
    assert_eq!(key.fingerprint().to_hex(), fingerprint);
    let public_hex: String = key
        .public_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(public_hex, device_id);
    assert_eq!(ak_identity::pairing_code(&key.fingerprint()), pairing);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn identity_info_counts_peers_and_flags_corrupt_book() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();

    // No peers.book yet: zero peers, no note.
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 0);
    assert_eq!(last_error_text(engine), "", "no note for a missing book");

    // Two paired peers are counted.
    let mut book = ak_identity::PeerBook::default();
    for name in ["laptop", "phone"] {
        book.upsert(ak_identity::PeerRecord {
            fingerprint: ak_identity::DeviceKeyPair::generate().fingerprint(),
            name: name.to_owned(),
            paired_unix_ms: 1,
        });
    }
    book.save(&id_dir.path().join("peers.book")).unwrap();
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 2);

    // A corrupt book still succeeds but reports 0 with a last_error note.
    fs::write(id_dir.path().join("peers.book"), b"NOTPEERS\n").unwrap();
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE, "corrupt book does not fail the call");
    assert_eq!(field_u64(&body, "peers"), 0);
    let note = last_error_text(engine);
    assert!(
        note.contains("peers.book") && note.contains('0'),
        "note names the book and the zero: {note}"
    );

    // Removing the book restores the clean zero (and clears the note).
    fs::remove_file(id_dir.path().join("peers.book")).unwrap();
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 0);
    assert_eq!(last_error_text(engine), "");

    // Argument errors: null id_dir / empty id_dir / null buffer / tiny cap.
    let c_path = CString::new(path_text.clone()).unwrap();
    assert_eq!(
        unsafe { ak_identity_info(engine, std::ptr::null(), [0 as c_char; 64].as_mut_ptr(), 64) },
        AK_ERR_NULL
    );
    let empty = CString::new("").unwrap();
    let mut buf = [0 as c_char; 64];
    assert_eq!(
        unsafe { ak_identity_info(engine, empty.as_ptr(), buf.as_mut_ptr(), buf.len()) },
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("id_dir"));
    assert_eq!(
        unsafe { ak_identity_info(engine, c_path.as_ptr(), std::ptr::null_mut(), 64) },
        AK_ERR_NULL
    );
    let mut tiny = [0 as c_char; 2];
    assert_eq!(
        unsafe { ak_identity_info(engine, c_path.as_ptr(), tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let message = last_error_text(engine);
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );

    // An id_dir that can never be created (regular file in the way).
    let as_file = tempfile::tempdir().unwrap();
    let file_path = as_file.path().join("i-am-a-file");
    fs::write(&file_path, b"x").unwrap();
    let (code, body) = call_identity_info(engine, &file_path.to_string_lossy());
    assert_eq!(code, AK_ERR_INVALID, "bad id_dir is -2");
    assert_eq!(body, "", "nothing written on failure");

    unsafe { ak_engine_free(engine) };
}

#[test]
fn jobs_list_missing_directory_is_an_empty_array() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();

    let (code, body) = call_jobs_list(engine, &path_text);
    assert_eq!(code, AK_DONE, "jobs_list: {}", last_error_text(engine));
    assert_eq!(body, "[]");
    assert!(
        !id_dir.path().join("checkpoints").exists(),
        "a read must not create the directory"
    );

    // `[]` still honors the buffer contract: cap 2 cannot hold "[]" + NUL.
    let c_path = CString::new(path_text.clone()).unwrap();
    let mut tiny = [0 as c_char; 2];
    assert_eq!(
        unsafe { ak_jobs_list(engine, c_path.as_ptr(), tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let message = last_error_text(engine);
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );
    let (code, body) = call_jobs_list(engine, &path_text);
    assert_eq!((code, body.as_str()), (AK_DONE, "[]"), "lossless retry");

    // Argument errors.
    assert_eq!(
        unsafe { ak_jobs_list(engine, std::ptr::null(), [0 as c_char; 8].as_mut_ptr(), 8) },
        AK_ERR_NULL
    );
    let empty = CString::new("").unwrap();
    let mut buf = [0 as c_char; 8];
    assert_eq!(
        unsafe { ak_jobs_list(engine, empty.as_ptr(), buf.as_mut_ptr(), buf.len()) },
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("id_dir"));
    assert_eq!(
        unsafe { ak_jobs_list(engine, c_path.as_ptr(), std::ptr::null_mut(), 8) },
        AK_ERR_NULL
    );

    // An id_dir that is a regular file has no checkpoints directory: [].
    let as_file = tempfile::tempdir().unwrap();
    let file_path = as_file.path().join("not-a-dir");
    fs::write(&file_path, b"x").unwrap();
    let (code, body) = call_jobs_list(engine, &file_path.to_string_lossy());
    assert_eq!((code, body.as_str()), (AK_DONE, "[]"));

    unsafe { ak_engine_free(engine) };
}

#[test]
fn jobs_list_after_loopback_lists_completed_records() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let recv_id = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(recv_id.path());
    let send_id = tempfile::tempdir().unwrap();
    // make_receiver_identity is direction-agnostic: any pre-created identity.
    let send_fp = make_receiver_identity(send_id.path());

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("ffi-source");
    let (total_bytes, total_entries) = make_source_tree(&src);
    let dst = tempfile::tempdir().unwrap();

    // Both sides carry an id_dir, so both persist under <id_dir>/checkpoints.
    let extra = format!(",\"id_dir\":{}", path_jstr(send_id.path()));
    let outcome = run_loopback(
        engine,
        &src,
        dst.path(),
        recv_id.path(),
        &recv_fp,
        false,
        &extra,
    );
    let wire_job = field_str(&outcome.send_json, "wire_job");

    // Receiver side: one completed receive record naming the sender.
    // Since the engine's record-key isolation (content id + peer +
    // direction + root), the record id is the per-instance RECORD id —
    // not the wire content id, and distinct per direction.
    let (code, body) = call_jobs_list(engine, &recv_id.path().to_string_lossy());
    assert_eq!(code, AK_DONE, "{}", last_error_text(engine));
    let recv_record = single_element(&body);
    assert_eq!(
        field_str(&recv_record, "id").len(),
        32,
        "32-hex id: {recv_record}"
    );
    assert_ne!(
        field_str(&recv_record, "id"),
        wire_job,
        "the receive record is keyed per instance, not by the wire content id"
    );
    assert_eq!(field_str(&recv_record, "direction"), "receive");
    assert_eq!(field_str(&recv_record, "state"), "completed");
    assert_eq!(field_str(&recv_record, "peer"), send_fp);
    assert_eq!(field_u64(&recv_record, "files"), total_entries);
    assert_eq!(field_u64(&recv_record, "bytes"), total_bytes);
    assert!(field_u64(&recv_record, "updated_unix_ms") > 0);
    assert!(
        body.contains(&jstr(&dst.path().to_string_lossy())),
        "root is the out_dir: {body}"
    );

    // Sender side: one completed send record pinned to the receiver, with
    // its own per-instance record id (direction-isolated from the above).
    let (code, body) = call_jobs_list(engine, &send_id.path().to_string_lossy());
    assert_eq!(code, AK_DONE);
    let send_record = single_element(&body);
    assert_eq!(field_str(&send_record, "id").len(), 32);
    assert_ne!(
        field_str(&send_record, "id"),
        field_str(&recv_record, "id"),
        "send and receive records are keyed separately"
    );
    assert_eq!(field_str(&send_record, "direction"), "send");
    assert_eq!(field_str(&send_record, "state"), "completed");
    assert_eq!(field_str(&send_record, "peer"), recv_fp);
    assert!(
        body.contains(&jstr(&src.to_string_lossy())),
        "root is the source: {body}"
    );

    // Buffer contract on a populated list: a small cap refuses the whole
    // array; the retry returns the identical bytes.
    let c_path = CString::new(send_id.path().to_string_lossy().into_owned()).unwrap();
    let mut tiny = [0 as c_char; 8];
    assert_eq!(
        unsafe { ak_jobs_list(engine, c_path.as_ptr(), tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let (code, retry) = call_jobs_list(engine, &send_id.path().to_string_lossy());
    assert_eq!(code, AK_DONE);
    assert_eq!(retry, body, "retry is lossless");

    unsafe { ak_engine_free(engine) };
}

#[test]
fn receive_expect_book_accepts_paired_sender() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let recv_id = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(recv_id.path());
    let send_id = tempfile::tempdir().unwrap();
    let send_key = ak_identity::DeviceKeyPair::generate();
    send_key.save(&send_id.path().join("device.key")).unwrap();
    let send_fp = send_key.fingerprint();

    // Pre-pair: the receiver's book holds the sender's fingerprint.
    let mut book = ak_identity::PeerBook::default();
    book.upsert(ak_identity::PeerRecord {
        fingerprint: send_fp,
        name: "ffi-sender".to_owned(),
        paired_unix_ms: 42,
    });
    book.save(&recv_id.path().join("peers.book")).unwrap();

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("book-src");
    let mut pattern = Pattern(0xB00C_0000_0000_0001);
    write_file(&src.join("payload.bin"), &pattern.bytes(200 * 1024));
    let dst = tempfile::tempdir().unwrap();

    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"book\",\"id_dir\":{},\
         \"device_name\":\"book-recv\",\"timeout_secs\":30}}",
        path_jstr(dst.path()),
        path_jstr(recv_id.path())
    );
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec).unwrap().as_ptr(),
                recv_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );
    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\"fp_hex\":\"{recv_fp}\",\
         \"chunk_kb\":64,\"id_dir\":{},\"timeout_secs\":30}}",
        path_jstr(&src),
        path_jstr(send_id.path())
    );
    let mut send_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_send_start(
                engine,
                CString::new(send_spec).unwrap().as_ptr(),
                send_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );

    let outcome = pump_jobs(engine, &recv_job, &send_job, false);
    assert_eq!(field_str(&outcome.send_json, "peer"), recv_fp);
    assert_eq!(
        field_str(&outcome.recv_json, "peer"),
        send_fp.to_hex(),
        "the book-admitted sender is reported as the peer"
    );
    assert_trees_identical(&src, dst.path());

    // The admitted transfer left a completed record under the receiver's
    // checkpoints, naming the paired sender.
    let (code, body) = call_jobs_list(engine, &recv_id.path().to_string_lossy());
    assert_eq!(code, AK_DONE);
    let record = single_element(&body);
    assert_eq!(field_str(&record, "direction"), "receive");
    assert_eq!(field_str(&record, "state"), "completed");
    assert_eq!(field_str(&record, "peer"), send_fp.to_hex());

    unsafe { ak_engine_free(engine) };
}

#[test]
fn receive_expect_book_rejects_bad_books_at_start() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let out_dir = tempfile::tempdir().unwrap();
    let out_field = path_jstr(out_dir.path());
    let mut job = [0u8; 16];

    // Missing book (id_dir holds only device.key): "pair first".
    let missing = tempfile::tempdir().unwrap();
    make_receiver_identity(missing.path());
    let spec = format!(
        "{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"book\",\"id_dir\":{}}}",
        path_jstr(missing.path())
    );
    assert_eq!(try_receive(engine, &spec, &mut job), AK_ERR_INVALID);
    let message = last_error_text(engine);
    assert!(
        message.contains("no paired peers") && message.contains("pair first"),
        "pair-first hint: {message}"
    );
    assert_eq!(job, [0u8; 16], "no job id is written on failure");

    // Empty book (header only) is the same refusal.
    let empty = tempfile::tempdir().unwrap();
    make_receiver_identity(empty.path());
    fs::write(empty.path().join("peers.book"), b"AKPEERS1\n").unwrap();
    let spec = format!(
        "{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"book\",\"id_dir\":{}}}",
        path_jstr(empty.path())
    );
    assert_eq!(try_receive(engine, &spec, &mut job), AK_ERR_INVALID);
    assert!(last_error_text(engine).contains("no paired peers"));

    // Corrupt book: a load failure, still -2.
    let corrupt = tempfile::tempdir().unwrap();
    make_receiver_identity(corrupt.path());
    fs::write(
        corrupt.path().join("peers.book"),
        b"AKPEERS1\nzz\tname\t1\n",
    )
    .unwrap();
    let spec = format!(
        "{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"book\",\"id_dir\":{}}}",
        path_jstr(corrupt.path())
    );
    assert_eq!(try_receive(engine, &spec, &mut job), AK_ERR_INVALID);
    assert!(
        last_error_text(engine).contains("peer book"),
        "corrupt book error: {}",
        last_error_text(engine)
    );

    // expect "book" without an id_dir cannot resolve a book at all.
    let spec = format!("{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"book\"}}");
    assert_eq!(try_receive(engine, &spec, &mut job), AK_ERR_INVALID);
    assert!(last_error_text(engine).contains("id_dir"));

    // A bad id_dir (regular file in the way) also refuses with -2.
    let as_file = tempfile::tempdir().unwrap();
    let file_path = as_file.path().join("not-a-dir");
    fs::write(&file_path, b"x").unwrap();
    let spec = format!(
        "{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"book\",\"id_dir\":{}}}",
        path_jstr(&file_path)
    );
    assert_eq!(try_receive(engine, &spec, &mut job), AK_ERR_INVALID);

    // The other policies are unaffected: "any" still starts (then cancels).
    let any_id = tempfile::tempdir().unwrap();
    make_receiver_identity(any_id.path());
    let spec = format!(
        "{{\"out_dir\":{out_field},\"port\":0,\"expect\":\"any\",\"id_dir\":{}}}",
        path_jstr(any_id.path())
    );
    assert_eq!(try_receive(engine, &spec, &mut job), AK_OK);
    assert_eq!(unsafe { ak_job_cancel(engine, job.as_ptr()) }, AK_OK);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn receive_expect_book_rejects_unpaired_sender() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    let recv_id = tempfile::tempdir().unwrap();
    let recv_fp = make_receiver_identity(recv_id.path());
    // The book holds some OTHER device, never the sender below.
    let mut book = ak_identity::PeerBook::default();
    book.upsert(ak_identity::PeerRecord {
        fingerprint: ak_identity::DeviceKeyPair::generate().fingerprint(),
        name: "someone else".to_owned(),
        paired_unix_ms: 1,
    });
    book.save(&recv_id.path().join("peers.book")).unwrap();

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("unpaired");
    write_file(&src.join("secret.bin"), b"must not land");
    let dst = tempfile::tempdir().unwrap();

    let port = free_udp_port();
    let recv_spec = format!(
        "{{\"out_dir\":{},\"port\":{port},\"expect\":\"book\",\"id_dir\":{},\
         \"timeout_secs\":30}}",
        path_jstr(dst.path()),
        path_jstr(recv_id.path())
    );
    let mut recv_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_receive_start(
                engine,
                CString::new(recv_spec).unwrap().as_ptr(),
                recv_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );
    // No id_dir on the sender: its identity is ephemeral and unpaired.
    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\"fp_hex\":\"{recv_fp}\",\
         \"timeout_secs\":30}}",
        path_jstr(&src)
    );
    let mut send_job = [0u8; 16];
    assert_eq!(
        unsafe {
            ak_send_start(
                engine,
                CString::new(send_spec).unwrap().as_ptr(),
                send_job.as_mut_ptr(),
            )
        },
        AK_OK,
        "{}",
        last_error_text(engine)
    );

    // Both sides fail: the receiver refuses the unauthorized peer and the
    // sender sees the connection torn down before any stream opens.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut send_json, mut recv_json) = (String::new(), String::new());
    while Instant::now() < deadline {
        loop {
            let (code, _) = poll_big(engine);
            if code != AK_EVENT {
                assert_eq!(code, AK_PENDING, "{}", last_error_text(engine));
                break;
            }
        }
        if send_json.is_empty() {
            let (code, json) = result_big(engine, &send_job);
            if code != AK_PENDING {
                send_json = json;
            }
        }
        if recv_json.is_empty() {
            let (code, json) = result_big(engine, &recv_job);
            if code != AK_PENDING {
                recv_json = json;
            }
        }
        if !send_json.is_empty() && !recv_json.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!send_json.is_empty(), "sender reached a terminal state");
    assert!(!recv_json.is_empty(), "receiver reached a terminal state");
    assert!(
        send_json.contains("\"error\""),
        "sender failed: {send_json}"
    );
    assert!(
        recv_json.contains("rejected by the authorize callback"),
        "receiver names the refusal: {recv_json}"
    );
    assert!(
        !dst.path().join("unpaired").exists(),
        "nothing from an unpaired sender lands"
    );
    // The refused connection produced no checkpoint record at all.
    let (code, body) = call_jobs_list(engine, &recv_id.path().to_string_lossy());
    assert_eq!((code, body.as_str()), (AK_DONE, "[]"));

    unsafe { ak_engine_free(engine) };
}

// ---------------------------------------------------------------------------
// ak_peer_list / ak_peer_add / ak_peer_remove (paired-peer book management).
// ---------------------------------------------------------------------------

/// Drive `ak_peer_list` through the C ABI with a large buffer.
fn call_peer_list(engine: *mut AkEngine, id_dir: &str) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = unsafe {
        ak_peer_list(
            engine,
            CString::new(id_dir).unwrap().as_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    (code, cbuf_to_string(&buf))
}

/// Drive `ak_peer_add` through the C ABI; `name: None` passes a null
/// pointer (the documented "no name given" form).
fn call_peer_add(
    engine: *mut AkEngine,
    id_dir: &str,
    fingerprint_hex: &str,
    name: Option<&str>,
) -> (i32, String) {
    let c_dir = CString::new(id_dir).unwrap();
    let c_fp = CString::new(fingerprint_hex).unwrap();
    let c_name = name.map(|text| CString::new(text).unwrap());
    let mut buf = [0 as c_char; 128];
    let code = unsafe {
        ak_peer_add(
            engine,
            c_dir.as_ptr(),
            c_fp.as_ptr(),
            c_name
                .as_ref()
                .map_or(std::ptr::null(), |text| text.as_ptr()),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    (code, cbuf_to_string(&buf))
}

/// Drive `ak_peer_remove` through the C ABI.
fn call_peer_remove(engine: *mut AkEngine, id_dir: &str, fingerprint_hex: &str) -> (i32, String) {
    let c_dir = CString::new(id_dir).unwrap();
    let c_fp = CString::new(fingerprint_hex).unwrap();
    let mut buf = [0 as c_char; 128];
    let code = unsafe {
        ak_peer_remove(
            engine,
            c_dir.as_ptr(),
            c_fp.as_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    (code, cbuf_to_string(&buf))
}

/// Deterministic valid 64-hex fingerprint: one byte value repeated 32 times.
fn fp_hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

/// Parse an `ak_identity::Fingerprint` from 64 hex chars (fixture side).
fn fp_from_hex(hex: &str) -> ak_identity::Fingerprint {
    ak_identity::Fingerprint::from_hex(hex).expect("test fingerprints are valid")
}

/// Split a flat (object-element) JSON array body into its element strings.
/// Only safe for tests whose names contain no `},{` sequence.
fn array_elements(body: &str) -> Vec<String> {
    assert!(
        body.starts_with('[') && body.ends_with(']'),
        "array: {body}"
    );
    body[1..body.len() - 1]
        .split("},{")
        .map(str::to_string)
        .collect()
}

#[test]
fn peer_list_missing_and_empty_book_is_empty_array() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());

    // A completely absent id_dir: [] and the directory is NOT created.
    let base = tempfile::tempdir().unwrap();
    let id_dir = base.path().join("fresh-id");
    assert!(!id_dir.exists());
    let path_text = id_dir.to_string_lossy().into_owned();
    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE, "peer_list: {}", last_error_text(engine));
    assert_eq!(body, "[]");
    assert!(!id_dir.exists(), "a read must not create the directory");

    // A header-only book (a `pair remove` that emptied the CLI's file) too.
    fs::create_dir_all(&id_dir).unwrap();
    fs::write(id_dir.join("peers.book"), b"AKPEERS1\n").unwrap();
    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!((code, body.as_str()), (AK_DONE, "[]"));

    // `[]` still honors the buffer contract: cap 2 cannot hold "[]" + NUL.
    let c_path = CString::new(path_text.clone()).unwrap();
    let mut tiny = [0 as c_char; 2];
    assert_eq!(
        unsafe { ak_peer_list(engine, c_path.as_ptr(), tiny.as_mut_ptr(), tiny.len()) },
        AK_ERR_BUFFER
    );
    let message = last_error_text(engine);
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );
    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!((code, body.as_str()), (AK_DONE, "[]"), "lossless retry");

    // Argument errors: null id_dir / null buf / empty id_dir.
    assert_eq!(
        unsafe { ak_peer_list(engine, std::ptr::null(), [0 as c_char; 8].as_mut_ptr(), 8) },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe { ak_peer_list(engine, c_path.as_ptr(), std::ptr::null_mut(), 8) },
        AK_ERR_NULL
    );
    let empty = CString::new("").unwrap();
    let mut buf = [0 as c_char; 8];
    assert_eq!(
        unsafe { ak_peer_list(engine, empty.as_ptr(), buf.as_mut_ptr(), buf.len()) },
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("id_dir"));

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_list_parses_cli_written_book_sorted() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();

    // A book byte-for-byte in the CLI's `pair add` format: AKPEERS1 header,
    // one `fingerprint\tname\tpaired_unix_ms` line per peer, in insertion
    // (NOT fingerprint) order; names may be empty or unicode.
    let (a, b, c) = (fp_hex(0x10), fp_hex(0x42), fp_hex(0xfe));
    let text = format!("AKPEERS1\n{c}\t手机 二号\t42\n{a}\tlaptop pro\t1700000000000\n{b}\t\t7\n");
    fs::write(id_dir.path().join("peers.book"), text).unwrap();

    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE, "{}", last_error_text(engine));
    // Sorted by fingerprint; exact element shape; names verbatim (a peer
    // the CLI paired without a name keeps its empty stored name).
    assert_eq!(
        body,
        format!(
            "[{{\"fingerprint\":\"{a}\",\"name\":\"laptop pro\",\
             \"paired_unix_ms\":1700000000000}},\
             {{\"fingerprint\":\"{b}\",\"name\":\"\",\"paired_unix_ms\":7}},\
             {{\"fingerprint\":\"{c}\",\"name\":\"手机 二号\",\"paired_unix_ms\":42}}]"
        )
    );

    // A book written through ak-identity's own save (what the CLI binary
    // actually calls) parses identically.
    let mut book = ak_identity::PeerBook::default();
    book.upsert(ak_identity::PeerRecord {
        fingerprint: fp_from_hex(&a),
        name: "saved-via-ak-identity".to_owned(),
        paired_unix_ms: 5,
    });
    book.save(&id_dir.path().join("peers.book")).unwrap();
    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(
        body,
        format!(
            "[{{\"fingerprint\":\"{a}\",\"name\":\"saved-via-ak-identity\",\
             \"paired_unix_ms\":5}}]"
        )
    );

    // A stored name with JSON metacharacters is escaped, never injected.
    fs::write(
        id_dir.path().join("peers.book"),
        format!("AKPEERS1\n{b}\tquote\" back\\slash\t9\n"),
    )
    .unwrap();
    let (code, body) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(
        body,
        format!(
            "[{{\"fingerprint\":\"{b}\",\"name\":\"quote\\\" back\\\\slash\",\
             \"paired_unix_ms\":9}}]"
        )
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_list_corrupt_book_is_invalid() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let fp = fp_hex(0x01);

    let cases: Vec<(&str, String)> = vec![
        ("wrong header", "NOTPEERS\n".to_owned()),
        ("bad fingerprint line", "AKPEERS1\nzz\tname\t1\n".to_owned()),
        ("bad timestamp", format!("AKPEERS1\n{fp}\tname\tnope\n")),
        ("missing fields", format!("AKPEERS1\n{fp}\tname\n")),
    ];
    let mut last_content = String::new();
    for (label, content) in cases {
        fs::write(id_dir.path().join("peers.book"), &content).unwrap();
        last_content = content;
        let (code, body) = call_peer_list(engine, &path_text);
        assert_eq!(code, AK_ERR_INVALID, "case {label}");
        assert_eq!(body, "", "nothing written on failure");
        let message = last_error_text(engine);
        assert!(
            message.contains("cannot load peer book"),
            "case {label}: {message}"
        );
    }

    // The writers refuse the same corrupt book instead of clobbering it.
    let (code, body) = call_peer_add(engine, &path_text, &fp, Some("x"));
    assert_eq!((code, body.as_str()), (AK_ERR_INVALID, ""));
    assert!(last_error_text(engine).contains("cannot load peer book"));
    let (code, body) = call_peer_remove(engine, &path_text, &fp);
    assert_eq!((code, body.as_str()), (AK_ERR_INVALID, ""));
    assert!(last_error_text(engine).contains("cannot load peer book"));
    assert_eq!(
        fs::read_to_string(id_dir.path().join("peers.book")).unwrap(),
        last_content,
        "the corrupt book is left untouched"
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_add_pairs_new_peers_with_name_or_unnamed_default() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let (a, b, c) = (fp_hex(0xaa), fp_hex(0xbb), fp_hex(0xcc));

    // Named first peer: {"peers":1}, persisted in the CLI's AKPEERS1 format.
    let (code, body) = call_peer_add(engine, &path_text, &a, Some("laptop"));
    assert_eq!(code, AK_DONE, "add: {}", last_error_text(engine));
    assert_eq!(body, "{\"peers\":1}");
    let saved = fs::read_to_string(id_dir.path().join("peers.book")).unwrap();
    assert!(saved.starts_with("AKPEERS1\n"), "CLI header: {saved:?}");
    assert_eq!(saved.lines().count(), 2, "header + one entry");
    // Round-trips through ak-identity (i.e. the CLI reads what the FFI wrote).
    let book = ak_identity::PeerBook::load(&id_dir.path().join("peers.book")).unwrap();
    let record = book.get(&fp_from_hex(&a)).unwrap();
    assert_eq!(record.name, "laptop");
    assert!(record.paired_unix_ms > 0, "pairing time stamped");

    // Second peer with a null name and third with an empty string: both
    // mean "no name given" and store "(unnamed)".
    let (code, body) = call_peer_add(engine, &path_text, &b, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":2}"));
    let (code, body) = call_peer_add(engine, &path_text, &c, Some(""));
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":3}"));

    // The list reflects all three, sorted, with the documented default.
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    let elements = array_elements(&list);
    assert_eq!(elements.len(), 3);
    assert!(elements[0].contains(&format!("\"fingerprint\":\"{a}\"")));
    assert!(elements[0].contains("\"name\":\"laptop\""));
    assert!(elements[1].contains(&format!("\"fingerprint\":\"{b}\"")));
    assert!(elements[1].contains("\"name\":\"(unnamed)\""));
    assert!(elements[2].contains(&format!("\"fingerprint\":\"{c}\"")));
    assert!(elements[2].contains("\"name\":\"(unnamed)\""));
    for element in &elements {
        assert!(field_u64(element, "paired_unix_ms") > 0);
    }

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_add_upsert_keeps_original_pairing_time_like_cli() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let fp = fp_hex(0x33);

    // Seed the book the way the CLI paired this peer long ago: a fixed
    // ancient timestamp that must survive every FFI re-add below.
    fs::write(
        id_dir.path().join("peers.book"),
        format!("AKPEERS1\n{fp}\told name\t1000\n"),
    )
    .unwrap();

    // Re-add with a new name: renamed, pairing time preserved, count stays.
    let (code, body) = call_peer_add(engine, &path_text, &fp, Some("renamed"));
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    let element = single_element(&list);
    assert_eq!(field_str(&element, "name"), "renamed");
    assert_eq!(
        field_u64(&element, "paired_unix_ms"),
        1000,
        "CLI TOFU: the original pairing time is kept on rename"
    );

    // Re-add with an empty name: the record is left untouched.
    let (code, body) = call_peer_add(engine, &path_text, &fp, Some(""));
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));
    let (_, list) = call_peer_list(engine, &path_text);
    let element = single_element(&list);
    assert_eq!(
        field_str(&element, "name"),
        "renamed",
        "a no-name re-add never renames"
    );
    assert_eq!(field_u64(&element, "paired_unix_ms"), 1000);

    // Same for a null name.
    let (code, body) = call_peer_add(engine, &path_text, &fp, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));

    // The on-disk book still matches what ak-identity (the CLI) expects.
    let book = ak_identity::PeerBook::load(&id_dir.path().join("peers.book")).unwrap();
    let record = book.get(&fp_from_hex(&fp)).unwrap();
    assert_eq!(
        (record.name.as_str(), record.paired_unix_ms),
        ("renamed", 1000)
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_add_rejects_bad_fingerprints_and_control_names() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let good = fp_hex(0x77);

    // Bad fingerprints: wrong length, non-hex, empty.
    let bad_fps = [
        "".to_owned(),
        "ab".to_owned(),
        good[..63].to_owned(),
        format!("{good}0"),
        good.replace('7', "g"),
    ];
    for bad in &bad_fps {
        let (code, body) = call_peer_add(engine, &path_text, bad, Some("x"));
        assert_eq!(code, AK_ERR_INVALID, "fingerprint {bad:?}");
        assert_eq!(body, "");
        assert!(last_error_text(engine).contains("64 hex"));
    }
    assert!(
        !id_dir.path().join("peers.book").exists(),
        "nothing persisted on a rejected add"
    );

    // Control characters in names are refused outright (raw bytes under
    // 0x20 — the ABI never sanitizes silently).
    for bad_name in ["two\nlines", "tab\there", "carriage\rreturn", "\u{1}beep"] {
        let (code, body) = call_peer_add(engine, &path_text, &good, Some(bad_name));
        assert_eq!(
            (code, body.as_str()),
            (AK_ERR_INVALID, ""),
            "name {bad_name:?}"
        );
        assert!(last_error_text(engine).contains("control"));
    }
    assert!(!id_dir.path().join("peers.book").exists());

    // Oversized name (beyond the ABI-wide 4096-byte string cap).
    let huge = "x".repeat(4097);
    let (code, _) = call_peer_add(engine, &path_text, &good, Some(&huge));
    assert_eq!(code, AK_ERR_INVALID);

    // Null arguments: id_dir and fingerprint are required; `name` is the
    // documented optional one. A null buf is refused BEFORE anything is
    // persisted.
    let c_dir = CString::new(path_text.clone()).unwrap();
    let c_good = CString::new(good.clone()).unwrap();
    assert_eq!(
        unsafe {
            ak_peer_add(
                engine,
                c_dir.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe {
            ak_peer_add(
                engine,
                std::ptr::null(),
                c_good.as_ptr(),
                std::ptr::null(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe {
            ak_peer_add(
                engine,
                c_dir.as_ptr(),
                c_good.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                64,
            )
        },
        AK_ERR_NULL
    );
    assert!(
        !id_dir.path().join("peers.book").exists(),
        "a null buf persists nothing"
    );

    // Empty id_dir.
    let empty = CString::new("").unwrap();
    assert_eq!(
        unsafe {
            ak_peer_add(
                engine,
                empty.as_ptr(),
                c_good.as_ptr(),
                std::ptr::null(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_INVALID
    );
    assert!(last_error_text(engine).contains("id_dir"));

    // After all that, one valid add still works.
    let (code, body) = call_peer_add(engine, &path_text, &good, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_remove_deletes_existing_and_reports_missing() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let (a, b) = (fp_hex(0x51), fp_hex(0x52));

    let (code, _) = call_peer_add(engine, &path_text, &a, Some("one"));
    assert_eq!(code, AK_DONE);
    let (code, _) = call_peer_add(engine, &path_text, &b, Some("two"));
    assert_eq!(code, AK_DONE);

    // Remove an existing peer: count JSON, atomic rewrite, CLI-readable.
    let (code, body) = call_peer_remove(engine, &path_text, &a);
    assert_eq!(code, AK_DONE, "remove: {}", last_error_text(engine));
    assert_eq!(body, "{\"peers\":1}");
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    let element = single_element(&list);
    assert_eq!(field_str(&element, "fingerprint"), b);
    let book = ak_identity::PeerBook::load(&id_dir.path().join("peers.book")).unwrap();
    assert!(book.get(&fp_from_hex(&a)).is_none());
    assert_eq!(book.iter().count(), 1);

    // Removing it again: NOT_FOUND with a hint in the last-error buffer.
    let (code, body) = call_peer_remove(engine, &path_text, &a);
    assert_eq!(code, AK_ERR_NOT_FOUND);
    assert_eq!(body, "");
    let message = last_error_text(engine);
    assert!(
        message.contains("not in") && message.contains("nothing removed"),
        "hint: {message}"
    );

    // A book that never existed has no peers either: same NOT_FOUND, and
    // still nothing is created on disk.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_text = fresh.path().to_string_lossy().into_owned();
    let (code, _) = call_peer_remove(engine, &fresh_text, &b);
    assert_eq!(code, AK_ERR_NOT_FOUND);
    assert!(!fresh.path().join("peers.book").exists());

    // Bad fingerprint / null arguments / empty id_dir.
    let (code, _) = call_peer_remove(engine, &path_text, "nope");
    assert_eq!(code, AK_ERR_INVALID);
    assert!(last_error_text(engine).contains("64 hex"));
    let c_dir = CString::new(path_text.clone()).unwrap();
    let c_b = CString::new(b.clone()).unwrap();
    assert_eq!(
        unsafe {
            ak_peer_remove(
                engine,
                std::ptr::null(),
                c_b.as_ptr(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe {
            ak_peer_remove(
                engine,
                c_dir.as_ptr(),
                std::ptr::null(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_NULL
    );
    assert_eq!(
        unsafe {
            ak_peer_remove(
                engine,
                c_dir.as_ptr(),
                c_b.as_ptr(),
                std::ptr::null_mut(),
                16,
            )
        },
        AK_ERR_NULL
    );
    let empty = CString::new("").unwrap();
    assert_eq!(
        unsafe {
            ak_peer_remove(
                engine,
                empty.as_ptr(),
                c_b.as_ptr(),
                [0 as c_char; 16].as_mut_ptr(),
                16,
            )
        },
        AK_ERR_INVALID
    );

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_add_and_remove_feed_identity_info_peer_count() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let base = tempfile::tempdir().unwrap();
    let id_dir = base.path().join("id");
    let path_text = id_dir.to_string_lossy().into_owned();
    let (a, b) = (fp_hex(0x64), fp_hex(0x65));

    // A brand-new id_dir: identity_info creates it and reports zero peers.
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 0);

    let (code, body) = call_peer_add(engine, &path_text, &a, Some("laptop"));
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));
    let (code, body) = call_peer_add(engine, &path_text, &b, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":2}"));

    // identity_info counts the FFI-added peers (same peers.book).
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 2);

    let (code, body) = call_peer_remove(engine, &path_text, &b);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));
    let (code, body) = call_identity_info(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert_eq!(field_u64(&body, "peers"), 1);

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_writers_pre_flight_the_buffer_before_persisting() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let (a, b) = (fp_hex(0x90), fp_hex(0x91));
    let c_dir = CString::new(path_text.clone()).unwrap();
    let c_b = CString::new(b.clone()).unwrap();
    // {"peers":N} for N in 0..=9 is 11 bytes; 12 with the NUL terminator.
    let mut tiny = [0 as c_char; 11];

    let (code, _) = call_peer_add(engine, &path_text, &a, Some("seed"));
    assert_eq!(code, AK_DONE);

    // ADD with a too-small cap: refused BEFORE the save, so the book is
    // untouched and the retry is a plain fresh call.
    let code = unsafe {
        ak_peer_add(
            engine,
            c_dir.as_ptr(),
            c_b.as_ptr(),
            std::ptr::null(),
            tiny.as_mut_ptr(),
            tiny.len(),
        )
    };
    assert_eq!(code, AK_ERR_BUFFER);
    let message = last_error_text(engine);
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    let element = single_element(&list);
    assert_eq!(
        field_str(&element, "fingerprint"),
        a,
        "the refused add paired nothing"
    );

    // The retry with a large buffer succeeds; re-running the add is
    // idempotent anyway.
    let (code, body) = call_peer_add(engine, &path_text, &b, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":2}"));

    // REMOVE with a too-small cap: nothing removed, retry then works.
    let code = unsafe {
        ak_peer_remove(
            engine,
            c_dir.as_ptr(),
            c_b.as_ptr(),
            tiny.as_mut_ptr(),
            tiny.len(),
        )
    };
    assert_eq!(code, AK_ERR_BUFFER);
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    assert!(
        list.contains(&b),
        "the peer survived the refused remove: {list}"
    );
    let (code, body) = call_peer_remove(engine, &path_text, &b);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":1}"));

    unsafe { ak_engine_free(engine) };
}

#[test]
fn peer_writer_save_failure_is_internal_and_leaves_book_intact() {
    let engine = ak_engine_new();
    assert!(!engine.is_null());
    let id_dir = tempfile::tempdir().unwrap();
    let path_text = id_dir.path().to_string_lossy().into_owned();
    let (a, b) = (fp_hex(0xa0), fp_hex(0xa1));

    let (code, _) = call_peer_add(engine, &path_text, &a, Some("first"));
    assert_eq!(code, AK_DONE);

    // Block the atomic-write temp slots: ak-identity saves via a
    // per-process unique `peers.book.<pid>.<seq>.tmp` created with
    // O_EXCL/create_new, so pre-creating a range of seq slots as
    // DIRECTORIES makes every save fail (opening a directory for write is
    // an error on every platform) whichever seq comes next. Must surface
    // as AK_ERR_INTERNAL naming the book.
    let pid = std::process::id();
    const BLOCKED_SEQS: u32 = 512;
    for seq in 0..BLOCKED_SEQS {
        let _ = fs::create_dir(id_dir.path().join(format!("peers.book.{pid}.{seq}.tmp")));
    }
    let (code, body) = call_peer_add(engine, &path_text, &b, Some("second"));
    assert_eq!(code, AK_ERR_INTERNAL);
    assert_eq!(body, "");
    let message = last_error_text(engine);
    assert!(
        message.contains("cannot save peer book"),
        "save failure names the book: {message}"
    );

    // Remove shares the save path.
    let (code, body) = call_peer_remove(engine, &path_text, &a);
    assert_eq!((code, body.as_str()), (AK_ERR_INTERNAL, ""));

    // The book on disk is untouched: still exactly the first peer.
    let (code, list) = call_peer_list(engine, &path_text);
    assert_eq!(code, AK_DONE);
    let element = single_element(&list);
    assert_eq!(field_str(&element, "fingerprint"), a);

    // Removing the blockers restores writability.
    for seq in 0..BLOCKED_SEQS {
        let _ = fs::remove_dir(id_dir.path().join(format!("peers.book.{pid}.{seq}.tmp")));
    }
    let (code, body) = call_peer_add(engine, &path_text, &b, None);
    assert_eq!((code, body.as_str()), (AK_DONE, "{\"peers\":2}"));

    unsafe { ak_engine_free(engine) };
}
