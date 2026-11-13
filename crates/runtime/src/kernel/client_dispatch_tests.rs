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
    payload.insert("title".into(), Value::string("Hello"));
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
        schema["messages"] = serde_json::json!({
            "en": { "site:hero.title": "Hello", "site:posts.count": "{{n}} posts", "site:cta": "Start" },
            "ar": { "site:hero.title": "مرحبا", "site:posts.count": "{{n}} منشورات", "site:cta": "ابدأ" }
        });
    }
    serde_json::from_value(schema).unwrap()
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
    let config = rendered_config(&kernel);
    assert_eq!(field(&config, "title"), Value::string("Hello"));
    assert_eq!(field(&config, "lang"), Value::string("en"));
}

#[test]
fn edge_a_locale_the_program_does_not_declare_is_refused() {
    let kernel = server_kernel_with(i18n_schema(true));
    let err = kernel
        .enter_locale(Some("fr"))
        .err()
        .expect("undeclared locale");
    assert!(err.to_string().contains("\"fr\""), "{err}");
}

#[test]
fn the_viewer_locale_paints_a_dispatch_that_names_none() {
    let kernel = server_kernel_with(i18n_schema(true));
    kernel.set_viewer_locale(Some("ar".into())).unwrap();
    let config = rendered_config(&kernel);
    assert_eq!(field(&config, "title"), Value::string("مرحبا"));
    assert_eq!(field(&config, "lang"), Value::string("ar"));
    let _locale = kernel.enter_locale(Some("en")).unwrap();
    assert_eq!(field(&rendered_config(&kernel), "title"), Value::string("Hello"), "a dispatch's own locale wins");
}

#[test]
fn the_effective_locale_reads_what_the_viewer_set_else_the_first_declared() {
    let kernel = server_kernel_with(i18n_schema(true));
    assert_eq!(kernel.effective_locale().as_deref(), Some("en"), "control: unset answers the first declared");
    kernel.set_viewer_locale(Some("ar".into())).unwrap();
    assert_eq!(kernel.effective_locale().as_deref(), Some("ar"));
    let _locale = kernel.enter_locale(Some("en")).unwrap();
    assert_eq!(kernel.effective_locale().as_deref(), Some("en"), "a dispatch's own locale wins, as for i18n/t");
}

#[test]
fn edge_an_undeclared_viewer_locale_is_refused() {
    let kernel = server_kernel_with(i18n_schema(true));
    assert!(kernel.set_viewer_locale(Some("fr".into())).is_err());
    assert_eq!(field(&rendered_config(&kernel), "lang"), Value::string("en"), "control: the refused locale is not kept");
}

#[test]
fn edge_a_program_without_locales_accepts_any_viewer_locale() {
    let kernel = server_kernel_with(i18n_schema(false));
    assert!(kernel.enter_locale(Some("en")).is_ok());
}

/// `Note` is persistent and bound to the store driver `Rows`, an orbital the browser never runs (EXEC-STORE-4).
fn store_bound_response_schema() -> OrbitalSchema {
    let mut schema = serde_json::to_value(server_response_schema()).unwrap();
    schema["orbitals"][0]["entity"]["persistence"] = serde_json::json!("persistent");
    schema["orbitals"][0]["entity"]["store"] = serde_json::json!("Rows");
    schema["orbitals"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "Rows",
        "entity": { "name": "RowsRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
        "traits": [{ "name": "RowsDriver", "linkedEntity": "RowsRun",
            "stateMachine": { "states": [{ "name": "ready", "isInitial": true }], "transitions": [] } }]
    }));
    serde_json::from_value(schema).unwrap()
}

fn note_response() -> EventResponse {
    let mut row = ValueMap::default();
    row.insert("id".into(), Value::string("n-1"));
    let mut data = HashMap::new();
    data.insert("Note".to_string(), vec![Value::object(row)]);
    EventResponse { success: true, transitioned: true, data: Some(data), ..Default::default() }
}

#[test]
fn a_client_kernel_keeps_the_rows_the_server_sent_for_a_store_bound_entity() {
    let (kernel, _) = client_kernel_with(store_bound_response_schema());
    kernel.apply_server_response(&note_response(), &HashSet::new()).unwrap();
    assert!(kernel.entity_source().read("Note", "n-1").unwrap().is_object());
}

#[test]
fn control_a_server_kernel_answers_a_store_bound_entity_only_through_its_driver() {
    let kernel = server_kernel_with(store_bound_response_schema());
    let refused = kernel.apply_server_response(&note_response(), &HashSet::new()).unwrap_err();
    assert!(refused.to_string().contains("answered only on the resumable path"), "{refused}");
}

#[test]
fn apply_server_response_merges_data_and_states() {
    let kernel = server_kernel_with(server_response_schema());
    let mut row = ValueMap::default();
    row.insert("id".into(), Value::string("n-1"));
    row.insert(
        "title".into(),
        Value::string("From server"),
    );
    let mut data = HashMap::new();
    data.insert("Note".to_string(), vec![Value::object(row)]);

    let mut listener_entity = ValueMap::default();
    listener_entity.insert("id".into(), Value::string("singleton"));
    listener_entity.insert("note".into(), Value::string("n-1"));
    let frames = crate::effects::FrameDelta::absolute([("Listener".to_string(), listener_entity)]);

    let mut states = HashMap::new();
    states.insert("Listener".to_string(), "ponged".to_string());

    let response = EventResponse {
        success: true,
        transitioned: true,
        states,
        emitted_events: Vec::new(),
        data: Some(data),
        frames: Some(frames),
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
        collapsed: Vec::new(),
        consumed_emits: Vec::new(),
        emit_source: orbital_core_base::bus_event::BusEventSource::from_trait("Persistor"),
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
            Value::object(ValueMap::from_iter([("value".into(), Value::from("hi"))])),
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
            Value::object(ValueMap::from_iter([("value".into(), Value::from("hi"))])),
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
    let frames = crate::effects::FrameDelta::absolute([("Notes".to_string(), serde_json::from_value(serde_json::json!({ "count": [] })).unwrap())]);
    let response = EventResponse { success: true, frames: Some(frames), ..Default::default() };
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
/// `Picker` (`[client]`) asks from its INIT; `Asker` (server) answers with a persist. Mounted in that order, the ask
/// is held for Asker's INIT (EXEC-SEED-2) and runs inside Asker's own seed dispatch.
fn held_client_ask_schema(asker_init_persists: bool) -> OrbitalSchema {
    let asker_init = if asker_init_persists {
        serde_json::json!({ "from": "idle", "to": "idle", "event": "INIT", "effects": [["persist", "create", "Note", {}]] })
    } else {
        serde_json::json!({ "from": "idle", "to": "idle", "event": "INIT" })
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Picker",
                    "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }],
                        "transitions": [{ "from": "idle", "to": "asking", "event": "INIT", "effects": [["emit", "ASK", {}]] }]
                    }
                },
                {
                    "name": "Asker",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "ASK", "triggers": "ASKED", "source": { "kind": "trait", "trait": "Picker" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            asker_init,
                            { "from": "idle", "to": "idle", "event": "ASKED", "effects": [["persist", "create", "Note", {}]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// EXEC-DIST-2 + EXEC-DIST-10: the host never runs a `[client]` emitter, so its held delivery's server work leaves as
/// its own leg after the seed's, not folded into the seed's INIT replay.
#[test]
fn a_held_delivery_from_a_client_only_trait_leaves_as_its_own_leg() {
    let (kernel, _collector) = client_kernel_with(held_client_ask_schema(false));
    kernel.mount_traits(["Picker", "Asker"]);
    let picker = kernel
        .dispatch_with_server_leg("App", "Picker", None, "INIT", Value::Null, Some("tab-1"))
        .unwrap();
    assert!(
        picker.server_legs.is_empty(),
        "the client-only seed posts nothing itself"
    );
    let asker = kernel
        .dispatch_with_server_leg("App", "Asker", None, "INIT", Value::Null, Some("tab-1"))
        .unwrap();
    let legs: Vec<(String, Option<String>, Option<String>)> = asker
        .server_legs
        .iter()
        .map(|l| {
            (
                l.event.clone(),
                l.target_trait.clone(),
                l.source_trait.clone(),
            )
        })
        .collect();
    assert!(
        legs.contains(&(
            "ASKED".to_string(),
            Some("Asker".to_string()),
            Some("Picker".to_string())
        )),
        "the held ask must reach the host: {legs:?}"
    );
}

#[test]
fn control_a_held_delivery_whose_seed_delegates_still_posts_the_seed_replay_first() {
    let (kernel, _collector) = client_kernel_with(held_client_ask_schema(true));
    kernel.mount_traits(["Picker", "Asker"]);
    kernel
        .dispatch_with_server_leg("App", "Picker", None, "INIT", Value::Null, Some("tab-1"))
        .unwrap();
    let asker = kernel
        .dispatch_with_server_leg("App", "Asker", None, "INIT", Value::Null, Some("tab-1"))
        .unwrap();
    let events: Vec<&str> = asker.server_legs.iter().map(|l| l.event.as_str()).collect();
    assert_eq!(
        events,
        vec!["INIT", "ASKED"],
        "the seed's own replay, then the held ask once"
    );
}

#[test]
fn control_a_server_trait_with_no_held_delivery_posts_only_its_own_replay() {
    let (kernel, _collector) = client_kernel_with(held_client_ask_schema(true));
    kernel.mount_traits(["Asker"]);
    let asker = kernel
        .dispatch_with_server_leg("App", "Asker", None, "INIT", Value::Null, Some("tab-1"))
        .unwrap();
    let events: Vec<&str> = asker.server_legs.iter().map(|l| l.event.as_str()).collect();
    assert_eq!(events, vec!["INIT"]);
}
/// `Asker` (`[client]`) asks `Mid` (server, pure), which hands on to `Deep` (server, persists): the browser runs the
/// whole cascade, and the host must run it from `Mid`, the first server trait the client-only emitter reached.
fn client_chain_schema(mid_persists: bool, deep_persists: bool) -> OrbitalSchema {
    let mid_effects = if mid_persists {
        serde_json::json!([["persist", "create", "Note", {}], ["emit", "NEXT", {}]])
    } else {
        serde_json::json!([["emit", "NEXT", {}]])
    };
    let deep_effects = if deep_persists {
        serde_json::json!([["persist", "create", "Note", {}]])
    } else {
        serde_json::json!([])
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Asker",
                    "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "CLICK", "effects": [["emit", "GO", {}]] }
                        ]
                    }
                },
                {
                    "name": "Mid",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "GO", "triggers": "GOT", "source": { "kind": "trait", "trait": "Asker" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "asked" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "asked", "event": "GOT", "effects": mid_effects }
                        ]
                    }
                },
                {
                    "name": "Deep",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "NEXT", "triggers": "RUN", "source": { "kind": "trait", "trait": "Mid" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "RUN", "effects": deep_effects }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn leg_shapes(legs: &[EventRequest]) -> Vec<(String, Option<String>, Option<String>)> {
    legs.iter()
        .map(|l| {
            (
                l.event.clone(),
                l.target_trait.clone(),
                l.source_trait.clone(),
            )
        })
        .collect()
}

/// EXEC-DIST-10: a server trait a client-only emitter reached is posted as a leg, so the host runs the cascade from it;
/// a deeper trait's server work is that cascade, never a leg of its own (or the host's copy of `Mid` never moves).
#[test]
fn a_client_only_emit_posts_its_first_server_listener_even_when_only_a_deeper_trait_needs_the_server(
) {
    let (kernel, _collector) = client_kernel_with(client_chain_schema(false, true));
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Asker", None, "CLICK", Value::Null, Some("tab-1"))
        .unwrap();
    assert_eq!(
        leg_shapes(&dispatch.server_legs),
        vec![(
            "GOT".to_string(),
            Some("Mid".to_string()),
            Some("Asker".to_string())
        )]
    );
}

#[test]
fn control_a_listener_that_needs_the_server_itself_keeps_its_one_leg() {
    let (kernel, _collector) = client_kernel_with(client_chain_schema(true, false));
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Asker", None, "CLICK", Value::Null, Some("tab-1"))
        .unwrap();
    assert_eq!(
        leg_shapes(&dispatch.server_legs),
        vec![(
            "GOT".to_string(),
            Some("Mid".to_string()),
            Some("Asker".to_string())
        )]
    );
}

#[test]
fn control_a_client_chain_with_no_server_work_posts_nothing() {
    let (kernel, _collector) = client_kernel_with(client_chain_schema(false, false));
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Asker", None, "CLICK", Value::Null, Some("tab-1"))
        .unwrap();
    assert!(dispatch.server_legs.is_empty());
}

/// The launchpad's CI board: `Board` (`[client]`) asks `Retry` (server, pure), which answers `ATTEMPT` back to `Board`;
/// `Board` then asks `Github` (server, persists). `retry_back` false: `Retry` answers nothing, so the chain stays one hop.
fn client_server_client_server_schema(retry_back: bool, github_persists: bool) -> OrbitalSchema {
    let retry_effects = if retry_back {
        serde_json::json!([["emit", "ATTEMPT", {}]])
    } else {
        serde_json::json!([])
    };
    let github_effects = if github_persists {
        serde_json::json!([["persist", "create", "Note", {}]])
    } else {
        serde_json::json!([])
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Board",
                    "dispatch": "client",
                    "listens": [{ "event": "ATTEMPT", "triggers": "TRY", "source": { "kind": "trait", "trait": "Retry" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "LOAD", "effects": [["emit", "START", {}]] },
                            { "from": "idle", "to": "idle", "event": "TRY", "effects": [["emit", "READ", {}]] }
                        ]
                    }
                },
                {
                    "name": "Retry",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "START", "triggers": "RUN", "source": { "kind": "trait", "trait": "Board" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "RUN", "effects": retry_effects }
                        ]
                    }
                },
                {
                    "name": "Github",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "READ", "triggers": "FETCH", "source": { "kind": "trait", "trait": "Board" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "FETCH", "effects": github_effects }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// EXEC-DIST-10 / EXEC-DIST-2: the host never runs a `[client]` trait, so its cascade from a server hop stops at the
/// client trait; the server trait that client step reaches after the round trip is posted as a leg of its own.
#[test]
fn a_server_client_server_chain_posts_the_second_server_trait_as_its_own_leg() {
    let (kernel, _collector) = client_kernel_with(client_server_client_server_schema(true, true));
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Board", None, "LOAD", Value::Null, Some("tab-1"))
        .unwrap();
    let shapes = leg_shapes(&dispatch.server_legs);
    assert!(shapes.contains(&("RUN".to_string(), Some("Retry".to_string()), Some("Board".to_string()))), "{shapes:?}");
    assert!(shapes.contains(&("FETCH".to_string(), Some("Github".to_string()), Some("Board".to_string()))), "{shapes:?}");
    assert_eq!(shapes.len(), 2, "{shapes:?}");
}

#[test]
fn control_a_single_server_hop_posts_exactly_one_leg() {
    let (kernel, _collector) = client_kernel_with(client_server_client_server_schema(false, true));
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Board", None, "LOAD", Value::Null, Some("tab-1"))
        .unwrap();
    let shapes = leg_shapes(&dispatch.server_legs);
    assert!(shapes.iter().all(|s| s.1.as_deref() != Some("Github")), "{shapes:?}");
}

fn loading_board_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Board",
                    "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "loading" }],
                        "transitions": [
                            { "from": "idle", "to": "loading", "event": "LOAD", "effects": [["emit", "START", {}]] }
                        ]
                    }
                },
                {
                    "name": "Retry",
                    "linkedEntity": "Note",
                    "listens": [{ "event": "START", "triggers": "RUN", "source": { "kind": "trait", "trait": "Board" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                        "transitions": [
                            { "from": "idle", "to": "running", "event": "RUN", "effects": [["persist", "create", "Note", {}]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// EXEC-DIST-4: a host's report of a `[client]` trait's state is ignored unless the response shows the host ran it;
/// a server-resident trait's reported state is applied.
#[test]
fn a_server_answer_listing_a_client_trait_idle_leaves_it_loading() {
    let (kernel, _collector) = client_kernel_with(loading_board_schema());
    let dispatch = kernel
        .dispatch_with_server_leg("App", "Board", None, "LOAD", Value::Null, Some("tab-1"))
        .unwrap();
    assert_eq!(dispatch.server_legs.len(), 1);
    let mut states = HashMap::new();
    states.insert("Board".to_string(), "idle".to_string());
    states.insert("Retry".to_string(), "running".to_string());
    let response = EventResponse { success: true, transitioned: true, states, ..Default::default() };
    kernel.apply_server_response(&response, &HashSet::new()).unwrap();
    assert_eq!(kernel.get_state("Board", None).unwrap().current_state, "loading");
    assert_eq!(kernel.get_state("Retry", None).unwrap().current_state, "running");
}

/// A route trait that emits `SUBMIT` (heard only by `Jobs`, an orbital the client program leaves out and lists in
/// `_metadata.serverListens`) and `NOTE` (heard by nobody).
fn server_listen_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Jobs", "listen": {
            "event": "SUBMIT", "triggers": "START", "source": { "kind": "orbital", "orbital": "Main", "trait": "Page" }
        } }] },
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Form", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Page",
                "linkedEntity": "Form",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "RUN", "effects": [["emit", "SUBMIT", { "n": 1 }]] },
                        { "from": "idle", "to": "idle", "event": "TOUCH", "effects": [["emit", "NOTE", {}]] }
                    ]
                },
                "emits": [{ "event": "SUBMIT" }, { "event": "NOTE" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Page" }] }]
        }]
    }))
    .unwrap()
}

// EXEC-DIST-2: an emit only a server-only orbital hears is server work, so the step leaves as a leg (G-CROSS).
#[test]
fn client_env_an_emit_a_server_only_listener_hears_leaves_as_a_leg() {
    let (kernel, _collector) = client_kernel_with(server_listen_schema());
    let dispatch = kernel.dispatch_with_server_leg("App", "Page", None, "RUN", Value::Null, None).unwrap();
    assert_eq!(dispatch.server_legs.len(), 1, "the step that emits SUBMIT replays on the host");
    assert_eq!(dispatch.server_legs[0].event, "RUN");
    assert_eq!(dispatch.server_legs[0].target_trait.as_deref(), Some("Page"));
}

#[test]
fn control_client_env_an_emit_nobody_off_the_client_hears_posts_nothing() {
    let (kernel, _collector) = client_kernel_with(server_listen_schema());
    let dispatch = kernel.dispatch_with_server_leg("App", "Page", None, "TOUCH", Value::Null, None).unwrap();
    assert!(dispatch.server_legs.is_empty());
}

/// `Worker` and `Relay` are mounted only on `boot` (program-wide, `Relay` hears NOISE); `Viewer` is a `[client]` route trait listening to its `PING`.
fn program_wide_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{
            "name": "Ui",
            "entity": { "name": "View", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Viewer", "linkedEntity": "View", "dispatch": "client",
                "listens": [{ "event": "PING", "triggers": "PONG", "source": { "kind": "orbital", "orbital": "Jobs", "trait": "Worker" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [{ "from": "idle", "to": "idle", "event": "PONG" }] }
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Viewer" }] }]
        }, {
            "name": "Jobs",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Worker", "linkedEntity": "Job",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", { "n": 1 }], ["emit", "NOISE", {}]] }] },
                "emits": [{ "event": "PING", "scope": "external" }, { "event": "NOISE", "scope": "external" }]
            }, {
                "name": "Relay", "linkedEntity": "Job", "dispatch": "client",
                "listens": [{ "event": "NOISE", "triggers": "HEARD", "source": { "kind": "orbital", "orbital": "Jobs", "trait": "Worker" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [{ "from": "idle", "to": "idle", "event": "HEARD" }] }
            }],
            "pages": [{ "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "Worker" }, { "ref": "Relay" }] }]
        }]
    }))
    .unwrap()
}

// EXEC-STORE-4 / EXEC-DIST-9: a host pushes a dispatch only for a trait a browser runs.
#[test]
fn a_store_driver_orbitals_trait_is_not_browser_run() {
    let kernel = server_kernel_with(store_bound_response_schema());
    assert!(!kernel.browser_runs("RowsDriver"));
}

#[test]
fn a_boot_only_trait_is_not_browser_run() {
    let kernel = server_kernel_with(program_wide_schema());
    assert!(!kernel.browser_runs("Worker"));
}

#[test]
fn control_a_page_mounted_trait_is_browser_run_in_both_dispatch_modes() {
    let kernel = server_kernel_with(program_wide_schema());
    assert!(kernel.browser_runs("Viewer"), "dispatch: client");
    let server_mode = server_kernel_with(server_response_schema());
    assert!(server_mode.browser_runs("Listener"), "server-dispatched, not boot-only");
}

// EXEC-DIST-9: a program-wide trait's emit reaches every browser only when a client-run trait listens to it.
#[test]
fn program_wide_emits_are_the_boot_trait_emits_a_browser_listens_to() {
    let kernel = server_kernel_with(program_wide_schema());
    let outcomes = kernel.dispatch("Worker", None, "GO", Value::Null).unwrap();
    let mut response = kernel.response_for(&outcomes, "Worker", "GO");
    kernel.stamp_emitted_sources(&mut response);
    let events: Vec<String> = kernel.program_wide_emits(&response).into_iter().map(|e| e.event).collect();
    assert_eq!(events, ["PING"], "NOISE is external and a boot-only Relay listens, but no browser runs Relay");
}

#[test]
fn control_a_route_trait_s_emits_are_not_program_wide() {
    let kernel = server_kernel_with(program_wide_schema());
    let outcomes = kernel.dispatch("Viewer", None, "PONG", Value::Null).unwrap();
    let mut response = kernel.response_for(&outcomes, "Viewer", "PONG");
    kernel.stamp_emitted_sources(&mut response);
    assert!(kernel.program_wide_emits(&response).is_empty());
}

fn host_painted_shelf_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string", "required": true }] },
            "traits": [{
                "name": "Shelf",
                "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "browsing" }],
                    "events": [{ "key": "INIT", "name": "Init" }, { "key": "LOADED", "name": "Loaded" }, { "key": "OPEN", "name": "Open" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": "loading" }]] },
                        { "from": "loading", "to": "browsing", "event": "LOADED", "effects": [["render-ui", "main", { "type": "data-list", "entity": "@payload.data" }]] },
                        { "from": "browsing", "to": "browsing", "event": "OPEN", "guard": ["=", 1, 2], "effects": [["render-ui", "main", { "type": "typography", "content": "detail" }]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn host_paint(content: serde_json::Value) -> EventResponse {
    EventResponse {
        success: true,
        states: HashMap::from([("Shelf".to_string(), "browsing".to_string())]),
        client_effects_by_trait: Some(vec![crate::effects::response::ClientEffectByTrait {
            trait_name: "Shelf".to_string(),
            effect: orbital_core_base::client_effect::ClientEffect::RenderUi { slot: "main".to_string(), config: serde_json::from_value(content).unwrap() },
            event: None,
            from_state: None,
            callsite_payload: None,
            repaint: None,
        }]),
        ..Default::default()
    }
}

fn repainted_main(outcomes: &[(String, ProcessEventResult)]) -> Vec<serde_json::Value> {
    outcomes
        .iter()
        .flat_map(|(_, result)| result.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::RenderUi { slot, content } if slot == "main" => Some(orbital_core_base::json_value::value_to_json(content)),
            _ => None,
        })
        .collect()
}

/// EXEC-STEP-5 with EXEC-RENDER-4, on a client: a slot the host painted (a server leg's render) is what the slot holds, so
/// a step that does not fire repaints that, never the entering transition's render evaluated against the new event's payload.
#[test]
fn an_unfired_step_on_a_client_repaints_what_the_host_painted() {
    let (kernel, _) = client_kernel_with(host_painted_shelf_schema());
    let rows = serde_json::json!({ "type": "data-list", "entity": [{ "id": "a" }, { "id": "b" }] });
    kernel.apply_server_response(&host_paint(rows.clone()), &HashSet::new()).unwrap();
    let click: Value = serde_json::from_value(serde_json::json!({ "id": "a" })).unwrap();
    let outcomes = kernel.dispatch("Shelf", None, "OPEN", click).unwrap();
    assert!(outcomes.iter().all(|(_, result)| !result.executed), "control: the guard refuses, so the step does not fire");
    assert_eq!(repainted_main(&outcomes), [rows], "the list keeps its rows");
}

#[test]
fn control_an_unfired_step_on_a_client_the_host_never_painted_shows_the_entering_render() {
    let (kernel, _) = client_kernel_with(host_painted_shelf_schema());
    let outcomes = kernel.dispatch("Shelf", None, "OPEN", Value::Null).unwrap();
    assert_eq!(repainted_main(&outcomes), [serde_json::json!({ "type": "typography", "content": "loading" })]);
}

/// `Viewer` (a `[client]` route trait) writes `Member` itself on `TRY`, and on `ASK` emits `CLAIM`, which `Claimer` (mounted
/// only on `boot`, so the program session steps it) turns into the same write.
fn server_field_schema() -> OrbitalSchema {
    let write = |event: &str, team: &str| serde_json::json!({ "from": "idle", "to": "idle", "event": event, "effects": [["persist", "update", "Member", { "id": "u1", "teamId": team }]] });
    serde_json::from_value(serde_json::json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{
            "name": "Ui",
            "entity": { "name": "Member", "persistence": "persistent", "collection": "members",
                "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" },
                           { "name": "teamId", "type": "string", "update": { "server": "set by the membership flow" } }] },
            "traits": [{
                "name": "Viewer", "linkedEntity": "Member", "dispatch": "client",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    write("TRY", "t-viewer"),
                    { "from": "idle", "to": "idle", "event": "ASK", "effects": [["emit", "CLAIM", {}]] }] },
                "emits": [{ "event": "CLAIM", "scope": "external" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Viewer" }] }]
        }, {
            "name": "Jobs",
            "entity": { "name": "Member", "persistence": "persistent", "collection": "members",
                "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" },
                           { "name": "teamId", "type": "string", "update": { "server": "set by the membership flow" } }] },
            "traits": [{
                "name": "Claimer", "linkedEntity": "Member",
                "listens": [{ "event": "CLAIM", "triggers": "CLAIMED", "source": { "kind": "orbital", "orbital": "Ui", "trait": "Viewer" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [write("CLAIMED", "t-program")] }
            }],
            "pages": [{ "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "Claimer" }] }]
        }]
    }))
    .unwrap()
}

fn member_team(cache: &InMemoryEntityCache) -> Value {
    cache.read("Member", "u1").unwrap().get_path("teamId").cloned().unwrap_or(Value::Null)
}

// EXEC-FIELD-1: a `server` field changes only in a program-session step.
#[test]
fn a_viewer_session_cannot_write_a_server_field_but_the_program_session_step_it_asked_for_can() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, cache.clone() as Arc<dyn EntitySource>, KernelConfig::default());
    kernel.register_schema(server_field_schema()).unwrap();
    cache.write("Member", "u1", &serde_json::from_value(serde_json::json!({ "id": "u1", "name": "Ada", "teamId": "" })).unwrap()).unwrap();
    let _session = kernel.enter_session("s1");
    let tried = kernel.dispatch("Viewer", None, "TRY", Value::Null).unwrap();
    assert!(tried.iter().all(|(_, r)| r.effect_results.iter().all(|e| !e.is_executed())), "{tried:?}");
    assert_eq!(member_team(&cache), Value::string(""), "the viewer's own write of a server field is refused");
    kernel.dispatch("Viewer", None, "ASK", Value::Null).unwrap();
    assert_eq!(member_team(&cache), Value::string("t-program"), "the program-session step wrote it");
}
