//! Internal wire primitives shared by the v2 submodules: a bounded payload
//! cursor, budget-checked string helpers, and the `SocketAddr` / candidate
//! list encoding carried over verbatim from v1 (family byte 4/6 + 4/16
//! address bytes + port u16 BE; port 0 is refused).
//!
//! Private to `protocol_v2`; the public surface lives in
//! [`super::frame`], [`super::message`] and [`super::peer_signal`].

use std::net::SocketAddr;

use super::frame::{DecodeError, EncodeError};
use super::MAX_CANDIDATES;

/// Bounded cursor over one frame payload. `take` never allocates; all limit
/// checks run before any Vec/String is built (contract: validate type and
/// length budget first, then allocate).
pub(super) struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Self {
        Cur { buf, pos: 0 }
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() - self.pos < n {
            return Err(DecodeError::Malformed("payload too short"));
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub(super) fn fixed<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| DecodeError::Malformed("fixed-width field"))
    }

    pub(super) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.fixed::<2>()?))
    }

    pub(super) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.fixed::<4>()?))
    }

    pub(super) fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.fixed::<8>()?))
    }

    pub(super) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// True when the string carries no C0/C1/DEL control characters. Shared by
/// decode ([`get_str`]) and encode ([`check_str`]) so the rule cannot drift
/// between the wire path and the production path (same rule as v1).
pub(super) fn is_control_free(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// Encode-side string validation: byte cap plus control-character hygiene.
/// v2 encoding is fallible (`try_encode`); it never truncates or silently
/// accepts a field decode would refuse.
pub(super) fn check_str(s: &str, limit: usize) -> Result<(), EncodeError> {
    if s.len() > limit {
        return Err(EncodeError::StringTooLong { limit });
    }
    if !is_control_free(s) {
        return Err(EncodeError::ControlChars);
    }
    Ok(())
}

/// Length-prefixed (u16 BE) string field. Only called after [`check_str`],
/// so the length always fits the budget and the cast cannot wrap.
pub(super) fn put_str(out: &mut Vec<u8>, s: &str) {
    crate::util::put_u16(out, s.len() as u16);
    out.extend_from_slice(s.as_bytes());
}

/// Decode a length-prefixed string field: byte cap, UTF-8 validity (which
/// already refuses overlong encodings and surrogates) and C0/C1/DEL control
/// characters are all enforced before the String is built.
pub(super) fn get_str(cur: &mut Cur<'_>, limit: usize) -> Result<String, DecodeError> {
    let len = cur.u16()? as usize;
    if len > limit {
        return Err(DecodeError::StringTooLong { limit });
    }
    let bytes = cur.take(len)?;
    let s =
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Malformed("invalid utf-8"))?;
    if !is_control_free(&s) {
        return Err(DecodeError::ControlChars);
    }
    Ok(s)
}

/// Family byte + address octets + port, identical to the v1 encoding.
pub(super) fn put_addr(out: &mut Vec<u8>, addr: &SocketAddr) {
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
    crate::util::put_u16(out, addr.port());
}

/// Encode-side address validation: port 0 is never a real candidate.
pub(super) fn check_addr(addr: &SocketAddr) -> Result<(), EncodeError> {
    if addr.port() == 0 {
        return Err(EncodeError::PortZero);
    }
    Ok(())
}

pub(super) fn get_addr(cur: &mut Cur<'_>) -> Result<SocketAddr, DecodeError> {
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

/// count:u8 + addresses, budget-checked before anything is written.
pub(super) fn put_candidates_checked(
    out: &mut Vec<u8>,
    candidates: &[SocketAddr],
) -> Result<(), EncodeError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(EncodeError::TooManyCandidates {
            count: candidates.len(),
            limit: MAX_CANDIDATES,
        });
    }
    for candidate in candidates {
        check_addr(candidate)?;
    }
    out.push(candidates.len() as u8);
    for candidate in candidates {
        put_addr(out, candidate);
    }
    Ok(())
}

pub(super) fn get_candidates(cur: &mut Cur<'_>) -> Result<Vec<SocketAddr>, DecodeError> {
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
