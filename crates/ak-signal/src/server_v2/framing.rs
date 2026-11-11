//! v2 frame I/O over any tokio stream: reads `[version=2][type][len u16 BE]
//! [payload]` frames up to the 4096-byte cap, rejecting v1 frames and
//! over-long declarations **before** allocating the payload.
//!
//! Direction legality is enforced on the write path
//! ([`write_message`]); the caller decodes with
//! ``protocol_v2::frame::CtrlFrame::decode_as`` to enforce it on the read
//! path. This module never speaks v1: a version-1 header byte is an I/O
//! error, so the v2 port cannot be downgraded into the plaintext codec.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol_v2::frame::{CtrlFrame, EncodeError, HEADER_LEN, MAX_PAYLOAD_BYTES, VERSION};
use crate::protocol_v2::{CtrlMessage, Role};

fn invalid_data(context: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, context)
}

/// Read one v2 frame. `Ok(None)` is a clean EOF between frames. Version
/// bytes other than 2 and declared lengths over the cap are errors so the
/// connection is dropped before any allocation.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    // No downgrade: the v2 port refuses v1 frames at the header byte.
    if header[0] != VERSION {
        return Err(invalid_data("frame version is not 2"));
    }
    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if declared > MAX_PAYLOAD_BYTES {
        return Err(invalid_data("frame exceeds the hard cap"));
    }
    let mut frame = header.to_vec();
    frame.resize(HEADER_LEN + declared, 0);
    reader.read_exact(&mut frame[HEADER_LEN..]).await?;
    Ok(Some(frame))
}

/// Encode and write one frame (message plus optional wave-28 extension
/// section) as sent by `sender`. Encoding is fallible per contract
/// (`try_encode_as`): an over-budget frame is an I/O error, never a
/// truncated frame, and an illegally-directed message is refused.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &CtrlFrame,
    sender: Role,
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(HEADER_LEN + 64);
    frame
        .try_encode_as(sender, &mut buf)
        .map_err(|e| invalid_data(encode_context(&e)))?;
    writer.write_all(&buf).await
}

/// Encode and write one plain message (no wave-28 extension) as sent by
/// `sender`; convenience wrapper over [`write_frame`].
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &CtrlMessage,
    sender: Role,
) -> io::Result<()> {
    write_frame(writer, &CtrlFrame::new(message.clone()), sender).await
}

fn encode_context(error: &EncodeError) -> &'static str {
    match error {
        EncodeError::PayloadTooLarge { .. } => "message payload exceeds the frame cap",
        EncodeError::WrongDirection { .. } => "message may not travel this direction",
        _ => "message failed budget validation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol_v2::DecodeError;
    use tokio::io::duplex;

    fn ping(nonce: u8) -> CtrlMessage {
        CtrlMessage::Ping { nonce: [nonce; 16] }
    }

    #[tokio::test]
    async fn frames_roundtrip_over_a_generic_stream() {
        let (mut client, mut server) = duplex(4096);
        write_message(&mut client, &ping(7), Role::Client)
            .await
            .unwrap();
        let bytes = read_frame(&mut server).await.unwrap().unwrap();
        let frame = CtrlFrame::decode_as(&bytes, Role::Server).unwrap();
        assert_eq!(frame.message, ping(7));
    }

    #[tokio::test]
    async fn version_one_headers_are_rejected_before_payload_allocation() {
        let (mut client, mut server) = duplex(64);
        // v1-shaped header: version byte 1.
        client.write_all(&[1u8, 0x01, 0x00, 0x00]).await.unwrap();
        let err = read_frame(&mut server).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn over_cap_declarations_are_rejected() {
        let (mut client, mut server) = duplex(64);
        client.write_all(&[2u8, 0x01, 0xFF, 0xFF]).await.unwrap();
        let err = read_frame(&mut server).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn truncated_payload_is_an_error_and_clean_eof_is_none() {
        let (mut client, mut server) = duplex(64);
        // Header declares 4 payload bytes; only 1 arrives before EOF.
        client
            .write_all(&[2u8, 0x1D, 0x00, 0x04, 0xAA])
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let err = read_frame(&mut server).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        // A close exactly between frames reads as `None`, not an error.
        let (mut a, mut b) = duplex(64);
        a.shutdown().await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn wrong_direction_writes_are_refused() {
        let (mut a, b) = duplex(64);
        // A client must not encode Welcome (server-to-client only).
        let err = write_message(
            &mut a,
            &CtrlMessage::Welcome {
                session_ttl_secs: 1,
                capabilities: 1,
            },
            Role::Client,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = b;
    }

    #[tokio::test]
    async fn unknown_type_bytes_survive_the_wire_but_fail_decode() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&[2u8, 0x7F, 0x00, 0x00]).await.unwrap();
        let bytes = read_frame(&mut b).await.unwrap().unwrap();
        assert!(matches!(
            CtrlFrame::decode(&bytes),
            Err(DecodeError::UnknownType(0x7F))
        ));
    }
}
