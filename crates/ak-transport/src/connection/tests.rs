use super::*;
use crate::identity::{tls_configs, transport_config};
use crate::{LocalIdentity, PairedPeer, SERVER_NAME};
use rcgen::{CertificateParams, KeyPair};

fn material(name: &str, expired: bool) -> (Vec<u8>, Vec<u8>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
    if expired {
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let cert = params.self_signed(&key).unwrap();
    (cert.der().to_vec(), key.serialize_der())
}

fn local(parts: &(Vec<u8>, Vec<u8>)) -> LocalIdentity {
    LocalIdentity::from_der(parts.0.clone(), parts.1.clone()).unwrap()
}
fn peer(parts: &(Vec<u8>, Vec<u8>)) -> PairedPeer {
    PairedPeer::from_certificate_der(parts.0.clone()).unwrap()
}
fn bind(parts: &(Vec<u8>, Vec<u8>), paired: &(Vec<u8>, Vec<u8>)) -> DirectEndpoint {
    DirectEndpoint::bind("127.0.0.1:0".parse().unwrap(), local(parts), peer(paired)).unwrap()
}
fn endpoints() -> (DirectEndpoint, DirectEndpoint) {
    let a = material(SERVER_NAME, false);
    let b = material(SERVER_NAME, false);
    (bind(&a, &b), bind(&b, &a))
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

#[test]
fn rejects_relay_unknown_and_invalid_addresses() {
    let relay = Candidate::Relay("127.0.0.1:1".parse().unwrap());
    assert_eq!(relay.address(), Err(Error::UntrustedPath));
    assert_eq!(Candidate::Unknown.address(), Err(Error::UntrustedPath));
    let invalid = [
        "0.0.0.0:12", "0.1.2.3:12", "224.0.0.1:12", "255.255.255.255:12",
        "240.0.0.1:12", "127.0.0.1:0", "[::]:12", "[ff02::1]:12",
        "[::ffff:127.0.0.1]:12", "[fe80::1]:12",
    ];
    for address in invalid {
        let candidate = Candidate::ManualDirect(address.parse().unwrap());
        assert_eq!(candidate.address(), Err(Error::InvalidAddress));
    }
    assert!(validate_address("127.0.0.1:0".parse().unwrap(), true).is_ok());
    assert!(Candidate::ManualDirect("[::1]:12".parse().unwrap()).address().is_ok());
}

#[test]
fn empty_or_oversize_identity_and_invalid_pairing_fail() {
    assert!(LocalIdentity::from_der(vec![], vec![1]).is_err());
    assert!(LocalIdentity::from_der(vec![1], vec![0; 16_385]).is_err());
    assert!(PairedPeer::from_certificate_der(vec![]).is_err());
    assert!(PairedPeer::from_certificate_der(vec![1, 2, 3]).is_err());
}

#[test]
fn certificate_pin_is_exact_not_just_a_ca_or_subject_name() {
    let a = material(SERVER_NAME, false);
    let b = material(SERVER_NAME, false);
    let pin = peer(&a);
    let cert = CertificateDer::from(a.0);
    assert!(pin.matches(std::slice::from_ref(&cert)));
    assert!(!pin.matches(&[CertificateDer::from(b.0)]));
    assert!(!pin.matches(&[cert.clone(), cert]));
    assert!(!pin.matches(&[]));
}

#[test]
fn early_data_and_tls_tickets_are_disabled() {
    let a = material(SERVER_NAME, false);
    let b = material(SERVER_NAME, false);
    let (server, client) = tls_configs(&local(&a), &peer(&b)).unwrap();
    assert_eq!(server.max_early_data_size, 0);
    assert_eq!(server.send_tls13_tickets, 0);
    assert!(!client.enable_early_data);
    assert_eq!(server.alpn_protocols, vec![ALPN.to_vec()]);
    assert_eq!(client.alpn_protocols, vec![ALPN.to_vec()]);
}

#[tokio::test]
async fn key_mismatch_and_unspecified_bind_are_rejected() {
    let a = material(SERVER_NAME, false);
    let b = material(SERVER_NAME, false);
    let mismatched = LocalIdentity::from_der(a.0.clone(), b.1.clone()).unwrap();
    let address = "127.0.0.1:0".parse().unwrap();
    assert!(DirectEndpoint::bind(address, mismatched, peer(&b)).is_err());
    let unspecified = "0.0.0.0:0".parse().unwrap();
    assert!(DirectEndpoint::bind(unspecified, local(&a), peer(&b)).is_err());
}

#[tokio::test]
async fn real_mutual_tls_fixed_probe_and_actual_socket_addresses() {
    let (mut a, mut b) = endpoints();
    let aa = a.local_addr().unwrap();
    let ba = b.local_addr().unwrap();
    let (mut ca, mut cb) = connections(&mut a, &mut b).await;
    assert_eq!(ca.local_addr(), aa);
    assert_eq!(ca.remote_addr(), ba);
    assert_eq!(cb.remote_addr(), aa);
    assert_eq!(ca.peer_id(), a.peer.id());
    assert_eq!(cb.peer_id(), b.peer.id());
    assert_eq!(ca.connection.max_datagram_size(), None);
    let (ping, pong) = tokio::join!(ca.probe(), cb.respond_to_probe());
    ping.unwrap();
    pong.unwrap();
}

async fn rejected_handshake(name: &str, expired: bool, wrong_server: bool, wrong_client: bool) {
    let am = material(SERVER_NAME, false);
    let bm = material(name, expired);
    let other = material(SERVER_NAME, false);
    let mut a = bind(&am, if wrong_server { &other } else { &bm });
    let mut b = bind(&bm, if wrong_client { &other } else { &am });
    let aa = a.local_addr().unwrap();
    let ba = b.local_addr().unwrap();
    let (client, server) = tokio::join!(
        a.connect(Candidate::ManualDirect(ba)),
        b.accept(Candidate::ManualDirect(aa))
    );
    assert!(server.is_err());
    if wrong_client {
        // TLS clients can finish locally before the server rejects their certificate.
        // No receiver-side wrapper is created; a transient client handle must close.
        if let Ok(connection) = client {
            connection.wait_closed().await.unwrap();
            assert_eq!(connection.ensure_live(), Err(Error::Closed));
        }
    } else {
        assert!(client.is_err());
    }
}

#[tokio::test]
async fn unpaired_server_is_rejected() {
    rejected_handshake(SERVER_NAME, false, true, false).await;
}
#[tokio::test]
async fn unpaired_client_is_rejected() {
    rejected_handshake(SERVER_NAME, false, false, true).await;
}
#[tokio::test]
async fn expired_server_is_rejected_even_when_pinned() {
    rejected_handshake(SERVER_NAME, true, false, false).await;
}
#[tokio::test]
async fn wrong_tls_name_is_rejected_even_when_pinned() {
    rejected_handshake("other.invalid", false, false, false).await;
}

async fn raw_client_rejected(no_certificate: bool) {
    let am = material(SERVER_NAME, false);
    let bm = material(SERVER_NAME, false);
    let mut b = bind(&bm, &am);
    let mut tls = if no_certificate {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(bm.0.clone())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13]).unwrap()
            .with_root_certificates(roots).with_no_client_auth()
    } else {
        tls_configs(&local(&am), &peer(&bm)).unwrap().1
    };
    tls.alpn_protocols = vec![if no_certificate { ALPN.to_vec() } else { b"wrong/alpn".to_vec() }];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport_config());
    let mut raw = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    raw.set_default_client_config(config);
    let incoming_from = raw.local_addr().unwrap();
    let connecting = raw.connect(b.local_addr().unwrap(), SERVER_NAME).unwrap();
    let (_, server) = tokio::join!(connecting, b.accept(Candidate::ManualDirect(incoming_from)));
    assert!(server.is_err());
}

#[tokio::test]
async fn missing_client_certificate_is_rejected() {
    raw_client_rejected(true).await;
}
#[tokio::test]
async fn mismatched_alpn_is_rejected() {
    raw_client_rejected(false).await;
}

#[tokio::test]
async fn unexpected_source_is_refused() {
    let (mut a, mut b) = endpoints();
    let ba = b.local_addr().unwrap();
    let mut wrong = a.local_addr().unwrap();
    wrong.set_port(if wrong.port() == 1 { 2 } else { 1 });
    let (_, server) = tokio::join!(
        a.connect(Candidate::ManualDirect(ba)),
        b.accept(Candidate::ManualDirect(wrong))
    );
    assert!(matches!(server, Err(Error::UnexpectedAddress)));
}

#[tokio::test]
async fn closed_connection_cannot_create_receive_session() {
    let (mut a, mut b) = endpoints();
    let (ca, cb) = connections(&mut a, &mut b).await;
    cb.close();
    ca.wait_closed().await.unwrap();
    assert!(matches!(ca.into_receive_session([1; 16]), Err(Error::Closed)));
}

#[tokio::test]
async fn address_guard_closes_a_mismatched_snapshot() {
    let (mut a, mut b) = endpoints();
    let (mut ca, _cb) = connections(&mut a, &mut b).await;
    // Unit fault injection of the recorded address, NOT a real NAT migration test.
    ca.remote.set_port(1);
    assert_eq!(ca.ensure_live(), Err(Error::PathChanged));
    assert_eq!(ca.ensure_live(), Err(Error::Closed));
}

async fn malformed_probe(payload: &[u8]) {
    let (mut a, mut b) = endpoints();
    let (ca, mut cb) = connections(&mut a, &mut b).await;
    let writer = async {
        let (mut send, _receive) = ca.connection.open_bi().await.unwrap();
        send.write_all(payload).await.unwrap();
        send.finish().unwrap();
    };
    let (_, result) = tokio::join!(writer, cb.respond_to_probe());
    assert_eq!(result, Err(Error::Protocol));
    assert_eq!(cb.ensure_live(), Err(Error::Closed));
}
#[tokio::test]
async fn incorrect_probe_is_rejected() {
    malformed_probe(b"BADMSG00").await;
}
#[tokio::test]
async fn oversize_probe_is_rejected() {
    malformed_probe(b"AKPING01x").await;
}
#[tokio::test]
async fn missing_probe_has_a_bounded_timeout() {
    let (mut a, mut b) = endpoints();
    let (_ca, mut cb) = connections(&mut a, &mut b).await;
    assert_eq!(cb.respond_to_probe().await, Err(Error::Timeout));
    assert_eq!(cb.ensure_live(), Err(Error::Closed));
}
