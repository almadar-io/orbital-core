// The server role stamps each emitted event with its emitter's identity (orbital, the ids the schema
// carries, the emit contract's id), so a client matches `listens` sources the way the JS host's stamp lets it.
// JS twin: `stampEmitSource` (`almadar-runtime/src/events/emit-stamp.ts`).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::json;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "id": "orb_A", "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "id": "trt_A", "name": "Emitter", "linkedEntity": "Item",
                "emits": [{ "event": "PING", "eventId": "evt_PING" }, { "event": "PONG" }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["emit", "PING", {}], ["emit", "PONG", {}]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn go() -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Emitter".to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| orbital_core_runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core_runtime::resumable::IoAnswer::Row(Err(
                            "no row".into()
                        )),
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

#[test]
fn an_emit_carries_its_emitters_orbital_and_ids() {
    let response = go();
    let source = response.emitted_events[0].source.as_ref().unwrap();
    assert_eq!(source.r#trait.as_deref(), Some("Emitter"));
    assert_eq!(source.orbital.as_deref(), Some("Main"));
    assert_eq!(source.orbital_id.as_deref(), Some("orb_A"));
    assert_eq!(source.trait_id.as_deref(), Some("trt_A"));
}

#[test]
fn an_emit_carries_the_id_of_its_contract_only_when_it_declares_one() {
    let response = go();
    assert_eq!(
        response.emitted_events[0]
            .source
            .as_ref()
            .unwrap()
            .event_id
            .as_deref(),
        Some("evt_PING")
    );
    assert_eq!(
        response.emitted_events[1].source.as_ref().unwrap().event_id,
        None
    );
}


#[test]
fn the_embedded_stream_preserves_effect_order_and_does_not_replay_the_previous_dispatch() {
    let mut program = serde_json::to_value(schema()).unwrap();
    program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"] = json!([
        ["emit", "PING", {}], ["navigate", "/next"], ["emit", "PONG", {}]
    ]);
    let server = EmbeddedServer::new(serde_json::from_value(program).unwrap()).unwrap();
    let mut progress: serde_json::Value = serde_json::from_str(&server.start_json("Main", &json!({ "event": "GO", "targetTrait": "Emitter" }).to_string()).unwrap()).unwrap();
    while progress["status"] == "await" {
        let replies: Vec<serde_json::Value> = progress["requests"].as_array().unwrap().iter().map(|request| json!({
            "id": request["id"], "answer": { "kind": "row", "result": { "error": "no row" } }
        })).collect();
        progress = serde_json::from_str(&server.resume_json(progress["flight"].as_u64().unwrap(), &serde_json::to_string(&replies).unwrap()).unwrap()).unwrap();
    }
    let items: serde_json::Value = serde_json::from_str(&server.dispatch_stream_json().unwrap()).unwrap();
    let types: Vec<&str> = items.as_array().unwrap().iter().map(|item| item["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["event", "effect", "event"], "the response crosses once, in the dispatch's own answer: the stream carries no complete");
    assert_eq!(items[0]["data"]["event"], "PING");
    assert_eq!(items[1]["data"], json!(["navigate", "/next"]));
    assert_eq!(items[2]["data"]["event"], "PONG");
    server.start_json("Main", &json!({ "event": "MISSING", "traits": [] }).to_string()).unwrap();
    let control: serde_json::Value = serde_json::from_str(&server.dispatch_stream_json().unwrap()).unwrap();
    assert!(control.as_array().unwrap().is_empty(), "control: a dispatch with no emit or client effect leaves no stream item and clones no response: {control}");
}

#[test]
fn substrate_completion_export_uses_the_native_result_and_failure_envelopes() {
    use orbital_core_runtime::embed::substrate_completion_event_json;
    let site = json!({ "trait": "Worker", "followOn": true, "emit": { "success": "DONE", "failure": "FAILED" } }).to_string();
    let response = json!({ "success": true, "transitioned": true, "states": {}, "emittedEvents": [{ "event": "SAID", "payload": { "n": 1 } }] });
    let request = json!(["O.Worker.GO", {}, { "orbital": "O", "event": "GO", "viewer": null }]).to_string();
    let success: serde_json::Value = serde_json::from_str(&substrate_completion_event_json(&site, "job", "program/dispatch", &request, &json!({ "ok": response }).to_string()).unwrap()).unwrap();
    assert_eq!(success["payload"]["result"], json!({ "ok": true, "emitted": [{ "event": "SAID", "payload": { "n": 1.0 } }] }), "a deferred event request answers the dispatch outcome, as in place: {success}");
    let wire = json!(["O.Worker.GO", {}, { "orbital": "O", "event": "GO", "wire": true }]).to_string();
    let raw: serde_json::Value = serde_json::from_str(&substrate_completion_event_json(&site, "job", "program/dispatch", &wire, &json!({ "ok": response }).to_string()).unwrap()).unwrap();
    assert_eq!(raw["payload"]["result"]["transitioned"], json!(true), "control: a wire request answers the whole response: {raw}");
    let failure: serde_json::Value = serde_json::from_str(&substrate_completion_event_json(&site, "job", "program/dispatch", &request, &json!({ "error": { "message": "missing", "errors": [{ "code": "NOT_FOUND", "status": 404 }] } }).to_string()).unwrap()).unwrap();
    assert_eq!((&failure["event"], &failure["payload"]["error"], &failure["payload"]["key"]), (&json!("FAILED"), &json!("missing"), &json!("job")), "{failure}");
    assert_eq!((failure["payload"]["errors"][0]["code"].as_str(), failure["payload"]["errors"][0]["status"].as_f64()), (Some("NOT_FOUND"), Some(404.0)), "{failure}");
    let read: serde_json::Value = serde_json::from_str(&substrate_completion_event_json(&site, "job", "program/read", "[\"x\"]", &json!({ "ok": { "answer": 42 } }).to_string()).unwrap()).unwrap();
    assert_eq!((&read["event"], read["payload"]["result"]["answer"].as_f64(), &read["payload"]["key"]), (&json!("DONE"), Some(42.0), &json!("job")), "control: another op answers what the host answered: {read}");
    let inert_site = json!({ "trait": "Worker", "followOn": true, "emit": {} }).to_string();
    assert_eq!(substrate_completion_event_json(&inert_site, "job", "program/read", "[]", "{\"ok\":1}").unwrap(), "null");
}
