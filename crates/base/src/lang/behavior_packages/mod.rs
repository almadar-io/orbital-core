//! Discovery of installed behavior packages (npm packages whose `package.json`
//! carries an `orb` block). Node-style walk-up from a working directory, nearest
//! package wins per name; the user store (`~/.orb/node_modules`) is always
//! consulted as the lowest-precedence final rung, so a project's own copy of a
//! package always shadows the user-store install of the same name.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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
    #[error("{package} declares `orb.compiler` {range:?}, which orb {running} does not satisfy (ORB_LANGUAGE_DATA_INCOMPATIBLE) — install a {package} whose range accepts this orb, or upgrade orb")]
    IncompatibleCompiler {
        package: String,
        range: String,
        running: String,
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
///
/// Memoized per process for each `(start_dir, user_store, cache_dir)`: a repeat call answers the remembered packages
/// while every directory the walk listed and every manifest it kept still has the modification time it had (see
/// [`DiscoveryStamp`]), so a long-lived host still sees a package added, removed or re-declared.
pub fn discover_with_cache(
    start_dir: &Path,
    user_store: &Path,
    cache_dir: &Path,
) -> Result<Vec<BehaviorPackage>, DiscoverError> {
    use std::sync::{Mutex, OnceLock};
    type Key = (PathBuf, PathBuf, PathBuf);
    static MEMO: OnceLock<Mutex<HashMap<Key, (DiscoveryStamp, Vec<BehaviorPackage>)>>> = OnceLock::new();
    let memo = MEMO.get_or_init(Default::default);
    let key = (start_dir.to_path_buf(), user_store.to_path_buf(), cache_dir.to_path_buf());
    if let Some((stamp, packages)) = memo.lock().ok().and_then(|m| m.get(&key).cloned()) {
        if stamp.is_current() {
            return Ok(packages);
        }
    }
    let packages = discover_uncached(start_dir, user_store, cache_dir)?;
    let stamp = DiscoveryStamp::of(start_dir, user_store, &packages);
    if let Ok(mut m) = memo.lock() {
        m.insert(key, (stamp, packages.clone()));
    }
    Ok(packages)
}

/// The modification times a discovery depended on: each `node_modules` rung, each `@scope` directory in it, and each
/// kept package's `package.json` and `sidecars.lock.json` (`None` while absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryStamp(Vec<(PathBuf, Option<std::time::SystemTime>)>);

impl DiscoveryStamp {
    fn modified(path: &Path) -> Option<std::time::SystemTime> {
        fs::metadata(path).and_then(|m| m.modified()).ok()
    }

    pub fn of(start_dir: &Path, user_store: &Path, packages: &[BehaviorPackage]) -> Self {
        let mut entries = Vec::new();
        for rung in node_modules_dirs(start_dir, user_store) {
            if let Ok(listing) = fs::read_dir(&rung) {
                for entry in listing.flatten() {
                    if entry.file_name().to_string_lossy().starts_with('@') {
                        let scope = entry.path();
                        entries.push((scope.clone(), Self::modified(&scope)));
                    }
                }
            }
            let modified = Self::modified(&rung);
            entries.push((rung, modified));
        }
        for package in packages {
            for file in ["package.json", "sidecars.lock.json"] {
                let path = package.root.join(file);
                let modified = Self::modified(&path);
                entries.push((path, modified));
            }
        }
        DiscoveryStamp(entries)
    }

    pub fn is_current(&self) -> bool {
        self.0.iter().all(|(path, modified)| Self::modified(path) == *modified)
    }
}

fn discover_uncached(
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

pub use crate::language_data::STD_PACKAGE;

pub use crate::language_data::{install_command, not_installed_message};

pub use crate::language_data::{compiler_satisfies, core_satisfies, std_peer_satisfies};

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
    let running = crate::language_data::running_version();
    match compiler_satisfies(&compiler_range, running) {
        Ok(true) => {}
        Ok(false) => {
            return Err(DiscoverError::IncompatibleCompiler {
                package: name.to_string(),
                range: compiler_range,
                running: running.to_string(),
            })
        }
        Err(e) => {
            return Err(DiscoverError::InvalidPackageJson {
                path: package_json_path.to_path_buf(),
                reason: format!("`orb.compiler` {compiler_range:?} is not a semver range: {e}"),
            })
        }
    }
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
mod tests;
