// EXEC-LOAD-2: a request that names a `uses lazy` orbital loads it through the server's lazy source; a name no lazy page
// declares is still not found. Twin of the native host's first-request test.

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::EventRequest;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::{LazyProgram, LazySource};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Files(AtomicUsize);

impl LazySource for Files {
    fn read(&self, file: &str) -> Result<LazyProgram, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(file, "lazy/Late.orb");
        let schema = serde_json::from_value(json!({
            "name": "Late", "typeContractVersion": 1,
            "orbitals": [{ "name": "Late",
                "entity": { "name": "Memo", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Hear", "linkedEntity": "Memo",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                        "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD" }] } }] }]
        }))
        .unwrap();
        Ok(LazyProgram {
            schema,
            catalogs: Default::default(),
        })
    }
}

fn importer() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{ "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Home", "linkedEntity": "Item",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }] }],
        "lazyPages": [{ "path": "/late", "orbital": "Late", "orbRef": "lazy/Late.orb", "traits": [{ "name": "Hear", "dispatchMode": "persistedAwaited" }] }]
    }))
    .unwrap()
}

fn hear() -> EventRequest {
    EventRequest {
        event: "HEARD".to_string(),
        target_trait: Some("Hear".to_string()),
        ..Default::default()
    }
}

#[test]
fn the_first_request_naming_a_lazy_orbital_loads_it() {
    let files = Arc::new(Files(AtomicUsize::new(0)));
    let server = EmbeddedServer::with_lazy_source(importer(), Some(files.clone())).unwrap();
    match server.start("Late", hear()).unwrap() {
        ServerProgress::Done(response) => {
            assert!(response.success, "{response:?}");
            assert_eq!(
                response.states.get("Hear").map(String::as_str),
                Some("heard")
            );
        }
        _ => panic!("the request needs no host answer"),
    }
    assert_eq!(files.0.load(Ordering::SeqCst), 1);
}

#[test]
fn control_a_name_no_lazy_page_declares_is_not_found_and_reads_nothing() {
    let files = Arc::new(Files(AtomicUsize::new(0)));
    let server = EmbeddedServer::with_lazy_source(importer(), Some(files.clone())).unwrap();
    let error = server.start("Nowhere", hear()).err().expect("not found");
    assert!(error.contains("Orbital not found: Nowhere"), "{error}");
    assert_eq!(files.0.load(Ordering::SeqCst), 0);
}

#[test]
fn control_a_server_with_no_lazy_source_does_not_know_the_orbital() {
    let server = EmbeddedServer::new(importer()).unwrap();
    let error = server.start("Late", hear()).err().expect("not found");
    assert!(error.contains("Orbital not found: Late"), "{error}");
}
