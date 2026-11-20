//! Per-file commit cost on the file system this runs on (batch Q review,
//! 2026-11-20). Not a test of AK-sending code: it times the raw operations
//! the receiver's verify phase does for each file, so the next change to
//! that phase can be chosen from numbers instead of guesses.
//!
//! Why: Q-N2 runs up to eight files' finish() at once. On the ext4 CI
//! runner that cut the verify phase of the 65,537-file run to about a
//! third; on the Windows runner only by about a quarter, which says the
//! main cost there does not overlap. The first Windows run of this
//! benchmark found flushing an already closed file cheap there, and
//! creating, writing and flushing on one handle expensive and serial; the
//! last two measurements compare finish()'s order (flush, link, next
//! file) with the same work in two passes (every flush, then every link).
//!
//! Each measurement runs over `FILES` one-byte files in a fresh directory,
//! on one thread and on `THREADS` threads, and prints one line:
//! `commit-cost: <operation> threads=<n>: <total> total, <per file> per file`.
//! Nothing is asserted beyond the operations succeeding; no path is printed.
//!
//! `cargo test --locked --release -p ak-itests --test bench_commit_cost -- --ignored --nocapture --test-threads=1`

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

const FILES: usize = 2000;
const THREADS: usize = 8;

/// `count` paths `f00000<suffix>`, `f00001<suffix>`… under `dir` (created).
fn paths(dir: &Path, count: usize, suffix: &str) -> Vec<PathBuf> {
    fs::create_dir_all(dir).unwrap();
    (0..count)
        .map(|i| dir.join(format!("f{i:05}{suffix}")))
        .collect()
}

/// Run `op` once per path on `threads` threads; the wall time of the lot.
fn run(paths: &[PathBuf], threads: usize, op: impl Fn(usize, &Path) + Sync) -> Duration {
    let next = AtomicUsize::new(0);
    let start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= paths.len() {
                    break;
                }
                op(i, &paths[i]);
            });
        }
    });
    start.elapsed()
}

fn report(operation: &str, threads: usize, files: usize, wall: Duration) {
    let per_file = wall.as_secs_f64() * 1e6 / files as f64;
    println!(
        "commit-cost: {operation} threads={threads}: {:.1} ms total, {per_file:.0} us per file",
        wall.as_secs_f64() * 1e3
    );
}

#[cfg(windows)]
fn write_through_options() -> Option<OpenOptions> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_WRITE_THROUGH);
    Some(options)
}

#[cfg(target_os = "linux")]
fn write_through_options() -> Option<OpenOptions> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_DSYNC: i32 = 0o10000;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(O_DSYNC);
    Some(options)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn write_through_options() -> Option<OpenOptions> {
    None
}

#[test]
#[ignore = "file-system benchmark; release only, prints per-file costs"]
fn per_file_commit_cost() {
    let base = tempfile::tempdir().unwrap();
    let mut round = 0;
    let mut fresh = |suffix: &str| {
        round += 1;
        paths(&base.path().join(format!("r{round:02}")), FILES, suffix)
    };

    for threads in [1, THREADS] {
        // Create, write one byte, close: what the chunk phase leaves behind.
        let files = fresh(".akpart");
        let wall = run(&files, threads, |i, path| {
            File::create(path).unwrap().write_all(&[i as u8]).unwrap();
        });
        report("create+write+close", threads, FILES, wall);

        // finish()'s first steps on such a file: open, read it whole.
        let wall = run(&files, threads, |_, path| {
            let mut bytes = Vec::new();
            File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
        });
        report("open+read", threads, FILES, wall);

        // The flush of data that is still in the cache (sync_all, as
        // finish() does it on its own handle).
        let wall = run(&files, threads, |_, path| {
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .sync_all()
                .unwrap();
        });
        report("open+sync_all (dirty)", threads, FILES, wall);

        // The same flush once nothing is dirty any more.
        let wall = run(&files, threads, |_, path| {
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .sync_all()
                .unwrap();
        });
        report("open+sync_all (clean)", threads, FILES, wall);

        // finish() sets the modification time on the open handle's path.
        let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let wall = run(&files, threads, |_, path| {
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        });
        report("open+set_modified", threads, FILES, wall);

        // Claiming the final name: link, then drop the part name.
        let wall = run(&files, threads, |_, path| {
            let target = path.with_extension("");
            fs::hard_link(path, &target).unwrap();
            fs::remove_file(path).unwrap();
        });
        report("hard_link+remove_file", threads, FILES, wall);

        // A plain rename, for comparison.
        let finals: Vec<PathBuf> = files.iter().map(|path| path.with_extension("")).collect();
        let wall = run(&finals, threads, |_, path| {
            fs::rename(path, path.with_extension("renamed")).unwrap();
        });
        report("rename", threads, FILES, wall);

        // Write-through instead of a later flush (FILE_FLAG_WRITE_THROUGH on
        // Windows, O_DSYNC on Linux): create, write one byte, close.
        if let Some(options) = write_through_options() {
            let files = fresh(".wt");
            let wall = run(&files, threads, |i, path| {
                options.open(path).unwrap().write_all(&[i as u8]).unwrap();
            });
            report("create+write-through+close", threads, FILES, wall);
        }

        // Write and flush on the same handle, then close.
        let files = fresh(".akpart");
        let wall = run(&files, threads, |i, path| {
            let mut file = File::create(path).unwrap();
            file.write_all(&[i as u8]).unwrap();
            file.sync_all().unwrap();
        });
        report("create+write+sync_all+close", threads, FILES, wall);

        // finish()'s order, one file after another: flush the part, then
        // claim the final name, before the next file's flush. This is what
        // the verify phase does now.
        let files = fresh(".akpart");
        write_closed(&files);
        let wall = run(&files, threads, |_, path| {
            sync_existing(path);
            claim(path);
        });
        report(
            "per file: sync_all, then hard_link+remove_file",
            threads,
            FILES,
            wall,
        );

        // The same work in two passes: every file's flush, then every
        // file's final name. Each file's data is still flushed before its
        // final name appears.
        let files = fresh(".akpart");
        write_closed(&files);
        let start = Instant::now();
        run(&files, threads, |_, path| sync_existing(path));
        run(&files, threads, |_, path| claim(path));
        report(
            "all sync_all, then all hard_link+remove_file",
            threads,
            FILES,
            start.elapsed(),
        );
    }
}

/// One-byte files created, written through the cache and closed (not timed).
fn write_closed(paths: &[PathBuf]) {
    run(paths, THREADS, |i, path| {
        File::create(path).unwrap().write_all(&[i as u8]).unwrap();
    });
}

/// Open an existing file for writing and flush it, as finish() does.
fn sync_existing(path: &Path) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .sync_all()
        .unwrap();
}

/// Claim the final name the way finish() does: link it, drop the part name.
fn claim(part: &Path) {
    fs::hard_link(part, part.with_extension("")).unwrap();
    fs::remove_file(part).unwrap();
}
