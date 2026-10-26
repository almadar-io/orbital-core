//! Substitution, sorts and labels shared by the prover's passes.

use super::*;

// ---------------------------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------------------------

/// `expr` with each `@config.<knob>` read as the trait's resolved config holds it for the run (call-site overrides
/// are folded in): a scalar default is that constant, no default reads null, and a default bound at run time
/// (`@user.…`, a forward) or a structured one is unresolved. A knob the trait does not declare is left as written.
pub(super) fn config_read(td: &TraitDefinition, expr: &SExpression) -> SExpression {
    let Some(config) = td.config.as_ref() else {
        return expr.clone();
    };
    subst_fields(expr, "@config", &|knob| {
        let field = config.get(knob)?;
        Some(match &field.default {
            None | Some(SExpression::Null) => SExpression::Null,
            Some(v @ (SExpression::Number(_) | SExpression::Bool(_))) => v.clone(),
            Some(SExpression::String(s)) if !s.starts_with('@') => SExpression::String(s.clone()),
            Some(other) => unresolved(&format!("@config.{knob} = {}", canonical(other))),
        })
    })
}

pub(super) fn configured(td: &TraitDefinition, mut o: Obligation) -> Obligation {
    o.assumptions = o.assumptions.iter().map(|a| config_read(td, a)).collect();
    o.goal = config_read(td, &o.goal);
    o
}

pub(super) fn bind(root: &str, field: &str) -> SExpression {
    SExpression::String(format!("{root}.{field}"))
}

pub fn unresolved(what: &str) -> SExpression {
    SExpression::Array(vec![
        SExpression::String(UNRESOLVED.to_string()),
        SExpression::String(what.to_string()),
    ])
}

pub(super) fn canonical(e: &SExpression) -> String {
    serde_json::to_string(e).unwrap_or_default()
}

pub(super) fn join(notes: &[String]) -> Option<String> {
    (!notes.is_empty()).then(|| notes.join("; "))
}

pub(super) fn path_label(paths: &[Path], i: usize) -> String {
    if paths.len() > 1 {
        format!(" [branch {}]", i + 1)
    } else {
        String::new()
    }
}

pub(super) fn unbuildable(prop: &Proposition, site: &str, reason: String) -> Obligation {
    Obligation {
        proposition: prop.clone(),
        site: site.to_string(),
        assumptions: Vec::new(),
        goal: SExpression::Bool(true),
        vars: Vars::new(),
        inexact: None,
        unbuildable: Some(reason),
    }
}

/// `expr` with each read `<root>.<field>[.rest]` replaced by `value(field)` when it gives one: a binding is
/// re-rooted (keeping `.rest`), any other value replaces a bare field read, and a nested read of a non-binding value
/// is unresolved. Exact whole-record replacement uses the same traversal separately.
pub fn subst_fields(
    expr: &SExpression,
    root: &str,
    value: &dyn Fn(&str) -> Option<SExpression>,
) -> SExpression {
    subst_record(expr, root, value, &SExpression::String(root.to_string()))
}

pub(super) fn subst_record(
    expr: &SExpression,
    root: &str,
    value: &dyn Fn(&str) -> Option<SExpression>,
    whole: &SExpression,
) -> SExpression {
    match expr {
        SExpression::String(s) => {
            let Some(rest) = s.strip_prefix(root) else {
                return expr.clone();
            };
            if rest.is_empty() {
                return whole.clone();
            }
            let Some(path) = rest.strip_prefix('.') else {
                return expr.clone();
            };
            let (field, tail) = match path.split_once('.') {
                Some((f, t)) => (f, Some(t)),
                None => (path, None),
            };
            match (value(field), tail) {
                (None, _) => expr.clone(),
                (Some(v), None) => v,
                (Some(SExpression::String(b)), Some(t)) if b.starts_with('@') => {
                    SExpression::String(format!("{b}.{t}"))
                }
                (Some(_), Some(_)) => unresolved(s),
            }
        }
        SExpression::Array(items) => SExpression::Array(
            items
                .iter()
                .map(|i| subst_record(i, root, value, whole))
                .collect(),
        ),
        SExpression::Object(map) => SExpression::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), subst_record(v, root, value, whole)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn default_of(def: &EntityDefinition, field: &str) -> SExpression {
    def.fields
        .iter()
        .find(|f| f.name.as_deref() == Some(field))
        .and_then(|f| f.default.clone())
        .unwrap_or(SExpression::Null)
}

pub fn field_sort(f: &FieldDefinition) -> Option<Sort> {
    match f.field_type {
        FieldType::Number | FieldType::Int | FieldType::Money | FieldType::Instant => Some(Sort::Num),
        FieldType::Boolean => Some(Sort::Bool),
        FieldType::Enum => Some(Sort::Str(f.values.clone())),
        FieldType::String
        | FieldType::Date
        | FieldType::Datetime
        | FieldType::Timestamp
        | FieldType::Email
        | FieldType::Url
        | FieldType::Phone
        | FieldType::Uuid
        | FieldType::Image => Some(Sort::Str(f.values.clone())),
        _ => None,
    }
}

pub(super) fn payload_sort(f: &PayloadField) -> Option<Sort> {
    match f.field_type.as_str() {
        "number" | "money" | "instant" => Some(Sort::Num),
        "boolean" => Some(Sort::Bool),
        "string" | "enum" | "date" | "datetime" | "timestamp" | "email" | "url" | "phone"
        | "uuid" | "image" => Some(Sort::Str(f.values.clone())),
        _ => None,
    }
}

/// `<root>.<field>` for every typed field of `def`; `id` is never null in a stored row.
pub(super) fn add_entity_vars(vars: &mut Vars, root: &str, def: &EntityDefinition, all_nullable: bool) {
    for f in &def.fields {
        let (Some(name), Some(sort)) = (f.name.as_deref(), field_sort(f)) else {
            continue;
        };
        let nullable =
            all_nullable || f.nullable == Some(true) || (f.required != Some(true) && name != "id");
        vars.insert(format!("{root}.{name}"), VarInfo { sort, nullable });
    }
    if !all_nullable {
        vars.entry(format!("{root}.id")).or_insert(VarInfo {
            sort: Sort::Str(None),
            nullable: false,
        });
    }
}
