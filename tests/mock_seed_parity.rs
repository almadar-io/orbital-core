//! The Rust half of the shared mock-seed parity vector.
//!
//! Reads the same vectors as `@almadar/core`'s `mock-parity-vector.test.ts`.
//! Canonical source: `@almadar-io/parity` `fixtures/mock-seed/*.json`; the
//! `almadar-sync rust` bake mirrors them into `tests/baked/mock-seed/` so this
//! repo stays hermetic (CI has no monorepo sibling to climb to). One authored
//! file, not a per-language pair — the point is that both implementations
//! produce the same bytes for the same declared fields.
//!
//! `id` is stripped before comparing: id and timestamp stamping is the caller's,
//! and the two paths use different id schemes by design (`"Member Id 1"` on the
//! interpreted path, `"Member-1"` here). The shared spec is field synthesis.

use orbital_core::runtime::seed::sample_rows;
use orbital_core::schema::{EntityDefinition, SExpression};
use orbital_core::Value;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Vector {
    entity: EntityDefinition,
    count: usize,
    rows: Vec<serde_json::Value>,
}

fn fixture(name: &str) -> Vector {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/baked/mock-seed")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("parity fixture {} unreadable: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parity fixture {name} malformed: {e}"))
}

/// Whole floats collapse to integers so `10.0` and `10` compare equal. Rust
/// numbers are all `f64`; JS has one number type and serializes `10`. That is an
/// encoding difference, not a policy one, and the spec is about values.
fn canonicalize_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.007_199_254_740_992e15 {
                    *value = serde_json::Value::Number((f as i64).into());
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(canonicalize_numbers),
        serde_json::Value::Object(map) => {
            map.iter_mut().for_each(|(_, v)| canonicalize_numbers(v));
        }
        _ => {}
    }
}

/// `Value` -> `serde_json::Value` with `id` dropped, so the comparison is about
/// the policy rather than the caller-owned id scheme.
fn normalize(row: &Value) -> serde_json::Value {
    let mut json =
        serde_json::to_value(SExpression::from(row.clone())).expect("a sample row must serialize");
    if let Some(obj) = json.as_object_mut() {
        obj.remove("id");
    }
    canonicalize_numbers(&mut json);
    json
}

fn assert_matches(name: &str) {
    let Vector {
        entity,
        count,
        rows: expected,
    } = fixture(name);
    let actual: Vec<serde_json::Value> =
        sample_rows(&entity, count).iter().map(normalize).collect();

    assert_eq!(
        actual.len(),
        expected.len(),
        "{name}: row count diverged between the two paths"
    );
    for (i, (got, want)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got,
            want,
            "{name}: row {} diverged.\n  rust: {}\n  js:   {}",
            i + 1,
            serde_json::to_string_pretty(got).unwrap_or_default(),
            serde_json::to_string_pretty(want).unwrap_or_default(),
        );
    }
}

#[test]
fn matches_the_js_owner_for_a_persistent_collection() {
    assert_matches("sample-rows.index.json");
}

#[test]
fn matches_the_js_owner_for_a_runtime_singleton() {
    assert_matches("sample-rows.runtime.index.json");
}

#[derive(serde::Deserialize)]
struct PersonaCase {
    spec: String,
    #[serde(rename = "resolvesToId")]
    resolves_to_id: Option<String>,
    #[serde(default)]
    error: bool,
}

#[derive(serde::Deserialize)]
struct PersonaVector {
    roster: Vec<serde_json::Value>,
    cases: Vec<PersonaCase>,
}

/// The `ALMADAR_PERSONA` resolution semantics against a declared roster — the
/// parity test that replaced the prose mirror comment which used to guard the
/// (now deleted) hardcoded rosters in both languages.
#[test]
fn matches_the_js_owner_for_persona_resolution() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/baked/mock-seed/persona-resolution.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("parity fixture {} unreadable: {e}", path.display()));
    let vector: PersonaVector =
        serde_json::from_str(&raw).expect("persona-resolution fixture malformed");
    let roster: Vec<Value> = vector
        .roster
        .iter()
        .map(|row| serde_json::from_value(row.clone()).expect("roster row must convert"))
        .collect();

    for case in &vector.cases {
        let outcome = orbital_core::runtime::resolve_persona_spec(&case.spec, &roster);
        if case.error {
            assert!(outcome.is_err(), "spec {:?} must be an error", case.spec);
        } else {
            let user = outcome.unwrap_or_else(|e| panic!("spec {:?} must resolve: {e}", case.spec));
            assert_eq!(
                user.get_path("id"),
                Some(&Value::String(
                    case.resolves_to_id
                        .clone()
                        .expect("case needs resolvesToId")
                )),
                "spec {:?} resolved to the wrong persona",
                case.spec
            );
        }
    }
}

#[derive(serde::Deserialize)]
struct DatasetVector {
    schema: orbital_core::OrbitalSchema,
    viewer: serde_json::Value,
    #[serde(rename = "nowMs")]
    now_ms: f64,
    count: usize,
    rows: std::collections::BTreeMap<String, Vec<serde_json::Value>>,
}

fn dataset_vector() -> DatasetVector {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/baked/mock-seed/dataset.seeded.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("parity fixture {} unreadable: {e}", path.display()));
    serde_json::from_str(&raw).expect("dataset fixture malformed")
}

/// The store as the kernel + seeder build it: collections bound, authored instances
/// materialized, then the seeded rows.
fn seed_store(
    vector: &DatasetVector,
    viewer: &Value,
) -> orbital_core::runtime::InMemoryEntityCache {
    use orbital_core::runtime::EntitySource;
    use orbital_core::schema::EntityRef;
    let store = orbital_core::runtime::InMemoryEntityCache::new();
    for orbital in &vector.schema.orbitals {
        for entity_ref in
            std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
        {
            if let EntityRef::Inline(def) = entity_ref {
                if let Some(collection) = &def.collection {
                    store.bind_collection(&def.name, collection);
                }
            }
        }
    }
    let now_iso = orbital_core::runtime::mock_random::iso_from_ms(vector.now_ms);
    for orbital in &vector.schema.orbitals {
        if let EntityRef::Inline(def) = &orbital.entity {
            for row in orbital_core::runtime::instance_rows(
                &def.name,
                def.instances.as_deref().unwrap_or_default(),
                &now_iso,
            ) {
                let _ = store.create(&def.name, &row);
            }
        }
    }
    orbital_core::runtime::seed_mock_dataset(
        &store,
        &vector.schema,
        vector.count,
        Some(viewer),
        vector.now_ms,
    );
    store
}

fn rows_json(
    store: &orbital_core::runtime::InMemoryEntityCache,
    entity: &str,
) -> Vec<serde_json::Value> {
    use orbital_core::runtime::EntitySource;
    store
        .list(entity)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            let mut json =
                serde_json::to_value(SExpression::from((**row).clone())).expect("a row must serialize");
            canonicalize_numbers(&mut json);
            json
        })
        .collect()
}

/// The whole seeded store for one schema is the same bytes as the runtime's (and so the
/// compiled apps' `MockDataService`, a facade over that store).
#[test]
fn matches_the_js_owner_for_a_seeded_dataset() {
    let vector = dataset_vector();
    let viewer: Value = serde_json::from_value(vector.viewer.clone()).expect("viewer must convert");
    let store = seed_store(&vector, &viewer);
    for (entity, expected) in &vector.rows {
        let mut expected = expected.clone();
        expected.iter_mut().for_each(canonicalize_numbers);
        let actual = rows_json(&store, entity);
        assert_eq!(actual.len(), expected.len(), "{entity}: row count diverged");
        for (i, (got, want)) in actual.iter().zip(expected.iter()).enumerate() {
            assert_eq!(
                got,
                want,
                "{entity}: row {} diverged.\n  rust: {}\n  js:   {}",
                i + 1,
                serde_json::to_string(got).unwrap_or_default(),
                serde_json::to_string(want).unwrap_or_default(),
            );
        }
    }
}

#[test]
fn control_another_viewer_changes_only_owner_stamps() {
    let vector = dataset_vector();
    let mut other = vector.viewer.clone();
    other["id"] = serde_json::Value::String("Person Id 5".to_string());
    other["role"] = serde_json::Value::String("agent".to_string());
    let viewer: Value = serde_json::from_value(other).expect("viewer must convert");
    let store = seed_store(&vector, &viewer);
    let mut tags = vector.rows["Tag"].clone();
    tags.iter_mut().for_each(canonicalize_numbers);
    assert_eq!(rows_json(&store, "Tag"), tags);
    let mut tickets = vector.rows["Ticket"].clone();
    tickets.iter_mut().for_each(canonicalize_numbers);
    assert_ne!(rows_json(&store, "Ticket"), tickets);
}
