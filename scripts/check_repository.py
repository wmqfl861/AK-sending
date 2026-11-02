#!/usr/bin/env python3
"""L0 repository checks only. This does NOT compile Rust or prove P2P behavior."""
from __future__ import annotations
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
EXPECTED = {
    "schema_version": 1,
    "project": "AK-sending",
    "architecture_decision": "ADR-001",
    "data_plane": "authenticated_endpoint_direct_only",
    "server_file_bytes_allowed": False,
    "relay_allowed": False,
    "turn_allowed": False,
    "cloud_spooling_allowed": False,
    "lan_preferred": True,
    "direct_failure_action": "pause_retry_or_wait",
    "compression": "none",
    "transcoding": False,
    "control_plane_allowed": ["device_discovery", "presence", "signaling", "stun", "nat_coordination"],
    "file_metadata_via_server": False,
    "implementation_stage": "authenticated_direct_adapter_experiment",
}
TRANSPORT_DEPENDENCIES = {
    "ak-core": {"path": "../ak-core"},
    "quinn": {"version": "=0.11.12", "default-features": False,
              "features": ["runtime-tokio", "rustls-ring"]},
    "rustls": {"version": "=0.23.45", "default-features": False, "features": ["ring", "std"]},
    "ring": {"version": "=0.17.14", "default-features": False, "features": ["std"]},
    "tokio": {"version": "=1.54.1", "default-features": False, "features": ["rt", "net", "time"]},
}
TRANSPORT_DEV_DEPENDENCIES = {
    "rcgen": {"version": "=0.14.10", "default-features": False, "features": ["crypto", "ring"]},
    "tokio": {"version": "=1.54.1", "default-features": False,
              "features": ["rt", "net", "time", "macros"]},
}


def validate_architecture(data: object) -> list[str]:
    if not isinstance(data, dict):
        return ["architecture configuration must be an object"]
    errors = []
    for key, expected in EXPECTED.items():
        actual = data.get(key)
        if type(actual) is not type(expected) or actual != expected:
            errors.append(f"architecture mismatch: {key}")
    for key in data.keys() - EXPECTED.keys():
        errors.append(f"unknown architecture option: {key}")
    return errors


def validate_transport(data: dict) -> list[str]:
    errors = []
    if data.get("dependencies") != TRANSPORT_DEPENDENCIES:
        errors.append("unreviewed transport dependency/version/feature change")
    if data.get("dev-dependencies") != TRANSPORT_DEV_DEPENDENCIES:
        errors.append("unreviewed transport test dependencies")
    if any(key in data for key in ("target", "build-dependencies", "features", "patch", "replace")):
        errors.append("unreviewed transport conditional dependencies or features")
    if data.get("package", {}).get("publish") is not False:
        errors.append("transport crate must not be publishable")
    if "build" in data.get("package", {}):
        errors.append("unreviewed transport build script")
    return errors


def check(root: Path) -> dict:
    errors: list[str] = []
    required = [
        "README.md", "AGENTS.md", "docs/BLUEPRINT.zh-CN.md", "docs/PRD.zh-CN.md",
        "docs/adr/001-direct-only.zh-CN.md", "docs/ARCHITECTURE.zh-CN.md",
        "docs/ROADMAP.zh-CN.md", "docs/TEST_PLAN.zh-CN.md", "docs/HANDOFF.zh-CN.md",
        "reports/BOOTSTRAP_VALIDATION.zh-CN.md", "crates/ak-core/src/lib.rs",
        "crates/ak-core/src/policy.rs", "crates/ak-core/src/chunk.rs", "crates/ak-core/src/job.rs",
        "crates/ak-transport/src/lib.rs", "docs/modules/M0-02_PLAN.zh-CN.md",
    ]
    for name in required:
        if not (root / name).is_file():
            errors.append(f"missing required file: {name}")
    try:
        data = json.loads((root / "config/architecture.json").read_text(encoding="utf-8"))
        errors.extend(validate_architecture(data))
    except (OSError, ValueError) as exc:
        errors.append(f"cannot read architecture configuration: {exc}")
    try:
        workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        crate = tomllib.loads((root / "crates/ak-core/Cargo.toml").read_text(encoding="utf-8"))
        if workspace.get("workspace", {}).get("members") != ["crates/ak-core", "crates/ak-transport"]:
            errors.append("unexpected workspace members")
        if crate.get("dependencies") != {}:
            errors.append("core crate must not acquire unreviewed dependencies")
        if crate.get("package", {}).get("publish") is not False:
            errors.append("core crate must not be publishable")
        transport = tomllib.loads((root / "crates/ak-transport/Cargo.toml").read_text(encoding="utf-8"))
        errors.extend(validate_transport(transport))
    except (OSError, ValueError) as exc:
        errors.append(f"invalid Cargo manifest: {exc}")
    checked_links = 0
    markdowns = sorted(p for p in root.rglob("*.md")
                       if not any(part in {"target", ".git", ".venv", "validation"}
                                  for part in p.relative_to(root).parts))
    for path in markdowns:
        text = path.read_text(encoding="utf-8")
        for link in re.findall(r"\[[^\]]*\]\(([^\s)]+)\)", text):
            if "://" in link or link.startswith(("mailto:", "#")):
                continue
            target = (path.parent / link.split("#", 1)[0]).resolve()
            if not target.is_relative_to(root.resolve()) or not target.exists():
                errors.append(f"broken/outside local link in {path.relative_to(root)}: {link}")
            checked_links += 1
    source_files = sorted((root / "crates").rglob("*.rs"))
    rust_tests = sum(p.read_text(encoding="utf-8").count("#[test]")
                     + p.read_text(encoding="utf-8").count("#[tokio::test]") for p in source_files)
    return {
        "level": "L0_static_repository_only",
        "passed": not errors,
        "markdown_files_checked": len(markdowns),
        "local_links_checked": checked_links,
        "rust_test_functions_detected": rust_tests,
        "execution_scope": "This invocation only: static files/configuration; no other test status asserted.",
        "errors": errors,
    }

if __name__ == "__main__":
    result = check(ROOT)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    sys.exit(0 if result["passed"] else 1)
