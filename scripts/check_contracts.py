#!/usr/bin/env python3
"""Contract consistency checks: implementation vs the frozen contracts.

Wave 24 automation of Codex final-review suggestion #1: the documents under
docs/contracts/ are the frozen interface between parallel module owners, so
the numbers the code implements must still match the numbers the contracts
freeze. This script re-verifies that alignment mechanically on the current
tree. It is ADVISORY: scripts/check_repository.py calls it inside
try/except and reports the outcome as a warning, never as a gate failure.
A FAIL is a named drift for the integrator (contract freeze notes require
integrator approval + ADR-004 sync before any edit).

Checks
------
1  wire-message payload budgets
1a docs/contracts/protocol-v2.md sections 3/4/6/6a per-tag budget tables
   (type tag -> payload bytes) vs the per-tag budget constants and TYPE_*
   tag assignments of the v2 codec,
   crates/ak-signal/src/protocol_v2/message.rs;
1b the limits-independent (fixed-size) arms of ak-wire's `payload_budget`,
   crates/ak-wire/src/lib.rs — above all the scheduler admission frames
   (types 13-15) — vs the frozen field layouts of
   docs/contracts/scheduler.md section 6.
2  FFI exported symbols: every `#[no_mangle] pub extern "C" fn` of
   crates/ak-ffi/src/*.rs (lib.rs's shim.rs + sched_ext.rs modules) vs the
   C ABI table of docs/contracts/ffi-v2.md section 1 (18 frozen exports;
   the wave-22 legacy `ak_sched_start` is the one sanctioned extra, "+1").
3  Scheduler configuration fields: the JSON field names
   SchedulerPolicy::from_json accepts, crates/ak-session/src/scheduler/
   config.rs, vs the frozen config keys of docs/contracts/scheduler.md
   section 3.
4  PairError business error codes: the u16 constants of
   crates/ak-signal/src/protocol_v2/message.rs vs the frozen error-code
   table of docs/contracts/pairing-flow.md section 12.

Usage
-----
    python scripts/check_contracts.py

Output: one PASS/FAIL verdict per check with a difference detail. Exit
code 0 whenever every check ran to a verdict (PASS or FAIL — findings are
reported, the integrator decides); 1 only when the tool itself could not
run (missing file, unparseable input). All output is ASCII.
"""
from __future__ import annotations

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]


def _read(rel: str) -> str:
    path = ROOT / rel
    if not path.is_file():
        raise FileNotFoundError(f"required input missing: {rel}")
    return path.read_text(encoding="utf-8")


def _section(text: str, number: str) -> str:
    """Body of the `## <number> ...` heading, up to the next `## ` heading."""
    match = re.search(rf"^## {number}[^\n]*\n(.*?)(?=^## |\Z)", text, re.M | re.S)
    if not match:
        raise ValueError(f"heading '## {number}' not found")
    return match.group(1)


def _fenced(text: str, tag: str) -> list[str]:
    return re.findall(rf"```{tag}\n(.*?)```", text, re.S)


# ---------------------------------------------------------------------------
# 1a. protocol-v2.md budget tables vs ak-signal protocol_v2 constants
# ---------------------------------------------------------------------------

def _camel_to_snake(name: str) -> str:
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).upper()


def _resolve_expr(expr: str, base: dict[str, int]) -> int | None:
    """Evaluate `204`, `NAME` or `A + B` against already-known constants."""
    total = 0
    for term in expr.split("+"):
        term = term.strip()
        if term.isdigit():
            total += int(term)
        elif term in base:
            total += base[term]
        else:
            return None
    return total


def _codec_budget_constants(text: str) -> tuple[dict[str, int], dict[str, int]]:
    """(variant -> payload bytes, VARIANT_SNAKE -> TYPE_ tag int).

    Associates each `pub const ... : usize = ...;` with the first word of
    its doc comment (`/// Hello: 2+32+...` -> variant `Hello`), then sums
    const-referencing expressions (`TRANSCRIPT_BYTES + PAIR_PROOF_BYTES`).
    """
    base: dict[str, int] = {}
    pending: list[tuple[str, str, str]] = []  # (const, expr, variant|None)
    last_doc: str | None = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("///"):
            word = re.match(r"///\s*`?([A-Za-z]+)", stripped)
            last_doc = word.group(1) if word else None
        elif stripped.startswith("pub const ") and ": usize" in stripped:
            m = re.match(r"pub const (\w+): usize = ([^;]+);", stripped)
            if m:
                pending.append((m.group(1), m.group(2).strip(), last_doc))
        elif stripped and not stripped.startswith("//"):
            last_doc = None
    for _ in range(4):  # fixpoint for A + B references
        for name, expr, _variant in pending:
            if name in base:
                continue
            value = _resolve_expr(expr, base)
            if value is not None:
                base[name] = value
    by_variant: dict[str, int] = {}
    for name, _expr, variant in pending:
        if variant and name in base:
            # First const documented for a variant wins (the budget consts
            # are declared right after their `/// Variant: ...` doc line).
            by_variant.setdefault(variant, base[name])
    tags = {
        snake: int(value, 16)
        for snake, value in re.findall(r"const TYPE_(\w+): u8 = (0x[0-9A-Fa-f]+);", text)
    }
    return by_variant, tags


def check_wire_budgets_protocol_v2() -> dict:
    doc = _read("docs/contracts/protocol-v2.md")
    code = _read("crates/ak-signal/src/protocol_v2/message.rs")
    by_variant, code_tags = _codec_budget_constants(code)

    rows: list[tuple[int, str, int]] = []
    for number in ("3", "4", "6", "6a"):
        for tag_hex, variant, budget in re.findall(
            r"^\|\s*`(0x[0-9A-Fa-f]{2})`\s*\|\s*`([A-Za-z]+)`\s*\|.*\|"
            r"\s*≤?\s*(\d+)\s*\|$",
            _section(doc, number),
            re.M,
        ):
            rows.append((int(tag_hex, 16), variant, int(budget)))

    details: list[str] = []
    if not rows:
        details.append("no budget table rows parsed from protocol-v2.md sections 3/4/6")
    for tag, variant, budget in rows:
        snake = _camel_to_snake(variant)
        if variant not in by_variant:
            details.append(f"tag 0x{tag:02X} {variant}: no budget constant found in message.rs")
            continue
        if code_tags.get(snake) != tag:
            details.append(
                f"tag 0x{tag:02X} {variant}: message.rs TYPE_{snake} is "
                f"0x{code_tags.get(snake, -1):02X}"
            )
        actual = by_variant[variant]
        if actual != budget:
            details.append(
                f"tag 0x{tag:02X} {variant}: budget contract={budget} code={actual}"
            )
    return {
        "id": "1a_wire_budgets_protocol_v2",
        "title": "protocol-v2.md budget tables (3/4/6/6a) vs ak-signal codec constants",
        "passed": not details,
        "rows_compared": len(rows),
        "details": details,
    }


# ---------------------------------------------------------------------------
# 1b. ak-wire fixed payload_budget arms vs scheduler.md section 6 layouts
# ---------------------------------------------------------------------------

_ADMISSION_MAP = {
    "TYPE_TASK_OPEN": "TaskOpen",
    "TYPE_ADMISSION": "TaskAdmission",
    "TYPE_ADMISSION_ACK": "TaskAdmissionAck",
}
_U_WIDTH = {"u8": 1, "u16": 2, "u32": 4, "u64": 8, "bool": 1}


def _scheduler_admission_layouts(doc: str) -> dict[str, int]:
    """Byte sums of the frozen TaskOpen/TaskAdmission/TaskAdmissionAck
    field layouts in scheduler.md section 6 (`name[N]`, `name:uK`)."""
    sums: dict[str, int] = {}
    for block in _fenced(_section(doc, "6"), "text"):
        current: str | None = None
        for line in block.splitlines():
            head = re.match(r"^(\w+)$", line.strip())
            if head and not line.startswith((" ", "\t")):
                current = head.group(1)
                sums.setdefault(current, 0)
                continue
            if current is None:
                continue
            field = re.match(r"^\s+(\w+)(?:\[(\d+)\]|:(u8|u16|u32|u64|bool))(?:\s|$)", line)
            if field:
                name, fixed, width = field.groups()
                if fixed is not None:
                    sums[current] += int(fixed)
                elif width in _U_WIDTH:
                    sums[current] += _U_WIDTH[width]
    return sums


def check_wire_budgets_scheduler() -> dict:
    wire = _read("crates/ak-wire/src/lib.rs")
    doc = _read("docs/contracts/scheduler.md")

    fixed: dict[str, int] = {}
    for arms, value in re.findall(
        r"^\s*(TYPE_[A-Z_]+(?:\s*\|\s*TYPE_[A-Z_]+)*)\s*=>\s*(\d+)\s*,", wire, re.M
    ):
        for arm in re.split(r"\s*\|\s*", arms):
            fixed[arm] = int(value)

    contract = _scheduler_admission_layouts(doc)
    details: list[str] = []
    for type_const, message in _ADMISSION_MAP.items():
        code_bytes = fixed.get(type_const)
        doc_bytes = contract.get(message)
        if code_bytes is None or doc_bytes is None:
            details.append(
                f"{message}: missing side (ak-wire {type_const}="
                f"{code_bytes}, scheduler.md §6 layout={doc_bytes})"
            )
        elif code_bytes != doc_bytes:
            details.append(
                f"{message}: ak-wire {type_const} budget {code_bytes} B != "
                f"scheduler.md §6 frozen layout sum {doc_bytes} B"
            )
    others = sorted(k for k in fixed if k not in _ADMISSION_MAP)
    return {
        "id": "1b_wire_budgets_scheduler",
        "title": "ak-wire payload_budget fixed arms vs scheduler.md §6 admission layouts",
        "passed": not details,
        "admission_frames": {
            _ADMISSION_MAP[k]: {"ak_wire_bytes": fixed.get(k),
                                "scheduler_md_layout_bytes": contract.get(
                                    _ADMISSION_MAP[k])}
            for k in _ADMISSION_MAP
        },
        "other_fixed_arms_info": {k: fixed[k] for k in others},
        "details": details,
    }


# ---------------------------------------------------------------------------
# 2. FFI exported symbols vs ffi-v2.md section 1
# ---------------------------------------------------------------------------

_LEGACY_EXTRA = {"ak_sched_start"}  # wave-22 entry, kept; ffi-v2 §1 replaced it


def check_ffi_exports() -> dict:
    doc = _read("docs/contracts/ffi-v2.md")
    contract: set[str] = set()
    for block in _fenced(doc, "c"):
        contract.update(re.findall(r"\bint32_t\s+(\w+)\s*\(", block))

    exported: dict[str, set[str]] = {}
    src = ROOT / "crates/ak-ffi/src"
    for path in sorted(src.glob("*.rs")):
        found = re.findall(r'#\[no_mangle\]\s*pub (?:unsafe )?extern "C" fn (\w+)\s*\(', path.read_text(encoding="utf-8"))
        if found:
            exported[path.name] = set(found)
    code: set[str] = set().union(*exported.values()) if exported else set()

    missing = sorted(contract - code)
    extras = sorted(
        name for name in code - contract
        if name.startswith(("ak_pairing_", "ak_sched_"))
    )
    unexpected = sorted(set(extras) - _LEGACY_EXTRA)
    details: list[str] = []
    if len(contract) != 18:
        details.append(f"ffi-v2.md §1 lists {len(contract)} signatures (expected 18)")
    for name in missing:
        details.append(f"missing export: {name}")
    for name in unexpected:
        details.append(f"export outside the contract: {name}")
    return {
        "id": "2_ffi_export_symbols",
        "title": "ak-ffi #[no_mangle] exports vs ffi-v2.md §1 ABI table",
        "passed": not details,
        "contract_symbols": len(contract),
        "exported_symbols": len(code),
        "legacy_extras": sorted(set(extras) & _LEGACY_EXTRA),
        "per_file": {name: len(values) for name, values in exported.items()},
        "details": details,
    }


# ---------------------------------------------------------------------------
# 3. Scheduler config fields vs scheduler.md section 3
# ---------------------------------------------------------------------------

def check_scheduler_config_fields() -> dict:
    doc = _read("docs/contracts/scheduler.md")
    code_text = _read("crates/ak-session/src/scheduler/config.rs")

    keys: set[str] = set()
    for block in _fenced(_section(doc, "3"), "json"):
        keys.update(re.findall(r'"(\w+)"\s*:', block))

    start = code_text.find("pub fn from_json")
    if start == -1:
        raise ValueError("SchedulerPolicy::from_json not found in config.rs")
    nxt = re.search(r"^\s*(?:pub )?fn \w+", code_text[start + 10 :], re.M)
    span = code_text[start : start + 10 + (nxt.start() if nxt else len(code_text))]
    accepted = set(re.findall(r'"([a-z_]+)"\s*=>', span))

    details: list[str] = []
    for name in sorted(keys - accepted):
        details.append(f"contract key not accepted by from_json: {name}")
    for name in sorted(accepted - keys):
        details.append(f"from_json accepts field outside contract §3: {name}")
    return {
        "id": "3_scheduler_config_fields",
        "title": "SchedulerPolicy::from_json fields vs scheduler.md §3 config keys",
        "passed": not details,
        "contract_keys": sorted(keys),
        "accepted_fields": sorted(accepted),
        "details": details,
    }


# ---------------------------------------------------------------------------
# 4. PairError codes vs pairing-flow.md section 12
# ---------------------------------------------------------------------------

def check_pair_error_codes() -> dict:
    doc = _read("docs/contracts/pairing-flow.md")
    code = _read("crates/ak-signal/src/protocol_v2/message.rs")

    contract: dict[str, int] = {}
    for block in _fenced(_section(doc, "12"), "text"):
        for value, name in re.findall(r"\b(0x[0-9A-Fa-f]{4})\s+(\w+)\b", block):
            contract[name] = int(value, 16)

    implemented: dict[str, int] = {
        name: int(value, 16)
        for name, value in re.findall(r"pub const (\w+): u16 = (0x[0-9A-Fa-f]+);", code)
    }

    details: list[str] = []
    for name in sorted(contract):
        if name not in implemented:
            details.append(f"contract code {name} (0x{contract[name]:04X}) not in message.rs")
        elif implemented[name] != contract[name]:
            details.append(
                f"{name}: contract 0x{contract[name]:04X} != code 0x{implemented[name]:04X}"
            )
    for name in sorted(set(implemented) - set(contract)):
        details.append(f"message.rs constant absent from pairing-flow.md §12: {name}")
    return {
        "id": "4_pair_error_codes",
        "title": "PairError code constants vs pairing-flow.md §12 error table",
        "passed": not details,
        "contract_codes": {n: f"0x{contract[n]:04X}" for n in sorted(contract)},
        "details": details,
    }


ALL_CHECKS = (
    check_wire_budgets_protocol_v2,
    check_wire_budgets_scheduler,
    check_ffi_exports,
    check_scheduler_config_fields,
    check_pair_error_codes,
)


def run_checks(root: Path | None = None) -> list[dict]:
    """Run every check; IO errors propagate (caller decides how to degrade)."""
    global ROOT
    if root is not None and Path(root).resolve() != ROOT:
        ROOT = Path(root).resolve()
    return [check() for check in ALL_CHECKS]


def summarize(root: Path | None = None) -> dict:
    """Compact advisory summary for scripts/check_repository.py."""
    results = run_checks(root)
    failures = [r["id"] for r in results if not r["passed"]]
    return {
        "status": "ok" if not failures else "warning",
        "checks": {r["id"]: "PASS" if r["passed"] else "FAIL" for r in results},
        "failures": failures,
        "failure_details": {
            r["id"]: r["details"] for r in results if not r["passed"]
        },
    }


def main() -> int:
    try:
        results = run_checks()
    except Exception as exc:  # tool-level failure: inputs unreadable/changed shape
        print(f"check_contracts: TOOL ERROR: {exc!r}")
        return 1
    width = 74
    for result in results:
        print("=" * width)
        verdict = "PASS" if result["passed"] else "FAIL"
        print(f"[{verdict}] {result['id']}: {result['title']}")
        for key, value in result.items():
            if key in {"id", "title", "passed", "details"}:
                continue
            print(f"    {key}: {value}")
        for line in result["details"]:
            print(f"    - {line}")
    failed = [r for r in results if not r["passed"]]
    print("=" * width)
    print(f"RESULT: {len(results) - len(failed)}/{len(results)} checks PASS", end="")
    print(f"; FAIL: {', '.join(r['id'] for r in failed)}" if failed else "")
    print("advisory: exit code stays 0; FAIL items are drift reports for the")
    print("integrator (contracts are frozen; edits need approval + ADR-004 sync).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
