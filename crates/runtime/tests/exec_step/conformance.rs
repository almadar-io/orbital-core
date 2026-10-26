//! EXEC-STEP conformance: what a step's effects do to the trait's frame.


use orbital_core_base::value::Value;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

#[test]
fn exec_step_3_set_merges_into_the_frame_and_keeps_untouched_fields() {
    let kernel = TestKernel::server().build(json!({
        "name": "Merge",
        "orbitals": [{
            "name": "MergeOrbital",
            "entity": { "name": "S", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "a", "type": "string" }, { "name": "b", "type": "string" }] },
            "traits": [{ "name": "Writer", "linkedEntity": "S",
                "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                    "transitions": [
                        { "from": "on", "to": "on", "event": "WRITE_A", "effects": [["set", "@entity.a", "first"]] },
                        { "from": "on", "to": "on", "event": "WRITE_B", "effects": [["set", "@entity.b", "second"]] }
                    ] } }]
        }]
    }));
    kernel.dispatch("Writer", None, "WRITE_A", Value::Null).unwrap();
    kernel.dispatch("Writer", None, "WRITE_B", Value::Null).unwrap();

    let frame = kernel.snapshot_trait("Writer", None).entity;
    assert_eq!(frame.get_path("b"), Some(&Value::from("second")));
    assert_eq!(frame.get_path("a"), Some(&Value::from("first")), "the earlier write survives a later set of another field");
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[test]
fn exec_step_6_an_absent_payload_reads_as_an_empty_object() {
    let kernel = TestKernel::server().build(json!({
        "name": "Payloads",
        "orbitals": [{
            "name": "P",
            "entity": { "name": "S", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "seen", "type": "object" }] },
            "traits": [{ "name": "Reader", "linkedEntity": "S",
                "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                    "transitions": [{ "from": "on", "to": "on", "event": "LOOK", "effects": [["set", "@entity.seen", "@payload"]] }] } }]
        }]
    }));
    kernel.dispatch("Reader", None, "LOOK", Value::Null).unwrap();
    assert_eq!(kernel.snapshot_trait("Reader", None).entity.get_path("seen"), Some(&empty_object()));
}

#[test]
fn exec_step_6_a_tick_reads_its_payload_as_an_empty_object() {
    let kernel = TestKernel::client().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }, { "name": "seen", "type": "object" }] },
            "traits": [{ "name": "Ticker", "linkedEntity": "Note", "dispatch": "client",
                "stateMachine": { "states": [{ "name": "running", "isInitial": true }],
                    "transitions": [{ "from": "running", "to": "running", "event": "INIT", "effects": [["set", "@entity.seen", { "boot": true }]] }] },
                "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.seen", "@payload"]] }] }]
        }]
    }));
    kernel.dispatch_with_server_leg("App", "Ticker", None, "INIT", Value::Null, None).unwrap();
    kernel.dispatch_tick("Main", "Ticker", None, "beat", None).unwrap();
    assert_eq!(kernel.snapshot_trait("Ticker", None).entity.get_path("seen"), Some(&empty_object()));
}

/// The settled response's effect results for one `persist` followed by `emit AFTER`, as `(effect, target)` pairs.
fn settled_effects(persist: Json) -> Vec<(String, Option<String>)> {
    let route = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let mut persist = persist;
    persist.as_array_mut().unwrap().push(route);
    let kernel = TestKernel::server().build(json!({
        "name": "Settled",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{ "name": "Notes", "linkedEntity": "Note",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [persist, ["emit", "AFTER", {}]] }] } }]
        }]
    }));
    let outcomes = kernel.dispatch("Notes", None, "GO", Value::Null).unwrap();
    orbital_core_runtime::effects::response::EventResponse::from_process_results(&outcomes, "Notes", "GO")
        .effect_results
        .unwrap_or_default()
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .map(|r| (r["effect"].as_str().unwrap_or_default().to_string(), r["target"].as_str().map(str::to_string)))
        .collect()
}

fn position(effects: &[(String, Option<String>)], effect: &str, target: Option<&str>) -> usize {
    effects
        .iter()
        .position(|(e, t)| e == effect && t.as_deref() == target)
        .unwrap_or_else(|| panic!("no {effect} {target:?} in {effects:?}"))
}

#[test]
fn exec_step_9_a_routed_success_emit_is_listed_right_after_the_effect_that_raised_it() {
    let effects = settled_effects(json!(["persist", "create", "Note", { "id": "n1", "title": "a" }]));
    let persist = position(&effects, "persist", None);
    assert_eq!(position(&effects, "emit", Some("SAVED")), persist + 1, "{effects:?}");
    assert!(position(&effects, "emit", Some("AFTER")) > persist + 1, "{effects:?}");
}

#[test]
fn exec_step_9_a_routed_failure_emit_is_listed_right_after_the_effect_that_raised_it() {
    let effects = settled_effects(json!(["persist", "update", "Note", { "id": "missing", "title": "b" }]));
    let persist = position(&effects, "persist", None);
    assert_eq!(position(&effects, "emit", Some("SAVE_FAILED")), persist + 1, "{effects:?}");
    assert!(position(&effects, "emit", Some("AFTER")) > persist + 1, "{effects:?}");
}
