//! Domain models for AK-sending. This crate performs no networking or I/O.
//!
//! Observations supplied by a caller are not cryptographic or routing evidence.
//! The trusted transport adapter in `ak-net` verifies identity and path before
//! any bytes are handed to the transfer engine.
#![forbid(unsafe_code)]

/// Constant-memory chunk arithmetic.
pub mod chunk;
/// Stable wire error codes shared by every crate.
pub mod error;
/// Save-target and source-handle abstractions for the storage layer.
pub mod handle;
/// Receiver-side task lifecycle state machine.
pub mod job;
/// Manifest entries and relative-path validation.
pub mod manifest;
/// Fail-closed route selection model.
pub mod policy;
