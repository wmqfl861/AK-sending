"""Tests for the T03 pairing-database snapshot script (a test diagnostic, not product code)."""
from __future__ import annotations
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

script = Path(__file__).resolve().parents[1] / "t03_pairing_snapshot.py"
spec = importlib.util.spec_from_file_location("t03_pairing_snapshot", script)
assert spec is not None and spec.loader is not None
snapshot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(snapshot)

NOW = 1_800_000_000_000

# Only the columns the snapshot reads; constraints are irrelevant here.
DDL = """
CREATE TABLE invite_codes (
  lookup_digest BLOB(32) PRIMARY KEY, code_id BLOB(16) UNIQUE NOT NULL,
  owner_device_id BLOB(32) NOT NULL, created_unix_ms INTEGER NOT NULL,
  expires_unix_ms INTEGER NULL,
  state TEXT NOT NULL CHECK (state IN ('active','revoked','expired')),
  revision INTEGER NOT NULL, create_request_id BLOB(16) NOT NULL,
  owner_endpoint BLOB NULL, max_redeems INTEGER NOT NULL DEFAULT 0);
CREATE TABLE pairing_transactions (
  pairing_id BLOB(16) PRIMARY KEY, state TEXT NOT NULL,
  created_unix_ms INTEGER NOT NULL, updated_unix_ms INTEGER NOT NULL,
  deadline_unix_ms INTEGER NOT NULL, code_revision INTEGER NOT NULL,
  owner_proof_mac BLOB(32), redeemer_proof_mac BLOB(32),
  owner_prepared_signature BLOB(64), redeemer_prepared_signature BLOB(64),
  commit_revision INTEGER, owner_applied_signature BLOB(64),
  redeemer_applied_signature BLOB(64), owner_endpoint BLOB, redeemer_endpoint BLOB);
CREATE TABLE pairing_relations (pairing_id BLOB(16) PRIMARY KEY);
"""

CODE_DETAIL_KEYS = {"state", "age_ms", "expires_in_ms", "max_redeems", "has_owner_endpoint"}
TRANSACTION_DETAIL_KEYS = {
    "state", "age_ms", "updated_age_ms", "deadline_in_ms", "code_revision",
    "owner_proof", "redeemer_proof", "owner_prepared", "redeemer_prepared",
    "committed", "owner_applied", "redeemer_applied", "owner_endpoint", "redeemer_endpoint",
}


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.db = Path(self._tmp.name) / "registry.db"

    def tearDown(self):
        self._tmp.cleanup()

    def _create(self, expires_unix_ms=NOW + 595_000, with_rows=True):
        conn = sqlite3.connect(str(self.db))
        conn.executescript(DDL)
        if with_rows:
            conn.execute(
                "INSERT INTO invite_codes VALUES (?,?,?,?,?,?,?,?,?,?)",
                (b"\x01" * 32, b"\x02" * 16, b"\x03" * 32, NOW - 5_000, expires_unix_ms,
                 "active", 3, b"\x04" * 16, b"endpoint", 1),
            )
            # Swept or aborted before the joiner's half arrived: owner proof only.
            conn.execute(
                "INSERT INTO pairing_transactions (pairing_id, state, created_unix_ms,"
                " updated_unix_ms, deadline_unix_ms, code_revision, owner_proof_mac)"
                " VALUES (?,?,?,?,?,?,?)",
                (b"\x05" * 16, "aborted", NOW - 4_000, NOW - 3_000, NOW + 6_000, 3, b"\x06" * 32),
            )
            # Both halves and the commit present; Applied not yet.
            conn.execute(
                "INSERT INTO pairing_transactions (pairing_id, state, created_unix_ms,"
                " updated_unix_ms, deadline_unix_ms, code_revision, owner_proof_mac,"
                " redeemer_proof_mac, owner_prepared_signature, redeemer_prepared_signature,"
                " commit_revision, owner_endpoint, redeemer_endpoint)"
                " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (b"\x07" * 16, "committed", NOW - 4_000, NOW - 2_500, NOW + 6_000, 3,
                 b"\x08" * 32, b"\x09" * 32, b"\x0a" * 64, b"\x0b" * 64, 7, b"owner", b"redeemer"),
            )
            conn.execute("INSERT INTO pairing_relations VALUES (?)", (b"\x0c" * 16,))
        conn.commit()
        conn.close()

    @staticmethod
    def _detail(out, table, state):
        return next(row for row in out[table]["row_detail"] if row["state"] == state)

    def test_missing_file_is_reported_and_never_created(self):
        out = snapshot.read_snapshot(str(self.db), NOW)
        self.assertFalse(out["ok"])
        self.assertEqual(out["error_kind"], "missing")
        self.assertFalse(self.db.exists(), "mode=ro must not create the database")

    def test_tables_absent_before_the_registry_creates_them(self):
        sqlite3.connect(str(self.db)).close()  # an empty database file
        out = snapshot.read_snapshot(str(self.db), NOW)
        self.assertTrue(out["ok"])
        for table in ("invite_codes", "pairing_transactions", "pairing_relations"):
            self.assertEqual(out[table], {"present": False}, table)

    def test_states_ages_and_flags_are_read(self):
        self._create()
        out = snapshot.read_snapshot(str(self.db), NOW)
        self.assertTrue(out["ok"])
        self.assertEqual(out["now_unix_ms"], NOW)
        codes = out["invite_codes"]
        self.assertEqual((codes["rows"], codes["by_state"], codes["active_unexpired"]), (1, {"active": 1}, 1))
        self.assertEqual(codes["row_detail"], [{
            "state": "active", "age_ms": 5_000, "expires_in_ms": 595_000,
            "max_redeems": 1, "has_owner_endpoint": True,
        }])
        transactions = out["pairing_transactions"]
        self.assertEqual(transactions["rows"], 2)
        self.assertEqual(transactions["by_state"], {"aborted": 1, "committed": 1})
        aborted = self._detail(out, "pairing_transactions", "aborted")
        self.assertEqual((aborted["age_ms"], aborted["updated_age_ms"], aborted["deadline_in_ms"]), (4_000, 3_000, 6_000))
        self.assertEqual(aborted["code_revision"], 3)
        self.assertTrue(aborted["owner_proof"])
        for key in ("redeemer_proof", "owner_prepared", "redeemer_prepared", "committed", "owner_applied"):
            self.assertFalse(aborted[key], key)
        committed = self._detail(out, "pairing_transactions", "committed")
        for key in ("owner_proof", "redeemer_proof", "owner_prepared", "redeemer_prepared",
                    "committed", "owner_endpoint", "redeemer_endpoint"):
            self.assertTrue(committed[key], key)
        self.assertFalse(committed["owner_applied"])
        self.assertEqual(committed["updated_age_ms"], 2_500)
        self.assertEqual(out["pairing_relations"], {"rows": 1})

    def test_an_expired_code_is_not_counted_as_live(self):
        self._create(expires_unix_ms=NOW - 1)
        out = snapshot.read_snapshot(str(self.db), NOW)
        codes = out["invite_codes"]
        self.assertEqual(codes["by_state"], {"active": 1})  # the sweeper has not rewritten it yet
        self.assertEqual(codes["active_unexpired"], 0)
        self.assertEqual(codes["row_detail"][0]["expires_in_ms"], -1)

    def test_output_carries_no_row_material(self):
        self._create()
        out = snapshot.read_snapshot(str(self.db), NOW)
        text = json.dumps(out, sort_keys=True)
        self.assertNotRegex(text, r"[0-9a-f]{16,}")
        self.assertNotIn("AKI1-", text)
        self.assertNotIn(self._tmp.name, text)
        self.assertTrue(set(out["invite_codes"]["row_detail"][0]) <= CODE_DETAIL_KEYS)
        for row in out["pairing_transactions"]["row_detail"]:
            self.assertTrue(set(row) <= TRANSACTION_DETAIL_KEYS)
        self.assertEqual(set(out) <= {"ok", "now_unix_ms", "journal_mode", "invite_codes",
                                      "pairing_transactions", "pairing_relations"}, True)

    def test_the_read_changes_nothing_and_leaves_no_lock(self):
        self._create()
        before = hashlib.sha256(self.db.read_bytes()).hexdigest()
        self.assertTrue(snapshot.read_snapshot(str(self.db), NOW)["ok"])
        self.assertEqual(hashlib.sha256(self.db.read_bytes()).hexdigest(), before)
        conn = sqlite3.connect(str(self.db))
        conn.execute("INSERT INTO pairing_relations VALUES (?)", (b"\x0d" * 16,))
        conn.commit()
        conn.close()  # a second ordinary connection still writes: no lock was left behind

    def test_cli_prints_one_json_object(self):
        self._create()
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = snapshot.main(["t03_pairing_snapshot.py", str(self.db)])
        self.assertEqual(code, 0)
        self.assertTrue(json.loads(stdout.getvalue())["ok"])
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.assertEqual(snapshot.main(["t03_pairing_snapshot.py"]), 0)
        self.assertEqual(json.loads(stdout.getvalue()), {"ok": False, "error_kind": "usage"})

    def test_the_uri_is_encoded_exactly_once(self):
        # A second encoding would turn a legal `%` into `%25` (and `%20` into `%2520`).
        uri = snapshot._read_only_uri("/tmp/a b/100%#x/registry.db")
        self.assertIn("a%20b/100%25%23x/registry.db", uri)
        self.assertNotIn("%2520", uri)
        self.assertTrue(uri.startswith("file:"))
        self.assertTrue(uri.endswith("?mode=ro"))

    def test_paths_with_spaces_percent_hash_and_non_ascii_are_read(self):
        for name in ("temp dir", "100% done", "hash#dir", "缓存", "a%20b"):
            with self.subTest(name=name):
                base = Path(self._tmp.name) / name
                base.mkdir()
                self.db = base / "registry.db"
                self._create()
                out = snapshot.read_snapshot(str(self.db), NOW)
                self.assertTrue(out["ok"], out)
                self.assertEqual(out["invite_codes"]["active_unexpired"], 1)
                self.assertEqual(out["pairing_transactions"]["rows"], 2)


if __name__ == "__main__":
    unittest.main()
