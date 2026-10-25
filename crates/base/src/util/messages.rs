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
    /// The key is not a string literal, so it cannot be checked against a catalog.
    Dynamic { path: String },
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
                let key_path = format!("{path}[1]");
                match items.get_mut(1) {
                    Some(Value::String(key)) if !key.is_empty() && !key.contains(NAMESPACE_SEPARATOR) => {
                        *key = format!("{namespace}{NAMESPACE_SEPARATOR}{key}");
                    }
                    Some(Value::String(key)) => issues.push(MessageKeyIssue::Invalid {
                        path: key_path,
                        key: key.clone(),
                    }),
                    _ => issues.push(MessageKeyIssue::Dynamic { path: key_path }),
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
    fn a_computed_key_is_reported_as_dynamic() {
        let mut v = json!({ "content": ["i18n/t", ["str/concat", "post.", "@entity.slug"]] });
        assert_eq!(
            qualify_message_keys(&mut v, "home"),
            vec![MessageKeyIssue::Dynamic { path: ".content[1]".into() }]
        );
    }

    #[test]
    fn edge_a_missing_key_is_reported_as_dynamic() {
        let mut v = json!(["i18n/t"]);
        assert_eq!(qualify_message_keys(&mut v, "home"), vec![MessageKeyIssue::Dynamic { path: "[1]".into() }]);
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
    namespace: &str,
    locales: Option<&[String]>,
    catalogs: &std::collections::BTreeMap<String, Option<Value>>,
) -> Vec<CatalogIssue> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut issues = Vec::new();
    let mut calls = Vec::new();
    let prefix = format!("{namespace}{NAMESPACE_SEPARATOR}");
    collect_calls(program, &prefix, &mut calls, &mut issues);

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
        assert!(check_catalogs(&program(), "home", Some(&locales()), &complete()).is_empty());
    }

    #[test]
    fn a_key_missing_from_en_is_reported() {
        let mut c = complete();
        c.insert("en".into(), Some(json!({ "posts.count": "{{n}} posts" })));
        assert_eq!(codes(&check_catalogs(&program(), "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_MISSING"]);
    }

    #[test]
    fn a_key_missing_from_another_declared_locale_is_untranslated() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": "مرحبا" })));
        assert_eq!(codes(&check_catalogs(&program(), "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_UNTRANSLATED"]);
    }

    #[test]
    fn edge_an_absent_catalog_file_reports_every_key() {
        let mut c = complete();
        c.insert("ar".into(), None);
        assert_eq!(
            codes(&check_catalogs(&program(), "home", Some(&locales()), &c)),
            vec!["ORB_I18N_KEY_UNTRANSLATED", "ORB_I18N_KEY_UNTRANSLATED"]
        );
    }

    #[test]
    fn placeholders_must_match_the_params_in_every_locale() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": "مرحبا", "posts.count": "{{count}} مقالات" })));
        let issues = check_catalogs(&program(), "home", Some(&locales()), &c);
        assert_eq!(codes(&issues), vec!["ORB_I18N_PARAM_MISMATCH"]);
    }

    #[test]
    fn edge_params_that_are_not_an_object_literal_are_rejected() {
        let p = json!(["i18n/t", "home:posts.count", "@config.counts"]);
        let c = catalogs(json!({ "posts.count": "{{n}}" }), json!({ "posts.count": "{{n}}" }));
        assert_eq!(codes(&check_catalogs(&p, "home", Some(&locales()), &c)), vec!["ORB_I18N_PARAM_MISMATCH"]);
    }

    #[test]
    fn an_unused_catalog_key_is_reported() {
        let mut c = complete();
        c.insert("en".into(), Some(json!({ "hero.title": "Hello", "posts.count": "{{n}} posts", "stale": "x" })));
        assert_eq!(codes(&check_catalogs(&program(), "home", Some(&locales()), &c)), vec!["ORB_I18N_KEY_UNUSED"]);
    }

    #[test]
    fn using_i18n_without_locales_is_rejected_and_control_without_calls_is_fine() {
        assert_eq!(codes(&check_catalogs(&program(), "home", None, &BTreeMap::new())), vec!["ORB_I18N_NO_LOCALES"]);
        assert!(check_catalogs(&json!(["do"]), "home", None, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn edge_locales_without_en_are_rejected() {
        let only_ar = vec!["ar".to_string()];
        assert_eq!(codes(&check_catalogs(&program(), "home", Some(&only_ar), &BTreeMap::new())), vec!["ORB_I18N_NO_LOCALES"]);
    }

    #[test]
    fn a_malformed_or_undeclared_catalog_is_invalid() {
        let mut c = complete();
        c.insert("ar".into(), Some(json!({ "hero.title": { "nested": "x" }, "posts.count": "{{n}}" })));
        c.insert("sl".into(), Some(json!({})));
        let found = codes(&check_catalogs(&program(), "home", Some(&locales()), &c));
        assert!(found.iter().filter(|c| **c == "ORB_I18N_CATALOG_INVALID").count() == 2, "{found:?}");
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
