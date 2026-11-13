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
    "crates/ak-session",
    "crates/ak-cli",
    "crates/ak-itests",
    "crates/ak-ffi",
]

# ADR-001/ADR-002: no relay/TURN/WebRTC/iroh may enter the dependency graph.
BANNED_CRATE_PATTERNS = ("relay", "turn", "iroh", "webrtc", "str0m", "datachannel")
# Explicit names for crates whose names dodge the substring patterns.
BANNED_CRATE_NAMES = {"coturn", "libquic-tunnel"}

# CR-P3-10 leftover (REVIEW_2026-11-11.zh-CN.md §4/§5, 2026-11-12 evening
# re-review): "集成者每次回写状态表后，用脚本检查列数和'本提交'字样".
# The section 7 status table has 5 columns (编号/批次/状态/提交/证据摘要);
# every execution row must keep exactly 5 columns (escaped \| is cell text,
# not a separator) and the 提交 column must carry a commit hash, never the
# "本提交" placeholder.
REVIEW_DOC = "docs/REVIEW_2026-11-11.zh-CN.md"
STATUS_TABLE_HEADING = "## 7."
STATUS_ROW_PREFIXES = ("| CR-", "| R36", "| E", "| wave-36")
STATUS_TABLE_COLUMNS = 5
COMMIT_PLACEHOLDER = "本提交"

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


def status_table_issues(text: str) -> tuple[int, list[str]]:
    """CR-P3-10: validate the review doc's section-7 status table.

    Every table row whose ID cell starts with CR-/R36/E (i.e. the execution
    rows; header/separator rows are excluded by the prefix match) must have
    exactly 5 columns and must not use the "本提交" placeholder in place of a
    commit hash. Column counting is escape-aware: ``\\|`` inside a cell is
    literal text (e.g. CR-P3-02's ``<>\"\\|?*``), never a separator. Empty
    leading/trailing fragments come from the delimiting pipes; interior empty
    fragments are real empty cells (e.g. CR-P2-08's blank 提交/证据摘要).
    Returns (rows_checked, errors) and fails closed when the section is gone.
    """
    lines = text.splitlines()
    start = None
    end = len(lines)
    for i, line in enumerate(lines):
        if line.startswith(STATUS_TABLE_HEADING):
            start = i
        elif start is not None and line.startswith("## "):
            end = i
            break
    if start is None:
        return 0, [f"{REVIEW_DOC}: status table section not found"]
    rows = 0
    errors: list[str] = []
    for i in range(start + 1, end):
        row = lines[i].lstrip()
        if not row.startswith(STATUS_ROW_PREFIXES):
            continue
        rows += 1
        cells = re.split(r"(?<!\\)\|", row)
        if cells and cells[0].strip() == "":
            cells = cells[1:]
        if cells and cells[-1].strip() == "":
            cells = cells[:-1]
        if len(cells) != STATUS_TABLE_COLUMNS:
            errors.append(
                f"{REVIEW_DOC}:{i + 1}: status table row has {len(cells)} columns, "
                f"expected {STATUS_TABLE_COLUMNS}"
            )
        # H0 (CR-P3-11): the commit column (4th cell) must be a commit
        # hash (hex string) or empty — "G6 commit" style placeholders are
        # rejected alongside "this commit".
        if len(cells) >= 4 and cells[3].strip():
            # Allow: hex hashes (7-40 chars), commit ranges with + or -,
            # or the literal "N/A" for not-applicable rows
            import re as _re
            valid = cells[3].strip().startswith(tuple("0123456789abcdef")) and len(cells[3].strip()) >= 7
            if not valid and cells[3] not in ('N/A', 'unknown', 'not yet'):
                errors.append(
                    f"{REVIEW_DOC}:{i + 1}: status table commit column has non-hash value {cells[3]!r}"
                )
        if COMMIT_PLACEHOLDER in row:
            errors.append(
                f'{REVIEW_DOC}:{i + 1}: status table row uses "{COMMIT_PLACEHOLDER}" '
                "placeholder instead of a commit hash"
            )
    return rows, errors


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
        "crates/ak-cli/src/main.rs", "crates/ak-itests/src/lib.rs", "crates/ak-ffi/src/lib.rs",
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
    # Skip build outputs: target/ holds cargo artifacts and dist copies of
    # README whose relative links legitimately dangle there.
    markdowns = sorted(
        path for path in root.rglob("*.md")
        if "target" not in path.relative_to(root).parts
        and "archive" not in path.relative_to(root).parts  # frozen history
    )
    for path in markdowns:
        text = path.read_text(encoding="utf-8")
        for link in re.findall(r"\[[^\]]*\]\(([^\s)]+)\)", text):
            if "://" in link or link.startswith(("mailto:", "#")):
                continue
            target = (path.parent / link.split("#", 1)[0]).resolve()
            if not target.is_relative_to(root.resolve()) or not target.exists():
                errors.append(f"broken/outside local link in {path.relative_to(root)}: {link}")
            checked_links += 1
        # CR-P2-12: code-formatted repo paths (backticked, not markdown
        # links) bypass the link checker — the reports/ archival left four
        # stale ones. Validate backticked reports/... paths exist (an
        # optional trailing :line/:col annotation is stripped).
        for ref in re.findall(r"`(reports/[^`]+?)`", text):
            if any(ch in ref for ch in "*?[…"):
                continue  # glob/ellipsis prose, not a literal path
            ref_path = ref.split(":", 1)[0] if not ref.endswith(":") else ref
            if not (root / ref_path).exists():
                errors.append(
                    f"stale code-formatted reports/ path in {path.relative_to(root)}: {ref}"
                )
    source_files = sorted((root / "crates").rglob("*.rs"))
    rust_tests = sum(p.read_text(encoding="utf-8").count("#[test]") for p in source_files)
    # CR-P3-10: gate the review status table's shape after every write-back.
    try:
        status_rows, status_errors = status_table_issues(
            (root / REVIEW_DOC).read_text(encoding="utf-8")
        )
    except OSError as exc:
        status_rows, status_errors = 0, [f"cannot read {REVIEW_DOC}: {exc}"]
    errors.extend(status_errors)
    # Contract consistency vs the frozen docs/contracts (wave-24 advisory
    # add-on, Codex final review #1). Appended call only: any drift or crash
    # reported here is a WARNING, never a gate failure, so every check above
    # keeps its exact previous behavior.
    contract_consistency: dict = {"status": "not-run"}
    try:
        import importlib.util

        spec = importlib.util.spec_from_file_location(
            "ak_check_contracts", Path(__file__).with_name("check_contracts.py")
        )
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        contract_consistency = module.summarize(root)
    except Exception as exc:  # noqa: BLE001 - advisory, degrade to a warning
        contract_consistency = {"status": "warning", "error": repr(exc)}
    return {
        "level": "L0_static_repository_only",
        "passed": not errors,
        "markdown_files_checked": len(markdowns),
        "local_links_checked": checked_links,
        "rust_test_cases_present_not_executed": rust_tests,
        "review_status_table_rows_checked": status_rows,
        "banned_dependency_scan": "clean" if not violations else violations,
        "network_tests_executed": False,
        "contract_consistency_warning": contract_consistency,
        "errors": errors,
    }


if __name__ == "__main__":
    result = check(ROOT)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    sys.exit(0 if result["passed"] else 1)
