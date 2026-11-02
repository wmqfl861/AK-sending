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

pub mod client;
pub mod codec;
pub mod server;
pub mod stun;

mod util;
