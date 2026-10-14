//! S-Expression type for guards and effects
//!
//! S-expressions are the core data structure for expressing:
//! - Guards (conditional expressions)
//! - Effects (side effects to execute)
//! - Computed values
//!
//! Examples:
//! - `["and", ["!=", "@payload.email", ""], [">=", "@payload.age", 18]]`
//! - `["persist", "create", "User", "@payload.data"]`
//! - `["render-ui", "main", { "type": "page-header", "title": "Hello" }]`

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// An S-expression object: keys in authored order, so lowering and codegen are deterministic.
pub type SObject = indexmap::IndexMap<String, SExpression>;

/// S-Expression - the fundamental unit of Orbital logic
///
/// An S-expression can be:
/// - A primitive value (null, bool, number, string)
/// - An array (function call or list)
/// - An object (key-value map)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
#[derive(Default)]
pub enum SExpression {
    /// Null value
    #[default]
    Null,
    /// Boolean value
    Bool(bool),
    /// Numeric value (integers and floats)
    Number(f64),
    /// String value (may contain @bindings)
    String(String),
    /// Array - either a function call `[op, ...args]` or a list
    Array(Vec<SExpression>),
    /// Object - key-value map
    Object(SObject),
}

impl SExpression {
    /// Check if this is a null value
    pub fn is_null(&self) -> bool {
        matches!(self, SExpression::Null)
    }

    /// Serialize to a `serde_json::Value` with every object's keys sorted.
    ///
    /// `Object` is backed by a `HashMap`, whose iteration order is randomly
    /// seeded per process — so serializing an expression containing an object
    /// literal yields a different byte sequence on each run. Any emitter that
    /// bakes an expression into generated source needs byte-stable output:
    /// otherwise identical input produces a different file every build, which
    /// churns diffs and thrashes the L1 factory signature cache.
    ///
    /// Lives here rather than in a backend because every shell (TypeScript,
    /// Python, Rust, Android, iOS) that inlines a policy or guard needs the
    /// same guarantee.
    pub fn to_canonical_json(&self) -> serde_json::Value {
        match self {
            SExpression::Null => serde_json::Value::Null,
            SExpression::Bool(b) => serde_json::Value::Bool(*b),
            SExpression::Number(n) => serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            SExpression::String(s) => serde_json::Value::String(s.clone()),
            SExpression::Array(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_canonical_json).collect())
            }
            SExpression::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for key in keys {
                    out.insert(key.clone(), map[key].to_canonical_json());
                }
                serde_json::Value::Object(out)
            }
        }
    }

    /// Canonicalize the flat lambda form `["fn", p1, p2, …, body]` into the
    /// grouped form `["fn", [p1, p2, …], body]` throughout this expression tree.
    /// Single-param (`["fn","p",body]`) and already-grouped forms are left as-is.
    /// Idempotent. Removing the flat shape gives every downstream consumer
    /// (the arity validator, both codegen backends, the JS-runtime parsers) one
    /// canonical multi-param `fn` shape instead of three the sites disagree on.
    pub fn normalize_fn_shapes(&mut self) {
        match self {
            SExpression::Array(items) => {
                for child in items.iter_mut() {
                    child.normalize_fn_shapes();
                }
                // Flat form has op + ≥2 string params + a trailing body (len > 3),
                // every element between op and body being a bare param name.
                let is_flat_fn = items.len() > 3
                    && matches!(items.first(), Some(SExpression::String(op)) if op == "fn" || op == "lambda")
                    && items[1..items.len() - 1]
                        .iter()
                        .all(|p| matches!(p, SExpression::String(_)));
                if is_flat_fn {
                    let body = items.pop().expect("len > 3 guarantees a body");
                    let params: Vec<SExpression> = items.drain(1..).collect();
                    items.push(SExpression::Array(params));
                    items.push(body);
                }
            }
            SExpression::Object(map) => {
                for v in map.values_mut() {
                    v.normalize_fn_shapes();
                }
            }
            _ => {}
        }
    }

    /// Try to get as a boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SExpression::Bool(b) => Some(*b),
            SExpression::Null => Some(false),
            SExpression::Number(n) => Some(*n != 0.0),
            SExpression::String(s) => Some(!s.is_empty()),
            SExpression::Array(a) => Some(!a.is_empty()),
            SExpression::Object(o) => Some(!o.is_empty()),
        }
    }

    /// Try to get as a number
    pub fn as_number(&self) -> Option<f64> {
        match self {
            SExpression::Number(n) => Some(*n),
            SExpression::Bool(true) => Some(1.0),
            SExpression::Bool(false) => Some(0.0),
            SExpression::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try to get as a string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SExpression::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as an array
    pub fn as_array(&self) -> Option<&Vec<SExpression>> {
        match self {
            SExpression::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Try to get as an object
    pub fn as_object(&self) -> Option<&crate::schema::SObject> {
        match self {
            SExpression::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Check if this is a binding reference (starts with @)
    pub fn is_binding(&self) -> bool {
        matches!(self, SExpression::String(s) if s.starts_with('@'))
    }

    /// Recognize the FC-5 dynamic-children tuple kept verbatim as IR:
    /// `["array/map", <collectionExpr>, ["fn", "<param>", <RenderUINode>]]`
    /// (or the grouped `["fn", ["<param>", …], <node>]` lambda form). This is
    /// the ONE mechanism for `children:`-position collection rendering — the
    /// existing renderItem lambda (`["fn", …]` + `@item` + splice) reused, no
    /// new node kind. Shared by the compiler's `@item`-scope validator and the
    /// codegen backends so they agree on the shape. Returns a borrowed view of
    /// the collection expression, the lambda parameter names, and the per-item
    /// render node; `None` for any other array shape.
    pub fn as_mapped_children(&self) -> Option<MappedChildren<'_>> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        if arr.len() != 3 || arr[0].as_str() != Some("array/map") {
            return None;
        }
        let SExpression::Array(lambda) = &arr[2] else {
            return None;
        };
        if lambda.len() != 3 {
            return None;
        }
        match lambda[0].as_str() {
            Some("fn") | Some("lambda") => {}
            _ => return None,
        }
        let params: Vec<&str> = match &lambda[1] {
            SExpression::String(p) => vec![p.as_str()],
            SExpression::Array(ps) => ps.iter().filter_map(|p| p.as_str()).collect(),
            _ => return None,
        };
        if params.is_empty() {
            return None;
        }
        Some(MappedChildren {
            collection: &arr[1],
            params,
            item_node: &lambda[2],
        })
    }

    /// Recognize a conditional render node kept verbatim as IR:
    /// `["if", <cond>, <then>, <else>?]`, matching the runtime kernel's own
    /// arity (`kernel.rs` `"if"` arm — the else arm is optional). Returns the
    /// branches that are themselves render nodes; the condition is a value
    /// expression checked by the binding pass, never a pattern.
    ///
    /// Exists because a conditional is a legal `children:` entry — `std-browse`
    /// has shipped `(if @config.bodySearch <toolbar> <empty>)` as `children[0]`
    /// of its dense/feed/gallery/triage bodies for months and it renders — but
    /// `validate_pattern` reached the tuple through its catch-all arm and
    /// rejected it as `ORB_RUI_INVALID_PATTERN`. The bodies above only escaped
    /// because they are referenced from inside an `(if)` arm and never walked;
    /// a body referenced directly by a guarded transition (master-detail's
    /// VIEW/CLOSE) is walked, and tripped. Same shape as `as_mapped_children`:
    /// recognize the tuple, validate the render-bearing parts, keep the IR.
    pub fn as_conditional_node(&self) -> Option<ConditionalNode<'_>> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        if !(arr.len() == 3 || arr.len() == 4) || arr[0].as_str() != Some("if") {
            return None;
        }
        Some(ConditionalNode {
            then_node: &arr[2],
            else_node: arr.get(3),
        })
    }

    /// Every possible value source of this expression, resolved through the
    /// operator registry's declared `returnSemantics` (see [`ValueWalk`]).
    /// A call whose operator declares a provenance kind yields the sources of
    /// the arguments that kind names; anything else (a literal, a binding, an
    /// undeclared operator) is an OPAQUE leaf — itself, crediting nothing
    /// beyond what it literally is.
    pub fn value_leaves(&self, walk: &ValueWalk<'_>) -> Vec<&SExpression> {
        let mut out = Vec::new();
        self.collect_value_leaves(walk, &mut out);
        out
    }

    fn collect_value_leaves<'a>(&'a self, walk: &ValueWalk<'_>, out: &mut Vec<&'a SExpression>) {
        let SExpression::Array(arr) = self else {
            out.push(self);
            return;
        };
        let Some(op) = arr.first().and_then(SExpression::as_str) else {
            out.push(self);
            return;
        };
        let args = &arr[1..];
        let plan = plan_value_sources(
            walk,
            op,
            args.len(),
            |i| args[i].is_lambda_form(walk),
            args.first().is_some_and(SExpression::is_statically_false),
        );
        let Some(slots) = plan else {
            out.push(self);
            return;
        };
        for slot in slots {
            match slot {
                ValueSlot::Arg(i) => args[i].collect_value_leaves(walk, out),
                ValueSlot::LambdaBody(i) => {
                    if let Some(body) = args[i].lambda_body() {
                        body.collect_value_leaves(walk, out);
                    }
                }
            }
        }
    }

    /// The single argument this call passes through unchanged: the operator
    /// declares `identity-of-arg<N>`, so the result is arg N narrowed or
    /// reordered and anything true of arg N's elements is true of the result's.
    pub fn passthrough_arg(&self, walk: &ValueWalk<'_>) -> Option<&SExpression> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        let op = arr.first()?.as_str()?;
        if !(walk.lookup)(op)?
            .semantics?
            .starts_with("identity-of-arg<")
        {
            return None;
        }
        let args = &arr[1..];
        match plan_value_sources(walk, op, args.len(), |_| false, false)?.as_slice() {
            [ValueSlot::Arg(i)] => Some(&args[*i]),
            _ => None,
        }
    }

    /// The body (last element) of a `[lambda-op, params, body]` array.
    fn lambda_body(&self) -> Option<&SExpression> {
        match self {
            SExpression::Array(arr) if arr.len() >= 3 => arr.last(),
            _ => None,
        }
    }

    /// True when this is a `[lambda-op, params, body]` call whose operator
    /// declares `returnType: function`.
    fn is_lambda_form(&self, walk: &ValueWalk<'_>) -> bool {
        self.lambda_body().is_some()
            && self
                .operator()
                .and_then(|op| (walk.lookup)(op))
                .is_some_and(|r| r.returns_function)
    }

    /// Mutable counterpart of [`SExpression::value_leaves`], for in-place
    /// rewrites (renames, codegen scope-qualification) that must reach
    /// descriptors nested behind every value source.
    pub fn value_leaves_mut(&mut self, walk: &ValueWalk<'_>) -> Vec<&mut SExpression> {
        let mut out = Vec::new();
        self.collect_value_leaves_mut(walk, &mut out);
        out
    }

    fn collect_value_leaves_mut<'a>(
        &'a mut self,
        walk: &ValueWalk<'_>,
        out: &mut Vec<&'a mut SExpression>,
    ) {
        let plan = match &*self {
            SExpression::Array(arr) => arr.first().and_then(SExpression::as_str).and_then(|op| {
                let args = &arr[1..];
                plan_value_sources(
                    walk,
                    op,
                    args.len(),
                    |i| args[i].is_lambda_form(walk),
                    args.first().is_some_and(SExpression::is_statically_false),
                )
            }),
            _ => None,
        };
        let Some(slots) = plan else {
            out.push(self);
            return;
        };
        let SExpression::Array(arr) = self else {
            unreachable!("a plan exists only for SExpression::Array")
        };
        for (i, arg) in arr[1..].iter_mut().enumerate() {
            match slots.iter().find(|slot| slot.arg() == i) {
                Some(ValueSlot::Arg(_)) => arg.collect_value_leaves_mut(walk, out),
                Some(ValueSlot::LambdaBody(_)) => {
                    if let SExpression::Array(lambda) = arg {
                        if let Some(body) = lambda.last_mut() {
                            body.collect_value_leaves_mut(walk, out);
                        }
                    }
                }
                None => {}
            }
        }
    }

    /// Get the binding path if this is a binding reference
    pub fn binding_path(&self) -> Option<&str> {
        match self {
            SExpression::String(s) if s.starts_with('@') => Some(&s[1..]),
            _ => None,
        }
    }

    /// Check if this is a function call (array starting with a string operator)
    pub fn is_call(&self) -> bool {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => {
                matches!(&arr[0], SExpression::String(_))
            }
            _ => false,
        }
    }

    /// Get the operator if this is a function call
    pub fn operator(&self) -> Option<&str> {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => arr[0].as_str(),
            _ => None,
        }
    }

    /// Get the arguments if this is a function call
    pub fn arguments(&self) -> Option<&[SExpression]> {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => Some(&arr[1..]),
            _ => None,
        }
    }

    /// True only when this expression is PROVABLY false without running the
    /// program — every leaf it depends on is already a literal.
    ///
    /// The motivating case is a guard whose `@config.<knob>` has been
    /// substituted at inline: `(when @config.running ...)` on a call site that
    /// set `running: false` folds to a literal `false`, and that arm can never
    /// fire. Any check that reasons about "can these two things happen together"
    /// has to know that, or it flags the very config knob authors are told to use
    /// to keep one owner per resource.
    ///
    /// A `string`-typed knob folds to its default the same way — a config knob
    /// declared `extraAddonEvent : string = ""` (the "this optional slot is
    /// unfulfilled" contract) substitutes to a literal empty string, not
    /// `Bool(false)`. A bare `(if @config.knob ...)` on that call site is
    /// exactly as dead as the boolean case, so every literal this crate's own
    /// evaluator treats as falsy (`Value::is_truthy`, evaluator/value.rs) is
    /// covered here too: `null`, `0`, `""`, and an empty object — not just
    /// `false`. One truthiness table, read by both the runtime and this
    /// static check, so they can never disagree about which literal is dead.
    ///
    /// Deliberately conservative: an expression this cannot decide returns
    /// `false` (meaning "not provably false"), never a guess. Comparisons against
    /// a binding, arithmetic, and anything unrecognised are all undecidable here.
    pub fn is_statically_false(&self) -> bool {
        match self {
            SExpression::Bool(false) => true,
            SExpression::Null => true,
            SExpression::Number(n) => *n == 0.0,
            SExpression::String(s) => s.is_empty(),
            SExpression::Object(obj) => obj.is_empty(),
            SExpression::Array(arr) => {
                let Some(op) = arr.first().and_then(|e| e.as_str()) else {
                    // No leading operator string — a literal array VALUE
                    // (not a call), so the same truthiness table applies:
                    // empty is falsy.
                    return arr.is_empty();
                };
                let args = &arr[1..];
                match op {
                    // Conjunction: one dead operand kills the whole guard.
                    "and" => args.iter().any(|a| a.is_statically_false()),
                    // Disjunction: dead only when every operand is dead.
                    "or" => !args.is_empty() && args.iter().all(|a| a.is_statically_false()),
                    "not" => args.len() == 1 && args[0].is_statically_true(),
                    // Two literals that differ can never be equal, and two that
                    // match can never be unequal.
                    "=" | "==" | "!=" => {
                        if args.len() != 2 || !args.iter().all(|a| a.is_literal()) {
                            return false;
                        }
                        (args[0] == args[1]) == (op == "!=")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// True only when this expression is PROVABLY truthy without running the program: the dual of
    /// [`Self::is_statically_false`], under the same truthiness table and the same conservatism. A comparison of two
    /// literals that match (a `@config.mode` substituted to its call-site value) is the motivating case: the other
    /// branch of the `if` it guards can never run.
    pub fn is_statically_true(&self) -> bool {
        match self {
            SExpression::Bool(true) => true,
            SExpression::Number(n) => *n != 0.0,
            SExpression::String(s) => !s.is_empty() && !s.starts_with('@'),
            SExpression::Object(obj) => !obj.is_empty(),
            SExpression::Array(arr) => {
                let Some(op) = arr.first().and_then(|e| e.as_str()) else {
                    return !arr.is_empty();
                };
                let args = &arr[1..];
                match op {
                    "and" => !args.is_empty() && args.iter().all(|a| a.is_statically_true()),
                    "or" => args.iter().any(|a| a.is_statically_true()),
                    "not" => args.len() == 1 && args[0].is_statically_false(),
                    "=" | "==" | "!=" => {
                        if args.len() != 2 || !args.iter().all(|a| a.is_literal()) {
                            return false;
                        }
                        (args[0] == args[1]) == (op != "!=")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// True when this node is a fully-known value — a literal, not a binding,
    /// call, or container. `@`-prefixed strings are bindings, not literals.
    fn is_literal(&self) -> bool {
        match self {
            SExpression::Bool(_) | SExpression::Number(_) | SExpression::Null => true,
            SExpression::String(s) => !s.starts_with('@'),
            _ => false,
        }
    }

    /// Every `@entity.<field>` this node READS, by root field. A `set` target is a write, so
    /// only its value is walked (`collect_entity_field_refs` counts the target too).
    pub fn collect_entity_reads(&self, out: &mut HashSet<String>) {
        match self {
            SExpression::String(s) => {
                if let Some(path) = s.strip_prefix("@entity.") {
                    let field = path.split(['.', '[']).next().unwrap_or("");
                    if !field.is_empty() {
                        out.insert(field.to_string());
                    }
                }
            }
            SExpression::Array(arr) => {
                let is_set = matches!(
                    (arr.first(), arr.get(1)),
                    (Some(SExpression::String(op)), Some(SExpression::String(target)))
                        if op == "set" && target.starts_with("@entity.")
                );
                let skip = if is_set { 2 } else { 0 };
                arr.iter()
                    .skip(skip)
                    .for_each(|i| i.collect_entity_reads(out));
            }
            SExpression::Object(obj) => obj.values().for_each(|v| v.collect_entity_reads(out)),
            _ => {}
        }
    }

    /// Walk this S-expression collecting every `@entity.<field>` reference into
    /// `referenced_fields` (keyed by field name → the originating event, first
    /// reference wins) and every `["set", "@entity.<field>", ...]` setter call
    /// into `set_fields`. Recurses through arrays and object values. `id`,
    /// `createdAt`, and `updatedAt` are skipped as referenced fields (runtime-
    /// managed); the setter scan keeps them.
    ///
    /// The single canonical walker behind the explicit-binding "every read has a
    /// setter" check — used by both the lolo lowerer and the post-inline
    /// compiler validator.
    pub fn collect_entity_field_refs(
        &self,
        referenced_fields: &mut HashMap<String, String>,
        set_fields: &mut HashSet<String>,
        transition_event: &str,
    ) {
        match self {
            SExpression::String(s) if s.starts_with("@entity.") => {
                let path = &s[8..];
                let field = path.split('.').next().unwrap_or("");
                let field = field.split('[').next().unwrap_or(field);
                if !field.is_empty()
                    && field != "id"
                    && field != "createdAt"
                    && field != "updatedAt"
                {
                    referenced_fields
                        .entry(field.to_string())
                        .or_insert_with(|| transition_event.to_string());
                }
            }
            SExpression::Array(arr) => {
                if arr.len() >= 2 {
                    if let (SExpression::String(op), SExpression::String(target)) =
                        (&arr[0], &arr[1])
                    {
                        if op == "set" && target.starts_with("@entity.") {
                            let setter_path = &target[8..];
                            let setter_field = setter_path.split('.').next().unwrap_or("");
                            let setter_field =
                                setter_field.split('[').next().unwrap_or(setter_field);
                            if !setter_field.is_empty() {
                                set_fields.insert(setter_field.to_string());
                            }
                        }
                    }
                }
                for item in arr {
                    item.collect_entity_field_refs(referenced_fields, set_fields, transition_event);
                }
            }
            SExpression::Object(obj) => {
                for value in obj.values() {
                    value.collect_entity_field_refs(
                        referenced_fields,
                        set_fields,
                        transition_event,
                    );
                }
            }
            _ => {}
        }
    }
}

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

/// One argument slot a declared kind resolves through.
#[derive(Debug, Clone, Copy)]
enum ValueSlot {
    Arg(usize),
    LambdaBody(usize),
}

impl ValueSlot {
    fn arg(self) -> usize {
        match self {
            ValueSlot::Arg(i) | ValueSlot::LambdaBody(i) => i,
        }
    }
}

/// The single interpreter of `returnSemantics` provenance kinds: which
/// argument slots of the call `op(args…)` its result draws from, or `None`
/// when the operator is undeclared, declares a non-provenance kind, or the
/// kind names an absent argument (opaque).
fn plan_value_sources(
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
        "union-of-args" => (0..arg_count).map(ValueSlot::Arg).collect(),
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

impl fmt::Display for SExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SExpression::Null => write!(f, "null"),
            SExpression::Bool(b) => write!(f, "{}", b),
            SExpression::Number(n) => write!(f, "{}", n),
            SExpression::String(s) => write!(f, "\"{}\"", s),
            SExpression::Array(arr) => {
                write!(f, "[")?;
                for (i, item) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
            SExpression::Object(obj) => {
                write!(f, "{{")?;
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{}\": {}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl From<bool> for SExpression {
    fn from(b: bool) -> Self {
        SExpression::Bool(b)
    }
}

impl From<i32> for SExpression {
    fn from(n: i32) -> Self {
        SExpression::Number(n as f64)
    }
}

impl From<i64> for SExpression {
    fn from(n: i64) -> Self {
        SExpression::Number(n as f64)
    }
}

impl From<f64> for SExpression {
    fn from(n: f64) -> Self {
        SExpression::Number(n)
    }
}

impl From<&str> for SExpression {
    fn from(s: &str) -> Self {
        SExpression::String(s.to_string())
    }
}

impl From<String> for SExpression {
    fn from(s: String) -> Self {
        SExpression::String(s)
    }
}

impl<T: Into<SExpression>> From<Vec<T>> for SExpression {
    fn from(v: Vec<T>) -> Self {
        SExpression::Array(v.into_iter().map(Into::into).collect())
    }
}

fn json_head(value: &serde_json::Value) -> Option<(&str, &[serde_json::Value])> {
    let arr = value.as_array()?;
    let (head, args) = arr.split_first()?;
    Some((head.as_str()?, args))
}

fn json_lambda_body(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value.as_array().filter(|a| a.len() >= 3)?.last()
}

fn json_plan(walk: &ValueWalk<'_>, value: &serde_json::Value) -> Option<Vec<ValueSlot>> {
    let (op, args) = json_head(value)?;
    plan_value_sources(
        walk,
        op,
        args.len(),
        |i| {
            json_lambda_body(&args[i]).is_some()
                && json_head(&args[i])
                    .and_then(|(head, _)| (walk.lookup)(head))
                    .is_some_and(|r| r.returns_function)
        },
        false,
    )
}

/// `serde_json::Value` counterpart of [`SExpression::value_leaves`]. Config
/// values arrive as raw JSON before/without a full `SExpression` parse. The
/// dead-branch fold of [`ValueWalk::skip_dead_branches`] is an `SExpression`
/// notion and does not apply here: every branch contributes.
pub fn json_value_leaves<'a>(
    value: &'a serde_json::Value,
    walk: &ValueWalk<'_>,
) -> Vec<&'a serde_json::Value> {
    let mut out = Vec::new();
    collect_json_value_leaves(value, walk, &mut out);
    out
}

fn collect_json_value_leaves<'a>(
    value: &'a serde_json::Value,
    walk: &ValueWalk<'_>,
    out: &mut Vec<&'a serde_json::Value>,
) {
    let Some(slots) = json_plan(walk, value) else {
        out.push(value);
        return;
    };
    let args = &value.as_array().expect("a plan exists only for arrays")[1..];
    for slot in slots {
        match slot {
            ValueSlot::Arg(i) => collect_json_value_leaves(&args[i], walk, out),
            ValueSlot::LambdaBody(i) => {
                if let Some(body) = json_lambda_body(&args[i]) {
                    collect_json_value_leaves(body, walk, out);
                }
            }
        }
    }
}

/// Mutable counterpart of [`json_value_leaves`], for sites that rewrite
/// descriptors in place (renames, codegen qualification).
pub fn json_value_leaves_mut<'a>(
    value: &'a mut serde_json::Value,
    walk: &ValueWalk<'_>,
) -> Vec<&'a mut serde_json::Value> {
    let mut out = Vec::new();
    collect_json_value_leaves_mut(value, walk, &mut out);
    out
}

fn collect_json_value_leaves_mut<'a>(
    value: &'a mut serde_json::Value,
    walk: &ValueWalk<'_>,
    out: &mut Vec<&'a mut serde_json::Value>,
) {
    let Some(slots) = json_plan(walk, value) else {
        out.push(value);
        return;
    };
    let arr = value.as_array_mut().expect("a plan exists only for arrays");
    for (i, arg) in arr[1..].iter_mut().enumerate() {
        match slots.iter().find(|slot| slot.arg() == i) {
            Some(ValueSlot::Arg(_)) => collect_json_value_leaves_mut(arg, walk, out),
            Some(ValueSlot::LambdaBody(_)) => {
                if let Some(body) = arg.as_array_mut().and_then(|a| a.last_mut()) {
                    collect_json_value_leaves_mut(body, walk, out);
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_guard() {
        let json = r#"["and", ["!=", "@payload.data.email", ""], [">=", "@payload.data.connectionSlots", 1]]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();

        assert!(expr.is_call());
        assert_eq!(expr.operator(), Some("and"));

        let args = expr.arguments().unwrap();
        assert_eq!(args.len(), 2);

        // First arg: ["!=", "@payload.data.email", ""]
        assert!(args[0].is_call());
        assert_eq!(args[0].operator(), Some("!="));

        // Check binding
        let ne_args = args[0].arguments().unwrap();
        assert!(ne_args[0].is_binding());
        assert_eq!(ne_args[0].binding_path(), Some("payload.data.email"));
    }

    #[test]
    fn test_parse_effect() {
        let json =
            r#"["render-ui", "main", { "type": "page-header", "title": "Community Members" }]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();

        assert!(expr.is_call());
        assert_eq!(expr.operator(), Some("render-ui"));

        let args = expr.arguments().unwrap();
        assert_eq!(args[0].as_str(), Some("main"));

        let props = args[1].as_object().unwrap();
        assert_eq!(
            props.get("type").and_then(|v| v.as_str()),
            Some("page-header")
        );
    }

    #[test]
    fn test_parse_persist_effect() {
        let json = r#"["persist", "create", "User", "@payload.data"]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();

        assert_eq!(expr.operator(), Some("persist"));
        let args = expr.arguments().unwrap();
        assert_eq!(args[0].as_str(), Some("create"));
        assert_eq!(args[1].as_str(), Some("User"));
        assert!(args[2].is_binding());
    }

    #[test]
    fn mapped_children_recognizes_single_param_tuple() {
        // ["array/map", "@entity.items", ["fn", "item", { type: badge, ... }]]
        let json = r#"["array/map", "@entity.items", ["fn", "item", { "type": "badge", "label": "@item.name" }]]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();
        let mc = expr
            .as_mapped_children()
            .expect("must recognize array/map tuple");
        assert_eq!(mc.collection.as_str(), Some("@entity.items"));
        assert_eq!(mc.params, vec!["item"]);
        assert_eq!(
            mc.item_node
                .as_object()
                .unwrap()
                .get("type")
                .and_then(|v| v.as_str()),
            Some("badge")
        );
    }

    #[test]
    fn mapped_children_recognizes_grouped_param_form() {
        // Normalized grouped lambda: ["fn", ["q", "i"], node]
        let json = r#"["array/map", "@config.questions", ["fn", ["q", "i"], { "type": "card" }]]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();
        let mc = expr.as_mapped_children().expect("grouped param form");
        assert_eq!(mc.params, vec!["q", "i"]);
    }

    #[test]
    fn mapped_children_rejects_non_map_arrays() {
        // Ordinary children entries / operator calls are NOT mapped-children.
        for json in [
            r#"{ "type": "badge" }"#,
            r#""@trait.Foo""#,
            r#"["array/filter", "@entity.items", ["fn", "item", true]]"#,
            r#"["array/map", "@entity.items"]"#,
            r#"["array/map", "@entity.items", { "type": "badge" }]"#,
        ] {
            let expr: SExpression = serde_json::from_str(json).unwrap();
            assert!(
                expr.as_mapped_children().is_none(),
                "must not recognize: {json}"
            );
        }
    }

    #[test]
    fn test_complex_guard_from_orbital_pattern() {
        // From Orbital_Pattern.orb - Dunbar's number guard
        let json = r#"["<", ["count", "Connection", ["and", ["=", "@Connection.requesterId", "@user.id"], ["=", "@Connection.status", "accepted"]]], 150]"#;
        let expr: SExpression = serde_json::from_str(json).unwrap();

        assert_eq!(expr.operator(), Some("<"));
        let args = expr.arguments().unwrap();

        // First arg is count expression
        assert_eq!(args[0].operator(), Some("count"));

        // Second arg is 150
        assert_eq!(args[1].as_number(), Some(150.0));
    }

    #[test]
    fn canonical_json_sorts_object_keys_so_emission_is_byte_stable() {
        // Build the SAME logical object many times. HashMap iteration order is
        // randomly seeded, so a non-canonical serializer disagrees with itself.
        let render = || {
            let mut map = SObject::new();
            map.insert("zebra".to_string(), SExpression::Number(1.0));
            map.insert("alpha".to_string(), SExpression::Bool(true));
            map.insert("middle".to_string(), SExpression::String("x".to_string()));
            let expr = SExpression::Array(vec![
                SExpression::String("=".to_string()),
                SExpression::Object(map),
            ]);
            serde_json::to_string(&expr.to_canonical_json()).expect("serializes")
        };
        let first = render();
        for _ in 0..64 {
            assert_eq!(
                render(),
                first,
                "canonical JSON must not vary between renders"
            );
        }
        assert_eq!(first, r#"["=",{"alpha":true,"middle":"x","zebra":1.0}]"#);
    }

    #[test]
    fn canonical_json_round_trips_nested_arrays_and_scalars() {
        let expr = SExpression::Array(vec![
            SExpression::String("or".to_string()),
            SExpression::Array(vec![
                SExpression::String("=".to_string()),
                SExpression::String("@user.role".to_string()),
                SExpression::String("supervisor".to_string()),
            ]),
            SExpression::Null,
        ]);
        assert_eq!(
            serde_json::to_string(&expr.to_canonical_json()).expect("serializes"),
            r#"["or",["=","@user.role","supervisor"],null]"#
        );
    }

    fn test_lookup(op: &str) -> Option<OperatorReturn> {
        let (semantics, returns_function) = match op {
            "if" => (Some("branch-union"), false),
            "array/filter" | "array/slice" | "array/sort" => (Some("identity-of-arg<0>"), false),
            "array/concat" => (Some("union-of-args"), false),
            "array/append" => (Some("union-of-args<0,1>"), false),
            "array/insert" => (Some("union-of-args<0,2>"), false),
            "do" => (Some("last-of-args"), false),
            "let" => (Some("identity-of-arg<1>"), false),
            "array/map" => (Some("lambda-result"), false),
            "array/zip" => (None, false),
            "fn" => (None, true),
            _ => return None,
        };
        Some(OperatorReturn {
            semantics,
            returns_function,
        })
    }

    const WALK: ValueWalk<'static> = ValueWalk {
        lookup: &test_lookup,
        skip_dead_branches: false,
    };

    fn sx(json: &str) -> SExpression {
        serde_json::from_str(json).unwrap()
    }

    fn leaf_strings(expr: &SExpression) -> Vec<String> {
        expr.value_leaves(&WALK)
            .iter()
            .map(|l| serde_json::to_string(&l.to_canonical_json()).unwrap())
            .collect()
    }

    #[test]
    fn value_leaves_returns_self_for_plain_and_undeclared() {
        assert_eq!(leaf_strings(&sx(r#""plain""#)), vec![r#""plain""#]);
        assert_eq!(leaf_strings(&sx(r#"["a", "b"]"#)), vec![r#"["a","b"]"#]);
        assert_eq!(
            leaf_strings(&sx(r#"{"event": "X"}"#)),
            vec![r#"{"event":"X"}"#]
        );
    }

    #[test]
    fn value_leaves_branch_union_takes_every_branch_never_the_condition() {
        assert_eq!(
            leaf_strings(&sx(r#"["if", "@user.role", "T", "E"]"#)),
            vec![r#""T""#, r#""E""#]
        );
        assert_eq!(leaf_strings(&sx(r#"["if", "@c", "T"]"#)), vec![r#""T""#]);
        assert_eq!(
            leaf_strings(&sx(r#"["if", "@a", ["if", "@b", "x", "y"], "z"]"#)),
            vec![r#""x""#, r#""y""#, r#""z""#]
        );
        // no branch at all: opaque, exactly the call itself
        assert_eq!(leaf_strings(&sx(r#"["if", "@c"]"#)), vec![r#"["if","@c"]"#]);
    }

    #[test]
    fn value_leaves_identity_kinds_see_through_subsets_and_reorders() {
        for op in ["array/filter", "array/slice", "array/sort"] {
            let call = format!(r#"["{op}", "LIST", 1, 2]"#);
            assert_eq!(leaf_strings(&sx(&call)), vec![r#""LIST""#], "{op}");
        }
        // nested: filter inside slice inside if
        assert_eq!(
            leaf_strings(&sx(
                r#"["array/slice", ["if", "@c", ["array/filter", "A", ["fn", "i", true]], "B"], 0, 3]"#
            )),
            vec![r#""A""#, r#""B""#]
        );
    }

    #[test]
    fn value_leaves_union_of_args_covers_every_argument_or_the_listed_ones() {
        assert_eq!(
            leaf_strings(&sx(r#"["array/concat", "A", ["if", "@c", "B", "C"], "D"]"#)),
            vec![r#""A""#, r#""B""#, r#""C""#, r#""D""#]
        );
        // append: the list and the added element, never the extra args
        assert_eq!(
            leaf_strings(&sx(r#"["array/append", "LIST", {"event": "X"}]"#)),
            vec![r#""LIST""#, r#"{"event":"X"}"#]
        );
        // insert: arg 0 and arg 2, skipping the index at arg 1
        assert_eq!(
            leaf_strings(&sx(r#"["array/insert", "LIST", 4, "ELEM"]"#)),
            vec![r#""LIST""#, r#""ELEM""#]
        );
        // a listed arg that is absent makes the call opaque
        assert_eq!(
            leaf_strings(&sx(r#"["array/append", "LIST"]"#)),
            vec![r#"["array/append","LIST"]"#]
        );
    }

    #[test]
    fn passthrough_arg_follows_only_identity_kinds() {
        let filtered = sx(r#"["array/filter", "LIST", ["fn", "i", true]]"#);
        assert_eq!(
            filtered.passthrough_arg(&WALK),
            Some(&SExpression::String("LIST".into()))
        );
        let bound = sx(r#"["let", [["x", 1]], "BODY"]"#);
        assert_eq!(
            bound.passthrough_arg(&WALK),
            Some(&SExpression::String("BODY".into()))
        );
        for other in [
            r#"["array/concat", "A", "B"]"#,
            r#"["if", "@c", "A", "B"]"#,
            r#"["array/zip", "A", "B"]"#,
            r#""plain""#,
            r#"["array/filter"]"#,
        ] {
            assert_eq!(sx(other).passthrough_arg(&WALK), None, "{other}");
        }
    }

    #[test]
    fn value_leaves_last_of_args_and_body_kinds() {
        assert_eq!(
            leaf_strings(&sx(r#"["do", ["set", "@x", 1], "LAST"]"#)),
            vec![r#""LAST""#]
        );
        assert_eq!(
            leaf_strings(&sx(r#"["let", [["x", 1]], ["if", "@c", "P", "Q"]]"#)),
            vec![r#""P""#, r#""Q""#]
        );
        assert_eq!(leaf_strings(&sx(r#"["do"]"#)), vec![r#"["do"]"#]);
    }

    #[test]
    fn value_leaves_lambda_result_yields_the_lambda_body_leaves() {
        assert_eq!(
            leaf_strings(&sx(
                r#"["array/map", "@entity.items", ["fn", "item", ["if", "@item.ok", {"event": "A"}, {"event": "B"}]]]"#
            )),
            vec![r#"{"event":"A"}"#, r#"{"event":"B"}"#]
        );
        // no lambda argument: opaque
        assert_eq!(
            leaf_strings(&sx(r#"["array/map", "@entity.items", "@config.fn"]"#)),
            vec![r#"["array/map","@entity.items","@config.fn"]"#]
        );
    }

    #[test]
    fn value_leaves_undeclared_operator_is_opaque_even_over_a_declared_one() {
        let zip = sx(r#"["array/zip", ["array/filter", "A", ["fn", "i", true]], "B"]"#);
        assert_eq!(
            leaf_strings(&zip),
            vec![serde_json::to_string(&zip.to_canonical_json()).unwrap()]
        );
        // a declared wrapper around an undeclared call ends at the undeclared call
        let wrapped = sx(r#"["array/slice", ["array/zip", "A", "B"], 0, 2]"#);
        assert_eq!(leaf_strings(&wrapped), vec![r#"["array/zip","A","B"]"#]);
    }

    #[test]
    fn value_leaves_skip_dead_branches_only_when_asked() {
        let expr = sx(r#"["if", false, "DEAD", "LIVE"]"#);
        assert_eq!(leaf_strings(&expr), vec![r#""DEAD""#, r#""LIVE""#]);
        let folding = ValueWalk {
            lookup: &test_lookup,
            skip_dead_branches: true,
        };
        assert_eq!(
            expr.value_leaves(&folding),
            vec![&SExpression::String("LIVE".into())]
        );
        let only_then = sx(r#"["if", "", "DEAD"]"#);
        assert!(only_then.value_leaves(&folding).is_empty());
    }

    #[test]
    fn value_leaves_mut_reaches_every_source_and_allows_rewrite() {
        let mut expr = sx(
            r#"["array/concat", ["array/filter", "A", ["fn", "i", true]], ["if", "@c", "B", "C"], ["array/map", "M", ["fn", "x", "BODY"]]]"#,
        );
        for leaf in expr.value_leaves_mut(&WALK) {
            if let SExpression::String(s) = leaf {
                *s = format!("rw-{s}");
            }
        }
        assert_eq!(
            expr,
            sx(
                r#"["array/concat", ["array/filter", "rw-A", ["fn", "i", true]], ["if", "@c", "rw-B", "rw-C"], ["array/map", "M", ["fn", "x", "rw-BODY"]]]"#
            )
        );
    }

    #[test]
    fn json_value_leaves_mirror_the_sexpression_walk() {
        let value = serde_json::json!(["array/concat",
            ["array/slice", [{"event": "A"}], 0, 1],
            ["if", "@c", [{"event": "B"}], [{"event": "C"}]],
            ["array/map", "M", ["fn", "x", {"event": "D"}]],
            ["array/zip", [{"event": "NOPE"}], []]
        ]);
        let events: Vec<String> = json_value_leaves(&value, &WALK)
            .iter()
            .filter_map(|l| {
                l.as_array()
                    .and_then(|a| a.first())
                    .or(Some(l))
                    .and_then(|d| d.get("event"))
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(events, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn json_value_leaves_mut_rewrites_in_place_and_skips_undeclared() {
        let mut value = serde_json::json!(["array/concat",
            ["array/sort", [{"action": "X"}], "k", "asc"],
            ["array/zip", [{"action": "Y"}], []]
        ]);
        for leaf in json_value_leaves_mut(&mut value, &WALK) {
            if let Some(arr) = leaf.as_array_mut() {
                for d in arr.iter_mut() {
                    if let Some(a) = d.get("action").and_then(|a| a.as_str()) {
                        d["action"] = serde_json::json!(format!("T.{a}"));
                    }
                }
            }
        }
        assert_eq!(value[1][1][0]["action"], "T.X");
        assert_eq!(value[2][1][0]["action"], "Y");
    }

    #[test]
    fn is_statically_true_is_the_dual_of_is_statically_false() {
        assert!(SExpression::Bool(true).is_statically_true());
        assert!(eq("==", lit("create"), lit("create")).is_statically_true());
        assert!(eq("!=", lit("a"), lit("b")).is_statically_true());
        assert!(!eq("==", lit("a"), lit("b")).is_statically_true());
        assert!(!eq("==", lit("@config.mode"), lit("create")).is_statically_true(), "a binding is undecidable");
        let or = SExpression::Array(vec![
            lit("or"),
            eq("==", lit("create"), lit("create")),
            lit("@payload.row"),
        ]);
        assert!(or.is_statically_true(), "one operand provably true");
        assert!(!or.is_statically_false());
        let and = SExpression::Array(vec![lit("and"), SExpression::Bool(true), lit("@payload.row")]);
        assert!(!and.is_statically_true() && !and.is_statically_false(), "undecidable");
        let not = SExpression::Array(vec![lit("not"), SExpression::Bool(false)]);
        assert!(not.is_statically_true());
        assert!(!SExpression::Bool(false).is_statically_true());
        assert!(!SExpression::String(String::new()).is_statically_true());
        assert!(!SExpression::Null.is_statically_true());
        assert!(!SExpression::String("@config.knob".to_string()).is_statically_true());
    }

    #[test]
    fn is_statically_false_true_for_bool_false() {
        assert!(SExpression::Bool(false).is_statically_false());
    }

    fn eq(op: &str, a: SExpression, b: SExpression) -> SExpression {
        SExpression::Array(vec![SExpression::String(op.to_string()), a, b])
    }

    fn lit(v: &str) -> SExpression {
        SExpression::String(v.to_string())
    }

    #[test]
    fn single_equals_on_differing_literals_is_statically_false() {
        assert!(eq("=", lit("panel"), lit("document")).is_statically_false());
    }

    #[test]
    fn control_single_equals_on_matching_literals_is_live() {
        assert!(!eq("=", lit("document"), lit("document")).is_statically_false());
    }

    #[test]
    fn control_single_equals_on_a_binding_is_undecidable() {
        assert!(!eq("=", lit("@config.detailLook"), lit("document")).is_statically_false());
    }

    #[test]
    fn double_equals_still_folds() {
        assert!(eq("==", lit("a"), lit("b")).is_statically_false());
        assert!(eq("!=", lit("a"), lit("a")).is_statically_false());
    }

    #[test]
    fn is_statically_false_true_for_empty_string() {
        // A `string`-typed config knob defaulting to "" folds to this, not
        // `Bool(false)` — the "optional slot unfulfilled" contract.
        assert!(SExpression::String(String::new()).is_statically_false());
    }

    #[test]
    fn is_statically_false_false_for_non_empty_string() {
        assert!(!SExpression::String("hi".to_string()).is_statically_false());
    }

    #[test]
    fn is_statically_false_true_for_null() {
        assert!(SExpression::Null.is_statically_false());
    }

    #[test]
    fn is_statically_false_true_for_zero() {
        assert!(SExpression::Number(0.0).is_statically_false());
    }

    #[test]
    fn is_statically_false_false_for_nonzero_number() {
        assert!(!SExpression::Number(1.0).is_statically_false());
    }

    #[test]
    fn is_statically_false_true_for_empty_object() {
        assert!(SExpression::Object(SObject::new()).is_statically_false());
    }

    #[test]
    fn is_statically_false_true_for_empty_array_literal() {
        assert!(SExpression::Array(vec![]).is_statically_false());
    }

    #[test]
    fn is_statically_false_false_for_binding_string() {
        // `@`-prefixed strings are bindings, not literals — undecidable.
        assert!(!SExpression::String("@config.knob".to_string()).is_statically_false());
    }

    fn reads(node: serde_json::Value) -> Vec<String> {
        let expr: SExpression = serde_json::from_value(node).expect("s-expression");
        let mut out = std::collections::HashSet::new();
        expr.collect_entity_reads(&mut out);
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn entity_reads_skip_the_set_target() {
        assert!(reads(serde_json::json!([
            "set",
            "@entity.searchTerm",
            "?searchTerm"
        ]))
        .is_empty());
    }

    #[test]
    fn entity_reads_count_a_field_read_inside_the_set_value() {
        assert_eq!(
            reads(serde_json::json!([
                "set",
                "@entity.total",
                ["+", "@entity.subtotal", 1]
            ])),
            vec!["subtotal"]
        );
    }

    #[test]
    fn entity_reads_count_render_bindings_by_their_root_field() {
        assert_eq!(
            reads(
                serde_json::json!(["render-ui", "main", { "type": "typography", "content": "@entity.profile.name", "items": "@entity.rows[0]" }])
            ),
            vec!["profile", "rows"]
        );
    }

    #[test]
    fn entity_reads_ignore_payload_and_config_bindings() {
        assert!(reads(serde_json::json!(["if", "@config.on", "@payload.x", "?y"])).is_empty());
    }
}
