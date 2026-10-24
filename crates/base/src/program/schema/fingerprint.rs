use super::TraitDefinition;

pub fn trait_execution_fingerprint(
    definition: &TraitDefinition,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(definition)?;
    if let Some(fields) = value.as_object_mut() {
        for field in [
            "id",
            "name",
            "description",
            "sourceBehavior",
            "sourceDeclaration",
            "sourceEntityDefinition",
            "entityRebindable",
            "entityContract",
            "effectRow",
        ] {
            fields.remove(field);
        }
    }
    value.sort_all_objects();
    Ok(crate::codec::hex(&crate::codec::sha256(
        &serde_json::to_vec(&value)?,
    )))
}

pub fn capture_trait_source_declaration(
    definition: &mut TraitDefinition,
) -> Result<(), serde_json::Error> {
    if definition.source_declaration.is_none() {
        if let Some(id) = definition.id.clone() {
            definition.source_declaration = Some(super::SourceDeclarationMetadata {
                trait_id: id,
                fingerprint: trait_execution_fingerprint(definition)?,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> TraitDefinition {
        serde_json::from_value(json!({
            "name":"Flow", "id":"decl-1", "linkedEntity":"Row",
            "config":{"text":{"type":"string","default":"hello"}},
            "stateMachine":{
                "states":[{"name":"a","isInitial":true},{"name":"b"}],
                "transitions":[
                    {"from":"a","to":"b","event":"NEXT","effects":[["set","@entity.value",1]]},
                    {"from":"b","to":"a","event":"RESET"}
                ]
            }
        }))
        .unwrap()
    }

    #[test]
    fn execution_fingerprint_tracks_order_bodies_and_config() {
        let baseline = definition();
        let fingerprint = trait_execution_fingerprint(&baseline).unwrap();
        let mut reordered = definition();
        reordered.state_machine.transitions.reverse();
        assert_ne!(
            fingerprint,
            trait_execution_fingerprint(&reordered).unwrap()
        );
        let mut edited = definition();
        edited.state_machine.transitions[0].effects = Some(vec![serde_json::from_value(json!([
            "set",
            "@entity.value",
            2
        ]))
        .unwrap()]);
        assert_ne!(fingerprint, trait_execution_fingerprint(&edited).unwrap());
        let mut configured = definition();
        configured
            .config
            .as_mut()
            .unwrap()
            .get_mut("text")
            .unwrap()
            .default = Some(super::super::SExpression::String("changed".into()));
        assert_ne!(
            fingerprint,
            trait_execution_fingerprint(&configured).unwrap()
        );
    }

    #[test]
    fn execution_fingerprint_ignores_declaration_labels_and_object_order() {
        let baseline = definition();
        let mut renamed = definition();
        renamed.id = Some("decl-2".into());
        renamed.name = "Renamed".into();
        renamed.description = Some("calm editor label".into());
        assert_eq!(
            trait_execution_fingerprint(&baseline).unwrap(),
            trait_execution_fingerprint(&renamed).unwrap()
        );
        let mut first = serde_json::to_value(definition()).unwrap();
        let mut second = first.clone();
        first["config"]["extra"] = json!({"type":"object","default":{"a":1,"b":2}});
        second["config"]["extra"] = json!({"default":{"b":2,"a":1},"type":"object"});
        assert_eq!(
            trait_execution_fingerprint(&serde_json::from_value(first).unwrap()).unwrap(),
            trait_execution_fingerprint(&serde_json::from_value(second).unwrap()).unwrap()
        );
    }
}
