//! `send`: address candidates with fallback, connect budgets and the
//! scheduler admission path.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// send: multi-address candidates with automatic fallback (wave 25)
// ---------------------------------------------------------------------------

/// Default per-address connect deadline of the send fallback (seconds):
/// an address whose bind/connect/handshake phase has not completed within
/// this budget is abandoned for the next candidate. The budget starts when
/// the engine reports `Progress::Connecting` — i.e. after the synchronous
/// source scan — and no longer applies once the transfer is connected (see
/// [`attempt_send_to`] and [`drive_with_connect_budget`]).
///
/// 10s (was 3s): the QUIC+TLS handshake needs several round trips, and a
/// real cross-continent run through a VPN exit measured over 3s on a slow
/// day — BOTH wave-25 (3s around the whole future) and the split-budget
/// wave-26 driver failed W→L at exactly 3s while the reverse direction
/// (China→local, fewer RTTs through the direct path) kept working. LAN
/// candidates still connect in milliseconds, so a larger budget costs
/// nothing on fast paths; `--connect-timeout` tunes it per environment.
pub(super) const SEND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Brief pause between address attempts: the failed attempt's QUIC endpoint
/// releases its UDP socket asynchronously after drop, so an immediate rebind
/// on an explicit --port could hit AddrInUse (same reasoning as the receive
/// loop's rebind retries).
const SEND_ATTEMPT_PAUSE: Duration = Duration::from_millis(150);

/// Sender-side LAN preference for one target: RFC1918 private IPv4 (and any
/// loopback) sorts before public IPv4 and IPv6. Reuses
/// `ak_net::classify::is_ipv4_private`; the classification is a routing
/// hint, never an authorization.
fn send_target_prefers_lan(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => ak_net::classify::is_ipv4_private(v4),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Dedupe the --to list, normalize v4-mapped candidates to plain IPv4
/// (`[::ffff:1.2.3.4]:port` dials as `1.2.3.4:port` — `ak_net`'s connect
/// bridges families for dual-stack sockets, but the engine's send-path
/// family fast-fail compares raw families, so the dialable plain form is
/// the canonical one), then stably sort LAN-first. Stable: the user's
/// order survives within the LAN group and within the public group.
pub(crate) fn order_send_targets(targets: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut ordered: Vec<SocketAddr> = Vec::with_capacity(targets.len());
    for addr in targets {
        let addr = ak_net::classify::expand_mapped(addr);
        if !ordered.contains(&addr) {
            ordered.push(addr);
        }
    }
    ordered.sort_by_key(|addr| !send_target_prefers_lan(addr));
    ordered
}

/// Local wildcard bind matching the candidate's address family (review
/// P2-01): the engine rejects a local/peer family mismatch before dialing,
/// so an IPv6 candidate must bind `[::]:port` (same-family wildcard), not
/// `0.0.0.0:port`. The explicit `--port` semantics survive unchanged —
/// same port, family-matched wildcard — and the per-attempt pause
/// ([`SEND_ATTEMPT_PAUSE`]) still gives the previous attempt's socket time
/// to release the port.
pub(crate) fn send_bind_for(addr: SocketAddr, port: u16) -> SocketAddr {
    if addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), port)
    } else {
        SocketAddr::from(([0, 0, 0, 0], port))
    }
}

/// Why one address attempt failed (wave 25 fallback bookkeeping).
#[derive(Debug)]
pub(super) struct AttemptFailure {
    pub(super) addr: SocketAddr,
    /// One-line reason for the aggregate error and the progress line.
    pub(super) reason: String,
    /// The original engine error (kept for the friendly-hint mapping).
    pub(super) err: EngineError,
    /// True when the engine had already reported `Connected`: a
    /// transfer-phase failure another address cannot fix.
    pub(super) after_connect: bool,
    /// Send-path hint that overrides [`errors::friendly_send_hint`] for
    /// this failure (wave 27: the scheduler-admission BUSY copy). `None`
    /// everywhere except an admission denial. Boxed to keep the struct
    /// under the `result_large_err` budget (the hint exists on exactly
    /// one failure shape; the indirection is free where it is absent).
    pub(super) hint: Option<Box<str>>,
}

impl AttemptFailure {
    /// The connect-deadline failure of [`attempt_send_to`].
    pub(super) fn connect_timeout(addr: SocketAddr) -> Self {
        AttemptFailure {
            addr,
            reason: format!(
                "timeout after {}s (no QUIC handshake)",
                SEND_CONNECT_TIMEOUT.as_secs()
            ),
            err: EngineError::IdleTimeout(SEND_CONNECT_TIMEOUT),
            after_connect: false,
            hint: None,
        }
    }

    /// Short tag for the `[send] failed (...)` progress line.
    fn short(&self) -> String {
        if self.reason.starts_with("timeout") {
            "timeout".to_owned()
        } else {
            clip_line(self.reason.split(['\n', '\r']).next().unwrap_or(""), 60).to_owned()
        }
    }
}

/// Single-line rendering of an engine error for progress/summary lines
/// (multi-line Display output would garble the fallback report).
pub(crate) fn one_line_reason(err: &EngineError) -> String {
    let text = err.to_string();
    let first = text.split(['\n', '\r']).next().unwrap_or("");
    clip_line(first, 120).to_owned()
}

/// Connect-phase bookkeeping for one send attempt (review P1-01). The
/// engine invokes its progress callback synchronously inside the polled
/// future, so every mark below is set while the driver task is inside
/// `send.poll()` itself — there is no cross-task race, and the recorded
/// instant is exactly when the engine moved from preparation to binding.
pub(super) struct ConnectPhaseMarks {
    /// When `Progress::Connecting` fired: `prepare_send` (the synchronous
    /// full-source scan: open every source, whole + chunk digests, sender
    /// checkpoint) is done and bind/connect/handshake start now.
    connecting_at: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Wakes [`drive_with_connect_budget`] when the connect phase starts.
    /// `Notify` stores a permit when there is no waiter yet, so the signal
    /// survives whichever order poll and callback run in.
    connecting: tokio::sync::Notify,
    /// Set on `Progress::Connected`: the handshake completed and the
    /// transfer is running under its own idle budget.
    pub(super) connected: std::sync::atomic::AtomicBool,
}

impl ConnectPhaseMarks {
    pub(super) fn new() -> Self {
        Self {
            connecting_at: std::sync::Mutex::new(None),
            connecting: tokio::sync::Notify::new(),
            connected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Progress-callback side: record the phase transitions the budget
    /// driver needs. `Connecting` marks the scan/connect boundary *before*
    /// waking the driver, so the recorded instant is always available.
    pub(super) fn mark(&self, progress: &Progress) {
        match progress {
            Progress::Connecting => {
                *self.connecting_at.lock().unwrap() = Some(tokio::time::Instant::now());
                self.connecting.notify_one();
            }
            Progress::Connected { .. } => {
                self.connected
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Drive one send attempt under split budgets (review P1-01: the 3-second
/// wave-25 deadline used to wrap the whole engine future, so a large or
/// slow-storage scan exhausted it before the first handshake byte):
///
/// * **before the connect-phase signal** there is no deadline at all — the
///   engine's synchronous source scan (`prepare_send`) must not burn the
///   connect budget, whatever the directory size or storage speed;
/// * **from the connect-phase signal** (bind/connect/QUIC+TLS handshake)
///   the attempt gets `budget`, starting exactly at the engine's
///   `Connecting` report;
/// * **once connected** the deadline no longer applies: the transfer is
///   governed by the engine's own idle budget and legitimately outlives
///   the connect budget.
///
/// `Err(())` means the connect phase exceeded its budget without a
/// handshake; the caller renders the "no QUIC handshake" failure. The
/// budget is a parameter (production passes [`SEND_CONNECT_TIMEOUT`]) so
/// the split itself is testable with synthetic futures and small delays
/// instead of real multi-second scans. Generic over the driven future for
/// the same reason.
pub(super) async fn drive_with_connect_budget<S>(
    marks: &ConnectPhaseMarks,
    mut send: std::pin::Pin<&mut S>,
    budget: Duration,
) -> Result<<S as Future>::Output, ()>
where
    S: Future + ?Sized,
{
    // Phase 1 — preparation: no deadline. The engine signals the boundary
    // from inside its own poll (see ConnectPhaseMarks); an attempt that
    // finishes or fails before ever connecting (invalid spec, duplicate
    // roots, bind failure) completes through the second branch untouched.
    let connect_started = tokio::select! {
        _ = marks.connecting.notified() => {
            marks
                .connecting_at
                .lock()
                .unwrap()
                .expect("Connecting instant is recorded before notify")
        }
        done = send.as_mut() => return Ok(done),
    };
    // Phase 2 — bind/connect/handshake: the budget counted from the
    // engine's own Connecting report, so scan time cannot leak in.
    match tokio::time::timeout_at(connect_started + budget, send.as_mut()).await {
        Ok(done) => Ok(done),
        Err(_deadline_elapsed) => {
            if marks.connected.load(std::sync::atomic::Ordering::Relaxed) {
                // Connected inside the deadline; from here the transfer is
                // governed by its own idle budget, not the connect deadline.
                Ok(send.await)
            } else {
                Err(())
            }
        }
    }
}

/// One full engine send against a single address, split-timed by
/// [`drive_with_connect_budget`]: the synchronous source scan is unbudgeted,
/// bind/connect/handshake gets [`SEND_CONNECT_TIMEOUT`], and a connected
/// transfer runs to completion unbounded (a real transfer legitimately
/// outlives 3 seconds). Phase marks are set from the progress callback,
/// which the engine invokes synchronously inside the polled future, so
/// there is no cross-task race.
///
/// `AttemptFailure` carries the original `EngineError` (and, since wave
/// 27, the admission BUSY hint) so the aggregate error keeps the full
/// chain; at 128 bytes it sits exactly on clippy's `result_large_err`
/// budget. Boxing the error would hide the concrete variant behind a
/// deref at every match site, so the shape is kept and the lint allowed
/// here (single site; the struct never nests).
#[allow(clippy::result_large_err)]
pub(super) async fn attempt_send_to(
    spec: SendSpec,
    printer: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, AttemptFailure> {
    let addr = spec.peer;
    let marks = Arc::new(ConnectPhaseMarks::new());
    let phase_marks = marks.clone();
    let wrapped = move |progress: Progress| {
        phase_marks.mark(&progress);
        printer(progress);
    };
    let send = ak_engine::send(spec, &wrapped);
    tokio::pin!(send);
    let result = match drive_with_connect_budget(&marks, send.as_mut(), SEND_CONNECT_TIMEOUT).await
    {
        Ok(result) => result,
        Err(()) => return Err(AttemptFailure::connect_timeout(addr)),
    };
    let after_connect = marks.connected.load(std::sync::atomic::Ordering::Relaxed);
    result.map_err(|err| AttemptFailure {
        addr,
        reason: one_line_reason(&err),
        err,
        after_connect,
        hint: None,
    })
}

/// Drive the wave-25 address fallback: try each target in order through
/// `attempt`. A connect-phase failure moves to the next address; a failure
/// after the connection was established is returned immediately (another
/// address cannot fix a protocol/transfer failure); the first success
/// returns its report. Total failure returns every (address, reason) pair
/// for the aggregate error. Progress lines print only for multi-address
/// runs, so single-address output stays identical to the classic CLI.
pub(super) async fn run_send_fallback<F, Fut>(
    targets: &[SocketAddr],
    mut attempt: F,
) -> Result<Report, Vec<AttemptFailure>>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<Report, AttemptFailure>>,
{
    let total = targets.len();
    let mut failures: Vec<AttemptFailure> = Vec::new();
    for (index, addr) in targets.iter().copied().enumerate() {
        if total > 1 && index == 0 {
            println!("[send] trying 1/{total}: {addr}...");
        }
        match attempt(addr).await {
            Ok(report) => return Ok(report),
            Err(failure) => {
                let next = index + 1;
                if total > 1 && next < total {
                    // The spec's announcement line doubles as the "trying"
                    // line for the next candidate.
                    println!(
                        "[send] failed ({}), trying {}/{total}: {}...",
                        failure.short(),
                        next + 1,
                        targets[next]
                    );
                    tokio::time::sleep(SEND_ATTEMPT_PAUSE).await;
                } else if total > 1 {
                    println!("[send] failed ({})", failure.short());
                }
                let after_connect = failure.after_connect;
                failures.push(failure);
                if after_connect {
                    // This address connected; the transfer itself failed.
                    // No other candidate can help -- surface it now.
                    return Err(failures);
                }
            }
        }
    }
    Err(failures)
}

/// Build the final error from the fallback failures. A single-address run
/// keeps the classic wording exactly (engine error, friendly hint as
/// context); a multi-address run lists every attempted address with its own
/// reason (or, for a post-connect failure, names the connected address and
/// the untried candidates).
pub(super) fn send_failures_error(
    failures: Vec<AttemptFailure>,
    targets: &[SocketAddr],
) -> anyhow::Error {
    // Address-independent input problems keep their dedicated friendly copy
    // regardless of how many addresses were tried.
    for failure in &failures {
        if let Some(io) = duplicate_roots_error(&failure.err) {
            return anyhow::Error::msg(format!(
                "two paths have the same name: please send them in separate runs ({io})"
            ));
        }
    }
    let mut failures = failures;
    let last = failures
        .pop()
        .expect("run_send_fallback returns at least one failure");
    // Wave 27: an admission-denial failure carries its own BUSY/retry copy
    // (scheduler.md §7); every other failure keeps the generic mapping.
    let hint = last
        .hint
        .as_deref()
        .map(str::to_owned)
        .or_else(|| errors::friendly_send_hint(&last.err));
    let base = anyhow::Error::from(last.err);
    if targets.len() == 1 {
        // Single address: byte-for-byte the classic error path.
        return match hint {
            Some(hint) => base.context(hint),
            None => base,
        };
    }
    let mut message = if last.after_connect {
        let untried: Vec<String> = targets
            .iter()
            .filter(|addr| !failures.iter().any(|f| f.addr == **addr) && **addr != last.addr)
            .map(|addr| addr.to_string())
            .collect();
        let untried = if untried.is_empty() {
            String::new()
        } else {
            format!(" (untried: {})", untried.join(", "))
        };
        format!(
            "connected to {} but the transfer failed: {}{untried}",
            last.addr, last.reason
        )
    } else {
        let mut message = format!("all {} peer address attempts failed:", targets.len());
        for failure in &failures {
            message.push_str(&format!("\n  {}: {}", failure.addr, failure.reason));
        }
        message.push_str(&format!("\n  {}: {}", last.addr, last.reason));
        message
    };
    if let Some(hint) = &hint {
        message.push_str(&format!("\n{hint}"));
    }
    base.context(message)
}

// ---------------------------------------------------------------------------
// send: scheduler admission over a Ready pairing relation (wave 27, P1-02)
// ---------------------------------------------------------------------------

/// One Ready pairing relation of this identity directory that matches the
/// pinned send target — the facts the scheduler admission context needs
/// (scheduler.md §6): which code group the task runs under and which
/// pairing relation selected the peer.
///
/// Source of truth (read-only, CLI side):
///
/// * the relation itself — the `ak-store` pairing journal
///   (`<id-dir>/pairing/<pairing hex>.akpair`, written by the wave-21/22
///   pairing flows): `phase applied` marks the relation whose `PairReady`
///   completed and whose entry landed in `peers.book` (the journal's
///   `applied` IS the local Ready mark — see
///   [`ak_session::pairing::PairingOrchestrator::finish_committed`]);
///   the record's `peer_public_key` is the peer device's Ed25519 key,
///   whose `Fingerprint::from_public_key` equals the TLS fingerprint the
///   send pins;
/// * the registry half — `<id-dir>/registry.pin` (`AKREG1 <addr> <64hex>`):
///   the pinned registry id is exactly `TaskOpen.group_registry` (the
///   registry certificate key IS the registry id, see
///   `ak_session::pairing::client`).
///
/// A relation without a usable registry pin cannot form a valid
/// `AdmissionCtx` (`group_registry` must name the real registry), so the
/// lookup returns `None` and the send keeps the legacy protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadyRelation {
    /// `TaskOpen.pairing_id`: the pairing transaction that produced the
    /// relation (one code may be redeemed by several devices — the task
    /// must name which relation it runs on, scheduler.md §2).
    pub(super) pairing_id: [u8; 16],
    /// The invite code identity of the relation (`code_id[16]`); travels
    /// as `TaskOpen.group` through the frozen 32-byte wire form
    /// [`ak_session::runtime::code_group_wire_form`] (BLAKE3 of the 16
    /// code bytes — the wave-27 convention shared with the receiving
    /// daemon's [`ak_session::runtime::AdmissionValidator`]).
    pub(super) code_id: [u8; 16],
    /// `TaskOpen.group_registry`: the pinned registry identity.
    pub(super) registry_id: [u8; 32],
}

/// The registry identity pinned under `id` (`registry.pin`), when one
/// exists. Absent file → `None` (this device never talked to a registry,
/// so no admission context is possible). A present-but-corrupt pin is an
/// error, never silently ignored (the fail-closed discipline of
/// [`invite::RegistrySettings::parse`]).
fn pinned_registry_id(id: &Path) -> Result<Option<[u8; 32]>> {
    let pin_path = id.join(invite::REGISTRY_PIN_FILE);
    match std::fs::read_to_string(&pin_path) {
        Ok(text) => Ok(Some(
            invite::RegistrySettings::parse(&text)
                .with_context(|| format!("corrupt {}", pin_path.display()))?
                .registry_id,
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("cannot read {}", pin_path.display())))
        }
    }
}

/// Owner-side revocation knowledge (best effort, pure local files): a code
/// this device itself created is revoked when `invite revoke` removed its
/// line from `<id-dir>/invites.store` — a store that exists but no longer
/// lists the code means the operator killed it. A redeemer has no local
/// revocation state (the store is owner-side only), so a missing store
/// never disqualifies a relation; the receiving owner's admission decider
/// remains the authority on whether the code still stands.
pub(super) fn code_revoked_locally(id: &Path, code_id: &[u8; 16]) -> bool {
    let Ok(text) = std::fs::read_to_string(id.join(invite::INVITE_STORE_FILE)) else {
        return false;
    };
    let prefix = format!("AKINV1 {}", hex_of(code_id));
    !text
        .lines()
        .any(|line| line.trim_start().starts_with(&prefix))
}

/// Find the Ready pairing relation of this identity directory that matches
/// `pinned` (wave 27, scheduler.md §6). Read-only: nothing is created,
/// nothing is written. Facts and selection rule:
///
/// * candidates are journal records with `phase applied` whose
///   `peer_public_key` derives exactly the pinned fingerprint;
/// * among several relations for the same peer (a re-pair, or several
///   codes redeemed with the same device) the most recently updated
///   applied record wins — its code group is the freshest pairing state;
///   ties break on `pairing_id` bytes so the choice is deterministic;
/// * owner-revoked codes (see [`code_revoked_locally`]) are skipped;
/// * a corrupt journal fails closed (the ak-store contract: a journal that
///   lies is worse than one that refuses) — the operator fixes the state
///   instead of the send silently dropping to the unadmitted path;
/// * no registry pin → `Ok(None)` (legacy protocol; see
///   [`ReadyRelation`]).
pub(super) fn ready_relation_for(id: &Path, pinned: &Fingerprint) -> Result<Option<ReadyRelation>> {
    let journal_dir = id.join(ak_session::pairing::JOURNAL_SUBDIR);
    if !journal_dir.exists() {
        // Fresh identity directory: no pairing has ever run here. Do not
        // even open the journal (PairingJournal::open would create it).
        return Ok(None);
    }
    let Some(registry_id) = pinned_registry_id(id)? else {
        return Ok(None);
    };
    let journal = ak_store::PairingJournal::open(&journal_dir).with_context(|| {
        format!(
            "cannot open the pairing journal at {}",
            journal_dir.display()
        )
    })?;
    let records = journal
        .load_all()
        .context("the pairing journal is corrupt; fix it before sending (fail closed)")?;
    let mut candidates: Vec<&ak_store::PairingRecord> = records
        .iter()
        .filter(|record| record.phase == ak_store::PairingPhase::Applied)
        .filter(|record| {
            record
                .peer_public_key
                .is_some_and(|key| Fingerprint::from_public_key(&key) == *pinned)
        })
        .filter(|record| !code_revoked_locally(id, &record.code_id))
        .collect();
    // Most recent Ready first; pairing-id bytes break exact ties so two
    // records updated within the same millisecond still choose stably.
    candidates.sort_by(|a, b| {
        b.updated_unix_ms
            .cmp(&a.updated_unix_ms)
            .then_with(|| a.pairing_id.cmp(&b.pairing_id))
    });
    Ok(candidates.first().map(|record| ReadyRelation {
        pairing_id: record.pairing_id,
        code_id: record.code_id,
        registry_id,
    }))
}

/// Bounded BUSY copy for an admission denial (scheduler.md §7: "远端准入
/// 队列满：返回有界 BUSY/retry hint"). The one-shot CLI keeps no local
/// queue item, so the honest advice is: nothing was sent, retry the same
/// command after the stated condition clears.
pub(super) fn admission_denied_hint(reason: ak_wire::AdmissionReason) -> String {
    use ak_wire::AdmissionReason;
    match reason {
        AdmissionReason::SerialWait => {
            "对端忙（BUSY）：同一邀请码下已有任务在传输（serial_wait）。\
            本次未发送任何数据；请等对端当前任务结束后重跑同一命令重试"
                .to_owned()
        }
        AdmissionReason::Capacity => "对端忙（BUSY）：对端的准入队列或并发上限已满（capacity）。\
            本次未发送任何数据；请稍后重跑同一命令重试"
            .to_owned(),
        AdmissionReason::StalePermit => {
            "对端忙（BUSY）：准入许可反复过期（stale_permit，已按有界预算重试）。\
            本次未发送任何数据；请稍后重跑同一命令重试"
                .to_owned()
        }
        AdmissionReason::Ok => {
            "对端忙（BUSY）：准入被拒。本次未发送任何数据；请稍后重跑同一命令重试".to_owned()
        }
    }
}

/// Outcome of one admission-capable send attempt against one address.
#[derive(Debug)]
enum AdmissionAttempt {
    /// The admission handshake ran and the whole transfer completed.
    Completed(Report),
    /// The connected peer's Hello did not carry `CAP_SCHEDULER` (a legacy
    /// receiver): the engine refused the scheduled task with
    /// `PROTOCOL_UNSUPPORTED` before any TaskOpen byte moved (the error
    /// itself is carried for the caller's diagnostics line). The caller
    /// may retry the same address on the legacy protocol — the frozen v1
    /// wire both ends still share (scheduler.md §6: old ends keep the old
    /// protocol; the refusal itself is what proves no silent downgrade of
    /// a scheduler-capable peer happened).
    LegacyPeer(EngineError),
    /// Any other failure: connect-phase errors (next address may help)
    /// and post-connect failures (another address cannot fix them).
    Failed(AttemptFailure),
}

/// One scheduler-admission send attempt against a single address (wave
/// 27): bind + connect under the same [`SEND_CONNECT_TIMEOUT`] budget the
/// legacy per-address attempt uses (the budget covers exactly the
/// bind/QUIC+TLS handshake phase — admission runs after the connection is
/// established, so the budget semantics of wave 26 are unchanged), then
/// hand the connected channel to [`ak_engine::send_on_channel`] with the
/// [`ak_engine::AdmissionCtx`] built from the Ready relation.
///
/// Error discipline (scheduler.md §6):
///
/// * an admission denial (capacity/serial_wait, or a stale-permit budget
///   exhausted) is a terminal BUSY failure — `after_connect = true`, so
///   the fallback driver stops: the same peer would deny on every
///   address, and there is no fallback to the unadmitted path;
/// * `PROTOCOL_UNSUPPORTED` from the on-channel entry means exactly "the
///   peer Hello lacked CAP_SCHEDULER" (the engine checks the capability
///   locally, before any TaskOpen) — surfaced as [`AdmissionAttempt::LegacyPeer`];
/// * everything else from `send_on_channel` happened after the connection
///   was established, so it is `after_connect = true` like the legacy
///   attempt's post-connect failures.
async fn attempt_send_admission_to(
    spec: SendSpec,
    relation: &ReadyRelation,
    printer: &(dyn Fn(Progress) + Send + Sync),
) -> AdmissionAttempt {
    let addr = spec.peer;
    // AdmissionCtx identities: task and permit ids are minted fresh per
    // invocation (scheduler.md §11: a user submission is a new task
    // instance with a random persistent id; the file-protocol wire job id
    // is content-derived inside the engine and not derivable before the
    // handshake without duplicating the engine's private source scan).
    // The all-zero sentinel cannot occur from `random_id16`; the
    // validating constructor is still the only way the ctx is built.
    let ctx = match ak_engine::AdmissionCtx::new(
        ak_identity::pairing_txn::random_id16(),
        ak_identity::pairing_txn::random_id16(),
        ak_session::runtime::code_group_wire_form(&relation.code_id),
        relation.registry_id,
        relation.pairing_id,
    ) {
        Ok(ctx) => ctx,
        Err(err) => {
            return AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: one_line_reason(&err),
                err,
                after_connect: false,
                hint: None,
            })
        }
    };
    // Bind + connect (QUIC + pinned TLS handshake) under the wave-26
    // connect budget. The source scan runs inside send_on_channel after
    // the channel exists, so unlike the legacy path it cannot eat into
    // this budget at all.
    let connect = async {
        let endpoint = ak_net::DirectEndpoint::bind(spec.bind, &spec.identity).await?;
        endpoint.connect(spec.peer, &spec.pinned).await
    };
    let channel = match tokio::time::timeout(SEND_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(channel)) => channel,
        Ok(Err(err)) => {
            let err = EngineError::Connect(err);
            return AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: one_line_reason(&err),
                err,
                after_connect: false,
                hint: None,
            });
        }
        Err(_deadline_elapsed) => {
            return AdmissionAttempt::Failed(AttemptFailure::connect_timeout(addr))
        }
    };
    match ak_engine::send_on_channel(channel, spec, ctx, printer).await {
        Ok(report) => AdmissionAttempt::Completed(report),
        Err(EngineError::AdmissionDenied { reason }) => {
            let hint = admission_denied_hint(reason);
            AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: format!("remote admission denied ({:?}); nothing was sent", reason),
                err: EngineError::AdmissionDenied { reason },
                after_connect: true,
                hint: Some(Box::from(hint)),
            })
        }
        Err(
            err @ EngineError::PeerError {
                code: ak_core::error::ErrorCode::ProtocolUnsupported,
                ..
            },
        ) => {
            // Exactly "peer does not advertise CAP_SCHEDULER": a legacy
            // receiver. No scheduler frames were sent; the frozen v1
            // protocol is the shared language for this pair.
            AdmissionAttempt::LegacyPeer(err)
        }
        Err(err) => AdmissionAttempt::Failed(AttemptFailure {
            addr,
            reason: one_line_reason(&err),
            err,
            after_connect: true,
            hint: None,
        }),
    }
}

// Options arrive flat from clap; grouping them into a struct here would only
// mirror the enum variant one-to-one (same allowance as the engine drivers).
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_command(
    id: &Path,
    paths: Vec<PathBuf>,
    to: Vec<SocketAddr>,
    fp_text: String,
    port: u16,
    conflict: String,
    multistream: bool,
    no_internet: bool,
    timeout: u64,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let pinned = resolve_fingerprint_arg(&fp_text, &peer_book_path(id))?;
    let (root, extras) = send_roots(&paths)?;
    let targets = order_send_targets(to);
    println!(
        "sending {} path(s) to peer {} (its pairing code must be {})",
        1 + extras.len(),
        pinned.short_id(),
        ak_identity::pairing_code(&pinned)
    );
    if targets.len() > 1 {
        println!(
            "peer addresses (LAN first): {}",
            targets
                .iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let conflict = conflict_policy(&conflict)?;
    let checkpoints = checkpoint_dir(id);
    let idle = idle_timeout_from_secs(timeout);
    let device_name = whoami();
    let printer = progress_printer("send");
    let _ = key;
    // Wave 27 (scheduler.md §6): when this identity directory holds a
    // Ready pairing relation matching the pinned peer, each address
    // attempt first asks the peer for scheduler admission
    // (TaskOpen → Admission → AdmissionAck) and only then transfers. The
    // lookup is read-only; without a relation (an explicit manual
    // fingerprint transfer) the send keeps the legacy protocol unchanged.
    let relation = ready_relation_for(id, &pinned)?;
    if let Some(relation) = &relation {
        println!(
            "[send] ready pairing relation {} (code {}): requesting scheduler admission",
            &hex_of(&relation.pairing_id)[..8],
            &hex_of(&relation.code_id)[..8]
        );
    }
    let admission = relation.as_ref();
    // One engine send per address; the spec is rebuilt per attempt because
    // SendSpec carries the peer address (and the engine consumes the spec).
    // The shared pieces are borrowed (not moved) so the closure stays FnMut
    // across attempts, and every attempt clones its own spec inputs — no
    // spec state survives one attempt to pollute the next. The engine's
    // prepared scan, however, is private to one `ak_engine::send` call and
    // cannot be shared across candidates from the CLI (see the P1-01 note
    // in reports/codex-review-2026-11-05-wave25.md: fixing that repetition
    // needs an ak-engine entry change, out of scope here).
    let (root, extras, bundle, checkpoints, device_name) =
        (&root, &extras, &bundle, &checkpoints, &device_name);
    let printer = printer.as_ref();
    let attempt = |addr: SocketAddr| async move {
        let mut spec = SendSpec::new(root.clone(), addr, pinned, bundle.clone());
        for extra in extras.iter() {
            spec.add_root(extra.clone());
        }
        // Family-matched wildcard per candidate (review P2-01): an IPv6
        // candidate binds [::]:port, an IPv4 candidate 0.0.0.0:port — the
        // engine's family fast-fail would otherwise kill every IPv6
        // candidate before the handshake.
        spec.bind = send_bind_for(addr, port);
        spec.conflict = conflict;
        spec.checkpoint_dir = checkpoints.clone();
        spec.device_name = device_name.clone();
        spec.idle_timeout = idle;
        spec.multistream = multistream;
        spec.policy.allow_internet = !no_internet;
        if let Some(relation) = admission {
            match attempt_send_admission_to(spec.clone(), relation, printer).await {
                AdmissionAttempt::Completed(report) => return Ok(report),
                AdmissionAttempt::LegacyPeer(err) => {
                    // Legacy receiver (no CAP_SCHEDULER in its Hello): the
                    // engine refused the scheduled task before any TaskOpen
                    // byte moved. Explicit, observable retry on the frozen
                    // v1 protocol both ends share — never a silent path
                    // choice.
                    println!("[send] {addr}: {}", one_line_reason(&err));
                    println!("[send] retrying {addr} on the legacy transfer protocol");
                    tokio::time::sleep(SEND_ATTEMPT_PAUSE).await;
                }
                // An admission denial (BUSY) or any other post-connect
                // failure: terminal for this run — the same peer would
                // answer the same way on every address, and there is no
                // fallback to the unadmitted path.
                AdmissionAttempt::Failed(failure) => return Err(failure),
            }
        }
        attempt_send_to(spec, printer).await
    };
    let report = match run_send_fallback(&targets, attempt).await {
        Ok(report) => report,
        Err(failures) => return Err(send_failures_error(failures, &targets)),
    };
    report_summary(&report);
    // TOFU: remember a successfully reached receiver.
    if record_peer_after_send(id, &pinned)? {
        println!(
            "recorded peer {} in {} (trust on first use; its pairing code is {})",
            pinned.short_id(),
            peer_book_path(id).display(),
            ak_identity::pairing_code(&pinned)
        );
    } else {
        println!("peer {} already paired", pinned.short_id());
    }
    Ok(())
}
