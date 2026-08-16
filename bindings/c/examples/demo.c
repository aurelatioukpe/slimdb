/* demo.c — end-to-end C example using the SlimDB C ABI. */
#include <stdio.h>
#include <string.h>

#include "slimdb.h"

static int check(int code, const char *what) {
    if (code != SLIMDB_OK) {
        fprintf(stderr, "FAILED: %s: %s\n", what, slimdb_error_string(code));
        return 1;
    }
    return 0;
}

int main(void) {
    slimdb_db_t *db = NULL;

    if (check(slimdb_open("demo.sdb", &db), "open")) return 1;

    /* Open → Set → Done */
    if (check(slimdb_set_json(db, "player.level", "42"), "set int")) return 1;
    if (check(slimdb_set_json(db, "player.name", "\"Aurel\""), "set str")) return 1;
    if (check(slimdb_set_json(db, "player.inventory",
                              "{\"sword\": 5, \"shield\": true}"),
              "set object")) return 1;

    char *out = NULL;
    if (check(slimdb_get_json(db, "player.level", &out), "get")) return 1;
    if (!out || strcmp(out, "42") != 0) {
        fprintf(stderr, "FAILED: expected 42, got %s\n", out ? out : "(null)");
        return 1;
    }
    slimdb_string_free(out);
    out = NULL;

    int exists = 0;
    if (check(slimdb_exists(db, "player.name", &exists), "exists")) return 1;
    if (!exists) { fprintf(stderr, "FAILED: player.name should exist\n"); return 1; }

    /* Transaction: commit persists, rollback doesn't. */
    slimdb_txn_t *txn = slimdb_begin(db);
    if (!txn) { fprintf(stderr, "FAILED: begin\n"); return 1; }
    if (check(slimdb_txn_set_json(txn, "txn.key", "[1, 2, 3]"), "txn set")) return 1;
    if (check(slimdb_commit(txn), "commit")) return 1;

    txn = slimdb_begin(db);
    if (check(slimdb_txn_set_json(txn, "txn.rolled", "true"), "txn set")) return 1;
    if (check(slimdb_rollback(txn), "rollback")) return 1;

    if (check(slimdb_exists(db, "txn.rolled", &exists), "exists rolled")) return 1;
    if (exists) { fprintf(stderr, "FAILED: rolled back value survived\n"); return 1; }

    /* Keys */
    char **keys = NULL;
    size_t nkeys = 0;
    if (check(slimdb_keys(db, &keys, &nkeys), "keys")) return 1;
    printf("keys (%zu):", nkeys);
    for (size_t i = 0; i < nkeys; i++) printf(" %s", keys[i]);
    printf("\n");
    slimdb_string_list_free(keys, nkeys);

    /* Maintenance */
    if (check(slimdb_compact(db), "compact")) return 1;
    if (check(slimdb_backup(db, "demo-backup.sdb"), "backup")) return 1;

    slimdb_stats_t stats;
    if (check(slimdb_stats(db, &stats), "stats")) return 1;
    printf("stats: keys=%zu durability=%d\n", stats.keys, stats.durability);

    if (check(slimdb_close(db), "close")) return 1;

    /* Reopen: everything survived. */
    if (check(slimdb_open("demo.sdb", &db), "reopen")) return 1;
    if (check(slimdb_get_json(db, "player.level", &out), "get after reopen")) return 1;
    printf("after reopen: player.level = %s (slimdb %s, abi %s)\n",
           out ? out : "(null)", slimdb_version(), slimdb_abi_version());
    if (out) slimdb_string_free(out);
    if (check(slimdb_close(db), "close")) return 1;

    printf("demo: OK\n");
    return 0;
}
