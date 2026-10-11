//! Browser-stored entities: `[persistent: x, local]` rows live in the client, so a
//! data effect (`fetch`/`persist`) on them runs on the client and a trait whose
//! data is all client-resident never touches the server. Twin of
//! `@almadar/std` `effectSiteFor` + `@almadar/core` `isClientResident`.

use orbital_core::effects::{effect_site, sexpr_runs_server_effect, Residence, RunsOn, ENTITY_ARGS};
use orbital_core::runtime::dispatch_mode::DispatchMode;
use orbital_core::schema::{EntityDefinition, TraitDefinition};
use serde_json::json;

fn entity(value: serde_json::Value) -> EntityDefinition {
    serde_json::from_value(value).expect("entity")
}

fn residence() -> Residence {
    Residence::of_entities([
        &entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "local": true, "fields": [] })),
        &entity(json!({ "name": "Scene", "persistence": "runtime", "fields": [] })),
        &entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] })),
    ])
}

fn sexpr(value: serde_json::Value) -> orbital_core::schema::SExpression {
    serde_json::from_value(value).expect("sexpr")
}

#[test]
fn local_and_seed_mock_round_trip_through_serde() {
    let e = entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "local": true, "seedMock": true, "fields": [] }));
    assert!(e.local && e.seed_mock);
    let back = serde_json::to_value(&e).unwrap();
    assert_eq!(back["local"], json!(true));
    assert_eq!(back["seedMock"], json!(true));
}

#[test]
fn control_an_entity_without_the_flags_serializes_without_them() {
    let e = entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] }));
    let back = serde_json::to_value(&e).unwrap();
    assert!(back.get("local").is_none() && back.get("seedMock").is_none(), "{back}");
}

#[test]
fn only_local_entities_store_rows_in_the_browser() {
    let r = residence();
    assert!(r.is_client("LocalInvoice"));
    assert!(!r.is_client("Invoice"));
    assert!(!r.is_client("Unknown"));
}

#[test]
fn control_a_runtime_entity_keeps_its_rows_on_the_server() {
    // Its frame is client-held (dispatch), its rows are the server's per-session store.
    assert!(!residence().is_client("Scene"));
    let scene = entity(json!({ "name": "Scene", "persistence": "runtime", "fields": [] }));
    assert!(scene.is_client_resident());
    assert_eq!(effect_site("fetch", &[sexpr(json!("Scene"))], &residence()), RunsOn::Server);
}

#[test]
fn the_entity_argument_table_matches_the_registry_positions() {
    assert_eq!(ENTITY_ARGS, &[("fetch", 0usize), ("persist", 1usize)]);
}

#[test]
fn a_data_effect_runs_where_its_entity_lives() {
    let r = residence();
    assert_eq!(effect_site("fetch", &[sexpr(json!("LocalInvoice"))], &r), RunsOn::Client);
    assert_eq!(effect_site("persist", &[sexpr(json!("create")), sexpr(json!("LocalInvoice"))], &r), RunsOn::Client);
    assert_eq!(effect_site("fetch", &[sexpr(json!("Invoice"))], &r), RunsOn::Server);
    assert_eq!(effect_site("fetch", &[sexpr(json!("@config.entity"))], &r), RunsOn::Server);
    assert_eq!(effect_site("call-service", &[sexpr(json!("llm"))], &r), RunsOn::Server);
    assert_eq!(effect_site("emit", &[sexpr(json!("DONE"))], &r), RunsOn::Any);
}

#[test]
fn the_server_effect_predicate_follows_residence() {
    let r = residence();
    assert!(!sexpr_runs_server_effect(&sexpr(json!(["fetch", "LocalInvoice", {}])), &r));
    assert!(sexpr_runs_server_effect(&sexpr(json!(["fetch", "Invoice", {}])), &r));
    assert!(sexpr_runs_server_effect(&sexpr(json!(["do", ["persist", "create", "LocalInvoice", {}], ["call-service", "x", "y", {}]])), &r));
    // Control: with no client-resident entities declared, every data effect is server work.
    assert!(sexpr_runs_server_effect(&sexpr(json!(["fetch", "LocalInvoice", {}])), &Residence::none()));
}

fn trait_on(entity: &str, local: bool, effects: serde_json::Value) -> TraitDefinition {
    serde_json::from_value(json!({
        "name": "T", "linkedEntity": entity, "local": local,
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] }
    }))
    .expect("trait")
}

#[test]
fn a_trait_on_a_local_entity_fetching_it_is_client_only() {
    let r = residence();
    let e = entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "local": true, "fields": [] }));
    let t = trait_on("LocalInvoice", false, json!([["fetch", "LocalInvoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&e), &r), DispatchMode::HybridClientOnly);
}

#[test]
fn control_a_trait_on_a_server_entity_fetching_it_awaits_the_server() {
    let r = residence();
    let e = entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] }));
    let t = trait_on("Invoice", false, json!([["fetch", "Invoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&e), &r), DispatchMode::PersistedAwaited);
}

#[test]
fn control_a_runtime_trait_fetching_a_server_entity_stays_optimistic() {
    let r = residence();
    let e = entity(json!({ "name": "Scene", "persistence": "runtime", "fields": [] }));
    let t = trait_on("Scene", false, json!([["fetch", "Invoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&e), &r), DispatchMode::RuntimeOptimistic);
}

#[test]
fn a_local_entity_trait_calling_a_service_is_optimistic() {
    let r = residence();
    let e = entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "local": true, "fields": [] }));
    let t = trait_on("LocalInvoice", false, json!([["call-service", "llm", "call", {}]]));
    assert_eq!(t.dispatch_mode(Some(&e), &r), DispatchMode::RuntimeOptimistic);
}

fn ir_trait() -> orbital_core::ir::OirTrait {
    use orbital_core::ir::{OirEffect, OirExpr, OirTrait, OirTransition};
    let mut t = OirTrait::new("Books");
    let on = |event: &str, effect: OirEffect| {
        OirTransition::new("idle", "idle", event).with_effects(vec![OirExpr::effect(effect)])
    };
    t.transitions.push(on("LOAD", OirEffect::fetch("LocalInvoice")));
    t.transitions.push(on("ADD", OirEffect::create("LocalInvoice", OirExpr::Null)));
    t.transitions.push(on("SYNC", OirEffect::fetch("Invoice")));
    t.transitions.push(on("CHARGE", OirEffect::call_service("billing", "charge", OirExpr::Null)));
    t.transitions.push(
        on("NEVER", OirEffect::fetch("LocalInvoice")).with_guard(OirExpr::bool(false)),
    );
    t
}

#[test]
fn the_browser_leg_events_are_the_ones_whose_data_lives_in_the_browser() {
    let events: Vec<String> = ir_trait().browser_leg_events(&residence()).into_iter().collect();
    assert_eq!(events, vec!["ADD".to_string(), "LOAD".to_string()]);
}

#[test]
fn control_with_no_browser_stored_entity_no_event_takes_a_browser_leg() {
    assert!(ir_trait().browser_leg_events(&Residence::none()).is_empty());
}

fn localized_invoice() -> EntityDefinition {
    entity(json!({
        "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "local": true,
        "fields": [],
        "instances": [{ "id": "C-1", "client": "Noor Logistics" }],
        "localeInstances": { "ar": [{ "id": "C-1", "client": "نور للخدمات اللوجستية" }] }
    }))
}

#[test]
fn locale_instances_round_trip_through_serde() {
    let back = serde_json::to_value(localized_invoice()).unwrap();
    assert_eq!(back["localeInstances"]["ar"][0]["client"], json!("نور للخدمات اللوجستية"));
}

#[test]
fn the_viewer_locale_takes_its_own_rows_else_the_default_rows() {
    let e = localized_invoice();
    let client = |rows: &[std::collections::HashMap<String, orbital_core::schema::SExpression>]| {
        rows[0].get("client").and_then(|v| v.as_str()).map(str::to_string)
    };
    assert_eq!(client(e.instances_for_locale(Some("ar"))).as_deref(), Some("نور للخدمات اللوجستية"));
    assert_eq!(client(e.instances_for_locale(Some("sl"))).as_deref(), Some("Noor Logistics"));
    assert_eq!(client(e.instances_for_locale(None)).as_deref(), Some("Noor Logistics"));
}

#[test]
fn control_an_entity_without_rows_has_none_for_any_locale() {
    let e = entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] }));
    assert!(e.instances_for_locale(Some("ar")).is_empty());
    let back = serde_json::to_value(&e).unwrap();
    assert!(back.get("localeInstances").is_none(), "{back}");
}

#[test]
fn a_persist_batch_runs_where_its_ops_entities_live() {
    use orbital_core::effects::persist_batch::{batch_placement, BatchPlacement};
    let r = residence();
    let local = sexpr(json!(["batch", [["create", "LocalInvoice", {}], ["delete", "LocalInvoice", "i"]]]));
    let server = sexpr(json!(["batch", [["create", "Invoice", {}]]]));
    let mapped = sexpr(json!(["batch", ["array/map", "@payload.data", ["fn", "x", ["update", "LocalInvoice", ["object/get", "@x", "id"], {}]]]]));
    let opaque = sexpr(json!(["batch", "@payload.ops"]));
    let mixed = sexpr(json!(["batch", [["create", "LocalInvoice", {}], ["create", "Invoice", {}]]]));
    let args = |e: &orbital_core::schema::SExpression| e.as_array().unwrap().to_vec();
    assert_eq!(effect_site("persist", &args(&local), &r), RunsOn::Client);
    assert_eq!(effect_site("persist", &args(&mapped), &r), RunsOn::Client);
    assert_eq!(effect_site("persist", &args(&server), &r), RunsOn::Server);
    assert_eq!(effect_site("persist", &args(&opaque), &r), RunsOn::Server);
    assert_eq!(effect_site("persist", &args(&mixed), &r), RunsOn::Server);
    assert_eq!(batch_placement(&args(&local)[1], &r), BatchPlacement::Client);
    assert_eq!(batch_placement(&args(&server)[1], &r), BatchPlacement::Server);
    assert_eq!(batch_placement(&args(&opaque)[1], &r), BatchPlacement::Undetermined);
    assert_eq!(batch_placement(&args(&mixed)[1], &r), BatchPlacement::Mixed);
}

#[test]
fn a_batch_of_browser_rows_is_not_server_work() {
    let r = residence();
    assert!(!sexpr_runs_server_effect(&sexpr(json!(["persist", "batch", [["create", "LocalInvoice", {}]]])), &r));
    assert!(sexpr_runs_server_effect(&sexpr(json!(["persist", "batch", [["create", "Invoice", {}]]])), &r));
}
