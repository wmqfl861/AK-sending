//! 信息性基准（非 T 编号验收项）：环回 chunk_size 调参扫描。
//!
//! 层级声明与如实边界：**环回调参只是引擎自身的参考**。QUIC 跑在本机
//! UDP 环回上，没有真实链路的 RTT/丢包/MTU/PHY 带宽约束，这里扫出的
//! “最快 chunk_size”只反映引擎在本机环回下的 CPU/拷贝/帧开销形状；
//! 真实网卡（千兆/万兆、跨机 RTT、不同 MTU）下的最优块尺寸很可能不同，
//! 真机调参以 L4 双机实测为准（见 TEST_PLAN 层级区分）。
//!
//! 变量控制：固定载荷 256 MiB = 16 x 16 MiB（进程内 LCG 确定性生成，
//! 无随机依赖、可复验，手法与 bench_throughput 相同）；in-flight window
//! 固定引擎默认（8，见 SendSpec::new，multistream 时并发流数受
//! min(window, 4) = 4 封顶）以免变量过多。扫描 chunk_size ∈
//! {64 KiB, 256 KiB, 1 MiB(引擎默认), 4 MiB} x 模式 ∈ {单流, 多流}，
//! 共 8 轮，轮序先小后大（64 KiB 起，同一块尺寸内先单流后多流）。
//!
//! 防污染：每轮独立的目的目录、设备密钥与 checkpoint 目录
//! （try_run_transfer_opts 每次调用全部新建）；源树只读，确定性生成
//! 一次后各轮共用。只断言字节正确性（树级逐字节比对 + 无 .akpart 残留
//! + 报告字节数一致），**不断言任何速度**：吞吐表是调参信息，不是门槛。
//!
//! 跑法（release-only，默认 #[ignore]）：
//! `cargo test --release -p ak-itests --test bench_chunk_sweep -- --ignored --nocapture`
#![forbid(unsafe_code)]

mod common;

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

use common::{mib_per_sec, run_bench_pass, write_file, BenchPass, Lcg, TransferOpts};

const FILE_COUNT: usize = 16;
const FILE_MIB: usize = 16;
const TOTAL_BYTES: u64 = (FILE_COUNT * FILE_MIB * 1024 * 1024) as u64;

/// Engine-default in-flight window (mirrors `SendSpec::new`), held fixed so
/// chunk size stays the only swept variable.
const ENGINE_DEFAULT_IN_FLIGHT_WINDOW: u32 = 8;

/// Sweep points in KiB, ordered small to large. 1024 KiB is the engine's
/// default chunk; 4096 KiB stays under ak-wire's 8 MiB chunk/frame budget.
const CHUNK_SWEEP_KIB: [u32; 4] = [64, 256, 1024, 4096];

/// Stream shapes per chunk size: single stream, then multistream.
const MODES: [(&str, bool); 2] = [("single-stream", false), ("multistream", true)];

fn chunk_from_kib(kib: u32) -> NonZeroU32 {
    NonZeroU32::new(kib * 1024).expect("sweep values are nonzero")
}

/// Deterministic 16 x 16 MiB tree (LCG bytes, reproducible across runs).
fn build_sweep_tree(root: &Path) {
    let mut lcg = Lcg::new(0x5EED_2026_1103);
    for index in 0..FILE_COUNT {
        write_file(
            &root.join(format!("sweep-{index:02}.bin")),
            &lcg.bytes(FILE_MIB * 1024 * 1024),
        );
    }
}

/// One sweep round: 256 MiB at (chunk, mode), fresh destination dir / keys /
/// checkpoints (per-call inside the harness). Asserts correctness only.
async fn run_round(src: &Path, chunk_kib: u32, mode: &str, multistream: bool) -> BenchPass {
    let opts = TransferOpts {
        multistream,
        in_flight_window: ENGINE_DEFAULT_IN_FLIGHT_WINDOW,
        chunk_size: Some(chunk_from_kib(chunk_kib)),
        ..TransferOpts::default()
    };
    let label = format!("{chunk_kib}KiB/{mode}");
    run_bench_pass(
        src,
        &label,
        &format!("{FILE_COUNT} x {FILE_MIB} MiB"),
        FILE_COUNT as u64,
        TOTAL_BYTES,
        600,
        opts,
    )
    .await
}

/// Informational chunk-size sweep: 4 chunk sizes x 2 stream shapes over one
/// deterministic 256 MiB tree, window pinned to the engine default. Prints a
/// per-round throughput table and a fastest-configuration conclusion line;
/// asserts byte correctness only (never speed). Release only (debug numbers
/// are meaningless).
#[ignore = "256 MiB x8 informational chunk-size sweep; release only: cargo test --release -p ak-itests --test bench_chunk_sweep -- --ignored --nocapture"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_size_sweep_256mib_loopback() {
    let base = tempfile::tempdir().expect("sweep tempdir");
    let src = base.path().join("sweep-src");
    let gen_started = Instant::now();
    build_sweep_tree(&src);
    let gen_elapsed = gen_started.elapsed();
    println!(
        "[sweep] generated {FILE_COUNT} x {FILE_MIB} MiB deterministic tree in {gen_elapsed:.2?}"
    );

    // Rounds ordered small chunk -> large chunk; within one chunk size,
    // single stream first, then multistream.
    let mut rounds: Vec<(u32, &str, BenchPass)> = Vec::new();
    for chunk_kib in CHUNK_SWEEP_KIB {
        for (mode, multistream) in MODES {
            let pass = run_round(&src, chunk_kib, mode, multistream).await;
            rounds.push((chunk_kib, mode, pass));
        }
    }

    // Informational throughput table (MiB/s; loopback numbers, see header).
    println!();
    println!(
        "[sweep table] {TOTAL_BYTES} bytes ({FILE_COUNT} x {FILE_MIB} MiB), window fixed at engine default ({ENGINE_DEFAULT_IN_FLIGHT_WINDOW}):"
    );
    println!(
        "{:<10} {:<14} {:>9} {:>8}",
        "chunk", "mode", "wall", "MiB/s"
    );
    for (chunk_kib, mode, pass) in &rounds {
        println!(
            "{:<10} {:<14} {:>9.2?} {:>8.1}",
            format!("{chunk_kib} KiB"),
            mode,
            pass.wall,
            mib_per_sec(TOTAL_BYTES, pass.wall)
        );
    }

    // Conclusion: fastest configuration by wall throughput (informational).
    let (best_chunk, best_mode, best_pass) = rounds
        .iter()
        .max_by(|a, b| {
            mib_per_sec(TOTAL_BYTES, a.2.wall).total_cmp(&mib_per_sec(TOTAL_BYTES, b.2.wall))
        })
        .expect("the sweep always runs rounds");
    println!(
        "[sweep conclusion] fastest: chunk {best_chunk} KiB + {best_mode} at {:.1} MiB/s (wall {:.2?}); loopback reference only - real-NIC optimum may differ",
        mib_per_sec(TOTAL_BYTES, best_pass.wall),
        best_pass.wall
    );
}
