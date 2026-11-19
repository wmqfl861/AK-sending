//! The v2 pairing-transaction engine: one TLS connection per device,
//! the claim/redeem/revoke flows, and the Prepared→Commit→Applied→Ready
//! state machine over the authoritative [`RegistryStore`]
//! (`docs/contracts/pairing-flow.md` §6).
//!
//! Entry points: [`run_v2`] (bind + serve), [`bind_v2`] (only the binding
//! and store opening, for callers that announce readiness in between) and
//! [`serve_v2`] (pre-bound listener, used by embedders and the integration
//! tests). All of them require
//! the `server-persistence` feature — without the authoritative store the
//! server refuses invite operations entirely rather than issuing codes it
//! cannot make unique (`storage-schema.md` §1).
//!
//! Error policy (`pairing-flow.md` §11): rate/quota rejections are
//! immediate (`RATE_LIMITED` / `RESOURCE_LIMIT`) and run **before** the
//! lookup; every other redeem failure — unknown code, wrong code,
//! expired, revoked, owner offline — is deferred to the deadline and
//! surfaces as the identical `PAIR_UNAVAILABLE` (initial
//! `InviteRedeemPending` responses have the same shape for existing and
//! non-existing codes). Business errors always travel as fixed `PairError`
//! frames; `ServerError` keeps only the legacy bounded session codes.
//!
//! What the server verifies (never the MACs — `auth_key` lives only on
//! the endpoints): every message's Ed25519 signature over its frozen
//! domain, the Hello-v2 exporter binding, transcript bindings (registry,
//! code, ids, both device keys, nonces, request, deadline), and
//! transcript-digest consistency across `PairProof`/`PairPrepared`/
//! `PairApplied`.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairTranscript, OWNER_PROOF_DOMAIN, REDEEMER_PROOF_DOMAIN,
};
use ak_identity::DeviceKeyPair;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::protocol_v2::{
    CodeId, CtrlFrame, CtrlMessage, DeviceId, Digest32, EndpointBlob, ErrorScope, PairProof,
    PairingId, PairingState, RedeemRequest, RegistryId, RequestId, Role, SessionId, Signature64,
    StateFilter, CODE_CONFLICT, CURSOR_END, PAIR_UNAVAILABLE, PROTOCOL_UNSUPPORTED, RATE_LIMITED,
    REDEEM_EXHAUSTED, REQUEST_CONFLICT, RESOURCE_LIMIT, REVISION_CONFLICT, STORAGE_UNAVAILABLE,
};
use crate::server_v2::config::{
    Config, Limits, JOURNAL_MAX_ROWS, OUTBOX_DEPTH, PREFIX_48_SHARE_MULTIPLIER,
    PREFIX_56_SHARE_MULTIPLIER, REDEEM_MAX_WINDOW, SOURCE_TABLE_IDLE_TTL, SOURCE_TABLE_MAX_ENTRIES,
    SWEEP_INTERVAL,
};
use crate::server_v2::framing::{read_frame, write_frame, write_message};
use crate::server_v2::ratelimit::{SlidingWindow, TokenBucket};
use crate::server_v2::store::{
    AbortDecision, AppliedDecision, BeginRedeem, ClaimOutcome, DueReport, PairingSnapshot,
    PreparedDecision, RegistryStore, RevokeOutcome,
};
use crate::server_v2::tls;
use crate::server_v2::{hello2, CAPABILITIES};
use crate::util;

/// Legacy bounded server error codes kept by v2 `ServerError` frames
/// (session-plane errors only; invite business errors use `PairError`).
pub const ERR_UNKNOWN_DEVICE: u16 = 1;
pub const ERR_SESSION_LIMIT: u16 = 2;
pub const ERR_RATE_LIMITED: u16 = 3;
pub const ERR_BAD_REQUEST: u16 = 4;
pub const ERR_SESSION_NOT_FOUND: u16 = 5;

type TlsServerStream = tokio_rustls::server::TlsStream<TcpStream>;

// ---- resource bounds (P1-5, CR-P1-03) ------------------------------------------
//
// Every number here is a hard bound on one dimension of connection
// sprawl: a peer that opens sockets and never speaks, one that handshakes
// and never says Hello, one that floods frames, and one that stalls a
// read or write mid-frame each lose their task within the matching
// budget instead of pinning memory and a Tokio task forever.
//
// CR-P1-03 adds the per-source dimension (IPv4 /32, IPv6 /64, the
// constants and their rationale live in [`crate::server_v2::config`]):
// one source can no longer occupy a globally meaningful share of these
// budgets, so a single machine can neither pin the serving cap with
// connect-only sockets nor drain the redeem budgets with self-generated
// identities.
//
// CR-P2-10 closes the remaining one-sided global levers: the redeem
// breaker and the serving cap now shed traffic by per-source FAIR SHARE
// instead of refusing everyone (a /64 source over its share is shed, a
// below-share source is still admitted — 20 sources × 30/min or 32
// sources × 8 connections can no longer lock every other source out),
// and IPv6 sources stack hierarchical /56 and /48 aggregate caps on top
// of the per-/64 caps (see [`v6_parents`]).

/// Budget for one TLS handshake after the TCP accept. 10 s (CR-P1-03,
/// shortened from 30 s): a connect-only socket now parks here at most
/// 10 s per per-source handshaking slot, and 10 s is still orders of
/// magnitude above a loopback/LAN TLS 1.3 handshake (milliseconds) —
/// anything slower is hostile or broken, not slow.
const TLS_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum simultaneously served TLS connections (accepted-and-counted,
/// including handshakes in progress) — the SOFT cap of the CR-P2-10
/// fair-share policy. At or over it, admission continues for sources
/// holding fewer than [`conn_fair_share`] connections (see that fn for
/// the exact invariants) and everyone else is refused, so the ~32
/// sources it takes to fill 256 slots cannot lock a newcomer out.
/// Beyond it a refused connection may still complete the handshake and
/// receive one `ServerError`, but those rejection tasks run under their
/// own small budget ([`MAX_REJECT_TASKS`]) — past that budget the
/// socket is dropped at accept time, so a flood of excess connections
/// can never stack unbounded handshake work (R36-P1-03).
const MAX_LIVE_CONNECTIONS: usize = 256;
/// HARD cap on simultaneously served TLS connections (CR-P2-10): twice
/// the soft cap. Fair-share admissions may overshoot
/// [`MAX_LIVE_CONNECTIONS`] (every below-share source keeps being
/// served while the soft cap is saturated), but never this bound — fds,
/// tasks and memory stay bounded no matter how the load is distributed.
/// 512 = 256 × 2 is coherent with [`FAIR_CONN_FLOOR`]: 256 sources can
/// each hold their full floor (2 connections, a control connection plus
/// the reconnect-overlap one) exactly at the hard cap.
const MAX_LIVE_CONNECTIONS_HARD: usize = 2 * MAX_LIVE_CONNECTIONS;
/// Floor of the per-source fair share of the serving cap (CR-P2-10):
/// even with hundreds of active sources, a well-behaved source — one
/// control connection, a second briefly during reconnect overlap (see
/// [`crate::server_v2::config::MAX_CONNS_PER_SOURCE`]) — can always
/// connect until the hard cap is reached. Below-share admission never
/// refuses a source this small.
const FAIR_CONN_FLOOR: usize = 2;
/// The per-minute window shared by every redeem budget layer (contract
/// §11; the constants in [`crate::server_v2::config`] are all per this
/// window).
const REDEEM_WINDOW: Duration = Duration::from_secs(60);
/// Simultaneous over-cap rejection tasks (the polite one-`ServerError`
/// answer). Small by design: it bounds CPU/sockets/tasks spent on
/// connections that are already past the real serving cap.
const MAX_REJECT_TASKS: usize = 32;
/// Deadline for the client's first frame — the `Hello` answering the
/// `AuthChallenge` this server sends immediately after the handshake.
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(15);
/// Per-frame read and write budget once the first frame arrived.
const FRAME_IO_DEADLINE: Duration = Duration::from_secs(30);
/// Grace period for the reader/writer tasks of a connection to wind down
/// on their own before the handler aborts them.
const TEARDOWN_GRACE: Duration = Duration::from_millis(250);
/// How long a redeemed-attempt idempotency record stays replayable after
/// its own deadline (retries racing the deadline must still dedup).
const REDEEM_REPLAY_GRACE: Duration = Duration::from_secs(60);

/// Bind `config.listen`, open the authoritative store at `config.db_path`,
/// and serve until `shutdown` fires.
pub async fn run_v2(
    config: Config,
    registry: &DeviceKeyPair,
    shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let (listener, store) = bind_v2(&config).await?;
    serve_v2_with_revision(
        listener,
        registry,
        store,
        config.limits,
        config.revision.as_deref(),
        shutdown,
    )
    .await
}

/// The part of [`run_v2`] that can fail before serving: bind
/// `config.listen` and open the authoritative store at `config.db_path`
/// (`:memory:` opens an in-memory store).
///
/// An embedder that announces readiness calls this first, reports the
/// listener's `local_addr` (the bound port, so port 0 works) and only
/// then serves with [`serve_v2_with_revision`]: whatever it announced is
/// already accepting connections, and a port in use or an unusable
/// database fails before anything was announced (2026-11-19 review — the
/// CLI used to print its pin before binding and then idle without
/// listening when the bind failed).
pub async fn bind_v2(config: &Config) -> io::Result<(TcpListener, RegistryStore)> {
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|e| io::Error::new(e.kind(), format!("listen on {}: {e}", config.listen)))?;
    let store = if config.db_path.as_os_str() == ":memory:" {
        RegistryStore::open_in_memory()
    } else {
        RegistryStore::open(&config.db_path)
    }
    .map_err(|e| io::Error::other(format!("registry store {}: {e}", config.db_path.display())))?;
    Ok((listener, store))
}

/// Serve v2 on an already-bound listener (embedders/tests bind ephemeral
/// ports themselves and get identical behavior). No build revision
/// configured: a `ServerInfoRequest` is answered with `unknown`.
pub async fn serve_v2(
    listener: TcpListener,
    registry: &DeviceKeyPair,
    store: RegistryStore,
    limits: Limits,
    shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    serve_v2_with_revision(listener, registry, store, limits, None, shutdown).await
}

/// [`serve_v2`] with an explicit build revision for the G6
/// `ServerInfoRequest` → `ServerInfo` answer (contract protocol-v2.md
/// §6a). The revision must be a ≤32-byte control-free UTF-8 build
/// fingerprint (e.g. `0.1.0+4739f4d`); anything else degrades to
/// `unknown` with a warning rather than failing the server.
pub async fn serve_v2_with_revision(
    listener: TcpListener,
    registry: &DeviceKeyPair,
    store: RegistryStore,
    limits: Limits,
    revision: Option<&str>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let bundle = ak_identity::tls::make_self_signed(registry);
    let acceptor = tls::server_config(&bundle)?;
    let shared = Arc::new(Shared {
        registry_id: registry.public_key(),
        acceptor,
        store,
        limits,
        server_revision: sanitize_revision(revision),
        inner: Mutex::new(Inner::default()),
        conn_count: AtomicUsize::new(0),
        reject_count: AtomicUsize::new(0),
    });
    log::info!(
        "v2 registry server listening on tls {} (registry {})",
        listener.local_addr()?,
        util::hex_prefix(&shared.registry_id, 4)
    );
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                // P1-5: no connection task may outlive the server. First
                // signal every live connection's close watch (reader,
                // writer and handler all subscribe — a Notify would wake
                // only one of them), then abort whatever is left and join
                // bounded before returning.
                let tasks: Vec<JoinHandle<()>> = {
                    let mut inner = shared.lock();
                    for entry in inner.conns.values() {
                        let _ = entry.close.send(true);
                    }
                    inner.conn_tasks.drain().map(|(_, task)| task).collect()
                };
                for mut task in tasks {
                    task.abort();
                    let _ = tokio::time::timeout(TEARDOWN_GRACE, &mut task).await;
                }
                return Ok(());
            }
            _ = tick.tick() => sweep(&shared),
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    let _ = tcp.set_nodelay(true);
                    // CR-P1-03 + CR-P2-10 admission, all of it the
                    // cheapest rejection in the whole pipeline: the
                    // check-and-reserve of the per-source counters, the
                    // IPv6 /56 and /48 aggregates, and the global
                    // soft-cap fair share happens under one lock, and a
                    // source over any of them loses its socket right
                    // here: no task is spawned, no handshake work is
                    // ever started for it. A connect-only flood
                    // therefore parks at most
                    // `MAX_HANDSHAKING_PER_SOURCE` accepts per source,
                    // can never occupy a globally meaningful share of
                    // the serving cap, and a prefix-rotation flood can
                    // never exceed its delegation's aggregate.
                    //
                    // The live-connection slot is reserved atomically
                    // BEFORE the lock: the fair-share decision needs the
                    // pre-reservation count, and fetch-add-then-decide
                    // keeps concurrent accepts from overshooting (each
                    // sees its own reservation ordinal; a refused one
                    // gives it straight back). The slot is released by
                    // the task's RAII guard on EVERY exit path
                    // (R36-P1-03): failed or timed-out TLS handshake,
                    // ALPN mismatch, over-cap rejection, cancellation
                    // (abort drops the future, running `Drop`) and the
                    // normal connection-loop return.
                    let source = source_key(peer.ip());
                    let reserved = shared.conn_count.fetch_add(1, Ordering::SeqCst);
                    let admission = {
                        let mut inner = shared.lock();
                        inner.source_admit(&shared.limits, source, reserved, Instant::now())
                    };
                    match admission {
                        SourceAdmission::Admit => {
                            let task_id = alloc_conn_task_id(&shared);
                            let conn_shared = Arc::clone(&shared);
                            let slot = ConnSlotGuard::admitted(
                                Arc::clone(&conn_shared),
                                task_id,
                                source,
                            );
                            let task = tokio::spawn(async move {
                                let mut slot = slot;
                                // P1-5: the TLS handshake itself is bounded.
                                let stream = match tokio::time::timeout(
                                    TLS_ACCEPT_TIMEOUT,
                                    conn_shared.acceptor.accept(tcp),
                                )
                                .await
                                {
                                    Ok(Ok(stream)) => stream,
                                    Ok(Err(e)) => {
                                        log::debug!("v2 tls handshake with {peer} failed: {e}");
                                        return;
                                    }
                                    Err(_) => {
                                        log::debug!(
                                            "v2 tls handshake with {peer} exceeded {:?}",
                                            TLS_ACCEPT_TIMEOUT
                                        );
                                        return;
                                    }
                                };
                                // CR-P1-03: the handshake phase is over — this
                                // connection's per-source handshaking slot goes
                                // back immediately; the per-source live slot and
                                // the global counters live until the task exits
                                // (guard `Drop`). Failure paths above skip this
                                // call; `Drop` then releases the handshaking
                                // slot exactly once itself.
                                slot.handshake_done();
                                if tls::check_alpn(stream.get_ref().1.alpn_protocol()).is_err() {
                                    log::debug!("v2 connection from {peer} without ALPN ak-signal/2");
                                    return;
                                }
                                connection_loop(stream, peer, Arc::clone(&conn_shared)).await;
                            });
                            register_conn_task(&shared, task_id, task);
                        }
                        SourceAdmission::OverSourceCaps => {
                            // The source (or one of its IPv6 parent
                            // prefixes) is over its own caps: return the
                            // just-reserved global slot and drop the
                            // socket at accept — no task, no handshake.
                            shared.conn_count.fetch_sub(1, Ordering::SeqCst);
                            log::warn!(
                                "v2 per-source/prefix connection caps (source {} concurrent \
                                 / {} handshaking, /56 aggregate {}, /48 aggregate {}) reached; \
                                 dropping {peer}",
                                shared.limits.max_conns_per_source,
                                shared.limits.max_handshaking_per_source,
                                shared.limits.max_conns_per_56,
                                shared.limits.max_conns_per_48,
                            );
                        }
                        SourceAdmission::OverGlobal => {
                            // Past the serving cap on the global side (at
                            // the soft cap with this source at/above its
                            // fair share, or at the hard cap) but within
                            // every source/prefix cap: give the reserved
                            // global slot back. The polite rejection
                            // (one bounded ServerError) below deliberately
                            // holds NO source slots — it is bounded
                            // globally by MAX_REJECT_TASKS and carrying
                            // source counters in a task that may outlive
                            // the source's own budget accounting would
                            // only couple the two mechanisms.
                            shared.conn_count.fetch_sub(1, Ordering::SeqCst);
                            // The polite rejection still needs a full TLS
                            // handshake, so it runs in its own task under a
                            // SEPARATE small budget — never an unbounded
                            // stack of handshakes past the real cap — and
                            // every reject task is tracked like any other so
                            // shutdown aborts it too. Past the reject budget
                            // the socket is dropped at accept time.
                            let budgeted = shared
                                .reject_count
                                .fetch_add(1, Ordering::SeqCst)
                                < MAX_REJECT_TASKS;
                            if budgeted {
                                let task_id = alloc_conn_task_id(&shared);
                                let conn_shared = Arc::clone(&shared);
                                let slot =
                                    ConnSlotGuard::reject(Arc::clone(&conn_shared), task_id);
                                let task = tokio::spawn(async move {
                                    let _slot = slot;
                                    let mut stream = match tokio::time::timeout(
                                        TLS_ACCEPT_TIMEOUT,
                                        conn_shared.acceptor.accept(tcp),
                                    )
                                    .await
                                    {
                                        Ok(Ok(stream)) => stream,
                                        Ok(Err(_)) | Err(_) => return,
                                    };
                                    if tls::check_alpn(stream.get_ref().1.alpn_protocol()).is_err()
                                    {
                                        return;
                                    }
                                    log::warn!(
                                        "v2 connection limit ({MAX_LIVE_CONNECTIONS} soft, \
                                         {MAX_LIVE_CONNECTIONS_HARD} hard) reached; rejecting {peer}"
                                    );
                                    let reply =
                                        server_error(ERR_SESSION_LIMIT, "connection limit reached");
                                    let sent = tokio::time::timeout(
                                        FRAME_IO_DEADLINE,
                                        write_message(&mut stream, &reply, Role::Server),
                                    )
                                    .await;
                                    if matches!(sent, Ok(Ok(()))) {
                                        let _ = tokio::time::timeout(
                                            TEARDOWN_GRACE,
                                            stream.shutdown(),
                                        )
                                        .await;
                                    }
                                });
                                register_conn_task(&shared, task_id, task);
                            } else {
                                shared.reject_count.fetch_sub(1, Ordering::SeqCst);
                                log::warn!(
                                    "v2 connection limit ({MAX_LIVE_CONNECTIONS} soft, \
                                     {MAX_LIVE_CONNECTIONS_HARD} hard) reached and reject budget \
                                     ({MAX_REJECT_TASKS}) exhausted; dropping {peer}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("v2 accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

// ---- shared state -----------------------------------------------------------

struct ConnEntry {
    conn_id: u64,
    out: mpsc::Sender<CtrlFrame>,
    /// Close signal of this connection (true = torn down). Shared handle:
    /// the handler owns one, the registry keeps this one so a replacement
    /// Hello (or the server shutdown) can tear the connection down. A
    /// watch, not a Notify — reader, writer and handler all wait on it
    /// and `send(true)` wakes every one of them.
    close: Arc<watch::Sender<bool>>,
    candidates: Vec<SocketAddr>,
    peer_ip: IpAddr,
}

/// Resolve once the close watch fires (true) or its sender is gone
/// (aborted handler): either way this connection is over.
async fn closed(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[derive(Clone)]
struct SessionEntry {
    triple: (PairingId, CodeId, [u8; 16]),
    endpoints: [DeviceId; 2],
    expires_at: Instant,
}

/// A redeem of an unknown/invalid code, kept only in memory: the client
/// sees the same `InviteRedeemPending` shape as for a live code and the
/// unified `PAIR_UNAVAILABLE` at the deadline (no existence oracle).
struct Ghost {
    requester: DeviceId,
    request_id: RequestId,
    deadline_ms: u64,
    expires_at: Instant,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<DeviceId, ConnEntry>,
    sessions: HashMap<SessionId, SessionEntry>,
    session_by_triple: HashMap<(PairingId, CodeId, [u8; 16]), SessionId>,
    ghosts: HashMap<PairingId, Ghost>,
    /// Redeem idempotency records keyed by the authenticated
    /// (redeemer, request_id) — the in-memory twin of the store's
    /// `request_journal` for claim/revoke (same shape, bounded by
    /// [`JOURNAL_MAX_ROWS`], pruned past deadline + grace).
    redeem_journal: HashMap<(DeviceId, RequestId), RedeemReplay>,
    next_conn_id: u64,
    /// Registry of live connection tasks so shutdown can abort them all
    /// (P1-5); the strict live-connection caps are `Shared::conn_count`
    /// and `Shared::reject_count`, this map is the abort list. Each task
    /// removes its own entry via its [`ConnSlotGuard`] (R36-P1-03).
    conn_tasks: HashMap<u64, JoinHandle<()>>,
    next_conn_task_id: u64,
    hello_replay: HelloReplayCache,
    /// CR-P1-03: per-source connection counters AND the per-source
    /// redeem window in one entry (keyed by [`source_key`]), so a
    /// source's redeem budget survives reconnects — a flooder that
    /// cycles TCP connections keeps its history. CR-P2-10 moves the
    /// table into an [`IdleTable`] (O(log n) max-size eviction).
    /// Pruned by the sweeper only after [`SOURCE_TABLE_IDLE_TTL`] with
    /// zero counters; bounded by [`SOURCE_TABLE_MAX_ENTRIES`].
    sources: IdleTable<IpAddr, SourceEntry>,
    /// CR-P2-10: aggregate counters of all /64 sources inside one IPv6
    /// /56 (see [`v6_parents`]); the caps are `Limits::max_conns_per_56`
    /// / `Limits::redeem_per_56_per_min` (4× the per-source budgets).
    prefixes56: IdleTable<IpAddr, SourceEntry>,
    /// CR-P2-10: the same at /48 (`Limits::max_conns_per_48` /
    /// `Limits::redeem_per_48_per_min`, 16×).
    prefixes48: IdleTable<IpAddr, SourceEntry>,
    /// CR-P1-03: per-identity redeem windows (keyed by the
    /// Hello-authenticated device key), likewise shared across
    /// reconnects and TTL/bounded-pruned.
    identity_redeem: IdleTable<DeviceId, IdentityRedeemEntry>,
    /// CR-P2-10: sources currently holding at least one live
    /// connection — the divisor of [`conn_fair_share`]. Maintained
    /// exactly and incrementally (0→live and live→0 transitions in
    /// [`Inner::source_admit`] / [`Inner::source_release`]; entries
    /// holding counters are never pruned or evicted, so it cannot
    /// drift).
    active_conn_sources: usize,
    /// CR-P2-10: sources with at least one redeem attempt inside the
    /// current 60 s window — the divisor of the fair-share redeem
    /// breaker. Incremented on a source window's empty→non-empty
    /// transition and recomputed exactly by the sweeper (events age
    /// out lazily, so decrement points are not observable).
    active_redeem_sources: usize,
    /// The server-wide redeem circuit breaker (CR-P1-03, fair-share
    /// semantics CR-P2-10; see
    /// [`crate::server_v2::config::REDEEM_GLOBAL_BREAKER_PER_MIN`]).
    global_redeem: Option<SlidingWindow>,
}

/// Per-source accounting (CR-P1-03): connection counters reserved at
/// accept and released by the owning task's [`ConnSlotGuard`] (the same
/// every-exit-path RAII discipline as the global counters), plus the
/// source's shared redeem window. CR-P2-10 reuses the same shape for
/// the IPv6 /56 and /48 aggregate tables ([`Inner::prefixes56`] /
/// [`Inner::prefixes48`]): the fields count the whole prefix's
/// connections, the caps come from the aggregate limits instead.
struct SourceEntry {
    /// Connections this source currently holds against
    /// `Limits::max_conns_per_source` (admitted at accept, live until
    /// the task exits).
    live: usize,
    /// The subset still between TCP accept and the finished TLS
    /// handshake (`Limits::max_handshaking_per_source`), released the
    /// moment the handshake resolves or fails.
    handshaking: usize,
    redeem: SlidingWindow,
    last_seen: Instant,
}

/// Per-identity redeem accounting (CR-P1-03).
struct IdentityRedeemEntry {
    redeem: SlidingWindow,
    last_seen: Instant,
}

/// Normalize a peer address to its accounting source (CR-P1-03):
/// a full IPv4 address (a /32 — one host) or the first 64 bits of an
/// IPv6 address (a /64 — the LAN or mobile delegation a home device
/// sits behind). IPv4-mapped IPv6 addresses collapse to the IPv4
/// source so a dual-stack bind cannot double a host's budgets.
fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let octets = v6.octets();
                let mut prefix = [0u8; 16];
                prefix[..8].copy_from_slice(&octets[..8]);
                IpAddr::V6(Ipv6Addr::from(prefix))
            }
        },
    }
}

/// The IPv6 prefix parents of a normalized source (CR-P2-10): `Some`
/// with the (/56, /48) keys for a real IPv6 source (host bits already
/// zero by /64 normalization, so only bytes 0..7 / 0..6 matter), `None`
/// for IPv4 — a /32 IS its own source, it has no parent hierarchy.
/// Production listens IPv4-only today; these tables future-proof the
/// `[::]` listen by making /64 rotation inside one delegation
/// (a /56 holds 256 /64s, a /48 holds 65,536) worthless before it
/// opens: the aggregates cap at 4× / 16× one /64's budgets.
fn v6_parents(source: IpAddr) -> Option<(IpAddr, IpAddr)> {
    match source {
        IpAddr::V4(_) => None,
        IpAddr::V6(v6) => {
            let octets = v6.octets();
            let mut p56 = [0u8; 16];
            p56[..7].copy_from_slice(&octets[..7]);
            let mut p48 = [0u8; 16];
            p48[..6].copy_from_slice(&octets[..6]);
            Some((
                IpAddr::V6(Ipv6Addr::from(p56)),
                IpAddr::V6(Ipv6Addr::from(p48)),
            ))
        }
    }
}

/// Per-source fair share of the serving cap at/over the soft cap
/// (CR-P2-10): `ceil(MAX_LIVE_CONNECTIONS / active sources)`, floored
/// at [`FAIR_CONN_FLOOR`]. Invariants it buys:
///
/// * a SMALL number of sources can never lock out everyone else —
///   filling the soft cap takes ≥ 32 sources (8 conns each), and once
///   it is full only sources already AT/ABOVE the share are refused,
///   so any source below the share (every newcomer, by definition)
///   still connects, overshooting the soft cap up to the hard cap;
/// * a well-behaved source always gets its per-source minimum — the
///   floor guarantees a control connection plus the reconnect-overlap
///   one until [`MAX_LIVE_CONNECTIONS_HARD`] is reached.
///
/// `ceil` (not floor) so the shares of all active sources sum to at
/// least the cap: rounding down could refuse a below-average source
/// while capacity goes unused.
fn conn_fair_share(active_conn_sources: usize) -> usize {
    let active = active_conn_sources.max(1);
    MAX_LIVE_CONNECTIONS.div_ceil(active).max(FAIR_CONN_FLOOR)
}

/// The outcome of [`Inner::source_admit`] — which accept-time rejection
/// path (if any) the caller takes.
enum SourceAdmission {
    /// All caps passed and every counter (source, /56, /48) was
    /// reserved under the lock: spawn the connection task.
    Admit,
    /// The source itself (or one of its IPv6 prefix parents) is over
    /// its own caps — drop the socket at accept, no task at all.
    OverSourceCaps,
    /// The source/prefix caps passed but the GLOBAL side refuses (at
    /// the soft cap with this source at/above [`conn_fair_share`], or
    /// at [`MAX_LIVE_CONNECTIONS_HARD`]) — the bounded one-ServerError
    /// rejection path.
    OverGlobal,
}

/// Entries of the CR-P1-03 accounting tables that can be TTL-pruned or
/// evicted: they can move their idle timestamp (the table's recency
/// index needs the previous one), and say whether they still hold live
/// resources (an entry owing counter decrements must never be evicted —
/// its guards still reference it).
trait IdleTracked {
    /// Move the activity timestamp to `now`; returns the previous one.
    fn touch(&mut self, now: Instant) -> Instant;
    fn holds_live_counters(&self) -> bool;
}

impl IdleTracked for SourceEntry {
    fn touch(&mut self, now: Instant) -> Instant {
        std::mem::replace(&mut self.last_seen, now)
    }
    fn holds_live_counters(&self) -> bool {
        self.live > 0 || self.handshaking > 0
    }
}

impl IdleTracked for IdentityRedeemEntry {
    fn touch(&mut self, now: Instant) -> Instant {
        std::mem::replace(&mut self.last_seen, now)
    }
    // Purely rate-window state — always evictable.
    fn holds_live_counters(&self) -> bool {
        false
    }
}

/// A bounded idle-TTL accounting table with O(log n) max-size eviction
/// (CR-P2-10, replacing the O(n) table scan of the old free-function
/// `make_room`): entries live in a [`HashMap`], and a [`BTreeSet`] keyed
/// by `(last_seen, key)` indexes them by recency so the stalest idle
/// entry — the eviction victim — is the front of the ordered index, not
/// the result of a scan. Every activity moves the entry's index slot
/// (one remove + one insert, O(log n)); pruning and eviction skip
/// entries that still hold live counters (their guards owe decrements).
///
/// The periodic TTL prune is NOT a scan in the common case either: the
/// recency index is ordered by `last_seen`, so pruning walks it from
/// the oldest end and stops at the first entry still inside the TTL —
/// everything after it is fresher by construction.
struct IdleTable<K, V> {
    entries: HashMap<K, V>,
    order: BTreeSet<(Instant, K)>,
}

impl<K, V> Default for IdleTable<K, V>
where
    K: Copy + Eq + std::hash::Hash + Ord,
    V: IdleTracked,
{
    fn default() -> Self {
        IdleTable {
            entries: HashMap::new(),
            order: BTreeSet::new(),
        }
    }
}

impl<K, V> IdleTable<K, V>
where
    K: Copy + Eq + std::hash::Hash + Ord,
    V: IdleTracked,
{
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)
    }

    /// Mutable entry access WITHOUT re-stamping activity — for the
    /// counter mutators, which stamp explicitly via [`Self::note_activity`]
    /// afterwards (so an index update only happens once per event).
    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries.get_mut(key)
    }

    /// The entry for `key` with its activity stamp moved to `now`
    /// (inserted with `init` when absent), eviction index in step.
    fn touch_entry(&mut self, key: K, now: Instant, init: impl FnOnce() -> V) -> &mut V {
        use std::collections::hash_map::Entry;
        match self.entries.entry(key) {
            Entry::Vacant(vacant) => {
                self.order.insert((now, key));
                vacant.insert(init())
            }
            Entry::Occupied(mut occupied) => {
                let previous = occupied.get_mut().touch(now);
                if previous != now {
                    self.order.remove(&(previous, key));
                    self.order.insert((now, key));
                }
                occupied.into_mut()
            }
        }
    }

    /// Move an existing entry's activity stamp to `now` (no insert);
    /// a no-op for absent keys.
    fn note_activity(&mut self, key: &K, now: Instant) {
        if let Some(entry) = self.entries.get_mut(key) {
            let previous = entry.touch(now);
            if previous != now {
                self.order.remove(&(previous, *key));
                self.order.insert((now, *key));
            }
        }
    }

    /// TTL prune (the sweeper, every [`SWEEP_INTERVAL`]): drop entries
    /// idle past [`SOURCE_TABLE_IDLE_TTL`] that hold no live counters.
    /// Walks the recency index from the oldest end and stops at the
    /// first entry still inside the TTL — the ordered index makes the
    /// happy path O(1) instead of a full-table `retain`.
    fn prune_idle(&mut self, now: Instant) {
        let mut stale = Vec::new();
        for &(last_seen, key) in &self.order {
            if now.duration_since(last_seen) <= SOURCE_TABLE_IDLE_TTL {
                break;
            }
            if !self
                .entries
                .get(&key)
                .map(|entry| entry.holds_live_counters())
                .unwrap_or(false)
            {
                stale.push((last_seen, key));
            }
        }
        for (last_seen, key) in stale {
            self.order.remove(&(last_seen, key));
            self.entries.remove(&key);
        }
    }

    /// Entries touched within `window` before `now` — the fresh suffix
    /// of the recency index, newest first. The sweeper's exact recompute
    /// of the fair-share divisors walks only this suffix (an entry can
    /// only carry fresh window events if its `last_seen` is fresh too,
    /// because every attempt stamps it).
    fn recent<'a>(
        &'a self,
        now: Instant,
        window: Duration,
    ) -> impl Iterator<Item = (&'a K, &'a V)> + 'a {
        self.order
            .iter()
            .rev()
            .take_while(move |&&(last_seen, _)| now.duration_since(last_seen) <= window)
            .filter_map(move |&(_, key)| self.entries.get_key_value(&key))
    }

    /// Make room for one new entry at [`SOURCE_TABLE_MAX_ENTRIES`]
    /// (CR-P1-03 bound; CR-P2-10 evicts in O(log n) via the recency
    /// index instead of a linear scan for the stalest victim): walk the
    /// index from the oldest end — idle-expired entries are dropped
    /// outright (their 60 s windows expired long before the TTL), and
    /// the first idle-but-fresh entry encountered is the stalest
    /// evictable one and is evicted. Entries holding live counters are
    /// never evicted (their guards still owe decrements); they are
    /// skipped and bound the walk (a live entry implies a connection,
    /// and the serving cap is orders of magnitude below this cap). No
    /// evictable entry → the caller treats the arrival as over budget
    /// rather than overrunning memory.
    fn make_room(&mut self, now: Instant) -> bool {
        if self.len() < SOURCE_TABLE_MAX_ENTRIES {
            return true;
        }
        let mut expired = Vec::new();
        let mut victim: Option<(Instant, K)> = None;
        for &(last_seen, key) in &self.order {
            if self
                .entries
                .get(&key)
                .map(|entry| entry.holds_live_counters())
                .unwrap_or(false)
            {
                continue;
            }
            if now.duration_since(last_seen) > SOURCE_TABLE_IDLE_TTL {
                expired.push((last_seen, key));
                continue;
            }
            victim = Some((last_seen, key));
            break;
        }
        if !expired.is_empty() {
            for (last_seen, key) in expired {
                self.order.remove(&(last_seen, key));
                self.entries.remove(&key);
            }
            return true;
        }
        match victim {
            Some((last_seen, key)) => {
                self.order.remove(&(last_seen, key));
                self.entries.remove(&key);
                true
            }
            None => false,
        }
    }
}

/// One journaled redeem attempt for idempotent retries: the response is a
/// pure function of these fields, so a retry replays byte-identical
/// `InviteRedeemPending` frames with the ORIGINAL pairing id.
#[derive(Clone, Copy)]
struct RedeemReplay {
    lookup_digest: Digest32,
    redeemer_nonce: [u8; 32],
    request_mac: [u8; 32],
    deadline_ms: u64,
    pairing_id: PairingId,
    expires_at: Instant,
}

impl RedeemReplay {
    /// Same redeem attempt (everything the signature binds)? A different
    /// attempt reusing the request id is a `REQUEST_CONFLICT`, exactly as
    /// the claim journal treats it.
    fn matches(&self, redeem: &RedeemRequest) -> bool {
        self.lookup_digest == redeem.lookup_digest
            && self.redeemer_nonce == redeem.redeemer_nonce
            && self.request_mac == redeem.request_mac
            && self.deadline_ms == redeem.deadline_ms
    }
}

struct Shared {
    registry_id: RegistryId,
    acceptor: tokio_rustls::TlsAcceptor,
    store: RegistryStore,
    limits: Limits,
    /// The build revision this server answers `ServerInfoRequest` with
    /// (G6 §6a; already sanitized to the wire budget at startup).
    server_revision: Arc<str>,
    inner: Mutex<Inner>,
    /// Live (accepted, not yet finished) connections — the admission
    /// counter for [`MAX_LIVE_CONNECTIONS`], reserved atomically at
    /// accept time and released by the owning task's [`ConnSlotGuard`]
    /// on every exit path (R36-P1-03).
    conn_count: AtomicUsize,
    /// Over-cap rejection tasks in flight — the admission counter for
    /// [`MAX_REJECT_TASKS`], so a flood past the serving cap can never
    /// stack unbounded handshake work (R36-P1-03).
    reject_count: AtomicUsize,
}

/// Which admission counter a [`ConnSlotGuard`] releases on drop.
enum SlotKind {
    /// One of the [`MAX_LIVE_CONNECTIONS`] served-connection slots.
    Live,
    /// One of the [`MAX_REJECT_TASKS`] over-cap rejection slots.
    Reject,
}

/// RAII ownership of one accepted connection's admission slot and
/// shutdown-abort registration (R36-P1-03). The counter returns exactly
/// once per connection task — failed or timed-out TLS handshake, ALPN
/// mismatch, early return, cancellation (abort drops the future, running
/// `Drop`) and the normal connection-loop exit — and the task removes
/// itself from the abort list on the same paths. A task that exits in
/// the tiny window before its handle is registered leaves a finished
/// handle behind — harmless (abort on a finished task is a no-op) and
/// swept at shutdown.
///
/// CR-P1-03 extends the same every-exit-path discipline to the
/// per-source counters: a live guard holds one `live` and (until
/// [`ConnSlotGuard::handshake_done`]) one `handshaking` slot against
/// its normalized source.
struct ConnSlotGuard {
    shared: Option<Arc<Shared>>,
    task_id: u64,
    kind: SlotKind,
    /// CR-P1-03: the normalized source this task's per-source counters
    /// were reserved against. `None` for reject tasks — they never take
    /// source slots (see the accept path).
    source: Option<IpAddr>,
    /// Whether the handshake phase is still open as far as this guard
    /// knows. Released exactly once — either by an explicit
    /// [`ConnSlotGuard::handshake_done`] after a completed handshake or
    /// by `Drop` on every path that never got there (failure, timeout,
    /// ALPN mismatch, cancellation).
    handshake_open: bool,
}

impl ConnSlotGuard {
    /// Guard for a connection holding one live-connection slot and one
    /// per-source slot pair (live + handshaking) against `source`.
    fn admitted(shared: Arc<Shared>, task_id: u64, source: IpAddr) -> Self {
        Self {
            shared: Some(shared),
            task_id,
            kind: SlotKind::Live,
            source: Some(source),
            handshake_open: true,
        }
    }

    /// Guard for an over-cap rejection task: tracked for shutdown abort,
    /// but counted against [`MAX_REJECT_TASKS`], not the serving cap and
    /// not any per-source cap (CR-P1-03).
    fn reject(shared: Arc<Shared>, task_id: u64) -> Self {
        Self {
            shared: Some(shared),
            task_id,
            kind: SlotKind::Reject,
            source: None,
            handshake_open: false,
        }
    }

    /// CR-P1-03: the TLS handshake resolved — hand the per-source
    /// handshaking slot back right away instead of waiting for the
    /// connection to end, so slow-or-silent sockets (the connect-only
    /// flood shape) are the only ones that hold it. No-op when the
    /// phase was already closed (or the guard holds no source).
    fn handshake_done(&mut self) {
        if !self.handshake_open {
            return;
        }
        self.handshake_open = false;
        if let (Some(shared), Some(source)) = (self.shared.as_ref(), self.source) {
            shared.lock().source_handshake_done(source, Instant::now());
        }
    }
}

impl Drop for ConnSlotGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            {
                let mut inner = shared.lock();
                if let Some(source) = self.source {
                    inner.source_release(source, self.handshake_open, Instant::now());
                }
                inner.conn_tasks.remove(&self.task_id);
            }
            match self.kind {
                SlotKind::Live => {
                    shared.conn_count.fetch_sub(1, Ordering::SeqCst);
                }
                SlotKind::Reject => {
                    shared.reject_count.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
    }
}

/// Fresh id for the shutdown-abort registry.
fn alloc_conn_task_id(shared: &Shared) -> u64 {
    let mut inner = shared.lock();
    inner.next_conn_task_id = inner.next_conn_task_id.wrapping_add(1);
    inner.next_conn_task_id
}

/// Track a spawned connection task so shutdown can abort it. A task
/// that already exited unregisters itself via its guard; when that is
/// observable (`is_finished`) the stale handle is not registered at all.
fn register_conn_task(shared: &Shared, id: u64, task: JoinHandle<()>) {
    let mut inner = shared.lock();
    if !task.is_finished() {
        inner.conn_tasks.insert(id, task);
    }
}

impl Inner {
    /// CR-P1-03 per-source admission + CR-P2-10 extensions, all
    /// check-and-reserve under the one lock the caller already holds
    /// for the decision, so concurrent accepts can never overshoot any
    /// cap: the source's own counters (`live`, `handshaking`), the IPv6
    /// /56 and /48 aggregates, and — with `conn_count`, this accept's
    /// pre-reservation global ordinal — the global soft cap with
    /// [`conn_fair_share`] shedding. Over a source/prefix cap (or an
    /// accounting table with no room left) the caller drops the socket
    /// at accept time; over the global side it takes the bounded
    /// one-ServerError rejection path. Nothing is reserved on failure.
    fn source_admit(
        &mut self,
        limits: &Limits,
        source: IpAddr,
        conn_count: usize,
        now: Instant,
    ) -> SourceAdmission {
        let parents = v6_parents(source);
        let parent56 = parents.map(|(p56, _)| p56);
        let parent48 = parents.map(|(_, p48)| p48);
        // Memory backstops first; a refusal here reserved nothing.
        if !self.sources.make_room(now) {
            return SourceAdmission::OverSourceCaps;
        }
        if parent56.is_some() && !self.prefixes56.make_room(now) {
            return SourceAdmission::OverSourceCaps;
        }
        if parent48.is_some() && !self.prefixes48.make_room(now) {
            return SourceAdmission::OverSourceCaps;
        }
        // The source's own caps (CR-P1-03, unchanged semantics).
        let (live, handshaking) = {
            let entry = self.sources.touch_entry(source, now, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(limits.redeem_per_source_per_min, REDEEM_WINDOW),
                last_seen: now,
            });
            (entry.live, entry.handshaking)
        };
        if live >= limits.max_conns_per_source || handshaking >= limits.max_handshaking_per_source {
            return SourceAdmission::OverSourceCaps;
        }
        // The IPv6 prefix aggregates (CR-P2-10): the /56 and /48 caps
        // are the per-source caps scaled by the frozen multipliers, so
        // rotating /64s inside one delegation cannot multiply a
        // delegation's serving footprint (4× at /56, 16× at /48).
        let handshaking_cap_56 = limits
            .max_handshaking_per_source
            .saturating_mul(PREFIX_56_SHARE_MULTIPLIER as usize);
        let handshaking_cap_48 = limits
            .max_handshaking_per_source
            .saturating_mul(PREFIX_48_SHARE_MULTIPLIER as usize);
        if let Some(p56) = parent56 {
            let (prefix_live, prefix_handshaking) = {
                let entry = self.prefixes56.touch_entry(p56, now, || SourceEntry {
                    live: 0,
                    handshaking: 0,
                    redeem: SlidingWindow::new(limits.redeem_per_56_per_min, REDEEM_WINDOW),
                    last_seen: now,
                });
                (entry.live, entry.handshaking)
            };
            if prefix_live >= limits.max_conns_per_56 || prefix_handshaking >= handshaking_cap_56 {
                return SourceAdmission::OverSourceCaps;
            }
        }
        if let Some(p48) = parent48 {
            let (prefix_live, prefix_handshaking) = {
                let entry = self.prefixes48.touch_entry(p48, now, || SourceEntry {
                    live: 0,
                    handshaking: 0,
                    redeem: SlidingWindow::new(limits.redeem_per_48_per_min, REDEEM_WINDOW),
                    last_seen: now,
                });
                (entry.live, entry.handshaking)
            };
            if prefix_live >= limits.max_conns_per_48 || prefix_handshaking >= handshaking_cap_48 {
                return SourceAdmission::OverSourceCaps;
            }
        }
        // The global side (CR-P2-10): below the soft cap, plain
        // admission. At/over it, admit only sources still under their
        // fair share — never past the hard cap.
        if conn_count >= MAX_LIVE_CONNECTIONS {
            let share = conn_fair_share(self.active_conn_sources);
            if conn_count >= MAX_LIVE_CONNECTIONS_HARD || live >= share {
                return SourceAdmission::OverGlobal;
            }
        }
        // Reserve everything: the source first (its 0→live transition
        // feeds the fair-share divisor), then both parents.
        {
            let entry = self
                .sources
                .get_mut(&source)
                .expect("source entry touched above");
            entry.live += 1;
            entry.handshaking += 1;
            if entry.live == 1 {
                self.active_conn_sources += 1;
            }
        }
        if let Some(p56) = parent56 {
            let entry = self.prefixes56.touch_entry(p56, now, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(limits.redeem_per_56_per_min, REDEEM_WINDOW),
                last_seen: now,
            });
            entry.live += 1;
            entry.handshaking += 1;
        }
        if let Some(p48) = parent48 {
            let entry = self.prefixes48.touch_entry(p48, now, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(limits.redeem_per_48_per_min, REDEEM_WINDOW),
                last_seen: now,
            });
            entry.live += 1;
            entry.handshaking += 1;
        }
        SourceAdmission::Admit
    }

    /// The TLS handshake phase of one admitted connection closed out
    /// (success or failure — callers only invoke this on success;
    /// failure paths leave the release to the guard's `Drop`). The
    /// handshaking counters of the source AND its IPv6 parents go back.
    fn source_handshake_done(&mut self, source: IpAddr, now: Instant) {
        if let Some(entry) = self.sources.get_mut(&source) {
            entry.handshaking = entry.handshaking.saturating_sub(1);
        }
        self.sources.note_activity(&source, now);
        if let Some((p56, p48)) = v6_parents(source) {
            if let Some(entry) = self.prefixes56.get_mut(&p56) {
                entry.handshaking = entry.handshaking.saturating_sub(1);
            }
            self.prefixes56.note_activity(&p56, now);
            if let Some(entry) = self.prefixes48.get_mut(&p48) {
                entry.handshaking = entry.handshaking.saturating_sub(1);
            }
            self.prefixes48.note_activity(&p48, now);
        }
    }

    /// Release one admitted connection's per-source slots — and the
    /// matching aggregates of its IPv6 parents. `handshaking` is true
    /// when the handshake phase was never closed out before the exit
    /// path (the guard's `Drop` passes its still-open flag). The
    /// source's live 1→0 transition keeps the fair-share divisor exact.
    fn source_release(&mut self, source: IpAddr, handshaking: bool, now: Instant) {
        if let Some(entry) = self.sources.get_mut(&source) {
            // Decrement the divisor on the 1→0 transition only — a
            // spurious extra release must not double-decrement it.
            if entry.live == 1 {
                self.active_conn_sources = self.active_conn_sources.saturating_sub(1);
            }
            entry.live = entry.live.saturating_sub(1);
            if handshaking {
                entry.handshaking = entry.handshaking.saturating_sub(1);
            }
        }
        self.sources.note_activity(&source, now);
        if let Some((p56, p48)) = v6_parents(source) {
            if let Some(entry) = self.prefixes56.get_mut(&p56) {
                entry.live = entry.live.saturating_sub(1);
                if handshaking {
                    entry.handshaking = entry.handshaking.saturating_sub(1);
                }
            }
            self.prefixes56.note_activity(&p56, now);
            if let Some(entry) = self.prefixes48.get_mut(&p48) {
                entry.live = entry.live.saturating_sub(1);
                if handshaking {
                    entry.handshaking = entry.handshaking.saturating_sub(1);
                }
            }
            self.prefixes48.note_activity(&p48, now);
        }
    }

    /// TTL-prune the CR-P1-03/CR-P2-10 accounting tables (called by the
    /// sweeper every [`SWEEP_INTERVAL`]) so they cannot grow
    /// unboundedly: a pruned source entry held zero counters (all its
    /// guards closed) and has been idle for [`SOURCE_TABLE_IDLE_TTL`] —
    /// far past the 60 s windows it carried, so pruning discards no
    /// live budget. Identity entries are pure window state on the same
    /// idle rule; prefix tables follow the source rule. Also recomputes
    /// the fair-share redeem divisor exactly (see
    /// [`IdleTable::recent`]).
    fn prune_source_tables(&mut self, now: Instant) {
        self.sources.prune_idle(now);
        self.prefixes56.prune_idle(now);
        self.prefixes48.prune_idle(now);
        self.identity_redeem.prune_idle(now);
        let mut active = 0usize;
        for (_, entry) in self.sources.recent(now, REDEEM_WINDOW) {
            if entry.redeem.count_in_window(now) > 0 {
                active += 1;
            }
        }
        self.active_redeem_sources = active;
    }

    /// CR-P1-03 redeem budgets (CR-P2-10: five shared layers now) after
    /// the per-connection window (which lives in `ConnState`): per /64
    /// source, the IPv6 /56 and /48 aggregates (no-ops for IPv4), per
    /// device identity, then the server-wide breaker. Any failing layer
    /// answers with the smallest retry hint among the failing layers.
    /// Layers checked before a failing one have already recorded the
    /// attempt — that is deliberate: the attempt really happened at that
    /// layer, and an attacker must not be able to keep one layer's
    /// window empty by exhausting another first. All attempts count,
    /// regardless of outcome (the contract §11 freeze of 2026-11-11:
    /// budgets bound total work, they do not punish guessing — invite
    /// codes have 160 bits of entropy, there is no enumeration to
    /// punish).
    fn redeem_allows(
        &mut self,
        limits: &Limits,
        source: IpAddr,
        identity: &DeviceId,
        now: Instant,
    ) -> (bool, u32) {
        let mut retry = u32::MAX;
        // Layer 1 — the per-/64 source table entry exists for any
        // connection that got this far (created at accept); make_room is
        // the memory backstop, not an expected event.
        if !self.sources.make_room(now) {
            return (false, 0);
        }
        let (source_ok, was_active) = {
            let entry = self.sources.touch_entry(source, now, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(limits.redeem_per_source_per_min, REDEEM_WINDOW),
                last_seen: now,
            });
            // CR-P2-10: a source window going empty→non-empty is exactly
            // a source becoming active for the fair-share divisor.
            let was_active = entry.redeem.count_in_window(now) > 0;
            let ok = entry.redeem.allow_at(now);
            if !ok {
                retry = retry.min(entry.redeem.retry_after_ms(now));
            }
            (ok, was_active)
        };
        if !source_ok {
            return (false, retry);
        }
        if !was_active {
            self.active_redeem_sources += 1;
        }
        // Layers 1a/1b — the IPv6 /56 and /48 aggregate windows
        // (CR-P2-10): the same redeem budget question one delegation
        // up, so 256 /64s inside one /56 share 4× (not 256×) one /64's
        // budget and one /48's residents share 16×. IPv4 sources have
        // no parents and skip both.
        if let Some((p56, p48)) = v6_parents(source) {
            if !self.prefixes56.make_room(now) || !self.prefixes48.make_room(now) {
                return (false, 0);
            }
            let ok56 = {
                let entry = self.prefixes56.touch_entry(p56, now, || SourceEntry {
                    live: 0,
                    handshaking: 0,
                    redeem: SlidingWindow::new(limits.redeem_per_56_per_min, REDEEM_WINDOW),
                    last_seen: now,
                });
                let ok = entry.redeem.allow_at(now);
                if !ok {
                    retry = retry.min(entry.redeem.retry_after_ms(now));
                }
                ok
            };
            if !ok56 {
                return (false, retry);
            }
            let ok48 = {
                let entry = self.prefixes48.touch_entry(p48, now, || SourceEntry {
                    live: 0,
                    handshaking: 0,
                    redeem: SlidingWindow::new(limits.redeem_per_48_per_min, REDEEM_WINDOW),
                    last_seen: now,
                });
                let ok = entry.redeem.allow_at(now);
                if !ok {
                    retry = retry.min(entry.redeem.retry_after_ms(now));
                }
                ok
            };
            if !ok48 {
                return (false, retry);
            }
        }
        // Layer 2 — the device identity, shared across reconnects.
        if !self.identity_redeem.make_room(now) {
            return (false, 0);
        }
        let identity_ok = {
            let entry = self
                .identity_redeem
                .touch_entry(*identity, now, || IdentityRedeemEntry {
                    redeem: SlidingWindow::new(limits.redeem_per_identity_per_min, REDEEM_WINDOW),
                    last_seen: now,
                });
            let ok = entry.redeem.allow_at(now);
            if !ok {
                retry = retry.min(entry.redeem.retry_after_ms(now));
            }
            ok
        };
        if !identity_ok {
            return (false, retry);
        }
        // Layer 3 — the server-wide breaker, FAIR SHARE under saturation
        // (CR-P2-10). While the 600/min window has room, plain
        // accounting. When it is full, do NOT reject everyone (the old
        // flat breaker let ~20 sources × 30/min deny every later
        // source's redeems): reject only sources whose own in-window
        // count has reached the fair share `breaker / active_sources`
        // (floor: the per-source limit, so a normal source is never
        // squeezed below its own budget); below-share attempts are
        // admitted and deliberately NOT recorded into the already-full
        // window — its memory stays at 600 entries, and it drains on
        // its own as the oldest recorded events age out. The total-work
        // bound becomes 30/min × active sources, with the per-/64 layer
        // still tightening first.
        let window = self.global_redeem.get_or_insert_with(|| {
            SlidingWindow::new(limits.redeem_global_breaker_per_min, REDEEM_WINDOW)
        });
        if window.allow_at(now) {
            return (true, 0);
        }
        let active = (self.active_redeem_sources as u32).max(1);
        let fair_share =
            (limits.redeem_global_breaker_per_min / active).max(limits.redeem_per_source_per_min);
        let in_window = self
            .sources
            .get(&source)
            .map(|entry| entry.redeem.count_in_window(now))
            .unwrap_or(0);
        if (in_window as u32) < fair_share {
            return (true, 0);
        }
        (false, window.retry_after_ms(now))
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Bounded outbound push: the per-connection outbox is `OUTBOX_DEPTH`
    /// deep; a full queue drops the message (recoverable via
    /// `PairStatusQuery` resends) instead of blocking any handler.
    /// Accepts a full frame or a plain message (`From<CtrlMessage>`).
    fn deliver<F: Into<CtrlFrame>>(&self, device: &DeviceId, frame: F) {
        let tx = {
            let inner = self.lock();
            inner.conns.get(device).map(|entry| entry.out.clone())
        };
        match tx {
            Some(tx) => deliver(&tx, frame.into()),
            None => log::debug!(
                "device {} offline; message dropped",
                util::hex_prefix(device, 4)
            ),
        }
    }

    /// CR-P1-03: the shared redeem budget layers (per source, per
    /// identity, global breaker) under one lock acquisition.
    fn redeem_allows(&self, source: IpAddr, identity: &DeviceId, now: Instant) -> (bool, u32) {
        let mut inner = self.lock();
        inner.redeem_allows(&self.limits, source, identity, now)
    }
}

fn deliver<F: Into<CtrlFrame>>(tx: &mpsc::Sender<CtrlFrame>, frame: F) {
    if let Err(e) = tx.try_send(frame.into()) {
        log::debug!("outbound queue full or closed; message dropped: {e}");
    }
}

/// Free-function convenience of [`deliver`] for plain messages (the
/// wave-28 extension stays empty).
fn deliver_msg(tx: &mpsc::Sender<CtrlFrame>, message: CtrlMessage) {
    deliver(tx, CtrlFrame::new(message));
}

fn pair_error(
    request_id: RequestId,
    scope: ErrorScope,
    object: [u8; 16],
    code: u16,
    retry_after_ms: u32,
) -> CtrlMessage {
    CtrlMessage::PairError {
        request_id,
        scope,
        object_id: object,
        code,
        retry_after_ms,
    }
}

/// Unified "target unavailable" — the one answer for unknown/expired/
/// revoked codes, foreign pairings, and MAC-shape failures alike.
fn unavailable(request_id: RequestId, object: [u8; 16]) -> CtrlMessage {
    pair_error(request_id, ErrorScope::Pairing, object, PAIR_UNAVAILABLE, 0)
}

fn server_error(code: u16, context: &str) -> CtrlMessage {
    let message = match code {
        ERR_UNKNOWN_DEVICE => "target device not online",
        ERR_SESSION_LIMIT => "session limit reached",
        ERR_RATE_LIMITED => "frame rate exceeded",
        ERR_BAD_REQUEST => "malformed request",
        ERR_SESSION_NOT_FOUND => "session not found",
        _ => context,
    }
    .to_string();
    CtrlMessage::ServerError { code, message }
}

/// Normalize a configured build revision to the `ServerInfo` wire budget
/// (G6 §6a): ≤32 UTF-8 bytes, no control characters, non-empty; `None`
/// or anything violating the budget degrades to `"unknown"` — an
/// operator-injected over-long or dirty string must not wedge the server
/// or leak more than a fingerprint.
fn sanitize_revision(revision: Option<&str>) -> Arc<str> {
    const FALLBACK: &str = "unknown";
    match revision {
        Some(text)
            if !text.is_empty()
                && text.len() <= crate::protocol_v2::message::MAX_REVISION_BYTES
                && !text.chars().any(|c| c.is_control()) =>
        {
            Arc::from(text)
        }
        Some(text) => {
            log::warn!(
                "ignoring configured server revision {:?} (empty, over the \
                 32-byte budget, or control characters); answering 'unknown'",
                text
            );
            Arc::from(FALLBACK)
        }
        None => Arc::from(FALLBACK),
    }
}

fn now_ms() -> u64 {
    RegistryStore::now_ms()
}

// ---- connection loop ---------------------------------------------------------

enum Flow {
    Continue,
    Disconnect,
}

struct ConnState {
    device: Option<DeviceId>,
    conn_id: u64,
    close: Arc<watch::Sender<bool>>,
    peer_ip: IpAddr,
    challenge: Option<[u8; 32]>,
    challenge_expires_ms: u64,
    exporter: [u8; 32],
    redeem_window: SlidingWindow,
}

async fn connection_loop(stream: TlsServerStream, peer: SocketAddr, shared: Arc<Shared>) {
    // The exporter is bound to this TLS connection only (rustls 0.23
    // exposes export_keying_material; plan A.5 as specified, no
    // certificate-only fallback).
    let exporter = match hello2::exporter_secret_server(stream.get_ref().1) {
        Ok(value) => value,
        Err(e) => {
            log::debug!("exporter unavailable for {peer}: {e}");
            return;
        }
    };
    let (out_tx, out_rx) = mpsc::channel::<CtrlFrame>(OUTBOX_DEPTH);
    let nonce = random_nonce32();
    let expires_ms = now_ms() + shared.limits.hello_max_age.as_millis() as u64;
    let mut stream = stream;
    // AuthChallenge first: one nonce, this connection, at most 10 s —
    // and the write itself under the shared frame budget.
    let challenge = CtrlMessage::AuthChallenge {
        registry_id: shared.registry_id,
        nonce,
        expires_ms,
    };
    let sent = tokio::time::timeout(
        FRAME_IO_DEADLINE,
        write_message(&mut stream, &challenge, Role::Server),
    )
    .await;
    if !matches!(sent, Ok(Ok(()))) {
        return;
    }
    let (close_tx, close_rx) = watch::channel(false);
    let mut state = ConnState {
        device: None,
        conn_id: 0,
        close: Arc::new(close_tx),
        peer_ip: peer.ip(),
        challenge: Some(nonce),
        challenge_expires_ms: expires_ms,
        exporter,
        redeem_window: SlidingWindow::new(
            shared.limits.redeem_per_conn_per_min,
            Duration::from_secs(60),
        ),
    };
    // P2 (帧读失步): reading and writing are separate tasks now. The old
    // loop selected `read_frame` against the outbox; whenever an outbound
    // message won, the partially-read frame future was DROPPED and the
    // next read began mid-frame — a protocol desync. Now one reader owns
    // the read half, one writer owns the write half (the client_v2
    // pattern), and the handler only ever touches channels. All three
    // tasks share the close watch: `send(true)` — or the handler's
    // disappearance — ends all of them; a writer that cannot drain tells
    // the handler through `writer_dead`.
    let (read_half, write_half) = tokio::io::split(stream);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOX_DEPTH);
    let (writer_dead_tx, mut writer_dead_rx) = mpsc::channel::<()>(1);
    let mut reader = tokio::spawn(reader_loop(
        read_half,
        inbound_tx,
        close_rx.clone(),
        FIRST_FRAME_DEADLINE,
        FRAME_IO_DEADLINE,
    ));
    let mut writer = tokio::spawn(writer_loop(
        write_half,
        out_rx,
        close_rx.clone(),
        writer_dead_tx,
        FRAME_IO_DEADLINE,
    ));
    let mut close_watch = close_rx;
    let mut bucket = TokenBucket::new(shared.limits.frame_rate_per_sec, shared.limits.frame_burst);
    log::debug!("v2 control connection from {peer}");
    loop {
        let bytes = tokio::select! {
            _ = closed(&mut close_watch) => break,
            // The writer cannot drain (stalled or failed write): the
            // connection is dead even though the socket may not know it.
            dead = writer_dead_rx.recv() => match dead {
                Some(()) => break,
                None => break,
            },
            received = inbound_rx.recv() => match received {
                Some(bytes) => bytes,
                // The reader died (deadline, EOF or I/O error).
                None => break,
            },
        };
        if !bucket.allow() {
            log::warn!("frame rate limit hit on v2 connection from {peer}");
            deliver(
                &out_tx,
                server_error(ERR_RATE_LIMITED, "frame rate exceeded"),
            );
            break;
        }
        // Direction legality at decode: a client may not emit
        // server-to-client types (decode_as enforces the frozen table).
        let frame = match CtrlFrame::decode_as(&bytes, Role::Server) {
            Ok(frame) => frame,
            Err(e) => {
                log::debug!("undecodable v2 frame from {peer}: {e}");
                break;
            }
        };
        match process(&shared, &mut state, &out_tx, frame) {
            Flow::Continue => {}
            Flow::Disconnect => break,
        }
    }
    // Teardown: wake the reader/writer, unregister, then join both
    // bounded (abort is the fallback) so neither task outlives the
    // handler. Dropping `state` afterwards also closes the watch for
    // anyone still holding a receiver.
    let _ = state.close.send(true);
    cleanup_connection(&shared, &mut state);
    drop(out_tx);
    join_or_abort(&mut reader).await;
    join_or_abort(&mut writer).await;
}

/// Wait for a task to finish within [`TEARDOWN_GRACE`]; abort it if the
/// grace expires (the task is parked on I/O that will never progress
/// once its sockets start closing).
async fn join_or_abort(task: &mut JoinHandle<()>) {
    if tokio::time::timeout(TEARDOWN_GRACE, &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = (&mut *task).await;
    }
}

/// The single reader of one connection: continuously reads frames into
/// the bounded inbound channel. The first frame (the `Hello` answering
/// the AuthChallenge) runs under `first_budget`; every later frame under
/// `steady_budget` ([`FIRST_FRAME_DEADLINE`] / [`FRAME_IO_DEADLINE`] in
/// production). `read_frame` checks the declared header length against
/// the 4092-byte payload cap BEFORE any payload allocation, so an
/// oversized declaration dies here without waiting for the bytes. Any
/// deadline, EOF or I/O error ends the reader, which closes the channel
/// and thereby ends the handler.
async fn reader_loop<R: AsyncRead + Unpin>(
    mut read: R,
    inbound: mpsc::Sender<Vec<u8>>,
    mut close: watch::Receiver<bool>,
    first_budget: Duration,
    steady_budget: Duration,
) {
    let mut first = true;
    loop {
        let budget = if first { first_budget } else { steady_budget };
        let received = tokio::select! {
            _ = closed(&mut close) => return,
            received = tokio::time::timeout(budget, read_frame(&mut read)) => received,
        };
        let bytes = match received {
            Ok(Ok(Some(bytes))) => bytes,
            // Deadline, clean EOF or I/O error: stop reading.
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return,
        };
        first = false;
        // Bounded hand-off: a handler this far behind is a flooding
        // peer, not a slow one — drop the connection.
        if inbound.try_send(bytes).is_err() {
            return;
        }
    }
}

/// The single writer of one connection: serializes every outbound frame
/// through one task (concurrent handlers can never interleave bytes
/// inside one frame) under the per-frame write budget. A stalled or
/// failed write tears the whole connection down via `close`.
async fn writer_loop<W: AsyncWrite + Unpin>(
    mut write: W,
    mut out: mpsc::Receiver<CtrlFrame>,
    mut close: watch::Receiver<bool>,
    dead: mpsc::Sender<()>,
    write_budget: Duration,
) {
    loop {
        let frame = tokio::select! {
            _ = closed(&mut close) => break,
            frame = out.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        let sent =
            tokio::time::timeout(write_budget, write_frame(&mut write, &frame, Role::Server)).await;
        if !matches!(sent, Ok(Ok(()))) {
            log::debug!("v2 outbound write stalled or failed; closing connection");
            // Tell the handler this connection is dead (best-effort: it
            // may already be tearing down).
            let _ = dead.try_send(());
            return;
        }
    }
    // Close signaled: flush any frames already queued (notably the
    // ERR_DEVICE_REPLACED reason from the replace path above) before the
    // TLS shutdown — the select above is randomly fair between `closed`
    // and `out.recv()`, so without this drain the reason frame can be
    // skipped and the peer sees a bare EOF after all.
    while let Ok(frame) = out.try_recv() {
        if !matches!(
            tokio::time::timeout(write_budget, write_frame(&mut write, &frame, Role::Server)).await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    let _ = write.shutdown().await;
}

fn cleanup_connection(shared: &Shared, state: &mut ConnState) {
    let Some(device) = state.device else {
        return;
    };
    let mut inner = shared.lock();
    if !inner
        .conns
        .get(&device)
        .map(|entry| entry.conn_id == state.conn_id)
        .unwrap_or(false)
    {
        return;
    }
    inner.conns.remove(&device);
    let sessions: Vec<SessionId> = inner
        .sessions
        .iter()
        .filter(|(_, session)| session.endpoints.contains(&device))
        .map(|(id, _)| *id)
        .collect();
    for id in sessions {
        if let Some(session) = inner.sessions.remove(&id) {
            inner.session_by_triple.remove(&session.triple);
        }
    }
    log::info!("v2 device {} disconnected", util::hex_prefix(&device, 4));
}

fn process(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    mut frame: CtrlFrame,
) -> Flow {
    if state.device.is_none() {
        return process_hello(shared, state, out, frame.message);
    }
    let me = state.device.unwrap_or([0u8; 32]);
    match frame.message {
        CtrlMessage::Hello { .. } => {
            log::debug!("duplicate Hello on authenticated v2 connection");
            Flow::Disconnect
        }
        CtrlMessage::Ping { nonce } => {
            deliver_msg(out, CtrlMessage::Pong { nonce });
            Flow::Continue
        }
        // G6 §6a: answer the revision query with this build's fingerprint
        // (sanitized to the wire budget at startup). Idempotent,
        // stateless, and rate-limited by the connection's frame token
        // bucket like every other frame; the server NEVER pushes it
        // unasked, so an old client never sees the new tag.
        CtrlMessage::ServerInfoRequest => {
            deliver_msg(
                out,
                CtrlMessage::ServerInfo {
                    revision: shared.server_revision.to_string(),
                },
            );
            Flow::Continue
        }
        CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        } => {
            // wave-28: the claim's extension section carries the owner
            // endpoint blob and the optional redeem cap.
            handle_claim(
                shared,
                out,
                me,
                request_id,
                lookup_digest,
                ttl_secs,
                frame.endpoint.take(),
                frame.max_redeems.take().unwrap_or(0),
            );
            Flow::Continue
        }
        CtrlMessage::InviteList {
            request_id,
            cursor,
            limit,
            state_filter,
        } => {
            handle_list(shared, out, me, request_id, cursor, limit, state_filter);
            Flow::Continue
        }
        CtrlMessage::InviteRevoke {
            request_id,
            code_id,
            expected_revision,
        } => {
            handle_revoke(shared, out, me, request_id, code_id, expected_revision);
            Flow::Continue
        }
        // wave-28: the redeem's extension section carries the redeemer
        // endpoint blob (forwarded to the owner with the offer).
        CtrlMessage::InviteRedeem(redeem) => {
            let endpoint = frame.endpoint.take();
            handle_redeem(shared, state, out, me, redeem, endpoint)
        }
        CtrlMessage::PairChallenge {
            transcript,
            owner_proof,
        } => {
            handle_pair_challenge(shared, me, transcript, owner_proof);
            Flow::Continue
        }
        CtrlMessage::PairProof {
            pairing_id,
            transcript_digest,
            redeemer_proof,
        } => {
            handle_pair_proof(shared, me, pairing_id, transcript_digest, redeemer_proof);
            Flow::Continue
        }
        CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest,
            signature,
        } => {
            handle_prepared(shared, out, me, pairing_id, transcript_digest, signature);
            Flow::Continue
        }
        CtrlMessage::PairApplied {
            pairing_id,
            transcript_digest,
            commit_revision,
            signature,
        } => {
            handle_applied(
                shared,
                out,
                me,
                pairing_id,
                transcript_digest,
                commit_revision,
                signature,
            );
            Flow::Continue
        }
        CtrlMessage::PairStatusQuery {
            request_id,
            pairing_id,
        } => {
            handle_status_query(shared, out, me, request_id, pairing_id);
            Flow::Continue
        }
        CtrlMessage::PairAbort {
            request_id,
            pairing_id,
            reason: _,
        } => {
            handle_abort(shared, out, me, request_id, pairing_id);
            Flow::Continue
        }
        CtrlMessage::Register { candidates } => {
            let mut inner = shared.lock();
            if let Some(entry) = inner.conns.get_mut(&me) {
                if entry.conn_id == state.conn_id {
                    entry.candidates = candidates;
                }
            }
            Flow::Continue
        }
        CtrlMessage::ConnectRequest {
            request_id: _,
            target,
            pairing_id,
            code_id,
            connection_id,
        } => {
            handle_connect(shared, out, me, target, pairing_id, code_id, connection_id);
            Flow::Continue
        }
        CtrlMessage::Signal { session, signal } => {
            handle_signal(shared, out, me, session, signal);
            Flow::Continue
        }
        CtrlMessage::Cancel { session } => {
            handle_cancel(shared, me, session);
            Flow::Continue
        }
        // Server-to-client and malformed-direction types are rejected by
        // decode_as before this point; the remainder is unreachable.
        other => {
            log::debug!("unexpected v2 message type {}", other.type_byte());
            Flow::Disconnect
        }
    }
}

// ---- hello ------------------------------------------------------------------

fn process_hello(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    message: CtrlMessage,
) -> Flow {
    let CtrlMessage::Hello {
        version,
        device_id,
        name,
        timestamp_ms,
        challenge,
        signature,
    } = message
    else {
        log::debug!("v2 connection must start with Hello");
        return Flow::Disconnect;
    };
    if version != crate::protocol_v2::VERSION.into() {
        log::debug!("hello v2 with unsupported version {version}");
        return Flow::Disconnect;
    }
    let Some(expected_nonce) = state.challenge else {
        log::debug!("hello without a live AuthChallenge");
        return Flow::Disconnect;
    };
    // The challenge is single-use and bound to this connection.
    state.challenge = None;
    if challenge != expected_nonce {
        log::debug!("hello challenge echo mismatch");
        return Flow::Disconnect;
    }
    let now = now_ms();
    if now > state.challenge_expires_ms {
        log::debug!("hello arrived after its AuthChallenge expired");
        return Flow::Disconnect;
    }
    if Duration::from_millis(now.abs_diff(timestamp_ms)) > shared.limits.hello_max_age {
        log::info!(
            "hello from {} rejected: stale",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Exporter-bound signature: valid only on this TLS connection.
    let signed = hello2::hello2_message(
        &shared.registry_id,
        &state.exporter,
        version,
        &device_id,
        &name,
        timestamp_ms,
        &challenge,
    );
    if !hello2::verify(&device_id, &signed, &signature) {
        log::info!(
            "hello from {} rejected: bad signature",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Replay window (defense in depth on top of the exporter binding and
    // the single-use challenge).
    let digest = hello_digest(&device_id, &signature);
    let registered = {
        let mut inner = shared.lock();
        if !inner
            .hello_replay
            .admit(digest, Instant::now(), shared.limits.hello_max_age)
        {
            false
        } else {
            inner.next_conn_id = inner.next_conn_id.wrapping_add(1);
            let conn_id = inner.next_conn_id;
            if let Some(previous) = inner.conns.get(&device_id) {
                // R36-P2-04 remainder (run 43321601964 diagnosis): the
                // replaced connection gets the REASON FRAME before the
                // close signal — a bare close reads as plain EOF on the
                // client, `replace_closed` never sets, and the daemon's
                // keeper reconnects IMMEDIATELY (backoff step 0),
                // killing the one-shot (invite revoke) that replaced it:
                // the exact connection war this loop used to cause by
                // silence. 0x1001/`ERR_DEVICE_REPLACED` is the
                // long-reserved code both client stacks already route.
                deliver_msg(
                    &previous.out,
                    server_error(0x1001, "replaced by a newer connection"),
                );
                let _ = previous.close.send(true);
            }
            inner.conns.insert(
                device_id,
                ConnEntry {
                    conn_id,
                    out: out.clone(),
                    close: state.close.clone(),
                    candidates: Vec::new(),
                    peer_ip: state.peer_ip,
                },
            );
            true
        }
    };
    if !registered {
        log::warn!(
            "hello from {} rejected: replay suspected",
            util::hex_prefix(&device_id, 4)
        );
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "replay suspected"));
        return Flow::Disconnect;
    }
    state.device = Some(device_id);
    state.conn_id = {
        let inner = shared.lock();
        inner
            .conns
            .get(&device_id)
            .map(|entry| entry.conn_id)
            .unwrap_or(0)
    };
    log::info!("v2 device {} online", util::hex_prefix(&device_id, 4));
    deliver_msg(
        out,
        CtrlMessage::Welcome {
            session_ttl_secs: shared.limits.session_ttl.as_secs() as u32,
            capabilities: CAPABILITIES,
        },
    );
    // Catch-up pushes: pending redeem offers for this owner, stored
    // owner challenges / redeemer proofs the other side already sent.
    catchup_after_hello(shared, &device_id);
    Flow::Continue
}

fn catchup_after_hello(shared: &Shared, me: &DeviceId) {
    match shared.store.pending_offers_for_owner(me) {
        Ok(offers) => {
            for offer in offers {
                let mut frame = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                    code_id: offer.code_id,
                    code_revision: offer.code_revision,
                    pairing_id: offer.pairing_id,
                    redeemer_device_id: offer.redeemer,
                    redeem: offer.redeem,
                });
                if let Some(blob) = offer.redeemer_endpoint {
                    frame = frame.with_endpoint(EndpointBlob(blob));
                }
                shared.deliver(me, frame);
            }
        }
        Err(e) => log::error!("pending offer catch-up failed: {e}"),
    }
    match shared.store.pending_challenges_for_redeemer(me) {
        Ok(challenges) => {
            for (_pairing_id, transcript, owner_proof) in challenges {
                shared.deliver(
                    me,
                    CtrlMessage::PairChallenge {
                        transcript,
                        owner_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("challenge catch-up failed: {e}"),
    }
    match shared.store.pending_proofs_for_owner(me) {
        Ok(proofs) => {
            for (pairing_id, transcript_digest, redeemer_proof) in proofs {
                shared.deliver(
                    me,
                    CtrlMessage::PairProof {
                        pairing_id,
                        transcript_digest,
                        redeemer_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("proof catch-up failed: {e}"),
    }
}

/// 256-bit digest of (device_id || signature) for the Hello replay
/// window — the same splitmix64-lane construction as the v1 server
/// (no crypto dependency on this plane; the v1 original and its
/// rationale live in the `legacy-plaintext-signal`-gated `crate::server`).
fn hello_digest(device_id: &DeviceId, signature: &Signature64) -> [u8; 32] {
    let mut lanes = [
        0x243F_6A88_85A3_08D3u64,
        0x1319_8A2E_0370_7344u64,
        0xA409_3822_299F_31D0u64,
        0x082E_FA98_EC4E_6C89u64,
    ];
    let absorb = |lanes: &mut [u64; 4], chunk: u64, index: usize| {
        for (i, lane) in lanes.iter_mut().enumerate() {
            *lane = lane
                .wrapping_add(chunk.rotate_left(((index * 8 + i * 3) % 64) as u32))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15 ^ ((i as u64) << 32));
        }
    };
    let mut input = [0u8; 96];
    input[..32].copy_from_slice(device_id);
    input[32..].copy_from_slice(signature);
    let mut chunks = [0u64; 12];
    for (i, chunk) in chunks.iter_mut().enumerate() {
        *chunk = u64::from_le_bytes(input[i * 8..i * 8 + 8].try_into().unwrap());
    }
    for (index, chunk) in chunks.iter().enumerate() {
        absorb(&mut lanes, *chunk, index);
    }
    let mut out = [0u8; 32];
    for (i, lane) in lanes.iter().enumerate() {
        let mut z = *lane;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out[i * 8..i * 8 + 8].copy_from_slice(&z.to_le_bytes());
    }
    out
}

/// Sliding-window memory of accepted Hello signature digests (v1
/// semantics, server-wide across connections).
#[derive(Debug, Default)]
struct HelloReplayCache {
    seen: VecDeque<([u8; 32], Instant)>,
}

impl HelloReplayCache {
    fn admit(&mut self, digest: [u8; 32], now: Instant, window: Duration) -> bool {
        while let Some((_, at)) = self.seen.front() {
            if now.duration_since(*at) > window {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        if self.seen.iter().any(|(seen, _)| *seen == digest) {
            return false;
        }
        if self.seen.len() >= crate::util::HELLO_REPLAY_CAPACITY {
            self.seen.pop_front();
        }
        self.seen.push_back((digest, now));
        true
    }
}

// ---- invite management --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_claim(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    lookup_digest: Digest32,
    ttl_secs: u32,
    owner_endpoint: Option<EndpointBlob>,
    max_redeems: u32,
) {
    if ttl_secs != 0
        && !(crate::server_v2::config::INVITE_TTL_MIN_SECS
            ..=crate::server_v2::config::INVITE_TTL_MAX_SECS)
            .contains(&ttl_secs)
    {
        deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return;
    }
    let now = now_ms();
    match shared.store.claim(
        &me,
        &request_id,
        &lookup_digest,
        ttl_secs,
        max_redeems,
        owner_endpoint.as_ref().map(|blob| blob.0.as_slice()),
        now,
    ) {
        Ok(ClaimOutcome::Claimed {
            code_id,
            created_ms,
            expires_ms,
            revision,
        }) => {
            deliver_msg(
                out,
                CtrlMessage::InviteClaimed {
                    request_id,
                    code_id,
                    created_ms,
                    expires_ms,
                    revision,
                },
            );
        }
        Ok(ClaimOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(ClaimOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::CodeConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                CODE_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::RateLimited) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RATE_LIMITED,
                60_000,
            ),
        ),
        Ok(ClaimOutcome::ResourceLimit) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RESOURCE_LIMIT,
                0,
            ),
        ),
        Err(e) => {
            log::error!("claim storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

/// Decode a frame the server itself produced and journaled. Invariant:
/// these bytes round-tripped `try_encode_as`; failure is a bug, not an
/// input, so it surfaces as a storage error reply.
fn decode_stored(bytes: &[u8]) -> CtrlMessage {
    CtrlFrame::decode(bytes)
        .map(|frame| frame.message)
        .unwrap_or(CtrlMessage::ServerError {
            code: STORAGE_UNAVAILABLE,
            message: "stored response unreadable".to_string(),
        })
}

fn handle_list(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    cursor: CodeId,
    limit: u8,
    state_filter: StateFilter,
) {
    let now = now_ms();
    match shared.store.list(&me, &cursor, limit, state_filter, now) {
        Ok(entries) => {
            let next_cursor = if entries.len() == limit as usize {
                entries.last().map(|e| e.code_id).unwrap_or(CURSOR_END)
            } else {
                CURSOR_END
            };
            deliver_msg(
                out,
                CtrlMessage::InviteListPage {
                    request_id,
                    next_cursor,
                    entries,
                },
            );
        }
        Err(e) => {
            log::error!("list storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_revoke(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    code_id: CodeId,
    expected_revision: u64,
) {
    let now = now_ms();
    match shared
        .store
        .revoke(&me, &request_id, &code_id, expected_revision, now)
    {
        Ok(RevokeOutcome::Revoked {
            revision,
            revoked_ms,
            aborted,
        }) => {
            deliver_msg(
                out,
                CtrlMessage::InviteRevoked {
                    request_id,
                    code_id,
                    revision,
                    revoked_ms,
                },
            );
            for pairing in aborted {
                notify_pairing_unavailable(shared, &pairing.owner, pairing.pairing_id);
                notify_pairing_unavailable(shared, &pairing.redeemer, pairing.pairing_id);
            }
        }
        Ok(RevokeOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(RevokeOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(RevokeOutcome::RevisionConflict) => deliver_msg(
            out,
            pair_error(request_id, ErrorScope::Code, code_id, REVISION_CONFLICT, 0),
        ),
        Ok(RevokeOutcome::Unavailable) => {
            // Unknown, foreign, or terminal — one answer.
            deliver_msg(
                out,
                pair_error(request_id, ErrorScope::Code, code_id, PAIR_UNAVAILABLE, 0),
            );
        }
        Err(e) => {
            log::error!("revoke storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn notify_pairing_unavailable(shared: &Shared, device: &DeviceId, pairing_id: PairingId) {
    shared.deliver(device, unavailable([0u8; 16], pairing_id));
}

// ---- redeem -------------------------------------------------------------------

fn handle_redeem(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    redeem: crate::protocol_v2::RedeemRequest,
    redeemer_endpoint: Option<EndpointBlob>,
) -> Flow {
    let now_instant = Instant::now();
    // Budget rejections run before the lookup and are immediate. The
    // per-connection window first (tightest, no shared state), then the
    // five shared layers (CR-P1-03 + CR-P2-10): per /64 source, the
    // IPv6 /56 and /48 aggregates, per identity, and the global breaker
    // with fair-share shedding — a flood from one source (even across
    // self-generated identities, reconnects, or /64 rotation inside its
    // delegation) exhausts only its own windows and can no longer flip
    // every other source's redeem to RATE_LIMITED.
    if !state.redeem_window.allow_at(now_instant) {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                state.redeem_window.retry_after_ms(now_instant),
            ),
        );
        return Flow::Continue;
    }
    let (shared_ok, retry) = shared.redeem_allows(source_key(state.peer_ip), &me, now_instant);
    if !shared_ok {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                retry,
            ),
        );
        return Flow::Continue;
    }
    let now = now_ms();
    // Deadline sanity (one attempt spans at most 120 s, contract §8).
    if redeem.deadline_ms <= now || redeem.deadline_ms > now + REDEEM_MAX_WINDOW.as_millis() as u64
    {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return Flow::Continue;
    }
    // Signature over the frozen redeem domain (MAC stays endpoint-only).
    let signed = hello2::redeem_message(
        &shared.registry_id,
        &redeem.lookup_digest,
        &me,
        &redeem.request_id,
        &redeem.redeemer_nonce,
        redeem.deadline_ms,
        &redeem.request_mac,
    );
    if !hello2::verify(&me, &signed, &redeem.signature) {
        // A forged redeem request is a protocol violation: disconnect.
        log::info!(
            "redeem from {} rejected: bad signature",
            util::hex_prefix(&me, 4)
        );
        return Flow::Disconnect;
    }
    // P2 (redeem 幂等): a retry with the same (redeemer, request_id)
    // replays the ORIGINAL response — minting a fresh pairing id per
    // retry forked the transaction (a second pending row for a real
    // code, a second ghost otherwise, and a second offer pushed to the
    // owner). Same journal discipline as the invite claim path; a
    // different attempt reusing the id is a REQUEST_CONFLICT.
    {
        let inner = shared.lock();
        let replay = inner.redeem_journal.get(&(me, redeem.request_id)).copied();
        match replay {
            Some(flags) if flags.matches(&redeem) => {
                drop(inner);
                deliver_msg(
                    out,
                    CtrlMessage::InviteRedeemPending {
                        request_id: redeem.request_id,
                        pairing_id: flags.pairing_id,
                        deadline_ms: flags.deadline_ms,
                    },
                );
                return Flow::Continue;
            }
            Some(_) => {
                drop(inner);
                deliver_msg(
                    out,
                    pair_error(
                        redeem.request_id,
                        ErrorScope::Request,
                        redeem.request_id,
                        REQUEST_CONFLICT,
                        0,
                    ),
                );
                return Flow::Continue;
            }
            None => {}
        }
    }
    let code = match shared.store.find_active_code(&redeem.lookup_digest, now) {
        Ok(code) => code,
        Err(e) => {
            log::error!("redeem lookup failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    redeem.request_id,
                    ErrorScope::Request,
                    redeem.request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
            return Flow::Continue;
        }
    };
    let pairing_id = random_id16();
    match code {
        // A live code owned by someone else: the real redeem path.
        Some(code) if code.owner != me => {
            match shared.store.begin_pairing(
                &pairing_id,
                &me,
                &redeem,
                &code,
                redeemer_endpoint.as_ref().map(|blob| blob.0.as_slice()),
                now,
            ) {
                Ok(BeginRedeem::Begun) => {
                    let expires_at =
                        now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
                    record_redeem_replay(
                        &mut shared.lock(),
                        &me,
                        &redeem,
                        pairing_id,
                        expires_at + REDEEM_REPLAY_GRACE,
                    );
                    deliver_msg(
                        out,
                        CtrlMessage::InviteRedeemPending {
                            request_id: redeem.request_id,
                            pairing_id,
                            deadline_ms: redeem.deadline_ms,
                        },
                    );
                    // Owner online: offer immediately; offline: the
                    // pending row waits and is pushed on the owner's next
                    // Hello (catch-up in process_hello). Wave-28: the
                    // redeemer's endpoint rides the offer frame, so the
                    // owner has the peer settings in this one push.
                    let mut offer = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                        code_id: code.code_id,
                        code_revision: code.revision,
                        pairing_id,
                        redeemer_device_id: me,
                        redeem,
                    });
                    if let Some(blob) = redeemer_endpoint {
                        offer = offer.with_endpoint(blob);
                    }
                    shared.deliver(&code.owner, offer);
                }
                Ok(BeginRedeem::OwnerPendingLimit | BeginRedeem::GlobalPendingLimit) => {
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            RESOURCE_LIMIT,
                            0,
                        ),
                    );
                }
                // Wave-28 single-redemption cap: immediate, explicit —
                // no deferred deadline semantics (the code is known to
                // exist; the redeemer already holds it).
                Ok(BeginRedeem::RedeemExhausted) => {
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Code,
                            code.code_id,
                            REDEEM_EXHAUSTED,
                            0,
                        ),
                    );
                }
                Err(e) => {
                    log::error!("begin pairing failure: {e}");
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            STORAGE_UNAVAILABLE,
                            0,
                        ),
                    );
                }
            }
        }
        // Unknown, expired, revoked, or self-redeem: same pending shape,
        // unified failure at the deadline (no existence oracle).
        _ => {
            let expires_at =
                now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
            {
                let mut inner = shared.lock();
                inner.ghosts.insert(
                    pairing_id,
                    Ghost {
                        requester: me,
                        request_id: redeem.request_id,
                        deadline_ms: redeem.deadline_ms,
                        expires_at,
                    },
                );
                record_redeem_replay(
                    &mut inner,
                    &me,
                    &redeem,
                    pairing_id,
                    expires_at + REDEEM_REPLAY_GRACE,
                );
            }
            deliver_msg(
                out,
                CtrlMessage::InviteRedeemPending {
                    request_id: redeem.request_id,
                    pairing_id,
                    deadline_ms: redeem.deadline_ms,
                },
            );
        }
    }
    Flow::Continue
}

/// Bounded insert into the redeem idempotency journal: expired entries
/// leave first, then arbitrary victims until there is room (the cap is
/// [`JOURNAL_MAX_ROWS`], same bound as the store's journal).
fn record_redeem_replay(
    inner: &mut Inner,
    redeemer: &DeviceId,
    redeem: &RedeemRequest,
    pairing_id: PairingId,
    expires_at: Instant,
) {
    let entry = RedeemReplay {
        lookup_digest: redeem.lookup_digest,
        redeemer_nonce: redeem.redeemer_nonce,
        request_mac: redeem.request_mac,
        deadline_ms: redeem.deadline_ms,
        pairing_id,
        expires_at,
    };
    inner
        .redeem_journal
        .retain(|_, existing| existing.expires_at > Instant::now());
    while inner.redeem_journal.len() >= JOURNAL_MAX_ROWS {
        let victim = *inner
            .redeem_journal
            .keys()
            .next()
            .expect("len checked above");
        inner.redeem_journal.remove(&victim);
    }
    inner
        .redeem_journal
        .insert((*redeemer, redeem.request_id), entry);
}

// ---- pairing handshakes --------------------------------------------------------

/// Pure transcript-binding check (`pairing-flow.md` §7: key/code
/// substitution detection). The transcript must name exactly the
/// registry, code, ids, keys, nonce, request and deadline the durable
/// row recorded.
fn transcript_bindings_ok(
    typed: &PairTranscript,
    snapshot: &PairingSnapshot,
    registry_id: &RegistryId,
) -> bool {
    typed.registry_id == *registry_id
        && typed.code_id == snapshot.code_id
        && typed.code_revision == snapshot.code_revision
        && typed.pairing_id == snapshot.pairing_id
        && typed.request_id == snapshot.request_id
        && typed.lookup_digest == snapshot.lookup_digest
        && typed.owner_device_id == snapshot.owner
        && typed.redeemer_device_id == snapshot.redeemer
        && typed.redeemer_nonce == snapshot.redeemer_nonce
        && typed.deadline_ms == snapshot.deadline_ms
}

fn handle_pair_challenge(
    shared: &Shared,
    me: DeviceId,
    transcript: [u8; 256],
    owner_proof: PairProof,
) {
    let typed = PairTranscript::from_bytes(&transcript);
    let now = now_ms();
    let snapshot = shared.store.pairing_snapshot(&typed.pairing_id);
    let snapshot = match snapshot {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair challenge for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    // The owner is the only legitimate author of a PairChallenge; wrong
    // sender, wrong state, deadline passed, or any binding mismatch is a
    // protocol failure. No reply distinguishes these (no oracle); the
    // pending transaction simply fails at its deadline.
    let valid_sender = snapshot.owner == me
        && snapshot.state == PairingState::Pending
        && now <= snapshot.deadline_ms;
    if !valid_sender || !transcript_bindings_ok(&typed, &snapshot, &shared.registry_id) {
        log::info!(
            "pair challenge from {} rejected: bindings",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    // Server-verifiable part of the proof: the Ed25519 signature over the
    // frozen owner domain, canonical transcript and MAC. The MAC itself
    // is endpoint-only material and is deliberately not checked here.
    let signed = hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &transcript, &owner_proof.mac);
    if !hello2::verify(&typed.owner_device_id, &signed, &owner_proof.signature) {
        log::info!("pair challenge rejected: owner signature");
        return;
    }
    let digest = hello2::transcript_digest(&transcript);
    match shared.store.store_owner_challenge(
        &typed.pairing_id,
        &me,
        &transcript,
        &digest,
        &owner_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.redeemer,
                CtrlMessage::PairChallenge {
                    transcript,
                    owner_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair challenge lost its race; dropped"),
        Err(e) => log::error!("store owner challenge failure: {e}"),
    }
}

fn handle_pair_proof(
    shared: &Shared,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    redeemer_proof: PairProof,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair proof for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    if snapshot.redeemer != me
        || snapshot.state != PairingState::Pending
        || now > snapshot.deadline_ms
        || snapshot.transcript_digest != Some(transcript_digest)
    {
        log::info!(
            "pair proof from {} rejected: bindings/digest",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    let transcript = match snapshot.transcript {
        Some(transcript) => transcript,
        None => return,
    };
    let signed =
        hello2::proof_signed_message(REDEEMER_PROOF_DOMAIN, &transcript, &redeemer_proof.mac);
    if !hello2::verify(&snapshot.redeemer, &signed, &redeemer_proof.signature) {
        log::info!("pair proof rejected: redeemer signature");
        return;
    }
    match shared.store.store_redeemer_proof(
        &pairing_id,
        &me,
        &transcript_digest,
        &redeemer_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.owner,
                CtrlMessage::PairProof {
                    pairing_id,
                    transcript_digest,
                    redeemer_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair proof lost its race; dropped"),
        Err(e) => log::error!("store redeemer proof failure: {e}"),
    }
}

/// The authoritative commit frame. Wave-28: the owner endpoint snapshot
/// rides the extension section, so the redeemer receives the owner's
/// connection settings at the same commit point (identical frame to both
/// participants; the owner merely sees its own blob again).
fn commit_message(snapshot: &PairingSnapshot) -> Option<CtrlFrame> {
    let mut frame = CtrlFrame::new(CtrlMessage::PairCommit {
        transcript: snapshot.transcript?,
        owner_proof: snapshot.owner_proof.clone()?,
        redeemer_proof: snapshot.redeemer_proof.clone()?,
        owner_prepared_signature: snapshot.owner_prepared_signature?,
        redeemer_prepared_signature: snapshot.redeemer_prepared_signature?,
        commit_revision: snapshot.commit_revision?,
    });
    if let Some(blob) = &snapshot.owner_endpoint {
        frame = frame.with_endpoint(EndpointBlob(blob.clone()));
    }
    Some(frame)
}

fn ready_message(snapshot: &PairingSnapshot) -> Option<CtrlMessage> {
    Some(CtrlMessage::PairReady {
        pairing_id: snapshot.pairing_id,
        transcript_digest: snapshot.transcript_digest?,
        commit_revision: snapshot.commit_revision?,
        owner_applied_signature: snapshot.owner_applied_signature?,
        redeemer_applied_signature: snapshot.redeemer_applied_signature?,
    })
}

fn handle_prepared(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || now > snapshot.deadline_ms
    {
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::prepared_message(&pairing_id, &transcript_digest);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "prepared from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_prepared_and_try_commit(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        now,
    );
    match decision {
        Ok(PreparedDecision::WaitingOther) => {}
        Ok(PreparedDecision::Committed { .. })
        | Ok(PreparedDecision::AlreadyFinal(PairingState::Committed | PairingState::Applied)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = commit_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(PairingState::Ready)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&me, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(_)) => {}
        Ok(PreparedDecision::Unavailable) => {
            // The aborting path (revoke, sweep, explicit abort) already
            // notified both participants; tell only the sender.
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Ok(PreparedDecision::RedeemExhausted) => {
            // P1-03: the cap bound at the commit linearization point — a
            // competing pending won this code's last redeem slot. The
            // store aborted this pairing without consuming the cap; both
            // participants get the explicit failure. Wave-30 (recheck
            // R29-P2-01): the error is PAIRING-scoped with THIS pairing's
            // id — the production RegistryClient dispatch routes
            // pairing-scope errors to the pairing inbox/plane and the
            // orchestrator's wait_frame terminates on
            // object_id == pairing_id, so the losing flow actually
            // receives the terminal 0x2109 instead of waiting out its
            // deadline (a Code-scoped frame with the all-zero request id
            // had no waiter and no fan-out — silently dropped).
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REDEEM_EXHAUSTED,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
        }
        Ok(PreparedDecision::ResourceLimit) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    RESOURCE_LIMIT,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
        }
        Ok(PreparedDecision::NotFound) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("prepared storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_applied(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    commit_revision: u64,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || !matches!(
            snapshot.state,
            PairingState::Committed | PairingState::Applied | PairingState::Ready
        )
    {
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::applied_message(&pairing_id, &transcript_digest, commit_revision);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "applied from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_applied(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        commit_revision,
        now,
    );
    match decision {
        Ok(AppliedDecision::WaitingOther) => {}
        Ok(AppliedDecision::Ready) | Ok(AppliedDecision::AlreadyReady) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(AppliedDecision::RevisionConflict) => {
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AppliedDecision::NotFound) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("applied storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

fn handle_status_query(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // P2 (反枚举): a live ghost of THIS requester answers with the exact
    // `PairStatusReply` shape a real pending transaction produces.
    // Before, the ghost fell straight through to the SQLite lookup, hit
    // `no row`, and answered `PAIR_UNAVAILABLE` immediately — one status
    // query right after `InviteRedeemPending` told a redeemer whether the
    // code existed, defeating the unified-deadline design. Ghost and real
    // now share one answer shape while the attempt is live; after the
    // deadline the ghost is gone and the same single SQLite query yields
    // the unified `PAIR_UNAVAILABLE` for both.
    {
        let inner = shared.lock();
        let live_deadline = inner
            .ghosts
            .get(&pairing_id)
            .filter(|ghost| ghost.requester == me && ghost.expires_at > Instant::now())
            .map(|ghost| ghost.deadline_ms);
        if let Some(deadline_ms) = live_deadline {
            drop(inner);
            deliver_msg(
                out,
                CtrlMessage::PairStatusReply {
                    request_id,
                    pairing_id,
                    state: PairingState::Pending,
                    commit_revision: 0,
                    deadline_ms,
                },
            );
            return;
        }
    }
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        // Unknown or non-participant: one unified answer.
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable(request_id, pairing_id));
            return;
        }
    };
    if snapshot.owner != me && snapshot.redeemer != me {
        deliver_msg(out, unavailable(request_id, pairing_id));
        return;
    }
    deliver_msg(
        out,
        CtrlMessage::PairStatusReply {
            request_id,
            pairing_id,
            state: snapshot.state,
            commit_revision: snapshot.commit_revision.unwrap_or(0),
            deadline_ms: snapshot.deadline_ms,
        },
    );
    // Recovery resends after the reply (contract §6): committed
    // transactions re-deliver the fixed PairCommit, ready ones the fixed
    // PairReady.
    match snapshot.state {
        PairingState::Committed | PairingState::Applied => {
            if let Some(frame) = commit_message(&snapshot) {
                deliver(out, frame);
            }
        }
        PairingState::Ready => {
            if let Some(frame) = ready_message(&snapshot) {
                deliver(out, frame);
            }
        }
        _ => {}
    }
}

fn handle_abort(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // Participants only; unknown pairings and foreign ones look the same.
    let snapshot = shared.store.pairing_snapshot(&pairing_id);
    let participant = matches!(&snapshot, Ok(Some(s)) if s.owner == me || s.redeemer == me);
    if !participant {
        deliver_msg(out, unavailable(request_id, pairing_id));
        return;
    }
    match shared.store.abort_pairing(&pairing_id, now_ms()) {
        Ok(AbortDecision::Aborted(aborted)) => {
            // The other participant learns the pairing is gone; the
            // aborter gets silence (idempotent success).
            let other = if aborted.owner == me {
                aborted.redeemer
            } else {
                aborted.owner
            };
            shared.deliver(&other, unavailable([0u8; 16], pairing_id));
        }
        Ok(AbortDecision::NotAbortable) => {
            // Committed transactions must be recovered, not dropped.
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AbortDecision::NotFound) => {
            deliver_msg(out, unavailable(request_id, pairing_id));
        }
        Err(e) => log::error!("abort storage failure: {e}"),
    }
}

// ---- v2 sessions (contract §10) ------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_connect(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    target: DeviceId,
    pairing_id: PairingId,
    code_id: CodeId,
    connection_id: [u8; 16],
) {
    if target == me {
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "self connect"));
        return;
    }
    // v2 sessions are keyed by the verified triple: a completed pairing
    // relation must exist for exactly this (pairing_id, code_id) and this
    // endpoint pair.
    let verified = shared
        .store
        .relation_exists(&pairing_id, &code_id, &me, &target)
        .unwrap_or(false);
    if !verified {
        deliver_msg(
            out,
            pair_error(
                [0u8; 16],
                ErrorScope::Connection,
                connection_id,
                PAIR_UNAVAILABLE,
                0,
            ),
        );
        return;
    }
    let mut inner = shared.lock();
    if !inner.conns.contains_key(&target) {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_UNKNOWN_DEVICE, "target device not online"),
        );
        return;
    }
    let triple = (pairing_id, code_id, connection_id);
    // Idempotent: an existing session for the same triple is re-announced.
    if let Some(existing) = inner.session_by_triple.get(&triple) {
        if let Some(entry) = inner.sessions.get(existing) {
            let entry = entry.clone();
            let session = *existing;
            drop(inner);
            announce_session(shared, &entry, session, me, target);
            return;
        }
    }
    let my_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&me))
        .count();
    let target_count = inner
        .sessions
        .values()
        .filter(|session| session.endpoints.contains(&target))
        .count();
    if my_count >= shared.limits.max_sessions_per_device
        || target_count >= shared.limits.max_sessions_per_device
    {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_SESSION_LIMIT, "session limit reached"),
        );
        return;
    }
    let mut session_id = random_id16();
    for _ in 0..8 {
        if !inner.sessions.contains_key(&session_id) {
            break;
        }
        session_id = random_id16();
    }
    let entry = SessionEntry {
        triple,
        endpoints: [me, target],
        expires_at: Instant::now() + shared.limits.session_ttl,
    };
    inner.sessions.insert(session_id, entry.clone());
    inner.session_by_triple.insert(triple, session_id);
    drop(inner);
    announce_session(shared, &entry, session_id, me, target);
}

fn announce_session(
    shared: &Shared,
    session_entry: &SessionEntry,
    session: SessionId,
    me: DeviceId,
    target: DeviceId,
) {
    let expires_ms = now_ms() + shared.limits.session_ttl.as_millis() as u64;
    let (pairing_id, code_id, connection_id) = session_entry.triple;
    for (device, peer) in [(me, target), (target, me)] {
        shared.deliver(
            &device,
            CtrlMessage::SessionOpened {
                session,
                peer,
                pairing_id,
                code_id,
                connection_id,
                expires_ms,
            },
        );
    }
    // SameEgress hint: both endpoints' control-plane peer IPs known and
    // identical (ordering hint only, never a trust claim).
    let inner = shared.lock();
    let me_ip = inner.conns.get(&me).map(|entry| entry.peer_ip);
    let target_ip = inner.conns.get(&target).map(|entry| entry.peer_ip);
    drop(inner);
    if crate::util::same_egress(me_ip, target_ip) {
        for device in [me, target] {
            shared.deliver(
                &device,
                CtrlMessage::Signal {
                    session,
                    signal: crate::protocol_v2::PeerSignal::SameEgress,
                },
            );
        }
    }
}

fn handle_signal(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    session: SessionId,
    signal: crate::protocol_v2::PeerSignal,
) {
    let inner = shared.lock();
    let Some(entry) = inner.sessions.get(&session) else {
        drop(inner);
        deliver_msg(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    };
    if !entry.endpoints.contains(&me) {
        drop(inner);
        // Unknown and non-member sessions look identical (no oracle).
        deliver_msg(
            out,
            server_error(ERR_SESSION_NOT_FOUND, "session not found"),
        );
        return;
    }
    let other = if entry.endpoints[0] == me {
        entry.endpoints[1]
    } else {
        entry.endpoints[0]
    };
    drop(inner);
    // Verbatim relay of one bounded, structured signal.
    shared.deliver(&other, CtrlMessage::Signal { session, signal });
}

fn handle_cancel(shared: &Shared, me: DeviceId, session: SessionId) {
    let mut inner = shared.lock();
    let mine = inner
        .sessions
        .get(&session)
        .map(|entry| entry.endpoints.contains(&me))
        .unwrap_or(false);
    if mine {
        if let Some(entry) = inner.sessions.remove(&session) {
            inner.session_by_triple.remove(&entry.triple);
        }
    }
    // Cancelling an unknown session is idempotent, not an error.
}

// ---- sweeper --------------------------------------------------------------------

fn sweep(shared: &Shared) {
    let now = now_ms();
    let mut expired_ghosts: Vec<Ghost> = Vec::new();
    {
        let mut inner = shared.lock();
        // CR-P1-03: idle accounting entries go back to nothing (see
        // [`Inner::prune_source_tables`]).
        inner.prune_source_tables(Instant::now());
        let due: Vec<PairingId> = inner
            .ghosts
            .iter()
            .filter(|(_, ghost)| ghost.expires_at <= Instant::now())
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            if let Some(ghost) = inner.ghosts.remove(&id) {
                expired_ghosts.push(ghost);
            }
        }
        // Redeem replay records live only until deadline + grace.
        inner
            .redeem_journal
            .retain(|_, replay| replay.expires_at > Instant::now());
        // Session TTLs (coordination sessions only; data channels are
        // unaffected by coordination-session death).
        let dead: Vec<SessionId> = inner
            .sessions
            .iter()
            .filter(|(_, session)| session.expires_at <= Instant::now())
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            if let Some(entry) = inner.sessions.remove(&id) {
                inner.session_by_triple.remove(&entry.triple);
            }
        }
    }
    for ghost in expired_ghosts {
        log::debug!(
            "ghost redeem expired at its {} ms deadline",
            ghost.deadline_ms
        );
        shared.deliver(
            &ghost.requester,
            pair_error(
                ghost.request_id,
                ErrorScope::Pairing,
                [0u8; 16],
                PAIR_UNAVAILABLE,
                0,
            ),
        );
    }
    let report: DueReport = match shared.store.expire_due(now) {
        Ok(report) => report,
        Err(e) => {
            log::error!("sweep expiry failure: {e}");
            return;
        }
    };
    for aborted in report.aborted {
        notify_pairing_unavailable(shared, &aborted.owner, aborted.pairing_id);
        notify_pairing_unavailable(shared, &aborted.redeemer, aborted.pairing_id);
    }
}

#[cfg(test)]
mod tests;
