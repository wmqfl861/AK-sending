//! End-to-end wire codec for the device-to-device file protocol (frame v1).
//!
//! Frame layout: `[version u8][type u16 BE][payload_len u32 BE][payload]`.
//! Every decode validates version, type, and length against [`Limits`] before
//! allocating; offsets and resource budgets are checked per message. u64
//! values travel as raw bytes, so no float conversion loses precision.
//!
//! Message set mirrors docs/PROTOCOL.zh-CN.md: Hello, Offer, Accept, Reject,
//! ManifestPage, Chunk, ChunkAck, ResumeRequest, Control, Finalize, FinalAck,
//! Error. There is deliberately no generic payload or blob message.
#![forbid(unsafe_code)]

use ak_core::error::ErrorCode;
use ak_core::manifest::{FileEntry, DIGEST_LEN};

/// Wire protocol version of frame v1.
pub const VERSION: u8 = 1;
/// Header size in bytes.
pub const HEADER_LEN: usize = 6;
/// Hard ceiling for any single frame regardless of limits.
pub const MAX_FRAME_BYTES: usize = (8 << 20) + 1024;
/// Capability bit: receiver supports resume from checkpoints.
pub const CAP_RESUME: u16 = 0x0001;

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
    pub fn encode(&self, out: &mut Vec<u8>) {
        todo!("owned by ak-wire agent")
    }

    /// Decode one frame from exactly-sized bytes under the given limits.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, DecodeError> {
        todo!("owned by ak-wire agent")
    }

    /// Largest frame the given limits can produce, for buffer preallocation.
    pub fn max_len(limits: &Limits) -> usize {
        todo!("owned by ak-wire agent")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-wire agent"]
    fn placeholder() {}
}
