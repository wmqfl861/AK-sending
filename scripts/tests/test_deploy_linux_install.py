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
PREV5_REV = "46473fc"  # round-5 candidate (K5 findings: unanchored absence,
                       # deactivating-as-stopped, standard-name exclusion)
PREV6_REV = "e8f811d"  # round-6 candidate (K6 findings: absent-file +
                        # running process passed, drop-in overrides ignored)
PREV7_REV = "bb1f673"  # round-7 candidate (K7 findings: cmdline read
                        # failure skipped the cross-check, --db-path uncompared)
PREV8_REV = "12a71c9"
PREV9_REV = "806ea6c"
PREV10_REV = "1fad0a2"  # round-11 candidate (K12 findings: LF truncation,
                         # config-cwd resolution, swallowed key chmod)
PREV10_PRE_REV = "09fceb6"  # round-10 tip (pre-move key chmod: set -e caught it)
PREV11_REV = "0b41201"
PREV12_REV = "943a346"
PREV13_REV = "f425256"
PREV14_REV = "0602f92"
PREV15_REV = "7a93314"
PREV16_REV = "4db2b4f"
PREV17_REV = "1d76871"  # round-18 tip (K19 findings: greedy db-value
                         # pre-parse, dangling/duplicate db bindings)
PREV18_REV = "110bb34"  # round-20 tip (K20 findings: globbing in the token
                         # walks retargets a bracket db filename; empty and
                         # option-shaped --db-path values accepted)  # round-17 tip (K18 findings: byte-evidence
                         # producer error ignored, equals-form verification bug)  # round-16 tip (K17 findings: extraction-error
                         # passthrough, substring-split id-dir corruption)  # round-15 tip (K16 findings: partial-snapshot
                         # acceptance, extraction-error-as-absent, bracket
                         # db rewrite no-op)  # round-14 tip (K15 findings: environ fail-open
                         # verification reads, relative --db-path unbound)  # round-13 tip (K14 findings: fail-open second cwd
                         # read, relative --id-dir unbound to running cwd)  # round-12 tip (K13 findings: cwd byte fidelity,
                         # generated-script path re-interpretation)  # round-9 candidate (K10 findings: environ parsing
                        # semantics, destination conflicts skipped)  # round-8 candidate (K9 finding: migration source
                        # picked by enumeration order, not runtime identity)

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
# Whole-daemon failure (e.g. the standard D-Bus ENOENT diagnostic): every
# invocation fails with this message and rc 1 (K3-N1 round-6 model).
if [ -n "${SYSTEMCTL_FAIL_ALL_MSG:-}" ]; then
  echo "$SYSTEMCTL_FAIL_ALL_MSG" >&2
  exit 1
fi
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
  # Optional transitional-start model (K2-N1 round 6): a unit left
  # 'deactivating' keeps its MainPID and running image, exactly like a real
  # stop-in-progress service.
  if [ -n "${SYSTEMCTL_START_STATE_UNIT:-}" ] && [ "$uu" = "$SYSTEMCTL_START_STATE_UNIT" ]; then
    echo "${SYSTEMCTL_START_STATE:-active}" > "$ST/$uu.active"
  else
    echo active > "$ST/$uu.active"
  fi
  exe=$(read_execstart "$uu")
  printf '%s\n' "$exe" > "$ST/$pid.exe"
  printf '%s\n' "$exe" > "$ST/$uu.running"
  cp "$exe" "$ST/$uu.running.bin" 2>/dev/null || : > "$ST/$uu.running.bin"
}
is_unit_active() { [ "$(cat "$ST/$1.active" 2>/dev/null || true)" = active ]; }
u="${!#}"
case "$1" in
  cat)
    # systemctl cat emits the MERGED unit: main fragment first, then every
    # drop-in in precedence order (so /etc overrides come last). '# path'
    # comment headers separate the files, exactly like the real tool.
    found=""
    if [ -z "${SYSTEMCTL_CAT_FORCE_FAIL:-}" ]; then
      for ud in "$PROBE_DIR/etc/systemd/system" "$PROBE_DIR/run/systemd/system" \
                "$PROBE_DIR/usr/lib/systemd/system" "$PROBE_DIR/lib/systemd/system"; do
        if [ -f "$ud/$u" ]; then found="$ud/$u"; break; fi
      done
    fi
    if [ -n "$found" ]; then
      echo "# $found"
      while IFS= read -r cl || [ -n "$cl" ]; do echo "$cl"; done < "$found"
      for ddn in "$PROBE_DIR/usr/lib/systemd/system/$u.d" \
                 "$PROBE_DIR/lib/systemd/system/$u.d" \
                 "$PROBE_DIR/run/systemd/system/$u.d" \
                 "$PROBE_DIR/etc/systemd/system/$u.d"; do
        [ -d "$ddn" ] || continue
        for df in "$ddn"/*.conf; do
          [ -f "$df" ] || continue
          echo "# $df"
          while IFS= read -r cl || [ -n "$cl" ]; do echo "$cl"; done < "$df"
        done
      done
      exit 0
    fi
    # Default: systemd v255's standard missing-unit diagnostic (verified
    # against upstream systemctl-util.c, LoadState=not-found finish path),
    # trailing period included. SYSTEMCTL_CAT_MSG overrides the text — '%s'
    # is replaced with the unit name (use it for per-unit fixtures); a text
    # without '%s' is used verbatim for every unit (bus errors).
    # SYSTEMCTL_CAT_FORCE_FAIL makes cat fail even for EXISTING units (a
    # daemon/query failure on a live box).
    msg="${SYSTEMCTL_CAT_MSG:-No files found for %s.}"
    case "$msg" in
      *%s*) echo "${msg%%%s*}$u${msg#*%s}" >&2 ;;
      *) echo "$msg" >&2 ;;
    esac
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
    # K11-N1 model hook: run at STOP ENTRY (service still active, writes
    # still possible) — models a real commit landing while the old service
    # runs. '@PROBE_DIR@' in the command is substituted with the probe path.
    if [ -n "${SYSTEMCTL_STOP_HOOK:-}" ] && [ "$u" = "${SYSTEMCTL_STOP_HOOK_UNIT:-}" ]; then
      hook_cmd=${SYSTEMCTL_STOP_HOOK//@PROBE_DIR@/$PROBE_DIR}
      eval "$hook_cmd" || exit 1
    fi
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
if [ -n "${SS_EMPTY:-}" ]; then exit 0; fi
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
"@REAL_CP@" -a "${mapped[@]}" || exit 1
# CP_CORRUPT_MATCH: after a successful copy whose args match, corrupt the
# DESTINATION bytes — models a mid-copy corruption the cmp-verify must catch.
if [ -n "${CP_CORRUPT_MATCH:-}" ]; then
  case "$*" in
    *"$CP_CORRUPT_MATCH"*)
      printf 'corrupted-by-stub\n' >> "${mapped[$(( ${#mapped[@]} - 1 ))]}"
      echo "cp stub: corrupted destination for $*" >&2 ;;
  esac
fi
exit 0
"""

CMP_STUB = r"""#!/usr/bin/env bash
set -u
printf 'cmp %s\n' "$*" >> "$PROBE_DIR/stubs.log"
mapped=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*|/usr/lib/systemd/*|/lib/systemd/*|/run/systemd/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_CMP@" "${mapped[@]}"
"""

CHMOD_STUB = r"""#!/usr/bin/env bash
set -u
printf 'chmod %s\n' "$*" >> "$PROBE_DIR/stubs.log"
# K12-N1 model: only the PRIVATE-KEY chmod fails; everything else behaves.
if [ "${FAIL_KEY_CHMOD:-0}" = 1 ] && [ "$1" = 600 ] && [ "${@: -1}" = "$PROBE_DIR/opt-ak/data/registry.key" ]; then
  echo "chmod stub: forced key permission failure" >&2
  exit 1
fi
exec "@REAL_CHMOD@" "$@"
"""

GREP_STUB = r"""#!/usr/bin/env bash
# K10-N1 (round 17/18) models:
#  - GREP_EXTRACT_VAR: EVERY '-za -m1 ^VAR=' grep for that variable exits 2
#    (the round-17 persistent-error shape).
#  - GREP_EXTRACT_FAIL_AT=2: only the Nth '-za -m1 ^VAR=' call for that
#    variable fails (rc=2, no stdout) — the round-18 shape where the FIRST
#    (presence) grep succeeds and the SECOND (value extraction) errors.
# Everything else passes through to the real grep.
if [ "${1:-}" = -za ] && [ "${2:-}" = -m1 ] && [ -n "${GREP_EXTRACT_VAR:-}" ] \
   && [ "$3" = "^${GREP_EXTRACT_VAR}=" ]; then
  n=0
  cnt="$PROBE_DIR/grep-${GREP_EXTRACT_VAR}-count"
  [ ! -f "$cnt" ] || read -r n < "$cnt"
  n=$((n + 1))
  printf '%s\n' "$n" > "$cnt"
  hit=0
  if [ -n "${GREP_EXTRACT_FAIL_AT:-}" ] && [ "$n" -eq "$GREP_EXTRACT_FAIL_AT" ]; then
    hit=1
  elif [ -z "${GREP_EXTRACT_FAIL_AT:-}" ]; then
    hit=1
  fi
  if [ "$hit" = 1 ]; then
    printf 'REVIEW_GREP_%s_ERROR=2\n' "$n" >> "$PROBE_DIR/stubs.log"
    echo "grep stub: forced extraction error for $GREP_EXTRACT_VAR (call $n)" >&2
    exit 2
  fi
fi
# K10-N1 (round 19) models for the BYTE-EVIDENCE grep ('-zao -m1 ^VAR=.*'):
#  - GREP_EVIDENCE_MODE=partial: run the real grep, emit the full match
#    minus its last two bytes (still ending in one LF), then exit 2 — the
#    reviewer's exact partial-output-then-error shape whose truncated count
#    '-1' matched the bash-captured length and passed the old check.
#  - GREP_EVIDENCE_MODE=complete: emit the FULL real match, then exit 2.
#  - GREP_EVIDENCE_MODE=none: no output, exit 2.
if [ "${1:-}" = -zao ] && [ "${2:-}" = -m1 ] && [ -n "${GREP_EVIDENCE_VAR:-}" ] \
   && [ "$3" = "^${GREP_EVIDENCE_VAR}=.*" ] && [ -n "${GREP_EVIDENCE_MODE:-}" ]; then
  case "${GREP_EVIDENCE_MODE}" in
    partial)
      "@REAL_GREP@" "$@" > "$PROBE_DIR/evidence-record" 2>/dev/null
      count=$(wc -c < "$PROBE_DIR/evidence-record")
      head -c "$((count - 2))" "$PROBE_DIR/evidence-record"
      printf 'REVIEW_BYTE_GREP_PARTIAL_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
      exit 2 ;;
    complete)
      "@REAL_GREP@" "$@" 2>/dev/null
      printf 'REVIEW_BYTE_GREP_COMPLETE_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
      exit 2 ;;
    none)
      printf 'REVIEW_BYTE_GREP_EMPTY_ERROR=2\n' >> "$PROBE_DIR/stubs.log"
      exit 2 ;;
  esac
fi
exec "@REAL_GREP@" "$@"
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
# K10-N1 (round 16) reviewer model: count every /proc/<pid>/environ read
# (ENV_READ_n markers); ENV_READ_FAIL_FROM makes every read from the Nth
# onward fail (a failing verification read used to skip the fidelity check).
if [ "$*" = /proc/4242/environ ]; then
  n=0
  [ ! -f "$PROBE_DIR/env-read-count" ] || read -r n < "$PROBE_DIR/env-read-count"
  n=$((n + 1))
  printf '%s\n' "$n" > "$PROBE_DIR/env-read-count"
  printf 'ENV_READ_%s\n' "$n" >> "$PROBE_DIR/stubs.log"
  if [ -n "${ENV_READ_FAIL_FROM:-}" ] && [ "$n" -ge "$ENV_READ_FAIL_FROM" ]; then
    printf 'ENV_READ_FAILED_%s\n' "$n" >> "$PROBE_DIR/stubs.log"
    exit 1
  fi
  # K10-N1 (round 17) model: emit the FIRST complete record, then fail —
  # tee succeeds on partial output, so only the READER's rc reveals the
  # truncation (PIPESTATUS[0] guard).
  if [ "${ENV_PARTIAL_FAIL:-0}" = 1 ]; then
    printf 'REVIEW_CAT_PARTIAL_ERROR=74\n' >> "$PROBE_DIR/stubs.log"
    printf 'HOME=/home/otheruser\0'
    exit 74
  fi
fi
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
  # K10-N1(b) round 13: the RUNNING process's actual working directory
  # (PROC_CWD_MAP="/proc/<pid>/cwd=<abs path>"; a mapping of 'unreadable'
  # makes the read fail like a denied /proc read would).
  # K10-N1 (round 14): PROC_CWD_RAW_FILE emits the file's EXACT bytes plus
  # the tool's own newline — mirroring a real readlink whose target path
  # itself ends in, or contains, LF bytes (the line-based map cannot
  # express that).
  /proc/[0-9]*/cwd)
    # K10-N1 (round 15) reviewer model: the FIRST cwd read succeeds, the
    # SECOND fails (marker-file based). Under the single-read fix the second
    # read never happens, so the injected failure can never open the check.
    if [ "${CWD_FAIL_ON_SECOND:-0}" = 1 ]; then
      if [ -f "$PROBE_DIR/cwd-read-once" ]; then
        printf 'review-cwd-second-read-failed\n' >> "$PROBE_DIR/stubs.log"
        exit 1
      fi
      : > "$PROBE_DIR/cwd-read-once"
    fi
    # K10-N1 (round 17) model: emit a COMPLETE value, THEN fail — tee
    # succeeds on the output, so only the reader's rc (PIPESTATUS[0])
    # reveals the failed read.
    if [ "${CWD_OUTPUT_THEN_FAIL:-0}" = 1 ]; then
      printf '/home/runtime-old\n'
      printf 'CWD_NONZERO_WITH_OUTPUT\n' >> "$PROBE_DIR/stubs.log"
      exit 1
    fi
    if [ -n "${PROC_CWD_RAW_FILE:-}" ]; then
      cat "$PROC_CWD_RAW_FILE"
      printf '\n'
      exit 0
    fi
    if [ -n "${PROC_CWD_MAP:-}" ]; then
      while IFS='=' read -r k v; do
        if [ "$k" = "$target" ]; then
          if [ "$v" = "unreadable" ]; then
            echo "readlink stub: forced cwd read failure" >&2
            exit 1
          fi
          printf '%s\n' "$v"
          exit 0
        fi
      done <<< "$PROC_CWD_MAP"
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
  # K 复核 K21-N2: LS_HOME_FILE provides the listing VERBATIM (one entry
  # per line, exact bytes) so a legal home name with SPACES stays one ls
  # record — the LS_HOME_ENTRIES word-split form cannot express that (and
  # the round-22 fix makes phase1 iterate ls LINES, never words).
  if [ -n "${LS_HOME_FILE:-}" ] && [ -f "$LS_HOME_FILE" ]; then
    cat "$LS_HOME_FILE"
    exit 0
  fi
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
    "cmp": CMP_STUB,
    "chmod": CHMOD_STUB,
    "grep": GREP_STUB,
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
make_deleted_unit_files_model() {
  # Round-7 fixture: both services KEEP running (active, MainPID, old image)
  # while their unit FILES are gone — cat naturally answers v255's
  # 'No files found for <unit>.', is-enabled 'not-found'-class, is-active
  # 'active'. systemd keeps running units after file deletion.
  make_upgrade_model
  rm -f "$PROBE_DIR/etc/systemd/system/ak-registry-server.service"
  rm -f "$PROBE_DIR/etc/systemd/system/ak-stun-server.service"
  echo "MODEL=running units whose unit FILES were deleted"
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
  DATA_SOURCE_DIR= CLOSE_7100=0 REMOTE_DATA_PREOWNED=1 \
  write_phase456 "$PROBE_DIR/p456.sh" || { echo GEN_FAILED; exit 90; }
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
        for tool in ("cp", "cat", "sed", "ls", "mv", "cmp", "chmod", "grep"):
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

    def decisions_block(self, text: str, origin: str) -> str:
        """real_run's phase 4-6 configuration decisions (compose_registry_exec
        call, DATA_SOURCE_DIR, REG_RW) — executed together with the decision
        block so an identified occupant's config flows into the CHOSEN
        values (K 复核 K2-R3 round 6)."""
        return self.section(
            text,
            '  # ---- deployment decisions for phase 4-6',
            '  # ---- phase 4/5/6: install + systemd + firewall',
            origin=origin, skip_if_missing=True)

    def run_decision_and_config(self, phase1_log: str, name: str,
                                text: str | None = None,
                                origin: str = "the current script"):
        """Execute the REAL decision block + phase-4-6 decisions +
        compose_registry_exec against a phase1 report, echoing the CHOSEN
        configuration (the reviewer's config-selection repro chain)."""
        text = text if text is not None else self.text
        compose = self.section(text, "compose_registry_exec() {",
                               "# systemd unit templates",
                               origin=origin, skip_if_missing=True)
        body = (
            compose + "\n"
            'WORK="$PROBE_DIR"\n'
            'KEEP_LEGACY_SIGNAL=0\n'
            'CHECK_ONLY=0\n'
            'deploy_decision() {\n'
            + self.decision_block(text, origin)
            + self.decisions_block(text, origin)
            + '\n\necho "CHOSEN_EXEC=$REGISTRY_EXEC"\n'
            'echo "CHOSEN_DATA_SOURCE=${DATA_SOURCE_DIR:-}"\n'
            'echo "CHOSEN_RW=$REG_RW"\n'
            'echo "CHOSEN_OLD_REGISTRY=${OLD_REGISTRY_UNIT:-}"\n'
            '}\n'
            'echo "DECISION_BEGIN"\n'
            'deploy_decision\n'
            'echo "DECISION_DONE"\n'
        )
        (self.probe / "phase1.log").write_text(phase1_log, encoding="utf-8", newline="\n")
        return self.run_driver("", body, name=name)

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

    def make_private_discovery_model(self, occupant: str = "oldregistry.service",
                                     vendor: bool = False, main_exec: str | None = None,
                                     dropin: list[str] | None = None,
                                     cmdline_extra: str = "",
                                     unit_service_lines: list[str] | None = None,
                                     environ: bytes | None = None):
        """K2-R3 OFFLINE permission model: a registry port held by ANOTHER
        USER's process (pid only visible to root) plus a PRIVATE identity
        directory under a foreign home. /proc/<pid>/cmdline deliberately has
        NO trailing newline, exactly like Linux. `occupant` names the unit
        the cgroup resolves to; `vendor` places that unit file under
        /usr/lib/systemd/system instead of /etc/systemd/system (round 6);
        `main_exec` replaces the main fragment's ExecStart tail; `dropin`
        writes /etc/systemd/system/<unit>.d/override.conf with those lines
        (round 7 drop-in semantics); `cmdline_extra` appends arguments to
        the running instance's cmdline."""
        proc = self.probe / "proc"
        (proc / "4242").mkdir(parents=True, exist_ok=True)
        (proc / "4243").mkdir(parents=True, exist_ok=True)
        (proc / "4242" / "cgroup").write_text(f"0::/system.slice/{occupant}\n",
                                              encoding="utf-8", newline="\n")
        (proc / "4243" / "cgroup").write_text("0::/system.slice/oldsignal.service\n",
                                              encoding="utf-8", newline="\n")
        # Default argv mirrors the default unit's own arguments — the
        # running instance was started by that unit (round 9: the
        # cross-check compares EFFECTIVE values, defaults included, so the
        # fixture must state what the instance actually runs).
        if not cmdline_extra:
            cmdline_extra = "--port 17200 --id-dir /home/otheruser/.ak-sending"
        # Argv-precise: every whitespace-separated argument becomes its own
        # NUL-terminated argv token, exactly like a real exec (the reviewer's
        # fidelity correction — one token per argument, never one blob).
        cmdline = b"/opt/ak-sending/ak-cli\0registry-server\0"
        for tok in cmdline_extra.split():
            cmdline += tok.encode() + b"\0"
        (proc / "4242" / "cmdline").write_bytes(cmdline)
        home = self.probe / "home" / "otheruser" / ".ak-sending"
        home.mkdir(parents=True, exist_ok=True)
        (home / "registry.key").write_text("private-key-bytes", encoding="utf-8")
        (home / "registry.db").write_text("private-db-bytes", encoding="utf-8")
        if vendor:
            unit_dir = self.probe / "usr" / "lib" / "systemd" / "system"
        else:
            unit_dir = self.probe / "etc" / "systemd" / "system"
        unit_dir.mkdir(parents=True, exist_ok=True)
        exec_tail = main_exec or ("/opt/ak-sending/ak-cli registry-server --port 17200 "
                                  "--id-dir /home/otheruser/.ak-sending")
        service_block = "[Service]\n"
        if unit_service_lines:
            service_block += "".join(line + "\n" for line in unit_service_lines)
        (unit_dir / occupant).write_text(
            "[Unit]\nDescription=foreign-user registry\n\n" + service_block +
            f"ExecStart={exec_tail}\n\n[Install]\nWantedBy=multi-user.target\n",
            encoding="utf-8", newline="\n")
        if environ is not None:
            (self.probe / "proc" / "4242" / "environ").write_bytes(environ)
        if dropin is not None:
            dropin_dir = self.probe / "etc" / "systemd" / "system" / f"{occupant}.d"
            dropin_dir.mkdir(parents=True, exist_ok=True)
            (dropin_dir / "override.conf").write_text(
                "".join(line + "\n" for line in dropin), encoding="utf-8", newline="\n")

    K9_BASE = "/opt/ak-sending/ak-cli registry-server --port 17200"

    def _make_k9_runtime_identity_fixture(self, root_decoy: bool):
        """The K9 reviewer's exact fixture: an identified oldregistry.service
        with User=otheruser, Environment=HOME=/home/otheruser,
        UnsetEnvironment=USERPROFILE, an argv-precise NUL cmdline (NO
        --id-dir on either side), /proc/4242/environ carrying HOME and no
        USERPROFILE, and the real key/db under /home/otheruser/.ak-sending.
        Optionally adds an UNRELATED /root/.ak-sending with its own key/db."""
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "Environment=HOME=/home/otheruser",
                                "UnsetEnvironment=USERPROFILE"],
            environ=b"HOME=/home/otheruser\0USER=otheruser\0")
        if root_decoy:
            root = self.probe / "root" / ".ak-sending"
            root.mkdir(parents=True, exist_ok=True)
            (root / "registry.key").write_text("unrelated-synthetic-root-key",
                                               encoding="utf-8")
            (root / "registry.db").write_text("unrelated-synthetic-root-db",
                                              encoding="utf-8")

    def _make_runtime_source(self):
        src = self.probe / "home" / "otheruser" / ".ak-sending"
        src.mkdir(parents=True, exist_ok=True)
        (src / "registry.key").write_text("private-key-bytes", encoding="utf-8")
        (src / "registry.db").write_text("private-db-bytes", encoding="utf-8")
        return src

    def _make_destination_decoy(self):
        dest = self.probe / "opt-ak" / "data"
        dest.mkdir(parents=True, exist_ok=True)
        (dest / "registry.key").write_text("unrelated-target-key", encoding="utf-8")
        (dest / "registry.db").write_text("unrelated-target-db", encoding="utf-8")
        return dest

    def _make_k13_quoted_fixture(self, profile: str):
        """A REAL USERPROFILE value containing shell-significant bytes
        (quote or dollar) that survives preflight verbatim; the true source
        dir holds runtime-real-* data while the de-quoted/expanded dir holds
        different data."""
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/configured-new",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0USERPROFILE="
                    + profile.encode() + b"\0USER=otheruser\0")
        ef = self.probe / "etc/default/oldregistry"
        ef.parent.mkdir(parents=True, exist_ok=True)
        ef.write_text('HOME=/home/otheruser' + chr(10)
                      + 'USERPROFILE="' + profile + '"' + chr(10)
                      + 'USER=otheruser' + chr(10),
                      encoding="utf-8", newline="\n")
        # The TRUE (byte-verbatim) source dir — holds runtime-real data.
        true_dir = self.probe / profile.lstrip("/") / ".ak-sending"
        true_dir.mkdir(parents=True, exist_ok=True)
        for name in ("registry.key", "registry.db"):
            (true_dir / name).write_text(f"runtime-real-{name}", encoding="utf-8")
        return true_dir

    def _run_k10_install(self, data_source: str, preowned: str, name: str,
                         run_env: dict | None = None, old_registry: str = "",
                         models: str = ""):
        """Generate + run phase456 with the decision-derived inputs baked
        (REGISTRY_EXEC/REG_RW/DATA_SOURCE_DIR/REMOTE_DATA_PREOWNED exactly as
        real_run would bake them for the K9 runtime fixture). `models` is a
        bash-side box-model preamble (e.g. 'make_upgrade_model' + newline)."""
        body = (
            models
            + 
            'mkdir -p "$PROBE_DIR/staging"\n'
            'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"\n'
            'chmod +x "$PROBE_DIR/staging/ak-cli"\n'
            'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            'SHORT_SHA=deadbee '
            'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
            f'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT={old_registry} '
            f'DATA_SOURCE_DIR={data_source} CLOSE_7100=0 REMOTE_DATA_PREOWNED={preowned} '
            f'write_phase456 "$PROBE_DIR/{name}.sh" || {{ echo GEN_FAILED; exit 90; }}\n'
            f'bash "$PROBE_DIR/{name}.sh" > "$PROBE_DIR/{name}.out" 2>&1; '
            f'echo "INSTALL_RC=$?"\n'
        )
        return self.generate_and_run_p456(body, run_env=run_env)

    def _make_k15_env_fixture(self, tail_lf: bool):
        profile = b"/home/otheruser" + (b"\x0a" if tail_lf else b"")
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=" + profile
                    + b"\0USER=otheruser\0")
        envf = self.probe / "etc/default/oldregistry"
        envf.parent.mkdir(parents=True, exist_ok=True)
        envf.write_bytes(b'HOME=/home/otheruser\nUSERPROFILE="' + profile + b'"\n')
        prefix = self.probe / "home/otheruser/.ak-sending"
        prefix.mkdir(parents=True, exist_ok=True)
        for name in ("registry.key", "registry.db"):
            (prefix / name).write_text(f"env-prefix-decoy-{name}", encoding="utf-8")

    def _make_k15_db_fixture(self, dbpath: str):
        identity = "/home/otheruser/.ak-sending"
        args = "--port 17200 --id-dir " + identity + " --db-path " + dbpath
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        (self.probe / identity.lstrip("/") / "registry.key").write_text(
            "same-synthetic-identity", encoding="utf-8")
        # Two REAL SQLite objects with distinct markers (the reviewer's
        # shape): the true object under the running cwd, the decoy under /
        # (the new unit's default cwd).
        import sqlite3
        for location, marker in (
                ("/home/runtime-old/" + dbpath, "runtime-real"),
                ("/" + dbpath, "new-cwd-decoy")):
            db = self.probe / location.lstrip("/")
            db.parent.mkdir(parents=True, exist_ok=True)
            if db.exists():
                db.unlink()
            con = sqlite3.connect(db)
            con.execute("CREATE TABLE review_marker(value TEXT)")
            con.execute("INSERT INTO review_marker VALUES (?)", (marker,))
            con.commit()
            con.close()

    def _make_k16_env_fixture(self, profile: bytes):
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=" + profile
                    + b"\0USER=otheruser\0")
        envf = self.probe / "etc/default/oldregistry"
        envf.parent.mkdir(parents=True, exist_ok=True)
        envf.write_bytes(b'HOME=/home/otheruser\nUSERPROFILE="' + profile + b'"\n')
        for d, marker in (("home/runtime-real/.ak-sending", "runtime-real"),
                          ("home/otheruser/.ak-sending", "fallback-decoy")):
            dd = self.probe / d
            dd.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (dd / name).write_text(f"{marker}-{name}", encoding="utf-8")

    def _make_k17_db_fixture(self, dbpath: str):
        identity = "/home/otheruser/.ak-sending"
        args = "--port 17200 --id-dir " + identity + " --db-path " + dbpath
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        (self.probe / identity.lstrip("/") / "registry.key").write_text(
            "same-synthetic-identity", encoding="utf-8")

    def _db_arg_of(self, exec_line: str) -> str:
        toks = exec_line.split()
        for i, tok in enumerate(toks):
            if tok == "--db-path":
                return toks[i + 1]
            if tok.startswith("--db-path="):
                return tok[len("--db-path="):]
        return ""

    def _make_k19_env_fixture(self):
        """The reviewer's fixture: USERPROFILE=/home/runtime-real followed by
        TWO real LF bytes then the NUL terminator; HOME=/home/otheruser.
        Bash substitution strips NUL + trailing LFs, leaving the prefix; the
        prefix dir holds prefix-decoy data (the WRONG source — the true
        runtime dir contains an LF and cannot be this dir)."""
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/runtime-real\x0a\x0a\0"
                    + b"USER=otheruser\0")
        envf = self.probe / "etc/default/oldregistry"
        envf.parent.mkdir(parents=True, exist_ok=True)
        envf.write_bytes(b"HOME=/home/otheruser\n"
                         b"USERPROFILE=\"/home/runtime-real\n\n\"\nUSER=otheruser\n")
        for d, marker in (("home/runtime-real/.ak-sending", "prefix-decoy"),
                          ("home/otheruser/.ak-sending", "fallback-decoy")):
            dd = self.probe / d
            dd.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (dd / name).write_text(f"{marker}-{name}", encoding="utf-8")

    def _run_p1_with_evidence_mode(self, mode: str):
        return self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EVIDENCE_VAR=USERPROFILE GREP_EVIDENCE_MODE=' + mode
            + ' bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))

    def _make_k20_literal_fixture(self, configured_extra: str = ""):
        """Configured unit AND running argv carry
        '--id-dir /home/otheruser/.ak-sending --db-path=--db-path --port 17200'
        (the db value is the literal filename '--db-path'; cwd /home/runtime-old).
        The REAL object lives at /home/runtime-old/--db-path; decoys sit at
        /home/runtime-old/--port (the misparsed binding) and /--db-path."""
        identity = "/home/otheruser/.ak-sending"
        base_args = ("--id-dir " + identity + " --db-path=--db-path --port 17200")
        configured = base_args + configured_extra
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + configured,
            cmdline_extra=base_args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        live = self.probe / identity.lstrip("/")
        live.mkdir(parents=True, exist_ok=True)
        (live / "registry.key").write_text("synthetic-runtime-key", encoding="utf-8")
        (live / "registry.db").write_text("synthetic-identity-default-db",
                                         encoding="utf-8")
        import sqlite3
        for obj, marker in (("/home/runtime-old/--db-path", "runtime-real"),
                            ("/home/runtime-old/--port", "misparsed-token-decoy"),
                            ("/--db-path", "new-cwd-decoy")):
            path = self.probe / obj.lstrip("/")
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists():
                path.unlink()
            con = sqlite3.connect(path)
            con.execute("CREATE TABLE review_marker(value TEXT)")
            con.execute("INSERT INTO review_marker VALUES (?)", (marker,))
            con.commit()
            con.close()

    def _make_k20_glob_fixture(self, sep: str, value: str = "db[1].sqlite"):
        """K20-N1 reviewer fixture: unit AND running argv carry
        '--port 17200 --id-dir <id> --db-path<sep><value>'; service cwd
        /home/runtime-old. The REAL object is /home/runtime-old/<value>
        ('runtime-real'); the glob-decoy sits at /home/runtime-old/db1.sqlite
        ('glob-decoy') — the file an unquoted walk's pathname expansion
        retargets the bracket filename to."""
        identity = "/home/otheruser/.ak-sending"
        args = "--port 17200 --id-dir " + identity + " --db-path" + sep + value
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        live = self.probe / identity.lstrip("/")
        live.mkdir(parents=True, exist_ok=True)
        (live / "registry.key").write_text("synthetic-runtime-key", encoding="utf-8")
        (live / "registry.db").write_text("synthetic-identity-default-db",
                                         encoding="utf-8")
        import sqlite3
        objects = [("/home/runtime-old/" + value, "runtime-real")]
        if value != "db1.sqlite":
            objects.append(("/home/runtime-old/db1.sqlite", "glob-decoy"))
        for obj, marker in objects:
            path = self.probe / obj.lstrip("/")
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists():
                path.unlink()
            con = sqlite3.connect(path)
            con.execute("CREATE TABLE review_marker(value TEXT)")
            con.execute("INSERT INTO review_marker VALUES (?)", (marker,))
            con.commit()
            con.close()

    def _drop_glob_match(self, sep: str):
        """The reviewer's cwd decoy: a plain file in the DECISION's cwd that
        pathname expansion of the bracket filename matches ('db1.sqlite' for
        the space form, '--db-path=db1.sqlite' for the equals form)."""
        name = "--db-path=db1.sqlite" if sep == "=" else "db1.sqlite"
        (self.probe / name).write_text("synthetic-shell-glob-match",
                                       encoding="utf-8")

    def _run_p1_isolated(self, script_name: str, out_name: str):
        """Generate phase1 and run it from an ISOLATED cwd — the reviewer
        kept phase1's cwd free of glob-target files so only the decision
        walk's expansion was in play (K20-N1)."""
        return self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/' + script_name + '" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'mkdir -p "$PROBE_DIR/preflight-cwd"' + chr(10)
            + '(cd "$PROBE_DIR/preflight-cwd" '
            + '&& PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/' + script_name + '") '
            + '> "$PROBE_DIR/' + out_name + '" 2>&1; echo "RUN_RC=$?"' + chr(10))

    def _make_oldregistry_running(self):
        """The reviewer's old-service state: oldregistry.service
        enabled/active under pid 4242 running the old image."""
        state = self.probe / "systemctl-state"
        state.mkdir(parents=True, exist_ok=True)
        for suffix, text in (("enabled", "1\n"), ("active", "active\n"),
                             ("pid", "4242\n")):
            (state / ("oldregistry.service." + suffix)).write_text(
                text, encoding="utf-8")
        (state / "4242.exe").write_text("/opt/ak-sending/ak-cli\n",
                                        encoding="utf-8")
        (state / "oldregistry.service.running").write_text(
            "/opt/ak-sending/ak-cli\n", encoding="utf-8")
        (state / "oldregistry.service.running.bin").write_text(
            AK_CLI_OLD, encoding="utf-8")

    def _chosen_of(self, out: str):
        """Parse a decision driver's CHOSEN_* echoes into
        (exec, rw, old_registry)."""
        def field(tag):
            line = [l for l in out.splitlines() if l.startswith(tag)][0]
            return line[len(tag):]
        return (field("CHOSEN_EXEC="), field("CHOSEN_RW="),
                field("CHOSEN_OLD_REGISTRY="))

    def _install_with_chosen(self, chosen_exec: str, chosen_rw: str,
                             old_registry: str, out_prefix: str,
                             text: str | None = None,
                             rev: str | None = None,
                             data_source: str = "/home/otheruser/.ak-sending"):
        """Run the REAL write_phase456 (current tree, or a baseline's own)
        with the decision's CHOSEN values baked — the reviewer's full-chain
        install step."""
        defs = (self.phase_defs(text, rev) if text is not None
                else self.definitions())
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" '
            + 'STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + chosen_exec + '" ' + chr(10)
            + 'REG_RW="' + chosen_rw + '" '
            + 'LEGACY_UNIT= OLD_REGISTRY_UNIT=' + old_registry + ' '
            + 'DATA_SOURCE_DIR=' + data_source + ' '
            + 'CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/' + out_prefix + '.sh" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/' + out_prefix + '.sh" '
            + '> "$PROBE_DIR/' + out_prefix + '.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10))
        return self.run_driver(defs + BOX_MODELS, body,
                               name="driver-install-" + out_prefix + ".sh")

    def _make_k21_id_fixture(self, sep: str,
                             idpath: str = "/home/otheruser/--id-dir"):
        """K21-N1 reviewer fixture: unit AND running argv carry
        '--id-dir<sep><idpath> --port 17200' where the id path ENDS in the
        literal '--id-dir'; service cwd /home/runtime-old. The REAL
        directory <idpath> holds runtime-real-key/db; the misbind decoy at
        /home/runtime-old/--port holds substring-decoy-key/db (what the
        greedy sed + cwd binding selected)."""
        args = "--id-dir" + sep + idpath + " --port 17200"
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        for path, key, db in ((idpath, "runtime-real-key", "runtime-real-db"),
                              ("/home/runtime-old/--port",
                               "substring-decoy-key", "substring-decoy-db")):
            live = self.probe / path.lstrip("/")
            live.mkdir(parents=True, exist_ok=True)
            (live / "registry.key").write_text(key, encoding="utf-8")
            (live / "registry.db").write_text(db, encoding="utf-8")
        self._make_oldregistry_running()

    def _make_k21_home_fixture(self, raw_homes: str, decoy: bool):
        """K21-N2 reviewer fixture: a fresh host (no occupant, no units)
        whose /home listing (LS_HOME_RAW, verbatim lines) may carry a
        SPACED home name. The real candidate dir gets synthetic-existing-*
        markers; when decoy=True the split-prefix directory /home/other
        gets synthetic-split-decoy-* markers DIRECTLY inside it (what the
        word-splitting loop probed)."""
        for home in [h for h in raw_homes.split("\n") if h]:
            live = self.probe / "home" / home / ".ak-sending"
            live.mkdir(parents=True, exist_ok=True)
            (live / "registry.key").write_text("synthetic-existing-key",
                                               encoding="utf-8")
            (live / "registry.db").write_text("synthetic-existing-db",
                                              encoding="utf-8")
        if decoy:
            prefix = self.probe / "home" / "other"
            prefix.mkdir(parents=True, exist_ok=True)
            (prefix / "registry.key").write_text("synthetic-split-decoy-key",
                                                 encoding="utf-8")
            (prefix / "registry.db").write_text("synthetic-split-decoy-db",
                                                encoding="utf-8")

    def _run_p1_homes(self, script: str, out: str, raw_homes: str):
        """phase1 on a fresh host (SS_EMPTY) with a verbatim /home listing
        (LS_HOME_FILE: exact bytes, one entry per line — spaced names stay
        one ls record)."""
        listing = self.probe / "ls-home-listing.txt"
        listing.write_text(raw_homes, encoding="utf-8", newline="\n")
        return self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/' + script + '" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'SS_EMPTY=1 LS_HOME_FILE=' + listing.as_posix()
            + ' bash "$PROBE_DIR/' + script + '" '
            + '> "$PROBE_DIR/' + out + '" 2>&1; echo "RUN_RC=$?"' + chr(10))

    def _k20_decision_of(self, configured: str, running: str, name: str,
                         baseline_rev: str = ""):
        identity = "/home/otheruser/.ak-sending"
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + configured,
            cmdline_extra=running,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        if baseline_rev:
            # K 复核 K2-R5: a baseline contrast runs the BASELINE's OWN
            # phase1 as well — the old-code proof must demonstrate the old
            # CHAIN accepting the shape. (Round 21: the current phase1
            # refuses CLI-invalid --db-path grammar pre-stop, so feeding a
            # current-tree phase1 report into the baseline decision would
            # only prove the decision's ERROR relay, not the defect.)
            # baseline_text/phase_defs are provided by BaselineContrastTests.
            text = self.baseline_text(baseline_rev)
            rc, out = self.run_driver(
                self.phase_defs(text, baseline_rev) + BOX_MODELS,
                'write_phase1 "$PROBE_DIR/p1prev.sh" '
                '|| { echo GEN_FAILED; exit 90; }' + chr(10)
                + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
                + 'bash "$PROBE_DIR/p1prev.sh" '
                + '> "$PROBE_DIR/p1prev.out" 2>&1; echo "RUN_RC=$?"'
                + chr(10))
            self.assertEqual(rc, 0, out)
            p1_out = (self.probe / "p1prev.out").read_text(encoding="utf-8")
            return self.run_decision_and_config(
                p1_out, name, text=text, origin=f"the {baseline_rev} baseline")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        return self.run_decision_and_config(p1_out, name)

    def _make_k18_db_fixture(self, identity: str, dbpath: str):
        args = "--port 17200 --id-dir " + identity + " --db-path " + dbpath
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        live = self.probe / identity.lstrip("/")
        live.mkdir(parents=True, exist_ok=True)
        (live / "registry.key").write_text("synthetic-runtime-key", encoding="utf-8")
        (live / "registry.db").write_text("synthetic-identity-default-db",
                                         encoding="utf-8")
        import sqlite3
        live_db = dbpath if dbpath.startswith("/") else "/home/runtime-old/" + dbpath
        alt_db = None if dbpath.startswith("/") else "/" + dbpath
        for obj, marker in ((live_db, "runtime-real"), (alt_db, "new-cwd-decoy")):
            if obj is None:
                continue
            path = self.probe / obj.lstrip("/")
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists():
                path.unlink()
            con = sqlite3.connect(path)
            con.execute("CREATE TABLE review_marker(value TEXT)")
            con.execute("INSERT INTO review_marker VALUES (?)", (marker,))
            con.commit()
            con.close()

    def _make_k14_relative_fixture(self, identity: str):
        """Unit + argv both carry '--id-dir <identity>'; the running cwd is
        /home/runtime-old. The TRUE source (runtime-real-*) lives under
        /home/runtime-old/state; the DEPLOYMENT-cwd decoy (deploy-cwd-decoy-*)
        lives under the probe root's state/ — the wrong source rounds <= 14
        copied."""
        args = "--port 17200 --id-dir " + identity
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        for rel, tag in (("home/runtime-old/state", "runtime-real"),
                         ("state", "deploy-cwd-decoy")):
            d = self.probe / rel
            d.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (d / name).write_text(f"{tag}-{name}", encoding="utf-8")

    def _make_k13_cwd_fixture(self, cwd_bytes: bytes):
        """Empty USERPROFILE + HOME set; runtime cwd comes from a RAW-BYTES
        readlink emission (the line-based map cannot express LF-bearing
        paths). The configured WorkingDirectory decoy and the cwd prefix
        decoy both hold data."""
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/configured-new",
                                "Environment=HOME=/home/otheruser",
                                "Environment=USERPROFILE="],
            environ=b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0")
        raw = self.probe / "cwd-bytes"
        raw.write_bytes(cwd_bytes)
        for rel, tag in (("home/runtime-old", "cwd-prefix-decoy"),
                         ("home/configured-new", "configured-decoy")):
            d = self.probe / rel / ".ak-sending"
            d.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (d / name).write_text(f"{tag}-{name}", encoding="utf-8")
        return raw

    def _make_sqlite_runtime_source(self):
        """The K11 reviewer's fixture: a real schema-v3 SQLite source
        database — the DDL is extracted from
        crates/ak-signal/src/server_v2/store.rs exactly as the review did —
        holding one active/revision=1 invite code, plus the registry.key."""
        import sqlite3
        store = (ROOT / "crates/ak-signal/src/server_v2/store.rs").read_text(encoding="utf-8")
        ddl = re.findall(r'const DDL_[A-Z_]+: &str = "(.*?)";', store, re.S)
        self.assertGreaterEqual(len(ddl), 1, "store.rs DDL constants not found")
        src = self.probe / "home" / "otheruser" / ".ak-sending"
        src.mkdir(parents=True, exist_ok=True)
        (src / "registry.key").write_text("private-key-bytes", encoding="utf-8")
        db = src / "registry.db"
        con = sqlite3.connect(db)
        try:
            con.executescript(chr(10).join(ddl))
            con.execute("PRAGMA user_version=3")
            con.execute(
                "INSERT INTO invite_codes (lookup_digest,code_id,owner_device_id,"
                "created_unix_ms,state,revision,create_request_id) "
                "VALUES (?,?,?,?,?,?,?)",
                (b"d" * 32, b"c" * 16, b"o" * 32, 1, "active", 1, b"r" * 16))
            con.commit()
        finally:
            con.close()
        return db

    def _write_late_commit_hook(self):
        """The review's real late commit: revoke the invite code and bump the
        revision, executed by the systemctl stub at STOP ENTRY (the old
        service still active). The database path travels as an ARGUMENT so
        MSYS converts it for the native Python; chr(10) keeps this source
        free of escape-sequence hazards."""
        nl = chr(10)
        hook = self.probe / "late_commit.py"
        hook.write_text(
            "import pathlib, sqlite3, sys" + nl
            + "assert pathlib.Path(sys.argv[2]).read_text().strip() == 'active', "
            "'commit landed after the service went inactive'" + nl
            + "with sqlite3.connect(sys.argv[1]) as c:" + nl
            + "    c.execute(\"UPDATE invite_codes SET state='revoked', "
            "revision=revision+1 WHERE code_id=? AND revision=1 AND "
            "state='active'\", (b'c'*16,))" + nl
            + "print('LATE_COMMIT=revoked')" + nl,
            encoding="utf-8", newline="\n")
        return ("python @PROBE_DIR@/late_commit.py"
                " @PROBE_DIR@/home/otheruser/.ak-sending/registry.db"
                " @PROBE_DIR@/systemctl-state/oldregistry.service.active")

    def _db_rows(self, path):
        import sqlite3
        con = sqlite3.connect(path)
        try:
            return con.execute("SELECT state, revision FROM invite_codes").fetchall()
        finally:
            con.close()

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
            run_env={"SYSTEMCTL_CAT_MSG": "No files found for %s."})
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
        self.assertIn("ERROR: cannot determine whether oldregistry.service exists", p1_out)
        self.assertIn("cannot determine existence of /etc/systemd/system/oldregistry.service",
                      p1_out)
        self.assertIn("Permission denied", p1_out)
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

    # -- K3-N1 (round 6): the standard D-Bus ENOENT diagnostic is NOT absence --
    BUS_ENOENT = "Failed to connect to bus: No such file or directory"

    def test_k3n1_bus_enoent_cat_only_never_exits_install_success(self):
        # Reviewer repro (b): ONLY systemctl cat fails with the standard bus
        # diagnostic; everything else works. Round 5 matched the bare
        # 'No such file' substring, classified the EXISTING enabled/active
        # units as absent, skipped EVERY stop and exited 0 with a new disk
        # under still-running old processes. Now: refusal, nothing touched.
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
            run_env={"SYSTEMCTL_CAT_MSG": self.BUS_ENOENT,
                     "SYSTEMCTL_CAT_FORCE_FAIL": "1"})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out,
                      "an undeterminable unit state must never exit install success")
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("systemctl cat unusable", p456_out)
        self.assertNotIn("nothing to stop", p456_out)
        log = self.stubs_log()
        self.assertNotIn("systemctl stop", log, "no service may be touched")
        # The box is untouched: old binary, old running image.
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_OLD)
        self.assertEqual(self.unit_state("ak-registry-server.service", "active"), "active")

    def test_k3n1_bus_enoent_on_all_calls_refuses_before_switchover(self):
        # Reviewer repro (a): EVERY systemctl call fails with the bus
        # diagnostic. Round 5 captured absent:: and installed binary/units
        # before dying at daemon-reload into recovery; the refusal must come
        # BEFORE anything is touched.
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
            run_env={"SYSTEMCTL_FAIL_ALL_MSG": self.BUS_ENOENT})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("systemctl cat unusable", p456_out)
        self.assertNotIn("nothing to stop", p456_out)
        self.assertNotIn("systemctl stop", self.stubs_log())
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "no binary may be installed on a dead systemd")

    def test_k3n1_bus_enoent_cat_plus_reload_failure_no_silent_success(self):
        # cat fails with the bus diagnostic AND daemon-reload is broken: the
        # outcome must be a loud refusal/recovery — never a silent success.
        rc, out = self.generate_and_run_p456(
            'make_upgrade_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
            run_env={"SYSTEMCTL_CAT_MSG": self.BUS_ENOENT,
                     "SYSTEMCTL_CAT_FORCE_FAIL": "1",
                     "SYSTEMCTL_FAIL_MATCH": "daemon-reload"})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        self.assertNotIn("RUN_RC=0", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("systemctl cat unusable", p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)

    # -- K2-N1 (round 6): transitional states are UNDETERMINABLE ---------------
    def test_k2n1_deactivating_unit_never_recovery_ok_with_new_image(self):
        # Reviewer repro: disabled/inactive pre-state; enable --now starts
        # the NEW registry, STUN fails, and the registry is left mid-stop
        # ('deactivating', MainPID alive, NEW image). Round 5 classified
        # deactivating as stopped, skipped the stop and printed RECOVERY OK
        # with the new process still running. Now: conservative stop +
        # CONFIRM — the unit ends verified-inactive with no NEW image.
        rc, out = self.generate_and_run_p456(
            'make_disabled_inactive_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n',
            run_env={"SYSTEMCTL_FAIL_SPEC": "ak-stun-server.service:enable-now",
                     "SYSTEMCTL_START_STATE_UNIT": "ak-registry-server.service",
                     "SYSTEMCTL_START_STATE": "deactivating"})
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        # The unit must NOT be left deactivating with the NEW image while
        # recovery claims success.
        self.assertNotEqual(self.unit_state("ak-registry-server.service", "active"),
                            "deactivating")
        self.assertNotEqual(self.unit_running_bin("ak-registry-server.service"),
                            AK_CLI_STUB,
                            "the NEW image must not survive recovery")
        self.assertEqual(self.unit_state("ak-registry-server.service", "active"), "inactive",
                         "recovery must conservatively stop and CONFIRM the final state")
        self.assertEqual(self.unit_state("ak-registry-server.service", "pid"), "",
                         "the stopped unit must have no live MainPID")
        # Recovery may only report OK for the CONFIRMED outcome.
        self.assertIn("RECOVERY OK", p456_out)

    # -- K2-R3 (round 6): standard-named vendor unit keeps its identity --------
    def test_k2r3_standard_named_vendor_unit_keeps_legacy_id_dir(self):
        # The reviewer's fixture: the port occupant IS ak-registry-server.service,
        # its unit file lives ONLY under /usr/lib/systemd/system and carries
        # --id-dir /var/lib/ak-legacy. Round 5 excluded the standard name
        # from the all-dir lookup, so the config was dropped and the deploy
        # silently re-pointed the identity to the default dir.
        self.make_private_discovery_model(occupant="ak-registry-server.service",
                                          vendor=True,
                                          cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        vendor_unit = (self.probe / "usr" / "lib" / "systemd" / "system"
                       / "ak-registry-server.service")
        vendor_unit.write_text(
            "[Unit]\nDescription=vendor-dir standard registry\n\n[Service]\n"
            "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /var/lib/ak-legacy\n\n[Install]\nWantedBy=multi-user.target\n",
            encoding="utf-8", newline="\n")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("unit ak-registry-server: present", p1_out)
        self.assertNotIn("unit ak-registry-server: absent", p1_out)
        self.assertIn("port 17200/tcp: in use (pid 4242, unit ak-registry-server.service)",
                      p1_out)
        # Standard-named occupant: config read (feeds existing_exec), no
        # old-registry-unit FIELD (it is already in the managed set).
        self.assertNotIn("old-registry-unit:", p1_out)
        self.assertRegex(
            p1_out, r"(?m)^  ExecStart=/opt/ak-sending/ak-cli registry-server "
                    r"--port 17200 --id-dir /var/lib/ak-legacy$")
        # Full config-selection chain: the REAL decision + phase-4/6
        # decisions + compose_registry_exec keep the legacy identity dir.
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-stdvendor.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("DECISION_DONE", out)
        self.assertIn("existing registry ExecStart: /opt/ak-sending/ak-cli "
                      "registry-server --port 17200 --id-dir /var/lib/ak-legacy", out)
        self.assertIn("existing ak-registry-server unit will be upgraded", out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("--id-dir /var/lib/ak-legacy", exec_line[0],
                      "the legacy identity dir must survive config selection")
        self.assertIn("CHOSEN_RW=/var/lib/ak-legacy", out,
                      "ReadWritePaths must keep pointing at the legacy identity dir")

    def test_k2r3_standard_named_absent_fresh_install_unchanged(self):
        # Inverse: no occupant, no unit anywhere -> plain absent, no
        # discovery side effects (fresh install semantics unchanged).
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SS_EMPTY=1 bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("unit ak-registry-server: absent", p1_out)
        self.assertIn("unit ak-stun-server: absent", p1_out)
        self.assertIn("port 17200/tcp: free", p1_out)

    # -- K3-N1 (round 7): absent unit FILE + running process is NOT fresh ----
    def test_k3n1_deleted_unit_file_with_running_process_refuses(self):
        # The reviewer's fixture: the unit files are deleted (between phase1
        # and phase456 + daemon-reload) while both services keep running —
        # cat naturally answers v255's 'No files found for <unit>.',
        # is-enabled reports not-found-class state, is-active 'active' with
        # the MainPID alive on the OLD image. Rounds <= 6 printed 'nothing
        # to stop', installed the new disk image and exited 0 with the
        # original process untouched. Now: refusal BEFORE the switchover,
        # disk and old process untouched.
        rc, out = self.generate_and_run_p456(
            'make_deleted_unit_files_model\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out,
                      "a running service whose unit file is gone must never be treated "
                      "as a fresh install")
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("absent but its run state is 'active'", p456_out)
        self.assertIn("no recoverable pre-state", p456_out)
        self.assertNotIn("nothing to stop", p456_out)
        self.assertNotIn("systemctl stop", self.stubs_log())
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "the disk must stay on the old binary")
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active",
                             f"the running {unit} must be untouched")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD)

    # -- K2-R3 (round 7): drop-in overrides and the running-cmdline cross-check
    AK_DEFAULT = "/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-default"
    AK_LEGACY = "/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /var/lib/ak-legacy"

    def _run_p1_and_config(self, name: str):
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8") \
            if (self.probe / "p1.out").exists() else ""
        return rc, out, p1_out

    def test_k2r3_dropin_clear_override_selects_legacy_id_dir(self):
        # The reviewer's exact fixture: main fragment (vendor dir) carries
        # --id-dir /var/lib/ak-default; a legal /etc drop-in CLEARS ExecStart
        # and re-sets it to /var/lib/ak-legacy; the running cmdline carries
        # ak-legacy. Rounds <= 6 read only the main file and chose
        # ak-default with an empty migration source.
        self.make_private_discovery_model(
            vendor=True, main_exec=self.AK_DEFAULT,
            dropin=["[Service]", "ExecStart=", f"ExecStart={self.AK_LEGACY}"],
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        rc, out, p1_out = self._run_p1_and_config("dropin-clear")
        self.assertIn("RUN_RC=0", out)
        self.assertRegex(p1_out, r"(?m)^  old-registry-unit: oldregistry\.service$")
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*--id-dir /var/lib/ak-legacy$")
        self.assertNotRegex(p1_out, r"(?m)^  ExecStart=.*ak-default$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-dropin.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("/var/lib/ak-legacy", exec_line[0])
        self.assertIn("CHOSEN_RW=/var/lib/ak-legacy", out)

    def test_k2r3_main_file_only_with_matching_cmdline_unchanged(self):
        # Inverse of the drop-in case: a single main-file ExecStart whose
        # relevant args MATCH the running cmdline flows through unchanged.
        self.make_private_discovery_model(
            cmdline_extra="--port 17200 --id-dir /home/otheruser/.ak-sending")
        rc, out, p1_out = self._run_p1_and_config("main-only")
        self.assertIn("RUN_RC=0", out)
        self.assertRegex(p1_out,
                         r"(?m)^  ExecStart=/opt/ak-sending/ak-cli registry-server "
                         r"--port 17200 --id-dir /home/otheruser/\.ak-sending$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-mainonly.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("--id-dir /home/otheruser/.ak-sending",
                      [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0])

    def test_k2r3_dropin_append_resolved_by_running_cmdline(self):
        # A drop-in WITHOUT the clearing line legally APPENDS: two effective
        # entries. The running cmdline resolves exactly one (ak-legacy).
        self.make_private_discovery_model(
            vendor=True, main_exec=self.AK_DEFAULT,
            dropin=["[Service]", f"ExecStart={self.AK_LEGACY}"],
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        rc, out, p1_out = self._run_p1_and_config("dropin-append")
        self.assertIn("RUN_RC=0", out)
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*--id-dir /var/lib/ak-legacy$")
        self.assertNotRegex(p1_out, r"(?m)^  ExecStart=.*ak-default$")

    def test_k2r3_dropin_append_ambiguous_refuses(self):
        # Same two entries but the running cmdline matches NEITHER: the
        # effective config cannot be determined -> refusal before any
        # switchover.
        self.make_private_discovery_model(
            vendor=True, main_exec=self.AK_DEFAULT,
            dropin=["[Service]", f"ExecStart={self.AK_LEGACY}"],
            cmdline_extra="--port 17200 --id-dir /var/lib/elsewhere")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("2 effective ExecStart entries", p1_out)
        self.assertIn("does not resolve exactly one", p1_out)

    def test_k2r3_effective_config_disagreeing_with_cmdline_refuses(self):
        # Single unambiguous config entry that MATERIALLY disagrees with the
        # running instance (configured ak-default, running ak-legacy).
        self.make_private_discovery_model(
            main_exec=self.AK_DEFAULT,
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("materially disagrees with the running cmdline", p1_out)

    def test_k2r3_cmdline_read_failure_fails_closed(self):
        # Reviewer repro (round 8): SAME disagreeing fixture as above, but
        # ONLY the cmdline READ fails (the cmdline file, occupant, cgroup
        # and old data all still present). Round 7 treated the empty string
        # as "no evidence available" and SKIPPED the cross-check, so both
        # phase1 and the config chain exited 0 selecting ak-default. The
        # cmdline of an identified occupant is not optional evidence: an
        # unreadable/empty read must FAIL CLOSED.
        self.make_private_discovery_model(
            main_exec=self.AK_DEFAULT,
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'CAT_FAIL_MATCH="/proc/4242/cmdline" bash "$PROBE_DIR/p1.sh" '
            '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a failed cmdline read must never turn into a silent pass")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("unreadable or empty", p1_out)
        self.assertIn("config cross-check has no evidence", p1_out)
        self.assertNotRegex(p1_out, r"(?m)^  ExecStart=")
        # The single-variable contrast (readable cmdline) refuses with the
        # DISAGREEMENT message instead — covered by
        # test_k2r3_effective_config_disagreeing_with_cmdline_refuses.

    def test_k2r3_db_path_disagreement_refuses(self):
        # Reviewer repro (round 8): port and --id-dir MATCH, but the
        # configured --db-path (new.db) disagrees with the running instance
        # (old.db). Round 7's compared set lacked --db-path, so the chain
        # selected new.db. reg_args_agree now covers the full data/identity
        # surface (--id-dir, --db-path) plus the listen surface.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-legacy --db-path /var/lib/ak-legacy/new.db",
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy "
                          "--db-path /var/lib/ak-legacy/old.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("materially disagrees with the running cmdline", p1_out)

    def test_k2r3_db_path_consistent_config_kept(self):
        # The consistent control from the review: config and running
        # instance both use old.db — accepted, and the real decision +
        # config chain KEEPS old.db (never new.db).
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-legacy --db-path /var/lib/ak-legacy/old.db",
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy "
                          "--db-path /var/lib/ak-legacy/old.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*--db-path /var/lib/ak-legacy/old\.db$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-dbpath.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("/var/lib/ak-legacy/old.db", exec_line[0])
        self.assertNotIn("new.db", exec_line[0])

    # -- K2-R3 (round 9): EFFECTIVE values, defaults included ------------------
    def test_k2r3_runtime_default_db_vs_configured_explicit_refuses(self):
        # Reviewer repro (a): the running instance has NO explicit --db-path
        # (its effective db is the id-dir's registry.db, per ak-cli's
        # registry_db_path()); the unit config explicitly sets new.db.
        # Round 8 skipped the comparison because the running side omitted
        # the flag, so phase1+decision both passed and new.db was chosen.
        # Effective-value comparison now refuses BOTH stages.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /home/otheruser/.ak-sending "
                      "--db-path /home/otheruser/.ak-sending/new.db",
            cmdline_extra="--port 17200 --id-dir /home/otheruser/.ak-sending")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a runtime default db must compare against a configured explicit db")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("materially disagrees with the running cmdline", p1_out)
        self.assertNotRegex(p1_out, r"(?m)^  ExecStart=.*new\.db$")
        # The decision on the refused report fails too (no CHOSEN config).
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, p1_out, "driver-decision-r9.sh")
        self.assertNotEqual(rc, 0,
                            "the decision must refuse on a phase1 ERROR report:\n" + out)
        self.assertNotIn("CHOSEN_", out)

    def test_k2r3_explicit_runtime_default_db_still_refuses(self):
        # Reviewer single-variable control (b): spelling the ALREADY-
        # EFFECTIVE runtime value out explicitly keeps refusing (this
        # worked in round 8; it must keep working with defaults derived).
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /home/otheruser/.ak-sending "
                      "--db-path /home/otheruser/.ak-sending/new.db",
            cmdline_extra="--port 17200 --id-dir /home/otheruser/.ak-sending "
                          "--db-path /home/otheruser/.ak-sending/registry.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("materially disagrees with the running cmdline", p1_out)

    def test_k2r3_both_sides_implicit_db_passes(self):
        # Control (c): BOTH sides omit --db-path with the same explicit
        # id-dir — both effective dbs are <id-dir>/registry.db; accepted,
        # and the config chain carries no --db-path (default semantics).
        self.make_private_discovery_model(
            cmdline_extra="--port 17200 --id-dir /home/otheruser/.ak-sending")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*--id-dir /home/otheruser/\.ak-sending$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-r9c.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertNotIn("--db-path", exec_line[0])

    def test_k2r3_equivalent_default_spellings_pass(self):
        # Control (d): equivalent defaults with different explicit
        # spellings — runtime --id-dir X with an IMPLICIT db (effective
        # X/registry.db) vs config --id-dir X with an EXPLICIT X/registry.db
        # — agree and pass, keeping the explicit spelling in CHOSEN_EXEC.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-legacy "
                      "--db-path /var/lib/ak-legacy/registry.db",
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  ExecStart=.*--db-path /var/lib/ak-legacy/registry\.db$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-r9d.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("/var/lib/ak-legacy/registry.db", exec_line[0])

    # -- K9-N1: the migration source binds to the RUNTIME identity -------------
    K9_BASE = "/opt/ak-sending/ak-cli registry-server --port 17200"

    def test_k9n1_runtime_identity_source_without_decoy(self):
        # Regression (a): no decoy — the runtime-established identity is
        # /home/otheruser/.ak-sending (main.rs id_dir(): HOME + /.ak-sending).
        self._make_k9_runtime_identity_fixture(root_decoy=False)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k9a.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending", out)

    def test_k9n1_runtime_identity_source_survives_root_decoy(self):
        # Regression (b) — the reviewer's flip case: the SAME running
        # service, plus one unrelated /root/.ak-sending. Rounds <= 9 picked
        # /root/.ak-sending by enumeration order; the runtime identity
        # stays the migration source.
        self._make_k9_runtime_identity_fixture(root_decoy=True)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        # Enumeration REPORTS both (root first — the old flip order) ...
        self.assertIn("registry-data: /root/.ak-sending (key:yes db:yes)", p1_out)
        self.assertIn("registry-data: /home/otheruser/.ak-sending (key:yes db:yes)", p1_out)
        # ... but the runtime identity decides.
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k9b.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending", out,
                      "an unrelated root decoy must not flip the migration source")
        self.assertNotIn("CHOSEN_DATA_SOURCE=/root/.ak-sending", out)

    def test_k9n1_explicit_id_dir_wins_over_decoys(self):
        # Regression (c): explicit --id-dir in the EFFECTIVE ExecStart is
        # the runtime source, decoys notwithstanding.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-legacy",
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        root = self.probe / "root" / ".ak-sending"
        root.mkdir(parents=True, exist_ok=True)
        (root / "registry.key").write_text("unrelated-synthetic-root-key", encoding="utf-8")
        (root / "registry.db").write_text("unrelated-synthetic-root-db", encoding="utf-8")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  runtime-id-dir: /var/lib/ak-legacy$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k9c.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/var/lib/ak-legacy", out)
        self.assertNotIn("CHOSEN_DATA_SOURCE=/root/.ak-sending", out)

    def test_k9n1_no_occupant_single_candidate_selected(self):
        # Regression (d): no identified occupant + exactly ONE candidate
        # identity dir (fresh install with legacy data) — that dir is used.
        root = self.probe / "root" / ".ak-sending"
        root.mkdir(parents=True, exist_ok=True)
        (root / "registry.key").write_text("legacy-root-key", encoding="utf-8")
        (root / "registry.db").write_text("legacy-root-db", encoding="utf-8")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SS_EMPTY=1 bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("port 17200/tcp: free", p1_out)
        self.assertIn("registry-data: /root/.ak-sending (key:yes db:yes)", p1_out)
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k9d.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/root/.ak-sending", out)

    def test_k9n1_no_occupant_multiple_candidates_refuse(self):
        # Regression (e): no identified occupant + multiple candidate dirs —
        # unresolvable fresh-install ambiguity: refuse instead of head -1.
        for d in ("root/.ak-sending", "home/otheruser/.ak-sending"):
            cand = self.probe / d
            cand.mkdir(parents=True, exist_ok=True)
            (cand / "registry.key").write_text("synthetic-key", encoding="utf-8")
            (cand / "registry.db").write_text("synthetic-db", encoding="utf-8")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SS_EMPTY=1 bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertEqual(p1_out.count("registry-data:"), 2, p1_out)
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k9e.sh")
        self.assertNotEqual(
            rc, 0, "multiple unbound identity dirs must refuse, not head -1:\n" + out)
        self.assertIn("multiple identity directories", out)
        self.assertIn("no identified occupant", out)
        self.assertNotIn("CHOSEN_", out)

    def test_k9n1_missing_runtime_env_evidence_refuses(self):
        # Regression (f): occupant with no explicit --id-dir and NO
        # HOME/USERPROFILE evidence anywhere (unit has neither User=-based
        # home info nor Environment=HOME; the runtime environ carries
        # neither variable) — the CLI default would fall back to the
        # unknowable working directory: refuse before any switchover.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            environ=b"PATH=/usr/bin\0")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("neither USERPROFILE nor HOME", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    # -- K10-N1: environ existence semantics and NUL-record boundaries --------
    def test_k10n1_empty_userprofile_resolves_runtime_cwd_not_config(self):
        # K12-review fixture (b): USERPROFILE is PRESENT but EMPTY; the unit's
        # CONFIG WorkingDirectory points at /home/configured-new while the
        # RUNNING process's actual cwd (/proc/4242/cwd) is /home/runtime-old
        # — a started-then-edited unit is a legitimate state. Rounds <= 12
        # resolved the relative .ak-sending against the CONFIG value and
        # picked the configured decoy; the runtime cwd is the only evidence
        # bound to the running instance.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/configured-new",
                                "Environment=HOME=/home/otheruser",
                                "Environment=USERPROFILE="],
            environ=b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0")
        for rel, tag in (("home/runtime-old", "runtime-real"),
                         ("home/configured-new", "configured-decoy")):
            d = self.probe / rel / ".ak-sending"
            d.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (d / name).write_text(f"{tag}-{name}", encoding="utf-8")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-old/\.ak-sending$")
        self.assertNotIn("runtime-id-dir: /home/configured-new", p1_out)
        self.assertIn("readlink /proc/4242/cwd", self.stubs_log(),
                      "the RUNNING cwd must actually be queried")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k10a.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/runtime-old/.ak-sending", out)
        self.assertNotIn("CHOSEN_DATA_SOURCE=/home/configured-new", out)

    def test_k10n1_empty_userprofile_unreadable_cwd_refuses(self):
        # Same relative-base input but the runtime cwd cannot be read
        # (PROC_CWD_MAP marks it unreadable): refuse rather than fall back to
        # the config WorkingDirectory or HOME.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/configured-new",
                                "Environment=HOME=/home/otheruser",
                                "Environment=USERPROFILE="],
            environ=b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'PROC_CWD_MAP="/proc/4242/cwd=unreadable" '
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot read the RUNNING working directory", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    def test_k10n1_value_with_embedded_lf_refuses_untruncated(self):
        # K12-review fixture (a): a REAL USERPROFILE variable whose value
        # contains an embedded LF ('/home/otheruser\nsuffix'). The CLI's
        # var_os -> PathBuf -> join keeps the LF byte (reviewer verified by
        # compiling main.rs:709-716), so the true identity dir is
        # '/home/otheruser\nsuffix/.ak-sending'. Rounds <= 12 truncated at
        # the LF and CONFIRMED '/home/otheruser/.ak-sending' — a different
        # directory. This pipeline's evidence is line-based, so the honest
        # behavior is to REFUSE the untransportable value, never
        # truncate-then-confirm.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0"
                    b"USERPROFILE=/home/otheruser\nsuffix\0USER=otheruser\0")
        envf = self.probe / "etc/default/oldregistry"
        envf.parent.mkdir(parents=True, exist_ok=True)
        envf.write_text("HOME=/home/otheruser\nUSERPROFILE='/home/otheruser\nsuffix'\n"
                        "USER=otheruser\n", encoding="utf-8", newline="\n")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "an LF-bearing path value must refuse, not confirm a "
                      "truncated directory")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot carry faithfully", p1_out)
        self.assertNotRegex(p1_out, r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending")
        self.assertNotIn("runtime-id-dir:", p1_out)
        # The truncated directory must never reach the config selection.
        block = self.decision_block(self.text, "the current script")
        rc, out = self.run_decision(block, p1_out, "driver-decision-k12lf.sh")
        self.assertNotEqual(rc, 0, out)

    def test_k10n1_environ_value_with_newline_not_fabricated(self):
        # Reviewer repro (b): EXTRA's VALUE contains '\nUSERPROFILE=/root'.
        # There is NO USERPROFILE variable. Round 10's NUL->LF conversion
        # fabricated one and selected /root/.ak-sending. NUL-record parsing
        # keeps the newline inside the value: HOME wins, never /root.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "Environment=HOME=/home/otheruser",
                                'Environment="EXTRA=hello\\nUSERPROFILE=/root"'],
            environ=b"HOME=/home/otheruser\0EXTRA=hello\nUSERPROFILE=/root\0"
                    b"USER=otheruser\0")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")
        self.assertNotIn("/root/.ak-sending", p1_out)
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k10b.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending", out)
        self.assertNotIn("CHOSEN_DATA_SOURCE=/root/.ak-sending", out)

    # -- K10-N2: destination identity/db conflicts refuse before switchover ---
    def test_k10n2_differing_destination_refuses(self):
        # Reviewer repro (a): the K9 runtime fixture plus an UNRELATED
        # registry.key/registry.db under $REMOTE_DATA. Round 10 skipped the
        # copy, exited 0 and enabled the unrelated bytes. Since K11-N1 the
        # conflict is detected AFTER the stops, so the refusal goes THROUGH
        # RECOVERY: the data bytes stay untouched, no switch happens, and
        # the stopped services are restored and running again.
        self._make_runtime_source()
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k10a",
            models="make_upgrade_model\n"
            'printf unrelated-target-key > "$REMOTE_DATA/registry.key"\n'
            'printf unrelated-target-db > "$REMOTE_DATA/registry.db"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=5", out)
        out456 = (self.probe / "p456k10a.out").read_text(encoding="utf-8")
        self.assertIn("DIFFERS from the runtime identity source", out456)
        self.assertIn("never silently enable unrelated identity bytes", out456)
        # Data bytes untouched and no service switch ('untouched' refers to
        # the data and the switch — directory setup and backups may exist).
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "unrelated-target-key", "the refusal must not overwrite")
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "no service switchover may run after the refusal")
        # The stops DID happen (conflict is evaluated post-stop) — so the
        # refusal must have gone through recovery with services restored.
        log = self.stubs_log()
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertIn("RECOVERY OK", out456)
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active",
                             f"{unit} must be running again after the recovery")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD)

    def test_k10n2_identical_destination_proceeds(self):
        # Repro (b): destination byte-identical to the runtime source —
        # provably the same data; proceed without copying.
        self._make_runtime_source()
        dest = self.probe / "opt-ak" / "data"
        dest.mkdir(parents=True, exist_ok=True)
        for f, text in (("registry.key", "private-key-bytes"),
                        ("registry.db", "private-db-bytes")):
            (dest / f).write_text(text, encoding="utf-8")
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k10b")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        out456 = (self.probe / "p456k10b.out").read_text(encoding="utf-8")
        self.assertIn("already byte-identical to the runtime source", out456)
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "private-key-bytes")

    def test_k10n2_clean_destination_copies_and_verifies(self):
        # Repro (c): clean destination — the migration copies (post-stop,
        # quiesced) and cmp-verifies; the destination ends with the runtime
        # source's bytes.
        self._make_runtime_source()
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k10c")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        out456 = (self.probe / "p456k10c.out").read_text(encoding="utf-8")
        self.assertIn("cmp-verified post-stop", out456)
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "private-key-bytes")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "private-db-bytes")

    def test_k10n2_corrupted_copy_refuses_and_recovers(self):
        # K11-N1 regression (b): a post-stop copy that lands differing bytes
        # (CP_CORRUPT_MATCH corrupts the destination after the copy) fails
        # the cmp verification and flows through recovery — the stopped
        # services are restored and running on the OLD configuration.
        self._make_runtime_source()
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k10c2",
            run_env={"CP_CORRUPT_MATCH": "registry.key"},
            models="make_upgrade_model\n"
            'for f in registry.key registry.db; do rm -f "$REMOTE_DATA/$f"; done\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=5", out)
        out456 = (self.probe / "p456k10c2.out").read_text(encoding="utf-8")
        self.assertIn("does not match its runtime source (cmp failed", out456)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "no switch happens after a failed copy")
        self.assertIn("RECOVERY OK", out456)
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD,
                             "recovery must restart the services on the old image")

    # -- K11-N1: the final migration runs against the QUIESCED source --------
    def test_k11n1_late_commit_included_in_post_stop_copy(self):
        # Reviewer repro (a): the review's real-schema commit (revoke the
        # invite, bump the revision) lands at STOP ENTRY — while the old
        # service still runs. Under the pre-K11 order the copy had already
        # happened and the target stayed one revision behind (INSTALL_RC=0
        # with a stale database). Now the copy runs AFTER the stops, so the
        # commit is INCLUDED: the target database ends at revoked/revision 2.
        # The stub call ORDER is asserted directly: stop old-registry ->
        # cp (from the runtime source) -> cmp.
        db = self._make_sqlite_runtime_source()
        hook = self._write_late_commit_hook()
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k11a",
            run_env={"SYSTEMCTL_STOP_HOOK": hook,
                     "SYSTEMCTL_STOP_HOOK_UNIT": "oldregistry.service"},
            old_registry="oldregistry",
            models="make_upgrade_model\n"
            'for f in registry.key registry.db; do rm -f "$REMOTE_DATA/$f"; done\n'
            'printf "[Service]\nExecStart=/opt/ak-sending/ak-cli registry-server --port 17200\n" > "$PROBE_DIR/etc/systemd/system/oldregistry.service"\n'
            'echo 1 > "$PROBE_DIR/systemctl-state/oldregistry.service.enabled"\n'
            'echo active > "$PROBE_DIR/systemctl-state/oldregistry.service.active"\n'
            'echo 4242 > "$PROBE_DIR/systemctl-state/oldregistry.service.pid"\n'
            'printf "%s\n" "$REMOTE_BIN" > "$PROBE_DIR/systemctl-state/4242.exe"\n'
            'cp "$REMOTE_BIN" "$PROBE_DIR/systemctl-state/oldregistry.service.running.bin"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        out456 = (self.probe / "p456k11a.out").read_text(encoding="utf-8")
        self.assertIn("LATE_COMMIT=revoked", out456)
        self.assertIn("cmp-verified post-stop", out456)
        # CALL ORDER: the stop (with its embedded commit) precedes the
        # migration copy and its cmp.
        log = self.stubs_log()
        stop_old = self.log_line(log, "systemctl stop oldregistry.service")
        cp_line = self.log_line(log, "cp -a /home/otheruser/")
        cmp_line = self.log_line(log, "cmp -s /home/otheruser/")
        self.assertLess(stop_old, cp_line,
                        "the migration copy must run AFTER the old service stopped")
        self.assertLess(cp_line, cmp_line, "the copy must be cmp-verified")
        # DATABASE CONTENT: source and target are both at the committed
        # revision — the new service's default database (main.rs registry_db_path)
        # is the TARGET, so it must not be one revision behind.
        self.assertEqual(self._db_rows(db), [("revoked", 2)])
        self.assertEqual(self._db_rows(self.probe / "opt-ak" / "data" / "registry.db"),
                         [("revoked", 2)],
                         "the target database must include the late commit")

    def test_k12n1_key_chmod_failure_enters_recovery(self):
        # K12-N1: ONLY the private-key chmod fails (everything else normal),
        # on an upgrade model where the key EXISTS. The old 'A && B' form
        # inside do_switchover swallowed the failure (INSTALL_RC=0, new
        # binary enabled, no recovery); now it returns 1 into the unified
        # recovery: nonzero install result, services restored and running
        # the OLD image.
        rc, out = self._run_k10_install(
            "", "1", "p456k12a",
            run_env={"FAIL_KEY_CHMOD": "1"},
            models="make_upgrade_model" + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=5", out,
                      "a swallowed key-chmod failure must not exit success")
        out456 = (self.probe / "p456k12a.out").read_text(encoding="utf-8")
        self.assertIn("chmod stub: forced key permission failure", out456)
        self.assertIn("RECOVERY OK", out456)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "recovery must restore the old binary")
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active",
                             f"{unit} must be running again after the recovery")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD)

    def test_k12n1_key_chmod_success_control_proceeds(self):
        # The no-injection control: the same model with a working chmod
        # completes the upgrade normally.
        rc, out = self._run_k10_install(
            "", "1", "p456k12b",
            models="make_upgrade_model" + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB)
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_STUB)

    def test_k12n1_fresh_install_without_key_skips_chmod(self):
        # The key-ABSENT branch stays skipped: a fresh install (no key) never
        # runs the key chmod and completes even with the injection armed.
        rc, out = self._run_k10_install("", "0", "p456k12c",
                                        run_env={"FAIL_KEY_CHMOD": "1"})
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        log = self.stubs_log()
        self.assertNotIn("chmod 600", log,
                         "the key chmod must not run when no key exists")

    # -- K10-N1 (round 14): cwd BYTE FIDELITY -----------------------------------
    def test_k10n1_cwd_trailing_lf_refuses(self):
        # Reviewer fixture (a): the real cwd ends in an LF byte. Command
        # substitution strips it and rounds <= 13 confirmed the truncated
        # '/home/runtime-old/.ak-sending'; now the emitting tool's byte
        # count exposes the loss and phase1 refuses.
        raw = self._make_k13_cwd_fixture(b"/home/runtime-old\n")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a cwd that lost its trailing LF byte must refuse")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("lost trailing bytes", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    def test_k10n1_cwd_embedded_lf_refuses(self):
        # Reviewer fixture (b): the cwd contains an embedded LF; the
        # line-based report would truncate at it. Refuse.
        raw = self._make_k13_cwd_fixture(b"/home/runtime-old\nsuffix")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot carry faithfully", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    def test_k10n1_ordinary_cwd_control_still_selects(self):
        # The ordinary control: a clean cwd keeps selecting the runtime dir.
        raw = self._make_k13_cwd_fixture(b"/home/runtime-old")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-old/\.ak-sending$")

    # -- K13-N1: generated scripts must not re-interpret paths ------------------
    def test_k13n1_quoted_path_refuses_before_switchover(self):
        # Reviewer fixture (a): USERPROFILE=/home/oth'er'user. Rounds <= 13
        # baked the quote into 'DATA_SOURCE=...' — the executing shell
        # stripped it and copied from a DIFFERENT directory (install 0).
        # The decision guard now refuses the shell-significant path.
        self._make_k13_quoted_fixture("/home/oth" + chr(39) + "er" + chr(39) + "user")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/configured-new" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        # NOTE: USERPROFILE is non-empty here, so the cwd is not consulted;
        # phase1 passes and reports the verbatim quoted path.
        self.assertRegex(p1_out, r"(?m)^  runtime-id-dir: /home/oth" + re.escape(chr(39))
                         + r"er" + re.escape(chr(39)) + r"user/\.ak-sending$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k13q.sh")
        self.assertNotEqual(
            rc, 0,
            "a shell-significant source path must refuse in the decision, "
            "before any switchover: " + out)
        self.assertIn("shell-significant bytes this deploy cannot bake faithfully", out)
        self.assertNotIn("CHOSEN_", out)
        # Nothing migrated: no managed destination data was created.
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())

    def test_k13n1_literal_dollar_path_refuses(self):
        # Reviewer fixture (b): USERPROFILE=/home/$USER — the dollar would
        # be EXPANDED by the executing install shell. Same refusal.
        self._make_k13_quoted_fixture("/home/$USER")
        # The expanded decoy dir (deployment-env expansion target) holds
        # different data — exactly the wrong-source trap.
        decoy = self.probe / "home" / "expanded-user" / ".ak-sending"
        decoy.mkdir(parents=True, exist_ok=True)
        for name in ("registry.key", "registry.db"):
            (decoy / name).write_text(f"expanded-decoy-{name}", encoding="utf-8")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/configured-new" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k13d.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("shell-significant bytes this deploy cannot bake faithfully", out)
        self.assertNotIn("CHOSEN_", out)

    def test_k13n1_ordinary_paths_still_pass_end_to_end(self):
        # Control: an ordinary path completes the full chain and the
        # on-disk destination bytes are the true source's (the default
        # discovery model's HOME-based identity, key/db private-*-bytes).
        self.make_private_discovery_model()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k13c.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending", out)
        rc, out = self._run_k10_install(
            "/home/otheruser/.ak-sending", "0", "p456k13c")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "private-key-bytes")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "private-db-bytes")

    def test_k10n2_fresh_install_onto_existing_identity_refuses(self):
        # Repro (d): NO migration source expected (fresh install) and the
        # destination already holds key/db — a conflict the operator must
        # resolve; refusing, never silently reusing.
        self._make_destination_decoy()
        rc, out = self._run_k10_install("", "0", "p456k10d")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=5", out)
        out456 = (self.probe / "p456k10d.out").read_text(encoding="utf-8")
        self.assertIn("fresh install found an existing", out456)
        self.assertIn("cannot be silently replaced or reused", out456)
        self.assertFalse((self.probe / "opt-ak" / "ak-cli").exists())
        # Fresh stops are no-ops (units absent); the post-stop refusal still
        # completes the (empty) recovery and reports its outcome.
        self.assertIn("RECOVERY OK", out456)

    def test_k10n2_inplace_upgrade_keeps_own_data(self):
        # Control: REMOTE_DATA_PREOWNED=1 (the identified occupant's runtime
        # identity IS $REMOTE_DATA) — the destination data is its own; no
        # refusal, nothing copied over it.
        self._make_destination_decoy()
        rc, out = self._run_k10_install("", "1", "p456k10e")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "unrelated-target-key")

    # -- K10-N1 (round 15): the cwd byte evidence cannot fail open -------------
    def test_k10n1_second_cwd_read_failure_cannot_skip_validation(self):
        # Reviewer fixture: cwd bytes '/home/runtime-old\n' (trailing LF);
        # the FIRST read succeeds, a hypothetical SECOND (count) read fails.
        # Rounds <= 14 read twice and skipped validation when the count read
        # failed (wc -c 0 -> '-gt 0' guard passed). The single-read capture
        # makes the second read nonexistent: the tail-LF loss is detected
        # from the SAME tee'd bytes and phase1 refuses — the injected
        # second-read failure never fires.
        raw = self._make_k13_cwd_fixture(b"/home/runtime-old\x0a")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'CWD_FAIL_ON_SECOND=1 PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "the byte-fidelity check must refuse, never skip")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("shows lost trailing bytes", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        # Exactly ONE cwd read happened (the marker proves the first ran);
        # the injected second-read failure never fired because there is no
        # second read.
        log = self.stubs_log()
        self.assertNotIn("review-cwd-second-read-failed", log)
        self.assertTrue((self.probe / "cwd-read-once").exists())

    def test_k10n1_all_reads_succeed_control_still_refuses_tail_lf(self):
        # Control: the stable tail-LF input (no failure injection) still
        # refuses via the same single-read evidence.
        raw = self._make_k13_cwd_fixture(b"/home/runtime-old\x0a")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("shows lost trailing bytes", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    # -- K14-N1: an explicit RELATIVE --id-dir binds to the RUNNING cwd --------
    def _k14_full_chain(self, name: str):
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, f"driver-decision-{name}.sh")
        return p1_out, rc, out

    def test_k14n1_relative_id_dir_resolves_running_cwd(self):
        # Reviewer fixture: --id-dir state with running cwd /home/runtime-old.
        # The chain must select /home/runtime-old/state (runtime-real bytes)
        # — NEVER the deployment cwd's decoy — and the generated unit must
        # carry ABSOLUTE identity paths only.
        self._make_k14_relative_fixture("state")
        p1_out, rc, out = self._k14_full_chain("k14rel")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-old/state$")
        self.assertIn("readlink /proc/4242/cwd", self.stubs_log(),
                      "the RUNNING cwd must actually be queried")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/runtime-old/state", out)
        self.assertIn("CHOSEN_RW=/home/runtime-old/state", out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertIn("--id-dir /home/runtime-old/state", exec_line)
        self.assertNotIn("--id-dir state ", exec_line + " ")
        # Execute the real install with the CHOSEN (absolute) values baked,
        # exactly as real_run would from this decision's output.
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee '
            + 'REGISTRY_EXEC="' + chosen_exec + '" '
            + 'REG_RW=/home/runtime-old/state LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=/home/runtime-old/state CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 '
            + 'write_phase456 "$PROBE_DIR/p456k14rel.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456k14rel.sh" > "$PROBE_DIR/p456k14rel.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "runtime-real-registry.key",
                         "the TRUE running source's bytes must land")
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        self.assertIn("--id-dir /home/runtime-old/state", unit)
        self.assertNotRegex(unit, r"--id-dir state(\s|$)")
        self.assertIn("ReadWritePaths=/home/runtime-old/state", unit)

    def test_k14n1_absolute_id_dir_control_unchanged(self):
        # Control: the absolute argument selects the same true source.
        self._make_k14_relative_fixture("/home/runtime-old/state")
        p1_out, rc, out = self._k14_full_chain("k14abs")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-old/state$")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/runtime-old/state", out)

    def test_k14n1_relative_id_dir_unreadable_cwd_refuses(self):
        # Relative --id-dir but the running cwd cannot be read: refuse.
        self._make_k14_relative_fixture("state")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=unreadable" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("RELATIVE --id-dir", p1_out)
        self.assertIn("refusing before any switchover", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    # -- K10-N1 (round 16): ONE environ snapshot; no fail-open read --------
    def test_k10n1_environ_read_exactly_once(self):
        # Reviewer requirement (b): /proc/<PID>/environ is read EXACTLY
        # ONCE (the single tee-captured snapshot). The old design read it
        # four times.
        self._make_k15_env_fixture(tail_lf=False)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        self.assertIn("ENV_READ_1", self.stubs_log())
        self.assertNotIn("ENV_READ_2", self.stubs_log(),
                         "/proc/4242/environ must be read EXACTLY once")

    def test_k10n1_environ_verification_read_failure_refuses(self):
        # Reviewer fixture (a): real trailing-LF USERPROFILE; under the old
        # design ONLY the 4th (verification) read failed and the '-gt 0'
        # guard skipped the check — the whole chain succeeded off the
        # truncated prefix decoy. With the single-snapshot design there is
        # no verification read of /proc at all; the failing read (modeled
        # by ENV_READ_FAIL_FROM=1 failing the FIRST/ONLY read) refuses.
        self._make_k15_env_fixture(tail_lf=True)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'ENV_READ_FAIL_FROM=1 bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "any evidence unavailability must refuse, never skip")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("a partial snapshot is not evidence", p1_out)
        self.assertIn("reader rc=1", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        # And nothing migrated anywhere.
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())

    def test_k10n1_environ_tail_lf_stable_control_refuses(self):
        # Control (c): all reads succeed, stable trailing LF -> refuse via
        # the snapshot's byte count.
        self._make_k15_env_fixture(tail_lf=True)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("lost trailing bytes", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    # -- K15-N1: a relative --db-path binds to the RUNNING cwd ------------------
    def test_k15n1_relative_db_path_absolutized_in_unit(self):
        # Reviewer fixture: relative --db-path 'home/otheruser/.ak-sending/
        # registry.db' with running cwd /home/runtime-old. The generated
        # unit must carry the RESOLVED ABSOLUTE db object (and ReadWritePaths
        # covering it), never the cwd-dependent relative form.
        rel = "home/otheruser/.ak-sending/registry.db"
        self._make_k15_db_fixture(rel)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  runtime-cwd: /home/runtime-old$")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k15rel.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertIn("--db-path /home/runtime-old/" + rel, exec_line)
        rw_line = [l for l in out.splitlines() if l.startswith("CHOSEN_RW=")][0]
        self.assertIn("/home/runtime-old/home/otheruser/.ak-sending", rw_line,
                      "ReadWritePaths must cover the RESOLVED db object")
        # The real generated unit (CHOSEN values baked).
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + chosen_exec + '" ' + chr(10)
            + 'REG_RW="/home/runtime-old/home/otheruser/.ak-sending '
            + '/home/otheruser/.ak-sending" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=/home/otheruser/.ak-sending CLOSE_7100=0 '
            + 'REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456k15rel.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456k15rel.sh" > "$PROBE_DIR/p456k15rel.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        self.assertIn("--db-path /home/runtime-old/" + rel, unit)
        self.assertNotRegex(unit, r"--db-path " + re.escape(rel) + r"(\s|$)")
        self.assertIn("/home/runtime-old/home/otheruser/.ak-sending", unit)

    def test_k15n1_absolute_db_path_control_unchanged(self):
        # Control: the absolute db argument keeps the original object.
        rel = "home/otheruser/.ak-sending/registry.db"
        self._make_k15_db_fixture("/home/runtime-old/" + rel)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k15abs.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertIn("--db-path /home/runtime-old/" + rel, exec_line)

    def test_k15n1_relative_db_path_unreadable_cwd_refuses(self):
        # Relative db with an unreadable running cwd: refuse before any
        # switchover (the db object cannot be bound).
        rel = "home/otheruser/.ak-sending/registry.db"
        self._make_k15_db_fixture(rel)
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=unreadable" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("RELATIVE --db-path", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    # -- K10-N1 (round 17): the READER's rc is evidence -------------------------
    def test_k10n1_cat_partial_output_then_fail_refuses(self):
        # Reviewer fixture: cat emits the FIRST complete HOME record and
        # THEN exits 74 — tee succeeds, so the old snapshot check accepted
        # the INCOMPLETE file, the missing USERPROFILE record let the chain
        # fall back to HOME and migrate the decoy. PIPESTATUS[0] refuses.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'ENV_PARTIAL_FAIL=1 bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a partial snapshot from a failed reader must refuse")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("reader rc=74", p1_out)
        self.assertIn("a partial snapshot is not evidence", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        # Nothing was migrated and nothing copied.
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())
        self.assertNotIn("systemctl stop", self.stubs_log())

    def test_k10n1_cat_ordinary_control_still_succeeds(self):
        # The ordinary control on the SAME fixture: the true runtime-real
        # source is selected and migrated.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-real/\.ak-sending$")

    def test_k10n1_cwd_output_then_fail_refuses(self):
        # The same pipe shape on the cwd capture: readlink emits the full
        # value and THEN fails — accepted before, refused by PIPESTATUS[0].
        # Fixture: empty USERPROFILE (relative base) forces the cwd read.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "Environment=HOME=/home/otheruser",
                                "Environment=USERPROFILE="],
            environ=b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'CWD_OUTPUT_THEN_FAIL=1 bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a cwd read that emitted a value then failed must refuse")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("reader rc=1", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        self.assertIn("CWD_NONZERO_WITH_OUTPUT", self.stubs_log())

    def test_k10n1_grep_extraction_error_refuses_not_absent(self):
        # Reviewer fixture: USERPROFILE IS present in the snapshot, but its
        # extraction grep exits 2 (tool error, no stdout). The round-16
        # 'continue' read the ERROR as variable-absent -> HOME fallback ->
        # decoy migrated. The rc triage refuses instead.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EXTRACT_VAR=USERPROFILE bash "$PROBE_DIR/p1.sh" '
            + '> "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a grep tool error must refuse, never read as absent")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot determine the presence of USERPROFILE", p1_out)
        self.assertIn("refusing rather than treat an error as variable-absent", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())

    # -- K15-N1 (round 17): bracket filenames cannot silently no-op -------------
    def test_k15n1_bracket_db_filename_refuses(self):
        # 'db[1].sqlite' is a LEGAL filename whose brackets are PATTERN
        # syntax in ${var//.../} — the round-16 rewrite silently no-opped
        # while the log claimed success, leaving a cwd-dependent unit.
        # Policy (stated in the script comment): the whitelist discipline
        # applies to the db argument BEFORE any rewrite — bracket bytes are
        # not bake-safe, so this REFUSES.
        self._make_k17_db_fixture("home/otheruser/.ak-sending/db[1].sqlite")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)  # phase1 passes (brackets are env-faithful)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k17br.sh")
        self.assertNotEqual(
            rc, 0,
            "a bracket db argument must refuse in the decision: " + out)
        self.assertIn("cannot bake faithfully into the generated unit", out)
        self.assertNotIn("CHOSEN_", out)

    def test_k15n1_ordinary_relative_db_rewrite_verified(self):
        # Control: an ordinary whitelisted relative db still rewrites, and
        # the RESULT-verification confirms the absolute form landed.
        self._make_k17_db_fixture("state/registry.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k17ord.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertIn("--db-path /home/runtime-old/state/registry.db", exec_line)
        self.assertNotIn("--db-path state/registry.db", exec_line + " ")

    # -- K10-N1 (round 18): presence OK + extraction error must refuse -------
    def test_k10n1_extraction_error_after_presence_ok_refuses(self):
        # Reviewer fixture: the FIRST (presence) grep for USERPROFILE passes
        # (rc=0, variable present), the SECOND (value extraction) grep
        # exits 2 with no stdout. Rounds <= 17's '|| true' + empty-continue
        # read that as variable-absent -> HOME fallback -> decoy migrated.
        # The extraction rc triage refuses instead.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EXTRACT_VAR=USERPROFILE GREP_EXTRACT_FAIL_AT=2 '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "an extraction error after a successful presence check "
                      "must refuse, never fall through as absent")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("cannot determine the value of USERPROFILE", p1_out)
        self.assertIn("grep rc=2", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())
        log = self.stubs_log()
        self.assertIn("REVIEW_GREP_2_ERROR=2", log,
                      "the SECOND grep call really failed")
        # The first call really succeeded (the marker file only exists once
        # the stub ran it; only one ERROR marker is present).
        self.assertNotIn("REVIEW_GREP_1_ERROR=2", log)

    def test_k10n1_healthy_extraction_control(self):
        # The healthy control on the same fixture: the true runtime-real
        # source is selected end-to-end.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-real/\.ak-sending$")

    # -- K15-N1 (round 18): the token-in-id-dir split regression --------------
    def test_k15n1_token_inside_id_dir_not_corrupted(self):
        # Reviewer fixture: the identity dir itself contains the substring
        # '--db-path-archive'; the REAL --db-path argument is relative.
        # The round-17 first-occurrence substring split corrupted the
        # identity and left the real db relative; the token walk must
        # absolutize ONLY the real argument and leave the id-dir untouched.
        identity = "/home/otheruser/--db-path-archive/.ak-sending"
        self._make_k18_db_fixture(identity, "state/registry.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k18tok.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertIn("--id-dir " + identity, exec_line,
                      "the identity dir must remain byte-identical")
        self.assertEqual(self._db_arg_of(exec_line),
                         "/home/runtime-old/state/registry.db",
                         "the REAL --db-path argument must be the resolved "
                         "absolute object")
        # The real generated unit with the CHOSEN values baked.
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + chosen_exec + '" ' + chr(10)
            + 'REG_RW="' + identity + ' /home/runtime-old/state" '
            + 'LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=' + identity + ' CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456k18tok.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456k18tok.sh" > "$PROBE_DIR/p456k18tok.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        unit_exec = [l for l in unit.splitlines() if l.startswith("ExecStart=")][0]
        self.assertIn("--id-dir " + identity, unit)
        self.assertEqual(self._db_arg_of(unit_exec),
                         "/home/runtime-old/state/registry.db")

    def test_k15n1_ordinary_and_bracket_controls_unchanged(self):
        # Ordinary relative db still rewrites; bracket still refuses;
        # absolute unchanged (round-17 behavior preserved).
        self._make_k18_db_fixture("/home/otheruser/.ak-sending", "state/registry.db")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k18ord.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertEqual(self._db_arg_of(exec_line),
                         "/home/runtime-old/state/registry.db")

    # -- K10-N1 (round 19): the byte-evidence PRODUCER's rc is evidence -----
    def test_k10n1_byte_evidence_partial_output_then_error_refuses(self):
        # Reviewer fixture: the byte-evidence grep emits the full match
        # minus its last two bytes (still one LF) and exits 2. wc's count
        # '-1' matched the truncated bash length, so rounds <= 18 passed the
        # check and the whole chain copied the prefix decoy. The producer-rc
        # guard refuses before the count is consumed.
        self._make_k19_env_fixture()
        rc, out = self._run_p1_with_evidence_mode("partial")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a byte-evidence grep that errored after partial output "
                      "must refuse, never pass on the partial count")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("byte-evidence producer for USERPROFILE of", p1_out)
        self.assertIn("grep rc=2", p1_out)
        self.assertIn("partial evidence is not evidence", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key").exists())
        self.assertIn("REVIEW_BYTE_GREP_PARTIAL_ERROR=2", self.stubs_log())

    def test_k10n1_byte_evidence_complete_output_then_error_refuses(self):
        # rc!=0 with the FULL bytes on stdout must also refuse (a healthy
        # count is not proof when the producer errored).
        self._make_k19_env_fixture()
        rc, out = self._run_p1_with_evidence_mode("complete")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("byte-evidence producer for USERPROFILE of", p1_out)
        self.assertIn("REVIEW_BYTE_GREP_COMPLETE_ERROR=2", self.stubs_log())

    def test_k10n1_byte_evidence_no_output_error_refuses(self):
        self._make_k19_env_fixture()
        rc, out = self._run_p1_with_evidence_mode("none")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("byte-evidence producer for USERPROFILE of", p1_out)
        self.assertIn("REVIEW_BYTE_GREP_EMPTY_ERROR=2", self.stubs_log())

    def test_k10n1_byte_evidence_healthy_tail_lf_control_refuses(self):
        # Healthy producer, the same two-LF snapshot: the count mismatch is
        # detected (the byte count exceeds the truncated bash length) and
        # phase1 refuses — this input must NEVER succeed.
        self._make_k19_env_fixture()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "a genuinely LF-terminated runtime value must refuse")
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertIn("lost trailing bytes", p1_out)
        self.assertNotIn("runtime-id-dir:", p1_out)

    def test_k10n1_byte_evidence_healthy_ordinary_control(self):
        # Ordinary healthy value: the chain succeeds on the true source.
        self._make_k16_env_fixture(b"/home/runtime-real")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-real/\.ak-sending$")

    # -- K15-N1 (round 19): the equals spelling must complete the chain ------
    def test_k15n1_relative_equals_db_form_completes_chain(self):
        # Reviewer fixture: '--db-path=state/registry.db' (equals form).
        # The round-18 verification accepted only a bare token and falsely
        # refused the correctly rewritten '--db-path=<abs>' form.
        identity = "/home/otheruser/.ak-sending"
        args = "--port 17200 --id-dir " + identity + " --db-path=state/registry.db"
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        live = self.probe / identity.lstrip("/")
        live.mkdir(parents=True, exist_ok=True)
        (live / "registry.key").write_text("synthetic-runtime-key", encoding="utf-8")
        (live / "registry.db").write_text("synthetic-identity-default-db",
                                         encoding="utf-8")
        import sqlite3
        for obj, marker in (("/home/runtime-old/state/registry.db", "runtime-real"),
                            ("/state/registry.db", "new-cwd-decoy")):
            path = self.probe / obj.lstrip("/")
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists():
                path.unlink()
            con = sqlite3.connect(path)
            con.execute("CREATE TABLE review_marker(value TEXT)")
            con.execute("INSERT INTO review_marker VALUES (?)", (marker,))
            con.commit()
            con.close()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k19eq.sh")
        self.assertEqual(
            rc, 0,
            "the rewritten equals form must PASS the option-bound verification "
            "(round-18 falsely refused it): " + out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        self.assertEqual(self._db_arg_of(chosen_exec),
                         "/home/runtime-old/state/registry.db")
        self.assertIn("--id-dir " + identity, exec_line)
        # The real generated unit with the CHOSEN values baked.
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + chosen_exec + '" ' + chr(10)
            + 'REG_RW="' + identity + ' /home/runtime-old/state" '
            + 'LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=' + identity + ' CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456k19eq.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456k19eq.sh" > "$PROBE_DIR/p456k19eq.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        unit_exec = [l for l in unit.splitlines() if l.startswith("ExecStart=")][0]
        self.assertEqual(self._db_arg_of(unit_exec),
                         "/home/runtime-old/state/registry.db",
                         "the unit must carry the resolved absolute db object")
        self.assertIn("--id-dir " + identity, unit)

    def test_k15n1_absolute_equals_db_control(self):
        # Absolute equals form: untouched (no relative rewrite involved).
        identity = "/home/otheruser/.ak-sending"
        args = ("--port 17200 --id-dir " + identity
                + " --db-path=/home/runtime-old/state/registry.db")
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k19abs.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertEqual(self._db_arg_of(exec_line[len("CHOSEN_EXEC="):]),
                         "/home/runtime-old/state/registry.db")

    # -- K19-N1 (round 20): a literal '--db-path' filename is a VALUE -------
    def test_k19n1_literal_db_filename_resolves_token_bound(self):
        # The reviewer's exact fixture: the equals value is the literal
        # filename '--db-path'. The old greedy sed treated the value as an
        # option start and bound '--port'; the token-bound extraction keeps
        # the literal, the whitelist passes ('-' only), and the chain reads
        # the REAL object at /home/runtime-old/--db-path.
        self._make_k20_literal_fixture()
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1.sh" > "$PROBE_DIR/p1.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out, "driver-decision-k20lit.sh")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        self.assertEqual(self._db_arg_of(chosen_exec),
                         "/home/runtime-old/--db-path",
                         "the literal filename must resolve against the running cwd")
        self.assertNotIn("--db-path=/home/runtime-old/--port", chosen_exec,
                         "'--port' must never be bound to --db-path")
        self.assertIn("--id-dir /home/otheruser/.ak-sending", exec_line)
        # Execute the real install with the CHOSEN values baked and prove
        # the object the new unit points at is the REAL one.
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + chosen_exec + '" ' + chr(10)
            + 'REG_RW="/home/otheruser/.ak-sending /home/runtime-old" '
            + 'LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=/home/otheruser/.ak-sending CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456k20lit.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456k20lit.sh" > "$PROBE_DIR/p456k20lit.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.generate_and_run_p456(body)
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        unit_exec = [l for l in unit.splitlines() if l.startswith("ExecStart=")][0]
        self.assertEqual(self._db_arg_of(unit_exec),
                         "/home/runtime-old/--db-path")
        import sqlite3
        obj = self.probe / "home/runtime-old/--db-path"
        con = sqlite3.connect(obj)
        marker = con.execute("SELECT value FROM review_marker").fetchone()[0]
        con.close()
        self.assertEqual(marker, "runtime-real",
                         "the resolved object is the REAL runtime database")

    # -- K19-N2 (round 20): dangling and duplicate db bindings refuse ---------
    def test_k19n2_trailing_dangling_option_refuses_pre_stop(self):
        # Configured unit edited to '--db-path=state/registry.db --db-path'
        # (running argv stays the legal single binding). The dangling option
        # has NO value — the real CLI errors "a value is required for
        # '--db-path <DB_PATH>'". The decision must refuse before any stop.
        running = "--port 17200 --id-dir /home/otheruser/.ak-sending --db-path=state/registry.db"
        configured = running + " --db-path"
        rc, out = self._k20_decision_of(configured, running,
                                         "driver-decision-k20dang.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("dangling '--db-path' option without a value", out)
        self.assertIn("a value is required", out)
        self.assertNotIn("CHOSEN_", out)
        self.assertNotIn("systemctl stop", self.stubs_log())

    def test_k19n2_duplicate_options_refuse_pre_stop(self):
        # Three duplicate shapes: two standalone, standalone+equals, two
        # equals. The real CLI errors "cannot be used multiple times".
        running = "--port 17200 --id-dir /home/otheruser/.ak-sending --db-path=state/registry.db"
        duplicates = [
            ("--port 17200 --id-dir /home/otheruser/.ak-sending --db-path state/registry.db --db-path state/registry.db"),
            (running + " --db-path=state/registry.db"),
            ("--port 17200 --id-dir /home/otheruser/.ak-sending --db-path=state/registry.db --db-path=other.db"),
        ]
        for i, configured in enumerate(duplicates):
            with self.subTest(shape=i):
                rc, out = self._k20_decision_of(
                    configured, running, f"driver-decision-k20dup{i}.sh")
                self.assertNotEqual(rc, 0, out)
                self.assertIn("cannot be used multiple times", out)
                self.assertIn("'--db-path' bindings", out)
                self.assertNotIn("CHOSEN_", out)
                self.assertNotIn("systemctl stop", self.stubs_log())

    # -- K20-N1 (round 21): the token walks must not run pathname expansion --
    def test_k20n1_glob_target_in_decision_cwd_refuses_pre_stop(self):
        # The reviewer's P1 fixture: unit AND running argv carry the literal
        # bracket filename 'db[1].sqlite'; a plain matching file
        # ('db1.sqlite' / '--db-path=db1.sqlite') sits in the DECISION's cwd
        # while phase1 runs from an isolated cwd. Round 20's unquoted walk
        # expanded the bracket filename against that file and retargeted the
        # database object; the frozen-token walk must refuse with the
        # whitelist message naming the UNEXPANDED bytes, either way round.
        for i, (sep, label) in enumerate(((" ", "space"), ("=", "equals"))):
            with self.subTest(spelling=label):
                self.setUp()
                self._make_k20_glob_fixture(sep)
                self._drop_glob_match(sep)
                rc, out = self._run_p1_isolated("p1k20g%d.sh" % i,
                                                "p1k20g%d.out" % i)
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=0", out)
                p1_out = (self.probe / ("p1k20g%d.out" % i)).read_text(
                    encoding="utf-8")
                rc, out = self.run_decision_and_config(
                    p1_out, "driver-decision-k20g%d.sh" % i)
                self.assertNotEqual(rc, 0, out)
                self.assertIn("contains bytes this deploy cannot bake "
                              "faithfully into the generated unit", out)
                self.assertIn("db[1].sqlite", out,
                              "the refusal must name the UNEXPANDED bracket "
                              "filename — the walk must not glob")
                self.assertNotIn("CHOSEN_", out)
                self.assertNotIn("systemctl stop", self.stubs_log())
                self.assertFalse(
                    (self.probe
                     / "etc/systemd/system/ak-registry-server.service").exists(),
                    "no unit may be generated from a globbed db path")
                self.doCleanups()

    def test_k20n1_bracket_db_arg_refuses_without_glob_target(self):
        # Control: with NO matching file in the cwd the bracket filename has
        # no glob expansion — the refusal must be IDENTICAL (the outcome may
        # not depend on the executing directory's contents).
        self._make_k20_glob_fixture(" ")
        rc, out = self._run_p1_isolated("p1k20gm.sh", "p1k20gm.out")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k20gm.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k20gm.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("contains bytes this deploy cannot bake faithfully "
                      "into the generated unit", out)
        self.assertIn("db[1].sqlite", out)
        self.assertNotIn("CHOSEN_", out)
        self.assertNotIn("systemctl stop", self.stubs_log())

    def test_k20n1_ordinary_db_arg_walks_identically_reads_runtime_real(self):
        # Ordinary-token control: a plain 'db1.sqlite' value walks through
        # the noglob tokenizer IDENTICALLY (word splitting is unchanged).
        # The full chain resolves it against the RUNNING cwd, stops the old
        # service, installs, and the object the new unit points at is the
        # REAL runtime database.
        self._make_k20_glob_fixture(" ", value="db1.sqlite")
        self._make_oldregistry_running()
        rc, out = self._run_p1_isolated("p1k20ord.sh", "p1k20ord.out")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k20ord.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k20ord.sh")
        self.assertEqual(rc, 0, out)
        chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
        self.assertEqual(self._db_arg_of(chosen_exec),
                         "/home/runtime-old/db1.sqlite")
        rc, out = self._install_with_chosen(
            chosen_exec, chosen_rw, old_registry, "p456k20ord")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        self.assertIn("systemctl stop", self.stubs_log(),
                      "the ordinary control still migrates the old service")
        p456_out = (self.probe / "p456k20ord.out").read_text(encoding="utf-8")
        self.assertIn("oldregistry:exists:enabled:active", p456_out)
        unit = (self.probe
                / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        unit_exec = [l for l in unit.splitlines()
                     if l.startswith("ExecStart=")][0]
        self.assertEqual(self._db_arg_of(unit_exec),
                         "/home/runtime-old/db1.sqlite")
        import sqlite3
        con = sqlite3.connect(self.probe / "home/runtime-old/db1.sqlite")
        marker = con.execute("SELECT value FROM review_marker").fetchone()[0]
        con.close()
        self.assertEqual(marker, "runtime-real",
                         "the resolved object is the REAL runtime database")

    # -- K20-N2 (round 21): CLI-invalid db VALUE SHAPES refuse pre-stop -------
    def test_k20n2_empty_equals_db_value_refuses_pre_stop(self):
        # The reviewer's fixture: the RUNNING argv is the legal default-db
        # form; the configured unit adds an explicit EMPTY '--db-path='.
        # The real CLI (reviewer-verified at 110bb34) refuses it with
        # "a value is required for '--db-path <DB_PATH>' but none was
        # supplied". Round 20 treated the empty value as "option absent",
        # compared it as the effective DEFAULT db, PASSED, and stopped the
        # service; the generated unit kept the shape the CLI rejects. Both
        # phase1 and the decision must refuse BEFORE any stop.
        running = "--port 17200 --id-dir /home/otheruser/.ak-sending"
        configured = running + " --db-path="
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + configured,
            cmdline_extra=running,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        rc, out = self._run_p1_isolated("p1k20e.sh", "p1k20e.out")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "phase1 itself must refuse the CLI-invalid shape "
                      "(the empty value is not the option-absent default)")
        p1_out = (self.probe / "p1k20e.out").read_text(encoding="utf-8")
        self.assertIn("an explicit EMPTY --db-path value", p1_out)
        self.assertIn("a value is required for '--db-path <DB_PATH>' "
                      "but none was supplied", p1_out)
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k20e.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("an explicit EMPTY --db-path value", out)
        self.assertNotIn("CHOSEN_", out)
        self.assertNotIn("systemctl stop", self.stubs_log())
        self.assertFalse(
            (self.probe
             / "etc/systemd/system/ak-registry-server.service").exists(),
            "no unit carrying a CLI-rejected argument may be generated")

    def test_k20n2_space_form_option_value_refuses_pre_stop(self):
        # The reviewer's supplementary shape: the RUNNING argv legally binds
        # the literal '--port' filename via the EQUALS form (K19-N1 control
        # on the running side); the configured unit re-spells it as the
        # SPACE form '--db-path --port 17200'. Round 20 consumed the OPTION
        # token '--port' as the db filename and left '17200' an unexpected
        # positional; the real CLI reports the DB missing-value error and
        # never starts. The configured shape must refuse pre-stop.
        identity = "/home/otheruser/.ak-sending"
        running = "--id-dir " + identity + " --db-path=--port --port 17200"
        configured = "--id-dir " + identity + " --db-path --port 17200"
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + configured,
            cmdline_extra=running,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        rc, out = self._run_p1_isolated("p1k20o.sh", "p1k20o.out")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=3", out,
                      "phase1 itself must refuse the option-shaped binding")
        p1_out = (self.probe / "p1k20o.out").read_text(encoding="utf-8")
        self.assertIn("space-form --db-path value is option-shaped", p1_out)
        self.assertIn("never consume an option as the filename", p1_out)
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k20o.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("space-form --db-path value is option-shaped", out)
        self.assertNotIn("CHOSEN_", out)
        self.assertNotIn("systemctl stop", self.stubs_log())
        self.assertFalse(
            (self.probe
             / "etc/systemd/system/ak-registry-server.service").exists())

    def test_k20_decisions_walk_refuses_cli_invalid_shapes(self):
        # Direct proof that the DECISIONS walk's own guards fire (phase1's
        # grammar gate is bypassed by feeding the report straight in): each
        # CLI-invalid shape refuses with the CLI contract cited, and the
        # noglob property holds at the decisions level too.
        base_log = (
            "sudo: passwordless-ok\n"
            "unit ak-registry-server: present\n"
            "  ExecStart=/opt/old/ak-cli registry-server {args}\n"
            "unit ak-stun-server: absent\n"
            "port 17200/tcp: free\n"
            "port 17101/udp: free\n"
            "  runtime-id-dir: /home/otheruser/.ak-sending\n"
            "  runtime-cwd: /home/runtime-old\n"
        )
        identity = "/home/otheruser/.ak-sending"
        shapes = [
            ("empty-equals",
             "--id-dir " + identity + " --port 17200 --db-path=",
             ["an explicit EMPTY --db-path value",
              "a value is required for '--db-path <DB_PATH>' but none was supplied"]),
            ("option-value",
             "--id-dir " + identity + " --db-path --port 17200",
             ["OPTION-SHAPED token as the space-form --db-path value",
              "missing-value error"]),
            ("bracket-glob-match",
             "--id-dir " + identity + " --port 17200 --db-path db[1].sqlite",
             ["contains bytes this deploy cannot bake faithfully into "
              "the generated unit", "db[1].sqlite"]),
        ]
        for label, args, needles in shapes:
            with self.subTest(shape=label):
                self.setUp()
                if label == "bracket-glob-match":
                    self._drop_glob_match(" ")
                rc, out = self.run_decision_and_config(
                    base_log.format(args=args),
                    "driver-decision-k20syn.sh")
                self.assertNotEqual(rc, 0, out)
                for needle in needles:
                    self.assertIn(needle, out)
                self.assertNotIn("CHOSEN_", out)
                self.assertNotIn("systemctl stop", self.stubs_log())
                self.doCleanups()

    # -- K21-N1 (round 22): token-bound identity extraction -------------------
    def test_k21n1_literal_option_shaped_id_dir_binds_verbatim(self):
        # The reviewer's P1 fixture: the legal id path ENDS in the literal
        # '--id-dir'. The greedy sed bound the FOLLOWING '--port' as the
        # identity (resolving the decoy /home/runtime-old/--port against the
        # running cwd) and the pattern rewrite then corrupted the args. The
        # token-bound chain must bind the path VERBATIM, copy the REAL
        # directory's key/db, and generate args the CLI accepts (the
        # reviewer's real-CLI probe: original argv parses, fails only at
        # identity loading; the corrupted argv gave rc 2 'unexpected
        # argument /home/runtime-old/--port found').
        for i, (sep, label) in enumerate(((" ", "space"), ("=", "equals"))):
            with self.subTest(spelling=label):
                self.setUp()
                self._make_k21_id_fixture(sep)
                script = "p1k21i%d.sh" % i
                outname = "p1k21i%d.out" % i
                rc, out = self.generate_and_run_p456(
                    'write_phase1 "$PROBE_DIR/' + script + '" '
                    '|| { echo GEN_FAILED; exit 90; }' + chr(10)
                    + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
                    + 'bash "$PROBE_DIR/' + script + '" '
                    + '> "$PROBE_DIR/' + outname + '" 2>&1; '
                    + 'echo "RUN_RC=$?"' + chr(10))
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=0", out)
                p1_out = (self.probe / outname).read_text(encoding="utf-8")
                self.assertIn("  runtime-id-dir: /home/otheruser/--id-dir",
                              p1_out,
                              "the runtime identity must bind the path "
                              "VERBATIM, never the following '--port'")
                self.assertNotIn("runtime-id-dir: /home/runtime-old/--port",
                                 p1_out)
                rc, out = self.run_decision_and_config(
                    p1_out, "driver-decision-k21i-decision%d.sh" % i)
                self.assertEqual(rc, 0, out)
                chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
                self.assertIn("--id-dir" + sep + "/home/otheruser/--id-dir",
                              chosen_exec)
                self.assertIn("--port 17200", chosen_exec)
                self.assertNotIn("/home/runtime-old/--port", chosen_exec)
                self.assertEqual(chosen_rw, "/home/otheruser/--id-dir")
                rc, out = self._install_with_chosen(
                    chosen_exec, chosen_rw, old_registry,
                    "p456k21i%d" % i,
                    data_source="/home/otheruser/--id-dir")
                self.assertEqual(rc, 0, out)
                self.assertIn("INSTALL_RC=0", out)
                self.assertIn("systemctl stop", self.stubs_log())
                p456_out = (self.probe / ("p456k21i%d.out" % i)).read_text(
                    encoding="utf-8")
                self.assertIn("oldregistry:exists:enabled:active", p456_out)
                dest = self.probe / "opt-ak" / "data"
                self.assertEqual(
                    (dest / "registry.key").read_text(encoding="utf-8"),
                    "runtime-real-key",
                    "the REAL directory's key must be migrated")
                self.assertEqual(
                    (dest / "registry.db").read_text(encoding="utf-8"),
                    "runtime-real-db")
                unit = (self.probe
                        / "etc/systemd/system/ak-registry-server.service") \
                    .read_text(encoding="utf-8")
                self.assertIn(
                    "--id-dir" + sep + "/home/otheruser/--id-dir --port 17200",
                    [l for l in unit.splitlines()
                     if l.startswith("ExecStart=")][0],
                    "the generated argv must stay CLI-valid (literal path "
                    "component preserved, real port option intact)")
                self.assertIn("ReadWritePaths=/home/otheruser/--id-dir", unit)
                decoy = self.probe / "home/runtime-old/--port"
                self.assertEqual(
                    (decoy / "registry.key").read_text(encoding="utf-8"),
                    "substring-decoy-key",
                    "the misbind decoy must never be touched")
                self.doCleanups()

    def test_k21n1_ordinary_id_dir_control_migrates_real(self):
        # Ordinary-path control: no option-shaped bytes anywhere — the
        # token-bound extraction behaves identically to the old sed and the
        # full chain migrates the REAL identity.
        self._make_k21_id_fixture(" ", idpath="/home/otheruser/identity")
        rc, out = self.generate_and_run_p456(
            'write_phase1 "$PROBE_DIR/p1k21ord.sh" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1k21ord.sh" '
            + '> "$PROBE_DIR/p1k21ord.out" 2>&1; echo "RUN_RC=$?"' + chr(10))
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k21ord.out").read_text(encoding="utf-8")
        self.assertIn("  runtime-id-dir: /home/otheruser/identity", p1_out)
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k21ord.sh")
        self.assertEqual(rc, 0, out)
        chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
        self.assertIn("--id-dir /home/otheruser/identity", chosen_exec)
        rc, out = self._install_with_chosen(
            chosen_exec, chosen_rw, old_registry, "p456k21ord",
            data_source="/home/otheruser/identity")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "runtime-real-key")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "runtime-real-db")

    def test_k21n1_id_dir_grammar_refuses_pre_stop(self):
        # The CLI grammar gates mirrored for --id-dir (error strings
        # verified against the real git-714099a CLI): explicit empty
        # equals suffix, option-shaped space-form value, dangling trailing
        # option ("a value is required for '--id-dir <ID_DIR>' but none was
        # supplied") and duplicate bindings ("the argument '--id-dir
        # <ID_DIR>' cannot be used multiple times"). All refuse in phase1,
        # before any stop.
        identity = "/home/otheruser/identity"
        running = "--port 17200 --id-dir " + identity
        shapes = [
            ("empty", "--port 17200 --id-dir=",
             ["an explicit EMPTY --id-dir value",
              "a value is required for '--id-dir <ID_DIR>' but none was "
              "supplied"]),
            ("option-value", "--port 17200 --id-dir --port 17200",
             ["space-form --id-dir value is option-shaped"]),
            ("dangling", "--port 17200 --id-dir",
             ["dangling '--id-dir' option without a value",
              "a value is required for '--id-dir <ID_DIR>'"]),
            ("duplicate", "--port 17200 --id-dir " + identity
             + " --id-dir " + identity,
             ["'--id-dir' bindings", "cannot be used multiple times"]),
        ]
        for label, configured, needles in shapes:
            with self.subTest(shape=label):
                self.setUp()
                self.make_private_discovery_model(
                    main_exec="/opt/ak-sending/ak-cli registry-server "
                    + configured,
                    cmdline_extra=running,
                    unit_service_lines=["User=otheruser",
                                        "WorkingDirectory=/home/runtime-old"],
                    environ=b"HOME=/home/otheruser\0"
                            b"USERPROFILE=/home/otheruser\0")
                rc, out = self.generate_and_run_p456(
                    'write_phase1 "$PROBE_DIR/p1k21g.sh" '
                    '|| { echo GEN_FAILED; exit 90; }' + chr(10)
                    + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
                    + 'bash "$PROBE_DIR/p1k21g.sh" '
                    + '> "$PROBE_DIR/p1k21g.out" 2>&1; echo "RUN_RC=$?"'
                    + chr(10))
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=3", out,
                              "phase1 must refuse the CLI-invalid --id-dir "
                              "shape pre-stop")
                p1_out = (self.probe / "p1k21g.out").read_text(encoding="utf-8")
                for needle in needles:
                    self.assertIn(needle, p1_out)
                self.assertNotIn("systemctl stop", self.stubs_log())
                self.assertFalse(
                    (self.probe
                     / "etc/systemd/system/ak-registry-server.service").exists())
                self.doCleanups()

    # -- K21-N2 (round 22): /home candidates iterate LINES --------------------
    def test_k21n2_spaced_home_is_one_whole_candidate(self):
        # The reviewer's fixture: a legal home named 'other user' plus the
        # split-prefix decoy directly under /home/other. The whole
        # '/home/other user/.ak-sending' is ONE candidate; the '/home/other'
        # fragment NEVER appears; the un-bakeable spaced name then refuses
        # pre-stop at the K13-N1 whitelist instead of silently migrating
        # the prefix decoy.
        self._make_k21_home_fixture("other user", decoy=True)
        rc, out = self._run_p1_homes("p1k21h.sh", "p1k21h.out", "other user")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k21h.out").read_text(encoding="utf-8")
        self.assertIn(
            "registry-data: /home/other user/.ak-sending (key:yes db:yes)",
            p1_out, "the spaced directory is ONE whole candidate")
        self.assertNotIn("registry-data: /home/other (", p1_out,
                         "the split-prefix fragment must never be a "
                         "candidate")
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k21h.sh")
        self.assertNotEqual(
            rc, 0,
            "the spaced name cannot be baked; the deploy must refuse "
            "pre-stop: " + out)
        self.assertIn("contains shell-significant bytes this deploy cannot "
                      "bake faithfully", out)
        self.assertNotIn("CHOSEN_DATA_SOURCE=/home/other" + chr(10), out)
        self.assertNotIn("systemctl enable", self.stubs_log())
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key")
                         .exists(), "nothing may be copied from the decoy")
        decoy = self.probe / "home" / "other" / "registry.key"
        self.assertEqual(decoy.read_text(encoding="utf-8"),
                         "synthetic-split-decoy-key",
                         "the prefix decoy must stay untouched")
        real = (self.probe / "home" / "other user" / ".ak-sending"
                / "registry.key")
        self.assertEqual(real.read_text(encoding="utf-8"),
                         "synthetic-existing-key",
                         "the real spaced candidate stays in place")

    def test_k21n2_spaced_plus_ordinary_candidates_refuse(self):
        # The bypass closure: a REAL spaced candidate plus an ordinary one
        # must BOTH be reported, so the no-occupant multi-candidate
        # refusal fires (the word-splitting loop dropped the spaced one and
        # silently migrated the other).
        self._make_k21_home_fixture("other user\notheruser", decoy=True)
        rc, out = self._run_p1_homes("p1k21p.sh", "p1k21p.out",
                                     "other user\notheruser")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k21p.out").read_text(encoding="utf-8")
        self.assertIn("registry-data: /home/other user/.ak-sending "
                      "(key:yes db:yes)", p1_out)
        self.assertIn("registry-data: /home/otheruser/.ak-sending "
                      "(key:yes db:yes)", p1_out)
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k21p.sh")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("multiple identity directories", out)
        self.assertNotIn("systemctl enable", self.stubs_log())
        self.assertFalse((self.probe / "opt-ak" / "data" / "registry.key")
                         .exists())

    def test_k21n2_ordinary_home_still_migrates(self):
        # Ordinary control: a single plain-named home enumerates and
        # migrates exactly as before.
        self._make_k21_home_fixture("otheruser", decoy=False)
        rc, out = self._run_p1_homes("p1k21o.sh", "p1k21o.out", "otheruser")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1k21o.out").read_text(encoding="utf-8")
        self.assertIn("registry-data: /home/otheruser/.ak-sending "
                      "(key:yes db:yes)", p1_out)
        rc, out = self.run_decision_and_config(p1_out,
                                               "driver-decision-k21o.sh")
        self.assertEqual(rc, 0, out)
        chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
        rc, out = self._install_with_chosen(
            chosen_exec, chosen_rw, old_registry, "p456k21o")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        self.assertIn("systemctl enable --now", self.stubs_log())
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "synthetic-existing-key")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "synthetic-existing-db")

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
        self.assertRegex(p1_out, r"(?m)^  cmdline: /opt/ak-sending/ak-cli registry-server "
                         r"--port 17200 --id-dir /home/otheruser/\.ak-sending *$")
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
            run_env={"SYSTEMCTL_CAT_MSG": "No files found for %s."})
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

    # -- K5-review contrasts: the round-5 candidate (46473fc) exhibits the
    #    three round-6 defects under the same fixtures; the current tree
    #    passes them (fail-to-pass witnesses).
    BUS_ENOENT = "Failed to connect to bus: No such file or directory"

    def test_k3n1_round5_candidate_bus_enoent_silent_install_success(self):
        # Reviewer repro (b) against the round-5 candidate: only cat fails
        # with the standard bus diagnostic -> everything "absent", no stops,
        # install exits 0 with a new disk under still-running old processes.
        rc, out, p456_out = self.run_baseline_p456(
            PREV5_REV, self.P456_ENV_PREBUILT, "p456prev5",
            run_env={"SYSTEMCTL_CAT_MSG": self.BUS_ENOENT,
                     "SYSTEMCTL_CAT_FORCE_FAIL": "1"},
            models="make_upgrade_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-5 candidate silently succeeded (the defect)")
        self.assertEqual(p456_out.count("not present; nothing to stop"), 2, p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "new disk image installed")
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"), AK_CLI_OLD,
                         "the old process kept running the old image")

    def test_k2n1_round5_candidate_deactivating_recovery_ok_with_new_image(self):
        # Reviewer repro against the round-5 candidate: deactivating was
        # classified as stopped -> RECOVERY OK while the unit is still
        # deactivating with its MainPID alive on the NEW image.
        rc, out, p456_out = self.run_baseline_p456(
            PREV5_REV, self.P456_ENV_PREBUILT, "p456prev5",
            run_env={"SYSTEMCTL_FAIL_SPEC": "ak-stun-server.service:enable-now",
                     "SYSTEMCTL_START_STATE_UNIT": "ak-registry-server.service",
                     "SYSTEMCTL_START_STATE": "deactivating"},
            models="make_disabled_inactive_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        self.assertIn("RECOVERY OK", p456_out,
                      "the round-5 candidate claimed recovery success (the defect)")
        self.assertEqual(self.unit_state("ak-registry-server.service", "active"),
                         "deactivating")
        self.assertEqual(self.unit_state("ak-registry-server.service", "pid"), "5001")
        self.assertEqual(self.unit_running_bin("ak-registry-server.service"),
                         AK_CLI_STUB, "the NEW image kept running")

    def test_k2r3_round5_candidate_standard_vendor_unit_loses_id_dir(self):
        # Reviewer repro against the round-5 candidate: the standard-named
        # vendor unit's config never reaches the decision, and the real
        # config selection re-points the identity to the default dir.
        self.make_private_discovery_model(occupant="ak-registry-server.service",
                                          vendor=True)
        (self.probe / "usr" / "lib" / "systemd" / "system"
         / "ak-registry-server.service").write_text(
            "[Unit]\nDescription=vendor-dir standard registry\n\n[Service]\n"
            "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /var/lib/ak-legacy\n\n[Install]\nWantedBy=multi-user.target\n",
            encoding="utf-8", newline="\n")
        text = self.baseline_text(PREV5_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev5.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1prev5.sh" > "$PROBE_DIR/p1prev5.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV5_REV) + BOX_MODELS, body,
                                  name="driver-prev5-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev5.out").read_text(encoding="utf-8")
        self.assertIn("unit ak-registry-server: absent", p1_out,
                      "the round-5 candidate missed the vendor-dir standard unit")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev5-decision.sh", text=text,
            origin=f"the {PREV5_REV} baseline")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertNotIn("/var/lib/ak-legacy", exec_line[0],
                         "the round-5 candidate dropped the legacy identity dir")
        self.assertNotIn("CHOSEN_RW=/var/lib/ak-legacy", out)

    # -- K6-review contrasts: the round-6 candidate (e8f811d) exhibits the
    #    two round-7 defects under the same fixtures; the current tree
    #    passes them (fail-to-pass witnesses).
    def test_k3n1_round6_candidate_deleted_files_silent_pass(self):
        # Reviewer repro against the round-6 candidate: unit files deleted
        # while both services keep running -> 'nothing to stop' x2, new disk
        # installed, install exits 0, original process untouched.
        rc, out, p456_out = self.run_baseline_p456(
            PREV6_REV, self.P456_ENV_PREBUILT, "p456prev6",
            models="make_deleted_unit_files_model\n")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-6 candidate silently succeeded (the defect)")
        self.assertEqual(p456_out.count("not present; nothing to stop"), 2, p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "new disk image installed")
        for unit in ("ak-registry-server.service", "ak-stun-server.service"):
            self.assertEqual(self.unit_state(unit, "active"), "active",
                             "the original processes kept running")
            self.assertEqual(self.unit_running_bin(unit), AK_CLI_OLD)

    def test_k2r3_round6_candidate_dropin_loses_legacy_dir(self):
        # Reviewer repro against the round-6 candidate: only the MAIN file
        # is read, so the drop-in's clearing override is ignored and the
        # real config selection picks /var/lib/ak-default.
        self.make_private_discovery_model(
            vendor=True,
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-default",
            dropin=["[Service]", "ExecStart=",
                    "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
                    "--id-dir /var/lib/ak-legacy"],
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        text = self.baseline_text(PREV6_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev6.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1prev6.sh" > "$PROBE_DIR/p1prev6.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV6_REV) + BOX_MODELS, body,
                                  name="driver-prev6-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev6.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*ak-default$",
                         "the round-6 candidate read only the main fragment")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev6-decision.sh", text=text,
            origin=f"the {PREV6_REV} baseline")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("/var/lib/ak-default", exec_line[0],
                      "the round-6 candidate chose the wrong identity dir (the defect)")
        self.assertIn("CHOSEN_RW=/var/lib/ak-default", out)
        self.assertNotIn("CHOSEN_RW=/var/lib/ak-legacy", out)

    # -- K7-review contrast: the round-7 candidate (bb1f673) fail-OPENED the
    #    cross-check when the cmdline read failed (and never compared
    #    --db-path); the current tree refuses both inputs.
    def test_k2r3_round7_candidate_cmdline_read_failure_selects_wrong_dir(self):
        # Reviewer repro against the round-7 candidate: only the cmdline
        # read fails; the disagreeing disk config (ak-default) is chosen
        # through the REAL phase1 -> decision -> config chain.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /var/lib/ak-default",
            cmdline_extra="--port 17200 --id-dir /var/lib/ak-legacy")
        text = self.baseline_text(PREV7_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev7.sh" || { echo GEN_FAILED; exit 90; }\n'
            'CAT_FAIL_MATCH="/proc/4242/cmdline" bash "$PROBE_DIR/p1prev7.sh" '
            '> "$PROBE_DIR/p1prev7.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV7_REV) + BOX_MODELS, body,
                                  name="driver-prev7-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-7 candidate passed phase1 despite the failed read (the defect)")
        p1_out = (self.probe / "p1prev7.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  ExecStart=.*ak-default$")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev7-decision.sh", text=text,
            origin=f"the {PREV7_REV} baseline")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_RW=/var/lib/ak-default", out,
                      "the round-7 candidate selected the wrong identity dir (the defect)")

    def test_k2r3_round7_candidate_runtime_default_db_selects_new_db(self):
        # K8-review contrast (e): the reviewer's exact default-db fixture —
        # the running instance relies on the id-dir's default registry.db
        # while the unit config explicitly sets new.db. The round-7
        # candidate compared only EXPLICIT running flags, so both stages
        # passed and new.db was chosen; the current tree refuses.
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server --port 17200 "
                      "--id-dir /home/otheruser/.ak-sending "
                      "--db-path /home/otheruser/.ak-sending/new.db",
            cmdline_extra="--port 17200 --id-dir /home/otheruser/.ak-sending")
        text = self.baseline_text(PREV7_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev7b.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1prev7b.sh" > "$PROBE_DIR/p1prev7b.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV7_REV) + BOX_MODELS, body,
                                  name="driver-prev7b-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-7 candidate passed phase1 on the default-db conflict (the defect)")
        p1_out = (self.probe / "p1prev7b.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev7b-decision.sh", text=text,
            origin=f"the {PREV7_REV} baseline")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")]
        self.assertTrue(exec_line, out)
        self.assertIn("/home/otheruser/.ak-sending/new.db", exec_line[0],
                      "the round-7 candidate chose the configured new.db over the "
                      "running default registry.db (the defect)")

    def test_k9n1_round8_candidate_root_decoy_flips_migration_source(self):
        # K9-review contrast (g): the reviewer's exact fixture (b) against
        # the round-8 candidate (12a71c9) — its decision takes the FIRST
        # enumerated registry-data line, so the unrelated /root/.ak-sending
        # flips the migration source while the running service is unchanged.
        self._make_k9_runtime_identity_fixture(root_decoy=True)
        text = self.baseline_text(PREV8_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev8.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1prev8.sh" > "$PROBE_DIR/p1prev8.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV8_REV) + BOX_MODELS, body,
                                  name="driver-prev8-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev8.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev8-decision.sh", text=text,
            origin=f"the {PREV8_REV} baseline")
        self.assertEqual(rc, 0, out)
        self.assertIn("CHOSEN_DATA_SOURCE=/root/.ak-sending", out,
                      "the round-8 candidate flipped the migration source to the "
                      "unrelated root decoy (the defect)")
        self.assertNotIn("CHOSEN_DATA_SOURCE=/home/otheruser/.ak-sending", out)

    def test_k10n2_round9_candidate_keeps_unrelated_destination(self):
        # K10-review contrast (e): the reviewer's destination fixture (a)
        # against the round-9 candidate (806ea6c) — the correct runtime
        # source is selected, but its install skips the copy because the
        # destination files exist, exits 0 and enables the unrelated bytes.
        self._make_runtime_source()
        self._make_destination_decoy()
        env_mode = (
            'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            'STAGING_DIR="$STAGING_DIR" STAGED_BIN="$STAGED_BIN" SHORT_SHA=deadbee '
            'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
            'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            'DATA_SOURCE_DIR=/home/otheruser/.ak-sending CLOSE_7100=0 '
        )
        rc, out, p456_out = self.run_baseline_p456(
            PREV9_REV, env_mode, "p456prev9")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-9 candidate silently succeeded (the defect)")
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "unrelated-target-key",
                         "the round-9 candidate kept the unrelated destination bytes")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "unrelated-target-db")
        self.assertTrue((self.probe / "opt-ak" / "ak-cli").exists(),
                        "the round-9 candidate switched the services onto them")

    def test_k11n1_round9_candidate_misses_late_commit(self):
        # K11-review contrast (e): the reviewer's exact commit-in-interval
        # scenario against the round-9 candidate (806ea6c) — its migration
        # copy runs BEFORE the stops, so the commit landing at stop entry
        # (while the service still runs) is NOT in the target: the install
        # succeeds and the new unit's default database stays one revision
        # behind (active/revision 1 vs the source's revoked/revision 2).
        db = self._make_sqlite_runtime_source()
        hook = self._write_late_commit_hook()
        env_mode = (
            'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            'STAGING_DIR="$STAGING_DIR" STAGED_BIN="$STAGED_BIN" SHORT_SHA=deadbee '
            'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $REMOTE_DATA" '
            'REG_RW="$REMOTE_DATA" LEGACY_UNIT= OLD_REGISTRY_UNIT=oldregistry '
            'DATA_SOURCE_DIR=/home/otheruser/.ak-sending CLOSE_7100=0 '
        )
        rc, out, p456_out = self.run_baseline_p456(
            PREV9_REV, env_mode, "p456prev9late",
            run_env={"SYSTEMCTL_STOP_HOOK": hook,
                     "SYSTEMCTL_STOP_HOOK_UNIT": "oldregistry.service"},
            models="make_upgrade_model\n"
            'for f in registry.key registry.db; do rm -f "$REMOTE_DATA/$f"; done\n'
            'printf "[Service]\nExecStart=/opt/ak-sending/ak-cli registry-server --port 17200\n" > "$PROBE_DIR/etc/systemd/system/oldregistry.service"\n'
            'echo 1 > "$PROBE_DIR/systemctl-state/oldregistry.service.enabled"\n'
            'echo active > "$PROBE_DIR/systemctl-state/oldregistry.service.active"\n'
            'echo 4242 > "$PROBE_DIR/systemctl-state/oldregistry.service.pid"\n'
            'printf "%s\n" "$REMOTE_BIN" > "$PROBE_DIR/systemctl-state/4242.exe"\n'
            'cp "$REMOTE_BIN" "$PROBE_DIR/systemctl-state/oldregistry.service.running.bin"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-9 candidate succeeded with a stale target (the defect)")
        self.assertEqual(self._db_rows(db), [("revoked", 2)],
                         "the source carries the late commit")
        self.assertEqual(self._db_rows(self.probe / "opt-ak" / "data" / "registry.db"),
                         [("active", 1)],
                         "the round-9 candidate's target missed the late commit "
                         "(the exact reviewed TOCTOU)")

    def test_k10n1_round11_candidate_truncates_lf_value(self):
        # K12-review contrast (a): the round-11 candidate (1fad0a2) on the
        # real-LF-value fixture — its 'head -n 1' truncates USERPROFILE at
        # the LF and CONFIRMS /home/otheruser/.ak-sending.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "EnvironmentFile=/etc/default/oldregistry"],
            environ=b"HOME=/home/otheruser\0"
                    b"USERPROFILE=/home/otheruser\nsuffix\0USER=otheruser\0")
        envf = self.probe / "etc/default/oldregistry"
        envf.parent.mkdir(parents=True, exist_ok=True)
        envf.write_text("HOME=/home/otheruser\nUSERPROFILE='/home/otheruser\nsuffix'\n"
                        "USER=otheruser\n", encoding="utf-8", newline="\n")
        text = self.baseline_text(PREV10_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev10.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p1prev10.sh" > "$PROBE_DIR/p1prev10.out" 2>&1; '
            + 'echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV10_REV) + BOX_MODELS,
                                  body, name="driver-prev10-lf.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-11 candidate confirmed the truncated dir (the defect)")
        p1_out = (self.probe / "p1prev10.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")

    def test_k10n1_round11_candidate_uses_config_cwd(self):
        # K12-review contrast (b): the round-11 candidate resolves the
        # relative base against the CONFIG WorkingDirectory and picks the
        # configured decoy while the running cwd says otherwise.
        self.make_private_discovery_model(
            main_exec=self.K9_BASE,
            cmdline_extra="--port 17200",
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/configured-new",
                                "Environment=HOME=/home/otheruser",
                                "Environment=USERPROFILE="],
            environ=b"HOME=/home/otheruser\0USERPROFILE=\0USER=otheruser\0")
        for rel, tag in (("home/runtime-old", "runtime-real"),
                         ("home/configured-new", "configured-decoy")):
            d = self.probe / rel / ".ak-sending"
            d.mkdir(parents=True, exist_ok=True)
            for name in ("registry.key", "registry.db"):
                (d / name).write_text(f"{tag}-{name}", encoding="utf-8")
        text = self.baseline_text(PREV10_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev10b.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev10b.sh" > "$PROBE_DIR/p1prev10b.out" 2>&1; '
            + 'echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV10_REV) + BOX_MODELS,
                                  body, name="driver-prev10-cwd.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev10b.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/configured-new/\.ak-sending$",
                         "the round-11 candidate trusted the config cwd (the defect)")

    def _run_baseline_install_chain(self, rev: str, data_source: str,
                                    registry_exec: str, reg_rw: str,
                                    name: str, cwd_map: bool = True):
        """Run the BASELINE's own decision-derived install: its write_phase456
        extracted from git, with the decision values baked exactly as the
        baseline real_run would bake them."""
        text = self.baseline_text(rev)
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="' + registry_exec + '" ' + chr(10)
            + 'REG_RW="' + reg_rw + '" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=' + data_source + ' CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/' + name + '.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + ('PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
               if cwd_map else '')
            + 'bash "$PROBE_DIR/' + name + '.sh" > "$PROBE_DIR/' + name
            + '.out" 2>&1; echo "INSTALL_RC=$?"' + chr(10)
        )
        return self.run_driver(self.phase_defs(text, rev) + BOX_MODELS, body,
                               name=f"driver-{rev[:7]}-{name}.sh")

    def test_k14n1_round13_tip_relative_id_dir_copies_decoy(self):
        # K14-review contrast: 943a346 (and identically 0b41201) on the
        # reviewer's exact relative-'state' fixture — the baseline reports
        # 'runtime-id-dir: state' verbatim (its own phase1 never queries the
        # running cwd for an EXPLICIT --id-dir) and its install copies the
        # DEPLOYMENT-cwd decoy with exit 0.
        self._make_k14_relative_fixture("state")
        # The baseline's own phase1 (verbatim relative report).
        text = self.baseline_text(PREV12_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev12.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev12.sh" > "$PROBE_DIR/p1prev12.out" 2>&1; '
            + 'echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV12_REV) + BOX_MODELS,
                                  body, name="driver-prev12-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev12.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out, r"(?m)^  runtime-id-dir: state$",
                         "the baseline reported the relative id-dir verbatim")
        self.assertNotIn("readlink /proc/4242/cwd", self.stubs_log())
        # The baseline's own decision values (verbatim relative) through the
        # BASELINE's generator + install.
        rc, out = self._run_baseline_install_chain(
            PREV12_REV, "state",
            "$REMOTE_BIN registry-server --port 17200 --id-dir state",
            "state", "p456prev12rel")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out,
                      "the baseline installed from the decoy (the defect)")
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "deploy-cwd-decoy-registry.key",
                         "the baseline copied the DEPLOYMENT-cwd decoy's bytes")

    def test_k13n1_round12_tip_quoted_path_copies_wrong_dir(self):
        # K14-review precision rebuild: the baseline generation boundary is
        # exercised for real — the verbatim quoted CHOSEN value travels via a
        # FILE (cat preserves every byte; passing it through the caller
        # shell would strip the quotes before the generator ever ran), the
        # BASELINE's own write_phase456 generates, and the BASELINE's own
        # install script executes the baked 'DATA_SOURCE=...' line where the
        # quote becomes shell syntax.
        q = chr(39)
        profile = "/home/oth" + q + "er" + q + "user"
        self._make_k13_quoted_fixture(profile)
        dequoted = self.probe / "home" / "otheruser" / ".ak-sending"
        dequoted.mkdir(parents=True, exist_ok=True)
        for name in ("registry.key", "registry.db"):
            (dequoted / name).write_text(f"dequoted-wrong-{name}", encoding="utf-8")
        # The verbatim source path in a file (byte-preserving transport).
        (self.probe / "ds-verbatim.txt").write_text(
            profile + "/.ak-sending", encoding="utf-8", newline="\n")
        text = self.baseline_text(PREV11_REV)
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'DS_V=$(cat "$PROBE_DIR/ds-verbatim.txt")' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="$REMOTE_BIN registry-server --port 17200 --id-dir $DS_V" ' + chr(10)
            + 'REG_RW="$DS_V" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR="$DS_V" CLOSE_7100=0 REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456prev11q.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'grep -n "^DATA_SOURCE=" "$PROBE_DIR/p456prev11q.sh" '
            + '| sed "s/^/GEN_DATA_SOURCE_LINE: /"' + chr(10)
            + 'bash "$PROBE_DIR/p456prev11q.sh" > "$PROBE_DIR/p456prev11q.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV11_REV) + BOX_MODELS,
                                  body, name="driver-prev11-q2.sh")
        self.assertEqual(rc, 0, out)
        # The generated source really carries the raw quoted bytes (the
        # generation boundary is exercised, not the caller shell).
        self.assertIn("GEN_DATA_SOURCE_LINE:", out)
        gen_line = [l for l in out.splitlines()
                    if l.startswith("GEN_DATA_SOURCE_LINE:")][0]
        self.assertIn(q, gen_line,
                      "the generated script must still carry the quote byte")
        self.assertIn("INSTALL_RC=0", out,
                      "the baseline install exited 0 from the wrong dir (the defect)")
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "dequoted-wrong-registry.key",
                         "the executing shell de-quoted the path and copied "
                         "the OTHER directory's bytes")

    def test_k10n1_round14_tip_environ_verification_read_fail_open(self):
        # K15-review contrast: f425256 on the reviewer's exact fixture (real
        # trailing-LF USERPROFILE, ONLY the 4th/verification environ read
        # failing). The old 4-read design skipped the check on wc -c 0 and
        # the whole chain confirmed the truncated prefix dir.
        self._make_k15_env_fixture(tail_lf=True)
        text = self.baseline_text(PREV13_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev13.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'ENV_READ_FAIL_FROM=4 bash "$PROBE_DIR/p1prev13.sh" '
            + '> "$PROBE_DIR/p1prev13.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV13_REV) + BOX_MODELS,
                                  body, name="driver-prev13-env.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-14 tip skipped the check (the defect)")
        p1_out = (self.probe / "p1prev13.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$",
                         "the round-14 tip confirmed the TRUNCATED dir")
        markers = [m for m in self.stubs_log().splitlines()
                   if m.startswith("ENV_")]
        self.assertIn("ENV_READ_FAILED_4", markers,
                      "the injected 4th-read failure really fired")

    def test_k15n1_round14_tip_relative_db_path_keeps_relative_form(self):
        # K15-review contrast: f425256 on the relative-db fixture — its
        # generated unit PRESERVES the relative --db-path with no
        # WorkingDirectory, so the new service would open the decoy object
        # under its default cwd (/).
        rel = "home/otheruser/.ak-sending/registry.db"
        self._make_k15_db_fixture(rel)
        text = self.baseline_text(PREV13_REV)
        args = "--port 17200 --id-dir /home/otheruser/.ak-sending --db-path " + rel
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'TS=t12345 BUILD_DIR= PREBUILT_MODE=1 '
            + 'STAGING_DIR="$PROBE_DIR/staging" STAGED_BIN="$PROBE_DIR/staging/ak-cli" '
            + 'SHORT_SHA=deadbee ' + chr(10)
            + 'REGISTRY_EXEC="$REMOTE_BIN registry-server ' + args + '" ' + chr(10)
            + 'REG_RW="/home/otheruser/.ak-sending" LEGACY_UNIT= OLD_REGISTRY_UNIT= '
            + 'DATA_SOURCE_DIR=/home/otheruser/.ak-sending CLOSE_7100=0 '
            + 'REMOTE_DATA_PREOWNED=0 ' + chr(10)
            + 'write_phase456 "$PROBE_DIR/p456prev13db.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'bash "$PROBE_DIR/p456prev13db.sh" > "$PROBE_DIR/p456prev13db.out" 2>&1; '
            + 'echo "INSTALL_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV13_REV) + BOX_MODELS,
                                  body, name="driver-prev13-db.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        unit = (self.probe / "etc/systemd/system/ak-registry-server.service") \
            .read_text(encoding="utf-8")
        exec_line = [l for l in unit.splitlines() if l.startswith("ExecStart=")][0]
        self.assertIn("--db-path " + rel, exec_line,
                      "the round-14 tip kept the RELATIVE db form (the defect)")
        self.assertNotIn("WorkingDirectory=", unit)
        self.assertNotIn("/home/runtime-old", unit)

    def test_k10n1_round15_tip_accepts_partial_snapshot(self):
        # K16-review contrast: 0602f92 on the cat partial-then-fail fixture —
        # tee's success masks the reader's 74, the incomplete snapshot loses
        # the USERPROFILE record and the whole chain migrates the HOME decoy.
        self._make_k16_env_fixture(b"/home/runtime-real")
        text = self.baseline_text(PREV14_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev14.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'ENV_PARTIAL_FAIL=1 bash "$PROBE_DIR/p1prev14.sh" '
            + '> "$PROBE_DIR/p1prev14.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV14_REV) + BOX_MODELS,
                                  body, name="driver-prev14-part.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-15 tip accepted the partial snapshot (the defect)")
        p1_out = (self.probe / "p1prev14.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$",
                         "the round-15 tip fell back to HOME off the partial snapshot")
        self.assertIn("REVIEW_CAT_PARTIAL_ERROR=74", self.stubs_log())

    def test_k10n1_round15_tip_extraction_error_as_absent(self):
        # K16-review contrast: 0602f92 on the extraction-error fixture —
        # its '|| true' + empty-continue read the grep ERROR as
        # variable-absent and fell back to HOME. (f425256 refused the same
        # input via the byte-count comparison.)
        self._make_k16_env_fixture(b"/home/runtime-real")
        text = self.baseline_text(PREV14_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev14g.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EXTRACT_VAR=USERPROFILE bash "$PROBE_DIR/p1prev14g.sh" '
            + '> "$PROBE_DIR/p1prev14g.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV14_REV) + BOX_MODELS,
                                  body, name="driver-prev14-grep.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-15 tip read the extraction error as absent (the defect)")
        p1_out = (self.probe / "p1prev14g.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")
        self.assertIn("REVIEW_GREP_1_ERROR=2", self.stubs_log())

    def test_k15n1_round15_tip_bracket_db_rewrite_noop(self):
        # K16-review contrast: 0602f92 on the bracket db fixture — the
        # ${var//pattern/} substitution treats '[1]' as a pattern class, the
        # rewrite silently no-ops, the LOG claims success and the generated
        # unit keeps the cwd-dependent relative argument.
        self._make_k17_db_fixture("home/otheruser/.ak-sending/db[1].sqlite")
        text = self.baseline_text(PREV14_REV)
        args = ("--port 17200 --id-dir /home/otheruser/.ak-sending "
                "--db-path home/otheruser/.ak-sending/db[1].sqlite")
        body = (
            'mkdir -p "$PROBE_DIR/staging"' + chr(10)
            + 'cp "$AK_CLI_STUB" "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'chmod +x "$PROBE_DIR/staging/ak-cli"' + chr(10)
            + 'REGISTRY_EXEC="$REMOTE_BIN registry-server ' + args + '"' + chr(10)
            + 'runtime_cwd=/home/runtime-old' + chr(10)
            + 'REG_RW="/home/otheruser/.ak-sending"' + chr(10)
            + 'rel_db="home/otheruser/.ak-sending/db[1].sqlite"' + chr(10)
            + 'abs_db="$runtime_cwd/$rel_db"' + chr(10)
            + 'REGISTRY_EXEC=${REGISTRY_EXEC//--db-path $rel_db/--db-path $abs_db}' + chr(10)
            + 'REGISTRY_EXEC=${REGISTRY_EXEC//--db-path=$rel_db/--db-path=$abs_db}' + chr(10)
            + 'printf "AFTER_REWRITE=%s\n" "$REGISTRY_EXEC"' + chr(10)
        )
        rc, out = self.run_driver("", body, name="driver-prev14-br.sh")
        self.assertEqual(rc, 0, out)
        after = [l for l in out.splitlines() if l.startswith("AFTER_REWRITE=")][0]
        self.assertIn("db[1].sqlite", after)
        self.assertNotIn("/home/runtime-old/home/otheruser/.ak-sending/db[1].sqlite",
                         after,
                         "the round-15 tip's pattern substitution silently no-opped "
                         "on the bracket filename (the defect)")

    def test_k10n1_round16_tip_extraction_error_after_presence_ok(self):
        # K17-review contrast: 7a93314 on the two-grep fixture — the
        # presence triage passes (rc=0) but the value-extraction '|| true'
        # swallows the second grep's rc=2, the empty record continues as
        # variable-absent and the chain migrates the HOME decoy.
        self._make_k16_env_fixture(b"/home/runtime-real")
        text = self.baseline_text(PREV15_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev15.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EXTRACT_VAR=USERPROFILE GREP_EXTRACT_FAIL_AT=2 '
            + 'bash "$PROBE_DIR/p1prev15.sh" '
            + '> "$PROBE_DIR/p1prev15.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV15_REV) + BOX_MODELS,
                                  body, name="driver-prev15-g2.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-16 tip fell through the extraction error (the defect)")
        p1_out = (self.probe / "p1prev15.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/otheruser/\.ak-sending$")
        self.assertIn("REVIEW_GREP_2_ERROR=2", self.stubs_log())
        self.assertNotIn("REVIEW_GREP_1_ERROR=2", self.stubs_log())

    def test_k15n1_round16_tip_substring_split_corrupts_id_dir(self):
        # K17-review contrast: 7a93314 on the token-in-id-dir fixture —
        # the first-occurrence substring split corrupts the identity
        # argument and leaves the REAL --db-path relative while the log
        # claims a rewrite. Executed with the baseline's own decisions
        # block driving the same rewrite code path.
        identity = "/home/otheruser/--db-path-archive/.ak-sending"
        self._make_k18_db_fixture(identity, "state/registry.db")
        # Run the baseline's own phase1 + decision to get its CHOSEN values
        # verbatim (the defect lives in ITS decisions-block rewrite).
        text = self.baseline_text(PREV15_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev15b.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev15b.sh" '
            + '> "$PROBE_DIR/p1prev15b.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV15_REV) + BOX_MODELS,
                                  body, name="driver-prev15b-p1.sh")
        self.assertEqual(rc, 0, out)
        p1_out = (self.probe / "p1prev15b.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev15b-decision.sh", text=text,
            origin=f"the {PREV15_REV} baseline")
        self.assertEqual(rc, 0, out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        chosen_exec = exec_line[len("CHOSEN_EXEC="):]
        # The identity argument is corrupted and the real db stays relative
        # (parsed as a standalone argument, not a substring).
        self.assertNotIn("--id-dir " + identity + " ", exec_line,
                         "the round-16 tip corrupted the id-dir (the defect)")
        self.assertEqual(self._db_arg_of(chosen_exec), "state/registry.db",
                         "the real --db-path argument stayed relative (the defect)")

    def test_k10n1_round17_tip_partial_evidence_error_passes(self):
        # K18-review contrast: 4db2b4f on the partial-output-then-error
        # fixture — its 'grep | wc' only surfaces wc's status, so the
        # truncated count '-1' matched and the whole chain copied the
        # prefix decoy with the reported dir /home/runtime-real/.ak-sending.
        self._make_k19_env_fixture()
        text = self.baseline_text(PREV16_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev16.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'GREP_EVIDENCE_VAR=USERPROFILE GREP_EVIDENCE_MODE=partial '
            + 'bash "$PROBE_DIR/p1prev16.sh" '
            + '> "$PROBE_DIR/p1prev16.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV16_REV) + BOX_MODELS,
                                  body, name="driver-prev16-be.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out,
                      "the round-17 tip passed on the partial count (the defect)")
        p1_out = (self.probe / "p1prev16.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-real/\.ak-sending$")
        self.assertIn("REVIEW_BYTE_GREP_PARTIAL_ERROR=2", self.stubs_log())

    def test_k15n1_round17_tip_falsely_refuses_equals_form(self):
        # K18-review contrast: 4db2b4f on the relative-equals fixture — its
        # rewrite produces '--db-path=<abs>' but the verification accepts
        # only a bare token, so the decision refuses and nothing runs.
        identity = "/home/otheruser/.ak-sending"
        args = "--port 17200 --id-dir " + identity + " --db-path=state/registry.db"
        self.make_private_discovery_model(
            main_exec="/opt/ak-sending/ak-cli registry-server " + args,
            cmdline_extra=args,
            unit_service_lines=["User=otheruser",
                                "WorkingDirectory=/home/runtime-old"],
            environ=b"HOME=/home/otheruser\0USERPROFILE=/home/otheruser\0")
        text = self.baseline_text(PREV16_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev16q.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev16q.sh" '
            + '> "$PROBE_DIR/p1prev16q.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV16_REV) + BOX_MODELS,
                                  body, name="driver-prev16-q.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev16q.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev16-q-decision.sh", text=text,
            origin=f"the {PREV16_REV} baseline")
        self.assertNotEqual(
            rc, 0,
            "the round-17 tip falsely refused the rewritten equals form "
            "(the defect): " + out)
        self.assertIn("the --db-path rewrite did not take", out)
        self.assertNotIn("CHOSEN_", out)

    def test_k19n1_round18_tip_misbinds_literal_filename(self):
        # K19-review contrast: 1d76871 on the literal-filename fixture —
        # its greedy sed pre-parse binds '--port' as the db value, the
        # verification compares against the SAME wrong absolute path and
        # passes, and the full chain points the new unit at
        # /home/runtime-old/--port (the misparsed-token decoy).
        self._make_k20_literal_fixture()
        text = self.baseline_text(PREV17_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev17.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev17.sh" '
            + '> "$PROBE_DIR/p1prev17.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV17_REV) + BOX_MODELS,
                                  body, name="driver-prev17-lit.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev17.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev17-lit-decision.sh", text=text,
            origin=f"the {PREV17_REV} baseline")
        self.assertEqual(
            rc, 0, "the round-18 tip passed the misparse (the defect): " + out)
        exec_line = [l for l in out.splitlines() if l.startswith("CHOSEN_EXEC=")][0]
        self.assertEqual(self._db_arg_of(exec_line[len("CHOSEN_EXEC="):]),
                         "/home/runtime-old/--port",
                         "the round-18 tip bound '--port' to --db-path (the defect)")

    def test_k19n1_round17_tip_refused_literal_filename_pre_stop(self):
        # Secondary contrast: 4db2b4f on the same fixture — its bare-token
        # verification refused BEFORE any stop (the input was not silently
        # misrouted there; the misrouting was opened by the round-19 change).
        self._make_k20_literal_fixture()
        text = self.baseline_text(PREV16_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev17b.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/p1prev17b.sh" '
            + '> "$PROBE_DIR/p1prev17b.out" 2>&1; echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV16_REV) + BOX_MODELS,
                                  body, name="driver-prev17b-lit.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev17b.out").read_text(encoding="utf-8")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev17b-decision.sh", text=text,
            origin=f"the {PREV16_REV} baseline")
        self.assertNotEqual(
            rc, 0,
            "4db2b4f refused the equals form pre-stop (the misparse path "
            "was not open there): " + out)

    def test_k19n2_round18_tip_accepts_dangling_and_duplicates(self):
        # K19-review contrast: 1d76871 on the dangling and duplicate
        # fixtures — its extraction/verification never checked the pending
        # end-state or the binding count, so both shapes passed into a
        # stop+install whose generated unit the real CLI rejects
        # ("a value is required for '--db-path <DB_PATH>' /
        # "cannot be used multiple times").
        running = "--port 17200 --id-dir /home/otheruser/.ak-sending --db-path=state/registry.db"
        for i, configured in enumerate(
                (running + " --db-path",
                 running + " --db-path=state/registry.db")):
            with self.subTest(shape=i):
                self.setUp()
                rc, out = self._k20_decision_of(
                    configured, running, f"driver-prev17-dup{i}.sh",
                    baseline_rev=PREV17_REV)
                self.assertEqual(
                    rc, 0,
                    "the round-18 tip accepted the invalid binding (the "
                    "defect): " + out)
                self.assertNotIn("dangling", out)
                self.assertNotIn("cannot be used multiple times", out)
                self.doCleanups()

    def _run_baseline_p1_isolated(self, rev: str, text: str, script: str,
                                  out: str):
        """A baseline's OWN write_phase1, run from an isolated cwd (K20-N1
        reviewer protocol: only the decision's cwd carries the match)."""
        body = (
            'write_phase1 "$PROBE_DIR/' + script + '" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'mkdir -p "$PROBE_DIR/preflight-cwd"' + chr(10)
            + '(cd "$PROBE_DIR/preflight-cwd" '
            + '&& PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
            + 'bash "$PROBE_DIR/' + script + '") '
            + '> "$PROBE_DIR/' + out + '" 2>&1; echo "RUN_RC=$?"' + chr(10))
        return self.run_driver(self.phase_defs(text, rev) + BOX_MODELS, body,
                               name=f"driver-{rev}-{script}.sh")

    def test_k20n1_round20_tip_expands_glob_and_reads_decoy(self):
        # K20-review contrast: 110bb34 on the reviewer's glob fixture WITH
        # the matching file in the decision cwd — the unquoted walk runs
        # pathname expansion, the whitelist vets the GLOBBED 'db1.sqlite',
        # the full chain stops the old service, and the generated unit
        # points at the GLOB-DECOY database (the defect). Both spellings.
        for sep, label in ((" ", "space"), ("=", "equals")):
            with self.subTest(spelling=label):
                self.setUp()
                self._make_k20_glob_fixture(sep)
                self._drop_glob_match(sep)
                self._make_oldregistry_running()
                text = self.baseline_text(PREV18_REV)
                rc, out = self._run_baseline_p1_isolated(
                    PREV18_REV, text, "p1prev18g.sh", "p1prev18g.out")
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=0", out,
                              "the round-20 tip's phase1 passed the bracket "
                              "value (the expansion lived in the decision)")
                p1_out = (self.probe / "p1prev18g.out").read_text(
                    encoding="utf-8")
                rc, out = self.run_decision_and_config(
                    p1_out, "driver-prev18g-decision.sh", text=text,
                    origin=f"the {PREV18_REV} baseline")
                self.assertEqual(
                    rc, 0,
                    "the round-20 tip accepted the globbed value (the "
                    "defect): " + out)
                chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
                self.assertEqual(
                    self._db_arg_of(chosen_exec),
                    "/home/runtime-old/db1.sqlite",
                    "the round-20 tip expanded 'db[1].sqlite' to the decoy "
                    "name (the defect)")
                rc, out = self._install_with_chosen(
                    chosen_exec, chosen_rw, old_registry, "p456prev18g",
                    text=text, rev=PREV18_REV)
                self.assertEqual(rc, 0, out)
                self.assertIn("INSTALL_RC=0", out)
                self.assertIn("systemctl stop", self.stubs_log(),
                              "the round-20 tip stopped the old service on "
                              "the globbed binding (the defect's harm)")
                p456_out = (self.probe / "p456prev18g.out").read_text(
                    encoding="utf-8")
                self.assertIn("oldregistry:exists:enabled:active", p456_out)
                unit = (self.probe
                        / "etc/systemd/system/ak-registry-server.service") \
                    .read_text(encoding="utf-8")
                unit_exec = [l for l in unit.splitlines()
                             if l.startswith("ExecStart=")][0]
                self.assertEqual(self._db_arg_of(unit_exec),
                                 "/home/runtime-old/db1.sqlite")
                import sqlite3
                con = sqlite3.connect(
                    self.probe / "home/runtime-old/db1.sqlite")
                marker = con.execute(
                    "SELECT value FROM review_marker").fetchone()[0]
                con.close()
                self.assertEqual(
                    marker, "glob-decoy",
                    "the round-20 tip's generated unit reads the GLOB "
                    "DECOY, not the real 'db[1].sqlite' object (the defect)")
                self.doCleanups()

    def test_k20n2_round20_tip_accepts_empty_and_option_value_bindings(self):
        # K20-review contrast: 110bb34 accepted BOTH CLI-invalid shapes the
        # reviewer demonstrated — the explicit EMPTY '--db-path=' (the real
        # CLI: rc 2 "a value is required for '--db-path <DB_PATH>' but none
        # was supplied"; the generated unit kept the shape after the stop)
        # and the SPACE form binding the next OPTION ('--db-path --port
        # 17200' -> '--port' consumed as the filename, '17200' left an
        # unexpected positional the CLI reports as "unexpected argument
        # '17200' found").
        identity = "/home/otheruser/.ak-sending"
        cases = [
            ("empty",
             "--port 17200 --id-dir " + identity + " --db-path=",
             "--port 17200 --id-dir " + identity,
             "--db-path="),
            ("option-value",
             "--id-dir " + identity + " --db-path --port 17200",
             "--id-dir " + identity + " --db-path=--port --port 17200",
             "--db-path /home/runtime-old/--port 17200"),
        ]
        for label, configured, running, unit_shape in cases:
            with self.subTest(shape=label):
                self.setUp()
                if label == "option-value":
                    # the equals-literal '--port' object the RUNNING side
                    # legally binds (also the object the defect retargets)
                    import sqlite3
                    obj = self.probe / "home/runtime-old/--port"
                    obj.parent.mkdir(parents=True, exist_ok=True)
                    if obj.exists():
                        obj.unlink()
                    con = sqlite3.connect(obj)
                    con.execute("CREATE TABLE review_marker(value TEXT)")
                    con.execute("INSERT INTO review_marker VALUES (?)",
                                ("runtime-real",))
                    con.commit()
                    con.close()
                self.make_private_discovery_model(
                    main_exec="/opt/ak-sending/ak-cli registry-server "
                    + configured,
                    cmdline_extra=running,
                    unit_service_lines=["User=otheruser",
                                        "WorkingDirectory=/home/runtime-old"],
                    environ=b"HOME=/home/otheruser\0"
                            b"USERPROFILE=/home/otheruser\0")
                live = self.probe / identity.lstrip("/")
                live.mkdir(parents=True, exist_ok=True)
                (live / "registry.key").write_text("synthetic-runtime-key",
                                                   encoding="utf-8")
                (live / "registry.db").write_text("synthetic-identity-default-db",
                                                  encoding="utf-8")
                self._make_oldregistry_running()
                text = self.baseline_text(PREV18_REV)
                rc, out = self._run_baseline_p1_isolated(
                    PREV18_REV, text, "p1prev18n2.sh", "p1prev18n2.out")
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=0", out,
                              "the round-20 tip's phase1 accepted the "
                              "CLI-invalid shape (the empty value fell "
                              "through as the default db; the option token "
                              "was consumed as the filename)")
                p1_out = (self.probe / "p1prev18n2.out").read_text(
                    encoding="utf-8")
                rc, out = self.run_decision_and_config(
                    p1_out, "driver-prev18n2-decision.sh", text=text,
                    origin=f"the {PREV18_REV} baseline")
                self.assertEqual(
                    rc, 0,
                    f"the round-20 tip accepted the {label} shape (the "
                    f"defect): " + out)
                chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
                self.assertIn(unit_shape.split(" ")[0], chosen_exec)
                rc, out = self._install_with_chosen(
                    chosen_exec, chosen_rw, old_registry, "p456prev18n2",
                    text=text, rev=PREV18_REV)
                self.assertEqual(rc, 0, out)
                self.assertIn("INSTALL_RC=0", out)
                self.assertIn("systemctl stop", self.stubs_log(),
                              "the round-20 tip stopped the old service "
                              "before generating the CLI-rejected unit "
                              "(the defect's harm)")
                unit = (self.probe
                        / "etc/systemd/system/ak-registry-server.service") \
                    .read_text(encoding="utf-8")
                unit_exec = [l for l in unit.splitlines()
                             if l.startswith("ExecStart=")][0]
                self.assertIn(unit_shape, unit_exec,
                              "the generated unit carries the CLI-rejected "
                              "binding (the reviewer's real-CLI probe at "
                              "110bb34 returned rc 2 on exactly this shape)")
                self.doCleanups()

    def test_k21n1_round20_tip_misbinds_id_dir_and_copies_decoy(self):
        # K21-review contrast: 110bb34 on the literal '--id-dir' path — the
        # greedy sed binds '--port' as the identity (resolved against the
        # running cwd to the DECOY), the pattern rewrite corrupts the real
        # port option, and the full chain stops the old service and copies
        # the DECOY directory's key/db. The reviewer's real git-714099a CLI
        # returns rc 2 "unexpected argument '/home/runtime-old/--port'
        # found" on the generated argv. Both spellings.
        for i, (sep, label) in enumerate(((" ", "space"), ("=", "equals"))):
            with self.subTest(spelling=label):
                self.setUp()
                self._make_k21_id_fixture(sep)
                text = self.baseline_text(PREV18_REV)
                script = "p1prev21i%d.sh" % i
                outname = "p1prev21i%d.out" % i
                rc, out = self.run_driver(
                    self.phase_defs(text, PREV18_REV) + BOX_MODELS,
                    'write_phase1 "$PROBE_DIR/' + script + '" '
                    '|| { echo GEN_FAILED; exit 90; }' + chr(10)
                    + 'PROC_CWD_MAP="/proc/4242/cwd=/home/runtime-old" '
                    + 'bash "$PROBE_DIR/' + script + '" '
                    + '> "$PROBE_DIR/' + outname + '" 2>&1; '
                    + 'echo "RUN_RC=$?"' + chr(10),
                    name="driver-prev21i%d.sh" % i)
                self.assertEqual(rc, 0, out)
                self.assertIn("RUN_RC=0", out,
                              "the round-20 tip's phase1 passed the "
                              "misbound identity (the defect)")
                p1_out = (self.probe / outname).read_text(encoding="utf-8")
                self.assertIn("  runtime-id-dir: /home/runtime-old/--port",
                              p1_out,
                              "the greedy extraction bound the FOLLOWING "
                              "'--port' as the identity (the defect)")
                rc, out = self.run_decision_and_config(
                    p1_out, "driver-prev21i-decision%d.sh" % i, text=text,
                    origin=f"the {PREV18_REV} baseline")
                self.assertEqual(
                    rc, 0,
                    "the round-20 tip accepted the misbound identity (the "
                    "defect): " + out)
                chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
                self.assertIn("/home/runtime-old/--port", chosen_exec,
                              "the corrupted absolute token is baked into "
                              "the generated argv (the defect)")
                self.assertIn("CHOSEN_DATA_SOURCE=/home/runtime-old/--port",
                              out)
                rc, out = self._install_with_chosen(
                    chosen_exec, chosen_rw, old_registry,
                    "p456prev21i%d" % i, text=text, rev=PREV18_REV,
                    data_source="/home/runtime-old/--port")
                self.assertEqual(rc, 0, out)
                self.assertIn("INSTALL_RC=0", out)
                self.assertIn("systemctl stop", self.stubs_log(),
                              "the round-20 tip stopped the old service on "
                              "the misbound identity (the defect's harm)")
                dest = self.probe / "opt-ak" / "data"
                self.assertEqual(
                    (dest / "registry.key").read_text(encoding="utf-8"),
                    "substring-decoy-key",
                    "the DECOY's key was migrated (the defect)")
                self.assertEqual(
                    (dest / "registry.db").read_text(encoding="utf-8"),
                    "substring-decoy-db")
                if sep == " ":
                    unit = (self.probe
                            / "etc/systemd/system/ak-registry-server.service"
                            ).read_text(encoding="utf-8")
                    unit_exec = [l for l in unit.splitlines()
                                 if l.startswith("ExecStart=")][0]
                    self.assertIn(
                        "--id-dir /home/otheruser/--id-dir "
                        "/home/runtime-old/--port 17200", unit_exec,
                        "the reviewer-verified corrupted argv shape: the "
                        "stray absolute token + bare positional 17200 the "
                        "real CLI rejects with rc 2")
                self.doCleanups()

    def test_k21n2_round20_tip_splits_spaced_home_and_copies_decoy(self):
        # K21-review contrast: 110bb34's word-splitting /home enumeration —
        # the spaced home splits into '/home/other' + a stray fragment, the
        # PREFIX decoy becomes the only candidate, and the full chain
        # migrates the decoy and enables the new units (the defect).
        self._make_k21_home_fixture("other user", decoy=True)
        listing = self.probe / "ls-home-listing.txt"
        listing.write_text("other user", encoding="utf-8", newline="\n")
        text = self.baseline_text(PREV18_REV)
        rc, out = self.run_driver(
            self.phase_defs(text, PREV18_REV) + BOX_MODELS,
            'write_phase1 "$PROBE_DIR/p1prev21h.sh" '
            '|| { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'SS_EMPTY=1 LS_HOME_FILE=' + listing.as_posix()
            + ' bash "$PROBE_DIR/p1prev21h.sh" '
            + '> "$PROBE_DIR/p1prev21h.out" 2>&1; echo "RUN_RC=$?"'
            + chr(10),
            name="driver-prev21h.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1prev21h.out").read_text(encoding="utf-8")
        self.assertEqual(
            [x for x in p1_out.splitlines() if x.startswith("registry-data:")],
            ["registry-data: /home/other (key:yes db:yes)"],
            "the round-20 tip split the spaced path into the PREFIX decoy "
            "(the defect)")
        rc, out = self.run_decision_and_config(
            p1_out, "driver-prev21h-decision.sh", text=text,
            origin=f"the {PREV18_REV} baseline")
        self.assertEqual(rc, 0,
                         "the round-20 tip accepted the split candidate "
                         "(the defect): " + out)
        self.assertIn("CHOSEN_DATA_SOURCE=/home/other", out)
        chosen_exec, chosen_rw, old_registry = self._chosen_of(out)
        rc, out = self._install_with_chosen(
            chosen_exec, chosen_rw, old_registry, "p456prev21h",
            text=text, rev=PREV18_REV, data_source="/home/other")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        self.assertIn("systemctl enable --now", self.stubs_log())
        dest = self.probe / "opt-ak" / "data"
        self.assertEqual((dest / "registry.key").read_text(encoding="utf-8"),
                         "synthetic-split-decoy-key",
                         "the SPLIT DECOY's key was migrated (the defect)")
        self.assertEqual((dest / "registry.db").read_text(encoding="utf-8"),
                         "synthetic-split-decoy-db")
        real = (self.probe / "home" / "other user" / ".ak-sending"
                / "registry.key")
        self.assertEqual(real.read_text(encoding="utf-8"),
                         "synthetic-existing-key",
                         "the real spaced source stayed in place, never "
                         "migrated (the defect's other half)")

    def test_k12n1_round11_candidate_swallows_key_chmod_failure(self):
        # K12-review contrast: 1fad0a2 (post-move) swallows the key-chmod
        # failure — INSTALL_RC=0, the new binary enabled, no recovery —
        # while 09fceb6 (pre-move, top-level set -e) caught it and left the
        # box untouched on the old image.
        for rev, expect_rc, expect_recovery in (
                (PREV10_REV, "RUN_RC=0", False),
                (PREV10_PRE_REV, "RUN_RC=1", False)):
            with self.subTest(rev=rev):
                self.setUp()
                # In-place semantics (the model's REMOTE_DATA IS the runtime
                # identity) so the K10-N2 fresh-dirty refusal stays out of
                # the way; both baseline tips carry REMOTE_DATA_PREOWNED.
                env_mode = self.P456_ENV_PREBUILT.replace(
                    "CLOSE_7100=0 ", "CLOSE_7100=0 REMOTE_DATA_PREOWNED=1 ")
                rc, out, p456_out = self.run_baseline_p456(
                    rev, env_mode, f"p456chmod-{rev[:7]}",
                    run_env={"FAIL_KEY_CHMOD": "1"},
                    models="make_upgrade_model" + chr(10))
                self.assertEqual(rc, 0, out)
                self.assertIn(expect_rc, out)
                self.assertIn("chmod stub: forced key permission failure",
                              p456_out)
                if rev == PREV10_REV:
                    self.assertNotIn("RECOVERY", p456_out,
                                     "the round-11 candidate swallowed it (the defect)")
                    self.assertEqual(
                        (self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                        AK_CLI_STUB,
                        "the round-11 candidate enabled the new image anyway")
                else:
                    # 09fceb6: top-level set -e caught it BEFORE any stop;
                    # old image, old state.
                    self.assertNotIn("systemctl stop", self.stubs_log())
                    self.assertEqual(
                        (self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                        AK_CLI_OLD)
                self.doCleanups()

    def test_k10n1_round12_tip_cwd_tail_lf_confirms_truncated_dir(self):
        # K13-review contrast (a): 0b41201 on the trailing-LF cwd fixture —
        # command substitution strips the LF and the truncated prefix dir is
        # confirmed through the whole chain.
        self._make_k13_cwd_fixture(b"/home/runtime-old\x0a")
        text = self.baseline_text(PREV11_REV)
        body = (
            'write_phase1 "$PROBE_DIR/p1prev11.sh" || { echo GEN_FAILED; exit 90; }' + chr(10)
            + 'PROC_CWD_RAW_FILE="$PROBE_DIR/cwd-bytes" '
            + 'bash "$PROBE_DIR/p1prev11.sh" > "$PROBE_DIR/p1prev11.out" 2>&1; '
            + 'echo "RUN_RC=$?"' + chr(10)
        )
        rc, out = self.run_driver(self.phase_defs(text, PREV11_REV) + BOX_MODELS,
                                  body, name="driver-prev11-cwd.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)  # the round-12 tip confirmed the truncated cwd
        p1_out = (self.probe / "p1prev11.out").read_text(encoding="utf-8")
        self.assertRegex(p1_out,
                         r"(?m)^  runtime-id-dir: /home/runtime-old/\.ak-sending$")

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
