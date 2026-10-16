//! There are no generic types in the language. The installed operator table and the installed service contracts
//! declare none: no `any`, `unknown`, `json`, `scalar`, bare `object`, bare `array`, untyped lambda or empty open object.
//! Polymorphism is by type variables, each bound by a parameter, and a record constraint appears only as a
//! variable's bound. The twin of `@almadar/std` `__tests__/operator-signatures.test.ts` and `@almadar/core`
//! `__tests__/service-registry-no-generic-types.test.ts`, run against what the compiler actually reads.

use orbital_core_base::language::registry::{
    operators, EmbeddedOperator, OperatorTypeRef, OperatorTypeRefKind as K,
};
use {orbital_core_base::language_data::language_data_after_preflight, orbital_core_eval::language_loader::preflight};
use serde_json::Value;
use std::collections::BTreeSet;

const GENERIC: [&str; 10] = [
    "any", "unknown", "json", "Json", "object", "array", "lambda", "function", "expression", "scalar",
];
const VARIABLES: [&str; 12] = ["T", "U", "K", "V", "R", "A", "B", "E", "P", "S", "O", "J"];

fn walk(ty: &OperatorTypeRef, path: &str, visit: &mut dyn FnMut(&OperatorTypeRef, &str)) {
    visit(ty, path);
    let OperatorTypeRef::Tagged(kind) = ty else {
        return;
    };
    match kind {
        K::Union { of } => of.iter().enumerate().for_each(|(i, m)| walk(m, &format!("{path}|{i}"), visit)),
        K::Array { of } => walk(of, &format!("{path}[]"), visit),
        K::Tuple { items } => items.iter().enumerate().for_each(|(i, m)| walk(m, &format!("{path}.{i}"), visit)),
        K::Map { key, value } => {
            walk(key, &format!("{path}.key"), visit);
            walk(value, &format!("{path}.value"), visit);
        }
        K::Object { fields, .. } => fields.iter().for_each(|(k, v)| walk(v, &format!("{path}.{k}"), visit)),
        K::Binding { shape: Some(shape) } => walk(shape, &format!("{path}.shape"), visit),
        K::Sexpr { of: Some(of) } => walk(of, &format!("{path}.of"), visit),
        K::Var { bound: Some(bound), .. } => walk(bound, &format!("{path}.bound"), visit),
        K::Fn { params, returns } => {
            params.iter().enumerate().for_each(|(i, m)| walk(m, &format!("{path}.p{i}"), visit));
            walk(returns, &format!("{path}.returns"), visit);
        }
        _ => {}
    }
}

fn positions<'a>(name: &str, op: &'a EmbeddedOperator) -> Vec<(String, &'a OperatorTypeRef)> {
    let mut out: Vec<(String, &'a OperatorTypeRef)> = op
        .params
        .iter()
        .map(|p| (format!("{name} param {}", p.name), &p.ty))
        .collect();
    out.push((format!("{name} returnType"), &op.return_type));
    if let Some(effect) = &op.effect {
        if let Some(produces) = &effect.produces {
            out.push((format!("{name} effect.produces"), produces));
        }
        for (key, ty) in effect.config.iter().flatten() {
            out.push((format!("{name} effect.config.{key}"), ty));
        }
    }
    out
}

fn variables_of(ty: &OperatorTypeRef) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk(ty, "", &mut |t, _| {
        if let OperatorTypeRef::Tagged(K::Var { name, .. }) = t {
            found.insert(name.clone());
        }
    });
    found
}

fn installed() {
    preflight().expect("the installed language data must load");
}

#[test]
fn the_operator_table_names_no_generic_type() {
    installed();
    let mut offenders = Vec::new();
    for (name, op) in operators() {
        for (where_, ty) in positions(name, op) {
            walk(ty, &where_, &mut |t, path| match t {
                OperatorTypeRef::Primitive(p) => {
                    let base = p.trim_end_matches("[]");
                    if GENERIC.contains(&base) || p.contains('[') || p.contains('|') {
                        offenders.push(format!("{path}: {p}"));
                    }
                }
                OperatorTypeRef::Tagged(K::Object { fields, .. }) if fields.is_empty() => {
                    offenders.push(format!("{path}: empty object"));
                }
                OperatorTypeRef::Tagged(K::Record) if !path.contains(".bound") => {
                    offenders.push(format!("{path}: record outside a variable bound"));
                }
                _ => {}
            });
        }
    }
    offenders.sort();
    assert!(offenders.is_empty(), "{} generic types:\n{}", offenders.len(), offenders.join("\n"));
}

#[test]
fn every_variable_is_declared_and_every_result_variable_is_bound_by_a_parameter() {
    installed();
    let mut problems = Vec::new();
    for (name, op) in operators() {
        for (where_, ty) in positions(name, op) {
            for v in variables_of(ty) {
                if !VARIABLES.contains(&v.as_str()) {
                    problems.push(format!("{where_}: undeclared variable {v}"));
                }
            }
        }
        if op.has_side_effects {
            continue;
        }
        let bound: BTreeSet<String> = op.params.iter().flat_map(|p| variables_of(&p.ty)).collect();
        for v in variables_of(&op.return_type) {
            if !bound.contains(&v) {
                problems.push(format!("{name}: result variable {v} is bound by no parameter"));
            }
        }
    }
    problems.sort();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_service_contracts_declare_no_generic_type() {
    installed();
    let registry: Value = serde_json::from_str(&language_data_after_preflight().services).unwrap();
    let mut found = Vec::new();
    fn visit(node: &Value, path: &str, found: &mut Vec<String>) {
        let Value::Object(map) = node else { return };
        let has = |k: &str| match map.get(k) {
            None | Some(Value::Null) => false,
            Some(Value::Object(o)) => !o.is_empty(),
            Some(_) => true,
        };
        let shaped = has("properties") || has("items") || has("mapValue") || has("cyclic") || has("shapedBy");
        if let Some(Value::String(ty)) = map.get("type") {
            for arm in ty.split('|') {
                let base = arm.trim().trim_end_matches("[]");
                if ["object", "array", "any", "unknown", "json", "Json", "scalar"].contains(&base) && !shaped {
                    found.push(format!("{path}: {ty}"));
                }
            }
        }
        if let Some(Value::Object(props)) = map.get("properties") {
            for (k, v) in props {
                visit(v, &format!("{path}.{k}"), found);
            }
        }
        for key in ["items", "mapValue"] {
            if let Some(v) = map.get(key) {
                visit(v, &format!("{path}.{key}"), found);
            }
        }
    }
    for (service, entry) in registry["services"].as_object().unwrap() {
        for action in entry["actions"].as_array().unwrap() {
            let action_name = action["name"].as_str().unwrap();
            for param in action["params"].as_array().unwrap() {
                visit(param, &format!("{service}.{action_name}({})", param["name"].as_str().unwrap()), &mut found);
            }
            if let Some(shape) = action["responseShape"].as_object() {
                for (field, v) in shape {
                    let node = match v {
                        Value::String(s) => serde_json::json!({ "type": s }),
                        other => other.clone(),
                    };
                    visit(&node, &format!("{service}.{action_name} -> {field}"), &mut found);
                }
            }
        }
    }
    assert!(found.is_empty(), "{} generic types:\n{}", found.len(), found.join("\n"));
}

#[test]
fn control_the_scan_sees_a_generic_type() {
    let bad: OperatorTypeRef = serde_json::from_value(serde_json::json!("any")).unwrap();
    let mut hits = 0;
    walk(&bad, "x", &mut |t, _| {
        if matches!(t, OperatorTypeRef::Primitive(p) if GENERIC.contains(&p.as_str())) {
            hits += 1;
        }
    });
    assert_eq!(hits, 1);
    let ok: OperatorTypeRef =
        serde_json::from_value(serde_json::json!({ "kind": "array", "of": { "kind": "var", "name": "T" } })).unwrap();
    assert_eq!(variables_of(&ok), BTreeSet::from(["T".to_string()]));
}
