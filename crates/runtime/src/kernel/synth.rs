//! Row synthesis from declared fields, for the verifier's planner.
//!
//! The planner (`verify/flows.rs`) needs a row or payload that satisfies an entity's declared
//! fields when it drives a flow; this is the one place that derives one. Row ordinal and field
//! declaration alone decide the values. Nothing here writes a store: seeding is the composed
//! `std-mock-seed` program's, through the kernel's `persist`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::policy::mock_candidates;
use orbital_core_base::schema::types::{
    Cardinality, EntityDefinition, FieldDefinition, FieldType, Persistence,
};
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;

use super::mock_random::iso_from_ms;

/// Recursion cap for self-referential `items` / `properties` walks.
const MAX_NESTED_DEPTH: usize = 3;

/// Rows to synthesize. A `[runtime]` entity is a per-orbital singleton, so it gets
/// exactly one row whose values equal its declared defaults — otherwise the
/// persistence layer disagrees with the declared-default layer in the `@entity`
/// merge and boots a machine into the wrong state.
pub fn sample_row_count(def: &EntityDefinition, requested: usize) -> usize {
    if matches!(def.persistence, Some(Persistence::Runtime)) {
        1
    } else {
        requested
    }
}

/// Synthesize sample rows for an entity from its declared fields. Each row gets a
/// stable `id` (`"<name>-<n>"`) plus one value per field.
pub fn sample_rows(def: &EntityDefinition, count: usize) -> Vec<Value> {
    let rows = sample_row_count(def, count);
    (1..=rows)
        .map(|i| {
            let mut row = sample_row(&def.name, &def.fields, i, def.persistence.as_ref());
            row.insert(
                "id".to_string(),
                Value::String(format!("{}-{}", def.name, i)),
            );
            Value::Object(row)
        })
        .collect()
}

/// One row's field values (reserved keys left to the caller).
pub fn sample_row(
    entity_name: &str,
    fields: &[FieldDefinition],
    index: usize,
    persistence: Option<&Persistence>,
) -> ValueMap {
    let mut row: ValueMap = ValueMap::default();
    for f in fields {
        let Some(name) = f.name.as_ref() else {
            continue;
        };
        if name == "id" || name == "createdAt" || name == "updatedAt" {
            continue;
        }
        if let Some(v) = sample_value(f, entity_name, index, persistence, 0) {
            row.insert(name.clone(), v);
        }
    }
    row
}

fn slug(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if cleaned.is_empty() {
        "value".to_string()
    } else {
        cleaned.to_lowercase()
    }
}

/// Image-bearing field names that seed a photo URL. Mirrors `IMAGE_FIELD_NAMES` in the
/// JS owner, which keeps it as a stopgap until entity fields can declare an image type.
const IMAGE_FIELD_NAMES: [&str; 18] = [
    "image",
    "imageurl",
    "image_url",
    "photo",
    "photourl",
    "photo_url",
    "avatar",
    "avatarurl",
    "avatar_url",
    "thumbnail",
    "thumbnailurl",
    "thumbnail_url",
    "picture",
    "pictureurl",
    "cover",
    "coverurl",
    "banner",
    "bannerurl",
];

fn is_image_field_name(name: &str) -> bool {
    IMAGE_FIELD_NAMES.contains(&name.to_lowercase().as_str())
}

/// Deterministic stock-photo URL salted by the row ordinal.
fn sample_image_url(entity_name: &str, field_name: &str, index: usize) -> String {
    format!(
        "https://picsum.photos/seed/{}/400/400",
        urlencode(&format!("{entity_name}-{field_name}-{index}"))
    )
}

/// `encodeURIComponent` for the subset of bytes a seed string can contain.
fn urlencode(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
            {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// The field's declared vocabulary, if it has a non-empty one. A union's `values` are
/// variant NAMES, never literal values, so it has none (`declaredValues` in the JS owner).
fn declared_values(field: &FieldDefinition) -> Option<&Vec<String>> {
    if matches!(field.field_type, FieldType::Union) {
        return None;
    }
    field.values.as_ref().filter(|v| !v.is_empty())
}

/// Is `field.default` a value the synthesizer must return verbatim?
///
/// `is_runtime` — a `[runtime]` singleton's seeded row must equal its
/// declared-defaults projection outright (persistence must agree with the
/// default layer in the `@entity` merge, or the machine boots into the
/// wrong state), so ANY declared default is honored there, unconditionally.
/// For a persistent, multi-row entity a number/boolean default is a
/// NEW-RECORD default, not a claim about every existing row — same
/// reasoning already applied below to a string default on a closed
/// vocabulary (the row cycle wins over it). Honoring it verbatim made
/// every mock-seeded counter/amount field read as literally, uniformly
/// zero across the whole collection. Mirrors `@almadar/core`'s TS twin
/// exactly (confirmed 2026-09-17).
pub fn is_declared_default_honored(field: &FieldDefinition, is_runtime: bool) -> bool {
    let Some(default) = field.default.as_ref() else {
        return false;
    };
    if is_runtime {
        return true;
    }
    match Value::from(default.clone()) {
        Value::Number(_) | Value::Bool(_) => false,
        Value::Array(_) | Value::Object(_) => true,
        Value::Null => false,
        Value::String(s) => {
            let values = declared_values(field);
            if s.is_empty() {
                // A placeholder unless the field's own vocabulary declares "".
                values.is_some_and(|v| v.iter().any(|m| m.is_empty()))
            } else {
                // A default on a closed vocabulary of a collection is a
                // new-record default, not a claim about existing rows.
                values.is_none()
            }
        }
        _ => false,
    }
}

/// Display text: a photo URL for an image-named field, else `"<Field Name> <n>"`.
fn sample_text(name: &str, entity_name: &str, index: usize) -> Value {
    if is_image_field_name(name) {
        return Value::String(sample_image_url(entity_name, name, index));
    }
    Value::String(format!("{} {}", title_case(name), index))
}

/// The fixed "now" rows resolve relative `@mock` dates against (2026-06-15T00:00:00.000Z).
pub const RELATIVE_MOCK_INDEX_ANCHOR_MS: f64 = 1_781_481_600_000.0;

/// A date field's `@mock` candidate: a signed offset (`-2h`, `-1d`, `+3w`; units m h d w)
/// is that far from the fixed anchor; anything else is used as written.
fn mock_date_value(date_only: bool, literal: &str) -> Value {
    let trimmed = literal.trim();
    let (sign, rest) = match trimmed.chars().next() {
        Some('-') => (-1.0, &trimmed[1..]),
        Some('+') => (1.0, &trimmed[1..]),
        _ => return Value::String(literal.to_string()),
    };
    let unit_ms = match rest.chars().last() {
        Some('m') => 60_000.0,
        Some('h') => 3_600_000.0,
        Some('d') => 86_400_000.0,
        Some('w') => 604_800_000.0,
        _ => return Value::String(literal.to_string()),
    };
    let digits = &rest[..rest.len() - 1];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Value::String(literal.to_string());
    }
    let Ok(amount) = digits.parse::<f64>() else {
        return Value::String(literal.to_string());
    };
    let iso = iso_from_ms(RELATIVE_MOCK_INDEX_ANCHOR_MS + sign * amount * unit_ms);
    if date_only {
        Value::String(iso.split('T').next().unwrap_or_default().to_string())
    } else {
        Value::String(iso)
    }
}

fn sample_date(index: usize, date_only: bool) -> Value {
    let month = (index % 12) + 1;
    if date_only {
        Value::String(format!("2026-{month:02}-15"))
    } else {
        Value::String(format!("2026-{month:02}-15T00:00:00.000Z"))
    }
}

/// 2026-01-15T00:00:00Z in epoch milliseconds, the first indexed instant sample.
const INDEXED_INSTANT_BASE_MS: f64 = 1_768_435_200_000.0;

fn sample_instant(index: usize) -> Value {
    Value::Number(INDEXED_INSTANT_BASE_MS + ((index % 12) as f64) * 30.0 * 86_400_000.0)
}

/// One sample value for a field. `None` means OMIT the key. Twin of `sampleFieldValue`.
fn sample_value(
    field: &FieldDefinition,
    entity_name: &str,
    index: usize,
    persistence: Option<&Persistence>,
    depth: usize,
) -> Option<Value> {
    let honored_default = field.default.as_ref().map(|d| Value::from(d.clone()));

    // Gate 2: the owning trait computes this, so the data layer must not write it.
    if field.intrinsic == Some(true) {
        return honored_default;
    }

    let is_runtime = matches!(persistence, Some(Persistence::Runtime));
    if is_declared_default_honored(field, is_runtime) {
        return honored_default;
    }

    // A declared `@mock "..."` wins over the type-based dispatch below and is
    // used LITERALLY — never mapped through a domain-name lookup table (that
    // would be this generator deciding what "person-name" means, exactly the
    // guess `.lolo`'s type system doesn't get to make). Comma-separated is an
    // explicit candidate list the AUTHOR supplies real values for
    // (`@mock "Acme Corp, Globex Inc, Wayne Enterprises"`), rotated by row
    // index. No comma is a single literal value, index-suffixed so rows
    // aren't byte-identical (`@mock "person-name"` -> "person-name-1",
    // "person-name-2", ...). No `@mock` at all falls straight through to the
    // ordinary type-based synthesis below, unchanged.
    if let Some(declared) = field.mock.as_deref() {
        let ordinal = if is_runtime { 1 } else { index };
        // An enum's candidates are its members, used as is: an index suffix would seed a non-member.
        if declared_values(field).is_some() {
            let members = mock_candidates(declared);
            if !members.is_empty() {
                return Some(Value::String(
                    members[(ordinal - 1) % members.len()].to_string(),
                ));
            }
        }
        // A behavior field's candidates are behavior specifiers, used verbatim as behavior values.
        if field.field_type == FieldType::Behavior {
            let candidates = mock_candidates(declared);
            return Some(
                match candidates.get((ordinal - 1) % candidates.len().max(1)) {
                    Some(spec) => Value::Object(ValueMap::from_iter([(
                        "behavior".to_string(),
                        Value::String(spec.to_string()),
                    )])),
                    None => Value::Null,
                },
            );
        }
        if matches!(
            field.field_type,
            FieldType::Date | FieldType::Datetime | FieldType::Timestamp
        ) {
            let candidates = mock_candidates(declared);
            let literal = candidates
                .get((ordinal - 1) % candidates.len().max(1))
                .map(|c| c.to_string())
                .unwrap_or_else(|| declared.to_string());
            return Some(mock_date_value(
                matches!(field.field_type, FieldType::Date),
                &literal,
            ));
        }
        return Some(typed_mock_value(field, mock_field_value(declared, ordinal)));
    }

    // `T.f`: an unlinked placeholder.
    if projection_of(field).is_some() {
        return Some(if field.field_type == FieldType::Array {
            Value::Array(Vec::new())
        } else {
            Value::String(String::new())
        });
    }

    // An enum with no `@mock` rotates its own values (a declared `@mock` wins above).
    if let Some(values) = declared_values(field) {
        let ordinal = if is_runtime { 1 } else { index };
        return Some(Value::String(values[(ordinal - 1) % values.len()].clone()));
    }

    let name = field.name.as_deref().unwrap_or("field");
    match field.field_type {
        FieldType::String => Some(sample_text(name, entity_name, index)),
        // Semantic domains synthesize a value that SATISFIES their own validator
        // (`orbital_core_eval` `semantic_string_value_valid`, the `validate/*` patterns) — the property that keeps the seeder and
        // the validator from drifting.
        FieldType::Email => Some(Value::String(format!(
            "{}{}@example.com",
            slug(name),
            index
        ))),
        FieldType::Url => Some(Value::String(format!(
            "https://example.com/{}/{}",
            slug(name),
            index
        ))),
        FieldType::Phone => Some(Value::String(format!(
            "+1-555-{:04}",
            1000 + (index % 9000)
        ))),
        FieldType::Uuid => Some(Value::String(format!(
            "00000000-0000-4000-8000-{index:012x}"
        ))),
        FieldType::Image => Some(Value::String(sample_image_url(entity_name, name, index))),
        // No declared min/max: `.lolo` cannot declare a field range, so
        // `FieldDefinition` carries none and the JS owner's fallback bounds apply.
        FieldType::Number | FieldType::Int => Some(Value::Number((index as f64) * 10.0)),
        FieldType::Bytes => Some(Value::String(orbital_core_base::codec::base64_encode(
            format!("{}-{}", slug(name), index).as_bytes(),
        ))),
        FieldType::Money => Some(Value::Number((index as f64) * 10.0)),
        // File seeds the canonical struct value (satisfies its own
        // type-compat validity rule, like the semantic domains above).
        FieldType::File => Some(Value::Object(ValueMap::from_iter([
            (
                "name".to_string(),
                Value::String(format!("{}-{}.pdf", slug(name), index)),
            ),
            (
                "url".to_string(),
                Value::String(format!(
                    "https://example.com/files/{}/{}.pdf",
                    slug(name),
                    index
                )),
            ),
            (
                "mimeType".to_string(),
                Value::String("application/pdf".to_string()),
            ),
            (
                "sizeBytes".to_string(),
                Value::Number((index as f64) * 1024.0),
            ),
        ]))),
        FieldType::Boolean => Some(Value::Bool(index.is_multiple_of(2))),
        FieldType::Date => Some(sample_date(index, true)),
        FieldType::Datetime | FieldType::Timestamp => Some(sample_date(index, false)),
        FieldType::Instant => Some(sample_instant(index)),
        FieldType::Enum => Some(Value::Null),
        // Placeholder only; the caller's post-pass links real sibling ids.
        // Matches `sampleRelation` in `@almadar/core/mock` exactly.
        FieldType::Relation => Some(
            match field.relation.as_ref().and_then(|r| r.cardinality.as_ref()) {
                Some(Cardinality::One) => Value::String(String::new()),
                _ => Value::Array(Vec::new()),
            },
        ),
        FieldType::Array => Some(sample_array(field, entity_name, index, persistence, depth)),
        FieldType::Tuple => Some(Value::Array(
            field
                .tuple_slots()
                .into_iter()
                .map(|slot| {
                    sample_value(slot, entity_name, index, persistence, depth + 1)
                        .unwrap_or(Value::Null)
                })
                .collect(),
        )),
        FieldType::Object => Some(sample_object(field, entity_name, index, persistence, depth)),
        // Seed the FIRST declared variant. Picking by declaration order is
        // deterministic and any other choice would be arbitrary; a union with no
        // carried variants is a by-name recursive back-reference with nothing to sample.
        FieldType::Union => {
            if depth >= MAX_NESTED_DEPTH {
                return None;
            }
            let variant = field
                .properties
                .as_ref()
                .zip(field.values.as_ref())
                .and_then(|(props, names)| names.first().and_then(|n| props.get(n)))?;
            let mut child = variant.clone();
            child.name = field.name.clone();
            sample_value(&child, entity_name, index, persistence, depth + 1)
        }
        FieldType::Trait | FieldType::Orbital | FieldType::Behavior => Some(Value::Null),
        FieldType::Slot
        | FieldType::Pattern
        | FieldType::Node
        | FieldType::SExpr
        | FieldType::Event
        | FieldType::EventAddress
        | FieldType::Field
        | FieldType::CallKey
        | FieldType::TypeVar(_) => None,
    }
}

fn sample_array(
    field: &FieldDefinition,
    entity_name: &str,
    index: usize,
    persistence: Option<&Persistence>,
    depth: usize,
) -> Value {
    let Some(items) = field.items.as_ref() else {
        return Value::Array(Vec::new());
    };
    if depth >= MAX_NESTED_DEPTH {
        return Value::Array(Vec::new());
    }
    let count = 3;
    let element_name = field.name.as_deref().unwrap_or("item");
    let out: Vec<Value> = (0..count)
        .map(|i| {
            let mut element = (**items).clone();
            element.name = Some(format!("{element_name}[{i}]"));
            sample_value(
                &element,
                entity_name,
                index * 10 + i,
                persistence,
                depth + 1,
            )
            .unwrap_or(Value::Null)
        })
        .collect();
    Value::Array(out)
}

fn sample_object(
    field: &FieldDefinition,
    entity_name: &str,
    index: usize,
    persistence: Option<&Persistence>,
    depth: usize,
) -> Value {
    let Some(properties) = field.properties.as_ref() else {
        return Value::Null;
    };
    if depth >= MAX_NESTED_DEPTH {
        return Value::Null;
    }
    // Sorted, so the row does not depend on the map's order.
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();
    let mut out: ValueMap = ValueMap::default();
    for prop_name in names {
        let mut child = properties[prop_name].clone();
        child.name = Some(prop_name.clone());
        out.insert(
            prop_name.clone(),
            sample_value(&child, entity_name, index, persistence, depth + 1).unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

/// `targetType`/`target_type` → `Target Type` for readable sample cell text.
fn title_case(name: &str) -> String {
    let spaced: String = name
        .chars()
        .flat_map(|c| {
            if c == '_' || c == '-' {
                vec![' ']
            } else if c.is_uppercase() {
                vec![' ', c]
            } else {
                vec![c]
            }
        })
        .collect();
    spaced
        .split_whitespace()
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(first) => first.to_uppercase().chain(cs).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn mock_field_value(declared: &str, index: usize) -> String {
    let candidates = mock_candidates(declared);
    if candidates.len() > 1 {
        candidates[(index - 1) % candidates.len()].to_string()
    } else {
        format!("{}-{}", declared.trim(), index)
    }
}

/// A declared `@mock` value in its field's own type: numeric fields seed numbers
/// and boolean fields seed booleans, so aggregates over seeded rows are real. A
/// literal that doesn't parse stays the declared string. Mirrors
/// `@almadar/core/mock`'s `typedMockValue`.
fn typed_mock_value(field: &FieldDefinition, literal: String) -> Value {
    match field.field_type {
        FieldType::Number | FieldType::Money | FieldType::Instant => {
            match literal.trim().parse::<f64>() {
                Ok(n) if n.is_finite() => Value::Number(n),
                _ => Value::String(literal),
            }
        }
        FieldType::Int => match literal.trim().parse::<i64>() {
            Ok(n) => Value::Number(n as f64),
            _ => Value::String(literal),
        },
        FieldType::Boolean => match literal.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(literal),
        },
        _ => Value::String(literal),
    }
}

/// Where a field's value (each element's, for an array) is projected from: `T.f`.
fn projection_of(
    field: &FieldDefinition,
) -> Option<(&orbital_core_base::schema::types::FieldProjection, &str)> {
    fn actual(
        projection: &orbital_core_base::schema::types::FieldProjection,
    ) -> Option<(&orbital_core_base::schema::types::FieldProjection, &str)> {
        projection.field.as_deref().map(|field| (projection, field))
    }
    field.projected_from.as_ref().and_then(actual).or_else(|| {
        (field.field_type == FieldType::Array)
            .then(|| {
                field
                    .items
                    .as_ref()
                    .and_then(|i| i.projected_from.as_ref())
                    .and_then(actual)
            })
            .flatten()
    })
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use serde_json::json;

    /// Built through serde so the tests exercise the same wire shape a real
    /// `.orb` carries, and so no `Default` derive has to be added upstream.
    fn field_json(value: serde_json::Value) -> FieldDefinition {
        serde_json::from_value(value).expect("field fixture must deserialize")
    }

    fn field(name: &str, ft: &str) -> FieldDefinition {
        field_json(json!({ "name": name, "type": ft }))
    }

    fn entity(
        name: &str,
        persistence: Option<&str>,
        fields: Vec<FieldDefinition>,
    ) -> EntityDefinition {
        let mut value = json!({ "name": name, "fields": fields });
        if let Some(p) = persistence {
            value["persistence"] = json!(p);
        }
        serde_json::from_value(value).expect("entity fixture must deserialize")
    }

    fn cell(rows: &[Value], i: usize, key: &str) -> Value {
        rows[i].get_path(key).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn honors_a_non_empty_string_default_without_a_vocabulary() {
        let f = field_json(json!({ "name": "title", "type": "string", "default": "Title" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 2);
        assert_eq!(cell(&rows, 0, "title"), Value::String("Title".to_string()));
    }

    #[test]
    fn honors_falsy_number_and_boolean_defaults_on_a_runtime_singleton() {
        let n = field_json(json!({ "name": "tokenCount", "type": "number", "default": 0 }));
        let b = field_json(json!({ "name": "enabled", "type": "boolean", "default": false }));
        let rows = sample_rows(&entity("E", Some("runtime"), vec![n, b]), 1);
        assert_eq!(cell(&rows, 0, "tokenCount"), Value::Number(0.0));
        assert_eq!(cell(&rows, 0, "enabled"), Value::Bool(false));
    }

    // C-MOCK-SEED-NUMBER-DEFAULT-FLATTENED: a `= 0`-style number/boolean
    // default is a runtime singleton's real starting state (honored
    // verbatim, see above) but on a persistent, multi-row entity it's a
    // new-record default only — every row honoring it verbatim made every
    // mock-seeded counter/amount field read as uniformly zero. Mirrors the
    // TS twin's test exactly (confirmed 2026-09-17).
    #[test]
    fn does_not_flatten_a_falsy_default_across_a_persistent_collection() {
        let n = field_json(
            json!({ "name": "tokenCount", "type": "number", "default": 0, "required": true }),
        );
        let rows = sample_rows(&entity("E", None, vec![n]), 6);
        assert_eq!(rows.len(), 6);
        assert!((0..6).any(|i| cell(&rows, i, "tokenCount") != Value::Number(0.0)));
    }

    #[test]
    fn treats_an_empty_string_default_as_absent() {
        let f = field_json(json!({ "name": "email", "type": "string", "default": "" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(
            cell(&rows, 0, "email"),
            Value::String("Email 1".to_string())
        );
    }

    #[test]
    fn gate_3_honors_empty_when_the_vocabulary_declares_it() {
        let f = field_json(
            json!({ "name": "finalStatus", "type": "string", "values": ["approved", "rejected", ""], "default": "" }),
        );
        assert!(is_declared_default_honored(&f, false));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(cell(&rows, 0, "finalStatus"), Value::String(String::new()));
    }

    #[test]
    fn cycles_a_vocabulary_one_based_over_the_declared_default() {
        let f = field_json(
            json!({ "name": "membershipTier", "type": "string", "values": ["basic", "premium", "unlimited"], "default": "basic" }),
        );
        let rows = sample_rows(&entity("Member", None, vec![f]), 4);
        let got: Vec<String> = (0..4)
            .map(|i| {
                cell(&rows, i, "membershipTier")
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(got, vec!["basic", "premium", "unlimited", "basic"]);
    }

    #[test]
    fn gate_1_runtime_entity_seeds_one_row_at_its_declared_default() {
        let f = field_json(
            json!({ "name": "result", "type": "string", "values": ["none", "victory", "defeat"], "default": "none" }),
        );
        let def = entity("BoardgameBoardItem", Some("runtime"), vec![f]);
        assert_eq!(sample_row_count(&def, 6), 1);
        let rows = sample_rows(&def, 6);
        assert_eq!(
            rows.len(),
            1,
            "a runtime singleton must seed exactly one row"
        );
        // C-MOCK-SEED-STRING-DEFAULT-IGNORED: a cycled `victory` would boot the
        // board straight into game-over and freeze its tick guard.
        assert_eq!(cell(&rows, 0, "result"), Value::String("none".to_string()));
    }

    #[test]
    fn the_same_field_on_a_collection_does_cycle() {
        let f = field_json(
            json!({ "name": "result", "type": "string", "values": ["none", "victory", "defeat"], "default": "none" }),
        );
        let rows = sample_rows(&entity("Match", Some("persistent"), vec![f]), 3);
        assert_eq!(
            cell(&rows, 1, "result"),
            Value::String("victory".to_string())
        );
    }

    #[test]
    fn gate_2_intrinsic_without_a_default_omits_the_key() {
        let f = field_json(json!({ "name": "boards", "type": "array", "intrinsic": true }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(rows[0].get_path("boards"), None);
    }

    #[test]
    fn non_scalar_fields_are_emitted_so_a_row_matches_the_js_key_set() {
        let arr = field("tags", "array");
        let obj = field("meta", "object");
        let rel = field("owner", "relation");
        let rows = sample_rows(&entity("E", None, vec![arr, obj, rel]), 1);
        assert_eq!(cell(&rows, 0, "tags"), Value::Array(Vec::new()));
        assert_eq!(cell(&rows, 0, "meta"), Value::Null);
        assert_eq!(cell(&rows, 0, "owner"), Value::Array(Vec::new()));
    }

    #[test]
    fn slot_and_pattern_fields_are_omitted() {
        for ft in ["slot", "pattern"] {
            let rows = sample_rows(&entity("E", None, vec![field("x", ft)]), 1);
            assert_eq!(rows[0].get_path("x"), None);
        }
    }

    #[test]
    fn date_is_date_only_and_datetime_carries_time() {
        let rows = sample_rows(
            &entity(
                "E",
                None,
                vec![field("joinedAt", "date"), field("seenAt", "datetime")],
            ),
            1,
        );
        assert_eq!(
            cell(&rows, 0, "joinedAt"),
            Value::String("2026-02-15".to_string())
        );
        assert!(cell(&rows, 0, "seenAt")
            .as_str()
            .unwrap_or_default()
            .contains('T'));
    }

    #[test]
    fn declared_mock_with_commas_rotates_the_literal_candidates_by_row() {
        let f = field_json(
            json!({ "name": "clientName", "type": "string", "mock": "Acme Corp, Globex Inc, Wayne Enterprises" }),
        );
        let rows = sample_rows(&entity("E", None, vec![f]), 4);
        assert_eq!(
            cell(&rows, 0, "clientName"),
            Value::String("Acme Corp".to_string())
        );
        assert_eq!(
            cell(&rows, 1, "clientName"),
            Value::String("Globex Inc".to_string())
        );
        assert_eq!(
            cell(&rows, 2, "clientName"),
            Value::String("Wayne Enterprises".to_string())
        );
        // Rotates back to the first candidate on the 4th row (3-item list).
        assert_eq!(
            cell(&rows, 3, "clientName"),
            Value::String("Acme Corp".to_string())
        );
    }

    #[test]
    fn declared_mock_without_a_comma_is_a_single_literal_value_suffixed_with_the_row_index() {
        let f =
            field_json(json!({ "name": "clientName", "type": "string", "mock": "person-name" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 2);
        assert_eq!(
            cell(&rows, 0, "clientName"),
            Value::String("person-name-1".to_string())
        );
        assert_eq!(
            cell(&rows, 1, "clientName"),
            Value::String("person-name-2".to_string())
        );
    }

    fn behavior_value(spec: &str) -> Value {
        Value::Object(ValueMap::from_iter([(
            "behavior".to_string(),
            Value::String(spec.to_string()),
        )]))
    }

    #[test]
    fn declared_mock_on_a_behavior_field_seeds_a_behavior_value_per_row_verbatim() {
        let f = field_json(json!({
            "name": "behavior",
            "type": "behavior",
            "required": true,
            "mock": "almadar-behaviors/std-booking-solution, almadar-behaviors/std-accounting-solution"
        }));
        let rows = sample_rows(&entity("E", None, vec![f]), 3);
        assert_eq!(
            cell(&rows, 0, "behavior"),
            behavior_value("almadar-behaviors/std-booking-solution")
        );
        assert_eq!(
            cell(&rows, 1, "behavior"),
            behavior_value("almadar-behaviors/std-accounting-solution")
        );
        assert_eq!(
            cell(&rows, 2, "behavior"),
            behavior_value("almadar-behaviors/std-booking-solution")
        );
    }

    #[test]
    fn control_a_behavior_field_without_mock_seeds_none() {
        let f = field_json(json!({ "name": "behavior", "type": "behavior", "required": true }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(cell(&rows, 0, "behavior"), Value::Null);
    }

    #[test]
    fn declared_mock_on_a_number_field_seeds_numbers() {
        let f =
            field_json(json!({ "name": "uptime", "type": "number", "mock": "99.99, 99.5, 100" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 3);
        assert_eq!(cell(&rows, 0, "uptime"), Value::Number(99.99));
        assert_eq!(cell(&rows, 1, "uptime"), Value::Number(99.5));
        assert_eq!(cell(&rows, 2, "uptime"), Value::Number(100.0));
    }

    #[test]
    fn declared_mock_on_a_money_field_seeds_numbers() {
        let f = field_json(json!({ "name": "total", "type": "money", "mock": "250000, 84000" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 2);
        assert_eq!(cell(&rows, 1, "total"), Value::Number(84000.0));
    }

    #[test]
    fn a_bytes_field_seeds_decodable_base64() {
        let f = field_json(json!({ "name": "payload", "type": "bytes" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 2);
        match cell(&rows, 1, "payload") {
            Value::String(s) => {
                assert!(!s.is_empty());
                assert!(orbital_core_base::codec::base64_decode(&s).is_ok(), "{s}");
            }
            other => panic!("expected a base64 string, got {other:?}"),
        }
    }

    #[test]
    fn declared_mock_on_a_boolean_field_seeds_booleans() {
        let f = field_json(json!({ "name": "inStock", "type": "boolean", "mock": "true, false" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 2);
        assert_eq!(cell(&rows, 0, "inStock"), Value::Bool(true));
        assert_eq!(cell(&rows, 1, "inStock"), Value::Bool(false));
    }

    #[test]
    fn declared_numeric_looking_mock_on_a_string_field_stays_text() {
        let f =
            field_json(json!({ "name": "matterNumber", "type": "string", "mock": "2026, 2027" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(
            cell(&rows, 0, "matterNumber"),
            Value::String("2026".to_string())
        );
    }

    #[test]
    fn declared_non_numeric_mock_on_a_number_field_stays_the_literal() {
        let f = field_json(json!({ "name": "uptime", "type": "number", "mock": "n/a, 99.5" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        assert_eq!(cell(&rows, 0, "uptime"), Value::String("n/a".to_string()));
    }

    #[test]
    fn no_declared_mock_falls_through_to_ordinary_type_based_synthesis_unchanged() {
        let f = field_json(json!({ "name": "clientName", "type": "string" }));
        let rows = sample_rows(&entity("E", None, vec![f]), 1);
        // Unchanged from today's plain-string behavior: "Client Name 1", not
        // anything `@mock`-shaped.
        assert_eq!(
            cell(&rows, 0, "clientName"),
            Value::String("Client Name 1".to_string())
        );
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn named_source_provenance_preserves_expanded_sampling_and_real_projections() {
        let mut field: FieldDefinition = serde_json::from_value(serde_json::json!({
            "name":"row","type":"object","properties":{"label":{"type":"string"}},
            "projectedFrom":{"type":"Shared"}
        }))
        .unwrap();
        assert!(projection_of(&field).is_none());
        let sampled = sample_value(&field, "Note", 0, None, 0).unwrap();
        assert!(sampled.get_path("label").is_some());
        field.projected_from.as_mut().unwrap().field = Some("label".into());
        assert_eq!(projection_of(&field).unwrap().1, "label");
        assert_eq!(
            sample_value(&field, "Note", 0, None, 0),
            Some(Value::String(String::new()))
        );
        let mut array: FieldDefinition = serde_json::from_value(serde_json::json!({
            "type":"array","items":{"type":"string","projectedFrom":{"type":"Shared"}}
        }))
        .unwrap();
        assert!(projection_of(&array).is_none());
        array
            .items
            .as_mut()
            .unwrap()
            .projected_from
            .as_mut()
            .unwrap()
            .field = Some("label".into());
        assert_eq!(projection_of(&array).unwrap().1, "label");
    }
}

#[cfg(test)]
mod enum_mock_precedence_tests {
    use super::*;

    // Twin of the TS "a declared @mock on an enum" tests: the author's candidates win
    // over the enum's own rotation; without @mock the rotation stands.
    fn status(mock: Option<&str>) -> FieldDefinition {
        let mut value = serde_json::json!({
            "name": "status", "type": "string", "required": true,
            "values": ["draft", "sent", "paid"]
        });
        if let Some(m) = mock {
            value["mock"] = serde_json::json!(m);
        }
        serde_json::from_value(value).expect("field fixture must deserialize")
    }

    fn seeded(field: &FieldDefinition) -> Vec<Value> {
        (1..=4)
            .map(|i| sample_value(field, "Invoice", i, None, 0).expect("a required field seeds"))
            .collect()
    }

    fn strings(xs: &[&str]) -> Vec<Value> {
        xs.iter().map(|s| Value::String(s.to_string())).collect()
    }

    #[test]
    fn a_declared_mock_on_an_enum_wins_over_the_rotation() {
        assert_eq!(
            seeded(&status(Some("paid, paid, sent"))),
            strings(&["paid", "paid", "sent", "paid"])
        );
    }

    #[test]
    fn edge_a_single_candidate_on_an_enum_is_used_as_is_on_every_row() {
        let rows: Vec<Value> = (1..=2)
            .map(|i| sample_value(&status(Some("paid")), "Invoice", i, None, 0).expect("seeds"))
            .collect();
        assert_eq!(rows, strings(&["paid", "paid"]));
    }

    #[test]
    fn control_an_enum_without_mock_still_rotates_its_values() {
        assert_eq!(
            seeded(&status(None)),
            strings(&["draft", "sent", "paid", "draft"])
        );
    }
}

#[cfg(test)]
mod relative_mock_date_tests {
    use super::*;

    fn field(name: &str, field_type: FieldType, mock: &str) -> FieldDefinition {
        let mut f: FieldDefinition =
            serde_json::from_value(serde_json::json!({ "name": name, "type": "string" }))
                .expect("field");
        f.field_type = field_type;
        f.mock = Some(mock.to_string());
        f
    }

    fn sample(f: &FieldDefinition, index: usize) -> Value {
        sample_value(f, "Member", index, None, 0).expect("a mock value")
    }

    // Twin of the TS "relative @mock dates (G-CORE-019)" index cases.
    #[test]
    fn index_strategy_offsets_resolve_against_the_fixed_anchor() {
        let posted = field("postedAt", FieldType::Datetime, "-1d, -2h, -3w");
        assert_eq!(
            sample(&posted, 1),
            Value::String("2026-06-14T00:00:00.000Z".into())
        );
        assert_eq!(
            sample(&posted, 2),
            Value::String("2026-06-14T22:00:00.000Z".into())
        );
        assert_eq!(
            sample(&posted, 3),
            Value::String("2026-05-25T00:00:00.000Z".into())
        );
        let due = field("dueOn", FieldType::Date, "+2d");
        assert_eq!(sample(&due, 1), Value::String("2026-06-17".into()));
    }

    #[test]
    fn control_a_fixed_literal_is_used_as_written_and_a_string_field_keeps_it_literally() {
        assert_eq!(
            sample(&field("opened", FieldType::Date, "2026-01-02"), 2),
            Value::String("2026-01-02".into())
        );
        assert_eq!(
            sample(&field("code", FieldType::String, "-1d, x"), 1),
            Value::String("-1d".into())
        );
    }
}

#[cfg(test)]
mod trait_field_seed_tests {
    use super::*;

    fn field(ty: FieldType) -> FieldDefinition {
        let mut f: FieldDefinition =
            serde_json::from_value(serde_json::json!({ "name": "panel", "type": "string" }))
                .expect("field");
        f.field_type = ty;
        f
    }

    #[test]
    fn a_trait_field_seeds_null() {
        let v = sample_value(&field(FieldType::Trait), "Panel", 1, None, 0);
        assert_eq!(v, Some(Value::Null));
    }

    #[test]
    fn control_other_reference_types_still_omit_the_key() {
        for ty in [FieldType::Slot, FieldType::Pattern, FieldType::SExpr] {
            assert_eq!(sample_value(&field(ty), "Panel", 1, None, 0), None);
        }
    }
}
