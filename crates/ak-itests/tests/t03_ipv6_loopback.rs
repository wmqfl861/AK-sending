//! TEST_PLAN T03（IPv6 环回形态，L2）：外网 IPv4/IPv6 可直连 —— 双端认证
//! 通过，原件摘要一致。
//!
//! 如实边界：本文件验证的是**IPv6 环回（::1）**上的完整引擎传输与路径
//! 判定，属于 T03 的地址族补充形态。环回 ::1 **不等于**真实公网 IPv6
//! 直连——没有 NAT64/DSCP/防火墙/前缀授权等真实网络因素，本文件的成功
//! 不能被引用为"跨网络 IPv6 直连已验证"。真实双机/跨网段验证仍属 T03
//! 的未覆盖部分（见 TEST_PLAN 自动化覆盖映射）。
//!
//! 三个断言层：
//! 1. L1 决策：`classify(::1) == LocalDirect`（回环按定义本地直连）。
//! 2. L2 通道：双方各绑 `[::1]:0`，QUIC+TLS 握手成功，两侧
//!    `DirectChannel::path()` 均为 LocalDirect，指纹互钉。
//! 3. L2 引擎：`SendSpec.bind` 与 `ReceiveSpec.bind` 都设为
//!    `[::1]:0`，引擎传输完成、字节一致、接收端实际绑定地址确为 ::1
//!    （证明整条数据路径真的跑在 IPv6 环回上，而非回退到 127.0.0.1）。
#![forbid(unsafe_code)]

mod common;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use ak_core::handle::ConflictPolicy;
use ak_core::policy::PathKind;
use ak_identity::DeviceKeyPair;
use ak_net::DirectEndpoint;

use common::{
    assert_no_part_files, assert_trees_identical, try_run_transfer_opts, write_file, TransferOpts,
};

fn v6_loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
}

/// L1 decision layer: loopback is local-direct by definition, with or without
/// any interface knowledge; fe80::/10 link-local is local-direct too.
#[test]
fn ipv6_loopback_and_link_local_classify_local_direct() {
    assert_eq!(
        ak_net::classify::classify(IpAddr::V6(Ipv6Addr::LOCALHOST), &[]),
        PathKind::LocalDirect
    );
    assert_eq!(
        ak_net::classify::classify(
            IpAddr::V6(Ipv6Addr::from([0xfe80, 0, 0, 0, 1, 1, 1, 1])),
            &[]
        ),
        PathKind::LocalDirect
    );
    assert!(ak_net::classify::is_ipv6_link_local(Ipv6Addr::from([
        0xfe80, 0, 0, 0, 0, 0, 0, 9
    ])));
}

/// L2 channel layer: both endpoints bind `[::1]:0`, the QUIC+TLS handshake
/// succeeds over IPv6 loopback, identity pins hold, and both sides classify
/// the path LocalDirect (loopback family, not a v4 fallback).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipv6_loopback_channel_handshake_pins_identity_and_reports_local_direct() {
    let server_key = DeviceKeyPair::generate();
    let client_key = DeviceKeyPair::generate();
    let server_bundle = ak_identity::tls::make_self_signed(&server_key);
    let client_bundle = ak_identity::tls::make_self_signed(&client_key);

    let server = DirectEndpoint::bind(v6_loopback(0), &server_bundle)
        .await
        .expect("[::1]:0 server bind (Windows IPv6 loopback must be available)");
    let client = DirectEndpoint::bind(v6_loopback(0), &client_bundle)
        .await
        .expect("[::1]:0 client bind");

    let server_addr = server.local_addr().expect("server local addr");
    assert_eq!(server_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_ne!(server_addr.port(), 0);
    let client_addr = client.local_addr().expect("client local addr");
    assert_eq!(client_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));

    let accept_task = tokio::spawn(async move { server.accept(&|_| true).await });
    let client_channel = client
        .connect(server_addr, &server_bundle.fingerprint)
        .await
        .expect("QUIC+TLS handshake over [::1]");
    let server_channel = accept_task
        .await
        .expect("accept task panicked")
        .expect("server accept over [::1]");

    // Identity pins in both directions over the v6 path.
    assert_eq!(
        client_channel.peer_fingerprint(),
        &server_bundle.fingerprint
    );
    assert_eq!(
        server_channel.peer_fingerprint(),
        &client_bundle.fingerprint
    );
    // Loopback (v6 family) classifies as a local direct path on both sides.
    assert_eq!(client_channel.path(), PathKind::LocalDirect);
    assert_eq!(server_channel.path(), PathKind::LocalDirect);
}

/// L2 engine layer: a full engine transfer with both binds on `[::1]:0`.
/// Completes, byte-identical, no residue, and the receiver's real bound
/// address is an IPv6 loopback one — the whole data path rode ::1.
#[tokio::test]
async fn ipv6_loopback_engine_transfer_is_byte_identical() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t03-v6-src");
    write_file(
        &src.join("readme.txt"),
        b"t03 ipv6 loopback: both engine binds are [::1]:0",
    );
    write_file(
        &src.join("blob/512k.bin"),
        &common::Lcg::new(0x3003_0000_0000_0001).bytes(512 * 1024),
    );
    write_file(
        &src.join("blob/empty-dir-marker.txt"),
        &common::Lcg::new(0x3003_0000_0000_0002).bytes(3 * 1024),
    );
    let dst = tempfile::tempdir().unwrap();

    let opts = TransferOpts {
        send_bind: v6_loopback(0),
        recv_bind: v6_loopback(0),
        ..TransferOpts::default()
    };
    let raw = try_run_transfer_opts(&src, dst.path(), 16, ConflictPolicy::Fail, 120, opts).await;
    let send_report = raw.send.expect("send over [::1] must succeed");
    let recv_report = raw.recv.expect("receive over [::1] must succeed");

    // The data path really was IPv6 loopback end to end.
    assert_eq!(raw.recv_local_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert!(
        raw.recv_local_addr.port() != 0,
        "receiver must report a concrete ephemeral port"
    );

    assert_eq!(
        send_report.files,
        common::snapshot_tree(&src).entry_count(),
        "Report.files counts manifest entries (files + dirs)"
    );
    assert_eq!(send_report.bytes, recv_report.bytes);
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(send_report.resumed_chunks, 0);
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());
}
