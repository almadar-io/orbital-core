//! Browser-stored entities: `[persistent: x, browser]` rows live in the client, so a
//! data effect (`fetch`/`persist`) on them runs on the client and a trait whose
//! data is all client-resident never touches the server. Twin of
//! `@almadar/std` `effectSiteFor` + `@almadar/core` `isClientResident`.

use orbital_core_base::runs_on::{effect_site, sexpr_runs_server_effect, Residence, RunsOn, ENTITY_ARGS};
use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_base::schema::{EntityDefinition, TraitDefinition};
use serde_json::json;

fn entity(value: serde_json::Value) -> EntityDefinition {
    serde_json::from_value(value).expect("entity")
}

fn residence() -> Residence {
    Residence::of_entities([
        &entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "browser": true, "fields": [] })),
        &entity(json!({ "name": "Scene", "persistence": "runtime", "fields": [] })),
        &entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] })),
    ])
}

fn sexpr(value: serde_json::Value) -> orbital_core_base::schema::SExpression {
    serde_json::from_value(value).expect("sexpr")
}

#[test]
fn browser_round_trips_through_serde() {
    let e = entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "browser": true, "fields": [] }));
    assert!(e.browser);
    let back = serde_json::to_value(&e).unwrap();
    assert_eq!(back["browser"], json!(true));
}

#[test]
fn control_an_entity_without_the_flag_serializes_without_it() {
    let e = entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] }));
    let back = serde_json::to_value(&e).unwrap();
    assert!(back.get("browser").is_none(), "{back}");
}

#[test]
fn the_retired_entity_flags_are_refused() {
    for retired in ["local", "seedMock"] {
        let value = json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", retired: true, "fields": [] });
        assert!(serde_json::from_value::<EntityDefinition>(value).is_err(), "{retired}");
    }
}

#[test]
fn only_browser_entities_store_rows_in_the_browser() {
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
    assert_eq!(effect_site("call-service", &[sexpr(json!("test-svc"))], &r), RunsOn::Server);
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

fn trait_on(entity: &str, dispatch: Option<&str>, effects: serde_json::Value) -> TraitDefinition {
    let mut value = json!({
        "name": "T", "linkedEntity": entity,
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] }
    });
    if let Some(mode) = dispatch {
        value["dispatch"] = json!(mode);
    }
    serde_json::from_value(value).expect("trait")
}

fn browser_invoice() -> EntityDefinition {
    entity(json!({ "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "browser": true, "fields": [] }))
}

#[test]
fn a_client_trait_on_a_browser_entity_is_client_only() {
    let t = trait_on("LocalInvoice", Some("client"), json!([["fetch", "LocalInvoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&browser_invoice()), &residence()), DispatchMode::HybridClientOnly);
}

#[test]
fn control_an_undeclared_trait_on_a_browser_entity_awaits_the_server() {
    let t = trait_on("LocalInvoice", None, json!([["fetch", "LocalInvoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&browser_invoice()), &residence()), DispatchMode::PersistedAwaited);
}

#[test]
fn control_a_trait_on_a_server_entity_fetching_it_awaits_the_server() {
    let r = residence();
    let e = entity(json!({ "name": "Invoice", "persistence": "persistent", "collection": "invoices", "fields": [] }));
    let t = trait_on("Invoice", None, json!([["fetch", "Invoice", {}]]));
    assert_eq!(t.dispatch_mode(Some(&e), &r), DispatchMode::PersistedAwaited);
}

#[test]
fn an_optimistic_trait_on_a_runtime_entity_is_optimistic_and_an_undeclared_one_is_server() {
    let r = residence();
    let e = entity(json!({ "name": "Scene", "persistence": "runtime", "fields": [] }));
    let declared = trait_on("Scene", Some("optimistic"), json!([["fetch", "Invoice", {}]]));
    let undeclared = trait_on("Scene", None, json!([["fetch", "Invoice", {}]]));
    assert_eq!(declared.dispatch_mode(Some(&e), &r), DispatchMode::RuntimeOptimistic);
    assert_eq!(undeclared.dispatch_mode(Some(&e), &r), DispatchMode::PersistedAwaited);
}

#[test]
fn an_optimistic_trait_on_a_browser_entity_calling_a_service_is_optimistic() {
    let t = trait_on("LocalInvoice", Some("optimistic"), json!([["call-service", "test-svc", "run", {}]]));
    assert_eq!(t.dispatch_mode(Some(&browser_invoice()), &residence()), DispatchMode::RuntimeOptimistic);
}

fn ir_trait() -> orbital_core_base::ir::OirTrait {
    use orbital_core_base::ir::{OirEffect, OirExpr, OirTrait, OirTransition};
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

fn ticking_trait() -> orbital_core_base::ir::OirTrait {
    use orbital_core_base::ir::{OirEffect, OirExpr, OirInterval, OirTick, OirTrait};
    let mut t = OirTrait::new("Editor");
    let tick = |name: &str, effect: OirEffect, background: bool| {
        let mut tick = OirTick::new(name, OirInterval::Milliseconds { value: 1000 });
        tick.effects = vec![OirExpr::effect(effect)];
        tick.background = background;
        tick
    };
    t.ticks.push(tick("autosave", OirEffect::create("LocalInvoice", OirExpr::Null), false));
    t.ticks.push(tick("sync", OirEffect::fetch("Invoice"), false));
    t.ticks.push(tick("nightly", OirEffect::create("LocalInvoice", OirExpr::Null), true));
    t
}

#[test]
fn a_client_tick_writing_browser_data_takes_a_run_tick_leg() {
    let events: Vec<String> = ticking_trait()
        .browser_leg_events(&residence())
        .into_iter()
        .collect();
    assert_eq!(
        events,
        vec![orbital_core_base::bus_event::tick_event_key("autosave")]
    );
    assert_eq!(
        orbital_core_base::bus_event::tick_event_key("autosave"),
        "$TICK_autosave"
    );
}

#[test]
fn control_with_no_browser_stored_entity_no_tick_takes_a_browser_leg() {
    assert!(ticking_trait()
        .browser_leg_events(&Residence::none())
        .is_empty());
}

fn localized_invoice() -> EntityDefinition {
    entity(json!({
        "name": "LocalInvoice", "persistence": "persistent", "collection": "invoices", "browser": true,
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
    let client = |rows: &[orbital_core_base::schema::SObject]| {
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
    use orbital_core_base::persist_batch::{batch_placement, BatchPlacement};
    let r = residence();
    let local = sexpr(json!(["batch", [["create", "LocalInvoice", {}], ["delete", "LocalInvoice", "i"]]]));
    let server = sexpr(json!(["batch", [["create", "Invoice", {}]]]));
    let mapped = sexpr(json!(["batch", ["array/map", "@payload.data", ["fn", "x", ["update", "LocalInvoice", ["object/get", "@x", "id"], {}]]]]));
    let opaque = sexpr(json!(["batch", "@payload.ops"]));
    let mixed = sexpr(json!(["batch", [["create", "LocalInvoice", {}], ["create", "Invoice", {}]]]));
    let args = |e: &orbital_core_base::schema::SExpression| e.as_array().unwrap().to_vec();
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

fn oir_entity(name: &str, persistence: orbital_core_base::ir::OirPersistence, browser: bool) -> orbital_core_base::ir::OirEntity {
    let mut e = orbital_core_base::ir::OirEntity::new(name);
    e.persistence = persistence;
    e.browser = browser;
    e
}

/// Where a tick runs (drift D-1): on the server when it is `[background]` or its trait is
/// server-awaited, in the client otherwise. One rule for the validator, the kernel and every shell.
/// The trait's declared mode decides; the entity it links never does.
#[test]
fn a_tick_runs_on_the_server_only_when_background_or_server_awaited() {
    use orbital_core_base::ir::{OirInterval, OirPersistence, OirTick, OirTrait};
    use orbital_core_base::schema::types::DispatchMode as Declared;
    let r = residence();
    let undeclared = OirTrait::new("T");
    let mut client = OirTrait::new("T");
    client.dispatch = Some(Declared::Client);
    let tick = OirTick::new("beat", OirInterval::Milliseconds { value: 1000 });
    let mut background = tick.clone();
    background.background = true;
    let runtime = oir_entity("Scene", OirPersistence::Runtime, false);
    let browser = oir_entity("LocalInvoice", OirPersistence::Persistent, true);
    let server = oir_entity("Invoice", OirPersistence::Persistent, false);
    for entity in [&runtime, &browser, &server] {
        assert!(client.tick_runs_on_client(&tick, Some(entity), &r), "a [client] trait ticks in the client");
        assert!(!undeclared.tick_runs_on_client(&tick, Some(entity), &r), "an undeclared trait ticks on the server");
        assert!(!client.tick_runs_on_client(&background, Some(entity), &r), "[background] runs with no tab open");
    }
}
