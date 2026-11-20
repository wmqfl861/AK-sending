#!/usr/bin/env bash
# Mixed-version loopback check for ak-cli (Windows Git Bash friendly).
#
# Upgrade compatibility before a release (batch S review, 2026-11-20): two
# builds of ak-cli -- typically the last released one ("old") and the
# candidate ("new") -- must keep transferring to each other in both
# directions. For every sender/receiver pairing of the two builds
# (new->new, new->old, old->new, old->old) and both data modes (single
# stream, --multistream) one job runs between two independent processes on
# 127.0.0.1:
#   1. the receiving build makes identity A and admits B (`pair add`), the
#      sending build makes identity B;
#   2. `receive --once` runs in the background and announces its address;
#   3. `send` pushes a throwaway random tree (nested directories, an empty
#      directory, small files and one multi-chunk file);
#   4. both processes must exit 0 and the received tree must match the
#      source byte for byte (`diff -r`).
# The payload comes from /dev/urandom into a temporary directory; no real
# user file is ever touched. Same machine, loopback only: this is a
# compatibility check, not a real-device or real-network test.
#
# Usage:
#   bash scripts/mixed_version_check.sh --old <ak-cli> --new <ak-cli> [OPTIONS]
# Options:
#   --old <path>   the earlier build (e.g. built from the previous release tag)
#   --new <path>   the candidate build
#   --port <n>     first UDP port for the receive legs (default 17800; each
#                  of the eight runs uses its own port, n .. n+7)
#   --keep         keep the temporary workspace (printed at the end)
#   -h | --help    this text
#
# Exit code 0 = all eight runs passed. On a failure the workspace with every
# log is kept and printed. Tooling: Git Bash coreutils (bash, grep, sed,
# head, diff, mktemp).

set -uo pipefail

OLD=""
NEW=""
PORT=17800
KEEP=0

usage() { sed -n '2,33p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --old)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
            OLD=$2; shift 2 ;;
    --new)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
            NEW=$2; shift 2 ;;
    --port) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
            PORT=$2; shift 2 ;;
    --keep) KEEP=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
case "$PORT" in
  ''|*[!0-9]*) echo "--port must be a number" >&2; exit 2 ;;
esac
if [ "$PORT" -lt 1024 ] || [ "$PORT" -gt 65520 ]; then
  echo "--port out of range" >&2; exit 2
fi
[ -n "$OLD" ] && [ -n "$NEW" ] || { echo "both --old and --new are required (see --help)" >&2; exit 2; }
for bin in "$OLD" "$NEW"; do
  [ -x "$bin" ] || { echo "not an executable: $bin" >&2; exit 2; }
done

note() { printf '[mixed] %s\n' "$*"; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-mixed.XXXXXXXX")
FAILED=0
cleanup() {
  if [ "$FAILED" -ne 0 ] || [ "$KEEP" -eq 1 ]; then
    note "workspace kept: $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

note "old: $("$OLD" --version 2>&1)"
note "new: $("$NEW" --version 2>&1)"

# One random source tree shared by every run (read-only for the senders).
SRC="$WORK/src/payload"
mkdir -p "$SRC/nested/deep" "$SRC/empty-dir"
for i in 1 2 3 4 5 6 7 8; do
  head -c $((i * 1531)) /dev/urandom > "$SRC/nested/small-$i.bin"
done
head -c 1 /dev/urandom > "$SRC/nested/deep/one-byte.bin"
: > "$SRC/nested/deep/empty.bin"
# Several 1 MiB chunks plus a tail, so the multi-chunk path runs.
head -c $((3 * 1024 * 1024 + 4099)) /dev/urandom > "$SRC/large.bin"

# fingerprint of the identity in <dir>, from its keygen log
fingerprint() { sed -n 's/^fingerprint (full): //p' "$1" | head -1; }

run_one() { # <sender bin> <receiver bin> <label> <port> [--multistream]
  local snd=$1 rcv=$2 label=$3 port=$4 mode=${5:-}
  local dir="$WORK/run-$port"
  mkdir -p "$dir/recv"
  "$rcv" --id-dir "$dir/idA" keygen > "$dir/keygenA.log" 2>&1
  "$snd" --id-dir "$dir/idB" keygen > "$dir/keygenB.log" 2>&1
  local fp_a fp_b
  fp_a=$(fingerprint "$dir/keygenA.log")
  fp_b=$(fingerprint "$dir/keygenB.log")
  if [ ${#fp_a} -ne 64 ] || [ ${#fp_b} -ne 64 ]; then
    note "$label: FAIL (keygen printed no fingerprint; see $dir)"; return 1
  fi
  "$rcv" --id-dir "$dir/idA" pair add "$fp_b" --name mixed-b > "$dir/pair.log" 2>&1 \
    || { note "$label: FAIL (pair add; see $dir/pair.log)"; return 1; }
  "$rcv" --id-dir "$dir/idA" receive --once --out "$dir/recv" --port "$port" \
    --timeout 60 > "$dir/receive.log" 2>&1 &
  local recv_pid=$! addr=""
  for _ in $(seq 1 80); do
    addr=$(sed -n 's/^address: //p' "$dir/receive.log" | head -1)
    [ -n "$addr" ] && break
    kill -0 "$recv_pid" 2>/dev/null || break
    sleep 0.25
  done
  if [ -z "$addr" ]; then
    kill "$recv_pid" 2>/dev/null
    note "$label: FAIL (receiver printed no address; see $dir/receive.log)"; return 1
  fi
  # shellcheck disable=SC2086
  "$snd" --id-dir "$dir/idB" send "$SRC" --to "127.0.0.1:$port" --fp "$fp_a" \
    --timeout 60 $mode > "$dir/send.log" 2>&1
  local send_rc=$?
  for _ in $(seq 1 120); do
    kill -0 "$recv_pid" 2>/dev/null || break
    sleep 0.5
  done
  if kill -0 "$recv_pid" 2>/dev/null; then
    kill "$recv_pid" 2>/dev/null
    note "$label: FAIL (receiver still running 60s after the send; see $dir)"; return 1
  fi
  wait "$recv_pid"
  local recv_rc=$?
  local job
  job=$(sed -n 's/^job \([0-9a-f]\{8\}\)[0-9a-f]\{24\} complete:.*/\1/p' "$dir/send.log" | head -1)
  if [ "$send_rc" -ne 0 ] || [ "$recv_rc" -ne 0 ]; then
    note "$label: FAIL (send exit $send_rc, receive exit $recv_rc; see $dir)"; return 1
  fi
  # A single-root directory send lands its contents directly in --out.
  if ! diff -r -q "$SRC" "$dir/recv" > "$dir/diff.log" 2>&1; then
    note "$label: FAIL (received tree differs; see $dir/diff.log)"; return 1
  fi
  note "$label: PASS (job ${job:-?}, tree identical)"
}

port=$PORT
for pair in "new new" "new old" "old new" "old old"; do
  set -- $pair
  snd_bin=$NEW; [ "$1" = old ] && snd_bin=$OLD
  rcv_bin=$NEW; [ "$2" = old ] && rcv_bin=$OLD
  for mode in "" "--multistream"; do
    run_one "$snd_bin" "$rcv_bin" "$1->$2 ${mode:-single-stream}" "$port" $mode || FAILED=1
    port=$((port + 1))
  done
done

if [ "$FAILED" -ne 0 ]; then
  note "RESULT: FAIL"
  exit 1
fi
note "RESULT: PASS (8/8 runs)"
