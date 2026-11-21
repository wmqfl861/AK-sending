//! Single-stream chunk receive and the chunk-phase STORAGE_FULL report.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

/// How to hold the connection open until the sender observed the FinalAck.
pub(super) enum TailHold {
    /// Single-stream mode: drain the control stream receive half.
    Stream(DirectRecvStream),
    /// Multistream mode: the control-reader task drains; its exit closes
    /// the event channel, which is what we await.
    Events(tokio::sync::mpsc::Receiver<StreamEvent>),
}

/// Text of the mid-transfer storage-full Error frame (fixed: it reaches the
/// peer, so it carries no local path or size).
pub(super) const STORAGE_FULL_MESSAGE: &str = "receiver ran out of disk space during the transfer";

/// Upper bound on how long a receive whose disk filled up mid-transfer
/// holds the connection after telling the sender (batch S review; also
/// capped by the idle budget). Meanwhile it keeps reading and dropping
/// the sender's in-flight frames, so a sender blocked by flow control gets
/// to its next ack read and finds the Error frame. The hold ends earlier
/// when the sender closes, or goes quiet for one liveness interval: a
/// sender stops sending once it has read the frame, and a sender process
/// that exits right away may never send a close.
pub(super) const STORAGE_FULL_DRAIN_WAIT: Duration = Duration::from_secs(10);

/// How long one storage-full drain read may wait: one liveness interval.
/// A sender that has read the Error frame stops sending, and a sender
/// process that exits right away may never send a close, so silence for
/// this long ends the hold too (see [`STORAGE_FULL_DRAIN_WAIT`]).
fn storage_full_quiet(idle: Duration) -> Duration {
    liveness_interval(idle)
}

/// Whether a receive failure is the destination running out of space (or
/// out of quota): the case the receiver reports to the sender as
/// `Error{StorageFull}` during the chunk phase.
pub(super) fn is_storage_full(error: &EngineError) -> bool {
    match error {
        EngineError::Io(error) | EngineError::Checkpoint(error) => matches!(
            error.kind(),
            std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
        ),
        _ => false,
    }
}

fn storage_full_frame() -> Frame {
    Frame {
        version: ak_wire::VERSION,
        message: Message::Error {
            code: ErrorCode::StorageFull,
            message: STORAGE_FULL_MESSAGE.into(),
        },
    }
}

/// Store one received chunk in its part file. Under the unit tests a seam
/// can make it fail the way a full disk does (see `seams`).
pub(super) fn write_chunk_into(
    sink: &mut ak_storage::LocalFileSink,
    job_id: &JobId,
    chunk_index: u64,
    data: &[u8],
) -> std::io::Result<()> {
    #[cfg(test)]
    seams::chunk_write(job_id)?;
    #[cfg(not(test))]
    let _ = job_id;
    sink.write_chunk(chunk_index, data)
}

/// Single-stream chunk phase failing on a local error (batch S review,
/// BACKLOG P3-02): when the disk filled up, tell the sender with an
/// `Error{StorageFull}` frame on the ack stream — the sender reads that
/// stream for its acks, and every engine version maps the frame to
/// `PeerError` — then hold the connection, reading and dropping the
/// sender's in-flight frames, until the sender closes, goes quiet
/// ([`storage_full_quiet`]) or the bound passes ([`STORAGE_FULL_DRAIN_WAIT`]).
/// Any other error is returned at once, as before. Returns `error`
/// unchanged either way.
async fn tell_storage_full_single(
    error: EngineError,
    tx: &mut DirectSendStream,
    rx: &mut DirectRecvStream,
    idle: Duration,
) -> EngineError {
    if is_storage_full(&error) && tx.send_frame(&storage_full_frame()).await.is_ok() {
        let _ = tx.finish();
        let limits = engine_limits();
        let quiet = storage_full_quiet(idle);
        // Quinn reads are not cancel-safe; the stream is never read again,
        // so a frame cut off by the quiet timer is moot.
        let _ = tokio::time::timeout(STORAGE_FULL_DRAIN_WAIT.min(idle), async {
            while let Ok(Ok(_)) = tokio::time::timeout(quiet, rx.recv_frame(&limits)).await {}
        })
        .await;
    }
    error
}

/// Multistream counterpart of [`tell_storage_full_single`], run by the
/// failing file's stream task: the frame goes on that file's own stream,
/// where the sender's task for the file reads its acks. The failure goes
/// to the driver at once ([`StreamEvent::DiskFull`]), so nothing the
/// sender's reaction sets off can be taken for the cause; the driver then
/// waits for this task's drain to end before it returns and the
/// connection closes. Draining keeps the sender's writes on this stream
/// from sticking on flow control before it reads the frame.
pub(super) async fn tell_storage_full_on_stream(
    stream: &mut DirectStream,
    error: EngineError,
    idle: Duration,
    events: &tokio::sync::mpsc::Sender<StreamEvent>,
) {
    let told = stream.send_frame(&storage_full_frame()).await.is_ok();
    let _ = stream.finish_send();
    let (drained_tx, drained) = tokio::sync::oneshot::channel();
    let _ = events.send(StreamEvent::DiskFull { error, drained }).await;
    if told {
        let limits = engine_limits();
        let quiet = storage_full_quiet(idle);
        // Same cancel-safety note as in tell_storage_full_single.
        let _ = tokio::time::timeout(STORAGE_FULL_DRAIN_WAIT.min(idle), async {
            while let Ok(Ok(_)) = tokio::time::timeout(quiet, stream.recv_frame(&limits)).await {}
        })
        .await;
    }
    drop(drained_tx);
}

/// A full disk already reported in the driver's event channel (see the
/// accept branch of [`multistream_receive`]); other pending events are
/// dropped, the receive is ending anyway.
pub(super) fn pending_storage_full(
    events: &mut tokio::sync::mpsc::Receiver<StreamEvent>,
) -> Option<EngineError> {
    while let Ok(event) = events.try_recv() {
        if let StreamEvent::DiskFull { error, .. } = event {
            return Some(error);
        }
    }
    None
}

/// Frozen v1 chunk loop: one stream, chunks one way, windowed acks the
/// other way. Returns the receive half so the caller can hold the
/// connection for the FinalAck.
#[allow(clippy::too_many_arguments)]
pub(super) async fn receive_single_stream(
    mut rx: DirectRecvStream,
    tx: &mut DirectSendStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    sender_keepalive: bool,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<DirectRecvStream, EngineError> {
    let limits = engine_limits();
    let mut cadence = ChunkCheckpointCadence::new(Instant::now());
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
                job_id: chunk_job,
                file_index,
                chunk_index,
                data,
            } => {
                if chunk_job != job_id {
                    return Err(protocol_error("chunk names a different job"));
                }
                let sink = sinks
                    .get_mut(file_index as usize)
                    .and_then(Option::as_mut)
                    .ok_or(EngineError::PeerClosed)?;
                if let Err(error) = write_chunk_into(sink, &job_id, chunk_index, &data) {
                    return Err(tell_storage_full_single(
                        EngineError::Io(error),
                        tx,
                        &mut rx,
                        idle,
                    )
                    .await);
                }
                if let Some(slot) = record.files.get_mut(file_index as usize) {
                    slot.verified_prefix_chunks = sink.verified_prefix_chunks();
                }
                // Durability contract (see module docs): this ack confirms a
                // verified digest written to the part file's page cache —
                // not a durable write. Durability lands at the checkpoint
                // cadence below and at finish()'s sync_all + rename.
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
                let tick = cadence.on_ack(Instant::now());
                if tick.save {
                    record.updated_unix_ms = now_ms();
                    if let Err(error) = store.save(record) {
                        return Err(tell_storage_full_single(
                            EngineError::Checkpoint(error),
                            tx,
                            &mut rx,
                            idle,
                        )
                        .await);
                    }
                }
                if tick.progress {
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
            Message::Finalize {
                job_id: finalize_job,
            } => {
                if finalize_job != job_id {
                    return Err(protocol_error("finalize names a different job"));
                }
                break 'transfer;
            }
            Message::Control {
                job_id: control_job,
                control: Control::Cancel,
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                return Err(EngineError::Cancelled);
            }
            // The sender's liveness signal (both Hellos carried
            // CAP_SENDER_KEEPALIVE): a no-op whose arrival already restarted
            // this loop's idle wait. Without the bit it stays unexpected.
            Message::Control {
                job_id: control_job,
                control: Control::Resume,
            } if sender_keepalive => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
            }
            Message::Error { code, message } => {
                return Err(EngineError::PeerError { code, message })
            }
            other => return Err(unexpected(other)),
        }
    }
    Ok(rx)
}
