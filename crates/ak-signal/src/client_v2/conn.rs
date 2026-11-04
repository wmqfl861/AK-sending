//! Connection lifecycle for the v2 client: pinned TLS handshake, the
//! AuthChallenge/Hello/Welcome exchange, and the single reader / single
//! writer tasks that own the stream after that.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use super::error::{ClientV2Error, ERR_DEVICE_REPLACED};
use super::tls;
use super::Inner;
use crate::protocol_v2::{
    CtrlFrame, CtrlMessage, Nonce32, RegistryId, Role, HEADER_LEN, MAX_FRAME_BYTES, VERSION,
};

/// One outbound item for the writer task. Unboxed on purpose:
/// `CtrlMessage` is the frozen unboxed message set (its largest variant is
/// 584 wire bytes), and outbound items are short-lived queue entries, never
/// stored in bulk.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Outbound {
    Frame(CtrlMessage),
    /// Best-effort TLS `close_notify`, then the writer exits.
    Shutdown,
}

/// Read exactly one v2 frame from any async reader. `Ok(None)` = clean EOF.
/// The declared length is checked against the 4096-byte cap before any
/// payload is allocated (same rule as the v1 `util::read_frame`).
pub(crate) async fn read_v2_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    if header[0] != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported frame version",
        ));
    }
    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if HEADER_LEN + declared > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds hard cap",
        ));
    }
    let mut frame = header.to_vec();
    frame.resize(HEADER_LEN + declared, 0);
    reader.read_exact(&mut frame[HEADER_LEN..]).await?;
    Ok(Some(frame))
}

/// Read one frame and decode it as a frame authored by the server; decode
/// and direction errors close the connection (closed message set, closed
/// direction table — protocol-v2.md §8/§10).
async fn read_server_message<S: AsyncRead + Unpin>(
    stream: &mut S,
    timeout: std::time::Duration,
) -> Result<CtrlMessage, ClientV2Error> {
    let bytes = tokio::time::timeout(timeout, read_v2_frame(stream))
        .await
        .map_err(|_| ClientV2Error::HandshakeTimeout(timeout))?
        .map_err(ClientV2Error::Io)?
        .ok_or(ClientV2Error::ConnectionLost)?;
    Ok(CtrlFrame::decode_as(&bytes, Role::Client)
        .map_err(ClientV2Error::Decode)?
        .message)
}

/// Open one pinned-TLS control connection and drive the frozen opening
/// exchange (`pairing-flow.md` §3, §6):
///
/// 1. TCP + TLS 1.3 with the registry fingerprint pin,
/// 2. server sends `AuthChallenge` — its `registry_id` must equal the pin
///    and the challenge must not already be expired,
/// 3. client answers `Hello` signed over
///    `"AKHELLO2" || registry_id || TLS exporter || canonical fields`,
/// 4. server answers `Welcome` (or `ServerError`).
///
/// On success the reader/writer tasks are running and the shared state says
/// `Connected(epoch)`.
pub(crate) async fn open_connection(inner: &Arc<Inner>) -> Result<(), ClientV2Error> {
    let connect_timeout = inner.config.connect_timeout;

    let tcp = tokio::time::timeout(connect_timeout, TcpStream::connect(inner.server))
        .await
        .map_err(|_| ClientV2Error::HandshakeTimeout(connect_timeout))?
        .map_err(ClientV2Error::Io)?;
    tcp.set_nodelay(true).map_err(ClientV2Error::Io)?;

    let config = Arc::new(tls::client_config(&inner.registry_fp).map_err(ClientV2Error::Io)?);
    let connector = TlsConnector::from(config);
    let mut stream: TlsStream<TcpStream> = tokio::time::timeout(
        connect_timeout,
        connector.connect(tls::server_name(inner.server.ip()), tcp),
    )
    .await
    .map_err(|_| ClientV2Error::HandshakeTimeout(connect_timeout))?
    .map_err(|e| {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("TLS handshake rejected: {e}"),
        )
    })?;

    // ---- AuthChallenge ----
    let challenge = match read_server_message(&mut stream, connect_timeout).await? {
        CtrlMessage::AuthChallenge {
            registry_id,
            nonce,
            expires_ms,
        } => AuthChallengeView {
            registry_id,
            nonce,
            expires_ms,
        },
        CtrlMessage::ServerError { code, message } => {
            return Err(ClientV2Error::Server { code, message })
        }
        other => return Err(ClientV2Error::Unexpected(other)),
    };
    if challenge.registry_id != *inner.registry_fp.as_bytes() {
        return Err(ClientV2Error::RegistryMismatch);
    }
    if challenge.expires_ms <= crate::util::now_unix_ms() {
        return Err(ClientV2Error::ChallengeExpired {
            expires_ms: challenge.expires_ms,
        });
    }

    // ---- Hello (challenge-bound, exporter-bound) ----
    let mut exporter = [0u8; tls::HELLO_EXPORTER_LEN];
    {
        let (_sock, conn) = stream.get_ref();
        conn.export_keying_material(&mut exporter, tls::HELLO_EXPORTER_LABEL, None)
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("TLS exporter unavailable: {e}"),
                )
            })
            .map_err(ClientV2Error::Io)?;
    }
    let timestamp_ms = crate::util::fresh_now_unix_ms();
    let device_id = inner.key.public_key();
    let signing_bytes = tls::hello_v2_signing_bytes(
        &challenge.registry_id,
        &exporter,
        tls::hello_version(),
        &device_id,
        &inner.name,
        timestamp_ms,
        &challenge.nonce,
    );
    let hello = CtrlMessage::Hello {
        version: tls::hello_version(),
        device_id,
        name: inner.name.clone(),
        timestamp_ms,
        challenge: challenge.nonce,
        signature: inner.key.sign(&signing_bytes),
    };
    let mut buf = Vec::with_capacity(96);
    CtrlFrame::new(hello)
        .try_encode_as(Role::Client, &mut buf)
        .map_err(ClientV2Error::Encode)?;
    tokio::time::timeout(connect_timeout, stream.write_all(&buf))
        .await
        .map_err(|_| ClientV2Error::HandshakeTimeout(connect_timeout))?
        .map_err(ClientV2Error::Io)?;

    // ---- Welcome ----
    match read_server_message(&mut stream, connect_timeout).await? {
        CtrlMessage::Welcome {
            session_ttl_secs, ..
        } if session_ttl_secs > 0 => {}
        CtrlMessage::ServerError { code, message } => {
            return Err(ClientV2Error::Server { code, message })
        }
        other => return Err(ClientV2Error::Unexpected(other)),
    }

    // ---- Hand over to the long-lived tasks ----
    // The epoch is claimed before the tasks exist so a lagging task of a
    // previous generation can never overwrite the newer connection's state.
    let epoch = inner.claim_epoch();
    let (read_half, write_half) = tokio::io::split(stream);
    let (tx, rx) = tokio::sync::mpsc::channel(inner.config.outbox_capacity);
    inner.install_connection(epoch, tx);
    let reader_inner = Arc::clone(inner);
    let reader_epoch = epoch;
    tokio::spawn(async move {
        reader_loop(reader_inner, read_half, reader_epoch).await;
    });
    let writer_inner = Arc::clone(inner);
    let writer_epoch = epoch;
    tokio::spawn(async move {
        writer_loop(writer_inner, write_half, rx, writer_epoch).await;
    });
    Ok(())
}

struct AuthChallengeView {
    registry_id: RegistryId,
    nonce: Nonce32,
    expires_ms: u64,
}

/// The single reader: consumes every frame, watches for the
/// replaced-connection notice, and routes to the bounded inboxes.
async fn reader_loop(inner: Arc<Inner>, mut read: ReadHalf<TlsStream<TcpStream>>, epoch: u64) {
    loop {
        match read_v2_frame(&mut read).await {
            Ok(Some(bytes)) => match CtrlFrame::decode_as(&bytes, Role::Client) {
                Ok(frame) => {
                    if let CtrlMessage::ServerError { code, .. } = &frame.message {
                        if *code == ERR_DEVICE_REPLACED {
                            inner.note_replaced();
                        }
                    }
                    inner.route_inbound(frame.message);
                }
                Err(e) => {
                    log::debug!("client_v2: decode error, dropping connection: {e}");
                    inner.on_disconnect(epoch);
                    return;
                }
            },
            Ok(None) => {
                log::debug!("client_v2: registry closed the connection");
                inner.on_disconnect(epoch);
                return;
            }
            Err(e) => {
                log::debug!("client_v2: read error, dropping connection: {e}");
                inner.on_disconnect(epoch);
                return;
            }
        }
    }
}

/// The single writer: serializes every outbound frame through one task so
/// concurrent requests can never interleave bytes inside one frame.
async fn writer_loop(
    inner: Arc<Inner>,
    mut write: WriteHalf<TlsStream<TcpStream>>,
    mut rx: tokio::sync::mpsc::Receiver<Outbound>,
    epoch: u64,
) {
    while let Some(outbound) = rx.recv().await {
        match outbound {
            Outbound::Frame(message) => {
                let mut buf = Vec::with_capacity(96);
                if let Err(e) = CtrlFrame::new(message).try_encode_as(Role::Client, &mut buf) {
                    // A message this client will not emit (direction or
                    // budget): refuse it and keep the connection.
                    log::warn!("client_v2: refusing to encode outbound frame: {e}");
                    continue;
                }
                if let Err(e) = write.write_all(&buf).await {
                    log::debug!("client_v2: write failed, dropping connection: {e}");
                    inner.on_disconnect(epoch);
                    return;
                }
            }
            Outbound::Shutdown => {
                let _ = write.shutdown().await;
                return;
            }
        }
    }
    // All senders dropped (reconnect replaced the outbox): close politely.
    let _ = write.shutdown().await;
}
