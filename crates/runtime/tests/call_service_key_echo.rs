// A keyed `call-service` echoes its key beside the record it answers with (D-A82); an unkeyed one is unchanged.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn schema(options: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Helper", "linkedEntity": "Job",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }],
                    "transitions": [{ "from": "idle", "to": "asking", "event": "ASK",
                        "effects": [["call-service", "svc", "work", {}, options]] }]
                } }]
        }]
    }))
    .unwrap()
}

fn run(options: Json, answer: IoAnswer) -> EventResponse {
    let server = EmbeddedServer::new(schema(options)).unwrap();
    let request = EventRequest {
        event: "ASK".to_string(),
        target_trait: Some("Helper".to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: match p.request {
                            IoRequest::CallService { .. } => answer.clone(),
                            _ => IoAnswer::Row(Err("no row".to_string())),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn payload<'a>(response: &'a EventResponse, event: &str) -> &'a Value {
    response
        .emitted_events
        .iter()
        .find(|e| e.event == event)
        .and_then(|e| e.payload.as_ref())
        .expect("the event fired with a payload")
}

fn routes(keyed: bool) -> Json {
    let emit = json!({ "success": "OK", "failure": "BAD" });
    if keyed {
        json!({ "key": "job", "emit": emit })
    } else {
        json!({ "emit": emit })
    }
}

fn ok() -> IoAnswer {
    IoAnswer::Service(Ok(serde_json::from_value(json!({ "answer": 42 })).unwrap()))
}

#[test]
fn a_keyed_success_carries_the_key_beside_the_result_fields() {
    let response = run(routes(true), ok());
    let p = payload(&response, "OK");
    assert_eq!(p.get_path("key").and_then(Value::as_str), Some("job"));
    assert_eq!(p.get_path("answer").and_then(Value::as_number), Some(42.0));
}

#[test]
fn a_call_service_key_expression_is_evaluated_before_dispatch() {
    let mut options = routes(true);
    options["key"] = json!(["str/concat", "j", "ob"]);
    let response = run(options, ok());
    assert_eq!(payload(&response, "OK").get_path("key").and_then(Value::as_str), Some("job"));
}

#[test]
fn an_unkeyed_success_is_unchanged() {
    let response = run(routes(false), ok());
    assert!(payload(&response, "OK").get_path("key").is_none());
}

#[test]
fn a_keyed_failure_carries_its_error_and_key() {
    let response = run(routes(true), IoAnswer::Service(Err("down".to_string())));
    let p = payload(&response, "BAD");
    assert_eq!(p.get_path("error").and_then(Value::as_str), Some("down"));
    assert_eq!(p.get_path("key").and_then(Value::as_str), Some("job"));
}

#[test]
fn an_unkeyed_failure_has_no_key() {
    let response = run(routes(false), IoAnswer::Service(Err("down".to_string())));
    assert!(payload(&response, "BAD").get_path("key").is_none());
}

#[test]
fn a_keyed_result_that_is_not_a_record_is_left_unchanged() {
    let response = run(
        routes(true),
        IoAnswer::Service(Ok(serde_json::from_value(json!(7)).unwrap())),
    );
    assert_eq!(payload(&response, "OK").as_number(), Some(7.0));
}
