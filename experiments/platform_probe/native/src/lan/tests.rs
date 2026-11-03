use super::*;
use std::time::Instant;

const LOCAL: u32 = 0x7f00_0001;
fn certificate() -> Vec<u8> {
    rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()])
        .unwrap()
        .cert
        .der()
        .to_vec()
}
fn control() -> Control {
    let der = certificate();
    Control {
        status: READY,
        own_id: PairedPeer::from_certificate_der(der.clone()).unwrap().id(),
        certificate: der,
        staged_peer: Vec::new(),
        config: None,
    }
}
fn staged() -> Control {
    let mut c = control();
    for (i, byte) in certificate().iter().enumerate() {
        assert_eq!(c.push(i as u32, u32::from(*byte)), 0);
    }
    c
}
fn release(r: &mut Registry, id: u64) -> i32 {
    let limit = Instant::now() + Duration::from_secs(4);
    loop {
        let result = r.release(id);
        if result != BUSY {
            return result;
        }
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn manual_address_scope_and_ports_are_strict() {
    for ip in [
        0,
        0x0808_0808,
        0xffff_ffff,
        0xe000_0001,
        0x7f00_0002,
        0xa9fe_0101,
    ] {
        assert!(address(ip, 42424).is_err());
    }
    for port in [0, 1023, 65536, u32::MAX] {
        assert!(address(LOCAL, port).is_err());
    }
    for ip in [LOCAL, 0x0a00_0001, 0xac10_0001, 0xac1f_fffe, 0xc0a8_0101] {
        for port in [1024, 42424, 65535] {
            assert!(address(ip, port).is_ok());
        }
    }
    for ip in [0xac0f_ffff, 0xac20_0000] {
        assert!(address(ip, 42424).is_err());
    }
}
#[test]
fn staging_is_ordered_bounded_and_non_mutating_on_error() {
    let mut c = control();
    assert_eq!(c.push(1, 1), BAD_INPUT);
    assert_eq!(c.push(0, 256), BAD_INPUT);
    for i in 0..CERT_LIMIT {
        assert_eq!(c.push(i as u32, 7), 0);
    }
    assert_eq!(c.push(CERT_LIMIT as u32, 7), BAD_INPUT);
    assert_eq!(c.staged_peer.len(), CERT_LIMIT);
}
#[test]
fn no_network_start_without_valid_immutable_pairing() {
    let local = address(LOCAL, 42001).unwrap();
    let mut c = control();
    assert_eq!(c.start(), BAD_INPUT);
    assert_eq!(c.configure(local, LOCAL, 42002, 0), BAD_INPUT);
    c.staged_peer = vec![0; 100];
    assert_eq!(c.configure(local, LOCAL, 42002, 0), BAD_INPUT);
    let mut c = staged();
    assert_eq!(c.configure(local, LOCAL, 42002, 2), BAD_INPUT);
    assert_eq!(c.configure(local, LOCAL, 42001, 0), BAD_INPUT);
    assert_eq!(c.configure(local, 0xc0a8_0101, 42002, 0), BAD_INPUT);
    assert_eq!(c.configure(local, LOCAL, 42002, 0), PAIRED);
    assert_eq!(c.push(0, 1), BAD_INPUT);
    assert_eq!(c.configure(local, LOCAL, 42003, 1), BAD_INPUT);
    assert_eq!(c.start(), START);
    assert_eq!(c.start(), BAD_INPUT);
}
#[test]
fn own_certificate_cannot_pair_with_itself() {
    let mut c = control();
    c.staged_peer = c.certificate.clone();
    assert_eq!(
        c.configure(address(LOCAL, 42001).unwrap(), LOCAL, 42002, 0),
        BAD_INPUT
    );
}
#[test]
fn read_exposes_only_bounded_public_bytes() {
    let mut c = staged();
    assert_eq!(Control::new().read(0, 0), BUSY);
    assert_eq!(c.read(0, u32::MAX), c.certificate.len() as i32);
    assert_eq!(c.read(1, u32::MAX), 32);
    assert_eq!(c.read(2, 0), BAD_INPUT);
    assert_eq!(c.read(1, 32), BAD_INPUT);
    assert_eq!(c.read(7, 0), BAD_INPUT);
    assert_eq!(
        c.configure(address(LOCAL, 42001).unwrap(), LOCAL, 42002, 0),
        PAIRED
    );
    assert_eq!(c.read(2, u32::MAX), 32);
}
#[test]
fn cancellation_and_success_are_terminal_and_cannot_be_reopened() {
    for phase in [
        PREPARING, READY, PAIRED, START, CONNECTING, ACTIVE, LISTENING,
    ] {
        let mut c = control();
        c.status = phase;
        assert_eq!(c.cancel(), CANCELLED);
        assert!(c.phase(ACTIVE).is_err());
        assert_eq!(c.start(), BAD_INPUT);
        assert_eq!(c.push(0, 1), BAD_INPUT);
        assert_eq!(c.cancel(), CANCELLED);
    }
    let mut c = control();
    c.status = PASSED;
    assert_eq!(c.cancel(), PASSED);
    assert!(c.phase(READY).is_err());
}
#[test]
fn task_slot_old_handles_and_release_before_worker_exit() {
    let mut r = Registry::new();
    let id = r.prepare(LOCAL, 42001) as u64;
    assert_eq!(r.prepare(LOCAL, 42002), i64::from(BUSY));
    assert_eq!(r.release(id), BUSY);
    assert_eq!(r.with(id + 1, |s, _| s.cancel()), INVALID);
    assert_eq!(r.with(id, |s, _| s.cancel()), CANCELLED);
    assert_eq!(release(&mut r, id), CANCELLED);
    assert_eq!(r.with(id, |s, _| s.status), INVALID);
    assert_eq!(r.release(id), INVALID);
    let next = r.prepare(LOCAL, 42001) as u64;
    assert!(next > id);
    assert_eq!(r.with(next, |s, _| s.cancel()), CANCELLED);
    assert_eq!(release(&mut r, next), CANCELLED);
}
#[test]
fn invalid_prepare_does_not_allocate_a_task_or_handle() {
    let mut r = Registry::new();
    assert_eq!(r.prepare(0, 42424), i64::from(BAD_INPUT));
    assert_eq!(r.next, 1);
    assert!(r.job.is_none());
    r.next = i64::MAX as u64 + 1;
    assert_eq!(r.prepare(LOCAL, 42424), i64::from(EXHAUSTED));
    assert!(r.job.is_none());
}
#[test]
fn explicit_expiry_without_start_never_succeeds() {
    let state = Arc::new(Mutex::new(Control::new()));
    assert_eq!(
        network::run_with_limit(
            address(LOCAL, 42001).unwrap(),
            state,
            Duration::from_millis(20)
        ),
        EXPIRED
    );
}
#[test]
fn ffi_panics_are_contained() {
    assert_eq!(ffi(|| panic!("fixture")), FAILED);
}
