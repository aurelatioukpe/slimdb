//! Dot-path parsing and navigation.
//!
//! Paths address values inside documents, e.g. `"player.level"` or
//! `"inventory.items.0"`. Segments are separated by `.`; empty segments are
//! rejected.

use std::collections::BTreeMap;

use crate::document::Value;
use crate::error::{Result, SlimDbError};

/// Split a dot path into its segments.
///
/// ```
/// use slimdb_core::path::split_path;
/// assert_eq!(split_path("player.level").unwrap(), vec!["player", "level"]);
/// assert!(split_path("a..b").is_err());
/// assert!(split_path("").is_err());
/// ```
pub fn split_path(path: &str) -> Result<Vec<String>> {
    if path.is_empty() {
        return Err(SlimDbError::BadPath("path is empty".to_string()));
    }
    let mut out = Vec::new();
    for seg in path.split('.') {
        if seg.is_empty() {
            return Err(SlimDbError::BadPath(format!("empty segment in {path:?}")));
        }
        out.push(seg.to_string());
    }
    Ok(out)
}

/// Check that `segs` can be applied to `map` without a type mismatch: every
/// traversed segment that already exists must be an object.
pub fn validate_path_shape(map: &BTreeMap<String, Value>, segs: &[String]) -> Result<()> {
    let mut current = map;
    for (i, seg) in segs.iter().enumerate() {
        if i + 1 == segs.len() {
            break;
        }
        match current.get(seg) {
            None => break,
            Some(Value::Object(o)) => current = o,
            Some(_) => {
                return Err(SlimDbError::TypeMismatch {
                    path: segs[..=i].join("."),
                    detail: "a non-object value lies on the path".to_string(),
                })
            }
        }
    }
    Ok(())
}

/// Get the value at `segs`, or `None` if the path does not exist.
pub fn get_at_path(map: &BTreeMap<String, Value>, segs: &[String]) -> Option<Value> {
    if segs.is_empty() {
        return None;
    }
    if segs.len() == 1 {
        return map.get(&segs[0]).cloned();
    }
    match map.get(&segs[0]) {
        Some(Value::Object(o)) => get_at_path(o, &segs[1..]),
        _ => None,
    }
}

/// True if the path exists.
pub fn exists_at_path(map: &BTreeMap<String, Value>, segs: &[String]) -> bool {
    if segs.is_empty() {
        return false;
    }
    if segs.len() == 1 {
        return map.contains_key(&segs[0]);
    }
    match map.get(&segs[0]) {
        Some(Value::Object(o)) => exists_at_path(o, &segs[1..]),
        _ => false,
    }
}

/// Set the value at `segs`, creating intermediate objects as needed.
///
/// Errors with [`SlimDbError::TypeMismatch`] if a traversed segment exists
/// but is not an object.
pub fn set_at_path(map: &mut BTreeMap<String, Value>, segs: &[String], value: Value) -> Result<()> {
    if segs.is_empty() {
        return Err(SlimDbError::BadPath("empty path".to_string()));
    }
    if segs.len() == 1 {
        map.insert(segs[0].clone(), value);
        return Ok(());
    }
    match map.get_mut(&segs[0]) {
        Some(Value::Object(o)) => set_at_path(o, &segs[1..], value),
        Some(_) => Err(SlimDbError::TypeMismatch {
            path: segs[0].clone(),
            detail: "a non-object value lies on the path".to_string(),
        }),
        None => {
            // Build the nested chain from the leaf up.
            let mut v = value;
            for seg in segs[1..].iter().rev() {
                let mut o = BTreeMap::new();
                o.insert(seg.clone(), v);
                v = Value::Object(o);
            }
            map.insert(segs[0].clone(), v);
            Ok(())
        }
    }
}

/// Delete the value at `segs`. Returns `true` if something was removed.
/// Parent objects are left in place even when they become empty.
pub fn delete_at_path(map: &mut BTreeMap<String, Value>, segs: &[String]) -> bool {
    if segs.is_empty() {
        return false;
    }
    if segs.len() == 1 {
        return map.remove(&segs[0]).is_some();
    }
    match map.get_mut(&segs[0]) {
        Some(Value::Object(o)) => delete_at_path(o, &segs[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn nested_set_get_delete() {
        let mut m = BTreeMap::new();
        let segs = split_path("player.level").unwrap();
        set_at_path(&mut m, &segs, Value::Int(42)).unwrap();
        assert_eq!(get_at_path(&m, &segs), Some(Value::Int(42)));
        assert!(exists_at_path(&m, &segs));

        // Missing intermediate path does not exist.
        assert!(!exists_at_path(&m, &split_path("player.score").unwrap()));
        assert_eq!(get_at_path(&m, &split_path("player.score").unwrap()), None);

        // Overwrite the leaf.
        set_at_path(&mut m, &segs, Value::Str("x".to_string())).unwrap();
        assert_eq!(get_at_path(&m, &segs), Some(Value::Str("x".to_string())));

        // Delete the leaf; parent survives.
        assert!(delete_at_path(&mut m, &segs));
        assert_eq!(m, map(&[("player", Value::Object(BTreeMap::new()))]));
        assert!(!delete_at_path(&mut m, &segs));
    }

    #[test]
    fn type_mismatch() {
        let mut m = map(&[("a", Value::Int(5))]);
        assert!(set_at_path(&mut m, &split_path("a.b").unwrap(), Value::Null).is_err());
        assert!(validate_path_shape(&m, &split_path("a.b").unwrap()).is_err());
        // Creating a fresh path under a missing key works.
        assert!(set_at_path(&mut m, &split_path("x.y.z").unwrap(), Value::Int(1)).is_ok());
    }

    #[test]
    fn split_rejects_empty_segments() {
        assert!(split_path("a..b").is_err());
        assert!(split_path(".a").is_err());
        assert!(split_path("a.").is_err());
        assert!(split_path("").is_err());
        assert_eq!(split_path("single").unwrap(), vec!["single"]);
    }
}
