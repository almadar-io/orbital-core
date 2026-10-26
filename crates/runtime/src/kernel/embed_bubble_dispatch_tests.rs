use super::*;
use crate::entity_source::InMemoryEntityCache;
use orbital_core_base::schema::types::ListenSource;

/// Mirrors the real std-notes reproduction named in the §158 gap:
/// `InlineButtonRender3` embedded by `NoteCatalog` (which declares
/// `emits CREATE_DRAFT`); `NotePersistor` `listens CREATE_DRAFT` from
/// source `NoteCatalog`. Generic names so the fixture stands on its own,
/// same shape as the real corpus.
fn notes_like_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "InlineButtonRender3",
                    "id": "child-btn",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": []
                    }
                },
                {
                    "name": "NoteCatalog",
                    "id": "host-catalog",
                    "traitEmbedIds": { "createButton": "child-btn" },
                    "emits": [{ "event": "CREATE_DRAFT" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                },
                {
                    "name": "NotePersistor",
                    "linkedEntity": "Note",
                    "listens": [{
                        "event": "CREATE_DRAFT",
                        "triggers": "DO_CREATE_DRAFT",
                        "source": { "kind": "trait", "trait": "NoteCatalog" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "created" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "created", "event": "DO_CREATE_DRAFT" }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// A Child embedded by a Host that has its own scoped listener firing off
/// the SAME seed event, plus two listeners distinguishing "sourced from
    /// the emitting trait itself" vs "sourced from its host" — used to prove
/// an `Effect::Emit` produced inside a transition does NOT bubble past the
/// trait that produced it, only the seed event does.
fn internal_emit_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Child",
                    "id": "child-1",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "SEED",
                              "effects": [["emit", "INTERNAL", {}]] }
                        ]
                    }
                },
                {
                    "name": "Host",
                    "id": "host-1",
                    "traitEmbedIds": { "slot": "child-1" },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                },
                {
                    "name": "ListenerOnChild",
                    "linkedEntity": "Note",
                    "listens": [{
                        "event": "INTERNAL", "triggers": "FIRED_FROM_CHILD",
                        "source": { "kind": "trait", "trait": "Child" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "fired" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "fired", "event": "FIRED_FROM_CHILD" }
                        ]
                    }
                },
                {
                    "name": "ListenerOnHost",
                    "linkedEntity": "Note",
                    "listens": [{
                        "event": "INTERNAL", "triggers": "FIRED_FROM_HOST",
                        "source": { "kind": "trait", "trait": "Host" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "fired" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "fired", "event": "FIRED_FROM_HOST" }
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
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

/// The §158 reproduction, pre-fix: `dispatch` only fanned out from the
/// seed trait itself, so `CREATE_DRAFT` sent to the embedded
/// `InlineButtonRender3` never reached `NotePersistor` (source
/// `NoteCatalog`) — this assertion fails on the pre-bubble `dispatch`.
#[test]
fn dispatch_bubbles_embedded_emit_to_host_listener() {
    let kernel = kernel_with(notes_like_schema());
    let outcomes = kernel
        .dispatch(
            "InlineButtonRender3",
            None,
            "CREATE_DRAFT",
            Value::Object(Default::default()),
        )
        .unwrap();
    let persistor = outcomes
        .iter()
        .find(|(t, _)| t == "NotePersistor")
        .expect("NotePersistor (listens CREATE_DRAFT from NoteCatalog) must fire when the embedded button dispatches CREATE_DRAFT");
    assert_eq!(persistor.1.new_state, "created");
}

/// A trait with no ancestors must dispatch byte-identically to the
/// pre-bubble single-scope path: one outcome, the target trait only.
#[test]
fn dispatch_unembedded_trait_is_single_scope() {
    let kernel = kernel_with(notes_like_schema());
    let outcomes = kernel
        .dispatch("NoteCatalog", None, "INIT", Value::Null)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].0, "NoteCatalog");
}

#[test]
fn dispatch_does_not_bubble_internal_emit_effects() {
    let kernel = kernel_with(internal_emit_schema());
    let outcomes = kernel
        .dispatch("Child", None, "SEED", Value::Object(Default::default()))
        .unwrap();
    assert!(
        outcomes
            .iter()
            .any(|(t, r)| t == "ListenerOnChild" && r.new_state == "fired"),
        "the emitting trait's own scope must still receive its Effect::Emit: {:?}",
        outcomes
            .iter()
            .map(|(t, r)| (t.clone(), r.new_state.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        !outcomes
            .iter()
            .any(|(t, r)| t == "ListenerOnHost" && r.new_state == "fired"),
        "an internal Effect::Emit must not bubble past its emitting trait's own scope: {:?}",
        outcomes
            .iter()
            .map(|(t, r)| (t.clone(), r.new_state.clone()))
            .collect::<Vec<_>>()
    );
}

/// The Rust twin of `audit_listens` "via host-transition": for every
/// declared `listens` whose source S declares (or whose descendant
/// declares, transitively — but here checked at S itself, the common
/// authoring shape where the composing host carries the `emits`
/// metadata for a primitive it embeds) `emits` of that event, dispatching
/// the event from EVERY trait embedded (directly or transitively) under S
/// must realize the listener in `dispatch`'s outcomes.
#[test]
fn embed_bubble_realizes_every_declared_host_transition_listener() {
    let schema = notes_like_schema();
    let kernel = kernel_with(schema.clone());

    let trait_definitions: HashMap<String, TraitDefinition> = schema.orbitals[0]
        .traits
        .iter()
        .filter_map(|t| match t {
            TraitReference::Inline(def) => Some((def.name.clone(), def.clone())),
            _ => None,
        })
        .collect();
    let embed_index = EmbedIndex::build(&trait_definitions);

    let mut checked = 0;
    for listener_def in trait_definitions.values() {
        let Some(listens) = &listener_def.listens else {
            continue;
        };
        for l in listens {
            let Some(ListenSource::Trait {
                trait_name: source, ..
            }) = &l.source
            else {
                continue;
            };
            let source_emits_event = trait_definitions
                .get(source)
                .and_then(|d| d.emits.as_ref())
                .is_some_and(|es| es.iter().any(|e| e.event == l.event));
            if !source_emits_event {
                continue;
            }
            for candidate in trait_definitions.keys() {
                if !embed_index.ancestors(candidate).iter().any(|a| a == source) {
                    continue;
                }
                checked += 1;
                let outcomes = kernel
                    .dispatch(candidate, None, &l.event, Value::Null)
                    .unwrap();
                assert!(
                    outcomes.iter().any(|(t, _)| t == &listener_def.name),
                    "{} (listens {} from {source}) must fire when embedded emitter {candidate} dispatches {}",
                    listener_def.name, l.event, l.event
                );
            }
        }
    }
    assert!(
        checked > 0,
        "fixture produced no host-transition-listener pairs to check"
    );
}

/// `Child` is embedded by `Host` (directly, or through `Mid` when `via_mid`); `Host` handles
/// `REQUEST` from `idle` AND from `open`, so a second delivery is a real second transition
/// (the seed run moves `idle -> open`, which changes the cascade's visit key). `route` is the
/// host's `listens` array; `Other` is an unembedded sibling that can emit too.
fn double_delivery_schema(route: serde_json::Value, via_mid: bool) -> OrbitalSchema {
    let host_embed = if via_mid { "mid-1" } else { "child-1" };
    let mut traits = vec![
        serde_json::json!({
            "name": "Child", "id": "child-1",
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
        }),
        serde_json::json!({
            "name": "Other", "id": "other-1",
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
        }),
        serde_json::json!({
            "name": "Host", "id": "host-1",
            "traitEmbedIds": { "slot": host_embed },
            "listens": route,
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                "transitions": [
                    { "from": "idle", "to": "open", "event": "REQUEST" },
                    { "from": "open", "to": "open", "event": "REQUEST" },
                    { "from": "open", "to": "open", "event": "ADMIT" }
                ]
            }
        }),
    ];
    if via_mid {
        traits.push(serde_json::json!({
            "name": "Mid", "id": "mid-1",
            "traitEmbedIds": { "slot": "child-1" },
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
        }));
    }
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits
        }]
    }))
    .unwrap()
}

fn route(source: &str, event: &str, triggers: &str) -> serde_json::Value {
    serde_json::json!([{
        "event": event, "triggers": triggers,
        "source": { "kind": "trait", "trait": source }
    }])
}

fn deliveries(outcomes: &[(String, ProcessEventResult)], trait_name: &str) -> usize {
    outcomes.iter().filter(|(t, _)| t == trait_name).count()
}

/// G-RUNTIME-053: the embedded child's action reaches `Host` as its own scope on the embed
/// chain AND through `Host`'s same-name listen route; one physical event must run `Host` once.
#[test]
fn same_name_route_from_embedded_child_delivers_once() {
    let kernel = kernel_with(double_delivery_schema(
        route("Child", "REQUEST", "REQUEST"),
        false,
    ));
    let outcomes = kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
    let host = outcomes.iter().find(|(t, _)| t == "Host").unwrap();
    assert_eq!(host.1.new_state, "open");
}

#[test]
fn same_name_route_through_a_transitive_embed_delivers_once() {
    let kernel = kernel_with(double_delivery_schema(
        route("Child", "REQUEST", "REQUEST"),
        true,
    ));
    let outcomes = kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
}

/// Control: no route, the host transition alone is one delivery.
#[test]
fn embedded_child_without_route_delivers_once() {
    let kernel = kernel_with(double_delivery_schema(serde_json::json!([]), false));
    let outcomes = kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 1);
}

/// Control: a route from a trait the host does NOT embed is the only path, so it must keep firing.
#[test]
fn route_from_unembedded_source_still_delivers() {
    let kernel = kernel_with(double_delivery_schema(
        route("Other", "REQUEST", "REQUEST"),
        false,
    ));
    let outcomes = kernel
        .dispatch("Other", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
}

/// Control: a route to a DIFFERENT trigger name is a distinct delivery, not a duplicate.
#[test]
fn differently_named_route_from_embedded_child_is_a_second_delivery() {
    let kernel = kernel_with(double_delivery_schema(
        route("Child", "REQUEST", "ADMIT"),
        false,
    ));
    let outcomes = kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 2, "{outcomes:?}");
}

/// Edge: the dedup is per physical event — a second dispatch delivers once more.
#[test]
fn dedup_does_not_leak_across_dispatches() {
    let kernel = kernel_with(double_delivery_schema(
        route("Child", "REQUEST", "REQUEST"),
        false,
    ));
    kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    let second = kernel
        .dispatch("Child", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&second, "Host"), 1);
}

/// Edge: a dispatch seeded on the host itself is unaffected by a route from its own child.
#[test]
fn host_seeded_dispatch_delivers_once() {
    let kernel = kernel_with(double_delivery_schema(
        route("Child", "REQUEST", "REQUEST"),
        false,
    ));
    let outcomes = kernel
        .dispatch("Host", None, "REQUEST", Value::Null)
        .unwrap();
    assert_eq!(deliveries(&outcomes, "Host"), 1);
}
