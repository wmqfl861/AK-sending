#!/usr/bin/env python3
"""J5: one build, ten consecutive FULL NAT matrices on one checkout SHA.

Run as the ordinary runner user: python3 scripts/run_nat_streak.py --output DIR
The matrix alone runs under sudo. No case filter, parallel rounds, retries or
resume are supported. This is Linux netns simulation evidence, not real-device
NAT acceptance. GITHUB_RUN_ID/RUN_ATTEMPT/SHA/JOB and RUNNER_TEMP are required.

Raw cargo/matrix output stays in a mode-0700 RUNNER_TEMP directory, outside the
published directory. Public files are reconstructed from typed, allowlisted
fields; even failure diagnostics never copy raw text, paths, addresses or keys.
The job database ID is deliberately null: GITHUB_JOB is the stable job key; an
integrator can resolve the numeric ID after the run through the GitHub API.

J2 producer contract (fail closed if absent or changed), exactly once per round:
[nat-matrix] [retry-evidence] case=decline-retry-40s first=declined same_pairing=true delay_ms=<integer> retry=completed
The fixed [nat-matrix] prefix is optional; all remaining fields are exact.
The producer must measure >=40000 ms with a monotonic clock after a REAL first
decline, reuse the SAME TaskPairing, and emit only after the second transfer's
byte check. Parsing this assertion cannot independently prove those internals.
"""
from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile

ROUNDS = 10
TARGETS = ("late-send-5s", "late-send-40s", "recv-no-lowttl", "decline-retry-40s")
KNOWN_CASES = (*TARGETS, "pcone-pcone", "pcone-fullcone", "v6-stateful", "symm-symm")
NAMESPACES = ("akalice", "akbob", "akr1", "akr2", "akcore", "aksvc", "akinet")
FINAL = "[nat-matrix] RESULT: all runnable matrix cases PASS (incl. fixture self-checks)"
BUILD = ["cargo", "build", "-p", "ak-cli", "--locked", "--features", "exp-knobs"]
MATRIX = ["sudo", "timeout", "--kill-after=30s", "15m", "bash",
          "scripts/nat_matrix_test.sh", "--skip-build", "--size-mb", "8"]
RETRY = re.compile(
    r"\[retry-evidence\] case=decline-retry-40s first=declined "
    r"same_pairing=true delay_ms=([0-9]{1,12}) retry=completed"
)
PASS = re.compile(r"\[nat-matrix\] PASS \[([^\]\r\n]+)\]: (.*)")
V4 = re.compile(r"direct transfer, 8MiB byte-identical via 203\.0\.113\.1:([0-9]{1,5})")
CSV_FIELDS = ("run_id", "attempt", "github_sha", "job_id", "round", "case",
              "utc", "verdict", "expectation", "cli_sha256", "cargo_lock_sha256")


class Rejected(Exception):
    def __init__(self, reason, exit_code=1):
        self.reason = reason
        self.exit_code = exit_code


def utc_now():
    # UTC is the observation/verdict time; the retry interval is producer-side
    # monotonic time, never inferred from these wall-clock timestamps.
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def sha256(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def command(repo, args, *, timeout=30):
    return subprocess.run(args, cwd=repo, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                          text=True, encoding="utf-8", errors="replace", timeout=timeout,
                          check=False)


def checkout_reasons(repo, expected):
    reasons = []
    try:
        head = command(repo, ["git", "rev-parse", "HEAD"])
        if head.returncode != 0 or head.stdout.strip() != expected:
            reasons.append("checkout_sha_mismatch")
        if command(repo, ["git", "diff", "--quiet", "HEAD", "--"]).returncode != 0:
            reasons.append("tracked_checkout_changed")
    except (OSError, subprocess.SubprocessError):
        reasons.append("checkout_inspection_failed")
    return reasons


def namespace_state(repo):
    # Failure to inspect is not an empty namespace list. Never delete preexisting
    # namespaces or repair a failed round and then count it as a clean round.
    try:
        result = command(repo, ["sudo", "-n", "ip", "netns", "list"])
        if result.returncode != 0:
            return False, [], ["namespace_inspection_failed"]
        names = {line.split()[0] for line in result.stdout.splitlines() if line.split()}
        remaining = [name for name in NAMESPACES if name in names]
        return not remaining, remaining, ["namespace_residue"] if remaining else []
    except (OSError, subprocess.SubprocessError):
        return False, [], ["namespace_inspection_failed"]


def diagnostic(line, number):
    """Classify into constants only. No arbitrary captured string is published."""
    case = "matrix"
    for candidate in KNOWN_CASES:
        if f"[{candidate}]" in line or f"case {candidate}:" in line:
            case = candidate
            break
    stage = "matrix"
    for needle, label in (
        ("sender", "sender"), ("receiver", "receiver"),
        ("fixture", "fixture"), ("sha256", "byte_check"),
        ("payload", "byte_check"), ("topology", "topology"),
        ("NAT rules", "topology"), ("IPv6", "ipv6_environment"),
        ("ip6tables", "ipv6_environment"), ("registry", "registry"),
        ("stun", "stun"), ("setup", "setup"),
    ):
        if needle in line:
            stage = label
            break
    return {"line": number, "case": case, "stage": stage}


def parse_round(path):
    reasons = []
    counts = {}
    targets = set()
    retry_count = 0
    retry_delay = None
    result_count = 0
    final_seen = False
    diagnostics = []
    with path.open(encoding="utf-8", errors="replace") as handle:
        for number, raw in enumerate(handle, 1):
            line = raw.rstrip("\r\n")
            matrix_line = line.startswith("[nat-matrix]")
            retry_text = line.removeprefix("[nat-matrix] ")
            retry_line = retry_text.startswith("[retry-evidence]")
            if final_seen and (matrix_line or retry_line):
                reasons.append("result_not_final")
            if line.startswith("[nat-matrix] FAIL") or line.startswith("[nat-matrix] SKIP"):
                reasons.append("matrix_skip" if line.startswith("[nat-matrix] SKIP") else "matrix_failure")
                if len(diagnostics) < 8:
                    diagnostics.append(diagnostic(line, number))
            if line.startswith("[nat-matrix] PASS"):
                match = PASS.fullmatch(line)
                if not match:
                    reasons.append("invalid_pass_record")
                    continue
                case, detail = match.groups()
                # Unknown labels remain internal: they never reach public JSON.
                counts[case] = counts.get(case, 0) + 1
                if counts[case] > 1:
                    reasons.append("duplicate_pass")
                if case in TARGETS:
                    address = V4.fullmatch(detail)
                    if address is None or not 1 <= int(address.group(1)) <= 65535:
                        reasons.append("invalid_target_pass")
                    else:
                        targets.add(case)
            if retry_line:
                retry_count += 1
                match = RETRY.fullmatch(retry_text)
                if match is None:
                    reasons.append("invalid_retry_evidence")
                else:
                    retry_delay = int(match.group(1))
                    if retry_delay < 40000:
                        reasons.append("retry_delay_too_short")
            if line.startswith("[nat-matrix] RESULT:"):
                result_count += 1
                if line != FINAL:
                    reasons.append("invalid_final_result")
                else:
                    final_seen = True
    if any(counts.get(case, 0) == 0 for case in TARGETS):
        reasons.append("missing_target_pass")
    if retry_count == 0:
        reasons.append("missing_retry_evidence")
    elif retry_count != 1:
        reasons.append("duplicate_retry_evidence")
    if result_count == 0:
        reasons.append("missing_final_result")
    elif result_count != 1:
        reasons.append("duplicate_final_result")
    return {
        "reason_codes": list(dict.fromkeys(reasons)),
        "observed_target_passes": [case for case in TARGETS if case in targets],
        "retry_delay_ms": retry_delay,
        "final_result_verified": result_count == 1 and final_seen,
        "diagnostics": diagnostics,
    }


def publish(output, result):
    # A fresh output directory and a literal upload allowlist prevent old rounds
    # or private files from being bundled. No raw log is copied or symlinked here.
    (output / "results.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    with (output / "results.csv").open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=CSV_FIELDS)
        writer.writeheader()
        writer.writerows(result["cases"])
    summary = [
        "NAT streak: Linux network-namespace simulation (not real-device evidence)",
        f"verdict={result['verdict']} completed_rounds={result['completed_rounds']}/10 passed_cases={result['passed_cases']}/40 exit_code={result['exit_code']}",
        f"run_id={result['run_id']} attempt={result['attempt']} github_sha={result['github_sha']} job_id={result['job_id']}",
        "job_database_id=unresolved (resolve the numeric ID after integration)",
        f"cli_sha256={result['cli_sha256']} cargo_lock_sha256={result['cargo_lock_sha256']}",
        "reason_codes=" + ",".join(result["reason_codes"]),
    ]
    (output / "summary.txt").write_text("\n".join(summary) + "\n", encoding="utf-8")


def record_round(output, result, number, details, rc, clean, remaining):
    reasons = list(dict.fromkeys(details["reason_codes"]))
    accepted = not reasons and rc == 0 and clean
    verdict = "PASS" if accepted else "FAIL"
    timestamp = utc_now()
    row = {
        "round": number, "utc": timestamp, "verdict": verdict,
        "matrix_exit_code": rc, "cleanup_verified": clean,
        "namespaces_remaining": remaining, **details,
    }
    row["reason_codes"] = reasons
    result["rounds"].append(row)
    safe_lines = []
    for case in TARGETS:
        evidence = {field: result[field] for field in
                    ("run_id", "attempt", "github_sha", "job_id", "cli_sha256", "cargo_lock_sha256")}
        evidence.update(round=number, case=case, utc=timestamp, verdict=verdict, expectation="pass-v4")
        result["cases"].append(evidence)
        safe_lines.append(" ".join(f"{field}={evidence[field]}" for field in CSV_FIELDS))
    safe_lines.append(f"RESULT round={number} result={verdict} matrix_exit_code={rc} cleanup_verified={str(clean).lower()} retry_delay_ms={details['retry_delay_ms']}")
    safe_lines.append("reason_codes=" + ",".join(reasons))
    (output / f"round-{number:02d}.pass.txt").write_text("\n".join(safe_lines) + "\n", encoding="utf-8")
    if accepted:
        result["completed_rounds"] += 1
        result["passed_cases"] += 4
    print(f"[nat-streak] round={number:02d}/10 verdict={verdict} matrix_exit_code={rc} cleanup_verified={str(clean).lower()} reasons={','.join(reasons) or 'none'}", flush=True)
    for item in details["diagnostics"]:
        print(f"[nat-streak] diagnostic round={number:02d} line={item['line']} case={item['case']} stage={item['stage']}", flush=True)
    return accepted


def process_exit(code):
    # subprocess uses negative signal numbers; shell/Actions use 128+signal.
    return code if code > 0 else 128 - code if code < 0 else 1


def run_streak(repo, output, environment):
    repo, output = Path(repo).resolve(), Path(output).resolve()
    result = {
        "schema_version": 1, "evidence_scope": "linux-netns-simulation",
        "run_id": None, "attempt": None, "github_sha": None, "job_id": None,
        "job_database_id": None, "cli_sha256": None, "cargo_lock_sha256": None,
        "started_utc": utc_now(), "finished_utc": None, "verdict": "FAIL",
        "requested_rounds": ROUNDS, "completed_rounds": 0, "passed_cases": 0,
        "build_exit_code": None, "exit_code": 1, "reason_codes": [], "rounds": [], "cases": [],
    }
    try:
        output.mkdir(parents=True, exist_ok=False)
    except OSError:
        print("[nat-streak] FAIL reason=output_directory_not_fresh_or_unwritable", flush=True)
        return 1
    try:
        metadata = {}
        for key, field, pattern in (
            ("GITHUB_RUN_ID", "run_id", r"[1-9][0-9]{0,19}"),
            ("GITHUB_RUN_ATTEMPT", "attempt", r"[1-9][0-9]{0,9}"),
            ("GITHUB_SHA", "github_sha", r"[0-9a-f]{40}"),
            ("GITHUB_JOB", "job_id", r"[A-Za-z_][A-Za-z0-9_-]{0,99}"),
        ):
            value = environment.get(key, "")
            if not re.fullmatch(pattern, value):
                raise Rejected("invalid_github_metadata")
            metadata[field] = value
        result.update(metadata)
        publish(output, result)
        if platform.system() != "Linux":
            raise Rejected("linux_required")
        if os.geteuid() == 0:
            raise Rejected("ordinary_build_user_required")
        reasons = checkout_reasons(repo, result["github_sha"])
        if reasons:
            result["reason_codes"].extend(reasons)
            raise Rejected("checkout_not_fixed")
        if command(repo, ["sudo", "-n", "true"]).returncode != 0:
            raise Rejected("passwordless_sudo_required")
        clean, _, reasons = namespace_state(repo)
        if not clean:
            result["reason_codes"].extend(reasons)
            raise Rejected("preexisting_namespace_or_inspection_failure")
        runner_temp = environment.get("RUNNER_TEMP")
        if not runner_temp or not Path(runner_temp).is_dir():
            raise Rejected("runner_temp_required")
        private = Path(tempfile.mkdtemp(prefix="ak-nat-streak-private-", dir=runner_temp))
        if private == output or output in private.parents or private in output.parents:
            raise Rejected("private_output_overlap")
        private.chmod(0o700)
        cli, lock = repo / "target/debug/ak-cli", repo / "Cargo.lock"
        result["cargo_lock_sha256"] = sha256(lock)
        # The matrix hardcodes target/debug/ak-cli; never let Cargo's inherited
        # target directory or target triple build a different artifact instead.
        build_env = dict(environment)
        build_env["CARGO_TARGET_DIR"] = str(repo / "target")
        build_env.pop("CARGO_BUILD_TARGET", None)
        with (private / "build.log").open("wb") as raw:
            build = subprocess.run(BUILD, cwd=repo, env=build_env, stdin=subprocess.DEVNULL,
                                   stdout=raw, stderr=subprocess.STDOUT, timeout=1800, check=False)
        result["build_exit_code"] = build.returncode
        if build.returncode != 0:
            raise Rejected("build_failed", process_exit(build.returncode))
        if not cli.is_file() or not os.access(cli, os.X_OK):
            raise Rejected("built_cli_missing_or_not_executable")
        result["cli_sha256"] = sha256(cli)
        for number in range(1, ROUNDS + 1):
            # Check before *every* launch as well as after exit. Refuse drift or
            # residue without starting another matrix or doing any rebuild.
            reasons = checkout_reasons(repo, result["github_sha"])
            if sha256(cli) != result["cli_sha256"]:
                reasons.append("cli_changed")
            if sha256(lock) != result["cargo_lock_sha256"]:
                reasons.append("cargo_lock_changed")
            clean, _, ns_reasons = namespace_state(repo)
            reasons.extend(ns_reasons)
            if reasons:
                result["reason_codes"].extend(reasons)
                raise Rejected("pre_round_gate_failed")
            raw_path = private / f"round-{number:02d}.log"
            rc = None
            execution_reasons = []
            print(f"[nat-streak] round={number:02d}/10 started utc={utc_now()}", flush=True)
            try:
                with raw_path.open("wb") as raw:
                    # GNU timeout owns the privileged process group; no pipe or
                    # tee can hide its return code. timeout sends TERM, then
                    # KILL after 30s. The matrix's EXIT trap owns normal cleanup.
                    execution = subprocess.run(MATRIX, cwd=repo, stdin=subprocess.DEVNULL,
                                               stdout=raw, stderr=subprocess.STDOUT, check=False)
                    rc = execution.returncode
                if rc != 0:
                    execution_reasons.append("matrix_timeout" if rc in (124, 137) else "matrix_nonzero_exit")
            except (OSError, subprocess.SubprocessError):
                execution_reasons.append("matrix_execution_failed")
            finally:
                # Also inspect after spawn/exit failures and timeouts. Any
                # residual fixed namespace is a failure, never silently healed.
                clean, remaining, ns_reasons = namespace_state(repo)
            details = parse_round(raw_path)
            details["reason_codes"].extend(execution_reasons)
            details["reason_codes"].extend(ns_reasons)
            if rc is not None:
                details["reason_codes"].extend(checkout_reasons(repo, result["github_sha"]))
                if sha256(cli) != result["cli_sha256"]:
                    details["reason_codes"].append("cli_changed")
                if sha256(lock) != result["cargo_lock_sha256"]:
                    details["reason_codes"].append("cargo_lock_changed")
            if not record_round(output, result, number, details, rc, clean, remaining):
                result["reason_codes"].extend(details["reason_codes"])
                raise Rejected("round_rejected", process_exit(rc) if rc else 1)
            # Save after each accepted round, so an interrupted run can never
            # inherit the previous run's PASS and still retains partial evidence.
            publish(output, result)
        result["verdict"] = "PASS"
        result["exit_code"] = 0
    except Rejected as error:
        result["reason_codes"].append(error.reason)
        result["exit_code"] = error.exit_code
    except subprocess.TimeoutExpired:
        result["reason_codes"].append("command_timeout")
        result["exit_code"] = 124
    except (OSError, subprocess.SubprocessError):
        # Exception text can contain command arguments, user paths or secrets.
        result["reason_codes"].append("command_or_evidence_io_failed")
        result["exit_code"] = 1
    except KeyboardInterrupt:
        result["reason_codes"].append("interrupted")
        result["exit_code"] = 130
    result["reason_codes"] = list(dict.fromkeys(result["reason_codes"]))
    result["finished_utc"] = utc_now()
    try:
        publish(output, result)
    except OSError:
        print("[nat-streak] FAIL reason=evidence_write_failed", flush=True)
        return 1
    print(f"[nat-streak] verdict={result['verdict']} completed_rounds={result['completed_rounds']}/10 passed_cases={result['passed_cases']}/40 exit_code={result['exit_code']} reasons={','.join(result['reason_codes']) or 'none'}", flush=True)
    return result["exit_code"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="new directory for allowlisted public evidence")
    options = parser.parse_args()
    return run_streak(Path(__file__).resolve().parents[1], options.output, os.environ)


if __name__ == "__main__":
    raise SystemExit(main())
