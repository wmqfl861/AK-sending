//! End-to-end wire codec for the device-to-device file protocol (frame v1).
//!
//! # Frame layout (frozen)
//!
//! `[version u8][type u16 BE][payload_len u32 BE][payload]` — a 7-byte header
//! followed by exactly `payload_len` payload bytes. Decode consumes exactly
//! one frame; leftover input or leftover payload is rejected.
//!
//! # Field encoding (frozen; all integers big-endian, raw bytes)
//!
//! - scalars: u16/u32/u64 as raw big-endian bytes; i64 as its two's
//!   complement bit pattern — u64/i64 values never pass through a float;
//! - bool: one byte, 0 = false, 1 = true, any other value is rejected;
//! - string: `[len u16 BE][UTF-8 bytes]`;
//! - fixed-size arrays (`[u8; 16]`, `[u8; 32]`): raw bytes;
//! - vector: `[count u32 BE][items in order]`;
//! - enum tag: one byte; Control uses Pause = 1, Cancel = 2;
//! - ErrorCode: u16;
//! - WireEntry: rel_path string, is_dir bool, size u64, mtime i64
//!   (two's complement as-is), digest_alg u8, whole_digest (32 raw bytes),
//!   chunk_digests vector;
//! - ResumeEntry: file_index u32, verified_prefix_chunks u64.
//!
//! # Message payloads (frozen field order)
//!
//! | message | type | payload fields |
//! |---------|------|----------------|
//! | Hello | 1 | proto_version u16, device_name str, public_key 32B, capabilities u16 |
//! | Offer | 2 | job_id 16B, root_name str, file_count u64, total_bytes u64, chunk_size u32 |
//! | Accept | 3 | job_id 16B |
//! | Reject | 4 | job_id 16B, reason u16 (ErrorCode) |
//! | ManifestPage | 5 | job_id 16B, page_index u32, last_page bool, entries vec\<WireEntry\> |
//! | Chunk | 6 | job_id 16B, file_index u32, chunk_index u64, data_len u32, data |
//! | ChunkAck | 7 | job_id 16B, file_index u32, chunk_index u64 |
//! | ResumeRequest | 8 | job_id 16B, files vec\<ResumeEntry\> |
//! | Control | 9 | job_id 16B, tag u8 |
//! | Finalize | 10 | job_id 16B |
//! | FinalAck | 11 | job_id 16B, verified bool |
//! | Error | 12 | code u16 (ErrorCode), message str |
//!
//! Every decode validates type, version, length, offsets and resource budgets
//! against [`Limits`] before allocating memory, and refuses unknown values.
//!
//! Message set mirrors docs/PROTOCOL.zh-CN.md. There is deliberately no
//! generic payload or blob message (ADR-001 D6 type lock): the set is closed
//! and unknown types are rejected.
#![forbid(unsafe_code)]

use ak_core::error::ErrorCode;
use ak_core::manifest::{validate_rel_path, FileEntry, DIGEST_LEN};

/// Wire protocol version of frame v1.
pub const VERSION: u8 = 1;
/// Header size in bytes: version(1) + type(2) + payload_len(4).
pub const HEADER_LEN: usize = 7;
/// Hard ceiling for any single frame regardless of limits.
pub const MAX_FRAME_BYTES: usize = (8 << 20) + 1024;
/// Capability bit: receiver supports resume from checkpoints.
pub const CAP_RESUME: u16 = 0x0001;

/// Message type value for [`Message::Hello`].
pub const TYPE_HELLO: u16 = 1;
/// Message type value for [`Message::Offer`].
pub const TYPE_OFFER: u16 = 2;
/// Message type value for [`Message::Accept`].
pub const TYPE_ACCEPT: u16 = 3;
/// Message type value for [`Message::Reject`].
pub const TYPE_REJECT: u16 = 4;
/// Message type value for [`Message::ManifestPage`].
pub const TYPE_MANIFEST_PAGE: u16 = 5;
/// Message type value for [`Message::Chunk`].
pub const TYPE_CHUNK: u16 = 6;
/// Message type value for [`Message::ChunkAck`].
pub const TYPE_CHUNK_ACK: u16 = 7;
/// Message type value for [`Message::ResumeRequest`].
pub const TYPE_RESUME_REQUEST: u16 = 8;
/// Message type value for [`Message::Control`].
pub const TYPE_CONTROL: u16 = 9;
/// Message type value for [`Message::Finalize`].
pub const TYPE_FINALIZE: u16 = 10;
/// Message type value for [`Message::FinalAck`].
pub const TYPE_FINAL_ACK: u16 = 11;
/// Message type value for [`Message::Error`].
pub const TYPE_ERROR: u16 = 12;

/// Control tag for [`Control::Pause`].
pub const CONTROL_PAUSE: u8 = 1;
/// Control tag for [`Control::Cancel`].
pub const CONTROL_CANCEL: u8 = 2;

/// Every message type value; keeps the set closed and iterable.
const TYPE_VALUES: [u16; 12] = [
    TYPE_HELLO,
    TYPE_OFFER,
    TYPE_ACCEPT,
    TYPE_REJECT,
    TYPE_MANIFEST_PAGE,
    TYPE_CHUNK,
    TYPE_CHUNK_ACK,
    TYPE_RESUME_REQUEST,
    TYPE_CONTROL,
    TYPE_FINALIZE,
    TYPE_FINAL_ACK,
    TYPE_ERROR,
];

/// Smallest possible encoded [`WireEntry`]: 2 + 1 + 8 + 8 + 1 + 32 + 4.
const MIN_ENTRY_BYTES: usize = 56;
/// Encoded size of one [`ResumeEntry`].
const RESUME_ENTRY_BYTES: usize = 12;

/// Transfer session identifier. Deterministic from source identity so a
/// retried or resumed transfer reuses the same job id.
pub type JobId = [u8; 16];

/// Decode-time resource budgets. Enforced before allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_chunk_bytes: u32,
    pub max_entries_per_page: u16,
    pub max_string_bytes: u16,
    pub max_error_message_bytes: u16,
    pub max_resume_entries: u16,
    pub max_chunk_digests: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 8 << 20,
            max_entries_per_page: 256,
            max_string_bytes: 1024,
            max_error_message_bytes: 256,
            max_resume_entries: 4096,
            max_chunk_digests: 65536,
        }
    }
}

/// In-band flow control commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Cancel,
}

/// Manifest entry as it appears on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEntry {
    /// `/`-separated validated relative path.
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_unix_ms: i64,
    pub digest_alg: u8,
    pub whole_digest: [u8; DIGEST_LEN],
    pub chunk_digests: Vec<[u8; DIGEST_LEN]>,
}

impl From<&FileEntry> for WireEntry {
    fn from(entry: &FileEntry) -> Self {
        WireEntry {
            rel_path: entry.rel_path.to_string_lossy().replace('\\', "/"),
            is_dir: entry.is_dir,
            size: entry.size,
            mtime_unix_ms: entry.mtime_unix_ms,
            digest_alg: entry.digest_alg,
            whole_digest: entry.whole_digest,
            chunk_digests: entry.chunk_digests.clone(),
        }
    }
}

impl TryFrom<WireEntry> for FileEntry {
    type Error = ak_core::manifest::InvalidRelPath;

    fn try_from(entry: WireEntry) -> Result<Self, Self::Error> {
        if entry.is_dir {
            FileEntry::directory(&entry.rel_path)
        } else {
            FileEntry::new(
                &entry.rel_path,
                entry.size,
                entry.mtime_unix_ms,
                entry.whole_digest,
                entry.chunk_digests,
            )
        }
    }
}

/// Per-file resume state: length of the verified contiguous chunk prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeEntry {
    pub file_index: u32,
    pub verified_prefix_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Hello {
        proto_version: u16,
        device_name: String,
        public_key: [u8; 32],
        capabilities: u16,
    },
    Offer {
        job_id: JobId,
        root_name: String,
        file_count: u64,
        total_bytes: u64,
        chunk_size: u32,
    },
    Accept {
        job_id: JobId,
    },
    Reject {
        job_id: JobId,
        reason: ErrorCode,
    },
    ManifestPage {
        job_id: JobId,
        page_index: u32,
        last_page: bool,
        entries: Vec<WireEntry>,
    },
    Chunk {
        job_id: JobId,
        file_index: u32,
        chunk_index: u64,
        data: Vec<u8>,
    },
    ChunkAck {
        job_id: JobId,
        file_index: u32,
        chunk_index: u64,
    },
    ResumeRequest {
        job_id: JobId,
        files: Vec<ResumeEntry>,
    },
    Control {
        job_id: JobId,
        control: Control,
    },
    Finalize {
        job_id: JobId,
    },
    FinalAck {
        job_id: JobId,
        verified: bool,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub version: u8,
    pub message: Message,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame shorter than the {HEADER_LEN}-byte header")]
    TruncatedHeader,
    #[error("unsupported frame version {0}")]
    BadVersion(u8),
    #[error("unknown message type {0}")]
    UnknownType(u16),
    #[error("payload length {declared} exceeds budget {budget} or hard cap")]
    Oversized { declared: u32, budget: u32 },
    #[error("payload truncated: declared {declared}, present {present}")]
    TruncatedPayload { declared: u32, present: usize },
    #[error("string exceeds {limit} bytes or is not valid UTF-8")]
    BadString { limit: u16 },
    #[error("relative path rejected by validation")]
    BadPath(#[from] ak_core::manifest::InvalidRelPath),
    #[error("digest count {count} exceeds {limit}")]
    TooManyDigests { count: u64, limit: u32 },
    #[error("entry count {count} exceeds {limit}")]
    TooManyEntries { count: u64, limit: u16 },
    #[error("resume entry count {count} exceeds {limit}")]
    TooManyResumeEntries { count: u64, limit: u16 },
    #[error("chunk data length {len} exceeds {limit}")]
    ChunkTooLarge { len: u64, limit: u32 },
    #[error("error code {0} not in the taxonomy")]
    UnknownErrorCode(u16),
    #[error("leftover {0} trailing bytes after message body")]
    Trailing(usize),
    #[error("integer field out of range: {0}")]
    Malformed(&'static str),
}

impl Frame {
    /// Append the encoded frame to `out`. Encoding is total and must not fail;
    /// builders validate lengths before construction.
    ///
    /// The `debug_assert!`s below fire only on caller bugs: strings must fit
    /// the u16 length field, vectors and chunk data the u32 count/length, the
    /// frame must carry [`VERSION`], and the whole frame must stay within
    /// [`MAX_FRAME_BYTES`] (decode refuses larger frames, so producing one is
    /// pointless). Limits-specific bounds such as `max_chunk_bytes` are the
    /// builder's responsibility; encode takes no limits by design.
    pub fn encode(&self, out: &mut Vec<u8>) {
        debug_assert_eq!(
            self.version,
            VERSION,
            "ak-wire: this codec only encodes frame version 1"
        );
        let start = out.len();
        out.resize(start + HEADER_LEN, 0);
        encode_payload(&self.message, out);
        let payload_len = out.len() - start - HEADER_LEN;
        debug_assert!(
            payload_len <= u32::MAX as usize,
            "ak-wire: payload of {payload_len} bytes overflows the u32 length field; caller bug"
        );
        debug_assert!(
            out.len() - start <= MAX_FRAME_BYTES,
            "ak-wire: frame of {} bytes exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES}); \
             split the message or tighten limits — caller bug",
            out.len() - start
        );
        out[start] = self.version;
        out[start + 1..start + 3].copy_from_slice(&type_of(&self.message).to_be_bytes());
        out[start + 3..start + HEADER_LEN].copy_from_slice(&(payload_len as u32).to_be_bytes());
    }

    /// Decode one frame from exactly-sized bytes under the given limits.
    ///
    /// Validation order: header length, version, known type, declared payload
    /// length against the per-type budget implied by `limits` and
    /// [`MAX_FRAME_BYTES`] — all before any allocation — then payload
    /// presence, per-message budgets while parsing, and exact exhaustion of
    /// the payload.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader);
        }
        let version = bytes[0];
        if version != VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let message_type = u16::from_be_bytes([bytes[1], bytes[2]]);
        let declared = u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]);
        let Some(type_bound) = payload_budget(message_type, limits) else {
            return Err(DecodeError::UnknownType(message_type));
        };
        let budget = type_bound.min((MAX_FRAME_BYTES - HEADER_LEN) as u64);
        if u64::from(declared) > budget {
            return Err(DecodeError::Oversized {
                declared,
                budget: budget as u32,
            });
        }
        let present = bytes.len() - HEADER_LEN;
        if present < declared as usize {
            return Err(DecodeError::TruncatedPayload { declared, present });
        }
        if present > declared as usize {
            return Err(DecodeError::Trailing(present - declared as usize));
        }
        let mut cursor = Cursor::new(&bytes[HEADER_LEN..]);
        let message = decode_message(message_type, &mut cursor, limits)?;
        let leftover = cursor.remaining();
        if leftover != 0 {
            return Err(DecodeError::Trailing(leftover));
        }
        Ok(Frame { version, message })
    }

    /// Largest frame the given limits can produce, for buffer preallocation.
    ///
    /// Exact upper bound: header plus the biggest per-message payload the
    /// limits permit. Under the default limits the manifest-page bound
    /// dominates and exceeds [`MAX_FRAME_BYTES`] (which decode enforces);
    /// deployments should tighten `max_entries_per_page`/`max_chunk_digests`
    /// so manifest pages fit the cap, or size receive buffers by
    /// [`MAX_FRAME_BYTES`] directly.
    pub fn max_len(limits: &Limits) -> usize {
        let mut max_bound = 0u64;
        for message_type in TYPE_VALUES {
            if let Some(bound) = payload_budget(message_type, limits) {
                max_bound = max_bound.max(bound);
            }
        }
        HEADER_LEN + max_bound as usize
    }
}

/// Wire type value of a message; the closed set has no generic payload.
fn type_of(message: &Message) -> u16 {
    match message {
        Message::Hello { .. } => TYPE_HELLO,
        Message::Offer { .. } => TYPE_OFFER,
        Message::Accept { .. } => TYPE_ACCEPT,
        Message::Reject { .. } => TYPE_REJECT,
        Message::ManifestPage { .. } => TYPE_MANIFEST_PAGE,
        Message::Chunk { .. } => TYPE_CHUNK,
        Message::ChunkAck { .. } => TYPE_CHUNK_ACK,
        Message::ResumeRequest { .. } => TYPE_RESUME_REQUEST,
        Message::Control { .. } => TYPE_CONTROL,
        Message::Finalize { .. } => TYPE_FINALIZE,
        Message::FinalAck { .. } => TYPE_FINAL_ACK,
        Message::Error { .. } => TYPE_ERROR,
    }
}

/// Exact payload upper bound for one message type under `limits`, or `None`
/// for types outside the frozen set. Pure arithmetic on limits, no allocation.
fn payload_budget(message_type: u16, limits: &Limits) -> Option<u64> {
    let string = u64::from(limits.max_string_bytes);
    // WireEntry: str(2+string) + bool(1) + size(8) + mtime(8) + alg(1)
    //            + whole_digest(32) + digests(4 + 32*n).
    let entry = 56 + string + 32 * u64::from(limits.max_chunk_digests);
    Some(match message_type {
        // proto_version(2) + str(2+string) + public_key(32) + capabilities(2).
        TYPE_HELLO | TYPE_OFFER => 38 + string,
        TYPE_ACCEPT => 16,
        TYPE_REJECT => 18,
        // job_id(16) + page_index(4) + last_page(1) + count(4) + entries.
        TYPE_MANIFEST_PAGE => 25 + u64::from(limits.max_entries_per_page) * entry,
        // job_id(16) + file_index(4) + chunk_index(8) + data_len(4) + data.
        TYPE_CHUNK => 32 + u64::from(limits.max_chunk_bytes),
        // job_id(16) + file_index(4) + chunk_index(8).
        TYPE_CHUNK_ACK => 28,
        // job_id(16) + count(4) + 12 bytes per resume entry.
        TYPE_RESUME_REQUEST => 20 + 12 * u64::from(limits.max_resume_entries),
        // job_id(16) + tag/bool(1).
        TYPE_CONTROL | TYPE_FINAL_ACK => 17,
        TYPE_FINALIZE => 16,
        // code(2) + str(2 + error message bytes).
        TYPE_ERROR => 4 + u64::from(limits.max_error_message_bytes),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Encoding helpers (total; caller bugs surface as debug_assert panics).
// ---------------------------------------------------------------------------

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    put_u64(out, value as u64);
}

fn put_bool(out: &mut Vec<u8>, value: bool) {
    out.push(u8::from(value));
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    debug_assert!(
        s.len() <= usize::from(u16::MAX),
        "ak-wire: string field of {} bytes overflows the u16 length field; caller bug",
        s.len()
    );
    put_u16(out, s.len() as u16);
    out.extend_from_slice(s.as_bytes());
}

/// Vector counts and chunk data share the u32 length prefix.
fn put_len32(out: &mut Vec<u8>, len: usize) {
    debug_assert!(
        len <= u32::MAX as usize,
        "ak-wire: length {len} overflows the u32 count field; caller bug"
    );
    put_u32(out, len as u32);
}

fn encode_payload(message: &Message, out: &mut Vec<u8>) {
    match message {
        Message::Hello {
            proto_version,
            device_name,
            public_key,
            capabilities,
        } => {
            put_u16(out, *proto_version);
            put_str(out, device_name);
            out.extend_from_slice(public_key);
            put_u16(out, *capabilities);
        }
        Message::Offer {
            job_id,
            root_name,
            file_count,
            total_bytes,
            chunk_size,
        } => {
            out.extend_from_slice(job_id);
            put_str(out, root_name);
            put_u64(out, *file_count);
            put_u64(out, *total_bytes);
            put_u32(out, *chunk_size);
        }
        Message::Accept { job_id } => out.extend_from_slice(job_id),
        Message::Reject { job_id, reason } => {
            out.extend_from_slice(job_id);
            put_u16(out, reason.code());
        }
        Message::ManifestPage {
            job_id,
            page_index,
            last_page,
            entries,
        } => {
            out.extend_from_slice(job_id);
            put_u32(out, *page_index);
            put_bool(out, *last_page);
            put_len32(out, entries.len());
            for entry in entries {
                put_str(out, &entry.rel_path);
                put_bool(out, entry.is_dir);
                put_u64(out, entry.size);
                put_i64(out, entry.mtime_unix_ms);
                out.push(entry.digest_alg);
                out.extend_from_slice(&entry.whole_digest);
                put_len32(out, entry.chunk_digests.len());
                for digest in &entry.chunk_digests {
                    out.extend_from_slice(digest);
                }
            }
        }
        Message::Chunk {
            job_id,
            file_index,
            chunk_index,
            data,
        } => {
            out.extend_from_slice(job_id);
            put_u32(out, *file_index);
            put_u64(out, *chunk_index);
            put_len32(out, data.len());
            out.extend_from_slice(data);
        }
        Message::ChunkAck {
            job_id,
            file_index,
            chunk_index,
        } => {
            out.extend_from_slice(job_id);
            put_u32(out, *file_index);
            put_u64(out, *chunk_index);
        }
        Message::ResumeRequest { job_id, files } => {
            out.extend_from_slice(job_id);
            put_len32(out, files.len());
            for file in files {
                put_u32(out, file.file_index);
                put_u64(out, file.verified_prefix_chunks);
            }
        }
        Message::Control { job_id, control } => {
            out.extend_from_slice(job_id);
            let tag = match control {
                Control::Pause => CONTROL_PAUSE,
                Control::Cancel => CONTROL_CANCEL,
            };
            out.push(tag);
        }
        Message::Finalize { job_id } => out.extend_from_slice(job_id),
        Message::FinalAck { job_id, verified } => {
            out.extend_from_slice(job_id);
            put_bool(out, *verified);
        }
        Message::Error { code, message } => {
            put_u16(out, code.code());
            put_str(out, message);
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding: bounds-checked cursor, budgets before allocation.
// ---------------------------------------------------------------------------

/// Bounds-checked read cursor over one payload. Running past the end is
/// [`DecodeError::Malformed`]; every length is checked before the caller
/// allocates anything.
struct Cursor<'a> {
    payload: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(payload: &'a [u8]) -> Self {
        Cursor { payload, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.payload.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(DecodeError::Malformed("field length overflows the cursor"))?;
        if end > self.payload.len() {
            return Err(DecodeError::Malformed("payload ends inside a field"));
        }
        let payload: &'a [u8] = self.payload;
        let slice = &payload[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Two's complement bit pattern, byte-exact in both directions.
    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(self.u64()? as i64)
    }

    fn boolean(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::Malformed("boolean byte must be 0 or 1")),
        }
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut arr = [0u8; N];
        arr.copy_from_slice(self.take(N)?);
        Ok(arr)
    }
}

/// `[len u16][bytes]`: length checked against `limit`, then presence, then
/// UTF-8 — all before the `String` is allocated.
fn limited_string(cursor: &mut Cursor, limit: u16) -> Result<String, DecodeError> {
    let len = cursor.u16()? as usize;
    if len > usize::from(limit) {
        return Err(DecodeError::BadString { limit });
    }
    let bytes = cursor.take(len)?;
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => Err(DecodeError::BadString { limit }),
    }
}

/// u16 restricted to the ak-core error taxonomy.
fn error_code(cursor: &mut Cursor) -> Result<ErrorCode, DecodeError> {
    let code = cursor.u16()?;
    ErrorCode::from_code(code).ok_or(DecodeError::UnknownErrorCode(code))
}

/// Decode one WireEntry; rel_path is validated via ak-core after the string
/// limits pass, and the digest-vector budget is checked before allocation.
fn wire_entry(cursor: &mut Cursor, limits: &Limits) -> Result<WireEntry, DecodeError> {
    let rel_path = limited_string(cursor, limits.max_string_bytes)?;
    validate_rel_path(&rel_path)?;
    let is_dir = cursor.boolean()?;
    let size = cursor.u64()?;
    let mtime_unix_ms = cursor.i64()?;
    let digest_alg = cursor.u8()?;
    let whole_digest = cursor.array::<DIGEST_LEN>()?;
    let count = u64::from(cursor.u32()?);
    if count > u64::from(limits.max_chunk_digests) {
        return Err(DecodeError::TooManyDigests {
            count,
            limit: limits.max_chunk_digests,
        });
    }
    // Never preallocate past what the remaining payload can actually hold.
    let capacity = (count as usize).min(cursor.remaining() / DIGEST_LEN);
    let mut chunk_digests = Vec::with_capacity(capacity);
    for _ in 0..count {
        chunk_digests.push(cursor.array::<DIGEST_LEN>()?);
    }
    Ok(WireEntry {
        rel_path,
        is_dir,
        size,
        mtime_unix_ms,
        digest_alg,
        whole_digest,
        chunk_digests,
    })
}

fn decode_message(
    message_type: u16,
    cursor: &mut Cursor,
    limits: &Limits,
) -> Result<Message, DecodeError> {
    Ok(match message_type {
        TYPE_HELLO => Message::Hello {
            proto_version: cursor.u16()?,
            device_name: limited_string(cursor, limits.max_string_bytes)?,
            public_key: cursor.array::<32>()?,
            capabilities: cursor.u16()?,
        },
        TYPE_OFFER => Message::Offer {
            job_id: cursor.array::<16>()?,
            root_name: limited_string(cursor, limits.max_string_bytes)?,
            file_count: cursor.u64()?,
            total_bytes: cursor.u64()?,
            chunk_size: cursor.u32()?,
        },
        TYPE_ACCEPT => Message::Accept {
            job_id: cursor.array::<16>()?,
        },
        TYPE_REJECT => Message::Reject {
            job_id: cursor.array::<16>()?,
            reason: error_code(cursor)?,
        },
        TYPE_MANIFEST_PAGE => {
            let job_id: JobId = cursor.array::<16>()?;
            let page_index = cursor.u32()?;
            let last_page = cursor.boolean()?;
            let count = u64::from(cursor.u32()?);
            if count > u64::from(limits.max_entries_per_page) {
                return Err(DecodeError::TooManyEntries {
                    count,
                    limit: limits.max_entries_per_page,
                });
            }
            let capacity = (count as usize).min(cursor.remaining() / MIN_ENTRY_BYTES);
            let mut entries = Vec::with_capacity(capacity);
            for _ in 0..count {
                entries.push(wire_entry(cursor, limits)?);
            }
            Message::ManifestPage {
                job_id,
                page_index,
                last_page,
                entries,
            }
        }
        TYPE_CHUNK => {
            let job_id: JobId = cursor.array::<16>()?;
            let file_index = cursor.u32()?;
            let chunk_index = cursor.u64()?;
            let len = u64::from(cursor.u32()?);
            if len > u64::from(limits.max_chunk_bytes) {
                return Err(DecodeError::ChunkTooLarge {
                    len,
                    limit: limits.max_chunk_bytes,
                });
            }
            let data = cursor.take(len as usize)?.to_vec();
            Message::Chunk {
                job_id,
                file_index,
                chunk_index,
                data,
            }
        }
        TYPE_CHUNK_ACK => Message::ChunkAck {
            job_id: cursor.array::<16>()?,
            file_index: cursor.u32()?,
            chunk_index: cursor.u64()?,
        },
        TYPE_RESUME_REQUEST => {
            let job_id: JobId = cursor.array::<16>()?;
            let count = u64::from(cursor.u32()?);
            if count > u64::from(limits.max_resume_entries) {
                return Err(DecodeError::TooManyResumeEntries {
                    count,
                    limit: limits.max_resume_entries,
                });
            }
            let capacity = (count as usize).min(cursor.remaining() / RESUME_ENTRY_BYTES);
            let mut files = Vec::with_capacity(capacity);
            for _ in 0..count {
                files.push(ResumeEntry {
                    file_index: cursor.u32()?,
                    verified_prefix_chunks: cursor.u64()?,
                });
            }
            Message::ResumeRequest { job_id, files }
        }
        TYPE_CONTROL => {
            let job_id: JobId = cursor.array::<16>()?;
            let control = match cursor.u8()? {
                CONTROL_PAUSE => Control::Pause,
                CONTROL_CANCEL => Control::Cancel,
                _ => return Err(DecodeError::Malformed("unknown control tag")),
            };
            Message::Control { job_id, control }
        }
        TYPE_FINALIZE => Message::Finalize {
            job_id: cursor.array::<16>()?,
        },
        TYPE_FINAL_ACK => Message::FinalAck {
            job_id: cursor.array::<16>()?,
            verified: cursor.boolean()?,
        },
        TYPE_ERROR => Message::Error {
            code: error_code(cursor)?,
            message: limited_string(cursor, limits.max_error_message_bytes)?,
        },
        _ => return Err(DecodeError::UnknownType(message_type)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic LCG (Knuth constants) for fuzz and message generation.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn byte(&mut self) -> u8 {
            (self.next_u64() >> 33) as u8
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }
    }

    fn digest(seed: u8) -> [u8; DIGEST_LEN] {
        core::array::from_fn(|i| seed.wrapping_add(i as u8))
    }

    fn file_entry(path: &str, seed: u8, digests: usize) -> WireEntry {
        WireEntry {
            rel_path: path.to_owned(),
            is_dir: false,
            size: 1 << 40,
            mtime_unix_ms: i64::MAX,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: digest(seed),
            chunk_digests: (0..digests)
                .map(|i| digest(seed.wrapping_add(i as u8)))
                .collect(),
        }
    }

    fn encoded(message: &Message) -> Vec<u8> {
        let mut buf = Vec::new();
        Frame {
            version: VERSION,
            message: message.clone(),
        }
        .encode(&mut buf);
        buf
    }

    /// Assemble a frame with arbitrary payload for error-branch tests.
    fn frame_of(message_type: u16, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
        buf.push(VERSION);
        buf.extend_from_slice(&message_type.to_be_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    fn assert_round_trips(message: Message) {
        let buf = encoded(&message);
        let decoded = Frame::decode(&buf, &Limits::default()).expect("valid frame decodes");
        assert_eq!(decoded.version, VERSION);
        assert_eq!(decoded.message, message);
    }

    // ------------------------------------------------------------------
    // Round trips, including empty/full boundaries.
    // ------------------------------------------------------------------

    #[test]
    fn hello_round_trip_with_max_length_string() {
        let name = "d".repeat(Limits::default().max_string_bytes as usize);
        assert_round_trips(Message::Hello {
            proto_version: 1,
            device_name: name,
            public_key: digest(7),
            capabilities: CAP_RESUME,
        });
        assert_round_trips(Message::Hello {
            proto_version: 1,
            device_name: String::new(),
            public_key: [0; 32],
            capabilities: 0,
        });
    }

    #[test]
    fn offer_round_trip_preserves_u64_max() {
        assert_round_trips(Message::Offer {
            job_id: [9; 16],
            root_name: "photos 2026".to_owned(),
            file_count: u64::MAX,
            total_bytes: u64::MAX,
            chunk_size: 8 << 20,
        });
        assert_round_trips(Message::Offer {
            job_id: [0; 16],
            root_name: String::new(),
            file_count: 0,
            total_bytes: 0,
            chunk_size: 1,
        });
    }

    #[test]
    fn accept_reject_finalize_round_trip_all_error_codes() {
        assert_round_trips(Message::Accept { job_id: [2; 16] });
        assert_round_trips(Message::Finalize { job_id: [3; 16] });
        for code in 1..=14u16 {
            let reason = ErrorCode::from_code(code).unwrap();
            assert_round_trips(Message::Reject {
                job_id: [1; 16],
                reason,
            });
        }
    }

    #[test]
    fn manifest_page_round_trip_empty_and_multi_page() {
        // Empty manifest: exactly one empty page; also a mid stream page.
        assert_round_trips(Message::ManifestPage {
            job_id: [4; 16],
            page_index: 0,
            last_page: true,
            entries: Vec::new(),
        });
        assert_round_trips(Message::ManifestPage {
            job_id: [4; 16],
            page_index: 1,
            last_page: false,
            entries: Vec::new(),
        });
    }

    #[test]
    fn manifest_page_round_trip_dir_and_file_extremes() {
        let dir = WireEntry {
            rel_path: "empty-dir".to_owned(),
            is_dir: true,
            size: 0,
            mtime_unix_ms: 0,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: [0; DIGEST_LEN],
            chunk_digests: Vec::new(),
        };
        let empty_file = file_entry("root/empty.bin", 1, 0); // empty file, 0 chunks
        let mut big = file_entry("root/a/deep/big.iso", 2, 3);
        big.size = u64::MAX;
        let mut oldest = file_entry("root/old.txt", 3, 1);
        oldest.mtime_unix_ms = i64::MIN;
        let mut newest = file_entry("root/new.txt", 4, 1);
        newest.mtime_unix_ms = i64::MAX;
        newest.size = 0;
        assert_round_trips(Message::ManifestPage {
            job_id: [5; 16],
            page_index: 2,
            last_page: true,
            entries: vec![dir, empty_file, big, oldest, newest],
        });
    }

    #[test]
    fn manifest_page_round_trip_full_page_boundary() {
        let limits = Limits::default();
        let entries: Vec<WireEntry> = (0..limits.max_entries_per_page)
            .map(|i| file_entry(&format!("f{i}.dat"), (i % 256) as u8, 0))
            .collect();
        let payload = 25 + entries
            .iter()
            .map(|e| MIN_ENTRY_BYTES + e.rel_path.len())
            .sum::<usize>();
        let message = Message::ManifestPage {
            job_id: [6; 16],
            page_index: 0,
            last_page: true,
            entries,
        };
        let buf = encoded(&message);
        assert_eq!(buf.len(), HEADER_LEN + payload);
        assert_eq!(Frame::decode(&buf, &limits).unwrap().message, message);
    }

    #[test]
    fn manifest_entry_accepts_digest_count_at_limit() {
        let limits = Limits::default();
        let entry = WireEntry {
            rel_path: "huge.bin".to_owned(),
            is_dir: false,
            size: u64::MAX,
            mtime_unix_ms: 1,
            digest_alg: 0,
            whole_digest: digest(1),
            chunk_digests: vec![digest(2); limits.max_chunk_digests as usize],
        };
        let message = Message::ManifestPage {
            job_id: [7; 16],
            page_index: 0,
            last_page: true,
            entries: vec![entry],
        };
        let buf = encoded(&message);
        assert_eq!(Frame::decode(&buf, &limits).unwrap().message, message);
    }

    #[test]
    fn chunk_round_trip_empty_max_data_and_u64_index() {
        assert_round_trips(Message::Chunk {
            job_id: [8; 16],
            file_index: u32::MAX,
            chunk_index: u64::MAX,
            data: Vec::new(),
        });
        // Chunk data of exactly max_chunk_bytes sits exactly on the budget.
        let limits = Limits::default();
        let message = Message::Chunk {
            job_id: [8; 16],
            file_index: 0,
            chunk_index: u64::MAX,
            data: vec![0xAB; limits.max_chunk_bytes as usize],
        };
        let buf = encoded(&message);
        assert_eq!(
            buf.len(),
            HEADER_LEN + 32 + limits.max_chunk_bytes as usize
        );
        assert_eq!(Frame::decode(&buf, &limits).unwrap().message, message);
    }

    #[test]
    fn chunk_ack_control_final_ack_round_trips() {
        assert_round_trips(Message::ChunkAck {
            job_id: [1; 16],
            file_index: 7,
            chunk_index: u64::MAX,
        });
        assert_round_trips(Message::Control {
            job_id: [3; 16],
            control: Control::Pause,
        });
        assert_round_trips(Message::Control {
            job_id: [3; 16],
            control: Control::Cancel,
        });
        assert_round_trips(Message::FinalAck {
            job_id: [4; 16],
            verified: true,
        });
        assert_round_trips(Message::FinalAck {
            job_id: [4; 16],
            verified: false,
        });
    }

    #[test]
    fn resume_request_round_trip_empty_and_entries() {
        assert_round_trips(Message::ResumeRequest {
            job_id: [2; 16],
            files: Vec::new(),
        });
        assert_round_trips(Message::ResumeRequest {
            job_id: [2; 16],
            files: vec![
                ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: u64::MAX,
                },
                ResumeEntry {
                    file_index: u32::MAX,
                    verified_prefix_chunks: 0,
                },
            ],
        });
    }

    #[test]
    fn resume_request_accepts_count_at_limit() {
        let limits = Limits::default();
        let files: Vec<ResumeEntry> = (0..limits.max_resume_entries as u32)
            .map(|i| ResumeEntry {
                file_index: i,
                verified_prefix_chunks: u64::from(i),
            })
            .collect();
        let message = Message::ResumeRequest {
            job_id: [9; 16],
            files,
        };
        let buf = encoded(&message);
        assert_eq!(
            buf.len(),
            HEADER_LEN + 20 + 12 * limits.max_resume_entries as usize
        );
        assert_eq!(Frame::decode(&buf, &limits).unwrap().message, message);
    }

    #[test]
    fn error_round_trip_with_max_length_message() {
        let limits = Limits::default();
        assert_round_trips(Message::Error {
            code: ErrorCode::HashMismatch,
            message: "e".repeat(limits.max_error_message_bytes as usize),
        });
        assert_round_trips(Message::Error {
            code: ErrorCode::Internal,
            message: String::new(),
        });
    }

    #[test]
    fn wire_entry_conversions_preserve_file_entries() {
        let entry =
            FileEntry::new("root/sub/name.bin", 123, -456, digest(3), vec![digest(4)]).unwrap();
        let wire = WireEntry::from(&entry);
        assert_eq!(wire.rel_path, "root/sub/name.bin");
        assert_eq!(FileEntry::try_from(wire).unwrap(), entry);
    }

    #[test]
    fn message_type_constants_form_the_closed_set() {
        for (index, message_type) in TYPE_VALUES.iter().enumerate() {
            assert_eq!(*message_type, index as u16 + 1);
        }
        assert_eq!(TYPE_VALUES.len(), 12);
    }

    // ------------------------------------------------------------------
    // max_len.
    // ------------------------------------------------------------------

    #[test]
    fn max_len_is_the_exact_upper_bound() {
        let defaults = Limits::default();
        // Manifest pages dominate under the default limits.
        let entry_bound =
            56 + u64::from(defaults.max_string_bytes) + 32 * u64::from(defaults.max_chunk_digests);
        let page_bound = 25 + u64::from(defaults.max_entries_per_page) * entry_bound;
        assert_eq!(Frame::max_len(&defaults), HEADER_LEN + page_bound as usize);

        // Chunk-dominated configuration.
        let chunky = Limits {
            max_chunk_bytes: 4096,
            max_entries_per_page: 1,
            max_string_bytes: 16,
            max_error_message_bytes: 16,
            max_resume_entries: 1,
            max_chunk_digests: 1,
        };
        assert_eq!(Frame::max_len(&chunky), HEADER_LEN + 32 + 4096);
    }

    #[test]
    fn max_len_covers_every_encoded_message() {
        let limits = Limits::default();
        let samples = vec![
            Message::Hello {
                proto_version: 1,
                device_name: "device".to_owned(),
                public_key: digest(1),
                capabilities: CAP_RESUME,
            },
            Message::Offer {
                job_id: [1; 16],
                root_name: "root".to_owned(),
                file_count: 2,
                total_bytes: 1 << 40,
                chunk_size: 1 << 20,
            },
            Message::Accept { job_id: [1; 16] },
            Message::Reject {
                job_id: [1; 16],
                reason: ErrorCode::AuthFailed,
            },
            Message::ManifestPage {
                job_id: [1; 16],
                page_index: 0,
                last_page: true,
                entries: vec![file_entry("a/b.txt", 1, 2)],
            },
            Message::Chunk {
                job_id: [1; 16],
                file_index: 3,
                chunk_index: 4,
                data: vec![1, 2, 3],
            },
            Message::ChunkAck {
                job_id: [1; 16],
                file_index: 3,
                chunk_index: 4,
            },
            Message::ResumeRequest {
                job_id: [1; 16],
                files: vec![ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: 1,
                }],
            },
            Message::Control {
                job_id: [1; 16],
                control: Control::Cancel,
            },
            Message::Finalize { job_id: [1; 16] },
            Message::FinalAck {
                job_id: [1; 16],
                verified: true,
            },
            Message::Error {
                code: ErrorCode::StorageFull,
                message: "no space".to_owned(),
            },
        ];
        let bound = Frame::max_len(&limits);
        for message in samples {
            let buf = encoded(&message);
            assert!(buf.len() <= bound, "frame of {} exceeds max_len", buf.len());
            Frame::decode(&buf, &limits).unwrap();
        }
    }

    // ------------------------------------------------------------------
    // Error branches.
    // ------------------------------------------------------------------

    #[test]
    fn rejects_headers_shorter_than_seven_bytes() {
        for len in [0usize, 1, 3, 6] {
            let err = Frame::decode(&vec![0u8; len], &Limits::default());
            assert!(
                matches!(err, Err(DecodeError::TruncatedHeader)),
                "len {len} accepted"
            );
        }
    }

    #[test]
    fn rejects_bad_versions() {
        let mut buf = encoded(&Message::Accept { job_id: [1; 16] });
        for bad in [0u8, 2, 255] {
            buf[0] = bad;
            assert!(matches!(
                Frame::decode(&buf, &Limits::default()),
                Err(DecodeError::BadVersion(v)) if v == bad
            ));
        }
        buf[0] = VERSION;
        assert!(Frame::decode(&buf, &Limits::default()).is_ok());
    }

    #[test]
    fn rejects_unknown_types() {
        for bad in [0u16, 13, 999, u16::MAX] {
            let buf = frame_of(bad, &[0; 16]);
            assert!(matches!(
                Frame::decode(&buf, &Limits::default()),
                Err(DecodeError::UnknownType(t)) if t == bad
            ));
        }
    }

    #[test]
    fn rejects_oversized_declared_payload() {
        // Chunk budget with defaults: 32 + 8 MiB.
        let chunk_budget = 32 + Limits::default().max_chunk_bytes;
        let mut buf = frame_of(TYPE_CHUNK, &[]);
        buf[3..7].copy_from_slice(&(chunk_budget + 1).to_be_bytes());
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::Oversized { declared, budget })
                if declared == chunk_budget + 1 && budget == chunk_budget
        ));

        // Fixed-size Accept has budget 16; the check fires before presence.
        let mut buf = frame_of(TYPE_ACCEPT, &[0; 8]);
        buf[3..7].copy_from_slice(&17u32.to_be_bytes());
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::Oversized { declared: 17, budget: 16 })
        ));

        // Even generous limits are clamped by the hard frame cap.
        let generous = Limits {
            max_chunk_bytes: u32::MAX,
            ..Limits::default()
        };
        let cap = (MAX_FRAME_BYTES - HEADER_LEN) as u32;
        let mut buf = frame_of(TYPE_CHUNK, &[]);
        buf[3..7].copy_from_slice(&(cap + 1).to_be_bytes());
        assert!(matches!(
            Frame::decode(&buf, &generous),
            Err(DecodeError::Oversized { budget, .. }) if budget == cap
        ));
    }

    #[test]
    fn rejects_truncated_payload() {
        let buf = encoded(&Message::Hello {
            proto_version: 1,
            device_name: "dev".to_owned(),
            public_key: [1; 32],
            capabilities: 0,
        });
        let full = buf.len();
        let mut cut = buf.clone();
        cut.truncate(full - 10);
        assert!(matches!(
            Frame::decode(&cut, &Limits::default()),
            Err(DecodeError::TruncatedPayload { declared, present })
                if declared as usize == full - HEADER_LEN && present == full - 10 - HEADER_LEN
        ));
    }

    #[test]
    fn rejects_trailing_bytes_inside_and_after_payload() {
        let buf = encoded(&Message::Accept { job_id: [1; 16] });
        // One extra byte after the declared frame.
        let mut outer = buf.clone();
        outer.push(0);
        assert!(matches!(
            Frame::decode(&outer, &Limits::default()),
            Err(DecodeError::Trailing(1))
        ));
        // One extra byte covered by the declared length of a variable-size
        // message (Hello's budget is large enough to admit it), leaving the
        // payload non-exhausted after parsing.
        let mut inner = encoded(&Message::Hello {
            proto_version: 1,
            device_name: "dev".to_owned(),
            public_key: [1; 32],
            capabilities: 0,
        });
        inner.push(0);
        let declared = (inner.len() - HEADER_LEN) as u32;
        inner[3..7].copy_from_slice(&declared.to_be_bytes());
        assert!(matches!(
            Frame::decode(&inner, &Limits::default()),
            Err(DecodeError::Trailing(1))
        ));
    }

    #[test]
    fn rejects_bad_strings() {
        let limits = Limits::default();
        // rel_path one byte past the string limit, inside a manifest page
        // (its large budget keeps the frame out of the Oversized branch).
        let entry = file_entry(&"a".repeat(limits.max_string_bytes as usize + 1), 1, 0);
        let buf = encoded(&Message::ManifestPage {
            job_id: [1; 16],
            page_index: 0,
            last_page: true,
            entries: vec![entry],
        });
        assert!(matches!(
            Frame::decode(&buf, &limits),
            Err(DecodeError::BadString { limit: 1024 })
        ));

        // Hello's budget is tight: an over-long device name is Oversized
        // before any string is parsed.
        let hello = Message::Hello {
            proto_version: 1,
            device_name: "a".repeat(limits.max_string_bytes as usize + 1),
            public_key: [1; 32],
            capabilities: 0,
        };
        assert!(matches!(
            Frame::decode(&encoded(&hello), &limits),
            Err(DecodeError::Oversized { .. })
        ));

        // Invalid UTF-8 in device_name.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u16.to_be_bytes()); // proto_version
        payload.extend_from_slice(&2u16.to_be_bytes()); // device_name len
        payload.extend_from_slice(&[0xFF, 0xFE]); // not UTF-8
        payload.extend_from_slice(&[0u8; 32]); // public_key
        payload.extend_from_slice(&CAP_RESUME.to_be_bytes());
        let buf = frame_of(TYPE_HELLO, &payload);
        assert!(matches!(
            Frame::decode(&buf, &limits),
            Err(DecodeError::BadString { limit: 1024 })
        ));
    }

    #[test]
    fn rejects_bad_relative_paths() {
        for bad in ["../up", "a\\b", "a\u{0}b", "/abs", "trailing/", "CON", "a/./b"] {
            let entry = file_entry(bad, 1, 0);
            let buf = encoded(&Message::ManifestPage {
                job_id: [1; 16],
                page_index: 0,
                last_page: true,
                entries: vec![entry],
            });
            assert!(
                matches!(
                    Frame::decode(&buf, &Limits::default()),
                    Err(DecodeError::BadPath(_))
                ),
                "path {bad:?} accepted"
            );
        }
    }

    #[test]
    fn rejects_entry_count_over_limit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]); // job_id
        payload.extend_from_slice(&0u32.to_be_bytes()); // page_index
        payload.push(1); // last_page
        payload.extend_from_slice(&257u32.to_be_bytes()); // count > 256
        let buf = frame_of(TYPE_MANIFEST_PAGE, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::TooManyEntries { count: 257, limit: 256 })
        ));
    }

    #[test]
    fn rejects_digest_count_over_limit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]); // job_id
        payload.extend_from_slice(&0u32.to_be_bytes()); // page_index
        payload.push(1); // last_page
        payload.extend_from_slice(&1u32.to_be_bytes()); // 1 entry
        payload.extend_from_slice(&1u16.to_be_bytes()); // rel_path len
        payload.push(b'a'); // rel_path
        payload.push(0); // is_dir
        payload.extend_from_slice(&0u64.to_be_bytes()); // size
        payload.extend_from_slice(&0i64.to_be_bytes()); // mtime
        payload.push(0); // digest_alg
        payload.extend_from_slice(&[0u8; DIGEST_LEN]); // whole_digest
        let over = Limits::default().max_chunk_digests + 1;
        payload.extend_from_slice(&over.to_be_bytes()); // digest count
        let buf = frame_of(TYPE_MANIFEST_PAGE, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::TooManyDigests { count, limit })
                if count == 65537 && limit == 65536
        ));
    }

    #[test]
    fn rejects_chunk_over_limit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]); // job_id
        payload.extend_from_slice(&0u32.to_be_bytes()); // file_index
        payload.extend_from_slice(&0u64.to_be_bytes()); // chunk_index
        let over = Limits::default().max_chunk_bytes + 1;
        payload.extend_from_slice(&over.to_be_bytes()); // data_len
        let buf = frame_of(TYPE_CHUNK, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::ChunkTooLarge { len, limit })
                if len == 8_388_609 && limit == 8_388_608
        ));
    }

    #[test]
    fn rejects_resume_count_over_limit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]); // job_id
        payload.extend_from_slice(&4097u32.to_be_bytes()); // count > 4096
        let buf = frame_of(TYPE_RESUME_REQUEST, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::TooManyResumeEntries { count: 4097, limit: 4096 })
        ));
    }

    #[test]
    fn rejects_unknown_error_codes() {
        for bad in [0u16, 15, u16::MAX] {
            let reject_payload = [vec![0u8; 16], bad.to_be_bytes().to_vec()].concat();
            let buf = frame_of(TYPE_REJECT, &reject_payload);
            assert!(matches!(
                Frame::decode(&buf, &Limits::default()),
                Err(DecodeError::UnknownErrorCode(c)) if c == bad
            ));

            let error_payload = [bad.to_be_bytes().to_vec(), 0u16.to_be_bytes().to_vec()].concat();
            let buf = frame_of(TYPE_ERROR, &error_payload);
            assert!(matches!(
                Frame::decode(&buf, &Limits::default()),
                Err(DecodeError::UnknownErrorCode(c)) if c == bad
            ));
        }
    }

    #[test]
    fn rejects_non_boolean_bytes() {
        // FinalAck.verified = 2.
        let mut payload = vec![0u8; 16];
        payload.push(2);
        let buf = frame_of(TYPE_FINAL_ACK, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::Malformed(_))
        ));

        // ManifestPage.last_page = 0xFF.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]);
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.push(0xFF);
        payload.extend_from_slice(&0u32.to_be_bytes());
        let buf = frame_of(TYPE_MANIFEST_PAGE, &payload);
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_unknown_control_tags() {
        for bad in [0u8, 3, 255] {
            let mut payload = vec![0u8; 16];
            payload.push(bad);
            let buf = frame_of(TYPE_CONTROL, &payload);
            assert!(
                matches!(
                    Frame::decode(&buf, &Limits::default()),
                    Err(DecodeError::Malformed(_))
                ),
                "tag {bad} accepted"
            );
        }
    }

    #[test]
    fn rejects_payload_ending_inside_a_field() {
        // Valid page, then one byte cut from both payload and declaration.
        let mut buf = encoded(&Message::ManifestPage {
            job_id: [1; 16],
            page_index: 0,
            last_page: true,
            entries: vec![file_entry("a.txt", 1, 1)],
        });
        buf.pop();
        let declared = (buf.len() - HEADER_LEN) as u32;
        buf[3..7].copy_from_slice(&declared.to_be_bytes());
        assert!(matches!(
            Frame::decode(&buf, &Limits::default()),
            Err(DecodeError::Malformed(_))
        ));
    }

    // ------------------------------------------------------------------
    // Deterministic fuzz: random and mutated frames never panic.
    // ------------------------------------------------------------------

    fn random_string(r: &mut Lcg, max: usize) -> String {
        let len = r.below(max as u64 + 1) as usize;
        (0..len)
            .map(|_| (b'a' + r.byte() % 26) as char)
            .collect()
    }

    fn random_message(r: &mut Lcg) -> Message {
        let job: JobId = [r.byte(); 16];
        match r.below(12) {
            0 => {
                let name = random_string(r, 16);
                let key = digest(r.byte());
                Message::Hello {
                    proto_version: 1,
                    device_name: name,
                    public_key: key,
                    capabilities: r.next_u64() as u16,
                }
            }
            1 => {
                let root = random_string(r, 16);
                Message::Offer {
                    job_id: job,
                    root_name: root,
                    file_count: r.next_u64(),
                    total_bytes: r.next_u64(),
                    chunk_size: r.below(1 << 20) as u32,
                }
            }
            2 => Message::Accept { job_id: job },
            3 => {
                let reason = ErrorCode::from_code(1 + r.below(14) as u16).unwrap();
                Message::Reject {
                    job_id: job,
                    reason,
                }
            }
            4 => {
                let count = r.below(3);
                let mut entries = Vec::new();
                for _ in 0..count {
                    let mut name = random_string(r, 4);
                    if name.is_empty() {
                        name.push('d'); // a leading '/' would make the path rooted
                    }
                    let rel_path = format!("{name}/f{}", r.byte());
                    let digests = r.below(3);
                    entries.push(WireEntry {
                        rel_path,
                        is_dir: r.byte().is_multiple_of(2),
                        size: r.next_u64(),
                        mtime_unix_ms: r.next_u64() as i64,
                        digest_alg: r.byte(),
                        whole_digest: digest(r.byte()),
                        chunk_digests: (0..digests).map(|_| digest(r.byte())).collect(),
                    });
                }
                Message::ManifestPage {
                    job_id: job,
                    page_index: r.below(100) as u32,
                    last_page: r.byte().is_multiple_of(2),
                    entries,
                }
            }
            5 => {
                let data = (0..r.below(64)).map(|_| r.byte()).collect();
                Message::Chunk {
                    job_id: job,
                    file_index: r.below(100) as u32,
                    chunk_index: r.next_u64(),
                    data,
                }
            }
            6 => Message::ChunkAck {
                job_id: job,
                file_index: r.below(100) as u32,
                chunk_index: r.next_u64(),
            },
            7 => {
                let count = r.below(4);
                let mut files = Vec::new();
                for _ in 0..count {
                    files.push(ResumeEntry {
                        file_index: r.below(100) as u32,
                        verified_prefix_chunks: r.next_u64(),
                    });
                }
                Message::ResumeRequest { job_id: job, files }
            }
            8 => Message::Control {
                job_id: job,
                control: if r.byte().is_multiple_of(2) {
                    Control::Pause
                } else {
                    Control::Cancel
                },
            },
            9 => Message::Finalize { job_id: job },
            10 => Message::FinalAck {
                job_id: job,
                verified: r.byte().is_multiple_of(2),
            },
            _ => {
                let code = ErrorCode::from_code(1 + r.below(14) as u16).unwrap();
                let message = random_string(r, 32);
                Message::Error { code, message }
            }
        }
    }

    #[test]
    fn random_messages_round_trip() {
        let mut r = Lcg(0x1234_5678_9abc_def0);
        let limits = Limits::default();
        for _ in 0..300 {
            let message = random_message(&mut r);
            let buf = encoded(&message);
            assert_eq!(Frame::decode(&buf, &limits).unwrap().message, message);
        }
    }

    #[test]
    fn fuzzed_and_mutated_frames_never_panic() {
        let mut r = Lcg(0x0ddb_a115_5eed_c0de);
        let limits = Limits::default();
        for i in 0..2000u32 {
            let bytes = if i % 3 == 0 {
                // Pure random garbage of random length (including 0 and 6).
                (0..r.below(HEADER_LEN as u64 + 40))
                    .map(|_| r.byte())
                    .collect::<Vec<_>>()
            } else {
                // Valid message, then 1-3 random corruptions.
                let message = random_message(&mut r);
                let mut buf = encoded(&message);
                let mutations = 1 + r.below(3);
                for _ in 0..mutations {
                    match r.below(4) {
                        0 if !buf.is_empty() => {
                            let at = r.below(buf.len() as u64) as usize;
                            buf[at] = r.byte();
                        }
                        1 if !buf.is_empty() => {
                            let at = r.below(buf.len() as u64) as usize;
                            buf.truncate(at);
                        }
                        2 => {
                            buf.push(r.byte());
                        }
                        _ => {
                            let at = r.below(buf.len() as u64 + 1) as usize;
                            buf.insert(at, r.byte());
                        }
                    }
                }
                buf
            };
            // Whatever the bytes say, decode must answer, never panic.
            let _ = Frame::decode(&bytes, &limits);
        }
    }
}
