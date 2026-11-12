//! 产品全流程（端到端叙事）：docs/ROADMAP.zh-CN.md “首个业务里程碑”的单机可执行子集。
//!
//! 里程碑原文：“关闭外网的同一路由网络，Windows↔Android 安全配对、选目录、
//! 超 4 GiB 文件、断网/恢复、最终摘要一致。”本文件把这条叙事拆成四个独立
//! `#[tokio::test]` 幕（各自独立临时目录与设备密钥），在单机环回（真 QUIC +
//! 真 TLS 身份 + 真磁盘）上执行其可执行子集：
//!
//! - 第一幕 `pairing_then_transfer_via_peer_book_admission`（T06/T01 形态）：
//!   双设备生成密钥 → A 导出配对信息（模拟 `pair export` 语义：公钥指纹 +
//!   配对码 + PeerBook save/load；AKPAIR1 文本格式本身由 ak-cli 单测归档）→
//!   B 配对 A → A 仅凭配对簿准入接收 → B 传输嵌套目录（空目录/二进制/CRLF）
//!   逐字节一致 → 未配对第三方 C 发送被拒于任何落盘之前（t06 手法）。
//! - 第二幕 `resume_after_receiver_kill_via_deterministic_job`（T10 形态）：
//!   64 KiB × 256 块（16 MiB）大文件，中途 abort 接收端（“断网”形态）→
//!   part/双侧检查点存活 → 同源重传 job_id 不变、双侧 resumed ≥ 1 → 深化
//!   t10：断言第二次连接后接收端检查点记录 state 最终为 "completed"。
//! - 第三幕 `multistream_multiroot_concurrent_milestone`（T10 多流多根形态）：
//!   同一发送设备的两条队列并发进入 `serve_concurrently(2)` 接收端；其中一条
//!   为多根（两目录一次传输）+ multistream；断言 Report.roots 与落盘顶层结构
//!   一致、逐根字节一致。
//! - 第四幕 `coordination_assisted_then_independent`（T07 收尾形态）：进程内
//!   真实 signal-server 完整会话建立并拿到候选 → 关停协调面（证明死亡、双端
//!   客户端观察到断链）→ 直连传输照常完成。叙事收束：协调面只在建立期存在，
//!   从未进入数据路径（ADR-001）。G5 / E1：第四幕使用旧明文 signal 平面，
//!   仅在 `legacy-plaintext-signal` 特性开启时编译（默认构建不含旧代码及其
//!   测试；`cargo test -p ak-itests --features legacy-plaintext-signal` 运行）。
//!
//! 如实边界：单机环回（L2 形态）。里程碑中的 >4 GiB 规模项（T12）不在此处
//! 重复（见其专属 u64/5 GiB 稀疏文件自动化）；Windows↔Android 真机、真实
//! 路由器断网/恢复与跨网段 NAT 验收仍属 L4，本文件不宣称。
#![forbid(unsafe_code)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
// G5 / E1: act-4-only imports, gated with the legacy signal plane.
#[cfg(feature = "legacy-plaintext-signal")]
use std::net::TcpListener as StdTcpListener;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "legacy-plaintext-signal")]
use std::time::Instant;

use ak_core::handle::ConflictPolicy;
use ak_core::handle::SaveTarget;
use ak_engine::{
    job_id_for, receive_with_addr, send, serve_concurrently, EngineError, Progress, ReceiveSpec,
    Report, SendSpec,
};
use ak_identity::tls::make_self_signed;
use ak_identity::{
    pairing_code, verify_pairing_code, DeviceKeyPair, Fingerprint, PeerBook, PeerRecord,
};
use ak_net::ConnectError;
// G5 / E1: the v1 plaintext signal plane is opt-in; act 4 below compiles
// only with the crate's `legacy-plaintext-signal` feature.
#[cfg(feature = "legacy-plaintext-signal")]
use ak_signal::client::{SignalClient, SignalError};
#[cfg(feature = "legacy-plaintext-signal")]
use ak_signal::codec::{CtrlMessage, PeerSignal};
#[cfg(feature = "legacy-plaintext-signal")]
use ak_signal::server::{self, Config, Limits};
use ak_store::{CheckpointStore, Direction, FileCheckpointStore};
#[cfg(feature = "legacy-plaintext-signal")]
use tokio::net::TcpStream;
use tokio::sync::{oneshot, watch};

use common::{
    assert_no_part_files, assert_trees_identical, snapshot_tree, try_run_transfer_parties,
    write_file, Lcg, TransferOpts, TransferParties, PART_SUFFIX,
};

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Milestone log line helper: job ids are printed as hex.
fn job_hex(job: &[u8; 16]) -> String {
    job.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Receiver admission backed by a saved-and-reloaded peer book（t06 手法）:
/// admission is pure book membership, nothing else.
fn authorize_from_book(book_path: &Path) -> common::AuthorizeFn {
    let book = PeerBook::load(book_path).expect("peer book loads");
    Box::new(move |fp: &Fingerprint| book.get(fp).is_some())
}

/// Reserve then release an ephemeral loopback TCP port（t07 手法）.
/// G5 / E1: act-4-only helper, gated with the legacy signal plane.
#[cfg(feature = "legacy-plaintext-signal")]
fn free_loopback_tcp_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .expect("reserve tcp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

/// Reserve then release an ephemeral loopback UDP port（engine 单测手法：
/// serve_concurrently 没有 addr_tx，测试只能先探测端口再让接收端绑定）.
fn free_loopback_udp_port() -> u16 {
    StdUdpSocket::bind(("127.0.0.1", 0))
        .expect("reserve udp port")
        .local_addr()
        .expect("reserved port")
        .port()
}

// ---------------------------------------------------------------------------
// 第一幕：配对 → 仅配对簿准入 → 嵌套目录传输 → 陌生人被拒
// ---------------------------------------------------------------------------

/// The full pairing-then-transfer story on one machine: two devices pair
/// through public material only (fingerprint + pairing code + on-disk peer
/// books, the `pair export` semantics), the receiver admits strictly by book
/// membership, the paired sender's nested directory (empty dir, binary, CRLF
/// text) lands byte-identical, and an unpaired third device C is refused
/// before a single byte reaches the destination（t06 场景 c 手法）.
#[tokio::test]
async fn pairing_then_transfer_via_peer_book_admission() {
    // Two devices generate keys: laptop A (receiver), phone B (sender).
    let a_key = DeviceKeyPair::generate();
    let b_key = DeviceKeyPair::generate();

    // A exports its public pairing info: fingerprint + human-verifiable
    // pairing code. B checks the code before trusting the fingerprint.
    let a_fp = a_key.fingerprint();
    let code = pairing_code(&a_fp);
    assert!(verify_pairing_code(&a_fp, &code), "own code must verify");
    // One extra digit can never be a valid 6-digit code (deterministic).
    assert!(!verify_pairing_code(&a_fp, &format!("{code}9")));
    eprintln!(
        "[e2e act1] A fingerprint {}… pairing code {code}",
        a_fp.short_id()
    );

    // B pairs A, A pairs B: both books persist and reload from disk (the
    // public pairing material is the only thing that crosses devices).
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a_book_path = a_dir.path().join("peers.book");
    let b_book_path = b_dir.path().join("peers.book");
    let mut a_book = PeerBook::default();
    a_book.upsert(PeerRecord {
        fingerprint: b_key.fingerprint(),
        name: "milestone-phone-b".into(),
        paired_unix_ms: 42,
    });
    a_book.save(&a_book_path).expect("A saves its peer book");
    let mut b_book = PeerBook::default();
    b_book.upsert(PeerRecord {
        fingerprint: a_fp,
        name: "milestone-laptop-a".into(),
        paired_unix_ms: 42,
    });
    b_book.save(&b_book_path).expect("B saves its peer book");

    // B picks A's pin out of its freshly loaded book (never a manual hex).
    let b_view = PeerBook::load(&b_book_path).expect("B reloads its book");
    let pinned = b_view
        .get(&a_fp)
        .expect("B's book holds A after pairing")
        .fingerprint;
    assert_eq!(pinned, a_fp);

    // B selects a directory: nested structure, an empty directory, binary
    // payloads, and a CRLF text file (byte fidelity, no transcoding).
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("selected-folder");
    let mut lcg = Lcg::new(0xE1E1_0000_0000_0001);
    write_file(
        &src.join("notes-crlf.txt"),
        b"line one\r\nline two\r\nno final newline",
    );
    write_file(&src.join("photos/img-0001.bin"), &lcg.bytes(160 * 1024));
    write_file(
        &src.join("photos/raw/img-0002.bin"),
        &lcg.bytes(53 * 1024 + 7),
    );
    fs::create_dir_all(src.join("photos/empty-album")).unwrap();
    write_file(
        &src.join("docs/deep/manual.txt"),
        b"milestone: paired transfer, byte-identical",
    );

    // Paired transfer: A admits strictly by its (reloaded) book.
    let dst = tempfile::tempdir().unwrap();
    let outcome = try_run_transfer_parties(
        &src,
        dst.path(),
        64,
        ConflictPolicy::Fail,
        120,
        TransferOpts::default(),
        TransferParties {
            sender_key: b_key.clone(),
            receiver_key: a_key.clone(),
            authorize: Some(authorize_from_book(&a_book_path)),
        },
    )
    .await;
    let send_report = outcome.send.expect("paired send must complete");
    let recv_report = outcome.recv.expect("paired receive must complete");
    assert_eq!(
        recv_report.peer_fingerprint.as_deref(),
        Some(b_key.fingerprint().to_hex().as_str()),
        "the receiver must see exactly the paired sender's fingerprint"
    );
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(send_report.resumed_chunks, 0, "fresh first transfer");
    assert_eq!(
        send_report.files,
        snapshot_tree(&src).entry_count(),
        "manifest entries (files + dirs)"
    );
    let src_tree = snapshot_tree(&src);
    assert_eq!(send_report.bytes, src_tree.total_bytes());
    eprintln!(
        "[e2e act1] job {} paired transfer: {} bytes, {} entries",
        job_hex(&send_report.job_id),
        send_report.bytes,
        send_report.files
    );
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());

    // Unpaired third device C: same book admission, stranger's key. The
    // book still holds exactly B (and not C) before the attempt.
    let c_key = DeviceKeyPair::generate();
    let a_view = PeerBook::load(&a_book_path).unwrap();
    assert!(a_view.get(&b_key.fingerprint()).is_some());
    assert!(a_view.get(&c_key.fingerprint()).is_none());
    let dst_refused = tempfile::tempdir().unwrap();
    let refused = try_run_transfer_parties(
        &src,
        dst_refused.path(),
        64,
        ConflictPolicy::Fail,
        120,
        TransferOpts::default(),
        TransferParties {
            sender_key: c_key,
            receiver_key: a_key,
            authorize: Some(authorize_from_book(&a_book_path)),
        },
    )
    .await;
    assert!(
        matches!(
            refused.send,
            Err(EngineError::Connect(_) | EngineError::Stream(_))
        ),
        "unpaired sender C must fail, got {:?}",
        refused.send
    );
    assert!(
        matches!(
            refused.recv,
            Err(EngineError::Connect(ConnectError::Unauthorized))
        ),
        "receiver must reject C as Unauthorized, got {:?}",
        refused.recv
    );
    let empty = snapshot_tree(dst_refused.path());
    assert!(
        empty.dirs.is_empty() && empty.files.is_empty(),
        "refused destination must stay empty, got files {:?} dirs {:?}",
        empty.files.keys().collect::<Vec<_>>(),
        empty.dirs.iter().collect::<Vec<_>>()
    );
    eprintln!("[e2e act1] unpaired C refused as Unauthorized before any byte landed");
}

// ---------------------------------------------------------------------------
// 第二幕：接收端中途被杀 → 检查点/part 存活 → 确定性 job 续传 → completed
// ---------------------------------------------------------------------------

/// The disconnect/recover story: B sends one 16 MiB file in 64 KiB chunks,
/// the receiver is aborted mid-flight ("断网" shape), the verified part
/// prefix and both checkpoint records survive, and the same source resumes
/// as the SAME deterministic job (job_id unchanged, both sides resumed
/// >= 1) to a byte-identical tree. Deepens t10: after the second
/// connection, the RECEIVER's checkpoint record for the job (same reused
/// checkpoint dir) must end with state == "completed".
#[tokio::test]
async fn resume_after_receiver_kill_via_deterministic_job() {
    const CHUNK: u32 = 64 * 1024;
    const FILE_BYTES: usize = 16 * 1024 * 1024; // 256 chunks of 64 KiB
    const TOTAL_CHUNKS: u64 = 256;
    const KILL_AFTER_EVENTS: u64 = 12;

    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("b-selected");
    let payload = Lcg::new(0xE2E2_0000_0000_0001).bytes(FILE_BYTES);
    write_file(&src.join("milestone-big.bin"), &payload);

    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    // ONE receiver checkpoint dir reused across both legs: leg 2 must leave
    // this job's record with state == "completed" inside it.
    let recv_ckpt = tempfile::tempdir().unwrap();

    let b_key = DeviceKeyPair::generate();
    let a_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&b_key);
    // Same device identity across both legs: keys survive a "restart".
    let receiver_bundle = make_self_signed(&a_key);

    let mut send_spec = SendSpec::new(
        src.clone(),
        loopback(0), // placeholder; replaced by the receiver's address below
        a_key.fingerprint(),
        sender_bundle,
    );
    send_spec.bind = loopback(0);
    send_spec.chunk_size = NonZeroU32::new(CHUNK).unwrap();
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let (chunks_tx, mut chunks_rx) = watch::channel(0u64);
    let progress = Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });

    // Leg 1: transfer until the receiver is killed mid-flight.
    let (addr_tx, addr_rx) = oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv_spec, &*progress, addr_tx).await })
    };
    send_spec.peer = addr_rx
        .await
        .expect("leg-1 receiver must report its address");
    let mut retry_spec = send_spec.clone();
    let sender = tokio::spawn(async move { send(send_spec, &|_| {}).await });

    while *chunks_rx.borrow() < KILL_AFTER_EVENTS {
        chunks_rx.changed().await.unwrap();
    }
    receiver.abort();
    let _ = receiver.await; // cancelled: socket closed, part handles released
    let leg1 = sender.await.expect("leg-1 sender task must not panic");
    assert!(
        matches!(
            leg1,
            Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
        ),
        "leg-1 sender must fail closed without fallback, got {leg1:?}"
    );

    // Survivors: a part file with a real verified prefix, and exactly one
    // sender job record whose id IS the deterministic job identity.
    let part_path = dst.path().join(format!("milestone-big.bin{PART_SUFFIX}"));
    let part_len = fs::metadata(&part_path)
        .unwrap_or_else(|error| panic!("part file must survive the kill: {error}"))
        .len();
    assert!(part_len > 0, "part prefix must hold verified bytes");
    let send_store = FileCheckpointStore::open(send_ckpt.path()).unwrap();
    let records = send_store.list().unwrap();
    assert_eq!(records.len(), 1, "exactly one send-job checkpoint survives");
    // Since wave 17 the checkpoint key is the per-instance record id
    // (content id + peer + direction + root); the deterministic content id
    // is re-derived from the unchanged source for the resume assertions.
    let record_id = records[0].job_id;
    let probe = ak_storage::LocalSource::open(&src, retry_spec.chunk_size).unwrap();
    let job_id = job_id_for(
        &probe.root_name(),
        <ak_storage::LocalSource as ak_core::handle::SourceHandle>::manifest(&probe),
    );
    eprintln!(
        "[e2e act2] job {} (record {}) receiver killed after {KILL_AFTER_EVENTS} chunk events; part {part_len} bytes survives",
        job_hex(&job_id),
        job_hex(&record_id)
    );

    // Leg 2: same identities, same destination, SAME receiver checkpoint
    // dir, fresh port. Deterministic job identity => same job resumes.
    let mut recv2 = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv2.bind = loopback(0);
    recv2.chunk_size = retry_spec.chunk_size;
    recv2.checkpoint_dir = recv_ckpt.path().to_path_buf();
    let (addr2_tx, addr2_rx) = oneshot::channel();
    let receiver2 = tokio::spawn(receive_with_addr(recv2, &|_| {}, addr2_tx));
    retry_spec.peer = addr2_rx
        .await
        .expect("leg-2 receiver must report its address");
    let (send_report, recv_report) = tokio::time::timeout(Duration::from_secs(120), async {
        let send_report = send(retry_spec, &|_| {})
            .await
            .expect("resumed send completes");
        let recv_report = receiver2
            .await
            .expect("leg-2 receiver task must not panic")
            .expect("resumed receive completes");
        (send_report, recv_report)
    })
    .await
    .expect("leg 2 must finish within 120s");

    assert_eq!(
        send_report.job_id, job_id,
        "same source must resume the same deterministic job"
    );
    assert_eq!(recv_report.job_id, job_id);
    assert!(
        send_report.resumed_chunks >= 1,
        "sender must skip verified chunks, got {}",
        send_report.resumed_chunks
    );
    assert!(
        recv_report.resumed_chunks >= 1,
        "receiver must report its verified prefix, got {}",
        recv_report.resumed_chunks
    );
    assert_eq!(send_report.bytes, FILE_BYTES as u64);
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
    eprintln!(
        "[e2e act2] job {} resumed: sender skipped {} chunks, receiver prefix {} chunks, {} bytes final",
        job_hex(&job_id),
        send_report.resumed_chunks,
        recv_report.resumed_chunks,
        send_report.bytes
    );

    // Deepened vs t10: after the SECOND connection, the reused receiver
    // checkpoint dir holds exactly this job's record, terminal state
    // "completed", receive direction, the sender's fingerprint, and the
    // fully verified chunk prefix.
    let recv_store = FileCheckpointStore::open(recv_ckpt.path()).unwrap();
    let recv_records = recv_store.list().unwrap();
    assert_eq!(
        recv_records.len(),
        1,
        "one receive record in the reused receiver checkpoint dir"
    );
    let record = &recv_records[0];
    // Wave 17: the checkpoint key is the per-instance record id binding
    // (content id + peer + direction + canonical root), not the wire id.
    let recv_record_id = ak_engine::record_id_for(
        &job_id,
        &b_key.fingerprint(),
        Direction::Receive,
        ak_storage::LocalSaveTarget::open(dst.path())
            .expect("reopen destination for canonical root")
            .root(),
    );
    assert_eq!(record.job_id, recv_record_id);
    assert_eq!(record.direction, Direction::Receive);
    assert_eq!(
        record.state, "completed",
        "receive checkpoint must end completed, got {:?}",
        record.state
    );
    assert_eq!(record.total_bytes, FILE_BYTES as u64);
    assert_eq!(record.peer_fingerprint, Some(b_key.fingerprint()));
    assert_eq!(record.files.len(), 1);
    assert_eq!(
        record.files[0].verified_prefix_chunks, TOTAL_CHUNKS,
        "the completed record must carry the fully verified prefix"
    );
}

// ---------------------------------------------------------------------------
// 第三幕：多根 + 多流 + 双队列并发（serve_concurrently(2)）
// ---------------------------------------------------------------------------

/// The multi-selection story at milestone scale: device B pushes TWO queues
/// concurrently into one `serve_concurrently(2)` receiver on A. Queue 1 is a
/// multi-root multistream job (photos + docs in one transfer), queue 2 a
/// single-root flow. Both jobs complete; `Report.roots` matches the on-disk
/// top-level layout exactly; every root lands byte-identical under its own
/// top-level directory name.
#[tokio::test]
async fn multistream_multiroot_concurrent_milestone() {
    let base = tempfile::tempdir().unwrap();
    let photos = base.path().join("photos");
    let docs = base.path().join("docs");
    let backup = base.path().join("backup");

    let mut lcg = Lcg::new(0xE3E3_0000_0000_0001);
    write_file(&photos.join("album/img-0001.bin"), &lcg.bytes(160 * 1024));
    write_file(
        &photos.join("album/raw/img-0002.bin"),
        &lcg.bytes(96 * 1024 + 3),
    );
    fs::create_dir_all(photos.join("album/empty")).unwrap();
    write_file(
        &docs.join("readme-crlf.txt"),
        b"act3\r\nmulti-root\r\npayload\r\n",
    );
    write_file(&docs.join("notes.txt"), b"two roots travel in one job");
    write_file(
        &backup.join("queue/second-flow.bin"),
        &lcg.bytes(120 * 1024),
    );

    let a_key = DeviceKeyPair::generate();
    let b_key = DeviceKeyPair::generate();

    let dst = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();
    let send1_ckpt = tempfile::tempdir().unwrap();
    let send2_ckpt = tempfile::tempdir().unwrap();

    // serve_concurrently has no addr_tx: probe a free UDP port, bind there,
    // and give the bind a beat before dialing（engine 单测手法）.
    let port = free_loopback_udp_port();
    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), make_self_signed(&a_key));
    recv_spec.bind = loopback(port);
    recv_spec.chunk_size = NonZeroU32::new(64 * 1024).unwrap();
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();
    let receiver = tokio::spawn(async move {
        serve_concurrently(recv_spec, 2, &|_index: u64, _event: Progress| {}).await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let pinned = a_key.fingerprint();
    // Queue 1: multi-root (photos primary + docs extra) + multistream.
    let mut spec1 = SendSpec::new(
        photos.clone(),
        loopback(port),
        pinned,
        make_self_signed(&b_key),
    );
    spec1.add_root(docs.clone());
    spec1.bind = loopback(0);
    spec1.chunk_size = NonZeroU32::new(64 * 1024).unwrap();
    spec1.checkpoint_dir = send1_ckpt.path().to_path_buf();
    spec1.multistream = true;
    spec1.device_name = "e2e-act3-b".into();

    // Queue 2: second concurrent flow from the same device, single root.
    let mut spec2 = SendSpec::new(
        backup.clone(),
        loopback(port),
        pinned,
        make_self_signed(&b_key),
    );
    spec2.bind = loopback(0);
    spec2.chunk_size = spec1.chunk_size;
    spec2.checkpoint_dir = send2_ckpt.path().to_path_buf();
    spec2.multistream = true;
    spec2.device_name = "e2e-act3-b".into();

    let sender1 = tokio::spawn(async move { send(spec1, &|_| {}).await });
    let sender2 = tokio::spawn(async move { send(spec2, &|_| {}).await });
    let report1 = sender1
        .await
        .expect("queue-1 task must not panic")
        .expect("multi-root queue completes");
    let report2 = sender2
        .await
        .expect("queue-2 task must not panic")
        .expect("second queue completes");
    let outcomes = receiver.await.expect("receiver task must not panic");
    assert_eq!(outcomes.len(), 2, "two expected jobs, two slots");

    // Receiver-side bookkeeping: both jobs Ok, no roots on receive reports,
    // and each sender's job id has its matching receiver report.
    let mut recv_by_job: BTreeMap<[u8; 16], Report> = BTreeMap::new();
    for (slot, outcome) in outcomes.into_iter().enumerate() {
        let report =
            outcome.unwrap_or_else(|error| panic!("receiver slot {slot} failed: {error:?}"));
        assert!(
            report.roots.is_empty(),
            "receiver reports carry no roots, got {:?}",
            report.roots
        );
        recv_by_job.insert(report.job_id, report);
    }
    assert_eq!(recv_by_job.len(), 2, "two distinct jobs");
    for sender_report in [&report1, &report2] {
        let peer_report = recv_by_job.get(&sender_report.job_id).unwrap_or_else(|| {
            panic!(
                "no receiver report for job {}",
                job_hex(&sender_report.job_id)
            )
        });
        assert_eq!(peer_report.bytes, sender_report.bytes);
        assert_eq!(peer_report.files, sender_report.files);
        assert_eq!(peer_report.resumed_chunks, 0);
    }

    // Root bookkeeping, with the real landing semantics asserted honestly:
    // a MULTI-root job lands every root under its own top-level name (the
    // names in Report.roots are exactly those directories), while a
    // SINGLE-root job reports its root name but lands the root's CONTENTS
    // at the destination top level (the destination is that root's landing
    // place, the name itself names the job).
    assert_eq!(
        report1.roots,
        vec!["photos".to_string(), "docs".to_string()],
        "queue 1 must list both roots, primary first"
    );
    assert_eq!(report2.roots, vec!["backup".to_string()]);
    for root in &report1.roots {
        assert!(
            dst.path().join(root).is_dir(),
            "multi-root report root {root} must land as a top-level directory"
        );
    }
    // Expected top level = queue 1's named roots + queue 2's content dirs.
    let queue2_children: BTreeSet<String> = fs::read_dir(&backup)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let expected_top: BTreeSet<String> = report1
        .roots
        .iter()
        .cloned()
        .chain(queue2_children)
        .collect();
    let actual_top: BTreeSet<String> = fs::read_dir(dst.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        actual_top, expected_top,
        "on-disk top-level layout must equal the named roots plus the single-root queue's contents"
    );

    // Manifest arithmetic and byte fidelity, per root.
    let photos_tree = snapshot_tree(&photos);
    let docs_tree = snapshot_tree(&docs);
    assert_eq!(
        report1.files,
        photos_tree.entry_count() + docs_tree.entry_count()
    );
    assert_eq!(
        report1.bytes,
        photos_tree.total_bytes() + docs_tree.total_bytes()
    );
    assert_eq!(report1.resumed_chunks, 0);
    assert_eq!(report2.files, snapshot_tree(&backup).entry_count());
    assert_eq!(report2.bytes, snapshot_tree(&backup).total_bytes());
    assert_trees_identical(&photos, &dst.path().join("photos"));
    assert_trees_identical(&docs, &dst.path().join("docs"));
    // Queue 2 (single root): contents at top level, so its subtree compares
    // against the destination root directly.
    assert_trees_identical(&backup.join("queue"), &dst.path().join("queue"));
    assert_no_part_files(dst.path());
    eprintln!(
        "[e2e act3] job {} roots {:?} ({} bytes) + job {} roots {:?} ({} bytes): 2 concurrent jobs served",
        job_hex(&report1.job_id),
        report1.roots,
        report1.bytes,
        job_hex(&report2.job_id),
        report2.roots,
        report2.bytes
    );
}

// ---------------------------------------------------------------------------
// 第四幕：协调面辅助建立 → 协调面死亡 → 直连独立完成（t07 手法）
//
// G5 / E1：第四幕全部内容属于旧明文 signal 平面，仅在
// `legacy-plaintext-signal` 特性开启时编译；默认构建的三幕保持不变。
// ---------------------------------------------------------------------------

/// Read one server message or fail (bounded wait)，t07 手法。
#[cfg(feature = "legacy-plaintext-signal")]
async fn next_msg(client: &SignalClient) -> CtrlMessage {
    tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("timed out waiting for a signaling message")
        .expect("signaling connection closed unexpectedly")
}

/// Next relayed peer signal inside `session`, skipping the server's
/// SameEgress hint (a server observation, not a peer signal)。
#[cfg(feature = "legacy-plaintext-signal")]
async fn next_peer_signal(client: &SignalClient, session: [u8; 16]) -> PeerSignal {
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

/// A real in-process signaling server on ephemeral loopback ports (t07).
#[cfg(feature = "legacy-plaintext-signal")]
struct SignalingPlane {
    tcp: SocketAddr,
    shutdown: watch::Sender<bool>,
    join: tokio::task::JoinHandle<std::io::Result<()>>,
}

#[cfg(feature = "legacy-plaintext-signal")]
async fn start_signaling_plane() -> SignalingPlane {
    for _ in 0..16 {
        let config = Config {
            tcp: loopback(free_loopback_tcp_port()),
            stun_udp: loopback(free_loopback_udp_port()),
            limits: Limits::default(),
        };
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tcp = config.tcp;
        let join = tokio::spawn(server::run(config, shutdown_rx));
        // Readiness = a real TCP connect succeeds (run binds TCP then UDP
        // before accepting); a finished task means the rebind lost the port
        // race, so detach it and try fresh ports.
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

#[cfg(feature = "legacy-plaintext-signal")]
impl SignalingPlane {
    /// Shut the coordination plane down and prove it is dead: clean task
    /// exit plus a fresh client can no longer complete Hello/Welcome.
    async fn stop(self) {
        self.shutdown
            .send(true)
            .expect("signaling server task still alive");
        let result = self.join.await.expect("signaling server task panicked");
        assert!(
            result.is_ok(),
            "signaling server must shut down cleanly: {result:?}"
        );
        match tokio::time::timeout(
            Duration::from_secs(3),
            SignalClient::connect(self.tcp, &DeviceKeyPair::generate(), "e2e-act4-death-probe"),
        )
        .await
        {
            // Timeout, refused, reset, EOF: no coordination service answering.
            Err(_) | Ok(Err(_)) => {}
            Ok(Ok(client)) => {
                let _ = client.close().await;
                panic!("coordination endpoint {} still answers", self.tcp);
            }
        }
    }
}

/// The severed client must observe its coordination connection dying; any
/// messages buffered from before the shutdown are drained first.
#[cfg(feature = "legacy-plaintext-signal")]
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

/// Result of the full coordination-plane session: both live clients plus
/// the candidate each side learned through the Signal relay.
#[cfg(feature = "legacy-plaintext-signal")]
struct Handshake {
    sender: SignalClient,
    receiver: SignalClient,
    candidate_seen_by_sender: SocketAddr,
    candidate_seen_by_receiver: SocketAddr,
}

/// Drive the full coordination flow between two real clients on a real
/// server: Hello/Welcome (inside `SignalClient::connect`), Register with
/// candidates, ConnectRequest -> SessionOpened on both sides, candidate
/// exchange and punch-timing exchange through the relay（t07 手法）. This is
/// the "coordination plane genuinely worked during establishment" premise.
#[cfg(feature = "legacy-plaintext-signal")]
async fn full_signaling_handshake(
    server_tcp: SocketAddr,
    sender_key: &DeviceKeyPair,
    receiver_key: &DeviceKeyPair,
    receiver_candidate: SocketAddr,
    sender_candidate: SocketAddr,
) -> Handshake {
    let sender = SignalClient::connect(server_tcp, sender_key, "e2e-act4-sender")
        .await
        .expect("sender Hello/Welcome");
    let receiver = SignalClient::connect(server_tcp, receiver_key, "e2e-act4-receiver")
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

    receiver
        .send_signal(session, PeerSignal::Candidates(vec![receiver_candidate]))
        .await
        .expect("receiver candidates");
    sender
        .send_signal(session, PeerSignal::Candidates(vec![sender_candidate]))
        .await
        .expect("sender candidates");
    let candidate_seen_by_sender = match next_peer_signal(&sender, session).await {
        PeerSignal::Candidates(mut list) => list.remove(0),
        other => panic!("expected Candidates, got {other:?}"),
    };
    let candidate_seen_by_receiver = match next_peer_signal(&receiver, session).await {
        PeerSignal::Candidates(mut list) => list.remove(0),
        other => panic!("expected Candidates, got {other:?}"),
    };

    // Punch timing flows receiver -> sender (the timing channel too).
    let punch = PeerSignal::PunchAt {
        epoch_unix_ms: 1_772_000_000_000,
        nonce: [0xE4; 8],
    };
    receiver
        .send_signal(session, punch.clone())
        .await
        .expect("PunchAt");
    match next_peer_signal(&sender, session).await {
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

/// The closing shape of the milestone story: establishment is
/// coordination-assisted (a real in-process signal server carries the full
/// session: Hello/Register/ConnectRequest/SessionOpened/candidates/punch),
/// then the coordination plane is killed and PROVEN dead, and the file
/// transfer still runs to a byte-identical finish on the candidate learned
/// over signaling — with pairing-book admission active the whole time. The
/// coordination plane exists only during establishment; it never sits in
/// the data path (ADR-001).
#[cfg(feature = "legacy-plaintext-signal")]
#[tokio::test]
async fn coordination_assisted_then_independent() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("milestone-src");
    write_file(
        &src.join("handoff.txt"),
        b"coordination assisted the handshake, then died; the direct path did not care",
    );
    write_file(
        &src.join("bin/payload.bin"),
        &Lcg::new(0xE4E4_0000_0000_0002).bytes(300 * 1024),
    );
    write_file(&src.join("nested/deep/note.txt"), b"deep nesting survives");
    fs::create_dir_all(src.join("empty-dir")).unwrap();

    // Pairing (act-1 semantics): A's book admits exactly B.
    let a_key = DeviceKeyPair::generate();
    let b_key = DeviceKeyPair::generate();
    let code = pairing_code(&a_key.fingerprint());
    assert!(verify_pairing_code(&a_key.fingerprint(), &code));
    let book_dir = tempfile::tempdir().unwrap();
    let book_path = book_dir.path().join("peers.book");
    let mut a_book = PeerBook::default();
    a_book.upsert(PeerRecord {
        fingerprint: b_key.fingerprint(),
        name: "milestone-phone-b".into(),
        paired_unix_ms: 43,
    });
    a_book.save(&book_path).expect("A saves its peer book");

    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    // The receiver engine is up FIRST: its bound address is the candidate
    // that will be registered and exchanged over the coordination plane.
    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), make_self_signed(&a_key));
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = NonZeroU32::new(64 * 1024).unwrap();
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();
    recv_spec.authorize = authorize_from_book(&book_path);
    let (addr_tx, addr_rx) = oneshot::channel();
    let receiver =
        tokio::spawn(async move { receive_with_addr(recv_spec, &|_| {}, addr_tx).await });
    let candidate = addr_rx.await.expect("receiver must report its address");

    // Establishment: full session on a REAL in-process signaling server.
    let plane = start_signaling_plane().await;
    let sender_candidate = loopback(free_loopback_udp_port());
    let handshake =
        full_signaling_handshake(plane.tcp, &b_key, &a_key, candidate, sender_candidate).await;
    assert_eq!(
        handshake.candidate_seen_by_sender, candidate,
        "the data address must be the one exchanged over signaling"
    );
    assert_eq!(handshake.candidate_seen_by_receiver, sender_candidate);
    eprintln!(
        "[e2e act4] session established over coordination: candidate {candidate}, pairing code {code}"
    );

    // Kill the coordination plane BEFORE any file byte moves, and prove it
    // is dead: clean shutdown, no new Hello/Welcome, both clients severed.
    plane.stop().await;
    assert_client_severed(&handshake.sender).await;
    assert_client_severed(&handshake.receiver).await;
    drop(handshake);

    // The direct transfer then runs to completion on that candidate, with
    // pairing-book admission still active.
    let mut send_spec = SendSpec::new(
        src.clone(),
        candidate,
        a_key.fingerprint(),
        make_self_signed(&b_key),
    );
    send_spec.bind = loopback(0);
    send_spec.chunk_size = NonZeroU32::new(64 * 1024).unwrap();
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();
    send_spec.device_name = "e2e-act4-b".into();

    let (send_report, recv_report) = tokio::time::timeout(Duration::from_secs(120), async {
        let send_report = send(send_spec, &|_| {})
            .await
            .expect("send completes after coordination death");
        let recv_report = receiver
            .await
            .expect("receiver task must not panic")
            .expect("receive completes after coordination death");
        (send_report, recv_report)
    })
    .await
    .expect("post-coordination transfer must finish within 120s");

    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(send_report.resumed_chunks, 0);
    assert_eq!(send_report.files, snapshot_tree(&src).entry_count());
    let src_tree = snapshot_tree(&src);
    assert_eq!(send_report.bytes, src_tree.total_bytes());
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_eq!(
        recv_report.peer_fingerprint.as_deref(),
        Some(b_key.fingerprint().to_hex().as_str()),
        "book admission must have seen exactly the paired sender"
    );
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
    eprintln!(
        "[e2e act4] job {} {} bytes landed byte-identical with the coordination plane dead",
        job_hex(&send_report.job_id),
        send_report.bytes
    );
}
