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
"""
from __future__ import annotations

import io
from pathlib import Path
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


def extract_helper(script: Path) -> str:
    """Return the ssh_helper.py source embedded in `script`'s heredoc."""
    text = script.read_text(encoding="utf-8")
    fn = text.index("write_ssh_helper() {")
    begin = text.index(HEREDOC_START, fn) + len(HEREDOC_START) + 1
    end = text.index(HEREDOC_END, begin)
    return text[begin:end]


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


def load_helper(script: Path, scenario) -> dict:
    """Exec the REAL embedded helper with paramiko faked and time on the
    scenario's fake clock (no real waiting, no network)."""
    FakeSSHClient.scenario = scenario
    namespace = {"__name__": "ssh_helper_under_test"}
    with mock.patch.dict(sys.modules, {"paramiko": fake_paramiko_module()}):
        exec(compile(extract_helper(script), str(script), "exec"), namespace)
    clock = scenario["clock"]
    namespace["time"] = types.SimpleNamespace(
        monotonic=clock.monotonic, sleep=clock.sleep
    )
    return namespace


def call_exec_mode(namespace, passfile: Path, with_exec_arg: bool):
    out_buffer, err_buffer = io.BytesIO(), io.BytesIO()
    argv = ["ssh_helper.py"] + (["exec"] if with_exec_arg else [])
    argv += ["127.0.0.1", "22", "ubuntu", str(passfile)]
    with mock.patch.object(sys, "argv", argv), \
            mock.patch.object(sys, "stdin", io.StringIO("true")), \
            mock.patch.object(sys, "stdout", types.SimpleNamespace(buffer=out_buffer)), \
            mock.patch.object(sys, "stderr", types.SimpleNamespace(buffer=err_buffer)):
        status = namespace["main"]()
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

    def test_zero_timeout_immediate_read_raises_on_this_fake_channel(self):
        # Witness that the fake reproduces the reviewed failure mode: the OLD
        # pattern (exec_command(timeout=0) then an immediate read) raises,
        # which made both OLD helpers return 255. Run against the reverted
        # helper, test_delayed_output_completes... fails with status 255.
        scenario = self.scenario()
        client = FakeSSHClient()
        FakeSSHClient.scenario = scenario
        _, stdout, _ = client.exec_command("true", timeout=0)
        with self.assertRaises(TimeoutError):
            stdout.read()

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
            start = source.index("def run_command(")
            end = source.index("\ndef ", start + 1)
            return source[start:end]

        self.assertEqual(run_command_body(DEPLOY), run_command_body(CROSS),
                         "the embedded run_command must stay identical in both scripts")


if __name__ == "__main__":
    unittest.main()
