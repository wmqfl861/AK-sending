//! M0-02: paired mutual TLS over native UDP, not a file transfer implementation.
//!
//! Pairing inputs must originate from a trusted local confirmation flow, never
//! from untrusted discovery/signaling. Full certificate pins include the public
//! key; certificate renewal requires new pairing. No system CA or automatic TOFU.
//! Manual socket addresses are not proof of LAN topology or absence of invisible
//! network forwarding. There is no application relay, arbitrary socket injection,
//! raw QUIC handle or file-data API here.
//!
//! Safe external code cannot fabricate the authenticated boundary:
//! ```compile_fail
//! use ak_transport::AuthenticatedDirectConnection;
//! let connection = AuthenticatedDirectConnection {};
//! ```
//! ```compile_fail
//! use ak_transport::ReceiveGrant;
//! let grant = ReceiveGrant {};
//! ```
//! ```compile_fail
//! fn escape(connection: &ak_transport::AuthenticatedDirectConnection) {
//!     let raw = &connection.connection;
//! }
//! ```
#![forbid(unsafe_code)]

mod connection;
mod identity;
mod receive;

pub use connection::{AuthenticatedDirectConnection, Candidate, DirectEndpoint};
pub use identity::{LocalIdentity, PairedPeer};
pub use receive::{ReceiveGrant, ReceiveSession};

use ak_core::receive::ReceiveError;
use std::fmt;

const ALPN: &[u8] = b"ak-direct/0.1";
const SERVER_NAME: &str = "ak-peer.invalid";
const OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Deliberately excludes key material, certificates and endpoint addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Configuration,
    InvalidAddress,
    UntrustedPath,
    UnexpectedAddress,
    Handshake,
    PeerMismatch,
    Protocol,
    Timeout,
    Closed,
    PathChanged,
    InvalidGrant,
    Consent(ReceiveError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "direct adapter error: {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<ReceiveError> for Error {
    fn from(value: ReceiveError) -> Self {
        Self::Consent(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
