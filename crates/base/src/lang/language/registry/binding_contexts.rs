//! Which sigils each binding context allows.

use super::*;

// ============================================================================
// 3.4 — Sigil context rules (BINDING_CONTEXT_RULES)
// ============================================================================

#[derive(serde::Deserialize)]
pub struct BindingContextRule {
    pub(super) allowed: Vec<String>,
}

/// `BINDING_CONTEXT_RULES`, read from the installed `@almadar/core`
/// (`orbLanguage.bindingRules`).
pub fn binding_context_rules() -> &'static HashMap<String, Vec<String>> {
    static PARSED: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let rules: HashMap<String, BindingContextRule> = serde_json::from_str(
            &crate::language_data::language_data_after_preflight().binding_rules,
        )
        .expect("installed binding-rules.json is corrupt — reinstall @almadar/core");
        rules.into_iter().map(|(k, v)| (k, v.allowed)).collect()
    })
}

/// Look up the allowed sigil roots for a given context name.
pub fn allowed_sigils_for_context(context: &str) -> Option<&'static [String]> {
    binding_context_rules().get(context).map(Vec::as_slice)
}

/// All known sigil context names.
pub fn known_contexts() -> HashSet<&'static str> {
    binding_context_rules().keys().map(String::as_str).collect()
}
