//! Shared helpers for integration tests: temp directories, a shadow model
//! of the database state, and a deterministic operation generator used by
//! the crash-recovery tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use slimdb_core::Value;

/// A temporary directory that cleans itself up on drop.
pub struct TestDir {
    pub path: PathBuf,
}

impl TestDir {
    pub fn new(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!(
            "slimdb-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        TestDir { path: dir }
    }

    /// Path of a database file inside the temp dir.
    pub fn db(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// xorshift64 — small deterministic PRNG for tests (no external deps).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// An operation against the database, mirroring the engine's semantics.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Set { path: String, value: Value },
    Delete { path: String },
    Clear,
    Txn(Vec<Op>),
}

/// A simple, independent implementation of the engine's path semantics, used
/// as an oracle by the crash tests.
#[derive(Debug, Clone, Default)]
pub struct Shadow {
    pub map: BTreeMap<String, Value>,
}

impl Shadow {
    pub fn new() -> Shadow {
        Shadow {
            map: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, path: &str, value: Value) {
        let segs: Vec<String> = path.split('.').map(|s| s.to_string()).collect();
        Self::set_at(&mut self.map, &segs, value.canonicalize());
    }

    fn set_at(map: &mut BTreeMap<String, Value>, segs: &[String], value: Value) {
        if segs.len() == 1 {
            map.insert(segs[0].clone(), value);
            return;
        }
        match map.get_mut(&segs[0]) {
            Some(Value::Object(o)) => Self::set_at(o, &segs[1..], value),
            _ => {
                let mut v = value;
                for seg in segs[1..].iter().rev() {
                    let mut o = BTreeMap::new();
                    o.insert(seg.clone(), v);
                    v = Value::Object(o);
                }
                map.insert(segs[0].clone(), v);
            }
        }
    }

    pub fn delete(&mut self, path: &str) -> bool {
        let segs: Vec<String> = path.split('.').map(|s| s.to_string()).collect();
        Self::delete_at(&mut self.map, &segs)
    }

    fn delete_at(map: &mut BTreeMap<String, Value>, segs: &[String]) -> bool {
        if segs.len() == 1 {
            return map.remove(&segs[0]).is_some();
        }
        match map.get_mut(&segs[0]) {
            Some(Value::Object(o)) => Self::delete_at(o, &segs[1..]),
            _ => false,
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Whether the path is valid for a set, mirroring the engine's
    /// `validate_path_shape`: every existing traversed segment must be an
    /// object.
    pub fn is_valid_set_path(&self, path: &str) -> bool {
        let segs: Vec<String> = path.split('.').map(|s| s.to_string()).collect();
        if segs.is_empty() {
            return false;
        }
        let mut current = &self.map;
        for (i, seg) in segs.iter().enumerate() {
            if i + 1 == segs.len() {
                break;
            }
            match current.get(seg) {
                None => break,
                Some(Value::Object(o)) => current = o,
                Some(_) => return false,
            }
        }
        true
    }

    /// Paths of all object values in the tree.
    pub fn object_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut prefix = Vec::new();
        Self::collect_objects(&self.map, &mut prefix, &mut out);
        out
    }

    fn collect_objects(
        map: &BTreeMap<String, Value>,
        prefix: &mut [String],
        out: &mut Vec<String>,
    ) {
        for (k, v) in map {
            if let Value::Object(o) = v {
                let mut p = prefix.to_vec();
                p.push(k.clone());
                out.push(p.join("."));
                Self::collect_objects(o, &mut p, out);
            }
        }
    }

    /// Paths that currently exist (top-level or leaves of objects).
    pub fn existing_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut prefix = Vec::new();
        Self::collect_paths(&self.map, &mut prefix, &mut out);
        out
    }

    fn collect_paths(map: &BTreeMap<String, Value>, prefix: &mut [String], out: &mut Vec<String>) {
        for (k, v) in map {
            let mut p = prefix.to_vec();
            p.push(k.clone());
            out.push(p.join("."));
            if let Value::Object(o) = v {
                Self::collect_paths(o, &mut p, out);
            }
        }
    }

    pub fn apply(&mut self, op: &Op) {
        match op {
            Op::Set { path, value } => self.set(path, value.clone()),
            Op::Delete { path } => {
                self.delete(path);
            }
            Op::Clear => self.clear(),
            Op::Txn(ops) => {
                for op in ops {
                    self.apply(op);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Generate a random value (int, float, string, bool, null, array, object).
pub fn random_value(rng: &mut Rng) -> Value {
    match rng.below(12) {
        0 => Value::Null,
        1 => Value::Bool(rng.below(2) == 0),
        2 => Value::Int(rng.next_u64() as i64),
        3 => Value::Float(f64::from_bits(rng.next_u64())),
        4 => Value::Str(format!("s{}", rng.below(1000))),
        5 => Value::Str("".to_string()),
        6 => Value::Binary(vec![rng.next_u64() as u8; 1 + rng.below(8)]),
        7 => Value::Array((0..rng.below(4)).map(|_| random_value(rng)).collect()),
        8 => {
            let mut m = BTreeMap::new();
            for _ in 0..rng.below(3) {
                m.insert(format!("f{}", rng.below(5)), random_value(rng));
            }
            Value::Object(m)
        }
        9 => Value::Int((rng.next_u64() % 1000) as i64),
        10 => Value::Int(-((rng.next_u64() % 1000) as i64)),
        _ => Value::Float((rng.next_u64() as f64) / 1e6),
    }
}

/// Generate the next operation, guaranteed valid on both the shadow and the
/// engine (paths are chosen against the shadow state, which is in sync with
/// the engine at generation time).
pub fn next_op(rng: &mut Rng, shadow: &mut Shadow) -> Op {
    let roll = rng.below(100);
    if roll < 40 {
        // Set at a fresh top-level key or under an existing object.
        let depth = 1 + rng.below(2);
        let path = candidate_path(rng, shadow, depth);
        Op::Set {
            path,
            value: random_value(rng),
        }
    } else if roll < 60 {
        // Deep nested set (exercises chain creation), with validity check.
        let depth = 1 + rng.below(3);
        let path = candidate_path(rng, shadow, depth);
        Op::Set {
            path,
            value: random_value(rng),
        }
    } else if roll < 75 {
        // Delete an existing path (sometimes missing).
        let existing = shadow.existing_paths();
        let path = if !existing.is_empty() && rng.below(2) == 0 {
            existing[rng.below(existing.len())].clone()
        } else {
            format!("k{}", rng.below(40))
        };
        Op::Delete { path }
    } else if roll < 82 {
        Op::Clear
    } else {
        // A small transaction of 1-3 valid ops. Every sub-op is applied to
        // the shadow immediately so that later sub-ops validate against the
        // engine's in-transaction view (the final `shadow.apply` of the whole
        // transaction is idempotent).
        let n = 1 + rng.below(3);
        let mut ops = Vec::new();
        for _ in 0..n {
            let op = match rng.below(3) {
                0 => {
                    let depth = 1 + rng.below(2);
                    Op::Set {
                        path: candidate_path(rng, shadow, depth),
                        value: random_value(rng),
                    }
                }
                1 => {
                    let existing = shadow.existing_paths();
                    let path = if !existing.is_empty() && rng.below(2) == 0 {
                        existing[rng.below(existing.len())].clone()
                    } else {
                        format!("k{}", rng.below(40))
                    };
                    Op::Delete { path }
                }
                _ => {
                    let depth = 1 + rng.below(2);
                    Op::Set {
                        path: candidate_path(rng, shadow, depth),
                        value: random_value(rng),
                    }
                }
            };
            shadow.apply(&op);
            ops.push(op);
        }
        Op::Txn(ops)
    }
}

/// A candidate set path of the given depth that is valid on the shadow.
fn candidate_path(rng: &mut Rng, shadow: &Shadow, depth: usize) -> String {
    for _ in 0..8 {
        let mut segs = Vec::new();
        for _ in 0..depth {
            segs.push(format!("k{}", rng.below(30)));
        }
        let path = segs.join(".");
        if shadow.is_valid_set_path(&path) {
            return path;
        }
    }
    // Fall back to a fresh top-level key, always valid.
    format!("k{}", rng.below(1000))
}

/// Apply `count` deterministic operations derived from `seed` to `shadow`,
/// returning the sequence of generated ops.
pub fn apply_deterministic_ops(shadow: &mut Shadow, seed: u64, count: usize) -> Vec<Op> {
    let mut rng = Rng::new(seed);
    let mut ops = Vec::with_capacity(count);
    for _ in 0..count {
        let op = next_op(&mut rng, shadow);
        shadow.apply(&op);
        ops.push(op);
    }
    ops
}
