//! Shared harness for the ak-itests scenario binaries (TEST_PLAN mapping).
//!
//! Every `tests/*.rs` file is compiled as its own binary and includes this
//! module via `mod common;`, so not every helper is used by every binary —
//! dead_code is allowed here on purpose. All helpers are test-only plumbing:
//! key generation, spec construction, a loopback send+receive run, and
//! byte-exact tree comparison helpers.
//!
//! Layer note: everything here drives real QUIC loopback transfers through
//! `ak_engine::send` / `ak_engine::receive_with_addr` — L2 shape (真实传输集成
//! on 127.0.0.1), never a real-machine / cross-network claim.
#![allow(dead_code)]
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ak_core::handle::ConflictPolicy;
use ak_engine::{receive_with_addr, send, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::DeviceKeyPair;

/// Part suffix used by ak-storage for in-progress receive files.
pub const PART_SUFFIX: &str = ".akpart";

/// Progress sink that ignores everything (mirrors the ak-engine test setup).
pub fn noop(_: Progress) {}

/// Deterministic LCG (Knuth constants) so scenario trees are reproducible
/// without pulling in a random-number dependency.
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Lcg(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    /// Uniform-ish value in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 33) as u8
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }
}

/// Create parent directories as needed and write exact bytes.
pub fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

/// A walked directory tree: relative `/`-separated dir names plus the exact
/// bytes of every file. Symlinks never occur in these test trees.
#[derive(Default, Debug)]
pub struct Tree {
    pub dirs: BTreeSet<String>,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Tree {
    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|bytes| bytes.len() as u64).sum()
    }

    pub fn entry_count(&self) -> u64 {
        (self.dirs.len() + self.files.len()) as u64
    }
}

/// Recursively snapshot a directory (root itself excluded, children relative).
pub fn snapshot_tree(root: &Path) -> Tree {
    let mut tree = Tree::default();
    visit(root, "", &mut tree);
    tree
}

fn visit(dir: &Path, prefix: &str, tree: &mut Tree) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.path().is_dir() {
            tree.dirs.insert(rel.clone());
            visit(&entry.path(), &rel, tree);
        } else {
            tree.files.insert(rel, fs::read(entry.path()).unwrap());
        }
    }
}

/// Fail when any `.akpart` residue is left behind (a committed transfer must
/// leave none).
pub fn assert_no_part_files(root: &Path) {
    let tree = snapshot_tree(root);
    let parts: Vec<&String> = tree
        .files
        .keys()
        .filter(|name| name.ends_with(PART_SUFFIX))
        .collect();
    assert!(parts.is_empty(), "leftover part files: {parts:?}");
}

/// Assert two trees have identical directory structure and byte-identical
/// files, reporting the first few divergences instead of dumping whole trees.
pub fn assert_trees_identical(src_root: &Path, dst_root: &Path) {
    let src = snapshot_tree(src_root);
    let dst = snapshot_tree(dst_root);

    let missing_dirs: Vec<&String> = src.dirs.difference(&dst.dirs).collect();
    let extra_dirs: Vec<&String> = dst.dirs.difference(&src.dirs).collect();
    assert!(
        missing_dirs.is_empty() && extra_dirs.is_empty(),
        "directory structure differs\nmissing: {missing_dirs:?}\nextra: {extra_dirs:?}"
    );

    let missing: Vec<&String> = src
        .files
        .keys()
        .filter(|name| !dst.files.contains_key(*name))
        .collect();
    let extra: Vec<&String> = dst
        .files
        .keys()
        .filter(|name| !src.files.contains_key(*name))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "file sets differ\nmissing: {missing:?}\nextra: {extra:?}"
    );

    let wrong_size: Vec<(&String, usize, usize)> = src
        .files
        .iter()
        .filter_map(|(name, bytes)| {
            let got = &dst.files[name];
            (got.len() != bytes.len()).then_some((name, bytes.len(), got.len()))
        })
        .collect();
    assert!(
        wrong_size.is_empty(),
        "file sizes differ (name, expected, got): {wrong_size:?}"
    );

    let mut wrong_bytes: Vec<&String> = Vec::new();
    for (name, bytes) in &src.files {
        if &dst.files[name] != bytes {
            wrong_bytes.push(name);
            if wrong_bytes.len() >= 10 {
                break;
            }
        }
    }
    assert!(
        wrong_bytes.is_empty(),
        "file contents differ for: {wrong_bytes:?}"
    );
}

/// Everything a loopback run produced: both reports plus the progress events
/// collected from both sides (sender and receiver interleaved).
pub struct TransferOutcome {
    pub send: Report,
    pub recv: Report,
    pub events: Vec<Progress>,
}

/// Like [`TransferOutcome`] but carrying each side's raw result so callers can
/// match on protocol-level errors (used to pin known engine defects).
pub struct RawOutcome {
    pub send: Result<Report, ak_engine::EngineError>,
    pub recv: Result<Report, ak_engine::EngineError>,
    pub events: Vec<Progress>,
}

/// Run one full loopback transfer: fresh Ed25519 identities for both sides,
/// port-0 binds, isolated per-run checkpoint dirs, real QUIC on 127.0.0.1.
/// Returns both sides' raw results; never panics on transfer errors (only on
/// task panics or a `budget_secs` overrun).
pub async fn try_run_transfer_full(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
    budget_secs: u64,
) -> RawOutcome {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    // Fresh checkpoint dirs per run so parallel tests never share job state.
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        receiver_key.fingerprint(),
        sender_bundle,
    );
    send_spec.device_name = "ak-itests-sender".into();
    send_spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).expect("chunk_kb must be nonzero");
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.device_name = "ak-itests-receiver".into();
    // Port 0 and loopback only: never touch the default 0.0.0.0:7100 (tests
    // run in parallel and would collide).
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.conflict = conflict;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();

    let events: Arc<Mutex<Vec<Progress>>> = Arc::new(Mutex::new(Vec::new()));
    let callback = {
        let events = Arc::clone(&events);
        Arc::new(move |progress: Progress| {
            if let Ok(mut log) = events.lock() {
                log.push(progress);
            }
        })
    };

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let recv_callback = Arc::clone(&callback);
    let receiver =
        tokio::spawn(async move { receive_with_addr(recv_spec, &*recv_callback, addr_tx).await });
    send_spec.peer = addr_rx.await.expect("receiver must report its address");
    let send_callback = Arc::clone(&callback);
    let sender = tokio::spawn(async move { send(send_spec, &*send_callback).await });

    let joined = tokio::time::timeout(Duration::from_secs(budget_secs), async {
        (receiver.await, sender.await)
    })
    .await
    .unwrap_or_else(|_| panic!("loopback transfer did not finish within {budget_secs}s"));
    let (recv_result, send_result) = joined;
    let send = send_result.expect("sender task must not panic");
    let recv = recv_result.expect("receiver task must not panic");

    let events = events
        .lock()
        .map(|mut log| std::mem::take(&mut *log))
        .unwrap_or_default();
    RawOutcome { send, recv, events }
}

/// Panicking variant of [`try_run_transfer_full`]: both sides must succeed.
pub async fn run_transfer_full(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
    budget_secs: u64,
) -> TransferOutcome {
    let raw = try_run_transfer_full(src, dst, chunk_kb, conflict, budget_secs).await;
    let send = raw
        .send
        .unwrap_or_else(|error| panic!("send failed: {error:?}"));
    let recv = raw
        .recv
        .unwrap_or_else(|error| panic!("receive failed: {error:?}"));
    TransferOutcome {
        send,
        recv,
        events: raw.events,
    }
}

/// Default-shaped run: ConflictPolicy::Fail, 180s budget.
pub async fn run_transfer(src: &Path, dst: &Path, chunk_kb: u32) -> TransferOutcome {
    run_transfer_full(src, dst, chunk_kb, ConflictPolicy::Fail, 180).await
}

/// Run with an explicit conflict policy, 180s budget.
pub async fn run_transfer_conflict(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
) -> TransferOutcome {
    run_transfer_full(src, dst, chunk_kb, conflict, 180).await
}

/// Read one file's exact bytes (assertion helper).
pub fn read_file(root: &Path, rel: &str) -> Vec<u8> {
    let path: PathBuf = root.join(rel);
    fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}
