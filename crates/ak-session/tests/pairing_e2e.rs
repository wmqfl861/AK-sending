//! End-to-end pairing tests (wave 21, A6): the real v2 registry server
//! (pinned TLS 1.3 + authoritative SQLite store, `server-persistence`
//! feature) against two in-process devices, each with its own
//! [`RegistryClient`], [`PairingOrchestrator`], pairing journal and trust
//! book. Every hop — claim, redeem, offer, challenge, proofs, prepared,
//! commit, applied, ready — is a real frame over a real TLS connection,
//! with real Ed25519 signatures and real keyed-BLAKE3 MACs verified on
//! both endpoints and (structurally) by the server.
//!
//! Frozen-server quirks these tests accommodate:
//!
//! * an unknown / revoked / expired redeem target is answered
//!   `InviteRedeemPending` first and the unified `PAIR_UNAVAILABLE` only
//!   at the redeem deadline (no existence oracle) — the orchestrator's
//!   `redeem_window_ms` is lowered to ~1.5 s so the negative tests stay
//!   fast;
//! * the minimum non-zero invite TTL is 60 s, so the "expired code" case
//!   is produced by a second real SQLite connection over the same
//!   database file calling the store's lazy expiry with a future `now`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairTranscript, PairingRecord, PairingVerifyError,
    VerifyExpectations,
};
use ak_identity::{DeviceKeyPair, Fingerprint, PeerBook};
use ak_session::pairing::{
    redeem_request_mac, ClientError, OrchestratorConfig, PairFlowError, PairingOrchestrator,
    RealTransport, RegistryClient, BOOK_NAME, JOURNAL_SUBDIR,
};
use ak_session::runtime::{
    FakeTransferExecutor, RealClock, RuntimeConfig, SchedulerPolicy, SessionEvent, SessionRuntime,
    ShutdownMode,
};
use ak_signal::protocol_v2::message::PAIR_UNAVAILABLE;
use ak_signal::protocol_v2::{RedeemRequest, StateFilter, CURSOR_START};
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::Limits;
use ak_store::{PairingJournal, PairingPhase};
use tokio::net::TcpListener;
use tokio::sync::watch;

// ---- harness --------------------------------------------------------------

fn init_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .try_init();
}

struct TestRegistry {
    #[allow(dead_code)]
    addr: std::net::SocketAddr,
    registry_id: [u8; 32],
    shutdown: watch::Sender<bool>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn spawn_registry(db: &Path) -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind v2 port");
    let addr = listener.local_addr().expect("local addr");
    let store = if db.as_os_str() == ":memory:" {
        RegistryStore::open_in_memory()
    } else {
        RegistryStore::open(db)
    }
    .expect("open registry store");
    let (shutdown, shutdown_rx) = watch::channel(false);
    let key = registry_key.clone();
    tokio::spawn(async move {
        let _ = serve_v2(listener, &key, store, Limits::default(), shutdown_rx).await;
    });
    TestRegistry {
        addr,
        registry_id: registry_key.public_key(),
        shutdown,
    }
}

struct Device {
    #[allow(dead_code)]
    name: String,
    key: DeviceKeyPair,
    client: RegistryClient,
    orch: PairingOrchestrator,
    dir: Arc<tempfile::TempDir>,
}

impl Device {
    fn journal_path(&self) -> PathBuf {
        self.dir.path().join(JOURNAL_SUBDIR)
    }

    fn book_path(&self) -> PathBuf {
        self.dir.path().join(BOOK_NAME)
    }

    fn journal_phases(&self) -> Vec<([u8; 16], PairingPhase)> {
        PairingJournal::open(&self.journal_path())
            .expect("open journal")
            .load_all()
            .expect("load journal")
            .into_iter()
            .map(|record| (record.pairing_id, record.phase))
            .collect()
    }
}

async fn connect_device(
    registry: &TestRegistry,
    name: &str,
    key: DeviceKeyPair,
    dir: Arc<tempfile::TempDir>,
    config: OrchestratorConfig,
) -> Device {
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, name)
        .await
        .expect("device connects to the real registry");
    assert!(client.is_connected());
    let orch =
        PairingOrchestrator::open(client.clone(), key.clone(), dir.path(), config).expect("open");
    Device {
        name: name.to_string(),
        key,
        client,
        orch,
        dir,
    }
}

async fn device(registry: &TestRegistry, name: &str) -> Device {
    connect_device(
        registry,
        name,
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().expect("tempdir")),
        fast_config(),
    )
    .await
}

/// Tunables that keep the flows quick while staying inside the frozen
/// server windows.
fn fast_config() -> OrchestratorConfig {
    OrchestratorConfig {
        redeem_window_ms: 8_000,
        ready_wait_ms: 20_000,
        claim_conflict_retries: 8,
    }
}

fn deadline_config(redeem_window_ms: u64) -> OrchestratorConfig {
    OrchestratorConfig {
        redeem_window_ms,
        ..fast_config()
    }
}

/// Poll `cond` until it holds or `ms` elapse (asserts on the last try).
async fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if cond() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "condition not met within {ms} ms"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Both sides of one completed pairing: books contain each other
/// byte-for-byte and journals are `applied` for this transaction.
async fn assert_fully_paired(a: &Device, b: &Device, pairing_id: [u8; 16]) {
    // Journals applied on both sides (a device may hold more pairings).
    wait_until(15_000, || {
        let applied = |phases: &Vec<([u8; 16], PairingPhase)>| {
            phases.contains(&(pairing_id, PairingPhase::Applied))
        };
        applied(&a.journal_phases()) && applied(&b.journal_phases())
    })
    .await;

    // Trust books cross-record the fingerprints (byte-level check: the
    // fingerprint hex appears in the file itself).
    let b_hex = Fingerprint::from_public_key(&b.key.public_key()).to_hex();
    let a_hex = Fingerprint::from_public_key(&a.key.public_key()).to_hex();
    let a_book = std::fs::read_to_string(a.book_path()).expect("a book exists");
    let b_book = std::fs::read_to_string(b.book_path()).expect("b book exists");
    assert!(a_book.contains(&b_hex), "a's book must record b: {a_book}");
    assert!(b_book.contains(&a_hex), "b's book must record a: {b_book}");
    assert!(a_book.starts_with("AKPEERS1\n"));
    assert!(b_book.starts_with("AKPEERS1\n"));
    let a_loaded = PeerBook::load(&a.book_path()).expect("a book parses");
    let b_loaded = PeerBook::load(&b.book_path()).expect("b book parses");
    // A device may hold several pairings; this one must be resolvable and
    // byte-identical on both sides.
    assert!(
        a_loaded
            .get(&Fingerprint::from_public_key(&b.key.public_key()))
            .is_some(),
        "a's book resolves b's fingerprint"
    );
    assert!(
        b_loaded
            .get(&Fingerprint::from_public_key(&a.key.public_key()))
            .is_some(),
        "b's book resolves a's fingerprint"
    );
}

// ---- 1: the full owner/redeemer flow ---------------------------------------

#[tokio::test]
async fn full_flow_paired_books_and_applied_journals() {
    init_logging();
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let b = device(&registry, "bob").await;

    let invite = a.orch.create_invite(0).await.expect("claim invite");
    assert!(invite.display.starts_with("AKI1-"));
    assert_eq!(invite.expires_ms, 0, "ttl 0 is long-lived");
    a.orch.spawn_owner_loop();

    let verified = b
        .orch
        .redeem(&invite.display)
        .await
        .expect("redeem reaches PairReady");
    let pairing_id = verified.pairing_id();
    assert_eq!(verified.lookup_digest(), invite.lookup_digest);
    assert_eq!(
        verified.redeemer_device_id(),
        b.key.public_key(),
        "the transcript names the real redeemer key"
    );
    assert_eq!(verified.owner_device_id(), a.key.public_key());

    assert_fully_paired(&a, &b, pairing_id).await;
}

// ---- 2: owner offline at redeem time ----------------------------------------

#[tokio::test]
async fn owner_offline_redeem_completes_after_catchup() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = connect_device(
        &registry,
        "alice",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(20_000),
    )
    .await;
    let b = connect_device(
        &registry,
        "bob",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(20_000),
    )
    .await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    // A goes offline before B redeems (connection loss, not a close: the
    // client must be able to come back).
    a.client.simulate_connection_loss().await;
    assert!(!a.client.is_connected());

    // B redeems into the void: the server defers the offer to A's next
    // Hello.
    let code = invite.display.clone();
    let b_orch = b.orch.clone();
    let redeem = tokio::spawn(async move { b_orch.redeem(&code).await });
    tokio::time::sleep(Duration::from_millis(700)).await;

    // A comes back: reconnect runs the opening exchange, the registry's
    // Hello catch-up re-pushes the pending offer, the owner loop answers.
    a.client.reconnect().await.expect("a reconnects");
    assert!(a.client.is_connected());
    a.orch.spawn_owner_loop();

    let verified = redeem.await.expect("task").expect("redeem completes");
    assert_fully_paired(&a, &b, verified.pairing_id()).await;
}

// ---- 3/4/5: unified PAIR_UNAVAILABLE ----------------------------------------

#[tokio::test]
async fn wrong_code_fails_with_unified_pair_unavailable() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let b = connect_device(
        &registry,
        "bob",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(1_500),
    )
    .await;

    // Parse-valid, never registered: same pending shape, unified failure
    // at the deadline.
    let ghost = ak_identity::invite::InviteCode::generate().to_display();
    let err = b
        .orch
        .redeem(&ghost)
        .await
        .expect_err("unknown code must fail");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(
        !b.book_path().exists(),
        "nothing may be recorded for a ghost redeem"
    );
    assert!(b.journal_phases().is_empty(), "no journal entry either");
}

#[tokio::test]
async fn revoked_code_fails_with_unified_pair_unavailable() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let b = connect_device(
        &registry,
        "bob",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(1_500),
    )
    .await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    let revoked = a.orch.revoke(invite.code_id).await.expect("revoke");
    assert_eq!(revoked.code_id, invite.code_id);
    assert!(revoked.revision > invite.revision);

    let err = b.orch.redeem(&invite.display).await.expect_err("revoked");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(!b.book_path().exists());
    assert!(b.journal_phases().is_empty());
}

#[tokio::test]
async fn expired_code_is_unified_through_the_real_store() {
    let db_dir = tempfile::tempdir().unwrap();
    let db = db_dir.path().join("registry.sqlite3");
    let registry = spawn_registry(&db).await;
    let a = device(&registry, "alice").await;
    let b = connect_device(
        &registry,
        "bob",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(1_500),
    )
    .await;

    // Minimum legal TTL is 60 s; force the lazy expiry with a future now
    // through a second real SQLite connection to the same database file.
    let invite = a.orch.create_invite(60).await.expect("claim");
    assert!(invite.expires_ms > 0);
    let store2 = RegistryStore::open(&db).expect("second store connection");
    let future_now = invite.expires_ms + 1;
    let report = store2.expire_due(future_now).expect("lazy expiry");
    assert!(
        !report.aborted.is_empty() || report.expired_codes >= 1,
        "the expiry sweep must touch the code"
    );
    assert!(
        store2
            .find_active_code(&invite.lookup_digest, future_now)
            .expect("store lookup")
            .is_none(),
        "expired code is no longer an active target"
    );

    // Through the live server the expired target is the same unified
    // answer at the deadline.
    let err = b.orch.redeem(&invite.display).await.expect_err("expired");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

// ---- 6: redeemer crash and restart recovery ---------------------------------

#[tokio::test]
async fn redeemer_crash_recovery_via_status_query() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = connect_device(
        &registry,
        "alice",
        DeviceKeyPair::generate(),
        Arc::new(tempfile::tempdir().unwrap()),
        deadline_config(30_000),
    )
    .await;
    let b_key = DeviceKeyPair::generate();
    let b_dir = Arc::new(tempfile::tempdir().unwrap());
    let b = connect_device(
        &registry,
        "bob",
        b_key.clone(),
        Arc::clone(&b_dir),
        deadline_config(30_000),
    )
    .await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    a.orch.spawn_owner_loop();

    // B drives to the verified commit and "crashes" before any local apply.
    let committed = b
        .orch
        .redeem_to_commit(&invite.display)
        .await
        .expect("redeem to commit");
    let pairing_id = committed.pairing_id;
    b.client.close().await;
    drop(b);

    // B restarts with the same identity and the same journal directory
    // (the Arc kept it alive across the "crash").
    let b2 = connect_device(&registry, "bob", b_key, b_dir, deadline_config(30_000)).await;
    let code = ak_identity::invite::InviteCode::parse(&invite.display).unwrap();
    let applied = b2.orch.recover(&[code]).await.expect("recovery");
    assert_eq!(applied, vec![pairing_id]);

    // A's still-running owner flow receives its PairReady once both sides
    // applied; both journals and books converge.
    wait_until(15_000, || {
        a.journal_phases() == vec![(pairing_id, PairingPhase::Applied)]
    })
    .await;
    assert_fully_paired(&a, &b2, pairing_id).await;
}

// ---- 7: wrong-auth-key attacker ----------------------------------------------

#[tokio::test]
async fn attacker_with_wrong_auth_key_never_lands_in_the_book() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let c = device(&registry, "mallory").await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    a.orch.spawn_owner_loop();

    // Mallory knows the lookup digest but not the secret, so every MAC he
    // produces is keyed wrong. The server does not check the request MAC
    // (endpoint material) and begins the transaction; the owner rejects it.
    let wrong_auth = ak_identity::invite::InviteCode::generate().auth_key();
    let request_id = random_id16();
    let nonce = random_nonce32();
    let deadline = ak_signal_test_now() + 3_000;
    let request_mac = redeem_request_mac(
        &wrong_auth,
        &registry.registry_id,
        &invite.lookup_digest,
        &c.key.public_key(),
        &request_id,
        &nonce,
        deadline,
    );
    let signature = c.key.sign(&ak_signal::server_v2::hello2::redeem_message(
        &registry.registry_id,
        &invite.lookup_digest,
        &c.key.public_key(),
        &request_id,
        &nonce,
        deadline,
        &request_mac,
    ));
    let ack = c
        .client
        .invite_redeem(RedeemRequest {
            request_id,
            lookup_digest: invite.lookup_digest,
            redeemer_nonce: nonce,
            deadline_ms: deadline,
            request_mac,
            signature,
        })
        .await
        .expect("the server accepts the signature (the MAC is endpoint-only)");
    let pairing_id = ack.pairing_id;

    // The owner aborts the transaction; Mallory observes the unified
    // pairing failure, never a challenge.
    let mut plane = c.client.subscribe_plane();
    let deadline_in = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline_in, plane.recv())
            .await
            .expect("frame within 10s")
            .expect("plane alive");
        if let ak_signal::protocol_v2::CtrlMessage::PairError { code, .. } = &frame {
            assert_eq!(*code, PAIR_UNAVAILABLE, "unified failure");
            break;
        }
        assert!(
            !matches!(
                frame,
                ak_signal::protocol_v2::CtrlMessage::PairChallenge { .. }
            ),
            "no challenge may reach a wrong-key attacker"
        );
    }
    // Nothing landed anywhere on the owner side.
    assert!(
        !a.book_path().exists(),
        "the attacker never reaches the trust book"
    );
    assert!(a.journal_phases().is_empty(), "no journal entry either");

    // And the pure transcript layer refuses wrong-key proofs: a record
    // MACed under Mallory's key over an otherwise valid transcript fails
    // verification with the invite-code mismatch.
    let transcript = PairTranscript {
        registry_id: registry.registry_id,
        code_id: invite.code_id,
        code_revision: invite.revision,
        pairing_id,
        request_id,
        lookup_digest: invite.lookup_digest,
        owner_device_id: a.key.public_key(),
        redeemer_device_id: c.key.public_key(),
        owner_nonce: random_nonce32(),
        redeemer_nonce: nonce,
        deadline_ms: deadline,
    };
    let forged = PairingRecord::build(transcript, &wrong_auth, &a.key, &c.key);
    let expectations = VerifyExpectations {
        // Alice's real key for this code:
        auth_key: &ak_identity::invite::InviteCode::parse(&invite.display)
            .unwrap()
            .auth_key(),
        expected_owner_nonce: &forged.transcript.owner_nonce,
        expected_redeemer_nonce: &nonce,
        now_unix_ms: ak_signal_test_now(),
    };
    assert_eq!(
        forged.verify_transcript(&expectations).unwrap_err(),
        PairingVerifyError::InviteCodeMismatch
    );
}

fn ak_signal_test_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---- 8: two redeemers, one long-lived code -----------------------------------

#[tokio::test]
async fn two_redeemers_of_one_long_lived_code_pair_independently() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let b = device(&registry, "bob").await;
    let c = device(&registry, "carol").await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    a.orch.spawn_owner_loop();

    let first = b.orch.redeem(&invite.display).await.expect("b pairs");
    let second = c.orch.redeem(&invite.display).await.expect("c pairs");
    assert_ne!(
        first.pairing_id(),
        second.pairing_id(),
        "each redeem is its own transaction"
    );

    wait_until(15_000, || a.journal_phases().len() == 2).await;
    assert_fully_paired(&a, &b, first.pairing_id()).await;
    assert_fully_paired(&a, &c, second.pairing_id()).await;

    // A's book holds exactly the two peers, byte-verified.
    let book_text = std::fs::read_to_string(a.book_path()).unwrap();
    assert!(book_text.contains(&Fingerprint::from_public_key(&b.key.public_key()).to_hex()));
    assert!(book_text.contains(&Fingerprint::from_public_key(&c.key.public_key()).to_hex()));
    assert_eq!(PeerBook::load(&a.book_path()).unwrap().iter().count(), 2);
}

// ---- 9: idempotent invite registration ----------------------------------------

#[tokio::test]
async fn create_invite_retry_with_same_request_id_yields_exactly_one_code() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;

    let request_id = random_id16();
    let first = a
        .orch
        .create_invite_with_request_id(request_id, 0)
        .await
        .expect("first claim");
    // A network-level retry of the same logical registration re-sends the
    // same request id and the same digest: the server replays its stored
    // answer instead of registering a second code.
    let second = a
        .orch
        .create_invite_with_request_id(request_id, 0)
        .await
        .expect("replayed claim");
    assert_eq!(first.code_id, second.code_id, "same server-side code");
    assert_eq!(first.display, second.display, "same shown code");

    let (_cursor, entries) = a
        .client
        .invite_list(CURSOR_START, 8, StateFilter::ACTIVE)
        .await
        .expect("list own invites");
    assert_eq!(entries.len(), 1, "exactly one registered code: {entries:?}");
}

// ---- 10: digest collision is CODE_CONFLICT -------------------------------------

#[tokio::test]
async fn duplicate_digest_registration_is_code_conflict() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    let err = a
        .client
        .invite_claim_with_id(random_id16(), invite.lookup_digest, 0)
        .await
        .expect_err("same digest cannot register twice");
    assert_eq!(
        err.pair_code(),
        Some(ak_signal::protocol_v2::message::CODE_CONFLICT)
    );
}

// ---- 11: recovery after completion is a no-op -----------------------------------

#[tokio::test]
async fn recovery_after_completion_is_idempotent() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let b = device(&registry, "bob").await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    a.orch.spawn_owner_loop();
    let verified = b.orch.redeem(&invite.display).await.expect("pair");
    assert_fully_paired(&a, &b, verified.pairing_id()).await;

    let a_applied = a.orch.recover(&[]).await.expect("a recovery");
    let b_applied = b.orch.recover(&[]).await.expect("b recovery");
    assert!(
        a_applied.is_empty(),
        "nothing left to recover: {a_applied:?}"
    );
    assert!(
        b_applied.is_empty(),
        "nothing left to recover: {b_applied:?}"
    );
    assert_eq!(
        a.journal_phases(),
        vec![(verified.pairing_id(), PairingPhase::Applied)]
    );
    assert_eq!(
        b.journal_phases(),
        vec![(verified.pairing_id(), PairingPhase::Applied)]
    );
}

// ---- 12: the runtime listener over the real transport -----------------------------

#[tokio::test]
async fn runtime_listener_receives_real_pairing_events() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;
    let b = device(&registry, "bob").await;

    // Dual runtimes: each wraps its device's client in the real adapter
    // and plugs it into the wave-20 injection seam.
    let a_runtime = spawn_runtime(&a);
    let b_runtime = spawn_runtime(&b);
    let mut a_events = a_runtime.subscribe();
    let mut b_events = b_runtime.subscribe();

    let invite = a.orch.create_invite(0).await.expect("claim");
    a.orch.spawn_owner_loop();
    let verified = b.orch.redeem(&invite.display).await.expect("pair");

    // Alice's runtime surfaces the real redeem offer for her code...
    let mut saw_offer = false;
    let mut saw_inbound = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline && !(saw_offer && saw_inbound) {
        let event = tokio::time::timeout_at(deadline, a_events.recv())
            .await
            .expect("event within 15s");
        match event {
            Ok(SessionEvent::RedeemOffered { code_id, .. }) => {
                assert_eq!(*code_id.as_bytes(), invite.code_id);
                saw_offer = true;
            }
            Ok(SessionEvent::InboundSubmitted { .. }) => saw_inbound = true,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                panic!("runtime event consumer lagged by {n}");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("stream closed"),
        }
    }
    assert!(saw_offer, "the listener saw the real InviteRedeemOffered");
    assert!(
        saw_inbound,
        "the listener submitted an inbound task on the real pairing"
    );

    // Bob's runtime saw the completed pairing too (the listener submits
    // an inbound task for the new relation).
    let mut b_saw_inbound = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !b_saw_inbound {
        let event = tokio::time::timeout_at(deadline, b_events.recv())
            .await
            .expect("b event within 10s");
        if let Ok(SessionEvent::InboundSubmitted { .. }) = event {
            b_saw_inbound = true;
        }
    }
    assert!(b_saw_inbound, "b's listener submitted the inbound task");

    let _ = verified;
    a_runtime
        .shutdown(ShutdownMode::Cancel)
        .await
        .expect("a down");
    b_runtime
        .shutdown(ShutdownMode::Cancel)
        .await
        .expect("b down");
}

fn spawn_runtime(device: &Device) -> ak_session::runtime::SessionHandle {
    let clock = RealClock::shared();
    let executor = Arc::new(FakeTransferExecutor::new(Arc::clone(&clock)));
    let transport = Arc::new(RealTransport::new(device.client.clone()));
    let config = RuntimeConfig::new(
        SchedulerPolicy::default(),
        clock,
        transport,
        executor,
        ak_session::scheduler::RegistryId::from_bytes([0u8; 32]),
    );
    SessionRuntime::start(config).expect("runtime starts")
}

// ---- 13: revoke pass-through semantics --------------------------------------------

#[tokio::test]
async fn revoke_twice_fails_closed() {
    let registry = spawn_registry(Path::new(":memory:")).await;
    let a = device(&registry, "alice").await;

    let invite = a.orch.create_invite(0).await.expect("claim");
    let revoked = a.orch.revoke(invite.code_id).await.expect("first revoke");
    assert_eq!(revoked.code_id, invite.code_id);
    assert!(revoked.revoked_ms > 0);

    // Revoking again with the stale revision fails closed.
    let err = a
        .orch
        .revoke(invite.code_id)
        .await
        .expect_err("double revoke");
    match &err {
        PairFlowError::Rejected { code } | PairFlowError::Unavailable { code } => {
            assert_ne!(*code, 0);
        }
        PairFlowError::Client(ClientError::Pair { code, .. }) => {
            assert_ne!(*code, 0);
        }
        other => panic!("expected a business error, got {other:?}"),
    }

    // Unknown codes are refused too.
    let err = a.orch.revoke([0x42; 16]).await.expect_err("unknown code");
    assert!(matches!(err, PairFlowError::Local(_)));
}
