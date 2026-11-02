"""Tests for the Python architecture validator, not the Rust production model."""
from __future__ import annotations
from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest

script = Path(__file__).resolve().parents[1] / "check_repository.py"
spec = importlib.util.spec_from_file_location("repository_checks", script)
assert spec is not None and spec.loader is not None
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)

class ArchitectureConfigTests(unittest.TestCase):
    def test_known_baseline_is_valid(self):
        self.assertEqual(checks.validate_architecture(deepcopy(checks.EXPECTED)), [])

    def test_forbidden_features_are_rejected(self):
        for key in ["server_file_bytes_allowed", "relay_allowed", "turn_allowed",
                    "cloud_spooling_allowed", "file_metadata_via_server", "transcoding"]:
            with self.subTest(key=key):
                data = deepcopy(checks.EXPECTED)
                data[key] = True
                self.assertTrue(checks.validate_architecture(data))

    def test_lan_preference_cannot_be_disabled(self):
        data = deepcopy(checks.EXPECTED)
        data["lan_preferred"] = False
        self.assertTrue(checks.validate_architecture(data))

    def test_missing_keys_fail_closed(self):
        for key in checks.EXPECTED:
            with self.subTest(key=key):
                data = deepcopy(checks.EXPECTED)
                del data[key]
                self.assertTrue(checks.validate_architecture(data))

    def test_unknown_bypass_key_rejected(self):
        data = deepcopy(checks.EXPECTED)
        data["temporary_relay_fallback"] = True
        self.assertTrue(checks.validate_architecture(data))

    def test_integer_zero_is_not_false(self):
        data = deepcopy(checks.EXPECTED)
        data["relay_allowed"] = 0
        self.assertTrue(checks.validate_architecture(data))

    def test_wrong_container_rejected(self):
        for data in [None, [], "", 0]:
            with self.subTest(data=data):
                self.assertTrue(checks.validate_architecture(data))

    def test_compression_rejected(self):
        data = deepcopy(checks.EXPECTED)
        data["compression"] = "gzip"
        self.assertTrue(checks.validate_architecture(data))

    def test_control_plane_cannot_gain_blob_messages(self):
        data = deepcopy(checks.EXPECTED)
        data["control_plane_allowed"].append("file_blob")
        self.assertTrue(checks.validate_architecture(data))

    def test_relay_failure_action_rejected(self):
        data = deepcopy(checks.EXPECTED)
        data["direct_failure_action"] = "relay"
        self.assertTrue(checks.validate_architecture(data))

if __name__ == "__main__":
    unittest.main()
