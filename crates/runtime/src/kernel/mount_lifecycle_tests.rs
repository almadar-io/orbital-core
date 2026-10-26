//! A trait never receives a routed event before its own lifecycle event has
//! run on the current mount (riya, 2026-09-24: SineLevelData's INIT seeded
//! the body's skate curve before SineBody's own INIT reset it). JS twin:
//! `evaluation/mount-lifecycle.ts`.
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Boot",
        "orbitals": [{
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
    }))
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
        "_awaitingInit".to_string(),
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
