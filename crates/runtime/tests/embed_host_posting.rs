// What a host posts for a dispatch and a mount, and how it folds the answer (twins of `postServerLeg`, `postMountLeg`, `foldBase`/`changedSinceCarried` in `almadar-runtime/src/evaluation/client-role.ts`; ui `BrowserPlayground.wire`, `Chat.realtime-chat`, `Riya.boot-order`).

use orbital_core_base::value::ValueMap;
use orbital_core_runtime::effects::{LifecycleEvent, MountSeed};
use orbital_core_runtime::embed::EmbeddedKernel;
use serde_json::json;
use std::collections::HashSet;
use {
    orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value,
    orbital_core_runtime::effects::response::EventResponse,
};

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "shop",
        "orbitals": [{
            "name": "Shop",
            "entity": { "name": "Item", "persistence": "persistent", "collection": "items",
                "fields": [{ "name": "id", "type": "string" }, { "name": "note", "type": "string" }, { "name": "count", "type": "number" }] },
            "traits": [
                { "name": "Lister", "linkedEntity": "Item",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                    "transitions": [
                        { "from": "idle", "to": "open", "event": "INIT", "effects": [] },
                        { "from": "open", "to": "open", "event": "LOAD", "effects": [["fetch", "Item", {}]] }] } },
                { "name": "Loader", "linkedEntity": "Item",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }],
                    "transitions": [{ "from": "idle", "to": "ready", "event": "INIT", "effects": [["fetch", "Item", {}]] }] } },
                { "name": "Badge", "linkedEntity": "Scratch",
                  "stateMachine": {
                    "states": [{ "name": "off", "isInitial": true }, { "name": "on" }],
                    "transitions": [{ "from": "off", "to": "on", "event": "INIT", "effects": [] }] } }
            ]
        }, {
            "name": "Scratchpad",
            "entity": { "name": "Scratch", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": []
        }]
    }))
    .unwrap()
}

fn kernel() -> EmbeddedKernel {
    EmbeddedKernel::new(schema(), "c1").unwrap()
}

fn seed(t: &str) -> MountSeed {
    MountSeed {
        r#trait: t.to_string(),
        event: LifecycleEvent::Init,
    }
}

#[test]
fn a_mount_posts_one_leg_per_orbital_for_the_seeds_that_delegated() {
    let k = kernel();
    let mount = k
        .dispatch_mount(
            &[seed("Lister"), seed("Loader"), seed("Badge")],
            None,
            Value::Null,
        )
        .unwrap();
    assert!(mount.failed.is_none());
    assert_eq!(mount.seeds.len(), 3);
    assert_eq!(mount.legs.len(), 1);
    let leg = &mount.legs[0];
    assert_eq!(leg.orbital, "Shop");
    assert_eq!(
        leg.seeds
            .iter()
            .map(|s| s.trait_name.as_str())
            .collect::<Vec<_>>(),
        vec!["Loader"]
    );
    assert_eq!(leg.leg.mount.as_ref().map(Vec::len), Some(1));
    assert_eq!(leg.leg.client_id.as_deref(), Some("c1"));
}

#[test]
fn a_stateful_host_is_also_posted_the_server_backed_seed_that_delegated_nothing() {
    let k = kernel();
    let mount = k
        .dispatch_mount(
            &[seed("Lister"), seed("Loader"), seed("Badge")],
            None,
            Value::Null,
        )
        .unwrap();
    assert_eq!(mount.stateful_legs.len(), 1);
    let names: Vec<&str> = mount.stateful_legs[0]
        .seeds
        .iter()
        .map(|s| s.trait_name.as_str())
        .collect();
    assert_eq!(names, vec!["Lister", "Loader"]);
    assert_eq!(
        mount.stateful_legs[0].leg.mount.as_ref().map(Vec::len),
        Some(2)
    );
}

#[test]
fn control_a_mount_of_client_only_seeds_posts_nothing() {
    let mount = kernel()
        .dispatch_mount(&[seed("Badge")], None, Value::Null)
        .unwrap();
    assert!(mount.legs.is_empty() && mount.stateful_legs.is_empty());
}

#[test]
fn a_failing_seed_stops_the_mount_and_is_named() {
    let mount = kernel()
        .dispatch_mount(&[seed("Badge"), seed("Missing")], None, Value::Null)
        .unwrap();
    let failed = mount.failed.expect("a failed seed");
    assert_eq!(failed.trait_name, "Missing");
    assert_eq!(failed.event, "INIT");
    assert!(mount.legs.is_empty());
}

fn row(v: serde_json::Value) -> Value {
    serde_json::from_value(v).unwrap()
}

fn answer(rows: ValueMap) -> EventResponse {
    EventResponse {
        success: true,
        entity_by_trait: Some(rows),
        ..Default::default()
    }
}

fn frame(k: &EmbeddedKernel, t: &str) -> serde_json::Value {
    serde_json::to_value(k.kernel().snapshot_trait(t, None).entity).unwrap()
}

#[test]
fn a_late_echo_of_the_carried_row_does_not_overwrite_a_newer_client_value() {
    let k = kernel();
    let carried = ValueMap::from([(
        "Loader".to_string(),
        row(json!({ "id": "singleton", "note": "old" })),
    )]);
    let base = k.fold_base(Some(&carried), &[], None).unwrap();
    k.apply_server_response_since(
        &answer(ValueMap::from([(
            "Loader".to_string(),
            row(json!({ "id": "singleton", "note": "old", "count": 3.0 })),
        )])),
        &HashSet::new(),
        None,
    )
    .unwrap();
    assert_eq!(frame(&k, "Loader")["note"], json!("old"));
    k.kernel().restore_trait(
        "Loader",
        None,
        orbital_core_runtime::kernel::TraitSnapshot {
            state: String::new(),
            entity: row(json!({ "id": "singleton", "note": "typing", "count": 3.0 })),
        },
    );
    let echo = answer(ValueMap::from([(
        "Loader".to_string(),
        row(json!({ "id": "singleton", "note": "old", "count": 4.0 })),
    )]));
    k.apply_server_response_since(&echo, &HashSet::new(), Some(&base))
        .unwrap();
    let after = frame(&k, "Loader");
    assert_eq!(
        after["note"],
        json!("typing"),
        "the echoed carried field is not a write"
    );
    assert_eq!(
        after["count"].as_f64(),
        Some(4.0),
        "a field the host changed folds"
    );
}

#[test]
fn control_without_a_base_the_echo_overwrites() {
    let k = kernel();
    k.kernel().restore_trait(
        "Loader",
        None,
        orbital_core_runtime::kernel::TraitSnapshot {
            state: String::new(),
            entity: row(json!({ "id": "singleton", "note": "typing" })),
        },
    );
    let echo = answer(ValueMap::from([(
        "Loader".to_string(),
        row(json!({ "id": "singleton", "note": "old" })),
    )]));
    k.apply_server_response_since(&echo, &HashSet::new(), None)
        .unwrap();
    assert_eq!(frame(&k, "Loader")["note"], json!("old"));
}

#[test]
fn a_row_with_no_carried_counterpart_folds_whole() {
    let k = kernel();
    let base = k.fold_base(Some(&ValueMap::new()), &[], None).unwrap();
    let echo = answer(ValueMap::from([(
        "Loader".to_string(),
        row(json!({ "id": "singleton", "note": "fresh" })),
    )]));
    k.apply_server_response_since(&echo, &HashSet::new(), Some(&base))
        .unwrap();
    assert_eq!(frame(&k, "Loader")["note"], json!("fresh"));
}

#[test]
fn a_seeded_trait_is_based_on_the_row_the_client_shows() {
    let k = kernel();
    k.kernel().restore_trait(
        "Loader",
        None,
        orbital_core_runtime::kernel::TraitSnapshot {
            state: String::new(),
            entity: row(json!({ "id": "singleton", "note": "shown" })),
        },
    );
    let carried = ValueMap::from([(
        "Loader".to_string(),
        row(json!({ "id": "singleton", "note": "pre-dispatch" })),
    )]);
    let seeded = k
        .fold_base(Some(&carried), &["Loader".to_string()], None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&seeded["Loader"]).unwrap()["note"],
        json!("shown")
    );
    let plain = k.fold_base(Some(&carried), &[], None).unwrap();
    assert_eq!(
        serde_json::to_value(&plain["Loader"]).unwrap()["note"],
        json!("pre-dispatch")
    );
    assert!(k.fold_base(None, &["Loader".to_string()], None).is_none());
}

#[test]
fn a_dispatch_traces_its_executed_hops() {
    let k = kernel();
    let wire = k.dispatch("Lister", None, "INIT", Value::Null).unwrap();
    let hops = wire.response.transitions.expect("a trace");
    assert_eq!(hops.len(), 1);
    assert_eq!(
        (
            hops[0].trait_name.as_str(),
            hops[0].from.as_str(),
            hops[0].to.as_str(),
            hops[0].event.as_str()
        ),
        ("Lister", "idle", "open", "INIT")
    );
}

#[test]
fn control_a_rejected_dispatch_has_no_trace() {
    let k = kernel();
    let wire = k.dispatch("Lister", None, "LOAD", Value::Null).unwrap();
    assert!(wire.response.transitions.is_none());
}

#[test]
fn a_mount_leg_addresses_the_entity_the_mount_named_and_none_otherwise() {
    let k = kernel();
    let named = k
        .dispatch_mount(&[seed("Loader")], Some("row-7"), Value::Null)
        .unwrap();
    assert_eq!(named.legs[0].leg.entity_id.as_deref(), Some("row-7"));
    let anonymous = kernel()
        .dispatch_mount(&[seed("Loader")], None, Value::Null)
        .unwrap();
    assert_eq!(anonymous.legs[0].leg.entity_id, None);
}

fn awaiting_init(leg: &orbital_core_runtime::effects::response::EventRequest) -> Vec<String> {
    let payload = serde_json::to_value(leg.payload.clone()).unwrap();
    let mut names: Vec<String> = payload["_awaitingInit"]
        .as_array()
        .expect("the leg carries the client's mount")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn a_mount_leg_names_the_server_backed_traits_still_awaiting_their_init() {
    let k = kernel();
    k.mount(vec![
        "Lister".to_string(),
        "Loader".to_string(),
        "Badge".to_string(),
    ]);
    let mount = k
        .dispatch_mount(&[seed("Loader")], None, Value::Null)
        .unwrap();
    assert_eq!(
        awaiting_init(&mount.legs[0].leg),
        vec!["Lister".to_string()],
        "Badge is client-only: its INIT never posts"
    );
}

#[test]
fn control_a_mount_whose_seeds_all_ran_awaits_nothing() {
    let k = kernel();
    k.mount(vec![
        "Lister".to_string(),
        "Loader".to_string(),
        "Badge".to_string(),
    ]);
    let mount = k
        .dispatch_mount(
            &[seed("Lister"), seed("Loader"), seed("Badge")],
            None,
            Value::Null,
        )
        .unwrap();
    assert!(awaiting_init(&mount.legs[0].leg).is_empty());
    assert!(awaiting_init(&mount.stateful_legs[0].leg).is_empty());
}

// A leg to a stateful host carries the shared frames its traits read (G-CROSS-260): the host holds every other piece of
// circuit state, but a client-side write into a `[shared]` frame is otherwise never seen by a host step reading that frame.
fn shared_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "Chat",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "collection": "messages", "shared": true,
                "fields": [{ "name": "id", "type": "string" }, { "name": "activeChannel", "type": "string" }] },
            "traits": [
                { "name": "Room", "linkedEntity": "Message", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SELECT", "effects": [["set", "@entity.activeChannel", "@payload.channel"]] }] } },
                { "name": "Thread", "linkedEntity": "Message", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }
            ]
        }, {
            "name": "NoteOrbital",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Editor", "linkedEntity": "Note", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "TOUCH", "effects": [["set", "@entity.touched", true]] }] } }]
        }]
    }))
    .unwrap()
}

#[test]
fn a_stateful_leg_keeps_the_shared_frame_rows_and_drops_the_rest() {
    let k = EmbeddedKernel::new(shared_schema(), "c1").unwrap();
    let leg: orbital_core_runtime::effects::response::EventRequest = serde_json::from_value(json!({
        "event": "INIT",
        "targetTrait": "Thread",
        "traits": [{ "trait": "Thread", "from": "idle" }, { "trait": "Editor", "from": "idle" }],
        "entityByTrait": { "Thread": { "activeChannel": "chan-5" }, "Editor": { "id": "n1" } }
    }))
    .unwrap();
    let shaped = serde_json::to_value(k.stateful_leg(leg)).unwrap();
    assert_eq!(
        shaped["entityByTrait"],
        json!({ "Thread": { "activeChannel": "chan-5" } })
    );
    assert!(shaped.get("traits").is_none());
    assert_eq!(shaped["targetTrait"], json!("Thread"));
}

#[test]
fn control_a_stateful_leg_with_no_shared_frame_carries_no_rows() {
    let k = EmbeddedKernel::new(shared_schema(), "c1").unwrap();
    let leg: orbital_core_runtime::effects::response::EventRequest =
        serde_json::from_value(json!({
            "event": "SAVE", "targetTrait": "Editor",
            "traits": [{ "trait": "Editor", "from": "idle" }],
            "entityByTrait": { "Editor": { "id": "n1" } }
        }))
        .unwrap();
    let shaped = serde_json::to_value(k.stateful_leg(leg)).unwrap();
    assert!(shaped.get("entityByTrait").is_none());
    assert!(shaped.get("traits").is_none());
}

#[test]
fn a_kernel_built_stateful_leg_carries_the_shared_frame_as_it_was_before_the_dispatch() {
    let k = EmbeddedKernel::new(shared_schema(), "c1").unwrap();
    k.dispatch(
        "Room",
        None,
        "SELECT",
        serde_json::from_value(json!({ "channel": "chan-1" })).unwrap(),
    )
    .unwrap();
    let wire = k
        .dispatch(
            "Room",
            None,
            "SELECT",
            serde_json::from_value(json!({ "channel": "chan-5" })).unwrap(),
        )
        .unwrap();
    let leg = serde_json::to_value(&wire.stateful_leg.expect("a stateful leg").leg).unwrap();
    assert_eq!(
        leg["entityByTrait"]["Room"]["activeChannel"],
        json!("chan-1")
    );
    assert!(leg.get("traits").is_none());
}

#[test]
fn control_a_kernel_built_stateful_leg_of_a_non_shared_trait_carries_no_rows() {
    let k = EmbeddedKernel::new(shared_schema(), "c1").unwrap();
    let wire = k.dispatch("Editor", None, "TOUCH", Value::Null).unwrap();
    let leg = serde_json::to_value(&wire.stateful_leg.expect("a stateful leg").leg).unwrap();
    assert!(leg.get("entityByTrait").is_none());
}

// A mount batch's stateful leg carries each seed's shared frame as it was before that seed ran (EXEC-DIST-2), so the
// host's echo of a field a client-only seed wrote later in the batch is not a write (G-VERIFY-054).
fn composer_schema() -> OrbitalSchema {
    composer_schema_with(json!([]))
}

fn composer_schema_with(thread_init: serde_json::Value) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "Chat",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "collection": "messages", "shared": true,
                "fields": [{ "name": "id", "type": "string" }, { "name": "activeChannel", "type": "string", "default": "" }] },
            "traits": [
                { "name": "Thread", "linkedEntity": "Message", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": thread_init }] } },
                { "name": "Composer", "linkedEntity": "Message", "local": true, "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [{ "from": "ready", "to": "ready", "event": "INIT", "effects": [["set", "@entity.activeChannel", "general"]] }] } }
            ]
        }]
    }))
    .unwrap()
}

fn host_answer(active_channel: &str) -> EventResponse {
    answer(ValueMap::from([(
        "Thread".to_string(),
        row(json!({ "activeChannel": active_channel })),
    )]))
}

fn mounted_composer() -> (
    EmbeddedKernel,
    Option<orbital_core_runtime::embed::FoldBase>,
) {
    mounted(composer_schema())
}

fn mounted(
    schema: OrbitalSchema,
) -> (
    EmbeddedKernel,
    Option<orbital_core_runtime::embed::FoldBase>,
) {
    let k = EmbeddedKernel::new(schema, "c1").unwrap();
    let mount = k
        .dispatch_mount(&[seed("Thread"), seed("Composer")], None, Value::Null)
        .unwrap();
    assert_eq!(frame(&k, "Composer")["activeChannel"], json!("general"));
    let group = &mount.stateful_legs[0];
    let carried = group.leg.entity_by_trait.clone();
    assert_eq!(
        carried
            .as_ref()
            .map(|rows| serde_json::to_value(&rows["Thread"]).unwrap()["activeChannel"].clone()),
        Some(json!("")),
        "the stateful mount leg carries the shared frame Thread ran from"
    );
    let base = k.fold_base(carried.as_ref(), &group.fold_seeded, None);
    (k, base)
}

#[test]
fn a_mounts_host_echo_of_the_shared_frame_keeps_a_client_only_seeds_write() {
    let (k, base) = mounted_composer();
    k.apply_server_response_since(&host_answer(""), &HashSet::new(), base.as_ref())
        .unwrap();
    assert_eq!(frame(&k, "Composer")["activeChannel"], json!("general"));
}

#[test]
fn control_a_field_the_host_changed_on_the_shared_frame_folds() {
    let (k, base) = mounted_composer();
    k.apply_server_response_since(&host_answer("random"), &HashSet::new(), base.as_ref())
        .unwrap();
    assert_eq!(frame(&k, "Composer")["activeChannel"], json!("random"));
}

#[test]
fn a_mount_seed_that_delegates_a_fetch_also_carries_its_shared_frame_to_a_stateful_host() {
    let (k, base) = mounted(composer_schema_with(
        json!([["fetch", "Message", { "emit": { "success": "LOADED" } }]]),
    ));
    k.apply_server_response_since(&host_answer(""), &HashSet::new(), base.as_ref())
        .unwrap();
    assert_eq!(frame(&k, "Composer")["activeChannel"], json!("general"));
}
