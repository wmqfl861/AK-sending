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
#      (units, --id-dir, registry data, UDP <stun-port> occupant)
#   2. upload: `git archive` tarball of <commit> (or --binary) to /tmp
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
#      rollback note (§0 migration step 2); TCP 7100 stay-open reminder
#   6. firewall: ufw allow <registry-port>/tcp + <stun-port>/udp when ufw is
#      active; otherwise prints the cloud security-group checklist
#   7. verify: units active, journal version lines, listening sockets,
#      --version recorded, registry id exported to /opt/ak-sending/registry-id
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
import sys

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


def main() -> int:
    if len(sys.argv) < 6:
        print(__doc__, file=sys.stderr)
        return 254
    mode = sys.argv[1]
    client = connect(sys.argv)
    try:
        if mode == "exec":
            command = sys.stdin.read()
            _, stdout, stderr = client.exec_command(command, timeout=0)
            out = stdout.read()
            err = stderr.read()
            status = stdout.channel.recv_exit_status()
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

  echo "[self-test] s1/6 argument parsing"
  sub=0; bash "$0" --self-test --nope >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "unknown option should exit 2, got $sub"
  sub=0; bash "$0" --self-test --ssh-port abc >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "bad --ssh-port should exit 2, got $sub"
  sub=0; bash "$0" >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 2 ] || fail "missing required options should exit 2, got $sub"
  sub=0; bash "$0" --help >/dev/null 2>&1 || sub=$?
  [ "$sub" -eq 0 ] || fail "--help should exit 0, got $sub"
  echo "[self-test]   4 validation sub-cases behaved"

  echo "[self-test] s2/6 unit templates"
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

  echo "[self-test] s3/6 git archive of the pinned commit"
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

  echo "[self-test] s4/6 version sha extraction"
  # Global on purpose: write_phase3 embeds it when s6 generates the build
  # script, mirroring how real_run sets it before any phase runs.
  SHORT_SHA=$( cd "$ROOT" && git rev-parse --short HEAD )
  case "$SHORT_SHA" in
    ''|*[!0-9a-f]*) fail "short sha malformed: $SHORT_SHA" ;;
  esac
  echo "[self-test]   pinned sha: $SHORT_SHA"
  [ -n "$PREBUILT" ] && [ ! -f "$PREBUILT" ] \
    && fail "--binary not found: $PREBUILT"

  echo "[self-test] s5/6 ssh helper compiles"
  write_ssh_helper
  PYTHON=$(command -v python || command -v python3 || true)
  if [ -n "$PYTHON" ] && "$PYTHON" -c 'import paramiko' >/dev/null 2>&1; then
    "$PYTHON" -m py_compile "$WORK/ssh_helper.py" \
      || fail "ssh_helper.py does not compile"
    echo "[self-test]   ssh_helper.py compiles clean (paramiko backend available)"
  else
    echo "[self-test]   paramiko not installed locally (password backend would need: pip install paramiko)"
  fi

  echo "[self-test] s6/6 generated remote phase scripts parse"
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
  grep -q '^for u in ak-registry-server ak-stun-server oldregistry oldsignal; do$' "$WORK/p456.sh" \
    || fail "unit-backup loop must bake the discovered old/legacy unit names"
  echo "[self-test]   phase1/3/456/7 generated + bash -n clean"

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
[ -x $REMOTE_BIN ] && echo "existing-binary: yes" || echo "existing-binary: no"
[ -x $REMOTE_BIN ] && echo "existing-version: \$($REMOTE_BIN --version 2>&1 || echo unreadable)"
for u in ak-registry-server ak-stun-server; do
  if [ -f /etc/systemd/system/\$u.service ]; then
    echo "unit \$u: present"
    sed -n 's/^ExecStart=/  ExecStart=/p' /etc/systemd/system/\$u.service
    sed -n 's/^User=/  User=/p' /etc/systemd/system/\$u.service
    systemctl is-active \$u.service >/dev/null 2>&1 && echo "  active: yes" || echo "  active: no"
  else
    echo "unit \$u: absent"
  fi
done
# Live listener on the registry port -> authoritative id-dir + old unit name
# (the existing production unit name is NOT verified per docs §0/§3, so it is
# discovered from the port, not assumed).
reg_line=\$(ss -ltnp 2>/dev/null | grep -E "[:.]$REGISTRY_PORT\b" | head -1 || true)
if [ -n "\$reg_line" ]; then
  reg_pid=\$(printf '%s' "\$reg_line" | sed -n 's/.*pid=\\([0-9]*\\).*/\\1/p')
  reg_unit=\$( [ -n "\${reg_pid:-}" ] && awk -F/ '{print \$NF}' /proc/\$reg_pid/cgroup 2>/dev/null | grep '\\.service\$' | tail -1 || true)
  echo "port $REGISTRY_PORT/tcp: in use (pid \${reg_pid:-unknown}, unit \${reg_unit:-not-a-systemd-unit})"
  [ -n "\${reg_pid:-}" ] && tr '\\0' ' ' < /proc/\$reg_pid/cmdline 2>/dev/null | sed 's/^/  cmdline: /' || true
  if [ -n "\${reg_unit:-}" ] && [ "\$reg_unit" != "ak-registry-server.service" ] && [ -f "/etc/systemd/system/\$reg_unit" ]; then
    echo "  old-registry-unit: \$reg_unit"
    sed -n 's/^ExecStart=/  ExecStart=/p' "/etc/systemd/system/\$reg_unit"
    sed -n 's/^User=/  old-registry-User=/p' "/etc/systemd/system/\$reg_unit"
  fi
else
  echo "port $REGISTRY_PORT/tcp: free"
fi
# Live listener on the STUN port -> the §0 conflict check.
stun_line=\$(ss -lunp 2>/dev/null | grep -E "[:.]$STUN_PORT\\b" | head -1 || true)
if [ -n "\$stun_line" ]; then
  stun_pid=\$(printf '%s' "\$stun_line" | sed -n 's/.*pid=\\([0-9]*\\).*/\\1/p')
  stun_unit=\$( [ -n "\${stun_pid:-}" ] && awk -F/ '{print \$NF}' /proc/\$stun_pid/cgroup 2>/dev/null | grep '\\.service\$' | tail -1 || true)
  echo "port $STUN_PORT/udp: in use (pid \${stun_pid:-unknown}, unit \${stun_unit:-not-a-systemd-unit})"
  [ -n "\${stun_pid:-}" ] && tr '\\0' ' ' < /proc/\$stun_pid/cmdline 2>/dev/null | sed 's/^/  cmdline: /' || true
else
  echo "port $STUN_PORT/udp: free"
fi
sig_line=\$(ss -ltnp 2>/dev/null | grep -E '[:.]7100\b' | head -1 || true)
[ -n "\$sig_line" ] && echo "port 7100/tcp (legacy signal): in use" || echo "port 7100/tcp (legacy signal): free"
# Registry identity/database discovery (never modified in this phase).
for d in $REMOTE_DATA /home/*/.ak-sending /root/.ak-sending; do
  if [ -e "\$d/registry.key" ] || [ -e "\$d/registry.db" ]; then
    echo "registry-data: \$d (key:\$([ -e \$d/registry.key ] && echo yes || echo no) db:\$([ -e \$d/registry.db ] && echo yes || echo no))"
  fi
done
command -v ufw >/dev/null 2>&1 && echo "ufw: \$(ufw status 2>/dev/null | head -1)" || echo "ufw: absent"
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
# BUILD_DIR/PREBUILT_MODE).
write_phase456() { # out-file ...
  cat > "$1" <<EOF
set -eu
TS=$TS
BUILD_DIR=${BUILD_DIR:-}
PREBUILT_MODE=${PREBUILT_MODE:-0}
SRC_BIN=\$BUILD_DIR/target/release/ak-cli
[ "\$PREBUILT_MODE" = 1 ] && SRC_BIN=$REMOTE_BASE/ak-cli.new
[ -x "\$SRC_BIN" ] || { echo "ERROR: built binary not found at \$SRC_BIN" >&2; exit 5; }

# --- service user + directories ------------------------------------------
if ! id -u $SERVICE_USER >/dev/null 2>&1; then
  echo "[install] creating system user $SERVICE_USER"
  sudo -n useradd --system --home-dir $REMOTE_BASE --shell /usr/sbin/nologin $SERVICE_USER
fi
sudo -n install -d -m 0755 -o $SERVICE_USER -g $SERVICE_USER $REMOTE_BASE $REMOTE_DATA

# --- backup current state (§3 step 1; the only rollback path across a
# --- schema migration, §4) ------------------------------------------------
BACKUP=$REMOTE_BASE/backup-\$TS
sudo -n install -d -m 0700 -o $SERVICE_USER -g $SERVICE_USER "\$BACKUP"
[ -f $REMOTE_BIN ] && sudo -n cp -a $REMOTE_BIN "\$BACKUP/ak-cli" && echo "[install] backup: binary -> \$BACKUP/ak-cli"
for f in registry.key registry.db; do
  [ -f "$REMOTE_DATA/\$f" ] && sudo -n cp -a "$REMOTE_DATA/\$f" "\$BACKUP/\$f" && echo "[install] backup: \$f -> \$BACKUP/\$f"
done
# Unit files that are about to be replaced/retired are backed up verbatim.
# The old/legacy unit NAMES are baked in at generation time (they are simple
# unit names, no quoting hazards); \$u is the only remote variable here.
for u in ak-registry-server ak-stun-server${OLD_REGISTRY_UNIT:+ $OLD_REGISTRY_UNIT}${LEGACY_UNIT:+ $LEGACY_UNIT}; do
  [ -f "/etc/systemd/system/\$u.service" ] && sudo -n cp -a "/etc/systemd/system/\$u.service" "\$BACKUP/\$u.service" \
    && echo "[install] backup: unit \$u.service -> \$BACKUP"
done

# --- preserve/migrate registry identity + database ------------------------
DATA_SOURCE=${DATA_SOURCE_DIR:-}
if [ -n "\$DATA_SOURCE" ] && [ "\$DATA_SOURCE" != "$REMOTE_DATA" ]; then
  for f in registry.key registry.db; do
    if [ -f "\$DATA_SOURCE/\$f" ] && [ ! -f "$REMOTE_DATA/\$f" ]; then
      sudo -n cp -a "\$DATA_SOURCE/\$f" "$REMOTE_DATA/\$f"
      echo "[install] migrated identity/db: \$DATA_SOURCE/\$f -> $REMOTE_DATA/\$f (source kept)"
    fi
  done
fi
sudo -n chown -R $SERVICE_USER:$SERVICE_USER $REMOTE_DATA
sudo -n chmod 700 $REMOTE_DATA
[ -f $REMOTE_DATA/registry.key ] && sudo -n chmod 600 $REMOTE_DATA/registry.key

# --- install the new binary ------------------------------------------------
sudo -n systemctl stop ak-registry-server.service 2>/dev/null || true
sudo -n systemctl stop ak-stun-server.service 2>/dev/null || true
OLD_REGISTRY_UNIT=${OLD_REGISTRY_UNIT:-}
if [ -n "\$OLD_REGISTRY_UNIT" ] && [ "\$OLD_REGISTRY_UNIT" != "ak-registry-server.service" ]; then
  echo "[install] stopping old registry unit \$OLD_REGISTRY_UNIT (replaced by ak-registry-server.service; unit file kept in \$BACKUP)"
  sudo -n systemctl stop "\$OLD_REGISTRY_UNIT" 2>/dev/null || true
  sudo -n systemctl disable "\$OLD_REGISTRY_UNIT" 2>/dev/null || true
fi
sudo -n install -m 0755 "\$SRC_BIN" $REMOTE_BASE/ak-cli.new
[ -f $REMOTE_BIN ] && sudo -n mv $REMOTE_BIN $REMOTE_BASE/ak-cli.old-\$TS
sudo -n mv $REMOTE_BASE/ak-cli.new $REMOTE_BIN
sudo -n chmod 755 $REMOTE_BIN
echo "[install] installed: $REMOTE_BIN (\$($REMOTE_BIN --version 2>&1 || echo version-unreadable))"

# --- §0 step 2: retire a legacy signal-server that owns UDP $STUN_PORT ----
LEGACY_UNIT=${LEGACY_UNIT:-}
if [ -n "\$LEGACY_UNIT" ]; then
  echo "[install] stopping legacy unit \$LEGACY_UNIT (UDP $STUN_PORT handover to stun-server; §0)"
  sudo -n systemctl stop "\$LEGACY_UNIT"
  sudo -n systemctl disable "\$LEGACY_UNIT" 2>/dev/null || true
  echo "[install] rollback for this step = systemctl stop ak-stun-server && systemctl start \$LEGACY_UNIT"
fi

# --- systemd units ----------------------------------------------------------
# Unit bodies are inlined as single-level quoted heredocs: every \$-free var
# below is expanded at GENERATION time (this outer heredoc is unquoted), so
# the remote script writes the final text verbatim. Never nest \$(...) or
# \$(cat <<...) here — the outer heredoc would execute it locally.
sudo -n tee /etc/systemd/system/ak-registry-server.service >/dev/null <<'REGUNIT'
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
sudo -n tee /etc/systemd/system/ak-stun-server.service >/dev/null <<'STUNUNIT'
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
sudo -n systemctl daemon-reload
sudo -n systemctl enable --now ak-registry-server.service ak-stun-server.service

# --- firewall (§6): TCP $REGISTRY_PORT + UDP $STUN_PORT ---------------------
if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q 'Status: active'; then
  sudo -n ufw allow $REGISTRY_PORT/tcp comment 'ak-registry TLS'
  sudo -n ufw allow $STUN_PORT/udp comment 'ak-stun'
  echo "[firewall] ufw: allowed $REGISTRY_PORT/tcp + $STUN_PORT/udp"
  if [ "${CLOSE_7100}" = 1 ]; then
    sudo -n ufw --force delete allow 7100/tcp >/dev/null 2>&1 && echo "[firewall] ufw: removed 7100/tcp (legacy signal)" || echo "[firewall] ufw: no 7100/tcp rule to remove"
  fi
else
  echo "[firewall] ufw absent or inactive — open in the CLOUD SECURITY GROUP: TCP $REGISTRY_PORT, UDP $STUN_PORT (Tencent console); §6 also recommends closing public TCP 7100 there"
fi
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
  existing_data=$(sed -n 's/^registry-data: \(.*\) (key:.*/\1/p' "$WORK/phase1.log" | head -1)
  # Live old unit's ExecStart is dumped AFTER the standard-unit loop, so the
  # last match wins (the live process is the authoritative argument source).
  existing_exec=$(sed -n "s/^  ExecStart=\(.*registry-server.*\)$/\1/p" "$WORK/phase1.log" | tail -1)
  legacy_unit=$(sed -n "s/^port $STUN_PORT\/udp: in use (pid [0-9unknown]*, unit \(.*\))$/\1/p" "$WORK/phase1.log" | head -1)
  old_registry_unit=$(sed -n 's/^  old-registry-unit: \(.*\)$/\1/p' "$WORK/phase1.log" | head -1)
  # Normalize: keep the bare unit name (no .service suffix); systemctl and
  # the backup loop re-add it.
  legacy_unit=${legacy_unit%.service}
  old_registry_unit=${old_registry_unit%.service}
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
  if [ -n "$PREBUILT" ]; then
    PREBUILT_MODE=1
    remote_run_quiet "sudo -n install -d -m 0755 '$REMOTE_BASE'" || fail "cannot create $REMOTE_BASE"
    remote_put "$PREBUILT" "$REMOTE_BASE/ak-cli.new" \
      || fail "upload of prebuilt binary failed"
    local pv
    pv=$(remote_run_quiet "$REMOTE_BASE/ak-cli.new --version" || true)
    case "$pv" in
      *"git $SHORT_SHA"*) note "prebuilt binary verified: $pv" ;;
      *) fail "prebuilt binary version mismatch (want 'git $SHORT_SHA', got: $pv)" ;;
    esac
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
  # Registry ExecStart: preserve an existing unit's arguments (docs §3 note:
  # never silently change --id-dir/--db-path). If the existing unit lacks an
  # explicit --id-dir, normalize to $REMOTE_DATA (with data migration below).
  REGISTRY_EXEC=""
  if [ -n "$existing_exec" ]; then
    if printf '%s' "$existing_exec" | grep -q -- '--id-dir'; then
      REGISTRY_EXEC=$existing_exec
      note "preserving existing registry ExecStart: $REGISTRY_EXEC"
    else
      REGISTRY_EXEC="$existing_exec --id-dir $REMOTE_DATA"
      note "existing ExecStart lacked --id-dir; appending $REMOTE_DATA: $REGISTRY_EXEC"
    fi
  else
    REGISTRY_EXEC="$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA"
    note "fresh registry ExecStart: $REGISTRY_EXEC"
  fi
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
