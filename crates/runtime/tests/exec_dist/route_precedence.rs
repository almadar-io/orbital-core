// EXEC-ENDPOINT-3: a route resolves to its most specific page, a literal segment beating a `:param` (twin of the web shell's `comparePathSpecificity`).

use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::json;

fn kernel() -> orbital_core_runtime::embed::EmbeddedKernel {
    let view = |name: &str| json!({ "name": name, "linkedEntity": "Note", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": name }]] }] } });
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "routes", "typeContractVersion": 1,
        "orbitals": [{ "name": "Notes", "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [view("NoteDoc"), view("NoteTree")],
            "pages": [
                { "name": "NoteDetailPage", "path": "/notes/:id", "traits": [{ "ref": "NoteDoc" }] },
                { "name": "NoteTreePage", "path": "/notes/tree", "traits": [{ "ref": "NoteTree" }] }
            ] }]
    }))
    .unwrap();
    orbital_core_runtime::embed::EmbeddedKernel::new(schema, "c1").unwrap()
}

fn mounted(route: &str) -> (Option<String>, Vec<String>) {
    let circuit = kernel().page_circuit(None, Some(route)).unwrap();
    (circuit.page, circuit.mounts.iter().map(|s| s.r#trait.clone()).collect())
}

#[test]
fn a_literal_route_opens_its_own_page_though_a_param_sibling_is_declared_first() {
    assert_eq!(mounted("/notes/tree"), (Some("NoteTreePage".to_string()), vec!["NoteTree".to_string()]));
}

#[test]
fn control_any_other_segment_still_opens_the_param_page() {
    assert_eq!(mounted("/notes/n-4"), (Some("NoteDetailPage".to_string()), vec!["NoteDoc".to_string()]));
}
