//! Trait config defaults and the forwards that resolve one knob from another.

use super::*;

/// Build the `@config` root for the live render context from the trait's declared
/// config field defaults. Mirrors the JS runtime, where `@config.X` resolves against
/// the trait's config defaults (and instance overrides). Std behaviors rendered
/// standalone rely on defaults only — instance/call-site overrides are a deeper
/// composition concern (follow-up). A field with no `default` is left out: its `@config.<knob>` is unset.
pub(super) fn config_defaults_value<T: std::borrow::Borrow<TraitDefinition>>(
    trait_def: &TraitDefinition,
    all_traits: &HashMap<String, T>,
) -> Value {
    let mut map: ValueMap = ValueMap::default();
    if let Some(cfg) = &trait_def.config {
        for (k, field) in cfg {
            if let Some(default) = field.default.as_ref() {
                map.insert(k.clone().into(), sexpr_to_value(default));
            }
        }
    }
    resolve_config_forwards(&mut map, all_traits);
    Value::object(map)
}

/// A config value that is exactly a `@user.<path>` binding reads the viewer one hop; a field the viewer
/// lacks, or one that is not a string, number or boolean, becomes `""`, never the sigil (twin of
/// `buildConfigBinding`, `almadar-runtime/src/traits/config-defaults.ts`).
pub(super) fn bind_user_forwards(config: Value, user: &Value) -> Value {
    let Value::Object(mut map) = config else {
        return config;
    };
    for value in map.values_mut() {
        let Value::String(s) = value else { continue };
        let Some(path) = s.strip_prefix("@user.") else {
            continue;
        };
        if path.is_empty()
            || !path.split('.').all(|seg| {
                !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            continue;
        }
        *value = match user.get_path(path) {
            Some(read @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => read.clone(),
            _ => Value::string(String::new()),
        };
    }
    Value::Object(map)
}

/// Chain wrapper-forward config defaults to the enclosing/host trait's knob.
///
/// A peer trait inlined from a wrapper (e.g. `std-export`'s `Button1`, inlined
/// from `ui-button`) can carry a config default that is itself a `@config.<key>`
/// forward — `Button1.label = @config.buttonLabel` — where `<key>` is a knob on
/// the HOST trait that composes it (`ExportMenu.buttonLabel = "Export"`), not a
/// knob of the peer. The inline phase collapses the peer's own `@config.label`
/// render binding to the forward token `@config.buttonLabel`, so the peer's
/// render-ui reads `buttonLabel` — a key ABSENT from its own config root, which
/// would otherwise paint the literal token.
///
/// For each of the trait's config fields whose default is a `@config.<key>`
/// forward, expose `<key>` in the live config root, resolved against the host
/// trait that declares it (its `default` already carries any folded call-site
/// override, so a host-level override flows to the child). The local field is
/// collapsed too, so both `@config.<key>` and `@config.<field>` resolve. The
/// forward stays in the trait's declared config + render-ui; only the LIVE render
/// context is chained, so the knob remains overridable.
pub(super) fn resolve_config_forwards<T: std::borrow::Borrow<TraitDefinition>>(
    map: &mut ValueMap,
    all_traits: &HashMap<String, T>,
) {
    let forwards: Vec<(String, String)> = map
        .iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => forward_key(s).map(|fk| (k.to_string(), fk)),
            _ => None,
        })
        .collect();
    for (field, fwd_key) in forwards {
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(field.clone());
        if let Some(resolved) = resolve_forward_key(&fwd_key, all_traits, &mut visited) {
            // Expose the host knob under its own name (the token the render-ui
            // reads) without clobbering a same-named local concrete knob…
            map.entry(fwd_key.into()).or_insert_with(|| resolved.clone());
            // …and collapse the forwarding field itself.
            map.insert(field.into(), resolved);
        }
    }
}

/// The single-segment key of a `@config.<key>` wrapper-forward string. Nested
/// paths (`@config.x.y`) are not host-knob forwards and return `None`.
pub(super) fn forward_key(s: &str) -> Option<String> {
    s.strip_prefix("@config.")
        .filter(|rest| !rest.contains('.'))
        .map(|rest| rest.to_string())
}

/// Resolve a host-knob `key` to its effective value by searching the orbital's
/// traits (deterministic name order) for the one that declares it. Prefers a
/// concrete default; if the only declarations are themselves forwards, chains
/// transitively. `visited` guards against forward cycles.
pub(super) fn resolve_forward_key<T: std::borrow::Borrow<TraitDefinition>>(
    key: &str,
    all_traits: &HashMap<String, T>,
    visited: &mut HashSet<String>,
) -> Option<Value> {
    if !visited.insert(key.to_string()) {
        return None;
    }
    let mut names: Vec<&String> = all_traits.keys().collect();
    names.sort();
    let mut chained_next: Option<String> = None;
    for name in names {
        let Some(cfg) = &all_traits[name].borrow().config else {
            continue;
        };
        let Some(field) = cfg.get(key) else { continue };
        let Some(default) = &field.default else {
            continue;
        };
        let v = sexpr_to_value(default);
        if let Value::String(s) = &v {
            if let Some(next) = forward_key(s) {
                if next != key {
                    chained_next.get_or_insert(next);
                }
                continue;
            }
        }
        return Some(v);
    }
    chained_next.and_then(|next| resolve_forward_key(&next, all_traits, visited))
}
