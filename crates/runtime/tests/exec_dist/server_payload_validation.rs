// The API-boundary payload check is scoped to the traits a request dispatches to: the target when named,
// else the traits whose held state handles the event; each against its own declaration of the event.
// JS twin: `PayloadValidator.validateEventPayload` + `evaluateOrbitalEvent` step 3
// (`payload-validation-scope.test.ts`, `payload-validation-container.test.ts`).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use serde_json::{json, Value as Json};
use {
    orbital_core_base::schema::types::OrbitalSchema,
    orbital_core_runtime::effects::response::{EventRequest, EventResponse},
};

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
        let kernel = RuntimeKernel::new(
            environment,
            Arc::new(InMemoryEntityCache::new()),
            Default::default(),
        );
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
        let null = kernel.dispatch(
            "InlineRender",
            None,
            "SEND",
            payload(json!({ "value": null })),
        );
        assert!(
            matches!(&null, Err(KernelError::PayloadInvalid(m)) if m.contains("value (null, expected string)")),
            "{null:?}"
        );
    }

    #[test]
    fn control_a_present_field_an_optional_one_and_a_client_kernel_dispatch_as_before() {
        let server = kernel(RuntimeEnvironment::Server);
        assert!(server
            .dispatch(
                "InlineRender",
                None,
                "SEND",
                payload(json!({ "value": "hi" }))
            )
            .is_ok());
        assert!(server
            .dispatch("ChatComposer", None, "SEND", payload(json!({})))
            .is_ok());
        let client = kernel(RuntimeEnvironment::Client);
        assert!(client
            .dispatch("InlineRender", None, "SEND", payload(json!({})))
            .is_ok());
    }
}

#[test]
fn dispatch_checks_orbital_aliases_and_entity_rows_through_named_definitions() {
    use orbital_core_base::effect_types::RuntimeEnvironment;
    use orbital_core_runtime::{entity_source::InMemoryEntityCache, kernel::RuntimeKernel};
    use std::sync::Arc;
    let mut program = serde_json::to_value(schema()).unwrap();
    let orbital = &mut program["orbitals"][0];
    orbital["types"] =
        json!({"Envelope": [{"name": "rows", "type": "[Message]", "required": true}]});
    orbital["traits"][0]["stateMachine"]["events"][0]["payloadSchema"] = json!([
        {"name": "data", "type": "Envelope", "required": true}
    ]);
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()),
        Default::default(),
    );
    kernel
        .register_schema(serde_json::from_value(program).unwrap())
        .unwrap();
    let bad = kernel.payload_rejection(
        [("ChatComposer", "SEND")],
        &serde_json::from_value(json!({"data": {"rows": [{"id": 42}]}})).unwrap(),
    );
    assert!(
        bad.as_deref()
            .is_some_and(|error| error.contains("data.rows[0].id")),
        "{bad:?}"
    );
    assert!(kernel
        .payload_rejection(
            [("ChatComposer", "SEND")],
            &serde_json::from_value(json!({"data": {"rows": [{"id": "ok"}]}})).unwrap()
        )
        .is_none());
}

mod native_record_event_admission {
    use super::*;
    use orbital_core_base::effect_types::RuntimeEnvironment;
    use orbital_core_base::value::Value;
    use orbital_core_runtime::entity_source::InMemoryEntityCache;
    use orbital_core_runtime::kernel::{KernelError, RuntimeKernel};
    use std::sync::Arc;

    fn program(version: Option<u32>, field: Json) -> Json {
        let mut program = serde_json::to_value(schema()).unwrap();
        if let Some(version) = version {
            program["typeContractVersion"] = json!(version);
        }
        program["orbitals"][0]["traits"][0]["stateMachine"]["events"][0]["payloadSchema"] =
            json!([field]);
        program
    }

    fn kernel(program: Json) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()),
            Default::default(),
        );
        kernel
            .register_schema(serde_json::from_value(program).unwrap())
            .unwrap();
        kernel
    }

    fn dispatch(kernel: &RuntimeKernel, payload: Json, accepted: bool) {
        let payload: Value = serde_json::from_value(payload).unwrap();
        let outcome = kernel.dispatch("ChatComposer", None, "SEND", payload);
        if accepted {
            let outcome = outcome.expect("a contract-admitted payload executes");
            assert!(outcome.iter().any(|(_, result)| result.executed));
        } else {
            assert!(
                matches!(&outcome, Err(KernelError::PayloadInvalid(_))),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn native_record_event_admission_separates_presence_from_null_for_all_four_forms() {
        for (required, nullable) in [(false, false), (true, false), (false, true), (true, true)] {
            let kernel = kernel(program(
                Some(1),
                json!({
                    "name": "value", "type": "string", "required": required, "nullable": nullable
                }),
            ));
            dispatch(&kernel, json!({}), !required);
            dispatch(&kernel, json!({"value": null}), nullable);
            dispatch(&kernel, json!({"value": "ok"}), true);
            dispatch(&kernel, json!({"value": 42}), false);
        }
    }

    #[test]
    fn native_record_event_admission_legacy_programs_keep_their_null_contract() {
        for (required, nullable) in [(false, false), (true, false), (false, true), (true, true)] {
            let kernel = kernel(program(
                None,
                json!({
                    "name": "value", "type": "string", "required": required, "nullable": nullable
                }),
            ));
            dispatch(&kernel, json!({}), !required);
            dispatch(&kernel, json!({"value": null}), !required);
            dispatch(&kernel, json!({"value": "ok"}), true);
        }
    }

    #[test]
    fn native_record_event_admission_checks_root_aliases_and_nested_values() {
        let mut program = program(
            Some(1),
            json!({
                "name": "data", "type": "Envelope", "required": true, "nullable": false
            }),
        );
        program["types"] = json!({"Envelope": [{
            "name": "nested", "type": "object", "required": true, "nullable": false,
            "properties": [{"name": "value", "type": "string", "required": true, "nullable": false}]
        }]});
        let kernel = kernel(program);
        dispatch(&kernel, json!({"data": {"nested": {"value": "ok"}}}), true);
        let bad: Value =
            serde_json::from_value(json!({"data": {"nested": {"value": 42}}})).unwrap();
        let rejection = kernel.payload_rejection([("ChatComposer", "SEND")], &bad);
        assert!(
            rejection
                .as_deref()
                .is_some_and(|message| message.contains("data.nested.value")),
            "{rejection:?}"
        );
        dispatch(&kernel, json!({"data": {"nested": {"value": 42}}}), false);
        dispatch(&kernel, json!({"data": {"nested": {"value": null}}}), false);
        dispatch(&kernel, json!({"data": {"nested": {}}}), false);
    }

    #[test]
    fn native_record_event_admission_local_aliases_override_root_aliases() {
        let mut program = program(
            Some(1),
            json!({
                "name": "data", "type": "Envelope", "required": true, "nullable": false
            }),
        );
        program["types"] = json!({"Envelope": [{
            "name": "value", "type": "string", "required": true, "nullable": false
        }]});
        program["orbitals"][0]["types"] = json!({"Envelope": [{
            "name": "value", "type": "number", "required": true, "nullable": false
        }]});
        let kernel = kernel(program);
        dispatch(&kernel, json!({"data": {"value": 42}}), true);
        dispatch(&kernel, json!({"data": {"value": "root-only"}}), false);
    }
}
