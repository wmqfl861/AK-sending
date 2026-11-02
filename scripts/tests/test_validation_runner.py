"""Test the evidence runner itself, not Rust or network behavior."""
from pathlib import Path
import importlib.util
import sys
import tempfile
import unittest
from unittest.mock import patch
import hashlib

path = Path(__file__).resolve().parents[1] / "run_validation.py"
spec = importlib.util.spec_from_file_location("validation", path)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class ValidationRunnerTests(unittest.TestCase):
    def run_case(self, code, timeout=10):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result = runner.run_one("case", [sys.executable, "-c", code], root, root, timeout)
            text = (root / "case.log").read_text(encoding="utf-8")
            return result, text

    def test_success_has_exit_code_and_evidence(self):
        result, text = self.run_case("print('synthetic success')")
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(len(result["log_sha256"]), 64)
        self.assertIn("synthetic success", text)

    def test_failure_is_not_masked(self):
        result, text = self.run_case("import sys; print('synthetic failure'); sys.exit(7)")
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["exit_code"], 7)
        self.assertIn("synthetic failure", text)

    def test_missing_tool_is_skipped_not_passed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result = runner.run_one("missing", [str(root / "missing-program")], root, root)
            self.assertEqual(result["status"], "skipped_missing_tool")
            self.assertIsNone(result["exit_code"])

    def test_timeout_is_recorded(self):
        result, text = self.run_case("import time; time.sleep(10)", 0.1)
        self.assertEqual(result["status"], "timed_out")
        self.assertIsNone(result["exit_code"])
        self.assertIn("timed out", text)

    def test_stderr_is_preserved(self):
        _, text = self.run_case("import sys; print('diagnostic', file=sys.stderr)")
        self.assertIn("diagnostic", text)

    def test_missing_failed_or_empty_suite_cannot_pass(self):
        self.assertFalse(runner.all_passed([]))
        for status in ["failed", "skipped_missing_tool", "timed_out", "execution_error"]:
            self.assertFalse(runner.all_passed([{"status": "passed"}, {"status": status}]))
        self.assertTrue(runner.all_passed([{"status": "passed"}]))

    def test_log_hash_streams_without_read_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            target = Path(temp) / "log.txt"
            data = b"synthetic log\n" * 100_000
            target.write_bytes(data)
            with patch.object(Path, "read_bytes", side_effect=AssertionError("unbounded read")):
                self.assertEqual(runner.file_sha256(target), hashlib.sha256(data).hexdigest())
