//! Field, record and payload shapes derived from declared types.

use super::*;

/// Structural `FieldDefinition` → `OirType`, needing no resolver context.
///
/// An `Object` is its `properties` as a struct, or a `Map` of its `items`; a `Union` that is a by-name
/// back-reference keeps the type it names ([`FieldDefinition::by_name_reference`]). Named-type registration is
/// the resolver's, not this function's. `Relation` builds a real
/// `Reference` when it has one: `sub`'s `Reference` arm, not `Json`'s, is
/// what accepts a row or an id string.
pub fn field_shape(field: &FieldDefinition) -> OirType {
    let shape = field_shape_inner(field);
    if field.nullable == Some(true) { OirType::optional(shape) } else { shape }
}

pub(super) fn field_shape_inner(field: &FieldDefinition) -> OirType {
    if matches!(field.field_type, FieldType::String | FieldType::Enum) {
        if let Some(members) = field.values.as_deref().and_then(crate::type_compat::literal_union_text) {
            return OirType::enum_type(members);
        }
    }
    match &field.field_type {
        FieldType::String => OirType::String,
        FieldType::Number => OirType::Number,
        FieldType::Int => OirType::Integer,
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
        FieldType::Array => {
            let inner = field
                .items
                .as_deref()
                .map(field_shape)
                .unwrap_or(OirType::Json);
            OirType::array(inner)
        }
        FieldType::Tuple => OirType::Tuple {
            items: field.tuple_slots().into_iter().map(field_shape).collect(),
        },
        FieldType::Object => match (&field.properties, &field.items) {
            (Some(properties), _) => {
                let mut fields: Vec<OirField> = properties
                    .iter()
                    .map(|(name, def)| {
                        let mut f = OirField::new(name.clone(), field_shape(def));
                        f.required = def.required.unwrap_or(false);
                        f
                    })
                    .collect();
                fields.sort_by(|a, b| a.name.cmp(&b.name));
                OirType::Struct { fields: fields.into(), additional: field.additional_properties.as_deref().map(field_shape).map(Box::new) }
            }
            (None, _) if field.additional_properties.is_some() => OirType::Struct { fields: crate::ir::types::no_fields(), additional: field.additional_properties.as_deref().map(field_shape).map(Box::new) },
            (None, Some(items)) => OirType::map(OirType::String, field_shape(items)),
            (None, None) => OirType::Json,
        },
        FieldType::Enum => OirType::enum_type("Enum"),
        FieldType::Relation => match &field.relation {
            Some(rel) if rel.is_to_many() && field.intrinsic == Some(true) => {
                OirType::array(OirType::row(&rel.entity))
            }
            Some(rel) if rel.is_to_many() => OirType::array(OirType::reference(&rel.entity)),
            Some(rel) if field.intrinsic == Some(true) => OirType::row(&rel.entity),
            Some(rel) => OirType::reference(&rel.entity),
            None => OirType::Json,
        },
        FieldType::Trait => OirType::Trait,
        FieldType::Orbital => OirType::Orbital,
        FieldType::Behavior => OirType::Behavior,
        FieldType::Slot | FieldType::Pattern | FieldType::Event | FieldType::EventAddress => {
            OirType::String
        }
        FieldType::Field => OirType::FieldRef {
            record: Box::new(record_shape(field.record.as_deref(), field.properties.as_ref())),
            value: field.items.as_deref().map(|item| Box::new(field_shape(item))),
        },
        FieldType::Node => OirType::Node,
        FieldType::SExpr => OirType::SExpr,
        FieldType::Union => match (field.by_name_reference(), &field.properties) {
            (Some(name), _) => OirType::TypeRef { name: name.to_string() },
            (None, Some(properties)) if !properties.is_empty() => {
                let mut variants: Vec<OirField> = properties
                    .iter()
                    .map(|(name, def)| OirField::new(name.clone(), field_shape(def)))
                    .collect();
                variants.sort_by(|a, b| a.name.cmp(&b.name));
                OirType::Union { variants: variants.into() }
            }
            (None, _) => OirType::Json,
        },
        FieldType::TypeVar(v) => OirType::Var { name: v.0.clone() },
    }
}

/// The record `R` of a `field R T`: a record alias is its `properties` as a struct; a type variable (`$r`) is a
/// variable; any other spelling (`@entity`, `@entity(<knob>)`, `@payload(<knob>)`, an entity name) is a reference
/// the consumer resolves where it knows the binding.
pub fn record_shape(record: Option<&str>, properties: Option<&HashMap<String, FieldDefinition>>) -> OirType {
    if let Some(properties) = properties.filter(|p| !p.is_empty()) {
        let mut fields: Vec<OirField> = properties
            .iter()
            .map(|(name, def)| {
                let mut f = OirField::new(name.clone(), field_shape(def));
                f.required = def.required.unwrap_or(false);
                f
            })
            .collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        return OirType::Struct { fields: fields.into(), additional: None };
    }
    match record {
        Some(text) => match text.strip_prefix('$') {
            Some(name) => OirType::Var { name: name.to_string() },
            None => OirType::Reference { entity: text.to_string() },
        },
        None => OirType::Json,
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
/// `$var`; any other identifier is the entity (or alias) of that name, as a `Reference`.
pub fn payload_type_shape(field_type: &str) -> OirType {
    let trimmed = field_type.trim();
    if let Some(slots) = crate::type_compat::tuple_type_slots(trimmed) {
        return OirType::Tuple {
            items: slots.into_iter().map(payload_type_shape).collect(),
        };
    }
    if let Some(inner) = payload_array_item_type(trimmed) {
        return OirType::array(payload_type_shape(inner));
    }
    // The key is a comma-free atom, so split on the FIRST comma (the value may itself be `Map<…>`/`[…]`).
    if let Some(inner) = trimmed.strip_prefix("Map<").and_then(|s| s.strip_suffix('>')) {
        if let Some(comma) = inner.find(',') {
            return OirType::map(payload_type_shape(&inner[..comma]), payload_type_shape(&inner[comma + 1..]));
        }
    }
    // Builtins are matched as the language spells them; another casing is a program's own type name.
    match trimmed {
        "string" => return OirType::String,
        "number" | "float" | "double" => return OirType::Number,
        "integer" | "int" => return OirType::Integer,
        "boolean" | "bool" => return OirType::Boolean,
        "date" => return OirType::Date,
        "datetime" | "timestamp" => return OirType::DateTime,
        "instant" => return OirType::Instant,
        "email" => return OirType::Email,
        "url" => return OirType::Url,
        "phone" => return OirType::Phone,
        "uuid" => return OirType::Uuid,
        "image" => return OirType::Image,
        "money" => return OirType::Money,
        "file" => return OirType::File,
        "bytes" => return OirType::Bytes,
        "CallKey" => return OirType::CallKey,
        // In a payload position a relation travels as the related row's id and an enum as its literal value.
        "relation" | "enum" => return OirType::String,
        "array" | "list" => return OirType::array(OirType::Json),
        "object" => return OirType::Struct { fields: crate::ir::types::no_fields(), additional: None },
        "json" | "tensor" => return OirType::Json,
        "SExpr" => return OirType::SExpr,
        "node" => return OirType::Node,
        "orbital" => return OirType::Orbital,
        "behavior" => return OirType::Behavior,
        "trait" => return OirType::Trait,
        // UI scalar and name-reference brands travel as strings.
        "slot" | "icon" | "asset" | "component" | "pattern" | "event" => return OirType::String,
        _ => {}
    }
    // `@entity(<knob>)`: a row of the entity a knob names, and nothing names it here: an empty record.
    if trimmed.starts_with("@entity(") && trimmed.ends_with(')') {
        return OirType::Struct { fields: crate::ir::types::no_fields(), additional: None };
    }
    if let Some(name) = trimmed.strip_prefix('$').filter(|n| !n.is_empty()) {
        return OirType::Var { name: name.to_string() };
    }
    if trimmed.is_empty() {
        return OirType::Json;
    }
    OirType::Reference {
        entity: trimmed.to_string(),
    }
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
pub fn payload_field_shape(field: &crate::schema::types::PayloadField) -> OirType {
    payload_field_shape_with_entity(field, &|_| false)
}

pub fn payload_field_shape_with_entity(
    field: &crate::schema::types::PayloadField,
    is_entity: &impl Fn(&str) -> bool,
) -> OirType {
    let shape = payload_field_shape_inner(field, is_entity);
    if field.nullable == Some(true) { OirType::optional(shape) } else { shape }
}

pub(super) fn payload_field_shape_inner(
    field: &crate::schema::types::PayloadField,
    is_entity: &impl Fn(&str) -> bool,
) -> OirType {
    if field.field_type != "union" {
        // A payload field typed by an entity carries that entity's row; a relation field of a row carries its id.
        if let Some(entity) = field.entity.as_deref().filter(|entity| is_entity(entity)) {
            let row = OirType::row(entity);
            return if field.field_type.trim_start().starts_with('[') {
                OirType::array(row)
            } else {
                row
            };
        }
    }
    if let Some(slots) = payload_tuple_slots(field) {
        return OirType::Tuple { items: slots.into_iter().map(|field| payload_field_shape_with_entity(field, is_entity)).collect() };
    }
    if let Some(item) = field.items.as_deref() {
        match payload_type_shape(&field.field_type) {
            OirType::Array { .. } => return OirType::array(payload_field_shape_with_entity(item, is_entity)),
            OirType::Map { key, .. } => return OirType::map(*key, payload_field_shape_with_entity(item, is_entity)),
            _ => {}
        }
    }
    if let (true, None, Some(item)) = (field.field_type == "object" && field.additional_properties.is_none(), &field.properties, field.items.as_deref()) {
        return OirType::map(OirType::String, payload_field_shape_with_entity(item, is_entity));
    }
    if field.field_type == "object" && field.properties.as_ref().is_none_or(Vec::is_empty) && field.additional_properties.is_some() {
        return OirType::Struct { fields: crate::ir::types::no_fields(), additional: field.additional_properties.as_deref().map(|field| payload_field_shape_with_entity(field, is_entity)).map(Box::new) };
    }
    let list = field.field_type.trim_start().starts_with('[');
    match &field.properties {
        Some(props) if props.is_empty() => {
            let trimmed = field.field_type.trim();
            let name = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(trimmed);
            let named = OirType::TypeRef { name: name.to_string() };
            if list {
                OirType::array(named)
            } else {
                named
            }
        }
        Some(props) => {
            let fields: Vec<OirField> = props
                .iter()
                .map(|p| {
                    let mut f = OirField::new(p.name.clone(), payload_field_shape_with_entity(p, is_entity));
                    f.required = p.required.unwrap_or(false);
                    f.description = p.description.clone();
                    f
                })
                .collect();
            if field.field_type == "union" {
                return OirType::Union { variants: fields.into() };
            }
            let record = OirType::Struct { fields: fields.into(), additional: field.additional_properties.as_deref().map(|field| payload_field_shape_with_entity(field, is_entity)).map(Box::new) };
            if list {
                OirType::array(record)
            } else {
                record
            }
        }
        None => match field.values.as_deref().and_then(crate::type_compat::literal_union_text) {
            Some(members) if field.field_type == "string" || field.field_type == "enum" => {
                OirType::Enum { name: members }
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
    ) -> Self {
        Self::with_resolver(name, ty, |next| {
            aliases.get(next).map(|members| declared_alias_shape(members))
        })
    }

    /// Close a declared type over exactly the named definitions its shape reaches.
    pub fn with_resolver(
        name: Option<String>,
        ty: OirType,
        resolve: impl Fn(&str) -> Option<OirType>,
    ) -> Self {
        let mut named = std::collections::BTreeMap::new();
        let mut pending: Vec<String> = Vec::new();
        names_in(&ty, &mut pending);
        while let Some(next) = pending.pop() {
            if named.contains_key(&next) {
                continue;
            }
            let Some(shape) = resolve(&next) else {
                continue;
            };
            names_in(&shape, &mut pending);
            named.insert(next, shape);
        }
        Self { version: None, name, ty, named }
    }

    /// The program alias `name` as a declared shape, when `aliases` declares it.
    pub fn of_alias(
        name: &str,
        aliases: &std::collections::BTreeMap<String, Vec<crate::schema::types::PayloadField>>,
    ) -> Option<Self> {
        let members = aliases.get(name)?;
        Some(Self::with_named(Some(name.to_string()), declared_alias_shape(members), aliases))
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
pub fn declared_alias_shape(members: &[crate::schema::types::PayloadField]) -> OirType {
    match members {
        [only] if only.name.is_empty() => payload_field_shape(only),
        _ => OirType::Struct {
            additional: None,
            fields: members
                .iter()
                .map(|p| {
                    let mut f = OirField::new(p.name.clone(), payload_field_shape(p));
                    f.required = p.required.unwrap_or(false);
                    f.description = p.description.clone();
                    f
                })
                .collect(),
        },
    }
}
