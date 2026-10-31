// G-CROSS-357: a render-ui inside a `let` resolves its props after the step against the post-`set` entity (EXEC-STEP-4),
// with the let bindings it ran under in scope, and a later repaint keeps them.

use orbital_core_base::client_effect::ClientEffect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn arm(event: &str, effects: Json) -> Json {
    json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
}

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "letscope",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Board", "persistence": "runtime", "shared": true,
                "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "default": 0 }] },
            "traits": [
                { "name": "Viewer", "linkedEntity": "Board", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    arm("INIT", json!([])),
                    arm("LOCAL", json!([["let", [["t", 3]], ["render-ui", "main", { "type": "typography", "content": ["str/concat", "n=", "@t"] }]]])),
                    arm("LOCAL_THEN_SET", json!([
                        ["let", [["t", 3]], ["render-ui", "main", { "type": "typography", "content": ["str/concat", "@t", "/", "@entity.score"] }]],
                        ["set", "@entity.score", 5]
                    ])),
                    arm("NO_LET", json!([["render-ui", "main", { "type": "typography", "content": ["str/concat", "s=", "@entity.score"] }]])),
                    arm("FRAGILE", json!([["render-ui", "aside", { "type": "typography", "content": ["str/concat", "big=", ["if", [">", "@entity.score", 0], ["array/len", "@entity.score"], 0]] }]]))
                  ] } },
                { "name": "Driver", "linkedEntity": "Board", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    arm("INIT", json!([])),
                    arm("BUMP", json!([["set", "@entity.score", ["+", "@entity.score", 1]]]))
                  ] } }
            ]
        }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(RuntimeEnvironment::Client, cache as Arc<dyn EntitySource>, KernelConfig::for_environment(RuntimeEnvironment::Client));
    k.register_schema(schema).unwrap();
    for t in ["Viewer", "Driver"] {
        k.dispatch_with_server_leg("Main", t, None, "INIT", Value::Null, None).unwrap();
    }
    k
}

fn run(k: &RuntimeKernel, trait_name: &str, event: &str) -> EventResponse {
    k.dispatch_with_server_leg("Main", trait_name, None, event, Value::Null, None).unwrap().response
}

fn viewer_text(r: &EventResponse) -> Vec<Json> {
    r.client_effects_by_trait
        .iter()
        .flatten()
        .filter(|e| e.trait_name == "Viewer")
        .filter_map(|e| match &e.effect {
            ClientEffect::RenderUi { .. } => serde_json::to_value(e.effect.render_ui_value()?).ok().map(|c| c["content"].clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_let_binding_reaches_an_operator_call_in_a_render_prop() {
    let k = kernel();
    let r = run(&k, "Viewer", "LOCAL");
    assert!(r.success, "{:?}", r.rejections);
    assert_eq!(viewer_text(&r), [json!("n=3")]);
}

#[test]
fn a_render_inside_a_let_still_reads_the_post_set_entity() {
    let k = kernel();
    let r = run(&k, "Viewer", "LOCAL_THEN_SET");
    assert!(r.success, "{:?}", r.rejections);
    assert_eq!(viewer_text(&r), [json!("3/5")]);
}

#[test]
fn a_repaint_of_a_render_inside_a_let_keeps_its_bindings() {
    let k = kernel();
    run(&k, "Viewer", "LOCAL_THEN_SET");
    let r = run(&k, "Driver", "BUMP");
    assert_eq!(viewer_text(&r), [json!("3/6")], "the repaint reads the new score with the let binding still bound");
}

#[test]
fn control_a_render_outside_any_let_resolves_as_before() {
    let k = kernel();
    let r = run(&k, "Viewer", "NO_LET");
    assert_eq!(viewer_text(&r), [json!("s=0")]);
}

// G-ORBITAL-CORE-090 (owner 2026-10-31): a repaint whose prop call fails keeps the slot's last paint and answers a typed
// `repaint-failed` rejection naming the trait, the slot and the error; the raw expression never reaches the host.
#[test]
fn a_repaint_whose_prop_call_fails_keeps_the_last_paint_and_answers_a_typed_fault() {
    use orbital_core_runtime::effects::response::TransitionRejectionCode;
    let k = kernel();
    let first = run(&k, "Viewer", "FRAGILE");
    assert!(first.success, "{:?}", first.rejections);
    let r = run(&k, "Driver", "BUMP");
    assert!(viewer_text(&r).is_empty(), "nothing broken is repainted: {:?}", viewer_text(&r));
    let fault = r
        .rejections
        .iter()
        .flatten()
        .find(|f| f.code == TransitionRejectionCode::RepaintFailed)
        .expect("a repaint-failed rejection");
    assert_eq!(fault.trait_name.as_deref(), Some("Viewer"));
    assert_eq!(fault.slot.as_deref(), Some("aside"));
    assert!(fault.error.as_deref().is_some_and(|e| !e.is_empty()));
}

#[test]
fn control_a_repaint_that_resolves_answers_no_fault() {
    use orbital_core_runtime::effects::response::TransitionRejectionCode;
    let k = kernel();
    run(&k, "Viewer", "NO_LET");
    let r = run(&k, "Driver", "BUMP");
    assert_eq!(viewer_text(&r), [json!("s=1")]);
    assert!(r.rejections.iter().flatten().all(|f| f.code != TransitionRejectionCode::RepaintFailed));
}
