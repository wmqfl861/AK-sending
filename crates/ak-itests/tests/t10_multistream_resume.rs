//! TEST_PLAN T10（扩展）：断网 / 双端进程重启 —— 多流模式下的中断续传，
//! 以及多流＋多根组合。
//!
//! 层级声明：L2 环回形态。ak-engine 单测已覆盖**单流**"中断→checkpoint
//! 存活→续传"；本文件把同一语义链搬到 multistream 数据路径（每文件一条
//! QUIC 流），并叠加 v3 多根发送：
//!
//! 1. 多流传输中途 abort 接收任务（进度计数达阈值后，复用引擎测试的手法）：
//!    part/checkpoint 存活 → 第二次多流传输完成、resumed_chunks >= 1、
//!    字节一致、无残留；
//! 2. 多流 + 多根（两个目录 extra_roots）组合一次通过：逐根字节一致，
//!    两个根各自落在自己的顶层目录名下。
//!
//! 如实边界：这是进程内环回＋任务级 abort 的"进程重启"形态，不是真实双
//! 进程崩溃/断网注入（见 TEST_PLAN T10 行的未覆盖部分）。
#![forbid(unsafe_code)]

mod common;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;

use ak_engine::{receive_with_addr, send, EngineError, Progress, ReceiveSpec, SendSpec};
use ak_identity::tls::make_self_signed;
use ak_identity::DeviceKeyPair;
use ak_store::{CheckpointStore, FileCheckpointStore};

use common::{
    assert_no_part_files, assert_trees_identical, snapshot_tree, write_file, PART_SUFFIX,
};

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Four files, each 256 KiB (256 chunks of 1 KiB), so multistream rides four
/// parallel per-file streams. Kill the receiver once >= 8 chunks are durable;
/// parts and the sender checkpoint must survive, and the second multistream
/// run must RESUME (not restart), complete, and land byte-identical trees.
#[tokio::test]
async fn multistream_disconnect_keeps_parts_and_resumes_to_identical_bytes() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t10-ms-src");
    for index in 0..4u64 {
        let bytes = common::Lcg::new(0x1000_0000_0000_0000 + index).bytes(256 * 1024);
        write_file(&src.join(format!("stream-{index}.bin")), &bytes);
    }
    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&sender_key);
    // Same device identity across both legs: keys survive a "process restart".
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
    spec.multistream = true;

    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
    let progress = Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });

    // Leg 1: multistream transfer, then kill the receiver mid-flight.
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv_spec, &*progress, addr_tx).await })
    };
    spec.peer = addr_rx.await.expect("leg-1 receiver bound");
    let leg2_spec = spec.clone();
    let sender = tokio::spawn(async move { send(spec, &|_| {}).await });

    while *chunks_rx.borrow() < 8 {
        chunks_rx.changed().await.unwrap();
    }
    receiver.abort();
    let _ = receiver.await; // cancelled: socket closed, part handles released
    let leg1 = sender.await.unwrap();
    assert!(
        matches!(
            leg1,
            Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
        ),
        "leg-1 sender must fail closed without fallback, got {leg1:?}"
    );

    // Survivors: at least one part file, and exactly one sender job record.
    let tree = snapshot_tree(dst.path());
    let parts: Vec<&String> = tree
        .files
        .keys()
        .filter(|name| name.ends_with(PART_SUFFIX))
        .collect();
    assert!(
        !parts.is_empty(),
        "part files must survive the multistream disconnect, got {parts:?}"
    );
    let store = FileCheckpointStore::open(send_ckpt.path()).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 1, "one send-job checkpoint must survive");
    let job_id = records[0].job_id;

    // Leg 2: same device identities, fresh port, same destination => resume.
    let recv2_ckpt = tempfile::tempdir().unwrap();
    let mut recv2 = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv2.bind = loopback(0);
    recv2.chunk_size = leg2_spec.chunk_size;
    recv2.checkpoint_dir = recv2_ckpt.path().to_path_buf();
    let (addr2_tx, addr2_rx) = tokio::sync::oneshot::channel();
    let receiver2 = tokio::spawn(receive_with_addr(recv2, &|_| {}, addr2_tx));
    let mut leg2_spec = leg2_spec;
    leg2_spec.peer = addr2_rx.await.expect("leg-2 receiver bound");
    let report = send(leg2_spec, &|_| {})
        .await
        .expect("resumed multistream send completes");
    let recv_report = receiver2
        .await
        .expect("leg-2 receiver task must not panic")
        .expect("resumed multistream receive completes");

    assert_eq!(report.job_id, job_id, "resume keeps the job identity");
    assert_eq!(recv_report.job_id, job_id);
    assert!(
        report.resumed_chunks >= 1,
        "sender must skip verified chunks, got {}",
        report.resumed_chunks
    );
    assert!(
        recv_report.resumed_chunks >= 1,
        "receiver must report its verified prefix, got {}",
        recv_report.resumed_chunks
    );
    assert_eq!(report.files, recv_report.files);
    assert_eq!(report.bytes, recv_report.bytes);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
}

/// Multistream + multi-root in one pass: primary root `alpha` plus extra root
/// `beta` (two directories), files spread over both, every file on its own
/// stream. One transfer lands both trees byte-identical under their root
/// names, and the sender report lists both roots in order.
#[tokio::test]
async fn multistream_multi_root_two_directories_one_pass() {
    let base = tempfile::tempdir().unwrap();
    let alpha = base.path().join("alpha");
    let beta = base.path().join("beta");
    write_file(&alpha.join("a.txt"), b"alpha root file");
    write_file(
        &alpha.join("nested/deep/b.bin"),
        &common::Lcg::new(0x1000_0000_0000_0100).bytes(40 * 1024),
    );
    std::fs::create_dir_all(alpha.join("empty/dir")).unwrap();
    write_file(
        &beta.join("c.bin"),
        &common::Lcg::new(0x1000_0000_0000_0200).bytes(24 * 1024),
    );
    write_file(&beta.join("inner/d.txt"), b"beta nested file");

    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&sender_key);
    let receiver_bundle = make_self_signed(&receiver_key);

    let mut spec = SendSpec::new(
        alpha.clone(),
        loopback(0), // placeholder; replaced by the receiver's address below
        receiver_key.fingerprint(),
        sender_bundle,
    );
    spec.add_root(beta.clone());
    spec.bind = loopback(0);
    spec.chunk_size = NonZeroU32::new(1024).unwrap();
    spec.checkpoint_dir = send_ckpt.path().to_path_buf();
    spec.multistream = true;

    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv_spec, &|_| {}, addr_tx));
    spec.peer = addr_rx.await.expect("receiver bound");
    let report = send(spec, &|_| {})
        .await
        .expect("multi-root multistream send completes");
    let recv_report = receiver
        .await
        .expect("receiver task must not panic")
        .expect("multi-root multistream receive completes");

    // Root bookkeeping: sender lists both roots in order, receiver none.
    assert_eq!(
        report.roots,
        vec!["alpha".to_string(), "beta".to_string()],
        "sender report must list both roots, primary first"
    );
    assert!(recv_report.roots.is_empty());
    assert_eq!(report.job_id, recv_report.job_id);
    assert_eq!(report.resumed_chunks, 0);
    // Manifest arithmetic: entries and bytes of both trees merged.
    let alpha_tree = snapshot_tree(&alpha);
    let beta_tree = snapshot_tree(&beta);
    assert_eq!(
        report.files,
        alpha_tree.entry_count() + beta_tree.entry_count()
    );
    assert_eq!(
        report.bytes,
        alpha_tree.total_bytes() + beta_tree.total_bytes()
    );

    // Each root lands byte-identical under its own top-level directory name.
    assert_trees_identical(&alpha, &dst.path().join("alpha"));
    assert_trees_identical(&beta, &dst.path().join("beta"));
    assert_no_part_files(dst.path());
}
