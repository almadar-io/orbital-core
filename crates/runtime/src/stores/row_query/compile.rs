//! Compiling a filter or an `@read` policy into a row predicate.

use super::*;

// ----- compiling a filter or an `@read` policy -----------------------------------------------

/// Why an S-expression is not a [`RowPredicate`]: the kernel runs it through the interpreter instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotExpressible {
    /// An operator with no predicate form (a function call on a field, `array/some` with a lambda, …).
    Operator(String),
    /// A form of an expressible operator the compiler does not map (two fields compared, a row path, …).
    Shape(String),
    /// A binding that does not resolve for this dispatch (a filter only; a policy compiles to `const false`).
    Binding(String),
    /// The evaluator and a predicate read a missing or null value differently here (an ordering comparison
    /// on a field the entity does not declare required or default, with no presence guard in an enclosing `and`):
    /// the interpreter keeps the result.
    NullSemantics(String),
}

impl NotExpressible {
    /// The `reason` of `kernel.fetch.pushdown-fallback`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NullSemantics(_) => "null-semantics",
            _ => "not-expressible",
        }
    }
}

impl std::fmt::Display for NotExpressible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operator(op) => write!(f, "operator `{op}` has no predicate form"),
            Self::Shape(what) => write!(f, "{what}"),
            Self::Binding(binding) => write!(f, "binding `{binding}` does not resolve"),
            Self::NullSemantics(field) => write!(
                f,
                "an ordering comparison on `{field}`, which is not declared required and no enclosing `and` guards"
            ),
        }
    }
}

/// Which predicate is being compiled: a policy that cannot be specialised hides every row, a filter is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateRole {
    Policy,
    Filter,
}

pub(super) enum Fail {
    Not(NotExpressible),
    Unresolved(String),
}

pub(super) enum Term {
    Field(String),
    Const(Value),
}

pub(super) fn shape(what: &str) -> Fail {
    Fail::Not(NotExpressible::Shape(what.to_string()))
}

pub(super) fn row_path_free(field: &str) -> Result<String, Fail> {
    if field.contains('.') || field.contains('[') {
        Err(shape("a row path is not a flat field"))
    } else {
        Ok(field.to_string())
    }
}

pub(super) fn as_field(value: &Value) -> Result<Option<String>, Fail> {
    as_field_in(value, None)
}

/// The row field `value` names. With a context, `(object/get @entity <binding>)` names the flat field the binding
/// holds: the evaluator reads that one field on every row, so it is the same predicate as the literal.
pub(super) fn as_field_in(value: &Value, ctx: Option<&BindingContext>) -> Result<Option<String>, Fail> {
    match value {
        Value::String(s) if s == "@entity" => Err(shape("the whole row is not a field")),
        Value::String(s) => match s.strip_prefix("@entity.") {
            Some(rest) => row_path_free(rest).map(Some),
            None => Ok(None),
        },
        Value::Array(items)
            if items.first().and_then(Value::as_str) == Some("object/get")
                && items.get(1).and_then(Value::as_str) == Some("@entity") =>
        {
            match (items.len(), items.get(2).and_then(Value::as_str)) {
                (3, Some(field)) => match (field.starts_with('@'), ctx) {
                    (true, Some(ctx)) => match ctx.resolve_binding(field) {
                        Some(Value::String(name)) => row_path_free(&name).map(Some),
                        Some(_) => Err(shape("the row field name is not a string")),
                        None => Err(Fail::Unresolved(field.to_string())),
                    },
                    _ => row_path_free(field).map(Some),
                },
                _ => Err(shape("object/get on the row is not a single flat field")),
            }
        }
        _ => Ok(None),
    }
}

/// Two constants of one non-null scalar kind: where the row-side equality is plain equality.
pub(super) fn same_scalar_kind(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_)) | (Value::String(_), Value::String(_)) | (Value::Bool(_), Value::Bool(_))
    )
}

pub(super) fn scalar_const(value: Value) -> Result<Value, Fail> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(value),
        _ => Err(shape("the value is not a scalar")),
    }
}

pub(super) fn term(value: &Value, ctx: &BindingContext) -> Result<Term, Fail> {
    if let Some(field) = as_field_in(value, Some(ctx))? {
        return Ok(Term::Field(field));
    }
    match value {
        Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
            Some(resolved) => scalar_const(resolved).map(Term::Const),
            None => Err(Fail::Unresolved(s.clone().into_inner())),
        },
        Value::String(s) if s.contains('@') => Err(shape("a string with an embedded binding")),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            Ok(Term::Const(value.clone()))
        }
        Value::Array(items) => Err(Fail::Not(NotExpressible::Operator(
            items
                .first()
                .and_then(Value::as_str)
                .unwrap_or("[…]")
                .to_string(),
        ))),
        _ => Err(shape("a value with no predicate form")),
    }
}

pub(super) fn list_of(value: &Value, ctx: &BindingContext) -> Result<Vec<Value>, Fail> {
    let resolved = match value {
        Value::Array(items) => items.clone(),
        Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
            Some(Value::Array(items)) => items,
            Some(_) => return Err(shape("the list binding is not a list")),
            None => return Err(Fail::Unresolved(s.clone().into_inner())),
        },
        _ => return Err(shape("the list is not a list")),
    };
    resolved
        .iter()
        .map(|item| match item {
            Value::String(s) if s.starts_with('@') => match ctx.resolve_binding(s) {
                Some(v) => scalar_const(v),
                None => Err(Fail::Unresolved(s.clone().into_inner())),
            },
            Value::String(s) if s.contains('@') => Err(shape("a string with an embedded binding")),
            other => scalar_const(other.clone()),
        })
        .collect()
}

/// The evaluator fails on a missing or null operand of `array/includes` and `str/includes`, which drops the row:
/// that equals a failing leaf only where a failure makes the whole predicate false (a top-level `and` chain).
/// Under `or`/`not`, or anywhere the field may be missing, the interpreter keeps the result.
pub(super) fn erroring_leaf(field: &str, required: &dyn Fn(&str) -> bool, exact: bool) -> Result<(), Fail> {
    if exact || required(field) {
        Ok(())
    } else {
        Err(Fail::Not(NotExpressible::NullSemantics(field.to_string())))
    }
}

/// What the enclosing `and` chains establish: fields shown present, and fields ordered against a number (where
/// `validate/required` is a null check, since a number has no empty form).
#[derive(Clone, Default)]
pub(super) struct Scope {
    pub(super) guarded: std::collections::HashSet<String>,
    pub(super) numeric: std::collections::HashSet<String>,
}

/// The fields among the direct conjuncts that an ordering compares with a number constant.
pub(super) fn numeric_ordered(args: &[Value], ctx: &BindingContext) -> std::collections::HashSet<String> {
    let mut fields = std::collections::HashSet::new();
    for arg in args {
        let Value::Array(items) = arg else { continue };
        let head = items.first().and_then(Value::as_str).unwrap_or("");
        if items.len() != 3 || !matches!(head, "<" | "<=" | ">" | ">=") {
            continue;
        }
        match (term(&items[1], ctx), term(&items[2], ctx)) {
            (Ok(Term::Field(field)), Ok(Term::Const(Value::Number(_))))
            | (Ok(Term::Const(Value::Number(_))), Ok(Term::Field(field))) => {
                fields.insert(field);
            }
            _ => {}
        }
    }
    fields
}

/// The field a null comparison `(op f null)` names.
pub(super) fn null_compared_field(expr: &Value, op: &str) -> Option<String> {
    let Value::Array(items) = expr else {
        return None;
    };
    if items.len() != 3 || items[0].as_str() != Some(op) {
        return None;
    }
    let other = match (&items[1], &items[2]) {
        (left, Value::Null) => left,
        (Value::Null, right) => right,
        _ => return None,
    };
    as_field(other).ok().flatten()
}

/// The fields the direct conjuncts of one `and` show present: `(!= f null)`, `(not (= f null))`, and
/// `(validate/required f)` where `f` is ordered against a number in the chain.
pub(super) fn present_fields(
    args: &[Value],
    numeric: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut fields = std::collections::HashSet::new();
    for arg in args {
        let not_null = null_compared_field(arg, "!=").or_else(|| match arg {
            Value::Array(items) if items.len() == 2 && items[0].as_str() == Some("not") => {
                null_compared_field(&items[1], "=")
            }
            _ => None,
        });
        fields.extend(not_null);
        if let Value::Array(items) = arg {
            if items.len() == 2 && items[0].as_str() == Some("validate/required") {
                if let Ok(Some(field)) = as_field(&items[1]) {
                    if numeric.contains(&field) {
                        fields.insert(field);
                    }
                }
            }
        }
    }
    fields
}

pub(super) fn compare_leaf_of(
    field: String,
    op: RowCompareOp,
    value: Value,
    required: &dyn Fn(&str) -> bool,
    scope: &Scope,
) -> Result<RowPredicate, Fail> {
    let ordering = matches!(
        op,
        RowCompareOp::Lt | RowCompareOp::Le | RowCompareOp::Gt | RowCompareOp::Ge
    );
    if ordering && !required(&field) && !scope.guarded.contains(&field) {
        return Err(Fail::Not(NotExpressible::NullSemantics(field)));
    }
    Ok(match (&value, op) {
        (Value::Null, RowCompareOp::Eq) => RowPredicate::IsNull {
            field,
            is_null: true,
        },
        (Value::Null, RowCompareOp::Ne) => RowPredicate::IsNull {
            field,
            is_null: false,
        },
        _ => RowPredicate::Compare { field, op, value },
    })
}

pub(super) fn compare_op_of(head: &str) -> Option<RowCompareOp> {
    match head {
        "=" | "==" => Some(RowCompareOp::Eq),
        "!=" => Some(RowCompareOp::Ne),
        "<" => Some(RowCompareOp::Lt),
        "<=" => Some(RowCompareOp::Le),
        ">" => Some(RowCompareOp::Gt),
        ">=" => Some(RowCompareOp::Ge),
        _ => None,
    }
}

pub(super) fn compile_expr(
    expr: &Value,
    ctx: &BindingContext,
    required: &dyn Fn(&str) -> bool,
    exact: bool,
    scope: &Scope,
) -> Result<RowPredicate, Fail> {
    match expr {
        Value::Bool(value) => Ok(RowPredicate::Const { value: *value }),
        Value::Array(items) => {
            let head = items.first().and_then(Value::as_str).unwrap_or("");
            let args = items.get(1..).unwrap_or(&[]);
            match head {
                "and" | "or" => {
                    let exact = exact && head == "and";
                    let inner = if head == "and" {
                        let mut numeric = scope.numeric.clone();
                        numeric.extend(numeric_ordered(args, ctx));
                        let mut guarded = scope.guarded.clone();
                        guarded.extend(present_fields(args, &numeric));
                        Scope { guarded, numeric }
                    } else {
                        scope.clone()
                    };
                    // The evaluator stops at the first truthy operand of an or (falsy of an and), so an operand
                    // after a constant that decides the chain is never evaluated and need not compile.
                    let decides = head == "or";
                    let mut of = Vec::with_capacity(args.len());
                    for a in args {
                        let compiled = compile_expr(a, ctx, required, exact, &inner)?;
                        let stops = matches!(compiled, RowPredicate::Const { value } if value == decides);
                        of.push(compiled);
                        if stops {
                            return Ok(RowPredicate::Const { value: decides });
                        }
                    }
                    Ok(if head == "and" {
                        RowPredicate::All { of }
                    } else {
                        RowPredicate::Any { of }
                    })
                }
                "not" if args.len() == 1 => Ok(RowPredicate::Not {
                    of: Box::new(compile_expr(&args[0], ctx, required, false, scope)?),
                }),
                "validate/required" if args.len() == 1 => {
                    // Equal to "not null" only where the field is ordered against a number in the same chain: a
                    // string or a list is also "required" only when non-empty.
                    match as_field_in(&args[0], Some(ctx))? {
                        Some(field) if scope.numeric.contains(&field) => Ok(RowPredicate::IsNull {
                            field,
                            is_null: false,
                        }),
                        _ => Err(shape(
                            "validate/required is a null check only on a field ordered against a number",
                        )),
                    }
                }
                "array/includes" if args.len() == 2 => {
                    if let Some(field) = as_field_in(&args[1], Some(ctx))? {
                        erroring_leaf(&field, required, exact)?;
                        Ok(RowPredicate::Member {
                            field,
                            values: list_of(&args[0], ctx)?,
                        })
                    } else if let Some(field) = as_field_in(&args[0], Some(ctx))? {
                        erroring_leaf(&field, required, exact)?;
                        match term(&args[1], ctx)? {
                            Term::Const(value) => Ok(RowPredicate::HasElement { field, value }),
                            Term::Field(_) => Err(shape("two row fields")),
                        }
                    } else {
                        Err(shape("array/includes names no row field"))
                    }
                }
                "str/includes" if args.len() == 2 => {
                    let Term::Const(Value::String(text)) = term(&args[1], ctx)? else {
                        return Err(shape("str/includes needle is not a constant string"));
                    };
                    if let Some(field) = as_field_in(&args[0], Some(ctx))? {
                        erroring_leaf(&field, required, exact)?;
                        return Ok(RowPredicate::Contains {
                            field,
                            text: text.into_inner(),
                            case_sensitive: true,
                        });
                    }
                    if let Value::Array(lower) = &args[0] {
                        if lower.len() == 2 && lower[0].as_str() == Some("str/lower") {
                            if let Some(field) = as_field_in(&lower[1], Some(ctx))? {
                                erroring_leaf(&field, required, exact)?;
                                if text.to_lowercase() != text {
                                    return Err(shape(
                                        "case-insensitive needle is not already lower case",
                                    ));
                                }
                                return Ok(RowPredicate::Contains {
                                    field,
                                    text: text.into_inner(),
                                    case_sensitive: false,
                                });
                            }
                        }
                    }
                    Err(shape("str/includes haystack is not a row field"))
                }
                other => match compare_op_of(other) {
                    Some(op) if args.len() == 2 => {
                        match (term(&args[0], ctx)?, term(&args[1], ctx)?) {
                            (Term::Field(field), Term::Const(value)) => {
                                compare_leaf_of(field, op, value, required, scope)
                            }
                            (Term::Const(value), Term::Field(field)) => {
                                compare_leaf_of(field, op.flipped(), value, required, scope)
                            }
                            (Term::Const(left), Term::Const(right))
                                if matches!(op, RowCompareOp::Eq | RowCompareOp::Ne)
                                    && same_scalar_kind(&left, &right) =>
                            {
                                let equal = same_value(&left, &right);
                                Ok(RowPredicate::Const {
                                    value: equal == (op == RowCompareOp::Eq),
                                })
                            }
                            _ => Err(shape("a comparison needs one row field and one constant")),
                        }
                    }
                    _ => Err(Fail::Not(NotExpressible::Operator(other.to_string()))),
                },
            }
        }
        _ => Err(shape("not a predicate form")),
    }
}

/// Compile an S-expression (as a `Value`) over `@entity` fields and literals into a [`RowPredicate`].
/// `@user`, `@payload`, `@config`, `@now` and the like are specialised to their value in `ctx`. A policy
/// that cannot be specialised compiles to `const false` (a row whose predicate fails is hidden); a filter
/// that cannot is [`NotExpressible::Binding`]. The compiler never drops a conjunct.
///
/// The evaluator reads a missing or null value as below a number while a predicate fails the comparison.
/// `orb validate` rejects an ordering over a field that can be missing unless the program guards it (ORB_X_ORDERING_OPTIONAL_OPERAND),
/// so an ordering comparison compiles on a field `required` says the entity declares required or defaulted, or when
/// a direct conjunct of an enclosing `and` is a presence guard on it: a null field then makes the whole
/// conjunction false in both worlds.
pub fn compile_predicate(
    expr: &Value,
    ctx: &BindingContext,
    role: PredicateRole,
    required: &dyn Fn(&str) -> bool,
) -> Result<RowPredicate, NotExpressible> {
    match compile_expr(expr, ctx, required, true, &Scope::default()) {
        Ok(predicate) => Ok(predicate),
        Err(Fail::Not(why)) => Err(why),
        Err(Fail::Unresolved(binding)) => match role {
            PredicateRole::Policy => Ok(RowPredicate::Const { value: false }),
            PredicateRole::Filter => Err(NotExpressible::Binding(binding)),
        },
    }
}

/// A fetch `filter` in the object form `{ field: value, ... }` as the equality conjunction it means,
/// `(and (= row.field value) ...)` (one field is its equality alone: `and` takes two or more); `{}` is the empty
/// conjunction, `true`. Any other form is returned as is.
pub fn lower_object_filter(filter: &Value) -> std::borrow::Cow<'_, Value> {
    use std::borrow::Cow;
    let Value::Object(map) = filter else {
        return Cow::Borrowed(filter);
    };
    if map.is_empty() {
        return Cow::Owned(Value::Bool(true));
    }
    let mut conjunction = vec![Value::string("and")];
    for (field, value) in map {
        conjunction.push(Value::array(vec![
            Value::String(("=".to_string()).into()),
            Value::Array(vec![
                Value::String(("object/get".to_string()).into()),
                Value::String(("@entity".to_string()).into()),
                Value::String(field.clone().into()),
            ].into()),
            value.clone(),
        ]));
    }
    if conjunction.len() == 2 {
        return Cow::Owned(conjunction.pop().expect("one equality"));
    }
    Cow::Owned(Value::array(conjunction))
}

/// The `RowOrder` of a fetch's `(field, descending)` spec.
pub fn order_of(field: &str, descending: bool) -> Result<RowOrder, NotExpressible> {
    if field.contains('.') || field.contains('[') {
        return Err(NotExpressible::Shape(
            "an order field that is a path".to_string(),
        ));
    }
    Ok(RowOrder {
        field: field.to_string(),
        direction: if descending {
            RowDirection::Desc
        } else {
            RowDirection::Asc
        },
    })
}
