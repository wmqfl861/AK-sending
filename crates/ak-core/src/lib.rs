//! Domain scaffold for AK-sending. This crate performs no networking or I/O.
//!
//! Observations supplied by a caller are not cryptographic or routing evidence.
//! A future trusted transport adapter must verify both before sending any bytes.
#![forbid(unsafe_code)]

pub mod chunk;
pub mod job;
pub mod policy;

/// Receiver-local consent lifecycle. Not a network or cryptographic verifier.
pub mod receive;
