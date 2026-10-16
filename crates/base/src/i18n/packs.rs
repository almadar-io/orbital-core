//! Language packs: every language but English is an installed npm package
//! whose `package.json` declares a top-level
//! `orbVocabulary: { compiler, vocabulary }`, the vocabulary file being one
//! [`I18nDefinition`]. Discovered node-style like behavior packages (each
//! ancestor `node_modules` nearest first, the `~/.orb` user store last;
//! nearest wins per package name). Twin of `@almadar/core/language-data`
//! `readVocabularyPacks` and `@almadar/core/i18n` `registerVocabulary`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;

use super::coverage::{check_translation_coverage, ProblemKind};
use super::tables::{english, I18nDefinition};
use crate::behavior_packages::{compiler_satisfies, node_modules_dirs, user_store_dir};
use crate::language_data::{running_version, LanguageDataError};

/// The top-level `package.json` key that makes a package a language pack.
pub const VOCABULARY_BLOCK: &str = "orbVocabulary";

/// One installed language pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabularyPack {
    pub package: String,
    pub version: String,
    /// The `orb` versions the pack supports (`orbVocabulary.compiler`).
    pub compiler: String,
    pub definition: I18nDefinition,
}

fn invalid(path: impl Into<PathBuf>, reason: String) -> LanguageDataError {
    LanguageDataError::Invalid {
        path: path.into(),
        reason,
    }
}

fn package_dirs(node_modules: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(node_modules) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if name.starts_with('@') {
            if let Ok(scoped) = fs::read_dir(&path) {
                for sub in scoped.flatten() {
                    let sub_name = sub.file_name().to_string_lossy().to_string();
                    out.push((format!("{name}/{sub_name}"), sub.path()));
                }
            }
        } else {
            out.push((name, path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// Every installed language pack visible from `start_dir`, validated
/// ([`validate_packs`]) and sorted by package name.
pub fn discover_vocabulary_packs(
    start_dir: &Path,
    user_store: &Path,
) -> Result<Vec<VocabularyPack>, LanguageDataError> {
    discover_for(start_dir, user_store, running_version())
}

fn discover_for(
    start_dir: &Path,
    user_store: &Path,
    running: &str,
) -> Result<Vec<VocabularyPack>, LanguageDataError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut packs: Vec<VocabularyPack> = Vec::new();
    for node_modules in node_modules_dirs(start_dir, user_store) {
        for dir in package_dirs(&node_modules) {
            let package_json = dir.join("package.json");
            if !package_json.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&package_json)
                .map_err(|e| invalid(&package_json, e.to_string()))?;
            let manifest: Value = serde_json::from_str(&raw)
                .map_err(|e| invalid(&package_json, format!("invalid JSON: {e}")))?;
            let Some(block) = manifest.get(VOCABULARY_BLOCK) else {
                continue;
            };
            let name = manifest
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(&package_json, "a language pack needs a `name`".into()))?
                .to_string();
            if !seen.insert(name.clone()) {
                continue;
            }
            packs.push(read_pack(
                &name,
                &manifest,
                block,
                &dir,
                &package_json,
                running,
            )?);
        }
    }
    packs.sort_by(|a, b| a.package.cmp(&b.package));
    validate_packs(&packs)?;
    Ok(packs)
}

fn read_pack(
    name: &str,
    manifest: &Value,
    block: &Value,
    dir: &Path,
    package_json: &Path,
    running: &str,
) -> Result<VocabularyPack, LanguageDataError> {
    let field = |key: &str| {
        block.get(key).and_then(Value::as_str).ok_or_else(|| {
            invalid(
                package_json,
                format!("{name}: `{VOCABULARY_BLOCK}.{key}` is missing"),
            )
        })
    };
    let compiler = field("compiler")?.to_string();
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match compiler_satisfies(&compiler, running) {
        Ok(true) => {}
        Ok(false) => {
            return Err(LanguageDataError::PackIncompatible {
                package: name.to_string(),
                version,
                range: compiler,
                running: running.to_string(),
            })
        }
        Err(e) => {
            return Err(invalid(
                package_json,
                format!(
                    "{name}: `{VOCABULARY_BLOCK}.compiler` {compiler:?} is not a semver range: {e}"
                ),
            ))
        }
    }
    let file = dir.join(field("vocabulary")?);
    let raw = fs::read_to_string(&file).map_err(|e| invalid(&file, format!("{name}: {e}")))?;
    let definition: I18nDefinition =
        serde_json::from_str(&raw).map_err(|e| invalid(&file, format!("{name}: {e}")))?;
    Ok(VocabularyPack {
        package: name.to_string(),
        version,
        compiler,
        definition,
    })
}

/// Refuses packs that make a program ambiguous: English replaced, one code
/// claimed twice, two English words on one spelling within a section, an empty
/// spelling, or a language selector another installed language uses. A
/// missing word is not refused — English spellings stay legal there.
pub fn validate_packs(packs: &[VocabularyPack]) -> Result<(), LanguageDataError> {
    let mut defs: BTreeMap<String, I18nDefinition> =
        BTreeMap::from([("en".to_string(), english().clone())]);
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for pack in packs {
        let code = &pack.definition.meta.code;
        if code == "en" {
            return Err(invalid(
                &pack.package,
                format!(
                    "{}: English is built in and cannot be replaced",
                    pack.package
                ),
            ));
        }
        if let Some(other) = owner.insert(code.clone(), &pack.package) {
            return Err(invalid(
                &pack.package,
                format!(
                    "language \"{code}\" is claimed by two packs: {other} and {}",
                    pack.package
                ),
            ));
        }
        defs.insert(code.clone(), pack.definition.clone());
    }
    let fatal: Vec<String> = check_translation_coverage(&defs)
        .into_iter()
        .filter(|p| {
            matches!(
                p.kind,
                ProblemKind::Empty | ProblemKind::SelectorNotUnique | ProblemKind::Collision
            ) && p.lang != "en"
        })
        .map(|p| {
            let package = owner.get(&p.lang).copied().unwrap_or_default();
            format!(
                "{package} ({}): {:?} {}.{}",
                p.lang, p.kind, p.section, p.key
            )
        })
        .collect();
    if fatal.is_empty() {
        Ok(())
    } else {
        Err(invalid(
            "language packs",
            format!("refused:\n  {}", fatal.join("\n  ")),
        ))
    }
}

static INSTALLED: OnceLock<Vec<VocabularyPack>> = OnceLock::new();

/// Installs the language packs process-wide, for hosts that cannot walk
/// `node_modules` (wasm, via the language bundle). The first install wins.
pub fn install_vocabulary_packs(packs: Vec<VocabularyPack>) -> Result<(), LanguageDataError> {
    validate_packs(&packs)?;
    let _ = INSTALLED.set(packs);
    Ok(())
}

/// The process-wide language packs: whatever [`install_vocabulary_packs`]
/// set, else discovered from the working directory (and the user store). A
/// failure is not cached.
pub fn vocabulary_packs() -> Result<&'static [VocabularyPack], LanguageDataError> {
    if let Some(packs) = INSTALLED.get() {
        return Ok(packs);
    }
    let cwd = std::env::current_dir().map_err(|e| {
        invalid(
            PathBuf::new(),
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let packs = discover_vocabulary_packs(&cwd, &user_store_dir().unwrap_or_default())?;
    Ok(INSTALLED.get_or_init(|| packs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "orb-vocab-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pack(root: &Path, name: &str, code: &str, orbital: &str, compiler: &str) {
        let dir = root.join("node_modules").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            json!({"name": name, "version": "1.0.0",
                   "orbVocabulary": {"compiler": compiler, "vocabulary": "vocabulary.json"}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("vocabulary.json"),
            json!({"meta": {"code": code, "name": code, "rtl": false},
                   "keywords": {"orbital": orbital, "state": format!("{code}-state")},
                   "orb": {"orbitals": format!("{code}-orbitals")}})
            .to_string(),
        )
        .unwrap();
    }

    fn discover(root: &Path) -> Result<Vec<VocabularyPack>, LanguageDataError> {
        discover_for(root, &root.join("store"), "17.12.0")
    }

    #[test]
    fn every_package_declaring_orb_vocabulary_is_a_pack() {
        let root = tmp("ok");
        write_pack(&root, "@someone/lolo-qa", "qa", "qa-orbital", ">=17.0.0");
        write_pack(&root, "lolo-qb", "qb", "qb-orbital", ">=17.0.0");
        let packs = discover(&root).unwrap();
        let got: Vec<(&str, &str)> = packs
            .iter()
            .map(|p| (p.package.as_str(), p.definition.meta.code.as_str()))
            .collect();
        assert_eq!(got, vec![("@someone/lolo-qa", "qa"), ("lolo-qb", "qb")]);
        assert_eq!(packs[0].definition.keywords["orbital"], "qa-orbital");
    }

    #[test]
    fn control_no_packs_is_english_only_not_an_error() {
        assert!(discover(&tmp("none")).unwrap().is_empty());
    }

    #[test]
    fn the_user_store_is_the_last_rung_and_a_project_copy_shadows_it() {
        let root = tmp("store");
        let store = root.join("store");
        write_pack(&store, "lolo-qc", "qc", "store-orbital", ">=17.0.0");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        assert_eq!(
            discover_for(&project, &store, "17.12.0").unwrap()[0]
                .definition
                .keywords["orbital"],
            "store-orbital"
        );
        write_pack(&project, "lolo-qc", "qc", "project-orbital", ">=17.0.0");
        let packs = discover_for(&project, &store, "17.12.0").unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].definition.keywords["orbital"], "project-orbital");
    }

    #[test]
    fn two_packages_claiming_one_code_are_refused_naming_both() {
        let root = tmp("dup");
        write_pack(&root, "lolo-qd-one", "qd", "qd1-orbital", ">=17.0.0");
        write_pack(&root, "lolo-qd-two", "qd", "qd2-orbital", ">=17.0.0");
        let err = discover(&root).unwrap_err().to_string();
        assert!(
            err.contains("lolo-qd-one") && err.contains("lolo-qd-two"),
            "{err}"
        );
    }

    #[test]
    fn a_pack_for_another_orb_is_incompatible() {
        let root = tmp("range");
        write_pack(&root, "lolo-qe", "qe", "qe-orbital", ">=18.0.0");
        let err = discover(&root).unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        assert!(err.to_string().contains("lolo-qe"), "{err}");
    }

    #[test]
    fn a_selector_english_already_uses_is_refused() {
        let root = tmp("selector");
        write_pack(&root, "lolo-qf", "qf", "orbital", ">=17.0.0");
        let err = discover(&root).unwrap_err().to_string();
        assert!(
            err.contains("lolo-qf") && err.contains("SelectorNotUnique"),
            "{err}"
        );
    }

    #[test]
    fn two_english_words_on_one_spelling_are_refused() {
        let root = tmp("collision");
        write_pack(&root, "lolo-qg", "qg", "qg-state", ">=17.0.0");
        let err = discover(&root).unwrap_err().to_string();
        assert!(
            err.contains("lolo-qg") && err.contains("Collision"),
            "{err}"
        );
    }

    #[test]
    fn english_cannot_be_replaced() {
        let root = tmp("en");
        write_pack(&root, "lolo-en", "en", "en-orbital", ">=17.0.0");
        assert!(discover(&root)
            .unwrap_err()
            .to_string()
            .contains("English is built in"));
    }

    #[test]
    fn edge_a_missing_vocabulary_file_names_the_package() {
        let root = tmp("nofile");
        write_pack(&root, "lolo-qh", "qh", "qh-orbital", ">=17.0.0");
        fs::remove_file(root.join("node_modules/lolo-qh/vocabulary.json")).unwrap();
        let err = discover(&root).unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("lolo-qh"), "{err}");
    }
}
