//! The TypeScript mirror of the embed wire, generated into `@almadar/core`
//! (`packages/almadar-core/src/generated/kernel-wire.ts`). `ts-rs` derives the declarations of the
//! wire types and of the `effects::response` types they contain; the types whose serde form is
//! hand-written or lives in `runtime/` are declared here, by hand, beside the serde they mirror.
//! `@almadar/core` stays the contract: its `kernel-wire` type test compares every generated type
//! with the core type it corresponds to. The payload, row and effect leaves are declared here as JSON types.
//!
//! `KERNEL_WIRE_TS_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_ts` rewrites the file; without it
//! the test fails when the committed file is stale.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::{
    AppliedWire, DispatchWire, FoldProgressWire, FoldedNextWire, FrameWire, IoReplyWire, MountWire, NextWire, ResolveProgressWire, ServerProgressWire,
    SnapshotWire,
};
use crate::effects::response::{EventRequest, EventResponse};
use crate::propositions::{PropositionObservation, PropositionVerdict};


use crate::embed::eval_wire::{EvalContextWire, EvalOutcomeWire, EvalTracedWire, GuardOutcomeWire, TraceEffectsWire, MutationOpWire, MutationVerdictWire, RowBindingWire};
use crate::canvas_theme::CanvasTheme;
use crate::motion::ResolvedMotion;
use crate::row_query::{QueryCapabilities, WriteCapabilities};
use orbital_core_base::row_set::RowSetRef;
use crate::kernel::{AwaitingTrait, TraitSnapshot};

use super::serde_index::{index, DefaultKind, FieldInfo, TypeInfo, VariantInfo};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::path::PathBuf;
use ts_rs::{Config, TypeVisitor, TS};

/// Declares a type whose TypeScript form is written here: `$name` is its exported name and
/// `$decl` its right-hand side.
macro_rules! declared_by_hand {
    ($ty:ty, $name:literal, $decl:literal $(, [$($dep:ty),*])?) => {
        impl TS for $ty {
            type WithoutGenerics = Self;
            type OptionInnerType = Self;
            fn name(_: &Config) -> String {
                $name.to_string()
            }
            fn inline(_: &Config) -> String {
                $name.to_string()
            }
            fn decl(_: &Config) -> String {
                format!("type {} = {}", $name, $decl)
            }
            fn decl_concrete(cfg: &Config) -> String {
                <Self as TS>::decl(cfg)
            }
            fn output_path() -> Option<PathBuf> {
                Some(PathBuf::from(concat!($name, ".ts")))
            }
            fn visit_dependencies(_v: &mut impl TypeVisitor)
            where
                Self: 'static,
            {
                $($(_v.visit::<$dep>();)*)?
            }
        }
    };
}
declared_by_hand!(
    TraitSnapshot,
    "TraitSnapshot",
    "{ state: string, entity: JsonValue }"
);
declared_by_hand!(
    AwaitingTrait,
    "AwaitingTrait",
    "{ trait: string, event: string, from: string }"
);
declared_by_hand!(
    crate::effects::frame_delta::FrameOp,
    "FrameOp",
    "[\"put\", string, EntityRow] | [\"set\", { [key in string]: EntityRow }] | [\"del\", string] | [\"clear\"]"
);
declared_by_hand!(CanvasTheme, "CanvasTheme", "JsonObject");
declared_by_hand!(ResolvedMotion, "ResolvedMotion", "JsonObject");

struct Collect<'a> {
    cfg: &'a Config,
    seen: Vec<TypeId>,
    decls: BTreeMap<String, String>,
    rust: BTreeMap<String, String>,
}

/// The Rust name of a type: the last path segment of its `type_name`, without generic arguments.
fn rust_name_of<T: ?Sized>() -> String {
    let full = std::any::type_name::<T>();
    let head = full.split('<').next().unwrap_or(full);
    head.rsplit("::").next().unwrap_or(head).to_string()
}

impl TypeVisitor for Collect<'_> {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        if T::output_path().is_some() {
            if self.seen.contains(&TypeId::of::<T>()) {
                return;
            }
            self.seen.push(TypeId::of::<T>());
            self.decls.insert(T::ident(self.cfg), T::decl(self.cfg));
            self.rust.insert(T::ident(self.cfg), rust_name_of::<T>());
        }
        T::visit_dependencies(self);
        T::visit_generics(self);
    }
}

/// The leaf types of every payload, row and effect on the wire: JSON. A date is its ISO text and an absent field is an
/// absent key, so a value of these types is a `JsonValue` and travels without a clone or a cast.
const WIRE_LEAVES: &str = "export type FieldValue = JsonValue;\n\nexport type EventPayloadValue = JsonValue;\n\nexport type EventPayload = JsonObject;\n\nexport type EntityRow = { id?: string } & JsonObject;\n\n";

fn collect_all(cfg: &Config) -> Collect<'_> {
    let mut collect = Collect {
        cfg,
        seen: Vec::new(),
        decls: BTreeMap::new(),
        rust: BTreeMap::new(),
    };
    collect.visit::<DispatchWire>();
    collect.visit::<AppliedWire>();
    collect.visit::<MountWire>();
    collect.visit::<FrameWire>();
    collect.visit::<SnapshotWire>();
    collect.visit::<ServerProgressWire>();
    collect.visit::<NextWire>();
    collect.visit::<FoldProgressWire>();
    collect.visit::<FoldedNextWire>();
    collect.visit::<ResolveProgressWire>();
    collect.visit::<IoReplyWire>();
    collect.visit::<EventRequest>();
    collect.visit::<EventResponse>();
    collect.visit::<QueryCapabilities>();
    collect.visit::<WriteCapabilities>();
    collect.visit::<RowSetRef>();
    collect.visit::<PropositionObservation>();
    collect.visit::<PropositionVerdict>();
    collect.visit::<EvalContextWire>();
    collect.visit::<EvalOutcomeWire>();
    collect.visit::<GuardOutcomeWire>();
    collect.visit::<EvalTracedWire>();
    collect.visit::<TraceEffectsWire>();
    collect.visit::<RowBindingWire>();
    collect.visit::<MutationOpWire>();
    collect.visit::<MutationVerdictWire>();
    collect.visit::<orbital_core_base::viewer::Viewer>();
    collect.visit::<orbital_core_base::os_grants::OsGrants>();
    collect.visit::<crate::program_machine::MachineRow>();
    collect.visit::<crate::program_machine::PlayedStep>();
    collect
}

fn kernel_wire_ts() -> String {
    let cfg = Config::new().with_large_int("number");
    let collect = collect_all(&cfg);

    let mut out = String::from(
        "// GENERATED by `cargo test -p orbital-core-runtime --lib kernel_wire_ts` (KERNEL_WIRE_TS_WRITE=1) from\n\
         // orbital-core `embed::wire`. Do not edit: change the Rust wire types and regenerate.\n\n\
         import {\n  asEventId,\n  asOrbitalId,\n  asTraitId,\n  type EventId,\n  type JsonObject,\n  type JsonValue,\n  type NamespacedEffectName,\n  type OrbitalId,\n  type RawUserClaims,\n  type ServerBatchSummary,\n  type TraitId,\n} from '../types/index.js';\n\n",
    );
    out.push_str(WIRE_LEAVES);
    for decl in collect.decls.values() {
        out.push_str("export ");
        out.push_str(decl);
        out.push_str("\n\n");
    }
    out.push_str(&wire_checks(&collect.decls, &collect.rust, GUARDED));
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// One field of an object the decoder rebuilds: whether the key may be absent, and the TypeScript value serde fills in
/// when it is.
#[derive(Debug, Clone)]
struct RecField {
    key: String,
    optional: bool,
    shape: Shape,
    default: Option<String>,
}

/// A TypeScript type as `ts-rs` writes it, reduced to what a structural check needs.
#[derive(Debug, Clone)]
enum Shape {
    Any,
    Text,
    Number,
    Bool,
    Null,
    Literal(String),
    Array(Box<Shape>),
    Tuple(Vec<Shape>),
    Object(Vec<(String, bool, Shape)>),
    Int(String),
    Record(Vec<RecField>, bool),
    Map(Box<Shape>),
    Union(Vec<Shape>),
    Both(Vec<Shape>),
    Named(String),
    Applied(String, Vec<Shape>),
}

struct ShapeParser {
    tokens: Vec<String>,
    at: usize,
}

impl ShapeParser {
    fn new(source: &str) -> Self {
        let mut tokens = Vec::new();
        let chars: Vec<char> = source.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() {
                i += 1;
            } else if c == '/' && chars.get(i + 1) == Some(&'*') {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            } else if c == '"' {
                let start = i;
                i += 1;
                while chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                tokens.push(chars[start..i].iter().collect());
            } else if c.is_alphanumeric() || c == '_' || c == '$' || c == '-' || c == '.' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$' | '-' | '.')) {
                    i += 1;
                }
                tokens.push(chars[start..i].iter().collect());
            } else {
                tokens.push(c.to_string());
                i += 1;
            }
        }
        Self { tokens, at: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn next(&mut self) -> String {
        let token = self.tokens.get(self.at).cloned().unwrap_or_else(|| panic!("kernel_wire_ts: the type ends early"));
        self.at += 1;
        token
    }

    fn expect(&mut self, want: &str) {
        let got = self.next();
        assert_eq!(got, want, "kernel_wire_ts: expected `{want}` in {:?}", self.tokens);
    }

    fn union(&mut self) -> Shape {
        if self.peek() == Some("|") {
            self.next();
        }
        let mut items = vec![self.both()];
        while self.peek() == Some("|") {
            self.next();
            items.push(self.both());
        }
        if items.len() == 1 { items.remove(0) } else { Shape::Union(items) }
    }

    fn both(&mut self) -> Shape {
        let mut items = vec![self.primary()];
        while self.peek() == Some("&") {
            self.next();
            items.push(self.primary());
        }
        if items.len() == 1 { items.remove(0) } else { Shape::Both(items) }
    }

    fn primary(&mut self) -> Shape {
        let token = self.next();
        let mut shape = match token.as_str() {
            "{" => self.object(),
            "[" => {
                let mut items = Vec::new();
                while self.peek() != Some("]") {
                    items.push(self.union());
                    if self.peek() == Some(",") {
                        self.next();
                    }
                }
                self.expect("]");
                Shape::Tuple(items)
            }
            "(" => {
                let inner = self.union();
                self.expect(")");
                inner
            }
            "string" => Shape::Text,
            "number" | "bigint" => Shape::Number,
            "boolean" => Shape::Bool,
            "null" => Shape::Null,
            "true" | "false" => Shape::Literal(token),
            "unknown" | "any" => Shape::Any,
            "Array" => {
                self.expect("<");
                let item = self.union();
                self.expect(">");
                Shape::Array(Box::new(item))
            }
            "Record" => {
                self.expect("<");
                let _key = self.union();
                self.expect(",");
                let value = self.union();
                self.expect(">");
                Shape::Map(Box::new(value))
            }
            _ if token.starts_with('"') => Shape::Literal(token),
            _ if token.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') => Shape::Literal(token),
            _ if self.peek() == Some("<") => {
                self.next();
                let mut args = vec![self.union()];
                while self.peek() == Some(",") {
                    self.next();
                    args.push(self.union());
                }
                self.expect(">");
                Shape::Applied(token, args)
            }
            _ => Shape::Named(token),
        };
        while self.peek() == Some("[") && self.tokens.get(self.at + 1).map(String::as_str) == Some("]") {
            self.at += 2;
            shape = Shape::Array(Box::new(shape));
        }
        shape
    }

    fn object(&mut self) -> Shape {
        if self.peek() == Some("[") {
            self.next();
            self.next();
            self.expect("in");
            let _key = self.union();
            self.expect("]");
            if self.peek() == Some("?") {
                self.next();
            }
            self.expect(":");
            let value = self.union();
            if self.peek() == Some(",") {
                self.next();
            }
            self.expect("}");
            return Shape::Map(Box::new(value));
        }
        let mut fields = Vec::new();
        while self.peek() != Some("}") {
            let key = self.next().trim_matches('"').to_string();
            let optional = self.peek() == Some("?");
            if optional {
                self.next();
            }
            self.expect(":");
            fields.push((key, optional, self.union()));
            if self.peek() == Some(",") {
                self.next();
            }
        }
        self.expect("}");
        Shape::Object(fields)
    }
}

fn shape_of(decl: &str) -> Shape {
    let (_, rhs) = decl.split_once('=').expect("a type declaration has `=`");
    let mut parser = ShapeParser::new(rhs.trim().trim_end_matches(';'));
    let shape = parser.union();
    assert!(parser.peek().is_none(), "kernel_wire_ts: trailing tokens in {decl}");
    shape
}

/// The type parameters of a declaration (`type Settled<T, E> = ...` -> `T`, `E`).
fn parameters_of(decl: &str) -> Vec<String> {
    let (head, _) = decl.split_once('=').expect("a type declaration has `=`");
    match head.split_once('<') {
        Some((_, params)) => params.trim().trim_end_matches('>').split(',').map(|p| p.trim().to_string()).collect(),
        None => Vec::new(),
    }
}

fn substituted(shape: &Shape, bindings: &[(String, Shape)]) -> Shape {
    let all = |items: &[Shape]| items.iter().map(|item| substituted(item, bindings)).collect::<Vec<_>>();
    match shape {
        Shape::Named(name) => match bindings.iter().find(|(param, _)| param == name) {
            Some((_, bound)) => bound.clone(),
            None => shape.clone(),
        },
        Shape::Array(item) => Shape::Array(Box::new(substituted(item, bindings))),
        Shape::Map(item) => Shape::Map(Box::new(substituted(item, bindings))),
        Shape::Tuple(items) => Shape::Tuple(all(items)),
        Shape::Union(items) => Shape::Union(all(items)),
        Shape::Both(items) => Shape::Both(all(items)),
        Shape::Applied(name, args) => Shape::Applied(name.clone(), all(args)),
        Shape::Object(fields) => Shape::Object(
            fields.iter().map(|(key, optional, field)| (key.clone(), *optional, substituted(field, bindings))).collect(),
        ),
        Shape::Record(fields, deny) => Shape::Record(
            fields
                .iter()
                .map(|f| RecField { shape: substituted(&f.shape, bindings), ..f.clone() })
                .collect(),
            *deny,
        ),
        Shape::Any | Shape::Text | Shape::Number | Shape::Int(_) | Shape::Bool | Shape::Null | Shape::Literal(_) => {
            shape.clone()
        }
    }
}

/// The TypeScript expression that holds when `v` has `shape`; a name outside the declared wire types is an opaque JSON blob.
fn check_expr(shape: &Shape, v: &str, declared: &BTreeMap<String, String>, used: &mut Vec<String>) -> String {
    match shape {
        Shape::Any => format!("{v} !== undefined"),
        Shape::Text => format!("typeof {v} === 'string'"),
        Shape::Number | Shape::Int(_) => format!("typeof {v} === 'number'"),
        Shape::Bool => format!("typeof {v} === 'boolean'"),
        Shape::Null => format!("{v} === null"),
        Shape::Literal(text) => format!("{v} === {text}"),
        Shape::Array(item) => format!(
            "(Array.isArray({v}) && {v}.every((e) => {}))",
            check_expr(item, "e", declared, used)
        ),
        Shape::Tuple(items) => {
            let checks: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, item)| check_expr(item, &format!("{v}[{i}]"), declared, used))
                .collect();
            format!("(Array.isArray({v}) && {v}.length === {} && {})", items.len(), checks.join(" && "))
        }
        Shape::Object(fields) => {
            let mut checks = vec![format!("isRecord({v})")];
            for (key, optional, field) in fields {
                let access = format!("{v}[{key:?}]");
                let inner = check_expr(field, &access, declared, used);
                checks.push(if *optional { format!("({access} === undefined || {inner})") } else { inner });
            }
            format!("({})", checks.join(" && "))
        }
        Shape::Record(fields, _) => {
            let plain: Vec<(String, bool, Shape)> =
                fields.iter().map(|f| (f.key.clone(), f.optional || f.default.is_some(), f.shape.clone())).collect();
            check_expr(&Shape::Object(plain), v, declared, used)
        }
        Shape::Map(value) => format!(
            "(isRecord({v}) && Object.values({v}).every((e) => {}))",
            check_expr(value, "e", declared, used)
        ),
        Shape::Union(items) => {
            format!("({})", items.iter().map(|i| check_expr(i, v, declared, used)).collect::<Vec<_>>().join(" || "))
        }
        Shape::Both(items) => {
            format!("({})", items.iter().map(|i| check_expr(i, v, declared, used)).collect::<Vec<_>>().join(" && "))
        }
        Shape::Named(name) if declared.contains_key(name) => {
            if !used.contains(name) {
                used.push(name.clone());
            }
            format!("check{name}({v})")
        }
        Shape::Named(name) => format!("accepts((x, q) => {}, {v})", leaf_builder(name, "x", "q")),
        Shape::Applied(name, args) => {
            let decl = declared
                .get(name)
                .unwrap_or_else(|| panic!("kernel_wire_ts: `{name}<..>` is not a declared wire type"));
            let params = parameters_of(decl);
            assert_eq!(params.len(), args.len(), "kernel_wire_ts: `{name}` takes {} type arguments", params.len());
            let bindings: Vec<(String, Shape)> = params.into_iter().zip(args.iter().cloned()).collect();
            check_expr(&substituted(&shape_of(decl), &bindings), v, declared, used)
        }
    }
}

/// The TypeScript expression that decodes `v` (at path `p`) as the named leaf type: the one place a leaf's
/// acceptance is stated, so a union check of the leaf runs this same builder.
fn leaf_builder(name: &str, v: &str, p: &str) -> String {
    match name {
        "JsonValue" | "FieldValue" | "EventPayloadValue" => format!("any({v}, {p})"),
        "JsonObject" | "EventPayload" => format!("rec({v}, {p})"),
        "EntityRow" => format!("entityRow({v}, {p})"),
        "EventId" => format!("asEventId(str({v}, {p}))"),
        "OrbitalId" => format!("asOrbitalId(str({v}, {p}))"),
        "TraitId" => format!("asTraitId(str({v}, {p}))"),
        "NamespacedEffectName" => format!("namespacedEffectName({v}, {p})"),
        "ServerBatchSummary" => format!("serverBatchSummary({v}, {p})"),
        "RawUserClaims" => format!("rawUserClaims({v}, {p})"),
        other => panic!("kernel_wire_ts: the decoder has no builder for the leaf type `{other}`"),
    }
}

/// What a failed check expected, in words.
fn describe(shape: &Shape) -> String {
    match shape {
        Shape::Any => "a value".to_string(),
        Shape::Text => "a string".to_string(),
        Shape::Number => "a number".to_string(),
        Shape::Int(rust) => format!("a {rust} integer"),
        Shape::Bool => "a boolean".to_string(),
        Shape::Null => "null".to_string(),
        Shape::Literal(text) => text.clone(),
        Shape::Array(_) => "a list".to_string(),
        Shape::Tuple(items) => format!("a list of {}", items.len()),
        Shape::Object(_) | Shape::Record(..) | Shape::Map(_) => "an object".to_string(),
        Shape::Union(items) => items.iter().map(describe).collect::<Vec<_>>().join(" | "),
        Shape::Both(items) => items.iter().map(describe).collect::<Vec<_>>().join(" & "),
        Shape::Named(name) | Shape::Applied(name, _) => name.clone(),
    }
}

/// The fields of an object shape, plain or strict.
fn record_of(shape: &Shape) -> Option<(Vec<RecField>, bool)> {
    match shape {
        Shape::Object(fields) => Some((
            fields
                .iter()
                .map(|(key, optional, shape)| RecField {
                    key: key.clone(),
                    optional: *optional,
                    shape: shape.clone(),
                    default: None,
                })
                .collect(),
            false,
        )),
        Shape::Record(fields, deny) => Some((fields.clone(), *deny)),
        _ => None,
    }
}

/// The key every variant of a union of objects fixes to one literal, when there is one.
fn discriminator(items: &[Shape]) -> Option<String> {
    let first = record_of(items.first()?)?.0;
    first
        .iter()
        .filter(|f| matches!(f.shape, Shape::Literal(_)))
        .map(|f| &f.key)
        .find(|key| {
            items.iter().all(|item| {
                record_of(item).is_some_and(|(fields, _)| {
                    fields.iter().any(|f| f.key == **key && !f.optional && matches!(f.shape, Shape::Literal(_)))
                })
            })
        })
        .cloned()
}

fn int_range(rust: &str) -> (&'static str, &'static str) {
    match rust {
        "u8" => ("0", "255"),
        "u16" => ("0", "65535"),
        "u32" => ("0", "4294967295"),
        "i8" => ("-128", "127"),
        "i16" => ("-32768", "32767"),
        "i32" => ("-2147483648", "2147483647"),
        "u64" | "usize" | "u128" => ("0", "Number.MAX_SAFE_INTEGER"),
        "NonZeroUsize" => ("1", "Number.MAX_SAFE_INTEGER"),
        "i64" | "isize" | "i128" => ("-Number.MAX_SAFE_INTEGER", "Number.MAX_SAFE_INTEGER"),
        other => panic!("kernel_wire_ts: `{other}` is not an integer type"),
    }
}

fn is_int_type(name: &str) -> bool {
    matches!(name, "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "NonZeroUsize")
}

fn type_args(segment: &syn::PathSegment) -> Vec<&syn::Type> {
    match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|a| if let syn::GenericArgument::Type(t) = a { Some(t) } else { None })
            .collect(),
        _ => Vec::new(),
    }
}

/// `shape` with the integer types of the Rust type `ty` it was printed from.
fn annotate(shape: &Shape, ty: &syn::Type) -> Shape {
    match ty {
        syn::Type::Path(path) => {
            let Some(segment) = path.path.segments.last() else { return shape.clone() };
            let name = segment.ident.to_string();
            let args = type_args(segment);
            match (name.as_str(), shape) {
                (n, Shape::Number) if is_int_type(n) => Shape::Int(name),
                ("Option", Shape::Union(items)) => Shape::Union(
                    items.iter().map(|i| if matches!(i, Shape::Null) { i.clone() } else { annotate(i, args[0]) }).collect(),
                ),
                ("Option" | "Box", _) => annotate(shape, args[0]),
                ("Vec" | "VecDeque" | "HashSet" | "BTreeSet", Shape::Array(item)) => {
                    Shape::Array(Box::new(annotate(item, args[0])))
                }
                ("HashMap" | "BTreeMap" | "IndexMap", Shape::Map(value)) => {
                    Shape::Map(Box::new(annotate(value, args[args.len() - 1])))
                }
                _ => shape.clone(),
            }
        }
        syn::Type::Tuple(tuple) => match shape {
            Shape::Tuple(items) if items.len() == tuple.elems.len() => {
                Shape::Tuple(items.iter().zip(tuple.elems.iter()).map(|(i, t)| annotate(i, t)).collect())
            }
            _ => shape.clone(),
        },
        _ => shape.clone(),
    }
}

fn is_option(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

/// The decoder's view of the wire types: each declaration read together with the serde derive of the Rust type it was
/// printed from. A type with no readable derive is decoded by its printed shape alone and listed in `fallbacks`.
struct Strict<'a> {
    declared: &'a BTreeMap<String, String>,
    rust: &'a BTreeMap<String, String>,
    index: &'a BTreeMap<String, Vec<TypeInfo>>,
    fallbacks: std::cell::RefCell<BTreeMap<String, String>>,
}

impl Strict<'_> {
    fn fall_back(&self, name: &str, why: &str) {
        self.fallbacks.borrow_mut().insert(name.to_string(), why.to_string());
    }

    fn shape(&self, name: &str) -> Shape {
        let plain = shape_of(&self.declared[name]);
        let Some(rust_name) = self.rust.get(name) else {
            self.fall_back(name, "no Rust type recorded");
            return plain;
        };
        match self.index.get(rust_name).map(Vec::as_slice) {
            Some([one]) => self.apply(name, plain, one),
            Some(many) => {
                self.fall_back(name, &format!("{} definitions named `{rust_name}`", many.len()));
                plain
            }
            None => {
                self.fall_back(name, "no serde derive in the orbital-core sources (manual serde or foreign type)");
                plain
            }
        }
    }

    fn apply(&self, name: &str, plain: Shape, info: &TypeInfo) -> Shape {
        match (info, &plain) {
            (TypeInfo::Custom(why), _) => {
                self.fall_back(name, why);
                plain
            }
            (TypeInfo::Struct { fields, deny, .. }, Shape::Object(ts)) => {
                Shape::Record(ts.iter().map(|(k, o, s)| self.field(name, fields, k, *o, s)).collect(), *deny)
            }
            (TypeInfo::Enum { tag: Some(tag), variants, deny }, Shape::Union(items)) => {
                Shape::Union(items.iter().map(|i| self.variant(name, tag, variants, *deny, i)).collect())
            }
            (TypeInfo::Enum { tag: Some(tag), variants, deny }, Shape::Object(_)) => {
                self.variant(name, tag, variants, *deny, &plain)
            }
            (TypeInfo::Enum { tag: None, .. }, Shape::Union(items)) if items.iter().all(|i| matches!(i, Shape::Literal(_))) => {
                plain
            }
            _ => {
                self.fall_back(name, "its serde form and its printed shape are not a struct, a tagged enum or a string enum");
                plain
            }
        }
    }

    fn variant(&self, name: &str, tag: &str, variants: &[VariantInfo], deny: bool, item: &Shape) -> Shape {
        if let Shape::Both(parts) = item {
            let [head, Shape::Named(inner)] = parts.as_slice() else {
                panic!("kernel_wire_ts: a variant of `{name}` is an intersection the generator does not read")
            };
            let Shape::Record(mut fields, inner_deny) = self.variant(name, tag, variants, deny, head) else {
                panic!("kernel_wire_ts: a variant of `{name}` is not an object")
            };
            let Shape::Record(more, more_deny) = self.shape(inner) else {
                panic!("kernel_wire_ts: the payload `{inner}` of a variant of `{name}` is not a serde struct")
            };
            fields.extend(more);
            return Shape::Record(fields, inner_deny || more_deny);
        }
        let Shape::Object(ts) = item else { panic!("kernel_wire_ts: a variant of `{name}` is not an object") };
        let literal = ts
            .iter()
            .find_map(|(k, _, s)| match s {
                Shape::Literal(text) if k == tag => Some(text.trim_matches('"').to_string()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("kernel_wire_ts: a variant of `{name}` has no `{tag}` literal"));
        let info = variants
            .iter()
            .find(|v| v.tag == literal)
            .unwrap_or_else(|| panic!("kernel_wire_ts: `{name}` has no serde variant `{literal}`"));
        Shape::Record(
            ts.iter()
                .map(|(k, o, s)| {
                    if k == tag {
                        RecField { key: k.clone(), optional: false, shape: s.clone(), default: None }
                    } else {
                        self.field(name, &info.fields, k, *o, s)
                    }
                })
                .collect(),
            deny,
        )
    }

    fn field(&self, owner: &str, fields: &[FieldInfo], key: &str, optional: bool, shape: &Shape) -> RecField {
        let info = fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("kernel_wire_ts: `{owner}.{key}` is printed but has no serde field"));
        let at = format!("{owner}.{key}");
        let default = if optional {
            None
        } else if info.skippable && !is_option(&info.ty) {
            Some(self.default_of(&info.ty, &at))
        } else {
            match &info.default {
                DefaultKind::Path(path) => panic!("kernel_wire_ts: `{at}` has `default = \"{path}\"`, which the generator cannot evaluate"),
                DefaultKind::Trait => Some(self.default_of(&info.ty, &at)),
                DefaultKind::No if is_option(&info.ty) => Some("null".to_string()),
                DefaultKind::No => None,
            }
        };
        RecField { key: key.to_string(), optional, shape: annotate(shape, &info.ty), default }
    }

    /// The TypeScript value of `Default::default()` for a Rust type serde defaults a missing field to.
    fn default_of(&self, ty: &syn::Type, at: &str) -> String {
        if let syn::Type::Path(path) = ty {
            if let Some(segment) = path.path.segments.last() {
                let name = segment.ident.to_string();
                return match name.as_str() {
                    "String" => "''".to_string(),
                    "bool" => "false".to_string(),
                    "f32" | "f64" => "0".to_string(),
                    n if is_int_type(n) => "0".to_string(),
                    "Option" => "null".to_string(),
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => "[]".to_string(),
                    "HashMap" | "BTreeMap" | "IndexMap" | "Map" => "{}".to_string(),
                    other => match self.index.get(other).map(Vec::as_slice) {
                        Some([TypeInfo::Struct { fields, derives_default: true, .. }]) => {
                            let parts: Vec<String> = fields
                                .iter()
                                .filter(|f| !is_option(&f.ty))
                                .map(|f| format!("{:?}: {}", f.key, self.default_of(&f.ty, &format!("{at}.{}", f.key))))
                                .collect();
                            format!("{{ {} }}", parts.join(", "))
                        }
                        _ => panic!("kernel_wire_ts: `{at}` defaults a `{other}` and the generator has no TypeScript value for it"),
                    },
                };
            }
        }
        panic!("kernel_wire_ts: `{at}` defaults a type the generator cannot read")
    }

    fn leaf(&self, name: &str, v: &str, p: &str) -> String {
        leaf_builder(name, v, p)
    }

    /// The TypeScript expression that decodes `v` (at path `p`) as `shape`: it throws naming the failing path, and
    /// yields the value serde would produce.
    fn build(&self, shape: &Shape, v: &str, p: &str, depth: usize, used: &mut Vec<String>, check_used: &mut Vec<String>) -> String {
        match shape {
            Shape::Any => format!("any({v}, {p})"),
            Shape::Text => format!("str({v}, {p})"),
            Shape::Number => format!("num({v}, {p})"),
            Shape::Int(rust) => {
                let (lo, hi) = int_range(rust);
                format!("int({v}, {p}, {lo}, {hi}, {rust:?})")
            }
            Shape::Bool => format!("bool({v}, {p})"),
            Shape::Null => format!("nul({v}, {p})"),
            Shape::Literal(text) => format!("lit({v}, {p}, {text})"),
            Shape::Array(item) => {
                let (e, q) = (format!("e{depth}"), format!("q{depth}"));
                format!("list({v}, {p}, ({e}, {q}) => {})", self.build(item, &e, &q, depth + 1, used, check_used))
            }
            Shape::Map(item) => {
                let (e, q) = (format!("e{depth}"), format!("q{depth}"));
                format!("dict({v}, {p}, ({e}, {q}) => {})", self.build(item, &e, &q, depth + 1, used, check_used))
            }
            Shape::Tuple(items) => {
                let t = format!("t{depth}");
                let parts: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(at, item)| self.build(item, &format!("{t}[{at}]"), &format!("{p} + '[{at}]'"), depth + 1, used, check_used))
                    .collect();
                format!("tuple({v}, {p}, {}, ({t}) => [{}])", items.len(), parts.join(", "))
            }
            Shape::Object(_) | Shape::Record(..) => {
                let (fields, deny) = record_of(shape).expect("an object shape");
                let r = format!("r{depth}");
                let mut parts = Vec::new();
                for field in &fields {
                    let key = &field.key;
                    let access = format!("{r}[{key:?}]");
                    let inner = self.build(&field.shape, &access, &format!("{p} + '.{key}'"), depth + 1, used, check_used);
                    parts.push(if field.optional {
                        format!("...({access} === undefined || {access} === null ? {{}} : {{ {key:?}: {inner} }})")
                    } else if let Some(default) = &field.default {
                        format!("{key:?}: {access} === undefined ? {default} : {inner}")
                    } else {
                        format!("{key:?}: {inner}")
                    });
                }
                let known = if deny {
                    format!("[{}]", fields.iter().map(|f| format!("{:?}", f.key)).collect::<Vec<_>>().join(", "))
                } else {
                    "null".to_string()
                };
                format!("obj({v}, {p}, {known}, ({r}) => ({{ {} }}))", parts.join(", "))
            }
            Shape::Union(items) => self.build_union(shape, items, v, p, depth, used, check_used),
            Shape::Both(items) => {
                let parts: Vec<String> =
                    items.iter().map(|i| format!("...{}", self.build(i, v, p, depth, used, check_used))).collect();
                format!("({{ {} }})", parts.join(", "))
            }
            Shape::Named(name) if self.declared.contains_key(name) => {
                if !used.contains(name) {
                    used.push(name.clone());
                }
                format!("build{name}({v}, {p})")
            }
            Shape::Named(name) => self.leaf(name, v, p),
            Shape::Applied(name, args) => {
                let params = parameters_of(&self.declared[name]);
                let bindings: Vec<(String, Shape)> = params.into_iter().zip(args.iter().cloned()).collect();
                self.build(&substituted(&self.shape(name), &bindings), v, p, depth, used, check_used)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_union(&self, shape: &Shape, items: &[Shape], v: &str, p: &str, depth: usize, used: &mut Vec<String>, check_used: &mut Vec<String>) -> String {
        if items.iter().all(|i| matches!(i, Shape::Literal(_))) {
            let lits: Vec<&str> = items.iter().map(|i| if let Shape::Literal(t) = i { t.as_str() } else { "" }).collect();
            return format!("oneOf({v}, {p}, [{}])", lits.join(", "));
        }
        if items.iter().any(|i| matches!(i, Shape::Null)) {
            let rest: Vec<Shape> = items.iter().filter(|i| !matches!(i, Shape::Null)).cloned().collect();
            let inner = if rest.len() == 1 { rest[0].clone() } else { Shape::Union(rest) };
            return format!("({v} === null ? null : {})", self.build(&inner, v, p, depth, used, check_used));
        }
        if let Some(key) = discriminator(items) {
            let mut chain = String::new();
            for item in items {
                let (fields, _) = record_of(item).expect("a tagged variant is an object");
                let Some(Shape::Literal(tag)) = fields.iter().find(|f| f.key == key).map(|f| &f.shape) else { continue };
                chain.push_str(&format!(
                    "rec({v}, {p})[{key:?}] === {tag} ? {} : ",
                    self.build(item, v, p, depth, used, check_used)
                ));
            }
            return format!("({chain}fail(unknownTag({p}, {key:?}, rec({v}, {p})[{key:?}])))");
        }
        let mut chain = String::new();
        for item in items {
            let holds = check_expr(item, v, self.declared, check_used);
            chain.push_str(&format!("{holds} ? {} : ", self.build(item, v, p, depth, used, check_used)));
        }
        format!("({chain}fail(miss({p}, {:?})))", describe(shape))
    }
}

const BUILD_HELPERS: &str = r#"const miss = (path: string, want: string): string => `${path}: expected ${want}`;

const unknownTag = (path: string, key: string, value: JsonValue | undefined): string => `${path}: unknown ${key} ${JSON.stringify(value)}`;

class KernelWireError extends Error {}

function fail(message: string): never {
  throw new KernelWireError(`kernel wire: ${message}`);
}

function accepts<T>(build: (v: JsonValue | undefined, p: string) => T, v: JsonValue | undefined): boolean {
  try {
    build(v, '');
    return true;
  } catch (error) {
    if (error instanceof KernelWireError) return false;
    throw error;
  }
}

function any(v: JsonValue | undefined, p: string): JsonValue {
  return v === undefined ? fail(miss(p, 'a value')) : v;
}

function str(v: JsonValue | undefined, p: string): string {
  return typeof v === 'string' ? v : fail(miss(p, 'a string'));
}

function num(v: JsonValue | undefined, p: string): number {
  return typeof v === 'number' ? v : fail(miss(p, 'a number'));
}

function int(v: JsonValue | undefined, p: string, min: number, max: number, rust: string): number {
  return typeof v === 'number' && Number.isInteger(v) && v >= min && v <= max ? v : fail(miss(p, `${rust} (an integer from ${min} to ${max})`));
}

function bool(v: JsonValue | undefined, p: string): boolean {
  return typeof v === 'boolean' ? v : fail(miss(p, 'a boolean'));
}

function nul(v: JsonValue | undefined, p: string): null {
  return v === null ? null : fail(miss(p, 'null'));
}

function lit<const L extends string | number | boolean>(v: JsonValue | undefined, p: string, want: L): L {
  return v === want ? want : fail(miss(p, JSON.stringify(want)));
}

function oneOf<const L extends string | number | boolean>(v: JsonValue | undefined, p: string, wants: readonly L[]): L {
  for (const want of wants) {
    if (v === want) return want;
  }
  return fail(miss(p, wants.map((want) => JSON.stringify(want)).join(' | ')));
}

function rec(v: JsonValue | undefined, p: string): JsonObject {
  return isRecord(v) ? v : fail(miss(p, 'an object'));
}

function isRow(record: JsonObject): record is EntityRow {
  return record['id'] === undefined || typeof record['id'] === 'string';
}

function entityRow(v: JsonValue | undefined, p: string): EntityRow {
  const row = rec(v, p);
  return isRow(row) ? row : fail(miss(`${p}.id`, 'a string'));
}

function isNamespaced(name: string): name is NamespacedEffectName {
  return name.includes('/');
}

function namespacedEffectName(v: JsonValue | undefined, p: string): NamespacedEffectName {
  const name = str(v, p);
  return isNamespaced(name) ? name : fail(miss(p, 'a namespaced effect name (head/tail)'));
}

function serverBatchSummary(v: JsonValue | undefined, p: string): ServerBatchSummary {
  const record = rec(v, p);
  return {
    operations: list(record['operations'], `${p}.operations`, entityRow),
    completedCount: num(record['completedCount'], `${p}.completedCount`),
    totalCount: num(record['totalCount'], `${p}.totalCount`),
  };
}

function rawUserClaims(v: JsonValue | undefined, p: string): RawUserClaims {
  const record = rec(v, p);
  const text = (key: string): string | undefined => (record[key] === undefined ? undefined : str(record[key], `${p}.${key}`));
  const nullable = (key: string): string | null | undefined =>
    record[key] === undefined ? undefined : record[key] === null ? null : str(record[key], `${p}.${key}`);
  const permissions = record['permissions'];
  return {
    ...record,
    ...(text('id') === undefined ? {} : { id: text('id') }),
    ...(text('uid') === undefined ? {} : { uid: text('uid') }),
    ...(nullable('email') === undefined ? {} : { email: nullable('email') }),
    ...(nullable('name') === undefined ? {} : { name: nullable('name') }),
    ...(nullable('displayName') === undefined ? {} : { displayName: nullable('displayName') }),
    ...(nullable('role') === undefined ? {} : { role: nullable('role') }),
    ...(permissions === undefined ? {} : { permissions: permissions === null ? null : list(permissions, `${p}.permissions`, str) }),
  };
}

function list<T>(v: JsonValue | undefined, p: string, at: (item: JsonValue | undefined, path: string) => T): T[] {
  if (!Array.isArray(v)) return fail(miss(p, 'a list'));
  return v.map((item, index) => at(item, `${p}[${index}]`));
}

function dict<T>(v: JsonValue | undefined, p: string, at: (item: JsonValue | undefined, path: string) => T): { [key in string]: T } {
  const out: { [key in string]: T } = {};
  for (const [key, item] of Object.entries(rec(v, p))) out[key] = at(item, `${p}.${key}`);
  return out;
}

function tuple<T>(v: JsonValue | undefined, p: string, length: number, make: (items: JsonValue[]) => T): T {
  if (!Array.isArray(v) || v.length !== length) return fail(miss(p, `a list of ${length}`));
  return make(v);
}

function obj<T>(v: JsonValue | undefined, p: string, known: readonly string[] | null, make: (record: JsonObject) => T): T {
  const record = rec(v, p);
  if (known !== null) {
    for (const key of Object.keys(record)) {
      if (!known.includes(key)) return fail(`${p}.${key}: unknown field`);
    }
  }
  return make(record);
}

"#;

/// The wire types a host receives or posts as JSON, each with an exported guard `is<Name>`.
const GUARDED: &[&str] = &[
    "DispatchWire",
    "AppliedWire",
    "MountWire",
    "FrameWire",
    "SnapshotWire",
    "ServerProgressWire",
    "NextWire",
    "FoldProgressWire",
    "FoldedNextWire",
    "ResolveProgressWire",
    "IoReplyWire",
    "EventRequest",
    "EventResponse",
    "IoRequestWire",
    "HttpBody",
    "HttpRequestWire",
    "HttpResponseWire",
];


/// The checks a host makes of the kernel wire at the boundary, generated from the declarations above: a guard for each
/// of `roots` and a check for every wire type reachable from it, recursively (fields, list elements, options, tuples,
/// string enums, tagged unions), and a throwing decoder for each root that accepts and produces what the serde derive of
/// the Rust type does (`Strict`). A name outside the wire types (`EntityRow`, `EventId`, ...) is an opaque JSON value
/// and only has to be present.
fn wire_checks(declared: &BTreeMap<String, String>, rust: &BTreeMap<String, String>, roots: &[&str]) -> String {
    let source_index = index();
    let strict = Strict { declared, rust, index: &source_index, fallbacks: Default::default() };
    let mut used: Vec<String> = roots.iter().map(|root| root.to_string()).collect();
    let mut builders = Vec::new();
    let mut next = 0;
    let mut check_used: Vec<String> = roots.iter().map(|root| root.to_string()).collect();
    while next < used.len() {
        let name = used[next].clone();
        next += 1;
        let body = strict.build(&strict.shape(&name), "v", "p", 0, &mut used, &mut check_used);
        builders.push((name, body));
    }
    builders.sort();
    let mut functions = Vec::new();
    let mut next = 0;
    while next < check_used.len() {
        let name = check_used[next].clone();
        next += 1;
        let shape = shape_of(&declared[&name]);
        let body = check_expr(&shape, "v", declared, &mut check_used);
        functions.push((name, body));
    }
    functions.sort();
    let mut out = String::from(
        "const isRecord = (v: JsonValue | undefined): v is JsonObject => typeof v === 'object' && v !== null && !Array.isArray(v);\n\n",
    );
    out.push_str(BUILD_HELPERS);
    for (name, body) in &functions {
        out.push_str(&format!("function check{name}(v: JsonValue | undefined): boolean {{\n  return {body};\n}}\n\n"));
    }
    for (name, body) in &builders {
        out.push_str(&format!("function build{name}(v: JsonValue | undefined, p: string): {name} {{\n  return {body};\n}}\n\n"));
    }
    let mut guards: Vec<&str> = roots.to_vec();
    guards.sort_unstable();
    for (at, name) in guards.iter().enumerate() {
        if at > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "/** The check of a kernel `{name}` at the host boundary: every field, and every wire type nested in it. */\nexport function is{name}(value: JsonValue): value is JsonValue & {name} {{\n  return check{name}(value);\n}}\n\n/** The `{name}` the host received, as serde reads it: integers in range, defaults filled in, unknown fields dropped (or refused where the type denies them); a throw names the first path that is not one. */\nexport function decode{name}(value: JsonValue): {name} {{\n  return build{name}(value, '{name}');\n}}\n"
        ));
    }
    out
}

fn committed_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("../orbital-shell-typescript/core/src/generated/kernel-wire.ts")
}

#[test]
fn kernel_wire_ts_is_current() {
    let generated = kernel_wire_ts();
    let path = committed_path();
    if std::env::var("KERNEL_WIRE_TS_WRITE").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(committed) => assert_eq!(
            committed, generated,
            "kernel-wire.ts is stale: KERNEL_WIRE_TS_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_ts"
        ),
        Err(_) => assert!(
            !path.parent().unwrap().exists(),
            "kernel-wire.ts is missing: KERNEL_WIRE_TS_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_ts"
        ),
    }
}

#[test]
fn every_wire_type_is_declared() {
    let ts = kernel_wire_ts();
    for name in [
        "DispatchWire",
        "RoutedLeg",
        "SeedWire",
        "AppliedWire",
        "MountWire",
        "MountLegWire",
        "MountFailure",
        "StatefulLegWire",
        "FrameWire",
        "FiredTick",
        "FrameDispatch",
        "SnapshotWire",
        "AmbiguousTrait",
        "ServerProgressWire",
        "ResolveProgressWire",
        "PendingRequestWire",
        "IoRequestWire",
        "IoReplyWire",
        "IoAnswerWire",
        "CallerWire",
        "CallSiteWire",
        "Envelope",
        "InstanceRef",
        "DeliverRefusal",
        "DeliverRefusalKind",
        "HttpBody",
        "HttpRequestWire",
        "HttpResponseWire",
        "HttpFailureWire",
        "CallRoutesWire",
        "SubstrateFailureWire",
        "EventRequest",
        "EventResponse",
        "ClientEffect",
        "ClientEffectByTrait",
        "FrameDelta",
        "FrameOp",
        "FramesResync",
        "RepaintReason",
        "EmittedEvent",
        "BusEventSource",
        "ServerEffectResult",
        "TransitionRejection",
        "DeliveryRecord",
        "DispatchLog",
        "MountSeed",
        "TraitSnapshot",
        "AwaitingTrait",
        "DispatchMode",
        "NavigateOptions",
        "RowPredicate",
        "QueryCapabilities",
        "WriteCapabilities",
        "RowSetRef",
        "RowPage",
        "RowIdPage",
        "RowWriteOutcome",
        "PropositionObservation",
        "PropositionVerdict",
        "EvalContextWire",
        "EvalOutcomeWire",
        "GuardOutcomeWire",
        "EvalTracedWire",
        "EvalStepWire",
        "EvalStepKind",
        "RowBindingWire",
        "MutationOpWire",
        "MutationVerdictWire",
    ] {
        assert!(
            ts.contains(&format!("export type {name} = ")),
            "{name} missing from the generated mirror"
        );
    }
    assert!(ts.contains("export type Settled<T, E> = "));
    assert!(!ts.contains("bigint"));
}

#[test]
fn every_guarded_wire_type_has_an_exported_guard() {
    let ts = kernel_wire_ts();
    for name in GUARDED {
        assert!(
            ts.contains(&format!("export function is{name}(value: JsonValue): value is JsonValue & {name} {{")),
            "{name} has no generated guard"
        );
        assert!(ts.contains(&format!("export type {name} = ")), "{name} is guarded but not declared");
    }
    assert!(!ts.contains("export function isNotAWireType("), "control: an undeclared type has no guard");
}

#[test]
fn a_union_branch_of_a_leaf_type_is_chosen_by_that_leafs_own_builder() {
    let named = |name: &str| check_expr(&Shape::Named(name.to_string()), "v", &BTreeMap::new(), &mut Vec::new());
    assert_eq!(named("EntityRow"), "accepts((x, q) => entityRow(x, q), v)");
    assert_eq!(named("ServerBatchSummary"), "accepts((x, q) => serverBatchSummary(x, q), v)");
    let ts = kernel_wire_ts();
    assert!(ts.contains("accepts((x, q) => entityRow(x, q), r0[\"data\"])"), "ServerEffectResult.data picks its branch by builder");
    assert!(!ts.contains("!== undefined ? entityRow("), "control: a defined value no longer selects the row branch unchecked");
}

#[test]
fn every_guarded_wire_type_has_a_throwing_decoder_built_from_its_serde_form() {
    let ts = kernel_wire_ts();
    for name in GUARDED {
        assert!(ts.contains(&format!("export function decode{name}(value: JsonValue): {name} {{")), "{name} has no generated decoder");
        assert!(ts.contains(&format!("function build{name}(v: JsonValue | undefined, p: string): {name} {{")), "{name} has no builder");
    }
    assert!(!ts.contains("export function decodeNotAWireType("), "control: an undeclared type has no decoder");
}

fn strict_of(source: &str, decl: &str, name: &str) -> (String, Strict<'static>) {
    let declared: &'static BTreeMap<String, String> =
        Box::leak(Box::new(BTreeMap::from([(name.to_string(), decl.to_string())])));
    let rust: &'static BTreeMap<String, String> =
        Box::leak(Box::new(BTreeMap::from([(name.to_string(), name.to_string())])));
    let indexed: &'static BTreeMap<String, Vec<TypeInfo>> = Box::leak(Box::new(super::serde_index::index_of_source(source)));
    let strict = Strict { declared, rust, index: indexed, fallbacks: Default::default() };
    let body = strict.build(&strict.shape(name), "v", "p", 0, &mut vec![name.to_string()], &mut Vec::new());
    (body, strict)
}

#[test]
fn integer_fields_decode_with_the_range_of_their_rust_type() {
    let source = "#[derive(Deserialize)] struct S { a: u8, b: i64, c: usize, d: f64, e: Option<u32>, f: Vec<i16>, g: String }";
    let decl = "type S = { a: number, b: number, c: number, d: number, e?: number, f: Array<number>, g: string, }";
    let (body, _) = strict_of(source, decl, "S");
    assert!(body.contains("int(r0[\"a\"], p + '.a', 0, 255, \"u8\")"), "{body}");
    assert!(body.contains("int(r0[\"b\"], p + '.b', -Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER, \"i64\")"), "{body}");
    assert!(body.contains("int(r0[\"c\"], p + '.c', 0, Number.MAX_SAFE_INTEGER, \"usize\")"), "{body}");
    assert!(body.contains("int(r0[\"e\"], p + '.e', 0, 4294967295, \"u32\")"), "an Option's inner integer is ranged: {body}");
    assert!(body.contains("-32768, 32767, \"i16\""), "a list's elements are ranged: {body}");
    assert!(body.contains("num(r0[\"d\"], p + '.d')"), "control: a float stays a number: {body}");
    assert!(body.contains("str(r0[\"g\"], p + '.g')"), "control: a string stays a string: {body}");
}

#[test]
fn a_non_zero_integer_decodes_with_a_range_that_starts_at_one() {
    let source = "#[derive(Deserialize)] struct S { k: std::num::NonZeroUsize, n: usize }";
    let decl = "type S = { k: number, n: number, }";
    let (body, _) = strict_of(source, decl, "S");
    assert!(body.contains("int(r0[\"k\"], p + '.k', 1, Number.MAX_SAFE_INTEGER, \"NonZeroUsize\")"), "{body}");
    assert!(body.contains("int(r0[\"n\"], p + '.n', 0, Number.MAX_SAFE_INTEGER, \"usize\")"), "control: usize still starts at zero: {body}");
}

#[test]
fn unknown_fields_are_refused_only_where_the_type_denies_them() {
    let denying = "#[derive(Deserialize)] #[serde(deny_unknown_fields)] struct D { a: String, b: u8 }";
    let lax = "#[derive(Deserialize)] struct D { a: String, b: u8 }";
    let decl = "type D = { a: string, b: number, }";
    let (body, _) = strict_of(denying, decl, "D");
    assert!(body.contains("obj(v, p, [\"a\", \"b\"], "), "{body}");
    let (body, _) = strict_of(lax, decl, "D");
    assert!(body.contains("obj(v, p, null, "), "control: serde drops unknown fields: {body}");
    assert!(!body.contains("[\"a\", \"b\"]"), "{body}");
    let tagged = "#[derive(Deserialize)] #[serde(tag = \"kind\", deny_unknown_fields)] enum E { X { n: u8 }, Y }";
    let decl = "type E = { \"kind\": \"X\", n: number, } | { \"kind\": \"Y\" }";
    let (body, _) = strict_of(tagged, decl, "E");
    assert!(body.contains("obj(v, p, [\"kind\", \"n\"], "), "a tagged enum's container denial reaches its variants: {body}");
}

#[test]
fn a_defaulted_field_decodes_to_the_rust_default_when_missing() {
    let source = "#[derive(Deserialize)] struct P { #[serde(default)] scopes: Vec<String>, #[serde(default)] n: u32, #[serde(default)] t: String, #[serde(default)] m: HashMap<String, u8>, plain: String, maybe: Option<String> }";
    let decl = "type P = { scopes: Array<string>, n: number, t: string, m: { [key in string]: number }, plain: string, maybe: string | null, }";
    let (body, _) = strict_of(source, decl, "P");
    assert!(body.contains("\"scopes\": r0[\"scopes\"] === undefined ? [] : list("), "{body}");
    assert!(body.contains("\"n\": r0[\"n\"] === undefined ? 0 : int("), "{body}");
    assert!(body.contains("\"t\": r0[\"t\"] === undefined ? '' : str("), "{body}");
    assert!(body.contains("\"m\": r0[\"m\"] === undefined ? {} : dict("), "{body}");
    assert!(body.contains("\"maybe\": r0[\"maybe\"] === undefined ? null : "), "a missing Option is None: {body}");
    assert!(body.contains("\"plain\": str("), "control: a field without a default is required: {body}");
    let container = "#[derive(Deserialize)] #[serde(default)] struct C { a: bool }";
    let (body, _) = strict_of(container, "type C = { a: boolean, }", "C");
    assert!(body.contains("\"a\": r0[\"a\"] === undefined ? false : bool("), "a container default reaches every field: {body}");
}

#[test]
#[should_panic(expected = "default = \"make\"")]
fn a_default_function_the_generator_cannot_evaluate_is_refused_loudly() {
    let source = "#[derive(Deserialize)] struct F { #[serde(default = \"make\")] n: u32 }";
    strict_of(source, "type F = { n: number, }", "F");
}

#[test]
fn the_types_decoded_by_their_printed_shape_alone_are_the_ones_serde_cannot_describe() {
    let cfg = Config::new().with_large_int("number");
    let collect = collect_all(&cfg);
    let source_index = index();
    let strict = Strict { declared: &collect.decls, rust: &collect.rust, index: &source_index, fallbacks: Default::default() };
    let mut used: Vec<String> = GUARDED.iter().map(|n| n.to_string()).collect();
    let mut next = 0;
    while next < used.len() {
        let name = used[next].clone();
        next += 1;
        strict.build(&strict.shape(&name), "v", "p", 0, &mut used, &mut Vec::new());
    }
    let fallbacks = strict.fallbacks.borrow();
    let names: Vec<&str> = fallbacks.keys().map(String::as_str).collect();
    assert_eq!(names, ["Accepted", "CanvasTheme", "ClientEffect", "DispatchMode", "FrameDispatch", "FrameOp", "OsOp", "RepaintReason", "ResolvedMotion", "Settled"], "{fallbacks:#?}");
    assert!(!fallbacks.contains_key("IoRequestWire"), "control: IoRequestWire is decoded from its serde derive");
}
