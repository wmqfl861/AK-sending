//! The per-connection loop: reader, writer, cleanup and message dispatch.
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- connection loop ---------------------------------------------------------

pub(super) enum Flow {
    Continue,
    Disconnect,
}

pub(super) struct ConnState {
    pub(super) device: Option<DeviceId>,
    pub(super) conn_id: u64,
    pub(super) close: Arc<watch::Sender<bool>>,
    pub(super) peer_ip: IpAddr,
    pub(super) challenge: Option<[u8; 32]>,
    pub(super) challenge_expires_ms: u64,
    pub(super) exporter: [u8; 32],
    pub(super) redeem_window: SlidingWindow,
}

pub(super) async fn connection_loop(
    stream: TlsServerStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
) {
    // The exporter is bound to this TLS connection only (rustls 0.23
    // exposes export_keying_material; plan A.5 as specified, no
    // certificate-only fallback).
    let exporter = match hello2::exporter_secret_server(stream.get_ref().1) {
        Ok(value) => value,
        Err(e) => {
            log::debug!("exporter unavailable for {peer}: {e}");
            return;
        }
    };
    let (out_tx, out_rx) = mpsc::channel::<CtrlFrame>(OUTBOX_DEPTH);
    let nonce = random_nonce32();
    let expires_ms = now_ms() + shared.limits.hello_max_age.as_millis() as u64;
    let mut stream = stream;
    // AuthChallenge first: one nonce, this connection, at most 10 s —
    // and the write itself under the shared frame budget.
    let challenge = CtrlMessage::AuthChallenge {
        registry_id: shared.registry_id,
        nonce,
        expires_ms,
    };
    let sent = tokio::time::timeout(
        FRAME_IO_DEADLINE,
        write_message(&mut stream, &challenge, Role::Server),
    )
    .await;
    if !matches!(sent, Ok(Ok(()))) {
        return;
    }
    let (close_tx, close_rx) = watch::channel(false);
    let mut state = ConnState {
        device: None,
        conn_id: 0,
        close: Arc::new(close_tx),
        peer_ip: peer.ip(),
        challenge: Some(nonce),
        challenge_expires_ms: expires_ms,
        exporter,
        redeem_window: SlidingWindow::new(
            shared.limits.redeem_per_conn_per_min,
            Duration::from_secs(60),
        ),
    };
    // P2 (帧读失步): reading and writing are separate tasks now. The old
    // loop selected `read_frame` against the outbox; whenever an outbound
    // message won, the partially-read frame future was DROPPED and the
    // next read began mid-frame — a protocol desync. Now one reader owns
    // the read half, one writer owns the write half (the client_v2
    // pattern), and the handler only ever touches channels. All three
    // tasks share the close watch: `send(true)` — or the handler's
    // disappearance — ends all of them; a writer that cannot drain tells
    // the handler through `writer_dead`.
    let (read_half, write_half) = tokio::io::split(stream);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOX_DEPTH);
    let (writer_dead_tx, mut writer_dead_rx) = mpsc::channel::<()>(1);
    let mut reader = tokio::spawn(reader_loop(
        read_half,
        inbound_tx,
        close_rx.clone(),
        FIRST_FRAME_DEADLINE,
        FRAME_IO_DEADLINE,
    ));
    let mut writer = tokio::spawn(writer_loop(
        write_half,
        out_rx,
        close_rx.clone(),
        writer_dead_tx,
        FRAME_IO_DEADLINE,
    ));
    let mut close_watch = close_rx;
    let mut bucket = TokenBucket::new(shared.limits.frame_rate_per_sec, shared.limits.frame_burst);
    log::debug!("v2 control connection from {peer}");
    loop {
        let bytes = tokio::select! {
            _ = closed(&mut close_watch) => break,
            // The writer cannot drain (stalled or failed write): the
            // connection is dead even though the socket may not know it.
            dead = writer_dead_rx.recv() => match dead {
                Some(()) => break,
                None => break,
            },
            received = inbound_rx.recv() => match received {
                Some(bytes) => bytes,
                // The reader died (deadline, EOF or I/O error).
                None => break,
            },
        };
        if !bucket.allow() {
            log::warn!("frame rate limit hit on v2 connection from {peer}");
            deliver(
                &out_tx,
                server_error(ERR_RATE_LIMITED, "frame rate exceeded"),
            );
            break;
        }
        // Direction legality at decode: a client may not emit
        // server-to-client types (decode_as enforces the frozen table).
        let frame = match CtrlFrame::decode_as(&bytes, Role::Server) {
            Ok(frame) => frame,
            Err(e) => {
                log::debug!("undecodable v2 frame from {peer}: {e}");
                break;
            }
        };
        match process(&shared, &mut state, &out_tx, frame) {
            Flow::Continue => {}
            Flow::Disconnect => break,
        }
    }
    // Teardown: wake the reader/writer, unregister, then join both
    // bounded (abort is the fallback) so neither task outlives the
    // handler. Dropping `state` afterwards also closes the watch for
    // anyone still holding a receiver.
    let _ = state.close.send(true);
    cleanup_connection(&shared, &mut state);
    drop(out_tx);
    join_or_abort(&mut reader).await;
    join_or_abort(&mut writer).await;
}

/// Wait for a task to finish within [`TEARDOWN_GRACE`]; abort it if the
/// grace expires (the task is parked on I/O that will never progress
/// once its sockets start closing).
async fn join_or_abort(task: &mut JoinHandle<()>) {
    if tokio::time::timeout(TEARDOWN_GRACE, &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = (&mut *task).await;
    }
}

/// The single reader of one connection: continuously reads frames into
/// the bounded inbound channel. The first frame (the `Hello` answering
/// the AuthChallenge) runs under `first_budget`; every later frame under
/// `steady_budget` ([`FIRST_FRAME_DEADLINE`] / [`FRAME_IO_DEADLINE`] in
/// production). `read_frame` checks the declared header length against
/// the 4092-byte payload cap BEFORE any payload allocation, so an
/// oversized declaration dies here without waiting for the bytes. Any
/// deadline, EOF or I/O error ends the reader, which closes the channel
/// and thereby ends the handler.
pub(super) async fn reader_loop<R: AsyncRead + Unpin>(
    mut read: R,
    inbound: mpsc::Sender<Vec<u8>>,
    mut close: watch::Receiver<bool>,
    first_budget: Duration,
    steady_budget: Duration,
) {
    let mut first = true;
    loop {
        let budget = if first { first_budget } else { steady_budget };
        let received = tokio::select! {
            _ = closed(&mut close) => return,
            received = tokio::time::timeout(budget, read_frame(&mut read)) => received,
        };
        let bytes = match received {
            Ok(Ok(Some(bytes))) => bytes,
            // Deadline, clean EOF or I/O error: stop reading.
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return,
        };
        first = false;
        // Bounded hand-off: a handler this far behind is a flooding
        // peer, not a slow one — drop the connection.
        if inbound.try_send(bytes).is_err() {
            return;
        }
    }
}

/// The single writer of one connection: serializes every outbound frame
/// through one task (concurrent handlers can never interleave bytes
/// inside one frame) under the per-frame write budget. A stalled or
/// failed write tears the whole connection down via `close`.
pub(super) async fn writer_loop<W: AsyncWrite + Unpin>(
    mut write: W,
    mut out: mpsc::Receiver<CtrlFrame>,
    mut close: watch::Receiver<bool>,
    dead: mpsc::Sender<()>,
    write_budget: Duration,
) {
    loop {
        let frame = tokio::select! {
            _ = closed(&mut close) => break,
            frame = out.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        let sent =
            tokio::time::timeout(write_budget, write_frame(&mut write, &frame, Role::Server)).await;
        if !matches!(sent, Ok(Ok(()))) {
            log::debug!("v2 outbound write stalled or failed; closing connection");
            // Tell the handler this connection is dead (best-effort: it
            // may already be tearing down).
            let _ = dead.try_send(());
            return;
        }
    }
    // Close signaled: flush any frames already queued (notably the
    // ERR_DEVICE_REPLACED reason from the replace path above) before the
    // TLS shutdown — the select above is randomly fair between `closed`
    // and `out.recv()`, so without this drain the reason frame can be
    // skipped and the peer sees a bare EOF after all.
    while let Ok(frame) = out.try_recv() {
        if !matches!(
            tokio::time::timeout(write_budget, write_frame(&mut write, &frame, Role::Server)).await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    let _ = write.shutdown().await;
}

fn cleanup_connection(shared: &Shared, state: &mut ConnState) {
    let Some(device) = state.device else {
        return;
    };
    let mut inner = shared.lock();
    if !inner
        .conns
        .get(&device)
        .map(|entry| entry.conn_id == state.conn_id)
        .unwrap_or(false)
    {
        return;
    }
    inner.conns.remove(&device);
    let sessions: Vec<SessionId> = inner
        .sessions
        .iter()
        .filter(|(_, session)| session.endpoints.contains(&device))
        .map(|(id, _)| *id)
        .collect();
    for id in sessions {
        if let Some(session) = inner.sessions.remove(&id) {
            inner.session_by_triple.remove(&session.triple);
        }
    }
    log::info!("v2 device {} disconnected", util::hex_prefix(&device, 4));
}

fn process(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    mut frame: CtrlFrame,
) -> Flow {
    if state.device.is_none() {
        return process_hello(shared, state, out, frame.message);
    }
    let me = state.device.unwrap_or([0u8; 32]);
    match frame.message {
        CtrlMessage::Hello { .. } => {
            log::debug!("duplicate Hello on authenticated v2 connection");
            Flow::Disconnect
        }
        CtrlMessage::Ping { nonce } => {
            deliver_msg(out, CtrlMessage::Pong { nonce });
            Flow::Continue
        }
        // G6 §6a: answer the revision query with this build's fingerprint
        // (sanitized to the wire budget at startup). Idempotent,
        // stateless, and rate-limited by the connection's frame token
        // bucket like every other frame; the server NEVER pushes it
        // unasked, so an old client never sees the new tag.
        CtrlMessage::ServerInfoRequest => {
            deliver_msg(
                out,
                CtrlMessage::ServerInfo {
                    revision: shared.server_revision.to_string(),
                },
            );
            Flow::Continue
        }
        CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        } => {
            // wave-28: the claim's extension section carries the owner
            // endpoint blob and the optional redeem cap.
            handle_claim(
                shared,
                out,
                me,
                request_id,
                lookup_digest,
                ttl_secs,
                frame.endpoint.take(),
                frame.max_redeems.take().unwrap_or(0),
            );
            Flow::Continue
        }
        CtrlMessage::InviteList {
            request_id,
            cursor,
            limit,
            state_filter,
        } => {
            handle_list(shared, out, me, request_id, cursor, limit, state_filter);
            Flow::Continue
        }
        CtrlMessage::InviteRevoke {
            request_id,
            code_id,
            expected_revision,
        } => {
            handle_revoke(shared, out, me, request_id, code_id, expected_revision);
            Flow::Continue
        }
        // wave-28: the redeem's extension section carries the redeemer
        // endpoint blob (forwarded to the owner with the offer).
        CtrlMessage::InviteRedeem(redeem) => {
            let endpoint = frame.endpoint.take();
            handle_redeem(shared, state, out, me, redeem, endpoint)
        }
        CtrlMessage::PairChallenge {
            transcript,
            owner_proof,
        } => {
            handle_pair_challenge(shared, me, transcript, owner_proof);
            Flow::Continue
        }
        CtrlMessage::PairProof {
            pairing_id,
            transcript_digest,
            redeemer_proof,
        } => {
            handle_pair_proof(shared, me, pairing_id, transcript_digest, redeemer_proof);
            Flow::Continue
        }
        CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest,
            signature,
        } => {
            handle_prepared(shared, out, me, pairing_id, transcript_digest, signature);
            Flow::Continue
        }
        CtrlMessage::PairApplied {
            pairing_id,
            transcript_digest,
            commit_revision,
            signature,
        } => {
            handle_applied(
                shared,
                out,
                me,
                pairing_id,
                transcript_digest,
                commit_revision,
                signature,
            );
            Flow::Continue
        }
        CtrlMessage::PairStatusQuery {
            request_id,
            pairing_id,
        } => {
            handle_status_query(shared, out, me, request_id, pairing_id);
            Flow::Continue
        }
        CtrlMessage::PairAbort {
            request_id,
            pairing_id,
            reason: _,
        } => {
            handle_abort(shared, out, me, request_id, pairing_id);
            Flow::Continue
        }
        CtrlMessage::Register { candidates } => {
            let mut inner = shared.lock();
            if let Some(entry) = inner.conns.get_mut(&me) {
                if entry.conn_id == state.conn_id {
                    entry.candidates = candidates;
                }
            }
            Flow::Continue
        }
        CtrlMessage::ConnectRequest {
            request_id: _,
            target,
            pairing_id,
            code_id,
            connection_id,
        } => {
            handle_connect(shared, out, me, target, pairing_id, code_id, connection_id);
            Flow::Continue
        }
        CtrlMessage::Signal { session, signal } => {
            handle_signal(shared, out, me, session, signal);
            Flow::Continue
        }
        CtrlMessage::Cancel { session } => {
            handle_cancel(shared, me, session);
            Flow::Continue
        }
        // Server-to-client and malformed-direction types are rejected by
        // decode_as before this point; the remainder is unreachable.
        other => {
            log::debug!("unexpected v2 message type {}", other.type_byte());
            Flow::Disconnect
        }
    }
}
