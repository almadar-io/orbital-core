//! Entities, their fields and persistence, and the relations between them.

use super::*;

/// Entity definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEntity {
    pub name: String,
    /// V4 dual-carry node id — the durable join key. Optional until the Phase-7
    /// flip; when present, `OirModule::resolve_entity_ref` joins by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Collection/table name for persistence
    pub collection: String,
    /// Persistence mode
    pub persistence: OirPersistence,
    /// Field definitions
    pub fields: Vec<OirField>,
    /// Default values for new instances
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defaults: Option<OirObject>,
    /// Pre-authored instances (seed data or static reference data)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<OirObject>,
    /// Cross-references (populated during resolution)
    #[serde(default)]
    pub used_by_traits: Vec<String>,
    #[serde(default)]
    pub used_by_pages: Vec<String>,

    /// Shared across all bound traits (vs per-trait copy); orthogonal to persistence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,

    /// `[identity]` — types the ambient `@user` viewer. See
    /// `schema::EntityDefinition::identity`. Carried into OIR so codegen can name
    /// the collection a generated app's persona roster is read from.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity: bool,

    /// `[persistent: x, browser]` — rows live in the browser. See
    /// `schema::EntityDefinition::browser`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,

    /// `@read` — see `schema::EntityDefinition::read_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_policy: Option<crate::ir::expr::OirExpr>,
    /// `@create` — see `schema::EntityDefinition::create_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_policy: Option<crate::ir::expr::OirExpr>,
    /// `@update` — see `schema::EntityDefinition::update_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<crate::ir::expr::OirExpr>,
    /// `@delete` — see `schema::EntityDefinition::delete_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_policy: Option<crate::ir::expr::OirExpr>,
}

impl OirEntity {
    /// Rows live in the client: `[runtime]` or `[persistent: x, browser]`.
    pub fn is_client_resident(&self) -> bool {
        self.browser || self.persistence == OirPersistence::Runtime
    }

    /// Create a new entity
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let collection = to_snake_case(&name);
        Self {
            name,
            id: None,
            description: None,
            collection,
            persistence: OirPersistence::Persistent,
            fields: vec![],
            defaults: None,
            instances: vec![],
            used_by_traits: vec![],
            used_by_pages: vec![],
            shared: false,
            identity: false,
            browser: false,
            read_policy: None,
            create_policy: None,
            update_policy: None,
            delete_policy: None,
        }
    }

    /// Whether this entity carries any of the four access directives.
    ///
    /// One declared directive is a declaration: an author who wrote `@read` and
    /// no `@create` chose an unrestricted create, and must not silently pick up
    /// a sibling's.
    pub fn declares_access_policy(&self) -> bool {
        self.read_policy.is_some()
            || self.create_policy.is_some()
            || self.update_policy.is_some()
            || self.delete_policy.is_some()
    }

    /// Find a field by name
    pub fn find_field(&self, name: &str) -> Option<&OirField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Check if entity has a field
    pub fn has_field(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name)
    }

    /// Get all required fields
    pub fn required_fields(&self) -> impl Iterator<Item = &OirField> {
        self.fields.iter().filter(|f| f.required)
    }

    /// Names of this entity's `persist create`-required columns: a field
    /// declaring `required: true`, excluding `crate::schema::
    /// FRAMEWORK_STAMPED_FIELDS`, any `@intrinsic` field (see
    /// [`Self::frame_fields`]), any field declaring a `default` (the store
    /// fills it in), and any field carrying `merged_from` (added by the
    /// inline phase's rebind merge from an imported atom's entity, never
    /// authored by this entity's own writer). Shared definition — the
    /// schema-level twin is `runtime::entity_access::
    /// required_create_field_names` (same predicate over
    /// `schema::FieldDefinition` instead of `OirField`); codegen consumers
    /// (`orbital-shell-typescript`'s persist codegen) read this one.
    pub fn required_create_field_names(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| f.required)
            .filter(|f| !crate::schema::types::FRAMEWORK_STAMPED_FIELDS.contains(&f.name.as_str()))
            .filter(|f| !f.intrinsic)
            .filter(|f| f.default.is_none())
            .filter(|f| f.merged_from.is_none())
            .map(|f| f.name.as_str())
            .collect()
    }

    /// Names of this entity's `@intrinsic` fields — the trait's own frame,
    /// never a persisted column, never mock-seeded, never an authorization
    /// input. This is the ONLY frame allow-list; codegen/runtime consumers
    /// must not derive a second one.
    pub fn frame_fields(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| f.intrinsic)
            .map(|f| f.name.as_str())
            .collect()
    }
}

/// Persistence mode for entities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OirPersistence {
    /// Stored in database
    #[default]
    Persistent,
    /// In-memory only, lost on restart
    Runtime,
}

/// Field definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirField {
    pub name: String,
    /// Universal type (NOT language-specific)
    pub field_type: OirType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<OirValue>,
    /// Validation constraints
    #[serde(default)]
    pub constraints: Vec<OirConstraint>,
    /// For enum fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    /// For relation fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<OirRelation>,
    /// `@intrinsic` in `.lolo` — trait-owned view-state/machinery the trait
    /// computes itself (mirrors `schema::FieldDefinition.intrinsic`). Never
    /// mock-seeded, never a persisted column, never an authorization input.
    /// The ONLY frame allow-list is [`OirEntity::frame_fields`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub intrinsic: bool,
    /// Mirrors `schema::FieldDefinition.merged_from` — name of the imported
    /// entity this field was copied FROM by the inline phase's rebind merge,
    /// when the host did not declare it itself. `None` for a host-declared
    /// field. See `OirEntity::required_create_field_names`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_from: Option<String>,
    /// The field exactly as the resolved schema declares it (`.orb` JSON). The mock
    /// seeder samples this shape on every path, so the compiled seed emits it verbatim
    /// rather than re-deriving it from the lossy `field_type`; carried on the IR wire so
    /// a shell program seeds the same rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<serde_json::Value>,
}

pub(super) fn default_true() -> bool {
    true
}

impl OirField {
    /// Create a new required field
    pub fn new(name: impl Into<String>, field_type: OirType) -> Self {
        Self {
            name: name.into(),
            field_type,
            description: None,
            required: true,
            default: None,
            constraints: vec![],
            enum_values: None,
            relation: None,
            intrinsic: false,
            merged_from: None,
            declared: None,
        }
    }

    /// Create an optional field
    pub fn optional(name: impl Into<String>, field_type: OirType) -> Self {
        Self {
            name: name.into(),
            field_type: OirType::Optional {
                inner: Box::new(field_type),
            },
            description: None,
            required: false,
            default: None,
            constraints: vec![],
            enum_values: None,
            relation: None,
            intrinsic: false,
            merged_from: None,
            declared: None,
        }
    }

    /// Set field as optional
    pub fn make_optional(mut self) -> Self {
        self.required = false;
        if !matches!(self.field_type, OirType::Optional { .. }) {
            self.field_type = OirType::Optional {
                inner: Box::new(self.field_type),
            };
        }
        self
    }

    /// The type to render at a field position plus whether that position is optional —
    /// `OirField::optional` records optionality both as `required:false` and as an `Optional`
    /// wrapper, event-payload fields only as `required:false`; renderers must mark it once.
    pub fn effective_type(&self) -> (&OirType, bool) {
        self.field_type.effective(self.required)
    }

    /// Add a constraint
    pub fn with_constraint(mut self, constraint: OirConstraint) -> Self {
        self.constraints.push(constraint);
        self
    }
}


/// Validation constraint
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OirConstraint {
    MinLength { value: usize },
    MaxLength { value: usize },
    Min { value: f64 },
    Max { value: f64 },
    Pattern { regex: String },
    Email,
    Url,
    Uuid,
    Phone,
    NonEmpty,
    Unique,
}

/// Relation definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirRelation {
    pub target_entity: String,
    /// V4 dual-carry id sibling of `target_entity` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_entity_id: Option<String>,
    pub cardinality: OirCardinality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign_key: Option<String>,
}

/// Relationship cardinality
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OirCardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}
