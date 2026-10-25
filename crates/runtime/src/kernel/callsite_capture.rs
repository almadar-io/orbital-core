//! Call-site payload capture: which embedded children a transition re-renders under its own payload.
//!
//! A JSX-hoisted child trait reads `@callsitePayload.<field>` — the payload of the transition that
//! composed it (`@trait.Child` in that transition's `render-ui`). A child renders once at its own
//! lifecycle event and never again, so after the composing transition the server re-fires the child's
//! lifecycle arm under the composing payload. This module is the static half: the adjacency, the
//! children a set of fired effects compose, and the lifecycle event a repaint may use. JS twin:
//! `@almadar/core` `embedded-trait-config.ts` + `StateMachineCore.repaintLifecycleEvent`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::TraitDefinition;
use std::collections::{HashMap, HashSet};

const TRAIT_BINDING_PREFIX: &str = "@trait.";
const CONFIG_BINDING_PREFIX: &str = "@config.";
const CALLSITE_PAYLOAD_PREFIX: &str = "@callsitePayload.";

/// The lifecycle events a trait runs when it enters a page, in the order a repaint tries them.
const LIFECYCLE_EVENTS: [&str; 3] = ["INIT", "LOAD", "$MOUNT"];

/// Names in the order they were first met.
pub type Names = Vec<String>;

fn add(into: &mut Names, name: &str) {
    if !into.iter().any(|n| n == name) {
        into.push(name.to_string());
    }
}

fn as_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

fn trait_refs(value: &serde_json::Value, into: &mut Names) {
    match value {
        serde_json::Value::String(s) => {
            if let Some(rest) = s.strip_prefix(TRAIT_BINDING_PREFIX) {
                let name = rest.split('.').next().unwrap_or_default();
                if !name.is_empty() {
                    add(into, name);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| trait_refs(v, into)),
        serde_json::Value::Object(map) => map.values().for_each(|v| trait_refs(v, into)),
        _ => {}
    }
}

fn config_keys(value: &serde_json::Value, into: &mut Names) {
    match value {
        serde_json::Value::String(s) => {
            if let Some(rest) = s.strip_prefix(CONFIG_BINDING_PREFIX) {
                let key = rest.split('.').next().unwrap_or_default();
                if !key.is_empty() {
                    add(into, key);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| config_keys(v, into)),
        serde_json::Value::Object(map) => map.values().for_each(|v| config_keys(v, into)),
        _ => {}
    }
}

fn contains_prefixed(value: &serde_json::Value, prefix: &str) -> bool {
    match value {
        serde_json::Value::String(s) => s.starts_with(prefix),
        serde_json::Value::Array(items) => items.iter().any(|v| contains_prefixed(v, prefix)),
        serde_json::Value::Object(map) => map.values().any(|v| contains_prefixed(v, prefix)),
        _ => false,
    }
}

/// Whether the trait's own config or state machine reads `@callsitePayload.<field>`.
fn references_callsite_payload(def: &TraitDefinition) -> bool {
    def.config
        .as_ref()
        .is_some_and(|config| contains_prefixed(&as_json(config), CALLSITE_PAYLOAD_PREFIX))
        || contains_prefixed(&as_json(&def.state_machine), CALLSITE_PAYLOAD_PREFIX)
}

/// Referrer trait -> the embedded children (`@trait.X` in its config or state machine) whose subtree
/// needs a re-render under the referrer's payload: a child that reads `@callsitePayload`, or is a
/// pass-through to a descendant that does. One orbital's traits, so same-named traits of sibling
/// orbitals never alias.
pub fn capture_children(traits: &[&TraitDefinition]) -> HashMap<String, Names> {
    let by_name: HashMap<&str, &TraitDefinition> =
        traits.iter().map(|t| (t.name.as_str(), *t)).collect();
    let mut adjacency: HashMap<&str, Names> = HashMap::new();
    for def in traits {
        let mut refs = Names::new();
        if let Some(config) = &def.config {
            trait_refs(&as_json(config), &mut refs);
        }
        trait_refs(&as_json(&def.state_machine), &mut refs);
        refs.retain(|name| name != &def.name);
        if !refs.is_empty() {
            adjacency.insert(def.name.as_str(), refs);
        }
    }

    fn reaches_capture(
        name: &str,
        by_name: &HashMap<&str, &TraitDefinition>,
        adjacency: &HashMap<&str, Names>,
        seen: &mut HashSet<String>,
    ) -> bool {
        if !seen.insert(name.to_string()) {
            return false;
        }
        if by_name
            .get(name)
            .is_some_and(|def| references_callsite_payload(def))
        {
            return true;
        }
        adjacency.get(name).is_some_and(|children| {
            children
                .iter()
                .any(|c| reaches_capture(c, by_name, adjacency, seen))
        })
    }

    let mut out = HashMap::new();
    for (referrer, children) in &adjacency {
        let keep: Names = children
            .iter()
            .filter(|child| reaches_capture(child, &by_name, &adjacency, &mut HashSet::new()))
            .cloned()
            .collect();
        if !keep.is_empty() {
            out.insert((*referrer).to_string(), keep);
        }
    }
    out
}

/// The `@trait.X` children some fired effects compose: directly, or through an `@config.K` they render
/// whose value on `def` holds `@trait.X`.
pub fn embedded_by_effects(def: &TraitDefinition, effects: &[&SExpression]) -> Names {
    let effects_json = serde_json::Value::Array(effects.iter().map(|e| as_json(*e)).collect());
    let mut refs = Names::new();
    trait_refs(&effects_json, &mut refs);
    if let Some(config) = &def.config {
        let mut keys = Names::new();
        config_keys(&effects_json, &mut keys);
        for key in keys {
            if let Some(field) = config.get(&key) {
                trait_refs(&as_json(field), &mut refs);
            }
        }
    }
    refs.retain(|name| name != &def.name);
    refs
}

/// The lifecycle event an embedded child can be repainted with: the one it handles from `state`, provided
/// every matching arm stays in that state and only renders. A repaint never moves a trait (a loaded
/// list's `browsing -> loading` INIT) nor reloads it (a fetch plus spinner self-loop).
pub fn repaint_lifecycle_event(def: &TraitDefinition, state: &str) -> Option<&'static str> {
    for event in LIFECYCLE_EVENTS {
        let arms: Vec<_> = def
            .state_machine
            .transitions
            .iter()
            .filter(|t| t.event == event && t.from.matches(state))
            .collect();
        if arms.is_empty() {
            continue;
        }
        let repaints = arms.iter().all(|t| {
            t.to == state
                && t.effects.iter().flatten().all(|effect| {
                    matches!(effect, SExpression::Array(items)
                        if matches!(items.first(), Some(SExpression::String(head)) if head == "render-ui"))
                })
        });
        return repaints.then_some(event);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(value: serde_json::Value) -> TraitDefinition {
        serde_json::from_value(value).unwrap()
    }

    fn host() -> TraitDefinition {
        def(
            json!({ "name": "Host", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "LOADED",
                "effects": [["render-ui", "main", { "type": "stack", "children": ["@trait.Child"] }]] }] } }),
        )
    }

    fn child(captures: bool) -> TraitDefinition {
        let label = if captures {
            "@callsitePayload.error"
        } else {
            "static"
        };
        def(
            json!({ "name": "Child", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                "effects": [["render-ui", "main", { "type": "button", "label": label }]] }] } }),
        )
    }

    #[test]
    fn a_child_that_reads_the_capture_is_a_capture_child_of_its_referrer() {
        let (h, c) = (host(), child(true));
        let map = capture_children(&[&h, &c]);
        assert_eq!(map["Host"], vec!["Child".to_string()]);
    }

    #[test]
    fn control_a_child_that_never_reads_it_is_not() {
        let (h, c) = (host(), child(false));
        assert!(capture_children(&[&h, &c]).is_empty());
    }

    #[test]
    fn a_pass_through_to_a_capturing_descendant_counts() {
        let h = host();
        let mid = def(
            json!({ "name": "Child", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                "effects": [["render-ui", "main", { "children": ["@trait.Leaf"] }]] }] } }),
        );
        let leaf = def(
            json!({ "name": "Leaf", "config": { "c": { "type": "string", "default": "@callsitePayload.x" } },
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }),
        );
        let map = capture_children(&[&h, &mid, &leaf]);
        assert_eq!(map["Host"], vec!["Child".to_string()]);
        assert_eq!(map["Child"], vec!["Leaf".to_string()]);
    }

    #[test]
    fn the_children_a_transition_composes_are_those_its_effects_name() {
        let h = host();
        let effects: Vec<&SExpression> = h.state_machine.transitions[0]
            .effects
            .iter()
            .flatten()
            .collect();
        assert_eq!(embedded_by_effects(&h, &effects), vec!["Child".to_string()]);
        assert!(embedded_by_effects(&h, &[]).is_empty());
    }

    #[test]
    fn a_child_composed_through_a_rendered_knob_is_named_too() {
        let h = def(
            json!({ "name": "Host", "config": { "body": { "type": "unknown", "default": "@trait.Child" } },
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                    "effects": [["render-ui", "main", "@config.body"]] }] } }),
        );
        let effects: Vec<&SExpression> = h.state_machine.transitions[0]
            .effects
            .iter()
            .flatten()
            .collect();
        assert_eq!(embedded_by_effects(&h, &effects), vec!["Child".to_string()]);
    }

    #[test]
    fn a_repaint_uses_a_lifecycle_arm_that_stays_and_only_renders() {
        assert_eq!(repaint_lifecycle_event(&child(true), "idle"), Some("INIT"));
    }

    #[test]
    fn a_repaint_never_moves_or_reloads_the_child() {
        let moves = def(
            json!({ "name": "C", "stateMachine": { "states": [{ "name": "browsing", "isInitial": true }, { "name": "loading" }],
            "transitions": [{ "from": "browsing", "to": "loading", "event": "INIT", "effects": [["render-ui", "main", {}]] }] } }),
        );
        assert_eq!(repaint_lifecycle_event(&moves, "browsing"), None);
        let reloads = def(
            json!({ "name": "C", "stateMachine": { "states": [{ "name": "a", "isInitial": true }],
            "transitions": [{ "from": "a", "to": "a", "event": "INIT", "effects": [["fetch", "X", {}], ["render-ui", "main", {}]] }] } }),
        );
        assert_eq!(repaint_lifecycle_event(&reloads, "a"), None);
        assert_eq!(repaint_lifecycle_event(&child(true), "elsewhere"), None);
    }
}
