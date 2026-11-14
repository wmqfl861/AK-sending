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
        # H0 (CR-P3-11) commit-prefix gate: "本提交" trips BOTH the
        # placeholder check and the non-hash check — assert both errors.
        rows_checked, errors = checks.status_table_issues(
            self._doc("| CR-P3-09 | G1 | 代码完成，验收中 | 本提交 | 证据 |")
        )
        self.assertEqual(rows_checked, 1)
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("本提交" in e for e in errors), errors)
        self.assertTrue(any("non-hash" in e for e in errors), errors)

    def test_non_hash_commit_column_fails(self):
        # H0 (CR-P3-11): "G6 提交"-style prose placeholders in the commit
        # column are rejected even without the literal "本提交" marker.
        rows_checked, errors = checks.status_table_issues(
            self._doc("| CR-P3-11 | H0 | 已完成 | G6 提交 | CI 绿 |")
        )
        self.assertEqual(rows_checked, 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("non-hash", errors[0])

    def test_annotated_hash_commit_column_passes(self):
        # Annotated hashes ("09d4465（服务端）") are real hashes with a
        # provenance note — startswith-hex keeps them valid (the gate's
        # stated purpose is catching placeholders, not annotations).
        rows_checked, errors = checks.status_table_issues(
            self._doc("| R36-P2-04 | G6 | 已完成 | 09d4465（服务端） | 原因帧 |")
        )
        self.assertEqual((rows_checked, errors), (1, []))

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


class HandoffConsistencyTests(unittest.TestCase):
    """CR-P3-20: STATUS "最新验证" run ids vs the newest HANDOFF-BATCH doc.

    Every test calls the real check functions on real (temporary) files or
    the real text-parsing helpers — nothing is mocked.
    """

    HANDOFF = "docs/HANDOFF-BATCH-J-2026-11-13.zh-CN.md"

    @staticmethod
    def _status(latest_lines: str) -> str:
        return (
            "# 项目状态\n\n## 当前状态\n\n- 阶段结论与链接。\n\n"
            "## 最新验证\n\n" + latest_lines + "\n\n## 交接入口\n\n- 链接。\n"
        )

    def _write(self, root: Path, name: str, text: str) -> None:
        (root / "docs").mkdir(parents=True, exist_ok=True)
        (root / "docs" / name).write_text(text, encoding="utf-8")

    def test_latest_handoff_picks_newest_date_then_letter(self):
        # Real temp files: newest date wins; within one date the later batch
        # letter wins (G/H/J all 2026-11-13 → J).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-F-2026-11-12.zh-CN.md", "F")
            self._write(root, "HANDOFF-BATCH-G-2026-11-13.zh-CN.md", "G")
            self._write(root, "HANDOFF-BATCH-H-2026-11-13.zh-CN.md", "H")
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "J")
            self.assertEqual(
                checks.latest_handoff(root).name, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md"
            )

    def test_missing_handoff_directory_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(checks.latest_handoff(Path(tmp)))

    def test_status_runs_present_and_no_contradiction_pass(self):
        status = self._status(
            "- NAT 十轮：run [43439835214](https://github.com/o/r/actions/runs/43439835214)，"
            "10/10 轮通过。\n- 普通 CI 43538159415 同 SHA 全绿。"
        )
        handoff = (
            "# 交接文档：批次 J\n\n"
            "## 3.1 run 表\n\n"
            "- 43439835214：十轮全部通过。\n"
            "- 43538159415：CI 全绿。\n"
        )
        runs_checked, errors = checks.handoff_run_number_issues(status, handoff, self.HANDOFF)
        self.assertEqual((runs_checked, errors), (2, []))
        self.assertEqual(checks.handoff_contradiction_issues(handoff, self.HANDOFF), [])

    def test_status_run_missing_from_handoff_fails(self):
        status = self._status("- main CI run [43558234099](https://github.com/o/r/actions/runs/43558234099) 全部成功。")
        handoff = "# 交接文档：批次 J\n\n- §3.1 只有 43439835214。\n"
        runs_checked, errors = checks.handoff_run_number_issues(status, handoff, self.HANDOFF)
        self.assertEqual(runs_checked, 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("43558234099", errors[0])
        self.assertIn(self.HANDOFF, errors[0])
        self.assertIn("最新验证", errors[0])

    def test_status_without_latest_verification_section_fails_closed(self):
        runs_checked, errors = checks.handoff_run_number_issues(
            "# 项目状态\n\n## 当前状态\n\n- 结论。\n", "# 交接文档：批次 J\n", self.HANDOFF
        )
        self.assertEqual(runs_checked, 0)
        self.assertEqual(len(errors), 1)
        self.assertIn("not found", errors[0])

    def test_confirmed_claim_plus_pending_word_fails(self):
        # The exact CR-P3-20 shape: §0 claims "一致性已核" while §4 still
        # says evidence is 尚待补齐. Error must cite both line numbers and
        # original excerpts for human review.
        handoff = (
            "# 交接文档：批次 J\n\n"
            "- **J6 三者一致性已核**：STATUS → §7 → 本文逐行对照完成。\n"
            "- 目前失败证据已开始回写，修复后 run 与对应提交尚待补齐。\n"
        )
        errors = checks.handoff_contradiction_issues(handoff, self.HANDOFF)
        self.assertEqual(len(errors), 1)
        self.assertIn("已核", errors[0])
        self.assertIn("尚待", errors[0])
        self.assertIn(f"{self.HANDOFF}:3:", errors[0])
        self.assertIn(f"{self.HANDOFF}:4:", errors[0])
        self.assertIn("一致性已核", errors[0])
        self.assertIn("尚待补齐", errors[0])

    def test_quoted_occurrences_do_not_trip(self):
        # Quoting speech or the "only write X later" instruction itself is
        # not a claim: both keywords sit inside quotes → no violation.
        handoff = (
            "# 交接文档：批次 J\n\n"
            "- 规则：核对完成后再写“三者一致性已核”，不提前宣称“尚待补齐”已解决。\n"
        )
        self.assertEqual(checks.handoff_contradiction_issues(handoff, self.HANDOFF), [])

    def test_pending_word_alone_does_not_trip(self):
        handoff = "# 交接文档：批次 J\n\n- §5：J1 视觉走查仍待执行。\n"
        self.assertEqual(checks.handoff_contradiction_issues(handoff, self.HANDOFF), [])


if __name__ == "__main__":
    unittest.main()
