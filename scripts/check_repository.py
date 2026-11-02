#!/usr/bin/env python3
"""L0 repository checks only. This does NOT compile Rust or prove P2P behavior.

Per ADR-002 (D6) the load-bearing constraints are:
- Rust tests (closed signaling message set, type-level channel construction),
- Cargo.lock dependency scan (no relay/TURN/WebRTC crates),
- the structure checks below.
config/architecture.json remains a declarative policy summary, not a mechanism.
"""
from __future__ import annotations
import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]

WORKSPACE_MEMBERS = [
    "crates/ak-core",
    "crates/ak-identity",
    "crates/ak-wire",
    "crates/ak-storage",
    "crates/ak-net",
    "crates/ak-discovery",
    "crates/ak-store",
    "crates/ak-signal",
    "crates/ak-engine",
    "crates/ak-cli",
    "crates/ak-itests",
]

# ADR-001/ADR-002: no relay/TURN/WebRTC/iroh may enter the dependency graph.
BANNED_CRATE_PATTERNS = ("relay", "turn", "iroh", "webrtc", "str0m", "datachannel")
# Explicit names for crates whose names dodge the substring patterns.
BANNED_CRATE_NAMES = {"coturn", "libquic-tunnel"}

EXPECTED_ARCHITECTURE = {
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
    "implementation_stage": "lan_cli_prototype",
}


def validate_architecture(data: object) -> list[str]:
    if not isinstance(data, dict):
        return ["architecture configuration must be an object"]
    errors = []
    for key, expected in EXPECTED_ARCHITECTURE.items():
        actual = data.get(key)
        if type(actual) is not type(expected) or actual != expected:
            errors.append(f"architecture mismatch: {key}")
    for key in data.keys() - EXPECTED_ARCHITECTURE.keys():
        errors.append(f"unknown architecture option: {key}")
    return errors


def banned_crates(lockfile: Path) -> list[str]:
    """Return crate names in Cargo.lock that violate the direct-only rule."""
    try:
        data = tomllib.loads(lockfile.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return ["<Cargo.lock unreadable>"]
    found: list[str] = []
    packages = data.get("package", [])
    if isinstance(packages, dict):
        packages = [packages]
    for package in packages:
        if not isinstance(package, dict):
            continue
        name = str(package.get("name", ""))
        lowered = name.lower()
        if lowered in BANNED_CRATE_NAMES or any(p in lowered for p in BANNED_CRATE_PATTERNS):
            found.append(name)
    return sorted(set(found))


def check(root: Path) -> dict:
    errors: list[str] = []
    required = [
        "README.md", "AGENTS.md", "docs/BLUEPRINT.zh-CN.md", "docs/PRD.zh-CN.md",
        "docs/adr/001-direct-only.zh-CN.md", "docs/adr/002-protocol-stack-and-order.zh-CN.md",
        "docs/ARCHITECTURE.zh-CN.md", "docs/ROADMAP.zh-CN.md", "docs/TEST_PLAN.zh-CN.md",
        "docs/HANDOFF.zh-CN.md",
        "crates/ak-core/src/lib.rs", "crates/ak-core/src/policy.rs",
        "crates/ak-core/src/chunk.rs", "crates/ak-core/src/job.rs",
        "crates/ak-wire/src/lib.rs", "crates/ak-identity/src/lib.rs",
        "crates/ak-storage/src/lib.rs", "crates/ak-net/src/lib.rs",
        "crates/ak-discovery/src/lib.rs", "crates/ak-store/src/lib.rs",
        "crates/ak-signal/src/lib.rs", "crates/ak-engine/src/lib.rs",
        "crates/ak-cli/src/main.rs", "crates/ak-itests/src/lib.rs",
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
        members = workspace.get("workspace", {}).get("members")
        if members != WORKSPACE_MEMBERS:
            errors.append(f"unexpected workspace members: {members}")
        core = tomllib.loads((root / "crates/ak-core/Cargo.toml").read_text(encoding="utf-8"))
        if core.get("dependencies") not in ({}, None):
            errors.append("ak-core must stay dependency-free (domain purity)")
        if core.get("package", {}).get("publish") is not False:
            errors.append("workspace crates must not be publishable")
    except (OSError, ValueError) as exc:
        errors.append(f"invalid Cargo manifest: {exc}")
    violations = banned_crates(root / "Cargo.lock")
    if violations:
        errors.append(f"banned crates in Cargo.lock (relay/TURN/WebRTC): {violations}")
    checked_links = 0
    markdowns = sorted(root.rglob("*.md"))
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
    rust_tests = sum(p.read_text(encoding="utf-8").count("#[test]") for p in source_files)
    return {
        "level": "L0_static_repository_only",
        "passed": not errors,
        "markdown_files_checked": len(markdowns),
        "local_links_checked": checked_links,
        "rust_test_cases_present_not_executed": rust_tests,
        "banned_dependency_scan": "clean" if not violations else violations,
        "network_tests_executed": False,
        "errors": errors,
    }


if __name__ == "__main__":
    result = check(ROOT)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    sys.exit(0 if result["passed"] else 1)
