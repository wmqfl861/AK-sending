//! TEST_PLAN T11（边界补充）：极限名称与极限路径长度。
//!
//! 层级声明：L2 环回形态（127.0.0.1 上走完整 QUIC + TLS + 引擎），不是
//! 真机或跨网验证。本文件覆盖四个边界形态：
//!
//! - **最大可传输文件名**：文件系统每组件上限 255（NTFS 按 UTF-16 单元、
//!   Linux 常见文件系统按字节）。本文件实测发现并钉住一个引擎边界：
//!   接收端先落 `<name>.akpart` 再改名提交，因此**文件**组件还要再留
//!   7 个字符给后缀 —— 248 字节（ASCII）名可完整传输，249 字节名在
//!   part 阶段被 OS 拒绝（Windows ERROR_INVALID_NAME 123 / Linux
//!   ENAMETOOLONG），见下方 `..._part_suffix_boundary` 双路断言。
//!   **目录**条目不走 part 文件，255 字节目录名不受此限。
//! - **路径总长逼近 Windows 260** 与 **`\\?\` verbatim 前缀下的更长路径**：
//!   经典 Win32 路径上限 260 字符；`fs::canonicalize` 在 Windows 上返回
//!   `\\?\` verbatim 形态，把上限抬到 ~32767。发送端 `LocalSource` 不做
//!   canonicalize（源按传入形态扫描），因此本文件源树统一以 canonicalize
//!   后的 verbatim 路径创建与传入；接收端 `LocalSaveTarget::open` 自行
//!   canonicalize，普通形态的目的目录即可承接 >260 的结果树。
//! - **尾随连续点号/空格**：Win32 路径规范化会剥掉组件尾部的点与空格
//!   （名字无法逐字节往返），ak-core 的 `validate_rel_path` 在 manifest
//!   校验层直接拒绝 —— 与 t11 主文件的 Windows 保留名测试同一手法。
//! - **单字符名**（ASCII 与 CJK）：最小合法组件。
//!
//! 诚实边界：若某形态本机创建失败（文件系统拒绝），测试 eprintln 原因并
//! 跳过该形态，不硬凑；长度断言只对成功创建的形态主张传输正确性。
//!
//! 本机实测（2026-11-03，NTFS / Windows 22624）：9/9 名称形态全部创建
//! 成功并逐字节往返；长路径链两端普通形态长度 近260 为 258/249、超长为
//! 360/351（源经 verbatim 创建与发送，>260 的落盘由接收端自身的
//! canonicalize 承接）；249 字节文件名钉出 `.akpart` 组件名扩张限制。
#![forbid(unsafe_code)]

mod common;

use std::fs;
use std::path::Path;

use ak_core::handle::ConflictPolicy;
use ak_core::manifest::{validate_rel_path, FileEntry, MAX_REL_PATH_BYTES};
use ak_engine::EngineError;
use ak_net::StreamError;

use common::{
    assert_no_part_files, assert_trees_identical, read_file, run_transfer, try_run_transfer_full,
    Lcg,
};

/// 尝试落盘一个形态；失败时 eprintln 原因并返回 false（调用方跳过该形态，
/// 绝不换个名字硬凑）。
fn try_write(path: &Path, bytes: &[u8]) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!(
                "SKIP {}: parent dir creation failed on this host: {err}",
                path.display()
            );
            return false;
        }
    }
    match fs::write(path, bytes) {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "SKIP {}: file creation failed on this host: {err}",
                path.display()
            );
            false
        }
    }
}

/// 满长名（248 字节文件名 = 255 − len(".akpart")，255 字节目录名，ASCII /
/// 带扩展名 / emoji / CJK / 空文件）与单字符名：经完整引擎环回传输后逐
/// 字节一致。
#[tokio::test]
async fn max_component_and_single_char_names_round_trip() {
    let base = tempfile::tempdir().unwrap();
    let src_plain = base.path().join("t11x-src");
    fs::create_dir_all(&src_plain).unwrap();
    // Verbatim 源：满长名目录内的文件绝对路径已 ~310 字符（temp 基座
    // ~50 + 255 + 分隔符 + 叶子名），普通形态创建/扫描会撞 260 上限。
    let src = src_plain.canonicalize().unwrap();
    let dst = tempfile::tempdir().unwrap();

    // (rel, 字节数)：文件名取 248 字节（接收端 part 后缀 +7 恰好顶到 255，
    // 见 `..._part_suffix_boundary`）；目录名不带后缀，直接顶到 255。
    // 覆盖不同 UTF-8 字符宽度；5000 字节载荷跨 2 个 4 KiB chunk；空文件
    // 钉 0-chunk 形态；末尾三组为单字符名（ASCII 与 CJK）。
    let specs: Vec<(String, usize)> = vec![
        ("a".repeat(248), 5_000),
        (format!("{}{}", "b".repeat(244), ".bin"), 1_024),
        ("🚀".repeat(62), 777),
        ("中".repeat(82), 4_096),
        ("e".repeat(248), 0),
        (format!("{}/leaf", "d".repeat(255)), 1_234),
        ("z".into(), 1),
        ("a/q".into(), 2),
        ("空/文".into(), 3),
    ];
    for (rel, _) in &specs {
        assert!(
            rel.len() <= MAX_REL_PATH_BYTES,
            "rel path {rel:?} ({}) exceeds the manifest budget {MAX_REL_PATH_BYTES}",
            rel.len()
        );
    }

    let mut rng = Lcg::new(0xE71E_2026_1103);
    let mut created: Vec<(String, Vec<u8>)> = Vec::new();
    for (rel, size) in &specs {
        let bytes = rng.bytes(*size);
        if try_write(&src.join(rel), &bytes) {
            created.push((rel.clone(), bytes));
        }
    }
    assert!(
        !created.is_empty(),
        "host refused every extreme-name form; nothing to transfer"
    );
    let longest = created
        .iter()
        .map(|(rel, _)| rel.split('/').map(str::len).max().unwrap_or(0))
        .max()
        .unwrap_or(0);
    eprintln!(
        "T11-extreme-names: {}/{} forms created on this host; longest component {} bytes",
        created.len(),
        specs.len(),
        longest
    );

    let outcome = run_transfer(&src, dst.path(), 4).await;

    let expected = common::snapshot_tree(&src);
    assert_eq!(outcome.send.files, expected.entry_count());
    assert_eq!(outcome.recv.files, expected.entry_count());
    assert_eq!(outcome.send.bytes, expected.total_bytes());
    assert_eq!(outcome.recv.bytes, expected.total_bytes());
    assert_eq!(outcome.send.job_id, outcome.recv.job_id);

    // 接收侧同样以 verbatim 形态核对（满长名使结果绝对路径超过 260）。
    let dst_real = dst.path().canonicalize().unwrap();
    for (rel, bytes) in &created {
        assert_eq!(
            read_file(&dst_real, rel),
            *bytes,
            "byte mismatch for extreme-name form {rel:?}"
        );
    }
    assert_trees_identical(&src, &dst_real);
    assert_no_part_files(&dst_real);

    // 手动以 verbatim 形态清理超长路径子树，TempDir 析构只剩短路径要收。
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst_real);
}

/// 实测发现的接收端边界（2026-11-03，本机 NTFS 探针：247/248 完成、
/// 249/255 失败）：接收端为每个文件先开 `<name>.akpart` 再改名提交，249
/// 字节（ASCII）名的 part 组件 256 字符已超文件系统 255 上限，OS 拒绝
/// （Windows ERROR_INVALID_NAME 123，实测 recv=Io(123)、连接拆除、
/// send=Stream(Connection("connection lost"))）。双路断言（与 t13 钉
/// ResumeRequest 缺陷同一手法）：引擎若将来改用不扩张组件名的 part 方案
/// （如按摘要命名 part），本测试转为断言完整落盘；现状则必须精确复现
/// 该失败且零最终文件落盘（不误报完成）。
#[tokio::test]
async fn two_hundred_forty_nine_byte_name_hits_part_suffix_boundary() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t11b-src");
    let name = "n".repeat(249);
    common::write_file(&src.join(&name), b"boundary");
    let dst = tempfile::tempdir().unwrap();
    let outcome = try_run_transfer_full(&src, dst.path(), 1, ConflictPolicy::Fail, 60).await;

    match (&outcome.send, &outcome.recv) {
        (Ok(send), Ok(recv)) => {
            eprintln!(
                "T11-249: transfer COMPLETES (part naming no longer inflates components): \
                 files={} bytes={}",
                send.files, recv.bytes
            );
            assert_eq!(send.files, 1);
            assert_eq!(recv.bytes, 8);
            assert_trees_identical(&src, dst.path());
            assert_no_part_files(dst.path());
        }
        (Err(EngineError::Stream(StreamError::Connection(_))), Err(EngineError::Io(io_err))) => {
            // KNOWN LIMITATION (recorded, not worked around in ak-itests): the
            // receiver's `<name>.akpart` inflates the final component by 7.
            #[cfg(windows)]
            assert_eq!(
                io_err.raw_os_error(),
                Some(123),
                "expected ERROR_INVALID_NAME for the 256-char part component, got {io_err}"
            );
            eprintln!(
                "T11-249: KNOWN LIMITATION reproduced — part component `<249-byte name>.akpart` \
                 (256 chars) exceeds the 255-per-component cap; receiver io error {io_err}, \
                 sender saw the connection torn down; no file may be committed"
            );
            let tree = common::snapshot_tree(dst.path());
            assert!(
                tree.files.is_empty() && tree.dirs.is_empty(),
                "aborted job must not commit anything, found files {:?} dirs {:?}",
                tree.files.keys().collect::<Vec<_>>(),
                tree.dirs.iter().collect::<Vec<_>>()
            );
        }
        (send, recv) => panic!("T11-249: unexpected outcome — send={send:?} recv={recv:?}"),
    }
}

/// 构造一条 `/` 分隔的组件链，使「最长基座 + 链 + 文件名」的普通形态
/// 字符总长钉在 `target`（或差 1）。前段用 50 字符组件，末尾一个定长
/// 组件补齐余量。
fn chain_for_target(target: usize, file: &str, max_base: usize) -> String {
    let mut rel = String::from("chain");
    loop {
        let abs = max_base + 1 + rel.chars().count() + 1 + file.chars().count();
        if abs + 1 + 50 > target {
            break;
        }
        rel.push('/');
        rel.push_str(&"x".repeat(50));
    }
    let abs = max_base + 1 + rel.chars().count() + 1 + file.chars().count();
    let pad = target - abs;
    if pad >= 2 {
        // 一个长度 pad-1 的组件共新增 pad 字符（含分隔符）。
        rel.push('/');
        rel.push_str(&"y".repeat(pad - 1));
    }
    rel
}

/// 路径总长逼近 Windows 260 与 `\\?\` verbatim 下的更长路径：一条链的
/// 最深文件普通形态总长落在 [255, 260)（两端都低于经典上限），另一条
/// 超过 260（源经 verbatim 创建与发送，接收端靠自身的 canonicalize 落盘），
/// 传输后逐字节一致。长度计数用字符数（temp 路径全 ASCII，字符数即
/// Win32 MAX_PATH 的 UTF-16 计数）；manifest 预算按 UTF-8 字节校验。
#[tokio::test]
async fn paths_near_and_beyond_windows_max_path_round_trip() {
    const NEAR_TARGET: usize = 258;
    const FAR_TARGET: usize = 360;

    let base = tempfile::tempdir().unwrap();
    let src_plain = base.path().join("t11p-src");
    fs::create_dir_all(&src_plain).unwrap();
    let src = src_plain.canonicalize().unwrap();
    let dst = tempfile::tempdir().unwrap();

    let src_base = src_plain.to_string_lossy().chars().count();
    let dst_base = dst.path().to_string_lossy().chars().count();
    let max_base = src_base.max(dst_base);

    let near_rel = chain_for_target(NEAR_TARGET, "leaf-near.bin", max_base);
    let far_rel = chain_for_target(FAR_TARGET, "leaf-far.bin", max_base);
    let plain_len = |rel: &str, file: &str, base_len: usize| {
        base_len + 1 + rel.chars().count() + 1 + file.chars().count()
    };
    let near_src = plain_len(&near_rel, "leaf-near.bin", src_base);
    let near_dst = plain_len(&near_rel, "leaf-near.bin", dst_base);
    let far_src = plain_len(&far_rel, "leaf-far.bin", src_base);
    let far_dst = plain_len(&far_rel, "leaf-far.bin", dst_base);

    assert!(
        near_src < 260 && near_dst < 260,
        "near chain must stay under the classic 260 limit on both sides (src {near_src}, dst {near_dst})"
    );
    assert!(
        near_src.max(near_dst) >= 255,
        "near chain must actually approach 260, max side is {}",
        near_src.max(near_dst)
    );
    assert!(
        far_src > 260 && far_dst > 260,
        "far chain must exceed 260 on both sides (src {far_src}, dst {far_dst})"
    );
    for rel in [&near_rel, &far_rel] {
        assert!(
            rel.len() <= MAX_REL_PATH_BYTES,
            "rel path ({}) exceeds the manifest budget {MAX_REL_PATH_BYTES}",
            rel.len()
        );
    }
    eprintln!(
        "T11-long-paths: plain-form file lengths — near src {near_src} / dst {near_dst} (<260), \
         far src {far_src} / dst {far_dst} (>260); far rel {} bytes",
        far_rel.len()
    );

    let mut rng = Lcg::new(0x10A7_2026_1103);
    let near_bytes = rng.bytes(1_234);
    let far_bytes = rng.bytes(4_321);
    let mut leaves: Vec<(String, Vec<u8>)> = Vec::new();
    let near_file_rel = format!("{near_rel}/leaf-near.bin");
    if try_write(&src.join(&near_file_rel), &near_bytes) {
        leaves.push((near_file_rel, near_bytes));
    }
    let far_file_rel = format!("{far_rel}/leaf-far.bin");
    if try_write(&src.join(&far_file_rel), &far_bytes) {
        leaves.push((far_file_rel, far_bytes));
    }
    assert!(
        !leaves.is_empty(),
        "host refused both long-path forms; nothing to transfer"
    );

    let outcome = run_transfer(&src, dst.path(), 4).await;

    let expected = common::snapshot_tree(&src);
    assert_eq!(outcome.send.files, expected.entry_count());
    assert_eq!(outcome.recv.files, expected.entry_count());
    assert_eq!(outcome.send.bytes, expected.total_bytes());
    assert_eq!(outcome.recv.bytes, expected.total_bytes());
    assert_eq!(outcome.send.job_id, outcome.recv.job_id);

    let dst_real = dst.path().canonicalize().unwrap();
    for (rel, bytes) in &leaves {
        assert_eq!(
            read_file(&dst_real, rel),
            *bytes,
            "byte mismatch for long-path leaf {rel:?}"
        );
    }
    assert_trees_identical(&src, &dst_real);
    assert_no_part_files(&dst_real);

    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst_real);
}

/// 尾随连续点号/空格与点号组件在 manifest 校验层被拒；内部连续点号、
/// 内部/前导空格不受牵连（校验只打击真正无法逐字节往返的形态）。
/// 条目构造器（清单构建层）同样拒绝。附带一个本机形态探针：Win32
/// 路径规范化会剥离尾随点号，因此「字面尾随点号名的源文件」在本机
/// 无法存在 —— eprintln 说明并依赖校验层断言兜底。
#[test]
fn trailing_dots_and_spaces_rejected_at_manifest_validation() {
    let rejected = [
        "file.", "file..", "file...", "file ", "file  ", "dir./x", "x/dir.", "dir /y", "y/dir ",
        "..", ".", "a/..", "../a", "a/../b", "a//b", "/a", "a/", "",
    ];
    for path in rejected {
        assert!(
            validate_rel_path(path).is_err(),
            "{path:?} must be rejected (trailing dot/space or dot component)"
        );
    }

    // Precision: only TRAILING dots/spaces and dot components are illegal;
    // interior runs of dots and interior/leading spaces round-trip fine.
    let accepted = [
        "a..b",
        "v1...final",
        "two  spaces.bin",
        " lead.txt",
        "a . b",
    ];
    for path in accepted {
        assert!(
            validate_rel_path(path).is_ok(),
            "{path:?} must stay legal (no trailing dot/space)"
        );
    }

    // The manifest-entry constructors share the same rule set.
    assert!(FileEntry::new("x.", 0, 0, [0; 32], Vec::new()).is_err());
    assert!(FileEntry::new("y ", 1, 0, [0; 32], vec![[0; 32]]).is_err());
    assert!(FileEntry::directory("z.").is_err());
    assert!(FileEntry::directory("w ").is_err());

    // Host-shape probe: a literal trailing-dot name cannot exist via normal
    // Win32 paths (the dot is stripped during path normalization), so the
    // "foreign source carries one" scenario is skipped on this host and the
    // validation-layer rejection above is the guard.
    let probe = tempfile::tempdir().unwrap();
    let attempt = probe.path().join("trailing.");
    let write_result = fs::write(&attempt, b"x");
    let listing: Vec<String> = fs::read_dir(probe.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    match (&write_result, listing.iter().find(|name| name.as_str() == "trailing.")) {
        (Err(err), _) => {
            eprintln!("SKIP: this host refuses creating trailing-dot names outright: {err}")
        }
        (Ok(_), Some(_)) => eprintln!(
            "NOTE: literal 'trailing.' was creatable on this host; the wire path is still gated by the validation-layer rejection"
        ),
        (Ok(_), None) => eprintln!(
            "SKIP: Win32 path normalization stripped the trailing dot (probe dir now holds {listing:?}); literal trailing-dot/space source names cannot exist here, rejection pinned at the manifest validation layer"
        ),
    }
}
