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

# CR-P3-20 (REVIEW_2026-11-14-batchJ.zh-CN.md §4, 2026-11-14): each fact is
# written in exactly one place — per-item status only in the section-7 table,
# run numbers/counts only in the current batch handoff, STATUS only stage
# conclusions plus a one-line-per-run "最新验证" index. Mechanical gates:
#   a. every run id in STATUS's "最新验证" must also appear in the newest
#      docs/HANDOFF-BATCH-*.zh-CN.md;
#   b. that handoff must not claim 已核 while still carrying
#      尚待/待补/仍待/待执行 placeholder wording.
# K-R6 (REVIEW_2026-11-14-batchK.zh-CN.md §1, 2026-11-14): "the newest
# handoff" in gates a/b means the handoff docs/STATUS.zh-CN.md actually
# LINKS to, not merely the newest HANDOFF-BATCH-* file in docs/ — on the
# reviewed commit STATUS still pointed at batch J while an unlinked batch
# K handoff existed, and the filename-based pick silently validated the
# wrong document. The link must exist, point at an existing file, and not
# be stale (a newer handoff file sitting unlinked in docs/ is drift).
# K2-R4 (REVIEW_2026-11-15-batchK2.zh-CN.md §1, 2026-11-15): the link is
# now RESOLVED against STATUS's own directory (docs/) and the resolved
# canonical path must BE the newest handoff's canonical path — keeping
# only the basename let STATUS link archive/<same-name> (or an external
# URL with the same basename) while the checker validated the unlinked
# docs/ copy.
# K3 (REVIEW_2026-11-15-batchK3.zh-CN.md §1, 2026-11-15): EVERY
# current-batch link is validated, not just a max()/first-picked one —
# a canonical link early in STATUS must not mask archive/ or external
# targets later on the page, and the CR-P3-20 gates always anchor to
# the canonical newest handoff.
STATUS_DOC = "docs/STATUS.zh-CN.md"
LATEST_VERIFICATION_HEADING = "## 最新验证"
HANDOFF_NAME_RE = re.compile(r"^HANDOFF-BATCH-([A-Z])-(\d{4}-\d{2}-\d{2})\.zh-CN\.md$")
# Markdown inline links `[label](target)` with an optional CommonMark
# title (`[label](target "title")`). The target capture stops at
# whitespace or a closing paren. The pattern is matched over the WHOLE
# document — `[^\]]*` labels legally span lines, and a per-line scan
# silently skips multi-line labels (K4 review §1). Both the repo-wide
# link walk in check() and the STATUS handoff gate use this one pattern,
# so every supported link form is parsed consistently.
MARKDOWN_LINK_RE = re.compile(
    r"\[[^\]]*\]\(([^\s)]+)(?:\s+(?:\"[^\"]*\"|'[^']*'))?\)"
)
# Windows drive-letter targets (`C:\…`, `C:/…`) are absolute jumps, not
# relative links, even though they contain no `://`.
WINDOWS_DRIVE_RE = re.compile(r"^[A-Za-z]:[\\/]")
RUN_ID_RE = re.compile(r"\d{9,15}")  # GitHub Actions run ids (currently 11 digits)
CONFIRMED_RE = re.compile(r"已核")  # also matches "一致性已核"
PENDING_RE = re.compile(r"尚待|待补|仍待|待执行")
# Quoted spans are excluded before matching (quoting someone, or instructions
# like "write 'X' only after ..." are not claims). Unpaired quotes simply
# stay on the line; the error message keeps line numbers and excerpts for
# human review, so this stays a tripwire rather than a verdict.
QUOTED_SPAN_PATTERNS = tuple(
    re.compile(pattern)
    for pattern in (r"“[^”]*”", r"「[^」]*」", r"『[^』]*』", r'"[^"]*"')
)

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


def _handoff_sort_key(name: str) -> tuple[str, str] | None:
    """(date, letter) ordering key for a handoff filename, None if not one.

    Shared by the filename scan (latest_handoff) and the STATUS-link scan
    (status_linked_handoff) so "which handoff is newer" has exactly one
    definition.
    """
    match = HANDOFF_NAME_RE.match(name)
    return None if match is None else (match.group(2), match.group(1))


def latest_handoff(root: Path) -> Path | None:
    """Newest docs/HANDOFF-BATCH-<letter>-<date>.zh-CN.md by (date, letter).

    Batch letters within one date (e.g. G/H/J all 2026-11-13) break the tie
    chronologically. Returns None when no handoff matches — callers fail
    closed on that.
    """
    newest: tuple[tuple[str, str], Path] | None = None
    for path in (root / "docs").glob("HANDOFF-BATCH-*.zh-CN.md"):
        key = _handoff_sort_key(path.name)
        if key is None:
            continue
        if newest is None or key > newest[0]:
            newest = (key, path)
    return None if newest is None else newest[1]


def _resolve_status_link(root: Path, link: str) -> tuple[Path | None, str | None]:
    """Resolve one STATUS link target against STATUS's own directory (docs/).

    The fragment is stripped, backslashes are normalized to "/", ""/"."
    segments dropped and ".." segments popped lexically — so the result is
    the file the link actually names, never a basename-only guess. Absolute
    jumps (leading "/", leading "\\", Windows drive letters) and any ".."
    that pops above docs/ are rejected; external URLs ("://" or mailto:)
    can never resolve to a local file. Returns (resolved_path, None) or
    (None, error_kind) with error_kind in {"external", "absolute",
    "escape"} — callers report each kind distinctly.
    """
    target = link.split("#", 1)[0]
    if "://" in link or link.startswith("mailto:"):
        return None, "external"
    if target.startswith(("/", "\\")) or WINDOWS_DRIVE_RE.match(target):
        return None, "absolute"
    parts: list[str] = []
    for part in target.replace("\\", "/").split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                return None, "escape"
            parts.pop()
        else:
            parts.append(part)
    return root.joinpath(Path(STATUS_DOC).parent, *parts), None


def status_handoff_issues(root: Path, status_text: str) -> tuple[Path | None, list[str]]:
    """K-R6 + K2-R4 + K3: resolve EVERY handoff link STATUS carries and gate it.

    STATUS indexes the current handoff from several sections (当前状态,
    最新验证, 交接入口) and keeps earlier batches as history links, so the
    page legitimately contains several HANDOFF-BATCH-* links. K3
    (REVIEW_2026-11-15-batchK3.zh-CN.md §1): EVERY link naming the CURRENT
    batch (same date+letter as the newest docs/HANDOFF-BATCH-* file) must
    resolve — relative to STATUS's own directory (docs/), fragment
    stripped, no absolute jumps or .. escapes — to EXACTLY that canonical
    file. Selecting one candidate by max()/first-wins let a canonical link
    early in STATUS mask archive/ or external links later on the page, so
    the check result depended on link order. Links naming older batches
    are history: they must still resolve to existing local files, but they
    are not compared against the current handoff.

    K4 (REVIEW_2026-11-15-batchK4.zh-CN.md §1): links are matched over the
    WHOLE document — Markdown labels legally span lines, and the previous
    per-line scan silently skipped a multi-line-label archive link. The
    optional CommonMark link title is parsed consistently by every
    MARKDOWN_LINK_RE consumer (handoff gate and repo-wide walk alike).
    Each failing link is reported with its STARTING line number.

    Returns (canonical newest handoff path the CR-P3-20 checks must anchor
    to, errors). The run-number/contradiction gates always anchor to the
    canonical doc — with all current-batch links gated to it, the actual
    entry point IS that file. Failure modes, all fail-closed:
    - no HANDOFF-BATCH-* file in docs/ at all (pre-K-R6 behavior kept);
    - STATUS links no batch handoff while handoff files exist, or links
      only older batches while a newer handoff file exists (stale entry
      point) — the canonical file is still returned so the run-number
      gate keeps running;
    - any handoff link that is an external URL (reported distinctly —
      external URLs can never satisfy the anchoring), an absolute jump, a
      path escaping docs/, or a target that is not an existing file;
    - any current-batch link resolving to a different file than the
      canonical newest handoff (archive/ copy, renamed/moved file). Each
      failing link is reported with its STATUS line number.
    """
    newest = latest_handoff(root)
    if newest is None:
        return None, [
            "no docs/HANDOFF-BATCH-*.zh-CN.md found for CR-P3-20 consistency checks"
        ]
    canonical = newest.resolve()
    newest_key = _handoff_sort_key(newest.name)
    errors: list[str] = []
    saw_handoff_link = False
    saw_current_batch_link = False
    highest: tuple[tuple[str, str], str] | None = None
    # Whole-text matching (K4 review §1): finditer over the full document
    # finds every link — including labels that legally span lines, which a
    # per-line scan silently skipped. The line number for each error comes
    # from the match's start offset, so a multi-line label is reported at
    # its starting line.
    for match in MARKDOWN_LINK_RE.finditer(status_text):
        link = match.group(1)
        lineno = status_text.count("\n", 0, match.start()) + 1
        name = link.split("#", 1)[0].replace("\\", "/").split("/")[-1]
        key = _handoff_sort_key(name)
        if key is None:
            continue
        saw_handoff_link = True
        if highest is None or key > highest[0]:
            highest = (key, name)
        if key == newest_key:
            saw_current_batch_link = True
        loc = f"{STATUS_DOC}:{lineno}"
        resolved, kind = _resolve_status_link(root, link)
        if kind == "external":
            errors.append(
                f"{loc}: handoff link {link} is an external URL — handoff "
                f"links must resolve to local files under docs/ (current "
                f"handoff: docs/{newest.name})"
            )
            continue
        if kind == "absolute":
            errors.append(
                f"{loc}: handoff link {link} is an absolute path — link "
                f"handoffs relative to docs/ (current handoff: docs/{newest.name})"
            )
            continue
        if kind == "escape":
            errors.append(
                f"{loc}: handoff link {link} escapes docs/ — link handoffs "
                f"inside docs/ (current handoff: docs/{newest.name})"
            )
            continue
        if not resolved.is_file():
            errors.append(
                f"{loc}: handoff link {link} resolves to "
                f"{resolved.relative_to(root).as_posix()}, which does not "
                "exist as a file"
            )
            continue
        if key == newest_key and resolved.resolve() != canonical:
            errors.append(
                f"{loc}: handoff link {link} resolves to "
                f"{resolved.relative_to(root).as_posix()} but the current "
                f"handoff is docs/{newest.name} — every current-batch link "
                "must resolve to that exact file (update the STATUS link)"
            )
    if not saw_handoff_link:
        return newest, errors + [
            f"{STATUS_DOC}: no HANDOFF-BATCH-*.zh-CN.md link found but handoff "
            f"files exist — link the current handoff {newest.name}"
        ]
    if not saw_current_batch_link:
        # Stale entry point (K-R6): the accumulated per-link errors (a
        # missing older-batch target, …) stay alongside this one.
        return newest, errors + [
            f"{STATUS_DOC}: links handoff up to {highest[1]} but the current "
            f"handoff is docs/{newest.name} — update the STATUS link to that "
            "exact file"
        ]
    return newest, errors


def _section_body(text: str, heading: str) -> list[str] | None:
    """Lines of the first `##` section whose title starts with `heading`."""
    lines = text.splitlines()
    start: int | None = None
    end = len(lines)
    for i, line in enumerate(lines):
        if line.startswith(heading):
            start = i
        elif start is not None and line.startswith("## "):
            end = i
            break
    if start is None:
        return None
    return lines[start + 1:end]


def _excerpt(line: str, word: str, width: int = 24) -> str:
    """Short excerpt of the original line around `word` for error messages."""
    pos = line.find(word)
    if pos < 0:
        return line.strip()[:2 * width]
    lo = max(0, pos - width)
    hi = min(len(line), pos + len(word) + width)
    return f"'{'…' if lo > 0 else ''}{line[lo:hi].strip()}{'…' if hi < len(line) else ''}'"


def handoff_run_number_issues(
    status_text: str, handoff_text: str, handoff_name: str
) -> tuple[int, list[str]]:
    """CR-P3-20 check a: STATUS "最新验证" run ids must exist in the handoff.

    Run numbers and counts live only in the current batch handoff's evidence
    tables (AGENTS.md "同一事实只写一处"), so every run id indexed by STATUS's
    "最新验证" section must be findable in the newest HANDOFF-BATCH document.
    Run ids are bare 9–15 digit runs, which covers both the ``[43439835214]``
    and plain ``43439835214`` forms (and the digits inside run URLs, which
    are the same number). Fails closed when the section is missing.
    """
    body = _section_body(status_text, LATEST_VERIFICATION_HEADING)
    if body is None:
        return 0, [f"{STATUS_DOC}: '{LATEST_VERIFICATION_HEADING[3:]}' section not found"]
    run_ids = set(RUN_ID_RE.findall("\n".join(body)))
    handoff_run_ids = set(RUN_ID_RE.findall(handoff_text))
    errors = [
        f"{STATUS_DOC}: run {run_id} listed in '{LATEST_VERIFICATION_HEADING[3:]}' "
        f"but not found in latest handoff {handoff_name}"
        for run_id in sorted(run_ids - handoff_run_ids)
    ]
    return len(run_ids), errors


def handoff_contradiction_issues(handoff_text: str, handoff_name: str) -> list[str]:
    """CR-P3-20 check b: newest handoff must not mix 已核 with pending words.

    A direct (non-quoted) "已核"/"一致性已核" claim and a direct
    尚待/待补/仍待/待执行 placeholder must not coexist in the same document —
    that combination is exactly the fifth drift CR-P3-20 caught. Occurrences
    inside quotes (quoting someone, or "only write X after ..." instructions)
    are ignored. Errors carry line numbers plus original excerpts so a human
    can confirm or waive them.
    """
    confirmed: list[str] = []
    pending: list[str] = []
    for lineno, line in enumerate(handoff_text.splitlines(), 1):
        visible = line
        for pattern in QUOTED_SPAN_PATTERNS:
            visible = pattern.sub("", visible)
        for match in CONFIRMED_RE.finditer(visible):
            confirmed.append(f"{handoff_name}:{lineno}: {_excerpt(line, match.group(0))}")
        for match in PENDING_RE.finditer(visible):
            pending.append(f"{handoff_name}:{lineno}: {_excerpt(line, match.group(0))}")
    if not (confirmed and pending):
        return []
    extra_confirmed = len(confirmed) - 3
    extra_pending = len(pending) - 3
    return [
        f"{handoff_name}: '已核' claim coexists with pending wording "
        "(尚待/待补/仍待/待执行) — 已核 at " + "; ".join(confirmed[:3])
        + (f" (+{extra_confirmed} more)" if extra_confirmed > 0 else "")
        + " — pending at " + "; ".join(pending[:3])
        + (f" (+{extra_pending} more)" if extra_pending > 0 else "")
    ]


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
        for link in MARKDOWN_LINK_RE.findall(text):
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
    # CR-P3-20: STATUS "最新验证" run ids must be traceable in the handoff
    # STATUS actually links to, and that handoff must not mix 已核 claims
    # with 尚待/待补/仍待/待执行 placeholders. K-R6
    # (REVIEW_2026-11-14-batchK.zh-CN.md §1): the link itself is gated too —
    # picking docs/'s newest HANDOFF-BATCH-* by filename silently validated
    # an unlinked handoff while STATUS still pointed at the previous batch.
    # K3 (REVIEW_2026-11-15-batchK3.zh-CN.md §1): every current-batch link
    # is gated to the canonical newest file, and the run/contradiction
    # gates anchor to that canonical doc. Fail closed when either document
    # cannot be located or read.
    latest_runs_checked = 0
    latest_handoff_name: str | None = None
    try:
        status_text = (root / STATUS_DOC).read_text(encoding="utf-8")
    except OSError as exc:
        errors.append(f"cannot read CR-P3-20 documents: {exc}")
    else:
        handoff_path, handoff_errors = status_handoff_issues(root, status_text)
        errors.extend(handoff_errors)
        if handoff_path is not None:
            try:
                handoff_text = handoff_path.read_text(encoding="utf-8")
            except OSError as exc:
                errors.append(f"cannot read CR-P3-20 documents: {exc}")
            else:
                latest_handoff_name = handoff_path.relative_to(root).as_posix()
                latest_runs_checked, run_errors = handoff_run_number_issues(
                    status_text, handoff_text, latest_handoff_name
                )
                errors.extend(run_errors)
                errors.extend(
                    handoff_contradiction_issues(handoff_text, latest_handoff_name)
                )
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
        "status_latest_verification_runs_checked": latest_runs_checked,
        "latest_handoff_checked": latest_handoff_name,
        "banned_dependency_scan": "clean" if not violations else violations,
        "network_tests_executed": False,
        "contract_consistency_warning": contract_consistency,
        "errors": errors,
    }


if __name__ == "__main__":
    result = check(ROOT)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    sys.exit(0 if result["passed"] else 1)
