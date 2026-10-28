//! Embedded JSON Registries
//!
//! Source-of-truth registries embedded directly into the orbital-compiler binary
//! at build time via `include_str!`. After Phase 3 of the Lolo v3 plan, the validator
//! reads operators, patterns, and behaviors from these embedded JSON sources rather
//! than hand-maintained Rust constants. Adding a new operator to `operators.json`
//! plus running `cargo build` is enough to update the validator.
//!
//! See `docs/Lolo_Language_Spec_v3_PLAN.md` Section 8 for context.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::PayloadTypeWhen;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
#[cfg(feature = "std")]
use std::path::PathBuf;
use crate::sync::OnceLock;

// ============================================================================
// 3.1 — canonical-operators.json (installed @almadar/std operator table)
// ============================================================================

/// The raw canonical-operators.json text, from the installed `@almadar/std`.
pub fn operators_json() -> &'static str {
    crate::language_data::operator_data_after_preflight()
        .operators
        .as_str()
}

/// Operator type reference, mirror of `OperatorTypeRef` in `packages/almadar-std/types.ts`.
///
/// A bare string names a concrete type (`"number"`, `"string"`, `"boolean"`, `"void"`, a std struct such as
/// `"tensor"`); everything else is a kind-discriminated object. There are no generic types: polymorphism is by
/// type variables ([`OperatorTypeRefKind::Var`]) bound per call. `#[serde(untagged)]` tries the tagged variants
/// by shape and falls back to `Primitive(String)` for bare strings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum OperatorTypeRef {
    /// Tagged shape (kind-discriminated object).
    Tagged(OperatorTypeRefKind),
    /// A concrete type named by a bare string, e.g. `"number"`, `"string"`, `"void"`, `"tensor"`.
    Primitive(String),
}

impl OperatorTypeRef {
    /// Whether null is a value of this type: a null literal or `null`, a union with such a member, an unevaluated
    /// expression whose value may be null, a type variable whose bound admits null (an unbounded one does), or a
    /// record's field (the field may be optional).
    pub fn admits_null(&self) -> bool {
        match self {
            OperatorTypeRef::Primitive(name) => matches!(name.as_str(), "null" | "sexpr"),
            OperatorTypeRef::Tagged(kind) => match kind {
                OperatorTypeRefKind::Literal { value } => value.is_null(),
                OperatorTypeRefKind::Union { of } => of.iter().any(OperatorTypeRef::admits_null),
                OperatorTypeRefKind::Var { bound, .. } => bound.as_ref().is_none_or(|b| b.admits_null()),
                OperatorTypeRefKind::Sexpr { of } => of.as_ref().is_none_or(|t| t.admits_null()),
                OperatorTypeRefKind::FieldOf { .. } => true,
                _ => false,
            },
        }
    }

    /// The coarse kind the signature declares, for readers that only branch on it: a primitive's own name, `array`
    /// (lists and tuples), `object` (records, shapes and maps), `function` (a function type), `union`, `var`,
    /// and the kind tag of everything else.
    pub fn kind_name(&self) -> &str {
        match self {
            OperatorTypeRef::Primitive(name) => name,
            OperatorTypeRef::Tagged(kind) => match kind {
                OperatorTypeRefKind::Union { .. } => "union",
                OperatorTypeRefKind::Literal { .. } => "literal",
                OperatorTypeRefKind::Array { .. } | OperatorTypeRefKind::Tuple { .. } => "array",
                OperatorTypeRefKind::Map { .. }
                | OperatorTypeRefKind::Object { .. }
                | OperatorTypeRefKind::Record
                | OperatorTypeRefKind::PickOf { .. }
                | OperatorTypeRefKind::OmitOf { .. }
                | OperatorTypeRefKind::MergeOf { .. }
                | OperatorTypeRefKind::ChildrenOf { .. } => "object",
                OperatorTypeRefKind::Entity { .. } => "entity",
                OperatorTypeRefKind::EntityRef => "entityRef",
                OperatorTypeRefKind::EventKey { .. } => "eventKey",
                OperatorTypeRefKind::UiSlot => "uiSlot",
                OperatorTypeRefKind::PatternType => "patternType",
                OperatorTypeRefKind::Binding { .. } => "binding",
                OperatorTypeRefKind::Sexpr { .. } => "sexpr",
                OperatorTypeRefKind::Var { .. } => "var",
                OperatorTypeRefKind::Fn { .. } => "function",
                OperatorTypeRefKind::KeyOf { .. } => "keyOf",
                OperatorTypeRefKind::FieldOf { .. } => "fieldOf",
                OperatorTypeRefKind::Shape => "shape",
            },
        }
    }
}

/// Tagged variants of `OperatorTypeRef`. Split from the outer enum so serde's
/// internally-tagged discriminator (`tag = "kind"`) has a fixed shape to match
/// against. The outer `#[serde(untagged)]` enum uses this for tagged input and
/// falls back to `Primitive` for bare strings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OperatorTypeRefKind {
    Union {
        of: Vec<OperatorTypeRef>,
    },
    Literal {
        value: serde_json::Value,
    },
    /// `[T]`
    Array {
        of: Box<OperatorTypeRef>,
    },
    /// `[A, B]`: a fixed-length array with a type per slot.
    Tuple {
        items: Vec<OperatorTypeRef>,
    },
    /// `Map K V`
    Map {
        key: Box<OperatorTypeRef>,
        value: Box<OperatorTypeRef>,
    },
    Object {
        #[serde(default)]
        fields: HashMap<String, OperatorTypeRef>,
        #[serde(default)]
        open: Option<bool>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        optional: Vec<String>,
    },
    Entity {
        #[serde(default)]
        collection: Option<String>,
    },
    #[serde(rename = "entityRef")]
    EntityRef,
    #[serde(rename = "eventKey")]
    EventKey {
        #[serde(default)]
        scope: Option<String>,
    },
    #[serde(rename = "uiSlot")]
    UiSlot,
    #[serde(rename = "patternType")]
    PatternType,
    Binding {
        #[serde(default)]
        shape: Option<Box<OperatorTypeRef>>,
    },
    /// An unevaluated expression; `of` is the type its value has once evaluated.
    Sexpr {
        #[serde(default)]
        of: Option<Box<OperatorTypeRef>>,
    },
    /// A type variable, bound per call. `bound` restricts what it may bind to; `each` binds it afresh at every
    /// argument it covers (a rest parameter).
    Var {
        name: String,
        #[serde(default)]
        bound: Option<Box<OperatorTypeRef>>,
        #[serde(default)]
        each: bool,
    },
    /// The constraint "a record" (a struct, a `Map K V` or an entity row); only valid as a variable `bound`.
    Record,
    /// `(P…) -> R`: a lambda parameter, or the value of `fn`.
    Fn {
        params: Vec<OperatorTypeRef>,
        returns: Box<OperatorTypeRef>,
    },
    /// `key-of R`: an argument naming a field of record variable `of` (a dot path through nested records).
    #[serde(rename = "keyOf")]
    KeyOf {
        of: String,
    },
    /// `field-of R`: the type of the field of `of` the call's `key-of` argument names.
    #[serde(rename = "fieldOf")]
    FieldOf {
        of: String,
    },
    /// `pick-of R`: `of` restricted to the fields the call's `[key-of R]` argument names.
    #[serde(rename = "pickOf")]
    PickOf {
        of: String,
    },
    /// `omit-of R`: `of` without the fields the call's `[key-of R]` argument names.
    #[serde(rename = "omitOf")]
    OmitOf {
        of: String,
    },
    /// `merge-of R`: the record whose fields are those of every binding of the `each` variable `of`, later winning.
    #[serde(rename = "mergeOf")]
    MergeOf {
        of: String,
    },
    /// The declared target shape of the call: an argument naming a type, which is also the result type.
    Shape,
    /// `children-of T R`: a structural fold's `childResults` over the record `of` (the call's shape): every field of
    /// `of` declared the shape, a list of it or a map of it, with the shape replaced by `result`.
    #[serde(rename = "childrenOf")]
    ChildrenOf {
        of: String,
        result: String,
    },
}

/// Effect-specific metadata on an operator, mirror of `OperatorEffectMeta` in
/// `packages/almadar-std/types.ts:168`. Populated only for operators with
/// `hasSideEffects: true` that opted into v2 effect metadata. `produces`
/// carries the payload the runtime puts on the bus when the operator's
/// `emit.success` fires; the V2 validator (P0.8) uses it to type-check
/// `emit: { success: "X" }` configs against declared event payloads.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedOperatorEffect {
    pub kind: String,
    #[serde(default)]
    pub produces: Option<OperatorTypeRef>,
    #[serde(default)]
    pub config: Option<HashMap<String, OperatorTypeRef>>,
}

/// One declared parameter of an operator: the type the evaluators check a present argument against.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedOperatorParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: OperatorTypeRef,
}

/// Metadata for a single operator parsed from `operators.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedOperator {
    pub category: String,
    #[serde(rename = "minArity")]
    pub min_arity: u32,
    #[serde(rename = "maxArity", default)]
    pub max_arity: Option<u32>,
    /// Declared parameters, by argument position; a last parameter named `...x` (or an unbounded `maxArity`) covers every further argument.
    #[serde(default)]
    pub params: Vec<EmbeddedOperatorParam>,
    #[serde(rename = "returnType")]
    pub return_type: OperatorTypeRef,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "hasSideEffects", default)]
    pub has_side_effects: bool,
    /// Input-dependent return-type tag (e.g. `union-of-args`); `None` for
    /// fixed-return operators.
    #[serde(rename = "returnSemantics", default)]
    pub return_semantics: Option<String>,
    /// Schema v2 effect metadata. Absent on operators that predate the v2
    /// uplift. The V2 validator consults `effect.produces` at effect call
    /// sites to check `emit: { success: "X" }` compatibility.
    #[serde(default)]
    pub effect: Option<EmbeddedOperatorEffect>,
    /// When true, the validator errors if the effect call site is missing
    /// a trailing options object with `emit.success` populated.
    ///
    /// ⚠️ **Currently set on NO operator**, so
    /// `effect::emit_declaration::validate_required_emit_success` returns
    /// immediately for every call and the pass is inert. This comment used to
    /// claim it was "set on fetch / persist / call-service / ref in
    /// canonical-operators.json" — it never was (verified 2026-08-30: zero
    /// occurrences of `requiresEmitSuccess` in that file).
    ///
    /// **Measured and REJECTED 2026-08-30 — do not enable without new
    /// evidence.** Switched on for fetch/persist/ref/call-service and scanned
    /// the whole corpus: **1,413 findings across 67 of 1,200 files.** Sampling
    /// them showed the flag's premise is simply false for this language. A
    /// bare `(fetch AppSearchTarget {})` — no emit at all, as in
    /// `std-app-search` — is the CORRECT idiom: the fetch loads the store and
    /// the reactive `@entity`-bound render picks it up, so there is no event to
    /// route and nothing is broken. Making that a hard error would force ~1,400
    /// ceremonial events into the corpus to satisfy a rule the language does
    /// not have.
    ///
    /// Kept (rather than deleted) because the field, the pass, and the registry
    /// slot are all still wired and someone may want the constraint for a
    /// narrower operator set later — but it is dormant on purpose, not by
    /// oversight. Tracked in `docs/Almadar_Std_Gaps.md` (B7).
    ///
    /// Generic flag — future "this op requires X" constraints can extend
    /// this struct with sibling booleans without writing new validator
    /// passes.
    #[serde(rename = "requiresEmitSuccess", default)]
    pub requires_emit_success: bool,
    /// The operator can fail on a well-typed value; it answers null then, never raising, so its result is optional
    /// (`T?`) and the program handles the missing value (owner ruling 2026-10-25).
    #[serde(default)]
    pub failable: Option<FailableOperator>,
    /// The sign of a numeric result (`sign` in the operator table): always at least zero (`nonNegative`), or derived
    /// from the arguments' signs (`sum`, `product`, `max`, `min`). The checker proves divisors and roots with it.
    #[serde(default)]
    pub sign: Option<ResultSign>,
    /// The event names the bus never delivers (`busExcluded`, on `emit`): a control or emit naming one fires nothing.
    #[serde(rename = "busExcluded", default)]
    pub bus_excluded: Vec<String>,
    /// The options that name a dispatch's target (`targetOptions`, on `program/dispatch`).
    #[serde(rename = "targetOptions", default)]
    pub target_options: Vec<String>,
    /// 0-indexed argument position of the operator's lambda/callback, when it
    /// takes one. The sexpr validator rejects a `(fn …)` form at any OTHER
    /// argument position — the decidable wrong-arg-order class
    /// (e.g. `(array/reduce arr (fn …) init)`).
    #[serde(rename = "lambdaArgPosition", default)]
    pub lambda_arg_position: Option<u32>,
    /// The regex (shared dialect) a `validate/<semantic type>` operator matches its value against: the one definition,
    /// owned by `@almadar/core` `SEMANTIC_STRING_PATTERNS` and carried by the operator table.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Execution site of an effect operator (`client`/`server`/`any`); absent on pure ones. Pinned to `crate::effects::runs_on` by `tests/runs_on_drift.rs`.
    #[serde(rename = "runsOn", default)]
    pub runs_on: Option<String>,
    /// Argument naming a data effect's target entity; the effect runs where that entity's rows live. Pinned to `crate::effects::ENTITY_ARGS` by `tests/runs_on_drift.rs`.
    #[serde(rename = "entityArgPosition", default)]
    pub entity_arg_position: Option<usize>,
    /// The host that executes the effect (`program`: the shared program host, `runProgramEffect`; `os`: the `os/` family under a declared grant); absent when the runtime executes it.
    #[serde(default)]
    pub host: Option<String>,
    /// What an `os/` or `kv/` call needs from the program's grants (`grant` in the operator table); `Some(vec![])` for
    /// a call on a handle, `None` on every other operator.
    #[serde(default)]
    pub grant: Option<Vec<crate::os_grants::OsGrantNeed>>,
    /// The `oauth-client` auth vocabulary (`oauth` in the operator table, on `http`).
    #[serde(default)]
    pub oauth: Option<EmbeddedOAuth>,
    /// Set on the host effects (`closedCall` in the operator table): the validator holds the call to its declared
    /// arity, options and request.
    #[serde(rename = "closedCall", default)]
    pub closed_call: Option<EmbeddedClosedCall>,
    /// The default port of each URL scheme the operator connects under (`defaultPorts` in the operator table).
    #[serde(rename = "defaultPorts", default)]
    pub default_ports: HashMap<String, u16>,
}

/// What a host effect's call is held to beyond its parameter types.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddedClosedCall {
    /// The first parameter is a request object: a literal one carries only its declared fields, every one not
    /// listed as optional present.
    #[serde(default)]
    pub request: bool,
    /// An integer literal at `arg` lies in `min..=max`.
    #[serde(rename = "intRange", default)]
    pub int_range: Option<EmbeddedIntRange>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct EmbeddedIntRange {
    pub arg: usize,
    pub min: i64,
    pub max: i64,
}

impl EmbeddedOperator {
    /// The trailing `options` parameter's field names (sorted) and its `emit` entry's field names (sorted): the keys a
    /// closed call may carry.
    pub fn option_keys(&self) -> Option<(Vec<&str>, Vec<&str>)> {
        let param = self.params.last().filter(|param| param.name == "options")?;
        let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = &param.ty else {
            return None;
        };
        let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut emit_keys: Vec<&str> = match fields.get("emit") {
            Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. })) => fields.keys().map(String::as_str).collect(),
            _ => Vec::new(),
        };
        emit_keys.sort_unstable();
        Some((keys, emit_keys))
    }

    /// The first parameter's field names (sorted) and those of them that are required: a request object's contract.
    pub fn request_fields(&self) -> Option<(Vec<&str>, Vec<&str>)> {
        let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, optional, .. }) = &self.params.first()?.ty else {
            return None;
        };
        let mut declared: Vec<&str> = fields.keys().map(String::as_str).collect();
        declared.sort_unstable();
        let required = declared.iter().copied().filter(|name| !optional.iter().any(|o| o == name)).collect();
        Some((declared, required))
    }
}

/// One `oauth-client` auth op: its input keys and the `oauth-endpoint` word it is sent to.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedOAuthOp {
    pub name: String,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    /// Absent for an op sent to a literal origin and path (`discovery`).
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// The `oauth-client` auth vocabulary the operator table declares on `http`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedOAuth {
    /// The word that stands where an origin goes; the URL comes from the client's metadata.
    #[serde(rename = "endpointTarget")]
    pub endpoint_target: String,
    #[serde(rename = "endpointKinds")]
    pub endpoint_kinds: Vec<String>,
    pub ops: Vec<EmbeddedOAuthOp>,
}

/// When a failable operator's result is not optional.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FailableOperator {
    /// Argument positions whose bad value is the only way to fail: with a literal at every one of them the call is
    /// checked when the program is validated, so its result is not optional.
    #[serde(rename = "unlessLiteral", default)]
    pub unless_literal: Vec<usize>,
    /// Argument positions whose value of the named type cannot make the call fail (an `instant` timestamp).
    #[serde(rename = "unlessTyped", default)]
    pub unless_typed: Vec<UnlessTyped>,
    /// Argument positions naming a nominal type (`as`'s `CallKey`): a conversion to it is checked where the program is
    /// validated, so the call cannot fail with a null.
    #[serde(rename = "unlessNominal", default)]
    pub unless_nominal: Vec<usize>,
    /// Argument positions whose value, typed non-optional, cannot make the call fail (`tree/fold` of a present value).
    #[serde(rename = "unlessPresent", default)]
    pub unless_present: Vec<usize>,
    /// Argument positions where a value proved nonzero (a divisor), at least zero (a square root's argument) or above
    /// zero (a logarithm's) cannot make the call fail.
    #[serde(rename = "unlessNonZero", default)]
    pub unless_non_zero: Vec<usize>,
    #[serde(rename = "unlessNonNegative", default)]
    pub unless_non_negative: Vec<usize>,
    #[serde(rename = "unlessPositive", default)]
    pub unless_positive: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResultSign {
    NonNegative,
    Sum,
    Product,
    Max,
    Min,
}

/// One argument position and the type that proves it cannot fail there.
#[derive(Debug, Clone, Deserialize)]
pub struct UnlessTyped {
    pub arg: usize,
    #[serde(rename = "type")]
    pub ty: String,
}

/// Metadata for an operator category.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedCategory {
    pub description: String,
    pub target: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OperatorsSchema {
    #[serde(default)]
    categories: HashMap<String, EmbeddedCategory>,
    operators: HashMap<String, EmbeddedOperator>,
}

pub fn parsed_operators() -> &'static OperatorsSchema {
    static PARSED: OnceLock<OperatorsSchema> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(operators_json())
            .expect("installed canonical-operators.json was validated at load")
    })
}

/// All operators keyed by name (e.g. `"+"`, `"render-ui"`, `"behavior/compose"`).
pub fn operators() -> &'static HashMap<String, EmbeddedOperator> {
    &parsed_operators().operators
}

/// Every slot an effect's `emit` options map declares in the operator table: the names an emit map may route a
/// result to.
pub fn emit_slots() -> &'static HashSet<String> {
    static SLOTS: OnceLock<HashSet<String>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let mut slots = HashSet::new();
        for op in operators().values() {
            for param in &op.params {
                if let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = &param.ty {
                    if let Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields: emit, .. })) = fields.get("emit") {
                        slots.extend(emit.keys().cloned());
                    }
                }
            }
        }
        slots
    })
}

/// The default port the installed operator table declares for a raw TCP URL scheme (`redis`, `postgres`, …); `None`
/// for any other scheme, or before a table is installed.
pub fn tcp_scheme_default_port(scheme: &str) -> Option<u16> {
    static PORTS: OnceLock<HashMap<String, u16>> = OnceLock::new();
    crate::language_data::operator_data().ok()?;
    PORTS
        .get_or_init(|| operators().values().flat_map(|op| op.default_ports.iter().map(|(scheme, port)| (scheme.clone(), *port))).collect())
        .get(scheme)
        .copied()
}

/// The host effects the installed operator table holds to a closed call shape (`closedCall`).
pub fn closed_call_effects() -> impl Iterator<Item = &'static str> {
    operators().iter().filter(|(_, op)| op.closed_call.is_some()).map(|(name, _)| name.as_str())
}

/// The installed operator table's `oauth-client` vocabulary (declared on `http`).
pub fn oauth_vocabulary() -> &'static EmbeddedOAuth {
    get_operator("http")
        .and_then(|op| op.oauth.as_ref())
        .expect("the operator table declares the oauth-client vocabulary on `http`")
}

/// The keys of an OAuth client object, sorted: the fields of the `client` parameter of `oauth-authorize`.
pub fn oauth_client_keys() -> Vec<&'static str> {
    let param = get_operator("oauth-authorize")
        .and_then(|op| op.params.iter().find(|p| p.name == "client"))
        .expect("the operator table declares the `client` parameter of `oauth-authorize`");
    let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = &param.ty else {
        panic!("the `client` parameter of `oauth-authorize` is an object");
    };
    let mut keys: Vec<&'static str> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

/// Whether the installed operator table gives `name` an options parameter with an `emit` entry: the operators whose
/// results route to events. `false` for a name the table does not know, or before it is loaded.
pub fn takes_emit_options(name: &str) -> bool {
    try_get_operator(name).is_some_and(|op| {
        op.params.iter().any(|param| {
            matches!(&param.ty, OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) if fields.contains_key("emit"))
        })
    })
}

/// What a kernel reads at dispatch from the operator table: the events the bus never delivers (`emit` `busExcluded`)
/// and the options that name a dispatch's target (`program/dispatch` `targetOptions`). A browser kernel has no table, so
/// a compiled client program carries them (`_metadata.dispatchFacts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchFacts {
    pub bus_excluded: Vec<String>,
    pub target_options: Vec<String>,
}

static PROGRAM_DISPATCH_FACTS: OnceLock<DispatchFacts> = OnceLock::new();

/// The dispatch facts of the installed operator table; `None` when no table is installed.
pub fn table_dispatch_facts() -> Option<&'static DispatchFacts> {
    static TABLE: OnceLock<DispatchFacts> = OnceLock::new();
    crate::language_data::operator_data().ok()?;
    Some(TABLE.get_or_init(|| {
        let list = |op: &str, read: fn(&EmbeddedOperator) -> &Vec<String>| {
            get_operator(op).map(read).cloned().expect("the operator table declares its dispatch facts (checked at load)")
        };
        DispatchFacts {
            bus_excluded: list("emit", |op| &op.bus_excluded),
            target_options: list("program/dispatch", |op| &op.target_options),
        }
    }))
}

/// Install the dispatch facts a compiled program carries; refuses facts that differ from ones already installed.
pub fn install_dispatch_facts(facts: DispatchFacts) -> Result<(), String> {
    match PROGRAM_DISPATCH_FACTS.get() {
        Some(installed) if *installed != facts => Err(format!(
            "dispatchFacts: the program carries {facts:?}, but this kernel already runs a program with {installed:?}"
        )),
        Some(_) => Ok(()),
        None => {
            let _ = PROGRAM_DISPATCH_FACTS.set(facts);
            Ok(())
        }
    }
}

/// The installed operator table's dispatch facts, else those a registered program carried.
pub fn dispatch_facts() -> Option<&'static DispatchFacts> {
    table_dispatch_facts().or_else(|| PROGRAM_DISPATCH_FACTS.get())
}

/// All operator categories.
pub fn categories() -> &'static HashMap<String, EmbeddedCategory> {
    &parsed_operators().categories
}

/// Returns true if `name` is a known operator from `operators.json`.
pub fn is_known_operator(name: &str) -> bool {
    operators().contains_key(name)
}

/// Whether bare `name` is the basename of a namespaced operator (`delete` of `kv/delete`, `join` of `str/join`):
/// a list headed by it reads like a call with its namespace dropped.
pub fn names_operator_basename(name: &str) -> bool {
    !name.contains('/') && operators().keys().any(|op| op.rsplit_once('/').is_some_and(|(_, base)| base == name))
}

/// Look up the metadata for an operator by name.
pub fn get_operator(name: &str) -> Option<&'static EmbeddedOperator> {
    operators().get(name)
}

/// [`get_operator`] for callers that can run before the installed operator table is loaded: `None` until it is.
pub fn try_get_operator(name: &str) -> Option<&'static EmbeddedOperator> {
    crate::language_data::operator_data().ok()?;
    get_operator(name)
}

/// Whether the installed operator table routes `name` to the shared program host (`host: "program"`).
pub fn is_program_host_effect(name: &str) -> bool {
    try_get_operator(name).is_some_and(|op| op.host.as_deref() == Some("program"))
}

/// Every effect the installed operator table routes to the shared program host.
pub fn program_host_effects() -> impl Iterator<Item = &'static str> {
    operators()
        .iter()
        .filter(|(_, op)| op.host.as_deref() == Some("program"))
        .map(|(name, _)| name.as_str())
}

/// Every effect the installed operator table routes to the OS host (`host: "os"`).
pub fn os_host_effects() -> impl Iterator<Item = &'static str> {
    operators()
        .iter()
        .filter(|(_, op)| op.host.as_deref() == Some("os"))
        .map(|(name, _)| name.as_str())
}

/// Returns true if the operator category targets Rust (or if unknown).
pub fn is_rust_target(name: &str) -> bool {
    is_shell_target(name, "rust")
}

/// Returns true if the operator's canonical category targets Python.
pub fn is_python_target(name: &str) -> bool {
    is_shell_target(name, "python")
}

/// Returns true if the operator's canonical category lists `shell` in its
/// `target` set. The single lookup behind the per-shell helpers so the
/// operator→category→target resolution can never drift between shells.
pub fn is_shell_target(name: &str, shell: &str) -> bool {
    if let Some(op) = get_operator(name) {
        if let Some(cat) = categories().get(&op.category) {
            return cat.target.iter().any(|t| t == shell);
        }
    }
    false
}

// ============================================================================
// 3.2 — patterns-registry.json (installed @almadar/core language data)
// ============================================================================

mod patterns;
mod services;
mod binding_contexts;
pub use binding_contexts::{
    allowed_sigils_for_context, binding_context_rules, known_contexts, BindingContextRule,
};
pub use patterns::{
    builtin_theme_names, get_pattern, installed_behavior_index_contains, is_known_behavior,
    is_known_pattern, package_behavior_names, patterns, patterns_json, EmbeddedCallbackArg,
    EmbeddedPattern, EmbeddedPatternProp, EmbeddedPayloadField, RequiredFlag, PATTERNS,
};
pub use services::{
    baked_service_actions, baked_service_credentials, baked_service_registry, lolo_spelling,
    parse_service_registry_data, response_shape_fields, services_json, EmbeddedParamItems,
    EmbeddedParamMember, EmbeddedServiceCredential, EmbeddedServiceParam, ResponseShapeField,
    ServiceRegistryData, EVENT_ADDRESS_TYPE,
};

#[cfg(test)]
mod service_runtimes_tests;

#[cfg(test)]
mod tests;
