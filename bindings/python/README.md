# slimdb (Python)

The tiny embedded database for local apps. No server. No configuration. No SQL.

```python
import slimdb

with slimdb.open("game.sdb") as db:
    db.set("player.level", 42)
    db.set("player.name", "Aurel")
    assert db.get("player.level") == 42
```

## Install

```bash
pip install .
```

Requires a Rust toolchain (the wheel builds the C ABI and the PyO3 module
from source). Prebuilt wheels for Linux/macOS/Windows are a Phase 3 goal
(spec §12: `pip install slimdb` without local compilation).

## Design

Per the spec, this binding sits on the SlimDB **C ABI** (`bindings/c`), never
on the core's internal structures. Values cross the boundary as JSON — the
API model, never the storage format.

## API

`open(path, durability="safe")` → `SlimDb` with `set / get / delete / exists /
keys / clear / flush / compact / backup / restore / stats / transaction /
close`, plus `with` support. See `slimdb/__init__.py` and `examples/demo.py`.
