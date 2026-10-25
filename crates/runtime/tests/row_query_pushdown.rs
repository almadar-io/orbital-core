//! The row-query contract on the Rust side: the shared push-down fixtures
//! (`tests/baked/pushdown/*.json`, mirrored from `tests/baked/pushdown`), the sexpr
//! compiler against the interpreter, and the kernel's tiers (pushed / compiled / interpreted) and writes.

use orbital_core_eval::evaluator::BindingContext;
use orbital_core_runtime::entity_access::apply_read_access;
use orbital_core_runtime::resumable::{BlockingHost, DispatchOutcomes, HostAnswered, IoAnswer, IoReply, IoRequest, Progress};
use {orbital_core_runtime::row_query::{apply_write, apply_write_batch, compile_predicate, query_unsupported_reason, resolve_row_set, run_ids, run_query, CreateIds, NotExpressible, PredicateRole, QueryCapabilities, RowCommitWrite, RowPage, RowProjection, RowTable, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome, WriteCapabilities}, orbital_core_base::row_set::{RowPredicate, RowQuery, RowSetRef}};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn fixture(name: &str) -> Json {
    let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/baked/pushdown")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn v(j: &Json) -> Value {
    serde_json::from_value(j.clone()).expect("a value")
}

fn arcs(rows: &Json) -> Vec<Arc<Value>> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| Arc::new(v(r)))
        .collect()
}

fn table(rows: &Json) -> RowTable {
    arcs(rows)
        .into_iter()
        .map(|r| {
            (
                r.as_object().unwrap()["id"].as_str().unwrap().to_string(),
                r,
            )
        })
        .collect()
}

fn ids_of(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(orbital_core_runtime::row_query::row_id)
        .collect()
}

/// JSON with every number as a float, so `10` and `10.0` compare equal.
fn norm(j: &Json) -> Json {
    match j {
        Json::Number(n) => json!(n.as_f64()),
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, x)| (k.clone(), norm(x))).collect()),
        other => other.clone(),
    }
}

/// The requests that touch rows, without the frame's own read of its `singleton` row.
fn data_requests(log: &[IoRequest]) -> Vec<&IoRequest> {
    log.iter()
        .filter(|r| !matches!(r, IoRequest::ReadRow { id, .. } if id == "singleton"))
        .collect()
}

fn expected_ids(expect: &Json) -> Vec<String> {
    expect["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_string())
        .collect()
}

fn ctx_of(bindings: &Json) -> BindingContext {
    let mut ctx = BindingContext::new();
    if let Some(user) = bindings.get("user") {
        ctx.set_user(v(user));
    }
    ctx
}

fn all_required(_: &str) -> bool {
    true
}

#[test]
fn every_query_case_runs_to_its_recorded_page_and_round_trips_its_json() {
    let f = fixture("query-cases.json");
    let rows = arcs(&f["rows"]);
    let cases = f["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let query: RowQuery = serde_json::from_value(case["query"].clone()).expect(name);
        assert_eq!(
            norm(&serde_json::to_value(&query).unwrap()),
            norm(&case["query"]),
            "{name}: wire form"
        );
        let page = run_query(&rows, &query).unwrap();
        assert_eq!(ids_of(&page.rows), expected_ids(&case["expect"]), "{name}");
        assert_eq!(
            page.total as u64,
            case["expect"]["total"].as_u64().unwrap(),
            "{name}: total"
        );
        let idp = run_ids(&rows, &query).unwrap();
        assert_eq!(
            idp.ids,
            expected_ids(&case["expect"]),
            "{name}: ids projection"
        );
        assert_eq!(idp.total, page.total, "{name}");
        // the default `query_page` of a source is the same run
        let cache = InMemoryEntityCache::new();
        for r in &rows {
            cache
                .write("Ticket", r.as_object().unwrap()["id"].as_str().unwrap(), r)
                .unwrap();
        }
        let via_source = cache.query_page("Ticket", &query).unwrap_rows();
        assert_eq!(via_source, page, "{name}: source query_page");
    }
}

#[test]
fn every_sexpr_compiles_to_its_recorded_query() {
    let f = fixture("query-cases.json");
    for case in f["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let ctx = ctx_of(&case["bindings"]);
        let want: RowQuery = serde_json::from_value(case["query"].clone()).unwrap();
        let sexpr = &case["sexpr"];
        let policy = sexpr.get("readPolicy").map(|p| {
            compile_predicate(&v(p), &ctx, PredicateRole::Policy, &all_required).expect(name)
        });
        let filter = sexpr.get("filter").map(|p| {
            compile_predicate(&v(p), &ctx, PredicateRole::Filter, &all_required).expect(name)
        });
        assert_eq!(policy, want.policy, "{name}: policy");
        assert_eq!(filter, want.filter, "{name}: filter");
    }
}

#[test]
fn an_ordering_comparison_compiles_only_on_a_required_field() {
    let ctx = BindingContext::new();
    let expr = v(&json!([">", ["object/get", "@entity", "priority"], 3]));
    let err = compile_predicate(&expr, &ctx, PredicateRole::Filter, &|_| false).unwrap_err();
    assert_eq!(err, NotExpressible::NullSemantics("priority".to_string()));
    assert_eq!(err.reason(), "null-semantics");
    assert!(compile_predicate(&expr, &ctx, PredicateRole::Filter, &|f| f == "priority").is_ok());
    // control: equality has no null gap and compiles without a declaration
    let eq = v(&json!(["=", ["object/get", "@entity", "priority"], 3]));
    assert!(compile_predicate(&eq, &ctx, PredicateRole::Filter, &|_| false).is_ok());
}

#[test]
fn a_function_call_on_a_field_stays_on_the_interpreter() {
    let ctx = BindingContext::new();
    let expr = v(&json!([
        "=",
        ["str/lower", ["object/get", "@entity", "title"]],
        "x"
    ]));
    assert!(matches!(
        compile_predicate(&expr, &ctx, PredicateRole::Filter, &all_required),
        Err(NotExpressible::Operator(_))
    ));
    let unbound = v(&json!([
        "=",
        ["object/get", "@entity", "owner"],
        "@user.id"
    ]));
    assert!(matches!(
        compile_predicate(&unbound, &ctx, PredicateRole::Filter, &all_required),
        Err(NotExpressible::Binding(_))
    ));
    assert_eq!(
        compile_predicate(&unbound, &ctx, PredicateRole::Policy, &all_required).unwrap(),
        RowPredicate::Const { value: false }
    );
}

/// The gate (D-A21): over rows with missing, null, zero, empty and mixed fields, every expression the compiler
/// accepts selects exactly the rows the interpreter does; one it refuses is simply not compared.
#[test]
fn compiled_and_interpreted_select_the_same_rows_over_missing_and_null_fields() {
    let rows_json = json!([
        { "id": "a", "n": 1, "s": "Alpha", "t": ["x", "y"], "b": true, "o": "u1" },
        { "id": "b", "n": 0, "s": "", "t": [], "b": false, "o": "u2" },
        { "id": "c", "n": null, "s": null, "t": null, "b": null, "o": null },
        { "id": "d" },
        { "id": "e", "n": 5, "s": "beta ALPHA", "t": ["y"], "b": true },
        { "id": "f", "n": -2, "s": "zeta", "t": ["z"], "b": false, "o": "u1" }
    ]);
    let rows = arcs(&rows_json);
    let f = |name: &str| json!(["object/get", "@entity", name]);
    let exprs: Vec<Json> = vec![
        json!(["=", f("n"), 1]),
        json!(["=", f("n"), 0]),
        json!(["!=", f("n"), 1]),
        json!(["=", f("n"), null]),
        json!(["!=", f("n"), null]),
        json!(["=", f("s"), "Alpha"]),
        json!(["!=", f("s"), "Alpha"]),
        json!(["=", f("b"), true]),
        json!(["!=", f("b"), true]),
        json!(["=", f("o"), "@user.id"]),
        json!(["!=", f("o"), "@user.id"]),
        json!(["array/includes", ["u1", "u2"], f("o")]),
        json!(["not", ["array/includes", ["u1", "u2"], f("o")]]),
        json!(["array/includes", f("t"), "y"]),
        json!(["not", ["array/includes", f("t"), "y"]]),
        json!(["str/includes", f("s"), "lpha"]),
        json!(["not", ["str/includes", f("s"), "lpha"]]),
        json!(["str/includes", ["str/lower", f("s")], "alpha"]),
        json!(["not", ["str/includes", ["str/lower", f("s")], "alpha"]]),
        json!(["and", ["=", f("b"), true], ["!=", f("n"), 5]]),
        json!(["or", ["=", f("b"), false], ["=", f("o"), "u1"]]),
        json!(["not", ["and", ["=", f("b"), true], ["=", f("o"), "u1"]]]),
        json!(["not", ["not", ["=", f("o"), "u1"]]]),
        json!(["<", f("n"), 3]),
        json!([">=", f("n"), 0]),
        json!(["not", [">", f("n"), 0]]),
        json!(["<", f("s"), "b"]),
        json!(true),
        json!(false),
    ];
    let ctx = ctx_of(&json!({ "user": { "id": "u1", "role": "member" } }));
    let mut compiled_count = 0;
    let mut refused_for_null = 0;
    for expr in &exprs {
        let value = v(expr);
        // a required-nothing entity: ordering must be refused, everything else may compile
        let compiled = compile_predicate(&value, &ctx, PredicateRole::Filter, &|_| false);
        let predicate = match compiled {
            Ok(p) => p,
            Err(NotExpressible::NullSemantics(_)) => {
                refused_for_null += 1;
                continue;
            }
            Err(other) => panic!("{expr}: unexpectedly not expressible: {other}"),
        };
        compiled_count += 1;
        let mut interpreted: Vec<Value> = rows.iter().map(|r| (**r).clone()).collect();
        apply_read_access(&mut interpreted, None, Some(&value), &ctx, "Row");
        let query = RowQuery {
            filter: Some(predicate),
            ..RowQuery::default()
        };
        let page = run_query(&rows, &query).unwrap();
        assert_eq!(
            ids_of(&page.rows),
            ids_of(&interpreted),
            "{expr}: compiled and interpreted disagree"
        );
        assert_eq!(page.total, interpreted.len(), "{expr}");
    }
    assert!(compiled_count >= 15, "{compiled_count}");
    assert!(refused_for_null >= 4, "{refused_for_null}");
}

/// Control: where the field is required (never missing) an ordering comparison compiles and agrees.
#[test]
fn ordering_on_a_required_field_agrees_with_the_interpreter() {
    let rows = arcs(&json!([
        { "id": "a", "n": 1 }, { "id": "b", "n": 0 }, { "id": "c", "n": 5 }, { "id": "d", "n": -2 }
    ]));
    let ctx = BindingContext::new();
    for op in ["<", "<=", ">", ">="] {
        let value = v(&json!([op, ["object/get", "@entity", "n"], 1]));
        let predicate =
            compile_predicate(&value, &ctx, PredicateRole::Filter, &|f| f == "n").unwrap();
        let mut interpreted: Vec<Value> = rows.iter().map(|r| (**r).clone()).collect();
        apply_read_access(&mut interpreted, None, Some(&value), &ctx, "Row");
        let page = run_query(
            &rows,
            &RowQuery {
                filter: Some(predicate),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ids_of(&page.rows), ids_of(&interpreted), "{op}");
        assert!(!interpreted.is_empty());
    }
}

#[test]
fn capabilities_decide_the_pushdown_and_name_the_reason() {
    let full = QueryCapabilities::full();
    let f = fixture("query-cases.json");
    for case in f["cases"].as_array().unwrap() {
        let q: RowQuery = serde_json::from_value(case["query"].clone()).unwrap();
        assert_eq!(
            query_unsupported_reason(&full, &q),
            None,
            "{}",
            case["name"]
        );
    }
    let mut no_or = QueryCapabilities::full();
    no_or.combinators = vec![orbital_core_runtime::row_query::RowCombinator::All];
    let q = RowQuery {
        filter: Some(RowPredicate::Any { of: vec![] }),
        ..Default::default()
    };
    assert_eq!(
        query_unsupported_reason(&no_or, &q).unwrap().as_str(),
        "combinator-unsupported"
    );
    let mut no_window = QueryCapabilities::full();
    no_window.window = false;
    let q = RowQuery {
        limit: Some(2),
        ..Default::default()
    };
    assert_eq!(
        query_unsupported_reason(&no_window, &q).unwrap().as_str(),
        "window"
    );
    assert_eq!(
        query_unsupported_reason(&no_window, &RowQuery::default()),
        None
    );
}

fn expected_rows(expect: &Json) -> Vec<Value> {
    expect["rows"].as_array().unwrap().iter().map(v).collect()
}

fn rows_of(table: &RowTable) -> Vec<Value> {
    table.values().map(|r| (**r).clone()).collect()
}

/// A failed outcome carries a message the fixture does not pin: compare kinds for it, everything for the rest.
fn outcomes_match(got: &[RowWriteOutcome], want: &Json) -> bool {
    let want = want.as_array().unwrap();
    got.len() == want.len()
        && got.iter().zip(want).all(|(g, w)| match g {
            RowWriteOutcome::Failed { .. } => w["kind"] == "failed",
            other => norm(&serde_json::to_value(other).unwrap()) == norm(w),
        })
}

#[test]
fn every_write_case_matches_the_reference_semantics() {
    let f = fixture("write-cases.json");
    for case in f["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut t = table(&f["rows"]);
        let batch: RowWriteBatch = match case.get("batch") {
            Some(b) => serde_json::from_value(b.clone()).expect(name),
            None => RowWriteBatch {
                ops: vec![serde_json::from_value(case["op"].clone()).expect(name)],
                on_failure: RowWriteFailureMode::Stop,
            },
        };
        let got = apply_write_batch(&mut t, &batch, &mut CreateIds::Strict);
        assert!(
            outcomes_match(&got, &case["expect"]["outcomes"]),
            "{name}: {got:?}"
        );
        assert_eq!(rows_of(&t), expected_rows(&case["expect"]), "{name}: rows");
    }
}

/// The in-memory cache is one more implementation of the same contract. It differs in exactly one place: it mints
/// an id for an id-less create and merges over a taken id, as its `create` always did.
#[test]
fn the_cache_runs_every_write_case_the_same_way() {
    let f = fixture("write-cases.json");
    let mints = [
        "create-duplicate-id-fails",
        "create-without-id-fails",
        "atomic-rolls-back-on-failed-create",
    ];
    for case in f["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if mints.contains(&name) {
            continue;
        }
        let cache = InMemoryEntityCache::new();
        for r in arcs(&f["rows"]) {
            cache
                .write("Stock", r.as_object().unwrap()["id"].as_str().unwrap(), &r)
                .unwrap();
        }
        let batch: RowWriteBatch = match case.get("batch") {
            Some(b) => serde_json::from_value(b.clone()).unwrap(),
            None => RowWriteBatch {
                ops: vec![serde_json::from_value(case["op"].clone()).unwrap()],
                on_failure: RowWriteFailureMode::Stop,
            },
        };
        let got = cache.write_batch(&batch).unwrap().unwrap();
        assert!(
            outcomes_match(&got, &case["expect"]["outcomes"]),
            "{name}: {got:?}"
        );
        assert_eq!(
            cache
                .list("Stock")
                .unwrap()
                .iter()
                .map(|r| (**r).clone())
                .collect::<Vec<Value>>(),
            expected_rows(&case["expect"]),
            "{name}: rows"
        );
    }
}

#[test]
fn the_cache_mints_a_missing_id_and_refuses_a_taken_one_like_the_reference() {
    let cache = InMemoryEntityCache::new();
    cache
        .write(
            "Stock",
            "s1",
            &v(&json!({ "id": "s1", "title": "A", "stock": 1 })),
        )
        .unwrap();
    let created = cache
        .write_where(&RowWriteOp::Create {
            entity_type: "Stock".into(),
            data: v(&json!({ "title": "no id" })),
        })
        .unwrap()
        .unwrap();
    assert!(matches!(created, RowWriteOutcome::Created { .. }));
    let taken = cache
        .write_where(&RowWriteOp::Create {
            entity_type: "Stock".into(),
            data: v(&json!({ "id": "s1", "stock": 9 })),
        })
        .unwrap()
        .unwrap();
    assert_eq!(taken, RowWriteOutcome::DuplicateId);
    assert_eq!(
        norm(&serde_json::to_value(cache.read("Stock", "s1").unwrap().as_ref()).unwrap()),
        norm(&json!({ "id": "s1", "title": "A", "stock": 1 })),
        "the refused create left the row alone"
    );
    // control: the strict reference refuses the same create the same way
    let mut t = table(&json!([{ "id": "s1" }]));
    assert_eq!(
        apply_write(
            &mut t,
            &RowWriteOp::Create {
                entity_type: "Stock".into(),
                data: v(&json!({ "id": "s1" }))
            },
            &mut CreateIds::Strict
        ),
        RowWriteOutcome::DuplicateId
    );
}

#[test]
fn an_upsert_replaces_the_whole_row_and_keeps_the_stored_created_at() {
    let cache = InMemoryEntityCache::new();
    cache
        .write(
            "Stock",
            "s1",
            &v(&json!({ "id": "s1", "title": "A", "stock": 1, "createdAt": "2026-01-01T00:00:00.000Z" })),
        )
        .unwrap();
    let replaced = cache
        .write_where(&RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: v(&json!({ "id": "s1", "title": "B", "createdAt": "2030-01-01T00:00:00.000Z" })),
        })
        .unwrap()
        .unwrap();
    let RowWriteOutcome::Updated { row, .. } = replaced else {
        panic!("{replaced:?}")
    };
    assert_eq!(
        norm(&serde_json::to_value(&row).unwrap()),
        norm(&json!({ "id": "s1", "title": "B", "createdAt": "2026-01-01T00:00:00.000Z" }))
    );
    let fresh = cache
        .write_where(&RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: v(&json!({ "id": "s9", "title": "N" })),
        })
        .unwrap()
        .unwrap();
    assert!(matches!(fresh, RowWriteOutcome::Created { .. }), "control");
    let no_id = cache
        .write_where(&RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: v(&json!({ "title": "x" })),
        })
        .unwrap()
        .unwrap();
    assert!(matches!(no_id, RowWriteOutcome::Failed { .. }));
}

#[test]
fn every_id_and_resolve_case_matches() {
    let f = fixture("resolve-cases.json");
    let rows = arcs(&f["rows"]);
    for case in f["idCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let q: RowQuery = serde_json::from_value(case["query"].clone()).unwrap();
        let got = run_ids(&rows, &q).unwrap();
        assert_eq!(got.ids, expected_ids(&case["expect"]), "{name}");
        assert_eq!(
            got.total as u64,
            case["expect"]["total"].as_u64().unwrap(),
            "{name}"
        );
    }
    for case in f["resolveCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut t = table(&f["rows"]);
        for add in case
            .get("add")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let r = Arc::new(v(add));
            t.insert(
                r.as_object().unwrap()["id"].as_str().unwrap().to_string(),
                r,
            );
        }
        for set in case
            .get("set")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let id = set["id"].as_str().unwrap();
            let mut row = (**t.get(id).unwrap()).clone();
            for (k, val) in set["fields"].as_object().unwrap() {
                row.as_object_mut().unwrap().insert(k.clone(), v(val));
            }
            t.insert(id.to_string(), Arc::new(row));
        }
        let reference: RowSetRef = serde_json::from_value(case["ref"].clone()).expect(name);
        let policy: Option<RowPredicate> = case
            .get("policy")
            .map(|p| serde_json::from_value(p.clone()).unwrap());
        let got: RowPage = resolve_row_set(t.values(), &reference, policy.as_ref());
        assert_eq!(ids_of(&got.rows), expected_ids(&case["expect"]), "{name}");
        assert_eq!(
            got.total as u64,
            case["expect"]["total"].as_u64().unwrap(),
            "{name}"
        );
    }
}

// ----- the kernel's tiers and writes ----------------------------------------------------------

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "tiers",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "collection": "tickets",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" },
                           { "name": "status", "type": "string" }, { "name": "score", "type": "number", "required": true },
                           { "name": "owner", "type": "string" },
                           { "name": "embedding", "type": "array", "items": { "name": "component", "type": "number" } }],
                "read_policy": ["=", ["object/get", "@entity", "owner"], "u1"],
                "update_policy": [">", ["object/get", "@entity", "score"], 0] },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "LIST", "effects": [
                            ["fetch", "Ticket", { "filter": ["=", ["object/get", "@entity", "status"], "open"], "orderBy": "score:desc", "limit": 2,
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "LIST_UNORDERED", "effects": [
                            ["fetch", "Ticket", { "filter": ["=", ["object/get", "@entity", "status"], "open"], "limit": 2,
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "NEAREST", "effects": [
                            ["fetch", "Ticket", { "filter": ["=", ["object/get", "@entity", "status"], "open"],
                                "nearest": { "field": "embedding", "vector": [1, 0], "k": 2 },
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "NEAREST_IDS", "effects": [
                            ["fetch", "Ticket", { "filter": ["=", ["object/get", "@entity", "status"], "open"], "deliver": "ids",
                                "nearest": { "field": "embedding", "vector": [1, 0], "k": 2 },
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "NEAREST_WRONG_LENGTH", "effects": [
                            ["fetch", "Ticket", { "nearest": { "field": "embedding", "vector": [1, 0, 0], "k": 2 },
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "NEAREST_NO_K", "effects": [
                            ["fetch", "Ticket", { "nearest": { "field": "embedding", "vector": [1, 0], "k": 0 },
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "NEAREST_WITH_LIMIT", "effects": [
                            ["fetch", "Ticket", { "limit": 1, "nearest": { "field": "embedding", "vector": [1, 0], "k": 2 },
                                "emit": { "success": "LISTED" } }]] },
                        { "from": "idle", "to": "idle", "event": "BUMP", "effects": [
                            ["persist", "update", "Ticket", { "id": "t1", "score": 7 }],
                            ["persist", "update", "Ticket", { "id": "t2", "score": 8 }],
                            ["persist", "update", "Ticket", { "id": "nope", "score": 9 }],
                            ["persist", "update", "Ticket", { "id": "t3", "score": 6 }]] },
                        { "from": "idle", "to": "idle", "event": "BUMP_ONE", "effects": [
                            ["persist", "update", "Ticket", { "id": "t1", "score": 7 }]] },
                        { "from": "idle", "to": "idle", "event": "MAKE", "effects": [
                            ["persist", "update", "Ticket", { "id": "t1", "score": 7 }],
                            ["persist", "create", "Ticket", { "id": "t6", "title": "again", "score": 1, "owner": "u1" }]] },
                        { "from": "idle", "to": "idle", "event": "MAKE_TAKEN", "effects": [
                            ["persist", "create", "Ticket", { "id": "t2", "title": "again", "score": 1, "owner": "u1" }]] },
                        { "from": "idle", "to": "idle", "event": "SAVE_TWICE", "effects": [
                            ["persist", "upsert", "Ticket", { "id": "t2", "title": "first", "score": 1, "owner": "u1" }],
                            ["persist", "upsert", "Ticket", { "id": "t2", "title": "second", "score": 2, "owner": "u1" }]] },
                        { "from": "idle", "to": "idle", "event": "UPSERT_T1", "effects": [
                            ["persist", "upsert", "Ticket", { "id": "t1", "title": "replaced", "score": 5, "owner": "u1" }]] },
                        { "from": "idle", "to": "idle", "event": "BATCH_TAKEN", "effects": [
                            ["persist", "batch", [["create", "Ticket", { "id": "t2", "title": "again", "score": 1, "owner": "u1" }]]]] },
                        { "from": "idle", "to": "idle", "event": "BATCH_MISSING", "effects": [
                            ["persist", "batch", [["update", "Ticket", "nope", { "score": 9 }]]]] },
                        { "from": "idle", "to": "idle", "event": "BATCH_TAKEN_AFTER_OK", "effects": [
                            ["persist", "batch", [["update", "Ticket", "t1", { "score": 7 }],
                                ["create", "Ticket", { "id": "t2", "title": "again", "score": 1, "owner": "u1" }]]]] },
                        { "from": "idle", "to": "idle", "event": "MAKE_BATCH", "effects": [
                            ["persist", "batch", [["update", "Ticket", "t1", { "score": 7 }],
                                ["create", "Ticket", { "id": "t6", "title": "new", "score": 1, "owner": "u1" }]]]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn seeded() -> Arc<InMemoryEntityCache> {
    let cache = Arc::new(InMemoryEntityCache::new());
    for r in [
        json!({ "id": "t1", "title": "A", "status": "open", "score": 3, "owner": "u1" }),
        json!({ "id": "t2", "title": "B", "status": "open", "score": 9, "owner": "u1" }),
        json!({ "id": "t3", "title": "C", "status": "open", "score": 5, "owner": "u1" }),
        json!({ "id": "t4", "title": "D", "status": "closed", "score": 1, "owner": "u1" }),
        json!({ "id": "t5", "title": "E", "status": "open", "score": 8, "owner": "u2" }),
    ] {
        cache
            .write("Ticket", r["id"].as_str().unwrap(), &v(&r))
            .unwrap();
    }
    cache
}

fn seeded_vectors() -> Arc<InMemoryEntityCache> {
    let cache = Arc::new(InMemoryEntityCache::new());
    for r in [
        json!({ "id": "v1", "status": "open", "score": 1, "owner": "u1", "embedding": [1, 0] }),
        json!({ "id": "v2", "status": "open", "score": 1, "owner": "u1", "embedding": [0, 1] }),
        json!({ "id": "v3", "status": "open", "score": 1, "owner": "u1", "embedding": [1, 1] }),
        json!({ "id": "v4", "status": "closed", "score": 1, "owner": "u1", "embedding": [1, 0] }),
        json!({ "id": "v5", "status": "open", "score": 1, "owner": "u2", "embedding": [1, 0] }),
    ] {
        cache
            .write("Ticket", r["id"].as_str().unwrap(), &v(&r))
            .unwrap();
    }
    cache
}

fn failed_effects(out: &[(String, orbital_core_runtime::process_result::ProcessEventResult)]) -> usize {
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter(|er| er.is_failed())
        .count()
}

/// A kernel whose store is a host that answers through `Await`, answering from `host` (a real cache), with
/// `override_with` getting the first say; the requests asked are returned.
fn fly(
    kernel: &RuntimeKernel,
    event: &str,
    host: &InMemoryEntityCache,
    override_with: &dyn Fn(&IoRequest) -> Option<IoAnswer>,
) -> (Vec<IoRequest>, Result<DispatchOutcomes, String>) {
    let mut log = Vec::new();
    let mut progress = kernel
        .start_dispatch("Tickets", None, event, Value::Null, None, None)
        .unwrap();
    loop {
        match progress {
            Progress::Done(out) => return (log, Ok(out)),
            Progress::Await {
                continuation,
                requests,
            } => {
                let replies = requests
                    .iter()
                    .map(|p| {
                        log.push(p.request.clone());
                        let answer = override_with(&p.request).unwrap_or_else(|| {
                            BlockingHost {
                                source: host,
                                service: None,
                                substrate: None,
                            }
                            .answer(&p.request)
                        });
                        IoReply { id: p.id, answer }
                    })
                    .collect();
                progress = match kernel.resume(continuation, replies) {
                    Ok(p) => p,
                    Err(e) => return (log, Err(e.to_string())),
                };
            }
        }
    }
}

fn host_kernel() -> RuntimeKernel {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(HostAnswered),
        KernelConfig::default(),
    );
    k.register_schema(schema()).unwrap();
    k
}

fn listed(out: &[(String, orbital_core_runtime::process_result::ProcessEventResult)]) -> Vec<String> {
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref())
        .filter_map(|rv| rv.as_array())
        .flat_map(|rows| rows.iter().map(orbital_core_runtime::row_query::row_id))
        .collect()
}

#[test]
fn a_host_without_capabilities_is_listed_and_the_kernel_runs_the_query() {
    let k = host_kernel();
    let host = seeded();
    let (log, out) = fly(&k, "LIST", &host, &|_| None);
    assert!(
        data_requests(&log)
            .iter()
            .all(|r| matches!(r, IoRequest::ListRows { .. })),
        "{log:?}"
    );
    assert_eq!(listed(&out.unwrap()), vec!["t2", "t3"]);
}

#[test]
fn a_host_with_capabilities_gets_one_query_and_only_the_page_crosses() {
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let host = seeded();
    let (log, out) = fly(&k, "LIST", &host, &|r| match r {
        IoRequest::QueryRows {
            entity_type,
            query,
            project: RowProjection::Rows,
        } => Some(IoAnswer::Page(host_page(entity_type, query))),
        _ => None,
    });
    let log = data_requests(&log);
    assert_eq!(log.len(), 1, "{log:?}");
    let IoRequest::QueryRows { query, .. } = log[0] else {
        panic!("{log:?}")
    };
    assert_eq!(query.limit, Some(2));
    assert!(query.policy.is_some() && query.filter.is_some() && query.order.is_some());
    assert_eq!(listed(&out.unwrap()), vec!["t2", "t3"]);
}

/// EXEC-FETCH-1: an unordered fetch is pushed down like an ordered one, with no `order` on the query (the store
/// applies `createdAt` then `id`).
#[test]
fn an_unordered_fetch_is_pushed_down_without_an_order() {
    let answer = |r: &IoRequest| match r {
        IoRequest::QueryRows {
            entity_type,
            query,
            project: RowProjection::Rows,
        } => Some(IoAnswer::Page(host_page(entity_type, query))),
        _ => None,
    };
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let (log, out) = fly(&k, "LIST_UNORDERED", &seeded(), &answer);
    out.unwrap();
    let data = data_requests(&log);
    let queries: Vec<_> = data
        .iter()
        .filter_map(|r| match r {
            IoRequest::QueryRows { query, .. } => Some(query),
            _ => None,
        })
        .collect();
    assert_eq!(queries.len(), 1, "{log:?}");
    assert!(queries[0].order.is_none() && queries[0].limit == Some(2));
    // control: an ordered fetch carries its order
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let (log, out) = fly(&k, "LIST", &seeded(), &answer);
    out.unwrap();
    let ordered = data_requests(&log)
        .into_iter()
        .filter(|r| matches!(r, IoRequest::QueryRows { query, .. } if query.order.is_some()))
        .count();
    assert_eq!(ordered, 1, "{log:?}");
}

fn host_page(_entity: &str, query: &RowQuery) -> Result<RowPage, String> {
    let host = seeded();
    Ok(host.query_page("Ticket", query).unwrap_rows())
}

#[test]
fn a_host_that_declines_the_query_falls_back_to_a_listing() {
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let host = seeded();
    let (log, out) = fly(&k, "LIST", &host, &|r| match r {
        IoRequest::QueryRows { .. } => Some(IoAnswer::QueryUnsupported("no index".into())),
        _ => None,
    });
    let log = data_requests(&log);
    assert!(matches!(log[0], IoRequest::QueryRows { .. }), "{log:?}");
    assert!(matches!(log[1], IoRequest::ListRows { .. }), "{log:?}");
    assert_eq!(listed(&out.unwrap()), vec!["t2", "t3"]);
}

#[test]
fn a_query_outside_the_capabilities_is_never_sent() {
    let k = host_kernel();
    let mut caps = QueryCapabilities::full();
    caps.window = false;
    k.set_query_capabilities("Ticket", caps);
    let (log, out) = fly(&k, "LIST", &seeded(), &|_| None);
    assert!(
        data_requests(&log)
            .iter()
            .all(|r| matches!(r, IoRequest::ListRows { .. })),
        "{log:?}"
    );
    assert_eq!(listed(&out.unwrap()), vec!["t2", "t3"]);
}

#[test]
fn a_host_that_ignores_the_read_policy_is_caught() {
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let (_, out) = fly(&k, "LIST", &seeded(), &|r| match r {
        IoRequest::QueryRows { .. } => {
            let leaky = RowQuery {
                limit: Some(2),
                ..Default::default()
            };
            let page = seeded()
                .query_page(
                    "Ticket",
                    &RowQuery {
                        order: Some(orbital_core_base::row_set::RowOrder {
                            field: "score".into(),
                            direction: orbital_core_base::row_set::RowDirection::Desc,
                        }),
                        ..leaky
                    },
                )
                .unwrap_rows();
            Some(IoAnswer::Page(Ok(page)))
        }
        _ => None,
    });
    let results = out.unwrap();
    let failed = results
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed());
    assert!(failed, "t5 belongs to another owner and must not be served");
}

#[test]
fn a_run_of_persists_is_one_write_request_and_the_effects_run_as_one_at_a_time() {
    let k = host_kernel();
    let mut caps = WriteCapabilities::full();
    caps.conditional = true;
    k.set_write_capabilities("Ticket", caps);
    let host = seeded();
    let (log, out) = fly(&k, "BUMP", &host, &|_| None);
    let writes: Vec<&IoRequest> = log
        .iter()
        .filter(|r| matches!(r, IoRequest::WriteRows { .. }))
        .collect();
    // t1, t2, nope in one stop-mode request (it stops at `nope`); t3 follows as the remainder
    assert_eq!(writes.len(), 2, "{log:?}");
    let IoRequest::WriteRows { batch } = writes[0] else {
        unreachable!()
    };
    assert_eq!(batch.on_failure, RowWriteFailureMode::Stop);
    assert_eq!(batch.ops.len(), 4);
    assert!(
        !data_requests(&log)
            .iter()
            .any(|r| matches!(r, IoRequest::ReadRow { .. } | IoRequest::WriteRow { .. })),
        "{log:?}"
    );
    let results: Vec<_> = out
        .unwrap()
        .into_iter()
        .flat_map(|(_, r)| r.effect_results)
        .collect();
    let failed: Vec<bool> = results.iter().map(|r| r.is_failed()).collect();
    assert_eq!(
        failed,
        vec![false, false, true, false],
        "a missing row is a handled failure; later persists still run"
    );
    assert_eq!(
        host.read("Ticket", "t3").unwrap().as_object().unwrap()["score"],
        Value::Number(6.0)
    );
    assert_eq!(
        host.read("Ticket", "t1").unwrap().as_object().unwrap()["score"],
        Value::Number(7.0)
    );
}

#[test]
fn a_run_or_batch_holding_a_create_sends_it_in_write_rows() {
    for event in ["MAKE", "MAKE_BATCH"] {
        let k = host_kernel();
        k.set_write_capabilities("Ticket", WriteCapabilities::full());
        let (log, out) = fly(&k, event, &seeded(), &|_| None);
        let sent: Vec<&RowWriteBatch> = log
            .iter()
            .filter_map(|r| match r {
                IoRequest::WriteRows { batch } => Some(batch),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 1, "{event}: one WriteRows: {log:?}");
        assert!(
            matches!(sent[0].ops.as_slice(), [RowWriteOp::Update { .. }, RowWriteOp::Create { .. }]),
            "{event}: the update and the create travel together: {:?}",
            sent[0]
        );
        assert!(
            log.iter().all(|r| !matches!(r, IoRequest::CreateRow { .. })),
            "{event}: the store's own create is not asked: {log:?}"
        );
        assert!(!out
            .unwrap()
            .iter()
            .flat_map(|(_, r)| r.effect_results.iter())
            .any(|er| er.is_failed()));
    }
}

#[test]
fn a_create_on_a_taken_id_fails_duplicate_id_in_a_run() {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities::full());
    let (_, out) = fly(&k, "MAKE_TAKEN", &seeded(), &|_| None);
    let results: Vec<_> = out
        .unwrap()
        .into_iter()
        .flat_map(|(_, r)| r.effect_results)
        .collect();
    let failed: Vec<_> = results.iter().filter(|er| er.is_failed()).collect();
    assert_eq!(failed.len(), 1, "{results:?}");
    assert_eq!(
        failed[0].code,
        Some(orbital_core_base::bus_event::ServerEffectCode::DuplicateId)
    );
}

#[test]
fn an_upsert_in_a_run_is_pushed_and_replaces_a_taken_id() {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities::full());
    let host = seeded();
    let (log, out) = fly(&k, "SAVE_TWICE", &host, &|_| None);
    assert!(log.iter().any(|r| matches!(
        r,
        IoRequest::WriteRows { batch } if batch.ops.iter().all(|op| matches!(op, RowWriteOp::Upsert { .. }))
    )), "{log:?}");
    assert!(!out
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed()));
    let t2 = host.read("Ticket", "t2").unwrap();
    assert_eq!(t2.as_object().unwrap()["title"], Value::String("second".into()));
    assert!(t2.as_object().unwrap().get("status").is_none(), "replaced whole");
}

#[test]
fn an_upsert_over_a_row_the_update_policy_refuses_is_denied_and_changes_nothing() {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities::full());
    let denied_host = seeded();
    denied_host
        .write("Ticket", "t1", &v(&json!({ "score": 0 })))
        .unwrap();
    let (_, out) = fly(&k, "UPSERT_T1", &denied_host, &|_| None);
    let results: Vec<_> = out
        .unwrap()
        .into_iter()
        .flat_map(|(_, r)| r.effect_results)
        .collect();
    assert!(results.iter().any(|er| er.is_failed()), "{results:?}");
    assert_eq!(
        denied_host.read("Ticket", "t1").unwrap().as_object().unwrap()["title"],
        Value::String("A".into()),
        "the refused upsert left the row alone"
    );
    // control: a row the policy accepts is replaced
    let allowed_host = seeded();
    let (_, out) = fly(&k, "UPSERT_T1", &allowed_host, &|_| None);
    assert!(!out
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed()));
    assert_eq!(
        allowed_host.read("Ticket", "t1").unwrap().as_object().unwrap()["title"],
        Value::String("replaced".into())
    );
}

#[test]
fn a_host_without_write_capabilities_keeps_the_read_then_write_path() {
    let k = host_kernel();
    let (log, out) = fly(&k, "BUMP_ONE", &seeded(), &|_| None);
    assert!(
        log.iter()
            .all(|r| !matches!(r, IoRequest::WriteRows { .. })),
        "{log:?}"
    );
    assert!(log.iter().any(|r| matches!(r, IoRequest::ReadRow { .. })));
    assert!(!out
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed()));
}

#[test]
fn a_guarded_write_whose_guard_fails_on_the_pre_image_is_refused() {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities::full());
    let host = seeded();
    host.write("Ticket", "t1", &v(&json!({ "score": 0 })))
        .unwrap();
    let (log, out) = fly(&k, "BUMP_ONE", &host, &|_| None);
    let log = data_requests(&log);
    let IoRequest::WriteRows { batch } = log[0] else {
        panic!("{log:?}")
    };
    assert!(
        batch.ops[0].guard().is_some(),
        "the @update directive travels as the guard"
    );
    let results: Vec<_> = out
        .unwrap()
        .into_iter()
        .flat_map(|(_, r)| r.effect_results)
        .collect();
    assert!(results[0].is_failed());
    assert!(results[0].error.as_ref().unwrap().contains("denied"));
    assert_eq!(
        host.read("Ticket", "t1").unwrap().as_object().unwrap()["score"],
        Value::Number(0.0)
    );
}

#[test]
fn a_host_that_declines_the_write_falls_back_to_the_one_row_path() {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities::full());
    let (log, out) = fly(&k, "BUMP_ONE", &seeded(), &|r| match r {
        IoRequest::WriteRows { .. } => Some(IoAnswer::WriteUnsupported("no transactions".into())),
        _ => None,
    });
    let log = data_requests(&log);
    assert!(matches!(log[0], IoRequest::WriteRows { .. }), "{log:?}");
    assert!(
        log.iter().any(|r| matches!(r, IoRequest::ReadRow { .. })),
        "{log:?}"
    );
    assert!(
        log.iter().any(|r| matches!(r, IoRequest::WriteRow { .. })),
        "{log:?}"
    );
    assert!(!out
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed()));
}

fn committing_kernel() -> RuntimeKernel {
    let k = host_kernel();
    k.set_write_capabilities("Ticket", WriteCapabilities { commit_rows: Some(true), ..WriteCapabilities::full() });
    k
}

fn commits(log: &[&IoRequest]) -> usize {
    log.iter().filter(|r| matches!(r, IoRequest::CommitRows { .. })).count()
}

#[test]
fn a_host_that_commits_gets_the_kernels_rows_as_one_compare_and_commit() {
    let k = committing_kernel();
    let host = seeded();
    let (log, out) = fly(&k, "MAKE_BATCH", &host, &|_| None);
    let log = data_requests(&log);
    assert!(log.iter().all(|r| !matches!(r, IoRequest::WriteRows { .. })), "{log:?}");
    assert_eq!(commits(&log), 1, "{log:?}");
    let Some(IoRequest::CommitRows { rows }) = log.iter().find(|r| matches!(r, IoRequest::CommitRows { .. })) else { panic!("{log:?}") };
    let t1 = rows.iter().find(|r| r.id == "t1").unwrap();
    assert!(t1.expected.is_some() && matches!(t1.write, RowCommitWrite::Put { .. }));
    let t6 = rows.iter().find(|r| r.id == "t6").unwrap();
    assert!(t6.expected.is_none() && matches!(t6.write, RowCommitWrite::Put { .. }));
    assert_eq!(failed_effects(&out.unwrap()), 0);
    assert_eq!(host.read("Ticket", "t1").unwrap().as_object().unwrap()["score"], Value::Number(7.0));
    assert!(host.find("Ticket", "t6").unwrap().is_some());
}

#[test]
fn control_a_failing_atomic_batch_commits_nothing() {
    let k = committing_kernel();
    let host = seeded();
    let (log, out) = fly(&k, "BATCH_TAKEN_AFTER_OK", &host, &|_| None);
    let log = data_requests(&log);
    if let Some(IoRequest::CommitRows { rows }) = log.iter().find(|r| matches!(r, IoRequest::CommitRows { .. })) {
        assert!(rows.iter().all(|r| matches!(r.write, RowCommitWrite::Keep)), "{rows:?}");
    }
    assert_eq!(failed_effects(&out.unwrap()), 1);
    assert_eq!(host.read("Ticket", "t1").unwrap().as_object().unwrap()["score"], Value::Number(3.0));
}

#[test]
fn a_conflict_re_reads_and_re_runs_the_batch() {
    let k = committing_kernel();
    let host = seeded();
    let conflicts = std::cell::Cell::new(0);
    let (log, out) = fly(&k, "BUMP_ONE", &host, &|r| match r {
        IoRequest::CommitRows { .. } if conflicts.get() == 0 => {
            conflicts.set(1);
            Some(IoAnswer::Committed(Ok(false)))
        }
        _ => None,
    });
    let log = data_requests(&log);
    assert_eq!(commits(&log), 2, "{log:?}");
    assert!(log.iter().filter(|r| matches!(r, IoRequest::ReadRow { .. })).count() >= 2, "{log:?}");
    assert_eq!(failed_effects(&out.unwrap()), 0);
    assert_eq!(host.read("Ticket", "t1").unwrap().as_object().unwrap()["score"], Value::Number(7.0));
}

#[test]
fn the_wire_form_of_the_new_requests_and_answers_round_trips() {
    use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
    let batch = RowWriteBatch {
        ops: vec![RowWriteOp::Update {
            entity_type: "Ticket".into(),
            id: "t1".into(),
            data: v(&json!({ "score": 1 })),
            guard: Some(RowPredicate::Const { value: true }),
        }],
        on_failure: RowWriteFailureMode::Atomic,
    };
    let request = IoRequest::WriteRows { batch };
    let wire = IoRequestWire::from(&request);
    let text = serde_json::to_string(&wire).unwrap();
    assert!(
        text.contains("\"kind\":\"writeRows\"") && text.contains("\"onFailure\":\"atomic\""),
        "{text}"
    );
    assert_eq!(
        IoRequest::from(serde_json::from_str::<IoRequestWire>(&text).unwrap()),
        request
    );
    let answer = IoAnswer::Wrote(Ok(vec![
        RowWriteOutcome::NotFound,
        RowWriteOutcome::RolledBack,
    ]));
    let text = serde_json::to_string(&IoAnswerWire::from(&answer)).unwrap();
    assert_eq!(
        IoAnswer::from(serde_json::from_str::<IoAnswerWire>(&text).unwrap()),
        answer
    );
}

/// A store with no conditional writes: the kernel's per-op fallback.
struct Plain(InMemoryEntityCache);

impl EntitySource for Plain {
    fn read(&self, t: &str, id: &str) -> Result<Arc<Value>, String> {
        self.0.read(t, id)
    }
    fn write(&self, t: &str, id: &str, d: &Value) -> Result<Value, String> {
        self.0.write(t, id, d)
    }
    fn create(&self, t: &str, d: &Value) -> Result<Value, String> {
        self.0.create(t, d)
    }
    fn delete(&self, t: &str, id: &str) -> Result<(), String> {
        self.0.delete(t, id)
    }
    fn clear(&self) {
        self.0.clear()
    }
    fn count(&self) -> usize {
        self.0.count()
    }
    fn list(&self, t: &str) -> Result<Vec<std::sync::Arc<Value>>, String> {
        self.0.list(t)
    }
}

#[test]
fn a_batch_on_a_store_without_atomic_writes_restores_what_it_applied() {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "comp",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "collection": "tickets",
                "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number" }] },
            "traits": [{ "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [
                        ["persist", "batch", [
                            ["update", "Ticket", "t1", { "score": 7, "extra": true }],
                            ["delete", "Ticket", "t2"],
                            ["create", "Ticket", { "id": "t9", "score": 1 }],
                            ["update", "Ticket", "nope", { "score": 1 }]
                        ]]] }] } }]
        }]
    }))
    .unwrap();
    let store = Arc::new(Plain(InMemoryEntityCache::new()));
    store
        .write("Ticket", "t1", &v(&json!({ "id": "t1", "score": 3 })))
        .unwrap();
    store
        .write("Ticket", "t2", &v(&json!({ "id": "t2", "score": 4 })))
        .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        store.clone(),
        KernelConfig::default(),
    );
    k.register_schema(schema).unwrap();
    let out = k.dispatch("Tickets", None, "GO", Value::Null).unwrap();
    assert!(out
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .any(|er| er.is_failed()));
    let rows: Vec<Json> = store
        .list("Ticket")
        .unwrap()
        .iter()
        .map(|r| norm(&serde_json::to_value(&**r).unwrap()))
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["score"], json!(3.0));
    assert!(
        rows[0]["extra"].is_null(),
        "the field the update added is gone: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r["id"] == "t2"),
        "the delete was undone: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r["id"] == "t9"),
        "the create was undone: {rows:?}"
    );
}

// ----- EXEC-FETCH-1: the default order ---------------------------------------------------------

fn run_ids_of(rows: Json, query: Json) -> Vec<String> {
    let rows = arcs(&rows);
    let query: RowQuery = serde_json::from_value(query).unwrap();
    run_ids(rows.iter(), &query).unwrap().ids
}

#[test]
fn an_unordered_query_is_created_at_then_id_with_a_missing_stamp_last() {
    let rows = json!([
        { "id": "a", "createdAt": "2026-03-02T00:00:00.000Z" },
        { "id": "b" },
        { "id": "c", "createdAt": "2026-03-01T00:00:00.000Z" },
        { "id": "d", "createdAt": "2026-03-02T00:00:00.000Z" },
        { "id": "e", "createdAt": null }
    ]);
    assert_eq!(
        run_ids_of(rows.clone(), json!({})),
        ["c", "a", "d", "b", "e"]
    );
    assert_eq!(
        run_ids_of(rows.clone(), json!({ "offset": 1, "limit": 2, "total": "none" })),
        ["a", "d"]
    );
    // control: an authored order replaces the default
    assert_eq!(
        run_ids_of(rows, json!({ "order": { "field": "id", "direction": "desc" } })),
        ["e", "d", "c", "b", "a"]
    );
}

#[test]
fn a_created_at_of_mixed_kinds_has_one_total_order_number_then_string() {
    let rows = json!([
        { "id": "a", "createdAt": "2026-03-01T00:00:00.000Z" },
        { "id": "b", "createdAt": 5 },
        { "id": "c", "createdAt": 3 }
    ]);
    assert_eq!(run_ids_of(rows, json!({})), ["c", "b", "a"]);
}

#[test]
fn a_store_that_cannot_order_by_created_at_declines_an_unordered_query() {
    use orbital_core_runtime::row_query::RowOrderableFields;
    let mut caps = QueryCapabilities::full();
    caps.orderable_fields = RowOrderableFields::Listed {
        fields: vec!["id".to_string()],
    };
    assert!(query_unsupported_reason(&caps, &RowQuery::default()).is_some());
    caps.orderable_fields = RowOrderableFields::Listed {
        fields: vec!["id".to_string(), "createdAt".to_string()],
    };
    assert!(query_unsupported_reason(&caps, &RowQuery::default()).is_none());
}

// ----- nearest (EXEC-FETCH-2) -------------------------------------------------------------------

fn nearest_case_reasons(caps: &QueryCapabilities) -> Vec<(String, Option<String>)> {
    let f = fixture("query-cases.json");
    f["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let q: RowQuery = serde_json::from_value(case["query"].clone()).unwrap();
            (
                case["name"].as_str().unwrap().to_string(),
                query_unsupported_reason(caps, &q).map(|r| r.as_str().to_string()),
            )
        })
        .collect()
}

#[test]
fn nearest_error_cases_fail_with_their_reason_and_the_paired_valid_cases_do_not() {
    let f = fixture("query-cases.json");
    let rows = arcs(&f["rows"]);
    let errors = f["errorCases"].as_array().unwrap();
    assert!(!errors.is_empty());
    for case in errors {
        let name = case["name"].as_str().unwrap();
        let query: RowQuery = serde_json::from_value(case["query"].clone()).expect(name);
        let want = case["expect"]["error"].as_str().unwrap();
        let got = run_query(&rows, &query).unwrap_err();
        assert!(got.contains(want), "{name}: {got}");
        assert!(run_ids(&rows, &query).unwrap_err().contains(want), "{name}: ids");
        let cache = InMemoryEntityCache::new();
        for r in &rows {
            cache.write("Ticket", r.as_object().unwrap()["id"].as_str().unwrap(), r).unwrap();
        }
        assert!(cache.query_page("Ticket", &query).unwrap_failure().contains(want), "{name}: source");
        // control: the same filter with a well-formed vector answers
        let mut valid = query.clone();
        if let Some(n) = valid.nearest.as_mut() {
            n.vector = vec![1.0, 0.0, 0.0];
        }
        assert!(run_query(&rows, &valid).is_ok(), "{name}: control");
    }
}

#[test]
fn a_store_without_the_nearest_capability_is_never_asked_and_one_with_it_is() {
    let mut without = QueryCapabilities::full();
    without.nearest = None;
    for (name, reason) in nearest_case_reasons(&without) {
        let expected = name.starts_with("nearest-").then(|| "nearest".to_string());
        assert_eq!(reason, expected, "{name}");
    }
    for (name, reason) in nearest_case_reasons(&QueryCapabilities::full()) {
        assert_eq!(reason, None, "{name}");
    }
}

#[test]
fn the_kernel_computes_nearest_over_a_listing_when_the_store_lacks_the_capability() {
    let k = host_kernel();
    let (log, out) = fly(&k, "NEAREST", &seeded_vectors(), &|_| None);
    assert!(
        data_requests(&log)
            .iter()
            .all(|r| matches!(r, IoRequest::ListRows { .. })),
        "{log:?}"
    );
    assert_eq!(listed(&out.unwrap()), vec!["v1", "v3"]);
    // control: a store declaring every query form but nearest is listed too, with the same answer
    let k = host_kernel();
    let mut caps = QueryCapabilities::full();
    caps.nearest = None;
    k.set_query_capabilities("Ticket", caps);
    let (log, out) = fly(&k, "NEAREST", &seeded_vectors(), &|_| None);
    assert!(
        data_requests(&log)
            .iter()
            .all(|r| matches!(r, IoRequest::ListRows { .. })),
        "{log:?}"
    );
    assert_eq!(listed(&out.unwrap()), vec!["v1", "v3"]);
}

#[test]
fn a_store_with_the_nearest_capability_gets_the_query_and_only_the_page_crosses() {
    let k = host_kernel();
    k.set_query_capabilities("Ticket", QueryCapabilities::full());
    let (log, out) = fly(&k, "NEAREST", &seeded_vectors(), &|r| match r {
        IoRequest::QueryRows {
            query,
            project: RowProjection::Rows,
            ..
        } => Some(IoAnswer::Page(Ok(seeded_vectors()
            .query_page("Ticket", query)
            .unwrap_rows()))),
        _ => None,
    });
    let data = data_requests(&log);
    assert_eq!(data.len(), 1, "{log:?}");
    let IoRequest::QueryRows { query, .. } = data[0] else {
        panic!("{log:?}")
    };
    let nearest = query.nearest.as_ref().expect("nearest travels with the query");
    assert_eq!((nearest.field.as_str(), nearest.k), ("embedding", 2));
    assert!(query.order.is_none() && query.limit.is_none() && query.offset.is_none());
    assert_eq!(listed(&out.unwrap()), vec!["v1", "v3"]);
}

#[test]
fn a_nearest_fetch_that_cannot_run_fails_the_effect() {
    for event in ["NEAREST_WRONG_LENGTH", "NEAREST_NO_K", "NEAREST_WITH_LIMIT"] {
        let k = host_kernel();
        let (_, out) = fly(&k, event, &seeded_vectors(), &|_| None);
        assert_eq!(failed_effects(&out.unwrap()), 1, "{event}");
    }
    // control: the well-formed fetch does not fail
    let k = host_kernel();
    let (_, out) = fly(&k, "NEAREST", &seeded_vectors(), &|_| None);
    assert_eq!(failed_effects(&out.unwrap()), 0);
}

#[test]
fn a_nearest_fetch_delivered_by_reference_carries_the_query_and_counts_what_it_returned() {
    let k = host_kernel();
    let (_, out) = fly(&k, "NEAREST_IDS", &seeded_vectors(), &|_| None);
    let out = out.unwrap();
    let references: Vec<_> = out
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref())
        .flat_map(orbital_core_runtime::row_query::row_sets_in)
        .collect();
    assert_eq!(references.len(), 1, "{references:?}");
    assert_eq!(references[0].ids, vec!["v1", "v3"]);
    assert_eq!(references[0].total, 2);
    assert_eq!(references[0].query.nearest.as_ref().map(|n| n.k), Some(2));
    // control: the same fetch delivering rows returns the same two rows
    let k = host_kernel();
    let (_, out) = fly(&k, "NEAREST", &seeded_vectors(), &|_| None);
    assert_eq!(listed(&out.unwrap()), vec!["v1", "v3"]);
}

#[test]
fn resolving_a_nearest_reference_totals_the_rows_still_resolving_and_a_plain_one_recounts() {
    let rows = arcs(&json!([
        { "id": "a", "status": "open", "embedding": [1, 0] },
        { "id": "b", "status": "open", "embedding": [0, 1] },
        { "id": "c", "status": "open", "embedding": [1, 1] }
    ]));
    let reference = |nearest: Json| -> RowSetRef {
        serde_json::from_value(json!({
            "entityType": "T", "ids": ["a", "c"], "total": 2,
            "query": { "filter": { "kind": "compare", "field": "status", "op": "==", "value": "open" }, "nearest": nearest }
        }))
        .unwrap()
    };
    let near = reference(json!({ "field": "embedding", "vector": [1, 0], "k": 2 }));
    let got = resolve_row_set(rows.iter(), &near, None);
    assert_eq!((ids_of(&got.rows), got.total), (vec!["a".to_string(), "c".to_string()], 2));
    // control: without nearest the total re-counts every match of the filter
    let mut plain = near.clone();
    plain.query.nearest = None;
    let got = resolve_row_set(rows.iter(), &plain, None);
    assert_eq!((ids_of(&got.rows), got.total), (vec!["a".to_string(), "c".to_string()], 3));
}

fn batch_results(k: &RuntimeKernel, event: &str, host: &InMemoryEntityCache) -> Vec<orbital_core_base::effect_types::EffectResult> {
    fly(k, event, host, &|_| None)
        .1
        .unwrap()
        .into_iter()
        .flat_map(|(_, r)| r.effect_results)
        .collect()
}

/// EXEC-STEP-8 and the batch rules: a create of a taken id fails `duplicate-id`, an update of a missing row fails
/// `not-found`, with the failing index and how many ran before it; the same on a store that runs the batch pushed
/// down and on one that does not.
#[test]
fn a_batch_op_that_cannot_apply_fails_the_batch_with_its_code_index_and_count() {
    for pushed in [false, true] {
        for (event, code, index, completed) in [
            ("BATCH_TAKEN", orbital_core_base::bus_event::ServerEffectCode::DuplicateId, 0, 0),
            ("BATCH_MISSING", orbital_core_base::bus_event::ServerEffectCode::NotFound, 0, 0),
            ("BATCH_TAKEN_AFTER_OK", orbital_core_base::bus_event::ServerEffectCode::DuplicateId, 1, 1),
        ] {
            let k = host_kernel();
            if pushed {
                k.set_write_capabilities("Ticket", WriteCapabilities::full());
            }
            let host = seeded();
            let results = batch_results(&k, event, &host);
            let failed: Vec<_> = results.iter().filter(|er| er.is_failed()).collect();
            assert_eq!(failed.len(), 1, "{event} pushed={pushed}: {results:?}");
            assert_eq!(failed[0].code, Some(code), "{event} pushed={pushed}");
            let at = failed[0].batch_failure.as_ref().expect("a batch failure names its operation");
            assert_eq!((at.index, at.completed_count), (index, completed), "{event} pushed={pushed}");
            assert!(host.read("Ticket", "nope").is_err(), "{event}: nothing was created");
        }
        // control: a batch of ops that all apply succeeds
        let k = host_kernel();
        if pushed {
            k.set_write_capabilities("Ticket", WriteCapabilities::full());
        }
        let results = batch_results(&k, "MAKE_BATCH", &seeded());
        assert!(!results.iter().any(|er| er.is_failed()), "pushed={pushed}: {results:?}");
    }
}
