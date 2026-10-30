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

    fn complete(code: &str) -> I18nDefinition {
        let mut def = tables::english().clone();
        def.meta.code = code.to_string();
        def.meta.name = code.to_string();
        for section in SECTIONS {
            let map = match section {
                "keywords" => &mut def.keywords,
                "shapes" => &mut def.shapes,
                "tags" => &mut def.tags,
                "categories" => &mut def.categories,
                "capabilities" => &mut def.capabilities,
                "annotations" => &mut def.annotations,
                "sigils" => &mut def.sigils,
                "effects" => &mut def.effects,
                "types" => &mut def.types,
                "units" => &mut def.units,
                "literals" => &mut def.literals,
                "reservedEvents" => &mut def.reserved_events,
                "orb" => &mut def.orb,
                _ => &mut def.operators,
            };
            for value in map.values_mut() {
                *value = format!("{code}:{value}");
            }
        }
        def
    }

    fn defs_with(lang: I18nDefinition) -> BTreeMap<String, I18nDefinition> {
        BTreeMap::from([
            ("en".to_string(), tables::english().clone()),
            (lang.meta.code.clone(), lang),
        ])
    }

    fn key_problems(
        defs: &BTreeMap<String, I18nDefinition>,
    ) -> Vec<(String, String, ProblemKind, String)> {
        check_translation_coverage(defs)
            .into_iter()
            .filter(|p| matches!(p.kind, ProblemKind::Missing | ProblemKind::Orphan))
            .map(|p| (p.lang, p.section, p.kind, p.key))
            .collect()
    }

    #[test]
    fn pack_completeness_control_a_full_pack_has_no_key_problem() {
        assert_eq!(key_problems(&defs_with(complete("qz"))), Vec::new());
    }

    #[test]
    fn pack_completeness_reports_a_missing_orb_key_and_an_orphan_effect() {
        let mut pack = complete("qz");
        pack.orb.remove("typeContractVersion");
        pack.effects
            .insert("lambda".to_string(), "qz:lambda".to_string());
        assert_eq!(
            key_problems(&defs_with(pack)),
            vec![
                (
                    "qz".to_string(),
                    "effects".to_string(),
                    ProblemKind::Orphan,
                    "lambda".to_string()
                ),
                (
                    "qz".to_string(),
                    "orb".to_string(),
                    ProblemKind::Missing,
                    "typeContractVersion".to_string()
                ),
            ]
        );
    }

    #[test]
    fn pack_completeness_reports_an_orphan_operator_and_an_empty_value() {
        let mut pack = complete("qz");
        pack.operators
            .insert("nonexistent/orphan-operator".to_string(), "qz:x".to_string());
        pack.keywords.insert("state".to_string(), String::new());
        let problems = check_translation_coverage(&defs_with(pack));
        assert!(problems.iter().any(|p| p.kind == ProblemKind::Orphan
            && p.section == "operators"
            && p.key == "nonexistent/orphan-operator"));
        assert!(problems
            .iter()
            .any(|p| p.kind == ProblemKind::Empty && p.section == "keywords" && p.key == "state"));
    }

    #[cfg(feature = "std")]
    #[test]
    fn installed_packs_carry_exactly_the_english_key_set() {
        let start = match std::env::var_os("ORB_VOCAB_PACKS_FROM") {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::env::current_dir().unwrap(),
        };
        let packs =
            super::super::packs::discover_vocabulary_packs(&start, &start.join(".no-user-store"))
                .unwrap_or_else(|e| panic!("{e}"));
        let mut defs = BTreeMap::from([("en".to_string(), tables::english().clone())]);
        for pack in &packs {
            defs.insert(pack.definition.meta.code.clone(), pack.definition.clone());
        }
        let problems = key_problems(&defs);
        assert!(
            problems.is_empty(),
            "{} pack key problem(s) in {:?}:\n{}",
            problems.len(),
            packs.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            problems
                .iter()
                .map(|(lang, section, kind, key)| format!("  [{lang}] {section}.{kind:?}: {key}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
