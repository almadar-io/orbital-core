//! A behavior package's `orb` block: what it must and may declare.

use orbital_core::behavior_packages::discover;
use std::fs;
use std::path::Path;

fn package(root: &Path, extra: Option<(&str, &str)>) {
    let pkg = root.join("node_modules/pkg-a");
    fs::create_dir_all(pkg.join("behaviors/registry")).unwrap();
    for file in [
        "behaviors/registry/x.orb",
        "behaviors/behaviors-registry.json",
        "behaviors/lolo-behavior-contracts.json",
    ] {
        fs::write(pkg.join(file), "{}").unwrap();
    }
    let mut orb = serde_json::json!({
        "prefix": "pa",
        "registry": "behaviors/registry",
        "index": "behaviors/behaviors-registry.json",
        "loloContracts": "behaviors/lolo-behavior-contracts.json",
        "compiler": ">=0.0.0"
    });
    if let Some((key, value)) = extra {
        orb[key] = serde_json::Value::String(value.to_string());
    }
    let manifest = serde_json::json!({ "name": "pkg-a", "version": "1.0.0", "orb": orb });
    fs::write(pkg.join("package.json"), manifest.to_string()).unwrap();
}

#[test]
fn a_package_needs_no_signatures_file() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_published_with_a_signatures_key_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), Some(("signatures", "behaviors/signatures.lolo")));
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_published_with_a_factory_signatures_key_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    package(
        tmp.path(),
        Some(("factorySignatures", "behaviors/registry/factory-signatures.json")),
    );
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}
