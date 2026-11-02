//! Minimal STUN (RFC 5389 subset): binding request/response over UDP.
//!
//! Purpose: let a device learn its public mapped address using its own
//! transfer socket, and let the coordination server answer bindings. No
//! TURN, no relaying — STUN here never touches file data.
#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
pub const HEADER_LEN: usize = 20;
pub const BINDING_REQUEST: u16 = 0x0001;
pub const BINDING_RESPONSE: u16 = 0x0101;
pub const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
pub const MAPPED_ADDRESS: u16 = 0x0001;

#[derive(Debug, thiserror::Error)]
pub enum StunError {
    #[error("packet too short for a STUN header")]
    TooShort,
    #[error("magic cookie mismatch")]
    BadCookie,
    #[error("transaction id mismatch")]
    TransactionMismatch,
    #[error("message type {0:#06x} not handled")]
    WrongType(u16),
    #[error("no XOR-MAPPED-ADDRESS attribute")]
    NoMappedAddress,
    #[error("malformed attribute")]
    Malformed,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("no response within {0:?}")]
    Timeout(Duration),
}

/// Build a binding request with a caller-chosen transaction id.
pub fn binding_request(transaction_id: [u8; 12]) -> Vec<u8> {
    todo!("owned by ak-signal agent")
}

/// Build a binding response echoing the transaction id with the mapped address.
pub fn binding_response(transaction_id: [u8; 12], mapped: SocketAddr) -> Vec<u8> {
    todo!("owned by ak-signal agent")
}

/// Parse a received datagram as a binding request, returning its transaction id.
pub fn parse_binding_request(packet: &[u8]) -> Result<[u8; 12], StunError> {
    todo!("owned by ak-signal agent")
}

/// Parse a binding response, verifying cookie and transaction id, extracting
/// the XOR-mapped address.
pub fn parse_binding_response(packet: &[u8], transaction_id: [u8; 12]) -> Result<SocketAddr, StunError> {
    todo!("owned by ak-signal agent")
}

/// Query a STUN server from a socket the caller owns (the transfer socket, so
/// the mapped address applies to it). Retransmits until timeout.
pub async fn query_on(
    socket: &tokio::net::UdpSocket,
    server: SocketAddr,
    timeout: Duration,
) -> Result<SocketAddr, StunError> {
    todo!("owned by ak-signal agent")
}

/// Convenience wrapper: bind an ephemeral UDP socket and query once.
pub async fn query(server: SocketAddr, timeout: Duration) -> Result<(SocketAddr, SocketAddr), StunError> {
    todo!("owned by ak-signal agent (returns (local, mapped))")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-signal agent")
    fn placeholder() {}
}
