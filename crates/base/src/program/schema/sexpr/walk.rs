//! Walking a value expression: operator returns, bindings and value slots.

use super::*;

/// What the operator registry declares about one operator's result — the only
/// facts the value-source walk reads. The registry lives downstream of this
/// crate, so callers supply the lookup; nothing here names an operator.
#[derive(Debug, Clone, Copy)]
pub struct OperatorReturn {
    /// The declared `returnSemantics` kind, when the operator has one.
    pub semantics: Option<&'static str>,
    /// The operator's declared `returnType` is `function` (a lambda form).
    pub returns_function: bool,
}

/// Registry lookup: operator name to its declared result facts.
pub type OperatorLookup<'r> = &'r dyn Fn(&str) -> Option<OperatorReturn>;

/// Parameters of one value-source walk ([`SExpression::value_leaves`]).
///
/// The walk is driven entirely by declared `returnSemantics` (documented in
/// `@almadar/std` `types.ts`):
/// - `identity-of-arg<N>` recurses into argument N;
/// - `branch-union` unions every argument after the condition;
/// - `union-of-args` unions every argument, `union-of-args<i,j>` the listed ones;
/// - `last-of-args` recurses into the last argument;
/// - `lambda-result` recurses into the body of the lambda argument;
/// - anything else, including an operator with no declaration, is an opaque
///   leaf (itself): nothing is credited, nothing is guessed.
///
/// Arity is the validator's concern, not the walk's: a declared kind naming an
/// argument that is absent makes the call opaque.
#[derive(Clone, Copy)]
pub struct ValueWalk<'r> {
    pub lookup: OperatorLookup<'r>,
    /// A `branch-union` whose condition is statically false contributes
    /// nothing from its first branch (`SExpression` walks only).
    pub skip_dead_branches: bool,
}

/// The ordered initializer/body slots evaluated by the canonical `let` form. A binding is `[name, init]` or
/// `[name, init, shape]`: the optional third item is the local's declared type as shape text, static only (the
/// kernel never reads it).
#[derive(Debug)]
pub struct SequentialBindings<'a> {
    pub bindings: Vec<(
        &'a crate::value::Shared<String>,
        &'a SExpression,
        Option<&'a str>,
    )>,
    pub body: &'a SExpression,
}

impl<'a> SequentialBindings<'a> {
    pub fn from_args(args: &'a [SExpression]) -> Option<Self> {
        let bindings = args.first()?.as_array()?;
        let body = args.get(1)?;
        Some(Self {
            bindings: bindings
                .iter()
                .map(|binding| match binding.as_array()?.as_slice() {
                    [SExpression::String(name), initializer] => Some((name, initializer, None)),
                    [SExpression::String(name), initializer, SExpression::String(shape)] => {
                        Some((name, initializer, Some(shape.as_str())))
                    }
                    _ => None,
                })
                .collect::<Option<_>>()?,
            body,
        })
    }
}

/// One argument slot a declared kind resolves through.
#[derive(Debug, Clone, Copy)]
pub enum ValueSlot {
    Arg(usize),
    LambdaBody(usize),
}

impl ValueSlot {
    pub(super) fn arg(self) -> usize {
        match self {
            ValueSlot::Arg(i) | ValueSlot::LambdaBody(i) => i,
        }
    }
}

/// The single interpreter of `returnSemantics` provenance kinds: which
/// argument slots of the call `op(args…)` its result draws from, or `None`
/// when the operator is undeclared, declares a non-provenance kind, or the
/// kind names an absent argument (opaque).
pub(super) fn plan_value_sources(
    walk: &ValueWalk<'_>,
    op: &str,
    arg_count: usize,
    is_lambda: impl Fn(usize) -> bool,
    condition_dead: bool,
) -> Option<Vec<ValueSlot>> {
    let sem = (walk.lookup)(op)?.semantics?;
    let slots: Vec<ValueSlot> = match sem {
        "branch-union" => {
            if arg_count < 2 {
                return None;
            }
            let first_branch = if walk.skip_dead_branches && condition_dead {
                2
            } else {
                1
            };
            (first_branch..arg_count).map(ValueSlot::Arg).collect()
        }
        "union-of-args" | "coalesce" => (0..arg_count).map(ValueSlot::Arg).collect(),
        "last-of-args" => arg_count
            .checked_sub(1)
            .map(ValueSlot::Arg)
            .into_iter()
            .collect(),
        "lambda-result" => (0..arg_count)
            .rev()
            .find(|&i| is_lambda(i))
            .map(ValueSlot::LambdaBody)
            .into_iter()
            .collect(),
        _ => {
            let indices = sem
                .strip_prefix("identity-of-arg<")
                .or_else(|| sem.strip_prefix("union-of-args<"))
                .and_then(|rest| rest.strip_suffix('>'))?;
            let indices: Vec<usize> = indices
                .split(',')
                .map(|n| n.trim().parse().ok())
                .collect::<Option<_>>()?;
            if indices.iter().any(|&i| i >= arg_count) {
                return None;
            }
            indices.into_iter().map(ValueSlot::Arg).collect()
        }
    };
    (!slots.is_empty() || sem == "branch-union").then_some(slots)
}

/// Borrowed view of a recognized FC-5 dynamic-children tuple
/// (`["array/map", collection, ["fn", param, item_node]]`). Produced by
/// [`SExpression::as_mapped_children`].
#[derive(Debug)]
pub struct MappedChildren<'a> {
    /// The collection expression iterated over (a binding or operator call).
    pub collection: &'a SExpression,
    /// The lambda parameter name(s) bound over each element (e.g. `["item"]`).
    pub params: Vec<&'a str>,
    /// The per-item render node (a RenderUINode object) produced for each element.
    pub item_node: &'a SExpression,
}

/// Borrowed view of a recognized conditional render node
/// (`["if", cond, then, else?]`). Produced by
/// [`SExpression::as_conditional_node`]. The condition is deliberately not
/// exposed: it is a value expression, validated by the binding pass, never a
/// pattern.
#[derive(Debug)]
pub struct ConditionalNode<'a> {
    /// The render node produced when the condition is truthy.
    pub then_node: &'a SExpression,
    /// The render node produced otherwise; `None` for the 3-element form.
    pub else_node: Option<&'a SExpression>,
}
