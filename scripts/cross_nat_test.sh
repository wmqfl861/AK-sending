#!/usr/bin/env bash
# Cross-NAT (Windows sender -> public Linux receiver) QUIC transfer test.
#
# Real-internet validation for the deployment produced by
# scripts/deploy_linux.sh: this Windows machine (behind its NAT) pairs with
# the Linux box through the v2 registry (TLS 7200 on the server), then sends
# a random payload to the Linux ak-cli receiver over direct QUIC — the data
# path crosses the real internet + the local NAT; the registry/STUN never
# carry file bytes (ADR-001). The preflight `ak-cli doctor --stun` query is
# the OPERATIONS §0 step-3 external verification of the standalone
# stun-server (UDP 7101): it must answer with this machine's public reflexive
# address.
#
# Companion of scripts/cross_device_test.sh (bidirectional baseline). This
# script is the focused one-directional NAT case with the STUN gate.
#
# Usage:
#   bash scripts/cross_nat_test.sh --self-test
#   bash scripts/cross_nat_test.sh \
#     --ssh-user ubuntu --ssh-host <server-ip> \
#     [--ssh-pass <password> | --ssh-key <keyfile>] \
#     [--server-port 7200] [--stun-port 7101] \
#     [--registry-id <64hex>]        # default: fetched via SSH from
#                                    # /opt/ak-sending/registry-id (written by
#                                    # deploy_linux.sh), else the documented pin
#     [--remote-cli /opt/ak-sending/ak-cli] [--remote-port 7300] \
#     [--size-mb 5] [--timeout 60] [--binary <local ak-cli.exe>] \
#     [--skip-doctor] [--keep]
#
# Options:
#   --ssh-user <name>      SSH login user on the Linux server
#   --ssh-host <ip>        server address; also the default --server-host and
#                          the last-resort send candidate (public entry)
#   --ssh-port <n>         SSH TCP port (default 22)
#   --ssh-pass <secret>    SSH password (chmod-600 temp file only; backends:
#                          key ssh > sshpass > paramiko). Key auth preferred.
#   --ssh-key <path>       explicit identity file for plain ssh/scp
#   --server-host <ip>     registry host as seen from this Windows machine
#                          (default: --ssh-host)
#   --server-port <n>      registry TLS port (default 7200)
#   --stun-port <n>        standalone stun-server UDP port (default 7101)
#   --registry-id <64hex>  registry identity pin (registry-server prints it;
#                          default: fetched from the server via SSH)
#   --remote-cli <path>    ak-cli path on the server (default
#                          /opt/ak-sending/ak-cli, where deploy_linux.sh
#                          installs it)
#   --remote-server <a:p>  registry endpoint as seen from the server
#                          (default 127.0.0.1:<server-port>)
#   --remote-port <n>      receiver daemon UDP port on the server (default
#                          7300; the public candidate is <ssh-host>:<port>)
#   --local-port <n>       local daemon UDP port (default 7301)
#   --size-mb <n>          payload size in MiB (default 5)
#   --timeout <secs>       per-attempt send idle budget (default 60)
#   --binary <path>        local ak-cli (default <repo>/target/release/
#                          ak-cli.exe, then debug, then PATH)
#   --skip-doctor          skip the STUN preflight check (LAN-only debugging)
#   --keep                 keep the temporary workspace (always kept on FAIL)
#   --self-test            offline gate: argument parsing, identity
#                          generation, payload creation, candidate parser
#                          fixtures; no server, no SSH, no transfer
#   -h | --help            this text
#
# Flow (each step prints "[cross-nat] step N/6"):
#   0. preflight: local binary, SSH backend, remote cli, registry pin
#   1. NAT gate: ak-cli doctor --stun <host>:<stun-port> must report the
#      public reflexive address (查询成功) — external proof that the newly
#      deployed standalone stun-server answers through the real internet
#   2. local identity + owner daemon (pairing service) + invite code
#   3. remote identity + daemon redeeming the code (becomes the receiver)
#   4. candidate extraction (daemon NDJSON + <ssh-host>:<remote-port> last)
#   5. Windows -> Linux QUIC send + sha256 verify on the receiver
#   6. PASS/FAIL summary with throughput
#
# Exit code 0 = PASS (or self-test PASS).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
SSH_USER=""
SSH_HOST=""
SSH_PORT=22
SSH_PASS=""
SSH_KEY=""
SERVER_HOST=""
SERVER_PORT=7200
STUN_PORT=7101
REGISTRY_ID=""
REMOTE_CLI=/opt/ak-sending/ak-cli
REMOTE_SERVER=""
REMOTE_PORT=7300
LOCAL_PORT=7301
SIZE_MB=5
SEND_TIMEOUT=60
AK_BIN=""
SKIP_DOCTOR=0
KEEP=0
SELF_TEST=0
# Fallback pin recorded in docs/OPERATIONS.zh-CN.md §3 (public hex; the
# production registry identity as of 2026-11-05, unverified online).
DOCUMENTED_PIN=8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36

usage() { sed -n '/^# Usage:/,/^#   -h | --help /p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --ssh-user)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SSH_USER=$2; shift 2 ;;
    --ssh-host)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SSH_HOST=$2; shift 2 ;;
    --ssh-port)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SSH_PORT=$2; shift 2 ;;
    --ssh-pass)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SSH_PASS=$2; shift 2 ;;
    --ssh-key)      [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SSH_KEY=$2; shift 2 ;;
    --server-host)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SERVER_HOST=$2; shift 2 ;;
    --server-port)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SERVER_PORT=$2; shift 2 ;;
    --stun-port)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    STUN_PORT=$2; shift 2 ;;
    --registry-id)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    REGISTRY_ID=$2; shift 2 ;;
    --remote-cli)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    REMOTE_CLI=$2; shift 2 ;;
    --remote-server) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    REMOTE_SERVER=$2; shift 2 ;;
    --remote-port)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    REMOTE_PORT=$2; shift 2 ;;
    --local-port)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    LOCAL_PORT=$2; shift 2 ;;
    --size-mb)      [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SIZE_MB=$2; shift 2 ;;
    --timeout)      [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    SEND_TIMEOUT=$2; shift 2 ;;
    --binary)       [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                    AK_BIN=$2; shift 2 ;;
    --skip-doctor)  SKIP_DOCTOR=1; shift ;;
    --keep)         KEEP=1; shift ;;
    --self-test)    SELF_TEST=1; shift ;;
    -h|--help)      usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

num_in_range() { # value name lo hi
  case "$1" in ''|*[!0-9]*) echo "$2 must be a number" >&2; exit 2 ;; esac
  if [ "$1" -lt "$3" ] || [ "$1" -gt "$4" ]; then
    echo "$2 out of range ($3..$4)" >&2; exit 2
  fi
}
num_in_range "$SSH_PORT"    --ssh-port    1 65535
num_in_range "$SERVER_PORT" --server-port 1 65535
num_in_range "$STUN_PORT"   --stun-port   1 65535
num_in_range "$REMOTE_PORT" --remote-port 1 65535
num_in_range "$LOCAL_PORT"  --local-port  1 65535
num_in_range "$SIZE_MB"     --size-mb     1 4096
num_in_range "$SEND_TIMEOUT" --timeout    5 86400
if [ -n "$REGISTRY_ID" ]; then
  case "$REGISTRY_ID" in
    *[!0-9a-fA-F]*) echo "--registry-id must be 64 hex characters" >&2; exit 2 ;;
  esac
  [ ${#REGISTRY_ID} -eq 64 ] || { echo "--registry-id must be 64 hex characters" >&2; exit 2; }
  REGISTRY_ID=$(printf '%s' "$REGISTRY_ID" | tr 'A-F' 'a-f')
fi
if [ "$SELF_TEST" -eq 0 ]; then
  missing=""
  [ -n "$SSH_USER" ] || missing="$missing --ssh-user"
  [ -n "$SSH_HOST" ] || missing="$missing --ssh-host"
  if [ -n "$missing" ]; then
    echo "missing required option(s):$missing (see --help)" >&2; exit 2
  fi
fi
[ -n "$SERVER_HOST" ] || SERVER_HOST=$SSH_HOST
[ -n "$REMOTE_SERVER" ] || REMOTE_SERVER="127.0.0.1:$SERVER_PORT"

# ---------------------------------------------------------------------------
# Helpers (structure mirrors scripts/cross_device_test.sh)
# ---------------------------------------------------------------------------
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
step() { printf '[cross-nat] step %s\n' "$*"; }
note() { printf '[cross-nat]   %s\n' "$*"; }
fail() {
  FAILED=1
  printf '[cross-nat] FAIL: %s\n' "$*" >&2
  exit 1
}
WORK=""
LOGS=""
LOCALD_PID=""
RWORK=""
RPID=""
FAILED=0
RESULTS=""

record() { RESULTS="$RESULTS$1:$2
"; }

on_err() {
  FAILED=1
  printf '[cross-nat] FAILED at %s:%s (command: %s)\n' \
    "${BASH_SOURCE[0]}" "$1" "${2:-?}" >&2
}
trap 'on_err "$LINENO" "${BASH_COMMAND:-?}"' ERR

kill_bg() {
  [ -n "$1" ] || return 0
  kill "$1" 2>/dev/null || true
}

cleanup() {
  # Keep the failure scene on any non-zero exit (cross_device_test P2-09).
  local rc=$?
  trap - ERR
  kill_bg "$LOCALD_PID"
  # Remote cleanup first: remote_run needs $PASSFILE, deleted only after.
  if [ -n "$RWORK" ]; then
    if [ "${SSH_BACKEND:-none}" != none ]; then
      [ -n "$RPID" ] && remote_run "kill '$RPID' 2>/dev/null; pkill -f '$RWORK' 2>/dev/null" >/dev/null 2>&1 || true
      if [ "$rc" -ne 0 ] || [ "$FAILED" -eq 1 ] || [ "$KEEP" -eq 1 ]; then
        printf '[cross-nat] remote work dir kept (exit %s): %s\n' "$rc" "$RWORK" >&2
      else
        remote_run "rm -rf '$RWORK'" >/dev/null 2>&1 || true
      fi
    fi
  fi
  rm -f "${PASSFILE:-/nonexistent}" 2>/dev/null || true
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    if [ "$rc" -ne 0 ] || [ "$FAILED" -eq 1 ]; then
      printf '[cross-nat] logs kept for inspection: %s\n' "$LOGS" >&2
      printf '[cross-nat] workspace kept (exit %s): %s\n' "$rc" "$WORK" >&2
    elif [ "$KEEP" -eq 1 ]; then
      printf '[cross-nat] workspace kept (--keep): %s\n' "$WORK"
    else
      rm -rf "$WORK"
    fi
  fi
}
trap cleanup EXIT
trap 'FAILED=1; printf "[cross-nat] interrupted (scene kept)\n" >&2; exit 130' INT TERM

# P2-11 endpoint formatter: bare IPv6 literals get bracketed before ":port".
fmt_endpoint() { # host port -> "host:port"
  local host=$1 port=$2
  case "$host" in
    \[*\]) printf '%s:%s' "$host" "$port" ;;
    *:*)   printf '[%s]:%s' "$host" "$port" ;;
    *)     printf '%s:%s' "$host" "$port" ;;
  esac
}

locate_binary() {
  if [ -z "$AK_BIN" ]; then
    local candidate
    for candidate in "$ROOT/target/release/ak-cli.exe" \
                     "$ROOT/target/debug/ak-cli.exe" \
                     "$ROOT/target/release/ak-cli" \
                     "$ROOT/target/debug/ak-cli"; do
      if [ -x "$candidate" ]; then AK_BIN=$candidate; break; fi
    done
  fi
  if [ -z "$AK_BIN" ]; then
    AK_BIN=$(command -v ak-cli || command -v ak-cli.exe || true)
  fi
  [ -n "$AK_BIN" ] || fail "no ak-cli binary found (build with: cargo build --release -p ak-cli, or pass --binary <path>)"
  [ -x "$AK_BIN" ] || fail "binary not executable: $AK_BIN"
}

# ---------------------------------------------------------------------------
# SSH backend (key ssh > sshpass > paramiko), helper, runners
# ---------------------------------------------------------------------------
SSH_BACKEND=none
PYTHON=""
PASSFILE=""

detect_ssh_backend() {
  PYTHON=$(command -v python || command -v python3 || true)
  local key_args_ok=0
  local key_opts=""
  [ -n "$SSH_KEY" ] && key_opts="-i $SSH_KEY"
  if command -v ssh >/dev/null 2>&1; then
    if [ -z "$SSH_PASS" ]; then
      key_args_ok=1
    elif ssh -p "$SSH_PORT" $key_opts -o BatchMode=yes \
         -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new \
         "$SSH_USER@$SSH_HOST" true >/dev/null 2>&1; then
      key_args_ok=1
    fi
  fi
  if [ "$key_args_ok" -eq 1 ]; then
    SSH_BACKEND=openssh
  elif command -v sshpass >/dev/null 2>&1 && [ -n "$SSH_PASS" ]; then
    SSH_BACKEND=sshpass
  elif [ -n "$PYTHON" ] && "$PYTHON" -c 'import paramiko' >/dev/null 2>&1 \
       && [ -n "$SSH_PASS" ]; then
    SSH_BACKEND=paramiko
    write_ssh_helper
  fi
}

write_ssh_helper() {
  cat > "$WORK/ssh_helper.py" <<'PYEOF'
#!/usr/bin/env python3
"""SSH runner for scripts/cross_nat_test.sh (paramiko backend).

Usage: ssh_helper.py <host> <port> <user> <passfile>
The command is read from stdin. Exit status = remote exit status
(254 usage error, 255 connection/exec failure).
"""
import os
import sys
import time

import paramiko


def run_command(client, command):
    """Execute `command` and drain BOTH streams (K 复核 K-R3).

    exec_command(..., timeout=0) is NON-blocking in paramiko, so the previous
    "timeout=0 then read immediately" flow raised TimeoutError whenever the
    remote output (or EOF) had not arrived yet. This variant uses blocking
    channel semantics (timeout=None) plus a recv_ready/recv_stderr_ready
    polling loop, so delayed output, empty output and stderr-heavy commands
    all complete. An idle budget (AK_SSH_IDLE_TIMEOUT seconds, default 1800;
    reset on every progress) bounds the wait so a hung remote still fails.
    Keep this function identical in the other script's embedded helper
    (single source, verified by scripts/tests/test_ssh_helper_exec.py).
    """
    _, stdout, stderr = client.exec_command(command, timeout=None)
    channel = stdout.channel
    out = bytearray()
    err = bytearray()
    idle_budget = float(os.environ.get("AK_SSH_IDLE_TIMEOUT", "1800"))
    deadline = time.monotonic() + idle_budget
    while not channel.exit_status_ready():
        drained = False
        while channel.recv_ready():
            chunk = channel.recv(65536)
            if chunk:
                out.extend(chunk)
                drained = True
        while channel.recv_stderr_ready():
            chunk = channel.recv_stderr(65536)
            if chunk:
                err.extend(chunk)
                drained = True
        if drained:
            deadline = time.monotonic() + idle_budget
            continue
        if channel.closed:
            break
        if time.monotonic() > deadline:
            raise TimeoutError(
                f"no output/exit status for {idle_budget:g}s: {command[:80]!r}"
            )
        time.sleep(0.05)
    # Data arrives before the exit status; drain whatever is left.
    while channel.recv_ready():
        out.extend(channel.recv(65536))
    while channel.recv_stderr_ready():
        err.extend(channel.recv_stderr(65536))
    return channel.recv_exit_status(), bytes(out), bytes(err)


def main() -> int:
    if len(sys.argv) != 5:
        print("usage: ssh_helper.py <host> <port> <user> <passfile>", file=sys.stderr)
        return 254
    host, port_s, user, passfile = sys.argv[1:5]
    try:
        port = int(port_s)
    except ValueError:
        print(f"bad port: {port_s}", file=sys.stderr)
        return 254
    try:
        with open(passfile, "r", encoding="utf-8") as fh:
            password = fh.read()
    except OSError as exc:
        print(f"cannot read password file: {exc}", file=sys.stderr)
        return 254
    command = sys.stdin.read()
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        client.connect(
            hostname=host, port=port, username=user, password=password,
            look_for_keys=False, allow_agent=False, timeout=20,
            banner_timeout=20, auth_timeout=20,
        )
        transport = client.get_transport()
        if transport is not None:
            transport.set_keepalive(30)
        status, out, err = run_command(client, command)
        sys.stdout.buffer.write(out)
        sys.stdout.buffer.flush()
        sys.stderr.buffer.write(err)
        sys.stderr.buffer.flush()
        return status
    except Exception as exc:  # noqa: BLE001 - reported verbatim to the caller
        print(f"[ssh-helper] exec failed: {exc}", file=sys.stderr)
        return 255
    finally:
        client.close()


if __name__ == "__main__":
    sys.exit(main())
PYEOF
}

ssh_base_opts() {
  printf '%s\n' \
    -o ConnectTimeout=20 \
    -o StrictHostKeyChecking=accept-new \
    -o LogLevel=ERROR
  [ -z "$SSH_PASS" ] && printf '%s\n' -o BatchMode=yes
  [ -n "$SSH_KEY" ] && printf '%s\n' -i "$SSH_KEY"
}

remote_run() { # command-string; stdout = remote stdout; exit = remote status
  case "$SSH_BACKEND" in
    openssh)
      # shellcheck disable=SC2046
      ssh -p "$SSH_PORT" $(ssh_base_opts) "$SSH_USER@$SSH_HOST" "$1"
      ;;
    sshpass)
      # shellcheck disable=SC2046
      sshpass -f "$PASSFILE" ssh -p "$SSH_PORT" $(ssh_base_opts) \
        "$SSH_USER@$SSH_HOST" "$1"
      ;;
    paramiko)
      # Command travels via stdin so MSYS path conversion can never rewrite
      # remote /absolute paths; local helper paths still resolve for python.
      "$PYTHON" "$WORK/ssh_helper.py" \
        "$SSH_HOST" "$SSH_PORT" "$SSH_USER" "$PASSFILE" <<< "$1"
      ;;
    *)
      fail "no SSH backend (use key auth, or install sshpass, or: pip install paramiko + --ssh-pass)"
      ;;
  esac
}
remote_run_quiet() { remote_run "$1" 2>>"$LOGS/remote-stderr.log"; }

# ---------------------------------------------------------------------------
# Candidate-address parsing (from cross_device_test.sh: NDJSON candidates
# array first, human address block second; endpoint-shape validated)
# ---------------------------------------------------------------------------
parse_candidates() { # logfile
  local file=$1
  [ -f "$file" ] || return 1
  if [ -n "$PYTHON" ]; then
    if "$PYTHON" - "$file" <<'PYEOF'
import json
import re
import sys

addr_re = re.compile(r"^\[?[0-9A-Fa-f:.]+\]?:[0-9]+$")
found = False
try:
    with open(sys.argv[1], "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line.startswith("{") or '"listening"' not in line:
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("type") != "listening":
                continue
            for cand in event.get("candidates") or []:
                cand = str(cand).strip()
                if addr_re.match(cand):
                    print(cand)
                    found = True
            break
except OSError:
    pass
sys.exit(0 if found else 1)
PYEOF
    then
      return 0
    fi
  fi
  if grep -q '"candidates":\[' "$file" 2>/dev/null; then
    local legacy
    legacy=$(grep -o '"candidates":\[[^]]*\]' "$file" | head -1 \
      | sed -e 's/^"candidates":\[//' -e 's/\]$//' \
      | tr ',' '\n' | tr -d '" ' \
      | grep -E '^\[?[0-9A-Fa-f:.]+\]?:[0-9]+$' || true)
    if [ -n "$legacy" ]; then
      printf '%s\n' "$legacy"
      return 0
    fi
  fi
  if grep -q 'address candidates (try these):' "$file" 2>/dev/null; then
    local human
    human=$(sed -n '/address candidates (try these):/,$p' "$file" \
      | sed -n 's/^  \+\([^ ][^ ]*\) *(.*/\1/p' || true)
    if [ -n "$human" ]; then
      printf '%s\n' "$human"
      return 0
    fi
  fi
  return 1
}

dedup_lines() { awk '!seen[$0]++ { print }'; }
count_lines() { printf '%s\n' "$1" | grep -c . || true; }

wait_for() { # file needle attempts sleep-secs description
  local file=$1 needle=$2 attempts=$3 sleep_secs=$4 what=$5 i
  for ((i = 1; i <= attempts; i++)); do
    if [ -f "$file" ] && grep -q -- "$needle" "$file" 2>/dev/null; then
      return 0
    fi
    sleep "$sleep_secs"
  done
  printf '[cross-nat] timed out waiting for %s (%s in %s)\n' "$what" "$needle" "$file" >&2
  return 1
}

wait_remote_log() { # remotepath needle attempts sleep-secs description
  local rpath=$1 needle=$2 attempts=$3 sleep_secs=$4 what=$5 i
  for ((i = 1; i <= attempts; i++)); do
    remote_run "cat '$rpath'" > "$LOGS/remote-poll.log" 2>/dev/null || true
    if grep -q -- "$needle" "$LOGS/remote-poll.log" 2>/dev/null; then
      return 0
    fi
    sleep "$sleep_secs"
  done
  printf '[cross-nat] timed out waiting for %s (%s in %s)\n' "$what" "$needle" "$rpath" >&2
  return 1
}

sha256_of() { sha256sum "$1" | cut -d' ' -f1 | tr 'A-F' 'a-f'; }

make_workspace() {
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-crossnat.XXXXXXXX")
  LOGS="$WORK/logs"
  mkdir -p "$LOGS"
}

# ---------------------------------------------------------------------------
# Run summary (K 复核 STUN-label)
# ---------------------------------------------------------------------------
# The summary must state what actually happened: a run with --skip-doctor
# never executed the STUN gate, so the evidence line labels it SKIPPED —
# never "passed". print_summary renders the recorded per-step results
# (PASS/FAIL/SKIPPED) plus the final RESULT line from the real flags.
stun_gate_label() {
  if [ "$SKIP_DOCTOR" -eq 1 ]; then
    printf '%s\n' 'SKIPPED (--skip-doctor)'
  else
    printf '%s\n' 'ok'
  fi
}

print_summary() {
  local line name status overall=PASS
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    name=${line%%:*}; status=${line##*:}
    printf '[cross-nat]   %-42s %s\n' "$name" "$status"
    case "$status" in
      PASS|SKIPPED) ;;
      *) overall=FAIL ;;
    esac
  done <<< "$RESULTS"
  if [ "$overall" = PASS ]; then
    note "RESULT: PASS (registry pairing + STUN gate $(stun_gate_label) + real-NAT direct QUIC transfer sha256-verified)"
    return 0
  fi
  FAILED=1
  note "RESULT: FAIL (workspace and remote work dir kept for debugging)" >&2
  return 1
}

# ---------------------------------------------------------------------------
# --self-test: offline gate (no server, no SSH, no transfer)
# ---------------------------------------------------------------------------
self_test() {
  local sub

  step "s1/9 argument parsing + validation"
  sub=0; bash "$0" --self-test --definitely-not-an-option >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "unknown option should exit 2, got $sub"
  sub=0; bash "$0" --self-test --size-mb abc >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "non-numeric --size-mb should exit 2, got $sub"
  sub=0; bash "$0" --self-test --registry-id deadbeef >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "short --registry-id should exit 2, got $sub"
  sub=0; bash "$0" >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "missing required options should exit 2, got $sub"
  sub=0; bash "$0" --help >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 0 ] || fail "--help should exit 0, got $sub"
  note "5 validation sub-cases behaved"

  step "s2/9 locate ak-cli + version"
  locate_binary
  note "binary: $AK_BIN"
  note "version: $("$AK_BIN" --version 2>&1)"

  step "s3/9 identity generation + fingerprint parse"
  mkdir -p "$WORK/id"
  "$AK_BIN" --id-dir "$WORK/id" keygen > "$LOGS/keygen.log" 2>&1
  local fp
  fp=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen.log" | head -1)
  [ ${#fp} -eq 64 ] || fail "keygen fingerprint missing (see $LOGS/keygen.log)"
  note "fingerprint parsed: ${fp:0:16}... (64 hex ok)"

  step "s4/9 payload creation + sha256"
  local bytes=$((SIZE_MB * 1024 * 1024)) digest
  head -c "$bytes" /dev/urandom > "$WORK/payload.bin"
  [ "$(wc -c < "$WORK/payload.bin" | tr -d ' ')" -eq "$bytes" ] \
    || fail "payload has wrong size (expected $bytes bytes)"
  digest=$(sha256_of "$WORK/payload.bin")
  [ ${#digest} -eq 64 ] || fail "sha256 digest malformed: $digest"
  note "$WORK/payload.bin: $SIZE_MB MiB random, sha256 ${digest:0:16}... (64 hex ok)"

  step "s5/9 candidate/invite parsers against fixtures"
  printf '%s\n' \
    '{"type":"listening","addr":"0.0.0.0:53112","port":53112,"fingerprint":"ab","name":"n","mdns":true,"candidates":["103.236.80.135:53112", "192.168.1.100:53112", "[2001:db8::a]:53112"]}' \
    > "$LOGS/fix-ndjson.log"
  local cands
  cands=$(parse_candidates "$LOGS/fix-ndjson.log" | dedup_lines)
  [ "$(count_lines "$cands")" -eq 3 ] || fail "NDJSON fixture should yield 3 candidates, got: $(printf '%s' "$cands" | tr '\n' ' ')"
  printf '%s\n' "$cands" | grep -qx '\[2001:db8::a\]:53112' \
    || fail "NDJSON fixture lost the IPv6 candidate"
  note "NDJSON listening event: 3/3 candidates parsed (incl. IPv6)"
  printf '%s\n' 'invite code (bearer credential - treat like a password):' '  AKI1-5CBB-1BVX-C0FB-VCZA-C14A-2ZA5-K9KK-BWZF' 'code id 3ae44b58 revision 1' \
    > "$LOGS/invite-fixture.log"
  local code
  code=$(grep -o 'AKI1-[0-9A-Z-]*' "$LOGS/invite-fixture.log" | head -1)
  [ "${code:0:5}" = "AKI1-" ] || fail "invite code fixture parse failed: '$code'"
  note "invite code fixture parsed: $code"
  printf '%s\n' 'paired with redeemer 705f67e7338c8de413b563d482c282bf0dfc007c1c142f6b369e6c0c7aaa8bc5' \
    > "$LOGS/redeemer-fixture.log"
  local rfp
  rfp=$(sed -n 's/^paired with redeemer \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/redeemer-fixture.log" | head -1)
  [ ${#rfp} -eq 64 ] || fail "paired-with-redeemer fixture parse failed"
  note "paired-with-redeemer line: 64-hex fingerprint parsed"
  [ "$(fmt_endpoint 1.2.3.4 7300)" = "1.2.3.4:7300" ] \
    || fail "fmt_endpoint mangled an IPv4 endpoint"
  [ "$(fmt_endpoint 2001:db8::a 7300)" = "[2001:db8::a]:7300" ] \
    || fail "fmt_endpoint did not bracket a bare IPv6 literal"
  note "fmt_endpoint: IPv4 passthrough / IPv6 bracket"
  local doctor_line
  doctor_line='stun    ok      查询成功：本机 192.168.1.5:54321，公网映射地址 203.0.113.9:54321'
  printf '%s\n' "$doctor_line" | grep -q '查询成功' \
    || fail "doctor success-line fixture did not match"
  local mapped
  mapped=$(printf '%s\n' "$doctor_line" | sed -n 's/.*公网映射地址 \(\[*[0-9A-Fa-f:.]*\]*:[0-9]*\).*/\1/p')
  [ "$mapped" = "203.0.113.9:54321" ] || fail "reflexive-address parse failed: '$mapped'"
  note "doctor reflexive-address parse: $mapped"

  step "s6/9 ssh backend detection + helper compile"
  detect_ssh_backend
  case "$SSH_BACKEND" in
    openssh)  note "ssh backend: openssh (key auth)" ;;
    sshpass)  note "ssh backend: sshpass + ssh" ;;
    paramiko)
      note "ssh backend: python ($PYTHON) + paramiko"
      "$PYTHON" -m py_compile "$WORK/ssh_helper.py" 2>/dev/null \
        || fail "paramiko helper does not compile: $WORK/ssh_helper.py"
      note "ssh_helper.py compiles clean"
      ;;
    none)
      printf '[cross-nat]   WARNING: no SSH backend (key/sshpass/paramiko); real runs need one\n' >&2
      ;;
  esac

  step "s7/9 failure-scene retention"
  local sub_rc=0 sub_out
  sub_out=$(bash "$0" --self-test --binary /nonexistent/ak-cli 2>&1) || sub_rc=$?
  [ "$sub_rc" -ne 0 ] || fail "bad --binary should exit non-zero"
  printf '%s' "$sub_out" | grep -q 'workspace kept' \
    || fail "non-zero exit must keep the workspace; tail: $(printf '%s' "$sub_out" | tail -2 | tr '\n' ' ')"
  note "nested failing run (exit $sub_rc): workspace kept as required"

  step "s8/9 summary honesty: STUN label follows --skip-doctor (K 复核 STUN-label)"
  local lbl sum_rc=0
  SKIP_DOCTOR=1
  lbl=$(stun_gate_label)
  [ "$lbl" = "SKIPPED (--skip-doctor)" ] || fail "skip label wrong: '$lbl'"
  RESULTS=$'0/6 preflight:PASS\n1/6 NAT gate (stun):SKIPPED\n2/6 local identity + invite code:PASS\n3/6 remote receiver paired + listening:PASS\n4/6 candidate extraction:PASS\n5/6 Windows -> Linux transfer:PASS\n'
  print_summary > "$LOGS/summary-skip.log" 2>&1 || sum_rc=$?
  [ "$sum_rc" -eq 0 ] || fail "an all-pass run with a skipped STUN gate must still be PASS overall"
  grep -q '1/6 NAT gate (stun) *SKIPPED' "$LOGS/summary-skip.log" \
    || fail "the summary table must show the STUN gate as SKIPPED"
  grep -q 'STUN gate SKIPPED (--skip-doctor)' "$LOGS/summary-skip.log" \
    || fail "the RESULT line must label the STUN gate SKIPPED"
  grep -q 'STUN gate ok' "$LOGS/summary-skip.log" \
    && fail "a skipped run must not claim 'STUN gate ok'"
  SKIP_DOCTOR=0
  lbl=$(stun_gate_label)
  [ "$lbl" = "ok" ] || fail "run label wrong: '$lbl'"
  RESULTS=$'0/6 preflight:PASS\n1/6 NAT gate (stun):PASS\n2/6 local identity + invite code:PASS\n3/6 remote receiver paired + listening:PASS\n4/6 candidate extraction:PASS\n5/6 Windows -> Linux transfer:PASS\n'
  print_summary > "$LOGS/summary-run.log" 2>&1 || sum_rc=$?
  [ "$sum_rc" -eq 0 ] || fail "the doctor-run summary must be PASS"
  grep -q 'STUN gate ok' "$LOGS/summary-run.log" \
    || fail "the run RESULT line must label the STUN gate ok"
  grep -q 'SKIPPED' "$LOGS/summary-run.log" \
    && fail "a doctor run must not mention SKIPPED"
  RESULTS=""
  note "summary labels: skip -> 'SKIPPED (--skip-doctor)', run -> 'ok' (table + RESULT line)"

  step "s9/9 ssh helper drains a fake channel (K 复核 K-R3)"
  # Same fake-paramiko harness as scripts/deploy_linux.sh s7: the channel
  # only replaces the network; output/exit status appear AFTER a delay and
  # timeout=0 reads raise TimeoutError like paramiko's non-blocking mode.
  # The OLD helper (exec_command(timeout=0) + immediate read) exits 255 on
  # the delayed case; the current helper must return the remote status and
  # drain both streams; an empty-output command must complete cleanly.
  if [ -n "$PYTHON" ]; then
    mkdir -p "$WORK/fakepkg"
    cat > "$WORK/fakepkg/paramiko.py" <<'FAKEPY'
import os
import time

DELAY = float(os.environ.get("AK_FAKE_DELAY", "0"))
EMPTY = os.environ.get("AK_FAKE_EMPTY", "") == "1"
STATUS = int(os.environ.get("AK_FAKE_STATUS", "0"))


class AutoAddPolicy:
    pass


class _FakeChannel:
    def __init__(self):
        self.timeout = None
        self.closed = False
        self._start = time.monotonic()
        self._out = [] if EMPTY else [b"fake-stdout-after-delay\n"]
        self._err = [] if EMPTY else [b"fake-stderr-after-delay\n"]

    def _ready(self):
        return (time.monotonic() - self._start) >= DELAY

    def settimeout(self, value):
        self.timeout = value

    def recv_ready(self):
        return self._ready() and bool(self._out)

    def recv_stderr_ready(self):
        return self._ready() and bool(self._err)

    def exit_status_ready(self):
        return self._ready() and not self._out and not self._err

    def recv(self, size):
        while True:
            if self._out and self._ready():
                return self._out.pop(0)
            if self._ready():
                return b""
            if self.timeout == 0:
                raise TimeoutError("non-blocking recv with no data (paramiko timeout=0)")
            time.sleep(0.02)

    def recv_stderr(self, size):
        while True:
            if self._err and self._ready():
                return self._err.pop(0)
            if self._ready():
                return b""
            if self.timeout == 0:
                raise TimeoutError("non-blocking recv_stderr with no data (paramiko timeout=0)")
            time.sleep(0.02)

    def recv_exit_status(self):
        while not self._ready():
            time.sleep(0.02)
        return STATUS


class _FakeFile:
    def __init__(self, channel, is_stderr):
        self.channel = channel
        self._stderr = is_stderr

    def read(self):
        buffer = bytearray()
        while True:
            if self._stderr:
                data = self.channel.recv_stderr(65536)
            else:
                data = self.channel.recv(65536)
            if not data:
                break
            buffer.extend(data)
        return bytes(buffer)


class SSHClient:
    def set_missing_host_key_policy(self, policy):
        pass

    def connect(self, **kwargs):
        pass

    def get_transport(self):
        return None

    def exec_command(self, command, timeout=None):
        channel = _FakeChannel()
        channel.settimeout(timeout)
        return None, _FakeFile(channel, False), _FakeFile(channel, True)

    def close(self):
        pass
FAKEPY
    printf 'dummy\n' > "$WORK/fakepass"
    if command -v cygpath >/dev/null 2>&1; then
      # Windows Python cannot resolve MSYS /tmp paths inside PYTHONPATH.
      FAKEPKG=$(cygpath -w "$WORK/fakepkg")
    else
      FAKEPKG=$WORK/fakepkg
    fi
    local sub out
    sub=0
    out=$(PYTHONPATH="$FAKEPKG" AK_FAKE_DELAY=0.4 AK_FAKE_STATUS=7 \
      "$PYTHON" "$WORK/ssh_helper.py" 127.0.0.1 22 ubuntu "$WORK/fakepass" \
      <<< 'true' 2> "$LOGS/fake-exec.err") || sub=$?
    [ "$sub" -eq 7 ] || fail "fake-channel exec must return the remote status (got $sub; stderr: $(tail -1 "$LOGS/fake-exec.err" 2>/dev/null))"
    [ "$out" = "fake-stdout-after-delay" ] || fail "fake-channel exec lost the delayed stdout (got: '$out')"
    grep -q 'fake-stderr-after-delay' "$LOGS/fake-exec.err" \
      || fail "fake-channel exec did not drain the delayed stderr"
    sub=0
    out=$(PYTHONPATH="$FAKEPKG" AK_FAKE_DELAY=0.3 AK_FAKE_EMPTY=1 \
      "$PYTHON" "$WORK/ssh_helper.py" 127.0.0.1 22 ubuntu "$WORK/fakepass" \
      <<< 'true' 2> /dev/null) || sub=$?
    [ "$sub" -eq 0 ] || fail "empty-output command must complete cleanly through the helper (got $sub)"
    [ -z "$out" ] || fail "empty-output command must produce no stdout (got: '$out')"
    note "delayed (exit 7) + empty-output commands completed via the embedded helper"
  else
    printf '[cross-nat]   WARNING: no python; helper drain harness skipped\n' >&2
  fi

  note "PASS: self-test (no server contacted, no SSH, no transfer)"
  exit 0
}

# ---------------------------------------------------------------------------
# Real flow
# ---------------------------------------------------------------------------
real_run() {
  local LOCAL_ID="$WORK/local-id" LOCAL_OUT="$WORK/local-out"
  local LOCAL_FP="" REMOTE_FP="" CODE="" REGISTRY_ADDR=""
  local REMOTE_CANDS=""

  # -- step 0: preflight -------------------------------------------------------
  step "0/6 preflight (binary, backend, remote cli, registry pin)"
  locate_binary
  note "local cli: $AK_BIN ($("$AK_BIN" --version 2>&1))"
  detect_ssh_backend
  [ "$SSH_BACKEND" != none ] \
    || fail "no SSH backend: use key auth, or install sshpass, or pip install paramiko"
  umask 077
  PASSFILE="$WORK/.sshpass"
  [ -n "$SSH_PASS" ] && printf '%s' "$SSH_PASS" > "$PASSFILE"
  case "$SSH_BACKEND" in
    openssh)  note "ssh backend: openssh -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
    sshpass)  note "ssh backend: sshpass -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
    paramiko) note "ssh backend: paramiko -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
  esac
  REGISTRY_ADDR=$(fmt_endpoint "$SERVER_HOST" "$SERVER_PORT")
  note "registry (local view): $REGISTRY_ADDR"
  remote_run_quiet "test -x '$REMOTE_CLI'" \
    || fail "remote cli missing or not executable: $REMOTE_CLI (run scripts/deploy_linux.sh first; if SSH itself failed, see $LOGS/remote-stderr.log)"
  remote_run_quiet "'$REMOTE_CLI' --version" > "$LOGS/remote-version.log" 2>&1 \
    || fail "remote cli --version failed (see $LOGS/remote-version.log)"
  note "remote cli: $(tail -1 "$LOGS/remote-version.log")"
  remote_run_quiet "command -v sha256sum" | grep -q . \
    || fail "remote is missing sha256sum"
  if [ -z "$REGISTRY_ID" ]; then
    REGISTRY_ID=$(remote_run_quiet "cat /opt/ak-sending/registry-id 2>/dev/null" | tr -d '\r\n ' || true)
    if [ ${#REGISTRY_ID} -eq 64 ]; then
      note "registry pin fetched from /opt/ak-sending/registry-id: ${REGISTRY_ID:0:16}..."
    else
      REGISTRY_ID=$DOCUMENTED_PIN
      printf '[cross-nat]   WARNING: /opt/ak-sending/registry-id not found; using the pin documented in OPERATIONS §3\n' >&2
    fi
  fi
  [ ${#REGISTRY_ID} -eq 64 ] || fail "registry pin malformed: '$REGISTRY_ID'"
  note "registry pin: ${REGISTRY_ID:0:16}... (64 hex)"
  record "0/6 preflight" PASS

  # -- step 1: NAT gate (doctor STUN = §0 step-3 external verification) -------
  step "1/6 NAT gate: doctor --stun $SERVER_HOST:$STUN_PORT"
  if [ "$SKIP_DOCTOR" -eq 1 ]; then
    note "skipped (--skip-doctor)"
    # K 复核 STUN-label: the run summary must not claim a passed STUN gate.
    record "1/6 NAT gate (stun)" SKIPPED
  else
    "$AK_BIN" doctor --stun "$(fmt_endpoint "$SERVER_HOST" "$STUN_PORT")" \
      > "$LOGS/doctor.log" 2>&1 || true
    if ! grep -q '查询成功' "$LOGS/doctor.log"; then
      record "1/6 NAT gate (stun)" FAIL
      printf '[cross-nat]   doctor stun output (see $LOGS/doctor.log):\n' >&2
      grep -A2 'stun' "$LOGS/doctor.log" | sed 's/^/[cross-nat]     /' >&2 || true
      fail "stun-server $SERVER_HOST:$STUN_PORT did not answer a Binding request through the real internet — check UDP $STUN_PORT in the cloud security group / ufw, and 'journalctl -u ak-stun-server' on the server"
    fi
    local reflexive
    reflexive=$(sed -n 's/.*公网映射地址 \(\[*[0-9A-Fa-f:.]*\]*:[0-9]*\).*/\1/p' "$LOGS/doctor.log" | head -1)
    note "stun ok; this machine's public reflexive address (NAT evidence): ${reflexive:-<unparsed>}"
    record "1/6 NAT gate (stun)" PASS
  fi

  # -- step 2: local identity + owner daemon + invite code --------------------
  step "2/6 local identity + owner daemon + invite code"
  mkdir -p "$LOCAL_ID" "$LOCAL_OUT"
  "$AK_BIN" --id-dir "$LOCAL_ID" keygen > "$LOGS/keygen-local.log" 2>&1
  LOCAL_FP=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen-local.log" | head -1)
  [ ${#LOCAL_FP} -eq 64 ] || fail "local keygen fingerprint missing (see $LOGS/keygen-local.log)"
  note "local fingerprint: $LOCAL_FP"
  # Owner daemon: answers the redeem over the registry control connection.
  # --expect any is required because peers.book is still empty at startup.
  "$AK_BIN" --id-dir "$LOCAL_ID" daemon \
    --pairing --expect any \
    --out "$LOCAL_OUT" --port "$LOCAL_PORT" --json-lines \
    --server "$REGISTRY_ADDR" --registry-fp "$REGISTRY_ID" \
    > "$LOGS/local-daemon.log" 2> "$LOGS/local-daemon.err" &
  LOCALD_PID=$!
  wait_for "$LOGS/local-daemon.err" "pairing service:" 120 0.5 \
    "local daemon pairing service" \
    || fail "local daemon did not start (see $LOGS/local-daemon.err)"
  note "local daemon up (pid $LOCALD_PID, pairing service answering)"
  "$AK_BIN" --id-dir "$LOCAL_ID" invite create \
    --server "$REGISTRY_ADDR" --registry-fp "$REGISTRY_ID" \
    --ttl-secs 1800 --name cross-nat-test \
    > "$LOGS/invite-create.log" 2>&1
  CODE=$(grep -o 'AKI1-[0-9A-Z-]*' "$LOGS/invite-create.log" | head -1)
  [ -n "$CODE" ] || fail "no invite code in create output (see $LOGS/invite-create.log)"
  note "invite code: $CODE"
  record "2/6 local identity + invite code" PASS

  # -- step 3: remote receiver (identity + redeem + resident receive) ---------
  step "3/6 remote receiver on $SSH_HOST (redeem + listen)"
  RWORK=$(remote_run_quiet "mktemp -d /tmp/ak-crossnat.XXXXXXXX" | tr -d '\r\n')
  [ -n "$RWORK" ] || fail "cannot create remote work dir"
  local RID="$RWORK/id" ROUT="$RWORK/out"
  local RLOG="$RWORK/daemon.log" RERR="$RWORK/daemon.err"
  remote_run_quiet "mkdir -p '$RID' '$ROUT'" >/dev/null
  remote_run_quiet "'$REMOTE_CLI' --id-dir '$RID' keygen" > "$LOGS/keygen-remote.log" 2>&1 \
    || fail "remote keygen failed (see $LOGS/keygen-remote.log)"
  local rfp_fallback
  rfp_fallback=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen-remote.log" | head -1)
  note "remote work dir: $RWORK (fingerprint ${rfp_fallback:0:16}...)"
  local rstart
  rstart="nohup '$REMOTE_CLI' --id-dir '$RID' daemon \
--invite-code '$CODE' --out '$ROUT' --port $REMOTE_PORT --json-lines \
--server '$REMOTE_SERVER' --registry-fp '$REGISTRY_ID' \
> '$RLOG' 2> '$RERR' < /dev/null & echo \$!"
  RPID=$(remote_run_quiet "$rstart" | tr -d '\r\n')
  [ -n "$RPID" ] || fail "remote daemon did not launch"
  note "remote daemon launched (pid $RPID), redeeming $CODE"
  wait_for "$LOGS/local-daemon.err" "paired with redeemer " 45 2 \
    "invite redeem (owner side)" \
    || fail "owner daemon never saw the redeem (see $LOGS/local-daemon.err + remote $RERR)"
  REMOTE_FP=$(sed -n 's/^paired with redeemer \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/local-daemon.err" | head -1)
  if [ ${#REMOTE_FP} -ne 64 ] && [ ${#rfp_fallback} -eq 64 ]; then
    REMOTE_FP=$rfp_fallback
  fi
  [ ${#REMOTE_FP} -eq 64 ] || fail "cannot determine remote fingerprint"
  note "paired; remote fingerprint: $REMOTE_FP"
  wait_remote_log "$RLOG" '"type":"listening"' 30 2 "remote daemon listening event" \
    || fail "remote daemon never printed its listening event (see remote $RLOG / $RERR)"
  record "3/6 remote receiver paired + listening" PASS

  # -- step 4: candidate extraction --------------------------------------------
  step "4/6 candidate extraction (LAN first, public entry last)"
  remote_run_quiet "cat '$RLOG'" > "$LOGS/remote-daemon.log" 2>/dev/null || true
  remote_run_quiet "cat '$RERR'" > "$LOGS/remote-daemon.err" 2>/dev/null || true
  REMOTE_CANDS=$(parse_candidates "$LOGS/remote-daemon.log" | dedup_lines || true)
  local ssh_entry
  ssh_entry=$(fmt_endpoint "$SSH_HOST" "$REMOTE_PORT")
  REMOTE_CANDS="${REMOTE_CANDS:+$REMOTE_CANDS
}$ssh_entry"
  REMOTE_CANDS=$(printf '%s\n' "$REMOTE_CANDS" | dedup_lines)
  note "remote candidates (tried in order):"
  printf '%s\n' "$REMOTE_CANDS" | sed 's/^/    /'
  note "the last one is the public entry — from behind this NAT the private"
  note "candidates are expected to fail (3s each) before the public one wins"
  record "4/6 candidate extraction" PASS

  # -- step 5: Windows -> Linux transfer ----------------------------------------
  step "5/6 Windows -> Linux transfer ($SIZE_MB MiB, direct QUIC, sha256 verify)"
  local payload="$WORK/payload.bin" psha="" i=1 cand ok=0 attempt bytes
  bytes=$((SIZE_MB * 1024 * 1024))
  head -c "$bytes" /dev/urandom > "$payload"
  psha=$(sha256_of "$payload")
  note "payload $payload sha256 ${psha:0:16}..."
  for cand in $REMOTE_CANDS; do
    attempt="$LOGS/send-$i.log"
    note "trying remote candidate $i: $cand"
    local rc=0
    "$AK_BIN" --id-dir "$LOCAL_ID" send "$payload" \
      --to "$cand" --fp "$REMOTE_FP" --timeout "$SEND_TIMEOUT" \
      > "$attempt" 2>&1 || rc=$?
    if [ "$rc" -eq 0 ] && grep -q 'complete:' "$attempt"; then
      note "candidate $i OK: $(sed -n 's/^\(job [0-9a-f]* complete:.*\)$/\1/p' "$attempt" | head -1)"
      ok=1; break
    fi
    note "candidate $i failed (rc=$rc, see $(basename "$attempt")); trying next"
    i=$((i + 1))
  done
  if [ "$ok" -ne 1 ]; then
    record "5/6 Windows -> Linux transfer" FAIL
    note "all remote candidates failed (logs: $LOGS/send-*.log; check UDP reachability of $ssh_entry and the receiver's firewall)"
  fi
  wait_remote_log "$RLOG" '"type":"job_done"' 60 2 "remote job_done event" || true
  remote_run_quiet "cat '$RLOG'" > "$LOGS/remote-daemon.log" 2>/dev/null || true
  local rsha="" rname
  rname=$(basename "$payload")
  rsha=$(remote_run_quiet "sha256sum '$ROUT/$rname' 2>/dev/null" | tr -d '\r\n' | cut -d' ' -f1 || true)
  if [ ${#rsha} -eq 64 ] && [ "$rsha" = "$psha" ]; then
    note "sha256 MATCH on Linux: $rsha"
    record "5/6 Windows -> Linux transfer" PASS
  else
    record "5/6 Windows -> Linux transfer" FAIL
    note "sha256 MISMATCH on Linux: sent $psha, got ${rsha:-<missing file>}"
  fi
  local jdone dur=0 bts=0
  jdone=$(grep -h -o '"type":"job_done","id":"[0-9a-f]*","files":[0-9]*,"bytes":[0-9]*,"duration_ms":[0-9]*' \
    "$LOGS/remote-daemon.log" 2>/dev/null | tail -1 || true)
  if [ -n "$jdone" ]; then
    bts=$(printf '%s' "$jdone" | sed -n 's/.*"bytes":\([0-9]*\).*/\1/p')
    dur=$(printf '%s' "$jdone" | sed -n 's/.*"duration_ms":\([0-9]*\).*/\1/p')
    if [ "$dur" -gt 0 ] 2>/dev/null; then
      note "job_done: $bts bytes in $dur ms ($(( bts * 1000 / dur / 1024 / 1024 )) MiB/s effective)"
    fi
  fi

  # -- step 6: summary -----------------------------------------------------------
  step "6/6 summary"
  # K 复核 STUN-label: print_summary derives the STUN-gate wording from the
  # actual --skip-doctor flag (SKIPPED) instead of always claiming a pass.
  if print_summary; then
    exit 0
  fi
  exit 1
}

# ---------------------------------------------------------------------------
# Entry
# ---------------------------------------------------------------------------
if [ "$SELF_TEST" -eq 1 ]; then
  # Synthesize valid coordinates so every code path below the parser runs.
  : "${SSH_USER:=ubuntu}"
  : "${SSH_HOST:=127.0.0.1}"
  : "${SERVER_HOST:=127.0.0.1}"
  : "${REGISTRY_ID:=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef}"
  make_workspace
  detect_ssh_backend   # PYTHON picked up here; write_ssh_helper runs inside
  write_ssh_helper
  self_test
else
  make_workspace
  detect_ssh_backend
  real_run
fi
