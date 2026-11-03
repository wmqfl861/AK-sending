//! End-to-end tests for the ak-ffi C ABI. Every test drives the library
//! ONLY through the exported `ak_*` functions (never `core` directly):
//! real loopback QUIC transfers crossing the C boundary, spec-JSON
//! validation, cancellation, buffer-capacity handling, and two engines
//! running concurrently. Fixture preparation (writing source files,
//! pre-creating a receiver device key so its fingerprint is known) uses
//! plain `std`/`ak-identity` — the transfers themselves never bypass the
//! ABI.
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
    ak_engine_free, ak_engine_last_error, ak_engine_new, ak_job_cancel, ak_job_result,
    ak_poll_progress, ak_receive_start, ak_send_start, ak_version, AkEngine, AK_DONE,
    AK_ERR_BUFFER, AK_ERR_INVALID, AK_ERR_NOT_FOUND, AK_ERR_NULL, AK_EVENT, AK_FAILED, AK_OK,
    AK_PENDING,
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
    let code = ak_engine_last_error(engine, buf.as_mut_ptr(), buf.len());
    assert!(code >= 0, "last_error itself failed: {code}");
    cbuf_to_string(&buf)
}

fn poll_big(engine: *mut AkEngine) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = ak_poll_progress(engine, buf.as_mut_ptr(), buf.len());
    (code, cbuf_to_string(&buf))
}

fn result_big(engine: *mut AkEngine, job: &[u8; 16]) -> (i32, String) {
    let mut buf = [0 as c_char; 8192];
    let code = ak_job_result(engine, job.as_ptr(), buf.as_mut_ptr(), buf.len());
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
    let code = ak_receive_start(
        engine,
        CString::new(recv_spec).unwrap().as_ptr(),
        recv_job.as_mut_ptr(),
    );
    assert_eq!(code, AK_OK, "receive_start: {}", last_error_text(engine));

    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\
         \"fp_hex\":\"{recv_fp_hex}\",\"chunk_kb\":64,\
         \"device_name\":\"ffi-send\",\"timeout_secs\":30{extra_send_fields}}}",
        path_jstr(src)
    );
    let mut send_job = [0u8; 16];
    let code = ak_send_start(
        engine,
        CString::new(send_spec).unwrap().as_ptr(),
        send_job.as_mut_ptr(),
    );
    assert_eq!(code, AK_OK, "send_start: {}", last_error_text(engine));
    assert_ne!(send_job, [0u8; 16], "job ids must never be all-zero");
    assert_ne!(send_job, recv_job, "each job gets its own id");

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
            let code = ak_poll_progress(engine, tiny.as_mut_ptr(), tiny.len());
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
        let code = ak_job_result(engine, send_job.as_ptr(), tiny.as_mut_ptr(), tiny.len());
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
    let code = ak_version(buf.as_mut_ptr(), buf.len());
    assert_eq!(code, expected.len() as i32);
    assert_eq!(cbuf_to_string(&buf), expected);
    assert_eq!(buf[code as usize], 0, "NUL terminator right after payload");

    // Too-small capacity: specific negative code, capacity hint, lossless.
    let mut tiny = [0 as c_char; 4];
    assert_eq!(ak_version(tiny.as_mut_ptr(), tiny.len()), AK_ERR_BUFFER);
    let message = last_error_text(std::ptr::null_mut());
    assert!(
        message.contains("buffer too small") && message.contains("need"),
        "capacity hint: {message}"
    );
    // Reading does not clear the message (other entry points do).
    let mut buf = [0 as c_char; 256];
    let code = ak_engine_last_error(std::ptr::null(), buf.as_mut_ptr(), buf.len());
    assert_eq!(code as usize, message.len(), "full message written");
    assert_eq!(cbuf_to_string(&buf), message, "message survives reading");
    // Null buffer.
    assert_eq!(ak_version(std::ptr::null_mut(), 128), AK_ERR_NULL);

    // A fresh successful call clears the slot.
    let mut buf = [0 as c_char; 64];
    assert_eq!(
        ak_version(buf.as_mut_ptr(), buf.len()),
        expected.len() as i32
    );
    let code = ak_engine_last_error(std::ptr::null(), buf.as_mut_ptr(), buf.len());
    assert_eq!(code, 0, "successful calls clear the last error");
    assert_eq!(cbuf_to_string(&buf), "");

    // Engine lifecycle.
    let engine = ak_engine_new();
    assert!(!engine.is_null(), "engine_new must return a handle");
    ak_engine_free(std::ptr::null_mut()); // no-op, must not crash
    ak_engine_free(engine);
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
        ak_job_result(engine, bogus.as_ptr(), [0 as c_char; 64].as_mut_ptr(), 64),
        AK_ERR_NOT_FOUND
    );
    assert_eq!(ak_job_cancel(engine, bogus.as_ptr()), AK_ERR_NOT_FOUND);

    ak_engine_free(engine);
}

/// `ak_send_start` with error-message sanity, usable many times per test.
fn try_send(engine: *mut AkEngine, spec: &str, job: &mut [u8; 16]) -> i32 {
    let code = ak_send_start(
        engine,
        CString::new(spec).unwrap().as_ptr(),
        job.as_mut_ptr(),
    );
    if code < 0 {
        assert!(!last_error_text(engine).is_empty(), "errors set last_error");
    }
    code
}

fn try_receive(engine: *mut AkEngine, spec: &str, job: &mut [u8; 16]) -> i32 {
    let code = ak_receive_start(
        engine,
        CString::new(spec).unwrap().as_ptr(),
        job.as_mut_ptr(),
    );
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
        ak_send_start(engine, std::ptr::null(), job.as_mut_ptr()),
        AK_ERR_NULL
    );
    assert_eq!(
        ak_receive_start(engine, std::ptr::null(), job.as_mut_ptr()),
        AK_ERR_NULL
    );
    let ok_spec = CString::new(good).unwrap();
    assert_eq!(
        ak_send_start(engine, ok_spec.as_ptr(), std::ptr::null_mut()),
        AK_ERR_NULL
    );

    // Invalid UTF-8 in the C string.
    let bad_utf8: [c_char; 3] = [0xC3u8 as c_char, 0x28u8 as c_char, 0];
    assert_eq!(
        ak_send_start(engine, bad_utf8.as_ptr(), job.as_mut_ptr()),
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
    assert_eq!(ak_job_cancel(engine, job.as_ptr()), AK_OK);
    let (code, json) = result_big(engine, &job);
    assert_eq!(code, AK_FAILED, "cancelled job reports failure");
    assert!(json.contains("cancelled by caller"), "body: {json}");

    ak_engine_free(engine);
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
        ak_receive_start(
            engine,
            CString::new(recv_spec).unwrap().as_ptr(),
            recv_job.as_mut_ptr()
        ),
        AK_OK
    );
    let send_spec = format!(
        "{{\"source_roots\":[{}],\"peer\":\"127.0.0.1:{port}\",\"fp_hex\":\"{recv_fp}\",\
         \"chunk_kb\":64,\"timeout_secs\":60}}",
        path_jstr(&src)
    );
    let mut send_job = [0u8; 16];
    assert_eq!(
        ak_send_start(
            engine,
            CString::new(send_spec).unwrap().as_ptr(),
            send_job.as_mut_ptr()
        ),
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
    assert_eq!(ak_job_cancel(engine, send_job.as_ptr()), AK_OK);
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

    ak_engine_free(engine);
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
                ak_engine_free(engine);
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
        ak_receive_start(
            engine,
            CString::new(recv_spec).unwrap().as_ptr(),
            recv_job.as_mut_ptr()
        ),
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
            ak_send_start(
                engine,
                CString::new(spec).unwrap().as_ptr(),
                job.as_mut_ptr()
            ),
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

    ak_engine_free(engine);
}
