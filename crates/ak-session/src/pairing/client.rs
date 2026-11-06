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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
                    .len() == blob.len()
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
                CtrlMessage::PairError { scope, object_id, .. }
                    if *scope == ErrorScope::Pairing =>
                {
                    Some(*object_id)
                }
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
                delivered |= Dispatch::fan_out(&dispatch.plane, &message);
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
    fn clear_waiters(&self) {
        let mut dispatch = self.dispatch.lock().expect("client dispatch lock");
        dispatch.waiters.clear();
        dispatch.endpoint_blobs.clear();
        dispatch.endpoint_blob_order.clear();
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
    /// cap 30 s). Subscriptions survive; one-shot waiters were failed.
    /// On reconnect the registry re-pushes pending offers / challenges /
    /// proofs (Hello catch-up), which the plane stream delivers.
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
        Err(last.unwrap_or(ClientError::Offline))
    }

    /// Simulate an abrupt connection loss (tests and fault injection):
    /// tears the TLS stream down and fails the waiters **without** setting
    /// the closed flag, so a subsequent
    /// [`RegistryClient::reconnect`] can bring the client back. The
    /// registry observes EOF and marks this device offline (its pending
    /// pushes move to the Hello catch-up queue).
    pub async fn simulate_connection_loss(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.force_disconnect();
        // Dropping the live outbox sender makes the writer exit and close
        // the TLS stream; the reader then observes EOF.
        *self.inner.outbox.lock().expect("client outbox lock") = mpsc::channel(1).0;
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
                    })
                }
            }
        }
        Err(ClientError::Timeout {
            attempts: attempts.max(1),
        })
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
                )))
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
                session_ttl_secs, ..
            } if session_ttl_secs > 0 => {}
            CtrlMessage::ServerError { code, .. } => return Err(ClientError::Server { code }),
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected Welcome, got type 0x{:02x}",
                    other.type_byte()
                )))
            }
        }
    }

    // ---- Hand over to the long-lived tasks ----
    let epoch = inner.epoch.fetch_add(1, Ordering::AcqRel) + 1;
    let (read_half, write_half) = tokio::io::split(stream);
    let (tx, rx) = mpsc::channel(inner.config.outbox_capacity);
    *inner.outbox.lock().expect("client outbox lock") = tx;
    inner.connected.store(true, Ordering::Release);
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
}
