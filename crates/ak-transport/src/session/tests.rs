use super::*;
use crate::{Candidate, DirectEndpoint, LocalIdentity, PairedPeer};
fn endpoints() -> (DirectEndpoint, DirectEndpoint) {
    let a = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).unwrap();
    let b = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).unwrap();
    let aid =
        LocalIdentity::from_der(a.cert.der().to_vec(), a.signing_key.serialize_der()).unwrap();
    let bid =
        LocalIdentity::from_der(b.cert.der().to_vec(), b.signing_key.serialize_der()).unwrap();
    let ap = PairedPeer::from_certificate_der(a.cert.der().to_vec()).unwrap();
    let bp = PairedPeer::from_certificate_der(b.cert.der().to_vec()).unwrap();
    let address = "127.0.0.1:0".parse().unwrap();
    (
        DirectEndpoint::bind(address, aid, bp).unwrap(),
        DirectEndpoint::bind(address, bid, ap).unwrap(),
    )
}

async fn pair(
    a: &mut DirectEndpoint,
    b: &mut DirectEndpoint,
) -> (AuthenticatedDirectConnection, AuthenticatedDirectConnection) {
    let aa = a.local_addr().unwrap();
    let ba = b.local_addr().unwrap();
    let (ca, cb) = tokio::join!(
        a.connect(Candidate::ManualDirect(ba)),
        b.accept(Candidate::ManualDirect(aa))
    );
    (ca.unwrap(), cb.unwrap())
}

async fn raw_peer() -> (
    AuthenticatedDirectConnection,
    Wire,
    SessionReceiver,
    ReceiveGrant,
) {
    let (mut a, mut b) = endpoints();
    let (a, b) = pair(&mut a, &mut b).await;
    let context = context(&a, [4; 16]).unwrap();
    let raw = a.session_transport();
    let (wire, receiver) = tokio::join!(
        async {
            let (send, receive) = raw.open_bi().await.unwrap();
            let mut wire = Wire {
                send,
                receive,
                context,
            };
            wire.write(Kind::Hello, 0, &[]).await.unwrap();
            wire
        },
        SessionReceiver::accept(b, [4; 16])
    );
    let mut receiver = receiver.unwrap();
    let request = receiver.consent_request().unwrap().unwrap();
    let grant = receiver.approve(&request, Duration::from_secs(60)).unwrap();
    receiver.ready(&grant).await.unwrap();
    (a, wire, receiver, grant)
}

#[test]
fn sequence_exhaustion_never_wraps() {
    assert_eq!(next_sequence(u64::MAX), Err(Error::Protocol));
    assert_eq!(next_sequence(u64::MAX - 1), Ok(u64::MAX));
}

#[tokio::test]
async fn exporter_agrees_on_peers_and_changes_with_task_and_reconnect() {
    let (mut a, mut b) = endpoints();
    let (one, two) = pair(&mut a, &mut b).await;
    let old = one.session_binding(&[7; 16]).unwrap();
    assert_eq!(old, two.session_binding(&[7; 16]).unwrap());
    assert_ne!(old, one.session_binding(&[8; 16]).unwrap());
    drop(one);
    drop(two);
    let (one, two) = pair(&mut a, &mut b).await;
    assert_eq!(
        one.session_binding(&[7; 16]).unwrap(),
        two.session_binding(&[7; 16]).unwrap()
    );
    assert_ne!(old, one.session_binding(&[7; 16]).unwrap());
}

#[tokio::test]
async fn malformed_and_truncated_network_frames_close_before_delivery() {
    // Magic/version/type/flags, sequence, task, TLS binding, length and truncation.
    for case in 0..13 {
        let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
        let mut bytes = Header {
            kind: Kind::Data,
            sequence: 1,
            length: 1,
        }
        .encode(wire.context)
        .unwrap()
        .to_vec();
        match case {
            0 => bytes[0] ^= 1,
            1 => bytes[4] = 2,
            2 => bytes[5] = 250,
            3 => bytes[6] = 1,
            4 => bytes[15] = 0,
            5 => bytes[15] = 2,
            6 => bytes[16] ^= 1,
            7 => bytes[32] ^= 1,
            8 => bytes[64..68].copy_from_slice(&u32::MAX.to_be_bytes()),
            9 => bytes[64..68].copy_from_slice(&0u32.to_be_bytes()),
            10 => bytes.truncate(10),
            11 => (), // Full valid header but missing body.
            _ => {
                bytes[5] = Kind::Ack as u8;
                bytes[64..68].copy_from_slice(&0u32.to_be_bytes());
            }
        }
        wire.send.write_all(&bytes).await.unwrap();
        wire.send.finish().unwrap();
        assert!(receiver.receive_batch(&grant).await.is_err(), "case {case}");
        assert!(receiver.abort_handle().is_closed());
    }
}

#[tokio::test]
async fn duplicate_data_is_not_delivered_twice() {
    let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
    wire.read(&[Kind::Ready], 0).await.unwrap();
    for attempt in 0..2 {
        wire.write(Kind::Data, 1, b"batch").await.unwrap();
        let result = receiver.receive_batch(&grant).await;
        if attempt == 0 {
            assert_eq!(result.unwrap(), ReceivedBatch::Data(b"batch".to_vec()));
            wire.read(&[Kind::Ack], 1).await.unwrap();
        } else {
            assert!(result.is_err());
        }
    }
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn trailing_bytes_after_end_are_rejected() {
    let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
    wire.write(Kind::End, 1, &[]).await.unwrap();
    wire.send.write_all(b"extra").await.unwrap();
    wire.send.finish().unwrap();
    assert_eq!(receiver.receive_batch(&grant).await, Err(Error::Protocol));
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn exhausted_data_sequence_is_rejected_without_ack() {
    let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
    receiver.next = u64::MAX;
    wire.write(Kind::Data, u64::MAX, b"data").await.unwrap();
    assert_eq!(receiver.receive_batch(&grant).await, Err(Error::Protocol));
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn wrong_ready_and_wrong_ack_are_rejected_by_sender() {
    for wrong_ready in [false, true] {
        let (mut a, mut b) = endpoints();
        let (a, b) = pair(&mut a, &mut b).await;
        let context = context(&b, [5; 16]).unwrap();
        let raw = b.session_transport();
        let (sender, streams) = tokio::join!(SessionSender::open(a, [5; 16]), raw.accept_bi());
        let mut sender = sender.unwrap();
        let (send, receive) = streams.unwrap();
        let mut wire = Wire {
            send,
            receive,
            context,
        };
        wire.read(&[Kind::Hello], 0).await.unwrap();
        let (result, ()) = tokio::join!(sender.send_batch(b"data"), async {
            if wrong_ready {
                wire.write(Kind::Hello, 0, &[]).await.unwrap();
            } else {
                wire.write(Kind::Ready, 0, &[]).await.unwrap();
                wire.read(&[Kind::Data], 1).await.unwrap();
                wire.write(Kind::Ack, 2, &[]).await.unwrap();
            }
        });
        assert_eq!(result, Err(Error::Protocol));
        assert!(sender.abort_handle().is_closed());
    }
}

#[tokio::test]
async fn wrong_terminal_close_does_not_report_ended() {
    let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
    wire.read(&[Kind::Ready], 0).await.unwrap();
    wire.write(Kind::End, 1, &[]).await.unwrap();
    wire.send.finish().unwrap();
    let raw = _owner.session_transport();
    let (received, ()) = tokio::join!(receiver.receive_batch(&grant), async {
        wire.read(&[Kind::EndAck], 1).await.unwrap();
        raw.close(1_u32.into(), b"not normal completion");
    });
    assert_eq!(received, Err(Error::Closed));
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn cancelled_terminal_wait_closes_instead_of_reporting_ended() {
    let (_owner, mut wire, mut receiver, grant) = raw_peer().await;
    wire.read(&[Kind::Ready], 0).await.unwrap();
    wire.write(Kind::End, 1, &[]).await.unwrap();
    wire.send.finish().unwrap();
    let result = timeout(Duration::from_millis(30), receiver.receive_batch(&grant)).await;
    assert!(result.is_err());
    assert!(receiver.abort_handle().is_closed());
    assert!(receiver.receive_batch(&grant).await.is_err());
}
