//! §166 mirror gate: `@almadar/core` `fixtures/theme-tokens/every-key.json` (baked
//! into this crate as `orbital_core::fixtures::theme_tokens_every_key()`) carries every
//! `COLOR_TOKEN_KEYS` / `ILLUSTRATION_TOKEN_KEYS` member, generated from core's one
//! list. The Rust `ColorTokens`/`IllustrationTokens` must round-trip it byte-equal
//! (modulo key order): a key core has and Rust lacks is dropped on re-serialization
//! and fails here, not silently in a native theme.

use orbital_core::fixtures::theme_tokens_every_key;
use orbital_core::schema::{ColorTokens, DiagramTokens, IllustrationTokens, SceneTokens};

fn fixture() -> serde_json::Value {
    serde_json::from_str(theme_tokens_every_key()).unwrap()
}

#[test]
fn color_tokens_mirror_carries_every_core_key() {
    let original = fixture()["color"].clone();
    let parsed: ColorTokens = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
}

#[test]
fn illustration_tokens_mirror_carries_every_core_key() {
    let original = fixture()["illustration"].clone();
    let parsed: IllustrationTokens = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
}

#[test]
fn diagram_tokens_mirror_carries_every_core_key() {
    let original = fixture()["diagram"].clone();
    let parsed: DiagramTokens = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
}

#[test]
fn scene_tokens_mirror_carries_every_core_key() {
    let original = fixture()["scene"].clone();
    let parsed: SceneTokens = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
}
