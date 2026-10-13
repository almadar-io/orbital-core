// A `listens` source routes by the emitter's identity: ids when the listen carries them (rename-proof),
// names otherwise. A name-only `trait` source hears only the listener's own orbital; a source of kind
// `orbital` names the emitter's orbital, which is how a listener reaches across orbitals. JS twin:
// `buildSourceMatcher` (`almadar-runtime/src/events/identity/routing.ts`) and
// `identity-rename-routing.test.ts`.

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn kernel(schema: Json) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(schema).unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k
}

fn player(trait_name: &str, id: Option<&str>) -> Json {
    let mut def = json!({
        "name": trait_name, "linkedEntity": "S",
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "UP", "effects": [["emit", "TURN", { "dir": "up" }]] }] }
    });
    if let Some(id) = id {
        def["id"] = json!(id);
    }
    def
}

fn listener(source: Json) -> Json {
    json!({
        "name": "Mechanic", "linkedEntity": "S",
        "listens": [{ "event": "TURN", "triggers": "STEER", "source": source }],
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "STEER" }] }
    })
}

fn orbital(name: &str, traits: Vec<Json>) -> Json {
    json!({
        "name": name,
        "entity": { "name": "S", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
        "traits": traits
    })
}

fn heard(orbitals: Vec<Json>, seed: &str) -> bool {
    let k = kernel(json!({ "name": "App", "orbitals": orbitals }));
    k.dispatch(seed, None, "UP", Value::Null)
        .unwrap()
        .iter()
        .any(|(t, r)| t == "Mechanic" && r.executed)
}

#[test]
fn an_orbital_source_hears_the_trait_of_that_orbital() {
    let src = json!({ "kind": "orbital", "orbital": "A", "trait": "Play" });
    assert!(heard(
        vec![orbital("A", vec![player("Play", None), listener(src)])],
        "Play"
    ));
}

#[test]
fn control_an_orbital_source_does_not_hear_a_same_named_trait_of_another_orbital() {
    let src = json!({ "kind": "orbital", "orbital": "Other", "trait": "Play" });
    let orbitals = vec![
        orbital("A", vec![player("Play", None), listener(src)]),
        orbital("Other", vec![]),
    ];
    assert!(!heard(orbitals, "Play"));
}

#[test]
fn an_orbital_source_hears_a_trait_of_another_orbital_it_names() {
    let src = json!({ "kind": "orbital", "orbital": "B", "trait": "Play" });
    let orbitals = vec![
        orbital("A", vec![listener(src)]),
        orbital("B", vec![player("Play", None)]),
    ];
    assert!(heard(orbitals, "Play"));
}

#[test]
fn a_trait_source_without_an_id_hears_the_named_trait() {
    let src = json!({ "kind": "trait", "trait": "Play" });
    assert!(heard(
        vec![orbital(
            "A",
            vec![player("Play", None), listener(src.clone())]
        )],
        "Play"
    ));
    let other = json!({ "kind": "trait", "trait": "Other" });
    assert!(!heard(
        vec![orbital("A", vec![player("Play", None), listener(other)])],
        "Play"
    ));
}

#[test]
fn control_a_name_only_trait_source_does_not_hear_the_same_named_trait_of_another_orbital() {
    let src = json!({ "kind": "trait", "trait": "Play" });
    let orbitals = vec![
        orbital("A", vec![listener(src)]),
        orbital("B", vec![player("Play", None)]),
    ];
    assert!(!heard(orbitals, "Play"));
}

#[test]
fn a_name_only_trait_source_hears_the_trait_of_the_listeners_own_orbital_among_several() {
    let src = json!({ "kind": "trait", "trait": "Play" });
    let orbitals = vec![
        orbital("A", vec![player("Play", None), listener(src)]),
        orbital("B", vec![]),
    ];
    assert!(heard(orbitals, "Play"));
}

#[test]
fn a_trait_id_survives_a_rename_of_the_emitter() {
    let src = json!({ "kind": "trait", "trait": "Emitter", "traitId": "trt_A" });
    let renamed = vec![orbital(
        "A",
        vec![player("PulseEmitter", Some("trt_A")), listener(src)],
    )];
    assert!(heard(renamed, "PulseEmitter"));
}

#[test]
fn control_the_same_rename_breaks_a_name_only_source() {
    let src = json!({ "kind": "trait", "trait": "Emitter" });
    let renamed = vec![orbital(
        "A",
        vec![player("PulseEmitter", None), listener(src)],
    )];
    assert!(!heard(renamed, "PulseEmitter"));
}

#[test]
fn control_a_listen_carrying_an_id_the_emitter_lacks_is_a_mismatch_not_a_name_fallback() {
    let src = json!({ "kind": "trait", "trait": "Play", "traitId": "trt_A" });
    assert!(!heard(
        vec![orbital("A", vec![player("Play", None), listener(src)])],
        "Play"
    ));
}

#[test]
fn an_orbital_source_with_both_ids_matches_by_id_after_a_rename() {
    let mut a = orbital(
        "RenamedA",
        vec![
            player("Play", Some("trt_A")),
            listener(json!({
                "kind": "orbital", "orbital": "OldA", "orbitalId": "orb_A", "trait": "OldPlay", "traitId": "trt_A"
            })),
        ],
    );
    a["id"] = json!("orb_A");
    assert!(heard(vec![a], "Play"));
}
