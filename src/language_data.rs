//! Language data: the pattern vocabulary every phase validates against, read
//! from the INSTALLED, version-pinned `@almadar/core` package, and the operator
//! table, read from the installed `@almadar/std` (neither is baked into the
//! binary). `docs/Almadar_Behavior_Packages.md` §14.
//!
//! One loader serves L1 (`orbital-lolo`) and L2 (`orbital-compiler`): the
//! package is found with the §5 node-style walk (project `node_modules`
//! ancestors first, the `~/.orb` user store last), its `package.json`
//! top-level `orbLanguage` block names the data files, and `orbLanguage.compiler` is checked
//! against this binary's version. There is no embedded fallback and no env
//! override.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;
use thiserror::Error;

use crate::behavior_packages::{compiler_satisfies, find_npm_package, user_store_dir};

/// The package that ships the pattern vocabulary.
pub const CORE_PACKAGE: &str = "@almadar/core";
/// The package that ships the canonical operator table.
pub use crate::behavior_packages::STD_PACKAGE;

pub const CODE_NOT_INSTALLED: &str = "ORB_LANGUAGE_DATA_NOT_INSTALLED";
pub const CODE_INCOMPATIBLE: &str = "ORB_LANGUAGE_DATA_INCOMPATIBLE";
pub const CODE_INVALID: &str = "ORB_LANGUAGE_DATA_INVALID";
/// A missing `@almadar/std` is a missing behavior package, whichever check
/// notices first: the same code the compiler's inline phase reports.
pub const CODE_STD_NOT_INSTALLED: &str = "ORB_BEHAVIOR_PACKAGE_NOT_INSTALLED";

/// The raw text of the four language-data files plus where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageData {
    pub core_version: String,
    pub root: PathBuf,
    pub patterns: String,
    pub component_mapping: String,
    pub event_contracts: String,
    pub services: String,
    /// `BINDING_CONTEXT_RULES` (`orbLanguage.bindingRules`): the sigil roots
    /// each binding context (guard / effect / tick) allows.
    pub binding_rules: String,
    /// The program value types (`orbLanguage.programTypes`): the records `orbital`, `trait` and `behavior` values
    /// are, by name, in the `.orb` field-definition shape.
    pub program_types: String,
    /// The theme presets (`orbLanguage.themes`, a directory of one
    /// `ThemeDefinition` per `.json` file), sorted by file name.
    pub themes: Vec<String>,
}

/// The canonical operator table plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorData {
    pub std_version: String,
    pub root: PathBuf,
    pub operators: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LanguageDataError {
    #[error("{}", not_installed_message(package))]
    NotInstalled { package: &'static str },
    #[error(
        "installed {package}@{version} requires orb {range}, but this orb is {running} \
         — install a {package} whose `orbLanguage.compiler` accepts {running}, or upgrade orb"
    )]
    Incompatible {
        package: &'static str,
        version: String,
        range: String,
        running: String,
    },
    #[error(
        "installed {package}@{version} declares {} rust-target operator(s) this orb does not \
         implement: {} — upgrade orb, or install a {package} this orb supports",
        operators.len(),
        operators.join(", ")
    )]
    Unimplemented {
        package: &'static str,
        version: String,
        operators: Vec<String>,
    },
    #[error("installed language data is invalid at {path:?}: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error(
        "installed language pack {package}@{version} requires orb {range}, but this orb is {running} \
         — update the pack (`orb update`) or upgrade orb"
    )]
    PackIncompatible {
        package: String,
        version: String,
        range: String,
        running: String,
    },
}

impl LanguageDataError {
    pub fn code(&self) -> &'static str {
        match self {
            LanguageDataError::NotInstalled { package } if *package == STD_PACKAGE => {
                CODE_STD_NOT_INSTALLED
            }
            LanguageDataError::NotInstalled { .. } => CODE_NOT_INSTALLED,
            LanguageDataError::Incompatible { .. }
            | LanguageDataError::Unimplemented { .. }
            | LanguageDataError::PackIncompatible { .. } => CODE_INCOMPATIBLE,
            LanguageDataError::Invalid { .. } => CODE_INVALID,
        }
    }

    /// True when installing the package would fix it (drives the CLI's
    /// `needs_install`).
    pub fn needs_install(&self) -> bool {
        matches!(self, LanguageDataError::NotInstalled { .. })
    }
}

pub use crate::behavior_packages::{install_command, not_installed_message};

pub(crate) fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Resolves `@almadar/core` from `start_dir` / `user_store` and reads the
/// language data it declares. Pure: touches no global state.
pub fn load_language_data(
    start_dir: &Path,
    user_store: &Path,
) -> Result<LanguageData, LanguageDataError> {
    load_language_data_for(start_dir, user_store, running_version())
}

fn load_language_data_for(
    start_dir: &Path,
    user_store: &Path,
    running: &str,
) -> Result<LanguageData, LanguageDataError> {
    let block = read_language_block(CORE_PACKAGE, start_dir, user_store, running)?;
    let program_types = block.read("programTypes")?;
    crate::language::program_types::parse(&program_types).map_err(|reason| LanguageDataError::Invalid {
        path: block.declared_path("programTypes").unwrap_or_default(),
        reason,
    })?;
    Ok(LanguageData {
        patterns: block.read("patterns")?,
        component_mapping: block.read("componentMapping")?,
        event_contracts: block.read("eventContracts")?,
        services: block.read("services")?,
        binding_rules: block.read("bindingRules")?,
        program_types,
        themes: block.read_json_dir("themes")?,
        core_version: block.pkg.version,
        root: block.pkg.root,
    })
}

/// Resolves `@almadar/std` from `start_dir` / `user_store` and reads the
/// operator table it declares. Refuses a table with a rust-target operator the
/// evaluator in this binary does not implement. Pure: touches no global state.
pub fn load_operator_data(
    start_dir: &Path,
    user_store: &Path,
) -> Result<OperatorData, LanguageDataError> {
    load_operator_data_for(start_dir, user_store, running_version())
}

fn load_operator_data_for(
    start_dir: &Path,
    user_store: &Path,
    running: &str,
) -> Result<OperatorData, LanguageDataError> {
    let block = read_language_block(STD_PACKAGE, start_dir, user_store, running)?;
    let operators = block.read("operators")?;
    let missing =
        unimplemented_rust_operators(&operators).map_err(|reason| LanguageDataError::Invalid {
            path: block.declared_path("operators").unwrap_or_default(),
            reason,
        })?;
    if !missing.is_empty() {
        return Err(LanguageDataError::Unimplemented {
            package: STD_PACKAGE,
            version: block.pkg.version,
            operators: missing,
        });
    }
    Ok(OperatorData {
        operators,
        std_version: block.pkg.version,
        root: block.pkg.root,
    })
}

/// Operators whose canonical category targets `rust` but that the evaluator
/// cannot resolve, sorted.
fn unimplemented_rust_operators(table: &str) -> Result<Vec<String>, String> {
    let doc: Value = serde_json::from_str(table).map_err(|e| format!("invalid JSON: {e}"))?;
    let categories = doc
        .get("categories")
        .and_then(Value::as_object)
        .ok_or("missing `categories`")?;
    let operators = doc
        .get("operators")
        .and_then(Value::as_object)
        .ok_or("missing `operators`")?;
    let targets_rust = |category: &str| {
        categories
            .get(category)
            .and_then(|c| c.get("target"))
            .and_then(Value::as_array)
            .is_some_and(|t| t.iter().any(|x| x.as_str() == Some("rust")))
    };
    let mut missing: Vec<String> = operators
        .iter()
        .filter(|(_, meta)| {
            targets_rust(meta.get("category").and_then(Value::as_str).unwrap_or(""))
        })
        .filter(|(name, _)| crate::evaluator::get_operator(name).is_none())
        .map(|(name, _)| name.clone())
        .collect();
    missing.sort();
    Ok(missing)
}

struct LanguageBlock {
    pkg: crate::behavior_packages::NpmPackage,
    block: serde_json::Map<String, Value>,
    package_json: PathBuf,
}

impl LanguageBlock {
    fn declared_path(&self, field: &str) -> Option<PathBuf> {
        self.block
            .get(field)
            .and_then(Value::as_str)
            .map(|rel| self.pkg.root.join(rel))
    }

    fn read_json_dir(&self, field: &str) -> Result<Vec<String>, LanguageDataError> {
        let dir = self
            .declared_path(field)
            .ok_or_else(|| LanguageDataError::Invalid {
                path: self.package_json.clone(),
                reason: format!("missing `orbLanguage.{field}`"),
            })?;
        let invalid = |reason: String| LanguageDataError::Invalid {
            path: dir.clone(),
            reason,
        };
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| invalid(e.to_string()))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        if files.is_empty() {
            return Err(invalid("no `.json` files".to_string()));
        }
        files.sort();
        files
            .iter()
            .map(|p| {
                fs::read_to_string(p).map_err(|e| LanguageDataError::Invalid {
                    path: p.clone(),
                    reason: e.to_string(),
                })
            })
            .collect()
    }

    fn read(&self, field: &str) -> Result<String, LanguageDataError> {
        let path = self
            .declared_path(field)
            .ok_or_else(|| LanguageDataError::Invalid {
                path: self.package_json.clone(),
                reason: format!("missing `orbLanguage.{field}`"),
            })?;
        fs::read_to_string(&path).map_err(|e| LanguageDataError::Invalid {
            path,
            reason: e.to_string(),
        })
    }
}

/// Finds `package` and returns its range-checked top-level `orbLanguage` block.
fn read_language_block(
    package: &'static str,
    start_dir: &Path,
    user_store: &Path,
    running: &str,
) -> Result<LanguageBlock, LanguageDataError> {
    let pkg = find_npm_package(package, start_dir, user_store)
        .ok_or(LanguageDataError::NotInstalled { package })?;
    let package_json = pkg.root.join("package.json");
    let invalid = |reason: String| LanguageDataError::Invalid {
        path: package_json.clone(),
        reason,
    };
    let raw = fs::read_to_string(&package_json).map_err(|e| invalid(e.to_string()))?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|e| invalid(format!("invalid JSON: {e}")))?;
    // Its own top-level key, never inside `orb`: an `orb` block is a behavior
    // package to every orb binary, and older binaries reject one without the
    // behavior fields — so a language block there would break them all.
    let block = value
        .get("orbLanguage")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| invalid("missing `orbLanguage` block".to_string()))?;
    let range = block
        .get("compiler")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing `orbLanguage.compiler` range".to_string()))?;

    match compiler_satisfies(range, running) {
        Ok(true) => {}
        Ok(false) => {
            return Err(LanguageDataError::Incompatible {
                package,
                version: pkg.version,
                range: range.to_string(),
                running: running.to_string(),
            })
        }
        Err(e) => {
            return Err(invalid(format!(
                "`orbLanguage.compiler` {range:?} is not a semver range: {e}"
            )))
        }
    }
    Ok(LanguageBlock {
        pkg,
        block,
        package_json,
    })
}

static LANGUAGE_DATA: OnceLock<LanguageData> = OnceLock::new();

/// Installs the language data process-wide, for hosts that cannot walk
/// `node_modules` (wasm, tests). Returns the data back if something was
/// already installed or lazily loaded.
#[expect(clippy::result_large_err, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub fn install_language_data(data: LanguageData) -> Result<(), LanguageData> {
    LANGUAGE_DATA.set(data)
}

/// The process-wide language data: whatever `install_language_data` set, else
/// lazily loaded from the current working directory (and the user store).
/// A failure is not cached, so a later install or a fixed environment is
/// picked up.
pub fn language_data() -> Result<&'static LanguageData, LanguageDataError> {
    if let Some(data) = LANGUAGE_DATA.get() {
        return Ok(data);
    }
    let cwd = std::env::current_dir().map_err(|e| LanguageDataError::Invalid {
        path: PathBuf::new(),
        reason: format!("cannot read the working directory: {e}"),
    })?;
    let data = load_language_data(&cwd, &user_store_dir().unwrap_or_default())?;
    Ok(LANGUAGE_DATA.get_or_init(|| data))
}

/// Accessors that cannot return a `Result` (`embedded::patterns()` and
/// friends) call this. Every entry point that returns a `Result` calls
/// [`language_data`] first and surfaces the structured error, so reaching the
/// panic here means an entry point skipped that preflight.
pub fn language_data_after_preflight() -> &'static LanguageData {
    match language_data() {
        Ok(data) => data,
        Err(e) => panic!(
            "{}: {e} (the entry point must call language_data() first)",
            e.code()
        ),
    }
}

/// The one check every compiler-backed entry point runs first: the pattern
/// vocabulary (`@almadar/core`) and the operator table (`@almadar/std`) are
/// installed, compatible, and implemented by this binary, and every installed
/// language pack is compatible and unambiguous.
pub fn preflight() -> Result<(), LanguageDataError> {
    language_data()?;
    operator_data()?;
    crate::i18n::packs::vocabulary_packs()?;
    Ok(())
}

/// Installs the language data for a host that cannot walk `node_modules`
/// (wasm): the same data the native loaders read, handed over as one JSON
/// bundle the host assembles from ITS installed packages —
/// `{ core: { version, compiler, patterns, componentMapping, eventContracts,
/// services, bindingRules, programTypes, themes: [ThemeDefinition…] }, std: { version, compiler,
/// operators }, vocabularies: [{ package, version, compiler, vocabulary }] }`.
/// The same checks apply: each `compiler` range must accept this binary, every
/// rust-target operator must be implemented, and the language packs must be
/// unambiguous. The first install wins; a later call leaves the installed data
/// unchanged.
pub fn install_language_bundle(bundle: &str) -> Result<(), LanguageDataError> {
    let (language, ops, packs) = parse_language_bundle(bundle, running_version())?;
    let _ = install_language_data(language);
    let _ = install_operator_data(ops);
    crate::i18n::packs::install_vocabulary_packs(packs)?;
    Ok(())
}

fn parse_language_bundle(
    bundle: &str,
    running: &str,
) -> Result<
    (
        LanguageData,
        OperatorData,
        Vec<crate::i18n::packs::VocabularyPack>,
    ),
    LanguageDataError,
> {
    let invalid = |reason: String| LanguageDataError::Invalid {
        path: PathBuf::from("<language bundle>"),
        reason,
    };
    let value: Value =
        serde_json::from_str(bundle).map_err(|e| invalid(format!("invalid JSON: {e}")))?;
    let part = |name: &str| -> Result<&serde_json::Map<String, Value>, LanguageDataError> {
        value
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| invalid(format!("missing `{name}`")))
    };
    let text = |obj: &serde_json::Map<String, Value>, field: &str, part: &str| {
        obj.get(field)
            .map(Value::to_string)
            .ok_or_else(|| invalid(format!("missing `{part}.{field}`")))
    };
    let version = |obj: &serde_json::Map<String, Value>, part: &str| {
        obj.get("version")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid(format!("missing `{part}.version`")))
    };
    let check = |package: &'static str, obj: &serde_json::Map<String, Value>, part: &str| {
        let range = obj
            .get("compiler")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("missing `{part}.compiler` range")))?;
        match compiler_satisfies(range, running) {
            Ok(true) => Ok(()),
            Ok(false) => Err(LanguageDataError::Incompatible {
                package,
                version: version(obj, part)?,
                range: range.to_string(),
                running: running.to_string(),
            }),
            Err(e) => Err(invalid(format!(
                "`{part}.compiler` {range:?} is not a semver range: {e}"
            ))),
        }
    };

    let core = part("core")?;
    let std = part("std")?;
    check(CORE_PACKAGE, core, "core")?;
    check(STD_PACKAGE, std, "std")?;
    let themes = core
        .get("themes")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| invalid("missing or empty `core.themes`".to_string()))?
        .iter()
        .map(Value::to_string)
        .collect();
    let operators = text(std, "operators", "std")?;
    let missing = unimplemented_rust_operators(&operators).map_err(&invalid)?;
    if !missing.is_empty() {
        return Err(LanguageDataError::Unimplemented {
            package: STD_PACKAGE,
            version: version(std, "std")?,
            operators: missing,
        });
    }
    let program_types = text(core, "programTypes", "core")?;
    crate::language::program_types::parse(&program_types)
        .map_err(|reason| invalid(format!("`core.programTypes`: {reason}")))?;
    let language = LanguageData {
        core_version: version(core, "core")?,
        root: PathBuf::new(),
        patterns: text(core, "patterns", "core")?,
        component_mapping: text(core, "componentMapping", "core")?,
        event_contracts: text(core, "eventContracts", "core")?,
        services: text(core, "services", "core")?,
        binding_rules: text(core, "bindingRules", "core")?,
        program_types,
        themes,
    };
    let ops = OperatorData {
        std_version: version(std, "std")?,
        root: PathBuf::new(),
        operators,
    };
    let packs = value
        .get("vocabularies")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid(
                "missing `vocabularies` (an array, empty when no language pack is installed)"
                    .to_string(),
            )
        })?
        .iter()
        .map(|entry| bundle_pack(entry, running).map_err(&invalid))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((language, ops, packs))
}

fn bundle_pack(entry: &Value, running: &str) -> Result<crate::i18n::packs::VocabularyPack, String> {
    let text = |field: &str| {
        entry
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("a `vocabularies` entry is missing `{field}`"))
    };
    let package = text("package")?;
    let compiler = text("compiler")?;
    match compiler_satisfies(&compiler, running) {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "language pack {package} requires orb {compiler}, but this orb is {running}"
            ))
        }
        Err(e) => {
            return Err(format!(
                "{package}: `compiler` {compiler:?} is not a semver range: {e}"
            ))
        }
    }
    let definition = entry
        .get("vocabulary")
        .cloned()
        .ok_or_else(|| format!("{package}: missing `vocabulary`"))
        .and_then(|v| serde_json::from_value(v).map_err(|e| format!("{package}: {e}")))?;
    Ok(crate::i18n::packs::VocabularyPack {
        package,
        version: text("version")?,
        compiler,
        definition,
    })
}

static OPERATOR_DATA: OnceLock<OperatorData> = OnceLock::new();

/// Installs the operator table process-wide (wasm, tests). Returns the data
/// back if something was already installed or lazily loaded.
pub fn install_operator_data(data: OperatorData) -> Result<(), OperatorData> {
    OPERATOR_DATA.set(data)
}

/// The process-wide operator table: whatever `install_operator_data` set, else
/// lazily loaded from the working directory (and the user store). A failure is
/// not cached.
pub fn operator_data() -> Result<&'static OperatorData, LanguageDataError> {
    if let Some(data) = OPERATOR_DATA.get() {
        return Ok(data);
    }
    let cwd = std::env::current_dir().map_err(|e| LanguageDataError::Invalid {
        path: PathBuf::new(),
        reason: format!("cannot read the working directory: {e}"),
    })?;
    let data = load_operator_data(&cwd, &user_store_dir().unwrap_or_default())?;
    Ok(OPERATOR_DATA.get_or_init(|| data))
}

/// The operator table when something already installed or loaded it; never touches the filesystem.
pub fn operator_data_if_loaded() -> Option<&'static OperatorData> {
    OPERATOR_DATA.get()
}

/// For accessors that cannot return a `Result`; see
/// [`language_data_after_preflight`].
pub fn operator_data_after_preflight() -> &'static OperatorData {
    match operator_data() {
        Ok(data) => data,
        Err(e) => panic!(
            "{}: {e} (the entry point must call operator_data() first)",
            e.code()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior_packages::discover;
    use serde_json::json;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "orb-language-data-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn install_core(root: &Path, version: &str, compiler: &str) -> PathBuf {
        let dir = root.join("node_modules/@almadar/core");
        fs::create_dir_all(dir.join("dist/patterns")).unwrap();
        fs::write(
            dir.join("package.json"),
            json!({
                "name": "@almadar/core",
                "version": version,
                "orbLanguage": {
                    "compiler": compiler,
                    "patterns": "dist/patterns/patterns-registry.json",
                    "componentMapping": "dist/patterns/component-mapping.json",
                    "eventContracts": "dist/patterns/event-contracts.json",
                    "services": "dist/patterns/services-registry.json",
                    "bindingRules": "dist/language/binding-rules.json",
                    "programTypes": "dist/language/program-types.json",
                    "themes": "themes"
                }
            })
            .to_string(),
        )
        .unwrap();
        for (file, body) in [
            ("patterns-registry.json", r#"{"patterns":{"p":{}}}"#),
            ("component-mapping.json", r#"{"mappings":{}}"#),
            ("event-contracts.json", r#"{"contracts":{}}"#),
            ("services-registry.json", r#"{"services":{}}"#),
        ] {
            fs::write(dir.join("dist/patterns").join(file), body).unwrap();
        }
        fs::create_dir_all(dir.join("dist/language")).unwrap();
        fs::write(
            dir.join("dist/language/binding-rules.json"),
            r#"{"guard":{"allowed":["zz"]}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("dist/language/program-types.json"),
            r#"{"OrbitalValue":{"type":"object","properties":{"behavior":{"type":"string","required":true}}}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("themes")).unwrap();
        fs::write(
            dir.join("themes/zz-theme.json"),
            r#"{"name":"zz-theme","tokens":{}}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn installed_core_resolves_and_reads_all_four_files() {
        let tmp = Tmp::new("ok");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let data = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap();
        assert_eq!(data.core_version, "10.1.0");
        assert!(data.patterns.contains("\"p\""));
        assert!(data.component_mapping.contains("mappings"));
        assert!(data.event_contracts.contains("contracts"));
        assert!(data.services.contains("services"));
        assert!(data.binding_rules.contains("\"zz\""));
        assert!(data.program_types.contains("OrbitalValue"));
        assert_eq!(data.themes.len(), 1);
        assert!(data.themes[0].contains("zz-theme"));
    }

    #[test]
    fn edge_a_core_without_program_types_or_with_a_malformed_table_is_invalid() {
        let tmp = Tmp::new("no-program-types");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::write(
            dir.join("dist/language/program-types.json"),
            r#"{"OrbitalValue":{"type":"nonsense"}}"#,
        )
        .unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("OrbitalValue"), "{err}");
        fs::remove_file(dir.join("dist/language/program-types.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn edge_a_core_without_binding_rules_is_invalid_naming_the_key() {
        let tmp = Tmp::new("no-binding-rules");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("dist/language/binding-rules.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn edge_a_theme_directory_without_presets_is_invalid() {
        let tmp = Tmp::new("no-themes");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("themes/zz-theme.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("no `.json` files"), "{err}");
    }

    #[test]
    fn resolves_from_a_nested_start_dir_by_walking_up() {
        let tmp = Tmp::new("walk");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let nested = tmp.0.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        assert!(load_language_data_for(&nested, &tmp.0.join("store"), "17.12.0").is_ok());
    }

    #[test]
    fn user_store_is_the_last_rung() {
        let tmp = Tmp::new("store");
        let store = tmp.0.join("store");
        install_core(&store, "9.0.0", ">=17.0.0");
        let project = tmp.0.join("project");
        fs::create_dir_all(&project).unwrap();
        let data = load_language_data_for(&project, &store, "17.12.0").unwrap();
        assert_eq!(data.core_version, "9.0.0");
        install_core(&project, "10.0.0", ">=17.0.0");
        let data = load_language_data_for(&project, &store, "17.12.0").unwrap();
        assert_eq!(
            data.core_version, "10.0.0",
            "project copy shadows the store"
        );
    }

    #[test]
    fn missing_core_is_not_installed_with_the_install_hint() {
        let tmp = Tmp::new("missing");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(
            err,
            LanguageDataError::NotInstalled {
                package: CORE_PACKAGE
            }
        );
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_NOT_INSTALLED");
        assert!(err.needs_install());
        let msg = err.to_string();
        assert!(msg.contains("@almadar/core is not installed"), "{msg}");
        assert!(msg.contains("run `orb update`"), "{msg}");
    }

    #[test]
    fn compiler_range_mismatch_is_incompatible_naming_both_versions() {
        let tmp = Tmp::new("range");
        install_core(&tmp.0, "10.1.0", ">=17.12.0");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.11.2").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        assert!(!err.needs_install());
        let msg = err.to_string();
        assert!(
            msg.contains("10.1.0") && msg.contains("17.11.2") && msg.contains(">=17.12.0"),
            "{msg}"
        );
    }

    #[test]
    fn edge_missing_language_block_and_missing_data_file_are_invalid_not_empty() {
        let tmp = Tmp::new("invalid");
        let dir = install_core(&tmp.0, "10.1.0", ">=17.0.0");
        fs::remove_file(dir.join("dist/patterns/services-registry.json")).unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");

        fs::write(
            dir.join("package.json"),
            r#"{"name":"@almadar/core","version":"1.0.0","orb":{"compiler":">=1.0.0","language":{}}}"#,
        )
        .unwrap();
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("orbLanguage"), "{err}");
    }

    #[test]
    fn edge_bad_compiler_range_is_invalid() {
        let tmp = Tmp::new("badrange");
        install_core(&tmp.0, "10.1.0", "not a range");
        let err = load_language_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
    }

    #[test]
    fn core_language_block_is_not_mistaken_for_a_behavior_package() {
        let tmp = Tmp::new("not-behavior");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let found =
            discover(&tmp.0, &tmp.0.join("store")).expect("core must not make discovery fail");
        assert!(
            found.is_empty(),
            "core is language data, not a behavior package: {found:?}"
        );
    }

    #[test]
    fn edge_a_malformed_behavior_block_still_errors_loudly() {
        let tmp = Tmp::new("malformed-behavior");
        let dir = tmp.0.join("node_modules/@x/broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"@x/broken","version":"1.0.0","orb":{"registry":"registry"}}"#,
        )
        .unwrap();
        assert!(discover(&tmp.0, &tmp.0.join("store")).is_err());
    }

    fn install_std(root: &Path, compiler: &str, table: &str) -> PathBuf {
        let dir = root.join("node_modules/@almadar/std");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            json!({
                "name": "@almadar/std",
                "version": "8.2.0",
                "orbLanguage": { "compiler": compiler, "operators": "canonical-operators.json" }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(dir.join("canonical-operators.json"), table).unwrap();
        dir
    }

    fn table(op: &str, category_targets: &str) -> String {
        format!(
            r#"{{"categories":{{"arithmetic":{{"target":["rust","ts"]}},"other":{{"target":{category_targets}}}}},
                "operators":{{"+":{{"category":"arithmetic"}},"{op}":{{"category":"other"}}}}}}"#
        )
    }

    #[test]
    fn installed_std_operator_table_resolves() {
        let tmp = Tmp::new("ops-ok");
        install_std(&tmp.0, ">=17.0.0", &table("-", r#"["rust"]"#));
        let data = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap();
        assert_eq!(data.std_version, "8.2.0");
        assert!(data.operators.contains("arithmetic"));
    }

    #[test]
    fn missing_std_is_not_installed_naming_std() {
        let tmp = Tmp::new("ops-missing");
        install_core(&tmp.0, "10.1.0", ">=17.0.0");
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(
            err,
            LanguageDataError::NotInstalled {
                package: STD_PACKAGE
            }
        );
        assert_eq!(err.code(), "ORB_BEHAVIOR_PACKAGE_NOT_INSTALLED");
        assert!(err.needs_install());
        let msg = err.to_string();
        assert!(msg.contains("@almadar/std is not installed"), "{msg}");
        assert!(msg.contains("run `orb update`"), "{msg}");
    }

    #[test]
    fn std_range_mismatch_is_incompatible_naming_std() {
        let tmp = Tmp::new("ops-range");
        install_std(&tmp.0, ">=18.0.0", &table("-", r#"["rust"]"#));
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        let msg = err.to_string();
        assert!(
            msg.contains("@almadar/std@8.2.0") && msg.contains(">=18.0.0"),
            "{msg}"
        );
    }

    #[test]
    fn a_rust_target_operator_the_evaluator_lacks_is_refused() {
        let tmp = Tmp::new("ops-unimpl");
        install_std(
            &tmp.0,
            ">=17.0.0",
            &table("zz/not-implemented", r#"["rust"]"#),
        );
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        assert!(!err.needs_install());
        assert!(err.to_string().contains("zz/not-implemented"), "{err}");
    }

    #[test]
    fn control_the_same_operator_outside_a_rust_category_is_accepted() {
        let tmp = Tmp::new("ops-ts-only");
        install_std(
            &tmp.0,
            ">=17.0.0",
            &table("zz/not-implemented", r#"["ts"]"#),
        );
        assert!(load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").is_ok());
    }

    #[test]
    fn edge_malformed_table_and_missing_operators_field_are_invalid() {
        let tmp = Tmp::new("ops-invalid");
        let dir = install_std(&tmp.0, ">=17.0.0", r#"{"operators":{}}"#);
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        assert!(err.to_string().contains("categories"), "{err}");

        fs::write(
            dir.join("package.json"),
            r#"{"name":"@almadar/std","version":"8.2.0","orbLanguage":{"compiler":">=17.0.0"}}"#,
        )
        .unwrap();
        let err = load_operator_data_for(&tmp.0, &tmp.0.join("store"), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("orbLanguage.operators"), "{err}");
    }

    fn bundle(compiler: &str, extra_op: &str) -> String {
        json!({
            "core": {
                "version": "10.1.0", "compiler": compiler,
                "patterns": {"patterns": {"p": {}}}, "componentMapping": {"mappings": {}},
                "eventContracts": {"contracts": {}}, "services": {"services": {}},
                "bindingRules": {"guard": {"allowed": ["zz"]}},
                "programTypes": {"OrbitalValue": {"type": "object", "properties": {"behavior": {"type": "string", "required": true}}}},
                "themes": [{"name": "zz", "tokens": {}}]
            },
            "std": {
                "version": "8.2.0", "compiler": compiler,
                "operators": serde_json::from_str::<Value>(&table(extra_op, r#"["rust"]"#)).unwrap()
            },
            "vocabularies": [{
                "package": "lolo-zz", "version": "1.0.0", "compiler": compiler,
                "vocabulary": {"meta": {"code": "zz", "name": "Zz", "rtl": false},
                               "keywords": {"orbital": "zz-orbital"}}
            }]
        })
        .to_string()
    }

    #[test]
    fn a_language_bundle_carries_what_the_native_loaders_read() {
        let (lang, ops, packs) =
            parse_language_bundle(&bundle(">=17.0.0", "-"), "17.12.0").unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].definition.keywords["orbital"], "zz-orbital");
        assert_eq!(lang.core_version, "10.1.0");
        assert!(lang.patterns.contains("\"p\""));
        assert!(lang.binding_rules.contains("\"zz\""));
        assert!(lang.program_types.contains("OrbitalValue"));
        assert_eq!(lang.themes.len(), 1);
        assert_eq!(ops.std_version, "8.2.0");
        assert!(ops.operators.contains("arithmetic"));
    }

    #[test]
    fn a_language_bundle_gets_the_same_checks() {
        let err = parse_language_bundle(&bundle(">=18.0.0", "-"), "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INCOMPATIBLE");
        let err = parse_language_bundle(&bundle(">=17.0.0", "zz/not-implemented"), "17.12.0")
            .unwrap_err();
        assert!(err.to_string().contains("zz/not-implemented"), "{err}");
    }

    #[test]
    fn edge_a_bundle_missing_a_part_or_themes_is_invalid() {
        let err = parse_language_bundle(r#"{"core":{}}"#, "17.12.0").unwrap_err();
        assert_eq!(err.code(), "ORB_LANGUAGE_DATA_INVALID");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"]["themes"] = json!([]);
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.themes"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v.as_object_mut().unwrap().remove("vocabularies");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("vocabularies"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["vocabularies"][0]["compiler"] = json!(">=18.0.0");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("lolo-zz"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"].as_object_mut().unwrap().remove("bindingRules");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.bindingRules"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"].as_object_mut().unwrap().remove("programTypes");
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("core.programTypes"), "{err}");
        let mut v: Value = serde_json::from_str(&bundle(">=17.0.0", "-")).unwrap();
        v["core"]["programTypes"] = json!({"OrbitalValue": {"type": "nonsense"}});
        let err = parse_language_bundle(&v.to_string(), "17.12.0").unwrap_err();
        assert!(err.to_string().contains("OrbitalValue"), "{err}");
    }
}
