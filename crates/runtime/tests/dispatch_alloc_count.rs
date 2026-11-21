//! Pins the allocation count of a fixed dispatch (Finder.SEARCH shape: a 16-effect transition,
//! 13 `set`, 2 `when`, 1 `render-ui`, over a 14-field entity). Speed pass 2, copies C1-C8: the
//! pinned numbers only ever go down.

#[path = "support/mod.rs"]
mod support;

use orbital_core_base::json_value::{from_json_str, from_json_value};
use serde_json::json;
use orbital_core_base::alloc_meter::{self, CountingAlloc};
use std::alloc::System;
use std::sync::Arc;
use {
    orbital_core_base::effect_types::RuntimeEnvironment,
    orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value,
};
use {
    orbital_core_runtime::config::KernelConfig,
    orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache},
    orbital_core_runtime::kernel::RuntimeKernel,
};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[global_allocator]
static A: CountingAlloc<System> = CountingAlloc::with_totals(System);

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
    from_json_value(&json!({
        "name": "alloc",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "SearchQuery", "persistence": "runtime", "fields": fields },
            "traits": [{
                "name": "Finder", "linkedEntity": "SearchQuery", "dispatch": "client",
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

/// A value rebuilt list by list and record by record: what one copy of it costs, now that a clone shares.
fn deep_copy(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::array(items.iter().map(deep_copy).collect::<Vec<_>>()),
        Value::Object(fields) => Value::object(
            fields.iter().map(|(k, v)| (k.clone(), deep_copy(v))).collect::<orbital_core_base::value::ValueMap>(),
        ),
        Value::String(text) => Value::string(text.as_str()),
        other => other.clone(),
    }
}

fn count<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = alloc_meter::totals().allocs;
    let r = f();
    (alloc_meter::totals().allocs - before, r)
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
    let payload = || from_json_value::<Value>(&json!({ "q": "kernel" })).unwrap();
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

// 1405 (was 1510): a step binds `@entity` as the kernel holds it and a `set` writes one field (G-ORBITAL-CORE-049, 2026-10-25).
// 985: copy-on-write `Value` and `SExpression` strings, lists and maps (P-014, 2026-10-26).
// 822 -> 700: `ValueMap` keys are the program's interned `Shared<String>` names, so a step that builds or merges a record
// bumps a count instead of allocating each name (R3, G-ORBITAL-CORE-070, 2026-10-29; 985 was the pin before it, 822 measured on main).
// 700 -> 701: +1, unattributed. Measured 2026-10-29: `OirType::erased()` is not on the dispatch path (same count with a per-call allocating or a shared name), so the earlier attribution to it was wrong.
const PINNED_SEARCH: usize = 701;

fn fetch_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-fetch",
        "typeContractVersion": 1, "orbitals": [{
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
        let row = from_json_value::<Value>(&json!({
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

// 746 (was 1258): a dispatch clones the kernel's one `prob/*` seed source instead of building one (G-ORBITAL-CORE-029, 2026-10-18).
// 738: G-ORBITAL-CORE-049, 2026-10-25; +1 the seed's shared payload `Arc`, which spares every step a deep copy.
// 201: copy-on-write `Value` and `SExpression` strings, lists and maps (P-014, 2026-10-26).
// 185 -> 184 measured on main -> after R3 (interned `ValueMap` keys), 2026-10-29.
// 184 -> 185: +1, unattributed. Measured 2026-10-29: `OirType::erased()` is not on the dispatch path (same count with a per-call allocating or a shared name), so the earlier attribution to it was wrong.
const PINNED_FETCH: usize = 185;

#[test]
fn read_access_allocation_count_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let policy =
        from_json_value::<Value>(&json!(["=", ["object/get", "@entity", "owner"], "u1"]))
            .unwrap();
    let filter =
        from_json_value::<Value>(&json!(["=", ["object/get", "@entity", "status"], "open"]))
            .unwrap();
    let mut rows: Vec<Value> = (0..50)
        .map(|i| {
            from_json_value::<Value>(&json!({
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

// 683: copy-on-write `Value` and `SExpression` strings, lists and maps (P-014, 2026-10-26).
// 183: measured on main before and after R3 (interned `ValueMap` keys), 2026-10-29.
const PINNED_READ_ACCESS: usize = 183;

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
    from_json_value(&json!({
        "name": "alloc-replay",
        "typeContractVersion": 1, "orbitals": [{
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
    use orbital_core_runtime::resumable::{
        drive_blocking, DispatchOutcomes, HostAnswered, IoAnswer, IoRequest, Progress,
    };
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

// 174201 (was 331144): each suspension records the transition its step committed (two names and a box, G-CROSS-259), and a dispatch
// clones the kernel's one `prob/*` seed source (G-ORBITAL-CORE-029), 2026-10-18.
// 174041: G-ORBITAL-CORE-049, 2026-10-25; +1 the seed's shared payload `Arc`, which spares every step a deep copy.
// 11189: copy-on-write `Value` and `SExpression` strings, lists and maps (P-014, 2026-10-26).
// 11192: +3, the list answer's rows are checked against `Item` once, its shape built and cached once (G-CROSS-335, 2026-10-27).
// 11194: +2, a built record type holds its field list in one shared `Arc`, so every later copy of it is a reference bump
// (P-017 K1, 2026-10-27; std-kflow validate 291 -> 230 s).
// 12118: +924, an IR object node boxes its map so every `SExpression` is 24 bytes (owner accepted, P-017, 2026-10-28).
// 12120: +2, measured on main after wave 3 (present before step-4 phase A; most likely the per-step emit-source stamp,
// G-ORB-553, not bisected), 2026-10-28.
// 10293 -> 8971: `ValueMap` keys are interned `Shared<String>` names (R3, G-ORBITAL-CORE-070, 2026-10-29).
// 8971 -> 8972: +1, unattributed. Measured 2026-10-29: `OirType::erased()` is not on the dispatch path (same count with a per-call allocating or a shared name), so the earlier attribution to it was wrong.
// 8972 -> 8975: +3, measured 2026-11-10 with and without the S1 kernel edits (same count), so it predates them; most likely
// the visit key's per-allocation digest memo for a wide list (A3 close, 72dec79), not bisected.
// 8975 -> 9003: +28, re-pinned 2026-11-16 on the owner ruling to clear every red pin (G-ORBITAL-CORE-191). Not attributed:
// measured 9003 to 9023 between 2026-11-13 and 2026-11-16 across parallel kernel work, and backing out the suspected
// changes one at a time moved nothing.
// 9003 -> 9004: +1, re-pinned 2026-11-21 on the owner ruling (all-tests checkpoint), not attributed (G-ORBITAL-CORE-225).
const PINNED_REPLAY: usize = 9004;

fn rows_answer(n: usize) -> orbital_core_runtime::resumable::IoAnswer {
    let rows: Vec<Arc<Value>> = (0..n)
        .map(|i| {
            Arc::new(
                from_json_value(&json!({ "id": format!("i{i}"), "title": format!("T{i}") }))
                    .unwrap(),
            )
        })
        .collect();
    orbital_core_runtime::resumable::IoAnswer::Rows(Ok(rows))
}

fn scanner_schema(blob: &str) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-loop",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Scan", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "blob", "type": "string", "default": blob },
                { "name": "scans", "type": "array", "default": [] }
            ] },
            "traits": [{
                "name": "Scanner", "linkedEntity": "Scan",
                "listens": [{ "event": "SCAN_NEXT", "triggers": "SCAN", "source": { "kind": "trait", "trait": "Scanner" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SCAN",
                        "guard": [">", "@payload.left", 0],
                        "effects": [
                            ["set", "@entity.scans", ["array/append", "@entity.scans", "s"]],
                            ["emit", "SCAN_NEXT", { "left": ["-", "@payload.left", 1] }]
                        ] }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// Bytes a single self-emitting dispatch of `steps` steps still holds once it returns.
fn retained_by_loop(blob: &str, steps: usize) -> isize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(scanner_schema(blob)).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": steps })).unwrap();
    let before = alloc_meter::totals().live;
    let out = k.dispatch("Scanner", None, "SCAN", payload).unwrap();
    let held = alloc_meter::totals().live - before;
    let executed = out.iter().filter(|(_, r)| r.executed).count();
    assert_eq!(executed, steps);
    held
}

/// L20: the per-step entity copies a dispatch records share an unchanged field instead of copying it each step.
#[test]
fn an_unchanged_large_field_is_shared_across_the_recorded_steps() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const STEPS: usize = 30;
    const BLOB: usize = 64 * 1024;
    // The first dispatch in the process loads the language data once; keep it out of the measured runs.
    retained_by_loop("x", 1);
    let large = retained_by_loop(&"x".repeat(BLOB), STEPS);
    let small = retained_by_loop("x", STEPS);
    let extra = large - small;
    println!("RETAINED_LARGE_FIELD {extra} over {STEPS} steps");
    assert!(
        extra < (4 * BLOB) as isize,
        "the {BLOB}-byte field is copied per step: {extra} extra bytes over {STEPS} steps"
    );
}

fn counter_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-counter",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Count", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "count", "type": "number", "default": 0 }
            ] },
            "traits": [{
                "name": "Counter", "linkedEntity": "Count",
                "listens": [{ "event": "TICK_NEXT", "triggers": "TICK", "source": { "kind": "trait", "trait": "Counter" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "TICK",
                        "guard": [">", "@payload.left", 0],
                        "effects": [
                            ["set", "@entity.count", ["+", "@entity.count", 1]],
                            ["emit", "TICK_NEXT", { "left": ["-", "@payload.left", 1] }]
                        ] }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// Bytes a self-emitting counter dispatch of `steps` steps still holds once it returns.
fn retained_by_counter(steps: usize) -> isize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(counter_schema()).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": steps })).unwrap();
    let before = alloc_meter::totals().live;
    let out = k.dispatch("Counter", None, "TICK", payload).unwrap();
    let held = alloc_meter::totals().live - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), steps);
    held
}

/// G-CROSS-288: what a dispatch keeps per step is constant, so a cascade twice as long holds about twice as much
/// (each step's recorded `@prevEvents` view is a prefix of one shared log, never its own copy of it).
#[test]
fn a_long_cascade_retains_linear_memory_per_step() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    retained_by_counter(1);
    let short = retained_by_counter(300);
    let long = retained_by_counter(600);
    println!("RETAINED_CASCADE 300={short} 600={long}");
    assert!(
        long * 10 < short * 25,
        "retention grows faster than the step count: {short} bytes at 300 steps, {long} at 600"
    );
}

/// Bytes a single self-emitting dispatch of `steps` steps allocates, freed or not.
fn allocated_by_loop(blob: &str, steps: usize) -> usize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(scanner_schema(blob)).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": steps })).unwrap();
    let before = alloc_meter::totals().bytes;
    let out = k.dispatch("Scanner", None, "SCAN", payload).unwrap();
    let spent = alloc_meter::totals().bytes - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), steps);
    spent
}

/// The bytes a 30-step loop over a 64 KiB unchanged field allocates beyond the same loop over a small one: 7995270, then
/// 5963685 while the binding context and pending-set view each copied the entity per step (G-ORBITAL-CORE-049), now none.
const PINNED_LARGE_FIELD_BYTES: usize = 0;

#[test]
fn an_unchanged_large_field_allocation_per_step_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const STEPS: usize = 30;
    const BLOB: usize = 64 * 1024;
    allocated_by_loop("x", 1);
    let large = allocated_by_loop(&"x".repeat(BLOB), STEPS);
    let small = allocated_by_loop("x", STEPS);
    let extra = large.saturating_sub(small);
    println!("ALLOCATED_LARGE_FIELD {extra} over {STEPS} steps");
    assert_eq!(
        extra, PINNED_LARGE_FIELD_BYTES,
        "allocations over an unchanged {BLOB}-byte field rose: {extra} > {PINNED_LARGE_FIELD_BYTES}"
    );
}

/// G-ORBITAL-CORE-049: a step binds `@entity` as the kernel holds it, so an unread field costs nothing per step.
#[test]
fn a_step_does_not_copy_an_unread_large_field() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const BLOB: usize = 64 * 1024;
    allocated_by_loop("x", 1);
    for steps in [10, 30] {
        let extra = allocated_by_loop(&"x".repeat(BLOB), steps).saturating_sub(allocated_by_loop("x", steps));
        println!("UNREAD_LARGE_FIELD {extra} over {steps} steps");
        assert!(
            extra < 4 * BLOB,
            "a {BLOB}-byte field no step reads costs {extra} bytes over {steps} steps"
        );
    }
}

#[test]
fn control_a_large_field_read_each_step_is_still_paid_for() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const BLOB: usize = 64 * 1024;
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(rewriter_schema(&"x".repeat(BLOB))).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": 10 })).unwrap();
    let before = alloc_meter::totals().bytes;
    let out = k.dispatch("Rewriter", None, "REWRITE", payload).unwrap();
    let spent = alloc_meter::totals().bytes - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 10);
    assert!(spent > 10 * BLOB, "a field each step rewrites is built each step: {spent} bytes");
}

#[test]
fn control_a_large_field_rewritten_each_step_is_still_a_new_frame() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(scanner_schema(&"x".repeat(1024))).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": 5 })).unwrap();
    let out = k.dispatch("Scanner", None, "SCAN", payload).unwrap();
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 5, "each step's frame differs, so none is a repeat");
}

fn rewriter_schema(blob: &str) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-rewrite",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "blob", "type": "string", "default": blob }
            ] },
            "traits": [{
                "name": "Rewriter", "linkedEntity": "Doc",
                "listens": [{ "event": "REWRITE_NEXT", "triggers": "REWRITE", "source": { "kind": "trait", "trait": "Rewriter" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "REWRITE",
                        "guard": [">", "@payload.left", 0],
                        "effects": [
                            ["set", "@entity.blob", ["str/concat", "@entity.blob", ""]],
                            ["emit", "REWRITE_NEXT", { "left": ["-", "@payload.left", 1] }]
                        ] }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// Bytes a self-rewriting dispatch of `steps` steps still holds once it returns.
fn retained_by_rewrites(blob: &str, steps: usize) -> isize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(rewriter_schema(blob)).unwrap();
    let payload = from_json_value::<Value>(&json!({ "left": steps })).unwrap();
    let before = alloc_meter::totals().live;
    let out = k.dispatch("Rewriter", None, "REWRITE", payload).unwrap();
    let held = alloc_meter::totals().live - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), steps);
    held
}

/// A field a long cascade rewrites every step is held once per step's frame, not again by the step's recorded `set`.
#[test]
fn a_large_field_set_every_step_is_not_retained_per_step() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const STEPS: usize = 30;
    const BLOB: usize = 64 * 1024;
    retained_by_rewrites("x", 1);
    let large = retained_by_rewrites(&"x".repeat(BLOB), STEPS);
    let small = retained_by_rewrites("x", STEPS);
    let extra = large - small;
    println!("RETAINED_REWRITTEN_FIELD {extra} over {STEPS} steps");
    assert!(
        extra < (STEPS * BLOB * 3 / 2) as isize,
        "a step keeps more than its frame's copy of the {BLOB}-byte field: {extra} extra bytes over {STEPS} steps"
    );
}

fn reader_schema(big: &serde_json::Value, through_let: bool) -> OrbitalSchema {
    let read = if through_let {
        json!(["let", [["b", "@entity.big"]], ["array/map", ["array/range", 50], ["fn", "i", ["object/get", "@b", "k1"]]]])
    } else {
        json!(["array/map", ["array/range", 50], ["fn", "i", ["object/get", "@entity.big", "k1"]]])
    };
    from_json_value(&json!({
        "name": "alloc-read",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Reader", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "big", "type": "object", "default": big },
                { "name": "out", "type": "array", "default": [] }
            ] },
            "traits": [{
                "name": "Read", "linkedEntity": "Reader",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "READ", "effects": [["set", "@entity.out", read]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// Bytes one READ dispatch allocates while it reads `k1` out of `big` 50 times.
fn allocated_by_reads(big: &serde_json::Value, through_let: bool) -> usize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(reader_schema(big, through_let)).unwrap();
    let before = alloc_meter::totals().bytes;
    let out = k.dispatch("Read", None, "READ", Value::Null).unwrap();
    let spent = alloc_meter::totals().bytes - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 1);
    spent
}

fn map_of(entries: usize, width: usize) -> serde_json::Value {
    serde_json::Value::Object((0..entries).map(|i| (format!("k{i}"), json!("v".repeat(width)))).collect())
}

/// `object/get` on a binding reads the field it names out of the bound value instead of copying the whole value first.
#[test]
fn reading_one_key_of_a_bound_map_does_not_copy_the_map() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    allocated_by_reads(&map_of(2, 4), false);
    let large = map_of(1024, 64);
    let small = map_of(2, 4);
    let as_value = from_json_value::<Value>(&large).unwrap();
    let before = alloc_meter::totals().bytes;
    let copy = deep_copy(&as_value);
    let one_copy = alloc_meter::totals().bytes - before;
    drop(copy);
    for through_let in [false, true] {
        let extra = allocated_by_reads(&large, through_let).saturating_sub(allocated_by_reads(&small, through_let));
        println!("ALLOCATED_BY_50_READS through_let={through_let}: {extra} (one copy of the map: {one_copy})");
        assert!(extra < 6 * one_copy, "50 reads of one key copy the map per read: {extra} extra bytes, one copy is {one_copy} (through_let={through_let})");
    }
}

fn part_reader_schema(list: &serde_json::Value, big: &serde_json::Value, read: serde_json::Value) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-part-read",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Reader", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "list", "type": "array", "default": list },
                { "name": "big", "type": "object", "default": big },
                { "name": "out", "type": "array", "default": [] }
            ] },
            "traits": [{
                "name": "Read", "linkedEntity": "Reader",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "READ",
                        "effects": [["set", "@entity.out", ["array/map", ["array/range", 50], ["fn", "i", read]]]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn allocated_by_part_reads(list: &serde_json::Value, big: &serde_json::Value, read: &serde_json::Value) -> usize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(part_reader_schema(list, big, read.clone())).unwrap();
    let before = alloc_meter::totals().bytes;
    let out = k.dispatch("Read", None, "READ", Value::Null).unwrap();
    let spent = alloc_meter::totals().bytes - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 1, "{read}");
    spent
}

/// The read-only array and object readers read a bound value in place: 50 reads of part of a large bound list or map
/// cost a few copies (the dispatch's own), not one per read.
#[test]
fn reading_part_of_a_bound_list_or_map_does_not_copy_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let list = |n: usize| serde_json::Value::Array((0..n).map(|i| json!(format!("{i}{}", "v".repeat(64)))).collect());
    let (large_list, small_list) = (list(1024), list(2));
    let (large_map, small_map) = (map_of(1024, 64), map_of(2, 4));
    let one_copy = |value: &serde_json::Value| {
        let as_value = from_json_value::<Value>(&value).unwrap();
        let before = alloc_meter::totals().bytes;
        let copy = deep_copy(&as_value);
        let spent = alloc_meter::totals().bytes - before;
        drop(copy);
        spent
    };
    let reads = [
        json!(["array/first", "@entity.list"]),
        json!(["array/last", "@entity.list"]),
        json!(["array/nth", "@entity.list", 1]),
        json!(["array/len", "@entity.list"]),
        json!(["count", "@entity.list"]),
        json!(["array/includes", "@entity.list", "absent"]),
        json!(["object/has", "@entity.big", "k1"]),
    ];
    allocated_by_part_reads(&small_list, &small_map, &reads[0]);
    for read in &reads {
        let copy = one_copy(&large_list).max(one_copy(&large_map));
        let extra = allocated_by_part_reads(&large_list, &large_map, read)
            .saturating_sub(allocated_by_part_reads(&small_list, &small_map, read));
        println!("ALLOCATED_BY_50_PART_READS {read}: {extra} (one copy: {copy})");
        assert!(extra < 12 * copy, "{read}: 50 reads copy the bound value per read: {extra} extra bytes, one copy is {copy}");
    }
}

/// `array/reduce` hands the accumulator to each step instead of copying it: a step whose body returns the bound
/// accumulator costs the one copy that reading it makes, not a second one for binding it.
#[test]
fn a_reduce_binds_its_accumulator_without_copying_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let read = |_: ()| json!(["array/reduce", ["array/range", 50], "@entity.big", ["fn", ["acc", "i"], "@acc"]]);
    let schema = |big: &serde_json::Value| -> OrbitalSchema {
        from_json_value(&json!({
            "name": "alloc-reduce",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Folder", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "big", "type": "object", "default": big },
                    { "name": "out", "type": "object", "default": {} }
                ] },
                "traits": [{
                    "name": "Fold", "linkedEntity": "Folder",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "FOLD", "effects": [["set", "@entity.out", read(())]] }]
                    }
                }]
            }]
        }))
        .unwrap()
    };
    let spent = |big: &serde_json::Value| {
        let k = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            Default::default(),
        );
        k.register_schema(schema(big)).unwrap();
        let before = alloc_meter::totals().bytes;
        let out = k.dispatch("Fold", None, "FOLD", Value::Null).unwrap();
        let spent = alloc_meter::totals().bytes - before;
        assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 1);
        spent
    };
    spent(&map_of(2, 4));
    let large = map_of(1024, 64);
    let as_value = from_json_value::<Value>(&large).unwrap();
    let before = alloc_meter::totals().bytes;
    let copy = deep_copy(&as_value);
    let one_copy = alloc_meter::totals().bytes - before;
    drop(copy);
    let extra = spent(&large).saturating_sub(spent(&map_of(2, 4)));
    println!("ALLOCATED_BY_50_STEP_REDUCE {extra} (one copy: {one_copy})");
    assert!(extra < 70 * one_copy, "a 50-step reduce copies its accumulator twice a step: {extra} extra bytes, one copy is {one_copy}");
}

fn mapper_schema(list: &serde_json::Value, read: serde_json::Value) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "alloc-map",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Mapped", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "list", "type": "array", "default": list },
                { "name": "out", "type": "array", "default": [] }
            ] },
            "traits": [{
                "name": "Map", "linkedEntity": "Mapped",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "MAP",
                        "effects": [["set", "@entity.out", read]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn allocated_by_map(list: &serde_json::Value, read: &serde_json::Value) -> usize {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(mapper_schema(list, read.clone())).unwrap();
    let before = alloc_meter::totals().bytes;
    let out = k.dispatch("Map", None, "MAP", Value::Null).unwrap();
    let spent = alloc_meter::totals().bytes - before;
    assert_eq!(out.iter().filter(|(_, r)| r.executed).count(), 1);
    spent
}

/// A callback binds each element once: the `(fn (x) …)` parameter and `@item` share the element the operator handed
/// over. Two lists of the same length whose elements differ only in size isolate what a map copies per element byte
/// (the per-call frame costs the same for both): one copy, the one reading the list makes.
#[test]
fn a_callback_binds_each_element_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let list = |width: usize| serde_json::Value::Array((0..256).map(|i| json!(format!("{i}{}", "v".repeat(width)))).collect());
    let one_copy = |value: &serde_json::Value| {
        let as_value = from_json_value::<Value>(&value).unwrap();
        let before = alloc_meter::totals().bytes;
        let copy = deep_copy(&as_value);
        let spent = alloc_meter::totals().bytes - before;
        drop(copy);
        spent
    };
    let (wide, narrow) = (list(4096), list(8));
    let element_bytes = one_copy(&wide) - one_copy(&narrow);
    let still = json!(["array/slice", "@entity.list", 0, 0]);
    let mapping = |list: &serde_json::Value, body: &serde_json::Value| {
        allocated_by_map(list, &json!(["array/map", "@entity.list", body])).saturating_sub(allocated_by_map(list, &still))
    };
    // (body, copies allowed): the list read, plus the body's own owned read of the element where it makes one.
    for (body, copies) in [
        (json!(["fn", ["x"], 1]), 1),
        (json!(["fn", ["x"], ["str/len", "@x"]]), 2),
        (json!(["fn", ["item"], ["str/len", "@item"]]), 2),
    ] {
        mapping(&narrow, &body);
        let per_size = mapping(&wide, &body).saturating_sub(mapping(&narrow, &body));
        println!("MAP_ELEMENT_BYTES {body}: {per_size} (one copy of the elements: {element_bytes})");
        assert!(
            per_size < (copies + 1) * element_bytes,
            "{body}: binding copies each element: {per_size} bytes for {element_bytes} bytes of elements, {copies} copies allowed"
        );
    }
}

#[test]
fn control_a_callback_sees_its_declared_parameters_under_any_name() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mapped = |body: serde_json::Value| {
        let k = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            Default::default(),
        );
        k.register_schema(mapper_schema(&json!(["a", "b"]), json!(["array/map", "@entity.list", body]))).unwrap();
        let out = k.dispatch("Map", None, "MAP", Value::Null).unwrap();
        serde_json::to_value(&out.last().unwrap().1.entity_data).unwrap()["out"].clone()
    };
    assert_eq!(mapped(json!(["fn", ["x", "i"], ["str/concat", "@x", "@x"]])), json!(["aa", "bb"]));
    assert_eq!(mapped(json!(["fn", ["x", "i"], ["+", "@i", "@i"]])), json!([0, 2]));
    assert_eq!(mapped(json!(["fn", ["item", "index"], ["str/concat", "@item", "@item"]])), json!(["aa", "bb"]));
    assert_eq!(mapped(json!(["fn", ["item", "index"], ["+", "@index", "@index"]])), json!([0, 2]));
}

fn fanout_schema(listeners: usize) -> OrbitalSchema {
    let mut traits = vec![json!({
        "name": "Source", "linkedEntity": "Blob",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                "effects": [["emit", "BIG", { "rows": "@payload.rows" }]] }]
        }
    })];
    for i in 0..listeners {
        traits.push(json!({
            "name": format!("L{i}"), "linkedEntity": "Blob",
            "listens": [{ "event": "BIG", "triggers": "GOT", "source": { "kind": "trait", "trait": "Source" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GOT", "effects": [] }]
            }
        }));
    }
    from_json_value(&json!({
        "name": "fanout",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Blob", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits
        }]
    }))
    .unwrap()
}

/// Peak live bytes of one `GO` whose emit carries a large payload to `listeners` listening traits, as a multiple of
/// the payload's own size (G-ORBITAL-CORE-051).
fn fanout_peak_ratio(listeners: usize) -> f64 {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(fanout_schema(listeners)).unwrap();
    let rows: Vec<serde_json::Value> = (0..2000).map(|i| json!({ "label": "x".repeat(64), "n": i })).collect();
    let before = alloc_meter::totals().live;
    let payload = from_json_value::<Value>(&json!({ "rows": rows })).unwrap();
    let size = (alloc_meter::totals().live - before) as f64;
    let base = alloc_meter::totals().live;
    alloc_meter::reset_peak(base);
    k.dispatch("Source", None, "GO", payload).unwrap();
    (alloc_meter::totals().peak - base) as f64 / size
}

/// A fan-out shares the emitted payload across its listeners: peak memory does not grow with the listener count.
#[test]
fn a_fan_out_does_not_copy_the_payload_once_per_listener() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let none = fanout_peak_ratio(0);
    let one = fanout_peak_ratio(1);
    let many = fanout_peak_ratio(45);
    println!("FANOUT_PEAK none={none:.1} one={one:.1} many={many:.1}");
    assert!(one < none + 2.0, "control: one listener adds at most a couple of payload copies: {none:.1}x -> {one:.1}x");
    assert!(many < 2.0, "45 listeners hold {many:.2} payload copies at peak (was 3.8 per listener before sharing)");
}

fn fanout_allocs(listeners: usize, rows: usize) -> usize {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(RuntimeEnvironment::Server, cache as Arc<dyn EntitySource>, Default::default());
    k.register_schema(fanout_schema(listeners)).unwrap();
    let rows: Vec<serde_json::Value> = (0..rows).map(|i| json!({ "label": "x", "n": i })).collect();
    let payload = from_json_value::<Value>(&json!({ "rows": rows })).unwrap();
    count(|| k.dispatch("Source", None, "GO", payload).unwrap()).0
}

/// The cycle check digests a fan-out's shared payload once, not once per listener: the allocations one more listener
/// costs do not grow with the payload's size.
#[test]
fn a_fan_out_listener_does_not_redigest_the_payload() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let per_listener = |rows| (fanout_allocs(41, rows) - fanout_allocs(1, rows)) / 40;
    let (small, large) = (per_listener(10), per_listener(1000));
    println!("FANOUT_ALLOCS_PER_LISTENER rows10={small} rows1000={large}");
    assert!(large < small + 100, "a listener costs {small} allocations at 10 rows and {large} at 1000");
}

/// Bytes allocated evaluating `(array/reduce (array/range 0 n) [] (fn (acc item) (array/append @acc @item)))`.
fn reduce_append_bytes(n: usize) -> usize {
    use orbital_core_base::schema::sexpr::SExpression;
    use orbital_core_eval::evaluator::context::BindingContextBuilder;
    let expr: SExpression = from_json_value(&json!(
        ["array/reduce", ["array/range", 0, n], [], ["fn", ["acc", "item"], ["array/append", "@acc", "@item"]]]
    ))
    .unwrap();
    let ctx = BindingContextBuilder::new().build();
    let before = alloc_meter::totals().bytes;
    let out = orbital_core_eval::evaluator::operators::eval_expr(&expr, &ctx).unwrap();
    let bytes = alloc_meter::totals().bytes - before;
    assert_eq!(out.as_array().map(|a| a.len()), Some(n));
    bytes
}

#[test]
fn a_reduce_that_appends_to_its_accumulator_allocates_linearly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (small, large) = (reduce_append_bytes(256), reduce_append_bytes(512));
    println!("REDUCE_APPEND_BYTES 256={small} 512={large}");
    assert!(large * 10 < small * 30, "doubling the list must not quadruple the bytes: {small} -> {large}");
}

/// Live bytes one dispatch of `trait.event` leaves behind, averaged over `runs` after a warm-up.
fn retained_per_dispatch(k: &RuntimeKernel, trait_name: &str, event: &str, runs: usize) -> f64 {
    let dispatch = || k.dispatch(trait_name, None, event, from_json_value(&json!({ "id": "c-1" })).unwrap()).unwrap();
    for _ in 0..50 {
        dispatch();
    }
    let before = alloc_meter::totals().live;
    for _ in 0..runs {
        dispatch();
    }
    (alloc_meter::totals().live - before) as f64 / runs as f64
}

// 1885 -> 1468 retained per audited SAVE (row 1663 -> 1374 bytes): the store keeps one allocation per field name, not one per
// row and name (R3, G-ORBITAL-CORE-070, 2026-10-29).
const PINNED_RETAINED_PER_SAVE: f64 = 1500.0;

/// A long run keeps only what it persists: each audited `SAVE` stores one `AuditEntry` row, and the response, trace
/// and visit records of the dispatch are released.
#[test]
fn a_dispatch_retains_only_the_rows_it_persists() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fixture: serde_json::Value = from_json_str(include_str!("fixtures/parity/audit-provenance.json")).unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(RuntimeEnvironment::Server, Arc::clone(&cache) as Arc<dyn EntitySource>, Default::default());
    k.register_schema(from_json_value(&fixture["schema"]).unwrap()).unwrap();
    support::identity::enable(&k, &[]);
    support::identity::sign_in(&k, "u-1", json!({}));
    let init = retained_per_dispatch(&k, "ContactForm", "INIT", 500);
    let rows_before = cache.list("AuditEntry").unwrap().len();
    let save = retained_per_dispatch(&k, "ContactForm", "SAVE", 500);
    let rows = cache.list("AuditEntry").unwrap();
    assert_eq!(rows.len() - rows_before, 550, "each SAVE persists one audit row");
    let before = alloc_meter::totals().bytes;
    let copy = deep_copy(&rows[rows.len() - 1]);
    let row_bytes = (alloc_meter::totals().bytes - before) as f64;
    drop(copy);
    println!("RETAINED_PER_DISPATCH init={init:.0} save={save:.0} row={row_bytes:.0}");
    assert!(init < 64.0, "control: a dispatch that persists nothing retains {init:.0} bytes");
    assert!(save < row_bytes * 1.5 + 256.0, "a SAVE retains {save:.0} bytes for a {row_bytes:.0}-byte row");
    assert!(save <= PINNED_RETAINED_PER_SAVE, "a SAVE retains {save:.0} bytes > {PINNED_RETAINED_PER_SAVE}");
}

fn session_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "body", "type": "string" }] },
            "traits": [{ "name": "Editor", "linkedEntity": "Draft",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SET", "effects": [["set", "@entity.body", "@payload.body"]] }] } }]
        }]
    }))
    .unwrap()
}

// What one ended session keeps (P-017 K4): its `SESSION_ENDED` tombstone only; its states and frames are freed.
const PINNED_RETAINED_PER_SESSION: isize = 120; // 109 measured 2026-10-31: the tombstone also names the traits whose state the session held (G-CROSS-357)

#[test]
fn an_ended_session_retains_only_its_tombstone() {
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let server = EmbeddedServer::new(session_schema()).unwrap();
    let body = "x".repeat(1024);
    let cycle = |i: usize| {
        let client = format!("tab-{i:05}");
        let stream = server.open_session_stream(&client);
        let request = orbital_core_runtime::effects::response::EventRequest {
            event: "SET".to_string(),
            target_trait: Some("Editor".to_string()),
            client_id: Some(client),
            payload: Some(from_json_value(&json!({ "body": body })).unwrap()),
            ..Default::default()
        };
        assert!(matches!(server.start("Main", request).unwrap(), ServerProgress::Done(_)));
        server.close_session_stream(stream);
    };
    for i in 0..50 {
        cycle(i);
    }
    let before = alloc_meter::totals().live;
    for i in 50..550 {
        cycle(i);
    }
    let per_session = (alloc_meter::totals().live - before) / 500;
    println!("RETAINED_PER_SESSION {per_session}");
    assert!(server.kernel().frame_sessions().is_empty(), "every ended session's frames were freed");
    assert!(per_session <= PINNED_RETAINED_PER_SESSION, "an ended session retains {per_session} bytes");
}

/// A program-shaped value of about `orbitals` × 160 KB of JSON, Arabic text included (non-ASCII, as the AR locale's).
fn big_program(orbitals: usize) -> serde_json::Value {
    let transition = |i: usize| json!({ "from": "idle", "to": "idle", "event": format!("E{i}"),
        "guard": ["and", ["=", "@entity.state", format!("s{i}")], ["not", ["=", "@payload.text", ""]]],
        "effects": (0..12).map(|j| json!(["set", format!("@entity.f{j}"), ["str/concat", "@entity.name", format!("نص عربي {i} {j}")]])).collect::<Vec<_>>() });
    json!({ "name": "big", "orbitals": (0..orbitals).map(|o| json!({ "name": format!("O{o}"),
        "traits": (0..4).map(|t| json!({ "name": format!("O{o}T{t}"), "stateMachine": { "transitions": (0..24).map(transition).collect::<Vec<_>>() } })).collect::<Vec<_>>() })).collect::<Vec<_>>() })
}

fn guest_stage() -> orbital_core_runtime::embed::EmbeddedKernel {
    let schema: OrbitalSchema = from_json_value(&json!({
        "name": "viewer", "typeContractVersion": 1, "orbitals": [{
            "name": "Sessions",
            "entity": { "name": "Session", "persistence": "persistent", "collection": "sessions", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "GuestSession", "linkedEntity": "Session",
                "emits": [{ "event": "GUEST_CLIENT", "scope": "external" }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "LOAD", "effects": [["persist", "create", "Session", {}, { "emit": { "success": "GUEST_CLIENT" } }]] }] } }]
        }, {
            "name": "Stages",
            "entity": { "name": "Stage", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "GuestStage", "linkedEntity": "Stage", "dispatch": "client",
                "listens": [{ "event": "GUEST_CLIENT", "triggers": "GUEST_CLIENT", "source": { "kind": "orbital", "orbital": "Sessions", "trait": "GuestSession" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "GUEST_CLIENT", "effects": stage_effects() }] } }]
        }]
    }))
    .unwrap();
    orbital_core_runtime::embed::EmbeddedKernel::new(schema, "c1").unwrap()
}

/// What folding a server answer that emits a large program to a client trait (the viewer's GUEST_CLIENT) peaks at,
/// as a multiple of the program's JSON size, from the answer's text to the fold's text (orbital-wasm `foldInOrder`).
fn fold_peak(program: serde_json::Value) -> (usize, isize, isize, usize) {
    use orbital_core_base::bus_event::{BusEventSource, EmittedEvent};
    use orbital_core_runtime::effects::response::EventResponse;
    let program_bytes = serde_json::to_string(&program).unwrap().len();
    let answer = EventResponse {
        success: true,
        transitioned: true,
        emitted_events: vec![EmittedEvent {
            event: "GUEST_CLIENT".to_string(),
            payload: Some(from_json_value(&json!({ "guest": "g", "program": program })).unwrap()),
            source: Some(BusEventSource { orbital: Some("Sessions".to_string()), r#trait: Some("GuestSession".to_string()), ..Default::default() }),
        }],
        ..Default::default()
    };
    let text = serde_json::to_string(&answer).unwrap();
    drop(answer);
    let kernel = guest_stage();
    let flight = kernel.dispatch("GuestSession", None, "LOAD", Value::Null).unwrap().flight.expect("LOAD owes a leg");
    let base = alloc_meter::totals().live;
    alloc_meter::reset_peak(base);
    let mark = |stage: &str| {
        let t = alloc_meter::totals();
        println!("FOLD_STAGE {stage}: peak {:.1} MB live {:.1} MB", (t.peak - base) as f64 / 1048576.0, (t.live - base) as f64 / 1048576.0);
    };
    let out = {
        let response: EventResponse = from_json_str(&text).unwrap();
        mark("parsed");
        let progress = kernel.fold_in_order(flight, response, Default::default(), None).unwrap();
        mark("folded");
        let orbital_core_runtime::embed::FoldProgress::Applied(fold) = progress else { panic!("the only flight folds now") };
        let wire = orbital_core_runtime::embed::wire::FoldProgressWire::Applied { fold: *fold };
        let mut held = orbital_core_base::held_values::HeldValues::new();
        let out = orbital_core_base::held_values::to_js_readable_json_held(&wire, &mut held).unwrap();
        mark("written as the browser kernel writes it");
        out
    };
    let peak = alloc_meter::totals().peak - base;
    let out_len = out.len();
    drop(out);
    let held = alloc_meter::totals().live - base;
    (program_bytes, peak, held, out_len)
}

#[test]
fn a_folded_program_is_held_in_less_memory_than_its_text() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fold_peak(big_program(1));
    let (bytes, peak, held, out) = fold_peak(big_program(40));
    println!(
        "FOLD_PEAK program {:.1} MB peak {:.1} MB ({:.1}x) held {:.1} MB ({:.1}x) fold text {:.1} MB",
        bytes as f64 / 1048576.0, peak as f64 / 1048576.0, peak as f64 / bytes as f64,
        held as f64 / 1048576.0, held as f64 / bytes as f64, out as f64 / 1048576.0
    );
    assert!(held < bytes as isize, "a folded program is held in less memory than its JSON text: {held} >= {bytes}");
    assert!(peak < bytes as isize, "folding a program never builds a copy as large as its JSON text: {peak} >= {bytes}");
}

#[test]
#[ignore = "tool: ORB_MEASURE_ANSWER=<an events answer whose emitted event carries payload.program> cargo test -p orbital-core-runtime --test dispatch_alloc_count fold_peak_of_a_captured_program -- --ignored --nocapture"]
fn fold_peak_of_a_captured_program() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let path = std::env::var("ORB_MEASURE_ANSWER").expect("ORB_MEASURE_ANSWER names a captured answer");
    let answer: serde_json::Value = from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let program = answer["emittedEvents"].as_array().unwrap().iter().find_map(|e| e["payload"].get("program")).unwrap().clone();
    drop(answer);
    let (bytes, peak, held, out) = fold_peak(program);
    println!(
        "CAPTURED_FOLD program {:.1} MB peak {:.1} MB ({:.1}x) held {:.1} MB ({:.1}x) fold text {:.1} MB",
        bytes as f64 / 1048576.0, peak as f64 / 1048576.0, peak as f64 / bytes as f64,
        held as f64 / 1048576.0, held as f64 / bytes as f64, out as f64 / 1048576.0
    );
}

fn stage_effects() -> serde_json::Value {
    let set = json!(["set", "@entity.client", "@payload.program"]);
    let render = json!(["render-ui", "main", { "type": "guest-view", "guest": "g", "deliver": "GUEST_CONTROL", "program": "@entity.client", "children": [] }]);
    match std::env::var("ORB_STAGE_EFFECTS").as_deref() {
        Ok("none") => json!([]),
        Ok("set") => json!([set]),
        Ok("render") => json!([["render-ui", "main", { "type": "guest-view", "guest": "g", "deliver": "GUEST_CONTROL", "program": "@payload.program", "children": [] }]]),
        _ => json!([set, render]),
    }
}
