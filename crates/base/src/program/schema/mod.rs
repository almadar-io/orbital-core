//! Schema types for Orbital
//!
//! These types mirror the TypeScript OrbitalSchema and can deserialize `.orb` files.

pub mod embed_tokens;
pub mod format;
pub mod fingerprint;
pub use fingerprint::{capture_trait_source_declaration, trait_execution_fingerprint};
pub mod behavior_value;
pub mod quote;
pub mod sexpr;
pub mod types;

pub use embed_tokens::{
    collect_state_machine_trait_tokens, collect_trait_tokens, trait_embed_children,
};
pub use format::format_sexpr;
pub use sexpr::{SObject, 
    json_value_leaves, json_value_leaves_mut, MappedChildren, OperatorLookup, OperatorReturn,
    RewriteVisit, SExpression, SequentialBindings, ValueSlot, ValueWalk,
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
                    for p in td.propositions_mut() {
                        p.normalize_fn_shapes();
                    }
                    for tr in &mut td.state_machine.transitions {
                        if let Some(g) = tr.guard.as_mut() {
                            g.normalize_fn_shapes();
                        }
                        if let Some(effs) = tr.effects.as_mut() {
                            for e in effs.iter_mut() {
                                e.normalize_fn_shapes();
                            }
                        }
                    }
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
}
