//! Discovery of installed behavior packages (npm packages whose `package.json`
//! carries an `orb` block). Node-style walk-up from a working directory, nearest
//! package wins per name; the user store (`~/.orb/node_modules`) is always
//! consulted as the lowest-precedence final rung, so a project's own copy of a
//! package always shadows the user-store install of the same name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

/// An installed behavior package resolved from a `package.json` `orb` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorPackage {
    pub name: String,
    pub version: String,
    pub prefix: String,
    pub root: PathBuf,
    pub registry: RegistrySource,
    /// The package's own `files` allowlist (`package.json`), as written; it bounds which behaviors under
    /// [`Self::registry`] the package ships (see [`Self::ships`]).
    pub files: Vec<String>,
    /// The `.lolo` tree (`orb.source`) the package's contracts are lowered from;
    /// `None` when the manifest declares only a materialized `orb.registry`.
    pub lolo_source: Option<PathBuf>,
    /// The `orb.vectors` snapshot (`dist/vectors.vec`), when declared; a lock entry resolves it to the cache.
    pub vectors: Option<PathBuf>,
    /// Declared paths that `sidecars.lock.json` resolves to the content-addressed
    /// cache instead of the package. The matching field above holds `cache_path`.
    pub sidecars: Vec<LockedSidecar>,
    pub compiler_range: String,
    pub std_peer_range: Option<String>,
    /// `@almadar/core` version range this package declares, `peerDependencies`
    /// taking precedence over `dependencies` when both are present.
    pub core_range: Option<String>,
    pub is_link: bool,
}

impl BehaviorPackage {
    /// Whether the behavior file at `path` is one the package ships: covered by a `files` entry that
    /// lies on the declared registry/source tree, or every file when no entry does (a package that
    /// declares nothing about the tree ships all of it).
    pub fn ships(&self, path: &Path) -> bool {
        let root = self.registry.root();
        let on_tree: Vec<PathBuf> = self
            .files
            .iter()
            .map(|f| self.root.join(f))
            .filter(|f| f.starts_with(root) || root.starts_with(f))
            .collect();
        on_tree.is_empty() || on_tree.iter().any(|f| path.starts_with(f))
    }

    pub fn registry_is_locked(&self) -> bool {
        self.sidecars.iter().any(|sidecar| sidecar.cache_path.as_path() == self.registry.root())
    }
}

/// Where a package's behavior programs come from — decided by its manifest, never
/// by which files exist: a declared `orb.registry` is a materialized tree of
/// lowered `.orb` programs (a published package); without one, `orb.source` names
/// the `.lolo` sources, lowered on read (a workspace package, so an edit is live).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrySource {
    Materialized(PathBuf),
    Live(PathBuf),
}

impl RegistrySource {
    pub fn root(&self) -> &Path {
        match self {
            RegistrySource::Materialized(p) | RegistrySource::Live(p) => p,
        }
    }

    /// The extension of the behavior files under [`Self::root`].
    pub fn extension(&self) -> &'static str {
        match self {
            RegistrySource::Materialized(_) => "orb",
            RegistrySource::Live(_) => "lolo",
        }
    }
}

/// A declared path that `sidecars.lock.json` places in the sidecar store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedSidecar {
    pub rel_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub url: String,
    pub cache_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SidecarLockFile {
    version: u32,
    files: BTreeMap<String, SidecarLockEntry>,
}

#[derive(Debug, Deserialize)]
struct SidecarLockEntry {
    sha256: String,
    bytes: u64,
    url: String,
}

/// `$ALMADAR_SIDECAR_CACHE`, else `~/.almadar/sidecars`.
pub fn sidecar_cache_dir() -> PathBuf {
    match std::env::var_os("ALMADAR_SIDECAR_CACHE") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".almadar")
            .join("sidecars"),
    }
}

/// The host of an `scheme://host/...` url; never the path or query, which carry the capability token.
pub fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    (!host.is_empty()).then_some(host)
}

fn read_sidecar_lock(
    root: &Path,
    cache_dir: &Path,
) -> Result<BTreeMap<String, LockedSidecar>, DiscoverError> {
    let lock_path = root.join("sidecars.lock.json");
    if !lock_path.exists() {
        return Ok(BTreeMap::new());
    }
    let invalid = |reason: String| DiscoverError::InvalidSidecarLock {
        path: lock_path.clone(),
        reason,
    };
    let raw = fs::read_to_string(&lock_path).map_err(|e| DiscoverError::Io {
        path: lock_path.clone(),
        source: e,
    })?;
    let lock: SidecarLockFile =
        serde_json::from_str(&raw).map_err(|e| invalid(format!("malformed: {e}")))?;
    if lock.version != 1 {
        return Err(invalid(format!("unsupported version {}", lock.version)));
    }
    let mut out = BTreeMap::new();
    for (rel_path, entry) in lock.files {
        let hex_ok = entry.sha256.len() == 64
            && entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !hex_ok {
            return Err(invalid(format!(
                "'{rel_path}': sha256 must be 64 lowercase hex characters"
            )));
        }
        if url_host(&entry.url).is_none() {
            return Err(invalid(format!("'{rel_path}': url must be absolute")));
        }
        let cache_path = cache_dir.join(&entry.sha256);
        out.insert(
            rel_path.clone(),
            LockedSidecar {
                rel_path,
                sha256: entry.sha256,
                bytes: entry.bytes,
                url: entry.url,
                cache_path,
            },
        );
    }
    Ok(out)
}

#[derive(Debug, Error)]
pub enum DiscoverError {
    #[error("behavior name '{name}' is declared by both {path_a:?} and {path_b:?}")]
    DuplicateBehaviorName {
        name: String,
        path_a: PathBuf,
        path_b: PathBuf,
    },
    #[error("invalid sidecars.lock.json at {path:?}: {reason}")]
    InvalidSidecarLock { path: PathBuf, reason: String },
    #[error("behavior prefix '{prefix}' is declared by both {package_a} ({path_a:?}) and {package_b} ({path_b:?})")]
    PrefixConflict {
        prefix: String,
        package_a: String,
        path_a: PathBuf,
        package_b: String,
        path_b: PathBuf,
    },
    #[error("package '{package}' declares '{field}' at {path:?}, which does not exist")]
    MissingDeclaredPath {
        package: String,
        field: &'static str,
        path: PathBuf,
    },
    #[error("invalid package.json at {path:?}: {reason}")]
    InvalidPackageJson { path: PathBuf, reason: String },
    #[error("io error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn index_registry_source(
    source: &RegistrySource,
) -> Result<HashMap<String, PathBuf>, DiscoverError> {
    let mut index = HashMap::new();
    let mut visited = HashSet::new();
    let mut stack = vec![source.root().to_path_buf()];
    while let Some(dir) = stack.pop() {
        let io = |source| DiscoverError::Io { path: dir.clone(), source };
        let canonical = fs::canonicalize(&dir).map_err(io)?;
        if !visited.insert(canonical) {
            continue;
        }
        let mut entries = fs::read_dir(&dir)
            .map_err(io)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io)?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| DiscoverError::Io {
                path: path.clone(),
                source,
            })?;
            let is_dir = if file_type.is_symlink() {
                fs::metadata(&path)
                    .map_err(|source| DiscoverError::Io { path: path.clone(), source })?
                    .is_dir()
            } else {
                file_type.is_dir()
            };
            if is_dir {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == source.extension()) {
                if let Some(name) = path.file_stem().and_then(|name| name.to_str()) {
                    insert_registry_path(&mut index, name.to_string(), path)?;
                }
            }
        }
    }
    Ok(index)
}

fn insert_registry_path(
    index: &mut HashMap<String, PathBuf>,
    name: String,
    path: PathBuf,
) -> Result<(), DiscoverError> {
    match index.entry(name) {
        std::collections::hash_map::Entry::Occupied(entry) => Err(DiscoverError::DuplicateBehaviorName {
            name: entry.key().clone(),
            path_a: entry.get().clone(),
            path_b: path,
        }),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(path);
            Ok(())
        }
    }
}

pub fn index_registry_sources<'a>(
    sources: impl IntoIterator<Item = &'a RegistrySource>,
) -> Result<HashMap<String, PathBuf>, DiscoverError> {
    let mut sources: Vec<_> = sources.into_iter().collect();
    sources.sort_by(|a, b| a.root().cmp(b.root()));
    let mut index = HashMap::new();
    for source in sources {
        let mut entries: Vec<_> = index_registry_source(source)?.into_iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (name, path) in entries {
            insert_registry_path(&mut index, name, path)?;
        }
    }
    Ok(index)
}

/// Node-style walk-up from `start_dir`: keeps installed behavior packages,
/// nearest wins per package name. `user_store` is always the final rung —
/// consulted after every ancestor `node_modules`, regardless of whether a
/// `package.json` exists above `start_dir` — so it is the lowest-precedence
/// source a package can be discovered from: a project copy of the same
/// package name always shadows it (nearest-wins dedupe already covers that),
/// and the prefix-conflict check runs over the union of both.
pub fn discover(
    start_dir: &Path,
    user_store: &Path,
) -> Result<Vec<BehaviorPackage>, DiscoverError> {
    discover_with_cache(start_dir, user_store, &sidecar_cache_dir())
}

/// `discover` with an explicit sidecar cache directory. Never downloads.
pub fn discover_with_cache(
    start_dir: &Path,
    user_store: &Path,
    cache_dir: &Path,
) -> Result<Vec<BehaviorPackage>, DiscoverError> {
    let mut found: Vec<BehaviorPackage> = Vec::new();
    let mut names_seen: HashSet<String> = HashSet::new();

    for node_modules in node_modules_dirs(start_dir, user_store) {
        scan_node_modules(&node_modules, cache_dir, &mut found, &mut names_seen)?;
    }

    check_prefix_conflicts(&found)?;
    for package in found.iter().filter(|package| !package.registry_is_locked()) {
        index_registry_source(&package.registry)?;
    }
    Ok(found)
}

/// The shared node-style walk-up rungs: each ancestor's `node_modules`, then
/// the user store's `node_modules` last. Nearest-first order, one definition
/// shared by `discover` and `find_npm_package`.
pub fn node_modules_dirs(
    start_dir: &Path,
    user_store: &Path,
) -> impl Iterator<Item = PathBuf> {
    let mut dirs: Vec<PathBuf> = start_dir
        .ancestors()
        .map(|dir| dir.join("node_modules"))
        .collect();
    dirs.push(user_store.join("node_modules"));
    dirs.into_iter()
}

/// An ordinary installed npm package (no `orb` block required) — resolved
/// through the same node-style walk-up rungs as `discover`, for tools like
/// verify engines that ship as plain npm packages with a `bin` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmPackage {
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub bin: BTreeMap<String, PathBuf>,
    pub is_link: bool,
}

/// Resolves `name` against the node-style walk-up rungs (nearest first, user
/// store last); the first rung where `<dir>/<name>/package.json` exists wins
/// — no falling through to a farther rung on a read/parse failure.
pub fn find_npm_package(name: &str, start_dir: &Path, user_store: &Path) -> Option<NpmPackage> {
    for node_modules in node_modules_dirs(start_dir, user_store) {
        let pkg_dir = node_modules.join(name);
        let package_json_path = pkg_dir.join("package.json");
        if !package_json_path.is_file() {
            continue;
        }
        return build_npm_package(&pkg_dir, &package_json_path);
    }
    None
}

/// Joins a manifest `bin` entry (e.g. `./dist/index.js`) onto `root`,
/// dropping `.` components so the result reads as a normal path. Does not
/// resolve `..` or symlinks — only `Component::CurDir` is stripped.
fn normalize_bin_path(root: &Path, rel: &str) -> PathBuf {
    root.join(rel).components().collect::<PathBuf>()
}

fn build_npm_package(pkg_dir: &Path, package_json_path: &Path) -> Option<NpmPackage> {
    let raw = fs::read_to_string(package_json_path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let obj = value.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let version = obj.get("version")?.as_str()?.to_string();
    let is_link = fs::symlink_metadata(pkg_dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let root = fs::canonicalize(pkg_dir).ok()?;
    let unscoped_name = name.rsplit('/').next().unwrap_or(&name);

    let bin = match obj.get("bin") {
        Some(Value::String(s)) => {
            BTreeMap::from([(unscoped_name.to_string(), normalize_bin_path(&root, s))])
        }
        Some(Value::Object(entries)) => entries
            .iter()
            .filter_map(|(key, v)| {
                v.as_str()
                    .map(|s| (key.clone(), normalize_bin_path(&root, s)))
            })
            .collect(),
        _ => BTreeMap::new(),
    };

    Some(NpmPackage {
        name,
        version,
        root,
        bin,
        is_link,
    })
}

/// `$HOME/.orb` (unix) or `%USERPROFILE%\.orb` (windows).
pub fn user_store_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(|h| PathBuf::from(h).join(".orb"))
}

/// The canonical `@almadar/std` package name — the one behavior package every
/// loader/lowerer/CLI path requires installed, with no embedded fallback
/// (Phase 2).
pub const STD_PACKAGE: &str = "@almadar/std";

/// The command that installs `package`: `orb update` for what every orb needs
/// (`@almadar/core`, `@almadar/std`, which it installs itself), `orb add
/// <package>` for anything else.
pub fn install_command(package: &str) -> String {
    if package == STD_PACKAGE || package == crate::language_data::CORE_PACKAGE {
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

fn scan_node_modules(
    node_modules: &Path,
    cache_dir: &Path,
    found: &mut Vec<BehaviorPackage>,
    names_seen: &mut HashSet<String>,
) -> Result<(), DiscoverError> {
    let entries = match fs::read_dir(node_modules) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(DiscoverError::Io {
                path: node_modules.to_path_buf(),
                source: e,
            })
        }
    };

    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| DiscoverError::Io {
            path: node_modules.to_path_buf(),
            source: e,
        })?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if let Some(scope) = file_name.strip_prefix('@') {
            let _ = scope;
            let scope_entries = match fs::read_dir(&path) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(DiscoverError::Io {
                        path: path.clone(),
                        source: e,
                    })
                }
            };
            for se in scope_entries {
                let se = se.map_err(|e| DiscoverError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                let sub_name = se.file_name().to_string_lossy().to_string();
                candidates.push((format!("{file_name}/{sub_name}"), se.path()));
            }
        } else {
            candidates.push((file_name, path));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    for (_dir_name, dir_path) in candidates {
        let meta = match fs::metadata(&dir_path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(DiscoverError::Io {
                    path: dir_path.clone(),
                    source: e,
                })
            }
        };
        if !meta.is_dir() {
            continue;
        }

        let package_json_path = dir_path.join("package.json");
        if !package_json_path.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&package_json_path).map_err(|e| DiscoverError::Io {
            path: package_json_path.clone(),
            source: e,
        })?;
        let value: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return Err(DiscoverError::InvalidPackageJson {
                    path: package_json_path.clone(),
                    reason: format!("invalid JSON: {e}"),
                })
            }
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let orb = match obj.get("orb") {
            Some(Value::Object(o)) if is_behavior_block(o) => o,
            _ => continue,
        };

        let name = require_str(obj, "name", &package_json_path)?;
        if names_seen.contains(&name) {
            continue;
        }

        let package = build_package(&name, obj, orb, &dir_path, &package_json_path, cache_dir)?;
        names_seen.insert(name);
        found.push(package);
    }

    Ok(())
}

/// The `orb` fields only a behavior package declares. Discovery keys on
/// these, never on the mere presence of an `orb` block: `@almadar/core`
/// carries `orb.language` (language data, see `language_data`) and is not a
/// behavior package. A block with ANY behavior field is a behavior package,
/// so a malformed one (say `registry` without `prefix`) still errors loudly.
const BEHAVIOR_BLOCK_FIELDS: [&str; 3] = ["prefix", "registry", "source"];

fn is_behavior_block(orb: &Map<String, Value>) -> bool {
    BEHAVIOR_BLOCK_FIELDS.iter().any(|f| orb.contains_key(*f))
}

fn build_package(
    name: &str,
    top: &Map<String, Value>,
    orb: &Map<String, Value>,
    dir_path: &Path,
    package_json_path: &Path,
    cache_dir: &Path,
) -> Result<BehaviorPackage, DiscoverError> {
    let version = require_str(top, "version", package_json_path)?;
    let prefix = require_str(orb, "prefix", package_json_path)?;
    let registry_rel = optional_str(orb, "registry", package_json_path)?;
    let source_rel = optional_str(orb, "source", package_json_path)?;
    let compiler_range = require_str(orb, "compiler", package_json_path)?;
    let vectors_rel = optional_str(orb, "vectors", package_json_path)?;
    let std_peer_range = peer_dependency(top, "@almadar/std");
    let core_range =
        peer_dependency(top, "@almadar/core").or_else(|| dependency(top, "@almadar/core"));

    let is_link = fs::symlink_metadata(dir_path)
        .map_err(|e| DiscoverError::Io {
            path: dir_path.to_path_buf(),
            source: e,
        })?
        .file_type()
        .is_symlink();
    let root = fs::canonicalize(dir_path).map_err(|e| DiscoverError::Io {
        path: dir_path.to_path_buf(),
        source: e,
    })?;

    let lock = read_sidecar_lock(&root, cache_dir)?;
    let mut sidecars: Vec<LockedSidecar> = Vec::new();
    let mut declared = |rel: &str| -> (PathBuf, bool) {
        match lock.get(rel) {
            Some(entry) => {
                if !sidecars.iter().any(|s| s.rel_path == entry.rel_path) {
                    sidecars.push(entry.clone());
                }
                (entry.cache_path.clone(), true)
            }
            None => (root.join(rel), false),
        }
    };
    let (registry, registry_locked) = match (&registry_rel, &source_rel) {
        (Some(rel), _) => {
            let (path, locked) = declared(rel);
            (RegistrySource::Materialized(path), locked)
        }
        (None, Some(rel)) => (RegistrySource::Live(root.join(rel)), false),
        (None, None) => {
            return Err(DiscoverError::InvalidPackageJson {
                path: package_json_path.to_path_buf(),
                reason: "declares neither `orb.registry` (materialized) nor `orb.source` (live)"
                    .to_string(),
            })
        }
    };
    let lolo_source = source_rel.as_ref().map(|rel| root.join(rel));
    let vectors_declared: Option<(PathBuf, bool)> = vectors_rel.as_deref().map(declared);
    let vectors: Option<PathBuf> = vectors_declared.as_ref().map(|(p, _)| p.clone());

    let registry_field = match &registry {
        RegistrySource::Materialized(_) => "registry",
        RegistrySource::Live(_) => "source",
    };
    for (field, path, locked) in [
        (
            registry_field,
            &registry.root().to_path_buf(),
            registry_locked,
        ),
    ] {
        if !locked && !path.exists() {
            return Err(DiscoverError::MissingDeclaredPath {
                package: name.to_string(),
                field,
                path: path.clone(),
            });
        }
    }
    if let Some((path, locked)) = &vectors_declared {
        if !locked && !path.exists() {
            return Err(DiscoverError::MissingDeclaredPath {
                package: name.to_string(),
                field: "vectors",
                path: path.clone(),
            });
        }
    }

    Ok(BehaviorPackage {
        name: name.to_string(),
        version,
        prefix,
        root,
        registry,
        files: package_files(top),
        lolo_source,
        vectors,
        sidecars,
        compiler_range,
        std_peer_range,
        core_range,
        is_link,
    })
}

fn optional_str(
    obj: &Map<String, Value>,
    field: &'static str,
    path: &Path,
) -> Result<Option<String>, DiscoverError> {
    match obj.get(field) {
        None => Ok(None),
        Some(_) => require_str(obj, field, path).map(Some),
    }
}

fn require_str(
    obj: &Map<String, Value>,
    field: &'static str,
    path: &Path,
) -> Result<String, DiscoverError> {
    match obj.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(DiscoverError::InvalidPackageJson {
            path: path.to_path_buf(),
            reason: format!("field '{field}' must be a string"),
        }),
        None => Err(DiscoverError::InvalidPackageJson {
            path: path.to_path_buf(),
            reason: format!("missing field '{field}'"),
        }),
    }
}

fn package_files(top: &Map<String, Value>) -> Vec<String> {
    top.get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.as_str().map(|f| f.trim_end_matches('/').to_string()))
        .collect()
}

fn peer_dependency(top: &Map<String, Value>, name: &str) -> Option<String> {
    top.get("peerDependencies")?
        .get(name)?
        .as_str()
        .map(|s| s.to_string())
}

fn dependency(top: &Map<String, Value>, name: &str) -> Option<String> {
    top.get("dependencies")?
        .get(name)?
        .as_str()
        .map(|s| s.to_string())
}

fn check_prefix_conflicts(found: &[BehaviorPackage]) -> Result<(), DiscoverError> {
    let mut by_prefix: HashMap<&str, &BehaviorPackage> = HashMap::new();
    for pkg in found {
        match by_prefix.get(pkg.prefix.as_str()) {
            Some(existing) if existing.name != pkg.name => {
                return Err(DiscoverError::PrefixConflict {
                    prefix: pkg.prefix.clone(),
                    package_a: existing.name.clone(),
                    path_a: existing.root.clone(),
                    package_b: pkg.name.clone(),
                    path_b: pkg.root.clone(),
                });
            }
            Some(_) => {}
            None => {
                by_prefix.insert(pkg.prefix.as_str(), pkg);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "orbital-core-behavior-packages-test-{}-{}-{}",
                std::process::id(),
                n,
                tag
            ));
            fs::create_dir_all(&dir).unwrap();
            TestTempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_package(node_modules: &Path, name: &str, prefix: &str, version: &str) -> PathBuf {
        let pkg_dir = node_modules.join(name);
        fs::create_dir_all(&pkg_dir).unwrap();
        let registry_dir = pkg_dir.join("behaviors/registry");
        fs::create_dir_all(&registry_dir).unwrap();
        fs::write(registry_dir.join(format!("{prefix}-x.orb")), "{}").unwrap();
        fs::create_dir_all(pkg_dir.join("dist")).unwrap();
        fs::write(pkg_dir.join("dist/vectors.vec"), "x").unwrap();

        let pkg_json = serde_json::json!({
            "name": name,
            "version": version,
            "orb": {
                "prefix": prefix,
                "registry": "behaviors/registry",
                "vectors": "dist/vectors.vec",
                "compiler": ">=4.10.0"
            },
            "peerDependencies": { "@almadar/std": ">=1.0.0" }
        });
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::to_string_pretty(&pkg_json).unwrap(),
        )
        .unwrap();
        pkg_dir
    }

    /// `fake_package`, but with an explicit `dependencies`/`peerDependencies`
    /// `@almadar/core` range (either may be `None`) — for `core_range`
    /// precedence tests.
    fn fake_package_with_core(
        node_modules: &Path,
        name: &str,
        prefix: &str,
        version: &str,
        core_dependency: Option<&str>,
        core_peer_dependency: Option<&str>,
    ) -> PathBuf {
        let pkg_dir = fake_package(node_modules, name, prefix, version);
        let raw = fs::read_to_string(pkg_dir.join("package.json")).unwrap();
        let mut pkg_json: Value = serde_json::from_str(&raw).unwrap();
        let obj = pkg_json.as_object_mut().unwrap();
        if let Some(range) = core_dependency {
            obj.insert(
                "dependencies".to_string(),
                serde_json::json!({ "@almadar/core": range }),
            );
        }
        if let Some(range) = core_peer_dependency {
            obj.insert(
                "peerDependencies".to_string(),
                serde_json::json!({ "@almadar/std": ">=1.0.0", "@almadar/core": range }),
            );
        }
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::to_string_pretty(&pkg_json).unwrap(),
        )
        .unwrap();
        pkg_dir
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn write_lock(pkg_dir: &Path, files: Value) {
        let lock = serde_json::json!({ "version": 1, "files": files });
        fs::write(
            pkg_dir.join("sidecars.lock.json"),
            serde_json::to_string(&lock).unwrap(),
        )
        .unwrap();
    }

    fn lock_entry(bytes: u64) -> Value {
        serde_json::json!({
            "sha256": SHA,
            "bytes": bytes,
            "url": "https://storage.example.test/o/x?token=SECRET"
        })
    }

    #[test]
    fn locked_vectors_absent_from_package_pass_and_resolve_to_cache() {
        let tmp = TestTempDir::new("lock-absent");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        fs::remove_file(pkg_dir.join("dist/vectors.vec")).unwrap();
        write_lock(
            &pkg_dir,
            serde_json::json!({ "dist/vectors.vec": lock_entry(7) }),
        );
        let cache = tmp.path().join("cache");
        let found = discover_with_cache(tmp.path(), &tmp.path().join("store"), &cache).unwrap();
        let pkg = &found[0];
        assert_eq!(pkg.vectors, Some(cache.join(SHA)));
        assert_eq!(pkg.sidecars.len(), 1);
        assert_eq!(pkg.sidecars[0].rel_path, "dist/vectors.vec");
        assert_eq!(pkg.sidecars[0].sha256, SHA);
        assert_eq!(pkg.sidecars[0].bytes, 7);
        assert_eq!(pkg.sidecars[0].cache_path, cache.join(SHA));
        assert!(
            !cache.exists(),
            "discovery must never create or fill the cache"
        );
    }

    #[test]
    fn lock_entry_wins_even_when_the_package_also_has_the_file() {
        let tmp = TestTempDir::new("lock-wins");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        write_lock(
            &pkg_dir,
            serde_json::json!({ "dist/vectors.vec": lock_entry(7) }),
        );
        let cache = tmp.path().join("cache");
        let found = discover_with_cache(tmp.path(), &tmp.path().join("store"), &cache).unwrap();
        assert_eq!(found[0].vectors, Some(cache.join(SHA)));
    }

    #[test]
    fn missing_vectors_without_a_lock_entry_still_fails() {
        let tmp = TestTempDir::new("no-lock-missing");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        fs::remove_file(pkg_dir.join("dist/vectors.vec")).unwrap();
        let err = discover_with_cache(tmp.path(), &tmp.path().join("store"), &tmp.path().join("c"))
            .unwrap_err();
        assert!(
            matches!(
                err,
                DiscoverError::MissingDeclaredPath {
                    field: "vectors",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn a_lock_that_does_not_cover_the_missing_path_still_fails() {
        let tmp = TestTempDir::new("lock-other-path");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        fs::remove_file(pkg_dir.join("dist/vectors.vec")).unwrap();
        write_lock(
            &pkg_dir,
            serde_json::json!({ "dist/other.json": lock_entry(1) }),
        );
        let err = discover_with_cache(tmp.path(), &tmp.path().join("store"), &tmp.path().join("c"))
            .unwrap_err();
        assert!(
            matches!(
                err,
                DiscoverError::MissingDeclaredPath {
                    field: "vectors",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn an_old_manifest_with_embeddings_loads_with_no_snapshot() {
        let tmp = TestTempDir::new("old-embeddings");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        let json = fs::read_to_string(pkg_dir.join("package.json"))
            .unwrap()
            .replace(
                r#""vectors": "dist/vectors.vec""#,
                r#""embeddings": ["dist/gone.json", 3]"#,
            );
        fs::write(pkg_dir.join("package.json"), json).unwrap();
        let found =
            discover_with_cache(tmp.path(), &tmp.path().join("store"), &tmp.path().join("c"))
                .unwrap();
        assert_eq!(found[0].vectors, None);
    }

    #[test]
    fn a_manifest_with_neither_key_loads_and_a_non_string_vectors_is_refused() {
        let tmp = TestTempDir::new("neither");
        let nm = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
        let json = fs::read_to_string(pkg_dir.join("package.json")).unwrap();
        let without = json.replace(r#""vectors": "dist/vectors.vec","#, "");
        fs::write(pkg_dir.join("package.json"), without).unwrap();
        let cache = tmp.path().join("c");
        let found = discover_with_cache(tmp.path(), &tmp.path().join("store"), &cache).unwrap();
        assert_eq!(found[0].vectors, None);
        let bad = json.replace(
            r#""vectors": "dist/vectors.vec""#,
            r#""vectors": ["dist/vectors.vec"]"#,
        );
        fs::write(pkg_dir.join("package.json"), bad).unwrap();
        let err = discover_with_cache(tmp.path(), &tmp.path().join("store"), &cache).unwrap_err();
        assert!(
            matches!(err, DiscoverError::InvalidPackageJson { .. }),
            "{err}"
        );
    }

    #[test]
    fn malformed_locks_fail_discovery() {
        let bad: Vec<(&str, String)> = vec![
            ("not json", "{".to_string()),
            ("wrong version", r#"{"version":2,"files":{}}"#.to_string()),
            ("no files", r#"{"version":1}"#.to_string()),
            (
                "short sha",
                r#"{"version":1,"files":{"a":{"sha256":"abc","bytes":1,"url":"https://h.test/x"}}}"#
                    .to_string(),
            ),
            (
                "uppercase sha",
                format!(
                    r#"{{"version":1,"files":{{"a":{{"sha256":"{}","bytes":1,"url":"https://h.test/x"}}}}}}"#,
                    SHA.to_uppercase()
                ),
            ),
            (
                "negative bytes",
                format!(
                    r#"{{"version":1,"files":{{"a":{{"sha256":"{SHA}","bytes":-1,"url":"https://h.test/x"}}}}}}"#
                ),
            ),
            (
                "missing url",
                format!(r#"{{"version":1,"files":{{"a":{{"sha256":"{SHA}","bytes":1}}}}}}"#),
            ),
            (
                "relative url",
                format!(
                    r#"{{"version":1,"files":{{"a":{{"sha256":"{SHA}","bytes":1,"url":"/x"}}}}}}"#
                ),
            ),
        ];
        for (label, body) in bad {
            let tmp = TestTempDir::new("lock-bad");
            let nm = tmp.path().join("node_modules");
            let pkg_dir = fake_package(&nm, "pkg-a", "pa", "1.0.0");
            fs::write(pkg_dir.join("sidecars.lock.json"), body).unwrap();
            let err =
                discover_with_cache(tmp.path(), &tmp.path().join("store"), &tmp.path().join("c"))
                    .unwrap_err();
            assert!(
                matches!(err, DiscoverError::InvalidSidecarLock { .. }),
                "{label}: {err}"
            );
        }
    }

    #[test]
    fn package_without_a_lock_has_no_sidecars() {
        let tmp = TestTempDir::new("no-lock");
        let nm = tmp.path().join("node_modules");
        fake_package(&nm, "pkg-a", "pa", "1.0.0");
        let found =
            discover_with_cache(tmp.path(), &tmp.path().join("store"), &tmp.path().join("c"))
                .unwrap();
        assert!(found[0].sidecars.is_empty());
    }

    #[test]
    fn single_package_all_fields_populated() {
        let tmp = TestTempDir::new("single");
        let node_modules = tmp.path().join("node_modules");
        fake_package(
            &node_modules,
            "@almadar-io/behaviors",
            "almadar-behaviors",
            "0.16.171",
        );
        // Give it a manifest above so the walk does not fall through to a user store.
        fs::write(tmp.path().join("package.json"), "{}").unwrap();

        let user_store = tmp.path().join("unused-user-store");
        let packages = discover(tmp.path(), &user_store).unwrap();

        assert_eq!(packages.len(), 1);
        let pkg = &packages[0];
        assert_eq!(pkg.name, "@almadar-io/behaviors");
        assert_eq!(pkg.version, "0.16.171");
        assert_eq!(pkg.prefix, "almadar-behaviors");
        assert_eq!(pkg.compiler_range, ">=4.10.0");
        assert_eq!(pkg.std_peer_range.as_deref(), Some(">=1.0.0"));
        assert_eq!(pkg.core_range, None);
        assert!(!pkg.is_link);
        assert!(pkg.vectors.as_ref().unwrap().starts_with(&pkg.root));
        assert_eq!(
            pkg.root,
            fs::canonicalize(node_modules.join("@almadar-io/behaviors")).unwrap()
        );
        assert!(pkg.registry.root().exists());
        assert_eq!(pkg.lolo_source, None);
    }

    #[test]
    fn nested_package_at_grandparent() {
        let tmp = TestTempDir::new("nested");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let grandparent = tmp.path();
        let parent = grandparent.join("parent");
        let start = parent.join("child");
        fs::create_dir_all(&start).unwrap();

        fake_package(
            &grandparent.join("node_modules"),
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
        );

        let packages = discover(&start, &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "pkg-a");
    }

    #[test]
    fn nearest_wins_over_farther_duplicate() {
        let tmp = TestTempDir::new("nearest-wins");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let far_node_modules = tmp.path().join("node_modules");
        let near_dir = tmp.path().join("near");
        let near_node_modules = near_dir.join("node_modules");
        fs::create_dir_all(&near_node_modules).unwrap();

        fake_package(&far_node_modules, "pkg-a", "pkg-a-prefix", "1.0.0");
        fake_package(&near_node_modules, "pkg-a", "pkg-a-prefix", "2.0.0");

        let packages = discover(&near_dir, &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].version, "2.0.0");
        assert_eq!(
            packages[0].root,
            fs::canonicalize(near_node_modules.join("pkg-a")).unwrap()
        );
    }

    fn live_package(node_modules: &Path, name: &str, prefix: &str, files: &[&str]) -> PathBuf {
        let pkg_dir = fake_package(node_modules, name, prefix, "1.0.0");
        let lolo = pkg_dir.join("behaviors/lolo");
        for file in files {
            let path = lolo.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let raw = fs::read_to_string(pkg_dir.join("package.json")).unwrap();
        let mut pkg_json: Value = serde_json::from_str(&raw).unwrap();
        let orb = pkg_json["orb"].as_object_mut().unwrap();
        orb.remove("registry");
        orb.insert("source".to_string(), Value::String("behaviors/lolo".to_string()));
        fs::write(pkg_dir.join("package.json"), serde_json::to_string_pretty(&pkg_json).unwrap()).unwrap();
        pkg_dir
    }

    fn discover_in(tmp: &TestTempDir) -> Result<Vec<BehaviorPackage>, DiscoverError> {
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        discover(tmp.path(), &tmp.path().join("unused-user-store"))
    }

    #[test]
    fn unique_behavior_names_in_a_package_pass() {
        let tmp = TestTempDir::new("unique-names");
        live_package(&tmp.path().join("node_modules"), "pkg-a", "pkg-a", &["infra/atoms/std-a.lolo", "ui/atoms/std-b.lolo"]);
        assert_eq!(discover_in(&tmp).unwrap().len(), 1);
    }

    #[test]
    fn same_behavior_name_twice_in_one_package_is_refused_naming_both_files() {
        let tmp = TestTempDir::new("dup-names");
        live_package(
            &tmp.path().join("node_modules"),
            "pkg-a",
            "pkg-a",
            &["infra/atoms/std-calendar.lolo", "ui/core/atoms/std-calendar.lolo"],
        );
        match discover_in(&tmp).unwrap_err() {
            DiscoverError::DuplicateBehaviorName { name, path_a, path_b } => {
                assert_eq!(name, "std-calendar");
                let both = format!("{path_a:?}{path_b:?}");
                assert!(both.contains("infra/atoms/std-calendar.lolo"), "{both}");
                assert!(both.contains("ui/core/atoms/std-calendar.lolo"), "{both}");
            }
            other => panic!("expected DuplicateBehaviorName, got {other:?}"),
        }
    }

    #[test]
    fn same_behavior_name_in_two_different_packages_is_not_a_discovery_error() {
        let tmp = TestTempDir::new("cross-package-names");
        let node_modules = tmp.path().join("node_modules");
        live_package(&node_modules, "pkg-a", "pkg-a", &["atoms/std-shared.lolo"]);
        live_package(&node_modules, "pkg-b", "pkg-b", &["atoms/std-shared.lolo"]);
        assert_eq!(discover_in(&tmp).unwrap().len(), 2);
    }

    #[test]
    fn prefix_conflict_between_two_names() {
        let tmp = TestTempDir::new("prefix-conflict");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        fake_package(&node_modules, "pkg-a", "shared-prefix", "1.0.0");
        fake_package(&node_modules, "pkg-b", "shared-prefix", "1.0.0");

        let err = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap_err();
        match err {
            DiscoverError::PrefixConflict {
                prefix,
                package_a,
                package_b,
                ..
            } => {
                assert_eq!(prefix, "shared-prefix");
                let mut names = [package_a, package_b];
                names.sort();
                assert_eq!(names, ["pkg-a".to_string(), "pkg-b".to_string()]);
            }
            other => panic!("expected PrefixConflict, got {other:?}"),
        }
    }

    #[test]
    fn missing_declared_path_names_the_field() {
        let tmp = TestTempDir::new("missing-path");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        let pkg_dir = fake_package(&node_modules, "pkg-a", "pkg-a-prefix", "1.0.0");
        fs::remove_dir_all(pkg_dir.join("behaviors/registry")).unwrap();

        let err = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap_err();
        match err {
            DiscoverError::MissingDeclaredPath { package, field, .. } => {
                assert_eq!(package, "pkg-a");
                assert_eq!(field, "registry");
            }
            other => panic!("expected MissingDeclaredPath, got {other:?}"),
        }
    }

    #[test]
    fn user_store_fallback_when_no_manifest_above() {
        let tmp = TestTempDir::new("user-store");
        // No package.json anywhere under tmp.
        let start = tmp.path().join("start");
        fs::create_dir_all(&start).unwrap();

        let user_store = tmp.path().join("user-store-root");
        fake_package(
            &user_store.join("node_modules"),
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
        );

        let packages = discover(&start, &user_store).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "pkg-a");
    }

    /// The user store is the final rung unconditionally now — a project dir
    /// WITH its own `package.json` (but no ancestor `node_modules` copy of
    /// the package) still resolves it from the user store.
    #[test]
    fn user_store_found_from_project_with_its_own_manifest() {
        let tmp = TestTempDir::new("user-store-still-consulted");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let start = tmp.path().join("start");
        fs::create_dir_all(&start).unwrap();

        let user_store = tmp.path().join("user-store-root");
        fake_package(
            &user_store.join("node_modules"),
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
        );

        let packages = discover(&start, &user_store).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "pkg-a");
        assert_eq!(
            packages[0].root,
            fs::canonicalize(user_store.join("node_modules/pkg-a")).unwrap()
        );
    }

    /// A project's own copy of a package shadows the user-store install of
    /// the same name — nearest-wins dedupe, user store as the lowest rung.
    #[test]
    fn project_copy_shadows_user_store_copy_of_same_name() {
        let tmp = TestTempDir::new("project-shadows-user-store");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let project_node_modules = tmp.path().join("node_modules");
        fake_package(&project_node_modules, "pkg-a", "pkg-a-prefix", "2.0.0");

        let user_store = tmp.path().join("user-store-root");
        fake_package(
            &user_store.join("node_modules"),
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
        );

        let packages = discover(tmp.path(), &user_store).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].version, "2.0.0");
        assert_eq!(
            packages[0].root,
            fs::canonicalize(project_node_modules.join("pkg-a")).unwrap()
        );
    }

    /// The prefix-conflict check runs over the union of ancestor + user-store
    /// packages, not just the ancestor walk.
    #[test]
    fn prefix_conflict_between_project_package_and_user_store_package() {
        let tmp = TestTempDir::new("prefix-conflict-project-vs-user-store");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        fake_package(
            &tmp.path().join("node_modules"),
            "pkg-a",
            "shared-prefix",
            "1.0.0",
        );

        let user_store = tmp.path().join("user-store-root");
        fake_package(
            &user_store.join("node_modules"),
            "pkg-b",
            "shared-prefix",
            "1.0.0",
        );

        let err = discover(tmp.path(), &user_store).unwrap_err();
        match err {
            DiscoverError::PrefixConflict {
                prefix,
                package_a,
                package_b,
                ..
            } => {
                assert_eq!(prefix, "shared-prefix");
                let mut names = [package_a, package_b];
                names.sort();
                assert_eq!(names, ["pkg-a".to_string(), "pkg-b".to_string()]);
            }
            other => panic!("expected PrefixConflict, got {other:?}"),
        }
    }

    #[test]
    fn core_range_parsed_from_dependencies() {
        let tmp = TestTempDir::new("core-range-deps");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        fake_package_with_core(
            &node_modules,
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
            Some("^10.99.0"),
            None,
        );

        let packages = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].core_range.as_deref(), Some("^10.99.0"));
    }

    #[test]
    fn core_range_peer_dependency_wins_over_dependency() {
        let tmp = TestTempDir::new("core-range-peer-wins");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        fake_package_with_core(
            &node_modules,
            "pkg-a",
            "pkg-a-prefix",
            "1.0.0",
            Some("^9.0.0"),
            Some("^10.99.0"),
        );

        let packages = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].core_range.as_deref(), Some("^10.99.0"));
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_entry_reports_is_link_and_realpath() {
        use std::os::unix::fs::symlink;

        let tmp = TestTempDir::new("symlink");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let real_root = tmp.path().join("real-packages");
        fs::create_dir_all(&real_root).unwrap();
        let real_pkg_dir = fake_package(&real_root, "pkg-a", "pkg-a-prefix", "1.0.0");

        let node_modules = tmp.path().join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        symlink(&real_pkg_dir, node_modules.join("pkg-a")).unwrap();

        let packages = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert!(packages[0].is_link);
        assert_eq!(packages[0].root, fs::canonicalize(&real_pkg_dir).unwrap());
    }

    #[test]
    fn compiler_and_peer_semver_checks() {
        assert!(compiler_satisfies(">=4.10.0", "4.10.0").unwrap());
        assert!(!compiler_satisfies(">=4.10.0", "4.9.9").unwrap());
        assert!(!compiler_satisfies("^4.10", "5.0.0").unwrap());
        assert!(std_peer_satisfies(">=16.0.0", "16.223.0").unwrap());
        assert!(core_satisfies("^10.99.0", "10.99.1").unwrap());
        assert!(!core_satisfies("^10.99.0", "11.0.0").unwrap());
    }

    #[test]
    fn package_json_without_orb_block_is_ignored() {
        let tmp = TestTempDir::new("no-orb");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        let pkg_dir = node_modules.join("plain-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::json!({ "name": "plain-pkg", "version": "1.0.0" }).to_string(),
        )
        .unwrap();

        let packages = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap();
        assert!(packages.is_empty());
    }

    #[test]
    fn scoped_package_is_found() {
        let tmp = TestTempDir::new("scoped");
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let node_modules = tmp.path().join("node_modules");
        fake_package(&node_modules, "@scope/name", "scope-prefix", "1.0.0");

        let packages = discover(tmp.path(), &tmp.path().join("unused-user-store")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "@scope/name");
        assert_eq!(packages[0].prefix, "scope-prefix");
    }

    #[test]
    fn not_installed_message_names_the_package_and_install_command() {
        assert_eq!(
            not_installed_message(STD_PACKAGE),
            "@almadar/std is not installed — run `orb update` (or with `--global`)"
        );
        assert_eq!(
            not_installed_message("@fake-io/behaviors"),
            "@fake-io/behaviors is not installed — run `orb add @fake-io/behaviors` (or with `--global`)"
        );
        assert_eq!(
            install_command(crate::language_data::CORE_PACKAGE),
            "orb update",
            "core is what orb installs itself"
        );
    }

    /// A plain npm package.json (no `orb` block) — `find_npm_package` targets
    /// ordinary npm packages like verify engines, not behavior packages.
    fn fake_npm_package(
        node_modules: &Path,
        name: &str,
        version: &str,
        bin: Option<Value>,
    ) -> PathBuf {
        let pkg_dir = node_modules.join(name);
        fs::create_dir_all(&pkg_dir).unwrap();
        let mut pkg_json = serde_json::json!({ "name": name, "version": version });
        if let Some(bin_value) = bin {
            pkg_json
                .as_object_mut()
                .unwrap()
                .insert("bin".to_string(), bin_value);
        }
        fs::write(
            pkg_dir.join("package.json"),
            serde_json::to_string_pretty(&pkg_json).unwrap(),
        )
        .unwrap();
        pkg_dir
    }

    #[test]
    fn find_npm_package_nearest_wins_over_farther_duplicate() {
        let tmp = TestTempDir::new("npm-nearest-wins");
        let far_node_modules = tmp.path().join("node_modules");
        let near_dir = tmp.path().join("b");
        let near_node_modules = near_dir.join("node_modules");
        fs::create_dir_all(&near_node_modules).unwrap();
        let start = near_dir.join("c");
        fs::create_dir_all(&start).unwrap();

        fake_npm_package(&far_node_modules, "verify-tool", "1.0.0", None);
        fake_npm_package(&near_node_modules, "verify-tool", "2.0.0", None);

        let user_store = tmp.path().join("unused-user-store");
        let pkg = find_npm_package("verify-tool", &start, &user_store).unwrap();
        assert_eq!(pkg.version, "2.0.0");
        assert_eq!(
            pkg.root,
            fs::canonicalize(near_node_modules.join("verify-tool")).unwrap()
        );
    }

    #[test]
    fn find_npm_package_user_store_is_last_rung() {
        let tmp = TestTempDir::new("npm-user-store-last");
        let start = tmp.path().join("start");
        fs::create_dir_all(&start).unwrap();
        let user_store = tmp.path().join("user-store-root");
        fake_npm_package(
            &user_store.join("node_modules"),
            "verify-tool",
            "1.0.0",
            None,
        );

        let pkg = find_npm_package("verify-tool", &start, &user_store).unwrap();
        assert_eq!(pkg.version, "1.0.0");

        // A project copy of the same name shadows the user-store copy.
        let project_node_modules = tmp.path().join("node_modules");
        fake_npm_package(&project_node_modules, "verify-tool", "2.0.0", None);
        let pkg = find_npm_package("verify-tool", &start, &user_store).unwrap();
        assert_eq!(pkg.version, "2.0.0");
        assert_eq!(
            pkg.root,
            fs::canonicalize(project_node_modules.join("verify-tool")).unwrap()
        );
    }

    #[test]
    fn find_npm_package_string_bin_keys_by_unscoped_name() {
        let tmp = TestTempDir::new("npm-string-bin");
        let node_modules = tmp.path().join("node_modules");
        fake_npm_package(
            &node_modules,
            "@almadar-io/verify-tool",
            "1.0.0",
            Some(serde_json::json!("./dist/index.js")),
        );

        let user_store = tmp.path().join("unused-user-store");
        let pkg = find_npm_package("@almadar-io/verify-tool", tmp.path(), &user_store).unwrap();
        assert_eq!(pkg.name, "@almadar-io/verify-tool");
        let root = fs::canonicalize(node_modules.join("@almadar-io/verify-tool")).unwrap();
        assert_eq!(pkg.root, root);
        assert_eq!(pkg.bin.len(), 1);
        let bin_path = pkg.bin.get("verify-tool").unwrap();
        assert_eq!(bin_path, &root.join("./dist/index.js"));
        assert!(
            !bin_path
                .components()
                .any(|c| c == std::path::Component::CurDir),
            "manifest `./` prefix must not survive into the resolved bin path, got {}",
            bin_path.display()
        );
    }

    #[test]
    fn find_npm_package_object_bin_has_all_entries() {
        let tmp = TestTempDir::new("npm-object-bin");
        let node_modules = tmp.path().join("node_modules");
        fake_npm_package(
            &node_modules,
            "multi-bin-pkg",
            "1.0.0",
            Some(serde_json::json!({
                "cmd-a": "./bin/a.js",
                "cmd-b": "./bin/b.js",
            })),
        );

        let user_store = tmp.path().join("unused-user-store");
        let pkg = find_npm_package("multi-bin-pkg", tmp.path(), &user_store).unwrap();
        let root = fs::canonicalize(node_modules.join("multi-bin-pkg")).unwrap();
        assert_eq!(pkg.bin.len(), 2);
        assert_eq!(pkg.bin.get("cmd-a"), Some(&root.join("./bin/a.js")));
        assert_eq!(pkg.bin.get("cmd-b"), Some(&root.join("./bin/b.js")));
    }

    #[test]
    #[cfg(unix)]
    fn find_npm_package_symlinked_entry_reports_is_link_and_realpath() {
        use std::os::unix::fs::symlink;

        let tmp = TestTempDir::new("npm-symlink");
        let real_root = tmp.path().join("real-packages");
        fs::create_dir_all(&real_root).unwrap();
        let real_pkg_dir = fake_npm_package(&real_root, "verify-tool", "1.0.0", None);

        let node_modules = tmp.path().join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        symlink(&real_pkg_dir, node_modules.join("verify-tool")).unwrap();

        let user_store = tmp.path().join("unused-user-store");
        let pkg = find_npm_package("verify-tool", tmp.path(), &user_store).unwrap();
        assert!(pkg.is_link);
        assert_eq!(pkg.root, fs::canonicalize(&real_pkg_dir).unwrap());
    }

    #[test]
    fn find_npm_package_missing_or_unparseable_returns_none() {
        let tmp = TestTempDir::new("npm-missing-unparseable");
        let node_modules = tmp.path().join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        let user_store = tmp.path().join("unused-user-store");

        assert!(find_npm_package("does-not-exist", tmp.path(), &user_store).is_none());

        let bad_pkg_dir = node_modules.join("bad-json-pkg");
        fs::create_dir_all(&bad_pkg_dir).unwrap();
        fs::write(bad_pkg_dir.join("package.json"), "{ not valid json").unwrap();
        assert!(find_npm_package("bad-json-pkg", tmp.path(), &user_store).is_none());
    }
}
