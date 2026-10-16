//! Byte-identical `.orb` output (owner ruling, Stage C 2026-09-06). `serde_json`
//! runs with the `preserve_order` feature workspace-wide
//! (`orbital-compiler/Cargo.toml`), so `Map` is backed by an `IndexMap` and
//! `to_string_pretty` emits keys in HashMap/insertion order — meaning two
//! resolves of the SAME schema, or the same resolve run twice, can differ
//! byte-for-byte even though nothing about the schema changed. This is the
//! ONE writer every schema-serialization site routes through before
//! stringifying: recursively sorts every object's keys; arrays keep their
//! authored order (order is semantic there — `transitions`, `traits`, …).
//!
//! Four call-site-local copies of this exact recursive-sort predated it
//! (`orbital-compiler`'s `inline/orbital.rs` test helper, its
//! `orbital_import_resolve_determinism.rs` integration test, and
//! `orbital-lolo`'s `lower.rs` / `formatter.rs` golden-comparison
//! `canonicalize` helpers) — `orbital-core` is the most-upstream crate all
//! four already depend on, so this is the one production owner.

use serde_json::Value;

/// Recursively sort every JSON object's keys; arrays are returned unchanged
/// (element order is semantic — codegen and diffing both depend on it).
pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonical_json(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted = serde_json::Map::new();
            for (k, v) in entries {
                sorted.insert(k, v);
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// `value` as pretty JSON with every object's keys sorted: the same bytes
/// every run, whatever map type the value holds.
pub fn to_canonical_string_pretty<T: serde::Serialize>(value: &T) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&canonical_json(&serde_json::to_value(value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_hash_map_serializes_the_same_bytes_every_run() {
        let map: std::collections::HashMap<String, u32> = (0..64).map(|i| (format!("k{i:02}"), i)).collect();
        let text = to_canonical_string_pretty(&map).unwrap();
        let keys: Vec<&str> = text.lines().filter_map(|l| l.trim().strip_prefix('"')?.split('"').next()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys.len(), 64);
        assert_eq!(keys, sorted);
    }

    #[test]
    fn control_array_order_survives_pretty_serialization() {
        assert_eq!(to_canonical_string_pretty(&vec![3, 1, 2]).unwrap(), "[\n  3,\n  1,\n  2\n]");
    }

    #[test]
    fn sorts_object_keys_recursively() {
        let input = json!({ "b": 1, "a": { "z": 1, "y": 2 }, "c": [3, 2, 1] });
        let out = canonical_json(&input);
        let s = serde_json::to_string(&out).unwrap();
        assert_eq!(s, r#"{"a":{"y":2,"z":1},"b":1,"c":[3,2,1]}"#);
    }

    #[test]
    fn array_element_order_is_preserved() {
        let input = json!({ "list": [{"b": 1, "a": 2}, {"d": 3, "c": 4}] });
        let out = canonical_json(&input);
        assert_eq!(out, json!({ "list": [{"a": 2, "b": 1}, {"c": 4, "d": 3}] }));
    }

    #[test]
    fn scalars_and_null_pass_through() {
        assert_eq!(canonical_json(&json!(null)), json!(null));
        assert_eq!(canonical_json(&json!(42)), json!(42));
        assert_eq!(canonical_json(&json!("x")), json!("x"));
        assert_eq!(canonical_json(&json!(true)), json!(true));
    }

    #[test]
    fn idempotent_on_already_sorted_input() {
        let input = json!({ "a": 1, "b": 2 });
        assert_eq!(
            canonical_json(&canonical_json(&input)),
            canonical_json(&input)
        );
    }
}
