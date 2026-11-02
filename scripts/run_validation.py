#!/usr/bin/env python3
"""Run real checks and preserve evidence. Missing/failed required checks fail the run.
Includes domain and local UDP/TLS tests; no cross-device/product claim. Python >=3.11.
Run cargo fetch --locked before invoking this offline validation runner.
"""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def configure_console(stream=None) -> None:
    """Keep Unicode command evidence printable under Windows redirected stdout."""
    stream = sys.stdout if stream is None else stream
    reconfigure = getattr(stream, "reconfigure", None)
    if reconfigure is not None:
        reconfigure(encoding="utf-8", errors="backslashreplace")


def file_sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run_one(name: str, argv: list[str], cwd: Path, output: Path, timeout: float = 300) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    started = datetime.now(timezone.utc).isoformat()
    tick = time.monotonic()
    log = output / f"{name}.log"
    status, exit_code = "failed", None
    with log.open("w", encoding="utf-8") as stream:
        stream.write(f"command={json.dumps(argv)}\n")
        stream.flush()
        try:
            result = subprocess.run(argv, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT,
                                    timeout=timeout, check=False)
            exit_code = result.returncode
            status = "passed" if exit_code == 0 else "failed"
        except FileNotFoundError:
            status = "skipped_missing_tool"
            stream.write("Required executable not found. This check did not run.\n")
        except subprocess.TimeoutExpired:
            status = "timed_out"
            stream.write("Command timed out; the direct child was terminated.\n")
        except OSError as exc:
            status = "execution_error"
            stream.write(f"Cannot execute command: {exc}\n")
    return {"name": name, "argv": argv, "started_at": started, "status": status,
            "exit_code": exit_code, "elapsed_seconds": round(time.monotonic() - tick, 3),
            "log": log.name, "log_sha256": file_sha256(log)}


def all_passed(results: list[dict]) -> bool:
    return bool(results) and all(row["status"] == "passed" for row in results)


def git_value(*args: str) -> str | None:
    try:
        p = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True,
                           timeout=10, check=False)
        return p.stdout.strip() if p.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def main() -> int:
    configure_console()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "validation" / "local")
    parser.add_argument("--timeout", type=float, default=300)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be positive")
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error("output directory is not empty; choose a new attempt directory")
    commands = [
        ("rustc-version", ["rustc", "--version", "--verbose"]),
        ("cargo-version", ["cargo", "--version"]),
        ("static", [sys.executable, "scripts/check_repository.py"]),
        ("python-tests", [sys.executable, "-m", "unittest", "discover", "-s", "scripts/tests", "-v"]),
        ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        ("rust-tests", ["cargo", "test", "--workspace", "--locked", "--offline"]),
        ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--offline", "--", "-D", "warnings"]),
        ("features", ["cargo", "tree", "--workspace", "--locked", "--offline", "--edges", "features"]),
        ("clean-sources", ["git", "diff", "--exit-code", "--", "Cargo.toml", "Cargo.lock",
                           "crates", "scripts", "config", ".github"]),
    ]
    results = []
    for name, argv in commands:
        row = run_one(name, argv, ROOT, output, args.timeout)
        results.append(row)
        print(f"\n=== {name}: {row['status']} ===", flush=True)
        with (output / row["log"]).open(encoding="utf-8", errors="replace") as log:
            for line in log:
                print(line, end="")
    sources = sorted(set(ROOT.glob("Cargo.*")) | set(ROOT.glob("rust*.toml")) |
                     set((ROOT / "crates").rglob("*.rs")) |
                     set((ROOT / "crates").rglob("Cargo.toml")) |
                     set((ROOT / "scripts").rglob("*.py")))
    report = {"scope": "L0/L1 plus real same-host UDP/TLS integration; not cross-device acceptance",
              "commit": git_value("rev-parse", "HEAD"),
              "working_tree": git_value("status", "--porcelain"),
              "platform": platform.platform(), "python": platform.python_version(),
              "ci_run_id": os.getenv("GITHUB_RUN_ID"), "results": results,
              "source_sha256": {str(p.relative_to(ROOT)).replace("\\", "/"):
                                file_sha256(p) for p in sources},
              "passed": all_passed(results),
              "not_covered": ["cross-device/LAN/WAN reachability", "user file IO", "NAT",
                              "mobile physical devices", "performance", "no-relay packet capture",
                              "separate MSRV build", "formal security audit"]}
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
