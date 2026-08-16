/*
 * slimdb.h — SlimDB C ABI (the fundamental contract; all bindings sit on it).
 *
 * The tiny embedded database for local apps. No server. No configuration.
 * No SQL.
 *
 * ## Conventions
 *
 * - Opaque handles: `slimdb_db_t*` and `slimdb_txn_t*` are owned by the
 *   caller. Close databases with `slimdb_close`, finish transactions with
 *   `slimdb_commit` / `slimdb_rollback`. Using a handle after close is
 *   undefined behavior (like a closed `FILE*`).
 * - A single handle must not be used from two threads at once (the engine is
 *   documented as single-process, single-writer).
 * - Values cross the ABI as JSON strings. JSON is the API model — it is never
 *   used as a storage format.
 * - Strings returned by this library (`slimdb_get_json`, `slimdb_keys`) must
 *   be freed with `slimdb_string_free` / `slimdb_string_list_free`.
 * - All functions return an error code: `SLIMDB_OK` on success, one of the
 *   `SLIMDB_ERR_*` codes otherwise. Get a human-readable message with
 *   `slimdb_error_string`.
 *
 * ## Example
 *
 * ```c
 * #include "slimdb.h"
 *
 * slimdb_db_t *db = NULL;
 * if (slimdb_open("game.sdb", &db) != SLIMDB_OK) return 1;
 * slimdb_set_json(db, "player.level", "42");
 * char *out = NULL;
 * if (slimdb_get_json(db, "player.level", &out) == SLIMDB_OK && out) {
 *     printf("%s\n", out);   // prints 42
 *     slimdb_string_free(out);
 * }
 * slimdb_close(db);
 * ```
 */

#ifndef SLIMDB_H
#define SLIMDB_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------ */
/* Types                                                               */
/* ------------------------------------------------------------------ */

typedef struct slimdb_db slimdb_db_t;    /* opaque database handle     */
typedef struct slimdb_txn slimdb_txn_t;  /* opaque transaction handle  */

/* ------------------------------------------------------------------ */
/* Error codes                                                         */
/* ------------------------------------------------------------------ */

enum {
    SLIMDB_OK = 0,
    SLIMDB_ERR_IO = 1,                  /* underlying I/O failure        */
    SLIMDB_ERR_CORRUPT = 2,             /* database file is corrupted    */
    SLIMDB_ERR_DECODE = 3,              /* value/JSON could not be decoded */
    SLIMDB_ERR_UNSUPPORTED_VERSION = 4, /* newer format version          */
    SLIMDB_ERR_BAD_PATH = 5,            /* malformed path                */
    SLIMDB_ERR_TYPE_MISMATCH = 6,       /* path traverses a non-object   */
    SLIMDB_ERR_INVALID_ARG = 7,         /* NULL pointer / bad mode       */
    SLIMDB_ERR_TXN = 8,                 /* transaction misuse            */
    SLIMDB_ERR_PANIC = 9                /* internal error (never happens) */
};

/* Durability modes for slimdb_open_with_mode */
enum {
    SLIMDB_DURABILITY_SAFE = 0,     /* fsync per write (default) */
    SLIMDB_DURABILITY_BALANCED = 1, /* periodic flush            */
    SLIMDB_DURABILITY_FAST = 2      /* buffered only             */
};

/* Database statistics */
typedef struct {
    size_t keys;             /* number of top-level keys          */
    unsigned long long data_file_size; /* snapshot size in bytes  */
    unsigned long long wal_file_size;  /* WAL size in bytes       */
    unsigned long long sequence;       /* next write sequence     */
    int durability;                    /* SLIMDB_DURABILITY_*     */
} slimdb_stats_t;

/* ------------------------------------------------------------------ */
/* Lifecycle                                                           */
/* ------------------------------------------------------------------ */

/* Open a database (safe durability). `out` receives the handle. */
int slimdb_open(const char *path, slimdb_db_t **out);

/* Open with an explicit durability mode. */
int slimdb_open_with_mode(const char *path, int mode, slimdb_db_t **out);

/* Close (compacts first) and free the handle. */
int slimdb_close(slimdb_db_t *db);

/* ------------------------------------------------------------------ */
/* Data access (JSON interchange)                                      */
/* ------------------------------------------------------------------ */

/* Set the value at `path` from a JSON string. */
int slimdb_set_json(slimdb_db_t *db, const char *path, const char *json);

/* Get the value at `path` as a JSON string. On success, `out` receives a
 * string to free with slimdb_string_free, or NULL if the path is missing. */
int slimdb_get_json(slimdb_db_t *db, const char *path, char **out);

/* Delete the value at `path`; `removed` (may be NULL) receives 1 or 0. */
int slimdb_delete(slimdb_db_t *db, const char *path, int *removed);

/* Whether a value exists at `path`; `out` (may be NULL) receives 1 or 0. */
int slimdb_exists(slimdb_db_t *db, const char *path, int *out);

/* List top-level keys. `out` receives an array of `count` strings; free with
 * slimdb_string_list_free. */
int slimdb_keys(slimdb_db_t *db, char ***out, size_t *count);

/* Remove every key. */
int slimdb_clear(slimdb_db_t *db);

/* ------------------------------------------------------------------ */
/* Durability & maintenance                                           */
/* ------------------------------------------------------------------ */

/* Flush buffered WAL bytes and fsync. */
int slimdb_flush(slimdb_db_t *db);

/* Compact the WAL into the snapshot file (atomic). */
int slimdb_compact(slimdb_db_t *db);

/* Write a standalone snapshot to `dest`. */
int slimdb_backup(slimdb_db_t *db, const char *dest);

/* Replace the database contents with the backup at `src`. */
int slimdb_restore(slimdb_db_t *db, const char *src);

/* Fill `out` with database statistics. */
int slimdb_stats(slimdb_db_t *db, slimdb_stats_t *out);

/* ------------------------------------------------------------------ */
/* Transactions                                                        */
/* ------------------------------------------------------------------ */

/* Begin a transaction; returns NULL on failure. While a transaction is open,
 * the database handle must not be used by any other call, and must outlive
 * the transaction. */
slimdb_txn_t *slimdb_begin(slimdb_db_t *db);

int slimdb_txn_set_json(slimdb_txn_t *txn, const char *path, const char *json);
int slimdb_txn_delete(slimdb_txn_t *txn, const char *path, int *removed); /* removed may be NULL */
int slimdb_txn_clear(slimdb_txn_t *txn);

/* Commit (or roll back) and free the handle. */
int slimdb_commit(slimdb_txn_t *txn);
int slimdb_rollback(slimdb_txn_t *txn);

/* ------------------------------------------------------------------ */
/* Memory & version                                                    */
/* ------------------------------------------------------------------ */

/* Free a string returned by slimdb_get_json. */
void slimdb_string_free(char *s);

/* Free a string list returned by slimdb_keys. */
void slimdb_string_list_free(char **list, size_t count);

/* Human-readable message for an error code (static; never freed). */
const char *slimdb_error_string(int code);

/* Library version and ABI contract version (static; never freed). */
const char *slimdb_version(void);
const char *slimdb_abi_version(void);

#ifdef __cplusplus
}
#endif

#endif /* SLIMDB_H */
