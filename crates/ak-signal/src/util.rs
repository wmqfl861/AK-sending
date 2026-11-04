//! Internal helpers shared by the signaling modules: wall-clock time,
//! best-effort unique ids, bounded frame reads, and log-safe hex. None of
//! this touches file data; the coordination plane has no other payload.
#![forbid(unsafe_code)]

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::AsyncReadExt;
use tokio::net::tcp::OwnedReadHalf;

use crate::codec;

/// Milliseconds since the Unix epoch. Consumed only by Hello freshness
/// windows and id mixing; elapsed-time decisions use `Instant`.
pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wall-clock milliseconds with a process-wide monotonic guarantee: two
/// calls never return the same value. Used for Hello timestamps so a client
/// never signs the exact same Hello bytes twice (the server's replay cache
/// rejects a repeated signature inside the freshness window; a legitimate
/// sub-millisecond reconnect would otherwise be indistinguishable from a
/// replay). The nudge stays within ±1 ms of the true clock, far inside the
/// ±10 s freshness window.
pub(crate) fn fresh_now_unix_ms() -> u64 {
    static LAST: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut now = now_unix_ms();
    if now <= *last {
        now = *last + 1;
    }
    *last = now;
    now
}

pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub(crate) fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Best-effort unique bytes for STUN transaction ids and session ids.
/// Not a CSPRNG (production dependencies are limited to
/// ak-identity/tokio/thiserror/log); a time+counter splitmix stream is
/// sufficient because ids are only ever compared against live maps.
pub(crate) fn unique_bytes(n: usize) -> Vec<u8> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut state =
        nanos ^ count.wrapping_mul(0x51ED_2701_2183_2749) ^ ((std::process::id() as u64) << 32);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let chunk = splitmix64(&mut state).to_be_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&chunk[..take]);
    }
    out
}

pub(crate) fn unique_array<const N: usize>() -> [u8; N] {
    let bytes = unique_bytes(N);
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    out
}

/// First `n` bytes as lowercase hex, for logs. Device ids are public keys,
/// never secrets; names and other user data stay out of logs entirely.
pub(crate) fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let mut s = String::new();
    for b in bytes.iter().take(n) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Read one length-prefixed signaling frame from a TCP stream. `Ok(None)`
/// means the peer closed cleanly. A declared length above the hard cap is
/// an error so the caller drops the connection before allocating anything.
pub(crate) async fn read_frame(reader: &mut OwnedReadHalf) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; codec::HEADER_LEN];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if codec::HEADER_LEN + declared > codec::MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds hard cap",
        ));
    }
    let mut frame = header.to_vec();
    frame.resize(codec::HEADER_LEN + declared, 0);
    reader.read_exact(&mut frame[codec::HEADER_LEN..]).await?;
    Ok(Some(frame))
}
