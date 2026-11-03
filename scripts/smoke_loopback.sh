#!/usr/bin/env bash
# Real two-process loopback smoke for ak-cli (Windows Git Bash friendly).
#
# What one run proves, end to end, with two independent ak-cli processes on
# 127.0.0.1 (never touching any real user file -- all payload bytes come from
# /dev/urandom into a throwaway directory):
#   1. A keygen + `pair add` gate-keeps `receive` (paired-peers-only policy);
#   2. `receive --once` in the background announces its bound address;
#   3. B `send --multistream` pushes TWO roots (nested tree + empty dir +
#      a standalone file root) in one job over loopback QUIC;
#   4. every received file is `cmp`-verified byte for byte;
#   5. `pair list` shows A<->B pairing (explicit + trust-on-first-use) and
#      `jobs list` shows a completed checkpoint on BOTH sides;
#   6. (optional section, on by default; --skip-probe drops it) a local
#      signal-server + TWO `probe` processes exchange candidates, punch and
#      finish a pinned QUIC handshake (same coordination+data planes a real
#      two-machine run would use).
#
# Usage:
#   bash scripts/smoke_loopback.sh [OPTIONS]
# Options:
#   --binary <path>   ak-cli binary (default: <repo>/target/debug/ak-cli.exe,
#                     then target/release/ak-cli.exe, then `ak-cli` on PATH)
#   --port <n>        UDP port for the receive leg (default 17777; the probe
#                     section reuses port+1/+2 for signaling/STUN)
#   --skip-probe      skip the optional probe section (fast path)
#   --keep            keep the temporary workspace (printed at the end)
#   -h | --help       this text
#
# Exit code 0 = every assertion passed. On failure the background processes
# are killed and the log directory is KEPT and printed for inspection.
# Tooling: only Git Bash coreutils (bash, grep, sed, cmp, head, find, mktemp).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
PORT=17777
SKIP_PROBE=0
KEEP=0
AK_BIN=""

usage() { sed -n '2,30p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              AK_BIN=$2; shift 2 ;;
    --port)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              PORT=$2; shift 2 ;;
    --skip-probe) SKIP_PROBE=1; shift ;;
    --keep)  KEEP=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
case "$PORT" in
  ''|*[!0-9]*) echo "--port must be a number" >&2; exit 2 ;;
esac
if [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65535 ]; then
  echo "--port out of range" >&2; exit 2
fi
SIG_TCP=$((PORT + 1))   # signal-server TCP (probe STUN = tcp port + 1)
SIG_STUN=$((PORT + 2))

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
step() { printf '[smoke] step %s\n' "$*"; }
note() { printf '[smoke] %s\n' "$*"; }
fail() {
  # Called from `|| fail ...` chains where the ERR trap never fires, so the
  # keep-logs-on-failure flag must be set here, not only in the trap.
  FAILED=1
  printf '[smoke] FAIL: %s\n' "$*" >&2
  exit 1
}
assert_contains() { # file needle description
  grep -q -- "$2" "$1" || fail "expected \"$2\" in $(basename "$1"): $3"
}

# Workspace + background process bookkeeping.
WORK=""
LOGS=""
RECV_PID=""
SIG_PID=""
PROBE_A_PID=""
PROBE_B_PID=""
FAILED=0

on_err() {
  FAILED=1
  printf '[smoke] FAILED at %s:%s (command: %s)\n' "${BASH_SOURCE[0]}" "$1" "$2" >&2
}
trap 'on_err "$LINENO" "${BASH_COMMAND:-?}"' ERR

kill_bg() { # pid-var content
  [ -n "$1" ] || return 0
  kill "$1" 2>/dev/null || true
}

cleanup() {
  kill_bg "$RECV_PID"
  kill_bg "$PROBE_A_PID"
  kill_bg "$PROBE_B_PID"
  kill_bg "$SIG_PID"
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    if [ "$FAILED" -eq 1 ]; then
      printf '[smoke] logs kept for inspection: %s\n' "$LOGS" >&2
      printf '[smoke] workspace kept: %s\n' "$WORK" >&2
    elif [ "$KEEP" -eq 1 ]; then
      printf '[smoke] workspace kept (--keep): %s\n' "$WORK"
    else
      rm -rf "$WORK"
    fi
  fi
}
trap cleanup EXIT
trap 'printf "[smoke] interrupted\n" >&2; exit 130' INT TERM

# ---------------------------------------------------------------------------
# Step 1: locate the binary
# ---------------------------------------------------------------------------
step "1/8 locate ak-cli binary"
if [ -z "$AK_BIN" ]; then
  for candidate in "$ROOT/target/debug/ak-cli.exe" \
                   "$ROOT/target/release/ak-cli.exe"; do
    if [ -x "$candidate" ]; then AK_BIN=$candidate; break; fi
  done
fi
if [ -z "$AK_BIN" ]; then
  AK_BIN=$(command -v ak-cli || command -v ak-cli.exe || true)
fi
[ -n "$AK_BIN" ] || fail "no ak-cli binary found (build with: cargo build -p ak-cli, or pass --binary <path>)"
[ -x "$AK_BIN" ] || fail "binary not executable: $AK_BIN"
VERSION=$("$AK_BIN" --version 2>&1)
note "binary: $AK_BIN"
note "version: $VERSION"

# ---------------------------------------------------------------------------
# Step 2: throwaway workspace + random payload (never real user files)
# ---------------------------------------------------------------------------
step "2/8 build temp workspace + random data"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-smoke.XXXXXXXX")
LOGS="$WORK/logs"
mkdir -p "$WORK/idA" "$WORK/idB" "$WORK/recv" \
         "$WORK/src/root1/nested/deep" "$WORK/src/root1/emptydir" "$LOGS"

# Random payload: one nested tree (multi-chunk file included) + an empty
# directory + a standalone second root file. Sizes are fixed so the
# assertions stay deterministic; the bytes differ on every run.
head -c 98304  /dev/urandom > "$WORK/src/root1/file-a.bin"          # 96 KiB
head -c 196608 /dev/urandom > "$WORK/src/root1/nested/file-b.bin"   # 192 KiB
head -c 1237   /dev/urandom > "$WORK/src/root1/nested/deep/file-c.bin" # small
head -c 49152  /dev/urandom > "$WORK/src/second-root.bin"           # 48 KiB
TOTAL_SRC=$(find "$WORK/src" -type f | wc -l | tr -d ' ')
note "workspace: $WORK"
note "generated $TOTAL_SRC random files (~332 KiB), nested dirs + an empty dir"

# ---------------------------------------------------------------------------
# Step 3: identities + pairing (A admits B into its peer book)
# ---------------------------------------------------------------------------
step "3/8 A keygen / B keygen / A admits B (pair add)"
"$AK_BIN" --id-dir "$WORK/idA" keygen          > "$LOGS/keygenA.log" 2>&1
"$AK_BIN" --id-dir "$WORK/idB" keygen          > "$LOGS/keygenB.log" 2>&1
FP_A=$(sed -n 's/^fingerprint (full): //p' "$LOGS/keygenA.log" | head -1)
FP_B=$(sed -n 's/^fingerprint (full): //p' "$LOGS/keygenB.log" | head -1)
DID_A=$(sed -n 's/^device id: //p' "$LOGS/keygenA.log" | head -1)
DID_B=$(sed -n 's/^device id: //p' "$LOGS/keygenB.log" | head -1)
[ ${#FP_A} -eq 64 ] && [ ${#FP_B} -eq 64 ] || fail "keygen fingerprints missing"
[ ${#DID_A} -eq 64 ] && [ ${#DID_B} -eq 64 ] || fail "keygen device ids missing"
note "A fingerprint: $FP_A"
note "B fingerprint: $FP_B"

"$AK_BIN" --id-dir "$WORK/idA" pair add "$FP_B" --name smoke-b \
  > "$LOGS/pair-add.log" 2>&1
assert_contains "$LOGS/pair-add.log" "paired smoke-b (" "pair add announces the new peer"
note "A paired B explicitly; B will trust A on first use after a good send"

# ---------------------------------------------------------------------------
# Step 4: background receive (capture the bound port)
# ---------------------------------------------------------------------------
step "4/8 A receive --once in the background (port $PORT)"
"$AK_BIN" --id-dir "$WORK/idA" receive --once --out "$WORK/recv" \
  --port "$PORT" --timeout 60 > "$LOGS/receive.log" 2>&1 &
RECV_PID=$!
ADDR=""
for _ in $(seq 1 80); do
  ADDR=$(sed -n 's/^address: //p' "$LOGS/receive.log" | head -1)
  [ -n "$ADDR" ] && break
  kill -0 "$RECV_PID" 2>/dev/null || break
  sleep 0.25
done
[ -n "$ADDR" ] || fail "receiver never printed its address (see $LOGS/receive.log)"
RECV_PORT=${ADDR##*:}
case "$RECV_PORT" in ''|*[!0-9]*) fail "cannot parse port from address: $ADDR";; esac
assert_contains "$LOGS/receive.log" \
  "accept policy: paired peers only (1 in " "receive admits paired peers only"
note "receiver bound $ADDR (waiting for one sender)"

# ---------------------------------------------------------------------------
# Step 5: B sends two roots in one job, multistream on
# ---------------------------------------------------------------------------
step "5/8 B send --multistream (two roots -> 127.0.0.1:$RECV_PORT)"
"$AK_BIN" --id-dir "$WORK/idB" send \
  "$WORK/src/root1" "$WORK/src/second-root.bin" \
  --to "127.0.0.1:$RECV_PORT" --fp "$FP_A" --multistream --timeout 60 \
  > "$LOGS/send.log" 2>&1
assert_contains "$LOGS/send.log" "sending 2 path(s) to peer " "send announces both roots"
SEND_JOB=$(sed -n 's/^job \([0-9a-f]\{32\}\) complete:.*/\1/p' "$LOGS/send.log" | head -1)
[ ${#SEND_JOB} -eq 32 ] || fail "send did not complete a job (see $LOGS/send.log)"
assert_contains "$LOGS/send.log" "roots: root1, second-root.bin" "both root names in the report"
note "send job $SEND_JOB complete"

# receive --once exits by itself after the single job; collect its status.
for _ in $(seq 1 120); do
  kill -0 "$RECV_PID" 2>/dev/null || break
  sleep 0.5
done
if kill -0 "$RECV_PID" 2>/dev/null; then
  fail "receiver still running 60s after the send finished"
fi
set +e
wait "$RECV_PID"
RECV_RC=$?
set -e
[ "$RECV_RC" -eq 0 ] || fail "receive exited $RECV_RC (see $LOGS/receive.log)"
RECV_JOB=$(sed -n 's/^job \([0-9a-f]\{32\}\) complete:.*/\1/p' "$LOGS/receive.log" | head -1)
[ "$RECV_JOB" = "$SEND_JOB" ] \
  || fail "receiver job id ($RECV_JOB) differs from sender job id ($SEND_JOB)"
note "receive job $RECV_JOB complete (same job id on both sides)"

# ---------------------------------------------------------------------------
# Step 6: byte-for-byte verification
# ---------------------------------------------------------------------------
step "6/8 cmp every file byte-for-byte (+ empty dir survived)"
cmp "$WORK/src/root1/file-a.bin"              "$WORK/recv/root1/file-a.bin"
cmp "$WORK/src/root1/nested/file-b.bin"       "$WORK/recv/root1/nested/file-b.bin"
cmp "$WORK/src/root1/nested/deep/file-c.bin"  "$WORK/recv/root1/nested/deep/file-c.bin"
cmp "$WORK/src/second-root.bin"               "$WORK/recv/second-root.bin/second-root.bin"
[ -d "$WORK/recv/root1/emptydir" ] || fail "empty directory root1/emptydir was not recreated"
TOTAL_RECV=$(find "$WORK/recv" -type f | wc -l | tr -d ' ')
[ "$TOTAL_RECV" -eq "$TOTAL_SRC" ] \
  || fail "file count mismatch: sent $TOTAL_SRC, received $TOTAL_RECV"
note "4/4 files identical (cmp), empty dir present, $TOTAL_RECV files on disk"

# ---------------------------------------------------------------------------
# Step 7: pair list / jobs list output assertions
# ---------------------------------------------------------------------------
step "7/8 pair list + jobs list assertions (both sides)"
"$AK_BIN" --id-dir "$WORK/idA" pair list > "$LOGS/pair-list-A.log" 2>&1
"$AK_BIN" --id-dir "$WORK/idB" pair list > "$LOGS/pair-list-B.log" 2>&1
assert_contains "$LOGS/pair-list-A.log" "1 paired peer(s) in " "A book has exactly one peer"
assert_contains "$LOGS/pair-list-A.log" "fingerprint (full): $FP_B" "A book carries B's full fingerprint"
assert_contains "$LOGS/pair-list-B.log" "1 paired peer(s) in " "B book gained the TOFU entry"
assert_contains "$LOGS/pair-list-B.log" "fingerprint (full): $FP_A" "B book carries A's full fingerprint"

"$AK_BIN" --id-dir "$WORK/idA" jobs list > "$LOGS/jobs-A.log" 2>&1
"$AK_BIN" --id-dir "$WORK/idB" jobs list > "$LOGS/jobs-B.log" 2>&1
assert_contains "$LOGS/jobs-A.log" "1 job checkpoint(s) in " "A has one checkpoint"
assert_contains "$LOGS/jobs-B.log" "1 job checkpoint(s) in " "B has one checkpoint"
assert_contains "$LOGS/jobs-A.log" "$SEND_JOB receive completed " "A checkpoint: receive + completed"
assert_contains "$LOGS/jobs-B.log" "$SEND_JOB send completed "    "B checkpoint: send + completed"
note "pair books symmetric (A explicit, B TOFU); both checkpoints completed"

# ---------------------------------------------------------------------------
# Step 8: probe section (signaling + STUN + hole punch + pinned QUIC)
# ---------------------------------------------------------------------------
if [ "$SKIP_PROBE" -eq 1 ]; then
  step "8/8 probe section skipped (--skip-probe)"
  note "transfer smoke passed; coordination-plane probe not exercised"
else
  step "8/8 probe: signal-server + two probe processes"
  # `probe --peer` wants the peer's DEVICE ID (the `device id:` line), not
  # the fingerprint -- the server keys online devices by public key.
  "$AK_BIN" --id-dir "$WORK/idA" signal-server \
    --tcp-port "$SIG_TCP" --stun-port "$SIG_STUN" > "$LOGS/signal.log" 2>&1 &
  SIG_PID=$!
  SIG_LINE=""
  for _ in $(seq 1 40); do
    SIG_LINE=$(sed -n 's/^signaling on //p' "$LOGS/signal.log" | head -1)
    [ -n "$SIG_LINE" ] && break
    kill -0 "$SIG_PID" 2>/dev/null || break
    sleep 0.25
  done
  [ -n "$SIG_LINE" ] || fail "signal-server did not start (see $LOGS/signal.log)"
  note "signal-server up: $SIG_LINE"

  # Sequencing matters: A must REGISTER before B connects toward it. A's own
  # connect_peer toward the not-yet-online B answers with a harmless
  # "not online" error which A absorbs while looping; B's later connect
  # opens the session for both.
  "$AK_BIN" --id-dir "$WORK/idA" probe --server "127.0.0.1:$SIG_TCP" \
    --peer "$DID_B" > "$LOGS/probeA.log" 2>&1 &
  PROBE_A_PID=$!
  for _ in $(seq 1 80); do
    grep -q '^registered ' "$LOGS/probeA.log" 2>/dev/null && break
    kill -0 "$PROBE_A_PID" 2>/dev/null || break
    sleep 0.25
  done
  grep -q '^registered ' "$LOGS/probeA.log" 2>/dev/null \
    || fail "probe A never registered (see $LOGS/probeA.log)"
  "$AK_BIN" --id-dir "$WORK/idB" probe --server "127.0.0.1:$SIG_TCP" \
    --peer "$DID_A" > "$LOGS/probeB.log" 2>&1 &
  PROBE_B_PID=$!
  note "probe A registered; probe B launched (session opens for both)"

  # Both probes exit on their own (handshake done or bounded deadline).
  for _ in $(seq 1 240); do
    A_DEAD=0; B_DEAD=0
    kill -0 "$PROBE_A_PID" 2>/dev/null || A_DEAD=1
    kill -0 "$PROBE_B_PID" 2>/dev/null || B_DEAD=1
    [ "$A_DEAD" -eq 1 ] && [ "$B_DEAD" -eq 1 ] && break
    sleep 0.5
  done
  if kill -0 "$PROBE_A_PID" 2>/dev/null || kill -0 "$PROBE_B_PID" 2>/dev/null; then
    fail "probes still running after 120s (see $LOGS/probeA.log / $LOGS/probeB.log)"
  fi
  set +e
  wait "$PROBE_A_PID"; PROBE_A_RC=$?
  wait "$PROBE_B_PID"; PROBE_B_RC=$?
  set -e
  [ "$PROBE_A_RC" -eq 0 ] || fail "probe A exited $PROBE_A_RC (see $LOGS/probeA.log)"
  [ "$PROBE_B_RC" -eq 0 ] || fail "probe B exited $PROBE_B_RC (see $LOGS/probeB.log)"
  assert_contains "$LOGS/probeA.log" "QUIC handshake OK" "probe A reached the pinned handshake"
  assert_contains "$LOGS/probeB.log" "QUIC handshake OK" "probe B reached the pinned handshake"
  A_HS=$(grep 'QUIC handshake OK' "$LOGS/probeA.log" | head -1)
  B_HS=$(grep 'QUIC handshake OK' "$LOGS/probeB.log" | head -1)
  note "A: $A_HS"
  note "B: $B_HS"
  kill_bg "$SIG_PID"
  wait "$SIG_PID" 2>/dev/null || true
  note "coordination plane verified (signaling + STUN + punch + QUIC)"
fi

note "PASS: real two-process loopback smoke completed (port $PORT)"
exit 0
