//! 确定性随机性质套件：同一套不变量，种子扫过随机树形状空间。
//!
//! 层级声明：L2 环回形态（127.0.0.1 上走完整 QUIC + TLS + 引擎 + 检查点），
//! 与 crate 其余场景同一手法；不是真机/跨网/真实断网主张。
//!
//! 三个套件（生成器与失败诊断见 `prop_common`）：
//!
//! 1. **随机树传输不变量**（`#[ignore]`，release）：种子 1..=25，每种子一棵
//!    随机树（0-40 文件、单文件 0-512 KiB、深度 0-6、名称混合 CJK/内部
//!    空格/多点扩展/单字符/点前缀/全混合、随机空目录链），chunk_kb 随机
//!    ∈ {16,64,256}，multistream 随机。断言：
//!    - 双端 Report 成功且 files == 目录数+文件数、bytes == 生成器总字节；
//!    - 双端 job_id 一致；全新目的地下 resumed_chunks == 0；
//!    - 接收端 ChunkReceived 事件数 == 总 chunk 数（每 chunk 恰一次）；
//!    - 目的树与源树逐字节一致；无 .akpart 残留。
//! 2. **随机中断点续传不变量**（`#[ignore]`，release）：种子 1..=10，树同上
//!    规模减半（0-20 文件；总 chunk 兜底 >=8，生成器确定性加长 0 号文件），
//!    中断点 N ∈ [1, floor(total/2)] 由第二种子流抽取；数据面进行到 N 个
//!    ChunkReceived 后 abort 接收任务（复用 ak-engine/t10 的手法）。断言：
//!    - leg-1 发送端必须失败关闭（Stream/PeerClosed/Connect，无回退）；
//!    - 中断只发生在数据面：0 个最终文件落盘、>=1 个 .akpart 存活、
//!      发送端恰好一条检查点记录；
//!    - 重传（同源/同 chunk/同发送检查点目录/同身份）完成：resumed_chunks
//!      >= 1（双端）、job_id 与中断前一致、逐字节一致、无残留。
//! 3. **快速回归**（默认运行，debug 可跑）：种子 1/2/3 的缩小版（文件 <=6、
//!    深度 <=3、空目录 <=2），跑套件 1 的全部传输不变量——日常
//!    `cargo test -p ak-itests` 即覆盖随机路径。同种子号在快速档与 release
//!    档抽取自同一 LCG 流但上限不同，故是“同一种子编号的小规模抽样”，
//!    不保证是 release 树的子树。
//!
//! 已知限制（如实）：
//! - 全空树（0 文件 0 目录）不覆盖：生成器把文件数夹到 >=1（单根空清单的
//!   传输形态属 ak-engine 单测/多根专项的范围）；
//! - 多根（extra_roots）不在随机矩阵内（t10 有专项）；
//! - 中断是进程内任务级 abort 的“进程重启”形态，不是真实双进程崩溃/断网
//!   注入；abort 落点与“最后一刻完成”之间存在理论竞态——套件以
//!   “leg-1 必须失败关闭”断言兜底，竞态命中时报出清晰失败而非误判通过；
//! - 名字空间刻意避开尾随点/空格、点组件与 Windows 保留设备名（这些形态
//!   的拒绝行为由 t11 钉住）。
//!
//! 跑法：
//! - 快速版：`cargo test -p ak-itests --test property_random`
//! - 随机大扫：`cargo test --release -p ak-itests --test property_random -- --ignored --nocapture`
#![forbid(unsafe_code)]

mod prop_common;

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_core::handle::ConflictPolicy;
use ak_engine::{receive_with_addr, send, EngineError, Progress, ReceiveSpec, SendSpec};
use ak_identity::tls::make_self_signed;
use ak_identity::DeviceKeyPair;
use ak_store::{CheckpointStore, FileCheckpointStore};

use prop_common::{
    assert_no_part_files_ctx, assert_trees_identical_ctx, build_random_tree, snapshot_tree,
    try_run_transfer_opts, GenSummary, Lcg, TransferOpts, TreeParams, ABORT_SEED_SALT, PART_SUFFIX,
};

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// 按载荷规模给预算：宽裕但不无限（debug 慢、release 快，都以它为上限）。
fn budget_secs(bytes: u64) -> u64 {
    120 + bytes / (64 * 1024)
}

/// 单种子完整随机传输：构建 -> 环回传输 -> 全套不变量。返回 wall 耗时。
async fn run_transfer_seed(seed: u64, params: &TreeParams) -> Duration {
    let src = tempfile::tempdir().unwrap();
    let summary = build_random_tree(src.path(), seed, params);
    let dst = tempfile::tempdir().unwrap();
    let ctx = summary.ctx();
    let opts = TransferOpts {
        multistream: summary.multistream,
        ..TransferOpts::default()
    };
    let budget = budget_secs(summary.bytes);
    let started = Instant::now();
    let raw = try_run_transfer_opts(
        src.path(),
        dst.path(),
        summary.chunk_kb,
        ConflictPolicy::Fail,
        budget,
        opts,
    )
    .await;
    let wall = started.elapsed();

    let send_report = raw
        .send
        .unwrap_or_else(|error| panic!("{ctx}send failed: {error:?}"));
    let recv_report = raw
        .recv
        .unwrap_or_else(|error| panic!("{ctx}receive failed: {error:?}"));

    let expected_entries = (summary.dirs + summary.files) as u64;
    assert_eq!(
        send_report.files, expected_entries,
        "{ctx}sender entry count (dirs+files) mismatch"
    );
    assert_eq!(
        recv_report.files, expected_entries,
        "{ctx}receiver entry count (dirs+files) mismatch"
    );
    assert_eq!(send_report.bytes, summary.bytes, "{ctx}sender byte count");
    assert_eq!(recv_report.bytes, summary.bytes, "{ctx}receiver byte count");
    assert_eq!(
        send_report.job_id, recv_report.job_id,
        "{ctx}job id mismatch between endpoints"
    );
    assert_eq!(
        send_report.resumed_chunks, 0,
        "{ctx}fresh destination must not resume (sender)"
    );
    assert_eq!(
        recv_report.resumed_chunks, 0,
        "{ctx}fresh destination must not resume (receiver)"
    );
    let chunks_seen = raw
        .events
        .iter()
        .filter(|event| matches!(event, Progress::ChunkReceived { .. }))
        .count() as u64;
    assert_eq!(
        chunks_seen, summary.total_chunks,
        "{ctx}ChunkReceived events must equal the generator's chunk total"
    );

    assert_trees_identical_ctx(&ctx, src.path(), dst.path());
    assert_no_part_files_ctx(&ctx, dst.path());

    println!(
        "[prop transfer seed={seed}] files={} dirs={} bytes={} chunks={} chunk_kb={} \
         multistream={} wall={wall:.2?} (send {:.2?} / recv {:.2?})",
        summary.files,
        summary.dirs,
        summary.bytes,
        summary.total_chunks,
        summary.chunk_kb,
        summary.multistream,
        send_report.duration,
        recv_report.duration,
    );
    wall
}

/// 单种子随机中断续传：leg-1 中途 abort 接收任务 -> leg-2 同身份重传完成。
/// 返回 wall 耗时。
async fn run_resume_seed(seed: u64) -> Duration {
    let src = tempfile::tempdir().unwrap();
    let summary: GenSummary = build_random_tree(src.path(), seed, &TreeParams::release_resume());
    let ctx = summary.ctx();
    let total_chunks = summary.total_chunks;
    // 中断点：独立种子流（树规划完成后才知道总 chunk 数）。
    let abort_at = 1 + Lcg::new(seed ^ ABORT_SEED_SALT).below((total_chunks / 2).max(1));

    let dst = tempfile::tempdir().unwrap();
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = make_self_signed(&sender_key);
    // 同一设备身份跨两腿存活（“进程重启”形态）。
    let receiver_bundle = make_self_signed(&receiver_key);
    let chunk_size = NonZeroU32::new(summary.chunk_kb * 1024).unwrap();

    let mut spec = SendSpec::new(
        src.path().to_path_buf(),
        loopback(0), // 占位；收到接收端地址后替换。
        receiver_key.fingerprint(),
        sender_bundle,
    );
    spec.device_name = "prop-resume-sender".into();
    spec.chunk_size = chunk_size;
    spec.checkpoint_dir = send_ckpt.path().to_path_buf();
    spec.multistream = summary.multistream;

    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv_spec.device_name = "prop-resume-receiver".into();
    recv_spec.bind = loopback(0);
    recv_spec.chunk_size = chunk_size;
    recv_spec.conflict = ConflictPolicy::Fail;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    // 数据面进度计数：ChunkReceived 每 chunk 恰一次（接收端）。
    let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
    let progress = Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });

    // ---- Leg 1：传输进行到第 abort_at 个 chunk 后杀接收任务 ----------------
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv_spec, &*progress, addr_tx).await })
    };
    spec.peer = addr_rx
        .await
        .expect("leg-1 receiver must report its address");
    let leg2_spec = spec.clone();
    let mut sender = tokio::spawn(async move { send(spec, &|_| {}).await });

    let mut reached = *chunks_rx.borrow() >= abort_at;
    while !reached {
        tokio::select! {
            changed = chunks_rx.changed() => {
                if changed.is_err() {
                    // 接收端已结束（watch 关闭）：以当前计数判定。
                    break;
                }
            }
            _ = &mut sender => break,
        }
        reached = *chunks_rx.borrow() >= abort_at;
    }
    receiver.abort();
    let _ = receiver.await; // 取消：套接字关闭、part 句柄释放。
    let leg1 = sender.await.expect("leg-1 sender task must not panic");
    assert!(
        reached,
        "{ctx}abort point never reached (wanted {abort_at}/{total_chunks} ChunkReceived); \
         leg-1 sender outcome: {leg1:?}"
    );
    assert!(
        matches!(
            leg1,
            Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
        ),
        "{ctx}leg-1 sender must fail closed after the mid-data abort, got {leg1:?}"
    );

    // ---- 中断只发生在数据面：0 最终文件、>=1 part、1 条发送检查点 --------
    let tree = snapshot_tree(dst.path());
    let finals: Vec<&String> = tree
        .files
        .keys()
        .filter(|name| !name.ends_with(PART_SUFFIX))
        .collect();
    assert!(
        finals.is_empty(),
        "{ctx}abort during the data phase must leave zero committed files, found {finals:?}"
    );
    let parts = tree.files.len() - finals.len();
    assert!(
        parts >= 1,
        "{ctx}at least one .akpart must survive the abort, found {parts}"
    );
    let store = FileCheckpointStore::open(send_ckpt.path()).unwrap();
    let records = store.list().unwrap();
    assert_eq!(
        records.len(),
        1,
        "{ctx}exactly one sender checkpoint record must survive the abort"
    );
    let job_id = records[0].job_id;

    // ---- Leg 2：同源/同 chunk/同发送检查点目录/同身份 -> 续传完成 ---------
    let started = Instant::now();
    let budget = budget_secs(summary.bytes);
    let recv2_ckpt = tempfile::tempdir().unwrap();
    let mut recv2 = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv2.device_name = "prop-resume-receiver".into();
    recv2.bind = loopback(0);
    recv2.chunk_size = chunk_size;
    recv2.conflict = ConflictPolicy::Fail;
    recv2.checkpoint_dir = recv2_ckpt.path().to_path_buf();
    let (addr2_tx, addr2_rx) = tokio::sync::oneshot::channel();
    let receiver2 = tokio::spawn(receive_with_addr(recv2, &|_| {}, addr2_tx));
    let mut spec2 = leg2_spec;
    spec2.peer = addr2_rx
        .await
        .expect("leg-2 receiver must report its address");
    let send_report = tokio::time::timeout(Duration::from_secs(budget), send(spec2, &|_| {}))
        .await
        .unwrap_or_else(|_| panic!("{ctx}leg-2 send exceeded the {budget}s budget"))
        .unwrap_or_else(|error| panic!("{ctx}leg-2 send failed: {error:?}"));
    let recv_report = tokio::time::timeout(Duration::from_secs(budget), receiver2)
        .await
        .unwrap_or_else(|_| panic!("{ctx}leg-2 receive exceeded the {budget}s budget"))
        .expect("leg-2 receiver task must not panic")
        .unwrap_or_else(|error| panic!("{ctx}leg-2 receive failed: {error:?}"));
    let wall = started.elapsed();

    // ---- 续传不变量 --------------------------------------------------------
    assert_eq!(
        send_report.job_id, job_id,
        "{ctx}job id must survive the interruption (resume, not a new job)"
    );
    assert_eq!(
        recv_report.job_id, job_id,
        "{ctx}receiver job id must match the pre-abort checkpoint record"
    );
    assert!(
        send_report.resumed_chunks >= 1,
        "{ctx}sender must skip verified chunks on resume, resumed={}",
        send_report.resumed_chunks
    );
    assert!(
        recv_report.resumed_chunks >= 1,
        "{ctx}receiver must report its verified prefix, resumed={}",
        recv_report.resumed_chunks
    );
    let expected_entries = (summary.dirs + summary.files) as u64;
    assert_eq!(
        send_report.files, expected_entries,
        "{ctx}resumed sender entry count mismatch"
    );
    assert_eq!(
        recv_report.files, expected_entries,
        "{ctx}resumed receiver entry count mismatch"
    );
    assert_eq!(send_report.bytes, summary.bytes, "{ctx}resumed byte count");
    assert_eq!(
        recv_report.bytes, summary.bytes,
        "{ctx}resumed recv byte count"
    );
    assert_trees_identical_ctx(&ctx, src.path(), dst.path());
    assert_no_part_files_ctx(&ctx, dst.path());

    println!(
        "[prop resume seed={seed}] files={} dirs={} bytes={} chunks={} chunk_kb={} \
         multistream={} abort_at={abort_at} parts_after_abort={parts} \
         resumed_send={} resumed_recv={} wall={wall:.2?}",
        summary.files,
        summary.dirs,
        summary.bytes,
        total_chunks,
        summary.chunk_kb,
        summary.multistream,
        send_report.resumed_chunks,
        recv_report.resumed_chunks,
    );
    wall
}

/// 套件 1：25 种子随机树传输不变量扫描（release-only，`#[ignore]`）。
#[tokio::test]
#[ignore = "25-seed randomized sweep; release only: cargo test --release -p ak-itests --test property_random -- --ignored --nocapture"]
async fn random_tree_transfers_hold_invariants_across_25_seeds() {
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    for seed in 1..=25 {
        slowest = slowest.max(run_transfer_seed(seed, &TreeParams::release_transfer()).await);
    }
    println!(
        "[prop transfer suite] 25 seeds done in {:.2?} (slowest seed {:.2?})",
        started.elapsed(),
        slowest
    );
}

/// 套件 2：10 种子随机中断点续传不变量扫描（release-only，`#[ignore]`）。
#[tokio::test]
#[ignore = "10-seed randomized interrupt-resume sweep; release only: cargo test --release -p ak-itests --test property_random -- --ignored --nocapture"]
async fn random_interruptions_resume_to_identical_trees_across_10_seeds() {
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    for seed in 1..=10 {
        slowest = slowest.max(run_resume_seed(seed).await);
    }
    println!(
        "[prop resume suite] 10 seeds done in {:.2?} (slowest seed {:.2?})",
        started.elapsed(),
        slowest
    );
}

/// 快速回归（默认运行，debug 可跑）：种子 1 的缩小版随机树。
#[tokio::test]
async fn fast_random_seed_1_small_tree_transfers_byte_identical() {
    run_transfer_seed(1, &TreeParams::fast_debug()).await;
}

/// 快速回归：种子 2。
#[tokio::test]
async fn fast_random_seed_2_small_tree_transfers_byte_identical() {
    run_transfer_seed(2, &TreeParams::fast_debug()).await;
}

/// 快速回归：种子 3。
#[tokio::test]
async fn fast_random_seed_3_small_tree_transfers_byte_identical() {
    run_transfer_seed(3, &TreeParams::fast_debug()).await;
}
