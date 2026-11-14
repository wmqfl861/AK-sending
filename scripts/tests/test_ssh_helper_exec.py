"""K 复核 K-R3 regressions: execute the REAL paramiko helpers embedded in
scripts/deploy_linux.sh and scripts/cross_nat_test.sh against a fake paramiko
whose channel only replaces the network.

The fake channel reproduces the paramiko semantics the review relied on:
timeout=0 means NON-blocking (reads raise TimeoutError when data/EOF has not
arrived yet), timeout=None means blocking. With output delayed past the
exec_command() return, the OLD helper (timeout=0 + immediate read) exited
255 for both scripts; the current helpers must instead complete, drain BOTH
streams and return the remote exit status. An empty-output command must also
complete. The helpers stay single-sourced: the run_command body embedded in
both scripts must remain identical.

K2 复核 K2-R5 honesty rules for this suite:
- the old-pattern exit-255 behavior is asserted through the SAME harness as
  a negative control (test_old_zero_timeout_pattern_fails_on_this_harness),
  not by a fake-only witness that passed regardless of the helpers;
- the mocked stdio exposes BOTH the text interface (print diagnostics) and
  .buffer (byte output), so a helper diagnostic can never crash the mock
  and turn a behavioral result into a harness AttributeError;
- extraction/structure gaps surface as explicit assertion failures naming
  the missing piece, never as ValueError setup crashes.
"""
from __future__ import annotations

import io
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
DEPLOY = ROOT / "scripts/deploy_linux.sh"
CROSS = ROOT / "scripts/cross_nat_test.sh"
HEREDOC_START = "<<'PYEOF'"
HEREDOC_END = "\nPYEOF"
# The original batch-K baseline whose helpers are EXECUTED as the old-code
# proof (K 复核 K2-R5: run the old script's actual code, not a paraphrase).
BASE_REV = "f9582e5"

# The pre-fix helper pattern (f9582e5): exec_command(timeout=0) followed by
# an immediate stdout.read(). Kept verbatim in shape so the negative control
# exercises the reviewed old behavior, not a paraphrase of it.
OLD_PATTERN_HELPER = '''
import sys

import paramiko


def main() -> int:
    command = sys.stdin.read()
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(hostname="127.0.0.1", port=22, username="ubuntu",
                   password="dummy", look_for_keys=False, allow_agent=False)
    try:
        _, stdout, stderr = client.exec_command(command, timeout=0)
        out = stdout.read()
        err = stderr.read()
        status = stdout.channel.recv_exit_status()
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


def extract_helper_text(text: str, origin: str = "script") -> str:
    """Return the ssh_helper.py source embedded in a script's heredoc text."""
    for marker in ("write_ssh_helper() {", HEREDOC_START, HEREDOC_END):
        if marker not in text:
            raise AssertionError(
                f"{origin} embeds no ssh helper heredoc (missing {marker!r}) "
                "— nothing to behavior-test"
            )
    fn = text.index("write_ssh_helper() {")
    begin = text.index(HEREDOC_START, fn) + len(HEREDOC_START) + 1
    end = text.index(HEREDOC_END, begin)
    return text[begin:end]


def extract_helper(script: Path) -> str:
    """Return the ssh_helper.py source embedded in `script`'s heredoc."""
    return extract_helper_text(script.read_text(encoding="utf-8"), script.name)


class FakeClock:
    """Deterministic clock: sleep() advances fake time, nothing really waits."""

    def __init__(self):
        self.now = 1000.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FakeChannel:
    """Paramiko-like channel: data and the exit status appear only after
    `scenario["delay"]` fake seconds. timeout=0 reads raise TimeoutError
    exactly like paramiko's non-blocking mode (the reviewed failure mode)."""

    def __init__(self, scenario):
        self.scenario = scenario
        self.clock = scenario["clock"]
        self.ready_at = self.clock.now + scenario["delay"]
        self._out = list(scenario["out"])
        self._err = list(scenario["err"])
        self.timeout = None
        self.closed = False

    def _ready(self):
        return self.clock.now >= self.ready_at

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
            self.clock.sleep(0.05)

    def recv_stderr(self, size):
        while True:
            if self._err and self._ready():
                return self._err.pop(0)
            if self._ready():
                return b""
            if self.timeout == 0:
                raise TimeoutError("non-blocking recv_stderr with no data")
            self.clock.sleep(0.05)

    def recv_exit_status(self):
        while not self._ready():
            self.clock.sleep(0.05)
        return self.scenario["status"]


class FakeChanFile:
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


class FakeSSHClient:
    scenario = None
    last_timeout = "never-called"

    def set_missing_host_key_policy(self, policy):
        pass

    def connect(self, **kwargs):
        pass

    def get_transport(self):
        return None

    def exec_command(self, command, timeout=None):
        FakeSSHClient.last_timeout = timeout
        channel = FakeChannel(self.scenario)
        channel.settimeout(timeout)
        return None, FakeChanFile(channel, False), FakeChanFile(channel, True)

    def close(self):
        pass


def fake_paramiko_module():
    module = types.ModuleType("paramiko")
    module.SSHClient = FakeSSHClient
    module.AutoAddPolicy = lambda: None
    return module


def exec_helper_source(source: str, origin: str, scenario) -> dict:
    """Exec helper `source` with paramiko faked and time on the scenario's
    fake clock (no real waiting, no network)."""
    FakeSSHClient.scenario = scenario
    namespace = {"__name__": "ssh_helper_under_test"}
    with mock.patch.dict(sys.modules, {"paramiko": fake_paramiko_module()}):
        exec(compile(source, origin, "exec"), namespace)
    clock = scenario["clock"]
    namespace["time"] = types.SimpleNamespace(
        monotonic=clock.monotonic, sleep=clock.sleep
    )
    return namespace


def load_helper(script: Path, scenario) -> dict:
    """Exec the REAL embedded helper with paramiko faked and time on the
    scenario's fake clock (no real waiting, no network)."""
    return exec_helper_source(extract_helper(script), str(script), scenario)


def call_exec_mode(namespace, passfile: Path, with_exec_arg: bool):
    out_buffer, err_buffer = io.BytesIO(), io.BytesIO()
    argv = ["ssh_helper.py"] + (["exec"] if with_exec_arg else [])
    argv += ["127.0.0.1", "22", "ubuntu", str(passfile)]
    # TextIOWrapper over one BytesIO exposes BOTH the text interface
    # (print(..., file=sys.stderr) diagnostics) and .buffer (the helper's
    # byte output). K2-R5: a SimpleNamespace(buffer=...) mock crashed with
    # AttributeError the moment a helper printed a diagnostic, mixing mock
    # failures into old-code results instead of clean behavior failures.
    stdout = io.TextIOWrapper(out_buffer, encoding="utf-8")
    stderr = io.TextIOWrapper(err_buffer, encoding="utf-8")
    with mock.patch.object(sys, "argv", argv), \
            mock.patch.object(sys, "stdin", io.StringIO("true")), \
            mock.patch.object(sys, "stdout", stdout), \
            mock.patch.object(sys, "stderr", stderr):
        status = namespace["main"]()
        stdout.flush()
        stderr.flush()
    return status, out_buffer.getvalue(), err_buffer.getvalue()


class SshHelperDrainTests(unittest.TestCase):
    """Both embedded helpers, driven end-to-end through main()."""

    scripts = ((DEPLOY, True), (CROSS, False))  # (script, exec-mode arg?)

    def setUp(self):
        self.assertTrue(DEPLOY.is_file() and CROSS.is_file(),
                        "both scripts must exist to extract their helpers")
        tmp = tempfile.TemporaryDirectory(prefix="ak-ssh-helper-test-")
        self.addCleanup(tmp.cleanup)
        self.passfile = Path(tmp.name) / "passfile"
        self.passfile.write_text("dummy-password\n", encoding="utf-8")

    def scenario(self, **overrides):
        scenario = {
            "clock": FakeClock(),
            "delay": 5.0,  # fake seconds; far past exec_command()'s return
            "out": [b"delayed stdout\n"],
            "err": [b"delayed stderr\n"],
            "status": 7,
        }
        scenario.update(overrides)
        return scenario

    def test_delayed_output_completes_and_returns_remote_status(self):
        for script, with_exec_arg in self.scripts:
            with self.subTest(script=script.name):
                FakeSSHClient.last_timeout = "never-called"
                namespace = load_helper(script, self.scenario())
                status, out, err = call_exec_mode(
                    namespace, self.passfile, with_exec_arg)
                self.assertEqual(status, 7)
                self.assertEqual(out, b"delayed stdout\n")
                self.assertEqual(err, b"delayed stderr\n")
                # K-R3: the channel must be opened with BLOCKING semantics;
                # timeout=0 is non-blocking in paramiko and was the bug.
                self.assertIsNone(FakeSSHClient.last_timeout)

    def test_empty_output_command_completes_cleanly(self):
        for script, with_exec_arg in self.scripts:
            with self.subTest(script=script.name):
                namespace = load_helper(
                    script, self.scenario(out=[], err=[], status=0))
                status, out, err = call_exec_mode(
                    namespace, self.passfile, with_exec_arg)
                self.assertEqual(status, 0)
                self.assertEqual(out, b"")
                self.assertEqual(err, b"")

    def test_old_zero_timeout_pattern_fails_on_this_harness(self):
        # Negative control (K2-R5): drive the OLD pre-fix helper pattern —
        # exec_command(timeout=0) plus an immediate stdout.read() — through
        # the SAME fake-paramiko harness the real helpers are tested with.
        # It must exit 255 without transferring the delayed output. This
        # proves the harness reproduces the reviewed failure mode (so the
        # current helpers' passes are meaningful) and can itself fail: if
        # the fake channel stopped raising on timeout=0, the old pattern
        # would complete and this control would fail. It replaces a former
        # "witness" that exercised only the fake channel object and passed
        # regardless of which helper was embedded.
        namespace = exec_helper_source(
            OLD_PATTERN_HELPER, "old-pattern-helper.py", self.scenario()
        )
        status, out, err = call_exec_mode(
            namespace, self.passfile, with_exec_arg=False)
        self.assertEqual(status, 255)
        self.assertEqual(out, b"")
        self.assertIn(b"[ssh-helper] exec failed", err)

    def test_helpers_never_pass_a_zero_timeout(self):
        for script, _ in self.scripts:
            with self.subTest(script=script.name):
                # The call site, not the prose: exec_command(timeout=0) is
                # paramiko's NON-blocking mode and was the reviewed bug.
                self.assertNotIn("exec_command(command, timeout=0)",
                                 extract_helper(script))

    def test_run_command_is_single_sourced_between_both_scripts(self):
        def run_command_body(script):
            source = extract_helper(script)
            # K2-R5: a missing run_command is a behavioral gap (the K-R3
            # drain helper is absent), reported as a readable assertion —
            # not a ValueError setup crash mixed into the results.
            if "def run_command(" not in source:
                self.fail(
                    f"{script.name} embeds no run_command() — the K-R3 "
                    "blocking-drain helper is missing"
                )
            start = source.index("def run_command(")
            end = source.index("\ndef ", start + 1)
            return source[start:end]

        self.assertEqual(run_command_body(DEPLOY), run_command_body(CROSS),
                         "the embedded run_command must stay identical in both scripts")


class OldHelperFromGitTests(unittest.TestCase):
    """K 复核 K2-R5 old-code proof: EXECUTE the helper ACTUALLY embedded in
    the git baseline f9582e5 (extracted via `git show`), not a paraphrase.

    The inline OLD_PATTERN_HELPER negative control above proves the harness
    can fail; these tests prove the OLD SHIPPED CODE itself fails the same
    way (exit 255, no output transferred) on both the delayed-output and the
    empty-output scenarios, while the current helpers pass them.

    Skip scope is EXACTLY an unreadable git revision (K3 review §2:
    "missing markers do not universally skip"): when `git show` cannot
    produce the baseline script the contrast is skipped rather than
    fabricated, but a baseline that IS readable yet lacks the helper
    heredoc markers fails extraction with an AssertionError — it is never
    skipped."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="ak-ssh-old-helper-test-")
        self.addCleanup(tmp.cleanup)
        self.passfile = Path(tmp.name) / "passfile"
        self.passfile.write_text("dummy-password\n", encoding="utf-8")

    def baseline_helper(self, script: Path) -> str:
        text = git_show(BASE_REV, f"scripts/{script.name}")
        if not text:
            self.skipTest(f"git baseline {BASE_REV} not available locally; "
                          "cannot run the old-code contrast honestly")
        return extract_helper_text(text, f"{script.name}@{BASE_REV}")

    def scenario(self, **overrides):
        scenario = {
            "clock": FakeClock(),
            "delay": 5.0,
            "out": [b"delayed stdout\n"],
            "err": [b"delayed stderr\n"],
            "status": 7,
        }
        scenario.update(overrides)
        return scenario

    def test_baseline_helpers_fail_on_delayed_output_with_255(self):
        for script, with_exec_arg in ((DEPLOY, True), (CROSS, False)):
            with self.subTest(script=script.name):
                source = self.baseline_helper(script)
                # The baseline really shipped the reviewed buggy call.
                self.assertIn("exec_command(command, timeout=0)", source)
                namespace = exec_helper_source(
                    source, f"{script.name}@{BASE_REV}", self.scenario())
                status, out, err = call_exec_mode(
                    namespace, self.passfile, with_exec_arg)
                self.assertEqual(status, 255,
                                 "the baseline helper must exit 255 on delayed output")
                self.assertEqual(out, b"")
                self.assertIn(b"[ssh-helper]", err)

    def test_baseline_helpers_fail_on_empty_output_with_255(self):
        for script, with_exec_arg in ((DEPLOY, True), (CROSS, False)):
            with self.subTest(script=script.name):
                source = self.baseline_helper(script)
                namespace = exec_helper_source(
                    source, f"{script.name}@{BASE_REV}",
                    self.scenario(out=[], err=[], status=0))
                status, _, _ = call_exec_mode(
                    namespace, self.passfile, with_exec_arg)
                self.assertEqual(status, 255,
                                 "even an empty-output command failed through the "
                                 "baseline helper (timeout=0 + immediate read)")


if __name__ == "__main__":
    unittest.main()
