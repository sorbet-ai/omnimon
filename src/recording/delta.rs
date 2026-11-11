//! Field-level deltas between consecutive snapshots, as JSON values: only
//! what changed is stored. Generic over the snapshot's shape, so fields
//! added later are recorded without touching this code.
//!
//! A delta mirrors the value it applies to:
//! - object -> object: changed or new keys (recursively), plus `"-": [removed keys]`;
//! - array of process rows (objects with a `pid`) -> `{"@": {key: delta},
//!   "+": [new rows], "-": [gone keys], "=": [key order, only if it changed]}`,
//!   keyed by `gpu:pid`;
//! - other arrays of objects with an unchanged length -> `{"~": {index: delta}}`;
//! - anything else -> the new value, or `{"!": value}` when that would read
//!   as a delta (an object replacing an array).
//!
//! Applying needs the same previous value the delta was made from, so a
//! decoder can tell "delta" from "replacement" by type alone: an object patch
//! on an old object or array is a delta, anything else replaces.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

const GONE: &str = "-";
const CHANGED: &str = "@";
const ADDED: &str = "+";
const ORDER: &str = "=";
const INDEXED: &str = "~";
const REPLACE: &str = "!";

/// `gpu:pid` of a process row; `-` stands for "no GPU" (host processes).
fn row_key(v: &Value) -> Option<String> {
    let o = v.as_object()?;
    let pid = o.get("pid")?.as_u64()?;
    match o.get("gpu") {
        None => Some(format!("-:{pid}")),
        Some(g) => Some(format!("{}:{pid}", g.as_u64()?)),
    }
}

/// Keys of every row, if all rows have one and they're unique.
fn row_keys(a: &[Value]) -> Option<Vec<String>> {
    let keys: Vec<String> = a.iter().map(row_key).collect::<Option<_>>()?;
    let unique: HashSet<&String> = keys.iter().collect();
    (unique.len() == keys.len()).then_some(keys)
}

/// The delta turning `a` into `b`, or None when they're equal.
pub fn diff(a: &Value, b: &Value) -> Option<Value> {
    if a == b {
        return None;
    }
    Some(match (a, b) {
        (Value::Object(a), Value::Object(b)) => Value::Object(diff_object(a, b)),
        (Value::Array(a), Value::Array(b)) => diff_array(a, b).unwrap_or_else(|| Value::Array(b.clone())),
        (Value::Array(_), Value::Object(_)) => Value::Object(Map::from_iter([(REPLACE.to_string(), b.clone())])),
        _ => b.clone(),
    })
}

fn diff_object(a: &Map<String, Value>, b: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, vb) in b {
        match a.get(k) {
            None => {
                out.insert(k.clone(), vb.clone());
            }
            Some(va) => {
                if let Some(d) = diff(va, vb) {
                    out.insert(k.clone(), d);
                }
            }
        }
    }
    let gone: Vec<Value> = a.keys().filter(|k| !b.contains_key(*k)).map(|k| Value::String(k.clone())).collect();
    if !gone.is_empty() {
        out.insert(GONE.into(), Value::Array(gone));
    }
    out
}

/// None: just store `b` whole.
fn diff_array(a: &[Value], b: &[Value]) -> Option<Value> {
    if let (Some(ka), Some(kb)) = (row_keys(a), row_keys(b))
        && !(a.is_empty() && b.is_empty())
    {
        let old: HashMap<&str, &Value> = ka.iter().map(String::as_str).zip(a).collect();
        let now: HashSet<&str> = kb.iter().map(String::as_str).collect();
        let (mut changed, mut added) = (Map::new(), Vec::new());
        for (k, vb) in kb.iter().zip(b) {
            match old.get(k.as_str()) {
                None => added.push(vb.clone()),
                Some(va) => {
                    if let Some(d) = diff(va, vb) {
                        changed.insert(k.clone(), d);
                    }
                }
            }
        }
        let gone: Vec<&String> = ka.iter().filter(|k| !now.contains(k.as_str())).collect();
        // The order a decoder rebuilds: survivors in old order, then the new ones.
        let rebuilt = ka.iter().filter(|k| now.contains(k.as_str())).chain(kb.iter().filter(|k| !old.contains_key(k.as_str())));
        let mut out = Map::new();
        if !changed.is_empty() {
            out.insert(CHANGED.into(), Value::Object(changed));
        }
        if !added.is_empty() {
            out.insert(ADDED.into(), Value::Array(added));
        }
        if !gone.is_empty() {
            out.insert(GONE.into(), gone.into_iter().cloned().collect());
        }
        if !rebuilt.eq(kb.iter()) {
            out.insert(ORDER.into(), kb.iter().cloned().collect());
        }
        return Some(Value::Object(out));
    }
    if a.len() == b.len() && !a.is_empty() && a.iter().chain(b).all(Value::is_object) {
        let changed: Map<String, Value> =
            a.iter().zip(b).enumerate().filter_map(|(i, (va, vb))| Some((i.to_string(), diff(va, vb)?))).collect();
        return Some(Value::Object(Map::from_iter([(INDEXED.to_string(), Value::Object(changed))])));
    }
    None
}

/// Apply a delta made by [`diff`] against the value `v` it was made from.
pub fn apply(v: &mut Value, d: &Value) {
    match (v, d) {
        (Value::Object(o), Value::Object(p)) => {
            for (k, dv) in p {
                if k == GONE {
                    continue;
                }
                match o.get_mut(k) {
                    Some(old) => apply(old, dv),
                    None => {
                        o.insert(k.clone(), dv.clone());
                    }
                }
            }
            for k in p.get(GONE).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                o.remove(k);
            }
        }
        (v @ Value::Array(_), Value::Object(p)) if p.contains_key(REPLACE) => *v = p[REPLACE].clone(),
        (Value::Array(a), Value::Object(p)) => apply_array(a, p),
        (v, d) => *v = d.clone(),
    }
}

fn apply_array(a: &mut Vec<Value>, p: &Map<String, Value>) {
    if let Some(idx) = p.get(INDEXED).and_then(Value::as_object) {
        for (i, dv) in idx {
            if let Some(old) = i.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                apply(old, dv);
            }
        }
        return;
    }
    let gone: HashSet<&str> = p.get(GONE).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    a.retain(|r| row_key(r).is_none_or(|k| !gone.contains(k.as_str())));
    if let Some(changed) = p.get(CHANGED).and_then(Value::as_object) {
        for r in a.iter_mut() {
            if let Some(dv) = row_key(r).and_then(|k| changed.get(&k)) {
                apply(r, dv);
            }
        }
    }
    a.extend(p.get(ADDED).and_then(Value::as_array).into_iter().flatten().cloned());
    if let Some(order) = p.get(ORDER).and_then(Value::as_array) {
        let pos: HashMap<&str, usize> = order.iter().filter_map(Value::as_str).enumerate().map(|(i, k)| (k, i)).collect();
        a.sort_by_key(|r| row_key(r).and_then(|k| pos.get(k.as_str()).copied()).unwrap_or(usize::MAX));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(a: Value, b: Value) -> Option<Value> {
        let d = diff(&a, &b);
        let mut x = a.clone();
        if let Some(d) = &d {
            apply(&mut x, d);
        }
        assert_eq!(x, b, "delta {d:?}");
        d
    }

    #[test]
    fn objects_store_only_changes() {
        let d = round_trip(json!({"a": 1, "b": {"c": 2, "d": 3}, "gone": 1}), json!({"a": 1, "b": {"c": 2, "d": 4}, "new": null}));
        assert_eq!(d, Some(json!({"b": {"d": 4}, "new": null, "-": ["gone"]})));
        assert_eq!(round_trip(json!({"a": 1}), json!({"a": 1})), None);
        // Type changes replace.
        round_trip(json!({"a": null}), json!({"a": {"x": 1}}));
        round_trip(json!({"a": {"x": 1}}), json!({"a": null}));
        round_trip(json!({"a": [1, 2]}), json!({"a": {"x": 1}}));
        round_trip(json!({"a": {"x": 1}}), json!({"a": [1]}));
    }

    #[test]
    fn process_rows_are_keyed_by_gpu_and_pid() {
        let a = json!([{"gpu": 0, "pid": 1, "cpu": 1.0, "cmd": "long"}, {"gpu": 1, "pid": 1, "cpu": 0.0}, {"pid": 9, "cpu": 3.0}]);
        let b = json!([{"gpu": 1, "pid": 1, "cpu": 0.0}, {"gpu": 0, "pid": 1, "cpu": 2.0, "cmd": "long"}, {"gpu": 0, "pid": 5}]);
        let d = round_trip(a.clone(), b).unwrap();
        assert_eq!(d["@"], json!({"0:1": {"cpu": 2.0}}), "unchanged fields aren't repeated");
        assert_eq!(d["-"], json!(["-:9"]));
        assert_eq!(d["+"], json!([{"gpu": 0, "pid": 5}]));
        assert!(d.get("=").is_some(), "reordered rows record their order");
        // Same order: no order list.
        let d = round_trip(a, json!([{"gpu": 0, "pid": 1, "cpu": 1.0, "cmd": "long"}, {"gpu": 1, "pid": 1, "cpu": 0.0}])).unwrap();
        assert!(d.get("=").is_none(), "{d}");
        round_trip(json!([]), json!([{"pid": 1}]));
        round_trip(json!([{"pid": 1}]), json!([]));
    }

    #[test]
    fn other_arrays() {
        // Same length, objects: per index.
        let d = round_trip(json!([{"u": 1}, {"u": 2}]), json!([{"u": 1}, {"u": 3}])).unwrap();
        assert_eq!(d, json!({"~": {"1": {"u": 3}}}));
        // Different length, scalars, duplicate keys: replaced.
        round_trip(json!([{"u": 1}]), json!([{"u": 1}, {"u": 2}]));
        assert_eq!(round_trip(json!([1, 2]), json!([1, 3])), Some(json!([1, 3])));
        round_trip(json!([{"pid": 1}, {"pid": 1, "x": 1}]), json!([{"pid": 1}, {"pid": 1, "x": 2}]));
        round_trip(json!([{"pid": 1, "x": 1}]), json!([{"pid": 1}, {"pid": 1}]));
    }
}
