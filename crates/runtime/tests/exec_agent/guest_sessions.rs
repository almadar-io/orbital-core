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
