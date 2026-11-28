//! A trait never receives a routed event before its own lifecycle event has
//! run on the current mount (riya, 2026-09-24: SineLevelData's INIT seeded
//! the body's skate curve before SineBody's own INIT reset it). JS twin:
//! `evaluation/mount-lifecycle.ts`.
use orbital_core_base::json_value::from_json_value;
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&(serde_json::json!({
        "name": "Boot",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Level",
            "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Seeder",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT",
                              "effects": [["emit", "SEED", { "v": 1, "echo": "@payload" }]] },
                            { "from": "idle", "to": "idle", "event": "PING" }
                        ]
                    }
                },
                {
                    "name": "Body",
                    "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }, { "name": "seeded" }],
                        "transitions": [
                            { "from": "idle", "to": "ready", "event": "INIT" },
                            { "from": "ready", "to": "seeded", "event": "SET" }
                        ]
                    }
                }
            ]
        }]
    })))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn body_state(kernel: &RuntimeKernel) -> String {
    // No row yet = never touched = its initial state.
    kernel
        .get_state("Body", None)
        .map_or_else(|| "idle".to_string(), |s| s.current_state)
}

fn seeds(list: &[(&str, &str)]) -> Vec<crate::effects::response::MountSeed> {
    list.iter()
        .map(|(t, e)| crate::effects::response::MountSeed {
            r#trait: t.to_string(),
            event: crate::effects::response::LifecycleEvent::parse(e)
                .expect("a lifecycle event"),
        })
        .collect()
}

/// JS twin: `evaluate-mount-batch.test.ts` — one mount request runs each seed's own
/// lifecycle event in order; the held SEED reaches Body once Body's own INIT ran.
#[test]
fn a_mount_batch_runs_every_seed_in_order_and_keeps_the_lifecycle_hold() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    let outcomes = kernel
        .dispatch_mount(
            &seeds(&[("Seeder", "INIT"), ("Body", "INIT")]),
            None,
            Value::Null,
        )
        .unwrap();
    let ran: Vec<String> = outcomes
        .iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, r)| format!("{t}:{}", r.event.clone().unwrap_or_default()))
        .collect();
    assert_eq!(ran, vec!["Seeder:INIT", "Body:INIT", "Body:SET"]);
    assert_eq!(body_state(&kernel), "seeded");
}

/// A server leg's `_awaitingInit` is synced once against EVERY seed: a later seed's
/// held delivery is not released before its own INIT runs here.
#[test]
fn a_mount_batch_syncs_the_clients_mount_against_every_seed() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    kernel
        .dispatch_mount(
            &seeds(&[("Seeder", "INIT"), ("Body", "INIT")]),
            None,
            leg(&[]),
        )
        .unwrap();
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn control_an_unknown_seed_is_skipped_and_the_others_still_run() {
    let kernel = kernel();
    let outcomes = kernel
        .dispatch_mount(
            &seeds(&[("Nowhere", "INIT"), ("Body", "INIT")]),
            None,
            Value::Null,
        )
        .unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "Body" && r.executed));
    assert!(!outcomes.iter().any(|(t, _)| t == "Nowhere"));
}

#[test]
fn a_seed_waits_for_the_receivers_own_init() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    kernel
        .dispatch("Seeder", None, "INIT", Value::Null)
        .unwrap();
    assert_eq!(
        body_state(&kernel),
        "idle",
        "held, not delivered before Body's INIT"
    );
    kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "seeded");
}

/// A server host follows the client's mount from each leg's
/// `_awaitingInit` (JS twin: `riya-level-seed-reaches-the-body.test.ts`).
fn leg(awaiting: &[&str]) -> Value {
    Value::object(ValueMap::from_iter([(
        "_awaitingInit".into(),
        Value::Array(
            awaiting
                .iter()
                .map(|t| Value::String(t.to_string().into()))
                .collect(),
        ),
    )]))
}

#[test]
fn a_server_leg_holds_a_seed_for_a_trait_the_client_still_awaits() {
    let kernel = kernel();
    kernel
        .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
        .unwrap();
    assert_eq!(body_state(&kernel), "idle");
    kernel
        .dispatch_continuing("Body", None, "INIT", leg(&[]), None, None)
        .unwrap();
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn a_server_leg_releases_what_the_client_has_since_initialized() {
    let kernel = kernel();
    kernel
        .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
        .unwrap();
    // Body's INIT stayed client-side; the next leg says nothing awaits.
    kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    kernel
        .dispatch_continuing("Seeder", None, "PING", leg(&[]), None, None)
        .unwrap();
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn the_sidecar_never_reaches_payload_bindings() {
    let kernel = kernel();
    let out = kernel
        .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
        .unwrap();
    let blob = format!(
        "{:?}",
        out.iter()
            .map(|(_, r)| &r.effect_results)
            .collect::<Vec<_>>()
    );
    assert!(
        blob.contains("echo"),
        "the seed emit echoes @payload: {blob}"
    );
    assert!(!blob.contains("_awaitingInit"), "{blob}");
}

#[test]
fn control_without_a_declared_mount_nothing_is_held() {
    let kernel = kernel();
    kernel
        .dispatch("Seeder", None, "INIT", Value::Null)
        .unwrap();
    kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "ready");
}

#[test]
fn an_unmounted_trait_keeps_nothing_held() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    kernel
        .dispatch("Seeder", None, "INIT", Value::Null)
        .unwrap();
    kernel.unmount_traits(["Body"]);
    kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "ready");
}

fn kernel_with_listener_without_a_lifecycle_arm() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&(serde_json::json!({
        "name": "Boot",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Level",
            "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Seeder", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "SEND", "effects": [["emit", "SEED", {}]] }] } },
                { "name": "Body",
                  "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "seeded" }],
                    "transitions": [{ "from": "idle", "to": "seeded", "event": "SET" }] } }
            ]
        }]
    })))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

/// A trait that handles no lifecycle event has none to await: a mount seeds nothing for it, so holding its
/// deliveries for an INIT that can never run would drop every listen it has.
#[test]
fn a_mounted_trait_with_no_lifecycle_arm_hears_its_listens() {
    let kernel = kernel_with_listener_without_a_lifecycle_arm();
    kernel.remount_traits(["Seeder", "Body"]);
    kernel.dispatch("Seeder", None, "INIT", Value::Null).unwrap();
    kernel.dispatch("Seeder", None, "SEND", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn control_a_mounted_trait_with_a_lifecycle_arm_still_waits_for_it() {
    let kernel = kernel();
    kernel.remount_traits(["Seeder", "Body"]);
    kernel.dispatch("Seeder", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "idle", "Body's delivery is held until its own INIT");
}

fn mount_page(kernel: &RuntimeKernel) -> Vec<String> {
    kernel
        .dispatch_mount(&seeds(&[("Seeder", "INIT"), ("Body", "INIT")]), None, Value::Null)
        .unwrap()
        .iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, r)| format!("{t}:{}", r.event.clone().unwrap_or_default()))
        .collect()
}

/// EXEC-SEED-8: a page entered again runs from the top. Body's INIT has an arm only in its initial state, so a Body
/// left in `seeded` by the earlier visit would answer the new visit's INIT with nothing (Friday's chat, blank on return).
#[test]
fn a_trait_entering_a_mount_again_starts_from_its_initial_state() {
    let kernel = kernel();
    kernel.remount_traits(["Seeder", "Body"]);
    mount_page(&kernel);
    assert_eq!(body_state(&kernel), "seeded");
    kernel.remount_traits(["Seeder", "Body"]);
    assert_eq!(body_state(&kernel), "idle");
    assert_eq!(mount_page(&kernel), vec!["Seeder:INIT", "Body:INIT", "Body:SET"]);
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn a_trait_added_to_the_mount_again_starts_from_its_initial_state() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    mount_page(&kernel);
    kernel.mount_traits(["Body"]);
    assert_eq!(body_state(&kernel), "idle");
}

#[test]
fn control_a_trait_the_new_mount_does_not_name_keeps_its_state() {
    let kernel = kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    mount_page(&kernel);
    kernel.mount_traits(["Seeder"]);
    assert_eq!(body_state(&kernel), "seeded");
}

#[test]
fn edge_every_row_instance_of_an_entering_trait_starts_from_its_initial_state() {
    let kernel = kernel();
    kernel.dispatch("Body", Some("w1"), "INIT", Value::Null).unwrap();
    assert_eq!(kernel.get_state("Body", Some("w1")).map(|s| s.current_state), Some("ready".to_string()));
    kernel.remount_traits(["Body"]);
    assert_eq!(kernel.get_state("Body", Some("w1")).map(|s| s.current_state), None, "recreated at its initial state when next touched");
}

#[test]
fn control_one_session_entering_a_page_leaves_another_sessions_traits_where_they_are() {
    let kernel = kernel();
    let state_in = |session: &str| {
        let _s = kernel.enter_session(session);
        kernel.get_state("Body", None).map_or_else(|| "idle".to_string(), |s| s.current_state)
    };
    for session in ["a", "b"] {
        let _s = kernel.enter_session(session);
        kernel.remount_traits(["Seeder", "Body"]);
        mount_page(&kernel);
    }
    assert_eq!(state_in("a"), "seeded");
    {
        let _s = kernel.enter_session("b");
        kernel.remount_traits(["Seeder", "Body"]);
    }
    assert_eq!(state_in("b"), "idle");
    assert_eq!(state_in("a"), "seeded");
}

fn rail_kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&(serde_json::json!({
        "name": "Rail", "typeContractVersion": 1, "orbitals": [{
            "name": "Rails",
            "entity": { "name": "RailMemory", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" }, { "name": "opened", "type": "boolean", "default": false }
            ] },
            "traits": [{
                "name": "Rail", "linkedEntity": "RailMemory",
                "stateMachine": {
                    "states": [{ "name": "listing", "isInitial": true }],
                    "transitions": [{ "from": "listing", "to": "listing", "event": "INIT",
                        "effects": [["when", ["not", "@entity.opened"], ["do", ["set", "@entity.opened", true], ["emit", "OPENED", {}]]]] }]
                }
            }]
        }]
    })))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn rail_opens_again(kernel: &RuntimeKernel) -> bool {
    let ran = kernel.dispatch("Rail", None, "INIT", Value::Null).unwrap();
    ran.iter().any(|(_, r)| r.effect_results.iter().any(|e| format!("{:?}", e.effect).contains("OPENED")))
}

fn rail_opens(kernel: &RuntimeKernel) -> bool {
    kernel
        .dispatch_mount(&seeds(&[("Rail", "INIT")]), None, Value::Null)
        .unwrap()
        .iter()
        .any(|(_, r)| r.effect_results.iter().any(|e| format!("{:?}", e.effect).contains("OPENED")))
}

/// EXEC-SEED-8: what a trait wrote to its own `@entity` in one visit is gone on the next, as a reloaded page's memory
/// is (Friday's chat rail kept `entryOpened` and opened no conversation on return).
#[test]
fn a_trait_entering_a_mount_again_forgets_what_it_wrote_to_its_own_entity() {
    let kernel = rail_kernel();
    kernel.remount_traits(["Rail"]);
    assert!(rail_opens(&kernel));
    assert!(!rail_opens_again(&kernel), "control: within one visit the flag holds");
    kernel.remount_traits(["Rail"]);
    assert!(rail_opens(&kernel));
}

#[test]
fn control_another_sessions_trait_memory_survives_a_page_entered_elsewhere() {
    let kernel = rail_kernel();
    for session in ["a", "b"] {
        let _s = kernel.enter_session(session);
        kernel.remount_traits(["Rail"]);
        assert!(rail_opens(&kernel));
    }
    {
        let _s = kernel.enter_session("b");
        kernel.remount_traits(["Rail"]);
    }
    let _s = kernel.enter_session("a");
    assert!(!rail_opens_again(&kernel));
}

fn client_request(mount: bool) -> crate::effects::response::EventRequest {
    crate::effects::response::EventRequest {
        event: "INIT".to_string(),
        client_id: Some("tab".to_string()),
        mount: mount.then(|| seeds(&[("Rail", "INIT")])),
        target_trait: (!mount).then(|| "Rail".to_string()),
        ..Default::default()
    }
}

/// EXEC-SEED-8 on the host: a client's page mount reaches the server as a request carrying its mount batch, and the
/// server's own copy of the trait for that session starts from the top too (Friday's chat rail runs on the server).
#[test]
fn a_clients_mount_request_starts_the_hosts_copy_of_its_traits_from_the_top() {
    let kernel = rail_kernel();
    let _s = kernel.enter_session("tab");
    assert!(rail_opens_again(&kernel));
    kernel.seed_carried_request(&client_request(true));
    assert!(rail_opens_again(&kernel));
}

#[test]
fn control_a_clients_ordinary_request_keeps_the_hosts_copy() {
    let kernel = rail_kernel();
    let _s = kernel.enter_session("tab");
    assert!(rail_opens_again(&kernel));
    kernel.seed_carried_request(&client_request(false));
    assert!(!rail_opens_again(&kernel));
}

fn paged_kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&(serde_json::json!({
        "name": "Paged", "typeContractVersion": 1, "orbitals": [{
            "name": "Chat",
            "entity": { "name": "Msg", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Rail", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }] } },
                { "name": "Shell", "stateMachine": { "states": [{ "name": "listing", "isInitial": true }, { "name": "open" }],
                    "transitions": [{ "from": "listing", "to": "open", "event": "OPEN" }] } }
            ],
            "pages": [{ "name": "Chat", "path": "/chat", "traits": [{ "ref": "Rail" }, { "ref": "Shell" }] }]
        }]
    })))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn page_mount_request(route: Option<&str>) -> crate::effects::response::EventRequest {
    crate::effects::response::EventRequest {
        event: "INIT".to_string(),
        client_id: Some("tab".to_string()),
        mount: Some(seeds(&[("Rail", "INIT")])),
        route: route.map(str::to_string),
        ..Default::default()
    }
}

/// EXEC-SEED-8: the host restarts every trait of the page a client's mount opens, not only its seeds: Friday's chat
/// shell is embedded by the layout and has no seed of its own, and stayed `open` with nothing painted on return.
#[test]
fn a_clients_page_mount_starts_every_trait_of_the_page_from_the_top() {
    let kernel = paged_kernel();
    let _s = kernel.enter_session("tab");
    kernel.dispatch("Shell", None, "OPEN", Value::Null).unwrap();
    assert_eq!(kernel.get_state("Shell", None).map(|s| s.current_state), Some("open".to_string()));
    kernel.seed_carried_request(&page_mount_request(Some("/chat")));
    assert_eq!(kernel.get_state("Shell", None).map(|s| s.current_state), None);
}

#[test]
fn control_a_mount_naming_no_route_restarts_only_its_seeds() {
    let kernel = paged_kernel();
    let _s = kernel.enter_session("tab");
    kernel.dispatch("Shell", None, "OPEN", Value::Null).unwrap();
    kernel.seed_carried_request(&page_mount_request(None));
    assert_eq!(kernel.get_state("Shell", None).map(|s| s.current_state), Some("open".to_string()));
}
