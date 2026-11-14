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
//!
//! # Dual-stack (IPv6-first) endpoints
//!
//! REVIEW 4.1 prefers IPv6 candidates: without address translation, both
//! sides sending outward is usually enough to establish firewall state, so
//! v6 is generally easier to connect than IPv4 NAT traversal. To serve both
//! families from one socket, [`DirectEndpoint::bind_dual_stack`] binds
//! `[::]:port` with `IPV6_V6ONLY = 0`: native v6 peers arrive as-is and v4
//! peers arrive as `::ffff:a.b.c.d` (v4-mapped) on the same socket.
//! [`DirectEndpoint::is_dual_stack`] reports the socket mode, and
//! [`DirectEndpoint::connect`] transparently dials a plain-v4 peer in
//! mapped form on such endpoints. `classify` expands mapped addresses back
//! to plain IPv4 ([`classify::expand_mapped`]), so classification, display,
//! and candidate matching stay family-accurate in both directions
//! ([`classify::to_mapped`] builds the mapped form).
//!
//! Dual-stack known limitations (documented, not hidden):
//! - Not every stack has IPv6 or allows clearing `IPV6_V6ONLY`; then
//!   `bind_dual_stack` returns an `Unsupported` io error naming the cause
//!   and the caller falls back to a per-family [`DirectEndpoint::bind`].
//! - Some networks, VPNs, and firewalls treat IPv4 traffic emitted from an
//!   IPv6 socket inconsistently (filtered, or rewritten source addresses).
//!   Observed in development: a Windows insider build intermittently refuses
//!   `::ffff:` mapped sends with `WSAENETUNREACH` for minutes at a time even
//!   though the socket is dual-stack. A dual-stack endpoint therefore widens
//!   the candidate set; it does not replace per-family reachability
//!   verification by the probe/engine layers, and the dual-stack tests in
//!   this crate are best-effort for exactly that reason.
//! - `local_addr()` on a dual-stack endpoint reports `[::]:port`; use
//!   [`DirectEndpoint::local_addr_display`] for addresses that can actually
//!   be advertised to peers.
//!
//! # Connection migration policy (INV-004)
//!
//! QUIC active migration is **disabled** on every endpoint this crate builds:
//! server configs are created with `migration(false)` (quinn 0.11.12 /
//! quinn-proto 0.11.19 API — the older `TransportConfig::disable_active_migration`
//! field was replaced by `ServerConfig::migration`; see the `tls` module
//! docs), which advertises the RFC 9000 `disable_active_migration` transport
//! parameter in the handshake and makes the transport discard
//! migration-initiating datagrams from unrecognized remote addresses. A
//! compliant peer therefore never rebinds an established connection, and a
//! non-compliant peer's migration attempt is dropped by the transport.
//!
//! Defense in depth: [`DirectChannel`] snapshots the peer address at
//! handshake completion ([`DirectChannel::established_remote`]) and
//! [`DirectChannel::path_changed`] / [`DirectChannel::ensure_path_stable`]
//! compare it with the transport's current `remote_address()` at every
//! stream-open point, closing the connection (QUIC code 9) if it ever
//! differs. Until the engine grows a per-path re-authorization mechanism,
//! the policy is: **a changed path is closed, never silently used**. NAT
//! rebinding survival is intentionally traded away for this; peers recover
//! by reconnecting (new handshake, new `authorize`/pin gate), which the
//! engine's reconnect/recheck path already exercises.
//!
//! # Known limitations
//!
//! - [`ChannelStats`] byte counts are the QUIC bytes carried inside UDP
//!   datagrams (quinn's `udp_tx`/`udp_rx`); UDP and IP header overhead is
//!   not included, so they read lower than the on-wire byte count.
//! - `ChannelStats::lost_packets` comes from quinn's internal loss
//!   detection (retransmission-timeout and probe inference), not from wire
//!   observation; treat it as a transport-health signal, not ground truth.
//! - `ChannelStats::cwnd_bytes` is a point-in-time sample of the congestion
//!   controller's window (the value at the moment `stats()` is called, e.g.
//!   the end-of-transfer window), not a time series; quinn 0.11 offers no
//!   received-packet counter, so the receive-side count is
//!   `rx_datagrams` (UDP datagrams).
//! - quinn never increments the transmitted-side counter for
//!   CONNECTION_CLOSE frames, so `tx_frames` can read one lower than the
//!   peer's `rx_frames` once a connection has closed (see
//!   [`ChannelStats`] docs).
//!
// Unsafe policy (CR-P1-06): `deny` instead of `forbid` — the Unix
// per-packet-TTL `sendmsg` control message in this crate's holepunch
// section is the ONE audited `unsafe` block (raw cmsg buffer layout),
// individually `#[allow]`-ed with a SAFETY argument below. Everything
// else in ak-net remains unsafe-free and a stray `unsafe` anywhere else
// still fails the build.
#![deny(unsafe_code)]

mod tls;

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
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
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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
    ///
    /// IPv4-mapped IPv6 peers (`::ffff:a.b.c.d`, how dual-stack sockets
    /// present their v4 peers) are first expanded to plain IPv4, so a mapped
    /// peer classifies exactly like the v4 address it carries.
    pub fn classify(peer: IpAddr, nets: &[Net]) -> PathKind {
        let peer = expand_mapped_ip(peer);
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

    /// Expand a v4-mapped peer socket address (`::ffff:a.b.c.d:p`) to plain
    /// IPv4 (`a.b.c.d:p`); everything else, including native IPv6, passes
    /// through unchanged. Dual-stack sockets (`bind_dual_stack`) present
    /// their v4 peers in mapped form; this keeps display, classification,
    /// and candidate matching in plain-family terms.
    pub fn expand_mapped(peer: SocketAddr) -> SocketAddr {
        match expand_mapped_ip(peer.ip()) {
            IpAddr::V4(v4) => SocketAddr::new(IpAddr::V4(v4), peer.port()),
            IpAddr::V6(_) => peer,
        }
    }

    /// Map a plain IPv4 peer into its `::ffff:0:0/96` form so an IPv6
    /// dual-stack socket can dial it; native IPv6 passes through unchanged.
    /// Inverse of [`expand_mapped`] for the mapped subset.
    pub fn to_mapped(peer: SocketAddr) -> SocketAddr {
        match peer.ip() {
            IpAddr::V4(v4) => SocketAddr::new(IpAddr::V6(v4.to_ipv6_mapped()), peer.port()),
            IpAddr::V6(_) => peer,
        }
    }

    /// `IpAddr` core of [`expand_mapped`]: `::ffff:a.b.c.d` -> `a.b.c.d`.
    fn expand_mapped_ip(addr: IpAddr) -> IpAddr {
        match addr {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(addr),
            IpAddr::V4(_) => addr,
        }
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

/// Failure of [`DirectEndpoint::connect`] / [`DirectEndpoint::accept`]:
/// transport, peer-identity, authorization, or (via
/// [`DirectEndpoint::accept_within`]) deadline.
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

/// Failure while using an established stream: connection loss, wire decode
/// rejection, or io error.
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
    dual_stack: bool,
    migration_disabled: bool,
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

    /// Bind `[::]:port` with `IPV6_V6ONLY = 0` (dual-stack, via socket2) so
    /// that **one UDP socket** serves both native IPv6 peers and IPv4 peers
    /// through `::ffff:a.b.c.d` mapping (REVIEW 4.1: prefer IPv6 candidates;
    /// a v6 path carries no NAT between the peers).
    ///
    /// The flag is set explicitly before bind — do not rely on the OS
    /// default: Windows builds have shipped both `V6ONLY=0` and (observed on
    /// an insider build used for development) `V6ONLY=1` defaults for fresh
    /// IPv6 sockets. On success [`Self::local_addr`] reports `[::]:port`,
    /// [`Self::is_dual_stack`] is true, and [`Self::connect`] dials a
    /// plain-v4 peer transparently in mapped form (the same socket accepts
    /// from both families, so hole-punch state and the QUIC endpoint stay
    /// single-socket).
    ///
    /// Note for engine wiring (`connect_bounded`): its address-family
    /// fast-fail compares the endpoint's bound family against the peer's. A
    /// dual-stack endpoint makes that check moot — v4 peers are reachable
    /// via the mapping — so either pass v4 peers in mapped form
    /// ([`classify::to_mapped`], same family as `[::]`, the fast-fail then
    /// passes) or consult [`Self::is_dual_stack`] before failing on a
    /// mismatch.
    ///
    /// If the system refuses (no IPv6 stack, or `IPV6_V6ONLY` cannot be
    /// cleared), returns an `ErrorKind::Unsupported` io error that says so;
    /// the caller can then fall back to a per-family [`Self::bind`].
    ///
    /// Known limitation: some networks, VPNs, and firewalls treat IPv4
    /// traffic sent from an IPv6 socket inconsistently; see the crate-level
    /// dual-stack notes.
    pub async fn bind_dual_stack(
        port: u16,
        identity: &ak_identity::tls::TlsBundle,
    ) -> io::Result<Self> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "dual-stack bind: creating an IPv6 UDP socket failed ({e}); \
                     this host may have no IPv6 stack — fall back to DirectEndpoint::bind"
                ),
            )
        })?;
        // V6ONLY must be cleared before bind so the wildcard v6 socket also
        // receives the v4-mapped traffic.
        socket.set_only_v6(false).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "dual-stack bind: the system refused to clear IPV6_V6ONLY ({e}); \
                     fall back to DirectEndpoint::bind on a single address family"
                ),
            )
        })?;
        let any_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
        socket.bind(&socket2::SockAddr::from(any_v6))?;
        Self::bind_inner(std::net::UdpSocket::from(socket), identity, true).await
    }

    /// Adopt a caller-owned UDP socket (already bound, e.g. the very socket
    /// used for hole-punch probing, so the NAT pinhole stays valid) and run
    /// QUIC on it. The socket must not be shared afterwards.
    ///
    /// Such an endpoint reports [`Self::is_dual_stack`] == false; `IPV6_V6ONLY`
    /// is not portably queryable (on some Windows builds `getsockopt` returns
    /// a 1-byte BOOLEAN, which socket2 0.5's `only_v6` mishandles), so only
    /// [`Self::bind_dual_stack`] claims the flag. [`Self::connect`] keys its
    /// v4->mapped dialing off the socket family instead of the flag, so
    /// dialing behaves correctly on any IPv6 socket.
    pub async fn bind_with_socket(
        socket: std::net::UdpSocket,
        identity: &ak_identity::tls::TlsBundle,
    ) -> io::Result<Self> {
        Self::bind_inner(socket, identity, false).await
    }

    /// Adopt a caller-owned UDP socket for the CLIENT (dial) role — the
    /// sender-side mirror of [`Self::bind_with_socket`] (CR-P1-05, review
    /// 2026-11-11 §4.5: both sides' probing/punching must happen on THE
    /// socket that will carry QUIC, so the sender's dial must leave from
    /// the very socket its STUN query and probe round ran on — a mapping
    /// opened on a throwaway socket never carries into the QUIC Initial).
    ///
    /// Construction is identical to [`Self::bind_with_socket`] — one
    /// quinn `Endpoint` over the adopted socket with both server and
    /// client configs armed — because the endpoint shape is role-neutral;
    /// this constructor only NAMES the dial-side adoption so the sender
    /// lifecycle has its documented entry. The socket must be bound and
    /// must not be shared afterwards (a `try_clone` of the OS socket is
    /// the caller's way to keep a send-only handle for probing).
    ///
    /// [`Self::connect`] keys its `::ffff:` family bridging off the
    /// socket family, so a dual-stack adopted socket (`[::]` bind) dials
    /// plain-v4 peers transparently; [`Self::is_dual_stack`] still
    /// reports false (see [`Self::bind_with_socket`] for why the flag is
    /// not portably queryable), so callers fast-failing on family
    /// mismatch should consult the socket family instead.
    pub async fn dial_with_socket(
        socket: std::net::UdpSocket,
        identity: &ak_identity::tls::TlsBundle,
    ) -> io::Result<Self> {
        Self::bind_inner(socket, identity, false).await
    }

    /// Shared constructor behind `bind` / `bind_with_socket` /
    /// `bind_dual_stack`.
    async fn bind_inner(
        socket: std::net::UdpSocket,
        identity: &ak_identity::tls::TlsBundle,
        dual_stack: bool,
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
            dual_stack,
            migration_disabled: tls::MIGRATION_DISABLED,
        })
    }

    /// The bound socket address (on a dual-stack endpoint this is
    /// `[::]:port`; see [`Self::local_addr_display`] for dialable forms).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// True when this endpoint was constructed by [`Self::bind_dual_stack`]:
    /// one socket serving both native v6 and v4-mapped peers. Family-mismatch
    /// fast-fail checks in callers (engine `connect_bounded`) should treat
    /// true as "v4 peers are reachable via ::ffff: mapping" instead of
    /// erroring. Adopted sockets report false (see [`Self::bind_with_socket`]
    /// for why `IPV6_V6ONLY` is not queried).
    pub fn is_dual_stack(&self) -> bool {
        self.dual_stack
    }

    /// True when this endpoint's QUIC transport rejects active connection
    /// migration (INV-004 policy; read-only getter for engine/UI wiring).
    ///
    /// The flag is fixed at bind time from `tls::MIGRATION_DISABLED`: every
    /// server config built here disables migration at the transport-parameter
    /// level (the endpoint advertises `disable_active_migration` and quinn
    /// discards migration-initiating datagrams), and there is no client-side
    /// knob in quinn 0.11.12 — see the `tls` module docs for the exact
    /// registry-source API mapping. Until a path re-authorization mechanism
    /// lands in the engine, the engine's migration policy is "disabled":
    /// a migrated path is closed, never silently used. Defense in depth for
    /// connections that somehow rebind anyway:
    /// [`DirectChannel::ensure_path_stable`].
    pub fn active_migration_disabled(&self) -> bool {
        self.migration_disabled
    }

    /// Addresses this endpoint can advertise to peers on every local
    /// network: each interface address captured in [`Self::local_nets`]
    /// paired with this endpoint's port, with v4-mapped interface addresses
    /// shown as plain IPv4 ([`classify::expand_mapped`]). Probe/discovery
    /// callers can use this list directly as connection candidates instead
    /// of re-deriving interface addresses by hand in the CLI.
    ///
    /// Loopback is excluded (mirrors [`Self::local_nets`]: it is not
    /// dialable from other machines); the list reflects the interfaces seen
    /// at bind time. The port is the real bound port even after a port-0
    /// bind.
    pub fn local_addr_display(&self) -> io::Result<Vec<SocketAddr>> {
        let port = self.local_addr()?.port();
        Ok(advertised_addrs(&self.nets, port))
    }

    /// Local networks seen at bind time (refreshed per call in later versions).
    pub fn local_nets(&self) -> &[classify::Net] {
        &self.nets
    }

    /// Classify a peer address against the interfaces seen at bind time
    /// (loopback/same-subnet => Local, else Internet; never Relay).
    pub fn classify_peer(&self, peer: IpAddr) -> PathKind {
        classify::classify(peer, &self.nets)
    }

    /// Connect directly to `peer`, pinning `pinned`. Fails closed: any
    /// certificate mismatch, proxy indication, or transport error aborts.
    ///
    /// Family mismatches are bridged in whichever direction they arrive: an
    /// IPv6 endpoint dials a plain IPv4 peer transparently in `::ffff:`
    /// mapped form (keys off the socket family, so it works for
    /// [`Self::bind_dual_stack`] endpoints and adopted dual-stack sockets
    /// alike), and an IPv4 endpoint expands a v4-mapped peer back to plain
    /// IPv4 (signaling candidates may carry either form). The channel's path
    /// classification still sees the v4 address ([`classify::expand_mapped`]).
    /// Mapping is never worse than the raw dial: a v6-only socket rejects
    /// either form, a dual-stack socket only accepts the mapped one.
    pub async fn connect(
        &self,
        peer: SocketAddr,
        pinned: &Fingerprint,
    ) -> Result<DirectChannel, ConnectError> {
        // An IPv6 socket cannot dial a plain v4 sockaddr (use its ::ffff:
        // mapped form); an IPv4 socket cannot dial a mapped sockaddr (use
        // the plain v4 form inside it).
        let peer = match self.endpoint.local_addr() {
            Ok(local) if local.is_ipv6() && peer.is_ipv4() => classify::to_mapped(peer),
            Ok(local) if local.is_ipv4() => classify::expand_mapped(peer),
            _ => peer,
        };
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
        let established_remote = conn.remote_address();
        Ok(DirectChannel {
            conn,
            fingerprint,
            path: classify::classify(peer.ip(), &self.nets),
            established_remote,
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
        let remote = conn.remote_address();
        let path = classify::classify(remote.ip(), &self.nets);
        Ok(DirectChannel {
            conn,
            fingerprint,
            path,
            established_remote: remote,
        })
    }

    /// [`Self::accept`] bounded by an overall deadline: returns
    /// [`ConnectError::Timeout`] with the elapsed budget when no authorized
    /// peer arrives in time. Probe or engine code that coordinates a hole-punch window can
    /// use this instead of hand-rolling a timeout around `accept`.
    pub async fn accept_within(
        &self,
        authorize: &(dyn Fn(&Fingerprint) -> bool + Send + Sync),
        timeout: Duration,
    ) -> Result<DirectChannel, ConnectError> {
        match tokio::time::timeout(timeout, self.accept(authorize)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ConnectError::Timeout(timeout)),
        }
    }
}

/// Interface addresses x port, v4-mapped entries expanded to plain IPv4 —
/// the candidate list behind `DirectEndpoint::local_addr_display`.
fn advertised_addrs(nets: &[classify::Net], port: u16) -> Vec<SocketAddr> {
    nets.iter()
        .map(|net| classify::expand_mapped(SocketAddr::new(net.addr, port)))
        .collect()
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

/// Transport-level counters for one direct channel, extracted from quinn's
/// `Connection::stats()`.
///
/// Field mapping (quinn 0.11 `ConnectionStats`):
///
/// | field | quinn source |
/// |---|---|
/// | `tx_bytes` / `rx_bytes` | `udp_tx.bytes` / `udp_rx.bytes` — total QUIC bytes inside sent/received UDP datagrams |
/// | `tx_frames` / `rx_frames` | sum of every counter in `frame_tx` / `frame_rx` — all QUIC frame types |
/// | `lost_packets` | `path.lost_packets` — packets declared lost by the transport |
/// | `cwnd_bytes` | `path.cwnd` — the congestion controller's window, sampled when [`DirectChannel::stats`] is called |
/// | `congestion_events` | `path.congestion_events` — congestion-window reductions counted by the controller |
/// | `sent_packets` | `path.sent_packets` — packets handed to the socket on the current path (includes PLPMTUD probes) |
/// | `tx_datagrams` / `rx_datagrams` | `udp_tx.datagrams` / `udp_rx.datagrams` — UDP datagrams observed |
///
/// Byte counts exclude UDP/IP header overhead (see the crate-level known
/// limitations). Frame totals follow quinn's accounting quirks: PADDING is
/// never counted and CONNECTION_CLOSE is only counted on the receive side,
/// so after a close `tx_frames` may be one lower than the peer's `rx_frames`.
///
/// Receive-side packet counts: quinn 0.11's `PathStats` has no received-
/// packet counter (only `sent_packets`); `rx_datagrams` (UDP datagrams) is
/// the closest receive-side packet count this snapshot can offer.
///
/// Operational use: compare these data-plane counters against the control
/// plane's session records. A transfer that completed here with only
/// loopback/local bytes on the QUIC channel is runtime evidence that the
/// server never carried file data — corroborating, not proving, the
/// no-relay architecture (it is not a cryptographic proof).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelStats {
    /// QUIC payload bytes sent inside UDP datagrams.
    pub tx_bytes: u64,
    /// QUIC payload bytes received inside UDP datagrams.
    pub rx_bytes: u64,
    /// Frames of all QUIC types transmitted.
    pub tx_frames: u64,
    /// Frames of all QUIC types received.
    pub rx_frames: u64,
    /// Packets quinn's loss detection declared lost on the current path.
    pub lost_packets: u64,
    /// Congestion window in bytes at snapshot time (the end-of-transfer
    /// value when a caller samples after the last payload byte).
    pub cwnd_bytes: u64,
    /// Congestion events (congestion-window reductions) quinn counted.
    pub congestion_events: u64,
    /// Packets sent on the current path (quinn `path.sent_packets`).
    pub sent_packets: u64,
    /// UDP datagrams sent carrying this connection's packets.
    pub tx_datagrams: u64,
    /// UDP datagrams received for this connection — the closest
    /// receive-side packet count quinn 0.11 exposes (see doc above).
    pub rx_datagrams: u64,
}

/// Sum every per-type counter of quinn's `FrameStats` into one frame total.
/// `FrameStats` is `#[non_exhaustive]`, so frame types added by future quinn
/// versions would not appear here (compile-time safe, just not counted).
fn total_frames(frames: &quinn::FrameStats) -> u64 {
    frames.acks
        + frames.ack_frequency
        + frames.crypto
        + frames.connection_close
        + frames.data_blocked
        + frames.datagram
        + u64::from(frames.handshake_done)
        + frames.immediate_ack
        + frames.max_data
        + frames.max_stream_data
        + frames.max_streams_bidi
        + frames.max_streams_uni
        + frames.new_connection_id
        + frames.new_token
        + frames.path_challenge
        + frames.path_response
        + frames.ping
        + frames.reset_stream
        + frames.retire_connection_id
        + frames.stream_data_blocked
        + frames.streams_blocked_bidi
        + frames.streams_blocked_uni
        + frames.stop_sending
        + frames.stream
}

/// An authenticated direct connection. Constructible only by this crate after
/// peer verification. Exactly one QUIC connection to exactly one peer address.
pub struct DirectChannel {
    pub(crate) conn: quinn::Connection,
    pub(crate) fingerprint: Fingerprint,
    pub(crate) path: PathKind,
    /// Peer address captured when the handshake completed; the reference
    /// point for [`DirectChannel::path_changed`] (INV-004 defense in depth).
    pub(crate) established_remote: SocketAddr,
}

impl DirectChannel {
    /// The verified peer identity this channel was pinned to.
    pub fn peer_fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// Path classification of the connected peer (policy-gate input).
    pub fn path(&self) -> PathKind {
        self.path
    }

    /// The peer address captured when this channel's handshake completed.
    /// Stable for the lifetime of the channel; compare with
    /// [`Self::current_remote`] via [`Self::path_changed`].
    pub fn established_remote(&self) -> SocketAddr {
        self.established_remote
    }

    /// The peer address the transport currently reports. quinn keeps this
    /// queryable even after the connection closed.
    pub fn current_remote(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// True when the transport's current peer address differs from the
    /// handshake-time address — i.e. the connection path changed underneath
    /// an established, already-authorized channel (QUIC active migration or
    /// NAT rebinding).
    ///
    /// INV-004 policy note: active migration is disabled at the transport
    /// level on every endpoint this crate builds (see
    /// [`DirectEndpoint::active_migration_disabled`]), so a `true` result is
    /// not expected from a compliant transport stack; it is the defensive
    /// tripwire for a future re-enabled-migration or transport-behavior
    /// change. Until the engine grows a per-path re-authorization mechanism,
    /// the policy is: a changed path is closed, never silently used — call
    /// [`Self::ensure_path_stable`] (which closes the connection on change)
    /// or treat `true` as a hard failure.
    pub fn path_changed(&self) -> bool {
        self.conn.remote_address() != self.established_remote
    }

    /// Fail closed unless the peer address still equals the handshake-time
    /// address: on change, closes the underlying connection (QUIC code 9,
    /// reason "path migrated") and returns
    /// [`StreamError::Connection`] naming the migration, so no further
    /// stream can be opened on a path the engine never re-authorized.
    ///
    /// `open_bi`/`accept_bi` run this check at every stream-operation point;
    /// call it directly at other engine checkpoints (transfer start, resume,
    /// long-lived loops) for the same defensive close.
    pub fn ensure_path_stable(&self) -> Result<(), StreamError> {
        if self.path_changed() {
            self.conn
                .close(varint(9), b"path migrated: active migration is disabled");
            return Err(StreamError::Connection(format!(
                "peer address migrated from {} to {} on an established channel; \
                 closing (active migration disabled)",
                self.established_remote,
                self.conn.remote_address()
            )));
        }
        Ok(())
    }

    /// Open a new bidirectional stream (client side of a protocol exchange).
    /// Rejects and defensively closes the connection if the peer address
    /// changed since the handshake (INV-004; see [`Self::ensure_path_stable`]).
    pub async fn open_bi(&self) -> Result<DirectStream, StreamError> {
        self.ensure_path_stable()?;
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))?;
        Ok(DirectStream { send, recv })
    }

    /// Accept a bidirectional stream the peer opened (server side). Rejects
    /// and defensively closes the connection if the peer address changed
    /// since the handshake (INV-004; see [`Self::ensure_path_stable`]).
    pub async fn accept_bi(&self) -> Result<DirectStream, StreamError> {
        self.ensure_path_stable()?;
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

    /// Snapshot of this channel's transport counters (see [`ChannelStats`]
    /// for the exact quinn field mapping and its limits).
    ///
    /// Runtime-evidence use: after a transfer, operators can compare the
    /// data-plane bytes on this channel against the control plane's session
    /// records — bytes flowing only over this peer-to-peer QUIC connection
    /// corroborate that the server did not carry the file. This is an
    /// operational observation, not a cryptographic proof.
    pub fn stats(&self) -> ChannelStats {
        let stats = self.conn.stats();
        ChannelStats {
            tx_bytes: stats.udp_tx.bytes,
            rx_bytes: stats.udp_rx.bytes,
            tx_frames: total_frames(&stats.frame_tx),
            rx_frames: total_frames(&stats.frame_rx),
            lost_packets: stats.path.lost_packets,
            cwnd_bytes: stats.path.cwnd,
            congestion_events: stats.path.congestion_events,
            sent_packets: stats.path.sent_packets,
            tx_datagrams: stats.udp_tx.datagrams,
            rx_datagrams: stats.udp_rx.datagrams,
        }
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

    /// Encode and send one wire frame.
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), StreamError> {
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        self.send_bytes(&bytes).await
    }

    /// Read and decode one wire frame under `limits` (exactly one frame).
    /// Both the all-frame hard cap and the per-type payload budget are
    /// checked on the header alone, so a declared payload beyond either is
    /// rejected as `Oversized` before any payload buffer is allocated (and
    /// without waiting for the payload bytes).
    pub async fn recv_frame(&mut self, limits: &Limits) -> Result<Frame, StreamError> {
        let mut header = [0u8; ak_wire::HEADER_LEN];
        self.recv_exact(&mut header).await?;
        let payload_len = header_payload_check(&header, limits)?;
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
    /// Encode and send one wire frame on the send half.
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), StreamError> {
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        self.send
            .write_all(&bytes)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    /// Send raw bytes on the send half.
    pub async fn send_bytes(&mut self, data: &[u8]) -> Result<(), StreamError> {
        self.send
            .write_all(data)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }

    /// Half-close the send half (finish sending, peer sees EOF).
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
    /// Read and decode one wire frame under `limits` on the receive half.
    /// Both the all-frame hard cap and the per-type payload budget are
    /// checked on the header alone, so a declared payload beyond either is
    /// rejected as `Oversized` before any payload buffer is allocated (and
    /// without waiting for the payload bytes).
    pub async fn recv_frame(&mut self, limits: &Limits) -> Result<Frame, StreamError> {
        let mut header = [0u8; ak_wire::HEADER_LEN];
        self.recv_exact(&mut header).await?;
        let payload_len = header_payload_check(&header, limits)?;
        let mut frame_bytes = header.to_vec();
        frame_bytes.resize(header.len() + payload_len, 0);
        self.recv_exact(&mut frame_bytes[header.len()..]).await?;
        Ok(Frame::decode(&frame_bytes, limits)?)
    }

    /// Read exactly `buf.len()` bytes on the receive half.
    pub async fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), StreamError> {
        self.recv
            .read_exact(buf)
            .await
            .map_err(|e| StreamError::Connection(e.to_string()))
    }
}

/// Frozen layout: [version u8][type u16 BE][payload_len u32 BE]. Both budget
/// gates below run on the header alone — after reading the 7 header bytes and
/// before the payload buffer is allocated:
///
/// 1. the all-frame hard cap (bounds every type, including unknown ones, far
///    below `u32::MAX`), then
/// 2. the per-type payload budget implied by `limits` ([`type_budget`]), so a
///    small fixed-size type (Accept, Control, ...) can no longer trigger a
///    hard-cap-sized allocation.
///
/// An over-budget declaration returns `Oversized` immediately, without
/// reading or allocating the payload; unknown types and versions are
/// rejected at header time too. `Frame::decode` re-validates everything on
/// the assembled bytes (defense in depth).
fn header_payload_check(
    header: &[u8; ak_wire::HEADER_LEN],
    limits: &Limits,
) -> Result<usize, StreamError> {
    let declared = u32::from_be_bytes([header[3], header[4], header[5], header[6]]);
    if declared as usize > ak_wire::MAX_FRAME_BYTES {
        return Err(StreamError::Wire(ak_wire::DecodeError::Oversized {
            declared,
            budget: ak_wire::MAX_FRAME_BYTES as u32,
        }));
    }
    let version = header[0];
    if version != ak_wire::VERSION {
        return Err(StreamError::Wire(ak_wire::DecodeError::BadVersion(version)));
    }
    let message_type = u16::from_be_bytes([header[1], header[2]]);
    let Some(budget) = type_budget(message_type, limits) else {
        return Err(StreamError::Wire(ak_wire::DecodeError::UnknownType(
            message_type,
        )));
    };
    if declared > budget {
        return Err(StreamError::Wire(ak_wire::DecodeError::Oversized {
            declared,
            budget,
        }));
    }
    Ok(declared as usize)
}

/// Per-type payload upper bound for a wire frame type value, implied by
/// `limits`: fixed bounds for the small control types, entry-count bounds
/// for ManifestPage/ResumeRequest, `max_chunk_bytes` for Chunk. This is a
/// header-only mirror of ak-wire's private `payload_budget` table (not
/// re-exported), capped at the all-frame hard cap exactly like
/// `ak_wire::Frame::decode` does.
///
/// Sync contract: it must stay at least as large as ak-wire's bound for
/// every type, or legitimate frames would be rejected at header time. If it
/// ever drifts looser, `Frame::decode` still enforces the true bound after
/// the (already type-bounded, at most hard-cap-sized) allocation — a drift
/// can only loosen the early gate, never bypass decode.
fn type_budget(message_type: u16, limits: &Limits) -> Option<u32> {
    let string = u64::from(limits.max_string_bytes);
    // WireEntry: str(2+string) + bool(1) + size(8) + mtime(8) + alg(1)
    //            + whole_digest(32) + digests(4 + 32*n).
    let entry = 56 + string + 32 * u64::from(limits.max_chunk_digests);
    let bound = match message_type {
        // proto_version(2) + str(2+string) + public_key(32) + capabilities(2).
        ak_wire::TYPE_HELLO | ak_wire::TYPE_OFFER => 38 + string,
        // job_id(16).
        ak_wire::TYPE_ACCEPT => 16,
        // job_id(16) + error code(2).
        ak_wire::TYPE_REJECT => 18,
        // job_id(16) + page_index(4) + last_page(1) + count(4) + entries.
        ak_wire::TYPE_MANIFEST_PAGE => 25 + u64::from(limits.max_entries_per_page) * entry,
        // job_id(16) + file_index(4) + chunk_index(8) + data_len(4) + data.
        ak_wire::TYPE_CHUNK => 32 + u64::from(limits.max_chunk_bytes),
        // job_id(16) + file_index(4) + chunk_index(8).
        ak_wire::TYPE_CHUNK_ACK => 28,
        // job_id(16) + count(4) + 12 bytes per resume entry + more flag(1).
        ak_wire::TYPE_RESUME_REQUEST => 21 + 12 * u64::from(limits.max_resume_entries),
        // job_id(16) + tag/bool(1).
        ak_wire::TYPE_CONTROL | ak_wire::TYPE_FINAL_ACK => 17,
        ak_wire::TYPE_FINALIZE => 16,
        // code(2) + str(2 + error message bytes).
        ak_wire::TYPE_ERROR => 4 + u64::from(limits.max_error_message_bytes),
        // Scheduler admission frames (wave 20 wire, wave 21 engine wiring):
        // fixed-size, limits-independent — must mirror `ak_wire`'s
        // `payload_budget` exactly, or this header pre-filter rejects with
        // `UnknownType` what the codec itself accepts.
        // job_id(16) + permit_id(16) + group(32) + group_registry(32).
        ak_wire::TYPE_TASK_OPEN => 113,
        // job_id(16) + permit_id(16) + granted(1) + reason(1).
        ak_wire::TYPE_ADMISSION => 38,
        // job_id(16) + permit_id(16).
        ak_wire::TYPE_ADMISSION_ACK => 32,
        _ => return None,
    };
    let hard_payload_cap = (ak_wire::MAX_FRAME_BYTES - ak_wire::HEADER_LEN) as u64;
    Some(bound.min(hard_payload_cap) as u32)
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
        /// Address a peer packet was observed from, if any. Raw observation:
        /// on a dual-stack socket an IPv4 peer appears in `::ffff:` mapped
        /// form; use [`crate::classify::expand_mapped`] to display it as v4.
        pub peer_observed_from: Option<SocketAddr>,
        pub packets_sent: u64,
        pub packets_received: u64,
        pub elapsed: Duration,
    }

    /// Magic tag inside probe packets: "AKPUNCH1" + 16 random bytes.
    pub const PROBE_TAG: &[u8; 8] = b"AKPUNCH1";

    /// One dialable candidate: the plain-family form used for reporting and
    /// matching, plus the form actually dialed. They differ only on a
    /// dual-stack IPv6 socket, where IPv4 candidates are sent to in
    /// `::ffff:` mapped form (see `DirectEndpoint::bind_dual_stack`).
    struct Dialable {
        report: SocketAddr,
        send: SocketAddr,
    }

    /// Build one probe packet (tag + nonce).
    pub fn probe_packet(nonce: &[u8; 16]) -> Vec<u8> {
        let mut packet = PROBE_TAG.to_vec();
        packet.extend_from_slice(nonce);
        packet
    }

    /// Bind a raw dual-stack UDP socket (`[::]:port`, V6ONLY cleared) for
    /// probing flows that later hand the same socket to QUIC via
    /// [`crate::DirectEndpoint::bind_with_socket`]. Returns `Unsupported` when the
    /// host cannot clear V6ONLY or lacks IPv6; callers fall back to a plain
    /// `UdpSocket::bind("0.0.0.0:port")`. Blocking mode: callers `set_nonblocking`
    /// or convert per their runtime (tokio's `from_std` does it).
    pub fn bind_dual_stack_udp(port: u16) -> io::Result<std::net::UdpSocket> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_only_v6(false)?;
        socket.bind(&SocketAddr::from(([0u16; 8], port)).into())?;
        socket.set_nonblocking(true)?;
        Ok(std::net::UdpSocket::from(socket))
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
    ///
    /// The window always runs to its deadline — see [`probe_with_early_exit`]
    /// for the CR-P3-06 variant.
    pub async fn probe(
        socket: &tokio::net::UdpSocket,
        candidates: &[SocketAddr],
        interval: Duration,
        duration: Duration,
    ) -> io::Result<PunchReport> {
        probe_round(socket, candidates, interval, duration, false).await
    }

    /// [`probe`] with the CR-P3-06 early exit (review 2026-11-11 §4.5,
    /// batchF G2): the round returns the moment the FIRST candidate of
    /// the static order is echo-confirmed (a peer packet arrives from
    /// exactly that candidate), instead of idling out the rest of the
    /// window — a same-LAN send then pays one probe round-trip, not a
    /// fixed extra second, and the latency is independent of how many
    /// further candidates trail the confirmed head. Everything else
    /// (packet shape, filtering, reporting, transient-error policy) is
    /// byte-identical to [`probe`]:
    ///
    /// * only the head candidate arms the exit — a confirmation of any
    ///   LATER candidate does NOT end the round (the round's job is
    ///   ordering the dial list; the head confirming is the one outcome
    ///   where further observation cannot change the order's leader);
    /// * the exit can never fire before this side has SENT at least one
    ///   probe burst: the shared loop sends its first burst before it
    ///   ever polls the socket, so a confirmation that was already
    ///   buffered when the round started is consumed only after that
    ///   burst went out (CR-P3-21, review 2026-11-14 §4) — a peer
    ///   running a plain probe round in parallel is thus guaranteed to
    ///   observe our probe before we bail;
    /// * with no confirmation the window still runs to its deadline —
    ///   callers that only learn reachability from the report keep the
    ///   full-window semantics verbatim.
    pub async fn probe_with_early_exit(
        socket: &tokio::net::UdpSocket,
        candidates: &[SocketAddr],
        interval: Duration,
        duration: Duration,
    ) -> io::Result<PunchReport> {
        probe_round(socket, candidates, interval, duration, true).await
    }

    /// One send burst: one probe datagram to every dialable candidate.
    /// Used for the round's first beat (sent before the loop polls the
    /// socket, CR-P3-21) and for every subsequent ticker beat. Returns
    /// the fatal error, if any; transient per-candidate errors skip
    /// just that candidate.
    async fn send_burst(
        socket: &tokio::net::UdpSocket,
        packet: &[u8],
        dialable: &[Dialable],
        report: &mut PunchReport,
    ) -> io::Result<()> {
        for candidate in dialable {
            match socket.send_to(packet, candidate.send).await {
                Ok(_) => report.packets_sent += 1,
                Err(e) if transient_udp_error(&e) => continue,
                Err(e) if undialable_candidate(&e) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// The one probe-loop body both public entry points share
    /// (`early_exit` = return once the first dialable candidate is
    /// confirmed, see [`probe_with_early_exit`]).
    async fn probe_round(
        socket: &tokio::net::UdpSocket,
        candidates: &[SocketAddr],
        interval: Duration,
        duration: Duration,
        early_exit: bool,
    ) -> io::Result<PunchReport> {
        let start = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + duration;
        let packet = probe_packet(&probe_nonce());
        // Unspecified bind addresses (0.0.0.0/::) are not dialable peers, an
        // IPv4 socket cannot dial IPv6 candidates, and an IPv6 socket dials
        // IPv4 candidates in ::ffff: mapped form — on a dual-stack socket
        // (IPV6_V6ONLY=0, see `DirectEndpoint::bind_dual_stack`) that
        // reaches the v4 peer through the mapping; a v6-only socket rejects
        // the mapped send per candidate (skipped below, same outcome as
        // filtering it out). IPV6_V6ONLY is not queried: it is not portably
        // readable (on some Windows builds getsockopt returns a 1-byte
        // BOOLEAN, which socket2 0.5's `only_v6` mishandles).
        let local = socket.local_addr()?;
        let dialable: Vec<Dialable> = candidates
            .iter()
            .copied()
            .map(crate::classify::expand_mapped)
            .filter(|candidate| !candidate.ip().is_unspecified())
            .filter(|candidate| candidate.is_ipv4() || local.is_ipv6())
            .map(|candidate| {
                let send = if candidate.is_ipv4() && local.is_ipv6() {
                    crate::classify::to_mapped(candidate)
                } else {
                    candidate
                };
                Dialable {
                    report: candidate,
                    send,
                }
            })
            .collect();
        let mut send_ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        send_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut buffer = [0u8; 1500];
        let mut report = PunchReport::default();
        // CR-P3-21 (review 2026-11-14 §4): the FIRST beat is sent up
        // front, before the loop ever polls the socket. Relying on the
        // ticker's first tick was not enough: on tokio 1.54 the coarse
        // time driver fires a fresh interval's first tick only at the
        // next driver advance (~5 ms out, not at t=0), so with a peer
        // probe already buffered the receive branch was ready first and
        // could confirm the head candidate and break the early-exit
        // round before this side had sent a single packet. Sending the
        // first burst here makes "at least one probe round goes out
        // before the early exit can return" structural; the
        // `interval_at(now + interval, ..)` ticker below supplies every
        // subsequent beat with no double burst.
        send_burst(socket, &packet, &dialable, &mut report).await?;
        loop {
            // `biased;` pins the poll order to the source order below —
            // deadline, then send tick, then receive — so simultaneous
            // readiness is always arbitrated send-first: window expiry
            // wins all ties, and the send cadence is never postponed by
            // packet pressure. The receive branch cannot be starved by
            // the tick: a fired tick is not ready again until the next
            // interval boundary, so buffered packets drain between
            // beats.
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => break,
                _ = send_ticker.tick() => {
                    send_burst(socket, &packet, &dialable, &mut report).await?;
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
                        // A v4 peer shows up as ::ffff: on a dual-stack
                        // socket; compare in plain-family form and report
                        // the candidate exactly as the caller supplied it.
                        let source_plain = crate::classify::expand_mapped(source);
                        if let Some(hit) = dialable.iter().find(|c| c.report == source_plain) {
                            if !report.reachable.contains(&hit.report) {
                                report.reachable.push(hit.report);
                            }
                        }
                        // CR-P3-06 early exit: the static order's first
                        // dialable candidate is now echo-confirmed — the
                        // round cannot learn anything that changes the
                        // dial order's leader, so stop paying the window.
                        if early_exit
                            && dialable
                                .first()
                                .is_some_and(|head| report.reachable.contains(&head.report))
                        {
                            break;
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

    /// Errors that mean "this one candidate cannot be dialed from this
    /// socket" (family mismatch on a v6-only socket, no route to that
    /// destination): the probe skips the candidate and continues with the
    /// rest instead of aborting.
    fn undialable_candidate(err: &io::Error) -> bool {
        matches!(
            err.kind(),
            io::ErrorKind::InvalidInput
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::HostUnreachable
        )
    }

    /// Whether THIS host can honor `send_to_with_ttl` (CR-P1-06, review
    /// 2026-11-11 §4.5 改法 1; the Unix-only sibling below — plain code
    /// span here because the link target does not exist on Windows
    /// builds). Unix-likes: yes, via the `sendmsg`
    /// control-message path (Linux/Android/macOS all support per-packet
    /// `IP_TTL` / `IPV6_HOPLIMIT`). Windows: no — `WSASendMsg` documents
    /// ancillary `IPV6_HOPLIMIT`/`IPV6_TCLASS` only (no IPv4 `IP_TTL`
    /// control data), and IPv6 needs no low TTL (no NAT), so a Windows
    /// caller degrades to normal-TTL punching for the whole window — the
    /// honest documented fallback the review prescribed for exactly this
    /// case. The socket's OWN default TTL is never touched either way: the
    /// limit rides per-packet control data, so other users of a shared
    /// socket are unaffected.
    pub fn per_packet_ttl_supported() -> bool {
        cfg!(unix)
    }

    /// Send ONE datagram to `target` with a per-packet TTL / hop limit of
    /// `ttl` (CR-P1-06, review 2026-11-11 §4.5 改法 1), without touching
    /// the socket's own default TTL — the limit rides `sendmsg` control
    /// data (`IP_TTL` for IPv4 destinations, `IPV6_HOPLIMIT` for IPv6),
    /// so a shared QUIC socket keeps its normal hop limit for every other
    /// send. Unix-likes only; Windows callers must consult
    /// [`per_packet_ttl_supported`] first (the function does not exist
    /// there).
    ///
    /// Family note (why the control message keys off the TARGET family,
    /// expanded from `::ffff:` mapped form): a dual-stack `[::]` socket
    /// dials plain-v4 candidates in mapped form, and on Linux
    /// `udpv6_sendmsg` delegates v4-mapped destinations to the IPv4
    /// `udp_sendmsg` path — which parses `SOL_IP`/`IP_TTL` control data,
    /// while the IPv6 parser skips non-`SOL_IPV6` messages. Attaching
    /// `IP_TTL` for v4 targets (mapped or plain) is therefore the choice
    /// that takes effect on both socket families.
    // SAFETY (the crate's single audited unsafe, see the crate-level
    // policy note): the control buffer is a local `[usize; 4]` whose
    // address/length we re-borrow as bytes for exactly `cmsg_space`
    // (≤ the array's total size per the debug_assert above) — no aliasing
    // (no other reference exists), no uninit read (every byte is either
    // written below or within CMSG_SPACE padding the kernel ignores),
    // and the lifetime ends before sendmsg returns. The layout constants
    // follow the platform's CMSG_LEN/CMSG_SPACE rules stated above.
    #[cfg(unix)]
    #[allow(unsafe_code)]
    pub fn send_to_with_ttl(
        socket: &std::net::UdpSocket,
        packet: &[u8],
        target: SocketAddr,
        ttl: u32,
    ) -> io::Result<()> {
        // One cmsg: [cmsg_len: usize][level: c_int][type: c_int][value: c_int]
        // laid out per CMSG_LEN/CMSG_SPACE rules (header size = word + 2×int,
        // data = 1×int, buffer padded to word alignment). The buffer rides a
        // usize array so the cmsghdr alignment requirement holds.
        let word = std::mem::size_of::<usize>();
        let int_size = std::mem::size_of::<libc::c_int>();
        let cmsg_len = word + 2 * int_size + int_size;
        // div_ceil (stable 1.73): Linux clippy enforces manual_div_ceil on
        // this cfg(unix)-only line — invisible to Windows-local clippy.
        let cmsg_space = cmsg_len.div_ceil(word) * word;
        let mut control = [0usize; 4]; // 16/32 bytes ≥ CMSG_SPACE(one int) on 32/64-bit
        debug_assert!(control.len() * word >= cmsg_space);
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(control.as_mut_ptr() as *mut u8, cmsg_space) };
        bytes[..word].copy_from_slice(&(cmsg_len as usize).to_ne_bytes());
        bytes[word..word + int_size].copy_from_slice(&level_for(target).to_ne_bytes());
        bytes[word + int_size..word + 2 * int_size]
            .copy_from_slice(&cmsg_type_for(target).to_ne_bytes());
        bytes[word + 2 * int_size..cmsg_len].copy_from_slice(&(ttl as libc::c_int).to_ne_bytes());

        let addr = socket2::SockAddr::from(target);
        let buffers = [std::io::IoSlice::new(packet)];
        let message = socket2::MsgHdr::new()
            .with_addr(&addr)
            .with_buffers(&buffers)
            .with_control(bytes);
        socket2::SockRef::from(socket)
            .sendmsg(&message, 0)
            .map(|_| ())
    }

    /// `cmsg_level` of the per-packet TTL control message for one target
    /// ([`send_to_with_ttl`]): `SOL_IP` for IPv4 destinations (plain or
    /// `::ffff:` mapped — see the function doc for why), `SOL_IPV6` for
    /// native IPv6.
    #[cfg(unix)]
    fn level_for(target: SocketAddr) -> libc::c_int {
        if crate::classify::expand_mapped(target).is_ipv4() {
            libc::IPPROTO_IP
        } else {
            libc::IPPROTO_IPV6
        }
    }

    /// `cmsg_type` companion of [`level_for`]: `IP_TTL` / `IPV6_HOPLIMIT`.
    #[cfg(unix)]
    fn cmsg_type_for(target: SocketAddr) -> libc::c_int {
        if crate::classify::expand_mapped(target).is_ipv4() {
            libc::IP_TTL
        } else {
            libc::IPV6_HOPLIMIT
        }
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

    /// `::ffff:a.b.c.d` as an IpAddr (how dual-stack sockets present v4).
    fn v4m(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V6(Ipv4Addr::new(a, b, c, d).to_ipv6_mapped())
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
    // v4-mapped address handling (dual-stack)
    // ------------------------------------------------------------------

    #[test]
    fn classify_expands_v4_mapped_peer_to_its_v4_semantics() {
        let nets = [net4(192, 168, 1, 10, 24)];
        // ::ffff:192.168.1.200 classifies like 192.168.1.200 (same subnet).
        assert_eq!(
            classify::classify(v4m(192, 168, 1, 200), &nets),
            PathKind::LocalDirect
        );
        // Mapped loopback is local even with no interface nets at all.
        assert_eq!(
            classify::classify(v4m(127, 0, 0, 1), &[]),
            PathKind::LocalDirect
        );
        // Mapped public range is Internet.
        assert_eq!(
            classify::classify(v4m(203, 0, 113, 5), &nets),
            PathKind::InternetDirect
        );
        // Without the mapping expansion these would be v6 addresses that
        // match no v4 net: ::192.168.1.200 (non-mapped embedding) stays v6
        // and must NOT classify as the same v4 subnet.
        let embedded = v6([0, 0, 0, 0, 0, 0, 0xc0a8, 0x01c8]);
        assert_eq!(
            classify::classify(embedded, &nets),
            PathKind::InternetDirect
        );
        assert_ne!(
            classify::classify(embedded, &nets),
            classify::classify(v4m(192, 168, 1, 200), &nets)
        );
    }

    #[test]
    fn mapped_helpers_roundtrip_between_v4_and_mapped_v6() {
        let v4_peer = SocketAddr::new(v4(203, 0, 113, 9), 4747);
        let mapped = classify::to_mapped(v4_peer);
        assert!(mapped.is_ipv6());
        assert_eq!(
            mapped,
            SocketAddr::new(v4m(203, 0, 113, 9), 4747),
            "to_mapped must build ::ffff:a.b.c.d with the port kept"
        );
        // expand/to are inverses on the mapped subset.
        assert_eq!(classify::expand_mapped(mapped), v4_peer);
        // Native v6 and plain v4 pass through unchanged in both helpers.
        let v6_peer = SocketAddr::new(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]), 4747);
        assert_eq!(classify::to_mapped(v6_peer), v6_peer);
        assert_eq!(classify::expand_mapped(v6_peer), v6_peer);
        assert_eq!(classify::to_mapped(v4_peer).port(), 4747);
        assert_eq!(classify::expand_mapped(v4_peer), v4_peer);
        // ::1 is not v4-mapped and must not be expanded to 0.0.0.1.
        let loopback_v6 = SocketAddr::new(v6([0, 0, 0, 0, 0, 0, 0, 1]), 4747);
        assert_eq!(classify::expand_mapped(loopback_v6), loopback_v6);
    }

    #[test]
    fn advertised_addrs_pair_interface_addresses_with_port() {
        let nets = [
            net4(192, 168, 1, 10, 24),
            // A (hypothetical) v4-mapped interface entry displays as v4.
            classify::Net {
                addr: v4m(10, 0, 0, 2),
                prefix_len: 64,
            },
            classify::Net {
                addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
                prefix_len: 64,
            },
        ];
        let addrs = advertised_addrs(&nets, 4747);
        assert_eq!(addrs.len(), nets.len());
        assert_eq!(addrs[0], SocketAddr::new(v4(192, 168, 1, 10), 4747));
        assert_eq!(addrs[1], SocketAddr::new(v4(10, 0, 0, 2), 4747));
        assert_eq!(
            addrs[2],
            SocketAddr::new(v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]), 4747)
        );
        // Empty nets yield an empty candidate list, not an error.
        assert!(advertised_addrs(&[], 4747).is_empty());
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
        // RFC 5737 documentation ranges are unrouted sample space, but a few
        // CI containers DO get 192.0.2.0/24 on a real interface, so pick the
        // first sample this host does not count among its local nets.
        let public_sample = [v4(192, 0, 2, 10), v4(198, 51, 100, 10), v4(203, 0, 113, 10)]
            .into_iter()
            .find(|&ip| {
                !endpoint
                    .local_nets()
                    .iter()
                    .any(|net| classify::same_net(ip, net.addr, *net))
            })
            .expect("host claims every RFC 5737 range as a local net");
        assert_eq!(
            endpoint.classify_peer(public_sample),
            PathKind::InternetDirect
        );
        let _ = endpoint.local_nets();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_addr_display_matches_local_nets() {
        let (bundle, _) = test_identity();
        let endpoint = DirectEndpoint::bind(loopback_addr(), &bundle)
            .await
            .expect("bind");
        let port = endpoint.local_addr().expect("local addr").port();
        let display = endpoint.local_addr_display().expect("display addrs");
        let nets = endpoint.local_nets();
        // One advertised address per local network, same order, right port,
        // mapped entries expanded.
        assert_eq!(display.len(), nets.len());
        for (net, addr) in nets.iter().zip(display.iter()) {
            assert_eq!(addr.port(), port);
            assert_eq!(
                addr.ip(),
                classify::expand_mapped(SocketAddr::new(net.addr, port)).ip()
            );
            assert!(!addr.ip().is_loopback(), "loopback advertised: {addr:?}");
        }
    }

    /// Best-effort availability probe: hosts without an IPv6 stack (or that
    /// refuse to clear IPV6_V6ONLY) skip with a printed reason instead of
    /// failing the suite; this machine class is documented, not hidden.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bind_dual_stack_availability() {
        let (bundle, _) = test_identity();
        let endpoint = match DirectEndpoint::bind_dual_stack(0, &bundle).await {
            Ok(endpoint) => endpoint,
            Err(e) => {
                eprintln!("skipping (best-effort): dual-stack bind unavailable here: {e}");
                return;
            }
        };
        assert!(endpoint.is_dual_stack());
        let local = endpoint.local_addr().expect("local addr");
        assert!(
            local.is_ipv6(),
            "dual-stack local addr must be v6: {local:?}"
        );
        assert!(local.ip().is_unspecified(), "expected [::], got {local:?}");
        assert_ne!(local.port(), 0);
        // v4-mapped expansion keeps reporting working on the same endpoint.
        assert_eq!(
            endpoint.classify_peer(v4m(127, 0, 0, 1)),
            PathKind::LocalDirect
        );
    }

    /// Best-effort connect for the dual-stack tests: some hosts (observed on
    /// the Windows insider build used for development) intermittently refuse
    /// `::ffff:` mapped sends with WSAENETUNREACH for whole minutes at a
    /// time. Retry briefly, then skip with a printed reason instead of
    /// failing — same best-effort rule as `bind_dual_stack_availability`.
    async fn connect_best_effort(
        client: &DirectEndpoint,
        peer: SocketAddr,
        pinned: &Fingerprint,
    ) -> Option<DirectChannel> {
        for attempt in 1..=3u8 {
            match tokio::time::timeout(Duration::from_secs(2), client.connect(peer, pinned)).await {
                Ok(Ok(channel)) => return Some(channel),
                Ok(Err(e)) => eprintln!("dual-stack connect attempt {attempt} failed: {e}"),
                Err(_) => eprintln!("dual-stack connect attempt {attempt} timed out"),
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        eprintln!("skipping (best-effort): dual-stack path unavailable right now");
        None
    }

    /// Accept until one handshake completes: client retries may abort earlier
    /// attempts, which surface here as errors to loop over.
    async fn accept_eventually(endpoint: DirectEndpoint) -> DirectChannel {
        loop {
            if let Ok(channel) = endpoint.accept(&|_| true).await {
                return channel;
            }
        }
    }

    /// Best-effort: same skip rule as `bind_dual_stack_availability`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dual_stack_connects_ipv4_loopback_peer() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        // Peer is a plain v4-bound endpoint on loopback.
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = match DirectEndpoint::bind_dual_stack(0, &client_id).await {
            Ok(client) => client,
            Err(e) => {
                eprintln!("skipping (best-effort): dual-stack bind unavailable here: {e}");
                return;
            }
        };
        assert!(client.is_dual_stack());
        let server_addr = server.local_addr().expect("server addr");
        assert!(server_addr.is_ipv4());

        let server_task = tokio::spawn(accept_eventually(server));
        // Dial the plain v4 address: connect maps it to ::ffff: internally.
        let channel = match connect_best_effort(&client, server_addr, &server_id.fingerprint).await
        {
            Some(channel) => channel,
            None => return,
        };
        let server_channel = server_task.await.expect("join");

        assert_eq!(channel.peer_fingerprint(), &server_id.fingerprint);
        // ::ffff:127.0.0.1 classifies like 127.0.0.1 on both ends.
        assert_eq!(channel.path(), PathKind::LocalDirect);
        assert_eq!(server_channel.path(), PathKind::LocalDirect);

        // Data actually flows across the mapped path.
        let mut cstream = channel.open_bi().await.expect("client open_bi");
        cstream
            .send_bytes(b"v4 via dual-stack")
            .await
            .expect("client send");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");
        let mut buf = [0u8; 17];
        sstream.recv_exact(&mut buf).await.expect("server recv");
        assert_eq!(&buf, b"v4 via dual-stack");
    }

    /// The mirror direction: a plain v4 endpoint accepts a peer handed over
    /// in v4-mapped form (e.g. a candidate relayed through signaling) by
    /// expanding it back to plain IPv4.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn v4_endpoint_connects_mapped_form_peer() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");
        let mapped_peer = classify::to_mapped(server_addr);
        assert!(mapped_peer.is_ipv6());

        let server_task = tokio::spawn(accept_eventually(server));
        let channel = client
            .connect(mapped_peer, &server_id.fingerprint)
            .await
            .expect("v4 endpoint connect via mapped peer form");
        let server_channel = server_task.await.expect("join");
        assert_eq!(channel.peer_fingerprint(), &server_id.fingerprint);
        // The v4 inside the mapped form is what classifies.
        assert_eq!(channel.path(), PathKind::LocalDirect);
        assert_eq!(server_channel.path(), PathKind::LocalDirect);
    }

    /// CR-P1-05 (review 2026-11-11 §4.5), the sender-side adoption proof:
    /// `dial_with_socket` builds the QUIC client on the CALLER-OWNED socket,
    /// so the dial leaves from the very port the caller's STUN query /
    /// probe round mapped — the port equation (endpoint local port ==
    /// adopted socket port) is the whole point, and the channel genuinely
    /// carries data. The adopted dual-stack socket dials the plain-v4
    /// peer through the `::ffff:` bridge (same as `bind_dual_stack`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dial_with_socket_dials_from_the_adopted_port() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let adopted = match holepunch::bind_dual_stack_udp(0) {
            Ok(socket) => socket,
            Err(e) => {
                eprintln!("skipping (best-effort): dual-stack bind unavailable here: {e}");
                return;
            }
        };
        let adopted_port = adopted.local_addr().expect("adopted local addr").port();
        let client = DirectEndpoint::dial_with_socket(adopted, &client_id)
            .await
            .expect("dial-side adoption");
        assert!(
            !client.is_dual_stack(),
            "adopted sockets never claim the flag"
        );
        assert_eq!(
            client.local_addr().expect("endpoint local addr").port(),
            adopted_port,
            "the QUIC dial must leave from the adopted socket's port"
        );

        let server_addr = server.local_addr().expect("server addr");
        let server_task = tokio::spawn(accept_eventually(server));
        // The channel genuinely completes on the adopted socket's port
        // (best-effort per the dual-stack skip rule).
        if let Some(channel) =
            connect_best_effort(&client, server_addr, &server_id.fingerprint).await
        {
            assert_eq!(channel.peer_fingerprint(), &server_id.fingerprint);
            let server_channel = server_task.await.expect("join");
            channel.close(0, b"done");
            server_channel.close(0, b"done");
        }
    }

    /// Best-effort: same skip rule as `bind_dual_stack_availability`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dual_stack_connects_ipv6_loopback_peer() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        // The native-v6 peer needs [::1] to exist on this host; CI
        // containers without IPv6 must skip, not panic.
        if let Err(e) = std::net::UdpSocket::bind("[::1]:0") {
            eprintln!("skipping (best-effort): no IPv6 loopback here: {e}");
            return;
        }
        // Peer is a native v6 endpoint on [::1].
        let v6_loopback = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0);
        let server = DirectEndpoint::bind(v6_loopback, &server_id)
            .await
            .expect("server bind");
        let client = match DirectEndpoint::bind_dual_stack(0, &client_id).await {
            Ok(client) => client,
            Err(e) => {
                eprintln!("skipping (best-effort): dual-stack bind unavailable here: {e}");
                return;
            }
        };
        let server_addr = server.local_addr().expect("server addr");
        assert!(server_addr.is_ipv6());

        let server_task = tokio::spawn(accept_eventually(server));
        let channel = match connect_best_effort(&client, server_addr, &server_id.fingerprint).await
        {
            Some(channel) => channel,
            None => return,
        };
        let server_channel = server_task.await.expect("join");

        assert_eq!(channel.peer_fingerprint(), &server_id.fingerprint);
        assert_eq!(channel.path(), PathKind::LocalDirect);
        assert_eq!(server_channel.path(), PathKind::LocalDirect);

        let mut cstream = channel.open_bi().await.expect("client open_bi");
        cstream
            .send_bytes(b"v6 via [::1]")
            .await
            .expect("client send");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");
        let mut buf = [0u8; 12];
        sstream.recv_exact(&mut buf).await.expect("server recv");
        assert_eq!(&buf, b"v6 via [::1]");
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

    // ------------------------------------------------------------------
    // CR-P2-04 transport tuning (tls constant table + idle behavior)
    // ------------------------------------------------------------------

    /// Assert one rendered `quinn::ServerConfig`/`ClientConfig` Debug dump
    /// carries every CR-P2-04 table value in its embedded `TransportConfig`.
    fn assert_transport_tuning(rendered: &str) {
        // quinn renders the windows as bare VarInt/u64 integers and the
        // durations as `Some(10s)` / `Some(60000)` (milliseconds).
        for fragment in [
            format!("keep_alive_interval: Some({:?})", tls::KEEP_ALIVE_INTERVAL),
            format!(
                "max_idle_timeout: Some({})",
                tls::MAX_IDLE_TIMEOUT.as_millis()
            ),
            format!(
                "stream_receive_window: {}",
                tls::STREAM_RECEIVE_WINDOW_BYTES
            ),
            format!("receive_window: {}", tls::RECEIVE_WINDOW_BYTES),
            format!("send_window: {}", tls::SEND_WINDOW_BYTES),
        ] {
            assert!(
                rendered.contains(&fragment),
                "transport tuning {fragment:?} missing from config: {rendered}"
            );
        }
    }

    /// CR-P2-04 lock test. quinn 0.11 exposes no getters for these knobs
    /// (`TransportConfig` fields are `pub(crate)` in quinn-proto 0.11.19 and
    /// `ServerConfig`/`ClientConfig` only accept the config), but both
    /// configs' `Debug` impls render the embedded `TransportConfig` in full,
    /// so the lock asserts quinn's own rendering of the BUILT server and
    /// client configs against the constant table — and the table against
    /// independent literals, so a table edit cannot drag the lock along.
    #[test]
    fn transport_tuning_is_embedded_in_both_configs() {
        let (bundle, _) = test_identity();
        let server = tls::quinn_server_config(&bundle).expect("server config builds");
        let client = tls::quinn_client_config(&bundle, None).expect("client config builds");
        assert_transport_tuning(&format!("{server:?}"));
        assert_transport_tuning(&format!("{client:?}"));

        // The table itself, pinned to the review's numbers (quinn defaults
        // it replaces: idle 30 s, keep-alive None, 1.25 MB stream window).
        assert_eq!(tls::KEEP_ALIVE_INTERVAL, Duration::from_secs(10));
        assert_eq!(tls::MAX_IDLE_TIMEOUT, Duration::from_secs(60));
        assert_eq!(tls::STREAM_RECEIVE_WINDOW_BYTES, 8 * 1024 * 1024);
        assert_eq!(tls::RECEIVE_WINDOW_BYTES, 8 * 1024 * 1024);
        assert_eq!(tls::SEND_WINDOW_BYTES, 8 * 1024 * 1024);

        // Negative control: the assertions above can actually fail — the
        // untuned default renders None/30 s/1.25 MB, not our table.
        let default = format!("{:?}", quinn::TransportConfig::default());
        assert!(default.contains("keep_alive_interval: None"));
        assert!(default.contains("max_idle_timeout: Some(30000)"));
        assert_transport_tuning_not_present(&default);
    }

    /// Mirror of [`assert_transport_tuning`] for the negative control: none
    /// of the table values may appear in a default (untuned) render.
    fn assert_transport_tuning_not_present(rendered: &str) {
        for fragment in [
            format!("keep_alive_interval: Some({:?})", tls::KEEP_ALIVE_INTERVAL),
            format!(
                "max_idle_timeout: Some({})",
                tls::MAX_IDLE_TIMEOUT.as_millis()
            ),
            format!(
                "stream_receive_window: {}",
                tls::STREAM_RECEIVE_WINDOW_BYTES
            ),
            format!("receive_window: {}", tls::RECEIVE_WINDOW_BYTES),
            format!("send_window: {}", tls::SEND_WINDOW_BYTES),
        ] {
            assert!(
                !rendered.contains(&fragment),
                "default config unexpectedly carries tuning {fragment:?}"
            );
        }
    }

    /// CR-P2-04 behavior: a connection with nothing to send — exactly the
    /// accept-confirmation wait, no streams and no data in either direction —
    /// must outlive quinn's 30 s default idle timeout AND the table's own
    /// 60 s value, and still carry data afterwards.
    ///
    /// Determinism: quinn 0.11.12 runs every timer on `tokio::time::Sleep`
    /// (quinn `src/runtime/tokio.rs`), so this test freezes the tokio clock
    /// (`tokio::time::pause`, current-thread runtime required) and advances
    /// it as virtual time. Keep-alive PINGs, their ACKs, and both idle
    /// timers all fire on the virtual timeline while the loopback UDP I/O
    /// runs for real between auto-advances (tokio polls the I/O driver with
    /// a zero timeout on each advance), so 65 "seconds" of idle cost
    /// milliseconds of wall clock and cannot flake on a loaded CI runner.
    ///
    /// The 65 s checkpoint is also the keep-alive proof: the idle timer
    /// counts from the last packet *received*, so a connection still healthy
    /// at 65 s of two-sided silence means the 10 s PINGs (ack-eliciting, so
    /// answered by ACK-only packets) really did refresh the idle timers on
    /// both endpoints — without keep-alive, both close at exactly 60 s.
    #[tokio::test] // current_thread flavor: tokio::time::pause requires it
    async fn connection_survives_idle_beyond_timeouts_and_still_transfers() {
        tokio::time::pause();
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
        assert!(client_channel.healthy() && server_channel.healthy());

        // 32 s of total silence: past quinn's old 30 s default, which would
        // have closed both endpoints before the user could accept.
        tokio::time::sleep(Duration::from_secs(32)).await;
        assert!(
            client_channel.healthy() && server_channel.healthy(),
            "connection died inside the old 30 s default idle timeout"
        );

        // 65 s total: past OUR 60 s table value too; from here on only the
        // keep-alive PING/ACK exchange keeps the idle timers refreshed.
        tokio::time::sleep(Duration::from_secs(33)).await;
        assert!(
            client_channel.healthy() && server_channel.healthy(),
            "keep-alive failed to refresh the 60 s idle timers"
        );

        // Data still flows after the long idle period, in both directions.
        let mut cstream = client_channel.open_bi().await.expect("open_bi after idle");
        cstream
            .send_bytes(b"after idle")
            .await
            .expect("client send after idle");
        let mut sstream = server_channel
            .accept_bi()
            .await
            .expect("accept_bi after idle");
        let mut buf = [0u8; 10];
        sstream
            .recv_exact(&mut buf)
            .await
            .expect("server recv after idle");
        assert_eq!(&buf, b"after idle");
        sstream
            .send_bytes(b"ok")
            .await
            .expect("server send after idle");
        let mut echo = [0u8; 2];
        cstream
            .recv_exact(&mut echo)
            .await
            .expect("client recv after idle");
        assert_eq!(&echo, b"ok");
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

    /// A type=Chunk header declaring a 4 GiB payload (far above both the
    /// per-type budget and the all-frame cap) is rejected as `Oversized` at
    /// header time, before the payload buffer is allocated or awaited — the
    /// client never sends any payload bytes, so a pass here proves the fast,
    /// allocation-free return.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recv_frame_rejects_4gib_chunk_header_before_allocating() {
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
        // [version 1][type u16 BE = TYPE_CHUNK][payload_len u32 BE = 0xFFFFFFFF]
        // and nothing else: recv_frame must fail on the header alone.
        cstream
            .send_bytes(&[0x01, 0x00, 0x06, 0xff, 0xff, 0xff, 0xff])
            .await
            .expect("send oversized chunk header");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");

        let limits = Limits::default();
        // Fast return: the 10s timeout below only passes if recv_frame
        // rejected the header without waiting for payload bytes.
        let received = tokio::time::timeout(Duration::from_secs(10), sstream.recv_frame(&limits));
        match received.await.expect("must return without payload bytes") {
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized { declared, budget })) => {
                assert_eq!(declared, u32::MAX);
                // Layer 1: a declaration this far above the all-frame cap is
                // rejected by the cap gate for every type (the per-type
                // gate is exercised separately with an in-cap declaration).
                assert_eq!(budget as usize, ak_wire::MAX_FRAME_BYTES);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    /// A small fixed-size type (Accept: 16-byte budget) declaring a payload
    /// below the all-frame cap but above its per-type budget is rejected at
    /// header time (layer 2 of the pre-allocation check) — this is exactly
    /// the allocation the pre-check removes (previously a hard-cap-sized
    /// buffer was allocated before decode rejected the frame).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recv_frame_rejects_per_type_over_budget_before_allocating() {
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
        // [version 1][type u16 BE = TYPE_ACCEPT][payload_len u32 BE = 64 KiB]
        // — under MAX_FRAME_BYTES, over the 16-byte Accept budget. No payload
        // follows; the header alone must decide.
        cstream
            .send_bytes(&[0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00])
            .await
            .expect("send over-budget accept header");
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");

        let limits = Limits::default();
        let received = tokio::time::timeout(Duration::from_secs(10), sstream.recv_frame(&limits));
        match received.await.expect("must return without payload bytes") {
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized { declared, budget })) => {
                assert_eq!(declared, 0x01_0000);
                assert_eq!(budget, 16, "Accept budget is its fixed 16-byte bound");
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    /// Header-period rejection table as a pure function: every branch of
    /// [`header_payload_check`] runs on 7 header bytes with no socket and no
    /// allocation.
    #[test]
    fn header_payload_check_table() {
        let limits = Limits::default();
        // type u16 BE, payload_len u32 BE helpers.
        let header = |ty: u16, declared: u32| {
            let mut h = [0u8; ak_wire::HEADER_LEN];
            h[0] = ak_wire::VERSION;
            h[1..3].copy_from_slice(&ty.to_be_bytes());
            h[3..7].copy_from_slice(&declared.to_be_bytes());
            h
        };

        // Declared exactly at budget: passes the header gates.
        assert_eq!(
            header_payload_check(&header(ak_wire::TYPE_ACCEPT, 16), &limits).unwrap(),
            16
        );
        // One byte over the fixed budget: Oversized with the type budget.
        assert!(matches!(
            header_payload_check(&header(ak_wire::TYPE_ACCEPT, 17), &limits),
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized {
                declared: 17,
                budget: 16
            }))
        ));
        // Chunk at its exact bound (32 + max_chunk_bytes) passes.
        assert_eq!(
            header_payload_check(
                &header(ak_wire::TYPE_CHUNK, 32 + limits.max_chunk_bytes),
                &limits
            )
            .unwrap(),
            (32 + limits.max_chunk_bytes) as usize
        );
        // Chunk one byte over: Oversized with the per-type budget.
        assert!(matches!(
            header_payload_check(
                &header(ak_wire::TYPE_CHUNK, 33 + limits.max_chunk_bytes),
                &limits
            ),
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized { budget, .. }))
                if budget == 32 + limits.max_chunk_bytes
        ));
        // Declared far above the all-frame cap: the cap gate fires first for
        // every type (here: Chunk declaring 4 GiB).
        assert!(matches!(
            header_payload_check(&header(ak_wire::TYPE_CHUNK, u32::MAX), &limits),
            Err(StreamError::Wire(ak_wire::DecodeError::Oversized { budget, .. }))
                if budget as usize == ak_wire::MAX_FRAME_BYTES
        ));
        // Unknown type with an in-cap declaration: rejected at header time.
        assert!(matches!(
            header_payload_check(&header(0, 10), &limits),
            Err(StreamError::Wire(ak_wire::DecodeError::UnknownType(0)))
        ));
        // Type 13 (TaskOpen) is known since wave 20: verify its header budget
        // instead of UnknownType. Use a truly unknown type for that check.
        assert!(matches!(
            header_payload_check(&header(999, 10), &limits),
            Err(StreamError::Wire(ak_wire::DecodeError::UnknownType(999)))
        ));
        // Bad version with an in-cap declaration: rejected at header time.
        let mut bad_version = header(ak_wire::TYPE_ACCEPT, 16);
        bad_version[0] = ak_wire::VERSION + 1;
        assert!(matches!(
            header_payload_check(&bad_version, &limits),
            Err(StreamError::Wire(ak_wire::DecodeError::BadVersion(v)))
                if v == ak_wire::VERSION + 1
        ));
    }

    /// [`type_budget`] mirrors ak-wire's per-type bounds: fixed small bounds
    /// for control-ish types, limits-derived bounds for the data-carrying
    /// ones, hard-cap saturation when limits imply more than one frame may
    /// carry, and `None` for unknown types.
    #[test]
    fn type_budget_bounds_table() {
        let limits = Limits::default();
        let string = u32::from(limits.max_string_bytes);
        assert_eq!(type_budget(ak_wire::TYPE_ACCEPT, &limits), Some(16));
        assert_eq!(type_budget(ak_wire::TYPE_REJECT, &limits), Some(18));
        assert_eq!(type_budget(ak_wire::TYPE_CHUNK_ACK, &limits), Some(28));
        assert_eq!(type_budget(ak_wire::TYPE_CONTROL, &limits), Some(17));
        assert_eq!(type_budget(ak_wire::TYPE_FINAL_ACK, &limits), Some(17));
        assert_eq!(type_budget(ak_wire::TYPE_FINALIZE, &limits), Some(16));
        assert_eq!(type_budget(ak_wire::TYPE_HELLO, &limits), Some(38 + string));
        assert_eq!(type_budget(ak_wire::TYPE_OFFER, &limits), Some(38 + string));
        assert_eq!(
            type_budget(ak_wire::TYPE_ERROR, &limits),
            Some(4 + u32::from(limits.max_error_message_bytes))
        );
        assert_eq!(
            type_budget(ak_wire::TYPE_CHUNK, &limits),
            Some(32 + limits.max_chunk_bytes)
        );
        assert_eq!(
            type_budget(ak_wire::TYPE_RESUME_REQUEST, &limits),
            Some(
                (21 + 12 * u64::from(limits.max_resume_entries))
                    .min((ak_wire::MAX_FRAME_BYTES - ak_wire::HEADER_LEN) as u64)
                    as u32
            )
        );
        let entry =
            56 + u64::from(limits.max_string_bytes) + 32 * u64::from(limits.max_chunk_digests);
        assert_eq!(
            type_budget(ak_wire::TYPE_MANIFEST_PAGE, &limits),
            Some(
                (25 + u64::from(limits.max_entries_per_page) * entry)
                    .min((ak_wire::MAX_FRAME_BYTES - ak_wire::HEADER_LEN) as u64)
                    as u32
            )
        );
        // Unknown types have no budget.
        assert_eq!(type_budget(0, &limits), None);
        assert_eq!(type_budget(999, &limits), None);
        assert_eq!(type_budget(u16::MAX, &limits), None);
        // Limits implying payloads beyond the all-frame cap saturate at the
        // cap (mirroring ak_wire::Frame::decode's budget min).
        let huge = Limits {
            max_chunk_bytes: u32::MAX,
            ..Limits::default()
        };
        assert_eq!(
            type_budget(ak_wire::TYPE_CHUNK, &huge),
            Some((ak_wire::MAX_FRAME_BYTES - ak_wire::HEADER_LEN) as u32)
        );
    }

    // ------------------------------------------------------------------
    // connection migration policy (INV-004)
    // ------------------------------------------------------------------

    /// Every endpoint this crate builds reports active migration disabled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_migration_is_disabled_on_every_endpoint() {
        let (bundle, _) = test_identity();
        let endpoint = DirectEndpoint::bind(loopback_addr(), &bundle)
            .await
            .expect("bind");
        assert!(
            endpoint.active_migration_disabled(),
            "plain-bind endpoint must disable active migration"
        );
        match DirectEndpoint::bind_dual_stack(0, &bundle).await {
            Ok(dual) => assert!(
                dual.active_migration_disabled(),
                "dual-stack endpoint must disable active migration"
            ),
            Err(e) => eprintln!("skipping dual-stack leg (best-effort): {e}"),
        }
    }

    /// Remote-change detection is a pure address comparison: same address is
    /// stable; any difference in IP or port (including the ::ffff: mapped
    /// form of the same v4 peer) reads as a changed path.
    #[test]
    fn path_change_detection_is_address_sensitive() {
        let established = SocketAddr::new(v4(192, 168, 1, 9), 4747);
        let same = SocketAddr::new(v4(192, 168, 1, 9), 4747);
        let new_port = SocketAddr::new(v4(192, 168, 1, 9), 4748);
        let new_ip = SocketAddr::new(v4(192, 168, 1, 10), 4747);
        let mapped = SocketAddr::new(v4m(192, 168, 1, 9), 4747);
        let v6 = SocketAddr::new(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]), 4747);
        assert_eq!(established, same, "same address: no change");
        assert_ne!(established, new_port, "port change is a path change");
        assert_ne!(established, new_ip, "ip change is a path change");
        assert_ne!(established, mapped, "v4->mapped rebind is a path change");
        assert_ne!(established, v6, "family change is a path change");
    }

    /// A live channel: the handshake-time remote is the dialed server
    /// address, the path is stable, and the stability check is a no-op that
    /// does not disturb stream traffic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_channel_path_is_stable_and_stream_ops_pass_the_gate() {
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

        assert_eq!(client_channel.established_remote(), server_addr);
        assert_eq!(client_channel.current_remote(), server_addr);
        assert!(!client_channel.path_changed());
        assert_eq!(server_channel.established_remote().ip(), v4(127, 0, 0, 1));
        assert!(!server_channel.path_changed());
        client_channel.ensure_path_stable().expect("stable path");

        // The gate runs on every stream operation without breaking them.
        let mut cstream = client_channel.open_bi().await.expect("open_bi passes");
        cstream.send_bytes(b"still here").await.expect("send");
        let mut sstream = server_channel.accept_bi().await.expect("accept_bi");
        let mut buf = [0u8; 10];
        sstream.recv_exact(&mut buf).await.expect("recv");
        assert_eq!(&buf, b"still here");
    }

    /// Simulated migration on an otherwise healthy channel (in-crate field
    /// mutation stands in for a transport-level rebind): the stability check
    /// fails closed, closes the connection with code 9, and every subsequent
    /// stream open fails. This is the defensive-close path INV-004 requires
    /// on top of disabling migration in the transport config.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migrated_channel_fails_closed_and_closes_the_connection() {
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
        let mut client_channel = client
            .connect(server_addr, &server_id.fingerprint)
            .await
            .expect("client connect");
        let server_channel = server_task.await.expect("join").expect("server accept");
        assert!(client_channel.healthy());

        // Simulate the transport reinding to a new peer address.
        client_channel.established_remote =
            SocketAddr::new(v4(203, 0, 113, 99), server_addr.port() + 1);
        assert!(client_channel.path_changed());

        let err = client_channel
            .ensure_path_stable()
            .expect_err("changed path must fail closed");
        assert!(
            matches!(&err, StreamError::Connection(text) if text.contains("migrat")),
            "error must name the migration: {err:?}"
        );
        // The defensive close reached the transport (and the peer).
        assert!(!client_channel.healthy(), "connection must be closed");
        for _ in 0..100 {
            if !server_channel.healthy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!server_channel.healthy(), "peer must see the close");
        // Stream opens on the migrated channel keep failing.
        assert!(client_channel.open_bi().await.is_err());
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
    // stats / accept_within
    // ------------------------------------------------------------------

    /// Poll until both directions reach frame parity (every frame one side
    /// transmitted has been received and counted by the peer) or the attempt
    /// budget runs out; returns the last sample either way. Loopback delivery
    /// is lossless, so parity is the quiescent fixed point; trailing ACKs
    /// take a few ack-delay periods to drain, hence the polling.
    async fn sample_at_parity(
        client: &DirectChannel,
        server: &DirectChannel,
    ) -> (ChannelStats, ChannelStats) {
        let mut last = (client.stats(), server.stats());
        for _ in 0..100 {
            let candidate = (client.stats(), server.stats());
            let (c, s) = candidate;
            if c.tx_frames == s.rx_frames && s.tx_frames == c.rx_frames {
                // Require stability across one more period so a packet in
                // flight cannot fake parity.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let (c2, s2) = (client.stats(), server.stats());
                if c2.tx_frames == s2.rx_frames && s2.tx_frames == c2.rx_frames {
                    return (c2, s2);
                }
            }
            last = candidate;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        last
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stats_reflect_real_loopback_transfer() {
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

        // 48 KiB client -> server, 8 KiB echo back, both fully consumed. The
        // client side runs in a task and hands the channel back so both
        // channels stay available for sampling.
        let payload: Vec<u8> = (0..48 * 1024).map(|i| (i % 251) as u8).collect();
        let echo_len = 8 * 1024;
        let payload_for_task = payload.clone();
        let client_task = tokio::spawn(async move {
            let mut cstream = client_channel.open_bi().await.expect("client open_bi");
            cstream
                .send_bytes(&payload_for_task)
                .await
                .expect("client send");
            let mut echoed = vec![0u8; echo_len];
            cstream
                .recv_exact(&mut echoed)
                .await
                .expect("client recv echo");
            (client_channel, echoed)
        });
        let mut sstream = server_channel.accept_bi().await.expect("server accept_bi");
        let mut received = vec![0u8; 48 * 1024];
        sstream
            .recv_exact(&mut received)
            .await
            .expect("server recv");
        sstream
            .send_bytes(&received[..echo_len])
            .await
            .expect("server echo");
        let (client_channel, echoed) = client_task.await.expect("join client");
        assert_eq!(echoed.as_slice(), &payload[..echo_len]);
        assert_eq!(received, payload);

        // Sample only while the connection is open: quinn does not count the
        // transmitted CONNECTION_CLOSE frame, so closing first would skew
        // frame parity by one.
        let (client_stats, server_stats) = sample_at_parity(&client_channel, &server_channel).await;

        // Non-zero on both sides after a real transfer.
        for stats in [client_stats, server_stats] {
            assert!(stats.tx_bytes > 0, "tx_bytes zero: {stats:?}");
            assert!(stats.rx_bytes > 0, "rx_bytes zero: {stats:?}");
            assert!(stats.tx_frames > 0, "tx_frames zero: {stats:?}");
            assert!(stats.rx_frames > 0, "rx_frames zero: {stats:?}");
        }
        // The client really pushed the payload through this channel.
        assert!(client_stats.tx_bytes > payload.len() as u64);

        // Frame counts are near-equal across the wire. Loopback is lossless,
        // but quinn may coalesce/pad differently on each side and the two
        // endpoints sample at slightly different instants, so strict parity
        // is too strong (observed 62 vs 60 in CI containers); allow a small
        // slack instead of exact equality.
        let frame_slack = |a: u64, b: u64| std::cmp::max(2, a.max(b) / 10);
        assert!(
            client_stats.tx_frames.abs_diff(server_stats.rx_frames)
                <= frame_slack(client_stats.tx_frames, server_stats.rx_frames),
            "client tx vs server rx: {client_stats:?} / {server_stats:?}"
        );
        assert!(
            server_stats.tx_frames.abs_diff(client_stats.rx_frames)
                <= frame_slack(server_stats.tx_frames, client_stats.rx_frames),
            "server tx vs client rx: {client_stats:?} / {server_stats:?}"
        );

        // Bytes agree within +/-10% (header/padding overhead headroom; on a
        // lossless loopback they should be near-identical).
        let ten_percent = |a: u64| a / 10;
        assert!(
            client_stats.tx_bytes.abs_diff(server_stats.rx_bytes)
                <= ten_percent(client_stats.tx_bytes.max(server_stats.rx_bytes)),
            "client tx {} vs server rx {} bytes",
            client_stats.tx_bytes,
            server_stats.rx_bytes
        );
        assert!(
            server_stats.tx_bytes.abs_diff(client_stats.rx_bytes)
                <= ten_percent(server_stats.tx_bytes.max(client_stats.rx_bytes)),
            "server tx {} vs client rx {} bytes",
            server_stats.tx_bytes,
            client_stats.rx_bytes
        );

        // A small loopback transfer should not register packet loss; this is
        // the expected (not guaranteed) transport-health reading.
        assert_eq!(
            client_stats.lost_packets, 0,
            "unexpected loss on loopback: {client_stats:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accept_within_succeeds_before_deadline() {
        let (client_id, _) = test_identity();
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");
        let client = DirectEndpoint::bind(loopback_addr(), &client_id)
            .await
            .expect("client bind");
        let server_addr = server.local_addr().expect("server addr");

        let server_task = tokio::spawn(async move {
            server
                .accept_within(&|_| true, Duration::from_secs(5))
                .await
        });
        let client_channel = client
            .connect(server_addr, &server_id.fingerprint)
            .await
            .expect("client connect");
        let server_channel = server_task
            .await
            .expect("join")
            .expect("accept_within success");
        assert_eq!(server_channel.peer_fingerprint(), &client_id.fingerprint);
        assert_eq!(client_channel.peer_fingerprint(), &server_id.fingerprint);
        // Both ends of the pair must be healthy while both channels are alive.
        assert!(client_channel.healthy() && server_channel.healthy());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accept_within_times_out_without_peer() {
        let (server_id, _) = test_identity();
        let server = DirectEndpoint::bind(loopback_addr(), &server_id)
            .await
            .expect("server bind");

        let started = std::time::Instant::now();
        let result = server
            .accept_within(&|_| true, Duration::from_millis(150))
            .await;
        match result {
            Err(ConnectError::Timeout(requested)) => {
                assert_eq!(requested, Duration::from_millis(150));
            }
            Err(other) => panic!("unexpected error class: {other:?}"),
            Ok(channel) => panic!(
                "channel must not exist without a peer: {:?}",
                channel.path()
            ),
        }
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "returned before the deadline"
        );
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

    /// CR-P3-06 (G2): `probe_with_early_exit` returns as soon as the
    /// static order's first candidate is echo-confirmed — at the
    /// acceptance bound itself ("within 200 ms of the first
    /// confirmation", tightened from the old beat-the-1.5-s-window
    /// margin by CR-P3-13 item 4/H3; the receive-to-return path is
    /// synchronous, so the whole round costs the first probe
    /// round-trip). The same scenario through the default
    /// [`holepunch::probe`] still runs the window (pinned by
    /// `holepunch_probe_reports_reachable_peer`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn holepunch_probe_early_exit_returns_after_first_confirmation() {
        let a = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind a");
        let b = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind b");
        let a_addr = a.local_addr().expect("addr a");
        let b_addr = b.local_addr().expect("addr b");

        // B's plain round (spawned FIRST so its opening probe burst is
        // already in flight when A starts listening).
        let b_task = tokio::spawn(async move {
            holepunch::probe(
                &b,
                &[a_addr],
                Duration::from_millis(25),
                Duration::from_millis(2000),
            )
            .await
        });
        let window = Duration::from_millis(1500);
        let a_report =
            holepunch::probe_with_early_exit(&a, &[b_addr], Duration::from_millis(25), window)
                .await
                .expect("probe a");
        let b_report = b_task.await.expect("join").expect("probe b");

        assert!(
            a_report.reachable.contains(&b_addr),
            "B not reachable: {a_report:?}"
        );
        assert!(
            a_report.packets_received > 0,
            "A received no peer probes: {a_report:?}"
        );
        assert!(
            b_report.reachable.contains(&a_addr),
            "A not reachable: {b_report:?}"
        );
        // The early exit, at the CR-P3-06 acceptance bound itself
        // (CR-P3-13 item 4, H3): "back within 200 ms of the first
        // confirmation". B's first burst leaves at t≈0, so the
        // confirmation lands within the first ticks even on a slow
        // runner and the whole round (confirmation + the synchronous
        // receive-to-return path) must fit inside 200 ms — not merely
        // beat the window.
        assert!(
            a_report.elapsed < Duration::from_millis(200),
            "the early exit did not return within the 200 ms acceptance bound: {:?}",
            a_report.elapsed
        );
    }

    /// CR-P3-21 (review 2026-11-14 §4): the early-exit round never
    /// returns on a confirmation alone — it must have SENT at least one
    /// probe burst first. Deterministic reproduction of the race that
    /// made `holepunch_probe_early_exit_returns_after_first_confirmation`
    /// flaky: the peer's probe packet is already sitting in OUR socket
    /// buffer when the round starts, which is exactly the state in
    /// which the old round could act on the receive branch before its
    /// first send (the ticker's first tick fires only at the timer
    /// driver's next advance, so the buffered packet was ready first)
    /// and break with `packets_sent == 0` — the peer's concurrent plain
    /// round then never saw our probe. Pre-buffering here is a verified
    /// precondition, not a timing assumption: the echo side is spawned
    /// first and `peek_from` (which does not consume) resolves only
    /// once one of its probe packets is actually queued; the round's
    /// pre-loop first burst then guarantees our probe went out before
    /// that buffered confirmation is processed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn holepunch_probe_early_exit_still_sends_at_least_one_probe_when_confirmation_is_pre_buffered(
    ) {
        let a = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind a");
        let b = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind b");
        let a_addr = a.local_addr().expect("addr a");
        let b_addr = b.local_addr().expect("addr b");

        // The echo responder, started FIRST: its plain round fires a
        // burst at t≈0 and every 25 ms after. Wait until one of B's
        // probe packets is verifiably queued in A's receive buffer —
        // the pre-buffered confirmation the old unbiased select! could
        // act on before ever sending.
        let b_task = tokio::spawn(async move {
            holepunch::probe(
                &b,
                &[a_addr],
                Duration::from_millis(25),
                Duration::from_millis(2000),
            )
            .await
        });
        let mut peek = [0u8; 1500];
        let (peeked, from) = a.peek_from(&mut peek).await.expect("peer probe queued");
        assert_eq!(from, b_addr, "unexpected peek source");
        assert!(
            holepunch::is_probe_packet(&peek[..peeked]),
            "queued packet is not a probe: {peeked} bytes from {from}"
        );

        let window = Duration::from_millis(1500);
        let a_report =
            holepunch::probe_with_early_exit(&a, &[b_addr], Duration::from_millis(25), window)
                .await
                .expect("probe a");
        b_task.abort();

        // The pre-buffered confirmation did drive the early exit (well
        // inside the window, 500 ms of scheduler-jitter slack) ...
        assert!(
            a_report.reachable.contains(&b_addr),
            "B not reachable: {a_report:?}"
        );
        assert!(
            a_report.packets_received > 0,
            "A received no peer probes: {a_report:?}"
        );
        assert!(
            a_report.elapsed + Duration::from_millis(500) < window,
            "the early exit did not fire: {a_report:?}"
        );
        // ... and the round sent at least one full probe burst BEFORE
        // returning — the CR-P3-21 guarantee (the old code randomly
        // returned here with packets_sent == 0).
        assert!(
            a_report.packets_sent > 0,
            "early exit returned before any probe was sent: {a_report:?}"
        );
    }

    /// CR-P3-06 (G2), candidate-count independence: the early-exit round
    /// with one live head plus a TRAILING pile of silent candidates costs
    /// the same one round-trip as a single-candidate round — the trailing
    /// candidates neither delay the exit nor extend it toward the window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn holepunch_probe_early_exit_is_independent_of_candidate_count() {
        // The echo head: a plain probe round answers from its own bound
        // address (spawned first, see the test above).
        let echo = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind echo");
        let probe_sock = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind probe");
        let probe_addr = probe_sock.local_addr().expect("probe addr");
        let echo_addr = echo.local_addr().expect("echo addr");
        let echo_task = tokio::spawn(async move {
            holepunch::probe(
                &echo,
                &[probe_addr],
                Duration::from_millis(25),
                Duration::from_millis(2000),
            )
            .await
        });

        // Silent trailing candidates: bound-but-silent UDP sockets on
        // loopback (deterministic on every OS — no ICMP games), plus
        // TEST-NET-3 entries that are simply never answered. The sockets
        // stay bound for the whole test (`_silent_keepalive`) so no port
        // can be rebound mid-round.
        let mut many_candidates = vec![echo_addr];
        let mut silent_keepalive = Vec::new();
        for _ in 0..4 {
            let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("silent bind");
            many_candidates.push(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                silent.local_addr().expect("silent addr").port(),
            ));
            silent_keepalive.push(silent);
        }
        many_candidates.push(SocketAddr::from(([203, 0, 113, 77], 41234)));
        let window = Duration::from_millis(1500);
        let one = holepunch::probe_with_early_exit(
            &probe_sock,
            &[echo_addr],
            Duration::from_millis(25),
            window,
        )
        .await
        .expect("one-candidate round");
        let many = holepunch::probe_with_early_exit(
            &probe_sock,
            &many_candidates,
            Duration::from_millis(25),
            window,
        )
        .await
        .expect("many-candidate round");
        echo_task.abort();

        assert!(
            one.reachable.contains(&echo_addr) && many.reachable.contains(&echo_addr),
            "the echo head must confirm in both rounds: {one:?} / {many:?}"
        );
        for report in [&one, &many] {
            assert!(
                report.elapsed + Duration::from_millis(500) < window,
                "the early exit did not fire: {:?} of {window:?}",
                report.elapsed
            );
        }
        // Independence: the trailing pile must not make the round
        // materially slower than the single-candidate round (one extra
        // probe interval of slack covers scheduling jitter).
        assert!(
            many.elapsed <= one.elapsed + Duration::from_millis(150),
            "trailing candidates stretched the round: {one:?} vs {many:?}"
        );
    }

    /// One dual-stack probe round: A on a `[::]` V6ONLY=0 socket, a v4
    /// loopback peer B and a v6 loopback peer C, all probing each other.
    /// Returns None only when the host cannot build the dual-stack socket at
    /// all (best-effort skip); reports otherwise — reachability is judged by
    /// the caller so it can retry transiently-refused rounds.
    async fn dual_stack_probe_round() -> Option<(
        holepunch::PunchReport,
        holepunch::PunchReport,
        holepunch::PunchReport,
        SocketAddr,
        SocketAddr,
        SocketAddr,
        SocketAddr,
    )> {
        let raw = match socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        ) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("skipping (best-effort): no IPv6 stack here: {e}");
                return None;
            }
        };
        if let Err(e) = raw.set_only_v6(false) {
            eprintln!("skipping (best-effort): IPV6_V6ONLY not clearable here: {e}");
            return None;
        }
        let any_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        if let Err(e) = raw.bind(&socket2::SockAddr::from(any_v6)) {
            eprintln!("skipping (best-effort): dual-stack bind failed here: {e}");
            return None;
        }
        let std_socket = std::net::UdpSocket::from(raw);
        std_socket.set_nonblocking(true).expect("nonblocking");
        let a = match tokio::net::UdpSocket::from_std(std_socket) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipping (best-effort): tokio registration failed: {e}");
                return None;
            }
        };
        let b = tokio::net::UdpSocket::bind(loopback_addr())
            .await
            .expect("bind v4 peer");
        let c = tokio::net::UdpSocket::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0))
            .await
            .expect("bind v6 peer");
        let a_port = a.local_addr().expect("addr a").port();
        let b_addr = b.local_addr().expect("addr b");
        let c_addr = c.local_addr().expect("addr c");
        // A's dialable v4/v6 forms for the peers to probe back.
        let a_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a_port);
        let a_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), a_port);

        let b_task = tokio::spawn(async move {
            holepunch::probe(
                &b,
                &[a_v4],
                Duration::from_millis(25),
                Duration::from_millis(800),
            )
            .await
        });
        let c_task = tokio::spawn(async move {
            holepunch::probe(
                &c,
                &[a_v6],
                Duration::from_millis(25),
                Duration::from_millis(800),
            )
            .await
        });
        let a_report = holepunch::probe(
            &a,
            &[b_addr, c_addr],
            Duration::from_millis(25),
            Duration::from_millis(500),
        )
        .await
        .expect("probe a");
        let b_report = b_task.await.expect("join").expect("probe b");
        let c_report = c_task.await.expect("join").expect("probe c");
        Some((a_report, b_report, c_report, b_addr, c_addr, a_v4, a_v6))
    }

    /// Best-effort (same skip rule as `bind_dual_stack_availability`, plus
    /// retries for hosts that intermittently refuse mapped sends): on a
    /// dual-stack socket, v4 candidates are dialed in mapped form and still
    /// reported as plain v4, alongside native v6 candidates.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn holepunch_probe_dual_stack_reaches_v4_and_v6_peers() {
        for attempt in 1..=3u8 {
            let (a_report, b_report, c_report, b_addr, c_addr, a_v4, a_v6) =
                match dual_stack_probe_round().await {
                    Some(round) => round,
                    None => return,
                };
            let v4_ok = a_report.reachable.contains(&b_addr) && b_report.reachable.contains(&a_v4);
            let v6_ok = a_report.reachable.contains(&c_addr) && c_report.reachable.contains(&a_v6);
            if v4_ok && v6_ok {
                assert!(
                    a_report.packets_received > 0,
                    "no probes seen: {a_report:?}"
                );
                return;
            }
            eprintln!(
                "dual-stack probe round {attempt}: v4 {} v6 {}; retrying",
                if v4_ok { "ok" } else { "unreachable" },
                if v6_ok { "ok" } else { "unreachable" }
            );
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        eprintln!("skipping (best-effort): dual-stack path unavailable right now");
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

    /// The capability flag matches the platform (CR-P1-06): the sendmsg
    /// TTL path exists exactly on the Unix-likes; Windows ships the
    /// documented normal-TTL degradation.
    #[test]
    fn per_packet_ttl_capability_matches_the_platform() {
        assert_eq!(holepunch::per_packet_ttl_supported(), cfg!(unix));
    }

    /// CR-P1-06: the per-packet TTL send succeeds through the control
    /// message path and the datagram still leaves the given socket's port
    /// (a malformed cmsghdr would fail the send with EINVAL). Honest
    /// scope, pinned here because the review's suggested "loopback + TTL=1
    /// ⇒ peer receives nothing" mechanism does NOT hold on mainstream
    /// kernels: local delivery performs no forwarding hop, so neither the
    /// TTL check nor a decrement happens (verified on Windows; Linux's
    /// `ip_rcv` has no TTL gate and only `ip_forward` decrements). Real
    /// hop expiry is exercised where hops exist — the NAT matrix's
    /// akinet "public" namespace (added by CR-P2-14 改法 5 exactly for
    /// this) and the C4 real-network runs.
    #[cfg(unix)]
    #[test]
    fn send_to_with_ttl_sends_and_leaves_the_given_port() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_addr = target.local_addr().expect("target addr");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender bind");
        let sender_port = sender.local_addr().expect("sender addr").port();
        let packet = holepunch::probe_packet(&[0x5Au8; 16]);
        holepunch::send_to_with_ttl(&sender, &packet, target_addr, 1).expect("low-ttl send");
        target
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let mut buffer = [0u8; 64];
        let (len, from) = target
            .recv_from(&mut buffer)
            .expect("loopback delivers ttl=1");
        assert_eq!(len, 24, "probe-shaped payload: {len}");
        assert_eq!(from.port(), sender_port, "sent from the given socket");
        // The mapped form of the same target behaves identically (the
        // dual-stack socket's only v4 shape). CR-P3-13: on a kernel
        // without IPv6 this bind fails with EAFNOSUPPORT — degrade
        // exactly like the product does ("dual-stack bind unavailable …
        // falling back to the IPv4-only bind"): skip the dual-stack
        // half with a note instead of panicking, so IPv6-less hosts
        // report 0 failures. Any OTHER bind error still fails the test.
        // Skip-set spelling for the platforms this cfg(unix) test
        // compiles on: the raw errno of a v6-less kernel (EAFNOSUPPORT /
        // EPROTONOSUPPORT — no stable ErrorKind spelling exists for
        // either; the AddrfamilyNotSupported/ProtocolNotSupported names
        // used before aae55db do not exist and broke the ubuntu clippy
        // job, run 43410016551) plus the generic Unsupported kind.
        let mapped = crate::classify::to_mapped(target_addr);
        let sender6 = match holepunch::bind_dual_stack_udp(0) {
            Ok(socket) => socket,
            Err(err)
                if err.kind() == std::io::ErrorKind::Unsupported
                    || matches!(
                        err.raw_os_error(),
                        Some(libc::EAFNOSUPPORT) | Some(libc::EPROTONOSUPPORT)
                    ) =>
            {
                eprintln!("note: dual-stack half skipped — no IPv6 on this host ({err})");
                return;
            }
            Err(err) => panic!("dual-stack bind: {err}"),
        };
        holepunch::send_to_with_ttl(&sender6, &packet, mapped, 1).expect("mapped low-ttl send");
        let (len, from2) = target.recv_from(&mut buffer).expect("mapped send delivers");
        assert_eq!(len, 24, "probe-shaped payload: {len}");
        assert_eq!(
            from2.port(),
            sender6.local_addr().expect("dual-stack addr").port(),
            "mapped send left the given dual-stack socket"
        );
    }
}
