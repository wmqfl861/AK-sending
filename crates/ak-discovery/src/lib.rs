//! Peer candidate discovery.
//!
//! Three sources per REVIEW 4.3: manual entry (fully offline), candidates
//! relayed by the coordination plane (same-egress hint), and mDNS/DNS-SD for
//! same-LAN discovery. Discovery never equals authorization: every candidate
//! still goes through the authenticated direct channel before any transfer.
//!
//! Apple platforms will use native Bonjour via the Flutter plugin; this crate
//! serves the Rust CLI prototype and non-Apple builds.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use ak_identity::Fingerprint;

/// mDNS service type advertised by AK-sending endpoints.
pub const SERVICE_TYPE: &str = "_ak-sending._udp.local.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    Manual,
    Mdns,
    Signaling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCandidate {
    pub addr: SocketAddr,
    pub fingerprint: Option<Fingerprint>,
    pub source: CandidateSource,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("invalid address list: {0}")]
    Parse(String),
    #[error("mDNS error: {0}")]
    Mdns(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Candidate ranking per ADR-001 and REVIEW 4.1: same-LAN first (LocalDirect
/// possible), then IPv6 public, then IPv4 public; stable within a tier; a
/// candidate that has both sources keeps its higher-priority source. Dedupes
/// by address.
pub fn rank(candidates: Vec<PeerCandidate>, nets: &[ak_net_shim::Net]) -> Vec<PeerCandidate> {
    todo!("owned by ak-discovery agent")
}

/// `ip:port` pairs, optionally `ip:port=<fingerprint hex>`, comma or space
/// separated. For manual pairing when multicast is unavailable.
pub fn parse_manual_list(text: &str) -> Result<Vec<PeerCandidate>, DiscoveryError> {
    todo!("owned by ak-discovery agent")
}

/// mDNS advertise + browse over DNS-SD (mdns-sd crate, no relay).
pub struct MdnsDiscovery {
    daemon: mdns_sd::ServiceDaemon,
    announced: Option<mdns_sd::FullService>,
}

impl MdnsDiscovery {
    pub fn new() -> Result<Self, DiscoveryError> {
        todo!("owned by ak-discovery agent")
    }

    /// Advertise `port` with the device fingerprint (hex) and name in TXT.
    pub fn advertise(
        &mut self,
        port: u16,
        fingerprint: &Fingerprint,
        device_name: &str,
    ) -> Result<(), DiscoveryError> {
        todo!("owned by ak-discovery agent")
    }

    pub fn stop_advertise(&mut self) {
        todo!("owned by ak-discovery agent")
    }

    /// Browse for `duration`, returning discovered candidates (IPv4 and IPv6).
    pub fn browse(&self, duration: Duration) -> Result<Vec<PeerCandidate>, DiscoveryError> {
        todo!("owned by ak-discovery agent")
    }
}

/// Local import path shim so ranking shares one `Net` type with ak-net
/// without making ak-discovery depend on the whole network stack.
pub mod ak_net_shim {
    pub use ak_core::policy::PathKind;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Net {
        pub addr: std::net::IpAddr,
        pub prefix_len: u8,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-discovery agent"]
    fn placeholder() {}
}
