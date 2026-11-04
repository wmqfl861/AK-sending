//! Injected signaling/pairing dependency: [`PairingTransport`].
//!
//! The runtime never talks to the real TLS signaling service (A3
//! `client_v2`, still in flight). Instead it depends on this trait; the
//! method names and payload shapes mirror the public A3 control-plane
//! messages (`InviteClaim`/`InviteClaimed`, `InviteRedeem`/
//! `InviteRedeemPending`, the `PairChallenge`…`PairReady` transcript and
//! `ConnectRequest`/`SessionOpened`). Wave 21 provides the real adapter
//! over `ak-signal`; wave 20 tests and dogfooding use
//! [`FakePairingTransport`], whose offline buffering models the
//! reconnect-replay behaviour required of the real client.
use ak_core::error::ErrorCode;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use tokio::sync::watch;

use crate::scheduler::key::{CodeId, RegistryId};

/// Byte length of a `pairing_id[16]`.
pub const PAIRING_ID_LEN: usize = 16;
/// Byte length of a peer fingerprint (`peer_fingerprint()`, 32 bytes).
pub const PEER_FINGERPRINT_LEN: usize = 32;

/// Bound and sanitize an input-derived string.
fn sanitize(detail: &str) -> String {
    let mut out: String = detail
        .chars()
        .take(64)
        .map(|c| match c as u32 {
            0x00..=0x1F | 0x7F..=0x9F => '?',
            _ => c,
        })
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

macro_rules! runtime_id {
    ($name:ident, $len:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; $len]);

        impl $name {
            /// Wrap raw bytes.
            pub fn from_bytes(bytes: [u8; $len]) -> Self {
                $name(bytes)
            }

            /// Raw bytes.
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            /// Deterministic test id from a counter (big-endian low bytes).
            pub fn from_counter(counter: u64) -> Self {
                let mut bytes = [0u8; $len];
                bytes[$len - 8..].copy_from_slice(&counter.to_be_bytes());
                $name(bytes)
            }

            /// Lowercase hex display form.
            pub fn to_hex(&self) -> String {
                self.0.iter().map(|b| format!("{b:02x}")).collect()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }
    };
}

runtime_id!(
    PairingId,
    PAIRING_ID_LEN,
    "Pairing-transaction identity (`pairing_id[16]`)."
);
runtime_id!(
    PeerFingerprint,
    PEER_FINGERPRINT_LEN,
    "Authenticated peer identity (`DirectChannel.peer_fingerprint()`)."
);

/// Transport-level pairing failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// The signaling connection is down; retry later.
    #[error("transport offline")]
    Offline,
    /// The peer/server rejected the request.
    #[error("pairing rejected: {reason}")]
    Rejected {
        /// Bounded sanitized reason.
        reason: String,
    },
    /// A deadline/lease expired.
    #[error("pairing deadline expired")]
    Expired,
}

impl PairingError {
    /// Construct [`PairingError::Rejected`] with a sanitized reason.
    pub fn rejected(reason: &str) -> Self {
        PairingError::Rejected {
            reason: sanitize(reason),
        }
    }

    /// Stable error code recorded when a task fails on this transport
    /// error.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            PairingError::Offline => ErrorCode::DirectUnavailable,
            PairingError::Rejected { .. } => ErrorCode::AuthFailed,
            PairingError::Expired => ErrorCode::TargetRevoked,
        }
    }
}

/// A future returned by [`PairingTransport`] methods.
pub type PairingFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PairingError>> + Send + 'a>>;

/// Owner request registering a freshly generated invite code by lookup
/// digest (mirrors the A3 `InviteClaim` message; the plaintext secret never
/// leaves the endpoints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteClaimRequest {
    /// Hash of the code material the server indexes.
    pub lookup_digest: [u8; 32],
    /// Requested lifetime; 0 = long-lived.
    pub ttl_secs: u32,
}

/// Server acknowledgment of a claim (A3 `InviteClaimed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteClaimed {
    /// Identity assigned to the claimed code.
    pub code_id: CodeId,
    /// Server-side creation time (ms).
    pub created_ms: u64,
    /// Expiry (ms); 0 = long-lived.
    pub expires_ms: u64,
    /// Revision of the invite record.
    pub revision: u64,
}

/// Redeem attempt (the fixed 184-byte `InviteRedeem` request structure of
/// the A3 control plane, mirrored field for field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemPayload {
    /// Client-side request correlation id.
    pub request_id: [u8; 16],
    /// Hash under which the code was claimed.
    pub lookup_digest: [u8; 32],
    /// Fresh redeemer nonce.
    pub redeemer_nonce: [u8; 32],
    /// Redeem deadline (ms).
    pub deadline_ms: u64,
    /// MAC binding the domain fields.
    pub request_mac: [u8; 32],
    /// Ed25519 signature over the frozen domain.
    pub signature: [u8; 64],
}

/// Server acceptance of a redeem (A3 `InviteRedeemPending`); the same shape
/// is answered whether or not the code exists (no existence leak).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedeemAccepted {
    /// Transaction identity issued for this pairing.
    pub pairing_id: PairingId,
    /// Deadline for completing the pairing transcript (ms).
    pub deadline_ms: u64,
}

/// The redeemer's half of the pairing transcript (A3 `PairProof`/
/// `PairPrepared` material), submitted to finish the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingConfirmation {
    /// The transaction being confirmed.
    pub pairing_id: PairingId,
    /// Digest of the frozen 256-byte transcript.
    pub transcript_digest: [u8; 32],
    /// 96-byte proof (`mac[32] + signature[64]`).
    pub proof: [u8; 96],
}

/// Result of a completed pairing transaction (A3 `PairReady`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingReadyInfo {
    /// The completed transaction.
    pub pairing_id: PairingId,
    /// The code the relation belongs to.
    pub code_id: CodeId,
    /// The peer this device is now paired with.
    pub peer: PeerFingerprint,
    /// Authoritative commit revision.
    pub commit_revision: u64,
}

/// Result of opening a data-session toward a paired peer (A3
/// `ConnectRequest` → `SessionOpened`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSessionInfo {
    /// Signaling session id.
    pub session_id: [u8; 16],
    /// Session expiry (ms).
    pub expires_ms: u64,
}

/// An inbound pairing-plane event (the runtime listener consumes these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingEvent {
    /// A redeemer answered one of this device's codes (A3
    /// `InviteRedeemOffered`); the pairing module (wave 21) answers the
    /// challenge, the listener only surfaces it.
    RedeemOffered {
        /// The code being redeemed.
        code_id: CodeId,
        /// The pairing transaction.
        pairing_id: PairingId,
        /// The redeeming peer.
        peer: PeerFingerprint,
    },
    /// A pairing relation became active. Inbound transfers from this peer
    /// are legitimate now, so the listener submits a receive task.
    PairReady {
        /// Registry owning the code.
        registry_id: RegistryId,
        /// The code that produced the relation.
        code_id: CodeId,
        /// The pairing relation.
        pairing_id: PairingId,
        /// The newly paired peer.
        peer: PeerFingerprint,
    },
    /// A pairing transaction aborted.
    PairAborted {
        /// The aborted transaction.
        pairing_id: PairingId,
        /// Sanitized abort reason code.
        reason: u8,
    },
    /// The signaling connection dropped.
    Disconnected,
    /// The signaling connection is back; `replayed` counts the buffered
    /// events replayed right after this marker.
    Reconnected {
        /// Number of replayed events following this marker.
        replayed: u16,
    },
}

/// Injected signaling dependency of the runtime (see the module docs).
///
/// All methods are dyn-compatible; implementations must answer
/// [`PairingError::Offline`] while disconnected instead of blocking
/// indefinitely, except `next_pairing_event`, which parks until an event
/// (or a reconnect) is available.
pub trait PairingTransport: Send + Sync {
    /// Register a freshly generated invite code (`InviteClaim`).
    fn claim_invite(&self, claim: InviteClaimRequest) -> PairingFuture<'_, InviteClaimed>;

    /// Start redeeming a code (`InviteRedeem`).
    fn redeem(&self, request: RedeemPayload) -> PairingFuture<'_, RedeemAccepted>;

    /// Finish the pairing transcript toward `PairReady`.
    fn confirm_pairing(
        &self,
        confirmation: PairingConfirmation,
    ) -> PairingFuture<'_, PairingReadyInfo>;

    /// Open a data session toward a paired peer (`ConnectRequest`).
    fn open_session(
        &self,
        pairing_id: PairingId,
        code_id: CodeId,
    ) -> PairingFuture<'_, OpenSessionInfo>;

    /// Next inbound pairing-plane event (parks until one exists).
    fn next_pairing_event(&self) -> PairingFuture<'_, PairingEvent>;

    /// Whether the signaling connection is currently up.
    fn is_connected(&self) -> bool;
}

/// Shared state of [`FakePairingTransport`].
#[derive(Debug, Default)]
struct FakeState {
    online: bool,
    queue: VecDeque<PairingEvent>,
    claim_calls: Vec<InviteClaimRequest>,
    redeem_calls: Vec<RedeemPayload>,
    confirm_calls: Vec<PairingConfirmation>,
    open_calls: Vec<(PairingId, CodeId)>,
    next_pairing: u64,
    next_code: u64,
    next_session: u64,
}

/// Deterministic fake transport.
///
/// * request methods succeed immediately when online and fail
///   [`PairingError::Offline`] when offline; every call is recorded;
/// * events pushed with [`FakePairingTransport::push_event`] while offline
///   are buffered and, on [`FakePairingTransport::set_online`], replayed in
///   order behind a `Reconnected{replayed}` marker — the behaviour the real
///   A3 client must offer after a reconnect.
#[derive(Debug)]
pub struct FakePairingTransport {
    state: Mutex<FakeState>,
    tick_tx: watch::Sender<u64>,
    tick_rx: watch::Receiver<u64>,
}

impl FakePairingTransport {
    /// Create a transport; `initially_online` sets the connection state.
    pub fn new(initially_online: bool) -> Self {
        let (tick_tx, tick_rx) = watch::channel(0u64);
        FakePairingTransport {
            state: Mutex::new(FakeState {
                online: initially_online,
                ..FakeState::default()
            }),
            tick_tx,
            tick_rx,
        }
    }

    fn tick(&self) {
        let next = *self.tick_rx.borrow() + 1;
        let _ = self.tick_tx.send(next);
    }

    /// Queue an inbound pairing event. While offline the event is buffered
    /// (it will be replayed on reconnect); while online it is delivered by
    /// the next `next_pairing_event` call.
    pub fn push_event(&self, event: PairingEvent) {
        let mut state = self.state.lock().expect("fake transport lock");
        state.queue.push_back(event);
        drop(state);
        self.tick();
    }

    /// Simulate a connect/disconnect. Going online prepends a
    /// `Reconnected{replayed}` marker in front of everything buffered while
    /// offline.
    pub fn set_online(&self, online: bool) {
        let mut state = self.state.lock().expect("fake transport lock");
        if state.online == online {
            return;
        }
        if online {
            let replayed = state.queue.len();
            state.queue.push_front(PairingEvent::Reconnected {
                replayed: replayed as u16,
            });
        }
        state.online = online;
        drop(state);
        self.tick();
    }

    /// Recorded `claim_invite` calls.
    pub fn claim_calls(&self) -> Vec<InviteClaimRequest> {
        self.state
            .lock()
            .expect("fake transport lock")
            .claim_calls
            .clone()
    }

    /// Recorded `redeem` calls.
    pub fn redeem_calls(&self) -> Vec<RedeemPayload> {
        self.state
            .lock()
            .expect("fake transport lock")
            .redeem_calls
            .clone()
    }

    /// Recorded `confirm_pairing` calls.
    pub fn confirm_calls(&self) -> Vec<PairingConfirmation> {
        self.state
            .lock()
            .expect("fake transport lock")
            .confirm_calls
            .clone()
    }

    /// Recorded `open_session` calls.
    pub fn open_calls(&self) -> Vec<(PairingId, CodeId)> {
        self.state
            .lock()
            .expect("fake transport lock")
            .open_calls
            .clone()
    }

    /// Number of events still buffered/queued (test introspection).
    pub fn pending_events(&self) -> usize {
        self.state.lock().expect("fake transport lock").queue.len()
    }
}

/// Deterministic `CodeId` from a counter (big-endian low bytes).
fn code_from_counter(counter: u64) -> CodeId {
    let mut bytes = [0u8; 16];
    bytes[8..].copy_from_slice(&counter.to_be_bytes());
    CodeId::from_bytes(bytes)
}

impl PairingTransport for FakePairingTransport {
    fn claim_invite(&self, claim: InviteClaimRequest) -> PairingFuture<'_, InviteClaimed> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("fake transport lock");
            if !state.online {
                return Err(PairingError::Offline);
            }
            state.claim_calls.push(claim.clone());
            state.next_code += 1;
            Ok(InviteClaimed {
                code_id: code_from_counter(state.next_code),
                created_ms: 0,
                expires_ms: u64::from(claim.ttl_secs) * 1000,
                revision: 1,
            })
        })
    }

    fn redeem(&self, request: RedeemPayload) -> PairingFuture<'_, RedeemAccepted> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("fake transport lock");
            if !state.online {
                return Err(PairingError::Offline);
            }
            state.redeem_calls.push(request.clone());
            state.next_pairing += 1;
            Ok(RedeemAccepted {
                pairing_id: PairingId::from_counter(state.next_pairing),
                deadline_ms: request.deadline_ms,
            })
        })
    }

    fn confirm_pairing(
        &self,
        confirmation: PairingConfirmation,
    ) -> PairingFuture<'_, PairingReadyInfo> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("fake transport lock");
            if !state.online {
                return Err(PairingError::Offline);
            }
            state.confirm_calls.push(confirmation);
            state.next_code += 1;
            Ok(PairingReadyInfo {
                pairing_id: confirmation.pairing_id,
                code_id: code_from_counter(state.next_code),
                peer: PeerFingerprint::from_counter(state.next_pairing.max(1)),
                commit_revision: 1,
            })
        })
    }

    fn open_session(
        &self,
        pairing_id: PairingId,
        code_id: CodeId,
    ) -> PairingFuture<'_, OpenSessionInfo> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("fake transport lock");
            if !state.online {
                return Err(PairingError::Offline);
            }
            state.open_calls.push((pairing_id, code_id));
            state.next_session += 1;
            Ok(OpenSessionInfo {
                session_id: *PairingId::from_counter(state.next_session).as_bytes(),
                expires_ms: 0,
            })
        })
    }

    fn next_pairing_event(&self) -> PairingFuture<'_, PairingEvent> {
        Box::pin(async move {
            let mut ticks = self.tick_tx.subscribe();
            loop {
                {
                    let mut state = self.state.lock().expect("fake transport lock");
                    if state.online {
                        if let Some(event) = state.queue.pop_front() {
                            drop(state);
                            self.tick();
                            return Ok(event);
                        }
                    }
                }
                if ticks.changed().await.is_err() {
                    return Err(PairingError::Offline);
                }
            }
        })
    }

    fn is_connected(&self) -> bool {
        self.state.lock().expect("fake transport lock").online
    }
}
