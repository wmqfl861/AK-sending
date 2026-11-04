//! Signaling client: one TCP connection, Hello-authenticated, then
//! register/connect/signal exchanges.
#![forbid(unsafe_code)]
// The public SignalError type (frozen contract) carries a whole CtrlMessage
// in its Unexpected variant, which pushes it over clippy's large-err
// threshold. Boxing it would change the frozen API, so allow it here.
#![allow(clippy::result_large_err)]

use std::net::SocketAddr;

use ak_identity::DeviceKeyPair;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

use crate::codec::{self, CtrlFrame, CtrlMessage, DeviceId, PeerSignal, SessionId};
use crate::util;

/// Signaling client failure: io, protocol surprise, server refusal,
/// closed connection, or codec rejection.
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
    writer: Mutex<Option<OwnedWriteHalf>>,
    inbox: Mutex<mpsc::UnboundedReceiver<CtrlMessage>>,
}

impl SignalClient {
    /// Connect, send a signed Hello, and await Welcome.
    ///
    /// The Hello timestamp comes from [`crate::util::fresh_now_unix_ms`], so
    /// rapid retries never sign identical Hello bytes (which the server's
    /// replay window would reject).
    pub async fn connect(
        server: SocketAddr,
        key: &DeviceKeyPair,
        name: &str,
    ) -> Result<Self, SignalError> {
        let hello = codec::signed_hello(key, name, util::fresh_now_unix_ms());
        Self::connect_hello(server, hello).await
    }

    /// `connect` with a caller-built Hello. Internal seam so tests can sign
    /// with ed25519-dalek directly while the ak-identity constructors may
    /// still be parallel-agent stubs.
    pub(crate) async fn connect_hello(
        server: SocketAddr,
        hello: CtrlMessage,
    ) -> Result<Self, SignalError> {
        let device_id = match hello {
            CtrlMessage::Hello { device_id, .. } => device_id,
            other => return Err(SignalError::Unexpected(other)),
        };
        let stream = TcpStream::connect(server).await?;
        let _ = stream.set_nodelay(true);
        let (read_half, write_half) = stream.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(read_half, tx));
        let client = SignalClient {
            device_id,
            writer: Mutex::new(Some(write_half)),
            inbox: Mutex::new(rx),
        };
        client.send(hello).await?;
        match client.next().await? {
            CtrlMessage::Welcome { .. } => Ok(client),
            CtrlMessage::ServerError { code, message } => {
                Err(SignalError::Server { code, message })
            }
            other => Err(SignalError::Unexpected(other)),
        }
    }

    /// This client's device id (the key's public bytes from the Hello).
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Publish our candidates (LAN and public, max 16).
    pub async fn register(&self, candidates: &[SocketAddr]) -> Result<(), SignalError> {
        self.send(CtrlMessage::Register {
            candidates: candidates.to_vec(),
        })
        .await
    }

    /// Ask to reach `target`; session arrives via [`next`](Self::next).
    pub async fn connect_peer(&self, target: DeviceId) -> Result<(), SignalError> {
        self.send(CtrlMessage::ConnectRequest { target }).await
    }

    /// Send one peer signal inside a session.
    pub async fn send_signal(
        &self,
        session: SessionId,
        signal: PeerSignal,
    ) -> Result<(), SignalError> {
        self.send(CtrlMessage::Signal { session, signal }).await
    }

    /// Cancel a session.
    pub async fn cancel(&self, session: SessionId) -> Result<(), SignalError> {
        self.send(CtrlMessage::Cancel { session }).await
    }

    /// Receive the next server message (SessionOpened, Signal, errors).
    /// After `SessionOpened`, the server may deliver
    /// `Signal { signal: SameEgress, .. }`: both endpoints were observed
    /// behind the same network egress, so callers should rank LAN
    /// candidates ahead of public ones (`ak_discovery::rank` already
    /// imposes that order; the hint is for UI and as a fallback).
    pub async fn next(&self) -> Result<CtrlMessage, SignalError> {
        let mut inbox = self.inbox.lock().await;
        inbox.recv().await.ok_or(SignalError::Closed)
    }

    /// Close politely.
    pub async fn close(&self) -> Result<(), SignalError> {
        let writer = self.writer.lock().await.take();
        drop(writer); // dropping the owned write half sends FIN
        Ok(())
    }

    async fn send(&self, message: CtrlMessage) -> Result<(), SignalError> {
        let mut guard = self.writer.lock().await;
        match guard.as_mut() {
            Some(writer) => {
                let mut buf = Vec::with_capacity(128);
                CtrlFrame {
                    version: codec::VERSION,
                    message,
                }
                .encode(&mut buf);
                writer.write_all(&buf).await?;
                Ok(())
            }
            None => Err(SignalError::Closed),
        }
    }

    /// Encode helper for tests.
    pub fn frame_of(message: CtrlMessage) -> CtrlFrame {
        CtrlFrame {
            version: crate::codec::VERSION,
            message,
        }
    }
}

/// Background reader: bounded frame reads pushed onto the inbound queue.
/// Any decode error or EOF terminates the queue, surfacing as `Closed`.
async fn read_loop(mut read: OwnedReadHalf, inbox: mpsc::UnboundedSender<CtrlMessage>) {
    loop {
        match util::read_frame(&mut read).await {
            Ok(Some(bytes)) => match CtrlFrame::decode(&bytes) {
                Ok(frame) => {
                    if inbox.send(frame.message).is_err() {
                        return; // client dropped
                    }
                }
                Err(e) => {
                    log::debug!("signaling decode error, closing inbound queue: {e}");
                    return;
                }
            },
            Ok(None) | Err(_) => return,
        }
    }
}

/// Test helpers shared across this crate's test modules. Signing goes
/// through ed25519-dalek directly because DeviceKeyPair construction is a
/// parallel-agent stub that panics until ak-identity lands.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::codec::hello_signing_bytes;
    use ed25519_dalek::{Signer, SigningKey};

    pub(crate) fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    pub(crate) fn device(signing: &SigningKey) -> DeviceId {
        signing.verifying_key().to_bytes()
    }

    fn hello_with_timestamp(signing: &SigningKey, name: &str, timestamp: u64) -> CtrlMessage {
        let device_id = device(signing);
        let signature = signing
            .sign(&hello_signing_bytes(1, &device_id, name, timestamp))
            .to_bytes();
        CtrlMessage::Hello {
            version: 1,
            device_id,
            name: name.to_string(),
            timestamp_unix_ms: timestamp,
            signature,
        }
    }

    pub(crate) async fn connect(
        server: SocketAddr,
        signing: &SigningKey,
        name: &str,
    ) -> SignalClient {
        let hello = hello_with_timestamp(signing, name, util::fresh_now_unix_ms());
        SignalClient::connect_hello(server, hello).await.unwrap()
    }

    /// Connect with a properly signed but stale Hello.
    pub(crate) async fn connect_stale(
        server: SocketAddr,
        signing: &SigningKey,
        name: &str,
    ) -> Result<SignalClient, SignalError> {
        let stale = util::fresh_now_unix_ms() - 60_000;
        let hello = hello_with_timestamp(signing, name, stale);
        SignalClient::connect_hello(server, hello).await
    }

    /// Connect with a fresh timestamp but a signature from the wrong key.
    pub(crate) async fn connect_forged(
        server: SocketAddr,
        signing: &SigningKey,
        name: &str,
    ) -> Result<SignalClient, SignalError> {
        let impostor = key(99);
        // device_id is `signing`'s, but the signature comes from the
        // impostor key, so verification must fail.
        let device_id = device(signing);
        let timestamp = util::fresh_now_unix_ms();
        let signature = impostor
            .sign(&hello_signing_bytes(1, &device_id, name, timestamp))
            .to_bytes();
        let forged = CtrlMessage::Hello {
            version: 1,
            device_id,
            name: name.to_string(),
            timestamp_unix_ms: timestamp,
            signature,
        };
        SignalClient::connect_hello(server, forged).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::hello_signing_bytes;
    use crate::server::test_support as server_support;
    use std::time::Duration;

    #[tokio::test]
    async fn connect_refused_is_an_io_error() {
        let hello = {
            let signing = test_support::key(1);
            let device_id = test_support::device(&signing);
            CtrlMessage::Hello {
                version: 1,
                device_id,
                name: "x".to_string(),
                timestamp_unix_ms: util::now_unix_ms(),
                signature: [0; 64],
            }
        };
        let result = SignalClient::connect_hello("127.0.0.1:1".parse().unwrap(), hello).await;
        assert!(matches!(result, Err(SignalError::Io(_))));
    }

    #[tokio::test]
    async fn osrng_generated_key_passes_hello_verification() {
        use ed25519_dalek::Signer;
        let signing = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let device_id = test_support::device(&signing);
        let name = "osrng";
        let timestamp = util::now_unix_ms();
        let signature = signing
            .sign(&hello_signing_bytes(1, &device_id, name, timestamp))
            .to_bytes();
        let hello = CtrlMessage::Hello {
            version: 1,
            device_id,
            name: name.to_string(),
            timestamp_unix_ms: timestamp,
            signature,
        };
        assert!(codec::verify_hello(&hello, timestamp).is_ok());
    }

    #[tokio::test]
    async fn close_then_next_reports_closed() {
        let server = server_support::start(crate::server::Limits::default()).await;
        let client = test_support::connect(server.tcp, &test_support::key(3), "solo").await;
        client.close().await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap();
        assert!(matches!(result, Err(SignalError::Closed)));
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn device_id_matches_key() {
        let server = server_support::start(crate::server::Limits::default()).await;
        let signing = test_support::key(4);
        let client = test_support::connect(server.tcp, &signing, "whoami").await;
        assert_eq!(client.device_id(), test_support::device(&signing));
        server.stop().await.unwrap();
    }
}
