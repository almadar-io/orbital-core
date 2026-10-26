use super::*;
use crate::entity_source::InMemoryEntityCache;
use orbital_core_base::protocols::ProtocolSide;

fn trait_json(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [] }]
        }
    })
}

fn kernel(pages: serde_json::Value, traits: &[&str]) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits.iter().map(|t| trait_json(t)).collect::<Vec<_>>(),
            "pages": pages
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn names(seeds: Vec<crate::effects::response::MountSeed>) -> Vec<String> {
    assert!(seeds.iter().all(|s| s.event == crate::effects::response::LifecycleEvent::Init));
    seeds.into_iter().map(|s| s.r#trait).collect()
}

fn app() -> RuntimeKernel {
    kernel(
        serde_json::json!([
            { "name": "Home", "path": "/", "traits": [{ "ref": "RouteT" }] },
            { "name": "Other", "path": "/other", "traits": [{ "ref": "OtherT" }] },
            { "name": "Api", "path": "", "protocol": "http", "traits": [{ "ref": "HttpT" }] },
            { "name": "Mcp", "path": "", "protocol": "stdio", "traits": [{ "ref": "StdioT" }] },
            { "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "BootT" }] }
        ]),
        &["RouteT", "OtherT", "HttpT", "StdioT", "BootT", "Orphan"],
    )
}

fn lifecycle_kernel() -> RuntimeKernel {
    let with = |name: &str, event: &str| serde_json::json!({
        "name": name,
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": event, "effects": [] }]
        }
    });
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [with("Inits", "INIT"), with("Loads", "LOAD"), with("Mounts", "$MOUNT"), with("Deaf", "GO")],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Inits" }, { "ref": "Loads" }, { "ref": "Mounts" }, { "ref": "Deaf" }] }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn each_mount_seed_carries_the_first_lifecycle_event_its_trait_handles() {
    use crate::effects::response::LifecycleEvent;
    let seeds: Vec<(String, LifecycleEvent)> =
        lifecycle_kernel().route_mounts("/").into_iter().map(|s| (s.r#trait, s.event)).collect();
    assert_eq!(
        seeds,
        [
            ("Inits".to_string(), LifecycleEvent::Init),
            ("Loads".to_string(), LifecycleEvent::Load),
            ("Mounts".to_string(), LifecycleEvent::Mount),
        ],
        "a trait that handles no lifecycle event is not seeded"
    );
}

#[test]
fn the_server_host_boots_the_http_and_boot_traits() {
    assert_eq!(names(app().boot_mounts(ProtocolSide::Server)), ["HttpT", "BootT"]);
}

#[test]
fn the_process_host_boots_the_stdio_and_boot_traits() {
    assert_eq!(names(app().boot_mounts(ProtocolSide::Process)), ["StdioT", "BootT"]);
}

#[test]
fn a_route_trait_never_boots_and_the_client_side_has_no_start_mounts() {
    let k = app();
    for side in [ProtocolSide::Server, ProtocolSide::Process, ProtocolSide::Start] {
        assert!(!names(k.boot_mounts(side)).contains(&"RouteT".to_string()), "{side:?}");
    }
    assert!(k.boot_mounts(ProtocolSide::Client).is_empty());
}

#[test]
fn a_route_mounts_only_that_routes_traits() {
    let k = app();
    assert_eq!(names(k.route_mounts("/")), ["RouteT"]);
    assert_eq!(names(k.route_mounts("/other")), ["OtherT"]);
    assert!(k.route_mounts("/missing").is_empty());
}

#[test]
fn a_trait_on_no_page_is_in_neither() {
    let k = app();
    let mut all = names(k.boot_mounts(ProtocolSide::Server));
    all.extend(names(k.boot_mounts(ProtocolSide::Process)));
    all.extend(names(k.route_mounts("/")));
    all.extend(names(k.route_mounts("/other")));
    assert!(!all.contains(&"Orphan".to_string()));
}

#[test]
fn a_trait_listed_on_two_boot_pages_mounts_once() {
    let k = kernel(
        serde_json::json!([
            { "name": "A", "path": "", "protocol": "boot", "traits": [{ "ref": "T" }] },
            { "name": "B", "path": "", "protocol": "http", "traits": [{ "ref": "T" }] }
        ]),
        &["T"],
    );
    assert_eq!(names(k.boot_mounts(ProtocolSide::Server)), ["T"]);
}
