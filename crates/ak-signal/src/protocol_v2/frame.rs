//! v2 frame layer: `[version=2 u8][type u8][payload_len u16 BE][payload]`,
//! hard cap 4096 bytes per frame including the 4-byte header.
//!
//! Rules frozen by the contract (`docs/contracts/protocol-v2.md`, identical
//! to plan A.4): unknown type bytes, declared lengths over the payload cap,
//! truncated or trailing input are all rejected; the payload budget is
//! validated before decoding allocates; and encoding is fallible
//! ([`CtrlFrame::try_encode`]) — production code can never emit a frame by
//! truncating lengths. Direction legality is checked by the `_as` variants,
//! which refuse to encode/decode a message traveling the wrong way (see
//! [`super::direction`]).

use super::direction::Role;
use super::message::{self, CtrlMessage, EndpointBlob};

/// Signaling protocol version (frame header byte 0). v1 stays at 1; v2 is
/// never negotiated down to plaintext v1 frames.
pub const VERSION: u8 = 2;
/// Frame header size: version(1) + type(1) + payload_len(2).
pub const HEADER_LEN: usize = 4;
/// Hard cap for one signaling frame, header included.
pub const MAX_FRAME_BYTES: usize = 4096;
/// Hard cap for one frame payload (header excluded).
pub const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - HEADER_LEN;

/// Why a frame could not be encoded. v2 encoding validates every budget a
/// decoder would check, so an over-limit field is an error, not a silently
/// truncated or over-long frame.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("string field exceeds {limit} bytes")]
    StringTooLong { limit: usize },
    #[error("string field contains control characters (C0/C1/DEL)")]
    ControlChars,
    #[error("candidate count {count} exceeds {limit}")]
    TooManyCandidates { count: usize, limit: usize },
    #[error("invite summary count {count} exceeds {limit}")]
    TooManyEntries { count: usize, limit: usize },
    #[error("candidate port must not be 0")]
    PortZero,
    #[error("invite list limit {0} outside 1..=32")]
    BadInviteLimit(u8),
    #[error("state filter byte 0x{0:02x} uses undefined bits")]
    BadStateFilter(u8),
    #[error("payload {len} bytes exceeds cap {cap}")]
    PayloadTooLarge { len: usize, cap: usize },
    #[error("endpoint blob of {len} bytes outside 1..={limit}")]
    EndpointBlobSize { len: usize, limit: usize },
    #[error("type {ty:#04x} carries no wave-28 extension section")]
    BadExtension { ty: u8 },
    #[error("type {ty:#04x} must not be sent by {sender:?}")]
    WrongDirection { ty: u8, sender: Role },
}

/// Why a v2 frame was rejected; every decode path validates version, type,
/// direction (in `decode_as`), lengths, budgets and string hygiene before
/// accepting a frame.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame shorter than the 4-byte header")]
    TruncatedHeader,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("unknown message type {0:#04x}")]
    UnknownType(u8),
    #[error("payload {declared} bytes exceeds cap {cap}")]
    Oversized { declared: u16, cap: u16 },
    #[error("payload truncated: declared {declared}, present {present}")]
    TruncatedPayload { declared: u16, present: usize },
    #[error("candidate count {count} exceeds {limit}")]
    TooManyCandidates { count: usize, limit: usize },
    #[error("invite summary count {count} exceeds {limit}")]
    TooManyEntries { count: usize, limit: usize },
    #[error("string field exceeds {limit} bytes")]
    StringTooLong { limit: usize },
    #[error("string field contains control characters (C0/C1/DEL)")]
    ControlChars,
    #[error("field out of range: {0}")]
    Malformed(&'static str),
    #[error("invite list limit {0} outside 1..=32")]
    BadInviteLimit(u8),
    #[error("state filter byte 0x{0:02x} uses undefined bits")]
    BadStateFilter(u8),
    #[error("extension flags 0x{0:02x} use undefined or empty bits")]
    BadExtensionFlags(u8),
    #[error("endpoint blob of {len} bytes outside 1..={limit}")]
    EndpointBlobSize { len: usize, limit: usize },
    #[error("type {ty:#04x} carries no wave-28 extension section")]
    BadExtension { ty: u8 },
    #[error("leftover {0} trailing bytes")]
    Trailing(usize),
    #[error("type {ty:#04x} must not be sent by {sender:?}")]
    WrongDirection { ty: u8, sender: Role },
}

/// One version-2 signaling frame. The version byte is fixed at
/// [`VERSION`]; only the message varies. The wave-28 fields
/// ([`endpoint`](CtrlFrame::endpoint), [`max_redeems`](CtrlFrame::max_redeems))
/// ride an optional trailing section inside the declared payload of the
/// four extended tags — `CtrlFrame::new` leaves them empty, producing
/// bytes identical to the frozen legacy shape, so pre-wave-28 peers and
/// shapes interoperate unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtrlFrame {
    pub message: CtrlMessage,
    /// Wave-28 endpoint blob: connection settings carried verbatim.
    /// Legal on `InviteClaim` (owner settings, stored on the code row),
    /// `InviteRedeem` (redeemer settings, forwarded to the owner inside
    /// `InviteRedeemOffered`) and `PairCommit` (owner settings relayed
    /// down to the redeemer at the authoritative commit point).
    pub endpoint: Option<EndpointBlob>,
    /// Wave-28 claim redeem cap (`InviteClaim` only): 0 = unlimited (the
    /// legacy default), 1 = the v1 "one code, one task" model. Encoding
    /// refuses the field on every other type.
    pub max_redeems: Option<u32>,
}

impl From<CtrlMessage> for CtrlFrame {
    fn from(message: CtrlMessage) -> Self {
        CtrlFrame::new(message)
    }
}

impl CtrlFrame {
    pub fn new(message: CtrlMessage) -> Self {
        CtrlFrame {
            message,
            endpoint: None,
            max_redeems: None,
        }
    }

    /// Attach the wave-28 endpoint blob (builder-style).
    pub fn with_endpoint(mut self, blob: EndpointBlob) -> Self {
        self.endpoint = Some(blob);
        self
    }

    /// Attach the wave-28 claim redeem cap (builder-style).
    pub fn with_max_redeems(mut self, cap: u32) -> Self {
        self.max_redeems = Some(cap);
        self
    }

    /// Append the frame to `out`. Fallible by contract: every field budget
    /// (string caps, candidate/entry counts, candidate ports, the wave-28
    /// extension section) is validated before writing, and the final
    /// payload length is checked against the frame cap instead of being
    /// truncated or wrapped.
    pub fn try_encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let mut payload = Vec::new();
        message::encode_payload(
            &self.message,
            self.endpoint.as_ref(),
            self.max_redeems,
            &mut payload,
        )?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(EncodeError::PayloadTooLarge {
                len: payload.len(),
                cap: MAX_PAYLOAD_BYTES,
            });
        }
        out.reserve(HEADER_LEN + payload.len());
        out.push(VERSION);
        out.push(self.message.type_byte());
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(())
    }

    /// [`CtrlFrame::try_encode`] with direction legality enforced: refuses
    /// when `sender` may not emit this message type (e.g. a client encoding
    /// a `Welcome`, or any endpoint but the server encoding a
    /// `Signal(SameEgress)`).
    pub fn try_encode_as(&self, sender: Role, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        if !self.message.allows_sender(sender) {
            return Err(EncodeError::WrongDirection {
                ty: self.message.type_byte(),
                sender,
            });
        }
        self.try_encode(out)
    }

    /// Strict decode of exactly one frame (leftover input is an error; see
    /// [`DecodeError::Trailing`]). Unknown types, bad versions, declared
    /// lengths over the cap, truncated payloads, undefined enum values and
    /// trailing bytes are all rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader);
        }
        let version = bytes[0];
        if version != VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let ty = bytes[1];
        if !message::is_known_type(ty) {
            return Err(DecodeError::UnknownType(ty));
        }
        let declared = u16::from_be_bytes([bytes[2], bytes[3]]);
        if HEADER_LEN + declared as usize > MAX_FRAME_BYTES {
            return Err(DecodeError::Oversized {
                declared,
                cap: MAX_PAYLOAD_BYTES as u16,
            });
        }
        let end = HEADER_LEN + declared as usize;
        if bytes.len() < end {
            return Err(DecodeError::TruncatedPayload {
                declared,
                present: bytes.len() - HEADER_LEN,
            });
        }
        if bytes.len() > end {
            return Err(DecodeError::Trailing(bytes.len() - end));
        }
        let (message, endpoint, max_redeems) =
            message::decode_payload(ty, &bytes[HEADER_LEN..end])?;
        Ok(CtrlFrame {
            message,
            endpoint,
            max_redeems,
        })
    }

    /// [`CtrlFrame::decode`] with direction legality enforced: the frame is
    /// decoded as if sent by `receiver`'s peer and refused when that peer
    /// may not emit this message type. A server must not accept another
    /// server's `Welcome`; a client must not accept a `Register`.
    pub fn decode_as(bytes: &[u8], receiver: Role) -> Result<Self, DecodeError> {
        let frame = Self::decode(bytes)?;
        let sender = receiver.opposite();
        if !frame.message.allows_sender(sender) {
            return Err(DecodeError::WrongDirection {
                ty: frame.message.type_byte(),
                sender,
            });
        }
        Ok(frame)
    }
}
