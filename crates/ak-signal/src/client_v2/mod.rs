//! Control-protocol v2 client: pinned-TLS connection, single-reader
//! dispatch, idempotent request API.
//!
//! Contract: `docs/contracts/protocol-v2.md` (message set + directions) and
//! `docs/contracts/pairing-flow.md` (idempotent retries, bounded inboxes,
//! replacement semantics). This module is the client half of task A3; the
//! server half (A2) is developed in parallel and the real interop is
//! deferred to wave 21 — tests here run against a scripted fake registry
//! over real TLS.
//!
//! # Dispatch model (the v1 defect, fixed)
//!
//! Exactly **one reader task** consumes every inbound frame and routes it by
//! `request_id` / `pairing_id` / `connection_id` / `session` into the
//! matching **bounded** inbox (capacity 64 by default). There is no shared
//! unbounded inbox: a waiter that vanished, a full inbox, or an unsolicited
//! frame is counted in [`ClientV2::dropped_unmatched_frames`] and logged,
//! never allowed to wedge the reader or grow memory (pairing-flow §11).
//!
//! # Idempotency and cancellation
//!
//! Every request carries a fresh CSPRNG `request_id`. A response timeout
//! re-sends the **same frame with the same id** (default twice) so the
//! server's idempotency table resolves the attempt
//! (pairing-flow §5: "网络超时不直接生成新码；先以原 request_id 重试").
//! Dropping the request future cancels only the wait — the waiter is
//! removed from the dispatch registry and the connection stays up. After a
//! reconnect the one-shot waiters are failed with
//! [`ClientV2Error::ConnectionLost`] and the request-id space restarts:
//! ids are fresh CSPRNG draws per logical request, the pending map is
//! purged on disconnect, so a stale response can never be matched to a new
//! request (collision probability 2^-128 aside).
//!
//! # One instance per device (no self-eviction)
//!
//! The daemon and the CLI **must share one `ClientV2`** held by the B6
//! runtime, instead of each opening its own control connection: connecting
//! twice with the same `device_id` makes the server replace the older
//! connection (v1 semantics carried into v2 — the old stream gets
//! `ServerError { code: ERR_DEVICE_REPLACED }` and closes). The replaced
//! side can observe that via [`ClientV2::is_replaced`] /
//! [`ClientV2::replaced_watcher`]; the flag is sticky across reconnects so
//! an external duplicate process stays visible.
//!
//! # Reconnects
//!
//! [`ClientV2::reconnect`] retries the full pinned handshake with the
//! frozen backoff (1 s / 2 s / 4 s doubling, cap 30 s). Pairing and session
//! subscriptions survive a reconnect (committed transactions recover,
//! pairing-flow §6); one-shot waiters do not.
#![forbid(unsafe_code)]
// The frozen error type carries a whole CtrlMessage in `Unexpected` (same
// pattern as the v1 SignalError), which exceeds clippy's large-err
// threshold; boxing would change the API.
#![allow(clippy::result_large_err)]

mod config;
mod conn;
mod dispatch;
mod error;
mod tls;

pub use config::{Backoff, ClientV2Config, DEFAULT_INBOX_CAPACITY, DEFAULT_OUTBOX_CAPACITY};
pub use error::{ClientV2Error, PairErrorInfo, ERR_DEVICE_REPLACED};
pub use tls::{hello_v2_signing_bytes, CTRL_ALPN, HELLO_EXPORTER_LABEL, HELLO_EXPORTER_LEN};

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use ak_identity::{DeviceKeyPair, Fingerprint};
use dispatch::{DispatchKey, DispatchRegistry, RouteOutcome};
use rand::rngs::OsRng;
use rand::RngCore;
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex};

use crate::protocol_v2::CtrlMessage;
use crate::protocol_v2::{
    AbortReason, CodeId, ConnectionId, DeviceId, Digest32, ErrorScope, InviteSummary, PairProof,
    PairingId, PeerSignal, RequestId, SessionId, Signature64, StateFilter, Transcript256,
};

/// Connection state reported through [`ClientV2::state_watcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// A connect attempt is running.
    Connecting,
    /// Live connection (its generation / epoch).
    Connected(u64),
    /// The connection dropped; waiters were failed.
    Disconnected,
    /// [`ClientV2::close`] completed.
    Closed,
}

/// Successful `InviteClaim` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedInvite {
    pub request_id: RequestId,
    pub code_id: CodeId,
    pub created_ms: u64,
    /// 0 = long-lived.
    pub expires_ms: u64,
    pub revision: u64,
}

/// One `InviteList` page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitePage {
    pub request_id: RequestId,
    /// All-zero (`CURSOR_END`) = no further pages.
    pub next_cursor: CodeId,
    pub entries: Vec<InviteSummary>,
}

/// Successful `InviteRevoke` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedInvite {
    pub request_id: RequestId,
    pub code_id: CodeId,
    pub revision: u64,
    pub revoked_ms: u64,
}

/// `InviteRedeemPending`: the redeem attempt was accepted and is now driven
/// by the push flow on the pairing inbox (`subscribe_pairing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemAck {
    pub request_id: RequestId,
    pub pairing_id: PairingId,
    pub deadline_ms: u64,
}

/// `PairStatusReply` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairStatus {
    pub request_id: RequestId,
    pub pairing_id: PairingId,
    pub state: crate::protocol_v2::PairingState,
    pub commit_revision: u64,
    pub deadline_ms: u64,
}

/// `SessionOpened` payload for one `ConnectRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedSession {
    pub request_id: RequestId,
    pub session: SessionId,
    pub peer: DeviceId,
    pub pairing_id: PairingId,
    pub code_id: CodeId,
    pub connection_id: ConnectionId,
    pub expires_ms: u64,
}

/// Shared state behind the cheap-to-clone [`ClientV2`] handle.
pub(crate) struct Inner {
    pub(crate) server: SocketAddr,
    pub(crate) registry_fp: Fingerprint,
    pub(crate) key: DeviceKeyPair,
    pub(crate) name: String,
    pub(crate) config: ClientV2Config,

    /// Bounded waiters / subscriptions; see [`dispatch`].
    pub(crate) registry: StdMutex<DispatchRegistry>,
    /// Outbound queue of the live writer task.
    pub(crate) outbox: StdMutex<mpsc::Sender<conn::Outbound>>,
    /// Serializes reconnect/close against each other.
    pub(crate) lifecycle: AsyncMutex<()>,

    epoch: AtomicU64,
    connected: AtomicBool,
    closed: AtomicBool,
    replaced: AtomicBool,
    unmatched: AtomicU64,
    replaced_tx: watch::Sender<bool>,
    state_tx: watch::Sender<ConnState>,
}

impl Inner {
    fn new(
        server: SocketAddr,
        registry_fp: Fingerprint,
        key: DeviceKeyPair,
        name: String,
        config: ClientV2Config,
    ) -> Result<Arc<Self>, ClientV2Error> {
        tls::check_display_name(&name)?;
        let (replaced_tx, _) = watch::channel(false);
        let (state_tx, _) = watch::channel(ConnState::Connecting);
        Ok(Arc::new(Inner {
            server,
            registry_fp,
            key,
            name,
            config,
            registry: StdMutex::new(DispatchRegistry::new()),
            outbox: StdMutex::new(mpsc::channel(1).0),
            lifecycle: AsyncMutex::new(()),
            epoch: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            replaced: AtomicBool::new(false),
            unmatched: AtomicU64::new(0),
            replaced_tx,
            state_tx,
        }))
    }

    /// CSPRNG request id (pairing-flow §1: ids never come from the
    /// clock/counter helper).
    fn new_request_id() -> RequestId {
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        id
    }

    fn new_nonce16() -> [u8; 16] {
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        nonce
    }

    /// Claim the next connection generation (reconnect bookkeeping).
    pub(crate) fn claim_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Install the live connection's outbox and flip state to Connected.
    pub(crate) fn install_connection(&self, epoch: u64, tx: mpsc::Sender<conn::Outbound>) {
        *self.outbox.lock().expect("client_v2 outbox poisoned") = tx;
        self.connected.store(true, Ordering::Release);
        self.state_tx.send_replace(ConnState::Connected(epoch));
    }

    /// Reader/writer exit path: only the current generation may transition,
    /// and only once. One-shot waiters are failed; stream subscriptions
    /// survive for the next connection.
    pub(crate) fn on_disconnect(&self, epoch: u64) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.epoch.load(Ordering::Acquire) != epoch {
            return; // a newer generation owns the state now
        }
        if !self.connected.swap(false, Ordering::AcqRel) {
            return; // already down
        }
        self.registry
            .lock()
            .expect("client_v2 registry poisoned")
            .clear_transients();
        self.state_tx.send_replace(ConnState::Disconnected);
    }

    /// Unconditional teardown used by reconnect/close (user-initiated).
    /// Runs even when already marked closed — [`ClientV2::close`] flips the
    /// closed flag first and still needs the connected flip and waiter
    /// cleanup; the state watcher just must not be downgraded afterwards.
    pub(crate) fn force_disconnect(&self) {
        if self.connected.swap(false, Ordering::AcqRel) {
            self.registry
                .lock()
                .expect("client_v2 registry poisoned")
                .clear_transients();
            if !self.closed.load(Ordering::Acquire) {
                self.state_tx.send_replace(ConnState::Disconnected);
            }
        }
    }

    /// Mark this device's connection as replaced by a newer one.
    pub(crate) fn note_replaced(&self) {
        if !self.replaced.swap(true, Ordering::AcqRel) {
            self.replaced_tx.send_replace(true);
            log::warn!(
                "client_v2: this control connection was replaced by a newer \
                 connection of the same device id"
            );
        }
    }

    /// Route one inbound message; count and log unmatched frames.
    pub(crate) fn route_inbound(&self, message: CtrlMessage) {
        let outcome = self
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .route(message);
        if outcome == RouteOutcome::Unmatched {
            let n = self.unmatched.fetch_add(1, Ordering::AcqRel) + 1;
            log::debug!("client_v2: dropped unmatched frame (total {n})");
        }
    }

    /// Queue one frame for the writer. Bounded and non-blocking: a full
    /// queue means the TLS writer is stalled, which surfaces as
    /// [`ClientV2Error::OutboxFull`] instead of unbounded buffering.
    pub(crate) fn send_frame(&self, message: CtrlMessage) -> Result<(), ClientV2Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientV2Error::Closed);
        }
        if !self.connected.load(Ordering::Acquire) {
            return Err(ClientV2Error::ConnectionLost);
        }
        let tx = self
            .outbox
            .lock()
            .expect("client_v2 outbox poisoned")
            .clone();
        match tx.try_send(conn::Outbound::Frame(message)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(ClientV2Error::OutboxFull),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientV2Error::ConnectionLost),
        }
    }
}

/// Removes the waiter registration when the request future is dropped or
/// finishes (cancellation must not kill the connection, and a stale slot
/// must not block a re-registration).
struct WaiterGuard<'a> {
    inner: &'a Inner,
    key: DispatchKey,
    sender: mpsc::Sender<CtrlMessage>,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.inner
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .remove_if_match(&self.key, &self.sender);
    }
}

/// Connected, Hello-authenticated v2 control client.
///
/// Cheap to clone (`Arc` inside): the daemon, the CLI and the B6 runtime
/// should share **one** instance per device — see the module docs on
/// replacement semantics.
#[derive(Clone)]
pub struct ClientV2 {
    inner: Arc<Inner>,
}

impl ClientV2 {
    /// Connect with the default configuration: pinned TLS, AuthChallenge /
    /// signed Hello / Welcome, then a live single-reader dispatcher.
    pub async fn connect(
        server: SocketAddr,
        registry_fp: Fingerprint,
        key: &DeviceKeyPair,
        name: &str,
    ) -> Result<Self, ClientV2Error> {
        Self::connect_with_config(server, registry_fp, key, name, ClientV2Config::default()).await
    }

    /// [`ClientV2::connect`] with explicit tunables.
    pub async fn connect_with_config(
        server: SocketAddr,
        registry_fp: Fingerprint,
        key: &DeviceKeyPair,
        name: &str,
        config: ClientV2Config,
    ) -> Result<Self, ClientV2Error> {
        let inner = Inner::new(server, registry_fp, key.clone(), name.to_string(), config)?;
        conn::open_connection(&inner).await?;
        Ok(ClientV2 { inner })
    }

    /// This device's id (the key's public bytes).
    pub fn device_id(&self) -> DeviceId {
        self.inner.key.public_key()
    }

    /// Effective configuration.
    pub fn config(&self) -> &ClientV2Config {
        &self.inner.config
    }

    /// Current connection generation (bumped on every successful
    /// reconnect); useful for correlating with [`ConnState::Connected`].
    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Acquire)
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    /// Sticky: a newer connection of the same device id took over (see the
    /// module docs — share one instance instead).
    pub fn is_replaced(&self) -> bool {
        self.inner.replaced.load(Ordering::Acquire)
    }

    /// Async notification for [`ClientV2::is_replaced`].
    pub fn replaced_watcher(&self) -> watch::Receiver<bool> {
        self.inner.replaced_tx.subscribe()
    }

    /// Connection state changes (connect / drop / close).
    pub fn state_watcher(&self) -> watch::Receiver<ConnState> {
        self.inner.state_tx.subscribe()
    }

    /// Frames the dispatcher could not deliver (no matching waiter or
    /// subscriber). Observable so tests and operators can see a
    /// misbehaving registry instead of silent loss.
    pub fn dropped_unmatched_frames(&self) -> u64 {
        self.inner.unmatched.load(Ordering::Acquire)
    }

    /// General event inbox for unkeyed server pushes (`ServerError`,
    /// server→redeemer `PairChallenge`, stray `AuthChallenge`/`Welcome`).
    /// Bounded (default 64); a full inbox drops and counts.
    pub fn subscribe_events(&self) -> mpsc::Receiver<CtrlMessage> {
        self.inner
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .subscribe_events(self.inner.config.inbox_capacity)
    }

    /// Subscribe to one pairing transaction's push stream
    /// (`InviteRedeemOffered`, `PairProof`, `PairCommit`, `PairReady`).
    /// Multiple subscribers allowed; subscriptions survive reconnects.
    pub fn subscribe_pairing(
        &self,
        pairing_id: PairingId,
    ) -> Result<mpsc::Receiver<CtrlMessage>, ClientV2Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientV2Error::Closed);
        }
        Ok(self
            .inner
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .subscribe(
                DispatchKey::Pairing(pairing_id),
                self.inner.config.inbox_capacity,
            ))
    }

    /// Subscribe to one coordination session's relay stream (`Signal`
    /// frames). Survives reconnects.
    pub fn subscribe_session(
        &self,
        session: SessionId,
    ) -> Result<mpsc::Receiver<CtrlMessage>, ClientV2Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientV2Error::Closed);
        }
        Ok(self
            .inner
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .subscribe(
                DispatchKey::Session(session),
                self.inner.config.inbox_capacity,
            ))
    }

    // ---- request API ------------------------------------------------------

    /// Register a freshly generated invite code by lookup digest. The
    /// plaintext secret never travels; on timeout the same `request_id` is
    /// re-sent (server-side idempotency resolves it).
    pub async fn invite_claim(
        &self,
        lookup_digest: Digest32,
        ttl_secs: u32,
    ) -> Result<ClaimedInvite, ClientV2Error> {
        let request_id = Inner::new_request_id();
        let message = CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        };
        let response = self
            .roundtrip(DispatchKey::Request(request_id), message)
            .await?;
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
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Page through this device's own invites. `cursor` = `CURSOR_START`
    /// for the first page; `limit` 1..=32; only defined state-filter bits.
    pub async fn invite_list(
        &self,
        cursor: CodeId,
        limit: u8,
        state_filter: StateFilter,
    ) -> Result<InvitePage, ClientV2Error> {
        let request_id = Inner::new_request_id();
        let message = CtrlMessage::InviteList {
            request_id,
            cursor,
            limit,
            state_filter,
        };
        let response = self
            .roundtrip(DispatchKey::Request(request_id), message)
            .await?;
        match response {
            CtrlMessage::InviteListPage {
                next_cursor,
                entries,
                ..
            } => Ok(InvitePage {
                request_id,
                next_cursor,
                entries,
            }),
            CtrlMessage::PairError {
                scope,
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Revoke one of this device's invite codes (optimistic concurrency on
    /// `expected_revision`).
    pub async fn invite_revoke(
        &self,
        code_id: CodeId,
        expected_revision: u64,
    ) -> Result<RevokedInvite, ClientV2Error> {
        let request_id = Inner::new_request_id();
        let message = CtrlMessage::InviteRevoke {
            request_id,
            code_id,
            expected_revision,
        };
        let response = self
            .roundtrip(DispatchKey::Request(request_id), message)
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
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Start a redeem attempt. The fixed request structure (MAC + device
    /// signature) is built by the ak-identity typed layer; this client only
    /// guarantees the idempotent exchange: on timeout the same
    /// `redeem.request_id` is re-sent. The rest of the flow arrives on
    /// [`ClientV2::subscribe_pairing`] for the returned `pairing_id`.
    pub async fn invite_redeem(
        &self,
        redeem: crate::protocol_v2::RedeemRequest,
    ) -> Result<RedeemAck, ClientV2Error> {
        let request_id = redeem.request_id;
        let message = CtrlMessage::InviteRedeem(redeem);
        let response = self
            .roundtrip(DispatchKey::Request(request_id), message)
            .await?;
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
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Query a pairing transaction's state (participants only, enforced by
    /// the server). After Commit/Ready the server re-sends the fixed
    /// `PairCommit`/`PairReady` on the pairing inbox.
    pub async fn pair_status_query(
        &self,
        pairing_id: PairingId,
    ) -> Result<PairStatus, ClientV2Error> {
        let request_id = Inner::new_request_id();
        let message = CtrlMessage::PairStatusQuery {
            request_id,
            pairing_id,
        };
        let response = self
            .roundtrip(DispatchKey::Request(request_id), message)
            .await?;
        match response {
            CtrlMessage::PairStatusReply {
                pairing_id,
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
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Abort an uncommitted pairing transaction. Fire-and-forget: the
    /// contract defines no response for `PairAbort` (late business errors,
    /// if any, arrive as `PairError` pushes); the idempotency id is still
    /// attached for server-side logging.
    pub async fn pair_abort(
        &self,
        pairing_id: PairingId,
        reason: AbortReason,
    ) -> Result<(), ClientV2Error> {
        let message = CtrlMessage::PairAbort {
            request_id: Inner::new_request_id(),
            pairing_id,
            reason,
        };
        self.inner.send_frame(message)
    }

    /// Owner side of the transcript flow: forward the owner proof to the
    /// redeemer (server relays verbatim; no idempotency key exists on this
    /// message by contract).
    pub async fn send_pair_challenge(
        &self,
        transcript: Transcript256,
        owner_proof: PairProof,
    ) -> Result<(), ClientV2Error> {
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
    ) -> Result<(), ClientV2Error> {
        self.inner.send_frame(CtrlMessage::PairProof {
            pairing_id,
            transcript_digest,
            redeemer_proof,
        })
    }

    /// One side's durable prepared journal record (after the local pending
    /// journal write).
    pub async fn send_pair_prepared(
        &self,
        pairing_id: PairingId,
        transcript_digest: Digest32,
        signature: Signature64,
    ) -> Result<(), ClientV2Error> {
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
    ) -> Result<(), ClientV2Error> {
        self.inner.send_frame(CtrlMessage::PairApplied {
            pairing_id,
            transcript_digest,
            commit_revision,
            signature,
        })
    }

    // ---- legacy v2-carried session flow -----------------------------------

    /// Publish this device's candidates (max 16, ports non-zero).
    pub async fn register(&self, candidates: &[SocketAddr]) -> Result<(), ClientV2Error> {
        self.inner.send_frame(CtrlMessage::Register {
            candidates: candidates.to_vec(),
        })
    }

    /// Ask the server to open a session toward `target`, keyed by the
    /// verified `(pairing_id, code_id, connection_id)` triple. The waiter
    /// matches on `connection_id`; the idempotency id rides the
    /// `ConnectRequest`.
    pub async fn connect_peer(
        &self,
        target: DeviceId,
        pairing_id: PairingId,
        code_id: CodeId,
        connection_id: ConnectionId,
    ) -> Result<OpenedSession, ClientV2Error> {
        let request_id = Inner::new_request_id();
        let message = CtrlMessage::ConnectRequest {
            request_id,
            target,
            pairing_id,
            code_id,
            connection_id,
        };
        let response = self
            .roundtrip(DispatchKey::Connection(connection_id), message)
            .await?;
        match response {
            CtrlMessage::SessionOpened {
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            } => Ok(OpenedSession {
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
                object_id,
                code,
                retry_after_ms,
                ..
            } => Err(pair_error(scope, object_id, code, retry_after_ms)),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    /// Relay one peer signal inside a session.
    pub async fn send_signal(
        &self,
        session: SessionId,
        signal: PeerSignal,
    ) -> Result<(), ClientV2Error> {
        self.inner
            .send_frame(CtrlMessage::Signal { session, signal })
    }

    /// Cancel a coordination session (client→server only in v2).
    pub async fn cancel_session(&self, session: SessionId) -> Result<(), ClientV2Error> {
        self.inner.send_frame(CtrlMessage::Cancel { session })
    }

    /// Keepalive round-trip (`Ping` → matching `Pong` nonce).
    pub async fn ping(&self) -> Result<(), ClientV2Error> {
        let nonce = Inner::new_nonce16();
        let message = CtrlMessage::Ping { nonce };
        let response = self.roundtrip(DispatchKey::Ping(nonce), message).await?;
        match response {
            CtrlMessage::Pong { nonce: got } if got == nonce => Ok(()),
            other => Err(ClientV2Error::Unexpected(other)),
        }
    }

    // ---- lifecycle ---------------------------------------------------------

    /// Tear the connection down and connect again, retrying the full pinned
    /// handshake with the frozen backoff (1 s / 2 s / 4 s ... cap 30 s).
    /// One-shot waiters were already failed; pairing/session/event
    /// subscriptions stay live for the new generation.
    pub async fn reconnect(&self) -> Result<(), ClientV2Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ClientV2Error::Closed);
        }
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.inner.force_disconnect();
        self.inner.state_tx.send_replace(ConnState::Connecting);
        let attempts = self.inner.config.reconnect_attempts.max(1);
        let mut backoff = Backoff::default();
        let mut last = None;
        for attempt in 1..=attempts {
            match conn::open_connection(&self.inner).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < attempts {
                        let delay = backoff.next_delay();
                        log::info!(
                            "client_v2: reconnect attempt {attempt}/{attempts} failed ({e}); \
                             next try in {delay:?}"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.expect("attempts >= 1"))
    }

    /// Close the connection and stop dispatching. Idempotent.
    pub async fn close(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return; // already closed
        }
        self.inner.force_disconnect();
        let tx = self
            .inner
            .outbox
            .lock()
            .expect("client_v2 outbox poisoned")
            .clone();
        let _ = tx.try_send(conn::Outbound::Shutdown);
        self.inner
            .registry
            .lock()
            .expect("client_v2 registry poisoned")
            .clear_all();
        self.inner.state_tx.send_replace(ConnState::Closed);
    }

    // ---- internals ---------------------------------------------------------

    /// Idempotent request round-trip: register a bounded waiter, send,
    /// await the keyed response; on timeout re-send the **same frame**
    /// (same request id) up to `request_retries` times. Dropping the future
    /// cancels the wait without touching the connection (see
    /// [`WaiterGuard`]).
    async fn roundtrip(
        &self,
        key: DispatchKey,
        message: CtrlMessage,
    ) -> Result<CtrlMessage, ClientV2Error> {
        let attempts = self.inner.config.request_retries.saturating_add(1);
        let timeout = self.inner.config.response_timeout;
        for _attempt in 1..=attempts {
            let (sender, mut rx) = self
                .inner
                .registry
                .lock()
                .expect("client_v2 registry poisoned")
                .register_waiter(key.clone(), self.inner.config.inbox_capacity)
                .map_err(|()| ClientV2Error::RequestInFlight)?;
            let _guard = WaiterGuard {
                inner: &self.inner,
                key: key.clone(),
                sender,
            };
            self.inner.send_frame(message.clone())?;
            match tokio::time::timeout(timeout, rx.recv()).await {
                // Response timeout: same request id, same bytes, again.
                Err(_elapsed) => continue,
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {
                    // Registry cleared the waiter: connection dropped (or
                    // was replaced) while we waited.
                    return Err(if self.is_replaced() {
                        ClientV2Error::Replaced
                    } else {
                        ClientV2Error::ConnectionLost
                    });
                }
            }
        }
        Err(ClientV2Error::Timeout { attempts })
    }
}

/// Map a `PairError` response onto the error type (business errors resolve
/// the pending request; they never echo user input).
fn pair_error(
    scope: ErrorScope,
    object_id: [u8; 16],
    code: u16,
    retry_after_ms: u32,
) -> ClientV2Error {
    ClientV2Error::Pair(PairErrorInfo {
        scope,
        object_id,
        code,
        retry_after_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique_random_draws() {
        let a = Inner::new_request_id();
        let b = Inner::new_request_id();
        assert_ne!(a, b);
    }

    #[test]
    fn bad_names_are_refused_before_any_io() {
        assert!(matches!(
            tls::check_display_name("bad\u{1}name"),
            Err(ClientV2Error::BadName { .. })
        ));
    }
}
