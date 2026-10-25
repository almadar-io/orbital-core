//! OIR Expression Types
//!
//! Universal expression type for guards and effects (parsed S-expressions).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

/// Expression-object fields in authored order (deterministic codegen and IR wire).
pub type OirFields = crate::collections::IndexMap<String, OirExpr>;

/// Universal expression type (parsed S-expression)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OirExpr {
    // Literals
    Null,
    Bool {
        value: bool,
    },
    Number {
        value: f64,
    },
    String {
        value: String,
    },
    Array {
        items: Vec<OirExpr>,
    },
    Object {
        fields: OirFields,
    },

    // Bindings
    Binding {
        binding: OirBinding,
    },

    // Operations
    BinaryOp {
        op: OirBinaryOp,
        left: Box<OirExpr>,
        right: Box<OirExpr>,
    },
    UnaryOp {
        op: OirUnaryOp,
        operand: Box<OirExpr>,
    },
    Conditional {
        condition: Box<OirExpr>,
        then_branch: Box<OirExpr>,
        else_branch: Box<OirExpr>,
    },
    Call {
        function: String,
        args: Vec<OirExpr>,
    },

    // Collection operations
    Map {
        source: Box<OirExpr>,
        variable: String,
        body: Box<OirExpr>,
    },
    Filter {
        source: Box<OirExpr>,
        variable: String,
        predicate: Box<OirExpr>,
    },
    Reduce {
        source: Box<OirExpr>,
        variable: String,
        accumulator: String,
        initial: Box<OirExpr>,
        body: Box<OirExpr>,
    },

    /// A lambda `(fn params body)` — the single shared representation every
    /// backend consumes. `params` is the normalized parameter list (one entry
    /// for `["fn","p",body]`, N for the grouped `["fn",[p1,…,pN],body]` form);
    /// the lolo lowerer canonicalizes the flat form away before resolve.
    /// Replaces the per-site `Call{function:"fn"}` re-parsing each backend used
    /// to do, so a validator-accepted lambda is always codegen-emittable.
    Lambda {
        params: Vec<String>,
        body: Box<OirExpr>,
    },

    // Effects (in effect position only)
    Effect {
        effect: Box<crate::ir::effects::OirEffect>,
    },
}

#[allow(clippy::should_implement_trait)] // add/sub/not/neg are AST builder helpers, not operator traits
impl OirExpr {
    // Literal constructors
    pub fn null() -> Self {
        Self::Null
    }

    pub fn bool(value: bool) -> Self {
        Self::Bool { value }
    }

    pub fn number(value: f64) -> Self {
        Self::Number { value }
    }

    pub fn string(value: impl Into<String>) -> Self {
        Self::String {
            value: value.into(),
        }
    }

    pub fn array(items: Vec<OirExpr>) -> Self {
        Self::Array { items }
    }

    pub fn object(fields: OirFields) -> Self {
        Self::Object { fields }
    }

    /// Convert this OIR expression back to its canonical [`SExpression`] form —
    /// the inverse of the resolve lowering. This is the bridge that lets the one
    /// canonical evaluator (`orbital_core::Evaluator` / `eval_expr`, which is
    /// `SExpression`-based) evaluate an `OirExpr` guard, instead of any consumer
    /// re-implementing a parallel walker. Operator spellings come from the
    /// existing `symbol()` methods + binding `Display`, EXCEPT the four that are
    /// not operator names in both evaluators — `&&`/`||`/`++`/`!` are remapped
    /// to `and`/`or`/`str/concat`/`not`. The Rust evaluator tolerates the raw
    /// symbols; `@almadar/evaluator` does not, and it fails OPEN on an unknown
    /// head, so only the remapped spellings are safe to emit.
    ///
    /// Returns `None` for nodes that have no S-expression guard form (`Effect`);
    /// callers in a guard context treat that as "can't decide".
    pub fn to_sexpression(&self) -> Option<crate::schema::sexpr::SExpression> {
        use crate::schema::sexpr::SExpression as S;
        let arr = |head: &str, parts: Vec<S>| {
            let mut v = Vec::with_capacity(parts.len() + 1);
            v.push(S::String(head.to_string()));
            v.extend(parts);
            S::Array(v)
        };
        Some(match self {
            OirExpr::Null => S::Null,
            OirExpr::Bool { value } => S::Bool(*value),
            OirExpr::Number { value } => S::Number(*value),
            OirExpr::String { value } => S::String(value.clone()),
            OirExpr::Binding { binding } => S::String(binding.to_string()),
            OirExpr::Array { items } => {
                let mut v = Vec::with_capacity(items.len());
                for it in items {
                    v.push(it.to_sexpression()?);
                }
                S::Array(v)
            }
            OirExpr::Object { fields } => {
                let mut m = crate::schema::sexpr::SObject::with_capacity_and_hasher(fields.len(), Default::default());
                for (k, val) in fields {
                    m.insert(k.clone(), val.to_sexpression()?);
                }
                S::Object(m)
            }
            OirExpr::BinaryOp { op, left, right } => {
                // Three `symbol()` spellings are NOT operator names. `++`
                // (Concat) maps to `str/concat`; `&&`/`||` map to `and`/`or`.
                //
                // `&&`/`||` matter more than they look: the RUST evaluator
                // accepts them, the JS one (`@almadar/evaluator`) does not — and
                // an unknown head is not an error there, it returns the array
                // unchanged, which is TRUTHY. So a lowered `@read` policy handed
                // to the JS evaluator admitted every row for every viewer:
                // fail-OPEN, silent, in exactly the code path that exists to
                // deny. Emit spellings BOTH evaluators implement.
                let head = match op {
                    OirBinaryOp::Concat => "str/concat",
                    OirBinaryOp::And => "and",
                    OirBinaryOp::Or => "or",
                    other => other.symbol(),
                };
                arr(head, vec![left.to_sexpression()?, right.to_sexpression()?])
            }
            OirExpr::UnaryOp { op, operand } => {
                let head = match op {
                    OirUnaryOp::Not => "not",
                    OirUnaryOp::Neg => "-",
                };
                arr(head, vec![operand.to_sexpression()?])
            }
            OirExpr::Conditional {
                condition,
                then_branch,
                else_branch,
            } => arr(
                "if",
                vec![
                    condition.to_sexpression()?,
                    then_branch.to_sexpression()?,
                    else_branch.to_sexpression()?,
                ],
            ),
            OirExpr::Call { function, args } => {
                let mut parts = Vec::with_capacity(args.len());
                for a in args {
                    parts.push(a.to_sexpression()?);
                }
                arr(function, parts)
            }
            OirExpr::Lambda { params, body } => {
                let params_sexpr = if params.len() == 1 {
                    S::String(params[0].clone())
                } else {
                    S::Array(params.iter().map(|p| S::String(p.clone())).collect())
                };
                arr("fn", vec![params_sexpr, body.to_sexpression()?])
            }
            OirExpr::Map {
                source,
                variable,
                body,
            } => arr(
                "array/map",
                vec![
                    source.to_sexpression()?,
                    arr(
                        "fn",
                        vec![S::String(variable.clone()), body.to_sexpression()?],
                    ),
                ],
            ),
            OirExpr::Filter {
                source,
                variable,
                predicate,
            } => arr(
                "array/filter",
                vec![
                    source.to_sexpression()?,
                    arr(
                        "fn",
                        vec![S::String(variable.clone()), predicate.to_sexpression()?],
                    ),
                ],
            ),
            OirExpr::Reduce {
                source,
                variable,
                accumulator,
                initial,
                body,
            } => arr(
                "array/reduce",
                vec![
                    source.to_sexpression()?,
                    arr(
                        "fn",
                        vec![
                            S::Array(vec![
                                S::String(accumulator.clone()),
                                S::String(variable.clone()),
                            ]),
                            body.to_sexpression()?,
                        ],
                    ),
                    initial.to_sexpression()?,
                ],
            ),
            // No S-expression guard form.
            OirExpr::Effect { .. } => return None,
        })
    }

    // Binding constructors
    pub fn binding(binding: OirBinding) -> Self {
        Self::Binding { binding }
    }

    pub fn entity_binding(path: Vec<String>) -> Self {
        Self::Binding {
            binding: OirBinding {
                root: OirBindingRoot::Entity,
                path,
            },
        }
    }

    pub fn payload_binding(path: Vec<String>) -> Self {
        Self::Binding {
            binding: OirBinding {
                root: OirBindingRoot::Payload,
                path,
            },
        }
    }

    pub fn state_binding() -> Self {
        Self::Binding {
            binding: OirBinding {
                root: OirBindingRoot::State,
                path: vec![],
            },
        }
    }

    // Binary operation constructors
    pub fn eq(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Eq,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn ne(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Ne,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn and(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::And,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn or(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Or,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn lt(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Lt,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn gt(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Gt,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn add(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Add,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn sub(left: OirExpr, right: OirExpr) -> Self {
        Self::BinaryOp {
            op: OirBinaryOp::Sub,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    // Unary operation constructors
    pub fn not(operand: OirExpr) -> Self {
        Self::UnaryOp {
            op: OirUnaryOp::Not,
            operand: Box::new(operand),
        }
    }

    pub fn neg(operand: OirExpr) -> Self {
        Self::UnaryOp {
            op: OirUnaryOp::Neg,
            operand: Box::new(operand),
        }
    }

    // Conditional
    pub fn if_then_else(condition: OirExpr, then_branch: OirExpr, else_branch: OirExpr) -> Self {
        Self::Conditional {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
        }
    }

    // Function call
    pub fn call(function: impl Into<String>, args: Vec<OirExpr>) -> Self {
        Self::Call {
            function: function.into(),
            args,
        }
    }

    pub fn lambda(params: Vec<String>, body: OirExpr) -> Self {
        Self::Lambda {
            params,
            body: Box::new(body),
        }
    }

    // Collection operations
    pub fn map(source: OirExpr, variable: impl Into<String>, body: OirExpr) -> Self {
        Self::Map {
            source: Box::new(source),
            variable: variable.into(),
            body: Box::new(body),
        }
    }

    pub fn filter(source: OirExpr, variable: impl Into<String>, predicate: OirExpr) -> Self {
        Self::Filter {
            source: Box::new(source),
            variable: variable.into(),
            predicate: Box::new(predicate),
        }
    }

    // Effect wrapper
    pub fn effect(effect: crate::ir::effects::OirEffect) -> Self {
        Self::Effect {
            effect: Box::new(effect),
        }
    }

    /// Check if this expression is a literal
    pub fn is_literal(&self) -> bool {
        matches!(
            self,
            Self::Null
                | Self::Bool { .. }
                | Self::Number { .. }
                | Self::String { .. }
                | Self::Array { .. }
                | Self::Object { .. }
        )
    }

    /// Check if this expression is a binding
    pub fn is_binding(&self) -> bool {
        matches!(self, Self::Binding { .. })
    }

    /// Check if this expression is an effect
    pub fn is_effect(&self) -> bool {
        matches!(self, Self::Effect { .. })
    }

    /// Get the effect if this is an effect expression
    pub fn as_effect(&self) -> Option<&crate::ir::effects::OirEffect> {
        match self {
            Self::Effect { effect } => Some(effect),
            _ => None,
        }
    }

    /// Collect all bindings used in this expression
    pub fn collect_bindings(&self) -> Vec<&OirBinding> {
        let mut bindings = vec![];
        self.collect_bindings_recursive(&mut bindings);
        bindings
    }

    /// True when this expression tree (descending into nested effects, the
    /// same traversal as `collect_bindings`) contains a `Call` to `function`.
    /// Lets codegen decide per-trait imports for operators whose emitted form
    /// is a shared helper call rather than an inline expression.
    /// True when this tree runs an effect whose site is `server`
    /// (`effects::runs_on`), at any depth — through nested effects too.
    ///
    /// Fold-aware, like `effects::sexpr_runs_server_effect`: a branch whose
    /// condition is statically false never runs.
    pub fn runs_server_effect(&self, residence: &crate::runs_on::Residence) -> bool {
        match self {
            Self::Effect { effect } => {
                crate::runs_on::ir_effect_site(effect, residence) == crate::runs_on::RunsOn::Server
                    || effect.child_exprs().iter().any(|e| e.runs_server_effect(residence))
            }
            Self::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_live = !condition.is_statically_false();
                (then_live && then_branch.runs_server_effect(residence))
                    || else_branch.runs_server_effect(residence)
            }
            _ => self.child_exprs().iter().any(|e| e.runs_server_effect(residence)),
        }
    }

    /// Whether this expression runs a data effect that `residence` moves from
    /// the server to the browser store (a `fetch`/`persist` on a local entity).
    pub fn runs_browser_data_effect(&self, residence: &crate::runs_on::Residence) -> bool {
        use crate::runs_on::{effect_runs_on, ir_effect_site, RunsOn};
        match self {
            Self::Effect { effect } => {
                (effect_runs_on(effect) == RunsOn::Server
                    && ir_effect_site(effect, residence) == RunsOn::Client)
                    || effect
                        .child_exprs()
                        .iter()
                        .any(|e| e.runs_browser_data_effect(residence))
            }
            Self::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_live = !condition.is_statically_false();
                (then_live && then_branch.runs_browser_data_effect(residence))
                    || else_branch.runs_browser_data_effect(residence)
            }
            _ => self
                .child_exprs()
                .iter()
                .any(|e| e.runs_browser_data_effect(residence)),
        }
    }

    /// True when this expression provably evaluates falsy — the same rule as
    /// `SExpression::is_statically_false`, through the canonical bridge.
    pub fn is_statically_false(&self) -> bool {
        self.to_sexpression().is_some_and(|s| s.is_statically_false())
    }

    pub fn calls_function(&self, function: &str) -> bool {
        match self {
            Self::Call {
                function: f, args, ..
            } => f == function || args.iter().any(|a| a.calls_function(function)),
            Self::Binding { .. }
            | Self::Null
            | Self::Bool { .. }
            | Self::Number { .. }
            | Self::String { .. } => false,
            Self::BinaryOp { left, right, .. } => {
                left.calls_function(function) || right.calls_function(function)
            }
            Self::UnaryOp { operand, .. } => operand.calls_function(function),
            Self::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                condition.calls_function(function)
                    || then_branch.calls_function(function)
                    || else_branch.calls_function(function)
            }
            Self::Map { source, body, .. } => {
                source.calls_function(function) || body.calls_function(function)
            }
            Self::Filter {
                source, predicate, ..
            } => source.calls_function(function) || predicate.calls_function(function),
            Self::Reduce {
                source,
                initial,
                body,
                ..
            } => {
                source.calls_function(function)
                    || initial.calls_function(function)
                    || body.calls_function(function)
            }
            Self::Array { items } => items.iter().any(|i| i.calls_function(function)),
            Self::Object { fields } => fields.values().any(|v| v.calls_function(function)),
            Self::Effect { effect } => effect
                .child_exprs()
                .iter()
                .any(|e| e.calls_function(function)),
            Self::Lambda { body, .. } => body.calls_function(function),
        }
    }

    /// Every direct child expression, descending into nested effects via
    /// `OirEffect::child_exprs` — the one enumeration walkers use to visit a
    /// whole expression tree (same coverage as `calls_function`). Exhaustive
    /// match: a new variant is compiler-forced to declare its children here.
    pub fn child_exprs(&self) -> Vec<&OirExpr> {
        match self {
            Self::Null
            | Self::Bool { .. }
            | Self::Number { .. }
            | Self::String { .. }
            | Self::Binding { .. } => Vec::new(),
            Self::Array { items } => items.iter().collect(),
            Self::Object { fields } => fields.values().collect(),
            Self::BinaryOp { left, right, .. } => vec![left, right],
            Self::UnaryOp { operand, .. } => vec![operand],
            Self::Conditional {
                condition,
                then_branch,
                else_branch,
            } => vec![condition, then_branch, else_branch],
            Self::Call { args, .. } => args.iter().collect(),
            Self::Map { source, body, .. } => vec![source, body],
            Self::Filter {
                source, predicate, ..
            } => vec![source, predicate],
            Self::Reduce {
                source,
                initial,
                body,
                ..
            } => vec![source, initial, body],
            Self::Lambda { body, .. } => vec![body],
            Self::Effect { effect } => effect.child_exprs(),
        }
    }

    pub fn collect_bindings_recursive<'a>(&'a self, bindings: &mut Vec<&'a OirBinding>) {
        match self {
            Self::Binding { binding } => bindings.push(binding),
            Self::BinaryOp { left, right, .. } => {
                left.collect_bindings_recursive(bindings);
                right.collect_bindings_recursive(bindings);
            }
            Self::UnaryOp { operand, .. } => {
                operand.collect_bindings_recursive(bindings);
            }
            Self::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                condition.collect_bindings_recursive(bindings);
                then_branch.collect_bindings_recursive(bindings);
                else_branch.collect_bindings_recursive(bindings);
            }
            Self::Call { args, .. } => {
                for arg in args {
                    arg.collect_bindings_recursive(bindings);
                }
            }
            Self::Map { source, body, .. } => {
                source.collect_bindings_recursive(bindings);
                body.collect_bindings_recursive(bindings);
            }
            Self::Filter {
                source, predicate, ..
            } => {
                source.collect_bindings_recursive(bindings);
                predicate.collect_bindings_recursive(bindings);
            }
            Self::Reduce {
                source,
                initial,
                body,
                ..
            } => {
                source.collect_bindings_recursive(bindings);
                initial.collect_bindings_recursive(bindings);
                body.collect_bindings_recursive(bindings);
            }
            Self::Array { items } => {
                for item in items {
                    item.collect_bindings_recursive(bindings);
                }
            }
            Self::Object { fields } => {
                for value in fields.values() {
                    value.collect_bindings_recursive(bindings);
                }
            }
            Self::Effect { effect } => {
                effect.collect_bindings_recursive(bindings);
            }
            // A lambda body closes over outer bindings (`@payload`, `@entity`,
            // `@user`) — collect them so scope-var detection (e.g.
            // `effects_use_payload`) sees a payload read nested inside a
            // `(array/map … (fn x … @payload.data …))`.
            Self::Lambda { body, .. } => {
                body.collect_bindings_recursive(bindings);
            }
            _ => {}
        }
    }
}

/// Binding to a context value
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirBinding {
    pub root: OirBindingRoot,
    #[serde(default)]
    pub path: Vec<String>,
}

impl OirBinding {
    /// Create a new binding
    pub fn new(root: OirBindingRoot, path: Vec<String>) -> Self {
        Self { root, path }
    }

    /// Create an entity binding
    pub fn entity(path: Vec<String>) -> Self {
        Self {
            root: OirBindingRoot::Entity,
            path,
        }
    }

    /// Create a payload binding
    pub fn payload(path: Vec<String>) -> Self {
        Self {
            root: OirBindingRoot::Payload,
            path,
        }
    }

    /// Parse a binding string like "@entity.name" or "@payload.id"
    pub fn parse(s: &str) -> Option<Self> {
        if !s.starts_with('@') {
            return None;
        }

        let s = &s[1..]; // Remove @
        let parts: Vec<&str> = s.split('.').collect();

        if parts.is_empty() {
            return None;
        }

        // `@trait.<TraitName>[.<slot>]` is special: the trait name is the
        // discriminator on the root variant, not a path segment. The optional
        // slot name is the only path segment.
        if parts[0] == "trait" && parts.len() >= 2 {
            let trait_name = parts[1].to_string();
            let path: Vec<String> = parts[2..].iter().map(|s| s.to_string()).collect();
            return Some(Self {
                root: OirBindingRoot::Trait(trait_name),
                path,
            });
        }

        // Render-resolved schema sigils — root-only, no path. A dotted
        // form like `@pages.foo` is NOT the sigil (it falls through to Local).
        if parts.len() == 1 {
            if parts[0] == "pages" {
                return Some(Self {
                    root: OirBindingRoot::Pages,
                    path: vec![],
                });
            }
            if parts[0] == "currentTheme" {
                return Some(Self {
                    root: OirBindingRoot::CurrentTheme,
                    path: vec![],
                });
            }
            if parts[0] == "locale" {
                return Some(Self {
                    root: OirBindingRoot::Locale,
                    path: vec![],
                });
            }
        }

        let root = match parts[0] {
            "entity" => OirBindingRoot::Entity,
            "payload" => OirBindingRoot::Payload,
            "state" => OirBindingRoot::State,
            "config" => OirBindingRoot::Config,
            "user" => OirBindingRoot::User,
            "event" => OirBindingRoot::Event,
            "prevEvents" => OirBindingRoot::PrevEvents,
            "prevStates" => OirBindingRoot::PrevStates,
            "fromState" => OirBindingRoot::FromState,
            "toState" => OirBindingRoot::ToState,
            other => OirBindingRoot::Local(other.to_string()),
        };

        let path = parts[1..].iter().map(|s| s.to_string()).collect();

        Some(Self { root, path })
    }
}

impl std::fmt::Display for OirBinding {
    /// Convert to binding string
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `@trait.<TraitName>[.<slot>]` serializes with the trait name as the
        // second segment; any path entries (slot name, at most one) come after.
        if let OirBindingRoot::Trait(trait_name) = &self.root {
            return if self.path.is_empty() {
                write!(f, "@trait.{}", trait_name)
            } else {
                write!(f, "@trait.{}.{}", trait_name, self.path.join("."))
            };
        }

        let root_str = match &self.root {
            OirBindingRoot::Entity => "entity",
            OirBindingRoot::Payload => "payload",
            OirBindingRoot::State => "state",
            OirBindingRoot::Config => "config",
            OirBindingRoot::User => "user",
            OirBindingRoot::Event => "event",
            OirBindingRoot::PrevEvents => "prevEvents",
            OirBindingRoot::PrevStates => "prevStates",
            OirBindingRoot::FromState => "fromState",
            OirBindingRoot::ToState => "toState",
            OirBindingRoot::Pages => return write!(f, "@pages"),
            OirBindingRoot::CurrentTheme => return write!(f, "@currentTheme"),
            OirBindingRoot::Locale => return write!(f, "@locale"),
            OirBindingRoot::Trait(_) => unreachable!("handled above"),
            OirBindingRoot::Local(name) => name,
        };

        if self.path.is_empty() {
            write!(f, "@{}", root_str)
        } else {
            write!(f, "@{}.{}", root_str, self.path.join("."))
        }
    }
}

/// Binding root type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OirBindingRoot {
    /// @entity
    Entity,
    /// @payload
    Payload,
    /// @state
    State,
    /// @config
    Config,
    /// @user — the authenticated viewer. One sigil only; the former
    /// `@currentUser` alias was retired (it resolved in this path and in no
    /// other, so an author using it got a working compiled app and an empty
    /// interpreted one).
    User,
    /// @pages — render-resolved: the host orbital's declared pages, as a
    /// `NavItem[]` (`href = page.path`, `label = page.name`). Config-default
    /// only (never appears in guards/effects/ticks); sourced from the loaded
    /// `OrbitalSchema` at render time. Preserved verbatim through OIR like
    /// `@trait` — the S-expression evaluator does NOT resolve it.
    Pages,
    /// @currentTheme — render-resolved: the host orbital's active theme key
    /// (precedence: `Orbital.theme` → app-level `OrbitalSchema.theme` →
    /// the baseline default). Config-default only; sourced from the loaded
    /// `OrbitalSchema` at render time. Preserved verbatim through OIR like
    /// `@trait`.
    CurrentTheme,
    /// @locale — the active locale code. Render-time: `orb validate` keeps it
    /// inside `render-ui`, where the client's I18nProvider supplies it.
    Locale,
    /// @event — the delivery being processed: `{ event, payload, source }` (Runtime Spec Clause 5.5).
    Event,
    /// @prevEvents — deliveries this trait already received this dispatch.
    PrevEvents,
    /// @prevStates — states this trait left this dispatch.
    PrevStates,
    /// @fromState — the transition's source state.
    FromState,
    /// @toState — the transition's target state.
    ToState,
    /// @trait.<TraitName>[.<slot>] — references another trait's current render-ui output.
    /// Client-only binding: compiler preserves verbatim through OIR, server passes through,
    /// client resolves at render time via `<TraitFrame>` in `@almadar/ui`.
    Trait(String),
    /// let-bound variable
    Local(String),
}

/// Binary operators
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirBinaryOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // Comparison
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    // Logical
    And,
    Or,
    // String
    Concat,
}

impl OirBinaryOp {
    /// Get the operator symbol
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
            Self::Concat => "++",
        }
    }

    /// Check if this is a comparison operator
    pub fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Gt | Self::Le | Self::Ge
        )
    }

    /// Check if this is a logical operator
    pub fn is_logical(&self) -> bool {
        matches!(self, Self::And | Self::Or)
    }

    /// Check if this is an arithmetic operator
    pub fn is_arithmetic(&self) -> bool {
        matches!(
            self,
            Self::Add | Self::Sub | Self::Mul | Self::Div | Self::Mod
        )
    }
}

/// Unary operators
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirUnaryOp {
    Not,
    Neg,
}

impl OirUnaryOp {
    /// Get the operator symbol
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Not => "!",
            Self::Neg => "-",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binding_parse() {
        let binding = OirBinding::parse("@entity.name").unwrap();
        assert_eq!(binding.root, OirBindingRoot::Entity);
        assert_eq!(binding.path, vec!["name"]);

        let binding = OirBinding::parse("@payload.user.id").unwrap();
        assert_eq!(binding.root, OirBindingRoot::Payload);
        assert_eq!(binding.path, vec!["user", "id"]);

        let binding = OirBinding::parse("@state").unwrap();
        assert_eq!(binding.root, OirBindingRoot::State);
        assert!(binding.path.is_empty());

        assert!(OirBinding::parse("entity.name").is_none());
    }

    #[test]
    fn test_binding_to_string() {
        let binding = OirBinding::entity(vec!["name".to_string()]);
        assert_eq!(binding.to_string(), "@entity.name");

        let binding = OirBinding::new(OirBindingRoot::State, vec![]);
        assert_eq!(binding.to_string(), "@state");
    }

    #[test]
    fn test_trait_binding_parse() {
        // `@trait.<TraitName>` — no slot
        let binding = OirBinding::parse("@trait.ItemSearch").unwrap();
        assert_eq!(
            binding.root,
            OirBindingRoot::Trait("ItemSearch".to_string())
        );
        assert!(binding.path.is_empty());

        // `@trait.<TraitName>.<slot>` — explicit slot
        let binding = OirBinding::parse("@trait.ItemSearch.main").unwrap();
        assert_eq!(
            binding.root,
            OirBindingRoot::Trait("ItemSearch".to_string())
        );
        assert_eq!(binding.path, vec!["main"]);

        // Bare `@trait` — no trait name, falls back to Local("trait") so
        // the validator can produce `BindingTraitMissingName` with context.
        let binding = OirBinding::parse("@trait").unwrap();
        assert_eq!(binding.root, OirBindingRoot::Local("trait".to_string()));
    }

    #[test]
    fn test_trait_binding_to_string_roundtrip() {
        let binding = OirBinding::new(OirBindingRoot::Trait("ItemSearch".to_string()), vec![]);
        assert_eq!(binding.to_string(), "@trait.ItemSearch");

        let binding = OirBinding::new(
            OirBindingRoot::Trait("ItemSearch".to_string()),
            vec!["main".to_string()],
        );
        assert_eq!(binding.to_string(), "@trait.ItemSearch.main");

        // Full round-trip: serialize → parse → serialize.
        let serialized = binding.to_string();
        let reparsed = OirBinding::parse(&serialized).unwrap();
        assert_eq!(reparsed, binding);
    }

    #[test]
    fn test_expr_constructors() {
        let expr = OirExpr::eq(
            OirExpr::entity_binding(vec!["status".to_string()]),
            OirExpr::string("active"),
        );
        assert!(matches!(
            expr,
            OirExpr::BinaryOp {
                op: OirBinaryOp::Eq,
                ..
            }
        ));
    }

    #[test]
    fn test_collect_bindings() {
        let expr = OirExpr::and(
            OirExpr::eq(
                OirExpr::entity_binding(vec!["status".to_string()]),
                OirExpr::string("active"),
            ),
            OirExpr::gt(
                OirExpr::entity_binding(vec!["count".to_string()]),
                OirExpr::number(0.0),
            ),
        );

        let bindings = expr.collect_bindings();
        assert_eq!(bindings.len(), 2);
    }

    /// `to_sexpression` must never emit a head the JS evaluator
    /// (`@almadar/evaluator`) does not implement. An unknown head there is not
    /// an error — it returns the array unchanged, which is truthy — so a guard
    /// or access policy built from one silently allows everything. `&&`, `||`,
    /// `++` and `!` are the four `symbol()` spellings with no JS operator.
    #[test]
    fn lowered_heads_are_all_operators_the_js_evaluator_implements() {
        use crate::schema::sexpr::SExpression as S;
        const NOT_JS_OPERATORS: [&str; 4] = ["&&", "||", "++", "!"];

        fn heads(expr: &S, out: &mut Vec<String>) {
            if let S::Array(items) = expr {
                if let Some(S::String(head)) = items.first() {
                    out.push(head.clone());
                }
                for item in items {
                    heads(item, out);
                }
            }
        }

        let cases = vec![
            OirExpr::and(
                OirExpr::Bool { value: true },
                OirExpr::Bool { value: false },
            ),
            OirExpr::or(
                OirExpr::Bool { value: true },
                OirExpr::Bool { value: false },
            ),
            OirExpr::not(OirExpr::Bool { value: true }),
            OirExpr::BinaryOp {
                op: OirBinaryOp::Concat,
                left: Box::new(OirExpr::String { value: "a".into() }),
                right: Box::new(OirExpr::String { value: "b".into() }),
            },
        ];
        for case in cases {
            let sexpr = case.to_sexpression().expect("has a guard form");
            let mut found = Vec::new();
            heads(&sexpr, &mut found);
            for head in found {
                assert!(
                    !NOT_JS_OPERATORS.contains(&head.as_str()),
                    "to_sexpression emitted `{head}`, which @almadar/evaluator does not \
                     implement — it would return the expression unchanged (truthy) and \
                     silently allow every row"
                );
            }
        }
    }
}
