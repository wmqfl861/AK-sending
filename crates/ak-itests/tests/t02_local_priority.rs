//! TEST_PLAN T02（决策形态）：本地与公网候选同时可达 —— 优先本地，服务器
//! 不承载文件。
//!
//! 层级声明：本文件是**纯单元级决策检查**（ak-discovery::rank + ak-net
//! classify），归档在场景文件里以保持 T 编号可查；它不是 L2 传输场景。
//! “优先本地”的可判定部分 = 候选排序：LAN 组（同网段 / IPv6 链路本地）
//! 永远排在公网组之前，组内 IPv6 先于 IPv4，去重保留更高优先级来源。
//! “服务器不承载文件”是 ADR-001 架构不变量（无 relay/上传路径，类型层面
//! 不存在该通道），不在此断言 —— 证明方式见 TEST_PLAN“证明服务器不传文件”
//! 一节，需真实协调服务抓包，超出本 crate 范围。
#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ak_core::policy::PathKind;
use ak_discovery::ak_net_shim::Net as RankNet;
use ak_discovery::{rank, CandidateSource, PeerCandidate};
use ak_net::classify;

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn v6(segments: [u16; 8]) -> IpAddr {
    IpAddr::V6(Ipv6Addr::from(segments))
}

fn candidate(addr_text: &str, source: CandidateSource) -> PeerCandidate {
    PeerCandidate {
        addr: addr_text.parse().expect("test candidate address"),
        fingerprint: None,
        source,
        // Display-only metadata added to the public struct later; ranking
        // tests deliberately exercise the unnamed-peer shape.
        device_name: None,
    }
}

/// LAN/public mixed list: local candidates must outrank every public one,
/// IPv6 before IPv4 inside each tier, and the winner really classifies as a
/// local direct path while the trailing public candidate does not.
#[test]
fn mixed_lan_public_candidates_rank_local_first() {
    let rank_nets = [
        RankNet {
            addr: v4(192, 168, 1, 10),
            prefix_len: 24,
        },
        RankNet {
            addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
            prefix_len: 64,
        },
    ];
    // Same nets in ak-net's own type for the classification cross-check.
    let classify_nets = [
        classify::Net {
            addr: v4(192, 168, 1, 10),
            prefix_len: 24,
        },
        classify::Net {
            addr: v6([0xfd00, 0, 0, 0, 0, 0, 0, 1]),
            prefix_len: 64,
        },
    ];

    // Deliberately adversarial input order: public entries first.
    let ranked = rank(
        vec![
            candidate("8.8.8.8:7100", CandidateSource::Mdns), // public v4
            candidate("[2001:db8::1]:7100", CandidateSource::Mdns), // public v6
            candidate("203.0.113.9:7100", CandidateSource::Manual), // public v4, manual
            candidate("192.168.1.55:7100", CandidateSource::Mdns), // LAN v4
            candidate("[fe80::9]:7100", CandidateSource::Mdns), // LAN v6 (link-local)
            candidate("[fd00::42]:7100", CandidateSource::Signaling), // LAN v6 (ULA)
        ],
        &rank_nets,
    );

    let addresses: Vec<String> = ranked.iter().map(|c| c.addr.to_string()).collect();
    assert_eq!(
        addresses,
        vec![
            "[fe80::9]:7100",     // LAN tier, IPv6 first (stable input order)
            "[fd00::42]:7100",    // LAN tier, IPv6
            "192.168.1.55:7100",  // LAN tier, IPv4
            "[2001:db8::1]:7100", // public tier, IPv6 before IPv4
            "8.8.8.8:7100",       // public v4 (came before 203.0.113.9)
            "203.0.113.9:7100",   // public v4, stable within the tier
        ],
        "T02 decision: local candidates must outrank public ones, v6 before v4"
    );

    // Cross-module tie-in: the top-ranked candidate is genuinely a local
    // direct path per ak-net's classifier; the last one is not.
    assert_eq!(
        classify::classify(ranked[0].addr.ip(), &classify_nets),
        PathKind::LocalDirect
    );
    assert_eq!(
        classify::classify(ranked[ranked.len() - 1].addr.ip(), &classify_nets),
        PathKind::InternetDirect
    );
    // Every LAN-tier winner classifies local, every public one does not.
    for candidate in ranked.iter().take(3) {
        assert_eq!(
            classify::classify(candidate.addr.ip(), &classify_nets),
            PathKind::LocalDirect
        );
    }
}

/// Duplicate addresses collapse to one slot keeping the higher-priority
/// source (Manual > Signaling > Mdns) — a manual pairing never loses to a
/// server-relayed or mDNS hint for the same endpoint.
#[test]
fn duplicate_address_keeps_the_highest_priority_source() {
    let ranked = rank(
        vec![
            candidate("192.168.1.55:7100", CandidateSource::Mdns),
            candidate("8.8.8.8:7100", CandidateSource::Mdns),
            candidate("192.168.1.55:7100", CandidateSource::Manual),
            candidate("8.8.8.8:7100", CandidateSource::Signaling),
        ],
        &[],
    );
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].addr.to_string(), "192.168.1.55:7100");
    assert_eq!(ranked[0].source, CandidateSource::Manual);
    assert_eq!(ranked[1].addr.to_string(), "8.8.8.8:7100");
    assert_eq!(ranked[1].source, CandidateSource::Signaling);
}

/// With no local nets known (fresh boot, no interface data), ranking must not
/// invent local priority: everything is public, IPv6 first — never a relay or
/// unknown-tier entry, because no such tier exists.
#[test]
fn no_known_nets_means_public_only_grouping() {
    let ranked = rank(
        vec![
            candidate("192.168.1.55:7100", CandidateSource::Mdns),
            candidate("8.8.8.8:7100", CandidateSource::Mdns),
            candidate("[2001:db8::1]:7100", CandidateSource::Signaling),
            // Link-local still counts as on-link by definition (fe80::/10).
            candidate("[fe80::1]:7100", CandidateSource::Mdns),
        ],
        &[],
    );
    let addresses: Vec<String> = ranked.iter().map(|c| c.addr.to_string()).collect();
    assert_eq!(
        addresses,
        vec![
            "[fe80::1]:7100",
            "[2001:db8::1]:7100",
            "192.168.1.55:7100",
            "8.8.8.8:7100",
        ]
    );
}
