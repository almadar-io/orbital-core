//! A behavior package's `orb` block: what it must and may declare.

use orbital_core_base::behavior_packages::discover;
use std::fs;
use std::path::Path;

fn package(root: &Path, extra: Option<(&str, &str)>) {
    let pkg = root.join("node_modules/pkg-a");
    fs::create_dir_all(pkg.join("behaviors/registry")).unwrap();
    fs::write(pkg.join("behaviors/registry/x.orb"), "{}").unwrap();
    let mut orb = serde_json::json!({
        "prefix": "pa",
        "registry": "behaviors/registry",
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
fn a_package_published_with_an_index_key_still_loads_without_its_file() {
    let tmp = tempfile::tempdir().unwrap();
    package(
        tmp.path(),
        Some(("index", "behaviors/behaviors-registry.json")),
    );
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_ships_what_its_files_allowlist_covers_of_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let pkg_dir = tmp.path().join("node_modules/pkg-a");
    fs::create_dir_all(pkg_dir.join("behaviors/registry/shipped")).unwrap();
    fs::create_dir_all(pkg_dir.join("behaviors/registry/internal")).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(pkg_dir.join("package.json")).unwrap()).unwrap();
    let mut manifest = manifest;
    manifest["files"] = serde_json::json!(["dist", "behaviors/registry/shipped/"]);
    fs::write(pkg_dir.join("package.json"), manifest.to_string()).unwrap();
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    let pkg = &found[0];
    let registry = pkg.registry.root();
    assert!(pkg.ships(&registry.join("shipped/a.orb")));
    assert!(!pkg.ships(&registry.join("internal/b.orb")));
    assert!(!pkg.ships(&registry.join("x.orb")));
}

#[test]
fn a_package_whose_files_say_nothing_of_the_registry_ships_all_of_it() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let pkg_dir = tmp.path().join("node_modules/pkg-a");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(pkg_dir.join("package.json")).unwrap()).unwrap();
    manifest["files"] = serde_json::json!(["dist"]);
    fs::write(pkg_dir.join("package.json"), manifest.to_string()).unwrap();
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert!(found[0].ships(&found[0].registry.root().join("any/dir/c.orb")));
}

#[test]
fn a_package_published_with_a_signatures_key_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    package(
        tmp.path(),
        Some(("signatures", "behaviors/signatures.lolo")),
    );
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_published_with_a_factory_signatures_key_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    package(
        tmp.path(),
        Some((
            "factorySignatures",
            "behaviors/registry/factory-signatures.json",
        )),
    );
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_needs_no_contracts_file() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].lolo_source, None);
}

#[test]
fn a_package_published_with_a_contracts_key_still_loads_without_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    package(
        tmp.path(),
        Some(("loloContracts", "behaviors/lolo-behavior-contracts.json")),
    );
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found[0].prefix, "pa");
}

#[test]
fn a_package_declaring_source_exposes_its_lolo_tree() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), Some(("source", "behaviors/lolo")));
    fs::create_dir_all(tmp.path().join("node_modules/pkg-a/behaviors/lolo")).unwrap();
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert!(found[0]
        .lolo_source
        .as_ref()
        .unwrap()
        .ends_with("behaviors/lolo"));
}

#[test]
fn duplicate_materialized_behavior_names_are_refused_with_both_paths() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let registry = tmp.path().join("node_modules/pkg-a/behaviors/registry");
    fs::create_dir_all(registry.join("nested")).unwrap();
    fs::write(registry.join("nested/x.orb"), "{}").unwrap();
    let error = discover(tmp.path(), &tmp.path().join("store"))
        .expect_err("duplicate behavior names must not silently shadow");
    let message = error.to_string();
    assert!(
        message.contains("pkg-a") && message.contains("x.orb"),
        "{message}"
    );
    assert!(
        message.contains(&registry.join("x.orb").display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(&registry.join("nested/x.orb").display().to_string()),
        "{message}"
    );
}

#[test]
fn duplicate_live_behavior_names_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let pkg = tmp.path().join("node_modules/pkg-a");
    let path = pkg.join("package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest["orb"].as_object_mut().unwrap().remove("registry");
    manifest["orb"]["source"] = serde_json::json!("behaviors/lolo");
    fs::write(path, manifest.to_string()).unwrap();
    for topic in ["infra", "ui"] {
        fs::create_dir_all(pkg.join("behaviors/lolo").join(topic)).unwrap();
        fs::write(
            pkg.join("behaviors/lolo")
                .join(topic)
                .join("std-calendar.lolo"),
            "app Calendar {}",
        )
        .unwrap();
    }
    let error = discover(tmp.path(), &tmp.path().join("store"))
        .expect_err("live duplicate names must not silently shadow");
    assert!(error.to_string().contains("std-calendar"), "{error}");
}

#[test]
fn distinct_names_in_nested_registry_directories_are_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    package(tmp.path(), None);
    let registry = tmp.path().join("node_modules/pkg-a/behaviors/registry");
    fs::create_dir_all(registry.join("nested")).unwrap();
    fs::write(registry.join("nested/y.orb"), "{}").unwrap();
    let found = discover(tmp.path(), &tmp.path().join("store")).unwrap();
    assert_eq!(found.len(), 1);
}

fn peer_package(root: &Path, live: bool, name: &str) {
    let pkg = root.join("node_modules/pkg-b");
    let tree = if live {
        "behaviors/lolo"
    } else {
        "behaviors/registry"
    };
    fs::create_dir_all(pkg.join(tree)).unwrap();
    fs::write(
        pkg.join(tree)
            .join(format!("{name}.{}", if live { "lolo" } else { "orb" })),
        "{}",
    )
    .unwrap();
    let mut orb = serde_json::json!({ "prefix": "pb", "compiler": ">=0.0.0" });
    orb[if live { "source" } else { "registry" }] = serde_json::json!(tree);
    fs::write(
        pkg.join("package.json"),
        serde_json::json!({ "name": "pkg-b", "version": "1.0.0", "orb": orb }).to_string(),
    )
    .unwrap();
}

#[test]
fn the_same_trailing_name_in_two_installed_packages_is_legal() {
    // `<prefix>/<name>` tells them apart; only the bare `std/behaviors/<name>` spelling is ambiguous
    // (`ORB_BEHAVIOR_AMBIGUOUS`, docs/Almadar_Behavior_Packages.md).
    for live in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        package(tmp.path(), None);
        peer_package(tmp.path(), live, "x");
        assert_eq!(
            discover(tmp.path(), &tmp.path().join("store"))
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn distinct_trailing_names_across_installed_packages_are_allowed() {
    for live in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        package(tmp.path(), None);
        peer_package(tmp.path(), live, "y");
        assert_eq!(
            discover(tmp.path(), &tmp.path().join("store"))
                .unwrap()
                .len(),
            2
        );
    }
}
