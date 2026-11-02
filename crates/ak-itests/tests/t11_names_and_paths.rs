//! TEST_PLAN T11（形态映射）：文件名冲突 / `..` / 链接竞态 —— 名称与路径保真。
//!
//! 层级声明：L2 环回形态（127.0.0.1 上走完整 QUIC + TLS + 引擎），不是真机
//! 或跨网验证。路径越界 / 符号链接防护的对抗性部分另见 t05（篡改清单）与
//! ak-storage 单测；本文件验证合法但“难缠”的名称在传输后逐字节、逐结构一致：
//!
//! - Unicode 文件名（中文 / emoji / 重音字符 / 西里尔文）
//! - 含空格的目录与文件名
//! - CRLF 文本与任意二进制（含 NUL / 0xFF / 全字节值）混合内容
//! - 20 层深层嵌套
//! - Windows 保留名（CON 等）源在本机无法存在，场景跳过并在测试内注释说明
#![forbid(unsafe_code)]

mod common;

use common::{assert_no_part_files, assert_trees_identical, run_transfer, write_file, Lcg};
use std::fs;

#[tokio::test]
async fn unicode_names_spaces_crlf_binary_and_deep_nesting_round_trip() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("t11-src");
    fs::create_dir_all(&src).unwrap();
    let dst = tempfile::tempdir().unwrap();

    // --- Unicode file names ------------------------------------------------
    write_file(
        &src.join("中文文件名.txt"),
        "中文内容，验证字节保真\r\n第二行\r\n".as_bytes(),
    );
    // All 256 byte values twice: NULs, 0xFF, high bytes, everything.
    let all_bytes: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
    write_file(&src.join("🎉🚀.bin"), &all_bytes);
    // Accented Latin name, random binary payload (may contain NUL bytes).
    let mut rng = Lcg::new(0x00C0_FFEE);
    write_file(&src.join("café-ünïcode-ñame.dat"), &rng.bytes(400));
    // Emoji directory + Chinese subdir + Cyrillic file.
    write_file(
        &src.join("emoji dir 📁/nested 中文/текст.txt"),
        "Привет\r\nмир\r\n".as_bytes(),
    );

    // --- Names with spaces (interior spaces are legal; trailing space/dot
    // are rejected by ak-core validation and cannot appear on the wire) -----
    write_file(
        &src.join("folder with spaces/naïve file name.bin"),
        b"line one\r\nline two\nbare CR\rNUL\x00\xff tail",
    );

    // --- CRLF + binary mixed content ---------------------------------------
    let mut mixed = Vec::new();
    mixed.extend_from_slice(b"header\r\n");
    mixed.extend_from_slice(&[0x00, 0x01, 0xFE, 0xFF]);
    mixed.extend_from_slice(b"\r\ntail\r\n");
    write_file(&src.join("crlf+binary mixed.dat"), &mixed);

    // --- 20-level deep nesting ---------------------------------------------
    let mut deep = src.clone();
    for level in 1..=20 {
        deep.push(format!("d{level:02}"));
    }
    fs::create_dir_all(&deep).unwrap();
    let mut rng2 = Lcg::new(0xDEAD_BEEF);
    write_file(&deep.join("leaf.bin"), &rng2.bytes(1_234));

    // --- Empty directories, Unicode and spaced ------------------------------
    fs::create_dir_all(src.join("空目录 empty")).unwrap();
    fs::create_dir_all(src.join("space dir/inner dir")).unwrap();

    let expected = common::snapshot_tree(&src);
    assert!(expected.dirs.len() >= 24, "sanity: deep chain present");
    assert!(expected.files.len() >= 7);

    let outcome = run_transfer(&src, dst.path(), 4).await;

    // Totals reported by both sides must match the on-disk source tree.
    assert_eq!(outcome.send.files, expected.entry_count());
    assert_eq!(outcome.recv.files, expected.entry_count());
    assert_eq!(outcome.send.bytes, expected.total_bytes());
    assert_eq!(outcome.recv.bytes, expected.total_bytes());
    assert_eq!(outcome.send.job_id, outcome.recv.job_id);

    // Byte-identical files and identical directory structure.
    assert_trees_identical(&src, dst.path());
    assert_no_part_files(dst.path());

    // Spot-check the nastiest names explicitly (structure check already
    // covers them; these asserts give precise failure messages).
    assert_eq!(
        common::read_file(dst.path(), "中文文件名.txt"),
        "中文内容，验证字节保真\r\n第二行\r\n".as_bytes()
    );
    assert_eq!(common::read_file(dst.path(), "🎉🚀.bin"), all_bytes);
    let mut spot = dst.path().to_path_buf();
    for level in 1..=20 {
        spot.push(format!("d{level:02}"));
    }
    assert!(spot.join("leaf.bin").is_file());
    assert_eq!(
        fs::read(spot.join("leaf.bin")).unwrap(),
        common::read_file(&src, "d01/d02/d03/d04/d05/d06/d07/d08/d09/d10/d11/d12/d13/d14/d15/d16/d17/d18/d19/d20/leaf.bin")
    );
}

/// Windows reserved device names (CON, PRN, AUX, NUL, COM1..9, LPT1..9) as
/// *source* files cannot be constructed on this host at all: NTFS will not
/// create a normal file named `CON` outside the Win32 device namespace, so
/// the "source from another system contains CON" scenario is skipped here
/// (honest limitation). What IS verifiable locally is the guard: such names
/// are rejected by the shared path validation before they can reach storage
/// or the wire, so a foreign manifest carrying them fails closed in t05's
/// decode path instead of writing a device-named file.
#[test]
fn windows_reserved_name_source_is_impossible_and_rejected_at_validation() {
    for reserved in ["CON", "con.txt", "NUL.dat", "aux", "lpt1.log", "COM9"] {
        assert!(
            ak_core::manifest::validate_rel_path(reserved).is_err(),
            "reserved name {reserved:?} must be rejected"
        );
    }
    // Sanity for the skip: creating a literal CON file via std either fails
    // or silently targets the console device instead of creating a file, so
    // no loopback source with this name can exist. `exists()` alone is not a
    // proof (the device answers stat too) — require a real regular file.
    let base = tempfile::tempdir().unwrap();
    let con = base.path().join("CON");
    let _ = std::fs::write(&con, b"x");
    let created = std::fs::symlink_metadata(&con)
        .map(|meta| meta.is_file())
        .unwrap_or(false);
    if created {
        // If some future Windows/filesystem combo permits it, the transfer
        // path would still refuse it (validation above); note and move on.
        eprintln!(
            "NOTE: literal CON source was creatable on this host; reserved-name scenario still gated by validation"
        );
    } else {
        eprintln!(
            "SKIP: literal CON source cannot exist on Windows; reserved-name transfer scenario skipped"
        );
    }
}
