//! Cross-path fixture (W2-J1): pins `derive_id` byte-for-byte against the JS
//! twin (`@almadar/core`'s `deriveId` in `src/types/identity.ts`). Both sides
//! read `tests/fixtures/derive_id.json` and must produce identical output.

use std::fs;
use std::path::PathBuf;

use orbital_core_base::identity::derive_id;
use serde::Deserialize;

#[derive(Deserialize)]
struct FixtureCase {
    parent: String,
    discriminator: String,
    expected: String,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("tests/fixtures/derive_id.json")
}

#[test]
fn derive_id_matches_the_js_twin_fixture() {
    let raw = fs::read_to_string(fixture_path()).expect("read derive_id.json fixture");
    let cases: Vec<FixtureCase> = serde_json::from_str(&raw).expect("parse derive_id.json fixture");
    assert!(!cases.is_empty(), "fixture must not be empty");
    for case in &cases {
        let actual = derive_id(&case.parent, &case.discriminator);
        assert_eq!(
            actual, case.expected,
            "derive_id({:?}, {:?}) mismatch",
            case.parent, case.discriminator
        );
    }
}
