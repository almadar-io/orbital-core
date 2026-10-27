//! A schema registered straight into the kernel (no compiler resolve): the
//! host orbital's entity never decides a trait's dispatch mode.
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel_with(entity_persistence: &str, linked: Option<&str>) -> RuntimeKernel {
    let mut t = serde_json::json!({
        "name": "Pure",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["set", "@entity.n", 1]] }]
        }
    });
    if let Some(e) = linked {
        t["linkedEntity"] = serde_json::json!(e);
    }
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Host",
        "orbitals": [
            { "name": "Frames",
              "entity": { "name": "Frame", "persistence": entity_persistence,
                          "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
              "traits": [t] },
            { "name": "Rows",
              "entity": { "name": "Row", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [] }
        ]
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

fn mode(kernel: &RuntimeKernel) -> DispatchMode {
    kernel.trait_snapshot(None)["Pure"].1
}

#[test]
fn an_undeclared_trait_on_a_runtime_orbital_awaits_the_server() {
    assert_eq!(
        mode(&kernel_with("runtime", None)),
        DispatchMode::PersistedAwaited
    );
}

#[test]
fn a_client_trait_on_a_persisted_orbital_is_client_only() {
    let kernel = kernel_with("persistent", None);
    let mut def = orbital_core_base::schema::TraitDefinition::clone(&kernel.registry.trait_definitions.read()["Pure"]);
    def.dispatch = Some(orbital_core_base::schema::types::DispatchMode::Client);
    assert_eq!(kernel.dispatch_mode_of(&def), DispatchMode::HybridClientOnly);
}

#[test]
fn an_unlinked_trait_on_a_persisted_orbital_awaits_the_server() {
    assert_eq!(
        mode(&kernel_with("persistent", None)),
        DispatchMode::PersistedAwaited
    );
}

#[test]
fn a_declared_binding_to_another_orbital_wins() {
    assert_eq!(
        mode(&kernel_with("runtime", Some("Row"))),
        DispatchMode::PersistedAwaited
    );
}
