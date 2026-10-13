//! A `call-service` request checked against its service contract before it is requested.
//!
//! One function serves both callers: the runtime (JSON values, every key's kind
//! is known) and the compiler's static check (literal kinds; bindings and other
//! computed values are [`ArgKind::Dynamic`] and never type-checked). The
//! contracts are the [`ServiceRegistryData`] the language reader already loads.
//!
//! JS twins: `packages/almadar-integrations/src/core/validation.ts`
//! (`validateParams`) and the compiler's `check_params` (I-13).

use crate::language::registry::{EmbeddedServiceParam, ServiceRegistryData};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// Envelope keys owned by the call-service form itself, never action params.
pub const RESERVED_ENVELOPE_KEYS: [&str; 4] = ["emit", "onSuccess", "onError", "timeout"];

/// What is known about one supplied argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// The key is present but holds `undefined`: the same as not sending it.
    Undefined,
    Null,
    String,
    Number,
    Boolean,
    Object,
    Array,
    /// A value not known until run time (a binding, a computed form): present, never type-checked.
    Dynamic,
}

impl ArgKind {
    /// The kind of a JSON value.
    pub fn of_json(value: &Value) -> Self {
        match value {
            Value::Null => ArgKind::Null,
            Value::Bool(_) => ArgKind::Boolean,
            Value::Number(_) => ArgKind::Number,
            Value::String(_) => ArgKind::String,
            Value::Array(_) => ArgKind::Array,
            Value::Object(_) => ArgKind::Object,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ArgKind::Undefined => "undefined",
            ArgKind::Null => "null",
            ArgKind::String => "string",
            ArgKind::Number => "number",
            ArgKind::Boolean => "boolean",
            ArgKind::Object => "object",
            ArgKind::Array => "array",
            ArgKind::Dynamic => "dynamic",
        }
    }
}

/// The supplied arguments of one call, keyed by param name.
pub type Args = BTreeMap<String, ArgKind>;

/// The arguments of a JSON object.
pub fn args_of_json(object: &serde_json::Map<String, Value>) -> Args {
    object
        .iter()
        .map(|(k, v)| (k.clone(), ArgKind::of_json(v)))
        .collect()
}

/// The kind a declared TS-flavored type string requires; `None` for types that
/// are deliberately unchecked (named TS types, unions of non-literals).
pub fn checkable_type(declared: &str) -> Option<ArgKind> {
    let t = declared.trim();
    match t {
        "string" => Some(ArgKind::String),
        "number" => Some(ArgKind::Number),
        "boolean" => Some(ArgKind::Boolean),
        "object" => Some(ArgKind::Object),
        "array" => Some(ArgKind::Array),
        _ if t.ends_with("[]") => Some(ArgKind::Array),
        _ if t.starts_with('\'') => Some(ArgKind::String),
        _ => None,
    }
}

/// One way a call disagrees with its contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamIssue {
    UnknownService {
        service: String,
    },
    UnknownAction {
        service: String,
        action: String,
    },
    UnknownParam {
        param: String,
        declared: Vec<String>,
    },
    MissingRequired {
        param: String,
    },
    TypeMismatch {
        param: String,
        expected: ArgKind,
        actual: ArgKind,
    },
}

impl ParamIssue {
    /// The param the issue names (`integration` / `action` for lookup failures).
    pub fn param(&self) -> &str {
        match self {
            ParamIssue::UnknownService { .. } => "integration",
            ParamIssue::UnknownAction { .. } => "action",
            ParamIssue::UnknownParam { param, .. }
            | ParamIssue::MissingRequired { param }
            | ParamIssue::TypeMismatch { param, .. } => param,
        }
    }
}

impl fmt::Display for ParamIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamIssue::UnknownService { service } => write!(f, "Unknown integration: {service}"),
            ParamIssue::UnknownAction { action, .. } => write!(f, "Unknown action: {action}"),
            ParamIssue::UnknownParam { param, declared } => {
                write!(
                    f,
                    "Unknown parameter: {param} (declared: {})",
                    declared.join(", ")
                )
            }
            ParamIssue::MissingRequired { param } => {
                write!(f, "Missing required parameter: {param}")
            }
            ParamIssue::TypeMismatch {
                expected, actual, ..
            } => {
                write!(f, "Expected {}, got {}", expected.as_str(), actual.as_str())
            }
        }
    }
}

/// The declared params of `service.action`, or why there are none to check.
fn declared<'a>(
    registry: &'a ServiceRegistryData,
    service: &str,
    action: &str,
) -> Result<Option<&'a [EmbeddedServiceParam]>, ParamIssue> {
    let Some(actions) = registry.actions.get(service) else {
        return Err(ParamIssue::UnknownService {
            service: service.to_string(),
        });
    };
    // A declared-only service (host-injected handler) names no actions: its calls are unchecked.
    if actions.is_empty() {
        return Ok(None);
    }
    if !actions.contains(action) {
        return Err(ParamIssue::UnknownAction {
            service: service.to_string(),
            action: action.to_string(),
        });
    }
    Ok(registry
        .params
        .get(service)
        .and_then(|a| a.get(action))
        .map(Vec::as_slice))
}

/// Check one call's arguments against the contract. Unknown params come first
/// (sorted by name), then per declared param, in declared order, a missing
/// required one and a type mismatch. Reserved envelope keys are skipped.
pub fn validate_params(
    registry: &ServiceRegistryData,
    service: &str,
    action: &str,
    args: &Args,
) -> Vec<ParamIssue> {
    let params = match declared(registry, service, action) {
        Ok(Some(params)) => params,
        Ok(None) => return Vec::new(),
        Err(issue) => return vec![issue],
    };
    let mut issues = Vec::new();
    let mut names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    names.sort();
    for key in args.keys() {
        if RESERVED_ENVELOPE_KEYS.contains(&key.as_str()) {
            continue;
        }
        if !params.iter().any(|p| &p.name == key) {
            issues.push(ParamIssue::UnknownParam {
                param: key.clone(),
                declared: names.clone(),
            });
        }
    }
    for p in params {
        let kind = args
            .get(&p.name)
            .copied()
            .filter(|k| *k != ArgKind::Undefined);
        let Some(actual) = kind else {
            if p.required {
                issues.push(ParamIssue::MissingRequired {
                    param: p.name.clone(),
                });
            }
            continue;
        };
        if actual == ArgKind::Dynamic {
            continue;
        }
        if let Some(expected) = checkable_type(&p.param_type) {
            if expected != actual {
                issues.push(ParamIssue::TypeMismatch {
                    param: p.name.clone(),
                    expected,
                    actual,
                });
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::registry::parse_service_registry_data;
    use serde_json::json;

    fn registry() -> ServiceRegistryData {
        parse_service_registry_data(
            &json!({"services": {
                "stripe": {"actions": [
                    {"name": "createPaymentIntent", "params": [
                        {"name": "amount", "type": "number", "required": true},
                        {"name": "currency", "type": "string", "required": true},
                        {"name": "metadata", "type": "object", "required": false}]}]},
                "email": {"actions": [
                    {"name": "send", "params": [
                        {"name": "to", "type": "string", "required": true},
                        {"name": "subject", "type": "string", "required": true},
                        {"name": "body", "type": "string", "required": true},
                        {"name": "from", "type": "string", "required": false},
                        {"name": "htmlBody", "type": "string", "required": false},
                        {"name": "replyTo", "type": "string", "required": false},
                        {"name": "templateId", "type": "string", "required": false}]}]},
                "llm": {"actions": [
                    {"name": "generate", "params": [
                        {"name": "systemPrompt", "type": "string", "required": false},
                        {"name": "userPrompt", "type": "string", "required": true},
                        {"name": "temperature", "type": "number", "required": false}]},
                    {"name": "classify", "params": [
                        {"name": "categories", "type": "string[]", "required": true},
                        {"name": "mode", "type": "'fast' | 'deep'", "required": false},
                        {"name": "flag", "type": "boolean", "required": false},
                        {"name": "opaque", "type": "SomeNamedType", "required": false}]}]},
                "hostonly": {"actions": []}
            }})
            .to_string(),
        )
        .unwrap()
    }

    fn call(service: &str, action: &str, args: serde_json::Value) -> Vec<ParamIssue> {
        let args = args_of_json(args.as_object().unwrap());
        validate_params(&registry(), service, action, &args)
    }

    fn params_of(issues: &[ParamIssue]) -> Vec<&str> {
        issues.iter().map(ParamIssue::param).collect()
    }

    // validation.test.ts
    #[test]
    fn missing_required_params_are_reported_in_declared_order() {
        let issues = call("stripe", "createPaymentIntent", json!({}));
        assert_eq!(params_of(&issues), vec!["amount", "currency"]);
        assert!(issues
            .iter()
            .all(|i| matches!(i, ParamIssue::MissingRequired { .. })));
    }

    #[test]
    fn wrong_param_type_is_one_error_naming_the_param() {
        let issues = call(
            "stripe",
            "createPaymentIntent",
            json!({"amount": "2000", "currency": "usd"}),
        );
        assert_eq!(params_of(&issues), vec!["amount"]);
        assert!(issues[0].to_string().contains("Expected number"));
        assert_eq!(issues[0].to_string(), "Expected number, got string");
    }

    #[test]
    fn valid_params_pass() {
        assert!(call(
            "stripe",
            "createPaymentIntent",
            json!({"amount": 2000, "currency": "usd"})
        )
        .is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected_the_recipient_vs_to_class() {
        let issues = call(
            "email",
            "send",
            json!({"recipient": "a@b.c", "subject": "s", "body": "b", "to": "a@b.c"}),
        );
        assert!(issues
            .iter()
            .any(|i| i.param() == "recipient" && i.to_string().contains("Unknown parameter")));
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            ParamIssue::UnknownParam { declared, .. } => {
                assert_eq!(
                    declared,
                    &vec![
                        "body",
                        "from",
                        "htmlBody",
                        "replyTo",
                        "subject",
                        "templateId",
                        "to"
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reserved_envelope_keys_are_never_unknown() {
        let issues = call(
            "email",
            "send",
            json!({"to": "a", "subject": "s", "body": "b", "emit": {"success": "SENT"}, "timeout": 5, "onSuccess": "X", "onError": "Y"}),
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn widened_email_contract_is_accepted() {
        let issues = call(
            "email",
            "send",
            json!({"to": "a", "subject": "s", "body": "b", "from": "n@x.y", "htmlBody": "<b>", "replyTo": "o@x.y", "templateId": "d-1"}),
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn unknown_service_and_action() {
        let s = call("unknown", "action", json!({}));
        assert_eq!(
            s,
            vec![ParamIssue::UnknownService {
                service: "unknown".into()
            }]
        );
        assert!(s[0].to_string().contains("Unknown integration"));
        let a = call("stripe", "unknownAction", json!({}));
        assert!(a[0].to_string().contains("Unknown action"));
    }

    #[test]
    fn declared_only_service_is_unchecked() {
        assert!(call("hostonly", "anything", json!({"x": 1})).is_empty());
    }

    // validation.test.ts: an undefined param is an absent param
    #[test]
    fn undefined_optional_param_is_not_a_type_error() {
        let mut args = Args::new();
        args.insert("userPrompt".into(), ArgKind::String);
        args.insert("systemPrompt".into(), ArgKind::Undefined);
        args.insert("temperature".into(), ArgKind::Undefined);
        assert!(validate_params(&registry(), "llm", "generate", &args).is_empty());
    }

    #[test]
    fn undefined_required_param_is_missing() {
        let mut args = Args::new();
        args.insert("userPrompt".into(), ArgKind::Undefined);
        let issues = validate_params(&registry(), "llm", "generate", &args);
        assert_eq!(
            issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            vec!["Missing required parameter: userPrompt"]
        );
    }

    #[test]
    fn control_a_wrongly_typed_value_is_still_rejected() {
        let issues = call(
            "llm",
            "generate",
            json!({"userPrompt": "hi", "temperature": "hot"}),
        );
        assert_eq!(params_of(&issues), vec!["temperature"]);
    }

    // type rules shared with the compiler's static check (I-13)
    #[test]
    fn object_param_rejects_array_and_null() {
        assert_eq!(
            call(
                "stripe",
                "createPaymentIntent",
                json!({"amount": 1, "currency": "u", "metadata": []})
            )
            .len(),
            1
        );
        assert_eq!(
            call(
                "stripe",
                "createPaymentIntent",
                json!({"amount": 1, "currency": "u", "metadata": null})
            )
            .len(),
            1
        );
        assert!(call(
            "stripe",
            "createPaymentIntent",
            json!({"amount": 1, "currency": "u", "metadata": {}})
        )
        .is_empty());
    }

    #[test]
    fn array_suffix_literal_union_and_boolean_types_are_checked() {
        assert!(call(
            "llm",
            "classify",
            json!({"categories": ["a"], "mode": "fast", "flag": true})
        )
        .is_empty());
        let issues = call(
            "llm",
            "classify",
            json!({"categories": "a", "mode": 3, "flag": "yes"}),
        );
        assert_eq!(params_of(&issues), vec!["categories", "mode", "flag"]);
    }

    #[test]
    fn named_ts_types_are_unchecked() {
        assert!(call("llm", "classify", json!({"categories": [], "opaque": 5})).is_empty());
        assert_eq!(checkable_type("SomeNamedType"), None);
        assert_eq!(checkable_type(" string "), Some(ArgKind::String));
    }

    // compiler static check: dynamic values count as present and are never type-checked
    #[test]
    fn dynamic_arguments_satisfy_required_and_skip_type_checks() {
        let mut args = Args::new();
        args.insert("amount".into(), ArgKind::Dynamic);
        args.insert("currency".into(), ArgKind::Dynamic);
        assert!(validate_params(&registry(), "stripe", "createPaymentIntent", &args).is_empty());
    }

    #[test]
    fn no_arguments_report_every_required_param() {
        let issues = call("email", "send", json!({}));
        assert_eq!(params_of(&issues), vec!["to", "subject", "body"]);
    }

    #[test]
    fn unknown_param_and_missing_required_are_both_reported() {
        let issues = call(
            "email",
            "send",
            json!({"recipient": "a@b.c", "subject": "s", "body": "b"}),
        );
        assert!(issues
            .iter()
            .any(|i| matches!(i, ParamIssue::UnknownParam { param, .. } if param == "recipient")));
        assert!(issues
            .iter()
            .any(|i| matches!(i, ParamIssue::MissingRequired { param } if param == "to")));
    }

    #[test]
    fn numeric_type_mismatch_literal() {
        let issues = call(
            "email",
            "send",
            json!({"to": 5, "subject": "s", "body": "b"}),
        );
        assert_eq!(
            issues,
            vec![ParamIssue::TypeMismatch {
                param: "to".into(),
                expected: ArgKind::String,
                actual: ArgKind::Number
            }]
        );
    }

    // contracts-parity.test.ts: the installed contracts are the ones validated against
    #[test]
    fn every_installed_contract_accepts_a_call_stubbed_from_its_own_required_params() {
        let installed = crate::language::registry::baked_service_registry();
        let mut checked = 0;
        for (service, actions) in &installed.params {
            for (action, params) in actions {
                let mut args = Args::new();
                for p in params.iter().filter(|p| p.required) {
                    args.insert(
                        p.name.clone(),
                        checkable_type(&p.param_type).unwrap_or(ArgKind::String),
                    );
                }
                let issues = validate_params(installed, service, action, &args);
                assert!(issues.is_empty(), "{service}.{action}: {issues:?}");
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} contract actions checked");
    }

    #[test]
    fn installed_contracts_reject_an_empty_call_for_every_action_with_a_required_param() {
        let installed = crate::language::registry::baked_service_registry();
        for (service, actions) in &installed.params {
            for (action, params) in actions {
                let required = params.iter().filter(|p| p.required).count();
                let issues = validate_params(installed, service, action, &Args::new());
                assert_eq!(issues.len(), required, "{service}.{action}");
            }
        }
    }

    #[test]
    fn installed_email_send_matches_the_documented_contract() {
        let installed = crate::language::registry::baked_service_registry();
        let mut args = Args::new();
        for k in [
            "to",
            "subject",
            "body",
            "from",
            "htmlBody",
            "replyTo",
            "templateId",
        ] {
            args.insert(k.into(), ArgKind::String);
        }
        assert!(validate_params(installed, "email", "send", &args).is_empty());
        args.insert("recipient".into(), ArgKind::String);
        assert_eq!(validate_params(installed, "email", "send", &args).len(), 1);
    }
}
