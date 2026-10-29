//! The kind-string vocabulary parsed into `CompatType` and checked against the relation.

use super::*;

/// The legacy kind-string vocabulary, parsed into the relation's input space.
/// Markers exist for vocabulary the erased-kind alphabet carries but `OirType`
/// has no variant for (`entity`/`relation`, raw `int`/`float`/`null`/
/// `timestamp`, PascalCase named types, raw `Map<...>` tags).
#[derive(Debug, Clone, PartialEq)]
pub enum CompatType {
    Ty(OirType),
    Entity,
    Named(String),
    MapTag,
    Timestamp,
    MoneyTag,
    FileTag,
    Int,
    Float,
    Null,
    Unknown(String),
}

impl CompatType {
    /// A representative `OirType` for witness purposes when the parsed side
    /// is a marker rather than a real type; `None` for a spelling nothing declares.
    pub fn witness_type(&self) -> Option<OirType> {
        Some(match self {
            CompatType::Ty(t) => t.clone(),
            CompatType::Entity => OirType::Reference {
                entity: "entity".into(),
            },
            CompatType::Named(n) => OirType::Reference { entity: n.clone() },
            CompatType::MapTag => OirType::Map {
                key: Box::new(OirType::String),
                value: Box::new(OirType::erased()),
            },
            CompatType::Timestamp => OirType::DateTime,
            CompatType::MoneyTag => OirType::Money,
            CompatType::FileTag => OirType::File,
            CompatType::Int => OirType::Integer,
            CompatType::Float => OirType::Number,
            CompatType::Null => OirType::absent_slot(),
            CompatType::Unknown(_) => return None,
        })
    }
}

/// The strings of a closed string set written as a literal union (`"a" | "b"`), when `t` is one.
pub fn literal_union_members(text: &str) -> Option<Vec<String>> {
    decode_literal_union(text).ok().flatten()
}

/// Distinguishes nominal names from valid closed sets and malformed quoted sets.
pub fn decode_literal_union(text: &str) -> Result<Option<Vec<String>>, &'static str> {
    if !text.trim_start().starts_with('"') {
        return Ok(None);
    }
    quoted_literal_union_members(text).map(Some).ok_or("invalid quoted literal union")
}

pub(super) fn quoted_literal_union_members(text: &str) -> Option<Vec<String>> {
    let mut remaining = text.trim_start();
    let mut members = Vec::new();
    loop {
        let mut chars = remaining.char_indices();
        if chars.next()?.1 != '"' { return None; }
        let mut token = String::from("\"");
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in chars {
            if escaped {
                token.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => { token.push(ch); escaped = true; }
                '"' => { token.push(ch); end = Some(index + ch.len_utf8()); break; }
                ch if ch < ' ' => {
                    // Older shape text wrote control characters without JSON escaping.
                    let encoded = serde_json::to_string(&ch.to_string()).ok()?;
                    token.push_str(&encoded[1..encoded.len() - 1]);
                }
                _ => token.push(ch),
            }
        }
        members.push(serde_json::from_str::<String>(&token).ok()?);
        remaining = remaining.get(end?..)?.trim_start();
        if remaining.is_empty() { return Some(members); }
        remaining = remaining.strip_prefix('|')?.trim_start();
    }
}

/// The literal union (`"a" | "b"`) that writes a nonempty closed string set.
pub fn literal_union_text(values: &[String]) -> Option<String> {
    if values.is_empty() { return None; }
    Some(values.iter().map(|value| serde_json::to_string(value).expect("a string serializes")).collect::<Vec<_>>().join(" | "))
}

pub(super) fn is_string_literal_union(text: &str) -> bool {
    literal_union_members(text).is_some()
}

pub fn parse_compat_type(s: &str) -> CompatType {
    let t = s.trim_end_matches('!');
    match t {
        "entity" | "relation" => CompatType::Entity,
        "null" => CompatType::Null,
        "int" => CompatType::Int,
        "float" => CompatType::Float,
        "timestamp" => CompatType::Timestamp,
        "string" => CompatType::Ty(OirType::String),
        "number" => CompatType::Ty(OirType::Number),
        "boolean" => CompatType::Ty(OirType::Boolean),
        "date" => CompatType::Ty(OirType::Date),
        "datetime" => CompatType::Ty(OirType::DateTime),
        "instant" => CompatType::Ty(OirType::Instant),
        "email" => CompatType::Ty(OirType::Email),
        "url" => CompatType::Ty(OirType::Url),
        "phone" => CompatType::Ty(OirType::Phone),
        "uuid" => CompatType::Ty(OirType::Uuid),
        "image" => CompatType::Ty(OirType::Image),
        "bytes" => CompatType::Ty(OirType::Bytes),
        "callkey" | "CallKey" => CompatType::Ty(OirType::CallKey),
        "trait" => CompatType::Ty(OirType::Trait),
        "orbital" => CompatType::Ty(OirType::Orbital),
        "behavior" => CompatType::Ty(OirType::Behavior),
        // Raw `money`/`file` are NOT their erased kinds: the legacy predicate
        // special-cased them only as DECLARED literals (`money` accepts an
        // actual `number`; `file` an actual `object`), never as actuals — so
        // they get markers with exactly those one-way arms.
        "money" => CompatType::MoneyTag,
        "file" => CompatType::FileTag,
        "sexpr" | "SExpr" => CompatType::Ty(OirType::SExpr),
        "node" => CompatType::Ty(OirType::Node),
        // A tagged union of struct variants (`type Pick = A | B` in a payload):
        // an object-family declaration — the variants themselves are checked
        // by the variant-aware validators.
        "union" => CompatType::Ty(OirType::Union { variants: crate::ir::types::no_fields() }),
        // A schemaless object — an empty Struct is its nearest `OirType` shape;
        // the family arm makes every cross-variant object pairing erase.
        "object" => CompatType::Ty(OirType::Struct { fields: crate::ir::types::no_fields(), additional: None }),
        "array" => CompatType::Ty(OirType::array(OirType::erased())),
        _ if tuple_type_slots(t).is_some() => {
            let items = tuple_type_slots(t)
                .unwrap_or_default()
                .into_iter()
                .map(|slot| match parse_compat_type(slot) {
                    CompatType::Ty(ty) => ty,
                    _ => OirType::erased(),
                })
                .collect();
            CompatType::Ty(OirType::Tuple { items })
        }
        _ if t.starts_with('[') && t.ends_with(']') && t.len() > 1 => {
            let inner = match &t[1..t.len() - 1] {
                "string" => OirType::String,
                // Faithful to the declared kind (4.4): `[int]`/`[integer]` are
                // Array<Integer>, not the Number collapse — `sub`'s Element
                // arm still accepts `[integer]` where `[number]` is declared.
                "number" | "float" => OirType::Number,
                "int" | "integer" => OirType::Integer,
                "boolean" => OirType::Boolean,
                "orbital" => OirType::Orbital,
                "trait" => OirType::Trait,
                "behavior" => OirType::Behavior,
                nested if nested.starts_with('[') => match parse_compat_type(nested) {
                    CompatType::Ty(ty) => ty,
                    _ => OirType::erased(),
                },
                declared => match parse_compat_type(declared) {
                    CompatType::Ty(ty) => ty,
                    CompatType::Named(_) => OirType::Struct { fields: crate::ir::types::no_fields(), additional: None },
                    CompatType::MapTag => OirType::map(OirType::String, OirType::erased()),
                    _ => OirType::erased(),
                },
            };
            CompatType::Ty(OirType::Array {
                inner: Box::new(inner),
            })
        }
        _ if t.starts_with("Map<") && t.ends_with('>') => CompatType::MapTag,
        _ if t.len() > 1 && t.starts_with('$') => CompatType::Ty(OirType::Var {
            name: t[1..].to_string().into(),
        }),
        // `"a" | "b"`: a closed set of strings is an enum of strings.
        _ if is_string_literal_union(t) => CompatType::Ty(OirType::Enum {
            name: t.to_string(),
        }),
        _ if t.chars().next().is_some_and(char::is_uppercase) => {
            match crate::language::program_types::program_type(t) {
                Some(shape) => CompatType::Ty(shape.clone()),
                None => CompatType::Named(t.to_string()),
            }
        }
        _ => CompatType::Unknown(t.to_string()),
    }
}

/// The relation over the parsed vocabulary — the single place the legacy
/// string predicate's decision table now lives. `Ty`×`Ty` delegates to
/// [`sub`]; markers carry the string-only arms (`entity`/named types take a row or id string, raw `int`/`float`/`null` are
/// identity-only, `timestamp` is a date family
/// that does NOT cross-accept `date`/`datetime` names, and a raw `Map<...>`
/// tag counts as an object on both sides — plus the PascalCase quirk of also
/// accepting a string-kind actual).
pub fn sub_compat(src: &CompatType, dst: &CompatType) -> Option<Coercion> {
    use CompatType::*;
    // Optional is transparent, mirroring `sub`'s own prelude — inert for
    // string callers (`parse_compat_type` has no Optional spelling),
    // load-bearing once a caller passes a real `Ty(Optional<T>)`.
    if let Ty(OirType::Optional { inner }) = dst {
        return sub_compat(src, &Ty((**inner).clone())).map(|c| Coercion::Wrap(Box::new(c)));
    }
    if let Ty(OirType::Optional { inner }) = src {
        return sub_compat(&Ty((**inner).clone()), dst).map(|c| Coercion::Unwrap {
            checked: false,
            inner: Box::new(c),
        });
    }
    match (src, dst) {
        (Ty(s), Ty(d)) => sub(s, d),
        (_, Entity) => match src {
            Ty(t) if is_object_family(t) => Some(Coercion::Row { from_id: false }),
            Ty(t) if is_string_kind(t) => Some(Coercion::Row { from_id: true }),
            Entity => Some(Coercion::Identity),
            _ => None,
        },
        (_, Named(n)) => match src {
            Ty(t) if is_object_family(t) => Some(Coercion::Row { from_id: false }),
            Ty(t) if is_string_kind(t) => Some(Coercion::Row { from_id: true }),
            Named(m) if m == n => Some(Coercion::Identity),
            _ => None,
        },
        // Raw `Map<...>` tags: the object family on both sides, and — via the
        // legacy predicate's PascalCase arm — a string-kind actual too.
        (_, MapTag) => match src {
            Ty(t) if is_object_family(t) || is_json_family(t) => {
                Some(Coercion::Erase { from: t.clone() })
            }
            Ty(t) if is_string_kind(t) => Some(Coercion::Erase { from: t.clone() }),
            MapTag => Some(Coercion::Identity),
            _ => None,
        },
        (MapTag, Ty(d)) if is_object_family(d) || is_json_family(d) => {
            src.witness_type().map(|from| Coercion::Erase { from })
        }
        (_, Timestamp) => match src {
            Ty(t) if is_string_kind(t) => Some(Coercion::Widen(WidenKind::StringToDateTime)),
            Timestamp => Some(Coercion::Identity),
            _ => None,
        },
        // Raw `money` declared accepts exactly an actual `number`; as an
        // actual it matches nothing but `money` (the legacy asymmetry).
        (_, MoneyTag) => match src {
            Ty(OirType::Number) => Some(Coercion::Widen(WidenKind::MoneyNumber)),
            MoneyTag => Some(Coercion::Identity),
            _ => None,
        },
        // A real `Ty(Money)` declared side is the same kind the marker stands in for.
        (MoneyTag, Ty(OirType::Money)) => Some(Coercion::Identity),
        // Raw `file` declared accepts exactly an actual object-family value.
        (_, FileTag) => match src {
            Ty(t) if is_object_family(t) => Some(Coercion::Erase { from: t.clone() }),
            FileTag => Some(Coercion::Identity),
            _ => None,
        },
        // A real `Ty(File)` declared side is the same kind the marker stands in for.
        (FileTag, Ty(OirType::File)) => Some(Coercion::Identity),
        (_, Int) => match src {
            Int => Some(Coercion::Identity),
            _ => None,
        },
        (_, Float) => match src {
            Float => Some(Coercion::Identity),
            _ => None,
        },
        (_, Null) => match src {
            Null => Some(Coercion::Identity),
            _ => None,
        },
        (Unknown(s), Unknown(d)) if s == d => Some(Coercion::Identity),
        _ => None,
    }
}

/// Returns `true` when a value of declared type `actual` may be assigned to a
/// field of declared type `declared`. Thin wrapper over the `sub` relation:
/// both sides are parsed from the legacy kind-string vocabulary and the
/// relation's witness decides. The acceptance set is exactly the legacy
/// string predicate's — Optional-transparent, element-blind arrays, the
/// object/json/Map family, semantic-string refinements,
/// entity/named row-or-id, `Enum`/`Trait` as `string`.
/// The slot types of a `.lolo` tuple type `[T1, T2, …]` (two or more top-level slots;
/// a single `[T]` is an array), or `None`. The one tuple-type parser every consumer of
/// a type string uses.
pub fn tuple_type_slots(type_str: &str) -> Option<Vec<&str>> {
    let t = type_str.trim().trim_end_matches('!');
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    let mut slots = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in inner.char_indices() {
        match c {
            '[' | '{' | '<' | '(' => depth += 1,
            ']' | '}' | '>' | ')' => depth -= 1,
            ',' if depth == 0 => {
                slots.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    slots.push(inner[start..].trim());
    (slots.len() >= 2 && slots.iter().all(|s| !s.is_empty())).then_some(slots)
}

pub fn set_value_type_compatible(declared: &str, actual: &str) -> bool {
    let d = parse_compat_type(declared);
    let a = parse_compat_type(actual);
    sub_compat(&a, &d).is_some_and(|c| !c.requires_cast())
}
