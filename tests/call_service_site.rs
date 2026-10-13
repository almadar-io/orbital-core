// A `call-service` I/O request carries its call site (the calling trait, its orbital, the routes the site
// declares, and whether the step is a follow-on of the dispatch), and the host can settle the call as
// cancelled or deferred. JS twins: `call-service-host-ports`, `call-service-live-message`,
// `deferred-call-outcome`, `server-runtime-cancel-call` in `almadar-runtime/test`.

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::runtime::resumable::{CallRoutes, CallSite, IoAnswer, IoReply, IoRequest};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema, Value};
use serde_json::{json, Value as Json};

const ROUTES: &str = r#"{ "key": "job", "emit": { "success": "ANSWERED", "failure": "ANSWER_FAILED", "cancelled": "STOPPED", "onMessage": "STEP" } }"#;

fn call() -> Json {
    let options: Json = serde_json::from_str(ROUTES).unwrap();
    json!(["call-service", "svc", "work", {}, options])
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Starter", "linkedEntity": "Job",
                  "emits": [{ "event": "PING" }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "started" }],
                      "transitions": [{ "from": "idle", "to": "started", "event": "SUBMIT", "effects": [["emit", "PING"]] }]
                  } },
                { "name": "Helper", "linkedEntity": "Job",
                  "listens": [{ "event": "PING", "triggers": "ASK", "source": { "kind": "trait", "trait": "Starter" } }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }, { "name": "done" }, { "name": "stopped" }],
                      "transitions": [
                          { "from": "idle", "to": "asking", "event": "ASK", "effects": [call()] },
                          { "from": "asking", "to": "done", "event": "ANSWERED" },
                          { "from": "asking", "to": "stopped", "event": "STOPPED" }
                      ]
                  } },
                { "name": "Chain", "linkedEntity": "Job",
                  "emits": [{ "event": "NEXT" }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "first" }, { "name": "building" }],
                      "transitions": [
                          { "from": "idle", "to": "first", "event": "GO", "effects": [["emit", "NEXT"]] },
                          { "from": "first", "to": "building", "event": "NEXT", "effects": [call()] }
                      ]
                  } }
            ]
        }]
    }))
    .unwrap()
}

fn request(event: &str, target: &str) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some(target.to_string()),
        ..Default::default()
    }
}

/// Run `request`, answering every call with `answer`, and report the call sites seen.
fn run(request: EventRequest, answer: IoAnswer) -> (EventResponse, Vec<Option<CallSite>>) {
    run_in(schema(), request, answer)
}

fn run_in(
    schema: OrbitalSchema,
    request: EventRequest,
    answer: IoAnswer,
) -> (EventResponse, Vec<Option<CallSite>>) {
    let server = EmbeddedServer::new(schema).unwrap();
    let mut sites = Vec::new();
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return (response, sites),
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::CallService { site, .. } => {
                                sites.push(site);
                                answer.clone()
                            }
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

fn routes() -> CallRoutes {
    CallRoutes {
        success: Some("ANSWERED".into()),
        failure: Some("ANSWER_FAILED".into()),
        cancelled: Some("STOPPED".into()),
        on_message: Some("STEP".into()),
    }
}

fn emitted(response: &EventResponse) -> Vec<(&str, Option<&Value>)> {
    response
        .emitted_events
        .iter()
        .map(|e| (e.event.as_str(), e.payload.as_ref()))
        .collect()
}

fn ok() -> IoAnswer {
    IoAnswer::Service(Ok(serde_json::from_value(json!({ "answer": 42 })).unwrap()))
}

#[test]
fn a_requests_own_call_names_its_trait_orbital_and_routes() {
    let (_, sites) = run(request("ASK", "Helper"), ok());
    assert_eq!(
        sites,
        [Some(CallSite {
            orbital: Some("Main".into()),
            trait_name: "Helper".into(),
            follow_on: false,
            emit: routes()
        })]
    );
}

#[test]
fn a_listeners_call_is_a_follow_on() {
    let (_, sites) = run(request("SUBMIT", "Starter"), ok());
    let [Some(site)] = &sites[..] else {
        panic!("one call: {sites:?}")
    };
    assert_eq!(site.trait_name, "Helper");
    assert!(site.follow_on);
}

#[test]
fn the_calling_traits_own_next_step_is_a_follow_on() {
    let (_, sites) = run(request("GO", "Chain"), ok());
    let [Some(site)] = &sites[..] else {
        panic!("one call: {sites:?}")
    };
    assert_eq!(site.trait_name, "Chain");
    assert!(site.follow_on);
}

#[test]
fn a_result_fires_the_success_route() {
    let (response, _) = run(request("ASK", "Helper"), ok());
    assert_eq!(
        response.states.get("Helper").map(String::as_str),
        Some("done")
    );
}

#[test]
fn a_cancelled_call_fires_the_cancelled_route_with_its_key() {
    let (response, _) = run(request("ASK", "Helper"), IoAnswer::ServiceCancelled);
    assert_eq!(
        response.states.get("Helper").map(String::as_str),
        Some("stopped")
    );
    let events = emitted(&response);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].0, "STOPPED");
    assert_eq!(
        events[0]
            .1
            .and_then(|p| p.get_path("key"))
            .and_then(Value::as_str),
        Some("job")
    );
}

#[test]
fn control_a_failed_call_is_not_cancelled() {
    let (response, _) = run(
        request("ASK", "Helper"),
        IoAnswer::Service(Err("model down".to_string())),
    );
    assert!(emitted(&response)
        .iter()
        .any(|(e, _)| *e == "ANSWER_FAILED"));
    assert!(emitted(&response).iter().all(|(e, _)| *e != "STOPPED"));
}

#[test]
fn a_deferred_call_fires_no_outcome_now() {
    let (response, _) = run(request("ASK", "Helper"), IoAnswer::ServiceDeferred);
    assert!(response.success);
    assert_eq!(
        response.states.get("Helper").map(String::as_str),
        Some("asking")
    );
    assert!(emitted(&response).is_empty(), "{:?}", emitted(&response));
}

fn unrouted_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Plain", "linkedEntity": "Job",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "asked" }],
                    "transitions": [{ "from": "idle", "to": "asked", "event": "ASK",
                        "effects": [["call-service", "svc", "work", {}], ["emit", "AFTER"]] }]
                } }]
        }]
    }))
    .unwrap()
}

fn has_effect_failed(response: &EventResponse) -> bool {
    response
        .rejections
        .iter()
        .flatten()
        .any(|r| r.code == orbital_core::TransitionRejectionCode::EffectFailed)
}

#[test]
fn a_failed_call_with_no_failure_route_is_an_effect_failed_rejection_and_stops_its_transition() {
    let (response, _) = run_in(
        unrouted_schema(),
        request("ASK", "Plain"),
        IoAnswer::Service(Err("model down".to_string())),
    );
    assert!(has_effect_failed(&response));
    assert!(emitted(&response).iter().all(|(e, _)| *e != "AFTER"));
}

#[test]
fn control_a_failed_call_with_a_failure_route_is_handled_not_an_effect_failed_rejection() {
    let (response, _) = run(
        request("ASK", "Helper"),
        IoAnswer::Service(Err("model down".to_string())),
    );
    assert!(!has_effect_failed(&response));
}

#[test]
fn control_a_call_that_succeeds_runs_the_effects_after_it() {
    let (response, _) = run_in(unrouted_schema(), request("ASK", "Plain"), ok());
    assert!(!has_effect_failed(&response));
    assert!(emitted(&response).iter().any(|(e, _)| *e == "AFTER"));
}
