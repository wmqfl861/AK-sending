//! Shared session application layer for AK-sending.
//!
//! `ak-session` hosts the product-level scheduling and (in later waves)
//! pairing/runtime orchestration shared by the CLI daemon and the FFI
//! surface. The frozen contract lives in `docs/contracts/scheduler.md`
//! (wave 18); this crate performs no networking and no I/O.
//!
//! Wave 19 (B0) delivers the pure scheduler domain model in
//! [`scheduler`]: two-dimensional concurrency policy, bounded fair queue,
//! admission decisions and the task/permit state machine that the B6
//! runtime drives from a single actor.
//!
//! Wave 20 (B6) adds [`runtime`]: the resident session runtime in
//! Fake-DI form — a scheduler actor owning the core, task-lifecycle
//! orchestration (backoff, park, cancel), a pairing listener, bounded
//! event broadcast and drain/cancel shutdown. Real networking is injected
//! through the `PairingTransport`/`TransferExecutor` traits (wave 21
//! binds the real adapters), so this crate still performs no networking
//! and no direct I/O.
#![forbid(unsafe_code)]

/// Pure scheduler domain model (policy, queue, admission, state machine).
pub mod scheduler;

/// Resident session runtime (scheduler actor, task lifecycle, listener,
/// shutdown) over injected Fake/real dependencies.
pub mod runtime;

/// Automatic pairing orchestration (wave 21, A6): a real v2 control client
/// over the frozen `ak-signal` registry surface, the
/// [`runtime::transport::PairingTransport`] adapter over it, and the
/// owner/redeemer orchestration of the frozen pairing transcript.
pub mod pairing;
