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

mod tls;

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
    ///
    /// IPv4 prefixes come from the interface netmask. IPv6 links are matched
    /// as /64 regardless of the announced prefix (documented simplification:
    /// site prefixes larger than /64 are rare on LANs and /64 is the IID
    /// boundary required by SLAAC).
    pub fn local_nets() -> io::Result<Vec<Net>> {
        let mut nets = Vec::new();
        for iface in if_addrs::get_if_addrs()? {
            if iface.is_loopback() {
                continue;
            }
            let net = match iface.addr {
                if_addrs::IfAddr::V4(v4) => Net {
                    addr: IpAddr::V4(v4.ip),
                    prefix_len: prefix_from_mask(v4.netmask),
                },
                if_addrs::IfAddr::V6(v6) => Net {
                    addr: IpAddr::V6(v6.ip),
                    prefix_len: 64,
                },
            };
            nets.push(net);
        }
        Ok(nets)
    }

    fn prefix_from_mask(netmask: Ipv4Addr) -> u8 {
        u32::from(netmask).count_ones() as u8
    }

    /// True when `a` and `b` fall inside the same `net`.
    pub fn same_net(a: IpAddr, b: IpAddr, net: Net) -> bool {
        match (a, b, net.addr) {
            (IpAddr::V4(a), IpAddr::V4(b), IpAddr::V4(n)) => {
                let prefix = net.prefix_len.min(32);
                let mask = mask_u32(prefix);
                (u32::from(a) & mask) == (u32::from(n) & mask)
                    && (u32::from(b) & mask) == (u32::from(n) & mask)
            }
            (IpAddr::V6(a), IpAddr::V6(b), IpAddr::V6(n)) => {
                let prefix = net.prefix_len.min(128);
                let mask = mask_u128(prefix);
                (u128::from(a) & mask) == (u128::from(n) & mask)
                    && (u128::from(b) & mask) == (u128::from(n) & mask)
            }
            _ => false,
        }
    }

    fn mask_u32(prefix: u8) -> u32 {
        if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        }
    }

    fn mask_u128(prefix: u8) -> u128 {
        if prefix == 0 {
            0
        } else {
            u128::MAX << (128 - prefix)
        }
    }

    /// Loopback, link-local, or same-subnet-with-a-local-interface => Local,
    /// otherwise Internet. Never returns Relay/Unknown.
    pub fn classify(peer: IpAddr, nets: &[Net]) -> PathKind {
        if peer.is_loopback() {
            return PathKind::LocalDirect;
        }
        if let IpAddr::V6(v6) = peer {
            if is_ipv6_link_local(v6) {
                return PathKind::LocalDirect;
            }
        }
        if nets.iter().any(|net| same_net(peer, net.addr, *net)) {
            return PathKind::LocalDirect;
        }
        PathKind::InternetDirect
    }

    /// Ipv6 link-local check helper (fe80::/10), public for tests.
    pub fn is_ipv6_link_local(addr: Ipv6Addr) -> bool {
        (addr.segments()[0] & 0xffc0) == 0xfe80
    }

    /// RFC1918 + loopback check helper, public for tests.
    pub fn is_ipv4_private(addr: Ipv4Addr) -> bool {
        let octets = addr.octets();
        addr.is_loopback()
            || octets[0] == 10
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
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
    identity: ak_identity::tls::TlsBundle,
    nets: Vec<classify::Net>,
}

impl DirectEndpoint {
    /// Bind one UDP socket and configure QUIC with our identity for both
    /// client and server roles (hole-punch friendly: the same socket accepts).
    ///
    /// The server side requires client certificates; the client side pins a
    /// fingerprint per `connect` call. The endpoint-level default client
    /// config carries no pin and therefore refuses every peer, so only
    /// `connect` can produce a channel.
    pub async fn bind(
        addr: SocketAddr,
        identity: &ak_identity::tls::TlsBundle,
    ) -> io::Result<Self> {
        let socket = std::net::UdpSocket::bind(addr)?;
        Self::bind_with_socket(socket, identity).await
    }

    /// Adopt a caller-owned UDP socket (already bound, e.g. the very socket
    /// used for hole-punch probing, so the NAT pinhole stays valid) and run
    /// QUIC on it. The socket must not be shared afterwards.
    pub async fn bind_with_socket(
        socket: std::net::UdpSocket,
        identity: &ak_identity::tls::TlsBundle,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let runtime = quinn::default_runtime()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no async runtime found"))?;
        let server_config = tls::quinn_server_config(identity)?;
        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config),
            socket,
            runtime,
        )?;
        endpoint.set_default_client_config(tls::quinn_client_config(identity, None)?);
        let nets = classify::local_nets()?;
        Ok(Self {
            endpoint,
            identity: identity.clone(),
            nets,
        })
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
        let client_config = tls::quinn_client_config(&self.identity, Some(pinned))?;
        let connecting = self
            .endpoint
            .connect_with(client_config, peer, "ak-direct")
            .map_err(|e| ConnectError::Transport(e.to_string()))?;
        let conn = connecting.await.map_err(handshake_error)?;
        // Defense in depth: re-derive the fingerprint from the established
        // connection; the pinned value only becomes the channel identity if
        // the live peer still matches it.
        let fingerprint = peer_fingerprint(&conn).map_err(ConnectError::PeerIdentity)?;
        if fingerprint != *pinned {
            conn.close(varint(1), b"fingerprint mismatch");
            return Err(ConnectError::PeerIdentity(
                "post-handshake fingerprint mismatch".into(),
            ));
        }
        Ok(DirectChannel {
            conn,
            fingerprint,
            path: classify::classify(peer.ip(), &self.nets),
        })
    }

    /// Await one incoming connection; the peer certificate must be self-signed
    /// and `authorize` must accept its fingerprint.
    pub async fn accept(
        &self,
        authorize: &(dyn Fn(&Fingerprint) -> bool + Send + Sync),
    ) -> Result<DirectChannel, ConnectError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "endpoint closed"))?;
        let connecting = incoming
            .accept()
            .map_err(|e| ConnectError::Transport(e.to_string()))?;
        let conn = connecting.await.map_err(handshake_error)?;
        let fingerprint = peer_fingerprint(&conn).map_err(ConnectError::PeerIdentity)?;
        if !authorize(&fingerprint) {
            conn.close(varint(1), b"unauthorized");
            return Err(ConnectError::Unauthorized);
        }
        let path = classify::classify(conn.remote_address().ip(), &self.nets);
        Ok(DirectChannel {
            conn,
            fingerprint,
            path,
        })
    }
}

/// Extract and verify the peer certificate chain from an established QUIC
/// connection, returning the peer device fingerprint.
fn peer_fingerprint(conn: &quinn::Connection) -> Result<Fingerprint, String> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| "peer presented no certificate".to_string())?;
    let boxed_chain = identity
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .map_err(|_| "peer identity had an unexpected type".to_string())?;
    let end_entity = (*boxed_chain)
        .first()
        .ok_or_else(|| "peer presented an empty certificate chain".to_string())?;
    let public_key = ak_identity::tls::self_signed_public_key(end_entity.as_ref())
        .map_err(|why| why.to_string())?;
    Ok(Fingerprint::from_public_key(&public_key))
}

/// Map a QUIC handshake/transport failure onto `ConnectError`. Certificate
/// rejections (our verifiers phrase every failure with the word
/// "certificate") become `PeerIdentity`; everything else is `Transport`.
fn handshake_error(err: quinn::ConnectionError) -> ConnectError {
    let text = err.to_string();
    if text.to_ascii_lowercase().contains("certificate") {
        ConnectError::PeerIdentity(text)
    } else {
        ConnectError::Transport(text)
    }
}

/// An authenticated direct connection. Constructible only by this crate after
/// peer verification. Exactly one QUIC connection to exactly one peer address.
pub struct DirectChannel {
    pub(crate) conn: quinn::Connection,
    pub(crate) fingerprint: Fingerprint,
    pub(crate) path: PathKind,
}

impl DirectChannel {
    pub fn peer_fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn path(&self) -> PathKind {
        self.path
    }

    pub async fn open_bi(&self) -> Result<DirectStream, StreamError> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))?;
        Ok(DirectStream { send, recv })
    }

    pub async fn accept_bi(&self) -> Result<DirectStream, StreamError> {
        let (send, recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))?;
        Ok(DirectStream { send, recv })
    }

    /// Close the connection with a machine-readable code.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.conn.close(varint(code), reason);
    }

    /// True while the underlying connection reports itself healthy.
    /// Conservative: only a QUIC close (local or remote, including idle
    /// timeout) marks the channel unhealthy; `remote_address()` stays
    /// queryable after close and adds no signal here.
    pub fn healthy(&self) -> bool {
        self.conn.close_reason().is_none()
    }
}

fn varint(code: u32) -> quinn::VarInt {
    quinn::VarInt::from_u32(code)
}

/// One bidirectional stream carrying wire frames.
pub struct DirectStream {
    pub(crate) send: quinn::SendStream,
    pub(crate) recv: quinn::RecvStream,
}

impl DirectStream {
    /// Split into independent halves so protocol drivers can read and write
    /// the same stream concurrently (chunks one way, acks the other).
    pub fn split(self) -> (DirectSendStream, DirectRecvStream) {
        (
            DirectSendStream { send: self.send },
            DirectRecvStream { recv: self.recv },
        )
    }

    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), StreamError> {
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        self.send_bytes(&bytes).await
    }

    pub async fn recv_frame(&mut self, limits: &Limits) -> Result<Frame, StreamError> {
        let mut header = [0u8; ak_wire::HEADER_LEN];
        self.recv_exact(&mut header).await?;
        let payload_len = payload_len_of(&header)?;
        let mut frame_bytes = header.to_vec();
        frame_bytes.resize(header.len() + payload_len, 0);
        self.recv_exact(&mut frame_bytes[header.len()..]).await?;
        Ok(Frame::decode(&frame_bytes, limits)?)
    }

    /// Send raw bytes. Uses quinn's inherent async `write_all` (the tokio
    /// `AsyncWriteExt` impl on `SendStream` delegates to the same method).
    pub async fn send_bytes(&mut self, data: &[u8]) -> Result<(), StreamError> {
        self.send
            .write_all(data)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    /// Read exactly `buf.len()` bytes. Uses quinn's inherent async
    /// `read_exact` (same story as `send_bytes`).
    pub async fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), StreamError> {
        self.recv
            .read_exact(buf)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    /// Half-close: finish sending, keep receiving. Synchronous in quinn 0.11.
    pub fn finish_send(&mut self) -> Result<(), StreamError> {
        self.send
            .finish()
            .map_err(|e| StreamError::Connection(e.to_string()))
    }
}

/// Send half of a split [`DirectStream`].
pub struct DirectSendStream {
    send: quinn::SendStream,
}

impl DirectSendStream {
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), StreamError> {
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        self.send
            .write_all(&bytes)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    pub async fn send_bytes(&mut self, data: &[u8]) -> Result<(), StreamError> {
        self.send
            .write_all(data)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    pub fn finish(&mut self) -> Result<(), StreamError> {
        self.send
            .finish()
            .map_err(|e| StreamError::Connection(e.to_string()))
    }
}

/// Receive half of a split [`DirectStream`].
pub struct DirectRecvStream {
    recv: quinn::RecvStream,
}

impl DirectRecvStream {
    pub async fn recv_frame(&mut self, limits: &Limits) -> Result<Frame, StreamError> {
        let mut header = [0u8; ak_wire::HEADER_LEN];
        self.recv_exact(&mut header).await?;
        let payload_len = payload_len_of(&header)?;
        let mut frame_bytes = header.to_vec();
        frame_bytes.resize(header.len() + payload_len, 0);
        self.recv_exact(&mut frame_bytes[header.len()..]).await?;
        Ok(Frame::decode(&frame_bytes, limits)?)
    }

    pub async fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), StreamError> {
        self.recv
            .read_exact(buf)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }
}

/// Frozen layout: [version u8][type u16 BE][payload_len u32 BE]. The hard cap
/// bounds every frame before any allocation (it is far below u32::MAX, so the
/// cast is lossless).
fn payload_len_of(header: &[u8; ak_wire::HEADER_LEN]) -> Result<usize, StreamError> {
    let declared = u32::from_be_bytes([header[3], header[4], header[5], header[6]]);
    if declared as usize > ak_wire::MAX_FRAME_BYTES {
        return Err(StreamError::Wire(ak_wire::DecodeError::Oversized {
            declared,
            budget: ak_wire::MAX_FRAME_BYTES as u32,
        }));
    }
    Ok(declared as usize)
}

/// UDP simultaneous-open probing for NAT traversal (control plane coordinates
/// the attempt; the data path stays strictly peer-to-peer on this socket).
pub mod holepunch {
    use std::io;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};
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

    /// Non-cryptographic nonce: probe packets carry no authentication claim
    /// (the QUIC handshake does all identity work), they only need to be
    /// distinguishable from stray traffic.
    fn probe_nonce() -> [u8; 16] {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut nonce = [0u8; 16];
        nonce[..8].copy_from_slice(&nanos.to_le_bytes());
        nonce[8..].copy_from_slice(&count.wrapping_add(nanos.rotate_left(17)).to_le_bytes());
        nonce
    }

    /// From a socket already bound by the caller (the same socket later used
    /// for QUIC), send probes to every candidate and listen for peer probes
    /// for `duration`, `interval` between rounds.
    ///
    /// Transient socket errors that carry no reachability information (on
    /// Windows, probing a closed port makes a later `recv` fail with
    /// WSAECONNRESET) are skipped; only hard errors abort the probe.
    pub async fn probe(
        socket: &tokio::net::UdpSocket,
        candidates: &[SocketAddr],
        interval: Duration,
        duration: Duration,
    ) -> io::Result<PunchReport> {
        let start = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + duration;
        let packet = probe_packet(&probe_nonce());
        // Unspecified bind addresses (0.0.0.0/::) are not dialable peers, and
        // an IPv4 socket cannot reach IPv6 candidates (or vice versa).
        let socket_is_v4 = socket
            .local_addr()
            .map(|addr| addr.is_ipv4())
            .unwrap_or(true);
        let dialable: Vec<SocketAddr> = candidates
            .iter()
            .copied()
            .filter(|candidate| !candidate.ip().is_unspecified())
            .filter(|candidate| candidate.is_ipv4() == socket_is_v4)
            .collect();
        let mut send_ticker = tokio::time::interval(interval);
        send_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut buffer = [0u8; 1500];
        let mut report = PunchReport::default();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = send_ticker.tick() => {
                    for candidate in &dialable {
                        match socket.send_to(&packet, candidate).await {
                            Ok(_) => report.packets_sent += 1,
                            Err(e) if transient_udp_error(&e) => continue,
                            Err(e) => return Err(e),
                        }
                    }
                }
                received = socket.recv_from(&mut buffer) => {
                    let (len, source) = match received {
                        Ok(received) => received,
                        Err(e) if transient_udp_error(&e) => continue,
                        Err(e) => return Err(e),
                    };
                    if is_probe_packet(&buffer[..len]) {
                        report.packets_received += 1;
                        report.peer_observed_from.get_or_insert(source);
                        if dialable.contains(&source) && !report.reachable.contains(&source) {
                            report.reachable.push(source);
                        }
                    }
                }
            }
        }
        report.elapsed = start.elapsed();
        Ok(report)
    }

    /// Errors that only mean "this one datagram is gone", not "probing is
    /// impossible". SIO_UDP_CONNRESET on Windows turns ICMP port-unreachable
    /// into ConnectionReset on the next operation.
    fn transient_udp_error(err: &io::Error) -> bool {
        matches!(
            err.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::TimedOut
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{Ipv4Addr, Ipv6Addr};

    use ak_core::policy::PathKind;
    use ak_identity::tls::TlsBundle;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn v6(seg: [u16; 8]) -> IpAddr {
        IpAddr::V6(Ipv6Addr::from(seg))
    }

    fn net4(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> classify::Net {
        classify::Net {
            addr: v4(a, b, c, d),
            prefix_len: prefix,
        }
    }

    // ------------------------------------------------------------------
    // classify
    // ------------------------------------------------------------------

    #[test]
    fn classify_same_subnet_is_local() {
        let nets = [net4(192, 168, 1, 10, 24)];
        assert_eq!(
            classify::classify(v4(192, 168, 1, 200), &nets),
            PathKind::LocalDirect
        );
        assert!(classify::same_net(
            v4(192, 168, 1, 10),
            v4(192, 168, 1, 200),
            nets[0]
        ));
    }

    #[test]
    fn classify_public_v4_other_subnet_is_internet() {
        let nets = [net4(192, 168, 1, 10, 24)];
        assert_eq!(
            classify::classify(v4(203, 0, 113, 5), &nets),
            PathKind::InternetDirect
        );
        // even another private range that is not our subnet
        assert_eq!(
            classify::classify(v4(10, 1, 2, 3), &nets),
            PathKind::InternetDirect
        );
    }

    #[test]
    fn classify_ipv6_link_local_is_local() {
        let nets = [];
        let fe80 = v6([0xfe80, 0, 0, 0, 0, 0, 0, 0x1]);
        assert_eq!(classify::classify(fe80, &nets), PathKind::LocalDirect);
        assert!(classify::is_ipv6_link_local(Ipv6Addr::from([
            0xfe80, 0xabcd, 0, 0, 0, 0, 0, 1
        ])));
        assert!(!classify::is_ipv6_link_local(Ipv6Addr::from([
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1
        ])));
    }

    #[test]
    fn classify_same_ula_prefix_is_local() {
        let nets = [classify::Net {
            addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
            prefix_len: 64,
        }];
        let peer = v6([0xfd00, 0, 0, 0, 0x1111, 0x2222, 0x3333, 0x4444]);
        assert_eq!(classify::classify(peer, &nets), PathKind::LocalDirect);
    }

    #[test]
    fn classify_different_v6_prefix_is_internet() {
        let nets = [classify::Net {
            addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
            prefix_len: 64,
        }];
        let peer = v6([0xfd00, 0, 0, 0x0001, 0, 0, 0, 1]); // different /64
        assert_eq!(classify::classify(peer, &nets), PathKind::InternetDirect);
        let global = v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]);
        assert_eq!(classify::classify(global, &nets), PathKind::InternetDirect);
    }

    #[test]
    fn classify_loopback_is_local() {
        let nets = [];
        assert_eq!(
            classify::classify(v4(127, 0, 0, 1), &nets),
            PathKind::LocalDirect
        );
        assert_eq!(
            classify::classify(v6([0, 0, 0, 0, 0, 0, 0, 1]), &nets),
            PathKind::LocalDirect
        );
    }

    #[test]
    fn same_net_rejects_family_mismatch_and_edge_prefixes() {
        let n4 = net4(10, 0, 0, 1, 8);
        let n6 = classify::Net {
            addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
            prefix_len: 64,
        };
        assert!(!classify::same_net(
            v4(10, 1, 2, 3),
            v6([0xfd00, 0, 0, 0, 0, 0, 0, 2]),
            n4
        ));
        assert!(!classify::same_net(
            v6([0xfd00, 0, 0, 0, 0, 0, 0, 2]),
            v4(10, 1, 2, 3),
            n6
        ));
        // /0 IPv4 matches everything in-family
        let any = classify::Net {
            addr: v4(0, 0, 0, 0),
            prefix_len: 0,
        };
        assert!(classify::same_net(v4(1, 2, 3, 4), v4(5, 6, 7, 8), any));
        // out-of-range prefix lengths are clamped to the family width (/32)
        let host = classify::Net {
            addr: v4(10, 1, 2, 3),
            prefix_len: 200,
        };
        assert!(classify::same_net(v4(10, 1, 2, 3), v4(10, 1, 2, 3), host));
        assert!(!classify::same_net(v4(10, 1, 2, 4), v4(10, 1, 2, 3), host));
    }

    #[test]
    fn ipv4_private_helper() {
        assert!(classify::is_ipv4_private(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(classify::is_ipv4_private(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(classify::is_ipv4_private(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(classify::is_ipv4_private(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(classify::is_ipv4_private(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!classify::is_ipv4_private(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!classify::is_ipv4_private(Ipv4Addr::new(11, 0, 0, 1)));
        assert!(!classify::is_ipv4_private(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn local_nets_excludes_loopback_interfaces() {
        let nets = classify::local_nets().expect("interface enumeration");
        for net in &nets {
            assert!(
                !net.addr.is_loopback(),
                "loopback leaked into local_nets: {net:?}"
            );
        }
    }

    // ------------------------------------------------------------------
    // certificate parsing / verification
    // ------------------------------------------------------------------

    /// Build a real self-signed Ed25519 bundle with rcgen. Used when the
    /// parallel ak-identity crate has not landed its implementation yet.
    fn rcgen_bundle() -> (TlsBundle, [u8; 32]) {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 key");
        let params =
            rcgen::CertificateParams::new(vec!["ak-device".to_string()]).expect("cert params");
        let certificate = params.self_signed(&key_pair).expect("self-signed cert");
        let spki = key_pair.public_key_der();
        // Ed25519 SPKI is a 12-byte header followed by the raw 32-byte key.
        let public: [u8; 32] = spki[spki.len() - 32..]
            .try_into()
            .expect("ed25519 public key");
        let bundle = TlsBundle {
            cert_der: certificate.der().to_vec(),
            key_pkcs8_der: key_pair.serialized_der().to_vec(),
            fingerprint: Fingerprint::from_public_key(&public),
        };
        (bundle, public)
    }

    /// Prefer the real `ak-identity` implementation; fall back to rcgen while
    /// `DeviceKeyPair::generate` / `make_self_signed` are still `todo!()` stubs.
    fn test_identity() -> (TlsBundle, [u8; 32]) {
        let real = std::panic::catch_unwind(|| {
            let key = ak_identity::DeviceKeyPair::generate();
            let public = key.public_key();
            let bundle = ak_identity::tls::make_self_signed(&key);
            (bundle, public)
        });
        if let Ok((bundle, public)) = real {
            if ak_identity::tls::self_signed_public_key(&bundle.cert_der).is_ok() {
                return (bundle, public);
            }
        }
        rcgen_bundle()
    }

    #[test]
    fn self_signed_public_key_accepts_real_cert() {
        let (bundle, public) = test_identity();
        assert_eq!(
            ak_identity::tls::self_signed_public_key(&bundle.cert_der).ok(),
            Some(public)
        );
    }

    #[test]
    fn self_signed_public_key_rejects_garbage() {
        assert!(ak_identity::tls::self_signed_public_key(&[]).is_err());
        assert!(ak_identity::tls::self_signed_public_key(&[0x30, 0x03, 0x02, 0x01, 0x00]).is_err());
        assert!(ak_identity::tls::self_signed_public_key(b"not a certificate at all").is_err());
    }

    #[test]
    fn self_signed_public_key_rejects_tampered_cert() {
        let (bundle, _) = test_identity();
        // Tamper with the signed TBS certificate body (inside the serial number).
        let mut tampered_tbs = bundle.cert_der.clone();
        tampered_tbs[12] ^= 0xff;
        assert!(ak_identity::tls::self_signed_public_key(&tampered_tbs).is_err());
        // Tamper with the signature (last byte of the DER).
        let mut tampered_sig = bundle.cert_der.clone();
        let last = tampered_sig.len() - 1;
        tampered_sig[last] ^= 0xff;
        assert!(ak_identity::tls::self_signed_public_key(&tampered_sig).is_err());
    }

    // ------------------------------------------------------------------
    // endpoint / channel integration (loopback, port 0)
    // ------------------------------------------------------------------

    fn loopback_addr() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bind_reports_address_and_classifies() {
        let (bundle, _) = test_identity();
        let endpoint = DirectEndpoint::bind(loopback_addr(), &bundle)
            .await
            .expect("bind");
        let local = endpoint.local_addr().expect("local addr");
        assert_eq!(local.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(local.port(), 0);
        assert_eq!(
            endpoint.classify_peer(v4(127, 0, 0, 1)),
            PathKind::LocalDirect
        );
        // RFC 5737 documentation range is never a local subnet in practice.
        assert_eq!(
            endpoint.classify_peer(v4(192, 0, 2, 10)),
            PathKind::InternetDirect
        );
        let _ = endpoint.local_nets();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_accept_roundtrip_and_streams() {
        let (client_id, client_public) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        let server_task = tokio::spawn(async move { server.accept(&|_| true).await });
        let client_channel = client
            .connect(server_addr, &server_id.fingerprint)
            .await
            .expect("client connect");
        let server_channel = server_task.await.expect("join").expect("server accept");

        // Identity pins in both directions.
        assert_eq!(client_channel.peer_fingerprint(), &server_id.fingerprint);
        assert_eq!(
            server_channel.peer_fingerprint().as_bytes(),
            client_id.fingerprint.as_bytes()
        );
        assert_eq!(
            server_channel.peer_fingerprint(),
            &Fingerprint::from_public_key(&client_public)
        );
        // Loopback is a local direct path.
        assert_eq!(client_channel.path(), PathKind::LocalDirect);
        assert_eq!(server_channel.path(), PathKind::LocalDirect);
        assert!(client_channel.healthy() && server_channel.healthy());

        // open_bi / accept_bi plus raw byte roundtrips in both directions.
        // Send before awaiting accept_bi: a QUIC stream only becomes visible
        // to the peer once it carries data (or FIN).
        let mut cstream = client_channel.open_bi().await.expect("client open_bi");
        cstream
            .send_bytes(b"hello from client")
            .await
            .expect("client send");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");

        let mut buf = [0u8; 17];
        sstream.recv_exact(&mut buf).await.expect("server recv");
        assert_eq!(&buf, b"hello from client");

        sstream
            .send_bytes(b"hi from server")
            .await
            .expect("server send");
        let mut buf = [0u8; 14];
        cstream.recv_exact(&mut buf).await.expect("client recv");
        assert_eq!(&buf, b"hi from server");

        // Server-initiated stream flows the other way.
        let mut sstream2 = server_channel.open_bi().await.expect("server open_bi");
        sstream2.send_bytes(b"ping").await.expect("server send 2");
        let mut cstream2 = client_channel.accept_bi().await.expect("client accept_bi");
        let mut buf = [0u8; 4];
        cstream2.recv_exact(&mut buf).await.expect("client recv 2");
        assert_eq!(&buf, b"ping");

        // Half-close keeps the receiving side usable.
        cstream.finish_send().expect("finish");
        cstream
            .send_bytes(b"after finish")
            .await
            .expect_err("write after finish");
        let tail = sstream.recv.read_to_end(1024).await.expect("read_to_end");
        assert!(tail.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_pin_fails_closed() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        // Let the server consume (and reject) the aborted handshake instead of
        // leaving it to the idle timeout at teardown.
        let server_task = tokio::spawn(async move { server.accept(&|_| true).await });

        let mut wrong = *server_id.fingerprint.as_bytes();
        wrong[0] ^= 0xff;
        let pinned = Fingerprint::from_bytes(wrong);

        let result = client.connect(server_addr, &pinned).await;
        match result {
            Err(ConnectError::PeerIdentity(_)) | Err(ConnectError::Transport(_)) => {}
            Err(other) => panic!("unexpected error class: {other:?}"),
            Ok(channel) => panic!(
                "channel must not exist on pin mismatch: {:?}",
                channel.path()
            ),
        }
        // The server side must also fail: no channel is ever produced.
        assert!(server_task.await.expect("join").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unauthorized_accept_closes_connection() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        let server_task = tokio::spawn(async move { server.accept(&|_| false).await });
        let client_result = client.connect(server_addr, &server_id.fingerprint).await;

        let server_result = server_task.await.expect("join");
        assert!(matches!(server_result, Err(ConnectError::Unauthorized)));

        // Connect itself failing after the server aborted is also acceptable.
        if let Ok(channel) = client_result {
            // The close frame must reach the client: wait for it.
            for _ in 0..100 {
                if !channel.healthy() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(!channel.healthy(), "connection should be closed");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recv_frame_rejects_oversized_header_before_allocating() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        let server_task = tokio::spawn(async move { server.accept(&|_| true).await });
        let client_channel = client
            .connect(server_addr, &server_id.fingerprint)
            .await
            .expect("client connect");
        let server_channel = server_task.await.expect("join").expect("server accept");

        let mut cstream = client_channel.open_bi().await.expect("client open_bi");
        // [version][type u16][payload_len u32 BE] declaring 0xFFFFFFFF bytes.
        // Send before awaiting accept_bi so the peer can see the stream.
        cstream
            .send_bytes(&[0x01, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff])
            .await
            .expect("send oversized header");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");

        let limits = Limits::default();
        match sstream.recv_frame(&limits).await {
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized { declared, budget })) => {
                assert_eq!(declared, u32::MAX);
                assert_eq!(budget as usize, ak_wire::MAX_FRAME_BYTES);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_frame_recv_frame_roundtrip() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        let server_task = tokio::spawn(async move { server.accept(&|_| true).await });
        let client_channel = client
            .connect(server_addr, &server_id.fingerprint)
            .await
            .expect("client connect");
        let server_channel = server_task.await.expect("join").expect("server accept");

        let frame = Frame {
            version: ak_wire::VERSION,
            message: ak_wire::Message::Control {
                job_id: [0x0a; 16],
                control: ak_wire::Control::Pause,
            },
        };
        let mut cstream = client_channel.open_bi().await.expect("client open_bi");
        cstream.send_frame(&frame).await.expect("client send_frame");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");
        let decoded = sstream
            .recv_frame(&Limits::default())
            .await
            .expect("server recv_frame");
        assert_eq!(decoded, frame);
    }

    // ------------------------------------------------------------------
    // holepunch
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn holepunch_probe_reports_reachable_peer() {
        let a = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind a");
        let b = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind b");
        let a_addr = a.local_addr().expect("addr a");
        let b_addr = b.local_addr().expect("addr b");

        let b_task = tokio::spawn(async move {
            // B listens slightly longer than A so B's socket outlives A's
            // probes (Windows reports resets against dropped peers).
            holepunch::probe(
                &b,
                &[a_addr],
                Duration::from_millis(25),
                Duration::from_millis(800),
            )
            .await
        });
        let a_report = holepunch::probe(
            &a,
            &[b_addr],
            Duration::from_millis(25),
            Duration::from_millis(500),
        )
        .await
        .expect("probe a");
        let b_report = b_task.await.expect("join").expect("probe b");

        assert!(a_report.packets_sent > 0, "A sent no probes");
        assert!(a_report.packets_received > 0, "A received no peer probes");
        assert!(
            a_report.reachable.contains(&b_addr),
            "B not reachable: {a_report:?}"
        );
        assert_eq!(a_report.peer_observed_from, Some(b_addr));
        assert!(
            b_report.reachable.contains(&a_addr),
            "A not reachable: {b_report:?}"
        );
        assert!(a_report.elapsed >= Duration::from_millis(500));
    }

    #[test]
    fn probe_packet_shape_roundtrip() {
        let nonce = [7u8; 16];
        let packet = holepunch::probe_packet(&nonce);
        assert!(holepunch::is_probe_packet(&packet));
        assert!(!holepunch::is_probe_packet(b"AKPUNCH1"));
        assert!(!holepunch::is_probe_packet(&packet[..23]));
        let mut wrong_tag = packet.clone();
        wrong_tag[0] = b'X';
        assert!(!holepunch::is_probe_packet(&wrong_tag));
    }
}
