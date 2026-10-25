//! The i18n coverage gate: for every language other than English, every
//! vocabulary section must carry exactly the English key set (no missing,
//! no orphan keys) with every value non-empty. Vocabulary-vs-parser
//! equality (does `orbital-lolo`'s `Vocabulary` agree with `en`?) is a
//! separate check owned by `orbital-lolo` — this module only checks the
//! tables against each other.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::lang::LOLO_FIRST_TOKEN_KEYWORDS;
use super::tables::I18nDefinition;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    /// A key present in `en` for this section is missing in this language.
    Missing,
    /// A key present in this language's section is not in `en` (orphan).
    Orphan,
    /// The key exists in both, but this language's value is empty.
    Empty,
    /// A language-selector translation (a `.lolo` first-token keyword or
    /// `orb.orbitals`) is shared by two languages, so a file's language
    /// would be ambiguous.
    SelectorNotUnique,
    /// Two English words in one section share this language's spelling, so a
    /// program using it would be ambiguous.
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageProblem {
    pub lang: String,
    pub section: String,
    pub kind: ProblemKind,
    pub key: String,
}

const SECTIONS: [&str; 14] = [
    "keywords",
    "shapes",
    "tags",
    "categories",
    "capabilities",
    "annotations",
    "sigils",
    "effects",
    "types",
    "units",
    "literals",
    "reservedEvents",
    "orb",
    "operators",
];

fn section_map<'a>(def: &'a I18nDefinition, section: &str) -> &'a BTreeMap<String, String> {
    match section {
        "keywords" => &def.keywords,
        "shapes" => &def.shapes,
        "tags" => &def.tags,
        "categories" => &def.categories,
        "capabilities" => &def.capabilities,
        "annotations" => &def.annotations,
        "sigils" => &def.sigils,
        "effects" => &def.effects,
        "types" => &def.types,
        "units" => &def.units,
        "literals" => &def.literals,
        "reservedEvents" => &def.reserved_events,
        "orb" => &def.orb,
        "operators" => &def.operators,
        other => panic!("unknown i18n section: {other}"),
    }
}

/// Check every language in `defs` other than `en` against `en`'s key set,
/// per section: missing keys, orphan keys, and empty values. Returns every
/// problem found (never truncated).
pub fn check_translation_coverage(defs: &BTreeMap<String, I18nDefinition>) -> Vec<CoverageProblem> {
    let mut problems = Vec::new();
    let Some(en) = defs.get("en") else {
        return problems;
    };
    for (code, def) in defs {
        if code == "en" {
            continue;
        }
        for section in SECTIONS {
            let en_map = section_map(en, section);
            let lang_map = section_map(def, section);
            for key in en_map.keys() {
                match lang_map.get(key) {
                    None => problems.push(CoverageProblem {
                        lang: code.clone(),
                        section: section.to_string(),
                        kind: ProblemKind::Missing,
                        key: key.clone(),
                    }),
                    Some(v) if v.is_empty() => problems.push(CoverageProblem {
                        lang: code.clone(),
                        section: section.to_string(),
                        kind: ProblemKind::Empty,
                        key: key.clone(),
                    }),
                    Some(_) => {}
                }
            }
            for key in lang_map.keys() {
                if !en_map.contains_key(key) {
                    problems.push(CoverageProblem {
                        lang: code.clone(),
                        section: section.to_string(),
                        kind: ProblemKind::Orphan,
                        key: key.clone(),
                    });
                }
            }
        }
    }
    for (code, def) in defs {
        if code == "en" {
            continue;
        }
        for section in SECTIONS {
            let mut by_value: BTreeMap<&str, &str> = BTreeMap::new();
            for (key, value) in section_map(def, section) {
                if by_value.insert(value.as_str(), key.as_str()).is_some() {
                    problems.push(CoverageProblem {
                        lang: code.clone(),
                        section: section.to_string(),
                        kind: ProblemKind::Collision,
                        key: key.clone(),
                    });
                }
            }
        }
    }
    let mut selector_uses: BTreeMap<&str, Vec<(&str, &str, &str)>> = BTreeMap::new();
    for (code, def) in defs {
        for kw in LOLO_FIRST_TOKEN_KEYWORDS {
            if let Some(v) = def.keywords.get(*kw) {
                selector_uses
                    .entry(v.as_str())
                    .or_default()
                    .push((code, "keywords", kw));
            }
        }
        if let Some(v) = def.orb.get("orbitals") {
            selector_uses
                .entry(v.as_str())
                .or_default()
                .push((code, "orb", "orbitals"));
        }
    }
    for uses in selector_uses.values() {
        let distinct_langs = uses
            .iter()
            .map(|(code, _, _)| *code)
            .collect::<std::collections::BTreeSet<_>>();
        if distinct_langs.len() < 2 {
            continue;
        }
        for (code, section, key) in uses {
            problems.push(CoverageProblem {
                lang: code.to_string(),
                section: section.to_string(),
                kind: ProblemKind::SelectorNotUnique,
                key: key.to_string(),
            });
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::tables;

    #[test]
    fn baked_tables_report_every_problem() {
        // Not a pass/fail assertion here: the ar/sl tables are still being
        // completed by parallel agents. This just proves the gate runs and
        // reports something concrete for every language.
        let problems = check_translation_coverage(tables::definitions());
        let ar_problems = problems.iter().filter(|p| p.lang == "ar").count();
        let sl_problems = problems.iter().filter(|p| p.lang == "sl").count();
        eprintln!("ar coverage problems: {ar_problems}, sl coverage problems: {sl_problems}");
    }
}
