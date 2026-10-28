// D-12 in the kernel: a `render-ui` whose props hold an operator call with a mistyped argument fails its
// transition with the typed mismatch (an `effect-failed` rejection), as the JS executor throws. It used to
// keep the unevaluated expression as the prop. JS twin: `OrbPreview.mount-error.test.tsx` (kernel mode).

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
        "typeContractVersion": 1, "orbitals": [{
            "name": "PageOrbital",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [trait_with("Header", "shown", vec![render("header")]), trait_with("Broken", "ready", broken)]
        }]
    }))
    .unwrap()
}

fn mistyped() -> Vec<Json> {
    vec![json!([
        "render-ui",
        "main",
        { "type": "typography", "content": ["str/join", ["array/filter", "not-a-list", ["fn", "x", true]], ", "] }
    ])]
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
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
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
fn a_mistyped_argument_in_render_props_is_an_effect_failed_rejection_with_the_typed_error() {
    let response = run(schema(mistyped()));
    let rejection = response
        .rejections
        .iter()
        .flatten()
        .find(|r| r.code == TransitionRejectionCode::EffectFailed)
        .expect("an effect-failed rejection");
    assert_eq!(rejection.trait_name.as_deref(), Some("Broken"));
    assert!(
        rejection
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Type mismatch: expected array, got string")),
        "{:?}",
        rejection.error
    );
}

#[test]
fn the_failing_trait_paints_nothing_and_the_other_trait_still_paints() {
    let response = run(schema(mistyped()));
    assert_eq!(painted(&response, "Broken"), Vec::<String>::new());
    assert_eq!(painted(&response, "Header").len(), 1);
}

#[test]
fn control_a_well_typed_call_in_render_props_paints() {
    let ok = vec![json!([
        "render-ui",
        "main",
        { "type": "typography", "content": ["str/join", ["array/filter", ["list", "a", "b"], ["fn", "x", true]], ", "] }
    ])];
    let response = run(schema(ok));
    assert!(response
        .rejections
        .iter()
        .flatten()
        .all(|r| r.code != TransitionRejectionCode::EffectFailed));
    assert_eq!(painted(&response, "Broken").len(), 1);
}
