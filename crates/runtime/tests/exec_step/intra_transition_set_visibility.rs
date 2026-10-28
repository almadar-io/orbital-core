//! A `(set @entity.…)` must be visible to the LATER effects of the SAME transition.
//!
//! The dominant list-mutation idiom is `(set @entity.id ?id)` → `(persist update E
//! @entity)`: the row to act on arrives in the payload, is stamped onto the entity, and
//! the whole entity is written back. The kernel only merged Set-effect writes into the
//! stored entity AFTER the entire effect list had run, so the persist read a pre-set
//! `@entity`, resolved an EMPTY row id and failed with `Entity not found in cache: E:`.
//! Firing the same event from a state whose preamble had already set the id passed —
//! the tell that the write was invisible only *within* a transition.
//! `@almadar/runtime` mirrors the write into its bindings for exactly this reason
//! (`EffectExecutor.ts`); these are the missing Rust twin's regression tests
//! (C-KERNEL-INTRA-TRANSITION-SET-INVISIBLE).

use orbital_core_base::value::ValueMap;
use orbital_core_base::effect_types::{Effect, EffectOutcome};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::object(m)
}

/// One trait, one transition: stamp the payload's row id onto `@entity`, set a
/// field, then persist the whole entity — the `Inline*Manage` shape verbatim.
fn stamp_then_persist_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Policy",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "status", "type": "string" }
                ]
            },
            "traits": [{
                "name": "PolicyManage",
                "linkedEntity": "Policy",
                "stateMachine": {
                    "states": [
                        { "name": "browsing", "isInitial": true },
                        { "name": "loading" }
                    ],
                    "transitions": [
                        { "from": "browsing", "to": "loading", "event": "ARCHIVE",
                          "effects": [
                            ["set", "@entity.id", "@payload.id"],
                            ["set", "@entity.status", "archived"],
                            ["persist", "update", "Policy", "@entity"]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// A set whose VALUE reads a field set earlier in the same list.
fn chained_set_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Policy",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "status", "type": "string" },
                    { "name": "mirror", "type": "string" }
                ]
            },
            "traits": [{
                "name": "PolicyManage",
                "linkedEntity": "Policy",
                "stateMachine": {
                    "states": [
                        { "name": "browsing", "isInitial": true },
                        { "name": "loading" }
                    ],
                    "transitions": [
                        { "from": "browsing", "to": "loading", "event": "ARCHIVE",
                          "effects": [
                            ["set", "@entity.status", "archived"],
                            ["set", "@entity.mirror", "@entity.status"]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn kernel_with(schema: OrbitalSchema, cache: Arc<InMemoryEntityCache>) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn persist_sees_the_row_id_set_earlier_in_the_same_transition() {
    let cache = Arc::new(InMemoryEntityCache::new());
    // The row the payload points at exists — only the id resolution was broken.
    cache
        .create(
            "Policy",
            &obj(&[
                ("id", Value::string("Policy-1")),
                ("status", Value::string("active")),
            ]),
        )
        .unwrap();
    let kernel = kernel_with(stamp_then_persist_schema(), cache.clone());

    let result = kernel
        .process_event(
            "PolicyManage",
            Some("singleton"),
            "ARCHIVE",
            obj(&[("id", Value::string("Policy-1"))]),
        )
        .unwrap();
    assert!(result.executed, "the transition must fire");

    let persist = result
        .effect_results
        .iter()
        .find(|r| matches!(r.effect, Effect::Persist { .. }))
        .expect("the persist effect must be reported");
    assert!(
        matches!(persist.outcome, EffectOutcome::Executed),
        "persist did not execute ({:?}): {:?}",
        persist.outcome,
        persist.error
    );

    let row = cache.read("Policy", "Policy-1").unwrap();
    let fields = row.as_object().expect("the row must still exist");
    assert_eq!(
        fields.get("status"),
        Some(&Value::string("archived")),
        "the update must have landed on the payload's row"
    );
}

#[test]
fn a_set_value_reads_a_field_set_earlier_in_the_same_transition() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(chained_set_schema(), cache);

    let result = kernel
        .process_event(
            "PolicyManage",
            Some("singleton"),
            "ARCHIVE",
            Value::object(ValueMap::new()),
        )
        .unwrap();
    assert!(result.executed);

    let mirror = result
        .effect_results
        .iter()
        .filter_map(|r| match &r.effect {
            Effect::Set { path, value } if path == "@entity.mirror" => Some(value.clone()),
            _ => None,
        })
        .next()
        .expect("the mirroring set must be reported");
    assert_eq!(
        mirror,
        Value::string("archived"),
        "`@entity.status` must resolve to the value set one effect earlier"
    );
}

/// The same visibility inside control flow: a `when`/`if` condition, a `let` value and a
/// later `do` step read the transition's earlier `set`s too (std-global-search's fan-out is
/// gated on a field it just set). Twin: almadar-runtime `intra-transition-compound-sets.test.ts`.
fn compound_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Box", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
            "traits": [{
                "name": "T",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [
                        ["set", "@entity.n", 2],
                        ["when", [">", "@entity.n", 1], ["emit", "WHEN_SAW"]],
                        ["when", [">", "@entity.n", 5], ["emit", "NEVER"]],
                        ["if", [">", "@entity.n", 1], ["emit", "IF_SAW"], ["emit", "IF_MISSED"]],
                        ["let", [["m", "@entity.n"]], ["emit", "LET_SAW", { "m": "@m" }]],
                        ["do", ["set", "@entity.n", 3], ["when", [">", "@entity.n", 2], ["emit", "DO_SEQ"]]]
                    ] }]
                }
            }]
        }]
    }))
    .unwrap()
}

#[test]
fn control_flow_reads_the_transitions_earlier_sets() {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(compound_schema()).unwrap();
    let outcomes = kernel.dispatch("T", None, "GO", Value::Null).unwrap();
    let emitted: Vec<(String, Value)> = outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => Some((event.clone(), payload.clone())),
            _ => None,
        })
        .collect();
    let names: Vec<&str> = emitted.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names, vec!["WHEN_SAW", "IF_SAW", "LET_SAW", "DO_SEQ"]);
    let let_payload = &emitted[2].1;
    assert_eq!(
        let_payload.as_object().and_then(|o| o.get("m")),
        Some(&Value::Number(2.0))
    );
}
