//! Error types of the pairing orchestration (wave 21, A6).
//!
//! Every variant is bounded: server- and endpoint-side failures carry
//! fixed numeric codes or frozen enum values, never echo user input, and
//! never embed invite-code material.
#![forbid(unsafe_code)]

use ak_identity::invite::InviteCodeError;
use ak_identity::pairing_txn::PairingVerifyError;
use ak_signal::protocol_v2::ErrorScope;

/// Transport-level failure of [`crate::pairing::client::RegistryClient`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The control connection is down; retry or reconnect first.
    #[error("control connection is down")]
    Offline,
    /// The client was closed.
    #[error("client was closed")]
    Closed,
    /// A newer connection of the same device id took over.
    #[error("connection replaced by a newer one of the same device")]
    Replaced,
    /// The connection dropped while a reply was pending.
    #[error("connection lost while waiting for the reply")]
    ConnectionLost,
    /// No reply after `attempts` re-sends of the same request id.
    #[error("no reply after {attempts} attempts")]
    Timeout { attempts: u32 },
    /// A request with the same id is already in flight.
    #[error("a request with the same id is already in flight")]
    RequestInFlight,
    /// Legacy bounded server error (session plane).
    #[error("server error {code}")]
    Server { code: u16 },
    /// Fixed invite business error (`PairError`), never echoing user input.
    #[error("pair error 0x{code:04x} (scope {scope:?}, retry after {retry_after_ms} ms)")]
    Pair {
        /// Which object `object_id` named (carried separately by callers).
        scope: ErrorScope,
        /// The fixed business error code (e.g. `PAIR_UNAVAILABLE`).
        code: u16,
        /// Server-suggested retry delay, 0 when not applicable.
        retry_after_ms: u32,
    },
    /// Handshake or framing violation (bounded context string).
    #[error("protocol violation: {0}")]
    Protocol(String),
}

impl ClientError {
    /// The `PairError.code` when this is a business error, else `None`.
    pub fn pair_code(&self) -> Option<u16> {
        match self {
            ClientError::Pair { code, .. } => Some(*code),
            _ => None,
        }
    }
}

/// Failure of an orchestrated pairing flow (owner or redeemer side).
#[derive(Debug, thiserror::Error)]
pub enum PairFlowError {
    /// Transport-level failure.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The typed invite code was rejected during parsing (typo, homoglyph,
    /// wrong grouping …).
    #[error("invite code rejected: {0}")]
    BadCode(#[from] InviteCodeError),
    /// The unified "target unavailable" answer (unknown/expired/revoked
    /// code, owner offline past the deadline, foreign pairing).
    #[error("pairing target unavailable (0x{code:04x})")]
    Unavailable { code: u16 },
    /// Any other server-side business rejection.
    #[error("pairing rejected: 0x{code:04x}")]
    Rejected { code: u16 },
    /// Transcript verification failed (substituted key, wrong code,
    /// replayed nonce, expired proof, tampered digest).
    #[error("transcript verification failed: {0}")]
    Verify(#[from] PairingVerifyError),
    /// Bounded local-state violation (journal conflict, unknown pairing …).
    #[error("local pairing state error: {0}")]
    Local(String),
    /// Journal or trust-book I/O failure.
    #[error("local pairing store io error: {0}")]
    Io(#[from] std::io::Error),
    /// The flow did not complete within its bounded wait.
    #[error("timed out waiting for the pairing flow")]
    Timeout,
}

impl PairFlowError {
    /// Map a business error code onto the unified variants.
    pub fn from_pair_code(code: u16) -> Self {
        if code == ak_signal::protocol_v2::message::PAIR_UNAVAILABLE {
            PairFlowError::Unavailable { code }
        } else {
            PairFlowError::Rejected { code }
        }
    }
}
