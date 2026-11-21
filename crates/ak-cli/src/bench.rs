//! `bench`: in-process loopback throughput measurement.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// bench: in-process loopback throughput measurement (evidence generator)
// ---------------------------------------------------------------------------

/// Fixed PRNG seed: identical bench runs generate identical bytes, so
/// numbers are comparable across runs and machines.
const BENCH_SEED: u64 = 0x00C0_FFEE_1234_5678;
/// Printed with every bench result so nobody mistakes loopback for a NIC.
pub(super) const BENCH_DISCLAIMER: &str =
    "loopback throughput; real-NIC measurement needs two machines";
/// Idle budget for the bench engine runs; generous because a bench must
/// measure throughput, not trigger timeouts on a busy machine.
const BENCH_IDLE: Duration = Duration::from_secs(120);

/// Deterministic xorshift64 filler (Marsaglia). Fast and dependency-free;
/// NOT cryptographic -- it only needs pseudo-random bytes to exercise the
/// hashing, framing and QUIC paths.
pub(super) struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    /// A zero state would freeze the generator; map it to a fixed nonzero
    /// seed so every input produces a working stream.
    pub(super) fn new(seed: u64) -> Self {
        Xorshift64 {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Fill `buf` with little-endian words of the generator stream; a
    /// trailing partial word takes the low bytes.
    pub(super) fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

/// Split `total` bytes into exactly `files` sizes: even quotient, remainder
/// spread one byte at a time over the first files. Always sums to `total`.
pub(super) fn bench_file_sizes(total: u64, files: u32) -> Vec<u64> {
    let base = total / u64::from(files);
    let mut rem = total % u64::from(files);
    (0..files)
        .map(|_| {
            if rem > 0 {
                rem -= 1;
                base + 1
            } else {
                base
            }
        })
        .collect()
}

/// Best-effort scoped temp directory for bench artifacts (source tree,
/// per-round destinations and checkpoints). The tempfile crate is a
/// dev-dependency of this crate and cannot be used here, hence the manual
/// guard.
struct BenchTempDir(PathBuf);

impl BenchTempDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "ak-cli-bench-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("cannot create bench dir {}", path.display()))?;
        Ok(BenchTempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for BenchTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stream `size` bytes of PRNG output into one file in 1 MiB steps (the
/// default 256 MiB must never be materialized in memory as a whole).
fn write_bench_file(path: &Path, size: u64, prng: &mut Xorshift64) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let step = remaining.min(buf.len() as u64) as usize;
        prng.fill(&mut buf[..step]);
        file.write_all(&buf[..step])?;
        remaining -= step as u64;
    }
    Ok(())
}

/// Generate the flat bench source tree (bench-0000.bin, ...). Per-file seeds
/// are distinct so files do not share prefixes.
fn write_bench_source(src: &Path, sizes: &[u64]) -> Result<()> {
    std::fs::create_dir_all(src).with_context(|| format!("cannot create {}", src.display()))?;
    for (index, size) in sizes.iter().enumerate() {
        let path = src.join(format!("bench-{index:04}.bin"));
        let seed = BENCH_SEED
            ^ (index as u64)
                .wrapping_add(1)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut prng = Xorshift64::new(seed);
        write_bench_file(&path, *size, &mut prng)?;
    }
    Ok(())
}

fn bench_quiet_progress(_: Progress) {}

/// One completed bench round. `duration` is the engine's own Report.duration
/// (send side), so the measurement covers exactly what the engine measured.
#[derive(Debug, Clone)]
pub(super) struct BenchRound {
    pub(super) mode: &'static str,
    pub(super) duration: Duration,
    pub(super) bytes: u64,
}

/// Throughput of a round in decimal MB/s and binary GiB/s.
fn throughput(bytes: u64, duration: Duration) -> (f64, f64) {
    let secs = duration.as_secs_f64().max(1e-9);
    (
        bytes as f64 / 1_000_000.0 / secs,
        bytes as f64 / 1_073_741_824.0 / secs,
    )
}

/// Run one full sender->receiver transfer of `src` through the real engine
/// over loopback QUIC (fresh key pair, destination and checkpoint dirs per
/// round so nothing resumes from a previous round). Both sides run inside
/// this process; the data path is still the full QUIC + TLS + engine stack.
async fn bench_round(
    src: &Path,
    dst: &Path,
    checkpoints: &Path,
    chunk_kb: u32,
    multistream: bool,
) -> Result<BenchRound> {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();
    let chunk = NonZeroU32::new(chunk_kb.saturating_mul(1024))
        .context("chunk size must be at least 1 KiB")?;

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = chunk;
    recv_spec.checkpoint_dir = checkpoints.to_path_buf();
    recv_spec.device_name = "ak-bench".into();
    recv_spec.idle_timeout = BENCH_IDLE;
    // Loopback still pins both identities: only our own sender may connect.
    let sender_fp = sender_key.fingerprint();
    recv_spec.authorize = Box::new(move |fp: &Fingerprint| *fp == sender_fp);

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move {
        ak_engine::receive_with_addr(recv_spec, &bench_quiet_progress, addr_tx).await
    });
    let addr = addr_rx
        .await
        .context("bench receiver failed to bind its loopback port")?;

    let mut send_spec = SendSpec::new(src.to_path_buf(), addr, receiver_fp, sender_bundle);
    send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    send_spec.chunk_size = chunk;
    send_spec.checkpoint_dir = checkpoints.to_path_buf();
    send_spec.device_name = "ak-bench".into();
    send_spec.idle_timeout = BENCH_IDLE;
    send_spec.multistream = multistream;
    let report = ak_engine::send(send_spec, &bench_quiet_progress)
        .await
        .context("bench send failed")?;
    let recv_report = receiver
        .await
        .context("bench receiver task failed")?
        .context("bench receive failed")?;

    // A Report already implies whole-digest verification on the receive
    // side; the byte totals must also agree between both ends.
    if report.bytes != recv_report.bytes {
        bail!(
            "bench integrity mismatch: sent {} bytes but received {}",
            report.bytes,
            recv_report.bytes
        );
    }
    Ok(BenchRound {
        mode: if multistream { "multistream" } else { "single" },
        duration: report.duration,
        bytes: report.bytes,
    })
}

/// Machine-readable bench result (reuses the JSON string escaper).
pub(super) fn bench_json(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    rounds: &[BenchRound],
    ratio: Option<f64>,
) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"bytes\": {bytes},\n"));
    out.push_str(&format!("  \"files\": {files},\n"));
    out.push_str(&format!("  \"chunk_kb\": {chunk_kb},\n"));
    out.push_str("  \"rounds\": [\n");
    for (index, round) in rounds.iter().enumerate() {
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        let comma = if index + 1 < rounds.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"mode\": \"{}\", \"duration_ms\": {:.3}, \"bytes\": {}, \
             \"mb_per_s\": {:.3}, \"gib_per_s\": {:.6}}}{}\n",
            json_escape(round.mode),
            round.duration.as_secs_f64() * 1000.0,
            round.bytes,
            mb_per_s,
            gib_per_s,
            comma
        ));
    }
    out.push_str("  ],\n");
    match ratio {
        Some(ratio) => out.push_str(&format!("  \"ratio\": {ratio:.4},\n")),
        None => out.push_str("  \"ratio\": null,\n"),
    }
    out.push_str(&format!(
        "  \"note\": \"{}\"\n}}",
        json_escape(BENCH_DISCLAIMER)
    ));
    out
}

pub(super) async fn bench_command(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    multistream: bool,
    once: bool,
    json: bool,
) -> Result<()> {
    // Default: both rounds plus a comparison. --once runs exactly one round
    // (single-stream, or multistream when --multistream is also given).
    let run_single = !once || !multistream;
    let run_multi = !once || multistream;
    let mut plan: Vec<(&'static str, bool)> = Vec::new();
    if run_single {
        plan.push(("single-stream", false));
    }
    if run_multi {
        plan.push(("multistream", true));
    }

    let dir = BenchTempDir::new()?;
    let src = dir.path().join("src");
    let sizes = bench_file_sizes(bytes, files);
    eprintln!(
        "ak-cli bench: {} across {} files, chunk {chunk_kb} KiB; \
         generating deterministic source into {}",
        human_bytes(bytes),
        sizes.len(),
        src.display()
    );
    write_bench_source(&src, &sizes)?;

    let total = plan.len();
    let mut rounds: Vec<BenchRound> = Vec::new();
    for (index, (label, round_multistream)) in plan.iter().enumerate() {
        let dst = dir.path().join(format!("dst-{index}"));
        let checkpoints = dir.path().join(format!("ckpt-{index}"));
        std::fs::create_dir_all(&dst)
            .with_context(|| format!("cannot create {}", dst.display()))?;
        std::fs::create_dir_all(&checkpoints)
            .with_context(|| format!("cannot create {}", checkpoints.display()))?;
        eprintln!("round {}/{total} ({label}): running...", index + 1);
        let round = bench_round(&src, &dst, &checkpoints, chunk_kb, *round_multistream).await?;
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        eprintln!(
            "round {}/{total} ({label}): {} in {:.3} s -> {:.2} MB/s ({:.4} GiB/s)",
            index + 1,
            human_bytes(round.bytes),
            round.duration.as_secs_f64(),
            mb_per_s,
            gib_per_s
        );
        rounds.push(round);
    }

    // Compare only when both modes ran (rounds[0]=single, rounds[1]=multi).
    let ratio = if rounds.len() == 2 {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        Some(multi_mb / single_mb.max(1e-9))
    } else {
        None
    };

    if json {
        println!("{}", bench_json(bytes, files, chunk_kb, &rounds, ratio));
        return Ok(());
    }
    if let Some(ratio) = ratio {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        println!(
            "comparison: multistream {ratio:.2}x single-stream \
             ({multi_mb:.2} vs {single_mb:.2} MB/s)"
        );
    }
    println!("note: {BENCH_DISCLAIMER}");
    Ok(())
}
