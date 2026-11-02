//! TEST_PLAN T05 / T06（形态映射）：注入未知/越界路径候选、未认证路径 ——
//! 发送文件前拒绝。
//!
//! 层级声明：L2 环回形态，但刻意**绕过 ak-engine::send**：测试自己用
//! ak-net 的已认证直连通道扮演“被攻陷的发送端”，直接对
//! `engine::receive_with_addr` 说话，从而注入引擎自身永远不会产生的帧：
//!
//! 1. 先正常完成 Hello/Offer/Accept，随后发送**篡改的 ManifestPage** ——
//!    用 ak-wire 构造真实清单条目后把 rel_path 手动改坏为 `../evil`。
//!    断言：接收侧以解码期路径校验错误（BadPath → Stream(Wire(..))）失败
//!    返回，而不是把越界路径落盘；目标目录与其父目录都没有 evil 文件。
//!    （身份替换 / 错误钉扎 / 未授权拒绝已在 ak-engine 单测覆盖，不重复。）
//! 2. 发送声明 payload_len = 0xFFFFFFFF 的超长帧头 —— 断言接收侧立即以
//!    Oversized 拒绝（在任何分配之前），进程不崩、测试继续存活即证明
//!    没有按声明长度缓冲 4 GiB。
#![forbid(unsafe_code)]

mod common;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use ak_core::handle::SourceHandle;
use ak_engine::{receive_with_addr, EngineError, ReceiveSpec, Report};
use ak_identity::{tls::make_self_signed, DeviceKeyPair};
use ak_net::{DirectEndpoint, DirectRecvStream, DirectSendStream, StreamError};
use ak_storage::LocalSource;
use ak_wire::{DecodeError, Frame, Limits, Message, WireEntry, CAP_RESUME, VERSION};

/// Oversized-frame header: version 1, type = Offer(2), payload_len u32::MAX.
const HUGE_HEADER: [u8; 7] = [0x01, 0x00, 0x02, 0xFF, 0xFF, 0xFF, 0xFF];

fn hello_frame() -> Frame {
    Frame {
        version: VERSION,
        message: Message::Hello {
            proto_version: 1,
            device_name: "compromised-sender".into(),
            public_key: [0u8; 32],
            capabilities: CAP_RESUME,
        },
    }
}

/// Spawn a real engine receiver on loopback port 0 and connect a raw
/// (but properly authenticated — we hold both identities) "evil sender" to it.
/// Returns the receiver's join handle, its address, and the evil stream halves.
async fn evil_sender_channel(
    dst: &Path,
) -> (
    tokio::task::JoinHandle<Result<Report, EngineError>>,
    SocketAddr,
    DirectSendStream,
    DirectRecvStream,
) {
    let evil_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let evil_bundle = make_self_signed(&evil_key);
    let receiver_bundle = make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();

    let ckpt = tempfile::tempdir().unwrap();
    let mut spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    spec.checkpoint_dir = ckpt.path().to_path_buf();

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(spec, &|_| {}, addr_tx));
    let addr = addr_rx.await.expect("receiver bound an address");

    // Legitimate TLS + QUIC + pinning: the attack is at the protocol layer,
    // on top of an authenticated channel (exactly the T05 threat shape).
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &evil_bundle)
        .await
        .expect("evil endpoint bind");
    let channel = endpoint
        .connect(addr, &receiver_fp)
        .await
        .expect("evil connect");
    let stream = channel.open_bi().await.expect("evil open_bi");
    let (tx, rx) = stream.split();
    (receiver, addr, tx, rx)
}

/// Hello exchange both directions, then Offer -> Accept for one real file.
async fn handshake_and_offer(
    tx: &mut DirectSendStream,
    rx: &mut DirectRecvStream,
    job_id: [u8; 16],
    file_count: u64,
    total_bytes: u64,
) {
    tx.send_frame(&hello_frame()).await.expect("send hello");
    match rx.recv_frame(&Limits::default()).await {
        Ok(frame) => assert!(
            matches!(frame.message, Message::Hello { .. }),
            "expected peer hello, got {:?}",
            frame.message
        ),
        Err(error) => panic!("receiver hello missing: {error:?}"),
    }
    tx.send_frame(&Frame {
        version: VERSION,
        message: Message::Offer {
            job_id,
            root_name: "evil-root".into(),
            file_count,
            total_bytes,
            chunk_size: 1024,
        },
    })
    .await
    .expect("send offer");
    match rx.recv_frame(&Limits::default()).await {
        Ok(frame) => assert!(
            matches!(frame.message, Message::Accept { .. }),
            "expected accept, got {:?}",
            frame.message
        ),
        Err(error) => panic!("accept missing: {error:?}"),
    }
}

/// T11/T05 crossover negative: a manifest page whose second entry carries the
/// traversal path `../evil` (built with ak-wire from a REAL scanned entry,
/// then tampered). The receiver must fail closed at decode time with nothing
/// written, and the evil path must not exist inside or beside the target.
#[tokio::test]
async fn tampered_manifest_with_traversal_path_is_rejected_and_writes_nothing() {
    let base = tempfile::tempdir().unwrap();
    // A real, fully valid manifest entry (real digests) to hide the bad one behind.
    let payload = base.path().join("innocent.bin");
    std::fs::write(&payload, b"perfectly legitimate content").unwrap();
    let source = LocalSource::open(&payload, NonZeroU32::new(1024).unwrap()).expect("scan source");
    let entry = &source.manifest().entries[0];
    let valid = WireEntry::from(entry);
    let mut evil = WireEntry::from(entry);
    evil.rel_path = "../evil".to_string(); // the tamper

    let dst = tempfile::tempdir().unwrap();
    let (receiver, _addr, mut tx, mut rx) = evil_sender_channel(dst.path()).await;
    let job_id = [0x0Eu8, 0x11, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05];
    handshake_and_offer(&mut tx, &mut rx, job_id, 2, entry.size).await;

    tx.send_frame(&Frame {
        version: VERSION,
        message: Message::ManifestPage {
            job_id,
            page_index: 0,
            last_page: true,
            entries: vec![valid, evil],
        },
    })
    .await
    .expect("send tampered page");

    // The receiver must fail closed, not panic, not "complete".
    let outcome = tokio::time::timeout(Duration::from_secs(20), receiver)
        .await
        .expect("receiver must fail fast")
        .expect("receiver task must not panic");
    match outcome {
        Err(EngineError::Stream(StreamError::Wire(DecodeError::BadPath(_)))) => {}
        other => panic!("expected BadPath decode rejection, got {other:?}"),
    }

    // Nothing was written: no part files, no finals, no tree at all, and no
    // escape into the destination's parent.
    let tree = common::snapshot_tree(dst.path());
    assert!(
        tree.dirs.is_empty() && tree.files.is_empty(),
        "target must stay empty, got dirs={:?} files={:?}",
        tree.dirs,
        tree.files
    );
    assert!(!dst.path().join("evil").exists());
    assert!(
        !dst.path()
            .parent()
            .expect("dst parent")
            .join("evil")
            .exists(),
        "no traversal escape into the parent directory"
    );

    // The evil sender observes the connection dying — no further protocol
    // progress is possible after the rejection.
    match tx.send_frame(&hello_frame()).await {
        Ok(()) => {}
        Err(error) => assert!(
            matches!(error, StreamError::Connection(_)),
            "expected connection loss, got {error:?}"
        ),
    }
    let _ = rx.recv_frame(&Limits::default()).await;
}

/// T05 framing negative: a frame header declaring a 4 GiB payload. The
/// receiver must refuse it at header-parse time (Oversized) before allocating
/// anything — proven by failing fast well inside the timeout while the
/// process keeps running normally afterwards.
#[tokio::test]
async fn oversized_frame_header_is_rejected_without_allocation_or_crash() {
    let dst = tempfile::tempdir().unwrap();
    let (receiver, _addr, mut tx, mut rx) = evil_sender_channel(dst.path()).await;

    // Complete the hello exchange first so the receiver is waiting for Offer.
    tx.send_frame(&hello_frame()).await.expect("send hello");
    let peer_hello = rx
        .recv_frame(&Limits::default())
        .await
        .expect("receiver hello");
    assert!(matches!(peer_hello.message, Message::Hello { .. }));

    // [version][type u16 BE][payload_len u32 BE] = 0xFFFF_FFFF, then nothing.
    tx.send_bytes(&HUGE_HEADER)
        .await
        .expect("send oversized header");

    // Must fail fast — if the receiver tried to buffer the declared payload
    // this would hit the 20s timeout (or die long before).
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(20), receiver)
        .await
        .expect("receiver must reject immediately, not buffer 4 GiB")
        .expect("receiver task must not panic");
    let elapsed = started.elapsed();
    match outcome {
        Err(EngineError::Stream(StreamError::Wire(DecodeError::Oversized {
            declared,
            budget,
        }))) => {
            assert_eq!(declared, u32::MAX);
            assert_eq!(budget as usize, ak_wire::MAX_FRAME_BYTES);
        }
        other => panic!("expected Oversized rejection, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "rejection took {elapsed:?}; allocation must happen before reading, never"
    );

    // Process alive and nothing written: the target stays empty.
    let tree = common::snapshot_tree(dst.path());
    assert!(
        tree.dirs.is_empty() && tree.files.is_empty(),
        "target must stay empty"
    );
}
