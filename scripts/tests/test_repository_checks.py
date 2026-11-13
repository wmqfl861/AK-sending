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


class ReviewStatusTableTests(unittest.TestCase):
    """CR-P3-10: section-7 status table shape (columns + no 本提交)."""

    @staticmethod
    def _doc(*rows: str) -> str:
        return (
            "## 6. 门槛命令\n\n## 7. 状态表（执行 agent 每完成一项就更新）\n\n"
            "| 编号 | 批次 | 状态 | 提交 | 证据摘要 |\n"
            "|---|---|---|---|---|\n"
            + "\n".join(rows)
            + "\n\n## 8. 需所有者确认的事项\n"
        )

    def test_well_formed_rows_pass(self):
        for row in [
            "| CR-P1-01 | A | 已完成 | 3b3eb64 | CI 全绿 |",
            "| R36-P2-04 剩余 | E / G6 | 已完成 | 9029c08 | 服务端原因帧 |",
            "| E2/G4 | G4 | 已完成 | f2c5fc4 | 确认窗口 30s→120s |",
            # Blank 提交/证据摘要 cells are real columns, not stray pipes.
            "| CR-P2-08 | E | 待所有者（E1） | | |",
            "| CR-P2-12 | F0 | 未开始 | | 2026-11-12 新增 |",
        ]:
            with self.subTest(row=row):
                rows_checked, errors = checks.status_table_issues(self._doc(row))
                self.assertEqual((rows_checked, errors), (1, []))

    def test_six_column_row_fails(self):
        rows_checked, errors = checks.status_table_issues(
            self._doc("| CR-P1-01 | A | 已完成 | 3b3eb64 | CI 全绿 | 多余列 |")
        )
        self.assertEqual(rows_checked, 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("6 columns", errors[0])
        self.assertIn("expected 5", errors[0])

    def test_placeholder_row_fails(self):
        rows_checked, errors = checks.status_table_issues(
            self._doc("| CR-P3-09 | G1 | 代码完成，验收中 | 本提交 | 证据 |")
        )
        self.assertEqual(rows_checked, 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("本提交", errors[0])

    def test_escaped_pipe_keeps_five_columns(self):
        # CR-P3-02 shape: \| inside cell text is not a column separator.
        rows_checked, errors = checks.status_table_issues(
            self._doc(r'| CR-P3-02 | E | 已完成 | 44e4cbd | windows_incompatible（<>"\|?* 五字符） |')
        )
        self.assertEqual((rows_checked, errors), (1, []))

    def test_missing_section_fails_closed(self):
        rows_checked, errors = checks.status_table_issues("## 8. 其他\n\n| CR-P1-01 | A | 已完成 | x | y |\n")
        self.assertEqual(rows_checked, 0)
        self.assertEqual(len(errors), 1)
        self.assertIn("not found", errors[0])


if __name__ == "__main__":
    unittest.main()
