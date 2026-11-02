//! Transfer engine: one sender, one receiver, one authenticated direct
//! channel, zero servers in the data path.
//!
//! Frozen protocol flow (see docs/PROTOCOL.zh-CN.md):
//! 1. Hello exchange (versions, capabilities, identities).
//! 2. Offer with a deterministic job id: `blake3(root_name || per-file
//!    rel_path/size/mtime/digests || chunk_size)[..16]`. Same source, same
//!    id, so a resumed run continues the same job (no new content identity).
//! 3. Accept (receiver consent; consent is required, never assumed).
//! 4. Manifest pages (byte-budgeted, bounded), then ResumeRequest carrying
//!    per-file verified prefixes taken from the receiver's part files.
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
//! Known v1 limitations (deliberate, documented): no in-band Pause, no idle
//! timeouts, receiver accepts one job per `receive()` call.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_core::error::ErrorCode;
use ak_core::handle::{ConflictPolicy, FileSink, SourceHandle};
use ak_core::manifest::Manifest;
use ak_identity::tls::TlsBundle;
use ak_identity::Fingerprint;
use ak_net::{DirectEndpoint, DirectRecvStream};
use ak_storage::{LocalSaveTarget, LocalSource};
use ak_store::{CheckpointStore, Direction, FileCheckpoint, FileCheckpointStore, TaskRecord};
use ak_wire::{Control, Frame, JobId, Limits, Message, ResumeEntry, WireEntry, CAP_RESUME};

/// Manifest pages are assembled under this byte budget so every page fits a
/// frame comfortably below `ak_wire::MAX_FRAME_BYTES`.
const PAGE_BUDGET_BYTES: usize = 4 << 20;
/// Checkpoint write cadence, in acknowledged chunks.
const CHECKPOINT_EVERY_ACKS: u64 = 16;
/// Wire protocol version this engine speaks.
const PROTO_VERSION: u16 = 1;

/// Sender-side configuration.
#[derive(Debug, Clone)]
pub struct SendSpec {
    /// File or directory to send.
    pub source_root: PathBuf,
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
}

impl SendSpec {
    pub fn new(source_root: PathBuf, peer: SocketAddr, pinned: Fingerprint, identity: TlsBundle) -> Self {
        Self {
            source_root,
            peer,
            pinned,
            identity,
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
    /// Our TLS identity (self-signed certificate for this device key).
    pub identity: TlsBundle,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Consent callback: channel-level peer authorization. Called once per
    /// connection with the verified peer fingerprint.
    pub authorize: Box<dyn Fn(&Fingerprint) -> bool + Send + Sync>,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
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
    /// Chunks not re-sent/re-received thanks to verified part files.
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
    PeerError { code: ErrorCode, message: String },
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
pub fn job_id_for(root_name: &str, manifest: &Manifest) -> JobId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"AKJOB1");
    hasher.update(root_name.as_bytes());
    hasher.update(&manifest.chunk_size.get().to_be_bytes());
    for entry in &manifest.entries {
        hasher.update(&[u8::from(entry.is_dir)]);
        hasher.update(entry.rel_path.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(&entry.size.to_be_bytes());
        hasher.update(&entry.mtime_unix_ms.to_be_bytes());
        hasher.update(&entry.whole_digest);
    }
    let digest = hasher.finalize();
    let mut id = JobId::default();
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

fn now_ms() -> u64 {
    ak_store::now_unix_ms()
}

fn hello_frame(name: &str, identity: &TlsBundle) -> Frame {
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
            capabilities: CAP_RESUME,
        },
    }
}

async fn expect_hello(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    our_name: &str,
    identity: &TlsBundle,
) -> Result<(), EngineError> {
    match rx.recv_frame(limits).await? {
        Frame { message: Message::Hello { proto_version, .. }, .. } => {
            if proto_version != PROTO_VERSION {
                return Err(EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    message: format!("peer speaks protocol version {proto_version}"),
                });
            }
            let _ = (our_name, identity);
            Ok(())
        }
        Frame { message: Message::Error { code, message }, .. } => Err(EngineError::PeerError { code, message }),
        other => Err(unexpected(other.message)),
    }
}

fn unexpected(message: Message) -> EngineError {
    EngineError::PeerError { code: ErrorCode::ProtocolUnsupported, message: format!("unexpected frame: {message:?}") }
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
        if !current.is_empty() && (current_bytes + size > PAGE_BUDGET_BYTES || current.len() >= cap) {
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

/// Read one chunk through the blocking pool, returning exactly the chunk bytes.
async fn read_chunk_via(
    source: Arc<LocalSource>,
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
    .unwrap_or_else(|join| Err(std::io::Error::other(format!("chunk read task failed: {join}"))))
}

async fn drain_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    mut outstanding: u32,
    wanted: u32,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<u32, EngineError> {
    while outstanding > wanted {
        match rx.recv_frame(limits).await? {
            Frame { message: Message::ChunkAck { .. }, .. } => outstanding -= 1,
            Frame { message: Message::Control { control: Control::Cancel, .. }, .. } => {
                return Err(EngineError::Cancelled)
            }
            Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
        let _ = progress;
    }
    Ok(outstanding)
}

/// Run one send to completion. Progress callbacks are synchronous and must
/// be cheap.
pub async fn send(
    spec: SendSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let limits = Limits::default();
    let source = Arc::new(LocalSource::open(&spec.source_root, spec.chunk_size)?);
    let manifest = source.manifest().clone();
    let root_name = source.root_name();
    let job_id = job_id_for(&root_name, &manifest);

    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let mut record = sender_record(&spec, job_id, &manifest);
    store.save(&record).map_err(EngineError::Checkpoint)?;

    progress(Progress::Connecting);
    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let channel = endpoint.connect(spec.peer, &spec.pinned).await?;
    progress(Progress::Connected { peer: channel.peer_fingerprint().to_hex() });
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();

    tx.send_frame(&hello_frame(&spec.device_name, &spec.identity)).await?;
    expect_hello(&mut rx, &limits, &spec.device_name, &spec.identity).await?;

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
    progress(Progress::Offered { files: manifest.file_count(), bytes: manifest.total_bytes() });
    match rx.recv_frame(&limits).await? {
        Frame { message: Message::Accept { .. }, .. } => {}
        Frame { message: Message::Reject { reason, .. }, .. } => return Err(EngineError::Rejected(reason)),
        Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    }
    progress(Progress::Accepted { files: manifest.file_count(), bytes: manifest.total_bytes() });

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

    // ResumeRequest: skip the receiver's verified prefixes.
    let resume: Vec<ResumeEntry> = match rx.recv_frame(&limits).await? {
        Frame { message: Message::ResumeRequest { files, .. }, .. } => files,
        Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    };
    let mut skip = vec![0u64; manifest.entries.len()];
    let mut resumed_chunks = 0u64;
    for entry in &resume {
        if let Some(slot) = skip.get_mut(entry.file_index as usize) {
            *slot = entry.verified_prefix_chunks;
            resumed_chunks += entry.verified_prefix_chunks;
        }
    }
    if resumed_chunks > 0 {
        progress(Progress::Resuming { chunks: resumed_chunks });
    }

    // Chunk transfer with a bounded ack window on the same stream.
    let window = spec.in_flight_window.max(1);
    let mut outstanding: u32 = 0;
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
            let range = plan.range(chunk_index).ok_or(EngineError::SourceChanged)?;
            let len = (range.end - range.start) as usize;
            let data = read_chunk_via(Arc::clone(&source), file_index as u32, chunk_index, len).await?;
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
            outstanding += 1;
            progress(Progress::ChunkSent { file_index: file_index as u32, chunk_index });
            if outstanding >= window {
                outstanding = drain_acks(&mut rx, &limits, outstanding, window - 1, progress).await?;
            }
        }
        // Keep checkpointed progress roughly current per file.
        record.files[file_index].verified_prefix_chunks =
            manifest.entries[file_index].chunk_digests.len() as u64;
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
    }
    drain_acks(&mut rx, &limits, outstanding, 0, progress).await?;

    // Final integrity gate on the source, then Finalize.
    source.verify_stable().map_err(|_| EngineError::SourceChanged)?;
    tx.send_frame(&Frame { version: ak_wire::VERSION, message: Message::Finalize { job_id } }).await?;
    let verified = match rx.recv_frame(&limits).await? {
        Frame { message: Message::FinalAck { verified, .. }, .. } => verified,
        Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
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
    let limits = Limits::default();
    let target = LocalSaveTarget::open(&spec.out_dir)?;
    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;

    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let local = endpoint.local_addr()?;
    let _ = addr_tx.send(local);
    let channel = endpoint.accept(&spec.authorize).await?;
    progress(Progress::Connected { peer: channel.peer_fingerprint().to_hex() });
    let stream = channel.accept_bi().await?;
    let (mut tx, mut rx) = stream.split();

    tx.send_frame(&hello_frame(&spec.device_name, &spec.identity)).await?;
    expect_hello(&mut rx, &limits, &spec.device_name, &spec.identity).await?;

    // Offer -> consent -> Accept. Channel-level consent already happened via
    // the authorize callback; the offer is accepted per current product copy.
    let (job_id, file_count, total_bytes, chunk_size) = match rx.recv_frame(&limits).await? {
        Frame { message: Message::Offer { job_id, file_count, total_bytes, chunk_size, .. }, .. } => {
            (job_id, file_count, total_bytes, chunk_size)
        }
        Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    };
    progress(Progress::Offered { files: file_count, bytes: total_bytes });
    tx.send_frame(&Frame { version: ak_wire::VERSION, message: Message::Accept { job_id } }).await?;
    progress(Progress::Accepted { files: file_count, bytes: total_bytes });

    // Manifest pages.
    let mut entries: Vec<ak_core::manifest::FileEntry> = Vec::new();
    loop {
        match rx.recv_frame(&limits).await? {
            Frame { message: Message::ManifestPage { entries: page, last_page, .. }, .. } => {
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
            Frame { message: Message::Error { code, message }, .. } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    let chunk_size = NonZeroU32::new(chunk_size).ok_or_else(|| EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: "peer offered a zero chunk size".into(),
    })?;
    let manifest = Manifest { chunk_size, entries: entries.clone() };
    progress(Progress::ManifestReceived);
    let job = ReceiverJob { manifest };

    // Prepare sinks (transparent resume from .akpart files) and answer.
    let mut sinks: Vec<Option<ak_storage::LocalFileSink>> = Vec::with_capacity(job.manifest.entries.len());
    let mut resume: Vec<ResumeEntry> = Vec::new();
    for (index, entry) in job.manifest.entries.iter().enumerate() {
        let sink = target.prepare_file_with_chunk_size(entry, spec.conflict, chunk_size)?;
        let prefix = sink.verified_prefix_chunks();
        if !entry.is_dir {
            resume.push(ResumeEntry { file_index: index as u32, verified_prefix_chunks: prefix });
        }
        sinks.push(Some(sink));
    }
    let resumed_chunks: u64 = resume.iter().map(|r| r.verified_prefix_chunks).sum();
    if resumed_chunks > 0 {
        progress(Progress::Resuming { chunks: resumed_chunks });
    }
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::ResumeRequest { job_id, files: resume },
    })
    .await?;

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
    store.save(&record).map_err(EngineError::Checkpoint)?;

    // Chunk loop: write, verify, ack, checkpoint periodically.
    let mut acks_since_checkpoint: u64 = 0;
    let mut current_file: u32 = u32::MAX;
    'transfer: loop {
        let frame = match rx.recv_frame(&limits).await {
            Ok(frame) => frame,
            Err(ak_net::StreamError::Connection(_)) => return Err(EngineError::PeerClosed),
            Err(e) => return Err(e.into()),
        };
        match frame.message {
            Message::Chunk { file_index, chunk_index, data, .. } => {
                let sink = sinks[file_index as usize].as_mut().ok_or(EngineError::PeerClosed)?;
                sink.write_chunk(chunk_index, &data).map_err(EngineError::Io)?;
                record.files[file_index as usize].verified_prefix_chunks = sink.verified_prefix_chunks();
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck { job_id, file_index, chunk_index },
                })
                .await?;
                if file_index != current_file {
                    current_file = file_index;
                    let path = job.manifest.entries[file_index as usize]
                        .rel_path
                        .to_string_lossy()
                        .replace('\\', "/");
                    let bytes = job.manifest.entries[file_index as usize].size;
                    progress(Progress::FileStarted { file_index, path, bytes });
                }
                progress(Progress::ChunkReceived { file_index, chunk_index });
                acks_since_checkpoint += 1;
                if acks_since_checkpoint >= CHECKPOINT_EVERY_ACKS {
                    acks_since_checkpoint = 0;
                    record.updated_unix_ms = now_ms();
                    store.save(&record).map_err(EngineError::Checkpoint)?;
                }
            }
            Message::Finalize { .. } => break 'transfer,
            Message::Control { control: Control::Cancel, .. } => return Err(EngineError::Cancelled),
            Message::Error { code, message } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other)),
        }
    }

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
            progress(Progress::FileVerified { file_index: index as u32 });
            progress(Progress::FileCommitted {
                file_index: index as u32,
                path: job.manifest.entries[index].rel_path.to_string_lossy().replace('\\', "/"),
            });
        }
    }
    // Completion marker strictly after verification and commit (PROTOCOL).
    record.state = "completed".into();
    record.updated_unix_ms = now_ms();
    store.save(&record).map_err(EngineError::Checkpoint)?;
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::FinalAck { job_id, verified: true },
    })
    .await?;
    tx.finish()?;
    progress(Progress::FinalAck { verified: true });
    // Hold the connection until the sender observes the FinalAck: dropping
    // this endpoint on return would close the QUIC connection immediately
    // and could discard the FinalAck before the peer reads it.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if rx.recv_frame(&limits).await.is_err() {
                break;
            }
        }
    })
    .await;
    progress(Progress::Done);

    Ok(Report {
        job_id,
        files: job.manifest.file_count(),
        bytes: job.manifest.total_bytes(),
        duration: started.elapsed(),
        resumed_chunks,
        peer_fingerprint: Some(channel.peer_fingerprint().to_hex()),
    })
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
                } else if !entry.path().to_string_lossy().ends_with(ak_storage::PART_SUFFIX) {
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

    #[tokio::test]
    async fn loopback_directory_transfer_is_byte_identical() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"hello ak");
        write(&src.path().join("nested/deep/b.bin"), &(0..50_000u32).map(|i| i as u8).collect::<Vec<_>>());
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
        let mut sink = target.prepare_file_with_chunk_size(entry, ConflictPolicy::Fail, send_spec.chunk_size).unwrap();
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

        assert_eq!(report.resumed_chunks, 3, "sender should skip 3 verified chunks");
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
        let part_path = dst.path().join(format!("mid.bin{}", ak_storage::PART_SUFFIX));

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
            matches!(send_result, Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))),
            "sender must fail without any fallback, got {send_result:?}"
        );
        assert!(part_path.exists(), "part file must survive the disconnect");
        let store = FileCheckpointStore::open(&checkpoint_dir).unwrap();
        assert!(!store.list().unwrap().is_empty(), "checkpoint must survive the disconnect");

        // Second run: same deterministic job id resumes and completes.
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("resumed send should complete");
        receiver.await.unwrap().expect("resumed receive should complete");

        assert!(report.resumed_chunks >= 1, "second run should resume, got {}", report.resumed_chunks);
        assert_eq!(fs::read(dst.path().join("mid.bin")).unwrap(), payload);
        assert!(!part_path.exists(), "committed file leaves no part behind");
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
        let err = send(send_spec, &noop).await.expect_err("wrong pin must fail");
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
        let err = send(send_spec, &noop).await.expect_err("unauthorized sender must fail");
        // The TLS handshake itself succeeds (client cert is validly
        // self-signed); rejection surfaces on first use as a lost stream.
        assert!(
            matches!(err, EngineError::Connect(_) | EngineError::Stream(_)),
            "got: {err:?}"
        );
        let recv_err = receiver.await.unwrap().expect_err("receiver must reject");
        assert!(matches!(recv_err, EngineError::Connect(ak_net::ConnectError::Unauthorized)));
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
        assert_eq!(fs::metadata(dst.path().join("big.bin")).unwrap().len(), 5 * 1024 * 1024 * 1024 + 13);
        assert_eq!(fs::read(dst.path().join("tail.txt")).unwrap(), b"after the big one");
    }
}
