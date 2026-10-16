//! Which traits a trait embeds: the `@trait.<Name>` tokens in its own body.
//!
//! One walker for the L1 button-reachability check (`orbital-lolo`) and the L2 `EmbedGraph`
//! (`orbital-compiler`), so both read "A embeds B" from the same tokens.

use crate::schema::sexpr::SExpression;
use crate::schema::types::TraitDefinition;
use std::collections::HashSet;

/// Collect every `@trait.<Name>` token appearing anywhere inside `expr`.
pub fn collect_trait_tokens(expr: &SExpression, out: &mut HashSet<String>) {
    match expr {
        SExpression::String(s) => {
            if let Some(rest) = s.strip_prefix("@trait.") {
                let end = rest.find(['.', '[']).unwrap_or(rest.len());
                if end > 0 {
                    out.insert(rest[..end].to_string());
                }
            }
        }
        SExpression::Array(items) => {
            for item in items {
                collect_trait_tokens(item, out);
            }
        }
        SExpression::Object(map) => {
            for v in map.values() {
                collect_trait_tokens(v, out);
            }
        }
        _ => {}
    }
}

/// The `@trait.<Name>` tokens in a trait's state machine: every transition's guard and effects
/// and every tick's guard and effects.
pub fn collect_state_machine_trait_tokens(td: &TraitDefinition, out: &mut HashSet<String>) {
    for transition in &td.state_machine.transitions {
        if let Some(guard) = transition.guard.as_ref() {
            collect_trait_tokens(guard, out);
        }
        for effect in transition.effects.iter().flatten() {
            collect_trait_tokens(effect, out);
        }
    }
    for tick in td.ticks.iter().flatten() {
        if let Some(guard) = tick.guard.as_ref() {
            collect_trait_tokens(guard, out);
        }
        for effect in &tick.effects {
            collect_trait_tokens(effect, out);
        }
    }
}

/// Every trait name an inline trait embeds: config defaults plus state machine.
pub fn trait_embed_children(td: &TraitDefinition) -> HashSet<String> {
    let mut children = HashSet::new();
    for field in td.config.iter().flat_map(|c| c.values()) {
        if let Some(default) = field.default.as_ref() {
            collect_trait_tokens(default, &mut children);
        }
    }
    collect_state_machine_trait_tokens(td, &mut children);
    children.remove(&td.name);
    children
}
