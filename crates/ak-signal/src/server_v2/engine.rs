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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairTranscript, OWNER_PROOF_DOMAIN, REDEEMER_PROOF_DOMAIN,
};
use ak_identity::DeviceKeyPair;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::protocol_v2::{
    CodeId, CtrlFrame, CtrlMessage, DeviceId, Digest32, EndpointBlob, ErrorScope, PairProof,
    PairingId, PairingState, RedeemRequest, RegistryId, RequestId, Role, SessionId, Signature64,
    StateFilter, CODE_CONFLICT, CURSOR_END, PAIR_UNAVAILABLE, PROTOCOL_UNSUPPORTED, RATE_LIMITED,
    REDEEM_EXHAUSTED, REQUEST_CONFLICT, RESOURCE_LIMIT, REVISION_CONFLICT, STORAGE_UNAVAILABLE,
};
use crate::server_v2::config::{
    Config, Limits, JOURNAL_MAX_ROWS, OUTBOX_DEPTH, REDEEM_MAX_WINDOW, SWEEP_INTERVAL,
};
use crate::server_v2::framing::{read_frame, write_frame, write_message};
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

// ---- resource bounds (P1-5) ---------------------------------------------------
//
// Every number here is a hard bound on one dimension of connection
// sprawl: a peer that opens sockets and never speaks, one that handshakes
// and never says Hello, one that floods frames, and one that stalls a
// read or write mid-frame each lose their task within the matching
// budget instead of pinning memory and a Tokio task forever.

/// Budget for one TLS handshake after the TCP accept.
const TLS_ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum simultaneously served TLS connections (accepted-and-counted,
/// including handshakes in progress); beyond it the handshake completes
/// and the peer receives one `ServerError` before the disconnect.
const MAX_LIVE_CONNECTIONS: usize = 256;
/// Deadline for the client's first frame — the `Hello` answering the
/// `AuthChallenge` this server sends immediately after the handshake.
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(15);
/// Per-frame read and write budget once the first frame arrived.
const FRAME_IO_DEADLINE: Duration = Duration::from_secs(30);
/// Grace period for the reader/writer tasks of a connection to wind down
/// on their own before the handler aborts them.
const TEARDOWN_GRACE: Duration = Duration::from_millis(250);
/// How long a redeemed-attempt idempotency record stays replayable after
/// its own deadline (retries racing the deadline must still dedup).
const REDEEM_REPLAY_GRACE: Duration = Duration::from_secs(60);

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
        conn_count: AtomicUsize::new(0),
    });
    log::info!(
        "v2 registry server listening on tls {} (registry {})",
        listener.local_addr()?,
        util::hex_prefix(&shared.registry_id, 4)
    );
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                // P1-5: no connection task may outlive the server. First
                // signal every live connection's close watch (reader,
                // writer and handler all subscribe — a Notify would wake
                // only one of them), then abort whatever is left and join
                // bounded before returning.
                let tasks: Vec<JoinHandle<()>> = {
                    let mut inner = shared.lock();
                    for entry in inner.conns.values() {
                        let _ = entry.close.send(true);
                    }
                    inner.conn_tasks.drain().map(|(_, task)| task).collect()
                };
                for mut task in tasks {
                    task.abort();
                    let _ = tokio::time::timeout(TEARDOWN_GRACE, &mut task).await;
                }
                return Ok(());
            }
            _ = tick.tick() => sweep(&shared),
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    let _ = tcp.set_nodelay(true);
                    // Reserve a live-connection slot at accept time: the
                    // cap must also bound half-open handshakes.
                    let admitted = shared
                        .conn_count
                        .fetch_add(1, Ordering::SeqCst)
                        < MAX_LIVE_CONNECTIONS;
                    if !admitted {
                        shared.conn_count.fetch_sub(1, Ordering::SeqCst);
                    }
                    let task_id = admitted.then(|| {
                        let mut inner = shared.lock();
                        inner.next_conn_task_id = inner.next_conn_task_id.wrapping_add(1);
                        inner.next_conn_task_id
                    });
                    let conn_shared = shared.clone();
                    let task = tokio::spawn(async move {
                        // P1-5: the TLS handshake itself is bounded.
                        let mut stream = match tokio::time::timeout(
                            TLS_ACCEPT_TIMEOUT,
                            conn_shared.acceptor.accept(tcp),
                        )
                        .await
                        {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(e)) => {
                                log::debug!("v2 tls handshake with {peer} failed: {e}");
                                return;
                            }
                            Err(_) => {
                                log::debug!(
                                    "v2 tls handshake with {peer} exceeded {:?}",
                                    TLS_ACCEPT_TIMEOUT
                                );
                                return;
                            }
                        };
                        if tls::check_alpn(stream.get_ref().1.alpn_protocol()).is_err() {
                            log::debug!("v2 connection from {peer} without ALPN ak-signal/2");
                            return;
                        }
                        match task_id {
                            Some(id) => {
                                connection_loop(stream, peer, Arc::clone(&conn_shared)).await;
                                conn_shared.conn_count.fetch_sub(1, Ordering::SeqCst);
                                let mut inner = conn_shared.lock();
                                inner.conn_tasks.remove(&id);
                            }
                            None => {
                                // Over the global cap: one bounded
                                // ServerError, then the connection dies.
                                log::warn!(
                                    "v2 connection limit ({MAX_LIVE_CONNECTIONS}) reached; rejecting {peer}"
                                );
                                let reply =
                                    server_error(ERR_SESSION_LIMIT, "connection limit reached");
                                let sent = tokio::time::timeout(
                                    FRAME_IO_DEADLINE,
                                    write_message(&mut stream, &reply, Role::Server),
                                )
                                .await;
                                if matches!(sent, Ok(Ok(()))) {
                                    let _ = stream.shutdown().await;
                                }
                            }
                        }
                    });
                    if let Some(id) = task_id {
                        shared.lock().conn_tasks.insert(id, task);
                    }
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
    out: mpsc::Sender<CtrlFrame>,
    /// Close signal of this connection (true = torn down). Shared handle:
    /// the handler owns one, the registry keeps this one so a replacement
    /// Hello (or the server shutdown) can tear the connection down. A
    /// watch, not a Notify — reader, writer and handler all wait on it
    /// and `send(true)` wakes every one of them.
    close: Arc<watch::Sender<bool>>,
    candidates: Vec<SocketAddr>,
    peer_ip: IpAddr,
}

/// Resolve once the close watch fires (true) or its sender is gone
/// (aborted handler): either way this connection is over.
async fn closed(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
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
    /// Redeem idempotency records keyed by the authenticated
    /// (redeemer, request_id) — the in-memory twin of the store's
    /// `request_journal` for claim/revoke (same shape, bounded by
    /// [`JOURNAL_MAX_ROWS`], pruned past deadline + grace).
    redeem_journal: HashMap<(DeviceId, RequestId), RedeemReplay>,
    next_conn_id: u64,
    /// Registry of live connection tasks so shutdown can abort them all
    /// (P1-5); the strict live-connection cap is `Shared::conn_count`,
    /// this map is the abort list. A task that exits in the tiny window
    /// before its handle is registered leaves a finished handle behind —
    /// harmless (abort on a finished task is a no-op) and swept at
    /// shutdown.
    conn_tasks: HashMap<u64, JoinHandle<()>>,
    next_conn_task_id: u64,
    hello_replay: HelloReplayCache,
    global_redeem: Option<SlidingWindow>,
}

/// One journaled redeem attempt for idempotent retries: the response is a
/// pure function of these fields, so a retry replays byte-identical
/// `InviteRedeemPending` frames with the ORIGINAL pairing id.
#[derive(Clone, Copy)]
struct RedeemReplay {
    lookup_digest: Digest32,
    redeemer_nonce: [u8; 32],
    request_mac: [u8; 32],
    deadline_ms: u64,
    pairing_id: PairingId,
    expires_at: Instant,
}

impl RedeemReplay {
    /// Same redeem attempt (everything the signature binds)? A different
    /// attempt reusing the request id is a `REQUEST_CONFLICT`, exactly as
    /// the claim journal treats it.
    fn matches(&self, redeem: &RedeemRequest) -> bool {
        self.lookup_digest == redeem.lookup_digest
            && self.redeemer_nonce == redeem.redeemer_nonce
            && self.request_mac == redeem.request_mac
            && self.deadline_ms == redeem.deadline_ms
    }
}

struct Shared {
    registry_id: RegistryId,
    acceptor: tokio_rustls::TlsAcceptor,
    store: RegistryStore,
    limits: Limits,
    inner: Mutex<Inner>,
    /// Live (accepted, not yet finished) connections — the admission
    /// counter for [`MAX_LIVE_CONNECTIONS`], reserved atomically at
    /// accept time and released when the connection task exits.
    conn_count: AtomicUsize,
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
    /// Accepts a full frame or a plain message (`From<CtrlMessage>`).
    fn deliver<F: Into<CtrlFrame>>(&self, device: &DeviceId, frame: F) {
        let tx = {
            let inner = self.lock();
            inner.conns.get(device).map(|entry| entry.out.clone())
        };
        match tx {
            Some(tx) => deliver(&tx, frame.into()),
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

fn deliver<F: Into<CtrlFrame>>(tx: &mpsc::Sender<CtrlFrame>, frame: F) {
    if let Err(e) = tx.try_send(frame.into()) {
        log::debug!("outbound queue full or closed; message dropped: {e}");
    }
}

/// Free-function convenience of [`deliver`] for plain messages (the
/// wave-28 extension stays empty).
fn deliver_msg(tx: &mpsc::Sender<CtrlFrame>, message: CtrlMessage) {
    deliver(tx, CtrlFrame::new(message));
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
    close: Arc<watch::Sender<bool>>,
    peer_ip: IpAddr,
    challenge: Option<[u8; 32]>,
    challenge_expires_ms: u64,
    exporter: [u8; 32],
    redeem_window: SlidingWindow,
}

async fn connection_loop(stream: TlsServerStream, peer: SocketAddr, shared: Arc<Shared>) {
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
async fn reader_loop<R: AsyncRead + Unpin>(
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
async fn writer_loop<W: AsyncWrite + Unpin>(
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
        let sent = tokio::time::timeout(
            write_budget,
            write_frame(&mut write, &frame, Role::Server),
        )
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            log::debug!("v2 outbound write stalled or failed; closing connection");
            // Tell the handler this connection is dead (best-effort: it
            // may already be tearing down).
            let _ = dead.try_send(());
            return;
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

// ---- hello ------------------------------------------------------------------

fn process_hello(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
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
                let _ = previous.close.send(true);
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
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "replay suspected"));
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
    deliver_msg(
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
                let mut frame = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                    code_id: offer.code_id,
                    code_revision: offer.code_revision,
                    pairing_id: offer.pairing_id,
                    redeemer_device_id: offer.redeemer,
                    redeem: offer.redeem,
                });
                if let Some(blob) = offer.redeemer_endpoint {
                    frame = frame.with_endpoint(EndpointBlob(blob));
                }
                shared.deliver(me, frame);
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

#[allow(clippy::too_many_arguments)]
fn handle_claim(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    lookup_digest: Digest32,
    ttl_secs: u32,
    owner_endpoint: Option<EndpointBlob>,
    max_redeems: u32,
) {
    if ttl_secs != 0
        && !(crate::server_v2::config::INVITE_TTL_MIN_SECS
            ..=crate::server_v2::config::INVITE_TTL_MAX_SECS)
            .contains(&ttl_secs)
    {
        deliver_msg(
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
    match shared.store.claim(
        &me,
        &request_id,
        &lookup_digest,
        ttl_secs,
        max_redeems,
        owner_endpoint.as_ref().map(|blob| blob.0.as_slice()),
        now,
    )
    {
        Ok(ClaimOutcome::Claimed {
            code_id,
            created_ms,
            expires_ms,
            revision,
        }) => {
            deliver_msg(
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
        Ok(ClaimOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(ClaimOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::CodeConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                CODE_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::RateLimited) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RATE_LIMITED,
                60_000,
            ),
        ),
        Ok(ClaimOutcome::ResourceLimit) => deliver_msg(
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
            deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
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
            deliver_msg(
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
            deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
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
            deliver_msg(
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
        Ok(RevokeOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(RevokeOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(RevokeOutcome::RevisionConflict) => deliver_msg(
            out,
            pair_error(request_id, ErrorScope::Code, code_id, REVISION_CONFLICT, 0),
        ),
        Ok(RevokeOutcome::Unavailable) => {
            // Unknown, foreign, or terminal — one answer.
            deliver_msg(
                out,
                pair_error(request_id, ErrorScope::Code, code_id, PAIR_UNAVAILABLE, 0),
            );
        }
        Err(e) => {
            log::error!("revoke storage failure: {e}");
            deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    redeem: crate::protocol_v2::RedeemRequest,
    redeemer_endpoint: Option<EndpointBlob>,
) -> Flow {
    let now_instant = Instant::now();
    // Budget rejections run before the lookup and are immediate.
    if !state.redeem_window.allow_at(now_instant) {
        deliver_msg(
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
        deliver_msg(
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
        deliver_msg(
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
    // P2 (redeem 幂等): a retry with the same (redeemer, request_id)
    // replays the ORIGINAL response — minting a fresh pairing id per
    // retry forked the transaction (a second pending row for a real
    // code, a second ghost otherwise, and a second offer pushed to the
    // owner). Same journal discipline as the invite claim path; a
    // different attempt reusing the id is a REQUEST_CONFLICT.
    {
        let inner = shared.lock();
        let replay = inner.redeem_journal.get(&(me, redeem.request_id)).copied();
        match replay {
            Some(flags) if flags.matches(&redeem) => {
                drop(inner);
                deliver_msg(
                    out,
                    CtrlMessage::InviteRedeemPending {
                        request_id: redeem.request_id,
                        pairing_id: flags.pairing_id,
                        deadline_ms: flags.deadline_ms,
                    },
                );
                return Flow::Continue;
            }
            Some(_) => {
                drop(inner);
                deliver_msg(
                    out,
                    pair_error(
                        redeem.request_id,
                        ErrorScope::Request,
                        redeem.request_id,
                        REQUEST_CONFLICT,
                        0,
                    ),
                );
                return Flow::Continue;
            }
            None => {}
        }
    }
    let code = match shared.store.find_active_code(&redeem.lookup_digest, now) {
        Ok(code) => code,
        Err(e) => {
            log::error!("redeem lookup failure: {e}");
            deliver_msg(
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
            match shared.store.begin_pairing(
                &pairing_id,
                &me,
                &redeem,
                &code,
                redeemer_endpoint.as_ref().map(|blob| blob.0.as_slice()),
                now,
            ) {
                Ok(BeginRedeem::Begun) => {
                    let expires_at =
                        now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
                    record_redeem_replay(
                        &mut shared.lock(),
                        &me,
                        &redeem,
                        pairing_id,
                        expires_at + REDEEM_REPLAY_GRACE,
                    );
                    deliver_msg(
                        out,
                        CtrlMessage::InviteRedeemPending {
                            request_id: redeem.request_id,
                            pairing_id,
                            deadline_ms: redeem.deadline_ms,
                        },
                    );
                    // Owner online: offer immediately; offline: the
                    // pending row waits and is pushed on the owner's next
                    // Hello (catch-up in process_hello). Wave-28: the
                    // redeemer's endpoint rides the offer frame, so the
                    // owner has the peer settings in this one push.
                    let mut offer = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                        code_id: code.code_id,
                        code_revision: code.revision,
                        pairing_id,
                        redeemer_device_id: me,
                        redeem,
                    });
                    if let Some(blob) = redeemer_endpoint {
                        offer = offer.with_endpoint(blob);
                    }
                    shared.deliver(&code.owner, offer);
                }
                Ok(BeginRedeem::OwnerPendingLimit | BeginRedeem::GlobalPendingLimit) => {
                    deliver_msg(
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
                // Wave-28 single-redemption cap: immediate, explicit —
                // no deferred deadline semantics (the code is known to
                // exist; the redeemer already holds it).
                Ok(BeginRedeem::RedeemExhausted) => {
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Code,
                            code.code_id,
                            REDEEM_EXHAUSTED,
                            0,
                        ),
                    );
                }
                Err(e) => {
                    log::error!("begin pairing failure: {e}");
                    deliver_msg(
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
            let expires_at =
                now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
            {
                let mut inner = shared.lock();
                inner.ghosts.insert(
                    pairing_id,
                    Ghost {
                        requester: me,
                        request_id: redeem.request_id,
                        deadline_ms: redeem.deadline_ms,
                        expires_at,
                    },
                );
                record_redeem_replay(
                    &mut inner,
                    &me,
                    &redeem,
                    pairing_id,
                    expires_at + REDEEM_REPLAY_GRACE,
                );
            }
            deliver_msg(
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

/// Bounded insert into the redeem idempotency journal: expired entries
/// leave first, then arbitrary victims until there is room (the cap is
/// [`JOURNAL_MAX_ROWS`], same bound as the store's journal).
fn record_redeem_replay(
    inner: &mut Inner,
    redeemer: &DeviceId,
    redeem: &RedeemRequest,
    pairing_id: PairingId,
    expires_at: Instant,
) {
    let entry = RedeemReplay {
        lookup_digest: redeem.lookup_digest,
        redeemer_nonce: redeem.redeemer_nonce,
        request_mac: redeem.request_mac,
        deadline_ms: redeem.deadline_ms,
        pairing_id,
        expires_at,
    };
    inner
        .redeem_journal
        .retain(|_, existing| existing.expires_at > Instant::now());
    while inner.redeem_journal.len() >= JOURNAL_MAX_ROWS {
        let victim = *inner
            .redeem_journal
            .keys()
            .next()
            .expect("len checked above");
        inner.redeem_journal.remove(&victim);
    }
    inner
        .redeem_journal
        .insert((*redeemer, redeem.request_id), entry);
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

/// The authoritative commit frame. Wave-28: the owner endpoint snapshot
/// rides the extension section, so the redeemer receives the owner's
/// connection settings at the same commit point (identical frame to both
/// participants; the owner merely sees its own blob again).
fn commit_message(snapshot: &PairingSnapshot) -> Option<CtrlFrame> {
    let mut frame = CtrlFrame::new(CtrlMessage::PairCommit {
        transcript: snapshot.transcript?,
        owner_proof: snapshot.owner_proof.clone()?,
        redeemer_proof: snapshot.redeemer_proof.clone()?,
        owner_prepared_signature: snapshot.owner_prepared_signature?,
        redeemer_prepared_signature: snapshot.redeemer_prepared_signature?,
        commit_revision: snapshot.commit_revision?,
    });
    if let Some(blob) = &snapshot.owner_endpoint {
        frame = frame.with_endpoint(EndpointBlob(blob.clone()));
    }
    Some(frame)
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
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || now > snapshot.deadline_ms
    {
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::prepared_message(&pairing_id, &transcript_digest);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "prepared from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
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
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Ok(PreparedDecision::RedeemExhausted) => {
            // P1-03: the cap bound at the commit linearization point — a
            // competing pending won this code's last redeem slot. The
            // store aborted this pairing without consuming the cap; both
            // participants get the explicit failure. Wave-30 (recheck
            // R29-P2-01): the error is PAIRING-scoped with THIS pairing's
            // id — the production RegistryClient dispatch routes
            // pairing-scope errors to the pairing inbox/plane and the
            // orchestrator's wait_frame terminates on
            // object_id == pairing_id, so the losing flow actually
            // receives the terminal 0x2109 instead of waiting out its
            // deadline (a Code-scoped frame with the all-zero request id
            // had no waiter and no fan-out — silently dropped).
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REDEEM_EXHAUSTED,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
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
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("prepared storage failure: {e}");
            deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
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
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
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
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::applied_message(&pairing_id, &transcript_digest, commit_revision);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "applied from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
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
            deliver_msg(
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
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("applied storage failure: {e}");
            deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // P2 (反枚举): a live ghost of THIS requester answers with the exact
    // `PairStatusReply` shape a real pending transaction produces.
    // Before, the ghost fell straight through to the SQLite lookup, hit
    // `no row`, and answered `PAIR_UNAVAILABLE` immediately — one status
    // query right after `InviteRedeemPending` told a redeemer whether the
    // code existed, defeating the unified-deadline design. Ghost and real
    // now share one answer shape while the attempt is live; after the
    // deadline the ghost is gone and the same single SQLite query yields
    // the unified `PAIR_UNAVAILABLE` for both.
    {
        let inner = shared.lock();
        let live_deadline = inner
            .ghosts
            .get(&pairing_id)
            .filter(|ghost| ghost.requester == me && ghost.expires_at > Instant::now())
            .map(|ghost| ghost.deadline_ms);
        if let Some(deadline_ms) = live_deadline {
            drop(inner);
            deliver_msg(
                out,
                CtrlMessage::PairStatusReply {
                    request_id,
                    pairing_id,
                    state: PairingState::Pending,
                    commit_revision: 0,
                    deadline_ms,
                },
            );
            return;
        }
    }
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        // Unknown or non-participant: one unified answer.
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable(request_id, pairing_id));
            return;
        }
    };
    if snapshot.owner != me && snapshot.redeemer != me {
        deliver_msg(out, unavailable(request_id, pairing_id));
        return;
    }
    deliver_msg(
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
            if let Some(frame) = commit_message(&snapshot) {
                deliver(out, frame);
            }
        }
        PairingState::Ready => {
            if let Some(frame) = ready_message(&snapshot) {
                deliver(out, frame);
            }
        }
        _ => {}
    }
}

fn handle_abort(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // Participants only; unknown pairings and foreign ones look the same.
    let snapshot = shared.store.pairing_snapshot(&pairing_id);
    let participant = matches!(&snapshot, Ok(Some(s)) if s.owner == me || s.redeemer == me);
    if !participant {
        deliver_msg(out, unavailable(request_id, pairing_id));
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
            deliver_msg(
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
            deliver_msg(out, unavailable(request_id, pairing_id));
        }
        Err(e) => log::error!("abort storage failure: {e}"),
    }
}

// ---- v2 sessions (contract §10) ------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_connect(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    target: DeviceId,
    pairing_id: PairingId,
    code_id: CodeId,
    connection_id: [u8; 16],
) {
    if target == me {
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "self connect"));
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
        deliver_msg(
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
        deliver_msg(
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
        deliver_msg(
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
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    session: SessionId,
    signal: crate::protocol_v2::PeerSignal,
) {
    let inner = shared.lock();
    let Some(entry) = inner.sessions.get(&session) else {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    };
    if !entry.endpoints.contains(&me) {
        drop(inner);
        // Unknown and non-member sessions look identical (no oracle).
        deliver_msg(
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
        // Redeem replay records live only until deadline + grace.
        inner
            .redeem_journal
            .retain(|_, replay| replay.expires_at > Instant::now());
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
    use crate::server_v2::tls;

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
            owner_endpoint: None,
            redeemer_endpoint: None,
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

    // ---- integration harness (P1-5 / P2 regression tests) -------------------

    struct TestServer {
        addr: SocketAddr,
        registry: DeviceKeyPair,
        shutdown: watch::Sender<bool>,
        task: JoinHandle<io::Result<()>>,
    }

    async fn spawn_server() -> TestServer {
        let store = RegistryStore::open_in_memory().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = DeviceKeyPair::generate();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let key = registry.clone();
        let task = tokio::spawn(async move {
            serve_v2(listener, &key, store, Limits::default(), shutdown_rx).await
        });
        TestServer {
            addr,
            registry,
            shutdown,
            task,
        }
    }

    impl TestServer {
        fn registry_id(&self) -> RegistryId {
            self.registry.public_key()
        }

        async fn stop(self) {
            let _ = self.shutdown.send(true);
            let joined = tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .expect("serve_v2 returns after shutdown (bounded abort+join)");
            joined.expect("task join").expect("serve_v2 exits Ok");
        }
    }

    struct TestClient {
        stream: tokio_rustls::client::TlsStream<TcpStream>,
        key: DeviceKeyPair,
    }

    impl TestClient {
        async fn connect(
            addr: SocketAddr,
            registry_id: &RegistryId,
            key: DeviceKeyPair,
            name: &str,
        ) -> TestClient {
            let mut stream = tls::connect_pinned(addr, registry_id)
                .await
                .expect("pinned tls handshake");
            let bytes = read_frame(&mut stream)
                .await
                .unwrap()
                .expect("auth challenge");
            let CtrlMessage::AuthChallenge {
                registry_id,
                nonce,
                expires_ms,
            } = CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
            else {
                panic!("first server frame must be AuthChallenge");
            };
            assert!(expires_ms > now_ms());
            let exporter = hello2::exporter_secret_client(stream.get_ref().1).unwrap();
            let timestamp = now_ms();
            let signed = hello2::hello2_message(
                &registry_id,
                &exporter,
                u16::from(crate::protocol_v2::VERSION),
                &key.public_key(),
                name,
                timestamp,
                &nonce,
            );
            write_message(
                &mut stream,
                &CtrlMessage::Hello {
                    version: u16::from(crate::protocol_v2::VERSION),
                    device_id: key.public_key(),
                    name: name.to_owned(),
                    timestamp_ms: timestamp,
                    challenge: nonce,
                    signature: key.sign(&signed),
                },
                Role::Client,
            )
            .await
            .unwrap();
            let mut client = TestClient { stream, key };
            match client.recv().await {
                CtrlMessage::Welcome { .. } => client,
                other => panic!("expected Welcome, got {other:?}"),
            }
        }

        async fn send(&mut self, message: &CtrlMessage) {
            write_message(&mut self.stream, message, Role::Client)
                .await
                .unwrap();
        }

        async fn recv(&mut self) -> CtrlMessage {
            let deadline = Duration::from_secs(10);
            tokio::time::timeout(deadline, async {
                let bytes = read_frame(&mut self.stream).await.unwrap().expect("frame");
                CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
            })
            .await
            .expect("frame within deadline")
        }
    }

    /// A signed redeem request over an arbitrary lookup digest (the server
    /// verifies the signature, never the MAC).
    fn signed_redeem(
        key: &DeviceKeyPair,
        registry_id: &RegistryId,
        lookup: Digest32,
        request_id: RequestId,
        deadline: u64,
    ) -> RedeemRequest {
        let redeemer_nonce = random_nonce32();
        let request_mac = [0x5A; 32];
        let signature = key.sign(&hello2::redeem_message(
            registry_id,
            &lookup,
            &key.public_key(),
            &request_id,
            &redeemer_nonce,
            deadline,
            &request_mac,
        ));
        RedeemRequest {
            request_id,
            lookup_digest: lookup,
            redeemer_nonce,
            deadline_ms: deadline,
            request_mac,
            signature,
        }
    }

    // ---- P2 反枚举 -----------------------------------------------------------

    /// A status query for one's own LIVE ghost answers with the exact
    /// `PairStatusReply(Pending)` shape a real pending transaction gets.
    /// Before the fix the ghost fell through to the SQLite lookup and
    /// answered `PAIR_UNAVAILABLE` immediately — one status query right
    /// after the redeem pending told a redeemer whether the code existed.
    #[tokio::test]
    async fn status_query_of_a_live_ghost_replies_like_a_real_pending_transaction() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut redeemer =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

        let deadline = now_ms() + 8_000;
        let request = signed_redeem(
            &redeemer.key,
            &registry_id,
            [0xC0; 32],
            random_id16(),
            deadline,
        );
        redeemer.send(&CtrlMessage::InviteRedeem(request)).await;
        let pairing_id = match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending {
                pairing_id,
                deadline_ms,
                ..
            } => {
                assert_eq!(deadline_ms, deadline);
                pairing_id
            }
            other => panic!("expected InviteRedeemPending, got {other:?}"),
        };

        redeemer
            .send(&CtrlMessage::PairStatusQuery {
                request_id: random_id16(),
                pairing_id,
            })
            .await;
        match redeemer.recv().await {
            CtrlMessage::PairStatusReply {
                pairing_id: answered,
                state,
                commit_revision,
                deadline_ms,
                ..
            } => {
                assert_eq!(answered, pairing_id);
                assert_eq!(state, PairingState::Pending);
                assert_eq!(commit_revision, 0);
                assert_eq!(deadline_ms, deadline);
            }
            other => panic!("expected PairStatusReply, got {other:?}"),
        }

        // Another device probing the same id: still the unified
        // unavailable (no oracle in either direction).
        let mut stranger =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "s").await;
        stranger
            .send(&CtrlMessage::PairStatusQuery {
                request_id: random_id16(),
                pairing_id,
            })
            .await;
        match stranger.recv().await {
            CtrlMessage::PairError { code, .. } => assert_eq!(code, PAIR_UNAVAILABLE),
            other => panic!("expected PairError, got {other:?}"),
        }
        server.stop().await;
    }

    // ---- P2 redeem 幂等 --------------------------------------------------------

    /// A retry of the same (redeemer, request_id) replays the ORIGINAL
    /// pairing id instead of minting a second transaction; a different
    /// attempt reusing the id is a REQUEST_CONFLICT (same discipline as
    /// the invite claim journal).
    #[tokio::test]
    async fn redeem_retry_with_the_same_request_id_replays_the_original_pairing() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut redeemer =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

        let deadline = now_ms() + 8_000;
        let request_id = random_id16();
        let request = signed_redeem(
            &redeemer.key,
            &registry_id,
            [0xEE; 32],
            request_id,
            deadline,
        );

        redeemer
            .send(&CtrlMessage::InviteRedeem(request.clone()))
            .await;
        let first = match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("expected InviteRedeemPending, got {other:?}"),
        };

        // Identical retry: the same pairing id, the same deadline — no
        // second ghost, no second pending transaction.
        redeemer
            .send(&CtrlMessage::InviteRedeem(request.clone()))
            .await;
        match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending {
                pairing_id: replayed,
                deadline_ms,
                ..
            } => {
                assert_eq!(replayed, first, "retry must replay the original pairing id");
                assert_eq!(deadline_ms, deadline);
            }
            other => panic!("expected InviteRedeemPending replay, got {other:?}"),
        }

        // Same request id, different attempt: REQUEST_CONFLICT.
        let conflicting = signed_redeem(
            &redeemer.key,
            &registry_id,
            [0xEF; 32],
            request_id,
            deadline,
        );
        redeemer.send(&CtrlMessage::InviteRedeem(conflicting)).await;
        match redeemer.recv().await {
            CtrlMessage::PairError { code, .. } => assert_eq!(code, REQUEST_CONFLICT),
            other => panic!("expected REQUEST_CONFLICT, got {other:?}"),
        }
        server.stop().await;
    }

    // ---- P1-5 resource bounds ---------------------------------------------------

    /// One connection beyond the global cap completes the TLS handshake
    /// and receives exactly one ServerError before the disconnect.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn connections_beyond_the_global_cap_are_rejected_with_server_error() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();

        // Hold the cap's worth of live connections (TLS up, challenge
        // read, never Hello — they stay counted until closed).
        let mut held = Vec::new();
        for _ in 0..MAX_LIVE_CONNECTIONS {
            let mut stream = tls::connect_pinned(server.addr, &registry_id)
                .await
                .expect("tls handshake");
            let bytes = read_frame(&mut stream).await.unwrap().expect("challenge");
            match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
                CtrlMessage::AuthChallenge { .. } => {}
                other => panic!("expected AuthChallenge, got {other:?}"),
            }
            held.push(stream);
        }

        let mut extra = tls::connect_pinned(server.addr, &registry_id)
            .await
            .expect("over-cap tls still completes");
        let bytes = read_frame(&mut extra).await.unwrap().expect("error frame");
        match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_LIMIT),
            other => panic!("expected ServerError, got {other:?}"),
        }
        assert_eq!(
            read_frame(&mut extra).await.unwrap(),
            None,
            "the rejected connection is closed after the error"
        );
        drop(held);
        drop(extra);
        server.stop().await;
    }

    /// Shutdown aborts every live connection task and returns promptly.
    #[tokio::test]
    async fn shutdown_aborts_live_connection_tasks_and_returns() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut client =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "c").await;
        let _silent = tls::connect_pinned(server.addr, &registry_id)
            .await
            .unwrap();

        let _ = server.shutdown.send(true);
        let joined = tokio::time::timeout(Duration::from_secs(5), server.task)
            .await
            .expect("serve_v2 returns after shutdown");
        assert!(joined.unwrap().is_ok());

        // The aborted handler closed the client's socket: EOF or an I/O
        // error promptly, and never another frame.
        let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut client.stream))
            .await
            .expect("client socket closes promptly");
        assert!(
            frame.as_ref().map(|maybe| maybe.is_none()).unwrap_or(true),
            "no frame may arrive after shutdown (got {frame:?})"
        );
    }

    /// The reader's first-frame budget: a client that completed the
    /// connection but never speaks loses its task at the budget (real
    /// budgets are [`FIRST_FRAME_DEADLINE`]; the test injects a tiny one).
    #[tokio::test]
    async fn reader_enforces_the_first_frame_deadline_on_silence() {
        let (client, server_end) = tokio::io::duplex(64);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        let (_close_tx, close) = watch::channel(false);
        let reader = tokio::spawn(reader_loop(
            server_end,
            tx,
            close,
            Duration::from_millis(75),
            Duration::from_secs(30),
        ));
        // The client half stays open but never speaks: no EOF, no bytes —
        // only the first-frame budget can end this reader.
        let _silent_peer = client;
        let started = Instant::now();
        assert!(
            rx.recv().await.is_none(),
            "reader exits at the first-frame deadline"
        );
        reader.await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget ended the reader, not the peer"
        );
    }

    /// The reader forwards frames while they arrive inside the budget and
    /// exits once silence outlives the steady budget.
    #[tokio::test]
    async fn reader_forwards_frames_then_dies_on_silence() {
        let (mut client, server_end) = tokio::io::duplex(64);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        let (_close_tx, close) = watch::channel(false);
        let reader = tokio::spawn(reader_loop(
            server_end,
            tx,
            close,
            Duration::from_secs(30),
            Duration::from_millis(75),
        ));

        // One legal client frame passes through the reader untouched.
        write_message(
            &mut client,
            &CtrlMessage::Ping { nonce: [7; 16] },
            Role::Client,
        )
        .await
        .unwrap();
        let bytes = rx.recv().await.expect("frame forwarded");
        assert_eq!(
            CtrlFrame::decode_as(&bytes, Role::Server).unwrap().message,
            CtrlMessage::Ping { nonce: [7; 16] }
        );

        // Then silence: the steady read budget ends the reader.
        assert!(
            rx.recv().await.is_none(),
            "reader exits after the steady deadline"
        );
        reader.await.unwrap();
    }

    /// The writer's write budget: a write that can never drain (1-byte
    /// duplex buffer, nobody reading) reports the dead connection to the
    /// handler instead of parking forever.
    #[tokio::test]
    async fn writer_stall_tears_the_connection_down() {
        let (unread, server_end) = tokio::io::duplex(1);
        let (tx, rx) = mpsc::channel::<CtrlFrame>(4);
        let (_close_tx, close) = watch::channel(false);
        let (dead_tx, mut dead_rx) = mpsc::channel::<()>(1);
        let writer = tokio::spawn(writer_loop(
            server_end,
            rx,
            close,
            dead_tx,
            Duration::from_millis(75),
        ));

        tx.send(CtrlFrame::new(CtrlMessage::Pong { nonce: [9; 16] }))
            .await
            .unwrap();
        let started = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), dead_rx.recv())
            .await
            .expect("a stalled write reports the dead connection")
            .expect("the dead signal carries no payload");
        writer.await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the write budget fired, not the test timeout"
        );
        drop(unread);
    }

    // ---- wave-28 endpoint exchange (frame extension e2e) --------------------

    impl TestClient {
        async fn send_frame(&mut self, frame: &CtrlFrame) {
            crate::server_v2::framing::write_frame(&mut self.stream, frame, Role::Client)
                .await
                .unwrap();
        }

        async fn recv_frame(&mut self) -> CtrlFrame {
            let deadline = Duration::from_secs(10);
            tokio::time::timeout(deadline, async {
                let bytes = read_frame(&mut self.stream).await.unwrap().expect("frame");
                CtrlFrame::decode_as(&bytes, Role::Client).unwrap()
            })
            .await
            .expect("frame within deadline")
        }
    }

    /// Drive both participants from a live pending redeem to the
    /// authoritative commit: owner PairChallenge, redeemer PairProof,
    /// both PairPrepared. Returns the transcript bytes and digest.
    #[allow(clippy::too_many_arguments)]
    async fn drive_to_commit(
        owner: &mut TestClient,
        redeemer: &mut TestClient,
        registry_id: &RegistryId,
        code_id: CodeId,
        lookup: Digest32,
        pairing_id: PairingId,
        request_id: RequestId,
        redeemer_nonce: [u8; 32],
        deadline: u64,
    ) -> ([u8; 256], Digest32) {
        let transcript = PairTranscript {
            registry_id: *registry_id,
            code_id,
            code_revision: 1,
            pairing_id,
            request_id,
            lookup_digest: lookup,
            owner_device_id: owner.key.public_key(),
            redeemer_device_id: redeemer.key.public_key(),
            owner_nonce: [0x0D; 32],
            redeemer_nonce,
            deadline_ms: deadline,
        };
        let bytes = transcript.to_bytes();
        let owner_proof = PairProof {
            mac: [0x11; 32],
            signature: owner
                .key
                .sign(&hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &bytes, &[0x11; 32])),
        };
        owner
            .send(&CtrlMessage::PairChallenge {
                transcript: bytes,
                owner_proof: owner_proof.clone(),
            })
            .await;
        match redeemer.recv().await {
            CtrlMessage::PairChallenge {
                transcript,
                owner_proof: relayed,
            } => {
                assert_eq!(transcript, bytes);
                assert_eq!(relayed, owner_proof);
            }
            other => panic!("expected relayed PairChallenge, got {other:?}"),
        }
        let digest = hello2::transcript_digest(&bytes);
        let redeemer_proof = PairProof {
            mac: [0x22; 32],
            signature: redeemer
                .key
                .sign(&hello2::proof_signed_message(REDEEMER_PROOF_DOMAIN, &bytes, &[0x22; 32])),
        };
        redeemer
            .send(&CtrlMessage::PairProof {
                pairing_id,
                transcript_digest: digest,
                redeemer_proof: redeemer_proof.clone(),
            })
            .await;
        match owner.recv().await {
            CtrlMessage::PairProof {
                transcript_digest,
                redeemer_proof: relayed,
                ..
            } => {
                assert_eq!(transcript_digest, digest);
                assert_eq!(relayed, redeemer_proof);
            }
            other => panic!("expected relayed PairProof, got {other:?}"),
        }
        owner
            .send(&CtrlMessage::PairPrepared {
                pairing_id,
                transcript_digest: digest,
                signature: owner.key.sign(&hello2::prepared_message(&pairing_id, &digest)),
            })
            .await;
        redeemer
            .send(&CtrlMessage::PairPrepared {
                pairing_id,
                transcript_digest: digest,
                signature: redeemer.key.sign(&hello2::prepared_message(&pairing_id, &digest)),
            })
            .await;
        (bytes, digest)
    }

    /// Wave-28 end to end: the owner's connection settings claimed with
    /// the code and the redeemer's settings sent with the redeem both
    /// reach the opposite side, byte-identical, riding the existing
    /// offer/commit frames (no extra round trips).
    #[tokio::test]
    async fn endpoint_blobs_reach_both_sides_byte_identical() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut owner =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
        let mut redeemer =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

        let owner_blob = EndpointBlob(vec![0xA1; 217]);
        let redeemer_blob = EndpointBlob(vec![0xB2; 333]);
        let lookup: Digest32 = [0x7C; 32];

        // Claim with the owner endpoint (no cap: legacy multi-pairing).
        let claim_request = random_id16();
        owner
            .send_frame(
                &CtrlFrame::new(CtrlMessage::InviteClaim {
                    request_id: claim_request,
                    lookup_digest: lookup,
                    ttl_secs: 0,
                })
                .with_endpoint(owner_blob.clone()),
            )
            .await;
        let code_id = match owner.recv_frame().await.message {
            CtrlMessage::InviteClaimed { code_id, .. } => code_id,
            other => panic!("expected InviteClaimed, got {other:?}"),
        };

        // Redeem with the redeemer endpoint.
        let deadline = now_ms() + 8_000;
        let redeem = signed_redeem(&redeemer.key, &registry_id, lookup, random_id16(), deadline);
        let request_id = redeem.request_id;
        let redeemer_nonce = redeem.redeemer_nonce;
        redeemer
            .send_frame(
                &CtrlFrame::new(CtrlMessage::InviteRedeem(redeem))
                    .with_endpoint(redeemer_blob.clone()),
            )
            .await;
        let pairing_id = match redeemer.recv_frame().await.message {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("expected InviteRedeemPending, got {other:?}"),
        };

        // The owner's offer carries the redeemer endpoint verbatim.
        let offered = owner.recv_frame().await;
        match &offered.message {
            CtrlMessage::InviteRedeemOffered {
                code_id: offered_code,
                pairing_id: offered_pairing,
                ..
            } => {
                assert_eq!(*offered_code, code_id);
                assert_eq!(*offered_pairing, pairing_id);
            }
            other => panic!("expected InviteRedeemOffered, got {other:?}"),
        }
        assert_eq!(
            offered.endpoint,
            Some(redeemer_blob.clone()),
            "redeemer endpoint must ride the offer byte-identical"
        );

        // Complete the pairing; the redeemer's PairCommit carries the
        // owner endpoint verbatim.
        drive_to_commit(
            &mut owner,
            &mut redeemer,
            &registry_id,
            code_id,
            lookup,
            pairing_id,
            request_id,
            redeemer_nonce,
            deadline,
        )
        .await;
        let commit = redeemer.recv_frame().await;
        match &commit.message {
            CtrlMessage::PairCommit {
                commit_revision, ..
            } => assert_eq!(*commit_revision, 1),
            other => panic!("expected PairCommit, got {other:?}"),
        }
        assert_eq!(
            commit.endpoint,
            Some(owner_blob.clone()),
            "owner endpoint must ride the commit byte-identical"
        );
        // The owner's own commit copy also carries its blob (same fixed
        // frame to both participants; no third party is involved).
        let owner_commit = owner.recv_frame().await;
        assert!(matches!(owner_commit.message, CtrlMessage::PairCommit { .. }));
        assert_eq!(owner_commit.endpoint, Some(owner_blob.clone()));
        server.stop().await;
    }

    /// `max_redeems=1`: after one committed pairing the code is dead for
    /// every other redeemer — the second, different redeemer is rejected
    /// immediately with the dedicated `REDEEM_EXHAUSTED` code (no
    /// deferred-deadline ghost semantics).
    #[tokio::test]
    async fn single_redeem_cap_rejects_the_second_redeemer() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut owner =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
        let mut first =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "a").await;

        let lookup: Digest32 = [0x5E; 32];
        let claim_request = random_id16();
        owner
            .send_frame(
                &CtrlFrame::new(CtrlMessage::InviteClaim {
                    request_id: claim_request,
                    lookup_digest: lookup,
                    ttl_secs: 0,
                })
                .with_endpoint(EndpointBlob(vec![0xC3; 64]))
                .with_max_redeems(1),
            )
            .await;
        let code_id = match owner.recv_frame().await.message {
            CtrlMessage::InviteClaimed { code_id, .. } => code_id,
            other => panic!("expected InviteClaimed, got {other:?}"),
        };

        // First redeemer completes through the commit.
        let deadline = now_ms() + 8_000;
        let redeem = signed_redeem(&first.key, &registry_id, lookup, random_id16(), deadline);
        let request_id = redeem.request_id;
        let redeemer_nonce = redeem.redeemer_nonce;
        first
            .send(&CtrlMessage::InviteRedeem(redeem))
            .await;
        let pairing_id = match first.recv().await {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("expected InviteRedeemPending, got {other:?}"),
        };
        let _offer = owner.recv_frame().await; // offer push
        drive_to_commit(
            &mut owner,
            &mut first,
            &registry_id,
            code_id,
            lookup,
            pairing_id,
            request_id,
            redeemer_nonce,
            deadline,
        )
        .await;
        let _ = first.recv_frame().await; // PairCommit to the redeemer
        let _ = owner.recv_frame().await; // PairCommit to the owner

        // A second, different redeemer: immediate, explicit rejection.
        let mut second =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "b").await;
        let second_deadline = now_ms() + 8_000;
        let second_redeem = signed_redeem(
            &second.key,
            &registry_id,
            lookup,
            random_id16(),
            second_deadline,
        );
        let second_request = second_redeem.request_id;
        second
            .send(&CtrlMessage::InviteRedeem(second_redeem))
            .await;
        match second.recv().await {
            CtrlMessage::PairError {
                request_id,
                scope,
                object_id,
                code,
                ..
            } => {
                assert_eq!(request_id, second_request);
                assert_eq!(code, REDEEM_EXHAUSTED);
                assert_eq!(scope, ErrorScope::Code);
                assert_eq!(object_id, code_id);
            }
            other => panic!("expected REDEEM_EXHAUSTED PairError, got {other:?}"),
        }
        server.stop().await;
    }

    /// The same request id replaying a completed capped redeem still gets
    /// the original pending answer (idempotency unchanged by the cap).
    #[tokio::test]
    async fn redeem_replay_semantics_survive_the_cap() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut owner =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
        let mut redeemer =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

        let lookup: Digest32 = [0x6F; 32];
        owner
            .send_frame(
                &CtrlFrame::new(CtrlMessage::InviteClaim {
                    request_id: random_id16(),
                    lookup_digest: lookup,
                    ttl_secs: 0,
                })
                .with_max_redeems(1),
            )
            .await;
        let code_id = match owner.recv_frame().await.message {
            CtrlMessage::InviteClaimed { code_id, .. } => code_id,
            other => panic!("expected InviteClaimed, got {other:?}"),
        };
        let deadline = now_ms() + 8_000;
        let redeem = signed_redeem(&redeemer.key, &registry_id, lookup, random_id16(), deadline);
        let request_id = redeem.request_id;
        let redeemer_nonce = redeem.redeemer_nonce;
        redeemer
            .send(&CtrlMessage::InviteRedeem(redeem.clone()))
            .await;
        let pairing_id = match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("expected InviteRedeemPending, got {other:?}"),
        };
        let _offer = owner.recv_frame().await;
        drive_to_commit(
            &mut owner,
            &mut redeemer,
            &registry_id,
            code_id,
            lookup,
            pairing_id,
            request_id,
            redeemer_nonce,
            deadline,
        )
        .await;
        let _ = redeemer.recv_frame().await; // PairCommit

        // Same request id retry after the cap is in force: the original
        // result replays, not a fresh REDEEM_EXHAUSTED.
        redeemer
            .send(&CtrlMessage::InviteRedeem(redeem))
            .await;
        match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending {
                pairing_id: replayed,
                ..
            } => assert_eq!(replayed, pairing_id),
            other => panic!("expected replayed InviteRedeemPending, got {other:?}"),
        }
        server.stop().await;
    }

    /// P1-03: `max_redeems=1` binds at the commit linearization point,
    /// not only at redeem begin. Two redeemers both reach pending (pending
    /// consumes nothing), then race to Commit — exactly one pairing
    /// commits; the loser's Commit fails with the explicit
    /// `REDEEM_EXHAUSTED` error to both participants, the code refuses
    /// every new redeemer, and the loser's same-`request_id` retry still
    /// replays its original pending answer (idempotency unchanged).
    #[tokio::test]
    async fn two_pending_redeems_racing_commit_yield_exactly_one() {
        let server = spawn_server().await;
        let registry_id = server.registry_id();
        let mut owner =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
        let mut first =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "a").await;
        let mut second =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "b").await;

        let lookup: Digest32 = [0x7E; 32];
        let claim_request = random_id16();
        owner
            .send_frame(
                &CtrlFrame::new(CtrlMessage::InviteClaim {
                    request_id: claim_request,
                    lookup_digest: lookup,
                    ttl_secs: 0,
                })
                .with_max_redeems(1),
            )
            .await;
        let code_id = match owner.recv_frame().await.message {
            CtrlMessage::InviteClaimed { code_id, .. } => code_id,
            other => panic!("expected InviteClaimed, got {other:?}"),
        };

        // Both redeemers are admitted: pending never consumes the cap.
        let deadline = now_ms() + 8_000;
        let redeem_a = signed_redeem(&first.key, &registry_id, lookup, random_id16(), deadline);
        let request_a = redeem_a.request_id;
        let nonce_a = redeem_a.redeemer_nonce;
        first.send(&CtrlMessage::InviteRedeem(redeem_a.clone())).await;
        let pairing_a = match first.recv().await {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("first redeem must begin, got {other:?}"),
        };
        let _offer_a = owner.recv_frame().await; // push to the owner

        let redeem_b = signed_redeem(&second.key, &registry_id, lookup, random_id16(), deadline);
        second.send(&CtrlMessage::InviteRedeem(redeem_b.clone())).await;
        let pairing_b = match second.recv().await {
            CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
            other => panic!("second redeem must begin, got {other:?}"),
        };
        assert_ne!(pairing_a, pairing_b);
        let _offer_b = owner.recv_frame().await;

        // Racer A completes through the authoritative commit.
        drive_to_commit(
            &mut owner,
            &mut first,
            &registry_id,
            code_id,
            lookup,
            pairing_a,
            request_a,
            nonce_a,
            deadline,
        )
        .await;
        let _ = first.recv_frame().await; // PairCommit to the redeemer
        let _ = owner.recv_frame().await; // PairCommit to the owner

        // Racer B's Commit is refused inside the same write transaction:
        // both B and the owner get the explicit, code-scoped failure.
        drive_to_commit(
            &mut owner,
            &mut second,
            &registry_id,
            code_id,
            lookup,
            pairing_b,
            redeem_b.request_id,
            redeem_b.redeemer_nonce,
            deadline,
        )
        .await;
        for (who, frame) in [("redeemer", second.recv().await), ("owner", owner.recv().await)] {
            match frame {
                CtrlMessage::PairError {
                    scope,
                    object_id,
                    code,
                    ..
                } => {
                    assert_eq!(code, REDEEM_EXHAUSTED, "{who}");
                    // Wave-30 (R29-P2-01): pairing-scoped and addressed to
                    // THIS pairing so the production client dispatch and
                    // orchestrator wait_frame actually deliver the loss.
                    assert_eq!(scope, ErrorScope::Pairing, "{who}");
                    assert_eq!(object_id, pairing_b, "{who}");
                }
                other => panic!("{who}: expected REDEEM_EXHAUSTED, got {other:?}"),
            }
        }

        // The exhausted code refuses every new redeemer immediately.
        let mut third =
            TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "c").await;
        let third_redeem = signed_redeem(
            &third.key,
            &registry_id,
            lookup,
            random_id16(),
            now_ms() + 8_000,
        );
        third.send(&CtrlMessage::InviteRedeem(third_redeem)).await;
        match third.recv().await {
            CtrlMessage::PairError { code, .. } => assert_eq!(code, REDEEM_EXHAUSTED),
            other => panic!("expected REDEEM_EXHAUSTED, got {other:?}"),
        }

        // The losing redeemer retrying its own request id still replays
        // the original pending answer — the cap never breaks redeem
        // idempotency.
        second
            .send(&CtrlMessage::InviteRedeem(redeem_b))
            .await;
        match second.recv().await {
            CtrlMessage::InviteRedeemPending {
                pairing_id: replayed,
                ..
            } => assert_eq!(replayed, pairing_b),
            other => panic!("expected replayed InviteRedeemPending, got {other:?}"),
        }
        server.stop().await;
    }
}
