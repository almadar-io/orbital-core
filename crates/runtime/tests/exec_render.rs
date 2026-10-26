//! EXEC-RENDER conformance: how a `render-ui` effect resolves where it paints.

mod support;

use orbital_core_base::effect_types::{Effect, EffectOutcome};
use orbital_core_base::value::Value;
use orbital_core_runtime::config::KernelConfig;
use serde_json::{json, Value as Json};
use support::TestKernel;

fn render_outcome(config: Json, slot: &str) -> EffectOutcome {
    let kernel = TestKernel::client().config(KernelConfig::default()).build(json!({
        "name": "App",
        "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "E", "config": config,
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", slot, { "type": "typography", "content": "x" }]] }] } }]
        }]
    }));
    let outcomes = kernel.dispatch("T", None, "INIT", Value::Null).unwrap();
    outcomes
        .iter()
        .filter(|(t, _)| t == "T")
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|e| matches!(e.effect, Effect::RenderUi { .. }))
        .map(|e| e.outcome.clone())
        .expect("a render-ui effect result")
}

#[test]
fn exec_render_7_a_slot_binding_that_resolves_to_nothing_fails_the_effect() {
    let outcome = render_outcome(json!({ "detailSlot": { "type": "slot" } }), "@config.detailSlot");
    assert!(matches!(outcome, EffectOutcome::Failed), "{outcome:?}");
}

#[test]
fn exec_render_7_control_a_literal_slot_does_not_fail() {
    let outcome = render_outcome(json!({}), "main");
    assert!(!matches!(outcome, EffectOutcome::Failed), "{outcome:?}");
}
