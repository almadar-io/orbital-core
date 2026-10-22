//! Pattern registry — the single embed of the pattern→component / event-contract
//! / props-schema data the codegen shells consume.
//!
//! The three JSON files used to be baked byte-identically into EACH shell's
//! `src/` (orbital-shell-typescript, orbital-shell-python) plus a third copy of
//! `component-mapping.json` under `phases/validation/sync/`. pattern-sync wrote
//! them to 6 destinations, so a partial sync drifted codegen and the validator
//! against each other silently. They now live ONCE here and every consumer
//! imports this module (the shells re-export it; the validator reads the
//! component names from `component_names()`). `registry.json` is the same data
//! as the patterns registry — reused, not re-read. All of it comes from the
//! installed `@almadar/core` (`crate::language_data`), not the binary.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

fn language() -> &'static crate::language_data::LanguageData {
    crate::language_data::language_data_after_preflight()
}

/// Component mapping for a pattern.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentMapping {
    pub component: String,
    pub import_path: String,
    pub category: String,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub replaced_by: Option<String>,
    /// Client identifier (e.g. "trait-wars") for game-specific patterns.
    #[serde(default)]
    pub client: Option<String>,
}

/// One field of an emit/listen payload schema mirrored into the registry by
/// pattern-sync from the component's `EventEmit<P>` / `EventListen<P>` brand.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryPayloadField {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
}

/// One positional parameter of a React callback prop.
#[derive(Debug, Clone, Deserialize)]
pub struct CallbackArg {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub arg_type: String,
}

/// Recursive type schema for array elements and nested objects. Mirrors
/// `PropTypeSchema` in `tools/almadar-pattern-sync/types.ts`. Lets codegen
/// branch on element shape (string vs object) without name-based heuristics.
#[derive(Debug, Clone, Deserialize)]
pub struct PropTypeSchema {
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(rename = "enumValues", default)]
    pub enum_values: Option<Vec<String>>,
    #[serde(default)]
    pub items: Option<Box<PropTypeSchema>>,
    #[serde(default)]
    pub properties: Option<HashMap<String, PropTypeSchema>>,
    #[serde(default)]
    pub required: Option<Vec<String>>,
    /// Marks a recursive element reference (e.g. menu `subMenu` items are the
    /// parent item shape). A structural literal can't express the recursion —
    /// synthesized shapes emit `never` here, which stays assignable to the
    /// component's named recursive type.
    #[serde(default)]
    pub cyclic: bool,
    /// A member typed `T | null` without `?`: `.orb` data omits it, the TS type needs `null`.
    #[serde(default)]
    pub nullable: bool,
    /// Elements are drawable descriptors (`draw-*` data the painter reads), not rendered children.
    #[serde(rename = "drawableSlot", default)]
    pub drawable_slot: bool,
}

/// A rich prop's typed decoder (see `PropSchema::decoder`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PropDecoder {
    pub name: String,
    pub from: String,
    #[serde(rename = "acceptsString", default)]
    pub accepts_string: bool,
}

/// Prop type definition.
#[derive(Debug, Clone, Deserialize)]
pub struct PropSchema {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// F1 marker — see `embedded::EmbeddedPatternProp::kind`.
    #[serde(default)]
    pub kind: Option<String>,
    /// For `kind: "callback"`: positional parameter list. C2 uses the names to
    /// build a named-arg → object-payload wrapper at the dispatch site.
    #[serde(rename = "callbackArgs", default)]
    pub callback_args: Option<Vec<CallbackArg>>,
    /// Numeric literal-union props (`cols: 1 | 2 | … | 6`). The TS codegen
    /// asserts a `Number(...)` coercion down to this union so the component
    /// prop typechecks; validation mirrors the string `enumValues` rule.
    #[serde(rename = "numericEnumValues", default)]
    pub numeric_enum_values: Option<Vec<f64>>,
    /// For `kind: "event-ref"` from an `EventEmit<P>` brand.
    #[serde(rename = "emitPayloadSchema", default)]
    pub emit_payload_schema: Option<Vec<RegistryPayloadField>>,
    /// Element shape for array-typed props (mirrors JSON Schema `items`).
    #[serde(default)]
    pub items: Option<PropTypeSchema>,
    /// Per-key schemas for object-typed props sourced from a declared interface.
    #[serde(default)]
    pub properties: Option<HashMap<String, PropTypeSchema>>,
    /// Required keys for object-typed props.
    #[serde(rename = "propertyRequired", default)]
    pub property_required: Option<Vec<String>>,
    /// For entity-data props: `"record"` (single entity) or `"collection"`
    /// (array). Drives the bind-site cast cardinality.
    #[serde(default)]
    pub cardinality: Option<String>,
    /// The owning package's typed decoder a compiled app applies to a value reaching this prop
    /// (G-CROSS-131): `{ name, from, acceptsString? }`.
    #[serde(default)]
    pub decoder: Option<PropDecoder>,
    /// For entity-data props declared as `EntityWith<'a' | 'b'>`: the field
    /// names the component requires.
    #[serde(rename = "requiredFields", default)]
    pub required_fields: Option<Vec<String>>,
    /// The prop's declared default (from the component's TS default / `@default`).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

/// Pattern definition from registry.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryPattern {
    #[serde(rename = "type")]
    pub pattern_type: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub props_schema: HashMap<String, PropSchema>,
    /// Declared fields-consumption contract from the component's
    /// `@fieldsContract` JSDoc tag: `"form"` = editable SchemaFields
    /// (schema enrichment + relationsData), `"display"` = read-only
    /// display fields ({key, header} + typed meta). THE capability the
    /// codegen field-enrichment gates key on — never a pattern-name list.
    #[serde(default)]
    pub fields_contract: Option<String>,
    /// Props that give the pattern its accessible name, from the component's
    /// `@accessibleName` JSDoc tag. `ORB_A11Y_UNNAMED` requires one of them
    /// (or `aria-label` / `aria-labelledby`) on every instance.
    #[serde(default)]
    pub accessible_name: Option<Vec<String>>,
}

/// Event payload schema.
#[derive(Debug, Clone, Deserialize)]
pub struct PayloadSchema {
    #[serde(rename = "type")]
    pub payload_type: String,
    #[serde(default)]
    pub shape: Option<HashMap<String, String>>,
}

/// Event emitted by a component.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEvent {
    pub event: String,
    pub trigger: String,
    pub payload: PayloadSchema,
    #[serde(default)]
    pub optional: bool,
}

/// Event contract for a pattern.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventContract {
    pub emits: Vec<RegistryEvent>,
    pub requires: Vec<String>,
    #[serde(default)]
    pub entity_aware: bool,
    #[serde(default)]
    pub config_driven: bool,
}

#[derive(Debug, Deserialize)]
struct ComponentMappingFile {
    mappings: HashMap<String, ComponentMapping>,
}

#[derive(Debug, Deserialize)]
struct EventContractsFile {
    contracts: HashMap<String, EventContract>,
}

#[derive(Debug, Deserialize)]
struct PatternRegistryFile {
    patterns: HashMap<String, RegistryPattern>,
}

fn component_mappings() -> &'static HashMap<String, ComponentMapping> {
    static PARSED: OnceLock<HashMap<String, ComponentMapping>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let file: ComponentMappingFile =
            serde_json::from_str(&language().component_mapping)
                .expect("installed component-mapping.json is corrupt — reinstall @almadar/core");
        file.mappings
    })
}

fn event_contracts() -> &'static HashMap<String, EventContract> {
    static PARSED: OnceLock<HashMap<String, EventContract>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let file: EventContractsFile =
            serde_json::from_str(&language().event_contracts)
                .expect("installed event-contracts.json is corrupt — reinstall @almadar/core");
        file.contracts
    })
}

fn pattern_definitions() -> &'static HashMap<String, RegistryPattern> {
    static PARSED: OnceLock<HashMap<String, RegistryPattern>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let file: PatternRegistryFile =
            serde_json::from_str(&language().patterns)
                .expect("installed patterns-registry.json is corrupt — reinstall @almadar/core");
        file.patterns
    })
}

/// Get component mapping for a pattern type.
pub fn get_component_mapping(pattern_type: &str) -> Option<&'static ComponentMapping> {
    component_mappings().get(pattern_type)
}

/// Get event contract for a pattern type.
pub fn get_event_contract(pattern_type: &str) -> Option<&'static EventContract> {
    event_contracts().get(pattern_type)
}

/// Get pattern definition (includes props schema).
pub fn get_pattern_definition(pattern_type: &str) -> Option<&'static RegistryPattern> {
    pattern_definitions().get(pattern_type)
}

/// Check if a pattern type is registered.
/// The pattern's declared `@fieldsContract` (`"form"` | `"display"`), or
/// None for patterns without entity-bound field lists. See
/// [`RegistryPattern::fields_contract`].
pub fn get_fields_contract(pattern_type: &str) -> Option<&'static str> {
    get_pattern_definition(pattern_type)?
        .fields_contract
        .as_deref()
}

/// The pattern's declared accessible-name props (`@accessibleName`), or None
/// when the pattern needs no name of its own.
pub fn get_accessible_name(pattern_type: &str) -> Option<&'static [String]> {
    get_pattern_definition(pattern_type)?
        .accessible_name
        .as_deref()
}

pub fn is_registered_pattern(pattern_type: &str) -> bool {
    component_mappings().contains_key(pattern_type)
}

/// Get all registered pattern types.
pub fn get_all_patterns() -> Vec<&'static str> {
    component_mappings().keys().map(|s| s.as_str()).collect()
}

/// True when the pattern binds exactly one record: its `entity` prop declares
/// `cardinality: "record"` (e.g. detail-panel). Declared registry data, never
/// a pattern-name list.
pub fn binds_single_record(pattern_type: &str) -> bool {
    get_props_schema(pattern_type).is_some_and(|props| {
        props.values().any(|p| {
            p.kind.as_deref() == Some("entity") && p.cardinality.as_deref() == Some("record")
        })
    })
}

/// Get the props schema for a pattern type.
pub fn get_props_schema(pattern_type: &str) -> Option<&'static HashMap<String, PropSchema>> {
    pattern_definitions()
        .get(pattern_type)
        .map(|p| &p.props_schema)
}

/// All component identifiers in the mapping (e.g. `StatCard`, `EntityTable`).
/// The validator uses this to reserve names that would collide with generated
/// entity types — replacing its former private read of `component-mapping.json`.
pub fn component_names() -> impl Iterator<Item = &'static str> {
    component_mappings().values().map(|m| m.component.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_component_mappings() {
        assert!(get_component_mapping("stat-card").is_some());
        assert!(get_component_mapping("table-view").is_some());
        assert!(get_component_mapping("form-section").is_some());
    }

    #[test]
    fn loads_event_contracts() {
        let form = get_event_contract("form").expect("form contract");
        assert!(form.requires.contains(&"SAVE".to_string()));
    }

    #[test]
    fn stat_card_mapping() {
        let mapping = get_component_mapping("stat-card").unwrap();
        assert_eq!(mapping.component, "StatCard");
        assert_eq!(mapping.category, "display");
    }

    #[test]
    fn loads_pattern_definitions() {
        let stats = get_pattern_definition("stat-card").unwrap();
        assert_eq!(stats.category, "display");
        assert!(stats.props_schema.contains_key("label"));
        assert!(stats.props_schema.contains_key("value"));
    }

    #[test]
    fn props_schema_and_component_names() {
        let schema = get_props_schema("table-view").unwrap();
        assert!(schema.contains_key("columns"));
        assert!(schema.contains_key("entity"));
        assert!(component_names().any(|n| n == "StatCard"));
    }
}
