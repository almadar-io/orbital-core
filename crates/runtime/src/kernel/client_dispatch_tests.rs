use super::*;
use crate::entity_source::InMemoryEntityCache;
use crate::server_leg::ServerLegCollector;
use {
    orbital_core_base::bus_event::{BusEventSource, EmittedEvent},
    orbital_core_base::client_effect::ClientEffect,
};

/// `Persistor` (PersistedAwaited: no declared `dispatch`)
/// transitions `idle -[CREATE]-> created` via a real `persist` effect —
/// the server-only effect that must reach the `ServerLegCollector`.
fn persisted_awaited_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Persistor",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "created" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "created", "event": "CREATE",
                            "effects": [
                                ["set", "@entity.title", "Hello"],
                                ["persist", "create", "Note", {}]
                            ]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// `LocalToggle` declares `dispatch: "client"` but STILL carries a
/// `persist` effect — a schema no validator should ever let through
/// (`[client]` is only legal where nothing needs the server),
/// reproduced here deliberately to prove the kernel's
/// defensive drop holds even when the collector has something to drain.
fn hybrid_with_stray_persist_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "LocalToggle",
                "linkedEntity": "Note",
                "dispatch": "client",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "created" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "created", "event": "CREATE",
                            "effects": [["persist", "create", "Note", {}]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// `Move` declares `dispatch: "optimistic"` — RuntimeOptimistic. Its `MOVE` transition is a plain `Set`, no
/// server-only effect, so it never produces a leg either (nothing to
/// delegate) but DOES snapshot for optimistic rollback.
fn runtime_optimistic_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Cursor", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }]
            },
            "traits": [{
                "name": "Move",
                "linkedEntity": "Cursor",
                "dispatch": "optimistic",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "moved" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "moved", "event": "MOVE",
                            "effects": [["set", "@entity.x", 5], ["call-service", "telemetry", "move", {}]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// `RenderTrait` has NO linked entity, but its `SHOW` transition renders
/// `@payload.title` into `main` — proves render-ui resolves live in a
/// Client-env kernel (native shells depend on this).
fn render_ui_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "RenderTrait",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "shown" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "shown", "event": "SHOW",
                            "effects": [["render-ui", "main", { "title": "@payload.title" }]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// A listener trait, used by the `apply_server_response` tests: fires
/// `LISTENED` when a server-emitted `PING` (source `Emitter`) is folded.
fn server_response_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Emitter",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                },
                {
                    "name": "Listener",
                    "linkedEntity": "Note",
                    "listens": [{
                        "event": "PING", "triggers": "PONG",
                        "source": { "kind": "trait", "trait": "Emitter" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "ponged" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "ponged", "event": "PONG" }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn client_kernel_with(schema: OrbitalSchema) -> (RuntimeKernel, Arc<ServerLegCollector>) {
    let collector = Arc::new(ServerLegCollector::new());
    let config = KernelConfig {
        environment: RuntimeEnvironment::Client,
        delegate: Some(collector.clone() as Arc<dyn crate::effects::executor::EffectDelegate>),
        ..KernelConfig::default()
    };
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        config,
    );
    kernel.register_schema(schema).unwrap();
    (kernel, collector)
}

fn server_kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn client_env_hybrid_local_trait_never_produces_server_leg() {
    let (kernel, _collector) = client_kernel_with(hybrid_with_stray_persist_schema());
    let dispatch = kernel
        .dispatch_with_server_leg("App", "LocalToggle", None, "CREATE", Value::Null, None)
        .unwrap();
    assert_eq!(dispatch.mode, DispatchMode::HybridClientOnly);
    assert!(
        dispatch.server_legs.is_empty(),
        "a HybridClientOnly trait must never produce a server leg, even when its \
             (deliberately malformed) schema delegates a persist effect"
    );
}

#[test]
fn client_env_delegates_persist_and_collects_server_leg() {
    let (kernel, _collector) = client_kernel_with(persisted_awaited_schema());
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Persistor", None, "CREATE", Value::Null, None)
        .unwrap();
    assert_eq!(dispatch.mode, DispatchMode::PersistedAwaited);
    assert_eq!(dispatch.server_legs.len(), 1);
    let leg = dispatch.server_legs[0].clone();
    assert_eq!(leg.event, "CREATE");
    assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
    let traits = leg.traits.expect("executed transitions");
    assert!(traits
        .iter()
        .any(|t| t.r#trait == "Persistor" && t.from == "idle"));
    // No row before CREATE: the server replays from its own, nothing carried.
    assert!(leg.entity_by_trait.is_none());
}

#[test]
fn a_cascade_leg_names_the_event_each_trait_fired_on() {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Composer", "dispatch": "client", "linkedEntity": "Note", "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [{ "from": "ready", "to": "ready", "event": "SEND", "effects": [["emit", "SAVE", {}]] }] } },
                { "name": "Persistor", "linkedEntity": "Note",
                  "listens": [{ "event": "SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Composer" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE", "effects": [["persist", "create", "Note", {}]] }] } }
            ]
        }]
    }))
    .unwrap();
    let (kernel, _collector) = client_kernel_with(schema);
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
        .unwrap();
    let leg = dispatch
        .server_legs
        .first()
        .expect("the persistor's write owes a leg");
    assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
    assert_eq!(leg.event, "DO_CREATE");
    let traits = leg.traits.clone().expect("executed transitions");
    let event_of = |name: &str| {
        traits
            .iter()
            .find(|t| t.r#trait == name)
            .and_then(|t| t.event.clone())
    };
    assert_eq!(event_of("Composer").as_deref(), Some("SEND"));
    assert_eq!(event_of("Persistor").as_deref(), Some("DO_CREATE"));
}

#[test]
fn a_posted_dispatch_marks_each_non_local_trait_it_fired_as_awaiting() {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Composer", "dispatch": "client", "linkedEntity": "Note", "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [{ "from": "ready", "to": "ready", "event": "SEND", "effects": [["emit", "SAVE", {}]] }] } },
                { "name": "Persistor", "linkedEntity": "Note",
                  "listens": [{ "event": "SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Composer" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "saved" }],
                    "transitions": [{ "from": "idle", "to": "saved", "event": "DO_CREATE", "effects": [["persist", "create", "Note", {}]] }] } }
            ]
        }]
    }))
    .unwrap();
    let (kernel, _collector) = client_kernel_with(schema);
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
        .unwrap();
    assert_eq!(
        dispatch.awaiting,
        vec![AwaitingTrait {
            trait_name: "Persistor".into(),
            event: "DO_CREATE".into(),
            from: "idle".into(),
        }],
        "the local Composer never awaits; the Persistor awaits from the state it left"
    );
}

#[test]
fn control_a_local_dispatch_with_no_leg_awaits_nothing() {
    let (kernel, _collector) = client_kernel_with(hybrid_with_stray_persist_schema());
    let dispatch = kernel
        .dispatch_with_server_leg("App", "LocalToggle", None, "CREATE", Value::Null, None)
        .unwrap();
    assert!(dispatch.awaiting.is_empty());
}

#[test]
fn control_a_trait_entry_without_event_still_deserializes() {
    let entry: TraitFrom =
        serde_json::from_value(serde_json::json!({ "trait": "A", "from": "idle" })).unwrap();
    assert_eq!(entry.event, None);
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        serde_json::json!({ "trait": "A", "from": "idle" })
    );
}

#[test]
fn client_env_runtime_optimistic_snapshot_and_restore() {
    let (kernel, _collector) = client_kernel_with(runtime_optimistic_schema());
    kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
    assert_eq!(
        kernel
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "idle"
    );

    let dispatch = kernel
        .dispatch_with_server_leg("App", "Move", None, "MOVE", Value::Null, None)
        .unwrap();
    assert_eq!(dispatch.mode, DispatchMode::RuntimeOptimistic);
    let snapshot = dispatch
        .snapshot
        .expect("RuntimeOptimistic dispatch must snapshot");
    assert_eq!(
        snapshot.state, "idle",
        "snapshot captured BEFORE the dispatch committed"
    );
    assert!(
        snapshot.entity.is_null(),
        "no entity existed yet before MOVE"
    );
    assert!(
        !dispatch.server_legs.is_empty(),
        "MOVE's own call-service is the server work that makes it optimistic"
    );

    assert_eq!(
        kernel
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "moved"
    );
    let after = kernel.snapshot_trait("Move", None).entity;
    assert_eq!(
        after.as_object().unwrap().get("x"),
        Some(&Value::Number(5.0))
    );

    kernel.restore_trait("Move", None, snapshot);
    assert_eq!(
        kernel
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "idle"
    );
    assert!(
        kernel.snapshot_trait("Move", None).entity.is_null(),
        "rollback must leave the frame absent, matching the pre-dispatch snapshot"
    );
}

#[test]
fn render_ui_runs_locally_in_client_env() {
    let (kernel, _collector) = client_kernel_with(render_ui_schema());
    let mut payload = ValueMap::default();
    payload.insert("title".to_string(), Value::string("Hello"));
    let outcomes = kernel
        .dispatch("RenderTrait", None, "SHOW", Value::object(payload))
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "RenderTrait", "SHOW");
    let effects = response
        .client_effects();
    let render = effects
        .iter()
        .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
        .expect("a render-ui client effect");
    let ClientEffect::RenderUi { config, .. } = render else {
        unreachable!()
    };
    let title = config
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|o| o.get("title"))
        .cloned();
    assert_eq!(
        title,
        Some(Value::string("Hello")),
        "the binding must be RESOLVED (not the literal '@payload.title' token), \
             even though this kernel is Client-env with no render handler configured"
    );
}

/// App-level `theme: "clay-light"`, no orbital-level override — the
/// `std-notes` shape the live `orb dev` gate checks. `ThemeTrait`'s `SHOW`
/// transition renders the bare sigil directly, same as an atom's
/// `data-theme: @currentTheme` binding (`std-app-layout.lolo`).
fn render_ui_theme_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "theme": "clay-light",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "ThemeTrait",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "shown" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "shown", "event": "SHOW",
                            "effects": [["render-ui", "main", { "data-theme": "@currentTheme" }]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

#[test]
fn current_theme_binding_resolves_in_render_ui() {
    let kernel = server_kernel_with(render_ui_theme_schema());
    let outcomes = kernel
        .dispatch("ThemeTrait", None, "SHOW", Value::Null)
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "ThemeTrait", "SHOW");
    let effects = response
        .client_effects();
    let render = effects
        .iter()
        .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
        .expect("a render-ui client effect");
    let ClientEffect::RenderUi { config, .. } = render else {
        unreachable!()
    };
    let theme = config
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|o| o.get("data-theme"))
        .cloned();
    assert_eq!(
        theme,
        Some(Value::string("clay-light")),
        "the @currentTheme sigil must resolve to the declared app-level theme key, \
             not round-trip as the literal '@currentTheme' token"
    );
}

fn pages_schema(extra_shell_pages: serde_json::Value) -> OrbitalSchema {
    let mut shell_pages = vec![
        serde_json::json!({ "name": "HomePage", "path": "/", "traits": [{ "ref": "Layout" }] }),
        serde_json::json!({ "name": "DetailPage", "path": "/items/:id", "label": "Detail", "traits": [] }),
    ];
    shell_pages.extend(extra_shell_pages.as_array().cloned().unwrap_or_default());
    serde_json::from_value(serde_json::json!({
        "name": "SigilApp",
        "typeContractVersion": 1, "orbitals": [
            {
                "name": "ShellOrbital",
                "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Layout",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                            "effects": [["render-ui", "main", { "type": "stack", "items": "@pages" }]] }]
                    }
                }],
                "pages": shell_pages
            },
            {
                "name": "ItemsOrbital",
                "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [],
                "pages": [
                    { "name": "ItemsPage", "path": "/items", "label": "Items", "icon": "list", "roles": ["admin", "staff"], "traits": [] },
                    { "name": "DuplicatePage", "path": "/items", "label": "Items again", "traits": [] }
                ]
            }
        ]
    }))
    .unwrap()
}

fn rendered_items(schema: OrbitalSchema) -> Option<Value> {
    let kernel = server_kernel_with(schema);
    let outcomes = kernel
        .dispatch("Layout", None, "INIT", Value::Null)
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Layout", "INIT");
    response.client_effects().into_iter().find_map(|e| match e {
        ClientEffect::RenderUi { config, .. } => config
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|o| o.get("items"))
            .cloned(),
        _ => None,
    })
}

#[test]
fn pages_binding_resolves_to_the_app_wide_root_nav_entries() {
    let items = rendered_items(pages_schema(serde_json::json!([])));
    let expected: Value = serde_json::from_value(serde_json::json!([
        { "href": "/items", "label": "Items", "icon": "list", "roles": ["admin", "staff"] }
    ]))
    .unwrap();
    assert_eq!(items, Some(expected));
}

#[test]
fn pages_binding_lists_entries_in_orbital_order_without_duplicates_params_or_unlabeled_pages() {
    let items = rendered_items(pages_schema(serde_json::json!([
        { "name": "AboutPage", "path": "/about", "label": "About", "traits": [] }
    ])));
    let hrefs: Vec<String> = items
        .as_ref()
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    e.get_path("href")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(hrefs, ["/about", "/items"]);
}

#[test]
fn a_second_registered_program_adds_its_entries_after_the_first_and_skips_a_listed_path() {
    let kernel = server_kernel_with(pages_schema(serde_json::json!([])));
    let second: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Second",
        "typeContractVersion": 1, "orbitals": [{
            "name": "ExtraOrbital",
            "entity": { "name": "Extra", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [],
            "pages": [
                { "name": "Again", "path": "/items", "label": "Again", "traits": [] },
                { "name": "Zeta", "path": "/zeta", "label": "Zeta", "traits": [] }
            ]
        }]
    }))
    .unwrap();
    kernel.register_schema(second).unwrap();
    let outcomes = kernel
        .dispatch("Layout", None, "INIT", Value::Null)
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Layout", "INIT");
    let hrefs: Vec<String> = response
        .client_effects()
        .into_iter()
        .find_map(|e| match e {
            ClientEffect::RenderUi { config, .. } => config
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|o| o.get("items"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| {
                            i.get_path("href")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect()
                }),
            _ => None,
        })
        .unwrap();
    assert_eq!(hrefs, ["/items", "/zeta"]);
}

#[test]
fn control_a_program_with_no_labeled_root_page_has_no_entries() {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "NoNav",
        "typeContractVersion": 1, "orbitals": [{
            "name": "ShellOrbital",
            "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Layout",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", { "type": "stack", "items": "@pages" }]] }]
                }
            }],
            "pages": [{ "name": "HomePage", "path": "/", "traits": [] }]
        }]
    }))
    .unwrap();
    assert_eq!(rendered_items(schema), Some(Value::array(Vec::new())));
}

fn i18n_schema(locales: bool) -> OrbitalSchema {
    let mut schema = serde_json::json!({
        "name": "Site",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Home",
            "entity": { "name": "HomeView", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "HomePage",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{
                        "from": "idle", "to": "idle", "event": "SHOW",
                        "effects": [["render-ui", "main", {
                            "title": ["i18n/t", "site:hero.title"],
                            "count": ["i18n/t", "site:posts.count", { "n": 3 }],
                            "className": ["if", ["==", ["i18n/t", "site:cta"], ""], "hidden", ""],
                            "lang": "@locale"
                        }]]
                    }]
                }
            }]
        }]
    });
    if locales {
        schema["locales"] = serde_json::json!(["en", "ar"]);
    }
    serde_json::from_value(schema).unwrap()
}

fn i18n_catalogs() -> HashMap<String, HashMap<String, String>> {
    let catalog = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    HashMap::from([
        (
            "en".to_string(),
            catalog(&[
                ("site:hero.title", "Hello"),
                ("site:posts.count", "{{n}} posts"),
                ("site:cta", "Start"),
            ]),
        ),
        (
            "ar".to_string(),
            catalog(&[
                ("site:hero.title", "مرحبا"),
                ("site:posts.count", "{{n}} منشورات"),
                ("site:cta", "ابدأ"),
            ]),
        ),
    ])
}

fn field(config: &Value, key: &str) -> Value {
    config
        .as_object()
        .and_then(|o| o.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

fn rendered_config(kernel: &RuntimeKernel) -> Value {
    let outcomes = kernel
        .dispatch("HomePage", None, "SHOW", Value::Null)
        .unwrap();
    let response = EventResponse::from_process_results(&outcomes, "HomePage", "SHOW");
    let effects = response.client_effects();
    let Some(ClientEffect::RenderUi { config, .. }) = effects
        .iter()
        .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
    else {
        panic!("no render-ui effect");
    };
    config.clone().unwrap_or(Value::Null)
}

#[test]
fn i18n_t_renders_the_dispatch_viewers_locale() {
    let kernel = server_kernel_with(i18n_schema(true));
    kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
    let _locale = kernel.enter_locale(Some("ar")).unwrap();
    let config = rendered_config(&kernel);
    assert_eq!(field(&config, "title"), Value::string("مرحبا"));
    assert_eq!(field(&config, "count"), Value::string("3 منشورات"));
    assert_eq!(field(&config, "className"), Value::string(""));
    assert_eq!(field(&config, "lang"), Value::string("ar"));
}

#[test]
fn control_i18n_t_without_a_viewer_locale_renders_the_first_declared() {
    let kernel = server_kernel_with(i18n_schema(true));
    kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
    let config = rendered_config(&kernel);
    assert_eq!(field(&config, "title"), Value::string("Hello"));
    assert_eq!(field(&config, "lang"), Value::string("en"));
}

#[test]
fn edge_a_locale_the_program_does_not_declare_is_refused() {
    let kernel = server_kernel_with(i18n_schema(true));
    kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
    let err = kernel
        .enter_locale(Some("fr"))
        .err()
        .expect("undeclared locale");
    assert!(err.to_string().contains("\"fr\""), "{err}");
}

#[test]
fn edge_a_program_without_locales_accepts_any_viewer_locale() {
    let kernel = server_kernel_with(i18n_schema(false));
    assert!(kernel.enter_locale(Some("en")).is_ok());
}

#[test]
fn apply_server_response_merges_data_and_states() {
    let kernel = server_kernel_with(server_response_schema());
    let mut row = ValueMap::default();
    row.insert("id".to_string(), Value::string("n-1"));
    row.insert(
        "title".to_string(),
        Value::string("From server"),
    );
    let mut data = HashMap::new();
    data.insert("Note".to_string(), vec![Value::object(row)]);

    let mut entity_by_trait = ValueMap::default();
    let mut listener_entity = ValueMap::default();
    listener_entity.insert("id".to_string(), Value::string("singleton"));
    listener_entity.insert("note".to_string(), Value::string("n-1"));
    entity_by_trait.insert("Listener".to_string(), Value::object(listener_entity));

    let mut states = HashMap::new();
    states.insert("Listener".to_string(), "ponged".to_string());

    let response = EventResponse {
        success: true,
        transitioned: true,
        states,
        emitted_events: Vec::new(),
        data: Some(data),
        entity_by_trait: Some(entity_by_trait),
        ..Default::default()
    };

    kernel
        .apply_server_response(&response, &HashSet::new())
        .unwrap();

    let note = kernel.entity_source().read("Note", "n-1").unwrap();
    assert_eq!(
        note.as_object().unwrap().get("title"),
        Some(&Value::string("From server"))
    );
    let listener_frame = kernel.snapshot_trait("Listener", None).entity;
    assert_eq!(
        listener_frame.as_object().unwrap().get("note"),
        Some(&Value::string("n-1"))
    );
    assert_eq!(
        kernel
            .get_state("Listener", None)
            .unwrap()
            .current_state,
        "ponged"
    );
}

#[test]
fn apply_server_response_dedupes_locally_delivered_echo() {
    let kernel = server_kernel_with(server_response_schema());
    let response = EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::new(),
        emitted_events: vec![EmittedEvent {
            event: "PING".to_string(),
            payload: None,
            source: Some(BusEventSource {
                orbital: Some("Main".to_string()),
                ..BusEventSource::from_trait("Emitter")
            }),
        }],
        ..Default::default()
    };

    // Already delivered locally — must NOT re-fire the listener.
    let mut already_delivered = HashSet::new();
    already_delivered.insert(("Emitter".to_string(), "PING".to_string()));
    let outcomes = kernel
        .apply_server_response(&response, &already_delivered)
        .unwrap();
    assert!(
        !outcomes.iter().any(|(t, _)| t == "Listener"),
        "an echo of a locally-delivered event must not re-run its listener"
    );

    // Nothing delivered locally — the listener must fire.
    let outcomes = kernel
        .apply_server_response(&response, &HashSet::new())
        .unwrap();
    assert!(
        outcomes
            .iter()
            .any(|(t, r)| t == "Listener" && r.new_state == "ponged"),
        "an event not already delivered must still cascade through the local listener"
    );
}

#[test]
fn apply_server_response_rolls_back_on_error() {
    let (kernel, _collector) = client_kernel_with(runtime_optimistic_schema());
    kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Move", None, "MOVE", Value::Null, None)
        .unwrap();
    let snapshot = dispatch.snapshot.expect("RuntimeOptimistic snapshot");
    assert_eq!(
        kernel
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "moved"
    );

    // Caller pattern: the (simulated) server round trip errors/disagrees →
    // roll back to the pre-dispatch snapshot.
    kernel.restore_trait("Move", None, snapshot);

    assert_eq!(
        kernel
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "idle"
    );
    assert!(kernel.entity_source().read("Cursor", "singleton").is_err());
}

#[test]
fn already_delivered_from_includes_local_emits_and_seed() {
    let mut result = ProcessEventResult {
        event: None,
        executed: true,
        previous_state: "idle".to_string(),
        new_state: "created".to_string(),
        effects_executed: 1,
        effect_results: vec![EffectResult::executed(Effect::Emit {
            event: "DRAFT_CREATED".to_string(),
            payload: Value::Null,
        })],
        entity_data: SharedEntity::null(),
        linked_entity: "Note".to_string(),
        payload: std::sync::Arc::new(Value::Null),
        entry: None,
        fired_arm: None,
        transition_index: None,
        entity_id: "singleton".into(),
        state_entity_id: "singleton".into(),
        guards: Vec::new(),
        effect_runs: Vec::new(),
        cascade_dropped: Vec::new(),
        cascade_cut: None,
        consumed_emits: Vec::new(),
        failure: None,
        written: SharedEntity::null(),
        callsite_payload: None,
    };
    result.previous_state = "idle".to_string();
    let outcomes = vec![("Persistor".to_string(), result)];
    let response = EventResponse::from_process_results(&outcomes, "Persistor", "CREATE");
    let dispatch = ClientDispatch {
        response,
        server_legs: vec![EventRequest {
            event: "CREATE".to_string(),
            target_trait: Some("Persistor".to_string()),
            ..Default::default()
        }],
        outcomes,
        seed: ("Persistor".to_string(), "CREATE".to_string()),
        mode: DispatchMode::PersistedAwaited,
        snapshot: None,
        awaiting: Vec::new(),
        stateful_leg: None,
    };

    let delivered = already_delivered_from(&dispatch);
    assert!(delivered.contains(&("Persistor".to_string(), "DRAFT_CREATED".to_string())));
    assert!(delivered.contains(&("Persistor".to_string(), "CREATE".to_string())));
}

#[test]
fn kernel_concurrent_dispatch_and_apply_no_deadlock() {
    let kernel = Arc::new(server_kernel_with(server_response_schema()));
    kernel
        .dispatch("Emitter", None, "INIT", Value::Null)
        .unwrap();
    kernel
        .dispatch("Listener", None, "INIT", Value::Null)
        .unwrap();

    let dispatch_kernel = kernel.clone();
    let dispatcher = std::thread::spawn(move || {
        for _ in 0..200 {
            let _ = dispatch_kernel.dispatch("Emitter", None, "PING", Value::Null);
        }
    });

    let apply_kernel = kernel.clone();
    let applier = std::thread::spawn(move || {
        for _ in 0..200 {
            let response = EventResponse {
                success: true,
                emitted_events: vec![EmittedEvent {
                    event: "PING".to_string(),
                    payload: None,
                    source: Some(BusEventSource::from_trait("Emitter")),
                }],
                ..Default::default()
            };
            let _ = apply_kernel.apply_server_response(&response, &HashSet::new());
        }
    });

    dispatcher
        .join()
        .expect("dispatcher thread must not panic or deadlock");
    applier
        .join()
        .expect("applier thread must not panic or deadlock");
}

/// std-realtime-chat's shape: a `local` composer emits SAVE; a persistor
/// listening to it runs the server-bound `persist create`.
fn local_seed_downstream_persist_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Message", "fields": [
                { "name": "id", "type": "string" },
                { "name": "content", "type": "string" }
            ] },
            "traits": [
                {
                    "name": "Composer",
                    "linkedEntity": "Message",
                    "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [
                            { "from": "ready", "to": "ready", "event": "DRAFT",
                              "effects": [["set", "@entity.content", "@payload.value"]] },
                            { "from": "ready", "to": "ready", "event": "SEND",
                              "effects": [["emit", "SAVE", { "content": "@entity.content" }]] }
                        ]
                    }
                },
                {
                    "name": "Persistor",
                    "linkedEntity": "Message",
                    "listens": [{ "event": "SAVE", "source": { "kind": "trait", "trait": "Composer" }, "triggers": "DO_CREATE" }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "DO_CREATE",
                              "effects": [["persist", "create", "Message", { "content": "@payload.content" }]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// A `local` seed never needs the server for its OWN effects, but the
/// traits its cascade reached may; that work is posted, not dropped.
#[test]
fn client_env_local_seed_posts_its_cascade_server_work() {
    let (kernel, _collector) = client_kernel_with(local_seed_downstream_persist_schema());
    kernel
        .dispatch_with_server_leg(
            "App",
            "Composer",
            None,
            "DRAFT",
            Value::object(ValueMap::from_iter([("value".to_string(), Value::from("hi"))])),
            None,
        )
        .unwrap();
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
        .unwrap();
    assert_eq!(dispatch.mode, DispatchMode::HybridClientOnly);
    assert_eq!(dispatch.server_legs.len(), 1);
    let leg = &dispatch.server_legs[0];
    assert_eq!(leg.event, "DO_CREATE");
    assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
}

/// A local `Ticker` whose `beat` tick emits BEAT; `Listener` hears it; `Persistor` hears it
/// and persists. `Ticker` renders on INIT and has `ownArm` (an own BEAT arm) when asked.
fn tick_emit_schema(own_arm: bool) -> OrbitalSchema {
    let mut ticker_transitions = vec![serde_json::json!(
        { "from": "running", "to": "running", "event": "INIT",
          "effects": [["render-ui", "main", { "type": "typography", "content": "tick" }]] })];
    if own_arm {
        ticker_transitions.push(serde_json::json!(
            { "from": "running", "to": "running", "event": "BEAT", "effects": [] }));
    }
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "beats", "type": "number" }
            ] },
            "traits": [
                {
                    "name": "Ticker", "linkedEntity": "Note", "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "running", "isInitial": true }],
                        "transitions": ticker_transitions
                    },
                    "ticks": [
                        { "name": "beat", "interval": 1000,
                          "effects": [["set", "@entity.beats", 1], ["emit", "BEAT", { "n": 1 }]] },
                        { "name": "quiet", "interval": 1000,
                          "effects": [["set", "@entity.beats", 2]] },
                        { "name": "saver", "interval": 1000,
                          "effects": [["persist", "create", "Note", { "beats": 3 }]] },
                        { "name": "never", "interval": 1000,
                          "guard": ["=", 1, 2],
                          "effects": [["emit", "BEAT", {}]] }
                    ]
                },
                {
                    "name": "Listener", "linkedEntity": "Note", "dispatch": "client",
                    "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                        "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }]
                    }
                },
                {
                    "name": "Persistor", "linkedEntity": "Note",
                    "listens": [{ "event": "BEAT", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Ticker" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE",
                            "effects": [["persist", "create", "Note", { "beats": "@payload.n" }]] }]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn booted_ticker(own_arm: bool) -> (RuntimeKernel, Arc<ServerLegCollector>) {
    let (kernel, collector) = client_kernel_with(tick_emit_schema(own_arm));
    kernel
        .dispatch_with_server_leg("App", "Ticker", None, "INIT", Value::Null, None)
        .unwrap();
    collector.drain();
    (kernel, collector)
}

#[test]
fn a_tick_emit_reaches_its_listener_through_the_cascade() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "beat", Some("c1"))
        .unwrap();
    assert!(tick.outcome.executed);
    assert_eq!(tick.emits.len(), 1, "one emit, one cascade");
    let emit = &tick.emits[0].dispatch;
    assert_eq!(
        emit.response.states.get("Listener").map(String::as_str),
        Some("heard")
    );
    assert!(emit.response.transitioned);
}

#[test]
fn the_emitter_is_not_re_seeded_by_its_own_tick_emit() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "beat", None)
        .unwrap();
    let response = &tick.emits[0].dispatch.response;
    assert!(
        response.guard_failed.is_none(),
        "no guardFailed: {:?}",
        response.guard_failed
    );
    assert!(
        !response.states.contains_key("Ticker"),
        "the emitter is not an outcome of its own emit"
    );
    assert!(
        response
            .client_effects().is_empty(),
        "no repaint of the emitter's last render"
    );
}

#[test]
fn control_dispatching_the_emitted_event_as_a_seed_does_reseed_the_emitter() {
    let (kernel, _collector) = booted_ticker(false);
    let seeded = kernel
        .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
        .unwrap();
    assert_eq!(
        seeded.response.guard_failed.as_deref(),
        Some("Ticker.BEAT"),
        "the old re-entry path reports the spurious guardFailed"
    );
    assert!(
        !seeded.response.client_effects().is_empty(),
        "and repaints the emitter"
    );
}

#[test]
fn an_emitter_with_its_own_arm_for_the_event_still_runs_it() {
    let (kernel, _collector) = booted_ticker(true);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "beat", None)
        .unwrap();
    let response = &tick.emits[0].dispatch.response;
    assert_eq!(
        response.states.get("Ticker").map(String::as_str),
        Some("running")
    );
    assert!(
        response.transitioned,
        "the same-trait cascade ran the own BEAT arm"
    );
}

#[test]
fn a_listener_s_persist_yields_a_tick_stamped_server_leg() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "beat", Some("c1"))
        .unwrap();
    let emit = &tick.emits[0].dispatch;
    assert_eq!(emit.server_legs.len(), 1);
    let leg = &emit.server_legs[0];
    assert_eq!(leg.event, "DO_CREATE");
    assert_eq!(leg.tick.as_deref(), Some("beat"));
    assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
    assert_eq!(leg.client_id.as_deref(), Some("c1"));
    assert_eq!(emit.mode, DispatchMode::HybridClientOnly);
}

#[test]
fn a_tick_cascade_leg_awaits_nothing_but_the_same_cascade_dispatched_does() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "beat", None)
        .unwrap();
    assert!(
        tick.emits[0].dispatch.awaiting.is_empty(),
        "tick legs are latest-state broadcasts"
    );
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
        .unwrap();
    assert!(
        dispatch
            .awaiting
            .iter()
            .any(|a| a.trait_name == "Persistor" && a.event == "DO_CREATE"),
        "control: {:?}",
        dispatch.awaiting
    );
}

#[test]
fn control_a_dispatch_leg_carries_no_tick_stamp() {
    let (kernel, _collector) = booted_ticker(false);
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
        .unwrap();
    let leg = dispatch
        .server_legs
        .first()
        .expect("same cascade, same leg");
    assert!(leg.tick.is_none());
}

#[test]
fn a_tick_without_an_emit_has_no_cascade_and_no_leg() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "quiet", None)
        .unwrap();
    assert!(tick.outcome.executed);
    assert!(tick.emits.is_empty());
    assert_eq!(
        tick.discarded_server_effects, 0,
        "control: nothing server-bound"
    );
}

#[test]
fn a_ticks_own_server_effect_is_discarded_and_never_rides_the_next_leg() {
    let (kernel, collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "saver", None)
        .unwrap();
    assert!(tick.emits.is_empty(), "no event to replay it under");
    assert_eq!(tick.discarded_server_effects, 1, "counted, not silent");
    assert!(
        collector.drain().is_empty(),
        "the collector is clean afterwards"
    );
    let next = kernel
        .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
        .unwrap();
    assert_eq!(
        next.server_legs.first().map(|leg| leg.event.as_str()),
        Some("DO_CREATE"),
        "the next dispatch's leg is its own"
    );
}

#[test]
fn a_guard_blocked_tick_runs_no_effects_and_no_cascade() {
    let (kernel, _collector) = booted_ticker(false);
    let tick = kernel
        .dispatch_tick("Main", "Ticker", None, "never", None)
        .unwrap();
    assert!(!tick.outcome.executed);
    assert!(tick.emits.is_empty());
}

#[test]
fn dispatching_an_unknown_tick_or_trait_is_an_error() {
    let (kernel, _collector) = booted_ticker(false);
    assert!(kernel
        .dispatch_tick("Main", "Ticker", None, "nope", None)
        .is_err());
    assert!(kernel
        .dispatch_tick("Main", "Ghost", None, "beat", None)
        .is_err());
}

/// std-realtime-chat's composer: SEND persists the draft, then clears it.
fn clearing_composer_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "content", "type": "string" }
            ] },
            "traits": [{
                "name": "Composer",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "transitions": [
                        { "from": "ready", "to": "ready", "event": "DRAFT",
                          "effects": [["set", "@entity.content", "@payload.value"]] },
                        {
                            "from": "ready", "to": "ready", "event": "SEND",
                            "guard": ["!=", ["default", "@entity.content", ""], ""],
                            "effects": [
                                ["persist", "create", "Note", { "content": "@entity.content" }],
                                ["set", "@entity.content", ""]
                            ]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// The leg replays SEND from the pre-dispatch state, so it must carry the
/// pre-dispatch row: the post-dispatch one has the draft already cleared
/// and fails the replayed guard.
#[test]
fn client_env_leg_carries_the_row_the_event_was_dispatched_against() {
    let (kernel, _collector) = client_kernel_with(clearing_composer_schema());
    kernel
        .dispatch_with_server_leg(
            "App",
            "Composer",
            None,
            "DRAFT",
            Value::object(ValueMap::from_iter([("value".to_string(), Value::from("hi"))])),
            None,
        )
        .unwrap();
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
        .unwrap();
    let leg = dispatch
        .server_legs
        .first()
        .expect("persist produces a server leg");
    let row = leg
        .entity_by_trait
        .as_ref()
        .and_then(|m| m.get("Composer"))
        .expect("seed row carried");
    assert_eq!(row.get_path("content").and_then(Value::as_str), Some("hi"));
}

fn counted_note_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "count", "type": "number" }] },
            "traits": [{
                "name": "Notes",
                "linkedEntity": "Note",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
            }]
        }]
    }))
    .unwrap()
}

fn rows_response(rows: serde_json::Value) -> EventResponse {
    let rows: Vec<Value> = serde_json::from_value(rows).unwrap();
    EventResponse { success: true, data: Some(HashMap::from([("Note".to_string(), rows)])), ..Default::default() }
}

/// G-CROSS-335: a server row is checked where the client folds it in; one that contradicts its entity refuses the
/// response before any row is written.
#[test]
fn a_server_row_that_contradicts_its_entity_refuses_the_whole_fold() {
    let (kernel, _) = client_kernel_with(counted_note_schema());
    let refused = kernel.apply_server_response(
        &rows_response(serde_json::json!([{ "id": "n-1", "count": 1 }, { "id": "n-2", "count": "2" }])),
        &HashSet::new(),
    );
    assert!(refused.is_err_and(|e| e.to_string().contains("Note.count")));
    assert!(kernel.entity_source().read("Note", "n-1").is_err(), "nothing was written");
}

#[test]
fn a_mistyped_trait_frame_refuses_the_fold() {
    let (kernel, _) = client_kernel_with(counted_note_schema());
    let mut entity_by_trait = ValueMap::default();
    entity_by_trait.insert("Notes".to_string(), serde_json::from_value(serde_json::json!({ "count": [] })).unwrap());
    let response = EventResponse { success: true, entity_by_trait: Some(entity_by_trait), ..Default::default() };
    assert!(kernel.apply_server_response(&response, &HashSet::new()).is_err());
}

#[test]
fn control_fitting_server_rows_fold_and_an_idless_row_is_the_runtime_singleton() {
    let (kernel, _) = client_kernel_with(counted_note_schema());
    kernel
        .apply_server_response(
            &rows_response(serde_json::json!([{ "id": "n-1", "count": 1 }, { "count": 2 }])),
            &HashSet::new(),
        )
        .unwrap();
    assert!(kernel.entity_source().read("Note", "n-1").is_ok());
    assert!(kernel.entity_source().read("Note", RUNTIME_SINGLETON).is_ok());
    assert!(kernel.entity_source().read("Note", "singleton").is_err(), "the literal is the wire's spelling only");
}

fn declared_count_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Counter",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "set" }],
                    "events": [{ "key": "SET", "name": "Set", "payloadSchema": [{ "name": "n", "type": "number" }] }],
                    "transitions": [{ "from": "idle", "to": "set", "event": "SET" }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// G-CROSS-335: a client kernel's UI event is an entry: its payload is checked against the declared event.
#[test]
fn a_client_ui_event_whose_payload_contradicts_its_declaration_is_refused() {
    let (kernel, _) = client_kernel_with(declared_count_schema());
    let payload: Value = serde_json::from_value(serde_json::json!({ "n": "one" })).unwrap();
    assert!(kernel.dispatch("Counter", None, "SET", payload).is_err());
    let payload: Value = serde_json::from_value(serde_json::json!({ "n": 1 })).unwrap();
    let outcomes = kernel.dispatch("Counter", None, "SET", payload).unwrap();
    assert!(outcomes.iter().any(|(_, r)| r.new_state == "set"), "control: a fitting payload runs");
}

/// G-CROSS-335: an in-process seed is an entry too: a row that contradicts its entity refuses the seed, nothing written.
#[test]
fn an_in_process_seed_whose_row_contradicts_its_entity_is_refused() {
    let bridge = crate::client::bridge::KernelBridge::new(counted_note_schema()).unwrap();
    let rows = |v: serde_json::Value| -> Vec<Value> { serde_json::from_value(v).unwrap() };
    let refused = bridge.seed("Note", rows(serde_json::json!([{ "id": "n-1", "count": 1 }, { "id": "n-2", "count": "2" }])));
    assert!(refused.is_err_and(|e| e.contains("Note.count")));
    bridge.seed("Note", rows(serde_json::json!([{ "id": "n-1", "count": 1 }]))).unwrap();
    bridge.seed("Note", rows(serde_json::json!([{ "count": 2 }]))).unwrap();
}
