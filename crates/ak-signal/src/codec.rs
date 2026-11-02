//! Signaling wire codec: fixed message set, every field bounded.
//!
//! Frame layout: `[version u8][type u8][payload_len u16 BE][payload]`.
//! Hard cap 4096 bytes per frame. Candidates are `SocketAddr`s only; the
//! payload types have no byte-blob field by construction.
#![forbid(unsafe_code)]

use std::net::SocketAddr;

use ak_identity::{DeviceKeyPair, Fingerprint};

use crate::util::{put_u16, put_u32, put_u64};

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 4;
pub const MAX_FRAME_BYTES: usize = 4096;
pub const MAX_CANDIDATES: usize = 16;
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_ERROR_BYTES: usize = 128;
pub const HELLO_MAX_AGE_MS: u64 = 10_000;

pub type DeviceId = [u8; 32];
pub type SessionId = [u8; 16];

// Wire type values. The set is closed: decode rejects everything outside
// Hello..=ServerError and a test locks the mapping.
const TYPE_HELLO: u8 = 1;
const TYPE_WELCOME: u8 = 2;
const TYPE_REGISTER: u8 = 3;
const TYPE_CONNECT_REQUEST: u8 = 4;
const TYPE_SESSION_OPENED: u8 = 5;
const TYPE_SIGNAL: u8 = 6;
const TYPE_CANCEL: u8 = 7;
const TYPE_SERVER_ERROR: u8 = 8;

// PeerSignal three-state tag bytes.
const TAG_CANDIDATES: u8 = 1;
const TAG_PUNCH_AT: u8 = 2;
const TAG_BYE: u8 = 3;

/// Structured peer-to-peer signal payloads relayed by the server verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSignal {
    /// Connection candidates, LAN and public, IP:port only.
    Candidates(Vec<SocketAddr>),
    /// Agreed simultaneous-open epoch (ms) plus a nonce to match probes.
    PunchAt {
        epoch_unix_ms: u64,
        nonce: [u8; 8],
    },
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
    Welcome {
        session_ttl_secs: u32,
    },
    /// Replace this device's published candidates.
    Register {
        candidates: Vec<SocketAddr>,
    },
    /// Ask the server to open (or reuse) a session toward `target`.
    ConnectRequest {
        target: DeviceId,
    },
    /// Sent to both endpoints when a session exists.
    SessionOpened {
        session: SessionId,
        peer: DeviceId,
    },
    /// Relay one bounded peer signal within a session.
    Signal {
        session: SessionId,
        signal: PeerSignal,
    },
    Cancel {
        session: SessionId,
    },
    ServerError {
        code: u16,
        message: String,
    },
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
    #[error("hello timestamp age {age_ms}ms exceeds {max_ms}ms")]
    Freshness { age_ms: u64, max_ms: u64 },
}

impl CtrlFrame {
    /// Append the frame to `out`. Infallible by contract: all limits are
    /// enforced on decode, and callers constructing absurd fields only
    /// produce frames that decode rejects (lengths saturate instead of
    /// wrapping).
    pub fn encode(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.push(self.version);
        out.push(self.message.type_byte());
        out.push(0);
        out.push(0);
        encode_payload(&self.message, out);
        let payload_len = (out.len() - start - HEADER_LEN).min(u16::MAX as usize);
        let len_be = (payload_len as u16).to_be_bytes();
        out[start + 2] = len_be[0];
        out[start + 3] = len_be[1];
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader);
        }
        let version = bytes[0];
        if version != VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let ty = bytes[1];
        if !(TYPE_HELLO..=TYPE_SERVER_ERROR).contains(&ty) {
            return Err(DecodeError::UnknownType(ty));
        }
        let declared = u16::from_be_bytes([bytes[2], bytes[3]]);
        if HEADER_LEN + declared as usize > MAX_FRAME_BYTES {
            return Err(DecodeError::Oversized {
                declared,
                cap: (MAX_FRAME_BYTES - HEADER_LEN) as u16,
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
        let message = decode_payload(ty, &bytes[HEADER_LEN..end])?;
        Ok(CtrlFrame { version, message })
    }
}

impl CtrlMessage {
    fn type_byte(&self) -> u8 {
        match self {
            CtrlMessage::Hello { .. } => TYPE_HELLO,
            CtrlMessage::Welcome { .. } => TYPE_WELCOME,
            CtrlMessage::Register { .. } => TYPE_REGISTER,
            CtrlMessage::ConnectRequest { .. } => TYPE_CONNECT_REQUEST,
            CtrlMessage::SessionOpened { .. } => TYPE_SESSION_OPENED,
            CtrlMessage::Signal { .. } => TYPE_SIGNAL,
            CtrlMessage::Cancel { .. } => TYPE_CANCEL,
            CtrlMessage::ServerError { .. } => TYPE_SERVER_ERROR,
        }
    }
}

fn encode_payload(message: &CtrlMessage, out: &mut Vec<u8>) {
    match message {
        CtrlMessage::Hello {
            version,
            device_id,
            name,
            timestamp_unix_ms,
            signature,
        } => {
            put_u16(out, *version);
            out.extend_from_slice(device_id);
            put_str(out, name);
            put_u64(out, *timestamp_unix_ms);
            out.extend_from_slice(signature);
        }
        CtrlMessage::Welcome { session_ttl_secs } => put_u32(out, *session_ttl_secs),
        CtrlMessage::Register { candidates } => put_candidates(out, candidates),
        CtrlMessage::ConnectRequest { target } => out.extend_from_slice(target),
        CtrlMessage::SessionOpened { session, peer } => {
            out.extend_from_slice(session);
            out.extend_from_slice(peer);
        }
        CtrlMessage::Signal { session, signal } => {
            out.extend_from_slice(session);
            put_signal(out, signal);
        }
        CtrlMessage::Cancel { session } => out.extend_from_slice(session),
        CtrlMessage::ServerError { code, message } => {
            put_u16(out, *code);
            put_str(out, message);
        }
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u16(out, s.len().min(u16::MAX as usize) as u16);
    out.extend_from_slice(s.as_bytes());
}

fn put_candidates(out: &mut Vec<u8>, candidates: &[SocketAddr]) {
    out.push(candidates.len().min(u8::MAX as usize) as u8);
    for candidate in candidates {
        put_addr(out, candidate);
    }
}

fn put_addr(out: &mut Vec<u8>, addr: &SocketAddr) {
    match addr {
        SocketAddr::V4(v4) => {
            out.push(4);
            out.extend_from_slice(&v4.ip().octets());
        }
        SocketAddr::V6(v6) => {
            out.push(6);
            out.extend_from_slice(&v6.ip().octets());
        }
    }
    put_u16(out, addr.port());
}

fn put_signal(out: &mut Vec<u8>, signal: &PeerSignal) {
    match signal {
        PeerSignal::Candidates(candidates) => {
            out.push(TAG_CANDIDATES);
            put_candidates(out, candidates);
        }
        PeerSignal::PunchAt {
            epoch_unix_ms,
            nonce,
        } => {
            out.push(TAG_PUNCH_AT);
            put_u64(out, *epoch_unix_ms);
            out.extend_from_slice(nonce);
        }
        PeerSignal::Bye => out.push(TAG_BYE),
    }
}

/// Bounded cursor over a payload slice. `take` never allocates; all limit
/// checks run before any Vec/String is built.
struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cur { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() - self.pos < n {
            return Err(DecodeError::Malformed("payload too short"));
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| DecodeError::Malformed("fixed-width field"))
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.fixed::<2>()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.fixed::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.fixed::<8>()?))
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

fn get_str(cur: &mut Cur<'_>, limit: usize) -> Result<String, DecodeError> {
    let len = cur.u16()? as usize;
    if len > limit {
        return Err(DecodeError::StringTooLong { limit });
    }
    let bytes = cur.take(len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Malformed("invalid utf-8"))
}

fn get_addr(cur: &mut Cur<'_>) -> Result<SocketAddr, DecodeError> {
    let family = cur.u8()?;
    let ip = match family {
        4 => {
            let octets = cur.fixed::<4>()?;
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(octets))
        }
        6 => {
            let octets = cur.fixed::<16>()?;
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(octets))
        }
        _ => return Err(DecodeError::Malformed("address family")),
    };
    let port = cur.u16()?;
    if port == 0 {
        return Err(DecodeError::Malformed("port 0"));
    }
    Ok(SocketAddr::new(ip, port))
}

fn get_candidates(cur: &mut Cur<'_>) -> Result<Vec<SocketAddr>, DecodeError> {
    let count = cur.u8()? as usize;
    if count > MAX_CANDIDATES {
        return Err(DecodeError::TooManyCandidates {
            count,
            limit: MAX_CANDIDATES,
        });
    }
    let mut candidates = Vec::with_capacity(count);
    for _ in 0..count {
        candidates.push(get_addr(cur)?);
    }
    Ok(candidates)
}

fn get_signal(cur: &mut Cur<'_>) -> Result<PeerSignal, DecodeError> {
    match cur.u8()? {
        TAG_CANDIDATES => Ok(PeerSignal::Candidates(get_candidates(cur)?)),
        TAG_PUNCH_AT => Ok(PeerSignal::PunchAt {
            epoch_unix_ms: cur.u64()?,
            nonce: cur.fixed::<8>()?,
        }),
        TAG_BYE => Ok(PeerSignal::Bye),
        _ => Err(DecodeError::Malformed("signal tag")),
    }
}

fn decode_payload(ty: u8, payload: &[u8]) -> Result<CtrlMessage, DecodeError> {
    let mut cur = Cur::new(payload);
    let message = match ty {
        TYPE_HELLO => {
            let version = cur.u16()?;
            let device_id = cur.fixed::<32>()?;
            let name = get_str(&mut cur, MAX_NAME_BYTES)?;
            let timestamp_unix_ms = cur.u64()?;
            let signature = cur.fixed::<64>()?;
            CtrlMessage::Hello {
                version,
                device_id,
                name,
                timestamp_unix_ms,
                signature,
            }
        }
        TYPE_WELCOME => CtrlMessage::Welcome {
            session_ttl_secs: cur.u32()?,
        },
        TYPE_REGISTER => CtrlMessage::Register {
            candidates: get_candidates(&mut cur)?,
        },
        TYPE_CONNECT_REQUEST => CtrlMessage::ConnectRequest {
            target: cur.fixed::<32>()?,
        },
        TYPE_SESSION_OPENED => CtrlMessage::SessionOpened {
            session: cur.fixed::<16>()?,
            peer: cur.fixed::<32>()?,
        },
        TYPE_SIGNAL => CtrlMessage::Signal {
            session: cur.fixed::<16>()?,
            signal: get_signal(&mut cur)?,
        },
        TYPE_CANCEL => CtrlMessage::Cancel {
            session: cur.fixed::<16>()?,
        },
        TYPE_SERVER_ERROR => {
            let code = cur.u16()?;
            let message = get_str(&mut cur, MAX_ERROR_BYTES)?;
            CtrlMessage::ServerError { code, message }
        }
        _ => return Err(DecodeError::UnknownType(ty)),
    };
    let trailing = cur.remaining();
    if trailing > 0 {
        return Err(DecodeError::Trailing(trailing));
    }
    Ok(message)
}

/// Canonical signing bytes for Hello. Deterministic, version-tagged.
pub fn hello_signing_bytes(
    version: u16,
    device_id: &DeviceId,
    name: &str,
    timestamp_unix_ms: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 2 + 32 + name.len() + 8);
    out.extend_from_slice(b"AKHELLO1");
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(device_id);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&timestamp_unix_ms.to_be_bytes());
    out
}

/// Build a signed Hello for this device key.
pub fn signed_hello(key: &DeviceKeyPair, name: &str, now_unix_ms: u64) -> CtrlMessage {
    let device_id = key.public_key();
    let signature = key.sign(&hello_signing_bytes(
        VERSION as u16,
        &device_id,
        name,
        now_unix_ms,
    ));
    CtrlMessage::Hello {
        version: VERSION as u16,
        device_id,
        name: name.to_string(),
        timestamp_unix_ms: now_unix_ms,
        signature,
    }
}

/// Verify a Hello signature and freshness. Freshness window: ±HELLO_MAX_AGE_MS.
pub fn verify_hello(message: &CtrlMessage, now_unix_ms: u64) -> Result<Fingerprint, DecodeError> {
    let CtrlMessage::Hello {
        version,
        device_id,
        name,
        timestamp_unix_ms,
        signature,
    } = message
    else {
        return Err(DecodeError::Malformed("not a hello"));
    };
    if *version != VERSION as u16 {
        return Err(DecodeError::Malformed("hello version"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(DecodeError::StringTooLong {
            limit: MAX_NAME_BYTES,
        });
    }
    let signed = hello_signing_bytes(*version, device_id, name, *timestamp_unix_ms);
    if !ed25519_verify(device_id, &signed, signature) {
        return Err(DecodeError::Malformed("hello signature"));
    }
    let age_ms = now_unix_ms.abs_diff(*timestamp_unix_ms);
    if age_ms > HELLO_MAX_AGE_MS {
        return Err(DecodeError::Freshness {
            age_ms,
            max_ms: HELLO_MAX_AGE_MS,
        });
    }
    Ok(Fingerprint::from_public_key(device_id))
}

/// Signature check routed through ak-identity in production builds.
#[cfg(not(test))]
fn ed25519_verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    DeviceKeyPair::verify(public, message, signature)
}

/// Test builds verify with ed25519-dalek directly so ak-signal's tests do
/// not depend on the parallel ak-identity agent finishing its constructors.
#[cfg(test)]
fn ed25519_verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    match VerifyingKey::from_bytes(public) {
        Ok(key) => key
            .verify(message, &Signature::from_bytes(signature))
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn frame(message: CtrlMessage) -> CtrlFrame {
        CtrlFrame {
            version: VERSION,
            message,
        }
    }

    fn roundtrip(message: CtrlMessage) {
        let original = frame(message);
        let mut bytes = Vec::new();
        original.encode(&mut bytes);
        assert!(bytes.len() <= MAX_FRAME_BYTES, "frame over hard cap");
        assert_eq!(CtrlFrame::decode(&bytes).unwrap(), original);
    }

    /// Raw frame with arbitrary payload, bypassing encode.
    fn wrap(ty: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![VERSION, ty];
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn hello_message(name: &str) -> CtrlMessage {
        CtrlMessage::Hello {
            version: 1,
            device_id: [1; 32],
            name: name.to_string(),
            timestamp_unix_ms: 42,
            signature: [2; 64],
        }
    }

    fn signed_hello_from(key: &SigningKey, version: u16, name: &str, ts: u64) -> CtrlMessage {
        let device_id = key.verifying_key().to_bytes();
        let signature = key
            .sign(&hello_signing_bytes(version, &device_id, name, ts))
            .to_bytes();
        CtrlMessage::Hello {
            version,
            device_id,
            name: name.to_string(),
            timestamp_unix_ms: ts,
            signature,
        }
    }

    // ---- roundtrips ----

    #[test]
    fn roundtrip_hello() {
        roundtrip(hello_message("phone"));
        roundtrip(hello_message(""));
        roundtrip(CtrlMessage::Hello {
            version: 1,
            device_id: [9; 32],
            name: "x".repeat(MAX_NAME_BYTES),
            timestamp_unix_ms: u64::MAX,
            signature: [7; 64],
        });
    }

    #[test]
    fn roundtrip_welcome() {
        roundtrip(CtrlMessage::Welcome {
            session_ttl_secs: 120,
        });
        roundtrip(CtrlMessage::Welcome {
            session_ttl_secs: u32::MAX,
        });
    }

    #[test]
    fn roundtrip_register_v4_and_v6() {
        roundtrip(CtrlMessage::Register {
            candidates: vec![
                addr("192.168.1.2:5000"),
                addr("[2001:db8::1]:5001"),
                addr("127.0.0.1:65535"),
                addr("[::1]:1"),
            ],
        });
        roundtrip(CtrlMessage::Register { candidates: vec![] });
    }

    #[test]
    fn roundtrip_register_max_candidates() {
        let candidates = (0..MAX_CANDIDATES)
            .map(|i| addr(&format!("[2001:db8::{i:x}]:{}", 1024 + i)))
            .collect();
        roundtrip(CtrlMessage::Register { candidates });
    }

    #[test]
    fn roundtrip_connect_request() {
        roundtrip(CtrlMessage::ConnectRequest { target: [5; 32] });
    }

    #[test]
    fn roundtrip_session_opened() {
        roundtrip(CtrlMessage::SessionOpened {
            session: [3; 16],
            peer: [6; 32],
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
            message: "e".repeat(MAX_ERROR_BYTES),
        });
    }

    #[test]
    fn header_layout() {
        let mut bytes = Vec::new();
        frame(CtrlMessage::Welcome {
            session_ttl_secs: 7,
        })
        .encode(&mut bytes);
        assert_eq!(&bytes[..4], &[VERSION, 2, 0, 4]);
        assert_eq!(&bytes[4..], &7u32.to_be_bytes());
    }

    // ---- error branches ----

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
        let mut bytes = Vec::new();
        frame(CtrlMessage::Welcome {
            session_ttl_secs: 1,
        })
        .encode(&mut bytes);
        bytes[0] = VERSION + 1;
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::BadVersion(v)) if v == VERSION + 1
        ));
    }

    #[test]
    fn err_unknown_type() {
        for ty in [0u8, 9, 42, 255] {
            assert!(matches!(
                CtrlFrame::decode(&[VERSION, ty, 0, 0]),
                Err(DecodeError::UnknownType(t)) if t == ty
            ));
        }
    }

    #[test]
    fn err_oversized_declared_length() {
        // 4093-byte payload, one over the 4092 cap.
        assert!(matches!(
            CtrlFrame::decode(&[VERSION, 1, 0x0F, 0xFD]),
            Err(DecodeError::Oversized {
                declared: 4093,
                cap: 4092
            })
        ));
        assert!(matches!(
            CtrlFrame::decode(&[VERSION, 1, 0xFF, 0xFF]),
            Err(DecodeError::Oversized { .. })
        ));
    }

    #[test]
    fn payload_at_cap_passes_size_checks() {
        // Exactly 4092 bytes of payload: size checks pass (the padded body
        // then fails payload parsing, but never as Oversized).
        let mut bytes = vec![VERSION, 1, 0x0F, 0xFC];
        bytes.resize(HEADER_LEN + 4092, 0);
        match CtrlFrame::decode(&bytes) {
            Err(DecodeError::Oversized { .. }) => {
                panic!("at-cap payload must not be rejected as Oversized")
            }
            Err(_) => {}
            Ok(_) => panic!("zero-padded Hello payload must not decode"),
        }
    }

    #[test]
    fn err_truncated_payload() {
        let mut bytes = Vec::new();
        frame(hello_message("x")).encode(&mut bytes);
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::TruncatedPayload { declared, present }) if declared == present as u16 + 1
        ));
    }

    #[test]
    fn err_trailing_bytes() {
        let mut bytes = Vec::new();
        frame(CtrlMessage::Welcome {
            session_ttl_secs: 1,
        })
        .encode(&mut bytes);
        bytes.push(0);
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::Trailing(1))
        ));
    }

    #[test]
    fn err_too_many_candidates() {
        assert!(matches!(
            CtrlFrame::decode(&wrap(3, &[17])),
            Err(DecodeError::TooManyCandidates {
                count: 17,
                limit: 16
            })
        ));
    }

    #[test]
    fn err_name_too_long() {
        let mut bytes = Vec::new();
        frame(hello_message(&"n".repeat(MAX_NAME_BYTES + 1))).encode(&mut bytes);
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::StringTooLong {
                limit: MAX_NAME_BYTES
            })
        ));
    }

    #[test]
    fn err_error_string_too_long() {
        let mut bytes = Vec::new();
        frame(CtrlMessage::ServerError {
            code: 1,
            message: "e".repeat(MAX_ERROR_BYTES + 1),
        })
        .encode(&mut bytes);
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::StringTooLong {
                limit: MAX_ERROR_BYTES
            })
        ));
    }

    #[test]
    fn err_bad_address_family() {
        // count=1, family=5 (only 4 and 6 are defined).
        assert!(matches!(
            CtrlFrame::decode(&wrap(3, &[1, 5, 1, 0, 0, 1, 0x30, 0x39])),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn err_port_zero_rejected_on_decode() {
        // Encode is infallible and will write a port-0 candidate; decode
        // must refuse it.
        let mut bytes = Vec::new();
        frame(CtrlMessage::Register {
            candidates: vec![addr("127.0.0.1:0")],
        })
        .encode(&mut bytes);
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::Malformed(msg)) if msg.contains("port")
        ));
    }

    #[test]
    fn err_address_bytes_truncated() {
        // v6 candidate with only 5 of 16 address bytes present.
        assert!(matches!(
            CtrlFrame::decode(&wrap(3, &[1, 6, 0x20, 0x01, 0x0d, 0xb8])),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn err_bad_signal_tag() {
        let mut payload = vec![0u8; 16];
        payload.push(9);
        assert!(matches!(
            CtrlFrame::decode(&wrap(6, &payload)),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn err_signal_body_truncated() {
        // PunchAt tag but only 4 of the 8 nonce bytes.
        let mut payload = vec![0u8; 16];
        payload.extend_from_slice(&[2]);
        payload.extend_from_slice(&9u64.to_be_bytes());
        payload.extend_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(
            CtrlFrame::decode(&wrap(6, &payload)),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn err_hello_payload_too_short() {
        assert!(matches!(
            CtrlFrame::decode(&wrap(1, &[0, 1])),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn err_invalid_utf8_string() {
        // version=1, name len=3, then invalid utf-8; timestamp+signature padded.
        let mut payload = vec![0, 1, 0, 3, 0xff, 0xfe, 0xfd];
        payload.extend_from_slice(&[0u8; 8 + 64]);
        assert!(matches!(
            CtrlFrame::decode(&wrap(1, &payload)),
            Err(DecodeError::Malformed(_))
        ));
    }

    // ---- Hello signing ----

    #[test]
    fn signing_bytes_layout() {
        let device_id = [7u8; 32];
        let ts = 0x1122_3344_5566_7788u64;
        let bytes = hello_signing_bytes(1, &device_id, "alice", ts);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"AKHELLO1");
        expected.extend_from_slice(&1u16.to_be_bytes());
        expected.extend_from_slice(&device_id);
        expected.extend_from_slice(b"alice");
        expected.extend_from_slice(&ts.to_be_bytes());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn verify_hello_accepts_valid_signature() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let now = 1_000_000;
        let message = signed_hello_from(&key, 1, "phone", now);
        let fingerprint = verify_hello(&message, now).unwrap();
        assert_eq!(
            fingerprint,
            Fingerprint::from_public_key(&key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn verify_hello_rejects_wrong_signature() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let other = SigningKey::from_bytes(&[4u8; 32]);
        let device_id = key.verifying_key().to_bytes();
        let signature = other
            .sign(&hello_signing_bytes(1, &device_id, "phone", 42))
            .to_bytes();
        let message = CtrlMessage::Hello {
            version: 1,
            device_id,
            name: "phone".to_string(),
            timestamp_unix_ms: 42,
            signature,
        };
        assert!(matches!(
            verify_hello(&message, 42),
            Err(DecodeError::Malformed(msg)) if msg.contains("signature")
        ));
    }

    #[test]
    fn verify_hello_rejects_invalid_public_key() {
        let message = CtrlMessage::Hello {
            version: 1,
            device_id: [9; 32], // not a valid curve point
            name: "x".to_string(),
            timestamp_unix_ms: 42,
            signature: [0; 64],
        };
        assert!(matches!(
            verify_hello(&message, 42),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn verify_hello_freshness_window() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let now = 100_000u64;
        assert!(verify_hello(
            &signed_hello_from(&key, 1, "n", now - HELLO_MAX_AGE_MS),
            now
        )
        .is_ok());
        assert!(verify_hello(
            &signed_hello_from(&key, 1, "n", now + HELLO_MAX_AGE_MS),
            now
        )
        .is_ok());
        assert!(matches!(
            verify_hello(&signed_hello_from(&key, 1, "n", now - HELLO_MAX_AGE_MS - 1), now),
            Err(DecodeError::Freshness { age_ms, max_ms }) if age_ms == HELLO_MAX_AGE_MS + 1 && max_ms == HELLO_MAX_AGE_MS
        ));
        assert!(matches!(
            verify_hello(
                &signed_hello_from(&key, 1, "n", now + HELLO_MAX_AGE_MS + 1),
                now
            ),
            Err(DecodeError::Freshness { .. })
        ));
    }

    #[test]
    fn verify_hello_rejects_non_hello() {
        assert!(matches!(
            verify_hello(&CtrlMessage::Welcome { session_ttl_secs: 1 }, 0),
            Err(DecodeError::Malformed(msg)) if msg.contains("hello")
        ));
    }

    #[test]
    fn verify_hello_rejects_version_mismatch() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let message = signed_hello_from(&key, 2, "n", 42);
        assert!(matches!(
            verify_hello(&message, 42),
            Err(DecodeError::Malformed(msg)) if msg.contains("version")
        ));
    }

    #[test]
    fn verify_hello_rejects_oversized_name() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let message = signed_hello_from(&key, 1, &"n".repeat(MAX_NAME_BYTES + 1), 42);
        assert!(matches!(
            verify_hello(&message, 42),
            Err(DecodeError::StringTooLong { .. })
        ));
    }

    // ---- closed-set lock (ADR-001 INV-005 / ADR-002 D6) ----

    /// Exhaustive match: adding a CtrlMessage variant without assigning it a
    /// bounded wire type breaks compilation here.
    fn type_byte(message: &CtrlMessage) -> u8 {
        match message {
            CtrlMessage::Hello { .. } => 1,
            CtrlMessage::Welcome { .. } => 2,
            CtrlMessage::Register { .. } => 3,
            CtrlMessage::ConnectRequest { .. } => 4,
            CtrlMessage::SessionOpened { .. } => 5,
            CtrlMessage::Signal { .. } => 6,
            CtrlMessage::Cancel { .. } => 7,
            CtrlMessage::ServerError { .. } => 8,
        }
    }

    /// No variant of the closed message set can carry a blob: every frame
    /// stays under the hard cap and every type byte stays in 1..=8.
    #[test]
    fn closed_message_set_is_bounded() {
        let mut max_candidates = Vec::new();
        for i in 0..MAX_CANDIDATES {
            max_candidates.push(addr(&format!("[2001:db8::{i:x}]:{}", 1024 + i)));
        }
        let all = vec![
            CtrlMessage::Hello {
                version: 1,
                device_id: [3; 32],
                name: "n".repeat(MAX_NAME_BYTES),
                timestamp_unix_ms: u64::MAX,
                signature: [9; 64],
            },
            CtrlMessage::Welcome {
                session_ttl_secs: u32::MAX,
            },
            CtrlMessage::Register {
                candidates: max_candidates.clone(),
            },
            CtrlMessage::ConnectRequest { target: [5; 32] },
            CtrlMessage::SessionOpened {
                session: [1; 16],
                peer: [7; 32],
            },
            CtrlMessage::Signal {
                session: [1; 16],
                signal: PeerSignal::Candidates(max_candidates),
            },
            CtrlMessage::Signal {
                session: [1; 16],
                signal: PeerSignal::PunchAt {
                    epoch_unix_ms: u64::MAX,
                    nonce: [2; 8],
                },
            },
            CtrlMessage::Signal {
                session: [1; 16],
                signal: PeerSignal::Bye,
            },
            CtrlMessage::Cancel { session: [4; 16] },
            CtrlMessage::ServerError {
                code: u16::MAX,
                message: "e".repeat(MAX_ERROR_BYTES),
            },
        ];
        assert_eq!(all.len(), 8 + 2); // 8 types, Signal exercised 3 ways
        for message in all {
            let expected_type = type_byte(&message);
            let mut bytes = Vec::new();
            frame(message.clone()).encode(&mut bytes);
            assert!(bytes.len() <= MAX_FRAME_BYTES, "blob smuggled in?");
            assert_eq!(bytes[0], VERSION);
            assert_eq!(bytes[1], expected_type);
            assert!((1..=8).contains(&bytes[1]));
            assert_eq!(CtrlFrame::decode(&bytes).unwrap().message, message);
        }
        // Anything outside the closed set is refused.
        for ty in [0u8, 9, 200, 255] {
            assert!(matches!(
                CtrlFrame::decode(&[VERSION, ty, 0, 0]),
                Err(DecodeError::UnknownType(t)) if t == ty
            ));
        }
    }
}
