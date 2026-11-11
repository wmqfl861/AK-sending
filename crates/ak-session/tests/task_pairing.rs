//! Task-pairing lifecycle tests (wave 28, v1 minimal pairing model) over
//! the REAL v2 registry server: one random code = one task with exactly
//! two sides, the endpoint blob rides the wave-28 frame extension both
//! ways, `max_redeems = 1` refuses a second join, and every task end maps
//! to exactly one `TaskDisconnected` event with its source (Local /
//! Network / ByPeer).
//!
//! Wave 29 (review 2026-11-06, P1-05): the complementary-direction rule
//! is enforced by the initiator's owner loop at the relayed offer —
//! BEFORE the server's irreversible commit — so a mismatched, endpoint-
//! less or malformed join is refused without consuming the single redeem
//! and without either side journaling or booking anything; the post-commit
//! `DirectionConflict` remains covered as defense in depth against a
//! gate-less initiator.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use ak_identity::DeviceKeyPair;
use ak_session::pairing::{
    ClientState, OrchestratorConfig, PairingOrchestrator, RegistryClient, TaskDirection,
    TaskDisconnectReason, TaskError, TaskEvent, TaskPairing, TaskPairingConfig,
};
use ak_signal::protocol_v2::message::REDEEM_EXHAUSTED;
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::Limits;
use tokio::net::TcpListener;
use tokio::sync::watch;

// ---- harness --------------------------------------------------------------

struct TestRegistry {
    addr: SocketAddr,
    registry_id: [u8; 32],
    shutdown: watch::Sender<bool>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn spawn_registry() -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind v2 port");
    let addr = listener.local_addr().expect("local addr");
    let store = RegistryStore::open_in_memory().expect("open registry store");
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

struct TaskDevice {
    pairing: TaskPairing,
    dir: tempfile::TempDir,
}

fn fast_config() -> TaskPairingConfig {
    TaskPairingConfig {
        ttl_secs: 600,
        probe: Default::default(),
    }
}

async fn task_device(registry: &TestRegistry, name: &str) -> TaskDevice {
    let dir = tempfile::tempdir().unwrap();
    let key = DeviceKeyPair::generate();
    let client = RegistryClient::connect(registry.addr, registry.registry_id, &key, name)
        .await
        .expect("connect");
    let orch = PairingOrchestrator::open(
        client,
        key,
        dir.path(),
        OrchestratorConfig {
            redeem_window_ms: 1_500,
            ready_wait_ms: 5_000,
            claim_conflict_retries: 8,
        },
    )
    .expect("open orchestrator");
    TaskDevice {
        pairing: TaskPairing::open(orch, fast_config()),
        dir,
    }
}

fn localhost(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// The next task event (fails the test on lag/close instead of hanging).
async fn next_event(events: &mut tokio::sync::broadcast::Receiver<TaskEvent>) -> TaskEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("event within the bound")
        .expect("event stream live")
}

// ---- lifecycle --------------------------------------------------------------

/// The full v1 model over the real registry: the initiator's code carries
/// its endpoint down to the joiner (`PairCommit` extension) and the
/// joiner's endpoint up to the initiator (`InviteRedeemOffered`
/// extension); both sides parse the peer endpoint with the direction rule
/// intact, and the delivered bytes are exactly what each side encoded.
#[tokio::test]
async fn task_pairing_delivers_endpoints_both_ways_over_the_real_registry() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let joiner = task_device(&registry, "bob").await;

    let owner_candidates = vec![localhost(52001), "192.168.1.9:52002".parse().unwrap()];
    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &owner_candidates)
        .await
        .expect("create task code");
    assert!(
        code.invite.expires_ms > 0,
        "short-TTL task codes carry an expiry"
    );
    owner.pairing.spawn_owner_loop();

    // The joiner mirrors the declared direction (the initiator sends).
    let joiner_candidates = vec![localhost(52011)];
    let paired = joiner
        .pairing
        .join_task_code(
            &code.invite.display,
            &joiner_candidates,
            TaskDirection::Recv,
        )
        .await
        .expect("join task code");
    assert_eq!(paired.peer.name, "alice");
    assert_eq!(paired.peer.candidates, owner_candidates);
    assert_eq!(paired.peer.direction, TaskDirection::Send);
    assert_eq!(paired.direction, TaskDirection::Recv);

    // The owner side: the task-aware owner loop parsed the joiner's blob
    // (off the relayed `InviteRedeemOffered`) and registered the live
    // task — a decode failure or direction conflict would have refused
    // the completion instead.
    let owner_done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let active = owner.pairing.active_tasks();
            if active.len() == 1 {
                break active;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the owner completes the task within the bound");
    assert_eq!(owner_done[0].direction, TaskDirection::Send);
    assert!(owner_done[0].pairing.is_some(), "pairing id known");

    // Wire byte-exactness: the blob the joiner decoded is exactly the
    // bytes the owner attached at claim time.
    assert_eq!(
        ak_session::pairing::encode_endpoint_blob(&paired.peer).unwrap(),
        code.endpoint_blob,
        "delivered endpoint bytes are the initiator's canonical blob"
    );
}

/// One code pairs once: after the pairing committed, a second join is
/// refused with the dedicated `REDEEM_EXHAUSTED` business error.
#[tokio::test]
async fn max_redeems_one_refuses_the_second_join() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let joiner = task_device(&registry, "bob").await;
    let stranger = task_device(&registry, "mallory").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();
    joiner
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52011)],
            TaskDirection::Recv,
        )
        .await
        .expect("first join pairs");

    // The pairing row is committed server-side; the single redeem budget
    // is exhausted. The second join is refused immediately.
    let second = stranger
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52021)],
            TaskDirection::Recv,
        )
        .await;
    match second {
        Err(TaskError::Flow(err)) => {
            let code_err = match &err {
                ak_session::pairing::PairFlowError::Rejected { code } => *code,
                ak_session::pairing::PairFlowError::Unavailable { code } => *code,
                other => panic!("expected the redeem-exhausted refusal, got {other:?}"),
            };
            assert_eq!(
                code_err, REDEEM_EXHAUSTED,
                "the dedicated code, got 0x{code_err:04x}"
            );
        }
        other => panic!("second join must be refused, got {other:?}"),
    }
}

/// Local pairing artefacts under one device's identity dir: everything in
/// the `pairing/` journal directory (`.akpair` entries and `.akdgm`
/// transcript pins) — empty means the device journaled nothing.
fn journal_artifacts(dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("pairing")) {
        for entry in entries.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    names
}

/// Whether the device's transactional trust book exists on disk.
fn has_trust_book(dir: &std::path::Path) -> bool {
    dir.join("peers.book").exists()
}

/// The P1-05 product flow (review 2026-11-06): the RECEIVER generates the
/// code and a cross-device SENDER joins it. The joiner's explicit
/// `my_direction` (the user's intent) is complementary, so the initiator's
/// pre-commit gate lets the pairing through and both sides register the
/// live task over the real registry.
#[tokio::test]
async fn receiver_generated_code_paired_by_a_cross_device_sender() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let joiner = task_device(&registry, "bob").await;

    let owner_candidates = vec![localhost(52001), "192.168.1.9:52002".parse().unwrap()];
    let code = owner
        .pairing
        .create_task_code(TaskDirection::Recv, &owner_candidates)
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();

    // The joiner is the sender: its own declaration, not a local default.
    let paired = joiner
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52011)],
            TaskDirection::Send,
        )
        .await
        .expect("the cross-device sender joins the receiver's code");
    assert_eq!(paired.direction, TaskDirection::Send);
    assert_eq!(paired.peer.direction, TaskDirection::Recv);
    assert_eq!(paired.peer.name, "alice");
    assert_eq!(paired.peer.candidates, owner_candidates);

    // The receiver's owner loop registered its side with the declared
    // direction intact.
    let owner_done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let active = owner.pairing.active_tasks();
            if active.len() == 1 && active[0].pairing.is_some() {
                break active;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the owner completes the task within the bound");
    assert_eq!(owner_done[0].direction, TaskDirection::Recv);
    assert_eq!(owner_done[0].code, code.invite.code_id);
}

/// A same-direction join is refused BEFORE the irreversible commit: the
/// initiator's pre-commit gate aborts the pending transaction, the joiner
/// observes the server's unified "pairing target unavailable" answer,
/// neither side journals or books anything, and — because an aborted
/// attempt does not consume `max_redeems = 1` — the very same code still
/// pairs with a correctly-directed join afterwards.
#[tokio::test]
async fn same_direction_join_is_refused_before_commit_and_keeps_the_code_usable() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let wrong = task_device(&registry, "bob").await;
    let right = task_device(&registry, "carol").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();

    // Both sides declare Send: the gate vetoes the offer, the owner flow
    // (and with it the server's commit) never starts.
    let refused = wrong
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52011)],
            TaskDirection::Send,
        )
        .await;
    match &refused {
        Err(TaskError::Flow(ak_session::pairing::PairFlowError::Unavailable { .. })) => {}
        other => panic!("pre-commit refusal must be the unified unavailable, got {other:?}"),
    }

    // Pre-commit means pre-local-writes: no journal entries (and no
    // transcript pins) and no trust book on EITHER side of the refused
    // attempt, and no live task registered for it.
    assert!(
        journal_artifacts(wrong.dir.path()).is_empty(),
        "the refused joiner journaled nothing"
    );
    assert!(
        !has_trust_book(wrong.dir.path()),
        "no trust book on the joiner"
    );
    assert!(
        journal_artifacts(owner.dir.path()).is_empty(),
        "the owner journaled nothing for the aborted attempt"
    );
    assert!(
        !has_trust_book(owner.dir.path()),
        "no trust book on the owner"
    );
    assert!(wrong.pairing.active_tasks().is_empty());
    assert!(
        owner.pairing.active_tasks()[0].pairing.is_none(),
        "the owner's task stays unpaired"
    );

    // The single redeem was NOT consumed: a complementary join of the
    // very same code succeeds (a consumed code would answer
    // REDEEM_EXHAUSTED).
    let paired = right
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52021)],
            TaskDirection::Recv,
        )
        .await
        .expect("the code must still pair with the correct direction");
    assert_eq!(paired.direction, TaskDirection::Recv);
    let owner_done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let active = owner.pairing.active_tasks();
            if active.len() == 1 && active[0].pairing.is_some() {
                break active;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the owner completes the task within the bound");
    assert_eq!(owner_done[0].direction, TaskDirection::Send);
}

/// A pre-wave-28 joiner (plain redeem, no endpoint blob) cannot pair a
/// task code: the gate refuses the offer pre-commit for exactly that
/// reason, and the code remains usable for a wave-29 join.
#[tokio::test]
async fn legacy_joiner_without_an_endpoint_blob_is_refused_before_commit() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let legacy = task_device(&registry, "bob").await;
    let modern = task_device(&registry, "carol").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();

    // The legacy path carries no endpoint extension section at all.
    let refused = legacy
        .pairing
        .orchestrator()
        .redeem(&code.invite.display)
        .await;
    match &refused {
        Err(ak_session::pairing::PairFlowError::Unavailable { .. }) => {}
        other => panic!("a blob-less task redeem must be refused, got {other:?}"),
    }
    assert!(
        journal_artifacts(legacy.dir.path()).is_empty(),
        "the refused legacy joiner journaled nothing"
    );
    assert!(!has_trust_book(legacy.dir.path()));

    // The abort did not consume the redeem budget.
    modern
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52021)],
            TaskDirection::Recv,
        )
        .await
        .expect("the code still pairs with a wave-29 joiner");
}

/// Defense in depth (kept from wave 28): against a gate-LESS initiator —
/// an owner loop running pre-wave-29 code — a same-direction join still
/// refuses at completion with the typed `DirectionConflict`, after the
/// pairing committed. This is the fallback the pre-commit gate makes
/// unreachable for wave-29 initiators, not the product path.
#[tokio::test]
async fn direction_conflict_against_a_gateless_owner_still_refuses_at_completion() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let joiner = task_device(&registry, "bob").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    // The plain orchestrator loop answers without the task-layer gate.
    owner.pairing.orchestrator().spawn_owner_loop();
    let err = joiner
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52011)],
            TaskDirection::Send,
        )
        .await
        .expect_err("both sides declaring Send must refuse");
    assert!(
        matches!(err, TaskError::DirectionConflict { .. }),
        "{err:?}"
    );
}

/// Disconnect ends the task locally: the code is revoked server-side (the
/// listing shows `Revoked`) and exactly one `TaskDisconnected{Local}`
/// event fires.
#[tokio::test]
async fn disconnect_revokes_the_code_and_emits_local() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let mut events = owner.pairing.subscribe();

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Recv, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.disconnect(code.invite.code_id).await.unwrap();
    assert_eq!(
        next_event(&mut events).await,
        TaskEvent::TaskDisconnected {
            code: code.invite.code_id,
            reason: TaskDisconnectReason::Local,
        }
    );
    // Second disconnect: no second event (the task already ended).
    owner.pairing.disconnect(code.invite.code_id).await.unwrap();

    // The registry answers the revoked code with the unified
    // PAIR_UNAVAILABLE at the (fast) redeem deadline.
    let joiner = task_device(&registry, "bob").await;
    let refused = joiner
        .pairing
        .join_task_code(
            &code.invite.display,
            &[localhost(52011)],
            TaskDirection::Send,
        )
        .await;
    match refused {
        Err(TaskError::Flow(ak_session::pairing::PairFlowError::Unavailable { .. })) => {}
        other => panic!("a revoked task code must be unavailable, got {other:?}"),
    }
}

/// Wave 30 (review 2026-11-07, P2-08): a registry CONTROL-connection loss
/// is NOT a task verdict. The data planes of paired tasks are independent
/// of the registry, so the control link dying ends nothing — the tasks
/// stay registered and the event stream stays silent. Only a real
/// data-plane verdict (this side's link loss, the peer's departure) or a
/// local action ends a task.
#[tokio::test]
async fn registry_connection_loss_does_not_end_live_tasks() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let mut events = owner.pairing.subscribe();

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");

    owner
        .pairing
        .orchestrator()
        .client()
        .simulate_connection_loss()
        .await;

    // No task end, no state loss: the control plane is not the data plane.
    let stale = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
    assert!(
        stale.is_err(),
        "a registry disconnect must not end the task, got {stale:?}"
    );
    assert_eq!(
        owner.pairing.active_tasks().len(),
        1,
        "the task stays live through a registry outage"
    );

    // A REAL data-plane link-loss report still ends it as Network.
    owner.pairing.report_connection_lost(code.invite.code_id);
    assert_eq!(
        next_event(&mut events).await,
        TaskEvent::TaskDisconnected {
            code: code.invite.code_id,
            reason: TaskDisconnectReason::Network,
        }
    );
}

/// Wave 30 (review 2026-11-07, R29-P2-01): the commit-time redeem-cap
/// refusal (0x2109 — CODE-scoped, request id all-zero) reaches the LOSING
/// business flow through the REAL RegistryClient and orchestrator. Of two
/// concurrent complementary joins of one task code, exactly one pairs;
/// the loser fails PROMPTLY with the dedicated `REDEEM_EXHAUSTED`
/// refusal, not by waiting out its commit deadline.
#[tokio::test]
async fn concurrent_joins_the_loser_fails_promptly_with_redeem_exhausted() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let bob = task_device(&registry, "bob").await;
    let carol = task_device(&registry, "carol").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();

    let bob_candidates = [localhost(52011)];
    let carol_candidates = [localhost(52021)];
    let bob_join =
        bob.pairing
            .join_task_code(&code.invite.display, &bob_candidates, TaskDirection::Recv);
    let carol_join =
        carol
            .pairing
            .join_task_code(&code.invite.display, &carol_candidates, TaskDirection::Recv);
    let started = std::time::Instant::now();
    let (bob_res, carol_res) = tokio::join!(bob_join, carol_join);

    let loser = match (bob_res, carol_res) {
        (Ok(_paired), Err(loser)) | (Err(loser), Ok(_paired)) => loser,
        (Ok(_), Ok(_)) => panic!("max_redeems = 1 must not pair two joins"),
        (Err(a), Err(b)) => panic!("exactly one join must win; both failed: {a:?} vs {b:?}"),
    };
    match &loser {
        TaskError::Flow(err) => {
            let code_err = match err {
                ak_session::pairing::PairFlowError::Rejected { code } => *code,
                ak_session::pairing::PairFlowError::Unavailable { code } => *code,
                other => panic!("expected the redeem-exhausted refusal, got {other:?}"),
            };
            assert_eq!(
                code_err, REDEEM_EXHAUSTED,
                "the dedicated refusal, got 0x{code_err:04x}"
            );
        }
        other => panic!("a flow failure was expected, got {other:?}"),
    }
    // PROMPT: the broken dispatch made the loser wait out its commit
    // deadline (the fast test window is 1.5 s + a 10 s grace, > 11 s).
    // The attributed refusal lands within the pairing round trips.
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(8),
        "the loser must fail promptly, took {elapsed:?}"
    );
}

/// A data-plane loss maps by source: a peer-initiated close is ByPeer, an
/// idle-timeout/link loss is Network.
#[tokio::test]
async fn data_plane_loss_maps_by_source() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let mut events = owner.pairing.subscribe();
    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");

    owner.pairing.report_peer_disconnect(code.invite.code_id);
    assert_eq!(
        next_event(&mut events).await,
        TaskEvent::TaskDisconnected {
            code: code.invite.code_id,
            reason: TaskDisconnectReason::ByPeer,
        }
    );

    // The task already ended: a late network report must not resurrect a
    // second event for it.
    owner.pairing.report_connection_lost(code.invite.code_id);
    let stale = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
    assert!(stale.is_err(), "one end event per task, got {stale:?}");

    // A fresh task maps an idle-timeout report to Network.
    let code2 = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52002)])
        .await
        .expect("second task code");
    owner.pairing.report_connection_lost(code2.invite.code_id);
    assert_eq!(
        next_event(&mut events).await,
        TaskEvent::TaskDisconnected {
            code: code2.invite.code_id,
            reason: TaskDisconnectReason::Network,
        }
    );
}

// The (kept-simple) sanity that the client state watcher drives the
// network mapping at all.
#[tokio::test]
async fn client_state_watcher_reports_disconnected() {
    let registry = spawn_registry().await;
    let device = task_device(&registry, "alice").await;
    let mut state = device.pairing.orchestrator().client().state_watcher();
    device
        .pairing
        .orchestrator()
        .client()
        .simulate_connection_loss()
        .await;
    while state.changed().await.is_ok() {
        if matches!(*state.borrow_and_update(), ClientState::Disconnected) {
            return;
        }
    }
    panic!("the watcher must observe Disconnected");
}
