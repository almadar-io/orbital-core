use super::parse_service_registry_data;

#[test]
fn reads_each_services_declared_runtimes_in_the_same_parse_as_its_actions() {
    let raw = r#"{"services":{"page":{"runtimes":["browser"],"actions":[{"name":"watch"}]},"llm":{"runtimes":["node","browser"]}}}"#;
    let data = parse_service_registry_data(raw).unwrap();
    assert_eq!(data.runtimes["page"], vec!["browser".to_string()]);
    assert_eq!(
        data.runtimes["llm"],
        vec!["node".to_string(), "browser".to_string()]
    );
    assert!(data.actions["page"].contains("watch"));
}

#[test]
fn edge_a_service_without_the_field_declares_no_runtime() {
    let data = parse_service_registry_data(r#"{"services":{"old":{}}}"#).unwrap();
    assert!(data.runtimes["old"].is_empty());
}
