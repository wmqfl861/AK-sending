//! TEST_PLAN T07（环回 L2 形态）：连接已建立后停协调服务 —— 既有直连继续。
//!
//! 层级声明与如实边界：本文件是**单机环回**测试。本机没有真实 NAT、
//! 没有真实公网协调服务，因此这里证明的不是"跨网段断服后继续传"，而是
//! 架构不变量本身：**数据面（QUIC 直连 + 引擎协议）不依赖协调面存活**。
//! 信令服务器只参与握手期的发现/候选交换；一旦引擎拿到候选地址，协调面
//! 生死与传输无关。TEST_PLAN 中"不能推断后续重绑定无需协调"的限定依然
//! 成立——本文件不做任何重绑定断言。
//!
//! 两个场景：
//! 1. `coordination_down_before_transfer_leaves_direct_path_unaffected`：
//!    真实信令服务器（`ak_signal::server::run`，随机端口 + watch
//!    shutdown）上两个 `SignalClient` 完成 Hello/Register/ConnectRequest/
//!    SessionOpened/候选与打洞时序交换（证明协调面确实完整工作过，且
//!    引擎使用的对端地址正是信令交换来的候选）；随后**先关停信令服务
//!    器**（watch shutdown、新客户端无法完成 Hello/Welcome、双端连接被
//!    切断），再执行引擎级 send+receive，断言传输照常完成、字节一致。
//! 2. `coordination_down_mid_transfer_still_completes`：传输进行中
//!    （已有大量 chunk 落地后）关停信令服务器，传输仍完成——且关停后
//!    仍有新的 chunk 事件，证明数据在协调面死亡后仍在流动。
//!
//! 与 `t04_signal_irrelevant_to_data.rs` 构成证明对：协调面可有可无，
//! 从未处于数据路径（ADR-001：服务器不传文件）。
#![forbid(unsafe_code)]

mod common;

use std::net::{SocketAddr, TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ak_engine::{receive_with_addr, send, EngineError, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::DeviceKeyPair;
use ak_signal::client::SignalClient;
use ak_signal::client::SignalError;
use ak_signal::codec::{CtrlMessage, PeerSignal};
use ak_signal::server::{self, Config, Limits};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, watch};

use common::{assert_no_part_files, assert_trees_identical, write_file, Lcg};

/// Read one server message or fail (bounded wait), like ak-signal's own tests.
async fn next_msg(client: &SignalClient) -> CtrlMessage {
    tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("timed out waiting for a signaling message")
        .expect("signaling connection closed unexpectedly")
}

/// Next relayed peer signal inside `session`, skipping the server's
/// SameEgress hint (it is a server observation, not a peer signal).
async fn next_peer_signal_fn(client: &SignalClient, session: [u8; 16]) -> PeerSignal {
    loop {
        match next_msg(client).await {
            CtrlMessage::Signal { session: s, signal } => {
                assert_eq!(s, session);
                if matches!(signal, PeerSignal::SameEgress) {
                    continue;
                }
                return signal;
            }
            other => panic!("expected Signal, got {other:?}"),
        }
    }
}

/// A real in-process signaling server on ephemeral loopback ports.
struct SignalingPlane {
    tcp: SocketAddr,
    shutdown: watch::Sender<bool>,
    join: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// Reserve then release an ephemeral loopback port. The release-to-rebind race
/// is real but tiny; `start_signaling_plane` retries with fresh ports when the
/// rebind loses, and probes readiness with an actual TCP connect.
fn free_loopback_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .expect("reserve tcp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

fn free_loopback_udp_port() -> u16 {
    StdUdpSocket::bind(("127.0.0.1", 0))
        .expect("reserve udp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

async fn start_signaling_plane() -> SignalingPlane {
    for _ in 0..16 {
        let config = Config {
            tcp: SocketAddr::from(([127, 0, 0, 1], free_loopback_port())),
            stun_udp: SocketAddr::from(([127, 0, 0, 1], free_loopback_udp_port())),
            limits: Limits::default(),
        };
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tcp = config.tcp;
        let join = tokio::spawn(server::run(config, shutdown_rx));
        // Readiness = a real TCP connect succeeds. `run` binds TCP then UDP
        // before `serve` starts accepting, so an accepted probe implies both
        // sockets are up. A finished task means the rebind lost the port race;
        // detach it and try fresh ports (abort on a finished task is a no-op).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if join.is_finished() {
                break;
            }
            if TcpStream::connect(tcp).await.is_ok() && !join.is_finished() {
                return SignalingPlane {
                    tcp,
                    shutdown,
                    join,
                };
            }
            if Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        join.abort();
    }
    panic!("could not start the signaling server on an ephemeral loopback port");
}

impl SignalingPlane {
    /// Shut the whole coordination plane down and prove it is dead: the task
    /// must return cleanly and the TCP endpoint must refuse new connections.
    async fn stop(self) {
        self.shutdown
            .send(true)
            .expect("signaling server task still alive");
        let result = self.join.await.expect("signaling server task panicked");
        assert!(
            result.is_ok(),
            "signaling server must shut down cleanly: {result:?}"
        );
        assert_coordination_dead(self.tcp).await;
    }
}

/// Death predicate for the coordination plane: a fresh client must NOT be
/// able to complete a full Hello/Welcome handshake against it. (On this
/// Windows host, a plain TCP connect to a just-closed loopback port can
/// silently time out instead of getting an RST, so "refused" cannot be the
/// only accepted evidence of death; a working server would answer Welcome.)
async fn assert_coordination_dead(tcp: SocketAddr) {
    let probe_key = DeviceKeyPair::generate();
    match tokio::time::timeout(
        Duration::from_secs(3),
        SignalClient::connect(tcp, &probe_key, "itests-t07-death-probe"),
    )
    .await
    {
        // Timeout, refused, reset, EOF: no coordination service answering.
        Err(_) | Ok(Err(_)) => {}
        Ok(Ok(client)) => {
            let _ = client.close().await;
            panic!("coordination endpoint {tcp} still completes Hello/Welcome");
        }
    }
}

/// The client must observe its coordination connection being severed. Any
/// messages still buffered from before the shutdown are drained first.
async fn assert_client_severed(client: &SignalClient) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.next().await {
                Err(SignalError::Closed) => return,
                Ok(_) => continue,
                Err(other) => panic!("expected Closed, got {other}"),
            }
        }
    })
    .await
    .expect("client must observe the severed connection in time");
}

/// Result of a complete coordination-plane handshake: both live clients plus
/// the candidate each side learned through the Signal relay.
struct Handshake {
    sender: SignalClient,
    receiver: SignalClient,
    candidate_seen_by_sender: SocketAddr,
    candidate_seen_by_receiver: SocketAddr,
}

/// Drive the full coordination flow between two real clients on a real server:
/// Hello/Welcome (inside `SignalClient::connect`), Register with candidates,
/// ConnectRequest -> SessionOpened on both sides, then candidate exchange and
/// punch-timing exchange through the Signal relay. This is the "coordination
/// plane genuinely worked" premise of both T07 scenarios.
async fn full_signaling_handshake(
    server_tcp: SocketAddr,
    sender_key: &DeviceKeyPair,
    receiver_key: &DeviceKeyPair,
    receiver_candidate: SocketAddr,
    sender_candidate: SocketAddr,
) -> Handshake {
    let sender = SignalClient::connect(server_tcp, sender_key, "itests-t07-sender")
        .await
        .expect("sender Hello/Welcome");
    let receiver = SignalClient::connect(server_tcp, receiver_key, "itests-t07-receiver")
        .await
        .expect("receiver Hello/Welcome");

    sender
        .register(&[sender_candidate])
        .await
        .expect("sender Register");
    receiver
        .register(&[receiver_candidate])
        .await
        .expect("receiver Register");

    sender
        .connect_peer(receiver.device_id())
        .await
        .expect("ConnectRequest");
    let session = match next_msg(&sender).await {
        CtrlMessage::SessionOpened { session, peer } => {
            assert_eq!(peer, receiver.device_id());
            session
        }
        other => panic!("expected SessionOpened for sender, got {other:?}"),
    };
    match next_msg(&receiver).await {
        CtrlMessage::SessionOpened { session: s, peer } => {
            assert_eq!(s, session, "both endpoints must see the same session");
            assert_eq!(peer, sender.device_id());
        }
        other => panic!("expected SessionOpened for receiver, got {other:?}"),
    }

    // Candidates flow both ways through the relay. The server may inject its
    // SameEgress hint first (both clients share one loopback TCP peer IP, and
    // the server correctly notices); it is a hint, not a relayed peer signal,
    // so skip it while waiting for the candidates.
    receiver
        .send_signal(session, PeerSignal::Candidates(vec![receiver_candidate]))
        .await
        .expect("receiver candidates");
    sender
        .send_signal(session, PeerSignal::Candidates(vec![sender_candidate]))
        .await
        .expect("sender candidates");
    let candidate_seen_by_sender = match next_peer_signal_fn(&sender, session).await {
        PeerSignal::Candidates(mut list) => list.remove(0),
        other => panic!("expected Candidates, got {other:?}"),
    };
    let candidate_seen_by_receiver = match next_peer_signal_fn(&receiver, session).await {
        PeerSignal::Candidates(mut list) => list.remove(0),
        other => panic!("expected Candidates, got {other:?}"),
    };

    // Punch timing flows receiver -> sender (proves the timing channel too).
    let punch = PeerSignal::PunchAt {
        epoch_unix_ms: 1_772_000_000_000,
        nonce: [0x07; 8],
    };
    receiver
        .send_signal(session, punch.clone())
        .await
        .expect("PunchAt");
    match next_peer_signal_fn(&sender, session).await {
        signal @ PeerSignal::PunchAt { .. } => assert_eq!(signal, punch),
        other => panic!("expected PunchAt, got {other:?}"),
    }

    Handshake {
        sender,
        receiver,
        candidate_seen_by_sender,
        candidate_seen_by_receiver,
    }
}

/// One engine task (either side) plus a handle on the shared progress log.
struct EngineSide {
    join: tokio::task::JoinHandle<Result<Report, EngineError>>,
}

/// Shared progress log: both sides' callbacks push here.
type EventLog = Arc<Mutex<Vec<Progress>>>;

fn count_chunk_received(events: &EventLog) -> usize {
    events
        .lock()
        .map(|log| {
            log.iter()
                .filter(|event| matches!(event, Progress::ChunkReceived { .. }))
                .count()
        })
        .unwrap_or(0)
}

/// Wait until at least `want` ChunkReceived events show up (bounded).
async fn wait_for_chunk_events(events: &EventLog, want: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let count = count_chunk_received(events);
        if count >= want || Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn event_callback(events: EventLog) -> Arc<dyn Fn(Progress) + Send + Sync> {
    Arc::new(move |progress: Progress| {
        if let Ok(mut log) = events.lock() {
            log.push(progress);
        }
    })
}

/// Spawn the receiver engine first so its bound address (the candidate that
/// will be exchanged over signaling) exists before the handshake. Checkpoint
/// dirs are `.keep()`d (they outlive the tempdir guard by design here; the OS
/// temp cleaner eventually reclaims them).
async fn spawn_receiver(
    dst: &Path,
    identity: &DeviceKeyPair,
    chunk_kb: u32,
    events: EventLog,
) -> (SocketAddr, EngineSide) {
    let mut spec = ReceiveSpec::new(
        dst.to_path_buf(),
        ak_identity::tls::make_self_signed(identity),
    );
    spec.device_name = "itests-t07-recv".into();
    spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).expect("chunk_kb nonzero");
    spec.checkpoint_dir = tempfile::tempdir().unwrap().keep();

    let (addr_tx, addr_rx) = oneshot::channel();
    let callback = event_callback(events);
    let join = tokio::spawn(async move { receive_with_addr(spec, &*callback, addr_tx).await });
    let addr = addr_rx
        .await
        .expect("receiver must report its bound address");
    (addr, EngineSide { join })
}

/// Spawn the sender engine against the candidate learned over signaling: it
/// pins the receiver's fingerprint and presents the sender's own TLS identity.
async fn spawn_sender(
    src: &Path,
    peer: SocketAddr,
    sender_key: &DeviceKeyPair,
    receiver_key: &DeviceKeyPair,
    chunk_kb: u32,
    events: EventLog,
) -> EngineSide {
    let mut spec = SendSpec::new(
        src.to_path_buf(),
        peer,
        receiver_key.fingerprint(),
        ak_identity::tls::make_self_signed(sender_key),
    );
    spec.device_name = "itests-t07-send".into();
    spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).expect("chunk_kb nonzero");
    spec.checkpoint_dir = tempfile::tempdir().unwrap().keep();
    let callback = event_callback(events);
    EngineSide {
        join: tokio::spawn(async move { send(spec, &*callback).await }),
    }
}

impl EngineSide {
    async fn finish(self, budget_secs: u64) -> Result<Report, EngineError> {
        tokio::time::timeout(Duration::from_secs(budget_secs), self.join)
            .await
            .unwrap_or_else(|_| panic!("engine task did not finish within {budget_secs}s"))
            .expect("engine task panicked")
    }
}

fn build_small_tree(src: &Path) {
    let mut lcg = Lcg::new(0x7070_0000_0000_0001);
    write_file(
        &src.join("readme.txt"),
        b"t07: coordination plane dies, the direct path must not care",
    );
    write_file(&src.join("bin/payload.bin"), &lcg.bytes(256 * 1024));
    write_file(&src.join("nested/deep/data.bin"), &lcg.bytes(64 * 1024));
}

/// T07 main shape: coordination plane fully worked (handshake, candidates
/// exchanged), then is shut down BEFORE any file byte moves. The engine
/// transfer must still complete byte-identically on the candidate address
/// that arrived over signaling.
#[tokio::test]
async fn coordination_down_before_transfer_leaves_direct_path_unaffected() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t07-src");
    build_small_tree(&src);
    let dst = tempfile::tempdir().unwrap();

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();

    // Receiver engine is up and listening; its address is the candidate.
    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let (recv_addr, receiver) =
        spawn_receiver(dst.path(), &receiver_key, 16, Arc::clone(&events)).await;
    let sender_candidate = SocketAddr::from(([127, 0, 0, 1], free_loopback_port()));

    // Full coordination handshake against a REAL signaling server.
    let plane = start_signaling_plane().await;
    let handshake = full_signaling_handshake(
        plane.tcp,
        &sender_key,
        &receiver_key,
        recv_addr,
        sender_candidate,
    )
    .await;
    assert_eq!(
        handshake.candidate_seen_by_sender, recv_addr,
        "the data address must be the one exchanged over signaling"
    );
    assert_eq!(handshake.candidate_seen_by_receiver, sender_candidate);

    // Kill the coordination plane first, and prove it is dead: the endpoint
    // refuses connections and BOTH severed clients observe Closed.
    plane.stop().await;
    assert_client_severed(&handshake.sender).await;
    assert_client_severed(&handshake.receiver).await;
    drop(handshake);

    // Only now does the file transfer start, aimed at the candidate learned
    // from the (now dead) coordination plane.
    let sender = spawn_sender(&src, recv_addr, &sender_key, &receiver_key, 16, events).await;
    let send_report = sender
        .finish(120)
        .await
        .expect("send after coordination death");
    let recv_report = receiver
        .finish(120)
        .await
        .expect("receive after coordination death");

    // Report.files counts manifest entries (files + dirs).
    assert_eq!(send_report.files, common::snapshot_tree(&src).entry_count());
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(send_report.resumed_chunks, 0);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
}

/// T07 mid-transfer variant: chunks are already flowing when the coordination
/// plane is aborted. Data keeps flowing afterwards (more chunk events land
/// after the kill than before it) and the transfer completes byte-identically.
/// Small chunks over many files so the run is long enough to guarantee the
/// shutdown happens mid-flight.
#[tokio::test]
async fn coordination_down_mid_transfer_still_completes() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t07-mid-src");
    let mut lcg = Lcg::new(0x7070_0000_0000_0002);
    // 24 files x 2 MiB at 8 KiB chunks = 6144 chunks: comfortably longer
    // than the 256 chunk events we wait for before pulling the plug.
    for index in 0..24 {
        write_file(
            &src.join(format!("chunked/file-{index:02}.bin")),
            &lcg.bytes(2 * 1024 * 1024),
        );
    }
    let total_bytes: u64 = 24 * 2 * 1024 * 1024;
    let dst = tempfile::tempdir().unwrap();

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();

    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let (recv_addr, receiver) =
        spawn_receiver(dst.path(), &receiver_key, 8, Arc::clone(&events)).await;

    let plane = start_signaling_plane().await;
    let handshake = full_signaling_handshake(
        plane.tcp,
        &sender_key,
        &receiver_key,
        recv_addr,
        SocketAddr::from(([127, 0, 0, 1], free_loopback_port())),
    )
    .await;
    assert_eq!(handshake.candidate_seen_by_sender, recv_addr);
    drop(handshake);

    // Transfer starts while the coordination plane is still alive.
    let sender = spawn_sender(
        &src,
        recv_addr,
        &sender_key,
        &receiver_key,
        8,
        Arc::clone(&events),
    )
    .await;

    // Wait until a real prefix of the payload is on the wire, then pull the
    // plug on the coordination plane mid-transfer.
    let before_kill = wait_for_chunk_events(&events, 256, Duration::from_secs(60)).await;
    assert!(
        before_kill >= 256,
        "transfer must be mid-flight (got {before_kill} chunk events) before shutdown"
    );

    plane.stop().await;

    // Both sides must finish cleanly after the coordination plane died...
    let send_report = sender
        .finish(240)
        .await
        .expect("send survives coordination death");
    let recv_report = receiver
        .finish(240)
        .await
        .expect("receive survives coordination death");

    // ...and data must have kept flowing after the kill: every one of the
    // 6144 chunks lands, far more than the <= prefix seen at kill time.
    let chunks_final = count_chunk_received(&events);
    assert!(
        chunks_final > before_kill,
        "data must keep flowing after coordination death: {chunks_final} final vs {before_kill} at kill time"
    );

    // Report.files counts manifest entries (files + dirs).
    assert_eq!(send_report.files, common::snapshot_tree(&src).entry_count());
    assert_eq!(send_report.bytes, total_bytes);
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
}
