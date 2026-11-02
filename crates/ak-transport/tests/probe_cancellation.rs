use ak_transport::{
    AuthenticatedDirectConnection, Candidate, DirectEndpoint, Error, LocalIdentity, PairedPeer,
};
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;
use tokio::time::timeout;

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

#[tokio::test]
async fn outer_timeout_closes_both_probe_roles() {
    for responder in [false, true] {
        let (mut a, mut b) = endpoints();
        let (mut connection, _other) = pair(&mut a, &mut b).await;
        let deadline = Duration::from_millis(20);
        let cancelled = if responder {
            timeout(deadline, connection.respond_to_probe()).await
        } else {
            timeout(deadline, connection.probe()).await
        };
        assert!(cancelled.is_err());
        assert_eq!(connection.ensure_live(), Err(Error::Closed));
        assert!(matches!(
            connection.into_receive_session([1; 16]),
            Err(Error::Closed)
        ));
    }
}

#[tokio::test]
async fn dropping_polled_probe_future_closes_both_roles() {
    for responder in [false, true] {
        let (mut a, mut b) = endpoints();
        let (mut connection, _other) = pair(&mut a, &mut b).await;
        let mut operation = Box::pin(async {
            if responder {
                connection.respond_to_probe().await
            } else {
                connection.probe().await
            }
        });
        let pending = poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx).is_pending())).await;
        assert!(pending);
        drop(operation);
        assert_eq!(connection.ensure_live(), Err(Error::Closed));
    }
}
