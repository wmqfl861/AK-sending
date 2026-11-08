//! v2 server configuration and every coordination-plane budget as a named
//! constant (`docs/contracts/pairing-flow.md` §8, §11 — numbers frozen
//! there; the redeem budgets are the A2 task freeze).
//!
//! v2 listens on its own port; the v1 plaintext server keeps its own.
//! There is no shared socket and no downgrade path between them.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Redeem attempts allowed per control connection per minute (task A2 /
/// contract §11 device-level budget; the per-IP budget is not observable
/// on loopback and is covered by the global budget below).
pub const REDEEM_PER_CONN_PER_MIN: u32 = 10;
/// Redeem attempts allowed server-wide per minute (task A2 freeze).
pub const REDEEM_GLOBAL_PER_MIN: u32 = 60;
/// One redeem attempt may span at most this long (contract §8).
pub const REDEEM_MAX_WINDOW: Duration = Duration::from_secs(120);
/// Minimum non-zero invite TTL (contract §8: 60 s .. 30 days).
pub const INVITE_TTL_MIN_SECS: u32 = 60;
/// Maximum invite TTL (30 days, contract §8). `0` means long-lived.
pub const INVITE_TTL_MAX_SECS: u32 = 30 * 24 * 3600;
/// Active invites one owner may hold (contract §11).
pub const MAX_ACTIVE_INVITES_PER_OWNER: usize = 32;
/// Invite claims one owner may make per hour (contract §11).
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
    pub redeem_global_per_min: u32,
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
            redeem_global_per_min: REDEEM_GLOBAL_PER_MIN,
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
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_V2_PORT)),
            db_path: PathBuf::from(":memory:"),
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
        assert_eq!(limits.redeem_global_per_min, REDEEM_GLOBAL_PER_MIN);
        assert_eq!(
            limits.max_active_invites_per_owner,
            MAX_ACTIVE_INVITES_PER_OWNER
        );
        assert_eq!(limits.invite_claims_per_hour, INVITE_CLAIMS_PER_HOUR);
        assert_eq!(limits.max_relations_per_owner, MAX_RELATIONS_PER_OWNER);
        assert_eq!(limits.session_ttl, SESSION_TTL);
        // The frozen numbers themselves (task A2 / contract §11).
        assert_eq!(REDEEM_PER_CONN_PER_MIN, 10);
        assert_eq!(REDEEM_GLOBAL_PER_MIN, 60);
        assert_eq!(INVITE_CLAIMS_PER_HOUR, 6);
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
