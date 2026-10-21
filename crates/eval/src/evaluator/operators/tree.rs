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
//! untagged variant whose closed shape it fits (every key declared; on a tie, every required member present),
//! else the one `Map K V` variant (`type IrExpr = … | Map string IrExpr`), whose values are the node's children
//! when `V` is `Shape`; none or several is an error, never a guess.
//!
//! A node that is not an object (a list, a string, a number, a boolean) is read through the one variant of a union
//! declared with that kind (`type Expr = string | number | boolean | [Expr]`), or through the definition itself when
//! it is not a union. A list declared `[Shape]` has every element as a child.
//!
//! The fold is bottom-up: `step` is called with `(node childResults)`, where `childResults` holds one entry per
//! child field present on the node, mirroring its position (field → the child's result, list → the list of
//! results, map → the map of results); a list node's `childResults` is the list of its elements' results, a map
//! node's is the map of its values' results, and a node with no children gets `{}`. A null node folds to null without a step. Every call works on a strict subterm
//! of a finite value, so it terminates, and the walk keeps its own stack so a deep value cannot overflow the
//! thread's (G-ORB-433).

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
        UnionVariantChoice::NoMatch | UnionVariantChoice::Deferred => map_variant(def),
        UnionVariantChoice::Ambiguous(names) => Err(bad(format!(
            "the value fits several variants equally: {}",
            names.join(", ")
        ))),
        UnionVariantChoice::Malformed(reason) => Err(bad(reason)),
    }
}

fn is_map(def: &FieldDefinition) -> bool {
    def.field_type == FieldType::Object && def.properties.is_none() && def.items.is_some()
}

/// The one `Map K V` variant of the union `def`: an object no tag or closed shape claims belongs to it.
fn map_variant(def: &FieldDefinition) -> Result<&FieldDefinition, OrbitalError> {
    let (Some(names), Some(variants)) = (&def.values, &def.properties) else {
        return Err(bad("a union definition declares its variants (values + properties)"));
    };
    let maps: Vec<(&String, &FieldDefinition)> = names
        .iter()
        .filter_map(|name| variants.get(name).map(|v| (name, v)))
        .filter(|(_, v)| is_map(v))
        .collect();
    match maps.as_slice() {
        [(_, one)] => Ok(one),
        [] => Err(bad("the value belongs to no variant of its union")),
        several => Err(bad(format!(
            "the value fits several variants equally: {}",
            several.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// The variant of the union `def` declared with the kind of the non-object `value`; exactly one, never a guess.
fn kind_variant<'d>(
    def: &'d FieldDefinition,
    shape: &str,
    value: &Value,
) -> Result<&'d FieldDefinition, OrbitalError> {
    let (Some(names), Some(variants)) = (&def.values, &def.properties) else {
        return Err(bad("a union definition declares its variants (values + properties)"));
    };
    let matching: Vec<(&String, &FieldDefinition)> = names
        .iter()
        .filter_map(|name| variants.get(name).map(|v| (name, v)))
        .filter(|(_, v)| match (value, &v.field_type) {
            (Value::Array(_), FieldType::Array) => true,
            (Value::Number(_), FieldType::Number) => true,
            (Value::Bool(_), FieldType::Boolean) => true,
            (Value::String(s), FieldType::String) => v.values.as_ref().is_none_or(|allowed| allowed.contains(s)),
            _ => false,
        })
        .collect();
    match matching.as_slice() {
        [(_, one)] => Ok(one),
        [] => Err(OrbitalError::Eval(EvalError::TypeMismatch {
            expected: shape.to_string(),
            actual: value.type_name().to_string(),
        })),
        several => Err(bad(format!(
            "the value fits several variants equally: {}",
            several.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// How a node's children are laid out, in the order they are folded.
enum Plan {
    /// One entry per child field: the key and where it sits (`Map` carries the map's keys in order).
    Fields(Vec<(String, Slot)>),
    /// A list node whose elements are all children.
    List,
    /// A map node whose values are all children, in key order.
    Map(Vec<String>),
    Leaf,
}

enum Slot {
    Field,
    List(usize),
    Map(Vec<String>),
}

/// A node being folded: its value, its plan, the children still to fold, and the results so far.
struct Frame<'v> {
    value: &'v Value,
    plan: Plan,
    children: Vec<&'v Value>,
    results: Vec<Value>,
}

struct Fold<'a> {
    shape: &'a str,
    def: &'a FieldDefinition,
    params: &'a [String],
    body: &'a SExpression,
    ctx: &'a BindingContext,
}

impl Fold<'_> {
    fn mismatch(&self, expected: String, actual: &Value) -> OrbitalError {
        OrbitalError::Eval(EvalError::TypeMismatch {
            expected,
            actual: actual.type_name().to_string(),
        })
    }

    /// The node's plan and its children, read from its declaration, never from its shape or names.
    fn expand<'v>(&self, value: &'v Value) -> Result<Frame<'v>, OrbitalError> {
        let declared = match (value, &self.def.field_type) {
            (Value::Object(node), FieldType::Union) => select_variant(self.def, node)?,
            (Value::Object(_), FieldType::Object) => self.def,
            (Value::Object(_), _) => return Err(bad("the definition is not a struct or a tagged union")),
            (_, FieldType::Union) => kind_variant(self.def, self.shape, value)?,
            (Value::Array(_), FieldType::Array) => self.def,
            (other, _) => return Err(self.mismatch(self.shape.to_string(), other)),
        };
        let mut children = Vec::new();
        let plan = match value {
            Value::Array(items) => {
                if declared.items.as_deref().and_then(|i| i.by_name_reference()) == Some(self.shape) {
                    children.extend(items.iter());
                    Plan::List
                } else {
                    Plan::Leaf
                }
            }
            Value::Object(node) if is_map(declared) => {
                if declared.items.as_deref().and_then(|i| i.by_name_reference()) == Some(self.shape) {
                    children.extend(node.values());
                    Plan::Map(node.keys().cloned().collect())
                } else {
                    Plan::Leaf
                }
            }
            Value::Object(node) => {
                let fields = declared
                    .properties
                    .as_ref()
                    .ok_or_else(|| bad("the definition declares no fields"))?;
                let mut slots = Vec::new();
                for (key, child) in node {
                    let Some(position) = fields.get(key).and_then(|d| child_position(d, self.shape)) else {
                        continue;
                    };
                    let slot = match (position, child) {
                        (_, Value::Null) => continue,
                        (ChildPosition::Field, child) => {
                            children.push(child);
                            Slot::Field
                        }
                        (ChildPosition::List, Value::Array(items)) => {
                            children.extend(items.iter());
                            Slot::List(items.len())
                        }
                        (ChildPosition::Map, Value::Object(entries)) => {
                            children.extend(entries.values());
                            Slot::Map(entries.keys().cloned().collect())
                        }
                        (ChildPosition::List, other) => return Err(self.mismatch(format!("[{}]", self.shape), other)),
                        (ChildPosition::Map, other) => {
                            return Err(self.mismatch(format!("Map string {}", self.shape), other))
                        }
                    };
                    slots.push((key.clone(), slot));
                }
                Plan::Fields(slots)
            }
            _ => Plan::Leaf,
        };
        Ok(Frame { value, plan, children, results: Vec::new() })
    }

    fn step(&self, frame: Frame<'_>) -> EvalResult {
        let mut results = frame.results.into_iter();
        let child_results = match frame.plan {
            Plan::Leaf => Value::Object(ValueMap::new()),
            Plan::List => Value::Array(results.collect()),
            Plan::Map(keys) => Value::Object(keys.into_iter().zip(results).collect()),
            Plan::Fields(slots) => {
                let mut out = ValueMap::new();
                for (key, slot) in slots {
                    let result = match slot {
                        Slot::Field => results.next().unwrap_or(Value::Null),
                        Slot::List(n) => Value::Array(results.by_ref().take(n).collect()),
                        Slot::Map(keys) => Value::Object(keys.into_iter().zip(results.by_ref()).collect()),
                    };
                    out.insert(key, result);
                }
                Value::Object(out)
            }
        };
        let mut step_ctx = self.ctx.child();
        step_ctx.set_local(&self.params[0], frame.value.clone());
        step_ctx.set_local(&self.params[1], child_results);
        eval_expr(self.body, &step_ctx)
    }

    fn run(&self, root: &Value) -> EvalResult {
        if root.is_null() {
            return Ok(Value::Null);
        }
        let mut stack = vec![self.expand(root)?];
        loop {
            let top = stack.last_mut().expect("the walk holds the node in progress");
            if let Some(&child) = top.children.get(top.results.len()) {
                if child.is_null() {
                    top.results.push(Value::Null);
                } else {
                    let frame = self.expand(child)?;
                    stack.push(frame);
                }
                continue;
            }
            let done = stack.pop().expect("the walk holds the node in progress");
            let result = self.step(done)?;
            match stack.last_mut() {
                Some(parent) => parent.results.push(result),
                None => return Ok(result),
            }
        }
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
        .run(&value)
    }
}
