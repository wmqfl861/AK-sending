#!/usr/bin/env bash
# Cross-device smoke, HOST side: this Windows host (Git Bash) runs the
# ak-cli RECEIVER; an Android device/emulator runs the Flutter app as the
# SENDER, pointed at 10.0.2.2:<port> (the host as seen from inside the
# Android emulator).
#
# What one run proves:
#   1. the host receiver starts with a throwaway identity and prints the
#      three things the Android sender needs: address 10.0.2.2:<port>,
#      full fingerprint, pairing code;
#   2. (if adb + a device are available) the fixture tree lands on the
#      device at /sdcard/Download/ak-fixture/ for the app's send page;
#   3. after a human/integrator drives the app's send page, every fixture
#      file arrives in the host inbox and is cmp-verified byte for byte
#      (three accepted inbox layouts, see `check_layout` below);
#   4. `--self-test` exercises steps 1 + receiver startup output only and
#      exits 0 -- the host-side gate that runs WITHOUT any device.
#
# Security trade-off, stated honestly (also printed at run time): the
# Android side is a brand-new identity the host has never booked, so the
# receiver runs with `--expect any` (accepts EVERY peer). Authentication of
# the android->host direction therefore rests entirely on the ANDROID side:
# the app's send page pins the host fingerprint (--fp), and the operator
# visually cross-checks the host pairing code. The host->android direction
# is NOT authenticated this run (first-transfer convenience). To tighten a
# re-run, replace `--expect any` with `--expect <fingerprint|pairing-code>`.
#
# Usage:
#   bash scripts/smoke_android_host.sh [OPTIONS]
# Options:
#   --port <n>          UDP port the receiver binds (default 17800); the
#                       Android side targets 10.0.2.2:<port>
#   --mb <n>            random payload size in MiB (default 8) for
#                       fixture/payload.bin
#   --release           prefer target/release/ak-cli.exe and build release
#                       when absent (default: prefer debug)
#   --keep              keep the temporary workspace (printed at the end)
#   --timeout-secs <n>  how long to wait for the Android transfer
#                       (default 300)
#   --self-test         host-side gate: verify receiver startup + the
#                       address/fingerprint/pairing-code output, then clean
#                       up and exit 0 (skips the Android wait entirely)
#   --binary <path>     explicit ak-cli binary (default: target/ search,
#                       then cargo build fallback)
#   -h | --help         this text
#
# adb (optional, never required): $HOME/android-toolchain/sdk/platform-
# tools/adb.exe (override with AK_ADB=<path>). With a device attached the
# fixture is pushed to /sdcard/Download/ak-fixture/ automatically; without
# one the script prints manual instructions and still waits (same timeout).
# Extra unexpected files in the inbox (e.g. .akpart leftovers from an
# abandoned retry) are reported as warnings, not failures; fixture CONTENT
# must match byte for byte or the run FAILs.
#
# Exit code 0 = PASS (or self-test PASS). On failure the workspace is KEPT
# and printed. Tooling: Git Bash coreutils only (bash, grep, sed, cmp,
# find, mktemp, head, tr, awk).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
PORT=17800
MB=8
RELEASE=0
KEEP=0
TIMEOUT_SECS=300
SELF_TEST=0
AK_BIN=""

usage() { sed -n '2,57p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --port)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              PORT=$2; shift 2 ;;
    --mb)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              MB=$2; shift 2 ;;
    --release) RELEASE=1; shift ;;
    --keep)   KEEP=1; shift ;;
    --timeout-secs) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              TIMEOUT_SECS=$2; shift 2 ;;
    --self-test) SELF_TEST=1; shift ;;
    --binary) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              AK_BIN=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
for pair in "$PORT:--port:1:65535" "$MB:--mb:1:4096" "$TIMEOUT_SECS:--timeout-secs:5:86400"; do
  value=${pair%%:*}; rest=${pair#*:}
  name=${rest%%:*}; rest=${rest#*:}
  lo=${rest%%:*}; hi=${rest#*:}
  case "$value" in ''|*[!0-9]*) echo "$name must be a number" >&2; exit 2 ;; esac
  if [ "$value" -lt "$lo" ] || [ "$value" -gt "$hi" ]; then
    echo "$name out of range ($lo..$hi)" >&2; exit 2
  fi
done

# Receive-side idle budget (seconds): applies to protocol frames AFTER a
# connection exists; the accept wait for the first sender is unbounded in
# the engine, so a slow human on the Android UI never starves the receiver.
RECV_IDLE=60

# ---------------------------------------------------------------------------
# Helpers + workspace (throwaway; never touches real user files)
# ---------------------------------------------------------------------------
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
step() { printf '[android-host-smoke] step %s\n' "$*"; }
note() { printf '[android-host-smoke] %s\n' "$*"; }
fail() {
  # Called from `|| fail ...` chains where the ERR trap never fires.
  FAILED=1
  printf '[android-host-smoke] FAIL: %s\n' "$*" >&2
  exit 1
}
assert_contains() { # file needle description
  grep -q -- "$2" "$1" || fail "expected \"$2\" in $(basename "$1"): $3"
}

WORK=""
LOGS=""
RECV_PID=""
FAILED=0

on_err() {
  FAILED=1
  printf '[android-host-smoke] FAILED at %s:%s (command: %s)\n' \
    "${BASH_SOURCE[0]}" "$1" "${2:-?}" >&2
}
trap 'on_err "$LINENO" "${BASH_COMMAND:-?}"' ERR

kill_bg() {
  [ -n "$1" ] || return 0
  kill "$1" 2>/dev/null || true
}

cleanup() {
  kill_bg "$RECV_PID"
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    if [ "$FAILED" -eq 1 ]; then
      printf '[android-host-smoke] logs kept for inspection: %s\n' "$LOGS" >&2
      printf '[android-host-smoke] workspace kept: %s\n' "$WORK" >&2
    elif [ "$KEEP" -eq 1 ]; then
      printf '[android-host-smoke] workspace kept (--keep): %s\n' "$WORK"
    else
      rm -rf "$WORK"
    fi
  fi
}
trap cleanup EXIT
trap 'printf "[android-host-smoke] interrupted\n" >&2; exit 130' INT TERM

WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-android-host.XXXXXXXX")
LOGS="$WORK/logs"
HOST_ID="$WORK/host-id"
INBOX="$WORK/inbox"
FIXTURE="$WORK/fixture"
mkdir -p "$LOGS" "$HOST_ID" "$INBOX" "$FIXTURE"
note "workspace: $WORK (inbox: $INBOX)"

# ---------------------------------------------------------------------------
# Step 1: locate (or build) the host-side ak-cli binary
# ---------------------------------------------------------------------------
PROFILE=debug; [ "$RELEASE" -eq 1 ] && PROFILE=release
step "1/7 locate ak-cli binary (prefer $PROFILE)"
export PATH="$USERPROFILE/.cargo/bin:$PATH"
if [ -z "$AK_BIN" ]; then
  if [ "$RELEASE" -eq 1 ]; then
    CANDIDATES=("$ROOT/target/release/ak-cli.exe" "$ROOT/target/debug/ak-cli.exe")
  else
    CANDIDATES=("$ROOT/target/debug/ak-cli.exe" "$ROOT/target/release/ak-cli.exe")
  fi
  for candidate in "${CANDIDATES[@]}"; do
    if [ -f "$candidate" ]; then AK_BIN=$candidate; break; fi
  done
fi
if [ -z "$AK_BIN" ]; then
  note "no prebuilt ak-cli found - building ($PROFILE), see $LOGS/build.log"
  if command -v cargo >/dev/null 2>&1; then
    if [ "$RELEASE" -eq 1 ]; then
      cargo build --manifest-path "$ROOT/Cargo.toml" -p ak-cli --release \
        > "$LOGS/build.log" 2>&1 || note "release build failed (see $LOGS/build.log)"
    else
      cargo build --manifest-path "$ROOT/Cargo.toml" -p ak-cli \
        > "$LOGS/build.log" 2>&1 || note "debug build failed (see $LOGS/build.log)"
    fi
    for candidate in "${CANDIDATES[@]}"; do
      if [ -f "$candidate" ]; then AK_BIN=$candidate; break; fi
    done
  else
    note "cargo not on PATH - cannot build"
  fi
fi
[ -n "$AK_BIN" ] || fail "no ak-cli binary (build with: cargo build -p ak-cli, or pass --binary <path>)"
[ -f "$AK_BIN" ] || fail "binary not found: $AK_BIN"
VERSION=$("$AK_BIN" --version 2>&1)
note "binary: $AK_BIN"
note "version: $VERSION"

# ---------------------------------------------------------------------------
# Step 2: fixture tree for adb push (flat on purpose -- the receiver-side
# layout detector below only has to cover three shapes).
#   payload.bin            --mb MiB of /dev/urandom
#   <chinese name>.txt     -- small file with a non-ASCII name + content
#   emptydir/              -- empty directory (must survive the transfer)
# ---------------------------------------------------------------------------
step "2/7 build fixture tree ($MB MiB random + chinese-named file + empty dir)"
MB_BYTES=$((MB * 1024 * 1024))
CN_NAME='说明-中文文件名.txt'
EMPTY_DIR_NAME=emptydir
FIXTURE_FILES=("payload.bin" "$CN_NAME")
FIXTURE_ROOT_ON_DEVICE=/sdcard/Download/ak-fixture

head -c "$MB_BYTES" /dev/urandom > "$FIXTURE/payload.bin"
printf 'AK-sending android->host smoke: non-ASCII name and content, run %s\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$FIXTURE/$CN_NAME"
mkdir -p "$FIXTURE/$EMPTY_DIR_NAME"
note "fixture: $FIXTURE ($(find "$FIXTURE" -type f | wc -l | tr -d ' ') files, ${MB} MiB random payload)"

# ---------------------------------------------------------------------------
# Step 3: throwaway host identity (keygen) -> fingerprint + pairing code
# ---------------------------------------------------------------------------
step "3/7 host identity (keygen into throwaway id-dir)"
"$AK_BIN" --id-dir "$HOST_ID" keygen > "$LOGS/keygen.log" 2>&1
FP_HOST=$(sed -n 's/^fingerprint (full): //p' "$LOGS/keygen.log" | head -1)
PAIR_CODE=$(sed -n 's/^pairing code: //p' "$LOGS/keygen.log" | head -1)
case "$FP_HOST" in
  ''|*[!0-9a-f]*) fail "keygen fingerprint missing/invalid: $FP_HOST" ;;
esac
[ ${#FP_HOST} -eq 64 ] || fail "keygen fingerprint not 64 hex: $FP_HOST"
case "$PAIR_CODE" in
  [0-9][0-9][0-9]-[0-9][0-9][0-9]) : ;;
  *) fail "keygen pairing code malformed: $PAIR_CODE" ;;
esac
note "host fingerprint (full): $FP_HOST"
note "host pairing code: $PAIR_CODE"

# ---------------------------------------------------------------------------
# Step 4: background receiver. `--expect any` because the Android side is a
# NEW identity the host has never booked; the honest trade-off is printed
# below and in the header comment above. Loop mode (no --once) so a failed
# first attempt from the app UI can simply be retried within the timeout.
# ---------------------------------------------------------------------------
step "4/7 start receiver: receive --out inbox --port $PORT --expect any (background)"
"$AK_BIN" --id-dir "$HOST_ID" receive \
  --out "$INBOX" --port "$PORT" --expect any --timeout "$RECV_IDLE" \
  > "$LOGS/receive.log" 2>&1 &
RECV_PID=$!
ADDR=""
STARTUP_BEGIN=$SECONDS
for _ in $(seq 1 40); do
  ADDR=$(sed -n 's/^address: //p' "$LOGS/receive.log" | head -1)
  [ -n "$ADDR" ] && break
  kill -0 "$RECV_PID" 2>/dev/null || break
  sleep 0.25
done
[ -n "$ADDR" ] || fail "receiver never printed its address (see $LOGS/receive.log)"
STARTUP_TOOK=$((SECONDS - STARTUP_BEGIN))
RECV_PORT=${ADDR##*:}
case "$RECV_PORT" in ''|*[!0-9]*) fail "cannot parse port from address: $ADDR" ;; esac
[ "$RECV_PORT" = "$PORT" ] || fail "receiver bound $ADDR but --port asked for $PORT"
assert_contains "$LOGS/receive.log" \
  "warning: --expect any accepts EVERY peer" "receiver echoes the --expect any warning"
assert_contains "$LOGS/receive.log" \
  "fingerprint (full): $FP_HOST" "receiver fingerprint matches keygen"
assert_contains "$LOGS/receive.log" \
  "pairing code: $PAIR_CODE" "receiver pairing code matches keygen"
assert_contains "$LOGS/receive.log" \
  "waiting for sender" "receiver reached the wait-for-sender phase"
note "receiver up in ${STARTUP_TOOK}s, bound $ADDR, log: $LOGS/receive.log"

# ---------------------------------------------------------------------------
# The three things the Android side needs, then the security trade-off.
# ---------------------------------------------------------------------------
printf '[android-host-smoke] ===========================================================\n'
printf '[android-host-smoke]  ANDROID SENDER - enter these in the app send page:\n'
printf '[android-host-smoke]    address     : 10.0.2.2:%s   (host as seen from emulator)\n' "$PORT"
printf '[android-host-smoke]    fingerprint : %s\n' "$FP_HOST"
printf '[android-host-smoke]    pairing code: %s   (cross-check on the app side)\n' "$PAIR_CODE"
printf '[android-host-smoke] -----------------------------------------------------------\n'
printf '[android-host-smoke]  NOTE: a REAL phone (not emulator) must use the host\n'
printf '[android-host-smoke]  LAN ip instead of 10.0.2.2 (see: ipconfig), same port.\n'
printf '[android-host-smoke] ===========================================================\n'
printf '[android-host-smoke]  WARNING --expect any: this run accepts EVERY peer.\n'
printf '[android-host-smoke]  The Android side is a brand-new identity, so the host\n'
printf '[android-host-smoke]  cannot book it in advance. Authentication relies on the\n'
printf '[android-host-smoke]  ANDROID send page pinning the host fingerprint (--fp\n'
printf '[android-host-smoke]  above) + you eyeballing the pairing code. The host side\n'
printf '[android-host-smoke]  stays open this run (first-transfer convenience). To\n'
printf '[android-host-smoke]  tighten a re-run: --expect <fingerprint|pairing-code>.\n'
printf '[android-host-smoke] ===========================================================\n'

# ---------------------------------------------------------------------------
# Step 5a: --self-test gate - no Android wait, exit clean.
# ---------------------------------------------------------------------------
if [ "$SELF_TEST" -eq 1 ]; then
  step "5/7 self-test: startup + three-elements output verified, skipping the wait"
  note "receiver started in ${STARTUP_TOOK}s (address $ADDR)"
  note "fingerprint (full): $FP_HOST"
  note "pairing code: $PAIR_CODE"
  note "PASS: self-test (host receiver startup verified; no android involved)"
  exit 0
fi

# ---------------------------------------------------------------------------
# Step 5b: adb probe + fixture push (both optional; manual fallback).
# ---------------------------------------------------------------------------
ADB_PATH="${AK_ADB:-$HOME/android-toolchain/sdk/platform-tools/adb.exe}"
ADB_MODE=manual
SERIAL=""
if [ -f "$ADB_PATH" ]; then
  ADB_DEVICES_OUT=$("$ADB_PATH" devices 2>&1 || true)
  printf '%s\n' "$ADB_DEVICES_OUT" > "$LOGS/adb-devices.log"
  SERIAL=$(printf '%s\n' "$ADB_DEVICES_OUT" | awk 'NR>1 && NF==2 && $2=="device"{print $1; exit}')
  if [ -n "$SERIAL" ]; then
    ADB_MODE=device
  else
    note "adb present but no attached device (see $LOGS/adb-devices.log) - manual mode"
  fi
else
  note "adb not found at $ADB_PATH (override with AK_ADB=<path>) - manual mode"
fi

if [ "$ADB_MODE" = device ]; then
  step "5/7 adb device $SERIAL: push fixture -> $FIXTURE_ROOT_ON_DEVICE"
  set +e
  "$ADB_PATH" -s "$SERIAL" shell rm -rf "$FIXTURE_ROOT_ON_DEVICE" \
    > "$LOGS/adb-rm.log" 2>&1
  RC_RM=$?
  "$ADB_PATH" -s "$SERIAL" shell mkdir -p "$FIXTURE_ROOT_ON_DEVICE" \
    > "$LOGS/adb-mkdir.log" 2>&1
  RC_MKDIR=$?
  set -e
  [ "$RC_RM" -eq 0 ] || note "warning: remote rm -rf returned $RC_RM (continuing)"
  [ "$RC_MKDIR" -eq 0 ] || fail "adb mkdir failed rc=$RC_MKDIR (see $LOGS/adb-mkdir.log)"
  # Push the whole tree via "<dir>/." so the non-ASCII fixture name never
  # rides in an argv (MSYS -> Windows argv encoding could mangle it).
  set +e
  "$ADB_PATH" -s "$SERIAL" push "$FIXTURE/." "$FIXTURE_ROOT_ON_DEVICE/" \
    > "$LOGS/adb-push.log" 2>&1
  RC_PUSH=$?
  set -e
  if [ "$RC_PUSH" -ne 0 ]; then
    note "contents push failed rc=$RC_PUSH - falling back to per-entry push"
    for entry in "$FIXTURE"/*; do
      set +e
      "$ADB_PATH" -s "$SERIAL" push "$entry" "$FIXTURE_ROOT_ON_DEVICE/" \
        >> "$LOGS/adb-push.log" 2>&1
      RC_ENTRY=$?
      set -e
      [ "$RC_ENTRY" -eq 0 ] || fail "adb push failed for: $entry (see $LOGS/adb-push.log)"
    done
    "$ADB_PATH" -s "$SERIAL" shell mkdir -p "$FIXTURE_ROOT_ON_DEVICE/$EMPTY_DIR_NAME"
  fi
  REMOTE_LS=$("$ADB_PATH" -s "$SERIAL" shell ls -1 "$FIXTURE_ROOT_ON_DEVICE" 2>&1 | tr -d '\r')
  printf '%s\n' "$REMOTE_LS" > "$LOGS/adb-ls.log"
  printf '%s\n' "$REMOTE_LS" | grep -qx 'payload.bin' \
    || fail "payload.bin missing on device after push (ls: $REMOTE_LS)"
  note "remote fixture listing ($FIXTURE_ROOT_ON_DEVICE):"
  printf '%s\n' "$REMOTE_LS" | sed 's/^/[android-host-smoke]   /'
  printf '%s\n' "$REMOTE_LS" | grep -q "$EMPTY_DIR_NAME" \
    || note "note: $EMPTY_DIR_NAME not in remote ls (empty dir push may be skipped by this adb)"
else
  step "5/7 manual mode: push the fixture yourself, then send from the app"
  note "1) push the fixture tree to the device (Device File Explorer or adb):"
  note "     \"$ADB_PATH\" push \"$FIXTURE/.\" $FIXTURE_ROOT_ON_DEVICE/"
  note "   (or any adb you have; the point is the app must send THIS tree,"
  note "    because verification cmps the fixture against the inbox)"
  note "2) in the app send page enter:"
  note "     path(s)     : $FIXTURE_ROOT_ON_DEVICE (one root, the whole tree)"
  note "     peer address: 10.0.2.2:$PORT"
  note "     fingerprint : $FP_HOST"
fi

# ---------------------------------------------------------------------------
# Step 6: wait for the android transfer. Accepted inbox layouts (the app
# may send the fixture dir as ONE root, or its top-level entries as roots):
#   A  inbox/<root-name>/<contents>   (a dir root keeps its name at top
#      level: "ak-fixture" when sent from the device path)
#   B  inbox/<contents>               (contents land at the inbox root)
#   C  inbox/<file>/<file> + inbox/emptydir  (each top-level FILE sent as
#      its own root is wrapped in a dir of its name; dir roots are not)
# ---------------------------------------------------------------------------
files_match_under() { # base: every fixture file cmp-identical under base/
  local f
  for f in "${FIXTURE_FILES[@]}"; do
    cmp -s "$FIXTURE/$f" "$1/$f" || return 1
  done
  return 0
}
check_layout() { # sets LAYOUT="A:<dir>"|B|C when the inbox fully matches
  LAYOUT=""
  local d f
  for d in "$INBOX"/*/; do
    [ -d "$d" ] || continue
    if files_match_under "${d%/}" && [ -d "${d%/}/$EMPTY_DIR_NAME" ]; then
      LAYOUT="A:${d%/}"
      return 0
    fi
  done
  if files_match_under "$INBOX" && [ -d "$INBOX/$EMPTY_DIR_NAME" ]; then LAYOUT=B; return 0; fi
  for f in "${FIXTURE_FILES[@]}"; do
    cmp -s "$FIXTURE/$f" "$INBOX/$f/$f" || return 1
  done
  if [ -d "$INBOX/$EMPTY_DIR_NAME" ]; then LAYOUT=C; return 0; fi
  return 1
}

step "6/7 waiting for the android transfer (timeout ${TIMEOUT_SECS}s)"
note "expecting: ${FIXTURE_FILES[*]} (+ empty dir $EMPTY_DIR_NAME); layouts A/B/C accepted"
WAIT_BEGIN=$SECONDS
DEADLINE=$((SECONDS + TIMEOUT_SECS))
POLL_N=0
LAYOUT=""
while [ "$SECONDS" -lt "$DEADLINE" ]; do
  if check_layout; then break; fi
  if ! kill -0 "$RECV_PID" 2>/dev/null; then
    sleep 2
    if check_layout; then break; fi
    fail "receiver exited before the transfer finished (see $LOGS/receive.log)"
  fi
  POLL_N=$((POLL_N + 1))
  if [ $((POLL_N % 10)) -eq 0 ]; then
    INBOX_FILES=$(find "$INBOX" -type f 2>/dev/null | wc -l | tr -d ' ')
    LAST_LINE=$(tr '\r' '\n' < "$LOGS/receive.log" | tail -n 1 | cut -c1-90)
    note "waiting: $((SECONDS - WAIT_BEGIN))s/${TIMEOUT_SECS}s, inbox files: $INBOX_FILES, recv: $LAST_LINE"
  fi
  sleep 2
done
[ -n "$LAYOUT" ] || fail "timed out after ${TIMEOUT_SECS}s waiting for the android transfer (inbox listing: $(find "$INBOX" -mindepth 1 -maxdepth 2 | tr '\n' ' '); log: $LOGS/receive.log)"

# ---------------------------------------------------------------------------
# Step 7: final byte-for-byte verification + summary.
# ---------------------------------------------------------------------------
step "7/7 final byte-for-byte verification (layout $LAYOUT)"
verify_one() { # src dst label
  cmp "$1" "$2" || fail "content mismatch: $3 ($1 vs $2)"
  note "OK  $3 ($(wc -c < "$1" | tr -d ' ') bytes identical)"
}
if [ "${LAYOUT%%:*}" = A ]; then
  BASE=${LAYOUT#A:}
  note "single directory root landed as: ${BASE#"$INBOX"/}"
  for f in "${FIXTURE_FILES[@]}"; do verify_one "$FIXTURE/$f" "$BASE/$f" "$f"; done
  [ -d "$BASE/$EMPTY_DIR_NAME" ] || fail "empty dir ${BASE#"$INBOX"/}/$EMPTY_DIR_NAME missing"
elif [ "$LAYOUT" = B ]; then
  for f in "${FIXTURE_FILES[@]}"; do verify_one "$FIXTURE/$f" "$INBOX/$f" "$f"; done
  [ -d "$INBOX/$EMPTY_DIR_NAME" ] || fail "empty dir $EMPTY_DIR_NAME missing at inbox root"
else
  for f in "${FIXTURE_FILES[@]}"; do verify_one "$FIXTURE/$f" "$INBOX/$f/$f" "$f (wrapped root)"; done
  [ -d "$INBOX/$EMPTY_DIR_NAME" ] || fail "empty dir $EMPTY_DIR_NAME missing at inbox root"
fi
note "empty directory $EMPTY_DIR_NAME recreated"

EXPECTED_N=${#FIXTURE_FILES[@]}
INBOX_N=$(find "$INBOX" -type f | wc -l | tr -d ' ')
if [ "$INBOX_N" -ne "$EXPECTED_N" ]; then
  note "warning: inbox holds $INBOX_N file(s), fixture has $EXPECTED_N - extras:"
  find "$INBOX" -type f | head -20 | sed 's/^/[android-host-smoke]   /'
  note "(extras do not fail this run; e.g. .akpart leftovers from an abandoned retry)"
fi

JOB_ID=$(sed -n 's/^job \([0-9a-f]\{32\}\) complete:.*/\1/p' "$LOGS/receive.log" | head -1)
if [ ${#JOB_ID} -eq 32 ]; then
  note "receiver reported job $JOB_ID complete"
else
  note "note: no 'job ... complete' line found in the receiver log (byte verify is authoritative)"
fi

kill_bg "$RECV_PID"
set +e
wait "$RECV_PID" 2>/dev/null
set -e
RECV_PID=""

note "tighten a re-run: --expect <android-fingerprint|pairing-code> instead of any"
note "PASS: android -> host transfer verified byte-for-byte (layout $LAYOUT, $EXPECTED_N/$EXPECTED_N files + empty dir, port $PORT)"
exit 0
