//! The v1 task-plane CONNECTIVITY plumbing shared by ak-ffi and ak-cli
//! (CR-P2-11, review 2026-11-11 §4.5).
//!
//! Until this module existed the exact same v1 connectivity logic was
//! maintained twice — `crates/ak-ffi/src/task.rs` and
//! `crates/ak-cli/src/task_cli.rs` carried ~36 same-named functions
//! (candidate enumeration, the receiver's socket lifecycle, the echo
//! responder, the send-time probe round, the split connect budget, the
//! idle close notification), so every cross-NAT change had to be made and
//! tested twice and the copies drifted (the Windows WSAECONNRESET fix had
//! to be synchronized by hand). This module is that PURE MOVE/dedup: the
//! bodies, tunables, doc rationale and the mirrored unit tests were moved
//! here verbatim (the FFI copy is the reference text; the one behavioral
//! divergence found while merging is marked `F1-DIVERGENCE` below), and
//! the two adapters now keep only what genuinely differs — event emission
//! vs stdout lines, the FFI runtime state vs the CLI session state.
//!
//! Scope (the review's "把候选生成、接收端 socket 生命周期、探测与打洞、
//! 拨号排序收进一个共享模块"):
//!
//! * candidate enumeration and ordering — [`task_net::local_candidates`],
//!   [`task_net::filter_and_order_candidates`], [`task_net::insert_reflexive_candidate`],
//!   [`task_net::order_candidates`], [`task_net::blob_candidates`] and friends;
//! * the receiver socket lifecycle — [`task_net::ReceiverSocket`] /
//!   [`task_net::bind_receiver_socket`]: dual-stack `[::]` bind with C1's v4
//!   fallback chain, the optional STUN server-reflexive query ON THAT
//!   SAME SOCKET (bounded by [`task_net::STUN_QUERY_BUDGET`], fail-open), and the
//!   socket handed back for `DirectEndpoint::bind_with_socket` so the
//!   query's NAT mapping survives into the QUIC listener;
//! * the probe echo responder — [`task_net::bind_echo_responder_socket`] +
//!   [`task_net::echo_responder_loop`] (anti-reflection, per-source reply budgets,
//!   the CR-P1-02 leg-3 observation slot) and the transient-error
//!   tolerance helper [`task_net::responder_error_is_transient`];
//! * the send-time simultaneous-probe round — [`task_net::probe_dial_round`] with
//!   [`task_net::observed_dial_head`] / [`task_net::observed_first_order`] and the
//!   [`task_net::PROBE_DIAL_INTERVAL`] / [`task_net::PROBE_DIAL_WINDOW`] /
//!   [`task_net::OBSERVED_DIAL_BUDGET`] tunables;
//! * the split connect budget — [`task_net::ConnectMarks`] +
//!   [`task_net::drive_with_connect_budget`];
//! * the pairing-time probe retry ([`task_net::probe_with_retry`]) and the bounded
//!   error vocabularies ([`task_net::task_error_message`],
//!   [`task_net::handshake_failure_word`], [`task_net::one_line_reason`],
//!   [`task_net::windows_root_name_refusal`]);
//! * the idle close notification's dial core ([`task_net::deliver_close_notify`]).
//!
//! Everything here is adapter-neutral: no events, no stdout — the two
//! callers inject their reporting as small callbacks/arguments.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use ak_core::error::ErrorCode;
use ak_engine::{EngineError, Progress};
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_wire::{Frame, Message};

use super::{
    encode_endpoint_blob, PairFlowError, PeerEndpoint, ProbeOutcome, TaskDirection, TaskError,
    TaskPairing, ENDPOINT_BLOB_MAX_CANDIDATES,
};

// ---------------------------------------------------------------------------
// Tunables (moved verbatim from the two adapters — same rationale per value).
// ---------------------------------------------------------------------------

/// Per-candidate connect budget of a task-plane send (the wave-26 P1-01
/// value: cross-VPN QUIC+TLS handshakes measurably need more than 3 s; LAN
/// candidates still connect in milliseconds, so the larger budget costs
/// nothing on fast paths).
pub const TASK_CONNECT_BUDGET: Duration = Duration::from_secs(10);

/// Idle budget of a task-plane send: must comfortably cover the receiver's
/// pending-consent window (the sender's offer waits while the peer's user
/// decides) plus slow-storage source scans on the far end.
pub const TASK_SEND_IDLE: Duration = Duration::from_secs(120);

/// Brief pause between candidate attempts (the failed attempt's QUIC
/// endpoint releases its UDP socket asynchronously; both adapters use the
/// same 150 ms).
pub const ATTEMPT_PAUSE: Duration = Duration::from_millis(150);

/// Whole-list connect passes of a task-plane send (wave-29 P1-04
/// follow-up): a send may fire the instant its join returned, while the
/// paired peer's owner loop is still a beat away from registering the
/// connection its receiver admission is bound to — its first dial can be
/// REFUSED at the TLS gate through no fault of the sender. Purely
/// connect-level failures (nothing ever connected) are retried after a
/// short settle pause; identity checks are unaffected — the peer still
/// authorizes exactly the pairing's verified fingerprint.
pub const TASK_CONNECT_PASSES: usize = 3;

/// Settle pause before the next whole-list connect pass (see
/// [`TASK_CONNECT_PASSES`]).
pub const TASK_SETTLE_PAUSE: Duration = Duration::from_millis(300);

/// Tunables of the send-time simultaneous-probe round (CR-P1-02 改法第 3
/// 条): ONE dual-stack UDP socket fires `holepunch::probe` packets at
/// every dial candidate every [`PROBE_DIAL_INTERVAL`] for one
/// [`PROBE_DIAL_WINDOW`] before the first QUIC dial attempt. The window
/// IS the pre-dial latency budget (cap ~1-2 s per the fix brief): the
/// receiver's echo responder — and, while the pairing-time rounds still
/// overlap, the peer's own pairing probe — answers within the first
/// 150 ms burst on a live path, but `holepunch::probe` only reports at
/// window end, so the round blocks the full second even then (an
/// early-exit hook on the ak-net probe is the recorded API wish). The
/// round runs OUTSIDE the per-attempt connect budgets and pass counting:
/// [`TASK_CONNECT_BUDGET`] × [`TASK_CONNECT_PASSES`] behave exactly as
/// before, so the worst-case total connect phase grows by at most
/// window + [`OBSERVED_DIAL_BUDGET`] ≈ 3 s.
pub const PROBE_DIAL_INTERVAL: Duration = Duration::from_millis(150);
/// The probe round's wall-clock window (see [`PROBE_DIAL_INTERVAL`]).
pub const PROBE_DIAL_WINDOW: Duration = Duration::from_secs(1);

/// Connect budget of the FIRST dial attempt when it targets the probe
/// round's OBSERVED address (CR-P1-02 改法第 3 条). An observation names
/// a route that ANSWERED PROBES, not a socket that necessarily accepts
/// QUIC — today's echo responder (and any probe-echoing endpoint a
/// reflexive candidate can name) answers probes but holds no QUIC
/// listener, so the observed-first attempt must not cost the static
/// order's full [`TASK_CONNECT_BUDGET`] before the fallback continues. A
/// genuinely hot mapping completes its QUIC+TLS handshake well inside
/// 2 s (the cross-VPN handshakes that motivated the 10 s budget are not
/// hot mappings); an observed address that is also a real candidate
/// keeps its static slot and the full budget there.
pub const OBSERVED_DIAL_BUDGET: Duration = Duration::from_secs(2);

/// Bound for the resident receiver's first bind (surfaced as the code's
/// candidates). Binding a wildcard UDP socket is fast; the bound exists so
/// a wedged stack fails the create op instead of hanging it.
pub const RECEIVER_BIND_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounded wait for the registry connect of the task-pairing stack (same
/// budget as the pairing plane's background connect).
pub const REGISTRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Redeem window offered to the registry by a task-plane join: how long
/// the code owner has to answer once a joiner typed the code. A live
/// owner loop answers in milliseconds; unknown/expired/revoked codes and
/// offline owners surface the unified "unavailable" answer at this
/// deadline (the ak-session default of 90 s is the long-lived invite
/// UX, not the short-TTL task-code one).
pub const TASK_REDEEM_WINDOW_MS: u64 = 10_000;

/// Probe tunables of the task plane: one probe round listens 1.5 s with
/// probes every 150 ms (ak-session's defaults); a failed first round gets
/// exactly one retry after a short stagger so the two sides' windows
/// overlap even when one side's pairing completion lands late.
pub const TASK_PROBE_OPTIONS: super::ProbeOptions = super::ProbeOptions {
    interval: Duration::from_millis(150),
    window: Duration::from_millis(1_500),
};

/// Pause before the single pairing-probe retry (see
/// [`TASK_PROBE_OPTIONS`]).
pub const TASK_PROBE_RETRY_PAUSE: Duration = Duration::from_millis(300);

/// Upper bounds of the endpoint blob's candidate list (the wire bound is
/// 16 candidates / 512 bytes; the plane stays well under both so the
/// shrink loop below is a belt-and-braces guard, never the common path).
/// The dial cap truncates from the tail: IPv4 candidates lead (the
/// documented family order), so only a v4-heavy host (5+ interface
/// addresses) can crowd IPv6 out — the common laptop set (v4 + loopback +
/// ≤3 global v6 + ::1) fits whole.
pub const MAX_DIAL_CANDIDATES: usize = 8;

/// Echo-candidate cap of the endpoint blob (see
/// [`MAX_DIAL_CANDIDATES`]).
pub const MAX_ECHO_CANDIDATES: usize = 4;

/// Cap of globally-routable IPv6 interface addresses in the derived
/// candidate list (CR-P1-02 改法第 1 条): at most one address per distinct
/// /64 prefix first (a NIC's stable + privacy-temporary GUAs collapse to
/// one entry), at most this many overall, so a GUA-heavy host cannot
/// crowd the v4 candidates or push the blob at the 512-byte wire bound.
pub const MAX_V6_IFACE_CANDIDATES: usize = 3;

/// Total wall-clock budget one STUN server-reflexive query may add to
/// receiver startup (CR-P1-02 改法第 2 条). Chosen against
/// `ak_signal::stun::query_on`'s INTERNAL schedule — it retransmits on a
/// 1 s / 2 s / 4 s backoff all bounded by the timeout it is handed, so a
/// 2 s budget buys exactly two transmissions (t=0 and t≈1 s) and returns
/// `Timeout` by t=2 s at the latest: an unreachable/silent server costs
/// at most 2 s, fail-open, and never delays the bind past a fraction of
/// [`RECEIVER_BIND_TIMEOUT`]. A reachable server answers in one
/// round-trip (tens of ms in-region), where the second transmission is
/// pure insurance.
pub const STUN_QUERY_BUDGET: Duration = Duration::from_secs(2);

/// Wire marker of the task-plane close notification (wave-29 P2-03): a
/// disconnecting side that has no live transfer (whose QUIC drop the peer
/// would observe) dials the paired peer's advertised data endpoint — TLS
/// pinned to the pairing's verified fingerprint — and hands it exactly one
/// Error frame with `ErrorCode::Cancelled` and this message before
/// closing. The peer's resident receiver recognizes the marker and ends
/// its pairing ByPeer. Reusing the existing Error-frame semantics adds no
/// wire vocabulary.
pub const TASK_CLOSE_MESSAGE: &str = "TASK-CLOSE: paired device disconnected";

/// Per-candidate budget of the close-notify dial (see
/// [`TASK_CLOSE_MESSAGE`]).
pub const TASK_CLOSE_CONNECT_BUDGET: Duration = Duration::from_secs(3);

/// Overall budget of the close-notify dial (best effort: a peer behind a
/// dead path simply is not reachable; the disconnect itself already
/// succeeded locally).
pub const TASK_CLOSE_TOTAL_BUDGET: Duration = Duration::from_secs(6);

/// Anti-reflection constants of the probe echo responder (wave-29 P2-12):
/// replies carry the all-zero nonce (a real prober's nonce is
/// time/counter-derived and never all-zero) and are NEVER themselves
/// answered, and each source endpoint gets a bounded reply budget per
/// window so a spoofed flood cannot sustain CPU burn either.
pub const ECHO_REPLY_NONCE: [u8; 16] = [0u8; 16];

/// Per-source reply budget per [`ECHO_REPLY_WINDOW`] (a genuine probe
/// round sends ~7 packets/s).
pub const ECHO_REPLY_BUDGET: u32 = 32;

/// Window of the per-source echo reply budget.
pub const ECHO_REPLY_WINDOW: Duration = Duration::from_secs(1);

/// Hard cap of the echo responder's per-source budget table (the table
/// clears wholesale when full — bounded memory under spoofed source
/// churn).
pub const ECHO_SOURCES_MAX: usize = 1024;

// ---------------------------------------------------------------------------
// Candidate enumeration and ordering (CR-P1-02 改法第 1 条/第 2 条).
// ---------------------------------------------------------------------------

/// Every local interface address (v4, then globally-routable v6) with
/// `bound`'s port, loopbacks last in each family. The zero-address task
/// plane keeps it so a same-host join always has a working candidate of
/// each family — `is_ipv4_private` counts loopback as private and
/// `Ipv6Addr::is_loopback` likewise, so both ride WITHIN the LAN group,
/// insertion-ordered after the interface addresses). Selection and order
/// are [`filter_and_order_candidates`]'s documented rule (CR-P1-02 改法
/// 第 1 条: with the receiver dual-stack-bound, two NAT'd IPv6-capable
/// peers get a NAT-free v6 path).
pub fn local_candidates(bound: SocketAddr) -> Vec<SocketAddr> {
    let port = bound.port();
    let raw = match ak_net::classify::local_nets() {
        Ok(nets) => nets.into_iter().map(|net| net.addr).collect(),
        Err(_) => Vec::new(),
    };
    filter_and_order_candidates(raw)
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

/// Candidate selection over raw interface addresses — pure on purpose (no
/// NIC reads; tests pin the rule without host dependence). Rule:
///
/// * v4-mapped IPv6 input (`::ffff:a.b.c.d`) normalizes to plain IPv4
///   (same normalization as [`order_candidates`]);
/// * every plain IPv4 interface address is kept, deduped, input order
///   preserved — byte-identical to the pre-IPv6 list;
/// * IPv6 keeps ONLY globally-routable unicast addresses: link-local
///   `fe80::/10` is excluded (it needs a scope index the `ip:port`
///   candidate form cannot carry), and ULA `fc00::/7`, multicast,
///   documentation and other non-global ranges drop out with it. Kept
///   entries dedupe, collapse to at most one per distinct /64 prefix
///   (one interface = one candidate), and cap at
///   [`MAX_V6_IFACE_CANDIDATES`];
/// * `127.0.0.1` and `::1` are appended when absent (same-host joins
///   always keep a working candidate per family);
/// * family order is v4 first, then v6 — the consumer
///   ([`order_candidates`]) re-sorts LAN-first with this order stable
///   within each group, so a v4-only peer's dial sequence is unchanged.
pub fn filter_and_order_candidates(raw: Vec<std::net::IpAddr>) -> Vec<std::net::IpAddr> {
    use std::net::IpAddr;
    let mut v4: Vec<IpAddr> = Vec::with_capacity(raw.len());
    let mut v6: Vec<IpAddr> = Vec::new();
    let mut v6_prefixes: Vec<u64> = Vec::new();
    for addr in raw {
        let addr = match addr {
            IpAddr::V6(v6_addr) => match v6_addr.to_ipv4_mapped() {
                Some(v4_addr) => IpAddr::V4(v4_addr),
                None => {
                    let prefix = (u128::from(v6_addr) >> 64) as u64;
                    if is_global_unicast_v6(v6_addr)
                        && v6.len() < MAX_V6_IFACE_CANDIDATES
                        && !v6_prefixes.contains(&prefix)
                    {
                        v6_prefixes.push(prefix);
                        v6.push(IpAddr::V6(v6_addr));
                    }
                    continue;
                }
            },
            v4_addr => v4_addr,
        };
        if !v4.contains(&addr) {
            v4.push(addr);
        }
    }
    let v4_loopback = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    if !v4.contains(&v4_loopback) {
        v4.push(v4_loopback);
    }
    let v6_loopback = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
    if !v6.contains(&v6_loopback) {
        v6.push(v6_loopback);
    }
    v4.extend(v6);
    v4
}

/// Globally-routable IPv6 unicast (the IANA 2000::/3 block, with the
/// documentation `2001:db8::/32` and benchmarking `2001:2::/48` sub-ranges
/// carved out) — the stable-toolchain equivalent of std's unstable
/// `Ipv6Addr::is_unicast_global` for the ranges that can plausibly sit on
/// an interface. Everything a NAT-free peer path needs (GUAs) passes;
/// link-local `fe80::/10`, ULA `fc00::/7`, multicast and loopback sit
/// outside 2000::/3 and fail.
pub fn is_global_unicast_v6(addr: std::net::Ipv6Addr) -> bool {
    let segments = addr.segments();
    let global_block = (segments[0] & 0xe000) == 0x2000;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let benchmarking = segments[0] == 0x2001 && segments[1] == 0x0002;
    global_block && !documentation && !benchmarking
}

/// Insert the STUN server-reflexive candidate (CR-P1-02 改法第 2 条) into
/// a DERIVED candidate list — pure on purpose (tests pin the rule without
/// any socket). Rules:
///
/// * `None` (no STUN server configured, or the caller-supplied list is
///   explicit/verbatim) returns the list unchanged;
/// * a v4-mapped reflexive address normalizes to plain IPv4 (same
///   normalization as [`order_candidates`]); an unspecified address
///   (`0.0.0.0`/`::`, a broken server's reflection) is dropped — the
///   probe would skip it as undialable anyway;
/// * a reflexive address that EQUALS an existing candidate means the host
///   is not behind a mapping NAT for this socket (same-subnet server, or
///   the public IP is already on an interface) — the duplicate is
///   skipped, nothing new is dialable;
/// * otherwise the reflexive address is inserted right after the v4
///   INTERFACE block and before the loopback tail (the slot
///   [`filter_and_order_candidates`] reserves between them). Position
///   rationale: the reflexive address is public — HIGHER reachability
///   across NATs than private LAN addresses, but the consumer
///   ([`order_candidates`]) re-sorts LAN-first, so same-subnet matches
///   still dial first; within the non-LAN group the v4 reflexive leads
///   the interface IPv6 block (v4 reflection is the primary cross-NAT
///   leg; a v6 reflexive from a v6 STUN server rides the same slot).
pub fn insert_reflexive_candidate(
    candidates: Vec<SocketAddr>,
    reflexive: Option<SocketAddr>,
) -> Vec<SocketAddr> {
    let Some(reflexive) = reflexive else {
        return candidates;
    };
    let reflexive = ak_net::classify::expand_mapped(reflexive);
    if reflexive.ip().is_unspecified() {
        return candidates;
    }
    if candidates
        .iter()
        .any(|addr| ak_net::classify::expand_mapped(*addr) == reflexive)
    {
        return candidates;
    }
    let mut out = candidates;
    // The insertion slot: after the last plain-v4 interface address, i.e.
    // just before the first loopback/v6 entry of the derived order.
    let slot = out
        .iter()
        .position(|addr| match ak_net::classify::expand_mapped(*addr).ip() {
            std::net::IpAddr::V4(v4) => v4.is_loopback(),
            std::net::IpAddr::V6(_) => true,
        })
        .unwrap_or(out.len());
    out.insert(slot, reflexive);
    out
}

/// LAN preference for one candidate (RFC1918 private IPv4 — loopback
/// included by ak-net's `is_ipv4_private` — and loopback IPv6; a routing
/// hint, never an authorization).
pub fn prefers_lan(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => ak_net::classify::is_ipv4_private(v4),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Dedupe (normalizing v4-mapped IPv6 to plain IPv4), then stable-sort
/// LAN-first: within a group the registration order survives.
pub fn order_candidates(targets: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut ordered: Vec<SocketAddr> = Vec::with_capacity(targets.len());
    for addr in targets {
        let addr = ak_net::classify::expand_mapped(addr);
        if !ordered.contains(&addr) {
            ordered.push(addr);
        }
    }
    ordered.sort_by_key(|addr| !prefers_lan(addr));
    ordered
}

/// Assemble the endpoint-blob candidate list of one side: the dial
/// candidates (real receiver port first / caller extras verbatim / the
/// machine's interfaces), then — in the derived path only — the probe
/// echo port, deduped, capped, and shrunk until the blob fits the wire
/// bound (a defensive loop; the caps keep it far below 512 bytes). `name`
/// rides the fit-check endpoint exactly as the registering side's blob
/// will carry it.
pub fn blob_candidates(
    dial: Vec<SocketAddr>,
    extras: Vec<SocketAddr>,
    echo: Vec<SocketAddr>,
    name: &str,
) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::with_capacity(dial.len() + extras.len() + echo.len());
    for addr in dial.into_iter().take(MAX_DIAL_CANDIDATES) {
        let addr = ak_net::classify::expand_mapped(addr);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    for addr in extras {
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    for addr in echo.into_iter().take(MAX_ECHO_CANDIDATES) {
        let addr = ak_net::classify::expand_mapped(addr);
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    if out.len() > ENDPOINT_BLOB_MAX_CANDIDATES {
        out.truncate(ENDPOINT_BLOB_MAX_CANDIDATES);
    }
    while out.len() > 1 {
        let endpoint = PeerEndpoint {
            candidates: out.clone(),
            name: name.to_string(),
            direction: TaskDirection::Send,
        };
        if encode_endpoint_blob(&endpoint).is_ok() {
            break;
        }
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Pairing-time probe and bounded error vocabularies.
// ---------------------------------------------------------------------------

/// One probe round, retried once after a short stagger (the peer's own
/// probe may start a beat later than ours on the slow path).
pub async fn probe_with_retry(pairing: &TaskPairing, peer: &PeerEndpoint) -> ProbeOutcome {
    let first = pairing.probe_peer(peer).await;
    if matches!(first, ProbeOutcome::Success { .. }) {
        return first;
    }
    tokio::time::sleep(TASK_PROBE_RETRY_PAUSE).await;
    let second = pairing.probe_peer(peer).await;
    if matches!(second, ProbeOutcome::Success { .. }) {
        second
    } else {
        first
    }
}

/// Map an ak-session task-pairing failure onto the task plane's bounded
/// op error strings (the registry never reveals whether a code exists —
/// unknown, expired and revoked all answer `PAIR_UNAVAILABLE`, so the
/// joiner's honest wording covers all three).
///
/// F1-DIVERGENCE (CR-P2-11 move): the ak-ffi copy carried a dedicated
/// arm for `RATE_LIMITED` (0x2102, pairing-flow.md §12 — the device
/// exhausted its hourly invite-creation budget; actionable copy beats the
/// raw code) that the ak-cli copy lacked. Per the pure-move rules the
/// FFI behavior is kept for BOTH adapters.
pub fn task_error_message(error: &TaskError) -> String {
    match error {
        TaskError::Flow(PairFlowError::Rejected { code })
            if *code == ak_signal::protocol_v2::message::REDEEM_EXHAUSTED =>
        {
            "the task code was already joined (one code pairs exactly once)".to_string()
        }
        // RATE_LIMITED (0x2102, pairing-flow.md §12): the device exhausted
        // its hourly invite-creation budget. Actionable copy beats the raw
        // code — the user learns it REFILLS and roughly when.
        TaskError::Flow(PairFlowError::Rejected { code })
            if *code == ak_signal::protocol_v2::message::RATE_LIMITED =>
        {
            "rate limited: too many codes created this hour; the budget refills within an hour — wait and retry".to_string()
        }
        TaskError::Flow(PairFlowError::Unavailable { .. }) => {
            "unknown task code (or expired, revoked, owner offline)".to_string()
        }
        TaskError::Flow(PairFlowError::BadCode(cause)) => {
            format!("task code rejected: {cause}")
        }
        other => format!("registry task pairing failed: {other}"),
    }
}

/// Bounded reason word of one failed accept (wave-30 P1-04 residual): a
/// fixed two-word vocabulary, never the raw transport text (hostile peers
/// control parts of it).
pub fn handshake_failure_word(error: &ak_net::ConnectError) -> &'static str {
    match error {
        ak_net::ConnectError::PeerIdentity(_) => "peer certificate rejected",
        _ => "handshake transport failure",
    }
}

/// CR-P3-02 (review 2026-11-11) pre-Accept name preflight: the readable
/// refusal reason when this receiver runs on Windows and the offered root
/// name cannot exist on a Windows filesystem — one of `< > " | ? *`, or an
/// extended reserved device stem (`CONIN$`, `CONOUT$`, `CLOCK$`,
/// superscript `COM¹`–`COM³`/`LPT¹`–`LPT³`; the exact spelling list lives
/// in `ak_core::manifest::windows_incompatible`). `None` on every other
/// platform — the platform-neutral path rules already ran at decode time
/// and a Linux→Linux transfer may legitimately carry these names — and
/// for names Windows accepts.
///
/// Honest scope: the Offer's root name is the ONE payload name visible
/// before the wire Accept. The full manifest arrives only after Accept
/// inside ak-engine, so per-entry gating of every manifest row is an
/// engine-side follow-up; this gate refuses the top-level name before
/// the offer is ever surfaced.
pub fn windows_root_name_refusal(root_name: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    ak_core::manifest::windows_incompatible(root_name)
        .map(|issue| format!("cannot save {root_name:?} on Windows: {issue}"))
}

/// Single-line rendering of an engine error (multi-line Display output
/// would garble the aggregate message).
pub fn one_line_reason(error: &EngineError) -> String {
    let text = error.to_string();
    let first = text.split(['\n', '\r']).next().unwrap_or("");
    first.chars().take(120).collect()
}

// ---------------------------------------------------------------------------
// The receiver's pre-QUIC socket lifecycle (CR-P1-02 改法第 1/2 条).
// ---------------------------------------------------------------------------

/// The receiver's pre-QUIC bind outcome (CR-P1-02 改法第 2 条): the UDP
/// socket QUIC will adopt, plus the diagnostics of the two optional steps
/// that ran on it. Kept as one struct so the adapter's bind report (event
/// or stdout line) shows a single coherent snapshot.
pub struct ReceiverSocket {
    /// The bound socket — handed to QUIC unchanged via
    /// `DirectEndpoint::bind_with_socket`, so any NAT mapping the STUN
    /// query created stays valid for the QUIC listener.
    pub socket: std::net::UdpSocket,
    /// The STUN server-reflexive address observed ON `socket` (the
    /// address a peer on the far side of the NAT must dial), or `None`
    /// when no server was configured or the query failed (fail-open).
    pub reflexive: Option<SocketAddr>,
    /// Whether `socket` is the dual-stack `[::]` bind (C1, 改法第 1 条).
    /// Tracked HERE, not read back from the endpoint: adopted-socket
    /// endpoints report `is_dual_stack() == false` by contract (ak-net
    /// cannot portably re-query `IPV6_V6ONLY`), while dialing keys off
    /// the socket family and works either way.
    pub dual_stack: bool,
    /// Bounded reason the dual-stack bind fell back to v4 (C1 pattern).
    pub bind_fallback: Option<String>,
    /// Bounded reason the STUN query failed (`None` on success or when
    /// no server was configured — `reflexive` distinguishes those).
    pub stun_error: Option<String>,
}

/// Create the receiver's UDP socket and — when a STUN server is
/// configured — query its server-reflexive address ON THAT SAME SOCKET
/// before the socket is handed to QUIC (CR-P1-02 改法第 2 条: the mapping
/// the query opens on the NAT is exactly the mapping the QUIC listener
/// needs, so the query must not run on a throwaway socket).
///
/// Socket lifecycle, in order:
///
/// 1. `holepunch::bind_dual_stack_udp(0)` — C1's dual-stack `[::]`
///    wildcard (V6ONLY cleared, non-blocking armed). A host without IPv6
///    (or an unclearable V6ONLY) falls back to the historical v4
///    wildcard bind — C1's fallback chain, preserved verbatim.
/// 2. Optional STUN: the std socket is wrapped in a tokio `UdpSocket`
///    (same OS socket, so no new mapping), a v4 server address is sent
///    in `::ffff:` mapped form from the v6-family socket (the honest
///    form an AF_INET6 socket can dial; the NAT mapping this creates is
///    the one THIS socket keeps — a symmetric NAT may still map
///    destination-dependently, the known 改法第 4 条 residual),
///    `ak_signal::stun::query_on` runs under [`STUN_QUERY_BUDGET`], and
///    the std socket is recovered with `into_std` for QUIC.
/// 3. The caller adopts the socket with
///    `DirectEndpoint::bind_with_socket` (which re-arms non-blocking —
///    tokio round-trips never leave it in a state QUIC rejects).
///
/// Fail-open by design: a STUN failure (unreachable, silent, malformed)
/// costs at most [`STUN_QUERY_BUDGET`] and leaves the socket + local
/// candidates fully working; the bounded reason rides the diagnostics.
pub async fn bind_receiver_socket(stun: Option<SocketAddr>) -> Result<ReceiverSocket, String> {
    // Step 1 — C1's chain: dual-stack first, historical v4 fallback.
    let (mut socket, dual_stack, bind_fallback) = match ak_net::holepunch::bind_dual_stack_udp(0) {
        Ok(socket) => (socket, true, None),
        Err(reason) => (
            std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
                .map_err(|error| format!("receiver socket bind failed: {error}"))?,
            false,
            Some(reason.to_string()),
        ),
    };
    // Step 2 — the reflexive query on the SAME socket. `query_on` owns
    // its retransmission schedule; STUN_QUERY_BUDGET bounds the whole
    // exchange (see the const's rationale). The std socket MOVES into
    // the tokio wrapper and back via `into_std` — the same OS socket
    // handle throughout, so the query's send/recv map exactly the socket
    // QUIC is about to own.
    let Some(server) = stun else {
        // Not configured: no query, fail-open.
        return Ok(ReceiverSocket {
            socket,
            reflexive: None,
            dual_stack,
            bind_fallback,
            stun_error: None,
        });
    };
    if server.ip().is_unspecified() {
        // A configured-but-meaningless server (0.0.0.0): treat as absent.
        return Ok(ReceiverSocket {
            socket,
            reflexive: None,
            dual_stack,
            bind_fallback,
            stun_error: None,
        });
    }
    if let Err(error) = socket.set_nonblocking(true) {
        // The dual-stack path already armed this; only the v4 fallback
        // could refuse. Fail-open with the bounded reason.
        return Ok(ReceiverSocket {
            socket,
            reflexive: None,
            dual_stack,
            bind_fallback,
            stun_error: Some(format!("socket not queryable: {error}")),
        });
    }
    let tokio_socket = tokio::net::UdpSocket::from_std(socket)
        .map_err(|error| format!("receiver socket registration failed: {error}"))?;
    // The v4 server must ride the mapped form on the v6-family socket
    // (the ONLY v4 shape an AF_INET6 socket can send to; the reply's
    // XOR-MAPPED address comes back in plain v4 either way).
    let server_for_family = match (tokio_socket.local_addr(), server) {
        (Ok(local), _) if local.is_ipv6() && server.is_ipv4() => {
            ak_net::classify::to_mapped(server)
        }
        _ => server,
    };
    let outcome =
        ak_signal::stun::query_on(&tokio_socket, server_for_family, STUN_QUERY_BUDGET).await;
    socket = tokio_socket
        .into_std()
        .map_err(|error| format!("receiver socket recovery failed: {error}"))?;
    let (reflexive, stun_error) = match outcome {
        Ok(reflexive) => (Some(ak_net::classify::expand_mapped(reflexive)), None),
        // Fail-open: bounded reason only (StunError's vocabulary is
        // bounded; io errors carry the OS text like C1's fallback_reason
        // does).
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(ReceiverSocket {
        socket,
        reflexive,
        dual_stack,
        bind_fallback,
        stun_error,
    })
}

// ---------------------------------------------------------------------------
// The probe echo responder (wave-29 P2-12 + CR-P1-02 改法第 3 条).
// ---------------------------------------------------------------------------

/// Errors that only mean "this one datagram is gone", not "the responder
/// must stop" (the same set ak-net's probe tolerates; its helper is
/// private). On Windows, SIO_UDP_CONNRESET turns a late ICMP
/// port-unreachable for one of our REPLIES into ConnectionReset on the
/// NEXT recv — and a probe socket closes the moment its window ends
/// ([`probe_with_retry`], 改法第 3 条's [`probe_dial_round`]), so a reply
/// racing that close dooms the next recv. Returning there would kill the
/// responder between the pairing-time probe round and the send-time one
/// — the send-time round then honestly observes nothing — so the loop
/// skips these and keeps serving.
pub fn responder_error_is_transient(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    )
}

/// Bind the probe echo responder's UDP socket: dual-stack first
/// (CR-P1-02 改法第 1 条 — the responder must answer probes aimed at the
/// ADVERTISED IPv6 candidates too, and `bind_dual_stack_udp` already arms
/// non-blocking mode); a host without a clearable V6ONLY falls back to
/// the historical v4 wildcard socket. The caller spawns
/// [`echo_responder_loop`] on the returned socket and registers the task
/// in its own teardown table.
pub fn bind_echo_responder_socket() -> Result<(tokio::net::UdpSocket, SocketAddr), String> {
    let std_socket = match ak_net::holepunch::bind_dual_stack_udp(0) {
        Ok(socket) => socket,
        Err(_) => std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|error| format!("cannot bind the probe responder: {error}"))?,
    };
    std_socket
        .set_nonblocking(true)
        .map_err(|error| format!("cannot arm the probe responder: {error}"))?;
    let addr = std_socket
        .local_addr()
        .map_err(|error| format!("probe responder bind address: {error}"))?;
    let socket = tokio::net::UdpSocket::from_std(std_socket)
        .map_err(|error| format!("cannot register the probe responder: {error}"))?;
    Ok((socket, addr))
}

/// The echo responder body: answer ak-net hole-punch probe packets from
/// any advertised address with the all-zero nonce (never answered itself),
/// under a per-source reply budget per window. Since CR-P1-02 改法第 3
/// 条 the loop also OBSERVES: every genuine probe's source is remembered
/// in the shared `observed` slot (most recent, plain-family form) for the
/// adapter's close-notify dial, and `on_probe_source` fires for each
/// newly remembered observation — the CLI adapter prints its bounded
/// "probe observed from …" line there (the print site stays out of this
/// module), the FFI adapter passes a no-op.
///
/// Anti-reflection (wave-29 review P2-12): replies carry the fixed
/// all-zero nonce and a reply is NEVER itself answered, so two reachable
/// responders cannot loop answering each other (a real prober's nonce is
/// time/counter-derived and never all-zero); each source endpoint
/// additionally gets [`ECHO_REPLY_BUDGET`] replies per
/// [`ECHO_REPLY_WINDOW`] (a genuine probe round sends ~7 packets/s), and
/// the source table is hard-capped, so even a spoofed flood cannot
/// sustain unbounded reply traffic or memory growth.
pub async fn echo_responder_loop<F>(
    socket: tokio::net::UdpSocket,
    observed: Arc<StdMutex<Option<SocketAddr>>>,
    mut on_probe_source: F,
) where
    F: FnMut(SocketAddr),
{
    let mut buffer = [0u8; 1500];
    let mut budgets: HashMap<SocketAddr, (Instant, u32)> = HashMap::new();
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((len, source)) => {
                if !ak_net::holepunch::is_probe_packet(&buffer[..len]) {
                    continue;
                }
                // A reply (all-zero nonce) is never answered again — the
                // loop-killer. A genuine probe's nonce is
                // time/counter-derived and cannot be all-zero.
                if buffer[8..len] == ECHO_REPLY_NONCE {
                    continue;
                }
                // 改法第 3 条: remember the OBSERVED source (one bounded
                // store; poisoned locks lose the observation, never the
                // reply).
                let source_plain = ak_net::classify::expand_mapped(source);
                if let Ok(mut slot) = observed.lock() {
                    *slot = Some(source_plain);
                }
                on_probe_source(source_plain);
                let now = Instant::now();
                let allowed = match budgets.get_mut(&source) {
                    Some(entry) => {
                        if now.duration_since(entry.0) >= ECHO_REPLY_WINDOW {
                            *entry = (now, 1);
                            true
                        } else if entry.1 < ECHO_REPLY_BUDGET {
                            entry.1 += 1;
                            true
                        } else {
                            false
                        }
                    }
                    None => {
                        if budgets.len() >= ECHO_SOURCES_MAX {
                            budgets.clear();
                        }
                        budgets.insert(source, (now, 1));
                        true
                    }
                };
                if allowed {
                    let _ = socket
                        .send_to(&ak_net::holepunch::probe_packet(&ECHO_REPLY_NONCE), source)
                        .await;
                }
            }
            // Transient datagram loss (Windows turns a late ICMP
            // port-unreachable for one of our REPLIES — the probe socket
            // closed at its window end — into ConnectionReset on the
            // NEXT recv; ak-net's probe tolerates the same set) keeps the
            // responder alive for the NEXT probe round; only a hard
            // socket error ends the loop.
            Err(error) if responder_error_is_transient(&error) => continue,
            Err(_) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// The send-time simultaneous-probe round (CR-P1-02 改法第 3 条).
// ---------------------------------------------------------------------------

/// The send-time simultaneous-probe round's outcome for the dial loop
/// (CR-P1-02 改法第 3 条).
pub struct ProbeDialOrder {
    /// The full dial order: the observed head (when any) first, then the
    /// static LAN-first order, deduped ([`observed_first_order`]).
    pub order: Vec<SocketAddr>,
    /// The observed address leading `order` (always its first entry when
    /// present) — dialed under [`OBSERVED_DIAL_BUDGET`] instead of
    /// [`TASK_CONNECT_BUDGET`] (see the constant's rationale).
    pub head: Option<SocketAddr>,
}

/// One simultaneous-probe round (CR-P1-02 改法第 3 条) at the START of
/// the sender's connect phase: ONE dual-stack UDP socket
/// (`bind_dual_stack_udp`, v4 wildcard fallback for hosts without a
/// clearable V6ONLY) fires `holepunch::probe` packets at EVERY dial
/// candidate for [`PROBE_DIAL_WINDOW`]. The paired receiver's echo
/// responder answers on its bound socket (its reply IS a probe packet,
/// so the round observes the answering candidate), and a peer probe
/// round still overlapping from the pairing lands here as an observed
/// source the candidate list never named. The dial plan then puts the
/// observed address first ([`observed_dial_head`]); the socket itself is
/// dropped afterwards — the engine's `SendSpec` cannot adopt a
/// pre-bound socket (the recorded ak-net/ak-engine API wish), so the
/// probe socket's NAT mapping does NOT carry into the QUIC dial and the
/// observed-first leg buys its reachability proof, not port reuse.
///
/// `observe` fires once with the round's outcome — `(head, report,
/// probed)` — so the adapter can report what the round saw (the CLI
/// prints its bounded line there; the FFI passes a no-op).
///
/// Strictly additive and fail-open: a bind or probe error (and an empty
/// observation) returns the static order untouched — never worse than
/// the pre-C3 dialing.
pub async fn probe_dial_round<F>(ordered: Vec<SocketAddr>, observe: F) -> ProbeDialOrder
where
    F: FnOnce(Option<SocketAddr>, &ak_net::holepunch::PunchReport, usize),
{
    let probed = ordered.len();
    let std_socket = match ak_net::holepunch::bind_dual_stack_udp(0) {
        Ok(socket) => socket,
        Err(_) => match std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))) {
            Ok(socket) => socket,
            Err(_) => {
                return ProbeDialOrder {
                    order: ordered,
                    head: None,
                };
            }
        },
    };
    if std_socket.set_nonblocking(true).is_err() {
        return ProbeDialOrder {
            order: ordered,
            head: None,
        };
    }
    let Ok(socket) = tokio::net::UdpSocket::from_std(std_socket) else {
        return ProbeDialOrder {
            order: ordered,
            head: None,
        };
    };
    let report =
        match ak_net::holepunch::probe(&socket, &ordered, PROBE_DIAL_INTERVAL, PROBE_DIAL_WINDOW)
            .await
        {
            Ok(report) => report,
            Err(_) => {
                return ProbeDialOrder {
                    order: ordered,
                    head: None,
                };
            }
        };
    let head = observed_dial_head(&ordered, &report);
    observe(head, &report, probed);
    let order = observed_first_order(ordered, head);
    ProbeDialOrder { order, head }
}

/// Which observed address leads the dial order after one simultaneous
/// probe round (CR-P1-02 改法第 3 条) — pure on purpose (tests pin the
/// rule without any socket). Rules, in order:
///
/// 1. The FIRST candidate of the static LAN-first order that the round
///    saw answering (`report.reachable`): the route to that candidate is
///    confirmed bidirectional and the local NAT mapping toward it is hot
///    — the "dial the observed reply source first" leg. Reachable
///    entries outside the static list are ignored here (they name the
///    peer's ephemeral probe socket, not an advertised endpoint) and
///    fall through to rule 2 verbatim.
/// 2. The raw first source a PEER PROBE was observed from
///    (`report.peer_observed_from` — on the pairing overlap this is the
///    peer's own probe round arriving from an address the candidate list
///    never named, e.g. its NAT mapping), normalized from `::ffff:`
///    mapped form; an unspecified observation (a broken reflection) is
///    dropped.
///
/// `None` when the round observed nothing — the dial loop keeps the
/// static order exactly as before (strictly additive fallback).
pub fn observed_dial_head(
    ordered: &[SocketAddr],
    report: &ak_net::holepunch::PunchReport,
) -> Option<SocketAddr> {
    if let Some(hit) = ordered
        .iter()
        .find(|candidate| report.reachable.contains(candidate))
    {
        return Some(*hit);
    }
    report
        .peer_observed_from
        .map(ak_net::classify::expand_mapped)
        .filter(|addr| !addr.ip().is_unspecified())
}

/// Prepend one observed address to a dial order, deduped — the shared
/// merge of the sender's probe-informed dialing and the receiver side's
/// observed-source close notify: the observation leads, every static
/// entry survives in its existing order, `None` changes nothing.
pub fn observed_first_order(
    ordered: Vec<SocketAddr>,
    observed: Option<SocketAddr>,
) -> Vec<SocketAddr> {
    let Some(observed) = observed else {
        return ordered;
    };
    let observed = ak_net::classify::expand_mapped(observed);
    let mut out = Vec::with_capacity(ordered.len() + 1);
    out.push(observed);
    out.extend(ordered.into_iter().filter(|addr| *addr != observed));
    out
}

// ---------------------------------------------------------------------------
// The split connect budget (wave-25/26 P1-01 shape).
// ---------------------------------------------------------------------------

/// Connect-phase bookkeeping: the engine invokes its progress callback
/// synchronously inside the polled future, so the marks carry no
/// cross-task race.
///
/// CR-P2-11 move note: the ak-cli copy additionally tracked whether the
/// Offer ever went out (`offered`, its retryable-close seam); the shared
/// struct keeps that flag — the ak-ffi adapter merely never reads it (an
/// inert extra atomic, no observable behavior change).
pub struct ConnectMarks {
    connecting_at: StdMutex<Option<tokio::time::Instant>>,
    connecting: tokio::sync::Notify,
    connected: AtomicBool,
    /// Whether the Offer ever went out (the CLI's retryable-close seam).
    offered: AtomicBool,
}

impl ConnectMarks {
    /// Fresh marks; nothing observed yet.
    pub fn new() -> Self {
        Self {
            connecting_at: StdMutex::new(None),
            connecting: tokio::sync::Notify::new(),
            connected: AtomicBool::new(false),
            offered: AtomicBool::new(false),
        }
    }

    /// Record one engine progress fact (called synchronously from inside
    /// the polled send future).
    pub fn mark(&self, progress: &Progress) {
        match progress {
            Progress::Connecting => {
                if let Ok(mut slot) = self.connecting_at.lock() {
                    *slot = Some(tokio::time::Instant::now());
                }
                self.connecting.notify_one();
            }
            Progress::Connected { .. } => {
                self.connected.store(true, Ordering::Relaxed);
            }
            Progress::Offered { .. } | Progress::Accepted { .. } => {
                self.offered.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Whether the engine ever reported `Connected`.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Whether the engine ever reported `Offered`/`Accepted` (the CLI's
    /// retryable-close seam: a connection that died BEFORE any protocol
    /// exchange retries like a connect failure).
    pub fn offered(&self) -> bool {
        self.offered.load(Ordering::Relaxed)
    }
}

impl Default for ConnectMarks {
    fn default() -> Self {
        Self::new()
    }
}

/// The split-budget driver (P1-01): unbudgeted preparation, budgeted
/// bind/connect/handshake from the engine's own `Connecting` report,
/// unbudgeted transfer once connected.
///
/// The unit error is the adapters' moved shape verbatim — the caller
/// renders the bounded "timeout after Ns (no QUIC handshake)" reason from
/// its own budget constant, so the driver carries no error payload.
#[allow(clippy::result_unit_err)]
pub async fn drive_with_connect_budget<S>(
    marks: &ConnectMarks,
    mut send: std::pin::Pin<&mut S>,
    budget: Duration,
) -> Result<<S as Future>::Output, ()>
where
    S: Future + ?Sized,
{
    let connect_started = tokio::select! {
        _ = marks.connecting.notified() => {
            marks
                .connecting_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .expect("Connecting instant is recorded before notify")
        }
        done = send.as_mut() => return Ok(done),
    };
    match tokio::time::timeout_at(connect_started + budget, send.as_mut()).await {
        Ok(done) => Ok(done),
        Err(_deadline_elapsed) => {
            if marks.connected.load(Ordering::Relaxed) {
                Ok(send.await)
            } else {
                Err(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The idle close notification's dial core (wave-29 P2-03 shape).
// ---------------------------------------------------------------------------

/// Dial the paired peer's advertised data candidates (LAN-first,
/// family-matched, TLS pinned to the pairing's verified fingerprint — the
/// peer's receiver admission accepts exactly that key) and deliver ONE
/// `ErrorCode::Cancelled` + [`TASK_CLOSE_MESSAGE`] Error frame. The
/// peer's resident receiver recognizes the marker, ends its pairing
/// ByPeer and stops; an unreachable peer is an honest miss (its next
/// send fails to connect), never a failed disconnect.
///
/// CR-P1-02 改法第 3 条, the RECEIVER side's consumption of its observed
/// probe sources: `observed` is the most recent address the echo
/// responder OBSERVED a peer probe from (the raw hole-punch observation —
/// a NAT-mapped source the advertised list never named), and it leads the
/// dial order here: the close-notify dial is the receiver's one outbound
/// role, and the observed source is exactly the address an idle close
/// behind a NAT should try first. Best effort, so a stale observation
/// costs one bounded failed attempt before the static list, nothing more.
pub async fn deliver_close_notify(
    key: &DeviceKeyPair,
    candidates: Vec<SocketAddr>,
    pinned: Fingerprint,
    observed: Option<SocketAddr>,
) {
    let identity = make_self_signed(key);
    let candidates = order_candidates(candidates);
    let candidates = observed_first_order(
        candidates,
        observed.filter(|addr| !addr.ip().is_unspecified()),
    );
    let mut remaining = TASK_CLOSE_TOTAL_BUDGET;
    for addr in candidates {
        if remaining.is_zero() {
            return;
        }
        let bind = if addr.is_ipv6() {
            SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::from(([0, 0, 0, 0], 0))
        };
        let Ok(endpoint) = ak_net::DirectEndpoint::bind(bind, &identity).await else {
            return;
        };
        let dial = tokio::time::timeout(
            TASK_CLOSE_CONNECT_BUDGET.min(remaining),
            endpoint.connect(addr, &pinned),
        )
        .await;
        remaining = remaining.saturating_sub(TASK_CLOSE_CONNECT_BUDGET);
        let Ok(Ok(channel)) = dial else {
            continue; // candidate down: try the next
        };
        if let Ok(mut stream) = channel.open_bi().await {
            let frame = Frame {
                version: ak_wire::VERSION,
                message: Message::Error {
                    code: ErrorCode::Cancelled,
                    message: TASK_CLOSE_MESSAGE.to_string(),
                },
            };
            let _ = stream.send_frame(&frame).await;
            let _ = stream.finish_send();
        }
        channel.close(0, b"task closed");
        return; // one delivered notification is enough
    }
}

// ---------------------------------------------------------------------------
// Tests: the ONE set of unit tests for the shared candidate/reflexive/
// observed-order rules (previously maintained twice, FFI and CLI —
// CR-P2-11 collapsed the mirrors here) plus new unit pins for the pieces
// only product tests exercised before the move.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_order_lan_first_and_dedupe() {
        let lan: SocketAddr = "192.168.1.9:7000".parse().unwrap();
        let public: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:7000".parse().unwrap();
        // ak-net counts loopback as private, so loopback rides WITHIN the
        // LAN group (stable within equals: registration order).
        let ordered = order_candidates(vec![public, loopback, lan, lan]);
        assert_eq!(ordered, vec![loopback, lan, public]);
    }

    // -- candidate enumeration (CR-P1-02 改法第 1 条) -------------------------

    fn ips(texts: &[&str]) -> Vec<std::net::IpAddr> {
        texts
            .iter()
            .map(|text| text.parse().expect("test address"))
            .collect()
    }

    /// The documented selection order: v4 interface addresses first (input
    /// order, deduped), then 127.0.0.1, then global-unicast v6 (input
    /// order), then ::1. fe80::/10 drops out (undialable without a scope
    /// index), v4-mapped input normalizes into the v4 group, and a raw ::1
    /// never leaks into the middle of the v6 group.
    #[test]
    fn candidate_selection_keeps_both_families_in_documented_order() {
        let raw = ips(&[
            "2606:4700:4000::a",  // global v6, first interface
            "fe80::1",            // link-local: excluded
            "192.168.1.5",        // v4 interface
            "2606:4700:4000::b",  // same /64: collapsed away
            "::ffff:192.168.1.5", // v4-mapped: dedups with the v4
            "10.0.0.7",           // second v4 interface
            "2a00:1450:4001::81", // global v6, second prefix
            "::1",                // raw loopback: appended at tail
            "2001:db8::1",        // documentation range: excluded
            "fd00::42",           // ULA: excluded (not global)
            "ff02::1",            // multicast: excluded
        ]);
        assert_eq!(
            filter_and_order_candidates(raw),
            ips(&[
                "192.168.1.5",
                "10.0.0.7",
                "127.0.0.1",
                "2606:4700:4000::a",
                "2a00:1450:4001::81",
                "::1",
            ])
        );
    }

    /// One v6 candidate per /64 prefix (a NIC's stable + temporary GUAs
    /// collapse) and at most [`MAX_V6_IFACE_CANDIDATES`] overall — the
    /// blob stays comfortably under the wire bound on GUA-heavy hosts.
    #[test]
    fn candidate_selection_collapses_per_prefix_and_caps_global_v6() {
        let raw = ips(&[
            "2606:4700:4000::a",
            "2606:4700:4000::dead:beef", // same /64 as above
            "2a00:1450:4001::81",
            "2408:847a:11:2200::1",
            "2600:1f16:1a1:100::2",
            "2a01:4f8:1c17:77::3",
        ]);
        assert_eq!(
            filter_and_order_candidates(raw),
            ips(&[
                "127.0.0.1",
                "2606:4700:4000::a",
                "2a00:1450:4001::81",
                "2408:847a:11:2200::1",
                "::1",
            ])
        );
    }

    /// No interfaces at all: both loopbacks still give a same-host join a
    /// working candidate of each family (parity with the v4-only era,
    /// which always appended 127.0.0.1).
    #[test]
    fn candidate_selection_with_no_interfaces_keeps_both_loopbacks() {
        assert_eq!(
            filter_and_order_candidates(Vec::new()),
            ips(&["127.0.0.1", "::1"])
        );
    }

    /// The port mapping of the derived list: every candidate carries the
    /// bound port and both loopback families are present regardless of
    /// the host's interface set (v6 loopback is data, not a bind).
    #[test]
    fn local_candidates_map_ports_and_keep_both_loopbacks() {
        let locals = local_candidates(SocketAddr::from(([0, 0, 0, 0], 7100)));
        assert!(locals.contains(&"127.0.0.1:7100".parse::<SocketAddr>().unwrap()));
        assert!(locals.contains(&"[::1]:7100".parse::<SocketAddr>().unwrap()));
        assert!(locals.iter().all(|addr| addr.port() == 7100));
        assert!(
            locals.iter().all(|addr| addr.ip()
                != std::net::IpAddr::V6("fe80::1".parse::<std::net::Ipv6Addr>().unwrap())),
            "link-local never rides the candidate list"
        );
    }

    /// The blob caps and the mapped-normalization ordering in one place
    /// (the former CLI-side `candidate_helpers_match_the_ffi_shape` test,
    /// folded here when the mirrors collapsed — the two adapters now
    /// share one implementation).
    #[test]
    fn order_and_blob_helpers_match_the_documented_shape() {
        // LAN-first ordering with dedupe and v4-mapped normalization.
        let ordered = order_candidates(vec![
            "8.8.8.8:1".parse().unwrap(),
            "[::ffff:192.168.1.5]:2".parse().unwrap(),
            "192.168.1.5:2".parse().unwrap(),
            "127.0.0.1:3".parse().unwrap(),
        ]);
        assert_eq!(
            ordered,
            vec![
                "192.168.1.5:2".parse::<SocketAddr>().unwrap(),
                "127.0.0.1:3".parse().unwrap(),
                "8.8.8.8:1".parse().unwrap(),
            ]
        );
        // The blob caps dial candidates at 8 and echo candidates at 4.
        let dial: Vec<SocketAddr> = (1..=12)
            .map(|i| SocketAddr::from(([192, 168, 0, i], 7000)))
            .collect();
        let blob = blob_candidates(dial, Vec::new(), Vec::new(), "");
        assert_eq!(blob.len(), MAX_DIAL_CANDIDATES);
        // Caller extras ride verbatim between the dial block and the echo
        // block (the FFI adapter's parameterization).
        let blob = blob_candidates(
            vec!["192.168.1.5:7000".parse().unwrap()],
            vec!["203.0.113.7:7000".parse().unwrap()],
            vec!["192.168.1.5:7001".parse().unwrap()],
            "",
        );
        assert_eq!(
            blob,
            vec![
                "192.168.1.5:7000".parse::<SocketAddr>().unwrap(),
                "203.0.113.7:7000".parse().unwrap(),
                "192.168.1.5:7001".parse().unwrap(),
            ]
        );
    }

    /// The belt-and-braces shrink loop: a v6-heavy mix that passes the
    /// candidate caps but exceeds the 512-byte wire bound pops its tail
    /// until the blob encodes (the head — the registering side's primary
    /// candidates — survives). The uncapped `extras` leg is what can push
    /// the list past the bound (the dial/echo caps alone cannot).
    #[test]
    fn blob_candidates_shrinks_to_the_wire_bound() {
        let long_v6 = |i: u16| {
            SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::from([
                    0x2606,
                    0x4700 + i,
                    i,
                    i,
                    i,
                    i,
                    i,
                    i,
                ])),
                40_000 + i,
            )
        };
        // 8 dial + 8 uncapped extras = 16 full-length v6 candidates
        // (~49 bytes each encoded) — past the 512-byte bound.
        let dial: Vec<SocketAddr> = (1..=8u16).map(long_v6).collect();
        let extras: Vec<SocketAddr> = (9..=16u16).map(long_v6).collect();
        let blob = blob_candidates(dial, extras, Vec::new(), "");
        assert!(blob.len() > 1 && blob.len() < 16, "tail popped: {blob:?}");
        let endpoint = PeerEndpoint {
            candidates: blob,
            name: String::new(),
            direction: TaskDirection::Send,
        };
        assert!(encode_endpoint_blob(&endpoint).is_ok());
    }

    // -- reflexive-candidate insertion (CR-P1-02 改法第 2 条) ---------------

    fn addrs(texts: &[&str]) -> Vec<SocketAddr> {
        texts
            .iter()
            .map(|text| text.parse().expect("test address"))
            .collect()
    }

    /// The documented position: right after the v4 interface block, before
    /// the loopback tail and the interface-IPv6 block; `None` (no STUN
    /// server configured) leaves the derived list untouched.
    #[test]
    fn reflexive_candidate_inserts_after_the_v4_interface_block() {
        let derived = addrs(&[
            "192.168.1.5:7000",
            "10.0.0.7:7000",
            "127.0.0.1:7000",
            "[2606:4700:4000::a]:7000",
            "[::1]:7000",
        ]);
        assert_eq!(
            insert_reflexive_candidate(derived.clone(), Some("203.0.113.7:7000".parse().unwrap())),
            addrs(&[
                "192.168.1.5:7000",
                "10.0.0.7:7000",
                "203.0.113.7:7000",
                "127.0.0.1:7000",
                "[2606:4700:4000::a]:7000",
                "[::1]:7000",
            ])
        );
        assert_eq!(insert_reflexive_candidate(derived.clone(), None), derived);
        // No v4 interface at all: the reflexive address still takes the
        // reserved slot (ahead of the loopback tail; the consumer's
        // LAN-first re-sort keeps loopback first at dial time).
        assert_eq!(
            insert_reflexive_candidate(
                addrs(&["127.0.0.1:7000", "[::1]:7000"]),
                Some("203.0.113.7:7000".parse().unwrap())
            ),
            addrs(&["203.0.113.7:7000", "127.0.0.1:7000", "[::1]:7000"])
        );
    }

    /// Dedup: a reflexive address that equals an existing candidate (no
    /// NAT — the loopback-STUN same-socket case, or the public IP already
    /// on an interface) adds nothing; a v4-mapped reflexive normalizes to
    /// plain IPv4 before the comparison.
    #[test]
    fn reflexive_candidate_dedups_against_existing_entries() {
        let derived = addrs(&["192.168.1.5:7000", "127.0.0.1:7000", "[::1]:7000"]);
        // The reflected loopback (same socket, same port): nothing new.
        assert_eq!(
            insert_reflexive_candidate(derived.clone(), Some("127.0.0.1:7000".parse().unwrap())),
            derived
        );
        // The public IP already sits on an interface (1:1 NAT / cloud VM):
        // nothing new.
        assert_eq!(
            insert_reflexive_candidate(derived.clone(), Some("192.168.1.5:7000".parse().unwrap())),
            derived
        );
        // v4-mapped reflexive form normalizes before dedup and insertion.
        assert_eq!(
            insert_reflexive_candidate(derived, Some("[::ffff:203.0.113.7]:7000".parse().unwrap())),
            addrs(&[
                "192.168.1.5:7000",
                "203.0.113.7:7000",
                "127.0.0.1:7000",
                "[::1]:7000",
            ])
        );
    }

    /// A broken server reflecting an unspecified address adds nothing (the
    /// probe would skip it as undialable — fail-open, no junk candidate).
    #[test]
    fn reflexive_candidate_drops_unspecified_reflections() {
        let derived = addrs(&["192.168.1.5:7000", "127.0.0.1:7000"]);
        assert_eq!(
            insert_reflexive_candidate(derived.clone(), Some("0.0.0.0:7000".parse().unwrap())),
            derived
        );
        assert_eq!(
            insert_reflexive_candidate(derived, Some("[::]:7000".parse().unwrap())),
            addrs(&["192.168.1.5:7000", "127.0.0.1:7000"])
        );
    }

    /// Edge of the insertion slot: an empty derived list degenerates to
    /// the reflexive address alone (the `unwrap_or(out.len())` tail).
    #[test]
    fn reflexive_candidate_into_an_empty_list_leads_it() {
        assert_eq!(
            insert_reflexive_candidate(Vec::new(), Some("203.0.113.7:7000".parse().unwrap())),
            addrs(&["203.0.113.7:7000"])
        );
    }

    // -- observed-address dial order (CR-P1-02 改法第 3 条) -----------------

    fn punch_report(
        reachable: &[&str],
        peer_observed_from: Option<&str>,
    ) -> ak_net::holepunch::PunchReport {
        ak_net::holepunch::PunchReport {
            reachable: addrs(reachable),
            peer_observed_from: peer_observed_from.map(|text| text.parse().expect("observed")),
            packets_sent: 0,
            packets_received: 0,
            elapsed: Duration::ZERO,
        }
    }

    /// Rule (i): the observed head is the FIRST candidate of the static
    /// LAN-first order that answered the probe — arrival order in the
    /// report does not reorder the dial.
    #[test]
    fn observed_head_is_the_first_static_candidate_that_answered() {
        let ordered = addrs(&[
            "192.168.1.5:7000",
            "203.0.113.7:7000",
            "127.0.0.1:7000",
            "[::1]:7000",
        ]);
        // The later static candidate answered FIRST (arrival order) — the
        // head is still the earlier static entry that also answered.
        assert_eq!(
            observed_dial_head(
                &ordered,
                &punch_report(&["127.0.0.1:7000", "203.0.113.7:7000"], None)
            ),
            Some("203.0.113.7:7000".parse().unwrap())
        );
        // Reachable entries outside the static list (the peer's ephemeral
        // probe socket) never become the head by rule (i).
        assert_eq!(
            observed_dial_head(&ordered, &punch_report(&["198.51.100.9:40123"], None)),
            None
        );
    }

    /// Rule (ii): with no reachable candidate, the raw first peer-probe
    /// source leads — normalized from `::ffff:` mapped form (how a v4
    /// peer appears on the dual-stack probe socket); an unspecified
    /// observation is dropped (nothing observed).
    #[test]
    fn observed_head_falls_back_to_the_raw_peer_observation() {
        let ordered = addrs(&["192.168.1.5:7000", "127.0.0.1:7000"]);
        assert_eq!(
            observed_dial_head(
                &ordered,
                &punch_report(&[], Some("[::ffff:203.0.113.7]:41000"))
            ),
            Some("203.0.113.7:41000".parse().unwrap())
        );
        assert_eq!(
            observed_dial_head(&ordered, &punch_report(&[], Some("0.0.0.0:41000"))),
            None
        );
        assert_eq!(observed_dial_head(&ordered, &punch_report(&[], None)), None);
    }

    /// The merge: the observation leads the order, every static entry
    /// survives in its existing order, dedup keeps one occurrence, and
    /// `None` changes nothing (the strictly-additive fallback shape).
    #[test]
    fn observed_first_order_prepends_dedupes_and_preserves_the_static_order() {
        let ordered = addrs(&["192.168.1.5:7000", "127.0.0.1:7000", "[::1]:7000"]);
        assert_eq!(
            observed_first_order(ordered.clone(), Some("203.0.113.7:41000".parse().unwrap())),
            addrs(&[
                "203.0.113.7:41000",
                "192.168.1.5:7000",
                "127.0.0.1:7000",
                "[::1]:7000",
            ])
        );
        // An observation that IS a static entry moves to the front — one
        // occurrence, the rest keep their order.
        assert_eq!(
            observed_first_order(ordered.clone(), Some("127.0.0.1:7000".parse().unwrap())),
            addrs(&["127.0.0.1:7000", "192.168.1.5:7000", "[::1]:7000"])
        );
        // v4-mapped observation normalizes before the merge.
        assert_eq!(
            observed_first_order(
                ordered.clone(),
                Some("[::ffff:203.0.113.7]:41000".parse().unwrap())
            ),
            addrs(&[
                "203.0.113.7:41000",
                "192.168.1.5:7000",
                "127.0.0.1:7000",
                "[::1]:7000",
            ])
        );
        assert_eq!(observed_first_order(ordered.clone(), None), ordered);
    }

    // -- CR-P3-02: the Windows root-name preflight verdict ------------------

    /// CR-P3-02: the consent-time preflight is a verdict only on Windows —
    /// the platform-neutral rules stay the only gate elsewhere
    /// (Linux→Linux transfers may carry these names).
    #[test]
    fn windows_root_name_preflight_follows_the_local_platform() {
        if cfg!(windows) {
            for (bad, expected_char) in [("a?b.bin", '?'), ("star*name", '*')] {
                let reason = windows_root_name_refusal(bad)
                    .unwrap_or_else(|| panic!("must refuse {bad:?} on Windows"));
                assert!(
                    reason.contains(bad) && reason.contains(expected_char),
                    "reason must name the offender and the character: {reason}"
                );
            }
            for reserved in ["CONIN$.txt", "clock$", "com\u{b9}.dat", "LPT\u{b3}"] {
                assert!(
                    windows_root_name_refusal(reserved).is_some(),
                    "must refuse the extended reserved stem {reserved:?}"
                );
            }
            for ok in ["plain.bin", "multi", "设备照片.jpg"] {
                assert_eq!(
                    windows_root_name_refusal(ok),
                    None,
                    "must keep prompting the user for {ok:?}"
                );
            }
        } else {
            for linux_legal in ["a?b.bin", "CONIN$.txt", "clock$", "com\u{b9}.dat"] {
                assert_eq!(windows_root_name_refusal(linux_legal), None);
            }
        }
    }

    // -- CR-P2-11: unit pins for the pieces only product tests exercised
    //    before the move (new with the shared module, not mirrors). -------

    /// The bounded pairing-failure vocabulary, including the RATE_LIMITED
    /// arm the move unified on (the FFI behavior — see
    /// [`task_error_message`]'s divergence note).
    #[test]
    fn task_error_message_maps_the_bounded_vocabularies() {
        let exhausted = task_error_message(&TaskError::Flow(PairFlowError::Rejected {
            code: ak_signal::protocol_v2::message::REDEEM_EXHAUSTED,
        }));
        assert!(exhausted.contains("already joined"), "{exhausted}");
        let limited = task_error_message(&TaskError::Flow(PairFlowError::Rejected {
            code: ak_signal::protocol_v2::message::RATE_LIMITED,
        }));
        assert!(
            limited.contains("rate limited") && limited.contains("refills"),
            "{limited}"
        );
        let unavailable = task_error_message(&TaskError::Flow(PairFlowError::Unavailable {
            code: ak_signal::protocol_v2::message::PAIR_UNAVAILABLE,
        }));
        assert!(unavailable.contains("unknown task code"), "{unavailable}");
        let other = task_error_message(&TaskError::EndpointMissing);
        assert!(other.starts_with("registry task pairing failed"), "{other}");
    }

    /// The responder's transient-error set: datagram-loss kinds stay in
    /// the loop, everything else ends it.
    #[test]
    fn responder_error_is_transient_classifies_datagram_loss() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::TimedOut,
        ] {
            assert!(
                responder_error_is_transient(&std::io::Error::from(kind)),
                "{kind:?} is transient"
            );
        }
        for kind in [
            std::io::ErrorKind::AddrNotAvailable,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            assert!(
                !responder_error_is_transient(&std::io::Error::from(kind)),
                "{kind:?} is fatal"
            );
        }
    }

    /// The accept-failure vocabulary stays two fixed words (never the raw
    /// transport text a hostile peer controls).
    #[test]
    fn handshake_failure_word_uses_the_bounded_vocabulary() {
        assert_eq!(
            handshake_failure_word(&ak_net::ConnectError::PeerIdentity("bad pin".to_string())),
            "peer certificate rejected"
        );
        assert_eq!(
            handshake_failure_word(&ak_net::ConnectError::Unauthorized),
            "handshake transport failure"
        );
    }

    /// The receiver bind without a STUN server: no query, no failure, a
    /// genuinely bound socket (the fail-open no-server path, pinned at
    /// unit level — the product tests cover the with-server legs).
    #[tokio::test]
    async fn bind_receiver_socket_without_a_server_skips_the_query() {
        let sock = bind_receiver_socket(None).await.expect("binds");
        assert!(sock.reflexive.is_none(), "no server, no reflexive");
        assert!(sock.stun_error.is_none(), "no server, no failure");
        assert!(
            sock.socket.local_addr().is_ok(),
            "the socket is genuinely bound"
        );
    }

    /// The probe round is fail-open on a silent candidate list: the static
    /// order comes back untouched with no observed head, after one full
    /// window (the window IS the pre-dial budget).
    #[tokio::test]
    async fn probe_dial_round_fails_open_on_a_silent_candidate_list() {
        // A bound-but-silent UDP socket: packets land, nothing answers —
        // deterministic on every OS (the FFI STUN product test uses the
        // same trick).
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("silent bind");
        let candidate = SocketAddr::from((
            [127, 0, 0, 1],
            silent.local_addr().expect("silent addr").port(),
        ));
        let ordered = vec![candidate];
        let started = Instant::now();
        let plan = probe_dial_round(ordered.clone(), |_, _, _| {}).await;
        assert_eq!(plan.order, ordered, "fail-open keeps the static order");
        assert_eq!(plan.head, None, "nothing answered");
        assert!(
            started.elapsed() >= PROBE_DIAL_WINDOW - Duration::from_millis(100),
            "the round ran its window: {:?}",
            started.elapsed()
        );
    }

    /// The echo responder loop, unit level: a genuine probe is answered
    /// with the all-zero reply nonce, the observation slot is filled, and
    /// a reply is never itself answered (the reflection loop killer).
    #[tokio::test]
    async fn echo_loop_answers_probes_and_ignores_replies() {
        let observed = Arc::new(StdMutex::new(None));
        let (responder, bound) = bind_echo_responder_socket().expect("responder binds");
        let slot = Arc::clone(&observed);
        let task = tokio::spawn(async move {
            echo_responder_loop(responder, slot, |_| {}).await;
        });
        let dial = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        let probe_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("probe socket");

        probe_socket
            .send_to(&ak_net::holepunch::probe_packet(&[0xA5u8; 16]), dial)
            .await
            .expect("probe sent");
        let mut buffer = [0u8; 1500];
        let (len, from) = tokio::time::timeout(
            Duration::from_millis(500),
            probe_socket.recv_from(&mut buffer),
        )
        .await
        .expect("a reply arrives")
        .expect("reply");
        assert_eq!(len, 24, "the reply is a probe-shaped packet");
        assert_eq!(&buffer[8..len], &[0u8; 16][..], "zero reply nonce");
        assert_eq!(from, dial);
        assert!(
            observed.lock().expect("slot").is_some(),
            "the observation slot is filled"
        );

        // The reply itself is never answered (a reply is not a probe).
        probe_socket
            .send_to(&buffer[..len], dial)
            .await
            .expect("reply sent");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                probe_socket.recv_from(&mut buffer)
            )
            .await
            .is_err(),
            "a responder must never answer a reply"
        );
        task.abort();
    }

    /// The split-budget driver's early arm: a future that completes
    /// without ever reporting `Connecting` returns unbudgeted.
    #[tokio::test]
    async fn drive_returns_a_future_that_finishes_before_connecting() {
        let marks = ConnectMarks::new();
        let send = async { 7u32 };
        tokio::pin!(send);
        let out = drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(1))
            .await
            .expect("completes unbudgeted");
        assert_eq!(out, 7);
        assert!(!marks.connected());
    }

    /// The split-budget driver's deadline arm: once `Connecting` is
    /// reported, a future that never connects exhausts the budget.
    #[tokio::test]
    async fn drive_enforces_the_budget_when_never_connected() {
        let marks = ConnectMarks::new();
        marks.mark(&Progress::Connecting);
        let send = std::future::pending::<()>();
        tokio::pin!(send);
        let outcome =
            drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(20)).await;
        assert!(outcome.is_err(), "the budget deadline is enforced");
    }
}
