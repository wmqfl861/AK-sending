//! Transfer engine: one sender, one receiver, one authenticated direct
//! channel, zero servers in the data path.
//!
//! Frozen protocol flow (see docs/PROTOCOL.zh-CN.md):
//! 1. Hello exchange (versions, capabilities, identities).
//! 2. Offer with a deterministic job id: `blake3(root_name || per-file
//!    rel_path/size/mtime || chunk_size)[..16]`. Same source, same id, so a
//!    resumed run continues the same job (no new content identity).
//! 3. Accept (receiver consent; consent is required, never assumed).
//! 4. Manifest pages, then ResumeRequest carrying per-file verified prefixes.
//! 5. Per file, chunked send with pipelined acks (bounded in-flight window);
//!    receiver verifies chunk digests on arrival and checkpoints the prefix.
//! 6. Per file: receiver finish (whole digest, fsync, atomic rename).
//! 7. Finalize -> FinalAck only after every file verified and committed.
//! 8. Cancel at any point discards partials cleanly; checkpoints survive.
//!
//! The engine consumes `DirectChannel` values only — there is no API that
//! accepts an unverified connection or a relay-ish transport.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use ak_core::handle::ConflictPolicy;
use ak_identity::Fingerprint;
use ak_wire::JobId;

/// Sender-side configuration.
#[derive(Debug, Clone)]
pub struct SendSpec {
    /// File or directory to send.
    pub source_root: PathBuf,
    pub peer: SocketAddr,
    /// Expected peer fingerprint; the connection aborts on mismatch.
    pub pinned: Fingerprint,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Local bind (0.0.0.0:0 for auto).
    pub bind: SocketAddr,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
    /// Maximum unacknowledged chunks in flight.
    pub in_flight_window: u32,
}

impl SendSpec {
    pub fn new(source_root: PathBuf, peer: SocketAddr, pinned: Fingerprint) -> Self {
        Self {
            source_root,
            peer,
            pinned,
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
            in_flight_window: 8,
        }
    }
}

/// Receiver-side configuration.
pub struct ReceiveSpec {
    pub out_dir: PathBuf,
    pub bind: SocketAddr,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Consent callback: channel-level peer authorization. Called once per
    /// connection with the verified peer fingerprint.
    pub authorize: Box<dyn Fn(&Fingerprint) -> bool + Send>,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
}

impl ReceiveSpec {
    pub fn new(out_dir: PathBuf) -> Self {
        Self {
            out_dir,
            bind: SocketAddr::from(([0, 0, 0, 0], 7100)),
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            authorize: Box::new(|_| true),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
        }
    }
}

/// Progress events, distinct per direction so the CLI can render honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Connecting,
    Connected { peer: String },
    Offered { files: u64, bytes: u64 },
    Accepted { files: u64, bytes: u64 },
    ManifestPageSent { page: u32 },
    ManifestReceived,
    Resuming { chunks: u64 },
    FileStarted { file_index: u32, path: String, bytes: u64 },
    ChunkSent { file_index: u32, chunk_index: u64 },
    ChunkReceived { file_index: u32, chunk_index: u64 },
    FileVerified { file_index: u32 },
    FileCommitted { file_index: u32, path: String },
    Verifying,
    FinalAck { verified: bool },
    Done,
}

/// Final report. Presence of this struct means the transfer completed and,
/// on the receive side, every file passed whole-digest verification and was
/// committed with an atomic rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub job_id: JobId,
    pub files: u64,
    pub bytes: u64,
    pub duration: Duration,
    /// Chunks not re-sent/re-received thanks to checkpoints.
    pub resumed_chunks: u64,
    pub peer_fingerprint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("connect failed: {0}")]
    Connect(#[from] ak_net::ConnectError),
    #[error("stream failed: {0}")]
    Stream(#[from] ak_net::StreamError),
    #[error("peer rejected the offer: {0}")]
    Rejected(ak_core::error::ErrorCode),
    #[error("peer sent error {code}: {message}")]
    PeerError { code: ak_core::error::ErrorCode, message: String },
    #[error("peer vanished mid-transfer (checkpoint kept)")]
    PeerClosed,
    #[error("source changed during transfer")]
    SourceChanged,
    #[error("storage error: {0}")]
    Storage(#[from] ak_storage::OpenError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checkpoint store error: {0}")]
    Checkpoint(#[source] std::io::Error),
    #[error("final verification failed")]
    VerificationFailed,
    #[error("cancelled")]
    Cancelled,
}

/// Deterministic job id from source identity (see module docs).
pub fn job_id_for(root_name: &str, manifest: &ak_core::manifest::Manifest) -> JobId {
    todo!("owned by integration phase")
}

/// Run one send to completion. Progress callbacks are synchronous and must
/// be cheap.
pub async fn send(
    spec: SendSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    todo!("owned by integration phase")
}

/// Run one receive to completion (one job, then returns).
pub async fn receive(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    todo!("owned by integration phase")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced during integration"]
    fn placeholder() {}
}
