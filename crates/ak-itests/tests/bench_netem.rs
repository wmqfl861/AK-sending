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
//! 载荷与断言：进程内 LCG 确定性生成 8 x 32 MiB = 256 MiB（手法与
//! bench_throughput 相同；2026-11-13 H3/CR-P3-13 起，原 64 MiB——复核
//! §5.2 的判定是"64 MiB 太短"：8 MiB 窗生效时慢启动爬坡（约 9–10 个
//! RTT）在 64 MiB 里占比过高，实测 RTT 数（约 26）与 8 MiB 生效预期
//! （约 18）的差额无法归因）；单流、引擎默认 1 MiB chunk、in-flight
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
//! 对照与证据口径（2026-11-13 H3 起；I1 批次 2026-11-13 把复核 §5.2
//! 点名的两处"不可达"补齐为可达）：
//! - **RTT 数列**：汇总表每档打印 `wall / RTT`——8 MiB 窗生效时
//!   256 MiB 约 42 个 RTT（爬坡 9–10 + 256/8），实测显著超出即说明
//!   瓶颈在别处（拥塞控制 / UDP 收发缓冲 / 引擎节奏）。
//! - **quinn `Connection::stats()`（cwnd/拥塞事件/丢包/收发包数）**：
//!   I1 起可达——`ChannelStats`（ak-net）扩了 cwnd 末值/拥塞事件/
//!   path 发包数/UDP 收发数据报数，引擎（ak-engine）在发送与接收
//!   两条路径的 `Report` 返回前用 `SendSpec`/`ReceiveSpec` 的
//!   `stats_sink` 钩子把末值快照递给本 bench（见 `PassStats` 与
//!   `print_quinn_stats`）。每档打印 sender/receiver 两行合一的
//!   stats 行（信息性，不断言）。quinn 0.11 没有收方向的包计数，
//!   收侧以 `rx_datagrams`（UDP 数据报数）近似——字段语义见
//!   `ak_net::ChannelStats` 文档。
//! - **默认窗口实测对照（A/B 臂）**：I1 起可达——`ak-net tls.rs` 提供
//!   隐藏实验开关 `AK_EXP_QUIC_WINDOW_KIB`（默认关，生产零变化；
//!   语义见 tls.rs），RTT≈200 ms 档在默认臂（CR-P2-04 8 MiB 常量
//!   表）之后设 `AK_EXP_QUIC_WINDOW_KIB=1280`（= quinn 默认
//!   1.25 MiB 流/连接窗）重跑一轮，打同一张表的 `A/B 1280KiB` 行与
//!   一条 `[netem A/B]` 对照行（吞吐+RTT 数）。**坍塌绊线断言只保留
//!   在默认臂上**，A/B 臂纯信息。tls.rs 在覆写生效时向 stderr 打
//!   一行 EXPERIMENTAL 公告（每次取值变化一行），runner 校验器据
//!   此确认 A/B 臂确实带覆写跑了。
//! - 主机级证据照旧：每档打印 `lo` 收发字节/包/丢包
//!   （`/proc/net/dev`）与 UDP 栈 `RcvbufErrors`/`SndbufErrors`
//!   （`/proc/net/snmp`）的档前/档后差值——复核点名的两个候选瓶颈
//!   （UDP 收发缓冲）正落在这里。
//!
//! 如实记录：本文件尚无任何 Linux 特权环境实测数字（Windows 开发机
//! 只能验证跳过路径）；首个特权运行后把数字补记在此。
//!
//! 跑法（release-only，默认 #[ignore]）：
//! `cargo test --release -p ak-itests --test bench_netem -- --ignored --nocapture`
#![forbid(unsafe_code)]

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ak_engine::{receive_with_addr, send, ChannelStatsSink, ReceiveSpec, SendSpec};
use ak_net::ChannelStats;
use common::{
    assert_no_part_files, assert_trees_identical, mib_per_sec, write_file, BenchPass, Lcg,
};

const FILE_COUNT: usize = 8;
const FILE_MIB: usize = 32;
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

/// Deterministic 8 x 32 MiB tree (LCG bytes, reproducible across runs).
fn build_bench_tree(root: &Path) {
    let mut lcg = Lcg::new(0xDE1A_2026_1112);
    for index in 0..FILE_COUNT {
        write_file(
            &root.join(format!("netem-{index:02}.bin")),
            &lcg.bytes(FILE_MIB * 1024 * 1024),
        );
    }
}

// ---------------------------------------------------------------------------
// Host-level per-tier evidence (H3, CR-P3-13 §5.2): the quinn
// `Connection::stats()` the review asked for is NOT reachable through the
// engine path (see the module docs); these /proc counters are the closest
// reachable facts, and they speak directly to two of the review's three
// candidate bottlenecks (UDP send/receive buffer pressure, packet loss).
// ---------------------------------------------------------------------------

/// Snapshot of the host counters this bench can read: `lo` interface
/// counters from `/proc/net/dev` and the UDP stack error counters from
/// `/proc/net/snmp`. Global (not per-connection) — meaningful here because
/// the deep-test process is the only heavy network user during a tier.
#[derive(Debug, Clone, Copy, Default)]
struct HostCounters {
    lo_rx_bytes: u64,
    lo_rx_packets: u64,
    lo_rx_drops: u64,
    lo_tx_bytes: u64,
    lo_tx_packets: u64,
    lo_tx_drops: u64,
    udp_in_errors: u64,
    udp_rcvbuf_errors: u64,
    udp_sndbuf_errors: u64,
}

impl HostCounters {
    /// `later - self`, saturating (counter resets read as zero delta).
    fn delta_to(&self, later: &HostCounters) -> HostCounters {
        HostCounters {
            lo_rx_bytes: later.lo_rx_bytes.saturating_sub(self.lo_rx_bytes),
            lo_rx_packets: later.lo_rx_packets.saturating_sub(self.lo_rx_packets),
            lo_rx_drops: later.lo_rx_drops.saturating_sub(self.lo_rx_drops),
            lo_tx_bytes: later.lo_tx_bytes.saturating_sub(self.lo_tx_bytes),
            lo_tx_packets: later.lo_tx_packets.saturating_sub(self.lo_tx_packets),
            lo_tx_drops: later.lo_tx_drops.saturating_sub(self.lo_tx_drops),
            udp_in_errors: later.udp_in_errors.saturating_sub(self.udp_in_errors),
            udp_rcvbuf_errors: later
                .udp_rcvbuf_errors
                .saturating_sub(self.udp_rcvbuf_errors),
            udp_sndbuf_errors: later
                .udp_sndbuf_errors
                .saturating_sub(self.udp_sndbuf_errors),
        }
    }
}

/// Read the current snapshot; `None` where the /proc files do not exist
/// (non-Linux — unreachable in practice: the bench already skipped) or do
/// not parse (either way the bench prints a note and continues without
/// them).
fn read_host_counters() -> Option<HostCounters> {
    let net_dev = std::fs::read_to_string("/proc/net/dev").ok()?;
    let snmp = std::fs::read_to_string("/proc/net/snmp").ok()?;
    let mut counters = HostCounters::default();
    let mut saw_lo = false;
    for line in net_dev.lines() {
        let Some((ifname, rest)) = line.split_once(':') else {
            continue;
        };
        if ifname.trim() != LOOP_DEV {
            continue;
        }
        saw_lo = true;
        // Interleaved layout: RX bytes packets errs drop … TX bytes packets
        // errs drop … (16 fields; drops at 3 and 11).
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        if fields.len() >= 12 {
            counters.lo_rx_bytes = fields[0];
            counters.lo_rx_packets = fields[1];
            counters.lo_rx_drops = fields[3];
            counters.lo_tx_bytes = fields[8];
            counters.lo_tx_packets = fields[9];
            counters.lo_tx_drops = fields[11];
        }
        break;
    }
    if !saw_lo {
        return None;
    }
    // The `Udp:` block is a header line followed by a value line.
    let mut udp_lines = snmp.lines().filter(|line| line.starts_with("Udp:"));
    let header: Vec<&str> = udp_lines.next()?.split_whitespace().skip(1).collect();
    let values: Vec<u64> = udp_lines
        .next()?
        .split_whitespace()
        .skip(1)
        .map(|field| field.parse().unwrap_or(0))
        .collect();
    let named = |name: &str| {
        header
            .iter()
            .position(|entry| *entry == name)
            .and_then(|index| values.get(index).copied())
            .unwrap_or(0)
    };
    counters.udp_in_errors = named("InErrors");
    counters.udp_rcvbuf_errors = named("RcvbufErrors");
    counters.udp_sndbuf_errors = named("SndbufErrors");
    Some(counters)
}

/// Print one tier's counter delta (`before` -> `after`); a `None` snapshot
/// prints the reachability note instead (never fails the bench).
fn print_host_counters(label: &str, before: Option<HostCounters>, after: Option<HostCounters>) {
    match (before, after) {
        (Some(before), Some(after)) => {
            let d = before.delta_to(&after);
            println!(
                "[netem bench] {label} host counters (tier delta): lo rx {} B / {} pkts ({} drops), lo tx {} B / {} pkts ({} drops); udp InErrors +{}/RcvbufErrors +{}/SndbufErrors +{}",
                d.lo_rx_bytes,
                d.lo_rx_packets,
                d.lo_rx_drops,
                d.lo_tx_bytes,
                d.lo_tx_packets,
                d.lo_tx_drops,
                d.udp_in_errors,
                d.udp_rcvbuf_errors,
                d.udp_sndbuf_errors,
            );
        }
        _ => println!(
            "[netem bench] {label} host counters: /proc/net/dev or /proc/net/snmp unreadable on this host - no tier deltas printed"
        ),
    }
}

/// A/B arm override value (2026-11-13 review §5.2): 1280 KiB = quinn's
/// default 1.25 MiB stream/connection windows, applied via the tls.rs
/// hidden experimental knob `AK_EXP_QUIC_WINDOW_KIB` (see the module docs
/// and `crates/ak-net/src/tls.rs`).
const AB_WINDOW_KIB: &str = "1280";

/// Tier label of the A/B arm everywhere it prints (stats line, host
/// counters, summary-table row, comparison line). Deliberately distinct
/// from the three strict sample labels (`no-netem`, `delay 50ms`,
/// `delay 100ms`) the runner validator counts — a label collision would
/// make that validator see "two samples" and fail the run.
const AB_LABEL: &str = "A/B 1280KiB";

/// Environment variable of the tls.rs window override (mirrored here for
/// the set/remove around the A/B pass; the definition lives in ak-net).
const AB_WINDOW_ENV: &str = "AK_EXP_QUIC_WINDOW_KIB";

/// Both sides' end-of-transfer quinn stats for one pass. Captured through
/// the engine's `stats_sink` hook (I1, 2026-11-13): the engine owns the
/// `quinn::Connection`, so the bench cannot read `Connection::stats()`
/// directly — the sink receives a `ChannelStats` snapshot right before
/// each side's `Report` is returned (cwnd = end-of-transfer value).
struct PassStats {
    send: ChannelStats,
    recv: ChannelStats,
}

/// One timed 256 MiB single-stream pass (engine-default chunk, app window
/// [`IN_FLIGHT_WINDOW`]), capturing both sides' end-of-transfer quinn
/// stats via the engine's `stats_sink`.
///
/// Local pass runner (not `common::run_bench_pass`): the shared harness
/// builds its own specs and cannot attach the engine's stats sink without
/// edits outside this file's domain. The spec shape mirrors
/// `common::try_run_transfer_opts` exactly (sender 0.0.0.0:0, receiver
/// 127.0.0.1:0, single stream, engine-default chunk, fresh identities and
/// checkpoint dirs per pass), and every byte-correctness assertion of
/// `run_bench_pass` is kept: file/byte counts on both reports, matching
/// job ids, a fresh (non-resumed) transfer, byte-identical trees, no
/// `.akpart` residue. Speed is never asserted here (floors live at the
/// tier level, on the default arm only).
///
/// Also asserts the stats plumbing itself: a successful pass must have
/// notified BOTH sinks — a silent plumbing regression fails loudly instead
/// of printing empty stats lines.
async fn run_pass(src: &Path, label: &str) -> (BenchPass, PassStats) {
    // Wall clock starts before identity generation, exactly like
    // `common::run_bench_pass` starts before `try_run_transfer_opts`, so
    // this bench's numbers stay comparable with the H3/CR-P3-13 runs.
    let started = Instant::now();
    let dst = tempfile::tempdir().expect("dst tempdir");
    let sender_key = ak_identity::DeviceKeyPair::generate();
    let receiver_key = ak_identity::DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    // Fresh checkpoint dirs per pass so passes never share transfer state.
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        // Peer placeholder; replaced by the receiver's reported address.
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_key.fingerprint(),
        sender_bundle,
    );
    send_spec.device_name = "ak-itests-netem-sender".into();
    send_spec.in_flight_window = IN_FLIGHT_WINDOW;
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let mut recv_spec = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv_spec.device_name = "ak-itests-netem-receiver".into();
    // Port 0 and loopback only (same shape as the shared harness; never the
    // default 0.0.0.0:7100).
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    // Stats capture: one slot per side; the engine notifies each sink once,
    // right before its Report is returned.
    let send_slot: Arc<Mutex<Option<ChannelStats>>> = Arc::new(Mutex::new(None));
    let recv_slot: Arc<Mutex<Option<ChannelStats>>> = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&send_slot);
        send_spec.stats_sink = Some(ChannelStatsSink::new(move |stats| {
            *slot.lock().expect("send stats slot") = Some(*stats);
        }));
    }
    {
        let slot = Arc::clone(&recv_slot);
        recv_spec.stats_sink = Some(ChannelStatsSink::new(move |stats| {
            *slot.lock().expect("recv stats slot") = Some(*stats);
        }));
    }

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver =
        tokio::spawn(async move { receive_with_addr(recv_spec, &|_| {}, addr_tx).await });
    let recv_local_addr = addr_rx.await.expect("receiver must report its address");
    send_spec.peer = recv_local_addr;
    let sender = tokio::spawn(async move { send(send_spec, &|_| {}).await });

    let joined = tokio::time::timeout(Duration::from_secs(BUDGET_SECS), async {
        (receiver.await, sender.await)
    })
    .await
    .unwrap_or_else(|_| panic!("[bench {label}] pass did not finish within {BUDGET_SECS}s"));
    let wall = started.elapsed();

    let (recv_result, send_result) = joined;
    let send_report = send_result
        .expect("sender task must not panic")
        .unwrap_or_else(|e| panic!("[bench {label}] send failed: {e:?}"));
    let recv_report = recv_result
        .expect("receiver task must not panic")
        .unwrap_or_else(|e| panic!("[bench {label}] receive failed: {e:?}"));

    // Correctness only — never speed (mirrors run_bench_pass exactly).
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

    // Plumbing invariant: a successful pass must have produced BOTH stats
    // snapshots (the engine notifies on the Report path; a None here is a
    // regression in the sink plumbing, not a transfer failure).
    let stats = PassStats {
        send: send_slot
            .lock()
            .expect("send stats slot")
            .take()
            .expect("[bench {label}] sender stats sink never fired"),
        recv: recv_slot
            .lock()
            .expect("recv stats slot")
            .take()
            .expect("[bench {label}] receiver stats sink never fired"),
    };

    let wall_tp = mib_per_sec(TOTAL_BYTES, wall);
    let send_tp = mib_per_sec(TOTAL_BYTES, send_report.duration);
    let recv_tp = mib_per_sec(TOTAL_BYTES, recv_report.duration);
    println!(
        "[bench {label:>13}] {TOTAL_BYTES} bytes ({FILE_COUNT} x {FILE_MIB} MiB): wall {wall:.2?} ({wall_tp:.1} MiB/s) | engine send {:.2?} ({send_tp:.1} MiB/s) | engine recv {:.2?} ({recv_tp:.1} MiB/s)",
        send_report.duration, recv_report.duration,
    );
    (
        BenchPass {
            wall,
            send: send_report.duration,
            recv: recv_report.duration,
        },
        stats,
    )
}

/// Print one tier's quinn stats line (I1, informational — never asserted at
/// this layer; the runner validator only checks the line is present and
/// numeric). One line carries both sides; quinn 0.11 has no received-PACKET
/// counter, so the receive side reports `rx_datagrams` (see
/// `ak_net::ChannelStats`).
fn print_quinn_stats(tier: &str, stats: &PassStats) {
    println!(
        "[netem bench] {tier} quinn stats: sender cwnd={}B congestion_events={} lost_packets={} sent_packets={} tx_bytes={} rx_datagrams={} | receiver cwnd={}B congestion_events={} lost_packets={} sent_packets={} tx_bytes={} rx_datagrams={}",
        stats.send.cwnd_bytes,
        stats.send.congestion_events,
        stats.send.lost_packets,
        stats.send.sent_packets,
        stats.send.tx_bytes,
        stats.send.rx_datagrams,
        stats.recv.cwnd_bytes,
        stats.recv.congestion_events,
        stats.recv.lost_packets,
        stats.recv.sent_packets,
        stats.recv.tx_bytes,
        stats.recv.rx_datagrams,
    );
}

/// Single-stream throughput under simulated public-internet RTT (`tc netem`
/// on `lo`, delay-only 50/100 ms) versus a no-netem baseline. Linux + root
/// only: every environment gap (non-Linux host, missing `tc`, pre-existing
/// netem qdisc on `lo`, missing CAP_NET_ADMIN) prints its reason and skips
/// instead of failing the deep-test suite. Release only (debug numbers are
/// meaningless).
#[ignore = "Linux tc-netem throughput bench (CR-P2-04); needs root/CAP_NET_ADMIN and skips elsewhere; release only: cargo test --release -p ak-itests --test bench_netem -- --ignored --nocapture"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn netem_delay_50_100ms_256mib_loopback_throughput() {
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

    // Bench: deterministic 256 MiB tree; baseline first, with lo unshaped.
    let base = tempfile::tempdir().expect("bench tempdir");
    let src = base.path().join("netem-src");
    let gen_started = Instant::now();
    build_bench_tree(&src);
    println!(
        "[netem bench] generated {FILE_COUNT} x {FILE_MIB} MiB deterministic tree in {:.2?}",
        gen_started.elapsed()
    );

    let baseline_before = read_host_counters();
    let (baseline, baseline_stats) = run_pass(&src, "baseline").await;
    print_host_counters("baseline", baseline_before, read_host_counters());
    print_quinn_stats("baseline", &baseline_stats);
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

        let tier_before = read_host_counters();
        let (pass, pass_stats) = run_pass(&src, &format!("netem-{delay_ms}ms")).await;
        print_host_counters(
            &format!("delay {delay_ms}ms"),
            tier_before,
            read_host_counters(),
        );
        print_quinn_stats(&format!("delay {delay_ms}ms"), &pass_stats);
        let tp = mib_per_sec(TOTAL_BYTES, pass.wall);
        assert!(
            tp >= floor_mib_per_s,
            "throughput at netem delay {delay_ms}ms (RTT ~{rtt_ms}ms) collapsed to {tp:.1} MiB/s, below the {floor_mib_per_s} MiB/s tripwire - flow-control windows regressed below the CR-P2-04 sizing (see ak-net tls.rs constant table)"
        );
        shaped.push((delay_ms, pass));
    }

    // A/B arm (I1, 2026-11-13 review §5.2): re-run the RTT≈200 ms tier
    // (the last SHAPES entry still shapes `lo`) with quinn's DEFAULT
    // 1.25 MiB windows via the tls.rs hidden experimental override — the
    // same product transport-config path, one runtime knob. The collapse
    // tripwire above is deliberately DEFAULT-ARM ONLY: this arm exists to
    // measure how much slower the old defaults are, so its numbers are
    // informational (printed, never asserted). The env flip is safe here:
    // single test process (--test-threads=1 in the runner, the only test
    // in this binary), strictly between passes, and each pass builds
    // fresh endpoints/connections that read the variable at config time.
    let ab_delay_ms = SHAPES
        .last()
        .map(|&(delay_ms, _)| delay_ms)
        .expect("SHAPES is non-empty");
    let (ab_state_ok, ab_state_text) = run_tc(&["qdisc", "show", "dev", LOOP_DEV]);
    assert!(
        ab_state_ok && ab_state_text.contains("netem"),
        "netem qdisc not visible on dev {LOOP_DEV} before the A/B pass (shaping silently absent): {ab_state_text}"
    );
    std::env::set_var(AB_WINDOW_ENV, AB_WINDOW_KIB);
    let ab_before = read_host_counters();
    let (ab_pass, ab_stats) = run_pass(&src, "netem-100ms-win1280kib").await;
    print_host_counters(AB_LABEL, ab_before, read_host_counters());
    print_quinn_stats(AB_LABEL, &ab_stats);
    // The env is process-global: restore the constant-table windows so no
    // later code in this process inherits the override. tls.rs announces
    // both the override and this restore on stderr (once per state
    // change); the runner validator pins the override announcement.
    std::env::remove_var(AB_WINDOW_ENV);

    // Informational summary: the numbers are the point; only the floors
    // above are asserted.
    println!();
    println!(
        "[netem summary] {TOTAL_BYTES} bytes ({FILE_COUNT} x {FILE_MIB} MiB), single stream, engine-default chunk, in-flight window {IN_FLIGHT_WINDOW} (QUIC windows: CR-P2-04 8 MiB send/stream/connection; A/B arm re-runs delay {ab_delay_ms}ms with quinn default 1.25 MiB windows via {AB_WINDOW_ENV}={AB_WINDOW_KIB}):"
    );
    println!(
        "{:<14} {:>16} {:>8} {:>8} {:>12}",
        "shape", "wall", "MiB/s", "RTTs", "vs baseline"
    );
    println!(
        "{:<14} {:>16.2?} {:>8.1} {:>8} {:>12}",
        "no-netem", baseline.wall, baseline_tp, "-", "1.00x"
    );
    for (delay_ms, pass) in &shaped {
        let tp = mib_per_sec(TOTAL_BYTES, pass.wall);
        // The review's key metric (2026-11-13 §5.2): the tier's cost in
        // RTTs. With a fully effective 8 MiB window, 256 MiB needs about
        // 42 RTTs (slow-start ramp 9-10 + 256/8); ~26-ish per-64-MiB
        // numbers of the first run meant something else was binding.
        let rtts = pass.wall.as_secs_f64() / (*delay_ms as f64 / 1000.0 * 2.0);
        println!(
            "{:<14} {:>16.2?} {:>8.1} {:>8.1} {:>12.2}x",
            format!("delay {delay_ms}ms"),
            pass.wall,
            tp,
            rtts,
            tp / baseline_tp.max(f64::EPSILON)
        );
    }
    // A/B row: same columns, same shaping as the delay-100ms tier, quinn
    // default windows. Informational — no tripwire on this arm.
    let ab_tp = mib_per_sec(TOTAL_BYTES, ab_pass.wall);
    let ab_rtts = ab_pass.wall.as_secs_f64() / (ab_delay_ms as f64 / 1000.0 * 2.0);
    println!(
        "{:<14} {:>16.2?} {:>8.1} {:>8.1} {:>12.2}x",
        AB_LABEL,
        ab_pass.wall,
        ab_tp,
        ab_rtts,
        ab_tp / baseline_tp.max(f64::EPSILON)
    );
    // Direct A/B comparison against the delay-100ms default arm (the last
    // shaped row): throughput + RTT count of both window configurations
    // under identical shaping, and the defaults/8MiB throughput ratio.
    let (default_delay_ms, default_pass) = shaped
        .last()
        .expect("the shaped tiers include the A/B reference tier");
    let default_tp = mib_per_sec(TOTAL_BYTES, default_pass.wall);
    let default_rtts = default_pass.wall.as_secs_f64() / (*default_delay_ms as f64 / 1000.0 * 2.0);
    println!(
        "[netem A/B] delay {default_delay_ms}ms (RTT ~{}ms): CR-P2-04 8 MiB windows {default_tp:.1} MiB/s ({default_rtts:.1} RTTs) vs quinn defaults 1.25 MiB ({AB_WINDOW_ENV}={AB_WINDOW_KIB}) {ab_tp:.1} MiB/s ({ab_rtts:.1} RTTs) -> defaults/8MiB throughput ratio {:.2}x (informational; the collapse tripwire is asserted on the default arm only)",
        default_delay_ms * 2,
        ab_tp / default_tp.max(f64::EPSILON),
    );
    println!(
        "[netem reference] both directions traverse lo, so RTT ~ 2 x delay; single-stream caps at these RTTs: quinn defaults (1.25 MiB stream window) ~12.5 MiB/s @100ms / ~6.25 @200ms; CR-P2-04 8 MiB windows ~80 / ~40 MiB/s (ideal, before slow-start ramp and engine overhead); ramp-inclusive 256 MiB estimate with an effective 8 MiB window ~42 RTTs (~61 / ~30 MiB/s). The A/B row/line measures the default-window case directly on this box; per-tier quinn stats (cwnd end value, congestion events, loss/packet counters, both sides) print above each tier - cwnd plateauing far below the window or congestion events climbing there is the review's other-bottleneck signal. Loopback + artificial delay only - not a real-WAN claim."
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
