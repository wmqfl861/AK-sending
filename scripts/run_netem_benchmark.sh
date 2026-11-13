#!/usr/bin/env bash
# G8 evidence runner: Linux loopback + artificial delay, NOT real-WAN/device
# validation. Unlike the standalone Rust test, a SKIP here is a CI failure.
# Usage (runner user, NOT sudo): bash scripts/run_netem_benchmark.sh [--output-dir NEW_DIR]
# Builds only bench_netem, then uses passwordless sudo in an owned, fresh netns.
# No tc mutation ever runs in the host namespace. No product knobs are changed.

select_executable() {
  python3 -I - "$1" "$2" <<'PY'
import json
import os
from pathlib import Path
import sys

try:
    root = Path(sys.argv[2]).resolve()
    messages = [json.loads(line) for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines() if line.strip()]
    finished = [m for m in messages if m.get("reason") == "build-finished"]
    if len(finished) != 1 or finished[0].get("success") is not True:
        raise ValueError("Cargo did not report exactly one successful build-finished")
    candidates = []
    for message in messages:
        target, profile = message.get("target", {}), message.get("profile", {})
        if (message.get("reason") == "compiler-artifact"
                and target.get("name") == "bench_netem"
                and target.get("kind") == ["test"]
                and Path(message.get("manifest_path", "")).resolve() == root / "crates/ak-itests/Cargo.toml"
                and Path(target.get("src_path", "")).resolve() == root / "crates/ak-itests/tests/bench_netem.rs"
                and profile.get("test") is True
                and profile.get("debug_assertions") is False
                and profile.get("opt_level") in ("1", "2", "3", "s", "z")
                and message.get("executable")):
            candidates.append(message["executable"])
    if len(candidates) != 1:
        raise ValueError(f"expected exactly one release bench_netem test artifact, got {len(candidates)}")
    exe = Path(candidates[0])
    if not exe.is_absolute() or not exe.is_file() or not os.access(exe, os.X_OK):
        raise ValueError("Cargo test executable is not an existing absolute executable file")
    if "\n" in str(exe) or "\r" in str(exe):
        raise ValueError("invalid executable path")
    print(exe.resolve().as_posix())
except (OSError, ValueError, TypeError, AttributeError) as error:
    sys.exit(f"artifact selection failed: {error}")
PY
}

validate_measurement() {
  python3 -I - "$1" <<'PY'
import math
from pathlib import Path
import re
import sys

try:
    text = Path(sys.argv[1]).read_text(encoding="utf-8")
    if re.search(r"\bSKIP\b", text, re.IGNORECASE):
        raise ValueError("benchmark printed SKIP; NOT-MEASURED")
    if "[netem summary] 268435456 bytes (8 x 32 MiB), single stream, engine-default chunk, in-flight window 32 (QUIC windows: CR-P2-04 8 MiB send/stream/connection; A/B arm re-runs delay 100ms with quinn default 1.25 MiB windows via AK_EXP_QUIC_WINDOW_KIB=1280):" not in text:
        raise ValueError("missing 256 MiB netem summary (with the A/B arm note)")
    if "[netem bench] cleanup verified: dev lo carries no netem qdisc" not in text:
        raise ValueError("benchmark cleanup was not verified")
    if not re.search(r"^test result: ok\. 1 passed; 0 failed; 0 ignored;", text, re.MULTILINE):
        raise ValueError("expected one passing, non-skipped ignored test")
    number = r"[0-9]+(?:\.[0-9]+)?"
    duration = rf"({number})(ns|us|µs|μs|ms|s)"
    rows = []
    for label, delay, rtt in (("no-netem", 0, 0), ("delay 50ms", 50, 100), ("delay 100ms", 100, 200)):
        lines = re.findall(rf"^{re.escape(label)}\s+.*$", text, re.MULTILINE)
        if len(lines) != 1:
            raise ValueError(f"expected one numeric sample for {label}, got {len(lines)}")
        match = re.fullmatch(rf"{re.escape(label)}\s+{duration}\s+({number})\s+({number}|-|inf)\s+({number})x\s*", lines[0])
        if not match:
            raise ValueError(f"invalid numeric sample or units for {label}")
        wall, unit, throughput, rtts, ratio = match.groups()
        if not all(math.isfinite(float(v)) for v in (wall, throughput, ratio)) or float(wall) <= 0 or float(throughput) <= 0:
            raise ValueError(f"non-positive/non-finite sample for {label}")
        rows.append(f"| {label} | {delay} | {rtt} | {wall}{unit} | {throughput} | {rtts} | {ratio}x |")
    # A/B arm row (I1, 2026-11-13 review 5.2): same columns and shaping as
    # the delay-100ms tier, quinn default 1.25 MiB windows via the tls.rs
    # experimental override. Informational -- parsed and reported, never
    # asserted against a floor (the collapse tripwire is default-arm only).
    ab_label = "A/B 1280KiB"
    lines = re.findall(rf"^{re.escape(ab_label)}\s+.*$", text, re.MULTILINE)
    if len(lines) != 1:
        raise ValueError(f"expected one numeric sample for {ab_label}, got {len(lines)}")
    match = re.fullmatch(rf"{re.escape(ab_label)}\s+{duration}\s+({number})\s+({number}|-|inf)\s+({number})x\s*", lines[0])
    if not match:
        raise ValueError(f"invalid numeric sample or units for {ab_label}")
    wall, unit, throughput, rtts, ratio = match.groups()
    if not all(math.isfinite(float(v)) for v in (wall, throughput, ratio)) or float(wall) <= 0 or float(throughput) <= 0:
        raise ValueError(f"non-positive/non-finite sample for {ab_label}")
    rows.append(f"| {ab_label} (quinn 1.25 MiB defaults) | 100 | 200 | {wall}{unit} | {throughput} | {rtts} | {ratio}x |")
    # Per-tier quinn stats lines (I1): exactly one per tier, numeric, and
    # the sender side must show a live connection (cwnd/packets/bytes > 0)
    # plus receiver rx datagrams -- all-zero stats would mean the sink
    # plumbing fired on a dead snapshot instead of the transfer.
    stats_fields = (r"cwnd=([0-9]+)B congestion_events=([0-9]+) lost_packets=([0-9]+) "
                    r"sent_packets=([0-9]+) tx_bytes=([0-9]+) rx_datagrams=([0-9]+)")
    for label in ("baseline", "delay 50ms", "delay 100ms", "A/B 1280KiB"):
        pattern = rf"^\[netem bench\] {re.escape(label)} quinn stats: sender {stats_fields} \| receiver {stats_fields}\s*$"
        found = [line for line in text.splitlines() if re.fullmatch(pattern, line)]
        if len(found) != 1:
            raise ValueError(f"expected one quinn stats line for {label}, got {len(found)}")
        stats = re.fullmatch(pattern, found[0]).groups()
        sender_cwnd, _sender_events, _sender_lost, sender_sent, sender_tx, _sender_rx = stats[:6]
        _recv_cwnd, _recv_events, _recv_lost, _recv_sent, _recv_tx, recv_rx = stats[6:]
        if min(int(sender_cwnd), int(sender_sent), int(sender_tx), int(recv_rx)) <= 0:
            raise ValueError(f"quinn stats line for {label} shows a dead snapshot (zero cwnd/packets/bytes)")
    # A/B activation proof: tls.rs announces the experimental override on
    # stderr when (and only when) a transport config is built with the
    # variable set; without this line the A/B row may silently be another
    # default-window pass.
    if not re.search(r"^\[ak-net\] EXPERIMENTAL AK_EXP_QUIC_WINDOW_KIB=1280: overriding all three CR-P2-04 flow-control windows with [0-9]+ bytes \(1280 KiB\) - hidden debug knob, never a production setting$", text, re.MULTILINE):
        raise ValueError("A/B override activation line missing (AK_EXP_QUIC_WINDOW_KIB=1280 never announced)")
    ab_line = re.search(r"^\[netem A/B\] delay 100ms \(RTT ~200ms\): CR-P2-04 8 MiB windows ([0-9.]+) MiB/s \(([0-9.]+) RTTs\) vs quinn defaults 1\.25 MiB \(AK_EXP_QUIC_WINDOW_KIB=1280\) ([0-9.]+) MiB/s \(([0-9.]+) RTTs\) -> defaults/8MiB throughput ratio ([0-9.]+)x \(informational; the collapse tripwire is asserted on the default arm only\)$", text, re.MULTILINE)
    if not ab_line:
        raise ValueError("missing or malformed [netem A/B] comparison line")
    ab_8mib_tp, ab_8mib_rtts, ab_default_tp, ab_default_rtts, ab_ratio = ab_line.groups()
    if not all(math.isfinite(float(v)) and float(v) > 0 for v in (ab_8mib_tp, ab_default_tp)) or not math.isfinite(float(ab_ratio)):
        raise ValueError("non-positive/non-finite values in the [netem A/B] comparison line")
    # The Rust test owns all correctness/floor assertions; never override them.
    # The RTTs column is informational (the unshaped baseline prints a
    # literal '-': the wall/RTT ratio is undefined without shaping) --
    # finite checks only cover wall, throughput and ratio.
    print("| Shape | One-way delay (ms) | Approx. added RTT (ms) | Wall time | Throughput (MiB/s) | RTTs (informational) | vs baseline |")
    print("|---|---:|---:|---:|---:|---:|---:|")
    print("\n".join(rows))
    print()
    print(f"A/B (delay 100ms, RTT ~200ms): 8 MiB windows {ab_8mib_tp} MiB/s ({ab_8mib_rtts} RTTs) vs quinn 1.25 MiB defaults {ab_default_tp} MiB/s ({ab_default_rtts} RTTs), defaults/8MiB ratio {ab_ratio}x -- informational, no assertion.")
    print("Per-tier quinn stats lines (sender cwnd end value, congestion events, loss/packet counters, both sides) validated present for baseline, delay 50ms, delay 100ms and the A/B arm; see benchmark.log.")
except (OSError, ValueError) as error:
    sys.exit(f"measurement validation failed: {error}")
PY
}

log() { printf '[netem-runner] %s\n' "$*" | tee -a "$output_dir/runner.log"; }
fail() { reason="$2"; log "$status: $reason"; exit "$1"; }
ns_exec() { sudo -n ip netns exec "$namespace" "$@"; }

cleanup_namespace() {
  [[ "$ns_owned" == 1 ]] || return 0
  local failed=0 pids signal pid state namespaces name rest
  # Only kill/query qdiscs after proving this namespace differs from the host.
  if [[ "$ns_isolated" == 1 ]]; then
    for signal in TERM KILL; do
      if pids=$(sudo -n ip netns pids "$namespace"); then
        for pid in $pids; do
          if [[ "$pid" =~ ^[1-9][0-9]*$ ]]; then
            sudo -n kill "-$signal" "$pid" 2>/dev/null || true
          else
            failed=1
          fi
        done
        [[ -z "$pids" ]] || sleep 0.2
      else
        failed=1
      fi
    done
    if pids=$(sudo -n ip netns pids "$namespace") && [[ -z "$pids" ]]; then
      printf 'cleanup: namespace processes gone\n'
    else
      printf 'cleanup: FAILED to verify namespace processes gone\n'
      failed=1
    fi
    if state=$(ns_exec tc qdisc show dev lo); then
      if [[ "$state" == *netem* ]]; then
        ns_exec tc qdisc del dev lo root || true
      fi
      if state=$(ns_exec tc qdisc show dev lo) && [[ "$state" != *netem* ]]; then
        printf 'cleanup: lo has no netem qdisc\n'
      else
        printf 'cleanup: FAILED to verify lo qdisc removal\n'
        failed=1
      fi
    else
      printf 'cleanup: FAILED to inspect lo qdisc\n'
      failed=1
    fi
  else
    printf 'cleanup: namespace isolation unverified; refusing process/qdisc operations\n'
    failed=1
  fi
  # Attempt deletion even after process/qdisc failures, but never hide failures.
  sudo -n ip netns delete "$namespace" || failed=1
  if namespaces=$(sudo -n ip netns list); then
    while read -r name rest; do
      [[ "$name" != "$namespace" ]] || failed=1
    done <<< "$namespaces"
  else
    failed=1
  fi
  if [[ "$failed" == 0 ]]; then
    printf 'cleanup: owned namespace deleted and absence verified\n'
  else
    printf 'cleanup: FAILED; inspect this job before accepting any samples\n'
  fi
  return "$failed"
}

write_summary() {
  {
    printf '# G8 netem benchmark evidence\n\nStatus: **%s**\n\n' "$status"
    printf 'Loopback + artificial delay in a Linux namespace; not real-WAN or real-device validation.\n\n'
    printf 'Reason: %s\n\n' "$reason"
    printf -- '- Build exit code: %s\n- Build log exit code: %s\n' "$build_rc" "$build_log_rc"
    printf -- '- Benchmark exit code: %s\n- Benchmark log exit code: %s\n' "$benchmark_rc" "$benchmark_log_rc"
    printf -- '- Namespace cleanup: %s\n- Runner exit code: %s\n\n' "$cleanup_result" "$final_rc"
    if [[ -f "$output_dir/provenance.txt" ]]; then
      printf '## Provenance\n\n```text\n'
      while IFS= read -r line; do printf '%s\n' "$line"; done < "$output_dir/provenance.txt"
      printf '```\n\n'
    fi
    if [[ -s "$output_dir/metrics.md" ]]; then
      printf '## Parsed samples (accepted only when status is MEASURED)\n\n'
      while IFS= read -r line; do printf '%s\n' "$line"; done < "$output_dir/metrics.md"
    else
      printf 'No validated sample table. See benchmark.log and validation.log if present.\n'
    fi
  } > "$output_dir/summary.md"
}

finish() {
  final_rc=$?
  trap - EXIT INT TERM HUP
  set +e
  cleanup_namespace 2>&1 | tee -a "$output_dir/runner.log"
  local cleanup_codes=("${PIPESTATUS[@]}")
  if [[ "$ns_owned" == 1 ]]; then
    cleanup_result=VERIFIED
    if [[ "${cleanup_codes[0]}" != 0 || "${cleanup_codes[1]}" != 0 ]]; then
      cleanup_result=FAILED
      status=FAILED
      reason="$reason; namespace cleanup failed"
      [[ "$final_rc" != 0 ]] || final_rc=1
    fi
  fi
  # On signals, cleanup first kills the namespace's timeout/test children;
  # waiting then reaps the logging job and captures its real pipeline statuses.
  if [[ -n "$benchmark_job" ]]; then
    wait "$benchmark_job"
  fi
  if [[ -f "$output_dir/benchmark-exit-codes.txt" ]]; then
    read -r benchmark_rc benchmark_log_rc < "$output_dir/benchmark-exit-codes.txt"
  fi
  if [[ "$final_rc" != 0 && "$status" == MEASURED ]]; then
    status=FAILED
  fi
  if [[ "$final_rc" == 0 && "$status" != MEASURED ]]; then
    final_rc=1
  fi
  write_summary || final_rc=1
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    while IFS= read -r line; do printf '%s\n' "$line"; done < "$output_dir/summary.md" >> "$GITHUB_STEP_SUMMARY" || final_rc=1
  fi
  printf '[netem-runner] %s; namespace cleanup=%s; exit=%s\n' "$status" "$cleanup_result" "$final_rc"
  exit "$final_rc"
}

run_logged_benchmark() {
  set +e
  ns_exec timeout --signal=TERM --kill-after=15s 2100s "$executable" \
    --exact netem_delay_50_100ms_256mib_loopback_throughput \
    --ignored --nocapture --test-threads=1 2>&1 | tee "$output_dir/benchmark.log"
  local codes=("${PIPESTATUS[@]}")
  printf '%s %s\n' "${codes[0]}" "${codes[1]}" > "$output_dir/benchmark-exit-codes.txt"
}

main() {
  set -euo pipefail
  export LC_ALL=C CARGO_TERM_COLOR=never
  local repo_root tool revision lock_hash rust_version cargo_version kernel tc_version ip_version
  local host_netns isolated_netns state
  repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
  output_dir=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --output-dir)
        [[ $# -ge 2 && -n "$2" ]] || { printf 'Missing --output-dir value\n' >&2; return 2; }
        output_dir="$2"; shift 2 ;;
      *) printf 'Usage: bash scripts/run_netem_benchmark.sh [--output-dir NEW_DIR]\n' >&2; return 2 ;;
    esac
  done
  if [[ -z "$output_dir" ]]; then
    output_dir=$(mktemp -d "${TMPDIR:-/tmp}/ak-netem-evidence.XXXXXXXX")
  else
    # Never mix this run with an earlier success/skip log.
    mkdir -- "$output_dir"
  fi
  output_dir=$(cd -- "$output_dir" && pwd)
  status=NOT-MEASURED reason='preflight/build not completed'
  build_rc=not-run build_log_rc=not-run benchmark_rc=not-run benchmark_log_rc=not-run
  ns_owned=0 ns_isolated=0 cleanup_result=NOT-CREATED benchmark_job=""
  namespace="ak-netem-${BASHPID}-${RANDOM}-${RANDOM}"
  trap finish EXIT
  trap 'status=FAILED; reason="Interrupted (INT)"; exit 130' INT
  trap 'status=FAILED; reason="Interrupted (TERM)"; exit 143' TERM
  trap 'status=FAILED; reason="Interrupted (HUP)"; exit 129' HUP
  log 'Linux netem evidence only; local skips are not measurements'
  [[ "$(uname -s)" == Linux ]] || fail 1 'Linux with sch_netem and passwordless sudo is required'
  for tool in python3 cargo rustc git sudo ip tc timeout readlink tee; do
    command -v "$tool" >/dev/null 2>&1 || fail 1 "required tool missing: $tool"
  done
  [[ "$(id -u)" != 0 ]] || fail 1 'invoke as the runner user, not root; Cargo must never build under sudo'
  sudo -n true || fail 1 'passwordless sudo is unavailable'
  cd -- "$repo_root"
  revision=$(git rev-parse HEAD)
  if [[ -n "${GITHUB_SHA:-}" && "$GITHUB_SHA" != "$revision" ]]; then
    fail 1 'checkout revision does not match GITHUB_SHA'
  fi
  rust_version=$(rustc --version)
  cargo_version=$(cargo --version)
  lock_hash=$(python3 -I -c 'import hashlib; from pathlib import Path; print(hashlib.sha256(Path("Cargo.lock").read_bytes()).hexdigest())')
  kernel=$(uname -srvm)
  tc_version=$(tc -V)
  ip_version=$(ip -V)
  printf 'Source revision: %s\nRust: %s\nCargo: %s\nCargo.lock SHA-256: %s\nKernel: %s\ntc: %s\nip: %s\n' \
    "$revision" "$rust_version" "$cargo_version" "$lock_hash" "$kernel" "$tc_version" "$ip_version" > "$output_dir/provenance.txt"

  log 'Building only the release bench_netem test as the runner user'
  set +e
  # CR-P3-16 (review 2026-11-13-batchH §4): the tls.rs window override
  # (AK_EXP_QUIC_WINDOW_KIB, the A/B arm below) compiles only in exp-knobs
  # builds -- ak-itests' feature forwards to ak-net's gate. Without the
  # flag the override is absent and the A/B arm would silently measure the
  # default windows (the validator's activation-line check would fail).
  cargo test --locked --release -p ak-itests --test bench_netem --no-run \
    --features exp-knobs \
    --message-format=json-render-diagnostics 2> "$output_dir/build.stderr.log" | tee "$output_dir/cargo-artifacts.jsonl"
  local build_codes=("${PIPESTATUS[@]}")
  set -e
  build_rc=${build_codes[0]} build_log_rc=${build_codes[1]}
  [[ "$build_rc" == 0 ]] || fail "$build_rc" 'Cargo build failed (see build.stderr.log)'
  [[ "$build_log_rc" == 0 ]] || fail "$build_log_rc" 'Cargo artifact log capture failed'
  executable=$(select_executable "$output_dir/cargo-artifacts.jsonl" "$repo_root") || fail 1 'exact release test artifact selection failed'
  printf '%s\n' "$executable" > "$output_dir/executable.txt"

  host_netns=$(readlink /proc/self/ns/net)
  sudo -n ip netns add "$namespace" || fail 1 'fresh network namespace creation failed'
  ns_owned=1
  isolated_netns=$(ns_exec readlink /proc/self/ns/net) || fail 1 'cannot inspect namespace isolation'
  [[ -n "$isolated_netns" && "$isolated_netns" != "$host_netns" ]] || fail 1 'refusing to run in the host network namespace'
  ns_isolated=1
  sudo -n ip -n "$namespace" link set lo up || fail 1 'namespace loopback setup failed'
  # Creating an actual qdisc proves CAP_NET_ADMIN + sch_netem, even when
  # sch_netem is built into the kernel (lsmod/modprobe alone would not prove it).
  ns_exec tc qdisc add dev lo root netem delay 1ms || fail 1 'sch_netem/CAP_NET_ADMIN write probe failed'
  state=$(ns_exec tc qdisc show dev lo) || fail 1 'netem write probe could not be inspected'
  [[ "$state" == *netem* ]] || fail 1 'netem write probe was not visible'
  ns_exec tc qdisc del dev lo root netem || fail 1 'netem write probe could not be removed'
  state=$(ns_exec tc qdisc show dev lo) || fail 1 'cannot verify unshaped loopback before benchmark'
  [[ "$state" != *netem* ]] || fail 1 'netem write probe left loopback shaped'
  log 'sch_netem write/delete probe passed in isolated namespace; running exactly one ignored test'

  status=FAILED reason='benchmark did not finish'
  # Background + wait lets Bash process INT/TERM while the test is running.
  # The internal 35-minute timeout is below the workflow's outer job budget.
  run_logged_benchmark &
  benchmark_job=$!
  wait "$benchmark_job"
  benchmark_job=""
  read -r benchmark_rc benchmark_log_rc < "$output_dir/benchmark-exit-codes.txt"
  [[ "$benchmark_rc" == 0 ]] || fail "$benchmark_rc" 'benchmark failed; original test exit preserved'
  [[ "$benchmark_log_rc" == 0 ]] || fail "$benchmark_log_rc" 'benchmark log capture failed'
  if ! validate_measurement "$output_dir/benchmark.log" > "$output_dir/metrics.md" 2> "$output_dir/validation.log"; then
    status=NOT-MEASURED
    fail 1 'SKIP, missing numeric samples, or unverified benchmark cleanup (see validation.log)'
  fi
  status=MEASURED reason='three numeric samples and benchmark assertions passed; namespace cleanup checked at exit'
  # Success is provisional: EXIT cleanup can still turn it into failure.
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
