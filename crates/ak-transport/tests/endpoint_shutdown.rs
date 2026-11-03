use ak_transport::{
    AuthenticatedDirectConnection, Candidate, DirectEndpoint, LocalIdentity, PairedPeer,
};
fn endpoints() -> (DirectEndpoint, DirectEndpoint) {
    let a = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).unwrap();
    let b = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).unwrap();
    let aid =
        LocalIdentity::from_der(a.cert.der().to_vec(), a.signing_key.serialize_der()).unwrap();
    let bid =
        LocalIdentity::from_der(b.cert.der().to_vec(), b.signing_key.serialize_der()).unwrap();
    let ap = PairedPeer::from_certificate_der(a.cert.der().to_vec()).unwrap();
    let bp = PairedPeer::from_certificate_der(b.cert.der().to_vec()).unwrap();
    let local = "127.0.0.1:0".parse().unwrap();
    (
        DirectEndpoint::bind(local, aid, bp).unwrap(),
        DirectEndpoint::bind(local, bid, ap).unwrap(),
    )
}
async fn connections(
    a: &mut DirectEndpoint,
    b: &mut DirectEndpoint,
) -> (AuthenticatedDirectConnection, AuthenticatedDirectConnection) {
    let aa = a.local_addr().unwrap();
    let ba = b.local_addr().unwrap();
    let (a, b) = tokio::join!(
        a.connect(Candidate::ManualDirect(ba)),
        b.accept(Candidate::ManualDirect(aa))
    );
    (a.unwrap(), b.unwrap())
}

#[tokio::test]
async fn idle_endpoint_shutdown_completes() {
    let (a, b) = endpoints();
    assert_eq!(a.shutdown().await, Ok(()));
    assert_eq!(b.shutdown().await, Ok(()));
}

#[tokio::test]
async fn endpoint_shutdown_closes_authenticated_peer_before_runtime_exit() {
    let (mut a, mut b) = endpoints();
    let (ca, cb) = connections(&mut a, &mut b).await;
    let (closed, observed) = tokio::join!(a.shutdown(), cb.wait_closed());
    assert_eq!(closed, Ok(()));
    assert_eq!(observed, Ok(()));
    assert!(ca.ensure_live().is_err());
    assert!(cb.ensure_live().is_err());
}
