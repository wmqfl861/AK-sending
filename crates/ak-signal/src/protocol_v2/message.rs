//! v2 `CtrlMessage`: the closed control-plane message set, tags
//! `0x01..=0x1E`, every field bounded and laid out byte-exactly as frozen
//! in the contract (`docs/contracts/protocol-v2.md` == plan A.4 tables).
//!
//! Layout rules: big-endian integers; fixed arrays without length prefix;
//! payload budgets exclude the 4-byte frame header; undefined tag bytes,
//! undefined closed-enum values, over-budget fields, truncation and
//! trailing bytes are all rejected on decode, and mirrored budgets are
//! enforced on encode ([`super::frame::EncodeError`]).
//!
//! Field-order note for reviewers: each `encode_payload`/`decode_payload`
//! arm below lists fields in the exact wire order of the contract tables;
//! tests lock every per-tag payload budget.

use std::net::SocketAddr;

use super::frame::{DecodeError, EncodeError};
use super::peer_signal;
use super::wire::{self, Cur};
use super::{
    MAX_ERROR_BYTES, MAX_INVITE_ENTRIES, MAX_INVITE_LIMIT, MAX_NAME_BYTES, MIN_INVITE_LIMIT,
};
use crate::util::{put_u16, put_u32, put_u64};

// ---- Type aliases (contract A.4) ----------------------------------------

/// Client-chosen idempotency key for one request/response exchange.
pub type RequestId = [u8; 16];
/// Server-assigned invite identifier (never the plaintext code).
pub type CodeId = [u8; 16];
/// Server-assigned pairing-transaction identifier.
pub type PairingId = [u8; 16];
/// Client-chosen identifier of one connection attempt within a pairing.
pub type ConnectionId = [u8; 16];
/// Server-assigned coordination session identifier.
pub type SessionId = [u8; 16];
/// Device identity on the coordination plane: the Ed25519 public key bytes.
pub type DeviceId = [u8; 32];
/// Authoritative registry service identity (TLS-pinned).
pub type RegistryId = [u8; 32];
/// 32-byte digest (e.g. `lookup_digest`, `transcript_digest`).
pub type Digest32 = [u8; 32];
/// 32-byte nonce (challenges, redeem nonces).
pub type Nonce32 = [u8; 32];
/// 32-byte MAC tag (owner/redeemer proofs, request MACs).
pub type Mac32 = [u8; 32];
/// Ed25519 signature bytes.
pub type Signature64 = [u8; 64];
/// Opaque 256-byte pairing transcript carried by `PairChallenge` /
/// `PairCommit`. Typed construction and verification (the frozen field
/// layout: registry_id, code_id, code_revision, pairing_id, request_id,
/// lookup_digest, both device ids, both nonces, deadline) belongs to
/// ak-identity (`InviteCode`/typed transcript, plan A.7 task A4); the
/// codec only guarantees the fixed size and round-trips it verbatim.
pub type Transcript256 = [u8; 256];

/// Size of one frozen pairing transcript.
pub const TRANSCRIPT_BYTES: usize = 256;
/// Size of one fixed proof: `mac[32] + signature[64]`.
pub const PAIR_PROOF_BYTES: usize = 96;
/// Size of one `InviteSummary` wire entry.
pub const INVITE_SUMMARY_BYTES: usize = 41;
/// Size of the fixed `InviteRedeem` request structure.
pub const REDEEM_REQUEST_BYTES: usize = 184;

// ---- wave-28 endpoint extension (optional, backward compatible) ------------
//
// The v1 minimal pairing model (wave-28) exchanges each side's local
// connection settings alongside the existing claim/redeem/pairing frames.
// The mount is a trailing *extension section* inside the declared payload
// of four extended tags (`InviteClaim` 0x0A, `InviteRedeem` 0x10,
// `InviteRedeemOffered` 0x12, `PairCommit` 0x16): absent section ⇒ the
// payload is byte-identical to the frozen legacy shape, so old peers and
// old shapes decode unchanged. The section is NOT a generic extension
// hole: exactly two fields exist (endpoint blob, claim redeem cap), the
// flag bits outside them are refused, and only the four extended tags may
// carry one.

/// Opaque endpoint blob carried verbatim by the wave-28 extension section:
/// the owner/redeemer connection settings (upper layers suggest JSON with
/// a candidate-address array, device name and direction declaration).
/// The codec only fixes the size bound (1..=[`ENDPOINT_BLOB_MAX_BYTES`]
/// bytes); encoding and validation of the contents belong to the upper
/// layers, and the blob is deliberately NOT covered by the redeem or
/// pairing signatures (connectivity hints, not credentials).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointBlob(pub Vec<u8>);

/// Byte cap of one [`EndpointBlob`] (wave-28).
pub const ENDPOINT_BLOB_MAX_BYTES: usize = 512;
/// Extension flag: endpoint blob present (all four extended tags).
pub const EXT_ENDPOINT_FLAG: u8 = 0x01;
/// Extension flag: `max_redeems:u32` present (`InviteClaim` only).
pub const EXT_MAX_REDEEMS_FLAG: u8 = 0x02;
/// All defined extension flag bits.
pub const EXT_FLAGS_KNOWN: u8 = EXT_ENDPOINT_FLAG | EXT_MAX_REDEEMS_FLAG;
/// Max trailing extension section on the endpoint-only tags:
/// flags(1) + len(2) + [`ENDPOINT_BLOB_MAX_BYTES`].
pub const EXT_SECTION_ENDPOINT_ONLY: usize = 1 + 2 + ENDPOINT_BLOB_MAX_BYTES;
/// Max trailing extension section on `InviteClaim`: additionally
/// `max_redeems:u32`.
pub const EXT_SECTION_CLAIM: usize = EXT_SECTION_ENDPOINT_ONLY + 4;

/// All-zero 16-byte cursor: `InviteList.cursor` sentinel meaning "start at
/// the beginning". Never issued to a real object.
pub const CURSOR_START: CodeId = [0u8; 16];
/// All-zero 16-byte cursor: `InviteListPage.next_cursor` sentinel meaning
/// "no further pages". Never issued to a real object.
pub const CURSOR_END: CodeId = [0u8; 16];

// ---- Per-tag payload budgets (contract A.4 tables, payload only) --------

/// Hello: 2+32+(2+64)+8+32+64.
pub const HELLO_MAX_PAYLOAD: usize = 204;
/// Welcome: 4+4.
pub const WELCOME_PAYLOAD: usize = 8;
/// Register: 1+16*19.
pub const REGISTER_MAX_PAYLOAD: usize = 305;
/// ConnectRequest: 16+32+16+16+16.
pub const CONNECT_REQUEST_PAYLOAD: usize = 96;
/// SessionOpened: 16+32+16+16+16+8.
pub const SESSION_OPENED_PAYLOAD: usize = 104;
/// Signal: 16 + PeerSignal::MAX_ENCODED_BYTES.
pub const SIGNAL_MAX_PAYLOAD: usize = 322;
/// Cancel: 16.
pub const CANCEL_PAYLOAD: usize = 16;
/// ServerError: 2+(2+128).
pub const SERVER_ERROR_MAX_PAYLOAD: usize = 132;
/// AuthChallenge: 32+32+8.
pub const AUTH_CHALLENGE_PAYLOAD: usize = 72;
/// InviteClaim: 16+32+4 plus the optional wave-28 extension section.
pub const INVITE_CLAIM_PAYLOAD: usize = 52 + EXT_SECTION_CLAIM;
/// InviteClaimed: 16+16+8+8+8.
pub const INVITE_CLAIMED_PAYLOAD: usize = 56;
/// InviteList: 16+16+1+1.
pub const INVITE_LIST_PAYLOAD: usize = 34;
/// InviteListPage: 16+16+1+32*41.
pub const INVITE_LIST_PAGE_MAX_PAYLOAD: usize = 1345;
/// InviteRevoke: 16+16+8.
pub const INVITE_REVOKE_PAYLOAD: usize = 40;
/// InviteRevoked: 16+16+8+8.
pub const INVITE_REVOKED_PAYLOAD: usize = 48;
/// InviteRedeem: fixed `RedeemRequest` plus the optional wave-28 section.
pub const INVITE_REDEEM_PAYLOAD: usize = REDEEM_REQUEST_BYTES + EXT_SECTION_ENDPOINT_ONLY;
/// InviteRedeemPending: 16+16+8.
pub const INVITE_REDEEM_PENDING_PAYLOAD: usize = 40;
/// InviteRedeemOffered: 16+8+16+32+184 plus the optional wave-28 section.
pub const INVITE_REDEEM_OFFERED_PAYLOAD: usize = 256 + EXT_SECTION_ENDPOINT_ONLY;
/// PairChallenge: 256+96.
pub const PAIR_CHALLENGE_PAYLOAD: usize = TRANSCRIPT_BYTES + PAIR_PROOF_BYTES;
/// PairProof: 16+32+96.
pub const PAIR_PROOF_PAYLOAD: usize = 144;
/// PairPrepared: 16+32+64.
pub const PAIR_PREPARED_PAYLOAD: usize = 112;
/// PairCommit: 256+96+96+64+64+8 plus the optional wave-28 section.
pub const PAIR_COMMIT_PAYLOAD: usize = 584 + EXT_SECTION_ENDPOINT_ONLY;
/// PairApplied: 16+32+8+64.
pub const PAIR_APPLIED_PAYLOAD: usize = 120;
/// PairReady: 16+32+8+64+64.
pub const PAIR_READY_PAYLOAD: usize = 184;
/// PairStatusQuery: 16+16.
pub const PAIR_STATUS_QUERY_PAYLOAD: usize = 32;
/// PairStatusReply: 16+16+1+8+8.
pub const PAIR_STATUS_REPLY_PAYLOAD: usize = 49;
/// PairAbort: 16+16+2.
pub const PAIR_ABORT_PAYLOAD: usize = 34;
/// PairError: 16+1+16+2+4.
pub const PAIR_ERROR_PAYLOAD: usize = 39;
/// Ping: 16.
pub const PING_PAYLOAD: usize = 16;
/// Pong: 16.
pub const PONG_PAYLOAD: usize = 16;

// ---- Fixed business error codes (plan A.6) ------------------------------
// Carried in `PairError.code` / `ServerError.code`; kept as constants so
// the numeric assignments stay greppable. Not a closed enum on the wire:
// the contract freezes the values, unknown codes are a policy matter.

/// Redemption target missing, wrong, expired, revoked, owner offline or
/// MAC-invalid; unified so existence is not leaked.
pub const PAIR_UNAVAILABLE: u16 = 0x2101;
/// Rate-limit rejection (per device / per address budget).
pub const RATE_LIMITED: u16 = 0x2102;
/// Resource budget exhausted (pending redeem, active invites, storage).
pub const RESOURCE_LIMIT: u16 = 0x2103;
/// Same request id replayed with different content.
pub const REQUEST_CONFLICT: u16 = 0x2104;
/// High-entropy code registration collision (never a public lookup).
pub const CODE_CONFLICT: u16 = 0x2105;
/// Optimistic-concurrency revision mismatch.
pub const REVISION_CONFLICT: u16 = 0x2106;
/// Peer lacks the capability this flow requires.
pub const PROTOCOL_UNSUPPORTED: u16 = 0x2107;
/// Authoritative registry storage unavailable.
pub const STORAGE_UNAVAILABLE: u16 = 0x2108;
/// Wave-28 single-redemption cap reached: the code row's `max_redeems`
/// budget of committed pairings is exhausted (0 = unlimited, the legacy
/// default). Returned immediately (no deferred deadline semantics).
pub const REDEEM_EXHAUSTED: u16 = 0x2109;

// ---- Closed enums --------------------------------------------------------
// The contract freezes `scope`/`state`/`reason` as closed enums without
// enumerating numeric values; the assignments below are the A1 codec
// choices (listed in the A.3/A.4 field order) and tests lock them. Adding
// a value is a contract change, not a local edit.

/// Lifecycle state of one invite code (A.3 `state active | revoked |
/// expired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InviteState {
    /// Claimed and redeemable.
    Active = 0,
    /// Owner-revoked; tombstoned, never reused.
    Revoked = 1,
    /// TTL elapsed; tombstoned, never reused.
    Expired = 2,
}

impl InviteState {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(InviteState::Active),
            1 => Some(InviteState::Revoked),
            2 => Some(InviteState::Expired),
            _ => None,
        }
    }
}

/// `InviteList.state_filter`: a bitmask over [`InviteState`]. Only the
/// three defined bits are legal; bits 3..=7 are undefined and rejected.
/// The empty filter (select nothing) is representable but useless — the
/// contract only requires undefined bits to be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateFilter(u8);

impl StateFilter {
    /// Select active codes.
    pub const ACTIVE: Self = StateFilter(0x01);
    /// Select revoked codes.
    pub const REVOKED: Self = StateFilter(0x02);
    /// Select expired codes.
    pub const EXPIRED: Self = StateFilter(0x04);
    /// Select every defined state.
    pub const ALL: Self = StateFilter(0x07);

    /// Defined bits only; `None` for any byte with undefined bits set.
    pub fn from_bits(bits: u8) -> Option<Self> {
        if bits & !0x07 == 0 {
            Some(StateFilter(bits))
        } else {
            None
        }
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, state: InviteState) -> bool {
        let bit = match state {
            InviteState::Active => 0x01,
            InviteState::Revoked => 0x02,
            InviteState::Expired => 0x04,
        };
        self.0 & bit != 0
    }
}

/// `PairStatusReply.state`: server-side pairing-transaction state machine
/// (A.3 Prepared/Commit/Applied plus the pending and terminal ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PairingState {
    /// Redeem accepted, waiting for owner/proofs.
    Pending = 0,
    /// Both `PairPrepared` seen, commit not yet durable.
    Prepared = 1,
    /// Server persisted the commit (authoritative point).
    Committed = 2,
    /// At least one side confirmed local book application.
    Applied = 3,
    /// Both sides applied; pairing relation active.
    Ready = 4,
    /// Aborted or failed before commit.
    Aborted = 5,
}

impl PairingState {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(PairingState::Pending),
            1 => Some(PairingState::Prepared),
            2 => Some(PairingState::Committed),
            3 => Some(PairingState::Applied),
            4 => Some(PairingState::Ready),
            5 => Some(PairingState::Aborted),
            _ => None,
        }
    }
}

/// `PairError.scope`: which kind of 16-byte object `object_id` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorScope {
    /// `object_id` is the all-zero sentinel, no object referenced.
    None = 0,
    /// `object_id` is a [`RequestId`].
    Request = 1,
    /// `object_id` is a [`CodeId`].
    Code = 2,
    /// `object_id` is a [`PairingId`].
    Pairing = 3,
    /// `object_id` is a [`ConnectionId`].
    Connection = 4,
    /// `object_id` is a [`SessionId`].
    Session = 5,
}

impl ErrorScope {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ErrorScope::None),
            1 => Some(ErrorScope::Request),
            2 => Some(ErrorScope::Code),
            3 => Some(ErrorScope::Pairing),
            4 => Some(ErrorScope::Connection),
            5 => Some(ErrorScope::Session),
            _ => None,
        }
    }
}

/// `PairAbort.reason`: why a pairing transaction was aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AbortReason {
    /// No more specific reason recorded.
    Unspecified = 0,
    /// Local user cancelled the pairing.
    User = 1,
    /// Deadline elapsed (pending redeem or uncommitted transaction).
    Expired = 2,
    /// Protocol violation / proof verification failed.
    Protocol = 3,
    /// Owner offline past the redeem deadline.
    OwnerOffline = 4,
    /// Invite revoked before commit.
    Revoked = 5,
}

impl AbortReason {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(AbortReason::Unspecified),
            1 => Some(AbortReason::User),
            2 => Some(AbortReason::Expired),
            3 => Some(AbortReason::Protocol),
            4 => Some(AbortReason::OwnerOffline),
            5 => Some(AbortReason::Revoked),
            _ => None,
        }
    }
}

// ---- Fixed embedded structures -------------------------------------------

/// One `InviteSummary` wire entry: `code_id[16] | state:u8 |
/// created_ms:u64 | expires_ms:u64 | revision:u64` (41 bytes).
/// `expires_ms == 0` means long-lived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteSummary {
    pub code_id: CodeId,
    pub state: InviteState,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub revision: u64,
}

/// `PairProofData = mac[32] + signature[64]` (96 bytes). The MAC and
/// signature cover the role domain and the full canonical transcript;
/// constructing/verifying them is ak-identity's job, the codec only fixes
/// the wire shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairProof {
    pub mac: Mac32,
    pub signature: Signature64,
}

/// The fixed fields of `InviteRedeem` (184 bytes), embedded verbatim in
/// `InviteRedeemOffered` (`redeem`). Implemented as a fixed structure, not
/// a byte array or generic nested message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemRequest {
    pub request_id: RequestId,
    pub lookup_digest: Digest32,
    pub redeemer_nonce: Nonce32,
    pub deadline_ms: u64,
    pub request_mac: Mac32,
    pub signature: Signature64,
}

// ---- The closed message set ----------------------------------------------

// Wire type values (contract A.4). The set is closed: decode rejects every
// byte outside 0x01..=0x1E and tests enumerate all 256 values.
const TYPE_HELLO: u8 = 0x01;
const TYPE_WELCOME: u8 = 0x02;
const TYPE_REGISTER: u8 = 0x03;
const TYPE_CONNECT_REQUEST: u8 = 0x04;
const TYPE_SESSION_OPENED: u8 = 0x05;
const TYPE_SIGNAL: u8 = 0x06;
const TYPE_CANCEL: u8 = 0x07;
const TYPE_SERVER_ERROR: u8 = 0x08;
const TYPE_AUTH_CHALLENGE: u8 = 0x09;
const TYPE_INVITE_CLAIM: u8 = 0x0A;
const TYPE_INVITE_CLAIMED: u8 = 0x0B;
const TYPE_INVITE_LIST: u8 = 0x0C;
const TYPE_INVITE_LIST_PAGE: u8 = 0x0D;
const TYPE_INVITE_REVOKE: u8 = 0x0E;
const TYPE_INVITE_REVOKED: u8 = 0x0F;
const TYPE_INVITE_REDEEM: u8 = 0x10;
const TYPE_INVITE_REDEEM_PENDING: u8 = 0x11;
const TYPE_INVITE_REDEEM_OFFERED: u8 = 0x12;
const TYPE_PAIR_CHALLENGE: u8 = 0x13;
const TYPE_PAIR_PROOF: u8 = 0x14;
const TYPE_PAIR_PREPARED: u8 = 0x15;
const TYPE_PAIR_COMMIT: u8 = 0x16;
const TYPE_PAIR_APPLIED: u8 = 0x17;
const TYPE_PAIR_READY: u8 = 0x18;
const TYPE_PAIR_STATUS_QUERY: u8 = 0x19;
const TYPE_PAIR_STATUS_REPLY: u8 = 0x1A;
const TYPE_PAIR_ABORT: u8 = 0x1B;
const TYPE_PAIR_ERROR: u8 = 0x1C;
const TYPE_PING: u8 = 0x1D;
const TYPE_PONG: u8 = 0x1E;

/// First and last defined type byte; everything outside is `UnknownType`.
pub(super) const MIN_TYPE: u8 = TYPE_HELLO;
pub(super) const MAX_TYPE: u8 = TYPE_PONG;

pub(super) fn is_known_type(ty: u8) -> bool {
    (MIN_TYPE..=MAX_TYPE).contains(&ty)
}

/// True for the four wave-28 extended tags whose payload may carry the
/// trailing endpoint/claim extension section. Every other tag keeps the
/// strict frozen shape: any trailing byte is `DecodeError::Trailing`.
pub(super) fn is_extended_type(ty: u8) -> bool {
    matches!(
        ty,
        TYPE_INVITE_CLAIM | TYPE_INVITE_REDEEM | TYPE_INVITE_REDEEM_OFFERED | TYPE_PAIR_COMMIT
    )
}

/// The closed v2 client<->server message set (one frame payload each; the
/// type-byte mapping and every direction are locked by tests).
///
/// The enum is deliberately unboxed: the largest variant (`PairCommit`)
/// is 584 bytes by wire contract and every frame stays under the 4096-byte
/// cap; frames are short-lived and never stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtrlMessage {
    /// First message on a connection. The signature covers
    /// `"AKHELLO2" || registry_id || TLS exporter || canonical fields
    /// (without signature)`; `challenge` echoes the connection's
    /// `AuthChallenge` nonce.
    Hello {
        version: u16,
        device_id: DeviceId,
        name: String,
        timestamp_ms: u64,
        challenge: Nonce32,
        signature: Signature64,
    },
    Welcome {
        session_ttl_secs: u32,
        capabilities: u32,
    },
    /// Replace this device's published candidates (count:u8 + up to 16).
    Register {
        candidates: Vec<SocketAddr>,
    },
    /// Ask the server to open a session toward `target`, keyed by the
    /// verified `(pairing_id, code_id, connection_id)` triple.
    ConnectRequest {
        request_id: RequestId,
        target: DeviceId,
        pairing_id: PairingId,
        code_id: CodeId,
        connection_id: ConnectionId,
    },
    /// Sent to both endpoints when a session exists.
    SessionOpened {
        session: SessionId,
        peer: DeviceId,
        pairing_id: PairingId,
        code_id: CodeId,
        connection_id: ConnectionId,
        expires_ms: u64,
    },
    /// Relay one bounded peer signal within a session.
    Signal {
        session: SessionId,
        signal: peer_signal::PeerSignal,
    },
    Cancel {
        session: SessionId,
    },
    /// Legacy bounded server error; invite business errors use `PairError`.
    ServerError {
        code: u16,
        message: String,
    },
    /// Server's one-shot challenge that binds this control connection;
    /// valid once, at most 10 seconds (TLS layer policy).
    AuthChallenge {
        registry_id: RegistryId,
        nonce: Nonce32,
        expires_ms: u64,
    },

    // ---- invite management ----
    /// Register a freshly generated (client-side) invite code by its
    /// lookup digest; the plaintext secret never leaves the endpoints.
    InviteClaim {
        request_id: RequestId,
        lookup_digest: Digest32,
        ttl_secs: u32,
    },
    InviteClaimed {
        request_id: RequestId,
        code_id: CodeId,
        created_ms: u64,
        /// 0 = long-lived.
        expires_ms: u64,
        revision: u64,
    },
    /// Page through this device's own invites. `cursor` all-zero = start;
    /// `limit` 1..=32; only defined state-filter bits.
    InviteList {
        request_id: RequestId,
        cursor: CodeId,
        limit: u8,
        state_filter: StateFilter,
    },
    InviteListPage {
        request_id: RequestId,
        /// All-zero = no further pages.
        next_cursor: CodeId,
        entries: Vec<InviteSummary>,
    },
    InviteRevoke {
        request_id: RequestId,
        code_id: CodeId,
        expected_revision: u64,
    },
    InviteRevoked {
        request_id: RequestId,
        code_id: CodeId,
        revision: u64,
        revoked_ms: u64,
    },

    // ---- redeem and pairing transcript ----
    /// Redeem attempt with the fixed request structure (MAC + signature
    /// over the issuer/lookup/key/request/nonce/deadline domain).
    InviteRedeem(RedeemRequest),
    /// Same shape whether or not the code exists (no existence leak).
    InviteRedeemPending {
        request_id: RequestId,
        pairing_id: PairingId,
        deadline_ms: u64,
    },
    /// The redeem offer forwarded to the owner, embedding the redeemer's
    /// fixed request structure verbatim.
    InviteRedeemOffered {
        code_id: CodeId,
        code_revision: u64,
        pairing_id: PairingId,
        redeemer_device_id: DeviceId,
        redeem: RedeemRequest,
    },
    /// Frozen 256-byte transcript plus the owner's 96-byte proof.
    PairChallenge {
        transcript: Transcript256,
        owner_proof: PairProof,
    },
    /// Redeer's answer: transcript digest plus its 96-byte proof.
    PairProof {
        pairing_id: PairingId,
        transcript_digest: Digest32,
        redeemer_proof: PairProof,
    },
    /// One side's durable prepared journal record, signed.
    PairPrepared {
        pairing_id: PairingId,
        transcript_digest: Digest32,
        signature: Signature64,
    },
    /// Authoritative server commit; re-sent on status queries after
    /// Commit/Ready for crash recovery.
    PairCommit {
        transcript: Transcript256,
        owner_proof: PairProof,
        redeemer_proof: PairProof,
        owner_prepared_signature: Signature64,
        redeemer_prepared_signature: Signature64,
        commit_revision: u64,
    },
    /// One side's confirmation that its local trust book was applied.
    PairApplied {
        pairing_id: PairingId,
        transcript_digest: Digest32,
        commit_revision: u64,
        signature: Signature64,
    },
    /// Both applied signatures; activates the pairing relation.
    PairReady {
        pairing_id: PairingId,
        transcript_digest: Digest32,
        commit_revision: u64,
        owner_applied_signature: Signature64,
        redeemer_applied_signature: Signature64,
    },
    /// Participants-only status query (client asks, server answers).
    PairStatusQuery {
        request_id: RequestId,
        pairing_id: PairingId,
    },
    PairStatusReply {
        request_id: RequestId,
        pairing_id: PairingId,
        state: PairingState,
        commit_revision: u64,
        deadline_ms: u64,
    },
    PairAbort {
        request_id: RequestId,
        pairing_id: PairingId,
        reason: AbortReason,
    },
    /// Fixed invite business error; never echoes user input.
    PairError {
        request_id: RequestId,
        scope: ErrorScope,
        object_id: [u8; 16],
        code: u16,
        retry_after_ms: u32,
    },

    // ---- liveness ----
    /// Keepalive: client pings, server answers with `Pong` (frozen
    /// directions; the server never pings first).
    Ping {
        nonce: [u8; 16],
    },
    Pong {
        nonce: [u8; 16],
    },
}

impl CtrlMessage {
    /// Wire type byte; the mapping is locked by tests.
    pub fn type_byte(&self) -> u8 {
        match self {
            CtrlMessage::Hello { .. } => TYPE_HELLO,
            CtrlMessage::Welcome { .. } => TYPE_WELCOME,
            CtrlMessage::Register { .. } => TYPE_REGISTER,
            CtrlMessage::ConnectRequest { .. } => TYPE_CONNECT_REQUEST,
            CtrlMessage::SessionOpened { .. } => TYPE_SESSION_OPENED,
            CtrlMessage::Signal { .. } => TYPE_SIGNAL,
            CtrlMessage::Cancel { .. } => TYPE_CANCEL,
            CtrlMessage::ServerError { .. } => TYPE_SERVER_ERROR,
            CtrlMessage::AuthChallenge { .. } => TYPE_AUTH_CHALLENGE,
            CtrlMessage::InviteClaim { .. } => TYPE_INVITE_CLAIM,
            CtrlMessage::InviteClaimed { .. } => TYPE_INVITE_CLAIMED,
            CtrlMessage::InviteList { .. } => TYPE_INVITE_LIST,
            CtrlMessage::InviteListPage { .. } => TYPE_INVITE_LIST_PAGE,
            CtrlMessage::InviteRevoke { .. } => TYPE_INVITE_REVOKE,
            CtrlMessage::InviteRevoked { .. } => TYPE_INVITE_REVOKED,
            CtrlMessage::InviteRedeem(_) => TYPE_INVITE_REDEEM,
            CtrlMessage::InviteRedeemPending { .. } => TYPE_INVITE_REDEEM_PENDING,
            CtrlMessage::InviteRedeemOffered { .. } => TYPE_INVITE_REDEEM_OFFERED,
            CtrlMessage::PairChallenge { .. } => TYPE_PAIR_CHALLENGE,
            CtrlMessage::PairProof { .. } => TYPE_PAIR_PROOF,
            CtrlMessage::PairPrepared { .. } => TYPE_PAIR_PREPARED,
            CtrlMessage::PairCommit { .. } => TYPE_PAIR_COMMIT,
            CtrlMessage::PairApplied { .. } => TYPE_PAIR_APPLIED,
            CtrlMessage::PairReady { .. } => TYPE_PAIR_READY,
            CtrlMessage::PairStatusQuery { .. } => TYPE_PAIR_STATUS_QUERY,
            CtrlMessage::PairStatusReply { .. } => TYPE_PAIR_STATUS_REPLY,
            CtrlMessage::PairAbort { .. } => TYPE_PAIR_ABORT,
            CtrlMessage::PairError { .. } => TYPE_PAIR_ERROR,
            CtrlMessage::Ping { .. } => TYPE_PING,
            CtrlMessage::Pong { .. } => TYPE_PONG,
        }
    }
}

/// Encode one message payload. `endpoint` / `max_redeems` carry the
/// wave-28 extension fields: both `None` writes the byte-identical legacy
/// shape; any combination is only legal on the extended tags
/// ([`is_extended_type`]), and `max_redeems` additionally only on
/// `InviteClaim`. Mirrored budgets are validated before anything is
/// written, exactly like every frozen field.
pub(super) fn encode_payload(
    message: &CtrlMessage,
    endpoint: Option<&EndpointBlob>,
    max_redeems: Option<u32>,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    let ty = message.type_byte();
    let want_endpoint = endpoint.is_some();
    let want_cap = max_redeems.is_some();
    if (want_endpoint || want_cap) && !is_extended_type(ty) {
        return Err(EncodeError::BadExtension { ty });
    }
    if want_cap && ty != TYPE_INVITE_CLAIM {
        return Err(EncodeError::BadExtension { ty });
    }
    match message {
        CtrlMessage::Hello {
            version,
            device_id,
            name,
            timestamp_ms,
            challenge,
            signature,
        } => {
            put_u16(out, *version);
            out.extend_from_slice(device_id);
            wire::check_str(name, MAX_NAME_BYTES)?;
            wire::put_str(out, name);
            put_u64(out, *timestamp_ms);
            out.extend_from_slice(challenge);
            out.extend_from_slice(signature);
        }
        CtrlMessage::Welcome {
            session_ttl_secs,
            capabilities,
        } => {
            put_u32(out, *session_ttl_secs);
            put_u32(out, *capabilities);
        }
        CtrlMessage::Register { candidates } => {
            wire::put_candidates_checked(out, candidates)?;
        }
        CtrlMessage::ConnectRequest {
            request_id,
            target,
            pairing_id,
            code_id,
            connection_id,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(target);
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(code_id);
            out.extend_from_slice(connection_id);
        }
        CtrlMessage::SessionOpened {
            session,
            peer,
            pairing_id,
            code_id,
            connection_id,
            expires_ms,
        } => {
            out.extend_from_slice(session);
            out.extend_from_slice(peer);
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(code_id);
            out.extend_from_slice(connection_id);
            put_u64(out, *expires_ms);
        }
        CtrlMessage::Signal { session, signal } => {
            out.extend_from_slice(session);
            peer_signal::encode(signal, out)?;
        }
        CtrlMessage::Cancel { session } => out.extend_from_slice(session),
        CtrlMessage::ServerError { code, message } => {
            put_u16(out, *code);
            wire::check_str(message, MAX_ERROR_BYTES)?;
            wire::put_str(out, message);
        }
        CtrlMessage::AuthChallenge {
            registry_id,
            nonce,
            expires_ms,
        } => {
            out.extend_from_slice(registry_id);
            out.extend_from_slice(nonce);
            put_u64(out, *expires_ms);
        }
        CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(lookup_digest);
            put_u32(out, *ttl_secs);
        }
        CtrlMessage::InviteClaimed {
            request_id,
            code_id,
            created_ms,
            expires_ms,
            revision,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(code_id);
            put_u64(out, *created_ms);
            put_u64(out, *expires_ms);
            put_u64(out, *revision);
        }
        CtrlMessage::InviteList {
            request_id,
            cursor,
            limit,
            state_filter,
        } => {
            if !(MIN_INVITE_LIMIT..=MAX_INVITE_LIMIT).contains(limit) {
                return Err(EncodeError::BadInviteLimit(*limit));
            }
            out.extend_from_slice(request_id);
            out.extend_from_slice(cursor);
            out.push(*limit);
            out.push(state_filter.bits());
        }
        CtrlMessage::InviteListPage {
            request_id,
            next_cursor,
            entries,
        } => {
            if entries.len() > MAX_INVITE_ENTRIES {
                return Err(EncodeError::TooManyEntries {
                    count: entries.len(),
                    limit: MAX_INVITE_ENTRIES,
                });
            }
            out.extend_from_slice(request_id);
            out.extend_from_slice(next_cursor);
            out.push(entries.len() as u8);
            for entry in entries {
                put_summary(out, entry);
            }
        }
        CtrlMessage::InviteRevoke {
            request_id,
            code_id,
            expected_revision,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(code_id);
            put_u64(out, *expected_revision);
        }
        CtrlMessage::InviteRevoked {
            request_id,
            code_id,
            revision,
            revoked_ms,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(code_id);
            put_u64(out, *revision);
            put_u64(out, *revoked_ms);
        }
        CtrlMessage::InviteRedeem(redeem) => put_redeem(out, redeem),
        CtrlMessage::InviteRedeemPending {
            request_id,
            pairing_id,
            deadline_ms,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(pairing_id);
            put_u64(out, *deadline_ms);
        }
        CtrlMessage::InviteRedeemOffered {
            code_id,
            code_revision,
            pairing_id,
            redeemer_device_id,
            redeem,
        } => {
            out.extend_from_slice(code_id);
            put_u64(out, *code_revision);
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(redeemer_device_id);
            put_redeem(out, redeem);
        }
        CtrlMessage::PairChallenge {
            transcript,
            owner_proof,
        } => {
            out.extend_from_slice(transcript);
            put_proof(out, owner_proof);
        }
        CtrlMessage::PairProof {
            pairing_id,
            transcript_digest,
            redeemer_proof,
        } => {
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(transcript_digest);
            put_proof(out, redeemer_proof);
        }
        CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest,
            signature,
        } => {
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(transcript_digest);
            out.extend_from_slice(signature);
        }
        CtrlMessage::PairCommit {
            transcript,
            owner_proof,
            redeemer_proof,
            owner_prepared_signature,
            redeemer_prepared_signature,
            commit_revision,
        } => {
            out.extend_from_slice(transcript);
            put_proof(out, owner_proof);
            put_proof(out, redeemer_proof);
            out.extend_from_slice(owner_prepared_signature);
            out.extend_from_slice(redeemer_prepared_signature);
            put_u64(out, *commit_revision);
        }
        CtrlMessage::PairApplied {
            pairing_id,
            transcript_digest,
            commit_revision,
            signature,
        } => {
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(transcript_digest);
            put_u64(out, *commit_revision);
            out.extend_from_slice(signature);
        }
        CtrlMessage::PairReady {
            pairing_id,
            transcript_digest,
            commit_revision,
            owner_applied_signature,
            redeemer_applied_signature,
        } => {
            out.extend_from_slice(pairing_id);
            out.extend_from_slice(transcript_digest);
            put_u64(out, *commit_revision);
            out.extend_from_slice(owner_applied_signature);
            out.extend_from_slice(redeemer_applied_signature);
        }
        CtrlMessage::PairStatusQuery {
            request_id,
            pairing_id,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(pairing_id);
        }
        CtrlMessage::PairStatusReply {
            request_id,
            pairing_id,
            state,
            commit_revision,
            deadline_ms,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(pairing_id);
            out.push(state.as_u8());
            put_u64(out, *commit_revision);
            put_u64(out, *deadline_ms);
        }
        CtrlMessage::PairAbort {
            request_id,
            pairing_id,
            reason,
        } => {
            out.extend_from_slice(request_id);
            out.extend_from_slice(pairing_id);
            put_u16(out, u16::from(reason.as_u8()));
        }
        CtrlMessage::PairError {
            request_id,
            scope,
            object_id,
            code,
            retry_after_ms,
        } => {
            out.extend_from_slice(request_id);
            out.push(scope.as_u8());
            out.extend_from_slice(object_id);
            put_u16(out, *code);
            put_u32(out, *retry_after_ms);
        }
        CtrlMessage::Ping { nonce } => out.extend_from_slice(nonce),
        CtrlMessage::Pong { nonce } => out.extend_from_slice(nonce),
    }
    // wave-28 trailing extension section (absent ⇒ legacy bytes).
    let mut flags = 0u8;
    if want_endpoint {
        flags |= EXT_ENDPOINT_FLAG;
    }
    if want_cap {
        flags |= EXT_MAX_REDEEMS_FLAG;
    }
    if flags != 0 {
        out.push(flags);
        if let Some(blob) = endpoint {
            if blob.0.is_empty() || blob.0.len() > ENDPOINT_BLOB_MAX_BYTES {
                return Err(EncodeError::EndpointBlobSize {
                    len: blob.0.len(),
                    limit: ENDPOINT_BLOB_MAX_BYTES,
                });
            }
            put_u16(out, blob.0.len() as u16);
            out.extend_from_slice(&blob.0);
        }
        if let Some(cap) = max_redeems {
            put_u32(out, cap);
        }
    }
    Ok(())
}

fn put_summary(out: &mut Vec<u8>, entry: &InviteSummary) {
    out.extend_from_slice(&entry.code_id);
    out.push(entry.state.as_u8());
    put_u64(out, entry.created_ms);
    put_u64(out, entry.expires_ms);
    put_u64(out, entry.revision);
}

fn put_proof(out: &mut Vec<u8>, proof: &PairProof) {
    out.extend_from_slice(&proof.mac);
    out.extend_from_slice(&proof.signature);
}

fn put_redeem(out: &mut Vec<u8>, redeem: &RedeemRequest) {
    out.extend_from_slice(&redeem.request_id);
    out.extend_from_slice(&redeem.lookup_digest);
    out.extend_from_slice(&redeem.redeemer_nonce);
    put_u64(out, redeem.deadline_ms);
    out.extend_from_slice(&redeem.request_mac);
    out.extend_from_slice(&redeem.signature);
}

/// Decode one message payload. Returns the message plus the decoded
/// wave-28 extension fields (`None, None` for the legacy shape). The
/// extension section is validated before any blob is allocated:
/// undefined flag bits, the `max_redeems` flag off `InviteClaim`,
/// out-of-budget blob lengths and leftover bytes are all refused.
pub(super) fn decode_payload(
    ty: u8,
    payload: &[u8],
) -> Result<(CtrlMessage, Option<EndpointBlob>, Option<u32>), DecodeError> {
    let mut cur = Cur::new(payload);
    let message = match ty {
        TYPE_HELLO => CtrlMessage::Hello {
            version: cur.u16()?,
            device_id: cur.fixed::<32>()?,
            name: wire::get_str(&mut cur, MAX_NAME_BYTES)?,
            timestamp_ms: cur.u64()?,
            challenge: cur.fixed::<32>()?,
            signature: cur.fixed::<64>()?,
        },
        TYPE_WELCOME => CtrlMessage::Welcome {
            session_ttl_secs: cur.u32()?,
            capabilities: cur.u32()?,
        },
        TYPE_REGISTER => CtrlMessage::Register {
            candidates: wire::get_candidates(&mut cur)?,
        },
        TYPE_CONNECT_REQUEST => CtrlMessage::ConnectRequest {
            request_id: cur.fixed::<16>()?,
            target: cur.fixed::<32>()?,
            pairing_id: cur.fixed::<16>()?,
            code_id: cur.fixed::<16>()?,
            connection_id: cur.fixed::<16>()?,
        },
        TYPE_SESSION_OPENED => CtrlMessage::SessionOpened {
            session: cur.fixed::<16>()?,
            peer: cur.fixed::<32>()?,
            pairing_id: cur.fixed::<16>()?,
            code_id: cur.fixed::<16>()?,
            connection_id: cur.fixed::<16>()?,
            expires_ms: cur.u64()?,
        },
        TYPE_SIGNAL => CtrlMessage::Signal {
            session: cur.fixed::<16>()?,
            signal: peer_signal::decode(&mut cur)?,
        },
        TYPE_CANCEL => CtrlMessage::Cancel {
            session: cur.fixed::<16>()?,
        },
        TYPE_SERVER_ERROR => CtrlMessage::ServerError {
            code: cur.u16()?,
            message: wire::get_str(&mut cur, MAX_ERROR_BYTES)?,
        },
        TYPE_AUTH_CHALLENGE => CtrlMessage::AuthChallenge {
            registry_id: cur.fixed::<32>()?,
            nonce: cur.fixed::<32>()?,
            expires_ms: cur.u64()?,
        },
        TYPE_INVITE_CLAIM => CtrlMessage::InviteClaim {
            request_id: cur.fixed::<16>()?,
            lookup_digest: cur.fixed::<32>()?,
            ttl_secs: cur.u32()?,
        },
        TYPE_INVITE_CLAIMED => CtrlMessage::InviteClaimed {
            request_id: cur.fixed::<16>()?,
            code_id: cur.fixed::<16>()?,
            created_ms: cur.u64()?,
            expires_ms: cur.u64()?,
            revision: cur.u64()?,
        },
        TYPE_INVITE_LIST => {
            let request_id = cur.fixed::<16>()?;
            let cursor = cur.fixed::<16>()?;
            let limit = cur.u8()?;
            if !(MIN_INVITE_LIMIT..=MAX_INVITE_LIMIT).contains(&limit) {
                return Err(DecodeError::BadInviteLimit(limit));
            }
            let bits = cur.u8()?;
            CtrlMessage::InviteList {
                request_id,
                cursor,
                limit,
                state_filter: StateFilter::from_bits(bits)
                    .ok_or(DecodeError::BadStateFilter(bits))?,
            }
        }
        TYPE_INVITE_LIST_PAGE => {
            let request_id = cur.fixed::<16>()?;
            let next_cursor = cur.fixed::<16>()?;
            let count = cur.u8()? as usize;
            if count > MAX_INVITE_ENTRIES {
                return Err(DecodeError::TooManyEntries {
                    count,
                    limit: MAX_INVITE_ENTRIES,
                });
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(get_summary(&mut cur)?);
            }
            CtrlMessage::InviteListPage {
                request_id,
                next_cursor,
                entries,
            }
        }
        TYPE_INVITE_REVOKE => CtrlMessage::InviteRevoke {
            request_id: cur.fixed::<16>()?,
            code_id: cur.fixed::<16>()?,
            expected_revision: cur.u64()?,
        },
        TYPE_INVITE_REVOKED => CtrlMessage::InviteRevoked {
            request_id: cur.fixed::<16>()?,
            code_id: cur.fixed::<16>()?,
            revision: cur.u64()?,
            revoked_ms: cur.u64()?,
        },
        TYPE_INVITE_REDEEM => CtrlMessage::InviteRedeem(get_redeem(&mut cur)?),
        TYPE_INVITE_REDEEM_PENDING => CtrlMessage::InviteRedeemPending {
            request_id: cur.fixed::<16>()?,
            pairing_id: cur.fixed::<16>()?,
            deadline_ms: cur.u64()?,
        },
        TYPE_INVITE_REDEEM_OFFERED => CtrlMessage::InviteRedeemOffered {
            code_id: cur.fixed::<16>()?,
            code_revision: cur.u64()?,
            pairing_id: cur.fixed::<16>()?,
            redeemer_device_id: cur.fixed::<32>()?,
            redeem: get_redeem(&mut cur)?,
        },
        TYPE_PAIR_CHALLENGE => CtrlMessage::PairChallenge {
            transcript: cur.fixed::<TRANSCRIPT_BYTES>()?,
            owner_proof: get_proof(&mut cur)?,
        },
        TYPE_PAIR_PROOF => CtrlMessage::PairProof {
            pairing_id: cur.fixed::<16>()?,
            transcript_digest: cur.fixed::<32>()?,
            redeemer_proof: get_proof(&mut cur)?,
        },
        TYPE_PAIR_PREPARED => CtrlMessage::PairPrepared {
            pairing_id: cur.fixed::<16>()?,
            transcript_digest: cur.fixed::<32>()?,
            signature: cur.fixed::<64>()?,
        },
        TYPE_PAIR_COMMIT => CtrlMessage::PairCommit {
            transcript: cur.fixed::<TRANSCRIPT_BYTES>()?,
            owner_proof: get_proof(&mut cur)?,
            redeemer_proof: get_proof(&mut cur)?,
            owner_prepared_signature: cur.fixed::<64>()?,
            redeemer_prepared_signature: cur.fixed::<64>()?,
            commit_revision: cur.u64()?,
        },
        TYPE_PAIR_APPLIED => CtrlMessage::PairApplied {
            pairing_id: cur.fixed::<16>()?,
            transcript_digest: cur.fixed::<32>()?,
            commit_revision: cur.u64()?,
            signature: cur.fixed::<64>()?,
        },
        TYPE_PAIR_READY => CtrlMessage::PairReady {
            pairing_id: cur.fixed::<16>()?,
            transcript_digest: cur.fixed::<32>()?,
            commit_revision: cur.u64()?,
            owner_applied_signature: cur.fixed::<64>()?,
            redeemer_applied_signature: cur.fixed::<64>()?,
        },
        TYPE_PAIR_STATUS_QUERY => CtrlMessage::PairStatusQuery {
            request_id: cur.fixed::<16>()?,
            pairing_id: cur.fixed::<16>()?,
        },
        TYPE_PAIR_STATUS_REPLY => CtrlMessage::PairStatusReply {
            request_id: cur.fixed::<16>()?,
            pairing_id: cur.fixed::<16>()?,
            state: PairingState::from_u8(cur.u8()?)
                .ok_or(DecodeError::Malformed("pairing state"))?,
            commit_revision: cur.u64()?,
            deadline_ms: cur.u64()?,
        },
        TYPE_PAIR_ABORT => CtrlMessage::PairAbort {
            request_id: cur.fixed::<16>()?,
            pairing_id: cur.fixed::<16>()?,
            reason: {
                // reason is u16 on the wire but a closed u8-backed enum:
                // refuse everything above 0x00FF instead of truncating.
                let raw = cur.u16()?;
                if raw > u8::MAX as u16 {
                    return Err(DecodeError::Malformed("abort reason"));
                }
                AbortReason::from_u8(raw as u8).ok_or(DecodeError::Malformed("abort reason"))?
            },
        },
        TYPE_PAIR_ERROR => CtrlMessage::PairError {
            request_id: cur.fixed::<16>()?,
            scope: ErrorScope::from_u8(cur.u8()?).ok_or(DecodeError::Malformed("error scope"))?,
            object_id: cur.fixed::<16>()?,
            code: cur.u16()?,
            retry_after_ms: cur.u32()?,
        },
        TYPE_PING => CtrlMessage::Ping {
            nonce: cur.fixed::<16>()?,
        },
        TYPE_PONG => CtrlMessage::Pong {
            nonce: cur.fixed::<16>()?,
        },
        _ => return Err(DecodeError::UnknownType(ty)),
    };
    let trailing = cur.remaining();
    let (endpoint, max_redeems) = if is_extended_type(ty) {
        decode_extension(ty, &mut cur)?
    } else {
        if trailing > 0 {
            return Err(DecodeError::Trailing(trailing));
        }
        (None, None)
    };
    Ok((message, endpoint, max_redeems))
}

/// Parse the wave-28 trailing extension section of one extended tag:
/// `flags:u8` then, per flag, `len:u16 + blob` (endpoint) and/or
/// `max_redeems:u32` (claim only). A cursor at the end of the frozen
/// fields is the legacy shape (`None, None`); anything else must parse
/// exactly and leave no leftovers.
fn decode_extension(
    ty: u8,
    cur: &mut Cur<'_>,
) -> Result<(Option<EndpointBlob>, Option<u32>), DecodeError> {
    if cur.remaining() == 0 {
        return Ok((None, None));
    }
    let flags = cur.u8()?;
    if flags & !EXT_FLAGS_KNOWN != 0 || flags == 0 {
        return Err(DecodeError::BadExtensionFlags(flags));
    }
    if flags & EXT_MAX_REDEEMS_FLAG != 0 && ty != TYPE_INVITE_CLAIM {
        return Err(DecodeError::BadExtension { ty });
    }
    let endpoint = if flags & EXT_ENDPOINT_FLAG != 0 {
        let len = cur.u16()? as usize;
        if len == 0 || len > ENDPOINT_BLOB_MAX_BYTES {
            return Err(DecodeError::EndpointBlobSize {
                len,
                limit: ENDPOINT_BLOB_MAX_BYTES,
            });
        }
        Some(EndpointBlob(cur.take(len)?.to_vec()))
    } else {
        None
    };
    let max_redeems = if flags & EXT_MAX_REDEEMS_FLAG != 0 {
        Some(cur.u32()?)
    } else {
        None
    };
    let leftover = cur.remaining();
    if leftover > 0 {
        return Err(DecodeError::Trailing(leftover));
    }
    Ok((endpoint, max_redeems))
}

fn get_summary(cur: &mut Cur<'_>) -> Result<InviteSummary, DecodeError> {
    Ok(InviteSummary {
        code_id: cur.fixed::<16>()?,
        state: InviteState::from_u8(cur.u8()?).ok_or(DecodeError::Malformed("invite state"))?,
        created_ms: cur.u64()?,
        expires_ms: cur.u64()?,
        revision: cur.u64()?,
    })
}

fn get_proof(cur: &mut Cur<'_>) -> Result<PairProof, DecodeError> {
    Ok(PairProof {
        mac: cur.fixed::<32>()?,
        signature: cur.fixed::<64>()?,
    })
}

fn get_redeem(cur: &mut Cur<'_>) -> Result<RedeemRequest, DecodeError> {
    Ok(RedeemRequest {
        request_id: cur.fixed::<16>()?,
        lookup_digest: cur.fixed::<32>()?,
        redeemer_nonce: cur.fixed::<32>()?,
        deadline_ms: cur.u64()?,
        request_mac: cur.fixed::<32>()?,
        signature: cur.fixed::<64>()?,
    })
}
