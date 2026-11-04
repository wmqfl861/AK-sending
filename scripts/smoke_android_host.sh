#!/usr/bin/env bash
# Cross-device smoke, HOST side, in TWO directions:
#
#   default (android -> host): this Windows host (Git Bash) runs the
#   ak-cli RECEIVER; an Android device/emulator runs the Flutter app as the
#   SENDER, pointed at 10.0.2.2:<port> (the host as seen from inside the
#   Android emulator).
#
#   --reverse (host -> android): the ANDROID side runs the ak-cli RECEIVER
#   (adb shell /data/local/tmp/ak-cli receive --out /data/local/tmp/rin
#   --port <port> --expect any --once, started in the background by this
#   script) and THIS host is the SENDER:
#     ak-cli send <fixture> --to 127.0.0.1:<port> --fp <android-fp>
#   The android fingerprint comes from a keygen run into a fixed id-dir on
#   the device; after the transfer /data/local/tmp/rin is pulled back with
#   adb and cmp-verified byte for byte against the fixture.
#
# What one default-direction run proves:
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
# What one --reverse run proves (real engine both ends, no human in the
# loop): android-side keygen + receiver startup under adb; a full
# host-sender -> android-receiver QUIC transfer of the fixture; the
# android receiver reports final ack verified=true; the received tree
# pulled back to the host cmps identical byte for byte (layouts A/B/C).
#
# TRANSPORT NOTE for --reverse (why a host UDP relay shim is needed):
# AK-sending speaks QUIC over UDP. adb forward/reverse tunnels are TCP-only
# and CANNOT carry it. The emulator console's UDP redirection (`adb emu
# redir add udp:A:B`, slirp) does forward host->guest datagrams, BUT it
# rewrites the SOURCE PORT of guest->host replies (slirp allocates an
# ephemeral reverse socket; measured live: reply from 127.0.0.1:56101 for
# a datagram sent to 127.0.0.1:17913). quinn 0.11 discards handshake
# packets whose source address differs from the dialed one, so a direct
# host -> redir -> android QUIC connection can never complete. The reverse
# mode therefore runs a tiny python UDP relay ON THE HOST: the sender
# talks to 127.0.0.1:<port> (the relay; replies keep that exact source),
# the relay forwards to the redir listener on 127.0.0.1:<port+5> which
# slirp maps to guest port <port>. Every packet stays a UDP datagram end
# to end; only the outer address pair is NATed -- the same rewrite a real
# router does. On a REAL phone none of this is needed: send to the phone's
# LAN ip instead (adb reverse tcp:... would still NOT work: TCP-only).
#
# Security trade-off, stated honestly (also printed at run time): the
# Android side is a brand-new identity the host has never booked, so the
# default-direction receiver runs with `--expect any` (accepts EVERY
# peer). Authentication of the android->host direction therefore rests
# entirely on the ANDROID side: the app's send page pins the host
# fingerprint (--fp), and the operator visually cross-checks the host
# pairing code. The host->android direction is NOT authenticated this run
# (first-transfer convenience). To tighten a re-run, replace `--expect
# any` with `--expect <fingerprint|pairing-code>`. --reverse mirrors this:
# the android receiver accepts any peer, but the HOST sender PINS the
# android fingerprint (--fp) and cross-checks its pairing code.
#
# Usage:
#   bash scripts/smoke_android_host.sh [OPTIONS]
# Options:
#   --port <n>          UDP port used by the run (default 17800; with
#                       --reverse the default is 17910). Default direction:
#                       the port the host receiver binds / android targets
#                       via 10.0.2.2. Reverse: android receiver port, host
#                       relay port, and sender target port; the redir
#                       listener additionally occupies <port>+5 on the
#                       host (default 17915).
#   --mb <n>            random payload size in MiB (default 8) for
#                       fixture/payload.bin
#   --release           prefer target/release/ak-cli.exe and build release
#                       when absent (default: prefer debug)
#   --keep              keep the temporary workspace (printed at the end)
#   --timeout-secs <n>  how long to wait for the Android transfer
#                       (default 300)
#   --self-test         gate mode: verify receiver startup + the
#                       address/fingerprint/pairing-code output, then clean
#                       up and exit 0. Default direction needs NO device;
#                       --reverse --self-test needs a device (android-side
#                       keygen + receiver startup) but sends nothing.
#   --reverse           run the host -> android direction (see above);
#                       requires an ONLINE EMULATOR (adb emu redir),
#                       python on PATH, and /data/local/tmp/ak-cli on the
#                       device
#   --binary <path>     explicit ak-cli binary (default: target/ search,
#                       then cargo build fallback)
#   -h | --help         this text
#
# adb: $HOME/android-toolchain/sdk/platform-tools/adb.exe (override with
# AK_ADB=<path>). Default direction: optional (manual fallback without a
# device). --reverse: REQUIRED (needs a device; `adb emu` only works
# against an emulator). All adb shell/push/pull calls in this script run
# with MSYS_NO_PATHCONV=1 and Windows-form local paths (Git Bash rewrites
# leading-slash arguments like /data/local/tmp into C:/Program Files/Git/
# ... for native exes, which would corrupt every device path).
# Extra unexpected files in the inbox (e.g. .akpart leftovers from an
# abandoned retry) are reported as warnings, not failures; fixture CONTENT
# must match byte for byte or the run FAILs.
#
# Exit code 0 = PASS (or self-test PASS). On failure the workspace is KEPT
# and printed. Tooling: Git Bash coreutils only (bash, grep, sed, cmp,
# find, mktemp, head, tr, awk); --reverse additionally needs python.

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
PORT=17800
PORT_GIVEN=0
REVERSE=0
MB=8
RELEASE=0
KEEP=0
TIMEOUT_SECS=300
SELF_TEST=0
AK_BIN=""

usage() { sed -n '/^# Usage:/,/^#   -h | --help /p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --port)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              PORT=$2; PORT_GIVEN=1; shift 2 ;;
    --mb)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              MB=$2; shift 2 ;;
    --release) RELEASE=1; shift ;;
    --keep)   KEEP=1; shift ;;
    --timeout-secs) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
              TIMEOUT_SECS=$2; shift 2 ;;
    --self-test) SELF_TEST=1; shift ;;
    --reverse) REVERSE=1; shift ;;
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
# --reverse defaults to its own port (17910) unless --port was explicit;
# the emulator redir listener then occupies PORT+5 (default 17915).
if [ "$REVERSE" -eq 1 ] && [ "$PORT_GIVEN" -eq 0 ]; then
  PORT=17910
fi

# Receive-side idle budget (seconds): applies to protocol frames AFTER a
# connection exists; the accept wait for the first sender is unbounded in
# the engine, so a slow human on the Android UI never starves the receiver.
RECV_IDLE=60

# ---------------------------------------------------------------------------
# adb plumbing shared by both directions (Git Bash path-conversion guard)
# ---------------------------------------------------------------------------
ADB_PATH="${AK_ADB:-$HOME/android-toolchain/sdk/platform-tools/adb.exe}"

# adb.exe is a native Windows program: Git Bash (MSYS2) rewrites arguments
# that look like POSIX absolute paths (/data/local/tmp/x becomes
# C:/Program Files/Git/data/local/tmp/x), silently corrupting every device
# path. adb_cmd disables that rewrite for exactly one invocation.
adb_cmd() {
  MSYS_NO_PATHCONV=1 "$ADB_PATH" "$@"
}

# The inverse problem: with the rewrite off, adb push/pull still need HOST
# paths in Windows form; cygpath -m prints C:/... with forward slashes
# (native-exe friendly). Falls back to the raw path outside Git Bash.
host_path_for_adb() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

# ---------------------------------------------------------------------------
# Reverse-mode (host -> android) fixed device paths. /data/local/tmp is
# shell-writable without root; all three are wiped at start AND at exit.
# The android receiver listens on UDP $PORT; the emulator console redir
# listener occupies host UDP $FWD_PORT (= $PORT + 5) and maps it to guest
# $PORT; the host python relay bridges 127.0.0.1:$PORT -> 127.0.0.1:$FWD_PORT.
# ---------------------------------------------------------------------------
AK_BIN_ON_DEVICE=/data/local/tmp/ak-cli
REV_IDDIR=/data/local/tmp/ak-smoke-rid
REV_OUT=/data/local/tmp/rin
FWD_PORT=$((PORT + 5))

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
RELAY_PID=""
ADB_RECV_PID=""
REDIR_ADDED=0
SERIAL=""
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
  kill_bg "$RELAY_PID"
  kill_bg "$ADB_RECV_PID"
  # Reverse-mode device residue: the emulator redir entry and the receiver
  # output/id-dir under /data/local/tmp. Best effort -- a failed adb here
  # must not mask the real result. (A leftover redir entry also disappears
  # when the emulator restarts.) The device-side receiver process is killed
  # explicitly because killing adb.exe on the host does NOT stop it.
  if [ "$REVERSE" -eq 1 ] && [ -n "$SERIAL" ]; then
    adb_cmd -s "$SERIAL" shell pkill -f "ak-cli --id-dir $REV_IDDIR" >/dev/null 2>&1 || true
    [ "$REDIR_ADDED" -eq 1 ] \
      && adb_cmd -s "$SERIAL" emu redir del "udp:$FWD_PORT" >/dev/null 2>&1 || true
    adb_cmd -s "$SERIAL" shell rm -rf "$REV_OUT" "$REV_IDDIR" >/dev/null 2>&1 || true
  fi
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
# REVERSE MODE (host -> android). This branch replaces the default
# host-receiver flow below and ends in exit, so the default direction code
# is untouched. Requirements: an ONLINE EMULATOR (`adb emu` is the only
# UDP-capable host->guest door and only emulators have it), python on PATH
# (host relay), and $AK_BIN_ON_DEVICE already pushed to the device.
# ---------------------------------------------------------------------------
if [ "$REVERSE" -eq 1 ]; then
  [ "$PORT" -le 65530 ] || fail "--port too high for --reverse (the redir listener needs port $((PORT + 5)))"
  EXPECTED_N=${#FIXTURE_FILES[@]}

  step "R1/8 adb: require an online emulator (reverse needs adb emu redir)"
  [ -f "$ADB_PATH" ] || fail "adb not found at $ADB_PATH (override with AK_ADB=<path>)"
  ADB_DEVICES_OUT=$(adb_cmd devices 2>&1 || true)
  printf '%s\n' "$ADB_DEVICES_OUT" > "$LOGS/adb-devices.log"
  SERIAL=$(printf '%s\n' "$ADB_DEVICES_OUT" | awk 'NR>1 && NF==2 && $2=="device"{print $1; exit}')
  [ -n "$SERIAL" ] || fail "no attached device -- reverse is a device test (see $LOGS/adb-devices.log)"
  adb_cmd -s "$SERIAL" shell ls "$AK_BIN_ON_DEVICE" >/dev/null 2>&1 \
    || fail "$AK_BIN_ON_DEVICE not found on $SERIAL (cross-build ak-cli for x86_64-linux-android and adb push it first)"
  note "device: $SERIAL; android binary: $AK_BIN_ON_DEVICE"

  step "R2/8 android identity: keygen into fixed id-dir $REV_IDDIR"
  adb_cmd -s "$SERIAL" shell rm -rf "$REV_IDDIR" "$REV_OUT" >/dev/null 2>&1 || true
  # A previous run's android receiver can OUTLIVE its adb client (killing
  # adb.exe on the host does not stop the device-side process), and it
  # would keep UDP $PORT bound -> "receiver failed to bind". Kill any
  # stale receiver carrying this smoke's fixed id-dir before rebinding.
  if adb_cmd -s "$SERIAL" shell pkill -f "ak-cli --id-dir $REV_IDDIR" >/dev/null 2>&1; then
    sleep 1
    note "stale android receiver (id-dir $REV_IDDIR) killed if any"
  fi
  adb_cmd -s "$SERIAL" shell mkdir -p "$REV_OUT" \
    || fail "cannot create $REV_OUT on device $SERIAL"
  adb_cmd -s "$SERIAL" shell "$AK_BIN_ON_DEVICE" --id-dir "$REV_IDDIR" keygen \
    > "$LOGS/android-keygen.log" 2>&1 \
    || fail "android keygen failed (see $LOGS/android-keygen.log)"
  FP_ANDROID=$(sed -n 's/^fingerprint (full): //p' "$LOGS/android-keygen.log" | head -1 | tr -d '\r')
  PAIR_ANDROID=$(sed -n 's/^pairing code: //p' "$LOGS/android-keygen.log" | head -1 | tr -d '\r')
  case "$FP_ANDROID" in
    ''|*[!0-9a-f]*) fail "android keygen fingerprint missing/invalid: $FP_ANDROID" ;;
  esac
  [ ${#FP_ANDROID} -eq 64 ] || fail "android fingerprint not 64 hex: $FP_ANDROID"
  case "$PAIR_ANDROID" in
    [0-9][0-9][0-9]-[0-9][0-9][0-9]) : ;;
    *) fail "android pairing code malformed: $PAIR_ANDROID" ;;
  esac
  note "android fingerprint (full): $FP_ANDROID"
  note "android pairing code: $PAIR_ANDROID"

  step "R3/8 emulator udp redir (host $FWD_PORT -> guest $PORT) + host python relay"
  command -v python >/dev/null 2>&1 || command -v python3 >/dev/null 2>&1 \
    || fail "python not on PATH (--reverse needs it for the host UDP relay; see the header for why)"
  PY=$(command -v python || command -v python3)
  adb_cmd -s "$SERIAL" emu redir add "udp:$FWD_PORT:$PORT" > "$LOGS/redir-add.log" 2>&1 \
    || fail "adb emu redir add failed (emulator-only gate; a real phone must use LAN addressing instead. If a stale entry exists: adb -s $SERIAL emu redir del udp:$FWD_PORT. See $LOGS/redir-add.log)"
  REDIR_ADDED=1
  REDIR_LIST=$(adb_cmd -s "$SERIAL" emu redir list 2>&1 | tr -d '\r')
  printf '%s\n' "$REDIR_LIST" > "$LOGS/redir-list.log"
  printf '%s\n' "$REDIR_LIST" | grep -q "udp:$FWD_PORT => $PORT" \
    || fail "redir entry 'udp:$FWD_PORT => $PORT' not confirmed (list: $REDIR_LIST)"
  note "redir active: $(printf '%s\n' "$REDIR_LIST" | grep "udp:$FWD_PORT" | head -1)"

  # The relay: why it exists (details in the header). quinn drops handshake
  # packets whose source address differs from the dialed one, and slirp's
  # udp redir rewrites the reply source port, so the sender cannot talk to
  # the redir listener directly. The relay keeps the sender-facing socket
  # (127.0.0.1:$PORT) as the stable reply source and shuttles datagrams to
  # the redir listener (127.0.0.1:$FWD_PORT) over its own second socket.
  cat > "$WORK/udprelay.py" <<'PYEOF'
# Host-local UDP relay (reverse smoke shim). Listens on 127.0.0.1:<listen>
# -- the address the host ak-cli sender dials -- and forwards every
# datagram through a second socket to 127.0.0.1:<fwd> (the emulator console
# `redir` udp listener, which slirp maps to the guest receiver port).
# Replies arriving back on the second socket are relayed to the last
# sender FROM the listening socket, so the sender always observes the
# exact peer address it dialed (QUIC address validation stays happy).
import socket
import sys
import threading

LISTEN_PORT = int(sys.argv[1])
FWD_PORT = int(sys.argv[2])

listen_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
listen_sock.bind(("127.0.0.1", LISTEN_PORT))
fwd_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
fwd_sock.bind(("127.0.0.1", 0))
peer = [None]
lock = threading.Lock()

def uplink():
    while True:
        data, addr = listen_sock.recvfrom(65536)
        with lock:
            peer[0] = addr
        fwd_sock.sendto(data, ("127.0.0.1", FWD_PORT))

def downlink():
    while True:
        data, _ = fwd_sock.recvfrom(65536)
        with lock:
            target = peer[0]
        if target is not None:
            listen_sock.sendto(data, target)

print("relay up: 127.0.0.1:%d <-> 127.0.0.1:%d (fwd socket local port %d)"
      % (LISTEN_PORT, FWD_PORT, fwd_sock.getsockname()[1]), flush=True)
threading.Thread(target=downlink, daemon=True).start()
uplink()
PYEOF
  "$PY" "$WORK/udprelay.py" "$PORT" "$FWD_PORT" > "$LOGS/relay.log" 2>&1 &
  RELAY_PID=$!
  RELAY_UP=""
  for _ in $(seq 1 40); do
    RELAY_UP=$(sed -n 's/^relay up: //p' "$LOGS/relay.log" | head -1)
    [ -n "$RELAY_UP" ] && break
    kill -0 "$RELAY_PID" 2>/dev/null || break
    sleep 0.25
  done
  [ -n "$RELAY_UP" ] || fail "relay did not start (is host udp $PORT already in use? see $LOGS/relay.log)"
  note "relay up: $RELAY_UP (pid $RELAY_PID)"

  step "R4/8 android receiver: receive --out rin --port $PORT --expect any --once (background)"
  # --timeout 240: the wait for the first sender is unbounded, this is the
  # frame-gap budget once the transfer starts (default 30s is tight for a
  # cold emulator; the host sender below uses --timeout 120).
  adb_cmd -s "$SERIAL" shell "$AK_BIN_ON_DEVICE" --id-dir "$REV_IDDIR" receive \
    --out "$REV_OUT" --port "$PORT" --expect any --once --timeout 240 \
    > "$LOGS/android-receive.log" 2>&1 &
  ADB_RECV_PID=$!
  A_LOG="$LOGS/android-receive.log"
  RX_BEGIN=$SECONDS
  RX_READY=0
  for _ in $(seq 1 80); do
    RX_READY=$(tr -d '\r' < "$A_LOG" 2>/dev/null | grep -c '^waiting for sender' || true)
    [ "${RX_READY:-0}" -ge 1 ] && break
    kill -0 "$ADB_RECV_PID" 2>/dev/null || break
    sleep 0.25
  done
  [ "${RX_READY:-0}" -ge 1 ] 2>/dev/null \
    || fail "android receiver never reached the wait-for-sender phase (see $A_LOG)"
  A_ADDR=$(sed -n 's/^address: //p' "$A_LOG" | head -1 | tr -d '\r')
  case "$A_ADDR" in
    *:$PORT) : ;;
    *) fail "android receiver bound '${A_ADDR:-nothing}' but --port asked for $PORT" ;;
  esac
  assert_contains "$A_LOG" \
    "warning: --expect any accepts EVERY peer" "android receiver echoes the --expect any warning"
  assert_contains "$A_LOG" \
    "fingerprint (full): $FP_ANDROID" "android receiver fingerprint matches keygen"
  assert_contains "$A_LOG" \
    "pairing code: $PAIR_ANDROID" "android receiver pairing code matches keygen"
  note "android receiver up in $((SECONDS - RX_BEGIN))s (address $A_ADDR), log: $A_LOG"

  printf '[android-host-smoke] ===========================================================\n'
  printf '[android-host-smoke]  HOST SENDER pins the ANDROID receiver identity:\n'
  printf '[android-host-smoke]    send <fixture> --to 127.0.0.1:%s --fp %s\n' "$PORT" "$FP_ANDROID"
  printf '[android-host-smoke]    (this script runs that send itself; cross-check\n'
  printf '[android-host-smoke]     the android pairing code %s by eye)\n' "$PAIR_ANDROID"
  printf '[android-host-smoke] -----------------------------------------------------------\n'
  printf '[android-host-smoke]  TRANSPORT: quic/udp over  host:%s relay -> host:%s\n' "$PORT" "$FWD_PORT"
  printf '[android-host-smoke]    redir -> guest:%s. adb forward/reverse are TCP-\n' "$PORT"
  printf '[android-host-smoke]    only; the bare redir path breaks quinn address\n'
  printf '[android-host-smoke]    validation (slirp rewrites the reply source\n'
  printf '[android-host-smoke]    port), hence the local udp relay shim.\n'
  printf '[android-host-smoke] -----------------------------------------------------------\n'
  printf '[android-host-smoke]  WARNING the android receiver runs --expect any\n'
  printf '[android-host-smoke]  (brand-new identity, host never booked it), so\n'
  printf '[android-host-smoke]  android->host is NOT authenticated this run;\n'
  printf '[android-host-smoke]  host->android IS (the sender pins --fp above).\n'
  printf '[android-host-smoke] ===========================================================\n'

  if [ "$SELF_TEST" -eq 1 ]; then
    step "R-self-test: android keygen + receiver startup + redir verified, no transfer"
    note "android fingerprint (full): $FP_ANDROID"
    note "android pairing code: $PAIR_ANDROID"
    note "host identity for the (skipped) send: $FP_HOST"
    note "PASS: reverse self-test (android receiver startup verified; nothing sent)"
    exit 0
  fi

  step "R5/8 host sender: send $FIXTURE --to 127.0.0.1:$PORT --fp <android fp>"
  set +e
  "$AK_BIN" --id-dir "$HOST_ID" send "$FIXTURE" \
    --to "127.0.0.1:$PORT" --fp "$FP_ANDROID" --timeout 120 \
    > "$LOGS/host-send.log" 2>&1
  SEND_RC=$?
  set -e
  if [ "$SEND_RC" -ne 0 ]; then
    tail -n 20 "$LOGS/host-send.log" 2>/dev/null | sed 's/^/[android-host-smoke]   /' >&2 || true
    fail "host sender exited rc=$SEND_RC (see $LOGS/host-send.log; relay: $LOGS/relay.log)"
  fi
  assert_contains "$LOGS/host-send.log" \
    "final ack verified=true" "sender got the receiver's final ack"
  S_JOB=$(sed -n 's/^job \([0-9a-f]\{32\}\) complete:.*/\1/p' "$LOGS/host-send.log" | head -1)
  [ ${#S_JOB} -eq 32 ] || fail "no 'job ... complete' line in the sender log (see $LOGS/host-send.log)"
  note "sender: job $S_JOB complete; last lines:"
  tail -n 6 "$LOGS/host-send.log" | sed 's/^/[android-host-smoke]   /'

  step "R6/8 wait for the android receiver to finish (--once exits)"
  for _ in $(seq 1 120); do
    kill -0 "$ADB_RECV_PID" 2>/dev/null || break
    sleep 0.5
  done
  if kill -0 "$ADB_RECV_PID" 2>/dev/null; then
    fail "android receiver still running after 60s past the sender's exit (see $A_LOG)"
  fi
  kill_bg "$ADB_RECV_PID"
  ADB_RECV_PID=""
  assert_contains "$A_LOG" \
    "final ack verified=true" "android receiver verified the whole-transfer digest"
  A_JOB=$(sed -n 's/^job \([0-9a-f]\{32\}\) complete:.*/\1/p' "$A_LOG" | tr -d '\r' | head -1)
  [ ${#A_JOB} -eq 32 ] || fail "android receiver never reported a complete job (see $A_LOG)"
  note "android receiver: job $A_JOB complete"
  [ "$A_JOB" = "$S_JOB" ] \
    || note "note: sender/receiver job ids differ ($S_JOB vs $A_JOB); byte verify below is authoritative"
  tail -n 4 "$A_LOG" | tr -d '\r' | sed 's/^/[android-host-smoke]   /'

  step "R7/8 adb pull $REV_OUT + byte-for-byte cmp against the fixture"
  PULLED="$WORK/pulled"
  mkdir -p "$PULLED"
  ( cd "$PULLED" && adb_cmd -s "$SERIAL" pull "$REV_OUT" . > "$LOGS/adb-pull.log" 2>&1 ) \
    || fail "adb pull failed (see $LOGS/adb-pull.log)"
  RX_BASE="$PULLED/$(basename "$REV_OUT")"
  [ -d "$RX_BASE" ] \
    || fail "pulled tree missing: $RX_BASE (pulled listing: $(find "$PULLED" -mindepth 1 -maxdepth 2 2>/dev/null | tr '\n' ' '))"

  rev_files_match_under() { # $1 = base dir holding the fixture files
    local f
    for f in "${FIXTURE_FILES[@]}"; do
      [ -f "$1/$f" ] || return 1
      cmp -s "$FIXTURE/$f" "$1/$f" || return 1
    done
    return 0
  }
  rev_check_layout() { # sets REVLAYOUT="A:<dir>"|B|C against $RX_BASE
    REVLAYOUT=""
    local d f
    for d in "$RX_BASE"/*/; do
      [ -d "$d" ] || continue
      if rev_files_match_under "${d%/}" && [ -d "${d%/}/$EMPTY_DIR_NAME" ]; then
        REVLAYOUT="A:${d%/}"
        return 0
      fi
    done
    if rev_files_match_under "$RX_BASE" && [ -d "$RX_BASE/$EMPTY_DIR_NAME" ]; then
      REVLAYOUT=B
      return 0
    fi
    for f in "${FIXTURE_FILES[@]}"; do
      cmp -s "$FIXTURE/$f" "$RX_BASE/$f/$f" || return 1
    done
    if [ -d "$RX_BASE/$EMPTY_DIR_NAME" ]; then
      REVLAYOUT=C
      return 0
    fi
    return 1
  }
  if ! rev_check_layout; then
    fail "pulled tree does not match the fixture (listing: $(find "$RX_BASE" -mindepth 1 -maxdepth 2 2>/dev/null | tr '\n' ' '); logs: $A_LOG)"
  fi
  note "received layout: ${REVLAYOUT%%:*}"
  if [ "${REVLAYOUT%%:*}" = A ]; then
    RB=${REVLAYOUT#A:}
    for f in "${FIXTURE_FILES[@]}"; do
      cmp "$FIXTURE/$f" "$RB/$f" || fail "content mismatch: $f ($FIXTURE/$f vs $RB/$f)"
      note "OK  $f ($(wc -c < "$FIXTURE/$f" | tr -d ' ') bytes identical)"
    done
  elif [ "$REVLAYOUT" = B ]; then
    for f in "${FIXTURE_FILES[@]}"; do
      cmp "$FIXTURE/$f" "$RX_BASE/$f" || fail "content mismatch: $f ($FIXTURE/$f vs $RX_BASE/$f)"
      note "OK  $f ($(wc -c < "$FIXTURE/$f" | tr -d ' ') bytes identical)"
    done
  else
    for f in "${FIXTURE_FILES[@]}"; do
      cmp "$FIXTURE/$f" "$RX_BASE/$f/$f" || fail "content mismatch: $f (wrapped root)"
      note "OK  $f ($(wc -c < "$FIXTURE/$f" | tr -d ' ') bytes identical)"
    done
  fi
  note "empty directory $EMPTY_DIR_NAME recreated on the android side"
  RX_N=$(find "$RX_BASE" -type f | wc -l | tr -d ' ')
  if [ "$RX_N" -ne "$EXPECTED_N" ]; then
    note "warning: pulled tree holds $RX_N file(s), fixture has $EXPECTED_N - extras:"
    find "$RX_BASE" -type f | head -20 | sed 's/^/[android-host-smoke]   /'
  fi

  step "R8/8 cleanup (redir del, device wipe) + verdict"
  adb_cmd -s "$SERIAL" emu redir del "udp:$FWD_PORT" > "$LOGS/redir-del.log" 2>&1 || true
  REDIR_ADDED=0
  adb_cmd -s "$SERIAL" shell rm -rf "$REV_OUT" "$REV_IDDIR" > "$LOGS/adb-rm.log" 2>&1 || true
  kill_bg "$RELAY_PID"
  RELAY_PID=""
  note "redir udp:$FWD_PORT removed; $REV_OUT and $REV_IDDIR wiped on device"
  note "tighten a re-run: android receiver --expect <host-fp|pairing-code> instead of any"
  note "PASS: host -> android transfer verified byte-for-byte (layout ${REVLAYOUT%%:*}, $EXPECTED_N/$EXPECTED_N files + empty dir, port $PORT via relay+redir)"
  exit 0
fi

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
# Every call goes through adb_cmd / host_path_for_adb: Git Bash rewrites
# leading-slash device paths for native adb.exe (see the plumbing block).
# ---------------------------------------------------------------------------
ADB_MODE=manual
if [ -f "$ADB_PATH" ]; then
  ADB_DEVICES_OUT=$(adb_cmd devices 2>&1 || true)
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
  adb_cmd -s "$SERIAL" shell rm -rf "$FIXTURE_ROOT_ON_DEVICE" \
    > "$LOGS/adb-rm.log" 2>&1
  RC_RM=$?
  adb_cmd -s "$SERIAL" shell mkdir -p "$FIXTURE_ROOT_ON_DEVICE" \
    > "$LOGS/adb-mkdir.log" 2>&1
  RC_MKDIR=$?
  set -e
  [ "$RC_RM" -eq 0 ] || note "warning: remote rm -rf returned $RC_RM (continuing)"
  [ "$RC_MKDIR" -eq 0 ] || fail "adb mkdir failed rc=$RC_MKDIR (see $LOGS/adb-mkdir.log)"
  # Push the whole tree via "<dir>/." so the non-ASCII fixture name never
  # rides in an argv (MSYS -> Windows argv encoding could mangle it); the
  # local source goes through host_path_for_adb because adb_cmd disables
  # the automatic rewrite that would otherwise form the Windows path.
  set +e
  adb_cmd -s "$SERIAL" push "$(host_path_for_adb "$FIXTURE")/." "$FIXTURE_ROOT_ON_DEVICE/" \
    > "$LOGS/adb-push.log" 2>&1
  RC_PUSH=$?
  set -e
  if [ "$RC_PUSH" -ne 0 ]; then
    note "contents push failed rc=$RC_PUSH - falling back to per-entry push"
    for entry in "$FIXTURE"/*; do
      set +e
      adb_cmd -s "$SERIAL" push "$(host_path_for_adb "$entry")" "$FIXTURE_ROOT_ON_DEVICE/" \
        >> "$LOGS/adb-push.log" 2>&1
      RC_ENTRY=$?
      set -e
      [ "$RC_ENTRY" -eq 0 ] || fail "adb push failed for: $entry (see $LOGS/adb-push.log)"
    done
    adb_cmd -s "$SERIAL" shell mkdir -p "$FIXTURE_ROOT_ON_DEVICE/$EMPTY_DIR_NAME"
  fi
  REMOTE_LS=$(adb_cmd -s "$SERIAL" shell ls -1 "$FIXTURE_ROOT_ON_DEVICE" 2>&1 | tr -d '\r')
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
  note "     MSYS_NO_PATHCONV=1 \"$ADB_PATH\" push \"$(host_path_for_adb "$FIXTURE")/.\" $FIXTURE_ROOT_ON_DEVICE/"
  note "   (Git Bash: the MSYS_NO_PATHCONV prefix + windows-form local path keep"
  note "    both adb arguments intact, exactly like the automatic push above)"
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
