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
        "typeContractVersion": 1, "orbitals": [{
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
                { "name": "Badge", "linkedEntity": "Scratch", "dispatch": "client",
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
        frames: Some(orbital_core_runtime::effects::FrameDelta::absolute(
            rows.into_iter().map(|(frame, row)| (frame.to_string(), row.as_object().unwrap().clone())),
        )),
        ..Default::default()
    }
}

fn frame(k: &EmbeddedKernel, t: &str) -> serde_json::Value {
    serde_json::to_value(k.kernel().snapshot_trait(t, None).entity).unwrap()
}

#[test]
fn a_late_echo_of_the_carried_row_does_not_overwrite_a_newer_client_value() {
    let k = kernel();
    let carried = ValueMap::from([("Loader".into(),
        row(json!({ "id": "singleton", "note": "old" })),
    )]);
    let base = k.fold_base(Some(&carried), &[], None).unwrap();
    k.apply_server_response_since(
        &answer(ValueMap::from([("Loader".into(),
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
    let echo = answer(ValueMap::from([("Loader".into(),
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
    let echo = answer(ValueMap::from([("Loader".into(),
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
    let echo = answer(ValueMap::from([("Loader".into(),
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
    let carried = ValueMap::from([("Loader".into(),
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
        "typeContractVersion": 1, "orbitals": [{
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
        "typeContractVersion": 1, "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "collection": "messages", "shared": true,
                "fields": [{ "name": "id", "type": "string" }, { "name": "activeChannel", "type": "string", "default": "" }] },
            "traits": [
                { "name": "Thread", "linkedEntity": "Message", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": thread_init }] } },
                { "name": "Composer", "linkedEntity": "Message", "dispatch": "client", "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [{ "from": "ready", "to": "ready", "event": "INIT", "effects": [["set", "@entity.activeChannel", "general"]] }] } }
            ]
        }]
    }))
    .unwrap()
}

fn host_answer(active_channel: &str) -> EventResponse {
    answer(ValueMap::from([(
        "$shared::Message".into(),
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

// A stateful host holds the circuit state, so a plain mount leg carries only the shared frame its seed ran from; the
// frames and states of a reconnect travel on `reseed_leg` (EXEC-DIST-9).
#[test]
fn a_remount_leg_carries_the_shared_frame_the_client_holds() {
    let (k, base) = mounted_composer();
    k.apply_server_response_since(&host_answer("random"), &HashSet::new(), base.as_ref()).unwrap();
    let again = k.dispatch_mount(&[seed("Thread")], None, Value::Null).unwrap();
    let carried = again.stateful_legs[0].leg.entity_by_trait.clone().expect("the leg carries the client's frame");
    assert_eq!(serde_json::to_value(&carried["Thread"]).unwrap()["activeChannel"], json!("random"));
}

#[test]
fn control_a_first_mount_leg_carries_the_default_the_seed_ran_from() {
    let k = EmbeddedKernel::new(composer_schema(), "c1").unwrap();
    let fresh = k.dispatch_mount(&[seed("Thread")], None, Value::Null).unwrap();
    let carried = fresh.stateful_legs[0].leg.entity_by_trait.clone().expect("a frame");
    assert_eq!(serde_json::to_value(&carried["Thread"]).unwrap()["activeChannel"], json!(""));
}

#[test]
fn a_reseed_leg_carries_the_state_and_frame_the_client_holds_for_each_seed() {
    let (k, base) = mounted_composer();
    k.apply_server_response_since(&host_answer("random"), &HashSet::new(), base.as_ref()).unwrap();
    let leg = k.reseed_leg(&[seed("Thread")], None);
    assert!(leg.mount.as_ref().is_some_and(|m| m.len() == 1));
    assert!(leg.traits.as_ref().is_some_and(|t| t.iter().any(|f| f.r#trait == "Thread" && f.from == "idle")));
    let carried = leg.entity_by_trait.expect("the client's frame");
    assert_eq!(serde_json::to_value(&carried["Thread"]).unwrap()["activeChannel"], json!("random"));
}

#[test]
fn control_a_reseed_leg_leaves_out_a_trait_the_client_holds_no_frame_for() {
    let k = EmbeddedKernel::new(composer_schema(), "c1").unwrap();
    let leg = k.reseed_leg(&[seed("Thread")], None);
    assert!(leg.entity_by_trait.as_ref().is_none_or(|rows| !rows.contains_key("Thread")), "{:?}", leg.entity_by_trait);
}
/// `Picker` (`[client]`) asks from its INIT; `Asker` (server) answers with a fetch. Mounted together, the ask is held for
/// Asker's INIT (EXEC-SEED-2), so it runs in Asker's seed dispatch, not Picker's.
fn held_ask_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "picker",
        "typeContractVersion": 1, "orbitals": [{
            "name": "People",
            "entity": { "name": "Person", "persistence": "persistent", "collection": "people",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Picker", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }],
                    "transitions": [{ "from": "idle", "to": "asking", "event": "INIT", "effects": [["emit", "ASK", {}]] }] } },
                { "name": "Asker", "linkedEntity": "Person",
                  "listens": [{ "event": "ASK", "triggers": "ASKED", "source": { "kind": "trait", "trait": "Picker" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "ASKED", "effects": [["fetch", "Person", {}]] }] } },
                { "name": "Quiet", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "off", "isInitial": true }, { "name": "on" }],
                    "transitions": [{ "from": "off", "to": "on", "event": "INIT" }] } }
            ]
        }]
    }))
    .unwrap()
}

fn mounted_seeds(stateful: bool) -> Vec<String> {
    let k = EmbeddedKernel::new(held_ask_schema(), "c1").unwrap();
    k.mount(vec![
        "Picker".to_string(),
        "Asker".to_string(),
        "Quiet".to_string(),
    ]);
    let mount = k
        .dispatch_mount(
            &[seed("Picker"), seed("Asker"), seed("Quiet")],
            None,
            Value::Null,
        )
        .unwrap();
    let legs = if stateful {
        &mount.stateful_legs
    } else {
        &mount.legs
    };
    assert_eq!(legs.len(), 1, "one leg for the orbital");
    legs[0].seeds.iter().map(|s| s.trait_name.clone()).collect()
}

/// EXEC-SEED-2 + EXEC-DIST-10: the host runs no copy of a `[client]` trait, so a client-only seed whose held emit a
/// later seed delivered to a server trait is replayed with the mount, ahead of that seed, or the ask never reaches it.
#[test]
fn a_mount_replays_the_client_only_seed_whose_held_emit_reached_a_server_trait() {
    assert_eq!(mounted_seeds(true), vec!["Picker", "Asker"]);
}

#[test]
fn control_a_host_that_carries_the_circuit_state_is_posted_the_same_seeds() {
    assert_eq!(mounted_seeds(false), vec!["Picker", "Asker"]);
}

#[test]
fn control_a_client_only_seed_that_reaches_no_server_trait_still_posts_nothing() {
    assert!(!mounted_seeds(true).contains(&"Quiet".to_string()));
}

/// `A` (`[client]`) emits X from INIT; `B` (`[client]`) hears X and emits Y; `C` hears Y. With `server_c` C fetches.
fn client_cascade_schema(server_c: bool) -> OrbitalSchema {
    let fetch: Vec<serde_json::Value> = if server_c { vec![json!(["fetch", "Person", {}])] } else { vec![] };
    serde_json::from_value(json!({
        "name": "cascade",
        "typeContractVersion": 1, "orbitals": [{
            "name": "People",
            "entity": { "name": "Person", "persistence": "persistent", "collection": "people",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "A", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "X", {"v": 1}]] }] } },
                { "name": "B", "dispatch": "client",
                  "listens": [{ "event": "X", "triggers": "GOT_X", "source": { "kind": "trait", "trait": "A" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "GOT_X", "effects": [["emit", "Y", {"v": 2}]] }] } },
                { "name": "C", "linkedEntity": "Person",
                  "listens": [{ "event": "Y", "triggers": "GOT_Y", "source": { "kind": "trait", "trait": "B" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "GOT_Y", "effects": fetch }] } }
            ]
        }]
    }))
    .unwrap()
}

fn client_cascade_mount(server_c: bool, order: &[&str]) -> orbital_core_runtime::embed::wire::MountWire {
    let k = EmbeddedKernel::new(client_cascade_schema(server_c), "c1").unwrap();
    k.mount(order.iter().map(|t| t.to_string()).collect());
    let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
    k.dispatch_mount(&seeds, None, Value::Null).unwrap()
}

/// EXEC-DIST-2 + EXEC-DIST-10: a server trait a mount seed's client-only cascade reaches leaves as a leg.
#[test]
fn a_mount_seeds_client_only_cascade_that_reaches_a_server_trait_posts_its_leg() {
    let orders: [&[&str]; 6] = [&["A", "B", "C"], &["C", "B", "A"], &["A", "C", "B"], &["A"], &["A", "B"], &["A", "C"]];
    for order in orders {
        let mount = client_cascade_mount(true, order);
        let targets: Vec<Option<String>> = mount
            .stateful_legs
            .iter()
            .chain(mount.legs.iter())
            .flat_map(|leg| leg.seeds.iter().map(|s| Some(s.trait_name.clone())))
            .collect();
        assert!(!mount.legs.is_empty(), "{order:?}: no leg posted ({targets:?})");
        let posted: Vec<&orbital_core_runtime::effects::response::EventRequest> = mount.legs.iter().map(|l| &l.leg).collect();
        let reaches_c = mount
            .seeds
            .iter()
            .flat_map(|s| s.server_legs.iter())
            .any(|r| r.leg.target_trait.as_deref() == Some("C") && r.leg.payload.as_ref().and_then(|p| p.get_path("v")).is_some());
        assert!(reaches_c, "{order:?}: no leg for C with its payload ({posted:?})");
    }
}

#[test]
fn control_a_mount_seeds_client_only_cascade_that_reaches_no_server_trait_posts_nothing() {
    let orders: [&[&str]; 4] = [&["A", "B", "C"], &["C", "B", "A"], &["A"], &["A", "B"]];
    for order in orders {
        let mount = client_cascade_mount(false, order);
        assert!(mount.legs.is_empty(), "{order:?}: {:?}", mount.legs);
        assert!(mount.seeds.iter().all(|s| s.server_legs.is_empty()));
    }
}

/// The launchpad ci-status shape: `Board` (`[client]`) INIT emits LOAD; `Status` (`[client]`) idle->loading emits
/// START (and SHOWN back to Board); `Retry` (optimistic, no server effect) answers START with ATTEMPT; `Status`
/// loading->loading on ATTEMPT emits ASK to `Gh` (server), which fetches.
fn client_retry_schema(server: bool) -> OrbitalSchema {
    let fetch: Vec<serde_json::Value> = if server { vec![json!(["fetch", "Person", {}])] } else { vec![] };
    serde_json::from_value(json!({
        "name": "retrying",
        "typeContractVersion": 1, "orbitals": [{
            "name": "People",
            "entity": { "name": "Person", "persistence": "persistent", "collection": "people",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Board", "dispatch": "client",
                  "listens": [{ "event": "SHOWN", "triggers": "BOARD_SHOWN", "source": { "kind": "trait", "trait": "Status" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "LOAD", {}]] },
                        { "from": "idle", "to": "idle", "event": "BOARD_SHOWN" }] } },
                { "name": "Status", "dispatch": "client",
                  "listens": [
                    { "event": "LOAD", "triggers": "STATUS_LOAD", "source": { "kind": "trait", "trait": "Board" } },
                    { "event": "ATTEMPT", "triggers": "STATUS_ATTEMPT", "source": { "kind": "trait", "trait": "Retry" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "loading" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "loading", "event": "STATUS_LOAD", "effects": [["emit", "START", {}], ["emit", "SHOWN", {}]] },
                        { "from": "loading", "to": "loading", "event": "STATUS_ATTEMPT", "effects": [["emit", "ASK", {"q": "x"}]] }] } },
                { "name": "Retry",
                  "listens": [{ "event": "START", "triggers": "RETRY_START", "source": { "kind": "trait", "trait": "Status" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "RETRY_START", "effects": [["emit", "ATTEMPT", {}]] }] } },
                { "name": "Gh", "linkedEntity": "Person",
                  "listens": [{ "event": "ASK", "triggers": "GH_ASK", "source": { "kind": "trait", "trait": "Status" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "GH_ASK", "effects": fetch }] } }
            ]
        }]
    }))
    .unwrap()
}

fn client_retry_mount(server: bool, order: &[&str]) -> orbital_core_runtime::embed::wire::MountWire {
    let k = EmbeddedKernel::new(client_retry_schema(server), "c1").unwrap();
    k.mount(order.iter().map(|t| t.to_string()).collect());
    let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
    k.dispatch_mount(&seeds, None, Value::Null).unwrap()
}

#[test]
fn a_mount_cascade_through_a_client_trait_and_an_optimistic_retry_posts_the_server_leg() {
    let orders: [&[&str]; 4] = [&["Board"], &["Board", "Status", "Retry", "Gh"], &["Gh", "Retry", "Status", "Board"], &["Board", "Gh"]];
    for order in orders {
        let mount = client_retry_mount(true, order);
        let posted: Vec<String> = mount.legs.iter().flat_map(|l| l.seeds.iter().map(|s| s.trait_name.clone())).collect();
        let to_gh = mount.seeds.iter().flat_map(|s| s.server_legs.iter()).any(|r| r.leg.target_trait.as_deref() == Some("Gh") && r.leg.payload.as_ref().and_then(|p| p.get_path("q")).is_some());
        assert!(to_gh && !mount.legs.is_empty(), "{order:?}: leg to Gh {to_gh}, posted {posted:?}");
    }
}

#[test]
fn control_a_mount_cascade_through_a_client_trait_and_a_retry_with_no_server_trait_posts_nothing() {
    for order in [&["Board"][..], &["Board", "Status", "Retry", "Gh"]] {
        let mount = client_retry_mount(false, order);
        assert!(mount.legs.is_empty(), "{order:?}: {:?}", mount.legs);
    }
}

/// `client_retry_schema` with `Gh` an optimistic client of `Http` (the server trait that fetches), as `GithubClient` is of
/// `GithubHttp`: the ask reaches the server one emit later.
fn client_retry_two_hop_schema(server: bool) -> OrbitalSchema {
    let mut schema = serde_json::to_value(client_retry_schema(false)).unwrap();
    let fetch: Vec<serde_json::Value> = if server { vec![json!(["fetch", "Person", {}])] } else { vec![] };
    let traits = schema["orbitals"][0]["traits"].as_array_mut().unwrap();
    let gh = traits.iter_mut().find(|t| t["name"] == "Gh").unwrap();
    gh["stateMachine"]["transitions"][1]["effects"] = json!([["emit", "SEND", {"q": "x"}]]);
    traits.push(json!({ "name": "Http", "linkedEntity": "Person",
        "listens": [{ "event": "SEND", "triggers": "HTTP_SEND", "source": { "kind": "trait", "trait": "Gh" } }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "idle", "event": "HTTP_SEND", "effects": fetch }] } }));
    for t in schema["orbitals"][0]["traits"].as_array_mut().unwrap() {
        if matches!(t["name"].as_str(), Some("Gh" | "Http" | "Retry")) {
            t["dispatch"] = json!("optimistic");
        }
    }
    serde_json::from_value(schema).unwrap()
}

fn permutations<'a>(items: &[&'a str]) -> Vec<Vec<&'a str>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    (0..items.len())
        .flat_map(|i| {
            let mut rest = items.to_vec();
            let head = rest.remove(i);
            permutations(&rest).into_iter().map(move |mut tail| {
                tail.insert(0, head);
                tail
            })
        })
        .collect()
}

#[test]
fn a_mount_cascade_that_reaches_the_server_trait_through_an_optimistic_client_posts_the_leg() {
    let mut orders = permutations(&["Board", "Status", "Retry", "Gh", "Http"]);
    orders.push(vec!["Board"]);
    let mut silent = Vec::new();
    for order in &orders {
        let k = EmbeddedKernel::new(client_retry_two_hop_schema(true), "c1").unwrap();
        k.mount(order.iter().map(|t| t.to_string()).collect());
        let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
        let mount = k.dispatch_mount(&seeds, None, Value::Null).unwrap();
        let reaches_server = mount
            .seeds
            .iter()
            .flat_map(|s| s.server_legs.iter())
            .any(|r| matches!(r.leg.target_trait.as_deref(), Some("Gh" | "Http")));
        if !reaches_server || mount.legs.is_empty() || mount.stateful_legs.is_empty() {
            silent.push(format!("{} (legs {}, stateful {})", order.join(","), mount.legs.len(), mount.stateful_legs.len()));
        }
    }
    assert!(silent.is_empty(), "no leg to the server cascade for: {silent:?}");
}

#[test]
fn control_a_mount_cascade_through_an_optimistic_client_with_no_server_trait_posts_nothing() {
    for order in [&["Status", "Board", "Gh", "Http", "Retry"][..], &["Board"]] {
        let k = EmbeddedKernel::new(client_retry_two_hop_schema(false), "c1").unwrap();
        k.mount(order.iter().map(|t| t.to_string()).collect());
        let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
        let mount = k.dispatch_mount(&seeds, None, Value::Null).unwrap();
        assert!(mount.legs.is_empty(), "{order:?}: {:?}", mount.legs);
    }
}

/// The launchpad finder shape: `Find` (`[client]`) INIT emits MATCH; `Match` (server, no server effect of its own)
/// hears it and emits SOURCES; `Sources` (optimistic) answers SOURCES with an `os/getenv` ask (or, with `ask` off, a
/// pure set).
fn client_to_pure_server_hop_schema(ask: bool) -> OrbitalSchema {
    let sources: Vec<serde_json::Value> = if ask {
        vec![json!(["os/getenv", "HOME", { "key": "HOME", "emit": { "success": "SRC_ENV", "failure": "SRC_FAILED" } }])]
    } else {
        vec![json!(["set", "@entity.n", 1])]
    };
    serde_json::from_value(json!({
        "name": "finder",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Finder",
            "entity": { "name": "Run", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
            "traits": [
                { "name": "Find", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [{ "from": "ready", "to": "ready", "event": "INIT", "effects": [["emit", "MATCH", {"q": "ci"}]] }] } },
                { "name": "Match", "linkedEntity": "Run",
                  "listens": [{ "event": "MATCH", "triggers": "MATCH", "source": { "kind": "trait", "trait": "Find" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "sourcing" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "sourcing", "event": "MATCH", "effects": [["emit", "SOURCES", {}]] }] } },
                { "name": "Sources", "linkedEntity": "Run", "dispatch": "optimistic",
                  "listens": [{ "event": "SOURCES", "triggers": "SOURCES", "source": { "kind": "trait", "trait": "Match" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "envs" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "envs", "event": "SOURCES", "effects": sources },
                        { "from": "envs", "to": "envs", "event": "SRC_ENV" },
                        { "from": "envs", "to": "idle", "event": "SRC_FAILED" }] } }
            ]
        }]
    }))
    .unwrap()
}

/// EXEC-DIST-2 + EXEC-DIST-10: the host runs the cascade a client-only INIT starts through a server hop to an `os` ask:
/// the hop is the leg with its payload, or, when the hop and the asker run in later seeds, the client-only seed is
/// replayed with the mount.
#[test]
fn a_client_init_that_reaches_a_server_hop_before_an_os_ask_posts_the_hop_as_the_leg() {
    let mut wrong = Vec::new();
    for order in permutations(&["Find", "Match", "Sources"]) {
        let k = EmbeddedKernel::new(client_to_pure_server_hop_schema(true), "c1").unwrap();
        k.mount(order.iter().map(|t| t.to_string()).collect());
        let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
        let mount = k.dispatch_mount(&seeds, None, Value::Null).unwrap();
        let targets: Vec<Option<String>> = mount.seeds.iter().flat_map(|s| s.server_legs.iter()).map(|r| r.leg.target_trait.clone()).collect();
        let to_match = mount
            .seeds
            .iter()
            .flat_map(|s| s.server_legs.iter())
            .any(|r| r.leg.target_trait.as_deref() == Some("Match") && r.leg.payload.as_ref().and_then(|p| p.get_path("q")).is_some());
        let seeds_of = |legs: &[orbital_core_runtime::embed::wire::MountLegWire]| -> Vec<String> {
            legs.iter().flat_map(|l| l.seeds.iter().map(|s| s.trait_name.clone())).collect()
        };
        let (stateful, carried) = (seeds_of(&mount.stateful_legs), seeds_of(&mount.legs));
        let replays_find = stateful.contains(&"Find".to_string()) && carried.contains(&"Find".to_string());
        if !to_match && !replays_find {
            wrong.push(format!("{}: legs {targets:?}, stateful seeds {stateful:?}, carried seeds {carried:?}", order.join(",")));
        }
    }
    assert!(wrong.is_empty(), "the server hop is not the leg: {wrong:#?}");
}

#[test]
fn control_a_client_init_whose_server_hop_chain_does_pure_work_posts_no_leg() {
    for order in permutations(&["Find", "Match", "Sources"]) {
        let k = EmbeddedKernel::new(client_to_pure_server_hop_schema(false), "c1").unwrap();
        k.mount(order.iter().map(|t| t.to_string()).collect());
        let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
        let mount = k.dispatch_mount(&seeds, None, Value::Null).unwrap();
        assert!(mount.seeds.iter().all(|s| s.server_legs.is_empty()), "{order:?}: {:?}", mount.legs);
    }
}


/// EXEC-CONC-8: the mount's answer folds in flight order under its group's oldest flight, as the web host posts it; a
/// seed the batch opened a flight for and posts in no group would hold that fold forever.
#[test]
fn a_mount_group_answer_folds_when_every_flight_the_batch_opened_is_in_a_group() {
    let mut stuck = Vec::new();
    for order in permutations(&["Find", "Match", "Sources"]) {
        let k = EmbeddedKernel::new(client_to_pure_server_hop_schema(true), "c1").unwrap();
        k.mount(order.iter().map(|t| t.to_string()).collect());
        let seeds: Vec<MountSeed> = order.iter().map(|t| seed(t)).collect();
        let mount = k.dispatch_mount(&seeds, None, Value::Null).unwrap();
        let grouped: HashSet<String> = mount.stateful_legs.iter().flat_map(|l| l.seeds.iter().map(|s| s.trait_name.clone())).collect();
        let orphans: Vec<String> = mount
            .seeds
            .iter()
            .filter(|w| w.flight.is_some() && !grouped.contains(&w.seed.trait_name))
            .map(|w| format!("{} (flight {:?})", w.seed.trait_name, w.flight))
            .collect();
        if !orphans.is_empty() {
            stuck.push(format!("{}: {orphans:?}", order.join(",")));
        }
    }
    assert!(stuck.is_empty(), "flights no group ends: {stuck:#?}");
}
