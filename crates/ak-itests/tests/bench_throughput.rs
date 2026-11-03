//! 信息性基准（非 T 编号验收项）：环回引擎吞吐 —— 单流 vs multistream。
//!
//! 层级声明与如实边界：**环回吞吐不等于网卡吞吐**。QUIC 跑在本机
//! UDP 环回上，无真实链路 RTT/丢包/PHY 带宽限制；这里的数字只用于
//! 对比同一环境下单流与多流的引擎自身效率，并为后续优化提供可复现
//! 的参照。千兆/万兆网卡的真实双机实测不在本文件范围内（见
//! TEST_PLAN 层级区分：真机验证为 L4）。
//!
//! 跑法（release-only，默认 #[ignore]）：
//! `cargo test --release -p ak-itests -- --ignored --nocapture`
//!
//! 最近一次实测（2026-11-03，第六轮，本机空闲无并行构建）：单流
//! 5.46s / 93.9 MiB/s，多流 5.23s / 97.9 MiB/s（比值 1.04x）；第三轮
//! （同日，与其他代理构建并行争用 CPU）旧值：单流 97.9 / 多流 103.3
//! （1.06x）。两组相差 ~4 MiB/s，量级与结论不变（环回下多流收益有限）；
//! 数字是参照信息，不是门槛。
//!
//! 载荷：进程内确定性生成 64 × 8 MiB = 512 MiB（LCG，无随机依赖、可
//! 复验）。跑两遍引擎传输——`multistream=false` 与 `multistream=true`
//! ——打印各自吞吐（MiB/s、耗时）。只断言字节正确性（树级逐字节比对
//! 加无 .akpart 残留加报告字节数一致），**不断言任何速度下限**：速度
//! 数字是信息，不是门槛。
#![forbid(unsafe_code)]

mod common;

use std::path::Path;
use std::time::Instant;

use common::{mib_per_sec, run_bench_pass, write_file, BenchPass, Lcg, TransferOpts};

const FILE_COUNT: usize = 64;
const FILE_MIB: usize = 8;
const TOTAL_BYTES: u64 = (FILE_COUNT * FILE_MIB * 1024 * 1024) as u64;

/// Deterministic 64 x 8 MiB tree (LCG bytes, reproducible across runs).
fn build_bench_tree(root: &Path) {
    let mut lcg = Lcg::new(0xBEDC_2026_1103);
    for index in 0..FILE_COUNT {
        write_file(
            &root.join(format!("bench-{index:02}.bin")),
            &lcg.bytes(FILE_MIB * 1024 * 1024),
        );
    }
}

/// One full 512 MiB transfer in the given stream shape: engine-default
/// 1 MiB chunk (no `chunk_size` override), in-flight window 32. Asserts
/// correctness only; the timings are informational.
async fn run_one_pass(src: &Path, label: &str, multistream: bool) -> BenchPass {
    let opts = TransferOpts {
        multistream,
        in_flight_window: 32,
        ..TransferOpts::default()
    };
    run_bench_pass(
        src,
        label,
        &format!("{FILE_COUNT} x {FILE_MIB} MiB"),
        FILE_COUNT as u64,
        TOTAL_BYTES,
        600,
        opts,
    )
    .await
}

/// Informational throughput comparison, single stream vs multistream, on one
/// deterministic 512 MiB tree. Release only (debug numbers are meaningless).
#[ignore = "512 MiB x2 informational throughput bench; release only: cargo test --release -p ak-itests -- --ignored --nocapture"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_vs_multistream_512mib_loopback_throughput() {
    let base = tempfile::tempdir().expect("bench tempdir");
    let src = base.path().join("bench-src");
    let gen_started = Instant::now();
    build_bench_tree(&src);
    let gen_elapsed = gen_started.elapsed();
    println!(
        "[bench] generated {FILE_COUNT} x {FILE_MIB} MiB deterministic tree in {gen_elapsed:.2?}"
    );

    let pass1 = run_one_pass(&src, "single-stream", false).await;
    let pass2 = run_one_pass(&src, "multistream", true).await;

    let single = mib_per_sec(TOTAL_BYTES, pass1.wall);
    let multi = mib_per_sec(TOTAL_BYTES, pass2.wall);
    println!(
        "[bench summary] single-stream: {w1:.2?} ({single:.1} MiB/s) | multistream: {w2:.2?} ({multi:.1} MiB/s) | multistream/single ratio {ratio:.2}x",
        w1 = pass1.wall,
        w2 = pass2.wall,
        ratio = multi / single.max(f64::EPSILON),
    );
}
