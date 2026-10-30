//! Pins `derive_id` byte-for-byte against `tests/fixtures/derive_id.json`; the fixture is the contract.

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
fn derive_id_matches_the_fixture() {
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
