//! TEST_PLAN T06（深化）：身份替换 / 未认证路径 —— 发送文件前拒绝。
//!
//! 层级声明：L2 环回形态（场景 b 为 raw ak-net 通道层归档）。三个互补场景，
//! 所有拒绝都断言发生在**任何字节落盘之前**（目标目录为空）：
//!
//! (a) mid-transfer 断开后，**另一把密钥**的"接收端"抢占同一端口等待续传：
//!     发送端重试连接必须因证书钉扎失败而拒绝（身份替换攻击面——钉扎的是
//!     指纹而非地址，地址复用不能继承身份）；
//! (b) 发送端持有正确指纹（来自配对），但对端地址上站着第三方密钥的端点：
//!     raw ak-net 层 connect 在 TLS 握手期拒绝（与 ak-net 单测
//!     `wrong_pin_fails_closed` 同机制，此处作为 T06 场景归档）；
//! (c) 配对簿准入语义：receive `authorize` 只认 peers.book —— 用引擎
//!     `ReceiveSpec.authorize` 回调手工模拟簿内/簿外指纹各一次。
#![forbid(unsafe_code)]

mod common;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_core::handle::ConflictPolicy;
use ak_engine::{receive_with_addr, send, EngineError, Progress, ReceiveSpec, SendSpec};
use ak_identity::tls::make_self_signed;
use ak_identity::{DeviceKeyPair, Fingerprint, PeerBook, PeerRecord};
use ak_net::{ConnectError, DirectEndpoint};

use common::{
    assert_no_part_files, assert_trees_identical, snapshot_tree, try_run_transfer_parties,
    write_file, TransferOpts, TransferParties,
};

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// (a) Identity swap on the SAME port. Interrupt a transfer mid-flight, let a
/// different key's receiver take over the freed port, then retry the resume:
/// the sender still pins the ORIGINAL fingerprint, so the TLS handshake must
/// fail closed and not a single byte may reach the impostor's directory.
#[tokio::test]
async fn mid_transfer_port_takeover_by_another_key_is_refused_by_pinning() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t06-swap-src");
    // 24 chunks of 1 KiB: killable mid-flight with a durable part prefix.
    let payload: Vec<u8> = common::Lcg::new(0x0606_0000_0000_0001).bytes(24 * 1024);
    write_file(&src.join("swap.bin"), &payload);

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&sender_key);
    let receiver_bundle = make_self_signed(&receiver_key);
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();
    let dst_real = tempfile::tempdir().unwrap();
    let dst_impostor = tempfile::tempdir().unwrap();

    let mut send_spec = SendSpec::new(
        src.clone(),
        loopback(0), // placeholder; replaced by the receiver's address below
        receiver_key.fingerprint(),
        sender_bundle.clone(),
    );
    send_spec.bind = loopback(0);
    send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let mut recv_spec = ReceiveSpec::new(dst_real.path().to_path_buf(), receiver_bundle);
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
    let progress = Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });

    // Leg 1: connect, start transferring, then kill the receiver mid-flight.
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv_spec, &*progress, addr_tx).await })
    };
    let real_addr = addr_rx.await.expect("leg-1 receiver bound");
    send_spec.peer = real_addr;
    let retry_spec = send_spec.clone();
    let sender = tokio::spawn(async move { send(send_spec, &|_| {}).await });

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
    // Context: the swap targeted a resumable job (a real part file survived).
    assert!(
        dst_real.path().join("swap.bin.akpart").exists(),
        "leg-1 must leave a resumable part file"
    );

    // The impostor: a DIFFERENT device key's receiver squatting on the same
    // port, waiting for the sender's resume attempt. Quinn's UDP driver
    // releases the aborted receiver's socket asynchronously, so the takeover
    // bind may need a few attempts before the port is free again.
    let impostor_key = DeviceKeyPair::generate();
    let impostor_bundle = make_self_signed(&impostor_key);
    let impostor_ckpt = tempfile::tempdir().unwrap();
    let mut impostor = None;
    let mut impostor_addr = None;
    for _ in 0..20 {
        let mut impostor_spec =
            ReceiveSpec::new(dst_impostor.path().to_path_buf(), impostor_bundle.clone());
        impostor_spec.bind = loopback(real_addr.port());
        impostor_spec.chunk_size = retry_spec.chunk_size;
        impostor_spec.checkpoint_dir = impostor_ckpt.path().to_path_buf();
        let (imp_addr_tx, imp_addr_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(receive_with_addr(impostor_spec, &|_| {}, imp_addr_tx));
        match imp_addr_rx.await {
            Ok(addr) => {
                impostor = Some(task);
                impostor_addr = Some(addr);
                break;
            }
            Err(_) => {
                // Bind refused (port not yet released): drain and back off.
                let _ = task.await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    let impostor = impostor.expect("impostor must take over the freed port");
    let impostor_addr = impostor_addr.expect("impostor address");
    assert_eq!(
        impostor_addr, real_addr,
        "impostor must squat the same address"
    );

    // Resume retry: same source, same checkpoint dir (same job), same pinned
    // fingerprint — but the peer answering at that address is somebody else.
    let mut retry_spec = retry_spec;
    retry_spec.peer = real_addr;
    let started = Instant::now();
    let retry = send(retry_spec, &|_| {}).await;
    let elapsed = started.elapsed();
    match retry {
        Err(EngineError::Connect(ConnectError::PeerIdentity(_)))
        | Err(EngineError::Connect(ConnectError::Transport(_))) => {}
        Ok(report) => panic!("identity swap must not transfer, got {report:?}"),
        Err(other) => panic!("pinning must refuse the impostor, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "rejection must be prompt, took {elapsed:?}"
    );

    // The impostor never gains a channel either, and its target directory
    // stays empty: the failure is strictly before any disk write.
    let impostor_outcome = tokio::time::timeout(Duration::from_secs(20), impostor)
        .await
        .expect("impostor accept must fail promptly")
        .expect("impostor task must not panic");
    assert!(
        impostor_outcome.is_err(),
        "no channel may be produced for the impostor, got {impostor_outcome:?}"
    );
    let tree = snapshot_tree(dst_impostor.path());
    assert!(
        tree.dirs.is_empty() && tree.files.is_empty(),
        "impostor target must stay empty, got files {:?} dirs {:?}",
        tree.files.keys().collect::<Vec<_>>(),
        tree.dirs.iter().collect::<Vec<_>>()
    );
}

/// (b) Raw ak-net layer: the sender holds the CORRECT fingerprint from
/// pairing, but the endpoint at the peer address presents a third-party key.
/// `connect` must fail during the TLS handshake — no channel is ever produced
/// on either side, so no payload could ever ride that path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correct_pin_against_third_party_certificate_fails_at_handshake() {
    let sender_key = DeviceKeyPair::generate();
    let paired_key = DeviceKeyPair::generate(); // the fingerprint we hold
    let third_party_key = DeviceKeyPair::generate(); // who actually answers
    let sender_bundle = make_self_signed(&sender_key);
    let third_party_bundle = make_self_signed(&third_party_key);

    let third_party = DirectEndpoint::bind(loopback(0), &third_party_bundle)
        .await
        .expect("third-party endpoint bind");
    let third_party_addr = third_party.local_addr().expect("third-party local addr");
    let sender = DirectEndpoint::bind(loopback(0), &sender_bundle)
        .await
        .expect("sender endpoint bind");

    // The third party is a willing listener (its authorize accepts anyone);
    // the CLIENT-side pin must be what refuses.
    let server_task = tokio::spawn(async move { third_party.accept(&|_| true).await });

    let started = Instant::now();
    match sender
        .connect(third_party_addr, &paired_key.fingerprint())
        .await
    {
        Err(ConnectError::PeerIdentity(_)) | Err(ConnectError::Transport(_)) => {}
        Ok(channel) => panic!(
            "a channel to a third-party key must never exist, path {:?}",
            channel.path()
        ),
        Err(other) => panic!("unexpected error class: {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "handshake rejection must be prompt, took {:?}",
        started.elapsed()
    );

    // Symmetric refusal: the server side must not gain a channel either.
    let server_result = server_task.await.expect("server task must not panic");
    assert!(server_result.is_err(), "server must not gain a channel");
}

/// Fresh authorize callback backed by the peer book on disk: one receiver
/// run loads its pairing book, admission is pure book membership.
fn authorize_from_book(book_path: &Path) -> common::AuthorizeFn {
    let book = PeerBook::load(book_path).expect("peer book loads");
    Box::new(move |fp: &Fingerprint| book.get(fp).is_some())
}

/// (c) Pairing-book admission: the receiver's authorize callback consults a
/// real saved-and-reloaded peers.book. The in-book sender's transfer
/// completes byte-identical; an equally valid but out-of-book sender key is
/// refused before a single byte lands in the destination.
#[tokio::test]
async fn receive_authorize_enforces_peer_book_membership() {
    let book_dir = tempfile::tempdir().unwrap();
    let book_path = book_dir.path().join("peers.txt");

    let paired_sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let mut book = PeerBook::default();
    book.upsert(PeerRecord {
        fingerprint: paired_sender_key.fingerprint(),
        name: "t06-paired-laptop".into(),
        paired_unix_ms: 42,
    });
    book.save(&book_path).expect("save peer book");
    let reloaded = PeerBook::load(&book_path).expect("reload peer book");
    assert_eq!(
        reloaded.iter().count(),
        1,
        "the book holds exactly one peer"
    );

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t06-book-src");
    write_file(&src.join("secret.txt"), b"paired peers only");
    write_file(
        &src.join("nested/data.bin"),
        &common::Lcg::new(0x0606_0000_0000_0002).bytes(8 * 1024),
    );

    // In-book sender: admission passes, transfer completes, bytes identical.
    let dst_ok = tempfile::tempdir().unwrap();
    let in_book = try_run_transfer_parties(
        &src,
        dst_ok.path(),
        1,
        ConflictPolicy::Fail,
        120,
        TransferOpts::default(),
        TransferParties {
            sender_key: paired_sender_key.clone(),
            receiver_key: receiver_key.clone(),
            authorize: Some(authorize_from_book(&book_path)),
        },
    )
    .await;
    let send_ok = in_book.send.expect("in-book send must complete");
    let recv_ok = in_book.recv.expect("in-book receive must complete");
    assert_eq!(
        recv_ok.peer_fingerprint.as_deref(),
        Some(paired_sender_key.fingerprint().to_hex().as_str()),
        "the receiver must see exactly the paired sender's fingerprint"
    );
    assert_eq!(send_ok.job_id, recv_ok.job_id);
    assert_trees_identical(&src, dst_ok.path());
    assert_no_part_files(dst_ok.path());

    // Out-of-book sender: same book, same receiver, but a stranger's key.
    let stranger_key = DeviceKeyPair::generate();
    let book_view = PeerBook::load(&book_path).unwrap();
    assert!(
        book_view.get(&paired_sender_key.fingerprint()).is_some()
            && book_view.get(&stranger_key.fingerprint()).is_none(),
        "book membership must be exactly: paired in, stranger out"
    );
    let dst_refused = tempfile::tempdir().unwrap();
    let out_of_book = try_run_transfer_parties(
        &src,
        dst_refused.path(),
        1,
        ConflictPolicy::Fail,
        120,
        TransferOpts::default(),
        TransferParties {
            sender_key: stranger_key,
            receiver_key,
            authorize: Some(authorize_from_book(&book_path)),
        },
    )
    .await;
    // TLS itself succeeds (the cert is validly self-signed); rejection
    // surfaces on first use as a lost stream or failed connect.
    assert!(
        matches!(
            out_of_book.send,
            Err(EngineError::Connect(_) | EngineError::Stream(_))
        ),
        "out-of-book sender must fail, got {:?}",
        out_of_book.send
    );
    assert!(
        matches!(
            out_of_book.recv,
            Err(EngineError::Connect(ConnectError::Unauthorized))
        ),
        "receiver must reject the stranger as Unauthorized, got {:?}",
        out_of_book.recv
    );
    // Strictly before any byte lands:
    let tree = snapshot_tree(dst_refused.path());
    assert!(
        tree.dirs.is_empty() && tree.files.is_empty(),
        "refused destination must stay empty, got files {:?}",
        tree.files.keys().collect::<Vec<_>>()
    );
}
