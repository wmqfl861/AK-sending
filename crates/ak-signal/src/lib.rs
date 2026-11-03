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
pub mod client;
/// Signaling wire codec: fixed message set, every field bounded.
pub mod codec;
/// Signaling server (TCP signaling + UDP STUN responder).
pub mod server;
/// Minimal STUN binding (RFC 5389 subset).
pub mod stun;

mod util;
