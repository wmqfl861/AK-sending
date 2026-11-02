//! Signaling client: one TCP connection, Hello-authenticated, then
//! register/connect/signal exchanges.
#![forbid(unsafe_code)]

use std::net::SocketAddr;

use ak_identity::DeviceKeyPair;

use crate::codec::{CtrlFrame, CtrlMessage, DeviceId, PeerSignal, SessionId};

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected message: {0:?}")]
    Unexpected(CtrlMessage),
    #[error("server error {code}: {message}")]
    Server { code: u16, message: String },
    #[error("connection closed")]
    Closed,
    #[error("codec error: {0}")]
    Codec(#[from] crate::codec::DecodeError),
}

/// Connected, authenticated signaling client.
pub struct SignalClient {
    pub(super) device_id: DeviceId,
}

impl SignalClient {
    /// Connect, send a signed Hello, and await Welcome.
    pub async fn connect(
        server: SocketAddr,
        key: &DeviceKeyPair,
        name: &str,
    ) -> Result<Self, SignalError> {
        todo!("owned by ak-signal agent")
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Publish our candidates (LAN and public, max 16).
    pub async fn register(&self, candidates: &[SocketAddr]) -> Result<(), SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Ask to reach `target`; session arrives via [`next`](Self::next).
    pub async fn connect_peer(&self, target: DeviceId) -> Result<(), SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Send one peer signal inside a session.
    pub async fn send_signal(&self, session: SessionId, signal: PeerSignal) -> Result<(), SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Cancel a session.
    pub async fn cancel(&self, session: SessionId) -> Result<(), SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Receive the next server message (SessionOpened, Signal, errors).
    pub async fn next(&self) -> Result<CtrlMessage, SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Close politely.
    pub async fn close(&self) -> Result<(), SignalError> {
        todo!("owned by ak-signal agent")
    }

    /// Encode helper for tests.
    pub fn frame_of(message: CtrlMessage) -> CtrlFrame {
        CtrlFrame { version: crate::codec::VERSION, message }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-signal agent"]
    fn placeholder() {}
}
