//! QUIC direct channel: the only producer of authenticated direct connections.
//!
//! `DirectChannel` has no public constructor: it exists only after a QUIC+TLS
//! handshake whose peer certificate pins to a known Ed25519 device key
//! (client side) or verifies self-signed and passes the `authorize` callback
//! (server side). This is the type-level constraint from REVIEW D6: the file
//! engine can only consume channels that were constructed from verified peers.
//!
//! No relay exists in this stack: one UDP socket talks to one peer address;
//! there is no fallback transport and no server data path.
#![forbid(unsafe_code)]

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ak_core::policy::PathKind;
use ak_identity::Fingerprint;
use ak_wire::{Frame, Limits};

/// ALPN protocol identifier for the direct channel.
pub const ALPN: &[&str] = &["ak/1"];

/// Heuristic local-network classification used to fill `PathKind`.
///
/// Known limitation (documented, not hidden): a VPN can also use private
/// ranges, so classification is a routing hint. Authentication and policy are
/// unaffected; the engine re-runs policy on any path change.
pub mod classify {
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use ak_core::policy::PathKind;

    /// One local network: an interface address plus its prefix length.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Net {
        pub addr: IpAddr,
        pub prefix_len: u8,
    }

    /// Enumerate local interface networks (excludes loopback interfaces).
    pub fn local_nets() -> io::Result<Vec<Net>> {
        todo!("owned by ak-net agent")
    }

    /// True when `a` and `b` fall inside the same `net`.
    pub fn same_net(a: IpAddr, b: IpAddr, net: Net) -> bool {
        todo!("owned by ak-net agent")
    }

    /// Loopback, link-local, or same-subnet-with-a-local-interface => Local,
    /// otherwise Internet. Never returns Relay/Unknown.
    pub fn classify(peer: IpAddr, nets: &[Net]) -> PathKind {
        todo!("owned by ak-net agent")
    }

    /// Ipv6 link-local check helper (fe80::/10), public for tests.
    pub fn is_ipv6_link_local(addr: Ipv6Addr) -> bool {
        todo!("owned by ak-net agent")
    }

    /// RFC1918 + loopback check helper, public for tests.
    pub fn is_ipv4_private(addr: Ipv4Addr) -> bool {
        todo!("owned by ak-net agent")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("QUIC transport error: {0}")]
    Transport(String),
    #[error("peer certificate failed pinning: {0}")]
    PeerIdentity(String),
    #[error("peer rejected by the authorize callback")]
    Unauthorized,
    #[error("timeout after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("connection lost: {0}")]
    Connection(String),
    #[error("wire decode error: {0}")]
    Wire(#[from] ak_wire::DecodeError),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// A bound UDP endpoint speaking QUIC for direct device connections.
pub struct DirectEndpoint {
    endpoint: quinn::Endpoint,
    nets: Vec<classify::Net>,
}

impl DirectEndpoint {
    /// Bind one UDP socket and configure QUIC with our identity for both
    /// client and server roles (hole-punch friendly: the same socket accepts).
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        todo!("owned by ak-net agent")
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Local networks seen at bind time (refreshed per call in later versions).
    pub fn local_nets(&self) -> &[classify::Net] {
        &self.nets
    }

    pub fn classify_peer(&self, peer: IpAddr) -> PathKind {
        classify::classify(peer, &self.nets)
    }

    /// Connect directly to `peer`, pinning `pinned`. Fails closed: any
    /// certificate mismatch, proxy indication, or transport error aborts.
    pub async fn connect(
        &self,
        peer: SocketAddr,
        pinned: &Fingerprint,
    ) -> Result<DirectChannel, ConnectError> {
        todo!("owned by ak-net agent")
    }

    /// Await one incoming connection; the peer certificate must be self-signed
    /// and `authorize` must accept its fingerprint.
    pub async fn accept(
        &self,
        authorize: &(dyn Fn(&Fingerprint) -> bool + Send + Sync),
    ) -> Result<DirectChannel, ConnectError> {
        todo!("owned by ak-net agent")
    }
}

/// An authenticated direct connection. Constructible only by this crate after
/// peer verification. Exactly one QUIC connection to exactly one peer address.
pub struct DirectChannel {
    pub(super) conn: quinn::Connection,
    pub(super) fingerprint: Fingerprint,
    pub(super) path: PathKind,
}

impl DirectChannel {
    pub fn peer_fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn path(&self) -> PathKind {
        self.path
    }

    pub async fn open_bi(&self) -> Result<DirectStream, StreamError> {
        todo!("owned by ak-net agent")
    }

    pub async fn accept_bi(&self) -> Result<DirectStream, StreamError> {
        todo!("owned by ak-net agent")
    }

    /// Close the connection with a machine-readable code.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.conn.close(varint(code), reason);
    }

    /// True while the underlying connection reports itself healthy.
    pub fn healthy(&self) -> bool {
        todo!("owned by ak-net agent")
    }
}

fn varint(code: u32) -> quinn::VarInt {
    quinn::VarInt::from_u32(code)
}

/// One bidirectional stream carrying wire frames.
pub struct DirectStream {
    pub(super) send: quinn::SendStream,
    pub(super) recv: quinn::RecvStream,
}

impl DirectStream {
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), StreamError> {
        todo!("owned by ak-net agent")
    }

    pub async fn recv_frame(&mut self, limits: &Limits) -> Result<Frame, StreamError> {
        todo!("owned by ak-net agent")
    }

    /// Half-close: finish sending, keep receiving.
    pub async fn finish_send(&mut self) -> Result<(), StreamError> {
        self.send.finish().await.map_err(|e| StreamError::Connection(e.to_string()))
    }
}

/// UDP simultaneous-open probing for NAT traversal (control plane coordinates
/// the attempt; the data path stays strictly peer-to-peer on this socket).
pub mod holepunch {
    use std::io;
    use std::net::SocketAddr;
    use std::time::Duration;

    /// Result of one probe round.
    #[derive(Debug, Clone, Default)]
    pub struct PunchReport {
        /// Candidates that answered our probe with a valid peer packet.
        pub reachable: Vec<SocketAddr>,
        /// Address a peer packet was observed from, if any.
        pub peer_observed_from: Option<SocketAddr>,
        pub packets_sent: u64,
        pub packets_received: u64,
        pub elapsed: Duration,
    }

    /// Magic tag inside probe packets: "AKPUNCH1" + 16 random bytes.
    pub const PROBE_TAG: &[u8; 8] = b"AKPUNCH1";

    /// Build one probe packet (tag + nonce).
    pub fn probe_packet(nonce: &[u8; 16]) -> Vec<u8> {
        let mut packet = PROBE_TAG.to_vec();
        packet.extend_from_slice(nonce);
        packet
    }

    /// True when the packet looks like a peer probe packet.
    pub fn is_probe_packet(packet: &[u8]) -> bool {
        packet.len() == 24 && &packet[..8] == PROBE_TAG
    }

    /// From a socket already bound by the caller (the same socket later used
    /// for QUIC), send probes to every candidate and listen for peer probes
    /// for `duration`, `interval` between rounds.
    pub async fn probe(
        socket: &tokio::net::UdpSocket,
        candidates: &[SocketAddr],
        interval: Duration,
        duration: Duration,
    ) -> io::Result<PunchReport> {
        todo!("owned by ak-net agent")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-net agent"]
    fn placeholder() {}
}
