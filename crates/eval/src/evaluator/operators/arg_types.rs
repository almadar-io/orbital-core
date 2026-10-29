//! Operator signatures and declared shapes. The kernel raises a type mismatch where an OPERATOR reads an argument
//! of the wrong kind; it runs no separate per-call check of an argument against the signature, because
//! `orb validate` checks every declared signature statically and the kernel trusts validated types. Run-time checks
//! exist only where a `.lolo` line declares them: `as`, `json/parse T`, `xml/parse T` (this file's
//! [`declared_shape`] / [`misfit_of_shape`]) and the entry checks of EXEC-EVAL-10. What the dispatch reads from the
//! installed operator table is the arity (`Signature::arity_breach`), one lookup per call.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::sync::OnceLock;
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{discriminant, Discriminant};
use std::sync::atomic::{AtomicBool, Ordering};

use super::get_operator;
use crate::evaluator::operators::kind::OperatorKind;
use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::ir::types::{DeclaredShape, OirType};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::signature::Signature;
use orbital_core_base::value::Value;
use std::collections::BTreeMap;

/// Hashes an operator's discriminant to itself: the table is read on every operator call, and a keyed hash of one
/// small integer is most of what a lookup cost.
#[derive(Default)]
struct DiscriminantHasher(u64);

impl std::hash::Hasher for DiscriminantHasher {
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self
                .0
                .wrapping_mul(0x100_0000_01b3)
                .wrapping_add(u64::from(*byte) + 1);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type SignatureTable = HashMap<
    Discriminant<OperatorKind>,
    Signature<'static>,
    std::hash::BuildHasherDefault<DiscriminantHasher>,
>;

/// The signatures of the operators the evaluator dispatches, by operator.
static SIGNATURES: OnceLock<SignatureTable> = OnceLock::new();
static LOAD_FAILED: AtomicBool = AtomicBool::new(false);

fn build_signatures() -> SignatureTable {
    let mut signatures = SignatureTable::default();
    for (name, meta) in orbital_core_base::language::registry::operators() {
        if meta.has_side_effects {
            continue;
        }
        let Some(kind) = get_operator(name) else {
            continue;
        };
        if matches!(kind, OperatorKind::BehaviorCompositionStubOp(_)) {
            continue;
        }
        signatures.insert(discriminant(&kind), Signature::of(meta));
    }
    signatures
}

/// The signature the installed operator table declares for `kind`. Without an installed table there is none; a
/// failed load is not retried, but a table installed later is still honored.
pub fn declared_signature(kind: &OperatorKind) -> Option<Signature<'static>> {
    let signatures = match SIGNATURES.get() {
        Some(signatures) => signatures,
        None => {
            if orbital_core_base::language_data::operator_data_if_loaded().is_none() {
                if LOAD_FAILED.load(Ordering::Relaxed) {
                    return None;
                }
                if crate::language_loader::operator_data().is_err() {
                    LOAD_FAILED.store(true, Ordering::Relaxed);
                    return None;
                }
            }
            SIGNATURES.get_or_init(build_signatures)
        }
    };
    signatures.get(&discriminant(kind)).copied()
}

/// The signature the installed operator table declares for the evaluated operator `name` (an alias resolves to
/// its operator); the one source the validator reads.
pub fn operator_signature(name: &str) -> Option<Signature<'static>> {
    declared_signature(&get_operator(name)?)
}

orbital_core_base::kernel_local! {
    static DECLARED: RefCell<HashMap<String, std::rc::Rc<DeclaredShape>>> = RefCell::new(HashMap::new());
    /// The shapes by the address of the shared text they were read from, which each entry keeps alive so the address
    /// is never reused: a program's declared-shape text is one allocation read on every call, and hashing it whole was the cost.
    static DECLARED_AT: RefCell<HashMap<usize, (orbital_core_base::value::Shared<String>, std::rc::Rc<DeclaredShape>), std::hash::BuildHasherDefault<DiscriminantHasher>>> = RefCell::new(HashMap::default());
}

/// The declared shape a `let` binding or a `json/parse` shape records as JSON text: a bare `OirType`, or the type with
/// the definitions of the program types it names (`DeclaredShape`).
pub fn declared_shape(arg: &SExpression) -> Result<std::rc::Rc<DeclaredShape>, OrbitalError> {
    let SExpression::String(text) = arg else {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument(
            "declared type: the shape must be JSON text".into(),
        )));
    };
    let at = text.as_str().as_ptr() as usize;
    if let Some(known) = DECLARED_AT.with(|d| {
        d.borrow()
            .get(&at)
            .map(|(_, shape)| std::rc::Rc::clone(shape))
    }) {
        return Ok(known);
    }
    if let Some(known) = DECLARED.with(|d| d.borrow().get(text.as_str()).cloned()) {
        DECLARED_AT.with(|d| {
            d.borrow_mut()
                .insert(at, (text.clone(), std::rc::Rc::clone(&known)))
        });
        return Ok(known);
    }
    let shape = DeclaredShape::read(text).map_err(|error| {
        OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "declared type: the shape is not a type: {error}"
        )))
    })?;
    let shape = std::rc::Rc::new(shape);
    DECLARED.with(|d| {
        d.borrow_mut()
            .insert(text.clone().into_inner(), std::rc::Rc::clone(&shape))
    });
    DECLARED_AT.with(|d| {
        d.borrow_mut()
            .insert(at, (text.clone(), std::rc::Rc::clone(&shape)))
    });
    Ok(shape)
}

/// The type a declared type is written as in a message: `[number | string]`, `[Adv | string]`, `[SExpr]`.
pub fn declared_text(ty: &OirType) -> String {
    match ty {
        OirType::Array { inner } => format!("[{}]", declared_text(inner)),
        OirType::Optional { inner } => format!("{}?", declared_text(inner)),
        OirType::Union { variants } => variants
            .iter()
            .map(|v| match &v.field_type {
                OirType::Struct { .. } | OirType::TypeRef { .. } | OirType::Row { .. } => {
                    v.name.clone()
                }
                other => declared_text(other),
            })
            .collect::<Vec<_>>()
            .join(" | "),
        OirType::Map { key, value } => {
            format!("Map {} {}", declared_text(key), declared_text(value))
        }
        OirType::Tuple { items } => format!(
            "[{}]",
            items
                .iter()
                .map(declared_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        OirType::SExpr => "SExpr".to_string(),
        OirType::FieldRef { .. } => "string".to_string(),
        OirType::Struct { .. } | OirType::File => "object".to_string(),
        OirType::Row { entity } | OirType::Reference { entity } => entity.clone(),
        OirType::TypeRef { name } | OirType::Enum { name } | OirType::Var { name } => name.clone(),
        OirType::Integer | OirType::Money => "number".to_string(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
            .unwrap_or_else(|| "a value".to_string()),
    }
}

/// The type a by-name reference resolves to: a `TypeRef` or `Reference` whose name the recorded shape's `named` table
/// defines is that definition, through a chain of aliases; `None` when `ty` names no defined type (or a cycle of bare
/// aliases).
fn declared_alias<'a>(
    ty: &'a OirType,
    named: &'a BTreeMap<String, OirType>,
) -> Option<&'a OirType> {
    let mut resolved: Option<&'a OirType> = None;
    // A chain longer than the names `named` declares repeats one: a cycle.
    for _ in 0..=named.len() {
        let name = match resolved.unwrap_or(ty) {
            OirType::TypeRef { name } | OirType::Reference { entity: name } => name.as_str(),
            _ => return resolved,
        };
        match named.get(name) {
            Some(next) => resolved = Some(next),
            None => return resolved,
        }
    }
    None
}

/// Whether `union` admits every JSON-shaped value: it holds string, number and boolean, a list of itself and a map of
/// itself (each item optionally nullable). Answers how a list and a map item may be null.
fn json_total(union: &OirType, named: &BTreeMap<String, OirType>) -> Option<(bool, bool)> {
    let OirType::Union { variants } = union else {
        return None;
    };
    let mut kinds = (false, false, false);
    let (mut list, mut map) = (None, None);
    let itself = |inner: &OirType| -> Option<bool> {
        let (inner, nullable) = match inner {
            OirType::Optional { inner } => (inner.as_ref(), true),
            other => (other, false),
        };
        declared_alias(inner, named)
            .is_some_and(|resolved| std::ptr::eq(resolved, union))
            .then_some(nullable)
    };
    for variant in variants.iter() {
        match &variant.field_type {
            OirType::String => kinds.0 = true,
            OirType::Number => kinds.1 = true,
            OirType::Boolean => kinds.2 = true,
            OirType::Array { inner } => list = list.or(itself(inner)),
            OirType::Map { key, value } if matches!(key.as_ref(), OirType::String) => {
                map = map.or(itself(value))
            }
            _ => {}
        }
    }
    match (kinds, list, map) {
        ((true, true, true), Some(list), Some(map)) => Some((list, map)),
        _ => None,
    }
}

/// A value of a JSON-total union fits unless it holds a lambda or a null where the union admits none.
fn json_fits(value: &Value, list_null: bool, map_null: bool) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Array(items) => items.iter().all(|item| {
            if item.is_null() {
                list_null
            } else {
                json_fits(item, list_null, map_null)
            }
        }),
        Value::Object(fields) => fields.values().all(|item| {
            if item.is_null() {
                map_null
            } else {
                json_fits(item, list_null, map_null)
            }
        }),
        Value::Lambda { .. } => false,
    }
}

/// Where a value departs from its declared type: the path inside it, the kind found there, and the declared type at that path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misfit {
    pub path: String,
    pub actual: &'static str,
    pub expected: String,
}

impl Misfit {
    fn new(path: String, actual: &'static str, ty: &OirType) -> Self {
        Self {
            path,
            actual,
            expected: declared_text(ty),
        }
    }

    fn here(value: &Value, ty: &OirType) -> Self {
        Self::new(String::new(), value.type_name(), ty)
    }
}

/// The first mismatch under the strict type contract: null passes only at an optional type. A name `named` defines is checked as its definition,
/// so each item of a list of a recursive union matches a member of the union on its own; a closed string set
/// (`"a" | "b"`) admits only its members.
fn first_misfit(
    value: &Value,
    ty: &OirType,
    named: &BTreeMap<String, OirType>,
) -> Option<Misfit> {
    if value.is_null() && matches!(ty, OirType::Optional { .. }) {
        return None;
    }
    // A reference is the related row's id, or the row itself, which is checked as one.
    if let (OirType::Reference { .. }, Value::String(_)) = (ty, value) {
        return None;
    }
    if let Some(declared) = declared_alias(ty, named) {
        if let Some((list_null, map_null)) = json_total(declared, named) {
            if json_fits(value, list_null, map_null) {
                return None;
            }
        }
        return first_misfit(value, declared, named).map(|mut found| {
            if found.path.is_empty() {
                found.expected = declared_text(ty);
            }
            found
        });
    }
    let misfit = || Some(Misfit::here(value, ty));
    if value.is_null() && !matches!(ty, OirType::Union { .. }) {
        return misfit();
    }
    // The path is written only for a misfit: a value that fits allocates nothing.
    let at = |prefix: std::fmt::Arguments<'_>, mut found: Misfit| {
        found.path = format!("{prefix}{}", found.path);
        found
    };
    let items = |items: &[Value], inner: &OirType| {
        items.iter().enumerate().find_map(|(i, item)| {
            first_misfit(item, inner, named).map(|misfit| at(format_args!("[{i}]"), misfit))
        })
    };
    match ty {
        OirType::SExpr
        | OirType::Json
        | OirType::Node
        | OirType::Trait
        | OirType::Orbital
        | OirType::Behavior => None,
        // An unbound variable admits any value; a bound one was substituted before run time.
        OirType::Var { .. } => None,
        OirType::Optional { inner } => first_misfit(value, inner, named),
        OirType::Union { variants } => {
            if variants
                .iter()
                .any(|v| first_misfit(value, &v.field_type, named).is_none())
            {
                None
            } else {
                misfit()
            }
        }
        OirType::Array { inner } => match value {
            Value::Array(list) => items(list, inner),
            _ => misfit(),
        },
        OirType::Tuple { items: slots } => match value {
            Value::Array(list) if list.len() == slots.len() => list
                .iter()
                .zip(slots)
                .enumerate()
                .find_map(|(i, (item, slot))| {
                    first_misfit(item, slot, named)
                        .map(|misfit| at(format_args!("[{i}]"), misfit))
                }),
            _ => misfit(),
        },
        OirType::Map { value: inner, .. } => match value {
            Value::Object(map) => map.iter().find_map(|(k, item)| {
                first_misfit(item, inner, named)
                    .map(|misfit| at(format_args!(".{k}"), misfit))
            }),
            _ => misfit(),
        },
        OirType::Struct { fields, additional } => match value {
            Value::Object(map) => fields
                .iter()
                .find_map(|field| match map.get(&field.name) {
                    None => field.required.then(|| {
                        Misfit::new(format!(".{}", field.name), "absent", &field.field_type)
                    }),
                    Some(item) => first_misfit(item, &field.field_type, named)
                        .map(|misfit| at(format_args!(".{}", field.name), misfit)),
                })
                .or_else(|| {
                    additional.as_deref().and_then(|additional| {
                        map.iter()
                            .filter(|(key, _)| {
                                !fields
                                    .iter()
                                    .any(|field| field.name.as_str() == key.as_str())
                            })
                            .find_map(|(key, item)| {
                                first_misfit(item, additional, named)
                                    .map(|misfit| at(format_args!(".{key}"), misfit))
                            })
                    })
                }),
            _ => misfit(),
        },
        OirType::File | OirType::Row { .. } | OirType::TypeRef { .. } => {
            (!matches!(value, Value::Object(_))).then(|| Misfit::here(value, ty))
        }
        OirType::Reference { .. } => {
            (!matches!(value, Value::Object(_) | Value::String(_))).then(|| Misfit::here(value, ty))
        }
        OirType::Number | OirType::Integer | OirType::Money => {
            (!matches!(value, Value::Number(_))).then(|| Misfit::here(value, ty))
        }
        OirType::Boolean => (!matches!(value, Value::Bool(_))).then(|| Misfit::here(value, ty)),
        OirType::Date | OirType::DateTime | OirType::Instant => {
            let wanted = if matches!(ty, OirType::Date) {
                super::system::IsoShape::Date
            } else {
                super::system::IsoShape::DateTime
            };
            let fits = matches!(value, Value::String(text)
                if super::system::parse_iso(text).is_some_and(|(_, found)| found == wanted));
            (!fits).then(|| Misfit::here(value, ty))
        }
        OirType::Bytes => match value {
            Value::String(text) if orbital_core_base::codec::base64_decode(text).is_ok() => None,
            _ => misfit(),
        },
        OirType::Enum { name } => match (
            value,
            orbital_core_base::type_compat::decode_literal_union(name),
        ) {
            (Value::String(text), Ok(Some(members))) => {
                (!members.contains(text)).then(|| Misfit::new(String::new(), "string", ty))
            }
            (Value::String(_), Ok(None)) => None,
            _ => misfit(),
        },
        _ => (!matches!(value, Value::String(_))).then(|| Misfit::here(value, ty)),
    }
}

/// Where `value` departs from the declared `ty` (`""` for the value itself, `.rows[2].id` inside it) and what kind of
/// value is there, when it does; a by-name reference reads its definition from `named`.
pub fn misfit_of_declared(
    value: &Value,
    ty: &OirType,
    named: &BTreeMap<String, OirType>,
) -> Option<(String, &'static str)> {
    first_misfit(value, ty, named).map(|found| (found.path, found.actual))
}

/// `value` is of the declared `ty`, kind by kind, and a record's declared fields field by field.
pub fn value_of_declared(
    value: &Value,
    ty: &OirType,
    named: &BTreeMap<String, OirType>,
) -> Result<(), OrbitalError> {
    match first_misfit(value, ty, named) {
        None => Ok(()),
        Some(found) => Err(OrbitalError::Eval(EvalError::TypeMismatch {
            expected: declared_text(ty),
            actual: found.actual.to_string(),
        })),
    }
}

pub fn misfit_of_shape(value: &Value, declared: &DeclaredShape) -> Option<(String, &'static str)> {
    misfit_detail_of_shape(value, declared).map(|found| (found.path, found.actual))
}

/// [`misfit_of_shape`] with the declared type at the failing path.
pub fn misfit_detail_of_shape(value: &Value, declared: &DeclaredShape) -> Option<Misfit> {
    if declared.version.is_some_and(|version| version != 1) {
        return Some(Misfit::new(
            String::new(),
            "unsupported declared-shape version",
            &declared.ty,
        ));
    }
    first_misfit(value, &declared.ty, &declared.named)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::value::ValueMap;

    #[test]
    fn strict_record_contract_checks_null_presence_and_catchall_on_json_parse() {
        use crate::evaluator::context::BindingContext;
        use crate::evaluator::operators::{json::JsonParseOp, Operator};
        let context = BindingContext::new();
        let shape = serde_json::json!({"version":1,"type":{"kind":"struct","fields":[
            {"name":"id","fieldType":{"kind":"string"},"required":false},
            {"name":"token","fieldType":{"kind":"optional","inner":{"kind":"string"}},"required":true}
        ],"additional":{"kind":"number"}}});
        let declaration = SExpression::string(shape.to_string());
        for (value, accepted) in [
            (serde_json::json!({"token":null,"id":"","extra":2}), true),
            (serde_json::json!({"token":"ok"}), true),
            (serde_json::json!({}), false),
            (serde_json::json!({"token":false}), false),
            (serde_json::json!({"token":null,"id":null}), false),
            (serde_json::json!({"token":null,"extra":"bad"}), false),
            (serde_json::json!({"token":null,"extra":null}), false),
        ] {
            let parsed = JsonParseOp
                .evaluate(
                    &[SExpression::string(value.to_string()), declaration.clone()],
                    &context,
                )
                .unwrap();
            assert_eq!(!parsed.is_null(), accepted, "parse {value}: {parsed:?}");
        }
    }

    #[test]
    fn malformed_quoted_enum_is_not_a_nominal_type_in_a_strict_descriptor() {
        use crate::evaluator::context::BindingContext;
        use crate::evaluator::operators::{json::JsonParseOp, Operator};
        let context = BindingContext::new();
        for name in [r#""bad\q""#, r#""unterminated"#, r#""a" |"#] {
            let declaration = SExpression::string(
                serde_json::json!({"version":1,"type":{"kind":"enum","name":name}}).to_string(),
            );
            let parsed = JsonParseOp
                .evaluate(
                    &[SExpression::string("\"anything\""), declaration],
                    &context,
                )
                .unwrap();
            assert!(parsed.is_null(), "{name}: {parsed:?}");
        }
    }

    #[test]
    fn unknown_descriptor_version_cannot_fall_back_to_a_bare_type() {
        for wire in [
            serde_json::json!({"version":2,"kind":"string"}),
            serde_json::json!({"version":2,"type":{"kind":"string"},"kind":"string"}),
        ] {
            assert!(
                declared_shape(&SExpression::string(wire.to_string())).is_err(),
                "{wire}"
            );
        }
    }

    #[test]
    fn a_declared_shape_is_a_function_of_its_text_whichever_allocation_holds_it() {
        let text = serde_json::json!({"version":1,"type":{"kind":"string"}}).to_string();
        let first = SExpression::string(text.clone());
        let second = SExpression::string(text);
        let other = SExpression::string(serde_json::json!({"version":1,"type":{"kind":"number"}}).to_string());
        let a = declared_shape(&first).unwrap();
        let b = declared_shape(&second).unwrap();
        let again = declared_shape(&first).unwrap();
        assert!(std::rc::Rc::ptr_eq(&a, &b) && std::rc::Rc::ptr_eq(&a, &again));
        assert!(!std::rc::Rc::ptr_eq(&a, &declared_shape(&other).unwrap()), "control: other text, other shape");
    }

    fn num(n: f64) -> Value {
        Value::Number(n)
    }

    fn s(t: &str) -> Value {
        Value::string(t.to_string())
    }

    fn total_union(item: &str) -> BTreeMap<String, OirType> {
        let nullable = item == "V?";
        let item = |nullable: bool| {
            let v = OirType::TypeRef { name: "V".into() };
            if nullable {
                OirType::optional(v)
            } else {
                v
            }
        };
        let union = OirType::Union {
            variants: vec![
                orbital_core_base::ir::types::OirField::new("string", OirType::String),
                orbital_core_base::ir::types::OirField::new("number", OirType::Number),
                orbital_core_base::ir::types::OirField::new("boolean", OirType::Boolean),
                orbital_core_base::ir::types::OirField::new("list", OirType::array(item(nullable))),
                orbital_core_base::ir::types::OirField::new(
                    "map",
                    OirType::map(OirType::String, item(nullable)),
                ),
            ]
            .into(),
        };
        BTreeMap::from([("V".to_string(), union)])
    }

    fn v() -> OirType {
        OirType::TypeRef { name: "V".into() }
    }

    fn lambda() -> Value {
        Value::Lambda {
            params: Vec::new(),
            body: std::sync::Arc::new(SExpression::Null),
        }
    }

    #[test]
    fn a_json_total_union_admits_nested_data_and_reports_a_lambda_at_its_path() {
        let named = total_union("V?");
        let data: Value = serde_json::from_value(
            serde_json::json!({ "a": [1, "x", { "b": [true, null] }], "c": null }),
        )
        .unwrap();
        assert_eq!(
            misfit_of_declared(&data, &v(), &named),
            None,
            "nested JSON data fits"
        );
        let mut fields = ValueMap::new();
        fields.insert("f".to_string(), Value::array(vec![num(1.0), lambda()]));
        let bad = Value::object(fields);
        assert_eq!(
            misfit_of_declared(&bad, &v(), &named),
            Some((String::new(), "object")),
            "a lambda is not data: no member fits the record"
        );
    }

    #[test]
    fn a_non_nullable_item_null_misfits_and_a_nullable_one_admits_it() {
        let named = total_union("V");
        let data: Value = serde_json::from_value(serde_json::json!([1, null])).unwrap();
        assert!(
            first_misfit(&data, &v(), &named).is_some(),
            "the strict contract refuses a null item"
        );
        let nullable = total_union("V?");
        assert_eq!(
            first_misfit(&data, &v(), &nullable),
            None,
            "control: a nullable item admits it"
        );
    }

    #[test]
    fn a_misfit_names_the_type_declared_at_its_own_path() {
        use orbital_core_base::ir::types::OirField;
        let unit = || OirType::Struct {
            additional: None,
            fields: vec![OirField::new("content", OirType::String)].into(),
        };
        let named = BTreeMap::from([("Unit".to_string(), unit())]);
        let in_list = OirType::array(OirType::TypeRef {
            name: "Unit".into(),
        });
        let in_map = OirType::Map {
            key: Box::new(OirType::String),
            value: Box::new(OirType::Number),
        };
        let strict = |value: &Value, ty: &OirType| first_misfit(value, ty, &named);
        let null_content: Value =
            serde_json::from_value(serde_json::json!([{ "content": null }])).unwrap();
        let found = strict(&null_content, &in_list).unwrap();
        assert_eq!(
            (found.path.as_str(), found.actual, found.expected.as_str()),
            ("[0].content", "null", "string")
        );
        let bad_map: Value = serde_json::from_value(serde_json::json!({ "k": "x" })).unwrap();
        let found = strict(&bad_map, &in_map).unwrap();
        assert_eq!(
            (found.path.as_str(), found.expected.as_str()),
            (".k", "number")
        );
        let found = strict(&s("x"), &OirType::Number).unwrap();
        assert_eq!(
            (found.path.as_str(), found.expected.as_str()),
            ("", "number")
        );
        let fits: Value = serde_json::from_value(serde_json::json!([{ "content": "ok" }])).unwrap();
        assert_eq!(
            strict(&fits, &in_list),
            None,
            "control: a fitting value passes"
        );
    }

    #[test]
    fn a_field_without_bang_may_be_absent_but_null_needs_an_optional_type() {
        use orbital_core_base::ir::types::OirField;
        let mut absent_ok = OirField::new("role", OirType::String);
        absent_ok.required = false;
        let mut nullable = OirField::new(
            "permissions",
            OirType::optional(OirType::array(OirType::String)),
        );
        nullable.required = false;
        let required = OirField::new("id", OirType::String);
        let shape = OirType::Struct {
            additional: None,
            fields: vec![required, absent_ok, nullable].into(),
        };
        let named = BTreeMap::new();
        let check = |json: serde_json::Value| {
            let value: Value = serde_json::from_value(json).unwrap();
            first_misfit(&value, &shape, &named)
        };
        assert_eq!(check(serde_json::json!({"id": "u"})), None);
        assert_eq!(
            check(serde_json::json!({"id": "u", "role": "r", "permissions": ["a"]})),
            None
        );
        assert_eq!(
            check(serde_json::json!({"id": "u", "permissions": null})),
            None
        );
        let found = check(serde_json::json!({"id": "u", "role": null})).unwrap();
        assert_eq!((found.path.as_str(), found.actual), (".role", "null"));
        let found = check(serde_json::json!({"id": null})).unwrap();
        assert_eq!((found.path.as_str(), found.actual), (".id", "null"));
        let found = check(serde_json::json!({"id": "u", "permissions": "x"})).unwrap();
        assert_eq!(found.path.as_str(), ".permissions");
    }

    #[test]
    fn an_alias_cycle_resolves_to_nothing_without_looping() {
        let named = BTreeMap::from([
            ("A".to_string(), OirType::TypeRef { name: "B".into() }),
            ("B".to_string(), OirType::TypeRef { name: "A".into() }),
        ]);
        assert!(declared_alias(&OirType::TypeRef { name: "A".into() }, &named).is_none());
        let chain = BTreeMap::from([
            ("A".to_string(), OirType::TypeRef { name: "B".into() }),
            ("B".to_string(), OirType::Number),
        ]);
        assert_eq!(
            declared_alias(&OirType::TypeRef { name: "A".into() }, &chain),
            Some(&OirType::Number),
            "control: a chain resolves"
        );
    }

    #[test]
    fn declared_bytes_require_the_canonical_base64_wire_encoding() {
        let named = BTreeMap::new();
        for valid in ["", "AA==", "AP9h", "aGVsbG8="] {
            assert!(
                value_of_declared(&s(valid), &OirType::Bytes, &named).is_ok(),
                "{valid}"
            );
        }
        for invalid in ["plain text", "AA", "AA-_", "!!", "AB=="] {
            assert!(
                value_of_declared(&s(invalid), &OirType::Bytes, &named).is_err(),
                "{invalid}"
            );
            assert!(value_of_declared(&s(invalid), &OirType::String, &named).is_ok());
        }
        assert!(value_of_declared(&num(1.0), &OirType::Bytes, &named).is_err());
        let bytes = OirType::array(OirType::Bytes);
        assert!(value_of_declared(&Value::array(vec![s("AA==")]), &bytes, &named).is_ok());
        assert_eq!(
            misfit_of_declared(
                &Value::array(vec![s("AA=="), s("invalid bytes")]),
                &bytes,
                &named
            ),
            Some(("[1]".to_string(), "string"))
        );
    }
}
