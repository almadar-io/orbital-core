//! A `browser/tab-*` request, accepted or refused: the one parser the kernel runs a call on and `orb validate` reads a
//! literal call with. Every key a request may carry is closed, every value typed; the treatments (`hide`, `blur`, ...)
//! become the inline styles the content script sets, so the page script holds no rule.

use crate::agent_ports::find_input_by_address;
use crate::external_inputs::ExternalInput;
use crate::tab::{is_match_pattern, TabAction, TabApply, TabDelivery, TabField, TabImageFormat, TabOp, TabStyle};
use crate::value::{Value, ValueMap};
use url::Url;

/// The visual treatments a program may ask for.
pub const TREATMENTS: [&str; 5] = ["hide", "blur", "highlight", "label", "restore"];

/// The calling trait's position and the program's declared inputs: what a `tab-watch` resolves its `event` address
/// against.
pub struct WatchScope<'a> {
    pub caller_orbital: Option<&'a str>,
    pub caller_trait: &'a str,
    pub inputs: &'a [ExternalInput],
}

/// A `browser/tab-*` call whose request was accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct TabCall {
    pub plan: TabPlan,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TabPlan {
    List { pattern: Option<String> },
    Open { url: String, active: bool },
    Close { tab: i64 },
    Navigate { tab: i64, url: String },
    Query { tab: i64, selector: String, fields: Vec<TabField>, limit: Option<u32>, wait_ms: Option<u64> },
    Watch { watch_id: String, pattern: String, selector: String, fields: Vec<TabField>, deliver: TabDelivery },
    Unwatch { watch_id: String },
    Apply { tab: i64, apply: TabApply, treatment: String },
    ApplyMany { groups: Vec<(i64, Vec<TabApply>)> },
    Act { tab: i64, element_id: String, action: TabAction, text: Option<String> },
    Capture { tab: i64, format: TabImageFormat, quality: Option<u8> },
}

/// Accept the request of `op`, or say why it is refused. Nothing is asked of a host for a refused request.
pub fn parse_tab_call(
    op: TabOp,
    request: &Value,
    watch: Option<&WatchScope<'_>>,
    key: Option<&str>,
) -> Result<TabCall, String> {
    let name = op.name();
    let map = request.as_object().ok_or_else(|| format!("{name}: the request must be an object"))?;
    let plan = match op {
        TabOp::List => {
            closed(name, map, op.request_keys())?;
            let pattern = opt_pattern(name, map, "match")?;
            TabPlan::List { pattern }
        }
        TabOp::Open => {
            closed(name, map, op.request_keys())?;
            let raw = text(name, map, "url")?;
            let mut url = Url::parse(&raw).map_err(|e| format!("{name}: 'url' is not an absolute URL ({e})"))?;
            let params = opt_text_map(name, map, "params")?;
            if !params.is_empty() {
                let mut pairs = url.query_pairs_mut();
                for (param, value) in &params {
                    pairs.append_pair(param, value);
                }
            }
            let url = url.to_string();
            if let Some(pattern) = opt_pattern(name, map, "match")? {
                if !matches_pattern(&pattern, &url) {
                    return Err(format!("{name}: {url} is outside the declared pages {pattern}"));
                }
            }
            TabPlan::Open { url, active: opt_flag(name, map, "active")?.unwrap_or(false) }
        }
        TabOp::Close => {
            closed(name, map, op.request_keys())?;
            TabPlan::Close { tab: tab_id(name, map, "tab")? }
        }
        TabOp::Navigate => {
            closed(name, map, op.request_keys())?;
            let url = text(name, map, "url")?;
            Url::parse(&url).map_err(|e| format!("{name}: 'url' is not an absolute URL ({e})"))?;
            TabPlan::Navigate { tab: tab_id(name, map, "tab")?, url }
        }
        TabOp::Query => {
            closed(name, map, op.request_keys())?;
            let limit = match opt_count(name, map, "limit")? {
                None => None,
                Some(n) => Some(u32::try_from(n).map_err(|_| format!("{name}: 'limit' is too large"))?),
            };
            TabPlan::Query {
                tab: tab_id(name, map, "tab")?,
                selector: selector(name, map)?,
                fields: parse_fields(name, map)?,
                limit,
                wait_ms: opt_count(name, map, "waitMs")?,
            }
        }
        TabOp::Watch => {
            closed(name, map, op.request_keys())?;
            let scope = watch.ok_or_else(|| format!("{name}: needs the calling trait and the program's declared inputs"))?;
            let pattern = text(name, map, "match")?;
            if !is_match_pattern(&pattern) {
                return Err(format!("{name}: 'match' is not a URL match pattern: {pattern}"));
            }
            let selector = selector(name, map)?;
            let fields = parse_fields(name, map)?;
            let address = text(name, map, "event")?;
            let input = find_input_by_address(scope.inputs, &address, scope.caller_orbital, scope.caller_trait)
                .ok_or_else(|| {
                    format!(
                        "{name}: '{address}' names no declared external input from {}.{}",
                        scope.caller_orbital.unwrap_or_default(),
                        scope.caller_trait
                    )
                })?;
            let watch_id = watch_id_of(scope, &pattern, &selector, &fields, &address);
            TabPlan::Watch {
                watch_id,
                pattern,
                selector,
                fields,
                deliver: TabDelivery {
                    orbital: input.orbital.clone(),
                    trait_name: input.trait_name.clone(),
                    event: input.event.clone(),
                },
            }
        }
        TabOp::Unwatch => {
            closed(name, map, op.request_keys())?;
            TabPlan::Unwatch { watch_id: text(name, map, "watchId")? }
        }
        TabOp::Apply => {
            closed(name, map, op.request_keys())?;
            let (apply, treatment) = parse_treatment(name, map)?;
            TabPlan::Apply { tab: tab_id(name, map, "tab")?, apply, treatment }
        }
        TabOp::ApplyMany => {
            closed(name, map, op.request_keys())?;
            let items = match map.get("items") {
                Some(Value::Array(items)) => items,
                _ => return Err(format!("{name}: 'items' must be a list of {{ tab, elementId, treatment, label? }}")),
            };
            let mut groups: Vec<(i64, Vec<TabApply>)> = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let item_map = item
                    .as_object()
                    .ok_or_else(|| format!("{name}: 'items[{index}]' must be an object {{ tab, elementId, treatment, label? }}"))?;
                closed(&format!("{name} items[{index}]"), item_map, &["tab", "elementId", "treatment", "label"])?;
                let tab = tab_id(&format!("{name} items[{index}]"), item_map, "tab")?;
                let (apply, _) = parse_treatment(&format!("{name} items[{index}]"), item_map)?;
                match groups.iter_mut().find(|(t, _)| *t == tab) {
                    Some((_, mine)) => mine.push(apply),
                    None => groups.push((tab, vec![apply])),
                }
            }
            TabPlan::ApplyMany { groups }
        }
        TabOp::Act => {
            closed(name, map, op.request_keys())?;
            let action_name = text(name, map, "action")?;
            let action = TabAction::ALL
                .into_iter()
                .find(|a| a.as_str() == action_name)
                .ok_or_else(|| format!("{name}: 'action' must be one of click, focus, type, got '{action_name}'"))?;
            let text_value = opt_text(name, map, "text")?;
            if action == TabAction::Type && text_value.is_none() {
                return Err(format!("{name}: action 'type' needs 'text'"));
            }
            TabPlan::Act { tab: tab_id(name, map, "tab")?, element_id: text(name, map, "elementId")?, action, text: text_value }
        }
        TabOp::Capture => {
            closed(name, map, op.request_keys())?;
            let format = match opt_text(name, map, "format")? {
                None => TabImageFormat::Png,
                Some(given) => TabImageFormat::ALL
                    .into_iter()
                    .find(|f| f.as_str() == given)
                    .ok_or_else(|| format!("{name}: 'format' must be one of png, jpeg, got '{given}'"))?,
            };
            let quality = match opt_count(name, map, "quality")? {
                None => None,
                Some(_) if format != TabImageFormat::Jpeg => return Err(format!("{name}: 'quality' is for format 'jpeg' only")),
                Some(q) => Some(u8::try_from(q).ok().filter(|q| *q <= 100).ok_or_else(|| format!("{name}: 'quality' must be a whole number from 0 to 100"))?),
            };
            TabPlan::Capture { tab: tab_id(name, map, "tab")?, format, quality }
        }
    };
    Ok(TabCall { plan, key: key.map(str::to_string) })
}

fn closed(name: &str, map: &ValueMap, allowed: &[&str]) -> Result<(), String> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(extra) => Err(format!("{name}: '{extra}' is not a request key (expected {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn present<'a>(map: &'a ValueMap, field: &str) -> Option<&'a Value> {
    map.get(field).filter(|v| !v.is_null())
}

fn text(name: &str, map: &ValueMap, field: &str) -> Result<String, String> {
    match present(map, field) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(format!("{name}: '{field}' must be a string")),
    }
}

fn opt_text(name: &str, map: &ValueMap, field: &str) -> Result<Option<String>, String> {
    match present(map, field) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{name}: '{field}' must be a string")),
    }
}

fn opt_flag(name: &str, map: &ValueMap, field: &str) -> Result<Option<bool>, String> {
    match present(map, field) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{name}: '{field}' must be a boolean")),
    }
}

fn whole(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) if n.is_finite() && n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 => Some(*n as u64),
        _ => None,
    }
}

fn tab_id(name: &str, map: &ValueMap, field: &str) -> Result<i64, String> {
    present(map, field)
        .and_then(whole)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or_else(|| format!("{name}: '{field}' must be a whole number from 0"))
}

fn opt_count(name: &str, map: &ValueMap, field: &str) -> Result<Option<u64>, String> {
    match present(map, field) {
        None => Ok(None),
        Some(value) => whole(value).map(Some).ok_or_else(|| format!("{name}: '{field}' must be a whole number from 0")),
    }
}

fn opt_pattern(name: &str, map: &ValueMap, field: &str) -> Result<Option<String>, String> {
    match opt_text(name, map, field)? {
        Some(pattern) if !is_match_pattern(&pattern) => {
            Err(format!("{name}: '{field}' is not a URL match pattern: {pattern}"))
        }
        other => Ok(other),
    }
}

fn opt_text_map(name: &str, map: &ValueMap, field: &str) -> Result<Vec<(String, String)>, String> {
    match present(map, field) {
        None => Ok(Vec::new()),
        Some(Value::Object(entries)) => entries
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => Ok((k.clone(), s.clone())),
                _ => Err(format!("{name}: '{field}.{k}' must be a string")),
            })
            .collect(),
        Some(_) => Err(format!("{name}: '{field}' must be a map of name to string")),
    }
}

fn selector(name: &str, map: &ValueMap) -> Result<String, String> {
    let selector = text(name, map, "selector")?;
    if selector.trim().is_empty() {
        return Err(format!("{name}: 'selector' must not be empty"));
    }
    Ok(selector)
}

/// The `{name}` references of a field template.
pub fn template_references(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let ident_ok = end > start && !bytes[start].is_ascii_digit();
            if ident_ok && end < bytes.len() && bytes[end] == b'}' {
                out.push(template[start..end].to_string());
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn parse_fields(name: &str, map: &ValueMap) -> Result<Vec<TabField>, String> {
    let shape = "{ selector?, attr?, resolve?, template?, required? }";
    let specs = match map.get("fields") {
        Some(Value::Object(specs)) => specs,
        _ => return Err(format!("{name}: 'fields' must be a map of field name to {shape}")),
    };
    let mut fields = Vec::with_capacity(specs.len());
    for (field, spec) in specs {
        let spec = spec
            .as_object()
            .ok_or_else(|| format!("{name}: 'fields.{field}' must be an object {shape}"))?;
        let at = format!("{name} fields.{field}");
        closed(&at, spec, &["selector", "attr", "resolve", "template", "required"])?;
        fields.push(TabField {
            name: field.clone(),
            selector: opt_text(&at, spec, "selector")?,
            attr: opt_text(&at, spec, "attr")?,
            resolve: opt_flag(&at, spec, "resolve")?.unwrap_or(false),
            template: opt_text(&at, spec, "template")?,
            required: opt_flag(&at, spec, "required")?.unwrap_or(false),
        });
    }
    for field in &fields {
        let Some(template) = &field.template else { continue };
        if field.selector.is_some() || field.attr.is_some() {
            return Err(format!("{name}: 'fields.{}' must be a template or a selector/attr read, not both", field.name));
        }
        for reference in template_references(template) {
            match fields.iter().find(|f| f.name == reference) {
                None => {
                    return Err(format!(
                        "{name}: 'fields.{}.template' must be a template over declared fields; '{reference}' is not one",
                        field.name
                    ))
                }
                Some(target) if target.template.is_some() => {
                    return Err(format!(
                        "{name}: 'fields.{}.template' must be a template over read fields; '{reference}' is itself a template",
                        field.name
                    ))
                }
                Some(_) => {}
            }
        }
    }
    Ok(fields)
}

/// The inline styles and label one treatment comes to. `restore` and a treatment with nothing to show come to
/// neither; a label is shown when one is given or the treatment is `label`.
fn treatment_apply(element_id: String, treatment: &str, label: Option<String>) -> Option<TabApply> {
    let style = |property: &str, value: &str| vec![TabStyle { property: property.to_string(), value: value.to_string() }];
    let (styles, label) = match treatment {
        "hide" => (style("display", "none"), label),
        "blur" => (style("filter", "blur(8px)"), label),
        "highlight" => (style("outline", "3px solid currentColor"), label),
        "label" => (Vec::new(), Some(label.unwrap_or_default())),
        "restore" => (Vec::new(), None),
        _ => return None,
    };
    Some(TabApply { element_id, styles, label })
}

fn parse_treatment(name: &str, map: &ValueMap) -> Result<(TabApply, String), String> {
    let treatment = text(name, map, "treatment")?;
    let element_id = text(name, map, "elementId")?;
    let label = opt_text(name, map, "label")?;
    let apply = treatment_apply(element_id, &treatment, label)
        .ok_or_else(|| format!("{name}: 'treatment' must be one of {}, got '{treatment}'", TREATMENTS.join(", ")))?;
    Ok((apply, treatment))
}

/// A watch's id follows from what it watches (FNV-1a of its identity), so a restarted host attaches the same
/// watch under the same id and a page does not report its elements again.
fn watch_id_of(scope: &WatchScope<'_>, pattern: &str, selector: &str, fields: &[TabField], address: &str) -> String {
    let identity = serde_json::json!([
        scope.caller_orbital.unwrap_or_default(),
        scope.caller_trait,
        pattern,
        selector,
        fields,
        address
    ])
    .to_string();
    let mut hash: u32 = 0x811c_9dc5;
    for byte in identity.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("w-{hash:08x}")
}

/// Chrome match-pattern semantics: `<all_urls>`, or `scheme://host/path` with `*` wildcards.
pub fn matches_pattern(pattern: &str, url: &str) -> bool {
    if pattern == "<all_urls>" {
        return ["http:", "https:", "file:", "ftp:"].iter().any(|scheme| url.starts_with(scheme));
    }
    if !is_match_pattern(pattern) {
        return false;
    }
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let (host, path) = rest.split_at(slash);
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let proto = parsed.scheme();
    let scheme_ok = if scheme == "*" { proto == "http" || proto == "https" } else { proto == scheme };
    if !scheme_ok {
        return false;
    }
    let hostname = parsed.host_str().unwrap_or_default();
    if host != "*" {
        if let Some(base) = host.strip_prefix("*.") {
            if hostname != base && !hostname.ends_with(&format!(".{base}")) {
                return false;
            }
        } else if hostname != host {
            return false;
        }
    }
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    glob_matches(path, &target)
}

/// Whether `text` is exactly `pattern`, a `*` standing for any run of characters.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, rest @ ..] = parts.as_slice() else {
        return pattern == text;
    };
    let Some(mut remaining) = text.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return remaining.is_empty();
    };
    for part in middle {
        match remaining.find(part) {
            Some(at) => remaining = &remaining[at + part.len()..],
            None => return false,
        }
    }
    remaining.ends_with(last) && remaining.len() >= last.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn value(json: serde_json::Value) -> Value {
        serde_json::from_value(json).expect("value")
    }

    fn input(orbital: &str, trait_name: &str, event: &str) -> ExternalInput {
        ExternalInput {
            orbital: orbital.to_string(),
            trait_name: trait_name.to_string(),
            event: event.to_string(),
            payload_schema: Vec::new(),
            description: None,
            synonyms: None,
            tier: None,
        }
    }

    fn inputs() -> Vec<ExternalInput> {
        vec![input("Feed", "Watch", "ITEM_SEEN"), input("Feed", "Other", "ITEM_SEEN")]
    }

    fn scope<'a>(inputs: &'a [ExternalInput], caller_trait: &'a str) -> WatchScope<'a> {
        WatchScope { caller_orbital: Some("Feed"), caller_trait, inputs }
    }

    fn call(op: TabOp, request: serde_json::Value) -> Result<TabCall, String> {
        parse_tab_call(op, &value(request), None, None)
    }

    #[test]
    fn match_patterns_follow_chromes_semantics() {
        for (pattern, url) in [
            ("https://example.com/*", "https://example.com/watch?v=1"),
            ("https://*.example.com/*", "https://www.example.com/a"),
            ("https://*.example.com/*", "https://example.com/a"),
            ("*://example.com/results*", "http://example.com/results?q=x"),
            ("<all_urls>", "https://anything.test/"),
        ] {
            assert!(matches_pattern(pattern, url), "{pattern} matches {url}");
        }
        for (pattern, url) in [
            ("https://example.com/*", "https://example.org/"),
            ("https://*.example.com/*", "https://badexample.com/"),
            ("https://example.com/results*", "https://example.com/watch"),
            ("https://example.com/*", "http://example.com/"),
            ("https://example.com/a*c", "https://example.com/ab"),
            ("not a pattern", "https://example.com/"),
        ] {
            assert!(!matches_pattern(pattern, url), "{pattern} does not match {url}");
        }
    }

    #[test]
    fn a_glob_anchors_both_ends_and_orders_its_parts() {
        assert!(glob_matches("/a*b*c", "/aXbYc"));
        assert!(!glob_matches("/a*b*c", "/aXcYb"));
        assert!(glob_matches("/*", "/"));
        assert!(!glob_matches("/ab", "/abc"));
        assert!(glob_matches("/a*a", "/aa"));
        assert!(!glob_matches("/a*a", "/a"));
    }

    #[test]
    fn open_with_a_match_refuses_a_url_outside_it_before_asking() {
        let inside = call(TabOp::Open, json!({ "url": "https://search.example.com/results", "params": { "q": "x" }, "match": "https://search.example.com/*" }));
        assert!(inside.is_ok());
        let outside = call(TabOp::Open, json!({ "url": "https://elsewhere.example.org/", "match": "https://search.example.com/*" }));
        assert!(outside.unwrap_err().contains("outside the declared pages"));
    }

    #[test]
    fn requests_are_closed_and_typed() {
        for (op, request, fragment) in [
            (TabOp::Close, json!({ "tab": "7" }), "'tab' must be a whole number"),
            (TabOp::Close, json!({ "tab": 1.5 }), "'tab' must be a whole number"),
            (TabOp::Close, json!({ "tab": -1 }), "'tab' must be a whole number"),
            (TabOp::Close, json!({ "tab": 1, "extra": true }), "'extra' is not a request key"),
            (TabOp::Open, json!({ "url": "/relative" }), "not an absolute URL"),
            (TabOp::Open, json!({}), "'url' must be a string"),
            (TabOp::Open, json!({ "url": "https://a.test/", "params": { "q": 1 } }), "'params.q' must be a string"),
            (TabOp::List, json!({ "match": "nope" }), "not a URL match pattern"),
            (TabOp::Query, json!({ "tab": 1, "selector": "  ", "fields": {} }), "'selector' must not be empty"),
            (TabOp::Query, json!({ "tab": 1, "fields": {} }), "'selector' must be a string"),
            (TabOp::Query, json!({ "tab": 1, "selector": ".a" }), "'fields' must be a map"),
            (TabOp::Query, json!({ "tab": 1, "selector": ".a", "fields": {}, "limit": -2 }), "'limit' must be a whole number"),
            (TabOp::Apply, json!({ "tab": 1, "elementId": "e", "treatment": "explode" }), "'treatment' must be one of"),
            (TabOp::Act, json!({ "tab": 1, "elementId": "e", "action": "eval" }), "'action' must be one of"),
            (TabOp::Act, json!({ "tab": 1, "elementId": "e", "action": "type" }), "needs 'text'"),
            (TabOp::ApplyMany, json!({ "items": [{ "tab": 1, "elementId": "e", "treatment": "explode" }] }), "'treatment' must be one of"),
            (TabOp::ApplyMany, json!({ "items": "none" }), "'items' must be a list"),
            (TabOp::Unwatch, json!({}), "'watchId' must be a string"),
            (TabOp::Capture, json!({}), "'tab' must be a whole number"),
            (TabOp::Capture, json!({ "tab": 1, "format": "gif" }), "'format' must be one of png, jpeg"),
            (TabOp::Capture, json!({ "tab": 1, "quality": 50 }), "'quality' is for format 'jpeg' only"),
            (TabOp::Capture, json!({ "tab": 1, "format": "jpeg", "quality": 101 }), "'quality' must be a whole number from 0 to 100"),
            (TabOp::Capture, json!({ "tab": 1, "selector": ".a" }), "'selector' is not a request key"),
        ] {
            let error = call(op, request.clone()).expect_err(&request.to_string());
            assert!(error.contains(fragment), "{error} should contain {fragment}");
        }
    }

    #[test]
    fn control_valid_requests_are_accepted() {
        for (op, request) in [
            (TabOp::List, json!({})),
            (TabOp::Close, json!({ "tab": 0 })),
            (TabOp::Navigate, json!({ "tab": 2, "url": "https://example.com/x" })),
            (TabOp::Query, json!({ "tab": 1, "selector": ".a", "fields": { "t": { "selector": "h2" } }, "limit": 5, "waitMs": 100 })),
            (TabOp::Apply, json!({ "tab": 1, "elementId": "e", "treatment": "label", "label": null })),
            (TabOp::Act, json!({ "tab": 1, "elementId": "e", "action": "click" })),
            (TabOp::ApplyMany, json!({ "items": [] })),
            (TabOp::Capture, json!({ "tab": 1 })),
            (TabOp::Capture, json!({ "tab": 1, "format": "jpeg", "quality": 0 })),
        ] {
            assert!(call(op, request.clone()).is_ok(), "{request}");
        }
        assert_eq!(
            call(TabOp::Capture, json!({ "tab": 4 })).unwrap().plan,
            TabPlan::Capture { tab: 4, format: TabImageFormat::Png, quality: None },
            "png is the default format"
        );
    }

    #[test]
    fn field_specs_are_validated_as_the_page_contract_declares() {
        let q = |fields: serde_json::Value| call(TabOp::Query, json!({ "tab": 1, "selector": ".c", "fields": fields }));
        assert!(q(json!({ "a": { "selector": ".a" }, "b": { "template": "{a}!", "required": true } })).is_ok());
        assert!(q(json!({ "b": { "template": "a{b" } })).is_ok());
        assert!(q(json!({ "b": { "template": "{missing}" } })).unwrap_err().contains("missing"));
        assert!(q(json!({ "a": { "template": "x" }, "b": { "template": "{a}" } })).unwrap_err().contains("itself a template"));
        assert!(q(json!({ "a": { "selector": ".a" }, "b": { "template": "{a}", "selector": ".b" } })).unwrap_err().contains("not both"));
        assert!(q(json!({ "a": { "template": "{a}", "attr": "href" } })).unwrap_err().contains("not both"));
        assert!(q(json!({ "a": { "color": "red" } })).unwrap_err().contains("'color' is not a request key"));
        assert!(q(json!({ "a": "text" })).unwrap_err().contains("must be an object"));
        assert!(q(json!({ "a": { "resolve": "yes" } })).unwrap_err().contains("'resolve' must be a boolean"));
    }

    #[test]
    fn template_references_read_identifiers_in_braces_only() {
        assert_eq!(template_references("{author}/item/{itemId}"), vec!["author", "itemId"]);
        assert!(template_references("a{b").is_empty());
        assert!(template_references("{1a} {} {a-b}").is_empty());
        assert_eq!(template_references("{_x}{y2}"), vec!["_x", "y2"]);
    }

    #[test]
    fn control_a_watch_naming_no_declared_input_or_no_scope_is_refused() {
        let declared = inputs();
        let request = value(json!({ "match": "https://example.com/*", "selector": ".item", "fields": {}, "event": "NOPE" }));
        let error = parse_tab_call(TabOp::Watch, &request, Some(&scope(&declared, "Watch")), None).unwrap_err();
        assert!(error.contains("names no declared external input"), "{error}");
        let error = parse_tab_call(TabOp::Watch, &request, None, None).unwrap_err();
        assert!(error.contains("needs the calling trait"), "{error}");
        let bad_match = value(json!({ "match": "example.com", "selector": ".item", "fields": {}, "event": "ITEM_SEEN" }));
        assert!(parse_tab_call(TabOp::Watch, &bad_match, Some(&scope(&declared, "Watch")), None).is_err());
    }

    #[test]
    fn treatments_become_the_styles_and_label_the_page_script_applies() {
        let styles = |treatment: &str, label: Option<&str>| {
            let apply = treatment_apply("el-1".to_string(), treatment, label.map(str::to_string)).unwrap();
            (apply.styles.iter().map(|s| format!("{}:{}", s.property, s.value)).collect::<Vec<_>>(), apply.label)
        };
        assert_eq!(styles("hide", None), (vec!["display:none".to_string()], None));
        assert_eq!(styles("blur", Some("later")), (vec!["filter:blur(8px)".to_string()], Some("later".to_string())));
        assert_eq!(styles("highlight", None), (vec!["outline:3px solid currentColor".to_string()], None));
        assert_eq!(styles("label", None), (Vec::<String>::new(), Some(String::new())));
        assert_eq!(styles("label", Some("why")), (Vec::<String>::new(), Some("why".to_string())));
        assert_eq!(styles("restore", Some("ignored")), (Vec::<String>::new(), None));
        assert!(treatment_apply("el-1".to_string(), "explode", None).is_none());
    }
}
