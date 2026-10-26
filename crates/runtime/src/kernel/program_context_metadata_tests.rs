use super::*;
use serde_json::json;

#[test]
fn program_context_and_its_session_fork_exclude_app_credential_catalog_and_schemas() {
    let schema = |name: &str| -> OrbitalSchema {
        serde_json::from_value(json!({"name":name,"orbitals":[{"name":name,"entity":{"name":name,"persistence":"runtime","fields":[]},"traits":[{
            "name":name,"config":{"apiKey":{"type":"secret","default":format!("{name}_CREDENTIAL")}},
            "stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[{"from":"idle","to":"idle","event":"CHECK","effects":[["credential-status","@config.apiKey",{"emit":{"success":"OK"}}]]}]}
        }]}]})).unwrap()
    };
    let source =
        || Arc::new(crate::entity_source::InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
    let parent = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        source(),
        KernelConfig::default(),
    );
    parent.register_schema(schema("App")).unwrap();
    parent.set_managed_program(&serde_json::to_value(schema("Managed")).unwrap());
    assert_eq!(parent.program_credentials().unwrap().len(), 2);
    let child = parent
        .program_context(source(), KernelConfig::default(), schema("Control"))
        .unwrap();
    let credentials = child.program_credentials().unwrap();
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].env_var, "Control_CREDENTIAL");
    assert!(!child.schemas.read().contains_key("App"));
    assert!(child.managed_credentials.read().is_empty());
    assert!(child.entity_policies.read().contains_key("App"));
    let fork = child
        .fork_session(source(), KernelConfig::default())
        .unwrap();
    let credentials = fork.program_credentials().unwrap();
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].env_var, "Control_CREDENTIAL");
    assert!(fork.entity_policies.read().contains_key("App"));
    assert!(!fork.schemas.read().contains_key("App"));
    assert_eq!(parent.program_credentials().unwrap().len(), 2);
}
