"""Tests for the Python repository validator, not the Rust production model."""
from __future__ import annotations
from copy import deepcopy
import importlib.util
from pathlib import Path
import tempfile
import unittest

script = Path(__file__).resolve().parents[1] / "check_repository.py"
spec = importlib.util.spec_from_file_location("repository_checks", script)
assert spec is not None and spec.loader is not None
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class ArchitectureConfigTests(unittest.TestCase):
    def test_known_baseline_is_valid(self):
        self.assertEqual(checks.validate_architecture(deepcopy(checks.EXPECTED_ARCHITECTURE)), [])

    def test_forbidden_features_are_rejected(self):
        for key in ["server_file_bytes_allowed", "relay_allowed", "turn_allowed",
                    "cloud_spooling_allowed", "file_metadata_via_server", "transcoding"]:
            with self.subTest(key=key):
                data = deepcopy(checks.EXPECTED_ARCHITECTURE)
                data[key] = True
                self.assertTrue(checks.validate_architecture(data))

    def test_lan_preference_cannot_be_disabled(self):
        data = deepcopy(checks.EXPECTED_ARCHITECTURE)
        data["lan_preferred"] = False
        self.assertTrue(checks.validate_architecture(data))

    def test_missing_keys_fail_closed(self):
        for key in checks.EXPECTED_ARCHITECTURE:
            with self.subTest(key=key):
                data = deepcopy(checks.EXPECTED_ARCHITECTURE)
                del data[key]
                self.assertTrue(checks.validate_architecture(data))

    def test_unknown_bypass_key_rejected(self):
        data = deepcopy(checks.EXPECTED_ARCHITECTURE)
        data["temporary_relay_fallback"] = True
        self.assertTrue(checks.validate_architecture(data))

    def test_integer_zero_is_not_false(self):
        data = deepcopy(checks.EXPECTED_ARCHITECTURE)
        data["relay_allowed"] = 0
        self.assertTrue(checks.validate_architecture(data))

    def test_wrong_container_rejected(self):
        for data in [None, [], "", 0]:
            with self.subTest(data=data):
                self.assertTrue(checks.validate_architecture(data))

    def test_control_plane_cannot_gain_blob_messages(self):
        data = deepcopy(checks.EXPECTED_ARCHITECTURE)
        data["control_plane_allowed"].append("file_blob")
        self.assertTrue(checks.validate_architecture(data))

    def test_relay_failure_action_rejected(self):
        data = deepcopy(checks.EXPECTED_ARCHITECTURE)
        data["direct_failure_action"] = "relay"
        self.assertTrue(checks.validate_architecture(data))


class BannedDependencyTests(unittest.TestCase):
    @staticmethod
    def _write_lock(text: str) -> Path:
        fd, raw = tempfile.mkstemp(suffix=".lock")
        path = Path(raw)
        with open(fd, "w", encoding="utf-8") as fh:
            fh.write(text)
        return path

    def test_clean_lockfile_passes(self):
        path = self._write_lock('[[package]]\nname = "quinn"\nversion = "0.11.12"\n\n'
                                '[[package]]\nname = "blake3"\nversion = "1.5"\n')
        self.assertEqual(checks.banned_crates(path), [])

    def test_relay_turn_and_webrtc_crates_are_caught(self):
        for name in ["iroh", "iroh-relay", "turn", "rusturn", "turn-rs", "webrtc",
                     "str0m", "libdatachannel", "coturn"]:
            path = self._write_lock(f'[[package]]\nname = "{name}"\nversion = "0.1"\n')
            with self.subTest(name=name):
                self.assertEqual(checks.banned_crates(path), [name])

    def test_unreadable_lockfile_fails_closed(self):
        self.assertNotEqual(checks.banned_crates(Path("does-not-exist.lock")), [])


if __name__ == "__main__":
    unittest.main()
