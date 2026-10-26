use super::*;

/// A trait fixture carrying only what `EmbedIndex::build` reads: its own
/// `id` and (optionally) the embed-token -> child-id map a host declares.
fn trait_def(name: &str, id: &str, embeds: &[(&str, &str)]) -> TraitDefinition {
    let mut json = serde_json::json!({
        "name": name,
        "id": id,
        "stateMachine": { "states": [], "transitions": [] },
    });
    if !embeds.is_empty() {
        let map: serde_json::Map<String, serde_json::Value> = embeds
            .iter()
            .map(|(token, child_id)| {
                (
                    (*token).to_string(),
                    serde_json::Value::String((*child_id).to_string()),
                )
            })
            .collect();
        json["traitEmbedIds"] = serde_json::Value::Object(map);
    }
    serde_json::from_value(json).expect("trait fixture")
}

fn defs(list: Vec<TraitDefinition>) -> HashMap<String, TraitDefinition> {
    list.into_iter().map(|t| (t.name.clone(), t)).collect()
}

#[test]
fn embed_index_inverts_trait_embed_ids() {
    let child = trait_def("Child", "child-1", &[]);
    let host = trait_def("Host", "host-1", &[("slot", "child-1")]);
    let idx = EmbedIndex::build(&defs(vec![child, host]));
    assert_eq!(idx.ancestors("Child"), &["Host".to_string()]);
    assert!(idx.ancestors("Host").is_empty());
    assert!(idx.ancestors("Unknown").is_empty());
}

#[test]
fn embed_index_transitive_chain_innermost_first() {
    let child = trait_def("Child", "child-1", &[]);
    let host = trait_def("Host", "host-1", &[("slot", "child-1")]);
    let grandhost = trait_def("Grandhost", "grand-1", &[("slot2", "host-1")]);
    let idx = EmbedIndex::build(&defs(vec![child, host, grandhost]));
    assert_eq!(
        idx.ancestors("Child"),
        &["Host".to_string(), "Grandhost".to_string()],
        "innermost host first, outward to the root"
    );
    assert_eq!(idx.ancestors("Host"), &["Grandhost".to_string()]);
}

/// A embeds B, B embeds A — malformed input no authoring path produces, but
/// the walk must terminate defensively rather than loop forever. Simply
/// returning (rather than hanging the test run) is the assertion.
#[test]
fn embed_index_cycle_guard_terminates() {
    let a = trait_def("A", "a-1", &[("slot", "b-1")]);
    let b = trait_def("B", "b-1", &[("slot", "a-1")]);
    let idx = EmbedIndex::build(&defs(vec![a, b]));
    assert!(idx.ancestors("A").len() <= 2);
    assert!(idx.ancestors("B").len() <= 2);
}
