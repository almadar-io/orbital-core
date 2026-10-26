use super::*;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::default();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

/// Non-empty incoming payload always wins (and is what gets persisted by the
/// caller) — even when a prior payload exists.
/// Empty incoming payload falls back to the last meaningful payload — the
/// render-ui re-render case (no-op event / tick) that used to revert
/// `@payload.data` to a literal token.
/// With nothing persisted, an empty incoming payload stays empty (no
/// fabrication — there is genuinely nothing to bind).
/// Payloads accumulate across events: a fetch/emit carries `{data}`, a later
/// gesture carries `{row}`; a view referencing `@payload.data` AND `@payload.row`
/// on the same frame must see BOTH. Replacement semantics would drop `data` when
/// the gesture fires.
/// Read-modify-write: Set fields override, untouched base fields survive. This
/// is the field-retention fix — persisting only the Set-touched partial would
/// drop `filterKind`/`current*`/etc. and break the next re-render.
/// Drift D-2: an empty `@payload` (no event payload, nothing remembered) reads `{}` on every
/// path, as in the JS runtime, never `null`.
#[test]
fn merge_entity_preserves_untouched_and_overrides_set() {
    let base = obj(&[
        ("id", Value::String("singleton".to_string())),
        ("filterKind", Value::String("all".to_string())),
        ("title", Value::String("Activity".to_string())),
    ]);
    let set = obj(&[("filterKind", Value::String("created".to_string()))]);
    let merged = merge_entity(&base, &set);
    let m = merged.as_object().unwrap();
    assert_eq!(m["filterKind"], Value::String("created".to_string())); // overridden
    assert_eq!(m["title"], Value::String("Activity".to_string())); // preserved
    assert_eq!(m["id"], Value::String("singleton".to_string())); // preserved
}

#[test]
fn merge_entity_empty_set_returns_base() {
    let base = obj(&[("a", Value::Number(1.0))]);
    assert_eq!(merge_entity(&base, &Value::Object(ValueMap::default())), base);
    assert_eq!(merge_entity(&base, &Value::Null), base);
}

#[test]
fn merge_entity_first_write_uses_set_when_no_base() {
    let set = obj(&[("x", Value::Bool(true))]);
    assert_eq!(merge_entity(&Value::Null, &set), set);
}
