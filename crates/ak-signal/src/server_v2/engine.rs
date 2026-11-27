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

/// One decision line of the pairing engine (Q-N3): a DEBUG log in every
/// build and, in this crate's own tests, a line in the tests' sink, so a
/// test can check that the diagnostics name the branch it drove.
pub(super) fn decision(args: std::fmt::Arguments<'_>) {
    log::debug!("{args}");
    #[cfg(test)]
    tests::DECISIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(args.to_string());
}
macro_rules! decision {
    ($($arg:tt)*) => {
        decision(format_args!($($arg)*))
    };
}

mod conn;
mod hello;
mod invites;
mod pairing;
mod sessions;
mod state;

use conn::*;
use hello::*;
use invites::*;
use pairing::*;
use sessions::*;
use state::*;

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
        #[cfg(test)]
        sweep_held: std::sync::atomic::AtomicBool::new(false),
    });
    #[cfg(test)]
    tests::register_shared(&shared);
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
            _ = tick.tick() => {
                if !shared.sweep_held() {
                    sweep(&shared);
                }
            }
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
        #[cfg(test)]
        tests::DECISIONS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!(
                "ghost redeem expired at its {} ms deadline",
                ghost.deadline_ms
            ));
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
    // Q-N3 recurrence (2026-11-27): one decision line per pairing — it
    // names the pairing the participants' `unavailable` will carry. Review
    // 20: each notification is then delivered with its own line, numbered
    // per connection in the order the outbox took it, and saying whether
    // it was queued at all.
    for aborted in report.aborted {
        decision!(
            "sweep: pairing {} is past its deadline and is aborted; notifying its owner and its redeemer",
            util::hex_prefix(&aborted.pairing_id, 4)
        );
        shared.deliver_noted(
            &aborted.owner,
            "owner",
            aborted.pairing_id,
            "sweep: the pairing is past its deadline and was aborted",
            unavailable([0u8; 16], aborted.pairing_id),
        );
        shared.deliver_noted(
            &aborted.redeemer,
            "redeemer",
            aborted.pairing_id,
            "sweep: the pairing is past its deadline and was aborted",
            unavailable([0u8; 16], aborted.pairing_id),
        );
    }
}

#[cfg(test)]
mod tests;
