//! C ABI over `ak-engine` for future UI frontends (Flutter via FFI, or any
//! native embedding). Contract draft (frozen by the ak-ffi agent):
//!
//! - Handle-based, no globals: `ak_engine_new` owns an embedded tokio
//!   runtime; every other call takes the handle.
//! - All strings cross the boundary as UTF-8 `*const c_char` in, and
//!   caller-provided buffers out (with capacity); no allocation is handed
//!   to the caller that the caller must free except via explicit APIs.
//! - Progress is pulled (poll) as small JSON lines, so any language loop
//!   can render it; errors are status codes plus a message buffer.
//! - `unsafe` exists ONLY in this crate's shim, each block carrying a
//!   SAFETY comment; all logic lives in safe functions below the shim.
//!
//! NOT yet wired to any frontend: verified by Rust-side tests that drive
//! the C ABI end-to-end (real loopback transfers through the boundary).
