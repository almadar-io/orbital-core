//! A trait instance dispatched without an entity id lives under `"singleton"`; reading it without
//! an entity id must address the same instance (C ABI `get_state`/`snapshot`, `ClientRuntime`).

use orbital_core::embed::EmbeddedKernel;
use orbital_core::{OrbitalSchema, Value};
use std::path::Path;

fn kernel(slug: &str) -> EmbeddedKernel {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/embed/resolved")
        .join(format!("{slug}.json"));
    let schema: OrbitalSchema =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    EmbeddedKernel::new(schema, "c").unwrap()
}

#[test]
fn an_unaddressed_read_sees_the_state_an_unaddressed_dispatch_left() {
    let k = kernel("rollback");
    k.dispatch("Driver", None, "INIT", Value::Null).unwrap();
    let singleton = k
        .kernel()
        .get_state("Driver", Some("singleton"))
        .map(|s| s.current_state)
        .unwrap();
    assert_eq!(k.get_state("Driver", None), singleton);
    assert_eq!(k.snapshot().states["Driver"], singleton);
}

#[test]
fn control_a_trait_never_dispatched_reads_its_initial_or_empty_state_without_error() {
    let k = kernel("rollback");
    k.dispatch("Driver", None, "INIT", Value::Null).unwrap();
    assert_eq!(k.get_state("NoSuchTrait", None), "");
}

#[test]
fn edge_an_addressed_read_is_unchanged() {
    let k = kernel("rollback");
    k.dispatch("Driver", Some("row-1"), "INIT", Value::Null).unwrap();
    assert_eq!(
        k.get_state("Driver", Some("row-1")),
        k.kernel()
            .get_state("Driver", Some("row-1"))
            .map(|s| s.current_state)
            .unwrap_or_default()
    );
}
