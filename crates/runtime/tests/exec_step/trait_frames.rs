// Runtime Spec Clause 2.3 on the Rust kernel: a `[shared]` entity is one frame for every trait bound to it, any other trait keeps a private frame over the row. Twin of `almadar-runtime/test/trait-frames.test.ts`.

use orbital_core_base::effect_types::Effect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn listener(name: &str, entity: &str, listens: Json, arms: Json) -> Json {
    json!({
        "name": name, "linkedEntity": entity, "listens": listens,
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": arms }
    })
}

fn arm(event: &str, effects: Json) -> Json {
    json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
}

fn from(trait_name: &str, event: &str, triggers: &str) -> Json {
    json!({ "event": event, "triggers": triggers, "source": { "kind": "trait", "trait": trait_name } })
}

fn kernel() -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "frames",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc", "shared": true, "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "v", "type": "string" }, { "name": "w", "type": "string", "default": "doc-w" }] },
            "auxiliaryEntities": [{ "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "v", "type": "string", "default": "none" }] }],
            "traits": [
                listener("Starter", "Doc", json!([]), json!([arm("GO", json!([
                    ["set", "@entity.v", "first"], ["set", "@entity.v", "start"], ["emit", "PING", { "v": "@entity.v" }]
                ]))])),
                listener("Reader", "Doc", json!([from("Starter", "PING", "READ")]), json!([arm("READ", json!([["emit", "SEEN", { "v": "@entity.v" }]]))])),
                listener("OwnB", "Note", json!([from("Starter", "PING", "WRITE"), from("OwnC", "OWN_C", "READ")]), json!([
                    arm("WRITE", json!([["set", "@entity.v", "b"]])),
                    arm("READ", json!([["emit", "OWN_SEEN", { "v": "@entity.v" }]]))
                ])),
                listener("OwnC", "Note", json!([from("Starter", "PING", "WRITE")]), json!([arm("WRITE", json!([["set", "@entity.v", "c"], ["emit", "OWN_C", {}]]))])),
                listener("OwnD", "Note", json!([from("OwnC", "OWN_C", "READ")]), json!([arm("READ", json!([["emit", "D_SEEN", { "v": "@entity.v", "w": "@entity.w" }]]))]))
            ]
        }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    (k, cache)
}

/// `v` of every `event` emitted by one `Starter.GO` dispatch, in order.
fn seen(k: &RuntimeKernel, event: &str) -> Vec<Json> {
    k.dispatch("Starter", None, "GO", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event: e, payload } if e == event => {
                Some(serde_json::to_value(payload).unwrap()["v"].clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_shared_write_is_seen_by_every_trait_on_the_entity() {
    let (k, _) = kernel();
    assert_eq!(seen(&k, "SEEN"), vec![json!("start")]);
}

#[test]
fn within_one_trait_the_later_set_wins() {
    let (k, _) = kernel();
    assert_eq!(seen(&k, "PING"), vec![json!("start")]);
}

#[test]
fn non_shared_traits_on_one_entity_keep_private_frames() {
    let (k, _) = kernel();
    assert_eq!(seen(&k, "OWN_SEEN"), vec![json!("b")]);
}

#[test]
fn a_private_frame_layers_over_the_row_and_never_over_a_siblings_write() {
    let (k, cache) = kernel();
    let row: Value = serde_json::from_value(json!({ "id": "singleton", "v": "row" })).unwrap();
    cache.write("Note", "singleton", &row).unwrap();
    assert_eq!(
        seen(&k, "D_SEEN"),
        vec![json!("row")],
        "OwnD never set v: it reads the row, not OwnC's frame"
    );
    assert_eq!(
        seen(&k, "OWN_SEEN"),
        vec![json!("b")],
        "OwnB's own write wins over the row"
    );
}

#[test]
fn a_frame_write_never_reaches_the_row_store() {
    let (k, cache) = kernel();
    seen(&k, "OWN_SEEN");
    assert!(cache
        .read("Note", "singleton")
        .map(|r| r.is_null())
        .unwrap_or(true));
}

#[test]
fn a_private_frame_persists_across_dispatches() {
    let (k, _) = kernel();
    seen(&k, "OWN_SEEN");
    assert_eq!(seen(&k, "OWN_SEEN"), vec![json!("b")]);
}

#[test]
fn snapshot_and_restore_round_trip_a_private_frame() {
    let (k, _) = kernel();
    let before = k.snapshot_trait("OwnB", None);
    seen(&k, "OWN_SEEN");
    let v = |k: &RuntimeKernel| {
        k.snapshot_trait("OwnB", None)
            .entity
            .as_object()
            .and_then(|o| o.get("v").cloned())
    };
    assert_eq!(v(&k), Some(Value::String("b".into())));
    k.restore_trait("OwnB", None, before);
    assert_eq!(v(&k), None);
}

#[test]
fn an_unwritten_private_field_reads_its_declared_default_not_a_siblings_write() {
    let (k, _) = kernel();
    assert_eq!(seen(&k, "D_SEEN"), vec![json!("none")]);
}

#[test]
fn the_primary_entitys_defaults_never_reach_a_trait_bound_to_another_entity() {
    let (k, _) = kernel();
    let payload = k
        .dispatch("Starter", None, "GO", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find_map(|er| match &er.effect {
            Effect::Emit { event, payload } if event == "D_SEEN" => {
                Some(serde_json::to_value(payload).unwrap())
            }
            _ => None,
        })
        .expect("D_SEEN");
    assert!(payload.get("w").is_none(), "an unset `@entity.w` is an absent prop: {payload}");
    assert_eq!(payload["v"], json!("none"));
}

#[test]
fn control_a_trait_bound_to_the_primary_entity_reads_its_default() {
    let (k, _) = kernel();
    let payload = k
        .dispatch("Starter", None, "GO", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find_map(|er| match &er.effect {
            Effect::Emit { event, payload } if event == "SEEN" => Some(serde_json::to_value(payload).unwrap()),
            _ => None,
        })
        .expect("SEEN");
    assert_eq!(payload["v"], json!("start"));
}

fn intrinsic_kernel() -> RuntimeKernel {
    let paged = |name: &str| {
        listener(name, "Page", json!([]), json!([
            arm("NEXT", json!([["set", "@entity.browsePage", ["+", "@entity.browsePage", 1]], ["set", "@entity.tag", name]])),
            arm("READ", json!([["emit", "SEEN", { "page": "@entity.browsePage", "tag": "@entity.tag" }]]))
        ]))
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "frames", "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Page", "shared": true, "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "browsePage", "type": "number", "default": 1, "intrinsic": true },
                { "name": "tag", "type": "string", "default": "none" }
            ] },
            "traits": [paged("A"), paged("B")]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k
}

fn seen_by(k: &RuntimeKernel, trait_name: &str) -> Json {
    k.dispatch(trait_name, None, "READ", Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find_map(|er| match &er.effect {
            Effect::Emit { event, payload } if event == "SEEN" => Some(serde_json::to_value(payload).unwrap()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn an_intrinsic_field_of_a_shared_entity_is_each_traits_own_frame() {
    let k = intrinsic_kernel();
    k.dispatch("A", None, "NEXT", Value::Null).unwrap();
    k.dispatch("A", None, "NEXT", Value::Null).unwrap();
    assert_eq!(seen_by(&k, "A")["page"], json!(3.0));
    assert_eq!(seen_by(&k, "B")["page"], json!(1.0), "paging A leaves B's page at its default");
    k.dispatch("B", None, "NEXT", Value::Null).unwrap();
    assert_eq!(seen_by(&k, "B")["page"], json!(2.0));
    assert_eq!(seen_by(&k, "A")["page"], json!(3.0));
}

#[test]
fn a_non_intrinsic_field_of_a_shared_entity_stays_shared() {
    let k = intrinsic_kernel();
    k.dispatch("A", None, "NEXT", Value::Null).unwrap();
    assert_eq!(seen_by(&k, "B")["tag"], json!("A"));
}

#[test]
fn snapshot_and_restore_round_trip_both_halves_of_a_shared_trait_frame() {
    let k = intrinsic_kernel();
    let before = k.snapshot_trait("A", None);
    k.dispatch("A", None, "NEXT", Value::Null).unwrap();
    assert_eq!(seen_by(&k, "A")["page"], json!(2.0));
    k.restore_trait("A", None, before);
    let after = seen_by(&k, "A");
    assert_eq!(after["page"], json!(1.0));
    assert_eq!(after["tag"], json!("none"));
}
