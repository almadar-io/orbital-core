// `ServerEffectResult.code` on a persist update/delete: `no-row-key` when no row key resolves,
// `not-found` when the keyed row is absent, none when it lands. One fixture, read by this suite and by
// `@almadar/runtime` `persist-effect-codes.test.ts` (canonical source orbital-core
// `fixtures/effect-codes`, mirrored into `tests/baked/`).

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use orbital_core_runtime::resumable::{BlockingHost, IoRequest};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize)]
struct Expect {
    success: bool,
    code: Option<String>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    event: String,
    payload: serde_json::Value,
    expect: Expect,
}

#[derive(Deserialize)]
struct Vector {
    schema: OrbitalSchema,
    cases: Vec<Case>,
}

fn vector() -> Vector {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/baked/effect-codes/persist-codes.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("persist-codes.json malformed: {e}"))
}

fn run(vector: &Vector, case: &Case) -> EventResponse {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(vector.schema.clone()).unwrap();
    let payload: Value = serde_json::from_value(case.payload.clone()).unwrap();
    let outcomes = kernel
        .dispatch("Editor", None, &case.event, payload)
        .unwrap();
    EventResponse::from_process_results(&outcomes, "Editor", &case.event)
}

#[test]
fn every_case_reports_the_fixtures_success_and_code() {
    let vector = vector();
    assert!(vector.cases.len() >= 6);
    for case in &vector.cases {
        let response = run(&vector, case);
        let persist = response
            .effect_results
            .iter()
            .flatten()
            .find(|r| serde_json::to_value(r).unwrap()["effect"] == "persist")
            .unwrap_or_else(|| {
                panic!(
                    "{}: no persist result in {:?}",
                    case.name, response.effect_results
                )
            });
        let wire = serde_json::to_value(persist).unwrap();
        assert_eq!(
            wire["success"], case.expect.success,
            "{}: {wire}",
            case.name
        );
        assert_eq!(
            wire.get("code").and_then(serde_json::Value::as_str),
            case.expect.code.as_deref(),
            "{}: {wire}",
            case.name
        );
    }
}

// EXEC-STORE-4: a store-bound entity's persist/fetch is answered by its driver orbital in-kernel.
fn store_program(bound: bool) -> OrbitalSchema {
    store_program_seeded(bound, false)
}

fn store_program_seeded(bound: bool, seeded: bool) -> OrbitalSchema {
    store_program_querying(bound, seeded, None)
}

/// `query`: the driver's declared `queryCapabilities` and whether it answers ROWS_QUERY (`true`) or declines it.
fn store_program_querying(bound: bool, seeded: bool, query: Option<bool>) -> OrbitalSchema {
    serde_json::from_value(store_program_json(bound, seeded, query)).unwrap()
}

fn store_program_json(bound: bool, seeded: bool, query: Option<bool>) -> serde_json::Value {
    let mut ticket = serde_json::json!({ "name": "Ticket", "persistence": "persistent",
        "fields": [{ "name": "id", "type": "string" }, { "name": "used", "type": "boolean" }] });
    if bound {
        ticket["store"] = serde_json::json!("Rows");
    }
    if seeded {
        ticket["instances"] = serde_json::json!([{ "id": "t1", "used": false }]);
    }
    let emit = |event: &str, body: serde_json::Value| serde_json::json!([["emit", event, body]]);
    let first_op_id = serde_json::json!(["object/get", ["array/nth", "@payload.batch.ops", 0], "id"]);
    let mirror_then = |event: &str, body: serde_json::Value| {
        serde_json::json!([["persist", "create", "Mirror", { "id": "@payload.key", "batch": "@payload.batch" }], ["emit", event, body]])
    };
    let mut program = serde_json::json!({ "name": "store-seam", "orbitals": [
        { "name": "Tickets", "pages": [], "entity": ticket, "traits": [{ "name": "Desk", "linkedEntity": "Ticket",
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "events": [{ "key": "TAKE", "name": "TAKE" }, { "key": "TAKE_UNUSED", "name": "TAKE_UNUSED" }, { "key": "LOOK", "name": "LOOK" }],
                "transitions": [
                    { "from": "idle", "to": "idle", "event": "TAKE", "effects": [["persist", "delete", "Ticket", "@payload.id"]] },
                    { "from": "idle", "to": "idle", "event": "TAKE_UNUSED", "effects": [["persist", "delete", "Ticket", "@payload.id",
                        { "where": ["fn", "row", ["=", "@row.used", false]] }]] },
                    { "from": "idle", "to": "idle", "event": "LOOK", "effects": [["fetch", "Ticket",
                        { "id": "@payload.id", "emit": { "success": "SEEN", "failure": "UNSEEN" } }]] }
                ] } }] },
        { "name": "Rows", "pages": [], "entity": { "name": "Mirror", "persistence": "persistent",
            "fields": [{ "name": "id", "type": "string" }, { "name": "batch", "type": "object" }] },
          "traits": [{ "name": "RowsDriver", "linkedEntity": "Mirror",
            "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                "events": [{ "key": "ROW_READ", "name": "ROW_READ" }, { "key": "ROW_LIST", "name": "ROW_LIST" }, { "key": "ROWS_QUERY", "name": "ROWS_QUERY" }, { "key": "ROWS_WRITE", "name": "ROWS_WRITE" }],
                "transitions": [
                    { "from": "ready", "to": "ready", "event": "ROW_READ", "guard": ["=", "@payload.id", "t1"],
                      "effects": emit("ROW_FOUND", serde_json::json!({ "key": "@payload.key", "row": { "id": "t1", "used": false } })) },
                    { "from": "ready", "to": "ready", "event": "ROW_READ", "guard": ["!=", "@payload.id", "t1"],
                      "effects": emit("ROW_FOUND", serde_json::json!({ "key": "@payload.key" })) },
                    { "from": "ready", "to": "ready", "event": "ROW_LIST",
                      "effects": emit("ROWS_LISTED", serde_json::json!({ "key": "@payload.key", "rows": [{ "id": "t1", "used": false }] })) },
                    { "from": "ready", "to": "ready", "event": "ROWS_QUERY", "effects": match query {
                        Some(false) => emit("STORE_UNSUPPORTED", serde_json::json!({ "key": "@payload.key", "reason": "no index" })),
                        _ => emit("ROWS_PAGE", serde_json::json!({ "key": "@payload.key", "rows": [{ "id": "t1", "used": true }], "total": 1 })),
                    } },
                    { "from": "ready", "to": "ready", "event": "ROWS_WRITE", "guard": ["=", first_op_id.clone(), "t1"],
                      "effects": mirror_then("ROWS_WRITTEN", serde_json::json!({ "key": "@payload.key", "outcomes": [{ "kind": "deleted", "before": { "id": "t1", "used": false } }] })) },
                    { "from": "ready", "to": "ready", "event": "ROWS_WRITE", "guard": ["!=", first_op_id, "t1"],
                      "effects": mirror_then("ROWS_WRITTEN", serde_json::json!({ "key": "@payload.key", "outcomes": [{ "kind": "not-found" }] })) }
                ] } }] }
    ] });
    if query.is_some() {
        let full = serde_json::to_value(orbital_core_runtime::row_query::QueryCapabilities::full()).unwrap();
        program["orbitals"][1]["config"] = serde_json::json!({ "queryCapabilities": { "type": "QueryCapabilities", "default": full } });
    }
    program
}

fn store_run(bound: bool, event: &str, id: &str) -> Vec<serde_json::Value> {
    store_run_program(store_program(bound), bound, event, id).0
}

fn store_run_program(program: OrbitalSchema, bound: bool, event: &str, id: &str) -> (Vec<serde_json::Value>, Vec<IoRequest>) {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(program).unwrap();
    let host_rows = InMemoryEntityCache::new();
    let asked = std::sync::Mutex::new(Vec::new());
    let payload: Value = serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
    let progress = kernel.start_dispatch("Desk", None, event, payload, None, None).unwrap();
    let outcomes = orbital_core_runtime::resumable::drive_blocking(&kernel, progress, &|request| {
        let ticket = matches!(request, IoRequest::ReadRow { entity_type, .. } | IoRequest::DeleteRow { entity_type, .. } if entity_type == "Ticket");
        assert!(!(bound && ticket), "a store-bound row ask reached the host: {request:?}");
        asked.lock().unwrap().push(request.clone());
        BlockingHost { source: &host_rows, service: None, substrate: None }.answer(request)
    })
    .unwrap_or_else(|e| panic!("{event} {id}: {e:?}"));
    let results = EventResponse::from_process_results(&outcomes, "Desk", event)
        .effect_results
        .iter()
        .flatten()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect();
    (results, asked.into_inner().unwrap())
}

fn mirrored_batches(asked: &[IoRequest]) -> Vec<serde_json::Value> {
    asked
        .iter()
        .filter_map(|r| match r {
            IoRequest::CreateRow { entity_type, data } if entity_type == "Mirror" => {
                Some(orbital_core_base::json_value::value_to_json(data)["batch"].clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_bound_entitys_instances_are_seeded_through_its_driver_never_dropped() {
    let (_, asked) = store_run_program(store_program_seeded(true, true), true, "LOOK", "t1");
    let batches = mirrored_batches(&asked);
    assert_eq!(batches.len(), 1, "{asked:?}");
    let ops = batches[0]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1, "{batches:?}");
    assert_eq!((ops[0]["kind"].as_str(), ops[0]["entityType"].as_str(), ops[0]["data"]["id"].as_str()), (Some("create"), Some("Ticket"), Some("t1")));
    let (_, unseeded) = store_run_program(store_program_seeded(true, false), true, "LOOK", "t1");
    assert!(mirrored_batches(&unseeded).is_empty(), "{unseeded:?}");
}

// The seed batch is a barrier: under the one dispatch lane (EXEC-CONC-0) a dispatch started while another's seed
// leg is parked on the host waits for that dispatch to commit, so it never steps before the seed landed.
#[test]
fn a_dispatch_started_while_the_seed_is_in_flight_writes_the_seed_before_its_first_step() {
    use orbital_core_runtime::resumable::Progress;
    let kernel = Arc::new(RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    ));
    kernel.register_schema(store_program_seeded(true, true)).unwrap();
    let payload = || -> Value { serde_json::from_value(serde_json::json!({ "id": "t1" })).unwrap() };
    let first = kernel.start_dispatch("Desk", None, "LOOK", payload(), None, None).unwrap();
    let seed_asks = match &first {
        Progress::Await { requests, .. } => requests
            .iter()
            .filter(|r| matches!(&r.request, IoRequest::ReadRow { entity_type, .. } if entity_type == "Mirror"))
            .count(),
        Progress::Done(_) => 0,
    };
    assert_eq!(seed_asks, 1, "{first:?}");
    let host_rows = Arc::new(InMemoryEntityCache::new());
    let (tx, rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn({
        let (kernel, host_rows) = (kernel.clone(), host_rows.clone());
        move || {
            let progress = kernel.start_dispatch("Desk", None, "LOOK", payload(), None, None).unwrap();
            tx.send(()).unwrap();
            let asked = std::sync::Mutex::new(Vec::new());
            let outcomes = orbital_core_runtime::resumable::drive_blocking(&kernel, progress, &|request| {
                asked.lock().unwrap().push(request.clone());
                BlockingHost { source: host_rows.as_ref(), service: None, substrate: None }.answer(request)
            })
            .unwrap();
            (outcomes, asked.into_inner().unwrap())
        }
    });
    assert!(rx.recv_timeout(std::time::Duration::from_millis(250)).is_err(), "the second dispatch stepped while the seed was in flight");
    let host = |request: &IoRequest| BlockingHost { source: host_rows.as_ref(), service: None, substrate: None }.answer(request);
    let seen = |outcomes: &orbital_core_runtime::resumable::DispatchOutcomes| {
        EventResponse::from_process_results(outcomes, "Desk", "LOOK")
            .effect_results
            .iter()
            .flatten()
            .any(|r| serde_json::to_value(r).unwrap()["target"] == "SEEN")
    };
    let outcomes = orbital_core_runtime::resumable::drive_blocking(&kernel, first, &host).unwrap();
    assert!(seen(&outcomes));
    let (outcomes, asked) = second.join().unwrap();
    assert!(seen(&outcomes));
    assert!(mirrored_batches(&asked).is_empty(), "a landed seed is written once: {asked:?}");
}

// resolve-rows on a bound entity reads the page its driver answers, the same as a fetch.
fn resolve_t1(bound: bool) -> Result<orbital_core_runtime::row_query::RowPage, String> {
    resolve_t1_in(store_program(bound), bound)
}

fn resolve_t1_in(program: OrbitalSchema, bound: bool) -> Result<orbital_core_runtime::row_query::RowPage, String> {
    use orbital_core_runtime::resumable::{IoReply, ResolveProgress};
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(program).unwrap();
    let reference = serde_json::from_value(serde_json::json!({ "entityType": "Ticket", "ids": ["t1"], "total": 1, "query": {} })).unwrap();
    let viewer = Value::Null;
    let host_rows = InMemoryEntityCache::new();
    let mut progress = kernel.start_resolve_rows(&reference, &viewer).map_err(|e| e.to_string())?;
    loop {
        match progress {
            ResolveProgress::Done(page) => return Ok(page),
            ResolveProgress::Await { continuation, requests } => {
                let replies = requests
                    .iter()
                    .map(|p| {
                        let ticket = matches!(&p.request, IoRequest::QueryRows { entity_type, .. } if entity_type == "Ticket");
                        assert!(!(bound && ticket), "a store-bound row ask reached the host: {p:?}");
                        IoReply { id: p.id, answer: BlockingHost { source: &host_rows, service: None, substrate: None }.answer(&p.request) }
                    })
                    .collect();
                progress = kernel.resume_resolve_rows(continuation, replies).map_err(|e| e.to_string())?;
            }
        }
    }
}

#[test]
fn a_bound_entitys_resolve_rows_reads_the_page_its_driver_answers() {
    let page = resolve_t1(true).unwrap();
    assert_eq!((page.rows.len(), page.total), (1, 1), "{page:?}");
    let control = resolve_t1(false).unwrap();
    assert!(control.rows.is_empty(), "{control:?}");
}

// A driver declaring queryCapabilities gets the query (ROWS_QUERY); STORE_UNSUPPORTED or none falls back to ROW_LIST.
#[test]
fn a_bound_resolve_pushes_its_query_to_a_driver_that_declares_capabilities() {
    let used = |query: Option<bool>| {
        let page = resolve_t1_in(store_program_querying(true, false, query), true).unwrap();
        assert_eq!(page.rows.len(), 1, "{page:?}");
        serde_json::to_value(&page.rows[0]).unwrap()["used"].clone()
    };
    assert_eq!(used(Some(true)), true, "pushed down: the driver's ROWS_PAGE row");
    assert_eq!(used(Some(false)), false, "declined: the listed row");
    assert_eq!(used(None), false, "control: no capabilities, the listed row");
}

fn persist_wire(results: &[serde_json::Value]) -> &serde_json::Value {
    results.iter().find(|r| r["effect"] == "persist").unwrap_or_else(|| panic!("no persist result in {results:?}"))
}

#[test]
fn a_bound_entitys_persist_delete_is_answered_by_its_driver_orbital() {
    let taken = store_run(true, "TAKE", "t1");
    assert_eq!(persist_wire(&taken)["success"], true, "{taken:?}");
    let gone = store_run(true, "TAKE", "nope");
    assert_eq!(persist_wire(&gone)["success"], false, "{gone:?}");
}

#[test]
fn control_an_unbound_entity_reads_the_kernel_store_where_the_row_is_absent() {
    let results = store_run(false, "TAKE", "t1");
    assert_eq!(persist_wire(&results)["success"], false, "{results:?}");
}

#[test]
fn a_bound_entitys_fetch_reads_the_row_its_driver_answers() {
    let seen = store_run(true, "LOOK", "t1");
    assert!(seen.iter().any(|r| r["effect"] == "emit" && r["target"] == "SEEN"), "{seen:?}");
    let fetched = seen.iter().find(|r| r["effect"] == "fetch").unwrap();
    assert_eq!(fetched["data"], serde_json::json!([{ "id": "t1", "used": false }]), "{seen:?}");
    let control = store_run(false, "LOOK", "t1");
    assert!(!control.iter().any(|r| r["effect"] == "emit" && r["target"] == "SEEN"), "{control:?}");
}

// `persist delete … { where: (fn row …) }`: ONE conditional op; a row the guard rejects is `not-found` (EXEC-STEP-8).
fn guarded_take(id: &str, guarded: bool) -> serde_json::Value {
    let program = store_program_seeded(false, false);
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(program).unwrap();
    for (row, used) in [("t1", false), ("t2", true)] {
        let data: Value = serde_json::from_value(serde_json::json!({ "id": row, "used": used })).unwrap();
        kernel_create(&kernel, &data);
    }
    let payload: Value = serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
    let event = if guarded { "TAKE_UNUSED" } else { "TAKE" };
    let outcomes = kernel.dispatch("Desk", None, event, payload).unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Desk", event);
    let results: Vec<serde_json::Value> =
        response.effect_results.iter().flatten().map(|r| serde_json::to_value(r).unwrap()).collect();
    persist_wire(&results).clone()
}

fn kernel_create(kernel: &RuntimeKernel, data: &Value) {
    kernel.entity_source().create("Ticket", data).unwrap();
}

#[test]
fn a_where_guard_deletes_a_matching_row_and_reports_a_rejected_one_not_found() {
    let taken = guarded_take("t1", true);
    assert_eq!(taken["success"], true, "{taken}");
    let rejected = guarded_take("t2", true);
    assert_eq!((rejected["success"].as_bool(), rejected["code"].as_str()), (Some(false), Some("not-found")), "{rejected}");
    let control = guarded_take("t2", false);
    assert_eq!(control["success"], true, "{control}");
}

#[test]
fn a_bound_entitys_guarded_take_sends_the_compiled_guard_to_its_driver() {
    let (results, asked) = store_run_program(store_program(true), true, "TAKE_UNUSED", "t1");
    assert_eq!(persist_wire(&results)["success"], true, "{results:?}");
    let batches = mirrored_batches(&asked);
    let deletes: Vec<&serde_json::Value> =
        batches.iter().flat_map(|b| b["ops"].as_array().unwrap()).filter(|op| op["kind"] == "delete").collect();
    assert_eq!(deletes.len(), 1, "{batches:?}");
    assert_eq!(deletes[0]["guard"], serde_json::json!({ "kind": "compare", "field": "used", "op": "==", "value": false }), "{batches:?}");
    let (_, unguarded) = store_run_program(store_program(true), true, "TAKE", "t1");
    let plain: Vec<serde_json::Value> = mirrored_batches(&unguarded);
    assert!(plain.iter().flat_map(|b| b["ops"].as_array().unwrap().clone()).all(|op| op.get("guard").is_none()), "{plain:?}");
}

// EXEC-STORE-6: an entity that declares `@index` paths bound to a driver whose `queryCapabilities.indexes` is
// `runtime` or `deploy` has them sent to the driver once, as one ROWS_ENSURE_INDEXES, before its first row ask.
fn indexed_store_program(realization: Option<&str>, ensure_fails: bool) -> OrbitalSchema {
    let mut program = store_program_json(true, false, Some(true));
    program["orbitals"][0]["entity"]["fields"]
        .as_array_mut()
        .unwrap()
        .extend([serde_json::json!({ "name": "status", "type": "string" }), serde_json::json!({ "name": "owner", "type": "string" })]);
    program["orbitals"][0]["entity"]["indexes"] =
        serde_json::json!([{ "fields": ["status"] }, { "fields": ["owner", "status"], "orderBy": "id:desc" }]);
    let mut capabilities = serde_json::to_value(orbital_core_runtime::row_query::QueryCapabilities::full()).unwrap();
    if let Some(realization) = realization {
        capabilities["indexes"] = serde_json::json!(realization);
    }
    program["orbitals"][1]["config"] = serde_json::json!({ "queryCapabilities": { "type": "QueryCapabilities", "default": capabilities } });
    let driver = &mut program["orbitals"][1]["traits"][0]["stateMachine"];
    driver["events"].as_array_mut().unwrap().push(serde_json::json!({ "key": "ROWS_ENSURE_INDEXES", "name": "ROWS_ENSURE_INDEXES" }));
    let effects = if ensure_fails {
        serde_json::json!([["emit", "STORE_FAILED", { "key": "@payload.key", "message": "cannot create index" }]])
    } else {
        serde_json::json!([
            ["persist", "create", "Mirror", { "id": "@payload.key", "batch": { "entityType": "@payload.entityType", "indexes": "@payload.indexes" } }],
            ["emit", "ROWS_INDEXES_ENSURED", { "key": "@payload.key" }]
        ])
    };
    driver["transitions"].as_array_mut().unwrap().push(
        serde_json::json!({ "from": "ready", "to": "ready", "event": "ROWS_ENSURE_INDEXES", "effects": effects }),
    );
    serde_json::from_value(program).unwrap()
}

/// The effect results of each event, in order, and every request the host was asked, over one kernel.
fn run_events(program: OrbitalSchema, events: &[&str]) -> (Vec<Vec<serde_json::Value>>, Vec<IoRequest>) {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(program).unwrap();
    let host_rows = InMemoryEntityCache::new();
    let asked = std::sync::Mutex::new(Vec::new());
    let mut results = Vec::new();
    for event in events {
        let payload: Value = serde_json::from_value(serde_json::json!({ "id": "t1" })).unwrap();
        let progress = kernel.start_dispatch("Desk", None, event, payload, None, None).unwrap();
        let outcomes = orbital_core_runtime::resumable::drive_blocking(&kernel, progress, &|request| {
            asked.lock().unwrap().push(request.clone());
            BlockingHost { source: &host_rows, service: None, substrate: None }.answer(request)
        })
        .unwrap_or_else(|e| panic!("{event}: {e:?}"));
        results.push(
            EventResponse::from_process_results(&outcomes, "Desk", event)
                .effect_results
                .iter()
                .flatten()
                .map(|r| serde_json::to_value(r).unwrap())
                .collect(),
        );
    }
    (results, asked.into_inner().unwrap())
}

fn ensured(asked: &[IoRequest]) -> Vec<serde_json::Value> {
    mirrored_batches(asked).into_iter().filter(|b| b.get("indexes").is_some()).collect()
}

fn declared_paths() -> serde_json::Value {
    serde_json::json!([{ "fields": ["status"] }, { "fields": ["owner", "status"], "orderBy": "id:desc" }])
}

fn saw(results: &[serde_json::Value], target: &str) -> bool {
    results.iter().any(|r| r["target"] == target)
}

#[test]
fn a_runtime_driver_is_sent_the_declared_paths_once_before_the_first_row_ask() {
    let (results, asked) = run_events(indexed_store_program(Some("runtime"), false), &["LOOK"]);
    let sent = ensured(&asked);
    assert_eq!(sent.len(), 1, "{asked:?}");
    assert_eq!((sent[0]["entityType"].as_str(), &sent[0]["indexes"]), (Some("Ticket"), &declared_paths()));
    assert!(saw(&results[0], "SEEN"), "{results:?}");
}

#[test]
fn a_deploy_driver_is_sent_the_declared_paths_too() {
    let (_, asked) = run_events(indexed_store_program(Some("deploy"), false), &["LOOK"]);
    assert_eq!(ensured(&asked).len(), 1, "{asked:?}");
}

#[test]
fn a_replayed_step_and_later_dispatches_do_not_resend_the_ensure() {
    let (results, asked) = run_events(indexed_store_program(Some("runtime"), false), &["LOOK", "LOOK", "TAKE"]);
    assert_eq!(ensured(&asked).len(), 1, "{asked:?}");
    assert!(results.iter().all(|r| !r.is_empty()), "{results:?}");
}

#[test]
fn control_a_driver_declaring_none_is_sent_no_ensure() {
    let (results, asked) = run_events(indexed_store_program(Some("none"), false), &["LOOK"]);
    assert!(ensured(&asked).is_empty(), "{asked:?}");
    assert!(saw(&results[0], "SEEN"), "{results:?}");
}

#[test]
fn control_a_driver_declaring_nothing_is_sent_no_ensure() {
    let (_, asked) = run_events(indexed_store_program(None, false), &["LOOK"]);
    assert!(ensured(&asked).is_empty(), "{asked:?}");
}

#[test]
fn control_an_entity_declaring_no_paths_is_sent_no_ensure() {
    let (_, asked) = run_events(
        serde_json::from_value({
            let mut p = store_program_json(true, false, Some(true));
            let mut c = serde_json::to_value(orbital_core_runtime::row_query::QueryCapabilities::full()).unwrap();
            c["indexes"] = serde_json::json!("runtime");
            p["orbitals"][1]["config"] = serde_json::json!({ "queryCapabilities": { "type": "QueryCapabilities", "default": c } });
            p
        })
        .unwrap(),
        &["LOOK"],
    );
    assert!(ensured(&asked).is_empty(), "{asked:?}");
}

#[test]
fn a_failed_ensure_fails_the_first_row_ask_with_the_named_error_and_scans_nothing() {
    let (results, _) = run_events(indexed_store_program(Some("runtime"), true), &["LOOK"]);
    assert!(saw(&results[0], "UNSEEN") && !saw(&results[0], "SEEN"), "{results:?}");
    let text = serde_json::to_string(&results[0]).unwrap();
    assert!(text.contains("could not ensure the indexes of Ticket: cannot create index"), "{text}");
}
