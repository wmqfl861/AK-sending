//! TEST_PLAN T09（形态映射）：接收目录撤权 / 存储满 —— 不误报完成，可恢复。
//!
//! 层级声明：L2 环回形态。Windows 上 chmod 只读不可靠（对目录基本无效且
//! 需要特权），故“撤权”用本机可构造的等价形态：out_dir 指向一个**文件**
//! 路径 —— 接收侧必须在开目标目录时拿到 OpenError（NotDirectory），以
//! 错误返回而非 panic、更不误报完成；第二个场景验证 conflict=rename 下
//! 重名二次传输得到 `name (2).ext`，且新旧两份内容各自完整（可恢复、不
//! 静默覆盖）。
#![forbid(unsafe_code)]

mod common;

use common::{assert_no_part_files, run_transfer, run_transfer_conflict, write_file};
use std::fs;
use std::num::NonZeroU32;

use ak_core::handle::ConflictPolicy;
use ak_engine::{receive, EngineError, ReceiveSpec};
use ak_identity::DeviceKeyPair;

/// out_dir 指向一个已存在的文件路径：`engine::receive` 必须返回
/// `EngineError::Storage(OpenError::NotDirectory)`（错误码化失败，不是
/// panic，也没有任何误报的 Report）。T09 的“不误报完成”下界。
#[tokio::test]
async fn out_dir_pointing_at_a_file_fails_with_storage_error_not_panic() {
    let base = tempfile::tempdir().unwrap();
    let not_a_dir = base.path().join("occupied.txt");
    fs::write(&not_a_dir, b"I am a file, not a directory").unwrap();

    let receiver_key = DeviceKeyPair::generate();
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let mut spec = ReceiveSpec::new(not_a_dir.clone(), receiver_bundle);
    spec.bind = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    spec.chunk_size = NonZeroU32::new(1024).unwrap();
    spec.checkpoint_dir = base.path().join("ckpt");

    let result = receive(spec, &|_| {}).await;
    match result {
        Err(EngineError::Storage(error)) => {
            assert!(
                matches!(error, ak_storage::OpenError::NotDirectory(_)),
                "expected NotDirectory, got {error:?}"
            );
        }
        other => panic!("expected EngineError::Storage(NotDirectory), got {other:?}"),
    }
    // The obstructing file is untouched; no stray part files next to it.
    assert_eq!(
        fs::read(&not_a_dir).unwrap(),
        b"I am a file, not a directory"
    );
}

/// conflict=rename 语义：同一源完整传输两次到同一目标 —— 第二次落成
/// `a (2).txt` / `nested/b (2).bin`，第一轮文件原样保留，两份内容都与源
/// 逐字节一致（各自完整，无静默覆盖）。
#[tokio::test]
async fn conflict_rename_second_transfer_lands_as_two_with_both_copies_intact() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t09-src");
    write_file(&src.join("a.txt"), b"first transfer content - complete");
    write_file(
        &src.join("nested/b.bin"),
        &(0..600u32).map(|i| i as u8).collect::<Vec<_>>(),
    );

    let dst = tempfile::tempdir().unwrap();

    // First transfer: clean landing under ConflictPolicy::Fail.
    let first = run_transfer(&src, dst.path(), 1).await;
    assert_eq!(
        common::read_file(dst.path(), "a.txt"),
        b"first transfer content - complete"
    );
    assert_eq!(
        common::read_file(dst.path(), "nested/b.bin"),
        (0..600u32).map(|i| i as u8).collect::<Vec<_>>()
    );
    assert_no_part_files(dst.path());

    // Second transfer of the same source under Rename: since wave 17 the
    // committed-final fast path (F3/F2) recognizes byte-identical finals as
    // the job's own committed output, so this is an idempotent full resume —
    // no re-transfer, no "(2)" copies of unchanged content.
    let second = run_transfer_conflict(&src, dst.path(), 1, ConflictPolicy::Rename).await;
    assert_eq!(
        second.send.job_id, first.send.job_id,
        "identical source must keep the same content identity"
    );
    assert_eq!(
        second.send.resumed_chunks, 2,
        "both files fully resumed via committed finals"
    );
    assert_eq!(second.recv.resumed_chunks, 2);
    assert_eq!(
        common::read_file(dst.path(), "a.txt"),
        b"first transfer content - complete"
    );
    let bin = (0..600u32).map(|i| i as u8).collect::<Vec<_>>();
    assert_eq!(common::read_file(dst.path(), "nested/b.bin"), bin);
    assert_no_part_files(dst.path());
    let tree = common::snapshot_tree(dst.path());
    assert_eq!(
        tree.files.len(),
        2,
        "no duplicates for identical re-transfer: {:?}",
        tree.files
    );

    // Rename policy still lands "(2)" copies when the content actually
    // differs — that is the real conflict the policy exists for.
    write_file(&src.join("a.txt"), b"second transfer content - changed");
    let third = run_transfer_conflict(&src, dst.path(), 1, ConflictPolicy::Rename).await;
    assert_eq!(
        third.send.resumed_chunks, 1,
        "b.bin unchanged => resumed; a.txt differs => re-sent"
    );
    assert_eq!(
        common::read_file(dst.path(), "a.txt"),
        b"first transfer content - complete"
    );
    assert_eq!(
        common::read_file(dst.path(), "a (2).txt"),
        b"second transfer content - changed"
    );
    assert_eq!(common::read_file(dst.path(), "nested/b.bin"), bin);
    assert!(
        !dst.path().join("nested/b (2).bin").exists(),
        "identical b.bin keeps one copy"
    );
    assert_no_part_files(dst.path());
    let tree = common::snapshot_tree(dst.path());
    assert_eq!(
        tree.files.len(),
        3,
        "only the truly conflicting file duplicates: {:?}",
        tree.files
    );
    assert_eq!(tree.dirs.len(), 1);
}
