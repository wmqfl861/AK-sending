#!/usr/bin/env python3
"""Read-only snapshot of one v2 pairing registry database, printed as JSON.

Q-N3 recurrence (2026-11-27): crates/ak-itests/tests/t03_ipv6_loopback.rs
spawns a real `ak-cli registry-server` per test with its database under a
temp dir. When a task-code chain fails (the joiner got PAIR_UNAVAILABLE and
timed out), the test reports what the three processes said -- but the
registry's INFO log does not name the branch that answered, and a
transaction row's final state alone cannot tell a sweep from an abort from a
late Prepared. This script reads that database ONCE, read-only, while the
registry is still alive, and prints one JSON object holding only counts,
states, ages and "is this column set" flags:

- invite_codes: rows by state, how many are active and not expired, and per
  row the age, the time left until expiry, max_redeems and whether an owner
  endpoint is stored;
- pairing_transactions: rows by state, and per row the state, the age, the
  time left until the deadline, the time since the last update and which of
  the proof / Prepared / Commit / Applied columns are set;
- pairing_relations: the row count.

Nothing else leaves the database: no ids, digests, signatures, endpoints or
paths are printed. The database is opened with `mode=ro` and every read runs
inside one deferred transaction, so the picture is consistent and no byte is
changed.

Usage
-----
    python -I scripts/t03_pairing_snapshot.py <db-path>

One JSON object goes to stdout. `ok` is false with an `error_kind` when the
database cannot be read (missing file, locked, schema not created yet); the
exit status is 0 whenever an object was printed, so the caller records the
outcome instead of failing the test over a missing snapshot.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
import time
import urllib.request

MAX_ROWS = 64
TRANSACTION_FLAGS = (
    "owner_proof",
    "redeemer_proof",
    "owner_prepared",
    "redeemer_prepared",
    "committed",
    "owner_applied",
    "redeemer_applied",
    "owner_endpoint",
    "redeemer_endpoint",
)


def _read_only_uri(path: str) -> str:
    """`file:` URI that opens an existing database without creating it.

    `pathname2url` already percent-encodes the path once; encoding it again
    would turn a legal `%` into `%25` and make SQLite look for another file.
    """
    return "file:" + urllib.request.pathname2url(path) + "?mode=ro"


def _by_state(rows: list) -> dict:
    counts: dict = {}
    for row in rows:
        counts[row[0]] = counts.get(row[0], 0) + 1
    return counts


def _invite_codes(conn: sqlite3.Connection, now_ms: int) -> dict:
    # state, created, expires, max_redeems, owner endpoint present
    rows = conn.execute(
        "SELECT state, created_unix_ms, expires_unix_ms, max_redeems,"
        " owner_endpoint IS NOT NULL"
        " FROM invite_codes ORDER BY created_unix_ms LIMIT ?",
        (MAX_ROWS,),
    ).fetchall()
    detail = []
    for state, created, expires, max_redeems, endpoint in rows:
        detail.append(
            {
                "state": state,
                "age_ms": now_ms - created,
                "expires_in_ms": None if expires is None else expires - now_ms,
                "max_redeems": max_redeems,
                "has_owner_endpoint": bool(endpoint),
            }
        )
    active_unexpired = sum(
        1
        for state, _created, expires, _max, _endpoint in rows
        if state == "active" and (expires is None or expires > now_ms)
    )
    return {
        "rows": len(rows),
        "by_state": _by_state(rows),
        "active_unexpired": active_unexpired,
        "row_detail": detail,
    }


def _pairing_transactions(conn: sqlite3.Connection, now_ms: int) -> dict:
    rows = conn.execute(
        "SELECT state, created_unix_ms, updated_unix_ms, deadline_unix_ms, code_revision,"
        " owner_proof_mac IS NOT NULL, redeemer_proof_mac IS NOT NULL,"
        " owner_prepared_signature IS NOT NULL, redeemer_prepared_signature IS NOT NULL,"
        " commit_revision IS NOT NULL,"
        " owner_applied_signature IS NOT NULL, redeemer_applied_signature IS NOT NULL,"
        " owner_endpoint IS NOT NULL, redeemer_endpoint IS NOT NULL"
        " FROM pairing_transactions ORDER BY created_unix_ms LIMIT ?",
        (MAX_ROWS,),
    ).fetchall()
    detail = []
    for row in rows:
        item = {
            "state": row[0],
            "age_ms": now_ms - row[1],
            "updated_age_ms": now_ms - row[2],
            "deadline_in_ms": row[3] - now_ms,
            "code_revision": row[4],
        }
        for name, value in zip(TRANSACTION_FLAGS, row[5:]):
            item[name] = bool(value)
        detail.append(item)
    return {"rows": len(rows), "by_state": _by_state(rows), "row_detail": detail}


def _read_all(conn: sqlite3.Connection, now_ms: int) -> dict:
    """Every read inside one deferred transaction (one consistent picture)."""
    conn.execute("BEGIN DEFERRED")
    try:
        tables = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
                " AND name IN ('invite_codes', 'pairing_transactions', 'pairing_relations')"
            )
        }
        out = {}
        for name, reader in (
            ("invite_codes", _invite_codes),
            ("pairing_transactions", _pairing_transactions),
        ):
            out[name] = reader(conn, now_ms) if name in tables else {"present": False}
        if "pairing_relations" in tables:
            count = conn.execute("SELECT COUNT(*) FROM pairing_relations").fetchone()[0]
            out["pairing_relations"] = {"rows": count}
        else:
            out["pairing_relations"] = {"present": False}
    finally:
        try:
            conn.execute("COMMIT")
        except sqlite3.Error:
            pass  # a failed read rolled the transaction back; the connection is closed next
    return out


def read_snapshot(path: str, now_ms: int | None = None) -> dict:
    """The JSON-able snapshot of the database at `path` (see the module doc).

    `now_ms` is the wall-clock millisecond the ages are measured against;
    it defaults to the current time and is recorded in the result so a
    caller can line the ages up with its own timeline.
    """
    if now_ms is None:
        now_ms = int(time.time() * 1000)
    out: dict = {"ok": False, "now_unix_ms": now_ms}
    if not os.path.isfile(path):
        out["error_kind"] = "missing"
        return out
    try:
        conn = sqlite3.connect(_read_only_uri(path), uri=True, timeout=2.0, isolation_level=None)
        try:
            conn.execute("PRAGMA query_only = ON")
            out["journal_mode"] = str(conn.execute("PRAGMA journal_mode").fetchone()[0])
            out.update(_read_all(conn, now_ms))
        finally:
            conn.close()
        out["ok"] = True
    except Exception as error:  # the caller records the kind; nothing else is known here
        out["error_kind"] = type(error).__name__
        out["error"] = str(error).replace(path, "<db>")[:200]
    return out


def main(argv: list) -> int:
    if len(argv) != 2:
        print(json.dumps({"ok": False, "error_kind": "usage"}, sort_keys=True))
        return 0
    print(json.dumps(read_snapshot(argv[1]), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
