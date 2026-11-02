//! Minimal STUN (RFC 5389 subset): binding request/response over UDP.
//!
//! Purpose: let a device learn its public mapped address using its own
//! transfer socket, and let the coordination server answer bindings. No
//! TURN, no relaying — STUN here never touches file data.
#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;

use crate::util::{put_u16, put_u32, unique_array};

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
pub const HEADER_LEN: usize = 20;
pub const BINDING_REQUEST: u16 = 0x0001;
pub const BINDING_RESPONSE: u16 = 0x0101;
pub const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
pub const MAPPED_ADDRESS: u16 = 0x0001;

const FAMILY_IPV4: u8 = 0x01;
const FAMILY_IPV6: u8 = 0x02;
/// RFC 5389: port is xored with the top 16 bits of the magic cookie.
const PORT_XOR_MASK: u16 = (MAGIC_COOKIE >> 16) as u16;

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
    let mut out = Vec::with_capacity(HEADER_LEN);
    put_u16(&mut out, BINDING_REQUEST);
    put_u16(&mut out, 0); // no attributes
    put_u32(&mut out, MAGIC_COOKIE);
    out.extend_from_slice(&transaction_id);
    out
}

/// Build a binding response echoing the transaction id with the mapped address.
pub fn binding_response(transaction_id: [u8; 12], mapped: SocketAddr) -> Vec<u8> {
    let mut attributes = Vec::new();
    // XOR-MAPPED-ADDRESS attribute.
    put_u16(&mut attributes, ATTR_XOR_MAPPED_ADDRESS);
    match mapped {
        SocketAddr::V4(v4) => {
            put_u16(&mut attributes, 8);
            attributes.push(0); // reserved
            attributes.push(FAMILY_IPV4);
            put_u16(&mut attributes, v4.port() ^ PORT_XOR_MASK);
            let cookie = MAGIC_COOKIE.to_be_bytes();
            for (i, byte) in v4.ip().octets().iter().enumerate() {
                attributes.push(byte ^ cookie[i]);
            }
        }
        SocketAddr::V6(v6) => {
            put_u16(&mut attributes, 20);
            attributes.push(0); // reserved
            attributes.push(FAMILY_IPV6);
            put_u16(&mut attributes, v6.port() ^ PORT_XOR_MASK);
            // IPv6 xors with cookie || transaction id (16 mask bytes).
            let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
            mask.extend_from_slice(&transaction_id);
            for (i, byte) in v6.ip().octets().iter().enumerate() {
                attributes.push(byte ^ mask[i]);
            }
        }
    }
    let mut out = Vec::with_capacity(HEADER_LEN + attributes.len());
    put_u16(&mut out, BINDING_RESPONSE);
    put_u16(&mut out, attributes.len() as u16);
    put_u32(&mut out, MAGIC_COOKIE);
    out.extend_from_slice(&transaction_id);
    out.extend_from_slice(&attributes);
    out
}

/// Parse a received datagram as a binding request, returning its transaction id.
pub fn parse_binding_request(packet: &[u8]) -> Result<[u8; 12], StunError> {
    if packet.len() < HEADER_LEN {
        return Err(StunError::TooShort);
    }
    let msg_type = u16::from_be_bytes([packet[0], packet[1]]);
    if msg_type != BINDING_REQUEST {
        return Err(StunError::WrongType(msg_type));
    }
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if cookie != MAGIC_COOKIE {
        return Err(StunError::BadCookie);
    }
    let attr_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if HEADER_LEN + attr_len != packet.len() {
        return Err(StunError::Malformed);
    }
    let mut transaction_id = [0u8; 12];
    transaction_id.copy_from_slice(&packet[8..20]);
    Ok(transaction_id)
}

/// Parse a binding response, verifying cookie and transaction id, extracting
/// the XOR-mapped address.
pub fn parse_binding_response(
    packet: &[u8],
    transaction_id: [u8; 12],
) -> Result<SocketAddr, StunError> {
    if packet.len() < HEADER_LEN {
        return Err(StunError::TooShort);
    }
    let msg_type = u16::from_be_bytes([packet[0], packet[1]]);
    if msg_type != BINDING_RESPONSE {
        return Err(StunError::WrongType(msg_type));
    }
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if cookie != MAGIC_COOKIE {
        return Err(StunError::BadCookie);
    }
    if packet[8..20] != transaction_id {
        return Err(StunError::TransactionMismatch);
    }
    let body_end =
        (HEADER_LEN + u16::from_be_bytes([packet[2], packet[3]]) as usize).min(packet.len());
    let mut xor_mapped: Option<SocketAddr> = None;
    let mut plain_mapped: Option<SocketAddr> = None;
    let mut pos = HEADER_LEN;
    while pos + 4 <= body_end {
        let attr_type = u16::from_be_bytes([packet[pos], packet[pos + 1]]);
        let attr_len = u16::from_be_bytes([packet[pos + 2], packet[pos + 3]]) as usize;
        let value_start = pos + 4;
        if value_start + attr_len > body_end {
            return Err(StunError::Malformed);
        }
        let value = &packet[value_start..value_start + attr_len];
        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS if xor_mapped.is_none() => {
                xor_mapped = Some(decode_xor_mapped(value, &transaction_id)?);
            }
            MAPPED_ADDRESS if plain_mapped.is_none() => {
                plain_mapped = decode_plain_mapped(value);
            }
            _ => {}
        }
        // Attributes are padded to 4-byte boundaries.
        pos += (attr_len + 4 + 3) & !3;
    }
    xor_mapped
        .or(plain_mapped)
        .ok_or(StunError::NoMappedAddress)
}

fn decode_xor_mapped(value: &[u8], transaction_id: &[u8; 12]) -> Result<SocketAddr, StunError> {
    if value.len() < 8 {
        return Err(StunError::Malformed);
    }
    let port = u16::from_be_bytes([value[2], value[3]]) ^ PORT_XOR_MASK;
    match value[1] {
        FAMILY_IPV4 => {
            if value.len() < 8 {
                return Err(StunError::Malformed);
            }
            let cookie = MAGIC_COOKIE.to_be_bytes();
            let mut octets = [0u8; 4];
            for i in 0..4 {
                octets[i] = value[4 + i] ^ cookie[i];
            }
            Ok(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(octets)),
                port,
            ))
        }
        FAMILY_IPV6 => {
            if value.len() < 20 {
                return Err(StunError::Malformed);
            }
            let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
            mask.extend_from_slice(transaction_id);
            let mut octets = [0u8; 16];
            for i in 0..16 {
                octets[i] = value[4 + i] ^ mask[i];
            }
            Ok(SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets)),
                port,
            ))
        }
        _ => Err(StunError::Malformed),
    }
}

fn decode_plain_mapped(value: &[u8]) -> Option<SocketAddr> {
    if value.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([value[2], value[3]]);
    match value[1] {
        FAMILY_IPV4 if value.len() >= 8 => {
            let mut octets = [0u8; 4];
            octets.copy_from_slice(&value[4..8]);
            Some(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(octets)),
                port,
            ))
        }
        FAMILY_IPV6 if value.len() >= 20 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&value[4..20]);
            Some(SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets)),
                port,
            ))
        }
        _ => None,
    }
}

/// Query a STUN server from a socket the caller owns (the transfer socket, so
/// the mapped address applies to it). Retransmits until timeout.
pub async fn query_on(
    socket: &UdpSocket,
    server: SocketAddr,
    timeout: Duration,
) -> Result<SocketAddr, StunError> {
    let transaction_id = unique_array::<12>();
    let request = binding_request(transaction_id);
    let start = std::time::Instant::now();
    let mut buf = vec![0u8; 128];
    let mut attempt = 0usize;
    loop {
        socket.send_to(&request, server).await?;
        // Retransmission schedule: 1s / 2s / 4s, then every 4s, all bounded
        // by the overall timeout.
        let backoff = match attempt {
            0 => Duration::from_secs(1),
            1 => Duration::from_secs(2),
            _ => Duration::from_secs(4),
        };
        attempt += 1;
        let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
            return Err(StunError::Timeout(timeout));
        };
        let window = backoff.min(remaining);
        match tokio::time::timeout(window, socket.recv_from(&mut buf)).await {
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok((len, _from))) => {
                if let Ok(mapped) = parse_binding_response(&buf[..len], transaction_id) {
                    return Ok(mapped);
                }
                // Unrelated datagram on the same port: keep waiting.
            }
            Err(_) => {
                // Window elapsed without a usable response: retransmit.
            }
        }
        if start.elapsed() >= timeout {
            return Err(StunError::Timeout(timeout));
        }
    }
}

/// Convenience wrapper: bind an ephemeral UDP socket and query once.
pub async fn query(
    server: SocketAddr,
    timeout: Duration,
) -> Result<(SocketAddr, SocketAddr), StunError> {
    let bind_addr: &str = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr).await?;
    let local = socket.local_addr()?;
    let mapped = query_on(&socket, server, timeout).await?;
    Ok((local, mapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv6Addr, SocketAddrV6};

    const TX: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    #[test]
    fn request_layout_is_exact() {
        let packet = binding_request(TX);
        assert_eq!(packet.len(), HEADER_LEN);
        assert_eq!(&packet[0..2], &[0x00, 0x01]);
        assert_eq!(&packet[2..4], &[0x00, 0x00]);
        assert_eq!(&packet[4..8], &[0x21, 0x12, 0xA4, 0x42]);
        assert_eq!(&packet[8..20], &TX);
    }

    #[test]
    fn request_parse_roundtrip() {
        assert_eq!(parse_binding_request(&binding_request(TX)).unwrap(), TX);
    }

    #[test]
    fn request_rejects_malformed() {
        let packet = binding_request(TX);
        assert!(matches!(
            parse_binding_request(&packet[..19]),
            Err(StunError::TooShort)
        ));
        let mut wrong_type = packet.clone();
        wrong_type[0] = 0x01; // 0x0101 is a response type, not a request
        assert!(matches!(
            parse_binding_request(&wrong_type),
            Err(StunError::WrongType(0x0101))
        ));
        let mut bad_cookie = packet.clone();
        bad_cookie[5] = 0x00;
        assert!(matches!(
            parse_binding_request(&bad_cookie),
            Err(StunError::BadCookie)
        ));
        let mut bad_len = packet;
        bad_len[3] = 4; // claims 4 attribute bytes that are not there
        assert!(matches!(
            parse_binding_request(&bad_len),
            Err(StunError::Malformed)
        ));
    }

    #[test]
    fn response_v4_bytes_are_exact_and_roundtrip() {
        let mapped: SocketAddr = "203.0.113.7:45047".parse().unwrap();
        let xport = (45047u16 ^ 0x2112).to_be_bytes();
        let cookie = [0x21u8, 0x12, 0xA4, 0x42];
        let ip = [203u8, 0, 113, 7];
        let mut xaddr = [0u8; 4];
        for i in 0..4 {
            xaddr[i] = ip[i] ^ cookie[i];
        }
        let mut expected = vec![0x01, 0x01, 0x00, 0x0C];
        expected.extend_from_slice(&cookie);
        expected.extend_from_slice(&TX);
        expected.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        expected.extend_from_slice(&xport);
        expected.extend_from_slice(&xaddr);
        assert_eq!(binding_response(TX, mapped), expected);
        assert_eq!(parse_binding_response(&expected, TX).unwrap(), mapped);
    }

    #[test]
    fn response_v6_xor_encoding_is_correct() {
        let mapped = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(
                0x2001, 0xdb8, 0x1234, 0x5678, 0x9abc, 0xdef0, 0x1122, 0x3344,
            ),
            44444,
            0,
            0,
        ));
        let packet = binding_response(TX, mapped);
        assert_eq!(packet.len(), HEADER_LEN + 24);
        let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
        mask.extend_from_slice(&TX);
        // Attribute header at 20, value at 24: [0][family][xport][xaddr(16)].
        assert_eq!(packet[24], 0);
        assert_eq!(packet[25], FAMILY_IPV6);
        assert_eq!(u16::from_be_bytes([packet[26], packet[27]]), 44444 ^ 0x2112);
        let octets = match mapped {
            SocketAddr::V6(v6) => v6.ip().octets(),
            SocketAddr::V4(_) => unreachable!("test constructs v6"),
        };
        for i in 0..16 {
            assert_eq!(packet[28 + i], octets[i] ^ mask[i]);
        }
        assert_eq!(parse_binding_response(&packet, TX).unwrap(), mapped);
    }

    #[test]
    fn response_rejects_malformed() {
        let packet = binding_response(TX, "203.0.113.7:45047".parse().unwrap());
        assert!(matches!(
            parse_binding_response(&packet[..19], TX),
            Err(StunError::TooShort)
        ));
        let mut wrong_type = packet.clone();
        wrong_type[1] = 0x00; // 0x0100 is not handled
        assert!(matches!(
            parse_binding_response(&wrong_type, TX),
            Err(StunError::WrongType(0x0100))
        ));
        let mut bad_cookie = packet.clone();
        bad_cookie[6] = 0x00;
        assert!(matches!(
            parse_binding_response(&bad_cookie, TX),
            Err(StunError::BadCookie)
        ));
        assert!(matches!(
            parse_binding_response(&packet, [9u8; 12]),
            Err(StunError::TransactionMismatch)
        ));
    }

    #[test]
    fn response_without_mapped_address_fails() {
        let mut packet = vec![0x01, 0x01, 0x00, 0x04];
        packet.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        packet.extend_from_slice(&TX);
        packet.extend_from_slice(&[0x80, 0x28, 0x00, 0x00]); // FINGERPRINT-shaped dummy
        assert!(matches!(
            parse_binding_response(&packet, TX),
            Err(StunError::NoMappedAddress)
        ));
    }

    #[test]
    fn response_accepts_plain_mapped_address_fallback() {
        let mapped: SocketAddr = "192.0.2.50:7777".parse().unwrap();
        let mut packet = vec![0x01, 0x01, 0x00, 0x0C];
        packet.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        packet.extend_from_slice(&TX);
        packet.extend_from_slice(&[0x00, 0x01, 0x00, 0x08, 0x00, 0x01]);
        packet.extend_from_slice(&7777u16.to_be_bytes());
        packet.extend_from_slice(&[192, 0, 2, 50]);
        assert_eq!(parse_binding_response(&packet, TX).unwrap(), mapped);
    }

    #[test]
    fn response_skips_unrelated_padded_attributes() {
        let mut packet = binding_response(TX, "203.0.113.7:45047".parse().unwrap());
        // Insert a dummy attribute (len 5, padded to 8) before XOR-MAPPED.
        let mut with_dummy = vec![0x01, 0x01, 0x00, 0x00];
        with_dummy.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        with_dummy.extend_from_slice(&TX);
        with_dummy.extend_from_slice(&[0xAB, 0xCD, 0x00, 0x05, 1, 2, 3, 4, 5, 0, 0, 0]);
        with_dummy.extend_from_slice(&packet[20..]);
        let total = with_dummy.len() - HEADER_LEN;
        with_dummy[2] = (total as u16).to_be_bytes()[0];
        with_dummy[3] = (total as u16).to_be_bytes()[1];
        packet = with_dummy;
        assert_eq!(
            parse_binding_response(&packet, TX).unwrap(),
            "203.0.113.7:45047".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn response_rejects_attribute_overrun() {
        let mut packet = vec![0x01, 0x01, 0x00, 0x08];
        packet.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        packet.extend_from_slice(&TX);
        // Attribute claims 8 value bytes but only 4 are present.
        packet.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x11, 0x22]);
        assert!(matches!(
            parse_binding_response(&packet, TX),
            Err(StunError::Malformed)
        ));
    }

    #[tokio::test]
    async fn query_on_against_local_responder() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = responder.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let fake_mapped: SocketAddr = "198.51.100.9:55555".parse().unwrap();
        let responder_task = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (len, from) = responder.recv_from(&mut buf).await.unwrap();
            let txid = parse_binding_request(&buf[..len]).unwrap();
            let reply = binding_response(txid, fake_mapped);
            responder.send_to(&reply, from).await.unwrap();
        });
        let got = query_on(&client, server_addr, Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(got, fake_mapped);
        responder_task.await.unwrap();
    }

    #[tokio::test]
    async fn query_on_times_out_when_silent() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap(); // never answers
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let result = query_on(
            &client,
            silent.local_addr().unwrap(),
            Duration::from_millis(300),
        )
        .await;
        assert!(matches!(result, Err(StunError::Timeout(t)) if t == Duration::from_millis(300)));
    }

    #[tokio::test]
    async fn query_returns_local_and_mapped() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = responder.local_addr().unwrap();
        let fake_mapped: SocketAddr = "198.51.100.20:40000".parse().unwrap();
        let responder_task = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (len, from) = responder.recv_from(&mut buf).await.unwrap();
            let txid = parse_binding_request(&buf[..len]).unwrap();
            let reply = binding_response(txid, fake_mapped);
            responder.send_to(&reply, from).await.unwrap();
        });
        let (local, mapped) = query(server_addr, Duration::from_secs(3)).await.unwrap();
        assert!(local.is_ipv4());
        assert_ne!(local.port(), 0);
        assert_eq!(mapped, fake_mapped);
        responder_task.await.unwrap();
    }
}
