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
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[cfg(feature = "std")]
use std::fs;
#[cfg(feature = "std")]
use std::path::{Path, PathBuf};
/// A bare-metal host has no filesystem; an error's location is its text.
#[cfg(not(feature = "std"))]
type PathBuf = String;
use crate::sync::OnceLock;

use serde_json::Value;
use thiserror::Error;

#[cfg(feature = "std")]
use crate::behavior_packages::{find_npm_package, user_store_dir};

/// The package that ships the pattern vocabulary.
pub const CORE_PACKAGE: &str = "@almadar/core";
/// The canonical `@almadar/std` package name — the one behavior package every
/// loader/lowerer/CLI path requires installed, with no embedded fallback
/// (Phase 2). It ships the canonical operator table.
pub const STD_PACKAGE: &str = "@almadar/std";

pub fn compiler_satisfies(range: &str, running_version: &str) -> Result<bool, semver::Error> {
    semver_satisfies(range, running_version)
}

pub fn std_peer_satisfies(range: &str, std_version: &str) -> Result<bool, semver::Error> {
    semver_satisfies(range, std_version)
}

pub fn core_satisfies(range: &str, core_version: &str) -> Result<bool, semver::Error> {
    semver_satisfies(range, core_version)
}

fn semver_satisfies(range: &str, version: &str) -> Result<bool, semver::Error> {
    let req = semver::VersionReq::parse(range)?;
    let ver = semver::Version::parse(version)?;
    Ok(req.matches(&ver))
}

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
    /// The endpoint protocol table (`orbLanguage.protocols`): the rows `crate::protocols` reads.
    pub protocols: String,
    /// The theme presets (`orbLanguage.themes`, a directory of one
    /// `ThemeDefinition` per `.json` file), sorted by file name.
    pub themes: Vec<String>,
}

impl LanguageData {
    /// Where the data was read from, as text for a message.
    pub fn root_text(&self) -> String {
        #[cfg(feature = "std")]
        return self.root.display().to_string();
        #[cfg(not(feature = "std"))]
        return self.root.clone();
    }
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
    #[error(
        "installed {package}@{version} declares {entry} this orb cannot read ({reason}) \
         — upgrade orb, or install a {package} this orb supports"
    )]
    Unreadable {
        package: &'static str,
        version: String,
        entry: String,
        reason: String,
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
            | LanguageDataError::Unreadable { .. }
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

/// The command that installs `package`: `orb update` for what every orb needs
/// (`@almadar/core`, `@almadar/std`, which it installs itself), `orb add
/// <package>` for anything else.
pub fn install_command(package: &str) -> String {
    if package == STD_PACKAGE || package == CORE_PACKAGE {
        "orb update".to_string()
    } else {
        format!("orb add {package}")
    }
}

/// The shared "not installed" message for a missing package — `orbital-compiler`'s
/// loader, `orbital-lolo`'s contracts loader, the language-data loader and
/// `orbital-cli` all report a missing package through this one function.
pub fn not_installed_message(package: &str) -> String {
    format!(
        "{package} is not installed — run `{}` (or with `--global`)",
        install_command(package)
    )
}

pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(feature = "std")]
/// Resolves `@almadar/core` from `start_dir` / `user_store` and reads the
/// language data it declares. Pure: touches no global state.
pub fn load_language_data(
    start_dir: &Path,
    user_store: &Path,
) -> Result<LanguageData, LanguageDataError> {
    load_language_data_for(start_dir, user_store, running_version())
}

#[cfg(feature = "std")]
pub fn load_language_data_for(
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
    let protocols = block.read("protocols")?;
    crate::protocols::parse(&protocols).map_err(|reason| LanguageDataError::Invalid {
        path: block.declared_path("protocols").unwrap_or_default(),
        reason,
    })?;
    Ok(LanguageData {
        patterns: block.read("patterns")?,
        component_mapping: block.read("componentMapping")?,
        event_contracts: block.read("eventContracts")?,
        services: block.read("services")?,
        binding_rules: block.read("bindingRules")?,
        program_types,
        protocols,
        themes: block.read_json_dir("themes")?,
        core_version: block.pkg.version,
        root: block.pkg.root,
    })
}

#[cfg(feature = "std")]
/// Resolves `@almadar/std` from `start_dir` / `user_store` and reads the
/// operator table it declares; the evaluator crate certifies it
/// ([`certify_operator_data`]). Pure: touches no global state.
pub fn load_operator_data(
    start_dir: &Path,
    user_store: &Path,
) -> Result<OperatorData, LanguageDataError> {
    load_operator_data_for(start_dir, user_store, running_version())
}

#[cfg(feature = "std")]
pub fn load_operator_data_for(
    start_dir: &Path,
    user_store: &Path,
    running: &str,
) -> Result<OperatorData, LanguageDataError> {
    let block = read_language_block(STD_PACKAGE, start_dir, user_store, running)?;
    let operators = block.read("operators")?;
    let data = OperatorData {
        operators,
        std_version: block.pkg.version,
        root: block.pkg.root,
    };
    check_operator_entries(&data)?;
    Ok(data)
}

/// Every category and operator entry reads as this binary's registry types, so a table from a newer std (a new type
/// kind, a new required field) is refused here, naming the entry, instead of panicking where the registry is first read.
pub fn check_operator_entries(data: &OperatorData) -> Result<(), LanguageDataError> {
    use crate::language::registry::{EmbeddedCategory, EmbeddedOperator};
    let invalid = |reason: String| LanguageDataError::Invalid {
        path: data.root.clone(),
        reason,
    };
    let doc: Value = serde_json::from_str(&data.operators).map_err(|e| invalid(format!("invalid JSON: {e}")))?;
    let section = |name: &str| {
        doc.get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| invalid(format!("missing `{name}`")))
    };
    let unreadable = |entry: String, e: serde_json::Error| LanguageDataError::Unreadable {
        package: STD_PACKAGE,
        version: data.std_version.clone(),
        entry,
        reason: e.to_string(),
    };
    for (name, meta) in section("categories")? {
        serde_json::from_value::<EmbeddedCategory>(meta.clone()).map_err(|e| unreadable(format!("category `{name}`"), e))?;
    }
    let operators = section("operators")?;
    for (name, meta) in operators {
        serde_json::from_value::<EmbeddedOperator>(meta.clone()).map_err(|e| unreadable(format!("operator `{name}`"), e))?;
    }
    for (name, field) in [("emit", "busExcluded"), ("program/dispatch", "targetOptions")] {
        if operators.get(name).and_then(|meta| meta.get(field)).and_then(Value::as_array).is_none() {
            return Err(LanguageDataError::Unreadable {
                package: STD_PACKAGE,
                version: data.std_version.clone(),
                entry: format!("operator `{name}`"),
                reason: format!("declares no `{field}` list"),
            });
        }
    }
    Ok(())
}

/// Operators whose canonical category targets `rust` but that the evaluator
/// cannot resolve, sorted.
pub fn unimplemented_rust_operators(table: &str, implemented: OperatorProbe) -> Result<Vec<String>, String> {
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
        .filter(|(name, _)| !implemented(name))
        .map(|(name, _)| name.clone())
        .collect();
    missing.sort();
    Ok(missing)
}

#[cfg(feature = "std")]
pub struct LanguageBlock {
    pkg: crate::behavior_packages::NpmPackage,
    block: serde_json::Map<String, Value>,
    package_json: PathBuf,
}

#[cfg(feature = "std")]
impl LanguageBlock {
    pub fn declared_path(&self, field: &str) -> Option<PathBuf> {
        self.block
            .get(field)
            .and_then(Value::as_str)
            .map(|rel| self.pkg.root.join(rel))
    }

    pub fn read_json_dir(&self, field: &str) -> Result<Vec<String>, LanguageDataError> {
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

    pub fn read(&self, field: &str) -> Result<String, LanguageDataError> {
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

#[cfg(feature = "std")]
/// Finds `package` and returns its range-checked top-level `orbLanguage` block.
pub fn read_language_block(
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

pub static LANGUAGE_DATA: OnceLock<LanguageData> = OnceLock::new();

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
    #[cfg(not(feature = "std"))]
    {
        Err(LanguageDataError::NotInstalled { package: CORE_PACKAGE })
    }
    #[cfg(feature = "std")]
    {
        let cwd = std::env::current_dir().map_err(|e| LanguageDataError::Invalid {
            path: PathBuf::new(),
            reason: format!("cannot read the working directory: {e}"),
        })?;
        let data = load_language_data(&cwd, &user_store_dir().unwrap_or_default())?;
        Ok(LANGUAGE_DATA.get_or_init(|| data))
    }
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
/// services, bindingRules, programTypes, protocols, themes: [ThemeDefinition…] }, std: { version, compiler,
/// operators }, vocabularies: [{ package, version, compiler, vocabulary }] }`.
/// The same checks apply: each `compiler` range must accept this binary and the
/// language packs must be unambiguous; the evaluator crate's wrapper also
/// certifies the operators. The first install wins; a later call leaves the installed data
/// unchanged.
pub fn install_language_bundle(bundle: &str) -> Result<(), LanguageDataError> {
    let (language, ops, packs) = parse_language_bundle(bundle, running_version())?;
    let _ = install_language_data(language);
    let _ = install_operator_data(ops);
    crate::i18n::packs::install_vocabulary_packs(packs)?;
    Ok(())
}

/// The installed language as the bundle [`install_language_bundle`] takes: the Rust twin of `readLanguageBundle`, for a
/// host with no `node_modules` (a generated Python app) that ships the language beside its program. Each `compiler`
/// range is exactly this binary, since the data was just checked against it.
pub fn language_bundle_json() -> Result<String, LanguageDataError> {
    let language = language_data()?;
    let operators = operator_data()?;
    let packs = crate::i18n::packs::vocabulary_packs()?;
    let invalid = |reason: String| LanguageDataError::Invalid {
        path: PathBuf::from("<language bundle>"),
        reason,
    };
    let part = |field: &str, text: &str| {
        serde_json::from_str::<Value>(text).map_err(|e| invalid(format!("`{field}` is not JSON: {e}")))
    };
    let themes = language
        .themes
        .iter()
        .map(|theme| part("themes", theme))
        .collect::<Result<Vec<_>, _>>()?;
    let compiler = format!("={}", running_version());
    let vocabularies = packs
        .iter()
        .map(|pack| {
            serde_json::to_value(&pack.definition)
                .map(|vocabulary| {
                    serde_json::json!({
                        "package": pack.package,
                        "version": pack.version,
                        "compiler": pack.compiler,
                        "vocabulary": vocabulary,
                    })
                })
                .map_err(|e| invalid(format!("{}: {e}", pack.package)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = serde_json::json!({
        "core": {
            "version": language.core_version,
            "compiler": compiler,
            "patterns": part("patterns", &language.patterns)?,
            "componentMapping": part("componentMapping", &language.component_mapping)?,
            "eventContracts": part("eventContracts", &language.event_contracts)?,
            "services": part("services", &language.services)?,
            "bindingRules": part("bindingRules", &language.binding_rules)?,
            "programTypes": part("programTypes", &language.program_types)?,
            "protocols": part("protocols", &language.protocols)?,
            "themes": themes,
        },
        "std": {
            "version": operators.std_version,
            "compiler": compiler,
            "operators": part("operators", &operators.operators)?,
        },
        "vocabularies": vocabularies,
    });
    serde_json::to_string(&bundle).map_err(|e| invalid(e.to_string()))
}

pub fn parse_language_bundle(
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
    let program_types = text(core, "programTypes", "core")?;
    crate::language::program_types::parse(&program_types)
        .map_err(|reason| invalid(format!("`core.programTypes`: {reason}")))?;
    let protocols = text(core, "protocols", "core")?;
    crate::protocols::parse(&protocols).map_err(|reason| invalid(format!("`core.protocols`: {reason}")))?;
    let language = LanguageData {
        core_version: version(core, "core")?,
        root: PathBuf::new(),
        patterns: text(core, "patterns", "core")?,
        component_mapping: text(core, "componentMapping", "core")?,
        event_contracts: text(core, "eventContracts", "core")?,
        services: text(core, "services", "core")?,
        binding_rules: text(core, "bindingRules", "core")?,
        program_types,
        protocols,
        themes,
    };
    let ops = OperatorData {
        std_version: version(std, "std")?,
        root: PathBuf::new(),
        operators,
    };
    check_operator_entries(&ops)?;
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

pub fn bundle_pack(entry: &Value, running: &str) -> Result<crate::i18n::packs::VocabularyPack, String> {
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

pub static OPERATOR_DATA: OnceLock<OperatorData> = OnceLock::new();

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
    #[cfg(not(feature = "std"))]
    {
        Err(LanguageDataError::NotInstalled { package: STD_PACKAGE })
    }
    #[cfg(feature = "std")]
    {
        let cwd = std::env::current_dir().map_err(|e| LanguageDataError::Invalid {
            path: PathBuf::new(),
            reason: format!("cannot read the working directory: {e}"),
        })?;
        let data = load_operator_data(&cwd, &user_store_dir().unwrap_or_default())?;
        Ok(OPERATOR_DATA.get_or_init(|| data))
    }
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

pub type OperatorProbe = fn(&str) -> bool;

/// Refuses an operator table naming a rust-target operator `implemented` rejects.
pub fn certify_operator_data(data: &OperatorData, implemented: OperatorProbe) -> Result<(), LanguageDataError> {
    let missing = unimplemented_rust_operators(&data.operators, implemented).map_err(|reason| {
        LanguageDataError::Invalid {
            path: data.root.clone(),
            reason,
        }
    })?;
    if missing.is_empty() {
        return Ok(());
    }
    Err(LanguageDataError::Unimplemented {
        package: STD_PACKAGE,
        version: data.std_version.clone(),
        operators: missing,
    })
}
