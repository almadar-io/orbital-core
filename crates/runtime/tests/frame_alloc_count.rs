//! Frame cost of N ticking traits (G-ORBITAL-CORE-002): the shape of the device rows
//! `frame-60hz-100` / `frame-60hz-1000` (one frame tick per trait, one `set` each, a shared
//! entity). Allocations per frame are pinned and only ever go down; time per frame is printed.

use orbital_core_runtime::embed::EmbeddedKernel;
use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static SAMPLING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SITES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn sample() {
    if SAMPLING.try_with(|s| s.replace(false)).unwrap_or(false) {
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        let _ = SITES.try_with(|sites| sites.borrow_mut().push(trace));
        let _ = SAMPLING.try_with(|s| s.set(true));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        sample();
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

#[global_allocator]
static A: Counting = Counting;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn kernel(ticking: usize) -> EmbeddedKernel {
    let traits: Vec<_> = (0..ticking)
        .map(|i| {
            json!({
                "id": format!("trt_bench_{i}"), "name": format!("FrameTicker{i}"),
                "linkedEntity": "Counter", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "running", "isInitial": true }, { "name": "stopped" }],
                    "transitions": [{ "from": "running", "to": "running", "event": "INIT", "effects": [
                        ["render-ui", "main", { "type": "stat-display", "label": "Beats", "value": "@entity.frames" }]] }]
                },
                "ticks": [{ "name": "frameTick", "interval": "frame",
                    "effects": [["set", "@entity.frames", ["+", "@entity.frames", 1.0]]] }]
            })
        })
        .collect();
    let pages_traits: Vec<_> = (0..ticking)
        .map(|i| json!({ "ref": format!("FrameTicker{i}"), "refId": format!("trt_bench_{i}") }))
        .collect();
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "frame-bench",
        "typeContractVersion": 1, "orbitals": [{
            "name": "TickCounterOrbital",
            "entity": { "name": "Counter", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "frames", "type": "number", "default": 0.0 }] },
            "traits": traits,
            "pages": [{ "id": "pag_bench", "name": "BenchPage", "path": "/bench", "traits": pages_traits }]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "bench").unwrap()
}

/// (allocations of one steady-state frame, mean ms per frame over `frames`).
fn measure(ticking: usize, frames: u64) -> (usize, f64) {
    let k = kernel(ticking);
    for i in 0..ticking {
        k.dispatch(
            &format!("FrameTicker{i}"),
            None,
            "INIT",
            orbital_core_base::value::Value::Null,
        )
        .unwrap();
    }
    let step = 1000.0 / 60.0;
    let mut now = 0.0f64;
    for _ in 0..5 {
        let f = k.frame(now as u64).unwrap().expect("frame fires");
        assert_eq!(f.fired.len(), ticking);
        now += step;
    }
    let before = ALLOCS.load(Ordering::SeqCst);
    let f = k.frame(now as u64).unwrap().expect("frame fires");
    let allocs = ALLOCS.load(Ordering::SeqCst) - before;
    assert_eq!(f.fired.len(), ticking);
    now += step;
    let t = Instant::now();
    for _ in 0..frames {
        k.frame(now as u64).unwrap();
        now += step;
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    // The C-ABI returns the frame as JSON text: time and size of that encoding.
    let f = k.frame(now as u64).unwrap().expect("frame fires");
    let t = Instant::now();
    let text = serde_json::to_string(&f).unwrap();
    println!(
        "  wire ticking={ticking} json_bytes={} encode_ms={:.3}",
        text.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    (allocs, ms)
}

// Back to the pin: a ticking dispatch clones the kernel's one `prob/*` seed source (G-ORBITAL-CORE-029, 2026-10-18).
// 16468 / 164105: copy-on-write `Value` and `SExpression` strings, lists and maps (P-014, 2026-10-26).
// 16362 / 163096: a frame builds one render list (P-015 T2, 2026-10-27).
// 2026-10-29: versioned frames diff each dispatch against the session's view record (K5, +), and `SharedEntity::merged`
// clones a key only for a field the base lacks (G-ORBITAL-CORE-002, five fewer per ticking trait, -); together 16179 / 161119.
// 2026-10-29: `ValueMap` keys are interned `Shared<String>` names (R3, K1): entity layering and the per-tick `set` row share names, -12 per
// ticking trait; 14979 / 149119 (was 16179 / 161119).
const PINNED_100: usize = 14979;
const PINNED_1000: usize = 149119;

#[test]
fn frame_allocation_count_is_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (a100, ms100) = measure(100, 30);
    let (a1000, ms1000) = measure(1000, 5);
    println!("FRAME_100 allocs={a100} ms={ms100:.3}");
    println!("FRAME_1000 allocs={a1000} ms={ms1000:.3}");
    assert!(
        a100 <= PINNED_100,
        "100-trait frame allocations rose: {a100} > {PINNED_100}"
    );
    assert!(
        a1000 <= PINNED_1000,
        "1000-trait frame allocations rose: {a1000} > {PINNED_1000}"
    );
}

/// Profiling harness: `FRAME_PROFILE_SECS=10 cargo test -p orbital-core --test frame_alloc_count -- --ignored profile_loop`.
#[test]
#[ignore]
fn profile_loop() {
    let secs: u64 = std::env::var("FRAME_PROFILE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let k = kernel(1000);
    for i in 0..1000 {
        k.dispatch(
            &format!("FrameTicker{i}"),
            None,
            "INIT",
            orbital_core_base::value::Value::Null,
        )
        .unwrap();
    }
    let start = Instant::now();
    let mut now = 0u64;
    while start.elapsed().as_secs() < secs {
        k.frame(now).unwrap();
        now += 16;
    }
}

/// Frame-delta sizing: writes two consecutive steady-state frames of 100 ticking traits as JSON to `FRAME_DUMP_DIR`.
/// `FRAME_DUMP_DIR=<dir> cargo test -p orbital-core-runtime --test frame_alloc_count -- --ignored frame_wire_dump`.
#[test]
#[ignore]
fn frame_wire_dump() {
    let dir = std::path::PathBuf::from(std::env::var("FRAME_DUMP_DIR").expect("FRAME_DUMP_DIR names the output folder"));
    let k = kernel(100);
    for i in 0..100 {
        k.dispatch(&format!("FrameTicker{i}"), None, "INIT", orbital_core_base::value::Value::Null).unwrap();
    }
    let step = 1000.0 / 60.0;
    let mut now = 0.0f64;
    for n in 0..7 {
        let f = k.frame(now as u64).unwrap().expect("frame fires");
        if n >= 5 {
            std::fs::write(dir.join(format!("frame-{n}.json")), serde_json::to_string(&f).unwrap()).unwrap();
        }
        now += step;
    }
}

// 34698: a steady frame carries every fired trait's state, both render lists and every `set` result (P-015 measures it).
// 18973: one render list, only the states a tick moved, `effectResults` only for a subscribed host (P-015 T2, 2026-10-27).
// 19025: frames are a versioned delta, one `["set", {frame: {fields}}]` op per dispatch (K5, 2026-10-29): the `session`/`from`/`to`/`ops` header is +52 bytes (+0.27%) over the old `entityByTrait` rows (18973); per-op field naming cost 19615 and per-frame ops 19815.
const PINNED_WIRE_100: usize = 19025;

/// The bytes a steady frame of 100 ticking traits puts on the wire, which only ever go down.
#[test]
fn frame_wire_bytes_are_pinned() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let k = kernel(100);
    for i in 0..100 {
        k.dispatch(&format!("FrameTicker{i}"), None, "INIT", orbital_core_base::value::Value::Null).unwrap();
    }
    let step = 1000.0 / 60.0;
    let mut now = 0.0f64;
    for _ in 0..5 {
        k.frame(now as u64).unwrap().expect("frame fires");
        now += step;
    }
    let f = k.frame(now as u64).unwrap().expect("frame fires");
    assert_eq!(f.fired.len(), 100);
    let bytes = serde_json::to_string(&f).unwrap().len();
    println!("FRAME_WIRE_100 bytes={bytes}");
    assert!(bytes <= PINNED_WIRE_100, "100-trait frame wire rose: {bytes} > {PINNED_WIRE_100}");
}

/// Wire bytes of one mount and some follow-up events of a lowered program, the frame delta against the rows the pre-K5
/// response carried for the same writes (`entityByTrait`, one row per bound trait). Ignored: it needs a lowered `.orb`.
/// `BROWSE_ORB=<file> BROWSE_TRAIT=<trait> BROWSE_EVENTS=A,B cargo test -p orbital-core-runtime --test frame_alloc_count -- --ignored --nocapture browse_wire_sizes`.
#[test]
#[ignore]
fn browse_wire_sizes() {
    use orbital_core_runtime::effects::response::{EventRequest, LifecycleEvent, MountSeed};
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    let path = std::env::var("BROWSE_ORB").expect("BROWSE_ORB names the lowered .orb");
    let trait_name = std::env::var("BROWSE_TRAIT").expect("BROWSE_TRAIT names the trait");
    let text = std::fs::read_to_string(path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let schema: orbital_core_base::schema::types::OrbitalSchema = serde_json::from_value(json.get("orb").cloned().unwrap_or(json)).unwrap();
    let bound: Vec<(String, Option<String>)> = schema
        .orbitals
        .iter()
        .flat_map(|o| o.traits.iter())
        .filter_map(|t| t.trait_name().map(|n| (n.to_string(), None)))
        .collect();
    let server = EmbeddedServer::new(schema).unwrap();
    let report = |label: &str, response: &orbital_core_runtime::effects::EventResponse| {
        let Some(delta) = &response.frames else {
            println!("BROWSE {label}: no frames");
            return;
        };
        let after = serde_json::to_string(delta).unwrap().len();
        let mut frames = std::collections::BTreeMap::new();
        delta.fold_into(&mut frames);
        let mut rows = serde_json::Map::new();
        for (frame, fields) in &frames {
            let traits: Vec<&String> = match frame.strip_prefix("$shared::") {
                Some(_) => bound.iter().map(|(n, _)| n).collect(),
                None => vec![frame],
            };
            for t in traits {
                rows.insert(t.clone(), serde_json::to_value(fields).unwrap());
            }
        }
        let before = serde_json::to_string(&serde_json::json!({ "entityByTrait": rows })).unwrap().len();
        println!("BROWSE {label}: entityByTrait {before} bytes -> frames {after} bytes ({} ops)", delta.ops.len());
    };
    let drive = |request: EventRequest| {
        let mut progress = server.start(&std::env::var("BROWSE_ORBITAL").unwrap(), request).unwrap();
        loop {
            match progress {
                ServerProgress::Done(response) => return response,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|p| orbital_core_runtime::resumable::IoReply { id: p.id, answer: orbital_core_runtime::resumable::IoAnswer::Row(Err("no row".into())) })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
                ServerProgress::Queued { .. } => panic!("queued"),
            }
        }
    };
    let mount = EventRequest {
        event: "INIT".into(),
        client_id: Some("tab".into()),
        mount: Some(vec![MountSeed { r#trait: trait_name.clone(), event: LifecycleEvent::Init }]),
        ..Default::default()
    };
    report("INIT mount", &drive(mount));
    for event in std::env::var("BROWSE_EVENTS").unwrap_or_default().split(',').filter(|e| !e.is_empty()) {
        let request = EventRequest { event: event.into(), target_trait: Some(trait_name.clone()), client_id: Some("tab".into()), ..Default::default() };
        report(event, &drive(request));
    }
}

fn allocs_of<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = ALLOCS.load(Ordering::SeqCst);
    let out = f();
    (ALLOCS.load(Ordering::SeqCst) - before, out)
}

/// Stage breakdown of one steady-state tick of 100 ticking traits:
/// `cargo test -p orbital-core-runtime --test frame_alloc_count -- --ignored --nocapture stage_breakdown`.
#[test]
#[ignore]
fn stage_breakdown() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let k = kernel(100);
    for i in 0..100 {
        k.dispatch(&format!("FrameTicker{i}"), None, "INIT", orbital_core_base::value::Value::Null).unwrap();
    }
    for _ in 0..5 {
        k.frame(0).unwrap();
    }
    let kernel = k.kernel();
    let (a, _) = allocs_of(|| kernel.process_tick("FrameTicker7", None, "frameTick").unwrap());
    println!("process_tick one trait: {a}");
    let (a, _) = allocs_of(|| kernel.process_tick("FrameTicker7", None, "frameTick").unwrap());
    println!("process_tick one trait (again): {a}");
    let (a, d) = allocs_of(|| kernel.dispatch_tick_unrepainted("TickCounterOrbital", "FrameTicker7", None, "frameTick", Some("bench")).unwrap());
    println!("dispatch_tick_unrepainted one trait: {a} (rerenders {})", d.rerenders.len());
    let (a, r) = allocs_of(|| kernel.rerender_stale());
    println!("rerender_stale: {a} (rerenders {})", r.len());
    let (a, _) = allocs_of(|| kernel.rerender_stale());
    println!("rerender_stale again: {a}");
    let (a, f) = allocs_of(|| k.frame(1000).unwrap().unwrap());
    println!("frame total: {a} ticks {}", f.fired.len());
}

/// Where the allocations of one steady-state tick come from, by the nearest `orbital_core*` frames:
/// `cargo test -p orbital-core-runtime --test frame_alloc_count -- --ignored --nocapture tick_alloc_sites`.
#[test]
#[ignore]
fn tick_alloc_sites() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let k = kernel(100);
    for i in 0..100 {
        k.dispatch(&format!("FrameTicker{i}"), None, "INIT", orbital_core_base::value::Value::Null).unwrap();
    }
    for _ in 0..5 {
        k.frame(0).unwrap();
    }
    SITES.with(|s| s.borrow_mut().clear());
    SAMPLING.with(|s| s.set(true));
    k.kernel().process_tick("FrameTicker7", None, "frameTick").unwrap();
    SAMPLING.with(|s| s.set(false));
    let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
    let sites = SITES.with(|s| std::mem::take(&mut *s.borrow_mut()));
    for trace in &sites {
        let frames: Vec<String> = trace
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix("at "))
            .filter(|l| l.contains("/crates/orbital-core/crates/") && !l.contains("frame_alloc_count"))
            .map(|l| l.rsplit("/crates/").next().unwrap_or(l).to_string())
            .take(4)
            .collect();
        *tally.entry(frames.join("  <-  ")).or_default() += 1;
    }
    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    println!("TOTAL {}", sites.len());
    for (site, n) in rows.iter().take(25) {
        println!("{n:4}  {site}");
    }
}
