//! Field, record and payload shapes derived from declared types.

use super::*;

/// What a declared type leaves without a shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeGap {
    /// An `object` with neither `properties`, `items` nor `additionalProperties`.
    ObjectWithoutShape,
    /// An `array` with no `items`.
    ArrayWithoutItems,
    /// A `relation` that names no entity.
    RelationWithoutEntity,
    /// A `union` with neither variants nor a by-name reference.
    UnionWithoutVariants,
    /// A `field` whose record is named by neither `record` nor `properties`.
    FieldWithoutRecord,
    /// A type text that is empty.
    EmptyTypeText,
    /// A bare `array`/`list` type text: no element type.
    BareArrayText,
    /// A type text of the retired generic spelling (`json`, `tensor`).
    GenericText(String),
}

impl std::fmt::Display for ShapeGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ObjectWithoutShape => f.write_str("an object with no properties, items or additionalProperties"),
            Self::ArrayWithoutItems => f.write_str("an array with no element type"),
            Self::RelationWithoutEntity => f.write_str("a relation that names no entity"),
            Self::UnionWithoutVariants => f.write_str("a union with no variants and no by-name reference"),
            Self::FieldWithoutRecord => f.write_str("a field type that names no record"),
            Self::EmptyTypeText => f.write_str("an empty type"),
            Self::BareArrayText => f.write_str("an array type with no element type"),
            Self::GenericText(text) => write!(f, "the generic type `{text}`"),
        }
    }
}

/// A type position that declares no shape, with the path from the type the caller asked about to it
/// (a property name, or `[]` for an element).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeError {
    pub path: Vec<String>,
    pub gap: ShapeGap,
}

impl ShapeError {
    pub fn at(gap: ShapeGap) -> Self {
        Self { path: Vec::new(), gap }
    }

    /// The same gap, one level further out: it sits under `segment` of the type now being read.
    pub fn within(mut self, segment: impl Into<String>) -> Self {
        self.path.insert(0, segment.into());
        self
    }
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "no declared shape: {}", self.gap)
        } else {
            write!(f, "no declared shape at `{}`: {}", self.path.join("."), self.gap)
        }
    }
}

impl std::error::Error for ShapeError {}

fn shapes<'a, T: 'a>(
    members: impl IntoIterator<Item = (String, &'a T)>,
    shape: impl Fn(&T) -> Result<OirType, ShapeError>,
) -> Result<Vec<OirField>, ShapeError> {
    members
        .into_iter()
        .map(|(name, def)| shape(def).map(|ty| OirField::new(name.clone(), ty)).map_err(|e| e.within(name)))
        .collect()
}

/// Structural `FieldDefinition` → `OirType`, needing no resolver context.
///
/// An `Object` is its `properties` as a struct, or a `Map` of its `items`; a `Union` that is a by-name
/// back-reference keeps the type it names ([`FieldDefinition::by_name_reference`]). Named-type registration is
/// the resolver's, not this function's. `Relation` builds a real `Reference` when it has one. A position that
/// declares no shape (an `object` without members, an `array` without `items`, a `relation` without an entity) is
/// a [`ShapeError`] naming where.
pub fn field_shape(field: &FieldDefinition) -> Result<OirType, ShapeError> {
    let shape = field_shape_inner(field)?;
    Ok(if field.nullable == Some(true) { OirType::optional(shape) } else { shape })
}

fn property_fields(properties: &crate::collections::IndexMap<String, FieldDefinition>) -> Result<Vec<OirField>, ShapeError> {
    let mut fields = properties
        .iter()
        .map(|(name, def)| {
            let mut f = OirField::new(name.clone(), field_shape(def).map_err(|e| e.within(name.clone()))?);
            f.required = def.required.unwrap_or(false);
            Ok(f)
        })
        .collect::<Result<Vec<OirField>, ShapeError>>()?;
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(fields)
}

pub(super) fn field_shape_inner(field: &FieldDefinition) -> Result<OirType, ShapeError> {
    if matches!(field.field_type, FieldType::String | FieldType::Enum) {
        if let Some(members) = field.values.as_deref().and_then(crate::type_compat::literal_union_text) {
            return Ok(OirType::enum_type(members));
        }
    }
    Ok(match &field.field_type {
        FieldType::String => OirType::String,
        FieldType::Number => OirType::Number,
        FieldType::Int => OirType::Integer,
        FieldType::Sized(width) => width.oir_type(),
        FieldType::Boolean => OirType::Boolean,
        FieldType::Date => OirType::Date,
        FieldType::Datetime | FieldType::Timestamp => OirType::DateTime,
        FieldType::Instant => OirType::Instant,
        FieldType::Email => OirType::Email,
        FieldType::Url => OirType::Url,
        FieldType::Phone => OirType::Phone,
        FieldType::Uuid => OirType::Uuid,
        FieldType::Image => OirType::Image,
        FieldType::Money => OirType::Money,
        FieldType::File => OirType::File,
        FieldType::Bytes => OirType::Bytes,
        FieldType::CallKey => OirType::CallKey,
        FieldType::MessageKey => OirType::MessageKey,
        FieldType::Array => {
            let items = field.items.as_deref().ok_or_else(|| ShapeError::at(ShapeGap::ArrayWithoutItems))?;
            OirType::array(field_shape(items).map_err(|e| e.within("[]"))?)
        }
        FieldType::Tuple => OirType::Tuple {
            items: field
                .tuple_slots()
                .into_iter()
                .enumerate()
                .map(|(index, slot)| field_shape(slot).map_err(|e| e.within(index.to_string())))
                .collect::<Result<_, _>>()?,
        },
        FieldType::Object => {
            let additional = || -> Result<Option<Box<OirType>>, ShapeError> {
                field
                    .additional_properties
                    .as_deref()
                    .map(|extra| field_shape(extra).map(Box::new).map_err(|e| e.within("[]")))
                    .transpose()
            };
            match (&field.properties, &field.items) {
                (Some(properties), _) => OirType::Struct { fields: property_fields(properties)?.into(), additional: additional()? },
                (None, _) if field.additional_properties.is_some() => OirType::Struct { fields: crate::ir::types::no_fields(), additional: additional()? },
                (None, Some(items)) => OirType::map(OirType::String, field_shape(items).map_err(|e| e.within("[]"))?),
                (None, None) => return Err(ShapeError::at(ShapeGap::ObjectWithoutShape)),
            }
        }
        FieldType::Enum => OirType::enum_type("Enum"),
        FieldType::Relation => match &field.relation {
            Some(rel) if rel.is_to_many() && field.intrinsic == Some(true) => {
                OirType::array(OirType::row(&rel.entity))
            }
            Some(rel) if rel.is_to_many() => OirType::array(OirType::reference(&rel.entity)),
            Some(rel) if field.intrinsic == Some(true) => OirType::row(&rel.entity),
            Some(rel) => OirType::reference(&rel.entity),
            None => return Err(ShapeError::at(ShapeGap::RelationWithoutEntity)),
        },
        FieldType::Trait => OirType::Trait,
        FieldType::Orbital => OirType::Orbital,
        FieldType::Behavior => OirType::Behavior,
        FieldType::Slot | FieldType::Pattern | FieldType::Event | FieldType::EventAddress => {
            OirType::String
        }
        FieldType::Field => OirType::FieldRef {
            record: Box::new(record_shape(field.record.as_deref(), field.properties.as_ref())?),
            value: field.items.as_deref().map(|item| field_shape(item).map(Box::new).map_err(|e| e.within("[]"))).transpose()?,
        },
        FieldType::Node => OirType::Node,
        FieldType::SExpr => OirType::SExpr,
        FieldType::Union => match (field.by_name_reference(), &field.properties) {
            (Some(name), _) => OirType::TypeRef { name: name.to_string() },
            (None, Some(properties)) if !properties.is_empty() => {
                let mut variants = shapes(properties.iter().map(|(name, def)| (name.clone(), def)), field_shape)?;
                variants.sort_by(|a, b| a.name.cmp(&b.name));
                OirType::Union { variants: variants.into() }
            }
            (None, _) => return Err(ShapeError::at(ShapeGap::UnionWithoutVariants)),
        },
        FieldType::TypeVar(v) => OirType::Var { name: v.0.clone().into() },
    })
}

/// The record `R` of a `field R T`: a record alias is its `properties` as a struct; a type variable (`$r`) is a
/// variable; any other spelling (`@entity`, `@entity(<knob>)`, `@payload(<knob>)`, an entity name) is a reference
/// the consumer resolves where it knows the binding. No record at all is a [`ShapeError`].
pub fn record_shape(record: Option<&str>, properties: Option<&crate::collections::IndexMap<String, FieldDefinition>>) -> Result<OirType, ShapeError> {
    if let Some(properties) = properties.filter(|p| !p.is_empty()) {
        return Ok(OirType::Struct { fields: property_fields(properties)?.into(), additional: None });
    }
    match record {
        Some(text) => Ok(match text.strip_prefix('$') {
            Some(name) => OirType::Var { name: name.to_string().into() },
            None => OirType::Reference { entity: text.to_string() },
        }),
        None => Err(ShapeError::at(ShapeGap::FieldWithoutRecord)),
    }
}

pub fn payload_array_item_type(field_type: &str) -> Option<&str> {
    let trimmed = field_type.trim();
    if crate::type_compat::tuple_type_slots(trimmed).is_some() {
        return None;
    }
    trimmed.strip_prefix('[')?.strip_suffix(']')
}

/// The type a `.orb` payload type text names: `[X]`, `Map<K,V>`, a tuple, a primitive or semantic keyword, a
/// `$var`; any other identifier is the entity (or alias) of that name, as a `Reference`. A text with no shape
/// (empty, a bare `array`, `json`) is a [`ShapeError`].
pub fn payload_type_shape(field_type: &str) -> Result<OirType, ShapeError> {
    let trimmed = field_type.trim();
    if let Some(slots) = crate::type_compat::tuple_type_slots(trimmed) {
        return Ok(OirType::Tuple {
            items: slots
                .into_iter()
                .enumerate()
                .map(|(index, slot)| payload_type_shape(slot).map_err(|e| e.within(index.to_string())))
                .collect::<Result<_, _>>()?,
        });
    }
    if let Some(inner) = payload_array_item_type(trimmed) {
        return Ok(OirType::array(payload_type_shape(inner).map_err(|e| e.within("[]"))?));
    }
    // The key is a comma-free atom, so split on the FIRST comma (the value may itself be `Map<…>`/`[…]`).
    if let Some(inner) = trimmed.strip_prefix("Map<").and_then(|s| s.strip_suffix('>')) {
        if let Some(comma) = inner.find(',') {
            return Ok(OirType::map(
                payload_type_shape(&inner[..comma]).map_err(|e| e.within("key"))?,
                payload_type_shape(&inner[comma + 1..]).map_err(|e| e.within("[]"))?,
            ));
        }
    }
    // Builtins are matched as the language spells them; another casing is a program's own type name.
    match trimmed {
        "string" => return Ok(OirType::String),
        "number" | "float" | "double" => return Ok(OirType::Number),
        "integer" | "int" => return Ok(OirType::Integer),
        "boolean" | "bool" => return Ok(OirType::Boolean),
        "date" => return Ok(OirType::Date),
        "datetime" | "timestamp" => return Ok(OirType::DateTime),
        "instant" => return Ok(OirType::Instant),
        "email" => return Ok(OirType::Email),
        "url" => return Ok(OirType::Url),
        "phone" => return Ok(OirType::Phone),
        "uuid" => return Ok(OirType::Uuid),
        "image" => return Ok(OirType::Image),
        "money" => return Ok(OirType::Money),
        "file" => return Ok(OirType::File),
        "bytes" => return Ok(OirType::Bytes),
        "CallKey" => return Ok(OirType::CallKey),
        "MessageKey" => return Ok(OirType::MessageKey),
        sealed if crate::sealed::is_sealed_type(sealed) => return Ok(crate::sealed::sealed_type_ref(sealed)),
        // In a payload position a relation travels as the related row's id and an enum as its literal value.
        "relation" | "enum" => return Ok(OirType::String),
        "array" | "list" => return Err(ShapeError::at(ShapeGap::BareArrayText)),
        "object" => return Ok(OirType::Struct { fields: crate::ir::types::no_fields(), additional: None }),
        "json" | "tensor" => return Err(ShapeError::at(ShapeGap::GenericText(trimmed.to_string()))),
        "SExpr" => return Ok(OirType::SExpr),
        "node" => return Ok(OirType::Node),
        "orbital" => return Ok(OirType::Orbital),
        "behavior" => return Ok(OirType::Behavior),
        "trait" => return Ok(OirType::Trait),
        // UI scalar and name-reference brands travel as strings.
        "slot" | "icon" | "asset" | "component" | "pattern" | "event" => return Ok(OirType::String),
        width if crate::num_width::NumWidth::from_name(width).is_some() => {
            return Ok(crate::num_width::NumWidth::from_name(width).map_or(OirType::Number, |w| w.oir_type()))
        }
        _ => {}
    }
    // `@entity(<knob>)`: a row of the entity a knob names, and nothing names it here: an empty record.
    if trimmed.starts_with("@entity(") && trimmed.ends_with(')') {
        return Ok(OirType::Struct { fields: crate::ir::types::no_fields(), additional: None });
    }
    if let Some(name) = trimmed.strip_prefix('$').filter(|n| !n.is_empty()) {
        return Ok(OirType::Var { name: name.to_string().into() });
    }
    if trimmed.is_empty() {
        return Err(ShapeError::at(ShapeGap::EmptyTypeText));
    }
    Ok(OirType::Reference {
        entity: trimmed.to_string(),
    })
}

/// Structurally carried tuple slots in index order; the tuple's text remains the legacy primitive-only form.
pub fn payload_tuple_slots(
    field: &crate::schema::types::PayloadField,
) -> Option<Vec<&crate::schema::types::PayloadField>> {
    crate::type_compat::tuple_type_slots(&field.field_type)?;
    let mut slots = field
        .properties
        .as_ref()?
        .iter()
        .map(|slot| slot.name.parse::<usize>().ok().map(|index| (index, slot)))
        .collect::<Option<Vec<_>>>()?;
    slots.sort_by_key(|(index, _)| *index);
    Some(slots.into_iter().map(|(_, slot)| slot).collect())
}

/// The type a `.orb` payload field declares, read structurally: a by-name back-reference (`properties: []`) is the
/// `TypeRef` it names, `properties` are a union's variants or a record's members (`[object]` a list of it), `items`
/// a list's or map's element; an alias or entity name stays the `Reference` [`payload_type_shape`] makes of it.
pub fn payload_field_shape(field: &crate::schema::types::PayloadField) -> Result<OirType, ShapeError> {
    payload_field_shape_with_entity(field, &|_| false)
}

pub fn payload_field_shape_with_entity(
    field: &crate::schema::types::PayloadField,
    is_entity: &impl Fn(&str) -> bool,
) -> Result<OirType, ShapeError> {
    payload_shape(field, &PayloadNames { entity: is_entity, alias: &|_| false, sorted: false })
}

/// [`payload_field_shape_with_entity`] for codegen: a field projected from a declared alias (`projectedFrom.type`,
/// whole type, `is_alias` true) is the `TypeRef` naming that alias, so a shell hoists one model per declared name, and
/// a record's fields are in name order (the canonical order every generated record model and literal shares; union
/// arms keep their declared order, which arm selection reads).
pub fn payload_field_shape_named(
    field: &crate::schema::types::PayloadField,
    is_entity: &impl Fn(&str) -> bool,
    is_alias: &impl Fn(&str) -> bool,
) -> Result<OirType, ShapeError> {
    payload_shape(field, &PayloadNames { entity: is_entity, alias: is_alias, sorted: true })
}

/// [`declared_alias_shape`] whose members reference other declared aliases by name (see
/// [`payload_field_shape_named`]); the alias's own top-level projection is its definition, not a reference.
pub fn declared_alias_shape_named(
    members: &[crate::schema::types::PayloadField],
    is_alias: &impl Fn(&str) -> bool,
) -> Result<OirType, ShapeError> {
    let names = PayloadNames { entity: &|_| false, alias: is_alias, sorted: true };
    match members {
        [only] if only.name.is_empty() => {
            let mut body = only.clone();
            body.projected_from = None;
            payload_shape(&body, &names)
        }
        _ => Ok(OirType::Struct { additional: None, fields: record_members(members, &names)?.into() }),
    }
}

struct PayloadNames<'a> {
    entity: &'a dyn Fn(&str) -> bool,
    alias: &'a dyn Fn(&str) -> bool,
    sorted: bool,
}

/// A record's members, in name order when `names.sorted`.
fn record_members(props: &[crate::schema::types::PayloadField], names: &PayloadNames<'_>) -> Result<Vec<OirField>, ShapeError> {
    let mut fields = payload_members(props, names)?;
    if names.sorted {
        fields.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(fields)
}

fn payload_shape(field: &crate::schema::types::PayloadField, names: &PayloadNames<'_>) -> Result<OirType, ShapeError> {
    let shape = payload_field_shape_inner(field, names)?;
    Ok(if field.nullable == Some(true) { OirType::optional(shape) } else { shape })
}

fn payload_member(
    field: &crate::schema::types::PayloadField,
    names: &PayloadNames<'_>,
    segment: impl Into<String>,
) -> Result<OirType, ShapeError> {
    payload_shape(field, names).map_err(|e| e.within(segment))
}

fn payload_members(
    props: &[crate::schema::types::PayloadField],
    names: &PayloadNames<'_>,
) -> Result<Vec<OirField>, ShapeError> {
    props
        .iter()
        .map(|p| {
            let mut f = OirField::new(p.name.clone(), payload_member(p, names, p.name.clone())?);
            f.required = p.required.unwrap_or(false);
            f.description = p.description.clone();
            Ok(f)
        })
        .collect()
}

fn payload_field_shape_inner(
    field: &crate::schema::types::PayloadField,
    names: &PayloadNames<'_>,
) -> Result<OirType, ShapeError> {
    let is_entity = names.entity;
    if let Some(alias) = field.projected_from.as_ref().filter(|p| p.field.is_none()).map(|p| p.type_name.as_str()).filter(|n| (names.alias)(n)) {
        let named = OirType::TypeRef { name: alias.to_string() };
        return Ok(if field.field_type.trim_start().starts_with('[') { OirType::array(named) } else { named });
    }
    if field.field_type != "union" {
        // A payload field typed by an entity carries that entity's row; a relation field of a row carries its id.
        if let Some(entity) = field.entity.as_deref().filter(|entity| is_entity(entity)) {
            let row = OirType::row(entity);
            return Ok(if field.field_type.trim_start().starts_with('[') {
                OirType::array(row)
            } else {
                row
            });
        }
    }
    if let Some(slots) = payload_tuple_slots(field) {
        return Ok(OirType::Tuple {
            items: slots
                .into_iter()
                .enumerate()
                .map(|(index, field)| payload_member(field, names, index.to_string()))
                .collect::<Result<_, _>>()?,
        });
    }
    if let Some(item) = field.items.as_deref() {
        if matches!(field.field_type.trim(), "array" | "list") {
            return Ok(OirType::array(payload_member(item, names, "[]")?));
        }
        match payload_type_shape(&field.field_type) {
            Ok(OirType::Array { .. }) => return Ok(OirType::array(payload_member(item, names, "[]")?)),
            Ok(OirType::Map { key, .. }) => return Ok(OirType::map(*key, payload_member(item, names, "[]")?)),
            _ => {}
        }
    }
    if let (true, None, Some(item)) = (field.field_type == "object" && field.additional_properties.is_none(), &field.properties, field.items.as_deref()) {
        return Ok(OirType::map(OirType::String, payload_member(item, names, "[]")?));
    }
    let additional = || -> Result<Option<Box<OirType>>, ShapeError> {
        field
            .additional_properties
            .as_deref()
            .map(|extra| payload_member(extra, names, "[]").map(Box::new))
            .transpose()
    };
    if field.field_type == "object" && field.properties.as_ref().is_none_or(Vec::is_empty) && field.additional_properties.is_some() {
        return Ok(OirType::Struct { fields: crate::ir::types::no_fields(), additional: additional()? });
    }
    let list = field.field_type.trim_start().starts_with('[');
    match &field.properties {
        Some(props) if props.is_empty() => {
            let trimmed = field.field_type.trim();
            let name = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(trimmed);
            let named = OirType::TypeRef { name: name.to_string() };
            Ok(if list { OirType::array(named) } else { named })
        }
        Some(props) => {
            if field.field_type == "union" {
                return Ok(OirType::Union { variants: payload_members(props, names)?.into() });
            }
            let fields = record_members(props, names)?;
            let record = OirType::Struct { fields: fields.into(), additional: additional()? };
            Ok(if list { OirType::array(record) } else { record })
        }
        None => match field.values.as_deref().and_then(crate::type_compat::literal_union_text) {
            Some(members) if field.field_type == "string" || field.field_type == "enum" => {
                Ok(OirType::Enum { name: members })
            }
            _ => payload_type_shape(&field.field_type),
        },
    }
}

/// A declared type as `orb resolve` records it where the kernel checks a value at run time (a `json/parse` shape, a
/// `typed` declaration): the type, with the definition of every program type it names, transitively, so a recursive
/// alias stays finite; `name` is the program's name for it, when it has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredShape {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_type_contract_version")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub ty: OirType,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub named: std::collections::BTreeMap<String, OirType>,
}

pub(crate) fn deserialize_type_contract_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let version = Option::<u32>::deserialize(deserializer)?;
    if version.is_some_and(|version| version != 1) {
        return Err(serde::de::Error::custom("unsupported declared-shape version"));
    }
    Ok(version)
}

impl DeclaredShape {
    /// `ty` with the definitions, from the program's `aliases`, of every alias it names, transitively.
    pub fn with_named(
        name: Option<String>,
        ty: OirType,
        aliases: &std::collections::BTreeMap<String, Vec<crate::schema::types::PayloadField>>,
    ) -> Result<Self, ShapeError> {
        Self::with_resolver(name, ty, |next| aliases.get(next).map(|members| declared_alias_shape(members)).transpose())
    }

    /// Close a declared type over exactly the named definitions its shape reaches.
    pub fn with_resolver(
        name: Option<String>,
        ty: OirType,
        resolve: impl Fn(&str) -> Result<Option<OirType>, ShapeError>,
    ) -> Result<Self, ShapeError> {
        let mut named = std::collections::BTreeMap::new();
        let mut pending: Vec<String> = Vec::new();
        names_in(&ty, &mut pending);
        while let Some(next) = pending.pop() {
            if named.contains_key(&next) {
                continue;
            }
            let Some(shape) = resolve(&next)? else {
                continue;
            };
            names_in(&shape, &mut pending);
            named.insert(next, shape);
        }
        Ok(Self { version: None, name, ty, named })
    }

    /// The program alias `name` as a declared shape, when `aliases` declares it.
    pub fn of_alias(
        name: &str,
        aliases: &std::collections::BTreeMap<String, Vec<crate::schema::types::PayloadField>>,
    ) -> Result<Option<Self>, ShapeError> {
        let Some(members) = aliases.get(name) else { return Ok(None) };
        Self::with_named(Some(name.to_string()), declared_alias_shape(members)?, aliases).map(Some)
    }

    /// The declared shape a recorded text writes: this form, or a bare `OirType` (no named types); `None` for text
    /// that is neither (a type name).
    pub fn parse(text: &str) -> Option<Self> {
        Self::read(text).ok()
    }

    /// Decode one supported shape, preserving its diagnostic and legacy bare-type form.
    pub fn read(text: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        if value.get("version").is_some() || value.get("type").is_some() && value.get("kind").is_none() {
            return serde_json::from_value(value);
        }
        let ty: OirType = serde_json::from_value(value)?;
        Ok(Self { version: None, name: None, ty, named: Default::default() })
    }

    /// The program's name for the shape a recorded text writes: the shape's `name`, or the text itself (a type name).
    pub fn name_of(text: &str) -> std::borrow::Cow<'_, str> {
        match Self::parse(text).and_then(|shape| shape.name) {
            Some(name) => std::borrow::Cow::Owned(name),
            None => std::borrow::Cow::Borrowed(text),
        }
    }
}

/// Every name `ty` refers to by name (`TypeRef`, `Reference`).
pub fn names_in(ty: &OirType, out: &mut Vec<String>) {
    match ty {
        OirType::TypeRef { name } | OirType::Reference { entity: name } => out.push(name.clone()),
        OirType::Array { inner } | OirType::Optional { inner } => names_in(inner, out),
        OirType::Map { key, value } => {
            names_in(key, out);
            names_in(value, out);
        }
        OirType::Tuple { items } => items.iter().for_each(|item| names_in(item, out)),
        OirType::Struct { fields, additional } => {
            fields.iter().for_each(|f| names_in(&f.field_type, out));
            if let Some(additional) = additional { names_in(additional, out); }
        }
        OirType::Union { variants } => variants.iter().for_each(|v| names_in(&v.field_type, out)),
        _ => {}
    }
}

/// The type a program's `type X = …` alias is, from its `.orb` entry (`orbital.types[X]`): one nameless member is
/// the aliased type itself, any other member list is a record.
pub fn declared_alias_shape(members: &[crate::schema::types::PayloadField]) -> Result<OirType, ShapeError> {
    match members {
        [only] if only.name.is_empty() => payload_field_shape(only),
        _ => Ok(OirType::Struct { additional: None, fields: payload_members(members, &PayloadNames { entity: &|_| false, alias: &|_| false, sorted: false })?.into() }),
    }
}
