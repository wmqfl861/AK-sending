//! Domain models for AK-sending. This crate performs no networking or I/O.
//!
//! Observations supplied by a caller are not cryptographic or routing evidence.
//! The trusted transport adapter in `ak-net` verifies identity and path before
//! any bytes are handed to the transfer engine.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod error;
pub mod handle;
pub mod job;
pub mod manifest;
pub mod policy;
