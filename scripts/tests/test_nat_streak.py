"""J5 acceptance tests: synthetic logs and mocked process boundaries, NOT NAT evidence.

The coordinator, parser, hashes and output files are real. Only Linux identity and
subprocesses (git/cargo/sudo/ip/timeout) are replaced: no privileges or network
changes are required. Fixtures deliberately include the symmetric expected-failure
PASS, which must never contribute to the forty target passes.
"""
from __future__ import annotations

import contextlib
import csv
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/run_nat_streak.py"
CASES = ("late-send-5s", "late-send-40s", "recv-no-lowttl", "decline-retry-40s")
NAMESPACES = ("akalice", "akbob", "akr1", "akr2", "akcore", "aksvc", "akinet")
SHA = "1" * 40
CLI_BYTES = b"synthetic CLI, never executed\n"
LOCK_BYTES = b"synthetic Cargo.lock\n"
FINAL = "[nat-matrix] RESULT: all runnable matrix cases PASS (incl. fixture self-checks)"
RETRY = "[retry-evidence] case=decline-retry-40s first=declined same_pairing=true delay_ms=40000 retry=completed"
SAMPLE = "\n".join([
    "[nat-matrix] PASS [pcone-pcone]: direct transfer, 8MiB byte-identical via 203.0.113.1:42001",
    "[nat-matrix] PASS [pcone-fullcone]: direct transfer, 8MiB byte-identical via 203.0.113.1:42002",
    "[nat-matrix] PASS [v6-stateful]: direct transfer, 8MiB byte-identical via [2001:77:1::2]:42003",
    "[nat-matrix] PASS [symm-symm]: symmetric<->symmetric failed cleanly (bounded wording, no hang, no data moved)",
    "[nat-matrix] PASS [late-send-5s]: direct transfer, 8MiB byte-identical via 203.0.113.1:42004",
    "[nat-matrix] PASS [late-send-40s]: direct transfer, 8MiB byte-identical via 203.0.113.1:42005",
    "[nat-matrix] PASS [recv-no-lowttl]: direct transfer, 8MiB byte-identical via 203.0.113.1:42006",
    RETRY,
    "[nat-matrix] PASS [decline-retry-40s]: direct transfer, 8MiB byte-identical via 203.0.113.1:42007",
    FINAL,
    "",
])
PASS_40 = "[nat-matrix] PASS [late-send-40s]: direct transfer, 8MiB byte-identical via 203.0.113.1:42005"


class ProcessBoundary:
    """Reject every command outside the specified protocol, including real sudo."""

    def __init__(self, repo, logs=None, returncodes=None, **options):
        self.repo = repo
        self.logs = logs or [SAMPLE] * 10
        self.returncodes = returncodes or {}
        self.options = options
        self.commands = []
        self.matrix_calls = 0
        self.build_calls = 0

    def __call__(self, args, **kwargs):
        args = list(args)
        self.commands.append(args)
        if Path(kwargs["cwd"]) != self.repo:
            raise AssertionError("every child must run from the fixed checkout")
        stdout = ""
        rc = 0
        if args == ["git", "rev-parse", "HEAD"]:
            stdout = self.options.get("head", SHA) + "\n"
            if self.matrix_calls >= self.options.get("head_changes_at", 999):
                stdout = "2" * 40 + "\n"
        elif args == ["git", "diff", "--quiet", "HEAD", "--"]:
            rc = self.options.get("dirty_rc", 0)
        elif args == ["sudo", "-n", "true"]:
            rc = self.options.get("sudo_rc", 0)
        elif args == ["sudo", "-n", "ip", "netns", "list"]:
            stdout = self.options.get("namespaces", {}).get(self.matrix_calls, "")
            rc = self.options.get("namespace_rc", {}).get(self.matrix_calls, 0)
        elif args == ["cargo", "build", "-p", "ak-cli", "--locked", "--features", "exp-knobs"]:
            self.build_calls += 1
            if self.build_calls != 1:
                raise AssertionError("a streak may build only once")
            if self.options.get("build_exception"):
                raise OSError("AKI1-PRIVATE-EXCEPTION /identity/SECRET")
            rc = self.options.get("build_rc", 0)
            stdout = "error[E0123]: AKI1-PRIVATE-BUILD private_key=SECRET\n"
            if rc == 0 and not self.options.get("missing_cli"):
                cli = self.repo / "target/debug/ak-cli"
                cli.parent.mkdir(parents=True, exist_ok=True)
                cli.write_bytes(CLI_BYTES)
                cli.chmod(0o755)
        elif args == ["sudo", "timeout", "--kill-after=30s", "15m", "bash", "scripts/nat_matrix_test.sh", "--skip-build", "--size-mb", "8"]:
            self.matrix_calls += 1
            if self.build_calls != 1:
                raise AssertionError("matrix requires exactly one prior normal-user build")
            if self.matrix_calls > len(self.logs):
                raise AssertionError("coordinator continued after a failed round")
            if self.options.get("matrix_exception"):
                raise OSError("AKI1-PRIVATE-SPAWN /identity/SECRET")
            stdout = self.logs[self.matrix_calls - 1]
            rc = self.returncodes.get(self.matrix_calls, 0)
            if self.matrix_calls == self.options.get("mutate_cli_at"):
                (self.repo / "target/debug/ak-cli").write_bytes(b"changed binary")
            if self.matrix_calls == self.options.get("mutate_lock_at"):
                (self.repo / "Cargo.lock").write_bytes(b"changed lock")
        else:
            raise AssertionError(f"unexpected process boundary: {args!r}")
        sink = kwargs.get("stdout")
        if hasattr(sink, "write"):
            # Accept either text or binary log sinks, keeping the fake at the
            # OS boundary rather than replacing the production parser.
            try:
                sink.write(stdout)
            except TypeError:
                sink.write(stdout.encode("utf-8"))
            sink.flush()
            stdout = None
        elif not kwargs.get("text"):
            stdout = stdout.encode("utf-8")
        return subprocess.CompletedProcess(args, rc, stdout=stdout, stderr=None)


class NatStreakTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(RUNNER.is_file(), "J5 coordinator is not implemented yet")
        spec = importlib.util.spec_from_file_location("nat_streak_under_test", RUNNER)
        self.runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.runner)
        self.tmp = tempfile.TemporaryDirectory(prefix="ak-nat-streak-test-")
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name).resolve()
        self.repo = self.base / "checkout"
        self.repo.mkdir()
        (self.repo / "Cargo.lock").write_bytes(LOCK_BYTES)
        (self.repo / "scripts").mkdir()
        (self.repo / "scripts/nat_matrix_test.sh").write_text("# synthetic fixture\n", encoding="utf-8")
        self.private = self.base / "runner-temp"
        self.private.mkdir()
        self.output = self.base / "public-results"
        self.env = {
            "GITHUB_RUN_ID": "123456789",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_SHA": SHA,
            "GITHUB_JOB": "nat-streak",
            "RUNNER_TEMP": str(self.private),
        }

    def execute(self, logs=None, returncodes=None, *, system="Linux", uid=1001, **options):
        boundary = ProcessBoundary(self.repo, logs, returncodes, **options)
        console = io.StringIO()
        with mock.patch.object(self.runner.subprocess, "run", side_effect=boundary), \
                mock.patch.object(self.runner.platform, "system", return_value=system), \
                mock.patch.object(self.runner.os, "geteuid", return_value=uid, create=True), \
                contextlib.redirect_stdout(console), contextlib.redirect_stderr(console):
            rc = self.runner.run_streak(self.repo, self.output, self.env)
        self.boundary = boundary
        self.console = console.getvalue()
        self.result = json.loads((self.output / "results.json").read_text(encoding="utf-8"))
        return rc

    def assert_stopped(self, *, rounds=2, completed=1):
        self.assertEqual(self.boundary.matrix_calls, rounds)
        self.assertEqual(self.result["completed_rounds"], completed)
        self.assertEqual(self.result["passed_cases"], completed * 4)
        self.assertEqual(self.result["verdict"], "FAIL")
        self.assertEqual(len(self.result["cases"]), rounds * 4)
        self.assertNotIn("PASS", {row["verdict"] for row in self.result["cases"] if row["round"] > completed})
        self.assertFalse((self.output / f"round-{rounds + 1:02d}.pass.txt").exists())

    def invalid_round(self, log, **options):
        rc = self.execute(logs=[SAMPLE, log], **options)
        self.assertNotEqual(rc, 0)
        self.assert_stopped()
        return self.result["rounds"][-1]

    def test_success_requires_ten_sequential_rounds_and_exactly_forty_target_passes(self):
        self.assertEqual(self.execute(), 0)
        self.assertEqual(self.boundary.build_calls, 1)
        self.assertEqual(self.boundary.matrix_calls, 10)
        self.assertEqual(self.result["verdict"], "PASS")
        self.assertEqual(self.result["completed_rounds"], 10)
        self.assertEqual(self.result["passed_cases"], 40)
        self.assertEqual(len(self.result["cases"]), 40)
        self.assertEqual({row["case"] for row in self.result["cases"]}, set(CASES))
        for number in range(1, 11):
            rows = [row for row in self.result["cases"] if row["round"] == number]
            self.assertEqual(len(rows), 4)
            self.assertEqual({row["case"] for row in rows}, set(CASES))
            self.assertEqual({row["verdict"] for row in rows}, {"PASS"})
        self.assertEqual([item["matrix_exit_code"] for item in self.result["rounds"]], [0] * 10)
        self.assertTrue(all(item["cleanup_verified"] for item in self.result["rounds"]))
        self.assertEqual({item["retry_delay_ms"] for item in self.result["rounds"]}, {40000})

    def test_evidence_is_traceable_and_only_whitelisted_files_are_published(self):
        self.assertEqual(self.execute(), 0)
        expected = {"summary.txt", "results.json", "results.csv"}
        expected.update(f"round-{number:02d}.pass.txt" for number in range(1, 11))
        self.assertEqual({path.name for path in self.output.iterdir()}, expected)
        with (self.output / "results.csv").open(encoding="utf-8", newline="") as handle:
            rows = list(csv.DictReader(handle))
        self.assertEqual(len(rows), 40)
        for row in [*self.result["cases"], *rows]:
            self.assertEqual(row["run_id"], "123456789")
            self.assertEqual(str(row["attempt"]), "2")
            self.assertEqual(row["github_sha"], SHA)
            self.assertEqual(row["job_id"], "nat-streak")
            self.assertEqual(row["cli_sha256"], hashlib.sha256(CLI_BYTES).hexdigest())
            self.assertEqual(row["cargo_lock_sha256"], hashlib.sha256(LOCK_BYTES).hexdigest())
            self.assertRegex(row["utc"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z$")
        self.assertIsNone(self.result["job_database_id"])
        pass_files = "".join(path.read_text(encoding="utf-8") for path in self.output.glob("round-*.pass.txt"))
        self.assertEqual(pass_files.count("verdict=PASS"), 40)
        self.assertNotIn("203.0.113.1:", pass_files)

    def test_nonzero_exit_is_preserved_even_with_all_success_markers(self):
        self.assertEqual(self.execute(logs=[SAMPLE, SAMPLE], returncodes={2: 7}), 7)
        self.assert_stopped()
        self.assertEqual(self.result["rounds"][-1]["matrix_exit_code"], 7)

    def test_timeout_exit_is_preserved_and_stops_the_streak(self):
        self.assertEqual(self.execute(logs=[SAMPLE, SAMPLE], returncodes={2: 124}), 124)
        self.assert_stopped()
        self.assertIn("matrix_timeout", self.result["rounds"][-1]["reason_codes"])

    def test_forced_timeout_kill_is_never_counted_as_success(self):
        self.assertEqual(self.execute(logs=[SAMPLE, SAMPLE], returncodes={2: 137}), 137)
        self.assert_stopped()

    def test_matrix_fail_line_overrides_exit_zero_and_pass_lines(self):
        result = self.invalid_round(SAMPLE.replace(FINAL, "[nat-matrix] FAIL [late-send-40s]: sender did not complete (rc=1)\n" + FINAL))
        self.assertIn("matrix_failure", result["reason_codes"])

    def test_target_skip_even_with_pass_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace(FINAL, "[nat-matrix] SKIP case late-send-40s: unavailable\n" + FINAL))
        self.assertIn("matrix_skip", result["reason_codes"])

    def test_ipv6_environment_skip_makes_the_whole_round_unaccepted(self):
        result = self.invalid_round(SAMPLE.replace(FINAL, "[nat-matrix] SKIP case v6-stateful: no IPv6 stack\n" + FINAL))
        self.assertIn("matrix_skip", result["reason_codes"])

    def test_global_skip_exit_zero_is_not_success(self):
        self.invalid_round("[nat-matrix] SKIP: Linux-only network-namespace test\n")

    def test_missing_target_is_not_filled_by_symmetric_expected_failure_pass(self):
        result = self.invalid_round(SAMPLE.replace(PASS_40 + "\n", ""))
        self.assertIn("missing_target_pass", result["reason_codes"])

    def test_duplicate_target_pass_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace(PASS_40, PASS_40 + "\n" + PASS_40))
        self.assertIn("duplicate_pass", result["reason_codes"])

    def test_duplicate_nontarget_pass_is_also_rejected(self):
        duplicate = "[nat-matrix] PASS [symm-symm]: symmetric<->symmetric failed cleanly (bounded wording, no hang, no data moved)"
        result = self.invalid_round(SAMPLE.replace(duplicate, duplicate + "\n" + duplicate))
        self.assertIn("duplicate_pass", result["reason_codes"])

    def test_ipv6_target_cannot_masquerade_as_pass_v4(self):
        result = self.invalid_round(SAMPLE.replace(PASS_40, PASS_40.replace("203.0.113.1:42005", "[2001:77:1::2]:42005")))
        self.assertIn("invalid_target_pass", result["reason_codes"])

    def test_ipv4_mapped_ipv6_target_is_rejected(self):
        self.invalid_round(SAMPLE.replace(PASS_40, PASS_40.replace("203.0.113.1:42005", "[::ffff:203.0.113.1]:42005")))

    def test_wrong_ipv4_router_is_rejected(self):
        self.invalid_round(SAMPLE.replace(PASS_40, PASS_40.replace("203.0.113.1", "10.10.1.2")))

    def test_invalid_port_is_rejected(self):
        self.invalid_round(SAMPLE.replace(PASS_40, PASS_40.replace(":42005", ":65536")))

    def test_wrong_payload_size_is_rejected(self):
        self.invalid_round(SAMPLE.replace(PASS_40, PASS_40.replace("8MiB", "1MiB")))

    def test_expected_failure_wording_is_not_a_target_pass(self):
        self.invalid_round(SAMPLE.replace(PASS_40, "[nat-matrix] PASS [late-send-40s]: xfail held -- no data moved"))

    def test_missing_retry_evidence_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace(RETRY + "\n", ""))
        self.assertIn("missing_retry_evidence", result["reason_codes"])

    def test_short_retry_delay_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace("delay_ms=40000", "delay_ms=39999"))
        self.assertIn("retry_delay_too_short", result["reason_codes"])

    def test_longer_measured_retry_delay_is_preserved(self):
        self.assertEqual(self.execute(logs=[SAMPLE.replace("delay_ms=40000", "delay_ms=40789")] * 10), 0)
        self.assertEqual({item["retry_delay_ms"] for item in self.result["rounds"]}, {40789})

    def test_retry_must_follow_a_decline(self):
        result = self.invalid_round(SAMPLE.replace("first=declined", "first=failed"))
        self.assertIn("invalid_retry_evidence", result["reason_codes"])

    def test_retry_must_use_the_same_pairing(self):
        self.invalid_round(SAMPLE.replace("same_pairing=true", "same_pairing=false"))

    def test_retry_must_be_completed(self):
        self.invalid_round(SAMPLE.replace("retry=completed", "retry=started"))

    def test_retry_case_must_match_the_target(self):
        self.invalid_round(SAMPLE.replace(RETRY, RETRY.replace("decline-retry-40s", "late-send-40s")))

    def test_retry_delay_must_be_an_integer_measurement(self):
        self.invalid_round(SAMPLE.replace("delay_ms=40000", "delay_ms=40000ms"))

    def test_confirmed_matrix_prefixed_retry_summary_is_accepted(self):
        self.assertEqual(self.execute(logs=[SAMPLE.replace(RETRY, "[nat-matrix] " + RETRY)] * 10), 0)
        self.assertEqual(self.result["passed_cases"], 40)

    def test_prefixed_retry_counts_toward_the_same_uniqueness_check(self):
        result = self.invalid_round(SAMPLE.replace(RETRY, RETRY + "\n[nat-matrix] " + RETRY))
        self.assertIn("duplicate_retry_evidence", result["reason_codes"])

    def test_prefixed_retry_must_have_exact_fields_without_suffix(self):
        result = self.invalid_round(SAMPLE.replace(RETRY, "[nat-matrix] " + RETRY + " extra=untrusted"))
        self.assertIn("invalid_retry_evidence", result["reason_codes"])

    def test_duplicate_retry_evidence_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace(RETRY, RETRY + "\n" + RETRY))
        self.assertIn("duplicate_retry_evidence", result["reason_codes"])

    def test_missing_final_summary_is_rejected(self):
        result = self.invalid_round(SAMPLE.replace(FINAL + "\n", ""))
        self.assertIn("missing_final_result", result["reason_codes"])

    def test_duplicate_final_summary_is_rejected(self):
        result = self.invalid_round(SAMPLE + FINAL + "\n")
        self.assertIn("duplicate_final_result", result["reason_codes"])

    def test_final_summary_cannot_precede_target_evidence(self):
        result = self.invalid_round(FINAL + "\n" + SAMPLE.replace(FINAL + "\n", ""))
        self.assertIn("result_not_final", result["reason_codes"])

    def test_each_fixed_namespace_residue_blocks_the_next_round(self):
        # One invocation, all seven names: the evidence must identify the exact
        # whitelisted residue without accepting the otherwise passing round.
        residues = "\n".join(f"{name} (id: {number})" for number, name in enumerate(NAMESPACES)) + "\n"
        self.assertNotEqual(self.execute(logs=[SAMPLE, SAMPLE], namespaces={2: residues}), 0)
        self.assert_stopped()
        result = self.result["rounds"][-1]
        self.assertFalse(result["cleanup_verified"])
        self.assertEqual(set(result["namespaces_remaining"]), set(NAMESPACES))
        self.assertIn("namespace_residue", result["reason_codes"])

    def test_unrelated_namespace_does_not_count_as_matrix_residue(self):
        self.assertEqual(self.execute(namespaces={number: "unrelated-test (id: 42)\n" for number in range(11)}), 0)

    def test_preexisting_namespace_fails_before_build_or_matrix(self):
        self.assertNotEqual(self.execute(namespaces={0: "akinet (id: 4)\n"}), 0)
        self.assert_stopped(rounds=0, completed=0)
        self.assertEqual(self.boundary.build_calls, 0)

    def test_failed_namespace_inspection_is_not_treated_as_empty(self):
        self.assertNotEqual(self.execute(logs=[SAMPLE, SAMPLE], namespace_rc={2: 1}), 0)
        self.assert_stopped()
        self.assertIn("namespace_inspection_failed", self.result["rounds"][-1]["reason_codes"])

    def test_cleanup_is_checked_even_after_matrix_spawn_failure(self):
        self.assertNotEqual(self.execute(logs=[SAMPLE], matrix_exception=True), 0)
        self.assert_stopped(rounds=1, completed=0)
        self.assertEqual(self.boundary.commands[-1], ["sudo", "-n", "ip", "netns", "list"])
        self.assertIsNone(self.result["rounds"][-1]["matrix_exit_code"])

    def test_cli_hash_change_invalidates_the_round_and_stops(self):
        self.assertNotEqual(self.execute(logs=[SAMPLE, SAMPLE], mutate_cli_at=2), 0)
        self.assert_stopped()
        self.assertIn("cli_changed", self.result["rounds"][-1]["reason_codes"])

    def test_lock_hash_change_invalidates_the_round_and_stops(self):
        self.assertNotEqual(self.execute(logs=[SAMPLE, SAMPLE], mutate_lock_at=2), 0)
        self.assert_stopped()
        self.assertIn("cargo_lock_changed", self.result["rounds"][-1]["reason_codes"])

    def test_checkout_change_during_streak_is_rejected(self):
        self.assertNotEqual(self.execute(logs=[SAMPLE, SAMPLE], head_changes_at=2), 0)
        self.assert_stopped()
        self.assertIn("checkout_sha_mismatch", self.result["rounds"][-1]["reason_codes"])

    def test_checkout_must_match_github_sha_before_build(self):
        self.assertNotEqual(self.execute(head="2" * 40), 0)
        self.assertEqual(self.boundary.build_calls, 0)
        self.assert_stopped(rounds=0, completed=0)

    def test_tracked_worktree_drift_is_rejected(self):
        self.assertNotEqual(self.execute(dirty_rc=1), 0)
        self.assertEqual(self.boundary.build_calls, 0)
        self.assert_stopped(rounds=0, completed=0)

    def test_build_failure_preserves_exit_code_and_never_runs_matrix(self):
        self.assertEqual(self.execute(build_rc=101), 101)
        self.assertEqual(self.result["build_exit_code"], 101)
        self.assert_stopped(rounds=0, completed=0)
        self.assertNotIn("AKI1-PRIVATE-BUILD", self.console)

    def test_missing_cli_after_build_is_not_rebuilt_under_sudo(self):
        self.assertNotEqual(self.execute(missing_cli=True), 0)
        self.assertEqual(self.boundary.build_calls, 1)
        self.assert_stopped(rounds=0, completed=0)

    def test_root_execution_is_rejected_before_build(self):
        self.assertNotEqual(self.execute(uid=0), 0)
        self.assertEqual(self.boundary.build_calls, 0)
        self.assert_stopped(rounds=0, completed=0)

    def test_nonlinux_execution_is_a_failure_not_a_skip_success(self):
        self.assertNotEqual(self.execute(system="Windows"), 0)
        self.assertEqual(self.boundary.commands, [])
        self.assert_stopped(rounds=0, completed=0)

    def test_metadata_is_required_and_never_echoed_without_validation(self):
        self.env["GITHUB_JOB"] = "AKI1-SECRET\n::error::PRIVATE"
        self.assertNotEqual(self.execute(), 0)
        self.assertEqual(self.boundary.commands, [])
        artifacts = "".join(path.read_text(encoding="utf-8") for path in self.output.iterdir())
        self.assertNotIn("AKI1-SECRET", artifacts + self.console)

    def test_missing_retry_after_valid_round_never_reuses_prior_round_evidence(self):
        self.invalid_round(SAMPLE.replace(RETRY + "\n", ""))

    def test_failure_diagnostics_are_bounded_and_do_not_publish_raw_secrets(self):
        secret = "AKI1-RAW-SECRET private_key=DEADBEEF /identity/secret\n" + "X" * 20000
        log = SAMPLE.replace(FINAL, "[nat-matrix] FAIL [late-send-40s]: sender did not complete (rc=1) " + secret + "\n" + FINAL)
        result = self.invalid_round(log)
        artifacts = "".join(path.read_text(encoding="utf-8") for path in self.output.iterdir())
        for token in ("AKI1-RAW-SECRET", "DEADBEEF", "/identity/secret", "X" * 100):
            self.assertNotIn(token, self.console + artifacts)
        self.assertLess(len(self.console), 5000)
        self.assertTrue(result["diagnostics"])
        self.assertLessEqual(len(result["diagnostics"]), 8)
        self.assertEqual(result["diagnostics"][0]["case"], "late-send-40s")
        self.assertEqual(result["diagnostics"][0]["stage"], "sender")
        private_files = list(self.private.rglob("*.log"))
        self.assertTrue(private_files)
        self.assertTrue(any("AKI1-RAW-SECRET" in path.read_text(encoding="utf-8") for path in private_files))

    def test_spawn_exception_text_is_not_published(self):
        self.assertNotEqual(self.execute(build_exception=True), 0)
        self.assert_stopped(rounds=0, completed=0)
        artifacts = "".join(path.read_text(encoding="utf-8") for path in self.output.iterdir())
        self.assertNotIn("AKI1-PRIVATE-EXCEPTION", artifacts + self.console)
        self.assertNotIn("/identity/SECRET", artifacts + self.console)


if __name__ == "__main__":
    unittest.main()
