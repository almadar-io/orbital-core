//! Per-language vocabulary lookups over the baked i18n tables. A `Lang`
//! wraps one entry of `tables::definitions()` plus, for every vocabulary
//! section, an inverted `native word -> English word` map built once.
//! Canonicalization is always native -> English: the parser reads whatever
//! script the author wrote and stores the canonical English word, so every
//! downstream phase (lower, codegen, `.orb` JSON) only ever sees English.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::tables::{self, I18nDefinition};
use std::collections::HashMap;
use crate::sync::OnceLock;

/// One vocabulary section's inverted (native -> English) map.
type SectionMap = HashMap<&'static str, &'static str>;

pub struct Lang {
    pub code: &'static str,
    pub def: &'static I18nDefinition,
    keywords: SectionMap,
    shapes: SectionMap,
    tags: SectionMap,
    categories: SectionMap,
    capabilities: SectionMap,
    annotations: SectionMap,
    sigils: SectionMap,
    effects: SectionMap,
    types: SectionMap,
    units: SectionMap,
    literals: SectionMap,
    reserved_events: SectionMap,
    orb: SectionMap,
    operators: SectionMap,
}

fn invert(section: &'static std::collections::BTreeMap<String, String>) -> SectionMap {
    section
        .iter()
        .map(|(en, native)| (native.as_str(), en.as_str()))
        .collect()
}

fn build_lang(code: &'static str, def: &'static I18nDefinition) -> Lang {
    Lang {
        code,
        def,
        keywords: invert(&def.keywords),
        shapes: invert(&def.shapes),
        tags: invert(&def.tags),
        categories: invert(&def.categories),
        capabilities: invert(&def.capabilities),
        annotations: invert(&def.annotations),
        sigils: invert(&def.sigils),
        effects: invert(&def.effects),
        types: invert(&def.types),
        units: invert(&def.units),
        literals: invert(&def.literals),
        reserved_events: invert(&def.reserved_events),
        orb: invert(&def.orb),
        operators: invert(&def.operators),
    }
}

fn all_langs() -> &'static Vec<Lang> {
    static ALL: OnceLock<Vec<Lang>> = OnceLock::new();
    ALL.get_or_init(|| {
        tables::definitions()
            .iter()
            .map(|(code, def)| build_lang(code.as_str(), def))
            .collect()
    })
}

/// Every known language, `en` included.
pub fn all() -> &'static [Lang] {
    all_langs().as_slice()
}

/// The English `Lang` — built in; never needs a language pack.
pub fn english() -> &'static Lang {
    static EN: OnceLock<Lang> = OnceLock::new();
    EN.get_or_init(|| build_lang("en", tables::english()))
}

impl Lang {
    /// Build a `Lang` from an arbitrary `I18nDefinition` — used to
    /// construct one-off/derived vocabularies (e.g. an inverted
    /// English->native definition, for building a fixture that goes the
    /// opposite direction from the normal native->English canonicalization).
    /// Not part of `all()`/`english()`/`for_lolo_first_token`.
    pub fn from_definition(code: &'static str, def: &'static I18nDefinition) -> Lang {
        build_lang(code, def)
    }
}

/// The keywords that can legally open a `.lolo` program (LOLO.md §3 top-level
/// structure). A program's language is the one whose translation of any of
/// these equals the first token exactly; English otherwise.
pub const LOLO_FIRST_TOKEN_KEYWORDS: &[&str] =
    &["app", "uses", "expects", "type", "theme", "orbital"];

/// Never a substring match — `tok` must equal a non-English language's
/// translation of a first-token keyword exactly.
pub fn for_lolo_first_token(tok: &str) -> Option<&'static Lang> {
    if LOLO_FIRST_TOKEN_KEYWORDS.contains(&tok) {
        return None;
    }
    all().iter().find(|l| {
        l.code != "en"
            && LOLO_FIRST_TOKEN_KEYWORDS
                .iter()
                .any(|kw| l.def.keywords.get(*kw).map(String::as_str) == Some(tok))
    })
}

/// A `.orb` document's language is selected by its top-level keys: exactly
/// one non-English language's `orb["orbitals"]` translation appearing among
/// the keys identifies it. Never a substring match.
pub fn for_orb_top_level_keys<'a>(keys: impl Iterator<Item = &'a str>) -> Option<&'static Lang> {
    let keys: Vec<&str> = keys.collect();
    if keys.contains(&"orbitals") {
        return None;
    }
    all().iter().find(|l| {
        l.code != "en"
            && l.def
                .orb
                .get("orbitals")
                .map(String::as_str)
                .is_some_and(|native| keys.contains(&native))
    })
}

macro_rules! canon_fn {
    ($name:ident, $field:ident) => {
        pub fn $name(&self, native: &str) -> Option<&'static str> {
            self.$field.get(native).copied()
        }
    };
}

impl Lang {
    canon_fn!(canon_keyword, keywords);
    canon_fn!(canon_shape, shapes);
    canon_fn!(canon_tag, tags);
    canon_fn!(canon_category, categories);
    canon_fn!(canon_capability, capabilities);
    canon_fn!(canon_annotation, annotations);
    canon_fn!(canon_sigil, sigils);
    canon_fn!(canon_effect, effects);
    canon_fn!(canon_type, types);
    canon_fn!(canon_unit, units);
    canon_fn!(canon_literal, literals);
    canon_fn!(canon_reserved_event, reserved_events);
    canon_fn!(canon_orb_key, orb);
    canon_fn!(canon_operator, operators);

    /// An s-expression head is either a built-in effect word or an operator
    /// (`ns/name`) — tried in that order, first match wins. Never both: an
    /// English word cannot appear in both `effects` and `operators` (B1
    /// rule 4, checked by the coverage gate).
    pub fn canon_head(&self, native: &str) -> Option<&'static str> {
        self.canon_effect(native)
            .or_else(|| self.canon_operator(native))
    }

    /// A `@<root>` sigil's root is drawn from EITHER the `sigils` section
    /// (`@entity`, `@payload`, `@config`, …) or the `annotations` section
    /// (`@key`, `@label`, `@rebindable`, …) — the lexer produces the same
    /// `Sigil` token for both, and which one applies is decided by the
    /// parser's call-site position, never by the token itself. Trying
    /// `sigils` then `annotations` here is safe only because the two
    /// sections never share a native spelling for two different English
    /// words (B1 rule 3/4) — combining them is the one-line-documented
    /// allowance the i18n plan makes for non-overlapping call sites.
    pub fn canon_sigil_or_annotation(&self, native: &str) -> Option<&'static str> {
        self.canon_sigil(native)
            .or_else(|| self.canon_annotation(native))
    }

    /// Combined lookup over every non-`orb`/non-`operators` section, in a
    /// fixed order. Used at call sites where a single token position may
    /// hold a word from more than one section (e.g. a trait's `[category,
    /// scope, ...capabilities]` modifier list, where the first non-scope
    /// word is a category and later ones are capabilities) and at
    /// `expect_keyword`/`check_ident`, which are also asked to recognize
    /// shape/tag/category/capability/annotation words at their own call
    /// sites. Safe under the same B1 rule 3/4 argument as
    /// `canon_sigil_or_annotation` above: sections never disagree on a
    /// shared native spelling.
    pub fn canon_any(&self, native: &str) -> Option<&'static str> {
        self.canon_keyword(native)
            .or_else(|| self.canon_shape(native))
            .or_else(|| self.canon_tag(native))
            .or_else(|| self.canon_category(native))
            .or_else(|| self.canon_capability(native))
            .or_else(|| self.canon_annotation(native))
            .or_else(|| self.canon_sigil(native))
            .or_else(|| self.canon_type(native))
            .or_else(|| self.canon_unit(native))
            .or_else(|| self.canon_literal(native))
            .or_else(|| self.canon_reserved_event(native))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_canon_is_identity_over_its_own_section() {
        let en = english();
        assert_eq!(en.canon_keyword("app"), Some("app"));
        assert_eq!(en.canon_keyword("not-a-keyword"), None);
    }

    #[test]
    fn all_includes_english_and_the_installed_first_party_packs() {
        let codes: Vec<&str> = all().iter().map(|l| l.code).collect();
        assert!(codes.contains(&"en"));
        assert!(codes.contains(&"ar"));
        assert!(codes.contains(&"sl"));
    }
}
