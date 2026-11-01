//! The Swift and Kotlin mirrors of the embed wire, generated into the native shells' bridge packs from the same
//! declarations the TypeScript mirror is (`ts-rs` for the printed shape, the serde derive for integer widths and
//! flattened fields). A model throws on a payload that is not its shape; nothing defaults a missing field.
//!
//! `KERNEL_WIRE_NATIVE_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_native` rewrites both files;
//! without it the tests fail when a committed file is stale.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ts_rs::Config;

use super::super::serde_index::{flat_index, flat_index_of_source, index, Flat};
use super::{annotate, collect_all, record_of, shape_of, Shape, Strict, TypeInfo};

/// The wire types the native bridges decode; every wire type reachable from one is generated with it.
const NATIVE_ROOTS: &[&str] = &[
    "AppliedWire",
    "DispatchMode",
    "DispatchWire",
    "EventRequest",
    "EventResponse",
    "FrameDelta",
    "FrameWire",
    "TraitSnapshot",
];

/// Wire types a native model does not unfold: a union of tuples or a recursive union cannot be a struct, so the
/// type is an opaque JSON value that its consumer reads once, by its declared form.
const OPAQUE: &[&str] = &["ClientEffect", "FrameOp", "RowPredicate"];

#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Text,
    Int(String),
    Double,
    Bool,
    Json,
    Named(String),
    Opt(Box<Ty>),
    List(Box<Ty>),
    Map(Box<Ty>),
    Tuple(Vec<Ty>),
}

#[derive(Debug, Clone)]
enum Decl {
    Struct(Vec<(String, Ty)>),
    Enum(Vec<String>),
    Flat { own: Vec<(String, Ty)>, flattened: Vec<(String, String)> },
}

struct Model {
    decls: BTreeMap<String, Decl>,
    tuples: BTreeMap<String, Vec<Ty>>,
    opaque_used: BTreeSet<String>,
}

fn leaf(name: &str) -> Option<Ty> {
    match name {
        "JsonValue" | "FieldValue" | "EventPayloadValue" | "ServerBatchSummary" | "JsonObject" | "EntityRow" | "EventPayload" => Some(Ty::Json),
        "EventId" | "OrbitalId" | "TraitId" | "NamespacedEffectName" => Some(Ty::Text),
        _ => None,
    }
}

struct Resolver<'a> {
    strict: &'a Strict<'a>,
    flats: &'a BTreeMap<String, Flat>,
    opaque: &'a [&'a str],
    model: Model,
    queue: Vec<String>,
}

impl Resolver<'_> {
    fn named(&mut self, name: &str, at: &str) -> Ty {
        if let Some(found) = leaf(name) {
            return found;
        }
        if self.opaque.contains(&name) {
            self.model.opaque_used.insert(name.to_string());
            return Ty::Json;
        }
        assert!(self.strict.declared.contains_key(name), "kernel_wire_native: `{at}` names `{name}`, which is not a wire type");
        if !self.queue.iter().any(|q| q == name) {
            self.queue.push(name.to_string());
        }
        Ty::Named(name.to_string())
    }

    fn ty(&mut self, shape: &Shape, at: &str) -> Ty {
        match shape {
            Shape::Any => Ty::Json,
            Shape::Text => Ty::Text,
            Shape::Number => Ty::Double,
            Shape::Int(rust) => Ty::Int(rust.clone()),
            Shape::Bool => Ty::Bool,
            Shape::Literal(text) if text == "true" || text == "false" => Ty::Bool,
            Shape::Array(item) => Ty::List(Box::new(self.ty(item, at))),
            Shape::Map(item) => Ty::Map(Box::new(self.ty(item, at))),
            Shape::Tuple(items) => {
                let parts: Vec<Ty> = items.iter().enumerate().map(|(i, item)| self.ty(item, &format!("{at}[{i}]"))).collect();
                assert!((2..=4).contains(&parts.len()), "kernel_wire_native: `{at}` is a tuple of {}", parts.len());
                self.model.tuples.insert(tuple_name(&parts), parts.clone());
                Ty::Tuple(parts)
            }
            Shape::Named(name) => self.named(name, at),
            Shape::Union(items) => {
                let present: Vec<&Shape> = items.iter().filter(|i| !matches!(i, Shape::Null)).collect();
                let nullable = present.len() != items.len();
                let tys: Vec<Ty> = present.iter().map(|i| self.ty(i, at)).collect();
                let one = match tys.as_slice() {
                    [single] => single.clone(),
                    many if many.iter().all(|t| matches!(t, Ty::Json)) => Ty::Json,
                    _ => panic!("kernel_wire_native: `{at}` is a union the generator does not model"),
                };
                if nullable { Ty::Opt(Box::new(one)) } else { one }
            }
            other => panic!("kernel_wire_native: `{at}` has a shape the generator does not model: {other:?}"),
        }
    }

    fn fields(&mut self, owner: &str, shape: &Shape) -> Vec<(String, Ty)> {
        let (fields, _) = record_of(shape).unwrap_or_else(|| panic!("kernel_wire_native: `{owner}` is not an object"));
        fields
            .iter()
            .map(|f| {
                let at = format!("{owner}.{}", f.key);
                let ty = self.ty(&f.shape, &at);
                let ty = if f.optional && !matches!(ty, Ty::Opt(_)) { Ty::Opt(Box::new(ty)) } else { ty };
                (f.key.clone(), ty)
            })
            .collect()
    }

    fn flat(&mut self, name: &str, flat: &Flat) -> Decl {
        let printed = shape_of(&self.strict.declared[name]);
        let Shape::Object(fields) = &printed else { panic!("kernel_wire_native: `{name}` flattens a type but is not printed as an object") };
        let mut own = Vec::new();
        for info in &flat.own {
            let (_, optional, shape) = fields
                .iter()
                .find(|(key, ..)| *key == info.key)
                .unwrap_or_else(|| panic!("kernel_wire_native: `{name}.{}` is a serde field the printed type lacks", info.key));
            let ty = self.ty(&annotate(shape, &info.ty), &format!("{name}.{}", info.key));
            own.push((info.key.clone(), if *optional && !matches!(ty, Ty::Opt(_)) { Ty::Opt(Box::new(ty)) } else { ty }));
        }
        let mut flattened = Vec::new();
        for (field, rust) in &flat.flattened {
            let ts = self
                .strict
                .rust
                .iter()
                .find(|(_, r)| *r == rust)
                .map(|(ts, _)| ts.clone())
                .unwrap_or_else(|| panic!("kernel_wire_native: `{name}` flattens `{rust}`, which is not a printed wire type"));
            self.named(&ts, name);
            flattened.push((field.clone(), ts));
        }
        Decl::Flat { own, flattened }
    }

    fn decl(&mut self, name: &str) -> Decl {
        let rust = self.strict.rust.get(name).cloned().unwrap_or_default();
        if let Some(flat) = self.flats.get(&rust).cloned() {
            return self.flat(name, &flat);
        }
        let shape = self.strict.shape(name);
        match &shape {
            Shape::Literal(l) if l.starts_with('"') => Decl::Enum(vec![l.trim_matches('"').to_string()]),
            Shape::Record(..) | Shape::Object(_) => Decl::Struct(self.fields(name, &shape)),
            Shape::Union(items) if items.iter().all(|i| matches!(i, Shape::Literal(l) if l.starts_with('"'))) => Decl::Enum(
                items
                    .iter()
                    .map(|i| match i {
                        Shape::Literal(l) => l.trim_matches('"').to_string(),
                        _ => unreachable!(),
                    })
                    .collect(),
            ),
            other => panic!("kernel_wire_native: `{name}` is not a struct or a string enum ({other:?}); name it in OPAQUE or model it"),
        }
    }
}

fn resolve(strict: &Strict<'_>, flats: &BTreeMap<String, Flat>, roots: &[&str], opaque: &[&str]) -> Model {
    let mut resolver = Resolver {
        strict,
        flats,
        opaque,
        model: Model { decls: BTreeMap::new(), tuples: BTreeMap::new(), opaque_used: BTreeSet::new() },
        queue: roots.iter().map(|r| r.to_string()).collect(),
    };
    let mut next = 0;
    while next < resolver.queue.len() {
        let name = resolver.queue[next].clone();
        next += 1;
        let decl = resolver.decl(&name);
        resolver.model.decls.insert(name, decl);
    }
    resolver.model
}

fn type_label(ty: &Ty) -> String {
    match ty {
        Ty::Text => "String".to_string(),
        Ty::Int(rust) => format!("Int{rust}"),
        Ty::Double => "Double".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Json => "Json".to_string(),
        Ty::Named(n) => n.clone(),
        Ty::Opt(t) => format!("Opt{}", type_label(t)),
        Ty::List(t) => format!("ListOf{}", type_label(t)),
        Ty::Map(t) => format!("MapOf{}", type_label(t)),
        Ty::Tuple(items) => tuple_name(items),
    }
}

fn tuple_name(items: &[Ty]) -> String {
    let mut label = String::from("Tuple");
    for item in items {
        let part = type_label(item);
        label.push_str(&part[..1].to_uppercase());
        label.push_str(&part[1..]);
    }
    label
}

fn identifier(word: &str, at: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in word.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            if upper {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            upper = false;
        } else if matches!(c, '-' | '$' | '.' | '/') {
            upper = !out.is_empty();
        } else {
            panic!("kernel_wire_native: `{word}` ({at}) has no identifier form");
        }
    }
    assert!(!out.is_empty() && !out.starts_with(|c: char| c.is_ascii_digit()), "kernel_wire_native: `{word}` ({at}) has no identifier form");
    out
}

fn enum_cases(values: &[String], at: &str) -> Vec<(String, String)> {
    let cases: Vec<(String, String)> = values.iter().map(|v| (identifier(v, at), v.clone())).collect();
    let names: BTreeSet<&String> = cases.iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), cases.len(), "kernel_wire_native: `{at}` has two values with one identifier");
    cases
}

const SWIFT_KEYWORDS: &[&str] = &[
    "associatedtype", "class", "deinit", "enum", "extension", "fileprivate", "func", "import", "init", "inout", "internal", "let",
    "open", "operator", "private", "precedencegroup", "protocol", "public", "rethrows", "static", "struct", "subscript",
    "typealias", "var", "break", "case", "catch", "continue", "default", "defer", "do", "else", "fallthrough", "for", "guard", "if",
    "in", "repeat", "return", "throw", "switch", "where", "while", "Any", "as", "await", "false", "is", "nil", "self", "Self", "super",
    "throws", "true", "try",
];

const KOTLIN_KEYWORDS: &[&str] = &[
    "as", "break", "class", "continue", "do", "else", "false", "for", "fun", "if", "in", "interface", "is", "null", "object",
    "package", "return", "super", "this", "throw", "true", "try", "typealias", "typeof", "val", "var", "when", "while",
];

fn name_in(word: &str, keywords: &[&str], at: &str) -> String {
    let ident = identifier(word, at);
    assert_eq!(ident, word, "kernel_wire_native: the key `{word}` ({at}) is not an identifier");
    if keywords.contains(&ident.as_str()) { format!("`{ident}`") } else { ident }
}

fn swift_int(rust: &str) -> &'static str {
    match rust {
        "u8" => "UInt8",
        "u16" => "UInt16",
        "u32" => "UInt32",
        "u64" | "usize" | "NonZeroUsize" => "UInt64",
        "i8" => "Int8",
        "i16" => "Int16",
        "i32" => "Int32",
        "i64" | "isize" => "Int64",
        other => panic!("kernel_wire_native: `{other}` has no Swift integer type"),
    }
}

fn kotlin_int(rust: &str) -> &'static str {
    match rust {
        "u8" | "u16" | "i8" | "i16" | "i32" => "Int",
        "u32" | "u64" | "usize" | "NonZeroUsize" | "i64" | "isize" => "Long",
        other => panic!("kernel_wire_native: `{other}` has no Kotlin integer type"),
    }
}

fn swift_ty(ty: &Ty) -> String {
    match ty {
        Ty::Text => "String".to_string(),
        Ty::Int(rust) => swift_int(rust).to_string(),
        Ty::Double => "Double".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Json => "AnyCodable".to_string(),
        Ty::Named(n) => format!("Kernel{n}"),
        Ty::Opt(t) => format!("{}?", swift_ty(t)),
        Ty::List(t) => format!("[{}]", swift_ty(t)),
        Ty::Map(t) => format!("[String: {}]", swift_ty(t)),
        Ty::Tuple(items) => format!("Kernel{}", tuple_name(items)),
    }
}

fn kotlin_ty(ty: &Ty) -> String {
    match ty {
        Ty::Text => "String".to_string(),
        Ty::Int(rust) => kotlin_int(rust).to_string(),
        Ty::Double => "Double".to_string(),
        Ty::Bool => "Boolean".to_string(),
        Ty::Json => "JsonElement".to_string(),
        Ty::Named(n) => format!("Kernel{n}"),
        Ty::Opt(t) => format!("{}?", kotlin_ty(t)),
        Ty::List(t) => format!("List<{}>", kotlin_ty(t)),
        Ty::Map(t) => format!("Map<String, {}>", kotlin_ty(t)),
        Ty::Tuple(items) => format!("Kernel{}", tuple_name(items)),
    }
}

const TUPLE_SLOTS: [&str; 4] = ["first", "second", "third", "fourth"];

fn swift(model: &Model) -> String {
    let mut out = String::from(
        "// GENERATED by `cargo test -p orbital-core-runtime --lib kernel_wire_native` (KERNEL_WIRE_NATIVE_WRITE=1) from\n\
         // orbital-core `embed::wire`. Do not edit: change the Rust wire types and regenerate.\n\n\
         import Foundation\n",
    );
    for (name, items) in &model.tuples {
        let name = format!("Kernel{name}");
        out.push_str(&format!("\nstruct {name}: Codable, Equatable {{\n"));
        for (slot, item) in TUPLE_SLOTS.iter().zip(items) {
            out.push_str(&format!("    let {slot}: {}\n", swift_ty(item)));
        }
        let params: Vec<String> = TUPLE_SLOTS.iter().zip(items).map(|(slot, item)| format!("_ {slot}: {}", swift_ty(item))).collect();
        out.push_str(&format!("\n    init({}) {{\n", params.join(", ")));
        for slot in TUPLE_SLOTS.iter().take(items.len()) {
            out.push_str(&format!("        self.{slot} = {slot}\n"));
        }
        out.push_str("    }\n\n    init(from decoder: Decoder) throws {\n        var items = try decoder.unkeyedContainer()\n");
        out.push_str(&format!(
            "        guard items.count == {} else {{\n            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: \"expected a list of {}, got \\(items.count ?? -1)\"))\n        }}\n",
            items.len(),
            items.len()
        ));
        for (slot, item) in TUPLE_SLOTS.iter().zip(items) {
            out.push_str(&format!("        {slot} = try items.decode({}.self)\n", swift_ty(item)));
        }
        out.push_str("    }\n\n    func encode(to encoder: Encoder) throws {\n        var items = encoder.unkeyedContainer()\n");
        for slot in TUPLE_SLOTS.iter().take(items.len()) {
            out.push_str(&format!("        try items.encode({slot})\n"));
        }
        out.push_str("    }\n}\n");
    }
    for (name, decl) in &model.decls {
        match decl {
            Decl::Enum(values) => {
                out.push_str(&format!("\nenum Kernel{name}: String, Codable {{\n"));
                for (case, raw) in enum_cases(values, name) {
                    let case = name_in(&case, SWIFT_KEYWORDS, name);
                    out.push_str(&format!("    case {case} = {raw:?}\n"));
                }
                out.push_str("}\n");
            }
            Decl::Struct(fields) => {
                out.push_str(&format!("\nstruct Kernel{name}: Codable {{\n"));
                for (key, ty) in fields {
                    let key = name_in(key, SWIFT_KEYWORDS, name);
                    match ty {
                        Ty::Opt(_) => out.push_str(&format!("    var {key}: {} = nil\n", swift_ty(ty))),
                        _ => out.push_str(&format!("    let {key}: {}\n", swift_ty(ty))),
                    }
                }
                out.push_str("}\n");
            }
            Decl::Flat { own, flattened } => {
                out.push_str(&format!("\nstruct Kernel{name}: Codable {{\n"));
                for (key, ty) in own {
                    out.push_str(&format!("    let {}: {}\n", name_in(key, SWIFT_KEYWORDS, name), swift_ty(ty)));
                }
                for (field, ts) in flattened {
                    out.push_str(&format!("    let {}: Kernel{ts}\n", name_in(field, SWIFT_KEYWORDS, name)));
                }
                out.push_str("\n    private enum CodingKeys: String, CodingKey {\n");
                for (key, _) in own {
                    out.push_str(&format!("        case {} = {key:?}\n", name_in(key, SWIFT_KEYWORDS, name)));
                }
                out.push_str("    }\n\n    init(from decoder: Decoder) throws {\n        let own = try decoder.container(keyedBy: CodingKeys.self)\n");
                for (key, ty) in own {
                    let id = name_in(key, SWIFT_KEYWORDS, name);
                    let call = match ty {
                        Ty::Opt(inner) => format!("try own.decodeIfPresent({}.self, forKey: .{id})", swift_ty(inner)),
                        _ => format!("try own.decode({}.self, forKey: .{id})", swift_ty(ty)),
                    };
                    out.push_str(&format!("        {id} = {call}\n"));
                }
                for (field, ts) in flattened {
                    out.push_str(&format!("        {} = try Kernel{ts}(from: decoder)\n", name_in(field, SWIFT_KEYWORDS, name)));
                }
                out.push_str("    }\n\n    func encode(to encoder: Encoder) throws {\n        var own = encoder.container(keyedBy: CodingKeys.self)\n");
                for (key, ty) in own {
                    let id = name_in(key, SWIFT_KEYWORDS, name);
                    let call = if matches!(ty, Ty::Opt(_)) { "encodeIfPresent" } else { "encode" };
                    out.push_str(&format!("        try own.{call}({id}, forKey: .{id})\n"));
                }
                for (field, _) in flattened {
                    out.push_str(&format!("        try {}.encode(to: encoder)\n", name_in(field, SWIFT_KEYWORDS, name)));
                }
                out.push_str("    }\n}\n");
            }
        }
    }
    out
}

fn kotlin(model: &Model) -> String {
    let mut out = String::from(
        "// GENERATED by `cargo test -p orbital-core-runtime --lib kernel_wire_native` (KERNEL_WIRE_NATIVE_WRITE=1) from\n\
         // orbital-core `embed::wire`. Do not edit: change the Rust wire types and regenerate.\n\n\
         package generated.patterns.core\n\n\
         import kotlinx.serialization.KSerializer\nimport kotlinx.serialization.Serializable\nimport kotlinx.serialization.SerialName\nimport kotlinx.serialization.SerializationException\nimport kotlinx.serialization.descriptors.SerialDescriptor\nimport kotlinx.serialization.encoding.Decoder\nimport kotlinx.serialization.encoding.Encoder\nimport kotlinx.serialization.json.JsonArray\nimport kotlinx.serialization.json.JsonDecoder\nimport kotlinx.serialization.json.JsonElement\nimport kotlinx.serialization.json.JsonEncoder\nimport kotlinx.serialization.json.JsonObject\nimport kotlinx.serialization.json.decodeFromJsonElement\nimport kotlinx.serialization.json.encodeToJsonElement\n",
    );
    for (name, items) in &model.tuples {
        let name = format!("Kernel{name}");
        let params: Vec<String> = TUPLE_SLOTS.iter().zip(items).map(|(slot, item)| format!("val {slot}: {}", kotlin_ty(item))).collect();
        out.push_str(&format!(
            "\n@Serializable(with = {name}Serializer::class)\ndata class {name}({})\n\nobject {name}Serializer : KSerializer<{name}> {{\n    override val descriptor: SerialDescriptor = JsonArray.serializer().descriptor\n\n    override fun deserialize(decoder: Decoder): {name} {{\n        val json = (decoder as JsonDecoder).json\n        val items = decoder.decodeJsonElement() as? JsonArray ?: throw SerializationException(\"expected a list of {}\")\n        if (items.size != {}) throw SerializationException(\"expected a list of {}, got ${{items.size}}\")\n        return {name}(",
            params.join(", "),
            items.len(),
            items.len(),
            items.len()
        ));
        let reads: Vec<String> = items.iter().enumerate().map(|(i, item)| format!("json.decodeFromJsonElement<{}>(items[{i}])", kotlin_ty(item))).collect();
        out.push_str(&reads.join(", "));
        out.push_str(&format!(")\n    }}\n\n    override fun serialize(encoder: Encoder, value: {name}) {{\n        val json = (encoder as JsonEncoder).json\n        encoder.encodeJsonElement(JsonArray(listOf("));
        let writes: Vec<String> = TUPLE_SLOTS.iter().take(items.len()).map(|slot| format!("json.encodeToJsonElement(value.{slot})")).collect();
        out.push_str(&writes.join(", "));
        out.push_str(")))\n    }\n}\n");
    }
    for (name, decl) in &model.decls {
        match decl {
            Decl::Enum(values) => {
                out.push_str(&format!("\n@Serializable\nenum class Kernel{name} {{\n"));
                let cases = enum_cases(values, name);
                for (at, (case, raw)) in cases.iter().enumerate() {
                    let case = name_in(case, KOTLIN_KEYWORDS, name);
                    let end = if at + 1 == cases.len() { "" } else { "," };
                    out.push_str(&format!("    @SerialName({}) {case}{end}\n", format!("{raw:?}").replace('$', "\\$")));
                }
                out.push_str("}\n");
            }
            Decl::Struct(fields) => {
                out.push_str(&format!("\n@Serializable\ndata class Kernel{name}(\n"));
                let lines: Vec<String> = fields
                    .iter()
                    .map(|(key, ty)| {
                        let key = name_in(key, KOTLIN_KEYWORDS, name);
                        match ty {
                            Ty::Opt(_) => format!("    val {key}: {} = null", kotlin_ty(ty)),
                            _ => format!("    val {key}: {}", kotlin_ty(ty)),
                        }
                    })
                    .collect();
                out.push_str(&lines.join(",\n"));
                out.push_str("\n)\n");
            }
            Decl::Flat { own, flattened } => {
                let mut params: Vec<String> = own.iter().map(|(key, ty)| format!("val {}: {}", name_in(key, KOTLIN_KEYWORDS, name), kotlin_ty(ty))).collect();
                params.extend(flattened.iter().map(|(field, ts)| format!("val {}: Kernel{ts}", name_in(field, KOTLIN_KEYWORDS, name))));
                out.push_str(&format!(
                    "\n@Serializable(with = Kernel{name}Serializer::class)\ndata class Kernel{name}({})\n\nobject Kernel{name}Serializer : KSerializer<Kernel{name}> {{\n    override val descriptor: SerialDescriptor = JsonObject.serializer().descriptor\n\n    override fun deserialize(decoder: Decoder): Kernel{name} {{\n        val json = (decoder as JsonDecoder).json\n        val node = decoder.decodeJsonElement() as? JsonObject ?: throw SerializationException(\"expected an object for {name}\")\n        return Kernel{name}(\n",
                    params.join(", ")
                ));
                let mut reads: Vec<String> = Vec::new();
                for (key, ty) in own {
                    let id = name_in(key, KOTLIN_KEYWORDS, name);
                    let read = match ty {
                        Ty::Opt(inner) => format!("node[{key:?}]?.let {{ json.decodeFromJsonElement<{}?>(it) }}", kotlin_ty(inner)),
                        _ => format!("json.decodeFromJsonElement<{}>(node[{key:?}] ?: throw SerializationException(\"{name} lacks `{key}`\"))", kotlin_ty(ty)),
                    };
                    reads.push(format!("            {id} = {read}"));
                }
                for (field, ts) in flattened {
                    reads.push(format!("            {} = json.decodeFromJsonElement<Kernel{ts}>(node)", name_in(field, KOTLIN_KEYWORDS, name)));
                }
                out.push_str(&reads.join(",\n"));
                out.push_str(&format!("\n        )\n    }}\n\n    override fun serialize(encoder: Encoder, value: Kernel{name}) {{\n        throw SerializationException(\"Kernel{name} is read from the kernel, never written\")\n    }}\n}}\n"));
            }
        }
    }
    out
}

fn model_of(strict: &Strict<'_>, flats: &BTreeMap<String, Flat>) -> Model {
    resolve(strict, flats, NATIVE_ROOTS, OPAQUE)
}

fn generated() -> (String, String) {
    let cfg = Config::new().with_large_int("number");
    let collect = collect_all(&cfg);
    let source_index = index();
    let strict = Strict { declared: &collect.decls, rust: &collect.rust, index: &source_index, fallbacks: Default::default() };
    let model = model_of(&strict, &flat_index());
    for name in OPAQUE {
        assert!(model.opaque_used.contains(*name), "kernel_wire_native: `{name}` is listed as opaque but no native model reaches it");
    }
    (swift(&model), kotlin(&model))
}

fn committed_path(relative: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join("..").join(relative)
}

const SWIFT_PATH: &str = "orbital-shell-ios/renderers/swiftui/renderers/bridge/generated/KernelWire.swift";
const KOTLIN_PATH: &str = "orbital-shell-android/renderers/compose/renderers/bridge/generated/KernelWire.kt";

fn is_current(relative: &str, generated: &str) {
    let path = committed_path(relative);
    if std::env::var("KERNEL_WIRE_NATIVE_WRITE").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, generated).unwrap();
        return;
    }
    let shell = path.ancestors().nth(path.strip_prefix(committed_path("")).unwrap().components().count()).unwrap().join(relative.split('/').next().unwrap());
    match std::fs::read_to_string(&path) {
        Ok(committed) => assert_eq!(
            committed, generated,
            "{relative} is stale: KERNEL_WIRE_NATIVE_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_native"
        ),
        Err(_) => assert!(
            !shell.exists(),
            "{relative} is missing: KERNEL_WIRE_NATIVE_WRITE=1 cargo test -p orbital-core-runtime --lib kernel_wire_native"
        ),
    }
}

#[test]
fn kernel_wire_native_swift_is_current() {
    is_current(SWIFT_PATH, &generated().0);
}

#[test]
fn kernel_wire_native_kotlin_is_current() {
    is_current(KOTLIN_PATH, &generated().1);
}

fn model_of_source(source: &str, decls: &[(&str, &str)], roots: &[&str], opaque: &[&str]) -> Model {
    let declared: &'static BTreeMap<String, String> =
        Box::leak(Box::new(decls.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect()));
    let rust: &'static BTreeMap<String, String> = Box::leak(Box::new(decls.iter().map(|(n, _)| (n.to_string(), n.to_string())).collect()));
    let indexed: &'static BTreeMap<String, Vec<TypeInfo>> = Box::leak(Box::new(super::super::serde_index::index_of_source(source)));
    let strict = Strict { declared, rust, index: indexed, fallbacks: Default::default() };
    resolve(&strict, &flat_index_of_source(source), roots, opaque)
}

#[test]
fn the_generated_models_cover_every_native_root() {
    let (swift, kotlin) = generated();
    for root in NATIVE_ROOTS {
        assert!(swift.contains(&format!(" Kernel{root}: ")), "{root} has no Swift model");
        assert!(kotlin.contains(&format!("class Kernel{root}")), "{root} has no Kotlin model");
    }
    assert!(!swift.contains("KernelNotAWireType"), "control: an undeclared type has no model");
}

#[test]
fn integers_keep_their_rust_width_and_a_float_stays_a_double() {
    let source = "#[derive(Serialize)] struct S { a: u8, b: u64, c: i32, d: f64, e: Option<usize>, f: Vec<u32> }";
    let decl = "type S = { a: number, b: number, c: number, d: number, e?: number, f: Array<number>, }";
    let model = model_of_source(source, &[("S", decl)], &["S"], &[]);
    let (swift, kotlin) = (swift(&model), kotlin(&model));
    for line in ["let a: UInt8", "let b: UInt64", "let c: Int32", "let d: Double", "var e: UInt64? = nil", "let f: [UInt32]"] {
        assert!(swift.contains(line), "{line} in {swift}");
    }
    for line in ["val a: Int,", "val b: Long,", "val c: Int,", "val d: Double,", "val e: Long? = null,", "val f: List<Long>"] {
        assert!(kotlin.contains(line), "{line} in {kotlin}");
    }
}

#[test]
fn a_string_enum_becomes_an_enum_with_the_wire_spelling() {
    let source = "#[derive(Serialize)] enum E { A, B }";
    let model = model_of_source(source, &[("E", "type E = \"no-match\" | \"hop-cap\"")], &["E"], &[]);
    let (swift, kotlin) = (swift(&model), kotlin(&model));
    assert!(swift.contains("case noMatch = \"no-match\""), "{swift}");
    assert!(kotlin.contains("@SerialName(\"hop-cap\") hopCap"), "{kotlin}");
}

#[test]
fn a_dollar_in_a_wire_value_is_escaped_in_the_kotlin_string_and_not_in_the_swift_one() {
    let model = model_of_source("#[derive(Serialize)] enum E { A }", &[("E", "type E = \"$MOUNT\"")], &["E"], &[]);
    assert!(kotlin(&model).contains("@SerialName(\"\\$MOUNT\") MOUNT"), "{}", kotlin(&model));
    assert!(swift(&model).contains("case MOUNT = \"$MOUNT\""), "control: Swift keeps the dollar");
}

#[test]
fn a_pair_decodes_only_from_a_list_of_exactly_two() {
    let source = "#[derive(Serialize)] struct S { pairs: Vec<(String, String)> }";
    let model = model_of_source(source, &[("S", "type S = { pairs: Array<[string, string]>, }")], &["S"], &[]);
    let (swift, kotlin) = (swift(&model), kotlin(&model));
    assert!(swift.contains("guard items.count == 2 else"), "{swift}");
    assert!(swift.contains("let pairs: [KernelTupleStringString]"), "{swift}");
    assert!(kotlin.contains("if (items.size != 2) throw SerializationException"), "{kotlin}");
}

#[test]
fn a_flattened_struct_decodes_its_own_fields_and_then_the_flattened_type_from_the_same_object() {
    let source = "#[derive(Serialize)] struct Inner { n: u64 } #[derive(Serialize)] struct Outer { #[serde(rename = \"trait\")] trait_name: String, #[serde(flatten)] inner: Inner }";
    let model = model_of_source(
        source,
        &[("Outer", "type Outer = { trait: string, n: number, }"), ("Inner", "type Inner = { n: number, }")],
        &["Outer"],
        &[],
    );
    let (swift, kotlin) = (swift(&model), kotlin(&model));
    assert!(swift.contains("let inner: KernelInner"), "{swift}");
    assert!(swift.contains("inner = try KernelInner(from: decoder)"), "{swift}");
    assert!(swift.contains("let n: UInt64"), "the flattened type keeps its integer width: {swift}");
    assert!(kotlin.contains("inner = json.decodeFromJsonElement<KernelInner>(node)"), "{kotlin}");
}

#[test]
fn an_optional_key_decodes_when_absent_and_a_required_key_never_defaults() {
    let source = "#[derive(Serialize)] struct S { a: String, #[serde(skip_serializing_if = \"Option::is_none\")] b: Option<String> }";
    let model = model_of_source(source, &[("S", "type S = { a: string, b?: string, }")], &["S"], &[]);
    let (swift, kotlin) = (swift(&model), kotlin(&model));
    assert!(swift.contains("let a: String\n"), "control: a required field has no default: {swift}");
    assert!(swift.contains("var b: String? = nil"), "{swift}");
    assert!(kotlin.contains("val a: String,"), "{kotlin}");
    assert!(kotlin.contains("val b: String? = null"), "{kotlin}");
}

#[test]
#[should_panic(expected = "name it in OPAQUE or model it")]
fn a_tagged_union_the_generator_does_not_model_is_refused_loudly() {
    let source = "#[derive(Serialize)] #[serde(tag = \"kind\")] enum U { X, Y }";
    model_of_source(source, &[("U", "type U = { \"kind\": \"X\" } | { \"kind\": \"Y\" }")], &["U"], &[]);
}

#[test]
#[should_panic(expected = "which is not a wire type")]
fn a_field_naming_an_unknown_type_is_refused_loudly() {
    let source = "#[derive(Serialize)] struct S { a: Mystery }";
    model_of_source(source, &[("S", "type S = { a: Mystery, }")], &["S"], &[]);
}
