// G-CROSS-335, EXEC-EVAL-10: an emit is checked once, where it is emitted, against its emitter's declaration of it; one
// that contradicts it refuses the dispatch. A listener's declared payload is a view it reads through, not a check.

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::kernel::{KernelError, RuntimeKernel};
use serde_json::{json, Value as Json};

fn kernel(sent: Json) -> RuntimeKernel {
    kernel_declaring(sent, json!([{ "event": "PING", "payloadSchema": [{ "name": "n", "type": "number" }] }]))
}

fn kernel_declaring(sent: Json, emits: Json) -> RuntimeKernel {
    kernel_with_types(sent, emits, json!({}))
}

fn kernel_with_types(sent: Json, emits: Json, types: Json) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main", "types": types,
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Sender", "linkedEntity": "Note", "emits": emits, "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", sent]] }]
                } },
                { "name": "Receiver", "linkedEntity": "Note",
                  "listens": [{ "event": "PING", "triggers": "RECEIVE", "source": { "kind": "trait", "trait": "Sender" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                    "events": [{ "key": "RECEIVE", "name": "Receive", "payloadSchema": [{ "name": "n", "type": "number" }] }],
                    "transitions": [{ "from": "idle", "to": "heard", "event": "RECEIVE" }]
                  } }
            ]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

#[test]
fn an_emit_that_contradicts_its_emitters_declaration_refuses_the_dispatch() {
    let refused = kernel(json!({ "n": "one" })).dispatch("Sender", None, "GO", Value::Null);
    assert!(
        matches!(&refused, Err(KernelError::PayloadInvalid(m)) if m.contains("Sender emitted") && m.contains("PING")),
        "{refused:?}"
    );
}

#[test]
fn an_emit_whose_payload_fails_to_evaluate_reports_that_failure_not_a_check_of_the_unevaluated_expression() {
    let outcomes = kernel(json!({ "n": ["+", 1, "x"] })).dispatch("Sender", None, "GO", Value::Null);
    let outcomes = outcomes.unwrap_or_else(|e| panic!("the dispatch must carry the evaluation failure, got {e:?}"));
    let failed = outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|er| er.error.as_deref().is_some_and(|e| e.contains("Type mismatch")));
    assert!(failed.is_some(), "{outcomes:?}");
}

#[test]
fn a_listeners_narrower_view_is_not_checked_at_delivery() {
    let outcomes = kernel_declaring(json!({ "n": "one" }), json!([])).dispatch("Sender", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(name, r)| name == "Receiver" && r.new_state == "heard"), "{outcomes:?}");
}

#[test]
fn control_a_delivery_that_fits_reaches_the_receiver() {
    let outcomes = kernel(json!({ "n": 1 }))
        .dispatch("Sender", None, "GO", Value::Null)
        .unwrap();
    assert!(
        outcomes
            .iter()
            .any(|(name, r)| name == "Receiver" && r.new_state == "heard"),
        "{outcomes:?}"
    );
}

fn envelope(id: &str, payload: Json) -> orbital_core_base::envelope::Envelope {
    use orbital_core_base::envelope::{Envelope, InstanceRef};
    Envelope {
        id: id.into(),
        from: InstanceRef {
            instance: "api".into(),
            orbital: "Remote".into(),
            trait_name: None,
        },
        to: InstanceRef {
            instance: "web".into(),
            orbital: "Main".into(),
            trait_name: Some("Receiver".into()),
        },
        event: "RECEIVE".into(),
        payload: serde_json::from_value(payload).unwrap(),
        causal_id: "c".into(),
        hop: 1,
    }
}

#[test]
fn an_inbound_envelope_whose_payload_contradicts_the_event_is_rejected_as_payload_invalid() {
    use orbital_core_base::envelope::{DeliverRefusal, DeliverRefusalKind, EnvelopeVerdict};
    let kernel = kernel(json!({ "n": 1 }));
    assert_eq!(
        kernel.dispatch_envelope(envelope("e1", json!({ "n": "one" }))),
        EnvelopeVerdict::Rejected(DeliverRefusal {
            kind: DeliverRefusalKind::PayloadInvalid,
            id: "e1".into()
        })
    );
    assert_ne!(
        kernel.snapshot_trait("Receiver", None).state,
        "heard",
        "the refused envelope ran nothing"
    );
    assert_eq!(
        kernel.dispatch_envelope(envelope("e1", json!({ "n": 1 }))),
        EnvelopeVerdict::Accepted,
        "control: a fitting payload under the refused id runs, the refusal was not journaled"
    );
    assert_eq!(kernel.snapshot_trait("Receiver", None).state, "heard");
}

fn qualified_badge_kernel(sent: Json) -> RuntimeKernel {
    kernel_with_types(
        sent,
        json!([{ "event": "PING", "payloadSchema": [{ "name": "b", "type": "Atom::Badge" }] }]),
        json!({ "Atom::Badge": [{ "name": "id", "type": "string", "required": true }] }),
    )
}

#[test]
fn an_emit_naming_a_qualified_type_is_checked_against_that_types_shape() {
    let refused = qualified_badge_kernel(json!({ "b": { "id": 1 } })).dispatch("Sender", None, "GO", Value::Null);
    assert!(matches!(&refused, Err(KernelError::PayloadInvalid(m)) if m.contains("PING")), "{refused:?}");
}

#[test]
fn control_an_emit_fitting_a_qualified_type_is_delivered() {
    let outcomes = qualified_badge_kernel(json!({ "b": { "id": "x" } })).dispatch("Sender", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(name, _)| name == "Sender"), "{outcomes:?}");
}
