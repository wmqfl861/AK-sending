"""Runner contract tests with synthetic data; NEVER throughput/privilege evidence.

Only the OS/cargo boundary is mocked. The real Bash runner, JSON selection,
output validator, exit handling and EXIT/signal cleanup are exercised. No sudo,
network mutation, Rust compilation or external Python packages are needed.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/run_netem_benchmark.sh"
BASH = shutil.which("bash")
TEST_NAME = "netem_delay_50_100ms_256mib_loopback_throughput"
# Deliberately synthetic numbers, not measurements of AK-sending.
SAMPLE = """running 1 test
[netem summary] 268435456 bytes (8 x 32 MiB), single stream, engine-default chunk, in-flight window 32 (QUIC windows: CR-P2-04 8 MiB send/stream/connection):
shape                      wall    MiB/s      RTTs  vs baseline
no-netem               640.00ms    100.0        inf         1.00x
delay 50ms                1.60s     40.0       16.0         0.40x
delay 100ms               3.20s     20.0       16.0         0.20x
[netem bench] baseline host counters (tier delta): lo rx 0 B / 0 pkts (0 drops), lo tx 0 B / 0 pkts (0 drops); udp InErrors +0/RcvbufErrors +0/SndbufErrors +0
[netem bench] cleanup verified: dev lo carries no netem qdisc
test netem_delay_50_100ms_256mib_loopback_throughput ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.44s
"""

# Strict mocks reject unexpected commands, especially host-network mutations.
# State files survive the subshells used by pipelines and command substitution.
MOCKS = r'''
uname() { printf '%s\n' "${MOCK_OS:-Linux}"; }
id() { [[ "$*" == "-u" ]] || return 90; printf '1001\n'; }
rustc() { printf 'rustc 1.99.1 (synthetic fixture)\n'; }
git() { printf '1111111111111111111111111111111111111111\n'; }
readlink() {
  [[ "$*" == /proc/self/ns/net ]] || return 90
  printf 'net:[%s]\n' "${MOCK_IN_NS:-host}"
}
cargo() {
  if [[ "$*" == --version ]]; then printf 'cargo 1.99.1 (fixture)\n'; return; fi
  printf '%s\n' "$*" >> "$MOCK_ROOT/commands"
  [[ "$*" == 'test --locked --release -p ak-itests --test bench_netem --no-run --message-format=json-render-diagnostics' ]] || return 90
  command "$TEST_PYTHON" -I -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text(), end="")' "$MOCK_ROOT/artifacts.jsonl"
  return "${MOCK_BUILD_RC:-0}"
}
sudo() {
  [[ "$1" == -n ]] || return 90
  shift
  printf 'sudo %s\n' "$*" >> "$MOCK_ROOT/commands"
  if [[ "${MOCK_NO_SUDO:-0}" == 1 ]]; then return 1; fi
  case "$1" in
    true) return 0 ;;
    ip) shift; mock_ip "$@" ;;
    kill)
      [[ "$2" == -TERM || "$2" == -KILL ]] || return 90
      [[ "$3" == 987654 ]] || return 90
      if [[ "$2" == -KILL && "${MOCK_STUBBORN_PID:-0}" != 1 ]]; then
        rm -f "$MOCK_ROOT/pids"
      fi
      ;;
    *) return 90 ;;
  esac
}
mock_ip() {
  if [[ "$1" == -n ]]; then
    [[ "$*" == "-n $(<"$MOCK_ROOT/ns") link set lo up" ]] || return 90
    return 0
  fi
  [[ "$1" == netns ]] || return 90
  case "$2" in
    add)
      [[ ! -e "$MOCK_ROOT/ns" ]] || return 90
      printf '%s' "$3" > "$MOCK_ROOT/ns"
      ;;
    exec)
      [[ "$3" == "$(<"$MOCK_ROOT/ns")" ]] || return 90
      shift 3
      (export MOCK_IN_NS=isolated; "$@")
      ;;
    pids)
      [[ "$3" == "$(<"$MOCK_ROOT/ns")" ]] || return 90
      if [[ -f "$MOCK_ROOT/pids" ]]; then printf '987654\n'; fi
      ;;
    delete)
      [[ "$3" == "$(<"$MOCK_ROOT/ns")" ]] || return 90
      [[ "${MOCK_DELETE_FAIL:-0}" != 1 ]] || return 1
      rm -f "$MOCK_ROOT/ns"
      ;;
    list)
      if [[ -f "$MOCK_ROOT/ns" ]]; then printf '%s\n' "$(<"$MOCK_ROOT/ns")"; fi
      ;;
    *) return 90 ;;
  esac
}
ip() { [[ "$*" == -V ]] || return 90; printf 'ip synthetic\n'; }
tc() {
  if [[ "$*" == -V ]]; then printf 'tc synthetic\n'; return; fi
  [[ "${MOCK_IN_NS:-}" == isolated ]] || { printf 'HOST TC MUTATION REJECTED\n' >&2; return 90; }
  printf 'namespace tc %s\n' "$*" >> "$MOCK_ROOT/commands"
  case "$*" in
    'qdisc add dev lo root netem delay 1ms')
      [[ "${MOCK_NO_NETEM:-0}" != 1 ]] || return 1
      printf 'netem' > "$MOCK_ROOT/qdisc"
      ;;
    'qdisc show dev lo')
      if [[ -f "$MOCK_ROOT/qdisc" ]]; then printf 'qdisc netem 8001: root delay 1ms\n';
      else printf 'qdisc noqueue 0: root\n'; fi
      ;;
    'qdisc del dev lo root'|'qdisc del dev lo root netem')
      [[ "${MOCK_QDISC_STUCK:-0}" != 1 || ! -f "$MOCK_ROOT/ran" ]] || return 1
      rm -f "$MOCK_ROOT/qdisc"
      ;;
    *) return 90 ;;
  esac
}
timeout() {
  [[ "$*" == "--signal=TERM --kill-after=15s 2100s "* ]] || return 90
  shift 3
  printf 'benchmark %s\n' "$*" >> "$MOCK_ROOT/commands"
  [[ "$*" == "$MOCK_EXE --exact netem_delay_50_100ms_256mib_loopback_throughput --ignored --nocapture --test-threads=1" ]] || return 90
  : > "$MOCK_ROOT/ran"
  : > "$MOCK_ROOT/qdisc"
  : > "$MOCK_ROOT/pids"
  command "$TEST_PYTHON" -I -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text(), end="")' "$MOCK_ROOT/bench.log"
  if [[ "${MOCK_SIGNAL:-0}" == 1 ]]; then builtin kill -TERM "$MOCK_MAIN_PID"; return 143; fi
  return "${MOCK_BENCH_RC:-0}"
}
tee() {
  command tee "$@"
  if [[ "${MOCK_TEE_FAIL:-0}" == 1 && "$*" == *benchmark.log* ]]; then return 74; fi
}
sleep() { :; }
export MOCK_MAIN_PID=$BASHPID
'''


class RunnerTests(unittest.TestCase):
    def test_exact_name_matches_the_bench_source_and_runner(self):
        # aae55db lesson (netem run 43410016378): the runner invokes the
        # bench with `--exact NAME`; the 64->256 MiB payload rename left
        # the runner selecting ZERO tests and (by design) failing the
        # workflow as NOT-MEASURED in step 6 -- while THIS contract suite
        # stayed green, because it validated the script only against its
        # own mocks (constant + mock expectation drifted together). Pin
        # the triangle: Python constant <-> runner script <-> bench
        # source, so a future rename fails HERE, in step 5, instead.
        bench = (ROOT / "crates/ak-itests/tests/bench_netem.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            f"async fn {TEST_NAME}",
            bench,
            "the bench no longer defines TEST_NAME -- update the runner's "
            "--exact filter and this suite together",
        )
        runner = RUNNER.read_text(encoding="utf-8")
        self.assertIn(
            f"--exact {TEST_NAME}",
            runner,
            "the runner's --exact filter drifted from TEST_NAME",
        )

    def setUp(self):
        self.assertIsNotNone(BASH, "Bash is required for runner contract tests")
        self.assertTrue(RUNNER.is_file(), "the isolated netem runner has not been implemented")
        self.tmp = tempfile.TemporaryDirectory(prefix="ak-netem-contract-")
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name).resolve()
        self.exe = self.work / "bench_netem-exact fixture"
        self.exe.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
        self.exe.chmod(0o755)
        self.artifacts = self.work / "artifacts.jsonl"
        self.log = self.work / "bench.log"
        self.log.write_text(SAMPLE, encoding="utf-8")
        self.write_artifacts([self.artifact()])

    def artifact(self, **updates):
        message = {
            "reason": "compiler-artifact",
            "package_id": f"path+file://{ROOT.as_posix()}/crates/ak-itests#0.1.0",
            "manifest_path": (ROOT / "crates/ak-itests/Cargo.toml").as_posix(),
            "target": {"name": "bench_netem", "kind": ["test"],
                       "src_path": (ROOT / "crates/ak-itests/tests/bench_netem.rs").as_posix()},
            "profile": {"test": True, "opt_level": "3", "debug_assertions": False},
            "executable": self.exe.as_posix(),
            "filenames": [self.exe.as_posix()],
            "fresh": False,
        }
        message.update(updates)
        return message

    def write_artifacts(self, messages, success=True):
        messages = messages + [{"reason": "build-finished", "success": success}]
        self.artifacts.write_text("".join(json.dumps(m) + "\n" for m in messages), encoding="utf-8")

    def shell(self, body, *args, mock=False, env=None):
        environment = os.environ.copy()
        for name in ("GITHUB_SHA", "GITHUB_STEP_SUMMARY", "BASH_ENV", "ENV"):
            environment.pop(name, None)
        environment.update({"TEST_PYTHON": Path(sys.executable).as_posix(),
                            "MOCK_ROOT": self.work.as_posix(), "MOCK_EXE": self.exe.as_posix(),
                            "MSYS_NO_PATHCONV": "1"})
        environment.update(env or {})
        # Git Bash's /c/... paths need conversion for native Windows Python.
        # This adapter is only in the test harness, never in the Linux runner.
        prefix = r'''source "$1"; shift
python3() {
  local arg
  local converted=()
  for arg in "$@"; do
    if [[ ( "$OSTYPE" == msys* || "$OSTYPE" == cygwin* ) && "$arg" == /* ]]; then
      arg=$(cygpath -m "$arg")
    fi
    converted+=("$arg")
  done
  command "$TEST_PYTHON" "${converted[@]}"
}
'''
        if mock:
            prefix += MOCKS
        return subprocess.run([BASH, "-c", prefix + body, "contract-test", RUNNER.as_posix(),
                               *(str(arg) for arg in args)], env=environment,
                              text=True, encoding="utf-8", errors="replace",
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)

    def select(self):
        return self.shell('select_executable "$1" "$2"', self.artifacts.as_posix(), ROOT.as_posix())

    def validate(self, text):
        self.log.write_text(text, encoding="utf-8")
        return self.shell('validate_measurement "$1"', self.log.as_posix())

    def run_main(self, **env):
        out = self.work / "evidence"
        result = self.shell('main --output-dir "$1"', out.as_posix(), mock=True, env=env)
        summary = (out / "summary.md").read_text(encoding="utf-8") if (out / "summary.md").exists() else ""
        commands = (self.work / "commands").read_text(encoding="utf-8") if (self.work / "commands").exists() else ""
        return result, summary, commands

    def test_selects_exact_json_artifact_not_a_stale_filename(self):
        stale = self.work / "bench_netem-stale"
        stale.write_text("stale", encoding="utf-8")
        other = self.artifact(target={"name": "bench_other", "kind": ["test"]}, executable=stale.as_posix())
        self.write_artifacts([other, self.artifact()])
        result = self.select()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), self.exe.as_posix())

    def test_rejects_missing_ambiguous_wrong_package_or_non_release_artifacts(self):
        cases = [[], [self.artifact(), self.artifact()],
                 [self.artifact(manifest_path=(ROOT / "Cargo.toml").as_posix())],
                 [self.artifact(profile={"test": False, "opt_level": "0", "debug_assertions": True})],
                 [self.artifact(executable=None)],
                 [self.artifact(executable=(self.work / "missing").as_posix())]]
        for messages in cases:
            with self.subTest(messages=messages):
                self.write_artifacts(messages)
                self.assertNotEqual(self.select().returncode, 0)

    def test_rejects_malformed_or_failed_cargo_json(self):
        self.artifacts.write_text("not JSON\n", encoding="utf-8")
        self.assertNotEqual(self.select().returncode, 0)
        self.write_artifacts([self.artifact()], success=False)
        self.assertNotEqual(self.select().returncode, 0)

    def test_accepts_three_numeric_samples_and_preserves_units(self):
        result = self.validate(SAMPLE)
        self.assertEqual(result.returncode, 0, result.stderr)
        for value in ("640.00ms", "1.60s", "3.20s", "100.0", "40.0", "20.0", "MiB/s"):
            self.assertIn(value, result.stdout)

    def test_rejects_skip_even_with_three_numeric_samples(self):
        result = self.validate(SAMPLE + "[netem bench] SKIP: fixture\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SKIP", result.stderr)

    def test_rejects_incomplete_duplicate_non_numeric_and_unverified_output(self):
        cases = [SAMPLE.replace("delay 100ms               3.20s     20.0       16.0         0.20x\n", ""),
                 SAMPLE + "delay 50ms                1.60s     40.0       16.0         0.40x\n",
                 SAMPLE.replace("40.0", "NaN"), SAMPLE.replace("40.0", "0.0"),
                 SAMPLE.replace("1.60s", "1.60"),
                 SAMPLE.replace("[netem summary]", "[different summary]"),
                 SAMPLE.replace("[netem bench] cleanup verified: dev lo carries no netem qdisc", ""),
                 SAMPLE.replace("1 passed", "0 passed")]
        for index, text in enumerate(cases):
            with self.subTest(index=index):
                self.assertNotEqual(self.validate(text).returncode, 0)

    def test_mocked_run_requires_measured_output_and_verified_namespace_cleanup(self):
        result, summary, commands = self.run_main()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Status: **MEASURED**", summary)
        self.assertIn("Namespace cleanup: VERIFIED", summary)
        self.assertIn("Benchmark exit code: 0", summary)
        self.assertIn("Cargo.lock SHA-256", summary)
        self.assertIn("rustc 1.99.1", summary)
        self.assertIn("tc synthetic", summary)
        self.assertIn("1111111111111111111111111111111111111111", summary)
        self.assertFalse((self.work / "ns").exists())
        self.assertFalse((self.work / "pids").exists())
        self.assertFalse((self.work / "qdisc").exists())
        self.assertIn("sudo kill -KILL 987654", commands)
        self.assertNotIn("sudo cargo", commands)
        self.assertNotIn("HOST TC", result.stderr)

    def test_non_linux_is_not_measured_and_does_not_build(self):
        result, summary, commands = self.run_main(MOCK_OS="MINGW64_NT")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NOT-MEASURED", summary)
        self.assertNotIn("test --locked", commands)

    def test_missing_passwordless_sudo_is_not_measured(self):
        result, summary, _ = self.run_main(MOCK_NO_SUDO="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NOT-MEASURED", summary)
        self.assertFalse((self.work / "ran").exists())

    def test_missing_netem_is_not_measured_and_namespace_is_removed(self):
        result, summary, _ = self.run_main(MOCK_NO_NETEM="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NOT-MEASURED", summary)
        self.assertFalse((self.work / "ran").exists())
        self.assertFalse((self.work / "ns").exists())

    def test_cargo_exit_is_not_hidden_by_tee(self):
        result, summary, _ = self.run_main(MOCK_BUILD_RC="101")
        self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
        self.assertIn("Build exit code: 101", summary)
        self.assertNotIn("Status: **MEASURED**", summary)
        self.assertFalse((self.work / "ran").exists())

    def test_benchmark_failure_is_preserved_even_after_three_samples(self):
        result, summary, _ = self.run_main(MOCK_BENCH_RC="101")
        self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
        self.assertIn("Benchmark exit code: 101", summary)
        self.assertIn("Status: **FAILED**", summary)
        self.assertIn("Namespace cleanup: VERIFIED", summary)
        self.assertFalse((self.work / "ns").exists())

    def test_zero_exit_skip_is_ci_failure(self):
        self.log.write_text("[netem bench] SKIP: synthetic missing capability\n", encoding="utf-8")
        result, summary, _ = self.run_main()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NOT-MEASURED", summary)
        self.assertIn("Benchmark exit code: 0", summary)
        self.assertFalse((self.work / "ns").exists())

    def test_failed_log_capture_cannot_report_measured(self):
        result, summary, _ = self.run_main(MOCK_TEE_FAIL="1")
        self.assertEqual(result.returncode, 74, result.stdout + result.stderr)
        self.assertIn("Benchmark exit code: 0", summary)
        self.assertNotIn("Status: **MEASURED**", summary)
        self.assertFalse((self.work / "ns").exists())

    def test_cleanup_failure_cannot_report_measured_but_still_deletes_namespace(self):
        result, summary, _ = self.run_main(MOCK_QDISC_STUCK="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Namespace cleanup: FAILED", summary)
        self.assertNotIn("Status: **MEASURED**", summary)
        self.assertFalse((self.work / "ns").exists())

    def test_process_cleanup_failure_still_attempts_namespace_deletion(self):
        result, summary, commands = self.run_main(MOCK_STUBBORN_PID="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Namespace cleanup: FAILED", summary)
        self.assertIn("netns delete", commands)

    def test_namespace_delete_failure_is_not_silenced(self):
        result, summary, _ = self.run_main(MOCK_DELETE_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Namespace cleanup: FAILED", summary)
        self.assertTrue((self.work / "ns").exists())

    def test_test_failure_exit_wins_over_cleanup_failure(self):
        result, summary, _ = self.run_main(MOCK_BENCH_RC="101", MOCK_QDISC_STUCK="1")
        self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
        self.assertIn("Namespace cleanup: FAILED", summary)

    def test_timeout_is_failure_with_cleanup(self):
        result, summary, _ = self.run_main(MOCK_BENCH_RC="124")
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertIn("Namespace cleanup: VERIFIED", summary)
        self.assertNotIn("Status: **MEASURED**", summary)

    def test_term_trap_cleans_namespace_and_does_not_pass(self):
        result, summary, _ = self.run_main(MOCK_SIGNAL="1")
        self.assertEqual(result.returncode, 143, result.stdout + result.stderr)
        self.assertIn("Namespace cleanup: VERIFIED", summary)
        self.assertNotIn("Status: **MEASURED**", summary)
        self.assertFalse((self.work / "ns").exists())


if __name__ == "__main__":
    unittest.main()
