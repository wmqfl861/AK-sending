//! TEST_PLAN T08（可行形态）：路径变更 —— 暂停正文，重新验证，失败不发。
//!
//! 层级声明：L2 环回形态。"地址族/地址不变但对端身份变化"的拒绝面已由
//! `t06_identity_swap` 覆盖；本文件覆盖**路径重评估**形态：发送端两次
//! connect 之间对端端口变化（接收端重绑新端口）。
//!
//! 如实声明：**QUIC 连接迁移（connection migration）不在本测试范围**——
//! 引擎在这里建立的是新连接；"路径重评估"体现为：旧地址的有界拒绝（暂停
//! 正文、失败不发、无任何 relay/回退）＋ 新地址上的完整重新钉扎验证。
//!
//! 链条断言：
//! 1. 第一次传输中途接收端被杀（同 T10 手法）：part/checkpoint 存活，
//!    发送端任务记录 job_id = J；
//! 2. 发送端用**旧地址**重试：端口已死 → 有界失败（拒绝判定），
//!    resume 状态不受影响；
//! 3. 接收端以**同一设备身份**（同密钥，同指纹）重绑**新端口**；发送端
//!    用新地址 connect：TLS 重新钉扎通过 → 传输完成，job_id 仍为 J
//!    （同源内容身份）、resumed_chunks >= 1 —— 证明
//!    "路径变化 → 重新验证 → 续传"链条。
#![forbid(unsafe_code)]

mod common;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_engine::{receive_with_addr, send, EngineError, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::tls::make_self_signed;
use ak_identity::DeviceKeyPair;
use ak_store::{CheckpointStore, FileCheckpointStore};

use common::{assert_no_part_files, assert_trees_identical, write_file, PART_SUFFIX};

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Spawn one engine receiver on an ephemeral loopback port and report the
/// address it actually bound.
async fn spawn_receiver(
    spec: ReceiveSpec,
) -> (
    tokio::task::JoinHandle<Result<Report, EngineError>>,
    SocketAddr,
) {
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(receive_with_addr(spec, &|_| {}, addr_tx));
    let addr = addr_rx.await.expect("receiver must report its address");
    (handle, addr)
}

#[tokio::test]
async fn port_change_reconnect_rechecks_path_and_resumes_the_same_job() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t08-path-src");
    // 48 chunks of 1 KiB: killable mid-file with a durable verified prefix.
    write_file(
        &src.join("path.bin"),
        &common::Lcg::new(0x0800_0000_0000_0001).bytes(48 * 1024),
    );
    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    let part_path = dst.path().join(format!("path.bin{}", PART_SUFFIX));

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&sender_key);
    // One receiver device identity across both legs (keys survive rebind).
    let receiver_bundle = make_self_signed(&receiver_key);

    let mut spec = SendSpec::new(
        src.clone(),
        loopback(0), // placeholder; replaced by the receiver's address below
        receiver_key.fingerprint(),
        sender_bundle,
    );
    spec.bind = loopback(0);
    spec.chunk_size = NonZeroU32::new(1024).unwrap();
    spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let recv_ckpt = tempfile::tempdir().unwrap();
    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    // ---- Leg 1: transfer, then kill the receiver mid-flight ----------------
    let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
    let progress = Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv_spec, &*progress, addr_tx).await })
    };
    let addr1 = addr_rx.await.expect("leg-1 receiver bound");
    spec.peer = addr1;
    let retry_spec = spec.clone();
    let sender = tokio::spawn(async move { send(spec, &|_| {}).await });
    while *chunks_rx.borrow() < 4 {
        chunks_rx.changed().await.unwrap();
    }
    receiver.abort();
    let _ = receiver.await; // cancelled: its UDP socket is closed, port freed
    let leg1 = sender.await.unwrap();
    assert!(
        matches!(
            leg1,
            Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
        ),
        "leg-1 sender must fail closed without fallback, got {leg1:?}"
    );
    assert!(part_path.exists(), "part file survives the disconnect");

    // The task identity leg 1 recorded (deterministic from the source tree).
    let store = FileCheckpointStore::open(send_ckpt.path()).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 1, "exactly one send-job record after leg 1");
    let job_id = records[0].job_id;

    // ---- Stale address: the old port is dead; refuse, do not fall back -----
    // Honest scope note: on loopback a dead UDP port answers nothing, so the
    // refusal decision is bounded by the idle budget (2s here). This is the
    // "pause and fail closed" form — no relay, no alternate path, no silent
    // retry elsewhere; and the resume state on disk is untouched.
    let mut stale = retry_spec.clone();
    stale.peer = addr1;
    stale.idle_timeout = Duration::from_secs(2);
    let started = Instant::now();
    let refused = send(stale, &|_| {}).await;
    let refused_elapsed = started.elapsed();
    assert!(
        matches!(
            refused,
            Err(EngineError::Connect(_)) | Err(EngineError::IdleTimeout(_))
        ),
        "stale address must be refused, got {refused:?}"
    );
    assert!(
        refused_elapsed < Duration::from_secs(15),
        "refusal must be bounded, took {refused_elapsed:?}"
    );
    assert!(
        part_path.exists(),
        "the refused attempt must not disturb the resume state"
    );

    // ---- Leg 2: the SAME receiver identity re-binds a NEW port -------------
    let leg2_ckpt = tempfile::tempdir().unwrap();
    let mut addr2 = None;
    let mut leg2_receiver = None;
    for _ in 0..4 {
        let mut recv2 = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
        recv2.bind = loopback(0);
        recv2.chunk_size = retry_spec.chunk_size;
        recv2.checkpoint_dir = leg2_ckpt.path().to_path_buf();
        let (handle, addr) = spawn_receiver(recv2).await;
        if addr.port() == addr1.port() {
            // Rebinding the identical port would weaken the "path changed"
            // claim; roll again (astronomically rare on the ephemeral range).
            handle.abort();
            let _ = handle.await;
            continue;
        }
        addr2 = Some(addr);
        leg2_receiver = Some(handle);
        break;
    }
    let addr2 = addr2.expect("receiver rebound on a fresh port");
    let leg2_receiver = leg2_receiver.expect("leg-2 receiver handle");
    assert_ne!(addr2, addr1, "the peer address (port) must actually change");

    let mut retry_spec = retry_spec;
    retry_spec.peer = addr2;
    let report = send(retry_spec, &|_| {})
        .await
        .expect("new-address send must complete after re-verification");
    let recv_report = leg2_receiver
        .await
        .expect("leg-2 receiver task must not panic")
        .expect("new-address receive must complete");

    // Same source identity across the path change, and resume really happened.
    assert_eq!(
        report.job_id, job_id,
        "the reconnected transfer must keep the original job identity"
    );
    assert_eq!(recv_report.job_id, job_id);
    assert!(
        report.resumed_chunks >= 1,
        "second transfer must resume, got {}",
        report.resumed_chunks
    );
    assert!(
        recv_report.resumed_chunks >= 1,
        "receiver must report its verified prefix, got {}",
        recv_report.resumed_chunks
    );
    assert_eq!(
        report.peer_fingerprint.as_deref(),
        Some(receiver_key.fingerprint().to_hex().as_str()),
        "the new path must still be pinned to the SAME device fingerprint"
    );
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
    assert!(!part_path.exists(), "commit renames the part file away");

    // The one job record now reads completed — strictly after verification.
    let final_records = FileCheckpointStore::open(send_ckpt.path())
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(final_records.len(), 1);
    assert_eq!(final_records[0].job_id, job_id);
    assert_eq!(final_records[0].state, "completed");
}
