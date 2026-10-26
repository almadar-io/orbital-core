//! The three kernel abilities an LLM tool loop is written with: `program/inputs` (reflection over the
//! running program's declared external inputs), `program/dispatch` (fire one of them AS THE CALLER) and
//! `persist/read-named` (read an entity chosen at run time through its `@read` policy for the caller).
//!
//! This module is the pure half both execution paths share: what an input reflects as, the JSON Schema of a
//! payload, the check of an argument against a stated payload, how an address resolves and what a dispatch
//! plan is before the host is asked. The kernel (`orbital-core-runtime` `kernel/agent_ports`) runs the plan;
//! the host runs the dispatch. Twin of `@almadar/core` `src/access/agentPorts.ts`; one corpus
//! (`crates/runtime/tests/fixtures/agent-ports/vectors.json`) pins both.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::external_inputs::ExternalInput;
use crate::schema::types::{
    split_event_address, EntityRef, ListenSource, OrbitalSchema, PayloadField, Persistence,
};
use crate::value::{Value, ValueMap};

pub const AGENT_PORT_OPERATORS: [&str; 4] =
    ["program/inputs", "program/dispatch", "persist/read-named", "program/self"];

/// The `errors[].code` of a `persist/read-named` for a name the program does not persist.
pub const UNKNOWN_ENTITY_CODE: &str = "unknown-entity";

const REFUSED_ARGUMENTS: &str = "arguments must be a JSON object";
const MISMATCHED_ARGUMENTS: &str = "the arguments do not match the tool";

/// Whether `name` is one of the three agent-port operators.
pub fn is_agent_port_operator(name: &str) -> bool {
    AGENT_PORT_OPERATORS.contains(&name)
}

/// An operator-level failure: the message, and the typed issues (`{ code, message }`) it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPortFailure {
    pub error: String,
    pub errors: Vec<Value>,
}

impl AgentPortFailure {
    pub fn plain(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            errors: Vec::new(),
        }
    }
}

fn text(s: &str) -> Value {
    Value::string(s.to_string())
}

fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = ValueMap::default();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::object(map)
}

fn through_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| serde_json::from_value::<Value>(json).ok())
        .unwrap_or(Value::Null)
}

/// A refusal the model is answered with: `{ ok: false, error }`.
fn refusal(error: &str, issues: Option<Vec<Value>>) -> Value {
    let mut map = ValueMap::default();
    map.insert("ok".to_string(), Value::Bool(false));
    map.insert("error".to_string(), text(error));
    if let Some(issues) = issues {
        map.insert("issues".to_string(), Value::array(issues));
    }
    Value::object(map)
}

/// One declared input with its qualified address and the JSON Schema of its payload; `payload` states the
/// payload the model is shown in place of the input's own.
pub fn reflect_input(input: &ExternalInput, payload: Option<&[PayloadField]>) -> Value {
    let payload = payload.unwrap_or(&input.payload_schema);
    let mut map = ValueMap::default();
    map.insert("orbital".to_string(), text(&input.orbital));
    map.insert("trait".to_string(), text(&input.trait_name));
    map.insert("event".to_string(), text(&input.event));
    map.insert(
        "address".to_string(),
        text(&format!(
            "{}.{}.{}",
            input.orbital, input.trait_name, input.event
        )),
    );
    if let Some(description) = &input.description {
        map.insert("description".to_string(), text(description));
    }
    if let Some(synonyms) = &input.synonyms {
        map.insert("synonyms".to_string(), text(synonyms));
    }
    if let Some(tier) = &input.tier {
        map.insert("tier".to_string(), text(tier));
    }
    map.insert("payloadSchema".to_string(), through_json(&payload));
    map.insert(
        "parameters".to_string(),
        payload_schema_to_json_schema(payload),
    );
    Value::object(map)
}

const INPUTS_OPTIONS: [&str; 4] = ["select", "state", "stats", "trace"];

/// The runtime reads a `program/inputs` also asks for: the running trait states of one orbital with their entity
/// (`state`), the dispatch counters (`stats`) and the host's last `trace` dispatch steps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputsReads {
    pub state: Option<String>,
    pub stats: bool,
    pub trace: Option<usize>,
}

/// The [`InputsReads`] of a `program/inputs`'s options; the options were checked by [`plan_inputs`].
pub fn inputs_reads(args: &[Value]) -> Result<InputsReads, String> {
    let Some(Value::Object(options)) = args.first() else {
        return Ok(InputsReads::default());
    };
    let state = match options.get("state") {
        None => None,
        Some(Value::String(orbital)) if !orbital.is_empty() => Some(orbital.clone()),
        Some(_) => return Err("program/inputs: state must name an orbital".to_string()),
    };
    let stats = match options.get("stats") {
        None => false,
        Some(Value::Bool(on)) => *on,
        Some(_) => return Err("program/inputs: stats must be true or false".to_string()),
    };
    let trace = match options.get("trace") {
        None => None,
        Some(Value::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as usize),
        Some(_) => return Err("program/inputs: trace must be a whole number of steps".to_string()),
    };
    Ok(InputsReads { state: state.map(|v| v.into_inner()), stats, trace })
}

const SELECT_SHAPE: &str = "program/inputs: select must be a list of { event, payload? }";

struct Selection {
    event: String,
    payload: Option<Vec<PayloadField>>,
}

fn select_option(raw: Option<&Value>) -> Result<Vec<Selection>, String> {
    let options = match raw {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(options)) => options,
        Some(_) => return Err("program/inputs: the options must be an object".to_string()),
    };
    if let Some(key) = options.keys().find(|key| !INPUTS_OPTIONS.contains(&key.as_str())) {
        return Err(format!("program/inputs: unsupported option \"{key}\""));
    }
    let Some(select) = options.get("select") else {
        return Ok(Vec::new());
    };
    let Value::Array(entries) = select else {
        return Err(SELECT_SHAPE.to_string());
    };
    let mut chosen = Vec::with_capacity(entries.len());
    for entry in entries {
        let Value::Object(fields) = entry else {
            return Err(SELECT_SHAPE.to_string());
        };
        if fields.keys().any(|key| key != "event" && key != "payload") {
            return Err(SELECT_SHAPE.to_string());
        }
        let Some(Value::String(event)) = fields.get("event") else {
            return Err(SELECT_SHAPE.to_string());
        };
        let payload = match fields.get("payload") {
            None => None,
            Some(declared) => Some(
                serde_json::to_value(declared)
                    .ok()
                    .and_then(|json| serde_json::from_value::<Vec<PayloadField>>(json).ok())
                    .ok_or_else(|| {
                        "program/inputs: a select payload must be a list of payload fields"
                            .to_string()
                    })?,
            ),
        };
        chosen.push(Selection {
            event: event.clone().into_inner(),
            payload,
        });
    }
    Ok(chosen)
}

/// `program/inputs`: every declared input, the persisted entities `persist/read-named` accepts and, for an
/// optional `{ select: [{ event, payload? }] }`, the inputs those addresses name, in order, each with the stated
/// payload in place of its own. A selected address that is not a declared external input fails the call.
pub fn plan_inputs(
    args: &[Value],
    inputs: &[ExternalInput],
    entities: &[String],
    caller_orbital: Option<&str>,
    caller_trait: &str,
) -> Result<Value, AgentPortFailure> {
    if args.len() > 1 {
        return Err(AgentPortFailure::plain(
            "program/inputs: takes only options",
        ));
    }
    let chosen = select_option(args.first()).map_err(AgentPortFailure::plain)?;
    inputs_reads(args).map_err(AgentPortFailure::plain)?;
    let mut selected = Vec::with_capacity(chosen.len());
    for choice in &chosen {
        let Some(input) =
            find_input_by_address(inputs, &choice.event, caller_orbital, caller_trait)
        else {
            return Err(AgentPortFailure::plain(format!(
                "program/inputs: '{}' is not a declared external input",
                choice.event
            )));
        };
        selected.push(reflect_input(input, choice.payload.as_deref()));
    }
    Ok(object([
        (
            "inputs",
            Value::Array(inputs.iter().map(|i| reflect_input(i, None)).collect()),
        ),
        (
            "entities",
            Value::Array(entities.iter().map(|e| text(e)).collect()),
        ),
        ("selected", Value::array(selected)),
    ]))
}

/// The JSON Schema type a declared field type has on the wire, or `None` for a type with no JSON shape.
fn json_type_of(field_type: &str) -> Option<&'static str> {
    match field_type {
        "string" | "date" | "timestamp" | "datetime" | "email" | "url" | "phone" | "uuid"
        | "image" | "bytes" | "callkey" | "CallKey" | "enum" | "trait" | "slot" | "pattern" | "secret" | "event"
        | "EventAddress" | "field" => Some("string"),
        "number" | "instant" => Some("number"),
        "boolean" => Some("boolean"),
        "array" => Some("array"),
        "object" | "relation" | "orbital" | "behavior" => Some("object"),
        _ => None,
    }
}

/// `[T]` is an array of `T`; anything else is its own element.
fn element_of(field_type: &str) -> (bool, &str) {
    if field_type.len() >= 2 && field_type.starts_with('[') && field_type.ends_with(']') {
        (true, &field_type[1..field_type.len() - 1])
    } else {
        (false, field_type)
    }
}

/// A declared payload as the JSON Schema of one object: what a tool-calling model fills.
pub fn payload_schema_to_json_schema(fields: &[PayloadField]) -> Value {
    let mut properties = ValueMap::default();
    let mut required: Vec<Value> = Vec::new();
    for field in fields {
        if field.properties.as_ref().is_some_and(Vec::is_empty) && field.field_type != "union" {
            continue;
        }
        properties.insert(field.name.clone(), payload_field_to_json_schema(field));
        if field.required == Some(true) {
            required.push(text(&field.name));
        }
    }
    let mut out = ValueMap::default();
    out.insert("type".to_string(), text("object"));
    if !fields.is_empty() {
        out.insert("additionalProperties".to_string(), Value::Bool(false));
    }
    out.insert("properties".to_string(), Value::object(properties));
    if !required.is_empty() {
        out.insert("required".to_string(), Value::array(required));
    }
    Value::object(out)
}

fn payload_field_to_json_schema(field: &PayloadField) -> Value {
    let (is_array, element) = element_of(&field.field_type);
    let mut shape = match &field.properties {
        Some(properties) => payload_schema_to_json_schema(properties),
        None => {
            let mut base = ValueMap::default();
            if let Some(json) = json_type_of(element) {
                base.insert("type".to_string(), text(json));
            }
            Value::object(base)
        }
    };
    if let (Some(values), Value::Object(map)) = (&field.values, &mut shape) {
        map.insert(
            "enum".to_string(),
            Value::Array(values.iter().map(|v| text(v)).collect()),
        );
    }
    if is_array {
        object([("type", text("array")), ("items", shape)])
    } else {
        shape
    }
}

/// One way a value misses its declared payload.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadIssue {
    pub path: String,
    pub reason: &'static str,
    pub expected: Option<String>,
}

/// Check a value against a declared payload: required fields, primitive and container types, closed value
/// sets, declared object shapes (closed) at any depth. An object field without declared properties stays open;
/// a type with no JSON shape (`node`, an entity name) is not checked beyond its container.
pub fn payload_issues(fields: &[PayloadField], value: &Value) -> Vec<PayloadIssue> {
    object_issues(fields, value, "")
}

fn issue(path: &str, reason: &'static str, expected: Option<String>) -> PayloadIssue {
    PayloadIssue {
        path: path.to_string(),
        reason,
        expected,
    }
}

fn join(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

fn object_issues(fields: &[PayloadField], value: &Value, at: &str) -> Vec<PayloadIssue> {
    let Value::Object(map) = value else {
        return vec![issue(
            if at.is_empty() { "." } else { at },
            "wrong-type",
            Some("object".to_string()),
        )];
    };
    let mut issues = Vec::new();
    for field in fields {
        match map.get(&field.name) {
            None | Some(Value::Null) => {
                if field.required == Some(true) {
                    issues.push(issue(&join(at, &field.name), "missing", None));
                }
            }
            Some(v) => issues.extend(field_issues(field, v, &join(at, &field.name))),
        }
    }
    let allowed = if fields.is_empty() {
        "(no keys)".to_string()
    } else {
        fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    for key in map.keys() {
        if !fields.iter().any(|f| &f.name == key) {
            issues.push(issue(&join(at, key), "unknown", Some(allowed.clone())));
        }
    }
    issues
}

fn field_issues(field: &PayloadField, value: &Value, at: &str) -> Vec<PayloadIssue> {
    let (is_array, element) = element_of(&field.field_type);
    if is_array || field.field_type == "array" {
        let Value::Array(items) = value else {
            return vec![issue(at, "wrong-type", Some(field.field_type.clone()))];
        };
        return items
            .iter()
            .enumerate()
            .flat_map(|(i, item)| element_issues(field, element, item, &format!("{at}[{i}]")))
            .collect();
    }
    element_issues(field, element, value, at)
}

fn element_issues(
    field: &PayloadField,
    element: &str,
    value: &Value,
    at: &str,
) -> Vec<PayloadIssue> {
    if let Some(properties) = &field.properties {
        if field.field_type != "union" {
            return object_issues(properties, value, at);
        }
        let fits = properties.iter().any(|variant| {
            element_issues(variant, element_of(&variant.field_type).1, value, at).is_empty()
        });
        return if fits {
            Vec::new()
        } else {
            vec![issue(
                at,
                "wrong-type",
                Some(
                    properties
                        .iter()
                        .map(|v| v.field_type.as_str())
                        .collect::<Vec<_>>()
                        .join(" | "),
                ),
            )]
        };
    }
    let json = if element == "object" {
        Some("object")
    } else {
        json_type_of(element)
    };
    let matches = match json {
        None => true,
        Some("object") => matches!(value, Value::Object(_)),
        Some("array") => matches!(value, Value::Array(_)),
        Some("string") => matches!(value, Value::String(_)),
        Some("number") => matches!(value, Value::Number(_)),
        Some("boolean") => matches!(value, Value::Bool(_)),
        Some(_) => false,
    };
    if !matches {
        return vec![issue(at, "wrong-type", Some(element.to_string()))];
    }
    if let Some(values) = &field.values {
        if !matches!(value, Value::String(s) if values.contains(s)) {
            return vec![issue(at, "not-one-of", Some(values.join(" | ")))];
        }
    }
    Vec::new()
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Lambda { .. } => "function",
    }
}

/// A path part split into its key and the trailing `[n]` indexes (`tags[1]` is `tags` and `[1]`).
fn split_path_part(part: &str) -> (&str, Vec<usize>) {
    for (start, _) in part
        .char_indices()
        .chain(std::iter::once((part.len(), ' ')))
    {
        let rest = &part[start..];
        let mut indexes = Vec::new();
        let mut remaining = rest;
        while let Some(open) = remaining.strip_prefix('[') {
            let Some(close) = open.find(']') else { break };
            let digits = &open[..close];
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                break;
            }
            let Ok(index) = digits.parse::<usize>() else {
                break;
            };
            indexes.push(index);
            remaining = &open[close + 1..];
        }
        if remaining.is_empty() {
            return (&part[..start], indexes);
        }
    }
    (part, Vec::new())
}

/// The value at an issue path (`a.b[0]`, `.` for the whole payload).
fn value_at<'v>(root: &'v Value, path: &str) -> Option<&'v Value> {
    if path == "." {
        return Some(root);
    }
    let mut current = root;
    for part in path.split('.') {
        let (key, indexes) = split_path_part(part);
        let Value::Object(map) = current else {
            return None;
        };
        current = map.get(key)?;
        for index in indexes {
            let Value::Array(items) = current else {
                return None;
            };
            current = items.get(index)?;
        }
    }
    Some(current)
}

/// The issues as data; each wrong-type issue gains the kind of value that was sent.
pub fn issues_value(issues: &[PayloadIssue], root: &Value) -> Vec<Value> {
    issues
        .iter()
        .map(|found| {
            let mut map = ValueMap::default();
            map.insert("path".to_string(), text(&found.path));
            map.insert("reason".to_string(), text(found.reason));
            if let Some(expected) = &found.expected {
                map.insert("expected".to_string(), text(expected));
            }
            if found.reason == "wrong-type" {
                if let Some(sent) = value_at(root, &found.path) {
                    map.insert("received".to_string(), text(json_kind(sent)));
                }
            }
            Value::object(map)
        })
        .collect()
}

/// The persisted entities of a program, once, in schema order: the inline entities of every orbital
/// that are not `[runtime]`. Twin of `readableEntitiesOf` (`@almadar/core`).
pub fn readable_entities_of(schema: &OrbitalSchema) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for orbital in &schema.orbitals {
        let primary = std::iter::once(&orbital.entity);
        let auxiliary = orbital.auxiliary_entities.iter().flatten();
        for entity in primary.chain(auxiliary) {
            let EntityRef::Inline(definition) = entity else {
                continue;
            };
            if definition.persistence == Some(Persistence::Runtime)
                || out.contains(&definition.name)
            {
                continue;
            }
            out.push(definition.name.clone());
        }
    }
    out
}

/// The declared input an address names, resolved from the calling trait's position: `EVENT` is that trait's
/// own, `Trait.EVENT` a trait of its orbital, `Orbital.Trait.EVENT` any orbital. `*.EVENT` names none.
pub fn find_input_by_address<'a>(
    inputs: &'a [ExternalInput],
    address: &str,
    caller_orbital: Option<&str>,
    caller_trait: &str,
) -> Option<&'a ExternalInput> {
    let (event, source) = split_event_address(address);
    let (orbital, trait_name) = match source {
        Some(ListenSource::Any) => return None,
        Some(ListenSource::Orbital {
            orbital,
            trait_name,
            ..
        }) => (Some(orbital), trait_name),
        Some(ListenSource::Trait { trait_name, .. }) => {
            (caller_orbital.map(str::to_string), trait_name)
        }
        None => (caller_orbital.map(str::to_string), caller_trait.to_string()),
    };
    let orbital = orbital?;
    inputs
        .iter()
        .find(|i| i.orbital == orbital && i.trait_name == trait_name && i.event == event)
}

/// What a `program/dispatch` comes to before the host is asked.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchPlan {
    /// The effect itself is malformed: it fails, and nothing is answered to a model.
    Failed(AgentPortFailure),
    /// The call is refused: this is the answer (`{ ok: false, error, issues? }`), and nothing is dispatched.
    Refused(Value),
    /// Dispatch `input` with `payload`, as the caller, through the host's ordinary input channel.
    Dispatch {
        input: ExternalInput,
        payload: Value,
    },
    /// Dispatch `payload` as the event request `options` describe (`DISPATCH_REQUEST_OPTIONS`, checked here): the host
    /// routes each option onto its dispatch seam, and `wire: true` answers the whole `EventResponse`.
    Request {
        payload: Value,
        options: ValueMap,
    },
    /// Deliver `body` and `headers` to the inbound handler `name` through the kernel's inbound route, credential
    /// and answer path (EXEC-INBOUND-1); the host answers the handler's `{ status, body }`.
    Inbound {
        name: String,
        body: String,
        headers: ValueMap,
    },
}

const INBOUND_SHAPE: &str =
    "program/dispatch: an inbound delivery is { inbound: name, body?: text, headers?: { name: text } }";

fn inbound_plan(options: &ValueMap) -> Result<DispatchPlan, String> {
    if options.keys().any(|key| !["inbound", "body", "headers"].contains(&key.as_str())) {
        return Err(INBOUND_SHAPE.to_string());
    }
    let Some(Value::String(name)) = options.get("inbound").filter(|v| v.as_str().is_some_and(|n| !n.is_empty())) else {
        return Err(INBOUND_SHAPE.to_string());
    };
    let body = match options.get("body") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(body)) => body.clone().into_inner(),
        Some(_) => return Err(INBOUND_SHAPE.to_string()),
    };
    let headers = match options.get("headers") {
        None | Some(Value::Null) => ValueMap::default(),
        Some(Value::Object(headers)) if headers.values().all(|v| v.as_str().is_some()) => headers.clone().into_inner(),
        Some(_) => return Err(INBOUND_SHAPE.to_string()),
    };
    Ok(DispatchPlan::Inbound { name: name.clone().into_inner(), body, headers })
}

enum Arguments {
    Payload(Value),
    Refused,
}

fn arguments_of(raw: Option<&Value>) -> Result<Arguments, String> {
    match raw {
        None | Some(Value::Null) => Ok(Arguments::Payload(Value::object(ValueMap::default()))),
        Some(Value::String(source)) => {
            let source = if source.is_empty() {
                "{}"
            } else {
                source.as_str()
            };
            Ok(match serde_json::from_str::<Value>(source) {
                Ok(parsed @ Value::Object(_)) => Arguments::Payload(parsed),
                _ => Arguments::Refused,
            })
        }
        Some(given @ Value::Object(_)) => Ok(Arguments::Payload(given.clone())),
        Some(_) => {
            Err("program/dispatch: the arguments must be an object or JSON text".to_string())
        }
    }
}

/// The event-request options of a `program/dispatch` (owner, 2026-10-21): what the HTTP dispatch routes carry.
pub const DISPATCH_REQUEST_OPTIONS: [&str; 14] = [
    "wire", "stream", "input", "orbital", "targetTrait", "entityId", "event", "traits", "mount", "clientId", "locale",
    "delivery", "dispatchLog", "viewer",
];

/// The request options that name a dispatch's target (the operator table's `program/dispatch` `targetOptions`); a
/// request naming none of them is addressed at the calling trait.
pub fn request_target_options() -> &'static [String] {
    crate::language::registry::dispatch_facts()
        .map(|facts| facts.target_options.as_slice())
        .expect("a kernel dispatches only after its operator table or a registered program supplied the dispatch facts")
}

/// The event a `program/dispatch` fires on its own calling trait: a bare event address whose options carry a request
/// option and name no target. The kernel plans it so and the validator counts it as fired.
pub fn calling_trait_event<'a, 'k>(address: &'a str, mut option_keys: impl Iterator<Item = &'k str>) -> Option<&'a str> {
    let bare = !address.is_empty() && !address.contains('.') && !address.starts_with('@');
    let mut requested = false;
    for key in option_keys.by_ref() {
        if request_target_options().iter().any(|target| target == key) {
            return None;
        }
        requested |= key != "schema";
    }
    (bare && requested).then_some(address)
}

fn list_of_records(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    let Value::Array(items) = value else { return false };
    items.iter().all(|item| {
        let Value::Object(fields) = item else { return false };
        required.iter().all(|key| fields.get(*key).and_then(Value::as_str).is_some())
            && fields.iter().all(|(key, v)| {
                required.contains(&key.as_str()) || (optional.contains(&key.as_str()) && v.as_str().is_some())
            })
    })
}

fn request_option_error(key: &str, value: &Value) -> Option<String> {
    let ok = match key {
        "wire" | "stream" | "input" => matches!(value, Value::Bool(_)),
        "orbital" | "event" => value.as_str().is_some_and(|s| !s.is_empty()),
        "targetTrait" | "entityId" | "clientId" | "locale" => value.as_str().is_some(),
        "traits" => value.is_null() || list_of_records(value, &["trait", "from"], &["event"]),
        "mount" => list_of_records(value, &["trait", "event"], &[]),
        "delivery" | "dispatchLog" | "viewer" => matches!(value, Value::Object(_) | Value::Null),
        _ => return Some(format!("program/dispatch: unsupported option \"{key}\"")),
    };
    (!ok).then(|| format!("program/dispatch: option \"{key}\" has the wrong shape"))
}

type DispatchOptions = (Option<Vec<PayloadField>>, ValueMap);

fn schema_option(raw: Option<&Value>) -> Result<DispatchOptions, String> {
    let Some(raw) = raw else { return Ok((None, ValueMap::default())) };
    let Value::Object(options) = raw else {
        return if raw.is_null() {
            Ok((None, ValueMap::default()))
        } else {
            Err("program/dispatch: the options must be an object".to_string())
        };
    };
    let mut request = ValueMap::default();
    for (key, value) in options.iter().filter(|(key, _)| key.as_str() != "schema") {
        if let Some(error) = request_option_error(key, value) {
            return Err(error);
        }
        request.insert(key.clone(), value.clone());
    }
    if request.get("stream") == Some(&Value::Bool(true)) && request.get("wire") != Some(&Value::Bool(true)) {
        return Err("program/dispatch: stream items are part of the wire answer: stream needs wire".to_string());
    }
    let Some(schema) = options.get("schema") else {
        return Ok((None, request));
    };
    serde_json::to_value(schema)
        .ok()
        .and_then(|json| serde_json::from_value::<Vec<PayloadField>>(json).ok())
        .map(|contract| (Some(contract), request))
        .ok_or_else(|| "program/dispatch: schema must be a list of payload fields".to_string())
}

/// Plan a `program/dispatch` from its positional arguments (`address`, `arguments`, `options`) over the
/// program's declared inputs, from the position of the calling trait.
pub fn plan_dispatch(
    args: &[Value],
    inputs: &[ExternalInput],
    caller_orbital: Option<&str>,
    caller_trait: &str,
) -> DispatchPlan {
    let failed = |message: String| DispatchPlan::Failed(AgentPortFailure::plain(message));
    if let [Value::Object(options)] = args {
        if options.contains_key("inbound") {
            return inbound_plan(options).unwrap_or_else(failed);
        }
    }
    if args.is_empty() || args.len() > 3 {
        return failed(
            "program/dispatch: expected an address, its arguments and optional options".to_string(),
        );
    }
    let Value::String(address) = &args[0] else {
        return failed("program/dispatch: the address must be text".to_string());
    };
    let (contract, request) = match schema_option(args.get(2)) {
        Ok(options) => options,
        Err(message) => return failed(message),
    };
    let parsed = match arguments_of(args.get(1)) {
        Ok(parsed) => parsed,
        Err(message) => return failed(message),
    };
    if !request.is_empty() {
        let mut request = request;
        let mut address = address.clone();
        if request_target_options().iter().all(|key| !request.contains_key(key)) {
            if address.contains('.') {
                return failed("program/dispatch: a request addressed at the calling trait names a bare event".to_string());
            }
            let Some(orbital) = caller_orbital else {
                return failed("program/dispatch: a request addressed at the calling trait needs the trait's orbital".to_string());
            };
            request.insert("orbital".to_string(), Value::string(orbital.to_string()));
            request.insert("targetTrait".to_string(), Value::string(caller_trait.to_string()));
            request.insert("event".to_string(), Value::String(address.clone()));
            address = format!("{orbital}.{caller_trait}.{address}").into();
        } else if !(request.contains_key("orbital") && request.contains_key("event")) {
            return failed("program/dispatch: an event request names its orbital and event".to_string());
        }
        let text = |key: &str| request.get(key).and_then(Value::as_str).unwrap_or_default();
        let named = format!("{}.{}.{}", text("orbital"), text("targetTrait"), text("event"));
        if address != named {
            return failed(format!("program/dispatch: the address '{address}' is not the request's '{named}'"));
        }
        let Arguments::Payload(payload) = parsed else {
            return DispatchPlan::Refused(refusal(REFUSED_ARGUMENTS, None));
        };
        if let Some(issues) = contract.map(|c| payload_issues(&c, &payload)).filter(|i| !i.is_empty()) {
            return DispatchPlan::Refused(refusal(MISMATCHED_ARGUMENTS, Some(issues_value(&issues, &payload))));
        }
        return DispatchPlan::Request { payload, options: request };
    }
    let Some(input) = find_input_by_address(inputs, address, caller_orbital, caller_trait) else {
        return DispatchPlan::Refused(refusal(
            &format!("'{address}' is not a declared external input"),
            None,
        ));
    };
    let Arguments::Payload(payload) = parsed else {
        return DispatchPlan::Refused(refusal(REFUSED_ARGUMENTS, None));
    };
    if let Some(contract) = contract {
        let issues = payload_issues(&contract, &payload);
        if !issues.is_empty() {
            return DispatchPlan::Refused(refusal(
                MISMATCHED_ARGUMENTS,
                Some(issues_value(&issues, &payload)),
            ));
        }
    }
    DispatchPlan::Dispatch {
        input: input.clone(),
        payload,
    }
}

/// The entity a `persist/read-named` names, when it is one of `readable`; otherwise the typed failure.
pub fn read_named_target(args: &[Value], readable: &[String]) -> Result<String, AgentPortFailure> {
    if args.len() != 1 {
        return Err(AgentPortFailure::plain(
            "persist/read-named: expected the entity name",
        ));
    }
    let Value::String(name) = &args[0] else {
        return Err(AgentPortFailure::plain(
            "persist/read-named: the entity name must be text",
        ));
    };
    if readable.contains(name) {
        return Ok(name.clone().into_inner());
    }
    let message = format!("persist/read-named: '{name}' is not a persisted entity of this program");
    Err(AgentPortFailure {
        error: message.clone(),
        errors: vec![object([
            ("code", text(UNKNOWN_ENTITY_CODE)),
            ("message", Value::string(message)),
        ])],
    })
}

/// `persist/read-named`'s result: the entity, how many rows, and the rows.
pub fn read_named_result(entity: &str, rows: Vec<Value>) -> Value {
    object([
        ("entity", text(entity)),
        ("count", Value::Number(rows.len() as f64)),
        ("rows", Value::array(rows)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_inputs::external_inputs_of;
    use serde_json::Value as Json;

    const VECTORS: &str = include_str!("../../../runtime/tests/fixtures/agent-ports/vectors.json");

    fn fixture() -> Json {
        serde_json::from_str(VECTORS).expect("the shared vectors parse")
    }

    fn norm(json: Json) -> Json {
        match json {
            Json::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Json::from(f as i64),
                _ => Json::Number(n),
            },
            Json::Array(items) => Json::Array(items.into_iter().map(norm).collect()),
            Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, norm(v))).collect()),
            other => other,
        }
    }

    fn as_json(value: &Value) -> Json {
        norm(serde_json::to_value(value).expect("a value serializes"))
    }

    fn schema_of(fixture: &Json) -> OrbitalSchema {
        serde_json::from_value(fixture["schema"].clone()).expect("the vectors' schema deserializes")
    }

    fn args_of(case: &Json) -> Vec<Value> {
        case["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|a| serde_json::from_value::<Value>(a.clone()).expect("an argument"))
            .collect()
    }

    fn cases_of<'a>(fixture: &'a Json, op: &'a str) -> impl Iterator<Item = &'a Json> {
        fixture["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .filter(move |case| case["op"] == op)
    }

    #[test]
    fn inputs_reflect_the_same_way_for_every_shared_vector() {
        let fixture = fixture();
        let schema = schema_of(&fixture);
        let inputs = external_inputs_of(&schema);
        let entities = readable_entities_of(&schema);
        let mut seen = 0;
        for case in cases_of(&fixture, "program/inputs") {
            seen += 1;
            let name = case["name"].as_str().unwrap_or_default();
            let expect = &case["expect"];
            match plan_inputs(&args_of(case), &inputs, &entities, Some("Desk"), "Chat") {
                Ok(result) => {
                    assert_eq!(expect["ok"], true, "{name}");
                    assert_eq!(as_json(&result), norm(expect["result"].clone()), "{name}");
                }
                Err(failure) => {
                    assert_eq!(expect["ok"], false, "{name}");
                    assert_eq!(expect["error"], failure.error.as_str(), "{name}");
                }
            }
        }
        assert!(
            seen > 8,
            "the corpus carries the reflection and its controls"
        );
    }

    #[test]
    fn a_dispatch_plans_the_same_way_for_every_shared_vector() {
        let fixture = fixture();
        let inputs = external_inputs_of(&schema_of(&fixture));
        let mut seen = 0;
        for case in cases_of(&fixture, "program/dispatch") {
            seen += 1;
            let plan = plan_dispatch(&args_of(case), &inputs, Some("Desk"), "Chat");
            let name = case["name"].as_str().unwrap_or_default();
            let expect = &case["expect"];
            match (&case["dispatched"], plan) {
                (Json::Null, DispatchPlan::Failed(failure)) => {
                    assert_eq!(expect["ok"], false, "{name}");
                    assert_eq!(expect["error"], failure.error.as_str(), "{name}");
                }
                (Json::Null, DispatchPlan::Refused(answer)) => {
                    assert_eq!(as_json(&answer), norm(expect["result"].clone()), "{name}");
                }
                (dispatched, DispatchPlan::Dispatch { input, payload }) => {
                    let address = format!("{}.{}.{}", input.orbital, input.trait_name, input.event);
                    assert_eq!(dispatched["address"], address.as_str(), "{name}");
                    assert_eq!(
                        as_json(&payload),
                        norm(dispatched["payload"].clone()),
                        "{name}"
                    );
                }
                (dispatched, plan) => {
                    panic!("{name}: dispatched {dispatched} but planned {plan:?}")
                }
            }
        }
        assert!(seen > 10, "the corpus carries the dispatch cases");
    }

    #[test]
    fn a_read_names_only_a_persisted_entity_of_the_program() {
        let fixture = fixture();
        let readable = readable_entities_of(&schema_of(&fixture));
        assert_eq!(readable, vec!["Note".to_string(), "Tag".to_string()]);
        for case in cases_of(&fixture, "persist/read-named") {
            let name = case["name"].as_str().unwrap_or_default();
            let expect = &case["expect"];
            match read_named_target(&args_of(case), &readable) {
                Ok(entity) => {
                    assert_eq!(expect["ok"], true, "{name}");
                    assert_eq!(expect["result"]["entity"], entity.as_str(), "{name}");
                }
                Err(failure) => {
                    assert_eq!(expect["ok"], false, "{name}");
                    assert_eq!(expect["error"], failure.error.as_str(), "{name}");
                    let errors = Json::Array(failure.errors.iter().map(as_json).collect());
                    let expected = expect
                        .get("errors")
                        .cloned()
                        .unwrap_or(Json::Array(Vec::new()));
                    assert_eq!(errors, expected, "{name}");
                }
            }
        }
    }

    #[test]
    fn a_path_part_splits_into_its_key_and_trailing_indexes() {
        assert_eq!(split_path_part("tags[1]"), ("tags", vec![1]));
        assert_eq!(split_path_part("grid[2][0]"), ("grid", vec![2, 0]));
        assert_eq!(split_path_part("title"), ("title", vec![]));
        assert_eq!(split_path_part("odd[x]"), ("odd[x]", vec![]));
        assert_eq!(split_path_part("[3]"), ("", vec![3]));
    }

    #[test]
    fn a_wrong_type_issue_names_what_was_sent_and_a_missing_one_does_not() {
        let fields = vec![PayloadField {
            nullable: None,
            additional_properties: None,
            name: "n".to_string(),
            field_type: "number".to_string(),
            values: None,
            required: Some(true),
            description: None,
            properties: None,
            type_when: None,
            entity: None,
            projected_from: None,
            items: None,
        }];
        let sent = serde_json::from_value::<Value>(serde_json::json!({ "n": "x" })).unwrap();
        let issues = issues_value(&payload_issues(&fields, &sent), &sent);
        assert_eq!(
            as_json(&Value::array(issues)),
            serde_json::json!([{ "path": "n", "reason": "wrong-type", "expected": "number", "received": "string" }])
        );
        let empty = Value::object(ValueMap::default());
        let issues = issues_value(&payload_issues(&fields, &empty), &empty);
        assert_eq!(
            as_json(&Value::array(issues)),
            serde_json::json!([{ "path": "n", "reason": "missing" }])
        );
    }

    #[test]
    fn only_the_three_agent_port_operators_are_named() {
        assert!(AGENT_PORT_OPERATORS
            .iter()
            .all(|op| is_agent_port_operator(op)));
        assert!(!is_agent_port_operator("program/eval"));
        assert!(!is_agent_port_operator("persist"));
    }

    fn value(json: Json) -> Value {
        serde_json::from_value(json).expect("a value")
    }

    #[test]
    fn program_inputs_reads_its_state_stats_and_trace_options() {
        let reads = inputs_reads(&[value(serde_json::json!({ "state": "Desk", "stats": true, "trace": 3 }))]).unwrap();
        assert_eq!(reads, InputsReads { state: Some("Desk".into()), stats: true, trace: Some(3) });
        assert_eq!(inputs_reads(&[]).unwrap(), InputsReads::default(), "control: no options read nothing");
        let f = fixture();
        let inputs = external_inputs_of(&schema_of(&f));
        assert!(plan_inputs(&[value(serde_json::json!({ "stats": true }))], &inputs, &[], None, "Chat").is_ok());
        for (bad, why) in [
            (serde_json::json!({ "stats": "yes" }), "stats must be true or false"),
            (serde_json::json!({ "trace": -1 }), "trace must be a whole number"),
            (serde_json::json!({ "trace": 1.5 }), "trace must be a whole number"),
            (serde_json::json!({ "state": "" }), "state must name an orbital"),
            (serde_json::json!({ "states": "Desk" }), "unsupported option \"states\""),
        ] {
            let failure = plan_inputs(&[value(bad.clone())], &inputs, &[], None, "Chat").unwrap_err();
            assert!(failure.error.contains(why), "{bad}: {}", failure.error);
        }
    }

    #[test]
    fn program_dispatch_plans_an_inbound_delivery_from_its_options() {
        let f = fixture();
        let inputs = external_inputs_of(&schema_of(&f));
        let plan = plan_dispatch(
            &[value(serde_json::json!({ "inbound": "github", "body": "{}", "headers": { "x-sig": "s" } }))],
            &inputs,
            None,
            "Chat",
        );
        let DispatchPlan::Inbound { name, body, headers } = plan else { panic!("{plan:?}") };
        assert_eq!((name.as_str(), body.as_str()), ("github", "{}"));
        assert_eq!(headers.get("x-sig").and_then(Value::as_str), Some("s"));
        let bare = plan_dispatch(&[value(serde_json::json!({ "inbound": "github" }))], &inputs, None, "Chat");
        assert!(matches!(&bare, DispatchPlan::Inbound { body, headers, .. } if body.is_empty() && headers.is_empty()), "{bare:?}");
        let control = plan_dispatch(&[value(serde_json::json!({ "title": "x" }))], &inputs, None, "Chat");
        assert!(matches!(control, DispatchPlan::Failed(_)), "control: an object without inbound is no address");
        for bad in [
            serde_json::json!({ "inbound": "" }),
            serde_json::json!({ "inbound": "github", "body": 3 }),
            serde_json::json!({ "inbound": "github", "headers": { "x": 1 } }),
            serde_json::json!({ "inbound": "github", "verb": "POST" }),
        ] {
            let plan = plan_dispatch(&[value(bad.clone())], &inputs, None, "Chat");
            assert!(matches!(&plan, DispatchPlan::Failed(f) if f.error == INBOUND_SHAPE), "{bad}: {plan:?}");
        }
    }

    fn request(extra: Json) -> Json {
        let mut options = serde_json::json!({ "orbital": "Desk", "targetTrait": "Chat", "event": "ADD_NOTE" });
        for (k, v) in extra.as_object().unwrap() {
            options[k] = v.clone();
        }
        options
    }

    fn plan_request(options: Json) -> DispatchPlan {
        let f = fixture();
        let inputs = external_inputs_of(&schema_of(&f));
        plan_dispatch(&[text("Desk.Chat.ADD_NOTE"), value(serde_json::json!({ "title": "x" })), value(options)], &inputs, None, "Chat")
    }

    #[test]
    fn program_dispatch_carries_each_request_option_to_the_host() {
        for (key, given) in [
            ("wire", serde_json::json!(true)),
            ("stream", serde_json::json!(false)),
            ("input", serde_json::json!(false)),
            ("entityId", serde_json::json!("n1")),
            ("traits", serde_json::json!([{ "trait": "Chat", "from": "idle", "event": "ADD_NOTE" }])),
            ("mount", serde_json::json!([{ "trait": "Chat", "event": "INIT" }])),
            ("clientId", serde_json::json!("tab-1")),
            ("locale", serde_json::json!("ar")),
            ("delivery", serde_json::json!({ "event": "ADD_NOTE" })),
            ("dispatchLog", serde_json::json!({ "prevEvents": [], "prevStates": [] })),
            ("viewer", serde_json::json!({ "id": "u1" })),
        ] {
            let mut extra = serde_json::Map::new();
            extra.insert(key.to_string(), given.clone());
            let plan = plan_request(request(Json::Object(extra)));
            let DispatchPlan::Request { payload, options } = &plan else { panic!("{key}: {plan:?}") };
            assert_eq!(options.get(key).map(as_json), Some(norm(given)), "{key}");
            assert_eq!(as_json(payload), serde_json::json!({ "title": "x" }), "{key}");
            for named in ["orbital", "targetTrait", "event"] {
                assert!(options.contains_key(named), "{key}: {named} travels");
            }
        }
        let control = plan_dispatch(&[text("Desk.Chat.ADD_NOTE"), value(serde_json::json!({ "title": "x" }))], &external_inputs_of(&schema_of(&fixture())), None, "Chat");
        assert!(matches!(control, DispatchPlan::Dispatch { .. }), "control: no request option is the declared-input dispatch: {control:?}");
    }

    #[test]
    fn program_dispatch_refuses_a_malformed_or_unknown_request_option() {
        for (bad, why) in [
            (request(serde_json::json!({ "verb": "POST" })), "unsupported option \"verb\""),
            (request(serde_json::json!({ "stream": true })), "stream needs wire"),
            (request(serde_json::json!({ "wire": "yes" })), "option \"wire\" has the wrong shape"),
            (request(serde_json::json!({ "input": 1 })), "option \"input\" has the wrong shape"),
            (request(serde_json::json!({ "entityId": 3 })), "option \"entityId\" has the wrong shape"),
            (request(serde_json::json!({ "traits": [{ "trait": "Chat" }] })), "option \"traits\" has the wrong shape"),
            (request(serde_json::json!({ "mount": [{ "trait": "Chat", "event": "INIT", "extra": "x" }] })), "option \"mount\" has the wrong shape"),
            (request(serde_json::json!({ "clientId": false })), "option \"clientId\" has the wrong shape"),
            (request(serde_json::json!({ "locale": [] })), "option \"locale\" has the wrong shape"),
            (request(serde_json::json!({ "delivery": "x" })), "option \"delivery\" has the wrong shape"),
            (request(serde_json::json!({ "dispatchLog": 1 })), "option \"dispatchLog\" has the wrong shape"),
            (request(serde_json::json!({ "viewer": "u1" })), "option \"viewer\" has the wrong shape"),
            (serde_json::json!({ "wire": true, "event": "ADD_NOTE" }), "names its orbital and event"),
            (request(serde_json::json!({ "orbital": "" })), "option \"orbital\" has the wrong shape"),
            (request(serde_json::json!({ "targetTrait": "Other" })), "is not the request's 'Desk.Other.ADD_NOTE'"),
        ] {
            let plan = plan_request(bad.clone());
            assert!(matches!(&plan, DispatchPlan::Failed(f) if f.error.contains(why)), "{bad}: {plan:?}");
        }
        assert!(matches!(plan_request(request(serde_json::json!({ "traits": null }))), DispatchPlan::Request { .. }), "control: null traits is discovery");
    }

    #[test]
    fn the_dispatch_target_options_and_the_bus_excluded_events_are_the_operator_table_s() {
        let data = crate::language_data::operator_data().expect("the installed operator table");
        let table: Json = serde_json::from_str(&data.operators).unwrap();
        let listed = |op: &str, field: &str| -> Vec<String> {
            table["operators"][op][field].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
        };
        assert_eq!(request_target_options(), listed("program/dispatch", "targetOptions").as_slice());
        assert_eq!(crate::events::bus_excluded_events(), listed("emit", "busExcluded").as_slice());
        assert!(crate::events::is_bus_excluded_event("$MOUNT"));
        assert!(!crate::events::is_bus_excluded_event("SAVE"), "control: an ordinary event is delivered");
    }

    #[test]
    fn a_program_s_dispatch_facts_install_once_and_conflicting_ones_are_refused() {
        use crate::language::registry::{install_dispatch_facts, DispatchFacts};
        let facts = |excluded: &[&str]| DispatchFacts {
            bus_excluded: excluded.iter().map(|e| e.to_string()).collect(),
            target_options: vec!["orbital".into(), "targetTrait".into(), "event".into()],
        };
        assert_eq!(install_dispatch_facts(facts(&["INIT", "LOAD", "$MOUNT"])), Ok(()));
        assert_eq!(install_dispatch_facts(facts(&["INIT", "LOAD", "$MOUNT"])), Ok(()), "a second program with the same facts");
        let conflict = install_dispatch_facts(facts(&["INIT"]));
        assert!(conflict.is_err_and(|e| e.contains("already runs a program")), "control: different facts are refused");
    }

    #[test]
    fn the_emit_slots_are_every_emit_option_field_the_operator_table_declares() {
        let slots = crate::language::registry::emit_slots();
        for slot in ["success", "failure", "cancelled", "onMessage", "onChange"] {
            assert!(slots.contains(slot), "{slot}: {slots:?}");
        }
        assert!(!slots.contains("onSave"), "control: a name no emit options declare is no slot: {slots:?}");
    }

    #[test]
    fn an_event_request_naming_no_target_is_addressed_at_the_calling_trait() {
        let inputs = external_inputs_of(&schema_of(&fixture()));
        let viewer = serde_json::json!({ "viewer": { "id": "u1" } });
        let plan = plan_dispatch(&[text("ADD_NOTE"), value(serde_json::json!({ "title": "x" })), value(viewer.clone())], &inputs, Some("Desk"), "Chat");
        let DispatchPlan::Request { payload, options } = &plan else { panic!("{plan:?}") };
        assert_eq!(as_json(payload), serde_json::json!({ "title": "x" }));
        for (key, expected) in [("orbital", "Desk"), ("targetTrait", "Chat"), ("event", "ADD_NOTE")] {
            assert_eq!(options.get(key).and_then(Value::as_str), Some(expected), "{key}");
        }
        assert_eq!(options.get("viewer").map(as_json), Some(serde_json::json!({ "id": "u1" })));
        let undeclared = plan_dispatch(&[text("NOT_AN_INPUT"), value(serde_json::json!({})), value(viewer.clone())], &inputs, Some("Desk"), "Chat");
        assert!(matches!(&undeclared, DispatchPlan::Request { options, .. } if options.get("event").and_then(Value::as_str) == Some("NOT_AN_INPUT")), "the calling trait's own event needs no input declaration: {undeclared:?}");
        for (address, orbital, why) in [
            ("Chat.ADD_NOTE", Some("Desk"), "a request addressed at the calling trait names a bare event"),
            ("ADD_NOTE", None, "a request addressed at the calling trait needs the trait's orbital"),
        ] {
            let plan = plan_dispatch(&[text(address), value(serde_json::json!({})), value(viewer.clone())], &inputs, orbital, "Chat");
            assert!(matches!(&plan, DispatchPlan::Failed(f) if f.error.contains(why)), "{address}: {plan:?}");
        }
        let partial = plan_dispatch(&[text("ADD_NOTE"), value(serde_json::json!({})), value(serde_json::json!({ "event": "ADD_NOTE", "wire": true }))], &inputs, Some("Desk"), "Chat");
        assert!(matches!(&partial, DispatchPlan::Failed(f) if f.error.contains("names its orbital and event")), "control: a partly named target is still refused: {partial:?}");
        let control = plan_dispatch(&[text("ADD_NOTE"), value(serde_json::json!({ "title": "x" }))], &inputs, Some("Desk"), "Chat");
        assert!(matches!(control, DispatchPlan::Dispatch { .. }), "control: no request option is the declared-input dispatch: {control:?}");
    }
}
