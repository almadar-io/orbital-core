use super::*;
use crate::entity_source::InMemoryEntityCache;
use orbital_core_base::os_grants::{DesktopKind, FsMode, StdioKind};
use serde_json::json;
use std::path::Path;

fn schema(name: &str, grants: serde_json::Value) -> OrbitalSchema {
    serde_json::from_value(json!({ "name": name, "osGrants": grants, "typeContractVersion": 1, "orbitals": [] })).unwrap()
}

fn kernel() -> RuntimeKernel {
    RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    )
}

#[test]
fn a_kernel_with_no_grant_declaring_program_grants_nothing() {
    let k = kernel();
    k.register_schema(
        serde_json::from_value(json!({ "name": "plain", "typeContractVersion": 1, "orbitals": [] })).unwrap(),
    )
    .unwrap();
    assert!(k.os_grants().is_empty());
}

#[test]
fn the_registered_programs_grants_are_unioned_without_duplicates() {
    let k = kernel();
    k.register_schema(schema(
        "a",
        json!({ "fs": [{ "glob": "/data/**", "modes": ["read"] }], "exec": ["git"], "stdio": ["out"] }),
    ))
    .unwrap();
    k.register_schema(schema(
        "b",
        json!({ "fs": [{ "glob": "/data/**", "modes": ["read"] }, { "glob": "/tmp/**", "modes": ["write"] }],
                "exec": ["git", "ls"], "desktop": ["notify"] }),
    ))
    .unwrap();
    let grants = k.os_grants();
    assert_eq!(grants.fs.len(), 2);
    assert_eq!(grants.exec, vec!["git".to_string(), "ls".to_string()]);
    assert!(grants.fs_allows(Path::new("/data/a"), FsMode::Read));
    assert!(grants.fs_allows(Path::new("/tmp/a"), FsMode::Write));
    assert!(!grants.fs_allows(Path::new("/tmp/a"), FsMode::Read));
    assert!(grants.stdio_allows(StdioKind::Out));
    assert!(grants.desktop_allows(DesktopKind::Notify));
}

#[test]
fn an_orbitals_own_grants_reach_the_kernel_and_an_undeclared_one_does_not() {
    let with_own: OrbitalSchema = serde_json::from_value(json!({
        "name": "p", "version": "1.0.0",
        "typeContractVersion": 1, "orbitals": [{"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": [],
                      "osGrants": {"exec": ["git"]}}]
    }))
    .unwrap();
    let k = kernel();
    k.register_schema(with_own).unwrap();
    assert!(k.os_grants().exec_allows("git"));

    let without: OrbitalSchema = serde_json::from_value(json!({
        "name": "q", "version": "1.0.0",
        "typeContractVersion": 1, "orbitals": [{"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": []}]
    }))
    .unwrap();
    let control = kernel();
    control.register_schema(without).unwrap();
    assert!(!control.os_grants().exec_allows("git"));
}

#[test]
fn grants_round_trip_through_the_schema_json() {
    let s = schema("a", json!({ "env": ["HOME"] }));
    let back = serde_json::to_value(&s).unwrap();
    assert_eq!(back["osGrants"]["env"], json!(["HOME"]));
    let plain: OrbitalSchema =
        serde_json::from_value(json!({ "name": "p", "typeContractVersion": 1, "orbitals": [] })).unwrap();
    assert!(serde_json::to_value(&plain)
        .unwrap()
        .get("osGrants")
        .is_none());
}
