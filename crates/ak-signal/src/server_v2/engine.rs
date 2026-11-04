//! The v2 pairing-transaction engine: one TLS connection per device,
//! the claim/redeem/revoke flows, and the Prepared→Commit→Applied→Ready
//! state machine over the authoritative [`RegistryStore`]
//! (`docs/contracts/pairing-flow.md` §6).
//!
//! Entry points: [`run_v2`] (bind + serve) and [`serve_v2`] (pre-bound
//! listener, used by embedders and the integration tests). Both require
//! the `server-persistence` feature — without the authoritative store the
//! server refuses invite operations entirely rather than issuing codes it
//! cannot make unique (`storage-schema.md` §1).
//!
//! Error policy (`pairing-flow.md` §11): rate/quota rejections are
//! immediate (`RATE_LIMITED` / `RESOURCE_LIMIT`) and run **before** the
//! lookup; every other redeem failure — unknown code, wrong code,
//! expired, revoked, owner offline — is deferred to the deadline and
//! surfaces as the identical `PAIR_UNAVAILABLE` (initial
//! `InviteRedeemPending` responses have the same shape for existing and
//! non-existing codes). Business errors always travel as fixed `PairError`
//! frames; `ServerError` keeps only the legacy bounded session codes.
//!
//! What the server verifies (never the MACs — `auth_key` lives only on
//! the endpoints): every message's Ed25519 signature over its frozen
//! domain, the Hello-v2 exporter binding, transcript bindings (registry,
//! code, ids, both device keys, nonces, request, deadline), and
//! transcript-digest consistency across `PairProof`/`PairPrepared`/
//! `PairApplied`.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairTranscript, OWNER_PROOF_DOMAIN, REDEEMER_PROOF_DOMAIN,
};
use ak_identity::DeviceKeyPair;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Notify};

use crate::protocol_v2::{
    CodeId, CtrlFrame, CtrlMessage, DeviceId, Digest32, ErrorScope, PairProof, PairingId,
    PairingState, RegistryId, RequestId, Role, SessionId, Signature64, StateFilter, CODE_CONFLICT,
    CURSOR_END, PAIR_UNAVAILABLE, PROTOCOL_UNSUPPORTED, RATE_LIMITED, REQUEST_CONFLICT,
    RESOURCE_LIMIT, REVISION_CONFLICT, STORAGE_UNAVAILABLE,
};
use crate::server_v2::config::{Config, Limits, OUTBOX_DEPTH, REDEEM_MAX_WINDOW, SWEEP_INTERVAL};
use crate::server_v2::framing::{read_frame, write_message};
use crate::server_v2::ratelimit::{SlidingWindow, TokenBucket};
use crate::server_v2::store::{
    AbortDecision, AppliedDecision, BeginRedeem, ClaimOutcome, DueReport, PairingSnapshot,
    PreparedDecision, RegistryStore, RevokeOutcome,
};
use crate::server_v2::tls;
use crate::server_v2::{hello2, CAPABILITIES};
use crate::util;

/// Legacy bounded server error codes kept by v2 `ServerError` frames
/// (session-plane errors only; invite business errors use `PairError`).
pub const ERR_UNKNOWN_DEVICE: u16 = 1;
pub const ERR_SESSION_LIMIT: u16 = 2;
pub const ERR_RATE_LIMITED: u16 = 3;
pub const ERR_BAD_REQUEST: u16 = 4;
pub const ERR_SESSION_NOT_FOUND: u16 = 5;

type TlsServerStream = tokio_rustls::server::TlsStream<TcpStream>;

/// Bind `config.listen`, open the authoritative store at `config.db_path`,
/// and serve until `shutdown` fires.
pub async fn run_v2(
    config: Config,
    registry: &DeviceKeyPair,
    shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let listener = TcpListener::bind(config.listen).await?;
    let store = if config.db_path.as_os_str() == ":memory:" {
        RegistryStore::open_in_memory()
    } else {
        RegistryStore::open(&config.db_path)
    }
    .map_err(|e| io::Error::other(format!("registry store: {e}")))?;
    serve_v2(listener, registry, store, config.limits, shutdown).await
}

/// Serve v2 on an already-bound listener (embedders/tests bind ephemeral
/// ports themselves and get identical behavior).
pub async fn serve_v2(
    listener: TcpListener,
    registry: &DeviceKeyPair,
    store: RegistryStore,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let bundle = ak_identity::tls::make_self_signed(registry);
    let acceptor = tls::server_config(&bundle)?;
    let shared = Arc::new(Shared {
        registry_id: registry.public_key(),
        acceptor,
        store,
        limits,
        inner: Mutex::new(Inner::default()),
    });
    log::info!(
        "v2 registry server listening on tls {} (registry {})",
        listener.local_addr()?,
        util::hex_prefix(&shared.registry_id, 4)
    );
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = tick.tick() => sweep(&shared),
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    let _ = tcp.set_nodelay(true);
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        match shared.acceptor.accept(tcp).await {
                            Ok(stream) => {
                                if tls::check_alpn(stream.get_ref().1.alpn_protocol()).is_err() {
                                    log::debug!("v2 connection from {peer} without ALPN ak-signal/2");
                                    return;
                                }
                                connection_loop(stream, peer, shared).await;
                            }
                            Err(e) => log::debug!("v2 tls handshake with {peer} failed: {e}"),
                        }
                    });
                }
                Err(e) => {
                    log::warn!("v2 accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

// ---- shared state -----------------------------------------------------------

struct ConnEntry {
    conn_id: u64,
    out: mpsc::Sender<CtrlMessage>,
    close: Arc<Notify>,
    candidates: Vec<SocketAddr>,
    peer_ip: IpAddr,
}

#[derive(Clone)]
struct SessionEntry {
    triple: (PairingId, CodeId, [u8; 16]),
    endpoints: [DeviceId; 2],
    expires_at: Instant,
}

/// A redeem of an unknown/invalid code, kept only in memory: the client
/// sees the same `InviteRedeemPending` shape as for a live code and the
/// unified `PAIR_UNAVAILABLE` at the deadline (no existence oracle).
struct Ghost {
    requester: DeviceId,
    request_id: RequestId,
    deadline_ms: u64,
    expires_at: Instant,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<DeviceId, ConnEntry>,
    sessions: HashMap<SessionId, SessionEntry>,
    session_by_triple: HashMap<(PairingId, CodeId, [u8; 16]), SessionId>,
    ghosts: HashMap<PairingId, Ghost>,
    next_conn_id: u64,
    hello_replay: HelloReplayCache,
    global_redeem: Option<SlidingWindow>,
}

struct Shared {
    registry_id: RegistryId,
    acceptor: tokio_rustls::TlsAcceptor,
    store: RegistryStore,
    limits: Limits,
    inner: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Bounded outbound push: the per-connection outbox is `OUTBOX_DEPTH`
    /// deep; a full queue drops the message (recoverable via
    /// `PairStatusQuery` resends) instead of blocking any handler.
    fn deliver(&self, device: &DeviceId, message: CtrlMessage) {
        let tx = {
            let inner = self.lock();
            inner.conns.get(device).map(|entry| entry.out.clone())
        };
        match tx {
            Some(tx) => deliver(&tx, message),
            None => log::debug!(
                "device {} offline; message dropped",
                util::hex_prefix(device, 4)
            ),
        }
    }

    fn global_redeem_allows(&self, now: Instant) -> (bool, u32) {
        let mut inner = self.lock();
        let window = inner.global_redeem.get_or_insert_with(|| {
            SlidingWindow::new(self.limits.redeem_global_per_min, Duration::from_secs(60))
        });
        (window.allow_at(now), window.retry_after_ms(now))
    }
}

fn deliver(tx: &mpsc::Sender<CtrlMessage>, message: CtrlMessage) {
    if let Err(e) = tx.try_send(message) {
        log::debug!("outbound queue full or closed; message dropped: {e}");
    }
}

fn pair_error(
    request_id: RequestId,
    scope: ErrorScope,
    object: [u8; 16],
    code: u16,
    retry_after_ms: u32,
) -> CtrlMessage {
    CtrlMessage::PairError {
        request_id,
        scope,
        object_id: object,
        code,
        retry_after_ms,
    }
}

/// Unified "target unavailable" — the one answer for unknown/expired/
/// revoked codes, foreign pairings, and MAC-shape failures alike.
fn unavailable(request_id: RequestId, object: [u8; 16]) -> CtrlMessage {
    pair_error(request_id, ErrorScope::Pairing, object, PAIR_UNAVAILABLE, 0)
}

fn server_error(code: u16, context: &str) -> CtrlMessage {
    let message = match code {
        ERR_UNKNOWN_DEVICE => "target device not online",
        ERR_SESSION_LIMIT => "session limit reached",
        ERR_RATE_LIMITED => "frame rate exceeded",
        ERR_BAD_REQUEST => "malformed request",
        ERR_SESSION_NOT_FOUND => "session not found",
        _ => context,
    }
    .to_string();
    CtrlMessage::ServerError { code, message }
}

fn now_ms() -> u64 {
    RegistryStore::now_ms()
}

// ---- connection loop ---------------------------------------------------------

enum Flow {
    Continue,
    Disconnect,
}

struct ConnState {
    device: Option<DeviceId>,
    conn_id: u64,
    close: Arc<Notify>,
    peer_ip: IpAddr,
    challenge: Option<[u8; 32]>,
    challenge_expires_ms: u64,
    exporter: [u8; 32],
    redeem_window: SlidingWindow,
}

async fn connection_loop(mut stream: TlsServerStream, peer: SocketAddr, shared: Arc<Shared>) {
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
    let (out_tx, mut out_rx) = mpsc::channel::<CtrlMessage>(OUTBOX_DEPTH);
    let nonce = random_nonce32();
    let expires_ms = now_ms() + shared.limits.hello_max_age.as_millis() as u64;
    // AuthChallenge first: one nonce, this connection, at most 10 s.
    if write_message(
        &mut stream,
        &CtrlMessage::AuthChallenge {
            registry_id: shared.registry_id,
            nonce,
            expires_ms,
        },
        Role::Server,
    )
    .await
    .is_err()
    {
        return;
    }
    let mut state = ConnState {
        device: None,
        conn_id: 0,
        close: Arc::new(Notify::new()),
        peer_ip: peer.ip(),
        challenge: Some(nonce),
        challenge_expires_ms: expires_ms,
        exporter,
        redeem_window: SlidingWindow::new(
            shared.limits.redeem_per_conn_per_min,
            Duration::from_secs(60),
        ),
    };
    let mut bucket = TokenBucket::new(shared.limits.frame_rate_per_sec, shared.limits.frame_burst);
    log::debug!("v2 control connection from {peer}");
    loop {
        let bytes = tokio::select! {
            _ = state.close.notified() => break,
            message = out_rx.recv() => match message {
                Some(message) => {
                    if write_message(&mut stream, &message, Role::Server).await.is_err() {
                        break;
                    }
                    continue;
                }
                None => break,
            },
            received = read_frame(&mut stream) => match received {
                Ok(Some(bytes)) => bytes,
                Ok(None) | Err(_) => break,
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
        let message = match CtrlFrame::decode_as(&bytes, Role::Server) {
            Ok(frame) => frame.message,
            Err(e) => {
                log::debug!("undecodable v2 frame from {peer}: {e}");
                break;
            }
        };
        match process(&shared, &mut state, &out_tx, message) {
            Flow::Continue => {}
            Flow::Disconnect => break,
        }
    }
    cleanup_connection(&shared, &mut state);
    drop(out_tx);
    let _ = stream.shutdown().await;
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
    out: &mpsc::Sender<CtrlMessage>,
    message: CtrlMessage,
) -> Flow {
    if state.device.is_none() {
        return process_hello(shared, state, out, message);
    }
    let me = state.device.unwrap_or([0u8; 32]);
    match message {
        CtrlMessage::Hello { .. } => {
            log::debug!("duplicate Hello on authenticated v2 connection");
            Flow::Disconnect
        }
        CtrlMessage::Ping { nonce } => {
            deliver(out, CtrlMessage::Pong { nonce });
            Flow::Continue
        }
        CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        } => {
            handle_claim(shared, out, me, request_id, lookup_digest, ttl_secs);
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
        CtrlMessage::InviteRedeem(redeem) => handle_redeem(shared, state, out, me, redeem),
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

// ---- hello ------------------------------------------------------------------

fn process_hello(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlMessage>,
    message: CtrlMessage,
) -> Flow {
    let CtrlMessage::Hello {
        version,
        device_id,
        name,
        timestamp_ms,
        challenge,
        signature,
    } = message
    else {
        log::debug!("v2 connection must start with Hello");
        return Flow::Disconnect;
    };
    if version != crate::protocol_v2::VERSION.into() {
        log::debug!("hello v2 with unsupported version {version}");
        return Flow::Disconnect;
    }
    let Some(expected_nonce) = state.challenge else {
        log::debug!("hello without a live AuthChallenge");
        return Flow::Disconnect;
    };
    // The challenge is single-use and bound to this connection.
    state.challenge = None;
    if challenge != expected_nonce {
        log::debug!("hello challenge echo mismatch");
        return Flow::Disconnect;
    }
    let now = now_ms();
    if now > state.challenge_expires_ms {
        log::debug!("hello arrived after its AuthChallenge expired");
        return Flow::Disconnect;
    }
    if Duration::from_millis(now.abs_diff(timestamp_ms)) > shared.limits.hello_max_age {
        log::info!(
            "hello from {} rejected: stale",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Exporter-bound signature: valid only on this TLS connection.
    let signed = hello2::hello2_message(
        &shared.registry_id,
        &state.exporter,
        version,
        &device_id,
        &name,
        timestamp_ms,
        &challenge,
    );
    if !hello2::verify(&device_id, &signed, &signature) {
        log::info!(
            "hello from {} rejected: bad signature",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Replay window (defense in depth on top of the exporter binding and
    // the single-use challenge).
    let digest = hello_digest(&device_id, &signature);
    let registered = {
        let mut inner = shared.lock();
        if !inner
            .hello_replay
            .admit(digest, Instant::now(), shared.limits.hello_max_age)
        {
            false
        } else {
            inner.next_conn_id = inner.next_conn_id.wrapping_add(1);
            let conn_id = inner.next_conn_id;
            if let Some(previous) = inner.conns.get(&device_id) {
                previous.close.notify_one();
            }
            inner.conns.insert(
                device_id,
                ConnEntry {
                    conn_id,
                    out: out.clone(),
                    close: state.close.clone(),
                    candidates: Vec::new(),
                    peer_ip: state.peer_ip,
                },
            );
            true
        }
    };
    if !registered {
        log::warn!(
            "hello from {} rejected: replay suspected",
            util::hex_prefix(&device_id, 4)
        );
        deliver(out, server_error(ERR_BAD_REQUEST, "replay suspected"));
        return Flow::Disconnect;
    }
    state.device = Some(device_id);
    state.conn_id = {
        let inner = shared.lock();
        inner
            .conns
            .get(&device_id)
            .map(|entry| entry.conn_id)
            .unwrap_or(0)
    };
    log::info!("v2 device {} online", util::hex_prefix(&device_id, 4));
    deliver(
        out,
        CtrlMessage::Welcome {
            session_ttl_secs: shared.limits.session_ttl.as_secs() as u32,
            capabilities: CAPABILITIES,
        },
    );
    // Catch-up pushes: pending redeem offers for this owner, stored
    // owner challenges / redeemer proofs the other side already sent.
    catchup_after_hello(shared, &device_id);
    Flow::Continue
}

fn catchup_after_hello(shared: &Shared, me: &DeviceId) {
    match shared.store.pending_offers_for_owner(me) {
        Ok(offers) => {
            for offer in offers {
                shared.deliver(
                    me,
                    CtrlMessage::InviteRedeemOffered {
                        code_id: offer.code_id,
                        code_revision: offer.code_revision,
                        pairing_id: offer.pairing_id,
                        redeemer_device_id: offer.redeemer,
                        redeem: offer.redeem,
                    },
                );
            }
        }
        Err(e) => log::error!("pending offer catch-up failed: {e}"),
    }
    match shared.store.pending_challenges_for_redeemer(me) {
        Ok(challenges) => {
            for (_pairing_id, transcript, owner_proof) in challenges {
                shared.deliver(
                    me,
                    CtrlMessage::PairChallenge {
                        transcript,
                        owner_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("challenge catch-up failed: {e}"),
    }
    match shared.store.pending_proofs_for_owner(me) {
        Ok(proofs) => {
            for (pairing_id, transcript_digest, redeemer_proof) in proofs {
                shared.deliver(
                    me,
                    CtrlMessage::PairProof {
                        pairing_id,
                        transcript_digest,
                        redeemer_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("proof catch-up failed: {e}"),
    }
}

/// 256-bit digest of (device_id || signature) for the Hello replay
/// window — the same splitmix64-lane construction as the v1 server
/// (no crypto dependency on this plane; see `crate::server` for the
/// rationale).
fn hello_digest(device_id: &DeviceId, signature: &Signature64) -> [u8; 32] {
    let mut lanes = [
        0x243F_6A88_85A3_08D3u64,
        0x1319_8A2E_0370_7344u64,
        0xA409_3822_299F_31D0u64,
        0x082E_FA98_EC4E_6C89u64,
    ];
    let absorb = |lanes: &mut [u64; 4], chunk: u64, index: usize| {
        for (i, lane) in lanes.iter_mut().enumerate() {
            *lane = lane
                .wrapping_add(chunk.rotate_left(((index * 8 + i * 3) % 64) as u32))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15 ^ ((i as u64) << 32));
        }
    };
    let mut input = [0u8; 96];
    input[..32].copy_from_slice(device_id);
    input[32..].copy_from_slice(signature);
    let mut chunks = [0u64; 12];
    for (i, chunk) in chunks.iter_mut().enumerate() {
        *chunk = u64::from_le_bytes(input[i * 8..i * 8 + 8].try_into().unwrap());
    }
    for (index, chunk) in chunks.iter().enumerate() {
        absorb(&mut lanes, *chunk, index);
    }
    let mut out = [0u8; 32];
    for (i, lane) in lanes.iter().enumerate() {
        let mut z = *lane;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out[i * 8..i * 8 + 8].copy_from_slice(&z.to_le_bytes());
    }
    out
}

/// Sliding-window memory of accepted Hello signature digests (v1
/// semantics, server-wide across connections).
#[derive(Debug, Default)]
struct HelloReplayCache {
    seen: VecDeque<([u8; 32], Instant)>,
}

impl HelloReplayCache {
    fn admit(&mut self, digest: [u8; 32], now: Instant, window: Duration) -> bool {
        while let Some((_, at)) = self.seen.front() {
            if now.duration_since(*at) > window {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        if self.seen.iter().any(|(seen, _)| *seen == digest) {
            return false;
        }
        if self.seen.len() >= crate::server::HELLO_REPLAY_CAPACITY {
            self.seen.pop_front();
        }
        self.seen.push_back((digest, now));
        true
    }
}

// ---- invite management --------------------------------------------------------

fn handle_claim(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    request_id: RequestId,
    lookup_digest: Digest32,
    ttl_secs: u32,
) {
    if ttl_secs != 0
        && !(crate::server_v2::config::INVITE_TTL_MIN_SECS
            ..=crate::server_v2::config::INVITE_TTL_MAX_SECS)
            .contains(&ttl_secs)
    {
        deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return;
    }
    let now = now_ms();
    match shared
        .store
        .claim(&me, &request_id, &lookup_digest, ttl_secs, now)
    {
        Ok(ClaimOutcome::Claimed {
            code_id,
            created_ms,
            expires_ms,
            revision,
        }) => {
            deliver(
                out,
                CtrlMessage::InviteClaimed {
                    request_id,
                    code_id,
                    created_ms,
                    expires_ms,
                    revision,
                },
            );
        }
        Ok(ClaimOutcome::Replay(bytes)) => deliver(out, decode_stored(&bytes)),
        Ok(ClaimOutcome::RequestConflict) => deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::CodeConflict) => deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                CODE_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::RateLimited) => deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RATE_LIMITED,
                60_000,
            ),
        ),
        Ok(ClaimOutcome::ResourceLimit) => deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RESOURCE_LIMIT,
                0,
            ),
        ),
        Err(e) => {
            log::error!("claim storage failure: {e}");
            deliver(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

/// Decode a frame the server itself produced and journaled. Invariant:
/// these bytes round-tripped `try_encode_as`; failure is a bug, not an
/// input, so it surfaces as a storage error reply.
fn decode_stored(bytes: &[u8]) -> CtrlMessage {
    CtrlFrame::decode(bytes)
        .map(|frame| frame.message)
        .unwrap_or(CtrlMessage::ServerError {
            code: STORAGE_UNAVAILABLE,
            message: "stored response unreadable".to_string(),
        })
}

fn handle_list(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    request_id: RequestId,
    cursor: CodeId,
    limit: u8,
    state_filter: StateFilter,
) {
    let now = now_ms();
    match shared.store.list(&me, &cursor, limit, state_filter, now) {
        Ok(entries) => {
            let next_cursor = if entries.len() == limit as usize {
                entries.last().map(|e| e.code_id).unwrap_or(CURSOR_END)
            } else {
                CURSOR_END
            };
            deliver(
                out,
                CtrlMessage::InviteListPage {
                    request_id,
                    next_cursor,
                    entries,
                },
            );
        }
        Err(e) => {
            log::error!("list storage failure: {e}");
            deliver(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_revoke(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    request_id: RequestId,
    code_id: CodeId,
    expected_revision: u64,
) {
    let now = now_ms();
    match shared
        .store
        .revoke(&me, &request_id, &code_id, expected_revision, now)
    {
        Ok(RevokeOutcome::Revoked {
            revision,
            revoked_ms,
            aborted,
        }) => {
            deliver(
                out,
                CtrlMessage::InviteRevoked {
                    request_id,
                    code_id,
                    revision,
                    revoked_ms,
                },
            );
            for pairing in aborted {
                notify_pairing_unavailable(shared, &pairing.owner, pairing.pairing_id);
                notify_pairing_unavailable(shared, &pairing.redeemer, pairing.pairing_id);
            }
        }
        Ok(RevokeOutcome::Replay(bytes)) => deliver(out, decode_stored(&bytes)),
        Ok(RevokeOutcome::RequestConflict) => deliver(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(RevokeOutcome::RevisionConflict) => deliver(
            out,
            pair_error(request_id, ErrorScope::Code, code_id, REVISION_CONFLICT, 0),
        ),
        Ok(RevokeOutcome::Unavailable) => {
            // Unknown, foreign, or terminal — one answer.
            deliver(
                out,
                pair_error(request_id, ErrorScope::Code, code_id, PAIR_UNAVAILABLE, 0),
            );
        }
        Err(e) => {
            log::error!("revoke storage failure: {e}");
            deliver(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn notify_pairing_unavailable(shared: &Shared, device: &DeviceId, pairing_id: PairingId) {
    shared.deliver(device, unavailable([0u8; 16], pairing_id));
}

// ---- redeem -------------------------------------------------------------------

fn handle_redeem(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    redeem: crate::protocol_v2::RedeemRequest,
) -> Flow {
    let now_instant = Instant::now();
    // Budget rejections run before the lookup and are immediate.
    if !state.redeem_window.allow_at(now_instant) {
        deliver(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                state.redeem_window.retry_after_ms(now_instant),
            ),
        );
        return Flow::Continue;
    }
    let (global_ok, retry) = shared.global_redeem_allows(now_instant);
    if !global_ok {
        deliver(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                retry,
            ),
        );
        return Flow::Continue;
    }
    let now = now_ms();
    // Deadline sanity (one attempt spans at most 120 s, contract §8).
    if redeem.deadline_ms <= now || redeem.deadline_ms > now + REDEEM_MAX_WINDOW.as_millis() as u64
    {
        deliver(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return Flow::Continue;
    }
    // Signature over the frozen redeem domain (MAC stays endpoint-only).
    let signed = hello2::redeem_message(
        &shared.registry_id,
        &redeem.lookup_digest,
        &me,
        &redeem.request_id,
        &redeem.redeemer_nonce,
        redeem.deadline_ms,
        &redeem.request_mac,
    );
    if !hello2::verify(&me, &signed, &redeem.signature) {
        // A forged redeem request is a protocol violation: disconnect.
        log::info!(
            "redeem from {} rejected: bad signature",
            util::hex_prefix(&me, 4)
        );
        return Flow::Disconnect;
    }
    let code = match shared.store.find_active_code(&redeem.lookup_digest, now) {
        Ok(code) => code,
        Err(e) => {
            log::error!("redeem lookup failure: {e}");
            deliver(
                out,
                pair_error(
                    redeem.request_id,
                    ErrorScope::Request,
                    redeem.request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
            return Flow::Continue;
        }
    };
    let pairing_id = random_id16();
    match code {
        // A live code owned by someone else: the real redeem path.
        Some(code) if code.owner != me => {
            match shared
                .store
                .begin_pairing(&pairing_id, &me, &redeem, &code, now)
            {
                Ok(BeginRedeem::Begun) => {
                    deliver(
                        out,
                        CtrlMessage::InviteRedeemPending {
                            request_id: redeem.request_id,
                            pairing_id,
                            deadline_ms: redeem.deadline_ms,
                        },
                    );
                    // Owner online: offer immediately; offline: the
                    // pending row waits and is pushed on the owner's next
                    // Hello (catch-up in process_hello).
                    shared.deliver(
                        &code.owner,
                        CtrlMessage::InviteRedeemOffered {
                            code_id: code.code_id,
                            code_revision: code.revision,
                            pairing_id,
                            redeemer_device_id: me,
                            redeem,
                        },
                    );
                }
                Ok(BeginRedeem::OwnerPendingLimit | BeginRedeem::GlobalPendingLimit) => {
                    deliver(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            RESOURCE_LIMIT,
                            0,
                        ),
                    );
                }
                Err(e) => {
                    log::error!("begin pairing failure: {e}");
                    deliver(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            STORAGE_UNAVAILABLE,
                            0,
                        ),
                    );
                }
            }
        }
        // Unknown, expired, revoked, or self-redeem: same pending shape,
        // unified failure at the deadline (no existence oracle).
        _ => {
            let mut inner = shared.lock();
            inner.ghosts.insert(
                pairing_id,
                Ghost {
                    requester: me,
                    request_id: redeem.request_id,
                    deadline_ms: redeem.deadline_ms,
                    expires_at: now_instant
                        + Duration::from_millis(redeem.deadline_ms.saturating_sub(now)),
                },
            );
            deliver(
                out,
                CtrlMessage::InviteRedeemPending {
                    request_id: redeem.request_id,
                    pairing_id,
                    deadline_ms: redeem.deadline_ms,
                },
            );
        }
    }
    Flow::Continue
}

// ---- pairing handshakes --------------------------------------------------------

/// Pure transcript-binding check (`pairing-flow.md` §7: key/code
/// substitution detection). The transcript must name exactly the
/// registry, code, ids, keys, nonce, request and deadline the durable
/// row recorded.
fn transcript_bindings_ok(
    typed: &PairTranscript,
    snapshot: &PairingSnapshot,
    registry_id: &RegistryId,
) -> bool {
    typed.registry_id == *registry_id
        && typed.code_id == snapshot.code_id
        && typed.code_revision == snapshot.code_revision
        && typed.pairing_id == snapshot.pairing_id
        && typed.request_id == snapshot.request_id
        && typed.lookup_digest == snapshot.lookup_digest
        && typed.owner_device_id == snapshot.owner
        && typed.redeemer_device_id == snapshot.redeemer
        && typed.redeemer_nonce == snapshot.redeemer_nonce
        && typed.deadline_ms == snapshot.deadline_ms
}

fn handle_pair_challenge(
    shared: &Shared,
    me: DeviceId,
    transcript: [u8; 256],
    owner_proof: PairProof,
) {
    let typed = PairTranscript::from_bytes(&transcript);
    let now = now_ms();
    let snapshot = shared.store.pairing_snapshot(&typed.pairing_id);
    let snapshot = match snapshot {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair challenge for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    // The owner is the only legitimate author of a PairChallenge; wrong
    // sender, wrong state, deadline passed, or any binding mismatch is a
    // protocol failure. No reply distinguishes these (no oracle); the
    // pending transaction simply fails at its deadline.
    let valid_sender = snapshot.owner == me
        && snapshot.state == PairingState::Pending
        && now <= snapshot.deadline_ms;
    if !valid_sender || !transcript_bindings_ok(&typed, &snapshot, &shared.registry_id) {
        log::info!(
            "pair challenge from {} rejected: bindings",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    // Server-verifiable part of the proof: the Ed25519 signature over the
    // frozen owner domain, canonical transcript and MAC. The MAC itself
    // is endpoint-only material and is deliberately not checked here.
    let signed = hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &transcript, &owner_proof.mac);
    if !hello2::verify(&typed.owner_device_id, &signed, &owner_proof.signature) {
        log::info!("pair challenge rejected: owner signature");
        return;
    }
    let digest = hello2::transcript_digest(&transcript);
    match shared.store.store_owner_challenge(
        &typed.pairing_id,
        &me,
        &transcript,
        &digest,
        &owner_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.redeemer,
                CtrlMessage::PairChallenge {
                    transcript,
                    owner_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair challenge lost its race; dropped"),
        Err(e) => log::error!("store owner challenge failure: {e}"),
    }
}

fn handle_pair_proof(
    shared: &Shared,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    redeemer_proof: PairProof,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair proof for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    if snapshot.redeemer != me
        || snapshot.state != PairingState::Pending
        || now > snapshot.deadline_ms
        || snapshot.transcript_digest != Some(transcript_digest)
    {
        log::info!(
            "pair proof from {} rejected: bindings/digest",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    let transcript = match snapshot.transcript {
        Some(transcript) => transcript,
        None => return,
    };
    let signed =
        hello2::proof_signed_message(REDEEMER_PROOF_DOMAIN, &transcript, &redeemer_proof.mac);
    if !hello2::verify(&snapshot.redeemer, &signed, &redeemer_proof.signature) {
        log::info!("pair proof rejected: redeemer signature");
        return;
    }
    match shared.store.store_redeemer_proof(
        &pairing_id,
        &me,
        &transcript_digest,
        &redeemer_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.owner,
                CtrlMessage::PairProof {
                    pairing_id,
                    transcript_digest,
                    redeemer_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair proof lost its race; dropped"),
        Err(e) => log::error!("store redeemer proof failure: {e}"),
    }
}

fn commit_message(snapshot: &PairingSnapshot) -> Option<CtrlMessage> {
    Some(CtrlMessage::PairCommit {
        transcript: snapshot.transcript?,
        owner_proof: snapshot.owner_proof.clone()?,
        redeemer_proof: snapshot.redeemer_proof.clone()?,
        owner_prepared_signature: snapshot.owner_prepared_signature?,
        redeemer_prepared_signature: snapshot.redeemer_prepared_signature?,
        commit_revision: snapshot.commit_revision?,
    })
}

fn ready_message(snapshot: &PairingSnapshot) -> Option<CtrlMessage> {
    Some(CtrlMessage::PairReady {
        pairing_id: snapshot.pairing_id,
        transcript_digest: snapshot.transcript_digest?,
        commit_revision: snapshot.commit_revision?,
        owner_applied_signature: snapshot.owner_applied_signature?,
        redeemer_applied_signature: snapshot.redeemer_applied_signature?,
    })
}

fn handle_prepared(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || now > snapshot.deadline_ms
    {
        deliver(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::prepared_message(&pairing_id, &transcript_digest);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "prepared from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_prepared_and_try_commit(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        now,
    );
    match decision {
        Ok(PreparedDecision::WaitingOther) => {}
        Ok(PreparedDecision::Committed { .. })
        | Ok(PreparedDecision::AlreadyFinal(PairingState::Committed | PairingState::Applied)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = commit_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(PairingState::Ready)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&me, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(_)) => {}
        Ok(PreparedDecision::Unavailable) => {
            // The aborting path (revoke, sweep, explicit abort) already
            // notified both participants; tell only the sender.
            deliver(out, unavailable([0u8; 16], pairing_id));
        }
        Ok(PreparedDecision::ResourceLimit) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    RESOURCE_LIMIT,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
        }
        Ok(PreparedDecision::NotFound) => {
            deliver(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("prepared storage failure: {e}");
            deliver(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_applied(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    commit_revision: u64,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || !matches!(
            snapshot.state,
            PairingState::Committed | PairingState::Applied | PairingState::Ready
        )
    {
        deliver(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::applied_message(&pairing_id, &transcript_digest, commit_revision);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "applied from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_applied(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        commit_revision,
        now,
    );
    match decision {
        Ok(AppliedDecision::WaitingOther) => {}
        Ok(AppliedDecision::Ready) | Ok(AppliedDecision::AlreadyReady) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(AppliedDecision::RevisionConflict) => {
            deliver(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AppliedDecision::NotFound) => {
            deliver(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("applied storage failure: {e}");
            deliver(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_status_query(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        // Unknown or non-participant: one unified answer.
        Ok(None) | Err(_) => {
            deliver(out, unavailable(request_id, pairing_id));
            return;
        }
    };
    if snapshot.owner != me && snapshot.redeemer != me {
        deliver(out, unavailable(request_id, pairing_id));
        return;
    }
    deliver(
        out,
        CtrlMessage::PairStatusReply {
            request_id,
            pairing_id,
            state: snapshot.state,
            commit_revision: snapshot.commit_revision.unwrap_or(0),
            deadline_ms: snapshot.deadline_ms,
        },
    );
    // Recovery resends after the reply (contract §6): committed
    // transactions re-deliver the fixed PairCommit, ready ones the fixed
    // PairReady.
    match snapshot.state {
        PairingState::Committed | PairingState::Applied => {
            if let Some(message) = commit_message(&snapshot) {
                deliver(out, message);
            }
        }
        PairingState::Ready => {
            if let Some(message) = ready_message(&snapshot) {
                deliver(out, message);
            }
        }
        _ => {}
    }
}

fn handle_abort(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // Participants only; unknown pairings and foreign ones look the same.
    let snapshot = shared.store.pairing_snapshot(&pairing_id);
    let participant = matches!(&snapshot, Ok(Some(s)) if s.owner == me || s.redeemer == me);
    if !participant {
        deliver(out, unavailable(request_id, pairing_id));
        return;
    }
    match shared.store.abort_pairing(&pairing_id, now_ms()) {
        Ok(AbortDecision::Aborted(aborted)) => {
            // The other participant learns the pairing is gone; the
            // aborter gets silence (idempotent success).
            let other = if aborted.owner == me {
                aborted.redeemer
            } else {
                aborted.owner
            };
            shared.deliver(&other, unavailable([0u8; 16], pairing_id));
        }
        Ok(AbortDecision::NotAbortable) => {
            // Committed transactions must be recovered, not dropped.
            deliver(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AbortDecision::NotFound) => {
            deliver(out, unavailable(request_id, pairing_id));
        }
        Err(e) => log::error!("abort storage failure: {e}"),
    }
}

// ---- v2 sessions (contract §10) ------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_connect(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    target: DeviceId,
    pairing_id: PairingId,
    code_id: CodeId,
    connection_id: [u8; 16],
) {
    if target == me {
        deliver(out, server_error(ERR_BAD_REQUEST, "self connect"));
        return;
    }
    // v2 sessions are keyed by the verified triple: a completed pairing
    // relation must exist for exactly this (pairing_id, code_id) and this
    // endpoint pair.
    let verified = shared
        .store
        .relation_exists(&pairing_id, &code_id, &me, &target)
        .unwrap_or(false);
    if !verified {
        deliver(
            out,
            pair_error(
                [0u8; 16],
                ErrorScope::Connection,
                connection_id,
                PAIR_UNAVAILABLE,
                0,
            ),
        );
        return;
    }
    let mut inner = shared.lock();
    if !inner.conns.contains_key(&target) {
        drop(inner);
        deliver(
            out,
            server_error(ERR_UNKNOWN_DEVICE, "target device not online"),
        );
        return;
    }
    let triple = (pairing_id, code_id, connection_id);
    // Idempotent: an existing session for the same triple is re-announced.
    if let Some(existing) = inner.session_by_triple.get(&triple) {
        if let Some(entry) = inner.sessions.get(existing) {
            let entry = entry.clone();
            let session = *existing;
            drop(inner);
            announce_session(shared, &entry, session, me, target);
            return;
        }
    }
    let my_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&me))
        .count();
    let target_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&target))
        .count();
    if my_count >= shared.limits.max_sessions_per_device
        || target_count >= shared.limits.max_sessions_per_device
    {
        drop(inner);
        deliver(
            out,
            server_error(ERR_SESSION_LIMIT, "session limit reached"),
        );
        return;
    }
    let mut session_id = random_id16();
    for _ in 0..8 {
        if !inner.sessions.contains_key(&session_id) {
            break;
        }
        session_id = random_id16();
    }
    let entry = SessionEntry {
        triple,
        endpoints: [me, target],
        expires_at: Instant::now() + shared.limits.session_ttl,
    };
    inner.sessions.insert(session_id, entry.clone());
    inner.session_by_triple.insert(triple, session_id);
    drop(inner);
    announce_session(shared, &entry, session_id, me, target);
}

fn announce_session(
    shared: &Shared,
    session_entry: &SessionEntry,
    session: SessionId,
    me: DeviceId,
    target: DeviceId,
) {
    let expires_ms = now_ms() + shared.limits.session_ttl.as_millis() as u64;
    let (pairing_id, code_id, connection_id) = session_entry.triple;
    for (device, peer) in [(me, target), (target, me)] {
        shared.deliver(
            &device,
            CtrlMessage::SessionOpened {
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            },
        );
    }
    // SameEgress hint: both endpoints' control-plane peer IPs known and
    // identical (ordering hint only, never a trust claim).
    let inner = shared.lock();
    let me_ip = inner.conns.get(&me).map(|entry| entry.peer_ip);
    let target_ip = inner.conns.get(&target).map(|entry| entry.peer_ip);
    drop(inner);
    if crate::server::same_egress(me_ip, target_ip) {
        for device in [me, target] {
            shared.deliver(
                &device,
                CtrlMessage::Signal {
                    session,
                    signal: crate::protocol_v2::PeerSignal::SameEgress,
                },
            );
        }
    }
}

fn handle_signal(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    session: SessionId,
    signal: crate::protocol_v2::PeerSignal,
) {
    let inner = shared.lock();
    let Some(entry) = inner.sessions.get(&session) else {
        drop(inner);
        deliver(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    };
    if !entry.endpoints.contains(&me) {
        drop(inner);
        // Unknown and non-member sessions look identical (no oracle).
        deliver(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    }
    let other = if entry.endpoints[0] == me {
        entry.endpoints[1]
    } else {
        entry.endpoints[0]
    };
    drop(inner);
    // Verbatim relay of one bounded, structured signal.
    shared.deliver(&other, CtrlMessage::Signal { session, signal });
}

fn handle_cancel(shared: &Shared, me: DeviceId, session: SessionId) {
    let mut inner = shared.lock();
    let mine = inner
        .sessions
        .get(&session)
        .map(|entry| entry.endpoints.contains(&me))
        .unwrap_or(false);
    if mine {
        if let Some(entry) = inner.sessions.remove(&session) {
            inner.session_by_triple.remove(&entry.triple);
        }
    }
    // Cancelling an unknown session is idempotent, not an error.
}

// ---- sweeper --------------------------------------------------------------------

fn sweep(shared: &Shared) {
    let now = now_ms();
    let mut expired_ghosts: Vec<Ghost> = Vec::new();
    {
        let mut inner = shared.lock();
        let due: Vec<PairingId> = inner
            .ghosts
            .iter()
            .filter(|(_, ghost)| ghost.expires_at <= Instant::now())
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            if let Some(ghost) = inner.ghosts.remove(&id) {
                expired_ghosts.push(ghost);
            }
        }
        // Session TTLs (coordination sessions only; data channels are
        // unaffected by coordination-session death).
        let dead: Vec<SessionId> = inner
            .sessions
            .iter()
            .filter(|(_, session)| session.expires_at <= Instant::now())
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            if let Some(entry) = inner.sessions.remove(&id) {
                inner.session_by_triple.remove(&entry.triple);
            }
        }
    }
    for ghost in expired_ghosts {
        log::debug!(
            "ghost redeem expired at its {} ms deadline",
            ghost.deadline_ms
        );
        shared.deliver(
            &ghost.requester,
            pair_error(
                ghost.request_id,
                ErrorScope::Pairing,
                [0u8; 16],
                PAIR_UNAVAILABLE,
                0,
            ),
        );
    }
    let report: DueReport = match shared.store.expire_due(now) {
        Ok(report) => report,
        Err(e) => {
            log::error!("sweep expiry failure: {e}");
            return;
        }
    };
    for aborted in report.aborted {
        notify_pairing_unavailable(shared, &aborted.owner, aborted.pairing_id);
        notify_pairing_unavailable(shared, &aborted.redeemer, aborted.pairing_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_replay_cache_admits_once_per_window() {
        let mut cache = HelloReplayCache::default();
        let t0 = Instant::now();
        let window = Duration::from_secs(10);
        let digest = [0xAB; 32];
        assert!(cache.admit(digest, t0, window));
        assert!(!cache.admit(digest, t0 + Duration::from_secs(1), window));
        assert!(cache.admit(digest, t0 + Duration::from_secs(11), window));
    }

    #[test]
    fn transcript_bindings_reject_key_and_code_substitution() {
        let registry = [0x11u8; 32];
        let snapshot = PairingSnapshot {
            pairing_id: [0x22; 16],
            request_id: [0x33; 16],
            code_id: [0x44; 16],
            lookup_digest: [0x55; 32],
            owner: [0x66; 32],
            redeemer: [0x77; 32],
            state: PairingState::Pending,
            deadline_ms: 9_000,
            code_revision: 1,
            commit_revision: None,
            transcript: None,
            transcript_digest: None,
            redeemer_nonce: [0x88; 32],
            owner_proof: None,
            redeemer_proof: None,
            owner_prepared_signature: None,
            redeemer_prepared_signature: None,
            owner_applied_signature: None,
            redeemer_applied_signature: None,
        };
        let typed = PairTranscript {
            registry_id: registry,
            code_id: snapshot.code_id,
            code_revision: 1,
            pairing_id: snapshot.pairing_id,
            request_id: snapshot.request_id,
            lookup_digest: snapshot.lookup_digest,
            owner_device_id: snapshot.owner,
            redeemer_device_id: snapshot.redeemer,
            owner_nonce: [0; 32],
            redeemer_nonce: snapshot.redeemer_nonce,
            deadline_ms: 9_000,
        };
        assert!(transcript_bindings_ok(&typed, &snapshot, &registry));
        // Key substitution: swapped redeemer key.
        let mut bad = typed;
        bad.redeemer_device_id = [0x99; 32];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        // Code substitution: foreign code id / lookup digest / revision.
        let mut bad = typed;
        bad.code_id = [0xAA; 16];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        let mut bad = typed;
        bad.lookup_digest = [0xBB; 32];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        let mut bad = typed;
        bad.code_revision = 2;
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        // Wrong registry (malicious server substitution).
        let mut bad = typed;
        bad.registry_id = [0x00; 32];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        // Wrong nonce / request / deadline.
        let mut bad = typed;
        bad.redeemer_nonce = [1; 32];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        let mut bad = typed;
        bad.request_id = [2; 16];
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
        let mut bad = typed;
        bad.deadline_ms = 8_999;
        assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    }
}
