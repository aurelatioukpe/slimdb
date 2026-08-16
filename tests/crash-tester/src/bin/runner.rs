//! The kill -9 crash test loop.
//!
//! For each iteration: spawn a writer applying deterministic ops to a fresh
//! database, SIGKILL it at a random moment, reopen the database, and verify
//! that the recovered state equals the shadow state after *some* prefix of
//! the operation stream (never more, never corrupted, never partial for a
//! transaction). When the writer finishes cleanly, the state must match
//! exactly.
//!
//! Usage: slimdb-runner <workdir> <iterations> [max-ops]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slimdb_core::{Db, Value};
use slimdb_crash_tester::OpStream;

fn dump_state(db: &Db) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    for key in db.keys() {
        map.insert(key.clone(), db.get(&key).unwrap().unwrap());
    }
    map
}

fn random_seed(it: usize) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (nanos as u64) ^ (it as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xC0FF_EE00
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Locate the writer binary next to this executable (same target dir).
fn writer_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("exe parent");
    let name = if cfg!(windows) {
        "slimdb-writer.exe"
    } else {
        "slimdb-writer"
    };
    let candidate = dir.join(name);
    if candidate.exists() {
        candidate
    } else {
        // Fall back to a sibling directory (e.g. when run via cargo).
        dir.join("../../target/release/slimdb-writer")
    }
}

fn spawn_writer(db_path: &std::path::Path, seed: u64, max_ops: usize) -> Child {
    Command::new(writer_bin())
        .arg(db_path)
        .arg(seed.to_string())
        .arg(max_ops.to_string())
        .spawn()
        .expect("spawn writer")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: slimdb-runner <workdir> <iterations> [max-ops]");
        std::process::exit(2);
    }
    let workdir = PathBuf::from(&args[1]);
    let iterations: usize = args[2].parse().expect("iterations");
    let max_ops: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3000);
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let mut failures = 0usize;
    let mut killed_count = 0usize;
    let mut clean_count = 0usize;

    for it in 0..iterations {
        let dir = workdir.join(format!("it{it}"));
        std::fs::create_dir_all(&dir).expect("create iteration dir");
        let db_path = dir.join("game.sdb");
        let seed = random_seed(it);
        let no_kill = it % 25 == 0;

        let mut child = spawn_writer(&db_path, seed, max_ops);
        let mut clean_finish = false;
        if no_kill {
            // Let it finish cleanly.
            let status = child.wait().expect("wait writer");
            if !status.success() {
                failures += 1;
                eprintln!("iter {it}: writer exited abnormally: {status}");
                continue;
            }
            clean_finish = true;
            clean_count += 1;
        } else {
            // Kill at a random point mid-stream (5..150 ms).
            sleep_ms(5 + (seed % 146));
            let _ = child.kill();
            let status = child.wait().expect("wait writer");
            if status.success() {
                // The writer beat the kill: treat as a clean finish.
                clean_finish = true;
                clean_count += 1;
            } else {
                killed_count += 1;
            }
        }

        // Rebuild the expected history from the seed.
        let mut stream = OpStream::new(seed);
        let mut history: Vec<BTreeMap<String, Value>> = vec![BTreeMap::new()];
        for _ in 0..max_ops {
            stream.generate();
            history.push(stream.state().map.clone());
        }

        // Reopen and compare.
        let recovered = match Db::open(&db_path) {
            Ok(db) => dump_state(&db),
            Err(e) => {
                failures += 1;
                eprintln!("iter {it}: reopen failed: {e}");
                continue;
            }
        };

        let ok = if clean_finish {
            recovered == history[max_ops]
        } else {
            history.contains(&recovered)
        };
        if !ok {
            failures += 1;
            eprintln!(
                "iter {it}: recovered state does not match the op history\n\
                 recovered: {recovered:#?}\n\
                 expected prefix of {} ops (clean: {clean_finish})",
                max_ops
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    println!(
        "crash-test: {iterations} iterations — {clean_count} clean finishes, {killed_count} SIGKILLs, {failures} failures"
    );
    if failures > 0 {
        std::process::exit(1);
    }
}
