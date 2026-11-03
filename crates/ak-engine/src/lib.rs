//! Transfer engine: one sender, one receiver, one authenticated direct
//! channel, zero servers in the data path.
//!
//! Frozen protocol flow (see docs/PROTOCOL.zh-CN.md):
//! 0. Policy gate (`authorize_path`): the freshly connected channel's path
//!    is checked against the configured `DirectPolicy`
//!    (`SendSpec::policy` / `ReceiveSpec::policy`). A path the policy does
//!    not allow closes the connection (code `POLICY_GATE_CLOSE_CODE`)
//!    before any protocol frame is sent or accepted — zero bytes of this
//!    protocol enter a refused route.
//! 1. Hello exchange (versions, capabilities, identities).
//! 2. Offer with a deterministic job id: `blake3(root_name || per-file
//!    rel_path/size/mtime/digests || chunk_size)[..16]`. Same source, same
//!    id, so a resumed run continues the same job (no new content identity).
//! 3. Accept (receiver consent; consent is required, never assumed).
//! 4. Manifest pages (byte-budgeted, bounded), then a sharded ResumeRequest
//!    exchange carrying per-file verified prefixes taken from the receiver's
//!    part files: consecutive frames of at most `max_resume_entries` entries
//!    each, `more = true` on every frame but the last (an empty job still
//!    sends one empty frame with `more = false`). This removes the old
//!    single-frame 65,535-entry cap on one job.
//! 5. Per file, chunked send with windowed acks; the receiver verifies every
//!    chunk digest on arrival and checkpoints the verified prefix.
//! 6. Per file: receiver finish (whole digest, fsync, atomic rename).
//! 7. Finalize -> FinalAck only after every file verified and committed; the
//!    checkpoint is marked completed strictly before FinalAck is sent.
//! 8. Cancel/Error at any point aborts; part files and checkpoints survive
//!    for resume. No relay fallback exists anywhere by construction.
//!
//! The engine consumes `DirectChannel` values only — there is no API that
//! accepts an unverified connection or a relay-ish transport.
//!
//! v2 additions over the frozen v1 flow (all capability-negotiated):
//! - idle timeouts: any receive wait longer than `idle_timeout` aborts with
//!   [`EngineError::IdleTimeout`]; checkpoints and part files survive;
//! - in-band Pause/Resume: the receiver may send `Control::Pause` while
//!   chunks flow; the sender stops issuing new chunks until `Control::Resume`
//!   (or the idle timeout fires);
//! - optional multistream: when both Hellos carry `CAP_MULTISTREAM` and the
//!   sender opts in, every file rides its own QUIC bidirectional stream (at
//!   most `min(in_flight_window, 4)` concurrent), with per-file windowed
//!   acks. Otherwise the single-stream v1 path runs unchanged.
//!
//! v3 additions (append-only, no wire-format change):
//! - storage-full precheck: before the receiver answers with a ResumeRequest
//!   it compares the destination's reported free space against the job's
//!   total bytes ([`check_headroom`]); a shortage aborts with
//!   [`EngineError::StorageFull`] after telling the peer over the wire;
//! - multi-root send jobs: [`SendSpec::extra_roots`] merges several
//!   files/directories into one manifest, each entry prefixed with its
//!   root's name; the receiver side is unchanged (the tree simply grows a
//!   top-level directory per root).
//!
//! Concurrent receive (round 6, no wire-format change):
//! [`serve_concurrently`] binds once and serves several senders — one
//! independent task per accepted connection, each running the exact
//! single-job body of [`receive_with_addr`] (authorize callback and policy
//! gate execute per connection). Checkpoints never overlap: every task
//! owns its own `<job>.akjob` file (one per job id, atomic writes). The
//! receive directory is shared read-only through `LocalSaveTarget`, whose
//! `prepare_file` keeps no interior mutable state, so the conflict policy
//! acts at file granularity — two concurrent jobs racing for the same
//! target path resolve it per file (under `ConflictPolicy::Fail` one of
//! them fails; `Rename` sidesteps). Per-task progress order is preserved;
//! events from different tasks interleave arbitrarily.
//!
//! Known v1 limitations still true (deliberate, documented): receiver accepts
//! one job per `receive()` call — and one job per connection under
//! [`serve_concurrently`] (one connection = one job).
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_core::error::ErrorCode;
use ak_core::handle::{ConflictPolicy, FileSink, SaveTarget, SourceHandle};
use ak_core::manifest::{FileEntry, Manifest};
use ak_core::policy::{DirectPolicy, PathKind};
use ak_identity::tls::TlsBundle;
use ak_identity::Fingerprint;
use ak_net::{DirectChannel, DirectEndpoint, DirectRecvStream, DirectSendStream, DirectStream};
use ak_storage::{LocalSaveTarget, LocalSource};
use ak_store::{CheckpointStore, Direction, FileCheckpoint, FileCheckpointStore, TaskRecord};
use ak_wire::{
    Control, Frame, JobId, Limits, Message, ResumeEntry, WireEntry, CAP_MULTISTREAM,
    CAP_PAUSE_RESUME, CAP_RESUME,
};

/// Manifest pages are assembled under this byte budget so every page fits a
/// frame comfortably below `ak_wire::MAX_FRAME_BYTES`.
const PAGE_BUDGET_BYTES: usize = 4 << 20;
/// Checkpoint write cadence, in acknowledged chunks.
const CHECKPOINT_EVERY_ACKS: u64 = 16;
/// Wire protocol version this engine speaks.
const PROTO_VERSION: u16 = 1;

/// The one decode-limits profile used by every engine receive site in both
/// roles. Encoding has no limits, so the decoder budget must cover whatever
/// the peer's encoder produces: the receiver shards its resume answer into
/// ResumeRequest frames of at most `max_resume_entries` entries each
/// ([`resume_request_frames`]), so matching
/// [`ak_wire::MAX_RESUME_ENTRIES`] keeps a full shard decodable while the
/// multi-frame exchange itself carries no per-job entry cap.
fn engine_limits() -> Limits {
    Limits {
        max_chunk_bytes: 8 << 20,
        max_entries_per_page: 256,
        max_string_bytes: 1024,
        max_error_message_bytes: 256,
        max_resume_entries: ak_wire::MAX_RESUME_ENTRIES,
        max_chunk_digests: 65536,
    }
}
/// Default idle timeout applied to every frame receive wait.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on concurrent per-file streams in multistream mode.
const MAX_PARALLEL_STREAMS: usize = 4;
/// QUIC application-error code for a policy-gate refusal: `0xA0` marks the
/// AK protocol family, `0x01` the policy gate. An application-space code
/// (no IANA transport meaning), carried in the CONNECTION_CLOSE frame when
/// [`authorize_path`] refuses a path so a peer's transport log can name
/// the reason. No protocol frame precedes it.
const POLICY_GATE_CLOSE_CODE: u32 = 0xA0_01;

/// Sender-side configuration.
#[derive(Debug, Clone)]
pub struct SendSpec {
    /// File or directory to send.
    pub source_root: PathBuf,
    /// Additional files/directories sent in the same job. Empty (the
    /// default) keeps the frozen single-root behavior; otherwise every
    /// root's manifest entries are prefixed with that root's name
    /// (`<root_name>/<rel_path>`) and merged into one manifest. Root names
    /// must be unique; duplicates fail the send with an `InvalidInput` io
    /// error before any connection is made.
    pub extra_roots: Vec<PathBuf>,
    pub peer: SocketAddr,
    /// Expected peer fingerprint; the connection aborts on mismatch.
    pub pinned: Fingerprint,
    /// Our TLS identity (self-signed certificate for this device key).
    pub identity: TlsBundle,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Local bind (0.0.0.0:0 for auto).
    pub bind: SocketAddr,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
    /// Maximum unacknowledged chunks in flight.
    pub in_flight_window: u32,
    /// Abort when no frame arrives within this budget on any receive wait
    /// (including while paused or waiting for acks).
    pub idle_timeout: Duration,
    /// Opt into parallel per-file streams. Takes effect only when the peer
    /// also advertises `CAP_MULTISTREAM`; otherwise the single-stream path
    /// runs. Also caps concurrent streams at `min(in_flight_window, 4)`.
    pub multistream: bool,
    /// Route policy gate, applied to the connected channel's path before
    /// any protocol frame is sent (see [`authorize_path`]). Default:
    /// `DirectPolicy::default()` (`allow_internet = true`,
    /// `allow_cellular = false`). A `LocalDirect` path always passes.
    pub policy: DirectPolicy,
}

impl SendSpec {
    pub fn new(
        source_root: PathBuf,
        peer: SocketAddr,
        pinned: Fingerprint,
        identity: TlsBundle,
    ) -> Self {
        Self {
            source_root,
            extra_roots: Vec::new(),
            peer,
            pinned,
            identity,
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
            in_flight_window: 8,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            multistream: false,
            policy: DirectPolicy::default(),
        }
    }

    /// Append another file or directory to this send job (multi-root send).
    /// Root names must be unique across the job; see [`SendSpec::extra_roots`].
    pub fn add_root(&mut self, path: PathBuf) {
        self.extra_roots.push(path);
    }
}

/// Receiver-side configuration.
pub struct ReceiveSpec {
    pub out_dir: PathBuf,
    pub bind: SocketAddr,
    /// Our TLS identity (self-signed certificate for this device key).
    pub identity: TlsBundle,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Consent callback: channel-level peer authorization. Called once per
    /// connection with the verified peer fingerprint.
    pub authorize: Box<dyn Fn(&Fingerprint) -> bool + Send + Sync>,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
    /// Abort when no frame arrives within this budget on any receive wait.
    pub idle_timeout: Duration,
    /// Route policy gate, applied to the accepted channel's path before
    /// any protocol frame is received or sent (see [`authorize_path`]).
    /// Default: `DirectPolicy::default()` (`allow_internet = true`,
    /// `allow_cellular = false`). A `LocalDirect` path always passes.
    pub policy: DirectPolicy,
}

impl ReceiveSpec {
    pub fn new(out_dir: PathBuf, identity: TlsBundle) -> Self {
        Self {
            out_dir,
            bind: SocketAddr::from(([0, 0, 0, 0], 7100)),
            identity,
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            authorize: Box::new(|_| true),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            policy: DirectPolicy::default(),
        }
    }
}

/// Progress events, distinct per direction so the CLI can render honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Connecting,
    Connected {
        peer: String,
    },
    Offered {
        files: u64,
        bytes: u64,
    },
    Accepted {
        files: u64,
        bytes: u64,
    },
    ManifestPageSent {
        page: u32,
    },
    ManifestReceived,
    Resuming {
        chunks: u64,
    },
    FileStarted {
        file_index: u32,
        path: String,
        bytes: u64,
    },
    ChunkSent {
        file_index: u32,
        chunk_index: u64,
    },
    ChunkReceived {
        file_index: u32,
        chunk_index: u64,
    },
    /// Cumulative verified bytes of one file, piggybacked on the checkpoint
    /// cadence (every [`CHECKPOINT_EVERY_ACKS`] acknowledged chunks).
    BytesAcked {
        file_index: u32,
        cumulative_bytes: u64,
    },
    /// The peer asked us to stop sending; we hold until Resume or timeout.
    Paused,
    /// The peer let us continue after a Pause.
    Resumed,
    FileVerified {
        file_index: u32,
    },
    FileCommitted {
        file_index: u32,
        path: String,
    },
    Verifying,
    FinalAck {
        verified: bool,
    },
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
    /// Chunks not re-sent/re-received thanks to verified part files.
    pub resumed_chunks: u64,
    pub peer_fingerprint: Option<String>,
    /// Root names of the job on the sender (primary first, then extras in
    /// the order they were added). Empty on the receiver side.
    pub roots: Vec<String>,
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
    PeerError { code: ErrorCode, message: String },
    #[error("peer vanished mid-transfer (checkpoint kept)")]
    PeerClosed,
    #[error("idle timeout: no frame within {0:?} (checkpoint kept)")]
    IdleTimeout(Duration),
    #[error("source changed during transfer")]
    SourceChanged,
    #[error("storage error: {0}")]
    Storage(#[from] ak_storage::OpenError),
    #[error("destination storage full: {available} bytes available, {needed} needed")]
    StorageFull {
        /// Free bytes the destination reported at precheck time.
        available: u64,
        /// Total payload bytes the job needs.
        needed: u64,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checkpoint store error: {0}")]
    Checkpoint(#[source] std::io::Error),
    #[error("final verification failed")]
    VerificationFailed,
    #[error("cancelled")]
    Cancelled,
    /// The connected route's path kind is not allowed by the configured
    /// [`DirectPolicy`] (internet refused by policy, or a Relay/Unknown
    /// path that must never carry file data — see [`authorize_path`]).
    /// The channel is closed with [`POLICY_GATE_CLOSE_CODE`] before any
    /// protocol byte moves.
    #[error("route refused by policy: {0:?}")]
    RouteForbidden(PathKind),
}

/// Deterministic job id from source identity (see module docs).
pub fn job_id_for(root_name: &str, manifest: &Manifest) -> JobId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"AKJOB1");
    hasher.update(root_name.as_bytes());
    hasher.update(&manifest.chunk_size.get().to_be_bytes());
    for entry in &manifest.entries {
        hasher.update(&[u8::from(entry.is_dir)]);
        hasher.update(
            entry
                .rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .as_bytes(),
        );
        hasher.update(&entry.size.to_be_bytes());
        hasher.update(&entry.mtime_unix_ms.to_be_bytes());
        hasher.update(&entry.whole_digest);
    }
    let digest = hasher.finalize();
    let mut id = JobId::default();
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Pure storage-headroom decision for the receive side. Unknown capacity
/// (`None`: the platform cannot report free space) passes — exhaustion is
/// still caught later by real write errors, never faked here. An exact fit
/// passes; any shortage fails with [`EngineError::StorageFull`] carrying the
/// real reported numbers.
pub fn check_headroom(available: Option<u64>, total_needed: u64) -> Result<(), EngineError> {
    match available {
        Some(available) if available < total_needed => Err(EngineError::StorageFull {
            available,
            needed: total_needed,
        }),
        _ => Ok(()),
    }
}

/// Pure pre-transport policy gate (ADR-001: a path that is unknown or not
/// allowed by policy never enters the file-data channel):
/// - `LocalDirect` always passes — local-first, no policy can lock out the
///   loopback/LAN path;
/// - `InternetDirect` passes only when `policy.allow_internet`;
/// - `Relay` and `Unknown` are always refused: neither can be produced by
///   the direct-only transport (`ak-net` classifies only LocalDirect /
///   InternetDirect), so meeting one is a defect — refuse defensively.
///
/// Relation to [`DirectPolicy::choose`]'s `Observation` model: the facts
/// `choose` re-checks per candidate (`peer_id` match, `authenticated`,
/// `encrypted`, `receive_authorized`) are deliberately NOT re-filled as
/// booleans here, because the transport already guarantees them **by
/// type** — the ADR-002 D6 constraint landing point. A
/// [`DirectChannel`] exists only after a TLS handshake that verified the
/// peer's self-signed certificate against the pinned fingerprint (that is
/// the identity/authentication/encryption triple), and the receiver's
/// `authorize` callback supplied channel-level consent at accept time.
/// Likewise `channel.path()` is the trusted `classify` product of
/// `ak-net`, not a caller-asserted flag. This gate is therefore a purely
/// permissive filter over the path kind — not a re-declaration of
/// Observation booleans, which would only create a place for untrusted
/// input to sneak back in. `Observation::cellular` has no
/// transport-level equivalent (a QUIC connection does not expose the
/// underlying network interface), so `allow_cellular` stays advisory in
/// the engine until an interface-aware adapter exists; and `choose`'s
/// local-probe sequencing (`WaitReason::LocalProbePending`) belongs to
/// candidate selection *before* connecting, not to this per-connection
/// gate.
pub fn authorize_path(policy: &DirectPolicy, path: PathKind) -> Result<(), EngineError> {
    match path {
        PathKind::LocalDirect => Ok(()),
        PathKind::InternetDirect if policy.allow_internet => Ok(()),
        PathKind::InternetDirect | PathKind::Relay | PathKind::Unknown => {
            Err(EngineError::RouteForbidden(path))
        }
    }
}

/// Gate a freshly obtained channel: refuse (and close with
/// [`POLICY_GATE_CLOSE_CODE`]) before a single protocol byte is sent or
/// accepted. Called at exactly two sites: `send` right after
/// `connect_bounded`, and `receive_with_addr` right after `accept` — in
/// both cases before the first stream is opened or the first frame moves.
fn gate_channel(policy: &DirectPolicy, channel: &DirectChannel) -> Result<(), EngineError> {
    if let Err(error) = authorize_path(policy, channel.path()) {
        channel.close(POLICY_GATE_CLOSE_CODE, b"route refused by policy");
        Err(error)
    } else {
        Ok(())
    }
}

fn now_ms() -> u64 {
    ak_store::now_unix_ms()
}

fn hello_frame(name: &str, identity: &TlsBundle, capabilities: u16) -> Frame {
    let mut public = [0u8; 32];
    public.copy_from_slice(identity.fingerprint.as_bytes());
    // The peer verifies the certificate itself; the key field is a hint for
    // display and is pinned via the TLS handshake, not trusted from here.
    Frame {
        version: ak_wire::VERSION,
        message: Message::Hello {
            proto_version: PROTO_VERSION,
            device_name: name.chars().take(64).collect(),
            public_key: public,
            capabilities,
        },
    }
}

/// Capabilities this receiver advertises: it can always resume, honor
/// Pause/Resume, and accept per-file parallel streams.
const RECV_CAPABILITIES: u16 = CAP_RESUME | CAP_PAUSE_RESUME | CAP_MULTISTREAM;

async fn expect_hello(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<u16, EngineError> {
    match recv_within(rx, limits, idle).await? {
        Frame {
            message:
                Message::Hello {
                    proto_version,
                    capabilities,
                    ..
                },
            ..
        } => {
            if proto_version != PROTO_VERSION {
                return Err(EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    message: format!("peer speaks protocol version {proto_version}"),
                });
            }
            Ok(capabilities)
        }
        Frame {
            message: Message::Error { code, message },
            ..
        } => Err(EngineError::PeerError { code, message }),
        other => Err(unexpected(other.message)),
    }
}

/// One frame receive under an idle deadline; silence past `idle` is fatal
/// with [`EngineError::IdleTimeout`].
async fn recv_within(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<Frame, EngineError> {
    match tokio::time::timeout(idle, rx.recv_frame(limits)).await {
        Ok(result) => result.map_err(EngineError::Stream),
        Err(_) => Err(EngineError::IdleTimeout(idle)),
    }
}

fn unexpected(message: Message) -> EngineError {
    EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: format!("unexpected frame: {message:?}"),
    }
}

/// Connect with a fast family check and a hard deadline. A v4 socket dialing
/// a v6 peer (or vice versa) would otherwise hang in QUIC's retry loop with
/// no error (found by the IPv6 scenario tests); the idle budget bounds every
/// other unreachable-peer case too.
async fn connect_bounded(
    endpoint: &DirectEndpoint,
    peer: SocketAddr,
    pinned: &Fingerprint,
    idle: Duration,
) -> Result<DirectChannel, EngineError> {
    let local = endpoint.local_addr()?;
    if local.is_ipv4() != peer.is_ipv4() {
        return Err(EngineError::Connect(ak_net::ConnectError::Io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "address family mismatch: endpoint bound on {local} cannot reach {peer}; \
                     use a matching-family --to/--port or an [::] bind"
                ),
            ),
        )));
    }
    match tokio::time::timeout(idle, endpoint.connect(peer, pinned)).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(EngineError::IdleTimeout(idle)),
    }
}

/// Assemble manifest pages under the byte budget.
fn assemble_pages(manifest: &Manifest, limits: &Limits) -> Vec<Vec<WireEntry>> {
    let mut pages: Vec<Vec<WireEntry>> = Vec::new();
    let mut current: Vec<WireEntry> = Vec::new();
    let mut current_bytes = 0usize;
    for entry in &manifest.entries {
        let wire = WireEntry::from(entry);
        let size = 128 + wire.rel_path.len() + 32 * wire.chunk_digests.len();
        let cap = usize::from(limits.max_entries_per_page);
        if !current.is_empty() && (current_bytes + size > PAGE_BUDGET_BYTES || current.len() >= cap)
        {
            pages.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += size;
        current.push(wire);
    }
    if !current.is_empty() || pages.is_empty() {
        pages.push(current);
    }
    pages
}

/// Shard the receiver's resume answer into ResumeRequest frames of at most
/// `max_per_frame` entries. Every frame but the last carries `more = true`;
/// an empty answer still produces exactly one empty frame with
/// `more = false`, so the exchange always terminates with a final frame the
/// sender can wait for. A `max_per_frame` of 0 is clamped to 1 (a shard
/// size, not a stall).
fn resume_request_frames(
    job_id: JobId,
    files: &[ResumeEntry],
    max_per_frame: usize,
) -> Vec<Message> {
    let max = max_per_frame.max(1);
    let mut frames: Vec<Message> = files
        .chunks(max)
        .map(|shard| Message::ResumeRequest {
            job_id,
            files: shard.to_vec(),
            more: true,
        })
        .collect();
    if let Some(Message::ResumeRequest { more, .. }) = frames.last_mut() {
        *more = false;
    } else {
        frames.push(Message::ResumeRequest {
            job_id,
            files: Vec::new(),
            more: false,
        });
    }
    frames
}

/// Sender-side accumulator for the sharded ResumeRequest exchange: folds in
/// frames until one arrives with `more = false`. Every frame must name the
/// expected job; a mismatch aborts with a protocol error.
struct ResumeCollector {
    job_id: JobId,
    entries: Vec<ResumeEntry>,
}

impl ResumeCollector {
    fn new(job_id: JobId) -> Self {
        ResumeCollector {
            job_id,
            entries: Vec::new(),
        }
    }

    /// Fold one frame's entries in; returns `true` when the exchange is
    /// complete (the frame carried `more = false`).
    fn push(
        &mut self,
        frame_job_id: JobId,
        files: Vec<ResumeEntry>,
        more: bool,
    ) -> Result<bool, EngineError> {
        if frame_job_id != self.job_id {
            return Err(EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                message: "resume frame names a different job".into(),
            });
        }
        self.entries.extend(files);
        Ok(!more)
    }
}

fn sender_record(spec: &SendSpec, job_id: JobId, manifest: &Manifest) -> TaskRecord {
    TaskRecord {
        job_id,
        direction: Direction::Send,
        root: spec.source_root.clone(),
        peer_fingerprint: Some(spec.pinned),
        chunk_size: spec.chunk_size.get(),
        total_files: manifest.file_count(),
        total_bytes: manifest.total_bytes(),
        state: "connecting".into(),
        created_unix_ms: now_ms(),
        updated_unix_ms: now_ms(),
        files: manifest
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| FileCheckpoint {
                file_index: index as u32,
                size: e.size,
                verified_prefix_chunks: 0,
            })
            .collect(),
    }
}

/// Sender-side source set: the primary root plus any extra roots, exposed to
/// the transfer loops as one manifest. With no extra roots this is an
/// identity wrapper, so the single-root wire behavior (manifest, job id,
/// Offer root name) is byte-for-byte the frozen one. With extras, every
/// entry of root `r` is re-pathed as `<root_name>/<rel_path>`; root names
/// must be unique or the job is refused up front.
struct SourceSet {
    sources: Vec<Arc<LocalSource>>,
    /// Display name of each root, in the same order as `sources`.
    names: Vec<String>,
    /// Merged manifest the wire protocol speaks.
    manifest: Manifest,
    /// Merged entry index -> (root ordinal, per-root entry index). Synthesized
    /// entries for entirely empty roots carry `u32::MAX` as their local index;
    /// they are directories and are never read.
    index_map: Vec<(u32, u32)>,
    /// True when more than one root was merged.
    multi: bool,
}

/// Map a manifest-construction failure (root name unusable as a transfer
/// path component, e.g. a Windows-reserved stem) to a clear invalid-input
/// error, before any connection is made.
fn invalid_root_name(name: &str) -> EngineError {
    EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("root name not usable as a transfer directory name: {name:?}"),
    ))
}

impl SourceSet {
    fn open(spec: &SendSpec) -> Result<Self, EngineError> {
        let mut sources = vec![Arc::new(LocalSource::open(
            &spec.source_root,
            spec.chunk_size,
        )?)];
        for extra in &spec.extra_roots {
            sources.push(Arc::new(LocalSource::open(extra, spec.chunk_size)?));
        }
        let names: Vec<String> = sources.iter().map(|s| s.root_name()).collect();
        let multi = !spec.extra_roots.is_empty();
        if multi {
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            if sorted.len() != names.len() {
                return Err(EngineError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("duplicate transfer root names: [{}]", names.join(", ")),
                )));
            }
        }
        let (manifest, index_map) = if multi {
            let mut entries = Vec::new();
            let mut index_map = Vec::new();
            for (ordinal, source) in sources.iter().enumerate() {
                let local = source.manifest();
                if local.entries.is_empty() {
                    // An entirely empty directory root still appears on the
                    // receiver as its own top-level directory.
                    entries.push(
                        FileEntry::directory(&names[ordinal])
                            .map_err(|_| invalid_root_name(&names[ordinal]))?,
                    );
                    index_map.push((ordinal as u32, u32::MAX));
                    continue;
                }
                for (local_index, entry) in local.entries.iter().enumerate() {
                    let rel = format!(
                        "{}/{}",
                        names[ordinal],
                        entry.rel_path.to_string_lossy().replace('\\', "/")
                    );
                    let merged = if entry.is_dir {
                        FileEntry::directory(&rel)
                    } else {
                        FileEntry::new(
                            &rel,
                            entry.size,
                            entry.mtime_unix_ms,
                            entry.whole_digest,
                            entry.chunk_digests.clone(),
                        )
                    }
                    .map_err(|_| invalid_root_name(&names[ordinal]))?;
                    entries.push(merged);
                    index_map.push((ordinal as u32, local_index as u32));
                }
            }
            (
                Manifest {
                    chunk_size: spec.chunk_size,
                    entries,
                },
                index_map,
            )
        } else {
            let manifest = sources[0].manifest().clone();
            let index_map = (0..manifest.entries.len() as u32).map(|i| (0, i)).collect();
            (manifest, index_map)
        };
        Ok(Self {
            sources,
            names,
            manifest,
            index_map,
            multi,
        })
    }

    /// Read a merged-index chunk through the owning root's source.
    fn read_chunk(
        &self,
        file_index: u32,
        chunk_index: u64,
        buf: &mut [u8],
    ) -> std::io::Result<usize> {
        let (ordinal, local_index) = self
            .index_map
            .get(file_index as usize)
            .copied()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "file index out of range")
            })?;
        let source = self.sources.get(ordinal as usize).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "root index out of range")
        })?;
        source.read_chunk(local_index, chunk_index, buf)
    }

    /// Re-check every root against its manifest.
    fn verify_stable(&self) -> Result<(), EngineError> {
        for source in &self.sources {
            source
                .verify_stable()
                .map_err(|_| EngineError::SourceChanged)?;
        }
        Ok(())
    }

    /// Name used for the Offer and the job id: the single root's name, or
    /// `"multi"` when several roots were merged.
    fn display_name(&self) -> &str {
        if self.multi {
            "multi"
        } else {
            &self.names[0]
        }
    }

    /// Root names for the report (primary first, extras in add order).
    fn roots(&self) -> Vec<String> {
        self.names.clone()
    }
}

/// Read one chunk through the blocking pool, returning exactly the chunk bytes.
async fn read_chunk_via(
    source: Arc<SourceSet>,
    file_index: u32,
    chunk_index: u64,
    len: usize,
) -> std::io::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; len];
        let read = source.read_chunk(file_index, chunk_index, &mut buf)?;
        buf.truncate(read);
        Ok(buf)
    })
    .await
    .unwrap_or_else(|join| {
        Err(std::io::Error::other(format!(
            "chunk read task failed: {join}"
        )))
    })
}

/// Sender-side ack window state, shared across drain calls of one transfer.
struct AckWindow {
    outstanding: u32,
    paused: bool,
    acks_since_progress: u64,
}

impl AckWindow {
    fn new() -> Self {
        AckWindow {
            outstanding: 0,
            paused: false,
            acks_since_progress: 0,
        }
    }
}

/// Wait until the window has room again. While paused, no ack expectation is
/// enforced: the loop only returns after a Resume (or fails on Cancel/Error/
/// idle). Every ack also feeds byte-level progress at checkpoint cadence.
async fn drain_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    wanted: u32,
    idle: Duration,
    window: &mut AckWindow,
    manifest: &Manifest,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), EngineError> {
    while window.outstanding > wanted || window.paused {
        match recv_within(rx, limits, idle).await? {
            Frame {
                message:
                    Message::ChunkAck {
                        file_index,
                        chunk_index,
                        ..
                    },
                ..
            } => {
                window.outstanding = window.outstanding.saturating_sub(1);
                window.acks_since_progress += 1;
                if window.acks_since_progress >= CHECKPOINT_EVERY_ACKS {
                    window.acks_since_progress = 0;
                    if let Some(entry) = manifest.entries.get(file_index as usize) {
                        let cumulative = (chunk_index + 1)
                            .saturating_mul(u64::from(manifest.chunk_size.get()))
                            .min(entry.size);
                        progress(Progress::BytesAcked {
                            file_index,
                            cumulative_bytes: cumulative,
                        });
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Pause,
                        ..
                    },
                ..
            } => {
                if !window.paused {
                    window.paused = true;
                    progress(Progress::Paused);
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Resume,
                        ..
                    },
                ..
            } => {
                if window.paused {
                    window.paused = false;
                    progress(Progress::Resumed);
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Cancel,
                        ..
                    },
                ..
            } => return Err(EngineError::Cancelled),
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    Ok(())
}

/// Sender-side progress relay from per-file stream tasks to the driver.
enum SendEvent {
    ChunkSent {
        file_index: u32,
        chunk_index: u64,
    },
    BytesAcked {
        file_index: u32,
        cumulative_bytes: u64,
    },
    Paused,
    Resumed,
}

/// Send every file on its own QUIC bidirectional stream, at most
/// `min(in_flight_window, 4)` at a time. Files fully covered by resume are
/// skipped: no stream is opened for them.
#[allow(clippy::too_many_arguments)]
async fn multistream_send_files(
    channel: &Arc<DirectChannel>,
    source: &Arc<SourceSet>,
    manifest: &Manifest,
    skip: &[u64],
    in_flight_window: u32,
    idle: Duration,
    job_id: JobId,
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), EngineError> {
    let limits = engine_limits();
    let window = in_flight_window.max(1);
    let concurrency = (window as usize).min(MAX_PARALLEL_STREAMS);
    let mut work: VecDeque<u32> = manifest
        .entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            !entry.is_dir
                && skip
                    .get(*index)
                    .is_some_and(|skip| *skip < entry.chunk_digests.len() as u64)
        })
        .map(|(index, _)| index as u32)
        .collect();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<SendEvent>(64);
    let mut set: tokio::task::JoinSet<Result<u32, EngineError>> = tokio::task::JoinSet::new();
    let mut events_alive = true;
    loop {
        while set.len() < concurrency {
            let Some(file_index) = work.pop_front() else {
                break;
            };
            let entry = &manifest.entries[file_index as usize];
            progress(Progress::FileStarted {
                file_index,
                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                bytes: entry.size,
            });
            set.spawn(multistream_file_send(
                Arc::clone(channel),
                Arc::clone(source),
                job_id,
                file_index,
                entry.size,
                skip[file_index as usize],
                manifest.chunk_size,
                window,
                idle,
                limits,
                events_tx.clone(),
            ));
        }
        if set.is_empty() {
            return Ok(());
        }
        tokio::select! {
            event = events_rx.recv(), if events_alive => {
                match event {
                    Some(SendEvent::ChunkSent { file_index, chunk_index }) => {
                        progress(Progress::ChunkSent { file_index, chunk_index })
                    }
                    Some(SendEvent::BytesAcked {
                        file_index,
                        cumulative_bytes,
                    }) => progress(Progress::BytesAcked {
                        file_index,
                        cumulative_bytes,
                    }),
                    Some(SendEvent::Paused) => progress(Progress::Paused),
                    Some(SendEvent::Resumed) => progress(Progress::Resumed),
                    None => events_alive = false,
                }
            }
            joined = set.join_next() => {
                match joined {
                    Some(Ok(Ok(file_index))) => {
                        if let (Some(entry), Some(checkpoint)) = (
                            manifest.entries.get(file_index as usize),
                            record.files.get_mut(file_index as usize),
                        ) {
                            checkpoint.verified_prefix_chunks = entry.chunk_digests.len() as u64;
                        }
                        record.updated_unix_ms = now_ms();
                        store.save(record).map_err(EngineError::Checkpoint)?;
                    }
                    Some(Ok(Err(error))) => return Err(error),
                    Some(Err(join)) => {
                        return Err(EngineError::Io(std::io::Error::other(format!(
                            "file stream task failed: {join}"
                        ))))
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

/// One file over one dedicated stream: sequential chunks with a per-stream
/// windowed acks, Pause/Resume honored mid-stream, then a half-close after
/// the last ack. Returns the file index for checkpoint bookkeeping.
#[allow(clippy::too_many_arguments)]
async fn multistream_file_send(
    channel: Arc<DirectChannel>,
    source: Arc<SourceSet>,
    job_id: JobId,
    file_index: u32,
    file_size: u64,
    skip: u64,
    chunk_size: NonZeroU32,
    window: u32,
    idle: Duration,
    limits: Limits,
    events: tokio::sync::mpsc::Sender<SendEvent>,
) -> Result<u32, EngineError> {
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();
    let plan = ak_core::chunk::ChunkPlan::new(file_size, chunk_size);
    let mut outstanding: u32 = 0;
    let mut paused = false;
    let mut acks_since_progress: u64 = 0;
    for chunk_index in skip..plan.chunk_count() {
        if paused {
            drain_file_acks(
                &mut rx,
                &limits,
                0,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &events,
            )
            .await?;
        }
        let range = plan.range(chunk_index).ok_or(EngineError::SourceChanged)?;
        let len = (range.end - range.start) as usize;
        let data = read_chunk_via(Arc::clone(&source), file_index, chunk_index, len).await?;
        if data.len() != len {
            return Err(EngineError::SourceChanged);
        }
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::Chunk {
                job_id,
                file_index,
                chunk_index,
                data,
            },
        })
        .await?;
        outstanding += 1;
        if events
            .send(SendEvent::ChunkSent {
                file_index,
                chunk_index,
            })
            .await
            .is_err()
        {
            return Err(EngineError::Cancelled);
        }
        if outstanding >= window {
            drain_file_acks(
                &mut rx,
                &limits,
                window - 1,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &events,
            )
            .await?;
        }
    }
    drain_file_acks(
        &mut rx,
        &limits,
        0,
        idle,
        &mut outstanding,
        &mut paused,
        file_index,
        file_size,
        chunk_size,
        &mut acks_since_progress,
        &events,
    )
    .await?;
    tx.finish()?;
    Ok(file_index)
}

/// Per-file variant of [`drain_acks`] for multistream tasks: progress goes
/// through an event channel because the driver owns the progress callback.
#[allow(clippy::too_many_arguments)]
async fn drain_file_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    wanted: u32,
    idle: Duration,
    outstanding: &mut u32,
    paused: &mut bool,
    file_index: u32,
    file_size: u64,
    chunk_size: NonZeroU32,
    acks_since_progress: &mut u64,
    events: &tokio::sync::mpsc::Sender<SendEvent>,
) -> Result<(), EngineError> {
    while *outstanding > wanted || *paused {
        match recv_within(rx, limits, idle).await? {
            Frame {
                message: Message::ChunkAck { chunk_index, .. },
                ..
            } => {
                *outstanding = outstanding.saturating_sub(1);
                *acks_since_progress += 1;
                if *acks_since_progress >= CHECKPOINT_EVERY_ACKS {
                    *acks_since_progress = 0;
                    let cumulative = (chunk_index + 1)
                        .saturating_mul(u64::from(chunk_size.get()))
                        .min(file_size);
                    if events
                        .send(SendEvent::BytesAcked {
                            file_index,
                            cumulative_bytes: cumulative,
                        })
                        .await
                        .is_err()
                    {
                        return Err(EngineError::Cancelled);
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Pause,
                        ..
                    },
                ..
            } => {
                if !*paused {
                    *paused = true;
                    if events.send(SendEvent::Paused).await.is_err() {
                        return Err(EngineError::Cancelled);
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Resume,
                        ..
                    },
                ..
            } => {
                if *paused {
                    *paused = false;
                    if events.send(SendEvent::Resumed).await.is_err() {
                        return Err(EngineError::Cancelled);
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        control: Control::Cancel,
                        ..
                    },
                ..
            } => return Err(EngineError::Cancelled),
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    Ok(())
}

/// Run one send to completion. Progress callbacks are synchronous and must
/// be cheap.
pub async fn send(
    spec: SendSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let limits = engine_limits();
    let sources = Arc::new(SourceSet::open(&spec)?);
    let manifest = sources.manifest.clone();
    let root_name = sources.display_name().to_owned();
    let job_id = job_id_for(&root_name, &manifest);

    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let mut record = sender_record(&spec, job_id, &manifest);
    store.save(&record).map_err(EngineError::Checkpoint)?;

    progress(Progress::Connecting);
    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let channel = connect_bounded(&endpoint, spec.peer, &spec.pinned, spec.idle_timeout).await?;
    // Policy gate (protocol step 0): a refused path closes the connection
    // before any stream is opened or frame sent.
    gate_channel(&spec.policy, &channel)?;
    let channel = Arc::new(channel);
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();

    let mut capabilities = CAP_RESUME | CAP_PAUSE_RESUME;
    if spec.multistream {
        capabilities |= CAP_MULTISTREAM;
    }
    tx.send_frame(&hello_frame(
        &spec.device_name,
        &spec.identity,
        capabilities,
    ))
    .await?;
    let peer_caps = expect_hello(&mut rx, &limits, spec.idle_timeout).await?;
    let multistream = spec.multistream && (peer_caps & CAP_MULTISTREAM) != 0;

    // Offer -> Accept/Reject (receiver consent is mandatory).
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Offer {
            job_id,
            root_name: root_name.chars().take(1024).collect(),
            file_count: manifest.file_count(),
            total_bytes: manifest.total_bytes(),
            chunk_size: spec.chunk_size.get(),
        },
    })
    .await?;
    progress(Progress::Offered {
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
    });
    match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
        Frame {
            message: Message::Accept { .. },
            ..
        } => {}
        Frame {
            message: Message::Reject { reason, .. },
            ..
        } => return Err(EngineError::Rejected(reason)),
        Frame {
            message: Message::Error { code, message },
            ..
        } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    }
    progress(Progress::Accepted {
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
    });

    // Manifest pages.
    let pages = assemble_pages(&manifest, &limits);
    for (index, page) in pages.iter().enumerate() {
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::ManifestPage {
                job_id,
                page_index: index as u32,
                last_page: index + 1 == pages.len(),
                entries: page.clone(),
            },
        })
        .await?;
        progress(Progress::ManifestPageSent { page: index as u32 });
    }

    // ResumeRequest: skip the receiver's verified prefixes. The answer is
    // sharded — frames accumulate (each wait under the idle budget) until
    // one carries more = false, and every frame must name this job.
    let mut resume = ResumeCollector::new(job_id);
    loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::ResumeRequest {
                        job_id: frame_job_id,
                        files,
                        more,
                    },
                ..
            } => {
                if resume.push(frame_job_id, files, more)? {
                    break;
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    let resume = resume.entries;
    let mut skip = vec![0u64; manifest.entries.len()];
    let mut resumed_chunks = 0u64;
    for entry in &resume {
        if let Some(slot) = skip.get_mut(entry.file_index as usize) {
            *slot = entry.verified_prefix_chunks;
            resumed_chunks += entry.verified_prefix_chunks;
        }
    }
    if resumed_chunks > 0 {
        progress(Progress::Resuming {
            chunks: resumed_chunks,
        });
    }

    // Chunk transfer. Multistream mode rides one QUIC stream per file; the
    // single-stream path keeps the frozen v1 wire behavior.
    if multistream {
        multistream_send_files(
            &channel,
            &sources,
            &manifest,
            &skip,
            spec.in_flight_window,
            spec.idle_timeout,
            job_id,
            &mut record,
            &store,
            progress,
        )
        .await?;
    } else {
        let window_size = spec.in_flight_window.max(1);
        let mut acks = AckWindow::new();
        for (file_index, entry) in manifest.entries.iter().enumerate() {
            if entry.is_dir {
                continue;
            }
            progress(Progress::FileStarted {
                file_index: file_index as u32,
                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                bytes: entry.size,
            });
            let plan = ak_core::chunk::ChunkPlan::new(entry.size, spec.chunk_size);
            for chunk_index in skip[file_index]..plan.chunk_count() {
                if acks.paused {
                    drain_acks(
                        &mut rx,
                        &limits,
                        window_size - 1,
                        spec.idle_timeout,
                        &mut acks,
                        &manifest,
                        progress,
                    )
                    .await?;
                }
                let range = plan.range(chunk_index).ok_or(EngineError::SourceChanged)?;
                let len = (range.end - range.start) as usize;
                let data =
                    read_chunk_via(Arc::clone(&sources), file_index as u32, chunk_index, len)
                        .await?;
                if data.len() != len {
                    return Err(EngineError::SourceChanged);
                }
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Chunk {
                        job_id,
                        file_index: file_index as u32,
                        chunk_index,
                        data,
                    },
                })
                .await?;
                acks.outstanding += 1;
                progress(Progress::ChunkSent {
                    file_index: file_index as u32,
                    chunk_index,
                });
                if acks.outstanding >= window_size {
                    drain_acks(
                        &mut rx,
                        &limits,
                        window_size - 1,
                        spec.idle_timeout,
                        &mut acks,
                        &manifest,
                        progress,
                    )
                    .await?;
                }
            }
            // Keep checkpointed progress roughly current per file.
            record.files[file_index].verified_prefix_chunks =
                manifest.entries[file_index].chunk_digests.len() as u64;
            record.updated_unix_ms = now_ms();
            store.save(&record).map_err(EngineError::Checkpoint)?;
        }
        drain_acks(
            &mut rx,
            &limits,
            0,
            spec.idle_timeout,
            &mut acks,
            &manifest,
            progress,
        )
        .await?;
    }

    // Final integrity gate on the sources, then Finalize.
    sources.verify_stable()?;
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Finalize { job_id },
    })
    .await?;
    // Pause/Resume is tolerated at the gate; only the verdict matters here.
    let verified = loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message: Message::FinalAck { verified, .. },
                ..
            } => break verified,
            Frame {
                message: Message::Control { .. },
                ..
            } => continue,
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    };
    progress(Progress::FinalAck { verified });
    if !verified {
        record.state = "failed".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        return Err(EngineError::VerificationFailed);
    }
    tx.finish()?;
    record.state = "completed".into();
    record.updated_unix_ms = now_ms();
    store.save(&record).map_err(EngineError::Checkpoint)?;
    progress(Progress::Done);

    Ok(Report {
        job_id,
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
        duration: started.elapsed(),
        resumed_chunks,
        peer_fingerprint: Some(spec.pinned.to_hex()),
        roots: sources.roots(),
    })
}

struct ReceiverJob {
    manifest: Manifest,
}

/// Run one receive to completion (one job, then returns). `addr_tx` receives
/// the bound address before the first accept, so callers can display it.
pub async fn receive_with_addr(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
    addr_tx: tokio::sync::oneshot::Sender<SocketAddr>,
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;

    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let local = endpoint.local_addr()?;
    let _ = addr_tx.send(local);
    let channel = endpoint.accept(&spec.authorize).await?;
    // One connection through the shared single-job protocol body — the very
    // same body [`serve_concurrently`] runs per connection.
    receive_job(channel, &target, &JobSpec::of(&spec), started, progress).await
}

/// The per-connection subset of [`ReceiveSpec`] the single-job protocol body
/// ([`receive_job`]) needs. [`ReceiveSpec`] is not `Clone` (its `authorize`
/// callback belongs to the accept side only), so the concurrent receiver
/// clones this small value into every spawned task instead.
#[derive(Debug, Clone)]
struct JobSpec {
    out_dir: PathBuf,
    identity: TlsBundle,
    device_name: String,
    conflict: ConflictPolicy,
    checkpoint_dir: PathBuf,
    idle_timeout: Duration,
    policy: DirectPolicy,
}

impl JobSpec {
    fn of(spec: &ReceiveSpec) -> Self {
        Self {
            out_dir: spec.out_dir.clone(),
            identity: spec.identity.clone(),
            device_name: spec.device_name.clone(),
            conflict: spec.conflict,
            checkpoint_dir: spec.checkpoint_dir.clone(),
            idle_timeout: spec.idle_timeout,
            policy: spec.policy,
        }
    }
}

/// One accepted connection driven through the full receive protocol:
/// policy gate, Hello/Offer/Accept, manifest pages, resume answer, chunk
/// phase (single-stream or multistream), verify, commit, FinalAck. Both
/// [`receive_with_addr`] and [`serve_concurrently`] run this verbatim, so
/// the concurrent path is the single-job path — no protocol divergence.
///
/// Concurrency isolation when several of these run at once (see module
/// docs): the checkpoint store is opened per call, and distinct job ids
/// mean distinct `<job>.akjob` files written atomically by exactly one
/// task; `target` is shared immutably (`LocalSaveTarget` holds only a
/// canonicalized root path — no interior mutable state), and different
/// manifest entries map to different `.akpart` files, so a same-path race
/// between two jobs is resolved by the per-file conflict policy.
async fn receive_job(
    channel: DirectChannel,
    target: &LocalSaveTarget,
    spec: &JobSpec,
    started: Instant,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let limits = engine_limits();
    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;

    // Policy gate (protocol step 0): a refused path closes the connection
    // before any stream is accepted or frame moves.
    gate_channel(&spec.policy, &channel)?;
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let stream = channel.accept_bi().await?;
    let (mut tx, mut rx) = stream.split();

    tx.send_frame(&hello_frame(
        &spec.device_name,
        &spec.identity,
        RECV_CAPABILITIES,
    ))
    .await?;
    let peer_caps = expect_hello(&mut rx, &limits, spec.idle_timeout).await?;
    let multistream = (peer_caps & CAP_MULTISTREAM) != 0;

    // Offer -> consent -> Accept. Channel-level consent already happened via
    // the authorize callback; the offer is accepted per current product copy.
    let (job_id, file_count, total_bytes, chunk_size) =
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::Offer {
                        job_id,
                        file_count,
                        total_bytes,
                        chunk_size,
                        ..
                    },
                ..
            } => (job_id, file_count, total_bytes, chunk_size),
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        };
    progress(Progress::Offered {
        files: file_count,
        bytes: total_bytes,
    });
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Accept { job_id },
    })
    .await?;
    progress(Progress::Accepted {
        files: file_count,
        bytes: total_bytes,
    });

    // Manifest pages.
    let mut entries: Vec<ak_core::manifest::FileEntry> = Vec::new();
    loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::ManifestPage {
                        entries: page,
                        last_page,
                        ..
                    },
                ..
            } => {
                for wire in page {
                    entries.push(wire.try_into().map_err(|_| EngineError::PeerError {
                        code: ErrorCode::ProtocolUnsupported,
                        message: "peer sent an invalid manifest entry".into(),
                    })?);
                }
                if last_page {
                    break;
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    let chunk_size = NonZeroU32::new(chunk_size).ok_or_else(|| EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: "peer offered a zero chunk size".into(),
    })?;
    let manifest = Manifest {
        chunk_size,
        entries: entries.clone(),
    };
    progress(Progress::ManifestReceived);
    let job = ReceiverJob { manifest };

    let mut record = TaskRecord {
        job_id,
        direction: Direction::Receive,
        root: spec.out_dir.clone(),
        peer_fingerprint: Some(*channel.peer_fingerprint()),
        chunk_size: chunk_size.get(),
        total_files: job.manifest.file_count(),
        total_bytes: job.manifest.total_bytes(),
        state: "transferring".into(),
        created_unix_ms: now_ms(),
        updated_unix_ms: now_ms(),
        files: job
            .manifest
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| FileCheckpoint {
                file_index: index as u32,
                size: e.size,
                verified_prefix_chunks: 0,
            })
            .collect(),
    };

    // Storage-full precheck: refuse the job before any part file is created.
    // Unknown capacity passes (see [`check_headroom`]); a shortage is told to
    // the peer over the wire, checkpointed as "failed_storage_full", and
    // returned as the local verdict. The sender already maps a peer Error at
    // its ResumeRequest wait into `EngineError::PeerError` — numbers are not
    // invented on either side.
    if let Err(error) = check_headroom(target.available_bytes(), job.manifest.total_bytes()) {
        // Best effort: the job dies of a local condition regardless of
        // whether the peer still hears us.
        let _ = tx
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Error {
                    code: ErrorCode::StorageFull,
                    message: "insufficient free space at destination".into(),
                },
            })
            .await;
        record.state = "failed_storage_full".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        return Err(error);
    }

    // Prepare sinks (transparent resume from .akpart files) and answer.
    let mut sinks: Vec<Option<ak_storage::LocalFileSink>> =
        Vec::with_capacity(job.manifest.entries.len());
    let mut resume: Vec<ResumeEntry> = Vec::new();
    for (index, entry) in job.manifest.entries.iter().enumerate() {
        let sink = target.prepare_file_with_chunk_size(entry, spec.conflict, chunk_size)?;
        let prefix = sink.verified_prefix_chunks();
        if !entry.is_dir {
            resume.push(ResumeEntry {
                file_index: index as u32,
                verified_prefix_chunks: prefix,
            });
        }
        sinks.push(Some(sink));
    }
    let resumed_chunks: u64 = resume.iter().map(|r| r.verified_prefix_chunks).sum();
    if resumed_chunks > 0 {
        progress(Progress::Resuming {
            chunks: resumed_chunks,
        });
    }

    // Everything below can fail mid-transfer; the outcome wrapper then
    // discards empty part files so a failed receive leaves no litter.
    // Parts holding verified chunks always survive for resume.
    let outcome = async {
        // ResumeRequest, sharded: each frame carries at most
        // `max_resume_entries` entries; the last (the only one for an empty
        // answer) carries more = false.
        for message in
            resume_request_frames(job_id, &resume, usize::from(limits.max_resume_entries))
        {
            tx.send_frame(&Frame {
                version: ak_wire::VERSION,
                message,
            })
            .await?;
        }
        store.save(&record).map_err(EngineError::Checkpoint)?;

        // Chunk phase: the frozen single-stream loop, or per-file streams
        // when both sides announced CAP_MULTISTREAM.
        let tail = if multistream {
            TailHold::Events(
                multistream_receive(
                    &channel,
                    rx,
                    job_id,
                    chunk_size,
                    &job.manifest,
                    &mut sinks,
                    &mut record,
                    &store,
                    spec.idle_timeout,
                    progress,
                )
                .await?,
            )
        } else {
            TailHold::Stream(
                receive_single_stream(
                    rx,
                    &mut tx,
                    job_id,
                    chunk_size,
                    &job.manifest,
                    &mut sinks,
                    &mut record,
                    &store,
                    spec.idle_timeout,
                    progress,
                )
                .await?,
            )
        };

        // Verify and commit every file; only then report completion.
        progress(Progress::Verifying);
        for (index, sink_slot) in sinks.iter_mut().enumerate() {
            if let Some(mut sink) = sink_slot.take() {
                if job.manifest.entries[index].is_dir {
                    sink.finish().map_err(EngineError::Io)?;
                    continue;
                }
                sink.finish().map_err(EngineError::Io)?;
                record.files[index].verified_prefix_chunks =
                    job.manifest.entries[index].chunk_digests.len() as u64;
                progress(Progress::FileVerified {
                    file_index: index as u32,
                });
                progress(Progress::FileCommitted {
                    file_index: index as u32,
                    path: job.manifest.entries[index]
                        .rel_path
                        .to_string_lossy()
                        .replace('\\', "/"),
                });
            }
        }
        // Completion marker strictly after verification and commit (PROTOCOL).
        record.state = "completed".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::FinalAck {
                job_id,
                verified: true,
            },
        })
        .await?;
        tx.finish()?;
        progress(Progress::FinalAck { verified: true });
        // Hold the connection until the sender observes the FinalAck:
        // dropping this endpoint on return would close the QUIC connection
        // immediately and could discard the FinalAck before the peer reads
        // it.
        match tail {
            TailHold::Events(mut events) => {
                // The control-reader task drains until the sender
                // half-closes its side after reading the FinalAck; that
                // closes this channel.
                let _ = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
            }
            TailHold::Stream(mut rx_tail) => {
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if rx_tail.recv_frame(&limits).await.is_err() {
                            break;
                        }
                    }
                })
                .await;
            }
        }
        progress(Progress::Done);

        Ok(Report {
            job_id,
            files: job.manifest.file_count(),
            bytes: job.manifest.total_bytes(),
            duration: started.elapsed(),
            resumed_chunks,
            peer_fingerprint: Some(channel.peer_fingerprint().to_hex()),
            roots: Vec::new(),
        })
    }
    .await;

    match outcome {
        Ok(report) => Ok(report),
        Err(error) => {
            // Sinks consumed by multistream tasks are already gone; every
            // sink still held here with no verified chunk is pure litter.
            for slot in sinks.iter_mut() {
                let empty = slot
                    .as_ref()
                    .is_some_and(|sink| sink.verified_prefix_chunks() == 0);
                if empty {
                    if let Some(sink) = slot.take() {
                        sink.discard();
                    }
                }
            }
            Err(error)
        }
    }
}

/// Terminal state of the accept loop before every expected connection
/// arrived. Produces one error per unfilled result slot: `io::Error` and
/// [`ak_net::ConnectError`] are not `Clone`, so [`AcceptStop::Failed`]
/// hands the original error to the first unfilled slot and descriptive io
/// errors to the rest.
enum AcceptStop {
    /// No authorized peer within the idle budget — the expected way a
    /// concurrent receive ends early (a sender that never came).
    TimedOut(Duration),
    /// The listener itself failed mid-loop.
    Failed(Option<ak_net::ConnectError>),
}

impl AcceptStop {
    fn error(&mut self) -> EngineError {
        match self {
            AcceptStop::TimedOut(idle) => EngineError::IdleTimeout(*idle),
            AcceptStop::Failed(first) => {
                first.take().map(EngineError::Connect).unwrap_or_else(|| {
                    EngineError::Io(std::io::Error::other(
                        "listener failed before every expected connection arrived",
                    ))
                })
            }
        }
    }
}

/// Serve several senders concurrently on one bound endpoint: one
/// independent task per accepted connection, each running the exact
/// single-job receive protocol of [`receive_with_addr`] (the authorize
/// callback and the policy gate execute per connection).
///
/// Completion condition: `expected_jobs` connections accepted and every
/// task finished. There is deliberately no stop parameter — callers that
/// expect slow senders should raise [`ReceiveSpec::idle_timeout`], which
/// bounds each accept wait (an unfilled slot reports
/// [`EngineError::IdleTimeout`] instead of hanging forever).
///
/// Ordering: result slot `i` — and the first `progress` argument `i` — is
/// the `i`-th accepted connection, 0-based. The returned Vec always holds
/// exactly `expected_jobs` entries (`expected_jobs = 0` yields an empty
/// Vec): a task that fails carries its [`EngineError`] in its own slot
/// without affecting the others, and if the listener stops before every
/// expected connection arrived, the unfilled slots carry that accept
/// failure. A pre-listen failure (opening the destination or binding)
/// yields a one-element Vec with that error. Per-task progress events keep
/// their order; events from different tasks interleave arbitrarily. The
/// callback itself runs on this driver task and must be cheap.
///
/// Concurrency safety (see also the module docs and [`receive_job`]): the
/// receive directory is shared only as `&LocalSaveTarget`
/// (`prepare_file*` has no interior mutable state; different entries map
/// to different `.akpart` files), and every task opens its own checkpoint
/// store with its own `<job>.akjob` file (atomic writes) — so under
/// `ConflictPolicy::Fail` two jobs racing for the same target path fail
/// per file, exactly as two sequential receives would.
pub async fn serve_concurrently(
    spec: ReceiveSpec,
    expected_jobs: usize,
    progress: &(dyn Fn(u64, Progress) + Send + Sync),
) -> Vec<Result<Report, EngineError>> {
    let target = match LocalSaveTarget::open(&spec.out_dir) {
        Ok(target) => Arc::new(target),
        Err(error) => return vec![Err(error.into())],
    };
    let endpoint = match DirectEndpoint::bind(spec.bind, &spec.identity).await {
        Ok(endpoint) => endpoint,
        Err(error) => return vec![Err(error.into())],
    };
    let job_spec = JobSpec::of(&spec);
    let idle = spec.idle_timeout;
    // `Box<dyn Fn + Send + Sync>` is 'static: the authorize callback moves
    // into the accept task and still runs once per connection.
    let authorize = spec.authorize;

    // The accept loop runs in its own task because `DirectEndpoint::accept`
    // drives a QUIC handshake to completion, which is not cancel-safe:
    // polling it inside the driver's select! below would drop a
    // mid-handshake connection every time a progress event arrived first.
    // Here its future is never cancelled.
    let (channel_tx, mut channel_rx) =
        tokio::sync::mpsc::channel::<Result<DirectChannel, ak_net::ConnectError>>(1);
    let expected = expected_jobs;
    tokio::spawn(async move {
        let mut accepted = 0usize;
        while accepted < expected {
            match endpoint.accept_within(&authorize, idle).await {
                Ok(channel) => {
                    accepted += 1;
                    if channel_tx.send(Ok(channel)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = channel_tx.send(Err(error)).await;
                    break;
                }
            }
        }
        // Dropping the endpoint closes the listener for NEW connections
        // only; already-accepted channels keep working (the transport keeps
        // its driver alive while connections exist).
    });

    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, Progress)>();
    let mut results: Vec<Option<Result<Report, EngineError>>> =
        (0..expected_jobs).map(|_| None).collect();
    let mut set: tokio::task::JoinSet<(u64, Result<Report, EngineError>)> =
        tokio::task::JoinSet::new();
    let mut accepted = 0usize;
    let mut accept_stop: Option<AcceptStop> = None;
    let mut lost_task: Option<EngineError> = None;

    // Phase 1: fill every slot with an accepted connection (or a terminal
    // accept failure) while forwarding progress and recording finishes.
    while accepted < expected_jobs && accept_stop.is_none() {
        tokio::select! {
            channel = channel_rx.recv() => {
                match channel {
                    Some(Ok(channel)) => {
                        let job = job_spec.clone();
                        let events = events_tx.clone();
                        let shared = Arc::clone(&target);
                        let index = accepted as u64;
                        accepted += 1;
                        set.spawn(async move {
                            // Unbounded on purpose: the sync progress
                            // callback must never block or drop a transfer
                            // event; the driver loop below drains eagerly.
                            let forward = move |event: Progress| {
                                let _ = events.send((index, event));
                            };
                            (index, receive_job(channel, &shared, &job, Instant::now(), &forward).await)
                        });
                    }
                    Some(Err(ak_net::ConnectError::Timeout(idle))) => {
                        accept_stop = Some(AcceptStop::TimedOut(idle));
                    }
                    Some(Err(error)) => {
                        accept_stop = Some(AcceptStop::Failed(Some(error)));
                    }
                    None => {
                        // The accept task ended without delivering a
                        // terminal error; nothing more can ever arrive.
                        accept_stop = Some(AcceptStop::Failed(None));
                    }
                }
            }
            event = events_rx.recv() => {
                if let Some((index, event)) = event {
                    progress(index, event);
                }
            }
            joined = set.join_next(), if !set.is_empty() => {
                match joined {
                    Some(Ok((index, outcome))) => results[index as usize] = Some(outcome),
                    Some(Err(join)) => {
                        lost_task = Some(EngineError::Io(std::io::Error::other(format!(
                            "receive task failed: {join}"
                        ))));
                    }
                    None => {}
                }
            }
        }
    }

    // Phase 2: every slot is filled; drain the still-running tasks. The
    // driver keeps its own events_tx alive so events_rx.recv() below can
    // never observe a closed channel (and busy-loop on it).
    while !set.is_empty() {
        tokio::select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok((index, outcome))) => results[index as usize] = Some(outcome),
                    Some(Err(join)) => {
                        lost_task = Some(EngineError::Io(std::io::Error::other(format!(
                            "receive task failed: {join}"
                        ))));
                    }
                    None => break,
                }
            }
            event = events_rx.recv() => {
                if let Some((index, event)) = event {
                    progress(index, event);
                }
            }
        }
    }
    // All tasks joined, so every sender clone is dropped; release ours and
    // flush whatever was still buffered when the last join completed.
    drop(events_tx);
    while let Ok((index, event)) = events_rx.try_recv() {
        progress(index, event);
    }

    results
        .into_iter()
        .enumerate()
        .map(|(index, slot)| match slot {
            Some(outcome) => outcome,
            // Slots the listener never filled before stopping.
            None if index >= accepted => Err(accept_stop
                .as_mut()
                .map(|stop| stop.error())
                .unwrap_or_else(|| {
                    EngineError::Io(std::io::Error::other(
                        "listener stopped before this connection arrived",
                    ))
                })),
            // A spawned task that never delivered an outcome (join failure).
            None => Err(lost_task.take().unwrap_or_else(|| {
                EngineError::Io(std::io::Error::other("receive task produced no outcome"))
            })),
        })
        .collect()
}

/// How to hold the connection open until the sender observed the FinalAck.
enum TailHold {
    /// Single-stream mode: drain the control stream receive half.
    Stream(DirectRecvStream),
    /// Multistream mode: the control-reader task drains; its exit closes
    /// the event channel, which is what we await.
    Events(tokio::sync::mpsc::Receiver<StreamEvent>),
}

/// Frozen v1 chunk loop: one stream, chunks one way, windowed acks the
/// other way. Returns the receive half so the caller can hold the
/// connection for the FinalAck.
#[allow(clippy::too_many_arguments)]
async fn receive_single_stream(
    mut rx: DirectRecvStream,
    tx: &mut DirectSendStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<DirectRecvStream, EngineError> {
    let limits = engine_limits();
    let mut acks_since_checkpoint: u64 = 0;
    let mut current_file: u32 = u32::MAX;
    'transfer: loop {
        let frame = match recv_within(&mut rx, &limits, idle).await {
            Ok(frame) => frame,
            Err(EngineError::Stream(ak_net::StreamError::Connection(_))) => {
                return Err(EngineError::PeerClosed)
            }
            Err(e) => return Err(e),
        };
        match frame.message {
            Message::Chunk {
                file_index,
                chunk_index,
                data,
                ..
            } => {
                let sink = sinks
                    .get_mut(file_index as usize)
                    .and_then(Option::as_mut)
                    .ok_or(EngineError::PeerClosed)?;
                sink.write_chunk(chunk_index, &data)
                    .map_err(EngineError::Io)?;
                if let Some(slot) = record.files.get_mut(file_index as usize) {
                    slot.verified_prefix_chunks = sink.verified_prefix_chunks();
                }
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck {
                        job_id,
                        file_index,
                        chunk_index,
                    },
                })
                .await?;
                if file_index != current_file {
                    current_file = file_index;
                    if let Some(entry) = manifest.entries.get(file_index as usize) {
                        progress(Progress::FileStarted {
                            file_index,
                            path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                            bytes: entry.size,
                        });
                    }
                }
                progress(Progress::ChunkReceived {
                    file_index,
                    chunk_index,
                });
                acks_since_checkpoint += 1;
                if acks_since_checkpoint >= CHECKPOINT_EVERY_ACKS {
                    acks_since_checkpoint = 0;
                    record.updated_unix_ms = now_ms();
                    store.save(record).map_err(EngineError::Checkpoint)?;
                    if let Some(entry) = manifest.entries.get(file_index as usize) {
                        let cumulative = sink
                            .verified_prefix_chunks()
                            .saturating_mul(u64::from(chunk_size.get()))
                            .min(entry.size);
                        progress(Progress::BytesAcked {
                            file_index,
                            cumulative_bytes: cumulative,
                        });
                    }
                }
            }
            Message::Finalize { .. } => break 'transfer,
            Message::Control {
                control: Control::Cancel,
                ..
            } => return Err(EngineError::Cancelled),
            Message::Error { code, message } => {
                return Err(EngineError::PeerError { code, message })
            }
            other => return Err(unexpected(other)),
        }
    }
    Ok(rx)
}

/// Event from a receiver-side stream task back to the driver loop.
enum StreamEvent {
    Chunk {
        file_index: u32,
        chunk_index: u64,
    },
    /// The file's stream FINed after the last ack; the sink travels back for
    /// the shared verify/commit phase.
    FileDone {
        file_index: u32,
        sink: ak_storage::LocalFileSink,
    },
    /// A stream task or the control stream failed.
    Failed(EngineError),
    /// Finalize observed on the control stream.
    Finalize,
    Cancelled,
}

/// Receive a multistream job: accept per-file streams until Finalize, drive
/// each stream in its own task (write/verify/ack), keep the checkpoint
/// current at the same cadence as the single-stream path. Returns the event
/// channel tail for the FinalAck hold (the control task keeps draining).
#[allow(clippy::too_many_arguments)]
async fn multistream_receive(
    channel: &DirectChannel,
    control_rx: DirectRecvStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EngineError> {
    let limits = engine_limits();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);

    // Control-stream reader: forwards Finalize/Cancel/Error, then keeps
    // draining silently so the connection is held until the sender
    // half-closes after reading the FinalAck.
    let control_tx = events_tx.clone();
    tokio::spawn(async move {
        let mut rx = control_rx;
        let mut finalized = false;
        loop {
            let frame = match tokio::time::timeout(idle, rx.recv_frame(&limits)).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(ak_net::StreamError::Connection(_))) | Err(_) => break,
                Ok(Err(e)) => {
                    let _ = control_tx.send(StreamEvent::Failed(e.into())).await;
                    break;
                }
            };
            match frame.message {
                Message::Finalize { .. } if !finalized => {
                    finalized = true;
                    if control_tx.send(StreamEvent::Finalize).await.is_err() {
                        break;
                    }
                }
                Message::Control {
                    control: Control::Cancel,
                    ..
                } => {
                    let _ = control_tx.send(StreamEvent::Cancelled).await;
                    break;
                }
                Message::Error { code, message } => {
                    let _ = control_tx
                        .send(StreamEvent::Failed(EngineError::PeerError {
                            code,
                            message,
                        }))
                        .await;
                    break;
                }
                other => {
                    let _ = control_tx
                        .send(StreamEvent::Failed(unexpected(other)))
                        .await;
                    break;
                }
            }
        }
    });

    let mut finalized = false;
    let mut open_streams = 0usize;
    let mut acks_since_checkpoint: u64 = 0;
    let mut last_activity = Instant::now();
    loop {
        if finalized && open_streams == 0 {
            break;
        }
        // Accept waits share the idle budget: any chunk or ack event on an
        // already-open stream refreshes it.
        let budget = idle.saturating_sub(last_activity.elapsed());
        if budget.is_zero() {
            return Err(EngineError::IdleTimeout(idle));
        }
        tokio::select! {
            event = events_rx.recv() => {
                let Some(event) = event else {
                    // Every producer ended. Without Finalize nothing more can
                    // arrive; with Finalize all streams already finished.
                    if finalized {
                        break;
                    }
                    return Err(EngineError::PeerClosed);
                };
                last_activity = Instant::now();
                match event {
                    StreamEvent::Chunk { file_index, chunk_index } => {
                        let Some(slot) = record.files.get_mut(file_index as usize) else {
                            return Err(EngineError::PeerClosed);
                        };
                        slot.verified_prefix_chunks =
                            slot.verified_prefix_chunks.max(chunk_index + 1);
                        progress(Progress::ChunkReceived { file_index, chunk_index });
                        acks_since_checkpoint += 1;
                        if acks_since_checkpoint >= CHECKPOINT_EVERY_ACKS {
                            acks_since_checkpoint = 0;
                            record.updated_unix_ms = now_ms();
                            let prefix = slot.verified_prefix_chunks;
                            store.save(record).map_err(EngineError::Checkpoint)?;
                            if let Some(entry) = manifest.entries.get(file_index as usize) {
                                let cumulative = prefix
                                    .saturating_mul(u64::from(chunk_size.get()))
                                    .min(entry.size);
                                progress(Progress::BytesAcked {
                                    file_index,
                                    cumulative_bytes: cumulative,
                                });
                            }
                        }
                    }
                    StreamEvent::FileDone { file_index, sink } => {
                        open_streams = open_streams.saturating_sub(1);
                        if let Some(slot) = sinks.get_mut(file_index as usize) {
                            *slot = Some(sink);
                        }
                        if let Some(entry) = manifest.entries.get(file_index as usize) {
                            if let Some(checkpoint) = record.files.get_mut(file_index as usize) {
                                checkpoint.verified_prefix_chunks =
                                    entry.chunk_digests.len() as u64;
                            }
                        }
                    }
                    StreamEvent::Failed(error) => return Err(error),
                    StreamEvent::Finalize => finalized = true,
                    StreamEvent::Cancelled => return Err(EngineError::Cancelled),
                }
            }
            accepted = tokio::time::timeout(budget, channel.accept_bi()), if !finalized => {
                match accepted {
                    Err(_elapsed) => return Err(EngineError::IdleTimeout(idle)),
                    Ok(Err(e)) => return Err(EngineError::Stream(e)),
                    Ok(Ok(mut stream)) => {
                        last_activity = Instant::now();
                        // The first frame on a new stream names its file.
                        let first = match tokio::time::timeout(idle, stream.recv_frame(&limits)).await {
                            Ok(Ok(frame)) => frame,
                            Ok(Err(e)) => return Err(EngineError::Stream(e)),
                            Err(_) => return Err(EngineError::IdleTimeout(idle)),
                        };
                        let Message::Chunk { file_index, .. } = first.message else {
                            return Err(unexpected(first.message));
                        };
                        let sink = sinks
                            .get_mut(file_index as usize)
                            .and_then(Option::take)
                            .ok_or(EngineError::PeerClosed)?;
                        if let Some(entry) = manifest.entries.get(file_index as usize) {
                            progress(Progress::FileStarted {
                                file_index,
                                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                                bytes: entry.size,
                            });
                        }
                        open_streams += 1;
                        receive_stream_task(
                            stream,
                            sink,
                            file_index,
                            first,
                            job_id,
                            idle,
                            events_tx.clone(),
                        );
                    }
                }
            }
        }
    }
    Ok(events_rx)
}

/// Drive one accepted per-file stream: write/verify/ack every chunk until
/// the sender half-closes, then hand the sink back to the driver.
fn receive_stream_task(
    mut stream: DirectStream,
    mut sink: ak_storage::LocalFileSink,
    file_index: u32,
    first: Frame,
    job_id: JobId,
    idle: Duration,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
) {
    tokio::spawn(async move {
        let limits = engine_limits();
        let mut current = Some(first);
        let result: Result<(), EngineError> = loop {
            let frame = match current.take() {
                Some(frame) => frame,
                None => match tokio::time::timeout(idle, stream.recv_frame(&limits)).await {
                    Ok(Ok(frame)) => frame,
                    // The sender half-closes only after every ack, so a clean
                    // early end means the file is complete.
                    Ok(Err(ak_net::StreamError::Connection(message)))
                        if message.starts_with("stream finished early") =>
                    {
                        break Ok(());
                    }
                    Ok(Err(ak_net::StreamError::Connection(_))) => {
                        break Err(EngineError::PeerClosed)
                    }
                    Ok(Err(e)) => break Err(EngineError::Stream(e)),
                    Err(_) => break Err(EngineError::IdleTimeout(idle)),
                },
            };
            match frame.message {
                Message::Chunk {
                    chunk_index, data, ..
                } => {
                    if let Err(e) = sink.write_chunk(chunk_index, &data) {
                        break Err(EngineError::Io(e));
                    }
                    if let Err(e) = stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::ChunkAck {
                                job_id,
                                file_index,
                                chunk_index,
                            },
                        })
                        .await
                    {
                        break Err(EngineError::Stream(e));
                    }
                    if events
                        .send(StreamEvent::Chunk {
                            file_index,
                            chunk_index,
                        })
                        .await
                        .is_err()
                    {
                        break Err(EngineError::Cancelled);
                    }
                }
                Message::Control {
                    control: Control::Cancel,
                    ..
                } => break Err(EngineError::Cancelled),
                Message::Error { code, message } => {
                    break Err(EngineError::PeerError { code, message })
                }
                other => break Err(unexpected(other)),
            }
        };
        // Best-effort half-close of the ack side; the peer already read all
        // acks before it half-closed ours.
        let _ = stream.finish_send();
        let _ = events
            .send(match result {
                Ok(()) => StreamEvent::FileDone { file_index, sink },
                Err(error) => StreamEvent::Failed(error),
            })
            .await;
    });
}

/// Run one receive to completion (see [`receive_with_addr`]).
pub async fn receive(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let (_tx, _rx) = tokio::sync::oneshot::channel::<SocketAddr>();
    receive_with_addr(spec, progress, _tx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::DeviceKeyPair;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn count_entries(dir: &Path) -> (u64, u64) {
        let mut files = 0u64;
        let mut bytes = 0u64;
        fn walk(dir: &Path, files: &mut u64, bytes: &mut u64) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    walk(&entry.path(), files, bytes);
                } else if !entry
                    .path()
                    .to_string_lossy()
                    .ends_with(ak_storage::PART_SUFFIX)
                {
                    *files += 1;
                    *bytes += meta.len();
                }
            }
        }
        walk(dir, &mut files, &mut bytes);
        (files, bytes)
    }

    fn specs(src: &Path, dst: &Path, chunk_kb: u32) -> (SendSpec, ReceiveSpec, Fingerprint) {
        let sender_key = DeviceKeyPair::generate();
        let receiver_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let receiver_fp = receiver_key.fingerprint();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let mut send_spec = SendSpec::new(
            src.to_path_buf(),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            receiver_fp,
            sender_bundle,
        );
        send_spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
        send_spec.checkpoint_dir = checkpoint.clone();
        let mut recv = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.chunk_size = send_spec.chunk_size;
        recv.checkpoint_dir = checkpoint;
        (send_spec, recv, receiver_fp)
    }

    fn noop(_progress: Progress) {}

    /// Loopback transfer under the default policy: the step-0 gate passes
    /// LocalDirect on both sides and the frozen flow runs unchanged — this
    /// is the default-policy gate integration.
    #[tokio::test]
    async fn loopback_directory_transfer_is_byte_identical() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"hello ak");
        write(
            &src.path().join("nested/deep/b.bin"),
            &(0..50_000u32).map(|i| i as u8).collect::<Vec<_>>(),
        );
        fs::create_dir_all(src.path().join("empty/dir")).unwrap();

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
        send_spec.device_name = "sender-test".into();
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let sender_spec = send_spec.clone();
        let sender = tokio::spawn(async move { send(sender_spec, &noop).await });
        let recv_outcome = receiver.await.unwrap();
        let send_outcome = sender.await.unwrap();
        if let Err(error) = &recv_outcome {
            eprintln!("receiver error: {error:?}");
        }
        let report = send_outcome.expect("send should complete");
        let recv_report = recv_outcome.expect("receive should complete");

        assert_eq!(report.files, 6); // 2 files + 4 directory entries (nested, deep, empty, empty/dir)
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
        assert_eq!(
            fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
            (0..50_000u32).map(|i| i as u8).collect::<Vec<_>>()
        );
        assert!(dst.path().join("empty/dir").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (2, 8 + 50_000));
        assert!(report.duration.as_nanos() > 0);
    }

    #[tokio::test]
    async fn resume_skips_verified_prefix_without_resending() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        // 8 KiB file with 1 KiB chunks gives an 8-chunk prefix to resume from.
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("resume.bin"), &payload);
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
        let checkpoint = send_spec.checkpoint_dir.clone();

        // Simulate an interrupted earlier run: 3 verified chunks on disk.
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let source_probe = LocalSource::open(&send_spec.source_root, send_spec.chunk_size).unwrap();
        let entry = &source_probe.manifest().entries[0];
        let mut sink = target
            .prepare_file_with_chunk_size(entry, ConflictPolicy::Fail, send_spec.chunk_size)
            .unwrap();
        for chunk in 0..3u64 {
            let mut buf = vec![0u8; 1024];
            let len = source_probe.read_chunk(0, chunk, &mut buf).unwrap();
            sink.write_chunk(chunk, &buf[..len]).unwrap();
        }
        drop(sink);

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("send should complete");
        let recv_report = receiver.await.unwrap().expect("receive should complete");

        assert_eq!(
            report.resumed_chunks, 3,
            "sender should skip 3 verified chunks"
        );
        assert_eq!(recv_report.resumed_chunks, 3);
        assert_eq!(fs::read(dst.path().join("resume.bin")).unwrap(), payload);
        let _ = FileCheckpointStore::open(&checkpoint).map(|s| s.remove(&report.job_id));
    }

    /// T04/T10 analog: the receiver dies mid-transfer. The sender must fail
    /// (never fall back to anything), and the checkpoint plus part file must
    /// survive so a second run resumes instead of restarting from zero.
    #[tokio::test]
    async fn mid_transfer_disconnect_keeps_checkpoint_and_resumes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
        write(&src.path().join("mid.bin"), &payload);
        let (send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 16 chunks of 1 KiB
        let checkpoint_dir = send_spec.checkpoint_dir.clone();
        let part_path = dst
            .path()
            .join(format!("mid.bin{}", ak_storage::PART_SUFFIX));

        let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
        let progress = std::sync::Arc::new(move |event: Progress| {
            if let Progress::ChunkReceived { .. } = event {
                chunks_tx.send_modify(|count| *count += 1);
            }
        });
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = {
            let progress = std::sync::Arc::clone(&progress);
            tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
        };
        let mut first_spec = send_spec.clone();
        first_spec.peer = addr_rx.await.unwrap();
        let sender = tokio::spawn(async move { send(first_spec, &noop).await });

        // Kill the receiver once a few chunks are durable.
        while *chunks_rx.borrow() < 4 {
            chunks_rx.changed().await.unwrap();
        }
        receiver.abort();
        let send_result = sender.await.unwrap();
        assert!(
            matches!(
                send_result,
                Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
            ),
            "sender must fail without any fallback, got {send_result:?}"
        );
        assert!(part_path.exists(), "part file must survive the disconnect");
        let store = FileCheckpointStore::open(&checkpoint_dir).unwrap();
        assert!(
            !store.list().unwrap().is_empty(),
            "checkpoint must survive the disconnect"
        );

        // Second run: same deterministic job id resumes and completes.
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("resumed send should complete");
        receiver
            .await
            .unwrap()
            .expect("resumed receive should complete");

        assert!(
            report.resumed_chunks >= 1,
            "second run should resume, got {}",
            report.resumed_chunks
        );
        assert_eq!(fs::read(dst.path().join("mid.bin")).unwrap(), payload);
        assert!(!part_path.exists(), "committed file leaves no part behind");
    }

    /// A v4 endpoint dialing a v6 peer must fail fast instead of hanging in
    /// QUIC's retry loop (defect found by the IPv6 scenario tests).
    #[tokio::test]
    async fn connect_family_mismatch_fails_fast_instead_of_hanging() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("f.txt"), b"x");
        let (mut send_spec, _, _) = specs(src.path(), dst.path(), 16);
        send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0)); // IPv4 endpoint
        send_spec.peer = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 9)); // IPv6 peer, nothing listening
        let started = Instant::now();
        let err = send(send_spec, &noop)
            .await
            .expect_err("family mismatch must fail");
        assert!(
            matches!(
                &err,
                EngineError::Connect(ak_net::ConnectError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
            ),
            "got: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must fail fast, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wrong_pin_fails_closed_before_any_transfer() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("secret.txt"), b"must not leak");
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
        // Pin a fingerprint that is NOT the receiver's.
        send_spec.pinned = DeviceKeyPair::generate().fingerprint();
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let err = send(send_spec, &noop)
            .await
            .expect_err("wrong pin must fail");
        assert!(matches!(err, EngineError::Connect(_)), "got: {err:?}");
        // Nothing may be written even though the offer was never reached.
        assert_eq!(count_entries(dst.path()), (0, 0));
        let _ = receiver.await;
    }

    #[tokio::test]
    async fn receiver_authorize_false_rejects_the_sender() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("x.txt"), b"x");
        let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 16);
        recv.authorize = Box::new(|_fp| false);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();
        send_spec.peer = addr;
        let err = send(send_spec, &noop)
            .await
            .expect_err("unauthorized sender must fail");
        // The TLS handshake itself succeeds (client cert is validly
        // self-signed); rejection surfaces on first use as a lost stream.
        assert!(
            matches!(err, EngineError::Connect(_) | EngineError::Stream(_)),
            "got: {err:?}"
        );
        let recv_err = receiver.await.unwrap().expect_err("receiver must reject");
        assert!(matches!(
            recv_err,
            EngineError::Connect(ak_net::ConnectError::Unauthorized)
        ));
        assert_eq!(count_entries(dst.path()), (0, 0));
    }

    #[tokio::test]
    async fn job_id_is_stable_for_source_and_sensitive_to_content() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"content-a");
        let manifest = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
            .unwrap()
            .manifest()
            .clone();
        let id_a = job_id_for("f.bin", &manifest);
        let id_b = job_id_for("f.bin", &manifest);
        assert_eq!(id_a, id_b);

        write(&src.path().join("f.bin"), b"content-b");
        let manifest2 = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
            .unwrap()
            .manifest()
            .clone();
        assert_ne!(id_a, job_id_for("f.bin", &manifest2));
    }

    // ------------------------------------------------------------------
    // Storage-full precheck (pure decision)
    // ------------------------------------------------------------------

    #[test]
    fn check_headroom_passes_unknown_available() {
        assert!(check_headroom(None, 0).is_ok());
        assert!(check_headroom(None, u64::MAX).is_ok());
    }

    #[test]
    fn check_headroom_passes_exact_fit() {
        assert!(check_headroom(Some(0), 0).is_ok());
        assert!(check_headroom(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_headroom_rejects_shortage_with_the_real_numbers() {
        assert!(matches!(
            check_headroom(Some(999), 1000),
            Err(EngineError::StorageFull {
                available: 999,
                needed: 1000
            })
        ));
        assert!(matches!(
            check_headroom(Some(0), 1),
            Err(EngineError::StorageFull {
                available: 0,
                needed: 1
            })
        ));
    }

    // The receive-path integration (a real volume reporting less free space
    // than the job size) cannot be constructed on a real local disk in this
    // environment: Windows temp directories have no quota mechanism and the
    // free-space probe reads the live volume. Only the pure decision above
    // is covered by automated tests; the Error-frame and
    // "failed_storage_full" checkpoint wiring is untested here by design
    // (no faked numbers, no faked filesystems). See the round-3 report.

    // ------------------------------------------------------------------
    // Policy gate (pre-transport route authorization)
    // ------------------------------------------------------------------

    #[test]
    fn authorize_path_local_direct_passes_even_with_local_only_policy() {
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        assert!(authorize_path(&DirectPolicy::default(), PathKind::LocalDirect).is_ok());
        assert!(authorize_path(&local_only, PathKind::LocalDirect).is_ok());
    }

    #[test]
    fn authorize_path_internet_direct_follows_allow_internet() {
        assert!(
            authorize_path(&DirectPolicy::default(), PathKind::InternetDirect).is_ok(),
            "the default policy allows internet"
        );
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        assert!(matches!(
            authorize_path(&local_only, PathKind::InternetDirect),
            Err(EngineError::RouteForbidden(PathKind::InternetDirect))
        ));
    }

    #[test]
    fn authorize_path_rejects_relay_and_unknown_even_when_permissive() {
        for path in [PathKind::Relay, PathKind::Unknown] {
            assert!(
                matches!(
                    authorize_path(&DirectPolicy::default(), path),
                    Err(EngineError::RouteForbidden(refused)) if refused == path
                ),
                "Relay/Unknown must always be refused, got a pass for {path:?}"
            );
        }
    }

    /// The documented API contract for integrators (e.g. CLI flags):
    /// untouched specs carry `allow_internet = true, allow_cellular = false`.
    #[test]
    fn specs_default_to_the_standard_direct_policy() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let (send_spec, recv_spec, _) = specs(src.path(), dst.path(), 16);
        for policy in [send_spec.policy, recv_spec.policy] {
            assert!(policy.allow_internet, "default must allow internet");
            assert!(!policy.allow_cellular, "default must refuse cellular");
        }
    }

    /// `allow_internet = false` must not affect a loopback transfer: the
    /// loopback peer classifies `LocalDirect` (ak-net::classify treats
    /// loopback as always local), which no policy can lock out. Both sides
    /// carry the local-only policy here.
    ///
    /// The InternetDirect-refused branch of the wiring cannot be produced
    /// naturally on a single machine: every address a test can actually
    /// connect to here classifies LocalDirect, and `DirectChannel` exposes
    /// no public constructor to forge a path (by design). That branch is
    /// covered by the pure unit branches above instead.
    #[tokio::test]
    async fn local_only_policy_still_completes_loopback_transfer() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("local-only.bin"), &payload);
        let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 4);
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        send_spec.policy = local_only;
        recv.policy = local_only;

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("local-only policy must not affect a LocalDirect loopback");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("local-only policy must not affect a LocalDirect loopback");
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(
            fs::read(dst.path().join("local-only.bin")).unwrap(),
            payload
        );
    }

    // ------------------------------------------------------------------
    // Multi-root send
    // ------------------------------------------------------------------

    /// Three-root fixture: two directory trees (nesting plus empty dirs) and
    /// one standalone file. Returns (tree_a, tree_b, loose, deep_payload).
    fn three_root_fixture(
        base: &Path,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<u8>,
    ) {
        let tree_a = base.join("photos");
        write(&tree_a.join("a.txt"), b"hello ak");
        let deep: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
        write(&tree_a.join("nested/deep/b.bin"), &deep);
        fs::create_dir_all(tree_a.join("empty/dir")).unwrap();
        let tree_b = base.join("docs");
        write(&tree_b.join("notes.md"), b"notes");
        fs::create_dir_all(tree_b.join("solo")).unwrap();
        let loose = base.join("readme.txt");
        write(&loose, b"loose file");
        (tree_a, tree_b, loose, deep)
    }

    #[tokio::test]
    async fn multi_root_transfer_merges_three_roots_byte_identical() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();

        let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 16);
        send_spec.add_root(tree_b.clone());
        send_spec.add_root(loose.clone());

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("multi-root send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multi-root receive completes");

        // Primary root first, extras in add order; the receiver reports none.
        assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
        assert_eq!(recv_report.roots, Vec::<String>::new());
        // 4 files + 5 directory entries (photos/{empty,empty/dir,nested,
        // nested/deep} + docs/solo); a single-file root adds no directory.
        assert_eq!(report.files, 9);
        assert_eq!(report.bytes, 8 + 30_000 + 5 + 10);
        assert_eq!(report.bytes, recv_report.bytes);

        assert_eq!(
            fs::read(dst.path().join("photos/a.txt")).unwrap(),
            b"hello ak"
        );
        assert_eq!(
            fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
            deep
        );
        assert!(dst.path().join("photos/empty/dir").is_dir());
        assert_eq!(
            fs::read(dst.path().join("docs/notes.md")).unwrap(),
            b"notes"
        );
        assert!(dst.path().join("docs/solo").is_dir());
        // A single-file root lands under a top-level directory of its own name.
        assert_eq!(
            fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
            b"loose file"
        );
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
    }

    #[tokio::test]
    async fn multi_root_multistream_transfer_is_byte_identical() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();

        let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 1);
        send_spec.add_root(tree_b.clone());
        send_spec.add_root(loose.clone());
        send_spec.multistream = true;
        send_spec.in_flight_window = 4;

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("multi-root multistream send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multi-root multistream receive completes");

        assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
        assert_eq!(report.files, 9);
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(
            fs::read(dst.path().join("photos/a.txt")).unwrap(),
            b"hello ak"
        );
        assert_eq!(
            fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
            deep
        );
        assert!(dst.path().join("photos/empty/dir").is_dir());
        assert_eq!(
            fs::read(dst.path().join("docs/notes.md")).unwrap(),
            b"notes"
        );
        assert!(dst.path().join("docs/solo").is_dir());
        assert_eq!(
            fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
            b"loose file"
        );
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
    }

    #[tokio::test]
    async fn multi_root_rejects_duplicate_root_names() {
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("data");
        let b = base.path().join("other").join("data");
        write(&a.join("one.txt"), b"one");
        write(&b.join("two.txt"), b"two");

        let dst = tempfile::tempdir().unwrap();
        let (mut send_spec, _recv, _) = specs(&a, dst.path(), 16);
        send_spec.add_root(b);
        // No receiver is spawned: the job must fail before any connect.
        let err = send(send_spec, &noop)
            .await
            .expect_err("duplicate root names must fail the job");
        assert!(
            matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
            "got: {err:?}"
        );
        assert_eq!(count_entries(dst.path()), (0, 0));
    }

    #[test]
    fn multi_root_manifest_is_deterministic_and_prefixed() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, _) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();
        let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
        spec.add_root(tree_b.clone());
        spec.add_root(loose.clone());

        let first = SourceSet::open(&spec).unwrap();
        let second = SourceSet::open(&spec).unwrap();
        assert!(first.multi);
        assert_eq!(first.display_name(), "multi");
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(
            job_id_for("multi", &first.manifest),
            job_id_for("multi", &second.manifest)
        );
        let rels: Vec<String> = first
            .manifest
            .entries
            .iter()
            .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(rels.len(), 9);
        assert!(rels.iter().all(|r| {
            r.starts_with("photos/") || r.starts_with("docs/") || r.starts_with("readme.txt/")
        }));
        // Roots stay contiguous so parents always precede children.
        let first_docs = rels.iter().position(|r| r.starts_with("docs/")).unwrap();
        assert!(rels[..first_docs].iter().all(|r| r.starts_with("photos/")));
    }

    #[test]
    fn single_root_source_set_is_the_frozen_manifest() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"x");
        let plain = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap()).unwrap();
        let (spec, _, _) = specs(src.path(), src.path(), 1);
        let set = SourceSet::open(&spec).unwrap();
        assert!(!set.multi);
        assert_eq!(set.manifest, plain.manifest().clone());
        assert_eq!(set.display_name(), plain.root_name());
    }

    #[test]
    fn empty_root_directory_becomes_a_top_level_entry() {
        let base = tempfile::tempdir().unwrap();
        let tree_a = base.path().join("photos");
        write(&tree_a.join("a.txt"), b"hello ak");
        let empty_root = base.path().join("nothing");
        fs::create_dir_all(&empty_root).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
        spec.add_root(empty_root.clone());
        let set = SourceSet::open(&spec).unwrap();
        let rels: Vec<String> = set
            .manifest
            .entries
            .iter()
            .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(rels, vec!["photos/a.txt", "nothing"]);
        assert!(set.manifest.entries[1].is_dir);
    }

    // ------------------------------------------------------------------
    // Concurrent receive (serve_concurrently)
    // ------------------------------------------------------------------

    /// serve_concurrently keeps its signature minimal (no addr_tx), so
    /// tests probe a free UDP port exactly the way the receiver will bind
    /// one; same-process UDP rebinding is immediate on every platform we
    /// run the suite on.
    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        socket.local_addr().unwrap().port()
    }

    fn concurrent_receive_spec(dst: &Path, checkpoint: &Path) -> (ReceiveSpec, Fingerprint) {
        let receiver_key = DeviceKeyPair::generate();
        let mut recv = ReceiveSpec::new(
            dst.to_path_buf(),
            ak_identity::tls::make_self_signed(&receiver_key),
        );
        recv.bind = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
        recv.checkpoint_dir = checkpoint.to_path_buf();
        (recv, receiver_key.fingerprint())
    }

    fn concurrent_send_spec(
        src: PathBuf,
        peer: SocketAddr,
        pinned: Fingerprint,
        chunk_kb: u32,
        checkpoint: &Path,
    ) -> SendSpec {
        let mut spec = SendSpec::new(
            src,
            peer,
            pinned,
            ak_identity::tls::make_self_signed(&DeviceKeyPair::generate()),
        );
        spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
        spec.checkpoint_dir = checkpoint.to_path_buf();
        spec
    }

    /// Two independent senders (own keys and certificates, own source
    /// trees, one pinned receiver) land concurrently in one
    /// serve_concurrently(2) receiver: both jobs complete, bytes are
    /// identical on both sides, and every progress event carries its task
    /// ordinal (0/1, accept order) with each ordinal observing its own
    /// Connected..Done sequence.
    #[tokio::test]
    async fn two_senders_concurrently_complete_into_one_receiver() {
        let src_a = tempfile::tempdir().unwrap();
        let src_b = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload_a: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
        let payload_b: Vec<u8> = (0..24_000u32).map(|i| (i % 251) as u8).collect();
        write(&src_a.path().join("alpha/a.bin"), &payload_a);
        write(&src_b.path().join("beta/b.bin"), &payload_b);
        fs::create_dir_all(src_b.path().join("beta/empty")).unwrap();

        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
        let spec_a = concurrent_send_spec(
            src_a.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            1,
            &checkpoint,
        );
        let spec_b = concurrent_send_spec(
            src_b.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            4,
            &checkpoint,
        );

        let seen: Arc<std::sync::Mutex<Vec<(u64, Progress)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let progress = move |index: u64, event: Progress| log.lock().unwrap().push((index, event));
        let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &progress).await });
        // Let the listener bind before the senders dial (the port was
        // probed, not reported).
        tokio::time::sleep(Duration::from_millis(150)).await;

        let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
        let sender_b = tokio::spawn(async move { send(spec_b, &noop).await });
        let report_a = sender_a.await.unwrap().expect("sender A completes");
        let report_b = sender_b.await.unwrap().expect("sender B completes");
        let outcomes = receiver.await.unwrap();

        assert_eq!(outcomes.len(), 2);
        let mut reports = Vec::with_capacity(2);
        for (index, outcome) in outcomes.into_iter().enumerate() {
            reports.push(outcome.unwrap_or_else(|error| panic!("job {index} failed: {error:?}")));
        }
        // Slots are in accept order, which is racy; compare as a set.
        let mut by_bytes: Vec<(u64, u64)> = reports.iter().map(|r| (r.bytes, r.files)).collect();
        by_bytes.sort_unstable();
        let mut expected = vec![(24_000, 3), (30_000, 2)];
        expected.sort_unstable();
        assert_eq!(by_bytes, expected, "reports: {reports:?}");
        assert_eq!(report_a.bytes, 30_000);
        assert_eq!(report_b.bytes, 24_000);

        assert_eq!(fs::read(dst.path().join("alpha/a.bin")).unwrap(), payload_a);
        assert_eq!(fs::read(dst.path().join("beta/b.bin")).unwrap(), payload_b);
        assert!(dst.path().join("beta/empty").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (2, 30_000 + 24_000));

        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().all(|(index, _)| *index < 2),
            "task ordinals out of range: {seen:?}"
        );
        for index in 0..2u64 {
            assert!(
                seen.iter()
                    .any(|(i, e)| *i == index && matches!(e, Progress::Connected { .. })),
                "no Connected event for task {index}: {seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|(i, e)| *i == index && matches!(e, Progress::Done)),
                "no Done event for task {index}: {seen:?}"
            );
        }
    }

    /// One sender aborted mid-transfer must not sink the other job: the
    /// Vec holds exactly one Ok (the healthy job, bytes intact) and one Err
    /// (the aborted one), and the aborted job's part file survives with its
    /// verified prefix for resume.
    #[tokio::test]
    async fn one_failing_sender_does_not_sink_the_other_job() {
        let src_a = tempfile::tempdir().unwrap();
        let src_b = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload_a: Vec<u8> = (0..20_000u32).map(|i| (i % 249) as u8).collect();
        let payload_b: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
        write(&src_a.path().join("solid/a.bin"), &payload_a);
        write(&src_b.path().join("doomed/b.bin"), &payload_b);

        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
        let spec_a = concurrent_send_spec(
            src_a.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            4,
            &checkpoint,
        );
        let mut spec_b = concurrent_send_spec(
            src_b.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            1,
            &checkpoint,
        );
        // Park after every chunk: the abort below then lands deterministically
        // mid-transfer.
        spec_b.in_flight_window = 1;

        let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &|_, _| {}).await });
        tokio::time::sleep(Duration::from_millis(150)).await;

        let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
        let (sent_tx, mut sent_rx) = tokio::sync::watch::channel(0u64);
        let sender_b = {
            let progress = std::sync::Arc::new(move |event: Progress| {
                if let Progress::ChunkSent { .. } = event {
                    sent_tx.send_modify(|count| *count += 1);
                }
            });
            tokio::spawn(async move { send(spec_b, &*progress).await })
        };
        while *sent_rx.borrow() < 2 {
            sent_rx.changed().await.unwrap();
        }
        sender_b.abort();

        let report_a = sender_a.await.unwrap().expect("healthy sender completes");
        let outcomes = receiver.await.unwrap();
        assert_eq!(outcomes.len(), 2);
        let oks: Vec<&Report> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        let errs: Vec<&EngineError> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();
        assert_eq!(oks.len(), 1, "outcomes: {outcomes:?}");
        assert_eq!(errs.len(), 1, "outcomes: {outcomes:?}");
        assert_eq!(oks[0].bytes, report_a.bytes);

        assert_eq!(fs::read(dst.path().join("solid/a.bin")).unwrap(), payload_a);
        // The aborted job leaves its verified part file for resume, and no
        // committed final file.
        assert!(
            dst.path()
                .join(format!("doomed/b.bin{}", ak_storage::PART_SUFFIX))
                .exists(),
            "part file must survive the aborted sender"
        );
        assert!(!dst.path().join("doomed/b.bin").exists());
    }

    /// The no-hang contract: a slot whose sender never arrives fails with
    /// IdleTimeout once the (per-accept) idle budget expires, instead of
    /// blocking forever.
    #[tokio::test]
    async fn serve_concurrently_times_out_unfilled_slots_instead_of_hanging() {
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (mut recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
        recv.idle_timeout = Duration::from_millis(200);
        let started = Instant::now();
        let outcomes = serve_concurrently(recv, 1, &|_index: u64, _event: Progress| {}).await;
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(outcomes[0], Err(EngineError::IdleTimeout(_))),
            "got: {outcomes:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not hang, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn serve_concurrently_zero_expected_jobs_returns_empty() {
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
        let outcomes = serve_concurrently(recv, 0, &|_index: u64, _event: Progress| {}).await;
        assert!(outcomes.is_empty());
    }

    /// T12 analog: >4 GiB via a sparse file. Release-only (debug hashing is
    /// too slow); run explicitly with `cargo test --release -p ak-engine --
    /// --ignored`.
    #[tokio::test]
    #[ignore = "5 GiB sparse transfer; run in release only"]
    async fn five_gib_sparse_file_transfers_with_u64_offsets() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let big = src.path().join("big.bin");
        let file = fs::File::create(&big).unwrap();
        file.set_len(5_u64 * 1024 * 1024 * 1024 + 13).unwrap();
        drop(file);
        write(&src.path().join("tail.txt"), b"after the big one");

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1024);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(async move { receive_with_addr(recv, &noop, addr_tx).await });
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("5 GiB send");
        receiver.await.unwrap().expect("5 GiB receive");
        assert_eq!(report.bytes, 5 * 1024 * 1024 * 1024 + 13 + 17); // big.bin + tail.txt (17 bytes)
        assert_eq!(
            fs::metadata(dst.path().join("big.bin")).unwrap().len(),
            5 * 1024 * 1024 * 1024 + 13
        );
        assert_eq!(
            fs::read(dst.path().join("tail.txt")).unwrap(),
            b"after the big one"
        );
    }

    /// Drive a raw v1 receiver through Hello/Offer/Accept/Manifest and reply
    /// with an empty ResumeRequest; returns the job id. Used by the pause and
    /// fallback tests.
    async fn fake_receiver_preamble(
        stream: &mut ak_net::DirectStream,
        identity: &TlsBundle,
        capabilities: u16,
    ) -> JobId {
        let limits = engine_limits();
        let _ = stream.recv_frame(&limits).await.unwrap(); // sender Hello
        stream
            .send_frame(&hello_frame("fake", identity, capabilities))
            .await
            .unwrap();
        let job_id = match stream.recv_frame(&limits).await.unwrap().message {
            Message::Offer { job_id, .. } => job_id,
            other => panic!("expected Offer, got {other:?}"),
        };
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Accept { job_id },
            })
            .await
            .unwrap();
        loop {
            match stream.recv_frame(&limits).await.unwrap().message {
                Message::ManifestPage { last_page, .. } => {
                    if last_page {
                        break;
                    }
                }
                other => panic!("expected ManifestPage, got {other:?}"),
            }
        }
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ResumeRequest {
                    job_id,
                    files: Vec::new(),
                    more: false,
                },
            })
            .await
            .unwrap();
        job_id
    }

    /// Ack every remaining chunk on the stream, answer Finalize with a
    /// positive FinalAck, and return the number of chunks acked.
    async fn fake_ack_until_finalize(stream: &mut ak_net::DirectStream, job_id: JobId) -> u64 {
        let limits = engine_limits();
        let mut acked = 0u64;
        loop {
            match stream.recv_frame(&limits).await.unwrap().message {
                Message::Chunk {
                    file_index,
                    chunk_index,
                    ..
                } => {
                    stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::ChunkAck {
                                job_id,
                                file_index,
                                chunk_index,
                            },
                        })
                        .await
                        .unwrap();
                    acked += 1;
                }
                Message::Finalize { .. } => {
                    stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::FinalAck {
                                job_id,
                                verified: true,
                            },
                        })
                        .await
                        .unwrap();
                    // Hold the connection like the real receiver does: the
                    // sender reads the FinalAck and then half-closes. Dropping
                    // the endpoint now could discard the FinalAck in flight.
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if stream.recv_frame(&limits).await.is_err() {
                                break;
                            }
                        }
                    })
                    .await;
                    return acked;
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    }

    /// The peer completes the handshake then goes silent: the sender must
    /// fail with IdleTimeout (never hang), keeping its checkpoint.
    #[tokio::test]
    async fn idle_timeout_when_peer_goes_silent_after_handshake() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("idle.bin"), b"payload");
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 16);
        send_spec.idle_timeout = Duration::from_millis(300);
        let checkpoint = send_spec.checkpoint_dir.clone();

        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;
        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            let limits = engine_limits();
            let _ = stream.recv_frame(&limits).await; // sender Hello
            stream
                .send_frame(&hello_frame("stall", &bundle, RECV_CAPABILITIES))
                .await
                .unwrap();
            let _ = stream.recv_frame(&limits).await; // Offer
                                                      // Silent from here; hold the stream open so the sender observes
                                                      // silence rather than a close.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let err = send(send_spec, &noop)
            .await
            .expect_err("silent peer must time out");
        assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
        assert!(
            !FileCheckpointStore::open(&checkpoint)
                .unwrap()
                .list()
                .unwrap()
                .is_empty(),
            "checkpoint must survive the idle abort"
        );
        fake.abort();
    }

    /// The sender stalls right after Hello: the receiver must fail with
    /// IdleTimeout while waiting for the Offer.
    #[tokio::test]
    async fn idle_timeout_on_receive_when_sender_stalls() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_millis(300);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        stream
            .send_frame(&hello_frame("stall", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&engine_limits()).await; // receiver Hello

        let err = receiver
            .await
            .unwrap()
            .expect_err("stalled sender must time out");
        assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
    }

    /// The receiver sends Pause mid-transfer: the sender stops issuing
    /// chunks until Resume, then finishes the job.
    #[tokio::test]
    async fn pause_holds_sender_until_resume() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("pause.bin"), &payload);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1); // 8 chunks
        send_spec.in_flight_window = 1;
        send_spec.idle_timeout = Duration::from_secs(20);
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
            let limits = engine_limits();
            let (file_index, chunk_index) = match stream.recv_frame(&limits).await.unwrap().message
            {
                Message::Chunk {
                    file_index,
                    chunk_index,
                    ..
                } => (file_index, chunk_index),
                other => panic!("expected Chunk, got {other:?}"),
            };
            // Pause, ack the in-flight chunk, then hold.
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Control {
                        job_id,
                        control: Control::Pause,
                    },
                })
                .await
                .unwrap();
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck {
                        job_id,
                        file_index,
                        chunk_index,
                    },
                })
                .await
                .unwrap();
            let leaked =
                tokio::time::timeout(Duration::from_millis(800), stream.recv_frame(&limits)).await;
            assert!(leaked.is_err(), "sender sent a chunk while paused");
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Control {
                        job_id,
                        control: Control::Resume,
                    },
                })
                .await
                .unwrap();
            1 + fake_ack_until_finalize(&mut stream, job_id).await
        });

        let events: Arc<std::sync::Mutex<Vec<Progress>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&events);
        let progress = move |event: Progress| log.lock().unwrap().push(event);
        let report = send(send_spec, &progress)
            .await
            .expect("send completes after Resume");
        let acked = fake.await.unwrap();
        assert_eq!(acked, 8, "all chunks must be acked after resume");
        assert_eq!(report.files, 1);

        let events = events.lock().unwrap();
        let paused = events
            .iter()
            .position(|e| matches!(e, Progress::Paused))
            .expect("Paused progress event");
        let resumed = events
            .iter()
            .position(|e| matches!(e, Progress::Resumed))
            .expect("Resumed progress event");
        let sent_while_paused = events[paused..=resumed]
            .iter()
            .filter(|e| matches!(e, Progress::ChunkSent { .. }))
            .count();
        assert_eq!(
            sent_while_paused, 0,
            "no chunk may be sent while paused (events: {events:?})"
        );
    }

    /// The sender opts into multistream but the peer does not advertise the
    /// capability: the transfer must run over the frozen single-stream path.
    #[tokio::test]
    async fn multistream_sender_falls_back_when_peer_lacks_capability() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
        write(&src.path().join("fallback.bin"), &payload);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
        send_spec.multistream = true;
        send_spec.in_flight_window = 2;
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            // A v1-only receiver: no CAP_MULTISTREAM bit in its Hello.
            let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
            fake_ack_until_finalize(&mut stream, job_id).await
        });
        let report = send(send_spec, &noop)
            .await
            .expect("fallback send completes");
        assert_eq!(fake.await.unwrap(), 8);
        assert_eq!(report.files, 1);
    }

    /// Both sides advertise CAP_MULTISTREAM and the sender opts in: every
    /// file rides its own stream, interleaved, and the result is identical.
    #[tokio::test]
    async fn multistream_loopback_transfer_is_byte_identical() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"hello ak");
        let deep: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
        write(&src.path().join("nested/deep/b.bin"), &deep);
        let third: Vec<u8> = (0..20_000u32).map(|i| (i % 199) as u8).collect();
        write(&src.path().join("nested/c.bin"), &third);
        fs::create_dir_all(src.path().join("empty/dir")).unwrap();

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 1 KiB chunks
        send_spec.multistream = true;
        send_spec.in_flight_window = 4; // up to 4 concurrent file streams

        let sent: Arc<std::sync::Mutex<Vec<u32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bytes_seen: Arc<std::sync::Mutex<Vec<(u32, u64)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sender_log = Arc::clone(&sent);
        let sender_progress = move |event: Progress| {
            if let Progress::ChunkSent { file_index, .. } = event {
                sender_log.lock().unwrap().push(file_index);
            }
        };
        let recv_log = Arc::clone(&bytes_seen);
        let recv_progress = move |event: Progress| {
            if let Progress::BytesAcked {
                file_index,
                cumulative_bytes,
            } = event
            {
                recv_log
                    .lock()
                    .unwrap()
                    .push((file_index, cumulative_bytes));
            }
        };

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(async move { receive_with_addr(recv, &recv_progress, addr_tx).await });
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &sender_progress)
            .await
            .expect("multistream send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multistream receive completes");

        assert_eq!(report.files, 7); // 3 files + 4 directory entries
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
        assert_eq!(
            fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
            deep
        );
        assert_eq!(fs::read(dst.path().join("nested/c.bin")).unwrap(), third);
        assert!(dst.path().join("empty/dir").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (3, 8 + 40_000 + 20_000));

        // Streams ran in parallel: file indexes interleave in ChunkSent.
        // Manifest order: a.txt=0, empty=1, empty/dir=2, nested=3,
        // nested/c.bin=4 (20 chunks), nested/deep=5, nested/deep/b.bin=6 (40).
        let sent = sent.lock().unwrap();
        let file_b = sent.iter().filter(|f| **f == 6).count();
        assert_eq!(file_b, 40, "b.bin must send all 40 chunks");
        let first_b = sent.iter().position(|f| *f == 6).unwrap();
        let last_b = sent.iter().rposition(|f| *f == 6).unwrap();
        assert!(
            sent[first_b..=last_b].contains(&4),
            "file streams did not interleave: {sent:?}"
        );

        // Byte-level progress fired at checkpoint cadence, chunk-aligned.
        let seen = bytes_seen.lock().unwrap();
        assert!(!seen.is_empty(), "no BytesAcked events observed");
        assert!(
            seen.iter().all(|(_, b)| b % 1024 == 0),
            "unaligned: {seen:?}"
        );
        assert!(
            seen.iter()
                .map(|(_, b)| *b)
                .max()
                .is_some_and(|m| m >= 16 * 1024),
            "expected at least one cadence event past 16 chunks: {seen:?}"
        );
    }

    /// The sender vanishes after the manifest: the receiver fails, and no
    /// empty `.akpart` litter remains in the destination.
    #[tokio::test]
    async fn failed_receive_discards_empty_part_files() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(5);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        stream
            .send_frame(&hello_frame("ghost", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
        let data = b"hello";
        let digest: [u8; 32] = *blake3::hash(data).as_bytes();
        let entry = WireEntry {
            rel_path: "ghost.txt".to_owned(),
            is_dir: false,
            size: data.len() as u64,
            mtime_unix_ms: 0,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: digest,
            chunk_digests: vec![digest],
        };
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id: [1; 16],
                    root_name: "ghost".to_owned(),
                    file_count: 1,
                    total_bytes: data.len() as u64,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // Accept
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id: [1; 16],
                    page_index: 0,
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // ResumeRequest: sinks exist now
                                                           // Vanish before any chunk.
        drop(stream);
        channel.close(0, b"test done");
        drop(endpoint);

        let err = receiver
            .await
            .unwrap()
            .expect_err("receiver must fail after the sender vanished");
        assert!(
            matches!(err, EngineError::PeerClosed | EngineError::Stream(_)),
            "got {err:?}"
        );
        let parts: Vec<_> = fs::read_dir(dst.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .to_string_lossy()
                    .ends_with(ak_storage::PART_SUFFIX)
            })
            .collect();
        assert!(parts.is_empty(), "empty part files survived: {parts:?}");
    }

    /// Regression for the 4,097-file defect, now per frame: a ResumeRequest
    /// frame covering a full shard of `max_resume_entries` entries must
    /// decode on the sender under engine limits.
    #[test]
    fn engine_limits_fit_resume_requests_for_maximum_jobs() {
        let limits = engine_limits();
        assert_eq!(limits.max_resume_entries, ak_wire::MAX_RESUME_ENTRIES);
        // One frame with the worst-case per-frame entry count, announcing a
        // following frame.
        let files: Vec<ResumeEntry> = (0..limits.max_resume_entries as u32)
            .map(|file_index| ResumeEntry {
                file_index,
                verified_prefix_chunks: 0,
            })
            .collect();
        let mut buf = Vec::new();
        Frame {
            version: ak_wire::VERSION,
            message: Message::ResumeRequest {
                job_id: [7; 16],
                files,
                more: true,
            },
        }
        .encode(&mut buf);
        assert!(buf.len() < ak_wire::MAX_FRAME_BYTES);
        let decoded = Frame::decode(&buf, &limits).expect("max-size ResumeRequest decodes");
        match decoded.message {
            Message::ResumeRequest { files, more, .. } => {
                assert_eq!(files.len(), 65_535);
                assert!(more);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Sharded ResumeRequest exchange (single-frame job cap removed)
    // ------------------------------------------------------------------

    /// The shard function: every non-final frame carries more = true, no
    /// frame exceeds the cap, an empty answer yields exactly one empty final
    /// frame, and concatenation reproduces the input.
    #[test]
    fn resume_request_frames_shard_within_the_cap() {
        let cap = engine_limits().max_resume_entries as usize;
        for total in [0usize, 1, cap - 1, cap, cap + 1, 2 * cap + 7] {
            let files: Vec<ResumeEntry> = (0..total as u32)
                .map(|i| ResumeEntry {
                    file_index: i,
                    verified_prefix_chunks: u64::from(i) * 3,
                })
                .collect();
            let frames = resume_request_frames([9; 16], &files, cap);
            assert!(!frames.is_empty(), "total {total}: at least one frame");
            let mut reunited = Vec::new();
            for (n, message) in frames.iter().enumerate() {
                let Message::ResumeRequest {
                    job_id,
                    files: shard,
                    more,
                } = message
                else {
                    panic!("total {total}: frame {n} is not a ResumeRequest");
                };
                assert_eq!(*job_id, [9; 16], "total {total}: frame {n} job id");
                assert!(
                    shard.len() <= cap,
                    "total {total}: frame {n} holds {} > {cap}",
                    shard.len()
                );
                assert_eq!(
                    *more,
                    n + 1 < frames.len(),
                    "total {total}: frame {n} more flag"
                );
                reunited.extend_from_slice(shard);
            }
            assert_eq!(
                reunited, files,
                "total {total}: concatenation must reproduce the input"
            );
        }
    }

    /// A resume frame naming a different job aborts the exchange; the
    /// matching job id accumulates until a more = false frame ends it.
    #[test]
    fn resume_collector_validates_job_ids_and_completion() {
        let mut collector = ResumeCollector::new([1; 16]);
        assert!(
            !collector
                .push(
                    [1; 16],
                    vec![ResumeEntry {
                        file_index: 0,
                        verified_prefix_chunks: 1,
                    }],
                    true
                )
                .unwrap(),
            "more = true must not finish the exchange"
        );
        assert_eq!(collector.entries.len(), 1);
        assert!(matches!(
            collector.push([2; 16], Vec::new(), false),
            Err(EngineError::PeerError { .. })
        ));
        assert!(
            collector.push([1; 16], Vec::new(), false).unwrap(),
            "more = false completes the exchange"
        );
        assert_eq!(collector.entries.len(), 1);
    }

    /// 100,000-entry job, unit level (no socket): shard, push every frame
    /// through the real wire encode/decode, and fold the decoded frames into
    /// the sender's accumulator loop. Every entry must arrive in order and
    /// no frame may exceed the per-frame cap. Real 100k-file transfers are
    /// deliberately not exercised here (I/O cost); release-time manual runs
    /// own that boundary.
    #[test]
    fn one_hundred_thousand_resume_entries_arrive_sharded() {
        const TOTAL: usize = 100_000;
        let limits = engine_limits();
        let cap = usize::from(limits.max_resume_entries);
        let job_id = [0xA5; 16];
        let files: Vec<ResumeEntry> = (0..TOTAL as u32)
            .map(|i| ResumeEntry {
                file_index: i,
                verified_prefix_chunks: u64::from(i),
            })
            .collect();

        let frames = resume_request_frames(job_id, &files, cap);
        assert_eq!(
            frames.len(),
            TOTAL.div_ceil(cap),
            "100k entries need two shards at cap {cap}"
        );

        // Simulated sender receive loop: decode each frame, fold it in.
        let mut collector = ResumeCollector::new(job_id);
        let mut final_frame = false;
        for message in frames {
            let mut buf = Vec::new();
            Frame {
                version: ak_wire::VERSION,
                message,
            }
            .encode(&mut buf);
            assert!(
                buf.len() < ak_wire::MAX_FRAME_BYTES,
                "shard frame must fit the frame cap"
            );
            match Frame::decode(&buf, &limits)
                .expect("shard frame decodes under engine limits")
                .message
            {
                Message::ResumeRequest {
                    job_id: frame_job_id,
                    files: shard,
                    more,
                } => {
                    assert!(
                        shard.len() <= cap,
                        "single frame exceeded the per-frame cap"
                    );
                    if collector
                        .push(frame_job_id, shard, more)
                        .expect("frame accepted")
                    {
                        final_frame = true;
                        break;
                    }
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
        assert!(final_frame, "exchange must end with a more = false frame");
        assert_eq!(
            collector.entries, files,
            "all 100,000 entries must arrive in order"
        );
    }
}
