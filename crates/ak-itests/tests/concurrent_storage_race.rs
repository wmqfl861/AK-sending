//! Concurrent storage race observables (wave 24, review 2026-11-05
//! "最值得做的改进" #2 / finding P2 "同目标并发提交"): what the destination
//! tree looks like when two tasks race for the SAME target through the
//! real `ak-storage` staged prepare/finish path, and how the staging
//! namespace defends itself against local tampering.
//!
//! Every scenario drives `LocalSaveTarget::prepare_file_staged` /
//! `finish` from two threads released together by a `Barrier` after both
//! parts are fully verified (the exact adjudication window the review
//! asked to be exercised concurrently, not sequentially), and asserts the
//! OBSERVABLE destination state: which files exist, their exact bytes,
//! and that nothing escaped the receive root.
//!
//! * **Same target, same content** (Fail + Rename mix, repeated rounds):
//!   exactly one commit and one content reuse — both finishers succeed,
//!   one final file, no ` (2)` duplicate, both staging parts consumed.
//! * **Same target, different content under Fail**: one commit, one
//!   `AlreadyExists` failure; the loser's verified part survives in ITS
//!   OWN staging tree; the winner's bytes are intact at the final name.
//! * **Same target, different content under Rename**: one commit plus one
//!   ` (2)` rename landing, both payloads byte-intact.
//! * **Staging root replaced by an external link**: `discard_staging`
//!   refuses (fail closed) and the external tree is untouched; a legit
//!   sibling task still cleans normally.
//! * **rel_path naming the reserved `.ak-staging` namespace**: prepare is
//!   rejected with zero disk writes (leading component, nested component,
//!   and the NTFS case-insensitive variant).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use ak_core::handle::{ConflictPolicy, FileSink, SourceHandle};
use ak_core::manifest::FileEntry;
use ak_storage::{discard_staging, staging_path, LocalSaveTarget, LocalSource, STAGING_DIR};

const TASK_A: [u8; 16] = [0xAA, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0xBB];
const TASK_B: [u8; 16] = [0xCC, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0xDD];

fn cs(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

/// A real scanned source (manifest + chunk bytes pre-extracted so worker
/// threads never share the `Send`-only source handle).
struct Scanned {
    entry: FileEntry,
    chunks: Vec<Vec<u8>>,
}

fn scan_payload(content: &[u8], chunk: u32) -> Scanned {
    let tmp = tempfile::tempdir().expect("src tempdir");
    let path = tmp.path().join("payload.bin");
    fs::write(&path, content).expect("write source");
    let source = LocalSource::open(&path, cs(chunk)).expect("scan source");
    let entry = source.manifest().entries[0].clone();
    let plan = ak_core::chunk::ChunkPlan::new(entry.size, cs(chunk));
    let chunks = (0..entry.chunk_digests.len() as u64)
        .map(|i| {
            let range = plan.range(i).expect("in-range chunk");
            let len = usize::try_from(range.end - range.start).expect("chunk len fits usize");
            let mut buf = vec![0u8; len];
            let read = source.read_chunk(0, i, &mut buf).expect("read chunk");
            assert_eq!(read, len);
            buf
        })
        .collect();
    // The extracted bytes are the transfer payload now; the source file
    // was only needed for the real scan.
    drop(source);
    drop(tmp);
    Scanned { entry, chunks }
}

/// One racer: prepare (staged) → write every verified chunk → wait on the
/// barrier → finish. The join result is the finish outcome.
fn race_finish(
    target: Arc<LocalSaveTarget>,
    task: [u8; 16],
    scanned: Scanned,
    conflict: ConflictPolicy,
    chunk_size: NonZeroU32,
    barrier: Arc<Barrier>,
) -> std::thread::JoinHandle<std::io::Result<()>> {
    std::thread::spawn(move || {
        let mut sink = target.prepare_file_staged(task, &scanned.entry, conflict, chunk_size)?;
        for (index, bytes) in scanned.chunks.iter().enumerate() {
            sink.write_chunk(index as u64, bytes)?;
        }
        // Both parts are now fully verified on disk; release together so
        // the two atomic claims genuinely race.
        barrier.wait();
        sink.finish()
    })
}

/// Recursive name → size map of everything under `root` (staging tree
/// included — it is part of the observable).
fn tree_sizes(root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for child in fs::read_dir(&dir).expect("read dir") {
            let child = child.expect("dir entry");
            let name = child.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = child.path();
            let meta = fs::symlink_metadata(&path).expect("stat");
            if meta.is_dir() {
                stack.push((path, rel));
            } else {
                out.insert(rel, meta.len());
            }
        }
    }
    out
}

fn assert_no_part_files(root: &Path) {
    let tree = tree_sizes(root);
    let parts: Vec<&String> = tree
        .keys()
        .filter(|name| {
            name.ends_with(".akpart") || name.split('/').any(|c| c.starts_with(".akpart-"))
        })
        .collect();
    assert!(parts.is_empty(), "no part residue anywhere: {parts:?}");
}

// ---- 1: same target, same content ---------------------------------------------

/// Two threads race prepare + finish for the same final name with the
/// SAME verified content (one Fail, one Rename — the policy the loser
/// holds must not matter for identical bytes): exactly one commit, one
/// content reuse. Repeated rounds so the atomic-claim `AlreadyExists`
/// re-adjudication branch gets real chances to fire.
#[test]
fn concurrent_same_target_same_content_is_one_commit_one_reuse() {
    // Several chunks so the hash/sync phases straddle the barrier window.
    let content: Vec<u8> = (0..128 * 1024u32).map(|i| (i % 251) as u8).collect();
    let chunk = 16 * 1024u32;
    for round in 0..5 {
        let dst = tempfile::tempdir().expect("dst tempdir");
        let target = Arc::new(LocalSaveTarget::open(dst.path()).expect("open target"));
        let a = scan_payload(&content, chunk);
        let b = scan_payload(&content, chunk);
        let barrier = Arc::new(Barrier::new(2));
        let handle_a = race_finish(
            Arc::clone(&target),
            TASK_A,
            a,
            ConflictPolicy::Fail,
            cs(chunk),
            Arc::clone(&barrier),
        );
        let handle_b = race_finish(
            target,
            TASK_B,
            b,
            ConflictPolicy::Rename,
            cs(chunk),
            barrier,
        );
        handle_a.join().expect("thread a").expect("commit or reuse");
        handle_b.join().expect("thread b").expect("reuse or commit");

        // Observable destination: exactly one final with the exact bytes.
        assert_eq!(
            fs::read(dst.path().join("payload.bin")).expect("final exists"),
            content,
            "round {round}: the one commit carries the shared content"
        );
        assert!(
            !dst.path().join("payload (2).bin").exists(),
            "round {round}: identical content must not spawn a duplicate"
        );
        let names: Vec<String> = tree_sizes(dst.path()).keys().cloned().collect();
        assert_eq!(
            names.iter().filter(|n| !n.starts_with(STAGING_DIR)).count(),
            1,
            "round {round}: exactly one visible final: {names:?}"
        );
        // Both staging parts were consumed by commit/reuse; the staging
        // directory levels themselves may remain for task cleanup.
        let parts: Vec<String> = names
            .into_iter()
            .filter(|n| n.ends_with(".akpart"))
            .collect();
        assert!(parts.is_empty(), "round {round}: parts consumed: {parts:?}");
    }
}

// ---- 2: same target, different content, Fail ----------------------------------

/// Different verified contents racing for one final name under Fail:
/// exactly one commit and one `AlreadyExists` failure. The winner's bytes
/// are intact; the loser's fully verified part stays in ITS OWN staging
/// tree (policy retry material), and nothing lands beside the final name.
#[test]
fn concurrent_different_content_fail_is_one_commit_one_failure() {
    let content_a: Vec<u8> = (0..96 * 1024u32).map(|i| (i % 251) as u8).collect();
    let content_b: Vec<u8> = (0..96 * 1024u32).map(|i| (i % 249) as u8).collect();
    let chunk = 16 * 1024u32;
    let dst = tempfile::tempdir().expect("dst tempdir");
    let target = Arc::new(LocalSaveTarget::open(dst.path()).expect("open target"));
    let a = scan_payload(&content_a, chunk);
    let b = scan_payload(&content_b, chunk);
    let barrier = Arc::new(Barrier::new(2));
    let handle_a = race_finish(
        Arc::clone(&target),
        TASK_A,
        a,
        ConflictPolicy::Fail,
        cs(chunk),
        Arc::clone(&barrier),
    );
    let handle_b = race_finish(target, TASK_B, b, ConflictPolicy::Fail, cs(chunk), barrier);
    let result_a = handle_a.join().expect("thread a");
    let result_b = handle_b.join().expect("thread b");

    // Exactly one success and one AlreadyExists, in either order.
    let results = [result_a, result_b];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|r| {
            r.as_ref()
                .is_err_and(|e| e.kind() == ErrorKind::AlreadyExists)
        })
        .count();
    assert_eq!(ok, 1, "exactly one commit: {results:?}");
    assert_eq!(conflicts, 1, "exactly one Fail refusal: {results:?}");

    // The final holds ONE of the two payloads whole; no mixture, no "(2)".
    let final_bytes = fs::read(dst.path().join("payload.bin")).expect("final exists");
    assert!(
        final_bytes == content_a || final_bytes == content_b,
        "the final carries one racer's exact payload"
    );
    assert!(!dst.path().join("payload (2).bin").exists());

    // The loser's part survived in its own staging tree for a policy
    // retry; the visible destination tree has no loose part.
    let a_part = staging_path(dst.path(), TASK_A).join("payload.bin.akpart");
    let b_part = staging_path(dst.path(), TASK_B).join("payload.bin.akpart");
    let surviving: Vec<&PathBuf> = [&a_part, &b_part]
        .iter()
        .filter(|p| p.exists())
        .copied()
        .collect();
    assert_eq!(
        surviving.len(),
        1,
        "exactly the loser's part stays: {surviving:?}"
    );
    let visible: Vec<String> = tree_sizes(dst.path())
        .keys()
        .filter(|n| !n.starts_with(STAGING_DIR))
        .cloned()
        .collect();
    assert_eq!(visible, vec!["payload.bin".to_owned()], "clean destination");
}

// ---- 3: same target, different content, Rename ---------------------------------

/// The same race under Rename: one commit plus one ` (2)` landing — two
/// successes, both payloads byte-intact under their distinct names, and
/// both parts consumed.
#[test]
fn concurrent_different_content_rename_is_one_commit_one_landing() {
    let content_a: Vec<u8> = (0..96 * 1024u32).map(|i| (i % 251) as u8).collect();
    let content_b: Vec<u8> = (0..96 * 1024u32).map(|i| (i % 249) as u8).collect();
    let chunk = 16 * 1024u32;
    let dst = tempfile::tempdir().expect("dst tempdir");
    let target = Arc::new(LocalSaveTarget::open(dst.path()).expect("open target"));
    let a = scan_payload(&content_a, chunk);
    let b = scan_payload(&content_b, chunk);
    let barrier = Arc::new(Barrier::new(2));
    let handle_a = race_finish(
        Arc::clone(&target),
        TASK_A,
        a,
        ConflictPolicy::Rename,
        cs(chunk),
        Arc::clone(&barrier),
    );
    let handle_b = race_finish(
        target,
        TASK_B,
        b,
        ConflictPolicy::Rename,
        cs(chunk),
        barrier,
    );
    handle_a.join().expect("thread a").expect("a lands");
    handle_b.join().expect("thread b").expect("b lands");

    // Both payloads are whole: the base name holds one, " (2)" the other.
    let base = fs::read(dst.path().join("payload.bin")).expect("base final");
    let renamed = fs::read(dst.path().join("payload (2).bin")).expect("renamed final");
    let both = [
        (base.clone(), content_a.clone(), content_b.clone()),
        (renamed.clone(), content_b, content_a),
    ];
    for (got, option1, option2) in both {
        assert!(
            got == option1 || got == option2,
            "each landing carries one racer's exact payload"
        );
    }
    assert_ne!(base, renamed, "the two landings are distinct content");
    assert_no_part_files(dst.path());
}

// ---- 4: staging directory replaced by an external link --------------------------

/// The P1-2 cleanup attack: `<root>/.ak-staging` replaced by a directory
/// symlink into an external tree. `discard_staging` must refuse (fail
/// closed) and nothing inside the linked tree — nor any other external
/// file — may be touched; a legit sibling task still cleans normally.
/// The task-level link variant (the link one level deeper) is refused the
/// same way. On filesystems where the test cannot create symlinks the
/// assertion degrades to an explicit, counted SKIP (crate convention).
#[test]
fn discard_staging_refuses_replaced_staging_levels_and_keeps_external_trees() {
    let dst = tempfile::tempdir().expect("dst tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");

    // Variant 1: the staging ROOT is the link.
    let task_hex = TASK_A
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let external_root = outside.path().join(STAGING_DIR);
    let external_task = external_root.join(&task_hex);
    fs::create_dir_all(&external_task).expect("external task tree");
    fs::write(external_task.join("f.bin.akpart"), b"external-data").expect("victim part");
    let outside_victim = outside.path().join("victim.txt");
    fs::write(&outside_victim, b"keep-me").expect("outside victim");
    if !make_symlink(&external_root, &dst.path().join(STAGING_DIR)) {
        return; // honest SKIP, printed and counted by make_symlink
    }
    let err = discard_staging(dst.path(), TASK_A).expect_err("refused");
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "fail closed: {err}");
    assert!(
        external_task.join("f.bin.akpart").exists(),
        "nothing inside the linked tree was touched"
    );
    assert_eq!(
        fs::read(&outside_victim).expect("victim readable"),
        b"keep-me",
        "external victim untouched"
    );
    // The planted link itself is still just a link at the root.
    assert!(fs::symlink_metadata(dst.path().join(STAGING_DIR))
        .expect("link stat")
        .file_type()
        .is_symlink());

    // Variant 2: the staging root is real, but THIS task's tree is a link
    // into the external tree (the `top` component check).
    fs::remove_file(dst.path().join(STAGING_DIR))
        .or_else(|_| fs::remove_dir(dst.path().join(STAGING_DIR)))
        .expect("drop the planted root link");
    fs::create_dir_all(dst.path().join(STAGING_DIR)).expect("real staging root");
    let external_b = outside.path().join("task-b-tree");
    fs::create_dir_all(&external_b).expect("external task-b tree");
    fs::write(external_b.join("g.bin.akpart"), b"also-external").expect("victim part b");
    if !make_symlink(&external_b, &staging_path(dst.path(), TASK_B)) {
        return; // honest SKIP
    }
    let err = discard_staging(dst.path(), TASK_B).expect_err("refused");
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "fail closed: {err}");
    assert!(
        external_b.join("g.bin.akpart").exists(),
        "the linked task tree was not deleted through"
    );

    // A legit sibling task next to the refusals still cleans normally.
    fs::remove_file(staging_path(dst.path(), TASK_B))
        .or_else(|_| fs::remove_dir(staging_path(dst.path(), TASK_B)))
        .expect("drop the task link");
    let real = staging_path(dst.path(), TASK_A);
    fs::create_dir_all(real.join("d")).expect("real tree");
    fs::write(real.join("d/x.akpart"), b"y").expect("real part");
    discard_staging(dst.path(), TASK_A).expect("legit cleanup");
    assert!(!real.exists());
}

/// Symlink plumbing with the crate's honest-skip convention (Windows needs
/// the symlink privilege; every skip prints a counted SKIP line).
fn make_symlink(src: &Path, dst: &Path) -> bool {
    match symlink_once(src, dst) {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "SKIP: cannot create symlink at {} ({err}); this assertion did NOT run",
                dst.display()
            );
            false
        }
    }
}

#[cfg(windows)]
fn symlink_once(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

#[cfg(unix)]
fn symlink_once(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

// ---- 5: reserved staging namespace in rel_path ----------------------------------

/// A manifest entry whose relative path names the reserved `.ak-staging`
/// namespace (leading component, nested component, and the NTFS
/// case-insensitive spelling) is refused by BOTH prepare variants before
/// anything touches the disk: the receive root keeps exactly zero entries.
#[test]
fn prepare_rejects_rel_paths_naming_the_staging_namespace_with_zero_writes() {
    let dst = tempfile::tempdir().expect("dst tempdir");
    let target = LocalSaveTarget::open(dst.path()).expect("open target");
    // A digest-consistent entry (single chunk) built by the real scanner,
    // then re-pointed at the hostile paths — the exact shape a malicious
    // sender would ship in its manifest.
    let probe = scan_payload(b"staging-namespace-probe", 16);
    let mut entry = probe.entry.clone();
    let task_hex = TASK_A
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let hostile = [
        format!(".ak-staging/{task_hex}/x"),
        "docs/.ak-staging/x".to_string(),
        ".AK-STAGING/x".to_string(),
    ];
    for rel in hostile {
        entry.rel_path = PathBuf::from(rel.clone());
        let staged = target.prepare_file_staged(TASK_A, &entry, ConflictPolicy::Fail, cs(16));
        assert!(staged.is_err(), "{rel}: staged prepare must refuse");
        let plain = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(16));
        assert!(plain.is_err(), "{rel}: plain prepare must refuse");
    }

    // Zero writes: nothing under the root at all.
    let tree = tree_sizes(dst.path());
    assert!(
        tree.is_empty(),
        "a refused namespace path must leave zero disk writes: {tree:?}"
    );
}
