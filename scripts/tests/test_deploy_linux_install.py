"""K 复核 K-R1/K-R2/K2-N1/K2-R2/K2-R3 offline regressions for scripts/deploy_linux.sh.

The REAL production generators (compose_registry_exec, write_phase1,
write_phase456, write_phase7) are evaluated as real Bash and the scripts they
generate are EXECUTED. Only the privileged remote surface is stubbed on PATH
(sudo, install, systemctl, tee, chown, useradd, uname, journalctl, ss, sleep,
test, cp, cat, sed, readlink, ls); every baked path points into a per-test
probe directory, so no real /opt, /etc or /proc state is touched. This proves,
for the --binary flow:
  - K-R1: the install source is a DISTINCT staging file, all checks run
    BEFORE any service stop, and a mid-switchover failure enters recovery;
  - K-R2: a preserved unit's ExecStart keeps its arguments but the
    executable is rewritten to the install path; phase 7 rejects a stale
    CONFIGURATION and preflight verifies the install path via sudo;
  - K2-N1: the WHOLE switchover (first stop through enable --now) sits in
    one recovery window — failures injected at the FIRST UNIT WRITE, at
    daemon-reload and at enable/start all restore the previous binary +
    unit files and re-enable/re-start what was enabled/active, and a FAILED
    recovery is loudly reported;
  - K2-R2: a stop failure is never swallowed (it aborts into recovery), and
    phase 7 verifies the RUNNING PROCESS (MainPID -> /proc/<PID>/exe via
    sudo readlink) — with the reviewer's precondition: OLD PROCESS STILL
    RUNNING, DISK PATH ALREADY REPLACED, verification must FAIL;
  - K2-R3: every read-only discovery/existence probe runs through sudo -n,
    and occupancy that cannot be determined fails the preflight BEFORE any
    switchover.

PERMISSION MODELS ARE OFFLINE: the ss stub shows other users' pid= info only
when invoked through the sudo stub, and the test/cat/cp/sed stubs map
/etc, /home, /root and /proc arguments into the probe directory. These model
what root-vs-non-root can observe on Linux; they are NOT a claim of having
verified real-Linux permissions.

OLD-CODE PROOFS (K 复核 K2-R5): BaselineContrastTests EXECUTES the install/
verify/preflight code actually shipped in the git baselines — the rejected
candidate f150503 (whose recovery branch ended before the unit writes and
whose verify checked only the configured path) and the original batch-K
baseline f9582e5 (no recovery at all). The baselines are run through the
same stubs; a missing revision skips the contrast (it cannot be fabricated).
For genuinely-new failure modes the honest statement is: the old-code run
demonstrates the UNMITIGATED failure, and the new-code test asserts the
mitigation EXISTS — the new assertions fail on the baseline runs.
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

# Git baselines executed by the old-code contrast proofs (K 复核 K2-R5).
PREV_REV = "f150503"   # the rejected repair-round candidate (K2 findings)
BASE_REV = "f9582e5"   # the original batch-K baseline (K findings)

SUDO_STUB = r"""#!/usr/bin/env bash
set -u
printf 'sudo %s\n' "$*" >> "$PROBE_DIR/stubs.log"
# Strip only sudo's OWN leading -n: `sudo -n sed -n ...` must keep sed's -n
# (stripping every -n made every sed print whole files).
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

SYSTEMCTL_STUB = r"""#!/usr/bin/env bash
set -u
printf 'systemctl %s\n' "$*" >> "$PROBE_DIR/stubs.log"
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
unit="${!#}"
case "$1" in
  is-active)
    amap="${SYSTEMCTL_ACTIVE_MAP:-ak-registry-server.service=active ak-stun-server.service=active}"
    for m in $amap; do
      if [ "${m%%=*}" = "$unit" ]; then echo "${m#*=}"; exit 0; fi
    done
    echo "${SYSTEMCTL_DEFAULT_ACTIVE:-active}"; exit 0 ;;
  is-enabled)
    emap="${SYSTEMCTL_ENABLED_MAP:-ak-registry-server.service=enabled ak-stun-server.service=enabled}"
    for m in $emap; do
      if [ "${m%%=*}" = "$unit" ]; then echo "${m#*=}"; exit 0; fi
    done
    echo "${SYSTEMCTL_DEFAULT_ENABLED:-enabled}"; exit 0 ;;
  show)
    case " $* " in
      *" MainPID "*) echo "${SYSTEMCTL_MAINPID:-4242}" ;;
      *) echo "ExecStart={ path=${SYSTEMCTL_LIVE_PATH:?SYSTEMCTL_LIVE_PATH unset} ; argv[]=${SYSTEMCTL_LIVE_PATH} registry-server --port 17200 ; ignore_errors=no }" ;;
    esac ;;
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
# without sudo the same foreign sockets show no process column — exactly the
# split the K2-R3 finding describes for a non-root `ss -p`.
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

# test/cp/cat/sed/readlink/ls stubs: map /etc, /home, /root and /proc
# arguments into the probe directory (an OFFLINE model of root-only views)
# and delegate to the real tool. The sudo stub execs these, so every probe
# that goes through sudo lands here (K 复核 K2-R3).
TEST_STUB = r"""#!/usr/bin/env bash
set -u
args=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*) args+=("$PROBE_DIR$a") ;;
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
    /etc/*|/home/*|/root/*|/proc/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_CP@" -a "${mapped[@]}"
"""

CAT_STUB = r"""#!/usr/bin/env bash
set -u
printf 'cat %s\n' "$*" >> "$PROBE_DIR/stubs.log"
mapped=()
for a in "$@"; do
  case "$a" in
    /etc/*|/home/*|/root/*|/proc/*) mapped+=("$PROBE_DIR$a") ;;
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
    /etc/*|/home/*|/root/*|/proc/*) mapped+=("$PROBE_DIR$a") ;;
    *) mapped+=("$a") ;;
  esac
done
exec "@REAL_SED@" "${mapped[@]}"
"""

# /proc/<PID>/exe model for the K2-R2 running-process check: PROC_EXE_MAP is
# a newline-separated "path=value" list; a missing entry is an error so an
# unmapped read can never silently pass.
READLINK_STUB = r"""#!/usr/bin/env bash
set -u
target="${@: -1}"
printf 'readlink %s\n' "$*" >> "$PROBE_DIR/stubs.log"
if [ -n "${PROC_EXE_MAP:-}" ]; then
  while IFS='=' read -r k v; do
    if [ "$k" = "$target" ]; then printf '%s\n' "$v"; exit 0; fi
  done <<< "$PROC_EXE_MAP"
fi
echo "readlink stub: no PROC_EXE_MAP entry for $target" >&2
exit 1
"""

LS_STUB = r"""#!/usr/bin/env bash
set -u
printf 'ls %s\n' "$*" >> "$PROBE_DIR/stubs.log"
for a in "$@"; do
  if [ "$a" = "/home" ]; then
    # Model the home directory listing root would see (foreign users incl.).
    for e in ${LS_HOME_ENTRIES:-otheruser}; do printf '%s\n' "$e"; done
    exit 0
  fi
done
exec "@REAL_LS@" "$@"
"""

STUBS = {
    "sudo": SUDO_STUB, "install": INSTALL_STUB, "systemctl": SYSTEMCTL_STUB,
    "tee": TEE_STUB, "chown": CHOWN_STUB, "useradd": USERADD_STUB,
    "uname": UNAME_STUB, "journalctl": JOURNALCTL_STUB, "ss": SS_STUB,
    "sleep": SLEEP_STUB, "test": TEST_STUB, "cp": CP_STUB, "cat": CAT_STUB,
    "sed": SED_STUB, "readlink": READLINK_STUB, "ls": LS_STUB,
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
# tell old bytes from new bytes on disk.
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
        # Bake the POSIX real tools into the delegating stubs.
        for tool in ("cp", "cat", "sed", "ls"):
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
    def section(self, text: str, start: str, end: str) -> str:
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

    def run_driver(self, definitions: str, body: str, env: dict | None = None,
                   name: str = "driver.sh", timeout: float = 60):
        driver = self.probe / name
        # The extracted definitions never end with a newline (the section end
        # marker points AT the "\n" before it) — always join with an explicit
        # newline or the first body line would be glued onto a comment.
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
        return self.run_driver(self.definitions() + STAGE_AND_GENERATE, body_extra,
                               env=run_env, name="driver.sh")

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
        """(src, dst) of each binary install into the ak-cli.new slot."""
        calls = []
        for line in log.splitlines():
            if line.startswith("install ") and line.endswith("ak-cli.new"):
                parts = line.split()
                calls.append((parts[1], parts[2]))
        return calls

    def make_upgrade_model(self) -> tuple[str, str]:
        """Model an UPGRADE box: old binary, old data, old enabled+active units.

        Returns (old_registry_unit_text, old_stun_unit_text) for equality
        checks after failed switches. All paths live inside the probe dir.
        """
        base = self.probe / "opt-ak"
        (base / "data").mkdir(parents=True, exist_ok=True)
        (base / "ak-cli").write_text(AK_CLI_OLD, encoding="utf-8", newline="\n")
        (base / "ak-cli").chmod(0o755)
        (base / "data" / "registry.key").write_text("key-bytes", encoding="utf-8")
        (base / "data" / "registry.db").write_text("db-bytes", encoding="utf-8")
        etc = self.probe / "etc" / "systemd" / "system"
        etc.mkdir(parents=True, exist_ok=True)
        old_reg = ("[Unit]\nDescription=OLD registry unit (upgrade model)\n\n"
                   "[Service]\nUser=akreg\n"
                   "ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 --id-dir /opt/ak-sending/data\n"
                   "\n[Install]\nWantedBy=multi-user.target\n")
        old_stun = ("[Unit]\nDescription=OLD stun unit (upgrade model)\n\n"
                    "[Service]\nUser=akreg\n"
                    "ExecStart=/opt/ak-sending/ak-cli stun-server --port 17101\n"
                    "\n[Install]\nWantedBy=multi-user.target\n")
        (etc / "ak-registry-server.service").write_text(old_reg, encoding="utf-8", newline="\n")
        (etc / "ak-stun-server.service").write_text(old_stun, encoding="utf-8", newline="\n")
        return old_reg, old_stun

    def make_private_discovery_model(self):
        """K2-R3 OFFLINE permission model: a registry port held by ANOTHER
        USER's process (pid only visible to root) plus a PRIVATE identity
        directory under a foreign home."""
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

    def assert_recovery_restored_old_state(self, out: str, old_reg: str, old_stun: str):
        """Shared K2-N1 assertions: old binary + old units back on disk, the
        previously enabled/active units re-enabled and RE-STARTED after the
        failure point, and the outcome REPORTED."""
        log = self.stubs_log()
        # The failure happened after the services were stopped; recovery must
        # re-enable and START them again afterwards (ordering vs the failing
        # call is asserted by each caller).
        self.assertIn("systemctl enable ak-registry-server.service", log)
        self.assertIn("systemctl enable ak-stun-server.service", log)
        self.assertIn("systemctl start ak-registry-server.service", log)
        self.assertIn("systemctl start ak-stun-server.service", log)
        self.assertIn("RECOVERY OK", out)
        self.assertIn("ERROR: service switchover failed", out)
        # Old bytes back: binary restored, units restored, no half-state left.
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD, "previous binary must be restored")
        etc = self.probe / "etc" / "systemd" / "system"
        self.assertEqual((etc / "ak-registry-server.service").read_text(encoding="utf-8"),
                         old_reg, "previous registry unit must be restored")
        self.assertEqual((etc / "ak-stun-server.service").read_text(encoding="utf-8"),
                         old_stun, "previous stun unit must be restored")
        self.assertFalse((self.probe / "opt-ak" / "ak-cli.new").exists(),
                         "the ak-cli.new intermediate must be cleaned up")
        self.assertFalse((self.probe / "opt-ak" / "ak-cli.old-t12345").exists(),
                         "the moved-aside old binary must be back in place")


class DeployInstallTests(ProbeHarness):
    """Current-tree behavior (K 复核 K-R1/K-R2/K2-N1/K2-R2/K2-R3)."""

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
        # K2-N1: the switchover opens by CAPTURING the unit state before any
        # stop; then stops, writes units, reloads, enables.
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("captured unit state", p456_out)
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertIn("systemctl stop ak-stun-server.service", log)
        self.assertLess(self.log_line(log, "systemctl stop ak-registry-server.service"),
                        self.log_line(log, "systemctl daemon-reload"))
        self.assertIn("systemctl enable --now ak-registry-server.service"
                      " ak-stun-server.service", log)
        self.assertNotIn("systemctl start", log)
        self.assertFalse((self.probe / "staging").exists())
        # K-R2 end-to-end: the written unit keeps the old ARGUMENTS but runs
        # the NEW executable.
        unit = (self.probe / "etc" / "systemd" / "system" / "ak-registry-server.service")
        self.assertTrue(unit.is_file(), "unit file was not captured by the tee stub")
        expected = (f"ExecStart={probe}/opt-ak/ak-cli registry-server"
                    " --port 7200 --id-dir /var/lib/ak-legacy")
        self.assertIn(expected, unit.read_text(encoding="utf-8"))
        self.assertNotIn("/usr/local/bin/ak-cli", unit.read_text(encoding="utf-8"))

    # -- K-R1: mid-switchover failure restarts the stopped services ----------
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
        stop_reg = self.log_line(log, "systemctl stop ak-registry-server.service")
        fail = self.log_line(log, "install -m 0755")
        start_reg = self.log_line(log, "systemctl start ak-registry-server.service")
        self.assertLess(stop_reg, fail)
        self.assertLess(fail, start_reg)
        self.assertIn("systemctl start ak-stun-server.service", log)
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

    # -- K2-N1: failure at the FIRST UNIT WRITE restores the old config ------
    def test_k2n1_first_unit_write_failure_restores_old_configuration(self):
        old_reg, old_stun = self.make_upgrade_model()
        rc, out = self.generate_and_run_p456(
            'export FAIL_TEE_UNIT=ak-registry-server\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure writing /etc/systemd/system/ak-registry-server.service",
                      p456_out)
        log = self.stubs_log()
        # K2-R3 witness: the unit backup existence probe went through sudo.
        self.assertIn("sudo -n test -f /etc/systemd/system/ak-registry-server.service", log)
        # Ordering: stop -> (binary install) -> FAILING first unit write ->
        # restore-from-backup -> daemon-reload -> enable -> start.
        stop = self.log_line(log, "systemctl stop ak-registry-server.service")
        fail = self.log_line(log, "tee /etc/systemd/system/ak-registry-server.service")
        restore = self.log_line(
            log, "ak-registry-server.service /etc/systemd/system/ak-registry-server.service")
        reload_line = self.log_line(log, "systemctl daemon-reload")
        enable = self.log_line(log, "systemctl enable ak-registry-server.service")
        start_reg = self.log_line(log, "systemctl start ak-registry-server.service")
        start_stun = self.log_line(log, "systemctl start ak-stun-server.service")
        for earlier, later, what in (
                (stop, fail, "stop before the failing unit write"),
                (fail, restore, "restore after the failing unit write"),
                (restore, reload_line, "daemon-reload after unit restore"),
                (reload_line, enable, "re-enable after reload"),
                (enable, start_reg, "registry restart after re-enable"),
                (start_reg, start_stun, "stun restart after registry")):
            self.assertLess(earlier, later, f"ordering broken: {what}")
        self.assert_recovery_restored_old_state(p456_out, old_reg, old_stun)

    # -- K2-N1: daemon-reload failure restores the old config ----------------
    def test_k2n1_daemon_reload_failure_restores_old_configuration(self):
        old_reg, old_stun = self.make_upgrade_model()
        rc, out = self.generate_and_run_p456(
            'export SYSTEMCTL_FAIL_MATCH="daemon-reload" SYSTEMCTL_FAIL_ONCE=1\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure: daemon-reload", p456_out)
        log = self.stubs_log()
        # The switchover's daemon-reload failed; recovery ran its own (second)
        # daemon-reload and then started the services again.
        first = self.log_line(log, "systemctl daemon-reload")
        start = self.log_line(log, "systemctl start ak-registry-server.service")
        self.assertLess(first, start)
        self.assertEqual(len([l for l in log.splitlines()
                              if l == "systemctl daemon-reload"]), 2, log)
        self.assert_recovery_restored_old_state(p456_out, old_reg, old_stun)

    # -- K2-N1: enable/start failure restores the old config -----------------
    def test_k2n1_enable_start_failure_restores_old_configuration(self):
        old_reg, old_stun = self.make_upgrade_model()
        rc, out = self.generate_and_run_p456(
            'export SYSTEMCTL_FAIL_MATCH="enable --now ak-registry-server.service"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        self.assertIn("forced failure: enable --now ak-registry-server.service", p456_out)
        log = self.stubs_log()
        fail = self.log_line(log, "systemctl enable --now ak-registry-server.service")
        start = self.log_line(log, "systemctl start ak-registry-server.service")
        self.assertLess(fail, start)
        self.assert_recovery_restored_old_state(p456_out, old_reg, old_stun)

    # -- K2-N1: a FAILED recovery is loudly reported, never silent -----------
    def test_k2n1_failed_recovery_is_loudly_reported(self):
        # Two independent injections: the FIRST unit write fails (entering
        # recovery), and every recovery START also fails — recovery must then
        # report INCOMPLETE loudly, not exit quietly.
        self.make_upgrade_model()
        rc, out = self.generate_and_run_p456(
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
        old_reg, old_stun = self.make_upgrade_model()
        rc, out = self.generate_and_run_p456(
            'export SYSTEMCTL_FAIL_MATCH="stop ak-registry-server.service"\n'
            'bash "$PROBE_DIR/p456.sh" > "$PROBE_DIR/p456.out" 2>&1; echo "RUN_RC=$?"\n')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=5", out)
        p456_out = (self.probe / "p456.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        # The very first stop failed: NOTHING may have been written or moved
        # (the old process model keeps running the old binary), and recovery
        # must re-assert the previous state and start what was active.
        self.assertNotIn("tee /etc/", log)
        self.assertIn("systemctl start ak-registry-server.service", log)
        self.assertIn("systemctl start ak-stun-server.service", log)
        self.assertIn("RECOVERY OK", p456_out)
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_OLD)
        etc = self.probe / "etc" / "systemd" / "system"
        self.assertEqual((etc / "ak-registry-server.service").read_text(encoding="utf-8"),
                         old_reg)
        self.assertEqual((etc / "ak-stun-server.service").read_text(encoding="utf-8"),
                         old_stun)

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
            f'SYSTEMCTL_LIVE_PATH={live_path} SYSTEMCTL_MAINPID={mainpid} '
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
        # DISK PATH was already replaced with the new version. The running
        # process's /proc/<PID>/exe still points at the renamed OLD inode
        # (ak-cli.old-<ts>), so verification must FAIL — the old check (the
        # configured ExecStart path + its --version) passed in this state.
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
        # Occupancy: the foreign-user registry process is now discoverable
        # (pid + owning unit), which the migration decision depends on.
        self.assertIn("port 17200/tcp: in use (pid 4242, unit oldregistry.service)", p1_out)
        self.assertIn("  old-registry-unit: oldregistry.service", p1_out)
        self.assertIn(
            "  ExecStart=/opt/ak-sending/ak-cli registry-server --port 17200 "
            "--id-dir /home/otheruser/.ak-sending", p1_out)
        self.assertIn("port 17101/udp: in use (pid 4243, unit oldsignal.service)", p1_out)
        # Identity: the PRIVATE dir under the foreign home is found.
        self.assertIn("registry-data: /home/otheruser/.ak-sending (key:yes db:yes)", p1_out)
        # The probes themselves went through sudo -n (K2-R3).
        log = self.stubs_log()
        self.assertIn("sudo -n ss -ltnp", log)
        self.assertIn("sudo -n ss -lunp", log)
        self.assertIn("sudo -n cat /proc/4242/cgroup", log)
        self.assertIn("sudo -n ls -1 /home", log)
        self.assertIn("sudo -n test -e /home/otheruser/.ak-sending/registry.key", log)
        # No BARE ss probe anywhere (a non-root ss -p cannot see the pid):
        # every ss invocation must have a matching sudo -n ss invocation.
        n_ss = len([l for l in log.splitlines() if l.startswith("ss ")])
        n_sudo_ss = len([l for l in log.splitlines() if l.startswith("sudo -n ss ")])
        self.assertEqual(n_ss, n_sudo_ss,
                         f"every ss probe must run through sudo -n (bare={n_ss}, sudo={n_sudo_ss})")

    def test_k2r3_phase1_fails_clearly_when_occupancy_undeterminable(self):
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


class BaselineContrastTests(ProbeHarness):
    """K 复核 K2-R5: old-code proofs that EXECUTE the baselines' own generated
    code with the same stubs.

    Honest statements only:
      - the baselines' install/verify/preflight code is really executed;
      - for failure modes that did not exist in the old code (K2-N1's unit
        write recovery), the old-code run demonstrates the UNMITIGATED
        failure (services left stopped, disk left broken, nothing started)
        and the NEW tests assert the mitigation exists;
      - a missing git baseline SKIPS the contrast instead of erroring out
        mid-setup (a test-setup error is not old behavior).
    """

    def baseline_text(self, rev: str) -> str:
        text = git_show(rev, "scripts/deploy_linux.sh")
        if not text:
            self.skipTest(f"git baseline {rev} not available locally; "
                          "cannot run the old-code contrast honestly")
        return text

    def phase_defs(self, text: str) -> str:
        """write_phase1 + write_phase456 + write_phase7 of a baseline.

        Deliberately extracts only generators that EXIST in the baseline —
        the previous round's proofs died extracting a function the old
        script never had (test-setup error, not old behavior)."""
        return (
            self.section(text, "write_phase1() {", "# Phase 3: build on the server")
            + self.section(text, "write_phase456() {", "# Phase 7: verification")
            + self.section(text, "write_phase7() {", "\nreal_run() {")
        )

    def run_prev_or_base_p456(self, rev: str, body_env: str, fail_env: dict,
                              out_name: str = "p456old.out"):
        text = self.baseline_text(rev)
        old_reg, old_stun = self.make_upgrade_model()
        if "PREBUILT_MODE=1" in body_env:
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
            + body_env
            + f'write_phase456 "$PROBE_DIR/{out_name}.sh" || {{ echo GEN_FAILED; exit 90; }}\n'
            + f'bash "$PROBE_DIR/{out_name}.sh" > "$PROBE_DIR/{out_name}" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text), body, env=fail_env,
                                  name=f"driver-{rev}.sh")
        return rc, out, (self.probe / out_name).read_text(encoding="utf-8"), old_reg, old_stun

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

    # -- K2-N1: baselines leave the box stopped and broken at the first unit
    #    write. The old code has no (reaching) recovery; the NEW tests above
    #    assert recovery EXISTS.
    def _assert_unmitigated_unit_write_failure(self, rc_out, p456_out):
        rc, out = rc_out
        self.assertEqual(rc, 0, out)  # the driver harness itself must succeed
        self.assertIn("RUN_RC=1", out)
        self.assertIn("forced failure writing /etc/systemd/system/ak-registry-server.service",
                      p456_out)
        log = self.stubs_log()
        stop = self.log_line(log, "systemctl stop ak-registry-server.service")
        fail = self.log_line(log, "tee /etc/systemd/system/ak-registry-server.service")
        self.assertLess(stop, fail, "old code stops the services before dying at the unit write")
        # UNMITIGATED: nothing is ever started again, no restore, no reload.
        self.assertNotIn("systemctl start", log)
        self.assertNotIn("systemctl daemon-reload", log)
        # No cp line whose DESTINATION is an /etc unit path (i.e. no restore).
        self.assertFalse(
            [l for l in log.splitlines()
             if l.startswith("cp ") and re.search(r" /etc/systemd/system/\S+$", l)],
            f"unexpected unit restore in the old code: {log}")
        # Disk left BROKEN: corrupted partial unit + new binary stranded in
        # place, old binary stranded in the rename slot.
        etc = self.probe / "etc" / "systemd" / "system"
        self.assertIn("CORRUPTED-UNIT-PARTIAL",
                      (etc / "ak-registry-server.service").read_text(encoding="utf-8"))
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB, "old code leaves the NEW binary in place")
        self.assertTrue((self.probe / "opt-ak" / "ak-cli.old-t12345").exists(),
                        "old code strands the previous binary in the rename slot")

    def test_k2n1_prev_candidate_unit_write_failure_is_unmitigated(self):
        # The rejected f150503 candidate: its recovery branch ended BEFORE
        # the unit writes, so a first-unit-write failure stranded the box —
        # exactly the reviewer's reproduction.
        rc, out, p456_out, _, _ = self.run_prev_or_base_p456(
            PREV_REV, self.P456_ENV_PREBUILT + "\n",
            {"FAIL_TEE_UNIT": "ak-registry-server"}, out_name="p456prev.out")
        self._assert_unmitigated_unit_write_failure((rc, out), p456_out)

    def test_k2n1_baseline_unit_write_failure_is_unmitigated(self):
        # The original f9582e5 baseline: no recovery at all.
        rc, out, p456_out, _, _ = self.run_prev_or_base_p456(
            BASE_REV, self.P456_ENV_BUILD + "\n",
            {"FAIL_TEE_UNIT": "ak-registry-server"}, out_name="p456base.out")
        self._assert_unmitigated_unit_write_failure((rc, out), p456_out)

    # -- K2-R2: the rejected candidate swallows the stop failure and its
    #    verify still passes while the OLD process keeps running.
    def test_k2r2_prev_candidate_swallow_and_false_success(self):
        text = self.baseline_text(PREV_REV)
        # Step 1: install with a FAILED registry stop. The old code swallowed
        # it ('|| true'), kept deploying and exited 0 — the model keeps the
        # old process "active" (is-active stays active in the stub).
        body = (
            'STAGING_DIR="$PROBE_DIR/staging"\n'
            'STAGED_BIN="$STAGING_DIR/ak-cli"\n'
            'mkdir -p "$STAGING_DIR"\n'
            'cp "$AK_CLI_STUB" "$STAGED_BIN"\n'
            'chmod +x "$STAGED_BIN"\n'
            + self.P456_ENV_PREBUILT
            + 'write_phase456 "$PROBE_DIR/p456prev.sh" || { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_FAIL_MATCH="stop ak-registry-server.service" '
            'bash "$PROBE_DIR/p456prev.sh" > "$PROBE_DIR/p456prev.out" 2>&1; echo "INSTALL_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text), body, name="driver-prev-stop.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("INSTALL_RC=0", out)
        prev_out = (self.probe / "p456prev.out").read_text(encoding="utf-8")
        log = self.stubs_log()
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertNotIn("systemctl start", log,
                         "old code never restarts the still-active old unit")
        self.assertIn("systemctl enable --now ak-registry-server.service", log)
        # The old process model: the unit stays "active"; its /proc exe keeps
        # pointing at the renamed old inode. The candidate's verify checked
        # only the CONFIGURED ExecStart path — and passes on this state.
        body7 = (
            'SHORT_SHA=deadbee write_phase7 "$PROBE_DIR/p7prev.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_LIVE_PATH="$REMOTE_BIN" SYSTEMCTL_MAINPID=4242 '
            'PROC_EXE_MAP="/proc/4242/exe=$REMOTE_BASE/ak-cli.old-t12345" '
            'bash "$PROBE_DIR/p7prev.sh" > "$PROBE_DIR/p7prev.out" 2>&1; echo "VERIFY_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text), body7, name="driver-prev-p7.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("VERIFY_RC=0", out,
                      "the rejected candidate's verify must pass in this state (the defect)")
        p7_out = (self.probe / "p7prev.out").read_text(encoding="utf-8")
        self.assertNotIn("readlink", self.stubs_log(),
                         "the rejected candidate never inspects the running process")

    def test_k2r2_baseline_verify_only_checks_disk_version(self):
        # The original f9582e5 phase 7 has no running-process check at all:
        # it runs the DISK path's --version and exits 0 while the old
        # process model still holds the old inode.
        text = self.baseline_text(BASE_REV)
        body = (
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BIN"\n'
            'chmod +x "$REMOTE_BIN"\n'
            'SHORT_SHA=deadbee write_phase7 "$PROBE_DIR/p7base.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'SYSTEMCTL_MAINPID=4242 '
            'PROC_EXE_MAP="/proc/4242/exe=$REMOTE_BASE/ak-cli.old-t12345" '
            'bash "$PROBE_DIR/p7base.sh" > "$PROBE_DIR/p7base.out" 2>&1; echo "VERIFY_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text), body, name="driver-base-p7.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("VERIFY_RC=0", out,
                      "the baseline verify passes on the old-process/disk-swapped state")
        self.assertNotIn("readlink", self.stubs_log())

    # -- K2-R3: the baselines' bare (non-sudo) probes cannot see the foreign
    #    process or the private identity (OFFLINE permission model).
    def test_k2r3_baseline_preflight_is_blind_to_foreign_process_and_private_dir(self):
        text = self.baseline_text(BASE_REV)
        self.make_private_discovery_model()
        body = (
            'write_phase1 "$PROBE_DIR/p1base.sh" || { echo GEN_FAILED; exit 90; }\n'
            'bash "$PROBE_DIR/p1base.sh" > "$PROBE_DIR/p1base.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        rc, out = self.run_driver(self.phase_defs(text), body, name="driver-base-p1.sh")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p1_out = (self.probe / "p1base.out").read_text(encoding="utf-8")
        # Exactly the outputs the review reproduced offline: pid unknown,
        # not-a-systemd-unit, identity invisible -> migration missed.
        self.assertIn("port 17200/tcp: in use (pid unknown, unit not-a-systemd-unit)", p1_out)
        self.assertIn("port 17101/udp: in use (pid unknown, unit not-a-systemd-unit)", p1_out)
        self.assertNotIn("old-registry-unit:", p1_out)
        self.assertNotIn("registry-data:", p1_out)
        # And the probes ran BARE (the defect): no sudo on any ss call.
        for line in self.stubs_log().splitlines():
            if line.startswith("sudo ") and " ss " in f" {line} ":
                self.fail(f"baseline unexpectedly used sudo for ss: {line}")


if __name__ == "__main__":
    unittest.main()
