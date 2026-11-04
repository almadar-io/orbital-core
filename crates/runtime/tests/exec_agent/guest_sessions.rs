// EXEC-CONC-7 guest sessions: `program/open` runs a program value as a guest kernel the opener's kernel keeps, with
// its own rows; `program/dispatch` and `program/inputs` reach it by its handle; `program/close` ends it.

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::entity_source::InMemoryEntityCache;
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::json;
use std::sync::Arc;

fn guest() -> serde_json::Value {
    json!({
        "name": "Diary",
        "typeContractVersion": 1,
        "orbitals": [{
            "name": "Diary",
            "entity": { "name": "DiaryNote", "persistence": "persistent", "collection": "diary_notes", "fields": [{ "name": "id", "type": "string" }, { "name": "text", "type": "string" }] },
            "traits": [{
                "name": "DiaryList",
                "linkedEntity": "DiaryNote",
                "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "ready" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [
                            ["fetch", "DiaryNote", { "emit": { "success": "NOTES_LOADED", "failure": "NOTES_FAILED" } }],
                            ["render-ui", "main", { "type": "typography", "content": "Reading the diary" }]
                        ] },
                        { "from": "loading", "to": "ready", "event": "NOTES_LOADED", "effects": [
                            ["render-ui", "main", { "type": "stack", "children": [
                                { "type": "typography", "content": "The diary is read" },
                                { "type": "button", "label": "Add", "action": "ADD", "actionPayload": { "text": "clicked" } },
                                { "type": "date-range-selector", "onSelect": "PICK_RANGE" }
                            ] }]
                        ] },
                        { "from": "loading", "to": "loading", "event": "NOTES_FAILED", "effects": [
                            ["render-ui", "main", { "type": "typography", "content": "The diary could not be read" }]
                        ] },
                        { "from": "ready", "to": "ready", "event": "ADD", "effects": [
                            ["persist", "create", "DiaryNote", { "text": "@payload.text" }],
                            ["render-ui", "main", { "type": "typography", "content": "A note was added" }]
                        ] }
                    ]
                }
            }],
            "pages": [{ "name": "DiaryPage", "path": "/diary", "traits": [{ "ref": "DiaryList" }] }]
        }]
    })
}

const EMIT: &str = r#"{ "success": "ANSWERED", "failure": "REFUSED" }"#;

fn emit() -> serde_json::Value {
    serde_json::from_str(EMIT).unwrap()
}

/// An opener whose `GO` runs `effect`; `ANSWERED` keeps the answer, `REFUSED` the failure, `OPENED` the guest handle.
fn opener(transitions: serde_json::Value) -> RuntimeKernel {
    let mut all = vec![
        json!({ "from": "idle", "to": "idle", "event": "OPENED", "effects": [["set", "@entity.guest", "@payload.result.guest"], ["set", "@entity.got", "@payload.result"]] }),
        json!({ "from": "idle", "to": "idle", "event": "ANSWERED", "effects": [["set", "@entity.got", "@payload.result"]] }),
        json!({ "from": "idle", "to": "idle", "event": "REFUSED", "effects": [["set", "@entity.refused", "@payload"]] }),
    ];
    all.extend(transitions.as_array().unwrap().iter().cloned());
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Playground",
        "typeContractVersion": 1,
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Run", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Opener",
                "linkedEntity": "Run",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": all }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()),
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn open_transition() -> serde_json::Value {
    json!({ "from": "idle", "to": "idle", "event": "GO", "effects": [
        ["program/open", "@payload.program", {}, { "emit": { "success": "OPENED", "failure": "REFUSED" } }]
    ] })
}

fn entity(kernel: &RuntimeKernel) -> serde_json::Value {
    serde_json::to_value(&kernel.snapshot_trait("Opener", None).entity).unwrap()
}

fn run(kernel: &RuntimeKernel, event: &str) -> serde_json::Value {
    let payload = if event == "GO" {
        serde_json::from_value(json!({ "program": guest() })).unwrap()
    } else {
        Value::Null
    };
    kernel.dispatch("Opener", None, event, payload).unwrap();
    entity(kernel)
}

fn opened() -> (RuntimeKernel, String, serde_json::Value) {
    let kernel = opener(json!([
        open_transition(),
        { "from": "idle", "to": "idle", "event": "ADD", "effects": [
            ["program/dispatch", "Diary.DiaryList.ADD", { "text": "hello" }, { "guest": "@entity.guest", "emit": emit() }]
        ] },
        { "from": "idle", "to": "idle", "event": "READ", "effects": [
            ["program/inputs", { "guest": "@entity.guest", "state": "Diary" }, { "emit": emit() }]
        ] },
        { "from": "idle", "to": "idle", "event": "CLOSE", "effects": [
            ["program/close", "@entity.guest", { "emit": emit() }]
        ] },
        { "from": "idle", "to": "idle", "event": "PAYLOAD_NAMES_GUEST", "effects": [
            ["program/dispatch", "Diary.DiaryList.ADD", { "guest": "@entity.guest", "text": "x" }, { "emit": emit() }]
        ] },
        { "from": "idle", "to": "idle", "event": "STRAY", "effects": [
            ["program/dispatch", "Diary.DiaryList.ADD", { "text": "x" }, { "guest": "no-such-guest", "emit": emit() }]
        ] }
    ]));
    let after = run(&kernel, "GO");
    let handle = after["guest"].as_str().unwrap_or_default().to_string();
    (kernel, handle, after)
}

fn trait_state(answer: &serde_json::Value, trait_name: &str) -> Option<String> {
    answer["state"]["traits"]
        .as_array()?
        .iter()
        .find(|t| t["trait"] == trait_name)
        .and_then(|t| t["state"].as_str())
        .map(str::to_string)
}

fn painted(answer: &serde_json::Value) -> Vec<String> {
    fn texts(node: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(text) = node["content"].as_str() {
            out.push(text.to_string());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            texts(child, out);
        }
    }
    let mut out = Vec::new();
    for slot in answer["rendered"].as_array().into_iter().flatten() {
        texts(&slot["content"], &mut out);
    }
    out
}

#[test]
fn opening_a_guest_mounts_its_page_and_its_fetch_answers_from_the_guests_own_rows() {
    let (_kernel, handle, after) = opened();
    assert!(!handle.is_empty(), "a handle names the guest: {after}");
    assert_eq!(
        after["got"]["states"]["DiaryList"], "ready",
        "the INIT fetch answered, so the trait moved on: {after}"
    );
    assert!(
        painted(&after["got"]).contains(&"The diary is read".to_string()),
        "the guest's paint is answered: {after}"
    );
    assert!(
        after["got"]["rendered"].as_array().is_some_and(|r| !r.is_empty() && r.iter().all(|p| p["embedded"] == false)),
        "a paint of the page's own trait says it is not embedded: {after}"
    );
    assert!(
        after.get("refused").is_none_or(serde_json::Value::is_null),
        "control: nothing was refused: {after}"
    );
}

#[test]
fn a_guest_dispatch_advances_the_guest_and_answers_its_states_paint_and_outcomes() {
    let (kernel, _, _) = opened();
    let after = run(&kernel, "ADD");
    assert_eq!(after["got"]["states"]["DiaryList"], "ready", "{after}");
    assert!(
        painted(&after["got"]).contains(&"A note was added".to_string()),
        "{after}"
    );
    let outcome = &after["got"]["outcomes"][0];
    assert_eq!(
        (
            outcome["trait"].as_str(),
            outcome["event"].as_str(),
            outcome["from"].as_str(),
            outcome["to"].as_str()
        ),
        (Some("DiaryList"), Some("ADD"), Some("ready"), Some("ready")),
        "{after}"
    );
}

#[test]
fn a_guest_keeps_its_state_between_dispatches_and_inputs_reads_it() {
    let (kernel, _, _) = opened();
    run(&kernel, "ADD");
    let after = run(&kernel, "READ");
    assert_eq!(
        trait_state(&after["got"], "DiaryList").as_deref(),
        Some("ready"),
        "{after}"
    );
}

#[test]
fn control_a_dispatch_to_an_unknown_guest_is_refused_by_name() {
    let (kernel, _, _) = opened();
    let after = run(&kernel, "STRAY");
    let error = after["refused"]["error"].as_str().unwrap_or_default();
    assert!(error.contains("no-such-guest"), "{after}");
}

#[test]
fn control_a_payload_naming_a_guest_is_not_a_guest_dispatch() {
    let (kernel, _, _) = opened();
    let after = run(&kernel, "PAYLOAD_NAMES_GUEST");
    assert!(
        after["got"]["outcomes"].is_null() && after["got"]["states"].is_null(),
        "only the options record names a guest; the payload is the event's own, so the opener's own path answered: {after}"
    );
}

#[test]
fn a_closed_guest_takes_no_more_dispatches() {
    let (kernel, _, _) = opened();
    let closed = run(&kernel, "CLOSE");
    assert!(
        closed.get("refused").is_none_or(serde_json::Value::is_null),
        "closing is answered: {closed}"
    );
    let after = run(&kernel, "ADD");
    assert!(
        after["refused"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("guest")),
        "{after}"
    );
}

#[test]
fn edge_a_guest_given_grants_needs_the_openers_kernel_spawn_grant() {
    let kernel = opener(json!([
        { "from": "idle", "to": "idle", "event": "GO", "effects": [
            ["program/open", "@payload.program", { "grants": { "net": {} } }, { "emit": { "success": "OPENED", "failure": "REFUSED" } }]
        ] }
    ]));
    let after = run(&kernel, "GO");
    assert!(
        after["refused"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("no kernel guest grant covers it")),
        "{after}"
    );
}

/// An opener that paints the guest's last screen inside a `guest-view` and turns its control clicks into guest dispatches.
fn painting_opener() -> RuntimeKernel {
    opener(json!([
        open_transition(),
        { "from": "idle", "to": "idle", "event": "PAINT", "effects": [
            ["set", "@entity.view", ["object/get", ["array/last", "@entity.got.rendered"], "content"]],
            ["render-ui", "main", { "type": "guest-view", "guest": "@entity.guest", "deliver": "GUEST_CONTROL", "source": "DiaryList", "children": ["@entity.view"] }]
        ] },
        { "from": "idle", "to": "idle", "event": "GUEST_CONTROL", "effects": [
            ["set", "@entity.clicked", "@payload"],
            ["program/dispatch", "@payload.address", "@payload.payload", { "guest": "@payload.guest", "emit": emit() }]
        ] }
    ]))
}

#[test]
fn a_click_on_a_painted_guest_control_reaches_the_guest_through_the_openers_declared_event() {
    let kernel = painting_opener();
    run(&kernel, "GO");
    run(&kernel, "PAINT");
    kernel.dispatch("Opener", None, "ADD", serde_json::from_value(json!({ "text": "clicked" })).unwrap()).unwrap();
    let after = entity(&kernel);
    assert_eq!(after["clicked"]["address"], "DiaryList.ADD", "{after}");
    assert_eq!(after["clicked"]["event"], "ADD", "{after}");
    assert_eq!(after["clicked"]["payload"]["text"], "clicked", "{after}");
    assert!(after["clicked"]["guest"].as_str().is_some_and(|g| !g.is_empty()), "{after}");
    assert_eq!(after["got"]["outcomes"][0]["event"], "ADD", "the guest advanced: {after}");
    assert!(painted(&after["got"]).contains(&"A note was added".to_string()), "{after}");
}

#[test]
fn control_an_event_no_painted_guest_control_carries_is_not_delivered() {
    let kernel = painting_opener();
    run(&kernel, "GO");
    run(&kernel, "PAINT");
    kernel.dispatch("Opener", None, "NOPE", Value::Null).unwrap();
    let after = entity(&kernel);
    assert!(after.get("clicked").is_none_or(serde_json::Value::is_null), "{after}");
}

#[test]
fn control_before_the_guest_view_is_painted_a_guest_event_is_not_delivered() {
    let kernel = painting_opener();
    run(&kernel, "GO");
    kernel.dispatch("Opener", None, "ADD", Value::Null).unwrap();
    let after = entity(&kernel);
    assert!(after.get("clicked").is_none_or(serde_json::Value::is_null), "{after}");
}

#[test]
fn a_guest_control_naming_its_event_through_a_callback_prop_is_routed_too() {
    let kernel = painting_opener();
    run(&kernel, "GO");
    run(&kernel, "PAINT");
    kernel.dispatch("Opener", None, "PICK_RANGE", serde_json::from_value(json!({ "value": "1M" })).unwrap()).unwrap();
    let after = entity(&kernel);
    assert_eq!(after["clicked"]["address"], "DiaryList.PICK_RANGE", "{after}");
    assert_eq!(after["clicked"]["payload"]["value"], "1M", "{after}");
}

#[test]
fn a_bare_event_dispatch_reaches_every_guest_trait_that_handles_it_and_answers_its_emits() {
    let kernel = opener(json!([
        open_transition(),
        { "from": "idle", "to": "idle", "event": "KEY", "effects": [
            ["program/dispatch", "ADD", { "text": "keyed" }, { "guest": "@entity.guest", "emit": emit() }]
        ] },
        { "from": "idle", "to": "idle", "event": "STRAY_KEY", "effects": [
            ["program/dispatch", "NOBODY_HANDLES", {}, { "guest": "@entity.guest", "emit": emit() }]
        ] }
    ]));
    run(&kernel, "GO");
    let after = run(&kernel, "KEY");
    assert_eq!(after["got"]["outcomes"][0]["trait"], "DiaryList", "{after}");
    assert_eq!(after["got"]["outcomes"][0]["event"], "ADD", "{after}");
    let stray = run(&kernel, "STRAY_KEY");
    assert_eq!(stray["got"]["outcomes"].as_array().map(Vec::len), Some(0), "control: an event no guest trait handles is a no-op: {stray}");
}

/// A browser kernel whose `Stage` delivers a guest control as `GUEST_CONTROL`; `LOCAL` paints its slot itself.
fn browser_stage() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Playground",
        "typeContractVersion": 1,
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Stage",
                "linkedEntity": "Run",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GUEST_CONTROL", "effects": [["set", "@entity.clicked", "@payload"]] },
                        { "from": "idle", "to": "idle", "event": "LOCAL", "effects": [["render-ui", "main", { "type": "typography", "content": "own paint" }]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()),
        KernelConfig::for_environment(RuntimeEnvironment::Client),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

/// A server leg's answer in which the server ran `Stage` and painted a guest-view on `main`.
fn server_painted_guest_view() -> orbital_core_runtime::effects::response::EventResponse {
    serde_json::from_value(json!({
        "success": true,
        "transitioned": true,
        "clientEffectsByTrait": [{
            "traitName": "Stage",
            "effect": ["render-ui", "main", { "type": "stack", "children": [
                { "type": "guest-view", "guest": "guest-7", "source": "DiaryList", "deliver": "GUEST_CONTROL", "children": [
                    { "type": "button", "label": "Add", "action": "ADD" }
                ] }
            ] }]
        }]
    }))
    .unwrap()
}

fn stage_entity(kernel: &RuntimeKernel) -> serde_json::Value {
    serde_json::to_value(&kernel.snapshot_trait("Stage", None).entity).unwrap()
}

#[test]
fn a_guest_view_the_server_painted_for_a_browser_trait_routes_its_clicks() {
    let kernel = browser_stage();
    kernel
        .apply_server_response(&server_painted_guest_view(), &std::collections::HashSet::new())
        .unwrap();
    kernel.dispatch("Stage", None, "ADD", serde_json::from_value(json!({ "text": "x" })).unwrap()).unwrap();
    let after = stage_entity(&kernel);
    assert_eq!(after["clicked"]["guest"], "guest-7", "{after}");
    assert_eq!(after["clicked"]["address"], "DiaryList.ADD", "{after}");
}

#[test]
fn control_once_the_browser_trait_paints_the_slot_itself_the_servers_guest_view_is_gone() {
    let kernel = browser_stage();
    kernel
        .apply_server_response(&server_painted_guest_view(), &std::collections::HashSet::new())
        .unwrap();
    kernel.dispatch("Stage", None, "LOCAL", Value::Null).unwrap();
    kernel.dispatch("Stage", None, "ADD", Value::Null).unwrap();
    let after = stage_entity(&kernel);
    assert!(after.get("clicked").is_none_or(serde_json::Value::is_null), "{after}");
}

/// The diary guest whose first screen names the first note it fetched, or "No notes yet" when it fetched none.
fn listing_guest() -> serde_json::Value {
    let mut program = guest();
    program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][1]["effects"] = json!([
        ["render-ui", "main", { "type": "typography", "content":
            ["if", ["=", ["array/len", "@payload.data"], 0], "No notes yet", ["object/get", ["array/nth", "@payload.data", 0], "text"]] }]
    ]);
    program
}

fn open_with_rows(rows: serde_json::Value) -> serde_json::Value {
    let kernel = opener(json!([
        { "from": "idle", "to": "idle", "event": "GO", "effects": [
            ["program/open", "@payload.program", { "rows": "@payload.rows" }, { "emit": { "success": "OPENED", "failure": "REFUSED" } }]
        ] }
    ]));
    let payload = serde_json::from_value(json!({ "program": listing_guest(), "rows": rows })).unwrap();
    kernel.dispatch("Opener", None, "GO", payload).unwrap();
    entity(&kernel)
}

#[test]
fn rows_given_to_open_are_in_the_guests_store_before_its_page_mounts() {
    let after = open_with_rows(json!([{ "entity": "DiaryNote", "rows": [{ "id": "n1", "text": "Seeded note" }] }]));
    assert!(
        painted(&after["got"]).contains(&"Seeded note".to_string()),
        "the guest's INIT fetch answered the seeded row: {after}"
    );
}

#[test]
fn control_a_guest_opened_without_rows_starts_with_an_empty_store() {
    let after = open_with_rows(json!([]));
    assert!(painted(&after["got"]).contains(&"No notes yet".to_string()), "{after}");
}

#[test]
fn edge_rows_that_do_not_fit_or_name_an_undeclared_entity_refuse_the_open() {
    let misfit = open_with_rows(json!([{ "entity": "DiaryNote", "rows": [{ "id": "n1", "text": 7 }] }]));
    assert!(
        misfit["refused"]["error"].as_str().is_some_and(|e| e.contains("DiaryNote")),
        "a row that breaks its entity's declaration is refused, naming it: {misfit}"
    );
    let stranger = open_with_rows(json!([{ "entity": "Ghost", "rows": [{ "id": "g1" }] }]));
    assert!(
        stranger["refused"]["error"].as_str().is_some_and(|e| e.contains("Ghost")),
        "rows for an entity the program does not declare are refused: {stranger}"
    );
    assert!(misfit["guest"].as_str().unwrap_or_default().is_empty(), "a refused open opens no guest: {misfit}");
}

fn paints(name: &str, content: &str) -> serde_json::Value {
    json!({
        "name": name,
        "linkedEntity": "ShopItem",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                ["render-ui", "main", { "type": "typography", "content": content }]
            ] }]
        }
    })
}

fn lists(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "linkedEntity": "ShopItem",
        "stateMachine": {
            "states": [{ "name": "loading", "isInitial": true }, { "name": "ready" }],
            "transitions": [
                { "from": "loading", "to": "loading", "event": "INIT", "effects": [
                    ["fetch", "ShopItem", { "emit": { "success": "LOADED", "failure": "FAILED" } }]
                ] },
                { "from": "loading", "to": "ready", "event": "LOADED", "effects": [
                    ["render-ui", "main", { "type": "typography", "content":
                        ["if", ["=", ["array/len", "@payload.data"], 0], "none listed", ["object/get", ["array/nth", "@payload.data", 0], "name"]] }]
                ] },
                { "from": "loading", "to": "loading", "event": "FAILED", "effects": [] }
            ]
        }
    })
}

fn shop_orbital(name: &str, entity: &str, traits: serde_json::Value, pages: serde_json::Value) -> serde_json::Value {
    json!({
        "name": name,
        "entity": { "name": entity, "persistence": "persistent", "collection": entity.to_lowercase(), "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" }] },
        "traits": traits,
        "pages": pages
    })
}

fn shop() -> serde_json::Value {
    json!({
        "name": "Shop",
        "typeContractVersion": 1,
        "orbitals": [shop_orbital("Main", "ShopItem",
            json!([paints("Booter", "boot ran"), paints("HomeT", "home ran"), lists("ListT"), paints("OtherT", "other ran")]),
            json!([
                { "name": "Home", "path": "/", "traits": [{ "ref": "HomeT" }] },
                { "name": "List", "path": "/list", "traits": [{ "ref": "ListT" }] },
                { "name": "Other", "path": "/other", "traits": [{ "ref": "OtherT" }] },
                { "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "Booter" }] }
            ]))]
    })
}

fn late_orbital_ref() -> &'static str {
    "lazy/Late.orb"
}

fn shop_with_lazy_page() -> serde_json::Value {
    let mut program = shop();
    program["lazyPages"] = json!([{
        "path": "/late", "orbital": "Late", "orbRef": late_orbital_ref(),
        "traits": [{ "name": "LateT", "dispatchMode": "persistedAwaited" }]
    }]);
    program["lazyPrograms"] = json!({
        late_orbital_ref(): {
            "name": "Late",
            "typeContractVersion": 1,
            "orbitals": [shop_orbital("Late", "LateItem",
                json!([{
                    "name": "LateT", "linkedEntity": "LateItem",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                            ["render-ui", "main", { "type": "typography", "content": "late ran" }]
                        ] }]
                    }
                }]),
                json!([{ "name": "LatePage", "path": "/late", "traits": [{ "ref": "LateT" }] }]))]
        }
    });
    program
}

fn shop_opener() -> RuntimeKernel {
    opener(json!([
        { "from": "idle", "to": "idle", "event": "GO", "effects": [
            ["program/open", "@payload.program", "@payload.options", { "emit": { "success": "OPENED", "failure": "REFUSED" } }]
        ] },
        { "from": "idle", "to": "idle", "event": "NAV", "effects": [
            ["program/navigate", { "guest": "@entity.guest", "route": "@payload.route", "rows": "@payload.rows" }, { "emit": emit() }]
        ] }
    ]))
}

fn shop_event(kernel: &RuntimeKernel, event: &str, payload: serde_json::Value) -> serde_json::Value {
    kernel.dispatch("Opener", None, event, serde_json::from_value(payload).unwrap()).unwrap();
    entity(kernel)
}

fn open_shop(program: serde_json::Value, options: serde_json::Value) -> (RuntimeKernel, serde_json::Value) {
    let kernel = shop_opener();
    let after = shop_event(&kernel, "GO", json!({ "program": program, "options": options }));
    (kernel, after)
}

fn mounted(answer: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = answer["states"].as_object().into_iter().flatten().map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

#[test]
fn open_mounts_the_boot_traits_and_the_default_routed_page_and_no_other_page() {
    let (_, after) = open_shop(shop(), json!({}));
    assert_eq!(mounted(&after["got"]), ["Booter", "HomeT"], "boot trait and the first route page mount: {after}");
    let texts = painted(&after["got"]);
    assert!(texts.contains(&"boot ran".to_string()) && texts.contains(&"home ran".to_string()), "{after}");
    assert!(!texts.contains(&"other ran".to_string()), "a trait only on another page is not mounted: {after}");
}

#[test]
fn open_with_a_route_mounts_that_page_and_the_boot_traits() {
    let (_, after) = open_shop(shop(), json!({ "route": "/other" }));
    assert_eq!(mounted(&after["got"]), ["Booter", "OtherT"], "{after}");
    assert!(
        !painted(&after["got"]).contains(&"home ran".to_string()),
        "control: the default page is not mounted when a route is named: {after}"
    );
}

#[test]
fn navigate_mounts_the_new_route_after_seeding_its_rows() {
    let (kernel, _) = open_shop(shop(), json!({}));
    let after = shop_event(
        &kernel,
        "NAV",
        json!({ "route": "/list", "rows": [{ "entity": "ShopItem", "rows": [{ "id": "i1", "name": "Seeded item" }] }] }),
    );
    assert_eq!(mounted(&after["got"]), ["ListT"], "only the new route's trait mounts: {after}");
    assert!(painted(&after["got"]).contains(&"Seeded item".to_string()), "the INIT fetch saw the seeded row: {after}");
}

#[test]
fn control_navigate_without_rows_fetches_an_empty_store() {
    let (kernel, _) = open_shop(shop(), json!({}));
    let after = shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [] }));
    assert!(painted(&after["got"]).contains(&"none listed".to_string()), "{after}");
}

#[test]
fn navigating_to_a_mounted_route_mounts_nothing_new() {
    let (kernel, _) = open_shop(shop(), json!({}));
    shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [] }));
    let again = shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [] }));
    assert!(mounted(&again["got"]).is_empty(), "{again}");
    assert!(again["got"]["outcomes"].as_array().is_some_and(Vec::is_empty), "{again}");
    let home = shop_event(&kernel, "NAV", json!({ "route": "/", "rows": [] }));
    assert!(mounted(&home["got"]).is_empty(), "control: the route open at start is already mounted: {home}");
}

#[test]
fn edge_navigate_refuses_an_unknown_guest_and_rows_for_an_undeclared_entity() {
    let (kernel, _) = open_shop(shop(), json!({}));
    let ghost = shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [{ "entity": "Ghost", "rows": [] }] }));
    assert!(ghost["refused"]["error"].as_str().is_some_and(|e| e.contains("program/navigate") && e.contains("Ghost")), "{ghost}");
    let stray = shop_opener();
    let none = shop_event(&stray, "NAV", json!({ "route": "/list", "rows": [] }));
    assert!(none["refused"]["error"].as_str().is_some_and(|e| e.contains("program/navigate")), "{none}");
}

#[test]
fn a_lazy_page_in_the_value_loads_only_when_navigated_to() {
    let (kernel, opened) = open_shop(shop_with_lazy_page(), json!({}));
    assert!(opened.get("refused").is_none_or(serde_json::Value::is_null), "{opened}");
    assert_eq!(mounted(&opened["got"]), ["Booter", "HomeT"], "the lazy page is not loaded by the open: {opened}");
    let after = shop_event(&kernel, "NAV", json!({ "route": "/late", "rows": [] }));
    assert_eq!(mounted(&after["got"]), ["LateT"], "{after}");
    assert!(painted(&after["got"]).contains(&"late ran".to_string()), "{after}");
}

#[test]
fn control_a_lazy_page_the_value_does_not_carry_refuses_the_navigate_naming_its_file() {
    let mut program = shop_with_lazy_page();
    program["lazyPrograms"] = json!({});
    let (kernel, opened) = open_shop(program, json!({}));
    assert!(opened.get("refused").is_none_or(serde_json::Value::is_null), "{opened}");
    let after = shop_event(&kernel, "NAV", json!({ "route": "/late", "rows": [] }));
    assert!(
        after["refused"]["error"].as_str().is_some_and(|e| e.contains("Late")),
        "a lazy page with no carried program is refused, not read from disk: {after}"
    );
}

/// `shop()` with a boot trait that hears each route mount's `$MOUNT` and stores a row named after the page.
fn shop_with_mount_seeder(listens: bool) -> serde_json::Value {
    let mut program = shop();
    let mut seeder = json!({
        "name": "Seeder",
        "linkedEntity": "ShopItem",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "events": [{ "key": "SEED", "name": "SEED", "payload": [
                { "name": "page", "type": "string", "required": true },
                { "name": "route", "type": "string", "required": true },
                { "name": "traits", "type": "array", "items": { "type": "string" }, "required": true },
                { "name": "entities", "type": "array", "items": { "type": "string" }, "required": true }
            ] }],
            "transitions": [
                { "from": "idle", "to": "idle", "event": "INIT", "effects": [] },
                { "from": "idle", "to": "idle", "event": "SEED", "effects": [
                    ["persist", "create", "ShopItem", { "name": ["str/concat", "@payload.page", " ", ["str/join", "@payload.entities", ","]] }]
                ] }
            ]
        }
    });
    if listens {
        seeder["listens"] = json!([{ "event": "$MOUNT", "triggers": "SEED" }]);
    }
    program["orbitals"][0]["traits"].as_array_mut().unwrap().push(seeder);
    program["orbitals"][0]["pages"].as_array_mut().unwrap().iter_mut()
        .find(|p| p["name"] == "Bg").unwrap()["traits"].as_array_mut().unwrap().push(json!({ "ref": "Seeder" }));
    program
}

#[test]
fn a_guest_route_open_announces_mount_before_the_page_mounts() {
    let (_, after) = open_shop(shop_with_mount_seeder(true), json!({ "route": "/list" }));
    assert!(after.get("refused").is_none_or(serde_json::Value::is_null), "{after}");
    assert!(
        painted(&after["got"]).contains(&"List ShopItem".to_string()),
        "the $MOUNT listener stored its row before ListT's INIT fetch, with the page and its entities: {after}"
    );
}

#[test]
fn control_a_guest_without_a_mount_listener_mounts_its_page_on_an_empty_store() {
    let (_, after) = open_shop(shop_with_mount_seeder(false), json!({ "route": "/list" }));
    assert!(painted(&after["got"]).contains(&"none listed".to_string()), "{after}");
}

#[test]
fn a_guest_navigate_announces_mount_for_the_new_route_before_mounting_it() {
    let (kernel, _) = open_shop(shop_with_mount_seeder(true), json!({ "route": "/other" }));
    let after = shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [] }));
    let texts = painted(&after["got"]);
    assert!(texts.contains(&"Other ShopItem".to_string()), "the open's own route was announced first: {after}");
}

#[test]
fn edge_an_open_naming_no_route_announces_the_page_it_resolves_to() {
    let (kernel, _) = open_shop(shop_with_mount_seeder(true), json!({}));
    let after = shop_event(&kernel, "NAV", json!({ "route": "/list", "rows": [] }));
    assert!(painted(&after["got"]).contains(&"Home ShopItem".to_string()), "{after}");
}

/// A page that lists both its layout trait and the trait the layout embeds (`@trait.Child`), as an inlined organism does.
fn shop_with_listed_embed() -> serde_json::Value {
    let mut program = shop();
    let traits = program["orbitals"][0]["traits"].as_array_mut().unwrap();
    traits.push(json!({
        "name": "Shell",
        "linkedEntity": "ShopItem",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                ["render-ui", "main", { "type": "stack", "children": ["@trait.Child"] }]
            ] }]
        }
    }));
    traits.push(paints("Child", "child ran"));
    program["orbitals"][0]["pages"].as_array_mut().unwrap().push(
        json!({ "name": "Portal", "path": "/portal", "traits": [{ "ref": "Child" }, { "ref": "Shell" }] }),
    );
    program
}

fn embedded_flag(answer: &serde_json::Value, trait_name: &str) -> Option<bool> {
    answer["rendered"].as_array()?.iter().find(|p| p["trait"] == trait_name).and_then(|p| p["embedded"].as_bool())
}

#[test]
fn a_trait_the_page_lists_and_another_trait_embeds_paints_as_embedded() {
    let (_, after) = open_shop(shop_with_listed_embed(), json!({ "route": "/portal" }));
    assert_eq!(embedded_flag(&after["got"], "Child"), Some(true), "Shell embeds Child, so Child is not a page root: {after}");
    assert_eq!(embedded_flag(&after["got"], "Shell"), Some(false), "control: the layout nobody embeds is the page root: {after}");
}

#[test]
fn edge_navigating_to_a_page_with_a_listed_embed_marks_it_embedded_too() {
    let (kernel, _) = open_shop(shop_with_listed_embed(), json!({}));
    let after = shop_event(&kernel, "NAV", json!({ "route": "/portal", "rows": [] }));
    assert_eq!(embedded_flag(&after["got"], "Child"), Some(true), "{after}");
    assert_eq!(embedded_flag(&after["got"], "Shell"), Some(false), "{after}");
}
