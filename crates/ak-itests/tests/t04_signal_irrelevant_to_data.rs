//! TEST_PLAN T04（补充形态，L2 环回）：数据面代码路径零信令依赖。
//!
//! 现有全部环回传输测试其实都"没有启动任何信令服务器"就在传文件；
//! 本测试把这一事实**显式化**为可断言的场景：
//!
//! 1. 先证明协调面在本测试中确实不存在——挑一个确定没有监听者的
//!    回环端口，`SignalClient::connect` 必须以连接拒绝（Io）失败；
//! 2. 随后在**从未启动任何协调/信令服务**的同一台机器上，直接执行
//!    引擎级 send+receive，断言传输正常完成、字节一致。
//!
//! 与 `t07_coordination_down.rs` 构成证明对：T07 证明"协调面曾经工作、
//! 死掉后数据面照常"；本文件证明"协调面从头到尾不存在、数据面照样
//! 工作"。两者合起来支撑 ADR-001 的架构不变量：协调面可有可无，从未
//! 处于文件数据路径。
//!
//! 如实边界：这是环回 L2 形态的代码路径证明，不是跨网段"两端不直连"
//! 的 T04 原场景（那需要真实网络故障注入，见 TEST_PLAN 覆盖映射）。
#![forbid(unsafe_code)]

mod common;

use std::net::{SocketAddr, TcpListener};

use ak_identity::DeviceKeyPair;
use ak_signal::client::{SignalClient, SignalError};

use common::{assert_no_part_files, assert_trees_identical, run_transfer, write_file, Lcg};

/// Pick a loopback port that provably has nothing listening behind it:
/// reserve an ephemeral port, then drop the listener.
fn dead_coordination_addr() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve ephemeral port");
    let port = listener.local_addr().expect("reserved port").port();
    drop(listener);
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// No coordination server exists at all: a signaling client cannot even
/// connect (connection refused), while the engine transfer — which by
/// construction never opens a signaling connection — completes normally.
#[tokio::test]
async fn engine_transfer_completes_with_no_signaling_server_ever_started() {
    // 1. The coordination plane provably does not exist on this machine
    //    (for this test): connecting to the dead port must fail with Io.
    let dead = dead_coordination_addr();
    let probe_key = DeviceKeyPair::generate();
    match SignalClient::connect(dead, &probe_key, "itests-t04-probe").await {
        // Connection refused: the coordination plane really does not exist.
        Err(SignalError::Io(_)) => {}
        Err(other) => panic!("expected SignalError::Io against dead port {dead}, got {other}"),
        Ok(_) => panic!("unexpectedly connected to dead coordination port {dead}"),
    }

    // 2. Never start any signaling/coordination service; run the engine
    //    transfer directly. It must complete byte-identically.
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t04-src");
    let mut lcg = Lcg::new(0x4004_0000_0000_0001);
    write_file(
        &src.join("note.txt"),
        b"t04: no signaling server was ever started for this transfer",
    );
    write_file(&src.join("data/one.bin"), &lcg.bytes(128 * 1024));
    write_file(&src.join("data/two.bin"), &lcg.bytes(32 * 1024));
    write_file(&src.join("empty"), &[]);

    let dst = tempfile::tempdir().unwrap();
    let outcome = run_transfer(&src, dst.path(), 8).await;

    // Both reports describe the same fully verified job. Report.files counts
    // manifest entries (files + dirs).
    let expected_entries = common::snapshot_tree(&src).entry_count();
    assert_eq!(outcome.send.files, expected_entries);
    assert_eq!(outcome.send.files, outcome.recv.files);
    assert_eq!(outcome.send.bytes, outcome.recv.bytes);
    assert_eq!(outcome.send.job_id, outcome.recv.job_id);
    assert_eq!(outcome.send.resumed_chunks, 0);
    assert_eq!(outcome.recv.resumed_chunks, 0);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());

    // The progress stream really reached Done (FinalAck true came first).
    assert!(
        outcome.events.contains(&ak_engine::Progress::Done),
        "progress stream must show Done"
    );
}
