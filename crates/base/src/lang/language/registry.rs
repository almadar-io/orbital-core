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
    /// The kernel raises when this operand is null (`rejectsNull` in the operator table); `orb validate` refuses a value
    /// that can be missing here.
    #[serde(rename = "rejectsNull", default)]
    pub rejects_null: bool,
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

/// The raw patterns-registry.json text, from the installed `@almadar/core`.
pub fn patterns_json() -> &'static str {
    crate::language_data::language_data_after_preflight()
        .patterns
        .as_str()
}

// ============================================================================
// 3.1b — Built-in theme names, from the installed @almadar/core presets
// ============================================================================

/// The `[data-theme]` keys of every installed `@almadar/core` preset
/// (`<name>-light` / `<name>-dark`, the selectors `@almadar/ui`'s generated
/// theme CSS defines). These (plus any orbital-declared inline theme names)
/// form the valid-theme set the theme-reference validator accepts.
pub fn builtin_theme_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| crate::theme::ThemePresets::installed().theme_keys())
}

// ============================================================================
// 3.1c — Embedded services-registry.json (call-service integrator contracts)
// ============================================================================

/// The raw services-registry.json text, from the installed `@almadar/core`.
pub fn services_json() -> &'static str {
    crate::language_data::language_data_after_preflight()
        .services
        .as_str()
}

/// Service name → declared action names, from the baked services registry.
/// A declared-only service (host-injected handler, no static contract) is
/// present with an EMPTY set — its name is known, its actions are unchecked.
///
/// Read from the installed `@almadar/core` (`crate::language_data`).
pub fn baked_service_actions() -> &'static HashMap<String, HashSet<String>> {
    &baked_service_registry().actions
}

/// One declared parameter of a service action, from the services registry.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedServiceParam {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    /// An array param's element shape: its members' declared types.
    #[serde(default)]
    pub items: Option<EmbeddedParamItems>,
    /// A struct param's members.
    #[serde(default)]
    pub properties: Option<HashMap<String, EmbeddedParamMember>>,
    /// A dictionary param's value type.
    #[serde(default, rename = "mapValue")]
    pub map_value: Option<EmbeddedParamMember>,
}

/// The element shape of an array service param.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddedParamItems {
    #[serde(default)]
    pub properties: HashMap<String, EmbeddedParamMember>,
}

/// One member of an array param's element.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbeddedParamMember {
    #[serde(default, rename = "type")]
    pub member_type: String,
}

/// The `@almadar/core` `EventAddress` contract type: a `listens`-addressed
/// event value, checked against the program's declared external inputs.
pub const EVENT_ADDRESS_TYPE: &str = "EventAddress";

/// The validation projections of the services registry: declared action names
/// plus per-action param contracts. One parse, one source — the CLI's live
/// CWD-registry load and this baked fallback both produce it via
/// [`parse_service_registry_data`], so the action gate and the param checks
/// (I-13) can never draw from two different registry generations.
#[derive(Debug, Clone, Default)]
pub struct ServiceRegistryData {
    /// Service name → declared action names. A declared-only service
    /// (host-injected handler, no static contract) carries an EMPTY set —
    /// its name is known, its actions are unchecked.
    pub actions: HashMap<String, HashSet<String>>,
    /// Service name → action name → declared params.
    pub params: HashMap<String, HashMap<String, Vec<EmbeddedServiceParam>>>,
    /// Service name → the runtimes its implementation declares (`node`, `browser`).
    /// A service without the field declares none.
    pub runtimes: HashMap<String, Vec<String>>,
    /// Service name → action name → the fields of the action's `responseShape`
    /// record, sorted by name. An action with no (or an empty) `responseShape`
    /// is absent: its result is not a known record.
    pub response_shapes: HashMap<String, HashMap<String, Vec<ResponseShapeField>>>,
}

/// One field of a service action's `responseShape` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseShapeField {
    pub name: String,
    pub field_type: String,
}

impl ServiceRegistryData {
    /// The fields of `service.action`'s `responseShape`, or `None` when the
    /// action declares none.
    pub fn response_shape(&self, service: &str, action: &str) -> Option<&[ResponseShapeField]> {
        self.response_shapes
            .get(service)?
            .get(action)
            .map(Vec::as_slice)
    }
}

/// The registry is written from TS contracts: its list type `T[]` is the `.lolo` `[T]`.
/// A TS string-literal union `'a' | 'b'` is the `.lolo` `"a" | "b"`.
pub fn lolo_spelling(t: &str) -> String {
    match t.strip_suffix("[]").filter(|base| !base.is_empty()) {
        Some(base) => {
            let element = base
                .strip_prefix('(')
                .and_then(|b| b.strip_suffix(')'))
                .unwrap_or(base);
            format!("[{}]", lolo_spelling(element))
        }
        None => literal_union_spelling(t).unwrap_or_else(|| t.to_string()),
    }
}

fn literal_union_spelling(t: &str) -> Option<String> {
    t.split(" | ")
        .map(|arm| {
            arm.strip_prefix('\'')
                .and_then(|a| a.strip_suffix('\''))
                .filter(|inner| !inner.contains(['\'', '"', '\\']))
                .map(|inner| format!("\"{inner}\""))
        })
        .collect::<Option<Vec<_>>>()
        .map(|arms| arms.join(" | "))
}

pub fn response_shape_fields(
    shape: &serde_json::Map<String, serde_json::Value>,
) -> Vec<ResponseShapeField> {
    let mut fields: Vec<ResponseShapeField> = shape
        .iter()
        .map(|(name, value)| ResponseShapeField {
            name: name.clone(),
            field_type: match value {
                serde_json::Value::String(t) => lolo_spelling(t),
                serde_json::Value::Object(o) => o
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| "any".to_string(), lolo_spelling),
                _ => "any".to_string(),
            },
        })
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

/// Parse raw services-registry.json text into both validation projections.
pub fn parse_service_registry_data(raw: &str) -> Result<ServiceRegistryData, serde_json::Error> {
    #[derive(Deserialize)]
    struct ActionEntry {
        name: String,
        #[serde(default)]
        params: Vec<EmbeddedServiceParam>,
        #[serde(default, rename = "responseShape")]
        response_shape: Option<serde_json::Map<String, serde_json::Value>>,
    }
    #[derive(Deserialize)]
    struct ServiceEntry {
        #[serde(default)]
        actions: Vec<ActionEntry>,
        #[serde(default)]
        runtimes: Vec<String>,
    }
    #[derive(Deserialize)]
    struct ServicesRegistry {
        #[serde(default)]
        services: HashMap<String, ServiceEntry>,
    }
    let registry: ServicesRegistry = serde_json::from_str(raw)?;
    let mut data = ServiceRegistryData::default();
    for (name, entry) in registry.services {
        let mut actions = HashSet::new();
        let mut params = HashMap::new();
        let mut shapes = HashMap::new();
        for a in entry.actions {
            actions.insert(a.name.clone());
            if let Some(shape) = a.response_shape.filter(|s| !s.is_empty()) {
                shapes.insert(a.name.clone(), response_shape_fields(&shape));
            }
            params.insert(a.name, a.params);
        }
        data.response_shapes.insert(name.clone(), shapes);
        data.actions.insert(name.clone(), actions);
        data.params.insert(name.clone(), params);
        data.runtimes.insert(name, entry.runtimes);
    }
    Ok(data)
}

/// The installed services registry, parsed once.
pub fn baked_service_registry() -> &'static ServiceRegistryData {
    static DATA: OnceLock<ServiceRegistryData> = OnceLock::new();
    DATA.get_or_init(|| {
        let raw = services_json();
        parse_service_registry_data(raw)
            .expect("installed services-registry.json is corrupt — reinstall @almadar/core")
    })
}

/// One credential env var a service declares in the services registry.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedServiceCredential {
    #[serde(rename = "envVar")]
    pub env_var: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

/// Service name → declared credential env vars, from the baked services
/// registry. Drives the generated `.env.example` / `SECRETS.md` manifest for
/// the services a schema actually invokes.
pub fn baked_service_credentials() -> &'static HashMap<String, Vec<EmbeddedServiceCredential>> {
    static MAP: OnceLock<HashMap<String, Vec<EmbeddedServiceCredential>>> = OnceLock::new();
    MAP.get_or_init(|| {
        #[derive(Deserialize)]
        struct ServiceEntry {
            #[serde(default)]
            credentials: Vec<EmbeddedServiceCredential>,
        }
        #[derive(Deserialize)]
        struct ServicesRegistry {
            #[serde(default)]
            services: HashMap<String, ServiceEntry>,
        }
        let raw = services_json();
        let registry: ServicesRegistry = serde_json::from_str(raw)
            .expect("installed services-registry.json is corrupt — reinstall @almadar/core");
        registry
            .services
            .into_iter()
            .map(|(name, entry)| (name, entry.credentials))
            .collect()
    })
}

#[cfg(test)]
mod service_runtimes_tests {
    use super::parse_service_registry_data;

    #[test]
    fn reads_each_services_declared_runtimes_in_the_same_parse_as_its_actions() {
        let raw = r#"{"services":{"page":{"runtimes":["browser"],"actions":[{"name":"watch"}]},"llm":{"runtimes":["node","browser"]}}}"#;
        let data = parse_service_registry_data(raw).unwrap();
        assert_eq!(data.runtimes["page"], vec!["browser".to_string()]);
        assert_eq!(
            data.runtimes["llm"],
            vec!["node".to_string(), "browser".to_string()]
        );
        assert!(data.actions["page"].contains("watch"));
    }

    #[test]
    fn edge_a_service_without_the_field_declares_no_runtime() {
        let data = parse_service_registry_data(r#"{"services":{"old":{}}}"#).unwrap();
        assert!(data.runtimes["old"].is_empty());
    }
}

/// A single field of an emit/listen payload schema, mirroring trait
/// `payloadSchema` entries so L1/L2 consumers can compare the component's
/// documented bus shape against the trait's declared `emits {}` payload.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPayloadField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    /// Declared knob-conditional type list (component JSDoc's
    /// `@payloadTypeWhen`, pattern-synced into the registry as `typeWhen`).
    /// The render-ui binding check (`phases/validation/render_ui/binding.rs`)
    /// evaluates this against the consuming trait's own resolved config —
    /// the SAME `resolve_payload_type_when` the inline narrowing pass uses.
    #[serde(rename = "typeWhen", default)]
    pub type_when: Option<Vec<PayloadTypeWhen>>,
    /// `event` when the member's TS type is `EventKey`.
    #[serde(default)]
    pub kind: Option<String>,
    /// `@payloadFor <sibling>`: the event-kind sibling this member carries the payload of.
    #[serde(rename = "payloadFor", default)]
    pub payload_for: Option<String>,
}

/// A single positional arg of a React callback prop. Used by L2 to verify
/// name-and-type parity between the callback and the trait's declared event
/// payload (gap #32).
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedCallbackArg {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    /// Nested structural shape for an object-typed arg (e.g. a canvas
    /// `onShapeClick` payload `{ id?, type?, index }`). The pattern-sync
    /// parser has always written this into `patterns-registry.json`; this
    /// struct used to drop it, forcing listen contracts down to a shapeless
    /// `payload : object`. Reuses `EmbeddedPatternProp` — the same
    /// `types`/`properties`/`required` nesting the JSON carries.
    #[serde(default)]
    pub schema: Option<Box<EmbeddedPatternProp>>,
}

/// JSON-schema-style `required` field — accepts either a boolean (used at
/// the pattern-prop top level, e.g. `columns: { required: true }`) OR an
/// array of property names (used at the nested-object level, e.g.
/// `items: { properties: { id: { types: ["string"] } }, required: ["id"] }`).
/// Pattern-sync writes both shapes; consumers use helpers below to read.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RequiredFlag {
    Bool(bool),
    Names(Vec<String>),
}

impl Default for RequiredFlag {
    fn default() -> Self {
        RequiredFlag::Bool(false)
    }
}

impl RequiredFlag {
    /// Whether the property carrying this flag is required ON ITS PARENT object —
    /// the `Bool(true)` form ONLY. The `Names([...])` form means something
    /// different ("these sub-keys are required WITHIN this property's own object,
    /// if the property is present"), read via [`as_names`](Self::as_names); it must
    /// NOT make the property itself parent-required. (The old `as_bool` conflated
    /// the two — `Names` non-empty → true — which wrongly demanded e.g. an optional
    /// `relation` on every `form-section` field.)
    pub fn required_on_parent(&self) -> bool {
        matches!(self, RequiredFlag::Bool(true))
    }

    /// Read as a slice of field names — the `Names([...])` form's required
    /// sub-keys; empty for the bool form.
    pub fn as_names(&self) -> &[String] {
        match self {
            RequiredFlag::Bool(_) => &[],
            RequiredFlag::Names(v) => v.as_slice(),
        }
    }
}

/// A single prop entry in a pattern's `propsSchema`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPatternProp {
    #[serde(default)]
    pub types: Vec<String>,
    /// Bounded string-literal union members (e.g. `VisualStyle`, button
    /// `variant`), present at any nesting level — top prop OR a nested
    /// `properties`/`items` field. Codegen consults this to drop an empty
    /// enum sub-field instead of emitting an invalid `""` (CG-SWEEP-1 Cat 11).
    #[serde(rename = "enumValues", default)]
    pub enum_values: Option<Vec<String>>,
    #[serde(default)]
    pub required: RequiredFlag,
    #[serde(default)]
    pub description: Option<String>,
    /// Optional marker for SCALAR props whose value is a field name on the
    /// linked entity. Currently the only recognised value is `"field-ref"`.
    /// See `docs/Almadar_Agent_Behaviors_Plan.md` Phase 3.2/3.3.
    ///
    /// Pattern-sync also writes the F1 plan's event-related kinds here:
    /// `"event"` (legacy, EventKey alias), `"event-ref"` (declarative
    /// bus-emit prop, sourced from `EventEmit<P>`), `"event-listen"`
    /// (`EventListen<P>` mirror), `"event-list"` (array prop whose elements
    /// carry an event-name field), `"event-map"` (dynamic-key object prop
    /// whose VALUES — not the prop itself — are declared event keys, e.g. a
    /// d-pad's direction→event map or a canvas's keycode→event map; the
    /// Record/index-signature value type resolved to `EventKey`),
    /// `"callback"` (React callback prop bound to an event name at the
    /// schema-author site — NEVER set for a render-prop that returns a
    /// value, see `renderCallback` below).
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional marker for ARRAY props whose string elements are field names
    /// on the linked entity. Currently the only recognised value is
    /// `"field-ref"`. Non-string array elements are ignored by the resolver.
    #[serde(rename = "itemKind", default)]
    pub item_kind: Option<String>,
    /// For `kind: "event-list"`: which field inside each array item holds
    /// the event key. Default is `"event"` — set explicitly when the
    /// descriptor type uses a different field name.
    #[serde(rename = "eventField", default)]
    pub event_field: Option<String>,
    /// `@notification`: the component reports a change it already applied (hover, close
    /// finished, resize), so a final program may leave the event unhandled (G-ORB-187).
    #[serde(default)]
    pub notification: bool,
    /// For `kind: "event-ref"` whose source is `EventEmit<P>`: the
    /// structural shape of `P`, as the bus payload the component fires
    /// when the prop is bound. Validator (L2.2) compares this against the
    /// trait's declared `emits { EVENT { ... } }` payload.
    #[serde(rename = "emitPayloadSchema", default)]
    pub emit_payload_schema: Option<Vec<EmbeddedPayloadField>>,
    /// For `kind: "event-listen"` whose source is `EventListen<P>`: the
    /// structural shape of `P`, as the payload the component subscribes to.
    #[serde(rename = "listenPayloadSchema", default)]
    pub listen_payload_schema: Option<Vec<EmbeddedPayloadField>>,
    /// For `kind: "callback"`: positional parameter list of the function
    /// type. Validator uses the names to build the param-name → payload-
    /// field map for the C2 codegen + runtime callback wrap.
    #[serde(rename = "callbackArgs", default)]
    pub callback_args: Option<Vec<EmbeddedCallbackArg>>,
    /// For array-typed props: per-element shape. Carried for the
    /// `ORB_RUI_ELEMENT_SHAPE_MISMATCH` rule so the validator can check
    /// each element of a bound array literal against the component's
    /// required element fields (e.g. `MarketingFooter.columns` →
    /// `items.properties = { title: required, items: required }`).
    #[serde(default)]
    pub items: Option<Box<EmbeddedPatternProp>>,
    /// For object-typed props OR for elements of `items`: per-field shape.
    /// Required fields here are enforced by the element-shape rule against
    /// the lolo literal's keys.
    #[serde(default)]
    pub properties: Option<HashMap<String, EmbeddedPatternProp>>,
    /// Entity-row clean slate: when a prop's TS type is `EntityWith<K>`,
    /// pattern-sync writes the keys `K` here. The bound entity (or a remap
    /// field-map) MUST provide every one of these fields, else the component
    /// renders blank rows. Enforced by `ORB_X_ENTITY_PROP_CONTRACT`.
    #[serde(rename = "requiredFields", default)]
    pub required_fields: Option<Vec<String>>,
    /// `@payloadFor <sibling>` pairing read off THIS struct member (not an
    /// enclosing prop — a struct member declared inside an array-item
    /// interface, e.g. `FloatingToolbarItem.actionPayload`, has no parent
    /// `ExtractedProp` to surface it on). Names a sibling key in this SAME
    /// `properties` map, itself carrying `kind: "event"`. The render-ui
    /// binding check (`check_callback_arg_schema`) uses this pair to derive
    /// the correct expected type (`event` / `@payload(.sibling)`) instead of
    /// falling back to `types.first()` ("string"/"object"), which produced a
    /// false `ORB_RUI_EVENT_BINDING_PAYLOAD_MISMATCH` against the trait's
    /// honestly-declared dependent type.
    #[serde(rename = "payloadFor", default)]
    pub payload_for: Option<String>,
}

/// A single pattern entry from `patterns-registry.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddedPattern {
    #[serde(rename = "type", default)]
    pub pattern_type: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "propsSchema", default)]
    pub props_schema: HashMap<String, EmbeddedPatternProp>,
    #[serde(rename = "entityAware", default)]
    pub entity_aware: bool,
    /// `form` for a pattern that collects field values and submits them (`form`, `form-section`).
    #[serde(rename = "fieldsContract", default)]
    pub fields_contract: Option<String>,
}

pub static PATTERNS: OnceLock<HashMap<String, EmbeddedPattern>> = OnceLock::new();

/// All patterns keyed by pattern type (e.g. `"table-view"`, `"button"`).
pub fn patterns() -> &'static HashMap<String, EmbeddedPattern> {
    PATTERNS.get_or_init(|| {
        // patterns-registry.json shape: { version, exportedAt, patterns: { name: PatternEntry } }
        let parsed: serde_json::Value = serde_json::from_str(patterns_json())
            .expect("installed patterns-registry.json is corrupt — reinstall @almadar/core");
        let patterns_obj = parsed
            .get("patterns")
            .and_then(|v| v.as_object())
            .expect("patterns-registry.json missing 'patterns' object at root");
        let mut map = HashMap::new();
        for (name, value) in patterns_obj {
            if let Ok(p) = serde_json::from_value::<EmbeddedPattern>(value.clone()) {
                map.insert(name.clone(), p);
            }
        }
        map
    })
}

/// Returns true if `pattern_type` is a known pattern.
pub fn is_known_pattern(pattern_type: &str) -> bool {
    patterns().contains_key(pattern_type)
}

/// Look up a pattern entry by type.
pub fn get_pattern(pattern_type: &str) -> Option<&'static EmbeddedPattern> {
    patterns().get(pattern_type)
}

// ============================================================================
// 3.3 — Known-behavior index (every behavior package is installed, not baked)
// ============================================================================
//
// `@almadar/std` and `@almadar-io/behaviors` (and any other behavior package)
// are installed, not baked: a name is known when an installed package ships a
// behavior file of that name (`behavior_index::indexed_names`).

#[cfg(feature = "std")]
/// Returns true if `name` is a behavior an installed behavior package ships. This
/// call site has no `LoaderConfig`/`packages` to reuse, so it re-discovers from
/// `current_dir()` / the user store, same as `CompilerOptions::from_env`.
pub fn is_known_behavior(name: &str) -> bool {
    installed_behavior_index_contains(name)
}

#[cfg(feature = "std")]
/// True when `name` is shipped by any installed behavior package. Package discovery +
/// each package's names are memoized per registry root.
pub fn installed_behavior_index_contains(name: &str) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let user_store = crate::behavior_packages::user_store_dir().unwrap_or_else(|| cwd.clone());
    let Ok(packages) = crate::behavior_packages::discover(&cwd, &user_store) else {
        return false;
    };
    packages
        .iter()
        .any(|pkg| package_behavior_names(pkg).contains(name))
}

#[cfg(feature = "std")]
pub fn package_behavior_names(pkg: &crate::behavior_packages::BehaviorPackage) -> HashSet<String> {
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, HashSet<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = pkg.registry.root().to_path_buf();
    if let Some(names) = cache
        .lock()
        .expect("package behavior name cache poisoned")
        .get(&key)
    {
        return names.clone();
    }
    let names = crate::behavior_index::indexed_names(pkg).unwrap_or_default();
    cache
        .lock()
        .expect("package behavior name cache poisoned")
        .insert(key, names.clone());
    names
}

// ============================================================================
// 3.4 — Sigil context rules (BINDING_CONTEXT_RULES)
// ============================================================================

#[derive(serde::Deserialize)]
pub struct BindingContextRule {
    allowed: Vec<String>,
}

/// `BINDING_CONTEXT_RULES`, read from the installed `@almadar/core`
/// (`orbLanguage.bindingRules`).
pub fn binding_context_rules() -> &'static HashMap<String, Vec<String>> {
    static PARSED: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let rules: HashMap<String, BindingContextRule> = serde_json::from_str(
            &crate::language_data::language_data_after_preflight().binding_rules,
        )
        .expect("installed binding-rules.json is corrupt — reinstall @almadar/core");
        rules.into_iter().map(|(k, v)| (k, v.allowed)).collect()
    })
}

/// Look up the allowed sigil roots for a given context name.
pub fn allowed_sigils_for_context(context: &str) -> Option<&'static [String]> {
    binding_context_rules().get(context).map(Vec::as_slice)
}

/// All known sigil context names.
pub fn known_contexts() -> HashSet<&'static str> {
    binding_context_rules().keys().map(String::as_str).collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contract_literal_union_reads_in_the_lolo_spelling_and_fits_a_string() {
        let union = lolo_spelling("'beginner' | 'intermediate' | 'advanced'");
        assert_eq!(union, r#""beginner" | "intermediate" | "advanced""#);
        assert!(crate::type_compat::set_value_type_compatible(
            "string", &union
        ));
        assert_eq!(
            lolo_spelling("('solo' | 'teams')[]"),
            r#"["solo" | "teams"]"#
        );
        assert_eq!(
            lolo_spelling("string | number"),
            "string | number",
            "control: a non-literal union is untouched"
        );
        assert_eq!(
            lolo_spelling("'it''s'"),
            "'it''s'",
            "control: not a plain literal, left as written"
        );
        assert!(
            !crate::type_compat::set_value_type_compatible("number", &union),
            "control: a string union does not fit a number"
        );
    }

    #[test]
    fn a_contract_list_type_reads_in_the_lolo_spelling() {
        let shape: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"steps":"AgentActivity[]","grid":"number[][]","name":"string","tags":{"type":"string[]"}}"#,
        )
        .unwrap();
        let types: Vec<(String, String)> = response_shape_fields(&shape)
            .into_iter()
            .map(|f| (f.name, f.field_type))
            .collect();
        assert_eq!(
            types,
            [
                ("grid".to_string(), "[[number]]".to_string()),
                ("name".to_string(), "string".to_string()),
                ("steps".to_string(), "[AgentActivity]".to_string()),
                ("tags".to_string(), "[string]".to_string()),
            ]
        );
        assert!(crate::type_compat::set_value_type_compatible(
            "[AgentActivity]",
            &types[2].1
        ));
        assert!(!crate::type_compat::set_value_type_compatible(
            "[string]",
            &types[2].1
        ));
    }

    fn shaped_registry() -> ServiceRegistryData {
        parse_service_registry_data(
            r#"{"services": {"files": {"actions": [
                {"name": "upload", "params": [],
                 "responseShape": {"url": "string", "meta": {"type": "object"}, "loose": 3}},
                {"name": "ping", "params": [], "responseShape": {}},
                {"name": "bare", "params": []}]}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_registry_action_exposes_its_response_shape_sorted_and_typed() {
        let registry = shaped_registry();
        let shape = registry.response_shape("files", "upload").expect("shape");
        let found: Vec<(&str, &str)> = shape
            .iter()
            .map(|f| (f.name.as_str(), f.field_type.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![("loose", "any"), ("meta", "object"), ("url", "string")]
        );
    }

    #[test]
    fn an_action_without_a_usable_response_shape_has_none() {
        let registry = shaped_registry();
        assert!(
            registry.response_shape("files", "ping").is_none(),
            "empty shape"
        );
        assert!(
            registry.response_shape("files", "bare").is_none(),
            "no shape"
        );
        assert!(
            registry.response_shape("files", "missing").is_none(),
            "unknown action"
        );
        assert!(
            registry.response_shape("nope", "upload").is_none(),
            "unknown service"
        );
    }

    // Mobile shells (android/ios) are NO-OP-DISPATCH: they interpret
    // server-returned clientEffects at runtime and never compile S-expr
    // operators, so no canonical category may target them. If this ever
    // changes, the exemption must be revisited (that shell would then need its
    // own operator-completeness gate, like Python's).
    #[test]
    fn no_canonical_category_targets_a_mobile_shell() {
        let doc: serde_json::Value = serde_json::from_str(operators_json()).unwrap();
        let forbidden = ["android", "ios", "kotlin", "swift", "mobile"];
        let mut offenders = Vec::new();
        for (cat, meta) in doc["categories"].as_object().unwrap() {
            if let Some(targets) = meta["target"].as_array() {
                for t in targets.iter().filter_map(|t| t.as_str()) {
                    if forbidden.contains(&t) {
                        offenders.push(format!("{cat} -> {t}"));
                    }
                }
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "Mobile shells are NO-OP-DISPATCH and must not be an operator target, \
             but these categories target one:\n  {}",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn operators_parse_and_lookup() {
        // Smoke test: a few well-known operators must exist.
        assert!(is_known_operator("+"));
        assert!(is_known_operator("if"));
        assert!(is_known_operator("set"));
        assert!(is_known_operator("render-ui"));
        assert!(is_known_operator("behavior/compose"));
        assert!(!is_known_operator("definitely-not-a-real-op-xyz"));
    }

    #[test]
    fn categories_parse() {
        let cats = categories();
        assert!(cats.contains_key("arithmetic"));
        assert!(cats.contains_key("std-composition"));
        let std_comp = cats.get("std-composition").unwrap();
        assert!(std_comp.target.iter().any(|t| t == "rust"));
    }

    #[test]
    fn patterns_parse_and_lookup() {
        assert!(is_known_pattern("table-view"));
        assert!(is_known_pattern("button"));
        assert!(!is_known_pattern("definitely-not-a-pattern-xyz"));
        let table = get_pattern("table-view").unwrap();
        assert_eq!(table.category, "display");
        assert!(table.props_schema.contains_key("entity"));
    }

    #[test]
    fn program_host_effects_are_the_installed_tables_host_program_operators() {
        let from_table: std::collections::BTreeSet<&str> = operators()
            .iter()
            .filter(|(_, op)| op.host.as_deref() == Some("program"))
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(from_table.contains("behavior/resolve"), "{from_table:?}");
        assert!(from_table.contains("program/validate"), "{from_table:?}");
        assert_eq!(
            program_host_effects().collect::<std::collections::BTreeSet<_>>(),
            from_table
        );
        for op in &from_table {
            assert!(is_program_host_effect(op), "{op}");
        }
        for op in [
            "persist",
            "call-service",
            "behavior/apply",
            "llm/generate",
            "no/such",
        ] {
            assert!(!is_program_host_effect(op), "control: {op}");
        }
    }

    #[test]
    fn os_host_effects_are_the_installed_tables_host_os_operators() {
        let from_os: std::collections::BTreeSet<&str> = os_host_effects().collect();
        for op in ["os/read-file", "os/spawn", "os/wait", "os/kill"] {
            assert!(from_os.contains(op), "{op} in {from_os:?}");
        }
        for op in ["os/path-join", "persist", "no/such"] {
            assert!(!from_os.contains(op), "control: {op}");
        }
    }

    #[test]
    fn sigil_context_rules_come_from_the_installed_core() {
        let guard = allowed_sigils_for_context("guard").expect("guard rules");
        assert!(guard.iter().any(|r| r == "payload"));
        let tick = allowed_sigils_for_context("tick").expect("tick rules");
        assert!(
            !tick.iter().any(|r| r == "payload"),
            "control: ticks fire on a clock and have no payload"
        );
        assert_eq!(allowed_sigils_for_context("nope"), None);
        assert_eq!(
            known_contexts(),
            ["guard", "effect", "tick"].into_iter().collect()
        );
    }
}
