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
        "typeContractVersion": 1, "orbitals": [{
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
        "typeContractVersion": 1, "orbitals": [{
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

fn composer_kernel(child_reads_payload: bool) -> RuntimeKernel {
    let child_effect = if child_reads_payload {
        serde_json::json!(["render-ui", "main", { "type": "text", "content": "@callsitePayload.label" }])
    } else {
        serde_json::json!(["render-ui", "main", { "type": "text", "content": "plain" }])
    };
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Host", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [] },
                        { "from": "idle", "to": "idle", "event": "SHOW",
                          "effects": [["render-ui", "main", { "type": "stack", "children": ["@trait.Child"] }]] }] } },
                { "name": "Child", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [child_effect] }] } }
            ],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Host" }, { "ref": "Child" }] }]
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
fn an_embedded_child_reading_its_composers_payload_is_left_out_of_the_route_mounts() {
    assert_eq!(names(composer_kernel(true).route_mounts("/")), ["Host"]);
}

#[test]
fn control_an_embedded_child_that_reads_no_payload_mounts_with_the_page() {
    assert_eq!(names(composer_kernel(false).route_mounts("/")), ["Host", "Child"]);
}

#[test]
fn control_a_child_its_composer_has_composed_is_mounted_again() {
    let kernel = composer_kernel(true);
    kernel.composed_callsite.write().insert("Child".into(), Value::object(ValueMap::default()));
    assert_eq!(names(kernel.route_mounts("/")), ["Host", "Child"]);
}

fn paged() -> RuntimeKernel {
    kernel(
        serde_json::json!([
            { "name": "Home", "path": "/", "isInitial": true, "traits": [{ "ref": "RouteT" }] },
            { "name": "Other", "path": "/other/:id", "traits": [{ "ref": "OtherT" }] },
            { "name": "Gone", "path": "/gone", "isNotFound": true, "traits": [{ "ref": "HttpT" }] },
            { "name": "Api", "path": "", "protocol": "http", "traits": [{ "ref": "StdioT" }] }
        ]),
        &["RouteT", "OtherT", "HttpT", "StdioT"],
    )
}

#[test]
fn page_circuit_without_page_or_route_is_the_initial_page() {
    let c = paged().page_circuit(None, None).unwrap();
    assert_eq!(c.page.as_deref(), Some("Home"));
    assert_eq!(c.route.as_deref(), Some("/"));
    assert!(!c.not_found);
    assert_eq!(names(c.mounts), ["RouteT"]);
    assert_eq!(c.orbital_by_trait.get("RouteT").map(String::as_str), Some("Main"));
}

#[test]
fn page_circuit_named_page_wins_over_the_route() {
    let c = paged().page_circuit(Some("Other"), Some("/")).unwrap();
    assert_eq!(c.page.as_deref(), Some("Other"));
    assert_eq!(names(c.mounts), ["OtherT"]);
}

#[test]
fn page_circuit_matches_a_route_with_the_kernels_route_matcher() {
    let k = paged();
    let c = k.page_circuit(None, Some("/other/7")).unwrap();
    assert_eq!(c.page.as_deref(), Some("Other"));
    assert_eq!(c.route.as_deref(), Some("/other/7"));
    assert!(!c.not_found);
    assert_eq!(c.mounts, k.route_mounts("/other/:id"));
    assert_eq!(c.mounts, k.route_mounts("/other/7"));
}

#[test]
fn page_circuit_unmatched_route_is_the_not_found_page() {
    let k = paged();
    let c = k.page_circuit(None, Some("/nope")).unwrap();
    assert!(c.not_found);
    assert_eq!(c.page.as_deref(), Some("Gone"));
    assert_eq!(c.mounts, k.route_mounts("/gone"));
}

#[test]
fn page_circuit_unmatched_route_without_a_not_found_page_has_no_page() {
    let c = app().page_circuit(None, Some("/nope")).unwrap();
    assert!(c.not_found);
    assert_eq!(c.page, None);
    assert!(c.mounts.is_empty());
}

#[test]
fn page_circuit_unknown_or_protocol_page_name_is_an_error() {
    assert_eq!(paged().page_circuit(Some("Nope"), None), Err("unknown page: Nope".to_string()));
    assert_eq!(paged().page_circuit(Some("Api"), None), Err("unknown page: Api".to_string()));
}

#[test]
fn page_circuit_unknown_trait_owner_is_an_error() {
    let k = kernel(
        serde_json::json!([{ "name": "Home", "path": "/", "isInitial": true, "traits": [{ "ref": "Ghost" }] }]),
        &["RouteT"],
    );
    assert_eq!(k.page_circuit(None, None), Err("unknown trait: Ghost".to_string()));
}

fn embed_kernel(host_embeds: bool) -> RuntimeKernel {
    let host = serde_json::json!({
        "name": "Host",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                "effects": [["render-ui", "main", if host_embeds { "@trait.Child" } else { "none" }]] }]
        }
    });
    let mid = serde_json::json!({
        "name": "Child",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", "@trait.Leaf"]] }]
        }
    });
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [host, mid, trait_json("Leaf")],
            "pages": [{ "name": "Home", "path": "/", "isInitial": true, "traits": [{ "ref": "Host" }] }]
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
fn page_circuit_reports_the_traits_a_page_trait_embeds_transitively() {
    let c = embed_kernel(true).page_circuit(None, None).unwrap();
    assert_eq!(c.embedded, ["Child", "Leaf"]);
    for t in ["Host", "Child", "Leaf"] {
        assert_eq!(c.orbital_by_trait.get(t).map(String::as_str), Some("Main"), "{t}");
    }
}

#[test]
fn page_circuit_without_an_embed_has_no_embedded_traits() {
    let c = embed_kernel(false).page_circuit(None, None).unwrap();
    assert!(c.embedded.is_empty());
    assert_eq!(c.orbital_by_trait.len(), 1);
}

#[test]
fn page_circuit_without_page_or_route_and_no_initial_page_is_the_first_route_page() {
    let c = app().page_circuit(None, None).unwrap();
    assert_eq!(c.page.as_deref(), Some("Home"));
    assert_eq!(c.route.as_deref(), Some("/"));
    assert!(!c.not_found);
    assert_eq!(names(c.mounts), ["RouteT"]);
}

#[test]
fn page_circuit_initial_page_wins_over_the_first_page() {
    let k = kernel(
        serde_json::json!([
            { "name": "First", "path": "/", "traits": [{ "ref": "A" }] },
            { "name": "Second", "path": "/two", "isInitial": true, "traits": [{ "ref": "B" }] }
        ]),
        &["A", "B"],
    );
    let c = k.page_circuit(None, None).unwrap();
    assert_eq!(c.page.as_deref(), Some("Second"));
    assert_eq!(c.route.as_deref(), Some("/two"));
}

#[test]
fn page_circuit_first_page_is_declaration_order_not_name_order() {
    let k = kernel(
        serde_json::json!([
            { "name": "Zeta", "path": "/z", "traits": [{ "ref": "A" }] },
            { "name": "Alpha", "path": "/a", "traits": [{ "ref": "B" }] }
        ]),
        &["A", "B"],
    );
    assert_eq!(k.page_circuit(None, None).unwrap().page.as_deref(), Some("Zeta"));
}

#[test]
fn page_circuit_embedded_by_includes_a_page_listed_trait_another_page_trait_embeds() {
    let c = embed_kernel(true).page_circuit(None, None).unwrap();
    assert_eq!(c.embedded_by, ["Child", "Leaf"]);
}

#[test]
fn page_circuit_embedded_by_names_a_page_listed_trait_a_sibling_references() {
    let host = serde_json::json!({
        "name": "Host",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                "effects": [["render-ui", "main", "@trait.Child"]] }]
        }
    });
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [host, trait_json("Child")],
            "pages": [{ "name": "Home", "path": "/", "isInitial": true, "traits": [{ "ref": "Host" }, { "ref": "Child" }] }]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    k.register_schema(schema).unwrap();
    let c = k.page_circuit(None, None).unwrap();
    assert_eq!(c.embedded_by, ["Child"]);
    assert!(c.embedded.is_empty());
}
