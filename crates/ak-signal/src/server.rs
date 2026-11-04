//! Signaling server: TCP signaling plus a UDP STUN responder in one process.
//!
//! Limits (independent of any file quota): per-connection message rate,
//! bounded candidates, bounded sessions per device, session TTL, Hello
//! freshness, and a Hello replay window (an exact re-send of an already
//! accepted signed Hello inside the freshness window is rejected as a
//! suspected replay — see [`HelloReplayCache`]). The server verifies Hello
//! signatures but trusts nothing else; it never sees file names, manifests,
//! or bytes.
//!
//! There is intentionally no upload/download/forward route. A test locks the
//! closed message set so no blob route can appear silently.
#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch, Notify};

use crate::codec::{self, CtrlFrame, CtrlMessage, DeviceId, PeerSignal, SessionId};
use crate::stun;
use crate::util;

/// Server error codes carried in ServerError frames.
pub const ERR_UNKNOWN_DEVICE: u16 = 1;
/// ConnectRequest named a device with no live registration.
pub const ERR_SESSION_LIMIT: u16 = 2;
/// Per-connection token bucket exhausted.
pub const ERR_RATE_LIMITED: u16 = 3;
/// Message invalid in the current connection state.
pub const ERR_BAD_REQUEST: u16 = 4;
/// Signal/Cancel named an unknown or expired session.
pub const ERR_SESSION_NOT_FOUND: u16 = 5;

/// Capacity of the Hello replay window (accepted-signature digests kept).
/// Bounded memory: 256 entries x (32-byte digest + Instant) is a few KiB.
pub const HELLO_REPLAY_CAPACITY: usize = 256;

/// 256-bit digest of (device_id || signature) for the Hello replay cache.
///
/// Non-cryptographic (no hash crate is a dependency of this crate's
/// coordination plane; ak-identity's blake3 use is not re-exported): four
/// independently seeded splitmix64 lanes over all 96 input bytes, each lane
/// finishing with its avalanche fold. The only failure modes are (a) an
/// accidental 256-bit collision — negligible at this cache's scale — causing
/// a false replay rejection of a legitimately distinct Hello, or (b) an
/// attacker crafting two distinct valid signatures for the same device that
/// collide, which first requires breaking Ed25519 uniqueness. Rejected
/// replays disconnect and may reconnect with a fresh timestamp, so even a
/// pathological false positive is self-healing on the next attempt.
fn hello_digest(device_id: &DeviceId, signature: &[u8; 64]) -> [u8; 32] {
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

/// Sliding-window memory of accepted Hello signature digests (server-wide,
/// shared across TCP connections — replays arrive on fresh connections).
///
/// Semantics: after a Hello passes signature verification and freshness, its
/// `(device_id, signature)` digest is looked up among digests admitted within
/// `hello_max_age`; a hit means the exact signed bytes were already accepted
/// inside the freshness window and the new copy is a suspected replay. The
/// connection is refused (ServerError mentioning the replay suspicion, then
/// disconnect) — an attacker who captured a valid Hello cannot use it to
/// hijack the device's registration, while a legitimate client reconnects
/// with a fresh timestamp (a different signature) and passes.
///
/// Bounded: at most [`HELLO_REPLAY_CAPACITY`] digests; entries older than the
/// window are dropped on every admission (and stale replays are already
/// rejected by the freshness gate, which uses the same window, so expiry
/// never weakens the check).
#[derive(Debug, Default)]
struct HelloReplayCache {
    seen: VecDeque<([u8; 32], Instant)>,
}

impl HelloReplayCache {
    /// Record `digest` seen at `now`. Returns `false` when the same digest
    /// was already admitted within `window` (replay suspected); returns
    /// `true` after recording it. `now` is a parameter so tests can exercise
    /// window expiry without sleeping.
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
        if self.seen.len() >= HELLO_REPLAY_CAPACITY {
            self.seen.pop_front();
        }
        self.seen.push_back((digest, now));
        true
    }
}

/// Server limits; every field is a coordination-plane resource cap.
#[derive(Debug, Clone)]
pub struct Limits {
    pub session_ttl: Duration,
    pub max_sessions_per_device: usize,
    /// Hello freshness window; doubles as the Hello replay window — an
    /// accepted Hello signature digest is remembered for this long, and a
    /// byte-identical Hello inside the window is rejected as a suspected
    /// replay (see [`HelloReplayCache`]).
    pub hello_max_age: Duration,
    /// Steady-state frames per second per connection.
    pub frame_rate_per_sec: u32,
    /// Burst allowance for the token bucket.
    pub frame_burst: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(120),
            max_sessions_per_device: 4,
            hello_max_age: Duration::from_secs(10),
            frame_rate_per_sec: 32,
            frame_burst: 64,
        }
    }
}

/// Where to listen: TCP signaling plus UDP STUN (same process, two sockets).
#[derive(Debug, Clone)]
pub struct Config {
    pub tcp: SocketAddr,
    pub stun_udp: SocketAddr,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tcp: SocketAddr::from(([0, 0, 0, 0], 7100)),
            stun_udp: SocketAddr::from(([0, 0, 0, 0], 7101)),
            limits: Limits::default(),
        }
    }
}

/// Runtime handle for embedding (tests, CLI).
pub struct ServerHandle {
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

/// Run the server until `shutdown` fires. Binding both sockets is part of
/// startup; errors abort the whole server.
pub async fn run(config: Config, shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.tcp).await?;
    let stun_socket = UdpSocket::bind(config.stun_udp).await?;
    serve(listener, stun_socket, config.limits, shutdown).await
}

/// Bound-sockets server loop. Split out of [`run`] so embedders (and tests)
/// can bind ephemeral ports themselves and still get the same behavior.
pub(crate) async fn serve(
    listener: TcpListener,
    stun_socket: UdpSocket,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let shared = Arc::new(Shared {
        limits,
        inner: Mutex::new(Inner::default()),
    });
    tokio::spawn(stun_responder(stun_socket, shutdown.clone()));
    let sweep_every = shared
        .limits
        .session_ttl
        .min(Duration::from_secs(30))
        .max(Duration::from_millis(100))
        / 2;
    let mut tick = tokio::time::interval(sweep_every);
    log::info!(
        "signaling server listening on tcp {}",
        listener.local_addr()?
    );
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = tick.tick() => {
                let outbound = {
                    let mut inner = shared.lock();
                    sweep_expired(&mut inner)
                };
                for (tx, msg) in outbound {
                    deliver(&tx, msg).await;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let shared = shared.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(connection_loop(stream, peer, shared, shutdown));
                }
                Err(e) => {
                    log::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

/// UDP STUN binding responder. Answers binding requests with the sender's
/// observed address; ignores everything else. Never relays any payload.
async fn stun_responder(socket: UdpSocket, mut shutdown: watch::Receiver<bool>) {
    let mut buf = vec![0u8; 256];
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            received = socket.recv_from(&mut buf) => match received {
                Ok((len, peer)) => match stun::parse_binding_request(&buf[..len]) {
                    Ok(txid) => {
                        let reply = stun::binding_response(txid, peer);
                        if let Err(e) = socket.send_to(&reply, peer).await {
                            log::debug!("stun reply to {peer} failed: {e}");
                        }
                    }
                    Err(e) => log::debug!("ignoring non-STUN datagram from {peer}: {e}"),
                },
                Err(e) => {
                    log::warn!("stun recv failed: {e}");
                    return;
                }
            },
        }
    }
}

/// One connected device. `conn_id` distinguishes generations of the same
/// device id so a replaced connection cannot tear down the new one.
/// `peer_ip` is the TCP peer address sampled once at Hello time; a device
/// reconnecting (possibly from a new network) replaces the whole entry,
/// refreshing the record.
struct DeviceEntry {
    conn_id: u64,
    out: mpsc::Sender<CtrlMessage>,
    close: Arc<Notify>,
    #[allow(dead_code)] // validated and stored for near-term connect enrichment
    candidates: Vec<SocketAddr>,
    peer_ip: IpAddr,
}

#[derive(Clone)]
struct SessionEntry {
    endpoints: [DeviceId; 2],
    expires_at: Instant,
}

#[derive(Default)]
struct Inner {
    devices: HashMap<DeviceId, DeviceEntry>,
    sessions: HashMap<SessionId, SessionEntry>,
    next_conn_id: u64,
    /// Server-wide Hello replay window (see [`HelloReplayCache`]).
    hello_replay: HelloReplayCache,
}

struct Shared {
    limits: Limits,
    inner: Mutex<Inner>,
}

impl Shared {
    /// Locks the registry; a poisoned lock (impossible without a panic
    /// inside a critical section) is recovered, not propagated.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Remove sessions by id and produce Cancel notifications for the other
/// endpoints. `except` skips one device (e.g. the cancelling endpoint).
fn remove_sessions(
    inner: &mut Inner,
    ids: &[SessionId],
    except: Option<&DeviceId>,
) -> Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)> {
    let mut outbound = Vec::new();
    for id in ids {
        let Some(entry) = inner.sessions.remove(id) else {
            continue;
        };
        for endpoint in entry.endpoints {
            if Some(&endpoint) == except {
                continue;
            }
            if let Some(device) = inner.devices.get(&endpoint) {
                outbound.push((device.out.clone(), CtrlMessage::Cancel { session: *id }));
            }
        }
    }
    outbound
}

fn sweep_expired(inner: &mut Inner) -> Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)> {
    let now = Instant::now();
    let expired: Vec<SessionId> = inner
        .sessions
        .iter()
        .filter(|(_, session)| session.expires_at <= now)
        .map(|(id, _)| *id)
        .collect();
    remove_sessions(inner, &expired, None)
}

fn disconnect_device(
    inner: &mut Inner,
    device: &DeviceId,
    conn_id: u64,
) -> Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)> {
    let mine = inner
        .devices
        .get(device)
        .map(|entry| entry.conn_id == conn_id)
        .unwrap_or(false);
    if !mine {
        return Vec::new();
    }
    inner.devices.remove(device);
    let sessions: Vec<SessionId> = inner
        .sessions
        .iter()
        .filter(|(_, session)| session.endpoints.contains(device))
        .map(|(id, _)| *id)
        .collect();
    remove_sessions(inner, &sessions, None)
}

fn fresh_session_id(inner: &Inner) -> SessionId {
    let mut id = util::unique_array::<16>();
    for _ in 0..8 {
        if !inner.sessions.contains_key(&id) {
            return id;
        }
        id = util::unique_array::<16>();
    }
    id
}

async fn deliver(tx: &mpsc::Sender<CtrlMessage>, message: CtrlMessage) {
    if tx.send(message).await.is_err() {
        log::debug!("peer outbound channel closed; message dropped");
    }
}

fn error_message(code: u16) -> String {
    match code {
        ERR_UNKNOWN_DEVICE => "target device not online".to_string(),
        ERR_SESSION_LIMIT => "session limit reached".to_string(),
        ERR_RATE_LIMITED => "frame rate exceeded".to_string(),
        ERR_BAD_REQUEST => "malformed request".to_string(),
        ERR_SESSION_NOT_FOUND => "session not found".to_string(),
        other => format!("error {other}"),
    }
}

/// Lazy token bucket: no background task, refilled on demand.
struct TokenBucket {
    tokens: f64,
    burst: f64,
    rate_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rate_per_sec: u32, burst: u32) -> Self {
        Self {
            tokens: burst as f64,
            burst: burst as f64,
            rate_per_sec: rate_per_sec as f64,
            last: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.last = now;
            self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.burst);
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct ConnState {
    device: Option<DeviceId>,
    conn_id: u64,
    close: Arc<Notify>,
    /// Remote IP of this TCP connection, sampled once at accept time; a
    /// TCP peer address never changes over the life of the connection.
    peer_ip: IpAddr,
}

enum Flow {
    Continue,
    Disconnect,
}

async fn connection_loop(
    stream: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let _ = stream.set_nodelay(true);
    let (mut reader, writer) = stream.into_split();
    let (out_tx, out_rx) = mpsc::channel::<CtrlMessage>(64);
    let writer_task = tokio::spawn(write_loop(writer, out_rx));
    let mut state = ConnState {
        device: None,
        conn_id: 0,
        close: Arc::new(Notify::new()),
        peer_ip: peer.ip(),
    };
    let mut bucket = TokenBucket::new(shared.limits.frame_rate_per_sec, shared.limits.frame_burst);
    log::debug!("signaling connection from {peer}");
    loop {
        let bytes = tokio::select! {
            _ = shutdown.changed() => break,
            _ = state.close.notified() => break,
            received = util::read_frame(&mut reader) => match received {
                Ok(Some(bytes)) => bytes,
                Ok(None) | Err(_) => break,
            },
        };
        if !bucket.allow() {
            log::warn!("rate limit hit on connection from {peer}");
            deliver(
                &out_tx,
                CtrlMessage::ServerError {
                    code: ERR_RATE_LIMITED,
                    message: error_message(ERR_RATE_LIMITED),
                },
            )
            .await;
            break;
        }
        let frame = match CtrlFrame::decode(&bytes) {
            Ok(frame) => frame,
            Err(e) => {
                log::debug!("undecodable frame from {peer}: {e}");
                break;
            }
        };
        match process(&shared, &mut state, &out_tx, frame.message).await {
            Flow::Continue => {}
            Flow::Disconnect => break,
        }
    }
    if let Some(device) = state.device {
        let outbound = {
            let mut inner = shared.lock();
            disconnect_device(&mut inner, &device, state.conn_id)
        };
        for (tx, msg) in outbound {
            deliver(&tx, msg).await;
        }
        log::info!("device {} disconnected", util::hex_prefix(&device, 4));
    }
    drop(out_tx);
    // Give the writer a moment to flush queued messages, then move on.
    let _ = tokio::time::timeout(Duration::from_secs(2), writer_task).await;
}

async fn write_loop(mut writer: OwnedWriteHalf, mut rx: mpsc::Receiver<CtrlMessage>) {
    while let Some(message) = rx.recv().await {
        let mut buf = Vec::with_capacity(128);
        CtrlFrame {
            version: codec::VERSION,
            message,
        }
        .encode(&mut buf);
        if writer.write_all(&buf).await.is_err() {
            return;
        }
    }
}

async fn process(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlMessage>,
    message: CtrlMessage,
) -> Flow {
    if state.device.is_none() {
        let (device_id, timestamp, signature) = match &message {
            CtrlMessage::Hello {
                device_id,
                timestamp_unix_ms,
                signature,
                ..
            } => (*device_id, *timestamp_unix_ms, *signature),
            _ => {
                log::debug!("connection must start with Hello");
                return Flow::Disconnect;
            }
        };
        let now = util::now_unix_ms();
        if Duration::from_millis(now.abs_diff(timestamp)) > shared.limits.hello_max_age {
            log::info!(
                "hello from {} rejected: stale",
                util::hex_prefix(&device_id, 4)
            );
            return Flow::Disconnect;
        }
        if let Err(e) = codec::verify_hello(&message, now) {
            log::info!(
                "hello from {} rejected: {e}",
                util::hex_prefix(&device_id, 4)
            );
            return Flow::Disconnect;
        }
        // Replay gate: the signature is valid, but if the exact signed bytes
        // were already accepted inside the freshness window, this copy is a
        // replay of a captured Hello, not the key holder reconnecting.
        let digest = hello_digest(&device_id, &signature);
        let conn_id = {
            let mut inner = shared.lock();
            if !inner
                .hello_replay
                .admit(digest, Instant::now(), shared.limits.hello_max_age)
            {
                None
            } else {
                inner.next_conn_id = inner.next_conn_id.wrapping_add(1);
                let conn_id = inner.next_conn_id;
                if let Some(previous) = inner.devices.get(&device_id) {
                    // Same device reconnected: retire the old connection.
                    previous.close.notify_one();
                }
                inner.devices.insert(
                    device_id,
                    DeviceEntry {
                        conn_id,
                        out: out.clone(),
                        close: state.close.clone(),
                        candidates: Vec::new(),
                        peer_ip: state.peer_ip,
                    },
                );
                Some(conn_id)
            }
        };
        let Some(conn_id) = conn_id else {
            log::warn!(
                "hello from {} rejected: replay suspected (signature already seen in window)",
                util::hex_prefix(&device_id, 4)
            );
            deliver(
                out,
                CtrlMessage::ServerError {
                    code: ERR_BAD_REQUEST,
                    message:
                        "replay suspected: hello signature already accepted in the freshness window"
                            .to_string(),
                },
            )
            .await;
            return Flow::Disconnect;
        };
        state.device = Some(device_id);
        state.conn_id = conn_id;
        log::info!("device {} online", util::hex_prefix(&device_id, 4));
        deliver(
            out,
            CtrlMessage::Welcome {
                session_ttl_secs: shared.limits.session_ttl.as_secs() as u32,
            },
        )
        .await;
        return Flow::Continue;
    }

    let Some(me) = state.device else {
        return Flow::Disconnect;
    };
    match message {
        CtrlMessage::Hello { .. } => {
            log::debug!("duplicate Hello on authenticated connection");
            Flow::Disconnect
        }
        CtrlMessage::Welcome { .. }
        | CtrlMessage::SessionOpened { .. }
        | CtrlMessage::ServerError { .. } => {
            // Server-to-client types must never arrive from a client.
            log::debug!("server-to-client frame type from client");
            Flow::Disconnect
        }
        CtrlMessage::Register { candidates } => {
            if candidates.len() > codec::MAX_CANDIDATES
                || candidates.iter().any(|addr| addr.port() == 0)
            {
                deliver(
                    out,
                    CtrlMessage::ServerError {
                        code: ERR_BAD_REQUEST,
                        message: error_message(ERR_BAD_REQUEST),
                    },
                )
                .await;
            } else {
                let mut inner = shared.lock();
                if let Some(entry) = inner.devices.get_mut(&me) {
                    if entry.conn_id == state.conn_id {
                        log::debug!(
                            "device {} registered {} candidates",
                            util::hex_prefix(&me, 4),
                            candidates.len()
                        );
                        entry.candidates = candidates;
                    }
                }
            }
            Flow::Continue
        }
        CtrlMessage::ConnectRequest { target } => {
            handle_connect(shared, out, me, target).await;
            Flow::Continue
        }
        CtrlMessage::Signal { session, signal } => {
            handle_signal(shared, out, me, session, signal).await;
            Flow::Continue
        }
        CtrlMessage::Cancel { session } => {
            handle_cancel(shared, me, session).await;
            Flow::Continue
        }
    }
}

/// Pure decision behind the SameEgress hint: both endpoints' recorded
/// control-plane TCP peer IPs are known and identical. An unknown
/// (`None`) address never matches, so the hint is never guessed.
pub(crate) fn same_egress(ip_a: Option<IpAddr>, ip_b: Option<IpAddr>) -> bool {
    match (ip_a, ip_b) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Queue `SessionOpened` for both endpoints and, when both currently live
/// behind the same observed TCP peer IP, a `SameEgress` hint to each after
/// their `SessionOpened`. Distinct or unknown IPs send no hint.
fn announce_session(
    inner: &Inner,
    outbound: &mut Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)>,
    session: SessionId,
    me: DeviceId,
    target: DeviceId,
) {
    let me_ip = inner.devices.get(&me).map(|entry| entry.peer_ip);
    let target_ip = inner.devices.get(&target).map(|entry| entry.peer_ip);
    if let Some(entry) = inner.devices.get(&me) {
        outbound.push((
            entry.out.clone(),
            CtrlMessage::SessionOpened {
                session,
                peer: target,
            },
        ));
    }
    if let Some(entry) = inner.devices.get(&target) {
        outbound.push((
            entry.out.clone(),
            CtrlMessage::SessionOpened { session, peer: me },
        ));
    }
    if same_egress(me_ip, target_ip) {
        log::info!(
            "session {}: endpoints share one egress, hinting LAN-first",
            util::hex_prefix(&session, 4)
        );
        for device in [me, target] {
            if let Some(entry) = inner.devices.get(&device) {
                outbound.push((
                    entry.out.clone(),
                    CtrlMessage::Signal {
                        session,
                        signal: PeerSignal::SameEgress,
                    },
                ));
            }
        }
    }
}

async fn handle_connect(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    target: DeviceId,
) {
    let mut outbound: Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)> = Vec::new();
    let mut error: Option<u16> = None;
    {
        let mut inner = shared.lock();
        // Expire stale sessions first so counts and reuse lookups are honest.
        outbound.extend(sweep_expired(&mut inner));
        if target == me {
            error = Some(ERR_BAD_REQUEST);
        } else if !inner.devices.contains_key(&target) {
            error = Some(ERR_UNKNOWN_DEVICE);
        } else if let Some(session) = inner
            .sessions
            .iter()
            .find(|(_, session)| {
                session.endpoints.contains(&me) && session.endpoints.contains(&target)
            })
            .map(|(id, _)| *id)
        {
            // Existing pair session: re-announce to both endpoints.
            announce_session(&inner, &mut outbound, session, me, target);
        } else {
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
                error = Some(ERR_SESSION_LIMIT);
            } else {
                let session = fresh_session_id(&inner);
                inner.sessions.insert(
                    session,
                    SessionEntry {
                        endpoints: [me, target],
                        expires_at: Instant::now() + shared.limits.session_ttl,
                    },
                );
                log::info!(
                    "session {} opened for devices {}/{}, ttl {:?}",
                    util::hex_prefix(&session, 4),
                    util::hex_prefix(&me, 4),
                    util::hex_prefix(&target, 4),
                    shared.limits.session_ttl
                );
                announce_session(&inner, &mut outbound, session, me, target);
            }
        }
    }
    for (tx, msg) in outbound {
        deliver(&tx, msg).await;
    }
    if let Some(code) = error {
        deliver(
            out,
            CtrlMessage::ServerError {
                code,
                message: error_message(code),
            },
        )
        .await;
    }
}

async fn handle_signal(
    shared: &Shared,
    out: &mpsc::Sender<CtrlMessage>,
    me: DeviceId,
    session: SessionId,
    signal: PeerSignal,
) {
    let mut outbound: Vec<(mpsc::Sender<CtrlMessage>, CtrlMessage)> = Vec::new();
    let mut error: Option<u16> = None;
    {
        let mut inner = shared.lock();
        match inner.sessions.get(&session).cloned() {
            Some(entry) if entry.expires_at <= Instant::now() => {
                // Expired: drop it and tell the sender it is gone.
                outbound.extend(remove_sessions(&mut inner, &[session], None));
                error = Some(ERR_SESSION_NOT_FOUND);
            }
            Some(entry) if entry.endpoints.contains(&me) => {
                let other = if entry.endpoints[0] == me {
                    entry.endpoints[1]
                } else {
                    entry.endpoints[0]
                };
                if let Some(device) = inner.devices.get(&other) {
                    // Verbatim relay of one bounded, structured signal.
                    outbound.push((device.out.clone(), CtrlMessage::Signal { session, signal }));
                }
            }
            // Unknown session id, or one this device is not part of. Both
            // look identical to the sender: no existence oracle.
            _ => {
                error = Some(ERR_SESSION_NOT_FOUND);
            }
        }
    }
    for (tx, msg) in outbound {
        deliver(&tx, msg).await;
    }
    if let Some(code) = error {
        deliver(
            out,
            CtrlMessage::ServerError {
                code,
                message: error_message(code),
            },
        )
        .await;
    }
}

async fn handle_cancel(shared: &Shared, me: DeviceId, session: SessionId) {
    let outbound = {
        let mut inner = shared.lock();
        let participant = inner
            .sessions
            .get(&session)
            .map(|entry| entry.endpoints.contains(&me))
            .unwrap_or(false);
        if participant {
            remove_sessions(&mut inner, &[session], Some(&me))
        } else {
            // Cancelling an unknown session is idempotent, not an error.
            Vec::new()
        }
    };
    for (tx, msg) in outbound {
        deliver(&tx, msg).await;
    }
}

/// Encode helper re-exported for tests.
pub fn frame_bytes(frame: &CtrlFrame) -> Vec<u8> {
    let mut out = Vec::new();
    frame.encode(&mut out);
    out
}

/// Tests use the real `serve` loop on ephemeral ports.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) struct TestServer {
        pub tcp: SocketAddr,
        pub stun: SocketAddr,
        handle: ServerHandle,
        join: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    pub(crate) async fn start(limits: Limits) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stun_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tcp = listener.local_addr().unwrap();
        let stun = stun_socket.local_addr().unwrap();
        let (tx, rx) = watch::channel(false);
        let join = tokio::spawn(serve(listener, stun_socket, limits, rx));
        TestServer {
            tcp,
            stun,
            handle: ServerHandle { shutdown: tx },
            join,
        }
    }

    impl TestServer {
        pub(crate) async fn stop(self) -> std::io::Result<()> {
            let _ = self.handle.shutdown.send(true);
            self.join.await.unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support as client_support;
    use crate::client::SignalClient;
    use ed25519_dalek::Signer as _;
    use tokio::io::AsyncReadExt;

    async fn next_msg(client: &SignalClient) -> CtrlMessage {
        tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("timed out waiting for a server message")
            .expect("connection closed unexpectedly")
    }

    /// Drain until a ServerError shows up (TTL sweep Cancel frames and
    /// SameEgress hints may arrive first); fails on timeout.
    async fn next_server_error(client: &SignalClient) -> CtrlMessage {
        loop {
            match next_msg(client).await {
                msg @ CtrlMessage::ServerError { .. } => return msg,
                CtrlMessage::Cancel { .. } | CtrlMessage::SessionOpened { .. } => continue,
                CtrlMessage::Signal {
                    signal: PeerSignal::SameEgress,
                    ..
                } => continue,
                other => panic!("unexpected message: {other:?}"),
            }
        }
    }

    /// Consume the SameEgress hint that follows `SessionOpened` when both
    /// test clients share one address (loopback is always 127.0.0.1).
    async fn expect_same_egress(client: &SignalClient, session: SessionId) {
        match next_msg(client).await {
            CtrlMessage::Signal {
                session: hinted,
                signal: PeerSignal::SameEgress,
            } => assert_eq!(hinted, session),
            other => panic!("expected SameEgress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_devices_full_session_flow() {
        let server = test_support::start(Limits::default()).await;
        let key_a = client_support::key(1);
        let key_b = client_support::key(2);
        let a = client_support::connect(server.tcp, &key_a, "alpha").await;
        let b = client_support::connect(server.tcp, &key_b, "beta").await;

        a.register(&["192.168.1.10:50000".parse().unwrap()])
            .await
            .unwrap();
        b.register(&[
            "10.0.0.2:50001".parse().unwrap(),
            "[2001:db8::5]:50002".parse().unwrap(),
        ])
        .await
        .unwrap();

        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        let CtrlMessage::SessionOpened {
            session: session_a,
            peer: peer_a,
        } = next_msg(&a).await
        else {
            panic!("expected SessionOpened for a");
        };
        let CtrlMessage::SessionOpened {
            session: session_b,
            peer: peer_b,
        } = next_msg(&b).await
        else {
            panic!("expected SessionOpened for b");
        };
        assert_eq!(session_a, session_b);
        assert_eq!(peer_a, client_support::device(&key_b));
        assert_eq!(peer_b, client_support::device(&key_a));

        // Both endpoints dialed in from 127.0.0.1, so after the paired
        // SessionOpened each must receive the SameEgress hint.
        expect_same_egress(&a, session_a).await;
        expect_same_egress(&b, session_b).await;

        // Candidates flow A -> B.
        let candidates = vec![
            "192.168.1.10:50010".parse().unwrap(),
            "[2001:db8::a]:50011".parse().unwrap(),
        ];
        a.send_signal(session_a, PeerSignal::Candidates(candidates.clone()))
            .await
            .unwrap();
        match next_msg(&b).await {
            CtrlMessage::Signal { session, signal } => {
                assert_eq!(session, session_b);
                assert_eq!(signal, PeerSignal::Candidates(candidates));
            }
            other => panic!("expected Signal, got {other:?}"),
        }

        // Punch timing flows B -> A.
        let punch = PeerSignal::PunchAt {
            epoch_unix_ms: 1_234_567_890,
            nonce: [7; 8],
        };
        b.send_signal(session_b, punch.clone()).await.unwrap();
        match next_msg(&a).await {
            CtrlMessage::Signal { session, signal } => {
                assert_eq!(session, session_a);
                assert_eq!(signal, punch);
            }
            other => panic!("expected Signal, got {other:?}"),
        }

        // Cancel notifies the peer and frees the session.
        a.cancel(session_a).await.unwrap();
        match next_msg(&b).await {
            CtrlMessage::Cancel { session } => assert_eq!(session, session_b),
            other => panic!("expected Cancel, got {other:?}"),
        }
        a.send_signal(session_a, PeerSignal::Bye).await.unwrap();
        match next_server_error(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_NOT_FOUND),
            other => panic!("expected ServerError, got {other:?}"),
        }

        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn connect_to_unknown_device_is_rejected() {
        let server = test_support::start(Limits::default()).await;
        let a = client_support::connect(server.tcp, &client_support::key(1), "solo").await;
        a.connect_peer([9; 32]).await.unwrap();
        match next_msg(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_UNKNOWN_DEVICE),
            other => panic!("expected ServerError, got {other:?}"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn session_limit_and_pair_reuse() {
        let limits = Limits {
            max_sessions_per_device: 1,
            ..Limits::default()
        };
        let server = test_support::start(limits).await;
        let key_a = client_support::key(1);
        let key_b = client_support::key(2);
        let key_c = client_support::key(3);
        let a = client_support::connect(server.tcp, &key_a, "a").await;
        let b = client_support::connect(server.tcp, &key_b, "b").await;
        let c = client_support::connect(server.tcp, &key_c, "c").await;

        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        let CtrlMessage::SessionOpened { session: first, .. } = next_msg(&a).await else {
            panic!("expected SessionOpened");
        };
        let _ = next_msg(&b).await; // b's SessionOpened
        expect_same_egress(&a, first).await; // loopback: same egress hint
        expect_same_egress(&b, first).await;

        // a is now at its session limit; a second target is refused.
        a.connect_peer(client_support::device(&key_c))
            .await
            .unwrap();
        match next_server_error(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_LIMIT),
            other => panic!("expected ServerError, got {other:?}"),
        }

        // Reconnecting the same pair reuses the existing session.
        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        match next_msg(&a).await {
            CtrlMessage::SessionOpened { session, peer } => {
                assert_eq!(session, first);
                assert_eq!(peer, client_support::device(&key_b));
            }
            other => panic!("expected SessionOpened, got {other:?}"),
        }
        match next_msg(&b).await {
            CtrlMessage::SessionOpened { session, .. } => assert_eq!(session, first),
            other => panic!("expected SessionOpened, got {other:?}"),
        }
        // c must have received nothing.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), next_msg(&c))
                .await
                .is_err()
        );
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn frame_rate_limit_triggers_server_error() {
        let limits = Limits {
            frame_rate_per_sec: 1,
            frame_burst: 1, // Hello consumes the only token.
            ..Limits::default()
        };
        let server = test_support::start(limits).await;
        let a = client_support::connect(server.tcp, &client_support::key(1), "a").await;
        a.register(&["10.0.0.1:6000".parse().unwrap()])
            .await
            .unwrap();
        match next_msg(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_RATE_LIMITED),
            other => panic!("expected rate-limit ServerError, got {other:?}"),
        }
        // The connection is cut right after the rejection.
        match a.next().await {
            Err(crate::client::SignalError::Closed) => {}
            other => panic!("expected closed, got {other:?}"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stale_hello_is_disconnected() {
        let server = test_support::start(Limits::default()).await;
        let key = client_support::key(1);
        let result = client_support::connect_stale(server.tcp, &key, "late").await;
        assert!(matches!(result, Err(crate::client::SignalError::Closed)));
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn bad_signature_hello_is_disconnected() {
        let server = test_support::start(Limits::default()).await;
        let key = client_support::key(1);
        let result = client_support::connect_forged(server.tcp, &key, "evil").await;
        assert!(matches!(result, Err(crate::client::SignalError::Closed)));
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn first_frame_must_be_hello() {
        let server = test_support::start(Limits::default()).await;
        let mut stream = TcpStream::connect(server.tcp).await.unwrap();
        let register = frame_bytes(&CtrlFrame {
            version: codec::VERSION,
            message: CtrlMessage::Register { candidates: vec![] },
        });
        stream.write_all(&register).await.unwrap();
        let mut byte = [0u8; 1];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Ok(_) | Err(_) => panic!("expected the server to close the connection"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn oversized_frame_is_disconnected() {
        let server = test_support::start(Limits::default()).await;
        let mut stream = TcpStream::connect(server.tcp).await.unwrap();
        stream
            .write_all(&[codec::VERSION, 1, 0x0F, 0xFD])
            .await
            .unwrap(); // 4093 > cap
        let mut byte = [0u8; 1];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Ok(_) | Err(_) => panic!("expected the server to close the connection"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn undecodable_frame_is_disconnected() {
        let server = test_support::start(Limits::default()).await;
        let mut stream = TcpStream::connect(server.tcp).await.unwrap();
        // Valid header (Hello, 1 payload byte), unusable payload.
        stream
            .write_all(&[codec::VERSION, 1, 0, 1, 0])
            .await
            .unwrap();
        let mut byte = [0u8; 1];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Ok(_) | Err(_) => panic!("expected the server to close the connection"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn session_ttl_expiry_frees_session() {
        let limits = Limits {
            session_ttl: Duration::from_millis(150),
            ..Limits::default()
        };
        let server = test_support::start(limits).await;
        let key_a = client_support::key(1);
        let key_b = client_support::key(2);
        let a = client_support::connect(server.tcp, &key_a, "a").await;
        let b = client_support::connect(server.tcp, &key_b, "b").await;
        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        let CtrlMessage::SessionOpened { session, .. } = next_msg(&a).await else {
            panic!("expected SessionOpened");
        };
        let _ = next_msg(&b).await;
        expect_same_egress(&a, session).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        a.send_signal(session, PeerSignal::Bye).await.unwrap();
        match next_server_error(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_NOT_FOUND),
            other => panic!("expected ServerError, got {other:?}"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn client_disconnect_cancels_peer_sessions() {
        let server = test_support::start(Limits::default()).await;
        let key_a = client_support::key(1);
        let key_b = client_support::key(2);
        let a = client_support::connect(server.tcp, &key_a, "a").await;
        let b = client_support::connect(server.tcp, &key_b, "b").await;
        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        let CtrlMessage::SessionOpened { session, .. } = next_msg(&a).await else {
            panic!("expected SessionOpened");
        };
        let _ = next_msg(&b).await;
        expect_same_egress(&b, session).await;
        drop(a); // abrupt disconnect: server must clean up and notify b
        match next_msg(&b).await {
            CtrlMessage::Cancel { session: cancelled } => assert_eq!(cancelled, session),
            other => panic!("expected Cancel, got {other:?}"),
        }
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stun_responder_answers_binding_requests() {
        let server = test_support::start(Limits::default()).await;
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let txid = [5u8; 12];
        socket
            .send_to(&stun::binding_request(txid), server.stun)
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        let (len, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
            .await
            .expect("stun response timeout")
            .unwrap();
        let mapped = stun::parse_binding_response(&buf[..len], txid).unwrap();
        assert_eq!(mapped, socket.local_addr().unwrap());
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_device_connection_replaces_the_old_one() {
        let server = test_support::start(Limits::default()).await;
        let key = client_support::key(7);
        let first = client_support::connect(server.tcp, &key, "dup").await;
        let second = client_support::connect(server.tcp, &key, "dup").await;
        // The old connection is retired by the server.
        match tokio::time::timeout(Duration::from_secs(3), first.next()).await {
            Ok(Err(crate::client::SignalError::Closed)) => {}
            other => panic!("expected first connection to close, got {other:?}"),
        }
        // The replacement keeps working.
        second
            .register(&["10.1.1.1:7000".parse().unwrap()])
            .await
            .unwrap();
        drop(second);
        server.stop().await.unwrap();
    }

    // ---- Hello replay window (signaling security hardening) ----

    /// Read one length-prefixed frame from a raw TCP stream (None on close).
    async fn read_one_frame(stream: &mut TcpStream) -> Option<CtrlFrame> {
        let mut header = [0u8; codec::HEADER_LEN];
        stream.read_exact(&mut header).await.ok()?;
        let len = u16::from_be_bytes([header[2], header[3]]) as usize;
        let mut frame = header.to_vec();
        frame.resize(codec::HEADER_LEN + len, 0);
        stream
            .read_exact(&mut frame[codec::HEADER_LEN..])
            .await
            .ok()?;
        CtrlFrame::decode(&frame).ok()
    }

    /// A properly signed Hello resent byte-for-byte on a second connection
    /// is rejected as a suspected replay: the attacker sees an error naming
    /// the replay and a disconnect, and never takes over the registration.
    /// The same device reconnecting with a fresh signature still passes.
    #[tokio::test]
    async fn replayed_hello_signature_is_rejected() {
        let server = test_support::start(Limits::default()).await;
        let key = client_support::key(21);
        let device_id = client_support::device(&key);
        // fresh_now_unix_ms guarantees the later client_support::connect for
        // the same key gets a strictly later timestamp (a distinct signature).
        let timestamp = util::fresh_now_unix_ms();
        let signature = key
            .sign(&codec::hello_signing_bytes(
                1, &device_id, "replay", timestamp,
            ))
            .to_bytes();
        let mut replayed_bytes = Vec::new();
        CtrlFrame {
            version: codec::VERSION,
            message: CtrlMessage::Hello {
                version: 1,
                device_id,
                name: "replay".to_string(),
                timestamp_unix_ms: timestamp,
                signature,
            },
        }
        .encode(&mut replayed_bytes);

        // Legitimate first use: Welcome.
        let mut first = TcpStream::connect(server.tcp).await.unwrap();
        first.write_all(&replayed_bytes).await.unwrap();
        match read_one_frame(&mut first).await {
            Some(frame) => assert!(matches!(frame.message, CtrlMessage::Welcome { .. })),
            None => panic!("expected Welcome on the first connection"),
        }

        // Byte-identical replay on a fresh connection: ServerError naming the
        // replay suspicion, then the connection is cut.
        let mut second = TcpStream::connect(server.tcp).await.unwrap();
        second.write_all(&replayed_bytes).await.unwrap();
        match read_one_frame(&mut second).await {
            Some(frame) => match frame.message {
                CtrlMessage::ServerError { code, message } => {
                    assert_eq!(code, ERR_BAD_REQUEST);
                    assert!(
                        message.to_ascii_lowercase().contains("replay"),
                        "error must name the replay suspicion: {message}"
                    );
                }
                other => panic!("expected ServerError, got {other:?}"),
            },
            None => panic!("expected ServerError before the close"),
        }
        let mut byte = [0u8; 1];
        match second.read(&mut byte).await {
            Ok(0) | Err(_) => {}
            Ok(_) => panic!("expected the server to close after the replay rejection"),
        }

        // The same device with a fresh timestamp (new signature) still gets
        // in — and legitimately replaces its own registration.
        let fresh = client_support::connect(server.tcp, &key, "replay").await;
        fresh
            .register(&["10.9.9.9:7001".parse().unwrap()])
            .await
            .unwrap();
        server.stop().await.unwrap();
    }

    /// Cache semantics as a pure function: in-window duplicates are replays,
    /// distinct digests pass, expired entries stop matching, and the memory
    /// stays bounded at [`HELLO_REPLAY_CAPACITY`] (the evicted oldest digest
    /// is admitted again rather than falsely flagged).
    #[test]
    fn hello_replay_cache_window_and_capacity() {
        let mut cache = HelloReplayCache::default();
        let window = Duration::from_secs(10);
        let t0 = Instant::now();
        assert!(cache.admit([1; 32], t0, window));
        assert!(
            !cache.admit([1; 32], t0 + Duration::from_secs(5), window),
            "same digest inside the window is a replay"
        );
        assert!(
            cache.admit([2; 32], t0 + Duration::from_secs(5), window),
            "a distinct digest passes"
        );
        assert!(
            cache.admit([1; 32], t0 + Duration::from_secs(11), window),
            "the expired entry no longer matches"
        );
        // The t0 entry was purged as expired before the re-admit, leaving
        // the t0+5s entry and the freshly recorded one.
        assert_eq!(cache.seen.len(), 2);

        // Capacity bound: 256 distinct digests, then one more evicts the
        // oldest instead of growing.
        let mut bounded = HelloReplayCache::default();
        for i in 0..HELLO_REPLAY_CAPACITY {
            assert!(bounded.admit([i as u8; 32], t0, window));
        }
        assert_eq!(bounded.seen.len(), HELLO_REPLAY_CAPACITY);
        // A non-uniform digest distinct from every uniform pattern above.
        let mut distinct = [0u8; 32];
        for (j, byte) in distinct.iter_mut().enumerate() {
            *byte = j as u8;
        }
        assert!(bounded.admit(distinct, t0, window));
        assert_eq!(
            bounded.seen.len(),
            HELLO_REPLAY_CAPACITY,
            "capacity must not grow beyond the bound"
        );
        // The evicted oldest (i = 0) is admitted again — a cache miss, not a
        // false replay — and only then becomes an in-window duplicate.
        assert!(bounded.admit([0; 32], t0, window));
        assert!(!bounded.admit([0; 32], t0, window));
    }

    /// The replay digest is deterministic, mixes both inputs, and does not
    /// systematically collide across realistic signature variation.
    #[test]
    fn hello_digest_is_deterministic_and_input_sensitive() {
        let sig = [5u8; 64];
        let a = hello_digest(&[1; 32], &sig);
        assert_eq!(a, hello_digest(&[1; 32], &sig), "deterministic");
        assert_ne!(a, hello_digest(&[2; 32], &sig), "device_id is mixed in");
        assert_ne!(a, hello_digest(&[1; 32], &[6; 64]), "signature is mixed in");
        let mut seen = std::collections::HashSet::new();
        for i in 0..1000u64 {
            let mut sig = [0u8; 64];
            sig[..8].copy_from_slice(&i.to_le_bytes());
            assert!(
                seen.insert(hello_digest(&[1; 32], &sig)),
                "collision at {i}"
            );
        }
    }

    // ---- SameEgress hint (review 4.1) ----

    /// Forge a registry entry with the given observed peer IP.
    fn device_entry(ip: IpAddr) -> (DeviceEntry, mpsc::Receiver<CtrlMessage>) {
        let (tx, rx) = mpsc::channel(16);
        (
            DeviceEntry {
                conn_id: 1,
                out: tx,
                close: Arc::new(Notify::new()),
                candidates: Vec::new(),
                peer_ip: ip,
            },
            rx,
        )
    }

    fn shared_registry() -> Arc<Shared> {
        Arc::new(Shared {
            limits: Limits::default(),
            inner: Mutex::new(Inner::default()),
        })
    }

    /// Nothing else was queued on this device's outbound channel. A
    /// dropped sender side (retired connection) counts as empty too.
    fn assert_empty(rx: &mut mpsc::Receiver<CtrlMessage>) {
        if let Ok(message) = rx.try_recv() {
            panic!("expected an empty outbound queue, got {message:?}");
        }
    }

    #[test]
    fn same_egress_decision_branches() {
        let a: IpAddr = "192.168.1.5".parse().unwrap();
        let b: IpAddr = "203.0.113.7".parse().unwrap();
        assert!(same_egress(Some(a), Some(a)));
        assert!(!same_egress(Some(a), Some(b)));
        // An unknown address on either side never produces a hint.
        assert!(!same_egress(None, Some(a)));
        assert!(!same_egress(Some(a), None));
        assert!(!same_egress(None, None));
        // v4 and v6 loopback are distinct addresses, not a shared egress.
        let v6: IpAddr = "::1".parse().unwrap();
        let v4: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!same_egress(Some(v4), Some(v6)));
    }

    /// Forged internal state (no sockets): distinct recorded peer IPs get
    /// SessionOpened for both endpoints and no SameEgress, no error.
    #[tokio::test]
    async fn same_egress_not_sent_when_ips_differ() {
        let shared = shared_registry();
        let me = [1u8; 32];
        let target = [2u8; 32];
        let (entry_me, mut rx_me) = device_entry("198.51.100.1".parse().unwrap());
        let (entry_target, mut rx_target) = device_entry("203.0.113.7".parse().unwrap());
        {
            let mut inner = shared.lock();
            inner.devices.insert(me, entry_me);
            inner.devices.insert(target, entry_target);
        }
        let (requester, mut rx_requester) = mpsc::channel(8);
        handle_connect(&shared, &requester, me, target).await;

        match rx_me.try_recv() {
            Ok(CtrlMessage::SessionOpened { peer, .. }) => assert_eq!(peer, target),
            other => panic!("expected SessionOpened for requester, got {other:?}"),
        }
        match rx_target.try_recv() {
            Ok(CtrlMessage::SessionOpened { peer, .. }) => assert_eq!(peer, me),
            other => panic!("expected SessionOpened for target, got {other:?}"),
        }
        assert_empty(&mut rx_me); // no SameEgress for distinct IPs
        assert_empty(&mut rx_target);
        assert_empty(&mut rx_requester); // and no ServerError
    }

    /// Forged internal state: a same-IP pair is hinted, and a device
    /// reconnecting from a new IP (whole entry replaced, like the Hello
    /// path does) flips the pair-reuse announcement back to no hint.
    #[tokio::test]
    async fn same_egress_refreshes_when_device_reconnects() {
        let shared = shared_registry();
        let me = [1u8; 32];
        let target = [2u8; 32];
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let (entry_me, mut rx_me) = device_entry(loopback);
        let (entry_target, mut rx_target) = device_entry(loopback);
        {
            let mut inner = shared.lock();
            inner.devices.insert(me, entry_me);
            inner.devices.insert(target, entry_target);
        }
        let (requester, mut rx_requester) = mpsc::channel(8);
        handle_connect(&shared, &requester, me, target).await;

        let session = match rx_me.try_recv() {
            Ok(CtrlMessage::SessionOpened { session, .. }) => session,
            other => panic!("expected SessionOpened, got {other:?}"),
        };
        assert!(matches!(
            rx_target.try_recv(),
            Ok(CtrlMessage::SessionOpened { .. })
        ));
        assert!(matches!(
            rx_me.try_recv(),
            Ok(CtrlMessage::Signal {
                session: s,
                signal: PeerSignal::SameEgress
            }) if s == session
        ));
        assert!(matches!(
            rx_target.try_recv(),
            Ok(CtrlMessage::Signal {
                session: s,
                signal: PeerSignal::SameEgress
            }) if s == session
        ));

        // target reconnects from a different network: its whole entry,
        // peer IP included, is replaced.
        let (replacement, mut rx_replacement) = device_entry("203.0.113.7".parse().unwrap());
        shared.lock().devices.insert(target, replacement);

        // Pair reuse re-announces from the current records: no hint now.
        handle_connect(&shared, &requester, me, target).await;
        match rx_me.try_recv() {
            Ok(CtrlMessage::SessionOpened { session: s, peer }) => {
                assert_eq!(s, session); // reused, not a new session
                assert_eq!(peer, target);
            }
            other => panic!("expected reused SessionOpened, got {other:?}"),
        }
        assert!(matches!(
            rx_replacement.try_recv(),
            Ok(CtrlMessage::SessionOpened { .. })
        ));
        assert_empty(&mut rx_me);
        assert_empty(&mut rx_replacement);
        assert_empty(&mut rx_target); // retired connection gets nothing
        assert_empty(&mut rx_requester);
    }

    // ---- adversarial input hardening (round 7) ----

    /// Small deterministic LCG (Numerical Recipes constants): the fuzz
    /// corpora below must be reproducible run to run.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }

        /// Append `len` pseudo-random bytes to `out`.
        fn fill(&mut self, out: &mut Vec<u8>, len: usize) {
            while out.len() < len {
                let chunk = self.next_u64().to_le_bytes();
                let take = (len - out.len()).min(8);
                out.extend_from_slice(&chunk[..take]);
            }
        }
    }

    /// One adversarial frame, mixing four shapes: pure garbage of random
    /// length 0..=8192; a plausible header (valid version, in-range type)
    /// wrapped around garbage payload bytes of matching or random length; a
    /// declared payload length above the hard cap; and a half frame whose
    /// header claims more body bytes than are ever sent.
    fn fuzz_frame(rng: &mut Lcg) -> Vec<u8> {
        let mut buf = Vec::new();
        let shape = rng.below(4);
        if shape == 0 {
            let len = rng.below(8193) as usize;
            rng.fill(&mut buf, len);
        } else if shape == 1 {
            buf.push(codec::VERSION);
            buf.push(1 + rng.below(8) as u8);
            let declared = rng.below(4093) as usize; // at or under the cap
            buf.extend_from_slice(&(declared as u16).to_be_bytes());
            let body = if rng.below(2) == 0 {
                declared
            } else {
                rng.below(8193) as usize
            };
            rng.fill(&mut buf, body);
        } else if shape == 2 {
            buf.push(codec::VERSION);
            buf.push(1 + rng.below(8) as u8);
            let declared = 4093 + rng.below(61_443) as u16; // over the cap
            buf.extend_from_slice(&declared.to_be_bytes());
            let len = rng.below(65) as usize;
            rng.fill(&mut buf, len);
        } else {
            buf.push(codec::VERSION);
            buf.push(1 + rng.below(8) as u8);
            let declared = 1 + rng.below(4092) as usize;
            buf.extend_from_slice(&(declared as u16).to_be_bytes());
            rng.fill(&mut buf, declared / 2); // half frame, then reconnect
        }
        buf
    }

    /// Direct fuzzing of the live server: >=500 adversarial frames over raw
    /// TCP connections with repeated reconnects (including half-frame
    /// drops). The assertions are: the server never panics (this test
    /// completing is the proof); every illegal frame ends as a connection
    /// close or silent discard, never a hang; and legal clients can still
    /// complete the full Hello/Welcome handshake during and after the fuzz.
    #[tokio::test]
    async fn server_survives_raw_tcp_frame_fuzzing() {
        let server = test_support::start(Limits::default()).await;
        let mut rng = Lcg(0x243F_6A88_85A3_08D3);
        let mut sent = 0usize;
        let mut connections = 0usize;
        let mut probes = 0usize;
        tokio::time::timeout(Duration::from_secs(60), async {
            while sent < 500 {
                let mut stream = TcpStream::connect(server.tcp).await.unwrap();
                connections += 1;
                let frames_this_conn = 1 + rng.below(4) as usize;
                for _ in 0..frames_this_conn {
                    let frame = fuzz_frame(&mut rng);
                    // The server may already have closed this connection
                    // after an earlier illegal frame; a failed write counts
                    // as the "connection closed" outcome and moves on.
                    if stream.write_all(&frame).await.is_err() {
                        break;
                    }
                    sent += 1;
                }
                // Half-close our side, then observe the outcome: close
                // (EOF or reset) or silence (bytes silently discarded).
                let _ = stream.shutdown().await;
                let mut tail = [0u8; 128];
                let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
                loop {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break; // quiet connection: garbage was discarded
                    }
                    match tokio::time::timeout(remaining, stream.read(&mut tail)).await {
                        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                        Ok(Ok(_)) => continue, // queued error frame; drain to EOF
                    }
                }
                drop(stream);
                // Mid-fuzz liveness probe: a legal client must still get a
                // full Hello/Welcome while the fuzzing is ongoing.
                if connections.is_multiple_of(40) {
                    let seed = 10 + (connections as u8 / 40);
                    let probe = tokio::time::timeout(
                        Duration::from_secs(5),
                        client_support::connect(server.tcp, &client_support::key(seed), "probe"),
                    )
                    .await
                    .expect("server stopped serving legal clients mid-fuzz");
                    drop(probe);
                    probes += 1;
                }
            }
        })
        .await
        .expect("fuzz loop stalled: server wedged on adversarial input");
        assert!(sent >= 500, "fuzz must deliver at least 500 frames");
        assert!(connections > 20, "fuzz must exercise repeated reconnects");
        assert!(probes >= 3, "mid-fuzz liveness probes must have run");

        // The service was not wrecked: a fresh legal pair completes the
        // full Hello/Welcome handshake and opens a session afterwards.
        let key_a = client_support::key(1);
        let key_b = client_support::key(2);
        let a = tokio::time::timeout(
            Duration::from_secs(5),
            client_support::connect(server.tcp, &key_a, "alpha"),
        )
        .await
        .expect("post-fuzz Hello/Welcome timed out");
        let b = tokio::time::timeout(
            Duration::from_secs(5),
            client_support::connect(server.tcp, &key_b, "beta"),
        )
        .await
        .expect("post-fuzz Hello/Welcome timed out");
        a.connect_peer(client_support::device(&key_b))
            .await
            .unwrap();
        let CtrlMessage::SessionOpened { session, peer } = next_msg(&a).await else {
            panic!("expected SessionOpened after fuzzing");
        };
        assert_eq!(peer, client_support::device(&key_b));
        match next_msg(&b).await {
            CtrlMessage::SessionOpened {
                session: theirs, ..
            } => assert_eq!(theirs, session),
            other => panic!("expected SessionOpened for b, got {other:?}"),
        }
        server.stop().await.unwrap();
    }

    /// A correctly signed Hello whose device name carries control
    /// characters (NUL, terminal escapes) is refused at the codec layer, so
    /// the server drops the connection instead of ever storing or echoing
    /// the name (display-injection hardening; see codec tests).
    #[tokio::test]
    async fn hello_with_control_characters_in_name_is_disconnected() {
        let server = test_support::start(Limits::default()).await;
        let key = client_support::key(11);
        let device_id = client_support::device(&key);
        let name = "bad\u{0}\u{1b}[2J".to_string();
        let timestamp = util::now_unix_ms();
        let signature = key
            .sign(&codec::hello_signing_bytes(1, &device_id, &name, timestamp))
            .to_bytes();
        // Encoding a dirty name still works (encode is infallible); the
        // server-side decode is what must reject it.
        let mut bytes = Vec::new();
        CtrlFrame {
            version: codec::VERSION,
            message: CtrlMessage::Hello {
                version: 1,
                device_id,
                name,
                timestamp_unix_ms: timestamp,
                signature,
            },
        }
        .encode(&mut bytes);
        let mut stream = TcpStream::connect(server.tcp).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
        let mut byte = [0u8; 1];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Ok(_) | Err(_) => panic!("expected the server to close the connection"),
        }
        server.stop().await.unwrap();
    }

    /// Rate-limit bypass attempt: hammering ConnectRequest from one
    /// connection toward five distinct targets must hit SESSION_LIMIT on
    /// the fifth (per-device session cap) instead of growing server state,
    /// the refusal must be stable on repeat, and dropping the connection
    /// must release the quota (the same device reconnects and opens
    /// sessions again).
    #[tokio::test]
    async fn one_connection_cannot_bypass_session_limit() {
        let server = test_support::start(Limits::default()).await; // 4 sessions per device
        let key_a = client_support::key(1);
        let a = client_support::connect(server.tcp, &key_a, "a").await;
        let mut targets = Vec::new();
        for seed in 2u8..=6 {
            let target = client_support::connect(server.tcp, &client_support::key(seed), "t").await;
            targets.push(target);
        }
        let device_of = |seed: u8| client_support::device(&client_support::key(seed));

        // Four distinct targets, all opened over this single connection.
        for (i, seed) in (2u8..=5).enumerate() {
            a.connect_peer(device_of(seed)).await.unwrap();
            let CtrlMessage::SessionOpened { session, peer } = next_msg(&a).await else {
                panic!("expected SessionOpened toward target {seed}");
            };
            assert_eq!(peer, device_of(seed));
            expect_same_egress(&a, session).await;
            let CtrlMessage::SessionOpened {
                session: theirs,
                peer,
            } = next_msg(&targets[i]).await
            else {
                panic!("expected target {seed} to see the session");
            };
            assert_eq!(peer, client_support::device(&key_a));
            assert_eq!(theirs, session);
            expect_same_egress(&targets[i], theirs).await;
        }

        // Fifth distinct target from the same connection: refused, and the
        // refusal is stable on repeat rather than silently leaking growth.
        a.connect_peer(device_of(6)).await.unwrap();
        match next_server_error(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_LIMIT),
            other => panic!("expected SESSION_LIMIT, got {other:?}"),
        }
        a.connect_peer(device_of(6)).await.unwrap();
        match next_server_error(&a).await {
            CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_LIMIT),
            other => panic!("expected SESSION_LIMIT again, got {other:?}"),
        }

        // Dropping the connection releases the quota: the peer observes
        // the Cancel (cleanup ran; see also
        // client_disconnect_cancels_peer_sessions), and the same device
        // reconnecting can open a session again.
        drop(a);
        match next_msg(&targets[0]).await {
            CtrlMessage::Cancel { .. } => {}
            other => panic!("expected Cancel after disconnect, got {other:?}"),
        }
        let a2 = client_support::connect(server.tcp, &key_a, "a").await;
        a2.connect_peer(device_of(2)).await.unwrap();
        let CtrlMessage::SessionOpened { session, peer } = next_msg(&a2).await else {
            panic!("expected SessionOpened after quota release");
        };
        assert_eq!(peer, device_of(2));
        expect_same_egress(&a2, session).await;
        server.stop().await.unwrap();
    }

    /// 500 random 0..64-byte UDP datagrams with valid Binding requests
    /// interspersed: the STUN responder must never panic and every valid
    /// request must still get a correct, transaction-matching response
    /// amid the garbage — including one final request after the flood.
    #[tokio::test]
    async fn stun_responder_survives_malformed_datagram_flood() {
        let server = test_support::start(Limits::default()).await;
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let expected = socket.local_addr().unwrap();
        let mut rng = Lcg(0x4528_21E6_38D0_1377);
        let mut buf = [0u8; 128];
        let mut answered = 0usize;
        for round in 0..500usize {
            if round.is_multiple_of(10) {
                let txid = [(round / 10 + 1) as u8; 12];
                socket
                    .send_to(&stun::binding_request(txid), server.stun)
                    .await
                    .unwrap();
                let mapped = loop {
                    match tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
                        .await
                    {
                        Err(_) => panic!("no STUN response in round {round}"),
                        Ok(Err(e)) => panic!("recv failed in round {round}: {e}"),
                        Ok(Ok((len, from))) => {
                            assert_eq!(from, server.stun, "unexpected datagram source");
                            if let Ok(mapped) = stun::parse_binding_response(&buf[..len], txid) {
                                break mapped;
                            }
                            // Stray reply to an earlier request: keep waiting.
                        }
                    }
                };
                assert_eq!(mapped, expected);
                answered += 1;
            } else {
                let mut junk = Vec::new();
                let len = rng.below(65) as usize;
                rng.fill(&mut junk, len);
                socket.send_to(&junk, server.stun).await.unwrap();
            }
        }
        assert_eq!(answered, 50);
        // The responder is still alive after the whole flood.
        socket
            .send_to(&stun::binding_request([0xAA; 12]), server.stun)
            .await
            .unwrap();
        let mapped = loop {
            match tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf)).await {
                Err(_) => panic!("no final STUN response after flood"),
                Ok(Err(e)) => panic!("final recv failed: {e}"),
                Ok(Ok((len, _))) => {
                    if let Ok(mapped) = stun::parse_binding_response(&buf[..len], [0xAA; 12]) {
                        break mapped;
                    }
                }
            }
        };
        assert_eq!(mapped, expected);
        server.stop().await.unwrap();
    }
}
