//! Authoritative registry server for control protocol v2: pairing invites
//! over TLS (`docs/contracts/pairing-flow.md` §3, §5–§8; plan A.5/A.6).
//!
//! This is the v2 coordination endpoint. It listens on its **own port**
//! (never the v1 plaintext port) and speaks only version-2 frames over
//! TLS 1.3 — there is no plaintext fallback and no version negotiation
//! (contract `protocol-v2.md` §1: "新邀请码功能仅通过 TLS 控制协议 v2
//! 使用，不自动降级明文"). The existing v1 [`crate::server`] stays
//! untouched and keeps serving the legacy plane.
//!
//! Module layout:
//!
//! - [`crate::server_v2::config`]: listen address, database path, every budget as a constant
//!   (redeem rate limits, invite quotas, session TTL, …).
//! - [`crate::server_v2::framing`]: v2 frame I/O over any tokio stream (rejects v1 frames
//!   before allocation).
//! - [`crate::server_v2::ratelimit`]: token bucket + sliding-window counters shared with the
//!   brute-force defenses.
//! - [`crate::server_v2::hello2`]: the frozen signature domains for Hello v2 (TLS-exporter
//!   bound), redeem requests, and Prepared/Applied confirmations. These
//!   byte layouts are the **client/server contract** — A3's dispatcher
//!   signs exactly these messages, the server verifies exactly these.
//! - [`crate::server_v2::tls`]: rustls 0.23 config pair with the ak-net discipline: a
//!   self-signed Ed25519 certificate derived from the registry key
//!   (`RegistryId` == the 32-byte public key), TLS 1.3 only, no webpki
//!   roots, no hostname check; clients pin the registry key.
//! - `store` (**`server-persistence` feature**): the authoritative SQLite
//!   registry (three tables per `docs/contracts/storage-schema.md`
//!   part 1). Linearized claim, idempotent requests, revision-checked
//!   revoke, lazy expiry, tombstones.
//! - `engine` (**`server-persistence` feature**): the pairing transaction
//!   state machine (`InviteRedeem → PairChallenge → PairProof →
//!   PairPrepared → PairCommit → PairApplied → PairReady`), the uniform
//!   `PAIR_UNAVAILABLE` error policy, the anti-brute-force budgets, and
//!   the `run_v2` entry point.
//!
//! Security boundary (unchanged from v1/ADR-001): the server forwards
//! fixed, bounded, structured control messages only. It never sees file
//! names, manifests, or bytes; it stores no plaintext invite code, no
//! `auth_key`, and no MAC material (`storage-schema.md` §2).
//!
//! What the server verifies vs. what it cannot: MACs over pairing
//! transcripts are keyed by the invite `auth_key`, which exists **only on
//! the two endpoints** — the server verifies structure, transcript
//! bindings (ids, keys, code, nonces, deadline, digest consistency) and
//! the Ed25519 signatures, but never claims to verify the MACs
//! (`pairing-flow.md` §6–§7).
#![forbid(unsafe_code)]

pub mod config;
pub mod framing;
pub mod hello2;
pub mod ratelimit;
pub mod tls;

#[cfg(feature = "server-persistence")]
pub mod engine;
#[cfg(feature = "server-persistence")]
pub mod store;

pub use crate::protocol_v2::RegistryId;
pub use config::{Config, Limits};
pub use framing::{read_frame, write_message};

/// Capability bit (Welcome.capabilities): invite claim/list/revoke management.
pub const CAP_INVITE_MANAGE: u32 = 0x0000_0001;
/// Capability bit: pairing transactions (redeem/proofs/commit/ready).
pub const CAP_PAIRING_TXN: u32 = 0x0000_0002;
/// Capability bit: v2 sessions keyed by (pairing_id, code_id, connection_id).
pub const CAP_V2_SESSIONS: u32 = 0x0000_0004;

/// Capabilities this server advertises in `Welcome`.
pub const CAPABILITIES: u32 = CAP_INVITE_MANAGE | CAP_PAIRING_TXN | CAP_V2_SESSIONS;
