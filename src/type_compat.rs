//! Canonical type-compatibility predicate shared across the Orbital crates.
//!
//! Single source of truth for "is this value's declared type assignable to the
//! declared entity-field type" in a `(set @entity.X <value>)` check. Both the
//! lolo L1 parse-time check and the compiler L2 post-resolve backstop call
//! this one function so the two layers can never drift.
//!
//! Phase 4.1 (spec: Almadar_Compiler_Spec.md Phase 4): the acceptance set is
//! now owned by the `sub`/`Coercion` relation — a structurally recursive,
//! exhaustive-by-construction function over all `OirType` variants, so adding
//! a variant forces a decision here instead of letting a string comparator
//! quietly answer `false`. `set_value_type_compatible` is a thin wrapper that
//! parses the legacy kind-string vocabulary into the relation. The relation is
//! lenient by design (it only rejects clear kind contradictions, deferring
//! element-type drift to the structural checks elsewhere); the remaining
//! leniency is enumerated in `mod compat_tests`, not implicit.

use crate::ir::OirType;

/// The semantic string domain a declared type names — the payload of a
/// [`Coercion::Refine`] (String → Email/Url/… is runtime-validated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticStringKind {
    Email,
    Url,
    Phone,
    Uuid,
    Image,
}

/// What a [`Coercion::Widen`] widens. The names describe the ASSIGNMENT
/// direction (src → dst); several cover both directions because the legacy
/// string predicate collapsed the numeric kinds and admitted both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidenKind {
    /// Integer ↔ Number (the legacy predicate erased both to `"number"`).
    IntegerNumber,
    /// Number/Integer ↔ Money (Money is numeric at rest).
    MoneyNumber,
    /// A value of one of a primitive union's member kinds joins the union.
    ToUnionMember,
    /// String-kind value parsed into a Date.
    StringToDate,
    /// String-kind value parsed into a DateTime.
    StringToDateTime,
    /// Numeric value read as a Date.
    NumericToDate,
    /// Numeric value read as a DateTime.
    NumericToDateTime,
    /// Any scalar renders as a node ("any rendered value is a ReactNode").
    ScalarToNode,
}

/// The witness for an accepted assignment `sub(src, dst) = Some(c)`: WHAT the
/// assignment costs, so widening becomes countable instead of invisible
/// (Type_Preservation §4.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Coercion {
    /// Structurally equal types (also kind-level equalities the legacy
    /// predicate could not distinguish, e.g. two different `Enum` names).
    Identity,
    Widen(WidenKind),
    /// T → Optional<T> (Phase 4.3), wrapping the inner witness.
    Wrap(Box<Coercion>),
    /// Optional<T> → T (Phase 4.3), unwrapping the inner witness.
    Unwrap {
        checked: bool,
        inner: Box<Coercion>,
    },
    /// Lift through Array value (structural since Phase 4.2 — the inner
    /// coercion is the element witness). Map values stay element-blind
    /// until their own arm tightening.
    Element(Box<Coercion>),
    /// Reference assignment: id string (`from_id: true`) or loaded row.
    Row {
        from_id: bool,
    },
    /// String-kind value entering a runtime-validated semantic domain.
    Refine(SemanticStringKind),
    /// Assignment that erases static information; `from` records what was
    /// lost (→ Json, cross-variant object family, → node content, → any).
    Erase {
        from: OirType,
    },
}

fn is_object_family(t: &OirType) -> bool {
    matches!(
        t,
        OirType::Struct { .. }
            | OirType::Union { .. }
            | OirType::Map { .. }
            | OirType::File
            | OirType::TypeRef { .. }
            | OirType::Row { .. }
    )
}

/// What an S-expression can be besides an options object: an atom, or a list of S-expressions.
fn is_program_data(t: &OirType) -> bool {
    match t {
        OirType::Optional { inner } => is_program_data(inner),
        OirType::Boolean => true,
        OirType::Array { inner } => is_program_data(inner) || is_object_family(inner) || is_json_family(inner),
        OirType::Tuple { items } => items
            .iter()
            .all(|i| is_program_data(i) || is_object_family(i) || is_json_family(i)),
        _ => is_string_kind(t) || is_number_kind(t),
    }
}

fn is_json_family(t: &OirType) -> bool {
    matches!(t, OirType::Json | OirType::SExpr)
}

/// Kinds the legacy predicate erased to `"string"`: String itself, and the
/// string-valued Enum/Trait (via `oir_type_kind`).
fn is_string_kind(t: &OirType) -> bool {
    matches!(t, OirType::String | OirType::Enum { .. } | OirType::Trait)
}

/// Kinds erased to `"number"`: Number, Integer, and Money (numeric at rest).
fn is_number_kind(t: &OirType) -> bool {
    matches!(t, OirType::Number | OirType::Integer | OirType::Money)
}

fn semantic_kind(t: &OirType) -> Option<SemanticStringKind> {
    match t {
        OirType::Email => Some(SemanticStringKind::Email),
        OirType::Url => Some(SemanticStringKind::Url),
        OirType::Phone => Some(SemanticStringKind::Phone),
        OirType::Uuid => Some(SemanticStringKind::Uuid),
        OirType::Image => Some(SemanticStringKind::Image),
        _ => None,
    }
}

/// True when `dst` is a union of primitive kinds (`string | number`) and `src` fits one member.
fn primitive_union_member(dst: &OirType, src: &OirType) -> bool {
    let OirType::Union { variants } = dst else {
        return false;
    };
    variants.iter().any(|v| {
        !matches!(
            v.field_type,
            OirType::Struct { .. } | OirType::Union { .. } | OirType::Map { .. } | OirType::Json
        ) && sub(src, &v.field_type).is_some()
    })
}

/// The type relation: `Some(coercion)` when a value of type `src` may be
/// assigned to a slot of type `dst`, `None` on a clear kind contradiction.
///
/// Exhaustive over all `OirType` variants: every arm's acceptance set is
/// reverse-engineered from the legacy string predicate (both sides erased by
/// `oir_type_kind`), decision-for-decision — Optional transparent both
/// directions, arrays element-blind, the object/json/Map family mutually
/// compatible, semantic-string refinements from any string-kind
/// source, the numeric kinds widening upward (Integer/Money → Number, Integer → Money) but never narrowing back to Integer, Reference taking a row
/// (object family) or an id string, and `Node` accepting any primitive (the
/// widening arm moved in from config_type.rs).
pub fn sub(src: &OirType, dst: &OirType) -> Option<Coercion> {
    // Optional is transparent both directions (mirrors `oir_type_kind`); the
    // witness is unchecked — Optional is accepted via an unchecked unwrap.
    if let OirType::Optional { inner } = dst {
        return sub(src, inner).map(|c| Coercion::Wrap(Box::new(c)));
    }
    if let OirType::Optional { inner } = src {
        return sub(inner, dst).map(|c| Coercion::Unwrap { checked: false, inner: Box::new(c) });
    }
    if src == dst {
        return Some(Coercion::Identity);
    }
    match dst {
        OirType::Optional { .. } => unreachable!("Optional stripped above"),
        // Enum/Trait are erased to "string"; a string-kind source is the
        // kind-level equality the legacy predicate admitted.
        OirType::Enum { .. } | OirType::Trait => match src {
            _ if is_string_kind(src) => Some(Coercion::Identity),
            _ => None,
        },
        // An orbital value is only ever an orbital value (identity is caught above).
        OirType::Orbital => None,
        OirType::Behavior => None,
        OirType::String => match src {
            _ if is_string_kind(src) => Some(Coercion::Identity),
            _ => None,
        },
        OirType::Number => match src {
            OirType::Integer => Some(Coercion::Widen(WidenKind::IntegerNumber)),
            OirType::Money => Some(Coercion::Widen(WidenKind::MoneyNumber)),
            _ => None,
        },
        // Integer is the narrower numeric kind: Number/Money do not narrow
        // back down to Integer (4.4); only Integer == Integer, caught above.
        OirType::Integer => None,
        OirType::Boolean => None,
        OirType::Date => match src {
            _ if is_string_kind(src) => Some(Coercion::Widen(WidenKind::StringToDate)),
            _ if is_number_kind(src) => Some(Coercion::Widen(WidenKind::NumericToDate)),
            _ => None,
        },
        OirType::DateTime => match src {
            _ if is_string_kind(src) => Some(Coercion::Widen(WidenKind::StringToDateTime)),
            _ if is_number_kind(src) => Some(Coercion::Widen(WidenKind::NumericToDateTime)),
            _ => None,
        },
        OirType::Email | OirType::Url | OirType::Phone | OirType::Uuid | OirType::Image => {
            match src {
                _ if is_string_kind(src) => {
                    Some(Coercion::Refine(semantic_kind(dst).expect("semantic dst")))
                }
                _ => None,
            }
        }
        OirType::Money => match src {
            OirType::Number | OirType::Integer => Some(Coercion::Widen(WidenKind::MoneyNumber)),
            _ => None,
        },
        // The object family (Struct/Union/Map/File/TypeRef) and the json family
        // (Json/SExpr) are mutually compatible.
        OirType::Struct { .. }
        | OirType::Union { .. }
        | OirType::Map { .. }
        | OirType::File
        | OirType::TypeRef { .. }
        | OirType::Row { .. }
        | OirType::Json
        | OirType::SExpr => match src {
            _ if is_object_family(src) || is_json_family(src) => {
                Some(Coercion::Erase { from: src.clone() })
            }
            _ if matches!(dst, OirType::SExpr) && is_program_data(src) => {
                Some(Coercion::Erase { from: src.clone() })
            }
            _ if primitive_union_member(dst, src) => Some(Coercion::Widen(WidenKind::ToUnionMember)),
            _ => None,
        },
        // Renderable content: object-family pattern trees, arrays of them,
        // string leaves — plus any scalar, which React paints as text (the
        // arm moved in from config_type.rs's file-local special case).
        OirType::Node => match src {
            _ if is_string_kind(src) || is_number_kind(src) || matches!(src, OirType::Boolean) => {
                Some(Coercion::Widen(WidenKind::ScalarToNode))
            }
            OirType::Array { .. } => Some(Coercion::Erase { from: src.clone() }),
            _ if is_object_family(src) => Some(Coercion::Erase { from: src.clone() }),
            _ => None,
        },
        // Structural (Phase 4.2): Array<T> ≤ Array<U> iff T ≤ U. Element
        // types that admit no coercion reject the assignment outright.
        OirType::Array { inner: d_inner } => match src {
            OirType::Array { inner: s_inner } => {
                sub(s_inner, d_inner).map(|element| Coercion::Element(Box::new(element)))
            }
            // The empty list is typed by the list slot it fills: the slot supplies the element.
            OirType::Tuple { items } if items.is_empty() => Some(Coercion::Identity),
            // A tuple is an array of its slots: every slot must fit the element.
            OirType::Tuple { items } => items
                .iter()
                .map(|slot| sub(slot, d_inner))
                .collect::<Option<Vec<_>>>()
                .and_then(|cs| cs.into_iter().next())
                .map(|element| Coercion::Element(Box::new(element))),
            _ => None,
        },
        // A tuple takes a same-arity tuple slot by slot, or an array whose element fits
        // every slot (a literal's arity is checked where the literal is validated).
        OirType::Tuple { items: d_items } => match src {
            OirType::Tuple { items: s_items } if s_items.len() == d_items.len() => s_items
                .iter()
                .zip(d_items)
                .map(|(s, d)| sub(s, d))
                .collect::<Option<Vec<_>>>()
                .map(|_| Coercion::Identity),
            // An array of unknowable elements (a literal's erased `array` kind) erases.
            OirType::Array { .. } if is_unknowable(src) => Some(Coercion::Erase { from: src.clone() }),
            OirType::Array { inner: s_inner } => d_items
                .iter()
                .map(|d| sub(s_inner, d))
                .collect::<Option<Vec<_>>>()
                .map(|_| Coercion::Identity),
            _ => None,
        },
        // An entity field takes a loaded row (object family) or an id string.
        OirType::Reference { .. } => match src {
            _ if is_object_family(src) => Some(Coercion::Row { from_id: false }),
            _ if is_string_kind(src) => Some(Coercion::Row { from_id: true }),
            _ => None,
        },
    }
}

/// The legacy kind-string vocabulary, parsed into the relation's input space.
/// Markers exist for vocabulary the erased-kind alphabet carries but `OirType`
/// has no variant for (`any`, `entity`/`relation`, raw `int`/`float`/`null`/
/// `timestamp`, PascalCase named types, raw `Map<...>` tags).
#[derive(Debug, Clone, PartialEq)]
enum CompatType {
    Ty(OirType),
    Any,
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
    /// is a marker rather than a real type.
    fn witness_type(&self) -> OirType {
        match self {
            CompatType::Ty(t) => t.clone(),
            CompatType::Entity => OirType::Reference { entity: "entity".into() },
            CompatType::Named(n) => OirType::Reference { entity: n.clone() },
            CompatType::MapTag => OirType::Map {
                key: Box::new(OirType::String),
                value: Box::new(OirType::Json),
            },
            CompatType::Timestamp => OirType::DateTime,
            CompatType::MoneyTag => OirType::Money,
            CompatType::FileTag => OirType::File,
            CompatType::Int => OirType::Integer,
            CompatType::Float => OirType::Number,
            CompatType::Null => OirType::Optional { inner: Box::new(OirType::Json) },
            CompatType::Any | CompatType::Unknown(_) => OirType::Json,
        }
    }
}

fn parse_compat_type(s: &str) -> CompatType {
    let t = s.trim_end_matches('!');
    match t {
        "any" => CompatType::Any,
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
        "email" => CompatType::Ty(OirType::Email),
        "url" => CompatType::Ty(OirType::Url),
        "phone" => CompatType::Ty(OirType::Phone),
        "uuid" => CompatType::Ty(OirType::Uuid),
        "image" => CompatType::Ty(OirType::Image),
        "trait" => CompatType::Ty(OirType::Trait),
        "orbital" => CompatType::Ty(OirType::Orbital),
        "behavior" => CompatType::Ty(OirType::Behavior),
        // Raw `money`/`file` are NOT their erased kinds: the legacy predicate
        // special-cased them only as DECLARED literals (`money` accepts an
        // actual `number`; `file` an actual `object`), never as actuals — so
        // they get markers with exactly those one-way arms.
        "money" => CompatType::MoneyTag,
        "file" => CompatType::FileTag,
        "json" => CompatType::Ty(OirType::Json),
        "node" => CompatType::Ty(OirType::Node),
        // A tagged union of struct variants (`type Pick = A | B` in a payload):
        // an object-family declaration — the variants themselves are checked
        // by the variant-aware validators.
        "union" => CompatType::Ty(OirType::Union { variants: vec![] }),
        // A schemaless object — an empty Struct is its nearest `OirType` shape;
        // the family arm makes every cross-variant object pairing erase.
        "object" => CompatType::Ty(OirType::Struct { fields: vec![] }),
        "array" => CompatType::Ty(OirType::Array { inner: Box::new(OirType::Json) }),
        _ if tuple_type_slots(t).is_some() => {
            let items = tuple_type_slots(t)
                .unwrap_or_default()
                .into_iter()
                .map(|slot| match parse_compat_type(slot) {
                    CompatType::Ty(ty) => ty,
                    _ => OirType::Json,
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
                _ => OirType::Json,
            };
            CompatType::Ty(OirType::Array { inner: Box::new(inner) })
        }
        _ if t.starts_with("Map<") && t.ends_with('>') => CompatType::MapTag,
        _ if t.chars().next().is_some_and(char::is_uppercase) => CompatType::Named(t.to_string()),
        _ => CompatType::Unknown(t.to_string()),
    }
}

/// The relation over the parsed vocabulary — the single place the legacy
/// string predicate's decision table now lives. `Ty`×`Ty` delegates to
/// [`sub`]; markers carry the string-only arms (`any` accepts everything,
/// `entity`/named types take a row or id string, raw `int`/`float`/`null` are
/// identity-only, `timestamp` is a date family
/// that does NOT cross-accept `date`/`datetime` names, and a raw `Map<...>`
/// tag counts as an object on both sides — plus the PascalCase quirk of also
/// accepting a string-kind actual).
fn sub_compat(src: &CompatType, dst: &CompatType) -> Option<Coercion> {
    use CompatType::*;
    // Optional is transparent, mirroring `sub`'s own prelude — inert for
    // string callers (`parse_compat_type` has no Optional spelling),
    // load-bearing once a caller passes a real `Ty(Optional<T>)`.
    if let Ty(OirType::Optional { inner }) = dst {
        return sub_compat(src, &Ty((**inner).clone())).map(|c| Coercion::Wrap(Box::new(c)));
    }
    if let Ty(OirType::Optional { inner }) = src {
        return sub_compat(&Ty((**inner).clone()), dst)
            .map(|c| Coercion::Unwrap { checked: false, inner: Box::new(c) });
    }
    match (src, dst) {
        (_, Any) => Some(Coercion::Erase { from: src.witness_type() }),
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
            Some(Coercion::Erase { from: src.witness_type() })
        }
        (_, Timestamp) => match src {
            Ty(t) if is_string_kind(t) => Some(Coercion::Widen(WidenKind::StringToDateTime)),
            Ty(t) if is_number_kind(t) => Some(Coercion::Widen(WidenKind::NumericToDateTime)),
            Timestamp => Some(Coercion::Identity),
            _ => None,
        },
        // Raw `money` declared accepts exactly an actual `number`; as an
        // actual it matches nothing but `money`/`any` (the legacy asymmetry).
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
/// entity/named row-or-id, `any` top, `Enum`/`Trait` as `string`.
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
    sub_compat(&a, &d).is_some()
}

/// The semantic domain a declared type-name names, if it names one. The single
/// owner: L1 (lolo), L2 (validator) and `orbital-verify` all route through this
/// so no layer can invent its own vocabulary.
pub fn semantic_string_kind(declared: &str) -> Option<&'static str> {
    match declared.trim_end_matches('!') {
        "email" => Some("email"),
        "url" => Some("url"),
        "phone" => Some("phone"),
        "uuid" => Some("uuid"),
        "image" => Some("image"),
        _ => None,
    }
}

/// Does `value` satisfy the semantic domain `kind`? Mirrors
/// `isSemanticStringValue` in `@almadar/core`; `image` is a URL.
pub fn semantic_string_value_valid(kind: &str, value: &str) -> bool {
    match kind {
        "email" => {
            let mut parts = value.split('@');
            let local = parts.next().unwrap_or("");
            let domain = parts.next().unwrap_or("");
            parts.next().is_none()
                && !local.is_empty()
                && !local.contains(char::is_whitespace)
                && domain.contains('.')
                && !domain.contains(char::is_whitespace)
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        "url" | "image" => {
            (value.starts_with("http://") || value.starts_with("https://"))
                && value.len() > "https://".len()
                && !value.contains(char::is_whitespace)
        }
        "phone" => {
            let digits = value.chars().filter(|c| c.is_ascii_digit()).count();
            digits >= 7
                && value
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '(' | ')' | '.' | '-'))
        }
        "uuid" => {
            let groups: Vec<&str> = value.split('-').collect();
            groups.len() == 5
                && [8usize, 4, 4, 4, 12]
                    .iter()
                    .zip(groups.iter())
                    .all(|(len, g)| g.len() == *len && g.chars().all(|c| c.is_ascii_hexdigit()))
        }
        _ => true,
    }
}

/// Returns `true` when `value` is one of the declared union `members`.
/// The single source for enum/union membership (Gap G-LOLO-ENUM-UNENFORCED) —
/// both the lolo author-time check and the compiler validator call this, so the
/// two layers cannot drift.
pub fn enum_value_is_member(value: &str, members: &[String]) -> bool {
    members.iter().any(|m| m == value)
}

/// Map an inferred `OirType` to the lowercase kind-string vocabulary that
/// `set_value_type_compatible` speaks (`string`/`number`/`boolean`/`date`/
/// `array`/`object`/`json`, or a PascalCase entity name for references).
/// `Optional` is transparent (nullability is not a kind); `Json`/`SExpr` are the
/// "unknowable" kinds.
pub fn oir_type_kind(t: &crate::ir::OirType) -> String {
    use crate::ir::OirType;
    match t {
        OirType::String => "string".to_string(),
        OirType::Number | OirType::Integer => "number".to_string(),
        OirType::Boolean => "boolean".to_string(),
        OirType::Date => "date".to_string(),
        OirType::DateTime => "datetime".to_string(),
        OirType::Json | OirType::SExpr => "json".to_string(),
        OirType::Array { .. } | OirType::Tuple { .. } => "array".to_string(),
        OirType::Map { .. } | OirType::Struct { .. } | OirType::Row { .. } => "object".to_string(),
        // Every variant of a struct union is an object, so the KIND is object;
        // variant selection happens at the shape check, not the kind lattice.
        OirType::Union { .. } => "object".to_string(),
        OirType::Optional { inner } => oir_type_kind(inner),
        OirType::Reference { entity } => entity.clone(),
        OirType::Email => "email".to_string(),
        OirType::Url => "url".to_string(),
        OirType::Phone => "phone".to_string(),
        OirType::Uuid => "uuid".to_string(),
        OirType::Image => "image".to_string(),
        // Money is numeric at rest; File is the canonical struct value.
        OirType::Money => "number".to_string(),
        OirType::File => "object".to_string(),
        OirType::Enum { .. } => "string".to_string(),
        OirType::Trait => "string".to_string(),
        OirType::Orbital => "orbital".to_string(),
        OirType::Behavior => "behavior".to_string(),
        OirType::Node => "node".to_string(),
        // A by-name reference to a named struct type — same wire shape as the
        // struct it names, so it reports the same KIND as `Struct`/`Union`.
        OirType::TypeRef { .. } => "object".to_string(),
    }
}

/// An inferred type carrying no element information never rejects —
/// enumerated site leniency, not a special case. `Optional` is stripped
/// first: an `Optional<Json>` is still unknowable.
fn is_unknowable(t: &OirType) -> bool {
    let t = match t {
        OirType::Optional { inner } => inner.as_ref(),
        _ => t,
    };
    match t {
        OirType::Json | OirType::SExpr => true,
        OirType::Array { inner } => matches!(**inner, OirType::Json | OirType::SExpr),
        _ => false,
    }
}

/// Returns `true` when a value whose *inferred* type is `inferred` may be
/// assigned to a field of declared type-name `declared_field`.
///
/// The companion to `set_value_type_compatible` for the `(set @field <expr>)`
/// path where the value's type is *inferred* through the SExpression
/// (Gap G-LOLO-SET-OP-RETURN). **Conservative on unknowable inferred types**:
/// an open `Json`/`SExpr`, or an array whose element type is undeclared (e.g.
/// an input-dependent operator the std metadata can't pin down, or an empty
/// literal), never rejects — better to under-enforce than false-positive.
/// A concrete inferred type — including a concrete Array element — compares
/// structurally through `sub`, passed by its real `OirType`, not an erased
/// kind string.
pub fn oir_type_satisfies_field(inferred: &crate::ir::OirType, declared_field: &str) -> bool {
    if is_unknowable(inferred) {
        return true;
    }
    sub_compat(&CompatType::Ty(inferred.clone()), &parse_compat_type(declared_field)).is_some()
}

/// Structured sibling of [`oir_type_satisfies_field`] for callers that
/// already hold the declared side as a real `OirType` (an entity field's
/// resolved [`crate::field_shape`]) rather than a kind-string. Same
/// unknowable-inferred leniency, compared through `sub` directly — no
/// string round trip on either side.
pub fn oir_type_satisfies(inferred: &OirType, declared: &OirType) -> bool {
    is_unknowable(inferred) || sub(inferred, declared).is_some()
}

/// Structured sibling of [`set_value_type_compatible`]: `declared` is a real
/// `OirType`, `actual_kind` stays the legacy marker-string vocabulary parsed
/// through [`parse_compat_type`]. An erased-element actual (a bracket
/// spelling the parser can't resolve, e.g. `[Foo]`) gets the same
/// [`is_unknowable`] leniency the inferred side already has.
pub fn oir_type_accepts_kind(declared: &OirType, actual_kind: &str) -> bool {
    let actual = parse_compat_type(actual_kind);
    if let CompatType::Ty(t) = &actual {
        if is_unknowable(t) {
            return true;
        }
    }
    sub_compat(&actual, &CompatType::Ty(declared.clone())).is_some()
}

/// The legacy kind-string vocabulary's representative `OirType`, for callers
/// that hold a source spelling (a payload's declared kind) but want to
/// compare structurally rather than through the string-only relation.
/// Marker spellings with no real `OirType` (`any`, `entity`, raw
/// `int`/`float`/`null`, `Map<...>`, PascalCase named types) get their
/// [`CompatType::witness_type`] stand-in.
pub fn compat_kind_oir(kind: &str) -> OirType {
    parse_compat_type(kind).witness_type()
}

/// Shallow primitive-kind of a **literal** `SExpression` value, in the same
/// lowercase-kind vocabulary `set_value_type_compatible` speaks. This is the
/// single source both the lolo L1 emit-payload / `uses`-config-override check
/// and the compiler L2 emit-payload backstop call, so the two layers can't
/// drift (the same discipline as `set_value_type_compatible`).
///
/// Returns `None` for values whose type only the expression/binding resolver
/// can know — the caller must defer these to inference rather than compare a
/// literal kind:
/// - `@`-bindings and interpolated strings (`@entity.x`, `"pre@x"`), and
/// - operator **applications** (`(array/find …)`, `(object/get …)`, `(math/max …)`).
///
/// The `Array` arm mirrors `eval_expr`'s own call-vs-literal discrimination via
/// `get_operator`, so this check and the runtime never disagree on what is a
/// computed expression versus a literal array: an `Array` whose head is a known
/// operator is a call (defer, `None`); anything else — empty, or a head that is
/// not a known operator (e.g. `["red", "blue"]`) — is a literal array (`"array"`).
pub fn literal_value_type(value: &crate::schema::SExpression) -> Option<&'static str> {
    use crate::schema::SExpression;
    match value {
        SExpression::Number(_) => Some("number"),
        SExpression::Bool(_) => Some("boolean"),
        SExpression::String(s) => {
            if s.starts_with('@') || s.contains('@') {
                None
            } else {
                Some("string")
            }
        }
        SExpression::Null => None,
        SExpression::Object(_) => Some("object"),
        SExpression::Array(_) => {
            if value
                .operator()
                .and_then(crate::evaluator::get_operator)
                .is_some()
            {
                None
            } else {
                Some("array")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{enum_value_is_member, oir_type_satisfies_field};
    use crate::ir::OirType;

    #[test]
    fn a_list_of_a_nominal_std_type_is_that_list() {
        let orbitals = OirType::array(OirType::Orbital);
        assert!(super::oir_type_satisfies(&orbitals, &super::compat_kind_oir("[orbital]")));
        assert!(
            !super::oir_type_satisfies(&OirType::array(OirType::String), &super::compat_kind_oir("[orbital]")),
            "control: a list of strings is not a list of orbitals"
        );
        assert!(super::oir_type_satisfies(&OirType::array(OirType::Trait), &super::compat_kind_oir("[trait]")));
        assert!(super::oir_type_satisfies(&OirType::array(OirType::Behavior), &super::compat_kind_oir("[behavior]")));
    }

    #[test]
    fn the_empty_list_fits_a_list_of_any_element() {
        let empty = OirType::Tuple { items: Vec::new() };
        assert!(super::oir_type_satisfies(&empty, &OirType::array(OirType::String)));
        assert!(super::oir_type_satisfies(&empty, &OirType::array(OirType::array(OirType::Number))));
        assert!(
            !super::oir_type_satisfies(&empty, &OirType::String),
            "control: an empty list is not a string"
        );
    }

    #[test]
    fn enum_membership() {
        let members = vec!["active".to_string(), "archived".to_string()];
        assert!(enum_value_is_member("active", &members));
        assert!(!enum_value_is_member("zzz", &members));
    }

    #[test]
    fn oir_type_compat_concrete() {
        assert!(oir_type_satisfies_field(&OirType::Number, "number"));
        assert!(!oir_type_satisfies_field(&OirType::String, "number"));
        assert!(oir_type_satisfies_field(
            &OirType::Array {
                inner: Box::new(OirType::Json)
            },
            "array"
        ));
        // Optional is transparent for the kind check.
        assert!(oir_type_satisfies_field(
            &OirType::Optional {
                inner: Box::new(OirType::Number)
            },
            "number"
        ));
    }

    #[test]
    fn oir_type_unknowable_never_rejects() {
        // open Json / SExpr (input-dependent ops we can't pin down) → compatible.
        assert!(oir_type_satisfies_field(&OirType::Json, "number"));
        assert!(oir_type_satisfies_field(&OirType::SExpr, "boolean"));
    }

    #[test]
    fn literal_value_type_defers_operator_calls_but_types_literals() {
        use super::literal_value_type;
        use crate::schema::SExpression;
        let s = |x: &str| SExpression::String(x.to_string());

        // Operator applications are computed expressions → defer (None), so the
        // caller routes them to the expression inferencer instead of comparing a
        // literal kind. (CG-EMIT-PAYLOAD-SEXPR: these used to mis-type as "array".)
        let array_find = SExpression::Array(vec![s("array/find"), s("@entity.units")]);
        assert_eq!(literal_value_type(&array_find), None);
        let object_get_find = SExpression::Array(vec![
            s("object/get"),
            SExpression::Array(vec![s("array/find"), s("@entity.units")]),
            s("id"),
        ]);
        assert_eq!(literal_value_type(&object_get_find), None);

        // Genuine literal arrays still type as "array" (head not a known operator,
        // or empty) — matching eval_expr's own literal-vs-call discrimination.
        assert_eq!(
            literal_value_type(&SExpression::Array(vec![s("red"), s("blue")])),
            Some("array")
        );
        assert_eq!(
            literal_value_type(&SExpression::Array(vec![])),
            Some("array")
        );

        // Scalars + bindings unchanged.
        assert_eq!(
            literal_value_type(&SExpression::Number(1.0)),
            Some("number")
        );
        assert_eq!(
            literal_value_type(&SExpression::Bool(true)),
            Some("boolean")
        );
        assert_eq!(literal_value_type(&s("hello")), Some("string"));
        assert_eq!(literal_value_type(&s("@entity.x")), None);
        assert_eq!(literal_value_type(&SExpression::Null), None);
    }
}

#[cfg(test)]
mod compat_tests {
    use super::{
        compat_kind_oir, oir_type_accepts_kind, oir_type_satisfies, oir_type_satisfies_field,
        set_value_type_compatible, Coercion, SemanticStringKind, WidenKind,
    };
    use crate::ir::OirType::*;

    type C = Coercion;

    // The legacy string-level acceptance set — pinned as booleans so the
    // wrapper provably preserves the predicate every call site relied on.
    #[test]
    fn equal_and_required_marker() {
        assert!(set_value_type_compatible("string", "string"));
        assert!(set_value_type_compatible("string!", "string"));
        assert!(set_value_type_compatible("number", "number!"));
    }

    #[test]
    fn any_accepts_all() {
        assert!(set_value_type_compatible("any", "object"));
        assert!(set_value_type_compatible("any", "number"));
        // Asymmetry: `any` as the ACTUAL is only accepted by `any` itself.
        assert!(!set_value_type_compatible("string", "any"));
    }

    #[test]
    fn date_family_accepts_string_or_number() {
        assert!(set_value_type_compatible("date", "string"));
        assert!(set_value_type_compatible("timestamp", "number"));
        assert!(!set_value_type_compatible("date", "boolean"));
        // `timestamp` does not cross-accept `date`/`datetime` names.
        assert!(!set_value_type_compatible("date", "timestamp"));
        assert!(!set_value_type_compatible("datetime", "timestamp"));
    }

    #[test]
    fn entity_and_named_accept_object_or_string() {
        assert!(set_value_type_compatible("entity", "object"));
        assert!(set_value_type_compatible("relation", "string"));
        assert!(set_value_type_compatible("Article", "object"));
        assert!(set_value_type_compatible("Article", "string"));
        assert!(!set_value_type_compatible("Article", "number"));
        assert!(!set_value_type_compatible("Article", "json"));
        // Distinct named types do not inter-assign.
        assert!(!set_value_type_compatible("Article", "Comment"));
        // Reverse directions are rejected.
        assert!(!set_value_type_compatible("object", "Article"));
        assert!(!set_value_type_compatible("string", "Article"));
    }

    #[test]
    fn array_kinds_compatible_across_elements() {
        assert!(set_value_type_compatible("array", "[CacheEntry]"));
        assert!(set_value_type_compatible("[Foo]", "array"));
        assert!(set_value_type_compatible("[Foo]", "[Bar]"));
        assert!(!set_value_type_compatible("string", "[Foo]"));
        assert!(!set_value_type_compatible("object", "[Foo]"));
        assert!(!set_value_type_compatible("[Foo]", "object"));
        // A node accepts an array of pattern trees.
        assert!(set_value_type_compatible("node", "[Foo]"));
    }

    #[test]
    fn oir_type_satisfies_field_keeps_inferred_element_type() {
        let string_array = Array { inner: Box::new(String) };
        let number_array = Array { inner: Box::new(Number) };
        let json_array = Array { inner: Box::new(Json) };
        // A concrete inferred element type compares structurally, not by the
        // erased "array" kind — this is the 4.2-follow-up fix: the inferred
        // side is no longer round-tripped through `oir_type_kind`.
        assert!(oir_type_satisfies_field(&string_array, "[string]"));
        assert!(!oir_type_satisfies_field(&number_array, "[string]"));
        // An inferred array whose element is itself unknowable never rejects
        // (the `is_unknowable` leniency), regardless of the declared element.
        assert!(oir_type_satisfies_field(&json_array, "[string]"));
        // Optional is transparent on the inferred side.
        let opt_string_array = Optional { inner: Box::new(string_array.clone()) };
        assert!(oir_type_satisfies_field(&opt_string_array, "[string]"));
    }

    // `oir_type_satisfies` / `oir_type_accepts_kind` are the structured
    // siblings of `oir_type_satisfies_field` / `set_value_type_compatible` —
    // same acceptance set, declared side passed as a real `OirType` instead
    // of a kind-string (the `field_shape` callers' shape).
    #[test]
    fn oir_type_satisfies_mirrors_the_str_wrapper() {
        let string_array = Array { inner: Box::new(String) };
        let number_array = Array { inner: Box::new(Number) };
        assert!(oir_type_satisfies(&string_array, &Array { inner: Box::new(String) }));
        assert!(!oir_type_satisfies(&number_array, &Array { inner: Box::new(String) }));
        // Unknowable inferred types never reject, declared side real or not.
        assert!(oir_type_satisfies(&Json, &Array { inner: Box::new(String) }));
        assert!(oir_type_satisfies(&SExpr, &Number));
    }

    #[test]
    fn oir_type_accepts_kind_mirrors_the_str_wrapper() {
        assert!(oir_type_accepts_kind(
            &Array { inner: Box::new(String) },
            "[string]"
        ));
        assert!(!oir_type_accepts_kind(
            &Array { inner: Box::new(String) },
            "[number]"
        ));
        // A real `Reference` declared side still takes a row or an id string.
        let article = Reference { entity: "Article".into() };
        assert!(oir_type_accepts_kind(&article, "string"));
        assert!(oir_type_accepts_kind(&article, "object"));
        assert!(!oir_type_accepts_kind(&article, "number"));
        assert!(oir_type_accepts_kind(&Money, "money"));
        assert!(oir_type_accepts_kind(&Money, "number"));
        assert!(oir_type_accepts_kind(&File, "file"));
        assert!(oir_type_accepts_kind(&File, "object"));
        // An erased-element actual never rejects.
        assert!(oir_type_accepts_kind(
            &Array { inner: Box::new(Json) },
            "[json]"
        ));
        assert!(oir_type_accepts_kind(
            &Array {
                inner: Box::new(Array { inner: Box::new(Number) })
            },
            "[[float]]"
        ));
    }

    #[test]
    fn compat_kind_oir_round_trips_the_legacy_vocabulary() {
        assert_eq!(compat_kind_oir("string"), String);
        assert_eq!(compat_kind_oir("number!"), Number);
        assert_eq!(
            compat_kind_oir("[string]"),
            Array { inner: Box::new(String) }
        );
        // Markers with no real `OirType` fall back to their witness type.
        assert_eq!(compat_kind_oir("money"), Money);
        assert_eq!(compat_kind_oir("any"), Json);
    }

    #[test]
    fn object_json_map_family() {
        assert!(set_value_type_compatible("object", "json"));
        assert!(set_value_type_compatible("json", "object"));
        assert!(set_value_type_compatible("Map<string,string>", "object"));
        assert!(set_value_type_compatible("object", "Map<string,string>"));
        assert!(set_value_type_compatible("Map<a,b>", "Map<c,d>"));
        // Raw `Map<...>` tags also accept a string actual (the legacy
        // predicate's PascalCase arm) — a quirk the relation preserves.
        assert!(set_value_type_compatible("Map<string,string>", "string"));
        assert!(!set_value_type_compatible("Map<string,string>", "number"));
    }

    #[test]
    fn a_tagged_union_declaration_takes_an_object_and_rejects_a_primitive() {
        assert!(set_value_type_compatible("union", "object"));
        assert!(!set_value_type_compatible("union", "string"));
        assert!(!set_value_type_compatible("union", "number"));
        assert!(!set_value_type_compatible("union", "array"));
    }

    #[test]
    fn scalar_is_not_a_type() {
        // `scalar` has no meaning in the kind vocabulary: identity only, it
        // admits no member kind and fits no declared kind.
        assert!(set_value_type_compatible("scalar", "scalar"));
        assert!(!set_value_type_compatible("scalar", "string"));
        assert!(!set_value_type_compatible("scalar", "number"));
        assert!(!set_value_type_compatible("scalar", "boolean"));
        assert!(!set_value_type_compatible("scalar", "null"));
        assert!(!set_value_type_compatible("object", "scalar"));
        assert!(!set_value_type_compatible("json", "scalar"));
        assert!(!set_value_type_compatible("string", "scalar"));
        // Raw int/float are identity-only declared kinds.
        assert!(set_value_type_compatible("int", "int"));
        assert!(!set_value_type_compatible("number", "int"));
        assert!(!set_value_type_compatible("int", "number"));
    }

    #[test]
    fn semantic_refinements_and_money_file() {
        assert!(set_value_type_compatible("email", "string"));
        assert!(set_value_type_compatible("url", "string"));
        assert!(set_value_type_compatible("phone", "string"));
        assert!(set_value_type_compatible("uuid", "string"));
        assert!(set_value_type_compatible("image", "string"));
        assert!(!set_value_type_compatible("email", "number"));
        // Semantic domains do not inter-assign or escape to string.
        assert!(!set_value_type_compatible("email", "url"));
        assert!(!set_value_type_compatible("string", "email"));
        assert!(set_value_type_compatible("money", "number"));
        assert!(!set_value_type_compatible("number", "money"));
        assert!(set_value_type_compatible("file", "object"));
        assert!(!set_value_type_compatible("file", "string"));
    }

    #[test]
    fn node_accepts_scalars() {
        // The arm moved in from config_type.rs: every scalar renders.
        assert!(set_value_type_compatible("node", "string"));
        assert!(set_value_type_compatible("node", "number"));
        assert!(set_value_type_compatible("node", "boolean"));
        assert!(set_value_type_compatible("node", "object"));
        assert!(!set_value_type_compatible("node", "json"));
        assert!(set_value_type_compatible("node", "number!"));
        assert!(!set_value_type_compatible("string", "node"));
    }

    #[test]
    fn clear_contradictions_rejected() {
        assert!(!set_value_type_compatible("string", "number"));
        assert!(!set_value_type_compatible("boolean", "string"));
        assert!(!set_value_type_compatible("json", "string"));
        assert!(!set_value_type_compatible("json", "array"));
        assert!(!set_value_type_compatible("entity", "json"));
        assert!(!set_value_type_compatible("date", "email"));
    }

    // The relation itself: each acceptance above pinned to its exact witness,
    // so the remaining leniency is enumerated here, not implicit.
    #[test]
    fn sub_identity_and_numeric_widening() {
        use super::sub;
        assert_eq!(sub(&String, &String), Some(C::Identity));
        // Integer -> Number widens; Number -> Integer does not narrow (4.4).
        assert_eq!(sub(&Integer, &Number), Some(C::Widen(WidenKind::IntegerNumber)));
        assert_eq!(sub(&Number, &Integer), None);
        assert_eq!(sub(&Money, &Integer), None);
        // Money <-> Number stay mutually compatible; Integer -> Money widens.
        assert_eq!(sub(&Money, &Number), Some(C::Widen(WidenKind::MoneyNumber)));
        assert_eq!(sub(&Number, &Money), Some(C::Widen(WidenKind::MoneyNumber)));
        assert_eq!(sub(&Integer, &Money), Some(C::Widen(WidenKind::MoneyNumber)));
        assert_eq!(sub(&Boolean, &Boolean), Some(C::Identity));
        assert_eq!(sub(&Boolean, &Number), None);
        assert_eq!(sub(&Number, &String), None);
        assert_eq!(sub(&String, &Number), None);
    }

    #[test]
    fn sub_optional_wrap_unwrap_witnesses() {
        use super::sub;
        assert_eq!(
            sub(&String, &Optional { inner: Box::new(String) }),
            Some(C::Wrap(Box::new(C::Identity)))
        );
        assert_eq!(
            sub(&Optional { inner: Box::new(String) }, &String),
            Some(C::Unwrap { checked: false, inner: Box::new(C::Identity) })
        );
        assert_eq!(
            sub(&Optional { inner: Box::new(Integer) }, &Number),
            Some(C::Unwrap {
                checked: false,
                inner: Box::new(C::Widen(WidenKind::IntegerNumber))
            })
        );
        assert_eq!(
            sub(
                &Optional { inner: Box::new(String) },
                &Optional { inner: Box::new(String) }
            ),
            Some(C::Wrap(Box::new(C::Unwrap {
                checked: false,
                inner: Box::new(C::Identity)
            })))
        );
        assert_eq!(
            sub(&Optional { inner: Box::new(Number) }, &String),
            None
        );
    }

    #[test]
    fn sub_string_kind_family() {
        use super::sub;
        let status = Enum { name: "Status".into() };
        // Enum/Trait erase to "string": kind-level equality with String.
        assert_eq!(sub(&String, &status), Some(C::Identity));
        assert_eq!(sub(&status, &String), Some(C::Identity));
        assert_eq!(sub(&Trait, &status), Some(C::Identity));
        // Semantic refinements accept any string-kind source.
        assert_eq!(
            sub(&String, &Email),
            Some(C::Refine(SemanticStringKind::Email))
        );
        assert_eq!(
            sub(&status, &Url),
            Some(C::Refine(SemanticStringKind::Url))
        );
        // …but semantic domains do not escape to string or inter-assign.
        assert_eq!(sub(&Email, &String), None);
        assert_eq!(sub(&Email, &Url), None);
        assert_eq!(sub(&Email, &Email), Some(C::Identity));
    }

    #[test]
    fn sub_date_family_and_scalars() {
        use super::sub;
        assert_eq!(sub(&String, &Date), Some(C::Widen(WidenKind::StringToDate)));
        assert_eq!(sub(&Integer, &DateTime), Some(C::Widen(WidenKind::NumericToDateTime)));
        assert_eq!(sub(&Money, &Date), Some(C::Widen(WidenKind::NumericToDate)));
        assert_eq!(sub(&Date, &DateTime), None);
        assert_eq!(sub(&DateTime, &Date), None);
        assert_eq!(sub(&Boolean, &Date), None);
    }

    #[test]
    fn a_primitive_union_takes_its_member_kinds_and_nothing_else() {
        use super::sub;
        let union = Union {
            variants: vec![
                crate::ir::OirField::new("string".to_string(), String),
                crate::ir::OirField::new("number".to_string(), Number),
            ],
        };
        assert_eq!(sub(&String, &union), Some(C::Widen(WidenKind::ToUnionMember)));
        assert_eq!(sub(&Number, &union), Some(C::Widen(WidenKind::ToUnionMember)));
        assert_eq!(sub(&Integer, &union), Some(C::Widen(WidenKind::ToUnionMember)));
        assert_eq!(sub(&Boolean, &union), None, "boolean is not a member");
        // A tagged union of structs takes no primitive.
        let tagged = Union {
            variants: vec![crate::ir::OirField::new("a".to_string(), Struct { fields: vec![] })],
        };
        assert_eq!(sub(&String, &tagged), None);
    }

    #[test]
    fn sub_object_json_family_mutually_compatible() {
        use super::sub;
        let row = Struct { fields: vec![] };
        let bag = Map { key: Box::new(String), value: Box::new(Json) };
        assert_eq!(sub(&row, &bag), Some(C::Erase { from: row.clone() }));
        assert_eq!(sub(&bag, &row), Some(C::Erase { from: bag.clone() }));
        assert_eq!(sub(&Json, &row), Some(C::Erase { from: Json }));
        assert_eq!(sub(&row, &Json), Some(C::Erase { from: row.clone() }));
        assert_eq!(sub(&Json, &SExpr), Some(C::Erase { from: Json }));
        assert_eq!(sub(&File, &row), Some(C::Erase { from: File }));
        // json does not leak into string/array-declared slots.
        assert_eq!(sub(&Json, &String), None);
        assert_eq!(sub(&Json, &Array { inner: Box::new(Json) }), None);
    }

    #[test]
    fn an_atom_and_a_list_of_program_data_are_an_sexpr() {
        use super::sub;
        let list = |inner: crate::ir::OirType| Array { inner: Box::new(inner) };
        for atom in [String, Number, Boolean] {
            assert!(sub(&atom, &SExpr).is_some(), "{atom:?} is an atom");
        }
        assert!(sub(&list(SExpr), &SExpr).is_some(), "a list of expressions is an expression");
        assert!(sub(&list(String), &SExpr).is_some());
        assert!(sub(&list(list(Number)), &SExpr).is_some());
        assert!(sub(&Tuple { items: vec![String, Number, list(SExpr)] }, &SExpr).is_some());
        assert!(sub(&Tuple { items: vec![] }, &SExpr).is_some(), "the empty list");
        assert!(sub(&Struct { fields: vec![] }, &SExpr).is_some(), "an options object");
        assert!(sub(&Optional { inner: Box::new(String) }, &SExpr).is_some());
        assert_eq!(sub(&SExpr, &String), None, "control: an expression is not a string");
        assert_eq!(sub(&SExpr, &list(SExpr)), None, "control: nor a list, until it is read as one");
        assert_eq!(sub(&list(Date), &SExpr), None, "control: a list holding a date is not program data");
        assert_eq!(sub(&String, &Json), None, "control: only the expression type takes atoms");
    }

    #[test]
    fn sub_arrays_structural() {
        use super::sub;
        let a = Array { inner: Box::new(String) };
        // Identical array: the top-level Identity shortcut answers before
        // the structural Array arm is consulted.
        assert_eq!(sub(&a, &a.clone()), Some(C::Identity));
        // Coercible element: the element witness is lifted (string-kind →
        // enum is the kind-level identity the Enum arm admits).
        let b = Array { inner: Box::new(Enum { name: "E".into() }) };
        assert_eq!(sub(&a, &b), Some(C::Element(Box::new(C::Identity))));
        // Element-incompatible arrays reject outright (Phase 4.2).
        let c = Array { inner: Box::new(Number) };
        assert_eq!(sub(&a, &c), None);
        assert_eq!(sub(&a, &String), None);
        assert_eq!(sub(&Json, &a), None);
    }

    #[test]
    fn sub_reference_row_or_id() {
        use super::sub;
        let article = Reference { entity: "Article".into() };
        let row = Struct { fields: vec![] };
        assert_eq!(sub(&row, &article), Some(C::Row { from_id: false }));
        assert_eq!(sub(&String, &article), Some(C::Row { from_id: true }));
        assert_eq!(
            sub(&Enum { name: "S".into() }, &article),
            Some(C::Row { from_id: true })
        );
        // Distinct references do not inter-assign; entities do not escape.
        assert_eq!(
            sub(&Reference { entity: "Comment".into() }, &article),
            None
        );
        assert_eq!(sub(&article, &String), None);
        assert_eq!(sub(&article, &row), None);
        assert_eq!(sub(&Json, &article), None);
    }

    #[test]
    fn sub_row_is_object_family() {
        use super::sub;
        let row = Row { entity: "Revision".into() };
        let revision_ref = Reference { entity: "Revision".into() };
        let object = Struct { fields: vec![] };
        // A row takes an object-family value and erases into shapeless destinations.
        assert_eq!(sub(&object, &row), Some(C::Erase { from: object.clone() }));
        assert_eq!(sub(&Json, &row), Some(C::Erase { from: Json }));
        assert_eq!(sub(&row, &object), Some(C::Erase { from: row.clone() }));
        assert_eq!(sub(&row, &row), Some(C::Identity));
        // A reference takes a loaded row.
        assert_eq!(sub(&row, &revision_ref), Some(C::Row { from_id: false }));
        // Controls: a Row is not an id, and an id is not a Row.
        assert_eq!(sub(&String, &row), None);
        assert_eq!(sub(&row, &String), None);
        assert_eq!(sub(&revision_ref, &row), None);
        assert_eq!(crate::oir_type_kind(&row), "object");
    }

    #[test]
    fn sub_node_renders_any_scalar() {
        use super::sub;
        assert_eq!(sub(&String, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
        assert_eq!(sub(&Number, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
        assert_eq!(sub(&Boolean, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
        assert_eq!(sub(&Money, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
        let row = Struct { fields: vec![] };
        assert_eq!(sub(&row, &Node), Some(C::Erase { from: row }));
        let arr = Array { inner: Box::new(String) };
        assert_eq!(sub(&arr, &Node), Some(C::Erase { from: arr }));
        // json is not renderable content.
        assert_eq!(sub(&Json, &Node), None);
        assert_eq!(sub(&Node, &String), None);
    }
}

/// What a render-ui prop binding carries, as far as a single-record prop is
/// concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundShape {
    /// A list of rows (`[T]`, `array`).
    Collection,
    /// One row: of the named type (`Order`), or an unnamed `object`.
    Record(Option<String>),
    /// Anything a record check has no opinion on (scalars, …).
    Other,
}

/// Why a binding cannot fill a single-record prop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBindingMismatch {
    CollectionIntoRecord,
    WrongEntity { expected: String, actual: String },
}

/// Classify a declared payload/field type string. `None` = undecidable here:
/// `@entity` / `$E` sentinels are fixed only once a consumer binds the trait.
pub fn bound_shape_of_type_str(declared: &str) -> Option<BoundShape> {
    let t = declared.trim().trim_end_matches('!');
    if t.starts_with('@') || t.starts_with('$') || t.starts_with("[@") || t.starts_with("[$") {
        return None;
    }
    if t == "array" || (t.starts_with('[') && t.ends_with(']')) {
        return Some(BoundShape::Collection);
    }
    if t == "object" {
        return Some(BoundShape::Record(None));
    }
    if t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return Some(BoundShape::Record(Some(t.to_string())));
    }
    Some(BoundShape::Other)
}

/// Classify a LOWERED payload field: a record/collection of an entity keeps the
/// entity name in its own `entity` slot (`"object"` + `Staff`), not in the type.
pub fn bound_shape_of_payload_field(field_type: &str, entity: Option<&str>) -> Option<BoundShape> {
    if entity.is_some_and(|e| e.starts_with('@') || e.starts_with('$')) {
        return None;
    }
    match (bound_shape_of_type_str(field_type)?, entity) {
        (BoundShape::Record(None), Some(e)) => Some(BoundShape::Record(Some(e.to_string()))),
        (shape, _) => Some(shape),
    }
}

/// A single-record prop (declared object-only) bound to `bound`. `expected`
/// is the entity the prop renders (the trait's linked entity) when known;
/// `is_entity` limits the identity compare to declared entities, so a struct
/// alias is never mistaken for a row of the wrong entity.
pub fn record_prop_binding_mismatch(
    bound: &BoundShape,
    expected: Option<&str>,
    is_entity: impl Fn(&str) -> bool,
) -> Option<RecordBindingMismatch> {
    match bound {
        BoundShape::Collection => Some(RecordBindingMismatch::CollectionIntoRecord),
        BoundShape::Record(Some(actual)) => match expected {
            Some(exp) if actual != exp && is_entity(actual) && is_entity(exp) => {
                Some(RecordBindingMismatch::WrongEntity {
                    expected: exp.to_string(),
                    actual: actual.clone(),
                })
            }
            _ => None,
        },
        BoundShape::Record(None) | BoundShape::Other => None,
    }
}

#[cfg(test)]
mod record_binding_tests {
    //! A render-ui prop that takes ONE record (`form-section.initialData`,
    //! `detail-panel.entity`) must not receive a list, nor a row of another
    //! entity. Shared by lolo L1 and compiler L2 (2026-09-30: 1,045 sites
    //! bound a collection into a record prop and validated 0/0).
    use super::*;

    const ENTITIES: [&str; 2] = ["Booking", "Staff"];
    fn is_entity(n: &str) -> bool {
        ENTITIES.contains(&n)
    }
    fn check(ty: &str, expected: Option<&str>) -> Option<RecordBindingMismatch> {
        bound_shape_of_type_str(ty)
            .and_then(|b| record_prop_binding_mismatch(&b, expected, is_entity))
    }

    #[test]
    fn a_collection_is_never_a_record() {
        for ty in ["[Booking]", "[object]", "array", "[Booking]!"] {
            assert_eq!(
                check(ty, Some("Booking")),
                Some(RecordBindingMismatch::CollectionIntoRecord),
                "{ty}"
            );
        }
    }

    #[test]
    fn a_row_of_another_entity_is_the_wrong_record() {
        assert_eq!(
            check("Staff", Some("Booking")),
            Some(RecordBindingMismatch::WrongEntity {
                expected: "Booking".into(),
                actual: "Staff".into()
            })
        );
    }

    #[test]
    fn control_a_row_of_the_same_entity_fits() {
        assert_eq!(check("Booking", Some("Booking")), None);
        assert_eq!(check("Booking!", Some("Booking")), None);
        assert_eq!(check("object", Some("Booking")), None);
    }

    #[test]
    fn edge_a_struct_alias_is_not_compared_as_an_entity() {
        assert_eq!(check("UploadedFile", Some("Booking")), None);
    }

    #[test]
    fn edge_sentinels_are_undecidable_before_inline() {
        for ty in ["@entity", "[@entity]", "$E", "[$E]"] {
            assert_eq!(bound_shape_of_type_str(ty), None, "{ty}");
        }
    }

    #[test]
    fn a_lowered_payload_field_carries_its_entity_separately() {
        // `data : Staff` lowers to type "object" + entity "Staff";
        // `data : [Staff]` to "[object]" + entity "Staff".
        assert_eq!(
            bound_shape_of_payload_field("object", Some("Staff")),
            Some(BoundShape::Record(Some("Staff".into())))
        );
        assert_eq!(bound_shape_of_payload_field("[object]", Some("Staff")), Some(BoundShape::Collection));
        assert_eq!(bound_shape_of_payload_field("object", None), Some(BoundShape::Record(None)));
        assert_eq!(bound_shape_of_payload_field("object", Some("@entity")), None);
        assert_eq!(bound_shape_of_payload_field("string", None), Some(BoundShape::Other));
    }

    #[test]
    fn edge_scalars_and_an_unknown_expected_entity_pass() {
        assert_eq!(check("string", Some("Booking")), None);
        assert_eq!(check("Staff", None), None);
    }
}

#[cfg(test)]
mod tuple_type_tests {
    use super::*;

    /// `.lolo` `[T1, T2, …]` (two or more slots) is a tuple; `[T]` stays an array.
    #[test]
    fn a_bracketed_list_of_two_or_more_types_is_a_tuple() {
        assert_eq!(tuple_type_slots("[number, number, number]"), Some(vec!["number", "number", "number"]));
        assert_eq!(tuple_type_slots("[[number], string]"), Some(vec!["[number]", "string"]));
        assert_eq!(tuple_type_slots("[Map<string, number>, number]"), Some(vec!["Map<string, number>", "number"]));
        assert_eq!(tuple_type_slots("[number, number]!"), Some(vec!["number", "number"]), "a required marker is not part of the type");
    }

    fn tuple(items: Vec<OirType>) -> OirType {
        OirType::Tuple { items }
    }

    #[test]
    fn a_tuple_relates_slot_by_slot_and_to_arrays() {
        let n3 = tuple(vec![OirType::Number, OirType::Number, OirType::Number]);
        assert!(sub(&n3, &n3).is_some());
        assert!(sub(&OirType::array(OirType::Number), &n3).is_some(), "an array literal fits when its element fits every slot");
        assert!(sub(&n3, &OirType::array(OirType::Number)).is_some(), "a tuple is an array of its slots");
        assert!(sub(&tuple(vec![OirType::String, OirType::Number]), &tuple(vec![OirType::Number, OirType::Number])).is_none());
        assert!(sub(&OirType::array(OirType::String), &n3).is_none());
        assert!(sub(&tuple(vec![OirType::Number, OirType::Number]), &n3).is_none(), "arity must match");
        assert_eq!(oir_type_kind(&n3), "array");
    }

    /// The kind-string relation knows a declared tuple: an array literal kind fits it, a
    /// tuple of the same slots fits, a string does not.
    #[test]
    fn a_declared_tuple_in_the_kind_relation() {
        assert!(set_value_type_compatible("[number, number, number]", "array"));
        assert!(set_value_type_compatible("[number, number, number]", "[number, number, number]"));
        assert!(!set_value_type_compatible("[number, number, number]", "string"));
        assert!(!set_value_type_compatible("[number, number]", "[string, string]"));
    }

    #[test]
    fn control_an_array_and_non_bracketed_types_are_not_tuples() {
        assert_eq!(tuple_type_slots("[number]"), None);
        assert_eq!(tuple_type_slots("Map<string, number>"), None);
        assert_eq!(tuple_type_slots("number"), None);
        assert_eq!(tuple_type_slots("[]"), None);
    }
}
