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
    schema_with_fields(vec![json!({ "name": "id", "type": "string" })], broken)
}

fn schema_with_fields(fields: Vec<Json>, broken: Vec<Json>) -> OrbitalSchema {
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
            "entity": { "name": "Item", "persistence": "runtime", "fields": fields },
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

// G-ORB-477: an emit whose payload fails to evaluate is a failed effect: an effect-failed rejection, and nothing
// is emitted, never the unevaluated payload.
fn emitted(response: &EventResponse, event: &str) -> Vec<Json> {
    response
        .emitted_events
        .iter()
        .filter(|e| e.event == event)
        .map(|e| serde_json::to_value(&e.payload).unwrap())
        .collect()
}

#[test]
fn an_emit_whose_payload_fails_to_evaluate_is_an_effect_failed_rejection_and_emits_nothing() {
    let response = run(schema(vec![json!(["emit", "RESPOND", {
        "requestId": "@entity.inflight",
        "headers": ["array/append", ["object/get", null, "responseHeaders"], { "name": "content-type" }]
    }])]));
    assert!(response
        .rejections
        .iter()
        .flatten()
        .any(|r| r.code == TransitionRejectionCode::EffectFailed && r.trait_name.as_deref() == Some("Broken")));
    assert_eq!(emitted(&response, "RESPOND"), Vec::<Json>::new());
}

#[test]
fn control_an_emit_whose_payload_evaluates_is_emitted_resolved() {
    let response = run(schema(vec![json!(["emit", "RESPOND", { "status": ["if", true, 403, 200] }])]));
    assert!(response.rejections.iter().flatten().all(|r| r.code != TransitionRejectionCode::EffectFailed));
    assert_eq!(emitted(&response, "RESPOND"), vec![json!({ "status": 403.0 })]);
}

fn effect_failed(response: &EventResponse) -> Option<String> {
    response
        .rejections
        .iter()
        .flatten()
        .find(|r| r.code == TransitionRejectionCode::EffectFailed)
        .map(|r| r.error.clone().unwrap_or_default())
}

#[test]
fn exec_eval_4_a_mistyped_call_in_navigate_params_is_the_effects_failure_and_paints_nothing() {
    let response = run(schema(vec![
        render("before"),
        json!(["navigate", "/items", { "tags": ["str/join", ["array/filter", "not-a-list", ["fn", "x", true]], ", "] }]),
    ]));
    let error = effect_failed(&response).expect("an effect-failed rejection");
    assert!(!error.is_empty());
    assert_eq!(painted(&response, "Broken"), Vec::<String>::new());
}

#[test]
fn exec_eval_4_control_well_typed_navigate_params_do_not_fail() {
    let response = run(schema(vec![json!(["navigate", "/items", { "tags": ["str/join", ["array/filter", ["a", "b"], ["fn", "x", true]], ", "] }])]));
    assert_eq!(effect_failed(&response), None);
}

// G-ORBITAL-CORE-089: a `let` whose binding value throws fails as the `let` (EXEC-EVAL-4); its body never runs with
// the name unbound.
#[test]
fn a_let_whose_binding_throws_is_an_effect_failed_rejection_and_its_body_never_runs() {
    let response = run(schema(vec![json!(["let", [["x", ["behavior/ref", "Indigo Pioneer"]]], render("after")])]));
    let rejection = response
        .rejections
        .iter()
        .flatten()
        .find(|r| r.code == TransitionRejectionCode::EffectFailed)
        .expect("the let's binding failure is an effect-failed rejection");
    assert_eq!(rejection.trait_name.as_deref(), Some("Broken"));
    assert!(painted(&response, "Broken").is_empty(), "the body ran with the name unbound");
}

#[test]
fn control_a_let_whose_bindings_evaluate_runs_its_body() {
    let response = run(schema(vec![json!(["let", [["x", "fine"]], render("@x")])]));
    assert!(response
        .rejections
        .iter()
        .flatten()
        .all(|r| r.code != TransitionRejectionCode::EffectFailed));
    assert_eq!(painted(&response, "Broken").len(), 1);
}

// G-ORBITAL-CORE-093: a `set` of null into a field that declares no null fails the effect (EXEC-STEP-3, EXEC-STEP-7).
// Validate refuses the unguarded sets that can write one, so a validated program never reaches this.
fn null_fields() -> Vec<Json> {
    vec![
        json!({ "name": "id", "type": "string" }),
        json!({ "name": "name", "type": "string" }),
        json!({ "name": "nick", "type": "string", "nullable": true }),
    ]
}

fn broken_state(response: &EventResponse) -> Option<&str> {
    response.states.get("Broken").map(String::as_str)
}

fn effect_failed_rejection(response: &EventResponse) -> Option<&orbital_core_runtime::effects::response::TransitionRejection> {
    response.rejections.iter().flatten().find(|r| r.code == TransitionRejectionCode::EffectFailed)
}

#[test]
fn a_null_set_into_a_non_nullable_field_fails_the_effect_and_stops_the_transition() {
    for value in [json!(null), json!("@entity.never")] {
        let response = run(schema_with_fields(null_fields(), vec![
            json!(["set", "@entity.nick", "kept"]),
            json!(["set", "@entity.name", value]),
            render("after"),
        ]));
        let rejection = effect_failed_rejection(&response).unwrap_or_else(|| panic!("an effect-failed rejection for {value}"));
        assert_eq!(rejection.trait_name.as_deref(), Some("Broken"));
        let error = rejection.error.as_deref().unwrap_or_default();
        assert!(error.contains("name") && error.contains("null"), "the error names the field and the null: {error}");
        assert_eq!(broken_state(&response), Some("ready"), "the new state stands");
        assert_eq!(painted(&response, "Broken"), Vec::<String>::new(), "nothing after the failure is reported");
    }
}

#[test]
fn control_a_null_into_a_nullable_field_and_a_value_into_a_non_nullable_one_run() {
    for effects in [
        vec![json!(["set", "@entity.nick", null]), render("after")],
        vec![json!(["set", "@entity.nick", "@entity.never"]), render("after")],
        vec![json!(["set", "@entity.name", "text"]), render("after")],
    ] {
        let response = run(schema_with_fields(null_fields(), effects.clone()));
        assert!(effect_failed_rejection(&response).is_none(), "{effects:?}");
        assert_eq!(painted(&response, "Broken").len(), 1, "{effects:?}");
    }
}
