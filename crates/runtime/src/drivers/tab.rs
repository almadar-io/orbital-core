//! The `browser/tab-*` effects at the kernel: a [`TabDriver`] is a sans-I/O step machine on the shared driver
//! loop. The request it runs was accepted by `orbital_core_base::tab_call::parse_tab_call`; every result is shaped
//! here, and the host is asked only for raw tab and page operations ([`TabAsk`]).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::driver::{Driver, DriverAnswer, DriverAsk, DriverStep};
use orbital_core_base::tab::{TabApply, TabAsk, TabCommand, TabFailure, TabFailureKind, TabField, TabImageFormat, TabOp};
use orbital_core_base::tab_call::{matches_pattern, TabCall, TabPlan};
use orbital_core_base::value::{Value, ValueMap};

/// The failure payload of a refused request: `{ error, kind: "invalid", key }`.
pub fn refusal_payload(message: &str, key: Option<&str>) -> Value {
    failure_payload(TabFailureKind::Invalid, message, key)
}

fn key_value(key: Option<&str>) -> Value {
    key.map_or(Value::Null, |k| Value::string(k.to_string()))
}

fn failure_payload(kind: TabFailureKind, message: &str, key: Option<&str>) -> Value {
    Value::object(ValueMap::from_iter([
        ("error".to_string(), Value::String(message.to_string().into())),
        ("kind".to_string(), Value::String(kind.as_str().to_string().into())),
        ("key".to_string(), key_value(key)),
    ]))
}

fn whole(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) if n.is_finite() && n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 => Some(*n as u64),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryPhase {
    First,
    AwaitingReady,
    Second,
}

#[derive(Debug)]
enum State {
    List { pattern: Option<String> },
    Open,
    Close,
    Navigate { url: String },
    Query { tab: i64, selector: String, fields: Vec<TabField>, limit: Option<u32>, wait_ms: Option<u64>, phase: QueryPhase },
    Watch { watch_id: String },
    Unwatch,
    Apply { tab: i64, element_id: String, treatment: String },
    ApplyMany { groups: Vec<(i64, Vec<TabApply>)>, at: usize, applied: Vec<Value>, missing: Vec<Value> },
    Act { tab: i64, element_id: String },
    Capture { tab: i64, format: TabImageFormat },
}

/// The step machine of one `browser/tab-*` call.
pub struct TabDriver {
    state: State,
    key: Option<String>,
}

fn ask(ask: TabAsk) -> DriverStep {
    DriverStep::Ask(DriverAsk::Tab { ask })
}

fn command(tab: i64, command: TabCommand) -> DriverStep {
    ask(TabAsk::Command { tab, command })
}

fn element_ref(tab: i64, element_id: &str) -> Value {
    Value::object(ValueMap::from_iter([
        ("tab".to_string(), Value::Number(tab as f64)),
        ("elementId".to_string(), Value::String(element_id.to_string().into())),
    ]))
}

impl TabDriver {
    /// The driver and its first step.
    pub fn start(call: TabCall) -> (Self, DriverStep) {
        let TabCall { plan, key } = call;
        let (state, step) = match plan {
            TabPlan::List { pattern } => (State::List { pattern }, ask(TabAsk::List)),
            TabPlan::Open { url, active } => (State::Open, ask(TabAsk::Open { url, active })),
            TabPlan::Close { tab } => (State::Close, ask(TabAsk::Close { tab })),
            TabPlan::Navigate { tab, url } => {
                let step = ask(TabAsk::Navigate { tab, url: url.clone() });
                (State::Navigate { url }, step)
            }
            TabPlan::Query { tab, selector, fields, limit, wait_ms } => {
                let step = command(tab, TabCommand::Query { selector: selector.clone(), fields: fields.clone(), limit, wait_ms });
                (State::Query { tab, selector, fields, limit, wait_ms, phase: QueryPhase::First }, step)
            }
            TabPlan::Watch { watch_id, pattern, selector, fields, deliver } => {
                let step = ask(TabAsk::Watch { watch_id: watch_id.clone(), match_pattern: pattern, selector, fields, deliver });
                (State::Watch { watch_id }, step)
            }
            TabPlan::Unwatch { watch_id } => (State::Unwatch, ask(TabAsk::Unwatch { watch_id })),
            TabPlan::Apply { tab, apply, treatment } => {
                let element_id = apply.element_id.clone();
                (State::Apply { tab, element_id, treatment }, command(tab, TabCommand::Apply(apply)))
            }
            TabPlan::ApplyMany { groups } => match groups.first() {
                None => {
                    let driver = TabDriver {
                        state: State::ApplyMany { groups: Vec::new(), at: 0, applied: Vec::new(), missing: Vec::new() },
                        key,
                    };
                    let done = driver.done_apply_many(Vec::new(), Vec::new());
                    return (driver, done);
                }
                Some((tab, items)) => {
                    let step = command(*tab, TabCommand::ApplyMany { items: items.clone() });
                    (State::ApplyMany { groups, at: 0, applied: Vec::new(), missing: Vec::new() }, step)
                }
            },
            TabPlan::Act { tab, element_id, action, text } => {
                let step = command(tab, TabCommand::Act { element_id: element_id.clone(), action, text });
                (State::Act { tab, element_id }, step)
            }
            TabPlan::Capture { tab, format, quality } => (State::Capture { tab, format }, ask(TabAsk::Capture { tab, format, quality })),
        };
        (TabDriver { state, key }, step)
    }

    fn ok(&self, mut fields: Vec<(&str, Value)>) -> DriverStep {
        fields.push(("key", key_value(self.key.as_deref())));
        DriverStep::Done(Ok(Value::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())))
    }

    fn fail(&self, kind: TabFailureKind, message: &str) -> DriverStep {
        DriverStep::Done(Err(failure_payload(kind, message, self.key.as_deref())))
    }

    fn malformed(&self, op: TabOp, what: &str) -> DriverStep {
        self.fail(TabFailureKind::Failed, &format!("{}: the host replied {what}", op.name()))
    }

    fn done_apply_many(&self, applied: Vec<Value>, missing: Vec<Value>) -> DriverStep {
        self.ok(vec![("applied", Value::array(applied)), ("missing", Value::array(missing))])
    }
}

fn tab_infos(reply: &Value, pattern: Option<&str>) -> Option<Vec<Value>> {
    let items = reply.as_array()?;
    let mut tabs = Vec::with_capacity(items.len());
    for item in items {
        let map = item.as_object()?;
        let tab = whole(map.get("tab")?)?;
        let url = map.get("url")?.as_str()?.to_string();
        let title = map.get("title")?.as_str()?.to_string();
        let active = map.get("active")?.as_bool()?;
        if pattern.is_some_and(|p| !matches_pattern(p, &url)) {
            continue;
        }
        tabs.push(Value::object(ValueMap::from_iter([
            ("tab".to_string(), Value::Number(tab as f64)),
            ("url".to_string(), Value::String(url.into())),
            ("title".to_string(), Value::String(title.into())),
            ("active".to_string(), Value::Bool(active)),
        ])));
    }
    Some(tabs)
}

fn elements_of(reply: &Value) -> Option<Vec<Value>> {
    let items = reply.as_array()?;
    for item in items {
        let map = item.as_object()?;
        map.get("elementId")?.as_str()?;
        let fields = map.get("fields")?.as_object()?;
        if fields.values().any(|v| v.as_str().is_none()) {
            return None;
        }
    }
    Some(items.clone())
}

fn ids_of(value: Option<&Value>) -> Option<Vec<String>> {
    value?.as_array()?.iter().map(|v| v.as_str().map(str::to_string)).collect()
}

impl Driver for TabDriver {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        let DriverAnswer::Tab(result) = answer else {
            return self.fail(TabFailureKind::Failed, "browser/tab: the host answered with the wrong reply");
        };
        let state = std::mem::replace(&mut self.state, State::Open);
        match state {
            State::List { pattern } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match tab_infos(&reply, pattern.as_deref()) {
                    Some(tabs) => self.ok(vec![("tabs", Value::array(tabs))]),
                    None => self.malformed(TabOp::List, "tabs that are not { tab, url, title, active }"),
                },
            },
            State::Open => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match whole(&reply) {
                    Some(tab) => self.ok(vec![("tab", Value::Number(tab as f64))]),
                    None => self.malformed(TabOp::Open, "a tab that is not a whole number"),
                },
            },
            State::Close => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(_) => self.ok(vec![("closed", Value::Bool(true))]),
            },
            State::Navigate { url } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(_) => self.ok(vec![("url", Value::string(url))]),
            },
            State::Query { tab, selector, fields, limit, wait_ms, phase } => {
                let again = |driver: &mut Self, phase: QueryPhase, wait_ms: Option<u64>, step: DriverStep| {
                    driver.state = State::Query { tab, selector: selector.clone(), fields: fields.clone(), limit, wait_ms, phase };
                    step
                };
                match (phase, result) {
                    (QueryPhase::First, Err(f)) if f.kind == TabFailureKind::NotReady => match wait_ms {
                        Some(timeout_ms) => again(self, QueryPhase::AwaitingReady, wait_ms, ask(TabAsk::AwaitReady { tab, timeout_ms })),
                        None => self.fail(f.kind, &f.message),
                    },
                    (QueryPhase::AwaitingReady, Ok(reply)) => {
                        let ready = reply.as_object().and_then(|m| m.get("ready")).and_then(Value::as_bool);
                        let waited = reply.as_object().and_then(|m| m.get("waitedMs")).and_then(whole);
                        let (Some(ready), Some(waited), Some(budget)) = (ready, waited, wait_ms) else {
                            return self.malformed(TabOp::Query, "a wait result that is not { ready, waitedMs }");
                        };
                        if !ready {
                            return self.fail(
                                TabFailureKind::NotReady,
                                &format!("Tab {tab} did not load its page within {budget}ms"),
                            );
                        }
                        let remaining = budget.saturating_sub(waited);
                        let step = command(
                            tab,
                            TabCommand::Query { selector: selector.clone(), fields: fields.clone(), limit, wait_ms: Some(remaining) },
                        );
                        again(self, QueryPhase::Second, Some(remaining), step)
                    }
                    (_, Err(f)) => self.fail(f.kind, &f.message),
                    (_, Ok(reply)) => match elements_of(&reply) {
                        Some(elements) => self.ok(vec![("tab", Value::Number(tab as f64)), ("elements", Value::array(elements))]),
                        None => self.malformed(TabOp::Query, "elements that are not { elementId, fields }"),
                    },
                }
            }
            State::Watch { watch_id } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(_) => self.ok(vec![("watchId", Value::string(watch_id))]),
            },
            State::Unwatch => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match reply.as_bool() {
                    Some(stopped) => self.ok(vec![("stopped", Value::Bool(stopped))]),
                    None => self.malformed(TabOp::Unwatch, "a stop that is not true or false"),
                },
            },
            State::Apply { tab, element_id, treatment } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match reply.as_bool() {
                    Some(applied) => self.ok(vec![
                        ("applied", Value::Bool(applied)),
                        ("tab", Value::Number(tab as f64)),
                        ("elementId", Value::string(element_id)),
                        ("treatment", Value::string(treatment)),
                    ]),
                    None => self.malformed(TabOp::Apply, "a result that is not true or false"),
                },
            },
            State::ApplyMany { groups, at, mut applied, mut missing } => {
                let (tab, items) = &groups[at];
                let reply = result.ok().and_then(|reply| {
                    let map = reply.as_object()?;
                    Some((ids_of(map.get("applied"))?, ids_of(map.get("missing"))?))
                });
                match reply {
                    Some((done, gone)) => {
                        applied.extend(done.iter().map(|id| element_ref(*tab, id)));
                        missing.extend(gone.iter().map(|id| element_ref(*tab, id)));
                    }
                    None => missing.extend(items.iter().map(|item| element_ref(*tab, &item.element_id))),
                }
                match groups.get(at + 1) {
                    Some((next_tab, next_items)) => {
                        let step = command(*next_tab, TabCommand::ApplyMany { items: next_items.clone() });
                        self.state = State::ApplyMany { groups, at: at + 1, applied, missing };
                        step
                    }
                    None => self.done_apply_many(applied, missing),
                }
            }
            State::Act { tab, element_id } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match reply.as_bool() {
                    Some(done) => self.ok(vec![
                        ("done", Value::Bool(done)),
                        ("tab", Value::Number(tab as f64)),
                        ("elementId", Value::string(element_id)),
                    ]),
                    None => self.malformed(TabOp::Act, "a result that is not true or false"),
                },
            },
            State::Capture { tab, format } => match result {
                Err(f) => self.fail(f.kind, &f.message),
                Ok(reply) => match reply.as_str().and_then(|url| url.strip_prefix(format.data_url_prefix())).filter(|image| !image.is_empty()) {
                    Some(image) => self.ok(vec![
                        ("tab", Value::Number(tab as f64)),
                        ("format", Value::string(format.as_str().to_string())),
                        ("image", Value::string(image.to_string())),
                    ]),
                    None => self.malformed(TabOp::Capture, &format!("an image that is not a {} data URL", format.as_str())),
                },
            },
        }
    }
}

/// A host failure as the call's failure payload.
pub fn host_failure_payload(failure: &TabFailure, key: Option<&str>) -> Value {
    failure_payload(failure.kind, &failure.message, key)
}


#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::external_inputs::ExternalInput;
    use orbital_core_base::tab_call::{parse_tab_call, WatchScope};
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

    /// Run a call against scripted host answers; returns the asks made and the final step.
    fn run(call: TabCall, answers: Vec<Result<Value, TabFailure>>) -> (Vec<TabAsk>, DriverStep) {
        let (mut driver, mut step) = TabDriver::start(call);
        let mut asks = Vec::new();
        let mut answers = answers.into_iter();
        loop {
            match step {
                DriverStep::Ask(DriverAsk::Tab { ask }) => {
                    asks.push(ask);
                    let answer = answers.next().expect("the script ran out of answers");
                    step = driver.resume(DriverAnswer::Tab(answer));
                }
                other => return (asks, other),
            }
        }
    }

    fn done_ok(step: DriverStep) -> Value {
        match step {
            DriverStep::Done(Ok(v)) => v,
            other => panic!("expected success, got {other:?}"),
        }
    }

    fn done_err(step: DriverStep) -> Value {
        match step {
            DriverStep::Done(Err(v)) => v,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    fn get<'a>(v: &'a Value, field: &str) -> &'a Value {
        v.as_object().and_then(|m| m.get(field)).unwrap_or_else(|| panic!("no {field} in {v:?}"))
    }

    fn not_ready() -> TabFailure {
        TabFailure::new(TabFailureKind::NotReady, "Tab 4 has no page script")
    }

    #[test]
    fn list_filters_the_hosts_tabs_by_the_match_pattern() {
        let tabs = value(json!([
            { "tab": 1, "url": "https://example.com/a", "title": "A", "active": true },
            { "tab": 2, "url": "https://other.test/", "title": "B", "active": false }
        ]));
        let (asks, step) = run(call(TabOp::List, json!({ "match": "https://example.com/*" })).unwrap(), vec![Ok(tabs.clone())]);
        assert_eq!(asks, vec![TabAsk::List]);
        let out = done_ok(step);
        assert_eq!(get(&out, "tabs").as_array().unwrap().len(), 1);
        assert!(get(&out, "key").is_null());
        let (_, all) = run(call(TabOp::List, json!({})).unwrap(), vec![Ok(tabs)]);
        assert_eq!(get(&done_ok(all), "tabs").as_array().unwrap().len(), 2);
    }

    #[test]
    fn open_encodes_params_into_the_url_keeping_the_ones_it_has() {
        let open = |request| match run(call(TabOp::Open, request).unwrap(), vec![Ok(Value::Number(99.0))]) {
            (asks, step) => (asks, done_ok(step)),
        };
        let (asks, out) = open(json!({ "url": "https://example.com/results", "params": { "q": "a&b c#d" } }));
        assert_eq!(asks, vec![TabAsk::Open { url: "https://example.com/results?q=a%26b+c%23d".to_string(), active: false }]);
        assert_eq!(get(&out, "tab"), &Value::Number(99.0));
        let (asks, _) = open(json!({ "url": "https://example.com/r?lang=en", "params": { "q": "x" }, "active": true }));
        assert_eq!(asks, vec![TabAsk::Open { url: "https://example.com/r?lang=en&q=x".to_string(), active: true }]);
        let (asks, _) = open(json!({ "url": "https://example.com/r?q=a" }));
        assert_eq!(asks, vec![TabAsk::Open { url: "https://example.com/r?q=a".to_string(), active: false }]);
    }

    #[test]
    fn query_asks_the_page_and_shapes_the_elements() {
        let elements = value(json!([{ "elementId": "el-1", "fields": { "title": "x" } }]));
        let fields = json!({ "title": { "selector": ".title" } });
        let accepted = parse_tab_call(
            TabOp::Query,
            &value(json!({ "tab": 7, "selector": ".item", "fields": fields, "limit": 3 })),
            None,
            Some("q-1"),
        )
        .unwrap();
        let (asks, step) = run(accepted, vec![Ok(elements.clone())]);
        assert_eq!(asks.len(), 1);
        let TabAsk::Command { tab: 7, command: TabCommand::Query { selector, limit: Some(3), wait_ms: None, .. } } = &asks[0] else {
            panic!("expected a query command, got {:?}", asks[0])
        };
        assert_eq!(selector, ".item");
        let out = done_ok(step);
        assert_eq!(get(&out, "tab"), &Value::Number(7.0));
        assert_eq!(get(&out, "elements"), &elements);
        assert_eq!(get(&out, "key"), &Value::string("q-1"));
    }

    #[test]
    fn query_with_a_wait_on_a_page_not_loaded_waits_for_it_then_reads_for_the_rest_of_the_wait() {
        let accepted = call(TabOp::Query, json!({ "tab": 4, "selector": ".item", "fields": {}, "waitMs": 1000 })).unwrap();
        let elements = value(json!([{ "elementId": "el-1", "fields": {} }]));
        let wait = value(json!({ "ready": true, "waitedMs": 250 }));
        let (asks, step) = run(accepted, vec![Err(not_ready()), Ok(wait), Ok(elements)]);
        assert_eq!(asks.len(), 3);
        assert_eq!(asks[1], TabAsk::AwaitReady { tab: 4, timeout_ms: 1000 });
        assert!(matches!(&asks[2], TabAsk::Command { command: TabCommand::Query { wait_ms: Some(750), .. }, .. }));
        assert_eq!(get(&done_ok(step), "tab"), &Value::Number(4.0));
    }

    #[test]
    fn edge_a_page_that_never_loads_within_the_wait_fails_not_ready() {
        let accepted = call(TabOp::Query, json!({ "tab": 4, "selector": ".item", "fields": {}, "waitMs": 30 })).unwrap();
        let wait = value(json!({ "ready": false, "waitedMs": 30 }));
        let (asks, step) = run(accepted, vec![Err(not_ready()), Ok(wait)]);
        assert_eq!(asks.len(), 2);
        let failure = done_err(step);
        assert_eq!(get(&failure, "kind"), &Value::string("not-ready"));
        assert_eq!(get(&failure, "error"), &Value::string("Tab 4 did not load its page within 30ms"));
    }

    #[test]
    fn control_without_a_wait_a_page_that_has_not_loaded_fails_at_once() {
        let accepted = call(TabOp::Query, json!({ "tab": 4, "selector": ".item", "fields": {} })).unwrap();
        let (asks, step) = run(accepted, vec![Err(not_ready())]);
        assert_eq!(asks.len(), 1);
        assert_eq!(get(&done_err(step), "kind"), &Value::string("not-ready"));
    }

    #[test]
    fn control_a_second_not_ready_after_the_wait_is_a_failure_not_another_wait() {
        let accepted = call(TabOp::Query, json!({ "tab": 4, "selector": ".item", "fields": {}, "waitMs": 100 })).unwrap();
        let wait = value(json!({ "ready": true, "waitedMs": 10 }));
        let (asks, step) = run(accepted, vec![Err(not_ready()), Ok(wait), Err(not_ready())]);
        assert_eq!(asks.len(), 3);
        assert_eq!(get(&done_err(step), "kind"), &Value::string("not-ready"));
    }

    #[test]
    fn a_host_reply_that_is_not_the_shape_is_a_failure_not_data() {
        let accepted = call(TabOp::Query, json!({ "tab": 4, "selector": ".item", "fields": {} })).unwrap();
        let (_, step) = run(accepted, vec![Ok(value(json!([{ "elementId": 1 }])))]);
        let failure = done_err(step);
        assert_eq!(get(&failure, "kind"), &Value::string("failed"));
        let (_, step) = run(call(TabOp::List, json!({})).unwrap(), vec![Ok(value(json!("no")))]);
        assert_eq!(get(&done_err(step), "kind"), &Value::string("failed"));
    }

    #[test]
    fn watch_resolves_its_event_address_from_the_calling_trait_and_names_the_watch_by_what_it_watches() {
        let declared = inputs();
        let request = |event: &str, selector: &str| {
            value(json!({ "match": "https://example.com/*", "selector": selector, "fields": { "title": { "selector": ".title" } }, "event": event }))
        };
        let watch = |event: &str, selector: &str, caller: &str| {
            parse_tab_call(TabOp::Watch, &request(event, selector), Some(&scope(&declared, caller)), None).unwrap()
        };
        let (asks, step) = run(watch("ITEM_SEEN", ".item", "Watch"), vec![Ok(Value::Null)]);
        let TabAsk::Watch { watch_id, match_pattern, deliver, .. } = &asks[0] else { panic!("expected a watch ask") };
        assert_eq!(match_pattern, "https://example.com/*");
        assert_eq!((deliver.orbital.as_str(), deliver.trait_name.as_str(), deliver.event.as_str()), ("Feed", "Watch", "ITEM_SEEN"));
        assert_eq!(get(&done_ok(step), "watchId"), &Value::string(watch_id.clone()));
        let id_of = |event: &str, selector: &str, caller: &str| match run(watch(event, selector, caller), vec![Ok(Value::Null)]).0.remove(0) {
            TabAsk::Watch { watch_id, .. } => watch_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(id_of("ITEM_SEEN", ".item", "Watch"), *watch_id);
        assert_ne!(id_of("ITEM_SEEN", ".other", "Watch"), *watch_id);
        assert_ne!(id_of("ITEM_SEEN", ".item", "Other"), *watch_id);
        let sibling = run(watch("Other.ITEM_SEEN", ".item", "Watch"), vec![Ok(Value::Null)]).0.remove(0);
        let TabAsk::Watch { deliver, .. } = sibling else { panic!("expected a watch ask") };
        assert_eq!(deliver.trait_name, "Other");
    }

    #[test]
    fn unwatch_reports_whether_the_host_kept_the_watch() {
        let (asks, step) = run(call(TabOp::Unwatch, json!({ "watchId": "w-1" })).unwrap(), vec![Ok(Value::Bool(true))]);
        assert_eq!(asks, vec![TabAsk::Unwatch { watch_id: "w-1".to_string() }]);
        assert_eq!(get(&done_ok(step), "stopped"), &Value::Bool(true));
        let (_, step) = run(call(TabOp::Unwatch, json!({ "watchId": "w-2" })).unwrap(), vec![Ok(Value::Bool(false))]);
        assert_eq!(get(&done_ok(step), "stopped"), &Value::Bool(false));
    }

    #[test]
    fn apply_and_act_say_which_element_they_acted_on() {
        let (asks, step) = run(call(TabOp::Apply, json!({ "tab": 3, "elementId": "el-2", "treatment": "hide" })).unwrap(), vec![Ok(Value::Bool(true))]);
        assert!(matches!(&asks[0], TabAsk::Command { tab: 3, command: TabCommand::Apply(a) } if a.styles.len() == 1));
        let out = done_ok(step);
        assert_eq!(get(&out, "applied"), &Value::Bool(true));
        assert_eq!(get(&out, "treatment"), &Value::string("hide"));
        assert_eq!(get(&out, "elementId"), &Value::string("el-2"));
        let (_, step) = run(call(TabOp::Act, json!({ "tab": 3, "elementId": "el-2", "action": "focus" })).unwrap(), vec![Ok(Value::Bool(true))]);
        let out = done_ok(step);
        assert_eq!(get(&out, "done"), &Value::Bool(true));
        assert_eq!(get(&out, "tab"), &Value::Number(3.0));
    }

    #[test]
    fn capture_asks_for_the_tab_in_its_format_and_hands_back_the_image_without_its_data_url_prefix() {
        let (asks, step) = run(
            call(TabOp::Capture, json!({ "tab": 5, "format": "jpeg", "quality": 70 })).unwrap(),
            vec![Ok(Value::string("data:image/jpeg;base64,/9j/4AAQ"))],
        );
        assert_eq!(asks, vec![TabAsk::Capture { tab: 5, format: TabImageFormat::Jpeg, quality: Some(70) }]);
        let out = done_ok(step);
        assert_eq!(get(&out, "tab"), &Value::Number(5.0));
        assert_eq!(get(&out, "format"), &Value::string("jpeg"));
        assert_eq!(get(&out, "image"), &Value::string("/9j/4AAQ"));
    }

    #[test]
    fn control_a_capture_reply_in_another_format_or_empty_is_malformed_and_a_host_failure_keeps_its_kind() {
        for reply in ["data:image/png;base64,iVBOR", "data:image/jpeg;base64,", "not a url"] {
            let (_, step) = run(call(TabOp::Capture, json!({ "tab": 1, "format": "jpeg" })).unwrap(), vec![Ok(Value::string(reply.to_string()))]);
            let failure = done_err(step);
            assert_eq!(get(&failure, "kind"), &Value::string("failed"), "{reply}");
        }
        let hidden = TabFailure::new(TabFailureKind::NotReady, "Tab 1 cannot be captured");
        let (_, step) = run(call(TabOp::Capture, json!({ "tab": 1 })).unwrap(), vec![Err(hidden)]);
        assert_eq!(get(&done_err(step), "kind"), &Value::string("not-ready"));
    }

    #[test]
    fn control_a_vanished_element_fails_the_apply_with_the_hosts_kind() {
        let gone = TabFailure::new(TabFailureKind::NotFound, "No page element el-9");
        let (_, step) = run(call(TabOp::Apply, json!({ "tab": 3, "elementId": "el-9", "treatment": "hide" })).unwrap(), vec![Err(gone)]);
        let failure = done_err(step);
        assert_eq!(get(&failure, "kind"), &Value::string("not-found"));
        assert_eq!(get(&failure, "error"), &Value::string("No page element el-9"));
    }

    #[test]
    fn apply_many_sends_each_tab_its_items_in_one_command_and_lists_what_was_applied_and_what_is_missing() {
        let accepted = call(
            TabOp::ApplyMany,
            json!({ "items": [
                { "tab": 1, "elementId": "el-1", "treatment": "hide" },
                { "tab": 2, "elementId": "el-7", "treatment": "blur", "label": "Not now" },
                { "tab": 1, "elementId": "gone", "treatment": "restore" }
            ] }),
        )
        .unwrap();
        let tab_one = value(json!({ "applied": ["el-1"], "missing": ["gone"] }));
        let tab_two = value(json!({ "applied": ["el-7"], "missing": [] }));
        let (asks, step) = run(accepted, vec![Ok(tab_one), Ok(tab_two)]);
        assert_eq!(asks.len(), 2);
        assert!(matches!(&asks[0], TabAsk::Command { tab: 1, command: TabCommand::ApplyMany { items } } if items.len() == 2));
        assert!(matches!(&asks[1], TabAsk::Command { tab: 2, command: TabCommand::ApplyMany { items } } if items.len() == 1));
        let out = done_ok(step);
        assert_eq!(get(&out, "applied"), &value(json!([{ "tab": 1, "elementId": "el-1" }, { "tab": 2, "elementId": "el-7" }])));
        assert_eq!(get(&out, "missing"), &value(json!([{ "tab": 1, "elementId": "gone" }])));
    }

    #[test]
    fn edge_the_items_of_a_tab_that_has_closed_are_missing_not_a_failed_batch() {
        let accepted = call(
            TabOp::ApplyMany,
            json!({ "items": [{ "tab": 404, "elementId": "el-1", "treatment": "hide" }, { "tab": 1, "elementId": "el-2", "treatment": "hide" }] }),
        )
        .unwrap();
        let closed = TabFailure::new(TabFailureKind::Failed, "No tab with id: 404");
        let ok = value(json!({ "applied": ["el-2"], "missing": [] }));
        let (_, step) = run(accepted, vec![Err(closed), Ok(ok)]);
        let out = done_ok(step);
        assert_eq!(get(&out, "applied"), &value(json!([{ "tab": 1, "elementId": "el-2" }])));
        assert_eq!(get(&out, "missing"), &value(json!([{ "tab": 404, "elementId": "el-1" }])));
    }

    #[test]
    fn control_an_empty_apply_many_asks_nothing_and_succeeds() {
        let (asks, step) = run(call(TabOp::ApplyMany, json!({ "items": [] })).unwrap(), Vec::new());
        assert!(asks.is_empty());
        let out = done_ok(step);
        assert_eq!(get(&out, "applied"), &Value::array(Vec::new()));
    }

    #[test]
    fn close_and_navigate_echo_what_they_did() {
        let (asks, step) = run(call(TabOp::Close, json!({ "tab": 9 })).unwrap(), vec![Ok(Value::Null)]);
        assert_eq!(asks, vec![TabAsk::Close { tab: 9 }]);
        assert_eq!(get(&done_ok(step), "closed"), &Value::Bool(true));
        let (_, step) = run(call(TabOp::Navigate, json!({ "tab": 9, "url": "https://example.com/x" })).unwrap(), vec![Ok(Value::Null)]);
        assert_eq!(get(&done_ok(step), "url"), &Value::string("https://example.com/x"));
    }

    #[test]
    fn a_host_failure_becomes_the_failure_payload_with_its_kind_and_the_key() {
        let accepted = parse_tab_call(TabOp::Close, &value(json!({ "tab": 9 })), None, Some("k")).unwrap();
        let (_, step) = run(accepted, vec![Err(TabFailure::new(TabFailureKind::Unavailable, "no tabs"))]);
        let failure = done_err(step);
        assert_eq!(get(&failure, "kind"), &Value::string("unavailable"));
        assert_eq!(get(&failure, "key"), &Value::string("k"));
        assert_eq!(refusal_payload("bad", None), value(json!({ "error": "bad", "kind": "invalid", "key": null })));
    }
}
