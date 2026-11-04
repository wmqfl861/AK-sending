//! TEST_PLAN T10（加深）：断网 / 双端进程重启 —— 内容身份与检查点语义链。
//!
//! 层级声明：L2 环回形态（ak-engine 已单测覆盖“传输中断→检查点存活→续传”
//! 的断点形态，此处不重复）。本文件加深同一源的连续任务链语义：
//!
//! 1. 第一次传输正常完成（job_id = J1）。
//! 2. 修改一个文件内容 + 新增一个文件 + 移走一个文件后再传：
//!    - job_id 必须不同（J2 ≠ J1，内容身份变化）；
//!    - 新树**完整重传**落盘（v1 语义：新任务不是增量，resumed_chunks == 0，
//!      如实断言）；
//!    - 旧文件不动：第一轮的落盘结果保持原字节。
//! 3. 把源恢复成与第一次**逐字节且同 mtime** 的状态（借助 rename 保留
//!    mtime）再传：job_id 必须回到 J1（确定性内容身份）。
#![forbid(unsafe_code)]

mod common;

use common::{assert_trees_identical, run_transfer, run_transfer_conflict, write_file};
use std::collections::HashSet;
use std::fs;

use ak_core::handle::ConflictPolicy;

const ALPHA_V1: &[u8] = b"alpha version one - original bytes";
const ALPHA_V2: &[u8] = b"alpha version TWO - mutated, different length";
const KEEP: &[u8] = b"keep me unchanged across all runs";
const GONE: &[u8] = b"this file leaves the tree for run two";
const DATA: &[u8] = b"nested payload unchanged";
const NEW: &[u8] = b"newly added file content";

#[tokio::test]
async fn same_source_chain_changes_job_id_with_content_and_restores_it() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("chain-src");
    write_file(&src.join("alpha.txt"), ALPHA_V1);
    write_file(&src.join("keep.bin"), KEEP);
    write_file(&src.join("gone.bin"), GONE);
    write_file(&src.join("nested/data.bin"), DATA);
    // Stash lives OUTSIDE the source root so stashed files never enter a
    // manifest. Renames stay on the same volume (same tempdir base), which
    // preserves file mtimes — that is what makes run three's job id equal
    // to run one's.
    let stash = base.path().join("stash");
    fs::create_dir_all(&stash).unwrap();

    // ---- Run 1: clean completion ------------------------------------------
    let dst1 = tempfile::tempdir().unwrap();
    let run1 = run_transfer(&src, dst1.path(), 1).await;
    assert_trees_identical(&src, dst1.path());
    let job1 = run1.send.job_id;

    // ---- Mutate: modify one file, add one file, remove one file -----------
    // alpha.txt and gone.bin are MOVED to the stash (mtime-preserving), then
    // a fresh alpha.txt with new content is written and new.bin added.
    fs::rename(src.join("alpha.txt"), stash.join("alpha.txt")).unwrap();
    fs::rename(src.join("gone.bin"), stash.join("gone.bin")).unwrap();
    write_file(&src.join("alpha.txt"), ALPHA_V2);
    write_file(&src.join("new.bin"), NEW);

    // ---- Run 2: same destination, rename-on-conflict ----------------------
    let run2 = run_transfer_conflict(&src, dst1.path(), 1, ConflictPolicy::Rename).await;
    assert_ne!(
        run2.send.job_id, job1,
        "content identity must change when the tree changes"
    );

    // Wave-17 semantics, asserted honestly: a new job re-transfers only the
    // files whose content actually changed. The two untouched files
    // (keep.bin, nested/data.bin) are recognized by the committed-final fast
    // path (byte-identical finals from run 1) and skipped; the mutated
    // alpha.txt and the new new.bin are sent fresh.
    assert_eq!(
        run2.send.resumed_chunks, 2,
        "only the two byte-identical files resume via committed finals"
    );
    assert_eq!(run2.recv.resumed_chunks, 2);

    // Old files untouched (run-1 bytes stay exactly as they were).
    assert_eq!(common::read_file(dst1.path(), "alpha.txt"), ALPHA_V1);
    assert_eq!(common::read_file(dst1.path(), "keep.bin"), KEEP);
    assert_eq!(common::read_file(dst1.path(), "gone.bin"), GONE);
    assert_eq!(common::read_file(dst1.path(), "nested/data.bin"), DATA);
    // New tree fully on disk: the mutated file lands renamed (true content
    // conflict), the addition lands at its exact path, and the byte-identical
    // unchanged files keep their single run-1 copies (wave-17 fast path — no
    // "(2)" duplicates of unchanged content).
    assert_eq!(common::read_file(dst1.path(), "alpha (2).txt"), ALPHA_V2);
    assert_eq!(common::read_file(dst1.path(), "new.bin"), NEW);
    assert!(
        !dst1.path().join("keep (2).bin").exists(),
        "identical keep.bin keeps one copy"
    );
    assert!(
        !dst1.path().join("nested/data (2).bin").exists(),
        "identical data.bin keeps one copy"
    );
    common::assert_no_part_files(dst1.path());

    // Completeness: every file of the mutated source has a byte-identical
    // copy somewhere in the destination tree.
    let dst_tree = common::snapshot_tree(dst1.path());
    let dst_contents: HashSet<&Vec<u8>> = dst_tree.files.values().collect();
    for (name, bytes) in common::snapshot_tree(&src).files {
        assert!(
            dst_contents.contains(&bytes),
            "source file {name} has no byte-identical copy in the destination"
        );
    }

    // ---- Restore the original tree (mtimes preserved via rename) ----------
    fs::remove_file(src.join("alpha.txt")).unwrap();
    fs::rename(stash.join("alpha.txt"), src.join("alpha.txt")).unwrap();
    fs::remove_file(src.join("new.bin")).unwrap();
    fs::rename(stash.join("gone.bin"), src.join("gone.bin")).unwrap();

    // ---- Run 3: original source again => identical job id ------------------
    let dst2 = tempfile::tempdir().unwrap();
    let run3 = run_transfer(&src, dst2.path(), 1).await;
    assert_eq!(
        run3.send.job_id, job1,
        "byte-and-mtime identical source must restore the original job id"
    );
    assert_eq!(run3.recv.job_id, job1);
    assert_trees_identical(&src, dst2.path());
    common::assert_no_part_files(dst2.path());
}
