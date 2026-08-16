"""SlimDB — the tiny embedded database for local apps.

No server. No configuration. No SQL.

    import slimdb

    db = slimdb.open("game.sdb")
    db.set("player.level", 42)
    db.set("player.name", "Aurel")
    assert db.get("player.level") == 42
    db.close()

Data is crash-safe by default (fsync per write). See docs/FORMAT.md for the
on-disk format.
"""

import json as _json

from ._slimdb import Db as _Db
from ._slimdb import Txn as _Txn

__version__ = "0.1.0"

__all__ = ["open", "SlimDb", "Transaction"]


def _dumps(value):
    # JSON is the API interchange model; non-finite floats are rejected
    # because JSON has no NaN/Infinity (they read back as null).
    return _json.dumps(value, separators=(",", ":"), allow_nan=False)


def open(path, durability="safe"):
    """Open (or create) the database at `path`.

    `durability` is one of "safe" (default, fsync per write), "balanced"
    (periodic flush) or "fast" (buffered only).
    """
    return SlimDb(path, durability)


class SlimDb:
    """A SlimDB database handle. Use `slimdb.open()` to create one."""

    def __init__(self, path, durability="safe"):
        self._db = _Db(path, durability)

    def set(self, path, value):
        """Set the value at `path`, creating intermediate objects as needed."""
        self._db.set_json(path, _dumps(value))

    def get(self, path):
        """Return the value at `path`, or None if it does not exist."""
        raw = self._db.get_json(path)
        return None if raw is None else _json.loads(raw)

    def delete(self, path):
        """Delete the value at `path`; returns True if something was removed."""
        return self._db.delete(path)

    def exists(self, path):
        """Whether a value exists at `path`."""
        return self._db.exists(path)

    def keys(self):
        """Top-level keys, sorted."""
        return self._db.keys()

    def clear(self):
        """Remove every key."""
        self._db.clear()

    def flush(self):
        """Flush buffered WAL bytes to disk and fsync."""
        self._db.flush()

    def compact(self):
        """Compact the WAL into the snapshot file (atomic)."""
        self._db.compact()

    def backup(self, dest):
        """Write a standalone, consistent snapshot to `dest`."""
        self._db.backup(dest)

    def restore(self, src):
        """Replace this database's contents with the backup at `src`."""
        self._db.restore(src)

    def stats(self):
        """Database statistics as a dict."""
        return self._db.stats()

    def transaction(self):
        """Begin a transaction (atomic batch of writes)."""
        return Transaction(self._db.transaction())

    def close(self):
        """Close the database (compacts first)."""
        self._db.close()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()

    def __del__(self):
        # Best-effort; prefer explicit close() or a `with` block.
        try:
            self._db.close()
        except Exception:
            pass


class Transaction:
    """An atomic batch of writes. Committed transactions survive crashes;
    dropped or rolled-back transactions leave no trace."""

    def __init__(self, txn):
        self._txn = txn

    def set(self, path, value):
        self._txn.set_json(path, _dumps(value))

    def delete(self, path):
        """Returns True if the path existed."""
        return self._txn.delete(path)

    def clear(self):
        self._txn.clear()

    def commit(self):
        """Commit all writes atomically."""
        self._txn.commit()

    def rollback(self):
        """Discard all writes."""
        self._txn.rollback()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self.commit()
        else:
            self.rollback()
