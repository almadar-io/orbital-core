// The API-boundary payload check is scoped to the traits a request dispatches to: the target when named,
// else the traits whose held state handles the event; each against its own declaration of the event.
// JS twin: `PayloadValidator.validateEventPayload` + `evaluateOrbitalEvent` step 3
// (`payload-validation-scope.test.ts`, `payload-validation-container.test.ts`).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::{json, Value as Json};

fn schema() -> OrbitalSchema {
    let trait_with = |name: &str, field: Json, arms: Json| {
        json!({
            "name": name, "linkedEntity": "Message",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "events": [{ "key": "SEND", "name": "Send", "payloadSchema": [field] }],
                "transitions": arms
            }
        })
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                trait_with("ChatComposer", json!({ "name": "value", "type": "string", "required": false }),
                    json!([{ "from": "idle", "to": "idle", "event": "SEND" }])),
                trait_with("InlineRender", json!({ "name": "value", "type": "string", "required": true }), json!([])),
                trait_with("Burst", json!({ "name": "particles", "type": "[object]" }),
                    json!([{ "from": "idle", "to": "idle", "event": "SEND" }]))
            ]
        }]
    }))
    .unwrap()
}

fn send(target: Option<&str>, payload: Json) -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "SEND".to_string(),
        target_trait: target.map(str::to_string),
        payload: Some(serde_json::from_value(payload).unwrap()),
        ..Default::default()
    };
    let mut progress = server.start("ChatOrbital", request).unwrap();
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

#[test]
fn a_dispatch_targeted_at_the_optional_trait_ignores_a_siblings_required_field() {
    assert!(send(Some("ChatComposer"), json!({})).success);
}

#[test]
fn the_same_dispatch_targeted_at_the_required_trait_is_rejected() {
    let response = send(Some("InlineRender"), json!({}));
    assert!(!response.success);
    assert!(!response.transitioned);
    assert_eq!(
        response.error.as_deref(),
        Some("Payload validation failed for event 'SEND': value (missing, expected string)")
    );
}

#[test]
fn a_null_required_field_is_reported_as_null() {
    let response = send(Some("InlineRender"), json!({ "value": null }));
    assert_eq!(
        response.error.as_deref(),
        Some("Payload validation failed for event 'SEND': value (null, expected string)")
    );
}

#[test]
fn control_the_required_field_present_passes() {
    assert!(send(Some("InlineRender"), json!({ "value": "hi" })).success);
}

#[test]
fn an_untargeted_dispatch_validates_only_against_the_traits_that_handle_the_event() {
    let response = send(None, json!({ "particles": [] }));
    assert!(response.success, "{:?}", response.error);
}

#[test]
fn a_declared_container_is_enforced() {
    let rejected = send(Some("Burst"), json!({ "particles": { "id": "p" } }));
    assert!(!rejected.success);
    assert_eq!(
        rejected.error.as_deref(),
        Some(
            "Payload validation failed for event 'SEND': particles (wrong-type, expected [object])"
        )
    );
    assert!(send(Some("Burst"), json!({ "particles": [{ "id": "p" }] })).success);
    assert!(
        send(Some("Burst"), json!({})).success,
        "an optional field may be absent"
    );
}

mod kernel_dispatch {
    use super::*;
    use orbital_core_base::effect_types::RuntimeEnvironment;
    use orbital_core_base::value::Value;
    use orbital_core_runtime::entity_source::InMemoryEntityCache;
    use orbital_core_runtime::kernel::{KernelError, RuntimeKernel};
    use std::sync::Arc;

    fn kernel(environment: RuntimeEnvironment) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(environment, Arc::new(InMemoryEntityCache::new()), Default::default());
        kernel.register_schema(schema()).unwrap();
        kernel
    }

    fn payload(value: Json) -> Value {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_server_kernel_refuses_a_missing_required_field_before_any_transition() {
        let kernel = kernel(RuntimeEnvironment::Server);
        let refused = kernel.dispatch("InlineRender", None, "SEND", payload(json!({})));
        assert!(
            matches!(&refused, Err(KernelError::PayloadInvalid(m)) if m == "Payload validation failed for event 'SEND': value (missing, expected string)"),
            "{refused:?}"
        );
        let null = kernel.dispatch("InlineRender", None, "SEND", payload(json!({ "value": null })));
        assert!(matches!(&null, Err(KernelError::PayloadInvalid(m)) if m.contains("value (null, expected string)")), "{null:?}");
    }

    #[test]
    fn control_a_present_field_an_optional_one_and_a_client_kernel_dispatch_as_before() {
        let server = kernel(RuntimeEnvironment::Server);
        assert!(server.dispatch("InlineRender", None, "SEND", payload(json!({ "value": "hi" }))).is_ok());
        assert!(server.dispatch("ChatComposer", None, "SEND", payload(json!({}))).is_ok());
        let client = kernel(RuntimeEnvironment::Client);
        assert!(client.dispatch("InlineRender", None, "SEND", payload(json!({}))).is_ok());
    }
}
