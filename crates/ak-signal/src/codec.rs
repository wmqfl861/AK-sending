//! Signaling wire codec: fixed message set, every field bounded.
//!
//! Frame layout: `[version u8][type u8][payload_len u16 BE][payload]`.
//! Hard cap 4096 bytes per frame. Candidates are `SocketAddr`s only; the
//! payload types have no byte-blob field by construction.
#![forbid(unsafe_code)]

use std::net::SocketAddr;

use ak_identity::{DeviceKeyPair, Fingerprint};

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 4;
pub const MAX_FRAME_BYTES: usize = 4096;
pub const MAX_CANDIDATES: usize = 16;
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_ERROR_BYTES: usize = 128;
pub const HELLO_MAX_AGE_MS: u64 = 10_000;

pub type DeviceId = [u8; 32];
pub type SessionId = [u8; 16];

/// Structured peer-to-peer signal payloads relayed by the server verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSignal {
    /// Connection candidates, LAN and public, IP:port only.
    Candidates(Vec<SocketAddr>),
    /// Agreed simultaneous-open epoch (ms) plus a nonce to match probes.
    PunchAt { epoch_unix_ms: u64, nonce: [u8; 8] },
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtrlMessage {
    /// First message on a connection; signature covers version|device_id|name|timestamp.
    Hello {
        version: u16,
        device_id: DeviceId,
        name: String,
        timestamp_unix_ms: u64,
        signature: [u8; 64],
    },
    Welcome { session_ttl_secs: u32 },
    /// Replace this device's published candidates.
    Register { candidates: Vec<SocketAddr> },
    /// Ask the server to open (or reuse) a session toward `target`.
    ConnectRequest { target: DeviceId },
    /// Sent to both endpoints when a session exists.
    SessionOpened { session: SessionId, peer: DeviceId },
    /// Relay one bounded peer signal within a session.
    Signal { session: SessionId, signal: PeerSignal },
    Cancel { session: SessionId },
    ServerError { code: u16, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtrlFrame {
    pub version: u8,
    pub message: CtrlMessage,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame shorter than the 4-byte header")]
    TruncatedHeader,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("payload {declared} bytes exceeds cap {cap}")]
    Oversized { declared: u16, cap: u16 },
    #[error("payload truncated: declared {declared}, present {present}")]
    TruncatedPayload { declared: u16, present: usize },
    #[error("candidate count {count} exceeds {limit}")]
    TooManyCandidates { count: usize, limit: usize },
    #[error("string field exceeds {limit} bytes")]
    StringTooLong { limit: usize },
    #[error("field out of range: {0}")]
    Malformed(&'static str),
    #[error("leftover {0} trailing bytes")]
    Trailing(usize),
}

impl CtrlFrame {
    pub fn encode(&self, out: &mut Vec<u8>) {
        todo!("owned by ak-signal agent")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        todo!("owned by ak-signal agent")
    }
}

/// Canonical signing bytes for Hello. Deterministic, version-tagged.
pub fn hello_signing_bytes(
    version: u16,
    device_id: &DeviceId,
    name: &str,
    timestamp_unix_ms: u64,
) -> Vec<u8> {
    todo!("owned by ak-signal agent")
}

/// Build a signed Hello for this device key.
pub fn signed_hello(key: &DeviceKeyPair, name: &str, now_unix_ms: u64) -> CtrlMessage {
    todo!("owned by ak-signal agent")
}

/// Verify a Hello signature and freshness. Freshness window: ±HELLO_MAX_AGE_MS.
pub fn verify_hello(message: &CtrlMessage, now_unix_ms: u64) -> Result<Fingerprint, DecodeError> {
    todo!("owned by ak-signal agent")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-signal agent"]
    fn placeholder() {}
}
