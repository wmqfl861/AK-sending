from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "transport_checks", Path(__file__).resolve().parents[1] / "check_repository.py")
assert spec is not None and spec.loader is not None
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class TransportManifestTests(unittest.TestCase):
    def baseline(self):
        return {"package": {"publish": False},
                "dependencies": deepcopy(checks.TRANSPORT_DEPENDENCIES),
                "dev-dependencies": deepcopy(checks.TRANSPORT_DEV_DEPENDENCIES)}

    def test_pinned_baseline(self):
        self.assertEqual(checks.validate_transport(self.baseline()), [])

    def test_default_features_cannot_enable_platform_trust(self):
        data = self.baseline()
        data["dependencies"]["quinn"]["default-features"] = True
        self.assertTrue(checks.validate_transport(data))

    def test_additional_runtime_dependency_is_rejected(self):
        data = self.baseline()
        data["dependencies"]["relay"] = "1"
        self.assertTrue(checks.validate_transport(data))

    def test_version_range_is_not_a_pin(self):
        data = self.baseline()
        data["dependencies"]["rustls"]["version"] = "0.23"
        self.assertTrue(checks.validate_transport(data))

    def test_conditional_dependency_and_build_script_rejected(self):
        for key in ("target", "build-dependencies", "features", "patch", "replace"):
            data = self.baseline()
            data[key] = {}
            self.assertTrue(checks.validate_transport(data))
        data = self.baseline()
        data["package"]["build"] = "build.rs"
        self.assertTrue(checks.validate_transport(data))
