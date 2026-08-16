"""Tests for the slimdb Python binding (unittest, no external dependencies).

Run with:  python -m unittest discover -s tests
"""

import os
import shutil
import tempfile
import unittest

import slimdb


class SlimDbTestCase(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="slimdb-test-")
        self.path = os.path.join(self.dir, "t.sdb")

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def test_set_get_delete_exists(self):
        db = slimdb.open(self.path)
        db.set("player.level", 42)
        db.set("player.name", "Aurel")
        db.set("items", [1, 2.5, None, True])
        assert db.get("player.level") == 42
        assert db.get("player.name") == "Aurel"
        assert db.get("items") == [1, 2.5, None, True]
        assert db.get("missing") is None
        assert db.exists("player.level")
        assert not db.exists("missing")
        assert db.delete("player.name") is True
        assert db.delete("player.name") is False
        assert db.get("player.name") is None
        db.close()

    def test_reopen_persistence(self):
        with slimdb.open(self.path) as db:
            db.set("a", {"x": 1, "y": [True, "z"]})
        with slimdb.open(self.path) as db:
            assert db.get("a") == {"x": 1, "y": [True, "z"]}

    def test_transaction_commit(self):
        with slimdb.open(self.path) as db:
            with db.transaction() as txn:
                txn.set("a", 1)
                txn.set("b.c", 2)
            assert db.get("a") == 1
            assert db.get("b.c") == 2

    def test_transaction_rollback_on_exception(self):
        """An exception inside a `with db.transaction()` block must roll back."""
        with slimdb.open(self.path) as db:
            db.set("a", 1)
            with self.assertRaises(ValueError):
                with db.transaction() as txn:
                    txn.set("a", 99)
                    txn.set("b", 2)
                    raise ValueError("boom")
            assert db.get("a") == 1, "transaction must be rolled back on exception"
            assert db.get("b") is None
            # The database stays usable after the rollback.
            db.set("c", 3)
            assert db.get("c") == 3

    def test_transaction_explicit_rollback(self):
        with slimdb.open(self.path) as db:
            db.set("a", 1)
            txn = db.transaction()
            txn.set("a", 99)
            txn.rollback()
            assert db.get("a") == 1

    def test_type_mismatch_raises(self):
        with slimdb.open(self.path) as db:
            db.set("a", 5)
            with self.assertRaises(Exception):
                db.set("a.b", 1)
            assert db.get("a") == 5

    def test_keys_sorted(self):
        with slimdb.open(self.path) as db:
            db.set("zeta", 1)
            db.set("alpha", 2)
            db.set("mid.x", 3)
            assert db.keys() == ["alpha", "mid", "zeta"]

    def test_clear_and_compact(self):
        with slimdb.open(self.path) as db:
            db.set("a", 1)
            db.set("b", 2)
            db.compact()
            assert db.get("a") == 1
            db.clear()
            assert db.keys() == []
            db.set("after", "ok")
        with slimdb.open(self.path) as db:
            assert db.get("after") == "ok"

    def test_backup_restore(self):
        backup = os.path.join(self.dir, "backup.sdb")
        with slimdb.open(self.path) as db:
            db.set("level", 7)
            db.backup(backup)
            db.set("level", 99)
            assert db.get("level") == 99
            db.restore(backup)
            assert db.get("level") == 7

    def test_durability_modes(self):
        for mode in ("safe", "balanced", "fast"):
            with slimdb.open(self.path, durability=mode) as db:
                db.set("k", mode)
                stats = db.stats()
                assert stats["keys"] == 1
        with self.assertRaises(Exception):
            slimdb.open(self.path, durability="nope")

    def test_close_is_idempotent(self):
        db = slimdb.open(self.path)
        db.set("a", 1)
        db.close()
        db.close()  # second close is a no-op


if __name__ == "__main__":
    unittest.main()
