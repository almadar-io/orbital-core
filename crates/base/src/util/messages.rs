//! Message catalogs for `(i18n/t "key" { params })`: every key a behavior
//! writes is qualified with the behavior's own name at lowering
//! (`"hero.title"` in `app std-almadar-home` → `"std-almadar-home:hero.title"`),
//! so one merged catalog per locale serves a whole program without two
//! behaviors' keys colliding, and nothing at runtime has to know which
//! behavior an expression came from.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde_json::Value;

pub const TRANSLATE_OP: &str = "i18n/t";
pub const NAMESPACE_SEPARATOR: char = ':';

/// Why a key could not be qualified.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageKeyIssue {
    /// The key is empty or already carries the reserved `:` separator.
    Invalid { path: String, key: String },
}

/// Qualifies every `["i18n/t", "<key>", …]` in `value` with `namespace`.
pub fn qualify_message_keys(value: &mut Value, namespace: &str) -> Vec<MessageKeyIssue> {
    let mut issues = Vec::new();
    walk(value, namespace, "", &mut issues);
    issues
}

fn walk(value: &mut Value, namespace: &str, path: &str, issues: &mut Vec<MessageKeyIssue>) {
    match value {
        Value::Array(items) => {
            if matches!(items.first(), Some(Value::String(op)) if op == TRANSLATE_OP) {
                if let Some(key @ Value::String(_)) = items.get_mut(1) {
                    issues.extend(qualify_message_key_value(key, namespace, &format!("{path}[1]")));
                }
            }
            for (i, item) in items.iter_mut().enumerate() {
                walk(item, namespace, &format!("{path}[{i}]"), issues);
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                walk(v, namespace, &format!("{path}.{k}"), issues);
            }
        }
        _ => {}
    }
}

/// Whether `text` is a key the lowering qualified (`<behavior>:<key>`): every literal a program writes in a
/// `MessageKey` slot is one by the time it is validated, so an unqualified one came from a plain string.
pub fn is_qualified_key(text: &str) -> bool {
    text.split_once(NAMESPACE_SEPARATOR).is_some_and(|(ns, key)| !ns.is_empty() && !key.is_empty())
}

/// Qualifies one `MessageKey` written where it is declared or passed. A string literal is qualified; a `@` binding
/// reads a key already qualified where it was written, and any other expression is typed by the checker, so both pass
/// through.
pub fn qualify_message_key_value(value: &mut Value, namespace: &str, path: &str) -> Vec<MessageKeyIssue> {
    match value {
        Value::String(key) if key.starts_with('@') => Vec::new(),
        Value::String(key) if !key.is_empty() && !key.contains(NAMESPACE_SEPARATOR) => {
            *key = format!("{namespace}{NAMESPACE_SEPARATOR}{key}");
            Vec::new()
        }
        Value::String(key) => vec![MessageKeyIssue::Invalid { path: path.to_string(), key: key.clone() }],
        _ => Vec::new(),
    }
}

/// One step into a knob's value: every item of a list, or one member of a record.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyStep {
    Item,
    Field(String),
}

/// The positions in a knob's value that hold a catalog key, read off its declared shape: the value itself
/// (`MessageKey`), each item (`[MessageKey]`), or a `MessageKey` member of a record, at any depth of lists and records.
pub fn message_key_paths(field: &crate::schema::ConfigField) -> Vec<Vec<KeyStep>> {
    match field.field_type.as_str() {
        "MessageKey" => vec![Vec::new()],
        "[MessageKey]" => vec![vec![KeyStep::Item]],
        _ => carried_key_paths(field.items.as_deref(), field.properties.as_ref()),
    }
}

fn carried_key_paths(
    items: Option<&crate::schema::FieldDefinition>,
    properties: Option<&crate::collections::IndexMap<String, crate::schema::FieldDefinition>>,
) -> Vec<Vec<KeyStep>> {
    let mut paths = Vec::new();
    if let Some(item) = items {
        paths.extend(definition_key_paths(item).into_iter().map(|rest| prepend(KeyStep::Item, rest)));
    }
    for (name, member) in properties.into_iter().flatten() {
        paths.extend(definition_key_paths(member).into_iter().map(|rest| prepend(KeyStep::Field(name.clone()), rest)));
    }
    paths
}

fn definition_key_paths(def: &crate::schema::FieldDefinition) -> Vec<Vec<KeyStep>> {
    if matches!(def.field_type, crate::schema::FieldType::MessageKey) {
        return vec![Vec::new()];
    }
    carried_key_paths(def.items.as_deref(), def.properties.as_ref())
}

fn prepend(step: KeyStep, mut rest: Vec<KeyStep>) -> Vec<KeyStep> {
    rest.insert(0, step);
    rest
}

fn at_path<'v>(value: &'v mut Value, path: &[KeyStep], at: String, out: &mut Vec<(String, &'v mut Value)>) {
    match path.split_first() {
        None => out.push((at, value)),
        Some((KeyStep::Item, rest)) => {
            if let Value::Array(items) = value {
                for (i, item) in items.iter_mut().enumerate() {
                    at_path(item, rest, format!("{at}[{i}]"), out);
                }
            }
        }
        Some((KeyStep::Field(name), rest)) => {
            if let Some(member) = value.as_object_mut().and_then(|m| m.get_mut(name)) {
                at_path(member, rest, format!("{at}.{name}"), out);
            }
        }
    }
}

fn each_trait_config_mut(
    schema: &mut crate::schema::OrbitalSchema,
    mut visit: impl FnMut(String, &mut crate::schema::ConfigField),
) {
    use crate::schema::TraitReference;
    for orbital in &mut schema.orbitals {
        for (index, trait_ref) in orbital.traits.iter_mut().enumerate() {
            let (name, config) = match trait_ref {
                TraitReference::Inline(def) => {
                    if !def.config.iter().flatten().any(|(_, f)| !message_key_paths(f).is_empty()) {
                        continue;
                    }
                    let def = std::sync::Arc::make_mut(def);
                    (def.name.clone(), def.config.as_mut())
                }
                TraitReference::Reference { name, reference, config, .. } => (
                    name.clone().or_else(|| reference.clone()).unwrap_or_else(|| index.to_string()),
                    config.as_mut(),
                ),
            };
            for (knob, field) in config.into_iter().flatten() {
                visit(format!("{}.{name}.config.{knob}", orbital.name), field);
            }
        }
    }
}

/// Qualifies the literal values of every config knob typed `MessageKey` or `[MessageKey]` (a trait's own declaration
/// or a call-site wiring the lowering typed from the atom) with `namespace`, as `i18n/t` literals are.
pub fn qualify_config_message_keys(schema: &mut crate::schema::OrbitalSchema, namespace: &str) -> Vec<MessageKeyIssue> {
    let mut issues = Vec::new();
    each_trait_config_mut(schema, |path, field| {
        let paths = message_key_paths(field);
        let Some(default) = field.default.as_mut().filter(|_| !paths.is_empty()) else {
            return;
        };
        let Ok(mut value) = serde_json::to_value(&*default) else { return };
        for key_path in &paths {
            let mut slots = Vec::new();
            at_path(&mut value, key_path, path.clone(), &mut slots);
            for (at, slot) in slots {
                issues.extend(qualify_message_key_value(slot, namespace, &at));
            }
        }
        if let Ok(qualified) = serde_json::from_value(value) {
            *default = qualified;
        }
    });
    issues
}

/// The literal keys every `MessageKey` / `[MessageKey]` config knob holds: catalog keys the program uses through a
/// knob rather than an `i18n/t` literal.
pub fn config_message_keys(schema: &crate::schema::OrbitalSchema) -> Vec<String> {
    use crate::schema::TraitReference;
    let fields = schema.orbitals.iter().flat_map(|o| &o.traits).flat_map(|t| match t {
        TraitReference::Inline(def) => def.config.iter().flatten(),
        TraitReference::Reference { config, .. } => config.iter().flatten(),
    });
    let mut keys = Vec::new();
    for (_, field) in fields {
        let paths = message_key_paths(field);
        let Some(default) = field.default.as_ref().filter(|_| !paths.is_empty()) else {
            continue;
        };
        let Ok(mut value) = serde_json::to_value(default) else { continue };
        for key_path in &paths {
            let mut slots = Vec::new();
            at_path(&mut value, key_path, String::new(), &mut slots);
            keys.extend(slots.into_iter().filter_map(|(_, v)| v.as_str()).filter(|k| !k.starts_with('@')).map(str::to_string));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_literal_key_is_qualified_with_the_behavior_name() {
        let mut v = json!(["render-ui", "main", { "type": "typography", "content": ["i18n/t", "hero.title"] }]);
        assert!(qualify_message_keys(&mut v, "std-almadar-home").is_empty());
        assert_eq!(v[2]["content"], json!(["i18n/t", "std-almadar-home:hero.title"]));
    }

    #[test]
    fn params_are_kept_and_nested_calls_are_qualified() {
        let mut v = json!(["i18n/t", "posts.count", { "n": ["array/len", "@entity.posts"], "label": ["i18n/t", "posts.label"] }]);
        assert!(qualify_message_keys(&mut v, "blog").is_empty());
        assert_eq!(v[1], json!("blog:posts.count"));
        assert_eq!(v[2]["label"], json!(["i18n/t", "blog:posts.label"]));
        assert_eq!(v[2]["n"], json!(["array/len", "@entity.posts"]));
    }

    #[test]
    fn control_other_operators_and_plain_strings_are_untouched() {
        let mut v = json!(["str/concat", "i18n/t", "hero.title"]);
        let before = v.clone();
        assert!(qualify_message_keys(&mut v, "home").is_empty());
        assert_eq!(v, before);
    }

    #[test]
    fn a_binding_or_computed_key_is_left_for_the_type_checker() {
        let mut v = json!({ "a": ["i18n/t", "@config.label"], "b": ["i18n/t", "@n.textKey"], "c": ["i18n/t", ["str/concat", "post.", "@entity.slug"]] });
        let before = v.clone();
        assert!(qualify_message_keys(&mut v, "home").is_empty());
        assert_eq!(v, before);
    }

    #[test]
    fn edge_a_missing_key_is_left_for_the_arity_check() {
        let mut v = json!(["i18n/t"]);
        assert!(qualify_message_keys(&mut v, "home").is_empty());
        assert_eq!(v, json!(["i18n/t"]));
    }

    #[test]
    fn a_qualified_key_has_a_namespace_and_a_key() {
        assert!(is_qualified_key("panel:begin"));
        for text in ["begin", ":begin", "panel:", ""] {
            assert!(!is_qualified_key(text), "{text:?}");
        }
    }

    #[test]
    fn a_message_key_value_qualifies_a_literal() {
        let mut one = json!("begin");
        assert!(qualify_message_key_value(&mut one, "panel", "label").is_empty());
        assert_eq!(one, json!("panel:begin"));

    }

    #[test]
    fn control_a_message_key_binding_or_expression_value_is_untouched() {
        for v in [json!("@config.label"), json!(["str/concat", "a", "b"]), json!(null)] {
            let mut got = v.clone();
            assert!(qualify_message_key_value(&mut got, "panel", "label").is_empty());
            assert_eq!(got, v);
        }
    }

    #[test]
    fn edge_an_empty_or_qualified_message_key_value_is_invalid() {
        let (mut empty, mut qualified) = (json!(""), json!("x:y"));
        assert_eq!(
            qualify_message_key_value(&mut empty, "panel", "labels[0]"),
            vec![MessageKeyIssue::Invalid { path: "labels[0]".into(), key: "".into() }]
        );
        assert_eq!(
            qualify_message_key_value(&mut qualified, "panel", "labels[1]"),
            vec![MessageKeyIssue::Invalid { path: "labels[1]".into(), key: "x:y".into() }]
        );
    }

    #[test]
    fn edge_an_empty_or_already_qualified_key_is_invalid() {
        let mut v = json!([["i18n/t", ""], ["i18n/t", "other:hero.title"]]);
        assert_eq!(
            qualify_message_keys(&mut v, "home"),
            vec![
                MessageKeyIssue::Invalid { path: "[0][1]".into(), key: "".into() },
                MessageKeyIssue::Invalid { path: "[1][1]".into(), key: "other:hero.title".into() },
            ]
        );
    }
}

/// A catalog rule a program breaks. Each maps to one `ORB_I18N_*` code.
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogIssue {
    /// `i18n/t` is used but the program declares no `locales`, or they omit `en`.
    NoLocales,
    /// A catalog file is not a flat object of strings, or names an undeclared locale.
    CatalogInvalid { locale: String, detail: String },
    /// A key is absent from the `en` catalog.
    KeyMissing { key: String },
    /// A key is absent from a declared locale other than `en`.
    KeyUntranslated { key: String, locale: String },
    /// The `{{placeholders}}` of a message differ from the params passed.
    ParamMismatch { key: String, locale: String, expected: Vec<String>, passed: Vec<String> },
    /// The params of an `i18n/t` call are not an object literal.
    ParamsNotLiteral { key: String },
    /// A catalog key no call uses.
    KeyUnused { key: String, locale: String },
}

impl CatalogIssue {
    pub fn code(&self) -> &'static str {
        match self {
            CatalogIssue::NoLocales => "ORB_I18N_NO_LOCALES",
            CatalogIssue::CatalogInvalid { .. } => "ORB_I18N_CATALOG_INVALID",
            CatalogIssue::KeyMissing { .. } => "ORB_I18N_KEY_MISSING",
            CatalogIssue::KeyUntranslated { .. } => "ORB_I18N_KEY_UNTRANSLATED",
            CatalogIssue::ParamMismatch { .. } | CatalogIssue::ParamsNotLiteral { .. } => {
                "ORB_I18N_PARAM_MISMATCH"
            }
            CatalogIssue::KeyUnused { .. } => "ORB_I18N_KEY_UNUSED",
        }
    }

    pub fn message(&self) -> String {
        match self {
            CatalogIssue::NoLocales => {
                "`i18n/t` is used, so the app must declare `locales [en, …]` (including `en`)".to_string()
            }
            CatalogIssue::CatalogInvalid { locale, detail } => format!("catalog `{locale}`: {detail}"),
            CatalogIssue::KeyMissing { key } => format!("key `{key}` is not in the `en` catalog"),
            CatalogIssue::KeyUntranslated { key, locale } => format!("key `{key}` is not in the `{locale}` catalog"),
            CatalogIssue::ParamMismatch { key, locale, expected, passed } => format!(
                "key `{key}` in `{locale}` expects params [{}] but the call passes [{}]",
                expected.join(", "),
                passed.join(", ")
            ),
            CatalogIssue::ParamsNotLiteral { key } => {
                format!("the params of `{key}` must be an object literal so its placeholders can be checked")
            }
            CatalogIssue::KeyUnused { key, locale } => format!("catalog `{locale}` key `{key}` is used by no `i18n/t` call"),
        }
    }
}

struct Call {
    key: String,
    params: Option<Vec<String>>,
}

fn collect_calls(value: &Value, prefix: &str, out: &mut Vec<Call>, issues: &mut Vec<CatalogIssue>) {
    match value {
        Value::Array(items) => {
            if let (Some(Value::String(op)), Some(Value::String(qualified))) = (items.first(), items.get(1)) {
                if op == TRANSLATE_OP {
                    if let Some(key) = qualified.strip_prefix(prefix) {
                        let params = match items.get(2) {
                            None => Some(Vec::new()),
                            Some(Value::Object(map)) => Some(map.keys().cloned().collect()),
                            Some(_) => {
                                issues.push(CatalogIssue::ParamsNotLiteral { key: key.to_string() });
                                None
                            }
                        };
                        out.push(Call { key: key.to_string(), params });
                    }
                }
            }
            for item in items {
                collect_calls(item, prefix, out, issues);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_calls(v, prefix, out, issues);
            }
        }
        _ => {}
    }
}

fn placeholders(message: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = message;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim().to_string();
        if !names.contains(&name) {
            names.push(name);
        }
        rest = &after[end + 2..];
    }
    names.sort();
    names
}

/// Checks a program's own `i18n/t` calls (keys qualified with `namespace`)
/// against its catalogs: `catalogs[locale]` is the parsed JSON of that
/// locale's catalog file, or `None` when the file is absent.
pub fn check_catalogs(
    program: &Value,
    knob_keys: &[String],
    namespace: &str,
    locales: Option<&[String]>,
    catalogs: &std::collections::BTreeMap<String, Option<Value>>,
) -> Vec<CatalogIssue> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut issues = Vec::new();
    let mut calls = Vec::new();
    let prefix = format!("{namespace}{NAMESPACE_SEPARATOR}");
    collect_calls(program, &prefix, &mut calls, &mut issues);
    calls.extend(
        knob_keys
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|key| Call { key: key.to_string(), params: None }),
    );

    let declared: Vec<String> = locales.map(|l| l.to_vec()).unwrap_or_default();
    if !declared.iter().any(|l| l == "en") {
        if !calls.is_empty() {
            issues.push(CatalogIssue::NoLocales);
        }
        return issues;
    }

    let mut parsed: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (locale, file) in catalogs {
        if !declared.contains(locale) {
            issues.push(CatalogIssue::CatalogInvalid {
                locale: locale.clone(),
                detail: "a catalog for a locale the app does not declare".to_string(),
            });
            continue;
        }
        let Some(file) = file else { continue };
        let Value::Object(map) = file else {
            issues.push(CatalogIssue::CatalogInvalid {
                locale: locale.clone(),
                detail: "must be a JSON object of key → string".to_string(),
            });
            continue;
        };
        let mut messages = BTreeMap::new();
        for (key, v) in map {
            match v {
                Value::String(s) => {
                    messages.insert(key.clone(), s.clone());
                }
                _ => issues.push(CatalogIssue::CatalogInvalid {
                    locale: locale.clone(),
                    detail: format!("key `{key}` must map to a string"),
                }),
            }
        }
        parsed.insert(locale.clone(), messages);
    }

    let used: BTreeSet<&str> = calls.iter().map(|c| c.key.as_str()).collect();
    for locale in &declared {
        let messages = parsed.get(locale);
        for call in &calls {
            match messages.and_then(|m| m.get(&call.key)) {
                None if locale == "en" => issues.push(CatalogIssue::KeyMissing { key: call.key.clone() }),
                None => issues.push(CatalogIssue::KeyUntranslated {
                    key: call.key.clone(),
                    locale: locale.clone(),
                }),
                Some(message) => {
                    if let Some(passed) = &call.params {
                        let mut passed = passed.clone();
                        passed.sort();
                        let expected = placeholders(message);
                        if expected != passed {
                            issues.push(CatalogIssue::ParamMismatch {
                                key: call.key.clone(),
                                locale: locale.clone(),
                                expected,
                                passed,
                            });
                        }
                    }
                }
            }
        }
        for key in messages.into_iter().flat_map(|m| m.keys()) {
            if !used.contains(key.as_str()) {
                issues.push(CatalogIssue::KeyUnused {
                    key: key.clone(),
                    locale: locale.clone(),
                });
            }
        }
    }
    let mut seen = BTreeSet::new();
    issues.retain(|i| seen.insert(format!("{i:?}")));
    issues
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn program() -> Value {
        json!(["do",
            ["i18n/t", "home:hero.title"],
            ["i18n/t", "home:posts.count", { "n": 3 }],
            ["i18n/t", "atom:other.key"]])
    }
    fn locales() -> Vec<String> {
        vec!["en".into(), "ar".into()]
    }
    fn catalogs(en: Value, ar: Value) -> BTreeMap<String, Option<Value>> {
        [("en".to_string(), Some(en)), ("ar".to_string(), Some(ar))].into_iter().collect()
    }
    fn codes(issues: &[CatalogIssue]) -> Vec<&'static str> {
        issues.iter().map(|i| i.code()).collect()
    }
    fn complete() -> BTreeMap<String, Option<Value>> {
        catalogs(
            json!({ "hero.title": "Hello", "posts.count": "{{n}} posts" }),
            json!({ "hero.title": "مرحبا", "posts.count": "{{n}} مقالات" }),
        )
    }

    #[test]
    fn control_complete_catalogs_pass_and_other_namespaces_are_ignored() {
        assert!(check_catalogs(&program(), &[], "home", Some(&locales()), &complete()).is_empty());
    }

    #[test]
    fn a_key_missing_from_en_is_reported() {
        let mut c = complete();
        c.insert("en".into(), Some(json!({ "posts.count": "{{n}} posts" })));
        assert_eq!(codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_MISSING"]);
    }

    #[test]
    fn a_key_missing_from_another_declared_locale_is_untranslated() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": "مرحبا" })));
        assert_eq!(codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_UNTRANSLATED"]);
    }

    #[test]
    fn edge_an_absent_catalog_file_reports_every_key() {
        let mut c = complete();
        c.insert("ar".into(), None);
        assert_eq!(
            codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c)),
            vec!["ORB_I18N_KEY_UNTRANSLATED", "ORB_I18N_KEY_UNTRANSLATED"]
        );
    }

    #[test]
    fn placeholders_must_match_the_params_in_every_locale() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": "مرحبا", "posts.count": "{{count}} مقالات" })));
        let issues = check_catalogs(&program(), &[], "home", Some(&locales()), &c);
        assert_eq!(codes(&issues), vec!["ORB_I18N_PARAM_MISMATCH"]);
    }

    #[test]
    fn edge_params_that_are_not_an_object_literal_are_rejected() {
        let p = json!(["i18n/t", "home:posts.count", "@config.counts"]);
        let c = catalogs(json!({ "posts.count": "{{n}}" }), json!({ "posts.count": "{{n}}" }));
        assert_eq!(codes(&check_catalogs(&p, &[], "home", Some(&locales()), &c)), vec!["ORB_I18N_PARAM_MISMATCH"]);
    }

    #[test]
    fn an_unused_catalog_key_is_reported() {
        let mut c = complete();
        c.insert("en".into(), Some(json!({ "hero.title": "Hello", "posts.count": "{{n}} posts", "stale": "x" })));
        assert_eq!(codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_UNUSED"]);
    }

    #[test]
    fn using_i18n_without_locales_is_rejected_and_control_without_calls_is_fine() {
        assert_eq!(codes(&check_catalogs(&program(), &[], "home", None, &BTreeMap::new())), vec!["ORB_I18N_NO_LOCALES"]);
        assert!(check_catalogs(&json!(["do"]), &[], "home", None, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn edge_locales_without_en_are_rejected() {
        let only_ar = vec!["ar".to_string()];
        assert_eq!(codes(&check_catalogs(&program(), &[], "home", Some(&only_ar), &BTreeMap::new())), vec!["ORB_I18N_NO_LOCALES"]);
    }

    #[test]
    fn a_malformed_or_undeclared_catalog_is_invalid() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": { "nested": "x" }, "posts.count": "{{n}}" })));
        c.insert("sl".into(), Some(json!({})));
        let found = codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c));
        assert!(found.iter().filter(|c| **c == "ORB_I18N_CATALOG_INVALID").count() == 2, "{found:?}");
    }

    #[test]
    fn a_knob_key_is_used_and_checked_like_a_literal() {
        let knobs = vec!["home:begin".to_string(), "atom:ignored".to_string()];
        let mut c = complete();
        c.insert("en".into(), Some(json!({ "hero.title": "Hello", "posts.count": "{{n}} posts", "begin": "Begin" })));
        c.insert("ar".into(), Some(json!({ "hero.title": "مرحبا", "posts.count": "{{n}} مقالات", "begin": "ابدأ" })));
        assert!(check_catalogs(&program(), &knobs, "home", Some(&locales()), &c).is_empty());
        assert_eq!(
            codes(&check_catalogs(&program(), &[], "home", Some(&locales()), &c)),
            vec!["ORB_I18N_KEY_UNUSED", "ORB_I18N_KEY_UNUSED"],
            "control: without the knob the key is unused"
        );
    }

    #[test]
    fn edge_a_knob_key_missing_from_en_is_reported() {
        let knobs = vec!["home:begin".to_string()];
        assert_eq!(
            codes(&check_catalogs(&program(), &knobs, "home", Some(&locales()), &complete())),
            vec!["ORB_I18N_KEY_MISSING", "ORB_I18N_KEY_UNTRANSLATED"]
        );
    }

    fn knob_schema() -> crate::schema::OrbitalSchema {
        serde_json::from_value(json!({
            "name": "panel",
            "orbitals": [{
                "name": "Panel",
                "entity": { "name": "PanelView", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Own", "linkedEntity": "PanelView", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [], "transitions": [] },
                      "config": {
                        "label": { "type": "MessageKey", "default": "title" },
                        "seasons": { "type": "[MessageKey]", "default": ["spring", "summer"], "items": { "type": "MessageKey" } },
                        "plain": { "type": "string", "default": "not a key" },
                        "options": { "type": "[RasmOption]", "default": [{ "value": "spring", "label": "season.spring" }, { "value": "@x", "label": "@config.other" }],
                          "items": { "type": "object", "properties": {
                            "value": { "name": "value", "type": "string" },
                            "label": { "name": "label", "type": "MessageKey" } } } }
                      } },
                    { "ref": "Button.traits.RasmPressable", "name": "Begin", "config": {
                        "label": { "type": "MessageKey", "default": "begin" },
                        "bound": { "type": "MessageKey", "default": "@config.label" },
                        "id": { "type": "unknown", "default": "begin" }
                    } }
                ]
            }]
        }))
        .expect("schema")
    }

    #[test]
    fn config_knobs_typed_message_key_are_qualified_and_collected() {
        let mut schema = knob_schema();
        assert!(qualify_config_message_keys(&mut schema, "panel").is_empty());
        let json = serde_json::to_string(&schema).unwrap();
        let mut keys = config_message_keys(&schema);
        keys.sort();
        assert_eq!(keys, vec!["panel:begin", "panel:season.spring", "panel:spring", "panel:summer", "panel:title"]);
        assert!(json.contains(r#""value":"spring","label":"panel:season.spring""#), "a key inside a record of a list: {json}");
        assert!(json.contains(r#""value":"@x","label":"@config.other""#), "control: a record's string member and a binding are untouched: {json}");
        assert!(json.contains(r#""default":"not a key""#), "control: a string knob is untouched: {json}");
        assert!(json.contains(r#""default":"@config.label""#), "control: a binding is untouched: {json}");
        assert!(json.contains(r#""id":{"type":"unknown","default":"begin"}"#), "control: an untyped wiring is untouched: {json}");
    }

    #[test]
    fn edge_a_qualified_knob_value_is_invalid() {
        let mut schema = knob_schema();
        let json = serde_json::to_string(&schema).unwrap().replace(r#""default":"title""#, r#""default":"x:title""#);
        schema = serde_json::from_str(&json).unwrap();
        assert_eq!(
            qualify_config_message_keys(&mut schema, "panel"),
            vec![MessageKeyIssue::Invalid { path: "Panel.Own.config.label".into(), key: "x:title".into() }]
        );
    }
}

/// `i18n/t` and `@locale` are render-time: the client's I18nProvider holds
/// the active locale's catalog. Every use outside a `render-ui` effect (a
/// guard, a `set`, an emit payload, a tick) is reported by path.
pub fn render_only_violations(program: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(orbitals) = program.get("orbitals").and_then(Value::as_array) else {
        return out;
    };
    for (oi, orbital) in orbitals.iter().enumerate() {
        for (ti, tr) in orbital.get("traits").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let base = format!("orbitals[{oi}].traits[{ti}]");
            let rows = [
                ("stateMachine.transitions", tr.get("stateMachine").and_then(|m| m.get("transitions"))),
                ("ticks", tr.get("ticks")),
                ("listens", tr.get("listens")),
            ];
            for (field, rows) in rows {
                for (ri, row) in rows.and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let at = format!("{base}.{field}[{ri}]");
                    if let Some(guard) = row.get("guard") {
                        scan(guard, &format!("{at}.guard"), &mut out);
                    }
                    for (ei, effect) in row.get("effects").and_then(Value::as_array).into_iter().flatten().enumerate() {
                        if effect.get(0).and_then(Value::as_str) != Some("render-ui") {
                            scan(effect, &format!("{at}.effects[{ei}]"), &mut out);
                        }
                    }
                }
            }
        }
    }
    out
}

fn scan(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::String(s) if s == "@locale" || s.starts_with("@locale.") => out.push(path.to_string()),
        Value::Array(items) => {
            if items.first().and_then(Value::as_str) == Some("render-ui") {
                return;
            }
            if items.first().and_then(Value::as_str) == Some(TRANSLATE_OP) {
                out.push(path.to_string());
                return;
            }
            for (i, item) in items.iter().enumerate() {
                scan(item, &format!("{path}[{i}]"), out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                scan(v, &format!("{path}.{k}"), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod render_only_tests {
    use super::*;
    use serde_json::json;

    fn program(transition: Value) -> Value {
        json!({ "orbitals": [{ "traits": [{ "stateMachine": { "transitions": [transition] } }] }] })
    }

    #[test]
    fn control_inside_render_ui_is_allowed() {
        let p = program(json!({ "effects": [["render-ui", "main", { "content": ["i18n/t", "a:b"], "dir": "@locale" }]] }));
        assert!(render_only_violations(&p).is_empty());
    }

    #[test]
    fn inside_a_set_effect_is_reported() {
        let p = program(json!({ "effects": [["set", "@entity.title", ["i18n/t", "a:b"]]] }));
        assert_eq!(render_only_violations(&p), vec!["orbitals[0].traits[0].stateMachine.transitions[0].effects[0][2]"]);
    }

    #[test]
    fn a_guard_reading_locale_is_reported() {
        let p = program(json!({ "guard": ["=", "@locale", "ar"], "effects": [] }));
        assert_eq!(render_only_violations(&p), vec!["orbitals[0].traits[0].stateMachine.transitions[0].guard[1]"]);
    }

    #[test]
    fn edge_render_ui_nested_in_do_is_allowed() {
        let p = program(json!({ "effects": [["do", ["render-ui", "main", { "content": ["i18n/t", "a:b"] }]]] }));
        assert!(render_only_violations(&p).is_empty());
    }
}

/// Locales written right to left; every other locale is left to right.
pub const RTL_LOCALES: &[&str] = &["ar", "he", "fa", "ur"];

pub fn locale_direction(locale: &str) -> &'static str {
    if RTL_LOCALES.contains(&locale) {
        "rtl"
    } else {
        "ltr"
    }
}

/// The locale a page path renders under: the declared locale its first
/// segment names, else the first declared one (`None` with no locales).
pub fn locale_of_path<'a>(locales: &'a [String], path: &str) -> Option<&'a str> {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    locales
        .iter()
        .find(|l| l.as_str() == first)
        .or_else(|| locales.first())
        .map(String::as_str)
}

/// Every explicit translation group (`translationOf`) in which one locale holds
/// more than one page, as `(group, locale, paths)` in first-seen order — such a
/// group has no single page per language to link as an alternate.
pub fn duplicate_translation_locales<'a>(
    pages: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    locales: &[String],
) -> Vec<(String, String, Vec<String>)> {
    let mut seen: Vec<(String, String, Vec<String>)> = Vec::new();
    for (path, group) in pages {
        let (Some(group), Some(locale)) = (group, locale_of_path(locales, path)) else {
            continue;
        };
        match seen.iter_mut().find(|(g, l, _)| g == group && l == locale) {
            Some((_, _, paths)) => paths.push(path.to_string()),
            None => seen.push((group.to_string(), locale.to_string(), vec![path.to_string()])),
        }
    }
    seen.retain(|(_, _, paths)| paths.len() > 1);
    seen
}
