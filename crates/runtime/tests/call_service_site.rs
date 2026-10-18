// A `call-service` I/O request carries its call site (the calling trait, its orbital, the routes the site
// declares, and whether the step is a follow-on of the dispatch), and the host can settle the call as
// cancelled or deferred. JS twins: `call-service-host-ports`, `call-service-live-message`,
// `deferred-call-outcome`, `server-runtime-cancel-call` in `almadar-runtime/test`.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{CallRoutes, CallSite, IoAnswer, IoReply, IoRequest};
use serde_json::{json, Value as Json};
use {
    orbital_core_base::schema::types::OrbitalSchema,
    orbital_core_base::value::Value,
    orbital_core_runtime::effects::response::{EventRequest, EventResponse},
};

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
            entity_id: "singleton".to_string(),
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
    response.rejections.iter().flatten().any(|r| {
        r.code == orbital_core_runtime::effects::response::TransitionRejectionCode::EffectFailed
    })
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

// The arguments a `call-service` hands its provider are resolved like any effect's: a binding at any depth is its value
// (JS twin: `EffectExecutor` `call-service` interpolates its params).
fn args_schema(args: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Judge", "linkedEntity": "Job",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "JUDGE", "effects": [["call-service", "knowledge", "status", args]] }] } }]
        }]
    }))
    .unwrap()
}

fn asked_args(args: Json) -> Json {
    let server = EmbeddedServer::new(args_schema(args)).unwrap();
    let progress = server
        .start(
            "Main",
            EventRequest {
                event: "JUDGE".to_string(),
                target_trait: Some("Judge".to_string()),
                payload: Some(
                    serde_json::from_value(json!({ "title": "Graphs 101", "tags": ["learn"] }))
                        .unwrap(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    match progress {
        ServerProgress::Await { requests, .. } => match &requests[0].request {
            IoRequest::CallService { args, .. } => serde_json::to_value(args).unwrap(),
            other => panic!("expected a call, got {other:?}"),
        },
        ServerProgress::Done(response) => panic!("the call was never asked: {response:?}"),
    }
}

#[test]
fn call_service_arguments_resolve_their_bindings() {
    assert_eq!(
        asked_args(
            json!({ "text": "@payload.title", "nested": { "tags": "@payload.tags" }, "categories": ["learn", "laugh"] })
        ),
        json!({ "text": "Graphs 101", "nested": { "tags": ["learn"] }, "categories": ["learn", "laugh"] })
    );
}

#[test]
fn control_literal_call_service_arguments_pass_unchanged() {
    assert_eq!(
        asked_args(json!({ "text": "plain", "on": true })),
        json!({ "text": "plain", "on": true })
    );
}

// A step whose effects await a call commits its target state before the await: a dispatch that overlaps it sees the
// state the transition entered, and a dispatch that moves the trait meanwhile keeps that move once the call answers
// (G-CROSS-259, ruled 2026-10-18; JS host twin: `@almadar/runtime` `browser-host-midcall-state`, `browser-host-cancel-call`).
fn overlap_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Judge", "linkedEntity": "Job",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [
                        { "from": "idle", "to": "busy", "event": "CALL", "effects": [["call-service", "slow", "work", {}, { "key": "job", "emit": { "success": "DONE", "cancelled": "CANCELLED" } }]] },
                        { "from": "busy", "to": "busy", "event": "PING", "effects": [] },
                        { "from": "busy", "to": "idle", "event": "STOP", "effects": [["cancel-call", "job"]] },
                        { "from": "busy", "to": "idle", "event": "DONE", "effects": [] },
                        { "from": "busy", "to": "busy", "event": "CANCELLED", "effects": [] },
                        { "from": "idle", "to": "idle", "event": "CANCELLED", "effects": [] }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn judge(event: &str) -> EventRequest {
    request(event, "Judge")
}

/// Answer every ask of `progress` that is not a `call-service` (a cancel of another flight's call is acknowledged).
fn settle_other_asks(server: &EmbeddedServer, mut progress: ServerProgress) -> EventResponse {
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::CancelCall { .. } => IoAnswer::Cancelled(Ok(())),
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => panic!("unexpected ask {other:?}"),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn awaiting_call(
    progress: ServerProgress,
) -> (u64, Vec<orbital_core_runtime::resumable::PendingRequest>) {
    match progress {
        ServerProgress::Await { flight, requests } => (flight, requests),
        ServerProgress::Done(response) => panic!("the call never suspended: {response:?}"),
    }
}

#[test]
fn a_suspended_step_shows_its_target_state_to_an_overlapping_dispatch() {
    let server = EmbeddedServer::new(overlap_schema()).unwrap();
    let (_flight, _) = awaiting_call(server.start("Main", judge("CALL")).unwrap());
    assert_eq!(server.get_state("Judge", None), "busy");
    let ping = settle_other_asks(&server, server.start("Main", judge("PING")).unwrap());
    assert!(ping.transitioned, "PING has an arm only from busy");
}

#[test]
fn control_an_unmoved_call_finishes_from_the_state_it_entered() {
    let server = EmbeddedServer::new(overlap_schema()).unwrap();
    let (flight, requests) = awaiting_call(server.start("Main", judge("CALL")).unwrap());
    let replies = requests
        .into_iter()
        .map(|p| IoReply {
            id: p.id,
            answer: IoAnswer::Service(Ok(Value::Null)),
        })
        .collect();
    let done = settle_other_asks(&server, server.resume(flight, replies).unwrap());
    assert!(done.emitted_events.iter().any(|e| e.event == "DONE"));
    assert_eq!(server.get_state("Judge", None), "idle");
}

#[test]
fn a_call_result_after_another_dispatch_moved_the_trait_is_handled_from_the_moved_state() {
    let server = EmbeddedServer::new(overlap_schema()).unwrap();
    let (flight, requests) = awaiting_call(server.start("Main", judge("CALL")).unwrap());
    let stop = settle_other_asks(&server, server.start("Main", judge("STOP")).unwrap());
    assert!(stop.transitioned, "STOP has an arm only from busy");
    assert_eq!(server.get_state("Judge", None), "idle");
    let replies = requests
        .into_iter()
        .map(|p| IoReply {
            id: p.id,
            answer: IoAnswer::ServiceCancelled,
        })
        .collect();
    settle_other_asks(&server, server.resume(flight, replies).unwrap());
    assert_eq!(
        server.get_state("Judge", None),
        "idle",
        "the replayed CALL must not put the trait back to busy"
    );
    awaiting_call(server.start("Main", judge("CALL")).unwrap());
}
