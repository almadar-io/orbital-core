//! `tree/fold`: the one structural fold over a value of a recursive type.
//!
//! `(tree/fold value step Shape definition)`. `Shape` names the value's recursive type; `definition` is that type's
//! `.orb` field definition as JSON text (the form lowering writes for a field of the type: a struct's `properties`, or
//! a tagged union's `values` + `properties`), in which every recursive back-reference is the by-name reference
//! `{type: "union", values: [Shape]}`. `definition` is written by the compiler from the declared type, never by hand.
//!
//! The children of a node are the fields whose DECLARED type is `Shape` (the child value), `[Shape]` (a list) or
//! `Map K Shape` (a map), read from the definition and never from the value's shape or names. A union node is
//! read through the one variant it belongs to: the variant whose single-valued tags it all carries, else the one
//! untagged variant whose closed shape it fits (every key declared; on a tie, every required member present);
//! none or several is an error, never a guess.
//!
//! The fold is bottom-up: `step` is called with `(node childResults)`, where `childResults` holds one entry per
//! child field present on the node, mirroring its position (field → the child's result, list → the list of
//! results, map → the map of results). Every call works on a strict subterm of a finite value, so it terminates.

use super::macros::*;
use super::*;
use orbital_core_base::schema::types::{FieldDefinition, FieldType, TagReading, UnionVariantChoice};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    static DEFINITIONS: RefCell<HashMap<String, Rc<FieldDefinition>>> = RefCell::new(HashMap::new());
}

fn bad(message: impl std::fmt::Display) -> OrbitalError {
    OrbitalError::Eval(EvalError::InvalidArgument(format!("tree/fold: {message}")))
}

fn definition_of(text: &str) -> Result<Rc<FieldDefinition>, OrbitalError> {
    if let Some(known) = DEFINITIONS.with(|d| d.borrow().get(text).cloned()) {
        return Ok(known);
    }
    let parsed: FieldDefinition = serde_json::from_str(text)
        .map_err(|e| bad(format!("the definition is not a field definition: {e}")))?;
    let parsed = Rc::new(parsed);
    DEFINITIONS.with(|d| d.borrow_mut().insert(text.to_string(), parsed.clone()));
    Ok(parsed)
}

/// Where a declared field holds children of the recursive type `shape`.
enum ChildPosition {
    Field,
    List,
    Map,
}

fn child_position(def: &FieldDefinition, shape: &str) -> Option<ChildPosition> {
    if def.by_name_reference() == Some(shape) {
        return Some(ChildPosition::Field);
    }
    let element = def.items.as_deref()?;
    if element.by_name_reference() != Some(shape) {
        return None;
    }
    match def.field_type {
        FieldType::Array => Some(ChildPosition::List),
        FieldType::Object if def.properties.is_none() => Some(ChildPosition::Map),
        _ => None,
    }
}

/// The variant of the tagged union `def` that `node` belongs to (the LOLO tagged-union rule).
fn select_variant<'d>(
    def: &'d FieldDefinition,
    node: &ValueMap,
) -> Result<&'d FieldDefinition, OrbitalError> {
    let keys: Vec<&str> = node.keys().map(String::as_str).collect();
    let choice = def.select_union_variant(&keys, |field, tag| match node.get(field) {
        Some(Value::String(s)) if s == tag => TagReading::Equal,
        _ => TagReading::Differs,
    });
    match choice {
        UnionVariantChoice::Matched(one) => Ok(one),
        UnionVariantChoice::NoMatch | UnionVariantChoice::Deferred => {
            Err(bad("the value belongs to no variant of its union"))
        }
        UnionVariantChoice::Ambiguous(names) => Err(bad(format!(
            "the value fits several variants equally: {}",
            names.join(", ")
        ))),
        UnionVariantChoice::Malformed(reason) => Err(bad(reason)),
    }
}

/// The declared fields of `node` under the root definition `def`.
fn fields_of<'d>(
    def: &'d FieldDefinition,
    node: &ValueMap,
) -> Result<&'d HashMap<String, FieldDefinition>, OrbitalError> {
    let declared = match def.field_type {
        FieldType::Union => select_variant(def, node)?,
        FieldType::Object => def,
        _ => return Err(bad("the definition is not a struct or a tagged union")),
    };
    declared
        .properties
        .as_ref()
        .ok_or_else(|| bad("the definition declares no fields"))
}

struct Fold<'a> {
    shape: &'a str,
    def: &'a FieldDefinition,
    params: &'a [String],
    body: &'a SExpression,
    ctx: &'a BindingContext,
}

impl Fold<'_> {
    fn node(&self, value: &Value) -> EvalResult {
        let node = match value {
            Value::Null => return Ok(Value::Null),
            Value::Object(node) => node,
            other => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: self.shape.to_string(),
                    actual: other.type_name().to_string(),
                }))
            }
        };
        let fields = fields_of(self.def, node)?;
        let mut children = ValueMap::new();
        for (key, child) in node {
            let Some(position) = fields.get(key).and_then(|d| child_position(d, self.shape)) else {
                continue;
            };
            let result = match (position, child) {
                (_, Value::Null) => continue,
                (ChildPosition::Field, child) => self.node(child)?,
                (ChildPosition::List, Value::Array(items)) => Value::Array(
                    items
                        .iter()
                        .map(|item| self.node(item))
                        .collect::<Result<_, _>>()?,
                ),
                (ChildPosition::Map, Value::Object(entries)) => Value::Object(
                    entries
                        .iter()
                        .map(|(k, item)| Ok((k.clone(), self.node(item)?)))
                        .collect::<Result<ValueMap, OrbitalError>>()?,
                ),
                (ChildPosition::List, other) => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: format!("[{}]", self.shape),
                        actual: other.type_name().to_string(),
                    }))
                }
                (ChildPosition::Map, other) => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: format!("Map string {}", self.shape),
                        actual: other.type_name().to_string(),
                    }))
                }
            };
            children.insert(key.clone(), result);
        }
        let mut step_ctx = self.ctx.child();
        step_ctx.set_local(&self.params[0], value.clone());
        step_ctx.set_local(&self.params[1], Value::Object(children));
        eval_expr(self.body, &step_ctx)
    }
}

/// tree/fold - `(tree/fold value (fn (node childResults) …) Shape definition)`: the structural bottom-up fold.
pub struct TreeFoldOp;
impl Operator for TreeFoldOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "tree/fold", >= 3);
        let SExpression::String(shape) = &args[2] else {
            return Err(bad("the third argument names the value's type"));
        };
        let Some(SExpression::String(text)) = args.get(3) else {
            return Err(bad(format!(
                "the declaration of {shape} was not written into the call"
            )));
        };
        let def = definition_of(text)?;
        let value = eval_expr(&args[0], ctx)?;
        let Value::Lambda { params, body } = eval_expr(&args[1], ctx)? else {
            return Err(bad("the step is a (fn (node childResults) …)"));
        };
        if params.len() != 2 {
            return Err(bad("the step takes exactly (node childResults)"));
        }
        Fold {
            shape,
            def: &*def,
            params: &params,
            body: &*body,
            ctx,
        }
        .node(&value)
    }
}
