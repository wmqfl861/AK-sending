//! Errors for the v2 control-plane client.

use std::time::Duration;

use crate::protocol_v2::{CtrlMessage, DecodeError, EncodeError, ErrorScope};

/// `ServerError.code` the v2 server sends on a control connection that is
/// being replaced by a newer connection of the same `device_id` (v1
/// replacement semantics carried into v2; see `docs/contracts/protocol-v2.md`
/// and the module docs of [`super`]). Range choice: above the legacy v1
/// codes `1..=5`, below the frozen v2 business codes `0x2101..=0x2108`.
///
/// Alignment note for A2 (wave 21): the server should send
/// `ServerError { code: ERR_DEVICE_REPLACED }` on the old connection before
/// closing it, so this client can surface [`ClientV2::is_replaced`] instead
/// of a plain connection loss.
pub const ERR_DEVICE_REPLACED: u16 = 0x1001;

/// One frozen `PairError` (tag `0x1C`) resolving or accompanying a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairErrorInfo {
    pub scope: ErrorScope,
    pub object_id: [u8; 16],
    pub code: u16,
    pub retry_after_ms: u32,
}

impl std::fmt::Display for PairErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "code {:#06x}, scope {:?}, retry after {} ms",
            self.code, self.scope, self.retry_after_ms
        )
    }
}

/// v2 client failure: transport, handshake, dispatch or business error.
///
/// The `Unexpected` variant carries a whole [`CtrlMessage`] (frozen error
/// surface, same pattern as the v1 `SignalError`), which trips clippy's
/// `large_enum_variant` / `result_large_err` thresholds; boxing would change
/// the API.
#[derive(Debug, thiserror::Error)]
#[allow(clippy::large_enum_variant)]
pub enum ClientV2Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame encode rejected: {0}")]
    Encode(#[from] EncodeError),
    #[error("frame decode rejected: {0}")]
    Decode(#[from] DecodeError),
    #[error("device name invalid: control characters or over {max} bytes")]
    BadName { max: usize },
    #[error("TLS or TCP handshake failed within {0:?}")]
    HandshakeTimeout(Duration),
    #[error("AuthChallenge names a different registry than the pinned fingerprint")]
    RegistryMismatch,
    #[error("AuthChallenge already expired at {expires_ms} ms")]
    ChallengeExpired { expires_ms: u64 },
    #[error("server rejected: code {code}: {message}")]
    Server { code: u16, message: String },
    #[error("request timed out after {attempts} attempt(s) with the same request id")]
    Timeout { attempts: u32 },
    #[error("connection lost before the response arrived")]
    ConnectionLost,
    #[error("this connection was replaced by a newer connection of the same device")]
    Replaced,
    #[error("client was closed")]
    Closed,
    #[error("outbound queue full (slow server or stalled TLS writer)")]
    OutboxFull,
    #[error("another request with the same idempotency key is already in flight")]
    RequestInFlight,
    #[error("pairing business error {0}")]
    Pair(PairErrorInfo),
    #[error("unexpected message: {0:?}")]
    Unexpected(CtrlMessage),
}
