//! Coordination plane: signaling server/client and a minimal STUN binding
//! implementation.
//!
//! Hard boundary (ADR-001): this plane only forwards fixed, bounded,
//! structured control messages — device presence, connection requests,
//! address candidates, punch timing. There is no payload/blob/file field, no
//! upload route, no CONNECT-style generic forwarding, and no TURN. The
//! message set is closed: unknown frame types are dropped, and this is locked
//! by tests.
#![forbid(unsafe_code)]

/// Hello-authenticated signaling client over one TCP connection.
///
/// G5 / E1 (frozen legacy model): the plaintext v1 plane is opt-in via the
/// `legacy-plaintext-signal` feature; default builds do not compile it.
#[cfg(feature = "legacy-plaintext-signal")]
pub mod client;
/// Control-protocol v2 client: pinned TLS, single-reader dispatch,
/// idempotent requests (A3).
pub mod client_v2;
/// Signaling wire codec: fixed message set, every field bounded.
///
/// G5 / E1: v1-only wire format. The v2 plane has its own independent
/// codec (`protocol_v2`) and framing (`server_v2::framing`); nothing
/// outside the gated v1 client/server consumes this module, so it is
/// gated with them behind `legacy-plaintext-signal`.
#[cfg(feature = "legacy-plaintext-signal")]
pub mod codec;
pub mod protocol_v2;
/// Signaling server (TCP signaling + UDP STUN responder).
///
/// G5 / E1 (frozen legacy model): opt-in via `legacy-plaintext-signal`.
/// The standalone STUN responder (`stun::serve_stun`, used by
/// `ak-cli stun-server` since CR-P2-13) is NOT gated with it.
#[cfg(feature = "legacy-plaintext-signal")]
pub mod server;
/// Control protocol v2 authoritative registry server: TLS transport,
/// SQLite invite registry (`server-persistence`), pairing transactions.
/// Own listen port; no plaintext downgrade.
pub mod server_v2;
/// Minimal STUN binding (RFC 5389 subset).
pub mod stun;

mod util;
