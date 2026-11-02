//! Cross-module integration tests for AK-sending.
//!
//! Each test file maps to TEST_PLAN scenarios; the crate exists so scenario
//! tests never conflict with in-crate unit tests during parallel development.
//! No production code lives here.
#![forbid(unsafe_code)]
