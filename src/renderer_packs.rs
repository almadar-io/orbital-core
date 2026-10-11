//! Native renderer packs (`docs/Almadar_Behavior_Packages.md` §14, 5c): the
//! generated per-target renderer sources and pattern fixtures live in their own
//! shell repos (`renderers/<name>/`) and ship with each shell release beside its
//! templates — read from disk at emit, never baked into the
//! binary, so a renderer change is a submodule bump, not a Rust rebuild. The
//! pack's `package.json` `orbRenderer` block declares the target, the renderer
//! interface version the sources were generated against, the pattern registry
//! version, the coverage list, and the `sources` / `fixtures` directories. A
//! shell refuses a pack whose interface differs from the one it emits against.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;
use thiserror::Error;

pub const CODE_NOT_INSTALLED: &str = "ORB_RENDERER_PACK_NOT_INSTALLED";
pub const CODE_INCOMPATIBLE: &str = "ORB_RENDERER_PACK_INCOMPATIBLE";
pub const CODE_INVALID: &str = "ORB_RENDERER_PACK_INVALID";

/// The templates directory name of `target`'s pack.
pub fn pack_dir_name(target: &str) -> String {
    format!("almadar-renderers-{target}")
}

/// A shipped templates-directory entry (`ios-shell`, `almadar-renderers-ios`, …):
/// `ORBITAL_SHELLS_DIR`, then a `templates/` (or `shells/`) folder beside the
/// running executable (a distributed shell program or `orb`).
fn shipped_dir(name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ORBITAL_SHELLS_DIR") {
        let path = PathBuf::from(dir).join(name);
        if path.exists() {
            return Some(path);
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for dir_name in ["templates", "shells"] {
        for depth in ["", "../", "../../", "../../../"] {
            let path = exe_dir.join(format!("{depth}{dir_name}/{name}"));
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

/// A shell's scaffold `name`: the shipped copy ([`shipped_dir`]), else the
/// shell repo's own `templates/<name>` (`crate_dir` is the shell crate's
/// `CARGO_MANIFEST_DIR`).
pub fn shell_template(name: &str, crate_dir: &Path) -> PathBuf {
    shipped_dir(name).unwrap_or_else(|| crate_dir.join("templates").join(name))
}

/// One installed pack: every source file (path relative to `sources`) and every
/// fixture (pattern name → JSON), both sorted.
#[derive(Debug, Clone)]
pub struct RendererPack {
    pub package: String,
    pub version: String,
    pub root: PathBuf,
    pub target: String,
    pub renderer_interface: u64,
    pub patterns_registry: String,
    pub coverage: Vec<String>,
    pub sources: Vec<(String, String)>,
    pub fixtures: Vec<(String, String)>,
}

impl RendererPack {
    /// One source file by its path relative to `sources`.
    pub fn source(&self, rel: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|(r, _)| r == rel)
            .map(|(_, c)| c.as_str())
    }

    pub fn covers(&self, pattern: &str) -> bool {
        self.coverage.iter().any(|p| p == pattern)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RendererPackError {
    #[error(
        "renderer pack {package} not found — `orb` ships it beside the binary under `templates/` \
         (reinstall orb, or set ORBITAL_SHELLS_DIR to a folder holding it); {target} output needs it"
    )]
    NotInstalled { package: String, target: String },
    #[error(
        "installed {package}@{version} was generated for renderer interface {found}, but this orb's \
         {target} shell emits against interface {expected} — the pack and the shell must come from the same orb release"
    )]
    Incompatible {
        package: String,
        version: String,
        target: String,
        found: u64,
        expected: u64,
    },
    #[error("installed renderer pack is invalid at {path:?}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

impl RendererPackError {
    pub fn code(&self) -> &'static str {
        match self {
            RendererPackError::NotInstalled { .. } => CODE_NOT_INSTALLED,
            RendererPackError::Incompatible { .. } => CODE_INCOMPATIBLE,
            RendererPackError::Invalid { .. } => CODE_INVALID,
        }
    }

    pub fn needs_install(&self) -> bool {
        matches!(self, RendererPackError::NotInstalled { .. })
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files_under(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Locates `target`'s pack (the shipped copy, else `local`, the shell repo's
/// own `renderers/<name>/`) and reads it.
pub fn find_renderer_pack(
    target: &str,
    expected: u64,
    local: &Path,
) -> Result<RendererPack, RendererPackError> {
    let name = pack_dir_name(target);
    let dir = shipped_dir(&name)
        .or_else(|| local.is_dir().then(|| local.to_path_buf()))
        .ok_or_else(|| RendererPackError::NotInstalled {
            package: name.clone(),
            target: target.to_string(),
        })?;
    load_renderer_pack(target, expected, &dir)
}

/// `target`'s pack for this process, located and read once
/// ([`find_renderer_pack`]); every later call returns the same result.
pub fn installed(
    target: &str,
    expected: u64,
    local: &Path,
) -> Result<&'static RendererPack, RendererPackError> {
    type Cache = HashMap<String, &'static Result<RendererPack, RendererPackError>>;
    static PACKS: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut packs = PACKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let entry = *packs
        .entry(target.to_string())
        .or_insert_with(|| Box::leak(Box::new(find_renderer_pack(target, expected, local))));
    entry.as_ref().map_err(Clone::clone)
}

/// Reads the pack at `root`, refusing one whose `rendererInterface` is not `expected`.
pub fn load_renderer_pack(
    target: &str,
    expected: u64,
    root: &Path,
) -> Result<RendererPack, RendererPackError> {
    let package = pack_dir_name(target);
    let manifest = root.join("package.json");
    if !manifest.is_file() {
        return Err(RendererPackError::NotInstalled {
            package,
            target: target.to_string(),
        });
    }
    let invalid = |path: &Path, reason: String| RendererPackError::Invalid {
        path: path.to_path_buf(),
        reason,
    };
    let raw = fs::read_to_string(&manifest).map_err(|e| invalid(&manifest, e.to_string()))?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|e| invalid(&manifest, format!("invalid JSON: {e}")))?;
    let block = value
        .get("orbRenderer")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(&manifest, "missing `orbRenderer` block".to_string()))?;
    let str_field = |field: &str| {
        block
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid(&manifest, format!("missing `orbRenderer.{field}`")))
    };
    let declared_target = str_field("target")?;
    if declared_target != target {
        return Err(invalid(
            &manifest,
            format!("declares target `{declared_target}`, expected `{target}`"),
        ));
    }
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let found = block
        .get("rendererInterface")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            invalid(
                &manifest,
                "missing `orbRenderer.rendererInterface`".to_string(),
            )
        })?;
    if found != expected {
        return Err(RendererPackError::Incompatible {
            package,
            version,
            target: target.to_string(),
            found,
            expected,
        });
    }
    let coverage = block
        .get("coverage")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(&manifest, "missing `orbRenderer.coverage`".to_string()))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    let sources_dir = root.join(str_field("sources")?);
    let mut paths = Vec::new();
    files_under(&sources_dir, &mut paths).map_err(|e| invalid(&sources_dir, e.to_string()))?;
    paths.sort();
    if paths.is_empty() {
        return Err(invalid(&sources_dir, "no renderer sources".to_string()));
    }
    let sources = paths
        .iter()
        .map(|p| {
            let rel = p
                .strip_prefix(&sources_dir)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            fs::read_to_string(p)
                .map(|c| (rel, c))
                .map_err(|e| invalid(p, e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let fixtures_dir = root.join(str_field("fixtures")?);
    let mut fixture_paths: Vec<PathBuf> = fs::read_dir(&fixtures_dir)
        .map_err(|e| invalid(&fixtures_dir, e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    fixture_paths.sort();
    let fixtures = fixture_paths
        .iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            fs::read_to_string(p)
                .map(|c| (name, c))
                .map_err(|e| invalid(p, e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(RendererPack {
        package,
        version,
        root: root.to_path_buf(),
        target: declared_target,
        renderer_interface: found,
        patterns_registry: str_field("patternsRegistry")?,
        coverage,
        sources,
        fixtures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(root: &Path, target: &str, interface: u64) -> PathBuf {
        let dir = root.join(format!("almadar-renderers-{target}"));
        fs::create_dir_all(dir.join("renderers/atoms")).unwrap();
        fs::create_dir_all(dir.join("fixtures")).unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::json!({
                "name": format!("almadar-renderers-{target}"), "version": "1.2.0", "private": true,
                "orbRenderer": { "target": target, "rendererInterface": interface,
                    "patternsRegistry": "10.1.0", "coverage": ["button", "card"],
                    "sources": "renderers", "fixtures": "fixtures" }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(dir.join("renderers/RenderersCommon.swift"), "// common").unwrap();
        fs::write(dir.join("renderers/atoms/button.swift"), "// button").unwrap();
        fs::write(dir.join("fixtures/button.json"), "{}").unwrap();
        dir
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("orb-packs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_shell_crate_local_pack_is_used_when_no_templates_dir_has_one() {
        let root = tmp("local");
        let local = pack(&root, "zzlocal", 1);
        let p = find_renderer_pack("zzlocal", 1, &local).unwrap();
        assert_eq!(p.target, "zzlocal");
    }

    #[test]
    fn control_a_missing_local_pack_is_not_installed() {
        let root = tmp("nolocal");
        let err = find_renderer_pack("zzlocal", 1, &root.join("renderers/zzlocal")).unwrap_err();
        assert!(matches!(err, RendererPackError::NotInstalled { .. }), "{err:?}");
    }

    #[test]
    fn an_installed_pack_yields_its_sources_fixtures_and_coverage() {
        let root = tmp("ok");
        pack(&root, "ios", 1);
        let p = load_renderer_pack("ios", 1, &root.join("almadar-renderers-ios")).unwrap();
        assert_eq!(p.version, "1.2.0");
        assert_eq!(
            p.sources
                .iter()
                .map(|(r, _)| r.as_str())
                .collect::<Vec<_>>(),
            vec!["RenderersCommon.swift", "atoms/button.swift"]
        );
        assert_eq!(p.fixtures, vec![("button".to_string(), "{}".to_string())]);
        assert!(p.covers("card") && !p.covers("zz"));
    }

    #[test]
    fn a_missing_pack_names_the_install_command() {
        let root = tmp("missing");
        let err =
            load_renderer_pack("android", 1, &root.join("almadar-renderers-android")).unwrap_err();
        assert_eq!(err.code(), "ORB_RENDERER_PACK_NOT_INSTALLED");
        assert!(err.needs_install());
        assert!(err.to_string().contains("ORBITAL_SHELLS_DIR"), "{err}");
    }

    #[test]
    fn an_interface_mismatch_is_refused_naming_both() {
        let root = tmp("iface");
        pack(&root, "ios", 2);
        let err = load_renderer_pack("ios", 1, &root.join("almadar-renderers-ios")).unwrap_err();
        assert_eq!(err.code(), "ORB_RENDERER_PACK_INCOMPATIBLE");
        assert!(
            err.to_string().contains("interface 2") && err.to_string().contains("interface 1"),
            "{err}"
        );
    }

    #[test]
    fn edge_a_pack_declaring_another_target_or_no_block_is_invalid() {
        let root = tmp("invalid");
        let dir = pack(&root, "ios", 1);
        let manifest = fs::read_to_string(dir.join("package.json")).unwrap();
        fs::write(
            dir.join("package.json"),
            manifest.replace("\"target\":\"ios\"", "\"target\":\"android\""),
        )
        .unwrap();
        let err = load_renderer_pack("ios", 1, &root.join("almadar-renderers-ios")).unwrap_err();
        assert_eq!(err.code(), "ORB_RENDERER_PACK_INVALID");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"almadar-renderers-ios","version":"1.0.0"}"#,
        )
        .unwrap();
        let err = load_renderer_pack("ios", 1, &root.join("almadar-renderers-ios")).unwrap_err();
        assert!(err.to_string().contains("orbRenderer"), "{err}");
    }

    #[test]
    fn a_shell_template_falls_back_to_the_shell_repos_templates_dir() {
        let crate_dir = Path::new("/nonexistent/orbital-shell-zz");
        assert_eq!(shell_template("zz-shell-not-shipped", crate_dir), crate_dir.join("templates/zz-shell-not-shipped"));
    }

    #[test]
    fn control_every_shell_repo_carries_its_scaffold_under_templates() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for (repo, name) in [("python", "python-shell"), ("rust", "rust-shell"), ("mobile", "mobile-shell"), ("android", "android-shell"), ("ios", "ios-shell"), ("chrome", "chrome-shell"), ("typescript", "almadar-shell")] {
            let dir = crates.join(format!("orbital-shell-{repo}"));
            if !dir.is_dir() {
                continue;
            }
            assert!(dir.join("templates").join(name).is_dir(), "{repo}: templates/{name}");
            assert!(!dir.join("template").exists() && !dir.join("renderer-pack").exists(), "{repo}: old layout left behind");
        }
    }
}