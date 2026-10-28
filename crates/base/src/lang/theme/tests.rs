use super::*;
use std::sync::Arc;

fn preset(json: &str) -> ThemeDefinition {
    serde_json::from_str(json).unwrap()
}

#[test]
fn theme_keys_follow_the_generated_css_selectors() {
    let both = preset(
        r##"{"name":"clay","tokens":{"colors":{"primary":"#000"}},"variants":{"dark":{}}}"##,
    );
    assert_eq!(theme_keys(&both), vec!["clay-light", "clay-dark"]);
    let light = preset(r##"{"name":"paper","tokens":{"colors":{"primary":"#fff"}}}"##);
    assert_eq!(theme_keys(&light), vec!["paper-light"]);
}

#[test]
fn a_dark_only_preset_has_no_light_key() {
    let dark = preset(r##"{"name":"game-rpg","tokens":{},"variants":{"dark":{}}}"##);
    assert_eq!(theme_keys(&dark), vec!["game-rpg-dark"]);
}

#[test]
fn control_a_non_dark_variant_adds_no_key() {
    let hc = preset(
        r##"{"name":"clay","tokens":{"colors":{"primary":"#000"}},"variants":{"high-contrast":{}}}"##,
    );
    assert_eq!(theme_keys(&hc), vec!["clay-light"]);
}

#[test]
fn preset_keys_are_sorted_across_presets() {
    let presets = ThemePresets::from_definitions(&[
        preset(r##"{"name":"zen","tokens":{"colors":{"a":"b"}}}"##),
        preset(r##"{"name":"aqua","tokens":{"colors":{"a":"b"}},"variants":{"dark":{}}}"##),
    ]);
    assert_eq!(
        presets.theme_keys(),
        vec!["aqua-dark", "aqua-light", "zen-light"]
    );
}
use crate::schema::types::{EntityRef, Orbital, TraitDefinition, TraitReference};

fn minimal_orbital(name: &str, theme: Option<ThemeRef>) -> Orbital {
    Orbital {
        spec: false,
        name: name.to_string(),
        id: None,
        uses: None,
        expects: None,
        theme,
        services: None,
        entity: EntityRef::Reference(format!("{name}Entity")),
        auxiliary_entities: None,
        traits: vec![TraitReference::Inline(Arc::new(minimal_trait_def(&format!(
            "{name}Trait"
        ))))],
        pages: None,
        emits: None,
        listens: None,
        exposes: None,
        reference: None,
        config: None,
        domain_context: None,
        os_grants: None,
        design: None,
        types: None,
    }
}

fn minimal_trait_def(name: &str) -> TraitDefinition {
    // `TraitDefinition` has many fields; `Default` isn't derived, so tests
    // build through `serde_json` instead of hand-listing every field —
    // only `name` + `stateMachine` (states + transitions, both required)
    // are non-`Option`.
    serde_json::from_value(serde_json::json!({
        "name": name,
        "stateMachine": {
            "states": [{ "name": "idle" }],
            "transitions": [],
        },
    }))
    .expect("minimal trait definition parses")
}

fn minimal_schema(app_theme: Option<ThemeRef>, orbitals: Vec<Orbital>) -> OrbitalSchema {
    OrbitalSchema {
        types: None,
        type_contract_version: None,
        resolved: None,        store: None,
        theme_definitions: None,
        themes: None,
        site: None,
        public_dir: None,
        locales: None,
        lazy_pages: None,
        name: "TestApp".to_string(),
        version: None,
        description: None,
        theme: app_theme,
        config: None,
        orbitals,
        metadata: None,
        schema_version: None,
        ledger: None,
        egress: None,
        inbound: None,
        os_grants: None,
        deployment: None,
    }
}

fn presets() -> ThemePresets {
    ThemePresets::installed().clone()
}

#[test]
fn resolve_theme_precedence_orbital_over_app_over_default() {
    let presets = presets();

    // Neither declares a theme → the baseline default.
    let schema = minimal_schema(None, vec![minimal_orbital("A", None)]);
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    assert_eq!(resolved.key, DEFAULT_THEME_KEY);

    // App declares one, orbital doesn't → app wins.
    let schema = minimal_schema(
        Some(ThemeRef::Reference("linear-clean-light".to_string())),
        vec![minimal_orbital("A", None)],
    );
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    assert_eq!(resolved.key, "linear-clean-light");

    // Orbital declares its own → orbital wins over the app-level one.
    let schema = minimal_schema(
        Some(ThemeRef::Reference("linear-clean-light".to_string())),
        vec![minimal_orbital(
            "A",
            Some(ThemeRef::Reference("clay-light".to_string())),
        )],
    );
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    assert_eq!(resolved.key, "clay-light");
    assert_eq!(resolved.base, "clay");
    assert_eq!(resolved.mode, ThemeMode::Light);
}

#[test]
fn resolve_theme_dark_merges_variant() {
    let presets = presets();
    let schema = minimal_schema(
        Some(ThemeRef::Reference("clay-dark".to_string())),
        vec![minimal_orbital("A", None)],
    );
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    assert_eq!(resolved.key, "clay-dark");
    assert_eq!(resolved.mode, ThemeMode::Dark);

    let light = resolve_theme(
        &minimal_schema(
            Some(ThemeRef::Reference("clay-light".to_string())),
            vec![minimal_orbital("A", None)],
        ),
        Some("A"),
        &presets,
        None,
    );
    // Dark colors differ from light colors — the variant actually merged in,
    // not just a base-tokens passthrough with the key relabeled.
    assert_ne!(
        resolved
            .tokens
            .colors
            .as_ref()
            .and_then(|c| c.get("primary")),
        light.tokens.colors.as_ref().and_then(|c| c.get("primary")),
    );
    // A dark-only value (present in the fixture's `variants.dark`) surfaces.
    assert!(resolved
        .tokens
        .colors
        .as_ref()
        .unwrap()
        .contains_key("background"));

    // requested_mode overrides the declared key's own suffix.
    let forced_dark = resolve_theme(
        &minimal_schema(
            Some(ThemeRef::Reference("clay-light".to_string())),
            vec![minimal_orbital("A", None)],
        ),
        Some("A"),
        &presets,
        Some(ThemeMode::Dark),
    );
    assert_eq!(forced_dark.key, "clay-dark");
    assert_eq!(
        forced_dark
            .tokens
            .colors
            .as_ref()
            .and_then(|c| c.get("primary")),
        resolved
            .tokens
            .colors
            .as_ref()
            .and_then(|c| c.get("primary")),
    );
}

#[test]
fn resolve_theme_preset_key_lookup() {
    let presets = presets();
    let schema = minimal_schema(
        Some(ThemeRef::Reference("clay-light".to_string())),
        vec![minimal_orbital("A", None)],
    );
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    // `clay.json`'s declared `tokens.colors.primary` — looked up through the
    // preset registry, not a literal passthrough of the reference string.
    assert_eq!(
        resolved
            .tokens
            .colors
            .as_ref()
            .and_then(|c| c.get("primary"))
            .cloned(),
        Some("#b97fd8".to_string())
    );
}

#[test]
fn installed_presets_cover_every_installed_file() {
    let files = crate::language_data::language_data_after_preflight()
        .themes
        .len();
    assert!(files > 0);
    assert_eq!(
        ThemePresets::installed().len(),
        files,
        "one preset per file, names unique"
    );
}

fn carried(schema: &OrbitalSchema) -> Vec<String> {
    schema
        .theme_definitions
        .iter()
        .flatten()
        .map(|d| d.name.clone())
        .collect()
}

#[test]
fn carries_referenced_presets_and_the_default_once() {
    let mut schema = minimal_schema(
        Some(ThemeRef::Reference("clay-dark".to_string())),
        vec![
            minimal_orbital("A", Some(ThemeRef::Reference("clay-light".to_string()))),
            minimal_orbital("B", Some(ThemeRef::Reference("aqua".to_string()))),
        ],
    );
    carry_theme_definitions(&mut schema, &presets());
    assert_eq!(carried(&schema), vec!["aqua", "clay", DEFAULT_THEME_BASE]);
}

#[test]
fn carries_the_themes_a_program_switches_to() {
    let mut schema = minimal_schema(
        Some(ThemeRef::Reference("clay-light".to_string())),
        vec![minimal_orbital("A", None)],
    );
    schema.themes = Some(vec!["aqua-dark".to_string(), "clay-dark".to_string()]);
    carry_theme_definitions(&mut schema, &presets());
    assert_eq!(carried(&schema), vec!["aqua", "clay", DEFAULT_THEME_BASE]);
}

#[test]
fn control_switched_to_themes_never_become_the_resolved_theme() {
    let mut schema = minimal_schema(
        Some(ThemeRef::Reference("clay-light".to_string())),
        vec![minimal_orbital("A", None)],
    );
    schema.themes = Some(vec!["aqua-dark".to_string()]);
    assert_eq!(resolve_theme(&schema, Some("A"), &presets(), None).base, "clay");
}

#[test]
fn control_no_declared_theme_carries_only_the_default() {
    let mut schema = minimal_schema(None, vec![minimal_orbital("A", None)]);
    carry_theme_definitions(&mut schema, &presets());
    assert_eq!(carried(&schema), vec![DEFAULT_THEME_BASE]);
}

#[test]
fn edge_an_unknown_key_carries_nothing_for_it_and_resolves_as_before() {
    let mut schema = minimal_schema(
        Some(ThemeRef::Reference("zz-unknown-dark".to_string())),
        vec![minimal_orbital("A", None)],
    );
    let before = resolve_theme(&schema, Some("A"), &presets(), None);
    carry_theme_definitions(&mut schema, &presets());
    assert_eq!(carried(&schema), vec![DEFAULT_THEME_BASE]);
    let from_program =
        ThemePresets::from_definitions(schema.theme_definitions.iter().flatten());
    let after = resolve_theme(&schema, Some("A"), &from_program, None);
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
}

#[test]
fn the_program_alone_resolves_like_the_installed_presets() {
    let mut schema = minimal_schema(
        Some(ThemeRef::Reference("clay-dark".to_string())),
        vec![
            minimal_orbital("A", None),
            minimal_orbital("B", Some(ThemeRef::Reference("aqua-light".to_string()))),
        ],
    );
    carry_theme_definitions(&mut schema, &presets());
    let from_program =
        ThemePresets::from_definitions(schema.theme_definitions.iter().flatten());
    for orbital in ["A", "B"] {
        let installed = resolve_theme(&schema, Some(orbital), &presets(), None);
        let carried = resolve_theme(&schema, Some(orbital), &from_program, None);
        assert_eq!(
            serde_json::to_value(&installed).unwrap(),
            serde_json::to_value(&carried).unwrap(),
            "{orbital}"
        );
    }
}

#[test]
fn resolved_theme_serde_round_trip() {
    let presets = presets();
    let schema = minimal_schema(
        Some(ThemeRef::Reference("clay-dark".to_string())),
        vec![minimal_orbital("A", None)],
    );
    let resolved = resolve_theme(&schema, Some("A"), &presets, None);
    let json = serde_json::to_value(&resolved).expect("serializes");
    let back: ResolvedTheme = serde_json::from_value(json.clone()).expect("deserializes");
    assert_eq!(serde_json::to_value(&back).unwrap(), json);
}
