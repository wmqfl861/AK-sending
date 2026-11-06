//! Control protocol v2 codec tests (A1).
//!
//! Contract under test: `docs/contracts/protocol-v2.md` (byte-identical to
//! `docs/plans/pairing-code-plan-part2.md` A.4). Locks, per the A1 gate:
//!
//! - every variant round-trips through both the plain and the
//!   direction-aware encode/decode paths;
//! - every per-tag payload budget equals the frozen table (payload bytes,
//!   header excluded), with exact-full and one-over boundaries;
//! - the type-byte set is closed — all 256 values enumerated, same for
//!   `PeerSignal` sub-tags;
//! - every variant rejects truncation (every strict prefix) and trailing
//!   bytes, undefined closed-enum values, port-0 candidates, bad state
//!   filters and out-of-range limits;
//! - direction legality (contract section 8) is refused by the codec, not
//!   just documented — including the server-only origin of `SameEgress`;
//! - the all-zero paging sentinels round-trip as first-class values.

use std::net::SocketAddr;

use ak_signal::protocol_v2::{
    self as v2, AbortReason, CtrlFrame, CtrlMessage, DecodeError, Direction, EncodeError,
    EndpointBlob, ErrorScope, InviteState, InviteSummary, PairProof, PairingState, PeerSignal,
    RedeemRequest, Role, StateFilter, CURSOR_END, CURSOR_START, ENDPOINT_BLOB_MAX_BYTES,
    EXT_ENDPOINT_FLAG, EXT_MAX_REDEEMS_FLAG, HEADER_LEN, MAX_FRAME_BYTES, MAX_PAYLOAD_BYTES,
    VERSION,
};

// ---- helpers -------------------------------------------------------------

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

/// IPv6 candidate `i`: the largest per-item encoding (19 bytes).
fn v6(i: u16) -> SocketAddr {
    addr(&format!("[2001:db8::{i:x}]:{}", 1024 + i))
}

fn summary(i: u8) -> InviteSummary {
    InviteSummary {
        code_id: [i; 16],
        state: InviteState::Active,
        created_ms: 1_000 + u64::from(i),
        expires_ms: 0,
        revision: u64::from(i),
    }
}

fn proof(seed: u8) -> PairProof {
    PairProof {
        mac: [seed; 32],
        signature: [seed + 1; 64],
    }
}

fn redeem(seed: u8) -> RedeemRequest {
    RedeemRequest {
        request_id: [seed; 16],
        lookup_digest: [seed + 1; 32],
        redeemer_nonce: [seed + 2; 32],
        deadline_ms: u64::MAX,
        request_mac: [seed + 3; 32],
        signature: [seed + 4; 64],
    }
}

/// One maximal sample per variant (30 total): longest strings, fullest
/// arrays, extreme integers, so the closed-set sweep also stresses budgets.
fn all_variants() -> Vec<CtrlMessage> {
    let max_candidates: Vec<SocketAddr> = (0..v2::MAX_CANDIDATES as u16).map(v6).collect();
    let max_entries: Vec<InviteSummary> = (0..v2::MAX_INVITE_ENTRIES as u8).map(summary).collect();
    vec![
        CtrlMessage::Hello {
            version: 2,
            device_id: [0x01; 32],
            name: "n".repeat(v2::MAX_NAME_BYTES),
            timestamp_ms: u64::MAX,
            challenge: [0x07; 32],
            signature: [0x09; 64],
        },
        CtrlMessage::Welcome {
            session_ttl_secs: u32::MAX,
            capabilities: u32::MAX,
        },
        CtrlMessage::Register {
            candidates: max_candidates.clone(),
        },
        CtrlMessage::ConnectRequest {
            request_id: [0x02; 16],
            target: [0x03; 32],
            pairing_id: [0x04; 16],
            code_id: [0x05; 16],
            connection_id: [0x06; 16],
        },
        CtrlMessage::SessionOpened {
            session: [0x01; 16],
            peer: [0x02; 32],
            pairing_id: [0x03; 16],
            code_id: [0x04; 16],
            connection_id: [0x05; 16],
            expires_ms: u64::MAX,
        },
        CtrlMessage::Signal {
            session: [0x01; 16],
            signal: PeerSignal::Candidates(max_candidates.clone()),
        },
        CtrlMessage::Cancel {
            session: [0x08; 16],
        },
        CtrlMessage::ServerError {
            code: u16::MAX,
            message: "e".repeat(v2::MAX_ERROR_BYTES),
        },
        CtrlMessage::AuthChallenge {
            registry_id: [0x01; 32],
            nonce: [0x02; 32],
            expires_ms: u64::MAX,
        },
        CtrlMessage::InviteClaim {
            request_id: [0x0A; 16],
            lookup_digest: [0x0B; 32],
            ttl_secs: u32::MAX,
        },
        CtrlMessage::InviteClaimed {
            request_id: [0x0A; 16],
            code_id: [0x0C; 16],
            created_ms: 42,
            expires_ms: 0,
            revision: u64::MAX,
        },
        CtrlMessage::InviteList {
            request_id: [0x0D; 16],
            cursor: CURSOR_START,
            limit: v2::MAX_INVITE_LIMIT,
            state_filter: StateFilter::ALL,
        },
        CtrlMessage::InviteListPage {
            request_id: [0x0D; 16],
            next_cursor: CURSOR_END,
            entries: max_entries,
        },
        CtrlMessage::InviteRevoke {
            request_id: [0x0E; 16],
            code_id: [0x0F; 16],
            expected_revision: u64::MAX,
        },
        CtrlMessage::InviteRevoked {
            request_id: [0x0E; 16],
            code_id: [0x0F; 16],
            revision: 7,
            revoked_ms: 99,
        },
        CtrlMessage::InviteRedeem(redeem(0x10)),
        CtrlMessage::InviteRedeemPending {
            request_id: [0x10; 16],
            pairing_id: [0x11; 16],
            deadline_ms: u64::MAX,
        },
        CtrlMessage::InviteRedeemOffered {
            code_id: [0x12; 16],
            code_revision: u64::MAX,
            pairing_id: [0x13; 16],
            redeemer_device_id: [0x14; 32],
            redeem: redeem(0x15),
        },
        CtrlMessage::PairChallenge {
            transcript: [0xAA; v2::message::TRANSCRIPT_BYTES],
            owner_proof: proof(1),
        },
        CtrlMessage::PairProof {
            pairing_id: [0x14; 16],
            transcript_digest: [0xCC; 32],
            redeemer_proof: proof(3),
        },
        CtrlMessage::PairPrepared {
            pairing_id: [0x15; 16],
            transcript_digest: [0xDD; 32],
            signature: [0x0D; 64],
        },
        CtrlMessage::PairCommit {
            transcript: [0xBB; v2::message::TRANSCRIPT_BYTES],
            owner_proof: proof(5),
            redeemer_proof: proof(7),
            owner_prepared_signature: [0x0E; 64],
            redeemer_prepared_signature: [0x0F; 64],
            commit_revision: u64::MAX,
        },
        CtrlMessage::PairApplied {
            pairing_id: [0x17; 16],
            transcript_digest: [0xEE; 32],
            commit_revision: u64::MAX,
            signature: [0x1E; 64],
        },
        CtrlMessage::PairReady {
            pairing_id: [0x18; 16],
            transcript_digest: [0x99; 32],
            commit_revision: u64::MAX,
            owner_applied_signature: [0x2A; 64],
            redeemer_applied_signature: [0x2B; 64],
        },
        CtrlMessage::PairStatusQuery {
            request_id: [0x19; 16],
            pairing_id: [0x1A; 16],
        },
        CtrlMessage::PairStatusReply {
            request_id: [0x19; 16],
            pairing_id: [0x1A; 16],
            state: PairingState::Ready,
            commit_revision: u64::MAX,
            deadline_ms: u64::MAX,
        },
        CtrlMessage::PairAbort {
            request_id: [0x1B; 16],
            pairing_id: [0x1C; 16],
            reason: AbortReason::OwnerOffline,
        },
        CtrlMessage::PairError {
            request_id: [0x1C; 16],
            scope: ErrorScope::Pairing,
            object_id: [0x2C; 16],
            code: v2::PAIR_UNAVAILABLE,
            retry_after_ms: 5_000,
        },
        CtrlMessage::Ping { nonce: [0x1D; 16] },
        CtrlMessage::Pong { nonce: [0x1E; 16] },
    ]
}

fn encode(message: &CtrlMessage) -> Vec<u8> {
    let mut out = Vec::new();
    CtrlFrame::new(message.clone())
        .try_encode(&mut out)
        .unwrap();
    out
}

/// Round-trip through all four paths: plain encode/decode and the
/// direction-aware pair with the one legal sender.
fn roundtrip(message: CtrlMessage) {
    let original = CtrlFrame::new(message);
    let mut bytes = Vec::new();
    original.try_encode(&mut bytes).unwrap();
    assert!(bytes.len() <= MAX_FRAME_BYTES, "frame over hard cap");
    assert_eq!(bytes[0], VERSION);
    let payload_len = (bytes.len() - HEADER_LEN) as u16;
    assert_eq!(&bytes[2..4], &payload_len.to_be_bytes());
    assert_eq!(CtrlFrame::decode(&bytes).unwrap(), original);

    let sender = if original.message.allows_sender(Role::Client) {
        Role::Client
    } else {
        Role::Server
    };
    let mut directed = Vec::new();
    original.try_encode_as(sender, &mut directed).unwrap();
    assert_eq!(directed, bytes);
    assert_eq!(
        CtrlFrame::decode_as(&bytes, sender.opposite()).unwrap(),
        original
    );
}

/// Raw v2 frame with an arbitrary payload, bypassing encode.
fn wrap(ty: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![VERSION, ty];
    v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// The contract's frozen direction table (section 8), by type byte.
fn expected_direction(ty: u8) -> Direction {
    match ty {
        0x01 | 0x03 | 0x04 | 0x07 | 0x0A | 0x0C | 0x0E | 0x10 | 0x15 | 0x17 | 0x19 | 0x1B
        | 0x1D => Direction::ClientToServer,
        0x02 | 0x05 | 0x08 | 0x09 | 0x0B | 0x0D | 0x0F | 0x11 | 0x12 | 0x16 | 0x18 | 0x1A
        | 0x1C | 0x1E => Direction::ServerToClient,
        0x06 | 0x13 | 0x14 => Direction::Both,
        other => panic!("type byte {other:#04x} not in the closed set"),
    }
}

// ---- roundtrips ----------------------------------------------------------

#[test]
fn roundtrip_hello() {
    roundtrip(CtrlMessage::Hello {
        version: 2,
        device_id: [1; 32],
        name: String::new(),
        timestamp_ms: 42,
        challenge: [5; 32],
        signature: [2; 64],
    });
    roundtrip(CtrlMessage::Hello {
        version: 2,
        device_id: [9; 32],
        name: "n".repeat(v2::MAX_NAME_BYTES),
        timestamp_ms: u64::MAX,
        challenge: [0xA5; 32],
        signature: [7; 64],
    });
}

#[test]
fn roundtrip_welcome() {
    roundtrip(CtrlMessage::Welcome {
        session_ttl_secs: 120,
        capabilities: 0,
    });
    roundtrip(CtrlMessage::Welcome {
        session_ttl_secs: u32::MAX,
        capabilities: u32::MAX,
    });
}

#[test]
fn roundtrip_register() {
    roundtrip(CtrlMessage::Register { candidates: vec![] });
    roundtrip(CtrlMessage::Register {
        candidates: vec![
            addr("192.168.1.2:5000"),
            addr("[2001:db8::1]:5001"),
            addr("127.0.0.1:65535"),
            addr("[::1]:1"),
        ],
    });
    roundtrip(CtrlMessage::Register {
        candidates: (0..v2::MAX_CANDIDATES as u16).map(v6).collect(),
    });
}

#[test]
fn roundtrip_connect_request() {
    roundtrip(CtrlMessage::ConnectRequest {
        request_id: [1; 16],
        target: [2; 32],
        pairing_id: [3; 16],
        code_id: [4; 16],
        connection_id: [5; 16],
    });
}

#[test]
fn roundtrip_session_opened() {
    roundtrip(CtrlMessage::SessionOpened {
        session: [3; 16],
        peer: [6; 32],
        pairing_id: [7; 16],
        code_id: [8; 16],
        connection_id: [9; 16],
        expires_ms: u64::MAX,
    });
}

#[test]
fn roundtrip_signal_all_kinds() {
    roundtrip(CtrlMessage::Signal {
        session: [3; 16],
        signal: PeerSignal::Candidates(vec![addr("10.1.2.3:9000"), addr("[fe80::1]:9001")]),
    });
    roundtrip(CtrlMessage::Signal {
        session: [3; 16],
        signal: PeerSignal::PunchAt {
            epoch_unix_ms: u64::MAX,
            nonce: [9; 8],
        },
    });
    roundtrip(CtrlMessage::Signal {
        session: [3; 16],
        signal: PeerSignal::Bye,
    });
    roundtrip(CtrlMessage::Signal {
        session: [3; 16],
        signal: PeerSignal::SameEgress,
    });
}

#[test]
fn roundtrip_cancel() {
    roundtrip(CtrlMessage::Cancel { session: [8; 16] });
}

#[test]
fn roundtrip_server_error() {
    roundtrip(CtrlMessage::ServerError {
        code: 3,
        message: "rate limited".to_string(),
    });
    roundtrip(CtrlMessage::ServerError {
        code: u16::MAX,
        message: "e".repeat(v2::MAX_ERROR_BYTES),
    });
}

#[test]
fn roundtrip_auth_challenge() {
    roundtrip(CtrlMessage::AuthChallenge {
        registry_id: [0x51; 32],
        nonce: [0x52; 32],
        expires_ms: u64::MAX,
    });
}

#[test]
fn roundtrip_invite_claim_and_claimed() {
    roundtrip(CtrlMessage::InviteClaim {
        request_id: [1; 16],
        lookup_digest: [2; 32],
        ttl_secs: 0, // long-lived
    });
    roundtrip(CtrlMessage::InviteClaim {
        request_id: [1; 16],
        lookup_digest: [2; 32],
        ttl_secs: 86_400,
    });
    roundtrip(CtrlMessage::InviteClaimed {
        request_id: [1; 16],
        code_id: [3; 16],
        created_ms: 1_000,
        expires_ms: 0, // 0 = long-lived
        revision: 1,
    });
}

#[test]
fn roundtrip_invite_list() {
    for (cursor, limit, filter) in [
        (CURSOR_START, 1, StateFilter::ACTIVE),
        ([0x77u8; 16], v2::MAX_INVITE_LIMIT, StateFilter::ALL),
        ([0x77u8; 16], 16, StateFilter::REVOKED),
        ([0x77u8; 16], 32, StateFilter::EXPIRED),
        ([0x77u8; 16], 32, StateFilter::from_bits(0).unwrap()),
        ([0x77u8; 16], 32, StateFilter::from_bits(0x03).unwrap()),
    ] {
        roundtrip(CtrlMessage::InviteList {
            request_id: [0x0C; 16],
            cursor,
            limit,
            state_filter: filter,
        });
    }
}

#[test]
fn roundtrip_invite_list_page() {
    roundtrip(CtrlMessage::InviteListPage {
        request_id: [1; 16],
        next_cursor: CURSOR_END,
        entries: vec![],
    });
    roundtrip(CtrlMessage::InviteListPage {
        request_id: [1; 16],
        next_cursor: [0x31; 16],
        entries: vec![summary(1), summary(2)],
    });
    let mut all_states = Vec::new();
    for (i, state) in [
        InviteState::Active,
        InviteState::Revoked,
        InviteState::Expired,
    ]
    .into_iter()
    .enumerate()
    {
        all_states.push(InviteSummary {
            code_id: [i as u8; 16],
            state,
            created_ms: u64::MAX,
            expires_ms: u64::MAX,
            revision: u64::MAX,
        });
    }
    roundtrip(CtrlMessage::InviteListPage {
        request_id: [1; 16],
        next_cursor: CURSOR_END,
        entries: all_states,
    });
    roundtrip(CtrlMessage::InviteListPage {
        request_id: [1; 16],
        next_cursor: CURSOR_END,
        entries: (0..v2::MAX_INVITE_ENTRIES as u8).map(summary).collect(),
    });
}

#[test]
fn roundtrip_invite_revoke_and_revoked() {
    roundtrip(CtrlMessage::InviteRevoke {
        request_id: [1; 16],
        code_id: [2; 16],
        expected_revision: u64::MAX,
    });
    roundtrip(CtrlMessage::InviteRevoked {
        request_id: [1; 16],
        code_id: [2; 16],
        revision: 8,
        revoked_ms: 123_456,
    });
}

#[test]
fn roundtrip_invite_redeem_family() {
    let request = redeem(0x10);
    roundtrip(CtrlMessage::InviteRedeem(request.clone()));
    roundtrip(CtrlMessage::InviteRedeemPending {
        request_id: request.request_id,
        pairing_id: [0x21; 16],
        deadline_ms: u64::MAX,
    });
    let offered = CtrlMessage::InviteRedeemOffered {
        code_id: [0x22; 16],
        code_revision: 4,
        pairing_id: [0x23; 16],
        redeemer_device_id: [0x24; 32],
        redeem: request,
    };
    roundtrip(offered);
}

#[test]
fn roundtrip_pair_challenge_and_proof() {
    roundtrip(CtrlMessage::PairChallenge {
        transcript: [0xAB; v2::message::TRANSCRIPT_BYTES],
        owner_proof: proof(0x31),
    });
    roundtrip(CtrlMessage::PairProof {
        pairing_id: [0x32; 16],
        transcript_digest: [0x33; 32],
        redeemer_proof: proof(0x34),
    });
}

#[test]
fn roundtrip_pair_prepared_applied_ready() {
    roundtrip(CtrlMessage::PairPrepared {
        pairing_id: [1; 16],
        transcript_digest: [2; 32],
        signature: [3; 64],
    });
    roundtrip(CtrlMessage::PairApplied {
        pairing_id: [1; 16],
        transcript_digest: [2; 32],
        commit_revision: 9,
        signature: [3; 64],
    });
    roundtrip(CtrlMessage::PairReady {
        pairing_id: [1; 16],
        transcript_digest: [2; 32],
        commit_revision: u64::MAX,
        owner_applied_signature: [4; 64],
        redeemer_applied_signature: [5; 64],
    });
}

#[test]
fn roundtrip_pair_commit() {
    roundtrip(CtrlMessage::PairCommit {
        transcript: [0xCD; v2::message::TRANSCRIPT_BYTES],
        owner_proof: proof(1),
        redeemer_proof: proof(2),
        owner_prepared_signature: [3; 64],
        redeemer_prepared_signature: [4; 64],
        commit_revision: u64::MAX,
    });
}

#[test]
fn roundtrip_pair_status_and_abort_all_enum_values() {
    for state in [
        PairingState::Pending,
        PairingState::Prepared,
        PairingState::Committed,
        PairingState::Applied,
        PairingState::Ready,
        PairingState::Aborted,
    ] {
        roundtrip(CtrlMessage::PairStatusReply {
            request_id: [1; 16],
            pairing_id: [2; 16],
            state,
            commit_revision: 5,
            deadline_ms: 6,
        });
    }
    roundtrip(CtrlMessage::PairStatusQuery {
        request_id: [1; 16],
        pairing_id: [2; 16],
    });
    for reason in [
        AbortReason::Unspecified,
        AbortReason::User,
        AbortReason::Expired,
        AbortReason::Protocol,
        AbortReason::OwnerOffline,
        AbortReason::Revoked,
    ] {
        roundtrip(CtrlMessage::PairAbort {
            request_id: [1; 16],
            pairing_id: [2; 16],
            reason,
        });
    }
}

#[test]
fn roundtrip_pair_error_all_scopes() {
    for scope in [
        ErrorScope::None,
        ErrorScope::Request,
        ErrorScope::Code,
        ErrorScope::Pairing,
        ErrorScope::Connection,
        ErrorScope::Session,
    ] {
        roundtrip(CtrlMessage::PairError {
            request_id: [1; 16],
            scope,
            object_id: [2; 16],
            code: v2::RATE_LIMITED,
            retry_after_ms: 250,
        });
    }
}

#[test]
fn roundtrip_ping_pong() {
    roundtrip(CtrlMessage::Ping { nonce: [0x11; 16] });
    roundtrip(CtrlMessage::Pong { nonce: [0x22; 16] });
}

// ---- closed set + budget table -------------------------------------------

#[test]
fn all_variants_roundtrip_closed_set() {
    let samples = all_variants();
    let mut seen: Vec<u8> = samples.iter().map(|m| m.type_byte()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 30, "one sample per variant expected");
    assert_eq!(seen.first(), Some(&0x01));
    assert_eq!(seen.last(), Some(&0x1E));
    for message in samples {
        let ty = message.type_byte();
        assert!((0x01..=0x1E).contains(&ty), "type byte outside legal set");
        roundtrip(message);
    }
}

/// Every per-tag payload budget from the contract tables, byte-locked
/// (payload only, 4-byte header excluded; variable messages at max fields).
#[test]
fn budget_table_locked() {
    use v2::message as m;
    let expected: &[(u8, usize)] = &[
        (0x01, m::HELLO_MAX_PAYLOAD),
        (0x02, m::WELCOME_PAYLOAD),
        (0x03, m::REGISTER_MAX_PAYLOAD),
        (0x04, m::CONNECT_REQUEST_PAYLOAD),
        (0x05, m::SESSION_OPENED_PAYLOAD),
        (0x06, m::SIGNAL_MAX_PAYLOAD),
        (0x07, m::CANCEL_PAYLOAD),
        (0x08, m::SERVER_ERROR_MAX_PAYLOAD),
        (0x09, m::AUTH_CHALLENGE_PAYLOAD),
        (0x0A, m::INVITE_CLAIM_PAYLOAD),
        (0x0B, m::INVITE_CLAIMED_PAYLOAD),
        (0x0C, m::INVITE_LIST_PAYLOAD),
        (0x0D, m::INVITE_LIST_PAGE_MAX_PAYLOAD),
        (0x0E, m::INVITE_REVOKE_PAYLOAD),
        (0x0F, m::INVITE_REVOKED_PAYLOAD),
        (0x10, m::INVITE_REDEEM_PAYLOAD),
        (0x11, m::INVITE_REDEEM_PENDING_PAYLOAD),
        (0x12, m::INVITE_REDEEM_OFFERED_PAYLOAD),
        (0x13, m::PAIR_CHALLENGE_PAYLOAD),
        (0x14, m::PAIR_PROOF_PAYLOAD),
        (0x15, m::PAIR_PREPARED_PAYLOAD),
        (0x16, m::PAIR_COMMIT_PAYLOAD),
        (0x17, m::PAIR_APPLIED_PAYLOAD),
        (0x18, m::PAIR_READY_PAYLOAD),
        (0x19, m::PAIR_STATUS_QUERY_PAYLOAD),
        (0x1A, m::PAIR_STATUS_REPLY_PAYLOAD),
        (0x1B, m::PAIR_ABORT_PAYLOAD),
        (0x1C, m::PAIR_ERROR_PAYLOAD),
        (0x1D, m::PING_PAYLOAD),
        (0x1E, m::PONG_PAYLOAD),
    ];
    // The frozen numbers themselves, independent of the module constants.
    // Wave-28: the four extended tags (0x0A/0x10/0x12/0x16) grew by the
    // optional trailing extension section (flags+u16 len+512-byte blob,
    // claim additionally u32 max_redeems); every other tag is unchanged.
    let frozen: &[(u8, usize)] = &[
        (0x01, 204),
        (0x02, 8),
        (0x03, 305),
        (0x04, 96),
        (0x05, 104),
        (0x06, 322),
        (0x07, 16),
        (0x08, 132),
        (0x09, 72),
        (0x0A, 571),
        (0x0B, 56),
        (0x0C, 34),
        (0x0D, 1345),
        (0x0E, 40),
        (0x0F, 48),
        (0x10, 699),
        (0x11, 40),
        (0x12, 771),
        (0x13, 352),
        (0x14, 144),
        (0x15, 112),
        (0x16, 1099),
        (0x17, 120),
        (0x18, 184),
        (0x19, 32),
        (0x1A, 49),
        (0x1B, 34),
        (0x1C, 39),
        (0x1D, 16),
        (0x1E, 16),
    ];
    for ((ty, constant), (_, number)) in expected.iter().zip(frozen) {
        assert_eq!(constant, number, "budget constant drift for tag {ty:#04x}");
    }
    // The four extended tags hit their budgets with a full extension
    // section (512-byte blob, claim with max_redeems); the frozen 26
    // legacy shapes are exact as before.
    let blob = EndpointBlob(vec![0xE5; v2::ENDPOINT_BLOB_MAX_BYTES]);
    let extended: Vec<(u8, CtrlFrame)> = vec![
        (
            0x0A,
            CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: [0x0A; 16],
                lookup_digest: [0x0B; 32],
                ttl_secs: u32::MAX,
            })
            .with_endpoint(blob.clone())
            .with_max_redeems(u32::MAX),
        ),
        (
            0x10,
            CtrlFrame::new(CtrlMessage::InviteRedeem(redeem(0x10)))
                .with_endpoint(blob.clone()),
        ),
        (
            0x12,
            CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                code_id: [0x12; 16],
                code_revision: u64::MAX,
                pairing_id: [0x13; 16],
                redeemer_device_id: [0x14; 32],
                redeem: redeem(0x15),
            })
            .with_endpoint(blob.clone()),
        ),
        (
            0x16,
            CtrlFrame::new(CtrlMessage::PairCommit {
                transcript: [0xBB; v2::message::TRANSCRIPT_BYTES],
                owner_proof: proof(5),
                redeemer_proof: proof(7),
                owner_prepared_signature: [0x0E; 64],
                redeemer_prepared_signature: [0x0F; 64],
                commit_revision: u64::MAX,
            })
            .with_endpoint(blob.clone()),
        ),
    ];
    for (ty, frame) in extended {
        let mut bytes = Vec::new();
        frame.try_encode(&mut bytes).unwrap();
        let budget = frozen
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, b)| *b)
            .unwrap();
        assert_eq!(
            bytes.len() - HEADER_LEN,
            budget,
            "tag {ty:#04x}: maximal extended sample must hit its budget exactly"
        );
        assert!(bytes.len() <= MAX_FRAME_BYTES);
    }
    for message in all_variants() {
        let ty = message.type_byte();
        let bytes = encode(&message);
        let budget = frozen
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, b)| *b)
            .unwrap();
        if matches!(ty, 0x0A | 0x10 | 0x12 | 0x16) {
            // Legacy shape of an extended tag: exactly the pre-wave-28 size.
            let legacy = match ty {
                0x0A => 52,
                0x10 => 184,
                0x12 => 256,
                _ => 584,
            };
            assert_eq!(bytes.len() - HEADER_LEN, legacy);
            continue;
        }
        assert_eq!(
            bytes.len() - HEADER_LEN,
            budget,
            "tag {ty:#04x}: maximal sample must hit its budget exactly"
        );
        assert!(bytes.len() <= MAX_FRAME_BYTES);
    }
}

#[test]
fn frame_constants_and_header_layout() {
    assert_eq!(VERSION, 2);
    assert_eq!(HEADER_LEN, 4);
    assert_eq!(MAX_FRAME_BYTES, 4096);
    assert_eq!(MAX_PAYLOAD_BYTES, 4092);
    let bytes = encode(&CtrlMessage::Welcome {
        session_ttl_secs: 7,
        capabilities: 9,
    });
    assert_eq!(
        &bytes,
        &[VERSION, 0x02, 0, 8]
            .iter()
            .chain(7u32.to_be_bytes().iter())
            .chain(9u32.to_be_bytes().iter())
            .copied()
            .collect::<Vec<u8>>()
    );
}

/// Byte-exact Hello layout including the v2-only `challenge` field, with a
/// multi-byte-UTF-8 name to prove the length prefix counts bytes.
#[test]
fn hello_wire_layout_byte_exact() {
    let name = "手机-📱";
    let message = CtrlMessage::Hello {
        version: 2,
        device_id: [0x11; 32],
        name: name.to_string(),
        timestamp_ms: 0x1122_3344_5566_7788,
        challenge: [0x77; 32],
        signature: [0x88; 64],
    };
    let bytes = encode(&message);
    let mut expected = Vec::new();
    expected.extend_from_slice(&2u16.to_be_bytes());
    expected.extend_from_slice(&[0x11; 32]);
    expected.extend_from_slice(&(name.len() as u16).to_be_bytes());
    expected.extend_from_slice(name.as_bytes());
    expected.extend_from_slice(&0x1122_3344_5566_7788u64.to_be_bytes());
    expected.extend_from_slice(&[0x77; 32]);
    expected.extend_from_slice(&[0x88; 64]);
    assert_eq!(bytes[1], 0x01);
    assert_eq!(&bytes[HEADER_LEN..], &expected);
    // The challenge sits exactly where the contract says: after timestamp.
    let challenge_at = 2 + 32 + 2 + name.len() + 8;
    assert_eq!(&expected[challenge_at..challenge_at + 32], &[0x77; 32]);
}

// ---- frame-level errors ----------------------------------------------------

#[test]
fn err_truncated_header() {
    assert!(matches!(
        CtrlFrame::decode(&[]),
        Err(DecodeError::TruncatedHeader)
    ));
    assert!(matches!(
        CtrlFrame::decode(&[VERSION, 1, 0]),
        Err(DecodeError::TruncatedHeader)
    ));
}

#[test]
fn err_bad_version() {
    for version in [1u8, 3, 255] {
        assert!(matches!(
            CtrlFrame::decode(&[version, 0x02, 0, 0]),
            Err(DecodeError::BadVersion(v)) if v == version
        ));
    }
}

/// The type set is closed: all 256 bytes enumerated (contract section 10),
/// not just a sample of "known + a few".
#[test]
fn err_unknown_type_all_256_enumerated() {
    let known: Vec<u8> = all_variants().iter().map(|m| m.type_byte()).collect();
    for ty in 0u8..=255 {
        let result = CtrlFrame::decode(&wrap(ty, &[]));
        if known.contains(&ty) {
            assert!(
                !matches!(result, Err(DecodeError::UnknownType(_))),
                "type {ty:#04x} is defined; empty payload must fail for another reason"
            );
        } else {
            assert!(
                matches!(result, Err(DecodeError::UnknownType(t)) if t == ty),
                "type {ty:#04x} must be rejected as unknown"
            );
        }
    }
}

#[test]
fn err_oversized_and_at_cap() {
    // One over the 4092 payload cap.
    assert!(matches!(
        CtrlFrame::decode(&[VERSION, 0x01, 0x0F, 0xFD]),
        Err(DecodeError::Oversized {
            declared: 4093,
            cap: 4092
        })
    ));
    assert!(matches!(
        CtrlFrame::decode(&[VERSION, 0x01, 0xFF, 0xFF]),
        Err(DecodeError::Oversized { .. })
    ));
    // Exactly 4092 payload bytes passes the size checks and only fails
    // later payload parsing (never as Oversized).
    let mut bytes = vec![VERSION, 0x01, 0x0F, 0xFC];
    bytes.resize(HEADER_LEN + 4092, 0);
    match CtrlFrame::decode(&bytes) {
        Err(DecodeError::Oversized { .. }) => panic!("at-cap payload rejected as Oversized"),
        Err(_) => {}
        Ok(_) => panic!("zero-padded Hello payload must not decode"),
    }
}

/// Every strict prefix of every variant's frame is refused (truncation is
/// never tolerated, header or payload).
#[test]
fn every_variant_rejects_every_strict_prefix() {
    for message in all_variants() {
        let bytes = encode(&message);
        for cut in 0..bytes.len() {
            assert!(
                CtrlFrame::decode(&bytes[..cut]).is_err(),
                "type {:#04x}: prefix of {} bytes must not decode",
                message.type_byte(),
                cut
            );
        }
    }
}

#[test]
fn every_variant_rejects_trailing_byte() {
    for message in all_variants() {
        let mut bytes = encode(&message);
        bytes.push(0);
        // The appended byte sits outside the declared length, so the
        // frame layer refuses it for every tag — extended or not — before
        // payload parsing; the wave-28 section lives INSIDE the declared
        // payload and is exercised by the crafted-payload tests below.
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::Trailing(1))
        ));
    }
}

// ---- string-field rules ----------------------------------------------------

#[test]
fn err_hello_name_budget_and_control_chars() {
    // Exactly-full and one-over on the 64-byte name budget.
    let mut max_bytes = Vec::new();
    CtrlFrame::new(CtrlMessage::Hello {
        version: 2,
        device_id: [1; 32],
        name: "n".repeat(v2::MAX_NAME_BYTES),
        timestamp_ms: 1,
        challenge: [2; 32],
        signature: [3; 64],
    })
    .try_encode(&mut max_bytes)
    .unwrap();
    assert_eq!(max_bytes.len() - HEADER_LEN, v2::message::HELLO_MAX_PAYLOAD);

    let over = CtrlMessage::Hello {
        version: 2,
        device_id: [1; 32],
        name: "n".repeat(v2::MAX_NAME_BYTES + 1),
        timestamp_ms: 1,
        challenge: [2; 32],
        signature: [3; 64],
    };
    assert!(matches!(
        CtrlFrame::new(over.clone()).try_encode(&mut Vec::new()),
        Err(EncodeError::StringTooLong {
            limit: v2::MAX_NAME_BYTES
        })
    ));
    // Decode of a crafted 65-byte name refuses identically.
    let mut payload = Vec::new();
    payload.extend_from_slice(&2u16.to_be_bytes());
    payload.extend_from_slice(&[0x01; 32]);
    payload.extend_from_slice(&65u16.to_be_bytes());
    payload.extend_from_slice(&[b'n'; 65]);
    payload.extend_from_slice(&[0u8; 8 + 32 + 64]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x01, &payload)),
        Err(DecodeError::StringTooLong {
            limit: v2::MAX_NAME_BYTES
        })
    ));

    // C0/DEL/C1 injection refused on both encode and decode.
    for name in ["a\u{0}b", "\u{1b}[2J wiped", "x\u{7f}y", "\u{9f}"] {
        assert!(matches!(
            CtrlFrame::new(CtrlMessage::Hello {
                version: 2,
                device_id: [1; 32],
                name: name.to_string(),
                timestamp_ms: 1,
                challenge: [2; 32],
                signature: [3; 64],
            })
            .try_encode(&mut Vec::new()),
            Err(EncodeError::ControlChars)
        ));
    }
    let mut payload = vec![0, 2, 0, 3, 0xff, 0xfe, 0xfd];
    payload.extend_from_slice(&[0u8; 8 + 32 + 64]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x01, &payload)),
        Err(DecodeError::Malformed(_)) // invalid utf-8
    ));
}

#[test]
fn err_server_error_message_budget_and_control_chars() {
    let mut max = Vec::new();
    CtrlFrame::new(CtrlMessage::ServerError {
        code: 1,
        message: "e".repeat(v2::MAX_ERROR_BYTES),
    })
    .try_encode(&mut max)
    .unwrap();
    assert_eq!(
        max.len() - HEADER_LEN,
        v2::message::SERVER_ERROR_MAX_PAYLOAD
    );
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::ServerError {
            code: 1,
            message: "e".repeat(v2::MAX_ERROR_BYTES + 1),
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::StringTooLong {
            limit: v2::MAX_ERROR_BYTES
        })
    ));
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::ServerError {
            code: 1,
            message: "err\u{0}or".to_string(),
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::ControlChars)
    ));
}

// ---- candidate / address rules ---------------------------------------------

#[test]
fn err_candidate_count_over_16_register_and_signal() {
    let seventeen: Vec<SocketAddr> = (0..=v2::MAX_CANDIDATES as u16).map(v6).collect();
    assert_eq!(seventeen.len(), 17);
    // Register: encode and decode.
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::Register {
            candidates: seventeen.clone(),
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::TooManyCandidates {
            count: 17,
            limit: 16
        })
    ));
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x03, &[17])),
        Err(DecodeError::TooManyCandidates {
            count: 17,
            limit: v2::MAX_CANDIDATES
        })
    ));
    // Signal::Candidates: same cap.
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::Signal {
            session: [0; 16],
            signal: PeerSignal::Candidates(seventeen),
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::TooManyCandidates {
            count: 17,
            limit: 16
        })
    ));
    let mut payload = vec![0u8; 16];
    payload.extend_from_slice(&[0x01, 17]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x06, &payload)),
        Err(DecodeError::TooManyCandidates { .. })
    ));
}

#[test]
fn err_address_family_and_port_zero() {
    // count=1, family=5 (only 4 and 6 are defined).
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x03, &[1, 5, 1, 0, 0, 1, 0x30, 0x39])),
        Err(DecodeError::Malformed(_))
    ));
    // v6 candidate with only 5 of 16 address bytes.
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x03, &[1, 6, 0x20, 0x01, 0x0d, 0xb8])),
        Err(DecodeError::Malformed(_))
    ));
    // Decode refuses port 0; production encode refuses it too (try_encode).
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x03, &[1, 4, 127, 0, 0, 1, 0, 0])),
        Err(DecodeError::Malformed(msg)) if msg.contains("port")
    ));
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::Register {
            candidates: vec![addr("127.0.0.1:0")],
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::PortZero)
    ));
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::Signal {
            session: [0; 16],
            signal: PeerSignal::Candidates(vec![addr("[::1]:0")]),
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::PortZero)
    ));
}

/// The PeerSignal sub-tag set is closed: all 256 bytes enumerated.
#[test]
fn err_signal_subtag_all_256_enumerated() {
    for tag in 0u8..=255 {
        let mut payload = vec![0u8; 16];
        payload.push(tag);
        let result = CtrlFrame::decode(&wrap(0x06, &payload));
        match tag {
            0x03 | 0x04 => assert!(result.is_ok(), "sub-tag {tag:#04x} carries no body"),
            0x01 | 0x02 => assert!(
                matches!(result, Err(DecodeError::Malformed(_))),
                "sub-tag {tag:#04x} with missing body must fail parsing"
            ),
            _ => assert!(
                matches!(result, Err(DecodeError::Malformed(msg)) if msg.contains("signal tag")),
                "sub-tag {tag:#04x} must be rejected as undefined"
            ),
        }
    }
}

#[test]
fn err_signal_body_edge_cases() {
    // SameEgress carries no data; extra payload is trailing.
    let mut payload = vec![0u8; 16];
    payload.extend_from_slice(&[0x04, 1, 2, 3]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x06, &payload)),
        Err(DecodeError::Trailing(3))
    ));
    // PunchAt tag with only 4 of the 8 nonce bytes.
    let mut payload = vec![0u8; 16];
    payload.push(0x02);
    payload.extend_from_slice(&9u64.to_be_bytes());
    payload.extend_from_slice(&[0, 0, 0, 0]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x06, &payload)),
        Err(DecodeError::Malformed(_))
    ));
}

// ---- invite list constraints -----------------------------------------------

#[test]
fn err_invite_entries_over_32() {
    let entries: Vec<InviteSummary> = (0..=v2::MAX_INVITE_ENTRIES as u8).map(summary).collect();
    assert_eq!(entries.len(), 33);
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::InviteListPage {
            request_id: [0; 16],
            next_cursor: CURSOR_END,
            entries,
        })
        .try_encode(&mut Vec::new()),
        Err(EncodeError::TooManyEntries {
            count: 33,
            limit: 32
        })
    ));
    // Decode: count byte 33 is refused before any entry is read.
    let mut payload = vec![0u8; 32];
    payload.push(33);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x0D, &payload)),
        Err(DecodeError::TooManyEntries {
            count: 33,
            limit: v2::MAX_INVITE_ENTRIES
        })
    ));
}

#[test]
fn err_invite_limit_bounds() {
    for bad in [0u8, 33, 255] {
        assert!(matches!(
            CtrlFrame::new(CtrlMessage::InviteList {
                request_id: [0; 16],
                cursor: CURSOR_START,
                limit: bad,
                state_filter: StateFilter::ALL,
            })
            .try_encode(&mut Vec::new()),
            Err(EncodeError::BadInviteLimit(l)) if l == bad
        ));
        let mut payload = vec![0u8; 32];
        payload.push(bad);
        payload.push(StateFilter::ALL.bits());
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x0C, &payload)),
            Err(DecodeError::BadInviteLimit(l)) if l == bad
        ));
    }
    for good in [v2::MIN_INVITE_LIMIT, v2::MAX_INVITE_LIMIT] {
        roundtrip(CtrlMessage::InviteList {
            request_id: [0; 16],
            cursor: CURSOR_START,
            limit: good,
            state_filter: StateFilter::ALL,
        });
    }
}

#[test]
fn state_filter_defined_bits_only() {
    assert_eq!(StateFilter::ACTIVE.bits(), 0x01);
    assert_eq!(StateFilter::REVOKED.bits(), 0x02);
    assert_eq!(StateFilter::EXPIRED.bits(), 0x04);
    assert_eq!(StateFilter::ALL.bits(), 0x07);
    assert!(StateFilter::ACTIVE.contains(InviteState::Active));
    assert!(!StateFilter::ACTIVE.contains(InviteState::Revoked));
    assert!(StateFilter::ALL.contains(InviteState::Expired));
    for bits in 0u8..=0x07 {
        assert_eq!(StateFilter::from_bits(bits).unwrap().bits(), bits);
    }
    for bits in [0x08u8, 0x10, 0x80, 0xFF] {
        assert!(StateFilter::from_bits(bits).is_none());
        let mut payload = vec![0u8; 32];
        payload.push(v2::MAX_INVITE_LIMIT);
        payload.push(bits);
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x0C, &payload)),
            Err(DecodeError::BadStateFilter(b)) if b == bits
        ));
    }
}

#[test]
fn err_invite_state_closed() {
    for state in 0u8..=2 {
        let mut payload = vec![0u8; 32];
        payload.push(1); // count = 1
        payload.extend_from_slice(&[0x0F; 16]); // code_id
        payload.push(state);
        payload.extend_from_slice(&[0u8; 24]); // created/expires/revision
        let decoded = CtrlFrame::decode(&wrap(0x0D, &payload)).unwrap();
        let CtrlMessage::InviteListPage { entries, .. } = decoded.message else {
            panic!("expected InviteListPage");
        };
        assert_eq!(entries[0].state.as_u8(), state);
    }
    for state in [3u8, 4, 200, 255] {
        let mut payload = vec![0u8; 32];
        payload.push(1);
        payload.extend_from_slice(&[0x0F; 16]);
        payload.push(state);
        payload.extend_from_slice(&[0u8; 24]);
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x0D, &payload)),
            Err(DecodeError::Malformed(msg)) if msg.contains("invite state")
        ));
    }
}

#[test]
fn err_pairing_state_scope_reason_closed() {
    // PairingState: 0..=5 decode, >=6 refuse.
    for state in 0u8..=5 {
        let mut payload = vec![0u8; 32];
        payload.push(state);
        payload.extend_from_slice(&[0u8; 16]);
        let decoded = CtrlFrame::decode(&wrap(0x1A, &payload)).unwrap();
        assert_eq!(
            match &decoded.message {
                CtrlMessage::PairStatusReply { state, .. } => state.as_u8(),
                _ => panic!("expected PairStatusReply"),
            },
            state
        );
    }
    for state in [6u8, 7, 255] {
        let mut payload = vec![0u8; 32];
        payload.push(state);
        payload.extend_from_slice(&[0u8; 16]);
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x1A, &payload)),
            Err(DecodeError::Malformed(msg)) if msg.contains("pairing state")
        ));
    }
    // ErrorScope: >=6 refuse.
    let mut payload = vec![0u8; 16];
    payload.push(6);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x1C, &payload)),
        Err(DecodeError::Malformed(msg)) if msg.contains("error scope")
    ));
    // AbortReason is u16 on the wire: 6 refuses, and 0x0100 (256) must be
    // refused outright, never truncated to Unspecified(0).
    for reason_bytes in [[0x00, 0x06], [0x01, 0x00], [0xFF, 0xFF]] {
        let mut payload = vec![0u8; 32];
        payload.extend_from_slice(&reason_bytes);
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x1B, &payload)),
            Err(DecodeError::Malformed(msg)) if msg.contains("abort reason")
        ));
    }
}

// ---- paging sentinels -------------------------------------------------------

#[test]
fn list_paging_sentinels() {
    assert_eq!(CURSOR_START, [0u8; 16]);
    assert_eq!(CURSOR_END, [0u8; 16]);
    // Start sentinel in a request, end sentinel in a reply: both first-class
    // wire values, round-trip verbatim.
    roundtrip(CtrlMessage::InviteList {
        request_id: [9; 16],
        cursor: CURSOR_START,
        limit: 32,
        state_filter: StateFilter::ALL,
    });
    roundtrip(CtrlMessage::InviteListPage {
        request_id: [9; 16],
        next_cursor: CURSOR_END,
        entries: vec![summary(1)],
    });
    // A real (nonzero) cursor round-trips too.
    roundtrip(CtrlMessage::InviteList {
        request_id: [9; 16],
        cursor: [0x42; 16],
        limit: 16,
        state_filter: StateFilter::ACTIVE,
    });
}

// ---- direction legality ------------------------------------------------------

#[test]
fn direction_table_locked() {
    for message in all_variants() {
        let ty = message.type_byte();
        assert_eq!(
            message.direction(),
            expected_direction(ty),
            "direction drift for tag {ty:#04x}"
        );
        assert_eq!(
            message.direction().allows_sender(Role::Client),
            expected_direction(ty).allows_sender(Role::Client)
        );
        assert_eq!(
            message.direction().allows_sender(Role::Server),
            expected_direction(ty).allows_sender(Role::Server)
        );
    }
    assert_eq!(Role::Client.opposite(), Role::Server);
    assert_eq!(Role::Server.opposite(), Role::Client);
}

/// Every variant, both roles: encoding as the wrong sender refuses without
/// writing a single byte; encoding as the right sender succeeds.
#[test]
fn direction_encode_rejects_wrong_sender_for_all_variants() {
    for message in all_variants() {
        for sender in [Role::Client, Role::Server] {
            let mut out = Vec::new();
            let result = CtrlFrame::new(message.clone()).try_encode_as(sender, &mut out);
            if message.allows_sender(sender) {
                result.unwrap();
                assert!(!out.is_empty());
            } else {
                assert!(matches!(
                    result,
                    Err(EncodeError::WrongDirection {
                        ty,
                        sender: s
                    }) if ty == message.type_byte() && s == sender
                ));
                assert!(out.is_empty(), "refused encode must not write bytes");
            }
        }
    }
}

#[test]
fn direction_decode_rejects_wrong_receiver() {
    // A server must not accept another server's Welcome.
    let welcome = encode(&CtrlMessage::Welcome {
        session_ttl_secs: 1,
        capabilities: 0,
    });
    assert!(matches!(
        CtrlFrame::decode_as(&welcome, Role::Server),
        Err(DecodeError::WrongDirection { ty: 0x02, .. })
    ));
    assert!(CtrlFrame::decode_as(&welcome, Role::Client).is_ok());
    // A client must not accept a Register / Hello / Ping.
    for message in [
        CtrlMessage::Register { candidates: vec![] },
        CtrlMessage::Hello {
            version: 2,
            device_id: [1; 32],
            name: "x".to_string(),
            timestamp_ms: 1,
            challenge: [2; 32],
            signature: [3; 64],
        },
        CtrlMessage::Ping { nonce: [1; 16] },
    ] {
        let bytes = encode(&message);
        assert!(matches!(
            CtrlFrame::decode_as(&bytes, Role::Client),
            Err(DecodeError::WrongDirection { .. })
        ));
        assert!(CtrlFrame::decode_as(&bytes, Role::Server).is_ok());
    }
    // Genuinely bidirectional messages decode for both receivers.
    for signal in [
        PeerSignal::Candidates(vec![addr("10.0.0.1:1")]),
        PeerSignal::PunchAt {
            epoch_unix_ms: 1,
            nonce: [0; 8],
        },
        PeerSignal::Bye,
    ] {
        let bytes = encode(&CtrlMessage::Signal {
            session: [1; 16],
            signal,
        });
        assert!(CtrlFrame::decode_as(&bytes, Role::Client).is_ok());
        assert!(CtrlFrame::decode_as(&bytes, Role::Server).is_ok());
    }
}

/// `SameEgress` is server-generated only (contract section 7): a client
/// may neither encode nor be the author of one; the server may.
#[test]
fn same_egress_server_origin_only() {
    let message = CtrlMessage::Signal {
        session: [1; 16],
        signal: PeerSignal::SameEgress,
    };
    let mut out = Vec::new();
    assert!(matches!(
        CtrlFrame::new(message.clone()).try_encode_as(Role::Client, &mut out),
        Err(EncodeError::WrongDirection { ty: 0x06, .. })
    ));
    assert!(out.is_empty());
    CtrlFrame::new(message.clone())
        .try_encode_as(Role::Server, &mut out)
        .unwrap();
    // Server sent it -> client receives fine; client sent it -> server refuses.
    assert!(CtrlFrame::decode_as(&out, Role::Client).is_ok());
    assert!(matches!(
        CtrlFrame::decode_as(&out, Role::Server),
        Err(DecodeError::WrongDirection { ty: 0x06, .. })
    ));
    // The envelope direction stays "both"; only the SameEgress refinement
    // is server-origin.
    assert_eq!(message.direction(), Direction::Both);
    assert_eq!(
        PeerSignal::SameEgress.direction(),
        Direction::ServerToClient
    );
    assert_eq!(
        PeerSignal::Bye.direction(),
        Direction::Both,
        "plain peer signals stay bidirectional"
    );
}

// ---- frozen business error codes ----------------------------------------------

#[test]
fn business_error_code_constants_frozen() {
    assert_eq!(v2::PAIR_UNAVAILABLE, 0x2101);
    assert_eq!(v2::RATE_LIMITED, 0x2102);
    assert_eq!(v2::RESOURCE_LIMIT, 0x2103);
    assert_eq!(v2::REQUEST_CONFLICT, 0x2104);
    assert_eq!(v2::CODE_CONFLICT, 0x2105);
    assert_eq!(v2::REVISION_CONFLICT, 0x2106);
    assert_eq!(v2::PROTOCOL_UNSUPPORTED, 0x2107);
    assert_eq!(v2::STORAGE_UNAVAILABLE, 0x2108);
    // They ride PairError.code verbatim.
    roundtrip(CtrlMessage::PairError {
        request_id: [1; 16],
        scope: ErrorScope::Code,
        object_id: [0; 16],
        code: v2::CODE_CONFLICT,
        retry_after_ms: 0,
    });
}

// ---- wave-28 endpoint extension -------------------------------------------

/// Frame-level round-trip through the plain and direction-aware paths.
fn roundtrip_frame(frame: CtrlFrame) {
    let mut bytes = Vec::new();
    frame.try_encode(&mut bytes).unwrap();
    assert!(bytes.len() <= MAX_FRAME_BYTES);
    assert_eq!(bytes[0], VERSION);
    assert_eq!(CtrlFrame::decode(&bytes).unwrap(), frame);
    let sender = if frame.message.allows_sender(Role::Client) {
        Role::Client
    } else {
        Role::Server
    };
    let mut directed = Vec::new();
    frame.try_encode_as(sender, &mut directed).unwrap();
    assert_eq!(directed, bytes);
    assert_eq!(
        CtrlFrame::decode_as(&bytes, sender.opposite()).unwrap(),
        frame
    );
}

fn sample_blob(seed: u8, len: usize) -> EndpointBlob {
    EndpointBlob(vec![seed; len])
}

#[test]
fn endpoint_extension_roundtrips_on_all_four_tags() {
    let blob = sample_blob(0x5A, 300);
    // Claim: endpoint + cap (owner side, client to server).
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::InviteClaim {
            request_id: [1; 16],
            lookup_digest: [2; 32],
            ttl_secs: 0,
        })
        .with_endpoint(blob.clone())
        .with_max_redeems(1),
    );
    // Redeem: endpoint (redeemer side).
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::InviteRedeem(redeem(0x33))).with_endpoint(blob.clone()),
    );
    // Offered: endpoint relayed to the owner.
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
            code_id: [3; 16],
            code_revision: 7,
            pairing_id: [4; 16],
            redeemer_device_id: [5; 32],
            redeem: redeem(0x34),
        })
        .with_endpoint(blob.clone()),
    );
    // Commit: owner endpoint down to the redeemer.
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::PairCommit {
            transcript: [0xEE; v2::message::TRANSCRIPT_BYTES],
            owner_proof: proof(1),
            redeemer_proof: proof(2),
            owner_prepared_signature: [3; 64],
            redeemer_prepared_signature: [4; 64],
            commit_revision: 9,
        })
        .with_endpoint(blob.clone()),
    );
    // Cap without endpoint: flags = 0x02 only.
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::InviteClaim {
            request_id: [1; 16],
            lookup_digest: [2; 32],
            ttl_secs: 60,
        })
        .with_max_redeems(0), // 0 = explicitly unlimited
    );
}

/// No extension fields ⇒ bytes identical to the frozen legacy shape, and
/// legacy payloads decode with both extension slots empty (old peers and
/// old frames interoperate unchanged).
#[test]
fn extension_absent_is_byte_identical_legacy_shape() {
    for message in [
        CtrlMessage::InviteClaim {
            request_id: [1; 16],
            lookup_digest: [2; 32],
            ttl_secs: 0,
        },
        CtrlMessage::InviteRedeem(redeem(0x10)),
        CtrlMessage::InviteRedeemOffered {
            code_id: [3; 16],
            code_revision: 1,
            pairing_id: [4; 16],
            redeemer_device_id: [5; 32],
            redeem: redeem(0x11),
        },
        CtrlMessage::PairCommit {
            transcript: [0x00; v2::message::TRANSCRIPT_BYTES],
            owner_proof: proof(1),
            redeemer_proof: proof(2),
            owner_prepared_signature: [3; 64],
            redeemer_prepared_signature: [4; 64],
            commit_revision: 1,
        },
    ] {
        let legacy = encode(&message); // CtrlFrame::new: no extension
        let frame = CtrlFrame::decode(&legacy).unwrap();
        assert!(frame.endpoint.is_none());
        assert!(frame.max_redeems.is_none());
        // Re-encoding the decoded frame reproduces the same bytes.
        let mut again = Vec::new();
        frame.try_encode(&mut again).unwrap();
        assert_eq!(again, legacy);
    }
}

/// Byte-exact section layout: flags byte, u16 BE length, blob, u32 BE cap.
#[test]
fn extension_section_wire_layout_byte_exact() {
    let blob_bytes: Vec<u8> = (0..77u16).map(|i| (i % 251) as u8).collect();
    let frame = CtrlFrame::new(CtrlMessage::InviteClaim {
        request_id: [0x11; 16],
        lookup_digest: [0x22; 32],
        ttl_secs: 86_400,
    })
    .with_endpoint(EndpointBlob(blob_bytes.clone()))
    .with_max_redeems(0x0102_0304);
    let bytes = {
        let mut out = Vec::new();
        frame.try_encode(&mut out).unwrap();
        out
    };
    let payload = &bytes[HEADER_LEN..];
    // 52 frozen bytes, then flags 0x03, len 77 BE, blob, cap BE.
    assert_eq!(payload.len(), 52 + 1 + 2 + 77 + 4);
    assert_eq!(payload[52], EXT_ENDPOINT_FLAG | EXT_MAX_REDEEMS_FLAG);
    assert_eq!(&payload[53..55], &(77u16).to_be_bytes());
    assert_eq!(&payload[55..55 + 77], &blob_bytes[..]);
    assert_eq!(&payload[55 + 77..], &0x0102_0304u32.to_be_bytes());

    // Offered: 256 frozen bytes, flags 0x01, len, blob.
    let frame = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
        code_id: [3; 16],
        code_revision: 1,
        pairing_id: [4; 16],
        redeemer_device_id: [5; 32],
        redeem: redeem(0x12),
    })
    .with_endpoint(EndpointBlob(blob_bytes.clone()));
    let mut out = Vec::new();
    frame.try_encode(&mut out).unwrap();
    let payload = &out[HEADER_LEN..];
    assert_eq!(payload.len(), 256 + 1 + 2 + 77);
    assert_eq!(payload[256], EXT_ENDPOINT_FLAG);
    assert_eq!(&payload[257..259], &(77u16).to_be_bytes());
    assert_eq!(&payload[259..], &blob_bytes[..]);
}

#[test]
fn err_endpoint_blob_budget() {
    let over = sample_blob(0x61, ENDPOINT_BLOB_MAX_BYTES + 1);
    // Encode: mirrored budget validation.
    for message in [
        CtrlMessage::InviteClaim {
            request_id: [1; 16],
            lookup_digest: [2; 32],
            ttl_secs: 0,
        },
        CtrlMessage::InviteRedeem(redeem(0x40)),
    ] {
        assert!(matches!(
            CtrlFrame::new(message.clone())
                .with_endpoint(over.clone())
                .try_encode(&mut Vec::new()),
            Err(EncodeError::EndpointBlobSize { len: 513, limit: 512 })
        ));
    }
    // Empty blob is not a legal field either.
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::InviteRedeem(redeem(0x41)))
            .with_endpoint(EndpointBlob(Vec::new()))
            .try_encode(&mut Vec::new()),
        Err(EncodeError::EndpointBlobSize { len: 0, .. })
    ));
    // Decode: declared over-budget and zero lengths are refused before
    // any allocation.
    for len in [0u16, (ENDPOINT_BLOB_MAX_BYTES + 1) as u16] {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]); // request_id
        payload.extend_from_slice(&[0u8; 32]); // lookup_digest
        payload.extend_from_slice(&4u32.to_be_bytes()); // ttl
        payload.push(EXT_ENDPOINT_FLAG);
        payload.extend_from_slice(&len.to_be_bytes());
        assert!(matches!(
            CtrlFrame::decode(&wrap(0x0A, &payload)),
            Err(DecodeError::EndpointBlobSize { .. })
        ));
    }
    // Exactly-full 512-byte blob passes.
    let full = sample_blob(0x62, ENDPOINT_BLOB_MAX_BYTES);
    roundtrip_frame(
        CtrlFrame::new(CtrlMessage::InviteRedeem(redeem(0x42))).with_endpoint(full),
    );
}

#[test]
fn err_extension_flags_and_tag_rules() {
    // Encode: extension fields are refused on non-extended tags.
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::Ping { nonce: [1; 16] })
            .with_endpoint(sample_blob(1, 16))
            .try_encode(&mut Vec::new()),
        Err(EncodeError::BadExtension { ty: 0x1D })
    ));
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
            code_id: [3; 16],
            code_revision: 1,
            pairing_id: [4; 16],
            redeemer_device_id: [5; 32],
            redeem: redeem(0x50),
        })
        .with_max_redeems(1)
        .try_encode(&mut Vec::new()),
        Err(EncodeError::BadExtension { ty: 0x12 })
    ));
    assert!(matches!(
        CtrlFrame::new(CtrlMessage::InviteRedeem(redeem(0x51)))
            .with_max_redeems(1)
            .try_encode(&mut Vec::new()),
        Err(EncodeError::BadExtension { ty: 0x10 })
    ));
    // Decode: undefined flag bits and the empty section are refused.
    for flags in [0x00u8, 0x04, 0x03 | 0x08, 0xFF] {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 16]);
        payload.extend_from_slice(&[0u8; 32]);
        payload.extend_from_slice(&4u32.to_be_bytes());
        payload.push(flags);
        if flags & EXT_ENDPOINT_FLAG != 0 {
            payload.extend_from_slice(&2u16.to_be_bytes());
            payload.extend_from_slice(&[0xAA, 0xBB]);
        }
        if flags & EXT_MAX_REDEEMS_FLAG != 0 {
            payload.extend_from_slice(&1u32.to_be_bytes());
        }
        assert!(
            matches!(
                CtrlFrame::decode(&wrap(0x0A, &payload)),
                Err(DecodeError::BadExtensionFlags(f)) if f == flags
            ),
            "flags {flags:#04x} must be refused"
        );
    }
    // Decode: the cap flag is illegal off the claim frame.
    let request = redeem(0x52);
    let mut payload = Vec::new();
    payload.extend_from_slice(&request.request_id);
    payload.extend_from_slice(&request.lookup_digest);
    payload.extend_from_slice(&request.redeemer_nonce);
    payload.extend_from_slice(&request.deadline_ms.to_be_bytes());
    payload.extend_from_slice(&request.request_mac);
    payload.extend_from_slice(&request.signature);
    payload.push(EXT_MAX_REDEEMS_FLAG);
    payload.extend_from_slice(&1u32.to_be_bytes());
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x10, &payload)),
        Err(DecodeError::BadExtension { ty: 0x10 })
    ));
    // Leftover bytes inside the section are trailing garbage.
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0u8; 16]);
    payload.extend_from_slice(&[0u8; 32]);
    payload.extend_from_slice(&4u32.to_be_bytes());
    payload.push(EXT_ENDPOINT_FLAG);
    payload.extend_from_slice(&2u16.to_be_bytes());
    payload.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
    assert!(matches!(
        CtrlFrame::decode(&wrap(0x0A, &payload)),
        Err(DecodeError::Trailing(1))
    ));
}
