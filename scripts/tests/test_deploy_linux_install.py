"""K 复核 K-R1/K-R2 offline regressions for scripts/deploy_linux.sh.

The REAL production generators (compose_registry_exec, write_phase1,
write_phase456, write_phase7) are evaluated as real Bash and the scripts they
generate are EXECUTED. Only the privileged remote surface is stubbed on PATH
(sudo, install, systemctl, tee, chown, useradd, uname, journalctl, ss, sleep);
every baked path points into a per-test probe directory, so no /opt, /etc or
system state is touched. This proves, for the --binary flow:
  - K-R1: the install source is a DISTINCT staging file (never ak-cli.new ==
    ak-cli.new), all checks run BEFORE any service stop, and a mid-switchover
    failure restarts the stopped services before aborting;
  - K-R2: a preserved unit's ExecStart keeps its arguments but the executable
    is rewritten to the install path, phase 7 detects a stale/lying live
    executable, and preflight verifies the install path via sudo.
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

SUDO_STUB = r'''#!/usr/bin/env bash
set -u
printf 'sudo %s\n' "$*" >> "$PROBE_DIR/stubs.log"
args=()
skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$a" in
    -n) ;;
    -m|-o|-g) skip=1 ;;
    *) args+=("$a") ;;
  esac
done
if [ -n "${SUDO_FAIL_TESTW:-}" ] && [ "${args[0]}" = test ] && [ "${args[1]}" = -w ]; then
  exit 1
fi
exec "${args[@]}"
'''

INSTALL_STUB = r'''#!/usr/bin/env bash
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
'''

SYSTEMCTL_STUB = r'''#!/usr/bin/env bash
set -u
printf 'systemctl %s\n' "$*" >> "$PROBE_DIR/stubs.log"
case "$1" in
  is-active) echo active ;;
  show)
    path="${SYSTEMCTL_LIVE_PATH:?SYSTEMCTL_LIVE_PATH unset}"
    echo "ExecStart={ path=$path ; argv[]=$path registry-server --port 17200 ; ignore_errors=no }"
    ;;
esac
exit 0
'''

TEE_STUB = r'''#!/usr/bin/env bash
set -u
dest="${@: -1}"
printf 'tee %s\n' "$dest" >> "$PROBE_DIR/stubs.log"
case "$dest" in
  /etc/*) dest="$PROBE_DIR$dest" ;;
esac
mkdir -p "$(dirname "$dest")"
cat > "$dest"
exit 0
'''

CHOWN_STUB = r'''#!/usr/bin/env bash
set -u
printf 'chown %s\n' "$*" >> "$PROBE_DIR/stubs.log"
exit 0
'''

USERADD_STUB = r'''#!/usr/bin/env bash
set -u
printf 'useradd %s\n' "$*" >> "$PROBE_DIR/stubs.log"
exit 0
'''

UNAME_STUB = r'''#!/usr/bin/env bash
set -u
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) echo Linux x86_64 ;;
esac
'''

JOURNALCTL_STUB = r'''#!/usr/bin/env bash
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
'''

SS_STUB = r'''#!/usr/bin/env bash
set -u
if [ -n "${SS_STUB_OUTPUT:-}" ]; then
  echo "LISTEN 0 0 0.0.0.0:17200 0.0.0.0:*"
  echo "UNCONN 0 0 0.0.0.0:17101 0.0.0.0:*"
fi
exit 0
'''

SLEEP_STUB = "#!/usr/bin/env bash\nexit 0\n"

STUBS = {
    "sudo": SUDO_STUB, "install": INSTALL_STUB, "systemctl": SYSTEMCTL_STUB,
    "tee": TEE_STUB, "chown": CHOWN_STUB, "useradd": USERADD_STUB,
    "uname": UNAME_STUB, "journalctl": JOURNALCTL_STUB, "ss": SS_STUB,
    "sleep": SLEEP_STUB,
}

AK_CLI_STUB = '#!/usr/bin/env bash\necho "ak-cli 0.1.0 (git deadbee stub)"\n'

PREAMBLE = r'''
set -uo pipefail
PROBE_DIR="$(cd "$1" && pwd)"
export PROBE_DIR
PATH="$PROBE_DIR/bin:$PATH"
export PATH
AK_CLI_STUB="$PROBE_DIR/ak-cli.stub"
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


class DeployInstallTests(unittest.TestCase):
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
        ak_stub = self.probe / "ak-cli.stub"
        ak_stub.write_text(AK_CLI_STUB, encoding="utf-8", newline="\n")
        ak_stub.chmod(0o755)
        self.driver_env = dict(os.environ)
        self.driver_env.pop("BASH_ENV", None)

    # -- plumbing -----------------------------------------------------------
    def section(self, start: str, end: str) -> str:
        a = self.text.index(start)
        b = self.text.index(end, a)
        return self.text[a:b]

    def definitions(self) -> str:
        return (
            self.section("compose_registry_exec() {", "# systemd unit templates")
            + self.section("write_phase1() {", "# Phase 3: build on the server")
            + self.section("write_phase456() {", "# Phase 7: verification")
            + self.section("write_phase7() {", "\nreal_run() {")
        )

    def run_bash(self, body: str, timeout: float = 30):
        driver = self.probe / "driver.sh"
        script = PREAMBLE + self.definitions() + body
        driver.write_text(script, encoding="utf-8", newline="\n")
        result = subprocess.run(
            [BASH, "--noprofile", "--norc", driver.as_posix(), self.probe.as_posix()],
            cwd=self.probe, env=self.driver_env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout,
        )
        return result.returncode, result.stdout.decode("utf-8", errors="replace")

    def stubs_log(self) -> str:
        path = self.probe / "stubs.log"
        return path.read_text(encoding="utf-8") if path.exists() else ""

    def bash_probe(self, out: str) -> str:
        """The probe path as Bash normalizes it (e.g. /c/Users/... on MSYS)."""
        match = re.search(r"^PROBE=(\S+)$", out, re.MULTILINE)
        self.assertIsNotNone(match, f"driver did not report PROBE: {out}")
        return match.group(1)

    def switchover_install_calls(self, log: str):
        """(src, dst) of each binary install into the ak-cli.new slot. The
        sudo stub strips the flags, so the install stub logs exactly
        `install <src> <dst>`."""
        calls = []
        for line in log.splitlines():
            if line.startswith("install ") and line.endswith("ak-cli.new"):
                parts = line.split()
                calls.append((parts[1], parts[2]))
        return calls

    def generate_and_run_p456(self, body_extra: str = "", run_env: dict | None = None):
        env = dict(self.driver_env)
        env.update(run_env or {})
        driver = self.probe / "driver.sh"
        script = PREAMBLE + self.definitions() + STAGE_AND_GENERATE + body_extra
        driver.write_text(script, encoding="utf-8", newline="\n")
        result = subprocess.run(
            [BASH, "--noprofile", "--norc", driver.as_posix(), self.probe.as_posix()],
            cwd=self.probe, env=env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30,
        )
        return result.returncode, result.stdout.decode("utf-8", errors="replace")

    # -- K-R2: ExecStart composition ----------------------------------------
    def test_compose_registry_exec_rewrites_executable_keeps_arguments(self):
        rc, out = self.run_bash(r'''
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
        # The binary install must run staging-file -> ak-cli.new (source and
        # destination are DIFFERENT files; the old flow installed ak-cli.new
        # onto itself after the services were stopped).
        swaps = self.switchover_install_calls(log)
        self.assertEqual(len(swaps), 1, log)
        src, dst = swaps[0]
        self.assertEqual(src, f"{probe}/staging/ak-cli")
        self.assertNotEqual(src, dst)
        self.assertEqual(dst, f"{probe}/opt-ak/ak-cli.new")
        # The deployed binary exists with the staged bytes.
        self.assertEqual((self.probe / "opt-ak" / "ak-cli").read_text(encoding="utf-8"),
                         AK_CLI_STUB)
        # Service sequence: two stops, then daemon-reload + enable --now —
        # and NO recovery starts on the success path.
        self.assertIn("systemctl stop ak-registry-server.service", log)
        self.assertIn("systemctl stop ak-stun-server.service", log)
        self.assertLess(log.index("systemctl stop ak-registry-server.service"),
                        log.index("systemctl daemon-reload"))
        self.assertIn("systemctl enable --now ak-registry-server.service"
                      " ak-stun-server.service", log)
        self.assertNotIn("systemctl start", log)
        # K-R1: the staging upload is cleaned up on success.
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
        self.assertIn("recovering services", p456_out)
        self.assertIn("forced switchover failure", p456_out)
        log = self.stubs_log()
        # The failure happened at the install step, i.e. AFTER the services
        # were stopped; recovery must START them again afterwards.
        self.assertIn("install -m 0755", log)
        stop_reg = log.index("systemctl stop ak-registry-server.service")
        fail = log.index("install -m 0755")
        start_reg = log.index("systemctl start ak-registry-server.service")
        self.assertLess(stop_reg, fail)
        self.assertLess(fail, start_reg)
        self.assertIn("systemctl start ak-stun-server.service", log)
        # The failing install still ran staging -> ak-cli.new (src != dst).
        swaps = self.switchover_install_calls(log)
        self.assertEqual(len(swaps), 1, log)
        self.assertNotEqual(swaps[0][0], swaps[0][1])

    # -- K-R1: same-file guard fires before anything is stopped --------------
    def test_same_file_source_and_destination_refused_before_stops(self):
        # Re-stage straight to the install-intermediate name: the generator
        # must refuse (K-R1) before any systemctl stop.
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

    # -- K-R2: phase 7 verifies the RUNNING executable ------------------------
    def _run_p7(self, live_path: str, short_sha: str = "deadbee"):
        body = (
            f'SHORT_SHA={short_sha} write_phase7 "$PROBE_DIR/p7.sh" '
            '|| { echo GEN_FAILED; exit 90; }\n'
            'mkdir -p "$REMOTE_BASE"\n'
            'cp "$AK_CLI_STUB" "$REMOTE_BIN"\n'
            'chmod +x "$REMOTE_BIN"\n'
            f'SYSTEMCTL_LIVE_PATH={live_path} SS_STUB_OUTPUT=1 '
            'bash "$PROBE_DIR/p7.sh" > "$PROBE_DIR/p7.out" 2>&1; echo "RUN_RC=$?"\n'
        )
        return self.generate_and_run_p456(body)

    def test_phase7_accepts_running_units_on_the_deployed_binary(self):
        rc, out = self._run_p7('"$REMOTE_BIN"')
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=0", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        bin_path = f"{self.bash_probe(out)}/opt-ak/ak-cli"
        self.assertIn(f"unit ak-registry-server runs {bin_path}", p7_out)
        self.assertIn(f"unit ak-stun-server runs {bin_path}", p7_out)

    def test_phase7_detects_stale_executable_preserved_from_old_unit(self):
        # The exact K-R2 scenario: systemd still runs /usr/local/bin/ak-cli
        # while the new binary was installed at $REMOTE_BIN.
        rc, out = self._run_p7("/usr/local/bin/ak-cli")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("runs /usr/local/bin/ak-cli", p7_out)
        self.assertIn("K-R2 stale executable", p7_out)

    def test_phase7_detects_live_binary_reporting_wrong_revision(self):
        rc, out = self._run_p7('"$REMOTE_BIN"', short_sha="beefdead")
        self.assertEqual(rc, 0, out)
        self.assertIn("RUN_RC=6", out)
        p7_out = (self.probe / "p7.out").read_text(encoding="utf-8")
        self.assertIn("want 'git beefdead'", p7_out)

    # -- K-R2: preflight install-path writability via sudo --------------------
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


if __name__ == "__main__":
    unittest.main()
