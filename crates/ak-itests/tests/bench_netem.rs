//! 信息性基准（非 T 编号验收项）：模拟公网时延的环回单流吞吐 —— Linux
//! `tc netem` 在 `lo` 上加 50 / 100 ms 单向时延（CR-P2-04 收尾项，
//! docs/REVIEW_2026-11-11.zh-CN.md §4.2 改法末条）。
//!
//! 层级声明与如实边界：**netem 环回 = 环回 + 人工时延，不是真实公网**。
//! 双端仍各绑 127.0.0.1（引擎环回形态与 bench_throughput 相同），netem
//! 挂在 `lo` 的 root qdisc 上；数据与 ACK 两个方向都经过 `lo`，因此
//! RTT ≈ 2 × delay（50 ms → RTT≈100 ms、100 ms → RTT≈200 ms）。只加
//! 时延（无丢包/抖动/限速注入），保持确定性友好；`limit 100000` 只是
//! netem 队列容量（默认 1000 包 ≈ 1.2 MB，会被 CR-P2-04 的 8 MiB 窗
//! 撑爆而注入人为丢包），不是带宽参数。本基准检验的是 CR-P2-04 的
//! 传输参数（8 MiB 流/连接收发窗，ak-net `tls.rs` 常量表）在长 RTT 下
//! 能否保住单流吞吐，不构成真实网卡/公网吞吐主张（真机双机为 L4）。
//!
//! 权限与跳过策略：创建 netem qdisc 需要 root（CAP_NET_ADMIN）。本基准
//! 开头依次探测，任一不满足都**打印原因并跳过（不失败）**，其余深测
//! 不受影响：非 Linux 主机 → 跳过；`tc` 不可用 → 跳过；`lo` 已带 netem
//! （他人残留）→ 拒绝叠加、跳过；“加 1 ms 再删”写权限探针失败 → 跳过
//! （GitHub hosted runner 以非 root 跑 deep-tests 即走此路径）。真实
//! 执行需要特权环境：root shell（`sudo -i` 后运行）或特权/自托管
//! runner；本文件不尝试 sudo 提权。
//!
//! 清理纪律：正式整形期间由 RAII 守卫兜底（Drop 里 `tc qdisc del`，
//! 两次删除尝试后若 `lo` 仍被整形则带手工修复命令 panic——宁可不沉默
//! 也绝不把整形的接口留给后续测试）；测试末尾显式 drop 守卫并断言
//! `lo` 上已无 netem。
//!
//! 载荷与断言：进程内 LCG 确定性生成 8 x 8 MiB = 64 MiB（手法与
//! bench_throughput 相同）；单流、引擎默认 1 MiB chunk、in-flight
//! window 32（32 MiB，刻意大于 QUIC 8 MiB 窗，让 CR-P2-04 的传输窗
//! 成为瓶颈方）。先测无 netem 基线，再依次 50 / 100 ms。每轮断言字节
//! 正确性（run_bench_pass 内建：树级逐字节比对、无 `.akpart` 残留、
//! 字节数一致）；吞吐只断言宽松的坍塌级下限——基线 >= 8 MiB/s，
//! delay 50 ms（RTT≈100 ms）下限 4 MiB/s，delay 100 ms（RTT≈200 ms）
//! 下限 2 MiB/s。下限只为捕获“窗口退化到亚 MiB 级 / 逐块停等”式的
//! 坍塌回归（quinn 旧默认 1.25 MiB 流窗在 RTT 100/200 ms 仍有约
//! 12.5/6.25 MiB/s，甚至逐块停等也有 ~10/~5 MiB/s，都不会触线），
//! 不用于锁定 8 MiB 窗的具体收益；收益数字照实打印，与参考上限
//! （8 MiB 窗在 RTT 100/200 ms 约 80/40 MiB/s，未计慢启动爬坡与
//! 引擎开销）对照阅读。
//!
//! 如实记录：本文件尚无任何 Linux 特权环境实测数字（Windows 开发机
//! 只能验证跳过路径）；首个特权运行后把数字补记在此。
//!
//! 跑法（release-only，默认 #[ignore]）：
//! `cargo test --release -p ak-itests --test bench_netem -- --ignored --nocapture`
#![forbid(unsafe_code)]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use common::{mib_per_sec, run_bench_pass, write_file, BenchPass, Lcg, TransferOpts};

const FILE_COUNT: usize = 8;
const FILE_MIB: usize = 8;
const TOTAL_BYTES: u64 = (FILE_COUNT * FILE_MIB * 1024 * 1024) as u64;

/// App-level in-flight window (chunks of the engine-default 1 MiB): 32 MiB,
/// deliberately above CR-P2-04's 8 MiB QUIC windows so the transport window
/// — the thing this bench watches under delay — is the binding constraint.
const IN_FLIGHT_WINDOW: u32 = 32;

/// Per-pass budget; generous so a throughput collapse surfaces via the floor
/// assertions (with numbers printed) instead of a bare timeout.
const BUDGET_SECS: u64 = 600;

/// netem queue capacity in packets. The default 1000 (~1.2 MB of ~1200-byte
/// QUIC packets) would drop packets once CR-P2-04's 8 MiB windows open past
/// ~1.2 MB in flight — injecting artificial loss into the very quantity
/// being measured. 100000 packets holds the full 8 MiB window (~7k packets)
/// with 14x headroom; it is queue capacity, not a bandwidth knob.
const NETEM_LIMIT_PACKETS: u64 = 100_000;

/// (netem one-way delay in ms, collapse-tripping throughput floor in MiB/s).
/// Floors are sanity tripwires, not targets: at RTT ~= 2 x delay the old
/// quinn defaults (1.25 MiB stream window) still yield ~12.5 / ~6.25 MiB/s
/// and even a degenerate stop-and-wait per 1 MiB chunk yields ~10 / ~5 MiB/s,
/// so anything under these floors means the effective window collapsed below
/// ~0.4 MiB or the engine started serializing tiny pieces. The interesting
/// numbers live in the printed table, not in assertions.
const SHAPES: [(u64, f64); 2] = [(50, 4.0), (100, 2.0)];

/// Baseline floor: plain loopback of 64 MiB measured ~94-98 MiB/s
/// (bench_throughput, 2026-11-03); 8 MiB/s only trips if the engine itself
/// is broken on this box, in which case the netem numbers would be noise.
const BASELINE_FLOOR_MIB_PER_S: f64 = 8.0;

/// The loopback device every packet of this bench traverses (both the data
/// and the ACK direction), hence RTT ~= 2 x netem delay.
const LOOP_DEV: &str = "lo";

/// Run `tc` with `args`, returning (success, trimmed combined stdout+stderr
/// for messages). A spawn failure (no tc binary) reports as !ok with the OS
/// error text, so callers can treat "no tc" and "tc failed" alike.
fn run_tc(args: &[&str]) -> (bool, String) {
    match Command::new("tc").args(args).output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), text.trim().to_string())
        }
        Err(error) => (false, format!("spawning `tc` failed: {error}")),
    }
}

/// Print the skip reason and return normally: an environment gap (not Linux,
/// no tc, no privileges, a leftover qdisc) must never fail the deep-test
/// suite. The deep-tests job on GitHub-hosted runners ends here.
fn skip_bench(reason: &str) {
    println!("[netem bench] SKIP: {reason}");
}

/// RAII guard: while alive, a netem qdisc we added sits on `lo`; on drop it
/// is removed (twice if needed), and a still-shaped interface after both
/// attempts panics with the manual fix. Panicking in Drop during an unwind
/// aborts the process — deliberate: a silently left-shaped loopback
/// interface would poison every later test on this host.
struct NetemGuard;

impl Drop for NetemGuard {
    fn drop(&mut self) {
        if run_tc(&["qdisc", "del", "dev", LOOP_DEV, "root", "netem"]).0 {
            return;
        }
        if run_tc(&["qdisc", "del", "dev", LOOP_DEV, "root"]).0 {
            return;
        }
        // Accept only a verifiably unshaped interface (e.g. an external
        // `tc qdisc del` raced us); otherwise fail loudly with the fix.
        if let (true, text) = run_tc(&["qdisc", "show", "dev", LOOP_DEV]) {
            if !text.contains("netem") {
                return;
            }
        }
        panic!(
            "could not remove the netem qdisc from dev {LOOP_DEV}; the interface is left shaped - manual fix: tc qdisc del dev {LOOP_DEV} root netem"
        );
    }
}

/// Deterministic 8 x 8 MiB tree (LCG bytes, reproducible across runs).
fn build_bench_tree(root: &Path) {
    let mut lcg = Lcg::new(0xDE1A_2026_1112);
    for index in 0..FILE_COUNT {
        write_file(
            &root.join(format!("netem-{index:02}.bin")),
            &lcg.bytes(FILE_MIB * 1024 * 1024),
        );
    }
}

/// One timed 64 MiB single-stream pass (engine-default chunk, window 32).
/// Byte-correctness assertions (byte-identical tree, counts, no `.akpart`)
/// live inside `run_bench_pass`.
async fn run_pass(src: &Path, label: &str) -> BenchPass {
    let opts = TransferOpts {
        multistream: false,
        in_flight_window: IN_FLIGHT_WINDOW,
        ..TransferOpts::default()
    };
    run_bench_pass(
        src,
        label,
        &format!("{FILE_COUNT} x {FILE_MIB} MiB"),
        FILE_COUNT as u64,
        TOTAL_BYTES,
        BUDGET_SECS,
        opts,
    )
    .await
}

/// Single-stream throughput under simulated public-internet RTT (`tc netem`
/// on `lo`, delay-only 50/100 ms) versus a no-netem baseline. Linux + root
/// only: every environment gap (non-Linux host, missing `tc`, pre-existing
/// netem qdisc on `lo`, missing CAP_NET_ADMIN) prints its reason and skips
/// instead of failing the deep-test suite. Release only (debug numbers are
/// meaningless).
#[ignore = "Linux tc-netem throughput bench (CR-P2-04); needs root/CAP_NET_ADMIN and skips elsewhere; release only: cargo test --release -p ak-itests --test bench_netem -- --ignored --nocapture"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn netem_delay_50_100ms_64mib_loopback_throughput() {
    if std::env::consts::OS != "linux" {
        skip_bench(&format!(
            "host is {} - tc netem is Linux-only; simulated-RTT bench not applicable",
            std::env::consts::OS
        ));
        return;
    }

    // Read-only probe: tc present, and lo not already shaped by a leftover.
    let (show_ok, show_text) = run_tc(&["qdisc", "show", "dev", LOOP_DEV]);
    if !show_ok {
        skip_bench(&format!(
            "`tc qdisc show dev {LOOP_DEV}` failed (iproute2 missing or unusable): {show_text}"
        ));
        return;
    }
    if show_text.contains("netem") {
        skip_bench(&format!(
            "dev {LOOP_DEV} already carries a netem qdisc - refusing to stack on another bench's leftover; remove it (`tc qdisc del dev {LOOP_DEV} root netem`) and re-run. Current state: {show_text}"
        ));
        return;
    }

    // Write probe: shape lo with a minimal 1 ms netem, then remove it again.
    // This proves CAP_NET_ADMIN via the real qdisc add (not an euid guess -
    // root inside a container/userns qualifies too) AND that this box
    // accepts our exact removal command, before any transfer time is spent.
    // Skip on failure: unprivileged environments (GitHub hosted runners)
    // land here by design.
    let (probe_ok, probe_text) = run_tc(&[
        "qdisc", "add", "dev", LOOP_DEV, "root", "netem", "delay", "1ms",
    ]);
    if !probe_ok {
        let hint = if probe_text.contains("Operation not permitted") {
            " - creating a netem qdisc requires root (CAP_NET_ADMIN); run this bench in a root shell (sudo -i) or a privileged/self-hosted runner"
        } else {
            ""
        };
        skip_bench(&format!(
            "cannot shape dev {LOOP_DEV} (`tc qdisc add ... netem delay 1ms` failed: {probe_text}){hint}"
        ));
        return;
    }
    let (probe_del_ok, probe_del_text) =
        run_tc(&["qdisc", "del", "dev", LOOP_DEV, "root", "netem"]);
    assert!(
        probe_del_ok,
        "probe netem was added but cannot be removed - refusing to continue with dev {LOOP_DEV} shaped; manual fix: tc qdisc del dev {LOOP_DEV} root netem ({probe_del_text})"
    );

    // Bench: deterministic 64 MiB tree; baseline first, with lo unshaped.
    let base = tempfile::tempdir().expect("bench tempdir");
    let src = base.path().join("netem-src");
    let gen_started = Instant::now();
    build_bench_tree(&src);
    println!(
        "[netem bench] generated {FILE_COUNT} x {FILE_MIB} MiB deterministic tree in {:.2?}",
        gen_started.elapsed()
    );

    let baseline = run_pass(&src, "baseline").await;
    let baseline_tp = mib_per_sec(TOTAL_BYTES, baseline.wall);
    assert!(
        baseline_tp >= BASELINE_FLOOR_MIB_PER_S,
        "baseline loopback throughput {baseline_tp:.1} MiB/s is below the sanity floor {BASELINE_FLOOR_MIB_PER_S} MiB/s - engine or box is broken, netem numbers would be meaningless"
    );

    // Real shaping from here on; the guard removes the qdisc even on panic.
    let guard = NetemGuard;
    let mut shaped: Vec<(u64, BenchPass)> = Vec::new();
    for (index, &(delay_ms, floor_mib_per_s)) in SHAPES.iter().enumerate() {
        let rtt_ms = delay_ms * 2;
        let verb = if index == 0 { "add" } else { "change" };
        let (shape_ok, shape_text) = run_tc(&[
            "qdisc",
            verb,
            "dev",
            LOOP_DEV,
            "root",
            "netem",
            // Tokenization AND order matter: tc's netem grammar takes
            // `limit PACKETS` BEFORE `delay TIME`, each as a SEPARATE argv
            // element. The original code passed "limit 100000" as one
            // token — tc rejects a single argument containing a space with
            // `What is "limit 100000"?` (netem runs 43264263605 and
            // 43268998412; the write probe passed only because its tokens
            // were already separate).
            "limit",
            &NETEM_LIMIT_PACKETS.to_string(),
            "delay",
            &format!("{delay_ms}ms"),
        ]);
        assert!(
            shape_ok,
            "tc qdisc {verb} ... netem delay {delay_ms}ms failed although the write probe succeeded: {shape_text}"
        );
        // Deterministic proof the shaping is actually in force during the
        // pass (no reliance on "it got slower, so it must be shaped").
        let (state_ok, state_text) = run_tc(&["qdisc", "show", "dev", LOOP_DEV]);
        assert!(
            state_ok && state_text.contains("netem"),
            "netem qdisc not visible on dev {LOOP_DEV} during the run (shaping silently absent): {state_text}"
        );

        let pass = run_pass(&src, &format!("netem-{delay_ms}ms")).await;
        let tp = mib_per_sec(TOTAL_BYTES, pass.wall);
        assert!(
            tp >= floor_mib_per_s,
            "throughput at netem delay {delay_ms}ms (RTT ~{rtt_ms}ms) collapsed to {tp:.1} MiB/s, below the {floor_mib_per_s} MiB/s tripwire - flow-control windows regressed below the CR-P2-04 sizing (see ak-net tls.rs constant table)"
        );
        shaped.push((delay_ms, pass));
    }

    // Informational summary: the numbers are the point; only the floors
    // above are asserted.
    println!();
    println!(
        "[netem summary] {TOTAL_BYTES} bytes ({FILE_COUNT} x {FILE_MIB} MiB), single stream, engine-default chunk, in-flight window {IN_FLIGHT_WINDOW} (QUIC windows: CR-P2-04 8 MiB send/stream/connection):"
    );
    println!(
        "{:<14} {:>16} {:>8} {:>12}",
        "shape", "wall", "MiB/s", "vs baseline"
    );
    println!(
        "{:<14} {:>16.2?} {:>8.1} {:>12}",
        "no-netem", baseline.wall, baseline_tp, "1.00x"
    );
    for (delay_ms, pass) in &shaped {
        let tp = mib_per_sec(TOTAL_BYTES, pass.wall);
        println!(
            "{:<14} {:>16.2?} {:>8.1} {:>12.2}x",
            format!("delay {delay_ms}ms"),
            pass.wall,
            tp,
            tp / baseline_tp.max(f64::EPSILON)
        );
    }
    println!(
        "[netem reference] both directions traverse lo, so RTT ~ 2 x delay; single-stream caps at these RTTs: quinn defaults (1.25 MiB stream window) ~12.5 MiB/s @100ms / ~6.25 @200ms; CR-P2-04 8 MiB windows ~80 / ~40 MiB/s (before slow-start ramp and engine overhead). Loopback + artificial delay only - not a real-WAN claim."
    );

    // Cleanup: drop the guard, then verify lo is verifiably unshaped.
    drop(guard);
    let (post_ok, post_text) = run_tc(&["qdisc", "show", "dev", LOOP_DEV]);
    assert!(
        post_ok && !post_text.contains("netem"),
        "netem qdisc still present on dev {LOOP_DEV} after cleanup: {post_text}"
    );
    println!("[netem bench] cleanup verified: dev {LOOP_DEV} carries no netem qdisc");
}
