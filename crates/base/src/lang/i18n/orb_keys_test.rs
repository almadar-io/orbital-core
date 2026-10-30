//! Vocabulary-vs-schema equality gate for the `orb` i18n section: every
//! serde field name serialized by `OrbitalSchema` (and everything it embeds)
//! must be a key of `en.orb`. Uses `syn` (dev-only) to parse the schema
//! source directly rather than duplicating field lists by hand — the schema
//! stays the single owner of the `.orb` shape.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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

/// A module file plus its child module files (`foo.rs` and `foo/*.rs`), so a
/// split module is read whole; test modules hold no schema structs.
fn collect_module_keys(path: &Path, out: &mut BTreeSet<String>) {
    collect_file_keys(path, out);
    let Ok(entries) = std::fs::read_dir(path.with_extension("")) else {
        return;
    };
    let mut children: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s != "tests" && !s.ends_with("_tests"))
        })
        .collect();
    children.sort();
    for child in children {
        collect_file_keys(&child, out);
    }
}

#[test]
fn schema_serde_keys_are_all_in_en_orb() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut keys = BTreeSet::new();
    for rel in [
        "src/program/schema/types.rs",
        "src/program/schema/mod.rs",
        "src/program/schema/sexpr.rs",
    ] {
        collect_module_keys(&Path::new(manifest_dir).join(rel), &mut keys);
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
    static KEYS: crate::sync::OnceLock<std::collections::BTreeSet<String>> =
        crate::sync::OnceLock::new();
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

fn ts_core() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../orbital-shell-typescript/core/src")
}

fn i18n_tables_ts() -> String {
    let list = |items: &[&str]| {
        items
            .iter()
            .map(|i| format!("'{i}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "// GENERATED by `I18N_TS_WRITE=1 cargo test -p orbital-core-base --lib ts_i18n_data` from orbital-core `lang::i18n`.\n\
         // Do not edit: change the Rust language data and regenerate.\n\n\
         export const RTL_LOCALES: readonly string[] = [{}];\n\n\
         export const LOLO_FIRST_TOKEN_KEYWORDS = [{}] as const;\n",
        list(crate::messages::RTL_LOCALES),
        list(super::lang::LOLO_FIRST_TOKEN_KEYWORDS),
    )
}

/// `@almadar/core`'s i18n data are byte copies of this crate's files; Rust owns them.
#[test]
fn ts_i18n_data_is_current() {
    let core = ts_core();
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lang/i18n");
    let files = [
        (here.join("en.json"), core.join("i18n/en.json"), None),
        (here.join("orb-notation.json"), core.join("i18n/orb-notation.json"), None),
        (here.join("en.json"), core.join("generated/i18n-tables.ts"), Some(i18n_tables_ts())),
    ];
    let write = std::env::var("I18N_TS_WRITE").as_deref() == Ok("1");
    for (source, target, generated) in files {
        let expected = match generated {
            Some(text) => text,
            None => std::fs::read_to_string(&source).unwrap(),
        };
        if write {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &expected).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&target).unwrap_or_default();
        assert_eq!(
            committed,
            expected,
            "{} is stale: I18N_TS_WRITE=1 cargo test -p orbital-core-base --lib ts_i18n_data",
            target.display()
        );
    }
}

/// `en.operators` spells every operator of the canonical table (and nothing
/// else), so a table change cannot leave English behind the std table.
#[test]
fn en_operators_equal_the_canonical_table() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let table = manifest
        .ancestors()
        .map(|p| p.join("packages/almadar-std/canonical-operators.json"))
        .find(|p| p.exists())
        .expect("packages/almadar-std/canonical-operators.json above the crate");
    let canonical: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&table).unwrap()).unwrap();
    let canonical: BTreeSet<String> = canonical["operators"]
        .as_object()
        .expect("canonical-operators.json has an operators object")
        .keys()
        .cloned()
        .collect();
    let english: BTreeSet<String> = tables::english().operators.keys().cloned().collect();
    let missing: Vec<&String> = canonical.difference(&english).collect();
    let orphan: Vec<&String> = english.difference(&canonical).collect();
    assert!(
        missing.is_empty() && orphan.is_empty(),
        "en.operators drifted from the canonical table: missing {missing:?}, orphan {orphan:?}"
    );
}
