use ak_core::job::State;
use ak_core::receive::ReceiveError;
use ak_transport::{
    AuthenticatedDirectConnection, Candidate, DirectEndpoint, Error, LocalIdentity, PairedPeer,
};
use std::time::Duration;

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

#[tokio::test]
async fn verified_connection_only_enters_awaiting_consent() {
    let (mut a, mut b) = endpoints();
    let (_sender, receiver) = pair(&mut a, &mut b).await;
    let id = receiver.peer_id();
    let mut session = receiver.into_receive_session([1; 16]).unwrap();
    assert_eq!(session.state(), State::AwaitingConsent);
    let request = session.consent_request().unwrap().unwrap();
    assert_eq!(request.task_id(), [1; 16]);
    assert_eq!(request.peer_id(), id);
    assert!(matches!(
        session.approve(&request, Duration::ZERO),
        Err(Error::Consent(ReceiveError::InvalidLifetime))
    ));
    assert_eq!(session.state(), State::AwaitingConsent);
    let grant = session.approve(&request, TTL).unwrap();
    session.check_receive(&grant).unwrap();
}

#[tokio::test]
async fn grants_and_dialogs_cannot_cross_sessions_even_with_identical_ids() {
    let (mut a, mut b) = endpoints();
    let (_sa, ra) = pair(&mut a, &mut b).await;
    let (_sb, rb) = pair(&mut a, &mut b).await;
    let mut one = ra.into_receive_session([1; 16]).unwrap();
    let mut two = rb.into_receive_session([1; 16]).unwrap();
    let request = one.consent_request().unwrap().unwrap();
    let grant = one.approve(&request, TTL).unwrap();
    assert!(two.approve(&request, TTL).is_err());
    assert_eq!(two.check_receive(&grant), Err(Error::InvalidGrant));
    let request2 = two.consent_request().unwrap().unwrap();
    let grant2 = two.approve(&request2, TTL).unwrap();
    assert_eq!(two.check_receive(&grant), Err(Error::InvalidGrant));
    two.check_receive(&grant2).unwrap();
    one.check_receive(&grant).unwrap();
}

#[tokio::test]
async fn pause_and_reconnect_require_new_dialog_and_new_grant() {
    let (mut a, mut b) = endpoints();
    let (sender, receiver) = pair(&mut a, &mut b).await;
    let mut session = receiver.into_receive_session([1; 16]).unwrap();
    let old_request = session.consent_request().unwrap().unwrap();
    let old_grant = session.approve(&old_request, TTL).unwrap();
    session.pause().unwrap();
    assert!(session.check_receive(&old_grant).is_err());
    drop(sender);
    let (_sender, receiver) = pair(&mut a, &mut b).await;
    session.reconnect(receiver).unwrap();
    assert_eq!(session.state(), State::AwaitingConsent);
    assert!(session.approve(&old_request, TTL).is_err());
    assert_eq!(session.check_receive(&old_grant), Err(Error::InvalidGrant));
    let request = session.consent_request().unwrap().unwrap();
    let grant = session.approve(&request, TTL).unwrap();
    assert_eq!(session.check_receive(&old_grant), Err(Error::InvalidGrant));
    session.check_receive(&grant).unwrap();
}

#[tokio::test]
async fn revoke_cancel_and_reject_close_the_transport() {
    for action in 0..3 {
        let (mut a, mut b) = endpoints();
        let (sender, receiver) = pair(&mut a, &mut b).await;
        let mut session = receiver.into_receive_session([1; 16]).unwrap();
        let request = session.consent_request().unwrap().unwrap();
        if action == 2 {
            session.reject(&request).unwrap();
            assert_eq!(session.state(), State::Rejected);
        } else {
            let grant = session.approve(&request, TTL).unwrap();
            if action == 0 {
                session.revoke().unwrap();
                assert_eq!(session.state(), State::Paused);
            } else {
                session.cancel().unwrap();
                assert_eq!(session.state(), State::Cancelled);
            }
            assert!(session.check_receive(&grant).is_err());
        }
        sender.wait_closed().await.unwrap();
    }
}

#[tokio::test]
async fn expired_grant_cannot_be_used() {
    let (mut a, mut b) = endpoints();
    let (_sender, receiver) = pair(&mut a, &mut b).await;
    let mut session = receiver.into_receive_session([1; 16]).unwrap();
    let request = session.consent_request().unwrap().unwrap();
    let grant = session.approve(&request, Duration::from_millis(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        session.check_receive(&grant),
        Err(Error::Consent(ReceiveError::PermissionExpired))
    );
    assert_eq!(session.state(), State::Paused);
}

#[tokio::test]
async fn actual_disconnect_revokes_permission_and_preserves_waiting_state() {
    let (mut a, mut b) = endpoints();
    let (sender, receiver) = pair(&mut a, &mut b).await;
    let mut session = receiver.into_receive_session([1; 16]).unwrap();
    let request = session.consent_request().unwrap().unwrap();
    let grant = session.approve(&request, TTL).unwrap();
    sender.close();
    session.wait_for_disconnect().await.unwrap();
    assert_eq!(session.state(), State::WaitingDirect);
    assert!(session.check_receive(&grant).is_err());
    assert_eq!(session.state(), State::WaitingDirect);
}

#[tokio::test]
async fn reconnect_with_another_identity_is_rejected_without_state_mutation() {
    let (mut a, mut b) = endpoints();
    let (_sender, receiver) = pair(&mut a, &mut b).await;
    let mut session = receiver.into_receive_session([1; 16]).unwrap();
    session.pause().unwrap();
    let (mut other_a, mut other_b) = endpoints();
    let (_other_sender, other_receiver) = pair(&mut other_a, &mut other_b).await;
    assert_eq!(session.reconnect(other_receiver), Err(Error::PeerMismatch));
    assert_eq!(session.state(), State::Paused);
}

#[tokio::test]
async fn verification_and_save_resume_without_reopening_receive() {
    for saving in [false, true] {
        let (mut a, mut b) = endpoints();
        let (sender, receiver) = pair(&mut a, &mut b).await;
        let mut session = receiver.into_receive_session([1; 16]).unwrap();
        let request = session.consent_request().unwrap().unwrap();
        let grant = session.approve(&request, TTL).unwrap();
        session.all_bytes_received(&grant).unwrap();
        if saving {
            session.verification_passed(&grant).unwrap();
        }
        session.pause().unwrap();
        drop(sender);
        let (_sender, receiver) = pair(&mut a, &mut b).await;
        session.reconnect(receiver).unwrap();
        let request = session.consent_request().unwrap().unwrap();
        let fresh = session.approve(&request, TTL).unwrap();
        assert_eq!(
            session.check_receive(&fresh),
            Err(Error::Consent(ReceiveError::WrongStage))
        );
        assert!(session.check_receive(&grant).is_err());
        if !saving {
            session.verification_passed(&fresh).unwrap();
        }
        session.save_committed(&fresh).unwrap();
        assert_eq!(session.state(), State::Completed);
        assert!(session.check_receive(&fresh).is_err());
    }
}
