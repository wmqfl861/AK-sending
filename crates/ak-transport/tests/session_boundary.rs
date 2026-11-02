use ak_core::job::State;
use ak_transport::{
    AuthenticatedDirectConnection, Candidate, DirectEndpoint, Error, LocalIdentity, PairedPeer,
    ReceiveGrant, ReceivedBatch, SessionReceiver, SessionSender, MAX_BATCH_BYTES,
};
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;
use tokio::time::timeout;
const TTL: Duration = Duration::from_secs(60);
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

async fn sessions() -> (SessionSender, SessionReceiver) {
    let (mut a, mut b) = endpoints();
    let (a, b) = pair(&mut a, &mut b).await;
    let (sender, receiver) = tokio::join!(
        SessionSender::open(a, [3; 16]),
        SessionReceiver::accept(b, [3; 16])
    );
    (sender.unwrap(), receiver.unwrap())
}
fn approve(receiver: &mut SessionReceiver) -> ReceiveGrant {
    let request = receiver.consent_request().unwrap().unwrap();
    receiver.approve(&request, TTL).unwrap()
}

#[tokio::test]
async fn local_consent_gates_two_batches_and_end_not_file_completion() {
    let (mut sender, mut receiver) = sessions().await;
    assert_eq!(receiver.state(), State::AwaitingConsent);
    let grant = approve(&mut receiver);
    receiver.ready(&grant).await.unwrap();
    for data in [vec![9], vec![7; MAX_BATCH_BYTES]] {
        let (sent, received) =
            tokio::join!(sender.send_batch(&data), receiver.receive_batch(&grant));
        sent.unwrap();
        assert_eq!(received.unwrap(), ReceivedBatch::Data(data));
    }
    let (sent, received) = tokio::join!(sender.finish(), receiver.receive_batch(&grant));
    sent.unwrap();
    assert_eq!(received.unwrap(), ReceivedBatch::Ended);
    assert_ne!(receiver.state(), State::Completed);
    assert!(receiver.receive_batch(&grant).await.is_err());
}

#[tokio::test]
async fn sender_cannot_send_before_local_ready_and_outer_timeout_closes() {
    let (mut sender, receiver) = sessions().await;
    assert!(
        timeout(Duration::from_millis(20), sender.send_batch(b"synthetic"))
            .await
            .is_err()
    );
    assert!(sender.abort_handle().is_closed());
    assert_eq!(receiver.state(), State::AwaitingConsent);
}

#[tokio::test]
async fn same_task_grants_cannot_cross_connections() {
    let (_sa, mut ra) = sessions().await;
    let (_sb, mut rb) = sessions().await;
    let ga = approve(&mut ra);
    let _gb = approve(&mut rb);
    assert_eq!(rb.ready(&ga).await, Err(Error::InvalidGrant));
    assert!(rb.abort_handle().is_closed());
    assert!(!ra.abort_handle().is_closed());
}

#[tokio::test]
async fn expired_permission_never_produces_ready() {
    let (_sender, mut receiver) = sessions().await;
    let request = receiver.consent_request().unwrap().unwrap();
    let grant = receiver
        .approve(&request, Duration::from_millis(1))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(receiver.ready(&grant).await.is_err());
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn expired_permission_during_pending_read_returns_no_body() {
    let (mut sender, mut receiver) = sessions().await;
    let request = receiver.consent_request().unwrap().unwrap();
    let grant = receiver
        .approve(&request, Duration::from_millis(10))
        .unwrap();
    receiver.ready(&grant).await.unwrap();
    let (sent, received) = tokio::join!(
        async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            sender.send_batch(b"not delivered").await
        },
        receiver.receive_batch(&grant)
    );
    assert!(sent.is_err());
    assert!(received.is_err());
    assert!(receiver.abort_handle().is_closed());
}

#[tokio::test]
async fn cloneable_abort_interrupts_pending_read_without_permission_handle() {
    let (_sender, mut receiver) = sessions().await;
    let grant = approve(&mut receiver);
    receiver.ready(&grant).await.unwrap();
    let abort = receiver.abort_handle();
    let (result, ()) = tokio::join!(receiver.receive_batch(&grant), async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        abort.abort();
    });
    assert!(result.is_err());
    assert!(abort.is_closed());
}

#[tokio::test]
async fn dropped_polled_operations_close_but_unpolled_borrows_do_not() {
    for sending in [false, true] {
        let (mut sender, mut receiver) = sessions().await;
        let grant = approve(&mut receiver);
        receiver.ready(&grant).await.unwrap();
        let abort = if sending {
            sender.abort_handle()
        } else {
            receiver.abort_handle()
        };
        {
            let operation = async {
                if sending {
                    sender.send_batch(b"data").await
                } else {
                    receiver.receive_batch(&grant).await.map(|_| ())
                }
            };
            drop(operation);
        }
        assert!(!abort.is_closed());
        let mut operation = Box::pin(async {
            if sending {
                sender.send_batch(b"data").await
            } else {
                receiver.receive_batch(&grant).await.map(|_| ())
            }
        });
        assert!(poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx).is_pending())).await);
        drop(operation);
        assert!(abort.is_closed());
    }
}

#[tokio::test]
async fn revoke_pause_cancel_and_reject_cannot_be_reused() {
    for action in 0..4 {
        let (mut sender, mut receiver) = sessions().await;
        let request = receiver.consent_request().unwrap().unwrap();
        if action == 3 {
            receiver.reject(&request).unwrap();
        } else {
            let grant = receiver.approve(&request, TTL).unwrap();
            receiver.ready(&grant).await.unwrap();
            match action {
                0 => receiver.revoke().unwrap(),
                1 => receiver.pause().unwrap(),
                _ => receiver.cancel().unwrap(),
            }
            assert!(receiver.receive_batch(&grant).await.is_err());
        }
        assert!(sender.send_batch(b"not delivered").await.is_err());
        assert!(receiver.abort_handle().is_closed());
    }
}

#[tokio::test]
async fn invalid_outgoing_sizes_close_without_unbounded_allocation() {
    for size in [0, MAX_BATCH_BYTES + 1] {
        let (mut sender, _receiver) = sessions().await;
        assert_eq!(
            sender.send_batch(&vec![0; size]).await,
            Err(Error::Protocol)
        );
        assert!(sender.abort_handle().is_closed());
    }
}

#[tokio::test]
async fn task_mismatch_during_hello_is_rejected() {
    let (mut a, mut b) = endpoints();
    let (a, b) = pair(&mut a, &mut b).await;
    let (_sender, receiver) = tokio::join!(
        SessionSender::open(a, [3; 16]),
        SessionReceiver::accept(b, [4; 16])
    );
    assert!(matches!(receiver, Err(Error::Protocol)));
}
