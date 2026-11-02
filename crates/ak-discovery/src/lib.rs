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

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
///
/// Tiering: (1) LAN group = the address falls inside any caller-provided
/// local [`ak_net_shim::Net`], or is an IPv6 link-local address; everything else is the
/// public group. (2) Within a group IPv6 sorts before IPv4. (3) Source
/// priority Manual > Signaling > Mdns only decides which duplicate survives
/// per address. The sort is stable, so equal-tier candidates keep input
/// order. This is self-contained on purpose: no interface enumeration here
/// (`if-addrs` belongs to the caller), `nets` may be empty.
pub fn rank(candidates: Vec<PeerCandidate>, nets: &[ak_net_shim::Net]) -> Vec<PeerCandidate> {
    // Layer 3 first: dedupe by address, keeping the highest-priority source
    // in the slot where the address first appeared.
    let mut slots: Vec<PeerCandidate> = Vec::with_capacity(candidates.len());
    let mut slot_of: HashMap<SocketAddr, usize> = HashMap::with_capacity(candidates.len());
    for candidate in candidates {
        match slot_of.get(&candidate.addr) {
            None => {
                slot_of.insert(candidate.addr, slots.len());
                slots.push(candidate);
            }
            Some(&slot) => {
                if source_rank(candidate.source) < source_rank(slots[slot].source) {
                    slots[slot] = candidate;
                }
            }
        }
    }

    // Layers 1 and 2; stable sort preserves first-seen order within a tier.
    slots.sort_by_key(|c| (group_rank(c.addr.ip(), nets), family_rank(c.addr.ip())));
    slots
}

/// `ip:port` pairs, optionally `ip:port=<fingerprint hex>`, comma or space
/// separated. For manual pairing when multicast is unavailable.
///
/// IPv6 addresses must be bracketed (`[2001:db8::1]:9000`), exactly as
/// `SocketAddr` display form. Any invalid item fails the whole list and the
/// error message quotes the offending fragment.
pub fn parse_manual_list(text: &str) -> Result<Vec<PeerCandidate>, DiscoveryError> {
    let mut out = Vec::new();
    for item in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|item| !item.is_empty())
    {
        out.push(parse_manual_entry(item)?);
    }
    Ok(out)
}

fn parse_manual_entry(item: &str) -> Result<PeerCandidate, DiscoveryError> {
    let (addr_part, fp_part) = match item.split_once('=') {
        Some((addr_part, fp_part)) => (addr_part, Some(fp_part)),
        None => (item, None),
    };
    let addr: SocketAddr = addr_part
        .parse()
        .map_err(|_| DiscoveryError::Parse(format!("invalid address entry '{item}'")))?;
    let fingerprint = match fp_part {
        None => None,
        Some(hex) => Some(
            decode_fingerprint(hex)
                .ok_or_else(|| DiscoveryError::Parse(format!("invalid fingerprint in '{item}'")))?,
        ),
    };
    Ok(PeerCandidate {
        addr,
        fingerprint,
        source: CandidateSource::Manual,
    })
}

/// mDNS advertise + browse over DNS-SD (mdns-sd crate, no relay).
pub struct MdnsDiscovery {
    daemon: mdns_sd::ServiceDaemon,
    announced: Option<mdns_sd::ServiceInfo>,
}

impl MdnsDiscovery {
    pub fn new() -> Result<Self, DiscoveryError> {
        let daemon =
            mdns_sd::ServiceDaemon::new().map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        Ok(Self {
            daemon,
            announced: None,
        })
    }

    /// Advertise `port` with the device fingerprint (hex) and name in TXT.
    ///
    /// The instance name is the fingerprint short hex so two devices never
    /// collide on one LAN. mDNS may be unavailable (multicast disabled,
    /// missing local-network permission); callers treat an error here as a
    /// degraded source, not a fatal one -- manual entry and signaling still
    /// work per REVIEW 4.3.
    pub fn advertise(
        &mut self,
        port: u16,
        fingerprint: &Fingerprint,
        device_name: &str,
    ) -> Result<(), DiscoveryError> {
        let fp_hex = hex_encode(fingerprint.as_bytes());
        let instance = short_hex(fingerprint.as_bytes());
        let host_name = format!("ak-{instance}.local.");
        let properties = [("fp", fp_hex.as_str()), ("name", device_name)];
        let info = mdns_sd::ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host_name,
            "",
            port,
            &properties[..],
        )
        .map_err(|e| DiscoveryError::Mdns(e.to_string()))?
        // No address is known at advertise time; let the daemon fill in
        // the host's interface addresses when registering.
        .enable_addr_auto();
        self.daemon
            .register(info.clone())
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        self.announced = Some(info);
        Ok(())
    }

    pub fn stop_advertise(&mut self) {
        if let Some(info) = self.announced.take() {
            // Graceful goodbye so peers drop us promptly. Best effort: an
            // error leaves the announcement to expire via TTL.
            if let Ok(status) = self.daemon.unregister(info.get_fullname()) {
                let _ = status.recv_timeout(Duration::from_millis(500));
            }
        }
    }

    /// Browse for `duration`, returning discovered candidates (IPv4 and IPv6).
    ///
    /// Every resolved address of every service instance becomes a candidate
    /// with source [`CandidateSource::Mdns`]. A malformed or missing `fp` TXT
    /// property yields `fingerprint: None` rather than an error: a candidate
    /// is only a hint and is authenticated before any transfer anyway.
    /// Returns what it collected when the time budget ends or the event
    /// channel closes.
    pub fn browse(&self, duration: Duration) -> Result<Vec<PeerCandidate>, DiscoveryError> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        let deadline = std::time::Instant::now() + duration;
        let mut out: Vec<PeerCandidate> = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            // Err means timeout or the daemon closed the channel; either way
            // the browsing window is over.
            let Ok(event) = receiver.recv_timeout(remaining) else {
                break;
            };
            let mdns_sd::ServiceEvent::ServiceResolved(info) = event else {
                continue;
            };
            let port = info.get_port();
            let fingerprint = info.get_property_val_str("fp").and_then(decode_fingerprint);
            for ip in info.get_addresses() {
                let candidate = PeerCandidate {
                    addr: SocketAddr::new(*ip, port),
                    fingerprint,
                    source: CandidateSource::Mdns,
                };
                if !out.iter().any(|c| c.addr == candidate.addr) {
                    out.push(candidate);
                }
            }
        }
        // Free the daemon-side channel for this search.
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        Ok(out)
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

fn source_rank(source: CandidateSource) -> u8 {
    match source {
        CandidateSource::Manual => 0,
        CandidateSource::Signaling => 1,
        CandidateSource::Mdns => 2,
    }
}

fn family_rank(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V6(_) => 0,
        IpAddr::V4(_) => 1,
    }
}

fn group_rank(ip: IpAddr, nets: &[ak_net_shim::Net]) -> u8 {
    if is_lan(ip, nets) {
        0
    } else {
        1
    }
}

fn is_lan(ip: IpAddr, nets: &[ak_net_shim::Net]) -> bool {
    nets.iter().any(|net| same_net(ip, net)) || is_v6_link_local(ip)
}

/// Prefix match with an address-family guard. An out-of-range prefix length
/// (family max exceeded) matches nothing, as does a family mismatch.
fn same_net(ip: IpAddr, net: &ak_net_shim::Net) -> bool {
    match (ip, net.addr) {
        (IpAddr::V4(ip), IpAddr::V4(net_ip)) => {
            if net.prefix_len > 32 {
                return false;
            }
            let mask = if net.prefix_len == 0 {
                0
            } else {
                !0u32 << (32 - u32::from(net.prefix_len))
            };
            (u32::from(ip) & mask) == (u32::from(net_ip) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net_ip)) => {
            if net.prefix_len > 128 {
                return false;
            }
            let mask = if net.prefix_len == 0 {
                0
            } else {
                !0u128 << (128 - u32::from(net.prefix_len))
            };
            (u128::from(ip) & mask) == (u128::from(net_ip) & mask)
        }
        _ => false,
    }
}

/// fe80::/10 per RFC 4291: link-local IPv6 is by definition on-link, so it
/// lands in the LAN tier even when the caller passes no nets.
fn is_v6_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        IpAddr::V4(_) => false,
    }
}

/// First 4 bytes as 8 hex chars (instance name / short display form).
fn short_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(8);
    for byte in bytes.iter().take(4) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 64 case-insensitive hex chars -> fingerprint. Local codec on purpose:
/// `Fingerprint::from_hex` belongs to ak-identity and this crate must not
/// panic while that implementation lands in parallel.
fn decode_fingerprint(text: &str) -> Option<Fingerprint> {
    if text.len() != 64 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (hex_digit(pair[0])?) << 4 | (hex_digit(pair[1])?);
    }
    Some(Fingerprint::from_bytes(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_net_shim::Net;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test ip")
    }

    fn cand(
        text: &str,
        fingerprint: Option<Fingerprint>,
        source: CandidateSource,
    ) -> PeerCandidate {
        PeerCandidate {
            addr: text.parse().expect("test addr"),
            fingerprint,
            source,
        }
    }

    fn fp(byte: u8) -> Fingerprint {
        Fingerprint::from_bytes([byte; 32])
    }

    fn net4(text: &str, prefix_len: u8) -> Net {
        Net {
            addr: ip(text),
            prefix_len,
        }
    }

    fn addr_strings(ranked: &[PeerCandidate]) -> Vec<String> {
        ranked.iter().map(|c| c.addr.to_string()).collect()
    }

    #[test]
    fn rank_orders_lan_then_public_with_v6_first() {
        let nets = [net4("192.168.1.0", 24)];
        let ranked = rank(
            vec![
                cand("8.8.8.8:9000", None, CandidateSource::Mdns),
                cand("[2001:db8::1]:9000", None, CandidateSource::Mdns),
                cand("192.168.1.55:9000", None, CandidateSource::Mdns),
                cand("[fe80::1]:9000", None, CandidateSource::Mdns),
            ],
            &nets,
        );
        // LAN tier (v6 first): fe80 (link-local even though no net covers it),
        // then 192.168.1.55. Public tier: v6 before v4.
        assert_eq!(
            addr_strings(&ranked),
            vec![
                "[fe80::1]:9000",
                "192.168.1.55:9000",
                "[2001:db8::1]:9000",
                "8.8.8.8:9000"
            ]
        );
    }

    #[test]
    fn rank_with_no_nets_is_public_only_grouped_by_family() {
        let ranked = rank(
            vec![
                cand("192.168.1.55:9000", None, CandidateSource::Mdns),
                cand("8.8.8.8:9000", None, CandidateSource::Mdns),
                cand("[2001:db8::1]:9000", None, CandidateSource::Mdns),
            ],
            &[],
        );
        assert_eq!(
            addr_strings(&ranked),
            vec!["[2001:db8::1]:9000", "192.168.1.55:9000", "8.8.8.8:9000"]
        );
    }

    #[test]
    fn rank_dedupes_by_addr_keeping_higher_priority_source() {
        let ranked = rank(
            vec![
                cand("1.2.3.4:5", Some(fp(1)), CandidateSource::Mdns),
                cand("9.9.9.9:5", Some(fp(3)), CandidateSource::Mdns),
                cand("1.2.3.4:5", Some(fp(2)), CandidateSource::Manual),
                cand("9.9.9.9:5", Some(fp(4)), CandidateSource::Signaling),
                // Same addr and same source twice: first occurrence wins.
                cand("4.4.4.4:5", Some(fp(5)), CandidateSource::Manual),
                cand("4.4.4.4:5", Some(fp(6)), CandidateSource::Manual),
            ],
            &[],
        );
        assert_eq!(ranked.len(), 3);
        // Dedup keeps the first-seen position but the better source/payload.
        assert_eq!(ranked[0].source, CandidateSource::Manual);
        assert_eq!(ranked[0].fingerprint, Some(fp(2)));
        assert_eq!(ranked[1].source, CandidateSource::Signaling);
        assert_eq!(ranked[1].fingerprint, Some(fp(4)));
        assert_eq!(ranked[2].fingerprint, Some(fp(5)));
    }

    #[test]
    fn rank_is_stable_within_a_tier() {
        let ranked = rank(
            vec![
                cand("2.2.2.2:5", None, CandidateSource::Signaling),
                cand("1.1.1.1:5", None, CandidateSource::Mdns),
            ],
            &[],
        );
        assert_eq!(addr_strings(&ranked), vec!["2.2.2.2:5", "1.1.1.1:5"]);
    }

    #[test]
    fn same_net_matches_prefix_and_family_strictly() {
        let net8 = net4("10.0.0.0", 8);
        assert!(same_net(ip("10.1.2.3"), &net8));
        assert!(same_net(ip("10.255.0.1"), &net8));
        assert!(!same_net(ip("11.0.0.1"), &net8));
        let net24 = net4("192.168.1.0", 24);
        assert!(same_net(ip("192.168.1.200"), &net24));
        assert!(!same_net(ip("192.168.2.1"), &net24));
        // /0 matches every address of the same family.
        assert!(same_net(ip("8.8.8.8"), &net4("0.0.0.0", 0)));
        // Out-of-range prefix lengths never match.
        assert!(!same_net(ip("10.0.0.1"), &net4("10.0.0.0", 33)));
        // IPv6 prefixes.
        let net6 = Net {
            addr: ip("2001:db8:aa::"),
            prefix_len: 48,
        };
        assert!(same_net(ip("2001:db8:aa::1"), &net6));
        assert!(!same_net(ip("2001:db8:bb::1"), &net6));
        assert!(!same_net(ip("2001:db8:aa::1"), &net24));
    }

    #[test]
    fn link_local_detection_is_v6_only() {
        assert!(is_v6_link_local(ip("fe80::1")));
        assert!(is_v6_link_local(ip("febf::1")));
        assert!(!is_v6_link_local(ip("fec0::1")));
        assert!(!is_v6_link_local(ip("169.254.1.1")));
    }

    fn fp_hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn parse_manual_list_accepts_valid_forms() {
        let parsed = parse_manual_list("192.168.1.5:9000").expect("single");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].addr,
            cand("192.168.1.5:9000", None, CandidateSource::Manual).addr
        );
        assert_eq!(parsed[0].source, CandidateSource::Manual);
        assert_eq!(parsed[0].fingerprint, None);

        // Mixed separators and brackets.
        let parsed = parse_manual_list(" 10.0.0.1:1,\t[::1]:2\n[2001:db8::7]:3 ").expect("mixed");
        assert_eq!(
            addr_strings(&parsed),
            vec!["10.0.0.1:1", "[::1]:2", "[2001:db8::7]:3"]
        );

        // Fingerprint suffix, including uppercase hex.
        let bytes = [0xabu8; 32];
        let text = format!("1.2.3.4:5={}", fp_hex(&bytes).to_uppercase());
        let parsed = parse_manual_list(&text).expect("with fp");
        assert_eq!(parsed[0].fingerprint, Some(Fingerprint::from_bytes(bytes)));

        // Empty and separator-only input yields no candidates, not an error.
        assert!(parse_manual_list("").expect("empty").is_empty());
        assert!(parse_manual_list(" ,\t\n").expect("separators").is_empty());
    }

    #[test]
    fn parse_manual_list_rejects_invalid_items_quoting_them() {
        let bad = [
            "not-an-entry",
            "1.2.3.4",
            "1.2.3.4:",
            "1.2.3.4:notaport",
            "1.2.3.4:70000",
            ":80",
            "[::1",
            "example.com:80",
            "1.2.3.4:5=xyz",
            "1.2.3.4:5=",
            "1.2.3.4:5=abcd", // too short
            "1.2.3.4:5=a",    // too short, quoted fragment check
            "1.2.3.4:5=0",    // single char
            "1.2.3.4:5==",
        ];
        for item in bad {
            let err = parse_manual_list(item).expect_err(item);
            assert!(
                err.to_string().contains(item),
                "error {err} does not quote the fragment '{item}'"
            );
        }
        // One bad item fails the whole list, even mixed with good ones.
        assert!(parse_manual_list("1.2.3.4:5, bad item").is_err());
    }

    #[test]
    fn decode_fingerprint_requires_64_hex_chars() {
        let bytes = [7u8; 32];
        assert_eq!(
            decode_fingerprint(&fp_hex(&bytes)),
            Some(Fingerprint::from_bytes(bytes))
        );
        assert_eq!(decode_fingerprint(&fp_hex(&bytes)[..63]), None);
        assert_eq!(decode_fingerprint(&format!("{}0", fp_hex(&bytes))), None);
        assert_eq!(
            decode_fingerprint(&fp_hex(&bytes).replacen('7', "z", 1)),
            None
        );
    }

    // Best-effort integration test: mDNS requires multicast, which may be
    // unavailable in this environment. Every degraded outcome prints the
    // reason via eprintln and counts as a skip, not a failure.
    #[test]
    fn mdns_advertise_browse_best_effort() {
        let fingerprint = fp(7);
        let mut discovery = match MdnsDiscovery::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: mDNS daemon unavailable in this environment: {e}");
                return;
            }
        };

        // A free UDP port number to advertise; discovery only publishes it.
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|sock| sock.local_addr())
            .map(|addr| addr.port())
            .unwrap_or(45_678);

        if let Err(e) = discovery.advertise(port, &fingerprint, "ak-test-device") {
            eprintln!("SKIP: mDNS advertise failed (multicast likely unavailable): {e}");
            return;
        }

        let found = match discovery.browse(Duration::from_secs(3)) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("SKIP: mDNS browse failed: {e}");
                return;
            }
        };
        discovery.stop_advertise();

        match found.iter().find(|c| c.fingerprint == Some(fingerprint)) {
            None => {
                eprintln!(
                    "SKIP: mDNS browse saw {} candidate(s) but none matched the local fingerprint \
                     (multicast loopback likely unavailable)",
                    found.len()
                );
            }
            Some(candidate) => {
                assert_eq!(candidate.source, CandidateSource::Mdns);
                assert_eq!(candidate.fingerprint, Some(fingerprint));
                assert_eq!(candidate.addr.port(), port);
            }
        }
    }
}
