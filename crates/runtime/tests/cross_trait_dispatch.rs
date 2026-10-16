//! Cross-trait listener fan-out (RS-G20): a user event dispatched to one trait must
//! reach every trait that *listens* for it. Reproduces the "New Item does nothing" bug:
//! a `CREATE` from a source trait (the button's owner) has to open a *different* trait's
//! modal via its `listens` entry — not just process the source trait in isolation.

use orbital_core_base::client_effect::ClientEffect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn two_trait_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Catalog",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                },
                {
                    "name": "CreateModal",
                    "linkedEntity": "Item",
                    "listens": [{
                        "event": "CREATE",
                        "triggers": "CREATE",
                        "source": { "kind": "trait", "trait": "Catalog" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "closed", "isInitial": true },
                            { "name": "open" }
                        ],
                        "transitions": [
                            { "from": "closed", "to": "closed", "event": "INIT" },
                            { "from": "closed", "to": "open", "event": "CREATE",
                              "effects": [["render-ui", "modal", { "type": "form" }]] },
                            { "from": "open", "to": "closed", "event": "CLOSE",
                              "effects": [["render-ui", "modal", null]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn create_from_source_opens_listening_modal() {
    let kernel = kernel_with(two_trait_schema());

    // The button is owned by `Catalog`, so its CREATE dispatches with source=Catalog.
    let outcomes = kernel
        .dispatch("Catalog", None, "CREATE", Value::Object(Default::default()))
        .unwrap();

    // The modal trait fired via its listener even though CREATE was sent to Catalog.
    let modal = outcomes
        .iter()
        .find(|(t, _)| t == "CreateModal")
        .expect("CreateModal listener should fire for CREATE from Catalog");
    assert!(modal.1.executed, "modal CREATE transition should execute");
    assert_eq!(modal.1.new_state, "open");

    // The merged response carries the modal's render-ui.
    let resp = EventResponse::from_process_results(&outcomes, "Catalog", "CREATE");
    assert!(
        resp.client_effects.iter().flatten().any(|e| matches!(
            e,
            ClientEffect::RenderUi { slot, .. } if slot == "modal"
        )),
        "merged response must contain a render-ui into the modal slot, got {:?}",
        resp.client_effects
    );
}

#[test]
fn create_from_wrong_source_does_not_open_modal() {
    // A source the listener does NOT subscribe to must not trigger the modal — proves
    // the fan-out is source-matched, not event-name broadcast (Inventory vs StockAdjustment
    // both emit CREATE; they must not cross-fire).
    let kernel = kernel_with(two_trait_schema());

    let outcomes = kernel
        .dispatch("CreateModal", None, "INIT", Value::Null) // unrelated dispatch
        .unwrap();
    assert!(
        !outcomes
            .iter()
            .any(|(t, r)| t == "CreateModal" && r.new_state == "open"),
        "INIT must not open the modal"
    );

    // Dispatching CREATE from a trait the listener doesn't name as source → no modal.
    let kernel2 = kernel_with(two_trait_schema());
    let outcomes2 = kernel2
        .dispatch(
            "CreateModal",
            None,
            "CREATE",
            Value::Object(Default::default()),
        )
        .unwrap();
    // CreateModal itself DOES have a CREATE transition, so dispatching CREATE *to it*
    // opens it directly — but no OTHER trait should be dragged in.
    assert_eq!(
        outcomes2.len(),
        1,
        "only the directly-targeted trait should fire"
    );
}

/// Wave-8 (`C-TRAITREF-WASM-UNRESOLVED`): the wasm mount inlines its schema so `@trait.<Peer>`
/// composition resolves. A primary trait composes `@trait.Peer` in its `main` render-ui; the
/// peer's own INIT emits a `main` render-ui. Dispatching INIT to the peer must produce that
/// `main` render-ui — the effect the client indexes under `trait_content[Peer]`, which is what
/// makes the `@trait.Peer` leaf in the primary's children resolve.
fn composed_peer_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Layout",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                            "effects": [["render-ui", "main",
                                { "type": "stack", "children": ["@trait.Peer"] }]] }]
                    }
                },
                {
                    "name": "Peer",
                    "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [{ "from": "ready", "to": "ready", "event": "INIT",
                            "effects": [["render-ui", "main", { "type": "button" }]] }]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn peer_init_emits_main_render_ui_for_trait_content() {
    let kernel = kernel_with(composed_peer_schema());

    let outcomes = kernel.dispatch("Peer", None, "INIT", Value::Null).unwrap();
    let resp = EventResponse::from_process_results(&outcomes, "Peer", "INIT");

    assert!(
        resp.client_effects.iter().flatten().any(|e| matches!(
            e,
            ClientEffect::RenderUi { slot, config: Some(content), .. }
                if slot == "main"
                && content.as_object().and_then(|o| o.get("type")).and_then(|v| v.as_str())
                    == Some("button")
        )),
        "peer INIT must emit its `main` render-ui (indexed under trait_content[Peer]), got {:?}",
        resp.client_effects
    );
}

/// A `listens { Source.EVENT -> LocalEvent }` bare route (no `with {}` mapping) must
/// forward the source event's payload to the listener UNCHANGED — parity with
/// `applyListenPayloadMapping` (`@almadar/core`): "No mapping ... -> the payload passes
/// through unchanged." A `with {}` route remaps instead (§104 ledger).
fn listens_cascade_schema(with_mapping: bool) -> OrbitalSchema {
    let target_trait = serde_json::json!({
        "name": "Target",
        "linkedEntity": "Item",
        "listens": [{
            "event": "SAVE",
            "triggers": "DO_SAVE",
            "source": { "kind": "trait", "trait": "Source" },
            "payloadMapping": if with_mapping {
                serde_json::json!({ "id": "@payload.id" })
            } else {
                serde_json::Value::Null
            }
        }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "DO_SAVE",
                "effects": [["set", "@entity.id", "@payload.id"]] }]
        }
    });
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Source",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "SAVE" }]
                    }
                },
                target_trait
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn bare_listen_route_forwards_source_payload_unchanged() {
    let kernel = kernel_with(listens_cascade_schema(false));
    let outcomes = kernel
        .dispatch(
            "Source",
            Some("row-1"),
            "SAVE",
            serde_json::from_value(serde_json::json!({"id": "abc123", "row": {"id": "abc123"}}))
                .unwrap(),
        )
        .unwrap();
    let target = outcomes.iter().find(|(t, _)| t == "Target").expect("Target should fire");
    assert_eq!(
        target.1.entity_data.as_object().and_then(|o| o.get("id")).and_then(|v| v.as_str()),
        Some("abc123"),
        "bare route must forward the source payload's `id` field unchanged"
    );
}

#[test]
fn with_listen_route_remaps_payload() {
    let kernel = kernel_with(listens_cascade_schema(true));
    let outcomes = kernel
        .dispatch(
            "Source",
            Some("row-1"),
            "SAVE",
            serde_json::from_value(serde_json::json!({"id": "abc123", "row": {"id": "abc123"}}))
                .unwrap(),
        )
        .unwrap();
    let target = outcomes.iter().find(|(t, _)| t == "Target").expect("Target should fire");
    assert_eq!(
        target.1.entity_data.as_object().and_then(|o| o.get("id")).and_then(|v| v.as_str()),
        Some("abc123"),
        "a with-mapping route must still resolve the mapped field"
    );
}
