//! Vocabulary-vs-schema equality gate for the `orb` i18n section: every
//! serde field name serialized by `OrbitalSchema` (and everything it embeds)
//! must be a key of `en.orb`. Uses `syn` (dev-only) to parse the schema
//! source directly rather than duplicating field lists by hand — the schema
//! stays the single owner of the `.orb` shape.

use super::tables;
use std::collections::BTreeSet;
use std::path::Path;

fn to_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            upper_next = true;
            continue;
        }
        if upper_next && i != 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
        upper_next = false;
    }
    out
}

/// `#[serde(...)]` attribute contents relevant to key extraction, collected
/// from a single `#[serde(...)]` attribute's meta list.
#[derive(Default)]
struct SerdeMeta {
    rename: Option<String>,
    rename_all: Option<String>,
    skip: bool,
    flatten: bool,
}

fn parse_serde_meta(attrs: &[syn::Attribute]) -> SerdeMeta {
    let mut meta = SerdeMeta::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let _ = attr.parse_nested_meta(|nested| {
            if nested.path.is_ident("rename") {
                let value = nested.value()?;
                let lit: syn::LitStr = value.parse()?;
                meta.rename = Some(lit.value());
            } else if nested.path.is_ident("rename_all") {
                let value = nested.value()?;
                let lit: syn::LitStr = value.parse()?;
                meta.rename_all = Some(lit.value());
            } else if nested.path.is_ident("skip") || nested.path.is_ident("skip_serializing") {
                meta.skip = true;
            } else if nested.path.is_ident("flatten") {
                meta.flatten = true;
            } else if let Ok(value) = nested.value() {
                // `key = "value"`/`key = expr` forms we don't otherwise
                // handle (`skip_serializing_if`, `tag`, …) — consume the
                // value so the outer parser can advance past it to the next
                // comma-separated meta item.
                let _: syn::Expr = value.parse()?;
            }
            // A bare flag with no value (e.g. `default`) needs nothing
            // consumed here — `parse_nested_meta` already advanced past it.
            Ok(())
        });
    }
    meta
}

fn field_key(field: &syn::Field, struct_rename_all: &Option<String>) -> Option<String> {
    let meta = parse_serde_meta(&field.attrs);
    if meta.skip || meta.flatten {
        return None;
    }
    if let Some(rename) = meta.rename {
        return Some(rename);
    }
    let raw = field.ident.as_ref()?.to_string();
    let raw = raw.strip_prefix("r#").unwrap_or(&raw).to_string();
    match struct_rename_all.as_deref() {
        Some("camelCase") => Some(to_camel_case(&raw)),
        _ => Some(raw),
    }
}

fn derives_serialize(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|nested| {
            if nested.path.is_ident("Serialize") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

fn collect_struct_keys(items: &[syn::Item], out: &mut BTreeSet<String>) {
    for item in items {
        match item {
            syn::Item::Struct(s) if derives_serialize(&s.attrs) => {
                if let syn::Fields::Named(named) = &s.fields {
                    let struct_meta = parse_serde_meta(&s.attrs);
                    for field in &named.named {
                        if let Some(key) = field_key(field, &struct_meta.rename_all) {
                            out.insert(key);
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    collect_struct_keys(nested, out);
                }
            }
            _ => {}
        }
    }
}

fn collect_file_keys(path: &Path, out: &mut BTreeSet<String>) {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("orb_keys_test: failed to read {}: {e}", path.display()));
    let parsed = syn::parse_file(&content)
        .unwrap_or_else(|e| panic!("orb_keys_test: failed to parse {}: {e}", path.display()));
    collect_struct_keys(&parsed.items, out);
}

#[test]
fn schema_serde_keys_are_all_in_en_orb() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut keys = BTreeSet::new();
    for rel in [
        "src/schema/types.rs",
        "src/schema/mod.rs",
        "src/schema/sexpr.rs",
    ] {
        collect_file_keys(&Path::new(manifest_dir).join(rel), &mut keys);
    }

    let en = &tables::definitions()["en"];
    let missing: Vec<&String> = keys
        .iter()
        .filter(|k| !en.orb.contains_key(*k) && !is_notation(k))
        .collect();
    assert!(
        missing.is_empty(),
        "orb_keys_test: {} schema serde key(s) missing from en.orb (never truncated): {:?}",
        missing.len(),
        missing
    );
}

/// Design-system token NAMES are serde keys of the theme structs but are NOT
/// language vocabulary, so they are deliberately absent from `en.orb` (owner
/// ruling 2026-09-07: the vocabulary is what the LANGUAGE defines; a library's
/// API surface stays English). The set is derived by walking `OrbitalSchema`
/// for keys reachable only under a theme `tokens` node and baked alongside the
/// tables by `almadar-sync i18n`, so this list cannot drift from the TS side.
fn is_notation(key: &str) -> bool {
    notation_keys().contains(key)
}

fn notation_keys() -> &'static std::collections::BTreeSet<String> {
    static KEYS: std::sync::OnceLock<std::collections::BTreeSet<String>> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        #[derive(serde::Deserialize)]
        struct Notation {
            keys: Vec<String>,
        }
        let raw = include_str!("orb-notation.json");
        let parsed: Notation = serde_json::from_str(raw).expect("baked orb-notation.json");
        parsed.keys.into_iter().collect()
    })
}

/// The baked exclusion set must actually be excluded — a notation key that
/// reappears in `en.orb` means the TS side re-introduced it.
#[test]
fn notation_keys_are_absent_from_en_orb() {
    let en = &tables::definitions()["en"];
    let present: Vec<&String> = notation_keys()
        .iter()
        .filter(|k| en.orb.contains_key(*k))
        .collect();
    assert!(
        present.is_empty(),
        "orb_keys_test: {} design-system token name(s) are in en.orb but must not be vocabulary: {:?}",
        present.len(),
        present
    );
}
