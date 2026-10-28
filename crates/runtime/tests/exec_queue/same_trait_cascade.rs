// Runtime Spec 5.3: a trait's own emit that it handles from its new state runs within the same dispatch, before fan-out. Twin of almadar-runtime `same-trait-cascade.test.ts`.

use orbital_core_base::effect_types::Effect;
use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "loop",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Counter", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                    "transitions": [
                        { "from": "idle", "to": "running", "event": "GO", "effects": [["emit", "STEP", { "n": 3 }], ["emit", "UNHANDLED", {}]] },
                        { "from": "running", "to": "running", "event": "STEP", "guard": [">", "@payload.n", 0], "effects": [["emit", "STEP", { "n": ["-", "@payload.n", 1] }]] },
                        { "from": "running", "to": "idle", "event": "STEP", "guard": ["=", "@payload.n", 0], "effects": [["emit", "DONE", {}]] }
                    ]
                } },
                { "name": "Watcher",
                  "listens": [{ "event": "STEP", "triggers": "SAW", "source": { "kind": "trait", "trait": "Counter" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SAW", "effects": [["emit", "WATCHED", { "n": "@payload.n" }]] }] } }
            ]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn emitted(k: &RuntimeKernel) -> Vec<String> {
    k.dispatch("Counter", None, "GO", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => {
                Some(match payload.as_object().and_then(|o| o.get("n")) {
                    Some(Value::Number(n)) => format!("{event}:{n}"),
                    _ => event.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_trait_drives_its_own_loop_to_completion_in_one_dispatch() {
    let k = kernel();
    let e = emitted(&k);
    assert!(e.contains(&"DONE".to_string()), "{e:?}");
    assert_eq!(
        k.get_state("Counter", None)
            .unwrap()
            .current_state,
        "idle"
    );
}

#[test]
fn every_step_still_fans_out_to_other_listeners() {
    let e = emitted(&kernel());
    let watched: Vec<&String> = e.iter().filter(|s| s.starts_with("WATCHED")).collect();
    assert_eq!(
        watched,
        vec!["WATCHED:3", "WATCHED:2", "WATCHED:1", "WATCHED:0"]
    );
}

#[test]
fn an_emit_the_trait_has_no_arm_for_is_not_looped_back() {
    let e = emitted(&kernel());
    assert_eq!(e.iter().filter(|s| *s == "UNHANDLED").count(), 1);
}
