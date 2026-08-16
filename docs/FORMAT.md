# SlimDB on-disk format (version 1)

This document is the public specification of the `*.sdb` binary format. It is
part of the Phase 1 definition of done: the format is documented here, before
any binding or public announcement.

All integers are little-endian. All checksums are CRC-32 (IEEE 802.3,
reflected, poly `0xEDB88320`, init `0xFFFFFFFF`, final XOR `0xFFFFFFFF`).

## Files

A database is two files:

| File | Role |
|---|---|
| `game.sdb` | Snapshot: compact, complete state. Only ever replaced atomically (temp + fsync + rename). May not exist yet for a brand-new database. |
| `game.sdb-wal` | Write-ahead log: every mutation since the last compaction. Append-only; truncated at compaction and at recovery. |

Both files share the same record framing. The WAL carries a header binding it
to a specific database id; a stale WAL (left over from another database) is
detected and discarded.

## Database snapshot header (32 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Magic: `"SLIMDB\0\0"` |
| 8 | 4 | Format version (`1`) |
| 12 | 8 | Database id (random per database) |
| 20 | 8 | `last_seq`: highest WAL sequence contained in this snapshot (`0` for a fresh database) |
| 28 | 4 | CRC-32 of bytes `[0, 28)` |

## WAL header (20 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | Magic: `"SWAL"` |
| 4 | 4 | Format version (`1`) |
| 8 | 8 | Database id of the database this WAL belongs to |
| 16 | 4 | CRC-32 of bytes `[0, 16)` |

A WAL whose header is missing or invalid is treated as empty and reset. This
is safe because the header is always fsynced before any record is appended:
an invalid header implies no durable records.

## Records

Every record is framed as follows:

| Field | Size | Meaning |
|---|---|---|
| MAGIC | 2 | `"SD"` |
| VERSION | 1 | `1` |
| TYPE | 1 | See below |
| LENGTH | 4 | Payload length in bytes |
| SEQUENCE | 8 | Monotonic write sequence number |
| PAYLOAD | LENGTH | Type-specific payload |
| CHECKSUM | 4 | CRC-32 of everything before it (MAGIC…PAYLOAD) |

### Record types

| TYPE | Name | Payload |
|---|---|---|
| 0 | SET | `u32 path_len || path (UTF-8) || value (SlimDB value codec)` |
| 1 | DELETE | `u32 path_len || path (UTF-8)` |
| 2 | CLEAR | empty |
| 3 | TXN_BEGIN | empty |
| 4 | TXN_COMMIT | empty |
| 5 | TXN_ROLLBACK | empty |

A snapshot contains only SET records (one per top-level key, full value).
The WAL contains SET/DELETE/CLEAR records, grouped in transactions by
TXN_BEGIN … TXN_COMMIT.

## Value codec

| Tag | Type | Payload |
|---|---|---|
| `0x00` | null | — |
| `0x01` | bool | 1 byte (`0`/`1`) |
| `0x02` | int | 8 bytes (i64) |
| `0x03` | float | 8 bytes (f64 bits; NaN canonicalized to `0x7FF8000000000000`) |
| `0x04` | string | `u32 len || UTF-8 bytes` |
| `0x05` | binary | `u32 len || bytes` |
| `0x06` | array | `u32 count || elements` |
| `0x07` | object | `u32 count || (u32 key_len || key || value)*`, keys sorted |

Nesting is limited to 128 levels on encode and decode. Objects use sorted
keys so serialization is deterministic.

## Paths

Paths are dot-separated segments, e.g. `player.inventory.sword`. Empty
segments are invalid. Setting a path creates missing intermediate objects;
traversing an existing non-object is a `TypeMismatch` error.

## Recovery

On open:

1. If `game.sdb` exists: validate the header (magic, version, checksum) —
   a failure is a hard error (the snapshot is only ever written atomically,
   so a bad snapshot is real corruption; restore from a backup). Replay the
   SET records into memory.
2. If `game.sdb-wal` exists with a valid header matching the database id:
   replay records into memory, then truncate the WAL. Otherwise reset the
   WAL.
3. If neither exists: create a new database with a fresh id.

WAL replay rules:

* Records with `seq <= last_seq` (the snapshot header's field) are **skipped**:
  the snapshot already contains them. Re-applying them would be incorrect —
  the snapshot state may have advanced past the point where an older record
  was valid (e.g. `k10.x` was set before `k10` became a scalar; replaying it
  on top of the snapshot would be a type mismatch). `last_seq` pins the exact
  prefix of the WAL history the snapshot represents.
* Remaining records are applied in order. SET/DELETE/CLEAR outside a
  transaction apply immediately; inside `TXN_BEGIN … TXN_COMMIT` they apply
  atomically at the COMMIT.
* A transaction at the end of the log without a COMMIT is uncommitted: its
  records are discarded and the WAL is truncated before the BEGIN.
* The first unparseable record is classified as either a **torn tail** or
  **corruption**:
  * If no valid record exists after it (checksum failure: the claimed end
    doesn't parse; frame failure: a bounded forward scan finds nothing), the
    region is a torn write from a crash — the WAL is truncated at the end of
    the last valid applied record.
  * If valid records exist after it, the file is corrupted — open fails with
    a clear `Corrupt` error.

The `last_seq` skip rule is what makes a crash between the atomic snapshot
rename and the WAL reset safe: the snapshot rename succeeds, then the crash
leaves the full WAL in place. On the next open, every WAL record is `<=`
`last_seq` and is skipped — the snapshot alone is the recovered state, so
recovery is idempotent.

## Compaction

`compact` performs, in order:

1. Flush + fsync the WAL (every buffered write becomes durable).
2. Write a fresh snapshot to `game.sdb.tmp` (header with `last_seq` = the
   last issued WAL sequence, then one SET record per top-level key), fsync.
3. Atomically rename `game.sdb.tmp` → `game.sdb`, then fsync the directory.
4. Reset the WAL to header-only and fsync.

A crash at any step leaves either the old snapshot + full WAL, or the new
snapshot + full WAL — both recover to the same state.

`backup` writes a standalone snapshot (same layout, no WAL needed). `restore`
atomically replaces the snapshot and resets the WAL with the backup's
database id.

## Durability modes

| Mode | Behavior | Guarantee |
|---|---|---|
| `safe` (default) | fsync after every write | committed data survives power loss |
| `balanced` | fsync every 100 ms or 1 MiB buffered | near-safe with batching |
| `fast` | no fsync; OS decides | best-effort; for non-critical data |

## Versioning policy

* Format version lives in both headers. Opening a file with a higher
  `FORMAT_VERSION` fails with `UnsupportedVersion` and a clear message —
  never silent corruption or silent migration.
* Backward compatibility: before every minor release, a `*.sdb` written by
  the previous release must open without error or loss (compatibility test,
  `core/tests/compat.rs`).
* Any change to this format must pass the full crash-test loop before it is
  considered done.

## Limits

* < 100 000 documents, < 1 GB per database (documented domain of validity).
* Single process, single writer. No locking between processes.
* A corrupt snapshot is a hard error (restore from backup); the WAL handles
  torn writes by truncation.
