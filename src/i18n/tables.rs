//! The vocabulary tables: English, built in (the grammar this binary
//! implements; `en.json` is synced from `@almadar/core` by pattern-sync's
//! `i18n` step), plus every installed language pack ([`super::packs`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I18nMeta {
    pub code: String,
    pub name: String,
    pub rtl: bool,
}

/// One language's vocabulary. A pack may leave a section (or a word) out:
/// English spellings stay legal there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct I18nDefinition {
    pub meta: I18nMeta,
    #[serde(default)]
    pub keywords: BTreeMap<String, String>,
    #[serde(default)]
    pub shapes: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub sigils: BTreeMap<String, String>,
    #[serde(default)]
    pub effects: BTreeMap<String, String>,
    #[serde(default)]
    pub types: BTreeMap<String, String>,
    #[serde(default)]
    pub units: BTreeMap<String, String>,
    #[serde(default)]
    pub literals: BTreeMap<String, String>,
    #[serde(default)]
    pub reserved_events: BTreeMap<String, String>,
    #[serde(default)]
    pub orb: BTreeMap<String, String>,
    #[serde(default)]
    pub operators: BTreeMap<String, String>,
}

/// The built-in English vocabulary.
pub fn english() -> &'static I18nDefinition {
    static EN: OnceLock<I18nDefinition> = OnceLock::new();
    EN.get_or_init(|| serde_json::from_str(include_str!("en.json")).expect("built-in en.json"))
}

/// English plus every installed language pack, keyed by language code.
/// Entry points surface a broken pack as a structured error through
/// `language_data::preflight`; reaching the panic here means one skipped it.
pub fn definitions() -> &'static BTreeMap<String, I18nDefinition> {
    static DEFS: OnceLock<BTreeMap<String, I18nDefinition>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let packs =
            super::packs::vocabulary_packs().unwrap_or_else(|e| panic!("{}: {e}", e.code()));
        let mut defs = BTreeMap::from([("en".to_string(), english().clone())]);
        for pack in packs {
            defs.insert(pack.definition.meta.code.clone(), pack.definition.clone());
        }
        defs
    })
}
