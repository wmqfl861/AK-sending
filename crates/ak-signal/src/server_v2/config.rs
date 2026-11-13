//! v2 server configuration and every coordination-plane budget as a named
//! constant (`docs/contracts/pairing-flow.md` §8, §11 — numbers frozen
//! there; the redeem budgets are the A2 task freeze, re-frozen 2026-11-11
//! by CR-P1-03 into the four redeem layers plus the per-source connection
//! caps, and extended 2026-11-12 by CR-P2-10 with the hierarchical IPv6
//! prefix aggregates and the fair-share semantics of the global layers).
//!
//! v2 listens on its own port; the v1 plaintext server keeps its own.
//! There is no shared socket and no downgrade path between them.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Redeem attempts allowed per control connection per minute (contract
/// §11). The tightest of the four redeem layers: one connection alone
/// can never spend another layer's budget. A reconnect gets a fresh
/// bucket — the cross-reconnect layers are the source and identity
/// windows below.
pub const REDEEM_PER_CONN_PER_MIN: u32 = 10;
/// Redeem attempts allowed per source per minute (contract §11,
/// CR-P1-03 2026-11-11). A "source" is the peer address normalized to
/// IPv4 /32 or IPv6 /64. 30/min comfortably covers a human redeeming a
/// handful of codes (wrong paste, retry, second device) while capping
/// what one machine — even one spinning up unlimited self-generated
/// identities — can spend: identities multiply attempts, sources do
/// not multiply identities' reach beyond their own window. This window
/// lives in the shared per-source table and survives reconnects.
pub const REDEEM_PER_SOURCE_PER_MIN: u32 = 30;
/// Redeem attempts allowed per device identity per minute (contract
/// §11, CR-P1-03 2026-11-11). The identity is the Hello-authenticated
/// Ed25519 public key. Bounded by, and stacked under, the per-source
/// window: mass-generating identities cannot lift a single source past
/// [`REDEEM_PER_SOURCE_PER_MIN`]. Also shared-table state, so
/// reconnecting with the same identity does not reset it.
pub const REDEEM_PER_IDENTITY_PER_MIN: u32 = 30;
/// Aggregate redeem attempts per minute over ALL /64 sources inside one
/// IPv6 /56 prefix (contract §11, CR-P2-10 2026-11-12). A /56 is the
/// common home-broadband delegation and holds 256 /64s; the aggregate is
/// [`PREFIX_56_SHARE_MULTIPLIER`] × the per-/64 limit, so rotating
/// subnets inside one household amplifies its redeem budget by at most
/// 4× instead of 256× — a legitimate multi-subnet home never approaches
/// it. Checked between the per-/64 and per-identity layers; stacks under
/// the /48 aggregate and the global breaker.
pub const REDEEM_PER_56_PER_MIN: u32 = PREFIX_56_SHARE_MULTIPLIER * REDEEM_PER_SOURCE_PER_MIN;
/// Aggregate redeem attempts per minute over ALL /64 sources inside one
/// IPv6 /48 prefix (contract §11, CR-P2-10 2026-11-12). A /48 is a
/// site/LIR-scale allocation holding 65,536 /64s; the aggregate is
/// [`PREFIX_48_SHARE_MULTIPLIER`] × the per-/64 limit, so site-scale
/// prefix rotation is capped at 16× (three-plus orders of magnitude
/// below full enumeration) while a campus/enterprise running a handful
/// of busy subnets stays far under it. 480 < 600 deliberately: a single
/// /48 cannot alone saturate the global fair-share breaker.
pub const REDEEM_PER_48_PER_MIN: u32 = PREFIX_48_SHARE_MULTIPLIER * REDEEM_PER_SOURCE_PER_MIN;
/// Aggregate multiplier of the per-source budgets granted to one IPv6
/// /56 (contract §11, CR-P2-10): redeem AND concurrent-connection
/// aggregates inside a /56 are capped at 4× one /64's budget.
pub const PREFIX_56_SHARE_MULTIPLIER: u32 = 4;
/// Aggregate multiplier of the per-source budgets granted to one IPv6
/// /48 (contract §11, CR-P2-10): redeem AND concurrent-connection
/// aggregates inside a /48 are capped at 16× one /64's budget.
pub const PREFIX_48_SHARE_MULTIPLIER: u32 = 16;
/// Server-wide redeem circuit breaker per minute (contract §11,
/// CR-P1-03 2026-11-11; fair-share semantics CR-P2-10 2026-11-12). NOT
/// an anti-enumeration bound — invite codes carry 160 bits of entropy,
/// online guessing is infeasible — purely a last-resort bound on
/// per-minute redeem work (signature checks + store lookups) if many
/// distinct sources misbehave at once, which no per-source budget can
/// express. FAIR SHARE: when the 600/min window is full the breaker no
/// longer rejects everyone (the CR-P2-10 finding: ~20 IPv4 sources ×
/// 30/min exhausted it and EVERY later source was refused) — it rejects
/// only sources whose own in-window count has reached
/// `REDEEM_GLOBAL_BREAKER_PER_MIN / active_sources` (floor:
/// [`REDEEM_PER_SOURCE_PER_MIN`], so a normal source is never squeezed
/// below its own budget). Below-share attempts are admitted and are NOT
/// recorded in the (already full) global window, keeping its memory at
/// 600 entries; the total-work bound becomes 30/min × active sources,
/// with the per-/64 layer still tightening first.
pub const REDEEM_GLOBAL_BREAKER_PER_MIN: u32 = 600;
/// Concurrent v2 connections one source (IPv4 /32, IPv6 /64) may hold
/// (contract §11, CR-P1-03 2026-11-11), checked at TCP accept. A real
/// device holds one control connection (a second briefly during
/// reconnect overlap); 8 leaves headroom for a small LAN behind one
/// IPv4 address while capping how many of the server-wide serving-cap
/// slots (the engine's `MAX_LIVE_CONNECTIONS`) one source can pin.
pub const MAX_CONNS_PER_SOURCE: usize = 8;
/// Aggregate concurrent v2 connections over all /64 sources inside one
/// IPv6 /56 (contract §11, CR-P2-10 2026-11-12) = 4 ×
/// [`MAX_CONNS_PER_SOURCE`]; the handshaking subset is capped at 4 ×
/// [`MAX_HANDSHAKING_PER_SOURCE`]. Same justification as
/// [`REDEEM_PER_56_PER_MIN`]: prefix rotation inside one home
/// delegation pins at most 4× one /64's serving footprint.
pub const MAX_CONNS_PER_56: usize = PREFIX_56_SHARE_MULTIPLIER as usize * MAX_CONNS_PER_SOURCE;
/// Aggregate concurrent v2 connections over all /64 sources inside one
/// IPv6 /48 (contract §11, CR-P2-10 2026-11-12) = 16 ×
/// [`MAX_CONNS_PER_SOURCE`]; the handshaking subset at 16 ×
/// [`MAX_HANDSHAKING_PER_SOURCE`]. See [`REDEEM_PER_48_PER_MIN`].
pub const MAX_CONNS_PER_48: usize = PREFIX_48_SHARE_MULTIPLIER as usize * MAX_CONNS_PER_SOURCE;
/// Of a source's concurrent connections, at most this many may sit
/// between the TCP accept and the finished TLS handshake (contract
/// §11, CR-P1-03 2026-11-11), counted from accept — BEFORE any task is
/// spawned. A socket beyond either per-source cap is dropped at accept
/// time with no task at all, so connect-only-never-handshake floods
/// park at most this many handshakes and can never occupy the global
/// serving cap (the CR-P1-03 vector: 256 dead sockets for 30 s used to
/// lock every legal client out).
pub const MAX_HANDSHAKING_PER_SOURCE: usize = 4;
/// Idle lifetime of per-source / per-identity accounting entries
/// (contract §11, CR-P1-03 2026-11-11). Entries whose connection
/// counters are zero and whose last activity is older than this TTL
/// are pruned by the sweeper, so the tables cannot grow unboundedly.
/// The TTL deliberately exceeds the 60 s rate windows (an entry that
/// old can only hold an expired window), and entries with live
/// counters are never pruned.
pub const SOURCE_TABLE_IDLE_TTL: Duration = Duration::from_secs(5 * 60);
/// Hard cap on per-source / per-identity table entries (contract §11,
/// CR-P1-03 2026-11-11). Normal operation never approaches it (a live
/// entry implies a live connection, and the serving cap is far below
/// this); it is the memory backstop against a botnet spraying distinct
/// sources/identities faster than the TTL prunes them. When full, the
/// stalest idle entry is evicted; if every entry holds live counters,
/// the new arrival is treated as over budget instead of overrunning
/// the cap.
pub const SOURCE_TABLE_MAX_ENTRIES: usize = 65_536;
/// One redeem attempt may span at most this long (contract §8).
pub const REDEEM_MAX_WINDOW: Duration = Duration::from_secs(120);
/// Minimum non-zero invite TTL (contract §8: 60 s .. 30 days).
pub const INVITE_TTL_MIN_SECS: u32 = 60;
/// Maximum invite TTL (30 days, contract §8). `0` means long-lived.
pub const INVITE_TTL_MAX_SECS: u32 = 30 * 24 * 3600;
/// Active invites one owner may hold (contract §11).
pub const MAX_ACTIVE_INVITES_PER_OWNER: usize = 32;
/// Invite claims one owner may make per hour (contract §11; raised from 6
/// to 30 on 2026-11-09 — 6/hour proved too tight for normal use).
pub const INVITE_CLAIMS_PER_HOUR: u32 = 30;
/// Simultaneously pending redeems per issuing owner (contract §11).
pub const MAX_PENDING_REDEEM_PER_OWNER: usize = 32;
/// Server-wide simultaneously pending redeems (contract §11).
pub const MAX_PENDING_REDEEM_GLOBAL: usize = 4096;
/// Pairing relations one owner may accumulate (contract §11 quota).
pub const MAX_RELATIONS_PER_OWNER: usize = 1024;
/// Steady-state frames per second per connection (contract §11 keeps the
/// v1 frame budget).
pub const FRAME_RATE_PER_SEC: u32 = 32;
/// Burst allowance for the frame token bucket.
pub const FRAME_BURST: u32 = 64;
/// Hello freshness window; also the AuthChallenge validity and the Hello
/// replay window (contract `protocol-v2.md` §9, `pairing-flow.md` §3).
pub const HELLO_MAX_AGE: Duration = Duration::from_secs(10);
/// Coordination session TTL (contract §8: current default 120 s).
pub const SESSION_TTL: Duration = Duration::from_secs(120);
/// Max simultaneous coordination sessions per device (contract §10).
pub const MAX_SESSIONS_PER_DEVICE: usize = 4;
/// Period of the lazy sweeper: deadline ghosts, expired transactions,
/// expired invite codes, session TTLs. Contract §6 requires in-transaction
/// expiry checks as well; the sweeper only accelerates cleanup.
pub const SWEEP_INTERVAL: Duration = Duration::from_millis(200);
/// Idempotency journal retention (bounded, lazily pruned).
pub const JOURNAL_RETENTION: Duration = Duration::from_secs(24 * 3600);
/// Hard cap on idempotency journal rows (bounded memory / storage).
pub const JOURNAL_MAX_ROWS: usize = 4096;
/// Depth of one connection's outbound queue; a slow consumer that lets it
/// fill is disconnected (contract §11: bounded server outbox).
pub const OUTBOX_DEPTH: usize = 64;
/// Default v2 TLS listen port (v1 plaintext signaling uses 7100/7101).
pub const DEFAULT_V2_PORT: u16 = 7200;

/// v2 server budgets. Every field defaults to one of the constants above;
/// embedders may override, tests use defaults unless a scenario needs a
/// tighter value.
#[derive(Debug, Clone)]
pub struct Limits {
    pub session_ttl: Duration,
    pub max_sessions_per_device: usize,
    pub hello_max_age: Duration,
    pub frame_rate_per_sec: u32,
    pub frame_burst: u32,
    pub redeem_per_conn_per_min: u32,
    pub redeem_per_source_per_min: u32,
    /// IPv6 /56 aggregate redeem window (see [`REDEEM_PER_56_PER_MIN`]).
    pub redeem_per_56_per_min: u32,
    /// IPv6 /48 aggregate redeem window (see [`REDEEM_PER_48_PER_MIN`]).
    pub redeem_per_48_per_min: u32,
    pub redeem_per_identity_per_min: u32,
    /// Server-wide redeem breaker (see [`REDEEM_GLOBAL_BREAKER_PER_MIN`]).
    pub redeem_global_breaker_per_min: u32,
    pub max_conns_per_source: usize,
    /// IPv6 /56 aggregate concurrent connections (see [`MAX_CONNS_PER_56`]).
    pub max_conns_per_56: usize,
    /// IPv6 /48 aggregate concurrent connections (see [`MAX_CONNS_PER_48`]).
    pub max_conns_per_48: usize,
    pub max_handshaking_per_source: usize,
    pub max_active_invites_per_owner: usize,
    pub invite_claims_per_hour: u32,
    pub max_pending_redeem_per_owner: usize,
    pub max_pending_redeem_global: usize,
    pub max_relations_per_owner: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            session_ttl: SESSION_TTL,
            max_sessions_per_device: MAX_SESSIONS_PER_DEVICE,
            hello_max_age: HELLO_MAX_AGE,
            frame_rate_per_sec: FRAME_RATE_PER_SEC,
            frame_burst: FRAME_BURST,
            redeem_per_conn_per_min: REDEEM_PER_CONN_PER_MIN,
            redeem_per_source_per_min: REDEEM_PER_SOURCE_PER_MIN,
            redeem_per_56_per_min: REDEEM_PER_56_PER_MIN,
            redeem_per_48_per_min: REDEEM_PER_48_PER_MIN,
            redeem_per_identity_per_min: REDEEM_PER_IDENTITY_PER_MIN,
            redeem_global_breaker_per_min: REDEEM_GLOBAL_BREAKER_PER_MIN,
            max_conns_per_source: MAX_CONNS_PER_SOURCE,
            max_conns_per_56: MAX_CONNS_PER_56,
            max_conns_per_48: MAX_CONNS_PER_48,
            max_handshaking_per_source: MAX_HANDSHAKING_PER_SOURCE,
            max_active_invites_per_owner: MAX_ACTIVE_INVITES_PER_OWNER,
            invite_claims_per_hour: INVITE_CLAIMS_PER_HOUR,
            max_pending_redeem_per_owner: MAX_PENDING_REDEEM_PER_OWNER,
            max_pending_redeem_global: MAX_PENDING_REDEEM_GLOBAL,
            max_relations_per_owner: MAX_RELATIONS_PER_OWNER,
        }
    }
}

/// Where the v2 authoritative registry server listens and persists.
///
/// `db_path` is the authoritative SQLite registry (storage-schema.md
/// part 1). Use `:memory:` for a throwable in-process registry (tests
/// that do not exercise restart recovery).
#[derive(Debug, Clone)]
pub struct Config {
    /// v2 TLS listen address — its own port, never the v1 plaintext port.
    pub listen: SocketAddr,
    /// Authoritative registry database file (`:memory:` for ephemeral).
    pub db_path: PathBuf,
    /// Build revision this server answers `ServerInfoRequest` with
    /// (G6 §6a): a ≤32-byte UTF-8 build fingerprint such as
    /// `0.1.0+4739f4d`. `None` (the default) answers `unknown`. The
    /// value is sanitized at startup — an over-long or dirty string
    /// degrades to `unknown` instead of failing the server.
    pub revision: Option<String>,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_V2_PORT)),
            db_path: PathBuf::from(":memory:"),
            revision: None,
            limits: Limits::default(),
        }
    }
}

impl Config {
    /// Config for an ephemeral-port server over an in-memory registry
    /// (integration-test default).
    pub fn ephemeral() -> Self {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            ..Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_match_the_frozen_constants() {
        let limits = Limits::default();
        assert_eq!(limits.redeem_per_conn_per_min, REDEEM_PER_CONN_PER_MIN);
        assert_eq!(limits.redeem_per_source_per_min, REDEEM_PER_SOURCE_PER_MIN);
        assert_eq!(
            limits.redeem_per_identity_per_min,
            REDEEM_PER_IDENTITY_PER_MIN
        );
        assert_eq!(
            limits.redeem_global_breaker_per_min,
            REDEEM_GLOBAL_BREAKER_PER_MIN
        );
        assert_eq!(limits.redeem_per_56_per_min, REDEEM_PER_56_PER_MIN);
        assert_eq!(limits.redeem_per_48_per_min, REDEEM_PER_48_PER_MIN);
        assert_eq!(limits.max_conns_per_source, MAX_CONNS_PER_SOURCE);
        assert_eq!(limits.max_conns_per_56, MAX_CONNS_PER_56);
        assert_eq!(limits.max_conns_per_48, MAX_CONNS_PER_48);
        assert_eq!(
            limits.max_handshaking_per_source,
            MAX_HANDSHAKING_PER_SOURCE
        );
        assert_eq!(
            limits.max_active_invites_per_owner,
            MAX_ACTIVE_INVITES_PER_OWNER
        );
        assert_eq!(limits.invite_claims_per_hour, INVITE_CLAIMS_PER_HOUR);
        assert_eq!(limits.max_relations_per_owner, MAX_RELATIONS_PER_OWNER);
        assert_eq!(limits.session_ttl, SESSION_TTL);
        // The frozen numbers themselves (contract §11; the redeem layers
        // and per-source connection caps were re-frozen 2026-11-11 by
        // CR-P1-03 — per-source/per-identity windows replace the flat
        // 60/min global limiter, which one source could exhaust — and
        // extended 2026-11-12 by CR-P2-10 with the hierarchical IPv6
        // prefix aggregates: 4× per-source at /56, 16× at /48, both
        // below the global breaker so one prefix cannot alone trip it).
        assert_eq!(REDEEM_PER_CONN_PER_MIN, 10);
        assert_eq!(REDEEM_PER_SOURCE_PER_MIN, 30);
        assert_eq!(REDEEM_PER_IDENTITY_PER_MIN, 30);
        assert_eq!(REDEEM_PER_56_PER_MIN, 120);
        assert_eq!(REDEEM_PER_48_PER_MIN, 480);
        assert_eq!(REDEEM_GLOBAL_BREAKER_PER_MIN, 600);
        assert_eq!(MAX_CONNS_PER_SOURCE, 8);
        assert_eq!(MAX_CONNS_PER_56, 32);
        assert_eq!(MAX_CONNS_PER_48, 128);
        assert_eq!(MAX_HANDSHAKING_PER_SOURCE, 4);
        assert_eq!(PREFIX_56_SHARE_MULTIPLIER, 4);
        assert_eq!(PREFIX_48_SHARE_MULTIPLIER, 16);
        // 2026-11-09: the contract raised the per-owner claim budget from
        // 6 to 30 per hour (pairing-flow.md §11/§12 note); the frozen
        // literal follows the contract (R36-P2-06).
        assert_eq!(INVITE_CLAIMS_PER_HOUR, 30);
        assert_eq!(MAX_ACTIVE_INVITES_PER_OWNER, 32);
        assert_eq!(MAX_RELATIONS_PER_OWNER, 1024);
        assert_eq!(INVITE_TTL_MIN_SECS, 60);
        assert_eq!(INVITE_TTL_MAX_SECS, 2_592_000);
    }

    #[test]
    fn default_config_listens_on_the_v2_port_not_v1() {
        let config = Config::default();
        assert_eq!(config.listen.port(), DEFAULT_V2_PORT);
        assert_eq!(config.listen.port(), 7200);
        assert_ne!(config.listen.port(), 7100);
    }

    #[test]
    fn ttl_bounds_bracket_the_contract_range() {
        assert_eq!(INVITE_TTL_MIN_SECS * 1000, 60_000);
        assert_eq!(INVITE_TTL_MAX_SECS, 30 * 24 * 3600);
        // 0 (long-lived) is legal and distinct from the minimum bound.
        assert_ne!(INVITE_TTL_MIN_SECS, 0);
    }
}
