"""Real Bash process regressions for NAT-matrix lifecycle; no sudo or networking.

Execute the production lifecycle/run_case functions, replacing only ip/CLI and
shortening poll sleeps. The children, pipes, signals, waits and exit statuses are
REAL. These tests prove process ownership, not Linux netns or NAT acceptance.
The external fixtures self-expire and emergency cleanup targets only their PIDs.
"""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
MATRIX = ROOT / "scripts/nat_matrix_test.sh"
BASH = shutil.which("bash")

IP_FIXTURE = r'''#!/usr/bin/env bash
set -u
case "$*" in
  'netns exec akalice '*|'netns exec akbob '*)
    shift 3
    exec "$@"
    ;;
  'netns exec akr1 sh -c '*|'netns exec akr2 sh -c '*)
    printf '%s\n' "$BASHPID" >> "$PROBE_DIR/sampler-command.pids"
    exec sleep "${PROBE_SAMPLER_SECONDS:-0.05}"
    ;;
  'netns pids '*)
    for file in "$PROBE_DIR"/receiver.pid "$PROBE_DIR"/sender.pid; do
      [[ -f "$file" ]] || continue
      read -r pid < "$file"
      kill -0 "$pid" 2>/dev/null && printf '%s\n' "$pid"
    done
    exit 0
    ;;
  'netns del '*|'netns delete '*)
    for file in "$PROBE_DIR"/receiver.pid "$PROBE_DIR"/sender.pid; do
      [[ -f "$file" ]] || continue
      read -r pid < "$file"
      if kill -0 "$pid" 2>/dev/null; then
        printf 'DELETE_WITH_LIVE_PROCESS=%s\n' "$pid" >> "$PROBE_DIR/events"
      fi
    done
    printf 'DELETE=%s\n' "$3" >> "$PROBE_DIR/events"
    ;;
  *) printf 'Unexpected ip boundary\n' >&2; exit 91 ;;
esac
'''

CLI_FIXTURE = r'''#!/usr/bin/env bash
set -u
role=''
for arg in "$@"; do
  case "$arg" in code-recv) role=receiver ;; join-send) role=sender ;; esac
done
printf '%s\n' "$BASHPID" > "$PROBE_DIR/$role.pid"
if [[ "$role" == receiver ]]; then
  printf 'AKI1-SYNTHETIC-CODE\n'
  if [[ "${PROBE_RECEIVER:-waiting}" == stubborn ]]; then trap '' TERM; fi
  # A finite process keeps a red regression from leaking indefinitely.
  exec sleep 4
elif [[ "$role" == sender ]]; then
  if [[ "${PROBE_SAMPLER_READY:-}" != "" ]]; then
    for ((poll=0; poll<100; poll++)); do
      [[ -s "$PROBE_DIR/$PROBE_SAMPLER_READY" ]] && break
      command sleep 0.01
    done
    [[ -s "$PROBE_DIR/$PROBE_SAMPLER_READY" ]] || exit 93
  fi
  if [[ "${PROBE_SENDER:-fail}" == hanging ]]; then exec sleep 4; fi
  printf 'no candidate accepted the connection\n'
  exit 9
fi
exit 92
'''

SLEEP_FIXTURE = r'''#!/usr/bin/env bash
set -u
if [[ "$1" == 5 ]]; then
  printf '%s\n' "$BASHPID" >> "$PROBE_DIR/sampler-sleep.pids"
  exec "$REAL_SLEEP" 4
fi
exec "$REAL_SLEEP" "$@"
'''

PREAMBLE = r'''
set -uo pipefail
set +m
PROBE_DIR="$(cd "$1" && pwd)"
printf '%s\n' "$BASHPID" > "$PROBE_DIR/driver.pid"
export PROBE_DIR
WORK="$PROBE_DIR/work"
mkdir -p "$WORK"
TAG='[nat-matrix]'
SIZE_MB=1
CLI="$PROBE_DIR/bin/cli-fixture"
REG_PORT=17300 REG_FP=synthetic STUN_PORT=17101
REAL_SLEEP="$(command -v sleep)"
export REAL_SLEEP
PATH="$PROBE_DIR/bin:$PATH"
export PATH
log() { printf '%s %s\n' "$TAG" "$*"; }
# Shorten poll delays with Bash builtins (forking hundreds of Windows sleep
# processes is slower than the nominal timeout). Fixture lifetimes stay real.
sleep() {
  local start="${EPOCHREALTIME/./}"
  while ((10#${EPOCHREALTIME/./} - 10#$start < 5000)); do :; done
}
'''

# This override records a caller-local PID (Bash dynamic scope), then uses the
# real log readiness boundary. It does not replace receiver launch/kill/wait.
READY_PROBE = r'''
wait_for_text() {
  local attempt
  for ((attempt=0; attempt<300; attempt++)); do
    if command grep -q "$2" "$1" 2>/dev/null; then
      printf '%s\n' "$recv_pid" > "$PROBE_DIR/tracked-receiver.pid"
      printf '%s\n' "$PIDS_KIDS" > "$PROBE_DIR/tracked-children"
      return 0
    fi
    command sleep 0.01
  done
  return 1
}
'''


class MatrixLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(BASH, "Bash is required for lifecycle regressions")
        self.tmp = tempfile.TemporaryDirectory(prefix="ak-matrix-lifecycle-")
        self.base = Path(self.tmp.name).resolve()
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self.kill_fixture_processes)
        (self.base / "bin").mkdir()
        for name, script in (("ip", IP_FIXTURE), ("cli-fixture", CLI_FIXTURE), ("sleep", SLEEP_FIXTURE)):
            path = self.base / "bin" / name
            path.write_text(script, encoding="utf-8", newline="\n")
            path.chmod(0o755)
        text = MATRIX.read_text(encoding="utf-8")
        # Evaluate existing production definitions, without running the privileged
        # entrypoint. This is execution, not source-text assertions. New helpers
        # in the two owned sections are loaded automatically too.
        lifecycle = text[text.index('PIDS_REG='):text.index('# 3. Topology builders.')]
        topology = text[text.index('make_ns()'):text.index('# 4. Services:')]
        cases = text[text.index('fail_case()'):text.index('# 7. The matrix.')]
        self.definitions = PREAMBLE + lifecycle + topology + cases

    def kill_fixture_processes(self):
        # Read only PIDs our test recorded. No pkill, name matching or host ns.
        tokens = []
        for path in self.base.glob("*.pid*"):
            tokens.extend(path.read_text(encoding="utf-8").split())
        tracked = self.base / "tracked-children"
        if tracked.exists():
            tokens.extend(tracked.read_text(encoding="utf-8").split())
        pids = sorted({token for token in tokens if token.isdecimal()})
        if pids:
            subprocess.run([BASH, "-c", 'kill -KILL "$@" 2>/dev/null || true',
                            "fixture-cleanup", *pids], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=5, check=False)

    def run_bash(self, body, *, environment=None, outer_input=None):
        script = self.definitions + READY_PROBE + body
        driver = self.base / "driver.sh"
        driver.write_text(script, encoding="utf-8", newline="\n")
        env = dict(os.environ)
        env.pop("BASH_ENV", None)
        env.update(environment or {})
        started = time.monotonic()
        log = self.base / "driver.log"
        input_path = self.base / "outer-stdin"
        input_path.write_text(outer_input or "", encoding="utf-8")
        timed_out = False
        with log.open("wb") as output, input_path.open("rb") as incoming:
            process = subprocess.Popen(
                [BASH, "--noprofile", "--norc", driver.as_posix(), self.base.as_posix()],
                cwd=self.base, env=env, stdin=incoming if outer_input is not None else subprocess.DEVNULL,
                stdout=output, stderr=subprocess.STDOUT,
            )
            try:
                code = process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                timed_out = True
                self.kill_fixture_processes()
                process.kill()
                code = process.wait(timeout=3)
        self.elapsed = time.monotonic() - started
        output_text = log.read_text(encoding="utf-8", errors="replace")
        self.assertFalse(timed_out, f"real Bash lifecycle exceeded 12s: {output_text}")
        result = subprocess.CompletedProcess(process.args, code, stdout=output_text)
        self.result = result
        return result

    def assert_no_tracked_survivors(self, output):
        self.assertNotIn("SURVIVOR=", output, output)
        events = self.base / "events"
        if events.exists():
            self.assertNotIn("DELETE_WITH_LIVE_PROCESS=", events.read_text(encoding="utf-8"))

    def test_receiver_pid_is_the_actual_command_not_a_function_wrapper(self):
        result = self.run_bash(r'''
run_case symm-symm fail
printf 'CASE_RC=%s\n' "$?"
''')
        self.assertEqual(result.returncode, 0, result.stdout)
        actual = (self.base / "receiver.pid").read_text().strip()
        tracked = (self.base / "tracked-receiver.pid").read_text().strip()
        self.assertEqual(tracked, actual, "TERM must target the actual receiver, not its parent shell")
        self.assertLess(self.elapsed, 3.5, "receiver wait must not wait for the 4s fixture's natural exit")

    def test_receiver_explicit_feed_is_independent_of_outer_stdin(self):
        result = self.run_bash(r'''
run_case symm-symm fail
printf 'CASE_RC=%s\n' "$?"
''', outer_input="unused outer input\n")
        actual = (self.base / "receiver.pid").read_text().strip()
        tracked = (self.base / "tracked-receiver.pid").read_text().strip()
        self.assertEqual(tracked, actual, result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_case_cleanup_reaps_receiver_feeder_and_sampler_before_next_case(self):
        result = self.run_bash(r'''
run_case symm-symm fail
stop_case_children || exit 93
for pid in $(<"$PROBE_DIR/tracked-children"); do
  kill -0 "$pid" 2>/dev/null && printf 'SURVIVOR=%s\n' "$pid"
done
printf 'NEXT_CASE_ALLOWED\n'
''')
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("NEXT_CASE_ALLOWED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_decline_feeder_waiting_for_a_prompt_does_not_delay_reaping(self):
        result = self.run_bash(r'''
run_case decline-retry-40s fail '' '' decline-first
stop_case_children || exit 93
for pid in $(<"$PROBE_DIR/tracked-children"); do
  kill -0 "$pid" 2>/dev/null && printf 'SURVIVOR=%s\n' "$pid"
done
printf 'CASE_REAPED\n'
''')
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("CASE_REAPED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_term_ignoring_receiver_is_killed_and_reaped_within_bound(self):
        result = self.run_bash(r'''
run_case symm-symm fail
stop_case_children || exit 93
read -r pid < "$PROBE_DIR/receiver.pid"
kill -0 "$pid" 2>/dev/null && printf 'SURVIVOR=%s\n' "$pid"
printf 'STUBBORN_REAPED\n'
''', environment={"PROBE_RECEIVER": "stubborn"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("STUBBORN_REAPED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_term_cleans_once_and_never_executes_the_next_case(self):
        result = self.run_bash(r'''
command sleep 4 &
PIDS_KIDS="$!"
printf '%s\n' "$!" > "$PROBE_DIR/signal-child.pid"
kill -TERM "$BASHPID"
printf 'NEXT_CASE_EXECUTED\n'
''')
        self.assertEqual(result.returncode, 143, result.stdout)
        self.assertNotIn("NEXT_CASE_EXECUTED", result.stdout)
        events = self.base / "events"
        if events.exists():
            deletes = events.read_text().splitlines()
            self.assertEqual(len(deletes), len(set(deletes)), "signal and EXIT must not clean twice")

    def test_int_cleans_and_never_executes_the_next_case(self):
        result = self.run_bash(r'''
command sleep 4 &
PIDS_KIDS="$!"
printf '%s\n' "$!" > "$PROBE_DIR/signal-child.pid"
kill -INT "$BASHPID"
printf 'NEXT_CASE_EXECUTED\n'
''')
        self.assertEqual(result.returncode, 130, result.stdout)
        self.assertNotIn("NEXT_CASE_EXECUTED", result.stdout)

    def test_core_creation_failure_never_configures_a_foreign_namespace(self):
        result = self.run_bash(r'''
ip() {
  if [[ "$*" == 'netns add akcore' ]]; then return 1; fi
  printf 'UNEXPECTED_MUTATION=%s\n' "$*"
  return 0
}
if build_core; then printf 'CORE_SUCCEEDED\n'; else printf 'CORE_REFUSED\n'; fi
''')
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("CORE_REFUSED", result.stdout)
        self.assertNotIn("UNEXPECTED_MUTATION=", result.stdout)
        self.assertNotIn("CORE_SUCCEEDED", result.stdout)

    def test_case_cleanup_reaps_a_running_sampler_command(self):
        result = self.run_bash(r'''
run_case symm-symm fail
stop_case_children || exit 93
for pid in $(<"$PROBE_DIR/sampler-command.pids"); do
  kill -0 "$pid" 2>/dev/null && printf 'SURVIVOR=%s\n' "$pid"
done
printf 'SAMPLER_COMMAND_REAPED\n'
''', environment={"PROBE_SAMPLER_SECONDS": "4", "PROBE_SAMPLER_READY": "sampler-command.pids"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("SAMPLER_COMMAND_REAPED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_case_cleanup_reaps_the_sampler_sleep(self):
        # The PATH sleep fixture execs a real sleeper and records its PID;
        # ordinary poll sleeps remain fast, without hiding sampler children.
        result = self.run_bash(r'''
run_case symm-symm fail
stop_case_children || exit 93
for pid in $(<"$PROBE_DIR/sampler-sleep.pids"); do
  kill -0 "$pid" 2>/dev/null && printf 'SURVIVOR=%s\n' "$pid"
done
printf 'SAMPLER_SLEEP_REAPED\n'
''', environment={"PROBE_SAMPLER_READY": "sampler-sleep.pids"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("SAMPLER_SLEEP_REAPED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        self.assertLess(self.elapsed, 3.5, result.stdout)

    def test_namespace_delete_happens_only_after_owned_children_exit(self):
        result = self.run_bash(r'''
command sleep 4 &
PIDS_KIDS="$!"
printf '%s\n' "$!" > "$PROBE_DIR/receiver.pid"
OWNED_NS=(akalice)
cleanup || exit 94
printf 'CLEANUP_FINISHED\n'
''')
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("CLEANUP_FINISHED", result.stdout)
        self.assert_no_tracked_survivors(result.stdout)
        events = (self.base / "events").read_text(encoding="utf-8")
        self.assertEqual(events.count("DELETE=akalice"), 1)
        self.assertNotIn("DELETE=aksvc", events, "never delete a namespace this invocation did not create")


if __name__ == "__main__":
    unittest.main()
