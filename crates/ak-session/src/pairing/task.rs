//! Minimal task pairing, wave 28 (v1 product model).
//!
//! One random code = one task with exactly two sides:
//!
//! * the **initiator** generates a short-TTL code through
//!   [`TaskPairing::create_task_code`], declaring the fixed direction
//!   ("I send" / "I receive") and reporting this device's address
//!   candidates as an opaque [endpoint blob](encode_endpoint_blob) the
//!   registry relays verbatim (`max_redeems = 1`: one code pairs once;
//!   a second redeem is refused with the dedicated `REDEEM_EXHAUSTED`
//!   business error);
//! * the **joiner** types the code into [`TaskPairing::join_task_code`],
//!   which redeems it carrying the joiner's own endpoint blob and an
//!   explicit `my_direction` (the user's intent, an FFI contract — never
//!   a local default);
//! * once the frozen pairing transcript reaches `PairReady`, both sides
//!   hold the peer's blob and parse it into a [`PeerEndpoint`]
//!   (`candidates`, `name`, `direction`) — the owner's blob travels in
//!   the `PairCommit` extension down to the joiner, the joiner's in the
//!   `InviteRedeemOffered` extension up to the owner;
//! * each side then runs one connectivity test,
//!   [`probe_peer`], over the peer candidates — LAN candidates first
//!   (via [`ak_net::classify`]) — which is the same UDP probe exchange the
//!   hole-punch flow uses, so both sides probing simultaneously confirm
//!   each other;
//! * the task lives until either side disconnects
//!   ([`TaskPairing::disconnect`] revokes the code), the peer's data
//!   plane closes on us, or this side's data-plane link times out —
//!   every end maps to exactly one [`TaskEvent::TaskDisconnected`] with
//!   its [`TaskDisconnectReason`]. A registry/control-connection drop is
//!   deliberately NOT a task end (wave 30, review P2-08): the data plane
//!   is independent of the registry.
//!
//! # Wire shape of the endpoint blob (this layer's codec)
//!
//! The blob is a ≤512-byte canonical JSON object —
//! `{"cands":["ip:port",...],"name":"...","dir":"send|recv"}` — encoded
//! and decoded only here; the registry treats it as opaque bytes
//! (`ak_signal::protocol_v2::EndpointBlob`, relayed verbatim on the
//! wave-28 extension sections of `InviteClaim`, `InviteRedeem`/
//! `InviteRedeemOffered` and `PairCommit`).
//!
//! # History: the parallel seam (agent A, wave 28)
//!
//! This module was written in parallel with the `ak-signal` frame
//! extension. The three [`wire`] adapters below were the seam; they are
//! now bound to the landed frames (claim with blob + `max_redeems = 1`,
//! redeem with blob, peer blob off the relayed frames) and kept as the
//! single place the wire details are named, so future frame changes
//! touch only them.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ak_core::policy::PathKind;
use ak_identity::pairing_txn::{random_id16, VerifiedPairing};
use ak_net::classify::{self, Net};
use ak_net::holepunch;
use ak_signal::protocol_v2::{CodeId, PairingId};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::pairing::error::PairFlowError;
use crate::pairing::orchestrator::{CreatedInvite, PairingOrchestrator, RedeemOffer};

/// Default lifetime of a task code: 10 minutes (the v1 "short TTL").
pub const TASK_CODE_TTL_SECS: u32 = 600;
/// Hard wire bound of the endpoint blob (the registry's passthrough cap).
pub const ENDPOINT_BLOB_MAX_BYTES: usize = 512;
/// Bound on the candidate list inside one blob.
pub const ENDPOINT_BLOB_MAX_CANDIDATES: usize = 16;
/// Name bound inside the blob — the registry Hello's wire-legal maximum.
const NAME_MAX_BYTES: usize = ak_signal::protocol_v2::MAX_NAME_BYTES;

/// Broadcast bound of the task event stream.
const TASK_EVENT_CAPACITY: usize = 64;

/// Fixed reason strings of [`CandidateFailure`] (bounded by construction).
mod reasons {
    pub const UNSPECIFIED: &str = "unspecified addresses are not dialable";
    pub const SILENT: &str = "no probe answer within the window";
    pub const SOCKET: &str = "probe socket error";
}

// ---- direction ---------------------------------------------------------------

/// Which side of the paired task this device is on. The direction is fixed
/// by the initiator's declaration; the joiner declares its own explicit
/// direction (`join_task_code`'s `my_direction` — the user's intent at the
/// UI, an API contract the FFI caller owns). A mismatched pair is refused
/// by the initiator's owner loop **before the server commits** (see
/// [`TaskPairing::spawn_owner_loop`]; the joiner then observes the
/// server's unified "pairing target unavailable" answer, the code stays
/// redeemable and nothing is journaled or booked on either side). The
/// post-completion [`TaskError::DirectionConflict`] remains as defense in
/// depth against a pre-gate initiator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskDirection {
    /// This device sends the payload.
    Send,
    /// This device receives the payload.
    Recv,
}

impl TaskDirection {
    /// The wire tag inside the endpoint blob.
    fn wire_tag(self) -> &'static str {
        match self {
            TaskDirection::Send => "send",
            TaskDirection::Recv => "recv",
        }
    }

    /// The direction's mirror (the other side of the same task).
    pub fn complement(self) -> TaskDirection {
        match self {
            TaskDirection::Send => TaskDirection::Recv,
            TaskDirection::Recv => TaskDirection::Send,
        }
    }

    fn from_wire_tag(tag: &str) -> Option<Self> {
        match tag {
            "send" => Some(TaskDirection::Send),
            "recv" => Some(TaskDirection::Recv),
            _ => None,
        }
    }
}

// ---- endpoint ----------------------------------------------------------------

/// The peer's endpoint as delivered by the pairing wire: where to reach it
/// (`candidates`, probed LAN-first) and who it is (`name`, `direction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEndpoint {
    /// Dialable `ip:port` candidates (IPv6 in `[..]:port` form).
    pub candidates: Vec<SocketAddr>,
    /// The peer's wire-legal display name.
    pub name: String,
    /// The peer's side of the task.
    pub direction: TaskDirection,
}

// ---- errors ------------------------------------------------------------------

/// Failure of a task-pairing operation. Every variant is bounded; blob
/// rejections carry fixed strings, never echo blob bytes.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// The underlying pairing flow failed (claim/redeem/revoke/…).
    #[error(transparent)]
    Flow(#[from] PairFlowError),
    /// The pairing completed but no endpoint blob arrived with it — the
    /// `ak-signal` `EndpointBlob` passthrough (agent A, wave 28) is not on
    /// the wire yet, or the peer predates it.
    #[error("no endpoint blob arrived with the pairing")]
    EndpointMissing,
    /// The encoded blob exceeds the 512-byte wire bound.
    #[error("endpoint blob is {size} bytes over the {ENDPOINT_BLOB_MAX_BYTES}-byte bound")]
    BlobTooLarge {
        /// Encoded size.
        size: usize,
    },
    /// The endpoint data is malformed or oversized (fixed reason).
    #[error("endpoint rejected: {0}")]
    BadEndpoint(&'static str),
    /// The two sides declared the same direction; a task is exactly one
    /// sender and one receiver.
    #[error("task direction conflict: initiator {initiator:?} vs joiner {joiner:?}")]
    DirectionConflict {
        /// What the initiator's blob declared.
        initiator: TaskDirection,
        /// What the joiner declared.
        joiner: TaskDirection,
    },
    /// No usable local candidate was supplied for the endpoint report.
    #[error("no dialable local candidate to report")]
    NoCandidates,
}

// ---- endpoint blob codec -----------------------------------------------------

/// Encode one endpoint into the canonical ≤512-byte JSON blob
/// `{"cands":[...],"name":"...","dir":"send|recv"}` (fixed key order,
/// compact separators — byte-stable across platforms and runs, so the
/// bytes one side attaches are the bytes the other side decodes).
pub fn encode_endpoint_blob(endpoint: &PeerEndpoint) -> Result<Vec<u8>, TaskError> {
    if endpoint.candidates.is_empty() {
        return Err(TaskError::NoCandidates);
    }
    if endpoint.candidates.len() > ENDPOINT_BLOB_MAX_CANDIDATES {
        return Err(TaskError::BadEndpoint("more than 16 candidates"));
    }
    if endpoint.name.len() > NAME_MAX_BYTES {
        return Err(TaskError::BadEndpoint("name over the wire-legal bound"));
    }
    let mut out = String::with_capacity(64 + endpoint.candidates.len() * 24);
    out.push_str("{\"cands\":[");
    for (index, candidate) in endpoint.candidates.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&candidate.to_string());
        out.push('"');
    }
    out.push_str("],\"name\":\"");
    append_json_escaped(&mut out, &endpoint.name);
    out.push_str("\",\"dir\":\"");
    out.push_str(endpoint.direction.wire_tag());
    out.push_str("\"}");
    let bytes = out.into_bytes();
    if bytes.len() > ENDPOINT_BLOB_MAX_BYTES {
        return Err(TaskError::BlobTooLarge { size: bytes.len() });
    }
    Ok(bytes)
}

/// Decode and validate a blob received off the wire. Strict: exactly the
/// three known keys (any order, no duplicates), canonical string escapes
/// accepted, nothing trailing, bounded sizes — a hostile blob never reads
/// more than the wire bound's worth of structure off the input.
pub fn decode_endpoint_blob(bytes: &[u8]) -> Result<PeerEndpoint, TaskError> {
    if bytes.len() > ENDPOINT_BLOB_MAX_BYTES {
        return Err(TaskError::BlobTooLarge { size: bytes.len() });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TaskError::BadEndpoint("not utf-8"))?;
    let mut parser = BlobParser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.expect_byte(b'{')?;
    let mut candidates: Option<Vec<SocketAddr>> = None;
    let mut name: Option<String> = None;
    let mut direction: Option<TaskDirection> = None;
    let mut seen_first = false;
    loop {
        parser.skip_ws();
        if parser.peek() == Some(b'}') {
            parser.advance()?;
            break;
        }
        if seen_first {
            parser.expect_byte(b',')?;
            parser.skip_ws();
        }
        let key = parser.parse_key()?;
        parser.expect_byte(b':')?;
        parser.skip_ws();
        match key.as_str() {
            "cands" => {
                if candidates.is_some() {
                    return Err(TaskError::BadEndpoint("duplicate cands key"));
                }
                candidates = Some(parser.parse_candidates()?);
            }
            "name" => {
                if name.is_some() {
                    return Err(TaskError::BadEndpoint("duplicate name key"));
                }
                name = Some(parser.parse_string(NAME_MAX_BYTES, "name too long")?);
            }
            "dir" => {
                if direction.is_some() {
                    return Err(TaskError::BadEndpoint("duplicate dir key"));
                }
                let tag = parser.parse_string(8, "dir tag too long")?;
                direction = Some(
                    TaskDirection::from_wire_tag(&tag)
                        .ok_or(TaskError::BadEndpoint("dir is not send/recv"))?,
                );
            }
            _ => return Err(TaskError::BadEndpoint("unknown key")),
        }
        seen_first = true;
    }
    // Strict end: only the closing brace ends the blob — no trailing
    // whitespace or junk (canonical encoders never emit it).
    if parser.pos != parser.bytes.len() {
        return Err(TaskError::BadEndpoint("trailing bytes"));
    }
    let candidates = candidates.ok_or(TaskError::BadEndpoint("missing cands key"))?;
    let name = name.ok_or(TaskError::BadEndpoint("missing name key"))?;
    let direction = direction.ok_or(TaskError::BadEndpoint("missing dir key"))?;
    if candidates.len() > ENDPOINT_BLOB_MAX_CANDIDATES {
        return Err(TaskError::BadEndpoint("more than 16 candidates"));
    }
    Ok(PeerEndpoint {
        candidates,
        name,
        direction,
    })
}

/// Append `text` as a JSON string body (the caller supplies the quotes),
/// escaping the mandatory set (`"` `\`) and every control byte; all other
/// UTF-8 passes through verbatim — canonical on every platform.
fn append_json_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// Strict one-pass parser for the blob grammar (see [`decode_endpoint_blob`]).
struct BlobParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl BlobParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Result<u8, TaskError> {
        let byte = self
            .peek()
            .ok_or(TaskError::BadEndpoint("blob ended mid-value"))?;
        self.pos += 1;
        Ok(byte)
    }

    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            self.pos += 1;
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), TaskError> {
        let byte = self.advance()?;
        if byte != expected {
            return Err(TaskError::BadEndpoint("unexpected byte"));
        }
        Ok(())
    }

    /// One object key: a quoted string restricted to printable ASCII (the
    /// codec's keys are fixed ASCII words).
    fn parse_key(&mut self) -> Result<String, TaskError> {
        self.expect_byte(b'"')?;
        let mut key = String::new();
        loop {
            match self.advance()? {
                b'"' => break,
                byte if (0x20..0x7F).contains(&byte) => key.push(byte as char),
                _ => return Err(TaskError::BadEndpoint("key is not printable ascii")),
            }
            if key.len() > 8 {
                return Err(TaskError::BadEndpoint("key too long"));
            }
        }
        Ok(key)
    }

    /// One JSON string value, decoding the escapes the encoder emits.
    /// `byte_budget` bounds the decoded length.
    fn parse_string(
        &mut self,
        byte_budget: usize,
        too_long: &'static str,
    ) -> Result<String, TaskError> {
        self.expect_byte(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.advance()?;
            match byte {
                b'"' => break,
                b'\\' => match self.advance()? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let digit = self.advance()?;
                            let value = (digit as char)
                                .to_digit(16)
                                .ok_or(TaskError::BadEndpoint("bad \\u escape"))?;
                            code = code * 16 + value;
                        }
                        let ch = char::from_u32(code)
                            .ok_or(TaskError::BadEndpoint("bad \\u code point"))?;
                        out.push(ch);
                    }
                    _ => return Err(TaskError::BadEndpoint("unknown escape")),
                },
                byte if byte < 0x20 => {
                    return Err(TaskError::BadEndpoint("raw control byte in string"))
                }
                byte if byte < 0x80 => out.push(byte as char),
                byte => {
                    // Multi-byte UTF-8: `advance` already consumed the lead
                    // byte; collect its continuation bytes.
                    let mut buf = vec![byte];
                    let extra = utf8_continuation_count(byte)
                        .ok_or(TaskError::BadEndpoint("bad utf-8 lead byte"))?;
                    for _ in 0..extra {
                        buf.push(self.advance()?);
                    }
                    let text = std::str::from_utf8(&buf)
                        .map_err(|_| TaskError::BadEndpoint("bad utf-8"))?;
                    out.push_str(text);
                }
            }
            if out.len() > byte_budget {
                return Err(TaskError::BadEndpoint(too_long));
            }
        }
        Ok(out)
    }

    /// The `cands` array: `"ip:port"` strings parsed into `SocketAddr`s.
    fn parse_candidates(&mut self) -> Result<Vec<SocketAddr>, TaskError> {
        self.expect_byte(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.advance()?;
            return Ok(out);
        }
        loop {
            self.skip_ws();
            let text = self.parse_string(64, "candidate too long")?;
            let addr: SocketAddr = text
                .parse()
                .map_err(|_| TaskError::BadEndpoint("candidate is not ip:port"))?;
            if addr.ip().is_unspecified() {
                return Err(TaskError::BadEndpoint("candidate is unspecified"));
            }
            out.push(addr);
            if out.len() > ENDPOINT_BLOB_MAX_CANDIDATES {
                return Err(TaskError::BadEndpoint("more than 16 candidates"));
            }
            self.skip_ws();
            match self.advance()? {
                b',' => continue,
                b']' => break,
                _ => return Err(TaskError::BadEndpoint("unexpected byte in cands")),
            }
        }
        Ok(out)
    }
}

/// Continuation-byte count of a UTF-8 lead byte (`None` for invalid leads).
fn utf8_continuation_count(lead: u8) -> Option<usize> {
    match lead {
        0xC2..=0xDF => Some(1),
        0xE0..=0xEF => Some(2),
        0xF0..=0xF4 => Some(3),
        _ => None,
    }
}

// ---- probe -------------------------------------------------------------------

/// Tunables of one [`probe_peer`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Interval between probe rounds.
    pub interval: Duration,
    /// Total listen window (both sides probe simultaneously, so one window
    /// observes the peer's probes and ours).
    pub window: Duration,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        ProbeOptions {
            interval: Duration::from_millis(150),
            window: Duration::from_millis(1_500),
        }
    }
}

/// Why one candidate did not answer the probe (fixed strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateFailure {
    /// The candidate that failed.
    pub candidate: SocketAddr,
    /// Fixed, bounded reason.
    pub reason: &'static str,
}

/// Result of one connectivity test against a [`PeerEndpoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// At least one candidate is live. `address` is the LAN-first best
    /// candidate the peer's own probes confirmed (or, failing that, the
    /// address a peer probe was observed from).
    Success {
        /// Where to reach the peer.
        address: SocketAddr,
    },
    /// Every candidate failed; one bounded reason per candidate (in
    /// LAN-first probe order, pre-filtered candidates first).
    AllFailed {
        /// Per-candidate failures.
        failures: Vec<CandidateFailure>,
    },
}

/// Order candidates LAN-first: addresses [`ak_net::classify`] puts on the
/// local network (loopback, link-local, same subnet) probe before internet
/// addresses; the relative order inside each class is preserved.
pub fn order_candidates_lan_first(candidates: &[SocketAddr], nets: &[Net]) -> Vec<SocketAddr> {
    let mut lan = Vec::new();
    let mut rest = Vec::new();
    for candidate in candidates {
        if classify::classify(candidate.ip(), nets) == PathKind::LocalDirect {
            lan.push(*candidate);
        } else {
            rest.push(*candidate);
        }
    }
    lan.extend(rest);
    lan
}

/// One connectivity test against the peer endpoint (see the module docs):
/// a dual-stack UDP socket runs the frozen [`ak_net::holepunch`] probe
/// exchange over the LAN-first ordered candidates for a bounded window.
/// The peer runs the same test simultaneously, so its probe packets are
/// observed here — success needs no application-level echo server.
pub async fn probe_peer(endpoint: &PeerEndpoint, options: ProbeOptions) -> ProbeOutcome {
    let nets = classify::local_nets().unwrap_or_default();
    let mut dialable = Vec::new();
    let mut failures = Vec::new();
    for candidate in &endpoint.candidates {
        if candidate.ip().is_unspecified() {
            failures.push(CandidateFailure {
                candidate: *candidate,
                reason: reasons::UNSPECIFIED,
            });
        } else {
            dialable.push(*candidate);
        }
    }
    if dialable.is_empty() {
        return ProbeOutcome::AllFailed { failures };
    }
    let ordered = order_candidates_lan_first(&dialable, &nets);

    let std_socket = holepunch::bind_dual_stack_udp(0)
        .or_else(|_| {
            std::net::UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
                socket.set_nonblocking(true)?;
                Ok(socket)
            })
        })
        .ok();
    let probe = match std_socket {
        Some(std_socket) => match tokio::net::UdpSocket::from_std(std_socket) {
            Ok(socket) => {
                holepunch::probe(&socket, &ordered, options.interval, options.window).await
            }
            Err(_) => Err(std::io::Error::other("from_std")),
        },
        None => Err(std::io::Error::other("bind")),
    };
    let report = match probe {
        Ok(report) => report,
        Err(_) => return all_failed(ordered, reasons::SOCKET, failures),
    };
    // Best answer: the first candidate in LAN-first order the peer's own
    // probes confirmed reachable; else any observed peer source (a NAT or
    // routing path the candidate list did not name — still connectivity).
    if let Some(hit) = ordered
        .iter()
        .find(|candidate| report.reachable.contains(candidate))
    {
        return ProbeOutcome::Success { address: *hit };
    }
    if let Some(observed) = report.peer_observed_from {
        return ProbeOutcome::Success {
            address: classify::expand_mapped(observed),
        };
    }
    all_failed(ordered, reasons::SILENT, failures)
}

/// Every dialable candidate failed with `reason`, keeping any pre-filter
/// failures collected earlier.
fn all_failed(
    ordered: Vec<SocketAddr>,
    reason: &'static str,
    mut failures: Vec<CandidateFailure>,
) -> ProbeOutcome {
    failures.extend(
        ordered
            .into_iter()
            .map(|candidate| CandidateFailure { candidate, reason }),
    );
    ProbeOutcome::AllFailed { failures }
}

// ---- events ------------------------------------------------------------------

/// Why a paired task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskDisconnectReason {
    /// The peer went away: its data-plane connection closed on us (or its
    /// side reported the task over).
    ByPeer,
    /// The task's DATA plane went away: an idle timeout / link loss on the
    /// paired connection (QUIC keepalive expiry). Wave 30 (review
    /// 2026-11-07, P2-08): a registry/control-connection drop is NOT this
    /// reason — the data plane is independent of the registry, so a task
    /// survives a registry outage until a real data-plane verdict (or a
    /// local/peer action) ends it.
    Network,
    /// This device ended the task ([`TaskPairing::disconnect`]: the code
    /// is revoked server-side, a pending redeem aborts).
    Local,
}

/// One observable task-pairing event (broadcast; see
/// [`TaskPairing::subscribe`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEvent {
    /// A paired task ended; exactly one event per task end, carrying why.
    TaskDisconnected {
        /// The task's invite code (the task identity across both sides).
        code: CodeId,
        /// Why the task ended.
        reason: TaskDisconnectReason,
    },
}

// ---- task pairing ------------------------------------------------------------

/// Tunables of [`TaskPairing`].
#[derive(Debug, Clone)]
pub struct TaskPairingConfig {
    /// Lifetime of freshly created task codes (default 10 minutes).
    pub ttl_secs: u32,
    /// Connectivity-test tunables ([`probe_peer`]).
    pub probe: ProbeOptions,
}

impl Default for TaskPairingConfig {
    fn default() -> Self {
        TaskPairingConfig {
            ttl_secs: TASK_CODE_TTL_SECS,
            probe: ProbeOptions::default(),
        }
    }
}

/// One live task tracked by its creator/joiner (snapshot form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTask {
    /// The task's invite code.
    pub code: CodeId,
    /// The pairing transaction once it exists (learned at this side's
    /// redeem/offer hop).
    pub pairing: Option<PairingId>,
    /// This device's side of the task.
    pub direction: TaskDirection,
}

/// A freshly created task code, ready to display. `Debug` is inherited
/// from [`CreatedInvite`]'s redacted form — the display string is the
/// bearer credential and never reaches logs.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskCode {
    /// The registered invite (display form, expiry, revision …).
    pub invite: CreatedInvite,
    /// The initiator's declared direction.
    pub direction: TaskDirection,
    /// The endpoint blob attached to the claim (retained locally; the wire
    /// copy is what the peer receives).
    pub endpoint_blob: Vec<u8>,
}

impl std::fmt::Debug for TaskCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCode")
            .field("invite", &self.invite)
            .field("direction", &self.direction)
            .field(
                "endpoint_blob",
                &format!("{} bytes", self.endpoint_blob.len()),
            )
            .finish()
    }
}

/// A completed task pairing: the frozen transcript is `Ready` on both
/// sides and this device holds the peer's endpoint.
#[derive(Debug, Clone)]
pub struct TaskPaired {
    /// The pairing transaction.
    pub pairing_id: PairingId,
    /// The task's invite code.
    pub code_id: CodeId,
    /// The peer as delivered by the wire.
    pub peer: PeerEndpoint,
    /// This device's side of the task.
    pub direction: TaskDirection,
    /// The verified pairing (transcript + both proofs), for the trust book.
    pub verified: VerifiedPairing,
}

/// The v1 task pairing surface (see the module docs). Clonable; wraps one
/// [`PairingOrchestrator`] (and through it the device's single
/// [`RegistryClient`](crate::pairing::RegistryClient)).
#[derive(Clone)]
pub struct TaskPairing {
    orch: PairingOrchestrator,
    config: TaskPairingConfig,
    events: broadcast::Sender<TaskEvent>,
    tasks: Arc<StdMutex<HashMap<CodeId, ActiveTask>>>,
}

impl TaskPairing {
    /// Wrap an orchestrator with explicit tunables.
    pub fn open(orch: PairingOrchestrator, config: TaskPairingConfig) -> Self {
        let (events, _) = broadcast::channel(TASK_EVENT_CAPACITY);
        TaskPairing {
            orch,
            config,
            events,
            tasks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Wrap an orchestrator with the v1 defaults.
    pub fn with_defaults(orch: PairingOrchestrator) -> Self {
        Self::open(orch, TaskPairingConfig::default())
    }

    /// Subscribe to the task event stream
    /// ([`TaskEvent::TaskDisconnected`]).
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    /// The wrapped orchestrator (invite listing, recovery, owner loop).
    pub fn orchestrator(&self) -> &PairingOrchestrator {
        &self.orch
    }

    /// Snapshot of the live tasks.
    pub fn active_tasks(&self) -> Vec<ActiveTask> {
        self.tasks
            .lock()
            .expect("task pairing lock")
            .values()
            .cloned()
            .collect()
    }

    // ---- initiator -----------------------------------------------------------

    /// Generate one task code with the configured TTL (default 10 minutes),
    /// declaring this device's direction and reporting its candidates
    /// (endpoint blob on the claim; `max_redeems = 1`). One code pairs
    /// exactly once; [`TaskPairing::disconnect`] revokes what is left of
    /// it.
    pub async fn create_task_code(
        &self,
        direction: TaskDirection,
        candidates: &[SocketAddr],
    ) -> Result<TaskCode, TaskError> {
        self.create_task_code_with_ttl(direction, candidates, self.config.ttl_secs)
            .await
    }

    /// [`TaskPairing::create_task_code`] with an explicit TTL in seconds.
    pub async fn create_task_code_with_ttl(
        &self,
        direction: TaskDirection,
        candidates: &[SocketAddr],
        ttl_secs: u32,
    ) -> Result<TaskCode, TaskError> {
        let endpoint = PeerEndpoint {
            candidates: validated_candidates(candidates)?,
            name: self.orch.client().name().to_string(),
            direction,
        };
        let blob = encode_endpoint_blob(&endpoint)?;
        let invite = wire::claim_task_code(&self.orch, random_id16(), ttl_secs, &blob).await?;
        self.tasks.lock().expect("task pairing lock").insert(
            invite.code_id,
            ActiveTask {
                code: invite.code_id,
                pairing: None,
                direction,
            },
        );
        Ok(TaskCode {
            invite,
            direction,
            endpoint_blob: blob,
        })
    }

    // ---- joiner ---------------------------------------------------------------

    /// Redeem a typed task code carrying this device's candidates and
    /// explicit direction (`my_direction` is the user's intent at the UI —
    /// the caller, not any local guess, declares which side of the task
    /// this device is on). The direction rule is enforced by the
    /// initiator's owner loop at the relayed offer, **before the server's
    /// irreversible commit**: a mismatched (or endpoint-less) declaration
    /// aborts the pending transaction there, so this call fails with the
    /// server's unified "pairing target unavailable" flow error while the
    /// code stays redeemable and neither side journals or books anything.
    /// [`TaskError::DirectionConflict`] after completion remains only as
    /// defense in depth against a pre-gate initiator (review 2026-11-06,
    /// P1-05). On `PairReady` both sides hold each other's endpoint —
    /// this side gets the initiator's blob off the relayed `PairCommit`.
    pub async fn join_task_code(
        &self,
        code_str: &str,
        candidates: &[SocketAddr],
        my_direction: TaskDirection,
    ) -> Result<TaskPaired, TaskError> {
        let endpoint = PeerEndpoint {
            candidates: validated_candidates(candidates)?,
            name: self.orch.client().name().to_string(),
            direction: my_direction,
        };
        let blob = encode_endpoint_blob(&endpoint)?;
        let redeem = wire::redeem_with_blob(&self.orch, code_str, &blob).await?;
        self.complete_task(redeem.verified, my_direction, redeem.owner_endpoint)
    }

    /// Owner-side completion: the daemon's owner loop finished a pairing
    /// for one of this device's task codes; extract the joiner's endpoint
    /// (the blob the registry forwarded inside `InviteRedeemOffered`) and
    /// register the live task. `my_direction` is the direction this
    /// device declared at [`TaskPairing::create_task_code`].
    pub fn complete_owner_pairing(
        &self,
        verified: &VerifiedPairing,
        my_direction: TaskDirection,
    ) -> Result<TaskPaired, TaskError> {
        let pairing_id = verified.record().transcript.pairing_id;
        let joiner_blob = wire::redeemer_endpoint_blob(self.orch.client(), pairing_id);
        self.complete_task(verified.clone(), my_direction, joiner_blob)
    }

    /// Shared completion of either side: parse the peer blob, enforce the
    /// direction rule, register the active task.
    fn complete_task(
        &self,
        verified: VerifiedPairing,
        my_direction: TaskDirection,
        peer_blob: Option<Vec<u8>>,
    ) -> Result<TaskPaired, TaskError> {
        let transcript = verified.record().transcript;
        let code_id = transcript.code_id;
        let pairing_id = transcript.pairing_id;
        let blob = peer_blob.ok_or(TaskError::EndpointMissing)?;
        let peer = decode_endpoint_blob(&blob)?;
        // Exactly one sender and one receiver per task: the peer's blob
        // must declare the mirror of this side's direction (order of the
        // pair is irrelevant to the equality, so one check covers both
        // sides — on the joiner the peer is the initiator and vice versa).
        if peer.direction == my_direction {
            return Err(TaskError::DirectionConflict {
                initiator: peer.direction,
                joiner: my_direction,
            });
        }
        let paired = TaskPaired {
            pairing_id,
            code_id,
            peer,
            direction: my_direction,
            verified,
        };
        self.tasks.lock().expect("task pairing lock").insert(
            code_id,
            ActiveTask {
                code: code_id,
                pairing: Some(pairing_id),
                direction: my_direction,
            },
        );
        Ok(paired)
    }

    // ---- disconnect -----------------------------------------------------------

    /// End one task locally: revoke the code server-side (a pending redeem
    /// of it aborts with the unified `PAIR_UNAVAILABLE`) and emit
    /// [`TaskEvent::TaskDisconnected`] with reason
    /// [`Local`](TaskDisconnectReason::Local). The end event fires at most
    /// once per task; revoking a code this process no longer holds (paired
    /// and consumed, or created before a restart) still ends the task.
    pub async fn disconnect(&self, code_id: CodeId) -> Result<(), TaskError> {
        if let Err(err) = self.orch.revoke(code_id).await {
            let tolerated = match &err {
                PairFlowError::Local(message) if message.contains("unknown invite code") => {
                    // Not held in this process (consumed pairing, adopted
                    // code): the local task end still proceeds.
                    true
                }
                PairFlowError::Unavailable { .. } => {
                    // Already gone server-side (expired/swept): same.
                    true
                }
                _ => false,
            };
            if !tolerated {
                return Err(err.into());
            }
        }
        self.finish_task(code_id, TaskDisconnectReason::Local);
        Ok(())
    }

    /// Report that the peer's data-plane connection went away (the channel
    /// closed under us): emits [`TaskDisconnectReason::ByPeer`].
    pub fn report_peer_disconnect(&self, code_id: CodeId) {
        self.finish_task(code_id, TaskDisconnectReason::ByPeer);
    }

    /// Report a data-plane idle timeout / link loss (QUIC keepalive
    /// expiry) of the task's OWN connection: emits
    /// [`TaskDisconnectReason::Network`]. This is a data-plane verdict —
    /// a registry/control-connection drop must never be routed here
    /// (wave-30 P2-08: the data plane is independent of the registry).
    pub fn report_connection_lost(&self, code_id: CodeId) {
        self.finish_task(code_id, TaskDisconnectReason::Network);
    }

    /// Install this pairing's pre-commit redeem-offer gate on the wrapped
    /// orchestrator (P1-05, wave 29). Every owner loop spawned from that
    /// orchestrator afterwards — including production embedders that use
    /// [`PairingOrchestrator::spawn_owner_loop_verified`] directly instead
    /// of [`TaskPairing::spawn_owner_loop`] — consults the gate before
    /// answering an `InviteRedeemOffered`.
    ///
    /// Wave 30 (review 2026-11-07, R29-P2-05): the installed closure
    /// captures ONLY the shared task table, never the `TaskPairing`. The
    /// gate lives inside the orchestrator, so a closure holding the whole
    /// pairing would form a strong reference ring
    /// (gate → pairing → orchestrator → gate) that outlives every
    /// external handle; the bare table keeps the gate's footprint to
    /// exactly the data [`veto_task_offer`] reads.
    pub fn install_redeem_offer_gate(&self) {
        let tasks = Arc::clone(&self.tasks);
        self.orch
            .set_redeem_offer_gate(Arc::new(move |offer| Self::veto_task_offer(&tasks, offer)));
    }

    /// Pre-commit veto over one redeem offer (P1-05, wave 29; installed by
    /// [`TaskPairing::install_redeem_offer_gate`] as the orchestrator's
    /// redeem-offer gate). For a code this `TaskPairing` created, the
    /// joiner's endpoint blob must be present and decodable and must
    /// declare the mirror of the code's direction — otherwise the offer
    /// is refused before the owner flow starts, so the server never
    /// commits, the aborted redeem does not consume the code's single
    /// budget, and neither side journals or books the pairing. Codes not
    /// created here (plain invites, adopted codes) carry no task rule
    /// and pass through untouched.
    fn veto_task_offer(
        tasks: &StdMutex<HashMap<CodeId, ActiveTask>>,
        offer: &RedeemOffer,
    ) -> Result<(), PairFlowError> {
        let my_direction = tasks
            .lock()
            .expect("task pairing lock")
            .get(&offer.code_id)
            .map(|task| task.direction);
        let Some(mine) = my_direction else {
            return Ok(());
        };
        let blob = offer
            .redeemer_endpoint
            .as_deref()
            .ok_or_else(|| PairFlowError::Local("task redeem without an endpoint blob".into()))?;
        let peer = decode_endpoint_blob(blob)
            .map_err(|err| PairFlowError::Local(format!("task redeem endpoint rejected: {err}")))?;
        if peer.direction == mine {
            return Err(PairFlowError::Local(format!(
                "task direction conflict: initiator {mine:?} vs joiner {:?}",
                peer.direction
            )));
        }
        Ok(())
    }

    /// Task-aware owner loop (wave 28): answers redeems of this device's
    /// codes through the frozen owner flow, and on every committed
    /// pairing parses the joiner's endpoint blob and registers the live
    /// task — the direction the code was created with is remembered, so
    /// the complementary-declaration rule is enforced on this side too.
    ///
    /// Wave 29 (review 2026-11-06, P1-05): the rule is enforced **before
    /// the owner flow starts**, through the orchestrator's redeem-offer
    /// gate. The joiner's endpoint blob arrives with the relayed
    /// `InviteRedeemOffered` — the first frame of the owner side — while
    /// the initiator's own direction only reaches the joiner inside
    /// `PairCommit` (the authoritative commit point), so the initiator is
    /// the only side that can run the complementary check pre-commit.
    /// A refusing gate aborts the pending transaction server-side:
    /// nothing is committed (an aborted attempt does not consume the
    /// single redeem), nothing is journaled or booked on either side, and
    /// the joiner's still-waiting redeem fails with the unified
    /// "pairing target unavailable" answer. Codes created by other
    /// processes (`adopt_persisted_code`) are not task codes; complete
    /// those manually through [`TaskPairing::complete_owner_pairing`].
    pub fn spawn_owner_loop(&self) -> JoinHandle<()> {
        let pairing = self.clone();
        self.install_redeem_offer_gate();
        self.orch
            .spawn_owner_loop_verified(Some(Arc::new(move |verified| {
                let code_id = verified.record().transcript.code_id;
                let my_direction = pairing
                    .tasks
                    .lock()
                    .expect("task pairing lock")
                    .get(&code_id)
                    .map(|task| task.direction);
                match my_direction {
                    Some(direction) => {
                        if let Err(err) = pairing.complete_owner_pairing(&verified, direction) {
                            log::warn!(
                                "task pairing completion failed ({}): {err}",
                                hex16(&code_id)
                            );
                        }
                    }
                    None => {
                        // Not one of this TaskPairing's task codes (a plain
                        // invite redeemed): the pairing itself stands.
                        log::debug!(
                            "task pairing: pairing {} is not a task code of this session",
                            hex16(&code_id)
                        );
                    }
                }
            })))
    }

    // Wave 30 (review 2026-11-07, P2-08): the former registry-connection
    // watcher — which ended every live task with
    // `TaskDisconnectReason::Network` when the CONTROL connection
    // dropped — is GONE. Task data planes are independent of the
    // registry: a paired transfer keeps running through a registry
    // outage, so a control-plane disconnect is not a task verdict. Tasks
    // end only on data-plane verdicts (`report_connection_lost`,
    // `report_peer_disconnect`) or local action (`disconnect`);
    // reconnecting the registry remains the daemon's policy decision.

    /// One connectivity test against a paired peer's endpoint, using the
    /// configured [`ProbeOptions`].
    pub async fn probe_peer(&self, peer: &PeerEndpoint) -> ProbeOutcome {
        probe_peer(peer, self.config.probe).await
    }

    /// Remove the task (if live) and emit its single end event.
    fn finish_task(&self, code_id: CodeId, reason: TaskDisconnectReason) {
        let removed = self
            .tasks
            .lock()
            .expect("task pairing lock")
            .remove(&code_id);
        if removed.is_some() {
            let _ = self.events.send(TaskEvent::TaskDisconnected {
                code: code_id,
                reason,
            });
        }
    }
}

/// Lowercase hex of a 16-byte id for log lines (bounded, non-secret).
fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Validate a caller-supplied candidate list: non-empty, no unspecified
/// addresses, order preserved.
fn validated_candidates(candidates: &[SocketAddr]) -> Result<Vec<SocketAddr>, TaskError> {
    if candidates.is_empty() {
        return Err(TaskError::NoCandidates);
    }
    if candidates.len() > ENDPOINT_BLOB_MAX_CANDIDATES {
        return Err(TaskError::BadEndpoint("more than 16 candidates"));
    }
    if candidates.iter().any(|c| c.ip().is_unspecified()) {
        return Err(TaskError::BadEndpoint("candidate is unspecified"));
    }
    Ok(candidates.to_vec())
}

// ==== wave-28 wire adapters: ak-signal `EndpointBlob` (agent A, landed) =======
//
// The registry's wave-28 frame extension carries opaque ≤512-byte
// `EndpointBlob` values on `InviteClaim` (owner settings + `max_redeems`,
// stored on the code row), `InviteRedeem` (joiner settings, forwarded to
// the owner inside `InviteRedeemOffered`) and `PairCommit` (owner
// settings, relayed down to the joiner at the authoritative commit
// point). These adapters are the single place this module names those
// wire details; everything above speaks blobs and endpoints.
mod wire {
    use ak_signal::protocol_v2::{EndpointBlob, PairingId};

    use crate::pairing::client::RegistryClient;
    use crate::pairing::error::PairFlowError;
    use crate::pairing::orchestrator::{CreatedInvite, PairingOrchestrator, TaskRedeem};

    /// Claim (register) one task code, attaching the initiator's endpoint
    /// blob and `max_redeems = 1` (one code pairs exactly once).
    pub(super) async fn claim_task_code(
        orch: &PairingOrchestrator,
        request_id: [u8; 16],
        ttl_secs: u32,
        blob: &[u8],
    ) -> Result<CreatedInvite, PairFlowError> {
        orch.create_task_invite(request_id, ttl_secs, EndpointBlob(blob.to_vec()))
            .await
    }

    /// Redeem a task code, attaching the joiner's endpoint blob, drive the
    /// transcript to `PairReady`, and hand back the initiator's blob the
    /// registry relayed inside `PairCommit`.
    pub(super) async fn redeem_with_blob(
        orch: &PairingOrchestrator,
        code_str: &str,
        blob: &[u8],
    ) -> Result<TaskRedeem, PairFlowError> {
        orch.redeem_task(code_str, EndpointBlob(blob.to_vec()))
            .await
    }

    /// The peer's endpoint blob off the owner side: the joiner's settings
    /// the registry forwarded inside `InviteRedeemOffered` (stored by the
    /// client before the offer was dispatched; taken exactly once).
    pub(super) fn redeemer_endpoint_blob(
        client: &RegistryClient,
        pairing_id: PairingId,
    ) -> Option<Vec<u8>> {
        client.take_endpoint_blob(pairing_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port)
    }

    fn sample_endpoint() -> PeerEndpoint {
        PeerEndpoint {
            candidates: vec![addr(192, 168, 1, 23, 52000), addr(1, 2, 3, 4, 52001)],
            name: "alice-laptop".into(),
            direction: TaskDirection::Send,
        }
    }

    // ---- codec ----------------------------------------------------------------

    #[test]
    fn endpoint_blob_encodes_to_the_frozen_canonical_bytes() {
        let blob = encode_endpoint_blob(&sample_endpoint()).unwrap();
        assert_eq!(
            String::from_utf8(blob.clone()).unwrap(),
            "{\"cands\":[\"192.168.1.23:52000\",\"1.2.3.4:52001\"],\"name\":\"alice-laptop\",\"dir\":\"send\"}"
        );
        assert!(blob.len() <= ENDPOINT_BLOB_MAX_BYTES);
    }

    #[test]
    fn endpoint_blob_roundtrip_is_byte_exact_through_the_wire() {
        // What one side encodes, the other decodes — and re-encoding the
        // decoded endpoint reproduces the identical passthrough bytes
        // (both directions of a pairing see the same blob).
        let blob = encode_endpoint_blob(&sample_endpoint()).unwrap();
        let decoded = decode_endpoint_blob(&blob).unwrap();
        assert_eq!(decoded, sample_endpoint());
        let reencoded = encode_endpoint_blob(&decoded).unwrap();
        assert_eq!(reencoded, blob, "codec must be canonical");

        // The mirrored endpoint (the other side of the same task) rides
        // the same wire shape with the mirrored direction.
        let mut mirrored = sample_endpoint();
        mirrored.direction = TaskDirection::Recv;
        mirrored.name = "bob-phone".into();
        let peer_blob = encode_endpoint_blob(&mirrored).unwrap();
        assert_eq!(
            decode_endpoint_blob(&peer_blob).unwrap().direction,
            TaskDirection::Recv
        );
    }

    #[test]
    fn endpoint_blob_survives_name_escapes_and_ipv6() {
        let endpoint = PeerEndpoint {
            candidates: vec!["[2001:db8::1]:5201".parse().unwrap()],
            name: "quote\" back\\ nl\n ünïcode".into(),
            direction: TaskDirection::Recv,
        };
        let blob = encode_endpoint_blob(&endpoint).unwrap();
        assert_eq!(decode_endpoint_blob(&blob).unwrap(), endpoint);
        // Canonical round trip.
        assert_eq!(
            encode_endpoint_blob(&decode_endpoint_blob(&blob).unwrap()).unwrap(),
            blob
        );
    }

    #[test]
    fn endpoint_blob_rejects_oversize_and_malformed_input() {
        // Oversize: 16 max-length IPv6 candidates + a long name blows the
        // 512-byte bound.
        let fat_candidate: SocketAddr = "[ffff:ffff:ffff:ffff:ffff:ffff:255.255.255.255]:65535"
            .parse()
            .unwrap();
        let fat = PeerEndpoint {
            candidates: vec![fat_candidate; 16],
            name: "n".repeat(NAME_MAX_BYTES),
            direction: TaskDirection::Send,
        };
        match encode_endpoint_blob(&fat) {
            Err(TaskError::BlobTooLarge { size }) => {
                assert!(size > ENDPOINT_BLOB_MAX_BYTES)
            }
            other => panic!("oversize blob must be refused: {other:?}"),
        }

        // Decode-side: over-bound input refused before any parse.
        let big = vec![b'{'; ENDPOINT_BLOB_MAX_BYTES + 1];
        assert!(matches!(
            decode_endpoint_blob(&big),
            Err(TaskError::BlobTooLarge { .. })
        ));

        let good = encode_endpoint_blob(&sample_endpoint()).unwrap();
        let bad_variants: Vec<(Vec<u8>, &'static str)> = vec![
            (b"{}".to_vec(), "missing keys"),
            (b"not json".to_vec(), "garbage"),
            (b"{\"cands\":[]}".to_vec(), "missing keys"),
            (
                b"{\"cands\":[\"1.2.3.4:5\"],\"name\":\"x\",\"dir\":\"both\"}".to_vec(),
                "bad dir",
            ),
            (
                b"{\"cands\":[\"1.2.3.4\"],\"name\":\"x\",\"dir\":\"send\"}".to_vec(),
                "candidate without port",
            ),
            (
                b"{\"cands\":[\"0.0.0.0:5\"],\"name\":\"x\",\"dir\":\"send\"}".to_vec(),
                "unspecified candidate",
            ),
            (
                b"{\"cands\":[\"1.2.3.4:5\"],\"name\":\"x\",\"dir\":\"send\",\"extra\":1}".to_vec(),
                "unknown key",
            ),
            (
                b"{\"cands\":[\"1.2.3.4:5\"],\"cands\":[\"1.2.3.4:6\"],\"name\":\"x\",\"dir\":\"send\"}"
                    .to_vec(),
                "duplicate key",
            ),
            (
                b"{\"cands\":[\"1.2.3.4:5\"],\"name\":\"x\",\"dir\":\"send\"} ".to_vec(),
                "trailing bytes",
            ),
        ];
        for (bytes, why) in bad_variants {
            assert!(
                decode_endpoint_blob(&bytes).is_err(),
                "must refuse {why}: {bytes:?}"
            );
        }
        // The good blob still decodes (sanity against an over-strict
        // parser) and the key ORDER does not matter on the wire.
        let reordered =
            b"{\"dir\":\"send\",\"name\":\"alice-laptop\",\"cands\":[\"192.168.1.23:52000\",\"1.2.3.4:52001\"]}";
        assert_eq!(decode_endpoint_blob(reordered).unwrap(), sample_endpoint());
        let _ = good;
    }

    #[test]
    fn direction_complement_and_decode_rules() {
        assert_eq!(TaskDirection::Send.complement(), TaskDirection::Recv);
        assert_eq!(TaskDirection::Recv.complement(), TaskDirection::Send);
        assert_eq!(
            decode_endpoint_blob(&encode_endpoint_blob(&sample_endpoint()).unwrap())
                .unwrap()
                .direction,
            TaskDirection::Send
        );
    }

    // ---- LAN-first ordering -----------------------------------------------------

    #[test]
    fn candidates_are_ordered_lan_first_and_stable_within_class() {
        let nets = vec![Net {
            addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
            prefix_len: 24,
        }];
        let lan_a = addr(192, 168, 1, 50, 1);
        let lan_b = addr(192, 168, 1, 60, 2);
        let loopback = addr(127, 0, 0, 1, 3);
        let net_a = addr(8, 8, 8, 8, 4);
        let net_b = addr(1, 1, 1, 1, 5);
        // Internet candidates first in the input: the LAN ones must move
        // ahead, keeping their relative input order; loopback classifies
        // local regardless of `nets`.
        let ordered = order_candidates_lan_first(&[net_a, lan_b, loopback, net_b, lan_a], &nets);
        assert_eq!(
            ordered,
            vec![lan_b, loopback, lan_a, net_a, net_b],
            "LAN first, stable within each class"
        );
    }

    // ---- probe ------------------------------------------------------------------

    #[tokio::test]
    async fn probe_peer_confirms_a_live_responder() {
        // A "peer" that runs the same probe exchange toward us: it answers
        // our probe packets with probe packets of its own.
        let peer_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = peer_socket.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buffer = [0u8; 1500];
            loop {
                let (len, source) = match peer_socket.recv_from(&mut buffer).await {
                    Ok(received) => received,
                    Err(_) => return,
                };
                if holepunch::is_probe_packet(&buffer[..len]) {
                    // Echo a probe packet back: exactly what the peer side
                    // of a paired task sends while running its own probe.
                    let nonce = ak_identity::pairing_txn::random_nonce32();
                    let mut tag = [0u8; 16];
                    tag.copy_from_slice(&nonce[..16]);
                    let _ = peer_socket
                        .send_to(&holepunch::probe_packet(&tag), source)
                        .await;
                }
            }
        });
        let endpoint = PeerEndpoint {
            candidates: vec![addr(203, 0, 113, 7, 9), peer_addr],
            name: "peer".into(),
            direction: TaskDirection::Recv,
        };
        let outcome = probe_peer(
            &endpoint,
            ProbeOptions {
                interval: Duration::from_millis(50),
                window: Duration::from_millis(1_200),
            },
        )
        .await;
        responder.abort();
        match outcome {
            ProbeOutcome::Success { address } => assert_eq!(address, peer_addr),
            other => panic!("live responder must probe successfully: {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_peer_reports_every_candidate_on_total_failure() {
        let dead = addr(203, 0, 113, 77, 9); // TEST-NET-3: never answers
        let closed = addr(127, 0, 0, 1, 1); // loopback, nothing listening
        let unspecified = addr(0, 0, 0, 0, 1234);
        let endpoint = PeerEndpoint {
            candidates: vec![dead, closed, unspecified],
            name: "ghost".into(),
            direction: TaskDirection::Send,
        };
        let outcome = probe_peer(
            &endpoint,
            ProbeOptions {
                interval: Duration::from_millis(40),
                window: Duration::from_millis(300),
            },
        )
        .await;
        match outcome {
            ProbeOutcome::AllFailed { failures } => {
                assert_eq!(failures.len(), 3, "one failure per candidate");
                let by_candidate: HashMap<_, _> = failures
                    .iter()
                    .map(|failure| (failure.candidate, failure.reason))
                    .collect();
                assert_eq!(
                    by_candidate.get(&unspecified),
                    Some(&reasons::UNSPECIFIED),
                    "pre-filtered unspecified candidate"
                );
                assert_eq!(
                    by_candidate.get(&dead),
                    Some(&reasons::SILENT),
                    "silent candidate"
                );
                assert_eq!(
                    by_candidate.get(&closed),
                    Some(&reasons::SILENT),
                    "closed-port candidate is silent, not an error"
                );
            }
            other => panic!("dead candidates must all fail: {other:?}"),
        }
    }

    // ---- events bookkeeping (pure) ----------------------------------------------

    #[test]
    fn task_event_carries_the_reason_taxonomy() {
        let reasons = [
            TaskDisconnectReason::ByPeer,
            TaskDisconnectReason::Network,
            TaskDisconnectReason::Local,
        ];
        for reason in reasons {
            let event = TaskEvent::TaskDisconnected {
                code: [7; 16],
                reason,
            };
            assert_eq!(event, event.clone());
        }
    }

    // ---- gate lifecycle (R29-P2-05) ---------------------------------------------

    /// The installed offer gate must capture ONLY the shared task table:
    /// dropping every external `TaskPairing` reference (no owner loop
    /// spawned) releases the table. The pre-fix closure captured the whole
    /// pairing, forming the strong ring gate → pairing → orchestrator →
    /// gate that outlived every handle and pinned the pairing context,
    /// the client cache and the in-memory code secrets.
    #[tokio::test]
    async fn offer_gate_holds_no_reference_ring() {
        use crate::pairing::client::RegistryClient;
        use crate::pairing::orchestrator::{OrchestratorConfig, PairingOrchestrator};

        // A real in-process registry: the client is genuinely connected
        // (the pairing stack cannot be built without one).
        let registry_key = ak_identity::DeviceKeyPair::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind registry port");
        let addr = listener.local_addr().expect("registry addr");
        let store = ak_signal::server_v2::store::RegistryStore::open_in_memory().expect("store");
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let server_key = registry_key.clone();
        let server = tokio::spawn(async move {
            let _ = ak_signal::server_v2::engine::serve_v2(
                listener,
                &server_key,
                store,
                ak_signal::server_v2::Limits::default(),
                stop_rx,
            )
            .await;
        });

        let dir = tempfile::tempdir().expect("id dir");
        let key = ak_identity::DeviceKeyPair::generate();
        let client = RegistryClient::connect(addr, registry_key.public_key(), &key, "lifecycle")
            .await
            .expect("connect");
        let orch =
            PairingOrchestrator::open(client, key, dir.path(), OrchestratorConfig::default())
                .expect("orchestrator");
        let pairing = TaskPairing::with_defaults(orch);
        let tasks_weak = Arc::downgrade(&pairing.tasks);
        pairing.install_redeem_offer_gate();
        drop(pairing);
        assert!(
            tasks_weak.upgrade().is_none(),
            "the gate must not hold a pairing→orchestrator→gate ring past drop"
        );

        let _ = stop_tx.send(true);
        server.abort();
    }
}
