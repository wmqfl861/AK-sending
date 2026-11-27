//! Shared state: connections, sessions, source admission, idle tables,
//! connection slots and the delivery helpers.
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- shared state -----------------------------------------------------------

pub(super) struct ConnEntry {
    pub(super) conn_id: u64,
    pub(super) out: mpsc::Sender<CtrlFrame>,
    /// Close signal of this connection (true = torn down). Shared handle:
    /// the handler owns one, the registry keeps this one so a replacement
    /// Hello (or the server shutdown) can tear the connection down. A
    /// watch, not a Notify — reader, writer and handler all wait on it
    /// and `send(true)` wakes every one of them.
    pub(super) close: Arc<watch::Sender<bool>>,
    pub(super) candidates: Vec<SocketAddr>,
    pub(super) peer_ip: IpAddr,
    /// The pairing notices queued to this connection so far (Q-N3, review
    /// 20): their numbers are the order in which the connection's outbox
    /// took them. Restarts with a replacement connection, as the outbox does.
    pub(super) notices: u64,
}

/// Resolve once the close watch fires (true) or its sender is gone
/// (aborted handler): either way this connection is over.
pub(super) async fn closed(rx: &mut watch::Receiver<bool>) {
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
pub(super) struct SessionEntry {
    pub(super) triple: (PairingId, CodeId, [u8; 16]),
    pub(super) endpoints: [DeviceId; 2],
    pub(super) expires_at: Instant,
}

/// A redeem of an unknown/invalid code, kept only in memory: the client
/// sees the same `InviteRedeemPending` shape as for a live code and the
/// unified `PAIR_UNAVAILABLE` at the deadline (no existence oracle).
pub(super) struct Ghost {
    pub(super) requester: DeviceId,
    pub(super) request_id: RequestId,
    pub(super) deadline_ms: u64,
    pub(super) expires_at: Instant,
}

#[derive(Default)]
pub(super) struct Inner {
    pub(super) conns: HashMap<DeviceId, ConnEntry>,
    pub(super) sessions: HashMap<SessionId, SessionEntry>,
    pub(super) session_by_triple: HashMap<(PairingId, CodeId, [u8; 16]), SessionId>,
    pub(super) ghosts: HashMap<PairingId, Ghost>,
    /// Redeem idempotency records keyed by the authenticated
    /// (redeemer, request_id) — the in-memory twin of the store's
    /// `request_journal` for claim/revoke (same shape, bounded by
    /// [`JOURNAL_MAX_ROWS`], pruned past deadline + grace).
    pub(super) redeem_journal: HashMap<(DeviceId, RequestId), RedeemReplay>,
    pub(super) next_conn_id: u64,
    /// Registry of live connection tasks so shutdown can abort them all
    /// (P1-5); the strict live-connection caps are `Shared::conn_count`
    /// and `Shared::reject_count`, this map is the abort list. Each task
    /// removes its own entry via its [`ConnSlotGuard`] (R36-P1-03).
    pub(super) conn_tasks: HashMap<u64, JoinHandle<()>>,
    next_conn_task_id: u64,
    pub(super) hello_replay: HelloReplayCache,
    /// CR-P1-03: per-source connection counters AND the per-source
    /// redeem window in one entry (keyed by [`source_key`]), so a
    /// source's redeem budget survives reconnects — a flooder that
    /// cycles TCP connections keeps its history. CR-P2-10 moves the
    /// table into an [`IdleTable`] (O(log n) max-size eviction).
    /// Pruned by the sweeper only after [`SOURCE_TABLE_IDLE_TTL`] with
    /// zero counters; bounded by [`SOURCE_TABLE_MAX_ENTRIES`].
    pub(super) sources: IdleTable<IpAddr, SourceEntry>,
    /// CR-P2-10: aggregate counters of all /64 sources inside one IPv6
    /// /56 (see [`v6_parents`]); the caps are `Limits::max_conns_per_56`
    /// / `Limits::redeem_per_56_per_min` (4× the per-source budgets).
    pub(super) prefixes56: IdleTable<IpAddr, SourceEntry>,
    /// CR-P2-10: the same at /48 (`Limits::max_conns_per_48` /
    /// `Limits::redeem_per_48_per_min`, 16×).
    pub(super) prefixes48: IdleTable<IpAddr, SourceEntry>,
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
    pub(super) active_conn_sources: usize,
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
pub(super) struct SourceEntry {
    /// Connections this source currently holds against
    /// `Limits::max_conns_per_source` (admitted at accept, live until
    /// the task exits).
    pub(super) live: usize,
    /// The subset still between TCP accept and the finished TLS
    /// handshake (`Limits::max_handshaking_per_source`), released the
    /// moment the handshake resolves or fails.
    pub(super) handshaking: usize,
    pub(super) redeem: SlidingWindow,
    pub(super) last_seen: Instant,
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
pub(super) fn source_key(ip: IpAddr) -> IpAddr {
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
pub(super) fn v6_parents(source: IpAddr) -> Option<(IpAddr, IpAddr)> {
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
pub(super) fn conn_fair_share(active_conn_sources: usize) -> usize {
    let active = active_conn_sources.max(1);
    MAX_LIVE_CONNECTIONS.div_ceil(active).max(FAIR_CONN_FLOOR)
}

/// The outcome of [`Inner::source_admit`] — which accept-time rejection
/// path (if any) the caller takes.
pub(super) enum SourceAdmission {
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
pub(super) trait IdleTracked {
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
pub(super) struct IdleTable<K, V> {
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
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)
    }

    /// Mutable entry access WITHOUT re-stamping activity — for the
    /// counter mutators, which stamp explicitly via [`Self::note_activity`]
    /// afterwards (so an index update only happens once per event).
    pub(super) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries.get_mut(key)
    }

    /// The entry for `key` with its activity stamp moved to `now`
    /// (inserted with `init` when absent), eviction index in step.
    pub(super) fn touch_entry(&mut self, key: K, now: Instant, init: impl FnOnce() -> V) -> &mut V {
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
    pub(super) fn note_activity(&mut self, key: &K, now: Instant) {
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
    pub(super) fn make_room(&mut self, now: Instant) -> bool {
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
pub(super) struct RedeemReplay {
    pub(super) lookup_digest: Digest32,
    pub(super) redeemer_nonce: [u8; 32],
    pub(super) request_mac: [u8; 32],
    pub(super) deadline_ms: u64,
    pub(super) pairing_id: PairingId,
    pub(super) expires_at: Instant,
}

impl RedeemReplay {
    /// Same redeem attempt (everything the signature binds)? A different
    /// attempt reusing the request id is a `REQUEST_CONFLICT`, exactly as
    /// the claim journal treats it.
    pub(super) fn matches(&self, redeem: &RedeemRequest) -> bool {
        self.lookup_digest == redeem.lookup_digest
            && self.redeemer_nonce == redeem.redeemer_nonce
            && self.request_mac == redeem.request_mac
            && self.deadline_ms == redeem.deadline_ms
    }
}

pub(super) struct Shared {
    pub(super) registry_id: RegistryId,
    pub(super) acceptor: tokio_rustls::TlsAcceptor,
    pub(super) store: RegistryStore,
    pub(super) limits: Limits,
    /// The build revision this server answers `ServerInfoRequest` with
    /// (G6 §6a; already sanitized to the wire budget at startup).
    pub(super) server_revision: Arc<str>,
    pub(super) inner: Mutex<Inner>,
    /// Live (accepted, not yet finished) connections — the admission
    /// counter for [`MAX_LIVE_CONNECTIONS`], reserved atomically at
    /// accept time and released by the owning task's [`ConnSlotGuard`]
    /// on every exit path (R36-P1-03).
    pub(super) conn_count: AtomicUsize,
    /// Over-cap rejection tasks in flight — the admission counter for
    /// [`MAX_REJECT_TASKS`], so a flood past the serving cap can never
    /// stack unbounded handshake work (R36-P1-03).
    pub(super) reject_count: AtomicUsize,
    /// Test-only: the serve loop skips `sweep` while it is set, so a test
    /// can run the sweep itself and choose its order against a handler.
    #[cfg(test)]
    pub(super) sweep_held: std::sync::atomic::AtomicBool,
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
pub(super) struct ConnSlotGuard {
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
    pub(super) fn admitted(shared: Arc<Shared>, task_id: u64, source: IpAddr) -> Self {
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
    pub(super) fn reject(shared: Arc<Shared>, task_id: u64) -> Self {
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
    pub(super) fn handshake_done(&mut self) {
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
pub(super) fn alloc_conn_task_id(shared: &Shared) -> u64 {
    let mut inner = shared.lock();
    inner.next_conn_task_id = inner.next_conn_task_id.wrapping_add(1);
    inner.next_conn_task_id
}

/// Track a spawned connection task so shutdown can abort it. A task
/// that already exited unregisters itself via its guard; when that is
/// observable (`is_finished`) the stale handle is not registered at all.
pub(super) fn register_conn_task(shared: &Shared, id: u64, task: JoinHandle<()>) {
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
    pub(super) fn source_admit(
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
    pub(super) fn source_handshake_done(&mut self, source: IpAddr, now: Instant) {
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
    pub(super) fn source_release(&mut self, source: IpAddr, handshaking: bool, now: Instant) {
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
    pub(super) fn prune_source_tables(&mut self, now: Instant) {
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
    pub(super) fn redeem_allows(
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
    pub(super) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Bounded outbound push: the per-connection outbox is `OUTBOX_DEPTH`
    /// deep; a full queue drops the message (recoverable via
    /// `PairStatusQuery` resends) instead of blocking any handler.
    /// Accepts a full frame or a plain message (`From<CtrlMessage>`).
    pub(super) fn deliver<F: Into<CtrlFrame>>(&self, device: &DeviceId, frame: F) {
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

    /// A pairing notification to one participant's current connection, with
    /// the branch that produced it (Q-N3, review 20). The notices a
    /// connection receives are numbered in the order its outbox takes them,
    /// decided under the engine lock: whichever tasks produce them, the
    /// first error a participant can have read is the lowest number it was
    /// sent. The line also says whether the frame was queued at all.
    pub(super) fn deliver_noted(
        &self,
        device: &DeviceId,
        role: &str,
        pairing_id: PairingId,
        note: &str,
        frame: impl Into<CtrlFrame>,
    ) {
        let line = {
            let mut inner = self.lock();
            match inner.conns.get_mut(device) {
                None => format!(
                    "pairing {} notice to the {role} {}: {note}; dropped: the device is offline",
                    util::hex_prefix(&pairing_id, 4),
                    util::hex_prefix(device, 4)
                ),
                Some(entry) => {
                    entry.notices += 1;
                    let outcome = queue_notice(&entry.out, frame.into());
                    format!(
                        "pairing {} notice {} to the {role} {}: {note}; {outcome}",
                        util::hex_prefix(&pairing_id, 4),
                        entry.notices,
                        util::hex_prefix(device, 4)
                    )
                }
            }
        };
        decision!("{line}");
    }

    /// A handler's answer to the sender of a request, on the connection the
    /// request came in on (`out`) — where it went before this line existed.
    /// Numbered as above while that connection is still the device's
    /// current one; once a newer Hello has replaced it, the answer is
    /// queued on `out` all the same and the line says so.
    pub(super) fn answer_noted(
        &self,
        out: &mpsc::Sender<CtrlFrame>,
        device: &DeviceId,
        role: &str,
        pairing_id: PairingId,
        note: &str,
        frame: impl Into<CtrlFrame>,
    ) {
        let line = {
            let mut inner = self.lock();
            match inner.conns.get_mut(device) {
                Some(entry) if entry.out.same_channel(out) => {
                    entry.notices += 1;
                    let outcome = queue_notice(out, frame.into());
                    format!(
                        "pairing {} notice {} to the {role} {}: {note}; {outcome}",
                        util::hex_prefix(&pairing_id, 4),
                        entry.notices,
                        util::hex_prefix(device, 4)
                    )
                }
                _ => {
                    let outcome = queue_notice(out, frame.into());
                    format!(
                        "pairing {} notice to the {role} {}: {note}; {outcome} (on a connection that a newer one replaced)",
                        util::hex_prefix(&pairing_id, 4),
                        util::hex_prefix(device, 4)
                    )
                }
            }
        };
        decision!("{line}");
    }

    /// Whether the serve loop leaves the sweep to a test.
    pub(super) fn sweep_held(&self) -> bool {
        #[cfg(test)]
        {
            self.sweep_held.load(Ordering::Relaxed)
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    /// CR-P1-03: the shared redeem budget layers (per source, per
    /// identity, global breaker) under one lock acquisition.
    pub(super) fn redeem_allows(
        &self,
        source: IpAddr,
        identity: &DeviceId,
        now: Instant,
    ) -> (bool, u32) {
        let mut inner = self.lock();
        inner.redeem_allows(&self.limits, source, identity, now)
    }
}

pub(super) fn deliver<F: Into<CtrlFrame>>(tx: &mpsc::Sender<CtrlFrame>, frame: F) {
    if let Err(e) = tx.try_send(frame.into()) {
        log::debug!("outbound queue full or closed; message dropped: {e}");
    }
}

/// Queue one pairing notice on a connection's outbox and say what became
/// of it (Q-N3, review 20).
fn queue_notice(tx: &mpsc::Sender<CtrlFrame>, frame: CtrlFrame) -> &'static str {
    match tx.try_send(frame) {
        Ok(()) => "enqueued",
        Err(mpsc::error::TrySendError::Full(_)) => "dropped: the outbound queue is full",
        Err(mpsc::error::TrySendError::Closed(_)) => "dropped: the outbound queue is closed",
    }
}

/// Free-function convenience of [`deliver`] for plain messages (the
/// wave-28 extension stays empty).
pub(super) fn deliver_msg(tx: &mpsc::Sender<CtrlFrame>, message: CtrlMessage) {
    deliver(tx, CtrlFrame::new(message));
}

pub(super) fn pair_error(
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
pub(super) fn unavailable(request_id: RequestId, object: [u8; 16]) -> CtrlMessage {
    pair_error(request_id, ErrorScope::Pairing, object, PAIR_UNAVAILABLE, 0)
}

pub(super) fn server_error(code: u16, context: &str) -> CtrlMessage {
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
pub(super) fn sanitize_revision(revision: Option<&str>) -> Arc<str> {
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

pub(super) fn now_ms() -> u64 {
    RegistryStore::now_ms()
}
