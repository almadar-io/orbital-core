use super::*;

fn kernel_with_program() -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(crate::entity_source::InMemoryEntityCache::new()),
        KernelConfig::default(),
    );
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Notes",
        "typeContractVersion": 1, "orbitals": [{
            "name": "NotesOrbital",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "NoteView", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "events": [{ "key": "INIT", "name": "INIT" }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [] }]
                }
            }],
            "pages": []
        }]
    }))
    .expect("schema");
    kernel.register_schema(schema).expect("registers");
    kernel
}

fn fork_of(kernel: &RuntimeKernel) -> RuntimeKernel {
    kernel
        .fork_session(
            Arc::new(crate::entity_source::InMemoryEntityCache::new()),
            KernelConfig::default(),
        )
        .expect("forks")
}

#[test]
fn a_fork_shares_the_registered_program() {
    let template = kernel_with_program();
    let fork = fork_of(&template);
    let (ours, theirs) = (template.registry.schemas.read(), fork.registry.schemas.read());
    assert!(
        Arc::ptr_eq(&ours["Notes"], &theirs["Notes"]),
        "the fork must share the program, not copy it"
    );
}

#[test]
fn a_fork_shares_the_registered_trait_definitions() {
    let template = kernel_with_program();
    let fork = fork_of(&template);
    let ours = template.registry.trait_definitions.read()["NoteView"].clone();
    let theirs = fork
        .state_manager
        .read()
        .shared_trait("NoteView")
        .expect("the fork knows the trait");
    assert!(
        Arc::ptr_eq(&ours, &theirs),
        "the fork's state machine must share the definition, not copy it"
    );
}

#[test]
fn control_a_program_registered_on_a_fork_stays_on_that_fork() {
    let template = kernel_with_program();
    let fork = fork_of(&template);
    let mut other = (*fork.registry.schemas.read()["Notes"]).clone();
    other.name = "Other".to_string();
    fork.register_schema(other).expect("registers on the fork");
    assert!(fork.registry.schemas.read().contains_key("Other"));
    assert!(!template.registry.schemas.read().contains_key("Other"));
}

#[test]
fn a_registered_trait_is_one_definition_shared_by_the_state_manager_and_the_registry() {
    let kernel = kernel_with_program();
    let registry = kernel.registry.trait_definitions.read()["NoteView"].clone();
    let state = kernel.state_manager.read().shared_trait("NoteView").expect("the state manager knows the trait");
    assert!(Arc::ptr_eq(&registry, &state), "registration must hold one definition, not a copy per table");
}

#[test]
fn control_two_kernels_registering_the_same_program_hold_their_own_definitions() {
    let (first, second) = (kernel_with_program(), kernel_with_program());
    let ours = first.registry.trait_definitions.read()["NoteView"].clone();
    let theirs = second.registry.trait_definitions.read()["NoteView"].clone();
    assert!(!Arc::ptr_eq(&ours, &theirs));
}
