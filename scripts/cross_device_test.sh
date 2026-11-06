#!/usr/bin/env bash
# Cross-device (Windows <-> remote Linux) automated transfer test for ak-cli.
#
# Automates the manually verified flow: Windows creates an identity and an
# invite code on the v2 pairing registry, the remote Linux box redeems the
# code (pairing both devices through the registry), then one random payload
# travels each direction over direct QUIC and both ends are sha256-verified.
# The registry (ak-cli registry-server) is expected to already run on the
# remote server; it coordinates pairing only and never carries file bytes.
#
# Usage:
#   bash scripts/cross_device_test.sh --self-test
#   bash scripts/cross_device_test.sh \
#     --server-host 122.51.121.24 --server-port 7200 \
#     --registry-id 8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36 \
#     --ssh-user ubuntu --ssh-host 122.51.121.24 --ssh-pass <password> \
#     --remote-cli /opt/ak-sending/ak-cli \
#     [--size-mb 5] [--keep]
#
# Options:
#   --server-host <ip>     registry host as seen from this Windows machine
#   --server-port <n>      registry TLS port (default 7200)
#   --registry-id <64hex>  registry identity pin (`registry-server` prints it)
#   --ssh-user <name>      SSH login user on the remote Linux box
#   --ssh-host <ip>        SSH host (= where the registry + ak-cli live).
#                          host:remote-port is ALWAYS appended last to the
#                          remote candidate list as the known public entry;
#                          this assumes the SSH host IS the receiving box --
#                          when it is only a jump host, use --remote-data-addr
#   --ssh-port <n>         SSH TCP port (default 22)
#   --ssh-pass <secret>    SSH password (kept only in a chmod-600 temp file;
#                          detected backend: sshpass, or python+paramiko)
#   --remote-cli <path>    ak-cli binary path on the remote (default
#                          /opt/ak-sending/ak-cli)
#   --remote-server <a:p>  registry endpoint as seen from the remote (default
#                          127.0.0.1:<server-port>: the registry runs there)
#   --remote-port <n>      remote daemon UDP port (default 7300)
#   --local-port <n>       local (Windows) daemon UDP port (default 7301)
#   --local-addr <ip:port> address the remote should dial for the reverse
#                          transfer (default: daemon candidates, then the IP
#                          this machine's SSH connection appears to come from)
#   --remote-data-addr <a:p> data-plane address of the remote receiver, for
#                          when --ssh-host is only a jump host AND the
#                          daemon-advertised candidates are all unreachable;
#                          appended last (after the ssh-host candidate);
#                          IPv6 literals must be bracketed ([2001:db8::a]:7300)
#   --size-mb <n>          payload size per direction in MiB (default 5)
#   --timeout <secs>       per-attempt send idle budget (default 60)
#   --binary <path>        local ak-cli (default <repo>/target/release/ak-cli.exe,
#                          then debug, then PATH)
#   --keep                 keep the temporary workspace (also kept on failure)
#   --self-test            offline gate: argument parsing, identity
#                          generation, file creation, parser fixtures; no
#                          server, no SSH, no transfer
#   -h | --help            this text
#
# Flow (each step prints "[cross-device] step N/6"):
#   1. local identity + invite code (registry reached via --server-host)
#   2. remote identity + invite redeem over SSH (registry via --remote-server)
#   3. candidate-address extraction (daemon NDJSON `candidates` array or the
#      human "address candidates" block; multi-address fallback afterwards)
#   4. Windows -> Linux transfer + sha256 verify
#   5. Linux -> Windows transfer + sha256 verify
#   6. PASS/FAIL summary
#
# Exit code 0 = PASS (or self-test PASS). On failure the workspace (and the
# remote work dir) are KEPT and printed. Tooling: Git Bash coreutils; for
# real runs additionally sshpass+ssh or python3 with paramiko
# (pip install paramiko).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
SERVER_HOST=""
SERVER_PORT=7200
REGISTRY_ID=""
SSH_USER=""
SSH_HOST=""
SSH_PORT=22
SSH_PASS=""
REMOTE_CLI=/opt/ak-sending/ak-cli
REMOTE_SERVER=""
REMOTE_PORT=7300
LOCAL_PORT=7301
LOCAL_ADDR=""
REMOTE_DATA_ADDR=""
SIZE_MB=5
SEND_TIMEOUT=60
KEEP=0
SELF_TEST=0
AK_BIN=""

usage() { sed -n '/^# Usage:/,/^#   -h | --help /p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --server-host) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SERVER_HOST=$2; shift 2 ;;
    --server-port) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SERVER_PORT=$2; shift 2 ;;
    --registry-id) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   REGISTRY_ID=$2; shift 2 ;;
    --ssh-user)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SSH_USER=$2; shift 2 ;;
    --ssh-host)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SSH_HOST=$2; shift 2 ;;
    --ssh-port)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SSH_PORT=$2; shift 2 ;;
    --ssh-pass)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SSH_PASS=$2; shift 2 ;;
    --remote-cli)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   REMOTE_CLI=$2; shift 2 ;;
    --remote-server) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   REMOTE_SERVER=$2; shift 2 ;;
    --remote-port) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   REMOTE_PORT=$2; shift 2 ;;
    --local-port)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   LOCAL_PORT=$2; shift 2 ;;
    --local-addr)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   LOCAL_ADDR=$2; shift 2 ;;
    --remote-data-addr) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   REMOTE_DATA_ADDR=$2; shift 2 ;;
    --size-mb)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SIZE_MB=$2; shift 2 ;;
    --timeout)     [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   SEND_TIMEOUT=$2; shift 2 ;;
    --binary)      [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                   AK_BIN=$2; shift 2 ;;
    --keep)        KEEP=1; shift ;;
    --self-test)   SELF_TEST=1; shift ;;
    -h|--help)     usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

num_in_range() { # value name lo hi
  case "$1" in ''|*[!0-9]*) echo "$2 must be a number" >&2; exit 2 ;; esac
  if [ "$1" -lt "$3" ] || [ "$1" -gt "$4" ]; then
    echo "$2 out of range ($3..$4)" >&2; exit 2
  fi
}
num_in_range "$SERVER_PORT" --server-port 1 65535
num_in_range "$SSH_PORT"    --ssh-port    1 65535
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
if [ -n "$LOCAL_ADDR" ]; then
  case "$LOCAL_ADDR" in
    *:*) : ;;
    *) echo "--local-addr must look like ip:port" >&2; exit 2 ;;
  esac
  # A port of 0 is a wildcard only when BINDING; as a dial target it can
  # never succeed, so fail fast instead of wasting a reverse-direction
  # attempt on it (found by a real run passing ip:0 by mistake).
  local_addr_port="${LOCAL_ADDR##*:}"
  case "$local_addr_port" in
    0) echo "--local-addr port must be the local daemon's real UDP port (got 0; default is 7301, override with --local-port)" >&2; exit 2 ;;
  esac
fi
if [ -n "$REMOTE_DATA_ADDR" ]; then
  # Same dial-target shape rules as --local-addr: full endpoint, non-zero
  # port, IPv6 literals bracketed (bare "::1:7300" would mis-split).
  case "$REMOTE_DATA_ADDR" in
    \[*\]:[0-9]*) : ;;
    *:*) : ;;
    *) echo "--remote-data-addr must look like ip:port (IPv6 as [v6]:port)" >&2; exit 2 ;;
  esac
  case "${REMOTE_DATA_ADDR##*:}" in
    0|'') echo "--remote-data-addr port must be non-zero" >&2; exit 2 ;;
  esac
fi
if [ "$SELF_TEST" -eq 0 ]; then
  missing=""
  [ -n "$SERVER_HOST" ]  || missing="$missing --server-host"
  [ -n "$REGISTRY_ID" ]  || missing="$missing --registry-id"
  [ -n "$SSH_USER" ]     || missing="$missing --ssh-user"
  [ -n "$SSH_HOST" ]     || missing="$missing --ssh-host"
  [ -n "$SSH_PASS" ]     || missing="$missing --ssh-pass"
  [ -n "$REMOTE_CLI" ]   || missing="$missing --remote-cli"
  if [ -n "$missing" ]; then
    echo "missing required option(s):$missing (see --help)" >&2; exit 2
  fi
fi
[ -n "$REMOTE_SERVER" ] || REMOTE_SERVER="127.0.0.1:$SERVER_PORT"
# P2-11 fix: single endpoint formatter — a bare IPv6 literal must be
# bracketed before ":port" is appended or Rust's SocketAddr parser (and
# every sed/awk consumer) misreads it. All host:port joins go through
# this helper; inputs already in "[v6]:port" form pass through.
fmt_endpoint() { # host port -> "host:port" (IPv6 literal bracketed)
  local host=$1 port=$2
  case "$host" in
    \[*\]) printf '%s:%s' "$host" "$port" ;;
    *:*)   printf '[%s]:%s' "$host" "$port" ;;
    *)     printf '%s:%s' "$host" "$port" ;;
  esac
}
REGISTRY_ADDR=$(fmt_endpoint "$SERVER_HOST" "$SERVER_PORT")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
step() { printf '[cross-device] step %s\n' "$*"; }
note() { printf '[cross-device]   %s\n' "$*"; }
fail() {
  # Called from `|| fail ...` chains where the ERR trap never fires.
  FAILED=1
  printf '[cross-device] FAIL: %s\n' "$*" >&2
  exit 1
}
WORK=""
LOGS=""
LOCALD_PID=""
RWORK=""
RPID=""
PASSFILE=""
FAILED=0
RESULTS=""      # one "name:STATUS" per flow step, for the summary

record() { RESULTS="$RESULTS$1:$2
"; }

on_err() {
  FAILED=1
  printf '[cross-device] FAILED at %s:%s (command: %s)\n' \
    "${BASH_SOURCE[0]}" "$1" "${2:-?}" >&2
}
trap 'on_err "$LINENO" "${BASH_COMMAND:-?}"' ERR

kill_bg() {
  [ -n "$1" ] || return 0
  kill "$1" 2>/dev/null || true
}

cleanup() {
  # P2-09 fix: decide retention from the ORIGINAL EXIT STATUS captured as
  # the first statement of the trap. The ERR trap does not inherit into
  # functions without `set -o errtrace`, so under `set -e` a failing
  # command inside a function (keygen, invite create, ...) used to exit
  # with FAILED still 0 and the failure scene was deleted as if the run
  # had succeeded. Any non-zero exit — command failure, `fail`, or an
  # interrupt — now keeps every artifact for debugging.
  local rc=$?
  trap - ERR
  kill_bg "$LOCALD_PID"
  # Remote cleanup FIRST: every remote_run needs $PASSFILE, which is
  # deleted only after the remote side is settled.
  if [ -n "$RWORK" ]; then
    if [ -n "$SSH_BACKEND" ] && [ "$SSH_BACKEND" != none ]; then
      [ -n "$RPID" ] && remote_run "kill '$RPID' 2>/dev/null; pkill -f '$RWORK' 2>/dev/null" >/dev/null 2>&1 || true
      if [ "$rc" -ne 0 ] || [ "$FAILED" -eq 1 ] || [ "$KEEP" -eq 1 ]; then
        printf '[cross-device] remote work dir kept (exit %s): %s\n' "$rc" "$RWORK" >&2
      else
        remote_run "rm -rf '$RWORK'" >/dev/null 2>&1 || true
      fi
    fi
  fi
  rm -f "$PASSFILE" 2>/dev/null || true
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    if [ "$rc" -ne 0 ] || [ "$FAILED" -eq 1 ]; then
      printf '[cross-device] logs kept for inspection: %s\n' "$LOGS" >&2
      printf '[cross-device] workspace kept (exit %s): %s\n' "$rc" "$WORK" >&2
    elif [ "$KEEP" -eq 1 ]; then
      printf '[cross-device] workspace kept (--keep): %s\n' "$WORK"
    else
      rm -rf "$WORK"
    fi
  fi
}
trap cleanup EXIT
# P2-09 fix: an interrupt must also keep the failure scene — mark FAILED
# before exiting non-zero so both retention signals agree.
trap 'FAILED=1; printf "[cross-device] interrupted (scene kept)\n" >&2; exit 130' INT TERM

# ---------------------------------------------------------------------------
# Local binary
# ---------------------------------------------------------------------------
locate_binary() {
  if [ -z "$AK_BIN" ]; then
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
  [ -n "$AK_BIN" ] || fail "no ak-cli binary found (build with: cargo build -p ak-cli, or pass --binary <path>)"
  [ -x "$AK_BIN" ] || fail "binary not executable: $AK_BIN"
}

# ---------------------------------------------------------------------------
# SSH backend detection (sshpass preferred, paramiko fallback) + helper
# ---------------------------------------------------------------------------
SSH_BACKEND=none
PYTHON=""

detect_ssh_backend() {
  PYTHON=$(command -v python || command -v python3 || true)
  if command -v sshpass >/dev/null 2>&1 && command -v ssh >/dev/null 2>&1; then
    SSH_BACKEND=sshpass
  elif [ -n "$PYTHON" ] && "$PYTHON" -c 'import paramiko' >/dev/null 2>&1; then
    SSH_BACKEND=paramiko
  else
    SSH_BACKEND=none
  fi
}

# The paramiko runner. The remote command travels over STDIN (never argv) so
# Git Bash's MSYS path conversion can never rewrite remote /absolute paths;
# the password is read from a chmod-600 file, never a process argument.
write_ssh_helper() {
  cat > "$WORK/ssh_helper.py" <<'PYEOF'
#!/usr/bin/env python3
"""SSH runner for scripts/cross_device_test.sh (paramiko backend).

Usage: ssh_helper.py <host> <port> <user> <passfile>
The command is read from stdin. Exit status = remote exit status
(254 usage error, 255 connection/exec failure).
"""
import sys

import paramiko


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
            hostname=host,
            port=port,
            username=user,
            password=password,
            look_for_keys=False,
            allow_agent=False,
            timeout=20,
            banner_timeout=20,
            auth_timeout=20,
        )
    except Exception as exc:  # noqa: BLE001 - reported verbatim to the caller
        print(f"[ssh-helper] connection failed: {exc}", file=sys.stderr)
        return 255
    try:
        _, stdout, stderr = client.exec_command(command)
        out = stdout.read()
        err = stderr.read()
        status = stdout.channel.recv_exit_status()
        sys.stdout.buffer.write(out)
        sys.stdout.buffer.flush()
        sys.stderr.buffer.write(err)
        sys.stderr.buffer.flush()
        return status
    except Exception as exc:  # noqa: BLE001
        print(f"[ssh-helper] exec failed: {exc}", file=sys.stderr)
        return 255
    finally:
        client.close()


if __name__ == "__main__":
    sys.exit(main())
PYEOF
}

remote_run() { # command-string; stdout = remote stdout; exit = remote status
  [ "$SSH_BACKEND" != none ] || fail "no SSH backend (install sshpass, or: pip install paramiko)"
  case "$SSH_BACKEND" in
    sshpass)
      sshpass -f "$PASSFILE" ssh -p "$SSH_PORT" \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o ConnectTimeout=20 -o LogLevel=ERROR \
        "$SSH_USER@$SSH_HOST" "$1"
      ;;
    paramiko)
      # Remote command travels via stdin, so no argv looks like a remote
      # path and MSYS path conversion must stay ON for the local helper
      # paths ($WORK/ssh_helper.py, $PASSFILE) to resolve for native python.
      "$PYTHON" "$WORK/ssh_helper.py" \
        "$SSH_HOST" "$SSH_PORT" "$SSH_USER" "$PASSFILE" <<< "$1"
      ;;
  esac
}
remote_run_quiet() { # capture-stdout variant: remote stderr goes to a debug log
  remote_run "$1" 2>>"$LOGS/remote-stderr.log"
}

# ---------------------------------------------------------------------------
# Candidate-address parsing (step 3 core; exercised by --self-test fixtures)
# ---------------------------------------------------------------------------
# Prints every dialable address the daemon advertised, one per line, in the
# daemon's own order (LAN-first). Understands both the wave-25 NDJSON
# `candidates` array of the `listening` event and the older human
# "address candidates (try these):" block; returns 1 when neither is present.
parse_candidates() { # logfile
  local file=$1
  [ -f "$file" ] || return 1
  # 1) NDJSON listening event with a candidates array (preferred; JSON-aware
  #    so IPv6 candidates like [2001:db8::1]:7100 survive intact).
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
  # 1b) No python: sed fallback for the same array. P2-11 fix: the
  #     `[^]]*` match stops at the FIRST ']' — an IPv6 candidate like
  #     "[2001:db8::a]:7300" would truncate to "[2001:db8::a" and friends.
  #     Instead of trusting "non-empty", every parsed line must pass the
  #     same endpoint-shape check as the python path; anything malformed
  #     is DROPPED, and an all-invalid parse returns failure (caller
  #     falls through to the human block) — never truncated addresses.
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
  # 2) Human text block: "address candidates (try these):" + aligned lines
  #    rendered as "  <addr padded> (label)" by the daemon.
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

dedup_lines() { # stdin -> stdout, order-preserving
  awk '!seen[$0]++ { print }'
}

wait_for() { # file needle attempts sleep-secs description
  local file=$1 needle=$2 attempts=$3 sleep_secs=$4 what=$5 i
  for ((i = 1; i <= attempts; i++)); do
    if [ -f "$file" ] && grep -q -- "$needle" "$file" 2>/dev/null; then
      return 0
    fi
    sleep "$sleep_secs"
  done
  printf '[cross-device] timed out waiting for %s (%s in %s)\n' "$what" "$needle" "$file" >&2
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
  printf '[cross-device] timed out waiting for %s (%s in %s)\n' "$what" "$needle" "$rpath" >&2
  return 1
}

sha256_of() { # file -> digest (lowercase hex)
  sha256sum "$1" | cut -d' ' -f1 | tr 'A-F' 'a-f'
}

count_lines() { printf '%s\n' "$1" | grep -c . || true; }

# ---------------------------------------------------------------------------
# Workspace
# ---------------------------------------------------------------------------
make_workspace() {
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-cross.XXXXXXXX")
  LOGS="$WORK/logs"
  mkdir -p "$LOGS"
}

# ---------------------------------------------------------------------------
# --self-test: no server, no SSH, no transfer. Exercises argument
# validation (via sub-invocations), identity generation, payload creation,
# and the candidate/invite/fingerprint parsers against fixed fixtures.
# ---------------------------------------------------------------------------
self_test() {
  local sub

  step "s1/7 argument parsing + validation"
  sub=0; bash "$0" --self-test --definitely-not-an-option >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "unknown option should exit 2, got $sub"
  sub=0; bash "$0" --self-test --size-mb abc >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "non-numeric --size-mb should exit 2, got $sub"
  sub=0; bash "$0" --self-test --registry-id deadbeef >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "short --registry-id should exit 2, got $sub"
  sub=0; bash "$0" --server-host 1.2.3.4 >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "missing required options should exit 2, got $sub"
  sub=0; bash "$0" --help >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 0 ] || fail "--help should exit 0, got $sub"
  note "5 validation sub-cases behaved (bad option / bad size / short registry id / missing required / --help)"

  step "s2/7 locate ak-cli + version"
  locate_binary
  note "binary: $AK_BIN"
  note "version: $("$AK_BIN" --version 2>&1)"

  step "s3/7 identity generation + fingerprint parse"
  mkdir -p "$WORK/id"
  "$AK_BIN" --id-dir "$WORK/id" keygen > "$LOGS/keygen.log" 2>&1
  local fp
  fp=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen.log" | head -1)
  [ ${#fp} -eq 64 ] || fail "keygen fingerprint missing (see $LOGS/keygen.log)"
  note "fingerprint parsed: ${fp:0:16}... (64 hex ok)"

  step "s4/7 payload file creation + sha256"
  local bytes=$((SIZE_MB * 1024 * 1024)) digest
  head -c "$bytes" /dev/urandom > "$WORK/payload.bin"
  [ "$(wc -c < "$WORK/payload.bin" | tr -d ' ')" -eq "$bytes" ] \
    || fail "payload has wrong size (expected $bytes bytes)"
  digest=$(sha256_of "$WORK/payload.bin")
  [ ${#digest} -eq 64 ] || fail "sha256 digest malformed: $digest"
  case "$digest" in *[!0-9a-f]*) fail "sha256 digest not lowercase hex: $digest" ;; esac
  note "$WORK/payload.bin: $SIZE_MB MiB random, sha256 ${digest:0:16}... (64 hex ok)"
  # invite-code line parse (format fixture; registry not contacted offline)
  printf 'invite code (bearer credential - treat like a password):\n  AKI1-5CBB-1BVX-C0FB-VCZA-C14A-2ZA5-K9KK-BWZF\ncode id 3ae44b58 revision 1\n' \
    > "$LOGS/invite-fixture.log"
  local code
  code=$(grep -o 'AKI1-[0-9A-Z-]*' "$LOGS/invite-fixture.log" | head -1)
  [ "${code:0:5}" = "AKI1-" ] || fail "invite code fixture parse failed: '$code'"
  note "invite code fixture parsed: $code"

  step "s5/7 candidate-address parsing fixtures"
  # Fixture A: wave-25 NDJSON listening event with a candidates array
  # (byte-for-byte the shape daemon.rs renders, including an IPv6 entry).
  printf '%s\n' \
    '{"type":"listening","addr":"0.0.0.0:53112","port":53112,"fingerprint":"ab","name":"n","mdns":true,"candidates":["103.236.80.135:53112", "192.168.1.100:53112", "[2001:db8::a]:53112"]}' \
    > "$LOGS/fix-ndjson.log"
  local cands
  cands=$(parse_candidates "$LOGS/fix-ndjson.log" | dedup_lines)
  [ "$(count_lines "$cands")" -eq 3 ] || fail "NDJSON fixture should yield 3 candidates, got: $(printf '%s' "$cands" | tr '\n' ' ')"
  printf '%s\n' "$cands" | grep -qx '103.236.80.135:53112' || fail "NDJSON fixture missing first candidate"
  printf '%s\n' "$cands" | grep -qx '\[2001:db8::a\]:53112' || fail "NDJSON fixture lost the IPv6 candidate"
  note "NDJSON listening event: 3/3 candidates parsed (incl. IPv6)"
  # Fixture B: legacy NDJSON without a candidates array -> no candidates.
  printf '%s\n' \
    '{"type":"listening","addr":"0.0.0.0:17801","port":17801,"fingerprint":"705f67e7","name":"x","mdns":true}' \
    > "$LOGS/fix-ndjson-legacy.log"
  if parse_candidates "$LOGS/fix-ndjson-legacy.log" > "$LOGS/fix-b.out" 2>/dev/null; then
    fail "legacy NDJSON fixture should report no candidates"
  fi
  note "legacy NDJSON (no candidates field): correctly empty"
  # Fixture C: human text block (pre-json-lines daemon output).
  {
    printf '%s\n' 'daemon listening on 0.0.0.0:7100 into /tmp/x (name ubuntu)'
    printf '%s\n' 'address candidates (try these):'
    printf '%s\n' '  10.0.2.15:7100        (iface eth0, same LAN)'
    printf '%s\n' '  172.17.0.1:7100       (iface docker0)'
    printf '%s\n' 'fingerprint (full): 7e04c8bac7dd9d031c86800b2b33ee4e7c2196a81185f03e68338645aac668e1'
  } > "$LOGS/fix-human.log"
  cands=$(parse_candidates "$LOGS/fix-human.log" | dedup_lines)
  [ "$(count_lines "$cands")" -eq 2 ] || fail "human fixture should yield 2 candidates, got: $(printf '%s' "$cands" | tr '\n' ' ')"
  printf '%s\n' "$cands" | grep -qx '10.0.2.15:7100' || fail "human fixture missing 10.0.2.15:7100"
  note 'human "address candidates" block: 2/2 candidates parsed'
  # Fixture D: paired-redeemer line (remote fingerprint source, step 2).
  printf '%s\n' 'pairing service: answering redeems for codes in /tmp/x/invites.store (registry 127.0.0.1:7200)' \
    > "$LOGS/fix-redeemer.log"
  printf '%s\n' 'paired with redeemer 705f67e7338c8de413b563d482c282bf0dfc007c1c142f6b369e6c0c7aaa8bc5' \
    >> "$LOGS/fix-redeemer.log"
  local rfp
  rfp=$(sed -n 's/^paired with redeemer \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/fix-redeemer.log" | head -1)
  [ ${#rfp} -eq 64 ] || fail "redeemer fingerprint fixture parse failed"
  note "paired-with-redeemer line: 64-hex fingerprint parsed"
  # Fixture E: fmt_endpoint brackets bare IPv6 literals and passes valid
  # forms through unchanged (P2-11).
  [ "$(fmt_endpoint 1.2.3.4 7300)" = "1.2.3.4:7300" ] \
    || fail "fmt_endpoint mangled an IPv4 endpoint"
  [ "$(fmt_endpoint 2001:db8::a 7300)" = "[2001:db8::a]:7300" ] \
    || fail "fmt_endpoint did not bracket a bare IPv6 literal"
  [ "$(fmt_endpoint '[2001:db8::a]' 7300)" = "[2001:db8::a]:7300" ] \
    || fail "fmt_endpoint double-bracketed an already-bracketed literal"
  note "fmt_endpoint: IPv4 passthrough / IPv6 bracket / pre-bracketed passthrough"
  # Fixture F: the no-python sed fallback must DROP truncated entries (the
  # [^]]* match cannot span an IPv6 ']' — P2-11) and keep only valid lines;
  # it must never emit a truncated address as if it parsed fine.
  local py_save="$PYTHON" sed_cands
  PYTHON=""
  sed_cands=$(parse_candidates "$LOGS/fix-ndjson.log" | dedup_lines)
  [ "$(count_lines "$sed_cands")" -eq 2 ] \
    || fail "sed fallback should keep 2 valid IPv4 candidates, got: $(printf '%s' "$sed_cands" | tr '\n' ' ')"
  printf '%s\n' "$sed_cands" | grep -qx '103.236.80.135:53112' \
    || fail "sed fallback lost a valid IPv4 candidate"
  if printf '%s\n' "$sed_cands" | grep -q '2001:db8::a'; then
    fail "sed fallback emitted a truncated IPv6 entry instead of dropping it"
  fi
  PYTHON="$py_save"
  note "sed fallback (no python): truncated IPv6 dropped, 2/2 IPv4 kept"

  step "s6/7 ssh backend detection + helper compile"
  detect_ssh_backend
  case "$SSH_BACKEND" in
    sshpass)   note "ssh backend: sshpass + ssh" ;;
    paramiko)
      note "ssh backend: python ($PYTHON) + paramiko"
      "$PYTHON" -m py_compile "$WORK/ssh_helper.py" 2>/dev/null \
        || fail "paramiko helper does not compile: $WORK/ssh_helper.py"
      note "ssh_helper.py compiles clean"
      ;;
    none)
      printf '[cross-device]   WARNING: no SSH backend (no sshpass, no paramiko); real runs need one\n' >&2
      printf '[cross-device]   install with: pip install paramiko   (or install sshpass)\n' >&2
      ;;
  esac

  step "s7/7 failure-scene retention (P2-09)"
  # A run that dies mid-flight (here: locate_binary rejects --binary) must
  # exit non-zero AND keep its workspace — the EXIT trap decides from the
  # original exit code, not from the ERR trap that never fires inside
  # functions under `set -e` without errtrace.
  local sub_rc=0 sub_out
  sub_out=$(bash "$0" --self-test --binary /nonexistent/ak-cli 2>&1) || sub_rc=$?
  [ "$sub_rc" -ne 0 ] || fail "bad --binary should exit non-zero"
  printf '%s' "$sub_out" | grep -q 'workspace kept' \
    || fail "non-zero exit must keep the workspace (P2-09); tail: $(printf '%s' "$sub_out" | tail -2 | tr '\n' ' ')"
  note "nested failing run (exit $sub_rc): workspace kept as required"

  note "PASS: self-test (no server contacted, no SSH, no transfer)"
  note "workspace (kept only with --keep): $WORK"
  exit 0
}

# ---------------------------------------------------------------------------
# Real flow
# ---------------------------------------------------------------------------
real_run() {
  local LOCAL_ID="$WORK/local-id" LOCAL_OUT="$WORK/local-out" LOCAL_SRC="$WORK/local-src"
  local LOCAL_FP="" REMOTE_FP="" CODE=""
  local REMOTE_CANDS="" LOCAL_CANDS=""

  # -- preflight ------------------------------------------------------------
  step "0/6 preflight (binary, backend, remote tooling)"
  locate_binary
  note "local cli: $AK_BIN ($("$AK_BIN" --version 2>&1))"
  detect_ssh_backend
  [ "$SSH_BACKEND" != none ] \
    || fail "no SSH backend: install sshpass, or pip install paramiko for python"
  umask 077
  PASSFILE="$WORK/.sshpass"
  printf '%s' "$SSH_PASS" > "$PASSFILE"
  write_ssh_helper
  note "ssh backend: $SSH_BACKEND -> $SSH_USER@$SSH_HOST:$SSH_PORT"
  note "registry (local view): $REGISTRY_ADDR, pin ${REGISTRY_ID:0:16}..."
  note "registry (remote view): $REMOTE_SERVER"
  remote_run_quiet "test -x '$REMOTE_CLI'" \
    || fail "remote cli missing or not executable: $REMOTE_CLI (if SSH itself failed, see $LOGS/remote-stderr.log)"
  remote_run_quiet "command -v sha256sum" | grep -q . \
    || fail "remote is missing sha256sum"
  remote_run_quiet "command -v head" | grep -q . \
    || fail "remote is missing head"
  note "remote cli reachable: $REMOTE_CLI; sha256sum/head present"

  # -- step 1: local identity + invite code ---------------------------------
  step "1/6 local identity + invite code (registry $REGISTRY_ADDR)"
  mkdir -p "$LOCAL_ID" "$LOCAL_OUT" "$LOCAL_SRC"
  "$AK_BIN" --id-dir "$LOCAL_ID" keygen > "$LOGS/keygen-local.log" 2>&1
  LOCAL_FP=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen-local.log" | head -1)
  [ ${#LOCAL_FP} -eq 64 ] || fail "local keygen fingerprint missing (see $LOGS/keygen-local.log)"
  note "local fingerprint: $LOCAL_FP"
  # Owner daemon: answers the redeem AND receives the reverse-direction job.
  # --expect any is required because peers.book is still empty at startup
  # (the redeemer lands in it only when it redeems, seconds from now).
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
    --ttl-secs 1800 --name cross-device-test \
    > "$LOGS/invite-create.log" 2>&1
  CODE=$(grep -o 'AKI1-[0-9A-Z-]*' "$LOGS/invite-create.log" | head -1)
  [ -n "$CODE" ] || fail "no invite code in create output (see $LOGS/invite-create.log)"
  note "invite code: $CODE"
  record "1/6 local identity + invite code" PASS

  # -- step 2: remote identity + redeem -------------------------------------
  step "2/6 remote identity + invite redeem (ssh $SSH_USER@$SSH_HOST)"
  RWORK=$(remote_run_quiet "mktemp -d /tmp/ak-cross.XXXXXXXX" | tr -d '\r\n')
  [ -n "$RWORK" ] || fail "cannot create remote work dir"
  local RID="$RWORK/id" ROUT="$RWORK/out" RSRC="$RWORK/src"
  local RLOG="$RWORK/daemon.log" RERR="$RWORK/daemon.err"
  remote_run_quiet "mkdir -p '$RID' '$ROUT' '$RSRC'" >/dev/null
  remote_run_quiet "'$REMOTE_CLI' --id-dir '$RID' keygen" > "$LOGS/keygen-remote.log" 2>&1 \
    || fail "remote keygen failed (see $LOGS/keygen-remote.log)"
  local rfp_fallback
  rfp_fallback=$(sed -n 's/^fingerprint (full): \([0-9a-f]\{64\}\)$/\1/p' "$LOGS/keygen-remote.log" | head -1)
  note "remote work dir: $RWORK (fingerprint ${rfp_fallback:0:16}...)"
  # Redeem + resident receive in one daemon; nohup-detached so it survives
  # the SSH channel closing. Fixed --remote-port so <ssh-host>:<port> stays a
  # valid fallback address in step 3.
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
  note "paired; remote fingerprint (from local daemon log): $REMOTE_FP"
  wait_remote_log "$RLOG" '"type":"listening"' 30 2 "remote daemon listening event" \
    || fail "remote daemon never printed its listening event (see remote $RLOG / $RERR)"
  record "2/6 remote identity + invite redeem" PASS

  # -- step 3: candidate extraction -----------------------------------------
  step "3/6 candidate-address extraction (both directions)"
  # Remote side: pull the daemon log and parse locally (NDJSON candidates
  # array first, human address block second, ssh-host fallback last).
  remote_run_quiet "cat '$RLOG'" > "$LOGS/remote-daemon.log" 2>/dev/null || true
  remote_run_quiet "cat '$RERR'" > "$LOGS/remote-daemon.err" 2>/dev/null || true
  REMOTE_CANDS=$(parse_candidates "$LOGS/remote-daemon.log" | dedup_lines || true)
  [ -n "$REMOTE_CANDS" ] || note "no advertised candidates (older binary)"
  # P2-10 fix: the ssh-host entry point is appended LAST *unconditionally*,
  # not only when advertised candidates are empty — behind a 1:1 NAT the
  # daemon publishes only private IPs and the public ssh entry is then the
  # sole reachable address, which the old empty-only fallback missed.
  # Assumes the ssh host IS the receiving box; when it is only a jump host,
  # pass --remote-data-addr with the receiver's data-plane address.
  local ssh_entry
  ssh_entry=$(fmt_endpoint "$SSH_HOST" "$REMOTE_PORT")
  if [ -n "$REMOTE_DATA_ADDR" ]; then
    REMOTE_CANDS="${REMOTE_CANDS:+$REMOTE_CANDS
}$ssh_entry
$REMOTE_DATA_ADDR"
  else
    REMOTE_CANDS="${REMOTE_CANDS:+$REMOTE_CANDS
}$ssh_entry"
  fi
  REMOTE_CANDS=$(printf '%s\n' "$REMOTE_CANDS" | dedup_lines)
  note "remote candidates (tried in order):"
  printf '%s\n' "$REMOTE_CANDS" | sed 's/^/    /'
  # Local side: daemon candidates, then the address this SSH connection
  # appears to come from (what the remote can dial back), --local-addr first.
  LOCAL_CANDS=$(parse_candidates "$LOGS/local-daemon.log" | dedup_lines || true)
  local ssh_conn client_ip=""
  ssh_conn=$(remote_run_quiet 'printf "%s" "$SSH_CONNECTION"' | tr -d '\r\n' || true)
  [ -n "$ssh_conn" ] && client_ip=$(printf '%s' "$ssh_conn" | cut -d' ' -f1)
  if [ -n "$LOCAL_ADDR" ]; then
    LOCAL_CANDS="$LOCAL_ADDR
$LOCAL_CANDS"
  fi
  if [ -n "$client_ip" ]; then
    LOCAL_CANDS="${LOCAL_CANDS:+$LOCAL_CANDS
}$(fmt_endpoint "$client_ip" "$LOCAL_PORT")"
  fi
  LOCAL_CANDS=$(printf '%s\n' "$LOCAL_CANDS" | dedup_lines)
  [ -n "$LOCAL_CANDS" ] \
    || fail "no local address for the reverse direction: pass --local-addr ip:$LOCAL_PORT"
  note "local candidates as seen from the remote (tried in order):"
  printf '%s\n' "$LOCAL_CANDS" | sed 's/^/    /'
  record "3/6 candidate extraction" PASS

  # -- step 4: Windows -> Linux ---------------------------------------------
  step "4/6 Windows -> Linux transfer ($SIZE_MB MiB, sha256 verify)"
  local w2l="$LOCAL_SRC/cross-win2linux.bin" w2l_sha="" i=1 cand ok=0 attempt
  local bytes=$((SIZE_MB * 1024 * 1024))
  head -c "$bytes" /dev/urandom > "$w2l"
  w2l_sha=$(sha256_of "$w2l")
  note "payload $w2l sha256 ${w2l_sha:0:16}..."
  for cand in $REMOTE_CANDS; do
    attempt="$LOGS/send-w2l-$i.log"
    note "trying remote candidate $i: $cand"
    local rc=0
    "$AK_BIN" --id-dir "$LOCAL_ID" send "$w2l" \
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
    record "4/6 Windows -> Linux transfer" FAIL
    note "all remote candidates failed; continuing to the reverse direction for maximum diagnostics"
  else
    wait_remote_log "$RLOG" '"type":"job_done"' 60 2 "remote job_done event" || true
    local rsha=""
    rsha=$(remote_run_quiet "sha256sum '$ROUT/cross-win2linux.bin' 2>/dev/null" | tr -d '\r\n' | cut -d' ' -f1 || true)
    if [ ${#rsha} -eq 64 ] && [ "$rsha" = "$w2l_sha" ]; then
      record "4/6 Windows -> Linux transfer" PASS
      note "sha256 MATCH on Linux: $rsha"
    else
      record "4/6 Windows -> Linux transfer" FAIL
      note "sha256 MISMATCH on Linux: sent $w2l_sha, got ${rsha:-<missing file>}"
    fi
  fi

  # -- step 5: Linux -> Windows ---------------------------------------------
  step "5/6 Linux -> Windows transfer ($SIZE_MB MiB, sha256 verify)"
  local l2w="$RSRC/cross-linux2win.bin" l2w_sha="" ok=0
  remote_run_quiet "head -c $bytes /dev/urandom > '$l2w'"
  l2w_sha=$(remote_run_quiet "sha256sum '$l2w'" | tr -d '\r\n' | cut -d' ' -f1 || true)
  [ ${#l2w_sha} -eq 64 ] || fail "remote payload/sha256 failed"
  note "remote payload $l2w sha256 ${l2w_sha:0:16}..."
  i=1
  for cand in $LOCAL_CANDS; do
    attempt="$LOGS/send-l2w-$i.log"
    note "trying local candidate $i: $cand"
    local rc=0
    remote_run_quiet "'$REMOTE_CLI' --id-dir '$RID' send '$l2w' \
--to '$cand' --fp '$LOCAL_FP' --timeout $SEND_TIMEOUT" > "$attempt" 2>&1 || rc=$?
    if [ "$rc" -eq 0 ] && grep -q 'complete:' "$attempt"; then
      note "candidate $i OK: $(sed -n 's/^\(job [0-9a-f]* complete:.*\)$/\1/p' "$attempt" | head -1)"
      ok=1; break
    fi
    note "candidate $i failed (rc=$rc, see $(basename "$attempt")); trying next"
    i=$((i + 1))
  done
  if [ "$ok" -ne 1 ]; then
    record "5/6 Linux -> Windows transfer" FAIL
    note "all local candidates failed from the remote side"
  else
    wait_for "$LOGS/local-daemon.log" '"type":"job_done"' 60 2 "local job_done event" || true
    local got="$LOCAL_OUT/cross-linux2win.bin" gsha=""
    if [ -f "$got" ]; then gsha=$(sha256_of "$got"); fi
    if [ ${#gsha} -eq 64 ] && [ "$gsha" = "$l2w_sha" ]; then
      record "5/6 Linux -> Windows transfer" PASS
      note "sha256 MATCH on Windows: $gsha"
    else
      record "5/6 Linux -> Windows transfer" FAIL
      note "sha256 MISMATCH on Windows: sent $l2w_sha, got ${gsha:-<missing file>}"
    fi
  fi

  # -- step 6: summary --------------------------------------------------------
  step "6/6 summary"
  # Re-fetch the remote daemon log so both job_done events make the summary.
  [ -n "$RWORK" ] && remote_run_quiet "cat '$RLOG'" > "$LOGS/remote-daemon.log" 2>/dev/null || true
  local line name status overall=PASS
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    name=${line%%:*}; status=${line##*:}
    printf '[cross-device]   %-42s %s\n' "$name" "$status"
    [ "$status" = PASS ] || overall=FAIL
  done <<< "$RESULTS"
  local jdone
  jdone=$(grep -h -o '"type":"job_done","id":"[0-9a-f]*","files":[0-9]*,"bytes":[0-9]*,"duration_ms":[0-9]*' \
    "$LOGS/local-daemon.log" "$LOGS/remote-daemon.log" 2>/dev/null || true)
  if [ -n "$jdone" ]; then
    note "job_done events:"
    printf '%s\n' "$jdone" | sed 's/^/    /'
  fi
  if [ "$overall" = PASS ]; then
    note "RESULT: PASS (invite pairing + both transfer directions sha256-verified)"
    exit 0
  else
    FAILED=1
    note "RESULT: FAIL (workspace and remote work dir kept for debugging)" >&2
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Entry
# ---------------------------------------------------------------------------
if [ "$SELF_TEST" -eq 1 ]; then
  # Self-test needs no registry values; synthesize valid ones so every code
  # path below the option parser runs unchanged.
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
