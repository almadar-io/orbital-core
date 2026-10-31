//! `fetch … { deliver: ids }` (docs/Almadar_Embedded_Kernel_Speed_Design.md section 5): the success payload's `data` is a
//! `RowSetRef`, a `render-ui` prop holding it leaves the kernel as a `RowSetMarker` leaf (EXEC-STEP-4's one exception),
//! the response's `dataRefs` carries it, `resolve_rows` re-resolves it against the rows as they are for the session's identity
//! (EXEC-RENDER-3 / Q6), and a kernel write to the referenced entity type repaints the slot with reason `rows-changed`.
//! The same fixtures run on the JS engine in `almadar-runtime/test/row-set-delivery-twin.test.ts`.

use {orbital_core_base::client_effect::ClientEffect, orbital_core_runtime::effects::RepaintReason};
use orbital_core_runtime::resumable::{BlockingHost, HostAnswered, IoReply, IoRequest, Progress};
use {orbital_core_runtime::row_query::{as_row_set_marker, QueryCapabilities, RowProjection}, orbital_core_base::row_set::{row_set_marker, RowPredicate, RowSetRef}};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::collections::HashSet;
use std::sync::Arc;

use crate::support::identity;

fn v(j: &Json) -> Value {
    serde_json::from_value(j.clone()).expect("a value")
}

fn j(value: &Value) -> Json {
    serde_json::to_value(value).expect("json")
}

fn schema(extra_fetch: Json, persistence: &str) -> OrbitalSchema {
    let mut fetch = json!({
        "filter": ["=", ["object/get", "@entity", "status"], "open"],
        "orderBy": "score:desc",
        "limit": 2,
        "emit": { "success": "LISTED", "failure": "LIST_FAILED" }
    });
    for (k, val) in extra_fetch.as_object().unwrap() {
        fetch[k] = val.clone();
    }
    serde_json::from_value(json!({
        "name": "rsd",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Ticket", "persistence": persistence,
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "status", "type": "string" },
                    { "name": "score", "type": "number" },
                    { "name": "owner", "type": "string" }
                ],
                "read_policy": ["=", ["object/get", "@entity", "owner"], "@user.id"]
            },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket", "dispatch": if persistence == "runtime" { json!("client") } else { json!(null) },
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "LIST", "effects": [["fetch", "Ticket", fetch]] },
                        { "from": "idle", "to": "idle", "event": "LISTED", "effects": [
                            ["render-ui", "main", { "type": "entity-table", "entity": "@payload.data", "total": "@payload.totalCount" }]
                        ] },
                        { "from": "idle", "to": "idle", "event": "BUMP", "effects": [
                            ["persist", "update", "Ticket", { "id": "t1", "score": 99 }]
                        ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn seeded(kernel_env: RuntimeEnvironment, schema: OrbitalSchema) -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        kernel_env,
        cache.clone() as Arc<dyn EntitySource>,
        KernelConfig::for_environment(kernel_env),
    );
    k.register_schema(schema).unwrap();
    identity::enable(&k, &[]);
    identity::sign_in(&k, "u1", json!({}));
    // Seeded AFTER registering: the cache keys its store by the entity's bound collection.
    for row in [
        json!({ "id": "t1", "status": "open", "score": 3, "owner": "u1" }),
        json!({ "id": "t2", "status": "open", "score": 9, "owner": "u1" }),
        json!({ "id": "t3", "status": "open", "score": 5, "owner": "u1" }),
        json!({ "id": "t4", "status": "closed", "score": 1, "owner": "u1" }),
        json!({ "id": "t5", "status": "open", "score": 8, "owner": "u1" }),
        json!({ "id": "t6", "status": "open", "score": 7, "owner": "u2" }),
    ] {
        cache
            .write("Ticket", row["id"].as_str().unwrap(), &v(&row))
            .unwrap();
    }
    (k, cache)
}

/// `reference` resolved in a session of its own signed in as `subject`.
fn resolve_as(k: &RuntimeKernel, subject: &str, reference: &RowSetRef) -> orbital_core_runtime::row_query::RowPage {
    identity::sign_in_as(k, subject, subject, json!({}));
    let _session = k.enter_session(subject);
    k.resolve_rows(reference).unwrap()
}

fn list(k: &RuntimeKernel) -> EventResponse {
    let out = k.dispatch("Tickets", None, "LIST", Value::Null).unwrap();
    EventResponse::from_process_results(&out, "Tickets", "LIST")
}

fn painted(r: &EventResponse) -> Json {
    r.client_effects()
        .into_iter()
        .find_map(|e| match e {
            ClientEffect::RenderUi { .. } => e.render_ui_value(),
            _ => None,
        })
        .map(|c| j(&c))
        .expect("a render-ui")
}

#[test]
fn a_deliver_ids_fetch_reaches_the_slot_as_a_marker_and_the_response_carries_the_ref() {
    let (k, _) = seeded(RuntimeEnvironment::Server, schema(json!({ "deliver": "ids" }), "persistent"));
    let r = list(&k);
    let marker = &painted(&r)["entity"];
    assert_eq!(marker["$rowSet"], json!(true));
    let reference = as_row_set_marker(&v(marker)).expect("a row-set marker");
    // viewer u1 sees open t1,t2,t3,t5 (t6 is u2's), ordered by score desc, window 2
    assert_eq!(reference.entity_type, "Ticket");
    assert_eq!(reference.ids, vec!["t2", "t5"]);
    assert_eq!(reference.total, 4);
    assert_eq!(reference.query.limit, Some(2));
    assert_eq!(painted(&r)["total"], json!(4.0));
    assert_eq!(r.data_refs.as_ref().unwrap().get("Ticket"), Some(&reference));
    assert!(r.data.is_none(), "no rows travel: {:?}", r.data);
}

#[test]
fn control_the_default_delivery_still_passes_the_rows_and_no_ref() {
    let (k, _) = seeded(RuntimeEnvironment::Server, schema(json!({}), "persistent"));
    let r = list(&k);
    let entity = &painted(&r)["entity"];
    assert_eq!(entity.as_array().unwrap().len(), 2);
    assert!(r.data_refs.is_none());
    assert_eq!(r.data.as_ref().unwrap()["Ticket"].len(), 2);
}

#[test]
fn a_deliver_ids_fetch_by_id_or_with_include_fails_instead_of_widening() {
    for extra in [json!({ "deliver": "ids", "id": "t1" }), json!({ "deliver": "ids", "include": ["owner"] })] {
        let (k, _) = seeded(RuntimeEnvironment::Server, schema(extra.clone(), "persistent"));
        let out = k.dispatch("Tickets", None, "LIST", Value::Null).unwrap();
        let r = EventResponse::from_process_results(&out, "Tickets", "LIST");
        assert!(r.data_refs.is_none(), "{extra}");
        assert!(r.client_effects().is_empty(), "{extra}: the failure event is not LISTED");
    }
}

fn reference_of(k: &RuntimeKernel) -> RowSetRef {
    let r = list(k);
    r.data_refs.unwrap().remove("Ticket").unwrap()
}

#[test]
fn resolve_rows_returns_the_ids_in_order_and_the_recounted_total() {
    let (k, cache) = seeded(RuntimeEnvironment::Server, schema(json!({ "deliver": "ids" }), "persistent"));
    let reference = reference_of(&k);
    let page = k.resolve_rows(&reference).unwrap();
    let ids: Vec<_> = page.rows.iter().map(orbital_core_runtime::row_query::row_id).collect();
    assert_eq!(ids, vec!["t2", "t5"]);
    assert_eq!(page.total, 4);
    // a row deleted since the fetch is omitted and the total is re-counted, not subtracted
    cache.delete("Ticket", "t2").unwrap();
    let page = k.resolve_rows(&reference).unwrap();
    let ids: Vec<_> = page.rows.iter().map(orbital_core_runtime::row_query::row_id).collect();
    assert_eq!(ids, vec!["t5"]);
    assert_eq!(page.total, 3);
    // a row now outside the fetch's filter is omitted too; total follows the filter
    cache
        .write("Ticket", "t5", &v(&json!({ "id": "t5", "status": "closed", "score": 8, "owner": "u1" })))
        .unwrap();
    let page = k.resolve_rows(&reference).unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(page.total, 2);
}

#[test]
fn resolve_rows_applies_the_current_viewers_read_policy_to_a_tampered_ref() {
    let (k, _) = seeded(RuntimeEnvironment::Server, schema(json!({ "deliver": "ids" }), "persistent"));
    let mut reference = reference_of(&k);
    // t6 is u2's row and is open: a client appends it to the window
    reference.ids.push("t6".to_string());
    let page = k.resolve_rows(&reference).unwrap();
    let ids: Vec<_> = page.rows.iter().map(orbital_core_runtime::row_query::row_id).collect();
    assert_eq!(ids, vec!["t2", "t5"], "u1 cannot read u2's row");
    assert_eq!(page.total, 4);
    // control: u2 resolving the same ids reads only their own row (the ref's compiled policy was u1's, so nothing
    // u1 could not read at dispatch time is revealed either)
    let page = resolve_as(&k, "u2", &reference);
    assert!(page.rows.is_empty(), "the ref's own policy (owner = u1) still applies: {:?}", page.rows);
}

#[test]
fn a_ref_without_its_policy_is_resolved_under_the_viewers_alone() {
    let (k, _) = seeded(RuntimeEnvironment::Server, schema(json!({ "deliver": "ids" }), "persistent"));
    let mut reference = reference_of(&k);
    reference.query.policy = None;
    reference.ids = vec!["t6".into(), "t2".into()];
    let page = resolve_as(&k, "u2", &reference);
    let ids: Vec<_> = page.rows.iter().map(orbital_core_runtime::row_query::row_id).collect();
    assert_eq!(ids, vec!["t6"]);
    assert_eq!(page.total, 1, "u2 sees one open row");
    assert!(matches!(reference.query.filter, Some(RowPredicate::Compare { .. })));
}

#[test]
fn the_marker_round_trips_through_a_value() {
    let reference = RowSetRef {
        entity_type: "Ticket".into(),
        ids: vec!["a".into(), "b".into()],
        total: 7,
        query: Default::default(),
    };
    let leaf = row_set_marker(&reference);
    assert_eq!(as_row_set_marker(&leaf), Some(reference));
    // controls: a plain object, a marker without its ref, and a falsy tag are not markers
    assert_eq!(as_row_set_marker(&v(&json!({ "ref": { "entityType": "T" } }))), None);
    assert_eq!(as_row_set_marker(&v(&json!({ "$rowSet": true }))), None);
    assert_eq!(as_row_set_marker(&v(&json!({ "$rowSet": false, "ref": {} }))), None);
}

// --- EXEC-RENDER-3: a kernel write re-resolves references to the written entity type ---

fn client_with_painted_ref() -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let (k, cache) = seeded(RuntimeEnvironment::Client, schema(json!({ "deliver": "ids" }), "runtime"));
    let reference = RowSetRef {
        entity_type: "Ticket".into(),
        ids: vec!["t2".into(), "t5".into()],
        total: 4,
        query: Default::default(),
    };
    k.dispatch_with_server_leg(
        "Main",
        "Tickets",
        None,
        "LISTED",
        v(&json!({ "data": j(&row_set_marker(&reference)), "totalCount": 4 })),
        None,
    )
    .unwrap();
    // the paint itself settles; nothing is stale yet
    assert!(k.rerender_stale().is_empty());
    (k, cache)
}

fn rows_changed_paints(k: &RuntimeKernel) -> Vec<(String, Json)> {
    let mut response = EventResponse::default();
    response.fold_rerenders(&k.rerender_stale());
    response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter(|e| e.repaint == Some(RepaintReason::RowsChanged))
        .filter_map(|e| match &e.effect {
            ClientEffect::RenderUi { slot, .. } => Some((slot.clone(), j(&e.effect.render_ui_value().unwrap()))),
            _ => None,
        })
        .collect()
}

fn server_says(k: &RuntimeKernel, response: Json) {
    let response: EventResponse = serde_json::from_value(response).expect("a response");
    k.apply_server_response(&response, &HashSet::new()).unwrap();
}

fn persisted(entity: &str, success: bool) -> Json {
    json!({ "success": true, "transitioned": false, "states": {}, "emittedEvents": [],
        "effectResults": [{ "effect": "persist", "action": "update", "entityType": entity, "success": success }] })
}

#[test]
fn a_persist_the_server_ran_for_this_dispatch_repaints_the_slot_with_the_unchanged_ref() {
    let (k, _) = client_with_painted_ref();
    server_says(&k, persisted("Ticket", true));
    let paints = rows_changed_paints(&k);
    assert_eq!(paints.len(), 1, "{paints:?}");
    assert_eq!(paints[0].0, "main");
    assert_eq!(paints[0].1["entity"]["ref"]["ids"], json!(["t2", "t5"]));
    // settled: a second look finds nothing more
    assert!(k.rerender_stale().is_empty());
}

#[test]
fn rows_the_server_response_carries_for_the_entity_repaint_it_too() {
    let (k, _) = client_with_painted_ref();
    server_says(
        &k,
        json!({ "success": true, "transitioned": false, "states": {}, "emittedEvents": [],
            "data": { "Ticket": [{ "id": "t9", "status": "open", "score": 1, "owner": "u1" }] } }),
    );
    assert_eq!(rows_changed_paints(&k).len(), 1);
}

#[test]
fn a_persist_batch_names_the_entity_types_it_wrote() {
    let (k, _) = client_with_painted_ref();
    server_says(
        &k,
        json!({ "success": true, "transitioned": false, "states": {}, "emittedEvents": [],
            "effectResults": [{ "effect": "persist", "action": "batch", "success": true,
                "data": { "completedCount": 1, "totalCount": 1, "operations": [{ "action": "update", "entityType": "Ticket", "id": "t1" }] } }] }),
    );
    assert_eq!(rows_changed_paints(&k).len(), 1);
}

#[test]
fn control_a_client_with_no_write_repaints_nothing() {
    let (k, _) = client_with_painted_ref();
    assert!(rows_changed_paints(&k).is_empty());
}

#[test]
fn control_a_write_to_another_entity_type_does_not_repaint() {
    let (k, _) = client_with_painted_ref();
    server_says(&k, persisted("Other", true));
    assert!(rows_changed_paints(&k).is_empty());
}

#[test]
fn control_a_persist_that_failed_wrote_nothing_and_does_not_repaint() {
    let (k, _) = client_with_painted_ref();
    server_says(&k, persisted("Ticket", false));
    assert!(rows_changed_paints(&k).is_empty());
}

#[test]
fn a_slot_painted_after_the_write_is_not_marked_by_it() {
    let (k, _) = client_with_painted_ref();
    server_says(&k, persisted("Ticket", true));
    let reference = RowSetRef {
        entity_type: "Ticket".into(),
        ids: vec!["t3".into()],
        total: 1,
        query: Default::default(),
    };
    k.dispatch_with_server_leg(
        "Main",
        "Tickets",
        None,
        "LISTED",
        v(&json!({ "data": j(&row_set_marker(&reference)), "totalCount": 1 })),
        None,
    )
    .unwrap();
    // the repaint of the first paint is owed; the fresh paint (the same slot, replaced) is not marked
    assert!(rows_changed_paints(&k).is_empty());
}

// --- a host-answered store: the ids-only query is what crosses ---

fn host_requests(capabilities: Option<QueryCapabilities>) -> (Vec<IoRequest>, EventResponse) {
    host_requests_with(json!({ "deliver": "ids" }), capabilities).0
}

/// The same flight with any extra fetch options; also returns the success event's payload.
fn host_requests_with(
    extra_fetch: Json,
    capabilities: Option<QueryCapabilities>,
) -> ((Vec<IoRequest>, EventResponse), Json) {
    let host_kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(HostAnswered),
        KernelConfig::default(),
    );
    host_kernel
        .register_schema(schema(extra_fetch, "persistent"))
        .unwrap();
    identity::enable(&host_kernel, &[]);
    identity::sign_in(&host_kernel, "u1", json!({}));
    if let Some(capabilities) = capabilities {
        host_kernel.set_query_capabilities("Ticket", capabilities);
    }
    let (_, host) = seeded(RuntimeEnvironment::Server, schema(json!({ "deliver": "ids" }), "persistent"));
    let host_store = Arc::new(InMemoryEntityCache::new());
    for row in host_rows(&host) {
        host_store
            .write("Ticket", orbital_core_runtime::row_query::row_id(&row).as_str(), &row)
            .unwrap();
    }
    let mut log = Vec::new();
    let mut progress = host_kernel
        .start_dispatch("Tickets", None, "LIST", Value::Null, None, None)
        .unwrap();
    let outcomes = loop {
        match progress {
            Progress::Done(out) => break out,
            Progress::Await { continuation, requests } => {
                let replies = requests
                    .iter()
                    .map(|p| {
                        log.push(p.request.clone());
                        IoReply {
                            id: p.id,
                            answer: BlockingHost { source: &*host_store, service: None, substrate: None }
                                .answer(&p.request),
                        }
                    })
                    .collect();
                progress = host_kernel.resume(continuation, replies).unwrap();
            }
        }
    };
    let listed = listed_payload(&outcomes);
    ((log, EventResponse::from_process_results(&outcomes, "Tickets", "LIST")), listed)
}

fn listed_payload(outcomes: &[(String, orbital_core_runtime::process_result::ProcessEventResult)]) -> Json {
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Emit { event, payload } if event == "LISTED" => Some(j(payload)),
            _ => None,
        })
        .expect("a LISTED emit")
}

fn totals_asked(log: &[IoRequest]) -> Vec<Option<orbital_core_base::row_set::RowTotalMode>> {
    log.iter()
        .filter_map(|r| match r {
            IoRequest::QueryRows { query, .. } => Some(query.total),
            _ => None,
        })
        .collect()
}

fn host_rows(cache: &InMemoryEntityCache) -> Vec<Arc<Value>> {
    cache.list("Ticket").unwrap()
}

#[test]
fn on_a_capable_host_only_the_ids_cross_and_the_ref_is_the_same() {
    let (log, response) = host_requests(Some(QueryCapabilities::full()));
    let queries: Vec<_> = log
        .iter()
        .filter_map(|r| match r {
            IoRequest::QueryRows { project, query, .. } => Some((*project, query.limit)),
            _ => None,
        })
        .collect();
    assert_eq!(queries, vec![(RowProjection::Ids, Some(2))], "{log:?}");
    assert!(log.iter().all(|r| !matches!(r, IoRequest::ListRows { .. })), "{log:?}");
    let reference = response.data_refs.unwrap().remove("Ticket").unwrap();
    assert_eq!(reference.ids, vec!["t2", "t5"]);
    assert_eq!(reference.total, 4);
}

#[test]
fn control_a_host_that_cannot_answer_ids_is_listed_and_the_ref_is_still_the_same() {
    let mut capabilities = QueryCapabilities::full();
    capabilities.ids_only = None;
    let (log, response) = host_requests(Some(capabilities));
    assert!(log.iter().all(|r| !matches!(r, IoRequest::QueryRows { .. })), "{log:?}");
    assert!(log.iter().any(|r| matches!(r, IoRequest::ListRows { .. })), "{log:?}");
    let reference = response.data_refs.unwrap().remove("Ticket").unwrap();
    assert_eq!((reference.ids, reference.total), (vec!["t2".to_string(), "t5".to_string()], 4));
}

// --- `total: none` (ruled Q12): the store is told not to count and the payload carries no totalCount ---

use orbital_core_base::row_set::RowTotalMode;

#[test]
fn total_none_on_a_rows_fetch_asks_the_host_for_no_count_and_the_payload_has_no_total_count() {
    let ((log, _), payload) =
        host_requests_with(json!({ "total": "none" }), Some(QueryCapabilities::full()));
    assert_eq!(totals_asked(&log), vec![Some(RowTotalMode::None)], "{log:?}");
    assert_eq!(payload["data"].as_array().unwrap().len(), 2);
    assert!(payload.get("totalCount").is_none(), "{payload}");
}

#[test]
fn control_a_fetch_without_total_none_asks_for_the_exact_total_and_reports_it() {
    let ((log, _), payload) = host_requests_with(json!({}), Some(QueryCapabilities::full()));
    assert_eq!(totals_asked(&log), vec![None], "{log:?}");
    assert_eq!(payload["totalCount"], json!(4.0));
}

#[test]
fn total_none_on_a_host_without_an_exact_count_is_still_pushed() {
    let mut capabilities = QueryCapabilities::full();
    capabilities.exact_total = false;
    let ((log, _), payload) = host_requests_with(json!({ "total": "none" }), Some(capabilities.clone()));
    assert_eq!(totals_asked(&log), vec![Some(RowTotalMode::None)], "{log:?}");
    assert!(payload.get("totalCount").is_none());
    let ((log, _), _) = host_requests_with(json!({}), Some(capabilities));
    assert!(log.iter().all(|r| !matches!(r, IoRequest::QueryRows { .. })), "{log:?}");
}

#[test]
fn total_none_on_a_deliver_ids_fetch_carries_the_window_and_resolve_rows_skips_the_count() {
    let (k, cache) = seeded(
        RuntimeEnvironment::Server,
        schema(json!({ "deliver": "ids", "total": "none" }), "persistent"),
    );
    let r = list(&k);
    assert!(!painted(&r)["total"].is_number(), "no count reaches the slot: {}", painted(&r));
    let reference = r.data_refs.unwrap().remove("Ticket").unwrap();
    assert_eq!(reference.ids, vec!["t2", "t5"]);
    assert_eq!(reference.total, 2, "the window size, not the count of every match");
    assert_eq!(reference.query.total, Some(RowTotalMode::None));
    cache.delete("Ticket", "t2").unwrap();
    let page = k.resolve_rows(&reference).unwrap();
    let ids: Vec<_> = page.rows.iter().map(orbital_core_runtime::row_query::row_id).collect();
    assert_eq!(ids, vec!["t5"]);
    assert_eq!(page.total, 1, "no re-count: the total is the resolved window");
}
