//! Witness search: a payload that makes a guard come out as wanted, for the verify planner's steps. Moved from
//! `orbital-core-runtime` `planner.rs` unchanged; the planner builds the [`Declared`] context it needs.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::sexpr::SExpression;

pub fn is_binding(text: &str) -> bool {
    text.starts_with('@')
}

fn is_literal(expr: &SExpression) -> bool {
    match expr {
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => true,
        SExpression::String(s) => !is_binding(s),
        _ => false,
    }
}

/// The constant a guard folds to, or `None` when a binding remains.
pub fn const_truth(guard: &SExpression) -> Option<bool> {
    match guard {
        SExpression::Bool(b) => Some(*b),
        SExpression::String(s) => (!is_binding(s)).then_some(!s.is_empty()),
        SExpression::Array(items) => {
            let (SExpression::String(op), args) = items.split_first()? else {
                return None;
            };
            let literal_pair = |negate: bool| match args {
                [a, b] if is_literal(a) && is_literal(b) => Some((a == b) != negate),
                _ => None,
            };
            match op.as_str() {
                "=" | "==" | "eq" => literal_pair(false),
                "!=" | "ne" | "not-eq" | "neq" => literal_pair(true),
                "not" => args.first().and_then(const_truth).map(|v| !v),
                "or" => {
                    let subs: Vec<Option<bool>> = args.iter().map(const_truth).collect();
                    if subs.contains(&Some(true)) {
                        Some(true)
                    } else if subs.iter().all(|s| *s == Some(false)) {
                        Some(false)
                    } else {
                        None
                    }
                }
                "and" => {
                    let subs: Vec<Option<bool>> = args.iter().map(const_truth).collect();
                    if subs.contains(&Some(false)) {
                        Some(false)
                    } else if subs.iter().all(|s| *s == Some(true)) {
                        Some(true)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `base` adjusted so `guard` holds over `@payload.*`, or `None` when the guard is not a combination
/// of `and` / `or` / `not`, comparisons of a payload path with a literal and bare payload paths.
pub fn conform_payload(
    guard: &SExpression,
    base: &serde_json::Value,
    declared: &Declared,
) -> Option<serde_json::Value> {
    let mut payload = base.clone();
    solve_guard(guard, true, &mut payload, declared).then_some(payload)
}

/// What the event declares about its payload fields: a bare `@payload.x` guard is satisfied by a
/// value of `x`'s declared type, never by a boolean the event's payload validator refuses.
pub struct Declared {
    pub types: std::collections::BTreeMap<String, String>,
    pub base: serde_json::Value,
    pub seed_id: Option<String>,
    /// The row an array-typed payload field holds one of: the field's declared element entity, else the
    /// trait's linked entity.
    pub rows: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Declared {
    fn truthiness(&self, path: &[String], want: bool) -> serde_json::Value {
        use serde_json::Value as J;
        let declared = match path {
            [field] => self.types.get(field),
            _ => None,
        };
        let Some(declared) = declared else {
            return J::Bool(want);
        };
        let existing = self.base.get(&path[0]);
        match (declared.as_str(), want) {
            ("boolean", _) => J::Bool(want),
            ("string", true) => match existing {
                Some(J::String(s)) if !s.is_empty() => J::String(s.clone()),
                _ => J::String("verify".to_string()),
            },
            ("string", false) => J::String(String::new()),
            ("number", true) => match existing.and_then(J::as_f64) {
                Some(n) if n != 0.0 => number_json(n),
                _ => J::from(1),
            },
            ("number", false) => J::from(0),
            (_, true) => {
                serde_json::json!({ "id": self.seed_id.clone().unwrap_or_else(|| "verify".to_string()) })
            }
            (_, false) => J::Null,
        }
    }
}

pub fn solve_guard(
    expr: &SExpression,
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    match expr {
        SExpression::String(path) => match payload_path(path) {
            Some(path) => {
                let value = declared.truthiness(&path, want);
                set_payload_path(payload, &path, value);
                true
            }
            None => false,
        },
        SExpression::Array(items) => {
            let Some((SExpression::String(op), args)) = items.split_first() else {
                return false;
            };
            match op.as_str() {
                "not" => matches!(args, [arg] if solve_guard(arg, !want, payload, declared)),
                "and" | "or" => {
                    let all = (op == "and") == want;
                    if all {
                        let solved = !args.is_empty()
                            && args
                                .iter()
                                .all(|arg| solve_guard(arg, want, payload, declared));
                        if solved && op == "and" {
                            apply_field_intersections(args, payload);
                        }
                        solved
                    } else {
                        // An operand that already holds (or fails) whatever the payload is settles the form unchanged.
                        if args.iter().any(|arg| const_truth(arg) == Some(want)) {
                            return true;
                        }
                        args.iter().any(|arg| {
                            let mut attempt = payload.clone();
                            let solved = solve_guard(arg, want, &mut attempt, declared);
                            if solved {
                                *payload = attempt;
                            }
                            solved
                        })
                    }
                }
                "=" | "==" | "!=" | "<" | "<=" | ">" | ">=" => match args {
                    [lhs, rhs] => solve_comparison(
                        if op == "==" { "=" } else { op },
                        lhs,
                        rhs,
                        want,
                        payload,
                        declared,
                    ),
                    _ => false,
                },
                "object/has" | "object/get" | "array/includes" | "str/startsWith"
                | "validate/url" | "validate/required" | "grid/in-bounds" => {
                    solve_membership_form(op, args, want, payload, declared)
                }
                "array/some" | "array/every" => solve_quantifier(op, args, want, payload, declared),
                "if" => solve_if(args, want, payload, declared),
                "let" => solve_let(args, want, payload, declared),
                _ => false,
            }
        }
        _ => false,
    }
}

/// The finite set of values one payload field may take for `guard` to pass, when the guard bounds it that way:
/// the keys of a map (`object/has`, or an `object/get` comparison) or a single literal.
fn candidate_set(guard: &SExpression) -> Option<(Vec<String>, Vec<String>)> {
    let SExpression::Array(items) = guard else {
        return None;
    };
    let (SExpression::String(op), args) = items.split_first()? else {
        return None;
    };
    match (op.as_str(), args) {
        ("object/has", [a, b]) => match split_payload_arg(a, b)? {
            (path, SExpression::Object(map)) => Some((path, map.keys().cloned().collect())),
            _ => None,
        },
        ("=" | "==" | "!=", [lhs, rhs]) => {
            let negate = op == "!=";
            let get = |e: &SExpression| match e {
                SExpression::Array(g) if matches!(g.first(), Some(SExpression::String(f)) if f == "object/get") => {
                    Some(g.clone())
                }
                _ => None,
            };
            if let Some((g, literal)) = get(lhs)
                .map(|g| (g, rhs))
                .or_else(|| get(rhs).map(|g| (g, lhs)))
            {
                let (path, collection) = split_payload_arg(g.get(1)?, g.get(2)?)?;
                let SExpression::Object(map) = collection else {
                    return None;
                };
                let values = map
                    .iter()
                    .filter(|(_, v)| (*v == literal) != negate)
                    .map(|(k, _)| k.clone())
                    .collect();
                return Some((path, values));
            }
            if negate {
                return None;
            }
            match (lhs, rhs) {
                (SExpression::String(p), SExpression::String(lit))
                | (SExpression::String(lit), SExpression::String(p))
                    if !lit.starts_with('@') =>
                {
                    payload_path(p).map(|path| (path, vec![lit.clone()]))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Two conjuncts bounding the SAME field to candidate sets must agree on a value in both; a field left with no shared
/// candidate is unbound rather than guessed.
fn apply_field_intersections(conjuncts: &[SExpression], payload: &mut serde_json::Value) {
    let mut by_path: Vec<(Vec<String>, Vec<Vec<String>>)> = Vec::new();
    for sub in conjuncts {
        if let Some((path, values)) = candidate_set(sub) {
            match by_path.iter_mut().find(|(p, _)| *p == path) {
                Some((_, sets)) => sets.push(values),
                None => by_path.push((path, vec![values])),
            }
        }
    }
    for (path, sets) in by_path {
        if sets.len() < 2 {
            continue;
        }
        let mut intersection = sets[0].clone();
        for set in &sets[1..] {
            intersection.retain(|v| set.contains(v));
        }
        match intersection.first() {
            Some(value) => {
                set_payload_path(payload, &path, serde_json::Value::String(value.clone()))
            }
            None => remove_payload_path(payload, &path),
        }
    }
}

fn remove_payload_path(payload: &mut serde_json::Value, path: &[String]) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    if tail.is_empty() {
        map.remove(head);
    } else if let Some(child) = map.get_mut(head) {
        remove_payload_path(child, tail);
    }
}

fn sexpr_truthy(value: &SExpression) -> bool {
    match value {
        SExpression::Null => false,
        SExpression::Bool(b) => *b,
        SExpression::Number(n) => *n != 0.0,
        SExpression::String(s) => !s.is_empty(),
        SExpression::Array(a) => !a.is_empty(),
        SExpression::Object(o) => !o.is_empty(),
    }
}

/// A key guaranteed absent from `keys`.
fn absent_key(keys: &[String]) -> String {
    let mut key = "not-a-key".to_string();
    while keys.contains(&key) {
        key.push('_');
    }
    key
}

/// `(f collection @payload.path)` or `(f @payload.path collection)`: the payload path and the literal collection.
fn split_payload_arg<'a>(
    a: &'a SExpression,
    b: &'a SExpression,
) -> Option<(Vec<String>, &'a SExpression)> {
    let literal = |e: &SExpression| matches!(e, SExpression::Object(_) | SExpression::Array(_));
    match (a, b) {
        (SExpression::String(p), other) if literal(other) => {
            payload_path(p).map(|path| (path, other))
        }
        (other, SExpression::String(p)) if literal(other) => {
            payload_path(p).map(|path| (path, other))
        }
        _ => None,
    }
}

fn solve_membership_form(
    op: &str,
    args: &[SExpression],
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    use serde_json::Value as J;
    let set = |payload: &mut J, path: &[String], value: J| set_payload_path(payload, path, value);
    match (op, args) {
        ("object/has", [a, b]) => {
            let Some((path, SExpression::Object(map))) = split_payload_arg(a, b) else {
                return false;
            };
            let keys: Vec<String> = map.keys().cloned().collect();
            match (want, keys.first()) {
                (true, Some(first)) => set(payload, &path, J::String(first.clone())),
                (true, None) => return false,
                (false, _) => set(payload, &path, J::String(absent_key(&keys))),
            }
            true
        }
        ("object/get", [a, b]) => {
            let Some((path, SExpression::Object(map))) = split_payload_arg(a, b) else {
                return false;
            };
            let keys: Vec<String> = map.keys().cloned().collect();
            if want {
                let Some(pass) = map
                    .iter()
                    .find(|(_, v)| sexpr_truthy(v))
                    .map(|(k, _)| k.clone())
                else {
                    return false;
                };
                set(payload, &path, J::String(pass));
            } else {
                set(payload, &path, J::String(absent_key(&keys)));
            }
            true
        }
        ("array/includes", [a, b]) => {
            let Some((path, SExpression::Array(list))) = split_payload_arg(a, b) else {
                return false;
            };
            let Some(first) = list.first() else {
                return false;
            };
            let pass = literal_json(first);
            let Some(pass) = pass else { return false };
            if want {
                set(payload, &path, pass);
            } else {
                let fail = match &pass {
                    J::String(s) => J::String(format!("{s}-not-in-list")),
                    _ => J::String("not-in-list".to_string()),
                };
                set(payload, &path, fail);
            }
            true
        }
        ("str/startsWith", [SExpression::String(p), SExpression::String(prefix)]) => {
            let Some(path) = payload_path(p) else {
                return false;
            };
            let value = if want {
                format!("{prefix}mock-suffix")
            } else if prefix.is_empty() {
                return false;
            } else {
                format!("not-{prefix}")
            };
            set(payload, &path, J::String(value));
            true
        }
        ("validate/url", [SExpression::String(p)]) => {
            let Some(path) = payload_path(p) else {
                return false;
            };
            set(
                payload,
                &path,
                J::String(
                    if want {
                        "https://example.test/mock"
                    } else {
                        "not-a-url"
                    }
                    .to_string(),
                ),
            );
            true
        }
        ("validate/required", [SExpression::String(p)]) => {
            let Some(path) = payload_path(p) else {
                return false;
            };
            // A field the event types as a row (not a scalar) is required as a row of its type.
            let scalar = match path.as_slice() {
                [field] => declared
                    .types
                    .get(field)
                    .is_none_or(|t| matches!(t.as_str(), "string" | "number" | "boolean")),
                _ => true,
            };
            let value = if scalar {
                J::String(if want { "mock-required-value" } else { "" }.to_string())
            } else {
                declared.truthiness(&path, want)
            };
            set(payload, &path, value);
            true
        }
        ("grid/in-bounds", [position, w, h, ..]) => {
            let empty_grid = matches!(w, SExpression::Number(n) if *n <= 0.0)
                || matches!(h, SExpression::Number(n) if *n <= 0.0);
            if let SExpression::String(p) = position {
                let Some(path) = payload_path(p) else {
                    return false;
                };
                if want && empty_grid {
                    return false;
                }
                set(
                    payload,
                    &path,
                    serde_json::json!({ "x": if want { 0 } else { -1 }, "y": 0 }),
                );
                return true;
            }
            let SExpression::Object(pos) = position else {
                return false;
            };
            let axes: Vec<Vec<String>> = ["x", "y"]
                .iter()
                .filter_map(|k| match pos.get(*k) {
                    Some(SExpression::String(p)) => payload_path(p),
                    _ => None,
                })
                .collect();
            if axes.is_empty() || (want && empty_grid) {
                return false;
            }
            if want {
                axes.iter().for_each(|path| set(payload, path, J::from(0)));
            } else {
                set(payload, &axes[0], J::from(-1));
            }
            true
        }
        _ => false,
    }
}

/// Rename the quantifier variable `name` to `payload`, so its predicate reads the scratch element as the payload.
fn rename_var(expr: &SExpression, name: &str) -> SExpression {
    match expr {
        SExpression::String(s) => {
            let own = format!("@{name}");
            if *s == own {
                SExpression::String("@payload".to_string())
            } else if let Some(rest) = s.strip_prefix(&format!("{own}.")) {
                SExpression::String(format!("@payload.{rest}"))
            } else {
                expr.clone()
            }
        }
        SExpression::Array(items) => {
            SExpression::Array(items.iter().map(|i| rename_var(i, name)).collect())
        }
        SExpression::Object(map) => SExpression::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), rename_var(v, name)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `(array/some list (fn x pred))` / `(array/every …)` over a payload list: a one-element witness (or violator) built
/// from the predicate, solved as a payload of its own.
fn solve_quantifier(
    op: &str,
    args: &[SExpression],
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    if let [list, SExpression::Array(function)] = args {
        if let Some(elements) = literal_elements(list) {
            return solve_literal_quantifier(
                op == "array/some",
                &elements,
                function,
                want,
                payload,
                declared,
            );
        }
    }
    let [SExpression::String(list), SExpression::Array(function)] = args else {
        return false;
    };
    let (Some(path), Some(SExpression::String(name)), Some(body)) =
        (payload_path(list), function.get(1), function.last())
    else {
        return false;
    };
    if function.first() != Some(&SExpression::String("fn".to_string())) || function.len() < 3 {
        return false;
    }
    let predicate = rename_var(body, name);
    let constant = const_truth(&predicate);
    let element = |target: bool| -> Option<serde_json::Value> {
        if constant == Some(target) {
            return Some(serde_json::json!({ "id": "mock-test-id" }));
        }
        if constant.is_some() {
            return None;
        }
        let mut scratch = serde_json::json!({});
        (solve_guard(&predicate, target, &mut scratch, declared)
            && scratch.as_object().is_some_and(|o| !o.is_empty()))
        .then_some(scratch)
    };
    let some = op == "array/some";
    let list_value = match (some, want) {
        (true, false) => serde_json::json!([]),
        (_, true) => match element(true) {
            Some(witness) => serde_json::json!([witness]),
            None => return false,
        },
        (false, false) => match element(false) {
            Some(violator) => serde_json::json!([violator]),
            None => return false,
        },
    };
    set_payload_path(payload, &path, list_value);
    true
}

/// The elements of a literal array operand: `(list a b)` or an array value with no leading operator, read the way the
/// evaluator reads it (an unknown leading string is a value, not a call). `None` for anything computed.
fn literal_elements(expr: &SExpression) -> Option<Vec<SExpression>> {
    let SExpression::Array(items) = expr else {
        return None;
    };
    match items.first() {
        None => Some(Vec::new()),
        Some(SExpression::String(op)) if op == "list" => Some(items[1..].to_vec()),
        Some(SExpression::String(op))
            if crate::evaluator::operators::get_operator(op).is_some() =>
        {
            None
        }
        Some(_) => Some(items.clone()),
    }
}

/// `array/some|every` over a literal array: the quantifier is a disjunction or conjunction of the predicate with the
/// element bound in, so each element is steered with the guard solver itself. `some` true and `every` false need one
/// element to land on the wanted side; `some` false and `every` true need all of them.
fn solve_literal_quantifier(
    some: bool,
    elements: &[SExpression],
    function: &[SExpression],
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    let (Some(SExpression::String(name)), Some(body)) = (function.get(1), function.last()) else {
        return false;
    };
    if function.first() != Some(&SExpression::String("fn".to_string())) || function.len() < 3 {
        return false;
    }
    let any_element = some == want;
    if any_element {
        elements.iter().any(|element| {
            let mut attempt = payload.clone();
            let solved = solve_guard(
                &substitute_var(body, name, element),
                want,
                &mut attempt,
                declared,
            );
            if solved {
                *payload = attempt;
            }
            solved
        })
    } else {
        let mut attempt = payload.clone();
        let solved = elements.iter().all(|element| {
            solve_guard(
                &substitute_var(body, name, element),
                want,
                &mut attempt,
                declared,
            )
        });
        if solved {
            *payload = attempt;
        }
        solved
    }
}

/// `(if cond then else)` is true iff (cond and then) or (not cond and else); each target takes the first arm that
/// can reach it, skipping an arm whose own constant has the wrong truth.
fn solve_if(
    args: &[SExpression],
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    let (Some(cond), Some(then)) = (args.first(), args.get(1)) else {
        return false;
    };
    let otherwise = args.get(2).cloned().unwrap_or(SExpression::Bool(false));
    let (cc, ct, ce) = (
        const_truth(cond),
        const_truth(then),
        const_truth(&otherwise),
    );
    if cc != Some(false) && ct != Some(!want) {
        let mut attempt = payload.clone();
        if solve_guard(cond, true, &mut attempt, declared)
            && solve_guard(then, want, &mut attempt, declared)
        {
            *payload = attempt;
            return true;
        }
    }
    if cc != Some(true) && ce != Some(!want) {
        let mut attempt = payload.clone();
        if solve_guard(cond, false, &mut attempt, declared)
            && solve_guard(&otherwise, want, &mut attempt, declared)
        {
            *payload = attempt;
            return true;
        }
    }
    false
}

/// Replace the binding `name` in `expr` with `value`: the bare reference, a dotted path into it, or an `object/get`.
pub fn substitute_var(expr: &SExpression, name: &str, value: &SExpression) -> SExpression {
    let own = format!("@{name}");
    match expr {
        SExpression::String(s) if *s == own => value.clone(),
        SExpression::String(s) => match (s.strip_prefix(&format!("{own}.")), value) {
            (Some(rest), SExpression::String(v)) if v.starts_with('@') => {
                SExpression::String(format!("{v}.{rest}"))
            }
            (Some(rest), SExpression::Object(map)) => {
                let mut cur = SExpression::Object(map.clone());
                for segment in rest.split('.') {
                    cur = match &cur {
                        SExpression::Object(o) => {
                            o.get(segment).cloned().unwrap_or(SExpression::Null)
                        }
                        _ => SExpression::Null,
                    };
                }
                cur
            }
            _ => expr.clone(),
        },
        SExpression::Array(items) => {
            if let [SExpression::String(f), SExpression::String(bound), ..] = &items[..] {
                if f == "fn" && bound == name {
                    return expr.clone();
                }
            }
            SExpression::Array(
                items
                    .iter()
                    .map(|i| substitute_var(i, name, value))
                    .collect(),
            )
        }
        SExpression::Object(map) => SExpression::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute_var(v, name, value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `(let ((a e1) …) body)`: bind each name into the later bindings and the body, then steer the body.
fn solve_let(
    args: &[SExpression],
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    let (Some(SExpression::Array(bindings)), Some(body)) = (args.first(), args.last()) else {
        return false;
    };
    if args.len() < 2 {
        return false;
    }
    let mut pairs: Vec<(String, SExpression)> = Vec::new();
    for binding in bindings {
        let SExpression::Array(pair) = binding else {
            return false;
        };
        let (Some(SExpression::String(name)), Some(value)) = (pair.first(), pair.get(1)) else {
            return false;
        };
        pairs.push((name.clone(), value.clone()));
    }
    for i in 0..pairs.len() {
        for j in (i + 1)..pairs.len() {
            let replaced = substitute_var(&pairs[j].1, &pairs[i].0, &pairs[i].1);
            pairs[j].1 = replaced;
        }
    }
    let mut body = body.clone();
    for (name, value) in pairs.iter().rev() {
        body = substitute_var(&body, name, value);
    }
    solve_guard(&body, want, payload, declared)
}

fn payload_path(text: &str) -> Option<Vec<String>> {
    let rest = text.strip_prefix("@payload.")?;
    (!rest.is_empty()).then(|| rest.split('.').map(str::to_string).collect())
}

fn set_payload_path(payload: &mut serde_json::Value, path: &[String], value: serde_json::Value) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    if !payload.is_object() {
        *payload = serde_json::Value::Object(serde_json::Map::new());
    }
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    if tail.is_empty() {
        map.insert(head.clone(), value);
    } else {
        set_payload_path(
            map.entry(head.clone()).or_insert(serde_json::Value::Null),
            tail,
            value,
        );
    }
}

pub fn literal_json(expr: &SExpression) -> Option<serde_json::Value> {
    match expr {
        SExpression::Null => Some(serde_json::Value::Null),
        SExpression::Bool(b) => Some(serde_json::Value::Bool(*b)),
        SExpression::Number(n) => Some(number_json(*n)),
        SExpression::String(s) if !s.starts_with('@') => Some(serde_json::Value::String(s.clone())),
        _ => None,
    }
}

fn number_json(n: f64) -> serde_json::Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
}

/// Make `lhs op rhs` equal `want` by setting the one payload path among the operands to a value derived
/// from the other operand's literal.
/// `(op (array/len @payload.f) n)` or its mirror: `f` becomes as many declared rows as satisfy the comparison.
/// `None` when neither side is an array length of a payload field.
fn solve_array_length(
    op: &str,
    lhs: &SExpression,
    rhs: &SExpression,
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> Option<bool> {
    let length_path = |e: &SExpression| match e {
        SExpression::Array(items) => match &items[..] {
            [SExpression::String(f), SExpression::String(p)] if f == "array/len" => payload_path(p),
            _ => None,
        },
        _ => None,
    };
    let (path, literal, flipped) = match (length_path(lhs), length_path(rhs)) {
        (Some(path), None) => (path, rhs, false),
        (None, Some(path)) => (path, lhs, true),
        _ => return None,
    };
    let SExpression::Number(n) = literal else {
        return Some(false);
    };
    let op = match (op, want) {
        ("=", true) | ("!=", false) => "=",
        ("=", false) | ("!=", true) => "!=",
        ("<", true) | (">=", false) => "<",
        ("<=", true) | (">", false) => "<=",
        (">", true) | ("<=", false) => ">",
        (">=", true) | ("<", false) => ">=",
        _ => return Some(false),
    };
    let op = if flipped {
        match op {
            "<" => ">",
            "<=" => ">=",
            ">" => "<",
            ">=" => "<=",
            same => same,
        }
    } else {
        op
    };
    let n = *n;
    let length = match op {
        "=" | ">=" | "<=" => n,
        ">" | "!=" => n + 1.0,
        "<" => n - 1.0,
        _ => return Some(false),
    };
    if length < 0.0 || length.fract() != 0.0 {
        return Some(false);
    }
    let [field] = &path[..] else {
        return Some(false);
    };
    let Some(row) = declared.rows.get(field) else {
        return Some(false);
    };
    let rows = serde_json::Value::Array((0..length as usize).map(|_| row.clone()).collect());
    set_payload_path(payload, &path, rows);
    Some(true)
}

/// `(= (object/get MAP @payload.k) literal)`: `k` becomes a key of `MAP` whose value does (or does not) equal the literal.
fn solve_object_get_comparison(
    op: &str,
    lhs: &SExpression,
    rhs: &SExpression,
    want: bool,
    payload: &mut serde_json::Value,
) -> Option<bool> {
    let (get, literal) = match (lhs, rhs) {
        (SExpression::Array(g), l) | (l, SExpression::Array(g)) if matches!(g.first(), Some(SExpression::String(f)) if f == "object/get") => {
            (g, l)
        }
        _ => return None,
    };
    if !matches!(op, "=" | "!=") {
        return None;
    }
    let (Some(a), Some(b)) = (get.get(1), get.get(2)) else {
        return None;
    };
    let (path, SExpression::Object(map)) = split_payload_arg(a, b)? else {
        return Some(false);
    };
    let equal = (op == "=") == want;
    let keys: Vec<String> = map.keys().cloned().collect();
    let matching: Vec<&String> = map
        .iter()
        .filter(|(_, v)| (*v == literal) == equal)
        .map(|(k, _)| k)
        .collect();
    let key = match matching.first() {
        Some(k) => (*k).clone(),
        None if !equal => absent_key(&keys),
        None => return Some(false),
    };
    set_payload_path(payload, &path, serde_json::Value::String(key));
    Some(true)
}

/// The payload field a comparison reads: `@payload.x`, or `(str/default @payload.x "…")` (a read with a fallback).
fn compared_payload_path(expr: &SExpression) -> Option<Vec<String>> {
    match expr {
        SExpression::String(p) => payload_path(p),
        SExpression::Array(items) => match &items[..] {
            [SExpression::String(op), SExpression::String(p), SExpression::String(_)]
                if op == "str/default" =>
            {
                payload_path(p)
            }
            _ => None,
        },
        _ => None,
    }
}

fn solve_comparison(
    op: &str,
    lhs: &SExpression,
    rhs: &SExpression,
    want: bool,
    payload: &mut serde_json::Value,
    declared: &Declared,
) -> bool {
    if let Some(solved) = solve_array_length(op, lhs, rhs, want, payload, declared) {
        return solved;
    }
    if let Some(solved) = solve_object_get_comparison(op, lhs, rhs, want, payload) {
        return solved;
    }
    let (path, literal, flipped) = match (compared_payload_path(lhs), compared_payload_path(rhs)) {
        (Some(path), None) => match literal_json(rhs) {
            Some(literal) => (path, literal, false),
            None => return false,
        },
        (None, Some(path)) => match literal_json(lhs) {
            Some(literal) => (path, literal, true),
            None => return false,
        },
        _ => return false,
    };
    let op = match (op, want) {
        ("=", true) | ("!=", false) => "=",
        ("=", false) | ("!=", true) => "!=",
        ("<", true) | (">=", false) => "<",
        ("<=", true) | (">", false) => "<=",
        (">", true) | ("<=", false) => ">",
        (">=", true) | ("<", false) => ">=",
        _ => return false,
    };
    // `literal op path` reads as `path op' literal` with the direction reversed.
    let op = if flipped {
        match op {
            "<" => ">",
            "<=" => ">=",
            ">" => "<",
            ">=" => "<=",
            same => same,
        }
    } else {
        op
    };
    let value = match (&literal, op) {
        (_, "=") => literal.clone(),
        (_, "<=" | ">=") => literal.clone(),
        (serde_json::Value::Number(n), "<") => number_json(n.as_f64().unwrap_or(0.0) - 1.0),
        (serde_json::Value::Number(n), ">") => number_json(n.as_f64().unwrap_or(0.0) + 1.0),
        (serde_json::Value::Number(n), "!=") => number_json(n.as_f64().unwrap_or(0.0) + 1.0),
        (serde_json::Value::Bool(b), "!=") => serde_json::Value::Bool(!b),
        (serde_json::Value::String(s), "!=") => serde_json::Value::String(format!("{s}-other")),
        (serde_json::Value::Null, "!=") => serde_json::Value::Bool(true),
        _ => return false,
    };
    set_payload_path(payload, &path, value);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declared(types: &[(&str, &str)]) -> Declared {
        Declared {
            types: types
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            base: json!({}),
            seed_id: Some("Contact Id 1".to_string()),
            rows: Default::default(),
        }
    }

    fn guard(expr: serde_json::Value) -> SExpression {
        serde_json::from_value(expr).unwrap()
    }

    #[test]
    fn a_required_object_field_gets_a_row_not_a_string() {
        let payload = conform_payload(
            &guard(json!(["validate/required", "@payload.row"])),
            &json!({}),
            &declared(&[("row", "Contact")]),
        );
        assert_eq!(payload, Some(json!({ "row": { "id": "Contact Id 1" } })));
    }

    #[test]
    fn control_a_required_string_field_gets_a_non_empty_string() {
        let payload = conform_payload(
            &guard(json!(["validate/required", "@payload.name"])),
            &json!({}),
            &declared(&[("name", "string")]),
        );
        assert_eq!(payload, Some(json!({ "name": "mock-required-value" })));
    }

    #[test]
    fn edge_an_or_settled_by_a_constant_true_operand_writes_nothing() {
        let expr = guard(json!([
            "or",
            ["=", "create", "create"],
            ["validate/required", "@payload.row"]
        ]));
        assert_eq!(
            conform_payload(&expr, &json!({}), &declared(&[("row", "Contact")])),
            Some(json!({}))
        );
    }

    #[test]
    fn control_an_or_with_a_constant_false_operand_solves_the_next() {
        let expr = guard(json!([
            "or",
            ["=", "create", "edit"],
            ["validate/required", "@payload.row"]
        ]));
        assert_eq!(
            conform_payload(&expr, &json!({}), &declared(&[("row", "Contact")])),
            Some(json!({ "row": { "id": "Contact Id 1" } }))
        );
    }
}
