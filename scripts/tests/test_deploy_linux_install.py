"""K 复核 K-R1/K-R2/K2-N1/K2-R2/K2-R3/K3-N1 offline regressions for
scripts/deploy_linux.sh.

The REAL production generators (compose_registry_exec, write_phase1,
write_phase456, write_phase7) are evaluated as real Bash and the scripts
they generate are EXECUTED. Only the privileged remote surface is stubbed on
PATH (sudo, install, systemctl, tee, chown, useradd, uname, journalctl, ss,
sleep, test, cp, cat, sed, readlink, ls, mv); every baked path points into a
per-test probe directory, so no real /opt, /etc or /proc state is touched.
The systemctl stub is STATEFUL: it models enabled/active state, per-unit
MainPIDs, 'start'-on-active as a NO-OP (systemd semantics), and the RUNNING
BINARY (a copy of the bytes the unit's ExecStart pointed at when it was last
started/restarted), so tests assert which IMAGE a unit actually runs after a
switchover or a recovery — not just which calls appear in a log. Covered,
for the --binary flow:
  - K-R1: DISTINCT staging source; all checks BEFORE any stop;
  - K-R2: ExecStart rewrite; phase 7 verifies the RUNNING PROCESS
    (MainPID -> /proc/<PID>/exe via sudo readlink);
  - K3-N1: a FRESH install (managed units absent) completes: absent units
    are "nothing to stop", install+enable+verify succeed; existing units
    (incl. legacy-only layouts) keep the strict K2-R2 stop;
  - K2-N1: the WHOLE switchover sits in one recovery window; recovery
    handles PARTIAL starts (restarts units the failed switch left active so
    they pick up the restored files), restores disabled/inactive pre-states
    (disables what got enabled, stops what got started), falls back to the
    pre-stop binary backup when the rename slot is unusable, verifies the
    running images it restored, and reports INCOMPLETE when anything cannot
    be confirmed;
  - K2-R2: a stop failure of an existing unit aborts into recovery;
  - K2-R3: EVERY privileged discovery probe (ss, cgroup reads, home
    enumeration, key/db existence) refuses the preflight when its answer
    cannot be determined; /proc/<PID>/cmdline output is newline-terminated
    so real_run's anchored parser keeps working; the REAL decision block
    refuses unknown REGISTRY occupants like unknown STUN occupants.

PERMISSION MODELS ARE OFFLINE: the ss stub shows other users' pid= info only
when invoked through the sudo stub, and the test/cat/cp/sed/ls stubs map
/etc, /home, /root and /proc arguments into the probe directory. These model
what root-vs-non-root can observe on Linux; they are NOT a claim of having
verified real-Linux permissions.

OLD-CODE PROOFS (K 复核 K2-R5): the contrast suites EXECUTE the install/
verify/preflight/decision code actually shipped in the git baselines —
f7dece2 (round-3 candidate: K3-N1 fresh-install abort, K2-N1 partial-start /
disabled-inactive / rename-fallback recovery gaps, cmdline parser gap),
f150503 (round-2 candidate) and f9582e5 (original batch-K baseline). Claim
scope, matching actual behavior: a missing git REVISION or a missing SECTION
MARKER in a BASELINE text skips that contrast (old code may legitimately
differ in shape); a missing marker in the CURRENT script is a test FAILURE
(the current generators must keep the documented shape). For genuinely-new
failure modes the honest statement is: the old-code run demonstrates the
UNMITIGATED failure, and the new-code test asserts the mitigation EXISTS.
"""
from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/deploy_linux.sh"
BASH = shutil.which("bash")

# Git baselines executed by the old-code contrast proofs.
PREV_REV = "f7dece2"   # round-3 candidate (K3-N1 / K2-N1 round-4 findings)
PREV3_REV = "f150503"  # round-2 candidate (K2 findings)
BASE_REV = "f9582e5"   # original batch-K baseline (K findings)

SUDO_STUB = r"""#!/usr/bin/env bash
set -u
printf 'sudo %s\n' "$*" >> "$PROBE_DIR/stubs.log"
# Strip only sudo's OWN leading -n: `sudo -n sed -n ...` must keep sed's -n.
args=()
skip=0
first=1
for a in "$@"; do
  if [ "$first" = 1 ]; then
    first=0
    [ "$a" = -n ] && continue
  fi
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$a" in
    -m|-o|-g) skip=1 ;;
    *) args+=("$a") ;;
  esac
done
if [ -n "${SUDO_FAIL_TESTW:-}" ] && [ "${args[0]}" = test ] && [ "${args[1]}" = -w ]; then
  exit 1
fi
# OFFLINE PERMISSION MODEL marker: children invoked through sudo can behave
# as root would (e.g. ss showing other users' pid= info).
export AK_VIA_SUDO=1
exec "${args[@]}"
"""

INSTALL_STUB = r"""#!/usr/bin/env bash
set -u
printf 'install %s\n' "$*" >> "$PROBE_DIR/stubs.log"
paths=()
for a in "$@"; do
  case "$a" in -*) ;; *) paths+=("$a") ;; esac
done
if [ -n "${FAIL_SWITCHOVER_INSTALL:-}" ]; then
  for a in "${paths[@]}"; do
    case "$a" in
      *ak-cli.new)
        echo "stub install: forced switchover failure" >&2
        exit 1
        ;;
    esac
  done
fi
if [ "$1" = "-d" ]; then
  mkdir -p "${paths[@]}"
  exit 0
fi
if [ ${#paths[@]} -ne 2 ]; then
  echo "stub install: unsupported: $*" >&2
  exit 1
fi
cp "${paths[0]}" "${paths[1]}" || exit 1
chmod 0755 "${paths[1]}" || exit 1
exit 0
"""

# STATEFUL systemctl model. State lives in $PROBE_DIR/systemctl-state:
#   <unit>.enabled (1/0), <unit>.active (active/inactive), <unit>.pid,
#   <pid>.exe (what /proc/<pid>/exe resolves to), <unit>.running[.bin] (the
#   executable path / BYTES the unit last started with — read from the
#   CURRENT unit file, so restarts after file restores pick up restored
#   content). 'start' on an active unit is a NO-OP, exactly like systemd.
# Whole-command failures (SYSTEMCTL_FAIL_MATCH [+ _ONCE]) fire BEFORE side
# effects; per-unit partial failures (SYSTEMCTL_FAIL_SPEC entries
# "unit:op[:afterN]") fire mid-command so a later unit can fail after
# earlier units already started, and later calls can fail after the probes.
SYSTEMCTL_STUB = r"""#!/usr/bin/env bash
set -u
printf 'systemctl %s\n' "$*" >> "$PROBE_DIR/stubs.log"
ST="$PROBE_DIR/systemctl-state"
mkdir -p "$ST" 2>/dev/null || true
UNITDIR="$PROBE_DIR/etc/systemd/system"
if [ -n "${SYSTEMCTL_FAIL_MATCH:-}" ]; then
  case "$*" in
    *"$SYSTEMCTL_FAIL_MATCH"*)
      if [ "${SYSTEMCTL_FAIL_ONCE:-0}" = 1 ]; then
        cnt="$PROBE_DIR/systemctl-fail-count"
        n=$(cat "$cnt" 2>/dev/null || echo 0)
        echo $((n + 1)) > "$cnt"
        if [ "$n" -eq 0 ]; then
          echo "systemctl stub: forced failure: $*" >&2
          exit 1
        fi
      else
        echo "systemctl stub: forced failure: $*" >&2
        exit 1
      fi
      ;;
  esac
fi
# Per-unit partial failures: SYSTEMCTL_FAIL_SPEC is a ';'-separated list of
# unit:op[:afterN] entries (no spaces). afterN (default 0) lets the first N
# matching calls succeed (e.g. capture + stop probes) and fails every later
# one (e.g. recovery-phase queries) — multiple entries can combine, e.g.
# "ak-stun-server.service:enable-now;ak-registry-server.service:is-active:2".
fails_spec() { # op unit -> 0/1: an injection entry targets this op+unit now?
  [ -z "${SYSTEMCTL_FAIL_SPEC:-}" ] && return 1
  for spec in ${SYSTEMCTL_FAIL_SPEC//;/ }; do
    fu=${spec%%:*}
    rest=${spec#*:}
    fop=${rest%%:*}
    fafter=0
    case "$rest" in *:*) fafter=${rest#*:} ;; esac
    [ "$fu" != "$2" ] && continue
    [ "$fop" != "$1" ] && continue
    cf="$ST.failcnt.$1.$2"
    n=$(cat "$cf" 2>/dev/null || echo 0)
    echo $((n + 1)) > "$cf"
    [ "$n" -ge "$fafter" ] && return 0
  done
  return 1
}
read_execstart() { # unit -> executable from the CURRENT unit file
  sed -n 's/^ExecStart=\([^ ]*\).*/\1/p' "$UNITDIR/$1" 2>/dev/null | head -1
}
stub_start() { # unit: record pid + the RUNNING image this start produced
  uu="$1"
  pid=$(( $(cat "$ST/nextpid" 2>/dev/null || echo 5000) + 1 ))
  echo "$pid" > "$ST/nextpid"
  echo "$pid" > "$ST/$uu.pid"
  echo active > "$ST/$uu.active"
  exe=$(read_execstart "$uu")
  printf '%s\n' "$exe" > "$ST/$pid.exe"
  printf '%s\n' "$exe" > "$ST/$uu.running"
  cp "$exe" "$ST/$uu.running.bin" 2>/dev/null || : > "$ST/$uu.running.bin"
}
is_unit_active() { [ "$(cat "$ST/$1.active" 2>/dev/null || true)" = active ]; }
u="${!#}"
case "$1" in
  cat)
    if [ -f "$UNITDIR/$u" ]; then exit 0; fi
    # Default: systemd v255's standard missing-unit diagnostic (verified
    # against upstream systemctl-util.c, LoadState=not-found finish path),
    # trailing period included. SYSTEMCTL_CAT_MSG overrides the exact text
    # for fixture variants (classic wording / undeterminable garbage).
    echo "${SYSTEMCTL_CAT_MSG:-No files found for $u.}" >&2
    exit 1 ;;
  is-active)
    if [ -n "${SYSTEMCTL_IS_ACTIVE:-}" ]; then
      echo "$SYSTEMCTL_IS_ACTIVE"
      [ "$SYSTEMCTL_IS_ACTIVE" = active ] && exit 0
      exit 3
    fi
    if fails_spec is-active "$u"; then
      # Query FAILURE: no stdout, non-zero — the round-5 reviewed shape.
      echo "systemctl stub: forced query failure: is-active $u" >&2
      exit 1
    fi
    a=$(cat "$ST/$u.active" 2>/dev/null || true)
    [ -z "$a" ] && a=inactive
    echo "$a"
    [ "$a" = active ] && exit 0
    exit 3 ;;
  is-enabled)
    if [ -n "${SYSTEMCTL_IS_ENABLED:-}" ]; then
      echo "$SYSTEMCTL_IS_ENABLED"
      [ "$SYSTEMCTL_IS_ENABLED" = enabled ] && exit 0
      exit 1
    fi
    if [ -f "$ST/$u.enabled" ] && grep -q 1 "$ST/$u.enabled" 2>/dev/null; then
      echo enabled; exit 0
    fi
    if [ -f "$UNITDIR/$u" ]; then echo disabled; exit 1; fi
    echo "not-found"; exit 1 ;;
  show)
    case " $* " in
      *" MainPID "*)
        if [ -n "${SYSTEMCTL_MAINPID:-}" ]; then echo "${SYSTEMCTL_MAINPID}"; exit 0; fi
        p=$(cat "$ST/$u.pid" 2>/dev/null || echo 0)
        [ -z "$p" ] && p=0
        echo "$p" ;;
      *)
        if [ -n "${SYSTEMCTL_LIVE_PATH:-}" ]; then
          echo "ExecStart={ path=${SYSTEMCTL_LIVE_PATH} ; argv[]=${SYSTEMCTL_LIVE_PATH} registry-server --port 17200 ; ignore_errors=no }"
        else
          exe=$(read_execstart "$u")
          echo "ExecStart={ path=${exe} ; argv[]=${exe} registry-server --port 17200 ; ignore_errors=no }"
        fi ;;
    esac ;;
  start)
    if fails_spec start "$u"; then
      echo "systemctl stub: forced per-unit failure: start $u" >&2
      exit 1
    fi
    # systemd semantics: start on an ACTIVE unit is a no-op — the running
    # process (and its image) is untouched.
    if is_unit_active "$u"; then exit 0; fi
    stub_start "$u" ;;
  restart)
    if fails_spec restart "$u"; then
      echo "systemctl stub: forced per-unit failure: restart $u" >&2
      exit 1
    fi
    stub_start "$u" ;;
  stop)
    if fails_spec stop "$u"; then
      echo "systemctl stub: forced per-unit failure: stop $u" >&2
      exit 1
    fi
    if [ -f "$UNITDIR/$u" ] || [ -f "$ST/$u.active" ]; then
      p=$(cat "$ST/$u.pid" 2>/dev/null || true)
      [ -n "$p" ] && rm -f "$ST/$p.exe"
      rm -f "$ST/$u.pid" "$ST/$u.running" "$ST/$u.running.bin"
      echo inactive > "$ST/$u.active"
      exit 0
    fi
    echo "Failed to stop $u: Unit $u not loaded." >&2
    exit 1 ;;
  enable)
    now=0
    for a in "$@"; do [ "$a" = "--now" ] && now=1; done
    shift
    for uu in "$@"; do
      case "$uu" in --*) continue ;; esac
      echo 1 > "$ST/$uu.enabled"
      if [ "$now" = 1 ]; then
        if fails_spec enable-now "$uu"; then
          echo "systemctl stub: forced per-unit failure starting $uu (enabled, not started)" >&2
          exit 1
        fi
        if ! is_unit_active "$uu"; then stub_start "$uu"; fi
      fi
    done ;;
  disable)
    shift
    for uu in "$@"; do
      case "$uu" in --*) continue ;; esac
      echo 0 > "$ST/$uu.enabled"
    done ;;
  daemon-reload) : ;;
esac
exit 0
"""

TEE_STUB = r"""#!/usr/bin/env bash
set -u
dest="${@: -1}"
printf 'tee %s\n' "$dest" >> "$PROBE_DIR/stubs.log"
case "$dest" in
  /etc/*) out="$PROBE_DIR$dest" ;;
  *) out="$dest" ;;
esac
mkdir -p "$(dirname "$out")"
if [ -n "${FAIL_TEE_UNIT:-}" ] && [ "$dest" = "/etc/systemd/system/$FAIL_TEE_UNIT.service" ]; then
  # Model a PARTIAL write: a corrupted file lands on disk, then the write
  # fails — recovery must overwrite this with the backed-up old unit.
  cat > /dev/null
  printf 'CORRUPTED-UNIT-PARTIAL %s\n' "$FAIL_TEE_UNIT" > "$out"
  echo "tee stub: forced failure writing $dest" >&2
  exit 1
fi
cat > "$out"
exit 0
"""

CHOWN_STUB = r"""#!/usr/bin/env bash
set -u
printf 'chown %s\n' "$*" >> "$PROBE_DIR/stubs.log"
exit 0
"""

USERADD_STUB = r"""#!/usr/bin/env bash
set -u
printf 'useradd %s\n' "$*" >> "$PROBE_DIR/stubs.log"
exit 0
"""

UNAME_STUB = r"""#!/usr/bin/env bash
set -u
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) echo Linux x86_64 ;;
esac
"""

JOURNALCTL_STUB = r"""#!/usr/bin/env bash
set -u
printf 'journalctl %s\n' "$*" >> "$PROBE_DIR/stubs.log"
cat <<'JEOF'
registry-server version: ak-cli 0.1.0 (git deadbee stub)
v2 registry server listening on tls 0.0.0.0:17200
registry id (pin with --registry-fp): aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111
stun-server version: ak-cli 0.1.0 (git deadbee stub)
stun responder listening on 0.0.0.0:17101 udp
JEOF
exit 0
"""

# OFFLINE PERMISSION MODEL (not a real-Linux permission claim): when invoked
# through the sudo stub the socket list carries OTHER USERS' pid= info;
# without sudo the same foreign sockets show no process column.
SS_STUB = r"""#!/usr/bin/env bash
set -u
printf 'ss %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${SS_FAIL:-}" ]; then echo "ss stub: forced failure" >&2; exit 1; fi
if [ "${AK_VIA_SUDO:-}" = 1 ]; then
  echo 'LISTEN 0 0 0.0.0.0:17200 0.0.0.0:* users:(("ak-cli",pid=4242,fd=3))'
  echo 'UNCONN 0 0 0.0.0.0:17101 0.0.0.0:* users:(("ak-cli",pid=4243,fd=3))'
else
  echo 'LISTEN 0 0 0.0.0.0:17200 0.0.0.0:*'
  echo 'UNCONN 0 0 0.0.0.0:17101 0.0.0.0:*'
fi
exit 0
"""

SLEEP_STUB = "#!/usr/bin/env bash\nexit 0\n"

TEST_STUB = r"""#!/usr/bin/env bash
set -u
args=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) args+=("$PROBE_DIR$a") ;;
    *) args+=("$a") ;;
  esac
done
test "${args[@]}"
"""

CP_STUB = r"""#!/usr/bin/env bash
set -u
printf 'cp %s\n' "$*" >> "$PROBE_DIR/stubs.log"
args=()
for a in "$@"; do case "$a" in -*) ;; *) args+=("$a") ;; esac; done
mapped=()
for a in "${args[@]}"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_CP@" -a "${mapped[@]}"
"""

MV_STUB = r"""#!/usr/bin/env bash
set -u
printf 'mv %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${MV_FAIL_MATCH:-}" ]; then
  case "$*" in
    *"$MV_FAIL_MATCH"*)
      echo "mv stub: forced failure: $*" >&2
      exit 1 ;;
  esac
fi
exec "@REAL_MV@" "$@"
"""

CAT_STUB = r"""#!/usr/bin/env bash
set -u
printf 'cat %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${CAT_FAIL_MATCH:-}" ]; then
  case "$*" in
    *"$CAT_FAIL_MATCH"*)
      echo "cat stub: forced failure: $*" >&2
      exit 1 ;;
  esac
fi
mapped=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_CAT@" "${mapped[@]}"
"""

SED_STUB = r"""#!/usr/bin/env bash
set -u
mapped=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_SED@" "${mapped[@]}"
"""

# /proc/<PID>/exe model: PROC_EXE_MAP (newline-separated path=value) is an
# explicit static override; otherwise the pid resolves through the stateful
# systemctl model. A missing entry is an error so an unmapped read can never
# silently pass.
READLINK_STUB = r"""#!/usr/bin/env bash
set -u
target="${@: -1}"
printf 'readlink %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${PROC_EXE_MAP:-}" ]; then
  while IFS='=' read -r k v; do
    if [ "$k" = "$target" ]; then printf '%s\n' "$v"; exit 0; fi
  done <<< "$PROC_EXE_MAP"
fi
case "$target" in
  /proc/[0-9]*/exe)
    pid=${target#/proc/}
    pid=${pid%/exe}
    if [ -f "$PROBE_DIR/systemctl-state/$pid.exe" ]; then
      cat "$PROBE_DIR/systemctl-state/$pid.exe"
      exit 0
    fi
    ;;
esac
echo "readlink stub: no mapping for $target" >&2
exit 1
"""

LS_STUB = r"""#!/usr/bin/env bash
set -u
printf 'ls %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${LS_FAIL_MATCH:-}" ]; then
  case "$*" in
    *"$LS_FAIL_MATCH"*)
      echo "sudo: ls: blocked by stub (forced)" >&2
      exit 1 ;;
  esac
fi
# Real GNU ls permission diagnostic (offline permission model): 'cannot
# access' + 'Permission denied' — the exact shape round 4 misread as absent.
if [ -n "${LS_PERMDENY_PATH:-}" ] && [ "${!#}" = "$LS_PERMDENY_PATH" ]; then
  echo "ls: cannot access '$LS_PERMDENY_PATH': Permission denied" >&2
  exit 2
fi
if [ "${1:-}" = "-1" ] && [ "$2" = "/home" ]; then
  # Model the home listing root would see (foreign users included).
  for e in ${LS_HOME_ENTRIES:-otheruser}; do printf '%s\n' "$e"; done
  exit 0
fi
mapped=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_LS@" "${mapped[@]}"
"""

STUBS = {
    "sudo": SUDO_STUB, "install": INSTALL_STUB, "systemctl": SYSTEMCTL_STUB,
    "tee": TEE_STUB, "chown": CHOWN_STUB, "useradd": USERADD_STUB,
    "uname": UNAME_STUB, "journalctl": JOURNALCTL_STUB, "ss": SS_STUB,
    "sleep": SLEEP_STUB, "test": TEST_STUB, "cp": CP_STUB, "cat": CAT_STUB,
    "sed": SED_STUB, "readlink": READLINK_STUB, "ls": LS_STUB, "mv": MV_STUB,
}
_REAL_TOOLS = {}


def _posix_tool(tool: str) -> str:
    """Resolve a real tool to its MSYS/POSIX path (fast exec, no Windows-path
    conversion on every stub call)."""
    cached = _REAL_TOOLS.get(tool)
    if cached:
        return cached
    result = subprocess.run(
        [BASH, "--noprofile", "--norc", "-c", f"command -v {tool}"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30,
        env={k: v for k, v in os.environ.items() if k != "PATH"} | {"PATH": "/usr/bin:/bin"},
    )
    path = result.stdout.decode("utf-8", errors="replace").strip()
    if not path.startswith("/"):
        path = shutil.which(tool) or ""
    _REAL_TOOLS[tool] = path
    return path


AK_CLI_STUB = '#!/usr/bin/env bash\necho "ak-cli 0.1.0 (git deadbee stub)"\n'
# The pre-upgrade binary: a DIFFERENT version string so restore checks can
# tell old bytes from new bytes on disk (and the stateful stub copies those
# bytes into <unit>.running.bin at every start/restart).
AK_CLI_OLD = '#!/usr/bin/env bash\necho "ak-cli 0.1.0 (git olddead old)"\n'

PREAMBLE = r'''
set -uo pipefail
PROBE_DIR="$(cd "$1" && pwd)"
export PROBE_DIR
PATH="$PROBE_DIR/bin:$PATH"
export PATH
AK_CLI_STUB="$PROBE_DIR/ak-cli.stub"
AK_CLI_OLD="$PROBE_DIR/ak-cli.oldstub"
REMOTE_BASE="$PROBE_DIR/opt-ak"
REMOTE_DATA="$REMOTE_BASE/data"
REMOTE_BIN="$REMOTE_BASE/ak-cli"
SERVICE_USER=akreg
REGISTRY_PORT=17200
STUN_PORT=17101
SHORT_SHA=deadbee
warn() { printf '[deploy-test] WARNING: %s\n' "$*" >&2; }
note() { printf '[deploy-test] %s\n' "$*"; }
fail() { printf '[deploy-test] FAIL: %s\n' "$*" >&2; exit 1; }
echo "PROBE=$PROBE_DIR"
'''

# Driver-side box models. The bash side knows the MSYS-normalized probe
# paths, which python cannot compute before the run.
BOX_MODELS = r'''
ST="$PROBE_DIR/systemctl-state"
mkdir -p "$ST"
write_old_unit() { # unit
  mkdir -p "$PROBE_DIR/etc/systemd/system"
  cat > "$PROBE_DIR/etc/systemd/system/$1.service" <<UUNIT
[Unit]
Description=OLD $1 (model)

[Service]
User=akreg
ExecStart=$REMOTE_BIN registry-server --port $REGISTRY_PORT --id-dir $REMOTE_DATA

[Install]
WantedBy=multi-user.target
UUNIT
}
make_upgrade_model() {
  mkdir -p "$REMOTE_DATA"
  cp "$PROBE_DIR/ak-cli.oldstub" "$REMOTE_BIN"
  chmod +x "$REMOTE_BIN"
  printf 'key-bytes\n' > "$REMOTE_DATA/registry.key"
  printf 'db-bytes\n' > "$REMOTE_DATA/registry.db"
  write_old_unit ak-registry-server
  write_old_unit ak-stun-server
  for u in ak-registry-server ak-stun-server; do
    echo 1 > "$ST/$u.service.enabled"
    echo active > "$ST/$u.service.active"
  done
  echo 4242 > "$ST/ak-registry-server.service.pid"
  echo 4243 > "$ST/ak-stun-server.service.pid"
  printf '%s\n' "$REMOTE_BIN" > "$ST/4242.exe"
  printf '%s\n' "$REMOTE_BIN" > "$ST/4243.exe"
  for u in ak-registry-server ak-stun-server; do
    printf '%s\n' "$REMOTE_BIN" > "$ST/$u.service.running"
    cp "$REMOTE_BIN" "$ST/$u.service.running.bin"
  done
  echo "MODEL=upgrade(enabled+active on the old binary)"
}
make_disabled_inactive_model() {
  make_upgrade_model
  for u in ak-registry-server ak-stun-server; do
    echo 0 > "$ST/$u.service.enabled"
    echo inactive > "$ST/$u.service.active"
    rm -f "$ST/$u.service.pid" "$ST/$u.service.running" "$ST/$u.service.running.bin"
  done
  rm -f "$ST/4242.exe" "$ST/4243.exe"
  echo "MODEL=disabled+inactive(units exist)"
}
make_legacy_only_model() {
  mkdir -p "$PROBE_DIR/etc/systemd/system"
  cat > "$PROBE_DIR/etc/systemd/system/oldsignal.service" <<LUNIT
[Unit]
Description=legacy signal (model)

[Service]
ExecStart=$REMOTE_BIN signal-server --port 7100

[Install]
WantedBy=multi-user.target
LUNIT
  echo 1 > "$ST/oldsignal.service.enabled"
  echo active > "$ST/oldsignal.service.active"
  echo 4243 > "$ST/oldsignal.service.pid"
  printf '%s\n' "$REMOTE_BIN" > "$ST/4243.exe"
  echo "MODEL=legacy-only(oldsignal active, managed units absent)"
}
'''

STAGE_AND_GENERATE = r'''
STAGING_DIR="$PROBE_DIR/staging"
STAGED_BIN="$STAGING_DIR/ak-cli"
mkdir -p "$STAGING_DIR"
cp "$AK_CLI_STUB" "$STAGED_BIN"
chmod +x "$STAGED_BIN"
COMPOSED="$(compose_registry_exec '/usr/local/bin/ak-cli registry-server --port 7200 --id-dir /var/lib/ak-legacy')"
echo "COMPOSED=$COMPOSED"
TS=t12345 BUILD_DIR= PREBUILT_MODE=1 STAGING_DIR="$STAGING_DIR" STAGED_BIN="$STAGED_BIN" \
  REGISTRY_EXEC="$COMPOSED" REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT= \
  DATA_SOURCE_DIR= CLOSE_7100=0 write_phase456 "$PROBE_DIR/p456.sh" || { echo GEN_FAILED; exit 90; }
'''


def git_show(rev: str, path: str) -> str:
    """Return `git show rev:path` text, or "" when the revision is absent."""
    try:
        result = subprocess.run(
            ["git", "-C", str(ROOT), "show", f"{rev}:{path}"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    if result.returncode != 0:
        return ""
    return result.stdout.decode("utf-8", errors="replace")


class ProbeHarness(unittest.TestCase):
    """Shared probe-directory plumbing for the current and baseline suites."""

    def setUp(self):
        self.assertTrue(SCRIPT.is_file(), "scripts/deploy_linux.sh must exist")
        self.assertIsNotNone(BASH, "Bash is required for these regressions")
        self.text = SCRIPT.read_text(encoding="utf-8")
        tmp = tempfile.TemporaryDirectory(prefix="ak-deploy-install-test-")
        self.addCleanup(tmp.cleanup)
        self.probe = Path(tmp.name).resolve()
        (self.probe / "bin").mkdir()
        for name, body in STUBS.items():
            stub = self.probe / "bin" / name
            stub.write_text(body, encoding="utf-8", newline="\n")
            stub.chmod(0o755)
        for tool in ("cp", "cat", "sed", "ls", "mv"):
            real = _posix_tool(tool)
            self.assertTrue(real, f"real {tool} is required for the stubs")
            stub = self.probe / "bin" / tool
            stub.write_text(
                stub.read_text(encoding="utf-8").replace(f"@REAL_{tool.upper()}@", real),
                encoding="utf-8", newline="\n")
        for name, body in (("ak-cli.stub", AK_CLI_STUB), ("ak-cli.oldstub", AK_CLI_OLD)):
            f = self.probe / name
            f.write_text(body, encoding="utf-8", newline="\n")
            f.chmod(0o755)
        self.driver_env = dict(os.environ)
        self.driver_env.pop("BASH_ENV", None)

    # -- plumbing -----------------------------------------------------------
    def section(self, text: str, start: str, end: str,
                origin: str = "the current script", skip_if_missing: bool = False) -> str:
        """Extract [start, end) from `text`. Claim scope (K 复核 K2-R5): a
        missing marker in a BASELINE text skips the contrast (old code may
        differ in shape); a missing marker in the CURRENT script FAILS the
        test (the generators must keep the documented shape)."""
        problem = None
        if start not in text:
            problem = f"start marker {start!r} not found in {origin}"
        elif end not in text[text.index(start):]:
            problem = f"end marker {end!r} not found after the start marker in {origin}"
        if problem:
            if skip_if_missing:
                self.skipTest(f"cannot extract the section: {problem}")
            raise AssertionError(problem)
        a = text.index(start)
        b = text.index(end, a)
        return text[a:b]

    def definitions(self) -> str:
        return (
            self.section(self.text, "compose_registry_exec() {", "# systemd unit templates")
            + self.section(self.text, "write_phase1() {", "# Phase 3: build on the server")
            + self.section(self.text, "write_phase456() {", "# Phase 7: verification")
            + self.section(self.text, "write_phase7() {", "\nreal_run() {")
        )

    def decision_block(self, text: str, origin: str) -> str:
        """real_run's phase1 decision block (ERROR checks through the
        CHECK_ONLY exit) — executed for the full phase1-output-to-decision
        chain (K 复核 K2-R3)."""
        return self.section(
            text,
            '  if grep -q \'^ERROR:\' "$WORK/phase1.log"; then',
            '  # ---- phase 2: upload',
            origin=origin, skip_if_missing=True)

    def run_driver(self, definitions: str, body: str, env: dict | None = None,
                   name: str = "driver.sh", timeout: float = 60):
        driver = self.probe / name
        # The extracted definitions never end with a newline — always join
        # with an explicit one or the first body line would be glued onto a
        # comment.
        script = PREAMBLE + definitions + "\n" + body
        driver.write_text(script, encoding="utf-8", newline="\n")
        run_env = dict(self.driver_env)
        run_env.update(env or {})
        result = subprocess.run(
            [BASH, "--noprofile", "--norc", driver.as_posix(), self.probe.as_posix()],
            cwd=self.probe, env=run_env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout,
        )
        return result.returncode, result.stdout.decode("utf-8", errors="replace")

    def generate_and_run_p456(self, body_extra: str = "", run_env: dict | None = None):
        return self.run_driver(self.definitions() + BOX_MODELS + STAGE_AND_GENERATE,
                               body_extra, env=run_env, name="driver.sh")

    def run_decision(self, block: str, phase1_log: str, name: str):
        """Execute an extracted real_run decision block against a phase1
        report (PREAMBLE provides note/warn/fail; CHECK_ONLY=0 lets the
        block fall through on acceptance)."""
        body = (
            'WORK="$PROBE_DIR"\n'
            'KEEP_LEGACY_SIGNAL=0\n'
            'CHECK_ONLY=0\n'
            'deploy_decision() {\n'
            + block
            + '\n}\n'
            'echo "DECISION_BEGIN"\n'
            'deploy_decision\n'
            'echo "DECISION_DONE"\n'
        )
        (self.probe / "phase1.log").write_text(phase1_log, encoding="utf-8", newline="\n")
        return self.run_driver("", body, name=name)

    def stubs_log(self) -> str:
        path = self.probe / "stubs.log"
        return path.read_text(encoding="utf-8") if path.exists() else ""

    def bash_probe(self, out: str) -> str:
        """The probe path as Bash normalizes it (e.g. /c/Users/... on MSYS)."""
        match = re.search(r"^PROBE=(\S+)$", out, re.MULTILINE)
        self.assertIsNotNone(match, f"driver did not report PROBE: {out}")
        return match.group(1)

    def log_line(self, log: str, needle: str) -> int:
        """0-based index of the first stubs.log line containing needle."""
        for i, line in enumerate(log.splitlines()):
            if needle in line:
                return i
        self.fail(f"stubs.log has no line containing {needle!r}:\n{log}")

    def switchover_install_calls(self, log: str):
        calls = []
        for line in log.splitlines():
            if line.startswith("install ") and line.endswith("ak-cli.new"):
                parts = line.split()
                calls.append((parts[1], parts[2]))
        return calls

    # -- stateful-systemctl assertions --------------------------------------
    def unit_state(self, unit: str, key: str) -> str:
        path = self.probe / "systemctl-state" / f"{unit}.{key}"
        return path.read_text(encoding="utf-8").strip() if path.exists() else ""

    def unit_running_bin(self, unit: str) -> str:
        path = self.probe / "systemctl-state" / f"{unit}.running.bin"
        return path.read_text(encoding="utf-8") if path.exists() else ""

    def make_private_discovery_model(self):
        """K2-R3 OFFLINE permission model: a registry port held by ANOTHER
        USER's process (pid only visible to root) plus a PRIVATE identity
        directory under a foreign home. /proc/<pid>/cmdline deliberately has
        NO trailing newline, exactly like Linux."""
        proc = self.probe / "proc"
        (proc / "4242").mkdir(parents=True, exist_ok=True)
        (proc / "4243").mkdir(parents=True, exist_ok=True)
        (proc / "4242" / "cgroup").write_text("0::/system.slice/oldregistry.service\n",
                                              encoding="utf-8", newline="\n")
        (proc / "4243" / "cgroup").write_text("0::/system.slice/oldsignal.service\n",
                                              encoding="utf-8", newline="\n")
        (proc / "4242" / "cmdline").write_bytes(b"/opt/ak-sending/ak-cli\0registry-server\0")
        home = self.probe / "home" / "otheruser" / ".ak-sending"
        home.mkdir(parents=True, exist_ok=True)
        (home / "registry.key").write_text("private-key-bytes", encoding="utf-8")
        (home / "registry.db").write_text("private-db-bytes", encoding="utf-8")
        etc = self.probe / "etc" / "systemd" / "system"
        etc.mkdir(parents=True, exist_ok=True)
        (etc / "oldregistry.service").write_text(
            "[Unit]\nDescription=foreign-user registry\n\n[Service]\n"
            "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /home/otheruser/.ak-sending\n\n[Install]\nWantedBy=multi-user.target\n",
            encoding="utf-8", newline="\n")

    def assert_recovery_restored_old_state(self, out: str):
        """Shared K2-N1 assertions: old binary + old unit BYTES back on disk,
        the previously enabled/active units re-enabled and (re)started on the
        OLD IMAGE (stateful stub), and the outcome REPORTED."""
        log = self.stubs_log()
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertIn(f"systemctl enable {unit}", log)
            started = (f"systemctl start {unit}" in log
                       or f"systemctl restart {unit}" in log)
            self.assertTrue(started, f"{unit} was not (re)started by recovery:\n{log}")
        self.assertIn("RECOVERY OK", out)
        self.assertIn("ERROR: service switchover failed", out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "previous binary must be restored")
        backup = self.probe / "opt-ak" / "backup-t12345"
        etc = self.probe / "etc" / "systemd" / "system"
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            live = (etc / unit).read_text(encoding="utf-8")
            self.assertEqual(live, (backup / unit).read_text(encoding="utf-8"),
                             f"previous {unit} must be restored from the backup")
            self.assertNotIn("--port 7200", live, "the NEW unit text must be gone")
        self.assertFalse((self.probe / "opt-ak" / "ak-cli.new").exists(),
                         "the ak-cli.new intermediate must be cleaned up")
        self.assertFalse((self.probe / "opt-ak" / "ak-cli.old-t12345").exists(),
                         "the moved-aside old binary must be back in place")
        # The RUNNING images end on the OLD binary (K2-N1 round 4: disk
        # restoration alone is not recovery).
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD,
                             f"{unit} must RUN the restored old binary")
            self.assertEqual(self.unit_state(unit, "active"), "active")


class DeployInstallTests(ProbeHarness):
    """Current-tree behavior (K 复核 K-R1/K-R2/K2-N1/K2-R2/K2-R3/K3-N1)."""

    # -- K-R2: ExecStart composition ----------------------------------------
    def test_compose_registry_exec_rewrites_executable_keeps_arguments(self):
        rc, out = self.run_driver(self.definitions(), r'''
c1="$(compose_registry_exec '/usr/local/bin/ak-cli registry-server --port 7200 --id-dir /var/lib/ak-legacy')"
echo "C1=[$c1]"
c2="$(compose_registry_exec '/usr/local/bin/ak-cli registry-server --port 7200')"
echo "C2=[$c2]"
c3="$(compose_registry_exec '')"
echo "C3=[$c3]"
c4="$(compose_registry_exec "$REMOTE_BIN registry-server --port 7300 --id-dir /elsewhere")"
echo "C4=[$c4]"
''')
        self.assertEqual(rc, 0, out)
        bin_path = f"{self.bash_probe(out)}/opt-ak/ak-cli"
        self.assertIn(f"C1=[{bin_path} registry-server --port 7200 --id-dir /var/lib/ak-legacy]", out)
        self.assertIn(f"C2=[{bin_path} registry-server --port 7200 --id-dir ", out)
        self.assertIn(f"C3=[{bin_path} registry-server --port 17200 --id-dir ", out)
        self.assertIn(f"C4=[{bin_path} registry-server --port 7300 --id-dir /elsewhere]", out)

    # -- K3-N1: FRESH install completes end to end ---------------------------
    def test_k3n1_fresh_install_absent_units_completes_and_verifies(self):
        # Absent managed units must be "nothing to stop" — round 3 stopped
        # unconditionally and the first 'Unit ... not loaded' error aborted
        # the install before anything was created.
        body = (
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "INSTALL_RC=$?"\n'
            'SHORT_SHA=deadbee write_phase7 "$PROBE_DIR/p7.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p7.sh" > "$PROBE_DIR/p7.out" 2>&1; echo "VERIFY_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        self.assertIn("VERIFY_RC=0", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        # Existence was CHECKED per unit; no stop of an absent unit; the
        # install went through: units written, reload, enable --now.
        self.assertIn("systemctl cat ak-registry-server.service", log)
        self.assertIn("systemctl cat ak-stun-server.service", log)
        self.assertEqual(p456_out.count("not present; nothing to stop (K3-N1)"), 2, p456_out)
        self.assertNotIn("systemctl stop ak-registry-server.service", log)
        self.assertNotIn("systemctl stop ak-stun-server.service", log)
        self.assertIn("systemctl daemon-reload", log)
        self.assertIn("systemctl enable --now ak-registry-server.service"
                      " ak-stun-server.service", log)
        self.assertNotIn("RECOVERING", p456_out)
        # The new binary is installed and BOTH units RUN it (stateful stub).
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB)
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active")
            self.assertEqual(self.unit_state(unit, "enabled"), "1")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_STUB,
                             f"{unit} must run the freshly installed binary")
        self.assertFalse((self.probe / "staging").exists())
        probe = self.bash_probe(out)
        unit = (self.probe / "etc" / "systemd" / "system" / "ak-registry-server.service")
        self.assertIn(f"ExecStart={probe}/opt-ak/ak-cli registry-server"
                      " --port 7200 --id-dir /var/lib/ak-legacy",
                      unit.read_text(encoding="utf-8"))

    def test_k3n1_legacy_only_migration_stops_only_what_exists(self):
        # Only a legacy signal unit exists: it is stopped STRICTLY (K2-R2),
        # the absent managed units are skipped (K3-N1), the install succeeds.
        body = (
            'make_legacy_only_model\n'
            'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            'SHORT_SHA=deadbee '
            'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
            'REG_RW="$REMOTE_DATA" LEGACY_UNIT=oldsignal OLD_REGISTRY_UNIT= '
            'DATA_SOURCE_DIR= CLOSE_7100=0 '
            'write_phase456 "$PROBE_DIR/p456l.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p456l.sh" > "$PROBE_DIR/p456l.out" 2>&1; echo "INSTALL_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        log = self.stubs_log()
        self.assertIn("systemctl stop oldsignal.service", log,
                      "the EXISTING legacy unit must still be stopped strictly (K2-R2)")
        self.assertNotIn("systemctl stop ak-registry-server.service", log)
        self.assertNotIn("systemctl stop ak-stun-server.service", log)
        self.assertEqual(self.unit_running_bin("ak-stun-server.service"), AK_CLI_STUB)

    # -- K-R1: prebuilt staging install, success path ------------------------
    def test_prebuilt_install_uses_distinct_staging_source_and_succeeds(self):
        rc, out = self.generate_and_run_p456(
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        probe = self.bash_probe(out)
        log = self.stubs_log()
        swaps = self.switchover_install_calls(log)
        self.assertEqual(len(swaps), 1, log)
        src, dst = swaps[0]
        self.assertEqual(src, f"{probe}/staging/ak-cli")
        self.assertNotEqual(src, dst)
        self.assertEqual(dst, f"{probe}/opt-ak/ak-cli.new")
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        # K3-N1: on a fresh box the switchover CAPTURES the (absent) state,
        # skips the stops and still installs + enables.
        self.assertIn("captured unit state", p456_out)
        self.assertIn("not present; nothing to stop (K3-N1)", p456_out)
        self.assertIn("systemctl daemon-reload", log)
        self.assertIn("systemctl enable --now ak-registry-server.service"
                      " ak-stun-server.service", log)
        self.assertNotIn("systemctl start", log)
        self.assertFalse((self.probe / "staging").exists())
        unit = (self.probe / "etc" / "systemd" / "system" / "ak-registry-server.service")
        self.assertTrue(unit.is_file(), "unit file was not captured by the tee stub")
        expected = (f"ExecStart={probe}/opt-ak/ak-cli registry-server"
                    " --port 7200 --id-dir /var/lib/ak-legacy")
        self.assertIn(expected, unit.read_text(encoding="utf-8"))
        self.assertNotIn("/usr/local/bin/ak-cli", unit.read_text(encoding="utf-8"))

    # -- K-R1: mid-switchover failure on a FRESH box rolls back cleanly -------
    def test_prebuilt_mid_switchover_failure_recovers_services(self):
        rc, out = self.generate_and_run_p456(
            'export FAIL_SWITCHOVER_INSTALL=1\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("RECOVERING previous service state", p456_out)
        self.assertIn("forced switchover failure", p456_out)
        log = self.stubs_log()
        self.assertIn("install -m 0755", log)
        # Fresh box: nothing was active before, so recovery must NOT start
        # anything — but it must report its (empty) outcome and exit 5.
        self.assertNotIn("systemctl start", log)
        self.assertNotIn("systemctl restart", log)
        self.assertIn("RECOVERY OK", p456_out)
        swaps = self.switchover_install_calls(log)
        self.assertEqual(len(swaps), 1, log)
        self.assertNotEqual(swaps[0][0], swaps[0][1])

    # -- K-R1: same-file guard fires before anything is stopped --------------
    def test_same_file_source_and_destination_refused_before_stops(self):
        body = (
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BASE/ak-cli.new"\n'
            'chmod +x "$REMOTE_BASE/ak-cli.new"\n'
            'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            'STAGING_DIR="$REMOTE_BASE" STAGED_BIN="$REMOTE_BASE/ak-cli.new" '
            'SHORT_SHA=deadbee '
            'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
            'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            'DATA_SOURCE_DIR= CLOSE_7100=0 '
            'write_phase456 "$PROBE_DIR/p456s.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p456s.sh" > "$PROBE_DIR/p456s.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456s.out").read_text(encoding="utf-8")
        self.assertIn("install source equals the install destination", p456_out)
        self.assertNotIn("systemctl", self.stubs_log())

    # -- K-R1: staged version gate fires before anything is stopped ----------
    def test_staged_version_mismatch_refused_before_stops(self):
        rc, out = self.generate_and_run_p456(
            'printf \'#!/usr/bin/env bash\\n\' > "$STAGED_BIN"\n'
            'echo \'echo "ak-cli 0.1.0 (git wrongver)"\' >> "$STAGED_BIN"\n'
            'chmod +x "$STAGED_BIN"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("refusing BEFORE service switchover", p456_out)
        self.assertNotIn("systemctl", self.stubs_log())

    # -- K2-N1: failure at the FIRST UNIT WRITE restores old state -----------
    def test_k2n1_first_unit_write_failure_restores_old_configuration(self):
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export FAIL_TEE_UNIT=ak-registry-server\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure writing /etc/systemd/system/ak-registry-server.service",
                      p456_out)
        log = self.stubs_log()
        self.assertIn("sudo -n test -f /etc/systemd/system/ak-registry-server.service", log)
        stop = self.log_line(log, "systemctl stop ak-registry-server.service")
        fail = self.log_line(log, "tee /etc/systemd/system/ak-registry-server.service")
        restore = self.log_line(
            log, "ak-registry-server.service /etc/systemd/system/ak-registry-server.service")
        reload_line = self.log_line(log, "systemctl daemon-reload")
        enable = self.log_line(log, "systemctl enable ak-registry-server.service")
        started = self.log_line(log, "systemctl start ak-registry-server.service")
        for earlier, later, what in (
                (stop, fail, "stop before the failing unit write"),
                (fail, restore, "restore after the failing unit write"),
                (restore, reload_line, "daemon-reload after unit restore"),
                (reload_line, enable, "re-enable after reload"),
                (enable, started, "registry restart after re-enable")):
            self.assertLess(earlier, later, f"ordering broken: {what}")
        self.assert_recovery_restored_old_state(p456_out)

    # -- K2-N1: daemon-reload failure restores old state ----------------------
    def test_k2n1_daemon_reload_failure_restores_old_configuration(self):
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export SYSTEMCTL_FAIL_MATCH="daemon-reload" SYSTEMCTL_FAIL_ONCE=1\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure: daemon-reload", p456_out)
        log = self.stubs_log()
        first = self.log_line(log, "systemctl daemon-reload")
        started = self.log_line(log, "systemctl start ak-registry-server.service")
        self.assertLess(first, started)
        self.assertEqual(len([l for l in log.splitlines()
                              if l == "systemctl daemon-reload"]), 2, log)
        self.assert_recovery_restored_old_state(p456_out)

    # -- K2-N1: enable/start failure restores old state -----------------------
    def test_k2n1_enable_start_failure_restores_old_configuration(self):
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export SYSTEMCTL_FAIL_MATCH="enable --now ak-registry-server.service"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure: enable --now ak-registry-server.service", p456_out)
        log = self.stubs_log()
        fail = self.log_line(log, "systemctl enable --now ak-registry-server.service")
        started = self.log_line(log, "systemctl start ak-registry-server.service")
        self.assertLess(fail, started)
        self.assert_recovery_restored_old_state(p456_out)

    # -- K2-N1(a): PARTIAL start — the failed switch left registry ACTIVE -----
    def test_k2n1_partial_enable_start_restarts_onto_restored_files(self):
        # The reviewer's exact sequence: enable --now starts the NEW registry
        # fine, then STUN fails. Recovery restores old files and must
        # RESTART the still-active registry (start would no-op) so the
        # RUNNING image ends on the OLD binary.
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export SYSTEMCTL_FAIL_SPEC=ak-stun-server.service:enable-now\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        enable_line = self.log_line(log, "systemctl enable --now ak-registry-server.service")
        restart = self.log_line(log, "systemctl restart ak-registry-server.service")
        self.assertLess(enable_line, restart,
                        "recovery must RESTART the unit the switch left active")
        self.assert_recovery_restored_old_state(p456_out)

    # -- K2-N1(b): disabled/inactive pre-states are restored ------------------
    def test_k2n1_disabled_inactive_pre_state_restored(self):
        # Both units existed but were DISABLED+INACTIVE. The switch enables
        # and starts registry, then STUN fails: recovery must DISABLE what
        # got enabled and STOP what got started.
        rc, out = self.generate_and_run_p456(
            'make_disabled_inactive_model\n'
            'export SYSTEMCTL_FAIL_SPEC=ak-stun-server.service:enable-now\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        # The switch HAD started registry (partial effect) before failing.
        self.assertIn("systemctl enable --now ak-registry-server.service", log)
        # ... and recovery undid it: disable + stop for BOTH units.
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "enabled"), "0",
                             f"{unit} must be disabled again (it was disabled before)")
            self.assertEqual(self.unit_state(unit, "active"), "inactive",
                             f"{unit} must be stopped again (it was inactive before)")
            self.assertIn(f"systemctl disable {unit}", log)
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertIn("RECOVERY OK", p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)

    # -- K2-N1(c): rename failure falls back to the pre-stop backup ----------
    def test_k2n1_binary_rename_failure_falls_back_to_pre_stop_backup(self):
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export MV_FAIL_MATCH="ak-cli.old-t12345" FAIL_TEE_UNIT=ak-registry-server\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        # The move-aside failed (script says so) and recovery then used the
        # pre-stop backup — both loudly reported.
        self.assertIn("could not move the previous binary aside", p456_out)
        self.assertIn("previous binary from the pre-stop backup", p456_out)
        self.assert_recovery_restored_old_state(p456_out)

    # -- K2-N1: a FAILED recovery is loudly reported, never silent -----------
    def test_k2n1_failed_recovery_is_loudly_reported(self):
        # Two independent injections: the FIRST unit write fails (entering
        # recovery), and every recovery START also fails — recovery must then
        # report INCOMPLETE loudly, not exit quietly.
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export FAIL_TEE_UNIT=ak-registry-server SYSTEMCTL_FAIL_MATCH="start ak-"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("RECOVERY INCOMPLETE", p456_out)
        self.assertIn("manual restore required", p456_out)
        self.assertIn("ERROR: service switchover failed", p456_out)

    # -- K2-R2: a stop failure aborts into recovery, never silently continues
    def test_k2r2_stop_failure_aborts_into_recovery_without_touching_disk(self):
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'export SYSTEMCTL_FAIL_MATCH="stop ak-registry-server.service"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        # The very first stop failed: NOTHING may have been written or moved
        # (the old process model keeps running the old binary), and recovery
        # must re-assert the previous state — the switch aborted before the
        # SECOND stop, so BOTH units are still active and recovery must
        # RESTART them onto the restored files (start would no-op).
        self.assertNotIn("tee /etc/", log)
        self.assertIn("systemctl restart ak-registry-server.service", log)
        self.assertIn("systemctl restart ak-stun-server.service", log)
        self.assertIn("RECOVERY OK", p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD)
        etc = self.probe / "etc" / "systemd" / "system"
        backup = self.probe / "opt-ak" / "backup-t12345"
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual((etc / unit).read_text(encoding="utf-8"),
                             (backup / unit).read_text(encoding="utf-8"))

    # -- K3-N1 (round 5): systemd v255's standard missing-unit diagnostic ----
    def test_k3n1_v255_missing_unit_message_completes_fresh_install(self):
        # The reviewer verified 'No files found for <name>.' (trailing
        # period) against upstream systemd v255 systemctl-util.c; round 4's
        # pattern set did not include it, so the real generated install
        # refused to switch on a FRESH box. With the exact v255 fixture
        # string the install must complete.
        rc, out = self.generate_and_run_p456(
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "INSTALL_RC=$?"\n',
            run_env={"SYSTEMCTL_CAT_MSG": "No files found for ak-registry-server.service."})
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertEqual(p456_out.count("not present; nothing to stop (K3-N1)"), 2, p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "the fresh install must complete")

    def test_k3n1_undeterminable_cat_message_refuses_before_switchover(self):
        # A cat diagnostic that is NOT a known missing-unit message (e.g. a
        # D-Bus error) must stay UNKNOWN: refuse before anything is touched.
        rc, out = self.generate_and_run_p456(
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "INSTALL_RC=$?"\n',
            run_env={"SYSTEMCTL_CAT_MSG": "systemctl: D-Bus connection error (stub)"})
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("systemctl cat unusable", p456_out)
        self.assertNotIn("not present; nothing to stop", p456_out)
        self.assertFalse((self.probe / "opt-ak" / "ak-cli").exists(),
                         "nothing may be installed when existence is undeterminable")
        self.assertNotIn("systemctl stop", self.stubs_log())

    # -- K2-N1 (round 5): a FAILED recovery query is never "inactive" ---------
    def test_k2n1_recovery_query_failure_reports_incomplete_never_new_ok(self):
        # The reviewer's exact scenario: both units disabled/inactive;
        # capture and stop probes succeed; enable --now starts the NEW
        # registry and STUN fails; ONLY the registry's recovery-phase
        # is-active query fails (no stdout, non-zero — afterN=2 lets the
        # capture + stop probes pass). Round 4 read the failed query as
        # "inactive", skipped the stop and printed RECOVERY OK while the NEW
        # process kept running. Now: conservative stop + CONFIRM, and an
        # unconfirmable state reports RECOVERY INCOMPLETE — never NEW+OK.
        rc, out = self.generate_and_run_p456(
            'make_disabled_inactive_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
            run_env={"SYSTEMCTL_FAIL_SPEC":
                     "ak-stun-server.service:enable-now;"
                     "ak-registry-server.service:is-active:2"})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("RECOVERY INCOMPLETE", p456_out)
        self.assertNotIn("RECOVERY OK", p456_out,
                         "a failed state query must never be reported as recovery success")
        # The switch HAD started the registry on the NEW binary; the failed
        # query must not leave it running: conservative stop + confirm.
        self.assertEqual(self.unit_state("ak-registry-server.service", "active"), "inactive",
                         "the conservatively-stopped registry must end inactive")
        self.assertNotEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_STUB,
                            "the NEW image must not be left running")
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)
        self.assertEqual(self.unit_state("ak-registry-server.service", "enabled"), "0")

    def test_k2n1_capture_rejects_unsupported_state_values(self):
        # Round 4 only refused EMPTY capture outputs; a non-empty 'unknown'
        # was recorded and the switch proceeded with an unrecoverable state.
        for override, expected in (
                ("SYSTEMCTL_IS_ENABLED", "enable-state"),
                ("SYSTEMCTL_IS_ACTIVE", "run-state")):
            with self.subTest(override=override):
                rc, out = self.generate_and_run_p456(
                    'make_upgrade_model\n'
                    'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
                    run_env={override: "unknown"})
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=5", out)
                p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
                self.assertIn(f"unsupported/undeterminable {expected}", p456_out)
                self.assertNotIn("systemctl stop", self.stubs_log(),
                                 "the refusal must happen BEFORE any stop")

    # -- K2-R3 (round 5a): Permission denied is UNDETERMINABLE, never absent --
    def test_k2r3_permission_denied_dir_probe_refuses(self):
        # Real GNU ls diagnostic shape: 'cannot access ...: Permission
        # denied' for the EXISTING private dir — round 4 matched 'cannot
        # access' as absent and the deploy continued with none-found.
        self.make_private_discovery_model()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'LS_PERMDENY_PATH="/home/otheruser/.ak-sending" bash "$PROBE_DIR/p1.sh" '
            '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot determine existence of /home/otheruser/.ak-sending", p1_out)
        self.assertIn("Permission denied", p1_out)
        self.assertNotIn("registry-data:", p1_out,
                         "an existing identity must never read as none-found")

    def test_k2r3_permission_denied_key_probe_refuses(self):
        self.make_private_discovery_model()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'LS_PERMDENY_PATH="/home/otheruser/.ak-sending/registry.key" '
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: cannot probe /home/otheruser/.ak-sending/registry.key", p1_out)
        self.assertIn("Permission denied", p1_out)
        self.assertNotIn("registry-data:", p1_out)

    # -- K2-R3 (round 5b): identified occupant must reach the migration set ---
    def test_k2r3_legacy_unit_in_vendor_dir_flows_into_migration_decision(self):
        # The old unit lives under /usr/lib/systemd/system (vendor dir),
        # not /etc/systemd/system: discovery must still find it, and the
        # REAL decision block must bind it into the migration set.
        self.make_private_discovery_model()
        (self.probe / "etc" / "systemd" / "system" / "oldregistry.service").unlink()
        vendor = self.probe / "usr" / "lib" / "systemd" / "system"
        vendor.mkdir(parents=True, exist_ok=True)
        (vendor / "oldregistry.service").write_text(
            "[Unit]\nDescription=vendor-dir legacy registry\n\n[Service]\n"
            "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /home/otheruser/.ak-sending\n\n[Install]\nWantedBy=multi-user.target\n",
            encoding="utf-8", newline="\n")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: in use (pid 4242, unit oldregistry.service)", p1_out)
        self.assertRegex(p1_out, r"(?m)^  old-registry-unit: oldregistry\.service$")
        self.assertIn("  ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /home/otheruser/.ak-sending", p1_out)
        # Full chain: the real decision block keeps the identified occupant.
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, p1_out, "driver-decision-vendor.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("registry-port occupant unit: oldregistry", out)
        self.assertIn("old registry unit 'oldregistry.service' will be stopped", out)

    def test_k2r3_legacy_unit_config_probe_failure_refuses(self):
        # The unit file exists but the privileged probe on it is denied:
        # undeterminable -> refusal; the occupant stays identified in the
        # output (never silently dropped from the migration set).
        self.make_private_discovery_model()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'LS_PERMDENY_PATH="/etc/systemd/system/oldregistry.service" '
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: in use (pid 4242, unit oldregistry.service)", p1_out)
        self.assertIn("ERROR: cannot determine whether /etc/systemd/system/oldregistry.service "
                      "exists", p1_out)
        self.assertNotIn("old-registry-unit:", p1_out)

    def test_k2r3_identified_occupant_without_unit_file_refuses(self):
        # cgroup identifies oldregistry.service but NO standard dir holds its
        # file: the migration target cannot be bound -> refusal (fail-closed),
        # never a silent empty migration set.
        self.make_private_discovery_model()
        (self.probe / "etc" / "systemd" / "system" / "oldregistry.service").unlink()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: in use (pid 4242, unit oldregistry.service)", p1_out)
        self.assertIn("was identified but its unit file cannot be located", p1_out)

    # -- K2-R2: phase 7 verifies the RUNNING process --------------------------
    def _run_p7(self, live_path: str, exe_map_expr: str, short_sha: str = "deadbee",
                mainpid: str = "4242"):
        """exe_map_expr: a quoted bash expression for the /proc/<pid>/exe
        readlink result, e.g. '"$REMOTE_BIN"' or '"$REMOTE_BIN (deleted)"'."""
        body = (
            f'SHORT_SHA={short_sha} write_phase7 "$PROBE_DIR/p7.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BIN"\n'
            'chmod +x "$REMOTE_BIN"\n'
            f'MAP_EXE={exe_map_expr}\n'
            f'SYSTEMCTL_IS_ACTIVE=active SYSTEMCTL_LIVE_PATH={live_path} '
            f'SYSTEMCTL_MAINPID={mainpid} '
            f'PROC_EXE_MAP="/proc/{mainpid}/exe=$MAP_EXE" '
            'bash "$PROBE_DIR/p7.sh" > "$PROBE_DIR/p7.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        return self.generate_and_run_p456(body)

    def test_phase7_accepts_running_units_on_the_deployed_binary(self):
        rc, out = self._run_p7('"$REMOTE_BIN"', '"$REMOTE_BIN"')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        bin_path = f"{self.bash_probe(out)}/opt-ak/ak-cli"
        self.assertIn(f"unit ak-registry-server runs {bin_path}", p7_out)
        self.assertIn(f"unit ak-stun-server runs {bin_path}", p7_out)
        self.assertIn("MainPID 4242", p7_out)

    def test_phase7_rejects_old_process_still_running_after_disk_swap(self):
        # The reviewer's K2-R2 reproduction precondition, verbatim: OLD
        # PROCESS STILL RUNNING (unit active, MainPID resolves) while the
        # DISK PATH was already replaced with the new version.
        rc, out = self._run_p7('"$REMOTE_BIN"', '"$REMOTE_BASE/ak-cli.old-t12345"')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("running-process mismatch", p7_out)
        self.assertIn("ak-cli.old-t12345", p7_out)

    def test_phase7_rejects_process_whose_exe_was_deleted(self):
        rc, out = self._run_p7('"$REMOTE_BIN"', '"$REMOTE_BIN (deleted)"')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("DELETED binary", p7_out)

    def test_phase7_detects_stale_executable_preserved_from_old_unit(self):
        rc, out = self._run_p7("/usr/local/bin/ak-cli", '"$REMOTE_BIN"')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("stale configuration", p7_out)
        self.assertIn("/usr/local/bin/ak-cli", p7_out)

    def test_phase7_detects_live_binary_reporting_wrong_revision(self):
        rc, out = self._run_p7('"$REMOTE_BIN"', '"$REMOTE_BIN"', short_sha="beefdead")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("want 'git beefdead'", p7_out)

    # -- K2-R2/K2-N1: preflight install-path writability via sudo -------------
    def test_phase1_checks_install_path_writability_via_sudo(self):
        # The discovery model provides the /proc fixtures the (now always
        # probing) preflight resolves; only the install-path probe is made
        # to fail in the second run.
        self.make_private_discovery_model()
        body = (
            'mkdir -p "$REMOTE_BASE"\n'
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
            'SUDO_FAIL_TESTW=1 bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1w.out" 2>&1; echo "RUNW_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        self.assertIn("RUNW_RC=0", out)
        base = f"{self.bash_probe(out)}/opt-ak"
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn(f"install-path: {base} writable via sudo", p1_out)
        self.assertNotIn("ERROR: sudo -n cannot write", p1_out)
        p1w_out = (self.probe / "p1w.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: sudo -n cannot write", p1w_out)

    # -- K2-R3: sudo-based discovery of foreign processes + private identity --
    def test_k2r3_phase1_discovers_foreign_process_and_private_identity_via_sudo(self):
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: in use (pid 4242, unit oldregistry.service)", p1_out)
        self.assertIn("  old-registry-unit: oldregistry.service", p1_out)
        self.assertIn(
            "  ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /home/otheruser/.ak-sending", p1_out)
        self.assertIn("port 17101/udp: in use (pid 4243, unit oldsignal.service)", p1_out)
        self.assertIn("registry-data: /home/otheruser/.ak-sending (key:yes db:yes)", p1_out)
        # K2-R3(b): the cmdline line is TERMINATED — the machine field
        # 'old-registry-unit' must start at column 0 on its OWN line even
        # though /proc/<pid>/cmdline has no trailing newline.
        self.assertRegex(p1_out, r"(?m)^  cmdline: /opt/ak-sending/ak-cli registry-server *$")
        self.assertRegex(p1_out, r"(?m)^  old-registry-unit: oldregistry\.service$")
        log = self.stubs_log()
        self.assertIn("sudo -n ss -ltnp", log)
        self.assertIn("sudo -n ss -lunp", log)
        self.assertIn("sudo -n cat /proc/4242/cgroup", log)
        self.assertIn("sudo -n ls -1 /home", log)
        self.assertIn("sudo -n env LC_ALL=C ls -d /home/otheruser/.ak-sending/registry.key", log)
        n_ss = len([l for l in log.splitlines() if l.startswith("ss ")])
        n_sudo_ss = len([l for l in log.splitlines() if l.startswith("sudo -n ss ")])
        self.assertEqual(n_ss, n_sudo_ss,
                         f"every ss probe must run through sudo -n (bare={n_ss}, sudo={n_sudo_ss})")

    # -- K2-R3(a): EVERY privileged probe refuses when undeterminable --------
    def test_k2r3_phase1_fails_when_occupancy_undeterminable(self):
        body = (
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SS_FAIL=1 bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: cannot determine TCP listeners", p1_out)
        self.assertIn("refusing before any switchover", p1_out)

    def test_k2r3_phase1_fails_when_cgroup_read_undeterminable(self):
        # Registry occupant pid KNOWN, but the privileged cgroup read is
        # denied: round 3 piped the error away and reported
        # "not-a-systemd-unit"; now it must refuse before any switchover.
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'CAT_FAIL_MATCH="/cgroup" bash "$PROBE_DIR/p1.sh" '
            '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: cannot read /proc/4242/cgroup", p1_out)
        self.assertNotIn("registry-data:", p1_out)

    def test_k2r3_phase1_fails_when_home_enumeration_undeterminable(self):
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'LS_FAIL_MATCH="-1 /home" bash "$PROBE_DIR/p1.sh" '
            '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: cannot enumerate /home", p1_out)
        self.assertNotIn("registry-data:", p1_out,
                         "an existing private identity must never read as 'none found'")

    def test_k2r3_phase1_fails_when_identity_probe_undeterminable(self):
        # Enumeration succeeds; ONLY the private key existence probe is
        # denied — still a hard refusal (the reviewer's selective-failure
        # reproduction).
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'LS_FAIL_MATCH="registry.key" bash "$PROBE_DIR/p1.sh" '
            '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("ERROR: cannot probe /home/otheruser/.ak-sending/registry.key", p1_out)
        self.assertNotIn("registry-data:", p1_out)

    # -- K2-R3: full chain phase1 output -> real_run decision -----------------
    def test_k2r3_decision_refuses_unknown_registry_occupant(self):
        # Round 3 refused an unknown STUN occupant but accepted an unknown
        # REGISTRY occupant; the real decision block must refuse both.
        log_text = (
            "sudo: passwordless-ok\n"
            "port 17200/tcp: in use (pid unknown, unit not-a-systemd-unit)\n"
            "port 17101/udp: free\n"
        )
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, log_text, "driver-decision-unknown.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("FAIL: TCP 17200 is in use but its owning process cannot be identified",
                      out)

    def test_k2r3_decision_refuses_non_systemd_registry_occupant(self):
        log_text = (
            "sudo: passwordless-ok\n"
            "port 17200/tcp: in use (pid 4242, unit not-a-systemd-unit)\n"
            "port 17101/udp: free\n"
        )
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, log_text, "driver-decision-nonsys.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("FAIL: TCP 17200 is held by a non-systemd process", out)

    def test_k2r3_cmdline_without_newline_flows_through_real_decision(self):
        # The exact no-newline /proc/<pid>/cmdline shape: phase1 must emit
        # TERMINATED machine fields and the REAL decision block must then
        # extract the old registry unit (previously the joined line made the
        # anchored parser return nothing and phase456 never stopped/backed
        # up the real old unit).
        self.make_private_discovery_model()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, p1_out, "driver-decision-chain.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("registry-port occupant unit: oldregistry", out)
        self.assertIn("old registry unit 'oldregistry.service' will be stopped", out)


class BaselineContrastTests(ProbeHarness):
    """K 复核 K2-R5 old-code proofs: EXECUTE the install/verify/preflight/
    decision code actually shipped in the git baselines with the same
    (stateful) stubs. Baseline shape gaps SKIP (old code may differ); the
    current script's shape gaps FAIL (see ProbeHarness.section)."""

    P456_ENV_PREBUILT = (
        'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
        'STAGING_DIR="$STAGING_DIR" STAGED_BIN="$STAGED_BIN" SHORT_SHA=deadbee '
        'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
        'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
        'DATA_SOURCE_DIR= CLOSE_7100=0 '
    )
    P456_ENV_BUILD = (
        'TS=t12345 BUILD_DIR="$PROBE_DIR/builddir" PREBUILT_MODE=0 SHORT_SHA=deadbee '
        'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
        'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
        'DATA_SOURCE_DIR= CLOSE_7100=0 '
    )

    def baseline_text(self, rev: str) -> str:
        text = git_show(rev, "scripts/deploy_linux.sh")
        if not text:
            self.skipTest(f"git baseline {rev} not available locally; "
                          "cannot run the old-code contrast honestly")
        return text

    def phase_defs(self, text: str, rev: str) -> str:
        """write_phase1 + write_phase456 + write_phase7 of a baseline."""
        return (
            self.section(text, "write_phase1() {", "# Phase 3: build on the server",
                         origin=rev, skip_if_missing=True)
            + self.section(text, "write_phase456() {", "# Phase 7: verification",
                           origin=rev, skip_if_missing=True)
            + self.section(text, "write_phase7() {", "\nreal_run() {",
                           origin=rev, skip_if_missing=True)
        )

    def run_baseline_p456(self, rev: str, env_mode: str, base: str,
                          run_env: dict | None = None, models: str = ""):
        """Generate + run a baseline's OWN write_phase456 with the shared
        stateful stubs. Returns (driver_rc, driver_out, p456_out_text)."""
        text = self.baseline_text(rev)
        if "PREBUILT_MODE=1" in env_mode:
            stage = (
                'STAGING_DIR="$PROBE_DIR/staging"\n'
                'STAGED_BIN="$STAGING_DIR/ak-cli"\n'
                'mkdir -p "$STAGING_DIR"\n'
                'cp "$AK_CLI_STUB" "$STAGED_BIN"\n'
                'chmod +x "$STAGED_BIN"\n'
            )
        else:
            stage = (
                'mkdir -p "$PROBE_DIR/builddir/target/release"\n'
                'cp "$AK_CLI_STUB" "$PROBE_DIR/builddir/target/release/ak-cli"\n'
                'chmod +x "$PROBE_DIR/builddir/target/release/ak-cli"\n'
            )
        body = (
            stage
            + models
            + env_mode
            + f'write_phase456 "$PROBE_DIR/{base}.sh" || {{ echo GEN_FAILED; exit 90; }}\n'
            + f'bash "$PROBE_DIR/{base}.sh" > "$PROBE_DIR/{base}.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, rev) + BOX_MODELS, body,
                                  env=run_env, name=f"driver-{rev}-{base}.sh")
        out_path = self.probe / f"{base}.out"
        p456_out = out_path.read_text(encoding="utf-8") if out_path.exists() else ""
        return rc, out, p456_out

    # -- K3-N1: the round-3 candidate aborts a fresh install; round-2 and the
    #    current tree complete it. Old-code statements are behavioral.
    def test_k3n1_round3_candidate_aborts_fresh_install(self):
        rc, out, p456_out = self.run_baseline_p456(
            PREV_REV, self.P456_ENV_PREBUILT, "p456prev")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out,
                      "the round-3 candidate must abort the fresh install (K3-N1)")
        log = self.stubs_log()
        self.assertIn("systemctl stop ak-registry-server.service", log,
                      "the round-3 candidate stopped the ABSENT unit blindly")
        self.assertNotIn("not present; nothing to stop", p456_out)
        self.assertFalse((self.probe / "opt-ak" / "ak-cli").exists(),
                         "the round-3 candidate never reached the install step")

    def test_k3n1_round2_candidate_completes_the_same_fresh_install(self):
        # Under the EXACT systemd v255 missing-unit diagnostic the reviewer
        # verified, the round-2 candidate still completes (its stop failures
        # were swallowed) while the round-3/4 candidates aborted.
        rc, out, _ = self.run_baseline_p456(
            PREV3_REV, self.P456_ENV_PREBUILT, "p456prev3",
            run_env={"SYSTEMCTL_CAT_MSG": "No files found for ak-registry-server.service."})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-2 candidate completed the same fresh fixture (reviewer anchor)")

    # -- K2-N1(a): the round-3 candidate's recovery does not RESTART the
    #    still-active unit — the NEW binary keeps running under RECOVERY OK.
    def test_k2n1_round3_partial_start_keeps_new_binary_running(self):
        rc, out, p456_out = self.run_baseline_p456(
            PREV_REV, self.P456_ENV_PREBUILT, "p456prev",
            run_env={"SYSTEMCTL_FAIL_SPEC": "ak-stun-server.service:enable-now"},
            models="make_upgrade_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        self.assertIn("RECOVERY OK", p456_out,
                      "the round-3 candidate reported a successful recovery")
        # ... while the registry unit still RUNS the NEW binary (start on an
        # active unit is a no-op): disk restored, process not.
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_STUB,
                         "the round-3 recovery left the NEW process running (the defect)")
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "the round-3 recovery did restore the disk")

    def test_k2n1_round3_disabled_inactive_not_restored(self):
        rc, out, p456_out = self.run_baseline_p456(
            PREV_REV, self.P456_ENV_PREBUILT, "p456prev",
            run_env={"SYSTEMCTL_FAIL_SPEC": "ak-stun-server.service:enable-now"},
            models="make_disabled_inactive_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        self.assertIn("RECOVERY OK", p456_out)
        # The switch enabled+started registry; the round-3 recovery neither
        # disabled nor stopped it (its capture blanked these states to
        # "unknown"), so it stays enabled+active on the NEW binary.
        self.assertEqual(self.unit_state("ak-registry-server.service", "enabled"), "1")
        self.assertEqual(self.unit_state("ak-registry-server.service", "active"), "active")
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_STUB)

    def test_k2n1_round3_rename_failure_ignores_backup(self):
        rc, out, p456_out = self.run_baseline_p456(
            PREV_REV, self.P456_ENV_PREBUILT, "p456prev",
            run_env={"MV_FAIL_MATCH": "ak-cli.old-t12345",
                     "FAIL_TEE_UNIT": "ak-registry-server"},
            models="make_upgrade_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        self.assertIn("RECOVERY OK", p456_out)
        # The pre-stop backup existed, but the round-3 recovery only looked
        # for ak-cli.old-<ts>: the NEW binary stayed installed and running.
        self.assertTrue((self.probe / "opt-ak" / "backup-t12345" / "ak-cli").exists())
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "the round-3 recovery left the NEW binary installed")
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_STUB)

    # -- K2-N1: baselines leave the box stopped and broken at the first unit
    #    write. The old code has no (reaching) recovery; the NEW tests above
    #    assert recovery EXISTS.
    def _assert_unmitigated_unit_write_failure(self, rc, out, p456_out):
        self.assertEqual(rc, 0, out)  # the driver harness itself must succeed
        self.assertIn("RUN_RC=1", out)
        self.assertIn("forced failure writing /etc/systemd/system/ak-registry-server.service",
                      p456_out)
        log = self.stubs_log()
        stop = self.log_line(log, "systemctl stop ak-registry-server.service")
        fail = self.log_line(log, "tee /etc/systemd/system/ak-registry-server.service")
        self.assertLess(stop, fail, "old code stops the services before dying at the unit write")
        self.assertNotIn("systemctl start", log)
        self.assertNotIn("systemctl daemon-reload", log)
        self.assertFalse(
            [l for l in log.splitlines()
             if l.startswith("cp ") and re.search(r" /etc/systemd/system/\S+$", l)],
            f"unexpected unit restore in the old code: {log}")
        etc = self.probe / "etc" / "systemd" / "system"
        self.assertIn("CORRUPTED-UNIT-PARTIAL",
                      (etc / "ak-registry-server.service").read_text(encoding="utf-8"))
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "old code leaves the NEW binary in place")

    def test_k2n1_round2_candidate_unit_write_failure_is_unmitigated(self):
        rc, out, p456_out = self.run_baseline_p456(
            PREV3_REV, self.P456_ENV_PREBUILT, "p456prev3",
            run_env={"FAIL_TEE_UNIT": "ak-registry-server"},
            models="make_upgrade_model\n")
        self.assertEqual(rc, 0, out)
        self._assert_unmitigated_unit_write_failure(rc, out, p456_out)

    def test_k2n1_baseline_unit_write_failure_is_unmitigated(self):
        rc, out, p456_out = self.run_baseline_p456(
            BASE_REV, self.P456_ENV_BUILD, "p456base",
            run_env={"FAIL_TEE_UNIT": "ak-registry-server"},
            models="make_upgrade_model\n")
        self.assertEqual(rc, 0, out)
        self._assert_unmitigated_unit_write_failure(rc, out, p456_out)

    # -- K2-R2: the round-2 candidate swallows the stop failure and its
    #    verify still passes while the OLD process keeps running.
    def test_k2r2_round2_candidate_swallow_and_false_success(self):
        text = self.baseline_text(PREV3_REV)
        defs = self.phase_defs(text, PREV3_REV)
        body = (
            'make_upgrade_model\n'
            'STAGING_DIR="$PROBE_DIR/staging"\n'
            'STAGED_BIN="$STAGING_DIR/ak-cli"\n'
            'mkdir -p "$STAGING_DIR"\n'
            'cp "$AK_CLI_STUB" "$STAGED_BIN"\n'
            'chmod +x "$STAGED_BIN"\n'
            + self.P456_ENV_PREBUILT
            + 'write_phase456 "$PROBE_DIR/p456prev3.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_FAIL_MATCH="stop ak-registry-server.service" '
            'bash "$PROBE_DIR/p456prev3.sh" > "$PROBE_DIR/p456prev3.out" 2>&1; echo "INSTALL_RC=$?"\n'
        )
        rc, out = self.run_driver(defs + BOX_MODELS, body, name="driver-prev3-stop.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        log = self.stubs_log()
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertNotIn("systemctl start", log,
                         "old code never restarts the still-active old unit")
        self.assertIn("systemctl enable --now ak-registry-server.service", log)
        # The old process model: the unit stays "active" with the OLD image
        # (start-on-active is a no-op) while the DISK path holds the new one.
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_OLD)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB)
        # The candidate's verify checked only the CONFIGURED ExecStart path
        # — and passes on exactly this state.
        body7 = (
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BIN"\n'
            'chmod +x "$REMOTE_BIN"\n'
            'SHORT_SHA=deadbee write_phase7 "$PROBE_DIR/p7prev3.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_IS_ACTIVE=active SYSTEMCTL_LIVE_PATH="$REMOTE_BIN" '
            'SYSTEMCTL_MAINPID=4242 '
            'PROC_EXE_MAP="/proc/4242/exe=$REMOTE_BASE/ak-cli.old-t12345" '
            'bash "$PROBE_DIR/p7prev3.sh" > "$PROBE_DIR/p7prev3.out" 2>&1; echo "VERIFY_RC=$?"\n'
        )
        rc, out = self.run_driver(defs + BOX_MODELS, body7, name="driver-prev3-p7.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("VERIFY_RC=0", out,
                      "the round-2 candidate's verify must pass in this state (the defect)")
        self.assertNotIn("readlink", self.stubs_log(),
                         "the round-2 candidate never inspects the running process")

    def test_k2r2_baseline_verify_only_checks_disk_version(self):
        text = self.baseline_text(BASE_REV)
        body = (
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BIN"\n'
            'chmod +x "$REMOTE_BIN"\n'
            'SHORT_SHA=deadbee write_phase7 "$PROBE_DIR/p7base.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_IS_ACTIVE=active SYSTEMCTL_MAINPID=4242 '
            'PROC_EXE_MAP="/proc/4242/exe=$REMOTE_BASE/ak-cli.old-t12345" '
            'bash "$PROBE_DIR/p7base.sh" > "$PROBE_DIR/p7base.out" 2>&1; echo "VERIFY_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, BASE_REV), body,
                                  name="driver-base-p7.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("VERIFY_RC=0", out,
                      "the baseline verify passes on the old-process/disk-swapped state")
        self.assertNotIn("readlink", self.stubs_log())

    # -- K2-R3: baselines' bare probes / parsing gaps, same stateful stubs.
    def test_k2r3_baseline_preflight_is_blind_to_foreign_process_and_private_dir(self):
        text = self.baseline_text(BASE_REV)
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1base.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1base.sh" > "$PROBE_DIR/p1base.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, BASE_REV), body,
                                  name="driver-base-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1base.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: in use (pid unknown, unit not-a-systemd-unit)", p1_out)
        self.assertIn("port 17101/udp: in use (pid unknown, unit not-a-systemd-unit)", p1_out)
        self.assertNotIn("old-registry-unit:", p1_out)
        self.assertNotIn("registry-data:", p1_out)
        for line in self.stubs_log().splitlines():
            if line.startswith("sudo ") and " ss " in f" {line} ":
                self.fail(f"baseline unexpectedly used sudo for ss: {line}")

    def test_k2r3_round3_cmdline_gap_breaks_real_decision(self):
        # Round 3 terminated nothing: its own phase1 output joins
        # 'old-registry-unit' onto the cmdline line and its REAL decision
        # block then extracts nothing — the full-chain defect.
        text = self.baseline_text(PREV_REV)
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1prev.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1prev.sh" > "$PROBE_DIR/p1prev.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV_REV), body,
                                  name="driver-prev-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev.out").read_text(encoding="utf-8")
        self.assertIn("pid 4242, unit oldregistry.service", p1_out)
        self.assertNotRegex(p1_out, r"(?m)^  old-registry-unit: oldregistry\.service$")
        block = self.decision_block(text, PREV_REV)
        rc, out = self.run_decision(block, p1_out, "driver-prev-decision.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("registry-port occupant unit: none-detected", out,
                      "the round-3 chain lost the old registry unit (the parser gap)")


if __name__ == "__main__":
    unittest.main()
