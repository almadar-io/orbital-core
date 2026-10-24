//! Sample-data seeding for entities.
//!
//! The kernel already materializes authored seed data (`EntityDefinition.instances`)
//! into the entity store at `register_schema`. This module is the companion for the
//! complementary case: entities that declare **no** instances but whose fields are
//! known — used by previews and the headless verifiers/screenshotter so tables, grids
//! and detail views render real columns + data instead of an empty state.
//!
//! This is the single owner of field→row synthesis; consumers (the Layer-2 render
//! probe, the screenshot probe) call [`seed_sample_data`] rather than rolling their own.

use orbital_core_base::value::ValueMap;
use orbital_core_base::schema::types::{Cardinality, EntityDefinition, EntityRef, FieldDefinition, FieldType, Persistence};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use std::collections::HashMap;

use super::entity_source::EntitySource;
use super::mock_random::{iso_from_ms, MockRandom};

/// Rows a mock store seeds per entity when the host names no count (`MOCK_SEED_COUNT` in the playground's
/// `mock-seed.ts`, `MockPersistenceAdapter`'s `defaultSeedCount`).
pub const MOCK_SEED_COUNT: usize = 6;

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

/// Where a sampled value's variety comes from. `Index` is a pure function of the row
/// ordinal (the verifier's synthesized payloads, the index parity vector); `Seeded`
/// draws from the shared LCG, which is how every mock store seeds its rows. Twin of
/// `@almadar/core/mock`'s `SampleStrategy`.
pub enum Entropy<'a> {
    Index,
    Seeded(&'a mut MockRandom),
}

impl Entropy<'_> {
    fn is_seeded(&self) -> bool {
        matches!(self, Entropy::Seeded(_))
    }
}

/// Synthesize sample rows for an entity from its declared fields. Each row gets a
/// stable `id` (`"<name>-<n>"`) plus one value per field. Mirrors
/// `@almadar/core/mock`'s `sampleRows` with `strategy: 'index'`; the shared fixture
/// in `orbital-core fixtures` pins them to the same bytes.
pub fn sample_rows(def: &EntityDefinition, count: usize) -> Vec<Value> {
    let rows = sample_row_count(def, count);
    (1..=rows)
        .map(|i| {
            let mut row = sample_row(
                &def.name,
                &def.fields,
                i,
                def.persistence.as_ref(),
                def.identity,
                &mut Entropy::Index,
            );
            row.insert(
                "id".to_string(),
                Value::String(format!("{}-{}", def.name, i)),
            );
            Value::Object(row)
        })
        .collect()
}

/// One row's field values (reserved keys left to the caller). Twin of `sampleRow`.
///
/// A seeded row leaves an optional, undefaulted field unset on every even row, so
/// "not yet set" affordances have rows to act on, except on an `[identity]` entity,
/// whose rows are the persona roster. Row fields only: an array element or object
/// property is part of a value.
pub fn sample_row(
    entity_name: &str,
    fields: &[FieldDefinition],
    index: usize,
    persistence: Option<&Persistence>,
    identity: bool,
    entropy: &mut Entropy,
) -> ValueMap {
    let mut row: ValueMap = ValueMap::new();
    for f in fields {
        let Some(name) = f.name.as_ref() else {
            continue;
        };
        if name == "id" || name == "createdAt" || name == "updatedAt" {
            continue;
        }
        if entropy.is_seeded()
            && !identity
            && f.required != Some(true)
            && f.default.is_none()
            && !matches!(f.field_type, FieldType::Relation)
            && projection_of(f).is_none()
            && index.is_multiple_of(2)
        {
            continue;
        }
        if let Some(v) = sample_value(f, entity_name, index, persistence, 0, entropy) {
            row.insert(name.clone(), v);
        }
    }
    row
}

/// Lowercase, punctuation-free fragment safe to embed in an email or URL.
/// Mirrors `slug()` in `@almadar/core/mock`.
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

/// Deterministic stock-photo URL. Mirrors `sampleImageUrl()` in the JS owner: the
/// seed's salt is a draw when seeded, the row ordinal otherwise.
fn sample_image_url(
    entity_name: &str,
    field_name: &str,
    index: usize,
    entropy: &mut Entropy,
) -> String {
    let salt = match entropy {
        Entropy::Seeded(rng) => format!("{}", rng.int(0.0, 1000.0)),
        Entropy::Index => index.to_string(),
    };
    format!(
        "https://picsum.photos/seed/{}/400/400",
        urlencode(&format!("{entity_name}-{field_name}-{salt}"))
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

/// Display text: a photo URL for an image-named field, else two title-cased words
/// (seeded) or `"<Field Name> <n>"` (index). Twin of `sampleText`.
fn sample_text(name: &str, entity_name: &str, index: usize, entropy: &mut Entropy) -> Value {
    if is_image_field_name(name) {
        return Value::String(sample_image_url(entity_name, name, index, entropy));
    }
    match entropy {
        Entropy::Seeded(rng) => Value::String(title_case(&rng.words(2))),
        Entropy::Index => Value::String(format!("{} {}", title_case(name), index)),
    }
}

/// The fixed "now" index-strategy rows resolve relative `@mock` dates against
/// (2026-06-15T00:00:00.000Z). Twin: `RELATIVE_MOCK_INDEX_ANCHOR` in @almadar/core.
pub const RELATIVE_MOCK_INDEX_ANCHOR_MS: f64 = 1_781_481_600_000.0;

/// A date field's `@mock` candidate: a signed offset (`-2h`, `-1d`, `+3w`; units m h d w)
/// is that far from the seeding clock (seeded) or the fixed anchor (index); anything else
/// is used as written. Twin: `mockDateValue` in @almadar/core `mock/sampleValue.ts`.
fn mock_date_value(date_only: bool, literal: &str, entropy: &mut Entropy) -> Value {
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
    let anchor = match entropy {
        Entropy::Seeded(rng) => rng.now_ms(),
        Entropy::Index => RELATIVE_MOCK_INDEX_ANCHOR_MS,
    };
    let iso = iso_from_ms(anchor + sign * amount * unit_ms);
    if date_only {
        Value::String(iso.split('T').next().unwrap_or_default().to_string())
    } else {
        Value::String(iso)
    }
}

/// Seeded dates fall within this many days either side of today: wide enough that a
/// monthly chart over mock rows has several months, while upcoming views still get
/// future dates. Twin: `SEEDED_DATE_WINDOW_DAYS` in @almadar/core `mock/sampleValue.ts`.
pub const SEEDED_DATE_WINDOW_DAYS: f64 = 120.0;

fn sample_date(index: usize, date_only: bool, entropy: &mut Entropy) -> Value {
    match entropy {
        Entropy::Seeded(rng) => {
            let iso = iso_from_ms(rng.straddling_date(SEEDED_DATE_WINDOW_DAYS));
            if date_only {
                Value::String(iso.split('T').next().unwrap_or_default().to_string())
            } else {
                Value::String(iso)
            }
        }
        Entropy::Index => {
            let month = (index % 12) + 1;
            if date_only {
                Value::String(format!("2026-{month:02}-15"))
            } else {
                Value::String(format!("2026-{month:02}-15T00:00:00.000Z"))
            }
        }
    }
}

/// One sample value for a field. `None` means OMIT the key. Twin of `sampleFieldValue`.
fn sample_value(
    field: &FieldDefinition,
    entity_name: &str,
    index: usize,
    persistence: Option<&Persistence>,
    depth: usize,
    entropy: &mut Entropy,
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
                    Some(spec) => Value::Object(ValueMap::from([(
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
                entropy,
            ));
        }
        return Some(typed_mock_value(field, mock_field_value(declared, ordinal)));
    }

    // `T.f`: an unlinked placeholder, filled from `T`'s seeded rows by `link_projections`.
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
        FieldType::String => Some(sample_text(name, entity_name, index, entropy)),
        // Semantic domains synthesize a value that SATISFIES their own validator
        // (`orbital_core_eval` `semantic_string_value_valid`, the `validate/*` patterns) — the property that keeps the seeder and
        // the validator from drifting.
        FieldType::Email => Some(Value::String(match entropy {
            Entropy::Seeded(rng) => rng.email(),
            Entropy::Index => format!("{}{}@example.com", slug(name), index),
        })),
        FieldType::Url => Some(Value::String(match entropy {
            Entropy::Seeded(rng) => rng.url(),
            Entropy::Index => format!("https://example.com/{}/{}", slug(name), index),
        })),
        FieldType::Phone => Some(Value::String(match entropy {
            Entropy::Seeded(rng) => rng.phone(),
            Entropy::Index => format!("+1-555-{:04}", 1000 + (index % 9000)),
        })),
        FieldType::Uuid => Some(Value::String(match entropy {
            Entropy::Seeded(rng) => rng.uuid(),
            Entropy::Index => format!("00000000-0000-4000-8000-{index:012x}"),
        })),
        FieldType::Image => Some(Value::String(sample_image_url(
            entity_name,
            name,
            index,
            entropy,
        ))),
        // No declared min/max: `.lolo` cannot declare a field range, so
        // `FieldDefinition` carries none and the JS owner's fallback bounds apply.
        FieldType::Number | FieldType::Int => Some(Value::Number(match entropy {
            Entropy::Seeded(rng) => rng.int(1.0, 250.0),
            Entropy::Index => (index as f64) * 10.0,
        })),
        FieldType::Bytes => Some(Value::String(orbital_core_base::codec::base64_encode(
            format!("{}-{}", slug(name), index).as_bytes(),
        ))),
        FieldType::Money => Some(Value::Number(match entropy {
            Entropy::Seeded(rng) => rng.int(10.0, 900.0),
            Entropy::Index => (index as f64) * 10.0,
        })),
        // File seeds the canonical struct value (satisfies its own
        // type-compat validity rule, like the semantic domains above).
        FieldType::File => Some(Value::Object(ValueMap::from([
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
        FieldType::Boolean => Some(Value::Bool(match entropy {
            Entropy::Seeded(rng) => rng.boolean(),
            Entropy::Index => index.is_multiple_of(2),
        })),
        FieldType::Date => Some(sample_date(index, true, entropy)),
        FieldType::Datetime | FieldType::Timestamp => Some(sample_date(index, false, entropy)),
        FieldType::Enum => Some(Value::Null),
        // Placeholder only; the caller's post-pass links real sibling ids.
        // Matches `sampleRelation` in `@almadar/core/mock` exactly.
        FieldType::Relation => Some(
            match field.relation.as_ref().and_then(|r| r.cardinality.as_ref()) {
                Some(Cardinality::One) => Value::String(String::new()),
                _ => Value::Array(Vec::new()),
            },
        ),
        FieldType::Array => Some(sample_array(
            field,
            entity_name,
            index,
            persistence,
            depth,
            entropy,
        )),
        FieldType::Tuple => Some(Value::Array(
            field
                .tuple_slots()
                .into_iter()
                .map(|slot| {
                    sample_value(slot, entity_name, index, persistence, depth + 1, entropy)
                        .unwrap_or(Value::Null)
                })
                .collect(),
        )),
        FieldType::Object => Some(sample_object(
            field,
            entity_name,
            index,
            persistence,
            depth,
            entropy,
        )),
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
            sample_value(&child, entity_name, index, persistence, depth + 1, entropy)
        }
        FieldType::Trait | FieldType::Orbital | FieldType::Behavior => Some(Value::Null),
        FieldType::Slot
        | FieldType::Pattern
        | FieldType::Node
        | FieldType::SExpr
        | FieldType::Event
        | FieldType::EventAddress
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
    entropy: &mut Entropy,
) -> Value {
    let Some(items) = field.items.as_ref() else {
        return Value::Array(Vec::new());
    };
    if depth >= MAX_NESTED_DEPTH {
        return Value::Array(Vec::new());
    }
    let count = match entropy {
        Entropy::Seeded(rng) => rng.int(3.0, 5.0) as usize,
        Entropy::Index => 3,
    };
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
                entropy,
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
    entropy: &mut Entropy,
) -> Value {
    let Some(properties) = field.properties.as_ref() else {
        return Value::Null;
    };
    if depth >= MAX_NESTED_DEPTH {
        return Value::Null;
    }
    // Sorted, so seeded draws do not depend on the map's order (the JS owner sorts too).
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();
    let mut out: ValueMap = ValueMap::new();
    for prop_name in names {
        let mut child = properties[prop_name].clone();
        child.name = Some(prop_name.clone());
        out.insert(
            prop_name.clone(),
            sample_value(&child, entity_name, index, persistence, depth + 1, entropy)
                .unwrap_or(Value::Null),
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
        FieldType::Number | FieldType::Money => match literal.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => Value::Number(n),
            _ => Value::String(literal),
        },
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

/// The env var declaring which columns hold a user id, as `Entity.field` pairs
/// (`RosterEntry.memberId,Appointment.patientId`). Declared, never inferred: a
/// name-matching guess would silently scope the wrong column.
pub const OWNER_FIELDS_ENV: &str = "ALMADAR_PERSONA_OWNS";

fn owner_fields_from_env() -> Vec<(String, String)> {
    std::env::var(OWNER_FIELDS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|pair| {
            let (entity, field) = pair.trim().split_once('.')?;
            if entity.is_empty() || field.is_empty() {
                return None;
            }
            Some((entity.to_lowercase(), field.to_string()))
        })
        .collect()
}

/// The `ALMADAR_PERSONA` viewer's id, resolved against the schema being seeded —
/// its [identity] roster is exactly the rows this module synthesizes for it.
fn viewer_id_from_env(schema: &OrbitalSchema) -> Option<String> {
    let viewer = super::persona::persona_from_env_for_schema(schema).ok()?;
    viewer
        .get_path("id")
        .and_then(|v| v.as_str().map(str::to_string))
}

/// Seed the mock store for `schema`, owner columns stamped with the `ALMADAR_PERSONA`
/// viewer. See [`seed_sample_data_as`].
pub fn seed_sample_data(source: &dyn EntitySource, schema: &OrbitalSchema, count: usize) {
    let viewer_id = viewer_id_from_env(schema);
    seed_sample_data_as(source, schema, count, viewer_id);
}

/// [`seed_sample_data`] with the owner-stamping viewer supplied explicitly instead
/// of read from `ALMADAR_PERSONA`. A headless harness that binds a viewer
/// programmatically (the L2 render probe's roster default) must stamp the SAME
/// viewer onto owner columns, or its ownership-scoped `@read` matches no row and a
/// correct app verifies as an empty one. `None` reproduces the unstamped behavior.
pub fn seed_sample_data_as(
    source: &dyn EntitySource,
    schema: &OrbitalSchema,
    count: usize,
    viewer_id: Option<String>,
) {
    let viewer = viewer_id.map(|id| {
        identity_roster_rows(schema, count, orbital_core_base::time_util::now_millis_f64())
            .into_iter()
            .find(|row| row.get_path("id").and_then(|v| v.as_str()) == Some(id.as_str()))
            .unwrap_or_else(|| {
                Value::Object(ValueMap::from([("id".to_string(), Value::String(id))]))
            })
    });
    seed_mock_dataset(
        source,
        schema,
        count,
        viewer.as_ref(),
        orbital_core_base::time_util::now_millis_f64(),
    );
}

/// The one mock-store seeder, twin of `@almadar/runtime`'s `MockPersistenceAdapter` as
/// `OrbitalServerRuntime` drives it (and so of the compiled apps' `MockDataService`):
///
/// - order: the `[identity]` entity first, then each orbital's primary entity and its
///   auxiliaries in declaration order. A primary entity with authored `instances` is
///   materialized by the kernel, never seeded; a store that already holds rows is
///   backfilled with the fields its new registrant declares instead of seeded again;
/// - one LCG stream seeded [`DEFAULT_MOCK_SEED`]; per row `<Entity> Id N`, then
///   `createdAt` / `updatedAt` draws, then the fields ([`sample_row`], seeded);
/// - the viewer owns every other row of each owner column (only row 0 of a
///   self-identity `id` column), unless the store's `@create` policy denies it, in
///   which case the first identity row that may create it owns it; a column the
///   `@read` policy scopes by ownership uses `@read` admission instead of `@create`;
/// - relation placeholders are then linked deterministically (see [`link_relations`]).
///
/// orbital-core `tests/baked/mock-seed/dataset.seeded.json` pins the result.
pub fn seed_mock_dataset(
    source: &dyn EntitySource,
    schema: &OrbitalSchema,
    count: usize,
    viewer: Option<&Value>,
    now_ms: f64,
) {
    let mut seeder = MockSeeder::new(source, schema, viewer, now_ms);
    for (def, seeds) in seed_order(schema) {
        seeder.register(def, seeds, count);
    }
    seeder.link_relations();
    seeder.link_projections();
}

/// The `[identity]` entity's seeded rows: the persona roster. They are the first rows of
/// every seeded store, so they do not depend on the rest of the schema.
pub fn identity_roster_rows(schema: &OrbitalSchema, count: usize, now_ms: f64) -> Vec<Value> {
    let Some(def) = identity_entity_def(schema) else {
        return Vec::new();
    };
    let scratch = super::entity_source::InMemoryEntityCache::new();
    let mut seeder = MockSeeder::new(&scratch, schema, None, now_ms);
    seeder.register(def, true, count);
    seeder.rows_of(&store_key_of(def))
}

fn is_blank(value: &Value) -> bool {
    value.as_str().is_none_or(str::is_empty)
}

/// An entity's authored `instances` as stored rows: an id-less instance takes the next
/// `<Entity> Id N`, and absent timestamps are `now` — as a mock-store `create` stamps them.
pub fn instance_rows(
    entity_name: &str,
    instances: &[orbital_core_base::schema::sexpr::SObject],
    now_iso: &str,
) -> Vec<Value> {
    let mut minted = 0;
    instances
        .iter()
        .map(|instance| {
            let mut row: ValueMap = instance
                .iter()
                .map(|(key, sexpr)| (key.clone(), super::kernel::sexpr_to_value(sexpr)))
                .collect();
            if row.get("id").is_none_or(is_blank) {
                minted += 1;
                row.insert(
                    "id".to_string(),
                    Value::String(mock_row_id(entity_name, minted)),
                );
            }
            for key in ["createdAt", "updatedAt"] {
                if row.get(key).is_none_or(is_blank) {
                    row.insert(key.to_string(), Value::String(now_iso.to_string()));
                }
            }
            Value::Object(row)
        })
        .collect()
}

/// `(definition, seeds)` in the shared registration order; `seeds` is false for a primary
/// entity whose authored instances the kernel materializes, and for a browser-stored
/// entity that does not declare `mock`.
fn seed_order(schema: &OrbitalSchema) -> Vec<(&EntityDefinition, bool)> {
    let mut out: Vec<(&EntityDefinition, bool)> = Vec::new();
    if let Some(identity) = identity_entity_def(schema) {
        if !has_instances(identity) {
            out.push((identity, true));
        }
    }
    for orbital in &schema.orbitals {
        if let EntityRef::Inline(def) = &orbital.entity {
            out.push((def, !has_instances(def) && mock_seeds(def)));
        }
        for aux in orbital.auxiliary_entities.iter().flatten() {
            if let EntityRef::Inline(def) = aux {
                out.push((def, mock_seeds(def)));
            }
        }
    }
    out
}

/// A browser-stored entity is never mock-seeded; its rows come from `instances`.
fn mock_seeds(def: &EntityDefinition) -> bool {
    !def.browser
}

fn has_instances(def: &EntityDefinition) -> bool {
    def.instances.as_ref().is_some_and(|v| !v.is_empty())
}

fn store_key_of(def: &EntityDefinition) -> String {
    def.collection
        .clone()
        .unwrap_or_else(|| def.name.clone())
        .to_lowercase()
}

/// `Person` -> `Person Id 3`: the mock id family, shared with the kernel's id-less
/// instances and `MockPersistenceAdapter.nextId`.
pub fn mock_row_id(label: &str, n: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    format!("{head}{} Id {n}", chars.as_str())
}

/// A relation cell a seeder may fill: missing, or the `""` / `[]` placeholder.
/// Where a field's value (each element's, for an array) is projected from: `T.f`.
fn projection_of(field: &FieldDefinition) -> Option<(&orbital_core_base::schema::types::FieldProjection, &str)> {
    fn actual(projection: &orbital_core_base::schema::types::FieldProjection) -> Option<(&orbital_core_base::schema::types::FieldProjection, &str)> {
        projection.field.as_deref().map(|field| (projection, field))
    }
    field.projected_from.as_ref().and_then(actual).or_else(|| {
        (field.field_type == FieldType::Array)
            .then(|| field.items.as_ref().and_then(|i| i.projected_from.as_ref()).and_then(actual))
            .flatten()
    })
}

fn is_relation_placeholder(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    }
}

fn is_scalar_cardinality(cardinality: Option<&Cardinality>) -> bool {
    matches!(
        cardinality,
        Some(Cardinality::One) | Some(Cardinality::ManyToOne)
    )
}

/// A cross-entity relation value for row `index` (0-based): a scalar relation takes
/// `targets[index % n]`; a list takes 2 (even rows) or 3 (odd rows) consecutive distinct
/// ids from offset `index`. Twin of `crossRelationValue` in `@almadar/core/mock`.
pub fn cross_relation_value(
    targets: &[String],
    index: usize,
    cardinality: Option<&Cardinality>,
) -> Option<Value> {
    if targets.is_empty() {
        return None;
    }
    if is_scalar_cardinality(cardinality) {
        return Some(Value::String(targets[index % targets.len()].clone()));
    }
    let mut picks: Vec<Value> = Vec::new();
    for offset in 0..targets.len().min(2 + (index % 2)) {
        let candidate = &targets[(index + offset) % targets.len()];
        if !picks.iter().any(|p| p.as_str() == Some(candidate.as_str())) {
            picks.push(Value::String(candidate.clone()));
        }
    }
    Some(Value::Array(picks))
}

fn row_id(row: &Value) -> Option<String> {
    row.get_path("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

struct MockSeeder<'a> {
    source: &'a dyn EntitySource,
    rng: MockRandom,
    viewer: Option<&'a Value>,
    owner_fields: Vec<(String, String)>,
    create_policies: ValueMap,
    read_policies: ValueMap,
    /// Registered definitions in first-registration order (one per entity name).
    registered: Vec<&'a EntityDefinition>,
    store_of_entity: HashMap<String, String>,
    store_by_id: HashMap<String, String>,
    /// Store key -> the entity its rows are read and written through, and the id label.
    entity_of_store: HashMap<String, String>,
    counters: HashMap<String, usize>,
    /// Store key -> row ids in creation order (the JS store's `Map` order).
    order: HashMap<String, Vec<String>>,
    stamped: std::collections::HashSet<(String, String, String)>,
}

impl<'a> MockSeeder<'a> {
    fn new(
        source: &'a dyn EntitySource,
        schema: &'a OrbitalSchema,
        viewer: Option<&'a Value>,
        now_ms: f64,
    ) -> Self {
        let mut owner_fields = owner_fields_from_schema(schema);
        owner_fields.extend(owner_fields_from_env());
        // First entity on a collection that declares any directive owns its policies
        // (`entityAccessPoliciesByStoreKey`).
        let mut create_policies: ValueMap = ValueMap::new();
        let mut read_policies: ValueMap = ValueMap::new();
        let mut decided: std::collections::HashSet<String> = std::collections::HashSet::new();
        for def in inline_entity_defs(schema) {
            let declared = def.read_policy.is_some()
                || def.create_policy.is_some()
                || def.update_policy.is_some()
                || def.delete_policy.is_some();
            if !declared || !decided.insert(store_key_of(def)) {
                continue;
            }
            if let Some(policy) = &def.create_policy {
                create_policies.insert(store_key_of(def), super::kernel::sexpr_to_value(policy));
            }
            if let Some(policy) = &def.read_policy {
                read_policies.insert(store_key_of(def), super::kernel::sexpr_to_value(policy));
            }
        }
        Self {
            source,
            rng: MockRandom::new(super::mock_random::DEFAULT_MOCK_SEED, now_ms),
            viewer,
            owner_fields,
            create_policies,
            read_policies,
            registered: Vec::new(),
            store_of_entity: HashMap::new(),
            store_by_id: HashMap::new(),
            entity_of_store: HashMap::new(),
            counters: HashMap::new(),
            order: HashMap::new(),
            stamped: std::collections::HashSet::new(),
        }
    }

    fn resolve_store_key(&self, entity: &str) -> String {
        let lower = entity.to_lowercase();
        self.store_of_entity.get(&lower).cloned().unwrap_or(lower)
    }

    fn target_store(&self, relation: &orbital_core_base::schema::types::RelationDefinition) -> String {
        relation
            .entity_id
            .as_deref()
            .and_then(|id| self.store_by_id.get(id).cloned())
            .unwrap_or_else(|| self.resolve_store_key(&relation.entity))
    }

    /// The store's rows: those this seeder created, in order, after any it found there.
    fn rows_of(&self, store: &str) -> Vec<Value> {
        let entity = self
            .entity_of_store
            .get(store)
            .cloned()
            .unwrap_or_else(|| store.to_string());
        let listed = self.source.list(&entity).unwrap_or_default();
        let created = self.order.get(store).cloned().unwrap_or_default();
        let mut out: Vec<Value> = listed
            .iter()
            .filter(|r| row_id(r).is_none_or(|id| !created.contains(&id)))
            .map(|r| (**r).clone())
            .collect();
        for id in &created {
            if let Ok(row) = self.source.read(&entity, id) {
                out.push((*row).clone());
            }
        }
        out
    }

    fn register(&mut self, def: &'a EntityDefinition, seeds: bool, count: usize) {
        if !seeds {
            return;
        }
        let store = store_key_of(def);
        self.store_of_entity
            .insert(def.name.to_lowercase(), store.clone());
        if let Some(id) = &def.id {
            self.store_by_id.insert(id.clone(), store.clone());
        }
        self.entity_of_store
            .entry(store.clone())
            .or_insert_with(|| def.name.clone());
        if !self
            .registered
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&def.name))
        {
            self.registered.push(def);
        }
        if self.rows_of(&store).is_empty() {
            self.seed(def, &store, count);
        } else {
            self.backfill(def, &store);
        }
    }

    fn seed(&mut self, def: &EntityDefinition, store: &str, requested: usize) {
        let label = self
            .entity_of_store
            .get(store)
            .cloned()
            .unwrap_or_else(|| def.name.clone());
        let entity = label.clone();
        let entity_key = def.name.to_lowercase();
        let mut owner_cols: Vec<String> = self
            .owner_fields
            .iter()
            .filter(|(e, _)| *e == entity_key)
            .map(|(_, f)| f.clone())
            .collect();
        owner_cols.sort_by_key(|c| c != "id");
        let viewer_id = self.viewer.and_then(row_id);
        for i in 0..sample_row_count(def, requested) {
            let n = self.counters.entry(store.to_string()).or_insert(0);
            *n += 1;
            let id = mock_row_id(&label, *n);
            let created = self.rng.past_date(1.0);
            let updated = (created + self.rng.int(0.0, 30.0) * super::mock_random::MS_PER_DAY)
                .min(self.rng.now_ms());
            let mut row = sample_row(
                &def.name,
                &def.fields,
                i + 1,
                def.persistence.as_ref(),
                def.identity,
                &mut Entropy::Seeded(&mut self.rng),
            );
            row.insert("id".to_string(), Value::String(id));
            row.insert("createdAt".to_string(), Value::String(iso_from_ms(created)));
            row.insert("updatedAt".to_string(), Value::String(iso_from_ms(updated)));
            if let Some(viewer_id) = viewer_id.as_deref() {
                for col in &owner_cols {
                    if !stamps_row(col, i) {
                        continue;
                    }
                    let mut candidate = row.clone();
                    candidate.insert(col.clone(), Value::String(viewer_id.to_string()));
                    let owner = if self.viewer_may_own(
                        store,
                        col,
                        &row,
                        viewer_id,
                        &Value::Object(candidate),
                    ) {
                        Some(viewer_id.to_string())
                    } else {
                        self.first_eligible_owner(store, col, &row)
                    };
                    let Some(owner) = owner else {
                        continue;
                    };
                    row.insert(col.clone(), Value::String(owner));
                    let current = row
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    self.stamped
                        .insert((store.to_string(), current, col.clone()));
                }
            }
            let final_id = row
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let _ = self.source.create(&entity, &Value::Object(row));
            self.order
                .entry(store.to_string())
                .or_default()
                .push(final_id);
        }
        tracing::debug!(target: orbital_core_base::logging::ns::SEED, entity = %def.name, store, "seed:sample");
    }

    /// A later registrant on a seeded store fills the fields it declares that the rows lack.
    fn backfill(&mut self, def: &EntityDefinition, store: &str) {
        let entity = self
            .entity_of_store
            .get(store)
            .cloned()
            .unwrap_or_else(|| def.name.clone());
        for (i, row) in self.rows_of(store).into_iter().enumerate() {
            let Some(id) = row_id(&row) else {
                continue;
            };
            let absent: Vec<FieldDefinition> = def
                .fields
                .iter()
                .filter(|f| {
                    f.name.as_deref().is_some_and(|n| {
                        n != "id"
                            && n != "createdAt"
                            && n != "updatedAt"
                            && row.get_path(n).is_none()
                    })
                })
                .cloned()
                .collect();
            if absent.is_empty() {
                continue;
            }
            let patch = sample_row(
                &def.name,
                &absent,
                i + 1,
                def.persistence.as_ref(),
                def.identity,
                &mut Entropy::Seeded(&mut self.rng),
            );
            if !patch.is_empty() {
                let _ = self.source.write(&entity, &id, &Value::Object(patch));
            }
        }
    }

    /// The store's `@create` policy against `user` (no user: allowed, as the JS gate).
    fn may_create(&self, store: &str, candidate: &Value, user: Option<&Value>) -> bool {
        let Some(user) = user else {
            return true;
        };
        let mut ctx = orbital_core_eval::evaluator::context::BindingContext::new();
        ctx.set_user(user.clone());
        super::entity_access::check_mutation_access(
            orbital_core_base::effect_types::PersistOperation::Create,
            candidate,
            self.create_policies.get(store),
            &ctx,
            store,
        )
        .is_ok()
    }

    /// The store's `@read` policy admits `user` to `candidate`.
    fn may_read(&self, store: &str, candidate: &Value, user: &Value) -> bool {
        let Some(policy) = self.read_policies.get(store) else {
            return false;
        };
        let mut ctx = orbital_core_eval::evaluator::context::BindingContext::new();
        ctx.set_user(user.clone());
        let mut rows = vec![candidate.clone()];
        super::entity_access::apply_read_access(&mut rows, Some(policy), None, &ctx, store);
        !rows.is_empty()
    }

    /// The identities (seed order) that may own an owner column `@read` scopes by
    /// ownership — some identity is admitted only while the column holds its id —
    /// or `None` when `@read` does not scope it (then `@create` decides). Twin of
    /// `MockPersistenceAdapter.readOwners`.
    fn read_owners(&self, store: &str, column: &str, row: &ValueMap) -> Option<Vec<String>> {
        if column == "id" || !self.read_policies.contains_key(store) {
            return None;
        }
        let identity_store = self.owner_column_target_store(store, column)?;
        let with = |owner: &str| {
            let mut candidate = row.clone();
            candidate.insert(column.to_string(), Value::String(owner.to_string()));
            Value::Object(candidate)
        };
        let identities = self.rows_of(&identity_store);
        let admits_as_owner = |identity_row: &Value| {
            row_id(identity_row).is_some_and(|id| self.may_read(store, &with(&id), identity_row))
        };
        let scoped = identities
            .iter()
            .any(|i| admits_as_owner(i) && !self.may_read(store, &with(""), i));
        if !scoped {
            return None;
        }
        Some(
            identities
                .iter()
                .filter(|i| admits_as_owner(i))
                .filter_map(row_id)
                .collect(),
        )
    }

    /// May the viewer own `row`'s `column`: `@read` ownership when it scopes the
    /// column, else the `@create` gate on `candidate` (the row with the viewer stamped).
    fn viewer_may_own(
        &self,
        store: &str,
        column: &str,
        row: &ValueMap,
        viewer_id: &str,
        candidate: &Value,
    ) -> bool {
        match self.read_owners(store, column, row) {
            Some(readers) => readers.iter().any(|r| r == viewer_id),
            None => self.may_create(store, candidate, self.viewer),
        }
    }

    /// The identity store an owner column's relation targets, if it is a relation.
    fn owner_column_target_store(&self, store: &str, column: &str) -> Option<String> {
        self.registered.iter().find_map(|def| {
            if store_key_of(def) != store {
                return None;
            }
            let field = def
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(column))?;
            if !matches!(field.field_type, FieldType::Relation) {
                return None;
            }
            field.relation.as_ref().map(|r| self.target_store(r))
        })
    }

    fn first_eligible_owner(&self, store: &str, column: &str, row: &ValueMap) -> Option<String> {
        if let Some(readers) = self.read_owners(store, column, row) {
            return readers.into_iter().next();
        }
        let identity_store = self.owner_column_target_store(store, column)?;
        self.rows_of(&identity_store)
            .into_iter()
            .find_map(|identity_row| {
                let candidate_id = row_id(&identity_row)?;
                let mut candidate = row.clone();
                candidate.insert(column.to_string(), Value::String(candidate_id.clone()));
                self.may_create(store, &Value::Object(candidate), Some(&identity_row))
                    .then_some(candidate_id)
            })
    }

    /// Fill relation placeholders with real ids: a self-relation joins the forest (row
    /// *i*'s parent is row ⌊(i−1)/2⌋, a list holds rows 2i+1 and 2i+2), a self-relation
    /// that is an owner column points at the row itself, and a cross-entity relation
    /// takes [`cross_relation_value`] over the target rows (for an owner column, only
    /// identity rows that may create the row). Stamped and non-placeholder cells are
    /// kept. Twin of `MockPersistenceAdapter.linkRelationFields`.
    fn link_relations(&mut self) {
        let mut stores: Vec<String> = Vec::new();
        let mut fields_by_store: HashMap<String, Vec<&FieldDefinition>> = HashMap::new();
        for def in &self.registered {
            let store = store_key_of(def);
            if !stores.contains(&store) {
                stores.push(store.clone());
            }
            let fields = fields_by_store.entry(store).or_default();
            for f in &def.fields {
                if !matches!(f.field_type, FieldType::Relation) || f.relation.is_none() {
                    continue;
                }
                if f.name.is_some() && !fields.iter().any(|g| g.name == f.name) {
                    fields.push(f);
                }
            }
        }
        let mut owner_columns: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for (entity, col) in &self.owner_fields {
            owner_columns.insert((self.resolve_store_key(entity), col.clone()));
        }

        for store in stores {
            let Some(fields) = fields_by_store.get(&store) else {
                continue;
            };
            let rows = self.rows_of(&store);
            let entity = self
                .entity_of_store
                .get(&store)
                .cloned()
                .unwrap_or_else(|| store.clone());
            let ids: Vec<String> = rows.iter().filter_map(row_id).collect();
            for field in fields {
                let (Some(name), Some(relation)) = (field.name.as_deref(), field.relation.as_ref())
                else {
                    continue;
                };
                let target = self.target_store(relation);
                let target_rows = if target == store {
                    rows.clone()
                } else {
                    self.rows_of(&target)
                };
                if target_rows.is_empty() {
                    continue;
                }
                let is_owner = owner_columns.contains(&(store.clone(), name.to_string()));
                let scalar = is_scalar_cardinality(relation.cardinality.as_ref());
                for (i, row) in rows.iter().enumerate() {
                    let Some(id) = row_id(row) else {
                        continue;
                    };
                    if !is_relation_placeholder(row.get_path(name))
                        || self
                            .stamped
                            .contains(&(store.clone(), id.clone(), name.to_string()))
                    {
                        continue;
                    }
                    let value = if target == store {
                        if is_owner {
                            Value::String(id.clone())
                        } else if scalar {
                            Value::String(if i == 0 {
                                String::new()
                            } else {
                                ids[(i - 1) / 2].clone()
                            })
                        } else {
                            Value::Array(
                                [2 * i + 1, 2 * i + 2]
                                    .into_iter()
                                    .filter(|c| *c < ids.len())
                                    .map(|c| Value::String(ids[c].clone()))
                                    .collect(),
                            )
                        }
                    } else {
                        let readers = if is_owner {
                            row.as_object()
                                .and_then(|o| self.read_owners(&store, name, o))
                        } else {
                            None
                        };
                        let eligible: Vec<String> = if let Some(readers) = readers {
                            readers
                        } else {
                            target_rows
                                .iter()
                                .filter(|t| {
                                    !is_owner || {
                                        let mut candidate =
                                            row.as_object().cloned().unwrap_or_default();
                                        candidate.insert(
                                            name.to_string(),
                                            t.get_path("id").cloned().unwrap_or(Value::Null),
                                        );
                                        self.may_create(&store, &Value::Object(candidate), Some(t))
                                    }
                                })
                                .filter_map(row_id)
                                .collect()
                        };
                        let Some(value) =
                            cross_relation_value(&eligible, i, relation.cardinality.as_ref())
                        else {
                            continue;
                        };
                        value
                    };
                    let _ = self.source.write(
                        &entity,
                        &id,
                        &Value::Object(ValueMap::from([(name.to_string(), value)])),
                    );
                }
            }
            tracing::debug!(target: orbital_core_base::logging::ns::SEED, store = %store, "seed:relations-linked");
        }
    }

    /// Fill each `T.f` placeholder from `T`'s seeded rows: row `i` takes row `i mod n`'s `f`
    /// (wrapped in an array for an array field). Only placeholder cells are filled, as for
    /// relations. JS twin: `MockPersistenceAdapter.linkProjectedFields`.
    fn link_projections(&mut self) {
        let mut done: Vec<(String, String)> = Vec::new();
        for def in self.registered.clone() {
            let store = store_key_of(def);
            let entity = self
                .entity_of_store
                .get(&store)
                .cloned()
                .unwrap_or_else(|| store.clone());
            for field in &def.fields {
                let (Some(name), Some((projection, source_field))) = (field.name.as_deref(), projection_of(field))
                else {
                    continue;
                };
                if done.contains(&(store.clone(), name.to_string())) {
                    continue;
                }
                done.push((store.clone(), name.to_string()));
                let target_rows = self.rows_of(&self.resolve_store_key(&projection.type_name));
                if target_rows.is_empty() {
                    continue;
                }
                for (i, row) in self.rows_of(&store).iter().enumerate() {
                    let Some(id) = row_id(row) else { continue };
                    if !is_relation_placeholder(row.get_path(name))
                        || self
                            .stamped
                            .contains(&(store.clone(), id.clone(), name.to_string()))
                    {
                        continue;
                    }
                    let picked = target_rows[i % target_rows.len()]
                        .get_path(source_field)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let value = if field.field_type == FieldType::Array {
                        Value::Array(vec![picked])
                    } else {
                        picked
                    };
                    let _ = self.source.write(
                        &entity,
                        &id,
                        &Value::Object(ValueMap::from([(name.to_string(), value)])),
                    );
                }
            }
        }
    }
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
        Value::Object(ValueMap::from([(
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
mod tests {
    use super::*;
    use orbital_core_base::value::ValueMap;

    #[test]
    fn named_source_provenance_preserves_expanded_sampling_and_real_projections() {
        let mut field: FieldDefinition = serde_json::from_value(serde_json::json!({
            "name":"row","type":"object","properties":{"label":{"type":"string"}},
            "projectedFrom":{"typeName":"Shared"}
        })).unwrap();
        assert!(projection_of(&field).is_none());
        let sampled = sample_value(&field, "Note", 0, None, 0, &mut Entropy::Index).unwrap();
        assert!(sampled.get_path("label").is_some());
        field.projected_from.as_mut().unwrap().field = Some("label".into());
        assert_eq!(projection_of(&field).unwrap().1, "label");
        assert_eq!(sample_value(&field, "Note", 0, None, 0, &mut Entropy::Index), Some(Value::String(String::new())));
        let mut array: FieldDefinition = serde_json::from_value(serde_json::json!({
            "type":"array","items":{"type":"string","projectedFrom":{"typeName":"Shared"}}
        })).unwrap();
        assert!(projection_of(&array).is_none());
        array.items.as_mut().unwrap().projected_from.as_mut().unwrap().field = Some("label".into());
        assert_eq!(projection_of(&array).unwrap().1, "label");
    }

    fn rows(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| {
                let mut m = ValueMap::new();
                m.insert("id".to_string(), Value::String(format!("r{}", i)));
                m.insert(
                    "memberId".to_string(),
                    Value::String("someone-else".to_string()),
                );
                Value::Object(m)
            })
            .collect()
    }

    fn owners_of(rows: &[Value]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| {
                r.get_path("memberId")
                    .and_then(|v| v.as_str().map(str::to_string))
            })
            .collect()
    }

    #[test]
    fn stamps_the_viewer_on_every_other_row_of_a_declared_column() {
        let mut r = rows(4);
        let fields = vec![("rosterentry".to_string(), "memberId".to_string())];
        stamp_owner(&mut r, "RosterEntry", Some("member-1"), &fields);
        assert_eq!(
            owners_of(&r),
            vec!["member-1", "someone-else", "member-1", "someone-else"]
        );
    }

    #[test]
    fn leaves_undeclared_entities_and_columns_alone() {
        let mut r = rows(2);
        let fields = vec![("appointment".to_string(), "patientId".to_string())];
        stamp_owner(&mut r, "RosterEntry", Some("member-1"), &fields);
        assert_eq!(owners_of(&r), vec!["someone-else", "someone-else"]);
    }

    #[test]
    fn without_a_viewer_nothing_is_stamped() {
        let mut r = rows(2);
        let fields = vec![("rosterentry".to_string(), "memberId".to_string())];
        stamp_owner(&mut r, "RosterEntry", None, &fields);
        assert_eq!(owners_of(&r), vec!["someone-else", "someone-else"]);
    }

    #[test]
    fn owner_field_specs_are_parsed_as_entity_dot_field_pairs() {
        std::env::set_var(
            OWNER_FIELDS_ENV,
            "RosterEntry.memberId, Appointment.patientId ,bogus,",
        );
        let parsed = owner_fields_from_env();
        std::env::remove_var(OWNER_FIELDS_ENV);
        assert_eq!(
            parsed,
            vec![
                ("rosterentry".to_string(), "memberId".to_string()),
                ("appointment".to_string(), "patientId".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod identity_owner_tests {
    use super::*;
    use serde_json::json;

    /// A schema with one `[identity]` entity and one entity holding a relation
    /// column that points at it (`patientId : Person` in `.lolo`).
    fn schema_with_identity(identity: bool, relation_target: &str) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                {
                    "name": "PersonOrbital",
                    "entity": {
                        "name": "Person",
                        "identity": identity,
                        "persistence": "persistent",
                        "collection": "people",
                        "fields": [{ "name": "id", "type": "string", "required": true }]
                    },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "AppointmentOrbital",
                    "entity": {
                        "name": "Appointment",
                        "persistence": "persistent",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            {
                                "name": "patientId",
                                "type": "relation",
                                "relation": { "entity": relation_target, "cardinality": "one" }
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize")
    }

    #[test]
    fn finds_the_declared_identity_entity() {
        let schema = schema_with_identity(true, "Person");
        assert_eq!(identity_entity_name(&schema), Some("Person"));
    }

    #[test]
    fn no_identity_entity_means_no_derived_owner_columns() {
        let schema = schema_with_identity(false, "Person");
        assert_eq!(identity_entity_name(&schema), None);
        assert!(owner_fields_from_schema(&schema).is_empty());
    }

    /// The ownership column is DERIVED from the declared relation target, not
    /// guessed from the field name.
    #[test]
    fn derives_owner_columns_from_identity_typed_relations() {
        let schema = schema_with_identity(true, "Person");
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("appointment".to_string(), "patientId".to_string())]
        );
    }

    /// A relation pointing at some OTHER entity is not an ownership column, even
    /// though the field is named exactly like one.
    #[test]
    fn a_relation_to_a_non_identity_entity_is_not_an_owner_column() {
        let schema = schema_with_identity(true, "Clinic");
        assert!(owner_fields_from_schema(&schema).is_empty());
    }

    /// Collection mirror of entity-access policy inheritance: an entity sharing
    /// a collection with a declaring sibling (a composed atom's own entity —
    /// which structurally CANNOT declare a relation to the app's identity
    /// entity) inherits the sibling's same-named owner column. A same-named
    /// column in a DIFFERENT collection stays untouched.
    #[test]
    fn same_collection_sibling_inherits_owner_column_by_name() {
        let mut schema = schema_with_identity(true, "Person");
        let appointment = &mut schema.orbitals[1];
        if let EntityRef::Inline(def) = &mut appointment.entity {
            def.collection = Some("appointments".to_string());
        }
        let atom_view: EntityDefinition = serde_json::from_value(json!({
            "name": "AppointmentView",
            "persistence": "persistent",
            "collection": "appointments",
            "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "patientId", "type": "string" }
            ]
        }))
        .expect("aux entity fixture must deserialize");
        let other_collection: EntityDefinition = serde_json::from_value(json!({
            "name": "Invoice",
            "persistence": "persistent",
            "collection": "invoices",
            "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "patientId", "type": "string" }
            ]
        }))
        .expect("entity fixture must deserialize");
        schema.orbitals[1].auxiliary_entities = Some(vec![
            EntityRef::Inline(atom_view),
            EntityRef::Inline(other_collection),
        ]);
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![
                ("appointment".to_string(), "patientId".to_string()),
                ("appointmentview".to_string(), "patientId".to_string()),
            ]
        );
    }

    /// The explicit-viewer seeder stamps owner columns with the viewer it is GIVEN,
    /// with no `ALMADAR_PERSONA` in the environment. A headless harness that binds a
    /// viewer programmatically (the L2 render probe's roster default) must seed rows
    /// that viewer owns — binding alone leaves an ownership-scoped `@read` matching
    /// nothing, which is exactly how C-VERIFY-UNBOUND-VIEWER-FAILS-EVERY-AUTHORIZED-APP
    /// survived its first attempted fix.
    #[test]
    fn explicit_viewer_stamps_owner_columns_without_the_env() {
        let schema = schema_with_identity(true, "Person");
        let cache = crate::entity_source::InMemoryEntityCache::new();
        seed_sample_data_as(&cache, &schema, 4, Some("Person-1".to_string()));

        let owners: Vec<Option<String>> = cache
            .list("Appointment")
            .expect("seeded rows must be listable")
            .iter()
            .map(|r| {
                r.get_path("patientId")
                    .and_then(|v| v.as_str().map(str::to_string))
            })
            .collect();
        assert_eq!(owners.len(), 4);
        assert_eq!(
            owners
                .iter()
                .filter(|o| o.as_deref() == Some("Person-1"))
                .count(),
            2,
            "every other row belongs to the supplied viewer: {owners:?}"
        );
    }

    /// `None` reproduces the unstamped behavior — the seeder must not invent a
    /// viewer when none is supplied, or a genuinely broken filter would look
    /// correct. The relation-linking pass still fills the column with REAL
    /// Person ids (each row a different sibling); what must never appear is the
    /// stamp PATTERN — one uniform viewer id repeated across every other row.
    #[test]
    fn no_explicit_viewer_leaves_owner_columns_unstamped() {
        let schema = schema_with_identity(true, "Person");
        let cache = crate::entity_source::InMemoryEntityCache::new();
        seed_sample_data_as(&cache, &schema, 4, None);

        let rows = cache
            .list("Appointment")
            .expect("seeded rows must be listable");
        let mut counts: HashMap<String, usize> = HashMap::new();
        for r in &rows {
            let patient = r
                .get_path("patientId")
                .and_then(|v| v.as_str())
                .expect("linking fills the relation column");
            assert!(
                patient.starts_with("Person Id "),
                "must be a real Person id"
            );
            *counts.entry(patient.to_string()).or_insert(0) += 1;
        }
        let max_repeats = counts.values().copied().max().unwrap_or(0);
        assert!(
            max_repeats <= 1,
            "no single id may repeat like a viewer stamp would: {counts:?}"
        );
    }
}

/// Self-identity: the `[identity]` entity's OWN `id` is the ownership key when its
/// access policy compares the row directly to the viewer. `std-time-tracking`'s
/// `Employee [identity]` declares
/// `@read (or (= @user.role "approver") (= (object/get @entity id) @user.id))`,
/// which the `.lolo` -> `.orb` lowering compiles to
/// `["=", ["object/get", "@entity", "id"], "@user.id"]` (verified against
/// `packages/almadar-behaviors/behaviors/registry/app/organisms/std-time-tracking.orb`)
/// — distinct from a relation column pointing AT the identity entity. JS twin:
/// `identity-owner-fields.test.ts`'s "self-identity ownership" block.
#[cfg(test)]
mod self_identity_owner_tests {
    use super::*;
    use serde_json::json;

    /// A lone `[identity]` entity declaring the given `@read` policy, no other entities.
    fn schema_with_self_identity_policy(read_policy: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "EmployeeOrbital",
                "entity": {
                    "name": "Employee",
                    "identity": true,
                    "persistence": "persistent",
                    "read_policy": read_policy,
                    "fields": [{ "name": "id", "type": "string", "required": true }]
                },
                "traits": [],
                "pages": []
            }]
        }))
        .expect("schema fixture must deserialize")
    }

    #[test]
    fn object_get_spelling_stamps_entity_id() {
        let schema = schema_with_self_identity_policy(json!([
            "or",
            ["=", "@user.role", "approver"],
            ["=", ["object/get", "@entity", "id"], "@user.id"]
        ]));
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("employee".to_string(), "id".to_string())]
        );
    }

    #[test]
    fn matches_either_operand_order() {
        let schema = schema_with_self_identity_policy(json!([
            "=",
            "@user.id",
            ["object/get", "@entity", "id"]
        ]));
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("employee".to_string(), "id".to_string())]
        );
    }

    #[test]
    fn dotted_path_spelling_also_matches() {
        let schema = schema_with_self_identity_policy(json!(["=", "@entity.id", "@user.id"]));
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("employee".to_string(), "id".to_string())]
        );
    }

    /// `(object/get @entity managerId) == @user.id` is a relation to the SAME
    /// identity roster, not the row's own id — the field-type scan already finds
    /// it; self-identity must not ALSO claim `Employee.id`.
    #[test]
    fn relation_column_stays_a_relation_owner_column_not_self_identity() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "EmployeeOrbital",
                "entity": {
                    "name": "Employee",
                    "identity": true,
                    "persistence": "persistent",
                    "read_policy": ["=", ["object/get", "@entity", "managerId"], "@user.id"],
                    "fields": [
                        { "name": "id", "type": "string", "required": true },
                        {
                            "name": "managerId",
                            "type": "relation",
                            "relation": { "entity": "Employee", "cardinality": "one" }
                        }
                    ]
                },
                "traits": [],
                "pages": []
            }]
        }))
        .expect("schema fixture must deserialize");
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("employee".to_string(), "managerId".to_string())]
        );
    }

    #[test]
    fn entity_id_compared_to_a_literal_is_not_ownership() {
        let schema =
            schema_with_self_identity_policy(json!(["=", ["object/get", "@entity", "id"], "x"]));
        assert!(owner_fields_from_schema(&schema).is_empty());
    }
}

/// Convergence with the codegen twin (`access_policies.rs::owner_fields`,
/// which shares `owner_columns_from_policy`): a policy naming a NON-relation
/// field is an owner column the type-only scan alone can never find — the
/// shape `ORB_S_OWNER_FIELD_NOT_IDENTITY_TYPED` flags as an authoring error,
/// but the seeder must still stamp it, matching what codegen already emits
/// as `ownerFields:`.
#[cfg(test)]
mod policy_derived_owner_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_non_relation_field_named_by_read_policy_is_an_owner_column() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                {
                    "name": "PersonOrbital",
                    "entity": { "name": "Person", "identity": true, "fields": [{ "name": "id", "type": "string" }] },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "NoteOrbital",
                    "entity": {
                        "name": "Note",
                        "read_policy": ["==", ["object/get", "@entity", "authorId"], "@user.id"],
                        "fields": [{ "name": "authorId", "type": "string" }]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize");
        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("note".to_string(), "authorId".to_string())]
        );
    }

    #[test]
    fn create_policy_is_excluded_from_the_policy_derived_arm() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                {
                    "name": "PersonOrbital",
                    "entity": { "name": "Person", "identity": true, "fields": [{ "name": "id", "type": "string" }] },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "NoteOrbital",
                    "entity": {
                        "name": "Note",
                        "create_policy": ["=", ["object/get", "@entity", "authorId"], "@user.id"],
                        "fields": [{ "name": "authorId", "type": "string" }]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize");
        assert!(owner_fields_from_schema(&schema).is_empty());
    }
}

/// `std-time-tracking`'s regression: a generic atom's self-relation
/// (`ModalRecord.seedRow : ModalRecord`, record-detail/modal edit-seed
/// plumbing — `@intrinsic` in `.lolo`) gets its relation target rewritten onto
/// whatever entity the composing trait binds, so `Employee [identity]` ends up
/// with an `Employee.seedRow : Employee` self-relation that is
/// type/target-indistinguishable from a real owner column. Before the fix,
/// `owner_fields_from_schema` classified it as an owner column, the seed
/// stamped the viewer id into it, and the seeded row referenced itself —
/// tripping `Employee`'s own `onDelete: restrict` on delete. JS twin:
/// `identity-owner-fields.test.ts`'s "intrinsic relation fields" block.
#[cfg(test)]
mod intrinsic_relation_field_owner_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn excludes_an_intrinsic_self_relation_to_the_identity_entity_even_alongside_a_real_owner_column(
    ) {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                {
                    "name": "EmployeeOrbital",
                    "entity": {
                        "name": "Employee",
                        "identity": true,
                        "persistence": "persistent",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            {
                                "name": "seedRow",
                                "type": "relation",
                                "relation": { "entity": "Employee", "cardinality": "one" },
                                "intrinsic": true
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "TimesheetOrbital",
                    "entity": {
                        "name": "Timesheet",
                        "fields": [
                            {
                                "name": "employeeId",
                                "type": "relation",
                                "relation": { "entity": "Employee", "cardinality": "one" }
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize");

        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("timesheet".to_string(), "employeeId".to_string())]
        );
    }

    #[test]
    fn an_intrinsic_relation_to_a_non_identity_entity_is_unaffected_either_way() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                {
                    "name": "MessageOrbital",
                    "entity": {
                        "name": "Message",
                        "fields": [
                            {
                                "name": "sender",
                                "type": "relation",
                                "relation": { "entity": "Person", "cardinality": "one" }
                            },
                            {
                                "name": "draftSeed",
                                "type": "relation",
                                "relation": { "entity": "Message", "cardinality": "one" },
                                "intrinsic": true
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "PersonOrbital",
                    "entity": {
                        "name": "Person",
                        "identity": true,
                        "fields": [{ "name": "id", "type": "string", "required": true }]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize");

        assert_eq!(
            owner_fields_from_schema(&schema),
            vec![("message".to_string(), "sender".to_string())]
        );
    }
}

#[cfg(test)]
mod relation_link_tests {
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use serde_json::json;

    fn doc_schema() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "WikiApp",
            "orbitals": [
                {
                    "name": "PageOrbital",
                    "entity": {
                        "name": "WikiPage",
                        "persistence": "persistent",
                        "collection": "wikipages",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            { "name": "title", "type": "string", "required": true },
                            {
                                "name": "parentId",
                                "type": "relation",
                                "relation": { "entity": "WikiPage", "cardinality": "one" },
                                "default": ""
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "RevisionOrbital",
                    "entity": {
                        "name": "WikiRevision",
                        "persistence": "persistent",
                        "collection": "wikirevisions",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            {
                                "name": "documentId",
                                "type": "relation",
                                "relation": { "entity": "WikiPage", "cardinality": "one" }
                            },
                            {
                                "name": "tagIds",
                                "type": "relation",
                                "relation": { "entity": "WikiPage", "cardinality": "many" }
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize")
    }

    fn ids_of(rows: &[std::sync::Arc<Value>]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| {
                r.get_path("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    /// A self-referential `one` relation links as an acyclic forest with a root.
    #[test]
    fn self_relation_links_an_acyclic_forest_with_a_root() {
        let source = InMemoryEntityCache::new();
        let schema = doc_schema();
        seed_sample_data_as(&source, &schema, 6, None);

        let rows = source.list("WikiPage").expect("pages seeded");
        let ids: std::collections::HashSet<String> = ids_of(&rows).into_iter().collect();

        let roots = rows
            .iter()
            .filter(|r| r.get_path("parentId").and_then(|v| v.as_str()) == Some(""))
            .count();
        assert!(
            roots > 0,
            "at least one root row must keep the empty parent"
        );

        // Every non-root parent is a REAL sibling id, never self; parent walks terminate.
        for row in &rows {
            let id = row.get_path("id").and_then(|v| v.as_str()).unwrap();
            let parent = row.get_path("parentId").and_then(|v| v.as_str()).unwrap();
            if parent.is_empty() {
                continue;
            }
            assert!(ids.contains(parent), "parent {parent} must be a seeded id");
            assert_ne!(parent, id, "no row may parent itself");
        }
        let by_id: HashMap<&str, &Value> = rows
            .iter()
            .map(|r| (r.get_path("id").and_then(|v| v.as_str()).unwrap(), &**r))
            .collect();
        for row in &rows {
            let mut current: &Value = row;
            let mut hops = 0;
            loop {
                let parent = current
                    .get_path("parentId")
                    .and_then(|v| v.as_str())
                    .unwrap();
                if parent.is_empty() {
                    break;
                }
                current = by_id[parent];
                hops += 1;
                assert!(hops <= rows.len(), "parent chain must be acyclic");
            }
        }
    }

    /// Cross-entity relations link real target ids for both cardinalities.
    #[test]
    fn cross_entity_relations_link_real_target_ids() {
        let source = InMemoryEntityCache::new();
        let schema = doc_schema();
        seed_sample_data_as(&source, &schema, 6, None);

        let page_ids: std::collections::HashSet<String> =
            ids_of(&source.list("WikiPage").expect("pages seeded"))
                .into_iter()
                .collect();
        let revisions = source.list("WikiRevision").expect("revisions seeded");
        assert!(!revisions.is_empty());
        for rev in &revisions {
            let document_id = rev.get_path("documentId").and_then(|v| v.as_str()).unwrap();
            assert!(
                page_ids.contains(document_id),
                "documentId {document_id} must be a seeded page id"
            );
            let tags = match rev.get_path("tagIds") {
                Some(Value::Array(a)) => a,
                other => panic!("tagIds must be an array, got {other:?}"),
            };
            assert!(!tags.is_empty(), "many-relation must link at least one id");
            for tag in tags {
                assert!(page_ids.contains(tag.as_str().unwrap()));
            }
        }
    }

    /// An owner-stamped relation cell is preserved: the linking pass only fills
    /// placeholders, so the viewer id `stamp_owner` wrote must survive.
    #[test]
    fn owner_stamp_survives_linking() {
        let source = InMemoryEntityCache::new();
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "OwnedApp",
            "orbitals": [
                {
                    "name": "PersonOrbital",
                    "entity": {
                        "name": "Person",
                        "identity": true,
                        "persistence": "persistent",
                        "collection": "people",
                        "fields": [{ "name": "id", "type": "string", "required": true }]
                    },
                    "traits": [],
                    "pages": []
                },
                {
                    "name": "NoteOrbital",
                    "entity": {
                        "name": "Note",
                        "persistence": "persistent",
                        "collection": "notes",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            {
                                "name": "authorId",
                                "type": "relation",
                                "relation": { "entity": "Person", "cardinality": "one" }
                            }
                        ]
                    },
                    "traits": [],
                    "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize");

        seed_sample_data_as(&source, &schema, 6, Some("viewer-1".to_string()));
        let notes = source.list("Note").expect("notes seeded");
        // stamp_owner assigns the viewer every OTHER row (i % 2 == 0).
        for (i, note) in notes.iter().enumerate() {
            let author = note.get_path("authorId").and_then(|v| v.as_str()).unwrap();
            if i % 2 == 0 {
                assert_eq!(author, "viewer-1", "owner stamp must survive linking");
            } else {
                assert!(
                    author.starts_with("Person Id "),
                    "unstamped rows link a real person, got {author}"
                );
            }
        }
    }
}

#[cfg(test)]
mod collection_mate_seed_tests {
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use serde_json::json;

    /// `Deal` owns the `deals` collection (with an owner column); `PipelineDeal`
    /// is a narrower view of the same table that declares no owner column.
    fn crm() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "Crm",
            "orbitals": [
                {
                    "name": "PersonOrbital",
                    "entity": { "name": "Person", "identity": true, "persistence": "persistent", "collection": "people",
                        "fields": [{ "name": "id", "type": "string", "required": true }] },
                    "traits": [], "pages": []
                },
                {
                    "name": "DealOrbital",
                    "entity": { "name": "Deal", "persistence": "persistent", "collection": "deals",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            { "name": "title", "type": "string" },
                            { "name": "owner", "type": "relation", "relation": { "entity": "Person", "cardinality": "one" } }
                        ] },
                    "traits": [], "pages": []
                },
                {
                    "name": "PipelineOrbital",
                    "entity": { "name": "PipelineDeal", "persistence": "persistent", "collection": "deals",
                        "fields": [
                            { "name": "id", "type": "string", "required": true },
                            { "name": "title", "type": "string" }
                        ] },
                    "traits": [], "pages": []
                }
            ]
        }))
        .expect("schema fixture must deserialize")
    }

    fn bind_all(cache: &InMemoryEntityCache, schema: &OrbitalSchema) {
        for def in inline_entity_defs(schema) {
            if let Some(c) = &def.collection {
                cache.bind_collection(&def.name, c);
            }
        }
    }

    /// A collection is seeded once, from its first declarer: the mate lists the
    /// same owner-stamped rows, so an ownership policy it inherits from the
    /// collection matches them (G-ORB-005).
    #[test]
    fn a_collection_is_seeded_once_and_its_mate_sees_owned_rows() {
        let schema = crm();
        let cache = InMemoryEntityCache::new();
        bind_all(&cache, &schema);
        seed_sample_data_as(&cache, &schema, 3, Some("u1".to_string()));
        let pipeline = cache.list("PipelineDeal").expect("list");
        assert_eq!(
            pipeline.len(),
            3,
            "one seeding of `deals`, not two: {pipeline:?}"
        );
        assert_eq!(
            pipeline,
            cache.list("Deal").expect("list"),
            "the same rows as `Deal`"
        );
        assert!(
            pipeline
                .iter()
                .any(|r| r.as_object().and_then(|o| o.get("owner"))
                    == Some(&Value::String("u1".into()))),
            "rows the viewer owns reach the mate: {pipeline:?}"
        );
    }

    #[test]
    fn control_entities_on_different_collections_are_each_seeded() {
        let schema = crm();
        let cache = InMemoryEntityCache::new();
        bind_all(&cache, &schema);
        seed_sample_data_as(&cache, &schema, 2, Some("u1".to_string()));
        assert_eq!(cache.list("Person").expect("list").len(), 2);
        assert_eq!(cache.list("Deal").expect("list").len(), 2);
    }
}

#[cfg(test)]
mod read_owner_tests {
    //! Twin of `@almadar/runtime` `test/mock-owner-read-spread.test.ts`: an owner
    //! column `@read` scopes by ownership is owned by the identities `@read`
    //! admits (the viewer keeps every other row, the rest round-robin over the
    //! admitted roster), so an ownership-scoped default persona sees its own
    //! rows (std-hr-portal `Employee.staffAccount`, G-ORB-024).
    use super::*;
    use crate::entity_source::InMemoryEntityCache;
    use serde_json::json;

    fn schema(read: Option<serde_json::Value>) -> OrbitalSchema {
        let mut employee = json!({
            "name": "Employee", "persistence": "persistent", "collection": "employees",
            "create_policy": ["=", "@user.role", "hr"],
            "fields": [
                { "name": "id", "type": "string", "required": true },
                { "name": "name", "type": "string" },
                { "name": "staffAccount", "type": "relation", "relation": { "entity": "Staff", "cardinality": "one" } }
            ]
        });
        if let Some(read) = read {
            employee["read_policy"] = read;
        }
        serde_json::from_value(json!({
            "name": "Hr",
            "orbitals": [
                { "name": "StaffOrbital",
                  "entity": { "name": "Staff", "identity": true, "persistence": "persistent", "collection": "staff",
                    "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "role", "type": "string" }] },
                  "traits": [], "pages": [] },
                { "name": "EmployeeOrbital", "entity": employee, "traits": [], "pages": [] }
            ]
        }))
        .expect("schema fixture must deserialize")
    }

    fn owners(read: Option<serde_json::Value>) -> Vec<String> {
        let schema = schema(read);
        let cache = InMemoryEntityCache::new();
        cache.bind_collection("Staff", "staff");
        cache.bind_collection("Employee", "employees");
        for (id, role) in [
            ("s1", "employee"),
            ("s2", "manager"),
            ("s3", "hr"),
            ("s4", "employee"),
        ] {
            let row = Value::Object(ValueMap::from([
                ("id".to_string(), Value::String(id.to_string())),
                ("role".to_string(), Value::String(role.to_string())),
            ]));
            cache.create("Staff", &row).expect("roster row");
        }
        seed_sample_data_as(&cache, &schema, 6, Some("s1".to_string()));
        cache
            .list("Employee")
            .expect("employees seeded")
            .iter()
            .map(|r| {
                r.get_path("staffAccount")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    fn ownership_read() -> serde_json::Value {
        json!([
            "or",
            ["=", "@user.role", "hr"],
            ["=", "@user.role", "manager"],
            ["=", ["object/get", "@entity", "staffAccount"], "@user.id"]
        ])
    }

    #[test]
    fn the_viewer_keeps_every_other_row_and_the_rest_go_round_robin_over_the_admitted_roster() {
        assert_eq!(
            owners(Some(ownership_read())),
            ["s1", "s2", "s1", "s4", "s1", "s2"]
        );
    }

    #[test]
    fn control_without_a_read_policy_the_create_authority_decides() {
        assert!(owners(None).iter().all(|o| o == "s3"));
    }

    #[test]
    fn edge_a_role_only_read_admits_nobody_by_ownership_so_create_decides() {
        assert!(owners(Some(json!(["=", "@user.role", "hr"])))
            .iter()
            .all(|o| o == "s3"));
    }
}

#[cfg(test)]
mod seeded_date_window_tests {
    use super::*;

    // Twin of the TS "seeded dates span months, not weeks" tests. The clock is the
    // epoch, so ±120 days is 1969-09-03 .. 1970-05-01 and ISO strings compare in order.
    fn seeded_dates() -> Vec<String> {
        let mut rng = MockRandom::new(7, 0.0);
        (1..=200)
            .map(
                |i| match sample_date(i, false, &mut Entropy::Seeded(&mut rng)) {
                    Value::String(s) => s,
                    other => panic!("a seeded date is a string, got {other:?}"),
                },
            )
            .collect()
    }

    #[test]
    fn seeded_dates_stay_within_120_days_either_side_of_today() {
        for d in seeded_dates() {
            assert!(
                d.as_str() >= "1969-09-03" && d.as_str() <= "1970-05-02",
                "{d}"
            );
        }
    }

    #[test]
    fn seeded_dates_span_months_not_weeks() {
        let all = seeded_dates();
        assert!(all.iter().any(|d| d.as_str() < "1969-11-02"));
        assert!(all.iter().any(|d| d.as_str() > "1970-03-02"));
    }

    #[test]
    fn control_the_index_strategy_still_dates_row_n_in_month_n() {
        assert_eq!(
            sample_date(3, true, &mut Entropy::Index),
            Value::String("2026-04-15".to_string())
        );
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
            .map(|i| {
                sample_value(field, "Invoice", i, None, 0, &mut Entropy::Index)
                    .expect("a required field seeds")
            })
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
            .map(|i| {
                sample_value(
                    &status(Some("paid")),
                    "Invoice",
                    i,
                    None,
                    0,
                    &mut Entropy::Index,
                )
                .expect("seeds")
            })
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

    fn sample(f: &FieldDefinition, index: usize, entropy: &mut Entropy) -> Value {
        sample_value(f, "Member", index, None, 0, entropy).expect("a mock value")
    }

    // Twin of the TS "relative @mock dates (G-CORE-019)" index cases.
    #[test]
    fn index_strategy_offsets_resolve_against_the_fixed_anchor() {
        let posted = field("postedAt", FieldType::Datetime, "-1d, -2h, -3w");
        assert_eq!(
            sample(&posted, 1, &mut Entropy::Index),
            Value::String("2026-06-14T00:00:00.000Z".into())
        );
        assert_eq!(
            sample(&posted, 2, &mut Entropy::Index),
            Value::String("2026-06-14T22:00:00.000Z".into())
        );
        assert_eq!(
            sample(&posted, 3, &mut Entropy::Index),
            Value::String("2026-05-25T00:00:00.000Z".into())
        );
        let due = field("dueOn", FieldType::Date, "+2d");
        assert_eq!(
            sample(&due, 1, &mut Entropy::Index),
            Value::String("2026-06-17".into())
        );
    }

    #[test]
    fn seeded_strategy_offsets_resolve_against_the_rng_clock() {
        let mut rng = MockRandom::new(7, 0.0);
        let past = sample(
            &field("postedAt", FieldType::Datetime, "-1d"),
            1,
            &mut Entropy::Seeded(&mut rng),
        );
        assert_eq!(past, Value::String("1969-12-31T00:00:00.000Z".into()));
    }

    #[test]
    fn control_a_fixed_literal_is_used_as_written_and_a_string_field_keeps_it_literally() {
        assert_eq!(
            sample(
                &field("opened", FieldType::Date, "2026-01-02"),
                2,
                &mut Entropy::Index
            ),
            Value::String("2026-01-02".into())
        );
        assert_eq!(
            sample(
                &field("code", FieldType::String, "-1d, x"),
                1,
                &mut Entropy::Index
            ),
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
        let v = sample_value(
            &field(FieldType::Trait),
            "Panel",
            1,
            None,
            0,
            &mut Entropy::Index,
        );
        assert_eq!(v, Some(Value::Null));
    }

    #[test]
    fn control_other_reference_types_still_omit_the_key() {
        for ty in [FieldType::Slot, FieldType::Pattern, FieldType::SExpr] {
            assert_eq!(
                sample_value(&field(ty), "Panel", 1, None, 0, &mut Entropy::Index),
                None
            );
        }
    }
}

#[allow(unused_imports)]
use orbital_core_base::policy::{field_ref_name, is_field_ref, POLICY_EQ_OPS, is_viewer_id, collect_owner_columns_from_policy, policies_self_identify, stamps_row};
#[allow(unused_imports)]
pub use orbital_core_base::policy::{mock_candidates, identity_entity_def, identity_entity_name, owner_columns_from_policy, owner_fields_from_schema, stamp_owner};
#[allow(unused_imports)]
pub(crate) use orbital_core_base::policy::{inline_entity_defs};
