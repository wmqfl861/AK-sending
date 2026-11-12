//! Control protocol v2 codec: the pairing-invite message plane.
//!
//! Frozen contract: `docs/contracts/protocol-v2.md` (byte-identical to
//! `docs/plans/pairing-code-plan-part2.md` section A.4). The frame layout
//! stays `[version u8][type u8][payload_len u16 BE][payload]` with version
//! byte `2` and the same 4096-byte hard cap; integers are big-endian and
//! fixed arrays carry no length prefix.
//!
//! Discipline carried over from the v1 codec (the legacy `crate::codec`
//! module, gated behind the `legacy-plaintext-signal` feature), applied to
//! an independent module — v1 and its decoder stay untouched and v2 frames
//! are never v1 frames with appended fields:
//!
//! - **Closed set**: type tags `0x01..=0x1E` and signal sub-tags
//!   `0x01..=0x04` are exhaustive; every other byte is rejected and tests
//!   enumerate all 256 values of each.
//! - **Budgets first**: per-tag payload budgets are validated (on decode
//!   before allocating, on encode via fallible [`frame::CtrlFrame::try_encode`])
//!   — unknown tags, undefined closed-enum values, oversized lengths,
//!   truncation and trailing bytes are all refused; nothing truncates.
//! - **Direction in the type layer**: [`CtrlMessage::direction`] plus the
//!   `_as` frame entry points make an illegally-directed frame an error,
//!   not a runtime convention.
//! - **No secrets, no payload data**: lookup digests, fixed proofs and
//!   bounded addresses only; plaintext invite codes never appear on this
//!   plane (they live in ak-identity per plan A.2/A.7-A4). The one
//!   wave-28 exception is the opaque, size-bounded [`EndpointBlob`]
//!   (connection-settings hints, ≤512 bytes, signature-uncovered)
//!   riding the trailing extension section of four extended tags.
//!
//! This module is pure codec: TLS transport, the SQLite registry, and
//! Hello-v2 signature verification (which binds the TLS exporter) belong to
//! the server/client/TLS work (plan A.5, tasks A2/A3/A5).

pub mod direction;
pub mod frame;
pub mod message;
pub mod peer_signal;

mod wire;

pub use direction::{Direction, Role};
pub use frame::{
    CtrlFrame, DecodeError, EncodeError, HEADER_LEN, MAX_FRAME_BYTES, MAX_PAYLOAD_BYTES, VERSION,
};
pub use message::{
    AbortReason, CodeId, ConnectionId, CtrlMessage, DeviceId, Digest32, EndpointBlob, ErrorScope,
    InviteState, InviteSummary, Mac32, Nonce32, PairProof, PairingId, PairingState, RedeemRequest,
    RegistryId, RequestId, SessionId, Signature64, StateFilter, Transcript256, CURSOR_END,
    CURSOR_START, ENDPOINT_BLOB_MAX_BYTES, EXT_ENDPOINT_FLAG, EXT_FLAGS_KNOWN,
    EXT_MAX_REDEEMS_FLAG,
};
pub use peer_signal::PeerSignal;

/// Most candidates one Register / Signal::Candidates message may carry.
pub const MAX_CANDIDATES: usize = 16;
/// Byte cap for the Hello device-name string field.
pub const MAX_NAME_BYTES: usize = 64;
/// Byte cap for the ServerError message string field.
pub const MAX_ERROR_BYTES: usize = 128;
/// Most InviteSummary entries one InviteListPage may carry.
pub const MAX_INVITE_ENTRIES: usize = 32;
/// InviteList page-size lower bound (contract: `limit` is 1..=32).
pub const MIN_INVITE_LIMIT: u8 = 1;
/// InviteList page-size upper bound.
pub const MAX_INVITE_LIMIT: u8 = 32;

// Fixed business error codes (plan A.6, plus the wave-28 addition) for
// convenient access at the module root; also available as
// `protocol_v2::message::*`.
pub use message::{
    CODE_CONFLICT, PAIR_UNAVAILABLE, PROTOCOL_UNSUPPORTED, RATE_LIMITED, REDEEM_EXHAUSTED,
    REQUEST_CONFLICT, RESOURCE_LIMIT, REVISION_CONFLICT, STORAGE_UNAVAILABLE,
};
