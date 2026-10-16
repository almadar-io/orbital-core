// A failing effect never fails the dispatch: every other trait settles and paints, and the failure is a
// per-trait `effect-failed` rejection carrying the error. The failing transition stops at the throw: no later
// effect runs and it paints nothing. JS twin: `effect-failure-contained.test.ts`.

use orbital_core_runtime::effects::{LifecycleEvent, MountSeed};
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse, TransitionRejectionCode}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::{json, Value as Json};

fn render(label: &str) -> Json {
    json!(["render-ui", "main", { "type": "typography", "content": label }])
}

fn schema(broken: Vec<Json>) -> OrbitalSchema {
    let trait_with = |name: &str, to: &str, effects: Vec<Json>| {
        json!({
            "name": name, "linkedEntity": "Item",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": to }],
                "transitions": [{ "from": "idle", "to": to, "event": "INIT", "effects": effects }]
            }
        })
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "PageOrbital",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [trait_with("Header", "shown", vec![render("header")]), trait_with("Broken", "ready", broken)]
        }]
    }))
    .unwrap()
}

fn throwing() -> Vec<Json> {
    vec![
        render("loading"),
        json!(["set", "@entity.app", ["behavior/ref", "Indigo Pioneer"]]),
        render("after"),
    ]
}

fn mount() -> EventRequest {
    EventRequest {
        event: "INIT".to_string(),
        mount: Some(
            ["Header", "Broken"]
                .map(|t| MountSeed {
                    r#trait: t.to_string(),
                    event: LifecycleEvent::Init,
                })
                .to_vec(),
        ),
        ..Default::default()
    }
}

fn run(schema: OrbitalSchema) -> EventResponse {
    let server = EmbeddedServer::new(schema).unwrap();
    let mut progress = server.start("PageOrbital", mount()).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => panic!("the fixture asks for {other:?}"),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn painted(response: &EventResponse, trait_name: &str) -> Vec<String> {
    response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter(|e| e.trait_name == trait_name)
        .map(|e| serde_json::to_string(&e.effect).unwrap())
        .collect()
}

#[test]
fn the_other_trait_settles_and_paints_and_the_failure_is_an_effect_failed_rejection() {
    let response = run(schema(throwing()));
    assert!(response.success);
    assert_eq!(
        response.states.get("Header").map(String::as_str),
        Some("shown")
    );
    assert_eq!(painted(&response, "Header").len(), 1);
    let rejection = response
        .rejections
        .iter()
        .flatten()
        .find(|r| r.code == TransitionRejectionCode::EffectFailed)
        .expect("an effect-failed rejection");
    assert_eq!(rejection.trait_name.as_deref(), Some("Broken"));
    assert_eq!(rejection.event.as_deref(), Some("INIT"));
    assert_eq!(rejection.from.as_deref(), Some("idle"));
    assert!(rejection.error.as_deref().is_some_and(|e| !e.is_empty()));
}

#[test]
fn the_failing_transitions_state_stands() {
    let response = run(schema(throwing()));
    assert_eq!(
        response.states.get("Broken").map(String::as_str),
        Some("ready")
    );
}

#[test]
fn control_with_no_failing_effect_there_is_no_effect_failed_rejection() {
    let response = run(schema(vec![render("fine")]));
    assert!(response.success);
    assert_eq!(
        response.states.get("Broken").map(String::as_str),
        Some("ready")
    );
    assert!(response
        .rejections
        .iter()
        .flatten()
        .all(|r| r.code != TransitionRejectionCode::EffectFailed));
    assert_eq!(painted(&response, "Broken").len(), 1);
}

#[test]
fn the_failing_transition_paints_nothing_not_even_before_the_throw() {
    let response = run(schema(throwing()));
    assert_eq!(painted(&response, "Broken"), Vec::<String>::new());
}

#[test]
fn control_a_transition_with_no_failure_paints_every_render() {
    let response = run(schema(vec![render("before"), json!(["set", "@entity.ok", true]), render("after")]));
    assert_eq!(painted(&response, "Broken").len(), 2);
}
