//! Safe core of the C ABI: everything below the pointer shim lives here and
//! this module is compiled with `forbid(unsafe_code)`. It owns
//!
//! - [`AkEngine`]: the handle type behind `*mut AkEngine`, embedding a
//!   dedicated 2-worker tokio runtime plus a job table
//!   (`Mutex<HashMap<[u8; 16], Arc<Job>>>`) and one engine-wide progress
//!   queue of ready-to-ship JSON lines;
//! - the hand-written minimal JSON parser/serializer ([`json`]): unknown
//!   fields are ignored (forward compatibility), type mismatches are
//!   errors, and string values never carry control characters (the same
//!   two-layer injection defense the wire codec uses: reject at the parse
//!   layer, never sanitize silently);
//! - JSON -> [`SendSpec`]/[`ReceiveSpec`] construction, assembled strictly
//!   from the already-verified `ak-engine` building blocks;
//! - serialization of engine [`Progress`] / [`Report`] values into the
//!   compact lowercase-key JSON lines the ABI documents.
//!
//! Panic containment, honest scope (C10): panics from any task spawned
//! here stay inside the runtime. Every export additionally wraps its SAFE
//! body — the logic call between the pointer reads and the buffer
//! write-backs — in [`guarded`], which turns an ordinary unwinding panic
//! into an `AK_ERR_INTERNAL` status plus message. That is the whole
//! guarantee: the pointer adapters, the last-error TLS bookkeeping, and
//! `ak_engine_free`'s destructor run OUTSIDE any catch, and no catch can
//! recover a fail-fast death (access violation, `panic = "abort"` or OOM
//! abort). It is error reporting, never a memory-safety promise.
#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ak_core::handle::ConflictPolicy;
use ak_core::policy::DirectPolicy;
use ak_engine::{
    receive_with_addr, send, serve_concurrently, EngineError, Progress, ReceiveSpec, Report,
    SendSpec,
};
use ak_identity::tls::{make_self_signed, TlsBundle};
use ak_identity::{
    pairing_code, verify_pairing_code, DeviceKeyPair, Fingerprint, PeerBook, PeerRecord,
};
use ak_store::{CheckpointStore, Direction, FileCheckpointStore};

// ---------------------------------------------------------------------------
// Status codes shared with the shim (documented in the crate header).
// ---------------------------------------------------------------------------

/// Call succeeded.
pub const AK_OK: i32 = 0;
/// `ak_poll_progress`: one event line was written to the caller buffer.
pub const AK_EVENT: i32 = 1;
/// `ak_job_result`: the job finished successfully (report JSON written).
pub const AK_DONE: i32 = 1;
/// `ak_job_result`: still running / `ak_poll_progress`: queue empty.
pub const AK_PENDING: i32 = 0;
/// `ak_job_result`: the job finished with an error (error JSON written).
pub const AK_FAILED: i32 = 2;
/// A required pointer argument was null.
pub const AK_ERR_NULL: i32 = -1;
/// Invalid input: malformed JSON, wrong field type, failed validation.
pub const AK_ERR_INVALID: i32 = -2;
/// Caller buffer too small; the operation is retryable with a larger cap
/// (the pending output was NOT consumed) and `ak_engine_last_error` names
/// the needed capacity.
pub const AK_ERR_BUFFER: i32 = -3;
/// The job id is unknown to this engine handle.
pub const AK_ERR_NOT_FOUND: i32 = -4;
/// Internal failure: lock poisoning, runtime build failure (C13: the
/// runtime builder's OS reason travels in the last-error message), or an
/// ordinary unwinding panic caught by [`guarded`] — fail-fast faults are
/// NOT catchable and kill the process instead of producing this code.
pub const AK_ERR_INTERNAL: i32 = -5;

/// This crate's C ABI revision (not the wire protocol version).
pub const FFI_API_VERSION: u16 = 1;

/// An error with its ABI status code; the message goes to the thread-local
/// last-error buffer.
#[derive(Debug, Clone)]
pub struct FfiError {
    pub code: i32,
    pub message: String,
}

impl FfiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        FfiError {
            code: AK_ERR_INVALID,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        FfiError {
            code: AK_ERR_NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        FfiError {
            code: AK_ERR_INTERNAL,
            message: message.into(),
        }
    }
}

/// Run a safe body, converting an ordinary unwinding panic into
/// `AK_ERR_INTERNAL` instead of letting it unwind across the C boundary
/// (which would be UB). Honest scope (C10): the catch covers ONLY the
/// closure — the pointer adapters (caller-buffer reads and write-backs)
/// and the last-error TLS bookkeeping run in the shim OUTSIDE this guard,
/// `ak_engine_free`'s destructor path has no catch at all, and no
/// `catch_unwind` can intercept a fail-fast death: an access violation,
/// an aborting panic (`panic = "abort"` builds), or an allocation-failure
/// abort kills the process regardless. This is error reporting, not a
/// memory-safety guarantee. `AssertUnwindSafe` is honest here: a panic
/// mid-call may leave engine bookkeeping inconsistent, but every structure
/// is mutex-guarded and the worst case is a poisoned lock surfaced as
/// `AK_ERR_INTERNAL` on later calls.
pub fn guarded<T>(body: impl FnOnce() -> Result<T, FfiError>) -> Result<T, FfiError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(FfiError::internal(format!("internal panic: {message}")))
        }
    }
}

// ---------------------------------------------------------------------------
// Thread-local last error.
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_ERROR: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Clear the thread-local message. The shim does this at every entry point
/// so the message always describes the most recent call on this thread.
pub fn clear_last_error() {
    LAST_ERROR.with(|slot| slot.borrow_mut().clear());
}

/// Record the thread-local message for the current error return.
pub fn set_last_error(message: impl Into<String>) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = message.into());
}

/// Snapshot of the thread-local message (empty when the last call on this
/// thread succeeded).
pub fn last_error_text() -> String {
    LAST_ERROR.with(|slot| slot.borrow().clone())
}

/// Version string written by `ak_version`, built from the `ak-wire`
/// constants so it can never drift from the protocol implementation.
pub fn version_string() -> String {
    let caps = ak_wire::CAP_RESUME | ak_wire::CAP_PAUSE_RESUME | ak_wire::CAP_MULTISTREAM;
    format!(
        "ak-ffi {FFI_API_VERSION} (wire v{}, caps 0x{caps:04x})",
        ak_wire::VERSION
    )
}

// ---------------------------------------------------------------------------
// Identity + checkpoint inspection (safe layer behind `ak_identity_info`
// and `ak_jobs_list`; neither needs the engine — both read the ak-cli
// on-disk conventions under `id_dir`).
// ---------------------------------------------------------------------------

/// Render the identity summary JSON for `id_dir` (schema in the crate
/// header): creates the directory and `device.key` when missing, counts
/// `peers.book` entries, and derives the pairing code.
///
/// A missing `peers.book` is simply zero peers; a CORRUPT one also reports
/// zero but leaves a note in the thread-local last-error buffer while the
/// call itself still succeeds (the identity half of the answer is sound).
pub fn identity_info_json(id_dir: &str) -> Result<String, FfiError> {
    if id_dir.is_empty() {
        return Err(FfiError::invalid("id_dir must be a non-empty path"));
    }
    let dir = PathBuf::from(id_dir);
    std::fs::create_dir_all(&dir).map_err(|error| {
        FfiError::invalid(format!("cannot create id_dir {}: {error}", dir.display()))
    })?;
    let key_path = dir.join("device.key");
    let (key, created) = DeviceKeyPair::load_or_create(&key_path).map_err(|error| {
        FfiError::invalid(format!(
            "cannot load device key at {}: {error}",
            key_path.display()
        ))
    })?;
    let fingerprint = key.fingerprint();
    let book_path = dir.join("peers.book");
    let peers = match PeerBook::load(&book_path) {
        Ok(book) => book.iter().count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            set_last_error(format!(
                "note: peers.book at {} is unreadable; reporting peers as 0: {error}",
                book_path.display()
            ));
            0
        }
    };
    Ok(format!(
        "{{\"device_id\":\"{}\",\"fingerprint\":\"{}\",\"pairing_code\":\"{}\",\
         \"peers\":{peers},\"created\":{}}}",
        hex_encode(&key.public_key()),
        fingerprint.to_hex(),
        pairing_code(&fingerprint),
        created
    ))
}

/// Render the jobs list JSON for `id_dir` (schema in the crate header): one
/// array element per parsable `<id_dir>/checkpoints/*.akjob` record, sorted
/// by job id (the store's order). A missing checkpoint directory is an
/// empty array; the whole array is rendered before any buffer write so an
/// `AK_ERR_BUFFER` retry is simply a fresh, lossless call.
pub fn jobs_list_json(id_dir: &str) -> Result<String, FfiError> {
    if id_dir.is_empty() {
        return Err(FfiError::invalid("id_dir must be a non-empty path"));
    }
    let dir = PathBuf::from(id_dir).join("checkpoints");
    if !dir.exists() {
        return Ok("[]".to_string());
    }
    let store = FileCheckpointStore::open(&dir).map_err(|error| {
        FfiError::invalid(format!(
            "cannot open checkpoint directory {}: {error}",
            dir.display()
        ))
    })?;
    let records = store.list().map_err(|error| {
        FfiError::invalid(format!(
            "cannot list checkpoint directory {}: {error}",
            dir.display()
        ))
    })?;
    let mut out = String::from("[");
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let direction = match record.direction {
            Direction::Send => "send",
            Direction::Receive => "receive",
        };
        let peer = match &record.peer_fingerprint {
            Some(fingerprint) => fingerprint.to_hex(),
            None => String::new(),
        };
        out.push_str(&format!(
            "{{\"id\":\"{}\",\"direction\":\"{direction}\",\"state\":{},\
             \"files\":{},\"bytes\":{},\"updated_unix_ms\":{},\"peer\":{},\"root\":{}}}",
            hex_encode(&record.job_id),
            json::quoted(&record.state),
            record.total_files,
            record.total_bytes,
            record.updated_unix_ms,
            json::quoted(&peer),
            json::quoted(&record.root.to_string_lossy()),
        ));
    }
    out.push(']');
    Ok(out)
}

// ---------------------------------------------------------------------------
// Peer book management (safe layer behind `ak_peer_list`, `ak_peer_add`,
// and `ak_peer_remove`; the same `<id_dir>/peers.book` the CLI's `pair`
// subcommands and the `expect: "book"` admission policy read and write).
// ---------------------------------------------------------------------------

/// Stored name for a peer paired without one (the CLI renders empty names
/// this way in `pair list`; the FFI bakes it in at write time).
const UNNAMED_PEER: &str = "(unnamed)";

/// Current wall clock in Unix milliseconds (mirrors the CLI helper).
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// The count JSON the writers return: `{"peers":N}` with the book size
/// after the operation.
fn peers_count_json(peers: usize) -> String {
    format!("{{\"peers\":{peers}}}")
}

/// The retryable buffer error for the writers. Raised BEFORE anything is
/// persisted, so `AK_ERR_BUFFER` leaves the book untouched; the message
/// mirrors the shim's capacity-hint wording.
fn buffer_too_small(need: usize, capacity: usize, what: &str) -> FfiError {
    FfiError {
        code: AK_ERR_BUFFER,
        message: format!(
            "buffer too small for {what}: need {need} bytes including the NUL terminator \
             (capacity {capacity}); retry with a larger buffer"
        ),
    }
}

/// Load `<id_dir>/peers.book` with the CLI convention: a missing file is a
/// fresh empty book, a corrupt file is a load error ("cannot load peer
/// book"), an empty `id_dir` is invalid.
fn load_peer_book(id_dir: &str) -> Result<(PathBuf, PeerBook), FfiError> {
    if id_dir.is_empty() {
        return Err(FfiError::invalid("id_dir must be a non-empty path"));
    }
    let path = PathBuf::from(id_dir).join("peers.book");
    match PeerBook::load(&path) {
        Ok(book) => Ok((path, book)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((path, PeerBook::default()))
        }
        Err(error) => Err(FfiError::invalid(format!(
            "cannot load peer book {}: {error}",
            path.display()
        ))),
    }
}

/// Render the paired-peers JSON for `id_dir` (schema in the crate header):
/// one element per book entry, sorted by fingerprint, names reported as
/// stored. A missing book is `[]`; a corrupt one is a load error. The whole
/// array is rendered before any buffer write, so an `AK_ERR_BUFFER` retry
/// is simply a fresh, lossless call.
pub fn peer_list_json(id_dir: &str) -> Result<String, FfiError> {
    let (_path, book) = load_peer_book(id_dir)?;
    let mut records: Vec<&PeerRecord> = book.iter().collect();
    records.sort_by_key(|record| *record.fingerprint.as_bytes());
    let mut out = String::from("[");
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"fingerprint\":\"{}\",\"name\":{},\"paired_unix_ms\":{}}}",
            record.fingerprint.to_hex(),
            json::quoted(&record.name),
            record.paired_unix_ms
        ));
    }
    out.push(']');
    Ok(out)
}

/// Pair (or rename) one peer in `<id_dir>/peers.book` and return the new
/// peer count JSON. Semantics mirror the CLI's TOFU `pair add`:
///
/// - an existing fingerprint keeps its original `paired_unix_ms`: a given
///   name renames the entry, no name (null or empty) leaves the record
///   untouched — so re-running an add is always idempotent;
/// - a NEW peer without a name is stored as "(unnamed)";
/// - names must not contain control characters — C0, DEL, or C1, the
///   `ak-wire` display-string rule (rejected, never sanitized) — and are
///   capped like every ABI string.
///
/// `cap` is the caller's buffer capacity: the count JSON is pre-flighted
/// against it BEFORE the save, so `AK_ERR_BUFFER` persists nothing and the
/// retry is a fresh call. A save failure is `AK_ERR_INTERNAL`.
pub fn peer_add(
    id_dir: &str,
    fingerprint_hex: &str,
    name: Option<&str>,
    cap: usize,
) -> Result<String, FfiError> {
    let fingerprint = Fingerprint::from_hex(fingerprint_hex)
        .ok_or_else(|| FfiError::invalid("fingerprint_hex must be 64 hex characters"))?;
    let name = name.filter(|text| !text.is_empty());
    if let Some(text) = name {
        if text.chars().any(|unit| json::is_control_char(unit as u32)) {
            return Err(FfiError::invalid(
                "name must not contain control characters",
            ));
        }
        if text.len() > json::MAX_STRING_BYTES {
            return Err(FfiError::invalid(format!(
                "name must be at most {} bytes",
                json::MAX_STRING_BYTES
            )));
        }
    }
    let (path, mut book) = load_peer_book(id_dir)?;
    match (book.get(&fingerprint), name) {
        (Some(existing), Some(name)) => {
            let paired_unix_ms = existing.paired_unix_ms;
            book.upsert(PeerRecord {
                fingerprint,
                name: name.to_owned(),
                paired_unix_ms,
            });
        }
        (Some(_), None) => {}
        (None, name) => book.upsert(PeerRecord {
            fingerprint,
            name: name.unwrap_or(UNNAMED_PEER).to_owned(),
            paired_unix_ms: now_unix_ms(),
        }),
    }
    let json = peers_count_json(book.iter().count());
    if json.len() + 1 > cap {
        return Err(buffer_too_small(json.len() + 1, cap, "the peer count JSON"));
    }
    book.save(&path).map_err(|error| {
        FfiError::internal(format!("cannot save peer book {}: {error}", path.display()))
    })?;
    Ok(json)
}

/// Remove one paired fingerprint from `<id_dir>/peers.book` and return the
/// remaining peer count JSON. A fingerprint not in the book (including a
/// missing book) is `AK_ERR_NOT_FOUND`; a corrupt book is a load error.
/// Like [`peer_add`], the count JSON is pre-flighted against `cap` before
/// anything is persisted, so `AK_ERR_BUFFER` removes nothing.
pub fn peer_remove(id_dir: &str, fingerprint_hex: &str, cap: usize) -> Result<String, FfiError> {
    let fingerprint = Fingerprint::from_hex(fingerprint_hex)
        .ok_or_else(|| FfiError::invalid("fingerprint_hex must be 64 hex characters"))?;
    let (path, mut book) = load_peer_book(id_dir)?;
    if !book.remove(&fingerprint) {
        return Err(FfiError::not_found(format!(
            "fingerprint {} is not in the peer book {}; nothing removed (see ak_peer_list)",
            fingerprint.short_id(),
            path.display()
        )));
    }
    let json = peers_count_json(book.iter().count());
    if json.len() + 1 > cap {
        return Err(buffer_too_small(json.len() + 1, cap, "the peer count JSON"));
    }
    book.save(&path).map_err(|error| {
        FfiError::internal(format!("cannot save peer book {}: {error}", path.display()))
    })?;
    Ok(json)
}

// ---------------------------------------------------------------------------
// Minimal hand-written JSON (no serde): see module docs for the policy.
// ---------------------------------------------------------------------------

pub(crate) mod json {
    /// Hard input cap: a spec document larger than this is rejected before
    /// any parsing work.
    pub const MAX_INPUT_BYTES: usize = 1 << 20;
    /// Per-string cap (byte length of the decoded content).
    pub const MAX_STRING_BYTES: usize = 4096;
    /// Nesting cap for arrays/objects, including inside unknown fields.
    pub const MAX_DEPTH: usize = 32;

    /// Control-character rule, byte-for-byte the `ak-wire` display-string
    /// rule (`matches!(c, 0..=0x1F | 0x7F..=0x9F)`): C0, DEL, and C1 are
    /// refused in every cross-boundary string — raw or escaped — so no
    /// layer of the stack accepts what another layer rejects.
    pub fn is_control_char(code: u32) -> bool {
        matches!(code, 0..=0x1F | 0x7F..=0x9F)
    }

    /// A parsed JSON value. Numbers keep their raw token; integer-ness is
    /// checked on demand so unknown float fields are skipped, not rejected.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        Num(String),
        Str(String),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }

    /// Parse one complete JSON document. Unknown-but-well-formed values are
    /// kept as-is; only malformed input, over-deep nesting, or strings
    /// containing control characters (raw or escaped) are errors.
    pub fn parse(text: &str) -> Result<Json, String> {
        if text.len() > MAX_INPUT_BYTES {
            return Err(format!(
                "spec JSON exceeds {MAX_INPUT_BYTES} bytes (got {})",
                text.len()
            ));
        }
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
            depth: 0,
        };
        let value = parser.value()?;
        parser.ws();
        if parser.pos != parser.bytes.len() {
            return Err(format!(
                "trailing characters after JSON value at byte {}",
                parser.pos
            ));
        }
        Ok(value)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
        depth: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while let Some(byte) = self.bytes.get(self.pos) {
                if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn value(&mut self) -> Result<Json, String> {
            match self.peek() {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string().map(Json::Str),
                Some(b't') => self.literal("true", Json::Bool(true)),
                Some(b'f') => self.literal("false", Json::Bool(false)),
                Some(b'n') => self.literal("null", Json::Null),
                Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.number(),
                Some(byte) => Err(format!(
                    "unexpected character {:?} at byte {}",
                    byte as char, self.pos
                )),
                None => Err("unexpected end of JSON input".to_string()),
            }
        }

        fn literal(&mut self, want: &str, value: Json) -> Result<Json, String> {
            if self.bytes[self.pos..].starts_with(want.as_bytes()) {
                self.pos += want.len();
                Ok(value)
            } else {
                Err(format!("invalid literal at byte {}", self.pos))
            }
        }

        fn string(&mut self) -> Result<String, String> {
            // Caller peeked the opening quote.
            self.pos += 1;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let Some(byte) = self.peek() else {
                    return Err("unterminated string".to_string());
                };
                match byte {
                    b'"' => {
                        self.pos += 1;
                        if out.len() > MAX_STRING_BYTES {
                            return Err(format!(
                                "string value longer than {MAX_STRING_BYTES} bytes"
                            ));
                        }
                        let text = String::from_utf8(out)
                            .map_err(|_| "string value is not valid UTF-8".to_string())?;
                        // Final defense for characters that only exist in
                        // decoded form: C1 controls (U+0080..U+009F) arrive
                        // as multi-byte UTF-8, never as a raw single byte,
                        // so the byte-level arms below cannot see them.
                        if let Some(unit) = text.chars().find(|&c| is_control_char(c as u32)) {
                            return Err(format!(
                                "control character U+{:04X} inside string value",
                                unit as u32
                            ));
                        }
                        return Ok(text);
                    }
                    b if b < 0x20 || b == 0x7F => {
                        // Injection defense, layer one: a control character
                        // (C0 or DEL) inside a JSON string value is refused
                        // outright; C1 joins via the decoded scan above.
                        return Err(format!(
                            "control character U+{:04X} inside string value at byte {}",
                            b, self.pos
                        ));
                    }
                    b'\\' => {
                        self.pos += 1;
                        let Some(escape) = self.peek() else {
                            return Err("unterminated escape".to_string());
                        };
                        self.pos += 1;
                        match escape {
                            b'"' => out.push(b'"'),
                            b'\\' => out.push(b'\\'),
                            b'/' => out.push(b'/'),
                            // Escapes that DECODE to control characters are
                            // rejected exactly like raw ones: values never
                            // carry control characters in either form.
                            b'b' | b'f' | b'n' | b'r' | b't' => {
                                return Err(format!(
                                    "escape \\{} decodes to a control character; \
                                     control characters in values are rejected",
                                    escape as char
                                ));
                            }
                            b'u' => {
                                let code = self.hex4()?;
                                if is_control_char(code) {
                                    return Err(format!(
                                        "escape \\u{code:04X} decodes to a control \
                                         character; control characters in values are \
                                         rejected"
                                    ));
                                }
                                if (0xD800..=0xDFFF).contains(&code) {
                                    return Err(
                                        "surrogate \\u escapes are not supported".to_string()
                                    );
                                }
                                let mut buffer = [0u8; 4];
                                out.extend_from_slice(
                                    char::from_u32(code)
                                        .unwrap_or('\u{FFFD}')
                                        .encode_utf8(&mut buffer)
                                        .as_bytes(),
                                );
                            }
                            other => {
                                return Err(format!("invalid escape \\{}", other as char));
                            }
                        }
                    }
                    b => {
                        out.push(b);
                        self.pos += 1;
                    }
                }
            }
        }

        fn hex4(&mut self) -> Result<u32, String> {
            let mut value = 0u32;
            for _ in 0..4 {
                let Some(byte) = self.peek() else {
                    return Err("unterminated \\u escape".to_string());
                };
                let digit = (byte as char).to_digit(16).ok_or_else(|| {
                    format!("invalid hex digit in \\u escape at byte {}", self.pos)
                })?;
                value = value * 16 + digit;
                self.pos += 1;
            }
            Ok(value)
        }

        fn number(&mut self) -> Result<Json, String> {
            let start = self.pos;
            if self.peek() == Some(b'-') {
                self.pos += 1;
            }
            match self.peek() {
                Some(b'0') => self.pos += 1,
                Some(byte) if byte.is_ascii_digit() => {
                    while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                        self.pos += 1;
                    }
                }
                _ => return Err(format!("invalid number at byte {}", self.pos)),
            }
            if self.peek() == Some(b'.') {
                self.pos += 1;
                if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(format!(
                        "number missing fraction digits at byte {}",
                        self.pos
                    ));
                }
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                self.pos += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(format!(
                        "number missing exponent digits at byte {}",
                        self.pos
                    ));
                }
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            Ok(Json::Num(
                std::str::from_utf8(&self.bytes[start..self.pos])
                    .unwrap_or_default()
                    .to_string(),
            ))
        }

        fn array(&mut self) -> Result<Json, String> {
            self.enter()?;
            self.pos += 1; // '['
            let mut items = Vec::new();
            self.ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                self.depth -= 1;
                return Ok(Json::Arr(items));
            }
            loop {
                self.ws();
                items.push(self.value()?);
                self.ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b']') => {
                        self.pos += 1;
                        self.depth -= 1;
                        return Ok(Json::Arr(items));
                    }
                    _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
                }
            }
        }

        fn object(&mut self) -> Result<Json, String> {
            self.enter()?;
            self.pos += 1; // '{'
            let mut members: Vec<(String, Json)> = Vec::new();
            self.ws();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                self.depth -= 1;
                return Ok(Json::Obj(members));
            }
            loop {
                self.ws();
                if self.peek() != Some(b'"') {
                    return Err(format!("expected object key at byte {}", self.pos));
                }
                let key = self.string()?;
                self.ws();
                if self.peek() != Some(b':') {
                    return Err(format!("expected ':' at byte {}", self.pos));
                }
                self.pos += 1;
                self.ws();
                let value = self.value()?;
                // Duplicate keys: last one wins (documented, keeps the
                // parser allocation-free on the hot path).
                if let Some(slot) = members.iter_mut().find(|(existing, _)| *existing == key) {
                    slot.1 = value;
                } else {
                    members.push((key, value));
                }
                self.ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        self.depth -= 1;
                        return Ok(Json::Obj(members));
                    }
                    _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
                }
            }
        }

        fn enter(&mut self) -> Result<(), String> {
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err(format!("JSON nesting deeper than {MAX_DEPTH} levels"));
            }
            Ok(())
        }
    }

    impl Json {
        /// Object members (empty slice when not an object).
        pub fn members(&self) -> &[(String, Json)] {
            match self {
                Json::Obj(members) => members,
                _ => &[],
            }
        }

        /// Look up a key (last occurrence wins).
        pub fn get(&self, key: &str) -> Option<&Json> {
            self.members()
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Json::Str(text) => Some(text),
                _ => None,
            }
        }

        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Json::Bool(value) => Some(*value),
                _ => None,
            }
        }

        /// Unsigned integer view; negative, fractional, and exponent forms
        /// are type errors at the extraction site.
        pub fn as_u64(&self) -> Option<u64> {
            match self {
                Json::Num(token) => token.parse::<u64>().ok(),
                _ => None,
            }
        }

        pub fn as_str_array(&self) -> Option<Vec<String>> {
            match self {
                Json::Arr(items) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>(),
                _ => None,
            }
        }
    }

    /// Append `text` as a quoted, escaped JSON string to `out`. Control
    /// characters (which the parser already refuses in inputs) are emitted
    /// as `\u00xx` so output lines are always one physical line.
    pub fn write_quoted(out: &mut String, text: &str) {
        out.push('"');
        for unit in text.chars() {
            match unit {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                control if (control as u32) < 0x20 => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let code = control as u32;
                    out.push_str("\\u00");
                    out.push(HEX[((code >> 4) & 0xF) as usize] as char);
                    out.push(HEX[(code & 0xF) as usize] as char);
                }
                other => out.push(other),
            }
        }
        out.push('"');
    }

    /// Convenience: a fresh quoted string.
    pub fn quoted(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        write_quoted(&mut out, text);
        out
    }
}

// ---------------------------------------------------------------------------
// Job machinery.
// ---------------------------------------------------------------------------

/// Hard cap on queued progress lines. Progress is expendable (the final
/// outcome travels separately); when a frontend stops polling we drop NEW
/// events rather than grow without bound. Never reached by a polling UI.
const MAX_QUEUED_EVENTS: usize = 65_536;

/// Terminal state of one job. The JSON bodies are pre-rendered at
/// completion time so `ak_job_result` is a pure buffer copy.
enum Outcome {
    Running,
    Finished { ok: bool, json: String },
}

struct Job {
    /// The outer task's JoinHandle, taken (and awaited) by the first
    /// `job_cancel`; `None` afterwards or once reaped.
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    outcome: Arc<Mutex<Outcome>>,
    /// For `once_jobs > 1` receives: the loopback address the endpoint is
    /// bound to. `serve_concurrently` runs its accept loop in a DETACHED
    /// task that owns the endpoint (ak-engine has no stop channel for it),
    /// so after the outer task is reaped this addr is "knocked" — a failed
    /// inbound connection attempt wakes the parked accept, which then sees
    /// its closed channel and drops the endpoint, releasing the UDP port.
    /// `None` for send jobs and single receives (their endpoint lives
    /// inside the outer future; reaping it frees the port deterministically).
    knock: Option<SocketAddr>,
}

/// Snapshot returned by [`AkEngine::job_result`].
pub enum JobState {
    Running,
    Finished { ok: bool, json: String },
}

/// The engine handle: one embedded runtime, one job table, one progress
/// queue. Created by `ak_engine_new`, destroyed by `ak_engine_free`
/// (which aborts any still-running jobs first; see `Drop`).
pub struct AkEngine {
    runtime: tokio::runtime::Runtime,
    state: Mutex<HashMap<[u8; 16], Arc<Job>>>,
    events: Arc<Mutex<VecDeque<String>>>,
    counter: AtomicU64,
    /// Instance tag for job ids: not cryptographic, only keeps ids minted
    /// by different engines from looking alike. Uniqueness within one
    /// engine comes from the counter alone.
    tag: u64,
    /// B8 (wave 22) pairing/scheduler runtimes behind the `ak_pairing_*` /
    /// `ak_sched_*` exports (see [`crate::pairing`]).
    pub(crate) v2: Mutex<crate::pairing::V2State>,
}

impl AkEngine {
    /// Build the engine with a 2-worker multi-thread runtime. `Err` carries
    /// the runtime builder's `io::Error` VERBATIM (C13): the OS reason —
    /// e.g. worker-thread creation refused under resource exhaustion —
    /// survives to the caller's error card instead of collapsing into a
    /// generic "cannot build" message, which is what distinguishes a
    /// thread-creation failure from a crash.
    pub fn new() -> Result<AkEngine, std::io::Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let tag = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(1)
            | 1; // never zero so a minted id can never be all-zero
        Ok(AkEngine {
            runtime,
            state: Mutex::new(HashMap::new()),
            events: Arc::new(Mutex::new(VecDeque::new())),
            counter: AtomicU64::new(0),
            tag,
            v2: Mutex::new(crate::pairing::V2State::default()),
        })
    }

    fn next_job_id(&self) -> [u8; 16] {
        loop {
            let serial = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
            let mut id = [0u8; 16];
            id[..8].copy_from_slice(&self.tag.to_be_bytes());
            id[8..].copy_from_slice(&serial.to_be_bytes());
            if id != [0u8; 16] {
                return id;
            }
        }
    }

    /// The embedded runtime (B8 pairing/scheduler tasks spawn here).
    pub(crate) fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    /// The engine-wide progress queue (`ak_poll_progress`); the B8
    /// scheduler executor forwards per-task transfer progress into it.
    pub(crate) fn events_queue(&self) -> Arc<Mutex<VecDeque<String>>> {
        Arc::clone(&self.events)
    }

    /// Ephemeral fallback checkpoint directory for jobs started WITHOUT an
    /// `id_dir`: isolated by FFI job id so concurrent jobs never share
    /// `<job>.akjob` files, and nothing persists for `ak_jobs_list`. Jobs
    /// WITH an `id_dir` use the CLI-visible `<id_dir>/checkpoints` instead
    /// (see [`stable_checkpoint_dir`]).
    fn checkpoint_dir(&self, job: &[u8; 16]) -> PathBuf {
        std::env::temp_dir()
            .join("ak-ffi-checkpoints")
            .join(hex16(job))
    }

    fn register(
        &self,
        job: [u8; 16],
        handle: tokio::task::JoinHandle<()>,
        outcome: &Arc<Mutex<Outcome>>,
        knock: Option<SocketAddr>,
    ) -> Result<(), FfiError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FfiError::internal("job table lock poisoned"))?;
        state.insert(
            job,
            Arc::new(Job {
                handle: Mutex::new(Some(handle)),
                outcome: Arc::clone(outcome),
                knock,
            }),
        );
        Ok(())
    }

    /// Validate the spec, mint a job id, and spawn the send task on the
    /// embedded runtime. Returns the 16-byte job id.
    pub fn start_send(&self, spec_json: &str) -> Result<[u8; 16], FfiError> {
        let args = json::parse(spec_json).map_err(FfiError::invalid)?;
        let job = self.next_job_id();
        let checkpoint_dir = self.checkpoint_dir(&job);
        let spec = build_send_spec(&args, &checkpoint_dir)?;

        let events = Arc::clone(&self.events);
        let outcome = Arc::new(Mutex::new(Outcome::Running));
        let task_outcome = Arc::clone(&outcome);
        let job_line = hex16(&job);
        let handle = self.runtime.spawn(async move {
            let sink = Arc::clone(&events);
            let job_hex = job_line.clone();
            let progress = move |event: Progress| push_event(&sink, &job_hex, None, &event);
            let result = send(spec, &progress).await;
            let finished = match result {
                Ok(report) => Outcome::Finished {
                    ok: true,
                    json: report_json(&job_line, &report),
                },
                Err(error) => Outcome::Finished {
                    ok: false,
                    json: error_json(&job_line, &error.to_string()),
                },
            };
            finish(&task_outcome, finished);
        });
        self.register(job, handle, &outcome, None)?;
        Ok(job)
    }

    /// Validate the spec, mint a job id, and spawn the receive task.
    /// `once_jobs = 1` runs the exact single-job `receive_with_addr` body
    /// and emits a synthetic `listening` event as soon as the endpoint is
    /// bound; `once_jobs > 1` runs `serve_concurrently` (no bound-address
    /// reporting exists on that path, hence the explicit-port requirement).
    pub fn start_receive(&self, spec_json: &str) -> Result<[u8; 16], FfiError> {
        let args = json::parse(spec_json).map_err(FfiError::invalid)?;
        let job = self.next_job_id();
        let checkpoint_dir = self.checkpoint_dir(&job);
        let (spec, once_jobs) = build_receive_spec(&args, &checkpoint_dir)?;
        // Concurrent receives park an ak-engine-owned endpoint in a detached
        // accept task; remember where to knock on cancel (see `Job::knock`).
        // `build_receive_spec` already rejects `once_jobs > 1` with port 0,
        // so the port here is the real bound port.
        let knock = (once_jobs > 1).then(|| SocketAddr::from(([127, 0, 0, 1], spec.bind.port())));

        let events = Arc::clone(&self.events);
        let outcome = Arc::new(Mutex::new(Outcome::Running));
        let task_outcome = Arc::clone(&outcome);
        let job_line = hex16(&job);
        let handle = self.runtime.spawn(async move {
            let finished = if once_jobs == 1 {
                let sink = Arc::clone(&events);
                let job_hex = job_line.clone();
                let progress = move |event: Progress| push_event(&sink, &job_hex, None, &event);
                let (addr_tx, mut addr_rx) = tokio::sync::oneshot::channel();
                let future = receive_with_addr(spec, &progress, addr_tx);
                tokio::pin!(future);
                // Surface the bound address before the first accept; if the
                // future ends first (bind failure) keep its result and skip
                // the event. `future` is pinned, so awaiting it after the
                // select is sound only on the branch where it never
                // completed — exactly what `early` tracks.
                let mut early: Option<Result<Report, EngineError>> = None;
                let bound = tokio::select! {
                    addr = &mut addr_rx => addr.ok(),
                    done = &mut future => {
                        early = Some(done);
                        None
                    }
                };
                if let Some(addr) = bound {
                    if let Ok(mut queue) = events.lock() {
                        push_capped(
                            &mut queue,
                            format!(
                                "{{\"job\":{},\"event\":\"listening\",\"addr\":\"{}\"}}",
                                json::quoted(&job_line),
                                addr
                            ),
                        );
                    }
                }
                let result = match early {
                    Some(result) => result,
                    None => future.await,
                };
                match result {
                    Ok(report) => Outcome::Finished {
                        ok: true,
                        json: report_json(&job_line, &report),
                    },
                    Err(error) => Outcome::Finished {
                        ok: false,
                        json: error_json(&job_line, &error.to_string()),
                    },
                }
            } else {
                let sink = Arc::clone(&events);
                let job_hex = job_line.clone();
                let progress = move |conn: u64, event: Progress| {
                    push_event(&sink, &job_hex, Some(conn), &event)
                };
                let reports = serve_concurrently(spec, once_jobs, &progress).await;
                match aggregate_reports(&reports) {
                    Ok(()) => Outcome::Finished {
                        ok: true,
                        json: multi_ok_json(&job_line, &reports),
                    },
                    Err(message) => Outcome::Finished {
                        ok: false,
                        json: multi_error_json(&job_line, &reports, &message),
                    },
                }
            };
            finish(&task_outcome, finished);
        });
        self.register(job, handle, &outcome, knock)?;
        Ok(job)
    }

    /// Pop the oldest progress line. `None` when the queue is empty.
    pub fn take_progress(&self) -> Result<Option<String>, FfiError> {
        let mut queue = self
            .events
            .lock()
            .map_err(|_| FfiError::internal("progress queue lock poisoned"))?;
        Ok(queue.pop_front())
    }

    /// Put a line back to the front (the shim does this when the caller
    /// buffer was too small, so no event is ever lost to a short buffer).
    pub fn push_front_progress(&self, line: String) -> Result<(), FfiError> {
        let mut queue = self
            .events
            .lock()
            .map_err(|_| FfiError::internal("progress queue lock poisoned"))?;
        queue.push_front(line);
        Ok(())
    }

    /// Snapshot of one job's terminal state.
    pub fn job_result(&self, job: &[u8; 16]) -> Result<JobState, FfiError> {
        let state = self
            .state
            .lock()
            .map_err(|_| FfiError::internal("job table lock poisoned"))?;
        let entry = state
            .get(job)
            .ok_or_else(|| FfiError::not_found(format!("unknown job {}", hex16(job))))?;
        let outcome = entry
            .outcome
            .lock()
            .map_err(|_| FfiError::internal("job outcome lock poisoned"))?;
        Ok(match &*outcome {
            Outcome::Running => JobState::Running,
            Outcome::Finished { ok, json } => JobState::Finished {
                ok: *ok,
                json: json.clone(),
            },
        })
    }

    /// Abort a running job and CONVERGE the teardown before returning:
    ///
    /// 1. the outer task is aborted;
    /// 2. its JoinHandle is awaited (bounded to 500 ms) — once that
    ///    resolves, the future is dropped, and with it every endpoint and
    ///    channel the task owned: the UDP port of a send job or a
    ///    `once_jobs = 1` receive is released deterministically, and the
    ///    task can never emit another progress event;
    /// 3. for `once_jobs > 1` receives the bound port is knocked (see
    ///    [`Job::knock`]) so ak-engine's detached accept task drops its
    ///    endpoint promptly instead of at its accept deadline;
    /// 4. the outcome is stamped `cancelled by caller` and a raced
    ///    trailing `done` event line for this job is removed, so a
    ///    cancelled job never presents a fake completion.
    ///
    /// A job that had already finished (either way) is left untouched.
    ///
    /// This call may block for a bounded few hundred milliseconds (steps 2
    /// and 3); it must run on the caller's (non-runtime) thread like every
    /// other entry point.
    pub fn job_cancel(&self, job: &[u8; 16]) -> Result<(), FfiError> {
        let entry = {
            let state = self
                .state
                .lock()
                .map_err(|_| FfiError::internal("job table lock poisoned"))?;
            state
                .get(job)
                .cloned()
                .ok_or_else(|| FfiError::not_found(format!("unknown job {}", hex16(job))))?
        };
        {
            // Fast path: a job that already finished is untouched (the
            // take/abort/await below would be harmless but pointless).
            let outcome = entry
                .outcome
                .lock()
                .map_err(|_| FfiError::internal("job outcome lock poisoned"))?;
            if !matches!(&*outcome, Outcome::Running) {
                return Ok(());
            }
        }
        // 1-2. Abort, then await the handle with a hard 500 ms cap. After
        // `abort()` the JoinHandle resolves once the future is dropped; the
        // cap bounds a task wedged in blocking work instead of hanging the
        // caller.
        if let Some(handle) = entry
            .handle
            .lock()
            .map_err(|_| FfiError::internal("job handle lock poisoned"))?
            .take()
        {
            handle.abort();
            // The timeout future must be CONSTRUCTED inside the async block:
            // `tokio::time::timeout` binds to the runtime timer driver at
            // creation, i.e. only after `block_on` entered the context.
            let _ = self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_millis(500), handle).await });
        }
        // 3. Release a concurrent receive's endpoint. Must run AFTER the
        // outer task was reaped: only then is the accept task's channel
        // receiver gone, so the woken accept observes the closed channel,
        // breaks, and drops the endpoint.
        if let Some(addr) = entry.knock {
            self.knock_endpoint(addr);
        }
        // 4. Stamp the outcome (unless the task completed on its own in
        // the race window — then its own terminal state stands) and strip
        // any queued fake-completion `done` line.
        let mut outcome = entry
            .outcome
            .lock()
            .map_err(|_| FfiError::internal("job outcome lock poisoned"))?;
        if matches!(&*outcome, Outcome::Running) {
            if let Ok(mut queue) = self.events.lock() {
                let job_key = format!("\"job\":\"{}\"", hex16(job));
                queue.retain(|line| {
                    !(line.contains(&job_key) && line.contains("\"event\":\"done\""))
                });
            }
            *outcome = Outcome::Finished {
                ok: false,
                json: error_json(&hex16(job), "cancelled by caller"),
            };
        }
        Ok(())
    }

    /// Wake the detached accept task that owns a cancelled concurrent
    /// receive's endpoint, and confirm the port was actually released. A
    /// one-shot QUIC connection attempt at `addr` fails fast — the knock
    /// client pins its OWN (wrong) fingerprint, so the TLS handshake is
    /// refused on loopback within milliseconds — and the failed inbound
    /// attempt is exactly what unblocks the parked `accept`, after which
    /// the accept loop breaks on its closed channel and drops the endpoint
    /// (freeing the UDP port). A short rebind probe then verifies the
    /// release from the CALLER's perspective, so `ak_job_cancel` returning
    /// means an immediate rebind on the same port succeeds (not just that a
    /// wake-up was sent). Bounded and best-effort: if the endpoint was
    /// already gone the probe passes instantly; in the worst case the
    /// accept deadline still bounds cleanup.
    fn knock_endpoint(&self, addr: SocketAddr) {
        let port = addr.port();
        // The rebind probe must use the SAME wildcard form the engine binds
        // (`0.0.0.0:port`): Windows allows a specific-address bind to
        // overlap an existing wildcard bind, so only a wildcard probe
        // reports the port as still held.
        let probe_addr = SocketAddr::from(([0, 0, 0, 0], port));
        // Already released (accept task ended on its own)? Then do nothing.
        if let Ok(probe) = std::net::UdpSocket::bind(probe_addr) {
            drop(probe);
            return;
        }
        self.runtime.block_on(async move {
            let key = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);
            let Ok(endpoint) =
                ak_net::DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle).await
            else {
                return;
            };
            // Pin the knock client's OWN fingerprint: it can never match the
            // receiver's certificate, so the handshake is refused quickly
            // and the TLS alert actually flies (aborting the client early
            // could leave the server's accept waiting for a client Finished
            // that never comes). A few attempts cover the case where the
            // accept loop happened to be between iterations at knock time.
            let pinned = key.fingerprint();
            for _ in 0..3 {
                let _ = tokio::time::timeout(
                    Duration::from_millis(300),
                    endpoint.connect(addr, &pinned),
                )
                .await;
                // Confirm the release before declaring convergence; the
                // probe socket is dropped immediately so it cannot itself
                // hold the port the caller wants to rebind.
                let mut waited = 0u32;
                loop {
                    if let Ok(probe) = std::net::UdpSocket::bind(probe_addr) {
                        drop(probe);
                        return;
                    }
                    if waited >= 200 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    waited += 10;
                }
            }
        });
    }
}

impl Drop for AkEngine {
    fn drop(&mut self) {
        // Abort every still-running job before the runtime goes away; the
        // runtime drop then waits briefly for in-flight blocking work
        // (chunk reads) only. Detached helper tasks (the concurrent accept
        // loop) are torn down with the runtime itself.
        if let Ok(state) = self.state.lock() {
            for job in state.values() {
                if let Ok(mut slot) = job.handle.lock() {
                    if let Some(handle) = slot.take() {
                        handle.abort();
                    }
                }
            }
        }
        // B8 runtimes: abort op tasks and the owner loop, close the
        // control clients, and stop the scheduler actor (bounded; the
        // runtime is still alive here — fields drop after this body).
        if let Ok(v2) = self.v2.lock() {
            for rt in v2.runtimes.values() {
                rt.teardown(&self.runtime);
            }
            if let Some(sched) = &v2.sched {
                sched.teardown(&self.runtime);
            }
            // B8 (wave 28) task runtimes: same discipline — wake every
            // blocked consent gate with a decline FIRST, then abort the
            // op/resident-receiver tasks (see `TaskRt::teardown`).
            for rt in v2.tasks.values() {
                rt.teardown(&self.runtime);
            }
        }
    }
}

/// Stamp the terminal outcome exactly once per task.
fn finish(outcome: &Mutex<Outcome>, value: Outcome) {
    if let Ok(mut slot) = outcome.lock() {
        if matches!(&*slot, Outcome::Running) {
            *slot = value;
        }
    }
}

/// Serialize one progress event as a compact JSON line, dropping it only if
/// the queue is at capacity (see `MAX_QUEUED_EVENTS`).
pub(crate) fn push_event(
    queue: &Arc<Mutex<VecDeque<String>>>,
    job: &str,
    conn: Option<u64>,
    event: &Progress,
) {
    let mut line = String::with_capacity(96);
    line.push_str("{\"job\":");
    line.push_str(&json::quoted(job));
    if let Some(conn) = conn {
        line.push_str(&format!(",\"conn\":{conn}"));
    }
    line.push_str(",\"event\":");
    line.push_str(&json::quoted(event_name(event)));
    for (key, value) in event_fields(event) {
        line.push(',');
        line.push_str(&json::quoted(key));
        line.push(':');
        match value {
            Field::Num(number) => line.push_str(&number.to_string()),
            Field::Flag(flag) => line.push_str(if flag { "true" } else { "false" }),
            Field::Str(text) => line.push_str(&json::quoted(text)),
        }
    }
    line.push('}');
    if let Ok(mut queue) = queue.lock() {
        push_capped(&mut queue, line);
    }
}

pub(crate) fn push_capped(queue: &mut VecDeque<String>, line: String) {
    if queue.len() < MAX_QUEUED_EVENTS {
        queue.push_back(line);
    }
}

enum Field<'a> {
    Num(u64),
    Flag(bool),
    Str(&'a str),
}

fn event_name(event: &Progress) -> &'static str {
    match event {
        Progress::Connecting => "connecting",
        Progress::Connected { .. } => "connected",
        Progress::Offered { .. } => "offered",
        Progress::Accepted { .. } => "accepted",
        Progress::ManifestPageSent { .. } => "manifest_page_sent",
        Progress::ManifestReceived => "manifest_received",
        Progress::Resuming { .. } => "resuming",
        Progress::FileStarted { .. } => "file_started",
        Progress::ChunkSent { .. } => "chunk_sent",
        Progress::ChunkReceived { .. } => "chunk_received",
        Progress::BytesAcked { .. } => "bytes_acked",
        Progress::Paused => "paused",
        Progress::Resumed => "resumed",
        Progress::FileVerified { .. } => "file_verified",
        Progress::FileCommitted { .. } => "file_committed",
        Progress::Verifying => "verifying",
        Progress::FinalAck { .. } => "final_ack",
        Progress::Done => "done",
    }
}

/// The wire event name of one [`Progress`] (B8 wave 28: shared with the
/// task plane's progress forwarding, which renders `ak_poll_progress`
/// lines under the conn/code id).
pub(crate) fn progress_name(event: &Progress) -> &'static str {
    event_name(event)
}

fn event_fields(event: &Progress) -> Vec<(&'static str, Field<'_>)> {
    match event {
        Progress::Connected { peer } => vec![("peer", Field::Str(peer))],
        Progress::Offered { files, bytes } | Progress::Accepted { files, bytes } => {
            vec![("files", Field::Num(*files)), ("bytes", Field::Num(*bytes))]
        }
        Progress::ManifestPageSent { page } => vec![("page", Field::Num(u64::from(*page)))],
        Progress::Resuming { chunks } => vec![("chunks", Field::Num(*chunks))],
        Progress::FileStarted {
            file_index,
            path,
            bytes,
        } => vec![
            ("file", Field::Num(u64::from(*file_index))),
            ("path", Field::Str(path)),
            ("bytes", Field::Num(*bytes)),
        ],
        Progress::ChunkSent {
            file_index,
            chunk_index,
        }
        | Progress::ChunkReceived {
            file_index,
            chunk_index,
        } => vec![
            ("file", Field::Num(u64::from(*file_index))),
            ("chunk", Field::Num(*chunk_index)),
        ],
        Progress::BytesAcked {
            file_index,
            cumulative_bytes,
        } => vec![
            ("file", Field::Num(u64::from(*file_index))),
            ("bytes", Field::Num(*cumulative_bytes)),
        ],
        Progress::FileVerified { file_index } => {
            vec![("file", Field::Num(u64::from(*file_index)))]
        }
        Progress::FileCommitted { file_index, path } => vec![
            ("file", Field::Num(u64::from(*file_index))),
            ("path", Field::Str(path)),
        ],
        Progress::FinalAck { verified } => vec![("verified", Field::Flag(*verified))],
        _ => Vec::new(),
    }
}

fn report_json(job: &str, report: &Report) -> String {
    let peer = match &report.peer_fingerprint {
        Some(hex) => json::quoted(hex),
        None => "null".to_string(),
    };
    let roots = report
        .roots
        .iter()
        .map(|root| json::quoted(root))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"job\":{},\"wire_job\":\"{}\",\"files\":{},\"bytes\":{},\"duration_ms\":{},\"resumed\":{},\"peer\":{},\"roots\":[{}]}}",
        json::quoted(job),
        hex16(&report.job_id),
        report.files,
        report.bytes,
        report.duration.as_millis().min(u64::MAX as u128),
        report.resumed_chunks,
        peer,
        roots
    )
}

fn error_json(job: &str, message: &str) -> String {
    format!(
        "{{\"job\":{},\"error\":{}}}",
        json::quoted(job),
        json::quoted(message)
    )
}

/// Fold `serve_concurrently` per-connection results: `Ok(())` when every
/// connection succeeded, otherwise `Err` carries the first error message.
fn aggregate_reports(results: &[Result<Report, EngineError>]) -> Result<(), String> {
    for result in results {
        if let Err(error) = result {
            return Err(error.to_string());
        }
    }
    Ok(())
}

/// Success body for a concurrent receive: an aggregate over every
/// connection, with each connection's wire job id preserved.
fn multi_ok_json(job: &str, results: &[Result<Report, EngineError>]) -> String {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut resumed = 0u64;
    let mut duration = Duration::ZERO;
    let mut wire_jobs = Vec::with_capacity(results.len());
    for report in results.iter().filter_map(|result| result.as_ref().ok()) {
        files += report.files;
        bytes += report.bytes;
        resumed += report.resumed_chunks;
        duration = duration.max(report.duration);
        wire_jobs.push(json::quoted(&hex16(&report.job_id)));
    }
    format!(
        "{{\"job\":{},\"jobs\":{},\"wire_jobs\":[{}],\"files\":{},\"bytes\":{},\"duration_ms\":{},\"resumed\":{},\"roots\":[]}}",
        json::quoted(job),
        results.len(),
        wire_jobs.join(","),
        files,
        bytes,
        duration.as_millis().min(u64::MAX as u128),
        resumed
    )
}

/// Render the failure body for a multi-connection receive: how many
/// connections were expected/ok/failed plus up to 8 error messages.
fn multi_error_json(
    job: &str,
    results: &[Result<Report, EngineError>],
    first_message: &str,
) -> String {
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let failed = results.len() - ok;
    let messages: Vec<String> = results
        .iter()
        .filter_map(|r| r.as_ref().err().map(|e| json::quoted(&e.to_string())))
        .take(8)
        .collect();
    format!(
        "{{\"job\":{},\"jobs\":{},\"ok\":{},\"failed\":{},\"error\":{},\"errors\":[{}]}}",
        json::quoted(job),
        results.len(),
        ok,
        failed,
        json::quoted(first_message),
        messages.join(",")
    )
}

fn hex16(bytes: &[u8; 16]) -> String {
    hex_encode(bytes)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xF) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// spec JSON -> engine specs.
// ---------------------------------------------------------------------------

fn field_str(args: &json::Json, key: &str, required: bool) -> Result<Option<String>, FfiError> {
    match args.get(key) {
        None | Some(json::Json::Null) if required => Err(FfiError::invalid(format!(
            "missing required field \"{key}\""
        ))),
        None | Some(json::Json::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| type_error(key, "string")),
    }
}

fn field_u64(args: &json::Json, key: &str) -> Result<Option<u64>, FfiError> {
    match args.get(key) {
        None | Some(json::Json::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| type_error(key, "unsigned integer")),
    }
}

fn field_bool(args: &json::Json, key: &str) -> Result<Option<bool>, FfiError> {
    match args.get(key) {
        None | Some(json::Json::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| type_error(key, "boolean")),
    }
}

fn type_error(key: &str, wanted: &str) -> FfiError {
    FfiError::invalid(format!("field \"{key}\" must be {wanted}"))
}

fn parse_conflict(text: &str) -> Result<ConflictPolicy, FfiError> {
    match text {
        "fail" => Ok(ConflictPolicy::Fail),
        "rename" => Ok(ConflictPolicy::Rename),
        other => Err(FfiError::invalid(format!(
            "field \"conflict\" must be \"fail\" or \"rename\", got {other:?}"
        ))),
    }
}

fn parse_timeout(secs: u64) -> Result<Duration, FfiError> {
    if !(1..=86_400).contains(&secs) {
        return Err(FfiError::invalid(
            "field \"timeout_secs\" must be between 1 and 86400",
        ));
    }
    Ok(Duration::from_secs(secs))
}

fn parse_device_name(name: &str) -> Result<String, FfiError> {
    if name.chars().count() > 64 {
        return Err(FfiError::invalid(
            "field \"device_name\" must be at most 64 characters",
        ));
    }
    Ok(name.to_string())
}

/// Device identity for a job. With `id_dir` the key is loaded from (or
/// created at) `<id_dir>/device.key` — the ak-cli convention — so a
/// frontend gets a stable identity across calls. Without it a fresh
/// ephemeral key is generated per job (documented: nothing persists).
fn resolve_identity(id_dir: Option<&str>) -> Result<(DeviceKeyPair, TlsBundle), FfiError> {
    let key = match id_dir {
        Some(dir) if !dir.is_empty() => {
            let path = PathBuf::from(dir).join("device.key");
            let (key, _created) = DeviceKeyPair::load_or_create(&path).map_err(|error| {
                FfiError::invalid(format!(
                    "cannot load device key at {}: {error}",
                    path.display()
                ))
            })?;
            key
        }
        _ => DeviceKeyPair::generate(),
    };
    let bundle = make_self_signed(&key);
    Ok((key, bundle))
}

/// The engine-facing consent callback (`ReceiveSpec.authorize`).
type Authorize = Box<dyn Fn(&Fingerprint) -> bool + Send + Sync>;

/// Receiver admission policy from `expect`: `any`, `book` (the peer book
/// under `id_dir`), a 64-hex fingerprint, or a 6-digit pairing code (with
/// or without the `123-456` dash).
enum Expect {
    Any,
    Pinned(Fingerprint),
    Code(String),
    /// Admit only fingerprints listed in `<id_dir>/peers.book`; resolved
    /// against the spec's `id_dir` in [`Expect::authorize`].
    Book,
}

impl Expect {
    fn parse(text: &str) -> Result<Expect, FfiError> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(Expect::Any);
        }
        if trimmed.eq_ignore_ascii_case("book") {
            return Ok(Expect::Book);
        }
        if let Some(pinned) = Fingerprint::from_hex(trimmed) {
            return Ok(Expect::Pinned(pinned));
        }
        let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
        let separators_ok = trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == ' ');
        if separators_ok && digits.len() == 6 {
            return Ok(Expect::Code(trimmed.to_string()));
        }
        Err(FfiError::invalid(
            "field \"expect\" must be \"any\", \"book\", a 64-hex fingerprint, or a 6-digit \
             pairing code",
        ))
    }

    /// Build the `ReceiveSpec.authorize` callback. `Book` needs the spec's
    /// `id_dir` and is validated eagerly (missing/empty/corrupt book is an
    /// `AK_ERR_INVALID` at start time, mirroring the CLI's "pair first").
    fn authorize(self, id_dir: Option<&str>) -> Result<Authorize, FfiError> {
        match self {
            Expect::Any => Ok(Box::new(|_| true)),
            Expect::Pinned(pinned) => Ok(Box::new(move |peer| *peer == pinned)),
            Expect::Code(code) => Ok(Box::new(move |peer| verify_pairing_code(peer, &code))),
            Expect::Book => book_authorizer(id_dir),
        }
    }
}

/// The `expect: "book"` admission callback plus its start-time validation.
///
/// Mirrors the CLI exactly: the book must load AND hold at least one peer
/// before the job starts (otherwise the caller must pair first or authorize
/// explicitly), and the callback re-reads the file on every connection so
/// pairing edits take effect without restarting the receive.
fn book_authorizer(id_dir: Option<&str>) -> Result<Authorize, FfiError> {
    let dir = id_dir.filter(|dir| !dir.is_empty()).ok_or_else(|| {
        FfiError::invalid(
            "expect \"book\" requires a non-empty \"id_dir\" (the peer book lives at \
             <id_dir>/peers.book)",
        )
    })?;
    let path = PathBuf::from(dir).join("peers.book");
    match PeerBook::load(&path) {
        // A missing file is the empty book: same "pair first" refusal.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(empty_book_error(&path)),
        Err(error) => Err(FfiError::invalid(format!(
            "cannot load peer book {}: {error}",
            path.display()
        ))),
        Ok(book) if book.iter().next().is_none() => Err(empty_book_error(&path)),
        Ok(_) => Ok(Box::new(move |peer| {
            PeerBook::load(&path)
                .map(|book| book.get(peer).is_some())
                .unwrap_or(false)
        })),
    }
}

fn empty_book_error(path: &Path) -> FfiError {
    FfiError::invalid(format!(
        "no paired peers in {}: pair first (add the sender's fingerprint to the book, \
         or print this device's pairing code via ak_identity_info), or authorize this \
         receive explicitly with \"expect\": \"any\" or a 64-hex fingerprint",
        path.display()
    ))
}

/// Checkpoint directory for jobs started with an `id_dir`: the ak-cli
/// convention `<id_dir>/checkpoints`, so `ak_jobs_list` (and the CLI) see
/// the records. `None` when no usable `id_dir` was given; the caller then
/// falls back to the ephemeral per-job temp directory.
fn stable_checkpoint_dir(id_dir: Option<&str>) -> Option<PathBuf> {
    let dir = id_dir.filter(|dir| !dir.is_empty())?;
    Some(PathBuf::from(dir).join("checkpoints"))
}

/// Build a [`SendSpec`] from the parsed JSON object. Schema (all strings
/// UTF-8, control characters rejected by the parser):
///
/// ```json
/// {
///   "source_roots": ["/path/one", "/path/two"],   // required, >= 1
///   "peer": "127.0.0.1:7100",                     // required ip:port
///   "fp_hex": "<receiver fingerprint, 64 hex>",   // required
///   "chunk_kb": 64,                               // optional, 1..=8192
///   "multistream": false,                         // optional bool
///   "no_internet": false,                         // optional policy gate
///   "timeout_secs": 30,                           // optional idle budget
///   "conflict": "fail",                           // "fail" | "rename"
///   "device_name": "laptop",                      // optional, <= 64 chars
///   "id_dir": "C:/ak/identity"                    // optional, <id_dir>/device.key
/// }
/// ```
///
/// Unknown fields are ignored. With an `id_dir` the job's checkpoints are
/// persisted under `<id_dir>/checkpoints` (visible to `ak_jobs_list`);
/// without one they go to an ephemeral per-job temp directory.
fn build_send_spec(args: &json::Json, checkpoint_dir: &Path) -> Result<SendSpec, FfiError> {
    let roots = match args.get("source_roots") {
        None | Some(json::Json::Null) => {
            return Err(FfiError::invalid("missing required field \"source_roots\""));
        }
        Some(value) => value
            .as_str_array()
            .ok_or_else(|| type_error("source_roots", "an array of strings"))?,
    };
    if roots.is_empty() {
        return Err(FfiError::invalid(
            "field \"source_roots\" must not be empty",
        ));
    }
    if roots.iter().any(|root| root.is_empty()) {
        return Err(FfiError::invalid(
            "entries of \"source_roots\" must be non-empty paths",
        ));
    }
    let peer_text =
        field_str(args, "peer", true)?.ok_or_else(|| FfiError::invalid("missing \"peer\""))?;
    let peer: SocketAddr = peer_text.parse().map_err(|_| {
        FfiError::invalid(format!(
            "field \"peer\" is not an ip:port address: {peer_text:?}"
        ))
    })?;
    let fp_text =
        field_str(args, "fp_hex", true)?.ok_or_else(|| FfiError::invalid("missing \"fp_hex\""))?;
    let pinned = Fingerprint::from_hex(&fp_text)
        .ok_or_else(|| FfiError::invalid("field \"fp_hex\" must be 64 hex characters"))?;

    let chunk_kb = field_u64(args, "chunk_kb")?;
    if let Some(kb) = chunk_kb {
        if !(1..=8192).contains(&kb) {
            return Err(FfiError::invalid(
                "field \"chunk_kb\" must be between 1 and 8192",
            ));
        }
    }
    let id_dir = field_str(args, "id_dir", false)?;
    let (_key, bundle) = resolve_identity(id_dir.as_deref())?;

    let mut spec = SendSpec::new(PathBuf::from(&roots[0]), peer, pinned, bundle);
    for extra in &roots[1..] {
        spec.add_root(PathBuf::from(extra));
    }
    // Match the bind family to the peer so the engine's family check can
    // never reject a legitimately reachable address.
    spec.bind = if peer.is_ipv6() {
        SocketAddr::from(([0u16; 8], 0))
    } else {
        SocketAddr::from(([0u8; 4], 0))
    };
    spec.checkpoint_dir =
        stable_checkpoint_dir(id_dir.as_deref()).unwrap_or_else(|| checkpoint_dir.to_path_buf());
    if let Some(kb) = chunk_kb {
        spec.chunk_size =
            NonZeroU32::new((kb * 1024) as u32).ok_or_else(|| FfiError::invalid("bad chunk_kb"))?;
    }
    if let Some(multistream) = field_bool(args, "multistream")? {
        spec.multistream = multistream;
    }
    if let Some(no_internet) = field_bool(args, "no_internet")? {
        spec.policy = DirectPolicy {
            allow_internet: !no_internet,
            ..DirectPolicy::default()
        };
    }
    if let Some(secs) = field_u64(args, "timeout_secs")? {
        spec.idle_timeout = parse_timeout(secs)?;
    }
    if let Some(text) = field_str(args, "conflict", false)? {
        spec.conflict = parse_conflict(&text)?;
    }
    if let Some(name) = field_str(args, "device_name", false)? {
        spec.device_name = parse_device_name(&name)?;
    }
    Ok(spec)
}

/// Build a [`ReceiveSpec`] (plus `once_jobs`) from the parsed JSON object:
///
/// ```json
/// {
///   "out_dir": "C:/inbox",              // required, created when missing
///   "port": 7100,                       // optional, default 7100
///   "expect": "any",                    // REQUIRED: "any" | "book" |
///                                       //   64-hex fingerprint | 6-digit code
///   "conflict": "fail",                 // "fail" | "rename"
///   "timeout_secs": 30,                 // optional idle budget
///   "id_dir": "C:/ak/identity",         // optional stable identity
///   "device_name": "phone",             // optional, <= 64 chars
///   "once_jobs": 1,                     // optional, 1..=64
///   "no_internet": false                // optional policy gate
/// }
/// ```
///
/// `expect` is REQUIRED: a spec without it is `AK_ERR_INVALID` ("expect
/// required: any|book|fingerprint|pairing code"). There is no default —
/// "any" accepts everyone and must be stated explicitly by the caller
/// (breaking change: earlier revisions silently defaulted to "any").
///
/// `expect: "book"` admits only fingerprints in `<id_dir>/peers.book` and
/// requires that book to exist, parse, and hold at least one peer at start
/// time (CLI semantics: pair first or authorize explicitly).
///
/// `once_jobs > 1` selects `serve_concurrently` and requires an explicit
/// non-zero port (that engine path does not report its bound address).
/// With an `id_dir` the job's checkpoints are persisted under
/// `<id_dir>/checkpoints` (visible to `ak_jobs_list`); without one they go
/// to an ephemeral per-job temp directory.
fn build_receive_spec(
    args: &json::Json,
    checkpoint_dir: &Path,
) -> Result<(ReceiveSpec, usize), FfiError> {
    let out_dir = field_str(args, "out_dir", true)?
        .ok_or_else(|| FfiError::invalid("missing \"out_dir\""))?;
    if out_dir.is_empty() {
        return Err(FfiError::invalid("field \"out_dir\" must be non-empty"));
    }
    std::fs::create_dir_all(&out_dir).map_err(|error| {
        FfiError::invalid(format!(
            "cannot create out_dir {}: {error}",
            Path::new(&out_dir).display()
        ))
    })?;

    let port = field_u64(args, "port")?.unwrap_or(7100);
    if port > u16::MAX as u64 {
        return Err(FfiError::invalid("field \"port\" must be a valid UDP port"));
    }
    let once_jobs = field_u64(args, "once_jobs")?.unwrap_or(1);
    if !(1..=64).contains(&once_jobs) {
        return Err(FfiError::invalid(
            "field \"once_jobs\" must be between 1 and 64",
        ));
    }
    if once_jobs > 1 && port == 0 {
        return Err(FfiError::invalid(
            "once_jobs > 1 requires an explicit non-zero \"port\" \
             (concurrent receive does not report its bound address)",
        ));
    }

    // `expect` is REQUIRED (breaking change, FFI API rev 1): an accidental
    // missing field must never widen admission to "accepts everyone". The
    // caller states its policy explicitly — "any" included.
    let expect_text = field_str(args, "expect", false)?
        .ok_or_else(|| FfiError::invalid("expect required: any|book|fingerprint|pairing code"))?;
    let expect = Expect::parse(&expect_text)?;

    if let Some(secs) = field_u64(args, "timeout_secs")? {
        parse_timeout(secs)?;
    }
    let id_dir = field_str(args, "id_dir", false)?;
    let (_key, bundle) = resolve_identity(id_dir.as_deref())?;

    let mut spec = ReceiveSpec::new(PathBuf::from(&out_dir), bundle);
    spec.bind = SocketAddr::from(([0u8; 4], port as u16));
    spec.authorize = expect.authorize(id_dir.as_deref())?;
    spec.checkpoint_dir =
        stable_checkpoint_dir(id_dir.as_deref()).unwrap_or_else(|| checkpoint_dir.to_path_buf());
    if let Some(secs) = field_u64(args, "timeout_secs")? {
        spec.idle_timeout = parse_timeout(secs)?;
    }
    if let Some(text) = field_str(args, "conflict", false)? {
        spec.conflict = parse_conflict(&text)?;
    }
    if let Some(name) = field_str(args, "device_name", false)? {
        spec.device_name = parse_device_name(&name)?;
    }
    if let Some(no_internet) = field_bool(args, "no_internet")? {
        spec.policy = DirectPolicy {
            allow_internet: !no_internet,
            ..DirectPolicy::default()
        };
    }
    Ok((spec, once_jobs as usize))
}
