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

#[global_allocator]
static A: Counting = Counting;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn kernel(ticking: usize) -> EmbeddedKernel {
    let traits: Vec<_> = (0..ticking)
        .map(|i| {
            json!({
                "id": format!("trt_bench_{i}"), "name": format!("FrameTicker{i}"),
                "linkedEntity": "Counter", "local": true, "scope": "instance",
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
        "orbitals": [{
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

const PINNED_100: usize = 17068;
const PINNED_1000: usize = 170105;

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
