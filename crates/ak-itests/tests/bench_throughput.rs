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
//! 载荷：进程内确定性生成 64 × 8 MiB = 512 MiB（LCG，无随机依赖、可
//! 复验）。跑两遍引擎传输——`multistream=false` 与 `multistream=true`
//! ——打印各自吞吐（MiB/s、耗时）。只断言字节正确性（树级逐字节比对
//! 加无 .akpart 残留加报告字节数一致），**不断言任何速度下限**：速度
//! 数字是信息，不是门槛。
#![forbid(unsafe_code)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use ak_core::handle::ConflictPolicy;

use common::{
    assert_no_part_files, assert_trees_identical, try_run_transfer_opts, write_file, Lcg,
    TransferOpts,
};

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

fn mib_per_sec(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(f64::EPSILON)
}

/// One full 512 MiB transfer in the given stream shape; asserts correctness
/// only, prints the throughput numbers. Returns (wall, engine_send, engine_recv).
async fn run_one_pass(
    src: &Path,
    label: &str,
    multistream: bool,
) -> (Duration, Duration, Duration) {
    let dst = tempfile::tempdir().expect("dst tempdir");
    let opts = TransferOpts {
        multistream,
        in_flight_window: 32,
        ..TransferOpts::default()
    };
    let started = Instant::now();
    let raw = try_run_transfer_opts(src, dst.path(), 1024, ConflictPolicy::Fail, 600, opts).await;
    let wall = started.elapsed();

    let send_report = raw
        .send
        .unwrap_or_else(|e| panic!("[bench {label}] send failed: {e:?}"));
    let recv_report = raw
        .recv
        .unwrap_or_else(|e| panic!("[bench {label}] receive failed: {e:?}"));

    // Correctness only — never speed.
    assert_eq!(
        send_report.files, FILE_COUNT as u64,
        "[bench {label}] file count"
    );
    assert_eq!(send_report.bytes, TOTAL_BYTES, "[bench {label}] byte count");
    assert_eq!(
        recv_report.bytes, TOTAL_BYTES,
        "[bench {label}] receiver byte count"
    );
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(
        send_report.resumed_chunks, 0,
        "[bench {label}] fresh full transfer"
    );
    assert_trees_identical(src, dst.path());
    assert_no_part_files(dst.path());

    let wall_tp = mib_per_sec(TOTAL_BYTES, wall);
    let send_tp = mib_per_sec(TOTAL_BYTES, send_report.duration);
    let recv_tp = mib_per_sec(TOTAL_BYTES, recv_report.duration);
    println!(
        "[bench {label:>13}] {TOTAL_BYTES} bytes ({FILE_COUNT} x {FILE_MIB} MiB): wall {wall:.2?} ({wall_tp:.1} MiB/s) | engine send {:.2?} ({send_tp:.1} MiB/s) | engine recv {:.2?} ({recv_tp:.1} MiB/s)",
        send_report.duration, recv_report.duration,
    );
    (wall, send_report.duration, recv_report.duration)
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

    let (wall1, _send1, _recv1) = run_one_pass(&src, "single-stream", false).await;
    let (wall2, _send2, _recv2) = run_one_pass(&src, "multistream", true).await;

    let single = mib_per_sec(TOTAL_BYTES, wall1);
    let multi = mib_per_sec(TOTAL_BYTES, wall2);
    println!(
        "[bench summary] single-stream: {wall1:.2?} ({single:.1} MiB/s) | multistream: {wall2:.2?} ({multi:.1} MiB/s) | multistream/single ratio {:.2}x",
        multi / single.max(f64::EPSILON),
    );
}
