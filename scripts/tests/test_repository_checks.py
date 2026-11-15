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


class StatusHandoffLinkTests(unittest.TestCase):
    """K-R6 (REVIEW_2026-11-14-batchK.zh-CN.md §1): CR-P3-20 gates anchor to
    the handoff docs/STATUS.zh-CN.md actually LINKS to, not the newest
    HANDOFF-BATCH-* file in docs/. On the reviewed commit STATUS still
    linked batch J while an unlinked batch K handoff existed, and the
    filename-based pick validated the wrong document.

    K2-R4 (REVIEW_2026-11-15-batchK2.zh-CN.md §1): the link is now resolved
    against STATUS's own directory (docs/) and the resolved canonical path
    must BE the newest handoff's canonical path — basename-only joining
    let STATUS link archive/<same-name> or an external same-basename URL
    while the checker validated the unlinked docs/ copy.

    K3 (REVIEW_2026-11-15-batchK3.zh-CN.md §1): EVERY current-batch link is
    validated — picking one candidate by max()/first-wins let a canonical
    link early in STATUS mask archive/ links in 最新验证/交接入口, making the
    result depend on link order. The run-number/contradiction gates anchor
    to the CANONICAL newest handoff.

    K4 (REVIEW_2026-11-15-batchK4.zh-CN.md §1): links are matched over the
    WHOLE document — the round-4 per-line scan silently skipped a link
    whose LABEL spans two lines, so a multi-line-label archive link passed
    the full checker. Optional CommonMark titles are parsed consistently
    by the shared MARKDOWN_LINK_RE (handoff gate and repo-wide walk).

    K5 (REVIEW_2026-11-15-batchK5.zh-CN.md §1): the shared regex also
    covers bare PARENTHESIZED titles (spec example 505) and legal
    whitespace/newlines between "(" and the destination (example 510) —
    an archive entry written in those forms used to bypass the complete
    checker with errors=[].

    K6 (REVIEW_2026-11-15-batchK6.zh-CN.md §1): titles may span LINES
    (only blank lines inside are forbidden, per the 0.31.2 link-title
    definition) — excluding newlines from quoted titles was a round-6
    regression that silently dropped a legal archive link. Entries the
    gate cannot parse at all (blank-line titles, links hidden inside
    another link's title) are explicitly rejected; angle-bracket
    destinations are unwrapped in both consumers.

    K7 (REVIEW_2026-11-15-batchK7.zh-CN.md §1): <…> destinations may
    contain SPACES (and newlines) — the angle form is parsed inside the
    shared destination grammar (post-hoc unwrapping of a
    whitespace-stopped token missed `<archive space/…>` entirely), the
    backstop uses the same grammar so its rejections carry the
    untruncated destination, and the wrapper is unwrapped before the
    fragment is stripped.
    """

    @staticmethod
    def _status(link_lines: str, latest_lines: str = "- 无新 run。",
                conclusion_lines: str = "- 阶段结论与链接。") -> str:
        # Layout used by the line-number assertions below: conclusion on
        # line 5, 最新验证 body on line 9, 交接入口 body on line 13.
        return (
            "# 项目状态\n\n## 当前状态\n\n" + conclusion_lines + "\n\n"
            "## 最新验证\n\n" + latest_lines + "\n\n## 交接入口\n\n" + link_lines + "\n"
        )

    def _write(self, root: Path, name: str, text: str) -> None:
        (root / "docs").mkdir(parents=True, exist_ok=True)
        (root / "docs" / name).write_text(text, encoding="utf-8")

    def test_linked_handoff_picks_newest_link_and_strips_anchors(self):
        # The real STATUS shape: current handoff linked with a §5 anchor,
        # previous batch kept as history, non-batch handoff docs ignored.
        # The anchor link and the bare link resolve to the same local file.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · "
                "[K 交接 §5](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#5-证据表) · "
                "[J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md) · "
                "[U1 清单](HANDOFF-CLIENT-UI-U1-2026-11-13.zh-CN.md) · "
                "[HANDOFF](HANDOFF.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_link_to_newest_handoff_passes(self):
        # (a) STATUS links K, K is the newest file (J kept as history) →
        # resolved to K with no errors.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · "
                "[J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_stale_link_while_newer_handoff_exists_fails(self):
        # (b) Codex's counterexample: STATUS still links J while a newer K
        # handoff sits unlinked in docs/. The error names both files. K3:
        # the returned path is the CANONICAL newest handoff — the
        # run-number/contradiction gates anchor to it, and the stale-link
        # error itself fails the check.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status("- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)")
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1)
            self.assertIn("HANDOFF-BATCH-J-2026-11-13.zh-CN.md", errors[0])
            self.assertIn("HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("update the STATUS link", errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_link_to_missing_handoff_file_fails(self):
        # (c) STATUS links a handoff filename of an OLDER batch that does
        # not exist in docs/ → two fail-closed errors: the missing target
        # AND "no link to the current batch". The canonical newest file is
        # still returned so the run-number gate keeps running.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status("- [K 交接](HANDOFF-BATCH-K-2026-11-13.zh-CN.md)")
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 2)
            self.assertTrue(any("does not exist" in e for e in errors), errors)
            self.assertTrue(any("HANDOFF-BATCH-K-2026-11-13.zh-CN.md" in e for e in errors), errors)
            self.assertTrue(any("current handoff" in e for e in errors), errors)
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def _write_handoff_pair(self, root: Path) -> None:
        """docs/ + archive/ copies of the SAME handoff name (K2-R4 repro)."""
        self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
        archive = root / "docs" / "archive"
        archive.mkdir(parents=True, exist_ok=True)
        (archive / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md").write_text(
            "# 交接：批次 K（archive 副本）\n", encoding="utf-8"
        )

    def test_same_name_different_directory_link_fails(self):
        # K2-R4 reproduction (a): STATUS links archive/<same-name> while a
        # separate real docs/<same-name> also exists. The basename-only
        # resolution validated the unlinked docs/ copy; the resolved path
        # must instead BE the newest handoff file. K3: the CANONICAL docs/
        # file is returned (anchoring never follows the stray link), and
        # the error names the archive link with its STATUS line number.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            status = self._status(
                "- [K 交接](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1)
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("update the STATUS link", errors[0])
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:13:"), errors[0])
            self.assertEqual(
                path, root / "docs" / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
            )

    def test_external_same_basename_url_fails_distinctly(self):
        # K2-R4 reproduction (b): an external URL with the handoff basename
        # was accepted as the local handoff link by the OLD specialized
        # gate AND by the OLD full checker (the general link walk skips
        # external URLs, so nothing else caught it). External URLs can
        # never satisfy the current-handoff anchoring — reported
        # distinctly, with the STATUS line number.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](https://example.com/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1)
            self.assertIn("external URL", errors[0])
            self.assertIn("https://example.com/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:13:"), errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_link_resolving_to_missing_file_fails(self):
        # K2-R4 regression, claim qualified per the K3 review: the OLD
        # SPECIALIZED gate redirected this link onto the existing docs/
        # basename and passed, but the OLD FULL checker already rejected it
        # through the general broken-link walk. The improvement asserted
        # here is the SPECIFIC failure mode: the handoff gate itself names
        # the resolved path (docs/missing/…) with its STATUS line number,
        # instead of relying on the generic walk.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](missing/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1)
            self.assertIn("missing/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("does not exist", errors[0])
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:13:"), errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_absolute_and_escaping_handoff_links_fail(self):
        # K2-R4: absolute jumps and ".." escapes out of docs/ are rejected
        # outright instead of collapsing onto the docs/ basename. The
        # canonical handoff is still returned for the run-number gate.
        for link in (
            "/etc/HANDOFF-BATCH-K-2026-11-14.zh-CN.md",
            "C:/docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md",
            "../HANDOFF-BATCH-K-2026-11-14.zh-CN.md",
            "subdir/../../HANDOFF-BATCH-K-2026-11-14.zh-CN.md",
        ):
            with self.subTest(link=link):
                with tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
                    path, errors = checks.status_handoff_issues(
                        root, self._status(f"- [K 交接]({link})")
                    )
                    self.assertEqual(len(errors), 1)
                    self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_internal_dotdot_resolving_to_newest_handoff_passes(self):
        # Lexical normalization keeps links that stay INSIDE docs/ working:
        # subdir/.. collapses back to docs/<handoff>, which is the newest
        # file — no false failure from the escape rule.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](./subdir/../HANDOFF-BATCH-K-2026-11-14.zh-CN.md)"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_no_handoff_link_while_files_exist_fails(self):
        # (d) STATUS links no batch handoff at all while handoff files
        # exist → the newest handoff is unreachable from the status entry
        # point. The newest file is still returned so the run-number gate
        # keeps running (the missing-link error already fails the check).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status("- [HANDOFF](HANDOFF.zh-CN.md)")
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1)
            self.assertIn("no HANDOFF-BATCH", errors[0])
            self.assertIn("HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_no_handoff_files_keeps_fail_closed_error(self):
        # Pre-K-R6 behavior preserved: a repo with no handoff docs at all
        # still fails closed (the no-link error only applies when handoff
        # files exist to be linked).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir(parents=True, exist_ok=True)
            path, errors = checks.status_handoff_issues(
                root, self._status("- [HANDOFF](HANDOFF.zh-CN.md)")
            )
            self.assertIsNone(path)
            self.assertEqual(len(errors), 1)
            self.assertIn("no docs/HANDOFF-BATCH-", errors[0])

    def test_run_numbers_anchor_to_the_canonical_handoff(self):
        # K3 anchoring: with STATUS stale-linking J while newer K exists,
        # the stale-link error fails the check AND the run gate validates
        # against the canonical K — run ids that live only in the stale J
        # are flagged, because CR-P3-20 keeps STATUS runs in the CURRENT
        # handoff (under K-R6's link-anchoring this exact state passed the
        # run gate; K3 re-anchors it to the canonical doc).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n\n- 43439835214：十轮全部通过。\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)",
                latest_lines="- 十轮：run [43439835214](https://github.com/o/r/actions/runs/43439835214)。",
            )
            path, link_errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(link_errors), 1)
            self.assertIn("current handoff", link_errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")
            runs, errors = checks.handoff_run_number_issues(
                status, path.read_text(encoding="utf-8"), f"docs/{path.name}"
            )
            self.assertEqual(runs, 1)
            self.assertEqual(len(errors), 1)
            self.assertIn("43439835214", errors[0])
            self.assertIn("HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])

    def test_run_numbers_in_canonical_handoff_clean_despite_stale_link(self):
        # Complementary: the run ids live in the canonical K, so the run
        # gate is clean; the check still fails through the stale-link
        # error alone (drift is caught by the link gate, not fabricated
        # run errors).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n\n- §3.1 无新 run。\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n\n- 43439835214：十轮全部通过。\n")
            status = self._status(
                "- [J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)",
                latest_lines="- 十轮：run [43439835214](https://github.com/o/r/actions/runs/43439835214)。",
            )
            path, link_errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(link_errors), 1)
            self.assertIn("update the STATUS link", link_errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")
            runs, errors = checks.handoff_run_number_issues(
                status, path.read_text(encoding="utf-8"), f"docs/{path.name}"
            )
            self.assertEqual((runs, errors), (1, []))

    # -- K3 (REVIEW_2026-11-15-batchK3.zh-CN.md §1): every current-batch
    # link is checked — no first-candidate short-circuit, no order
    # dependence. STATUS layout from _status: conclusion link on line 5,
    # 最新验证 link on line 9, 交接入口 link on line 13.

    def test_canonical_first_archive_later_links_fail(self):
        # The reviewer's exact reproduction: the FIRST K link (当前状态)
        # points at the canonical docs/ file, the LATER links (最新验证 and
        # 交接入口) point at a real archive/ copy. Both later links are
        # reported with their own STATUS line numbers; anchoring stays on
        # the canonical doc.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            status = self._status(
                "- [K 交接](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)",
                latest_lines="- 见 [K 交接 §5](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md#5-证据表)。",
                conclusion_lines="- 阶段结论见 [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md)。",
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 2, errors)
            for error in errors:
                self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", error)
                self.assertIn("docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", error)
                self.assertIn("every current-batch link", error)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertTrue(errors[1].startswith(f"{checks.STATUS_DOC}:13:"), errors[1])
            self.assertEqual(
                path, root / "docs" / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
            )

    def test_archive_first_canonical_later_links_still_fail(self):
        # Order independence: the same link multiset reversed (archive
        # links first and last, canonical in the middle) fails identically
        # — under the pre-K3 max()/first-wins selection this order failed
        # only by accident of position, and the canonical-first order
        # passed.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            status = self._status(
                "- [K 交接](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)",
                latest_lines="- 见 [K 交接 §5](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#5-证据表)。",
                conclusion_lines="- 阶段结论见 [K 交接](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)。",
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 2, errors)
            lines = sorted(e.split(":", 2)[1] for e in errors)
            self.assertEqual(lines, ["13", "5"])
            for error in errors:
                self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", error)
                self.assertIn("every current-batch link", error)
            self.assertEqual(
                path, root / "docs" / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
            )

    def test_all_canonical_links_across_sections_pass(self):
        # The healthy shape: canonical current-batch links in every
        # section (当前状态/最新验证/交接入口) plus a history link — no
        # errors, canonical anchoring.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-J-2026-11-13.zh-CN.md", "# 交接：批次 J\n")
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md) · "
                "[J 交接](HANDOFF-BATCH-J-2026-11-13.zh-CN.md)",
                latest_lines="- 见 [K 交接 §5](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#5-证据表)。",
                conclusion_lines="- 阶段结论见 [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md)。",
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_external_url_in_any_section_fails_naming_it(self):
        # A single external current-batch link anywhere on the page (here:
        # 最新验证, with canonical links elsewhere) fails the check naming
        # the URL and its STATUS line — it cannot hide behind valid links.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                "- [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md)",
                latest_lines="- 见 [K 交接](https://example.com/HANDOFF-BATCH-K-2026-11-14.zh-CN.md)。",
                conclusion_lines="- 阶段结论见 [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md)。",
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("external URL", errors[0])
            self.assertIn("https://example.com/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_multiline_label_archive_link_fails_at_starting_line(self):
        # K4 (REVIEW_2026-11-15-batchK4.zh-CN.md §1), the reviewer's exact
        # fixture: the archive link's LABEL spans two lines (legal
        # Markdown), the canonical links stay single-line. The round-4
        # per-LINE scan never matched the multi-line label, so the archive
        # target passed the full checker (the 3db5259 whole-text max()
        # checker still rejected this fixture). Whole-text matching now
        # finds it and reports the link's STARTING line (5).
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = "[" + "K\n交接" + "](archive/" + name + ")"
        canonical = "[" + "K" + "](" + name + ")"
        status = (
            "# 项目状态\n\n## 当前状态\n\n" + archive
            + "\n\n## 最新验证\n\n" + canonical
            + "\n\n## 交接入口\n\n" + canonical + "\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:5:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("docs/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(
                path, root / "docs" / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
            )

    def test_multiline_label_canonical_link_passes(self):
        # The healthy counterpart: a multi-line LABEL pointing at the
        # canonical file is legal and must not trip anything.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        multiline = "[" + "K\n交接" + "](" + name + ")"
        status = (
            "# 项目状态\n\n## 当前状态\n\n" + multiline
            + "\n\n## 最新验证\n\n[" + "K" + "](" + name + ")\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, name, "# 交接：批次 K\n")
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, name)

    def test_titled_archive_link_fails(self):
        # CommonMark inline title `[label](target "title")` — flagged by
        # the K4 review as a form BOTH the old and round-4 regexes missed
        # entirely. The shared pattern now parses it consistently with
        # plain links, so a titled archive link fails naming the target.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            status = self._status(
                '- [K 交接](archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md "标题")'
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:13:"), errors[0])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    def test_titled_canonical_link_passes(self):
        # A titled link to the canonical handoff is valid Markdown and
        # must keep passing (single- and double-quoted titles).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
            status = self._status(
                '- [K 交接](HANDOFF-BATCH-K-2026-11-14.zh-CN.md "标题") · '
                "[K 交接 §5](HANDOFF-BATCH-K-2026-11-14.zh-CN.md#5-证据表 '另一标题')"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md")

    # -- K5 (REVIEW_2026-11-15-batchK5.zh-CN.md §1): the CommonMark
    # destination+title grammar the shared regex must support — bare
    # parenthesized titles (spec example 505) and whitespace/newlines
    # between "(" and the destination (example 510).

    def _k5_status(self, archive_link: str) -> str:
        """Reviewer's fixture shape: canonical 当前状态/交接入口, the
        varied archive link in 最新验证 (starting line 9)."""
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        canonical = chr(91) + "K](" + name + ")"
        return (
            "# 项目状态\n\n## 当前状态\n\n" + canonical
            + "\n\n## 最新验证\n\n" + archive_link
            + "\n\n## 交接入口\n\n" + canonical + "\n"
        )

    def test_paren_title_archive_link_fails(self):
        # The reviewer's exact fixture: a parenthesized (unquoted) title —
        # `[K](archive/<name> (handoff evidence))` — was missed by the
        # round-5 regex (and the repo-wide walk), letting an archive entry
        # pass the complete checker with errors=[].
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](archive/" + name + " (handoff evidence))"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(path.name, name)

    def test_newline_before_destination_archive_link_fails(self):
        # Spec example 510 form: a newline between "(" and the destination
        # is legal Markdown — the link still starts (and is reported) at
        # line 9.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](\n  archive/" + name + ")"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertEqual(path.name, name)

    def test_space_before_destination_archive_link_fails(self):
        # Spec example 510 form: a space between "(" and the destination.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K]( archive/" + name + ")"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertEqual(path.name, name)

    def test_canonical_variants_of_k5_forms_pass(self):
        # The healthy counterparts: canonical destinations written in the
        # new forms (paren title, newline before dest, space before dest,
        # whitespace around a quoted title) all keep passing.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        paren = chr(91) + "K](" + name + " (handoff))"
        newline = chr(91) + "K](\n  " + name + ")"
        space = chr(91) + "K]( " + name + ")"
        padded = chr(91) + 'K]( ' + name + ' "标题" )'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, name, "# 交接：批次 K\n")
            status = (
                "# 项目状态\n\n## 当前状态\n\n" + paren
                + "\n\n## 最新验证\n\n" + newline
                + "\n\n## 交接入口\n\n" + space + " · " + padded + "\n"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, name)

    # -- K6 (REVIEW_2026-11-15-batchK6.zh-CN.md §1): CommonMark 0.31.2
    # titles may span lines (only blank lines inside are forbidden). The
    # round-6 regex excluded newlines from quoted titles — a regression
    # that made a legal multiline-title archive link pass the complete
    # checker with errors=[]. Entries the gate cannot parse at all are
    # now rejected explicitly, never silently ignored.

    def test_multiline_dq_title_archive_link_fails(self):
        # The reviewer's exact fixture: a newline inside the double-quoted
        # title. The a8bb3b9 (round-5) checker rejected this; the 9c2d3bd
        # (round-6) checker silently ignored it.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](archive/" + name + ' "handoff\nevidence")'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(path.name, name)

    def test_multiline_sq_title_archive_link_fails(self):
        # Single-quoted multiline title reproduces identically.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](archive/" + name + " 'handoff\nevidence')"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(path.name, name)

    def test_multiline_paren_title_archive_link_fails(self):
        # Decision for parenthesized multiline titles: PARSE them like the
        # quoted forms (no blank lines, no parens inside) — the archive
        # entry gets the normal every-current-batch-link error.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](archive/" + name + " (handoff\nevidence))"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(path.name, name)

    def test_blank_line_title_archive_link_rejected(self):
        # Decision for titles containing a BLANK line (illegal per the
        # 0.31.2 link-title definition): the full pattern cannot parse
        # the entry, and the gate REJECTS it explicitly instead of
        # silently ignoring it.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](archive/" + name + ' "x\n\ny")'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("not a supported/parseable Markdown inline link", errors[0])
            self.assertEqual(path.name, name)

    def test_angle_bracket_archive_fails_and_canonical_passes(self):
        # Angle-bracket destinations are now unwrapped by both consumers:
        # <archive/…> fails like the bare form; <canonical> passes.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            archive = chr(91) + "K](<archive/" + name + ">)"
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertEqual(path.name, name)
            canonical = chr(91) + "K](<" + name + ">)"
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(canonical))
            self.assertEqual(errors, [])
            self.assertEqual(path.name, name)

    def test_link_hidden_inside_another_title_rejected(self):
        # Backstop: an archive link written INSIDE another link's parsed
        # title region cannot be matched by the full pattern on its own —
        # the gate reports the hidden entry instead of ignoring it.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_pair(root)
            hidden = (chr(91) + "K](" + name + ' "see '
                      + chr(91) + "J](archive/" + name + ') now")')
            status = (
                "# 项目状态\n\n## 当前状态\n\n" + chr(91) + "K](" + name + ")"
                + "\n\n## 最新验证\n\n" + hidden
                + "\n\n## 交接入口\n\n" + chr(91) + "K](" + name + ")\n"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("not a supported/parseable Markdown inline link", errors[0])
            self.assertEqual(path.name, name)

    def test_canonical_multiline_title_variants_pass(self):
        # Canonical destinations with multiline titles in all three forms
        # keep passing.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, name, "# 交接：批次 K\n")
            status = (
                "# 项目状态\n\n## 当前状态\n\n"
                + chr(91) + "K](" + name + ' "a\nb")'
                + "\n\n## 最新验证\n\n" + chr(91) + "K](" + name + " 'c\nd')"
                + "\n\n## 交接入口\n\n" + chr(91) + "K](" + name + " (e\nf))"
                + "\n"
            )
            path, errors = checks.status_handoff_issues(root, status)
            self.assertEqual(errors, [])
            self.assertEqual(path.name, name)

    # -- K7 (REVIEW_2026-11-15-batchK7.zh-CN.md §1): CommonMark permits
    # SPACES inside <…> destinations. Both the full pattern (bare class
    # stops at whitespace) and the backstop (truncated token "archive")
    # missed `[K](<archive space/…>)` entirely — the angle form is now
    # parsed inside the shared destination grammar, the backstop uses
    # the same grammar, and the wrapper is unwrapped before the fragment
    # is stripped.

    def _write_handoff_space_pair(self, root: Path) -> None:
        """docs/ canonical + a same-name copy under `docs/archive space/`."""
        self._write(root, "HANDOFF-BATCH-K-2026-11-14.zh-CN.md", "# 交接：批次 K\n")
        spaced = root / "docs" / "archive space"
        spaced.mkdir(parents=True, exist_ok=True)
        (spaced / "HANDOFF-BATCH-K-2026-11-14.zh-CN.md").write_text(
            "# 交接：批次 K（archive space 副本）\n", encoding="utf-8"
        )

    def test_angle_space_archive_link_fails(self):
        # The reviewer's exact fixture: an angle destination containing a
        # SPACE pointing at a real separate copy under docs/archive
        # space/. The full pattern now captures the destination whole and
        # fails at the entry's line, naming the resolved path.
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](<archive space/" + name + ">)"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_space_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("archive space/HANDOFF-BATCH-K-2026-11-14.zh-CN.md", errors[0])
            self.assertIn("every current-batch link", errors[0])
            self.assertEqual(path.name, name)

    def test_angle_canonical_and_fragment_pass(self):
        # Healthy counterparts: an angle-wrapped canonical destination
        # passes, and the wrapper is unwrapped BEFORE the fragment is
        # stripped — `<name#anchor>` resolves to name, not `<name` (the
        # boundary the K7 review noted on the walk).
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(root, name, "# 交接：批次 K\n")
            for link in (chr(91) + "K](<" + name + ">)",
                         chr(91) + "K](<" + name + "#5-证据表>)"):
                with self.subTest(link=link):
                    path, errors = checks.status_handoff_issues(
                        root, self._k5_status(link))
                    self.assertEqual(errors, [])
                    self.assertEqual(path.name, name)

    def test_angle_newline_destination_is_parsed_strictly(self):
        # A newline inside <…> is legal CommonMark whitespace, but it is
        # NOT a space: the resolved path contains a literal newline, no
        # such file exists, and the gate fails with a definite
        # does-not-exist error at the entry's line (previously the entry
        # was silently missed).
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](<archive\nspace/" + name + ">)"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_space_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("does not exist", errors[0])
            self.assertEqual(path.name, name)

    def test_backstop_rejects_angle_space_with_blank_line_title(self):
        # Backstop fidelity: a VALID angle-with-space destination plus an
        # unparseable (blank-line) title is rejected naming the
        # UNTRUNCATED destination — not "archive".
        name = "HANDOFF-BATCH-K-2026-11-14.zh-CN.md"
        archive = chr(91) + "K](<archive space/" + name + '> "x\n\ny")'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write_handoff_space_pair(root)
            path, errors = checks.status_handoff_issues(
                root, self._k5_status(archive))
            self.assertEqual(len(errors), 1, errors)
            self.assertTrue(errors[0].startswith(f"{checks.STATUS_DOC}:9:"), errors[0])
            self.assertIn("<archive space/HANDOFF-BATCH-K-2026-11-14.zh-CN.md>", errors[0])
            self.assertIn("not a supported/parseable Markdown inline link", errors[0])
            self.assertEqual(path.name, name)


if __name__ == "__main__":
    unittest.main()
