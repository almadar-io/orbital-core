//! A prebuilt native renderer library a shell links into the app it generates: `<bundle>/manifest.json`
//! names the contract, shell version and feature set it was built for, and the SHA-256 of the library
//! under each target directory (`<bundle>/<target>/<library>`). A shell refuses any other bundle.

use crate::json_value::from_json_slice;
use std::prelude::v1::*;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeBundleManifest {
    pub contract_version: u32,
    pub shell_version: String,
    pub feature_set: String,
    /// Target directory (`arm64-v8a`, `aarch64-apple-ios-sim`) → the library's SHA-256, lowercase hex.
    pub libraries: BTreeMap<String, String>,
}

/// What the shell emitting against `contract_version` needs from a bundle.
pub struct NativeBundleExpectation<'a> {
    pub contract_version: u32,
    pub shell_version: &'a str,
    pub feature_set: &'a str,
    pub library: &'a str,
    pub targets: &'a [&'a str],
}

#[derive(Debug, Error)]
pub enum NativeBundleError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: {message}")]
    Manifest { path: PathBuf, message: String },
    #[error("{path}: {message}")]
    Library { path: PathBuf, message: String },
}

/// The file a generated native app lists the remote files it carries in: [`BundledFiles`], beside its program.
pub const BUNDLED_FILES: &str = "bundle-files.json";

/// The remote files a generated native app carries instead of downloading: `orb` fetches each `url` to `path`
/// (relative to [`BUNDLED_FILES`]) when it compiles the app, and the app registers the bytes under `url` at boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundledFiles {
    pub files: Vec<BundledFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundledFile {
    pub url: String,
    pub path: String,
}

impl BundledFiles {
    /// The font files of the themes `program` paints ([`crate::theme::painted_themes`]), at `fonts/<n>`.
    pub fn theme_fonts(program: &crate::schema::OrbitalSchema) -> Self {
        let files = crate::theme::theme_font_files(&crate::theme::painted_themes(program))
            .into_iter()
            .enumerate()
            .map(|(index, url)| BundledFile { url, path: format!("fonts/{index}") })
            .collect();
        Self { files }
    }
}

/// The SHA-256 of `path`'s bytes, lowercase hex.
pub fn file_sha256(path: &Path) -> Result<String, NativeBundleError> {
    let io = |source| NativeBundleError::Io { path: path.to_path_buf(), source };
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut digest = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let size = file.read(&mut chunk).map_err(io)?;
        if size == 0 {
            break;
        }
        digest.update(&chunk[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Each expected target's library path, once the manifest matches `expected` and every library has
/// the recorded SHA-256 and a header `header_ok` accepts (its first bytes).
pub fn verified_libraries(
    bundle: &Path,
    expected: &NativeBundleExpectation<'_>,
    header_ok: impl Fn(&[u8]) -> bool,
) -> Result<Vec<(String, PathBuf)>, NativeBundleError> {
    let manifest_path = bundle.join("manifest.json");
    let bytes = std::fs::read(&manifest_path)
        .map_err(|source| NativeBundleError::Io { path: manifest_path.clone(), source })?;
    let manifest: NativeBundleManifest = from_json_slice(&bytes).map_err(|e| NativeBundleError::Manifest {
        path: manifest_path.clone(),
        message: e.to_string(),
    })?;
    if manifest.contract_version != expected.contract_version
        || manifest.shell_version != expected.shell_version
        || manifest.feature_set != expected.feature_set
    {
        return Err(NativeBundleError::Manifest {
            path: manifest_path,
            message: format!(
                "expected native contract {}, shell {}, feature set {:?}; the bundle carries {}, {}, {:?}",
                expected.contract_version,
                expected.shell_version,
                expected.feature_set,
                manifest.contract_version,
                manifest.shell_version,
                manifest.feature_set
            ),
        });
    }
    let mut out = Vec::new();
    for target in expected.targets {
        let Some(sha) = manifest.libraries.get(*target) else {
            return Err(NativeBundleError::Manifest {
                path: manifest_path.clone(),
                message: format!("no library for target {target}"),
            });
        };
        let library = bundle.join(target).join(expected.library);
        let mut header = [0u8; 64];
        let read = std::fs::File::open(&library)
            .and_then(|mut f| f.read(&mut header))
            .map_err(|source| NativeBundleError::Io { path: library.clone(), source })?;
        if !header_ok(&header[..read]) {
            return Err(NativeBundleError::Library {
                path: library,
                message: format!("not a {target} library"),
            });
        }
        if &file_sha256(&library)? != sha {
            return Err(NativeBundleError::Library {
                path: library,
                message: "SHA-256 does not match the manifest".to_string(),
            });
        }
        out.push(((*target).to_string(), library));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_programs_theme_fonts_are_listed_once_each_at_their_own_path() {
        let program = |theme: serde_json::Value| -> crate::schema::OrbitalSchema {
            crate::json_value::from_json_value(&serde_json::json!({
                "name": "App", "typeContractVersion": 1, "theme": theme,
                "orbitals": [{"name": "Home", "entity": {"name": "Row", "fields": [{"name": "id", "type": "string"}]}, "traits": [], "pages": []}]
            }))
            .unwrap()
        };
        let inline = program(serde_json::json!({
            "name": "bundle-test",
            "tokens": { "typeScale": {
                "displayFile": "https://fonts.test/Display.ttf",
                "bodyFile": "https://fonts.test/Body.ttf",
                "monoFile": "https://fonts.test/Body.ttf"
            } }
        }));
        let bundled = BundledFiles::theme_fonts(&inline);
        assert_eq!(
            bundled.files,
            [
                BundledFile { url: "https://fonts.test/Body.ttf".into(), path: "fonts/0".into() },
                BundledFile { url: "https://fonts.test/Display.ttf".into(), path: "fonts/1".into() },
            ]
        );
        let text = serde_json::to_string(&bundled).unwrap();
        assert_eq!(crate::json_value::from_json_str::<BundledFiles>(&text).unwrap(), bundled);
        let plain = program(serde_json::json!({ "name": "bundle-plain", "tokens": {} }));
        assert!(BundledFiles::theme_fonts(&plain).files.is_empty(), "control: a theme naming no file bundles none");
    }

    fn bundle(root: &Path, feature_set: &str, body: &[u8]) -> PathBuf {
        let dir = root.join("bundle");
        std::fs::create_dir_all(dir.join("t1")).unwrap();
        std::fs::write(dir.join("t1/lib.a"), body).unwrap();
        let manifest = NativeBundleManifest {
            contract_version: 1,
            shell_version: "1.0.0".into(),
            feature_set: feature_set.into(),
            libraries: [("t1".to_string(), file_sha256(&dir.join("t1/lib.a")).unwrap())].into_iter().collect(),
        };
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        dir
    }

    fn expect<'a>(feature_set: &'a str, targets: &'a [&'a str]) -> NativeBundleExpectation<'a> {
        NativeBundleExpectation { contract_version: 1, shell_version: "1.0.0", feature_set, library: "lib.a", targets }
    }

    #[test]
    fn a_matching_bundle_answers_each_target_library_and_any_drift_is_refused() {
        let root = std::env::temp_dir().join(format!("orbital-native-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = bundle(&root, "", b"!<arch>\nbody");
        let ar = |h: &[u8]| h.starts_with(b"!<arch>\n");
        let libs = verified_libraries(&dir, &expect("", &["t1"]), ar).unwrap();
        assert_eq!(libs, vec![("t1".to_string(), dir.join("t1/lib.a"))]);
        assert!(verified_libraries(&dir, &expect("research", &["t1"]), ar).is_err(), "another feature set");
        assert!(verified_libraries(&dir, &expect("", &["t1", "t2"]), ar).is_err(), "a missing target");
        assert!(verified_libraries(&dir, &expect("", &["t1"]), |h: &[u8]| h.starts_with(b"\x7fELF")).is_err(), "a foreign header");
        std::fs::write(dir.join("t1/lib.a"), b"!<arch>\ntampered").unwrap();
        assert!(verified_libraries(&dir, &expect("", &["t1"]), ar).is_err(), "a changed library");
        std::fs::remove_dir_all(root).unwrap();
    }
}
