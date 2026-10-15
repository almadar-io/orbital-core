// A row-set reference resolved through a host-answered store (the server role): the resolve is a flight whose
// reads (the by-id window, then the re-count) are requests the host answers. The same `@read` re-check and
// re-count as `RuntimeKernel::resolve_rows`; twin of row_set_delivery.rs' `resolve_rows_*` on a directly readable
// store, and of `almadar-runtime/test/kernel-host-resolve-rows.test.ts` on the JS host.

use orbital_core::embed::{EmbeddedServer, ServerResolve};
use orbital_core::runtime::resumable::{BlockingHost, IoReply, IoRequest};
use orbital_core::runtime::row_query::{row_id, QueryCapabilities, RowPage, RowSetRef};
use orbital_core::runtime::{EntitySource, InMemoryEntityCache};
use orbital_core::{OrbitalSchema, Value};
use serde_json::{json, Value as Json};

fn v(j: &Json) -> Value {
    serde_json::from_value(j.clone()).expect("a value")
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "rsr",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Ticket", "persistence": "persistent",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "status", "type": "string" },
                    { "name": "score", "type": "number" },
                    { "name": "owner", "type": "string" }
                ],
                "read_policy": ["=", ["object/get", "@entity", "owner"], "@user.id"]
            },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "NOOP" }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn store() -> InMemoryEntityCache {
    let cache = InMemoryEntityCache::new();
    for row in [
        json!({ "id": "t1", "status": "open", "score": 3, "owner": "u1" }),
        json!({ "id": "t2", "status": "open", "score": 9, "owner": "u1" }),
        json!({ "id": "t3", "status": "open", "score": 5, "owner": "u1" }),
        json!({ "id": "t4", "status": "closed", "score": 1, "owner": "u1" }),
        json!({ "id": "t5", "status": "open", "score": 8, "owner": "u1" }),
        json!({ "id": "t6", "status": "open", "score": 7, "owner": "u2" }),
    ] {
        cache.write("Ticket", row["id"].as_str().unwrap(), &v(&row)).unwrap();
    }
    cache
}

fn reference(total_mode: Option<&str>) -> RowSetRef {
    let mut query = json!({
        "policy": { "kind": "compare", "field": "owner", "op": "==", "value": "u1" },
        "filter": { "kind": "compare", "field": "status", "op": "==", "value": "open" },
        "order": { "field": "score", "direction": "desc" },
        "limit": 2
    });
    if let Some(mode) = total_mode {
        query["total"] = json!(mode);
    }
    serde_json::from_value(json!({ "entityType": "Ticket", "ids": ["t2", "t5"], "total": 4, "query": query })).unwrap()
}

fn server() -> EmbeddedServer {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_query_capabilities("Ticket", QueryCapabilities::full());
    server
}

/// Resolves `reference` for `viewer`, answering every request from `store`; the requests it made come back too.
fn resolve(server: &EmbeddedServer, store: &InMemoryEntityCache, reference: &RowSetRef, viewer: &str) -> (RowPage, Vec<IoRequest>) {
    let mut log = Vec::new();
    let mut progress = server.resolve_rows(reference, &v(&json!({ "id": viewer }))).unwrap();
    loop {
        match progress {
            ServerResolve::Done(page) => return (page, log),
            ServerResolve::Await { flight, requests } => {
                assert!(log.len() < 8, "the flight does not settle: {log:?}");
                let replies = requests
                    .iter()
                    .map(|p| {
                        log.push(p.request.clone());
                        IoReply {
                            id: p.id,
                            answer: BlockingHost { source: store as &dyn EntitySource, service: None, substrate: None }.answer(&p.request),
                        }
                    })
                    .collect();
                progress = server.resume_resolve(flight, replies).unwrap();
            }
        }
    }
}

fn ids(page: &RowPage) -> Vec<String> {
    page.rows.iter().map(row_id).collect()
}

fn queries(log: &[IoRequest]) -> usize {
    log.iter().filter(|r| matches!(r, IoRequest::QueryRows { .. })).count()
}

#[test]
fn control_a_resolve_through_host_io_returns_the_ids_in_order_and_the_count_of_every_match() {
    let (server, store) = (server(), store());
    let (page, log) = resolve(&server, &store, &reference(None), "u1");
    assert_eq!(ids(&page), vec!["t2", "t5"]);
    assert_eq!(page.total, 4);
    assert_eq!(queries(&log), 2, "the by-id read and the re-count: {log:?}");
    assert!(log.iter().all(|r| !matches!(r, IoRequest::ListRows { .. })), "{log:?}");
    assert_eq!(server.in_flight(), 0);
}

#[test]
fn a_row_gone_or_now_hidden_is_omitted_and_the_total_is_recounted_through_the_host() {
    let (server, store) = (server(), store());
    store.delete("Ticket", "t2").unwrap();
    store
        .write("Ticket", "t5", &v(&json!({ "id": "t5", "status": "open", "score": 8, "owner": "u2" })))
        .unwrap();
    let (page, log) = resolve(&server, &store, &reference(None), "u1");
    assert!(page.rows.is_empty(), "{:?}", ids(&page));
    assert_eq!(page.total, 2, "t1 and t3 are what is left");
    assert_eq!(queries(&log), 2);
}

#[test]
fn the_current_viewers_read_policy_applies_to_the_host_rows() {
    let (server, store) = (server(), store());
    let (page, _) = resolve(&server, &store, &reference(None), "u2");
    assert!(page.rows.is_empty());
    assert_eq!(page.total, 0, "u2 sees none of the u1 reference's matches... only its own, which fail the reference's policy");
}

#[test]
fn total_none_resolves_with_one_read_and_no_recount() {
    let (server, store) = (server(), store());
    store.delete("Ticket", "t2").unwrap();
    let (page, log) = resolve(&server, &store, &reference(Some("none")), "u1");
    assert_eq!(ids(&page), vec!["t5"]);
    assert_eq!(page.total, 1, "the resolved window, not a count of every match");
    assert_eq!(queries(&log), 1, "{log:?}");
    assert!(log.iter().all(|r| match r {
        IoRequest::QueryRows { query, .. } => query.total == Some(orbital_core::runtime::row_query::RowTotalMode::None),
        _ => true,
    }));
}

#[test]
fn a_host_without_a_query_push_down_is_listed_and_the_result_is_the_same() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let store = store();
    let (page, log) = resolve(&server, &store, &reference(None), "u1");
    assert_eq!(ids(&page), vec!["t2", "t5"]);
    assert_eq!(page.total, 4);
    assert_eq!(queries(&log), 0);
    assert!(log.iter().any(|r| matches!(r, IoRequest::ListRows { .. })), "{log:?}");
}

#[test]
fn an_abandoned_resolve_flight_is_forgotten_and_cannot_be_resumed() {
    let (server, store) = (server(), store());
    let ServerResolve::Await { flight, .. } = server.resolve_rows(&reference(None), &v(&json!({ "id": "u1" }))).unwrap() else {
        panic!("a host-answered store awaits");
    };
    assert_eq!(server.in_flight(), 1);
    assert!(server.abandon(flight));
    assert_eq!(server.in_flight(), 0);
    assert!(server.resume_resolve(flight, Vec::new()).is_err());
    let _ = store;
}

#[test]
fn the_json_api_round_trips_a_flight() {
    use orbital_core::embed::wire::{IoAnswerWire, IoReplyWire};
    // a typed flight on a twin server supplies the typed request each JSON request stands for
    let (server, twin, store) = (server(), server(), store());
    let reference = reference(None);
    let host = BlockingHost { source: &store as &dyn EntitySource, service: None, substrate: None };
    let mut typed = twin.resolve_rows(&reference, &v(&json!({ "id": "u1" }))).unwrap();
    let mut out: Json = serde_json::from_str(
        &server
            .resolve_rows_json(&serde_json::to_string(&reference).unwrap(), &json!({ "id": "u1" }).to_string())
            .unwrap(),
    )
    .unwrap();
    while out["status"] == "await" {
        let ServerResolve::Await { flight: typed_flight, requests } = typed else { panic!("the flights diverged") };
        let replies: Vec<IoReplyWire> = requests
            .iter()
            .map(|p| IoReplyWire { id: p.id.raw(), answer: IoAnswerWire::from(&host.answer(&p.request)) })
            .collect();
        typed = twin
            .resume_resolve(
                typed_flight,
                replies.iter().cloned().map(IoReply::from).collect(),
            )
            .unwrap();
        out = serde_json::from_str(
            &server
                .resume_resolve_json(out["flight"].as_u64().unwrap(), &serde_json::to_string(&replies).unwrap())
                .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(out["status"], "done");
    assert_eq!(out["total"], json!(4));
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
}
