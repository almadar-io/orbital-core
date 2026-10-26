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
