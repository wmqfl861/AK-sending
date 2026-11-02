//! Tests from outside the crate: only public APIs, no private-field shortcuts.
use ak_core::{job::State, receive::ReceiveJob};
use std::time::Duration;

#[test]
fn public_api_requires_receiver_consent_after_every_reconnect() {
    let peer = [2; 32];
    let mut j = ReceiveJob::new([1; 16], peer);
    j.begin().unwrap();
    j.direct_ready(peer).unwrap();
    assert_eq!(j.state(), State::AwaitingConsent);
    assert!(j.check_transfer().is_err());
    let old = j.consent_request().unwrap();
    j.approve(&old, Duration::from_secs(60)).unwrap();
    j.check_transfer().unwrap();
    j.connection_lost().unwrap();
    j.resume().unwrap();
    j.direct_ready(peer).unwrap();
    assert!(j.approve(&old, Duration::from_secs(60)).is_err());
    let current = j.consent_request().unwrap();
    j.approve(&current, Duration::from_secs(60)).unwrap();
    j.all_bytes_received().unwrap();
    j.verification_passed().unwrap();
    assert_eq!(j.state(), State::Saving);
    j.save_committed().unwrap();
    assert_eq!(j.state(), State::Completed);
}

#[test]
fn public_cancellation_and_revocation_cannot_leave_write_permission() {
    for cancel in [true, false] {
        let mut j = ReceiveJob::new([1; 16], [2; 32]);
        j.begin().unwrap();
        j.direct_ready([2; 32]).unwrap();
        let request = j.consent_request().unwrap();
        j.approve(&request, Duration::from_secs(60)).unwrap();
        if cancel {
            j.cancel().unwrap();
        } else {
            j.revoke().unwrap();
        }
        assert!(j.check_transfer().is_err());
        assert!(j.save_committed().is_err());
        assert!(j.approve(&request, Duration::from_secs(60)).is_err());
    }
}

#[test]
fn post_receive_phases_never_reopen_writes_after_reauthorization() {
    for saving in [false, true] {
        for interruption in 0..3 {
            let mut j = ReceiveJob::new([1; 16], [2; 32]);
            j.begin().unwrap();
            j.direct_ready([2; 32]).unwrap();
            j.approve(&j.consent_request().unwrap(), Duration::from_secs(60))
                .unwrap();
            j.all_bytes_received().unwrap();
            if saving {
                j.verification_passed().unwrap();
            }
            let stage = j.state();
            match interruption {
                0 => j.pause().unwrap(),
                1 => j.revoke().unwrap(),
                _ => j.connection_lost().unwrap(),
            }
            j.resume().unwrap();
            // A second pause while reconnecting must not erase retained phase.
            j.pause().unwrap();
            j.resume().unwrap();
            j.direct_ready([2; 32]).unwrap();
            j.approve(&j.consent_request().unwrap(), Duration::from_secs(60))
                .unwrap();
            assert_eq!(j.state(), stage);
            assert!(j.check_transfer().is_err());
            assert!(j.all_bytes_received().is_err());
            if !saving {
                j.verification_passed().unwrap();
            }
            j.save_committed().unwrap();
            assert_eq!(j.state(), State::Completed);
        }
    }
}
