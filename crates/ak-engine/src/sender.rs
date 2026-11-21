//! The sending side: source set, ack window, single- and multi-stream send.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

fn sender_record(spec: &SendSpec, record_id: JobId, manifest: &Manifest) -> TaskRecord {
    TaskRecord {
        // The record key is the per-instance record id (see record_id_for);
        // the wire keeps speaking the content id in Offer and friends.
        job_id: record_id,
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
pub(super) struct SourceSet {
    sources: Vec<Arc<LocalSource>>,
    /// Display name of each root, in the same order as `sources`.
    names: Vec<String>,
    /// Merged manifest the wire protocol speaks.
    pub(super) manifest: Manifest,
    /// Merged entry index -> (root ordinal, per-root entry index). Synthesized
    /// entries for entirely empty roots carry `u32::MAX` as their local index;
    /// they are directories and are never read.
    index_map: Vec<(u32, u32)>,
    /// True when more than one root was merged.
    pub(super) multi: bool,
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
    pub(super) fn open(spec: &SendSpec) -> Result<Self, EngineError> {
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

    /// Re-check every root against its manifest. One stat per file, so it
    /// grows with the file count; the sender runs it on the blocking pool
    /// with the liveness signal going (see [`CAP_SENDER_KEEPALIVE`]).
    fn verify_stable(&self) -> Result<(), EngineError> {
        #[cfg(test)]
        if let Some(delay) = seams::source_check_delay(&self.names) {
            std::thread::sleep(delay);
        }
        for source in &self.sources {
            source
                .verify_stable()
                .map_err(|_| EngineError::SourceChanged)?;
        }
        Ok(())
    }

    /// Name used for the Offer and the job id: the single root's name, or
    /// `"multi"` when several roots were merged.
    pub(super) fn display_name(&self) -> &str {
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

/// Outcome of applying one ChunkAck against the sent-but-unacknowledged set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AckOutcome {
    /// The ack named a chunk we sent and had not been acknowledged yet.
    Confirmed,
    /// A duplicate of an already-confirmed chunk or a chunk we never sent:
    /// ignored and counted, never an error and never retiring an
    /// outstanding slot (review round 17, ACK idempotency).
    DuplicateOrUnknown,
}

/// Sent-but-unacknowledged chunk ids of one transfer (single stream) or one
/// file stream (multistream). Bounded by the in-flight window: ids leave the
/// set exactly when a genuine ack confirms them, so a peer replaying or
/// inventing acks can neither inflate nor fast-forward the window.
#[derive(Debug, Default)]
pub(super) struct AckLedger {
    pub(super) inflight: HashSet<(u32, u64)>,
    /// Duplicate/unknown acks seen so far (observability; asserted by tests).
    pub(super) duplicates: u64,
}

impl AckLedger {
    pub(super) fn track(&mut self, file_index: u32, chunk_index: u64) {
        self.inflight.insert((file_index, chunk_index));
    }

    pub(super) fn apply(&mut self, file_index: u32, chunk_index: u64) -> AckOutcome {
        if self.inflight.remove(&(file_index, chunk_index)) {
            AckOutcome::Confirmed
        } else {
            self.duplicates += 1;
            log::debug!(
                "ignoring duplicate or unknown ChunkAck (file {file_index}, chunk {chunk_index})"
            );
            AckOutcome::DuplicateOrUnknown
        }
    }
}

/// Sender-side ack window state, shared across drain calls of one transfer.
struct AckWindow {
    outstanding: u32,
    paused: bool,
    acks_since_progress: u64,
    /// Which chunks the outstanding count actually counts.
    ledger: AckLedger,
}

impl AckWindow {
    fn new() -> Self {
        AckWindow {
            outstanding: 0,
            paused: false,
            acks_since_progress: 0,
            ledger: AckLedger::default(),
        }
    }
}

/// Wait until the window has room again. While paused, no ack expectation is
/// enforced: the loop only returns after a Resume (or fails on Cancel/Error/
/// idle). Every genuine ack also feeds byte-level progress at checkpoint
/// cadence; duplicate or unknown acks are ignored and counted (see
/// [`AckLedger`]), never treated as errors and never retiring an outstanding
/// slot.
#[allow(clippy::too_many_arguments)]
async fn drain_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    job_id: JobId,
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
                        job_id: ack_job,
                        file_index,
                        chunk_index,
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("chunk ack names a different job"));
                }
                if window.ledger.apply(file_index, chunk_index) == AckOutcome::Confirmed {
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
            }
            Frame {
                message:
                    Message::Control {
                        job_id: control_job,
                        control,
                    },
                ..
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                match control {
                    Control::Pause => {
                        if !window.paused {
                            window.paused = true;
                            progress(Progress::Paused);
                        }
                    }
                    Control::Resume => {
                        if window.paused {
                            window.paused = false;
                            progress(Progress::Resumed);
                        }
                    }
                    Control::Cancel => return Err(EngineError::Cancelled),
                }
            }
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
    // Per-file progress goes into the record at once, onto disk at most
    // once per CHUNK_CHECKPOINT_INTERVAL (O-N2, review 2026-11-19).
    let mut last_save = Instant::now();
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
                        if last_save.elapsed() >= CHUNK_CHECKPOINT_INTERVAL {
                            record.updated_unix_ms = now_ms();
                            store.save(record).map_err(EngineError::Checkpoint)?;
                            last_save = Instant::now();
                        }
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
    let mut ledger = AckLedger::default();
    for chunk_index in skip..plan.chunk_count() {
        if paused {
            drain_file_acks(
                &mut rx,
                &limits,
                job_id,
                0,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &mut ledger,
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
        ledger.track(file_index, chunk_index);
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
                job_id,
                window - 1,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &mut ledger,
                &events,
            )
            .await?;
        }
    }
    drain_file_acks(
        &mut rx,
        &limits,
        job_id,
        0,
        idle,
        &mut outstanding,
        &mut paused,
        file_index,
        file_size,
        chunk_size,
        &mut acks_since_progress,
        &mut ledger,
        &events,
    )
    .await?;
    tx.finish()?;
    Ok(file_index)
}

/// Per-file variant of [`drain_acks`] for multistream tasks: progress goes
/// through an event channel because the driver owns the progress callback.
/// Same ack-idempotency rule: only chunks in this file's [`AckLedger`]
/// retire an outstanding slot; duplicates and unknowns are counted, not
/// errors.
#[allow(clippy::too_many_arguments)]
async fn drain_file_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    job_id: JobId,
    wanted: u32,
    idle: Duration,
    outstanding: &mut u32,
    paused: &mut bool,
    file_index: u32,
    file_size: u64,
    chunk_size: NonZeroU32,
    acks_since_progress: &mut u64,
    ledger: &mut AckLedger,
    events: &tokio::sync::mpsc::Sender<SendEvent>,
) -> Result<(), EngineError> {
    while *outstanding > wanted || *paused {
        match recv_within(rx, limits, idle).await? {
            Frame {
                message:
                    Message::ChunkAck {
                        job_id: ack_job,
                        chunk_index,
                        ..
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("chunk ack names a different job"));
                }
                if ledger.apply(file_index, chunk_index) == AckOutcome::Confirmed {
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
            }
            Frame {
                message:
                    Message::Control {
                        job_id: control_job,
                        control,
                    },
                ..
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                match control {
                    Control::Pause => {
                        if !*paused {
                            *paused = true;
                            if events.send(SendEvent::Paused).await.is_err() {
                                return Err(EngineError::Cancelled);
                            }
                        }
                    }
                    Control::Resume => {
                        if *paused {
                            *paused = false;
                            if events.send(SendEvent::Resumed).await.is_err() {
                                return Err(EngineError::Cancelled);
                            }
                        }
                    }
                    Control::Cancel => return Err(EngineError::Cancelled),
                }
            }
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
/// Source/manifest/checkpoint preparation shared by [`send`] and
/// [`send_on_channel`]: opens the sources, derives the content job id and
/// the per-instance record id, and persists the initial sender record
/// before anything connects.
struct PreparedSend {
    sources: Arc<SourceSet>,
    manifest: Manifest,
    root_name: String,
    job_id: JobId,
    record: TaskRecord,
    store: FileCheckpointStore,
}

fn prepare_send(spec: &SendSpec) -> Result<PreparedSend, EngineError> {
    let limits = engine_limits();
    // Engine-side Offer field validation (review round 17, defense in depth
    // with ak-wire's decode budget): a chunk size above the engine limit
    // would be refused by the receiver (and possibly already by the wire
    // decoder); refuse it here, before any connection is made, with a clear
    // local invalid-input error.
    if spec.chunk_size.get() > limits.max_chunk_bytes {
        return Err(EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "chunk_size {} bytes exceeds the engine limit of {} bytes",
                spec.chunk_size.get(),
                limits.max_chunk_bytes
            ),
        )));
    }
    let sources = Arc::new(SourceSet::open(spec)?);
    let manifest = sources.manifest.clone();
    let root_name = sources.display_name().to_owned();
    // `job_id` is the content identity the wire speaks; the checkpoint
    // record is keyed by the per-instance record id instead (see
    // `record_id_for`), so this same source sent to another peer (or this
    // machine also receiving through the same checkpoint directory) keeps a
    // separate record instead of overwriting this one.
    let job_id = job_id_for(&root_name, &manifest);
    let record_id = record_id_for(&job_id, &spec.pinned, Direction::Send, &spec.source_root);

    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let record = sender_record(spec, record_id, &manifest);
    store.save(&record).map_err(EngineError::Checkpoint)?;
    Ok(PreparedSend {
        sources,
        manifest,
        root_name,
        job_id,
        record,
        store,
    })
}

pub async fn send(
    spec: SendSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let prepared = prepare_send(&spec)?;
    progress(Progress::Connecting);
    // CR-P1-05: an adopted socket (same-socket STUN/probe lifecycle, see
    // [`SendSpec::adopt_socket`]) carries the caller's NAT mapping into
    // the QUIC dial; no adoption keeps the historical self-bind. The Arc
    // keeps the OS socket alive across retry passes; this send hands
    // quinn one duplicated handle of it (same local port).
    let endpoint = match &spec.adopted_socket {
        Some(socket) => {
            let handle = socket.try_clone().map_err(EngineError::from)?;
            DirectEndpoint::dial_with_socket(handle, &spec.identity).await?
        }
        None => DirectEndpoint::bind(spec.bind, &spec.identity).await?,
    };
    let channel = connect_bounded(&endpoint, spec.peer, &spec.pinned, spec.idle_timeout).await?;
    // Policy gate (protocol step 0): a refused path closes the connection
    // before any stream is opened or frame sent.
    gate_channel(&spec.policy, &channel)?;
    // Legacy path (wave 21 split): self-connected channel, admission absent
    // — no CAP_SCHEDULER bit, no TaskOpen. Wire-identical to the frozen v1
    // behavior; see [`send_on_channel`] for the scheduler-admission form.
    send_channel_body(channel, &spec, prepared, started, None, progress).await
}

/// Wave-21 on-channel send entry (scheduler.md §6), hardened wave 23: run
/// the whole send protocol on an already-connected [`DirectChannel`] the
/// runtime handed in. Differences from [`send`], and only these:
///
/// * **pinned-identity re-check (wave 23, review P1-4), the first thing
///   this entry does**: the channel's verified TLS peer fingerprint must
///   equal `spec.pinned` — the identity condition the legacy [`send`]
///   establishes itself at connect time. A mismatch returns
///   [`EngineError::PeerMismatch`] and closes the connection with
///   ``PEER_MISMATCH_CLOSE_CODE`` before any protocol byte moves (not even
///   the sources are scanned);
/// * no endpoint bind/connect here (the runtime owns the connection);
/// * the Hello advertises `CAP_SCHEDULER` and, when the peer does too
///   ([`ak_wire::scheduler_frames_allowed`]), the scheduler admission
///   handshake runs before the frozen Offer→FinalAck path:
///   `TaskOpen(task_id, group, group_registry, pairing, permit)` → wait
///   `Admission` → on grant send `AdmissionAck` and continue; on deny
///   return [`EngineError::AdmissionDenied`] with the closed wire reason
///   (`StalePermit` resubmits a fresh TaskOpen under a bounded budget);
/// * a peer without `CAP_SCHEDULER` fails immediately with
///   `PROTOCOL_UNSUPPORTED` — there is no silent fallback to unadmitted
///   transfer, because the caller explicitly asked for a scheduled task.
///
/// The policy gate still runs on the handed-in channel before any protocol
/// byte moves, and staging is keyed by the peer's admission decision (the
/// receiver stages under this `task_id`, not the sender).
pub async fn send_on_channel(
    channel: DirectChannel,
    spec: SendSpec,
    admission_ctx: AdmissionCtx,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    // Pinned-identity re-check (wave 23, review P1-4) — before anything
    // else, including the source scan: the runtime owns the connection,
    // so the engine re-derives the identity guarantee itself.
    if channel.peer_fingerprint() != &spec.pinned {
        let mismatch = EngineError::PeerMismatch {
            actual: channel.peer_fingerprint().to_hex(),
            expected: spec.pinned.to_hex(),
        };
        channel.close(
            PEER_MISMATCH_CLOSE_CODE,
            b"channel peer is not the pinned fingerprint",
        );
        return Err(mismatch);
    }
    let started = Instant::now();
    let prepared = prepare_send(&spec)?;
    // Policy gate: same step-0 refusal for a runtime-provided channel.
    gate_channel(&spec.policy, &channel)?;
    send_channel_body(
        channel,
        &spec,
        prepared,
        started,
        Some(&admission_ctx),
        progress,
    )
    .await
}

/// The protocol body shared by [`send`] and [`send_on_channel`]: everything
/// from the Connected progress event through Hello, (optional) admission,
/// Offer→Accept, manifest, resume, chunks (single-stream or multistream),
/// Finalize/FinalAck, and the report. `admission = None` keeps the frozen
/// no-scheduler wire behavior byte for byte.
#[allow(clippy::too_many_arguments)]
async fn send_channel_body(
    channel: DirectChannel,
    spec: &SendSpec,
    prepared: PreparedSend,
    started: Instant,
    admission: Option<&AdmissionCtx>,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let limits = engine_limits();
    let PreparedSend {
        sources,
        manifest,
        root_name,
        job_id,
        mut record,
        store,
    } = prepared;
    let channel = Arc::new(channel);
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();

    let mut capabilities = SEND_CAPABILITIES;
    if spec.multistream {
        capabilities |= CAP_MULTISTREAM;
    }
    if admission.is_some() {
        capabilities |= CAP_SCHEDULER;
    }
    tx.send_frame(&hello_frame(
        &spec.device_name,
        &spec.identity,
        capabilities,
    ))
    .await?;
    let peer_caps = expect_hello(&mut rx, &limits, spec.idle_timeout).await?;
    if let Some(ctx) = admission {
        // Capability-negotiated gate (scheduler.md §6): admission frames
        // only flow when BOTH Hellos carry CAP_SCHEDULER. A peer without
        // the bit cannot silently downgrade a scheduled task to the
        // unadmitted path — the sender asked for scheduling, so it fails
        // loudly with the stable protocol-unsupported code.
        if !scheduler_frames_allowed(capabilities, peer_caps) {
            return Err(EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                message: "peer does not advertise CAP_SCHEDULER; \
                          scheduled task admission cannot run"
                    .into(),
            });
        }
        admission_handshake_sender(&mut tx, &mut rx, &limits, spec.idle_timeout, ctx).await?;
    }
    let multistream = spec.multistream && (peer_caps & CAP_MULTISTREAM) != 0;
    let sender_keepalive = sender_keepalive_allowed(capabilities, peer_caps);

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
            message: Message::Accept { job_id: ack_job },
            ..
        } => {
            if ack_job != job_id {
                return Err(protocol_error("accept names a different job"));
            }
        }
        Frame {
            message:
                Message::Reject {
                    job_id: reject_job,
                    reason,
                },
            ..
        } => {
            if reject_job != job_id {
                return Err(protocol_error("reject names a different job"));
            }
            return Err(EngineError::Rejected(reason));
        }
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
    // one carries more = false, and every frame must name this job. The
    // collector also cross-validates every entry against this manifest
    // (index in range and not a directory, no repeats, prefix within the
    // file's real chunk count, total budget = manifest file count).
    let mut resume = ResumeCollector::new(job_id, &manifest, &limits);
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
        // See the multistream path: progress into the record per file, onto
        // disk at most once per CHUNK_CHECKPOINT_INTERVAL (O-N2).
        let mut last_save = Instant::now();
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
                        job_id,
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
                acks.ledger.track(file_index as u32, chunk_index);
                progress(Progress::ChunkSent {
                    file_index: file_index as u32,
                    chunk_index,
                });
                if acks.outstanding >= window_size {
                    drain_acks(
                        &mut rx,
                        &limits,
                        job_id,
                        window_size - 1,
                        spec.idle_timeout,
                        &mut acks,
                        &manifest,
                        progress,
                    )
                    .await?;
                }
            }
            // Keep checkpointed progress roughly current per file. The
            // record is rewritten whole on every save and grows with the
            // file count, so saving after every file cost O(files²) bytes
            // — most of a many-file job's chunk phase (O-N2).
            record.files[file_index].verified_prefix_chunks =
                manifest.entries[file_index].chunk_digests.len() as u64;
            if last_save.elapsed() >= CHUNK_CHECKPOINT_INTERVAL {
                record.updated_unix_ms = now_ms();
                store.save(&record).map_err(EngineError::Checkpoint)?;
                last_save = Instant::now();
            }
        }
        drain_acks(
            &mut rx,
            &limits,
            job_id,
            0,
            spec.idle_timeout,
            &mut acks,
            &manifest,
            progress,
        )
        .await?;
    }

    // Final integrity gate on the sources, then Finalize. The check stats
    // every source file, so on a job with hundreds of thousands of files
    // it alone can outlast the receiver's per-frame idle budget while the
    // receiver waits for Finalize (BACKLOG P3-02, batch S review). It runs
    // on the blocking pool; a receiver that negotiated
    // CAP_SENDER_KEEPALIVE gets a no-op Control::Resume every liveness
    // interval meanwhile, any other receiver sees the frozen silence.
    let keepalive = sender_keepalive.then_some(Frame {
        version: ak_wire::VERSION,
        message: Message::Control {
            job_id,
            control: Control::Resume,
        },
    });
    let mut last_signal = Instant::now();
    let checked = Arc::clone(&sources);
    offload_with_liveness(
        &mut tx,
        keepalive.as_ref(),
        liveness_interval(spec.idle_timeout),
        &mut last_signal,
        move || checked.verify_stable(),
    )
    .await??;
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Finalize { job_id },
    })
    .await?;
    // Pause/Resume is tolerated at the gate; only the verdict matters here.
    let verified = loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::FinalAck {
                        job_id: ack_job,
                        verified,
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("final ack names a different job"));
                }
                break verified;
            }
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

    // End-of-transfer transport-stats snapshot (see `ChannelStatsSink`):
    // taken after the last payload byte was acked (FinalAck above) and
    // before the report leaves the engine, so `cwnd` reads the
    // end-of-transfer window. Informational only; skipped when no sink.
    if let Some(sink) = &spec.stats_sink {
        sink.notify(&channel.stats());
    }

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
