// EXEC-DIST-16b: a browser-bound answer carries each SExpr-typed emit payload value as a framed blob, and decodes to the
// plain answer exactly.

use orbital_core_base::blobs::{unframe, BLOB_MARKER};
use orbital_core_base::json_value::{from_json_value, js_json_string};
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use serde_json::{json, Value as Json};

fn server() -> EmbeddedServer {
    EmbeddedServer::new(from_json_value(&json!({
        "name": "App", "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Opener", "linkedEntity": "Item",
                "emits": [
                    { "event": "CLIENT", "payloadSchema": [
                        { "name": "guest", "type": "string", "required": true },
                        { "name": "program", "type": "SExpr", "required": true },
                        { "name": "header", "type": "object", "properties": [{ "name": "name", "type": "string" }] }] },
                    { "event": "NOTE", "payloadSchema": [{ "name": "text", "type": "string" }] }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [
                        ["emit", "CLIENT", { "guest": "g", "program": { "name": "guest", "orbitals": [{ "name": "نص" }] }, "header": { "name": "guest" } }],
                        ["emit", "NOTE", { "text": "hi" }]] }]
                }
            }]
        }]
    }))
    .unwrap())
    .unwrap()
}

fn go(server: &EmbeddedServer) -> EventResponse {
    let request = EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Opener".to_string()),
        ..Default::default()
    };
    match server.start("Main", request).unwrap() {
        ServerProgress::Done(response) => response,
        _ => panic!("GO asks nothing"),
    }
}

/// The framed body's head with every blob put back in place of its marker.
fn decoded(body: &[u8]) -> (Json, usize) {
    let (head, blobs) = unframe(body).unwrap();
    let mut head: Json = serde_json::from_slice(head).unwrap();
    fn put_back(value: &mut Json, blobs: &[&[u8]]) {
        match value {
            Json::Object(fields) if fields.len() == 1 && fields.contains_key(BLOB_MARKER) => {
                let index = fields[BLOB_MARKER].as_u64().unwrap() as usize;
                *value = serde_json::from_slice(blobs[index]).unwrap();
            }
            Json::Object(fields) => fields.values_mut().for_each(|v| put_back(v, blobs)),
            Json::Array(items) => items.iter_mut().for_each(|v| put_back(v, blobs)),
            _ => {}
        }
    }
    put_back(&mut head, &blobs);
    (head, blobs.len())
}

#[test]
fn an_sexpr_typed_emit_payload_value_rides_as_a_blob() {
    let server = server();
    let response = go(&server);
    let body = server.kernel().framed_answer(&response).unwrap().1;
    let (head, blobs) = unframe(&body).unwrap();
    let head: Json = serde_json::from_slice(head).unwrap();
    let client = head["emittedEvents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "CLIENT")
        .unwrap();
    assert_eq!(client["payload"]["program"], json!({ (BLOB_MARKER): 0 }));
    assert_eq!(blobs.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Json>(blobs[0]).unwrap(),
        json!({ "name": "guest", "orbitals": [{ "name": "نص" }] })
    );
    assert_eq!(
        client["payload"]["header"],
        json!({ "name": "guest" }),
        "control: an object-typed field stays inline"
    );
    assert_eq!(client["payload"]["guest"], "g");
}

#[test]
fn a_framed_answer_decodes_to_the_plain_answer() {
    let server = server();
    let response = go(&server);
    let plain: Json = serde_json::from_str(&js_json_string(&response)).unwrap();
    let (decoded, blobs) = decoded(&server.kernel().framed_answer(&response).unwrap().1);
    assert_eq!(blobs, 1);
    assert_eq!(decoded, plain);
}

#[test]
fn control_an_answer_with_no_sexpr_typed_value_is_framed_with_no_blobs() {
    let server = server();
    let mut response = go(&server);
    response.emitted_events.retain(|e| e.event == "NOTE");
    let plain: Json = serde_json::from_str(&js_json_string(&response)).unwrap();
    let (decoded, blobs) = decoded(&server.kernel().framed_answer(&response).unwrap().1);
    assert_eq!(blobs, 0);
    assert_eq!(decoded, plain);
}
