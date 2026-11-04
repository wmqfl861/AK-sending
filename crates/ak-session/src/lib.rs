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
//! runtime will drive from a single actor.
#![forbid(unsafe_code)]

/// Pure scheduler domain model (policy, queue, admission, state machine).
pub mod scheduler;
