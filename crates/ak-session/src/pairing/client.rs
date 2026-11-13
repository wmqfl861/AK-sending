//! Real v2 control-plane client for the authoritative registry (wave 21).
//!
//! [`RegistryClient`] speaks the frozen control protocol v2 against the
//! real `ak-signal::server_v2` registry: pinned TLS 1.3 via
//! [`ak_signal::server_v2::tls::connect_pinned`] (ALPN `ak-signal/2`), the
//! `AuthChallenge`/`Hello`/`Welcome` opening exchange signed over the
//! frozen [`hello2`] domains (TLS-exporter bound), then a single reader
//! task routing every inbound frame into bounded inboxes while a single
//! writer task serializes outbound frames.
//!
//! # Why not `ak_signal::client_v2::ClientV2`?
//!
//! The wave-20 client (`client_v2`) and server (`server_v2`) were built by
//! parallel agents and were never run against each other — `client_v2`'s
//! tests use a scripted fake registry and `server_v2`'s tests use raw TLS
//! connections. They diverged on three opening-exchange constants, any one
//! of which makes `ClientV2::connect` fail against `serve_v2`:
//!
//! | item | `client_v2` (frozen) | `server_v2` (frozen) |
//! |---|---|---|
//! | ALPN | `ak-ctrl/2` (`CTRL_ALPN`) | `ak-signal/2` (`ALPN_V2`, enforced) |
//! | pin | TLS verifies `Fingerprint::from_public_key(key)` **and** requires `AuthChallenge.registry_id == pinned fingerprint bytes` | certificate key **is** the `RegistryId`; `AuthChallenge.registry_id` is the raw public key — the two client checks are jointly unsatisfiable |
//! | exporter label | `AK-CTRL-V2-HELLO` | `AK control hello exporter` |
//!
//! Both crates are frozen for wave 21, so the interop bridge lives here:
//! this client is built exclusively from `server_v2`'s **public** surface
//! (`tls::connect_pinned`, `framing::{read_frame, write_message}`,
//! `hello2::*`, `protocol_v2` codec) and therefore signs exactly the bytes
//! the shipped server verifies. The method family mirrors `ClientV2` so a
//! future wave can swap the implementation behind the same calls once
//! `ak-signal` reconciles the constants.
//!
//! # Dispatch model
//!
//! Mirrors the v2 discipline: one reader, bounded per-key inboxes, drop
//! counting, idempotent request round-trips (same `request_id` re-sent on
//! timeout), reconnect with the frozen backoff, subscriptions that survive
//! reconnects. Two additions the orchestration needs:
//!
//! * a **pairing-plane** stream ([`RegistryClient::subscribe_plane`]) that
//!   receives *every* pairing-plane frame — including `InviteRedeemOffered`
//!   for pairings the client cannot know yet. The frozen `client_v2`
//!   dispatch drops owner-directed offers when nobody subscribed to the
//!   (still unknown) pairing id, which makes the owner side of the flow
//!   impossible; routing them to a plane stream fixes that without any
//!   protocol change, because the server keeps re-pushing pending offers
//!   on every Hello until the owner proof arrives.
//! * `PairChallenge` (no envelope id) is additionally routed to the
//!   per-pairing inbox of the pairing id embedded in its transcript, so a
//!   redeemer that already knows its pairing id sees it on one stream.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ak_identity::pairing_txn::random_id16;
use ak_identity::DeviceKeyPair;
use ak_signal::client_v2::{ClaimedInvite, PairStatus, RedeemAck, RevokedInvite};
use ak_signal::protocol_v2::{
    CodeId, CtrlFrame, CtrlMessage, DeviceId, Digest32, EndpointBlob, ErrorScope, InviteSummary,
    PairProof, PairingId, RedeemRequest, RegistryId, Role, Signature64, StateFilter, Transcript256,
};
use ak_signal::server_v2::framing::{read_frame, write_frame, write_message};
use ak_signal::server_v2::hello2;
use ak_signal::server_v2::tls as registry_tls;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch, Mutex as AsyncMutex};

use crate::pairing::error::ClientError;

/// Wall-clock Unix milliseconds (the coordination plane's time domain).
pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Byte offset of `pairing_id` inside the frozen 256-byte transcript
/// (routing-only knowledge; contents are ak-identity's business).
const TRANSCRIPT_PAIRING_OFFSET: usize = 32 + 16 + 8;

/// Extract the pairing id from a frozen transcript (routing only).
pub(crate) fn transcript_pairing_id(transcript: &Transcript256) -> PairingId {
    let mut id = [0u8; 16];
    id.copy_from_slice(&transcript[TRANSCRIPT_PAIRING_OFFSET..TRANSCRIPT_PAIRING_OFFSET + 16]);
    id
}

/// Tunables of [`RegistryClient`]; defaults mirror the frozen client_v2
/// budgets where they exist.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Per-attempt response timeout of a request round-trip.
    pub response_timeout: Duration,
    /// Re-sends of the same request id after a timeout (0 = one attempt).
    pub request_retries: u32,
    /// Capacity of one per-pairing inbox.
    pub inbox_capacity: usize,
    /// Capacity of one pairing-plane inbox.
    pub plane_capacity: usize,
    /// Depth of the single writer's outbound queue.
    pub outbox_capacity: usize,
    /// Connection attempts of one [`RegistryClient::reconnect`].
    pub reconnect_attempts: u32,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            response_timeout: Duration::from_secs(5),
            request_retries: 2,
            inbox_capacity: 64,
            plane_capacity: 128,
            outbox_capacity: 64,
            reconnect_attempts: 4,
        }
    }
}

/// Connection state reported through [`RegistryClient::state_watcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// A connect/reconnect attempt is running.
    Connecting,
    /// Live connection (generation / epoch).
    Connected(u64),
    /// The connection dropped; subscriptions survive, waiters were failed.
    Disconnected,
    /// [`RegistryClient::close`] completed.
    Closed,
}

/// Bounded dispatch state guarded by a std mutex (never held across await).
#[derive(Debug, Default)]
struct Dispatch {
    /// One-shot request waiters keyed by `RequestId` (and `Ping` nonce).
    waiters: HashMap<[u8; 16], oneshot::Sender<CtrlMessage>>,
    /// Durable per-pairing inboxes (survive reconnects).
    pairing: HashMap<PairingId, Vec<mpsc::Sender<CtrlMessage>>>,
    /// Pairing-plane subscribers: every pairing-plane frame, `ServerError`
    /// and pairing-/code-scoped `PairError`, copied to each.
    plane: Vec<mpsc::Sender<CtrlMessage>>,
    /// Wave-28 endpoint blobs relayed by extended frames
    /// (`InviteRedeemOffered`: the redeemer's settings; `PairCommit`: the
    /// owner's), keyed by pairing id and stored BEFORE the carrier frame
    /// is dispatched, so a subscriber that just saw the frame can
    /// [`RegistryClient::take_endpoint_blob`] it exactly once.
    endpoint_blobs: HashMap<PairingId, Vec<u8>>,
    /// Insertion order of `endpoint_blobs` for FIFO eviction. Wave-29
    /// (review P2-15): a pairing that never completes never takes its
    /// blob, so without a bound an idle registry connection relaying
    /// aborted flows would grow the cache forever. `MAX_ENDPOINT_BLOBS`
    /// evicts the oldest (they are connection HINTS, not credentials —
    /// an evicted flow simply re-derives its endpoint from the next
    /// relay frame or fails the pairing honestly).
    endpoint_blob_order: std::collections::VecDeque<PairingId>,
    /// The at-most-one in-flight G6 `ServerInfoRequest` waiter. The
    /// message pair carries no request id (idempotent, stateless), so it
    /// cannot ride the keyed waiter table; the single slot is enforced by
    /// [`ClientInner::info_query`] and cleared on disconnect.
    server_info_waiter: Option<oneshot::Sender<CtrlMessage>>,
}

/// Cache bound for relayed endpoint blobs (review P2-15). Generous
/// versus concurrent in-flight pairings (registry limits connections
/// anyway); the value only matters as a hard ceiling.
const MAX_ENDPOINT_BLOBS: usize = 256;

impl Dispatch {
    /// Bounded fan-out to one sender list; full inboxes drop and count.
    fn fan_out(senders: &[mpsc::Sender<CtrlMessage>], message: &CtrlMessage) -> bool {
        let mut delivered = false;
        for tx in senders {
            if tx.try_send(message.clone()).is_ok() {
                delivered = true;
            }
        }
        delivered
    }
}

struct ClientInner {
    addr: SocketAddr,
    registry_id: RegistryId,
    key: DeviceKeyPair,
    name: String,
    config: ClientConfig,

    dispatch: StdMutex<Dispatch>,
    outbox: StdMutex<mpsc::Sender<CtrlFrame>>,
    lifecycle: AsyncMutex<()>,

    epoch: AtomicU64,
    connected: AtomicBool,
    closed: AtomicBool,
    replaced: AtomicBool,
    /// The CURRENT disconnect was a replacement kick (set in
    /// [`ClientInner::note_replaced`], cleared on the next successful
    /// [`ClientState::Connected`]). Unlike `replaced` (sticky, surfaces
    /// the event to the UI), this tells the connection keeper how to
    /// pace its reclaim — see [`KEEPER_REPLACEMENT_PAUSE`].
    replace_closed: AtomicBool,
    /// The `Welcome.capabilities` the CURRENT connection's Welcome
    /// carried (G6: 0 before the first Welcome; refreshed per connection
    /// epoch — reconnects re-read it, so a server downgrade mid-run is
    /// noticed on the next [`RegistryClient::server_info`] call).
    welcome_caps: AtomicU32,
    /// Per-connection-epoch cache of the G6 `ServerInfo` answer (cleared
    /// on every reconnect, so a server upgrade is re-queried).
    server_info_cache: StdMutex<Option<String>>,
    /// Single-flight gate for the G6 revision query (the wire pair has no
    /// request id; at most one query is in flight per client).
    info_query: AsyncMutex<()>,
    unmatched: AtomicU64,
    state_tx: watch::Sender<ClientState>,
}

impl ClientInner {
    /// Route one inbound frame (single-reader context, no await inside).
    /// Wave-28: the extension section is peeled off first — an endpoint
    /// blob on `InviteRedeemOffered` / `PairCommit` lands in the dispatch
    /// side-map keyed by the frame's pairing id BEFORE the carrier message
    /// fans out, so the flow that consumes the frame can take the blob
    /// with no ordering race.
    fn route(&self, frame: CtrlFrame) {
        let message = frame.message;
        let mut dispatch = self.dispatch.lock().expect("client dispatch lock");
        if let Some(EndpointBlob(blob)) = frame.endpoint {
            let pairing_id = match &message {
                CtrlMessage::InviteRedeemOffered { pairing_id, .. } => Some(*pairing_id),
                CtrlMessage::PairCommit { transcript, .. } => {
                    Some(transcript_pairing_id(transcript))
                }
                _ => None,
            };
            if let Some(pairing_id) = pairing_id {
                let blobs = &mut dispatch.endpoint_blobs;
                if blobs
                    .entry(pairing_id)
                    .or_insert_with(|| blob.clone())
                    .len()
                    == blob.len()
                    && !dispatch.endpoint_blob_order.contains(&pairing_id)
                {
                    dispatch.endpoint_blob_order.push_back(pairing_id);
                    while dispatch.endpoint_blob_order.len() > MAX_ENDPOINT_BLOBS {
                        if let Some(oldest) = dispatch.endpoint_blob_order.pop_front() {
                            dispatch.endpoint_blobs.remove(&oldest);
                        }
                    }
                }
            }
        }
        // Terminal pairing states drop their cached blob (review P2-15):
        // aborted/failed flows never take it, so recycle eagerly.
        {
            let terminal_id = match &message {
                CtrlMessage::PairError {
                    scope, object_id, ..
                } if *scope == ErrorScope::Pairing => Some(*object_id),
                _ => None,
            };
            if let Some(pairing_id) = terminal_id {
                if dispatch.endpoint_blobs.remove(&pairing_id).is_some() {
                    dispatch.endpoint_blob_order.retain(|id| *id != pairing_id);
                }
            }
        }
        let mut delivered = false;
        match &message {
            CtrlMessage::InviteClaimed { request_id, .. }
            | CtrlMessage::InviteListPage { request_id, .. }
            | CtrlMessage::InviteRevoked { request_id, .. }
            | CtrlMessage::InviteRedeemPending { request_id, .. }
            | CtrlMessage::PairStatusReply { request_id, .. } => {
                if let Some(tx) = dispatch.waiters.remove(request_id) {
                    delivered = tx.send(message).is_ok() || delivered;
                }
            }
            CtrlMessage::Pong { nonce } => {
                if let Some(tx) = dispatch.waiters.remove(nonce) {
                    delivered = tx.send(message).is_ok() || delivered;
                }
            }
            CtrlMessage::PairError {
                request_id,
                scope,
                object_id,
                ..
            } => {
                if let Some(tx) = dispatch.waiters.remove(request_id) {
                    delivered = tx.send(message.clone()).is_ok() || delivered;
                }
                if *scope == ErrorScope::Pairing {
                    // Pairing-scoped failures are also pairing-plane events
                    // (deadline ghosts, revoke aborts, sweep aborts).
                    if let Some(inboxes) = dispatch.pairing.get(object_id) {
                        delivered |= Dispatch::fan_out(inboxes, &message);
                    }
                    delivered |= Dispatch::fan_out(&dispatch.plane, &message);
                }
                if *scope == ErrorScope::Code {
                    // Code-scoped failures name no pairing and carry no
                    // usable request id (the commit-time REDEEM_EXHAUSTED
                    // refusal is sent with request_id all-zero), so the
                    // waiter path cannot see them. Fan them to the
                    // pairing plane (wave 30, review R29-P2-01): the
                    // in-flight owner/redeem flows subscribed there
                    // attribute the error to their own pairing (see the
                    // orchestrator's `wait_frame`) instead of waiting out
                    // their commit deadline.
                    delivered |= Dispatch::fan_out(&dispatch.plane, &message);
                }
            }
            CtrlMessage::InviteRedeemOffered { pairing_id, .. }
            | CtrlMessage::PairProof { pairing_id, .. }
            | CtrlMessage::PairReady { pairing_id, .. } => {
                if let Some(inboxes) = dispatch.pairing.get(pairing_id) {
                    delivered |= Dispatch::fan_out(inboxes, &message);
                }
                delivered |= Dispatch::fan_out(&dispatch.plane, &message);
            }
            CtrlMessage::PairCommit { transcript, .. } => {
                let pid = transcript_pairing_id(transcript);
                if let Some(inboxes) = dispatch.pairing.get(&pid) {
                    delivered |= Dispatch::fan_out(inboxes, &message);
                }
                delivered |= Dispatch::fan_out(&dispatch.plane, &message);
            }
            CtrlMessage::PairChallenge { transcript, .. } => {
                // No envelope id; the embedded pairing id still routes it to
                // a redeemer that subscribed to its own pairing.
                let pid = transcript_pairing_id(transcript);
                if let Some(inboxes) = dispatch.pairing.get(&pid) {
                    delivered |= Dispatch::fan_out(inboxes, &message);
                }
                delivered |= Dispatch::fan_out(&dispatch.plane, &message);
            }
            CtrlMessage::ServerError { code, .. } => {
                if *code == ak_signal::client_v2::ERR_DEVICE_REPLACED {
                    self.note_replaced();
                }
                // A pending G6 revision query degrades on an error answer
                // (e.g. a proxy that set the capability bit but cannot
                // answer): hand the frame to the waiter — it maps any
                // non-ServerInfo reply to None — and keep the plane copy.
                if let Some(tx) = dispatch.server_info_waiter.take() {
                    delivered |= tx.send(message.clone()).is_ok();
                }
                delivered |= Dispatch::fan_out(&dispatch.plane, &message);
            }
            CtrlMessage::ServerInfo { .. } => {
                // G6: the revision answer has no request id — the single
                // in-flight slot is its dispatch. Without a waiter it is
                // an unsolicited push (the server never does this) and
                // counts as unmatched like every other undeliverable.
                if let Some(tx) = dispatch.server_info_waiter.take() {
                    delivered |= tx.send(message).is_ok();
                }
            }
            // Client-authored and handshake frames never arrive here (the
            // direction layer rejects them at decode).
            _ => {}
        }
        dispatch.plane.retain(|tx| !tx.is_closed());
        for inboxes in dispatch.pairing.values_mut() {
            inboxes.retain(|tx| !tx.is_closed());
        }
        drop(dispatch);
        if !delivered {
            let n = self.unmatched.fetch_add(1, Ordering::AcqRel) + 1;
            log::debug!("pairing client: dropped unmatched frame (total {n})");
        }
    }

    fn note_replaced(&self) {
        self.replace_closed.store(true, Ordering::Release);
        if !self.replaced.swap(true, Ordering::AcqRel) {
            log::warn!(
                "pairing client: this control connection was replaced by a \
                 newer connection of the same device id"
            );
        }
    }

    /// Fail every one-shot waiter (disconnect) — their receivers observe a
    /// closed channel and map it to `ConnectionLost`/`Replaced`. Relayed
    /// endpoint blobs of pairings that never completed drop with them.
    /// The G6 revision query degrades (None) and its per-epoch cache
    /// drops with the connection that filled it.
    fn clear_waiters(&self) {
        let mut dispatch = self.dispatch.lock().expect("client dispatch lock");
        dispatch.waiters.clear();
        dispatch.server_info_waiter = None;
        dispatch.endpoint_blobs.clear();
        dispatch.endpoint_blob_order.clear();
        drop(dispatch);
        if let Ok(mut cache) = self.server_info_cache.lock() {
            *cache = None;
        }
    }

    /// Queue one plain message for the writer (bounded, non-blocking).
    fn send_frame(&self, message: CtrlMessage) -> Result<(), ClientError> {
        self.send_frame_with(CtrlFrame::new(message))
    }

    /// Queue one full frame — message plus optional wave-28 extension
    /// section (endpoint blob on `InviteClaim`/`InviteRedeem`, claim
    /// `max_redeems`) — for the writer (bounded, non-blocking).
    fn send_frame_with(&self, frame: CtrlFrame) -> Result<(), ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Closed);
        }
        if !self.connected.load(Ordering::Acquire) {
            return Err(ClientError::Offline);
        }
        let tx = self.outbox.lock().expect("client outbox lock").clone();
        match tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::debug!("pairing client: writer queue full, frame dropped");
                Err(ClientError::Offline)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientError::ConnectionLost),
        }
    }

    fn on_disconnect(&self, epoch: u64) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.epoch.load(Ordering::Acquire) != epoch {
            return; // a newer generation owns the state now
        }
        if !self.connected.swap(false, Ordering::AcqRel) {
            return;
        }
        self.clear_waiters();
        self.state_tx.send_replace(ClientState::Disconnected);
    }
}

/// Removes the waiter registration when the request future is dropped or
/// finishes (cancellation must not wedge the key).
struct WaiterGuard<'a> {
    inner: &'a ClientInner,
    key: [u8; 16],
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.inner
            .dispatch
            .lock()
            .expect("client dispatch lock")
            .waiters
            .remove(&self.key);
    }
}

/// Connected, Hello-authenticated v2 control client (see the module docs).
///
/// Cheap to clone (`Arc` inside): one device must share **one** instance —
/// the registry replaces older connections of the same device id.
#[derive(Clone)]
pub struct RegistryClient {
    inner: Arc<ClientInner>,
}

impl RegistryClient {
    /// Connect with the default configuration.
    pub async fn connect(
        addr: SocketAddr,
        registry_id: RegistryId,
        key: &DeviceKeyPair,
        name: &str,
    ) -> Result<Self, ClientError> {
        Self::connect_with_config(addr, registry_id, key, name, ClientConfig::default()).await
    }

    /// [`RegistryClient::connect`] with explicit tunables.
    pub async fn connect_with_config(
        addr: SocketAddr,
        registry_id: RegistryId,
        key: &DeviceKeyPair,
        name: &str,
        config: ClientConfig,
    ) -> Result<Self, ClientError> {
        let (state_tx, _) = watch::channel(ClientState::Connecting);
        let inner = Arc::new(ClientInner {
            addr,
            registry_id,
            key: key.clone(),
            name: name.to_string(),
            config,
            dispatch: StdMutex::new(Dispatch::default()),
            outbox: StdMutex::new(mpsc::channel(1).0),
            lifecycle: AsyncMutex::new(()),
            epoch: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            replaced: AtomicBool::new(false),
            replace_closed: AtomicBool::new(false),
            welcome_caps: AtomicU32::new(0),
            server_info_cache: StdMutex::new(None),
            info_query: AsyncMutex::new(()),
            unmatched: AtomicU64::new(0),
            state_tx,
        });
        check_display_name(&inner.name)?;
        open_connection(&inner).await?;
        Ok(RegistryClient { inner })
    }

    /// This device's id (the key's public bytes).
    pub fn device_id(&self) -> DeviceId {
        self.inner.key.public_key()
    }

    /// The wire-legal display name this client Hello'd with (wave 28:
    /// task-pairing endpoint blobs carry it as the human-readable label).
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Sign one of the frozen control-plane domains with this device's key
    /// (crate-internal use: the [`crate::pairing::RealTransport`] adapter
    /// signs `PairPrepared` confirmations here).
    pub(crate) fn sign_with_device_key(&self, message: &[u8]) -> [u8; 64] {
        self.inner.key.sign(message)
    }

    /// The pinned registry id (its Ed25519 public key).
    pub fn registry_id(&self) -> RegistryId {
        self.inner.registry_id
    }

    /// Effective configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// Current connection generation (bumped per successful reconnect).
    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Acquire)
    }

    /// Whether the control connection is currently up.
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    /// Sticky: a newer connection of the same device id took over.
    pub fn is_replaced(&self) -> bool {
        self.inner.replaced.load(Ordering::Acquire)
    }

    /// Whether the CURRENT disconnect was caused by a replacement kick
    /// (crate-private: the connection keeper's pacing input; cleared on
    /// the next successful connection). Distinct from the sticky
    /// [`RegistryClient::is_replaced`] event flag.
    pub(crate) fn last_close_was_replacement(&self) -> bool {
        self.inner.replace_closed.load(Ordering::Acquire)
    }

    /// Connection state changes.
    pub fn state_watcher(&self) -> watch::Receiver<ClientState> {
        self.inner.state_tx.subscribe()
    }

    /// Frames that matched no waiter, inbox or plane subscriber.
    pub fn dropped_unmatched_frames(&self) -> u64 {
        self.inner.unmatched.load(Ordering::Acquire)
    }

    /// Subscribe to one pairing transaction's stream (Offered / PairProof /
    /// PairCommit / PairReady / pairing-scoped PairError). Durable across
    /// reconnects; multiple subscribers allowed; bounded.
    pub fn subscribe_pairing(&self, pairing_id: PairingId) -> mpsc::Receiver<CtrlMessage> {
        let (tx, rx) = mpsc::channel(self.inner.config.inbox_capacity.max(1));
        self.inner
            .dispatch
            .lock()
            .expect("client dispatch lock")
            .pairing
            .entry(pairing_id)
            .or_default()
            .push(tx);
        rx
    }

    /// Subscribe to the pairing plane: every pairing-plane frame this client
    /// receives (including `InviteRedeemOffered` for not-yet-known pairing
    /// ids and relayed `PairChallenge`), plus `ServerError` and
    /// pairing-scoped `PairError`, plus code-scoped `PairError` (the
    /// commit-time `REDEEM_EXHAUSTED` refusal names a code, not a
    /// pairing — wave 30, review R29-P2-01). This is the owner-side
    /// discovery stream; see the module docs.
    pub fn subscribe_plane(&self) -> mpsc::Receiver<CtrlMessage> {
        let (tx, rx) = mpsc::channel(self.inner.config.plane_capacity.max(1));
        self.inner
            .dispatch
            .lock()
            .expect("client dispatch lock")
            .plane
            .push(tx);
        rx
    }

    /// Take (once) the wave-28 endpoint blob a relayed frame carried for
    /// `pairing_id`: the redeemer's settings off `InviteRedeemOffered`
    /// (owner side) or the owner's settings off `PairCommit` (redeemer
    /// side). The blob is stored before its carrier frame is dispatched,
    /// so a flow that just consumed the frame on any inbox or the plane
    /// finds it here; `None` means the peer predates the extension or the
    /// blob was already taken.
    pub fn take_endpoint_blob(&self, pairing_id: PairingId) -> Option<Vec<u8>> {
        let mut dispatch = self.inner.dispatch.lock().expect("client dispatch lock");
        let taken = dispatch.endpoint_blobs.remove(&pairing_id);
        if taken.is_some() {
            dispatch.endpoint_blob_order.retain(|id| *id != pairing_id);
        }
        taken
    }

    /// Peek (clone, without consuming) the wave-28 endpoint blob a relayed
    /// frame carried for `pairing_id`. The pre-commit redeem-offer gate
    /// (review 2026-11-06, P1-05) reads the redeemer's settings this way
    /// while the pairing flow that later completes still finds the blob
    /// for its exactly-once [`RegistryClient::take_endpoint_blob`].
    /// `None` means the peer predates the extension.
    pub fn peek_endpoint_blob(&self, pairing_id: PairingId) -> Option<Vec<u8>> {
        self.inner
            .dispatch
            .lock()
            .expect("client dispatch lock")
            .endpoint_blobs
            .get(&pairing_id)
            .cloned()
    }

    // ---- request API -------------------------------------------------------

    /// Register a freshly generated invite code by lookup digest (fresh
    /// CSPRNG request id; the plaintext secret never travels).
    pub async fn invite_claim(
        &self,
        lookup_digest: Digest32,
        ttl_secs: u32,
    ) -> Result<ClaimedInvite, ClientError> {
        self.invite_claim_with_id(random_id16(), lookup_digest, ttl_secs)
            .await
    }

    /// [`RegistryClient::invite_claim`] with a caller-chosen request id —
    /// the idempotency key. Re-sending the same id (same digest, same ttl)
    /// replays the stored answer, so a network retry yields exactly one
    /// code.
    pub async fn invite_claim_with_id(
        &self,
        request_id: [u8; 16],
        lookup_digest: Digest32,
        ttl_secs: u32,
    ) -> Result<ClaimedInvite, ClientError> {
        self.invite_claim_frame(request_id, lookup_digest, ttl_secs, None, 0)
            .await
    }

    /// Wave-28 task-code claim ([`RegistryClient::invite_claim_with_id`
    /// semantics]) with the endpoint blob and the redeem cap of the v1
    /// model riding the frame's extension section: `endpoint` is the
    /// initiator's connection settings the registry stores on the code row
    /// and relays to the joiner inside `PairCommit`; `max_redeems = 1`
    /// makes the code single-use (0 = unlimited, the legacy default).
    pub async fn invite_claim_task(
        &self,
        request_id: [u8; 16],
        lookup_digest: Digest32,
        ttl_secs: u32,
        endpoint: Option<EndpointBlob>,
        max_redeems: u32,
    ) -> Result<ClaimedInvite, ClientError> {
        self.invite_claim_frame(request_id, lookup_digest, ttl_secs, endpoint, max_redeems)
            .await
    }

    /// Shared claim round-trip (plain or task-shaped).
    async fn invite_claim_frame(
        &self,
        request_id: [u8; 16],
        lookup_digest: Digest32,
        ttl_secs: u32,
        endpoint: Option<EndpointBlob>,
        max_redeems: u32,
    ) -> Result<ClaimedInvite, ClientError> {
        let mut frame = CtrlFrame::new(CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        });
        if let Some(blob) = endpoint {
            frame = frame.with_endpoint(blob);
        }
        if max_redeems > 0 {
            frame = frame.with_max_redeems(max_redeems);
        }
        let response = self.roundtrip_frame(request_id, frame).await?;
        match response {
            CtrlMessage::InviteClaimed {
                code_id,
                created_ms,
                expires_ms,
                revision,
                ..
            } => Ok(ClaimedInvite {
                request_id,
                code_id,
                created_ms,
                expires_ms,
                revision,
            }),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to InviteClaim (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// Page through this device's own invites.
    pub async fn invite_list(
        &self,
        cursor: CodeId,
        limit: u8,
        state_filter: StateFilter,
    ) -> Result<(CodeId, Vec<InviteSummary>), ClientError> {
        let request_id = random_id16();
        let response = self
            .roundtrip(
                request_id,
                CtrlMessage::InviteList {
                    request_id,
                    cursor,
                    limit,
                    state_filter,
                },
            )
            .await?;
        match response {
            CtrlMessage::InviteListPage {
                next_cursor,
                entries,
                ..
            } => Ok((next_cursor, entries)),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to InviteList (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// Revoke one of this device's invite codes (optimistic concurrency on
    /// `expected_revision`). Revocation aborts pending redeems of the code
    /// server-side.
    pub async fn invite_revoke(
        &self,
        code_id: CodeId,
        expected_revision: u64,
    ) -> Result<RevokedInvite, ClientError> {
        let request_id = random_id16();
        let response = self
            .roundtrip(
                request_id,
                CtrlMessage::InviteRevoke {
                    request_id,
                    code_id,
                    expected_revision,
                },
            )
            .await?;
        match response {
            CtrlMessage::InviteRevoked {
                code_id,
                revision,
                revoked_ms,
                ..
            } => Ok(RevokedInvite {
                request_id,
                code_id,
                revision,
                revoked_ms,
            }),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to InviteRevoke (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// Start a redeem attempt with the fixed request structure (built by
    /// the orchestrator). On timeout the same request id is re-sent.
    pub async fn invite_redeem(&self, redeem: RedeemRequest) -> Result<RedeemAck, ClientError> {
        self.invite_redeem_frame(redeem, None).await
    }

    /// Wave-28 task-code redeem ([`RegistryClient::invite_redeem`
    /// semantics]) with the joiner's endpoint blob riding the frame's
    /// extension section: the registry stores it on the pairing row and
    /// forwards it to the code owner inside `InviteRedeemOffered`.
    pub async fn invite_redeem_with_endpoint(
        &self,
        redeem: RedeemRequest,
        endpoint: EndpointBlob,
    ) -> Result<RedeemAck, ClientError> {
        self.invite_redeem_frame(redeem, Some(endpoint)).await
    }

    /// Shared redeem round-trip (plain or task-shaped).
    async fn invite_redeem_frame(
        &self,
        redeem: RedeemRequest,
        endpoint: Option<EndpointBlob>,
    ) -> Result<RedeemAck, ClientError> {
        let request_id = redeem.request_id;
        let mut frame = CtrlFrame::new(CtrlMessage::InviteRedeem(redeem));
        if let Some(blob) = endpoint {
            frame = frame.with_endpoint(blob);
        }
        let response = self.roundtrip_frame(request_id, frame).await?;
        match response {
            CtrlMessage::InviteRedeemPending {
                pairing_id,
                deadline_ms,
                ..
            } => Ok(RedeemAck {
                request_id,
                pairing_id,
                deadline_ms,
            }),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to InviteRedeem (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// Query one pairing transaction's state (participants only). After
    /// Commit/Ready the server re-sends the fixed `PairCommit`/`PairReady`
    /// on the pairing stream right after the reply — the recovery path.
    pub async fn pair_status_query(
        &self,
        pairing_id: PairingId,
    ) -> Result<PairStatus, ClientError> {
        let request_id = random_id16();
        let response = self
            .roundtrip(
                request_id,
                CtrlMessage::PairStatusQuery {
                    request_id,
                    pairing_id,
                },
            )
            .await?;
        match response {
            CtrlMessage::PairStatusReply {
                state,
                commit_revision,
                deadline_ms,
                ..
            } => Ok(PairStatus {
                request_id,
                pairing_id,
                state,
                commit_revision,
                deadline_ms,
            }),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to PairStatusQuery (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// Abort an uncommitted pairing (fire-and-forget, idempotency id
    /// attached for server-side logging).
    pub async fn pair_abort(
        &self,
        pairing_id: PairingId,
        reason: ak_signal::protocol_v2::AbortReason,
    ) -> Result<(), ClientError> {
        self.inner.send_frame(CtrlMessage::PairAbort {
            request_id: random_id16(),
            pairing_id,
            reason,
        })
    }

    /// Owner side: forward the owner proof (and the frozen transcript) to
    /// the redeemer; the server relays verbatim.
    pub async fn send_pair_challenge(
        &self,
        transcript: Transcript256,
        owner_proof: PairProof,
    ) -> Result<(), ClientError> {
        self.inner.send_frame(CtrlMessage::PairChallenge {
            transcript,
            owner_proof,
        })
    }

    /// Redeemer side: answer a challenge with the redeemer proof.
    pub async fn send_pair_proof(
        &self,
        pairing_id: PairingId,
        transcript_digest: Digest32,
        redeemer_proof: PairProof,
    ) -> Result<(), ClientError> {
        self.inner.send_frame(CtrlMessage::PairProof {
            pairing_id,
            transcript_digest,
            redeemer_proof,
        })
    }

    /// One side's durable prepared record (`PairPrepared`).
    pub async fn send_pair_prepared(
        &self,
        pairing_id: PairingId,
        transcript_digest: Digest32,
        signature: Signature64,
    ) -> Result<(), ClientError> {
        self.inner.send_frame(CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest,
            signature,
        })
    }

    /// One side's confirmation that the local trust book was applied.
    pub async fn send_pair_applied(
        &self,
        pairing_id: PairingId,
        transcript_digest: Digest32,
        commit_revision: u64,
        signature: Signature64,
    ) -> Result<(), ClientError> {
        self.inner.send_frame(CtrlMessage::PairApplied {
            pairing_id,
            transcript_digest,
            commit_revision,
            signature,
        })
    }

    /// Keepalive round-trip.
    pub async fn ping(&self) -> Result<(), ClientError> {
        let nonce = random_id16();
        let response = self.roundtrip(nonce, CtrlMessage::Ping { nonce }).await?;
        match response {
            CtrlMessage::Pong { nonce: got } if got == nonce => Ok(()),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to Ping (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    /// The `Welcome.capabilities` bits of the CURRENT connection epoch
    /// (G6; 0 before the first Welcome). A diagnostic surface — the one
    /// decision gated on it is [`RegistryClient::server_info`].
    pub fn welcome_capabilities(&self) -> u32 {
        self.inner.welcome_caps.load(Ordering::Acquire)
    }

    /// The build revision of the serving registry (G6, contract
    /// protocol-v2.md §6a), e.g. `"0.1.0+4739f4d"`.
    ///
    /// `None` means "no information" and is NEVER an error:
    ///
    /// * the current server did not advertise the revision-query
    ///   capability bit — an older build, which must not be asked (the
    ///   pure-addition interop rule: old server × new client behaves
    ///   exactly as before);
    /// * or the query failed after the bit was seen (no answer within
    ///   the response budget, a `ServerError` answer, or the connection
    ///   dropped) — a diagnostic convenience query must not fault the
    ///   caller's control connection usage.
    ///
    /// The answer is cached per connection epoch (a reconnect re-queries
    /// against the new server build); at most one query is in flight
    /// (the wire pair carries no request id).
    pub async fn server_info(&self) -> Option<String> {
        const INFO_CAP: u32 = ak_signal::server_v2::CAP_SERVER_INFO;
        if self.welcome_capabilities() & INFO_CAP == 0 {
            return None; // old server: never asked
        }
        // Single flight (no request id on the wire) — and the cache is
        // re-checked under the gate so concurrent callers share one query.
        let _inflight = self.inner.info_query.lock().await;
        if let Some(cached) = self
            .inner
            .server_info_cache
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        {
            return Some(cached);
        }
        if self.welcome_capabilities() & INFO_CAP == 0 {
            return None; // re-check: the epoch may have changed while queued
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut dispatch = self.inner.dispatch.lock().expect("client dispatch lock");
            if dispatch.server_info_waiter.is_some() {
                return None; // unreachable behind the gate; kept defensive
            }
            dispatch.server_info_waiter = Some(tx);
        }
        let reply = match self.inner.send_frame(CtrlMessage::ServerInfoRequest) {
            Err(error) => {
                log::debug!("pairing client: server-info query not sent: {error}");
                None
            }
            Ok(()) => match tokio::time::timeout(self.inner.config.response_timeout, rx).await {
                Ok(Ok(CtrlMessage::ServerInfo { revision })) => Some(revision),
                Ok(Ok(CtrlMessage::ServerError { code, .. })) => {
                    log::debug!("pairing client: server-info query refused (0x{code:04x})");
                    None
                }
                Ok(Ok(other)) => {
                    log::debug!(
                        "pairing client: unexpected server-info answer (type 0x{:02x})",
                        other.type_byte()
                    );
                    None
                }
                Ok(Err(_dropped)) => None, // disconnected while waiting
                Err(_elapsed) => {
                    log::debug!("pairing client: server-info query timed out");
                    None
                }
            },
        };
        // The waiter slot is single-use; clear a timed-out one so a later
        // call can query again (an answered one is already taken).
        {
            let mut dispatch = self.inner.dispatch.lock().expect("client dispatch lock");
            dispatch.server_info_waiter = None;
        }
        if let Some(revision) = &reply {
            if let Ok(mut cache) = self.inner.server_info_cache.lock() {
                *cache = Some(revision.clone());
            }
        }
        reply
    }

    /// Ask the registry to open a coordination session toward a paired
    /// peer (keyed by the verified `(pairing_id, code_id, connection_id)`
    /// triple); the waiter matches on `connection_id`.
    pub async fn connect_peer(
        &self,
        target: DeviceId,
        pairing_id: PairingId,
        code_id: CodeId,
        connection_id: [u8; 16],
    ) -> Result<ak_signal::client_v2::OpenedSession, ClientError> {
        let request_id = random_id16();
        let response = self
            .roundtrip(
                request_id,
                CtrlMessage::ConnectRequest {
                    request_id,
                    target,
                    pairing_id,
                    code_id,
                    connection_id,
                },
            )
            .await?;
        match response {
            CtrlMessage::SessionOpened {
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            } => Ok(ak_signal::client_v2::OpenedSession {
                request_id,
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            }),
            CtrlMessage::PairError {
                scope,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, code, retry_after_ms)),
            other => Err(ClientError::Protocol(format!(
                "unexpected answer to ConnectRequest (type 0x{:02x})",
                other.type_byte()
            ))),
        }
    }

    // ---- lifecycle ---------------------------------------------------------

    /// Tear down and reconnect with the frozen backoff (1 s / 2 s / 4 s …
    /// cap 30 s between the attempts of ONE batch). Subscriptions survive;
    /// one-shot waiters were failed. On reconnect the registry re-pushes
    /// pending offers / challenges / proofs (Hello catch-up), which the
    /// plane stream delivers.
    ///
    /// A batch in which every attempt fails ends in the RETRYABLE terminal
    /// state [`ClientState::Disconnected`] — never [`ClientState::
    /// Connecting`] (R36-P1-02): a keeper that only reacts to
    /// `Disconnected` would otherwise wedge forever after one exhausted
    /// batch, because nothing ever publishes a fresh transition. Callers
    /// reschedule the next batch with their own capped backoff (see
    /// [`RegistryClient::spawn_connection_keeper`] /
    /// [`keeper_retry_pause`]).
    pub async fn reconnect(&self) -> Result<(), ClientError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientError::Closed);
        }
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.force_disconnect();
        self.inner.state_tx.send_replace(ClientState::Connecting);
        let attempts = self.inner.config.reconnect_attempts.max(1);
        let mut backoff = Duration::from_secs(1);
        let mut last = None;
        for attempt in 1..=attempts {
            match open_connection(&self.inner).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < attempts {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                    last = Some(e);
                }
            }
        }
        // The lifecycle lock is held, so `close()` cannot have interleaved:
        // publishing the retryable terminal state here is race-free.
        self.inner.state_tx.send_replace(ClientState::Disconnected);
        Err(last.unwrap_or(ClientError::Offline))
    }

    /// Actively drop the current connection and publish
    /// [`ClientState::Disconnected`]: the black-hole recovery primitive a
    /// connection keeper uses after repeated ping timeouts — a TCP
    /// connection whose far end silently vanished may never surface an
    /// I/O error, so the keeper tears it down itself and lets
    /// [`RegistryClient::reconnect`] rebuild it. Subscriptions survive;
    /// one-shot waiters fail; the closed flag stays unset.
    pub async fn drop_connection(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.force_disconnect();
        // Dropping the live outbox sender makes the writer exit and close
        // the TLS stream; the reader then observes EOF.
        *self.inner.outbox.lock().expect("client outbox lock") = mpsc::channel(1).0;
    }

    /// Simulate a replacement kick (tests and fault injection): deliver
    /// the registry's `ERR_DEVICE_REPLACED` ServerError through the real
    /// routing path (sets the sticky flag + the keeper's pacing signal,
    /// fails one-shot waiters), then drop the connection exactly as the
    /// server would after sending the kick.
    #[cfg(test)]
    pub(crate) async fn simulate_replacement_kick(&self) {
        let frame = ak_signal::protocol_v2::CtrlMessage::ServerError {
            code: ak_signal::client_v2::ERR_DEVICE_REPLACED,
            message: "replaced (simulated)".to_owned(),
        };
        self.inner.route(frame.into());
        self.simulate_connection_loss().await;
    }

    /// Simulate an abrupt connection loss (tests and fault injection):
    /// [`RegistryClient::drop_connection`] — a subsequent
    /// [`RegistryClient::reconnect`] can bring the client back. The
    /// registry observes EOF and marks this device offline (its pending
    /// pushes move to the Hello catch-up queue).
    pub async fn simulate_connection_loss(&self) {
        self.drop_connection().await;
    }

    /// Close the connection and stop dispatching. Idempotent.
    pub async fn close(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.force_disconnect();
        {
            let mut dispatch = self.inner.dispatch.lock().expect("client dispatch lock");
            dispatch.waiters.clear();
            dispatch.pairing.clear();
            dispatch.plane.clear();
        }
        // Dropping the live outbox sender makes the writer exit and close
        // the TLS stream; the reader observes EOF next.
        *self.inner.outbox.lock().expect("client outbox lock") = mpsc::channel(1).0;
        self.inner.state_tx.send_replace(ClientState::Closed);
    }

    fn force_disconnect(&self) {
        if self.inner.connected.swap(false, Ordering::AcqRel) {
            self.inner.clear_waiters();
            if !self.inner.closed.load(Ordering::Acquire) {
                self.inner.state_tx.send_replace(ClientState::Disconnected);
            }
        }
    }

    // ---- internals ---------------------------------------------------------

    /// Idempotent request round-trip: bounded waiter, send, await; on
    /// timeout re-send the same frame (same request id).
    async fn roundtrip(
        &self,
        key: [u8; 16],
        message: CtrlMessage,
    ) -> Result<CtrlMessage, ClientError> {
        self.roundtrip_frame(key, CtrlFrame::new(message)).await
    }

    /// [`RegistryClient::roundtrip`] at frame level — the re-send carries
    /// the identical bytes, wave-28 extension section included.
    async fn roundtrip_frame(
        &self,
        key: [u8; 16],
        frame: CtrlFrame,
    ) -> Result<CtrlMessage, ClientError> {
        let attempts = self.inner.config.request_retries.saturating_add(1);
        let timeout = self.inner.config.response_timeout;
        for _attempt in 1..=attempts {
            let (tx, rx) = oneshot::channel();
            {
                let mut dispatch = self.inner.dispatch.lock().expect("client dispatch lock");
                if dispatch.waiters.contains_key(&key) {
                    return Err(ClientError::RequestInFlight);
                }
                dispatch.waiters.insert(key, tx);
            }
            let _guard = WaiterGuard {
                inner: &self.inner,
                key,
            };
            self.inner.send_frame_with(frame.clone())?;
            match tokio::time::timeout(timeout, rx).await {
                Err(_elapsed) => continue, // same id, same bytes, again
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(_dropped)) => {
                    return Err(if self.is_replaced() {
                        ClientError::Replaced
                    } else {
                        ClientError::ConnectionLost
                    });
                }
            }
        }
        Err(ClientError::Timeout {
            attempts: attempts.max(1),
        })
    }

    // ---- connection keeper --------------------------------------------------

    /// Spawn the connection keeper (wave 35, common component) with the
    /// production schedule: a keepalive [`RegistryClient::ping`] every
    /// [`KEEPER_PING_INTERVAL`] (well inside the registry v2 server's 30 s
    /// per-frame `FRAME_IO_DEADLINE`, so a waiting code owner never idles
    /// out), a reconnect whenever the state watcher reports
    /// [`ClientState::Disconnected`], a capped backoff between failed
    /// reconnect batches ([`keeper_retry_pause`]) and black-hole recovery
    /// after [`KEEPER_PING_TIMEOUT_LIMIT`] consecutive ping timeouts
    /// ([`RegistryClient::drop_connection`] + reconnect).
    ///
    /// The keeper ends when `shutdown` flips to `true`, when its sender is
    /// dropped, or when the client reaches [`ClientState::Closed`]; the
    /// returned handle is detached (abort it to converge a teardown
    /// faster).
    pub fn spawn_connection_keeper(
        &self,
        shutdown: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        self.spawn_connection_keeper_with(ConnectionKeeperConfig::default(), shutdown)
    }

    /// [`RegistryClient::spawn_connection_keeper`] with an explicit
    /// schedule (tests inject a fast ping cadence; production uses the
    /// [`ConnectionKeeperConfig::default`]).
    pub fn spawn_connection_keeper_with(
        &self,
        config: ConnectionKeeperConfig,
        shutdown: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let client = self.clone();
        tokio::spawn(async move {
            connection_keeper_loop(client, config, shutdown).await;
        })
    }
}

// ---------------------------------------------------------------------------
// Connection keeper (wave 35 common component, R36-P1-01/R36-P1-02)
// ---------------------------------------------------------------------------

/// Keepalive ping interval of [`RegistryClient::spawn_connection_keeper`]:
/// 20 s, well inside the registry v2 server's 30 s per-frame
/// `FRAME_IO_DEADLINE` — an owner waiting on a task code stays online.
pub const KEEPER_PING_INTERVAL: Duration = Duration::from_secs(20);

/// Consecutive ping timeouts after which the keeper actively drops the
/// connection and reconnects (black-hole recovery): a TCP connection whose
/// far end silently vanished may never surface an I/O error, so the ping
/// budget is the only detector.
pub const KEEPER_PING_TIMEOUT_LIMIT: u32 = 3;

/// Capped backoff between reconnect BATCHES (the pause before batch *n+1*
/// where *n* batches failed in a row — each batch already backs off
/// 1/2/4…30 s internally between its own attempts). The first retry is
/// immediate (a plain blip recovers with no added delay); a long outage
/// settles at the 30 s cap so the keeper can never spin.
pub const KEEPER_RETRY_BACKOFF_STEPS: [Duration; 6] = [
    Duration::ZERO,
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(30),
];

/// Pause before the keeper RECLAIMS a replacement-kicked connection
/// (CR fix, 2026-11-12): a same-identity one-shot (e.g. the legacy
/// `ak-cli invite revoke`, which predates the connection lock) holds the
/// registry's one-connection-per-device slot for its brief operation;
/// the keeper's usual first-retry-is-immediate rule regrabbed the slot
/// mid-operation and kicked it — on slow CI, `invite revoke` died with
/// "connection lost while waiting for the reply" (runs 43029286253 and
/// 43159698726). A replacement kick is not a network blip: wait out the
/// full cap so any short borrower finishes, then reclaim (a well-behaved
/// lock participant never collides at all — it refuses loudly).
///
/// Batch G6 re-evaluation (2026-11-13) — KEPT at 30 s after the reason
/// frame landed (9029c08). The "on-demand" goal is already achieved by
/// the reason frame itself: the pause now triggers ONLY when the server
/// explicitly said "replaced" (0x1001); every ordinary failure (network
/// blip, server restart, EOF) reconnects at backoff step 0 as before —
/// before 9029c08 the pause in fact NEVER fired in production (bare
/// EOF). Shortening the FIXED value is the wrong trade:
/// 1. the one-shots this window protects do not participate in the local
///    lock BY PRODUCT SEMANTICS (revoke while a daemon lives is a
///    supported path), so pacing is their only protection — and
///    registry-class operations have observed long tails under load
///    (redeem transactions legitimately run ~90–105 s), so a shorter
///    window re-opens the exact connection war the pause ended;
/// 2. a copied identity on ANOTHER machine is outside every local lock
///    (see `pairing::connection_lock`); against a second resident keeper
///    the 30 s floor is also the oscillation damper — shortening speeds
///    that war up, not down;
/// 3. 30 s equals the reconnect backoff cap, so a replacement is costed
///    like a full outage batch — one coherent rule, no extra state;
/// 4. no field data argues for less (C4 real-device testing is still
///    pending). Revisit only with measured one-shot durations from C4.
pub const KEEPER_REPLACEMENT_PAUSE: Duration = Duration::from_secs(30);

/// The pause before reconnect batch `failed_batches + 1`
/// ([`KEEPER_RETRY_BACKOFF_STEPS`], saturating at the cap).
pub fn keeper_retry_pause(failed_batches: usize) -> Duration {
    KEEPER_RETRY_BACKOFF_STEPS[failed_batches.min(KEEPER_RETRY_BACKOFF_STEPS.len() - 1)]
}

/// Schedule of [`RegistryClient::spawn_connection_keeper_with`].
#[derive(Debug, Clone)]
pub struct ConnectionKeeperConfig {
    /// Keepalive ping cadence ([`KEEPER_PING_INTERVAL`] by default).
    pub ping_interval: Duration,
    /// Reclaim pause after a replacement kick
    /// ([`KEEPER_REPLACEMENT_PAUSE`] by default; tests inject a short one).
    pub replacement_pause: Duration,
}

impl Default for ConnectionKeeperConfig {
    fn default() -> Self {
        ConnectionKeeperConfig {
            ping_interval: KEEPER_PING_INTERVAL,
            replacement_pause: KEEPER_REPLACEMENT_PAUSE,
        }
    }
}

/// The keeper body (see [`RegistryClient::spawn_connection_keeper`]).
///
/// Each iteration first reacts to the CURRENT watcher value — not only to
/// transitions: a drop that landed before this loop's first poll (or
/// while it was parked in a ping) publishes no further transition, so a
/// purely `changed()`-driven keeper would sit on a dead `Disconnected`
/// client forever. `Disconnected` reconnects after [`keeper_retry_pause`],
/// `Connected` resets the counters, `Closed` ends the loop; the select
/// below pings on cadence (timeouts count toward the black-hole limit,
/// connection-level errors are left to the reader's own `Disconnected`).
/// Because [`RegistryClient::reconnect`] publishes `Disconnected` again
/// when a whole batch fails (R36-P1-02), the loop reschedules itself
/// through this same top-of-loop check instead of wedging on
/// `Connecting`.
async fn connection_keeper_loop(
    client: RegistryClient,
    config: ConnectionKeeperConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut state = client.state_watcher();
    let mut ping_timeouts: u32 = 0;
    let mut failed_batches: usize = 0;
    loop {
        let current = *state.borrow_and_update();
        match current {
            ClientState::Closed => break,
            ClientState::Connected(_) => {
                failed_batches = 0;
                ping_timeouts = 0;
            }
            ClientState::Connecting => {}
            ClientState::Disconnected => {
                let pause = if client.last_close_was_replacement() {
                    config.replacement_pause
                } else {
                    keeper_retry_pause(failed_batches)
                };
                if !pause.is_zero() {
                    tokio::select! {
                        _ = tokio::time::sleep(pause) => {}
                        _ = shutdown.changed() => break,
                    }
                    // Someone else may have reconnected during the pause;
                    // tearing their connection down would be pure churn.
                    if matches!(*state.borrow_and_update(), ClientState::Connected(_)) {
                        failed_batches = 0;
                        continue;
                    }
                }
                match client.reconnect().await {
                    Ok(()) => failed_batches = 0,
                    Err(ClientError::Closed) => break,
                    Err(_) => failed_batches += 1,
                }
                continue; // re-evaluate: Connected resets, still-Disconnected retries
            }
        }
        let next_ping = tokio::time::sleep(config.ping_interval);
        tokio::pin!(next_ping);
        tokio::select! {
            _ = &mut next_ping => {
                match client.ping().await {
                    Ok(()) => ping_timeouts = 0,
                    Err(ClientError::Closed) => break,
                    Err(ClientError::Timeout { .. }) => {
                        ping_timeouts += 1;
                        if ping_timeouts >= KEEPER_PING_TIMEOUT_LIMIT {
                            ping_timeouts = 0;
                            log::warn!(
                                "pairing client: {KEEPER_PING_TIMEOUT_LIMIT} consecutive ping \
                                 timeouts, dropping the control connection (black-hole recovery)"
                            );
                            client.drop_connection().await; // -> Disconnected; the top reconnects
                        }
                    }
                    Err(_) => {
                        // Connection-level failure (Offline/ConnectionLost/…):
                        // the reader surfaces it as Disconnected on its own;
                        // never count it toward the black-hole budget.
                        ping_timeouts = 0;
                    }
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    break; // the client itself is gone
                }
                // The loop top re-evaluates the new value.
            }
            _ = shutdown.changed() => break,
        }
    }
}

fn pair_error(scope: ErrorScope, code: u16, retry_after_ms: u32) -> ClientError {
    ClientError::Pair {
        scope,
        code,
        retry_after_ms,
    }
}

/// Validate the Hello device name against the wire rules (≤64 bytes, no
/// C0/DEL/C1 controls) before anything is signed or sent.
fn check_display_name(name: &str) -> Result<(), ClientError> {
    let bytes = name.as_bytes();
    let max = ak_signal::protocol_v2::MAX_NAME_BYTES;
    if bytes.len() > max
        || bytes
            .iter()
            .any(|&b| b < 0x20 || b == 0x7F || (0x80..=0x9F).contains(&b))
    {
        return Err(ClientError::Protocol(
            "device name is not wire-legal".into(),
        ));
    }
    Ok(())
}

/// Open one pinned connection and drive the frozen opening exchange, then
/// hand over to the reader/writer tasks (mirrors the server's expectations
/// exactly — see the module docs).
async fn open_connection(inner: &Arc<ClientInner>) -> Result<(), ClientError> {
    let connect_timeout = inner.config.response_timeout.mul_f32(2.0);
    let mut stream = tokio::time::timeout(
        connect_timeout,
        registry_tls::connect_pinned(inner.addr, &inner.registry_id),
    )
    .await
    .map_err(|_| ClientError::Timeout { attempts: 1 })?
    .map_err(|e| ClientError::Protocol(format!("pinned TLS dial failed: {e}")))?;

    // ---- AuthChallenge: registry id must equal the pin, not expired ----
    let challenge_nonce;
    {
        let bytes = read_with_timeout(&mut stream, connect_timeout).await?;
        let message = decode_server(&bytes)?;
        match message {
            CtrlMessage::AuthChallenge {
                registry_id,
                nonce,
                expires_ms,
            } => {
                if registry_id != inner.registry_id {
                    return Err(ClientError::Protocol(
                        "registry id does not match the pinned key".into(),
                    ));
                }
                if expires_ms <= now_unix_ms() {
                    return Err(ClientError::Protocol("auth challenge expired".into()));
                }
                challenge_nonce = nonce;
            }
            CtrlMessage::ServerError { code, .. } => {
                return Err(ClientError::Server { code });
            }
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected AuthChallenge, got type 0x{:02x}",
                    other.type_byte()
                )));
            }
        }
    }

    // ---- Hello, signed over the frozen exporter-bound domain ----
    let exporter = {
        let (_io, conn) = stream.get_ref();
        hello2::exporter_secret_client(conn)
            .map_err(|e| ClientError::Protocol(format!("tls exporter unavailable: {e}")))?
    };
    let version = u16::from(ak_signal::protocol_v2::VERSION);
    let device_id = inner.key.public_key();
    let timestamp_ms = now_unix_ms();
    let signing = hello2::hello2_message(
        &inner.registry_id,
        &exporter,
        version,
        &device_id,
        &inner.name,
        timestamp_ms,
        &challenge_nonce,
    );
    let hello = CtrlMessage::Hello {
        version,
        device_id,
        name: inner.name.clone(),
        timestamp_ms,
        challenge: challenge_nonce,
        signature: inner.key.sign(&signing),
    };
    write_message(&mut stream, &hello, Role::Client)
        .await
        .map_err(|e| ClientError::Protocol(format!("hello write failed: {e}")))?;

    // ---- Welcome ----
    {
        let bytes = read_with_timeout(&mut stream, connect_timeout).await?;
        match decode_server(&bytes)? {
            CtrlMessage::Welcome {
                session_ttl_secs,
                capabilities,
            } if session_ttl_secs > 0 => {
                // G6: remember the CURRENT connection epoch's capability
                // bits — `server_info()` asks only when the server
                // advertised the revision-query bit, so an old server is
                // never asked (pure-addition interop).
                inner.welcome_caps.store(capabilities, Ordering::Release);
                if let Ok(mut cache) = inner.server_info_cache.lock() {
                    *cache = None;
                }
            }
            CtrlMessage::ServerError { code, .. } => return Err(ClientError::Server { code }),
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected Welcome, got type 0x{:02x}",
                    other.type_byte()
                )));
            }
        }
    }

    // ---- Hand over to the long-lived tasks ----
    let epoch = inner.epoch.fetch_add(1, Ordering::AcqRel) + 1;
    let (read_half, write_half) = tokio::io::split(stream);
    let (tx, rx) = mpsc::channel(inner.config.outbox_capacity);
    *inner.outbox.lock().expect("client outbox lock") = tx;
    inner.connected.store(true, Ordering::Release);
    // A fresh connection is by definition not replacement-kicked; the
    // keeper's replacement pacing must not outlive the outage that
    // caused it (the sticky `replaced` flag above is the UI-facing one).
    inner.replace_closed.store(false, Ordering::Release);
    inner.state_tx.send_replace(ClientState::Connected(epoch));
    let reader_inner = Arc::clone(inner);
    let reader_epoch = epoch;
    tokio::spawn(async move {
        reader_loop(reader_inner, read_half, reader_epoch).await;
    });
    let writer_inner = Arc::clone(inner);
    let writer_epoch = epoch;
    tokio::spawn(async move {
        writer_loop(writer_inner, write_half, rx, writer_epoch).await;
    });
    Ok(())
}

async fn read_with_timeout<R: AsyncRead + Unpin>(
    stream: &mut R,
    timeout: Duration,
) -> Result<Vec<u8>, ClientError> {
    tokio::time::timeout(timeout, read_frame(stream))
        .await
        .map_err(|_| ClientError::Timeout { attempts: 1 })?
        .map_err(|e| ClientError::Protocol(format!("frame read failed: {e}")))?
        .ok_or(ClientError::ConnectionLost)
}

fn decode_server_frame(bytes: &[u8]) -> Result<CtrlFrame, ClientError> {
    CtrlFrame::decode_as(bytes, Role::Client)
        .map_err(|e| ClientError::Protocol(format!("undecodable server frame: {e}")))
}

/// The plain message of one server frame (extension discarded) — the
/// opening-exchange reader, which never sees extended frames.
fn decode_server(bytes: &[u8]) -> Result<CtrlMessage, ClientError> {
    decode_server_frame(bytes).map(|frame| frame.message)
}

/// The single reader: consumes every frame and routes it to the bounded
/// inboxes; exits (and fails waiters) on EOF/decode error.
async fn reader_loop<R: AsyncRead + Unpin>(inner: Arc<ClientInner>, mut read: R, epoch: u64) {
    loop {
        match read_frame(&mut read).await {
            Ok(Some(bytes)) => match decode_server_frame(&bytes) {
                Ok(frame) => inner.route(frame),
                Err(e) => {
                    log::debug!("pairing client: decode error, dropping connection: {e}");
                    inner.on_disconnect(epoch);
                    return;
                }
            },
            Ok(None) => {
                log::debug!("pairing client: registry closed the connection");
                inner.on_disconnect(epoch);
                return;
            }
            Err(e) => {
                log::debug!("pairing client: read error, dropping connection: {e}");
                inner.on_disconnect(epoch);
                return;
            }
        }
    }
}

/// The single writer: serializes outbound frames; exits when all senders
/// are gone (close) or the stream breaks.
async fn writer_loop<W: AsyncWrite + Unpin>(
    inner: Arc<ClientInner>,
    mut write: W,
    mut rx: mpsc::Receiver<CtrlFrame>,
    epoch: u64,
) {
    while let Some(frame) = rx.recv().await {
        if let Err(e) = write_frame(&mut write, &frame, Role::Client).await {
            log::debug!("pairing client: write failed, dropping connection: {e}");
            inner.on_disconnect(epoch);
            return;
        }
    }
    let _ = write.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_pairing_offset_matches_the_frozen_layout() {
        let mut transcript = [0u8; 256];
        transcript[TRANSCRIPT_PAIRING_OFFSET..TRANSCRIPT_PAIRING_OFFSET + 16]
            .copy_from_slice(&[0x5A; 16]);
        let typed = ak_identity::pairing_txn::PairTranscript::from_bytes(&transcript);
        assert_eq!(typed.pairing_id, [0x5A; 16]);
        assert_eq!(transcript_pairing_id(&transcript), [0x5A; 16]);
    }

    #[test]
    fn display_name_rules_mirror_the_wire_limits() {
        assert!(check_display_name("laptop-a").is_ok());
        assert!(check_display_name(&"x".repeat(64)).is_ok());
        assert!(check_display_name(&"x".repeat(65)).is_err());
        assert!(check_display_name("a\u{7}b").is_err());
        assert!(check_display_name("a\u{9b}b").is_err());
    }

    // ---- R36-P1-01 / R36-P1-02: keeper constants, state machine, recovery --

    #[test]
    fn keeper_constants_pin_the_registry_silence_budget() {
        // The registry v2 server cuts a connection after 30 s of inbound
        // silence (FRAME_IO_DEADLINE); the keepalive must sit well inside
        // that budget, and the black-hole detector trips on 3 timeouts.
        assert_eq!(KEEPER_PING_INTERVAL, Duration::from_secs(20));
        assert_eq!(
            ConnectionKeeperConfig::default().ping_interval,
            KEEPER_PING_INTERVAL
        );
        assert_eq!(KEEPER_PING_TIMEOUT_LIMIT, 3);
    }

    #[test]
    fn keeper_backoff_is_immediate_then_capped() {
        assert_eq!(keeper_retry_pause(0), Duration::ZERO);
        assert_eq!(keeper_retry_pause(1), Duration::from_secs(1));
        assert_eq!(keeper_retry_pause(2), Duration::from_secs(2));
        assert_eq!(keeper_retry_pause(3), Duration::from_secs(4));
        assert_eq!(keeper_retry_pause(4), Duration::from_secs(8));
        assert_eq!(keeper_retry_pause(5), Duration::from_secs(30));
        assert_eq!(keeper_retry_pause(1000), Duration::from_secs(30));
    }

    mod registry {
        use super::super::*;
        use ak_signal::server_v2::store::RegistryStore;
        use ak_signal::server_v2::Limits;
        use tokio::net::TcpListener;

        pub(super) struct TestRegistry {
            pub addr: std::net::SocketAddr,
            pub registry_id: [u8; 32],
            shutdown: watch::Sender<bool>,
        }

        impl Drop for TestRegistry {
            fn drop(&mut self) {
                let _ = self.shutdown.send(true);
            }
        }

        pub(super) async fn spawn() -> TestRegistry {
            spawn_with(None).await
        }

        /// [`registry::spawn`] with an optional build revision for the G6
        /// `ServerInfo` answer (`None` = the engine's `unknown` default).
        pub(super) async fn spawn_with(revision: Option<&str>) -> TestRegistry {
            use ak_signal::server_v2::engine::serve_v2_with_revision;
            let registry_key = DeviceKeyPair::generate();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind v2 port");
            let addr = listener.local_addr().expect("local addr");
            let store = RegistryStore::open_in_memory().expect("open registry store");
            let (shutdown, shutdown_rx) = watch::channel(false);
            let key = registry_key.clone();
            let revision = revision.map(str::to_string);
            tokio::spawn(async move {
                let _ = serve_v2_with_revision(
                    listener,
                    &key,
                    store,
                    Limits::default(),
                    revision.as_deref(),
                    shutdown_rx,
                )
                .await;
            });
            TestRegistry {
                addr,
                registry_id: registry_key.public_key(),
                shutdown,
            }
        }
    }

    /// R36-P1-02 core: a reconnect batch that fails ENTIRELY must end in
    /// `Disconnected` (the retryable terminal state), never leave the
    /// client parked in `Connecting` — so a watcher-driven keeper can keep
    /// rescheduling batches.
    #[tokio::test]
    async fn reconnect_batch_failure_ends_disconnected_not_connecting() {
        let registry = registry::spawn().await;
        let key = DeviceKeyPair::generate();
        let config = ClientConfig {
            reconnect_attempts: 1, // one fast failing attempt
            ..ClientConfig::default()
        };
        let client = RegistryClient::connect_with_config(
            registry.addr,
            registry.registry_id,
            &key,
            "keeper-probe",
            config,
        )
        .await
        .expect("connect to the live registry");
        client.ping().await.expect("live ping before the outage");
        drop(registry); // server gone: every dial now fails fast

        for batch in 1..=2 {
            let error = client.reconnect().await.expect_err("batch must fail");
            assert!(
                !matches!(error, ClientError::Closed),
                "the failure is the dead server, not a closed client"
            );
            // A FRESH subscriber (the shape every keeper has) must see the
            // retryable terminal state — batch {batch} included.
            let watcher = client.state_watcher();
            assert!(
                matches!(*watcher.borrow(), ClientState::Disconnected),
                "failed batch {batch} must end Disconnected, not Connecting"
            );
            drop(watcher);
        }
        client.close().await;
    }

    /// R36-P1-01: after an abrupt connection loss the keeper re-establishes
    /// the connection on its own (a fast injected ping cadence keeps the
    /// test quick) and the client is pingable again.
    #[tokio::test]
    async fn keeper_recovers_a_dropped_connection() {
        let registry = registry::spawn().await;
        let key = DeviceKeyPair::generate();
        let client =
            RegistryClient::connect(registry.addr, registry.registry_id, &key, "keeper-recovery")
                .await
                .expect("connect to the live registry");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let keeper = client.spawn_connection_keeper_with(
            ConnectionKeeperConfig {
                ping_interval: Duration::from_millis(100),
                replacement_pause: Duration::from_millis(100),
            },
            shutdown_rx,
        );
        client.simulate_connection_loss().await;
        let mut state = client.state_watcher();
        let recovered = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(*state.borrow_and_update(), ClientState::Connected(_)) {
                    return;
                }
                state.changed().await.expect("watcher alive");
            }
        })
        .await;
        assert!(recovered.is_ok(), "keeper must reconnect after the loss");
        client.ping().await.expect("pingable after recovery");
        shutdown_tx.send(true).expect("stop the keeper");
        let _ = tokio::time::timeout(Duration::from_secs(5), keeper).await;
        client.close().await;
        drop(registry);
    }

    /// Replacement pacing (2026-11-12 fix): when a same-device-id second
    /// connection takes over the registry slot, the keeper must NOT
    /// regrab it immediately (the immediate first retry kicked the
    /// short-lived borrower mid-operation — the `invite revoke` CI
    /// failure). It waits out `replacement_pause`, then reclaims.
    #[tokio::test]
    async fn keeper_paces_a_replacement_kick_before_reclaiming() {
        let registry = registry::spawn().await;
        let key = DeviceKeyPair::generate(); // one device id, two connections
        let a = RegistryClient::connect(registry.addr, registry.registry_id, &key, "pace-a")
            .await
            .expect("connect A");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let keeper = a.spawn_connection_keeper_with(
            ConnectionKeeperConfig {
                ping_interval: Duration::from_secs(3600), // pings off: only the kick matters
                replacement_pause: Duration::from_millis(700),
            },
            shutdown_rx,
        );
        assert!(!a.last_close_was_replacement());

        // The registry (the test fake does not implement same-id kicks)
        // would deliver ERR_DEVICE_REPLACED and close; simulate exactly
        // that through the real routing + teardown paths.
        a.simulate_replacement_kick().await;
        let mut state = a.state_watcher();
        let kicked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(*state.borrow_and_update(), ClientState::Disconnected) {
                    return;
                }
                state.changed().await.expect("watcher alive");
            }
        })
        .await;
        assert!(kicked.is_ok(), "B's connect must kick A");
        assert!(a.is_replaced(), "the sticky event flag is set");
        assert!(a.last_close_was_replacement());

        // Pacing: A must still be Disconnected well past a normal
        // first-retry-is-immediate reclaim (which would have reconnected
        // within ~a batch, i.e. milliseconds here).
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            matches!(*state.borrow_and_update(), ClientState::Disconnected),
            "the keeper must honor the replacement pause, not regrab instantly"
        );

        // ... and after the pause it does reclaim (B gets kicked in turn).
        let reclaimed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(*state.borrow_and_update(), ClientState::Connected(_)) {
                    return;
                }
                state.changed().await.expect("watcher alive");
            }
        })
        .await;
        assert!(reclaimed.is_ok(), "the keeper reclaims after the pause");
        assert!(
            !a.last_close_was_replacement(),
            "a fresh connection clears the pacing signal (sticky is_replaced stays)"
        );
        assert!(a.is_replaced(), "the sticky UI flag survives reconnects");

        shutdown_tx.send(true).expect("stop the keeper");
        let _ = tokio::time::timeout(Duration::from_secs(5), keeper).await;
        a.close().await;
        drop(registry);
    }

    /// G6 §6a end to end against the REAL `serve_v2_with_revision`: the
    /// client saves the current epoch's Welcome capabilities, asks only
    /// because the server advertised the bit, and gets the configured
    /// revision back. (New client × new server.)
    #[tokio::test]
    async fn server_info_roundtrips_against_the_real_registry() {
        let registry = registry::spawn_with(Some("0.1.0+abc12345")).await;
        let key = DeviceKeyPair::generate();
        let client =
            RegistryClient::connect(registry.addr, registry.registry_id, &key, "info-asker")
                .await
                .expect("connect to the live registry");
        assert!(
            client.welcome_capabilities() & ak_signal::server_v2::CAP_SERVER_INFO != 0,
            "the new server advertises the revision-query bit"
        );
        assert_eq!(
            client.server_info().await,
            Some("0.1.0+abc12345".to_string()),
            "the query returns the configured build revision"
        );
        client.close().await;
        drop(registry);
    }

    /// G6 interop gate (old server × new client): a server whose Welcome
    /// does NOT carry the capability bit is never asked —
    /// [`RegistryClient::server_info`] returns `None` without sending a
    /// frame (an old server's decoder would treat 0x1F as an unknown tag
    /// and drop the connection). The old-server Welcome is simulated by
    /// clearing the saved epoch capabilities of a live client; the real
    /// pre-G6 behavior (bits absent) is the same input to the decision.
    #[tokio::test]
    async fn server_info_is_not_asked_without_the_capability_bit() {
        let registry = registry::spawn().await;
        let key = DeviceKeyPair::generate();
        let client =
            RegistryClient::connect(registry.addr, registry.registry_id, &key, "old-server")
                .await
                .expect("connect to the live registry");
        // Simulate the old server's Welcome (no 0x0008 bit).
        client
            .inner
            .welcome_caps
            .store(0x0000_0007, Ordering::Release);
        assert_eq!(
            client.server_info().await,
            None,
            "no bit → no query: the method must not send ServerInfoRequest"
        );
        // The connection is untouched by the refused query.
        client
            .ping()
            .await
            .expect("the control connection still works");
        client.close().await;
        drop(registry);
    }

    /// G6 degradation: the query must never fault the caller — offline
    /// (the send fails), the answer degrades to `None` and the client
    /// keeps working otherwise.
    #[tokio::test]
    async fn server_info_degrades_to_none_when_disconnected() {
        let registry = registry::spawn_with(Some("0.1.0+abc12345")).await;
        let key = DeviceKeyPair::generate();
        let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, "degrade")
            .await
            .expect("connect to the live registry");
        assert_eq!(
            client.server_info().await,
            Some("0.1.0+abc12345".to_string())
        );
        // Drop the connection: a later query degrades instead of erroring.
        client.simulate_connection_loss().await;
        assert_eq!(
            client.server_info().await,
            None,
            "a failed query degrades to None, never an error"
        );
        client.close().await;
        drop(registry);
    }

    /// Batch G6 / 9029c08: the REAL replacement path end to end — a
    /// second same-device connection makes the live server deliver the
    /// ERR_DEVICE_REPLACED reason frame to the first connection, and the
    /// keeper paces its reclaim (waits out `replacement_pause`) instead
    /// of regrabbing the slot at backoff step 0. This is the production
    /// producer the old pacing test had to inject (`simulate_replacement_
    /// kick`); it pins that the reason frame really arms the pacing.
    #[tokio::test]
    async fn real_replacement_kick_paces_before_reclaiming() {
        let registry = registry::spawn().await;
        let key = DeviceKeyPair::generate(); // one device id, two connections
        let a = RegistryClient::connect(registry.addr, registry.registry_id, &key, "real-a")
            .await
            .expect("connect A");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let keeper = a.spawn_connection_keeper_with(
            ConnectionKeeperConfig {
                ping_interval: Duration::from_secs(3600), // pings off: only the kick matters
                replacement_pause: Duration::from_millis(700),
            },
            shutdown_rx,
        );
        assert!(!a.last_close_was_replacement());

        // The REAL kick: B connects with the same device identity.
        let b = RegistryClient::connect(registry.addr, registry.registry_id, &key, "real-b")
            .await
            .expect("connect B");
        let mut state = a.state_watcher();
        let kicked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(*state.borrow_and_update(), ClientState::Disconnected) {
                    return;
                }
                state.changed().await.expect("watcher alive");
            }
        })
        .await;
        assert!(kicked.is_ok(), "B's real Hello must kick A");
        // The reason frame (not the bare EOF) is what armed the pacing:
        // this is exactly what 9029c08 made the server send.
        assert!(a.is_replaced(), "the sticky event flag is set");
        assert!(
            a.last_close_was_replacement(),
            "the 0x1001 reason frame set the pacing signal"
        );
        drop(b);

        // Pacing: A stays Disconnected past the immediate first retry.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            matches!(*state.borrow_and_update(), ClientState::Disconnected),
            "the keeper honors the replacement pause after a real kick"
        );
        // ... and reclaims after the pause.
        let reclaimed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(*state.borrow_and_update(), ClientState::Connected(_)) {
                    return;
                }
                state.changed().await.expect("watcher alive");
            }
        })
        .await;
        assert!(reclaimed.is_ok(), "the keeper reclaims after the pause");

        shutdown_tx.send(true).expect("stop the keeper");
        let _ = tokio::time::timeout(Duration::from_secs(5), keeper).await;
        a.close().await;
        drop(registry);
    }
}
