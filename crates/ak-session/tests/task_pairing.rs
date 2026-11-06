//! Task-pairing lifecycle tests (wave 28, v1 minimal pairing model) over
//! the REAL v2 registry server: one random code = one task with exactly
//! two sides, the endpoint blob rides the wave-28 frame extension both
//! ways, `max_redeems = 1` refuses a second join, and every task end maps
//! to exactly one `TaskDisconnected` event with its source (Local /
//! Network / ByPeer).
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
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind v2 port");
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
    #[allow(dead_code)]
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
        .join_task_code(&code.invite.display, &joiner_candidates, TaskDirection::Recv)
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
        .join_task_code(&code.invite.display, &[localhost(52011)], TaskDirection::Recv)
        .await
        .expect("first join pairs");

    // The pairing row is committed server-side; the single redeem budget
    // is exhausted. The second join is refused immediately.
    let second = stranger
        .pairing
        .join_task_code(&code.invite.display, &[localhost(52021)], TaskDirection::Recv)
        .await;
    match second {
        Err(TaskError::Flow(err)) => {
            let code_err = match &err {
                ak_session::pairing::PairFlowError::Rejected { code } => *code,
                ak_session::pairing::PairFlowError::Unavailable { code } => *code,
                other => panic!("expected the redeem-exhausted refusal, got {other:?}"),
            };
            assert_eq!(code_err, REDEEM_EXHAUSTED, "the dedicated code, got 0x{code_err:04x}");
        }
        other => panic!("second join must be refused, got {other:?}"),
    }
}

/// A mismatched direction declaration (both sides claim to send) refuses
/// at completion — a task is exactly one sender and one receiver.
#[tokio::test]
async fn direction_conflict_refuses_the_pairing() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let joiner = task_device(&registry, "bob").await;

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    owner.pairing.spawn_owner_loop();
    let err = joiner
        .pairing
        .join_task_code(&code.invite.display, &[localhost(52011)], TaskDirection::Send)
        .await
        .expect_err("both sides declaring Send must refuse");
    assert!(matches!(err, TaskError::DirectionConflict { .. }), "{err:?}");
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
        .join_task_code(&code.invite.display, &[localhost(52011)], TaskDirection::Send)
        .await;
    match refused {
        Err(TaskError::Flow(ak_session::pairing::PairFlowError::Unavailable { .. })) => {}
        other => panic!("a revoked task code must be unavailable, got {other:?}"),
    }
}

/// A control-connection loss ends every live task as Network: the watcher
/// maps the client's `Disconnected` state.
#[tokio::test]
async fn network_loss_ends_the_task_as_network() {
    let registry = spawn_registry().await;
    let owner = task_device(&registry, "alice").await;
    let mut events = owner.pairing.subscribe();

    let code = owner
        .pairing
        .create_task_code(TaskDirection::Send, &[localhost(52001)])
        .await
        .expect("create task code");
    let watcher = owner.pairing.spawn_network_watcher();

    owner
        .pairing
        .orchestrator()
        .client()
        .simulate_connection_loss()
        .await;
    assert_eq!(
        next_event(&mut events).await,
        TaskEvent::TaskDisconnected {
            code: code.invite.code_id,
            reason: TaskDisconnectReason::Network,
        }
    );
    watcher.abort();
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
    assert!(
        stale.is_err(),
        "one end event per task, got {stale:?}"
    );

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
    let mut state = device
        .pairing
        .orchestrator()
        .client()
        .state_watcher();
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
