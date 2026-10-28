use super::*;
use crate::client::client_kernel::ClientKernel;

/// Same shape as `orbital-core`'s own `render_ui_schema` kernel-level fixture —
/// a bare `SHOW` transition that renders `@payload.title` into `main`.
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
                            "effects": [["render-ui", "main", { "type": "page-header", "title": "@payload.title" }]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// The render circuit (`drive_blocking` → `process_response` → `ui_slots`/
/// `trait_content`) driven through `ClientKernel` — the same fixture and
/// assertions a bridge-backed `ClientCore` would need to satisfy, re-run
/// through the B4 path so a regression in `with_client_kernel`'s wiring
/// shows up here rather than only in the lower-level `client_kernel` tests.
#[test]
fn client_core_drives_render_through_client_kernel() {
    let schema = render_ui_schema();
    let client_kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, client_kernel);
    assert!(
        core.server_bridge.is_none(),
        "the client-kernel path never populates server_bridge"
    );

    let mut payload = ValueMap::default();
    payload.insert("title".to_string(), Value::string("Hello"));
    core.drive_blocking("SHOW", Some("RenderTrait"), Value::object(payload));

    assert_eq!(core.current_state, "shown");
    let main_slot = core.ui_slots.get("main").expect("main slot rendered");
    assert_eq!(main_slot.len(), 1);
    let title = main_slot[0]
        .props
        .as_object()
        .and_then(|o| o.get("title"))
        .cloned();
    assert_eq!(title, Some(Value::string("Hello")));
}

/// Two peer traits that both own `main`, and a third embedded by the first (`@trait.Child`).
fn peer_main_schema() -> OrbitalSchema {
    let shows = |title: &str| {
        let (show, hide) = (
            format!("SHOW_{}", title.to_uppercase()),
            format!("HIDE_{}", title.to_uppercase()),
        );
        serde_json::json!([
            { "from": "idle", "to": "shown", "event": show, "effects": [["render-ui", "main", { "type": "page-header", "title": title }]] },
            { "from": "shown", "to": "shown", "event": show, "effects": [["render-ui", "main", { "type": "page-header", "title": format!("{title}2") }]] },
            { "from": "shown", "to": "idle", "event": hide, "effects": [["render-ui", "main", null]] }
        ])
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "First", "local": true, "traitEmbedIds": { "Child": "child-id" },
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }], "transitions": shows("first") } },
                { "name": "Second", "local": true,
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }], "transitions": shows("second") } },
                { "name": "Child", "id": "child-id", "local": true,
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }], "transitions": shows("child") } }
            ]
        }]
    }))
    .unwrap()
}

fn main_titles(core: &ClientCore) -> Vec<String> {
    core.ui_slots
        .get("main")
        .map(|contents| {
            contents
                .iter()
                .map(|c| match c.props.as_object().and_then(|o| o.get("title")) {
                    Some(Value::String(title)) => title.to_string(),
                    _ => String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// G-ORB-172: every non-embedded `main` writer stacks in first-write order (web
/// `useUISlots.aggregateSlot`, the native shells), a re-render keeps its place, and a
/// null render clears only its own source.
#[test]
fn peer_main_writers_stack_in_first_write_order_and_clear_per_source() {
    let schema = peer_main_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.drive_blocking("SHOW_FIRST", Some("First"), Value::Null);
    core.drive_blocking("SHOW_SECOND", Some("Second"), Value::Null);
    assert_eq!(main_titles(&core), ["first", "second"]);
    core.drive_blocking("SHOW_FIRST", Some("First"), Value::Null);
    assert_eq!(
        main_titles(&core),
        ["first2", "second"],
        "a re-render keeps its place"
    );
    core.drive_blocking("HIDE_SECOND", Some("Second"), Value::Null);
    assert_eq!(
        main_titles(&core),
        ["first2"],
        "a null render clears only its source"
    );
    assert!(
        !core.trait_content.contains_key("Second"),
        "nor is it left for a splice"
    );
}

#[test]
fn control_an_embedded_trait_never_writes_main_but_stays_spliceable() {
    let schema = peer_main_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.drive_blocking("SHOW_FIRST", Some("First"), Value::Null);
    core.drive_blocking("SHOW_CHILD", Some("Child"), Value::Null);
    assert_eq!(main_titles(&core), ["first"]);
    assert!(core.trait_content.contains_key("Child"));
}

fn await_row(
    request: &str,
    event: &str,
    from: &str,
    trait_name: &str,
    slot: &str,
) -> orbital_core_base::ir::types::AwaitRenderRow {
    serde_json::from_value::<orbital_core_base::ir::types::AwaitRender>(serde_json::json!({
        "trait": trait_name, "slot": slot, "skeleton": { "shape": { "variant": "list", "rows": 3 } }
    }))
    .map(|r| orbital_core_base::ir::types::AwaitRenderRow {
        request_trait: request.into(),
        event: event.into(),
        from: Some(vec![from.into()]),
        trait_name: r.trait_name,
        slot: r.slot,
        skeleton: r.skeleton,
    })
    .unwrap()
}

fn awaiting(trait_name: &str, event: &str, from: &str) -> crate::kernel::AwaitingTrait {
    crate::kernel::AwaitingTrait {
        trait_name: trait_name.into(),
        event: event.into(),
        from: from.into(),
    }
}

fn skeleton_core() -> ClientCore {
    let mut core = ClientCore::new(peer_main_schema(), None);
    core.set_await_renders(vec![
        await_row("Second", "LOAD", "idle", "Second", "main"),
        await_row("Second", "LOAD", "idle", "Child", "main"),
        await_row("Second", "LOAD", "idle", "Second", "sidebar"),
        await_row("Second", "LOAD", "idle", "Second", "modal"),
    ]);
    core.set_awaiting_motion(Some(std::time::Duration::from_millis(300)));
    core
}

#[test]
fn a_predicted_skeleton_shows_after_the_busy_delay_and_clears_when_the_dispatch_settles() {
    let mut core = skeleton_core();
    let t0 = web_time::Instant::now();
    let token = core
        .begin_awaiting(&[awaiting("Second", "LOAD", "idle")], t0)
        .expect("predicted");
    assert!(
        core.awaiting_main_contents(t0).is_empty(),
        "nothing before the busy delay"
    );
    let later = t0 + std::time::Duration::from_millis(301);
    let main = core.awaiting_main_contents(later);
    assert_eq!(
        main.len(),
        1,
        "the embedded Child is its host's frame, not a main writer"
    );
    assert_eq!(
        (main[0].pattern.as_str(), main[0].source_trait.as_deref()),
        ("skeleton", Some("Second"))
    );
    assert!(
        core.trait_content_with_awaiting(later)
            .contains_key("Child"),
        "the embedded frame gets it"
    );
    assert!(core.awaiting_slot_content("sidebar", later).is_some());
    assert!(
        core.awaiting_slot_content("modal", later).is_none(),
        "a portal slot never opens for a skeleton"
    );
    core.end_awaiting(token);
    assert!(core.awaiting_main_contents(later).is_empty());
}

#[test]
fn control_no_skeleton_when_the_theme_turns_them_off_or_nothing_is_predicted() {
    let mut core = skeleton_core();
    assert!(core
        .begin_awaiting(
            &[awaiting("Second", "SAVE", "idle")],
            web_time::Instant::now()
        )
        .is_none());
    assert!(core
        .begin_awaiting(
            &[awaiting("Second", "LOAD", "busy")],
            web_time::Instant::now()
        )
        .is_none());
    core.set_awaiting_motion(None);
    assert!(core
        .begin_awaiting(
            &[awaiting("Second", "LOAD", "idle")],
            web_time::Instant::now()
        )
        .is_none());
}

fn two_page_schema() -> OrbitalSchema {
    let shows = |title: &str| {
        serde_json::json!([
            { "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "page-header", "title": title }]] }
        ])
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "ListTrait", "local": true, "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": shows("list") } },
                { "name": "DetailTrait", "local": true, "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": shows("detail") } }
            ],
            "pages": [
                { "name": "ListPage", "path": "/notes", "traits": [{ "ref": "ListTrait" }] },
                { "name": "DetailPage", "path": "/notes/:id", "traits": [{ "ref": "DetailTrait" }] }
            ]
        }]
    }))
    .unwrap()
}

/// The desktop shows one page at a time, like the web and native routers: `main` holds the
/// current page's writers only, and a `navigate` effect moves to the page whose path matches.
#[test]
fn main_shows_the_current_pages_writers_and_navigate_switches_page() {
    let schema = two_page_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.drive_blocking("INIT", Some("ListTrait"), Value::Null);
    core.drive_blocking("INIT", Some("DetailTrait"), Value::Null);
    assert_eq!(
        core.current_page.as_deref(),
        Some("ListPage"),
        "the first page when none is initial"
    );
    let titles = |core: &ClientCore| -> Vec<String> {
        core.page_main_contents()
            .iter()
            .map(|c| match c.props.as_object().and_then(|o| o.get("title")) {
                Some(Value::String(t)) => t.to_string(),
                _ => String::new(),
            })
            .collect()
    };
    assert_eq!(titles(&core), ["list"]);
    core.navigate_to("/notes/42", None);
    assert_eq!(core.current_page.as_deref(), Some("DetailPage"));
    assert_eq!(titles(&core), ["detail"]);
    core.navigate_to("/nowhere", None);
    assert_eq!(
        core.current_page.as_deref(),
        Some("DetailPage"),
        "control: an unknown path keeps the page"
    );
    core.show_page_of("ListTrait");
    assert_eq!(core.current_page.as_deref(), Some("ListPage"));
    core.show_page_of("NoSuchTrait");
    assert_eq!(
        core.current_page.as_deref(),
        Some("ListPage"),
        "control: an unpaged trait keeps the page"
    );
}

/// G-ORB-194: the kernel cascade is the one owner of emit delivery. A dispatched
/// event whose transition emits X reaches X's listener exactly once, and the response
/// must not queue X again for `ClientCore` to replay to the emitter.
fn emit_listener_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "count", "type": "number" }
            ] },
            "traits": [
                {
                    "name": "Emitter", "local": true,
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "X", {}]] }
                        ]
                    }
                },
                {
                    "name": "Listener", "local": true,
                    "listens": [{ "event": "X", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Emitter" } }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "HEARD",
                              "effects": [["set", "@entity.count", ["+", ["default", "@entity.count", 0], 1]]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn dispatch_response_does_not_carry_emits_the_cascade_delivered() {
    let schema = emit_listener_schema();
    let kernel = ClientKernel::new(schema, None).unwrap();
    kernel
        .dispatch("Emitter", None, "INIT", Value::Null)
        .unwrap();
    kernel
        .dispatch("Listener", None, "INIT", Value::Null)
        .unwrap();
    let response = kernel.dispatch("Emitter", None, "GO", Value::Null).unwrap();
    assert!(
        response.emitted_events.is_empty(),
        "the cascade already delivered X: {:?}",
        response.emitted_events
    );
}

#[test]
fn process_response_never_replays_a_delivered_emit() {
    let schema = emit_listener_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel.clone());
    core.drive_blocking("INIT", Some("Emitter"), Value::Null);
    core.drive_blocking("INIT", Some("Listener"), Value::Null);
    core.drive_blocking("GO", Some("Emitter"), Value::Null);
    assert!(
        core.pending_events.is_empty(),
        "queued replay: {:?}",
        core.pending_events
    );
    core.settle(8);
    let listener =
        serde_json::to_value(kernel.snapshot_trait("Listener", None).entity).unwrap();
    let emitter = serde_json::to_value(kernel.snapshot_trait("Emitter", None).entity).unwrap();
    assert_eq!(
        listener.get("count").and_then(|v| v.as_f64()),
        Some(1.0),
        "listener ran once: {listener:?}"
    );
    assert_eq!(
        emitter.get("count").and_then(|v| v.as_f64()),
        None,
        "emitter re-seeded with X: {emitter:?}"
    );
}

#[test]
fn an_emitter_handling_its_own_emit_runs_it_once() {
    let mut schema = serde_json::to_value(emit_listener_schema()).unwrap();
    schema["orbitals"][0]["traits"][0]["stateMachine"]["transitions"]
        .as_array_mut()
        .unwrap()
        .push(
            serde_json::json!({ "from": "idle", "to": "idle", "event": "X",
            "effects": [["set", "@entity.count", ["+", ["default", "@entity.count", 0], 100]]] }),
        );
    let schema: OrbitalSchema = serde_json::from_value(schema).unwrap();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel.clone());
    core.drive_blocking("INIT", Some("Emitter"), Value::Null);
    core.drive_blocking("INIT", Some("Listener"), Value::Null);
    core.drive_blocking("GO", Some("Emitter"), Value::Null);
    core.settle(8);
    let emitter = serde_json::to_value(kernel.snapshot_trait("Emitter", None).entity).unwrap();
    assert_eq!(
        emitter.get("count").and_then(|v| v.as_f64()),
        Some(100.0),
        "{emitter:?}"
    );
}

fn peer_buttons_schema() -> OrbitalSchema {
    let peer = |name: &str| {
        serde_json::json!({ "name": name, "local": true, "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "clicked" }],
            "transitions": [
                { "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "button", "label": name, "action": "CLICK" }]] },
                { "from": "idle", "to": "clicked", "event": "CLICK" }
            ] } })
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [peer("First"), peer("Second")]
        }]
    }))
    .unwrap()
}

/// A control dispatches as the trait whose content rendered it, never as the active trait
/// (the Dioxus sink once sent every click as the active trait).
#[test]
fn a_control_dispatches_as_the_trait_that_rendered_it() {
    let schema = peer_buttons_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, Arc::clone(&kernel));
    core.init_all_traits_blocking();
    assert_eq!(core.current_trait_name, "First");
    core.dispatch_control("CLICK", Some("Second"), Value::Null);
    assert_eq!(kernel.snapshot_trait("Second", None).state, "clicked");
    assert_eq!(
        kernel.snapshot_trait("First", None).state,
        "idle",
        "the active trait is untouched"
    );
    core.dispatch_control("CLICK", None, Value::Null);
    assert_eq!(
        kernel.snapshot_trait("First", None).state,
        "clicked",
        "control: no source → the active trait"
    );
}

fn two_orbital_schema() -> OrbitalSchema {
    let orbital = |name: &str,
                   page: &str,
                   path: &str,
                   trait_name: &str,
                   effects: serde_json::Value| {
        serde_json::json!({
            "name": name,
            "entity": { "name": format!("{name}Row"), "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": trait_name, "local": true, "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "page-header", "title": trait_name }]] },
                    { "from": "idle", "to": "idle", "event": "GO", "effects": effects }
                ] } }],
            "pages": [{ "name": page, "path": path, "traits": [{ "ref": trait_name }] }]
        })
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [
            orbital("Home", "HomePage", "/home", "HomeTrait", serde_json::json!([["navigate", "/settings"]])),
            orbital("Settings", "SettingsPage", "/settings", "SettingsTrait", serde_json::json!([]))
        ]
    }))
    .unwrap()
}

/// A `navigate` to another orbital's page switches orbital and mounts its traits.
#[test]
fn navigating_to_another_orbitals_page_mounts_it() {
    let schema = two_orbital_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.init_all_traits_blocking();
    assert_eq!(core.current_page.as_deref(), Some("HomePage"));
    core.drive_blocking("GO", Some("HomeTrait"), Value::Null);
    assert_eq!(core.current_orbital_idx, 1);
    assert_eq!(core.current_page.as_deref(), Some("SettingsPage"));
    let main = core.slot_contents("main");
    assert_eq!(main.len(), 1, "the new orbital's INIT painted its page");
    assert_eq!(main[0].source_trait.as_deref(), Some("SettingsTrait"));
}

/// A dispatch the kernel refuses is reported, never dropped silently.
#[test]
fn a_refused_dispatch_is_reported() {
    let schema = peer_buttons_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.drive_blocking("CLICK", Some("NoSuchTrait"), Value::Null);
    assert!(matches!(core.status_message, Some((_, true))));
    core.status_message = None;
    core.drive_blocking("CLICK", Some("First"), Value::Null);
    assert!(
        core.status_message.is_none(),
        "control: an accepted dispatch reports nothing"
    );
}

fn routed_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "ListTrait", "local": true, "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "page-header", "title": "list" }]] }
                ] } },
                { "name": "DetailTrait", "local": true, "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "page-header", "title": "@payload.id", "subtitle": "@payload.tab" }]] }
                ] } }
            ],
            "pages": [
                { "name": "ListPage", "path": "/notes", "traits": [{ "ref": "ListTrait" }] },
                { "name": "DetailPage", "path": "/notes/:id", "traits": [{ "ref": "DetailTrait" }] }
            ]
        }]
    }))
    .unwrap()
}

fn main_prop(core: &ClientCore, key: &str) -> Vec<String> {
    core.page_main_contents()
        .iter()
        .map(|c| match c.props.as_object().and_then(|o| o.get(key)) {
            Some(Value::String(t)) => t.to_string(),
            _ => String::new(),
        })
        .collect()
}

/// A page mount runs only that page's traits, with the route params (and the navigate
/// params, which win) as the INIT payload, as the web router's `initPayload` does.
#[test]
fn a_page_mount_inits_only_its_traits_with_the_route_params() {
    let schema = routed_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, Arc::clone(&kernel));
    core.mount_page_blocking();
    assert!(core.trait_content.contains_key("ListTrait"));
    assert!(
        !core.trait_content.contains_key("DetailTrait"),
        "control: another page's trait is not mounted"
    );

    core.navigate_to("/notes/42", None);
    assert!(core.page_entered);
    core.mount_page_blocking();
    assert_eq!(main_prop(&core, "title"), ["42"]);

    let mut params = ValueMap::default();
    params.insert("id".to_string(), Value::string("7"));
    params.insert("tab".to_string(), Value::string("history"));
    core.navigate_to("/notes/42", Some(&Value::object(params)));
    core.mount_page_blocking();
    assert_eq!(
        main_prop(&core, "title"),
        ["7"],
        "an explicit navigate param wins over the route"
    );
    assert_eq!(main_prop(&core, "subtitle"), ["history"]);
}

/// `navigate-back` returns to the previous page and its params; with no history it stays.
#[test]
fn navigate_back_returns_to_the_previous_page() {
    let schema = routed_schema();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.mount_page_blocking();
    core.navigate_to("/notes/1", None);
    core.navigate_to("/notes/2", None);
    core.navigate_back();
    assert_eq!(core.current_page.as_deref(), Some("DetailPage"));
    assert_eq!(
        core.route_params.get("id"),
        Some(&Value::string("1"))
    );
    core.navigate_back();
    assert_eq!(core.current_page.as_deref(), Some("ListPage"));
    assert!(core.route_params.is_empty());
    core.page_entered = false;
    core.navigate_back();
    assert_eq!(
        core.current_page.as_deref(),
        Some("ListPage"),
        "control: nothing to go back to keeps the page"
    );
    assert!(!core.page_entered);
}

/// A `navigate` effect mounts the page it lands on, in the same blocking dispatch.
#[test]
fn a_navigate_effect_mounts_the_page_it_lands_on() {
    let mut schema = routed_schema();
    let list: &mut serde_json::Value =
        &mut serde_json::to_value(&schema.orbitals[0].traits[0]).unwrap();
    list["stateMachine"]["transitions"].as_array_mut().unwrap().push(serde_json::json!(
        { "from": "idle", "to": "idle", "event": "OPEN", "effects": [["navigate", "/notes/9"]] }
    ));
    schema.orbitals[0].traits[0] = serde_json::from_value(list.clone()).unwrap();
    let kernel = Arc::new(ClientKernel::new(schema.clone(), None).unwrap());
    let mut core = ClientCore::with_client_kernel(schema, kernel);
    core.mount_page_blocking();
    core.drive_blocking("OPEN", Some("ListTrait"), Value::Null);
    assert_eq!(core.current_page.as_deref(), Some("DetailPage"));
    assert_eq!(main_prop(&core, "title"), ["9"]);
    assert!(!core.page_entered, "the blocking dispatch mounted it");
}
