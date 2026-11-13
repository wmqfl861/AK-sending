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
//! * the sender socket lifecycle (CR-P1-05, review 2026-11-11 §4.5) —
//!   [`task_net::SenderSocket`] / [`task_net::bind_sender_socket`], the exact
//!   mirror of the receiver bind for the DIAL side: the reflexive address
//!   rides the sender's OWN endpoint blob and the socket is adopted by
//!   the engine (`ak_engine::SendSpec::adopt_socket` +
//!   `DirectEndpoint::dial_with_socket`) so the QUIC Initial leaves from
//!   the very port the STUN query mapped;
//! * the receiver-side hole punch (CR-P1-05) — [`task_net::punch_candidates`]
//!   with the [`task_net::PUNCH_INTERVAL`] / [`task_net::PUNCH_WINDOW`] /
//!   [`task_net::PUNCH_MAX_TARGETS`] tunables: send-only probe-shaped packets
//!   from a `try_clone` of the QUIC listener socket to every advertised
//!   sender candidate, concurrently with the sender's dial, stopped
//!   early by the first successful data-plane connection. Since CR-P1-06
//!   (review 2026-11-11 §4.5 + the batchF conntrack evidence) the FIRST
//!   phase of the punch sends its IPv4 datagrams with the low
//!   [`task_net::PUNCH_LOW_TTL`] — per-packet via
//!   `ak_net::holepunch::send_to_with_ttl`, never by touching the shared
//!   socket's own TTL — so they open the local NAT mapping but expire
//!   before the peer's NAT and leave no conflict record there;
//! * the sender's post-pairing reflexive opener + the shared dial pass
//!   (CR-P1-06 改法 2 + CR-P3-09) —
//!   [`task_net::sender_reflexive_opener`] (brief pause after pairing, then
//!   the adopted socket's first outbound packets toward the receiver's
//!   reflexive addresses) and [`task_net::staggered_dial_pass`] /
//!   [`task_net::reflexive_first_order`] (Happy-Eyeballs interleaved dialing
//!   with reflexive/observed addresses strictly first; sequential on the
//!   adopted socket, where concurrent QUIC endpoints on handles of one OS
//!   socket would compete for its receive queue);
//! * the pairing-time opener + mapping keepalive (CR-P1-07, review
//!   2026-11-13 §4.4 改法 1) —
//!   [`task_net::sender_pairing_opener`]: the SAME opener timing the CLI
//!   always had, now run at PAIRING COMPLETION by both adapters (the GUI's
//!   "pair, then pick files, then send" latency no longer outlives the
//!   receiver's punch window), followed by probe-shaped keepalive packets
//!   every [`task_net::OPENER_KEEPALIVE_INTERVAL`] until the send starts /
//!   the connection ends / [`task_net::OPENER_KEEPALIVE_BUDGET`] elapses —
//!   one shared function, so the CLI matrix exercises the exact code the
//!   GUI runs;
//! * the Windows receiver's low-TTL punch (CR-P1-08, review 2026-11-13
//!   §4.4 改法 3) — the private `SocketTtlGuard` (the socket-level
//!   `setsockopt(IP_TTL)` fallback for hosts without
//!   per-packet control data, armed only while no QUIC connection exists
//!   on the socket, restored at the phase end) and the delayed punch
//!   start of [`task_net::punch_candidates`] when NO low-TTL send exists
//!   at all (the sender's own low-TTL opener then owns the pre-punch
//!   window — 改法 3b's ordering without a wire-format capability bit);
//! * the probe echo responder — [`task_net::bind_echo_responder_socket`] +
//!   [`task_net::echo_responder_loop`] (anti-reflection, per-source reply budgets,
//!   the CR-P1-02 leg-3 observation slot) and the transient-error
//!   tolerance helper [`task_net::responder_error_is_transient`];
//! * the send-time simultaneous-probe round — [`task_net::probe_dial_round`]
//!   (throwaway socket, the C3 shape) and [`task_net::probe_dial_round_on`]
//!   (CR-P1-05: the round ON the adopted sender socket's clone) with
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Idle budget of a task-plane send (both adapters set the send AND the
/// receive spec's idle budget to it): must comfortably cover the
/// receiver's pending-consent window (the sender's offer waits while the
/// peer's user decides) plus slow-storage source scans on the far end.
/// 150 s since acceptance E2/G4 (review 2026-11-12 §6): the consent
/// window rose 30 s → 120 s, and the budget must stay ≥ 150 s so the
/// window itself never races the idle cut (QUIC keep-alive holds the
/// connection; this budget is the engine-level frame-idle bound).
pub const TASK_SEND_IDLE: Duration = Duration::from_secs(150);

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
/// 150 ms burst on a live path. Since CR-P3-06 (G2) the round runs
/// `holepunch::probe_with_early_exit`: the first candidate of the
/// static order being echo-confirmed ends the round then and there (a
/// same-LAN send pays one probe round-trip, not the fixed second — the
/// recorded API wish, landed); only an unconfirmed head blocks out the
/// window. The round runs OUTSIDE the per-attempt connect budgets and
/// pass counting: [`TASK_CONNECT_BUDGET`] × [`TASK_CONNECT_PASSES`]
/// behave exactly as before, so the worst-case total connect phase
/// grows by at most window + [`OBSERVED_DIAL_BUDGET`] ≈ 3 s.
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

/// Cadence of the RECEIVER-side hole punch (CR-P1-05, 改法第 3 条's
/// receiver mirror: the receiver's QUIC socket must SEND to the paired
/// sender, so a port-restricted-cone NAT on the receiver side admits the
/// sender's QUIC Initial). The review's suggested 100–200 ms band; 150 ms
/// matches [`PROBE_DIAL_INTERVAL`] so the punch reads as a probe round
/// in packet captures. The punch packet IS an `holepunch::probe` packet
/// (time-derived nonce): the sender's echo responder answers it — the
/// reply opens the SENDER's inbound filter toward the receiver's QUIC
/// mapping, the mirror direction for free — and the sender's QUIC stack
/// (once the socket is adopted) silently discards what it cannot parse.
pub const PUNCH_INTERVAL: Duration = Duration::from_millis(150);

/// Wall-clock window of the receiver-side hole punch (the review's
/// suggested 3–5 s band; 4 s comfortably covers the sender's probe round
/// (1 s) plus the observed-first dial budget (2 s) while staying inside
/// one connect pass). Bounded by design: the loop is spawned beside the
/// accept path and never blocks it; the first successful data-plane
/// connection (or the task's end) stops it early via the shared flag.
pub const PUNCH_WINDOW: Duration = Duration::from_secs(4);

/// Per-tick target cap of the punch loop (the endpoint blob's wire bound
/// is 16 candidates; the derived lists stay ≤ 8 dial + 4 echo — this cap
/// bounds one tick's sends even against a hostile/buggy blob).
pub const PUNCH_MAX_TARGETS: usize = 8;

/// CR-P1-06 (review 2026-11-11 §4.5 改法 1): TTL of the receiver's
/// FIRST-phase punch datagrams toward IPv4 candidates. The review band is
/// 2–3; 2 is chosen deliberately: the datagram must traverse exactly the
/// puncher's OWN NAT (which creates the mapping the sender's QUIC Initial
/// then rides) and expire one hop further out, BEFORE the peer's NAT sees
/// it — a value of 3 still reaches (and pollutes the conntrack of) a
/// peer one intermediate routing hop away, which is exactly the matrix
/// topology (r1 → akinet → r2) and not unusual behind a shared
/// uplink. IPv4 only: IPv6 has no NAT and keeps the normal hop limit
/// (改法 3). CONFIGURABLE CONSTANT, not policy: carrier-grade/double NAT
/// needs a larger value so the punch clears BOTH local NATs — the real
/// number comes from the C4 real-network runs.
pub const PUNCH_LOW_TTL: u32 = 2;

/// Wall-clock length of the punch's low-TTL first phase (CR-P1-06 改法 1:
/// normal-TTL punching is retained as the backup for NATs that behave
/// differently — 改法 3). The phase must cover the sender's post-pairing
/// sequence — [`SENDER_DIAL_PAUSE`] (300 ms) plus the reflexive opener's
/// sends — so the sender's adopted socket has opened its outbound NAT
/// flow before any normal-TTL punch can arrive at the sender's NAT and
/// re-create the inbound-only conntrack record the low TTL just avoided.
/// 1 s = pause + 700 ms of skew margin for the two sides' pairing
/// completion signals.
pub const PUNCH_LOW_TTL_PHASE: Duration = Duration::from_secs(1);

/// CR-P1-06 (改法 2): the sender's pause after pairing (the review band
/// is 200–500 ms) before its FIRST outbound packet toward the receiver
/// from the adopted socket ([`sender_reflexive_opener`]): the receiver's
/// low-TTL punches must have left its socket first, so the punch datagrams
/// (not the sender's QUIC Initial) are what opens the receiver's NAT
/// mapping, and the sender's own outbound flow is established before the
/// receiver's normal-TTL backup phase begins.
pub const SENDER_DIAL_PAUSE: Duration = Duration::from_millis(300);

/// CR-P1-07 (review 2026-11-13 §4.4 改法 1): cadence of the pairing-time
/// mapping KEEPALIVE — after the opener, one probe-shaped packet per
/// public target leaves the adopted socket every 15 s until the send
/// starts, the connection/task ends, or [`OPENER_KEEPALIVE_BUDGET`]
/// elapses. 15 s sits at half the 30 s Linux conntrack UNREPLIED UDP
/// timeout (the shortest mapping lifetime a home router is likely to
/// enforce): both sides' entries — the sender's outbound flow and the
/// receiver's punched mapping, which a keepalive datagram matches in the
/// reverse direction, marking it REPLIED/assured (120 s on Linux-type
/// NATs) — are refreshed with a full interval of margin.
pub const OPENER_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// CR-P1-07: total wall-clock budget of the pairing-time keepalive. The
/// task-code TTL default is 10 minutes (wave-29 P2-04) — a paired-but-
/// never-sent connection keeps its NAT mapping for exactly as long as
/// the code that created it could still have served a transfer, then the
/// honest expiry is a later send failing to connect.
pub const OPENER_KEEPALIVE_BUDGET: Duration = Duration::from_secs(10 * 60);

/// Granularity at which bounded side loops poll their shared stop flag
/// between sleeps ([`sender_pairing_opener`]'s keepalive and
/// [`punch_candidates`]'s delayed start): a plain `sleep_until(interval)`
/// would only notice the flag at the next tick — up to a full
/// [`OPENER_KEEPALIVE_INTERVAL`] after the send already started. Four
/// idle wakeups per keepalive tick is nothing; a 250 ms stop latency is
/// invisible next to the send phase's own probe round.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// CR-P3-09 (review 2026-11-11 §4.5): Happy-Eyeballs stagger — one
/// connect attempt starts every 250 ms while the previous ones are still
/// in flight, instead of the old serial loop whose per-candidate
/// [`TASK_CONNECT_BUDGET`] (10 s) made connection time grow linearly with
/// the candidate count (the review measured ~10 s wasted on unreachable
/// private candidates before the reflexive address was ever dialed).
/// With reflexive/observed addresses first
/// ([`reflexive_first_order`]) a live reflexive leg connects in the first
/// slot regardless of how many candidates follow.
pub const DIAL_STAGGER: Duration = Duration::from_millis(250);

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
// The receiver's pre-QUIC socket lifecycle (CR-P1-02 改法第 1/2 条) and the
// sender's mirror (CR-P1-05, review 2026-11-11 §4.5: both sides probe on
// THE socket that will carry QUIC).
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

/// The SENDER's pre-QUIC bind outcome (CR-P1-05): the exact mirror shape
/// of [`ReceiverSocket`] — same lifecycle (dual-stack bind, optional
/// same-socket STUN under [`STUN_QUERY_BUDGET`], fail-open), different
/// consumer: the reflexive address rides the sender's OWN endpoint blob
/// (so the paired receiver's punch loop has a dialable target), and the
/// socket is handed to the engine's
/// [`ak_engine::SendSpec::adopt_socket`] /
/// `DirectEndpoint::dial_with_socket` so the QUIC Initial leaves from the
/// very port the STUN query and the send-time probe round mapped.
pub struct SenderSocket {
    /// The bound socket — the future-QUIC dial socket. The caller keeps
    /// it for the send phase (per-attempt `try_clone` hands each engine
    /// attempt its own handle of the SAME OS socket: same local port,
    /// same NAT mapping).
    pub socket: std::net::UdpSocket,
    /// The STUN server-reflexive address observed ON `socket`, or `None`
    /// when no server was configured or the query failed (fail-open).
    pub reflexive: Option<SocketAddr>,
    /// Whether `socket` is the dual-stack `[::]` bind (see
    /// [`ReceiverSocket::dual_stack`] — the flag is tracked, not queried).
    pub dual_stack: bool,
    /// Bounded reason the dual-stack bind fell back to v4.
    pub bind_fallback: Option<String>,
    /// Bounded reason the STUN query failed (`None` on success or when
    /// no server was configured).
    pub stun_error: Option<String>,
}

/// The shared socket lifecycle core of [`bind_receiver_socket`] and
/// [`bind_sender_socket`]: dual-stack `[::]` wildcard bind (v4 fallback),
/// the optional same-socket STUN reflexive query under
/// [`STUN_QUERY_BUDGET`], fail-open throughout. One core because the two
/// sides' lifecycles are byte-identical up to the consumer — the CR-P1-05
/// rule (both sides probe on THE socket that will carry QUIC) is exactly
/// this symmetry.
async fn bind_and_stun_socket(
    stun: Option<SocketAddr>,
) -> Result<
    (
        std::net::UdpSocket,
        Option<SocketAddr>,
        bool,
        Option<String>,
        Option<String>,
    ),
    String,
> {
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
        return Ok((socket, None, dual_stack, bind_fallback, None));
    };
    if server.ip().is_unspecified() {
        // A configured-but-meaningless server (0.0.0.0): treat as absent.
        return Ok((socket, None, dual_stack, bind_fallback, None));
    }
    if let Err(error) = socket.set_nonblocking(true) {
        // The dual-stack path already armed this; only the v4 fallback
        // could refuse. Fail-open with the bounded reason.
        return Ok((
            socket,
            None,
            dual_stack,
            bind_fallback,
            Some(format!("socket not queryable: {error}")),
        ));
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
    Ok((socket, reflexive, dual_stack, bind_fallback, stun_error))
}

/// Create the receiver's UDP socket and — when a STUN server is
/// configured — query its server-reflexive address ON THAT SAME SOCKET
/// before the socket is handed to QUIC (CR-P1-02 改法第 2 条: the mapping
/// the query opens on the NAT is exactly the mapping the QUIC listener
/// needs, so the query must not run on a throwaway socket).
///
/// Socket lifecycle (the shared private core both binds share):
///
/// 1. `holepunch::bind_dual_stack_udp(0)` — C1's dual-stack `[::]`
///    wildcard (V6ONLY cleared, non-blocking armed). A host without IPv6
///    (or an unclearable V6ONLY) falls back to the historical v4
///    wildcard bind — C1's fallback chain, preserved verbatim.
/// 2. Optional STUN: the std socket is wrapped in a tokio `UdpSocket`
///    (same OS socket, so no new mapping), a v4 server address is sent
///    to in `::ffff:` mapped form from the v6-family socket (the honest
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
    let (socket, reflexive, dual_stack, bind_fallback, stun_error) =
        bind_and_stun_socket(stun).await?;
    Ok(ReceiverSocket {
        socket,
        reflexive,
        dual_stack,
        bind_fallback,
        stun_error,
    })
}

/// Bind the SENDER's future-QUIC socket and — when a STUN server is
/// configured — query its server-reflexive address ON THAT SAME SOCKET
/// (CR-P1-05, review 2026-11-11 §4.5 改法第 2 条: the sender's mapping
/// must be established on the dial socket BEFORE the redeem, because the
/// endpoint blob the redeem registers carries the reflexive address —
/// the paired receiver's punch loop then aims at a mapping the QUIC
/// Initial will actually leave from).
///
/// Same lifecycle as [`bind_receiver_socket`] (the shared core); the
/// differences are all in the consumer:
///
/// * the caller inserts `reflexive` into its OWN derived candidate list
///   via [`insert_reflexive_candidate`] before registering the blob;
/// * the caller keeps `socket` until the send phase: the send-time probe
///   round runs on a `try_clone` of it ([`probe_dial_round_on`]) and
///   every engine dial attempt adopts a `try_clone` of it
///   ([`ak_engine::SendSpec::adopt_socket`]) — one OS socket, one NAT
///   mapping, both probing and QUIC.
///
/// Fail-open by design (identical budget and reasons): a host without a
/// STUN server, or with an unreachable one, still gets the bound socket
/// and local candidates — the Windows/dev-host behavior without STUN is
/// byte-identical to the pre-CR path.
pub async fn bind_sender_socket(stun: Option<SocketAddr>) -> Result<SenderSocket, String> {
    let (socket, reflexive, dual_stack, bind_fallback, stun_error) =
        bind_and_stun_socket(stun).await?;
    Ok(SenderSocket {
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
/// the sender's connect phase, on a THROWAWAY socket — the legacy C3
/// shape, kept for callers with no adopted socket. The paired receiver's
/// echo responder answers on its bound socket (its reply IS a probe
/// packet, so the round observes the answering candidate), and a peer
/// probe round still overlapping from the pairing lands here as an
/// observed source the candidate list never named; the dial plan puts
/// the observed address first ([`observed_dial_head`]).
///
/// Callers that bound the future-QUIC sender socket (CR-P1-05) pass its
/// `try_clone` to [`probe_dial_round_on`] instead, so the round's
/// packets leave from the port the QUIC dial will leave from — the
/// throwaway socket's mapping does NOT carry into the dial, which is
/// exactly the one-way-punching defect this module's CR-P1-05 work
/// removed from the sender path (the engine adoption entry is
/// `ak_engine::SendSpec::adopt_socket`).
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
    probe_dial_round_on(std_socket, probed, ordered, observe).await
}

/// The unified probe round (CR-P1-05, review 2026-11-11 §4.5): same
/// packets, same window, same observation rules as
/// [`probe_dial_round`], but the round runs ON the CALLER-OWNED socket —
/// a `try_clone` of the adopted sender socket (a duplicate HANDLE of the
/// same OS socket: same local port, same NAT mapping), so the packets
/// the receiver's echo responder answers left from the exact port the
/// subsequent QUIC dial adopts. This is the sender half of the review's
/// core acceptance rule (审核改法第 4 条: both sides' probing must happen
/// on THE socket that will carry QUIC).
///
/// `socket` is consumed — the clone's handle closes when the round ends,
/// while the caller's original keeps the shared OS socket alive.
/// A registration failure of the clone fails open to the static order
/// exactly like the throwaway round's bind failure.
pub async fn probe_dial_round_on<F>(
    socket: std::net::UdpSocket,
    probed: usize,
    ordered: Vec<SocketAddr>,
    observe: F,
) -> ProbeDialOrder
where
    F: FnOnce(Option<SocketAddr>, &ak_net::holepunch::PunchReport, usize),
{
    if socket.set_nonblocking(true).is_err() {
        return ProbeDialOrder {
            order: ordered,
            head: None,
        };
    }
    let Ok(socket) = tokio::net::UdpSocket::from_std(socket) else {
        return ProbeDialOrder {
            order: ordered,
            head: None,
        };
    };
    // CR-P3-06 (G2): the round exits early once the static order's first
    // candidate is echo-confirmed — a same-LAN send pays one probe
    // round-trip instead of the fixed 1 s window; an unconfirmed head
    // still runs the whole window (fail-open reachability observation
    // verbatim). Both adapters (FFI `run_send`, CLI `drive_task_send`)
    // dial through HERE, so both get the early exit without touching
    // their files.
    let report = match ak_net::holepunch::probe_with_early_exit(
        &socket,
        &ordered,
        PROBE_DIAL_INTERVAL,
        PROBE_DIAL_WINDOW,
    )
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
// The receiver-side hole punch (CR-P1-05, review 2026-11-11 §4.5 改法第 3
// 条's receiver half: the QUIC socket itself must SEND to the sender).
// ---------------------------------------------------------------------------

/// The outcome snapshot of one [`punch_candidates`] run — a bounded
/// report for the adapters' diagnostics (event line / stdout line) and
/// the unit pins of cadence, window and early stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchReport {
    /// Probe-shaped punch packets actually sent (sum over all targets and
    /// ticks; per-datagram send errors do not count).
    pub packets_sent: usize,
    /// CR-P1-06: punch packets sent with the low [`PUNCH_LOW_TTL`]
    /// (first phase, IPv4 targets) — via per-packet control data OR the
    /// CR-P1-08 socket-level fallback (see `low_ttl_socket`). Zero only
    /// when the host has no low-TTL send at all (or the knob disabled
    /// it) and during a window entirely covered by early stop.
    pub low_ttl_packets: usize,
    /// CR-P1-08: the low phase rode the SOCKET-LEVEL TTL fallback
    /// (`setsockopt(IP_TTL)` armed/restored around the phase) — hosts
    /// without per-packet control data, e.g. Windows. `false` on
    /// per-packet hosts and when no low-TTL send exists at all.
    pub low_ttl_socket: bool,
    /// CR-P1-08 改法 3b: the whole punch was DELAYED past
    /// [`PUNCH_LOW_TTL_PHASE`] because this host has NO low-TTL send at
    /// all (both the per-packet path and the socket-level fallback
    /// unavailable, or the `AK_EXP_NO_TTL` test knob) — the sender's own
    /// low-TTL pairing-time opener ([`sender_pairing_opener`]) then owns
    /// the pre-punch window, so this side's normal-TTL punches can only
    /// ever arrive at the sender's EXISTING outbound flow instead of
    /// creating the inbound-only conntrack record that made SNAT rewrite
    /// the sender's external port.
    pub start_delayed: bool,
    /// Tick rounds that fired (≈ `window / interval`, fewer on early
    /// stop).
    pub ticks: usize,
    /// Whether the loop ended via the shared stop flag before the window
    /// elapsed (the early-stop path — set by the accept side on the
    /// first successful data-plane connection, or by task teardown).
    pub stopped_early: bool,
}

/// The RECEIVER-side hole punch (CR-P1-05): from `socket` — a
/// `try_clone` of the QUIC listener's socket taken BEFORE the socket was
/// handed to `DirectEndpoint::bind_with_socket` (same OS socket, same
/// local port, a duplicate HANDLE only) — send one small probe-shaped
/// packet to every advertised candidate of the paired SENDER, every
/// [`PUNCH_INTERVAL`] for at most [`PUNCH_WINDOW`], concurrently with
/// the sender's QUIC dial.
///
/// Why this direction matters: until CR-P1-05 the receiver's QUIC socket
/// never sent a byte to the peer (only the STUN query ever left it), so
/// a port-restricted-cone NAT on the receiver side filtered the sender's
/// QUIC Initial out. The punch's outbound packets open the receiver's
/// own mapping toward the sender's advertised (reflexive) addresses —
/// exactly the mapping the Initial then rides. The packet payload is an
/// `holepunch::probe` packet with a time-derived nonce: the sender's
/// echo responder answers it (opening the sender-side filter toward the
/// receiver's QUIC mapping — the mirror direction), and neither QUIC
/// stack ever reads it (the clone below NEVER receives: receiving on a
/// duplicated handle would compete with quinn for the socket's queue —
/// this loop is send-only by construction).
///
/// CR-P1-06 two-phase TTL (review 2026-11-11 §4.5 改法 1/3, batchF
/// conntrack evidence): the FIRST [`PUNCH_LOW_TTL_PHASE`] sends its
/// IPv4 datagrams with a low TTL — they open THIS side's NAT mapping but
/// expire before the sender's NAT, so they cannot leave the inbound-only
/// conntrack record that made Linux-type SNAT rewrite the sender's
/// external port (the pcone↔pcone failure). After the phase (and for
/// IPv6 targets always — no NAT, normal hop limit) the punch continues
/// at the socket's normal TTL: the backup behavior for NATs that need to
/// actually see the peer's traffic.
///
/// CR-P1-08 (review 2026-11-13 §4.4 改法 3): the low TTL rides per-packet
/// `sendmsg` control messages (`ak_net::holepunch::send_to_with_ttl`)
/// where the host has them; a host WITHOUT per-packet support (Windows:
/// `WSASendMsg` documents no IPv4 `IP_TTL` control data) falls back to
/// the `SocketTtlGuard` — the socket's OWN TTL is temporarily set low for
/// the phase and restored after. Safe at pairing time by construction:
/// the punch starts the moment the pairing completes and the task's QUIC
/// socket carries no connection yet (the acknowledged cost — an IPv4
/// peer's racing handshake reply would expire in transit and recover via
/// retransmission once the phase ends; LAN (1 hop) and IPv6 are
/// unaffected).
///
/// CR-P1-08 改法 3b, receiver half: when NO low-TTL send exists at all
/// (both paths unavailable, or the hidden `AK_EXP_NO_TTL` matrix knob),
/// the whole punch is DELAYED past [`PUNCH_LOW_TTL_PHASE`] — the paired
/// sender's own low-TTL pairing-time opener ([`sender_pairing_opener`])
/// opens the sender-side NAT flow inside that window, so this side's
/// normal-TTL punches arrive at an EXISTING outbound conntrack flow
/// (matching it reverse-direction, marking it replied) instead of
/// creating the inbound-only record. No wire-format capability bit is
/// needed: the receiver decides from its OWN capability alone.
///
/// Bounded and never blocking: family-undialable candidates are skipped
/// (a v4 socket cannot send to v6; a v6-family socket dials plain-v4
/// candidates in `::ffff:` mapped form, the same bridge
/// `DirectEndpoint::connect` uses), per-datagram send errors are
/// skipped (a dead candidate costs nothing — in the low phase a failed
/// TTL send is NOT retried at normal TTL, that would re-create the very
/// record the phase exists to avoid), at most
/// [`PUNCH_MAX_TARGETS`] candidates per tick, and the loop ends at the
/// FIRST of: the shared `stop` flag (set by the accept path on the first
/// successful data-plane connection — the review's early-stop rule — or
/// by teardown), or the window's deadline.
///
/// `report` fires once with the final [`PunchReport`] so the adapter can
/// log its bounded line (the CLI prints there; the FFI passes a no-op —
/// the frozen §8.3 event set gains no new event).
pub async fn punch_candidates<F>(
    socket: std::net::UdpSocket,
    candidates: &[SocketAddr],
    stop: &AtomicBool,
    report: F,
) where
    F: FnOnce(PunchReport),
{
    // The family bridge: the QUIC socket (and thus its clone) is the
    // dual-stack [::] bind or the v4 fallback; normalize each target to
    // the form this socket can actually send to.
    let local = match socket.local_addr() {
        Ok(local) => local,
        Err(_) => {
            report(PunchReport {
                packets_sent: 0,
                low_ttl_packets: 0,
                low_ttl_socket: false,
                start_delayed: false,
                ticks: 0,
                stopped_early: false,
            });
            return;
        }
    };
    let targets = punch_targets(local, candidates);
    // One packet for the whole run: same time-derived nonce (the reply,
    // if any, lands in quinn's queue and is discarded — the loop never
    // reads it back).
    let packet = ak_net::holepunch::probe_packet(&punch_nonce());
    // CR-P1-08: the low-TTL first phase needs a low-TTL SEND on this
    // host — per-packet control data first, else the socket-level
    // fallback (armed once HERE, before the window; the phase boundary
    // restores it), else no low phase at all. IPv6-only target lists
    // never need it (no NAT — the delay below would be pure latency).
    let knob_disabled = exp_no_low_ttl();
    let per_packet = !knob_disabled && ak_net::holepunch::per_packet_ttl_supported();
    let has_v4_target = targets
        .iter()
        .any(|addr| ak_net::classify::expand_mapped(*addr).is_ipv4());
    let mut socket_ttl = SocketTtlGuard::new(&socket);
    let mut low_ttl_socket = false;
    let low_ttl_available = if !has_v4_target || knob_disabled {
        false
    } else if per_packet {
        true
    } else {
        match socket_ttl.arm(PUNCH_LOW_TTL) {
            Ok(()) => {
                low_ttl_socket = true;
                true
            }
            Err(_) => false,
        }
    };
    let start_delayed = has_v4_target && !low_ttl_available;
    let mut packets_sent = 0usize;
    let mut low_ttl_packets = 0usize;
    let mut ticks = 0usize;
    if start_delayed
        && sleep_polling_stop(tokio::time::Instant::now() + PUNCH_LOW_TTL_PHASE, stop).await
    {
        // Teardown/first-connection arrived before the delayed window
        // even opened: the honest bounded end, nothing was sent.
        report(PunchReport {
            packets_sent,
            low_ttl_packets,
            low_ttl_socket,
            start_delayed,
            ticks,
            stopped_early: true,
        });
        return;
    }
    let started = tokio::time::Instant::now();
    // No low phase at all when unavailable: `low_phase_end == started`
    // makes every tick's `low_phase` false.
    let low_phase_end = if low_ttl_available {
        started + PUNCH_LOW_TTL_PHASE
    } else {
        started
    };
    let deadline = started + PUNCH_WINDOW;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        ticks += 1;
        let low_phase = tokio::time::Instant::now() < low_phase_end;
        if !per_packet && low_ttl_available && !low_phase && socket_ttl.armed() {
            // CR-P1-08: the phase boundary — restore the socket's normal
            // TTL (a refused restore retries next tick; the guard's Drop
            // retries once more, so the armed window is always bounded).
            let _ = socket_ttl.restore();
        }
        for target in &targets {
            // Per-datagram TTL decision (pure, unit-pinned below): low
            // TTL only for IPv4 targets inside the first phase on hosts
            // that can send it per-packet; everything else normal (the
            // socket-level fallback's low TTL rides the plain send
            // below while its guard is armed).
            let sent = match punch_send_ttl(*target, low_phase, per_packet) {
                // A refused/failed cmsg send is skipped, never retried
                // at normal TTL inside the low phase (see the function
                // doc — a normal-TTL fallback would re-create the very
                // conntrack record the phase exists to avoid).
                Some(ttl) => {
                    if send_low_ttl_punch(&socket, &packet, *target, ttl) {
                        low_ttl_packets += 1;
                        true
                    } else {
                        false
                    }
                }
                // Non-blocking UDP send_to: success is best-effort
                // delivery; per-datagram errors (unreachable,
                // WSAECONNRESET-shaped) cost nothing and never end the
                // loop.
                None => {
                    let ok = socket.send_to(&packet, target).is_ok();
                    if ok && socket_ttl.armed() && low_phase {
                        low_ttl_packets += 1;
                    }
                    ok
                }
            };
            if sent {
                packets_sent += 1;
            }
        }
        let now = tokio::time::Instant::now();
        if now + PUNCH_INTERVAL >= deadline {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(now + PUNCH_INTERVAL) => {},
            _ = tokio::time::sleep_until(deadline) => {
                // The window ends during this sleep; the deadline arm
                // wins (whichever fires first) and the loop exits on
                // the elapsed check below only via this break.
                break;
            }
        }
    }
    let stopped_early = stop.load(Ordering::Relaxed) && tokio::time::Instant::now() < deadline;
    report(PunchReport {
        packets_sent,
        low_ttl_packets,
        low_ttl_socket,
        start_delayed,
        ticks,
        stopped_early,
    });
}

/// CR-P1-08 (review 2026-11-13 §4.4 改法 3a): temporarily set a WHOLE
/// socket's IPv4 TTL low and restore it afterwards. For hosts without
/// per-packet TTL control data (Windows `WSASendMsg` documents no IPv4
/// `IP_TTL` cmsg) this is the receiver punch's low-phase fallback: at
/// pairing completion the task's QUIC socket carries NO connection yet,
/// so the socket-wide TTL cannot disturb any other traffic (the review's
/// §6.4 correction of its earlier "never touch a shared socket" rule).
/// Fail-open by design: a host that refuses the option (`ttl()`/
/// `set_ttl()` errors, e.g. a dual-stack shape the platform won't take
/// `IP_TTL` on) simply keeps normal TTL — the documented degradation.
struct SocketTtlGuard<'a> {
    socket: &'a std::net::UdpSocket,
    /// The TTL observed at arm time (restored verbatim; a failed read
    /// leaves `None` and the arm is refused).
    original: Option<u32>,
    armed: bool,
}

impl<'a> SocketTtlGuard<'a> {
    fn new(socket: &'a std::net::UdpSocket) -> Self {
        Self {
            socket,
            original: None,
            armed: false,
        }
    }

    fn armed(&self) -> bool {
        self.armed
    }

    /// Read the current TTL, set `ttl`, mark armed. A refused read/set
    /// leaves the guard unarmed (the caller degrades to normal TTL).
    fn arm(&mut self, ttl: u32) -> std::io::Result<()> {
        if self.armed {
            return Ok(());
        }
        let original = self.socket.ttl()?;
        self.socket.set_ttl(ttl)?;
        self.original = Some(original);
        self.armed = true;
        Ok(())
    }

    /// Restore the TTL observed at arm time. A refused set keeps the
    /// guard armed so the caller/Drop retries.
    fn restore(&mut self) -> std::io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        // `arm` captures the original before flipping `armed`, so an
        // armed guard always carries it.
        let original = self
            .original
            .expect("an armed SocketTtlGuard captured the original TTL");
        self.socket.set_ttl(original)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for SocketTtlGuard<'_> {
    fn drop(&mut self) {
        // The early-stop path (first connection / teardown) can land
        // mid-phase: never leave a low TTL behind on the QUIC socket.
        let _ = self.restore();
    }
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 4): hidden debug parameter,
/// DEFAULT OFF — never set in production. `AK_EXP_NO_TTL` disables the
/// receiver punch's low-TTL phase ENTIRELY (both the per-packet path and
/// the CR-P1-08 socket-level fallback), simulating a receiver host with
/// no low-TTL send at all (the pre-CR-P1-08 Windows shape) for the NAT
/// matrix's "receiver without low TTL" case. Such a receiver also
/// delays its punch start — see [`punch_candidates`].
fn exp_no_low_ttl() -> bool {
    std::env::var("AK_EXP_NO_TTL")
        .map(|value| !value.is_empty() && value != "0")
        .unwrap_or(false)
}

/// Sleep until `deadline` in [`STOP_POLL_INTERVAL`] chunks, returning
/// early (`true`) the moment `stop` flips: the bounded side loops share
/// one stop flag with no async signal, and a full-interval sleep would
/// notice a send that already started only at the next keepalive tick.
async fn sleep_polling_stop(deadline: tokio::time::Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        tokio::time::sleep((deadline - now).min(STOP_POLL_INTERVAL)).await;
    }
}

/// The per-datagram TTL decision of [`punch_candidates`] — pure on
/// purpose (unit-pinned without any socket): `Some(PUNCH_LOW_TTL)` for an
/// IPv4 target (plain or `::ffff:` mapped — the dual-stack socket's v4
/// form; both the decision and the TTL control message key off the
/// target's EXPANDED family, see
/// `ak_net::holepunch::send_to_with_ttl`) inside the low-TTL first
/// phase on a host with per-packet TTL support; `None` (normal socket
/// TTL) for everything else — IPv6 targets (no NAT → normal hop limit,
/// CR-P1-06 改法 3), the backup phase, and the Windows degradation.
pub fn punch_send_ttl(target: SocketAddr, low_phase: bool, ttl_supported: bool) -> Option<u32> {
    if low_phase && ttl_supported && ak_net::classify::expand_mapped(target).is_ipv4() {
        Some(PUNCH_LOW_TTL)
    } else {
        None
    }
}

/// One low-TTL punch datagram through the per-packet control-message
/// path (Unix; see [`punch_send_ttl`]). `false` when the send was
/// refused — the caller skips the datagram (no normal-TTL fallback
/// inside the low phase).
#[cfg(unix)]
fn send_low_ttl_punch(
    socket: &std::net::UdpSocket,
    packet: &[u8],
    target: SocketAddr,
    ttl: u32,
) -> bool {
    ak_net::holepunch::send_to_with_ttl(socket, packet, target, ttl).is_ok()
}

/// Off-Unix the low phase never arms ([`punch_send_ttl`] returns `None`
/// while `per_packet_ttl_supported()` is false), so this stub only keeps
/// the loop compiling — the documented Windows degradation is "normal
/// TTL for the whole window".
#[cfg(not(unix))]
fn send_low_ttl_punch(
    socket: &std::net::UdpSocket,
    packet: &[u8],
    target: SocketAddr,
    ttl: u32,
) -> bool {
    let _ = (socket, packet, target, ttl);
    false
}

/// Time/counter-derived nonce of the punch packet — never all-zero (the
/// echo responder's anti-reflection rule treats the all-zero nonce as a
/// REPLY and never answers it, which would waste the sender-side filter
/// opening).
fn punch_nonce() -> [u8; 16] {
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

/// The punch target selection over raw advertised candidates — pure on
/// purpose (tests pin the rule without any socket). Rules:
///
/// * at most [`PUNCH_MAX_TARGETS`] candidates are considered (a
///   hostile/buggy blob cannot blow up one tick's send count);
/// * unspecified addresses and port 0 (the sender-role "no listener"
///   marker of the pre-CR-P1-05 blobs) are not dialable — skipped;
/// * the same family bridge `DirectEndpoint::connect` uses: a v6-family
///   punch socket (the dual-stack `[::]` bind) dials plain-v4 candidates
///   in `::ffff:` mapped form, a v4 socket expands mapped candidates
///   back to plain v4;
/// * a genuinely undialable family (v4 socket, v6 candidate) is skipped;
/// * the result dedupes (normalized form), preserving order.
pub fn punch_targets(local: SocketAddr, candidates: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut targets: Vec<SocketAddr> = Vec::with_capacity(candidates.len());
    for candidate in candidates.iter().take(PUNCH_MAX_TARGETS) {
        if candidate.ip().is_unspecified() || candidate.port() == 0 {
            continue;
        }
        let target = if local.is_ipv6() && candidate.is_ipv4() {
            ak_net::classify::to_mapped(*candidate)
        } else if local.is_ipv4() {
            ak_net::classify::expand_mapped(*candidate)
        } else {
            *candidate
        };
        if target.is_ipv6() != local.is_ipv6() {
            continue;
        }
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

// ---------------------------------------------------------------------------
// The sender's post-pairing reflexive opener + the shared interleaved dial
// pass (CR-P1-06 改法 2 + CR-P3-09, review 2026-11-11 §4.5).
// ---------------------------------------------------------------------------

/// CR-P1-06 改法 2, the packet-send half: after the pairing completes,
/// wait [`SENDER_DIAL_PAUSE`] (the review's 200–500 ms band — the
/// receiver's low-TTL punch datagrams must have left its socket first),
/// then send one probe-shaped packet from a SEND-ONLY `try_clone` of the
/// ADOPTED sender socket to every PUBLIC (reflexive) candidate of the
/// paired receiver — this is the adopted socket's first outbound packet
/// toward the receiver, the flow that occupies the sender-NAT tuple
/// BEFORE the receiver's normal-TTL backup punches (phase 2, ≥
/// [`PUNCH_LOW_TTL_PHASE`]) can arrive there, and the mapping the QUIC
/// Initial then rides. Since CR-P1-07 the IPv4 datagrams ride the LOW
/// [`PUNCH_LOW_TTL`] where the host can send it (see the private
/// `send_opener_packets`): the opener only needs to open THIS side's
/// NAT flow — reaching the peer was never required (the peer's QUIC
/// stack silently discards the probe-shaped packet), and a normal-TTL
/// datagram arriving before the peer's own first punch would poison the
/// PEER's NAT with the inbound-only record CR-P1-06 exists to avoid.
///
/// No adopted socket (bind failed / an older flow): the wait still runs
/// (uniform, adapter-identical timing) but no packets are sent — a
/// throwaway socket's mapping would not carry into the dial, so there is
/// nothing useful to open. Returns the packets actually sent (0 without
/// an adopted socket or with no public candidates — diagnostics only).
/// Target filter of [`sender_reflexive_opener`]: everything except
/// loopback. RFC1918 stays IN (a reflexive address can itself be
/// private — double NAT; the CI matrix's 10/8 routers model exactly
/// that — and filtering it made the opener send zero packets, letting
/// the CR-P1-06 tuple conflict replay: run 43304434327 diagnosis §1).
/// Rust's native is_loopback semantics: `::ffff:127.0.0.1` is NOT
/// Ipv6Addr::is_loopback, so mapped-loopback candidates pass — harmless
/// (they are same-host; the opener packet lands locally, opens nothing).
pub fn opener_targets(static_order: &[SocketAddr]) -> Vec<SocketAddr> {
    static_order
        .iter()
        .copied()
        .filter(|addr| !addr.ip().is_loopback())
        .collect()
}

pub async fn sender_reflexive_opener(
    adopted: Option<&std::net::UdpSocket>,
    static_order: &[SocketAddr],
) -> usize {
    tokio::time::sleep(SENDER_DIAL_PAUSE).await;
    let Some(adopted) = adopted else {
        return 0;
    };
    // Reflexive/public targets only — LAN candidates cannot ride a NAT
    // mapping and the opener is exactly a NAT-tuple opener. "Public"
    // here excludes LOOPBACK only, not RFC1918: a STUN reflexive address
    // can itself be RFC1918 (double NAT: the outer NAT's inside
    // interface is private; the CI matrix's routers model exactly this),
    // and filtering by is_ipv4_private made the opener send ZERO packets
    // whenever the peer's reflexive fell in 10/8 — the receiver's
    // normal-TTL phase-2 punches then reached our NAT first and the
    // original CR-P1-06 tuple conflict replayed in full (run
    // 43304434327, packet-level diagnosis §1). The interface-derived
    // candidates that share the socket's actual LAN are harmless
    // opener targets (they simply get no mapping); the loopback
    // exclusion keeps same-host noise out. Selection shares the punch's
    // family bridge and caps.
    let Some((clone, targets)) = opener_send_handles(adopted, static_order) else {
        return 0;
    };
    send_opener_packets(&clone, &targets)
}

/// The opener's send-only clone of the adopted socket plus its
/// family-bridged, capped target list — the shared prep of the
/// standalone opener ([`sender_reflexive_opener`]) and the pairing-time
/// keepalive ([`sender_pairing_opener`]). `None` when the socket cannot
/// be cloned or no dialable target survives the filters (nothing to
/// open).
fn opener_send_handles(
    adopted: &std::net::UdpSocket,
    static_order: &[SocketAddr],
) -> Option<(std::net::UdpSocket, Vec<SocketAddr>)> {
    let local = adopted.local_addr().ok()?;
    let targets = punch_targets(local, &opener_targets(static_order));
    if targets.is_empty() {
        return None;
    }
    let clone = adopted.try_clone().ok()?;
    Some((clone, targets))
}

/// The opener's per-target send. CR-P1-07 (review 2026-11-13 §4.4
/// 改法 1/3b, sender half): IPv4 datagrams ride the LOW
/// [`PUNCH_LOW_TTL`] wherever this host can send it — per-packet control
/// data first (`send_low_ttl_punch`), else the [`SocketTtlGuard`]
/// socket-level fallback, else the honest normal-TTL degradation.
/// Rationale: the opener's job is to open THIS side's NAT flow for the
/// tuple the QUIC dial will ride — the packet reaching the peer was
/// never required (the peer's QUIC stack discards it) — while a
/// NORMAL-TTL opener that arrives before the peer's own first punch
/// packet creates exactly the inbound-only conntrack record at the
/// PEER's NAT that CR-P1-06 removed on the other leg. A receiver with
/// no low-TTL send of its own punches late for the same reason (see
/// [`punch_candidates`]). IPv6 targets keep the normal hop limit (no
/// NAT). Loopback/LAN delivery decrements no TTL — same-host and LAN
/// transfers are unaffected.
fn send_opener_packets(clone: &std::net::UdpSocket, targets: &[SocketAddr]) -> usize {
    let packet = ak_net::holepunch::probe_packet(&punch_nonce());
    let per_packet = ak_net::holepunch::per_packet_ttl_supported();
    let mut guard = SocketTtlGuard::new(clone);
    if !per_packet {
        // The socket-level fallback: the whole socket's TTL stays low for
        // exactly these sends (restored below); a refused arm degrades to
        // normal TTL — the honest pre-CR-P1-08 behavior.
        let _ = guard.arm(PUNCH_LOW_TTL);
    }
    let mut sent = 0usize;
    for target in targets {
        let delivered = if per_packet && ak_net::classify::expand_mapped(*target).is_ipv4() {
            // A refused/failed low-TTL send is skipped, never retried at
            // normal TTL (a normal datagram is exactly the inbound-record
            // creator the low TTL exists to avoid).
            send_low_ttl_punch(clone, &packet, *target, PUNCH_LOW_TTL)
        } else {
            // IPv6 (no NAT), the socket-level fallback (the armed guard's
            // low TTL rides the plain send), or the no-low-TTL
            // degradation.
            clone.send_to(&packet, target).is_ok()
        };
        if delivered {
            sent += 1;
        }
    }
    let _ = guard.restore();
    sent
}

/// Bounded end report of one [`sender_pairing_opener`] run (CR-P1-07):
/// the opener half's packet count plus the keepalive's tick/packet
/// counts and which bound ended it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenerKeepaliveReport {
    /// Probe-shaped packets the OPENER half sent (0 without an adopted
    /// socket or with no public candidates — diagnostics only, the same
    /// number the `reflexive_opener` event carries).
    pub opener_packets: usize,
    /// Keepalive packets sent after the opener (one per target per
    /// tick).
    pub keepalive_packets: usize,
    /// Keepalive ticks that fired (≈ elapsed / interval).
    pub keepalive_ticks: usize,
    /// The stop flag ended the keepalive before the budget elapsed (the
    /// send started, or the connection/task ended).
    pub stopped_early: bool,
    /// The total keepalive budget elapsed (the mapping hold ended by
    /// time, not by the stop flag).
    pub budget_elapsed: bool,
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 1), the SHARED sender-side
/// pairing-time core both adapters call at PAIRING COMPLETION:
///
/// 1. the opener half — byte-identical to [`sender_reflexive_opener`]
///    ([`SENDER_DIAL_PAUSE`], then the adopted socket's first outbound
///    packets toward the receiver's public/reflexive candidates, low TTL
///    for IPv4 where the host can send it);
/// 2. the keepalive half — every [`OPENER_KEEPALIVE_INTERVAL`] one
///    probe-shaped packet per public target from a send-only clone of
///    the SAME adopted socket, until the shared `stop` flag flips (the
///    send op sets it at its start; disconnect/teardown flip it too),
///    the connection/task ends, or [`OPENER_KEEPALIVE_BUDGET`] elapses.
///
/// Why the keepalive exists: the CLI always sent seconds after pairing,
/// inside the receiver's 4 s punch window; the GUI sends when the user
/// has picked files — minutes later, long after the receiver's punch
/// ended and (past 30 s) after Linux-type NATs expire the unreplied
/// mappings. Each keepalive datagram refreshes BOTH sides' entries: the
/// sender's outbound flow (forward direction) and the receiver's punched
/// mapping (reverse direction — which also marks it REPLIED/assured,
/// 120 s on Linux-type NATs), so the eventual QUIC Initial rides the
/// same hot tuple regardless of when the send starts.
///
/// `on_opener` fires ONCE with the opener half's packet count (both
/// adapters surface it — the FFI's `reflexive_opener` event, the CLI's
/// stdout line) whether or not an adopted socket exists. The keepalive
/// itself is silent by design (no event; the end report is the
/// adapter's optional bounded diagnostic). `None` adopted socket (bind
/// failed / an older flow) runs the pause and reports zero — uniform
/// timing, nothing to keep alive.
pub async fn sender_pairing_opener<F>(
    adopted: Option<&std::net::UdpSocket>,
    static_order: &[SocketAddr],
    stop: &AtomicBool,
    on_opener: F,
) -> OpenerKeepaliveReport
where
    F: FnOnce(usize),
{
    let opener_packets = sender_reflexive_opener(adopted, static_order).await;
    on_opener(opener_packets);
    let Some(adopted) = adopted else {
        return OpenerKeepaliveReport {
            opener_packets,
            ..OpenerKeepaliveReport::default()
        };
    };
    let Some((clone, targets)) = opener_send_handles(adopted, static_order) else {
        return OpenerKeepaliveReport {
            opener_packets,
            ..OpenerKeepaliveReport::default()
        };
    };
    let started = tokio::time::Instant::now();
    let deadline = started + OPENER_KEEPALIVE_BUDGET;
    let mut next_tick = started + OPENER_KEEPALIVE_INTERVAL;
    let mut keepalive_packets = 0usize;
    let mut keepalive_ticks = 0usize;
    loop {
        // Wake at the earlier of the next tick and the total budget; a
        // flipped stop flag returns within one STOP_POLL_INTERVAL.
        if sleep_polling_stop(next_tick.min(deadline), stop).await {
            return OpenerKeepaliveReport {
                opener_packets,
                keepalive_packets,
                keepalive_ticks,
                stopped_early: true,
                budget_elapsed: false,
            };
        }
        if tokio::time::Instant::now() >= deadline {
            return OpenerKeepaliveReport {
                opener_packets,
                keepalive_packets,
                keepalive_ticks,
                stopped_early: false,
                budget_elapsed: true,
            };
        }
        keepalive_ticks += 1;
        // A fresh nonce per tick (a real prober's nonce is time-derived;
        // the peer's echo responder never answers the all-zero reply
        // nonce either way).
        let packet = ak_net::holepunch::probe_packet(&punch_nonce());
        for target in &targets {
            if clone.send_to(&packet, target).is_ok() {
                keepalive_packets += 1;
            }
        }
        next_tick += OPENER_KEEPALIVE_INTERVAL;
    }
}

/// CR-P1-07 (review 2026-11-13 §4.4 改法 2): fold one adopted-socket
/// probe round's [`ProbeDialOrder`] into the pairing-time
/// [`ProbeOutcome`] vocabulary — any observed head (an echo-confirmed
/// candidate, or the raw source of a peer probe arriving on the QUIC
/// socket's own mapping) is a genuine two-way observation (`Success`);
/// silence over every candidate is the honest all-failed verdict with
/// ONE bounded reason per candidate. Pure.
pub fn round_probe_outcome(ordered: &[SocketAddr], plan: &ProbeDialOrder) -> ProbeOutcome {
    match plan.head {
        Some(address) => ProbeOutcome::Success { address },
        None => ProbeOutcome::AllFailed {
            failures: ordered
                .iter()
                .map(|candidate| super::CandidateFailure {
                    candidate: *candidate,
                    // The same bounded string ak-session's probe plane
                    // uses for a silent candidate (reasons::SILENT is
                    // private to the parent module — mirrored verbatim).
                    reason: "no probe answer within the window",
                })
                .collect(),
        },
    }
}

/// CR-P3-09: reorder one static dial order so every PUBLIC (reflexive)
/// candidate strictly PRECEDES the LAN candidates ("反射/观测地址严格排
/// 最前，保序私网候选在慢路径") — the pair of the review's two accepted
/// remedies, applied together with [`DIAL_STAGGER`]. Pure: stable
/// partition on [`prefers_lan`], relative order preserved inside each
/// group. The observed head is pinned FIRST by the caller
/// ([`observed_first_order`]) after this partition, so a LAN observation
/// still leads.
pub fn reflexive_first_order(ordered: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut out = Vec::with_capacity(ordered.len());
    out.extend(ordered.iter().copied().filter(|addr| !prefers_lan(addr)));
    out.extend(ordered.into_iter().filter(prefers_lan));
    out
}

/// One candidate attempt's verdict for [`staggered_dial_pass`] — the
/// adapter classifies its own outcome type into this shared vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptVerdict {
    /// The attempt fully succeeded (transfer completed): the pass returns
    /// it, every other in-flight attempt is cancelled.
    Done,
    /// The attempt connected but the transfer failed, or the peer
    /// declined: another address cannot fix it — the pass returns it and
    /// the op fails (the adapter keeps its own message/state handling).
    Fatal,
    /// Purely connect-level failure (timeout, refused, or a
    /// connection-died-before-anything-offered race the adapters retry):
    /// recorded with its bounded reason, the remaining candidates carry
    /// the pass.
    Retryable(String),
}

/// The outcome of one [`staggered_dial_pass`] over a dial order.
#[derive(Debug)]
pub struct PassOutcome<A> {
    /// First attempt classified [`AttemptVerdict::Done`], with its
    /// address (the candidate that actually carried the task).
    pub done: Option<(SocketAddr, A)>,
    /// First attempt classified [`AttemptVerdict::Fatal`], with its
    /// address (the op fails; the adapter renders its own bounded
    /// message from `A`).
    pub fatal: Option<(SocketAddr, A)>,
    /// `(address, bounded reason)` of every [`AttemptVerdict::Retryable`]
    /// failure, in dial order.
    pub failures: Vec<(SocketAddr, String)>,
}

/// One in-flight candidate attempt of the interleaved pass.
struct DialSlot<'a, A> {
    addr: SocketAddr,
    marks: Arc<ConnectMarks>,
    fut: std::pin::Pin<Box<dyn Future<Output = A> + Send + 'a>>,
}

/// What the pass's poll sweep observed: an attempt finished (its adapter
/// outcome is ready) or an attempt's engine reported `Connected` (its
/// QUIC+TLS handshake is up — the Happy-Eyeballs winner signal).
enum DialEvent<A> {
    Completed(usize, A),
    Connected(usize),
}

/// The shared ONE connect pass over a dial order (CR-P3-09, the loop the
/// two adapters previously maintained twice). Behavior:
///
/// * The order is first re-sorted by [`reflexive_first_order`] with the
///   probe round's observed head pinned first
///   ([`observed_first_order`]) — public/reflexive/observed addresses
///   strictly lead, LAN candidates keep their order on the slow path.
/// * The FIRST entry dials under [`OBSERVED_DIAL_BUDGET`] when it is the
///   observed head or a public (reflexive) candidate (the hot-mapping
///   rule the adapters previously applied to the observed head alone);
///   every other entry keeps the full [`TASK_CONNECT_BUDGET`].
/// * `sequential == true` (the caller dialed with an ADOPTED socket):
///   attempts run strictly one at a time, in order — concurrent QUIC
///   endpoints on `try_clone` handles of ONE OS socket would compete for
///   its receive queue (ak-net's adoption contract: "the socket must not
///   be shared afterwards"), so interleaving is reserved for the
///   self-bound path where every attempt owns its socket. The
///   reflexive-first order still puts the reflexive leg in slot 0, which
///   is what makes cross-NAT connection time independent of the private
///   candidate count.
/// * `sequential == false`: Happy-Eyeballs — candidate 0 starts
///   immediately, candidate n starts at n × [`DIAL_STAGGER`]; all
///   in-flight attempts are polled concurrently; the FIRST attempt whose
///   marks report `Connected` becomes the winner (the others are dropped
///   — dropping an engine send future mid-handshake cancels it cleanly,
///   and a cancelled loser has sent no protocol byte), and the winner
///   alone runs to completion. A `Done`/`Fatal` completion cancels the
///   rest the same way.
/// * A `Retryable` completion frees its slot (the stagger keeps its
///   cadence); when every candidate has been started and every attempt
///   finished retryable, the pass ends with the failure list — the
///   caller's whole-list pass loop (settle pause, pass counting) stays
///   in the adapter.
///
/// `attempt` builds one candidate's engine future: it receives the
/// candidate address, a FRESH [`ConnectMarks`] to wire into the engine
/// progress callback (the pass reads `connected()` as its winner signal),
/// and the attempt's connect budget. `verdict` classifies the adapter's
/// own outcome value.
pub async fn staggered_dial_pass<'a, A, F, Fut, V>(
    order: &[SocketAddr],
    head: Option<SocketAddr>,
    sequential: bool,
    mut attempt: F,
    mut verdict: V,
) -> PassOutcome<A>
where
    F: FnMut(SocketAddr, Arc<ConnectMarks>, Duration) -> Fut,
    Fut: Future<Output = A> + Send + 'a,
    V: FnMut(&A) -> AttemptVerdict,
{
    // CR-P3-09 ordering: reflexive/public strictly first, then the
    // observed head pinned before everything (a LAN observation still
    // leads — it names a route that answered probes).
    let order = observed_first_order(reflexive_first_order(order.to_vec()), head);
    let mut failures: Vec<(SocketAddr, String)> = Vec::new();

    if sequential {
        for index in 0..order.len() {
            let addr = order[index];
            let marks = Arc::new(ConnectMarks::new());
            let budget = dial_attempt_budget(index, &order, head);
            let outcome = attempt(addr, marks, budget).await;
            match verdict(&outcome) {
                AttemptVerdict::Done => {
                    return PassOutcome {
                        done: Some((addr, outcome)),
                        fatal: None,
                        failures,
                    }
                }
                AttemptVerdict::Fatal => {
                    return PassOutcome {
                        done: None,
                        fatal: Some((addr, outcome)),
                        failures,
                    }
                }
                AttemptVerdict::Retryable(reason) => failures.push((addr, reason)),
            }
        }
        return PassOutcome {
            done: None,
            fatal: None,
            failures,
        };
    }

    // Happy-Eyeballs: stagger the starts, poll everything in flight, take
    // the first `Connected` as the winner.
    let mut slots: Vec<DialSlot<'a, A>> = Vec::new();
    let mut next_index = 0usize;
    let mut next_start = tokio::time::Instant::now();
    loop {
        // Start every attempt whose stagger slot has arrived.
        while next_index < order.len() && tokio::time::Instant::now() >= next_start {
            let addr = order[next_index];
            let marks = Arc::new(ConnectMarks::new());
            let budget = dial_attempt_budget(next_index, &order, head);
            let fut = attempt(addr, Arc::clone(&marks), budget);
            slots.push(DialSlot {
                addr,
                marks,
                fut: Box::pin(fut),
            });
            next_index += 1;
            next_start += DIAL_STAGGER;
        }
        let more_to_start = next_index < order.len();
        // One sweep polls every in-flight attempt for completion and
        // re-checks the winner signal. The `Connected` flag flips inside
        // the attempt's own poll (the engine progress callback is
        // synchronous), so the re-check after each poll sees it without
        // any extra notification plumbing.
        let event = std::future::poll_fn(|cx| {
            for (index, slot) in slots.iter_mut().enumerate() {
                if let std::task::Poll::Ready(outcome) = slot.fut.as_mut().poll(cx) {
                    return std::task::Poll::Ready(DialEvent::Completed(index, outcome));
                }
                if slot.marks.connected() {
                    return std::task::Poll::Ready(DialEvent::Connected(index));
                }
            }
            std::task::Poll::Pending
        });
        tokio::select! {
            // The stagger timer only fires while candidates remain; when
            // it does, the loop head starts what is due.
            _ = tokio::time::sleep_until(next_start), if more_to_start => {
                if slots.is_empty() {
                    // Nothing in flight (everything so far already failed
                    // retryable): the next candidate starts NOW rather
                    // than at its stagger deadline — the stagger exists
                    // to interleave with LIVE attempts, not to idle.
                    next_start = tokio::time::Instant::now();
                }
                continue;
            }
            event = event => match event {
                DialEvent::Connected(index) => {
                    // Winner: its QUIC handshake is up — cancel the
                    // others (dropping their futures aborts the dial;
                    // a loser has offered nothing) and finish on it
                    // alone.
                    let winner = slots.swap_remove(index);
                    slots.clear();
                    let addr = winner.addr;
                    let outcome = winner.fut.await;
                    return match verdict(&outcome) {
                        AttemptVerdict::Done => PassOutcome {
                            done: Some((addr, outcome)),
                            fatal: None,
                            failures,
                        },
                        AttemptVerdict::Fatal => PassOutcome {
                            done: None,
                            fatal: Some((addr, outcome)),
                            failures,
                        },
                        // Connected but retryable (the wave-29
                        // admission race: the connection died before
                        // any protocol byte). The others were already
                        // cancelled, so the pass ends here — the
                        // adapter's settle pause and whole-list pass
                        // retry is the recorded remedy for exactly this
                        // race.
                        AttemptVerdict::Retryable(reason) => {
                            failures.push((addr, reason));
                            PassOutcome {
                                done: None,
                                fatal: None,
                                failures,
                            }
                        }
                    };
                }
                DialEvent::Completed(index, outcome) => {
                    let slot = slots.swap_remove(index);
                    match verdict(&outcome) {
                        AttemptVerdict::Done => {
                            slots.clear();
                            return PassOutcome {
                                done: Some((slot.addr, outcome)),
                                fatal: None,
                                failures,
                            };
                        }
                        AttemptVerdict::Fatal => {
                            slots.clear();
                            return PassOutcome {
                                done: None,
                                fatal: Some((slot.addr, outcome)),
                                failures,
                            };
                        }
                        AttemptVerdict::Retryable(reason) => {
                            failures.push((slot.addr, reason));
                            if slots.is_empty() {
                                if next_index >= order.len() {
                                    break; // every candidate tried, all retryable
                                }
                                // Nothing left in flight: the next
                                // candidate starts immediately (the
                                // stagger only spaces LIVE attempts).
                                next_start = tokio::time::Instant::now();
                            }
                        }
                    }
                }
            }
        }
    }
    PassOutcome {
        done: None,
        fatal: None,
        failures,
    }
}

/// The connect budget of dial slot `index` (the shared hot-mapping rule):
/// the FIRST entry dials under [`OBSERVED_DIAL_BUDGET`] when it is the
/// probe round's observed head or a public (reflexive) candidate — both
/// name a route that should complete a QUIC+TLS handshake well inside 2 s
/// when live; every other candidate (and a LAN-first order with no public
/// candidate at all) keeps the full [`TASK_CONNECT_BUDGET`].
fn dial_attempt_budget(index: usize, order: &[SocketAddr], head: Option<SocketAddr>) -> Duration {
    if index == 0 && (head.is_some() || order.first().is_some_and(|addr| !prefers_lan(addr))) {
        OBSERVED_DIAL_BUDGET
    } else {
        TASK_CONNECT_BUDGET
    }
}

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

    /// CR-P3-06 (G2), the wiring: the shared send-time round exits the
    /// moment the static order's FIRST candidate is echo-confirmed — an
    /// echo responder heading the list (plus silent trailing candidates)
    /// brings the round home in one probe round-trip, far inside the
    /// 1 s window. Both adapters dial through this round, so this pins
    /// the loopback/LAN send latency recovery end to end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_dial_round_exits_early_once_the_head_candidate_answers() {
        let observed = Arc::new(StdMutex::new(None));
        let (responder, bound) = bind_echo_responder_socket().expect("responder binds");
        let slot = Arc::clone(&observed);
        let task = tokio::spawn(async move {
            echo_responder_loop(responder, slot, |_| {}).await;
        });
        let echo_addr = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        // A silent TRAILING candidate: the exit must not wait for it.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("silent bind");
        let silent_addr = SocketAddr::from((
            [127, 0, 0, 1],
            silent.local_addr().expect("silent addr").port(),
        ));
        let started = Instant::now();
        let plan = probe_dial_round(vec![echo_addr, silent_addr], |_, _, _| {}).await;
        let elapsed = started.elapsed();
        task.abort();
        drop(silent);

        assert_eq!(plan.head, Some(echo_addr), "the echo head leads");
        assert!(
            plan.order.first() == Some(&echo_addr),
            "the observed head leads the dial order: {:?}",
            plan.order
        );
        assert!(
            elapsed + Duration::from_millis(300) < PROBE_DIAL_WINDOW,
            "the round must exit on the first confirmation, not the window: {elapsed:?}"
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

    // -- CR-P1-05 (review 2026-11-11 §4.5): the sender socket lifecycle,
    //    the unified probe round and the receiver punch loop ---------------

    /// One loopback STUN binding responder thread (the t03 product-test
    /// harness shape): answers every parseable binding request with the
    /// OBSERVED source (truthful reflection).
    fn spawn_test_stun_responder() -> (SocketAddr, std::sync::Arc<AtomicBool>) {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("responder bind");
        let addr = socket.local_addr().expect("responder addr");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            socket
                .set_read_timeout(Some(Duration::from_millis(50)))
                .expect("responder timeout");
            let mut buffer = [0u8; 128];
            while !flag.load(Ordering::Relaxed) {
                if let Ok((len, from)) = socket.recv_from(&mut buffer) {
                    if let Ok(transaction) = ak_signal::stun::parse_binding_request(&buffer[..len])
                    {
                        let _ = socket
                            .send_to(&ak_signal::stun::binding_response(transaction, from), from);
                    }
                }
            }
        });
        (addr, stop)
    }

    /// CR-P1-05, the sender lifecycle's same-socket proof at unit level:
    /// a TRUTHFUL responder reflects the sender socket's own address —
    /// the reflexive port MUST equal the bound port (a throwaway query
    /// socket would reflect its own ephemeral port), and the socket is
    /// genuinely bound (the send phase adopts exactly this handle).
    #[tokio::test]
    async fn bind_sender_socket_queries_stun_on_the_same_socket() {
        let (server, stop) = spawn_test_stun_responder();
        let started = Instant::now();
        let sock = bind_sender_socket(Some(server))
            .await
            .expect("sender socket binds");
        stop.store(true, Ordering::Relaxed);
        let bound = sock.socket.local_addr().expect("sender socket bound addr");
        let reflexive = sock.reflexive.expect("truthful responder reflects");
        assert_eq!(
            reflexive.port(),
            bound.port(),
            "the STUN query must run on the socket the dial adopts: {reflexive} vs {bound}"
        );
        assert_eq!(
            reflexive.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "the truthful loopback responder reflects the local source: {reflexive}"
        );
        assert!(sock.stun_error.is_none(), "no failure on success");
        assert!(
            started.elapsed() < STUN_QUERY_BUDGET + Duration::from_millis(500),
            "the query stays inside its fail-open budget"
        );
    }

    /// The no-server fail-open path of the sender bind (the Windows /
    /// dev-host default when STUN is absent): bound socket, no reflexive
    /// address, no failure — byte-identical to the receiver mirror.
    #[tokio::test]
    async fn bind_sender_socket_without_a_server_skips_the_query() {
        let sock = bind_sender_socket(None).await.expect("binds");
        assert!(sock.reflexive.is_none(), "no server, no reflexive");
        assert!(sock.stun_error.is_none(), "no server, no failure");
        assert!(
            sock.socket.local_addr().is_ok(),
            "the dial socket is genuinely bound"
        );
    }

    /// The unified probe round leaves from the CALLER-OWNED socket: a
    /// silent bound "candidate" observes the round's probe packets, and
    /// their SOURCE PORT is the adopted socket's port (the review's core
    /// rule — the round and the QUIC dial share one OS socket).
    #[tokio::test]
    async fn probe_round_on_leaves_from_the_caller_owned_port() {
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").expect("silent bind");
        silent
            .set_read_timeout(Some(Duration::from_millis(2_500)))
            .expect("read timeout");
        let candidate = SocketAddr::from((
            [127, 0, 0, 1],
            silent.local_addr().expect("silent addr").port(),
        ));
        let adopted = ak_net::holepunch::bind_dual_stack_udp(0)
            .or_else(|_| std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))))
            .expect("probe socket bind");
        let adopted_port = adopted.local_addr().expect("adopted addr").port();
        let round_socket = adopted.try_clone().expect("round clone");
        let ordered = vec![candidate];
        let plan =
            probe_dial_round_on(round_socket, ordered.len(), ordered.clone(), |_, _, _| {}).await;
        assert_eq!(plan.head, None, "a silent candidate observes nothing");
        assert_eq!(plan.order, ordered, "fail-open keeps the static order");
        // The buffered probe packet: probe-shaped, small, from the
        // adopted port.
        let mut buffer = [0u8; 1500];
        let (len, from) = silent
            .recv_from(&mut buffer)
            .expect("a probe packet arrived");
        assert!(
            ak_net::holepunch::is_probe_packet(&buffer[..len]),
            "the punch-side packet is probe-shaped"
        );
        assert!(len <= 64, "the packet is small: {len} bytes");
        assert_eq!(
            from.port(),
            adopted_port,
            "the round left from the adopted port"
        );
    }

    /// The punch loop's happy path: every dialable candidate is punched
    /// with a small probe-shaped packet (never the all-zero reply nonce —
    /// the echo responder would not answer it), cadence-bounded within
    /// the window, and the report counts what was sent. CR-P1-06: the
    /// two-phase TTL split shows in the report — on a TTL-capable host
    /// the first phase's IPv4 punches ride the low TTL (loopback delivers
    /// them regardless — local delivery decrements nothing, the expiry is
    /// the matrix's akinet hop's to prove) and the backup phase keeps
    /// sending after it. CR-P1-08: on a host without per-packet TTL the
    /// SOCKET-LEVEL fallback arms (low_ttl_socket) and the socket's own
    /// TTL is RESTORED after the phase; only when no low-TTL send exists
    /// at all does the window degrade to normal TTL — and then the punch
    /// START is delayed (start_delayed), which the happy path never is.
    #[tokio::test]
    async fn punch_candidates_punches_every_candidate_with_bounded_packets() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_addr = target.local_addr().expect("target addr");
        let puncher = std::net::UdpSocket::bind("127.0.0.1:0").expect("puncher bind");
        let puncher_port = puncher.local_addr().expect("puncher addr").port();
        let ttl_before = puncher.ttl().expect("read the puncher's TTL");
        let ttl_witness = puncher.try_clone().expect("ttl witness handle");
        let started = Instant::now();
        let stop = AtomicBool::new(false);
        // Same-family mock candidates: a real one plus undialable ones
        // (unspecified / port 0) that must be skipped (the pure
        // `punch_targets` test pins the selection rule itself).
        let candidates = vec![
            SocketAddr::from(([0, 0, 0, 0], 7000)),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            target_addr,
        ];
        let mut report = None;
        {
            let report_slot = &mut report;
            punch_candidates(puncher, &candidates, &stop, |r| *report_slot = Some(r)).await;
        }
        let report = report.expect("the report fires at loop end");
        target
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("target timeout");
        let mut punches = 0usize;
        loop {
            let mut buffer = [0u8; 1500];
            match target.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    assert!(
                        ak_net::holepunch::is_probe_packet(&buffer[..len]),
                        "the punch packet is probe-shaped"
                    );
                    assert!(len <= 64, "the punch packet is small: {len} bytes");
                    assert_ne!(
                        &buffer[8..len],
                        &[0u8; 16][..],
                        "a punch never carries the all-zero reply nonce"
                    );
                    assert_eq!(from.port(), puncher_port, "the punch left the given socket");
                    punches += 1;
                }
                Err(_) => break,
            }
        }
        assert!(punches >= 1, "at least one punch reached the candidate");
        assert!(
            punches <= (PUNCH_WINDOW.as_millis() / PUNCH_INTERVAL.as_millis()) as usize + 1,
            "the punch count is cadence-bounded: {punches}"
        );
        assert!(
            started.elapsed() >= PUNCH_WINDOW - Duration::from_millis(250),
            "the loop ran its window: {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < PUNCH_WINDOW + Duration::from_secs(1),
            "the loop never overruns its window: {:?}",
            started.elapsed()
        );
        assert!(
            !report.start_delayed,
            "this host has a low-TTL send (or only v6 targets): no delayed start: {report:?}"
        );
        // CR-P1-08: the socket's own TTL is ALWAYS back to its original
        // value after the run — the socket-level fallback must never leak
        // the low TTL past its phase (the witness handle reads the same
        // OS socket the puncher moved into the loop).
        assert_eq!(
            ttl_witness.ttl().expect("read the TTL after the run"),
            ttl_before,
            "the socket TTL is restored after the punch"
        );
        // The two-phase TTL split (CR-P1-06 + CR-P1-08). One candidate
        // per tick; the low phase covers ~PUNCH_LOW_TTL_PHASE/PUNCH_
        // INTERVAL ticks.
        let phase_ticks = (PUNCH_LOW_TTL_PHASE.as_millis() / PUNCH_INTERVAL.as_millis()) as usize;
        if ak_net::holepunch::per_packet_ttl_supported() {
            assert!(!report.low_ttl_socket, "per-packet host: {report:?}");
            assert!(
                report.low_ttl_packets > 0,
                "the first phase punches ride the low TTL: {report:?}"
            );
            assert!(
                report.low_ttl_packets <= phase_ticks + 1,
                "the low-TTL phase is phase-bounded: {report:?}"
            );
            assert!(
                report.packets_sent > report.low_ttl_packets,
                "the normal-TTL backup phase keeps punching after the low phase: {report:?}"
            );
        } else if report.low_ttl_socket {
            // CR-P1-08, the socket-level fallback (the Windows shape; a
            // plain v4 socket takes IP_TTL everywhere): same two-phase
            // counts, but the low TTL rode the whole socket.
            assert!(
                report.low_ttl_packets > 0,
                "the fallback's first phase rides the low TTL: {report:?}"
            );
            assert!(
                report.low_ttl_packets <= phase_ticks + 1,
                "the low-TTL phase is phase-bounded: {report:?}"
            );
            assert!(
                report.packets_sent > report.low_ttl_packets,
                "the normal-TTL backup phase keeps punching after the low phase: {report:?}"
            );
        } else {
            assert_eq!(
                report.low_ttl_packets, 0,
                "no low-TTL send at all: normal TTL for the whole window: {report:?}"
            );
            assert!(
                report.packets_sent >= punches,
                "every punch still went out at normal TTL: {report:?}"
            );
        }
    }

    /// The punch loop's early stop: the shared flag (set by the accept
    /// path on the first successful data-plane connection) ends the loop
    /// well inside the window. (Multi-thread runtime: the test thread
    /// blocks on the target socket while the punch task must keep
    /// running.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn punch_candidates_stops_early_on_the_flag() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_addr = target.local_addr().expect("target addr");
        let puncher = std::net::UdpSocket::bind("127.0.0.1:0").expect("puncher bind");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let task = tokio::spawn(async move {
            let mut report = None;
            punch_candidates(puncher, &[target_addr], &flag, |r| report = Some(r)).await;
            report
        });
        // Wait for the first punch, then flip the early-stop flag.
        target
            .set_read_timeout(Some(Duration::from_millis(2_000)))
            .expect("target timeout");
        let mut buffer = [0u8; 1500];
        target
            .recv_from(&mut buffer)
            .expect("the first punch arrives");
        stop.store(true, Ordering::Relaxed);
        let started = Instant::now();
        let report = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the loop ends promptly after the flag")
            .expect("join")
            .expect("report");
        assert!(report.stopped_early, "the report names the early stop");
        assert!(
            started.elapsed() < PUNCH_WINDOW,
            "the early stop beat the window"
        );
    }

    /// The punch target selection rule, pure: undialable entries (port 0
    /// of the pre-CR sender blobs, unspecified) drop out, the v4 socket
    /// skips v6 candidates, the v6-family (dual-stack) socket maps v4
    /// candidates, dedupe preserves order, and the per-tick cap holds.
    #[test]
    fn punch_targets_filter_bridge_and_cap() {
        let v4_local: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        // Port-0 and unspecified entries drop; the v6 candidate drops (a
        // v4 socket cannot dial it); the mapped v4 normalizes.
        assert_eq!(
            punch_targets(
                v4_local,
                &addrs(&[
                    "0.0.0.0:7000",
                    "203.0.113.7:0",
                    "[2001:db8::1]:7000",
                    "[::ffff:198.51.100.9]:7100",
                    "198.51.100.9:7100",
                ])
            ),
            addrs(&["198.51.100.9:7100"])
        );
        let v6_local: SocketAddr = "[::]:9000".parse().unwrap();
        // The dual-stack socket bridges plain v4 into mapped form and
        // keeps native v6 candidates.
        assert_eq!(
            punch_targets(
                v6_local,
                &addrs(&["203.0.113.7:7000", "[2001:db8::1]:7000"])
            ),
            addrs(&["[::ffff:203.0.113.7]:7000", "[2001:db8::1]:7000"])
        );
        // The per-tick cap: only the first PUNCH_MAX_TARGETS entries are
        // even considered.
        let many: Vec<SocketAddr> = (1..=PUNCH_MAX_TARGETS + 4)
            .map(|i| SocketAddr::from(([192, 0, 2, 1], 30_000 + i as u16)))
            .collect();
        assert_eq!(punch_targets(v4_local, &many).len(), PUNCH_MAX_TARGETS);
    }

    // -- CR-P1-06 + CR-P3-09 (review 2026-11-11 §4.5): the low-TTL punch
    //    phase, the reflexive opener and the shared interleaved dial pass --

    /// The per-datagram TTL decision matrix, pure: low TTL for IPv4
    /// targets (plain AND `::ffff:` mapped — the dual-stack socket's v4
    /// form) inside the first phase on a capable host; normal TTL for
    /// IPv6 (no NAT), for the backup phase, and for the Windows
    /// degradation. Plus the tunable sanity: the TTL sits in the review's
    /// 2–3 band, and the low phase comfortably covers the sender's
    /// post-pairing pause.
    #[test]
    fn punch_send_ttl_decision_and_constants() {
        let v4: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let mapped_v4: SocketAddr = "[::ffff:203.0.113.7]:7000".parse().unwrap();
        let v6: SocketAddr = "[2001:db8::1]:7000".parse().unwrap();
        assert_eq!(punch_send_ttl(v4, true, true), Some(PUNCH_LOW_TTL));
        assert_eq!(punch_send_ttl(mapped_v4, true, true), Some(PUNCH_LOW_TTL));
        // IPv6 never rides the low TTL (no NAT — normal hop limit).
        assert_eq!(punch_send_ttl(v6, true, true), None);
        // The backup phase and the unsupported-host degradation are
        // normal TTL.
        assert_eq!(punch_send_ttl(v4, false, true), None);
        assert_eq!(punch_send_ttl(v4, true, false), None);
        // The tunables: the review's 2–3 band, and the phase covers the
        // sender's pause with skew margin.
        assert!(
            (2..=3).contains(&PUNCH_LOW_TTL),
            "PUNCH_LOW_TTL sits in the review band: {PUNCH_LOW_TTL}"
        );
        assert!(
            PUNCH_LOW_TTL_PHASE >= SENDER_DIAL_PAUSE + Duration::from_millis(400),
            "the low phase must outlast the sender pause plus skew: {:?} vs {:?}",
            PUNCH_LOW_TTL_PHASE,
            SENDER_DIAL_PAUSE
        );
        assert!(DIAL_STAGGER >= Duration::from_millis(100));
    }

    /// CR-P3-09: the reflexive-first reorder — public candidates strictly
    /// precede the LAN block, relative order stable inside each group.
    #[test]
    fn reflexive_first_order_leads_with_public_candidates() {
        let ordered = addrs(&[
            "192.168.1.5:7000",  // LAN
            "203.0.113.7:7000",  // public (reflexive)
            "127.0.0.1:7000",    // LAN (loopback is private)
            "198.51.100.9:7000", // public
            "10.0.0.7:7000",     // LAN
        ]);
        assert_eq!(
            reflexive_first_order(ordered),
            addrs(&[
                "203.0.113.7:7000",
                "198.51.100.9:7000",
                "192.168.1.5:7000",
                "127.0.0.1:7000",
                "10.0.0.7:7000",
            ])
        );
        // An all-LAN list (same-subnet transfer) is unchanged.
        let lan_only = addrs(&["192.168.1.5:7000", "127.0.0.1:7000"]);
        assert_eq!(reflexive_first_order(lan_only.clone()), lan_only);
    }

    /// A tiny attempt-outcome vocabulary for the pass tests below: the
    /// marker IS the address that attempted, plus a class word.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MockOutcome {
        Ok(SocketAddr),
        Retry(SocketAddr),
        Bad(SocketAddr),
    }

    fn mock_verdict(outcome: &MockOutcome) -> AttemptVerdict {
        match outcome {
            MockOutcome::Ok(_) => AttemptVerdict::Done,
            MockOutcome::Retry(_) => AttemptVerdict::Retryable("mock retry".to_string()),
            MockOutcome::Bad(_) => AttemptVerdict::Fatal,
        }
    }

    /// CR-P3-09's acceptance shape, loopback: with unreachable private
    /// candidates ahead in the STATIC order, a live reflexive candidate
    /// still connects first — the pass reorders public-first and the
    /// never-connecting private attempts (pending forever) do not block
    /// the winner; total time is bounded by one stagger slot, not the
    /// candidate count.
    #[tokio::test(start_paused = true)]
    async fn staggered_pass_connects_via_reflexive_without_waiting_for_privates() {
        let reflexive: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let privates: Vec<SocketAddr> = (1..=6)
            .map(|i| SocketAddr::from(([192, 168, 0, i], 7000)))
            .collect();
        // STATIC order: privates first (the pre-CR dial order), reflexive
        // dead last — the pass's reorder must leapfrog it to the front.
        let mut order = privates.clone();
        order.push(reflexive);
        let started = tokio::time::Instant::now();
        let outcome = staggered_dial_pass(
            &order,
            None,
            false, // self-bound path: Happy-Eyeballs stagger
            |addr, marks, _budget| async move {
                if addr == reflexive {
                    // A live leg: handshake up quickly, transfer done.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    marks.mark(&Progress::Connected {
                        peer: "mock".to_string(),
                    });
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    MockOutcome::Ok(addr)
                } else {
                    // An unreachable private candidate: never connects
                    // (the old serial loop burned its full budget here).
                    std::future::pending::<()>().await;
                    MockOutcome::Retry(addr)
                }
            },
            mock_verdict,
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(
            outcome.done,
            Some((reflexive, MockOutcome::Ok(reflexive))),
            "the reflexive candidate carried the pass: {:?}",
            outcome.done
        );
        assert!(
            outcome.failures.is_empty(),
            "the cancelled privates never recorded failures: {:?}",
            outcome.failures
        );
        assert!(
            elapsed < DIAL_STAGGER * 2,
            "connection time is independent of the private candidate count: {elapsed:?}"
        );
    }

    /// The pass's budget rule: the leading PUBLIC candidate (the
    /// reflexive leg, no observation needed) dials under the bounded
    /// observed budget; a LAN-first order without any public candidate
    /// keeps the full task budget for slot 0.
    #[tokio::test(start_paused = true)]
    async fn staggered_pass_budget_rule_leading_public_gets_the_short_budget() {
        let public: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.5:7000".parse().unwrap();
        // Public leads the static order: the short hot-mapping budget.
        let order = vec![public, lan];
        let outcome = staggered_dial_pass(
            &order,
            None,
            true, // sequential: one attempt is enough to observe it
            |_addr, _marks, budget| async move { budget },
            |budget: &Duration| {
                if *budget == OBSERVED_DIAL_BUDGET {
                    AttemptVerdict::Done
                } else {
                    AttemptVerdict::Fatal
                }
            },
        )
        .await;
        assert!(
            outcome.done.is_some(),
            "public-first slot gets the observed budget: {outcome:?}"
        );
        // LAN leads, but the observed head is the public candidate: the
        // head is pinned first and carries the short budget.
        let order = vec![lan, public];
        let outcome = staggered_dial_pass(
            &order,
            Some(public),
            true,
            |_addr, _marks, budget| async move { budget },
            |budget: &Duration| {
                if *budget == OBSERVED_DIAL_BUDGET {
                    AttemptVerdict::Done
                } else {
                    AttemptVerdict::Fatal
                }
            },
        )
        .await;
        assert!(
            outcome.done.is_some(),
            "the pinned head gets the observed budget: {outcome:?}"
        );
        // An order with NO public candidate and no observation: slot 0
        // is LAN — full budget (legacy semantics).
        let lan2: SocketAddr = "10.0.0.7:7000".parse().unwrap();
        let order = vec![lan, lan2];
        let outcome = staggered_dial_pass(
            &order,
            None,
            true,
            |_addr, _marks, budget| async move { budget },
            |budget: &Duration| {
                if *budget == TASK_CONNECT_BUDGET {
                    AttemptVerdict::Done
                } else {
                    AttemptVerdict::Fatal
                }
            },
        )
        .await;
        assert!(
            outcome.done.is_some(),
            "an all-LAN order with no observation keeps the full budget: {outcome:?}"
        );
    }

    /// Sequential mode (the adopted-socket path): attempts run strictly
    /// one at a time — the next attempt starts only after the previous
    /// future finished (concurrent QUIC endpoints on one OS socket are
    /// forbidden), and the retryable/fatal classification behaves like
    /// the old serial loop.
    #[tokio::test(start_paused = true)]
    async fn staggered_pass_sequential_runs_one_attempt_at_a_time() {
        let first: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let second: SocketAddr = "192.168.1.5:7000".parse().unwrap();
        let timeline: Arc<StdMutex<Vec<(&'static str, u128)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let t0 = tokio::time::Instant::now();
        let log = Arc::clone(&timeline);
        let outcome = staggered_dial_pass(
            &[first, second],
            None,
            true,
            move |addr, _marks, _budget| {
                let log = Arc::clone(&log);
                async move {
                    if let Ok(mut log) = log.lock() {
                        log.push(("start", t0.elapsed().as_millis()));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(mut log) = log.lock() {
                        log.push(("end", t0.elapsed().as_millis()));
                    }
                    if addr == first {
                        MockOutcome::Retry(addr)
                    } else {
                        MockOutcome::Ok(addr)
                    }
                }
            },
            mock_verdict,
        )
        .await;
        let timeline = timeline.lock().expect("timeline").clone();
        assert_eq!(
            outcome.done,
            Some((second, MockOutcome::Ok(second))),
            "the second attempt won: {:?}",
            outcome.done
        );
        assert_eq!(
            outcome.failures,
            vec![(first, "mock retry".to_string())],
            "the first attempt's retryable failure is recorded"
        );
        assert_eq!(timeline.len(), 4, "two start/end pairs: {timeline:?}");
        assert!(
            timeline[2].1 >= timeline[1].1,
            "attempt 2 started only after attempt 1 finished: {timeline:?}"
        );
    }

    /// A Fatal attempt (post-connect failure / declined) ends the pass
    /// immediately — another address cannot fix it — and the outcome
    /// carries the failing address for the adapter's own message.
    #[tokio::test(start_paused = true)]
    async fn staggered_pass_fatal_stops_the_pass() {
        let a: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let b: SocketAddr = "192.168.1.5:7000".parse().unwrap();
        let outcome = staggered_dial_pass(
            &[a, b],
            None,
            false,
            |addr, _marks, _budget| async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                if addr == a {
                    MockOutcome::Bad(addr)
                } else {
                    MockOutcome::Ok(addr)
                }
            },
            mock_verdict,
        )
        .await;
        assert!(outcome.done.is_none(), "a fatal attempt never yields Done");
        assert_eq!(outcome.fatal, Some((a, MockOutcome::Bad(a))));
        assert!(outcome.failures.is_empty());
    }

    /// The reflexive opener: after [`SENDER_DIAL_PAUSE`] it sends exactly
    /// ONE probe-shaped packet per PUBLIC candidate from the ADOPTED
    /// socket's port (LAN candidates are skipped), and without an
    /// adopted socket it sends nothing (but still pauses — uniform
    /// adapter timing).
    /// The run-43304434327 regression pin: RFC1918 reflexive targets
    /// MUST pass the opener filter (double NAT makes them real), while
    /// loopback (plain and native v6) stays out.
    #[test]
    fn opener_targets_keeps_rfc1918_and_drops_loopback() {
        let kept = opener_targets(&[
            "10.10.0.1:50684".parse().unwrap(),
            "192.168.5.9:7000".parse().unwrap(),
            "[2001:77:1::2]:57961".parse().unwrap(),
            "[::ffff:10.0.0.1]:7000".parse().unwrap(),
        ]);
        assert_eq!(
            kept.len(),
            4,
            "RFC1918 v4, global v6 and mapped v4 all stay"
        );
        let dropped = opener_targets(&[
            "127.0.0.1:7000".parse().unwrap(),
            "[::1]:7000".parse().unwrap(),
        ]);
        assert!(dropped.is_empty(), "plain loopback and ::1 are filtered");
    }

    ///
    /// Routing note: the "public" target is a `::ffff:127.0.0.1` mapped
    /// loopback — `prefers_lan` sees the v6 variant (not v4-loopback,
    /// not v6-loopback ⇒ public) while the dual-stack socket's send
    /// still delivers on loopback, so the test neither depends on
    /// external routing nor leaves the host. Real public addresses are
    /// covered by the pure selection rule above.
    #[tokio::test(start_paused = true)]
    async fn sender_reflexive_opener_pauses_and_punches_public_targets() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_port = target.local_addr().expect("target addr").port();
        let adopted = ak_net::holepunch::bind_dual_stack_udp(0)
            .or_else(|_| std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))))
            .expect("adopted bind");
        let adopted_port = adopted.local_addr().expect("adopted addr").port();
        let public: SocketAddr = format!("[::ffff:127.0.0.1]:{target_port}")
            .parse()
            .expect("mapped loopback parses");
        let lan: SocketAddr = SocketAddr::from(([127, 0, 0, 1], target_port));
        let started = tokio::time::Instant::now();
        let sent = sender_reflexive_opener(Some(&adopted), &[lan, public]).await;
        let elapsed = started.elapsed();
        assert_eq!(sent, 1, "exactly the public candidate was opened");
        assert!(
            elapsed >= SENDER_DIAL_PAUSE,
            "the opener waits the post-pairing pause: {elapsed:?}"
        );
        assert!(
            elapsed < SENDER_DIAL_PAUSE + Duration::from_millis(150),
            "and nothing else delays it: {elapsed:?}"
        );
        target
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("target timeout");
        let mut buffer = [0u8; 1500];
        let (len, from) = target.recv_from(&mut buffer).expect("the opener packet");
        assert!(
            ak_net::holepunch::is_probe_packet(&buffer[..len]),
            "the opener packet is probe-shaped"
        );
        assert_eq!(from.port(), adopted_port, "it left the adopted socket");
        // The target socket RECEIVING the datagram is the proof it was
        // aimed at the public (mapped) candidate — `from` names the
        // sender (plain-v4 presentation of the dual-stack socket), and
        // the family bridging itself is pinned by
        // `punch_targets_filter_bridge_and_cap`.
        // No adopted socket: the pause still runs, nothing is sent.
        let started = tokio::time::Instant::now();
        let sent = sender_reflexive_opener(None, &[public]).await;
        assert_eq!(sent, 0);
        assert!(started.elapsed() >= SENDER_DIAL_PAUSE);
    }

    // -- CR-P1-07 / CR-P1-08 (review 2026-11-13 §4.4): the pairing-time
    //    opener + keepalive, the adopted-socket probe fold and the
    //    socket-level TTL guard ------------------------------------------

    /// The socket-level TTL guard: arm sets the socket's own TTL low,
    /// restore puts the ORIGINAL value back, and a guard dropped while
    /// armed restores too (the punch loop's early-stop path). Genuinely
    /// exercises `IP_TTL` on every host (a plain v4 socket takes it on
    /// Unix and Windows alike).
    #[test]
    fn socket_ttl_guard_arms_restores_and_restores_on_drop() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("guard socket bind");
        let original = socket.ttl().expect("read the socket TTL");
        let mut guard = SocketTtlGuard::new(&socket);
        assert!(!guard.armed(), "a fresh guard is unarmed");
        guard.arm(PUNCH_LOW_TTL).expect("arm the low TTL");
        assert!(guard.armed());
        assert_eq!(
            socket.ttl().expect("read the TTL while armed"),
            PUNCH_LOW_TTL,
            "the armed guard's low TTL is visible on the socket"
        );
        guard.restore().expect("restore");
        assert!(!guard.armed());
        assert_eq!(
            socket.ttl().expect("read the TTL after restore"),
            original,
            "the ORIGINAL TTL is back"
        );
        // Idempotence: restore/step on an unarmed guard are no-ops.
        guard
            .restore()
            .expect("restore of an unarmed guard is a no-op");
        // The Drop path (early stop mid-phase): never leave a low TTL on
        // the socket the QUIC listener is about to own.
        {
            let mut guard = SocketTtlGuard::new(&socket);
            guard.arm(PUNCH_LOW_TTL).expect("arm for the drop path");
        }
        assert_eq!(
            socket.ttl().expect("read the TTL after the armed drop"),
            original,
            "Drop restores an armed guard"
        );
    }

    /// The bounded stop-poll sleep: it sleeps exactly to its deadline
    /// with the flag down, and a flag flipped MID-sleep returns within
    /// one STOP_POLL_INTERVAL of the flip (never the full window).
    #[tokio::test(start_paused = true)]
    async fn sleep_polling_stop_wakes_on_deadline_and_on_the_flag() {
        let stop = Arc::new(AtomicBool::new(false));
        // Deadline path: the full window elapses, returning false.
        let started = tokio::time::Instant::now();
        assert!(!sleep_polling_stop(started + Duration::from_secs(2), &stop).await);
        assert_eq!(started.elapsed(), Duration::from_secs(2));
        // Already-set flag: returns immediately.
        stop.store(true, Ordering::Relaxed);
        let started = tokio::time::Instant::now();
        assert!(sleep_polling_stop(started + Duration::from_secs(2), &stop).await);
        assert_eq!(started.elapsed(), Duration::ZERO);
        // Mid-sleep flip (a spawned task flips at t=1s of a 5s window):
        // the return lands within one poll chunk of the flip.
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            flag.store(true, Ordering::Relaxed);
        });
        let started = tokio::time::Instant::now();
        assert!(sleep_polling_stop(started + Duration::from_secs(5), &stop).await);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1)
                && elapsed <= Duration::from_secs(1) + STOP_POLL_INTERVAL,
            "the stop is noticed within one poll chunk of the flip: {elapsed:?}"
        );
    }

    /// The pairing-time opener + keepalive (CR-P1-07): the opener half
    /// fires at SENDER_DIAL_PAUSE (`on_opener` carries its packet count),
    /// the keepalive then ticks every OPENER_KEEPALIVE_INTERVAL from the
    /// ADOPTED socket's port toward the SAME public targets, and the stop
    /// flag ends it — promptly, inside one poll chunk, with the honest
    /// report. Virtual time (start_paused) advances the ticks.
    #[tokio::test(start_paused = true)]
    async fn pairing_opener_runs_then_keepalives_until_stopped() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_port = target.local_addr().expect("target addr").port();
        let adopted = ak_net::holepunch::bind_dual_stack_udp(0)
            .or_else(|_| std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))))
            .expect("adopted bind");
        let adopted_port = adopted.local_addr().expect("adopted addr").port();
        let adopted = Arc::new(adopted);
        // Mapped loopback passes the opener's non-loopback filter (see
        // the standalone opener test); plain loopback is the filtered
        // LAN candidate.
        let public: SocketAddr = format!("[::ffff:127.0.0.1]:{target_port}")
            .parse()
            .expect("mapped loopback parses");
        let lan: SocketAddr = SocketAddr::from(([127, 0, 0, 1], target_port));
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = Arc::clone(&stop);
        let opener_seen = Arc::new(StdMutex::new(None::<usize>));
        let seen_slot = Arc::clone(&opener_seen);
        let adopted_task = Arc::clone(&adopted);
        let task = tokio::spawn(async move {
            sender_pairing_opener(
                Some(adopted_task.as_ref()),
                &[lan, public],
                &task_stop,
                move |packets| {
                    if let Ok(mut slot) = seen_slot.lock() {
                        *slot = Some(packets);
                    }
                },
            )
            .await
        });
        // The opener fires at the pause; let two keepalive ticks pass
        // (the task's tick grid starts at ITS pause completion — the
        // 500 ms tail keeps the second tick clear of this wake).
        tokio::time::sleep(
            SENDER_DIAL_PAUSE + 2 * OPENER_KEEPALIVE_INTERVAL + Duration::from_millis(500),
        )
        .await;
        let seen: Option<usize> = *opener_seen.lock().expect("slot");
        assert_eq!(
            seen,
            Some(1),
            "on_opener fired with the opener's packet count"
        );
        stop.store(true, Ordering::Relaxed);
        let report = tokio::time::timeout(STOP_POLL_INTERVAL * 4, task)
            .await
            .expect("the stop ends the keepalive promptly")
            .expect("join");
        assert!(report.stopped_early, "the flag ended it: {report:?}");
        assert!(!report.budget_elapsed);
        assert_eq!(report.opener_packets, 1);
        assert!(
            report.keepalive_ticks >= 2,
            "two ticks fired before the stop: {report:?}"
        );
        assert!(
            report.keepalive_packets >= report.keepalive_ticks,
            "one packet per target per tick: {report:?}"
        );
        // The target observed opener + keepalive packets, all from the
        // adopted socket's port (the mapping the QUIC dial rides).
        target
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("target timeout");
        let mut packets = 0usize;
        loop {
            let mut buffer = [0u8; 1500];
            match target.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    assert!(
                        ak_net::holepunch::is_probe_packet(&buffer[..len]),
                        "keepalive packets are probe-shaped"
                    );
                    assert_eq!(from.port(), adopted_port, "from the adopted socket");
                    packets += 1;
                }
                Err(_) => break,
            }
        }
        assert!(
            packets >= 3,
            "the opener packet plus two keepalive ticks reached the target: {packets}"
        );
        // The adopted socket's own TTL is untouched by the keepalive (the
        // opener's low TTL rides per-packet/socket data, restored inline).
        if let Ok(ttl) = adopted.ttl() {
            assert!(
                ttl != PUNCH_LOW_TTL || !ak_net::holepunch::per_packet_ttl_supported(),
                "no low TTL leaks past the opener on per-packet hosts: {ttl}"
            );
        }
    }

    /// The keepalive's honest expiry: with the stop flag never set, the
    /// total budget ends it (`budget_elapsed`), after ~budget/interval
    /// ticks — virtual time makes the 10-minute run instant.
    #[tokio::test(start_paused = true)]
    async fn pairing_opener_keepalive_ends_at_the_budget() {
        let target = std::net::UdpSocket::bind("127.0.0.1:0").expect("target bind");
        let target_port = target.local_addr().expect("target addr").port();
        let adopted = ak_net::holepunch::bind_dual_stack_udp(0)
            .or_else(|_| std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))))
            .expect("adopted bind");
        let public: SocketAddr = format!("[::ffff:127.0.0.1]:{target_port}")
            .parse()
            .expect("mapped loopback parses");
        let stop = AtomicBool::new(false);
        let report = sender_pairing_opener(Some(&adopted), &[public], &stop, |_| {}).await;
        assert!(report.budget_elapsed, "the budget ended it: {report:?}");
        assert!(!report.stopped_early);
        assert_eq!(report.opener_packets, 1);
        let expected_ticks =
            (OPENER_KEEPALIVE_BUDGET.as_secs() / OPENER_KEEPALIVE_INTERVAL.as_secs()) as usize;
        assert!(
            report.keepalive_ticks + 1 == expected_ticks
                || report.keepalive_ticks == expected_ticks,
            "ticks ≈ budget / interval (the deadline arm preempts the last tick): {report:?}"
        );
        // Without an adopted socket the opener count is 0 and the
        // keepalive never runs (nothing to hold), on_opener still fires.
        let mut opener_count = None;
        let report = sender_pairing_opener(None, &[public], &stop, |packets| {
            opener_count = Some(packets)
        })
        .await;
        assert_eq!(opener_count, Some(0), "the event fires with zero");
        assert_eq!(report, OpenerKeepaliveReport::default());
    }

    /// CR-P1-07 改法 2: the adopted-socket probe round folds into the
    /// pairing-time ProbeOutcome vocabulary — an observed head is a
    /// Success, silence is one bounded reason per candidate in the
    /// static order. Pure.
    #[test]
    fn round_probe_outcome_folds_heads_and_silence() {
        let ordered = addrs(&["192.168.1.5:7000", "203.0.113.7:7000"]);
        let observed: SocketAddr = "203.0.113.7:7000".parse().unwrap();
        let plan = ProbeDialOrder {
            order: ordered.clone(),
            head: Some(observed),
        };
        match round_probe_outcome(&ordered, &plan) {
            ProbeOutcome::Success { address } => assert_eq!(address, observed),
            other => panic!("an observed head folds to Success: {other:?}"),
        }
        let silent = ProbeDialOrder {
            order: ordered.clone(),
            head: None,
        };
        match round_probe_outcome(&ordered, &silent) {
            ProbeOutcome::AllFailed { failures } => {
                assert_eq!(
                    failures.len(),
                    ordered.len(),
                    "one failure row per static candidate"
                );
                assert_eq!(failures[0].candidate, ordered[0], "static order kept");
                assert!(
                    failures[0].reason.len() < 64,
                    "the reason is bounded: {:?}",
                    failures[0].reason
                );
            }
            other => panic!("silence folds to AllFailed: {other:?}"),
        }
    }
}
