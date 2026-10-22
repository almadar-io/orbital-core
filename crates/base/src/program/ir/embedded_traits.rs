//! Embedded-trait discovery shared by every shell.
//!
//! A trait referenced via `@trait.X` by a sibling's `render-ui` only paints
//! inside the sibling's frame, so it never owns a page slot on its own.
//! Mirrors `collectEmbeddedTraits` in `@almadar/runtime/ui`.

use crate::ir::effects::OirEffect;
use crate::ir::expr::OirExpr;
use crate::ir::types::{OirModule, OirPattern, OirValue};
use std::collections::HashSet;

/// Every `render-ui` pattern reachable from a transition's effects,
/// including ones nested inside `(if …)` branches. A top-level-only scan
/// misses anything wrapped in `OirExpr::Conditional`.
pub fn render_ui_patterns(effects: &[OirExpr]) -> Vec<&OirPattern> {
    fn walk<'a>(expr: &'a OirExpr, out: &mut Vec<&'a OirPattern>) {
        match expr {
            OirExpr::Effect { effect } => {
                if let OirEffect::RenderUi { pattern, .. } = effect.as_ref() {
                    out.push(pattern);
                }
            }
            OirExpr::Conditional {
                then_branch,
                else_branch,
                ..
            } => {
                walk(then_branch, out);
                walk(else_branch, out);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for effect in effects {
        walk(effect, &mut out);
    }
    out
}

/// The bare trait token of an `@trait.<PascalName>` string, else `None`.
/// Multi-segment forms (`@trait.X.anything`) intentionally do not match.
pub fn parse_trait_embed_token(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("@trait.")?;
    if rest.is_empty() || rest.contains('.') {
        return None;
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(rest)
}

/// Recursively collect trait names from every `@trait.<Name>` string in a
/// pattern's config tree. `resolve` maps a string to a trait name (callers
/// that track renamed embed tokens pass their own; see [`build_embedded_trait_set`]).
pub fn collect_trait_refs_from_pattern(
    pattern: &OirPattern,
    resolve: &dyn Fn(&str) -> Option<String>,
    out: &mut HashSet<String>,
) {
    collect_trait_refs_from_config(&pattern.config, resolve, out);
}

pub fn collect_trait_refs_from_config(
    config: &super::types::OirObject,
    resolve: &dyn Fn(&str) -> Option<String>,
    out: &mut HashSet<String>,
) {
    for value in config.values() {
        collect_trait_refs_from_value(value, resolve, out);
    }
}

pub fn collect_trait_refs_from_value(
    value: &OirValue,
    resolve: &dyn Fn(&str) -> Option<String>,
    out: &mut HashSet<String>,
) {
    match value {
        OirValue::String(s) => {
            if let Some(name) = resolve(s) {
                out.insert(name);
            }
        }
        OirValue::Array(items) => {
            for item in items {
                collect_trait_refs_from_value(item, resolve, out);
            }
        }
        OirValue::Object(obj) => collect_trait_refs_from_config(obj, resolve, out),
        _ => {}
    }
}

/// The flat set of trait names referenced via `@trait.X` by some render-ui
/// pattern in the module. A trait in this set renders only at the
/// `@trait.X` site of its host, never as a standalone slot writer.
pub fn build_embedded_trait_set(module: &OirModule) -> HashSet<String> {
    let resolve = |s: &str| parse_trait_embed_token(s).map(str::to_string);
    let mut out = HashSet::new();
    for trait_def in &module.traits {
        for trans in &trait_def.transitions {
            for pattern in render_ui_patterns(&trans.effects) {
                collect_trait_refs_from_pattern(pattern, &resolve, &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{OirState, OirTrait, OirTransition};

    fn host(name: &str, children: &[&str], conditional: bool) -> OirTrait {
        let mut pattern = OirPattern::new("stack");
        pattern.config.insert(
            "children".to_string(),
            OirValue::Array(
                children
                    .iter()
                    .map(|c| OirValue::String(format!("@trait.{c}")))
                    .collect(),
            ),
        );
        let render = OirExpr::effect(OirEffect::RenderUi {
            slot: "main".to_string(),
            pattern,
        });
        let mut t = OirTrait::new(name);
        t.states.push(OirState::initial("idle"));
        let mut tr = OirTransition::new("idle", "idle", "INIT");
        if conditional {
            tr.effects.push(OirExpr::Conditional {
                condition: Box::new(OirExpr::bool(true)),
                then_branch: Box::new(render),
                else_branch: Box::new(OirExpr::null()),
            });
        } else {
            tr.effects.push(render);
        }
        t.transitions.push(tr);
        t
    }

    #[test]
    fn referenced_trait_is_embedded_and_unreferenced_peers_are_not() {
        let mut module = OirModule::new("T", "1.0.0");
        module.traits.push(host("Layout", &["Browse"], false));
        module.traits.push(host("Browse", &[], false));
        module.traits.push(host("PeerA", &[], false));
        let set = build_embedded_trait_set(&module);
        assert!(set.contains("Browse"));
        assert!(!set.contains("Layout"));
        assert!(!set.contains("PeerA"));
    }

    #[test]
    fn reference_inside_a_conditional_branch_counts() {
        let mut module = OirModule::new("T", "1.0.0");
        module.traits.push(host("Layout", &["Browse"], true));
        assert!(build_embedded_trait_set(&module).contains("Browse"));
    }

    #[test]
    fn no_references_yields_an_empty_set() {
        let mut module = OirModule::new("T", "1.0.0");
        module.traits.push(host("PeerA", &[], false));
        module.traits.push(host("PeerB", &[], false));
        assert!(build_embedded_trait_set(&module).is_empty());
    }

    #[test]
    fn malformed_tokens_do_not_match() {
        for bad in [
            "@trait.x",
            "@trait.A.b",
            "@trait.",
            "@entity.A",
            "A",
            "@trait.1A",
        ] {
            assert!(parse_trait_embed_token(bad).is_none(), "{bad}");
        }
        assert_eq!(parse_trait_embed_token("@trait.NoteList"), Some("NoteList"));
    }
}
