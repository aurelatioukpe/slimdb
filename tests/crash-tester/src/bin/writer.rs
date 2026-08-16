//! The writer: opens a database in safe mode and applies a deterministic
//! stream of operations, fsyncing every write. It is killed at a random point
//! by the runner; any operation error (a real bug) makes it exit non-zero.

use slimdb_core::Db;
use slimdb_crash_tester::{Op, OpStream};

fn apply_op(db: &mut Db, op: &Op) {
    match op {
        Op::Set { path, value } => db
            .set(path, value.clone())
            .unwrap_or_else(|e| panic!("set({path:?}) failed: {e}")),
        Op::Delete { path } => {
            db.delete(path)
                .unwrap_or_else(|e| panic!("delete({path:?}) failed: {e}"));
        }
        Op::Clear => db.clear().unwrap_or_else(|e| panic!("clear failed: {e}")),
        Op::Txn(ops) => {
            let mut txn = db.transaction();
            for op in ops {
                apply_txn_op(&mut txn, op);
            }
            txn.commit()
                .unwrap_or_else(|e| panic!("commit failed: {e}"));
        }
    }
}

fn apply_txn_op(txn: &mut slimdb_core::Transaction<'_>, op: &Op) {
    match op {
        Op::Set { path, value } => txn
            .set(path, value.clone())
            .unwrap_or_else(|e| panic!("txn set({path:?}) failed: {e}")),
        Op::Delete { path } => {
            txn.delete(path)
                .unwrap_or_else(|e| panic!("txn delete({path:?}) failed: {e}"));
        }
        Op::Clear => txn
            .clear()
            .unwrap_or_else(|e| panic!("txn clear failed: {e}")),
        Op::Txn(_) => panic!("nested transaction in op stream"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: slimdb-writer <db-path> <seed> <max-ops>");
        std::process::exit(2);
    }
    let db_path = &args[1];
    let seed: u64 = args[2].parse().expect("seed");
    let max_ops: usize = args[3].parse().expect("max-ops");

    let mut stream = OpStream::new(seed);
    let mut db = Db::open(db_path).unwrap_or_else(|e| panic!("open failed: {e}"));

    for i in 0..max_ops {
        let op = stream.generate();
        apply_op(&mut db, &op);
        if i > 0 && i % 100 == 0 {
            // Exercise compaction under fire.
            db.compact()
                .unwrap_or_else(|e| panic!("compact failed: {e}"));
        }
    }

    db.close().unwrap_or_else(|e| panic!("close failed: {e}"));
    println!("writer: finished {max_ops} ops cleanly");
}
