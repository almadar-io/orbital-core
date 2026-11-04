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

#[test]
fn a_traits_embedded_refs_are_read_once_at_registration_and_walked_from_the_registry() {
    let kernel = composer_kernel(true);
    let refs = kernel.registry.embedded_trait_refs.read();
    assert_eq!(refs.get("Host").map(|r| r.as_slice()), Some(&["Child".to_string()][..]));
    assert_eq!(refs.get("Child").map(|r| r.len()), Some(0), "control: a trait that embeds nothing has an empty entry");
    drop(refs);
    assert_eq!(kernel.trait_refs_from(&["Host".to_string()]), ["Child"]);
    assert!(kernel.trait_refs_from(&["Missing".to_string()]).is_empty(), "edge: an unregistered root refers to nothing");
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

fn relation_to(name: &str, entity: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "type": "relation", "relation": { "entity": entity, "cardinality": "one" } })
}

fn entity_json(name: &str, relations: &[(&str, &str)]) -> serde_json::Value {
    let mut fields = vec![serde_json::json!({ "name": "id", "type": "string" })];
    fields.extend(relations.iter().map(|(field, to)| relation_to(field, to)));
    serde_json::json!({ "name": name, "fields": fields })
}

fn linked_trait(name: &str, entity: &str, effects: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": name, "linkedEntity": entity,
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }]
        }
    })
}

/// One orbital per entity; the pages bind traits by name.
fn entity_kernel(orbitals: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema =
        serde_json::from_value(serde_json::json!({ "name": "App", "typeContractVersion": 1, "orbitals": orbitals })).unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

#[test]
fn page_circuit_entities_are_the_linked_entities_and_their_declared_relations_transitively() {
    let k = entity_kernel(serde_json::json!([
        { "name": "Orders", "entity": entity_json("Order", &[("customerId", "Customer")]),
          "traits": [linked_trait("OrderList", "Order", serde_json::json!([]))],
          "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "OrderList" }] }] },
        { "name": "Customers", "entity": entity_json("Customer", &[("regionId", "Region")]), "traits": [] },
        { "name": "Regions", "entity": entity_json("Region", &[]), "traits": [] },
        { "name": "Unrelated", "entity": entity_json("Audit", &[]), "traits": [] }
    ]));
    let c = k.page_circuit(None, Some("/")).unwrap();
    assert_eq!(c.entities, ["Customer", "Order", "Region"]);
    assert_eq!(c.traits, ["OrderList"]);
}

#[test]
fn page_circuit_entities_include_the_embedded_traits_entities() {
    let host = serde_json::json!({
        "name": "Host", "linkedEntity": "Order",
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                "effects": [["render-ui", "main", { "type": "stack", "children": ["@trait.Child"] }]] }] }
    });
    let k = entity_kernel(serde_json::json!([
        { "name": "Orders", "entity": entity_json("Order", &[]), "traits": [host, linked_trait("Child", "Note", serde_json::json!([]))],
          "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Host" }] }] },
        { "name": "Notes", "entity": entity_json("Note", &[]), "traits": [] }
    ]));
    let c = k.page_circuit(None, Some("/")).unwrap();
    assert_eq!(c.embedded, ["Child"]);
    assert_eq!(c.traits, ["Host", "Child"]);
    assert_eq!(c.entities, ["Note", "Order"]);
}

#[test]
fn edge_a_relation_cycle_ends_where_it_closes_and_a_self_relation_is_listed_once() {
    let k = entity_kernel(serde_json::json!([
        { "name": "Teams", "entity": entity_json("Team", &[("leadId", "User"), ("parentId", "Team")]),
          "traits": [linked_trait("TeamList", "Team", serde_json::json!([]))],
          "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "TeamList" }] }] },
        { "name": "Users", "entity": entity_json("User", &[("teamId", "Team")]), "traits": [] }
    ]));
    assert_eq!(k.page_circuit(None, Some("/")).unwrap().entities, ["Team", "User"]);
}

#[test]
fn control_an_entity_only_a_fetch_effect_names_is_not_a_page_entity() {
    let k = entity_kernel(serde_json::json!([
        { "name": "Orders", "entity": entity_json("Order", &[]),
          "traits": [linked_trait("OrderList", "Order", serde_json::json!([["fetch", "Audit"]]))],
          "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "OrderList" }] }] },
        { "name": "Audits", "entity": entity_json("Audit", &[]), "traits": [] }
    ]));
    assert_eq!(k.page_circuit(None, Some("/")).unwrap().entities, ["Order"]);
}

#[test]
fn control_a_page_with_no_match_has_no_traits_or_entities() {
    let c = app().page_circuit(None, Some("/nope")).unwrap();
    assert!(c.traits.is_empty() && c.entities.is_empty());
}

/// Boot listens for `$MOUNT` and persists a Note when the page and route it announces are the ones asked for;
/// Reader is a route trait whose INIT runs after it.
fn mount_listener_kernel() -> RuntimeKernel {
    let boot = serde_json::json!({
        "name": "Boot", "linkedEntity": "Note",
        "listens": [{ "event": "$MOUNT", "triggers": "SEED_PAGE" }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "seeded" }],
            "transitions": [{
                "from": "idle", "to": "seeded", "event": "SEED_PAGE",
                "guard": ["and", ["=", "@payload.page", "Home"], ["=", "@payload.route", "/"]],
                "effects": [["persist", "create", "Note", {}]]
            }]
        }
    });
    let bystander = serde_json::json!({
        "name": "Bystander", "linkedEntity": "Note",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "SEED_PAGE" }]
        }
    });
    entity_kernel(serde_json::json!([{
        "name": "Main", "entity": entity_json("Note", &[]),
        "traits": [boot, bystander, linked_trait("Reader", "Note", serde_json::json!([["fetch", "Note"]]))],
        "pages": [
            { "name": "Home", "path": "/", "traits": [{ "ref": "Reader" }] },
            { "name": "Boot", "path": "", "protocol": "boot", "traits": [{ "ref": "Boot" }, { "ref": "Bystander" }] }
        ]
    }]))
}

fn ran(outcomes: &[(String, ProcessEventResult)]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, r)| format!("{t}:{}", r.event.clone().unwrap_or_default()))
        .collect()
}

#[test]
fn a_route_mount_delivers_mount_with_the_page_payload_to_the_listening_trait_before_the_seeds() {
    let k = mount_listener_kernel();
    let outcomes = k.dispatch_route_mount(Some("/"), &k.route_mounts("/"), None, Value::Null).unwrap();
    assert_eq!(ran(&outcomes), ["Boot:SEED_PAGE", "Reader:INIT"], "the listener settles before the route's own INIT");
    assert_eq!(k.get_state("Boot", None).map(|s| s.current_state).as_deref(), Some("seeded"), "the payload named page and route");
}

#[test]
fn control_a_trait_without_the_listen_gets_nothing_and_a_batch_naming_no_route_delivers_no_mount() {
    let k = mount_listener_kernel();
    let outcomes = k.dispatch_route_mount(Some("/"), &k.route_mounts("/"), None, Value::Null).unwrap();
    assert!(!outcomes.iter().any(|(t, _)| t == "Bystander"));
    let k = mount_listener_kernel();
    let outcomes = k.dispatch_mount(&k.route_mounts("/"), None, Value::Null).unwrap();
    assert_eq!(ran(&outcomes), ["Reader:INIT"]);
    assert_eq!(k.get_state("Boot", None).map(|s| s.current_state), None, "Boot was never touched");
}

#[test]
fn edge_a_route_no_page_matches_delivers_no_mount() {
    let k = mount_listener_kernel();
    let outcomes = k.dispatch_route_mount(Some("/nope"), &k.route_mounts("/"), None, Value::Null).unwrap();
    assert_eq!(ran(&outcomes), ["Reader:INIT"]);
}
