//! Schema types for Orbital
//!
//! These types mirror the TypeScript OrbitalSchema and can deserialize `.orb` files.

pub mod avl_projection;
pub mod embed_tokens;
pub mod fingerprint;
pub mod format;
pub use fingerprint::{capture_trait_source_declaration, trait_execution_fingerprint};
pub mod behavior_value;
pub mod quote;
pub mod sexpr;
pub mod types;

pub use embed_tokens::{
    collect_state_machine_trait_tokens, collect_trait_tokens, trait_embed_children,
};
pub use format::format_sexpr;
pub use sexpr::{
    json_value_leaves, json_value_leaves_mut, MappedChildren, OperatorLookup, OperatorReturn,
    RewriteVisit, SExpression, SObject, SequentialBindings, ValueSlot, ValueSourceSlots, ValueWalk,
};
pub use types::*;

use crate::error::Result;
use std::sync::Arc;

/// Parse an OrbitalSchema from a JSON string.
///
/// Wraps the deserialize in `serde_path_to_error` so a failure names the JSON
/// path of the offending node (e.g. `orbitals[0].traits[3].listens[0].source`)
/// instead of a bare line/column. Combined with `TraitReference`'s tag-dispatched
/// deserialize, the message names both the path AND the offending field.
pub fn parse_schema(json: &str) -> Result<OrbitalSchema> {
    let mut de = serde_json::Deserializer::from_str(json);
    let schema: OrbitalSchema = serde_path_to_error::deserialize(&mut de)?;
    Ok(schema)
}

/// The declared transition at `index` in one resolved trait's machine.
pub fn transition_at<'a>(
    schema: &'a OrbitalSchema,
    orbital_name: &str,
    trait_name: &str,
    index: usize,
) -> Option<&'a TransitionDefinition> {
    trait_machine(schema, orbital_name, trait_name)?
        .transitions
        .get(index)
}

/// The resolved state machine for a named trait in a named orbital.
pub fn trait_machine<'a>(
    schema: &'a OrbitalSchema,
    orbital_name: &str,
    trait_name: &str,
) -> Option<&'a StateMachine> {
    let orbital = schema.orbitals.iter().find(|o| o.name == orbital_name)?;
    let trait_ref = orbital
        .traits
        .iter()
        .find(|t| t.trait_name() == Some(trait_name))?;
    let TraitReference::Inline(trait_def) = trait_ref else {
        return None;
    };
    Some(&trait_def.state_machine)
}

/// Inline trait machines in program order, preserving their owning orbital.
pub fn program_machines(schema: &OrbitalSchema) -> Vec<(&str, &TraitDefinition)> {
    schema
        .orbitals
        .iter()
        .flat_map(|orbital| {
            orbital.traits.iter().filter_map(move |trait_ref| {
                let TraitReference::Inline(trait_def) = trait_ref else {
                    return None;
                };
                Some((orbital.name.as_str(), trait_def.as_ref()))
            })
        })
        .collect()
}

/// Zero-based position of a transition among the same `from --event-->` arms.
pub fn transition_arm_position(machine: &StateMachine, index: usize) -> Option<usize> {
    let selected = machine.transitions.get(index)?;
    Some(
        machine.transitions[..index]
            .iter()
            .filter(|t| t.from == selected.from && t.event == selected.event)
            .count(),
    )
}

/// Parse an OrbitalSchema from a file path
#[cfg(feature = "std")]
pub fn parse_schema_file(path: &std::path::Path) -> Result<OrbitalSchema> {
    let content = std::fs::read_to_string(path)?;
    parse_schema(&content)
}

/// Canonicalize every flat `fn` lambda in the schema to the grouped form
/// (see [`SExpression::normalize_fn_shapes`]). Walks the SExpr-bearing
/// positions: inline-trait transition guards + effects and tick guards +
/// effects, reference-trait `config` values, and entity-field defaults
/// (recursing into array/object field shapes). Idempotent.
///
/// Called from the lolo lowerer (so the emitted/baked `.orb` is canonical) and
/// post-inline in the compiler (so raw `.orb` input is canonicalized before
/// validation + codegen).
pub fn normalize_fn_shapes(schema: &mut OrbitalSchema) {
    for orbital in &mut schema.orbitals {
        normalize_entity_ref_defaults(&mut orbital.entity);
        for tref in &mut orbital.traits {
            match tref {
                TraitReference::Inline(td) => {
                    let td = Arc::make_mut(td);
                    td.state_machine.edit(|machine| {
                        for p in machine.propositions_mut() {
                            p.normalize_fn_shapes();
                        }
                        for tr in &mut machine.transitions {
                            if let Some(g) = tr.guard.as_mut() {
                                g.normalize_fn_shapes();
                            }
                            if let Some(effs) = tr.effects.as_mut() {
                                for e in effs.iter_mut() {
                                    e.normalize_fn_shapes();
                                }
                            }
                        }
                    });
                    if let Some(ticks) = td.ticks.as_mut() {
                        for tk in ticks.iter_mut() {
                            if let Some(g) = tk.guard.as_mut() {
                                g.normalize_fn_shapes();
                            }
                            for e in tk.effects.iter_mut() {
                                e.normalize_fn_shapes();
                            }
                        }
                    }
                }
                TraitReference::Reference { config, .. } => {
                    if let Some(cfg) = config.as_mut() {
                        for field in cfg.values_mut() {
                            if let Some(default) = field.default.as_mut() {
                                default.normalize_fn_shapes();
                            }
                        }
                    }
                }
            }
        }
    }
}

fn normalize_entity_ref_defaults(eref: &mut EntityRef) {
    match eref {
        EntityRef::Inline(ed) => {
            for f in &mut ed.fields {
                normalize_field_default(f);
            }
        }
        EntityRef::Extends(ere) => {
            if let Some(fields) = ere.fields.as_mut() {
                for f in fields.iter_mut() {
                    normalize_field_default(f);
                }
            }
        }
        EntityRef::Reference(_) => {}
    }
}

fn normalize_field_default(f: &mut FieldDefinition) {
    if let Some(d) = f.default.as_mut() {
        d.normalize_fn_shapes();
    }
    if let Some(items) = f.items.as_mut() {
        normalize_field_default(items);
    }
    if let Some(props) = f.properties.as_mut() {
        for p in props.values_mut() {
            normalize_field_default(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_schema() {
        let json = r#"{
            "name": "Test App",
            "version": "1.0.0",
            "orbitals": []
        }"#;

        let schema = parse_schema(json).unwrap();
        assert_eq!(schema.name, "Test App");
        assert_eq!(schema.version, Some("1.0.0".to_string()));
        assert!(schema.orbitals.is_empty());
    }

    #[test]
    fn transition_arm_position_uses_declaration_order() {
        let machine: StateMachine = serde_json::from_value(serde_json::json!({
            "states": [{"name": "idle"}, {"name": "ready"}],
            "transitions": [
                {"from": "idle", "to": "ready", "event": "GO"},
                {"from": "idle", "to": "idle", "event": "WAIT"},
                {"from": "idle", "to": "idle", "event": "GO"},
                {"from": "ready", "to": "idle", "event": "GO"}
            ]
        }))
        .unwrap();
        assert_eq!(transition_arm_position(&machine, 0), Some(0));
        assert_eq!(transition_arm_position(&machine, 1), Some(0));
        assert_eq!(transition_arm_position(&machine, 2), Some(1));
        assert_eq!(transition_arm_position(&machine, 3), Some(0));
        assert_eq!(transition_arm_position(&machine, 4), None);
    }

    #[test]
    fn transition_at_reads_one_resolved_trait() {
        let schema = parse_schema(
            r#"{
            "name": "Panel",
            "orbitals": [{
                "name": "DoorOrbital",
                "entity": {"name": "Door", "fields": []},
                "traits": [{
                    "name": "DoorLife",
                    "stateMachine": {
                        "states": [{"name": "closed"}, {"name": "open"}],
                        "transitions": [{"from": "closed", "to": "open", "event": "OPEN"}]
                    }
                }]
            }]
        }"#,
        )
        .unwrap();
        assert_eq!(
            transition_at(&schema, "DoorOrbital", "DoorLife", 0).map(|t| t.event.as_str()),
            Some("OPEN")
        );
        assert!(transition_at(&schema, "DoorOrbital", "DoorLife", 1).is_none());
        assert!(transition_at(&schema, "DoorOrbital", "Missing", 0).is_none());
        assert_eq!(
            program_machines(&schema)
                .iter()
                .map(|(_, trait_def)| trait_def.name.as_str())
                .collect::<Vec<_>>(),
            vec!["DoorLife"]
        );
    }
}
