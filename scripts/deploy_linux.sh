#!/usr/bin/env bash
# One-click Linux server deployment for AK-sending (registry-server + stun-server).
#
# Deploys the two server components as systemd services onto the Linux
# coordination server. Both are subcommands of the single `ak-cli` binary
# (docs/OPERATIONS.zh-CN.md §0):
#
#   TCP 7200  ak-cli registry-server   v2 pairing registry (TLS + SQLite)
#   UDP 7101  ak-cli stun-server       standalone STUN responder (RFC 5389)
#
# Coordination only — no file relay, ever (ADR-001).
#
# BUILD STRATEGY (read this first): the development host is Windows
# (x86_64-pc-windows-msvc) with no cross C toolchain (x86_64-linux-gnu-gcc),
# no Docker and no WSL. `ring` / `blake3` / `libsqlite3-sys` (bundled) all
# need a target C compiler, so a LOCAL cross build of x86_64-unknown-linux-gnu
# is impossible on this host (verified: cc-rs "failed to find tool
# x86_64-linux-gnu-gcc"; same classification as scripts/check_linux_targets.sh
# BLOCKED). This script therefore BUILDS ON THE SERVER: it uploads a
# `git archive` tarball of the pinned commit and runs the release build there
# (Ubuntu 24.04: apt build-essential + rustup; toolchain pinned by
# rust-toolchain.toml). If you DO have a prebuilt Linux ak-cli (e.g. from a
# Linux machine or CI), pass --binary <path> to skip the server-side build.
#
# Usage:
#   bash scripts/deploy_linux.sh --self-test
#   bash scripts/deploy_linux.sh --check-only --ssh-user ubuntu --ssh-host <ip>
#   bash scripts/deploy_linux.sh \
#     --ssh-user ubuntu --ssh-host <ip> \
#     [--ssh-port 22] [--ssh-pass <password> | --ssh-key <keyfile>] \
#     [--commit HEAD] [--binary <prebuilt-linux-ak-cli>] \
#     [--registry-port 7200] [--stun-port 7101] \
#     [--keep-legacy-signal] [--close-7100] [--keep-build-dir]
#
# Options:
#   --ssh-user <name>     SSH login user (needs passwordless sudo for the
#                         install steps; the cloud "ubuntu" user default)
#   --ssh-host <ip>       server address
#   --ssh-port <n>        SSH TCP port (default 22)
#   --ssh-pass <secret>   SSH password (kept only in a chmod-600 temp file;
#                         backends: plain key ssh > sshpass > paramiko).
#                         Key auth is preferred (CR-P3-05); omit for keys.
#   --ssh-key <path>      explicit identity file for plain ssh/scp
#   --commit <rev>        git revision to archive & build (default HEAD);
#                         the binary version line must show its short sha
#   --binary <path>       prebuilt x86_64-unknown-linux-gnu ak-cli to upload
#                         instead of building on the server
#   --registry-port <n>   registry TLS port (default 7200, frozen v2 port)
#   --stun-port <n>       standalone STUN UDP port (default 7101)
#   --keep-legacy-signal do NOT stop a legacy signal-server that currently
#                         owns UDP <stun-port>; the deploy will abort on the
#                         port conflict instead (§0: two processes cannot
#                         share 7101)
#   --close-7100          additionally close public TCP 7100 in ufw when ufw
#                         is active (§6 recommendation; cloud security group
#                         must be closed in the provider console)
#   --keep-build-dir      keep the remote build directory after success
#   --check-only          inspect + report, change NOTHING (no stop/start,
#                         no files written)
#   --self-test           offline gate: argument parsing, unit templates,
#                         tarball creation; no SSH, no server contact
#   -h | --help           this text
#
# What a full run does (each phase prints "[deploy] phase N/7"):
#   1. preflight: SSH backend, sudo, arch/os, existing-install discovery
#      (units, --id-dir, registry data, UDP <stun-port> occupant). The remote
#      script runs as the NON-ROOT SSH user, so EVERY read-only discovery
#      probe (ss occupancy, /proc pid resolution, unit files, identity dirs)
#      goes through the verified `sudo -n` path; occupancy that cannot be
#      determined aborts BEFORE any switchover (K 复核 K2-R3)
#   2. upload: `git archive` tarball of <commit> to /tmp; with --binary the
#      prebuilt ak-cli goes to a DISTINCT /tmp staging path and must pass
#      version + sha256 + executable checks BEFORE any service is stopped
#      (K 复核 K-R1)
#   3. build ON THE SERVER: apt build deps + rustup if missing +
#      AK_GIT_SHA=<sha> cargo build --locked --release -p ak-cli;
#      version line must contain "git <sha>" or the deploy aborts
#   4. install: backup ak-cli + registry.key + registry.db to
#      /opt/ak-sending/backup-<ts>/, install binary to /opt/ak-sending/ak-cli,
#      system user `akreg`, data dir /opt/ak-sending/data (existing identity
#      and database are preserved, never deleted)
#   5. systemd: ak-registry-server.service + ak-stun-server.service
#      (templates from docs/OPERATIONS.zh-CN.md §0/§3); a legacy
#      signal-server holding UDP <stun-port> is stopped+disabled with a
#      rollback note (§0 migration step 2); TCP 7100 stay-open reminder.
#      The WHOLE switchover (first stop through enable --now) is one
#      protected unit: before anything is stopped the previous binary, unit
#      files and each unit's enabled/active state are saved, and ANY failure
#      restores the runnable old configuration and REPORTS the recovery
#      outcome (K 复核 K2-N1; stop failures are never swallowed, K 复核 K2-R2)
#   6. firewall: ufw allow <registry-port>/tcp + <stun-port>/udp when ufw is
#      active; otherwise prints the cloud security-group checklist
#   7. verify: units active, journal version lines, listening sockets,
#      --version recorded, AND the RUNNING process itself verified per unit:
#      MainPID resolved, its actual executable read via /proc/<PID>/exe
#      (sudo readlink) and compared to the deployed binary — ExecStart is
#      only configuration and is checked separately (K 复核 K-R2/K2-R2);
#      registry id exported to /opt/ak-sending/registry-id
#
# Exit code 0 = success (or self-test PASS).

set -euo pipefail

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
SSH_USER=""
SSH_HOST=""
SSH_PORT=22
SSH_PASS=""
SSH_KEY=""
COMMIT=HEAD
PREBUILT=""
REGISTRY_PORT=7200
STUN_PORT=7101
KEEP_LEGACY_SIGNAL=0
CLOSE_7100=0
KEEP_BUILD_DIR=0
CHECK_ONLY=0
SELF_TEST=0

REMOTE_BASE=/opt/ak-sending            # binary + data + backups live here
REMOTE_DATA="$REMOTE_BASE/data"        # --id-dir: registry.key + registry.db
REMOTE_BIN="$REMOTE_BASE/ak-cli"
SERVICE_USER=akreg
# Registry pin recorded in the repo for the production box (docs §3; public
# hex, not a secret). Used only to REPORT whether a migrated identity matches.
DOCUMENTED_PIN=8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36

usage() { sed -n '/^# Usage:/,/^#   -h | --help /p' "$0"; }
while [ $# -gt 0 ]; do
  case "$1" in
    --ssh-user)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 SSH_USER=$2; shift 2 ;;
    --ssh-host)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 SSH_HOST=$2; shift 2 ;;
    --ssh-port)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 SSH_PORT=$2; shift 2 ;;
    --ssh-pass)  [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 SSH_PASS=$2; shift 2 ;;
    --ssh-key)   [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 SSH_KEY=$2; shift 2 ;;
    --commit)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 COMMIT=$2; shift 2 ;;
    --binary)    [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 PREBUILT=$2; shift 2 ;;
    --registry-port) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 REGISTRY_PORT=$2; shift 2 ;;
    --stun-port) [ $# -ge 2 ] || { echo "option $1 needs a value" >&2; exit 2; }
                 STUN_PORT=$2; shift 2 ;;
    --keep-legacy-signal) KEEP_LEGACY_SIGNAL=1; shift ;;
    --close-7100) CLOSE_7100=1; shift ;;
    --keep-build-dir)     KEEP_BUILD_DIR=1; shift ;;
    --check-only) CHECK_ONLY=1; shift ;;
    --self-test)  SELF_TEST=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

num_in_range() { # value name lo hi
  case "$1" in ''|*[!0-9]*) echo "$2 must be a number" >&2; exit 2 ;; esac
  if [ "$1" -lt "$3" ] || [ "$1" -gt "$4" ]; then
    echo "$2 out of range ($3..$4)" >&2; exit 2
  fi
}
num_in_range "$SSH_PORT"       --ssh-port       1 65535
num_in_range "$REGISTRY_PORT"  --registry-port  1 65535
num_in_range "$STUN_PORT"      --stun-port      1 65535

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
phase() { printf '[deploy] phase %s\n' "$*"; }
note()  { printf '[deploy]   %s\n' "$*"; }
warn()  { printf '[deploy]   WARNING: %s\n' "$*" >&2; }
fail() {
  FAILED=1
  printf '[deploy] FAIL: %s\n' "$*" >&2
  exit 1
}
WORK=""
PASSFILE=""
FAILED=0

on_err() {
  FAILED=1
  printf '[deploy] FAILED at %s:%s (command: %s)\n' \
    "${BASH_SOURCE[0]}" "$1" "${2:-?}" >&2
}
trap 'on_err "$LINENO" "${BASH_COMMAND:-?}"' ERR

cleanup() {
  # Keep the failure scene (logs, phase scripts) on any non-zero exit for
  # debugging; delete only on success (same policy as cross_device_test.sh).
  local rc=$?
  rm -f "$PASSFILE" 2>/dev/null || true
  if [ "$rc" -ne 0 ] || [ "$FAILED" -eq 1 ]; then
    [ -n "$WORK" ] && [ -d "$WORK" ] \
      && printf '[deploy] logs kept for inspection: %s\n' "$WORK" >&2
    return 0
  fi
  [ -n "$WORK" ] && [ -d "$WORK" ] && rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT
trap 'FAILED=1; printf "[deploy] interrupted\n" >&2; exit 130' INT TERM

# ---------------------------------------------------------------------------
# SSH backend: plain key ssh/scp > sshpass > python+paramiko (password).
# Same pattern as scripts/cross_device_test.sh.
# ---------------------------------------------------------------------------
SSH_BACKEND=none
PYTHON=""

detect_ssh_backend() {
  PYTHON=$(command -v python || command -v python3 || true)
  local key_args_ok=0
  local key_opts=""
  [ -n "$SSH_KEY" ] && key_opts="-i $SSH_KEY"
  # A working key (or agent) makes plain openssh the backend even when a
  # password was also given.
  if command -v ssh >/dev/null 2>&1; then
    if [ -z "$SSH_PASS" ]; then
      key_args_ok=1
    elif ssh -p "$SSH_PORT" $key_opts -o BatchMode=yes \
         -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new \
         "$SSH_USER@$SSH_HOST" true >/dev/null 2>&1; then
      key_args_ok=1
    fi
  fi
  if [ "$key_args_ok" -eq 1 ] && command -v scp >/dev/null 2>&1; then
    SSH_BACKEND=openssh
  elif command -v sshpass >/dev/null 2>&1 && command -v scp >/dev/null 2>&1 \
       && [ -n "$SSH_PASS" ]; then
    SSH_BACKEND=sshpass
  elif [ -n "$PYTHON" ] && "$PYTHON" -c 'import paramiko' >/dev/null 2>&1 \
       && [ -n "$SSH_PASS" ]; then
    SSH_BACKEND=paramiko
    write_ssh_helper
  fi
}

# paramiko helper with exec + sftp-put modes. The command travels on STDIN
# (never argv) so MSYS path conversion can never rewrite remote paths; the
# password is read from a chmod-600 file, never a process argument.
write_ssh_helper() {
  cat > "$WORK/ssh_helper.py" <<'PYEOF'
#!/usr/bin/env python3
"""SSH runner for scripts/deploy_linux.sh (paramiko backend).

Usage:
  ssh_helper.py exec <host> <port> <user> <passfile>   (command on stdin)
  ssh_helper.py put  <host> <port> <user> <passfile> <local> <remote>
Exit status = remote exit status (254 usage error, 255 connection failure).
"""
import os
import sys
import time

import paramiko


def connect(argv):
    host, port_s, user, passfile = argv[2:6]
    try:
        port = int(port_s)
    except ValueError:
        print(f"bad port: {port_s}", file=sys.stderr)
        sys.exit(254)
    try:
        with open(passfile, "r", encoding="utf-8") as fh:
            password = fh.read()
    except OSError as exc:
        print(f"cannot read password file: {exc}", file=sys.stderr)
        sys.exit(254)
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        hostname=host, port=port, username=user, password=password,
        look_for_keys=False, allow_agent=False, timeout=20,
        banner_timeout=20, auth_timeout=20,
    )
    transport = client.get_transport()
    if transport is not None:
        transport.set_keepalive(30)
    return client


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
    if len(sys.argv) < 6:
        print(__doc__, file=sys.stderr)
        return 254
    mode = sys.argv[1]
    client = connect(sys.argv)
    try:
        if mode == "exec":
            command = sys.stdin.read()
            status, out, err = run_command(client, command)
            sys.stdout.buffer.write(out)
            sys.stdout.buffer.flush()
            sys.stderr.buffer.write(err)
            sys.stderr.buffer.flush()
            return status
        if mode == "put":
            if len(sys.argv) != 8:
                print(__doc__, file=sys.stderr)
                return 254
            local, remote = sys.argv[6:8]
            sftp = client.open_sftp()
            try:
                sftp.put(local, remote)
            finally:
                sftp.close()
            return 0
        print(f"unknown mode: {mode}", file=sys.stderr)
        return 254
    except Exception as exc:  # noqa: BLE001 - reported verbatim
        print(f"[ssh-helper] {mode} failed: {exc}", file=sys.stderr)
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
  # Without a password the run must fail fast, never hang on a prompt.
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
      "$PYTHON" "$WORK/ssh_helper.py" exec \
        "$SSH_HOST" "$SSH_PORT" "$SSH_USER" "$PASSFILE" <<< "$1"
      ;;
    *)
      fail "no SSH backend (use key auth, or install sshpass, or: pip install paramiko + --ssh-pass)"
      ;;
  esac
}
remote_run_quiet() { remote_run "$1" 2>>"$WORK/remote-stderr.log"; }

remote_put() { # local remote
  case "$SSH_BACKEND" in
    openssh)
      # shellcheck disable=SC2046
      scp -P "$SSH_PORT" $(ssh_base_opts) "$1" "$SSH_USER@$SSH_HOST:$2"
      ;;
    sshpass)
      # shellcheck disable=SC2046
      sshpass -f "$PASSFILE" scp -P "$SSH_PORT" $(ssh_base_opts) \
        "$1" "$SSH_USER@$SSH_HOST:$2"
      ;;
    paramiko)
      "$PYTHON" "$WORK/ssh_helper.py" put \
        "$SSH_HOST" "$SSH_PORT" "$SSH_USER" "$PASSFILE" "$1" "$2"
      ;;
    *)
      fail "no SSH backend for upload"
      ;;
  esac
}

# Run a local script file on the remote host with bash (upload + execute).
# Avoids every nested-quoting hazard: the script travels as a file. The
# remote copy is pid-suffixed (concurrent runs cannot collide) and removed
# afterwards (best effort).
remote_script() { # local-script-path
  local base remote
  base=$(basename "$1")
  remote="/tmp/ak-deploy-${base%.sh}-$$.sh"
  remote_put "$1" "$remote"
  remote_run "bash '$remote'; rc=\$?; rm -f '$remote' 2>/dev/null; exit \$rc"
}

# ---------------------------------------------------------------------------
# Registry ExecStart composition (K 复核 K-R2)
# ---------------------------------------------------------------------------
# Compose the registry unit's ExecStart from an existing one ("" = fresh
# install). The ARGUMENTS (identity/database paths, --port, ...) are preserved
# verbatim, but the EXECUTABLE is always rewritten to the freshly installed
# and verified $REMOTE_BIN. Preserving the whole old line verbatim let a unit
# keep running e.g. /usr/local/bin/ak-cli while the phase-7 version check ran
# the NEW path and the deploy reported a successful upgrade it never made.
compose_registry_exec() { # existing-exec-or-empty
  local old_exe old_args
  if [ -z "$1" ]; then
    printf '%s\n' "$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA"
    return 0
  fi
  old_exe=${1%% *}                 # first token = the old executable
  old_args=${1#"$old_exe"}         # remainder = arguments, kept verbatim
  if [ "$old_exe" != "$REMOTE_BIN" ]; then
    warn "existing unit executable '$old_exe' is replaced by the newly installed $REMOTE_BIN (arguments preserved; K 复核 K-R2)"
  fi
  if printf '%s' "$old_args" | grep -q -- '--id-dir'; then
    printf '%s\n' "$REMOTE_BIN$old_args"
  else
    printf '%s\n' "$REMOTE_BIN$old_args --id-dir $REMOTE_DATA"
  fi
}

# ---------------------------------------------------------------------------
# systemd unit templates (docs/OPERATIONS.zh-CN.md §0/§3). Ports filled at
# write time; the registry unit always pins --id-dir for path determinism.
# ---------------------------------------------------------------------------
registry_unit_body() { # exec-start
  cat <<EOF
[Unit]
Description=AK-sending v2 pairing registry (coordination only, ADR-001)
After=network-online.target
Wants=network-online.target

[Service]
User=$SERVICE_USER
ExecStart=$1
Environment=RUST_LOG=info
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=$REMOTE_DATA
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

stun_unit_body() { # exec-start
  cat <<EOF
[Unit]
Description=AK-sending standalone STUN responder (binding only, ADR-001)
After=network-online.target
Wants=network-online.target

[Service]
User=$SERVICE_USER
ExecStart=$1
Environment=RUST_LOG=info
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

# ---------------------------------------------------------------------------
# --self-test: offline checks — argument validation, unit templates, git
# archive. No SSH, no server contact.
# ---------------------------------------------------------------------------
self_test() {
  local sub

  echo "[self-test] s1/8 argument parsing"
  sub=0; bash "$0" --self-test --nope >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "unknown option should exit 2, got $sub"
  sub=0; bash "$0" --self-test --ssh-port abc >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "bad --ssh-port should exit 2, got $sub"
  sub=0; bash "$0" >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "missing required options should exit 2, got $sub"
  sub=0; bash "$0" --help >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 0 ] || fail "--help should exit 0, got $sub"
  echo "[self-test]   4 validation sub-cases behaved"

  echo "[self-test] s2/8 unit templates"
  local reg_unit stun_unit
  reg_unit=$(registry_unit_body "$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA")
  printf '%s' "$reg_unit" | grep -q "^ExecStart=$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA$" \
    || fail "registry unit ExecStart malformed"
  printf '%s' "$reg_unit" | grep -q '^ReadWritePaths=/opt/ak-sending/data$' \
    || fail "registry unit must set ReadWritePaths to the data dir"
  printf '%s' "$reg_unit" | grep -q '^User=akreg$' \
    || fail "registry unit must run as the dedicated user"
  stun_unit=$(stun_unit_body "$REMOTE_BIN stun-server --port $STUN_PORT")
  printf '%s' "$stun_unit" | grep -q "^ExecStart=$REMOTE_BIN stun-server --port $STUN_PORT$" \
    || fail "stun unit ExecStart malformed"
  if printf '%s' "$stun_unit" | grep -q '^ReadWritePaths'; then
    fail "stun unit must NOT set ReadWritePaths (no data dir; §0 template)"
  fi
  echo "[self-test]   templates match the OPERATIONS §0/§3 shapes"

  echo "[self-test] s3/8 git archive of the pinned commit"
  ( cd "$ROOT" && git archive --format=tar.gz -o "$WORK/selftest-src.tar.gz" HEAD ) \
    || fail "git archive failed (not a git checkout?)"
  [ -s "$WORK/selftest-src.tar.gz" ] || fail "archive is empty"
  local listing
  listing=$(tar -tzf "$WORK/selftest-src.tar.gz")
  grep -qx 'Cargo.toml' <<< "$listing" \
    || fail "archive must contain Cargo.toml at the root"
  grep -qx 'crates/ak-cli/src/main.rs' <<< "$listing" \
    || fail "archive must contain crates/ak-cli"
  echo "[self-test]   $(printf '%s\n' "$listing" | wc -l | tr -d ' ') files, Cargo.toml + crates present"

  echo "[self-test] s4/8 version sha extraction"
  # Global on purpose: write_phase3 embeds it when s6 generates the build
  # script, mirroring how real_run sets it before any phase runs.
  SHORT_SHA=$( cd "$ROOT" && git rev-parse --short HEAD )
  case "$SHORT_SHA" in
    ''|*[!0-9a-f]*) fail "short sha malformed: $SHORT_SHA" ;;
  esac
  echo "[self-test]   pinned sha: $SHORT_SHA"
  [ -n "$PREBUILT" ] && [ ! -f "$PREBUILT" ] \
    && fail "--binary not found: $PREBUILT"

  echo "[self-test] s5/8 ssh helper compiles"
  write_ssh_helper
  PYTHON=$(command -v python || command -v python3 || true)
  if [ -n "$PYTHON" ] && "$PYTHON" -c 'import paramiko' >/dev/null 2>&1; then
    "$PYTHON" -m py_compile "$WORK/ssh_helper.py" \
      || fail "ssh_helper.py does not compile"
    echo "[self-test]   ssh_helper.py compiles clean (paramiko backend available)"
  else
    echo "[self-test]   paramiko not installed locally (password backend would need: pip install paramiko)"
  fi

  echo "[self-test] s6/8 generated remote phase scripts parse"
  local exec_stub="/opt/ak-sending/ak-cli registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA"
  TS=selftest BUILD_DIR=/tmp/ak-deploy-build-selftest PREBUILT_MODE=0 \
    REGISTRY_EXEC="$exec_stub" REG_RW=$REMOTE_DATA \
    LEGACY_UNIT=oldsignal OLD_REGISTRY_UNIT=oldregistry \
    DATA_SOURCE_DIR=/home/selftest/.ak-sending CLOSE_7100=1 \
    write_phase456 "$WORK/p456.sh"
  write_phase1 "$WORK/p1.sh"
  write_phase3 "$WORK/p3.sh" /tmp/ak-deploy-build-selftest
  write_phase7 "$WORK/p7.sh"
  local f
  for f in p1 p3 p456 p7; do
    bash -n "$WORK/$f.sh" || fail "generated $f.sh is not valid bash"
  done
  grep -q '^Description=AK-sending v2 pairing registry' "$WORK/p456.sh" \
    || fail "registry unit body missing from the install script"
  grep -q "^ExecStart=$exec_stub$" "$WORK/p456.sh" \
    || fail "registry ExecStart not baked into the unit text"
  grep -q '^ExecStart=/opt/ak-sending/ak-cli stun-server --port' "$WORK/p456.sh" \
    || fail "stun ExecStart missing from the install script"
  grep -q '^ReadWritePaths=/opt/ak-sending/data$' "$WORK/p456.sh" \
    || fail "registry unit ReadWritePaths missing"
  if grep -q '^ReadWritePaths' <<< "$(sed -n '/tee .*ak-stun-server.service/,/^STUNUNIT$/p' "$WORK/p456.sh")"; then
    fail "stun unit must not carry ReadWritePaths (§0 template)"
  fi
  grep -q 'AK_GIT_SHA' "$WORK/p3.sh" || fail "build script must inject AK_GIT_SHA"
  grep -q 'registry id (pin with --registry-fp)' "$WORK/p7.sh" \
    || fail "verify script must extract the registry pin"
  grep -q 'oldsignal' "$WORK/p456.sh" || fail "legacy signal retire step missing"
  grep -q 'oldregistry' "$WORK/p456.sh" || fail "old registry unit replace step missing"
  # The unit-backup loop must BAKE the discovered names (remote $vars would be
  # unset at loop time and the old unit files would never be backed up).
  grep -q '^SW_UNITS="ak-registry-server ak-stun-server oldregistry oldsignal"$' "$WORK/p456.sh" \
    || fail "SW_UNITS must bake the discovered old/legacy unit names"
  # K 复核 K2-R3: phase1 discovery must run through sudo -n and fail clearly
  # when occupancy cannot be determined (behavioral proof in the Python suite).
  grep -q 'sudo -n ss -ltnp' "$WORK/p1.sh" \
    || fail "phase1 must probe TCP occupancy through sudo -n (K2-R3)"
  grep -q 'sudo -n ss -lunp' "$WORK/p1.sh" \
    || fail "phase1 must probe UDP occupancy through sudo -n (K2-R3)"
  grep -q 'cannot determine TCP listeners' "$WORK/p1.sh" \
    || fail "phase1 must fail clearly when occupancy is undeterminable (K2-R3)"
  # K 复核 K2-R3 (round 4): EVERY privileged discovery probe must refuse on
  # an undeterminable answer, and the cmdline line must be newline-terminated
  # so real_run's anchored parser sees 'old-registry-unit' at column 0.
  grep -q 'cannot read /proc/\$reg_pid/cgroup' "$WORK/p1.sh" \
    || fail "an unreadable occupant cgroup must refuse the preflight (K2-R3)"
  grep -q 'cannot enumerate /home' "$WORK/p1.sh" \
    || fail "a failed home enumeration must refuse the preflight (K2-R3)"
  grep -q 'cannot probe \$d/registry.key' "$WORK/p1.sh" \
    || fail "an undeterminable key/db existence probe must refuse the preflight (K2-R3)"
  grep -q "sed 's/^/  cmdline: /'; printf" "$WORK/p1.sh" \
    || fail "the cmdline line must be terminated so machine fields stay parseable (K2-R3)"
  # K 复核 K2-R3 (round 5): 'cannot access ... Permission denied' must be
  # UNDETERMINABLE (locale pinned), and an identified occupant unit must be
  # locatable across ALL standard unit dirs or the preflight refuses.
  grep -q 'env LC_ALL=C ls -d' "$WORK/p1.sh" \
    || fail "the existence probe must pin the locale so ENOENT matching is deterministic (K2-R3)"
  grep -q 'usr/lib/systemd/system' "$WORK/p1.sh" \
    || fail "occupant unit lookup must cover vendor unit dirs (K2-R3)"
  grep -q 'find_unit_file' "$WORK/p1.sh" \
    || fail "unit discovery (managed AND occupant) must use the all-directory lookup (K2-R3)"
  # K 复核 K2-R3 (round 7): the EFFECTIVE config (drop-ins included, empty
  # 'ExecStart=' clearing) must be derived from systemctl cat's merged view
  # and cross-checked against the running cmdline.
  grep -q 'effective_execstart' "$WORK/p1.sh" \
    || fail "config derivation must use the merged main+drop-in view (K2-R3)"
  grep -q 'reg_args_agree' "$WORK/p1.sh" \
    || fail "the chosen config must be cross-checked against the running cmdline (K2-R3)"
  # K 复核 K2-R3 (round 8): the cross-check must FAIL CLOSED when the
  # cmdline cannot be read, and the compared surface must cover the
  # data/identity flags (--id-dir, --db-path) plus the listen surface.
  grep -q 'unreadable or empty' "$WORK/p1.sh" \
    || fail "an unreadable running cmdline must refuse instead of skipping the cross-check (K2-R3)"
  # K 复核 K2-R3 (round 9): EFFECTIVE values, defaults included — the
  # db-path default must be derived from the effective id-dir, and an
  # unestablishable environment-dependent default must refuse.
  grep -q 'a_db="\$a_id/registry.db"' "$WORK/p1.sh" \
    || fail "an omitted --db-path must compare as <effective id-dir>/registry.db (K2-R3)"
  grep -q 'cannot be established from its arguments' "$WORK/p1.sh" \
    || fail "an environment-dependent default on one side only must refuse (K2-R3)"
  # K 复核 K9-N1: the identified occupant's RUNTIME identity dir must be
  # derived and reported; the migration source binds to it, never to
  # directory-enumeration order.
  grep -q 'runtime-id-dir' "$WORK/p1.sh" \
    || fail "the occupant's runtime identity dir must be derived and reported (K9-N1)"
  grep -q 'to derive the default identity dir' "$WORK/p1.sh" \
    || fail "missing runtime environment evidence must refuse (K9-N1)"
  grep -q 'was identified but its unit file cannot be located' "$WORK/p1.sh" \
    || fail "an identified occupant without locatable config must refuse (K2-R3)"
  # K 复核 K2-N1: the unit writes, daemon-reload and enable --now must sit
  # INSIDE do_switchover's unified recovery window (they used to run bare).
  grep -q "^  sudo -n tee /etc/systemd/system/ak-registry-server.service >/dev/null <<'REGUNIT' || return 1$" "$WORK/p456.sh" \
    || fail "the first unit write must be inside do_switchover's recovery window (K2-N1)"
  grep -q "^  sudo -n tee /etc/systemd/system/ak-stun-server.service >/dev/null <<'STUNUNIT' || return 1$" "$WORK/p456.sh" \
    || fail "the second unit write must be inside do_switchover's recovery window (K2-N1)"
  grep -q 'sudo -n systemctl daemon-reload || return 1' "$WORK/p456.sh" \
    || fail "daemon-reload must be inside do_switchover's recovery window (K2-N1)"
  grep -q 'sudo -n systemctl enable --now ak-registry-server.service ak-stun-server.service || return 1' "$WORK/p456.sh" \
    || fail "enable --now must be inside do_switchover's recovery window (K2-N1)"
  grep -q 'stop_unit ak-registry-server || return 1' "$WORK/p456.sh" \
    || fail "stops must go through the existence-aware stop_unit (K3-N1/K2-R2)"
  grep -q 'nothing to stop (K3-N1)' "$WORK/p456.sh" \
    || fail "a confirmed-absent unit must be skipped, not stopped (K3-N1)"
  grep -q 'systemctl restart "\$u.service"' "$WORK/p456.sh" \
    || fail "recovery must RESTART units left active by a failed switch (K2-N1 partial start)"
  grep -q 'pre-stop backup \$BACKUP/ak-cli' "$WORK/p456.sh" \
    || fail "recovery must fall back to the pre-stop binary backup (K2-N1 rename failure)"
  grep -q 'cannot restore the previous binary' "$WORK/p456.sh" \
    || fail "a failed binary restore must report INCOMPLETE (K2-N1)"
  grep -q 'RECOVERY INCOMPLETE' "$WORK/p456.sh" \
    || fail "a failed recovery must be loudly reported (K2-N1)"
  # K 复核 K3-N1 (round 5/6): systemd v255's standard missing-unit
  # diagnostic must classify as absent — but ONLY when it NAMES the unit;
  # bus errors like 'Failed to connect to bus: No such file or directory'
  # must stay undeterminable.
  grep -q 'No files found for \$1.service' "$WORK/p456.sh" \
    || fail "the v255 missing-unit diagnostic must be recognized as absent, ANCHORED on the unit name (K3-N1)"
  grep -q 'inactive|failed) return 1' "$WORK/p456.sh" \
    || fail "qstate must treat ONLY final states as stopped; transitional states are undeterminable (K2-N1)"
  # K 复核 K3-N1 (round 7): a missing unit FILE with a running process is
  # NOT a fresh install — capture AND stop must refuse.
  grep -q 'absent but its run state' "$WORK/p456.sh" \
    || fail "capture must cross-check absent files against the run state (K3-N1)"
  grep -q 'refusing to skip its stop' "$WORK/p456.sh" \
    || fail "stop_unit must refuse to skip a running unit whose file is gone (K3-N1)"
  # K 复核 K2-N1 (round 5): unsupported captured states refuse pre-switch,
  # and recovery tri-states its is-active queries with CONFIRMED outcomes.
  grep -q 'unsupported/undeterminable enable-state' "$WORK/p456.sh" \
    || fail "an unsupported captured state must refuse BEFORE the switchover (K2-N1)"
  grep -q 'qstate' "$WORK/p456.sh" \
    || fail "recovery must tri-state its is-active queries (K2-N1)"
  grep -q 'cannot confirm \$u.service was stopped' "$WORK/p456.sh" \
    || fail "recovery must CONFIRM stopped units, reporting INCOMPLETE otherwise (K2-N1)"
  # K 复核 K2-R2: phase7 must verify the RUNNING process, not just config.
  grep -q 'systemctl show -p MainPID' "$WORK/p7.sh" \
    || fail "K2-R2: phase7 must resolve each unit's MainPID"
  grep -q 'sudo -n readlink "/proc/\$pid/exe"' "$WORK/p7.sh" \
    || fail "K2-R2: phase7 must read the running process's executable via /proc/<PID>/exe"
  echo "[self-test]   phase1/3/456/7 generated + bash -n clean"

  echo "[self-test] s7/8 ssh helper drains a fake channel (K 复核 K-R3)"
  # A fake paramiko package (PYTHONPATH shadows any real install) whose
  # channel only replaces the network: output and the exit status appear
  # AFTER a delay, and timeout=0 reads raise TimeoutError exactly like
  # paramiko's non-blocking mode — the K-R3 failure mode. The OLD helper
  # (exec_command(timeout=0) + immediate read) exits 255 on the delayed
  # case; the current helper must return the remote status and drain both
  # streams. The empty-output case must also complete cleanly.
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
      "$PYTHON" "$WORK/ssh_helper.py" exec 127.0.0.1 22 ubuntu "$WORK/fakepass" \
      <<< 'true' 2> "$WORK/fake-exec.err") || sub=$?
    [ "$sub" -eq 7 ] || fail "fake-channel exec must return the remote status (got $sub; stderr: $(tail -1 "$WORK/fake-exec.err" 2>/dev/null))"
    [ "$out" = "fake-stdout-after-delay" ] || fail "fake-channel exec lost the delayed stdout (got: '$out')"
    grep -q 'fake-stderr-after-delay' "$WORK/fake-exec.err" \
      || fail "fake-channel exec did not drain the delayed stderr"
    sub=0
    out=$(PYTHONPATH="$FAKEPKG" AK_FAKE_DELAY=0.3 AK_FAKE_EMPTY=1 \
      "$PYTHON" "$WORK/ssh_helper.py" exec 127.0.0.1 22 ubuntu "$WORK/fakepass" \
      <<< 'true' 2> /dev/null) || sub=$?
    [ "$sub" -eq 0 ] || fail "empty-output command must complete cleanly through the helper (got $sub)"
    [ -z "$out" ] || fail "empty-output command must produce no stdout (got: '$out')"
    echo "[self-test]   delayed (exit 7) + empty-output commands completed via the embedded helper"
  else
    echo "[self-test]   no python available; helper drain harness skipped"
  fi

  echo "[self-test] s8/8 K-R1/K2-N1/K2-R2 install logic (staging, unified recovery, ExecStart/MainPID)"
  # K 复核 K-R2: compose_registry_exec must keep the old line's ARGUMENTS but
  # rewrite the EXECUTABLE to the install path (execution, not greps).
  local c
  c=$(compose_registry_exec "/usr/local/bin/ak-cli registry-server --port 7200 --id-dir /var/lib/ak-legacy")
  [ "$c" = "$REMOTE_BIN registry-server --port 7200 --id-dir /var/lib/ak-legacy" ] \
    || fail "K-R2: ExecStart executable must be rewritten to $REMOTE_BIN with arguments preserved (got: $c)"
  c=$(compose_registry_exec "/usr/local/bin/ak-cli registry-server --port 7200")
  [ "$c" = "$REMOTE_BIN registry-server --port 7200 --id-dir $REMOTE_DATA" ] \
    || fail "K-R2: a preserved line lacking --id-dir must get it appended (got: $c)"
  c=$(compose_registry_exec "")
  [ "$c" = "$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA" ] \
    || fail "K-R2: fresh ExecStart malformed (got: $c)"
  c=$(compose_registry_exec "$REMOTE_BIN registry-server --port 7300 --id-dir /elsewhere")
  [ "$c" = "$REMOTE_BIN registry-server --port 7300 --id-dir /elsewhere" ] \
    || fail "K-R2: an already-correct ExecStart must pass through unchanged (got: $c)"
  # K 复核 K-R1: the prebuilt phase456 must bake a DISTINCT staging source,
  # refuse source == destination before any stop, and carry the recovery
  # path (behavioral proof runs in scripts/tests/test_deploy_linux_install.py).
  local stage_dir="/tmp/ak-deploy-staging-selftest2"
  TS=selftest2 BUILD_DIR= PREBUILT_MODE=1 \
    STAGING_DIR=$stage_dir STAGED_BIN=$stage_dir/ak-cli SHORT_SHA=$SHORT_SHA \
    REGISTRY_EXEC="$exec_stub" REG_RW=$REMOTE_DATA \
    LEGACY_UNIT= OLD_REGISTRY_UNIT= DATA_SOURCE_DIR= CLOSE_7100=0 \
    write_phase456 "$WORK/p456b.sh"
  bash -n "$WORK/p456b.sh" || fail "generated prebuilt p456b.sh is not valid bash"
  grep -q "&& SRC_BIN=$stage_dir/ak-cli\$" "$WORK/p456b.sh" \
    || fail "K-R1: prebuilt mode must take the binary from the staging path"
  grep -q 'install -m 0755 "$SRC_BIN" /opt/ak-sending/ak-cli.new' "$WORK/p456b.sh" \
    || fail "K-R1: install step must copy from the staging source to ak-cli.new"
  [ "$stage_dir/ak-cli" != "/opt/ak-sending/ak-cli.new" ] \
    || fail "K-R1: staging path must differ from the install destination"
  grep -q 'install source equals the install destination' "$WORK/p456b.sh" \
    || fail "K-R1: same-file guard missing from the install script"
  grep -q 'refusing BEFORE service switchover' "$WORK/p456b.sh" \
    || fail "K-R1: pre-switchover version gate missing from the install script"
  grep -q 'recover_services' "$WORK/p456b.sh" \
    || fail "K-R1/K2-N1: service-recovery path missing from the install script"
  grep -q 'RECOVERY OK' "$WORK/p456b.sh" \
    || fail "K2-N1: recovery must report its outcome"
  grep -q 'systemctl show -p ExecStart' "$WORK/p7.sh" \
    || fail "K-R2: phase7 must check the units' configured executable"
  grep -q 'systemctl show -p MainPID' "$WORK/p7.sh" \
    || fail "K2-R2: phase7 must verify the RUNNING process via MainPID + /proc/<PID>/exe"
  echo "[self-test]   staging source != destination, unified-recovery + outcome report baked, ExecStart/MainPID verified"

  echo "[self-test] PASS (nothing was contacted, nothing was deployed)"
  exit 0
}

# ---------------------------------------------------------------------------
# Phase scripts. Each is generated locally, uploaded as a file and run with
# bash on the server (quoting-safe). Env vars are substituted HERE so the
# remote scripts see plain values.
# ---------------------------------------------------------------------------

# Phase 1: preflight inspection. Prints a machine-parsable report; also
# decides migration inputs. Changes nothing. Env: SSH_* not needed; uses
# REGISTRY_PORT/STUN_PORT/REMOTE_*.
write_phase1() { # out-file
  cat > "$1" <<EOF
set -eu
echo "os: \$(uname -s) \$(uname -m)"
[ "\$(uname -s)" = Linux ] || { echo "ERROR: not Linux"; exit 3; }
[ "\$(uname -m)" = x86_64 ] || { echo "ERROR: not x86_64"; exit 3; }
echo "sudo: \$(sudo -n true >/dev/null 2>&1 && echo passwordless-ok || echo NEEDS-PASSWORD)"
command -v gcc >/dev/null 2>&1 && echo "gcc: \$(gcc --version | head -1)" || echo "gcc: missing"
command -v cargo >/dev/null 2>&1 && echo "cargo: \$(cargo --version)" || echo "cargo: missing (rustup will be installed)"
sudo -n test -x $REMOTE_BIN 2>/dev/null && echo "existing-binary: yes" || echo "existing-binary: no"
sudo -n test -x $REMOTE_BIN 2>/dev/null && echo "existing-version: \$($REMOTE_BIN --version 2>&1 || echo unreadable)"
# priv_exists <path>: privileged existence probe distinguishing
# exists / absent / cannot-determine ('env LC_ALL=C ls -d' rc + diagnostic
# text). Defined BEFORE its first use (unit discovery + identity probing).
# K 复核 K2-R3 (round 5): round 4 treated ANY 'cannot access' diagnostic as
# absent, but GNU ls prints 'cannot access ...: Permission denied' for
# exactly the case that must NEVER read as "no identity". The locale is
# pinned so the ENOENT text match is deterministic; ONLY a plain ENOENT
# diagnostic means absent — permission/I-O and every other diagnostic are
# UNDETERMINABLE (the caller refuses the preflight).
priv_exists() {
  local pe_out pe_rc
  pe_rc=0
  pe_out=\$(sudo -n env LC_ALL=C ls -d "\$1" 2>&1) || pe_rc=\$?
  if [ "\$pe_rc" = 0 ]; then echo yes; return 0; fi
  case "\$pe_out" in
    *"No such file or directory"*) echo no; return 0 ;;
    *) printf 'cannot determine existence of %s (%s)\\n' "\$1" "\$pe_out" >&2; return 2 ;;
  esac
}
# find_unit_file <unit.service>: echo the first existing unit-file path across
# the standard systemd directories. rc 0 = found (path on stdout), rc 1 =
# confirmed absent EVERYWHERE, rc 2 = a probe was undeterminable (priv_exists
# already printed why). K 复核 K2-R3 (round 6): existence must never be
# assumed from a single directory — a vendor-dir /usr/lib install of the
# STANDARD-NAMED unit is a legitimate upgrade input, not a fresh install.
find_unit_file() {
  local fud fus
  for fud in /etc/systemd/system /run/systemd/system /run/systemd/transient /usr/lib/systemd/system /lib/systemd/system; do
    fus=\$(priv_exists "\$fud/\$1") || return 2
    if [ "\$fus" = yes ]; then printf '%s\\n' "\$fud/\$1"; return 0; fi
  done
  return 1
}
# effective_execstart <unit.service>: echo the EFFECTIVE ExecStart entries of
# the unit — the MERGED view of the main fragment plus drop-ins as systemd
# loads them (unit.c loads fragments first, then drop-ins; load-fragment.c
# treats an EMPTY 'ExecStart=' as clearing the command list, later entries
# otherwise append). systemctl cat emits exactly that merged view (main
# first, then drop-ins in precedence order), so parse it here. K 复核 K2-R3
# (round 7): reading only the MAIN file let a legal drop-in override
# ('ExecStart=' + re-set with different --id-dir) be silently ignored and
# the deploy re-pointed the identity to the wrong directory. Refuses (exit
# 3) when the merged config cannot be read or carries no ExecStart at all;
# MULTIPLE surviving entries are returned for the caller to resolve against
# the running instance (or refuse).
effective_execstart() {
  local ec_cat
  ec_cat=\$(sudo -n env LC_ALL=C systemctl cat "\$1" 2>/dev/null) \\
    || { echo "ERROR: cannot read the effective configuration of \$1 (systemctl cat failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
  printf '%s\\n' "\$ec_cat" | awk '/^ExecStart=/ { v = substr(\$0, 11); if (v == "") { n = 0; split("", L) } else { L[++n] = v } } END { for (i = 1; i <= n; i++) print L[i] }'
}
# reg_args_agree <exec-line> <cmdline-line>: 0 when the EFFECTIVE values of
# the configured ExecStart and the running cmdline AGREE (defaults derived;
# see the rc contract inside). The compared surface is 'ak-cli
# registry-server's actual argument surface (verified against
# crates/ak-cli/src/main.rs): the global --id-dir (identity dir:
# registry.key plus the default db location) and the subcommand's
# --db-path select DATA/IDENTITY LOCATIONS (K 复核 K2-R3 round 8:
# --db-path used to be missing, so a configured new.db was chosen over the
# running old.db); --port and --bind select the LISTEN surface
# (occupancy-relevant).
reg_args_agree() {
  # K 复核 K2-R3 (round 9): compare EFFECTIVE values, DEFAULTS INCLUDED —
  # round 8 compared an explicit value only when the RUNNING side spelled
  # the flag out, so a running instance relying on the default db location
  # was silently switched to a configured --db-path pointing elsewhere.
  # Defaults verified against ak-cli itself (crates/ak-cli/src/main.rs):
  #   --db-path : <effective --id-dir>/registry.db  (registry_db_path());
  #               main.rs:3792 uses it when db_path is None.
  #   --id-dir  : <USERPROFILE|HOME>/.ak-sending (id_dir(), main.rs:709) —
  #               ENV-DEPENDENT: an omitted side's effective value cannot
  #               be established from its arguments alone.
  #   --port    : constant 7200 (ak-signal DEFAULT_V2_PORT).
  #   --bind    : constant 0.0.0.0 (resolve_listen_addr default).
  # rc 0 = agree; rc 1 = materially differ; rc 2 = an effective value's
  # actual source cannot be established (one side relies on an
  # environment-dependent default the other side does not share).
  local a_id b_id a_db b_db a_port b_port a_bind b_bind
  a_id=\$(printf '%s\\n' "\$1" | sed -n 's/.*--id-dir[= ]\\([^ ]*\\).*/\\1/p')
  b_id=\$(printf '%s\\n' "\$2" | sed -n 's/.*--id-dir[= ]\\([^ ]*\\).*/\\1/p')
  if [ "\$a_id" != "\$b_id" ]; then
    if [ -z "\$a_id" ] || [ -z "\$b_id" ]; then
      printf '%s\\n' "[preflight] the effective --id-dir of one side relies on the service environment and cannot be established from its arguments (configured '\$1' vs running '\$2')" >&2
      return 2
    fi
    return 1
  fi
  a_db=\$(printf '%s\\n' "\$1" | sed -n 's/.*--db-path[= ]\\([^ ]*\\).*/\\1/p')
  b_db=\$(printf '%s\\n' "\$2" | sed -n 's/.*--db-path[= ]\\([^ ]*\\).*/\\1/p')
  if [ -z "\$a_db" ] && [ -n "\$a_id" ]; then a_db="\$a_id/registry.db"; fi
  if [ -z "\$b_db" ] && [ -n "\$b_id" ]; then b_db="\$b_id/registry.db"; fi
  if [ "\$a_db" != "\$b_db" ]; then
    if [ -z "\$a_db" ] || [ -z "\$b_db" ]; then
      printf '%s\\n' "[preflight] the effective --db-path of one side relies on the service-environment default and cannot be established (configured '\$1' vs running '\$2')" >&2
      return 2
    fi
    return 1
  fi
  a_port=\$(printf '%s\\n' "\$1" | sed -n 's/.*--port[= ]\\([^ ]*\\).*/\\1/p')
  b_port=\$(printf '%s\\n' "\$2" | sed -n 's/.*--port[= ]\\([^ ]*\\).*/\\1/p')
  if [ -z "\$a_port" ]; then a_port=7200; fi
  if [ -z "\$b_port" ]; then b_port=7200; fi
  if [ "\$a_port" != "\$b_port" ]; then return 1; fi
  a_bind=\$(printf '%s\\n' "\$1" | sed -n 's/.*--bind[= ]\\([^ ]*\\).*/\\1/p')
  b_bind=\$(printf '%s\\n' "\$2" | sed -n 's/.*--bind[= ]\\([^ ]*\\).*/\\1/p')
  if [ -z "\$a_bind" ]; then a_bind=0.0.0.0; fi
  if [ -z "\$b_bind" ]; then b_bind=0.0.0.0; fi
  if [ "\$a_bind" != "\$b_bind" ]; then return 1; fi
  return 0
}
for u in ak-registry-server ak-stun-server; do
  # K 复核 K2-R3 (round 6): the managed units are looked up across ALL
  # standard directories (rounds <= 5 checked only /etc/systemd/system, so a
  # standard-named unit under /usr/lib/systemd/system read as "absent" and
  # its identity arguments were silently dropped from the migration).
  # Probe failures refuse; only a confirmed absent-everywhere is "absent".
  u_rc=0
  u_file=\$(find_unit_file "\$u.service") || u_rc=\$?
  if [ "\$u_rc" = 2 ]; then
    echo "ERROR: cannot determine whether \$u.service exists; refusing before any switchover (K2-R3)" >&2
    exit 3
  fi
  if [ -n "\$u_file" ]; then
    echo "unit \$u: present"
    # K 复核 K2-R3 (round 7): the EFFECTIVE config (main + drop-ins, with
    # the empty-'ExecStart=' clearing semantics), not just the main file.
    u_eff=\$(effective_execstart "\$u.service")
    u_n=\$(printf '%s\\n' "\$u_eff" | grep -c . || true)
    if [ "\$u_n" != 1 ]; then
      echo "ERROR: \$u.service has \$u_n effective ExecStart entries (drop-ins?) and no running instance resolves them here; refusing before any switchover (K2-R3)" >&2
      exit 3
    fi
    echo "  ExecStart=\$u_eff"
    sudo -n env LC_ALL=C systemctl cat "\$u.service" 2>/dev/null | sed -n 's/^User=/  User=/p' | tail -1
    systemctl is-active \$u.service >/dev/null 2>&1 && echo "  active: yes" || echo "  active: no"
  else
    echo "unit \$u: absent"
  fi
done
# Live listener on the registry port -> authoritative id-dir + old unit name
# (the existing production unit name is NOT verified per docs §0/§3, so it is
# discovered from the port, not assumed).
# K 复核 K2-R3: a NON-ROOT 'ss -p' cannot see other users' PIDs, so the old
# bare probes reported "pid unknown / not-a-systemd-unit" for a foreign-user
# registry and the deploy refused (or missed) a migratable old service. All
# three occupancy probes now read ONE sudo-captured snapshot per family; if
# occupancy CANNOT be determined the phase FAILS here — before any
# switchover — instead of misreporting the ports as free.
# K 复核 K2-R3 (round 4): a KNOWN occupant whose cgroup cannot be read is
# just as blocking — piping the sudo cat through awk swallowed the error and
# let the deploy continue with "not-a-systemd-unit". Every privileged probe
# in this phase now fails HARD when its answer cannot be determined.
ss_tcp=\$(sudo -n ss -ltnp 2>/dev/null) \
  || { echo "ERROR: cannot determine TCP listeners (sudo -n ss failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
ss_udp=\$(sudo -n ss -lunp 2>/dev/null) \
  || { echo "ERROR: cannot determine UDP sockets (sudo -n ss failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
reg_line=\$(printf '%s\\n' "\$ss_tcp" | grep -E "[:.]$REGISTRY_PORT\b" | head -1 || true)
if [ -n "\$reg_line" ]; then
  reg_pid=\$(printf '%s' "\$reg_line" | sed -n 's/.*pid=\\([0-9]*\\).*/\\1/p')
  reg_unit=""
  if [ -n "\${reg_pid:-}" ]; then
    cg=\$(sudo -n cat /proc/\$reg_pid/cgroup 2>/dev/null) \\
      || { echo "ERROR: cannot read /proc/\$reg_pid/cgroup of the $REGISTRY_PORT/tcp occupant (sudo -n cat failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
    reg_unit=\$(printf '%s\\n' "\$cg" | awk -F/ '{print \$NF}' | grep '\\.service\$' | tail -1 || true)
  fi
  echo "port $REGISTRY_PORT/tcp: in use (pid \${reg_pid:-unknown}, unit \${reg_unit:-not-a-systemd-unit})"
  # K 复核 K2-R3 (round 4): /proc/<PID>/cmdline has NO trailing newline; the
  # NUL->space conversion therefore joined the next machine field onto the
  # cmdline line and real_run's anchored '^  old-registry-unit:' parser lost
  # the old unit. Terminate the line explicitly so every machine field in
  # this report starts at column 0. The captured cmdline is ALSO the running
  # instance's authoritative argument list for the cross-check below.
  reg_cmdline=""
  if [ -n "\${reg_pid:-}" ]; then
    # NOTE (K 复核 K2-R3 round 8): the '|| true' cannot distinguish a read
    # failure from a short read (tr succeeds on empty input either way) —
    # EMPTINESS is the failure signal for a live service, and the occupant
    # block below refuses on an empty cmdline (fail closed).
    reg_cmdline=\$(sudo -n cat /proc/\$reg_pid/cmdline 2>/dev/null | tr '\\0' ' ' || true)
  fi
  [ -n "\$reg_cmdline" ] && { printf '%s\\n' "\$reg_cmdline" | sed 's/^/  cmdline: /'; printf '\\n'; } || true
  # K 复核 K2-R3 (round 5/6/7): an occupant unit IDENTIFIED from its cgroup
  # must be bound into the migration set — INCLUDING the standard name
  # ak-registry-server.service (round 5 excluded it, so a vendor-dir install
  # of the standard-named unit lost its --id-dir and the deploy silently
  # re-pointed the identity). Locate the unit file across every standard
  # directory; the EFFECTIVE configuration (main + drop-ins, empty
  # 'ExecStart=' clearing, later entries appending — round 7) feeds
  # real_run's ExecStart/--id-dir decisions regardless of the unit's name,
  # cross-checked against the RUNNING cmdline (port/--id-dir). The
  # old-registry-unit FIELD stays limited to non-standard names (the
  # standard-named unit is already in the managed stop/backup set).
  # Unlocatable, undeterminable, ambiguous or materially disagreeing
  # configs -> REFUSE.
  if [ -n "\${reg_unit:-}" ]; then
    reg_rc=0
    reg_unit_file=\$(find_unit_file "\$reg_unit") || reg_rc=\$?
    if [ "\$reg_rc" = 2 ]; then
      echo "ERROR: cannot determine whether \$reg_unit exists; refusing before any switchover (K2-R3)" >&2
      exit 3
    fi
    if [ -n "\$reg_unit_file" ]; then
      # K 复核 K2-R3 (round 8): the running cmdline is NOT optional
      # evidence. A live service's /proc/<PID>/cmdline is never empty
      # (argv[0] alone guarantees content), so an empty read means the
      # probe FAILED — round 7 skipped the cross-check in exactly that
      # case and a read failure silently selected the wrong config. The
      # cross-check FAILS CLOSED here instead.
      if [ -z "\$reg_cmdline" ]; then
        echo "ERROR: the running cmdline of \$reg_unit (pid \${reg_pid:-?}) is unreadable or empty; the config cross-check has no evidence; refusing before any switchover (K2-R3)" >&2
        exit 3
      fi
      [ "\$reg_unit" != "ak-registry-server.service" ] && echo "  old-registry-unit: \$reg_unit"
      reg_eff=\$(effective_execstart "\$reg_unit")
      reg_n=\$(printf '%s\\n' "\$reg_eff" | grep -c . || true)
      reg_pick=""
      if [ "\$reg_n" = 1 ]; then
        reg_pick="\$reg_eff"
      elif [ "\$reg_n" -gt 1 ]; then
        # Multiple surviving entries (no clearing drop-in): resolve against
        # the RUNNING cmdline; anything but a unique match refuses.
        reg_hits=0
        while IFS= read -r re_e; do
          [ -n "\$re_e" ] || continue
          if reg_args_agree "\$re_e" "\$reg_cmdline"; then
            reg_pick="\$re_e"
            reg_hits=\$((reg_hits + 1))
          fi
        done <<REG_EFF_H
\$reg_eff
REG_EFF_H
        if [ "\$reg_hits" != 1 ]; then
          echo "ERROR: \$reg_unit has \$reg_n effective ExecStart entries (no clearing drop-in) and the running cmdline does not resolve exactly one; refusing before any switchover (K2-R3)" >&2
          exit 3
        fi
      else
        echo "ERROR: \$reg_unit has no effective ExecStart; refusing before any switchover (K2-R3)" >&2
        exit 3
      fi
      # K 复核 K2-R3 (round 8/9): unconditional — the cmdline was verified
      # non-empty above, and a failed read already refused. rc 2 = an
      # effective value's source cannot be established (environment-
      # dependent default); rc 1 = material disagreement. Both refuse.
      rag_rc=0
      reg_args_agree "\$reg_pick" "\$reg_cmdline" || rag_rc=\$?
      if [ "\$rag_rc" = 2 ]; then
        echo "ERROR: the effective configuration of \$reg_unit cannot be established against the running cmdline (an effective value relies on an environment-dependent default); refusing before any switchover (K2-R3)" >&2
        exit 3
      fi
      if [ "\$rag_rc" = 1 ]; then
        echo "ERROR: the effective ExecStart of \$reg_unit materially disagrees with the running cmdline (configured '\$reg_pick' vs running '\$reg_cmdline'); refusing before any switchover (K2-R3)" >&2
        exit 3
      fi
      echo "  ExecStart=\$reg_pick"
      reg_cat2=\$(sudo -n env LC_ALL=C systemctl cat "\$reg_unit" 2>/dev/null) || true
      printf '%s\\n' "\$reg_cat2" | sed -n 's/^User=/  old-registry-User=/p' | tail -1
      # K 复核 K9-N1: derive the RUNTIME-ESTABLISHED identity dir of the
      # identified occupant — the migration source must be bound to the
      # running service, NEVER to directory-enumeration order. Explicit
      # --id-dir in the EFFECTIVE ExecStart wins; otherwise the CLI default
      # (main.rs id_dir(): USERPROFILE, else HOME, else '.') applies, taken
      # from the RUNTIME /proc/<PID>/environ and cross-checked against the
      # unit's merged Environment=/UnsetEnvironment= directives. Missing or
      # contradictory evidence refuses BEFORE any switchover.
      reg_rt_id=\$(printf '%s\\n' "\$reg_pick" | sed -n 's/.*--id-dir[= ]\\([^ ]*\\).*/\\1/p')
      if [ -z "\$reg_rt_id" ]; then
        reg_env=\$(sudo -n cat /proc/\$reg_pid/environ 2>/dev/null | tr '\\0' '\\n' || true)
        if [ -z "\$reg_env" ]; then
          echo "ERROR: no runtime environment evidence (unreadable /proc/\$reg_pid/environ) to derive the default identity dir of \$reg_unit; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
        ev_up=\$(printf '%s\\n' "\$reg_env" | sed -n 's/^USERPROFILE=\\(.*\\)\$/\\1/p' | head -1)
        ev_home=\$(printf '%s\\n' "\$reg_env" | sed -n 's/^HOME=\\(.*\\)\$/\\1/p' | head -1)
        ue_up=\$(printf '%s\\n' "\$reg_cat2" | sed -n 's/^Environment=\"\\?USERPROFILE=\\(.*\\)\$/\\1/p' | tail -1 | sed 's/^\"//; s/\"\$//')
        ue_home=\$(printf '%s\\n' "\$reg_cat2" | sed -n 's/^Environment=\"\\?HOME=\\(.*\\)\$/\\1/p' | tail -1 | sed 's/^\"//; s/\"\$//')
        uu_up=\$(printf '%s\\n' "\$reg_cat2" | grep -c '^UnsetEnvironment=.*USERPROFILE' || true)
        uu_home=\$(printf '%s\\n' "\$reg_cat2" | grep -c '^UnsetEnvironment=.*HOME' || true)
        if [ -n "\$ue_up" ] && [ "\$ue_up" != "\$ev_up" ]; then
          echo "ERROR: \$reg_unit sets USERPROFILE='\$ue_up' but its runtime environ reports '\$ev_up'; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
        if [ -n "\$ue_home" ] && [ "\$ue_home" != "\$ev_home" ]; then
          echo "ERROR: \$reg_unit sets HOME='\$ue_home' but its runtime environ reports '\$ev_home'; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
        if [ "\$uu_up" != 0 ] && [ -n "\$ev_up" ]; then
          echo "ERROR: \$reg_unit unsets USERPROFILE but its runtime environ still carries it; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
        if [ "\$uu_home" != 0 ] && [ -n "\$ev_home" ]; then
          echo "ERROR: \$reg_unit unsets HOME but its runtime environ still carries it; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
        if [ -n "\$ev_up" ]; then
          reg_rt_id="\$ev_up/.ak-sending"
        elif [ -n "\$ev_home" ]; then
          reg_rt_id="\$ev_home/.ak-sending"
        else
          echo "ERROR: neither USERPROFILE nor HOME in the runtime environ of \$reg_unit (and no explicit --id-dir); the CLI default would fall back to the unknowable working directory; refusing before any switchover (K9-N1)" >&2
          exit 3
        fi
      fi
      echo "  runtime-id-dir: \$reg_rt_id"
    else
      echo "ERROR: occupant unit \$reg_unit of port $REGISTRY_PORT/tcp was identified but its unit file cannot be located in any standard directory; refusing before any switchover (K2-R3)" >&2
      exit 3
    fi
  fi
else
  echo "port $REGISTRY_PORT/tcp: free"
fi
# Live listener on the STUN port -> the §0 conflict check.
stun_line=\$(printf '%s\\n' "\$ss_udp" | grep -E "[:.]$STUN_PORT\\b" | head -1 || true)
if [ -n "\$stun_line" ]; then
  stun_pid=\$(printf '%s' "\$stun_line" | sed -n 's/.*pid=\\([0-9]*\\).*/\\1/p')
  stun_unit=""
  if [ -n "\${stun_pid:-}" ]; then
    scg=\$(sudo -n cat /proc/\$stun_pid/cgroup 2>/dev/null) \\
      || { echo "ERROR: cannot read /proc/\$stun_pid/cgroup of the $STUN_PORT/udp occupant (sudo -n cat failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
    stun_unit=\$(printf '%s\\n' "\$scg" | awk -F/ '{print \$NF}' | grep '\\.service\$' | tail -1 || true)
  fi
  echo "port $STUN_PORT/udp: in use (pid \${stun_pid:-unknown}, unit \${stun_unit:-not-a-systemd-unit})"
  [ -n "\${stun_pid:-}" ] && { sudo -n cat /proc/\$stun_pid/cmdline 2>/dev/null | tr '\\0' ' ' | sed 's/^/  cmdline: /'; printf '\\n'; } || true
else
  echo "port $STUN_PORT/udp: free"
fi
sig_line=\$(printf '%s\\n' "\$ss_tcp" | grep -E '[:.]7100\b' | head -1 || true)
[ -n "\$sig_line" ] && echo "port 7100/tcp (legacy signal): in use" || echo "port 7100/tcp (legacy signal): free"
# Registry identity/database discovery (never modified in this phase).
# K 复核 K2-R3: private identity dirs (root-owned key/db under a private
# home) read as NONEXISTENT to the bare non-root '[ -e ]' probes, so the
# identity/database state could silently read "none found".
# K 复核 K2-R3 (round 4/5): priv_exists (defined above, before the occupant
# lookup) distinguishes exists / absent / cannot-determine, and ANY cannot-
# determine answer for a candidate identity path fails the preflight BEFORE
# a switchover — a blocked or permission-denied probe must never read as
# "no identity".
home_list=\$(sudo -n ls -1 /home 2>/dev/null) \\
  || { echo "ERROR: cannot enumerate /home (sudo -n ls failed); refusing before any switchover (K2-R3)" >&2; exit 3; }
home_ak_dirs=\$(printf '%s\\n' "\$home_list" | sed 's|^|/home/|; s|\$|/.ak-sending|')
for d in $REMOTE_DATA /root/.ak-sending \$home_ak_dirs; do
  dstate=\$(priv_exists "\$d") \\
    || { echo "ERROR: cannot determine whether \$d exists; refusing before any switchover (K2-R3)" >&2; exit 3; }
  if [ "\$dstate" = yes ]; then
    k=\$(priv_exists "\$d/registry.key") \\
      || { echo "ERROR: cannot probe \$d/registry.key; refusing before any switchover (K2-R3)" >&2; exit 3; }
    db=\$(priv_exists "\$d/registry.db") \\
      || { echo "ERROR: cannot probe \$d/registry.db; refusing before any switchover (K2-R3)" >&2; exit 3; }
    if [ "\$k" = yes ] || [ "\$db" = yes ]; then
      echo "registry-data: \$d (key:\$k db:\$db)"
    fi
  fi
done
# K 复核 K-R2: install steps run as this NON-ROOT SSH user through sudo -n;
# prove NOW — before anything is stopped — that the install path is usable
# through exactly that path (read-only mounts or restricted sudoers must
# abort in preflight, never mid-deploy). Read-only probes; mutate nothing.
if [ -d $REMOTE_BASE ]; then
  if sudo -n test -w $REMOTE_BASE 2>/dev/null; then
    echo "install-path: $REMOTE_BASE writable via sudo"
  else
    echo "ERROR: sudo -n cannot write $REMOTE_BASE (read-only mount or restricted sudoers?)"
  fi
else
  if sudo -n test -w \$(dirname $REMOTE_BASE) 2>/dev/null; then
    echo "install-path: $REMOTE_BASE absent; creatable via sudo (parent writable)"
  else
    echo "ERROR: sudo -n cannot create $REMOTE_BASE (parent not sudo-writable)"
  fi
fi
# K 复核 K2-R3: 'ufw status' is a read-only discovery probe that needs root;
# non-root it fails and would misreport the firewall as absent.
command -v ufw >/dev/null 2>&1 && echo "ufw: \$(sudo -n ufw status 2>/dev/null | head -1)" || echo "ufw: absent"
echo "disk-free /opt: \$(df -h /opt 2>/dev/null | tail -1 | awk '{print \$4}')"
exit 0
EOF
}

# Phase 3: build on the server. Env: SHORT_SHA, KEEP_BUILD_DIR not needed
# here. Runs as the SSH user (not root except apt). Writes a build log on the
# server and prints the last lines; the version gate must show git <sha>.
write_phase3() { # out-file remote-build-dir
  cat > "$1" <<EOF
set -eu
BUILD_DIR=$2
cd "\$BUILD_DIR"
if ! command -v gcc >/dev/null 2>&1; then
  echo "[build] installing build-essential (sudo) ..."
  sudo -n apt-get update -q
  sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q build-essential pkg-config ca-certificates curl
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "[build] installing rustup (minimal profile; toolchain pinned by rust-toolchain.toml) ..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  . "\$HOME/.cargo/env"
fi
echo "[build] cargo build --locked --release -p ak-cli (AK_GIT_SHA=$SHORT_SHA)"
export AK_GIT_SHA='$SHORT_SHA'
cargo build --locked --release -p ak-cli 2>&1 | tee build.log | tail -5
V=\$(./target/release/ak-cli --version 2>&1)
echo "[build] built: \$V"
case "\$V" in
  *"git $SHORT_SHA"*) : ;;
  *) echo "ERROR: version line must contain 'git $SHORT_SHA' (got: \$V); refusing to deploy an unidentifiable binary" >&2; exit 4 ;;
esac
exit 0
EOF
}

# Phase 4+5+6: install, systemd, firewall (root via sudo -n). Env:
# SHORT_SHA, TS, REGISTRY_PORT, STUN_PORT, plus deployment decisions
# (REGISTRY_EXEC, LEGACY_UNIT, DATA_SOURCE_DIR, CLOSE_7100, KEEP_BUILD_DIR,
# BUILD_DIR/PREBUILT_MODE, STAGING_DIR/STAGED_BIN for --binary).
write_phase456() { # out-file ...
  cat > "$1" <<EOF
set -eu
TS=$TS
SHORT_SHA=$SHORT_SHA
BUILD_DIR=${BUILD_DIR:-}
PREBUILT_MODE=${PREBUILT_MODE:-0}
STAGING_DIR=${STAGING_DIR:-}
STAGED_BIN=${STAGED_BIN:-}
SRC_BIN=\$BUILD_DIR/target/release/ak-cli
[ "\$PREBUILT_MODE" = 1 ] && SRC_BIN=${STAGED_BIN:-}
[ -n "\$SRC_BIN" ] && [ -x "\$SRC_BIN" ] || { echo "ERROR: source binary not found/executable at \${SRC_BIN:-<unset>}" >&2; exit 5; }
# K 复核 K-R1: the install source must be a DIFFERENT file from the install
# destination used below ($REMOTE_BASE/ak-cli.new). The old --binary flow
# uploaded straight to ak-cli.new and then ran install ak-cli.new ->
# ak-cli.new ("are the same file") AFTER the services were already stopped.
[ "\$SRC_BIN" != "$REMOTE_BASE/ak-cli.new" ] || { echo "ERROR: install source equals the install destination (\$SRC_BIN); refusing before any service is stopped" >&2; exit 5; }
if [ "\$PREBUILT_MODE" = 1 ]; then
  # K 复核 K-R1: re-verify the staged binary's version right before anything
  # is stopped (phase 2 checked at upload time; this closes the gap between
  # upload and switchover).
  V=\$( "\$SRC_BIN" --version 2>&1 || true )
  case "\$V" in
    *"git $SHORT_SHA"*) : ;;
    *) echo "ERROR: staged binary version is '\$V' (want 'git $SHORT_SHA'); refusing BEFORE service switchover" >&2; exit 5 ;;
  esac
fi

# --- service user + directories ------------------------------------------
if ! id -u $SERVICE_USER >/dev/null 2>&1; then
  echo "[install] creating system user $SERVICE_USER"
  sudo -n useradd --system --home-dir $REMOTE_BASE --shell /usr/sbin/nologin $SERVICE_USER
fi
sudo -n install -d -m 0755 -o $SERVICE_USER -g $SERVICE_USER $REMOTE_BASE $REMOTE_DATA

# --- backup current state (§3 step 1; the only rollback path across a
# --- schema migration, §4) ------------------------------------------------
# K 复核 K2-N1: EVERYTHING the unified recovery may need is saved HERE, i.e.
# BEFORE the first systemctl stop — previous binary, previous unit files and
# (step 1 of do_switchover) each unit's enabled/active state. SW_UNITS bakes
# the discovered old/legacy unit NAMES at generation time (remote \$vars would
# be unset at loop time and the old unit files would never be backed up);
# \$u/\$f are the only remote variables in this section.
SW_UNITS="ak-registry-server ak-stun-server${OLD_REGISTRY_UNIT:+ $OLD_REGISTRY_UNIT}${LEGACY_UNIT:+ $LEGACY_UNIT}"
BACKUP=$REMOTE_BASE/backup-\$TS
sudo -n install -d -m 0700 -o $SERVICE_USER -g $SERVICE_USER "\$BACKUP"
# K 复核 K2-R3: every existence probe below runs through sudo -n. This script
# runs as the NON-ROOT SSH user; a private tree (root-owned binary, private
# identity dir) must read as EXISTING to the deploy, never silently absent.
# K 复核 K2-N1: recovery below DEPENDS on these backups — a failed backup
# aborts BEFORE any service is stopped (a silent backup failure would make
# the later recovery worthless).
if sudo -n test -f $REMOTE_BIN 2>/dev/null; then
  sudo -n cp -a $REMOTE_BIN "\$BACKUP/ak-cli" \
    || { echo "ERROR: cannot back up binary $REMOTE_BIN to \$BACKUP; refusing BEFORE service switchover" >&2; exit 5; }
  echo "[install] backup: binary -> \$BACKUP/ak-cli"
fi
for f in registry.key registry.db; do
  if sudo -n test -f "$REMOTE_DATA/\$f" 2>/dev/null; then
    sudo -n cp -a "$REMOTE_DATA/\$f" "\$BACKUP/\$f" \
      || { echo "ERROR: cannot back up \$f to \$BACKUP; refusing BEFORE service switchover" >&2; exit 5; }
    echo "[install] backup: \$f -> \$BACKUP/\$f"
  fi
done
for u in \$SW_UNITS; do
  if sudo -n test -f "/etc/systemd/system/\$u.service" 2>/dev/null; then
    sudo -n cp -a "/etc/systemd/system/\$u.service" "\$BACKUP/\$u.service" \
      || { echo "ERROR: cannot back up unit \$u.service to \$BACKUP; refusing BEFORE service switchover" >&2; exit 5; }
    echo "[install] backup: unit \$u.service -> \$BACKUP"
  fi
done

# --- preserve/migrate registry identity + database ------------------------
DATA_SOURCE=${DATA_SOURCE_DIR:-}
if [ -n "\$DATA_SOURCE" ] && [ "\$DATA_SOURCE" != "$REMOTE_DATA" ]; then
  for f in registry.key registry.db; do
    if sudo -n test -f "\$DATA_SOURCE/\$f" 2>/dev/null && ! sudo -n test -f "$REMOTE_DATA/\$f" 2>/dev/null; then
      sudo -n cp -a "\$DATA_SOURCE/\$f" "$REMOTE_DATA/\$f"
      echo "[install] migrated identity/db: \$DATA_SOURCE/\$f -> $REMOTE_DATA/\$f (source kept)"
    fi
  done
fi
sudo -n chown -R $SERVICE_USER:$SERVICE_USER $REMOTE_DATA
sudo -n chmod 700 $REMOTE_DATA
sudo -n test -f $REMOTE_DATA/registry.key 2>/dev/null && sudo -n chmod 600 $REMOTE_DATA/registry.key

# --- service switchover under UNIFIED failure recovery (K 复核 K2-N1) --------
# Everything above only read state or created new files. From the first
# systemctl stop until enable --now this script owns STOPPED services.
# K 复核 K2-N1: the previous repair protected ONLY the binary install — its
# recovery branch ended before the legacy stop, the unit writes, the
# daemon-reload and the enable --now, which kept running bare under set -e.
# A failure at the FIRST unit write therefore left both services stopped
# with nothing started again (the review reproduced exactly that against the
# real generated install script). The ENTIRE switchover now lives inside
# do_switchover behind an 'if !' guard, and ANY failure in it runs
# recover_services first: previous binary back, previous unit files back,
# daemon-reload, re-enable/re-start what was enabled/active before.
OLD_REGISTRY_UNIT=${OLD_REGISTRY_UNIT:-}
LEGACY_UNIT=${LEGACY_UNIT:-}
sw_state=""
HAD_OLD_BIN=0
# probe_unit <name>: sets PU_EXISTS to exists|absent|unknown plus the RAW
# systemctl answers PU_EN / PU_AC (legal non-zero states like disabled /
# inactive / not-found are DATA, not failures — round 3 blanked them into a
# useless "unknown" and recovery then could not restore disabled/inactive
# pre-states; K 复核 K2-N1 round 4). systemctl cat decides existence:
# rc 0 = a unit definition exists; a not-found message = CONFIRMED absent
# (fresh install); anything else = cannot determine, which must never be
# treated as absent.
probe_unit() {
  # NOTE: legal non-zero states (disabled, inactive, not-found) still print
  # their value on stdout — keep it ('|| true'), never blank it into
  # "unknown" (round 3 did exactly that and recovery then could not restore
  # disabled/inactive pre-states; K 复核 K2-N1 round 4).
  PU_EN=\$(sudo -n systemctl is-enabled "\$1.service" 2>/dev/null) || true
  PU_AC=\$(sudo -n systemctl is-active "\$1.service" 2>/dev/null) || true
    if sudo -n env LC_ALL=C systemctl cat "\$1.service" >/dev/null 2>&1; then
      PU_EXISTS=exists
      return 0
    fi
  PU_MSG=\$(sudo -n env LC_ALL=C systemctl cat "\$1.service" 2>&1) || true
  case "\$PU_MSG" in
    # ONLY diagnostics that name THIS unit classify as absent (K 复核 K3-N1
    # round 6). Round 5 matched bare substrings, so systemd's standard bus
    # error 'Failed to connect to bus: No such file or directory' matched
    # *"No such file"* and a persistent D-Bus failure read as "unit absent"
    # — the install then skipped every stop (or ran to recovery) with the
    # state undeterminable. Known per-unit shapes: v255 systemctl cat prints
    # 'No files found for <name>.' (systemctl-util.c, LoadState=not-found);
    # classic prints 'Unit <name> could not be found.'; stop/cat on a
    # not-loaded unit prints '<name> is not loaded' / 'Unit <name> not
    # loaded'. Connection/permission/daemon errors never carry the unit
    # name and stay UNKNOWN (refusal).
    *"No files found for \$1.service"*|*"Unit \$1.service could not be found"*|*"\$1.service is not loaded"*|*"Unit \$1.service not loaded"*) PU_EXISTS=absent ;;
    *) PU_EXISTS=unknown ;;
  esac
}
# stop_unit <name> (K 复核 K3-N1): round 3 stopped the managed units
# UNCONDITIONALLY, so on a FRESH install (units absent) the very first
# 'Unit ... not loaded' stop error aborted the whole install before anything
# was created. A CONFIRMED-absent unit is "nothing to stop"; a unit that
# exists keeps the K2-R2 STRICT stop (a real stop failure of an existing
# unit stays fatal); an undeterminable existence is fatal too.
stop_unit() {
  probe_unit "\$1"
  case "\$PU_EXISTS" in
    exists)
      sudo -n systemctl stop "\$1.service" || return 1 ;;
    absent)
      # K 复核 K3-N1 (round 7): a missing unit FILE does not mean a stopped
      # service — systemd keeps running units after their file is deleted
      # (upstream unit_exists treats a not-found unit as existing unless it
      # is also inactive). Skipping the stop of a unit whose run state is
      # active/transitional/undeterminable would leave its process running
      # through the whole switchover.
      case "\$PU_AC" in
        inactive|failed)
          echo "[install] unit \$1.service not present; nothing to stop (K3-N1)" ;;
        *)
          echo "[install] the unit file for \$1.service is absent but the service reports '\$PU_AC'; refusing to skip its stop (K3-N1)" >&2
          return 1 ;;
      esac ;;
    *)
      echo "[install] cannot determine whether \$1.service exists; refusing to switch blind (K3-N1)" >&2
      return 1 ;;
  esac
}
# Capture the affected units' full pre-state BEFORE the protected window:
# exists/enabled/active per unit. A capture failure aborts with NOTHING
# touched yet (no recovery needed — nothing was stopped). Entries are
# name:exists|absent:enabled|disabled|...:active|inactive|...
sw_state=""
for u in \$SW_UNITS; do
  probe_unit "\$u"
  if [ "\$PU_EXISTS" = unknown ]; then
    echo "ERROR: cannot determine the current state of \$u.service (systemctl cat unusable); refusing BEFORE service switchover (K2-N1)" >&2
    exit 5
  fi
  if [ "\$PU_EXISTS" = exists ]; then
    # K 复核 K2-N1 (round 5): an unsupported/undeterminable STATE VALUE must
    # also refuse BEFORE the switchover — round 4 only refused empty output,
    # so a non-empty 'unknown' was recorded and the switch proceeded with an
    # unrecoverable state.
    case "\$PU_EN" in
      enabled|disabled) : ;;
      *)
        echo "ERROR: unsupported/undeterminable enable-state '\$PU_EN' for \$u.service; refusing BEFORE service switchover (K2-N1)" >&2
        exit 5 ;;
    esac
    case "\$PU_AC" in
      active|inactive|failed) : ;;
      *)
        echo "ERROR: unsupported/undeterminable run-state '\$PU_AC' for \$u.service; refusing BEFORE service switchover (K2-N1)" >&2
        exit 5 ;;
    esac
  else
    # K 复核 K3-N1 (round 7): absent FILE + active (or transitional or
    # undeterminable) run state is NOT a fresh install — a running process
    # whose unit file is gone has no recoverable pre-state; refuse BEFORE
    # anything is touched. Only absent plus a confirmed inactive-or-failed
    # run state may proceed (a 'failed' unit has no live process; systemd
    # reports both as not-running for this purpose).
    case "\$PU_AC" in
      inactive|failed) : ;;
      *)
        echo "ERROR: the unit file for \$u.service is absent but its run state is '\$PU_AC' (a deleted unit file does not stop a running service); no recoverable pre-state; refusing BEFORE service switchover (K3-N1)" >&2
        exit 5 ;;
    esac
  fi
  sw_state="\$sw_state\$u:\$PU_EXISTS:\$PU_EN:\$PU_AC;"
done
if sudo -n test -f $REMOTE_BIN 2>/dev/null; then HAD_OLD_BIN=1; fi
echo "[install] switchover begins; captured unit state: \$sw_state (previous binary present: \$HAD_OLD_BIN)"
recover_services() {
  # K 复核 K2-N1 (round-4 remainder): recovery must restore the RUNNING
  # state, not only the disk. (a) A unit this failed switch left ACTIVE must
  # be RESTARTED onto the restored files — 'start' on an active unit is a
  # no-op and left the NEW process running under a RECOVERY OK banner.
  # (b) disabled/inactive pre-states are restored as well: what the switch
  # enabled gets disabled again, what it started gets stopped.
  # (c) the previous binary is restored from ak-cli.old-<ts> with the
  # PRE-STOP BACKUP as fallback; if neither works the recovery reports
  # INCOMPLETE, never OK. Units restored to active are then image-verified
  # (MainPID -> /proc/<PID>/exe vs the restored unit's configured
  # executable); any state that cannot be confirmed reports INCOMPLETE.
  local rc=0 u en ac ex entry rest pid exe conf bin_ok
  echo "[install] switchover failed; RECOVERING previous service state (captured: \$sw_state)" >&2
  # (c) previous binary: renamed copy first, pre-stop backup as fallback.
  if [ "\$HAD_OLD_BIN" = 1 ]; then
    bin_ok=0
    if [ -f $REMOTE_BASE/ak-cli.old-\$TS ]; then
      sudo -n mv -f $REMOTE_BASE/ak-cli.old-\$TS $REMOTE_BIN 2>/dev/null && bin_ok=1 || rc=1
    fi
    if [ "\$bin_ok" = 0 ] && sudo -n test -f "\$BACKUP/ak-cli" 2>/dev/null; then
      echo "[install] recovery: previous binary from the pre-stop backup \$BACKUP/ak-cli (rename slot unusable; K2-N1)" >&2
      sudo -n cp -a "\$BACKUP/ak-cli" $REMOTE_BIN 2>/dev/null && bin_ok=1 || rc=1
    fi
    if [ "\$bin_ok" = 0 ]; then
      echo "ERROR: recovery cannot restore the previous binary (neither ak-cli.old-\$TS nor \$BACKUP/ak-cli usable)" >&2
      rc=1
    fi
  else
    # Fresh-install rollback: no binary existed before the switch.
    sudo -n rm -f $REMOTE_BIN 2>/dev/null || rc=1
  fi
  sudo -n rm -f $REMOTE_BASE/ak-cli.new 2>/dev/null || true
  for u in \$SW_UNITS; do
    if sudo -n test -f "\$BACKUP/\$u.service" 2>/dev/null; then
      sudo -n cp -a "\$BACKUP/\$u.service" "/etc/systemd/system/\$u.service" 2>/dev/null || rc=1
    fi
  done
  # Fresh-install rollback: when NO previous unit file existed for the two
  # managed units, remove whatever this failed switchover had already
  # written, so the disk returns to the pre-deploy state.
  for u in ak-registry-server ak-stun-server; do
    if ! sudo -n test -f "\$BACKUP/\$u.service" 2>/dev/null \\
       && sudo -n test -f "/etc/systemd/system/\$u.service" 2>/dev/null; then
      sudo -n rm -f "/etc/systemd/system/\$u.service" 2>/dev/null || rc=1
    fi
  done
  sudo -n systemctl daemon-reload 2>/dev/null || rc=1
  # qstate <unit>: 0 = active-ish, 1 = inactive-ish, 2 = CANNOT DETERMINE.
  # K 复核 K2-N1 (round 5): a FAILED is-active query (no stdout, non-zero)
  # must never be treated as "inactive" — round 4's if/else skipped the stop
  # for a failed query and left the NEW process running under a RECOVERY OK
  # banner. Failed queries are handled conservatively below and the FINAL
  # state is re-confirmed; anything unconfirmable reports INCOMPLETE.
  qstate() {
    local qs
    # Keep the output even on non-zero rc — 'inactive' arrives with rc 3 and
    # must never be blanked into "cannot determine" (the same clobber bug
    # the review found in the capture path).
    # K 复核 K2-N1 (round 6): ONLY the FINAL states count. Transitional
    # states (activating/reloading/deactivating) prove nothing — a
    # 'deactivating' unit still has its MainPID alive (systemd maps the
    # whole stop path to UNIT_DEACTIVATING), and round 5 classified it as
    # stopped, so recovery skipped the stop and printed RECOVERY OK with the
    # NEW image still running. Transitional/unknown => UNDETERMINABLE: the
    # callers take the conservative stop(+confirm)/restart path.
    qs=\$(sudo -n systemctl is-active "\$1.service" 2>/dev/null) || true
    case "\$qs" in
      active) return 0 ;;
      inactive|failed) return 1 ;;
      *) return 2 ;;
    esac
  }
  for entry in \${sw_state//;/ }; do
    u=\${entry%%:*}; rest=\${entry#*:}; ex=\${rest%%:*}; rest=\${rest#*:}
    en=\${rest%%:*}; ac=\${rest##*:}
    if [ "\$ex" = absent ]; then
      # The unit did not exist before the switch: undo whatever enable/start
      # this switch performed. Active OR undeterminable -> conservatively
      # stop, then CONFIRM the unit ended inactive.
      qstate "\$u"; st=\$?
      if [ "\$st" != 1 ]; then
        sudo -n systemctl stop "\$u.service" 2>/dev/null || rc=1
        qstate "\$u"; st2=\$?
        if [ "\$st2" != 1 ]; then
          echo "[install] recovery cannot confirm \$u.service ended inactive (query state=\$st2); treating as incomplete" >&2
          rc=1
        fi
      fi
      if [ "\$(sudo -n systemctl is-enabled "\$u.service" 2>/dev/null || true)" = enabled ]; then
        sudo -n systemctl disable "\$u.service" >/dev/null 2>&1 || rc=1
      fi
      continue
    fi
    case "\$en" in
      enabled)  sudo -n systemctl enable "\$u.service" >/dev/null 2>&1 || rc=1 ;;
      disabled) sudo -n systemctl disable "\$u.service" >/dev/null 2>&1 || rc=1 ;;
      *)
        echo "[install] recovery cannot confirm the previous enable-state of \$u ('\$en'); treating as incomplete" >&2
        rc=1 ;;
    esac
    case "\$ac" in
      active)
        # (a) restart-if-active: 'start' on the still-active NEW process is
        # a no-op; only 'restart' picks up the restored files. A FAILED
        # state query also takes the restart path (restart is correct for
        # both active and inactive units); the image verification below is
        # the confirmation.
        qstate "\$u"; st=\$?
        if [ "\$st" = 1 ]; then
          sudo -n systemctl start "\$u.service" 2>/dev/null || rc=1
        else
          sudo -n systemctl restart "\$u.service" 2>/dev/null || rc=1
        fi
        # Verify the running image of what recovery just (re)started.
        pid=\$(systemctl show -p MainPID --value "\$u.service" 2>/dev/null) || pid=""
        exe=\$(sudo -n readlink "/proc/\$pid/exe" 2>/dev/null) || exe=""
        conf=\$(systemctl show -p ExecStart "\$u.service" 2>/dev/null | head -1 | sed -n 's/^ExecStart={[ ]*path=\\([^;]*\\).*/\\1/p' | sed 's/[[:space:]]*\$//')
        if [ -z "\$pid" ] || [ "\$pid" = 0 ] || [ -z "\$exe" ] || [ "\$exe" != "\$conf" ]; then
          echo "ERROR: recovery cannot confirm \$u runs the restored executable (pid=\$pid exe=\$exe configured=\$conf); treating as incomplete" >&2
          rc=1
        fi
        ;;
      inactive|failed)
        # (b) the unit was NOT running before: undo any start of it. Active
        # OR undeterminable -> conservatively stop, then CONFIRM inactive.
        qstate "\$u"; st=\$?
        if [ "\$st" != 1 ]; then
          sudo -n systemctl stop "\$u.service" 2>/dev/null || rc=1
          qstate "\$u"; st2=\$?
          if [ "\$st2" != 1 ]; then
            echo "ERROR: recovery cannot confirm \$u.service was stopped (query state=\$st2); treating as incomplete" >&2
            rc=1
          fi
        fi
        ;;
      *)
        echo "[install] recovery cannot confirm the previous run-state of \$u ('\$ac'); treating as incomplete" >&2
        rc=1 ;;
    esac
  done
  if [ "\$rc" = 0 ]; then
    echo "[install] RECOVERY OK: previous binary/units restored, enable/active state re-asserted and running images verified (state: \$sw_state)" >&2
  else
    echo "ERROR: RECOVERY INCOMPLETE — services may still be down; manual restore required from \$BACKUP (captured state: \$sw_state)" >&2
  fi
  return "\$rc"
}
do_switchover() {
  # NOTE: this function is invoked via 'if ! do_switchover', so set -e is
  # SUSPENDED for its whole body — every step therefore carries an explicit
  # '|| return 1'; a bare failing step would otherwise be silently ignored,
  # which is worse than aborting into recovery.
  # 1) stop. K 复核 K2-R2: a stop failure of an EXISTING unit is NOT
  #    swallowed — the old code '|| true'd it, kept deploying, and
  #    enable --now then no-opped on the still-active old process (running
  #    the OLD inode) while the disk path already reported the new version.
  #    K 复核 K3-N1: absent units are skipped instead of erroring.
  stop_unit ak-registry-server || return 1
  stop_unit ak-stun-server || return 1
  if [ -n "\$OLD_REGISTRY_UNIT" ]; then
    echo "[install] stopping old registry unit \$OLD_REGISTRY_UNIT (replaced by ak-registry-server.service; unit file kept in \$BACKUP)"
    stop_unit "\$OLD_REGISTRY_UNIT" || return 1
    sudo -n systemctl disable "\$OLD_REGISTRY_UNIT" 2>/dev/null || true
  fi
  # §0 step 2: retire a legacy signal-server that owns UDP $STUN_PORT. The
  # handover belongs INSIDE the protected window (K 复核 K2-N1): outside it,
  # a later failure could leave the legacy unit stopped with nothing owning
  # the port.
  if [ -n "\$LEGACY_UNIT" ]; then
    echo "[install] stopping legacy unit \$LEGACY_UNIT (UDP $STUN_PORT handover to stun-server; §0)"
    stop_unit "\$LEGACY_UNIT" || return 1
    sudo -n systemctl disable "\$LEGACY_UNIT" 2>/dev/null || true
    echo "[install] rollback for this step = systemctl stop ak-stun-server && systemctl start \$LEGACY_UNIT"
  fi
  # 2) binary swap (K 复核 K-R1: staging source != destination, verified
  #    above BEFORE anything was stopped). K 复核 K2-N1(c): the move-aside
  #    of the previous binary must not be '|| true'd away — if the rename
  #    fails, say so and keep relying on the pre-stop backup below.
  sudo -n install -m 0755 "\$SRC_BIN" $REMOTE_BASE/ak-cli.new || return 1
  if [ "\$HAD_OLD_BIN" = 1 ]; then
    if ! sudo -n mv $REMOTE_BIN $REMOTE_BASE/ak-cli.old-\$TS 2>/dev/null; then
      echo "[install] could not move the previous binary aside; the pre-stop backup \$BACKUP/ak-cli stays the restore source (K2-N1)" >&2
    fi
  fi
  sudo -n mv $REMOTE_BASE/ak-cli.new $REMOTE_BIN || return 1
  sudo -n chmod 755 $REMOTE_BIN || return 1
  echo "[install] installed: $REMOTE_BIN (\$($REMOTE_BIN --version 2>&1 || echo version-unreadable))"
  # 3) unit writes. K 复核 K2-N1: these used to run BARE under set -e AFTER
  #    the recovery branch had already ended — a failure at the FIRST unit
  #    write left both services stopped with nothing started again.
  #    Unit bodies are inlined as single-level quoted heredocs: every \$-free
  #    var below is expanded at GENERATION time (this outer heredoc is
  #    unquoted), so the remote script writes the final text verbatim. Never
  #    nest \$(...) or \$(cat <<...) here — the outer heredoc would execute it
  #    locally.
  sudo -n tee /etc/systemd/system/ak-registry-server.service >/dev/null <<'REGUNIT' || return 1
[Unit]
Description=AK-sending v2 pairing registry (coordination only, ADR-001)
After=network-online.target
Wants=network-online.target

[Service]
User=$SERVICE_USER
ExecStart=${REGISTRY_EXEC}
Environment=RUST_LOG=info
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=${REG_RW}
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
REGUNIT
  sudo -n tee /etc/systemd/system/ak-stun-server.service >/dev/null <<'STUNUNIT' || return 1
[Unit]
Description=AK-sending standalone STUN responder (binding only, ADR-001)
After=network-online.target
Wants=network-online.target

[Service]
User=$SERVICE_USER
ExecStart=$REMOTE_BIN stun-server --port $STUN_PORT
Environment=RUST_LOG=info
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
STUNUNIT
  echo "[install] units written: ak-registry-server.service (ReadWritePaths=$REG_RW), ak-stun-server.service"
  # 4) reload + start (still inside the protected window; enable --now does
  #    NOT restart an already-active unit, which is why step 2 must not
  #    tolerate stop failures — K 复核 K2-R2).
  sudo -n systemctl daemon-reload || return 1
  sudo -n systemctl enable --now ak-registry-server.service ak-stun-server.service || return 1
  return 0
}
if ! do_switchover; then
  recover_services || true
  echo "ERROR: service switchover failed; recovery attempted (outcome reported above; backup in \$BACKUP)" >&2
  exit 5
fi

# --- firewall (§6): TCP $REGISTRY_PORT + UDP $STUN_PORT ---------------------
# K 复核 K2-R3: 'ufw status' needs root; probed through sudo like every
# other read-only discovery so an inactive firewall is not silently assumed.
if command -v ufw >/dev/null 2>&1 && sudo -n ufw status 2>/dev/null | grep -q 'Status: active'; then
  sudo -n ufw allow $REGISTRY_PORT/tcp comment 'ak-registry TLS'
  sudo -n ufw allow $STUN_PORT/udp comment 'ak-stun'
  echo "[firewall] ufw: allowed $REGISTRY_PORT/tcp + $STUN_PORT/udp"
  if [ "${CLOSE_7100}" = 1 ]; then
    sudo -n ufw --force delete allow 7100/tcp >/dev/null 2>&1 && echo "[firewall] ufw: removed 7100/tcp (legacy signal)" || echo "[firewall] ufw: no 7100/tcp rule to remove"
  fi
else
  echo "[firewall] ufw absent or inactive — open in the CLOUD SECURITY GROUP: TCP $REGISTRY_PORT, UDP $STUN_PORT (Tencent console); §6 also recommends closing public TCP 7100 there"
fi
# K 复核 K-R1: the /tmp staging upload is temporary; removed on success
# (kept on failure together with \$BACKUP for forensics).
[ "\$PREBUILT_MODE" = 1 ] && [ -n "\$STAGING_DIR" ] && rm -rf "\$STAGING_DIR" 2>/dev/null || true
exit 0
EOF
}

# Phase 7: verification (read-only). Env: REGISTRY_PORT, STUN_PORT, TS.
write_phase7() { # out-file
  cat > "$1" <<EOF
set -eu
sleep 2
for u in ak-registry-server ak-stun-server; do
  st=\$(systemctl is-active \$u.service 2>/dev/null || true)
  echo "unit \$u: \$st"
  [ "\$st" = active ] || { journalctl -u \$u.service -n 30 --no-pager 2>/dev/null | tail -20; exit 6; }
done
echo "--- registry journal ---"
journalctl -u ak-registry-server.service --since '-5 min' --no-pager 2>/dev/null | grep -E 'registry-server version:|v2 registry server listening on tls|registry id \(pin with' | tail -5
echo "--- stun journal ---"
journalctl -u ak-stun-server.service --since '-5 min' --no-pager 2>/dev/null | grep -E 'stun-server version:|stun responder' | tail -5
echo "--- sockets ---"
ss -ltn | grep -E "[:.]$REGISTRY_PORT\b" || { echo "ERROR: nothing listening on tcp $REGISTRY_PORT" >&2; exit 6; }
ss -lun | grep -E "[:.]$STUN_PORT\\b" || { echo "ERROR: nothing bound on udp $STUN_PORT" >&2; exit 6; }
echo "tcp $REGISTRY_PORT + udp $STUN_PORT listening"
echo "--- versions (record in the deploy ticket) ---"
$REMOTE_BIN registry-server --version
$REMOTE_BIN stun-server --version
# K 复核 K-R2/K2-R2: prove the RUNNING PROCESS executes the deployed binary.
# ExecStart is only the unit's CONFIGURATION: the old check read that
# configured path and ran --version on it, while an old process (stop
# failure swallowed, then enable --now no-opping on the still-active unit)
# kept running the OLD inode — the disk path reported the new version and
# both install and verify exited 0 in exactly that state (reviewed
# counterexample). The decisive proof is per unit: resolve MainPID, read the
# process's actual executable via /proc/<PID>/exe (sudo readlink: the unit
# runs as the dedicated service user and this script's SSH user cannot
# readlink another user's /proc/<PID>/exe), and compare it to the deployed
# binary.
for u in ak-registry-server ak-stun-server; do
  # (a) configuration sanity: what the unit is CONFIGURED to run, and the
  #     version that configured file reports.
  live_exe=\$(systemctl show -p ExecStart \$u.service 2>/dev/null | head -1 | sed -n 's/^ExecStart={[ ]*path=\\([^;]*\\).*/\\1/p' | sed 's/[[:space:]]*\$//')
  [ -n "\$live_exe" ] || { echo "ERROR: cannot determine the configured ExecStart executable of \$u (systemctl show; K-R2)" >&2; exit 6; }
  [ "\$live_exe" = "$REMOTE_BIN" ] || { echo "ERROR: \$u is configured to run \$live_exe but this deploy installed $REMOTE_BIN (K-R2 stale configuration)" >&2; exit 6; }
  live_ver=\$(\$live_exe --version 2>&1)
  case "\$live_ver" in
    *"git $SHORT_SHA"*) : ;;
    *) echo "ERROR: \$u is configured for \$live_exe reporting '\$live_ver' (want 'git $SHORT_SHA'; K-R2)" >&2; exit 6 ;;
  esac
  # (b) running-process proof (K 复核 K2-R2): MainPID + /proc/<PID>/exe.
  pid=\$(systemctl show -p MainPID --value \$u.service 2>/dev/null)
  case "\$pid" in
    ''|0) echo "ERROR: \$u has no MainPID; cannot attribute a running process to this deploy (K2-R2)" >&2; exit 6 ;;
  esac
  exe=\$(sudo -n readlink "/proc/\$pid/exe" 2>/dev/null) \\
    || { echo "ERROR: cannot read /proc/\$pid/exe of \$u via sudo (K2-R2 running-process check)" >&2; exit 6; }
  case "\$exe" in
    *' (deleted)') echo "ERROR: \$u MainPID \$pid executes a DELETED binary (\$exe) — not the freshly deployed file (K2-R2)" >&2; exit 6 ;;
  esac
  [ "\$exe" = "$REMOTE_BIN" ] || { echo "ERROR: \$u MainPID \$pid executes \$exe, not the deployed $REMOTE_BIN (K2-R2 running-process mismatch — an old process still on the old inode?)" >&2; exit 6; }
  echo "unit \$u runs \$live_exe (verified: MainPID \$pid /proc exe -> \$exe)"
done
# Export the registry pin (public hex; §5) for scripts/cross_nat_test.sh.
PIN=\$(journalctl -u ak-registry-server.service --since '-5 min' --no-pager 2>/dev/null | sed -n 's/.*registry id (pin with --registry-fp): \\([0-9a-f]\\{64\\}\\).*/\\1/p' | tail -1)
if [ -n "\$PIN" ]; then
  printf '%s\n' "\$PIN" | sudo -n tee $REMOTE_BASE/registry-id >/dev/null
  sudo -n chmod 644 $REMOTE_BASE/registry-id
  echo "registry id (pin with --registry-fp): \$PIN"
else
  echo "WARNING: could not extract the registry id from the journal" >&2
fi
exit 0
EOF
}

# ---------------------------------------------------------------------------
# Real deployment
# ---------------------------------------------------------------------------
real_run() {
  [ -n "$SSH_USER" ] || { echo "missing --ssh-user (see --help)" >&2; exit 2; }
  [ -n "$SSH_HOST" ] || { echo "missing --ssh-host (see --help)" >&2; exit 2; }

  WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-deploy.XXXXXXXX")
  umask 077

  # Resolve the pinned revision and short sha BEFORE anything else.
  phase "0/7 local prep (revision, backend)"
  if [ -n "$PREBUILT" ]; then
    [ -f "$PREBUILT" ] || fail "--binary not found: $PREBUILT"
    PREBUILT=$(cd "$(dirname "$PREBUILT")" && pwd)/$(basename "$PREBUILT")
    SHORT_SHA=$( cd "$ROOT" && git rev-parse --short "$COMMIT" )
  else
    ( cd "$ROOT" && git rev-parse --verify "$COMMIT^{commit}" >/dev/null ) \
      || fail "unknown revision: $COMMIT"
    SHORT_SHA=$( cd "$ROOT" && git rev-parse --short "$COMMIT" )
    ( cd "$ROOT" \
      && git archive --format=tar.gz --prefix=ak-deploy/ -o "$WORK/src-$SHORT_SHA.tar.gz" "$COMMIT" ) \
      || fail "git archive failed"
    note "source tarball: src-$SHORT_SHA.tar.gz ($(du -h "$WORK/src-$SHORT_SHA.tar.gz" | cut -f1))"
  fi
  note "revision: $COMMIT (short sha $SHORT_SHA)"
  note "binary version line must report: git $SHORT_SHA"

  PASSFILE="$WORK/.sshpass"
  [ -n "$SSH_PASS" ] && printf '%s' "$SSH_PASS" > "$PASSFILE"
  detect_ssh_backend
  [ "$SSH_BACKEND" != none ] || fail "no SSH backend (use key auth, or: pip install paramiko + --ssh-pass)"
  case "$SSH_BACKEND" in
    openssh)  note "ssh backend: openssh (key: ${SSH_KEY:-default}) -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
    sshpass)  note "ssh backend: sshpass -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
    paramiko) note "ssh backend: paramiko -> $SSH_USER@$SSH_HOST:$SSH_PORT" ;;
  esac
  remote_run_quiet true || fail "SSH connection failed (backend $SSH_BACKEND; see $WORK/remote-stderr.log)"

  # ---- phase 1: preflight inspection --------------------------------------
  phase "1/7 remote preflight"
  write_phase1 "$WORK/phase1.sh"
  remote_script "$WORK/phase1.sh" | tee "$WORK/phase1.log" \
    || fail "preflight failed (see $WORK/phase1.log + remote-stderr.log)"
  if grep -q '^ERROR:' "$WORK/phase1.log"; then
    fail "preflight reported an error: $(grep -m1 '^ERROR:' "$WORK/phase1.log")"
  fi
  if ! grep -q '^sudo: passwordless-ok' "$WORK/phase1.log"; then
    fail "the SSH user needs passwordless sudo for install steps (sudo -n true failed)"
  fi
  local existing_data="" legacy_unit="" existing_exec="" old_registry_unit=""
  # K 复核 K9-N1: the migration source is the RUNTIME-ESTABLISHED identity
  # of the identified occupant (phase1's runtime-id-dir: explicit --id-dir
  # of the effective ExecStart, else the environment-derived CLI default) —
  # NEVER the first enumerated directory. Directory enumeration only
  # REPORTS candidates: with NO identified occupant, exactly one candidate
  # may be selected (fresh install with legacy data); multiple candidates
  # are unresolvable ambiguity (refuse instead of head -1).
  runtime_id=$(sed -n 's/^  runtime-id-dir: \(.*\)$/\1/p' "$WORK/phase1.log" | head -1)
  data_candidates=$(sed -n 's/^registry-data: \(.*\) (key:.*/\1/p' "$WORK/phase1.log")
  n_candidates=$(printf '%s\n' "$data_candidates" | grep -c . || true)
  if [ -n "$runtime_id" ]; then
    existing_data=$runtime_id
  elif [ "$n_candidates" = 1 ]; then
    existing_data=$(printf '%s\n' "$data_candidates" | head -1)
  elif [ "$n_candidates" -gt 1 ]; then
    fail "multiple identity directories found with no identified occupant to bind the migration source ($(printf '%s ' $data_candidates)); resolve manually and re-run"
  else
    existing_data=""
  fi
  # Live old unit's ExecStart is dumped AFTER the standard-unit loop, so the
  # last match wins (the live process is the authoritative argument source).
  existing_exec=$(sed -n "s/^  ExecStart=\(.*registry-server.*\)$/\1/p" "$WORK/phase1.log" | tail -1)
  legacy_unit=$(sed -n "s/^port $STUN_PORT\/udp: in use (pid [0-9unknown]*, unit \(.*\))$/\1/p" "$WORK/phase1.log" | head -1)
  old_registry_unit=$(sed -n 's/^  old-registry-unit: \(.*\)$/\1/p' "$WORK/phase1.log" | head -1)
  # Normalize: keep the bare unit name (no .service suffix); systemctl and
  # the backup loop re-add it.
  legacy_unit=${legacy_unit%.service}
  old_registry_unit=${old_registry_unit%.service}
  # K 复核 K2-R3 (round 4): the decision below refused an UNKNOWN STUN
  # occupant but silently accepted an unknown REGISTRY occupant (empty
  # old_registry_unit) — the deploy then collided with it at enable time.
  # With sudo discovery an in-use registry port whose pid/unit cannot be
  # identified must refuse too, BEFORE anything is stopped. The occupant
  # verdict comes from the PORT line itself (the old-registry-unit line only
  # exists when a systemd unit was actually identified).
  if grep -q "^port $REGISTRY_PORT/tcp: in use (pid unknown" "$WORK/phase1.log"; then
    fail "TCP $REGISTRY_PORT is in use but its owning process cannot be identified; resolve manually (sudo ss -ltnp) and re-run"
  fi
  reg_occ=$(sed -n "s/^port $REGISTRY_PORT\/tcp: in use (pid [0-9unknown]*, unit \(.*\))$/\1/p" "$WORK/phase1.log" | head -1)
  if [ "$reg_occ" = "not-a-systemd-unit" ]; then
    fail "TCP $REGISTRY_PORT is held by a non-systemd process; resolve manually (sudo ss -ltnp) and re-run"
  fi
  note "existing registry data dir: ${existing_data:-none-found}"
  note "existing registry ExecStart: ${existing_exec:-none}"
  note "registry-port occupant unit: ${old_registry_unit:-none-detected}"
  note "UDP $STUN_PORT occupant: ${legacy_unit:-free}"
  if [ -n "$old_registry_unit" ] && [ "$old_registry_unit" != "ak-registry-server" ]; then
    warn "old registry unit '$old_registry_unit.service' will be stopped+disabled and replaced by ak-registry-server.service (its unit file is backed up)"
  fi
  if grep -q "unit ak-registry-server: present" "$WORK/phase1.log"; then
    note "existing ak-registry-server unit will be upgraded (ExecStart preserved unless it lacked --id-dir)"
  fi
  # Legacy signal conflict decision (§0 step 2).
  if [ -n "$legacy_unit" ] && [ "$legacy_unit" != "ak-stun-server.service" ]; then
    if [ "$legacy_unit" = "not-a-systemd-unit" ]; then
      fail "UDP $STUN_PORT is held by a non-systemd process; resolve manually (ss -lunp) and re-run"
    fi
    if [ "$KEEP_LEGACY_SIGNAL" -eq 1 ]; then
      fail "UDP $STUN_PORT is held by legacy unit '$legacy_unit'; re-run without --keep-legacy-signal to perform the §0 handover, or stop it manually"
    fi
    warn "legacy unit '$legacy_unit' will be STOPPED and DISABLED (stun-server takes UDP $STUN_PORT)"
  fi
  if [ "$CHECK_ONLY" -eq 1 ]; then
    note "--check-only: no changes made; full report above"
    exit 0
  fi

  # ---- phase 2: upload ------------------------------------------------------
  phase "2/7 upload"
  TS=$(date +%Y%m%d-%H%M%S)
  BUILD_DIR="/tmp/ak-deploy-build-$TS"
  PREBUILT_MODE=0
  STAGING_DIR=""
  STAGED_BIN=""
  if [ -n "$PREBUILT" ]; then
    PREBUILT_MODE=1
    # K 复核 K-R1: upload the prebuilt binary to a DISTINCT staging path under
    # /tmp. The old flow uploaded straight to $REMOTE_BASE/ak-cli.new and the
    # install step then ran `install ak-cli.new -> ak-cli.new` (source ==
    # destination, GNU install refuses) AFTER the services were already
    # stopped. All checks below run on the STAGED file, before any service is
    # touched; phase456 re-verifies right before the switchover.
    STAGING_DIR="/tmp/ak-deploy-staging-$TS"
    STAGED_BIN="$STAGING_DIR/ak-cli"
    remote_run_quiet "mkdir -p '$STAGING_DIR' && chmod 700 '$STAGING_DIR'" \
      || fail "cannot create remote staging dir $STAGING_DIR"
    remote_put "$PREBUILT" "$STAGED_BIN" \
      || fail "upload of prebuilt binary failed"
    remote_run_quiet "chmod 0755 '$STAGED_BIN'" \
      || fail "cannot set 0755 on the staged binary"
    remote_run_quiet "test -x '$STAGED_BIN'" \
      || fail "staged binary is not executable on the server"
    local pv local_sha rsha
    pv=$(remote_run_quiet "'$STAGED_BIN' --version" || true)
    case "$pv" in
      *"git $SHORT_SHA"*) note "staged binary version verified: $pv" ;;
      *) fail "prebuilt binary version mismatch (want 'git $SHORT_SHA', got: $pv)" ;;
    esac
    local_sha=$(sha256sum "$PREBUILT" | cut -d' ' -f1 | tr 'A-F' 'a-f')
    rsha=$(remote_run_quiet "sha256sum '$STAGED_BIN' 2>/dev/null" | cut -d' ' -f1 | tr -d '\r\n' | tr 'A-F' 'a-f' || true)
    [ "$rsha" = "$local_sha" ] \
      || fail "staged binary sha256 mismatch (local $local_sha, remote ${rsha:-none}) — upload corrupted?"
    note "staged at $STAGED_BIN (sha256 ${local_sha:0:16}...); checks done BEFORE service switchover"
  else
    remote_run_quiet "rm -rf '$BUILD_DIR' && mkdir -p '$BUILD_DIR'" \
      || fail "cannot prepare remote build dir"
    remote_put "$WORK/src-$SHORT_SHA.tar.gz" "$BUILD_DIR/src.tar.gz" \
      || fail "tarball upload failed"
    remote_run_quiet "tar -xzf '$BUILD_DIR/src.tar.gz' -C '$BUILD_DIR' --strip-components=1" \
      || fail "remote tar extraction failed"
    note "source extracted to $BUILD_DIR on the server"
  fi

  # ---- phase 3: build -------------------------------------------------------
  phase "3/7 build (on the server)"
  if [ "$PREBUILT_MODE" -eq 0 ]; then
    write_phase3 "$WORK/phase3.sh" "$BUILD_DIR"
    remote_script "$WORK/phase3.sh" | tee "$WORK/phase3.log" \
      || fail "server-side build failed (last lines above; full log: $BUILD_DIR/build.log on the server)"
    grep -q "built: ak-cli" "$WORK/phase3.log" \
      || fail "build log missing the version gate line"
  else
    note "skipped (--binary): using the uploaded prebuilt binary"
  fi

  # ---- deployment decisions for phase 4-6 ----------------------------------
  # Registry ExecStart (K 复核 K-R2): compose_registry_exec preserves the
  # existing unit's ARGUMENTS (identity/database paths, port) but rewrites
  # the EXECUTABLE to the newly installed+verified $REMOTE_BIN — the old
  # verbatim-preserve let systemd keep running /usr/local/bin/ak-cli while
  # the phase-7 check ran the new path. If the existing unit lacks an
  # explicit --id-dir, it is normalized to $REMOTE_DATA (data migration below).
  REGISTRY_EXEC=$(compose_registry_exec "${existing_exec:-}")
  note "registry ExecStart for the new unit: $REGISTRY_EXEC"
  # Data source for identity/db migration: the existing data dir found in
  # preflight, unless it IS the standard layout already.
  DATA_SOURCE_DIR=""
  [ -n "$existing_data" ] && [ "$existing_data" != "$REMOTE_DATA" ] && DATA_SOURCE_DIR=$existing_data
  [ -n "$DATA_SOURCE_DIR" ] && warn "registry identity/db will be COPIED (never moved) from $DATA_SOURCE_DIR into $REMOTE_DATA"
  # If a preserved ExecStart points --id-dir elsewhere, keep ReadWritePaths
  # pointed at THAT dir instead of the standard data dir.
  REG_RW=$REMOTE_DATA
  if [ -n "$existing_exec" ] && printf '%s' "$existing_exec" | grep -q -- '--id-dir'; then
    REG_RW=$(printf '%s' "$existing_exec" | sed -n 's/.*--id-dir[= ]\([^ ]*\).*/\1/p' | head -1)
    [ -n "$REG_RW" ] || REG_RW=$REMOTE_DATA
  fi
  REGISTRY_EXEC=${REGISTRY_EXEC//\'/}   # unit files must not carry quotes
  LEGACY_UNIT=${legacy_unit:-}
  OLD_REGISTRY_UNIT=${old_registry_unit:-}
  [ "$LEGACY_UNIT" = "ak-stun-server" ] && LEGACY_UNIT=""
  [ "$OLD_REGISTRY_UNIT" = "ak-registry-server" ] && OLD_REGISTRY_UNIT=""

  # ---- phase 4/5/6: install + systemd + firewall ----------------------------
  phase "4-6/7 install, systemd, firewall"
  write_phase456 "$WORK/phase456.sh"
  remote_script "$WORK/phase456.sh" | tee "$WORK/phase456.log" \
    || fail "install phase failed (see $WORK/phase456.log)"

  # ---- phase 7: verify -------------------------------------------------------
  phase "7/7 verify"
  write_phase7 "$WORK/phase7.sh"
  remote_script "$WORK/phase7.sh" | tee "$WORK/phase7.log" \
    || fail "verification failed (see $WORK/phase7.log)"
  grep -q 'registry-server version:' "$WORK/phase7.log" \
    || fail "journal missing 'registry-server version:' line"
  grep -q 'v2 registry server listening on tls' "$WORK/phase7.log" \
    || fail "journal missing 'v2 registry server listening on tls' line"
  grep -qi 'stun responder' "$WORK/phase7.log" \
    || fail "journal missing the stun responder startup line"
  grep -q 'listening' "$WORK/phase7.log" \
    || fail "sockets not listening after start"
  grep -q "unit ak-registry-server runs $REMOTE_BIN" "$WORK/phase7.log" \
    || fail "phase 7 did not verify the RUNNING registry executable against the deployed binary (K-R2)"

  local pin
  pin=$(sed -n 's/^registry id (pin with --registry-fp): \([0-9a-f]\{64\}\)$/\1/p' "$WORK/phase7.log" | tail -1)
  if [ -n "$pin" ] && [ "$pin" = "$DOCUMENTED_PIN" ]; then
    note "registry identity matches the pin documented in docs/OPERATIONS.zh-CN.md §3 ($DOCUMENTED_PIN)"
  elif [ -n "$pin" ]; then
    note "registry id (new pin): $pin"
    warn "this identity differs from the documented production pin ($DOCUMENTED_PIN) — if clients pin the old value they must be re-pinned (--registry-fp / AK_REGISTRY_ID, §5)"
  fi

  if [ "$KEEP_BUILD_DIR" -ne 1 ] && [ "$PREBUILT_MODE" -eq 0 ]; then
    remote_run_quiet "rm -rf '$BUILD_DIR'" || true
  else
    [ "$PREBUILT_MODE" -eq 0 ] && note "remote build dir kept: $BUILD_DIR"
  fi

  phase "summary"
  note "binary:      $REMOTE_BIN ($(grep -m1 'installed:' "$WORK/phase456.log" || true))"
  note "units:       ak-registry-server.service (tcp $REGISTRY_PORT TLS), ak-stun-server.service (udp $STUN_PORT)"
  note "data:        $REG_RW (registry.key + registry.db; backups under $REMOTE_BASE/backup-*)"
  note "registry id: ${pin:-see /opt/ak-sending/registry-id}"
  note "next steps:"
  note "  1) from OUTSIDE the server, verify STUN: ak-cli doctor --stun $SSH_HOST:$STUN_PORT  (expect 查询成功)"
  note "  2) full cross-NAT QUIC transfer test: bash scripts/cross_nat_test.sh --ssh-user $SSH_USER --ssh-host $SSH_HOST $( [ -n "$SSH_PASS" ] && printf %s '--ssh-pass <password>' || printf %s '[key auth]' )"
  note "  3) cloud security group: ensure TCP $REGISTRY_PORT + UDP $STUN_PORT open (TCP 7100 can be closed, §6)"
  note "rollback:    systemctl stop ak-registry-server ak-stun-server; restore $REMOTE_BASE/backup-$TS/* (see §4 for schema-migration caveats)"
  note "RESULT: deploy OK"
}

# ---------------------------------------------------------------------------
# Entry
# ---------------------------------------------------------------------------
if [ "$SELF_TEST" -eq 1 ]; then
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/ak-deploy-selftest.XXXXXXXX")
  : "${SSH_USER:=ubuntu}"
  : "${SSH_HOST:=127.0.0.1}"
  self_test
else
  real_run
fi
