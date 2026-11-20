use super::*;
use ak_identity::DeviceKeyPair;
use std::fs;
use std::path::Path;

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn count_entries(dir: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    fn walk(dir: &Path, files: &mut u64, bytes: &mut u64) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                walk(&entry.path(), files, bytes);
            } else if !entry
                .path()
                .to_string_lossy()
                .ends_with(ak_storage::PART_SUFFIX)
            {
                *files += 1;
                *bytes += meta.len();
            }
        }
    }
    walk(dir, &mut files, &mut bytes);
    (files, bytes)
}

fn specs(src: &Path, dst: &Path, chunk_kb: u32) -> (SendSpec, ReceiveSpec, Fingerprint) {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();
    let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_fp,
        sender_bundle,
    );
    send_spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
    send_spec.checkpoint_dir = checkpoint.clone();
    let mut recv = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.chunk_size = send_spec.chunk_size;
    recv.checkpoint_dir = checkpoint;
    (send_spec, recv, receiver_fp)
}

fn noop(_progress: Progress) {}

/// Loopback transfer under the default policy: the step-0 gate passes
/// LocalDirect on both sides and the frozen flow runs unchanged — this
/// is the default-policy gate integration.
#[tokio::test]
async fn loopback_directory_transfer_is_byte_identical() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("a.txt"), b"hello ak");
    write(
        &src.path().join("nested/deep/b.bin"),
        &(0..50_000u32).map(|i| i as u8).collect::<Vec<_>>(),
    );
    fs::create_dir_all(src.path().join("empty/dir")).unwrap();

    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
    send_spec.device_name = "sender-test".into();
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let sender_spec = send_spec.clone();
    let sender = tokio::spawn(async move { send(sender_spec, &noop).await });
    let recv_outcome = receiver.await.unwrap();
    let send_outcome = sender.await.unwrap();
    if let Err(error) = &recv_outcome {
        eprintln!("receiver error: {error:?}");
    }
    let report = send_outcome.expect("send should complete");
    let recv_report = recv_outcome.expect("receive should complete");

    assert_eq!(report.files, 6); // 2 files + 4 directory entries (nested, deep, empty, empty/dir)
    assert_eq!(report.bytes, recv_report.bytes);
    assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
    assert_eq!(
        fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
        (0..50_000u32).map(|i| i as u8).collect::<Vec<_>>()
    );
    assert!(dst.path().join("empty/dir").is_dir());
    let (files, bytes) = count_entries(dst.path());
    assert_eq!((files, bytes), (2, 8 + 50_000));
    assert!(report.duration.as_nanos() > 0);
}

/// I1 (2026-11-13 review §5.2): the end-of-transfer transport-stats
/// hook (`SendSpec`/`ReceiveSpec` `stats_sink`). After a completed
/// loopback transfer both sinks must have fired with snapshots of a
/// LIVE connection — positive congestion window and packet/byte
/// counts on the sender (cwnd = the end-of-transfer value, taken
/// right before the `Report` is returned), positive receive counts on
/// the receiver — and the byte counts must reflect the payload scale.
/// Default-off check: no sink on a spec changes nothing (every other
/// transfer test in this module runs sink-less).
#[tokio::test]
async fn stats_sink_reports_end_of_transfer_snapshots() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..50_000u32).map(|i| i as u8).collect();
    write(&src.path().join("stats.bin"), &payload);

    let (mut send_spec, mut recv_spec, _) = specs(src.path(), dst.path(), 16);
    let send_slot: std::sync::Arc<std::sync::Mutex<Option<ak_net::ChannelStats>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let recv_slot: std::sync::Arc<std::sync::Mutex<Option<ak_net::ChannelStats>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    send_spec.stats_sink = Some({
        let slot = std::sync::Arc::clone(&send_slot);
        ChannelStatsSink::new(move |stats| *slot.lock().unwrap() = Some(*stats))
    });
    recv_spec.stats_sink = Some({
        let slot = std::sync::Arc::clone(&recv_slot);
        ChannelStatsSink::new(move |stats| *slot.lock().unwrap() = Some(*stats))
    });

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv_spec, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let sender_spec = send_spec.clone();
    let sender = tokio::spawn(async move { send(sender_spec, &noop).await });
    sender.await.unwrap().expect("send should complete");
    receiver.await.unwrap().expect("receive should complete");

    let send_stats = send_slot
        .lock()
        .unwrap()
        .take()
        .expect("sender stats sink must fire on a successful send");
    let recv_stats = recv_slot
        .lock()
        .unwrap()
        .take()
        .expect("receiver stats sink must fire on a successful receive");
    assert!(
        send_stats.cwnd_bytes > 0,
        "sender cwnd is the live end-of-transfer congestion window: {send_stats:?}"
    );
    assert!(
        send_stats.sent_packets > 0 && send_stats.tx_datagrams > 0,
        "sender sent packets/datagrams on the transfer path: {send_stats:?}"
    );
    assert!(
        send_stats.tx_bytes >= 50_000,
        "sender carried the 50k payload inside its QUIC bytes: {send_stats:?}"
    );
    assert!(
        recv_stats.rx_datagrams > 0 && recv_stats.rx_bytes >= 50_000,
        "receiver observed the payload datagrams: {recv_stats:?}"
    );
    assert_eq!(fs::read(dst.path().join("stats.bin")).unwrap(), payload);
}

/// CR-P1-05 (review 2026-11-11 §4.5): a send whose spec ADOPTED a
/// pre-bound dual-stack socket dials through it — the plain-v4 peer
/// rides the `::ffff:` bridge of `connect_bounded` (the pre-adoption
/// fast-fail would have rejected this family combination), and the
/// full Offer→FinalAck path completes byte-identically. The port
/// equation (dial leaves from the adopted socket's port) is proven at
/// the ak-net transport layer; this test pins the ENGINE wiring.
#[tokio::test]
async fn adopted_socket_send_completes_through_the_dual_stack_dial() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("adopted.bin"), b"same-socket dial");
    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
    let adopted = match ak_net::holepunch::bind_dual_stack_udp(0) {
        Ok(socket) => socket,
        Err(reason) => {
            eprintln!("skipping: dual-stack UDP bind unavailable ({reason})");
            return;
        }
    };
    send_spec.adopt_socket(adopted);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    assert!(send_spec.peer.is_ipv4(), "the receiver binds v4 loopback");
    let report = send(send_spec, &noop)
        .await
        .expect("adopted send completes");
    receiver.await.unwrap().expect("receive completes");
    assert_eq!(report.files, 1);
    assert_eq!(
        fs::read(dst.path().join("adopted.bin")).unwrap(),
        b"same-socket dial"
    );
}

#[tokio::test]
async fn resume_skips_verified_prefix_without_resending() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    // 8 KiB file with 1 KiB chunks gives an 8-chunk prefix to resume from.
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    write(&src.path().join("resume.bin"), &payload);
    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
    let checkpoint = send_spec.checkpoint_dir.clone();

    // Simulate an interrupted earlier run: 3 verified chunks on disk.
    let target = LocalSaveTarget::open(dst.path()).unwrap();
    let source_probe = LocalSource::open(&send_spec.source_root, send_spec.chunk_size).unwrap();
    let entry = &source_probe.manifest().entries[0];
    let mut sink = target
        .prepare_file_with_chunk_size(entry, ConflictPolicy::Fail, send_spec.chunk_size)
        .unwrap();
    for chunk in 0..3u64 {
        let mut buf = vec![0u8; 1024];
        let len = source_probe.read_chunk(0, chunk, &mut buf).unwrap();
        sink.write_chunk(chunk, &buf[..len]).unwrap();
    }
    drop(sink);

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop).await.expect("send should complete");
    let recv_report = receiver.await.unwrap().expect("receive should complete");

    assert_eq!(
        report.resumed_chunks, 3,
        "sender should skip 3 verified chunks"
    );
    assert_eq!(recv_report.resumed_chunks, 3);
    assert_eq!(fs::read(dst.path().join("resume.bin")).unwrap(), payload);
    let _ = FileCheckpointStore::open(&checkpoint).map(|s| s.remove(&report.job_id));
}

/// T04/T10 analog: the receiver dies mid-transfer. The sender must fail
/// (never fall back to anything), and the checkpoint plus part file must
/// survive so a second run resumes instead of restarting from zero.
#[tokio::test]
async fn mid_transfer_disconnect_keeps_checkpoint_and_resumes() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
    write(&src.path().join("mid.bin"), &payload);
    let (send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 16 chunks of 1 KiB
    let checkpoint_dir = send_spec.checkpoint_dir.clone();
    let part_path = dst
        .path()
        .join(format!("mid.bin{}", ak_storage::PART_SUFFIX));

    let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
    let progress = std::sync::Arc::new(move |event: Progress| {
        if let Progress::ChunkReceived { .. } = event {
            chunks_tx.send_modify(|count| *count += 1);
        }
    });
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = std::sync::Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
    };
    let mut first_spec = send_spec.clone();
    first_spec.peer = addr_rx.await.unwrap();
    let sender = tokio::spawn(async move { send(first_spec, &noop).await });

    // Kill the receiver once a few chunks are durable.
    while *chunks_rx.borrow() < 4 {
        chunks_rx.changed().await.unwrap();
    }
    receiver.abort();
    let send_result = sender.await.unwrap();
    assert!(
        matches!(
            send_result,
            Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
        ),
        "sender must fail without any fallback, got {send_result:?}"
    );
    assert!(part_path.exists(), "part file must survive the disconnect");
    let store = FileCheckpointStore::open(&checkpoint_dir).unwrap();
    assert!(
        !store.list().unwrap().is_empty(),
        "checkpoint must survive the disconnect"
    );

    // Second run: same deterministic job id resumes and completes.
    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop)
        .await
        .expect("resumed send should complete");
    receiver
        .await
        .unwrap()
        .expect("resumed receive should complete");

    assert!(
        report.resumed_chunks >= 1,
        "second run should resume, got {}",
        report.resumed_chunks
    );
    assert_eq!(fs::read(dst.path().join("mid.bin")).unwrap(), payload);
    assert!(!part_path.exists(), "committed file leaves no part behind");
}

/// A v4 endpoint dialing a v6 peer must fail fast instead of hanging in
/// QUIC's retry loop (defect found by the IPv6 scenario tests).
#[tokio::test]
async fn connect_family_mismatch_fails_fast_instead_of_hanging() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("f.txt"), b"x");
    let (mut send_spec, _, _) = specs(src.path(), dst.path(), 16);
    send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0)); // IPv4 endpoint
    send_spec.peer = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 9)); // IPv6 peer, nothing listening
    let started = Instant::now();
    let err = send(send_spec, &noop)
        .await
        .expect_err("family mismatch must fail");
    assert!(
        matches!(
            &err,
            EngineError::Connect(ak_net::ConnectError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
        ),
        "got: {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "must fail fast, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn wrong_pin_fails_closed_before_any_transfer() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("secret.txt"), b"must not leak");
    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
    // Pin a fingerprint that is NOT the receiver's.
    send_spec.pinned = DeviceKeyPair::generate().fingerprint();
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let err = send(send_spec, &noop)
        .await
        .expect_err("wrong pin must fail");
    assert!(matches!(err, EngineError::Connect(_)), "got: {err:?}");
    // Nothing may be written even though the offer was never reached.
    assert_eq!(count_entries(dst.path()), (0, 0));
    let _ = receiver.await;
}

#[tokio::test]
async fn receiver_authorize_false_rejects_the_sender() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("x.txt"), b"x");
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 16);
    recv.authorize = Box::new(|_fp| false);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();
    send_spec.peer = addr;
    let err = send(send_spec, &noop)
        .await
        .expect_err("unauthorized sender must fail");
    // The TLS handshake itself succeeds (client cert is validly
    // self-signed); rejection surfaces on first use as a lost stream.
    assert!(
        matches!(err, EngineError::Connect(_) | EngineError::Stream(_)),
        "got: {err:?}"
    );
    let recv_err = receiver.await.unwrap().expect_err("receiver must reject");
    assert!(matches!(
        recv_err,
        EngineError::Connect(ak_net::ConnectError::Unauthorized)
    ));
    assert_eq!(count_entries(dst.path()), (0, 0));
}

#[tokio::test]
async fn job_id_is_stable_for_source_and_sensitive_to_content() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"content-a");
    let manifest = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
        .unwrap()
        .manifest()
        .clone();
    let id_a = job_id_for("f.bin", &manifest);
    let id_b = job_id_for("f.bin", &manifest);
    assert_eq!(id_a, id_b);

    write(&src.path().join("f.bin"), b"content-b");
    let manifest2 = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
        .unwrap()
        .manifest()
        .clone();
    assert_ne!(id_a, job_id_for("f.bin", &manifest2));
}

// ------------------------------------------------------------------
// Storage-full precheck (pure decision)
// ------------------------------------------------------------------

/// Minimal FileEntry for the name-gate decision tests (only rel_path
/// matters to [`windows_unsavable_entry`]).
fn name_entry(rel: &str) -> FileEntry {
    FileEntry {
        rel_path: std::path::PathBuf::from(rel),
        is_dir: false,
        size: 1,
        mtime_unix_ms: 0,
        digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
        whole_digest: [0u8; 32],
        chunk_digests: Vec::new(),
    }
}

#[test]
fn windows_unsavable_entry_flags_each_rule_at_any_depth() {
    for rel in [
        "what?now.bin",
        "a<b.txt",
        "dir>n/x.txt",
        "CONIN$.log",
        "COM¹.bin",
    ] {
        assert!(
            windows_unsavable_entry(&[name_entry(rel)]).is_some(),
            "{rel} must be flagged"
        );
    }
    // The offending path and a readable reason ride the message.
    let flagged = windows_unsavable_entry(&[name_entry("docs/what?now.bin")]).unwrap();
    assert!(flagged.contains("docs/what?now.bin"), "{flagged}");
    assert!(flagged.contains('?'), "{flagged}");
}

#[test]
fn windows_unsavable_entry_passes_clean_manifests_and_finds_offenders() {
    assert!(windows_unsavable_entry(&[]).is_none());
    // Clean names, including cases near the reserved-set boundaries.
    assert!(windows_unsavable_entry(&[
        name_entry("报告-最终.txt"),
        name_entry("myconin$/note.txt"), // substring, not a reserved stem
        name_entry("COM10/serial.log"),  // past the COM1-9 range
    ])
    .is_none());
    // Reserved stems are caught at any depth, with any extension.
    assert!(windows_unsavable_entry(&[name_entry("sub/CON.txt.ok")]).is_some());
}

#[test]
fn check_headroom_passes_unknown_available() {
    assert!(check_headroom(None, 0).is_ok());
    assert!(check_headroom(None, u64::MAX).is_ok());
}

#[test]
fn check_headroom_passes_exact_fit() {
    assert!(check_headroom(Some(0), 0).is_ok());
    assert!(check_headroom(Some(1000), 1000).is_ok());
}

#[test]
fn check_headroom_rejects_shortage_with_the_real_numbers() {
    assert!(matches!(
        check_headroom(Some(999), 1000),
        Err(EngineError::StorageFull {
            available: 999,
            needed: 1000
        })
    ));
    assert!(matches!(
        check_headroom(Some(0), 1),
        Err(EngineError::StorageFull {
            available: 0,
            needed: 1
        })
    ));
}

// The receive-path integration (a real volume reporting less free space
// than the job size) cannot be constructed on a real local disk in this
// environment: Windows temp directories have no quota mechanism and the
// free-space probe reads the live volume. Only the pure decision above
// is covered by automated tests; the Error-frame and
// "failed_storage_full" checkpoint wiring is untested here by design
// (no faked numbers, no faked filesystems). See the round-3 report.

// ------------------------------------------------------------------
// Policy gate (pre-transport route authorization)
// ------------------------------------------------------------------

#[test]
fn authorize_path_local_direct_passes_even_with_local_only_policy() {
    let local_only = DirectPolicy {
        allow_internet: false,
        allow_cellular: false,
    };
    assert!(authorize_path(&DirectPolicy::default(), PathKind::LocalDirect).is_ok());
    assert!(authorize_path(&local_only, PathKind::LocalDirect).is_ok());
}

#[test]
fn authorize_path_internet_direct_follows_allow_internet() {
    assert!(
        authorize_path(&DirectPolicy::default(), PathKind::InternetDirect).is_ok(),
        "the default policy allows internet"
    );
    let local_only = DirectPolicy {
        allow_internet: false,
        allow_cellular: false,
    };
    assert!(matches!(
        authorize_path(&local_only, PathKind::InternetDirect),
        Err(EngineError::RouteForbidden(PathKind::InternetDirect))
    ));
}

#[test]
fn authorize_path_rejects_relay_and_unknown_even_when_permissive() {
    for path in [PathKind::Relay, PathKind::Unknown] {
        assert!(
            matches!(
                authorize_path(&DirectPolicy::default(), path),
                Err(EngineError::RouteForbidden(refused)) if refused == path
            ),
            "Relay/Unknown must always be refused, got a pass for {path:?}"
        );
    }
}

/// The documented API contract for integrators (e.g. CLI flags):
/// untouched specs carry `allow_internet = true, allow_cellular = false`.
#[test]
fn specs_default_to_the_standard_direct_policy() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let (send_spec, recv_spec, _) = specs(src.path(), dst.path(), 16);
    for policy in [send_spec.policy, recv_spec.policy] {
        assert!(policy.allow_internet, "default must allow internet");
        assert!(!policy.allow_cellular, "default must refuse cellular");
    }
}

/// `allow_internet = false` must not affect a loopback transfer: the
/// loopback peer classifies `LocalDirect` (ak-net::classify treats
/// loopback as always local), which no policy can lock out. Both sides
/// carry the local-only policy here.
///
/// The InternetDirect-refused branch of the wiring cannot be produced
/// naturally on a single machine: every address a test can actually
/// connect to here classifies LocalDirect, and `DirectChannel` exposes
/// no public constructor to forge a path (by design). That branch is
/// covered by the pure unit branches above instead.
#[tokio::test]
async fn local_only_policy_still_completes_loopback_transfer() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    write(&src.path().join("local-only.bin"), &payload);
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 4);
    let local_only = DirectPolicy {
        allow_internet: false,
        allow_cellular: false,
    };
    send_spec.policy = local_only;
    recv.policy = local_only;

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop)
        .await
        .expect("local-only policy must not affect a LocalDirect loopback");
    let recv_report = receiver
        .await
        .unwrap()
        .expect("local-only policy must not affect a LocalDirect loopback");
    assert_eq!(report.files, 1);
    assert_eq!(report.bytes, recv_report.bytes);
    assert_eq!(
        fs::read(dst.path().join("local-only.bin")).unwrap(),
        payload
    );
}

// ------------------------------------------------------------------
// Multi-root send
// ------------------------------------------------------------------

/// Three-root fixture: two directory trees (nesting plus empty dirs) and
/// one standalone file. Returns (tree_a, tree_b, loose, deep_payload).
fn three_root_fixture(
    base: &Path,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    Vec<u8>,
) {
    let tree_a = base.join("photos");
    write(&tree_a.join("a.txt"), b"hello ak");
    let deep: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
    write(&tree_a.join("nested/deep/b.bin"), &deep);
    fs::create_dir_all(tree_a.join("empty/dir")).unwrap();
    let tree_b = base.join("docs");
    write(&tree_b.join("notes.md"), b"notes");
    fs::create_dir_all(tree_b.join("solo")).unwrap();
    let loose = base.join("readme.txt");
    write(&loose, b"loose file");
    (tree_a, tree_b, loose, deep)
}

#[tokio::test]
async fn multi_root_transfer_merges_three_roots_byte_identical() {
    let base = tempfile::tempdir().unwrap();
    let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
    let dst = tempfile::tempdir().unwrap();

    let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 16);
    send_spec.add_root(tree_b.clone());
    send_spec.add_root(loose.clone());

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop)
        .await
        .expect("multi-root send completes");
    let recv_report = receiver
        .await
        .unwrap()
        .expect("multi-root receive completes");

    // Primary root first, extras in add order; the receiver reports none.
    assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
    assert_eq!(recv_report.roots, Vec::<String>::new());
    // 4 files + 5 directory entries (photos/{empty,empty/dir,nested,
    // nested/deep} + docs/solo); a single-file root adds no directory.
    assert_eq!(report.files, 9);
    assert_eq!(report.bytes, 8 + 30_000 + 5 + 10);
    assert_eq!(report.bytes, recv_report.bytes);

    assert_eq!(
        fs::read(dst.path().join("photos/a.txt")).unwrap(),
        b"hello ak"
    );
    assert_eq!(
        fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
        deep
    );
    assert!(dst.path().join("photos/empty/dir").is_dir());
    assert_eq!(
        fs::read(dst.path().join("docs/notes.md")).unwrap(),
        b"notes"
    );
    assert!(dst.path().join("docs/solo").is_dir());
    // A single-file root lands under a top-level directory of its own name.
    assert_eq!(
        fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
        b"loose file"
    );
    let (files, bytes) = count_entries(dst.path());
    assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
}

#[tokio::test]
async fn multi_root_multistream_transfer_is_byte_identical() {
    let base = tempfile::tempdir().unwrap();
    let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
    let dst = tempfile::tempdir().unwrap();

    let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 1);
    send_spec.add_root(tree_b.clone());
    send_spec.add_root(loose.clone());
    send_spec.multistream = true;
    send_spec.in_flight_window = 4;

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop)
        .await
        .expect("multi-root multistream send completes");
    let recv_report = receiver
        .await
        .unwrap()
        .expect("multi-root multistream receive completes");

    assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
    assert_eq!(report.files, 9);
    assert_eq!(report.bytes, recv_report.bytes);
    assert_eq!(
        fs::read(dst.path().join("photos/a.txt")).unwrap(),
        b"hello ak"
    );
    assert_eq!(
        fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
        deep
    );
    assert!(dst.path().join("photos/empty/dir").is_dir());
    assert_eq!(
        fs::read(dst.path().join("docs/notes.md")).unwrap(),
        b"notes"
    );
    assert!(dst.path().join("docs/solo").is_dir());
    assert_eq!(
        fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
        b"loose file"
    );
    let (files, bytes) = count_entries(dst.path());
    assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
}

#[tokio::test]
async fn multi_root_rejects_duplicate_root_names() {
    let base = tempfile::tempdir().unwrap();
    let a = base.path().join("data");
    let b = base.path().join("other").join("data");
    write(&a.join("one.txt"), b"one");
    write(&b.join("two.txt"), b"two");

    let dst = tempfile::tempdir().unwrap();
    let (mut send_spec, _recv, _) = specs(&a, dst.path(), 16);
    send_spec.add_root(b);
    // No receiver is spawned: the job must fail before any connect.
    let err = send(send_spec, &noop)
        .await
        .expect_err("duplicate root names must fail the job");
    assert!(
        matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
        "got: {err:?}"
    );
    assert_eq!(count_entries(dst.path()), (0, 0));
}

#[test]
fn multi_root_manifest_is_deterministic_and_prefixed() {
    let base = tempfile::tempdir().unwrap();
    let (tree_a, tree_b, loose, _) = three_root_fixture(base.path());
    let dst = tempfile::tempdir().unwrap();
    let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
    spec.add_root(tree_b.clone());
    spec.add_root(loose.clone());

    let first = SourceSet::open(&spec).unwrap();
    let second = SourceSet::open(&spec).unwrap();
    assert!(first.multi);
    assert_eq!(first.display_name(), "multi");
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(
        job_id_for("multi", &first.manifest),
        job_id_for("multi", &second.manifest)
    );
    let rels: Vec<String> = first
        .manifest
        .entries
        .iter()
        .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(rels.len(), 9);
    assert!(rels.iter().all(|r| {
        r.starts_with("photos/") || r.starts_with("docs/") || r.starts_with("readme.txt/")
    }));
    // Roots stay contiguous so parents always precede children.
    let first_docs = rels.iter().position(|r| r.starts_with("docs/")).unwrap();
    assert!(rels[..first_docs].iter().all(|r| r.starts_with("photos/")));
}

#[test]
fn single_root_source_set_is_the_frozen_manifest() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("a.txt"), b"x");
    let plain = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap()).unwrap();
    let (spec, _, _) = specs(src.path(), src.path(), 1);
    let set = SourceSet::open(&spec).unwrap();
    assert!(!set.multi);
    assert_eq!(set.manifest, plain.manifest().clone());
    assert_eq!(set.display_name(), plain.root_name());
}

#[test]
fn empty_root_directory_becomes_a_top_level_entry() {
    let base = tempfile::tempdir().unwrap();
    let tree_a = base.path().join("photos");
    write(&tree_a.join("a.txt"), b"hello ak");
    let empty_root = base.path().join("nothing");
    fs::create_dir_all(&empty_root).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
    spec.add_root(empty_root.clone());
    let set = SourceSet::open(&spec).unwrap();
    let rels: Vec<String> = set
        .manifest
        .entries
        .iter()
        .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(rels, vec!["photos/a.txt", "nothing"]);
    assert!(set.manifest.entries[1].is_dir);
}

// ------------------------------------------------------------------
// Concurrent receive (serve_concurrently)
// ------------------------------------------------------------------

/// serve_concurrently keeps its signature minimal (no addr_tx), so
/// tests probe a free UDP port exactly the way the receiver will bind
/// one; same-process UDP rebinding is immediate on every platform we
/// run the suite on.
fn free_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    socket.local_addr().unwrap().port()
}

fn concurrent_receive_spec(dst: &Path, checkpoint: &Path) -> (ReceiveSpec, Fingerprint) {
    let receiver_key = DeviceKeyPair::generate();
    let mut recv = ReceiveSpec::new(
        dst.to_path_buf(),
        ak_identity::tls::make_self_signed(&receiver_key),
    );
    recv.bind = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
    recv.checkpoint_dir = checkpoint.to_path_buf();
    (recv, receiver_key.fingerprint())
}

fn concurrent_send_spec(
    src: PathBuf,
    peer: SocketAddr,
    pinned: Fingerprint,
    chunk_kb: u32,
    checkpoint: &Path,
) -> SendSpec {
    let mut spec = SendSpec::new(
        src,
        peer,
        pinned,
        ak_identity::tls::make_self_signed(&DeviceKeyPair::generate()),
    );
    spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
    spec.checkpoint_dir = checkpoint.to_path_buf();
    spec
}

/// Two independent senders (own keys and certificates, own source
/// trees, one pinned receiver) land concurrently in one
/// serve_concurrently(2) receiver: both jobs complete, bytes are
/// identical on both sides, and every progress event carries its task
/// ordinal (0/1, accept order) with each ordinal observing its own
/// Connected..Done sequence.
#[tokio::test]
async fn two_senders_concurrently_complete_into_one_receiver() {
    let src_a = tempfile::tempdir().unwrap();
    let src_b = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload_a: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
    let payload_b: Vec<u8> = (0..24_000u32).map(|i| (i % 251) as u8).collect();
    write(&src_a.path().join("alpha/a.bin"), &payload_a);
    write(&src_b.path().join("beta/b.bin"), &payload_b);
    fs::create_dir_all(src_b.path().join("beta/empty")).unwrap();

    let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
    let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
    let spec_a = concurrent_send_spec(
        src_a.path().to_path_buf(),
        recv.bind,
        receiver_fp,
        1,
        &checkpoint,
    );
    let spec_b = concurrent_send_spec(
        src_b.path().to_path_buf(),
        recv.bind,
        receiver_fp,
        4,
        &checkpoint,
    );

    let seen: Arc<std::sync::Mutex<Vec<(u64, Progress)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let progress = move |index: u64, event: Progress| log.lock().unwrap().push((index, event));
    let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &progress).await });
    // Let the listener bind before the senders dial (the port was
    // probed, not reported).
    tokio::time::sleep(Duration::from_millis(150)).await;

    let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
    let sender_b = tokio::spawn(async move { send(spec_b, &noop).await });
    let report_a = sender_a.await.unwrap().expect("sender A completes");
    let report_b = sender_b.await.unwrap().expect("sender B completes");
    let outcomes = receiver.await.unwrap();

    assert_eq!(outcomes.len(), 2);
    let mut reports = Vec::with_capacity(2);
    for (index, outcome) in outcomes.into_iter().enumerate() {
        reports.push(outcome.unwrap_or_else(|error| panic!("job {index} failed: {error:?}")));
    }
    // Slots are in accept order, which is racy; compare as a set.
    let mut by_bytes: Vec<(u64, u64)> = reports.iter().map(|r| (r.bytes, r.files)).collect();
    by_bytes.sort_unstable();
    let mut expected = vec![(24_000, 3), (30_000, 2)];
    expected.sort_unstable();
    assert_eq!(by_bytes, expected, "reports: {reports:?}");
    assert_eq!(report_a.bytes, 30_000);
    assert_eq!(report_b.bytes, 24_000);

    assert_eq!(fs::read(dst.path().join("alpha/a.bin")).unwrap(), payload_a);
    assert_eq!(fs::read(dst.path().join("beta/b.bin")).unwrap(), payload_b);
    assert!(dst.path().join("beta/empty").is_dir());
    let (files, bytes) = count_entries(dst.path());
    assert_eq!((files, bytes), (2, 30_000 + 24_000));

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().all(|(index, _)| *index < 2),
        "task ordinals out of range: {seen:?}"
    );
    for index in 0..2u64 {
        assert!(
            seen.iter()
                .any(|(i, e)| *i == index && matches!(e, Progress::Connected { .. })),
            "no Connected event for task {index}: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|(i, e)| *i == index && matches!(e, Progress::Done)),
            "no Done event for task {index}: {seen:?}"
        );
    }
}

/// One sender aborted mid-transfer must not sink the other job: the
/// Vec holds exactly one Ok (the healthy job, bytes intact) and one Err
/// (the aborted one), and the aborted job's part file survives with its
/// verified prefix for resume.
#[tokio::test]
async fn one_failing_sender_does_not_sink_the_other_job() {
    let src_a = tempfile::tempdir().unwrap();
    let src_b = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload_a: Vec<u8> = (0..20_000u32).map(|i| (i % 249) as u8).collect();
    let payload_b: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
    write(&src_a.path().join("solid/a.bin"), &payload_a);
    write(&src_b.path().join("doomed/b.bin"), &payload_b);

    let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
    let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
    let spec_a = concurrent_send_spec(
        src_a.path().to_path_buf(),
        recv.bind,
        receiver_fp,
        4,
        &checkpoint,
    );
    let mut spec_b = concurrent_send_spec(
        src_b.path().to_path_buf(),
        recv.bind,
        receiver_fp,
        1,
        &checkpoint,
    );
    // Park after every chunk: the abort below then lands deterministically
    // mid-transfer.
    spec_b.in_flight_window = 1;

    let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &|_, _| {}).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
    let (sent_tx, mut sent_rx) = tokio::sync::watch::channel(0u64);
    let sender_b = {
        let progress = std::sync::Arc::new(move |event: Progress| {
            if let Progress::ChunkSent { .. } = event {
                sent_tx.send_modify(|count| *count += 1);
            }
        });
        tokio::spawn(async move { send(spec_b, &*progress).await })
    };
    while *sent_rx.borrow() < 2 {
        sent_rx.changed().await.unwrap();
    }
    sender_b.abort();

    let report_a = sender_a.await.unwrap().expect("healthy sender completes");
    let outcomes = receiver.await.unwrap();
    assert_eq!(outcomes.len(), 2);
    let oks: Vec<&Report> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    let errs: Vec<&EngineError> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();
    assert_eq!(oks.len(), 1, "outcomes: {outcomes:?}");
    assert_eq!(errs.len(), 1, "outcomes: {outcomes:?}");
    assert_eq!(oks[0].bytes, report_a.bytes);

    assert_eq!(fs::read(dst.path().join("solid/a.bin")).unwrap(), payload_a);
    // The aborted job leaves its verified part file for resume, and no
    // committed final file.
    assert!(
        dst.path()
            .join(format!("doomed/b.bin{}", ak_storage::PART_SUFFIX))
            .exists(),
        "part file must survive the aborted sender"
    );
    assert!(!dst.path().join("doomed/b.bin").exists());
}

/// The no-hang contract: a slot whose sender never arrives fails with
/// IdleTimeout once the (per-accept) idle budget expires, instead of
/// blocking forever.
#[tokio::test]
async fn serve_concurrently_times_out_unfilled_slots_instead_of_hanging() {
    let dst = tempfile::tempdir().unwrap();
    let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
    let (mut recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
    recv.idle_timeout = Duration::from_millis(200);
    let started = Instant::now();
    let outcomes = serve_concurrently(recv, 1, &|_index: u64, _event: Progress| {}).await;
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0], Err(EngineError::IdleTimeout(_))),
        "got: {outcomes:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "must not hang, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn serve_concurrently_zero_expected_jobs_returns_empty() {
    let dst = tempfile::tempdir().unwrap();
    let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
    let (recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
    let outcomes = serve_concurrently(recv, 0, &|_index: u64, _event: Progress| {}).await;
    assert!(outcomes.is_empty());
}

/// T12 analog: >4 GiB via a sparse file. Release-only (debug hashing is
/// too slow); run explicitly with `cargo test --release -p ak-engine --
/// --ignored`.
#[tokio::test]
#[ignore = "5 GiB sparse transfer; run in release only"]
async fn five_gib_sparse_file_transfers_with_u64_offsets() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let big = src.path().join("big.bin");
    let file = fs::File::create(&big).unwrap();
    file.set_len(5_u64 * 1024 * 1024 * 1024 + 13).unwrap();
    drop(file);
    write(&src.path().join("tail.txt"), b"after the big one");

    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1024);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move { receive_with_addr(recv, &noop, addr_tx).await });
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &noop).await.expect("5 GiB send");
    receiver.await.unwrap().expect("5 GiB receive");
    assert_eq!(report.bytes, 5 * 1024 * 1024 * 1024 + 13 + 17); // big.bin + tail.txt (17 bytes)
    assert_eq!(
        fs::metadata(dst.path().join("big.bin")).unwrap().len(),
        5 * 1024 * 1024 * 1024 + 13
    );
    assert_eq!(
        fs::read(dst.path().join("tail.txt")).unwrap(),
        b"after the big one"
    );
}

/// Drive a raw v1 receiver through Hello/Offer/Accept/Manifest and reply
/// with an empty ResumeRequest; returns the job id. Used by the pause and
/// fallback tests.
async fn fake_receiver_preamble(
    stream: &mut ak_net::DirectStream,
    identity: &TlsBundle,
    capabilities: u16,
) -> JobId {
    let limits = engine_limits();
    let _ = stream.recv_frame(&limits).await.unwrap(); // sender Hello
    stream
        .send_frame(&hello_frame("fake", identity, capabilities))
        .await
        .unwrap();
    let job_id = match stream.recv_frame(&limits).await.unwrap().message {
        Message::Offer { job_id, .. } => job_id,
        other => panic!("expected Offer, got {other:?}"),
    };
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::Accept { job_id },
        })
        .await
        .unwrap();
    loop {
        match stream.recv_frame(&limits).await.unwrap().message {
            Message::ManifestPage { last_page, .. } => {
                if last_page {
                    break;
                }
            }
            other => panic!("expected ManifestPage, got {other:?}"),
        }
    }
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::ResumeRequest {
                job_id,
                files: Vec::new(),
                more: false,
            },
        })
        .await
        .unwrap();
    job_id
}

/// Ack every remaining chunk on the stream, answer Finalize with a
/// positive FinalAck, and return the number of chunks acked.
async fn fake_ack_until_finalize(stream: &mut ak_net::DirectStream, job_id: JobId) -> u64 {
    let limits = engine_limits();
    let mut acked = 0u64;
    loop {
        match stream.recv_frame(&limits).await.unwrap().message {
            Message::Chunk {
                file_index,
                chunk_index,
                ..
            } => {
                stream
                    .send_frame(&Frame {
                        version: ak_wire::VERSION,
                        message: Message::ChunkAck {
                            job_id,
                            file_index,
                            chunk_index,
                        },
                    })
                    .await
                    .unwrap();
                acked += 1;
            }
            Message::Finalize { .. } => {
                stream
                    .send_frame(&Frame {
                        version: ak_wire::VERSION,
                        message: Message::FinalAck {
                            job_id,
                            verified: true,
                        },
                    })
                    .await
                    .unwrap();
                // Hold the connection like the real receiver does: the
                // sender reads the FinalAck and then half-closes. Dropping
                // the endpoint now could discard the FinalAck in flight.
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if stream.recv_frame(&limits).await.is_err() {
                            break;
                        }
                    }
                })
                .await;
                return acked;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

/// The peer completes the handshake then goes silent: the sender must
/// fail with IdleTimeout (never hang), keeping its checkpoint.
#[tokio::test]
async fn idle_timeout_when_peer_goes_silent_after_handshake() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("idle.bin"), b"payload");
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 16);
    send_spec.idle_timeout = Duration::from_millis(300);
    let checkpoint = send_spec.checkpoint_dir.clone();

    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;
    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut stream = channel.accept_bi().await.unwrap();
        let limits = engine_limits();
        let _ = stream.recv_frame(&limits).await; // sender Hello
        stream
            .send_frame(&hello_frame("stall", &bundle, RECV_CAPABILITIES))
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await; // Offer
                                                  // Silent from here; hold the stream open so the sender observes
                                                  // silence rather than a close.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let err = send(send_spec, &noop)
        .await
        .expect_err("silent peer must time out");
    assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
    assert!(
        !FileCheckpointStore::open(&checkpoint)
            .unwrap()
            .list()
            .unwrap()
            .is_empty(),
        "checkpoint must survive the idle abort"
    );
    fake.abort();
}

/// The sender stalls right after Hello: the receiver must fail with
/// IdleTimeout while waiting for the Offer.
#[tokio::test]
async fn idle_timeout_on_receive_when_sender_stalls() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_millis(300);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
        .await
        .unwrap();
    let channel = endpoint
        .connect(addr, &receiver_key.fingerprint())
        .await
        .unwrap();
    let mut stream = channel.open_bi().await.unwrap();
    stream
        .send_frame(&hello_frame("stall", &sender_bundle, CAP_RESUME))
        .await
        .unwrap();
    let _ = stream.recv_frame(&engine_limits()).await; // receiver Hello

    let err = receiver
        .await
        .unwrap()
        .expect_err("stalled sender must time out");
    assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
}

/// The receiver sends Pause mid-transfer: the sender stops issuing
/// chunks until Resume, then finishes the job.
#[tokio::test]
async fn pause_holds_sender_until_resume() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    write(&src.path().join("pause.bin"), &payload);
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1); // 8 chunks
    send_spec.in_flight_window = 1;
    send_spec.idle_timeout = Duration::from_secs(20);
    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;

    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut stream = channel.accept_bi().await.unwrap();
        let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
        let limits = engine_limits();
        let (file_index, chunk_index) = match stream.recv_frame(&limits).await.unwrap().message {
            Message::Chunk {
                file_index,
                chunk_index,
                ..
            } => (file_index, chunk_index),
            other => panic!("expected Chunk, got {other:?}"),
        };
        // Pause, ack the in-flight chunk, then hold.
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Control {
                    job_id,
                    control: Control::Pause,
                },
            })
            .await
            .unwrap();
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ChunkAck {
                    job_id,
                    file_index,
                    chunk_index,
                },
            })
            .await
            .unwrap();
        let leaked =
            tokio::time::timeout(Duration::from_millis(800), stream.recv_frame(&limits)).await;
        assert!(leaked.is_err(), "sender sent a chunk while paused");
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Control {
                    job_id,
                    control: Control::Resume,
                },
            })
            .await
            .unwrap();
        1 + fake_ack_until_finalize(&mut stream, job_id).await
    });

    let events: Arc<std::sync::Mutex<Vec<Progress>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&events);
    let progress = move |event: Progress| log.lock().unwrap().push(event);
    let report = send(send_spec, &progress)
        .await
        .expect("send completes after Resume");
    let acked = fake.await.unwrap();
    assert_eq!(acked, 8, "all chunks must be acked after resume");
    assert_eq!(report.files, 1);

    let events = events.lock().unwrap();
    let paused = events
        .iter()
        .position(|e| matches!(e, Progress::Paused))
        .expect("Paused progress event");
    let resumed = events
        .iter()
        .position(|e| matches!(e, Progress::Resumed))
        .expect("Resumed progress event");
    let sent_while_paused = events[paused..=resumed]
        .iter()
        .filter(|e| matches!(e, Progress::ChunkSent { .. }))
        .count();
    assert_eq!(
        sent_while_paused, 0,
        "no chunk may be sent while paused (events: {events:?})"
    );
}

/// The sender opts into multistream but the peer does not advertise the
/// capability: the transfer must run over the frozen single-stream path.
#[tokio::test]
async fn multistream_sender_falls_back_when_peer_lacks_capability() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
    write(&src.path().join("fallback.bin"), &payload);
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
    send_spec.multistream = true;
    send_spec.in_flight_window = 2;
    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;

    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut stream = channel.accept_bi().await.unwrap();
        // A v1-only receiver: no CAP_MULTISTREAM bit in its Hello.
        let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
        fake_ack_until_finalize(&mut stream, job_id).await
    });
    let report = send(send_spec, &noop)
        .await
        .expect("fallback send completes");
    assert_eq!(fake.await.unwrap(), 8);
    assert_eq!(report.files, 1);
}

/// Both sides advertise CAP_MULTISTREAM and the sender opts in: every
/// file rides its own stream, interleaved, and the result is identical.
#[tokio::test]
async fn multistream_loopback_transfer_is_byte_identical() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("a.txt"), b"hello ak");
    let deep: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
    write(&src.path().join("nested/deep/b.bin"), &deep);
    let third: Vec<u8> = (0..20_000u32).map(|i| (i % 199) as u8).collect();
    write(&src.path().join("nested/c.bin"), &third);
    fs::create_dir_all(src.path().join("empty/dir")).unwrap();

    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 1 KiB chunks
    send_spec.multistream = true;
    send_spec.in_flight_window = 4; // up to 4 concurrent file streams

    let sent: Arc<std::sync::Mutex<Vec<u32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let bytes_seen: Arc<std::sync::Mutex<Vec<(u32, u64)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sender_log = Arc::clone(&sent);
    let sender_progress = move |event: Progress| {
        if let Progress::ChunkSent { file_index, .. } = event {
            sender_log.lock().unwrap().push(file_index);
        }
    };
    let recv_log = Arc::clone(&bytes_seen);
    let recv_progress = move |event: Progress| {
        if let Progress::BytesAcked {
            file_index,
            cumulative_bytes,
        } = event
        {
            recv_log
                .lock()
                .unwrap()
                .push((file_index, cumulative_bytes));
        }
    };

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver =
        tokio::spawn(async move { receive_with_addr(recv, &recv_progress, addr_tx).await });
    send_spec.peer = addr_rx.await.unwrap();
    let report = send(send_spec, &sender_progress)
        .await
        .expect("multistream send completes");
    let recv_report = receiver
        .await
        .unwrap()
        .expect("multistream receive completes");

    assert_eq!(report.files, 7); // 3 files + 4 directory entries
    assert_eq!(report.bytes, recv_report.bytes);
    assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
    assert_eq!(
        fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
        deep
    );
    assert_eq!(fs::read(dst.path().join("nested/c.bin")).unwrap(), third);
    assert!(dst.path().join("empty/dir").is_dir());
    let (files, bytes) = count_entries(dst.path());
    assert_eq!((files, bytes), (3, 8 + 40_000 + 20_000));

    // Streams ran in parallel: file indexes interleave in ChunkSent.
    // Manifest order: a.txt=0, empty=1, empty/dir=2, nested=3,
    // nested/c.bin=4 (20 chunks), nested/deep=5, nested/deep/b.bin=6 (40).
    let sent = sent.lock().unwrap();
    let file_b = sent.iter().filter(|f| **f == 6).count();
    assert_eq!(file_b, 40, "b.bin must send all 40 chunks");
    let first_b = sent.iter().position(|f| *f == 6).unwrap();
    let last_b = sent.iter().rposition(|f| *f == 6).unwrap();
    assert!(
        sent[first_b..=last_b].contains(&4),
        "file streams did not interleave: {sent:?}"
    );

    // Byte-level progress fired at checkpoint cadence, chunk-aligned.
    let seen = bytes_seen.lock().unwrap();
    assert!(!seen.is_empty(), "no BytesAcked events observed");
    assert!(
        seen.iter().all(|(_, b)| b % 1024 == 0),
        "unaligned: {seen:?}"
    );
    assert!(
        seen.iter()
            .map(|(_, b)| *b)
            .max()
            .is_some_and(|m| m >= 16 * 1024),
        "expected at least one cadence event past 16 chunks: {seen:?}"
    );
}

/// The sender vanishes after the manifest: the receiver fails, and no
/// empty `.akpart` litter remains in the destination.
#[tokio::test]
async fn failed_receive_discards_empty_part_files() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
        .await
        .unwrap();
    let channel = endpoint
        .connect(addr, &receiver_key.fingerprint())
        .await
        .unwrap();
    let mut stream = channel.open_bi().await.unwrap();
    let limits = engine_limits();
    stream
        .send_frame(&hello_frame("ghost", &sender_bundle, CAP_RESUME))
        .await
        .unwrap();
    let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
    let data = b"hello";
    let digest: [u8; 32] = *blake3::hash(data).as_bytes();
    let entry = WireEntry {
        rel_path: "ghost.txt".to_owned(),
        is_dir: false,
        size: data.len() as u64,
        mtime_unix_ms: 0,
        digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
        whole_digest: digest,
        chunk_digests: vec![digest],
    };
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::Offer {
                job_id: [1; 16],
                root_name: "ghost".to_owned(),
                file_count: 1,
                total_bytes: data.len() as u64,
                chunk_size: 1024,
            },
        })
        .await
        .unwrap();
    let _ = stream.recv_frame(&limits).await.unwrap(); // Accept
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::ManifestPage {
                job_id: [1; 16],
                page_index: 0,
                last_page: true,
                entries: vec![entry],
            },
        })
        .await
        .unwrap();
    let _ = stream.recv_frame(&limits).await.unwrap(); // ResumeRequest: sinks exist now
                                                       // Vanish before any chunk.
    drop(stream);
    channel.close(0, b"test done");
    drop(endpoint);

    let err = receiver
        .await
        .unwrap()
        .expect_err("receiver must fail after the sender vanished");
    assert!(
        matches!(err, EngineError::PeerClosed | EngineError::Stream(_)),
        "got {err:?}"
    );
    let parts: Vec<_> = fs::read_dir(dst.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .to_string_lossy()
                .ends_with(ak_storage::PART_SUFFIX)
        })
        .collect();
    assert!(parts.is_empty(), "empty part files survived: {parts:?}");
}

/// Regression for the 4,097-file defect, now per frame: a ResumeRequest
/// frame covering a full shard of `max_resume_entries` entries must
/// decode on the sender under engine limits.
#[test]
fn engine_limits_fit_resume_requests_for_maximum_jobs() {
    let limits = engine_limits();
    assert_eq!(limits.max_resume_entries, ak_wire::MAX_RESUME_ENTRIES);
    // One frame with the worst-case per-frame entry count, announcing a
    // following frame.
    let files: Vec<ResumeEntry> = (0..limits.max_resume_entries as u32)
        .map(|file_index| ResumeEntry {
            file_index,
            verified_prefix_chunks: 0,
        })
        .collect();
    let mut buf = Vec::new();
    Frame {
        version: ak_wire::VERSION,
        message: Message::ResumeRequest {
            job_id: [7; 16],
            files,
            more: true,
        },
    }
    .encode(&mut buf);
    assert!(buf.len() < ak_wire::MAX_FRAME_BYTES);
    let decoded = Frame::decode(&buf, &limits).expect("max-size ResumeRequest decodes");
    match decoded.message {
        Message::ResumeRequest { files, more, .. } => {
            assert_eq!(files.len(), 65_535);
            assert!(more);
        }
        other => panic!("unexpected message: {other:?}"),
    }
}

// ------------------------------------------------------------------
// Sharded ResumeRequest exchange (single-frame job cap removed)
// ------------------------------------------------------------------

/// The shard function: every non-final frame carries more = true, no
/// frame exceeds the cap, an empty answer yields exactly one empty final
/// frame, and concatenation reproduces the input.
#[test]
fn resume_request_frames_shard_within_the_cap() {
    let cap = engine_limits().max_resume_entries as usize;
    for total in [0usize, 1, cap - 1, cap, cap + 1, 2 * cap + 7] {
        let files: Vec<ResumeEntry> = (0..total as u32)
            .map(|i| ResumeEntry {
                file_index: i,
                verified_prefix_chunks: u64::from(i) * 3,
            })
            .collect();
        let frames = resume_request_frames([9; 16], &files, cap);
        assert!(!frames.is_empty(), "total {total}: at least one frame");
        let mut reunited = Vec::new();
        for (n, message) in frames.iter().enumerate() {
            let Message::ResumeRequest {
                job_id,
                files: shard,
                more,
            } = message
            else {
                panic!("total {total}: frame {n} is not a ResumeRequest");
            };
            assert_eq!(*job_id, [9; 16], "total {total}: frame {n} job id");
            assert!(
                shard.len() <= cap,
                "total {total}: frame {n} holds {} > {cap}",
                shard.len()
            );
            assert_eq!(
                *more,
                n + 1 < frames.len(),
                "total {total}: frame {n} more flag"
            );
            reunited.extend_from_slice(shard);
        }
        assert_eq!(
            reunited, files,
            "total {total}: concatenation must reproduce the input"
        );
    }
}

/// A resume frame naming a different job aborts the exchange; the
/// matching job id accumulates until a more = false frame ends it.
#[test]
fn resume_collector_validates_job_ids_and_completion() {
    let mut collector = ResumeCollector::new([1; 16], &one_file_manifest(8), &engine_limits());
    assert!(
        !collector
            .push(
                [1; 16],
                vec![ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: 1,
                }],
                true
            )
            .unwrap(),
        "more = true must not finish the exchange"
    );
    assert_eq!(collector.entries.len(), 1);
    assert!(matches!(
        collector.push([2; 16], Vec::new(), false),
        Err(EngineError::PeerError { .. })
    ));
    assert!(
        collector.push([1; 16], Vec::new(), false).unwrap(),
        "more = false completes the exchange"
    );
    assert_eq!(collector.entries.len(), 1);
}

/// 100,000-entry job, unit level (no socket): shard, push every frame
/// through the real wire encode/decode, and fold the decoded frames into
/// the sender's accumulator loop (which now cross-validates every entry
/// against a manifest of exactly this shape). Every entry must arrive in
/// order and no frame may exceed the per-frame cap. Real 100k-file
/// transfers are deliberately not exercised here (I/O cost);
/// release-time manual runs own that boundary.
#[test]
fn one_hundred_thousand_resume_entries_arrive_sharded() {
    const TOTAL: usize = 100_000;
    const CHUNKS_PER_FILE: u64 = 4;
    let limits = engine_limits();
    let cap = usize::from(limits.max_resume_entries);
    let job_id = [0xA5; 16];
    let files: Vec<ResumeEntry> = (0..TOTAL as u32)
        .map(|i| ResumeEntry {
            file_index: i,
            verified_prefix_chunks: u64::from(i) % CHUNKS_PER_FILE,
        })
        .collect();
    // The sender's manifest for this answer: one 4-chunk file per entry.
    let manifest = Manifest {
        chunk_size: NonZeroU32::new(1024).unwrap(),
        entries: (0..TOTAL)
            .map(|i| {
                FileEntry::new(
                    &format!("f{i}.bin"),
                    CHUNKS_PER_FILE * 1024,
                    0,
                    [7; 32],
                    vec![[7; 32]; CHUNKS_PER_FILE as usize],
                )
                .unwrap()
            })
            .collect(),
    };

    let frames = resume_request_frames(job_id, &files, cap);
    assert_eq!(
        frames.len(),
        TOTAL.div_ceil(cap),
        "100k entries need two shards at cap {cap}"
    );

    // Simulated sender receive loop: decode each frame, fold it in.
    let mut collector = ResumeCollector::new(job_id, &manifest, &limits);
    let mut final_frame = false;
    for message in frames {
        let mut buf = Vec::new();
        Frame {
            version: ak_wire::VERSION,
            message,
        }
        .encode(&mut buf);
        assert!(
            buf.len() < ak_wire::MAX_FRAME_BYTES,
            "shard frame must fit the frame cap"
        );
        match Frame::decode(&buf, &limits)
            .expect("shard frame decodes under engine limits")
            .message
        {
            Message::ResumeRequest {
                job_id: frame_job_id,
                files: shard,
                more,
            } => {
                assert!(
                    shard.len() <= cap,
                    "single frame exceeded the per-frame cap"
                );
                if collector
                    .push(frame_job_id, shard, more)
                    .expect("frame accepted")
                {
                    final_frame = true;
                    break;
                }
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
    assert!(final_frame, "exchange must end with a more = false frame");
    assert_eq!(
        collector.entries, files,
        "all 100,000 entries must arrive in order"
    );
}

// ------------------------------------------------------------------
// Receiver liveness during prepare and verify (review 2026-11-18)
// ------------------------------------------------------------------

/// The cadence is a quarter of the receiver's idle budget, capped at five
/// seconds: the default 30 s budget signals every 5 s, a 1 s budget
/// every 250 ms.
#[test]
fn liveness_interval_is_a_quarter_of_idle_capped_at_five_seconds() {
    assert_eq!(
        liveness_interval(DEFAULT_IDLE_TIMEOUT),
        LIVENESS_MAX_INTERVAL
    );
    assert_eq!(LIVENESS_MAX_INTERVAL, Duration::from_secs(5));
    assert_eq!(
        liveness_interval(Duration::from_secs(8)),
        Duration::from_secs(2)
    );
    assert_eq!(
        liveness_interval(Duration::from_secs(1)),
        Duration::from_millis(250)
    );
    assert_eq!(
        liveness_interval(Duration::from_secs(3600)),
        LIVENESS_MAX_INTERVAL
    );
}

fn resume_entry(index: u32) -> ResumeEntry {
    ResumeEntry {
        file_index: index,
        verified_prefix_chunks: u64::from(index % 4),
    }
}

/// A shard leaves when it fills a frame or when the interval has passed
/// since the last frame (then possibly empty), never otherwise; whatever
/// is left stays for the final frame.
#[test]
fn resume_shards_leave_when_full_or_when_due() {
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);
    let mut shards = ResumeShards::new(3, Duration::from_secs(5), t0);
    assert_eq!(shards.take_due(at(1)), None, "nothing pending, not due");
    shards.push(resume_entry(0));
    shards.push(resume_entry(1));
    assert_eq!(shards.take_due(at(2)), None, "partial and not due");
    shards.push(resume_entry(2));
    assert_eq!(
        shards.take_due(at(2)),
        Some(vec![resume_entry(0), resume_entry(1), resume_entry(2)]),
        "a full frame leaves at once"
    );
    assert_eq!(shards.take_due(at(6)), None, "the clock restarted at 2 s");
    assert_eq!(
        shards.take_due(at(7)),
        Some(Vec::new()),
        "due with nothing pending (a run of directories): an empty keep-alive shard"
    );
    shards.push(resume_entry(3));
    assert_eq!(shards.take_due(at(12)), Some(vec![resume_entry(3)]));
    shards.push(resume_entry(4));
    assert_eq!(shards.take_due(at(13)), None);
    assert_eq!(shards.into_rest(), vec![resume_entry(4)]);

    // A zero cap is clamped to one entry per frame, like the final shards.
    let mut tiny = ResumeShards::new(0, Duration::from_secs(5), t0);
    tiny.push(resume_entry(9));
    assert_eq!(tiny.take_due(t0), Some(vec![resume_entry(9)]));
}

/// The sender's collector folds progressive shards (full ones, a partial
/// one sent on the timer, an empty keep-alive one) plus the final frame
/// into exactly the answer the receiver prepared, through a real
/// encode/decode round trip.
#[test]
fn progressive_resume_shards_fold_into_the_full_answer() {
    let limits = engine_limits();
    let job_id = [5; 16];
    let manifest = Manifest {
        chunk_size: NonZeroU32::new(1024).unwrap(),
        entries: (0..10)
            .map(|i| {
                FileEntry::new(&format!("f{i}.bin"), 4 * 1024, 0, [1; 32], vec![[1; 32]; 4])
                    .unwrap()
            })
            .collect(),
    };
    let answer: Vec<ResumeEntry> = (0..10).map(resume_entry).collect();

    let t0 = Instant::now();
    let mut shards = ResumeShards::new(4, Duration::from_secs(1), t0);
    let mut messages = Vec::new();
    let mut emit = |shards: &mut ResumeShards, now: Instant| {
        if let Some(files) = shards.take_due(now) {
            messages.push(Message::ResumeRequest {
                job_id,
                files,
                more: true,
            });
        }
    };
    for entry in &answer[..4] {
        shards.push(*entry);
        emit(&mut shards, t0); // the fourth fills the frame
    }
    emit(&mut shards, t0 + Duration::from_secs(2)); // empty keep-alive
    shards.push(answer[4]);
    emit(&mut shards, t0 + Duration::from_millis(2500));
    shards.push(answer[5]);
    emit(&mut shards, t0 + Duration::from_secs(3)); // partial, on the timer
    for entry in &answer[6..] {
        shards.push(*entry);
        emit(&mut shards, t0 + Duration::from_secs(3)); // the last fills the frame
    }
    messages.extend(resume_request_frames(
        job_id,
        &shards.into_rest(),
        usize::from(limits.max_resume_entries),
    ));
    let sizes: Vec<(usize, bool)> = messages
        .iter()
        .map(|message| match message {
            Message::ResumeRequest { files, more, .. } => (files.len(), *more),
            other => panic!("unexpected message: {other:?}"),
        })
        .collect();
    assert_eq!(
        sizes,
        vec![(4, true), (0, true), (2, true), (4, true), (0, false)]
    );

    let mut collector = ResumeCollector::new(job_id, &manifest, &limits);
    let mut done = false;
    for message in messages {
        assert!(!done, "no frame may follow the final one");
        let mut buf = Vec::new();
        Frame {
            version: ak_wire::VERSION,
            message,
        }
        .encode(&mut buf);
        let Message::ResumeRequest {
            job_id: frame_job,
            files,
            more,
        } = Frame::decode(&buf, &limits).unwrap().message
        else {
            panic!("not a ResumeRequest");
        };
        done = collector.push(frame_job, files, more).unwrap();
    }
    assert!(done, "the exchange ends with the more = false frame");
    assert_eq!(collector.entries, answer);
}

/// Liveness while verifying (review 2026-11-18). In a multi-thread
/// runtime — the production shape — the sender's FinalAck wait is a real
/// timer: a verify phase longer than the sender's idle budget used to
/// end in a sender-side `IdleTimeout` while the receiver committed every
/// file. (The single-thread test runtime hid this: the receiver's
/// blocking verify pass also stopped the sender's timer.) The receiver's
/// progress callback stretches the verify phase here to several idle
/// budgets; the no-op `Control::Resume` signals keep the sender waiting
/// and both sides complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_verify_phase_keeps_the_sender_waiting() {
    const FILES: usize = 12;
    let idle = Duration::from_secs(1);
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for i in 0..FILES {
        write(&src.path().join(format!("f{i:02}.bin")), &[i as u8; 3000]);
    }
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 1);
    send_spec.idle_timeout = idle;
    recv.idle_timeout = idle;

    let window: Arc<std::sync::Mutex<(Option<Instant>, Option<Instant>)>> =
        Arc::new(std::sync::Mutex::new((None, None)));
    let progress = {
        let window = Arc::clone(&window);
        Arc::new(move |event: Progress| match event {
            Progress::Verifying => window.lock().unwrap().0 = Some(Instant::now()),
            // Each file's verification takes 3/10 of the idle budget.
            Progress::FileVerified { .. } => std::thread::sleep(idle * 3 / 10),
            Progress::FinalAck { .. } => window.lock().unwrap().1 = Some(Instant::now()),
            _ => {}
        })
    };
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
    };
    send_spec.peer = addr_rx.await.unwrap();
    let sender = tokio::spawn(async move { send(send_spec, &noop).await });
    let recv_outcome = receiver.await.unwrap();
    let send_outcome = sender.await.unwrap();
    let report = send_outcome.expect("the sender must outlast a long verify phase");
    recv_outcome.expect("the receiver completes");
    assert_eq!(report.files, FILES as u64);

    let (start, end) = *window.lock().unwrap();
    let span = end.expect("FinalAck sent") - start.expect("verify phase began");
    assert!(
        span >= idle * 3,
        "the verify phase must outlast the sender's idle budget several times over: {span:?}"
    );
    for i in 0..FILES {
        assert_eq!(
            fs::read(dst.path().join(format!("f{i:02}.bin"))).unwrap(),
            vec![i as u8; 3000]
        );
    }
}

/// A verify-phase failure while other files are being verified (batch Q
/// review, Q-N2). The receiver verifies and commits up to
/// VERIFY_PARALLEL_FILES files at a time; when one fails, every pass
/// already running must return before the receive does, so nothing is
/// still writing in the destination afterwards. Each file ends up either
/// committed with the right bytes or still a part file, and a second run
/// of the same job skips the committed files, fetches the damaged one
/// again and commits the rest without leaving extra copies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_failure_waits_for_running_passes_and_a_retry_completes() {
    const FILES: usize = 3 * VERIFY_PARALLEL_FILES;
    const BAD: usize = 2;
    const LEN: usize = 3000;
    let body = |i: usize| -> Vec<u8> { (0..LEN).map(|b| (b * 7 + i * 131 + 0x51) as u8).collect() };
    let name = |i: usize| format!("q{i:02}.bin");
    let part = |dir: &Path, i: usize| dir.join(format!("{}{}", name(i), ak_storage::PART_SUFFIX));
    let listing = |dir: &Path| -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, entry.metadata().unwrap().len())
            })
            .collect();
        entries.sort();
        entries
    };
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for i in 0..FILES {
        write(&src.path().join(name(i)), &body(i));
    }

    // First run: once every chunk is in, damage one part file in place
    // (same file, so its whole-file digest fails rather than its identity).
    let bad_part = part(dst.path(), BAD);
    let progress = {
        let bad_part = bad_part.clone();
        Arc::new(move |event: Progress| {
            if let Progress::Verifying = event {
                use std::io::{Seek, SeekFrom, Write};
                let mut file = fs::OpenOptions::new().write(true).open(&bad_part).unwrap();
                file.seek(SeekFrom::Start(100)).unwrap();
                file.write_all(&[!body(BAD)[100]]).unwrap();
                file.sync_all().unwrap();
            }
        })
    };
    let (send_spec, recv, _) = specs(src.path(), dst.path(), 1);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
    };
    let mut first = send_spec.clone();
    first.peer = addr_rx.await.unwrap();
    let sender = tokio::spawn(async move { send(first, &noop).await });
    let error = receiver
        .await
        .unwrap()
        .expect_err("the damaged file must fail its whole-file digest");
    let at_return = listing(dst.path());
    assert!(error.to_string().contains("HASH_MISMATCH"), "{error}");
    assert!(
        sender.await.unwrap().is_err(),
        "the sender must not report success"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        listing(dst.path()),
        at_return,
        "no verify pass may still be writing once the receive has returned"
    );
    assert!(
        bad_part.exists(),
        "the failed file keeps its part for resume"
    );
    for i in 0..FILES {
        let committed = dst.path().join(name(i));
        assert!(
            committed.exists() != part(dst.path(), i).exists(),
            "file {i}: exactly one of the final file and its part"
        );
        if committed.exists() {
            assert_eq!(fs::read(&committed).unwrap(), body(i), "file {i}");
        }
    }

    // Second run of the same job.
    let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    send(send_spec, &noop)
        .await
        .expect("the second run completes");
    receiver
        .await
        .unwrap()
        .expect("the second receive completes");
    for i in 0..FILES {
        assert_eq!(
            fs::read(dst.path().join(name(i))).unwrap(),
            body(i),
            "file {i}"
        );
    }
    assert_eq!(
        count_entries(dst.path()),
        (FILES as u64, (FILES * LEN) as u64),
        "no part files or extra copies remain"
    );
    assert_eq!(
        listing(dst.path()).len(),
        FILES,
        "nothing else in the destination"
    );
}

/// Chunk-phase checkpoint cadence (review 2026-11-19): progress on every
/// 16th ack as before, a save on such an ack only once a second has
/// passed since the last one — so a many-file burst no longer rewrites
/// the whole record every 16 files, while slow, large chunks still save
/// on every 16th ack.
#[test]
fn chunk_checkpoint_saves_at_most_once_per_interval() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let quiet = CadenceTick {
        progress: false,
        save: false,
    };
    let mut cadence = ChunkCheckpointCadence::new(t0);
    for _ in 0..15 {
        assert_eq!(cadence.on_ack(at(10)), quiet);
    }
    assert_eq!(
        cadence.on_ack(at(10)),
        CadenceTick {
            progress: true,
            save: false
        },
        "the 16th ack reports progress; the last save is too recent"
    );
    for _ in 0..15 {
        assert_eq!(cadence.on_ack(at(1000)), quiet);
    }
    assert_eq!(
        cadence.on_ack(at(1000)),
        CadenceTick {
            progress: true,
            save: true
        },
        "a second after the last save, the 16th ack saves"
    );
    for _ in 0..15 {
        cadence.on_ack(at(1500));
    }
    assert!(
        !cadence.on_ack(at(1500)).save,
        "half a second later: no save"
    );

    // A many-file burst: 65,536 acks within 0.65 s report progress 4,096
    // times and do not save at all.
    let mut cadence = ChunkCheckpointCadence::new(t0);
    let (mut ticks, mut saves) = (0, 0);
    for ack in 0..65_536u64 {
        let tick = cadence.on_ack(at(ack / 100));
        ticks += u64::from(tick.progress);
        saves += u64::from(tick.save);
    }
    assert_eq!((ticks, saves), (4096, 0));
    // Spread over ten seconds instead: about one save a second.
    let mut cadence = ChunkCheckpointCadence::new(t0);
    let mut saves = 0;
    for ack in 0..65_536u64 {
        saves += u64::from(cadence.on_ack(at(ack * 10_000 / 65_536)).save);
    }
    assert!((9..=10).contains(&saves), "{saves} saves over ten seconds");
}

/// One loopback QUIC stream: the opener's send half and the acceptor's
/// receive half, plus the endpoints and channels that must outlive them.
async fn loopback_stream() -> (
    DirectSendStream,
    DirectRecvStream,
    (DirectEndpoint, DirectEndpoint, DirectChannel, DirectChannel),
) {
    let server_key = DeviceKeyPair::generate();
    let client_key = DeviceKeyPair::generate();
    let server_bundle = ak_identity::tls::make_self_signed(&server_key);
    let client_bundle = ak_identity::tls::make_self_signed(&client_key);
    let server = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &server_bundle)
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let accepting = tokio::spawn(async move {
        let channel = server.accept(&|_| true).await.unwrap();
        let stream = channel.accept_bi().await.unwrap();
        (server, channel, stream)
    });
    let client = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &client_bundle)
        .await
        .unwrap();
    let client_channel = client
        .connect(addr, &server_key.fingerprint())
        .await
        .unwrap();
    let (mut tx, _client_rx) = client_channel.open_bi().await.unwrap().split();
    // A stream exists for the acceptor only once the opener has sent on it.
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Control {
            job_id: [0; 16],
            control: Control::Resume,
        },
    })
    .await
    .unwrap();
    let (server, server_channel, accepted) = accepting.await.unwrap();
    let (_server_tx, mut rx) = accepted.split();
    let _ = rx.recv_frame(&engine_limits()).await.unwrap(); // the opener frame
    (tx, rx, (server, client, server_channel, client_channel))
}

/// The offload helper (review 2026-11-19): while a blocking pass runs on
/// the blocking pool, the keepalive leaves on every interval and moves
/// `last_signal`; without a keepalive nothing is sent but the pass still
/// completes. Timing-based on purpose: the pass takes 650 ms, the
/// interval is 100 ms, and only a lower bound of four signals is asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offloaded_pass_keeps_signalling_until_it_finishes() {
    let (mut tx, mut rx, _keep) = loopback_stream().await;
    let limits = engine_limits();
    let keepalive = Frame {
        version: ak_wire::VERSION,
        message: Message::Control {
            job_id: [7; 16],
            control: Control::Resume,
        },
    };
    let mut last_signal = Instant::now();
    let before = last_signal;
    let value = offload_with_liveness(
        &mut tx,
        Some(&keepalive),
        Duration::from_millis(100),
        &mut last_signal,
        || {
            std::thread::sleep(Duration::from_millis(650));
            42
        },
    )
    .await
    .unwrap();
    assert_eq!(value, 42);
    assert!(last_signal > before, "each keepalive moves last_signal");
    let mut beats = 0;
    while let Ok(frame) =
        tokio::time::timeout(Duration::from_millis(300), rx.recv_frame(&limits)).await
    {
        let frame = frame.unwrap();
        assert!(
            matches!(
                frame.message,
                Message::Control {
                    job_id,
                    control: Control::Resume
                } if job_id == [7; 16]
            ),
            "only the keepalive may arrive: {:?}",
            frame.message
        );
        beats += 1;
    }
    assert!(
        beats >= 4,
        "a 650 ms pass at a 100 ms cadence: {beats} signals"
    );

    // No keepalive (a peer without CAP_PAUSE_RESUME): silent, still done.
    let mut quiet = Instant::now();
    let value = offload_with_liveness(&mut tx, None, Duration::from_millis(50), &mut quiet, || {
        std::thread::sleep(Duration::from_millis(200));
        7
    })
    .await
    .unwrap();
    assert_eq!(value, 7);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv_frame(&limits))
            .await
            .is_err(),
        "nothing may be sent without a keepalive"
    );
}

/// Large-file liveness end to end (review 2026-11-19), release only like
/// the 5 GiB test. A 4 GiB file arrives with all but its last four chunks
/// already in a verified part file, both sides on a 300 ms idle budget,
/// multi-thread runtime (the production shape). The receiver's prepare
/// re-hashes the prefix before it can answer the ResumeRequest, and its
/// verify phase hashes and flushes the whole 4 GiB file before the
/// FinalAck — each a single pass far longer than the sender's budget.
/// Before the offload the sender gave up at the ResumeRequest gate.
///
/// Only those two passes may outlast the budget here (batch P review).
/// The chunk phase has no liveness signal of its own (each ack is one), so
/// it is kept to four chunks: with a 1 GiB remainder the 300 ms budget
/// also timed a receiver-side checkpoint save, whose fsync takes hundreds
/// of milliseconds under that write load (intermittent on Linux), and on
/// Windows the first resumed write itself (see the prefix setup below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "4 GiB transfer; run in release only"]
async fn large_file_passes_keep_the_sender_waiting() {
    const GIB: u64 = 1024 * 1024 * 1024;
    const CHUNK: u64 = 1024 * 1024;
    const REMAINING_CHUNKS: u64 = 4;
    let size = 4 * GIB;
    let prefix = size - REMAINING_CHUNKS * CHUNK;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let file = fs::File::create(src.path().join("big.bin")).unwrap();
    file.set_len(size).unwrap();
    drop(file);
    // The verified prefix as an earlier session leaves it: every byte
    // written, then flushed. The content is zeros, so a zero-filled part
    // file of whole chunks is exactly a verified prefix. Nothing short of
    // writing it is that state on NTFS: a prefix made by `set_len`, or by
    // writing only its last byte, stays beyond the file's valid data
    // length, and the receiver's first resumed write then waits while the
    // file system zero-fills the whole prefix (23-27 s for ~4 GiB on
    // windows-latest; batch P handoff, section 9). Writing it costs time
    // here, before the timed transfer, and real disk space everywhere.
    let written = Instant::now();
    {
        use std::io::Write;
        let mut part = fs::File::create(dst.path().join("big.bin.akpart")).unwrap();
        let block = vec![0u8; 8 * 1024 * 1024];
        let mut left = prefix;
        while left > 0 {
            let n = left.min(block.len() as u64) as usize;
            part.write_all(&block[..n]).unwrap();
            left -= n as u64;
        }
        part.sync_all().unwrap();
    }
    eprintln!(
        "large-file passes: prefix of {prefix} bytes written and flushed in {:.2?}",
        written.elapsed()
    );

    let idle = Duration::from_millis(300);
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 1024);
    send_spec.idle_timeout = idle;
    recv.idle_timeout = idle;
    // Phase timeline of both sides, printed whatever the outcome, so a
    // failure shows which phase outlasted the budget (2026-11-19 review of
    // batch P: the Windows failure came with nothing but the error).
    let started = Instant::now();
    let timeline: Arc<std::sync::Mutex<Vec<(Duration, String)>>> = Arc::default();
    let note = {
        let timeline = Arc::clone(&timeline);
        move |what: String| timeline.lock().unwrap().push((started.elapsed(), what))
    };
    let resumed: Arc<std::sync::Mutex<Option<u64>>> = Arc::new(std::sync::Mutex::new(None));
    let progress = {
        let resumed = Arc::clone(&resumed);
        let note = note.clone();
        let chunks_received = std::sync::atomic::AtomicU64::new(0);
        Arc::new(move |event: Progress| match event {
            Progress::Resuming { chunks } => {
                *resumed.lock().unwrap() = Some(chunks);
                note(format!("recv resuming {chunks}"));
            }
            Progress::ChunkReceived { .. } => {
                let n = chunks_received.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n == 1 || n == REMAINING_CHUNKS {
                    note(format!("recv chunk #{n} stored"));
                }
            }
            Progress::ManifestReceived
            | Progress::Verifying
            | Progress::FinalAck { .. }
            | Progress::Done => note(format!("recv {event:?}")),
            _ => {}
        })
    };
    let send_progress = {
        let note = note.clone();
        let chunks_sent = std::sync::atomic::AtomicU64::new(0);
        move |event: Progress| match event {
            Progress::ChunkSent { .. } => {
                let n = chunks_sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n == 1 || n == REMAINING_CHUNKS {
                    note(format!("send chunk #{n} sent"));
                }
            }
            Progress::Resuming { .. } | Progress::FinalAck { .. } | Progress::Done => {
                note(format!("send {event:?}"));
            }
            _ => {}
        }
    };
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = {
        let progress = Arc::clone(&progress);
        tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
    };
    send_spec.peer = addr_rx.await.unwrap();
    let sent = send(send_spec, &send_progress).await;
    note(format!("send returned ok={}", sent.is_ok()));
    let received = tokio::time::timeout(Duration::from_secs(120), receiver).await;
    let phases: Vec<String> = timeline
        .lock()
        .unwrap()
        .iter()
        .map(|(at, what)| format!("{what} @ {at:.2?}"))
        .collect();
    eprintln!("large-file passes: phases: {}", phases.join(", "));
    eprintln!("large-file passes: send = {sent:?}; recv = {received:?}");
    let report = sent.expect("the sender must outlast both whole-file passes");
    received
        .expect("the receiver ends within 120 s")
        .unwrap()
        .expect("the receiver completes");
    assert_eq!(report.bytes, size);
    assert_eq!(
        *resumed.lock().unwrap(),
        Some(prefix / CHUNK),
        "the verified prefix (1 MiB chunks) must be resumed, not re-sent"
    );
    assert_eq!(
        fs::metadata(dst.path().join("big.bin")).unwrap().len(),
        size
    );
    assert!(!dst.path().join("big.bin.akpart").exists());
}

// ------------------------------------------------------------------
// Round 17: record id vs content id (review item 4)
// ------------------------------------------------------------------

/// A one-file manifest with `chunks` 1 KiB chunks (unit-test fixture).
fn one_file_manifest(chunks: usize) -> Manifest {
    Manifest {
        chunk_size: NonZeroU32::new(1024).unwrap(),
        entries: vec![FileEntry::new(
            "one.bin",
            chunks as u64 * 1024,
            0,
            [3; 32],
            vec![[3; 32]; chunks],
        )
        .unwrap()],
    }
}

/// The record id is a pure function of (content, peer, direction, root),
/// distinct from the content id, with separators normalized.
#[test]
fn record_id_binds_peer_direction_and_root() {
    let manifest = one_file_manifest(2);
    let content = job_id_for("one.bin", &manifest);
    let fp_a = DeviceKeyPair::generate().fingerprint();
    let fp_b = DeviceKeyPair::generate().fingerprint();
    let a = record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/src"));
    assert_eq!(
        a,
        record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/src")),
        "same inputs must derive the same record id"
    );
    assert_ne!(
        a,
        record_id_for(&content, &fp_b, Direction::Send, Path::new("C:/data/src")),
        "another peer must get another record"
    );
    assert_ne!(
        a,
        record_id_for(
            &content,
            &fp_a,
            Direction::Receive,
            Path::new("C:/data/src")
        ),
        "the opposite direction must get another record"
    );
    assert_ne!(
        a,
        record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/other")),
        "another root must get another record"
    );
    assert_ne!(
        a, content,
        "the record id must differ from the wire content id"
    );
    assert_eq!(
        a,
        record_id_for(&content, &fp_a, Direction::Send, Path::new("C:\\data\\src")),
        "equivalent path spellings (separator style) must agree"
    );
}

/// Same source sent to two different peers, serially, with one shared
/// sender checkpoint directory: two distinct Send records must survive
/// (the content-id keying of old overwrote one with the other).
#[tokio::test]
async fn same_source_to_two_peers_keeps_two_sender_records() {
    let src = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    write(&src.path().join("shared.bin"), &payload);
    let sender_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let checkpoint = tempfile::tempdir().unwrap();

    for round in 0..2 {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let this_fp = receiver_key.fingerprint();
        let mut recv = ReceiveSpec::new(
            dst.path().to_path_buf(),
            ak_identity::tls::make_self_signed(&receiver_key),
        );
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let mut send_spec = SendSpec::new(
            src.path().to_path_buf(),
            addr_rx.await.unwrap(),
            this_fp,
            sender_bundle.clone(),
        );
        send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
        send_spec.checkpoint_dir = checkpoint.path().to_path_buf();
        send(send_spec, &noop)
            .await
            .unwrap_or_else(|e| panic!("round {round} send failed: {e:?}"));
        receiver
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("round {round} receive failed: {e:?}"));
    }

    let store = FileCheckpointStore::open(checkpoint.path()).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 2, "records: {records:?}");
    assert!(
        records
            .iter()
            .all(|r| r.direction == Direction::Send && r.state == "completed"),
        "records: {records:?}"
    );
    assert_ne!(
        records[0].job_id, records[1].job_id,
        "the two peers must keep distinct record ids"
    );
}

/// One machine sends and receives through one shared checkpoint
/// directory (the loopback case): the send record and the receive
/// record must be separate files, not one overwriting the other.
#[tokio::test]
async fn loopback_send_and_receive_keep_separate_records() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let checkpoint = tempfile::tempdir().unwrap();
    write(&src.path().join("loop.bin"), b"same machine, two roles");
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let mut send_spec = SendSpec::new(
        src.path().to_path_buf(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_key.fingerprint(),
        ak_identity::tls::make_self_signed(&sender_key),
    );
    send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
    send_spec.checkpoint_dir = checkpoint.path().to_path_buf();
    let mut recv = ReceiveSpec::new(
        dst.path().to_path_buf(),
        ak_identity::tls::make_self_signed(&receiver_key),
    );
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.chunk_size = send_spec.chunk_size;
    recv.checkpoint_dir = checkpoint.path().to_path_buf();

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    send(send_spec, &noop).await.expect("loopback send");
    receiver.await.unwrap().expect("loopback receive");

    let store = FileCheckpointStore::open(checkpoint.path()).unwrap();
    let records = store.list().unwrap();
    assert_eq!(records.len(), 2, "records: {records:?}");
    assert_eq!(
        records
            .iter()
            .filter(|r| r.direction == Direction::Send)
            .count(),
        1,
        "records: {records:?}"
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r.direction == Direction::Receive)
            .count(),
        1,
        "records: {records:?}"
    );
    assert_ne!(
        records[0].job_id, records[1].job_id,
        "send and receive records must not share one key"
    );
}

// ------------------------------------------------------------------
// Round 17: committed-file retry idempotency (review item 5)
// ------------------------------------------------------------------

/// Specs with identities held fixed across runs, so both runs derive the
/// same record id (same content, peer, direction, root).
fn fixed_specs(
    src: &Path,
    dst: &Path,
    checkpoint: &Path,
    sender_key: &DeviceKeyPair,
    receiver_key: &DeviceKeyPair,
) -> (SendSpec, ReceiveSpec) {
    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_key.fingerprint(),
        ak_identity::tls::make_self_signed(sender_key),
    );
    send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
    send_spec.checkpoint_dir = checkpoint.to_path_buf();
    let mut recv = ReceiveSpec::new(
        dst.to_path_buf(),
        ak_identity::tls::make_self_signed(receiver_key),
    );
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.chunk_size = send_spec.chunk_size;
    recv.checkpoint_dir = checkpoint.to_path_buf();
    (send_spec, recv)
}

/// A fully completed job retried with the same record must be a no-op:
/// both sides succeed, the resume reports cover every chunk (full
/// prefix via the committed-file skip), nothing is retransmitted, and
/// the committed files are not touched again (no rename conflict, no
/// duplicate copies). Old behavior: the receiver prepared a fresh part
/// file for the already-committed target and finish() died on the
/// conflict policy.
#[tokio::test]
async fn completed_job_retry_is_idempotent_and_reports_full_resume() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let checkpoint = tempfile::tempdir().unwrap();
    let payloads: Vec<(String, Vec<u8>)> = (0..2)
        .map(|f| {
            (
                format!("retry-{f}.bin"),
                (0..8192u32)
                    .map(|i| ((i % 249 + f * 31) % 256) as u8)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    for (name, payload) in &payloads {
        write(&src.path().join(name), payload);
    }
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();

    // Run 1: the normal full transfer.
    {
        let (mut send_spec, recv) = fixed_specs(
            src.path(),
            dst.path(),
            checkpoint.path(),
            &sender_key,
            &receiver_key,
        );
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("first run sends");
        assert_eq!(report.resumed_chunks, 0);
        receiver.await.unwrap().expect("first run receives");
    }
    for (name, payload) in &payloads {
        assert_eq!(&fs::read(dst.path().join(name)).unwrap(), payload);
    }

    // Run 2: the retry of the same record.
    let (mut send_spec, recv) = fixed_specs(
        src.path(),
        dst.path(),
        checkpoint.path(),
        &sender_key,
        &receiver_key,
    );
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let send_report = send(send_spec, &noop)
        .await
        .expect("retry must not fail on the committed files");
    let recv_report = receiver
        .await
        .unwrap()
        .expect("retry receive must not fail on the committed files");

    let total_chunks: u64 = 8 + 8; // two 8 KiB files at 1 KiB chunks
    assert_eq!(
        send_report.resumed_chunks, total_chunks,
        "sender must skip every chunk via the full-prefix resume answer"
    );
    assert_eq!(
        recv_report.resumed_chunks, total_chunks,
        "receiver must report the committed files as fully resumed"
    );
    let (files, bytes) = count_entries(dst.path());
    assert_eq!(
        (files, bytes),
        (2, 16_384),
        "exactly the two committed files, no rename duplicates: {files} files"
    );
    for (name, payload) in &payloads {
        assert_eq!(&fs::read(dst.path().join(name)).unwrap(), payload);
    }
    let parts: Vec<_> = fs::read_dir(dst.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .to_string_lossy()
                .ends_with(ak_storage::PART_SUFFIX)
        })
        .collect();
    assert!(
        parts.is_empty(),
        "retry must leave no part files: {parts:?}"
    );
}

// ------------------------------------------------------------------
// Round 17: P1-1 chunk size limits (engine side, defense in depth)
// ------------------------------------------------------------------

#[tokio::test]
async fn send_refuses_chunk_size_above_the_engine_limit_before_connecting() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("f.txt"), b"x");
    let (mut send_spec, _, _) = specs(src.path(), dst.path(), 16);
    send_spec.chunk_size = NonZeroU32::new(16 << 20).unwrap(); // 16 MiB > 8 MiB
    send_spec.peer = SocketAddr::from(([127, 0, 0, 1], 9)); // nothing there
    let started = Instant::now();
    let err = send(send_spec, &noop)
        .await
        .expect_err("oversized chunk size must fail the spec");
    assert!(
        matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
        "got: {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "must refuse before any connection attempt, took {:?}",
        started.elapsed()
    );
}

/// The receiver must refuse an Offer whose chunk_size is zero or above
/// the engine limit BEFORE sending Accept — a legal frame carrying an
/// absurd chunk size must never reach the storage layer's per-chunk
/// allocations. The peer is told with an Error frame; whichever layer
/// refuses first (ak-wire's decoder may also reject), the job must die.
#[tokio::test]
async fn receiver_refuses_offer_chunk_size_outside_engine_limits() {
    for bad in [0u32, (8u32 << 20) + 1, u32::MAX] {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(5);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        stream
            .send_frame(&hello_frame("bad-offer", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id: [3; 16],
                    root_name: "bad".to_owned(),
                    file_count: 1,
                    total_bytes: 4,
                    chunk_size: bad,
                },
            })
            .await
            .unwrap();
        // The reply must be a refusal — an Error frame here, or a closed
        // stream if the wire decoder refused the frame first. Never an
        // Accept.
        match tokio::time::timeout(Duration::from_secs(5), stream.recv_frame(&limits)).await {
            Ok(Ok(Frame {
                message: Message::Error { code, .. },
                ..
            })) => assert_eq!(code, ErrorCode::ProtocolUnsupported),
            Ok(Ok(frame)) => panic!("chunk_size {bad}: expected refusal, got {frame:?}"),
            Ok(Err(_)) => {} // decoder/connection refused; equally fine
            Err(_) => panic!("chunk_size {bad}: receiver stayed silent"),
        }
        let outcome = receiver.await.unwrap();
        assert!(
            outcome.is_err(),
            "chunk_size {bad}: receiver must fail, got {outcome:?}"
        );
        assert_eq!(
            count_entries(dst.path()),
            (0, 0),
            "nothing may be written for a refused offer"
        );
    }
}

// ------------------------------------------------------------------
// Round 17: ManifestPage cross-frame validation (review item 3)
// ------------------------------------------------------------------

/// Wire entry fixture with real digests for `payload` at `chunk_size`.
fn chunked_wire_entry(name: &str, payload: &[u8], chunk_size: usize) -> WireEntry {
    let chunk_digests: Vec<[u8; 32]> = payload
        .chunks(chunk_size)
        .map(|c| *blake3::hash(c).as_bytes())
        .collect();
    WireEntry {
        rel_path: name.to_owned(),
        is_dir: false,
        size: payload.len() as u64,
        mtime_unix_ms: 0,
        digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
        whole_digest: *blake3::hash(payload).as_bytes(),
        chunk_digests,
    }
}

#[test]
fn manifest_assembler_validates_pages_against_the_offer() {
    let limits = engine_limits();
    let a = chunked_wire_entry("a.bin", &[1, 2, 3, 4], 2);
    let b = chunked_wire_entry("b.bin", &[5, 6, 7], 2);
    let bytes = a.size + b.size;

    // Happy path: two consecutive pages, totals reconciled.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
    assert!(!m.push([1; 16], 0, false, vec![a.clone()]).unwrap());
    assert!(m.push([1; 16], 1, true, vec![b.clone()]).unwrap());
    assert_eq!(m.entries.len(), 2);

    // Wrong job on a page.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
    assert!(m.push([2; 16], 0, true, vec![a.clone()]).is_err());

    // Page order: does not start at 0.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
    assert!(m.push([1; 16], 1, false, vec![a.clone()]).is_err());

    // Page order: repeats an index.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
    m.push([1; 16], 0, false, vec![a.clone()]).unwrap();
    assert!(m.push([1; 16], 0, true, vec![b.clone()]).is_err());

    // More entries than the offer announced.
    let mut m = ManifestAssembler::new([1; 16], 1, a.size, &limits);
    assert!(m
        .push([1; 16], 0, true, vec![a.clone(), b.clone()])
        .is_err());

    // Last page with fewer entries than announced.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
    m.push([1; 16], 0, false, vec![a.clone()]).unwrap();
    assert!(m.push([1; 16], 1, true, Vec::new()).is_err());

    // Entry sizes disagree with the offered total bytes.
    let mut m = ManifestAssembler::new([1; 16], 2, bytes + 1, &limits);
    assert!(!m.push([1; 16], 0, false, vec![a.clone()]).unwrap());
    assert!(m.push([1; 16], 1, true, vec![b.clone()]).is_err());

    // Per-page entry budget (engine double-check on top of the decoder).
    let cap = usize::from(limits.max_entries_per_page);
    let mut m = ManifestAssembler::new([1; 16], cap as u64 + 1, 0, &limits);
    let oversized: Vec<WireEntry> = (0..=cap)
        .map(|i| chunked_wire_entry(&format!("e{i}.bin"), b"", 1))
        .collect();
    assert!(m.push([1; 16], 0, false, oversized).is_err());
}

/// End-to-end wiring proof: a real receiver fed manifest pages whose
/// page_index does not start at 0 must abort with a protocol error and
/// write nothing.
#[tokio::test]
async fn receiver_rejects_manifest_pages_out_of_order() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
        .await
        .unwrap();
    let channel = endpoint
        .connect(addr, &receiver_key.fingerprint())
        .await
        .unwrap();
    let mut stream = channel.open_bi().await.unwrap();
    let limits = engine_limits();
    stream
        .send_frame(&hello_frame("bad-pages", &sender_bundle, CAP_RESUME))
        .await
        .unwrap();
    let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
    let entry = chunked_wire_entry("a.bin", &[1, 2, 3, 4], 2);
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::Offer {
                job_id: [4; 16],
                root_name: "bad-pages".to_owned(),
                file_count: 1,
                total_bytes: entry.size,
                chunk_size: 1024,
            },
        })
        .await
        .unwrap();
    let _ = stream.recv_frame(&limits).await.unwrap(); // Accept
    stream
        .send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::ManifestPage {
                job_id: [4; 16],
                page_index: 1, // must be 0
                last_page: true,
                entries: vec![entry],
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let err = receiver
        .await
        .unwrap()
        .expect_err("out-of-order manifest pages must fail the receive");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    assert_eq!(count_entries(dst.path()), (0, 0));
}

// ------------------------------------------------------------------
// Round 17: ResumeCollector cross-validation (review item 3)
// ------------------------------------------------------------------

#[test]
fn resume_collector_cross_validates_against_the_manifest() {
    let limits = engine_limits();
    let manifest = Manifest {
        chunk_size: NonZeroU32::new(1024).unwrap(),
        entries: vec![
            FileEntry::new("a.bin", 2048, 0, [1; 32], vec![[1; 32]; 2]).unwrap(),
            FileEntry::directory("sub").unwrap(),
            FileEntry::new("b.bin", 1024, 0, [2; 32], vec![[2; 32]; 1]).unwrap(),
        ],
    };

    // Happy path: valid entries across two frames.
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    assert!(!c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 0,
                verified_prefix_chunks: 2,
            }],
            true
        )
        .unwrap());
    assert!(c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 2,
                verified_prefix_chunks: 1
            }],
            false
        )
        .unwrap());
    assert_eq!(c.entries.len(), 2);

    // Out-of-range file index.
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    assert!(c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 3,
                verified_prefix_chunks: 0,
            }],
            false
        )
        .is_err());

    // Directory entries are never resumable.
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    assert!(c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 1,
                verified_prefix_chunks: 0,
            }],
            false
        )
        .is_err());

    // Repeated file index.
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    c.push(
        [7; 16],
        vec![ResumeEntry {
            file_index: 0,
            verified_prefix_chunks: 1,
        }],
        true,
    )
    .unwrap();
    assert!(c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 0,
                verified_prefix_chunks: 1,
            }],
            false
        )
        .is_err());

    // Prefix beyond the file's real chunk count.
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    assert!(c
        .push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 0,
                verified_prefix_chunks: 3,
            }],
            false
        )
        .is_err());

    // Per-frame entry budget (above limits.max_resume_entries in one
    // frame; the decoder enforces its own bound, this is the engine's).
    let cap = usize::from(limits.max_resume_entries);
    let oversized: Vec<ResumeEntry> = (0..=cap as u32)
        .map(|file_index| ResumeEntry {
            file_index,
            verified_prefix_chunks: 0,
        })
        .collect();
    let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
    assert!(c.push([7; 16], oversized, false).is_err());
}

// ------------------------------------------------------------------
// Round 17: ACK idempotency (review item 3)
// ------------------------------------------------------------------

#[test]
fn ack_ledger_confirms_once_and_counts_duplicates_and_unknowns() {
    let mut ledger = AckLedger::default();
    ledger.track(0, 0);
    ledger.track(3, 7);
    assert_eq!(ledger.apply(0, 0), AckOutcome::Confirmed);
    // Duplicate of an already-confirmed chunk: ignored, counted.
    assert_eq!(ledger.apply(0, 0), AckOutcome::DuplicateOrUnknown);
    // Chunk we never sent: ignored, counted.
    assert_eq!(ledger.apply(9, 9), AckOutcome::DuplicateOrUnknown);
    assert_eq!(ledger.apply(3, 7), AckOutcome::Confirmed);
    assert_eq!(ledger.duplicates, 2);
    assert!(ledger.inflight.is_empty(), "every tracked chunk retired");
}

/// Phantom acks must not fast-forward the window: with window = 1 the
/// peer acks chunks 0..6 honestly, then answers chunk 7 only with
/// duplicates of earlier acks. The sender must NOT let those duplicates
/// retire chunk 7's outstanding slot — it must time out instead of
/// completing (old behavior: any ack decremented the window and the
/// transfer "completed" with an unacked chunk).
#[tokio::test]
async fn duplicate_acks_do_not_retire_unacked_chunks() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    write(&src.path().join("phantom.bin"), &payload);
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
    send_spec.in_flight_window = 1;
    send_spec.idle_timeout = Duration::from_millis(600);
    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;

    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut stream = channel.accept_bi().await.unwrap();
        let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
        let limits = engine_limits();
        let mut seen = 0u64;
        let mut finalized = false;
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Tolerant read: the sender aborting on the idle timeout
                // (the expected outcome) drops the connection mid-loop.
                let frame = match stream.recv_frame(&limits).await {
                    Ok(frame) => frame,
                    Err(_) => break,
                };
                match frame.message {
                    Message::Chunk {
                        file_index,
                        chunk_index,
                        ..
                    } => {
                        seen += 1;
                        if chunk_index < 7 {
                            stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::ChunkAck {
                                        job_id,
                                        file_index,
                                        chunk_index,
                                    },
                                })
                                .await
                                .unwrap();
                        } else {
                            // Only duplicates for the last chunk.
                            let _ = stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::ChunkAck {
                                        job_id,
                                        file_index: 0,
                                        chunk_index: 0,
                                    },
                                })
                                .await;
                            let _ = stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::ChunkAck {
                                        job_id,
                                        file_index: 0,
                                        chunk_index: 0,
                                    },
                                })
                                .await;
                        }
                    }
                    Message::Finalize { .. } => {
                        finalized = true;
                        // Old behavior reached here: phantom acks had
                        // opened the window and the sender finished.
                        stream
                            .send_frame(&Frame {
                                version: ak_wire::VERSION,
                                message: Message::FinalAck {
                                    job_id,
                                    verified: true,
                                },
                            })
                            .await
                            .unwrap();
                        break;
                    }
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
        })
        .await;
        (seen, finalized)
    });

    let err = send(send_spec, &noop)
        .await
        .expect_err("phantom acks must not complete the transfer");
    assert!(
        matches!(err, EngineError::IdleTimeout(_)),
        "the unacked chunk 7 must keep the sender waiting, got {err:?}"
    );
    let (seen, finalized) = fake.await.unwrap();
    assert_eq!(seen, 8, "the fake peer must have seen every chunk");
    assert!(!finalized, "the sender must never reach Finalize");
}

/// Multistream tolerance: a receiver that prepends an unknown ack and
/// double-acks every real chunk (sloppy but harmless peer) must not
/// break the sender — duplicates and unknowns are ignored and counted,
/// and the transfer completes with every chunk sent exactly once.
#[tokio::test]
async fn multistream_tolerates_duplicate_and_unknown_acks() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload_a: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
    let payload_b: Vec<u8> = (0..4096u32).map(|i| (i % 199) as u8).collect();
    write(&src.path().join("a/one.bin"), &payload_a);
    write(&src.path().join("b/two.bin"), &payload_b);
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
    send_spec.multistream = true;
    send_spec.in_flight_window = 4;
    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;

    let sent: Arc<std::sync::Mutex<Vec<(u32, u64)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    let progress = move |event: Progress| {
        if let Progress::ChunkSent {
            file_index,
            chunk_index,
        } = event
        {
            log.lock().unwrap().push((file_index, chunk_index));
        }
    };

    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut control = channel.accept_bi().await.unwrap();
        let job_id =
            fake_receiver_preamble(&mut control, &bundle, CAP_RESUME | CAP_MULTISTREAM).await;
        let limits = engine_limits();
        // Two file streams follow the control stream, in open order.
        let mut acked = 0u64;
        for _ in 0..2 {
            let mut stream = channel.accept_bi().await.unwrap();
            // An unknown ack before anything else: must be ignored.
            let _ = stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck {
                        job_id,
                        file_index: 99,
                        chunk_index: 99,
                    },
                })
                .await;
            loop {
                match stream.recv_frame(&limits).await {
                    Ok(Frame {
                        message:
                            Message::Chunk {
                                file_index,
                                chunk_index,
                                ..
                            },
                        ..
                    }) => {
                        acked += 1;
                        stream
                            .send_frame(&Frame {
                                version: ak_wire::VERSION,
                                message: Message::ChunkAck {
                                    job_id,
                                    file_index,
                                    chunk_index,
                                },
                            })
                            .await
                            .unwrap();
                        // The duplicate: ignored by the ledger.
                        let _ = stream
                            .send_frame(&Frame {
                                version: ak_wire::VERSION,
                                message: Message::ChunkAck {
                                    job_id,
                                    file_index,
                                    chunk_index,
                                },
                            })
                            .await;
                    }
                    Ok(_) => panic!("expected only chunks on a file stream"),
                    Err(_) => break, // sender half-closed after last ack
                }
            }
        }
        match control.recv_frame(&limits).await.unwrap().message {
            Message::Finalize { .. } => {}
            other => panic!("expected Finalize, got {other:?}"),
        }
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::FinalAck {
                    job_id,
                    verified: true,
                },
            })
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if control.recv_frame(&limits).await.is_err() {
                    break;
                }
            }
        })
        .await;
        acked
    });

    let report = send(send_spec, &progress)
        .await
        .expect("duplicate/unknown acks must not break the sender");
    let acked = fake.await.unwrap();
    assert_eq!(report.files, 4); // 2 files + 2 directory entries
    assert_eq!(acked, 8 + 4, "every chunk must be acked once");
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 12, "no chunk may be re-sent: {sent:?}");
    let mut unique = sent.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), sent.len(), "chunk sends must be unique");
}

// ------------------------------------------------------------------
// Round 17: P1-2 control-stream activity budget (timing test)
// ------------------------------------------------------------------

/// P1-2 regression: a multistream transfer whose data phase alone
/// outlasts the idle budget many times over must still complete. The
/// control stream carries no frame between the ResumeRequest and the
/// Finalize (by design — the sender finalizes only after every file
/// stream finished), so the old independent control-read timeout killed
/// healthy transfers `idle` seconds into the data phase. Fixed engine:
/// data-stream events refresh one task-wide budget. Here: idle = 2 s,
/// one chunk every 300 ms, data phase > 6 s (3x idle), and the receiver
/// must verify and commit everything.
#[tokio::test]
async fn multistream_transfer_outlasting_idle_budget_completes() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(2);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    const FILES: usize = 2;
    const CHUNKS: usize = 11;
    const CHUNK_BYTES: usize = 1024;
    const DELAY: Duration = Duration::from_millis(300);
    let payloads: Vec<Vec<u8>> = (0..FILES)
        .map(|f| {
            (0..CHUNKS * CHUNK_BYTES)
                .map(|i| ((i % 251 + f * 37) % 256) as u8)
                .collect()
        })
        .collect();
    let entries: Vec<WireEntry> = payloads
        .iter()
        .enumerate()
        .map(|(f, p)| chunked_wire_entry(&format!("paced-{f}.bin"), p, CHUNK_BYTES))
        .collect();
    let receiver_fp = receiver_key.fingerprint();
    let verify_payloads = payloads.clone();

    // Fake multistream sender: control stream for the handshake, then
    // one paced stream per file (send chunk, await its ack, sleep).
    let fake = tokio::spawn(async move {
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
        let mut control = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        control
            .send_frame(&hello_frame(
                "paced",
                &sender_bundle,
                CAP_RESUME | CAP_PAUSE_RESUME | CAP_MULTISTREAM,
            ))
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // receiver Hello
        let job_id = [0xA5; 16];
        let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id,
                    root_name: "paced".to_owned(),
                    file_count: entries.len() as u64,
                    total_bytes,
                    chunk_size: CHUNK_BYTES as u32,
                },
            })
            .await
            .unwrap();
        match control.recv_frame(&limits).await.unwrap().message {
            Message::Accept { .. } => {}
            other => panic!("expected Accept, got {other:?}"),
        }
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id,
                    page_index: 0,
                    last_page: true,
                    entries: entries.clone(),
                },
            })
            .await
            .unwrap();
        match control.recv_frame(&limits).await.unwrap().message {
            Message::ResumeRequest { more, .. } => assert!(!more),
            other => panic!("expected ResumeRequest, got {other:?}"),
        }

        let started = Instant::now();
        for (file_index, payload) in payloads.iter().enumerate() {
            let mut stream = channel.open_bi().await.unwrap();
            for (chunk_index, data) in payload.chunks(CHUNK_BYTES).enumerate() {
                stream
                    .send_frame(&Frame {
                        version: ak_wire::VERSION,
                        message: Message::Chunk {
                            job_id,
                            file_index: file_index as u32,
                            chunk_index: chunk_index as u64,
                            data: data.to_vec(),
                        },
                    })
                    .await
                    .unwrap();
                match stream.recv_frame(&limits).await.unwrap().message {
                    Message::ChunkAck {
                        chunk_index: acked, ..
                    } => assert_eq!(acked, chunk_index as u64),
                    other => panic!("expected ChunkAck, got {other:?}"),
                }
                tokio::time::sleep(DELAY).await;
            }
            stream.finish_send().unwrap();
        }
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Finalize { job_id },
            })
            .await
            .unwrap();
        match tokio::time::timeout(Duration::from_secs(30), control.recv_frame(&limits)).await {
            Ok(Ok(Frame {
                message: Message::FinalAck { verified, .. },
                ..
            })) => assert!(verified, "receiver must verify the paced transfer"),
            Ok(Ok(frame)) => panic!("expected FinalAck, got {frame:?}"),
            Ok(Err(e)) => panic!("control stream died before FinalAck: {e}"),
            Err(_) => panic!("no FinalAck within 30s of Finalize"),
        }
        started.elapsed()
    });

    let recv_report = receiver
        .await
        .unwrap()
        .expect("a multistream receive must survive its data phase outlasting idle");
    let elapsed = fake.await.unwrap();
    assert!(
        elapsed >= Duration::from_secs(6),
        "the data phase must span > 6s (3x the 2s idle budget), took {elapsed:?}"
    );
    assert_eq!(recv_report.files, FILES as u64);
    assert_eq!(recv_report.bytes, (CHUNKS * CHUNK_BYTES * FILES) as u64);
    assert_eq!(recv_report.resumed_chunks, 0);
    for (f, payload) in verify_payloads.iter().enumerate() {
        assert_eq!(
            fs::read(dst.path().join(format!("paced-{f}.bin"))).unwrap(),
            *payload,
            "file {f} must be byte-identical"
        );
    }
}

/// Cross-frame per-stream consistency: a file stream that announced
/// file 0 must not deliver chunks claiming another file — the receiver
/// must abort with a protocol error instead of writing them (old code
/// acked them under the first file's identity).
#[tokio::test]
async fn multistream_file_stream_rejects_foreign_file_index() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let entry = chunked_wire_entry("owned.bin", &payload, 1024);
    let receiver_fp = receiver_key.fingerprint();
    let fake = tokio::spawn(async move {
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
        let mut control = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        control
            .send_frame(&hello_frame(
                "smuggler",
                &sender_bundle,
                CAP_RESUME | CAP_MULTISTREAM,
            ))
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap();
        let job_id = [0xB7; 16];
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id,
                    root_name: "smuggler".to_owned(),
                    file_count: 1,
                    total_bytes: entry.size,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // Accept
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id,
                    page_index: 0,
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // ResumeRequest

        // One stream announcing file 0, then a chunk claiming file 1.
        let mut stream = channel.open_bi().await.unwrap();
        let (c0, rest) = payload.split_at(1024);
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Chunk {
                    job_id,
                    file_index: 0,
                    chunk_index: 0,
                    data: c0.to_vec(),
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // ack for chunk 0
        let _ = stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Chunk {
                    job_id,
                    file_index: 1, // foreign file on this stream
                    chunk_index: 1,
                    data: rest[..1024].to_vec(),
                },
            })
            .await;
        // Hold briefly so the receiver's failure is not a race with a
        // dropped connection.
        tokio::time::sleep(Duration::from_millis(500)).await;
    });

    let err = receiver
        .await
        .unwrap()
        .expect_err("a foreign file index on a file stream must fail the receive");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    fake.await.unwrap();
    assert!(
        !dst.path().join("owned.bin").exists(),
        "nothing may be committed"
    );
}

/// M6 (batch M, 2026-11-17): concurrent file streams whose chunk frames
/// together exceed QUIC's connection-level receive window (8 MiB):
/// 3 files x 8 MiB in 4 MiB chunks. The old receiver read each new
/// stream's first frame inline in its driver loop; the sender's other
/// streams' unread data pinned the connection window, so that frame could
/// never complete. The receiver gave up after its idle budget and the
/// sender hung — the release chunk-size sweep's 4096 KiB / multistream
/// round, on Linux and Windows alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multistream_chunks_beyond_the_connection_window_share_complete() {
    const FILE_BYTES: usize = 8 * 1024 * 1024;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let files: Vec<Vec<u8>> = (0..3u32)
        .map(|file| {
            (0..FILE_BYTES as u32)
                .map(|i| i.wrapping_mul(31).wrapping_add(file * 7) as u8)
                .collect()
        })
        .collect();
    for (index, bytes) in files.iter().enumerate() {
        write(&src.path().join(format!("big-{index}.bin")), bytes);
    }
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 4096); // 4 MiB chunks
    send_spec.multistream = true;
    send_spec.in_flight_window = 8; // the engine default: up to 4 streams
    send_spec.idle_timeout = Duration::from_secs(10);
    recv.idle_timeout = Duration::from_secs(10);

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move { receive_with_addr(recv, &noop, addr_tx).await });
    send_spec.peer = addr_rx.await.unwrap();
    let (sent, received) = tokio::time::timeout(Duration::from_secs(60), async {
        let sent = send(send_spec, &noop).await;
        (sent, receiver.await.unwrap())
    })
    .await
    .expect("the transfer must finish, not deadlock on connection flow control");
    let sent = sent.expect("multistream send completes");
    let received = received.expect("multistream receive completes");
    assert_eq!(sent.bytes, (3 * FILE_BYTES) as u64);
    assert_eq!(received.bytes, sent.bytes);
    for (index, bytes) in files.iter().enumerate() {
        assert_eq!(
            &fs::read(dst.path().join(format!("big-{index}.bin"))).unwrap(),
            bytes,
            "big-{index}.bin must arrive byte-identical"
        );
    }
}

/// M6 (batch M, 2026-11-17): when a multistream receive fails, the
/// receiver closes the connection itself (`MULTISTREAM_ABORT_CLOSE_CODE`).
/// Its control reader (never cancelled, quinn reads are not cancel-safe)
/// and per-stream tasks hold stream handles that kept the QUIC connection
/// open — keep-alive PINGs included — so the sender never learned of the
/// failure and could block forever in a flow-controlled write. Same
/// failure as the foreign-file-index test; here the sender watches its
/// connection instead of dropping it.
#[tokio::test]
async fn multistream_receive_failure_closes_the_connection() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let entry = chunked_wire_entry("owned.bin", &payload, 1024);
    let receiver_fp = receiver_key.fingerprint();
    let fake = tokio::spawn(async move {
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
        let mut control = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        control
            .send_frame(&hello_frame(
                "closer",
                &sender_bundle,
                CAP_RESUME | CAP_MULTISTREAM,
            ))
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap();
        let job_id = [0xB7; 16];
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id,
                    root_name: "closer".to_owned(),
                    file_count: 1,
                    total_bytes: entry.size,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // Accept
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id,
                    page_index: 0,
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // ResumeRequest

        // One stream announcing file 0, then a chunk claiming file 1.
        let mut stream = channel.open_bi().await.unwrap();
        let (c0, rest) = payload.split_at(1024);
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Chunk {
                    job_id,
                    file_index: 0,
                    chunk_index: 0,
                    data: c0.to_vec(),
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // ack for chunk 0
        let _ = stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Chunk {
                    job_id,
                    file_index: 1, // foreign file on this stream
                    chunk_index: 1,
                    data: rest[..1024].to_vec(),
                },
            })
            .await;
        // The receiver fails on that chunk; its connection must then close
        // from ITS side while this sender keeps every handle alive.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while channel.healthy() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let closed = !channel.healthy();
        drop((stream, control));
        closed
    });

    let err = receiver
        .await
        .unwrap()
        .expect_err("a foreign file index on a file stream must fail the receive");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    assert!(
        fake.await.unwrap(),
        "the failed multistream receive must close the connection, not leave the sender waiting"
    );
}

/// M6 (batch M, 2026-11-17): a multistream receive that is CANCELLED mid
/// transfer (its task aborted, the "process restart" shape of the property
/// suite) must not leave its helper tasks holding stream handles: the
/// [`ReceiveTaskGuard`] aborts them, the last handles drop and the QUIC
/// connection closes, so the sender fails at once instead of waiting out
/// its idle budget (`IdleTimeout(30s)`) — or, blocked in a flow-controlled
/// write, forever.
#[tokio::test]
async fn cancelled_multistream_receive_closes_the_connection() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
    recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let (acked_tx, acked_rx) = tokio::sync::oneshot::channel::<()>();
    let addr = addr_rx.await.unwrap();

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let entry = chunked_wire_entry("owned.bin", &payload, 1024);
    let receiver_fp = receiver_key.fingerprint();
    let fake = tokio::spawn(async move {
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
        let mut control = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        control
            .send_frame(&hello_frame(
                "cancelled",
                &sender_bundle,
                CAP_RESUME | CAP_MULTISTREAM,
            ))
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap();
        let job_id = [0xB7; 16];
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id,
                    root_name: "cancelled".to_owned(),
                    file_count: 1,
                    total_bytes: entry.size,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // Accept
        control
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id,
                    page_index: 0,
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        let _ = control.recv_frame(&limits).await.unwrap(); // ResumeRequest

        // One file stream: chunk 0 and its ack put the receive mid transfer.
        let mut stream = channel.open_bi().await.unwrap();
        let c0 = &payload[..1024];
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Chunk {
                    job_id,
                    file_index: 0,
                    chunk_index: 0,
                    data: c0.to_vec(),
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // ack for chunk 0
                                                           // Mid transfer: let the test cancel the receive now.
        acked_tx.send(()).unwrap();
        // The receive is cancelled; its connection must then close from ITS
        // side while this sender keeps every handle alive.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while channel.healthy() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let closed = !channel.healthy();
        drop((stream, control));
        closed
    });

    acked_rx
        .await
        .expect("the fake sender reached the data phase");
    receiver.abort();
    let _ = receiver.await;
    assert!(
        fake.await.unwrap(),
        "a cancelled multistream receive must close the connection, not leave the sender waiting"
    );
}

// ------------------------------------------------------------------
// Wave 21: on-channel admission handshake + per-task staging
// ------------------------------------------------------------------

/// A fixed admission context for the on-channel tests: one task id,
/// one runtime-minted permit, one code group, one registry, one
/// pairing relation. Built through the validating constructor like
/// production code.
fn test_admission_ctx(task_id: [u8; 16]) -> AdmissionCtx {
    AdmissionCtx::new(task_id, [0x5C; 16], [0x77; 32], [0x91; 32], [0xB3; 16])
        .expect("test admission context is valid")
}

/// A fake wire peer holding one open bidirectional stream plus the
/// objects that keep the connection alive. Used to hand-write
/// admission frames against the real engine over real loopback QUIC.
struct FakePeer {
    tx: DirectSendStream,
    rx: DirectRecvStream,
    _channel: DirectChannel,
    _endpoint: DirectEndpoint,
}

fn wire(message: Message) -> Frame {
    Frame {
        version: ak_wire::VERSION,
        message,
    }
}

fn now_ms_for_test() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Bind a loopback endpoint, then spawn the accept + Hello exchange
/// (fake speaks first with `capabilities`) so the engine sender can
/// start connecting while it runs. Returns the address to connect to,
/// the fake's pinned fingerprint, and the join handle that yields the
/// fake's stream halves once the Hellos are through.
async fn fake_receiver_side(
    capabilities: u16,
) -> (tokio::task::JoinHandle<FakePeer>, SocketAddr, Fingerprint) {
    let key = DeviceKeyPair::generate();
    let identity = ak_identity::tls::make_self_signed(&key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let fingerprint = key.fingerprint();
    let task = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let stream = channel.accept_bi().await.unwrap();
        let (mut tx, mut rx) = stream.split();
        tx.send_frame(&hello_frame("fake-recv", &identity, capabilities))
            .await
            .unwrap();
        let hello = rx.recv_frame(&engine_limits()).await.unwrap();
        assert!(
            matches!(hello.message, Message::Hello { .. }),
            "engine must hello first: {:?}",
            hello.message
        );
        FakePeer {
            tx,
            rx,
            _channel: channel,
            _endpoint: endpoint,
        }
    });
    (task, addr, fingerprint)
}

/// Dial the engine's receiver endpoint, open the (sender-side) stream,
/// exchange Hello, and hand the fake's halves to the caller.
async fn fake_sender_side(addr: SocketAddr, pinned: Fingerprint, capabilities: u16) -> FakePeer {
    let key = DeviceKeyPair::generate();
    let identity = ak_identity::tls::make_self_signed(&key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
        .await
        .unwrap();
    let channel = endpoint.connect(addr, &pinned).await.unwrap();
    let stream = channel.open_bi().await.unwrap();
    let (mut tx, mut rx) = stream.split();
    tx.send_frame(&hello_frame("fake-send", &identity, capabilities))
        .await
        .unwrap();
    let hello = rx.recv_frame(&engine_limits()).await.unwrap();
    assert!(
        matches!(hello.message, Message::Hello { .. }),
        "engine must hello first: {:?}",
        hello.message
    );
    FakePeer {
        tx,
        rx,
        _channel: channel,
        _endpoint: endpoint,
    }
}

fn on_channel_send_spec(
    src: &Path,
    peer: SocketAddr,
    pinned: Fingerprint,
    identity: TlsBundle,
) -> SendSpec {
    let mut spec = SendSpec::new(src.to_path_buf(), peer, pinned, identity);
    spec.checkpoint_dir = std::env::temp_dir().join(format!(
        "ak-tests-onchannel-send-{}-{}",
        std::process::id(),
        now_ms_for_test()
    ));
    spec
}

fn on_channel_recv_spec(dst: &Path, identity: TlsBundle) -> ReceiveSpec {
    let mut spec = ReceiveSpec::new(dst.to_path_buf(), identity);
    spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    spec.checkpoint_dir = std::env::temp_dir().join(format!(
        "ak-tests-onchannel-recv-{}-{}",
        std::process::id(),
        now_ms_for_test()
    ));
    spec
}

/// Read the next TaskOpen from the fake's stream (the engine sender
/// just produced one) through the admission-frame reader, so the
/// wave-23 113-byte payload passes the header budget gate. Returns
/// every field: job, permit, group, registry, pairing, direction.
async fn read_task_open(
    fake: &mut FakePeer,
) -> (JobId, [u8; 16], [u8; 32], [u8; 32], [u8; 16], u8) {
    match recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(5))
        .await
        .unwrap()
        .message
    {
        Message::TaskOpen {
            job_id,
            permit_id,
            group,
            group_registry,
            pairing_id,
            direction,
        } => (
            job_id,
            permit_id,
            group,
            group_registry,
            pairing_id,
            direction,
        ),
        other => panic!("expected TaskOpen, got {other:?}"),
    }
}

/// Read the next Admission from the fake's stream through the
/// admission-frame reader (the wave-23 38-byte payload).
async fn read_admission(fake: &mut FakePeer) -> (JobId, [u8; 16], bool, AdmissionReason, u32) {
    match recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(5))
        .await
        .unwrap()
        .message
    {
        Message::Admission {
            job_id,
            permit_id,
            granted,
            reason,
            lease_ms,
        } => (job_id, permit_id, granted, reason, lease_ms),
        other => panic!("expected Admission, got {other:?}"),
    }
}

/// Spawn the engine sender toward `addr` with the test admission
/// context (the caller plays the receiving wire peer).
fn spawn_on_channel_sender(
    src: PathBuf,
    addr: SocketAddr,
    pinned: Fingerprint,
    task_id: [u8; 16],
) -> tokio::task::JoinHandle<Result<Report, EngineError>> {
    tokio::spawn(async move {
        let key = DeviceKeyPair::generate();
        let identity = ak_identity::tls::make_self_signed(&key);
        let spec = on_channel_send_spec(&src, addr, pinned, identity);
        let dialer = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &spec.identity)
            .await
            .unwrap();
        let channel = dialer.connect(spec.peer, &spec.pinned).await.unwrap();
        send_on_channel(channel, spec, test_admission_ctx(task_id), &noop).await
    })
}

/// After the engine sender failed, prove no further frame (no Offer,
/// no ManifestPage, no Chunk) can still arrive: the connection is gone.
async fn assert_wire_silent_after(fake: &mut FakePeer) {
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        let mut frames = Vec::new();
        loop {
            match fake.rx.recv_frame(&engine_limits()).await {
                Ok(frame) => frames.push(frame.message),
                Err(_) => break frames,
            }
        }
    })
    .await
    .expect("the closed connection must end the read loop");
    assert!(
        observed.is_empty(),
        "frames arrived after the denial: {observed:?}"
    );
}

/// Granted full round trip: real send_on_channel against real
/// receive_on_channel, admission granted, file bytes identical, the
/// decider saw exactly the wire admission context (group + registry +
/// pairing, wave 23), the grant carried the bounded lease, and the
/// per-task staging tree was discarded at the terminal state.
#[tokio::test]
async fn on_channel_admission_granted_completes_full_round_trip() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..30_000u32).map(|i| (i % 199) as u8).collect();
    write(&src.path().join("granted.bin"), &payload);

    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();

    let task_id: [u8; 16] = [0x42; 16];
    let ctx = test_admission_ctx(task_id);
    let seen_requests: Arc<std::sync::Mutex<Vec<AdmissionRequest>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let decider_requests = Arc::clone(&seen_requests);
    let decider = move |request: &AdmissionRequest| {
        decider_requests.lock().unwrap().push(*request);
        AdmissionDecision::Admit
    };

    let recv_spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        receive_on_channel(channel, recv_spec, &decider, &noop).await
    });

    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, receiver_fp, task_id);

    let recv_report = receiver.await.unwrap().expect("receive completes");
    let send_report = sender.await.unwrap().expect("send completes");
    assert_eq!(send_report.files, 1);
    assert_eq!(send_report.bytes, payload.len() as u64);
    assert_eq!(recv_report.bytes, payload.len() as u64);
    assert_eq!(fs::read(dst.path().join("granted.bin")).unwrap(), payload);
    // Wave 23: the decider saw the complete admission context — the
    // code group, the registry and the pairing relation the sender's
    // AdmissionCtx carried.
    assert_eq!(
        *seen_requests.lock().unwrap(),
        vec![AdmissionRequest {
            group: ctx.group,
            group_registry: ctx.group_registry,
            pairing_id: ctx.pairing_id,
        }]
    );
    // Terminal-state cleanup: the task's staging tree is gone.
    assert!(
        !ak_storage::staging_path(dst.path(), task_id).exists(),
        "staging tree must be discarded after completion"
    );
}

/// A SerialWait denial fails the send immediately with the dedicated
/// error and not one post-denial frame (no Offer) is ever observable.
#[tokio::test]
async fn on_channel_deny_serial_wait_fails_immediately_without_offer() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"deny me");
    let task_id: [u8; 16] = [0x11; 16];

    let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
    let mut fake = fake_task.await.unwrap();

    let (job_id, permit_id, group, registry, pairing, direction) = read_task_open(&mut fake).await;
    assert_eq!(job_id, task_id);
    assert_eq!(permit_id, [0x5C; 16], "attempt 0 runs on the ctx permit");
    assert_eq!(group, [0x77; 32]);
    assert_eq!(registry, [0x91; 32]);
    assert_eq!(pairing, [0xB3; 16]);
    assert_eq!(direction, ak_wire::DIRECTION_SEND);
    fake.tx
        .send_frame(&wire(Message::Admission {
            job_id,
            permit_id,
            granted: false,
            reason: AdmissionReason::SerialWait,
            lease_ms: 0,
        }))
        .await
        .unwrap();

    let err = sender
        .await
        .unwrap()
        .expect_err("SerialWait denial must fail the send");
    assert!(
        matches!(
            err,
            EngineError::AdmissionDenied {
                reason: AdmissionReason::SerialWait
            }
        ),
        "got: {err:?}"
    );
    assert_wire_silent_after(&mut fake).await;
}

/// A Capacity denial ends the handshake after exactly one TaskOpen
/// (no resubmit — only StalePermit retries) and stays wire-silent.
#[tokio::test]
async fn on_channel_deny_capacity_is_final_and_silent() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"capacity");
    let task_id: [u8; 16] = [0x22; 16];

    let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
    let mut fake = fake_task.await.unwrap();

    let (job_id, permit_id, _, _, _, _) = read_task_open(&mut fake).await;
    fake.tx
        .send_frame(&wire(Message::Admission {
            job_id,
            permit_id,
            granted: false,
            reason: AdmissionReason::Capacity,
            lease_ms: 0,
        }))
        .await
        .unwrap();

    let err = sender.await.unwrap().expect_err("Capacity denial fails");
    assert!(
        matches!(
            err,
            EngineError::AdmissionDenied {
                reason: AdmissionReason::Capacity
            }
        ),
        "got: {err:?}"
    );
    assert_wire_silent_after(&mut fake).await;
}

/// A StalePermit denial makes the sender resubmit a fresh TaskOpen
/// (new permit id); the granted second attempt continues into the
/// frozen path (the fake then Rejects the Offer — proving the protocol
/// resumed after admission, and only after it).
#[tokio::test]
async fn on_channel_stale_permit_resubmits_fresh_task_open() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"stale permit");
    let task_id: [u8; 16] = [0x33; 16];

    let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
    let mut fake = fake_task.await.unwrap();

    // First attempt: stale.
    let (job_id, permit_a, _, _, _, _) = read_task_open(&mut fake).await;
    assert_eq!(permit_a, [0x5C; 16], "attempt 0 runs on the ctx permit");
    fake.tx
        .send_frame(&wire(Message::Admission {
            job_id,
            permit_id: permit_a,
            granted: false,
            reason: AdmissionReason::StalePermit,
            lease_ms: 0,
        }))
        .await
        .unwrap();
    // Second attempt: grant, and expect the AdmissionAck.
    let (job_id, permit_b, _, _, _, _) = read_task_open(&mut fake).await;
    assert_ne!(permit_a, permit_b, "resubmit must use a fresh permit id");
    assert_eq!(job_id, task_id);
    fake.tx
        .send_frame(&wire(Message::Admission {
            job_id,
            permit_id: permit_b,
            granted: true,
            reason: AdmissionReason::Ok,
            lease_ms: ADMISSION_LEASE_MS,
        }))
        .await
        .unwrap();
    match fake.rx.recv_frame(&engine_limits()).await.unwrap().message {
        Message::AdmissionAck {
            job_id: acked_job,
            permit_id,
        } => {
            assert_eq!(acked_job, task_id);
            assert_eq!(permit_id, permit_b);
        }
        other => panic!("expected AdmissionAck, got {other:?}"),
    }
    // The frozen path now runs: the next frame is the Offer; the fake
    // rejects it and the sender surfaces the plain rejection.
    match fake.rx.recv_frame(&engine_limits()).await.unwrap().message {
        Message::Offer { job_id, .. } => {
            fake.tx
                .send_frame(&wire(Message::Reject {
                    job_id,
                    reason: ak_core::error::ErrorCode::TargetRevoked,
                }))
                .await
                .unwrap();
        }
        other => panic!("expected Offer after admission, got {other:?}"),
    }
    let err = sender
        .await
        .unwrap()
        .expect_err("the fake rejected the offer");
    assert!(matches!(err, EngineError::Rejected(_)), "got: {err:?}");
}

/// A peer whose Hello lacks CAP_SCHEDULER cannot silently downgrade a
/// scheduled send: PROTOCOL_UNSUPPORTED, before any TaskOpen is sent.
#[tokio::test]
async fn on_channel_peer_without_scheduler_cap_is_protocol_unsupported() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"no cap");
    let task_id: [u8; 16] = [0x44; 16];

    let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME).await;
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
    let mut fake = fake_task.await.unwrap();

    let err = sender
        .await
        .unwrap()
        .expect_err("peer without CAP_SCHEDULER must be refused");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    // No TaskOpen ever left: the fake sees the connection close with
    // nothing following its own Hello.
    assert_wire_silent_after(&mut fake).await;
}

/// Receiver side of a denial: the decider's reason is answered on the
/// wire (granted = false) and the local call fails with the same
/// reason; no receive body ran (no Offer was ever read).
#[tokio::test]
async fn receive_on_channel_denial_is_answered_on_the_wire() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Deny(AdmissionReason::Capacity);
        receive_on_channel(channel, spec, &decider, &noop).await
    });

    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    let task_id: [u8; 16] = [0x55; 16];
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [9; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    let (job_id, permit_id, granted, reason, lease_ms) = read_admission(&mut fake).await;
    assert_eq!(job_id, task_id);
    assert_eq!(permit_id, [9; 16]);
    assert!(!granted);
    assert_eq!(reason, AdmissionReason::Capacity);
    assert_eq!(lease_ms, 0, "denials carry no lease");
    let err = receiver
        .await
        .unwrap()
        .expect_err("the decider denied the task");
    assert!(
        matches!(
            err,
            EngineError::AdmissionDenied {
                reason: AdmissionReason::Capacity
            }
        ),
        "got: {err:?}"
    );
    // Nothing of the receive body ran: no destination file, no staging.
    assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
}

/// A grant is not enough: the receiver waits for the sender's
/// AdmissionAck before the body — with it missing, the idle timeout
/// fires (no Offer is read in the meantime).
#[tokio::test]
async fn receive_on_channel_grant_waits_for_admission_ack() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
    spec.idle_timeout = Duration::from_secs(1);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec, &decider, &noop).await
    });

    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    let task_id: [u8; 16] = [0x66; 16];
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [4; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    let (_, _, granted, reason, lease_ms) = read_admission(&mut fake).await;
    assert!(granted);
    assert_eq!(reason, AdmissionReason::Ok);
    assert_eq!(
        lease_ms, ADMISSION_LEASE_MS,
        "grants carry the bounded lease"
    );
    // Deliberately never send the AdmissionAck.
    let err = receiver
        .await
        .unwrap()
        .expect_err("missing AdmissionAck must time out");
    assert!(
        matches!(err, EngineError::IdleTimeout(_) | EngineError::Stream(_)),
        "got: {err:?}"
    );
    assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
}

/// The all-zero task id is the staging sentinel: it is refused as a
/// protocol violation, not handed to the decider or the staging layer.
#[tokio::test]
async fn receive_on_channel_refuses_the_zero_task_sentinel() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec, &decider, &noop).await
    });

    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: [0u8; 16],
            permit_id: [1; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    let err = receiver
        .await
        .unwrap()
        .expect_err("zero task id must be refused");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
}

// ------------------------------------------------------------------
// Wave 23: on-channel hardening (review P1-4)
// ------------------------------------------------------------------

/// AdmissionCtx::new is the controlled constructor: a valid identity
/// set builds the context, and every all-zero field is refused with the
/// dedicated error naming the offender — the context cannot be
/// assembled by field assignment from outside the crate.
#[test]
fn admission_ctx_constructor_validates_every_identity_field() {
    let ctx = AdmissionCtx::new([1; 16], [2; 16], [3; 32], [4; 32], [5; 16])
        .expect("a fully non-zero identity set is valid");
    assert_eq!(
        ctx,
        AdmissionCtx {
            task_id: [1; 16],
            permit_id: [2; 16],
            group: [3; 32],
            group_registry: [4; 32],
            pairing_id: [5; 16],
        }
    );
    for (field, attempt) in [
        (
            "task_id",
            AdmissionCtx::new([0; 16], [2; 16], [3; 32], [4; 32], [5; 16]),
        ),
        (
            "permit_id",
            AdmissionCtx::new([1; 16], [0; 16], [3; 32], [4; 32], [5; 16]),
        ),
        (
            "group_registry",
            AdmissionCtx::new([1; 16], [2; 16], [3; 32], [0; 32], [5; 16]),
        ),
        (
            "pairing_id",
            AdmissionCtx::new([1; 16], [2; 16], [3; 32], [4; 32], [0; 16]),
        ),
    ] {
        match attempt {
            Err(EngineError::InvalidAdmissionCtx { field: named }) => {
                assert_eq!(named, field, "the error must name the offending field");
            }
            other => panic!("all-zero {field} must be refused, got {other:?}"),
        }
    }
    // The all-zero group is deliberately NOT refused by the constructor
    // (the wave-23 contract lists task/permit/registry/pairing); the
    // receiver-side decider remains the authority over code groups.
    assert!(AdmissionCtx::new([1; 16], [2; 16], [0; 32], [4; 32], [5; 16]).is_ok());
}

/// A runtime-injected channel whose TLS peer is NOT the pinned
/// fingerprint is refused as the very first step of `send_on_channel`
/// (review P1-4): PeerMismatch, connection closed with the dedicated
/// code, and not one protocol byte (no stream, no Hello, no TaskOpen).
#[tokio::test]
async fn send_on_channel_refuses_a_channel_to_the_wrong_peer() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"wrong peer");
    let task_id: [u8; 16] = [0x88; 16];

    // A real loopback peer the channel actually connects to...
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let fp = receiver_key.fingerprint();

    // ...but the spec pins a DIFFERENT device's fingerprint.
    let pinned = DeviceKeyPair::generate().fingerprint();
    let pinned_for_spec = pinned;
    let sender = tokio::spawn(async move {
        let key = DeviceKeyPair::generate();
        let identity = ak_identity::tls::make_self_signed(&key);
        let spec = on_channel_send_spec(src.path(), addr, pinned_for_spec, identity);
        let dialer = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &spec.identity)
            .await
            .unwrap();
        // Connect to the REAL peer so the channel itself is healthy —
        // only the pinned identity check can refuse it.
        let channel = dialer.connect(spec.peer, &fp).await.unwrap();
        send_on_channel(channel, spec, test_admission_ctx(task_id), &noop).await
    });

    // The peer observes the refusal as a closed connection on which no
    // stream was ever opened — not even the engine's Hello moved.
    let observer = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let stream = channel.accept_bi().await;
        assert!(
            stream.is_err(),
            "no stream may be opened after a pinned-identity refusal"
        );
    });

    let err = sender
        .await
        .unwrap()
        .expect_err("a channel to the wrong peer must be refused");
    match &err {
        EngineError::PeerMismatch { actual, expected } => {
            assert_eq!(actual, &fp.to_hex(), "actual peer is the connected one");
            assert_eq!(expected, &pinned.to_hex());
        }
        other => panic!("expected PeerMismatch, got {other:?}"),
    }
    observer.await.unwrap();
}

/// The reverse direction of the send-side refusal (the gap review
/// P1-4 called out): `receive_on_channel` exists for scheduled tasks,
/// so a legacy sender whose Hello lacks CAP_SCHEDULER must NOT be
/// silently downgraded into the unadmitted receive body. It fails with
/// PROTOCOL_UNSUPPORTED, the channel closes, and nothing is received.
#[tokio::test]
async fn receive_on_channel_refuses_a_sender_without_scheduler_cap() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec, &decider, &noop).await
    });

    // A legacy sender: no CAP_SCHEDULER, straight to its Offer. Inline
    // dial (not fake_sender_side): the engine's refusal-close races the
    // fake's read of the engine's Hello, so both outcomes are accepted
    // — what the test pins down is the refusal itself, below.
    let sender_key = DeviceKeyPair::generate();
    let sender_identity = ak_identity::tls::make_self_signed(&sender_key);
    let sender_endpoint =
        DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_identity)
            .await
            .unwrap();
    let channel = sender_endpoint
        .connect(addr, &receiver_key.fingerprint())
        .await
        .unwrap();
    let stream = channel.open_bi().await.unwrap();
    let (mut tx, mut rx) = stream.split();
    tx.send_frame(&hello_frame("fake-send", &sender_identity, CAP_RESUME))
        .await
        .unwrap();
    let _ = rx.recv_frame(&engine_limits()).await;
    let mut fake = FakePeer {
        tx,
        rx,
        _channel: channel,
        _endpoint: sender_endpoint,
    };
    // The Offer write itself may also race the refusal-close; what
    // matters is that the engine never reads it as an unadmitted
    // receive.
    let _ = fake
        .tx
        .send_frame(&wire(Message::Offer {
            job_id: [0x9; 16],
            root_name: "legacy".into(),
            file_count: 1,
            total_bytes: 1,
            chunk_size: 1024,
        }))
        .await;

    let err = receiver
        .await
        .unwrap()
        .expect_err("a legacy sender must not be downgraded into the receive body");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    // The engine's own Hello was its last byte; the connection is gone
    // and nothing was written to the destination.
    assert_wire_silent_after(&mut fake).await;
    assert_eq!(
        fs::read_dir(dst.path()).unwrap().count(),
        0,
        "no file may land from a refused legacy sender"
    );
}

/// Wire-level fail-closed checks in front of the decider (wave 23): a
/// TaskOpen with the all-zero pairing id, the all-zero registry, or a
/// recv-direction tag is a protocol violation — the decider is never
/// consulted, the call fails with PROTOCOL_UNSUPPORTED and no staging
/// appears. Mirrors `AdmissionCtx::new` on the sending side.
#[tokio::test]
async fn receive_on_channel_refuses_sentinel_and_wrong_direction_task_opens() {
    for case in ["zero pairing id", "zero registry", "recv direction"] {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let consulted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&consulted);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = move |_: &AdmissionRequest| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                AdmissionDecision::Admit
            };
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        let task_id: [u8; 16] = [0x99; 16];
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [7; 16],
                group: [0x77; 32],
                group_registry: match case {
                    "zero registry" => [0u8; 32],
                    _ => [0x91; 32],
                },
                pairing_id: match case {
                    "zero pairing id" => [0u8; 16],
                    _ => [0xB3; 16],
                },
                direction: match case {
                    "recv direction" => ak_wire::DIRECTION_RECV,
                    _ => ak_wire::DIRECTION_SEND,
                },
            }))
            .await
            .unwrap();

        let err = receiver.await.unwrap().unwrap_err();
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "{case}: got {err:?}"
        );
        assert!(
            !consulted.load(std::sync::atomic::Ordering::SeqCst),
            "{case}: the decider must not be consulted"
        );
        assert!(
            !ak_storage::staging_path(dst.path(), task_id).exists(),
            "{case}: no staging may appear"
        );
    }
}

/// A wave-20-length TaskOpen (96-byte payload, before pairing_id and
/// direction existed) is rejected on a real channel — the same-type
/// shorter frame runs out mid-field; the two versions do not
/// interoperate (both ends must ship the same version).
#[tokio::test]
async fn receive_on_channel_refuses_wave_20_length_task_open() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec, &decider, &noop).await
    });

    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    // Hand-crafted wave-20 frame: job(16) + permit(16) + group(32)
    // + registry(32) = 96 payload bytes, no pairing_id/direction.
    let task_id: [u8; 16] = [0xAA; 16];
    let mut payload = Vec::new();
    payload.extend_from_slice(&task_id);
    payload.extend_from_slice(&[7u8; 16]);
    payload.extend_from_slice(&[0x77u8; 32]);
    payload.extend_from_slice(&[0x91u8; 32]);
    assert_eq!(payload.len(), 96);
    let mut bytes = vec![ak_wire::VERSION];
    bytes.extend_from_slice(&ak_wire::TYPE_TASK_OPEN.to_be_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&payload);
    fake.tx.send_bytes(&bytes).await.unwrap();

    let err = receiver
        .await
        .unwrap()
        .expect_err("the wave-20 frame must be rejected");
    assert!(
        matches!(err, EngineError::Stream(_)),
        "decode must reject the stale length, got {err:?}"
    );
    assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
}

/// A grant must carry a positive lease (wave 23): the sender refuses a
/// granted Admission with lease_ms = 0 as a protocol violation and
/// stays wire-silent afterwards.
#[tokio::test]
async fn on_channel_granted_admission_with_zero_lease_is_refused() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("f.bin"), b"no lease");
    let task_id: [u8; 16] = [0xBB; 16];

    let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
    let mut fake = fake_task.await.unwrap();

    let (job_id, permit_id, _, _, _, _) = read_task_open(&mut fake).await;
    fake.tx
        .send_frame(&wire(Message::Admission {
            job_id,
            permit_id,
            granted: true,
            reason: AdmissionReason::Ok,
            lease_ms: 0,
        }))
        .await
        .unwrap();

    let err = sender
        .await
        .unwrap()
        .expect_err("a granted admission with a zero lease must be refused");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    // No AdmissionAck, no Offer: the handshake failed closed.
    assert_wire_silent_after(&mut fake).await;
}

/// Hand-written full send protocol against receive_on_channel with a
/// poisoned chunk digest: the verified part file must live inside the
/// task's staging tree (not beside the final name), and a retry of the
/// same task id resumes from it (verified prefix 1) and completes with
/// the tree discarded.
#[tokio::test]
async fn scheduled_receive_stages_parts_and_resumes_within_the_task_tree() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let pinned = receiver_key.fingerprint();

    const CHUNK: usize = 4096;
    let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
    let (a, b) = payload.split_at(CHUNK);
    let digest = |bytes: &[u8]| {
        let mut out = [0u8; 32];
        out.copy_from_slice(blake3::hash(bytes).as_bytes());
        out
    };
    let whole = digest(&payload);
    let chunk_digests = vec![digest(a), digest(b)];
    let task_id: [u8; 16] = [0xAB; 16];
    let limits = engine_limits();

    // ---- run 1: correct chunk 0, manifest poisons chunk 1's digest.
    let mut spec1 = on_channel_recv_spec(dst.path(), receiver_identity.clone());
    spec1.idle_timeout = Duration::from_secs(5);
    let receiver1 = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec1, &decider, &noop).await
    });
    let mut fake = fake_sender_side(addr, pinned, CAP_RESUME | CAP_SCHEDULER).await;
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [2; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    let (_, _, granted, _, _) = read_admission(&mut fake).await;
    assert!(granted);
    fake.tx
        .send_frame(&wire(Message::AdmissionAck {
            job_id: task_id,
            permit_id: [2; 16],
        }))
        .await
        .unwrap();
    let job_id: JobId = [0xCD; 16];
    fake.tx
        .send_frame(&wire(Message::Offer {
            job_id,
            root_name: "poison".into(),
            file_count: 1,
            total_bytes: payload.len() as u64,
            chunk_size: CHUNK as u32,
        }))
        .await
        .unwrap();
    match fake.rx.recv_frame(&limits).await.unwrap().message {
        Message::Accept { job_id: acked } => assert_eq!(acked, job_id),
        other => panic!("expected Accept, got {other:?}"),
    }
    let entry = ak_core::manifest::FileEntry::with_digest_alg(
        "staged.bin",
        payload.len() as u64,
        1_700_000_000_000,
        ak_core::manifest::DIGEST_ALG_BLAKE3,
        whole,
        chunk_digests.clone(),
    )
    .unwrap();
    let mut wire_entry = WireEntry::from(&entry);
    wire_entry.chunk_digests[1] = [0xEE; 32]; // poisoned
    fake.tx
        .send_frame(&wire(Message::ManifestPage {
            job_id,
            page_index: 0,
            last_page: true,
            entries: vec![wire_entry],
        }))
        .await
        .unwrap();
    match fake.rx.recv_frame(&limits).await.unwrap().message {
        Message::ResumeRequest { files, more, .. } => {
            assert!(!more);
            assert_eq!(files[0].verified_prefix_chunks, 0, "fresh task");
        }
        other => panic!("expected ResumeRequest, got {other:?}"),
    }
    for (index, data) in [(0u64, a), (1u64, b)] {
        fake.tx
            .send_frame(&wire(Message::Chunk {
                job_id,
                file_index: 0,
                chunk_index: index,
                data: data.to_vec(),
            }))
            .await
            .unwrap();
    }
    let err = receiver1
        .await
        .unwrap()
        .expect_err("poisoned chunk digest must fail the receive");
    assert!(
        !matches!(err, EngineError::AdmissionDenied { .. }),
        "admission itself succeeded: {err:?}"
    );

    // Staging landing point: chunk 0 verified inside the task's tree.
    let staged_part = ak_storage::staging_path(dst.path(), task_id).join("staged.bin.akpart");
    assert!(
        staged_part.exists(),
        "verified part must live in the task staging tree"
    );
    assert_eq!(
        fs::metadata(&staged_part).unwrap().len(),
        CHUNK as u64,
        "exactly the verified prefix (chunk 0) survived"
    );
    assert!(
        !dst.path().join("staged.bin").exists(),
        "nothing committed beside the final name"
    );
    assert!(
        !dst.path().join("staged.bin.akpart").exists(),
        "no legacy same-directory part file may appear for a scheduled task"
    );

    // ---- run 2: same task id, corrected digest, resume + completion.
    // Run 1's endpoint moved into its spawn, so run 2 accepts on a
    // fresh endpoint bound to a fresh port (same destination dir, which
    // is what carries the staged prefix).
    let receiver_key2 = DeviceKeyPair::generate();
    let receiver_identity2 = ak_identity::tls::make_self_signed(&receiver_key2);
    let endpoint2 =
        DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity2)
            .await
            .unwrap();
    let addr2 = endpoint2.local_addr().unwrap();
    let mut spec2 = on_channel_recv_spec(dst.path(), receiver_identity2);
    spec2.idle_timeout = Duration::from_secs(5);
    let receiver2 = tokio::spawn(async move {
        let channel = endpoint2.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel(channel, spec2, &decider, &noop).await
    });

    let mut fake2 = fake_sender_side(
        addr2,
        receiver_key2.fingerprint(),
        CAP_RESUME | CAP_SCHEDULER,
    )
    .await;
    fake2
        .tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [3; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    let (_, _, granted, _, _) = read_admission(&mut fake2).await;
    assert!(granted);
    fake2
        .tx
        .send_frame(&wire(Message::AdmissionAck {
            job_id: task_id,
            permit_id: [3; 16],
        }))
        .await
        .unwrap();
    fake2
        .tx
        .send_frame(&wire(Message::Offer {
            job_id,
            root_name: "poison".into(),
            file_count: 1,
            total_bytes: payload.len() as u64,
            chunk_size: CHUNK as u32,
        }))
        .await
        .unwrap();
    match fake2.rx.recv_frame(&limits).await.unwrap().message {
        Message::Accept { job_id: acked } => assert_eq!(acked, job_id),
        other => panic!("expected Accept, got {other:?}"),
    }
    fake2
        .tx
        .send_frame(&wire(Message::ManifestPage {
            job_id,
            page_index: 0,
            last_page: true,
            entries: vec![WireEntry::from(&entry)],
        }))
        .await
        .unwrap();
    match fake2.rx.recv_frame(&limits).await.unwrap().message {
        Message::ResumeRequest { files, more, .. } => {
            assert!(!more);
            assert_eq!(
                files[0].verified_prefix_chunks, 1,
                "retry of the same task id must resume from the staged prefix"
            );
        }
        other => panic!("expected ResumeRequest, got {other:?}"),
    }
    fake2
        .tx
        .send_frame(&wire(Message::Chunk {
            job_id,
            file_index: 0,
            chunk_index: 1,
            data: b.to_vec(),
        }))
        .await
        .unwrap();
    match fake2.rx.recv_frame(&limits).await.unwrap().message {
        Message::ChunkAck {
            job_id: acked,
            file_index,
            chunk_index,
        } => {
            assert_eq!(acked, job_id);
            assert_eq!(file_index, 0);
            assert_eq!(chunk_index, 1);
        }
        other => panic!("expected ChunkAck, got {other:?}"),
    }
    fake2
        .tx
        .send_frame(&wire(Message::Finalize { job_id }))
        .await
        .unwrap();
    match fake2.rx.recv_frame(&limits).await.unwrap().message {
        Message::FinalAck {
            job_id: acked,
            verified,
        } => {
            assert_eq!(acked, job_id);
            assert!(verified);
        }
        other => panic!("expected FinalAck, got {other:?}"),
    }
    let report = receiver2.await.unwrap().expect("run 2 completes");
    assert_eq!(report.bytes, payload.len() as u64);
    assert_eq!(report.resumed_chunks, 1, "one chunk was resumed");
    assert_eq!(fs::read(dst.path().join("staged.bin")).unwrap(), payload);
    assert!(
        !ak_storage::staging_path(dst.path(), task_id).exists(),
        "terminal state discards the task's staging tree"
    );
}

// ------------------------------------------------------------------
// Wave 28: receive-side transfer consent (TransferConsent hook)
// ------------------------------------------------------------------

/// One scheduled receive driven by the hand-written fake up to (and
/// just past) the Offer. Holds the live channel so the connection
/// outlives the harness unless the receiver itself ends it.
struct ConsentHarness {
    receiver: tokio::task::JoinHandle<Result<Report, EngineError>>,
    tx: Option<DirectSendStream>,
    rx: Option<DirectRecvStream>,
    _channel: DirectChannel,
    _endpoint: DirectEndpoint,
    job_id: JobId,
    task_id: [u8; 16],
    dst: tempfile::TempDir,
    checkpoint_dir: PathBuf,
    _ckpt_keepalive: tempfile::TempDir,
    /// Set when `early_reader` was requested: a task blocked on the
    /// stream BEFORE the Offer moved, collecting every frame the
    /// receiver sends until the stream ends. This is the only
    /// deterministic way to observe a refusal frame — the receiver
    /// closes the connection right after sending it, so a reader that
    /// starts afterwards races the CONNECTION_CLOSE and loses.
    frames: Option<tokio::task::JoinHandle<Vec<Message>>>,
}

impl ConsentHarness {
    async fn send(&mut self, message: Message) {
        self.tx
            .as_mut()
            .expect("harness tx")
            .send_frame(&wire(message))
            .await
            .unwrap();
    }

    /// Next receiver frame, skipping the receiver's Hello (identity
    /// bearing, irrelevant to the frame comparisons here).
    async fn next_frame(&mut self) -> Message {
        let rx = self.rx.as_mut().expect("harness rx");
        let frame = rx.recv_frame(&engine_limits()).await.unwrap();
        match frame.message {
            Message::Hello { .. } => rx.recv_frame(&engine_limits()).await.unwrap().message,
            message => message,
        }
    }
}

/// Bind a receiver running `receive_on_channel_consent` with an
/// always-admitting decider, exchange Hello/TaskOpen/Admission/Ack and
/// send the Offer. `consent` is the hook under test; `early_reader`
/// additionally parks a frame reader on the stream before the Offer
/// (see [`ConsentHarness::frames`]).
async fn consent_harness(
    consent: Option<TransferConsent>,
    offered_files: u64,
    offered_bytes: u64,
    early_reader: bool,
) -> ConsentHarness {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
    spec.idle_timeout = Duration::from_secs(5);
    // Isolated checkpoint dir: the shared helper's millisecond-stamped
    // path collides between parallel tests, and the zero-disk-write
    // assertion below must see only this run's writes.
    let ckpt = tempfile::tempdir().unwrap();
    spec.checkpoint_dir = ckpt.path().to_path_buf();
    let checkpoint_dir = spec.checkpoint_dir.clone();
    let receiver = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
        receive_on_channel_consent(channel, spec, &decider, &noop, consent).await
    });
    let fake = fake_sender_side(addr, receiver_key.fingerprint(), CAP_SCHEDULER).await;
    let FakePeer {
        tx,
        mut rx,
        _channel,
        _endpoint,
    } = fake;
    let mut tx = tx;
    let task_id: [u8; 16] = [0x5C; 16];
    tx.send_frame(&wire(Message::TaskOpen {
        job_id: task_id,
        permit_id: [2; 16],
        group: [0x77; 32],
        group_registry: [0x91; 32],
        pairing_id: [0xB3; 16],
        direction: ak_wire::DIRECTION_SEND,
    }))
    .await
    .unwrap();
    // Admission (granted) — read through the admission-frame reader.
    match recv_admission_frame(&mut rx, &engine_limits(), Duration::from_secs(5))
        .await
        .unwrap()
        .message
    {
        Message::Admission { granted, .. } => {
            assert!(granted, "the admission decider always admits here")
        }
        other => panic!("expected Admission, got {other:?}"),
    }
    tx.send_frame(&wire(Message::AdmissionAck {
        job_id: task_id,
        permit_id: [2; 16],
    }))
    .await
    .unwrap();
    let job_id: JobId = [0xCD; 16];
    let (frames, rx) = if early_reader {
        (
            Some(tokio::spawn(async move {
                let mut seen = Vec::new();
                while let Ok(frame) = rx.recv_frame(&engine_limits()).await {
                    seen.push(frame.message);
                }
                seen
            })),
            None,
        )
    } else {
        (None, Some(rx))
    };
    tx.send_frame(&wire(Message::Offer {
        job_id,
        root_name: "consent-demo".into(),
        file_count: offered_files,
        total_bytes: offered_bytes,
        chunk_size: 4096,
    }))
    .await
    .unwrap();
    ConsentHarness {
        receiver,
        tx: Some(tx),
        rx,
        _channel,
        _endpoint,
        job_id,
        task_id,
        dst,
        checkpoint_dir,
        _ckpt_keepalive: ckpt,
        frames,
    }
}

/// A declining consent must: answer exactly one DECLINED error frame
/// (existing Error-frame semantics — the closed code set maps DECLINED
/// onto LocalPermissionDenied with the frozen message), never send an
/// Accept, fail locally with `ConsentDeclined`, close the connection,
/// and leave nothing on disk (no part file, no final file, no
/// checkpoint record).
#[tokio::test]
async fn consent_decline_sends_declined_error_and_writes_nothing() {
    let harness = consent_harness(
        Some(Arc::new(|_: &ConsentInfo| ConsentAnswer::Now(false))),
        2,
        4096,
        true,
    )
    .await;

    // The wire answer, observed by the early reader: exactly one
    // DECLINED error frame and then the connection ends.
    let frames = harness.frames.expect("early reader").await.unwrap();
    assert_eq!(
        frames,
        vec![Message::Error {
            code: DECLINED_WIRE_CODE,
            message: DECLINED_MESSAGE.to_string(),
        }],
        "the DECLINED frame and nothing else (no Accept)"
    );

    let err = harness
        .receiver
        .await
        .unwrap()
        .expect_err("the declining consent must fail the receive");
    match err {
        EngineError::ConsentDeclined { job_id } => assert_eq!(job_id, harness.job_id),
        other => panic!("expected ConsentDeclined, got {other:?}"),
    }

    // Zero disk writes: the destination holds no part file and no
    // final file (staged and legacy landing points both checked), and
    // the checkpoint store wrote no record.
    assert!(
        harness.dst.path().read_dir().unwrap().next().is_none(),
        "destination must stay empty"
    );
    assert!(
        !ak_storage::staging_path(harness.dst.path(), harness.task_id).exists(),
        "no per-task staging tree"
    );
    let records = harness
        .checkpoint_dir
        .read_dir()
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    assert_eq!(records, 0, "no checkpoint record may exist");
}

/// An accepting consent sees exactly the offered summary (job id, root
/// name, file count, total bytes) — the numbers the UI prompt shows.
#[tokio::test]
async fn consent_callback_receives_the_offered_summary() {
    let seen = Arc::new(std::sync::Mutex::new(None::<ConsentInfo>));
    let sink = Arc::clone(&seen);
    let mut harness = consent_harness(
        Some(Arc::new(move |info: &ConsentInfo| {
            *sink.lock().unwrap() = Some(info.clone());
            ConsentAnswer::Now(true)
        })),
        2,
        12_345,
        false,
    )
    .await;
    // The consent accepted: the next frame is the plain Accept.
    match harness.next_frame().await {
        Message::Accept { job_id } => assert_eq!(job_id, harness.job_id),
        other => panic!("an accepting consent must Accept, got {other:?}"),
    }
    let info = seen.lock().unwrap().clone().expect("callback ran");
    assert_eq!(info.job_id, harness.job_id);
    assert_eq!(info.root_name, "consent-demo");
    assert_eq!(info.file_count, 2);
    assert_eq!(info.total_bytes, 12_345);
    // The receiver now waits for the manifest; hang the fake up and
    // collect the failure — anything but ConsentDeclined.
    harness.tx = None;
    drop(harness.rx.take());
    let outcome = harness.receiver.await.unwrap();
    assert!(
        !matches!(outcome, Err(EngineError::ConsentDeclined { .. })),
        "accepting consent must not decline: {outcome:?}"
    );
}

/// The non-blocking shape (wave 29, review P2-07): a
/// `ConsentAnswer::Pending` parks the receive at the Offer point while
/// the callback itself already returned — the decision arrives through
/// the oneshot — and a decider that vanishes (every sender half
/// dropped) fails closed as a declination.
#[tokio::test]
async fn consent_pending_resolves_asynchronously_and_fails_closed() {
    // Resolved-accept leg: the receiver stays parked at the Offer until
    // the out-of-band decision arrives.
    let (decide_tx, decide_rx) = tokio::sync::oneshot::channel::<bool>();
    let parked = std::sync::Mutex::new(Some(decide_rx));
    let mut harness = consent_harness(
        Some(Arc::new(move |_: &ConsentInfo| {
            match parked.lock().unwrap().take() {
                Some(pending) => ConsentAnswer::Pending(pending),
                None => ConsentAnswer::Now(false),
            }
        })),
        1,
        10,
        false,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    decide_tx.send(true).unwrap();
    match harness.next_frame().await {
        Message::Accept { job_id } => assert_eq!(job_id, harness.job_id),
        other => panic!("a resolved-accept pending consent must Accept, got {other:?}"),
    }

    // Fail-closed leg: every decider drops without answering.
    let (gone_tx, gone_rx) = tokio::sync::oneshot::channel::<bool>();
    drop(gone_tx);
    let vanished = std::sync::Mutex::new(Some(gone_rx));
    let harness = consent_harness(
        Some(Arc::new(move |_: &ConsentInfo| {
            match vanished.lock().unwrap().take() {
                Some(pending) => ConsentAnswer::Pending(pending),
                None => ConsentAnswer::Now(false),
            }
        })),
        1,
        10,
        false,
    )
    .await;
    let outcome = harness.receiver.await.unwrap();
    assert!(
        matches!(outcome, Err(EngineError::ConsentDeclined { .. })),
        "a vanished decider must fail closed as a declination: {outcome:?}"
    );
}

/// `Some(always-true)` must be byte-for-byte the `None` path: the
/// receiver's wire frames (after the identity-bearing Hello) and the
/// final report of one complete transfer are identical between the
/// frozen entry and the consented entry.
#[tokio::test]
async fn consent_accept_path_is_byte_identical_to_the_default() {
    const CHUNK: usize = 4096;
    let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
    let digest = |bytes: &[u8]| {
        let mut out = [0u8; 32];
        out.copy_from_slice(blake3::hash(bytes).as_bytes());
        out
    };
    let (a, b) = payload.split_at(CHUNK);
    let entry = ak_core::manifest::FileEntry::with_digest_alg(
        "consent-identical.bin",
        payload.len() as u64,
        1_700_000_000_000,
        ak_core::manifest::DIGEST_ALG_BLAKE3,
        digest(&payload),
        vec![digest(a), digest(b)],
    )
    .unwrap();

    /// One complete hand-written scheduled transfer; collects every
    /// receiver frame after its Hello plus the final report.
    async fn one_run(
        consent: Option<TransferConsent>,
        payload: &[u8],
        entry: &ak_core::manifest::FileEntry,
    ) -> (Vec<Message>, Report) {
        let mut harness = consent_harness(consent, 1, payload.len() as u64, false).await;
        let mut frames = Vec::new();
        frames.push(harness.next_frame().await); // Accept
        harness
            .send(Message::ManifestPage {
                job_id: harness.job_id,
                page_index: 0,
                last_page: true,
                entries: vec![WireEntry::from(entry)],
            })
            .await;
        frames.push(harness.next_frame().await); // ResumeRequest
        for (index, data) in [(0u64, &payload[..CHUNK]), (1u64, &payload[CHUNK..])] {
            harness
                .send(Message::Chunk {
                    job_id: harness.job_id,
                    file_index: 0,
                    chunk_index: index,
                    data: data.to_vec(),
                })
                .await;
            frames.push(harness.next_frame().await); // ChunkAck
        }
        harness
            .send(Message::Finalize {
                job_id: harness.job_id,
            })
            .await;
        frames.push(harness.next_frame().await); // FinalAck
        let report = harness.receiver.await.unwrap().expect("run completes");
        (frames, report)
    }

    let (plain_frames, plain_report) = one_run(None, &payload, &entry).await;
    let consent = Some(Arc::new(|_: &ConsentInfo| ConsentAnswer::Now(true)) as TransferConsent);
    let (consent_frames, consent_report) = one_run(consent, &payload, &entry).await;

    // Every protocol frame after the Hello is identical.
    assert_eq!(
        plain_frames, consent_frames,
        "an accepting consent changes nothing on the wire"
    );
    assert_eq!(
        plain_frames.len(),
        5,
        "Accept, ResumeRequest, 2x ChunkAck, FinalAck"
    );
    // The reports agree on everything deterministic (duration and the
    // peer fingerprint are per-run by construction).
    assert_eq!(plain_report.job_id, consent_report.job_id);
    assert_eq!(plain_report.files, consent_report.files);
    assert_eq!(plain_report.bytes, consent_report.bytes);
    assert_eq!(plain_report.resumed_chunks, consent_report.resumed_chunks);
    assert_eq!(plain_report.roots, consent_report.roots);
}

// ------------------------------------------------------------------
// Wave 27: daemon scheduler handshake + pre-admitted receive
// ------------------------------------------------------------------

/// Spawn the daemon head for the wave-27 tests: accept one connection
/// and run `channel_scheduler_handshake`, yielding the (still
/// caller-owned) channel next to the handshake result.
fn spawn_daemon_handshake(
    endpoint: DirectEndpoint,
    spec: ReceiveSpec,
) -> tokio::task::JoinHandle<Result<(SchedulerHandshake, DirectChannel), EngineError>> {
    tokio::spawn(async move {
        let mut channel = endpoint.accept(&|_| true).await.unwrap();
        let handshake = channel_scheduler_handshake(&mut channel, &spec).await?;
        Ok((handshake, channel))
    })
}

/// A CAP_SCHEDULER peer's TaskOpen comes back through
/// `SchedulerHandshake::Scheduler` with every admission field byte
/// for byte what the peer put on the wire (wave 27: the daemon gets
/// the raw fields to bind the code group BEFORE admission answers).
#[tokio::test]
async fn scheduler_handshake_reads_a_scheduler_peers_task_open_byte_exact() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);

    let daemon = spawn_daemon_handshake(endpoint, spec);
    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    let task_id: [u8; 16] = [0x42; 16];
    let permit: [u8; 16] = [0x5C; 16];
    let group: [u8; 32] = [0x77; 32];
    let registry: [u8; 32] = [0x91; 32];
    let pairing: [u8; 16] = [0xB3; 16];
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: permit,
            group,
            group_registry: registry,
            pairing_id: pairing,
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();

    let (handshake, _channel) = daemon.await.unwrap().expect("handshake completes");
    match handshake {
        SchedulerHandshake::Scheduler { open } => {
            assert_eq!(open.job_id(), task_id, "job_id byte exact");
            assert_eq!(open.permit_id(), permit, "permit_id byte exact");
            assert_eq!(open.group(), group, "group byte exact");
            assert_eq!(open.group_registry(), registry, "group_registry byte exact");
            assert_eq!(open.pairing_id(), pairing, "pairing_id byte exact");
            assert_eq!(open.direction(), ak_wire::DIRECTION_SEND);
            // The convenience view carries the same pairing fields.
            assert_eq!(
                open.admission_request(),
                AdmissionRequest {
                    group,
                    group_registry: registry,
                    pairing_id: pairing,
                }
            );
        }
        SchedulerHandshake::Legacy { .. } => {
            panic!("a CAP_SCHEDULER peer must take the Scheduler arm")
        }
    }
    // The handshake did NOT answer admission: nothing of the
    // Admission/Ack exchange may have moved yet.
    if let Ok(Ok(frame)) = tokio::time::timeout(
        Duration::from_secs(1),
        recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_millis(200)),
    )
    .await
    {
        panic!("handshake must stay silent, saw {:?}", frame.message);
    }
}

/// A peer whose Hello lacks CAP_SCHEDULER takes the Legacy arm with
/// the peer's capability bits — and no TaskOpen is ever read or
/// expected from it.
#[tokio::test]
async fn scheduler_handshake_returns_legacy_for_a_legacy_peer() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);

    let daemon = spawn_daemon_handshake(endpoint, spec);
    // Legacy sender: no CAP_SCHEDULER bit, no TaskOpen — it would
    // proceed straight to its Offer.
    let mut fake = fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME).await;

    let (handshake, _channel) = daemon.await.unwrap().expect("handshake completes");
    match handshake {
        SchedulerHandshake::Legacy {
            peer_capabilities,
            stream,
        } => {
            assert_eq!(peer_capabilities, CAP_RESUME);
            // The Hello-exchanged stream rides along (wave 27
            // integrator fix): dropping it here is exactly what the
            // daemon must NOT do — receive_after_hello consumes it.
            drop(stream);
        }
        SchedulerHandshake::Scheduler { .. } => {
            panic!("a legacy peer must take the Legacy arm")
        }
    }
    // Nothing of the admission exchange may have been answered and no
    // file protocol ran on this channel from the engine side.
    if let Ok(Ok(frame)) = tokio::time::timeout(
        Duration::from_secs(1),
        recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_millis(200)),
    )
    .await
    {
        panic!("legacy arm must stay silent, saw {:?}", frame.message);
    }
}

/// The handshake re-runs the receive path's authorize check: a
/// channel whose fingerprint the callback refuses fails with the
/// accept path's `ConnectError::Unauthorized` semantics.
#[tokio::test]
async fn scheduler_handshake_refuses_an_unauthorized_peer() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
    spec.authorize = Box::new(|_| false);

    let daemon = spawn_daemon_handshake(endpoint, spec);
    // The peer dials and connects; the refusal happens on the entry's
    // identity re-check — before any stream or Hello, so the dial
    // itself succeeds and no Hello exchange is attempted (the
    // connection is closed under the peer right after).
    let sender_key = DeviceKeyPair::generate();
    let sender_identity = ak_identity::tls::make_self_signed(&sender_key);
    let dialer = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_identity)
        .await
        .unwrap();
    let _channel = dialer
        .connect(addr, &receiver_key.fingerprint())
        .await
        .unwrap();

    let err = match daemon.await.unwrap() {
        // DirectChannel is not Debug, so no expect_err here.
        Err(error) => error,
        Ok(_) => panic!("the refused fingerprint must fail the handshake"),
    };
    assert!(
        matches!(
            err,
            EngineError::Connect(ak_net::ConnectError::Unauthorized)
        ),
        "got: {err:?}"
    );
}

/// Full daemon round trip (wave 27): accept → handshake → upper-layer
/// group binding → pre-admitted receive, against a real
/// `send_on_channel` sender. An 8 KiB file moves, the granted
/// Admission + AdmissionAck exchange completes, the bytes land
/// identical and the per-task staging tree is discarded at the
/// terminal state.
#[tokio::test]
async fn preadmitted_round_trip_completes_an_8kib_transfer() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 241) as u8).collect();
    write(&src.path().join("round.bin"), &payload);

    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let pinned = receiver_key.fingerprint();
    let recv_spec = on_channel_recv_spec(dst.path(), receiver_identity);
    let seen: Arc<std::sync::Mutex<Vec<Progress>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = Arc::clone(&seen);

    let receiver = tokio::spawn(async move {
        let mut channel = endpoint.accept(&|_| true).await.unwrap();
        let open = match channel_scheduler_handshake(&mut channel, &recv_spec)
            .await
            .unwrap()
        {
            SchedulerHandshake::Scheduler { open } => open,
            SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
        };
        // Upper-layer analog (ak-session): bind the retained fields
        // to the code group before admitting.
        assert_eq!(
            open.admission_request(),
            AdmissionRequest {
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
            }
        );
        let progress = move |event: Progress| events.lock().unwrap().push(event);
        receive_on_channel_preadmitted(channel, recv_spec, open, &progress).await
    });

    let task_id: [u8; 16] = [0xD1; 16];
    let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, pinned, task_id);

    let recv_report = receiver
        .await
        .unwrap()
        .expect("preadmitted receive completes");
    let send_report = sender.await.unwrap().expect("send completes");
    assert_eq!(send_report.files, 1);
    assert_eq!(send_report.bytes, payload.len() as u64);
    assert_eq!(recv_report.bytes, payload.len() as u64);
    assert_eq!(fs::read(dst.path().join("round.bin")).unwrap(), payload);
    assert!(
        !ak_storage::staging_path(dst.path(), task_id).exists(),
        "terminal state discards the task's staging tree"
    );
    // The transfer entry reported the connection phase (Connected
    // names the peer the channel verified — the sender device).
    let events = seen.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Progress::Connected { .. })),
        "the pre-admitted entry reports Connected: {events:?}"
    );
    assert!(events.contains(&Progress::Done));
}

/// Per-task staging isolation across the daemon path: a task whose
/// receive fails mid-transfer keeps its verified part in ITS staging
/// tree (keyed by its own job_id), and a second task admitted right
/// after — through the same handshake/preadmitted pair — completes,
/// cleans up only its own tree and leaves the first task's staged
/// prefix untouched.
#[tokio::test]
async fn preadmitted_stages_isolate_by_job_id_and_cleanup_is_per_task() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let pinned = receiver_key.fingerprint();

    const CHUNK: usize = 4096;
    let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
    let (a, b) = payload.split_at(CHUNK);
    let digest = |bytes: &[u8]| {
        let mut out = [0u8; 32];
        out.copy_from_slice(blake3::hash(bytes).as_bytes());
        out
    };
    let whole = digest(&payload);
    let chunk_digests = vec![digest(a), digest(b)];
    let limits = engine_limits();

    // ---- task A (hand-driven): admitted through the daemon pair,
    // first chunk verifies, second digest is poisoned → the receive
    // fails with the verified prefix left in A's staging tree.
    let task_a: [u8; 16] = [0xC1; 16];
    let mut spec_a = on_channel_recv_spec(dst.path(), receiver_identity.clone());
    spec_a.idle_timeout = Duration::from_secs(5);
    let receiver_a = tokio::spawn(async move {
        let mut channel = endpoint.accept(&|_| true).await.expect("task A connects");
        let open = match channel_scheduler_handshake(&mut channel, &spec_a)
            .await
            .unwrap()
        {
            SchedulerHandshake::Scheduler { open } => open,
            SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
        };
        receive_on_channel_preadmitted(channel, spec_a, open, &noop).await
    });
    let mut fake_a = fake_sender_side(addr, pinned, CAP_RESUME | CAP_SCHEDULER).await;
    fake_a
        .tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_a,
            permit_id: [2; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
    // The pre-admitted entry answers the granted admission (bounded
    // lease) and waits for the ack — the daemon flow, split in two.
    let (granted_job, granted_permit, granted, reason, lease_ms) =
        read_admission(&mut fake_a).await;
    assert_eq!(granted_job, task_a);
    assert_eq!(granted_permit, [2; 16]);
    assert!(granted);
    assert_eq!(reason, AdmissionReason::Ok);
    assert_eq!(
        lease_ms, ADMISSION_LEASE_MS,
        "grant carries the bounded lease"
    );
    fake_a
        .tx
        .send_frame(&wire(Message::AdmissionAck {
            job_id: task_a,
            permit_id: [2; 16],
        }))
        .await
        .unwrap();

    // Frozen receive body, hand-written: poisoned chunk-1 digest.
    let wire_job: JobId = [0xCE; 16];
    fake_a
        .tx
        .send_frame(&wire(Message::Offer {
            job_id: wire_job,
            root_name: "iso-a".into(),
            file_count: 1,
            total_bytes: payload.len() as u64,
            chunk_size: CHUNK as u32,
        }))
        .await
        .unwrap();
    match fake_a.rx.recv_frame(&limits).await.unwrap().message {
        Message::Accept { job_id: acked } => assert_eq!(acked, wire_job),
        other => panic!("expected Accept, got {other:?}"),
    }
    let entry = ak_core::manifest::FileEntry::with_digest_alg(
        "iso-a.bin",
        payload.len() as u64,
        1_700_000_000_000,
        ak_core::manifest::DIGEST_ALG_BLAKE3,
        whole,
        chunk_digests,
    )
    .unwrap();
    let mut wire_entry = WireEntry::from(&entry);
    wire_entry.chunk_digests[1] = [0xEE; 32]; // poisoned
    fake_a
        .tx
        .send_frame(&wire(Message::ManifestPage {
            job_id: wire_job,
            page_index: 0,
            last_page: true,
            entries: vec![wire_entry],
        }))
        .await
        .unwrap();
    match fake_a.rx.recv_frame(&limits).await.unwrap().message {
        Message::ResumeRequest { files, more, .. } => {
            assert!(!more);
            assert_eq!(files[0].verified_prefix_chunks, 0, "fresh task");
        }
        other => panic!("expected ResumeRequest, got {other:?}"),
    }
    for (index, data) in [(0u64, a), (1u64, b)] {
        fake_a
            .tx
            .send_frame(&wire(Message::Chunk {
                job_id: wire_job,
                file_index: 0,
                chunk_index: index,
                data: data.to_vec(),
            }))
            .await
            .unwrap();
    }
    let err_a = receiver_a
        .await
        .unwrap()
        .expect_err("task A's poisoned digest must fail the receive");
    assert!(
        !matches!(err_a, EngineError::AdmissionDenied { .. }),
        "admission itself succeeded: {err_a:?}"
    );
    // Task A's verified prefix lives in ITS staging tree.
    let staged_a = ak_storage::staging_path(dst.path(), task_a).join("iso-a.bin.akpart");
    assert!(staged_a.exists(), "A's part must live in A's staging tree");
    assert_eq!(
        fs::metadata(&staged_a).unwrap().len(),
        CHUNK as u64,
        "exactly A's verified prefix survived"
    );

    // ---- task B (real sender): completes through the same daemon
    // pair into the same destination directory. Task A's endpoint
    // moved into its spawn, so B listens on a fresh endpoint bound
    // to the same identity (same trick as the staged-resume test's
    // run 2 — the destination dir is what carries the staged state).
    let src_b = tempfile::tempdir().unwrap();
    let payload_b: Vec<u8> = (0..5000u32).map(|i| (i % 199) as u8).collect();
    write(&src_b.path().join("iso-b.bin"), &payload_b);
    let task_b: [u8; 16] = [0xC2; 16];
    let endpoint_b =
        DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
            .await
            .unwrap();
    let addr_b = endpoint_b.local_addr().unwrap();
    let sender_b = spawn_on_channel_sender(src_b.path().to_path_buf(), addr_b, pinned, task_b);
    let spec_b = on_channel_recv_spec(dst.path(), receiver_identity);
    let receiver_b = tokio::spawn(async move {
        let mut channel = endpoint_b.accept(&|_| true).await.expect("task B connects");
        let open = match channel_scheduler_handshake(&mut channel, &spec_b)
            .await
            .unwrap()
        {
            SchedulerHandshake::Scheduler { open } => open,
            SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
        };
        assert_eq!(open.job_id(), task_b, "B's TaskOpen names B's task");
        receive_on_channel_preadmitted(channel, spec_b, open, &noop).await
    });

    let report_b = receiver_b.await.unwrap().expect("task B completes");
    sender_b.await.unwrap().expect("task B's send completes");
    assert_eq!(report_b.bytes, payload_b.len() as u64);
    assert_eq!(fs::read(dst.path().join("iso-b.bin")).unwrap(), payload_b);
    // B's terminal state discarded only B's tree...
    assert!(
        !ak_storage::staging_path(dst.path(), task_b).exists(),
        "B's staging tree is discarded at completion"
    );
    // ...while A's staged prefix survives untouched (per-task
    // isolation: B never swept, resumed or collided with A's parts).
    assert!(
        staged_a.exists(),
        "A's staged prefix must survive B's terminal cleanup"
    );
    assert_eq!(fs::metadata(&staged_a).unwrap().len(), CHUNK as u64);
}

/// The pre-admitted entry refuses a retained TaskOpen whose direction
/// tag says the peer is receiving (it must be the sender): the same
/// PROTOCOL_UNSUPPORTED violation the combined path produces, no
/// Admission is ever answered and nothing lands.
#[tokio::test]
async fn preadmitted_refuses_a_recv_direction_task_open() {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
        .await
        .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let spec = on_channel_recv_spec(dst.path(), receiver_identity);

    let receiver = tokio::spawn(async move {
        let mut channel = endpoint.accept(&|_| true).await.unwrap();
        let open = match channel_scheduler_handshake(&mut channel, &spec)
            .await
            .unwrap()
        {
            SchedulerHandshake::Scheduler { open } => open,
            SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
        };
        receive_on_channel_preadmitted(channel, spec, open, &noop).await
    });

    let mut fake =
        fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
    let task_id: [u8; 16] = [0xEE; 16];
    fake.tx
        .send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [7; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_RECV,
        }))
        .await
        .unwrap();

    let err = receiver
        .await
        .unwrap()
        .expect_err("a recv-direction TaskOpen must be refused");
    assert!(
        matches!(
            &err,
            EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                ..
            }
        ),
        "got: {err:?}"
    );
    assert!(
        !ak_storage::staging_path(dst.path(), task_id).exists(),
        "no staging may appear for a refused direction"
    );
    assert_eq!(
        fs::read_dir(dst.path()).unwrap().count(),
        0,
        "no file may land from a refused task"
    );
    // No Admission was answered: the fake sees the closed connection,
    // never a granted frame.
    if let Ok(Ok(frame)) = tokio::time::timeout(
        Duration::from_secs(2),
        recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(1)),
    )
    .await
    {
        panic!("no Admission may be answered, saw {:?}", frame.message);
    }
}

// ---------------------------------------------------------------------------
// Batch S review (BACKLOG P3-02): the sender's liveness signal during its
// final source check (CAP_SENDER_KEEPALIVE), and the receiver's report of
// a disk that fills up mid-transfer.
// ---------------------------------------------------------------------------

/// The root name a send of `src` offers (its directory name); it keys the
/// test seams, so parallel tests never share an entry.
fn root_name_of(src: &Path) -> String {
    src.file_name().unwrap().to_string_lossy().into_owned()
}

/// Engine to engine, with a final source check three idle budgets long.
/// Before the signal the receiver, waiting for Finalize under its
/// per-frame idle budget, gave up with `IdleTimeout` while the sender was
/// still checking — on a real disk from about a quarter of a million
/// files on. Multi-thread runtime: the receiver's timer runs while the
/// check sleeps on the sender's blocking pool.
async fn slow_source_check_round(multistream: bool) {
    let idle = Duration::from_millis(800);
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for i in 0..6u8 {
        write(&src.path().join(format!("s{i}.bin")), &[i; 5000]);
    }
    seams::slow_source_check(&root_name_of(src.path()), idle * 3);
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 1);
    send_spec.idle_timeout = idle;
    send_spec.multistream = multistream;
    recv.idle_timeout = idle;
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let started = Instant::now();
    let sender = tokio::spawn(async move { send(send_spec, &noop).await });
    let recv_outcome = receiver.await.unwrap();
    let send_outcome = sender.await.unwrap();
    recv_outcome.expect("the receiver must wait out the sender's source check");
    let report = send_outcome.expect("the send completes");
    assert_eq!(report.files, 6);
    assert!(
        started.elapsed() >= idle * 3,
        "the check must really outlast the idle budget: {:?}",
        started.elapsed()
    );
    for i in 0..6u8 {
        assert_eq!(
            fs::read(dst.path().join(format!("s{i}.bin"))).unwrap(),
            vec![i; 5000]
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_source_check_keeps_the_receiver_waiting() {
    slow_source_check_round(false).await;
}

/// Multistream: the signal arrives on the control stream after every data
/// stream finished, and must refresh the driver's idle budget too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_source_check_keeps_a_multistream_receiver_waiting() {
    slow_source_check_round(true).await;
}

/// The engine sender against a fake receiver advertising `fake_caps`, with
/// a 900 ms source check at a 100 ms liveness cadence (sender idle budget
/// 400 ms). Returns how many signals the fake saw before Finalize; any
/// other frame is a failure, and no signal may come before the last chunk.
async fn signals_before_finalize(fake_caps: u16) -> u64 {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("k.bin"), &[7u8; 3000]);
    seams::slow_source_check(&root_name_of(src.path()), Duration::from_millis(900));
    let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
    send_spec.idle_timeout = Duration::from_millis(400);
    let bundle = recv_spec.identity;
    let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
        .await
        .unwrap();
    send_spec.peer = endpoint.local_addr().unwrap();
    send_spec.pinned = receiver_fp;

    let fake = tokio::spawn(async move {
        let channel = endpoint.accept(&|_| true).await.unwrap();
        let mut stream = channel.accept_bi().await.unwrap();
        let job_id = fake_receiver_preamble(&mut stream, &bundle, fake_caps).await;
        let limits = engine_limits();
        let mut signals = 0u64;
        loop {
            match stream.recv_frame(&limits).await.unwrap().message {
                Message::Chunk {
                    file_index,
                    chunk_index,
                    ..
                } => {
                    assert_eq!(signals, 0, "the signal only covers the source check");
                    stream
                        .send_frame(&wire(Message::ChunkAck {
                            job_id,
                            file_index,
                            chunk_index,
                        }))
                        .await
                        .unwrap();
                }
                Message::Control {
                    job_id: signal_job,
                    control: Control::Resume,
                } => {
                    assert_eq!(signal_job, job_id, "the signal names the job");
                    signals += 1;
                }
                Message::Finalize { .. } => {
                    stream
                        .send_frame(&wire(Message::FinalAck {
                            job_id,
                            verified: true,
                        }))
                        .await
                        .unwrap();
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        while stream.recv_frame(&limits).await.is_ok() {}
                    })
                    .await;
                    return signals;
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    });
    let report = send(send_spec, &noop).await.expect("the send completes");
    assert_eq!(report.files, 1);
    fake.await.unwrap()
}

/// Only a receiver whose Hello carried CAP_SENDER_KEEPALIVE gets the
/// signal: an older receiver would refuse the frame, so it sees the
/// frozen silence instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_check_signals_only_a_receiver_that_negotiated_it() {
    let signalled =
        signals_before_finalize(CAP_RESUME | CAP_PAUSE_RESUME | CAP_SENDER_KEEPALIVE).await;
    assert!(
        signalled >= 3,
        "a 900 ms check at a 100 ms cadence signals several times: {signalled}"
    );
    let silent = signals_before_finalize(CAP_RESUME | CAP_PAUSE_RESUME).await;
    assert_eq!(
        silent, 0,
        "a receiver without the bit never sees the signal"
    );
}

/// The engine receiver against a fake sender whose Hello carries
/// `capabilities` and that sends one no-op Control::Resume naming
/// `signal_job` right after the resume answer, then its chunks and
/// Finalize. Returns the receiver's outcome, its destination directory
/// and the payload the fake sent.
async fn receive_with_sender_signal(
    capabilities: u16,
    signal_job: Option<JobId>,
) -> (Result<Report, EngineError>, tempfile::TempDir, Vec<u8>) {
    let dst = tempfile::tempdir().unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let mut recv = on_channel_recv_spec(dst.path(), receiver_bundle);
    recv.idle_timeout = Duration::from_secs(5);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    let addr = addr_rx.await.unwrap();
    let mut fake = fake_sender_side(addr, receiver_key.fingerprint(), capabilities).await;
    let limits = engine_limits();
    let job_id: JobId = [0x5A; 16];
    let payload: Vec<u8> = (0..3000u32).map(|i| (i % 249) as u8).collect();
    let entry = chunked_wire_entry("kept.bin", &payload, 1024);
    fake.tx
        .send_frame(&wire(Message::Offer {
            job_id,
            root_name: "sender-signal".to_owned(),
            file_count: 1,
            total_bytes: entry.size,
            chunk_size: 1024,
        }))
        .await
        .unwrap();
    assert!(matches!(
        fake.rx.recv_frame(&limits).await.unwrap().message,
        Message::Accept { .. }
    ));
    fake.tx
        .send_frame(&wire(Message::ManifestPage {
            job_id,
            page_index: 0,
            last_page: true,
            entries: vec![entry],
        }))
        .await
        .unwrap();
    loop {
        match fake.rx.recv_frame(&limits).await.unwrap().message {
            Message::ResumeRequest { more: false, .. } => break,
            Message::ResumeRequest { .. } => {}
            other => panic!("expected ResumeRequest, got {other:?}"),
        }
    }
    fake.tx
        .send_frame(&wire(Message::Control {
            job_id: signal_job.unwrap_or(job_id),
            control: Control::Resume,
        }))
        .await
        .unwrap();
    // A receiver that refused the signal is gone by now; the rest of the
    // script simply stops at the first failed read or write.
    let mut delivered = true;
    for (chunk_index, data) in payload.chunks(1024).enumerate() {
        let sent = fake
            .tx
            .send_frame(&wire(Message::Chunk {
                job_id,
                file_index: 0,
                chunk_index: chunk_index as u64,
                data: data.to_vec(),
            }))
            .await;
        let acked = match sent {
            Ok(()) => matches!(
                fake.rx.recv_frame(&limits).await.map(|frame| frame.message),
                Ok(Message::ChunkAck { .. })
            ),
            Err(_) => false,
        };
        if !acked {
            delivered = false;
            break;
        }
    }
    if delivered {
        fake.tx
            .send_frame(&wire(Message::Finalize { job_id }))
            .await
            .unwrap();
        assert!(matches!(
            fake.rx.recv_frame(&limits).await.unwrap().message,
            Message::FinalAck { verified: true, .. }
        ));
        let _ = fake.tx.finish();
    }
    (receiver.await.unwrap(), dst, payload)
}

/// The receiver side of the negotiation: the signal is accepted and
/// ignored from a sender that announced CAP_SENDER_KEEPALIVE, refused as
/// an unexpected frame from one that did not (the strictness every older
/// receiver has), and refused when it names another job.
#[tokio::test]
async fn receiver_takes_the_sender_signal_only_when_negotiated() {
    let (outcome, dst, payload) =
        receive_with_sender_signal(CAP_RESUME | CAP_SENDER_KEEPALIVE, None).await;
    let report = outcome.expect("a negotiated signal is a no-op");
    assert_eq!(report.files, 1);
    assert_eq!(fs::read(dst.path().join("kept.bin")).unwrap(), payload);

    let (outcome, _dst, _) = receive_with_sender_signal(CAP_RESUME, None).await;
    match outcome.expect_err("an unnegotiated signal stays unexpected") {
        EngineError::PeerError {
            code: ErrorCode::ProtocolUnsupported,
            message,
        } => assert!(message.contains("unexpected frame"), "{message}"),
        other => panic!("got {other:?}"),
    }

    let (outcome, _dst, _) =
        receive_with_sender_signal(CAP_RESUME | CAP_SENDER_KEEPALIVE, Some([0x11; 16])).await;
    match outcome.expect_err("a signal naming another job is a protocol error") {
        EngineError::PeerError {
            code: ErrorCode::ProtocolUnsupported,
            message,
        } => assert!(message.contains("different job"), "{message}"),
        other => panic!("got {other:?}"),
    }
}

/// Engine to engine, the destination fills up after five chunk writes
/// (BACKLOG P3-02). The receiver tells the sender before it lets go of the
/// connection, so the sender reports the peer's StorageFull rather than a
/// lost connection, and the receiver's own verdict is its io error. The
/// hold ends as soon as the sender closes, well inside its bound.
async fn disk_full_round(multistream: bool) {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 253) as u8).collect();
    write(&src.path().join("big.bin"), &payload);
    seams::fill_disk_after(&root_name_of(src.path()), 5);
    let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 1); // 40 chunks
    send_spec.multistream = multistream;
    // Bounds: single-stream hold min(10 s, idle), multistream the same.
    send_spec.idle_timeout = Duration::from_secs(10);
    recv.idle_timeout = Duration::from_secs(10);
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
    send_spec.peer = addr_rx.await.unwrap();
    let started = Instant::now();
    let sender = tokio::spawn(async move { send(send_spec, &noop).await });
    let recv_error = receiver
        .await
        .unwrap()
        .expect_err("the receive fails on the full disk");
    let send_error = sender.await.unwrap().expect_err("so does the send");
    match &send_error {
        EngineError::PeerError {
            code: ErrorCode::StorageFull,
            message,
        } => assert_eq!(message, STORAGE_FULL_MESSAGE),
        other => panic!("the sender must name the peer's full disk, got {other:?}"),
    }
    assert!(
        matches!(&recv_error, EngineError::Io(e) if e.kind() == std::io::ErrorKind::StorageFull),
        "the receiver keeps its local verdict, got {recv_error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the hold must end with the sender's close: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_full_mid_transfer_reaches_the_sender() {
    disk_full_round(false).await;
}

/// Multistream: the frame goes on the failing file's own stream, which the
/// sender's task for that file reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_full_mid_transfer_reaches_a_multistream_sender() {
    disk_full_round(true).await;
}
