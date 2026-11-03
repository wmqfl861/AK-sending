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
use std::time::{Duration, Instant};

use ak_core::handle::{default_chunk_size, ConflictPolicy};
use ak_engine::{receive_with_addr, send, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::{DeviceKeyPair, Fingerprint};

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
/// match on protocol-level errors (used to pin known engine defects), plus the
/// address the receiver actually bound (its family follows `recv_bind`, which
/// is how the T03 IPv6 run proves the data plane really rode ::1).
pub struct RawOutcome {
    pub send: Result<Report, ak_engine::EngineError>,
    pub recv: Result<Report, ak_engine::EngineError>,
    pub events: Vec<Progress>,
    pub recv_local_addr: SocketAddr,
}

/// Address-family and stream-shape knobs for [`try_run_transfer_opts`].
/// Defaults reproduce the historical shape used by every other test in this
/// crate: sender on the unspecified v4 bind (0.0.0.0:0, `SendSpec::new`'s
/// default), receiver explicitly on 127.0.0.1:0, single-stream, window of 8.
pub struct TransferOpts {
    /// Sender's UDP bind (`SendSpec.bind`; port 0 = ephemeral). NOT the peer
    /// address — that is always the receiver's reported address.
    pub send_bind: SocketAddr,
    /// Receiver's UDP bind (`ReceiveSpec.bind`); its family decides the whole
    /// data path's address family.
    pub recv_bind: SocketAddr,
    /// Sender-side opt-in; effective only when the peer also advertises it.
    pub multistream: bool,
    /// Maximum unacknowledged chunks in flight (sender side).
    pub in_flight_window: u32,
    /// Explicit chunk size override for BOTH endpoints (`SendSpec::chunk_size`
    /// and `ReceiveSpec::chunk_size`), so benches can sweep the chunk size
    /// without threading it through the legacy `chunk_kb` argument. `None`
    /// (the default) keeps the historical behavior: the `chunk_kb` argument
    /// of `try_run_transfer_opts` / `try_run_transfer_parties` decides; when
    /// that is 0 as well, the specs keep the engine default (1 MiB).
    pub chunk_size: Option<NonZeroU32>,
}

impl Default for TransferOpts {
    fn default() -> Self {
        Self {
            send_bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            recv_bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            multistream: false,
            in_flight_window: 8,
            chunk_size: None,
        }
    }
}

/// Receiver-side admission callback shape (mirrors `ReceiveSpec::authorize`).
pub type AuthorizeFn = Box<dyn Fn(&Fingerprint) -> bool + Send + Sync>;

/// Explicit transfer parties for scenario tests that must control WHO is
/// talking: pre-generated device identities (so a test can pre-pair, pre-pin,
/// or play an impostor) plus an optional receiver-side authorize callback
/// (pairing-book admission semantics).
pub struct TransferParties {
    pub sender_key: DeviceKeyPair,
    pub receiver_key: DeviceKeyPair,
    /// When set, replaces the receiver's default accept-any authorize
    /// callback (e.g. pairing-book membership).
    pub authorize: Option<AuthorizeFn>,
}

impl TransferParties {
    /// Fresh independent identities and an accept-any receiver: the
    /// historical shape every opts-based run used.
    pub fn fresh() -> Self {
        Self {
            sender_key: DeviceKeyPair::generate(),
            receiver_key: DeviceKeyPair::generate(),
            authorize: None,
        }
    }
}

/// Run one full loopback transfer with explicit [`TransferOpts`]: fresh
/// Ed25519 identities for both sides, port-0 binds, isolated per-run
/// checkpoint dirs, real QUIC. Returns both sides' raw results; never panics
/// on transfer errors (only on task panics or a `budget_secs` overrun).
pub async fn try_run_transfer_opts(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
    budget_secs: u64,
    opts: TransferOpts,
) -> RawOutcome {
    try_run_transfer_parties(
        src,
        dst,
        chunk_kb,
        conflict,
        budget_secs,
        opts,
        TransferParties::fresh(),
    )
    .await
}

/// [`try_run_transfer_opts`] with explicit [`TransferParties`]: the device
/// identities (and optionally the receiver's authorize callback) are the
/// caller's, so a scenario can pre-pair, pre-pin, or impersonate one side.
pub async fn try_run_transfer_parties(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
    budget_secs: u64,
    opts: TransferOpts,
    parties: TransferParties,
) -> RawOutcome {
    let sender_bundle = ak_identity::tls::make_self_signed(&parties.sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&parties.receiver_key);
    // Fresh checkpoint dirs per run so parallel tests never share job state.
    let send_ckpt = tempfile::tempdir().unwrap();
    let recv_ckpt = tempfile::tempdir().unwrap();

    let mut send_spec = SendSpec::new(
        src.to_path_buf(),
        // Peer placeholder; replaced by the receiver's reported address below.
        SocketAddr::from(([127, 0, 0, 1], 0)),
        parties.receiver_key.fingerprint(),
        sender_bundle,
    );
    send_spec.device_name = "ak-itests-sender".into();
    send_spec.bind = opts.send_bind;
    // Chunk precedence: explicit opts override > legacy chunk_kb argument
    // (chunk_kb = 0 means "none given") > engine default (1 MiB). Historical
    // callers pass a nonzero chunk_kb and no override, so they are unchanged.
    send_spec.chunk_size = opts
        .chunk_size
        .or_else(|| NonZeroU32::new(chunk_kb.checked_mul(1024).unwrap_or(0)))
        .unwrap_or_else(default_chunk_size);
    send_spec.checkpoint_dir = send_ckpt.path().to_path_buf();
    send_spec.multistream = opts.multistream;
    send_spec.in_flight_window = opts.in_flight_window;

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.device_name = "ak-itests-receiver".into();
    // Port 0 and loopback only: never touch the default 0.0.0.0:7100 (tests
    // run in parallel and would collide).
    recv_spec.bind = opts.recv_bind;
    recv_spec.chunk_size = send_spec.chunk_size;
    recv_spec.conflict = conflict;
    recv_spec.checkpoint_dir = recv_ckpt.path().to_path_buf();
    if let Some(authorize) = parties.authorize {
        recv_spec.authorize = authorize;
    }

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
    let recv_local_addr = addr_rx.await.expect("receiver must report its address");
    send_spec.peer = recv_local_addr;
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
    RawOutcome {
        send,
        recv,
        events,
        recv_local_addr,
    }
}

/// Historical default-shaped raw run: 127.0.0.1 both sides, single stream.
pub async fn try_run_transfer_full(
    src: &Path,
    dst: &Path,
    chunk_kb: u32,
    conflict: ConflictPolicy,
    budget_secs: u64,
) -> RawOutcome {
    try_run_transfer_opts(
        src,
        dst,
        chunk_kb,
        conflict,
        budget_secs,
        TransferOpts::default(),
    )
    .await
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

// ---------------------------------------------------------------------------
// Bench plumbing shared by the informational benches (bench_throughput,
// bench_chunk_sweep): a timed one-pass runner that checks correctness only
// and never asserts speed.
// ---------------------------------------------------------------------------

/// Throughput in MiB/s for a byte count over an elapsed duration
/// (zero-division guarded; informational only).
pub fn mib_per_sec(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(f64::EPSILON)
}

/// Timings of one bench pass: wall clock plus both engine-reported durations.
pub struct BenchPass {
    pub wall: Duration,
    pub send: Duration,
    pub recv: Duration,
}

/// One timed loopback pass shaped by `opts` (chunk override included).
///
/// Correctness checks only — file/byte counts, matching job ids, a fresh
/// (non-resumed) transfer, byte-identical trees, no `.akpart` residue —
/// never a speed assertion. `expected_files`/`expected_bytes` describe the
/// caller's payload, `shape` is a human note for the printed line (e.g.
/// "16 x 16 MiB"). Destination dir, device keys, and checkpoint dirs are
/// fresh per call (inside `try_run_transfer_opts`), so successive passes
/// never share transfer state.
pub async fn run_bench_pass(
    src: &Path,
    label: &str,
    shape: &str,
    expected_files: u64,
    expected_bytes: u64,
    budget_secs: u64,
    opts: TransferOpts,
) -> BenchPass {
    let dst = tempfile::tempdir().expect("dst tempdir");
    let started = Instant::now();
    // chunk_kb = 0: no legacy chunk argument; the chunk comes from
    // `opts.chunk_size` or, when that is None too, the engine default.
    let raw =
        try_run_transfer_opts(src, dst.path(), 0, ConflictPolicy::Fail, budget_secs, opts).await;
    let wall = started.elapsed();

    let send_report = raw
        .send
        .unwrap_or_else(|e| panic!("[bench {label}] send failed: {e:?}"));
    let recv_report = raw
        .recv
        .unwrap_or_else(|e| panic!("[bench {label}] receive failed: {e:?}"));

    // Correctness only — never speed.
    assert_eq!(
        send_report.files, expected_files,
        "[bench {label}] file count"
    );
    assert_eq!(
        send_report.bytes, expected_bytes,
        "[bench {label}] byte count"
    );
    assert_eq!(
        recv_report.bytes, expected_bytes,
        "[bench {label}] receiver byte count"
    );
    assert_eq!(send_report.job_id, recv_report.job_id);
    assert_eq!(
        send_report.resumed_chunks, 0,
        "[bench {label}] fresh full transfer"
    );
    assert_trees_identical(src, dst.path());
    assert_no_part_files(dst.path());

    let wall_tp = mib_per_sec(expected_bytes, wall);
    let send_tp = mib_per_sec(expected_bytes, send_report.duration);
    let recv_tp = mib_per_sec(expected_bytes, recv_report.duration);
    println!(
        "[bench {label:>13}] {expected_bytes} bytes ({shape}): wall {wall:.2?} ({wall_tp:.1} MiB/s) | engine send {:.2?} ({send_tp:.1} MiB/s) | engine recv {:.2?} ({recv_tp:.1} MiB/s)",
        send_report.duration, recv_report.duration,
    );
    BenchPass {
        wall,
        send: send_report.duration,
        recv: recv_report.duration,
    }
}
