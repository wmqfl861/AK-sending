//! Signaling server: TCP signaling plus a UDP STUN responder in one process.
//!
//! Limits (independent of any file quota): per-connection message rate,
//! bounded candidates, bounded sessions per device, session TTL, Hello
//! freshness. The server verifies Hello signatures but trusts nothing else;
//! it never sees file names, manifests, or bytes.
//!
//! There is intentionally no upload/download/forward route. A test locks the
//! closed message set so no blob route can appear silently.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use crate::codec::CtrlFrame;

/// Server limits; every field is a coordination-plane resource cap.
#[derive(Debug, Clone)]
pub struct Limits {
    pub session_ttl: Duration,
    pub max_sessions_per_device: usize,
    pub hello_max_age: Duration,
    /// Steady-state frames per second per connection.
    pub frame_rate_per_sec: u32,
    /// Burst allowance for the token bucket.
    pub frame_burst: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(120),
            max_sessions_per_device: 4,
            hello_max_age: Duration::from_secs(10),
            frame_rate_per_sec: 32,
            frame_burst: 64,
        }
    }
}

/// Where to listen: TCP signaling plus UDP STUN (same process, two sockets).
#[derive(Debug, Clone)]
pub struct Config {
    pub tcp: SocketAddr,
    pub stun_udp: SocketAddr,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tcp: SocketAddr::from(([0, 0, 0, 0], 7100)),
            stun_udp: SocketAddr::from(([0, 0, 0, 0], 7101)),
            limits: Limits::default(),
        }
    }
}

/// Runtime handle for embedding (tests, CLI).
pub struct ServerHandle {
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

/// Run the server until `shutdown` fires. Binding both sockets is part of
/// startup; errors abort the whole server.
pub async fn run(config: Config, shutdown: tokio::sync::watch::Receiver<bool>) -> std::io::Result<()> {
    todo!("owned by ak-signal agent")
}

/// Encode helper re-exported for tests.
pub fn frame_bytes(frame: &CtrlFrame) -> Vec<u8> {
    let mut out = Vec::new();
    frame.encode(&mut out);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-signal agent"]
    fn placeholder() {}
}
