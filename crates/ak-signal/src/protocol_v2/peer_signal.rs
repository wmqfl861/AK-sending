//! v2 `PeerSignal`: the four sub-tagged payloads relayed verbatim inside
//! `CtrlMessage::Signal`. The pairing feature adds **no** invite or generic
//! authentication payload here — pairing material travels in its own
//! explicit `CtrlMessage` variants (contract A.4.5).
//!
//! Closed set: sub-tags `0x01..=0x04`; every other tag byte is rejected and
//! tests enumerate all 256 values.

use std::net::SocketAddr;

use super::frame::{DecodeError, EncodeError};
use super::wire::{self, Cur};

/// Candidates: sub-tag + count + up to 16 addresses.
const TAG_CANDIDATES: u8 = 0x01;
/// Punch timing: sub-tag + u64 epoch + 8-byte nonce.
const TAG_PUNCH_AT: u8 = 0x02;
/// Session teardown notice: sub-tag only.
const TAG_BYE: u8 = 0x03;
/// Server-generated same-egress ordering hint: sub-tag only.
const TAG_SAME_EGRESS: u8 = 0x04;

/// Largest encoded `PeerSignal` including its sub-tag:
/// 1 (tag) + 1 (count) + 16 * 19 (largest, IPv6) = 306 bytes.
pub const MAX_ENCODED_BYTES: usize = 306;

/// Structured peer-to-peer signal payloads relayed by the server verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSignal {
    /// Connection candidates, LAN and public, IP:port only; at most 16.
    Candidates(Vec<SocketAddr>),
    /// Agreed simultaneous-open epoch (ms) plus a nonce to match probes.
    PunchAt {
        epoch_unix_ms: u64,
        nonce: [u8; 8],
    },
    Bye,
    /// Server-generated hint: both session endpoints were observed on the
    /// same control-plane TCP peer IP, so they likely share one egress/NAT
    /// and should try LAN candidates first. Carries no data, proves
    /// nothing about the LAN, and never originates from a client (locked
    /// by the direction rules in [`super::direction`]).
    SameEgress,
}

pub(super) fn encode(signal: &PeerSignal, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    match signal {
        PeerSignal::Candidates(candidates) => {
            out.push(TAG_CANDIDATES);
            wire::put_candidates_checked(out, candidates)?;
        }
        PeerSignal::PunchAt {
            epoch_unix_ms,
            nonce,
        } => {
            out.push(TAG_PUNCH_AT);
            crate::util::put_u64(out, *epoch_unix_ms);
            out.extend_from_slice(nonce);
        }
        PeerSignal::Bye => out.push(TAG_BYE),
        PeerSignal::SameEgress => out.push(TAG_SAME_EGRESS),
    }
    Ok(())
}

pub(super) fn decode(cur: &mut Cur<'_>) -> Result<PeerSignal, DecodeError> {
    match cur.u8()? {
        TAG_CANDIDATES => Ok(PeerSignal::Candidates(wire::get_candidates(cur)?)),
        TAG_PUNCH_AT => Ok(PeerSignal::PunchAt {
            epoch_unix_ms: cur.u64()?,
            nonce: cur.fixed::<8>()?,
        }),
        TAG_BYE => Ok(PeerSignal::Bye),
        TAG_SAME_EGRESS => Ok(PeerSignal::SameEgress),
        _ => Err(DecodeError::Malformed("signal tag")),
    }
}
