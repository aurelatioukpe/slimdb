"""SlimDB Python demo — Open → Set → Done."""

import os
import tempfile

import slimdb

workdir = tempfile.mkdtemp(prefix="slimdb-demo-")
path = os.path.join(workdir, "game.sdb")
print(f"database: {path}")

with slimdb.open(path) as db:
    # Open → Set → Done
    db.set("player.level", 42)
    db.set("player.name", "Aurel")
    db.set("player.inventory", {"sword": 5, "shield": True})
    db.set("visited.levels", [1, 3, 7])

    print("player.level      =", db.get("player.level"))
    print("player.name       =", db.get("player.name"))
    print("player.inventory  =", db.get("player.inventory"))
    print("keys              =", db.keys())

    # Transactions are atomic
    with db.transaction() as txn:
        txn.set("quest.title", "The Tiny Engine")
        txn.set("quest.steps", 3)
    print("quest.title       =", db.get("quest.title"))

    txn = db.transaction()
    txn.set("quest.rolled_back", True)
    txn.rollback()
    print("quest.rolled_back =", db.get("quest.rolled_back"))

    print("stats             =", db.stats())

# Reopen: everything committed survives.
with slimdb.open(path) as db:
    assert db.get("player.level") == 42
    assert db.get("quest.title") == "The Tiny Engine"
    assert db.get("quest.rolled_back") is None
    print("reopened: state intact ✓")

print("demo: OK")
