//! Multi-stream receive: the per-stream tasks and their driver loop.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

/// Event from a receiver-side stream task back to the driver loop.
pub(super) enum StreamEvent {
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
    /// An accepted per-file stream delivered its first frame (read by the
    /// stream's own opener task, never inline in the driver loop — see
    /// [`multistream_receive`]); the driver validates it and starts the
    /// stream's receive task.
    Opened {
        stream: DirectStream,
        first: Frame,
    },
    /// A stream task or the control stream failed.
    Failed(EngineError),
    /// Finalize observed on the control stream.
    Finalize,
    Cancelled,
    /// The sender's liveness signal on the control stream
    /// (`CAP_SENDER_KEEPALIVE`): nothing to process, but like every event
    /// it refreshes the driver's idle budget.
    KeepAlive,
    /// A stream task's chunk write found the disk full and the task told
    /// the sender (see [`tell_storage_full_on_stream`]); `drained` resolves
    /// when the task stops draining that stream.
    DiskFull {
        error: EngineError,
        drained: tokio::sync::oneshot::Receiver<()>,
    },
}

/// Abort handles of a multistream receive's helper tasks (control reader,
/// control monitor, per-stream openers and receive tasks). Dropping the
/// guard aborts every task that is still running, so a multistream receive
/// that fails or is cancelled (its future dropped, e.g. the receiving task
/// aborted) cannot leave stream handles behind (M6, batch M 2026-11-17).
/// Those handles kept the QUIC connection — and its keep-alives — alive, so
/// the sender waited out its whole idle budget (`IdleTimeout(30s)`, the
/// property suite's historical Windows failure, reproduced on Linux) or,
/// blocked in a flow-controlled write, forever. A completed receive
/// disarms the guard: its tasks then run exactly as before (the control
/// reader keeps draining through the FinalAck hold).
#[derive(Default)]
struct ReceiveTaskGuard(Vec<tokio::task::AbortHandle>);

impl ReceiveTaskGuard {
    fn track(&mut self, task: &tokio::task::JoinHandle<()>) {
        self.0.push(task.abort_handle());
    }

    /// Forget finished tasks so a long job holds one handle per live task.
    fn prune(&mut self) {
        self.0.retain(|task| !task.is_finished());
    }

    /// The receive completed: leave the remaining tasks running.
    fn disarm(mut self) {
        self.0.clear();
    }
}

impl Drop for ReceiveTaskGuard {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Receive a multistream job: accept per-file streams until Finalize, drive
/// each stream in its own task (write/verify/ack), keep the checkpoint
/// current at the same cadence as the single-stream path. Returns the event
/// channel tail for the FinalAck hold (the control task keeps draining).
#[allow(clippy::too_many_arguments)]
pub(super) async fn multistream_receive(
    channel: &DirectChannel,
    control_rx: DirectRecvStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    sender_keepalive: bool,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EngineError> {
    let limits = engine_limits();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);

    // P1-2 fix (review round 17): one task-wide activity budget for the
    // control stream. The control stream legitimately carries no frame
    // between the ResumeRequest and the Finalize — the sender only writes
    // Finalize after every file stream finished — so an independent
    // per-read timeout on it killed healthy long multistream transfers the
    // moment the data phase outlasted `idle`. Now data-stream events
    // (chunks acked, files finished, streams accepted) refresh the budget
    // through the activity channel below, and only whole-task silence for
    // `idle` aborts.
    //
    // Structure: an inner reader task owns the receive half and forwards
    // decoded frames — it is never cancelled, because quinn reads are not
    // cancel-safe and dropping a mid-read future would lose frame bytes.
    // The monitor task below runs cancel-safe selects only (channel recv,
    // sleep) so the deadline can move without touching the reader.
    let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel::<()>(1);
    let (frames_tx, mut frames_rx) =
        tokio::sync::mpsc::channel::<Result<Frame, ak_net::StreamError>>(8);
    let mut tasks = ReceiveTaskGuard::default();
    let reader = tokio::spawn(async move {
        let mut rx = control_rx;
        loop {
            match rx.recv_frame(&limits).await {
                Ok(frame) => {
                    if frames_tx.send(Ok(frame)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = frames_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    tasks.track(&reader);

    // Control monitor: forwards Finalize/Cancel/Error into the driver's
    // event channel, then keeps draining silently so the connection is
    // held until the sender half-closes after reading the FinalAck. All
    // frames must name this job (cross-frame validation).
    let control_tx = events_tx.clone();
    let monitor = tokio::spawn(async move {
        let mut finalized = false;
        let mut activity_open = true;
        let mut deadline = tokio::time::Instant::now() + idle;
        loop {
            tokio::select! {
                frame = frames_rx.recv() => {
                    let Some(frame) = frame else {
                        // Reader ended: the connection closed underneath it.
                        break;
                    };
                    deadline = tokio::time::Instant::now() + idle;
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(ak_net::StreamError::Connection(_)) => break,
                        Err(e) => {
                            let _ = control_tx.send(StreamEvent::Failed(e.into())).await;
                            break;
                        }
                    };
                    match frame.message {
                        Message::Finalize { job_id: finalize_job } if !finalized => {
                            if finalize_job != job_id {
                                let _ = control_tx
                                    .send(StreamEvent::Failed(protocol_error(
                                        "finalize names a different job",
                                    )))
                                    .await;
                                break;
                            }
                            finalized = true;
                            if control_tx.send(StreamEvent::Finalize).await.is_err() {
                                break;
                            }
                        }
                        Message::Control { job_id: control_job, control: Control::Cancel } => {
                            if control_job != job_id {
                                let _ = control_tx
                                    .send(StreamEvent::Failed(protocol_error(
                                        "control frame names a different job",
                                    )))
                                    .await;
                                break;
                            }
                            let _ = control_tx.send(StreamEvent::Cancelled).await;
                            break;
                        }
                        // The sender's liveness signal (CAP_SENDER_KEEPALIVE in
                        // both Hellos) while it re-checks its sources: every
                        // data stream is finished by then, so pass the
                        // activity on to the driver, whose own idle budget
                        // nothing else refreshes. A pulse dropped because the
                        // channel is full is covered by the pending events.
                        Message::Control { job_id: control_job, control: Control::Resume }
                            if sender_keepalive && !finalized =>
                        {
                            if control_job != job_id {
                                let _ = control_tx
                                    .send(StreamEvent::Failed(protocol_error(
                                        "control frame names a different job",
                                    )))
                                    .await;
                                break;
                            }
                            let _ = control_tx.try_send(StreamEvent::KeepAlive);
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
                _ = tokio::time::sleep_until(deadline) => {
                    // No control frame AND no data activity for the whole
                    // budget: the task is truly idle, abort it. (Data
                    // activity alone keeps the budget fresh even while the
                    // control stream itself stays silent.)
                    let _ = control_tx
                        .send(StreamEvent::Failed(EngineError::IdleTimeout(idle)))
                        .await;
                    break;
                }
                signal = activity_rx.recv(), if activity_open => {
                    match signal {
                        // Coalesced activity pulse: refresh the budget. A
                        // bounded(1) channel keeps this cheap no matter how
                        // many chunks flow; try_send on the producer side
                        // drops extra pulses while one is pending.
                        Some(()) => deadline = tokio::time::Instant::now() + idle,
                        // Every producer (the driver loop) is gone — e.g.
                        // the transfer finished and the FinalAck hold is
                        // running. Stop arming this branch; the last
                        // deadline still bounds the drain.
                        None => activity_open = false,
                    }
                }
            }
        }
    });
    tasks.track(&monitor);

    let mut finalized = false;
    let mut open_streams = 0usize;
    let mut cadence = ChunkCheckpointCadence::new(Instant::now());
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
                // Data activity refreshes the control monitor's budget too
                // (see the activity channel above): one coalesced pulse per
                // processed event, dropped while one is already pending.
                let _ = activity_tx.try_send(());
                match event {
                    StreamEvent::Chunk { file_index, chunk_index } => {
                        let Some(slot) = record.files.get_mut(file_index as usize) else {
                            return Err(EngineError::PeerClosed);
                        };
                        slot.verified_prefix_chunks =
                            slot.verified_prefix_chunks.max(chunk_index + 1);
                        progress(Progress::ChunkReceived { file_index, chunk_index });
                        let tick = cadence.on_ack(Instant::now());
                        let prefix = slot.verified_prefix_chunks;
                        if tick.save {
                            record.updated_unix_ms = now_ms();
                            store.save(record).map_err(EngineError::Checkpoint)?;
                        }
                        if tick.progress {
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
                        tasks.prune();
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
                    StreamEvent::Opened { stream, first } => {
                        // The first frame on a new stream names its file and
                        // must belong to this job.
                        let Message::Chunk {
                            job_id: chunk_job,
                            file_index,
                            ..
                        } = first.message
                        else {
                            return Err(unexpected(first.message));
                        };
                        if chunk_job != job_id {
                            return Err(protocol_error("chunk names a different job"));
                        }
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
                        tasks.track(&receive_stream_task(
                            stream,
                            sink,
                            file_index,
                            first,
                            job_id,
                            idle,
                            events_tx.clone(),
                        ));
                    }
                    StreamEvent::Failed(error) => return Err(error),
                    // The sender was just told on the failing file's stream:
                    // keep the connection (and that stream's task) until the
                    // task's drain ends, then fail with the local error.
                    StreamEvent::DiskFull { error, drained } => {
                        let _ =
                            tokio::time::timeout(STORAGE_FULL_DRAIN_WAIT.min(idle), drained).await;
                        return Err(error);
                    }
                    StreamEvent::Finalize => finalized = true,
                    StreamEvent::Cancelled => return Err(EngineError::Cancelled),
                    // The idle budget was refreshed above; nothing else.
                    StreamEvent::KeepAlive => {}
                }
            }
            accepted = tokio::time::timeout(budget, channel.accept_bi()), if !finalized => {
                match accepted {
                    Err(_elapsed) => return Err(EngineError::IdleTimeout(idle)),
                    // The sender closing after a full disk was told races
                    // the stream task's report through the event channel:
                    // the full disk is the real cause.
                    Ok(Err(e)) => {
                        return Err(pending_storage_full(&mut events_rx)
                            .unwrap_or(EngineError::Stream(e)))
                    }
                    Ok(Ok(stream)) => {
                        last_activity = Instant::now();
                        let _ = activity_tx.try_send(());
                        // M6 (batch M, 2026-11-17): the first frame on a new
                        // stream is read by the stream's own opener task,
                        // never inline here. While the driver awaited one
                        // stream's first frame it could neither accept nor
                        // let anything read the sender's other concurrently
                        // opened streams, whose buffered data still counts
                        // against QUIC's connection-level receive window
                        // (credit returns only as the application reads).
                        // Once streams x (chunk + header) exceeded that
                        // window (2 streams at 4 MiB chunks, 4 at 2 MiB,
                        // against 8 MiB) the first frame could never
                        // complete: a deadlock, which the release
                        // chunk-size sweep hit at 4096 KiB / multistream on
                        // Linux and Windows alike.
                        open_streams += 1;
                        let events = events_tx.clone();
                        let opener = tokio::spawn(async move {
                            let mut stream = stream;
                            let event = match tokio::time::timeout(
                                idle,
                                stream.recv_frame(&limits),
                            )
                            .await
                            {
                                Ok(Ok(first)) => StreamEvent::Opened { stream, first },
                                Ok(Err(e)) => StreamEvent::Failed(EngineError::Stream(e)),
                                Err(_) => StreamEvent::Failed(EngineError::IdleTimeout(idle)),
                            };
                            let _ = events.send(event).await;
                        });
                        tasks.track(&opener);
                    }
                }
            }
        }
    }
    tasks.disarm();
    Ok(events_rx)
}

/// Drive one accepted per-file stream: write/verify/ack every chunk until
/// the sender half-closes, then hand the sink back to the driver. Returns
/// the task's handle so the driver's [`ReceiveTaskGuard`] can abort it.
fn receive_stream_task(
    mut stream: DirectStream,
    mut sink: ak_storage::LocalFileSink,
    file_index: u32,
    first: Frame,
    job_id: JobId,
    idle: Duration,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
) -> tokio::task::JoinHandle<()> {
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
                    job_id: chunk_job,
                    file_index: frame_file_index,
                    chunk_index,
                    data,
                } => {
                    // Cross-frame consistency: every frame on this stream
                    // must belong to the job and to the file this stream
                    // announced in its first frame (otherwise a peer could
                    // smuggle chunks of one file into another file's ack
                    // stream).
                    if chunk_job != job_id {
                        break Err(protocol_error("chunk names a different job"));
                    }
                    if frame_file_index != file_index {
                        break Err(protocol_error("chunk arrives on a different file's stream"));
                    }
                    if let Err(e) = write_chunk_into(&mut sink, &job_id, chunk_index, &data) {
                        let error = EngineError::Io(e);
                        if is_storage_full(&error) {
                            // Reports the failure itself (see there).
                            tell_storage_full_on_stream(&mut stream, error, idle, &events).await;
                            return;
                        }
                        break Err(error);
                    }
                    // Durability contract (see module docs): the ack
                    // confirms a verified digest in the part file's page
                    // cache, not a durable write.
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
                    job_id: control_job,
                    control: Control::Cancel,
                } => {
                    if control_job != job_id {
                        break Err(protocol_error("control frame names a different job"));
                    }
                    break Err(EngineError::Cancelled);
                }
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
    })
}
