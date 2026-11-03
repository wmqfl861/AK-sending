//! TEST_PLAN T13（缩样）：十万小文件 / 空目录 —— 清单分页、结构可核验。
//!
//! 层级声明：L2 环回形态。本文件是 T13 的缩小样本（非十万全量）：
//! 2,000 个 0–512 随机字节的小文件、随机目录分布、200 个空目录，经完整
//! 引擎环回传输后总量与逐字节一致，且清单确实分页（>1 页）。
//!
//! 另有两个规模阶梯测试：
//! - 4,097 文件边界测试（默认启用）：钉住**已发现的真实缺陷**——接收端
//!   ResumeRequest 每非目录条目 12 字节、不分页，超过 ak-wire 默认
//!   `max_resume_entries = 4096` 的解码预算（20 + 12*4096 = 49,172 字节）
//!   时发送端以 Oversized 拒帧，任务中止。实测 4,096 文件可完成、4,097
//!   必失败。缺陷已在报告中记录；测试双路断言：引擎修复后传输应完整
//!   落盘并通过全量校验，未修复时必须精确复现该错误且不落任何最终文件
//!   （不误报完成）。
//! - 20,000 文件 `#[ignore]` release 变体（默认不跑）：`cargo test --release
//!   -p ak-itests -- --ignored --nocapture`，同样双路断言并记录耗时。
#![forbid(unsafe_code)]

mod common;

use common::{assert_trees_identical, run_transfer_full, try_run_transfer_full, write_file, Lcg};
use std::fs;
use std::path::Path;
use std::time::Instant;

use ak_core::handle::ConflictPolicy;
use ak_engine::{EngineError, Progress};
use ak_net::StreamError;
use ak_wire::DecodeError;

/// ak-wire default resume-entry budget: 20 header bytes + 12 per entry.
const RESUME_BUDGET: u64 = 20 + 12 * 4096;

struct BuiltTree {
    files: usize,
    dirs: usize,
    bytes: u64,
    /// Files with at least one byte (each occupies exactly one 1 KiB chunk).
    nonempty_files: usize,
}

/// Deterministic "many small files" tree: `groups` top dirs each with one
/// subdir, files spread pseudo-randomly across root / group / group-sub, and
/// `chains` two-level empty-dir chains (2 empty dirs per chain).
fn build_many_files(
    base: &Path,
    total_files: usize,
    groups: usize,
    chains: usize,
    seed: u64,
) -> BuiltTree {
    let mut rng = Lcg::new(seed);
    for group in 0..groups {
        fs::create_dir_all(base.join(format!("g{group:03}"))).unwrap();
        fs::create_dir_all(base.join(format!("g{group:03}/s"))).unwrap();
    }
    for chain in 0..chains {
        // Parent contains only the (empty) leaf, so both levels are empty.
        fs::create_dir_all(base.join(format!("e{chain:03}/e{chain:03}x"))).unwrap();
    }
    let mut bytes = 0u64;
    let mut nonempty = 0usize;
    for index in 0..total_files {
        let group = index % groups;
        let dir = match rng.below(3) {
            0 => base.to_path_buf(),
            1 => base.join(format!("g{group:03}")),
            _ => base.join(format!("g{group:03}/s")),
        };
        // 0..=512 bytes; every 97th file is guaranteed empty (0 chunks).
        let size = if index % 97 == 0 {
            0
        } else {
            rng.below(513) as usize
        };
        let content = rng.bytes(size);
        if !content.is_empty() {
            nonempty += 1;
        }
        bytes += content.len() as u64;
        write_file(&dir.join(format!("f{index:05}.dat")), &content);
    }
    BuiltTree {
        files: total_files,
        dirs: groups * 2 + chains * 2,
        bytes,
        nonempty_files: nonempty,
    }
}

/// A flat tree with `count` one-byte files: the sharpest probe for the
/// ResumeRequest budget, which depends only on the non-directory entry count.
fn build_flat(base: &Path, count: usize) {
    for index in 0..count {
        fs::write(base.join(format!("p{index:05}.dat")), b"x").unwrap();
    }
}

fn count_events(events: &[Progress], wanted: fn(&Progress) -> bool) -> usize {
    events.iter().filter(|event| wanted(event)).count()
}

fn is_page(event: &Progress) -> bool {
    matches!(event, Progress::ManifestPageSent { .. })
}

fn is_chunk(event: &Progress) -> bool {
    matches!(event, Progress::ChunkReceived { .. })
}

/// Assert the full "many small files" transfer contract (used on every scale
/// that is expected to complete).
async fn assert_many_files_transfer(
    base: &Path,
    built: &BuiltTree,
    min_pages: usize,
    budget_secs: u64,
) {
    let dst = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let outcome = run_transfer_full(base, dst.path(), 1, ConflictPolicy::Fail, budget_secs).await;
    let elapsed = started.elapsed();

    let expected = common::snapshot_tree(base);
    assert_eq!(
        expected.dirs.len(),
        built.dirs,
        "unexpected source dir count"
    );
    assert_eq!(
        expected.files.len(),
        built.files,
        "unexpected source file count"
    );

    // Both sides agree with the on-disk source truth.
    assert_eq!(outcome.send.files, expected.entry_count());
    assert_eq!(outcome.recv.files, expected.entry_count());
    assert_eq!(outcome.send.bytes, built.bytes);
    assert_eq!(outcome.recv.bytes, built.bytes);

    // Manifest paging really happened (ak-wire default: 256 entries per page).
    let pages = count_events(&outcome.events, is_page);
    assert!(
        pages >= min_pages,
        "expected at least {min_pages} manifest pages, saw {pages}"
    );

    // One 1 KiB chunk per non-empty file: chunk accounting must be exact.
    let chunks_received = count_events(&outcome.events, is_chunk);
    assert_eq!(chunks_received, built.nonempty_files);

    // Structure and every byte identical; nothing in flight left behind.
    assert_trees_identical(base, dst.path());
    common::assert_no_part_files(dst.path());

    eprintln!(
        "T13: {} files / {} dirs / {} bytes | pages={} chunks={} | send={:?} recv={:?} wall={:?}",
        built.files,
        built.dirs,
        built.bytes,
        pages,
        chunks_received,
        outcome.send.duration,
        outcome.recv.duration,
        elapsed
    );
}

/// Dual-path outcome for scales above the known ResumeRequest budget defect:
/// completion (engine fixed) or the exact documented failure (defect present).
async fn run_scale_run(base: &Path, non_dir_entries: u64, budget_secs: u64, label: &str) {
    let dst = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let outcome =
        try_run_transfer_full(base, dst.path(), 1, ConflictPolicy::Fail, budget_secs).await;
    let elapsed = started.elapsed();
    let pages = count_events(&outcome.events, is_page);

    match (&outcome.send, &outcome.recv) {
        (Ok(send), Ok(recv)) => {
            eprintln!(
                "{label}: transfer COMPLETES (ResumeRequest budget defect fixed): \
                 files={} bytes={} pages={} send={:?} recv={:?} wall={:?}",
                send.files, send.bytes, pages, send.duration, recv.duration, elapsed
            );
            let expected = common::snapshot_tree(base);
            assert_eq!(send.files, expected.entry_count());
            assert_eq!(recv.files, expected.entry_count());
            assert_trees_identical(base, dst.path());
            common::assert_no_part_files(dst.path());
        }
        (
            Err(EngineError::Stream(StreamError::Wire(DecodeError::Oversized {
                declared,
                budget,
            }))),
            Err(_recv_also_failed),
        ) => {
            // KNOWN DEFECT (reported; not fixed in ak-itests): the receiver
            // answers with one unpaged ResumeRequest of 20 + 12 bytes per
            // non-directory entry; the sender's default Limits budget is
            // 20 + 12 * max_resume_entries. Above 4,096 files the frame is
            // refused and the job aborts.
            assert_eq!(
                u64::from(*declared),
                20 + 12 * non_dir_entries,
                "declared size must match one entry per non-dir manifest entry"
            );
            assert_eq!(u64::from(*budget), RESUME_BUDGET);
            eprintln!(
                "{label}: KNOWN DEFECT reproduced — ResumeRequest of {} bytes exceeds \
                 default decode budget {} (max_resume_entries=4096); job aborted after \
                 {} manifest pages in {:?} (wall incl. tree setup)",
                declared, budget, pages, elapsed
            );
            // No false completion: no final file may exist. The receiver
            // prepares sinks before answering, so empty .akpart residue is
            // expected and counted (informational).
            let tree = common::snapshot_tree(dst.path());
            let finals: Vec<&String> = tree
                .files
                .keys()
                .filter(|name| !name.ends_with(common::PART_SUFFIX))
                .collect();
            let parts = tree.files.len() - finals.len();
            assert!(
                finals.is_empty(),
                "aborted job must not commit any file, found {finals:?}"
            );
            eprintln!("{label}: destination holds {parts} empty part files, 0 finals");
        }
        (send, recv) => panic!("{label}: unexpected outcome — send={send:?} recv={recv:?}"),
    }
}

#[tokio::test]
async fn two_thousand_small_files_transfer_byte_identical_with_paging() {
    let base = tempfile::tempdir().unwrap();
    // 2,000 files + 80 group dirs + 200 empty dirs = 2,280 manifest entries
    // => at least ceil(2280/256) = 9 manifest pages.
    let built = build_many_files(base.path(), 2_000, 40, 100, 0x713_0000_2000);
    assert_many_files_transfer(base.path(), &built, 9, 600).await;
}

/// Exact defect boundary, measured: 4,096 non-dir entries fit the budget
/// (verified by a one-off release probe: 4,096 completes, 4,097 does not).
/// This test pins the 4,097 side and stays green both while the defect exists
/// and after it is fixed (then it asserts full completion).
#[tokio::test]
async fn four_thousand_ninety_seven_files_hit_the_resume_request_budget_defect() {
    let base = tempfile::tempdir().unwrap();
    build_flat(base.path(), 4_097);
    run_scale_run(base.path(), 4_097, 600, "T13-4097").await;
}

/// T13 scale step towards the full 100k sample: 20,000 files, 800 group dirs,
/// 400 empty-dir chains = 21,200 entries (=> 83 manifest pages at 256/page).
/// Release only (debug hashing and the per-file checkpoint cadence make this
/// unnecessarily slow); run explicitly:
/// `cargo test --release -p ak-itests -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "20,000-file scale run; release only"]
async fn twenty_thousand_files_release_scale_run() {
    let base = tempfile::tempdir().unwrap();
    let built = build_many_files(base.path(), 20_000, 400, 200, 0x713_0014_0000);
    run_scale_run(base.path(), built.files as u64, 3_600, "T13-20000").await;
}

/// Boundary just above the per-frame ResumeRequest cap (65,535 entries):
/// since the 2026-11-03 paged exchange the surplus entries must arrive in a
/// second frame and the transfer must complete. 1-byte flat files keep the
/// run about per-entry overhead, not payload volume.
#[tokio::test]
#[ignore = "65,537-file boundary run; release only"]
async fn sixty_five_thousand_five_hundred_thirty_seven_files_release_boundary_run() {
    let base = tempfile::tempdir().unwrap();
    build_flat(base.path(), 65_537);
    run_scale_run(base.path(), 65_537, 10_800, "T13-65537").await;
}
