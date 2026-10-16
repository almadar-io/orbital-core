//! Pins the allocation count of a fixed dispatch (Finder.SEARCH shape: a 16-effect transition,
//! 13 `set`, 2 `when`, 1 `render-ui`, over a 14-field entity). Speed pass 2, copies C1-C8: the
//! pinned numbers only ever go down.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[global_allocator]
static A: Counting = Counting;

fn schema() -> OrbitalSchema {
    let fields: Vec<_> = (0..14)
        .map(|i| match i {
            0 => json!({ "name": "id", "type": "string" }),
            1 => json!({ "name": "tags", "type": "array", "default": ["a", "b", "c"] }),
            2 => json!({ "name": "modules", "type": "array", "default": ["x", "y"] }),
            _ => json!({ "name": format!("f{i}"), "type": "string", "default": "d" }),
        })
        .collect();
    let mut effects: Vec<serde_json::Value> = (3..14)
        .map(|i| {
            json!([
                "set",
                format!("@entity.f{i}"),
                ["str/concat", format!("@entity.f{i}"), "@payload.q"]
            ])
        })
        .collect();
    effects.push(json!([
        "set",
        "@entity.tags",
        ["array/append", "@entity.tags", "@payload.q"]
    ]));
    effects.push(json!([
        "set",
        "@entity.modules",
        ["array/append", "@entity.modules", "@payload.q"]
    ]));
    effects.push(json!([
        "when",
        ["=", "@entity.f3", "never"],
        ["set", "@entity.f4", "x"]
    ]));
    effects.push(json!([
        "when",
        ["=", "@entity.f5", "never"],
        ["set", "@entity.f6", "x"]
    ]));
    effects.push(json!(["render-ui", "main", { "type": "typography", "content": "@entity.f3" }]));
    serde_json::from_value(json!({
        "name": "alloc",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "SearchQuery", "persistence": "runtime", "fields": fields },
            "traits": [{
                "name": "Finder", "linkedEntity": "SearchQuery", "local": true,
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [] },
                        { "from": "idle", "to": "idle", "event": "SEARCH",
                          "guard": ["not", ["=", "@payload.q", ""]], "effects": effects }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn count<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = ALLOCS.load(Ordering::SeqCst);
    let r = f();
    (ALLOCS.load(Ordering::SeqCst) - before, r)
}

#[test]
fn search_dispatch_allocation_count_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        cache as Arc<dyn EntitySource>,
        KernelConfig::for_environment(RuntimeEnvironment::Client),
    );
    k.register_schema(schema()).unwrap();
    k.dispatch_with_server_leg("Main", "Finder", None, "INIT", Value::Null, None)
        .unwrap();
    let payload = || serde_json::from_value::<Value>(json!({ "q": "kernel" })).unwrap();
    for _ in 0..3 {
        k.dispatch_with_server_leg("Main", "Finder", None, "SEARCH", payload(), None)
            .unwrap();
    }
    let p = payload();
    let (n, r) = count(|| {
        k.dispatch_with_server_leg("Main", "Finder", None, "SEARCH", p, None)
            .unwrap()
    });
    assert!(r.response.success, "{:?}", r.response.error);
    println!("ALLOC_SEARCH {n}");
    assert!(
        n <= PINNED_SEARCH,
        "allocations rose: {n} > {PINNED_SEARCH}"
    );
}

const PINNED_SEARCH: usize = 1791;

fn fetch_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "alloc-fetch",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "collection": "tickets",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" },
                           { "name": "status", "type": "string" }, { "name": "owner", "type": "string" }],
                "read_policy": ["=", ["object/get", "@entity", "owner"], "u1"] },
            "traits": [{
                "name": "Tickets", "linkedEntity": "Ticket",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "LIST", "effects": [
                        ["fetch", "Ticket", { "filter": ["=", ["object/get", "@entity", "status"], "open"],
                            "emit": { "success": "LISTED" } }]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

#[test]
fn fetch_dispatch_allocation_count_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(fetch_schema()).unwrap();
    // Seed after registering: the cache keys its store by the bound collection.
    for i in 0..50 {
        let row = serde_json::from_value::<Value>(json!({
            "id": format!("t{i}"), "title": format!("T{i}"),
            "status": if i % 2 == 0 { "open" } else { "closed" }, "owner": "u1"
        }))
        .unwrap();
        cache.write("Ticket", &format!("t{i}"), &row).unwrap();
    }
    k.dispatch("Tickets", None, "LIST", Value::Null).unwrap();
    let (n, out) = count(|| k.dispatch("Tickets", None, "LIST", Value::Null).unwrap());
    let rows: usize = out
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref()?.as_array().map(|a| a.len()))
        .sum();
    assert_eq!(rows, 25);
    println!("ALLOC_FETCH {n}");
    assert!(n <= PINNED_FETCH, "allocations rose: {n} > {PINNED_FETCH}");
}

const PINNED_FETCH: usize = 4264;

#[test]
fn read_access_allocation_count_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let policy =
        serde_json::from_value::<Value>(json!(["=", ["object/get", "@entity", "owner"], "u1"]))
            .unwrap();
    let filter =
        serde_json::from_value::<Value>(json!(["=", ["object/get", "@entity", "status"], "open"]))
            .unwrap();
    let mut rows: Vec<Value> = (0..50)
        .map(|i| {
            serde_json::from_value::<Value>(json!({
                "id": format!("t{i}"), "title": format!("T{i}"),
                "status": if i % 2 == 0 { "open" } else { "closed" }, "owner": "u1"
            }))
            .unwrap()
        })
        .collect();
    let ctx = orbital_core_eval::evaluator::BindingContext::new();
    let mut warm = rows.clone();
    orbital_core_runtime::entity_access::apply_read_access(
        &mut warm,
        Some(&policy),
        Some(&filter),
        &ctx,
        "Ticket",
    );
    let (n, ()) = count(|| {
        orbital_core_runtime::entity_access::apply_read_access(
            &mut rows,
            Some(&policy),
            Some(&filter),
            &ctx,
            "Ticket",
        )
    });
    assert_eq!(rows.len(), 25);
    println!("ALLOC_READ_ACCESS {n}");
    assert!(
        n <= PINNED_READ_ACCESS,
        "allocations rose: {n} > {PINNED_READ_ACCESS}"
    );
}

const PINNED_READ_ACCESS: usize = 2769;

const REPLAY_ROWS: usize = 1000;
const REPLAY_WRITES: usize = 6;

fn replay_schema() -> OrbitalSchema {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let mut effects = vec![json!(["fetch", "Item", { "emit": { "success": "LOADED" } }])];
    for i in 0..REPLAY_WRITES {
        effects.push(
            json!(["persist", "create", "Note", { "id": format!("n{i}"), "title": "t" }, emit]),
        );
    }
    serde_json::from_value(json!({
        "name": "alloc-replay",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "persistent", "collection": "items",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Browse", "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": effects }]
                }
            }]
        }, {
            "name": "Notes",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": []
        }]
    }))
    .unwrap()
}

#[test]
fn resumable_replay_of_a_list_answer_allocation_count_is_pinned() {
    use orbital_core_runtime::resumable::{drive_blocking, DispatchOutcomes, HostAnswered, IoAnswer, IoRequest, Progress};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(HostAnswered) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(replay_schema()).unwrap();
    let listed = std::cell::RefCell::new(Some(rows_answer(REPLAY_ROWS)));
    let asked_lists = std::cell::Cell::new(0usize);
    let answer = |request: &IoRequest| match request {
        IoRequest::QueryRows { .. } => IoAnswer::QueryUnsupported("no push-down".to_string()),
        IoRequest::ListRows { .. } => {
            asked_lists.set(asked_lists.get() + 1);
            listed
                .borrow_mut()
                .take()
                .expect("the list is asked for once")
        }
        IoRequest::CreateRow { data, .. } => IoAnswer::Row(Ok(data.clone())),
        IoRequest::ReadRow { .. } => IoAnswer::Row(Ok(Value::Null)),
        other => panic!("unexpected {other:?}"),
    };
    let (n, outcomes): (usize, DispatchOutcomes) = count(|| {
        let progress: Progress<DispatchOutcomes> = k
            .start_dispatch("Browse", None, "GO", Value::Null, None, None)
            .unwrap();
        drive_blocking(&k, progress, &answer).unwrap()
    });
    assert_eq!(asked_lists.get(), 1);
    assert!(!outcomes.is_empty());
    println!("ALLOC_REPLAY {n}");
    assert!(
        n <= PINNED_REPLAY,
        "allocations rose: {n} > {PINNED_REPLAY}"
    );
}

const PINNED_REPLAY: usize = 469255;

fn rows_answer(n: usize) -> orbital_core_runtime::resumable::IoAnswer {
    let rows: Vec<Arc<Value>> = (0..n)
        .map(|i| {
            Arc::new(
                serde_json::from_value(json!({ "id": format!("i{i}"), "title": format!("T{i}") }))
                    .unwrap(),
            )
        })
        .collect();
    orbital_core_runtime::resumable::IoAnswer::Rows(Ok(rows))
}
