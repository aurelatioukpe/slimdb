# SlimDB

The tiny embedded database for local apps.

No server. No configuration. No SQL.

SlimDB replaces your `config.json` / `save.json` / `cache` files with a real
database — without ever running a server.

```rust
use slimdb_core::{Db, Value};

let mut db = Db::open("game.sdb")?;
db.set("player.level", Value::Int(42))?;
db.set("player.name", Value::Str("Aurel".to_string()))?;
assert_eq!(db.get("player.level")?, Some(Value::Int(42)));
db.close()?;
```

That's it: **Open → Set → Done**. No migrations, no daemon, no config file.
Data is crash-safe by default (fsync per write), recovered automatically on
open, and one file per database (`game.sdb` + a small `game.sdb-wal` log).

## Why not just a JSON file?

JSON files are fine until a `kill -9` or a power cut corrupts them. SlimDB
writes are crash-safe: every committed write survives, torn writes are
detected and truncated at recovery, and the on-disk format is checksummed
and versioned. See `docs/FORMAT.md` for the full spec.

## Feature set (v0.1)

- Values: `null`, `bool`, `int`, `float`, `string`, `binary`, `array`, `object`
- Path access: `db.get("player.level")`, `db.set("player.inventory.sword", 5)`
- Transactions: `begin` → writes → `commit` / `rollback`, atomic across crashes
- Durability modes: `safe` (default, fsync per write), `balanced`, `fast`
- `compact` (WAL → snapshot, atomic), `backup` / `restore`, `stats`, `flush`
- CLI: `slimdb game.sdb get player.level`

## CLI

```
slimdb game.sdb set player.level 42
slimdb game.sdb set player '{"name": "Aurel"}'
slimdb game.sdb get player.level
slimdb game.sdb keys
slimdb game.sdb stats
slimdb game.sdb compact
```

## Bindings

All bindings sit on the C ABI (`slimdb_*`, the fundamental contract), never
on the core's internals:

- **C** — `bindings/c`: hand-written `slimdb.h`, staticlib + cdylib, example, tests
- **Python** — `bindings/python`: PyO3 on the C ABI, `pip install .`

```python
import slimdb
with slimdb.open("game.sdb") as db:
    db.set("player.level", 42)
```

## Scope (documented limits)

Designed for local, single-process apps: **< 100 000 documents, < 1 GB**.
Not a server, not distributed, not an analytics engine. See
`docs/SPEC.md` for the full design contract.

## Safety

- Safe by default: every committed transaction is fsynced.
- Crash-safe: the signature test is a `kill -9` loop — random writes,
  random kill, reopen, state must match exactly what was committed
  (`tests/crash-tester`, plus an in-process torn-write fuzz in
  `core/tests/fuzz_torn.rs` that also injects mid-file corruption and
  requires a loud error, never a silent truncation).
- A nightly job runs a large batch (200k SIGKILLs/night by default,
  configurable up to the 1M-iteration DoD target) with parallel runners;
  every push runs a 200-iteration gate.
- The `*.sdb` file is only ever replaced atomically (temp + fsync + rename);
  a `*.sdb` that exists is never partially written.

## Building

```
cargo build --release
cargo test --workspace          # unit + integration tests
cargo test -p slimdb-core --release --test fuzz_torn   # torn-write fuzz (250 iters)
./target/release/slimdb-runner /tmp/crash 200          # kill -9 loop (200 iters)
```

## Roadmap status

| Phase | Status |
|---|---|
| 0 — Prototype jetable | superseded by the real engine (API fixed by the spec) |
| 1 — Moteur réel (format, WAL, recovery, crash test) | ✅ |
| 2 — API stable (txns, compaction, backup/restore, CLI) | ✅ |
| 3 — Binding C ABI + Python (PyO3 sur le C ABI, `pip install .`) | ✅ |
| 4 — Node (N-API) | planned |
| 5 — Godot (GDExtension) | planned |
| 6 — Durcissement (fuzzing, benchmarks, cross-platform CI) | in progress |

## License

MIT
