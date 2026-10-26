//! OIR Type Definitions
//!
//! Core types for the Orbital Intermediate Representation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{FieldDefinition, FieldType, ServiceRef, ThemeDefinition};
use crate::str_utils::to_snake_case;
use crate::collections::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Config field type declaration, preserving enum `values` alongside the
/// base `OirType` so codegen can emit literal unions (`"a" | "b"`) instead
/// of widening to `string`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OirConfigFieldType {
    #[serde(rename = "type")]
    pub field_type: OirType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// Root module - one per .orb schema
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirModule {
    /// Application name
    pub name: String,
    /// Semantic version
    pub version: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// All entity definitions
    pub entities: Vec<OirEntity>,
    /// All trait definitions (state machines + behaviors)
    pub traits: Vec<OirTrait>,
    /// All page definitions (UI)
    pub pages: Vec<OirPage>,
    /// Collected theme definitions from orbitals
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub themes: Vec<ThemeDefinition>,
    /// The app's default `data-theme` key (`"<base>-<light|dark>"`) — the
    /// app-level theme, else the first orbital's. `None` when nothing declares
    /// one; shells then use their baseline default.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default_theme: Option<String>,
    /// Collected service definitions from orbitals
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub services: Vec<ServiceRef>,
    /// The services this program calls whose only declared runtime is the
    /// browser (services registry `runtimes`): their calls run in the browser,
    /// never on a server. Stamped by the compiler from the registry it loaded.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub browser_services: Vec<String>,
    /// The program's `egress` origins: the only hosts its `http` effects may reach.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub egress: Vec<String>,
    /// The credentials the program declares: `secret` config knobs its `http` auth slots read
    /// (`secrets::program_credentials`), stamped by the compiler where the knob is still visible.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub program_credentials: Vec<crate::secrets::ProgramCredential>,
    /// The program's declared inbound handlers: the `/api/hooks/<name>` deliveries it serves.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub inbound: Vec<crate::inbound::InboundRoute>,
    /// The program's `os/` grants (closed by default): what its `os/` effects may touch.
    #[serde(skip_serializing_if = "crate::os_grants::OsGrants::is_empty", default)]
    pub os_grants: crate::os_grants::OsGrants,
    /// Entity names this module `expects` rather than declares — the consumer
    /// side of `expects entity X` / `expects entity X [identity]`. NAMES ONLY: no
    /// `OirEntity` is synthesized, so codegen and persistence never mistake an
    /// expectation for a declaration. They exist so existence checks (relation
    /// targets, `linkedEntity`, fetch/persist entity args) can tell "promised
    /// by the neighborhood" apart from "genuinely undefined"; the provider is
    /// checked at the link step once a composing program supplies it.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expected_entities: Vec<String>,
    /// `locales [en, ar, sl]` — the locales the program ships. A page's
    /// locale is its path's first segment when that names one of them,
    /// else the first.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub locales: Vec<String>,
    /// App site modifiers (`origin`, `icon`, `siteName`, `signIn`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<crate::schema::types::SiteMeta>,
    /// Named struct/union types discovered while resolving entity/payload
    /// fields — every alias name a [`OirType::TypeRef`] anywhere in the
    /// module points at, mapped to its full (non-recursive-shell)
    /// `OirType::Struct`/`Union` definition. Populated by the resolve phase
    /// (`resolve_field_shape` / `payload_field_to_oir_type`): whenever a
    /// struct's own fields resolve to a `TypeRef { name }` child, that struct
    /// IS `name`'s definition and gets registered here (first registration
    /// wins). The opt-in Phase-3D nominalization pass
    /// (`orbital-compiler::phases::nominalize`, `CompilerOptions::nominalize`)
    /// extends the table to anonymous shapes, minting positional
    /// `{Owner}{Field}…` lolo-ui-style names and never overwriting an
    /// existing entry. Every shell hoists one declaration per entry (the TS
    /// shell always did; the rest since the Phase-3D shell-readiness flip)
    /// and renders `TypeRef` as the bare name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub named_types: std::collections::BTreeMap<String, OirType>,
    /// Compilation metadata
    pub metadata: OirMetadata,
}

impl OirModule {
    /// Every transition's `await_render`, flattened in trait and transition order.
    pub fn await_render_rows(&self) -> Vec<AwaitRenderRow> {
        let mut rows = Vec::new();
        for t in &self.traits {
            for tr in &t.transitions {
                let from = match &tr.from {
                    OirTransitionFrom::Single(s) => Some(vec![s.clone()]),
                    OirTransitionFrom::Multiple(states) => Some(states.clone()),
                    OirTransitionFrom::Any => None,
                };
                for r in &tr.await_render {
                    rows.push(AwaitRenderRow {
                        request_trait: t.name.clone(),
                        event: tr.event.clone(),
                        from: from.clone(),
                        trait_name: r.trait_name.clone(),
                        slot: r.slot.clone(),
                        skeleton: r.skeleton.clone(),
                    });
                }
            }
        }
        rows
    }

    /// Create a new empty module
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            site: None,
            name: name.into(),
            version: version.into(),
            description: None,
            entities: vec![],
            traits: vec![],
            pages: vec![],
            themes: vec![],
            default_theme: None,
            services: vec![],
            browser_services: vec![],
            egress: vec![],
            program_credentials: vec![],
            inbound: vec![],
            os_grants: Default::default(),
            expected_entities: vec![],
            locales: vec![],
            named_types: std::collections::BTreeMap::new(),
            metadata: OirMetadata::default(),
        }
    }

    /// The locale a page path belongs to — see [`OirModule::locales`].
    pub fn locale_of_path(&self, path: &str) -> Option<&str> {
        crate::messages::locale_of_path(&self.locales, path)
    }

    /// Every service a `call-service` in a transition or tick names.
    pub fn invoked_services(&self) -> std::collections::BTreeSet<String> {
        let visit = invoked_services_in;
        let mut names = std::collections::BTreeSet::new();
        for t in &self.traits {
            for e in t
                .transitions
                .iter()
                .flat_map(|tr| &tr.effects)
                .chain(t.ticks.iter().flat_map(|tick| &tick.effects))
            {
                visit(e, &mut names);
            }
        }
        names
    }

    /// The module's entities with same-named duplicates collapsed, first wins.
    /// Orbitals that import the same atom each carry a clone of its entity, and
    /// `ir::validation` permits that, so every shell that declares one type per
    /// entity name goes through this.
    pub fn distinct_entities(&self) -> Vec<OirEntity> {
        let mut seen = std::collections::HashSet::new();
        self.entities
            .iter()
            .filter(|e| seen.insert(e.name.as_str()))
            .cloned()
            .collect()
    }

    /// Find an entity by name
    pub fn find_entity(&self, name: &str) -> Option<&OirEntity> {
        self.entities.iter().find(|e| e.name == name)
    }

    /// Find a trait by name
    pub fn find_trait(&self, name: &str) -> Option<&OirTrait> {
        self.traits.iter().find(|t| t.name == name)
    }

    /// V4 identity-keyed entity resolution. Prefer an exact **id** join when the
    /// reference carries one and some entity carries the same id; otherwise fall
    /// back to an exact **name** match (legacy / dual-carry). No fuzzy or
    /// similarity matching — exact id or exact name only. When `id` is `None`
    /// this is byte-for-byte the same lookup as `find_entity(name)`, so id-free
    /// schemas emit identical output.
    pub fn resolve_entity_ref(&self, id: Option<&str>, name: &str) -> Option<&OirEntity> {
        if let Some(id) = id {
            if let Some(hit) = self.entities.iter().find(|e| e.id.as_deref() == Some(id)) {
                return Some(hit);
            }
        }
        self.find_entity(name)
    }

    /// V4 identity-keyed trait resolution. Same id-first, name-fallback rule as
    /// [`resolve_entity_ref`].
    pub fn resolve_trait_ref(&self, id: Option<&str>, name: &str) -> Option<&OirTrait> {
        if let Some(id) = id {
            if let Some(hit) = self.traits.iter().find(|t| t.id.as_deref() == Some(id)) {
                return Some(hit);
            }
        }
        self.find_trait(name)
    }

    /// Find a page by name
    pub fn find_page(&self, name: &str) -> Option<&OirPage> {
        self.pages.iter().find(|p| p.name == name)
    }

    /// Propagate each collection's declared access directives to every other
    /// entity in the module bound to that same collection which declares none
    /// of its own — the compiled path's twin of
    /// `runtime::entity_access::resolve_collection_inheritance`.
    ///
    /// A `persistent:` collection is one table, so two entity declarations over
    /// it cannot honestly carry different visibility rules. Without this, the
    /// TypeScript shell emits a policy const per *declared* entity, and a page
    /// rendering through a std-atom trait that fetched the ATOM's entity on the
    /// same collection gets no policy at all — ALLOW-ALL
    /// (`R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`; measured on
    /// `std-customer-success`, where the atom's `HealthScore` [healthscores]
    /// emitted no const beside `HealthScoreSearch`'s).
    ///
    /// Runs once on the whole module after entity resolution, so it sees the
    /// organism's entity and the spliced-in atom's entity together. Only ever
    /// ADDS a restriction; conflicting declarations on one collection are
    /// rejected at validate time by `ORB_S_COLLECTION_POLICY_CONFLICT`.
    pub fn resolve_collection_policy_inheritance(&mut self) {
        type Directives = (
            Option<crate::ir::expr::OirExpr>,
            Option<crate::ir::expr::OirExpr>,
            Option<crate::ir::expr::OirExpr>,
            Option<crate::ir::expr::OirExpr>,
        );
        let declared: HashMap<String, Directives> = self
            .entities
            .iter()
            .filter(|e| e.declares_access_policy())
            .map(|e| {
                (
                    e.collection.clone(),
                    (
                        e.read_policy.clone(),
                        e.create_policy.clone(),
                        e.update_policy.clone(),
                        e.delete_policy.clone(),
                    ),
                )
            })
            .collect();
        if declared.is_empty() {
            return;
        }

        for entity in &mut self.entities {
            if entity.declares_access_policy() {
                continue;
            }
            let Some((read, create, update, delete)) = declared.get(&entity.collection) else {
                continue;
            };
            entity.read_policy = read.clone();
            entity.create_policy = create.clone();
            entity.update_policy = update.clone();
            entity.delete_policy = delete.clone();
        }
    }

    /// Find a page by path
    pub fn find_page_by_path(&self, path: &str) -> Option<&OirPage> {
        self.pages.iter().find(|p| p.path == path)
    }
}

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

fn default_true() -> bool {
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

/// Universal type system
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OirType {
    String,
    Number,
    Integer,
    Boolean,
    Date,
    DateTime,
    /// Epoch milliseconds the clock produced (`time/now`, `time/today`, and time arithmetic over an instant): always a
    /// valid moment, so formatting it cannot fail. Nothing widens into it; it fits wherever a number does.
    Instant,
    /// Semantic string domains. Every target renders these as its string type;
    /// they exist in the IR so `oir_type_to_mock_type` / `oir_type_to_form_type`
    /// can dispatch on the declared domain instead of guessing from a name.
    Email,
    Url,
    Phone,
    Uuid,
    Image,
    /// Monetary amount — numeric at rest. Distinct from `Number` so codegen
    /// and renderers dispatch currency formatting/inputs on the declared
    /// type instead of guessing from a field name.
    Money,
    /// Uploaded file/attachment — a STRUCT value at rest:
    /// `{ name: string, url: string, mimeType: string, sizeBytes: number }`.
    /// `url` is a `data:` URI in mock mode or an object-storage URL in
    /// production. Not a semantic string: it names a stored artifact.
    File,
    /// Binary value; standard padded base64 string at run time, never
    /// implicitly a `string`.
    Bytes,
    /// A call key: a string at rest, but a bare `string` converts to it only through
    /// the explicit cast (`Coercion::Explicit`). Distinct from the semantic string
    /// domains, whose `String` source is refined implicitly.
    CallKey,
    /// The name of a field of the record `record`, a string at run time; `value` is the type that field must have,
    /// when declared. Read through `object/get` of a record that is an `R`, it types the read.
    #[serde(rename_all = "camelCase")]
    FieldRef {
        record: Box<OirType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Box<OirType>>,
    },
    Json,
    /// Renderable UI content (a `node`-typed field/knob): a pattern node or
    /// array of them. Distinct from `Json` so codegen can emit the canonical
    /// pattern-tree type instead of a shapeless record.
    Node,
    #[serde(rename_all = "camelCase")]
    Array {
        inner: Box<OirType>,
    },
    /// Fixed-length array with a type per slot (`.lolo` `[number, number, number]`).
    #[serde(rename_all = "camelCase")]
    Tuple {
        items: Vec<OirType>,
    },
    /// Dynamic-key uniform-value map (`.lolo` `Map K V`) — emits
    /// `Record<K, V>` in codegen. Keys are arbitrary (not known at author
    /// time); the value type is uniform. Distinct from `Struct` (fixed keys).
    #[serde(rename_all = "camelCase")]
    Map {
        key: Box<OirType>,
        value: Box<OirType>,
    },
    #[serde(rename_all = "camelCase")]
    Optional {
        inner: Box<OirType>,
    },
    #[serde(rename_all = "camelCase")]
    Reference {
        entity: String,
    },
    /// An embedded full row of `entity` (an `@intrinsic` list-of-entity
    /// field), distinct from `Reference`, which is the id/relation value.
    #[serde(rename_all = "camelCase")]
    Row {
        entity: String,
    },
    #[serde(rename_all = "camelCase")]
    Enum {
        name: String,
    },
    /// Inline struct (anonymous object) — preserves the field shape
    /// declared by a lolo struct type alias (`type X = { f1: T1, f2: T2 }`)
    /// so codegen can emit a strict TypeScript type instead of widening
    /// to `Record<string, FieldValue | undefined>`.
    Struct {
        fields: Vec<OirField>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional: Option<Box<OirType>>,
    },
    /// A TAGGED union of struct variants. `variants` are in declaration order;
    /// each carries its own name so codegen can emit a discriminated union and
    /// the validator can select a variant by the value's tag field. A variant
    /// whose shape is a by-name recursive back-reference resolves to
    /// `OirType::TypeRef` rather than expanding forever.
    Union {
        variants: Vec<OirField>,
    },
    /// `@trait.X` reference type. Allowed only on trait config fields. The
    /// codegen + runtime substitute string values matching `@trait.<Name>`
    /// with embedded trait UI at render time. Maps to the `TraitFieldRef`
    /// branded string type in the emitted TypeScript.
    Trait,
    /// An orbital value held as data (language trio): `{ behavior, orbital, …§8b body }`
    /// or `none`. Never rendered — only a trait value embeds.
    Orbital,
    /// A whole behavior held as data: `{ behavior }` or `none`; embeds as a running program.
    Behavior,
    /// Compiled s-expression (`SExpr` from `@almadar/core`). Allowed only on
    /// trait config fields (e.g. a `conditionalFields` map's value). Emits the
    /// recursive `SExpr` TypeScript type, inlined as a local alias in codegen
    /// (like `TraitFieldRef`) so no cross-file import is needed.
    SExpr,
    /// A by-name reference to a named struct type declared elsewhere in the
    /// same expansion tree — same wire shape as the struct it names. Exists
    /// so a self-referential `type X = { … : [X] }` can be represented at
    /// all: a recursive back-reference cannot be INLINED (the `.orb` JSON is
    /// finite), so the resolver stops expanding at the cycle and leaves a
    /// `TypeRef` naming what it would have expanded to. Codegen backends with
    /// a named-type facility (the TS shell) hoist one declaration per name and
    /// emit a real recursive reference; every other backend renders the wire shape (see
    /// `ir/render.rs`'s default arm).
    TypeRef {
        name: String,
    },
    /// An unbound type variable (`t` of `type Page t` or a trait's `(t : Type)`):
    /// opaque, so it accepts only itself, or `Json` by erasure.
    Var {
        name: String,
    },
}

impl OirType {
    /// Rewrite existing type nodes; replacements are terminal for this traversal.
    pub fn rewrite(&mut self, replace: &impl Fn(&OirType) -> Option<OirType>) -> bool {
        if let Some(next) = replace(self) {
            let changed = *self != next;
            *self = next;
            return changed;
        }
        match self {
            Self::Array { inner } | Self::Optional { inner } => inner.rewrite(replace),
            Self::Map { key, value } => {
                let key_changed = key.rewrite(replace);
                value.rewrite(replace) | key_changed
            }
            Self::Tuple { items } => {
                let mut changed = false;
                for item in items {
                    changed |= item.rewrite(replace);
                }
                changed
            }
            Self::Struct { fields, additional } => {
                let mut changed = false;
                for field in fields { changed |= field.field_type.rewrite(replace); }
                if let Some(additional) = additional { changed |= additional.rewrite(replace); }
                changed
            }
            Self::FieldRef { record, value } => {
                let record_changed = record.rewrite(replace);
                match value {
                    Some(value) => value.rewrite(replace) | record_changed,
                    None => record_changed,
                }
            }
            Self::Union { variants: fields } => {
                let mut changed = false;
                for field in fields {
                    changed |= field.field_type.rewrite(replace);
                }
                changed
            }
            _ => false,
        }
    }

    pub fn substitute_type_vars(
        &mut self,
        bindings: &std::collections::BTreeMap<String, OirType>,
    ) -> bool {
        self.rewrite(&|ty| match ty {
            Self::Var { name } => bindings.get(name).cloned(),
            _ => None,
        })
    }

    /// Create an array type
    pub fn array(inner: OirType) -> Self {
        Self::Array {
            inner: Box::new(inner),
        }
    }

    /// Create a dynamic-key map type (`Record<key, value>`).
    pub fn map(key: OirType, value: OirType) -> Self {
        Self::Map {
            key: Box::new(key),
            value: Box::new(value),
        }
    }

    /// Create an optional type. Idempotent: wrapping an already-`Optional`
    /// type returns it unchanged rather than double-wrapping.
    /// The type at an optional-aware position: unwraps exactly one `Optional` layer and
    /// reports whether the position is optional (wrapped, or declared `required:false`).
    pub fn effective(&self, required: bool) -> (&OirType, bool) {
        match self {
            OirType::Optional { inner } => (inner.as_ref(), true),
            t => (t, !required),
        }
    }

    pub fn optional(inner: OirType) -> Self {
        if matches!(inner, Self::Optional { .. }) {
            return inner;
        }
        Self::Optional {
            inner: Box::new(inner),
        }
    }

    /// Create a reference type
    pub fn reference(entity: impl Into<String>) -> Self {
        Self::Reference {
            entity: entity.into(),
        }
    }

    /// Create an embedded-row type
    pub fn row(entity: impl Into<String>) -> Self {
        Self::Row {
            entity: entity.into(),
        }
    }

    /// Create an enum type
    pub fn enum_type(name: impl Into<String>) -> Self {
        Self::Enum { name: name.into() }
    }

    /// Check if this type is optional
    pub fn is_optional(&self) -> bool {
        matches!(self, Self::Optional { .. })
    }

    /// Get the inner type if this is an array or optional
    pub fn inner_type(&self) -> Option<&OirType> {
        match self {
            Self::Array { inner } | Self::Optional { inner } => Some(inner),
            _ => None,
        }
    }
}

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

fn field_shape_inner(field: &FieldDefinition) -> OirType {
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
                OirType::Struct { fields, additional: field.additional_properties.as_deref().map(field_shape).map(Box::new) }
            }
            (None, _) if field.additional_properties.is_some() => OirType::Struct { fields: Vec::new(), additional: field.additional_properties.as_deref().map(field_shape).map(Box::new) },
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
                OirType::Union { variants }
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
        return OirType::Struct { fields, additional: None };
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
        "object" => return OirType::Struct { fields: Vec::new(), additional: None },
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
        return OirType::Struct { fields: Vec::new(), additional: None };
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

fn payload_field_shape_inner(
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
        return OirType::Struct { fields: Vec::new(), additional: field.additional_properties.as_deref().map(|field| payload_field_shape_with_entity(field, is_entity)).map(Box::new) };
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
                return OirType::Union { variants: fields };
            }
            let record = OirType::Struct { fields, additional: field.additional_properties.as_deref().map(|field| payload_field_shape_with_entity(field, is_entity)).map(Box::new) };
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

/// Trait definition (state machine + behaviors)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTrait {
    pub name: String,
    /// V4 dual-carry node id — the durable join key. Optional until the Phase-7
    /// flip; when present, `OirModule::resolve_trait_ref` joins by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Owning orbital name from the source schema. Populated by the
    /// resolve phase (`for orbital in schema.orbitals { resolve_trait(...) }`)
    /// so codegen and the runtime can construct the qualified
    /// `Orbital.Trait` bus scope (gap #13). Defaults to empty when an older
    /// .orb is loaded without this field.
    #[serde(default)]
    pub orbital_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where this trait came from
    #[serde(default)]
    pub source: OirTraitSource,
    /// Entity this trait is linked to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_entity: Option<String>,
    /// V4 dual-carry id sibling of `linked_entity` — optional until the Phase-7 flip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_entity_id: Option<String>,
    /// V4 leverage-ids — carries the schema `TraitDefinition.entity_ref_ids` side-map
    /// (referenced entity NAME → stable entity id) into the OIR so codegen can source
    /// the id for a non-linked entity token (`(persist create Product …)`, `(fetch …)`)
    /// and resolve it via `resolve_entity_ref(id, name)` — surviving a renamed entity
    /// declaration without a name-rewrite pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref_ids: Option<OirIdMap>,
    /// V4 leverage-ids — carries `TraitDefinition.trait_embed_ids` (embed token NAME →
    /// embedded trait id) into the OIR so codegen resolves `@trait.<Name>` embeds via
    /// `resolve_trait_ref(id, name)` under a renamed sibling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_embed_ids: Option<OirIdMap>,
    /// Configuration schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<OirObject>,
    /// Declared config field types from the atom's `config {}` block.
    /// Populated by the inline phase when resolving ref traits so the
    /// TypeScript backend can emit a strongly-typed `<TraitName>Config`
    /// interface. Separate from `config` (which holds call-site overrides).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_config_schema: Option<OirConfigSchema>,
    /// Declared config field defaults from the atom's `config {}` block.
    /// Parallel to `declared_config_schema` — keys are the same field names,
    /// values are the parsed default expressions. The TypeScript backend
    /// uses this to emit a `DEFAULT_CONFIG` constant in each trait's logic
    /// hook so that `config?.icon` resolves to the declared default when no
    /// consumer override is provided. Solution-1 of G24's follow-up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_config_defaults: Option<OirObject>,

    // State Machine
    pub states: Vec<OirState>,
    pub events: Vec<OirEvent>,
    pub transitions: Vec<OirTransition>,
    /// Named, reusable guards
    #[serde(default)]
    pub guards: Vec<OirGuard>,

    // Behaviors
    #[serde(default)]
    pub ticks: Vec<OirTick>,
    #[serde(default)]
    pub listeners: Vec<OirListener>,
    /// Events this trait emits (for cross-trait communication)
    #[serde(default)]
    pub emits: Vec<OirEmit>,

    // Trait-owned data
    #[serde(default)]
    pub data_entities: Vec<OirEntity>,

    /// UI slots used by this trait (collected from render-ui effects)
    #[serde(default)]
    pub slots_used: Vec<String>,
    /// Name of the trait that embeds this one via a bare `@trait.<Name>`
    /// reference (an anonymous inline JSX embed — `<Component.traits.X …/>`
    /// — or a named `<trait.X/>`), when this trait is embedded chrome
    /// rather than a standalone top-level trait. Populated by the resolve
    /// phase from the SAME embed graph `phases/embed_graph.rs` builds for
    /// `@config.<knob>` forwarding (gap #13 sibling) — one graph, two
    /// consumers, so they can't disagree about who embeds whom.
    ///
    /// The compiled path's bus-key qualification (`orbital-shell-
    /// typescript`, `qualify_event`) needs this: an embedded trait's
    /// action/event-name config values are provided by its embedder and
    /// belong to the embedder's own event vocabulary — the runtime's embed
    /// routing delivers such an event to the HOST, never to a subscription
    /// on the embedded trait's own bus key (mirrors `@almadar/core`'s
    /// `collectEmbeddedTraitReferrers`, used by `click-wiring-audit.ts`'s
    /// `'host-transition'` wiring verdict). Qualifying under the embedded
    /// trait's own scope instead produces a bus key nothing listens to —
    /// C-EMBED-ACTION-SELF-SCOPED-NOT-HOST-SCOPED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_trait: Option<String>,

    /// Declared dispatch mode (`[client]` / `[optimistic]`). See
    /// [`OirTrait::dispatch_mode`]. Absent means server-gated dispatch.
    /// Never inferred from effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<crate::schema::types::DispatchMode>,
}

/// Client dispatch strategy for a trait's transitions. A pure lookup over
/// declared facts — never derived by inspecting a trait's effects (owner
/// ruling: nothing about dispatch may be inferred). See
/// [`OirTrait::dispatch_mode`]. Type alias onto the single owner
/// (`runtime::dispatch_mode::DispatchMode`) so the compiled path and the
/// runtime kernel's `schema::types::TraitDefinition::dispatch_mode` share one
/// enum, not two independently-defined ones with the same shape — variant
/// names are identical so every existing `OirDispatchMode::*` match arm and
/// downstream consumer (`orbital-shell-typescript`) compiles unchanged.
pub type OirDispatchMode = crate::dispatch_mode::DispatchMode;

impl OirTrait {
    /// Create a new trait
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
            orbital_name: String::new(),
            description: None,
            source: OirTraitSource::Schema,
            linked_entity: None,
            linked_entity_id: None,
            entity_ref_ids: None,
            trait_embed_ids: None,
            config: None,
            declared_config_schema: None,
            declared_config_defaults: None,
            states: vec![],
            events: vec![],
            transitions: vec![],
            guards: vec![],
            ticks: vec![],
            listeners: vec![],
            emits: vec![],
            data_entities: vec![],
            slots_used: vec![],
            host_trait: None,
            dispatch: None,
        }
    }

    /// Find the initial state
    pub fn initial_state(&self) -> Option<&OirState> {
        self.states.iter().find(|s| s.is_initial)
    }

    /// Find a state by name
    pub fn find_state(&self, name: &str) -> Option<&OirState> {
        self.states.iter().find(|s| s.name == name)
    }

    /// V4 identity-aware "is this the trait a reference points at?" check, for
    /// joins over a trait **slice** (where [`OirModule::resolve_trait_ref`] isn't
    /// available). Id match when both sides carry ids; exact-name fallback
    /// otherwise. Byte-identical to a name compare on id-free schemas.
    pub fn matches_ref(&self, id: Option<&str>, name: &str) -> bool {
        match (id, self.id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => self.name == name,
        }
    }

    /// V4 identity-aware "does this trait bind `entity`?" check. Joins by **id**
    /// when both the trait's `linked_entity_id` and the entity's `id` are
    /// present; falls back to an exact **name** compare otherwise. Byte-identical
    /// to the bare name compare on id-free schemas.
    pub fn links_entity(&self, entity: &OirEntity) -> bool {
        match (self.linked_entity_id.as_deref(), entity.id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => self.linked_entity.as_deref() == Some(entity.name.as_str()),
        }
    }

    /// Client dispatch strategy for this trait's transitions — a pure LOOKUP
    /// of the trait's declared `dispatch`: `client`, `optimistic`, else server.
    pub fn dispatch_mode(
        &self,
        _entity: Option<&OirEntity>,
        _residence: &crate::runs_on::Residence,
    ) -> OirDispatchMode {
        crate::dispatch_mode::compute(self.dispatch)
    }

    /// Where `tick` runs: on the server when it is `[background]` or this trait is
    /// server-awaited (`PersistedAwaited`), in the client otherwise. The one placement rule
    /// the validator (`ORB_T_CLIENT_TICK_SERVER_EFFECT`), the kernel and the shells share.
    pub fn tick_runs_on_client(
        &self,
        tick: &OirTick,
        entity: Option<&OirEntity>,
        residence: &crate::runs_on::Residence,
    ) -> bool {
        !tick.background
            && self.dispatch_mode(entity, residence) != OirDispatchMode::PersistedAwaited
    }

    /// Whether this trait's own effects reach the server: any transition, or
    /// any client tick (a `[background]` tick runs on the host, not a dispatch).
    /// A data effect on a client-resident entity (`residence`) is client work.
    pub fn touches_server(&self, residence: &crate::runs_on::Residence) -> bool {
        self.transitions
            .iter()
            .filter(|t| !t.guard.as_ref().is_some_and(|g| g.is_statically_false()))
            .any(|t| t.effects.iter().any(|e| e.runs_server_effect(residence)))
            || self
                .ticks
                .iter()
                .filter(|t| !t.background)
                .any(|t| t.effects.iter().any(|e| e.runs_server_effect(residence)))
    }

    /// The events whose live transitions run a data effect on a browser-stored
    /// entity: their leg goes to the browser store, not the server. A client tick
    /// that does is listed as its `runTick` event ([`crate::effects::tick_event_key`]).
    /// Twin of `@almadar/runtime` `browserLegEvents`.
    pub fn browser_leg_events(
        &self,
        residence: &crate::runs_on::Residence,
    ) -> std::collections::BTreeSet<String> {
        let transitions = self
            .transitions
            .iter()
            .filter(|t| !t.guard.as_ref().is_some_and(|g| g.is_statically_false()))
            .filter(|t| {
                t.effects
                    .iter()
                    .any(|e| e.runs_browser_data_effect(residence))
            })
            .map(|t| t.event.clone());
        let ticks = self
            .ticks
            .iter()
            .filter(|t| !t.background)
            .filter(|t| {
                t.effects
                    .iter()
                    .any(|e| e.runs_browser_data_effect(residence))
            })
            .map(|t| crate::bus_event::tick_event_key(&t.name));
        transitions.chain(ticks).collect()
    }

    /// Find an event by key
    pub fn find_event(&self, key: &str) -> Option<&OirEvent> {
        self.events.iter().find(|e| e.key == key)
    }

    /// Find transitions from a state
    pub fn transitions_from<'a>(
        &'a self,
        state: &'a str,
    ) -> impl Iterator<Item = &'a OirTransition> + 'a {
        self.transitions.iter().filter(move |t| match &t.from {
            OirTransitionFrom::Single(s) => s == state,
            OirTransitionFrom::Multiple(states) => states.contains(&state.to_string()),
            OirTransitionFrom::Any => true,
        })
    }

    /// Find a named guard
    pub fn find_guard(&self, name: &str) -> Option<&OirGuard> {
        self.guards.iter().find(|g| g.name == name)
    }
}

/// Trait source
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OirTraitSource {
    /// Defined in .orb file
    #[default]
    Schema,
    /// From std library
    Library,
    /// Inline in orbital definition
    Inline,
}

/// State definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirState {
    pub name: String,
    #[serde(default)]
    pub is_initial: bool,
    #[serde(default)]
    pub is_final: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Duration in milliseconds after which a TIMEOUT transition should
    /// fire, from `state X for Ns { ... }`. `None` for a state with no
    /// time bound. Mirrors `schema::StateDefinition::time_bound_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_bound_ms: Option<u64>,
}

impl OirState {
    /// Create a new state
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_initial: false,
            is_final: false,
            description: None,
            time_bound_ms: None,
        }
    }

    /// Create the initial state
    pub fn initial(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_initial: true,
            is_final: false,
            description: None,
            time_bound_ms: None,
        }
    }

    /// Create a final state
    pub fn final_state(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_initial: false,
            is_final: true,
            description: None,
            time_bound_ms: None,
        }
    }
}

/// Event definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEvent {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_schema: Option<OirTypeMap>,
    /// Names of payload fields marked `required` (from the `!` author
    /// marker in `.lolo`'s listens block). Preserved alongside
    /// `payload_schema` so the TypeScript codegen can inject API-
    /// boundary validation in each generated handler — same
    /// `validateEventPayload` call the runtime path uses, with the
    /// required-field list inlined per event. Without this, the IR
    /// drops the `!` flag and the compiled handler accepts empty
    /// payloads that the runtime path rejects, producing exactly the
    /// kind of asymmetry the validator is meant to close.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub payload_required_fields: std::collections::BTreeSet<String>,
    /// The declared external input record (`listens { EVENT -> input { … } }`)
    /// when outside clients may send this event on the input channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<crate::external_inputs::ExternalInput>,
}

impl OirEvent {
    /// Create a new event
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: None,
            description: None,
            payload_schema: None,
            payload_required_fields: std::collections::BTreeSet::new(),
            external: None,
        }
    }

    /// Create an event with a display name
    pub fn with_name(key: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: Some(name.into()),
            description: None,
            payload_schema: None,
            payload_required_fields: std::collections::BTreeSet::new(),
            external: None,
        }
    }
}

/// Transition definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTransition {
    /// Source state(s) - can be single, multiple, or "*" for any
    pub from: OirTransitionFrom,
    /// Target state
    pub to: String,
    /// Triggering event
    pub event: String,
    /// Guard condition (S-expression)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    /// Effects to execute (S-expressions)
    #[serde(default)]
    pub effects: Vec<crate::ir::expr::OirExpr>,
    /// Predicted renders this transition's server effects lead to (awaiting-server
    /// skeletons). Computed by orbital-compiler from the resolved schema; never authored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub await_render: Vec<AwaitRender>,
}

/// One `await_render` prediction keyed by the dispatch that leads to it: while the server leg of
/// `request_trait` taking `event` from one of `from` (any state when `None`) is in flight, `trait`'s
/// empty `slot` shows `skeleton`. The table a native shell embeds (the web reads the same
/// predictions off each transition).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaitRenderRow {
    pub request_trait: String,
    pub event: String,
    pub from: Option<Vec<String>>,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub slot: String,
    pub skeleton: SkeletonNode,
}

/// The same rows read off a resolved schema (`orb resolve` output, whose transitions carry the
/// compiler's `awaitRender`), for hosts that run on the schema rather than OIR (the desktop
/// clients). A transition without predictions contributes nothing; an unreadable prediction is
/// skipped with a warning, never guessed.
pub fn await_render_rows_from_resolved(resolved: &serde_json::Value) -> Vec<AwaitRenderRow> {
    let mut rows = Vec::new();
    let orbitals = resolved
        .get("orbitals")
        .and_then(serde_json::Value::as_array);
    for trait_value in orbitals
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("traits")?.as_array())
        .flatten()
    {
        let Some(name) = trait_value.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let transitions = trait_value
            .get("stateMachine")
            .and_then(|sm| sm.get("transitions"))
            .and_then(serde_json::Value::as_array);
        for transition in transitions.into_iter().flatten() {
            let Some(renders) = transition.get("awaitRender") else {
                continue;
            };
            let renders: Vec<AwaitRender> = match serde_json::from_value(renders.clone()) {
                Ok(renders) => renders,
                Err(error) => {
                    tracing::warn!(trait_name = %name, %error, "unreadable awaitRender skipped");
                    continue;
                }
            };
            let from = match transition.get("from") {
                Some(serde_json::Value::String(state)) if state != "*" => Some(vec![state.clone()]),
                Some(serde_json::Value::Array(states)) => Some(
                    states
                        .iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect(),
                ),
                _ => None,
            };
            let event = transition
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            for r in renders {
                rows.push(AwaitRenderRow {
                    request_trait: name.to_string(),
                    event: event.to_string(),
                    from: from.clone(),
                    trait_name: r.trait_name,
                    slot: r.slot,
                    skeleton: r.skeleton,
                });
            }
        }
    }
    rows
}

/// The skeleton shapes every shell can draw (`@almadar/core` `SKELETON_VARIANTS`).
pub const SKELETON_VARIANTS: [&str; 9] = [
    "header", "table", "list", "grid", "detail", "stats", "form", "card", "text",
];

/// A sized skeleton shape (`@almadar/core` `SkeletonShape`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkeletonShape {
    pub variant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<u32>,
}

/// A predicted skeleton: one shape, or a vertical stack (`@almadar/core` `SkeletonNode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkeletonNode {
    Shape { shape: SkeletonShape },
    Stack { stack: Vec<SkeletonNode> },
}

/// One predicted render: `trait` will render into `slot` something shaped like `skeleton`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AwaitRender {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub slot: String,
    pub skeleton: SkeletonNode,
}

impl OirTransition {
    /// Create a simple transition
    pub fn new(from: impl Into<String>, to: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            from: OirTransitionFrom::Single(from.into()),
            to: to.into(),
            event: event.into(),
            guard: None,
            effects: vec![],
            await_render: vec![],
        }
    }

    /// Create a transition from any state
    pub fn from_any(to: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            from: OirTransitionFrom::Any,
            to: to.into(),
            event: event.into(),
            guard: None,
            effects: vec![],
            await_render: vec![],
        }
    }

    /// Add a guard
    pub fn with_guard(mut self, guard: crate::ir::expr::OirExpr) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Add effects
    pub fn with_effects(mut self, effects: Vec<crate::ir::expr::OirExpr>) -> Self {
        self.effects = effects;
        self
    }
}

/// Transition source
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OirTransitionFrom {
    Single(String),
    Multiple(Vec<String>),
    #[serde(rename = "*")]
    Any,
}

impl OirTransitionFrom {
    /// Check if this matches a given state
    pub fn matches(&self, state: &str) -> bool {
        match self {
            Self::Single(s) => s == state,
            Self::Multiple(states) => states.iter().any(|s| s == state),
            Self::Any => true,
        }
    }
}

impl std::fmt::Display for OirTransitionFrom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(s) => write!(f, "{}", s),
            Self::Multiple(states) => write!(f, "[{}]", states.join(", ")),
            Self::Any => write!(f, "*"),
        }
    }
}

/// Named guard definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirGuard {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub expression: crate::ir::expr::OirExpr,
}

impl OirGuard {
    /// Create a new guard
    pub fn new(name: impl Into<String>, expression: crate::ir::expr::OirExpr) -> Self {
        Self {
            name: name.into(),
            description: None,
            expression,
        }
    }
}

/// Tick definition (scheduled behavior)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTick {
    pub name: String,
    /// Interval specification
    pub interval: OirInterval,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    #[serde(default)]
    pub effects: Vec<crate::ir::expr::OirExpr>,
    #[serde(default)]
    pub priority: i32,
    /// `[background]`: runs with no client mounted.
    #[serde(
        rename = "runsInBackground",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub background: bool,
    /// `[visible]`: a browser tick that pauses while its trait's rendered output is off screen.
    #[serde(
        rename = "whileVisible",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub while_visible: bool,
    /// The trait states the tick runs in; empty = every state.
    #[serde(rename = "appliesTo", default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
}

impl OirTick {
    /// Create a new tick
    pub fn new(name: impl Into<String>, interval: OirInterval) -> Self {
        Self {
            name: name.into(),
            interval,
            guard: None,
            effects: vec![],
            priority: 0,
            background: false,
            while_visible: false,
            applies_to: vec![],
        }
    }
}

/// Interval specification
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OirInterval {
    Milliseconds {
        value: u64,
    },
    Seconds {
        value: u64,
    },
    Minutes {
        value: u64,
    },
    /// Every frame (games)
    Frame,
    /// A validated 5-field cron expression (e.g. `"0 9 * * *"`), stored as
    /// the raw source string — see `crate::cron` for parsing/matching.
    /// `resolve_tick` only produces this variant after confirming the
    /// string is a syntactically valid cron expression.
    Cron { expression: String },
}

impl OirInterval {
    /// Convert to milliseconds. `None` for `Frame` (no fixed interval) and
    /// `Cron` (needs calendar matching, not a fixed delay — see `crate::cron`).
    pub fn to_millis(&self) -> Option<u64> {
        match self {
            Self::Milliseconds { value } => Some(*value),
            Self::Seconds { value } => Some(*value * 1000),
            Self::Minutes { value } => Some(*value * 60 * 1000),
            Self::Frame => None,
            Self::Cron { .. } => None,
        }
    }
}

/// Listener definition (cross-trait events)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirListener {
    /// BARE event name (no source prefix). The emitter fires this exact key
    /// on the bus; source filtering happens via `source_trait` / `source_orbital`.
    pub event_pattern: String,
    /// Trait name of the expected emitter. When present, the generated hook
    /// filters incoming events to those whose `source.trait` matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_trait: Option<String>,
    /// V4 dual-carry id sibling of `source_trait` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_trait_id: Option<String>,
    /// Orbital name of the expected emitter. When present (alongside
    /// `source_trait`), the generated hook additionally filters by
    /// `source.orbital` for fully-qualified cross-orbital listens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_orbital: Option<String>,
    /// V4 dual-carry id sibling of `source_orbital` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_orbital_id: Option<String>,
    /// `true` when the author wrote `* EVENT -> TRIGGER` (accept any source).
    /// Mutually exclusive with the source fields above.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_any: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<crate::ir::expr::OirExpr>,
    /// `with { field: expr }` payload projection. Evaluated payload-only at
    /// delivery (mirrors `runtime/listener.rs`); the mapped payload REPLACES
    /// the source payload. `None` delivers the source payload unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_mapping: Option<crate::ir::expr::OirFields>,
    /// Event to trigger internally
    pub triggers: String,
}

impl OirListener {
    /// Create a new listener with no source scoping (accepts any source).
    pub fn new(event_pattern: impl Into<String>, triggers: impl Into<String>) -> Self {
        Self {
            event_pattern: event_pattern.into(),
            source_trait: None,
            source_trait_id: None,
            source_orbital: None,
            source_orbital_id: None,
            source_any: false,
            guard: None,
            payload_mapping: None,
            triggers: triggers.into(),
        }
    }
}

/// Event emission definition - declares events this trait can emit to other traits
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEmit {
    /// Event name
    pub event: String,
    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Description of when/why this event is emitted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Scope: internal (same trait) or external (cross-trait)
    #[serde(default)]
    pub scope: OirEventScope,
    /// Payload schema for this event
    #[serde(default)]
    pub payload: Vec<OirPayloadField>,
}

impl OirEmit {
    /// Create a new emit definition
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            event_id: None,
            description: None,
            scope: OirEventScope::External,
            payload: vec![],
        }
    }
}

/// Event scope
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirEventScope {
    /// Internal to this trait
    Internal,
    /// Can be received by other traits
    #[default]
    External,
}

/// Payload field definition for emitted events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPayloadField {
    pub name: String,
    pub field_type: OirType,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Page definition (UI)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPage {
    pub name: String,
    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    /// Declared nav label (`@label`); absent = the page name as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `title:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<crate::schema::types::PageMeta>,
    /// `description:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<crate::schema::types::PageMeta>,
    /// `access:` page modifier; absent = no gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<crate::schema::types::PageAccess>,
    /// `indexing:` page modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<crate::schema::types::PageIndexing>,
    /// The upstream page this one was imported from; pages sharing it in
    /// different locales are language alternates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_page: Option<String>,
    /// `translationOf:` page modifier; groups alternates ahead of `source_page`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_type: Option<OirViewType>,
    #[serde(default)]
    pub sections: Vec<OirSection>,
    #[serde(default)]
    pub traits: Vec<OirTraitBinding>,
    #[serde(default)]
    pub entity_bindings: Vec<OirEntityBinding>,
    #[serde(default)]
    pub navigation: Vec<OirNavigation>,
}

impl OirPage {
    /// Create a new page
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
            label: None,
            path: path.into(),
            title: None,
            description: None,
            access: None,
            indexing: None,
            source_page: None,
            translation_of: None,
            layout: None,
            view_type: None,
            sections: vec![],
            traits: vec![],
            entity_bindings: vec![],
            navigation: vec![],
        }
    }

    /// Find a section by id
    pub fn find_section(&self, id: &str) -> Option<&OirSection> {
        self.sections.iter().find(|s| s.id == id)
    }
}

/// View type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirViewType {
    Dashboard,
    List,
    Detail,
    Create,
    Edit,
    Custom(String),
}

/// Section definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirSection {
    pub id: String,
    pub pattern: OirPattern,
    #[serde(default)]
    pub events: Vec<OirSectionEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

impl OirSection {
    /// Create a new section
    pub fn new(id: impl Into<String>, pattern: OirPattern) -> Self {
        Self {
            id: id.into(),
            pattern,
            events: vec![],
            position: None,
        }
    }
}

/// Pattern definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirPattern {
    /// Pattern type: "entity-table", "form-section", etc.
    pub pattern_type: String,
    /// Pattern configuration (varies by pattern)
    #[serde(default)]
    pub config: OirObject,
}

impl OirPattern {
    /// Create a new pattern
    pub fn new(pattern_type: impl Into<String>) -> Self {
        Self {
            pattern_type: pattern_type.into(),
            config: OirObject::default(),
        }
    }

    /// Add configuration
    pub fn with_config(mut self, key: impl Into<String>, value: OirValue) -> Self {
        self.config.insert(key.into(), value);
        self
    }
}

/// Section event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirSectionEvent {
    /// Trigger: "click", "submit", etc.
    pub trigger: String,
    /// Effect to execute
    pub action: crate::ir::expr::OirExpr,
}

/// Trait binding
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirTraitBinding {
    pub trait_name: String,
    /// V4 dual-carry id sibling of `trait_name` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<OirObject>,
}

impl OirTraitBinding {
    /// Create a new trait binding
    pub fn new(trait_name: impl Into<String>) -> Self {
        Self {
            trait_name: trait_name.into(),
            trait_id: None,
            config: None,
        }
    }
}

/// Entity binding
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirEntityBinding {
    pub entity_name: String,
    /// V4 dual-carry id sibling of `entity_name` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<crate::ir::expr::OirExpr>,
}

impl OirEntityBinding {
    /// Create a new entity binding
    pub fn new(entity_name: impl Into<String>) -> Self {
        Self {
            entity_name: entity_name.into(),
            entity_id: None,
            alias: None,
            filter: None,
        }
    }
}

/// Navigation item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OirNavigation {
    pub label: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// Name -> minted id, sorted by name (deterministic IR wire).
pub type OirIdMap = IndexMap<String, String>;

/// Config knob -> its declared type, in authored order.
pub type OirConfigSchema = IndexMap<String, OirConfigFieldType>;

/// Named types in authored order (deterministic codegen and IR wire).
pub type OirTypeMap = IndexMap<String, OirType>;

/// An IR object: keys in authored order, so codegen and the IR wire are deterministic.
pub type OirObject = IndexMap<String, OirValue>;

/// Universal value type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OirValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<OirValue>),
    Object(OirObject),
}

impl OirValue {
    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Try to get as bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get as number
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to get as string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as array
    pub fn as_array(&self) -> Option<&[OirValue]> {
        match self {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Try to get as object
    pub fn as_object(&self) -> Option<&OirObject> {
        match self {
            Self::Object(obj) => Some(obj),
            _ => None,
        }
    }
}

impl From<bool> for OirValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<f64> for OirValue {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<i64> for OirValue {
    fn from(n: i64) -> Self {
        Self::Number(n as f64)
    }
}

impl From<String> for OirValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for OirValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl<T: Into<OirValue>> From<Vec<T>> for OirValue {
    fn from(arr: Vec<T>) -> Self {
        Self::Array(arr.into_iter().map(Into::into).collect())
    }
}

/// Compilation metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OirMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

// `to_snake_case` is the canonical `crate::str_utils::to_snake_case` (imported
// at module top) — the byte-identical local copy was removed (Pillar F3).

/// Add every service a `call-service` effect inside `e` names to `names`.
pub fn invoked_services_in(
    e: &crate::ir::expr::OirExpr,
    names: &mut std::collections::BTreeSet<String>,
) {
    use {crate::ir::effects::OirEffect, crate::ir::expr::OirExpr};
    if let OirExpr::Effect { effect } = e {
        if let OirEffect::CallService { service, .. } = effect.as_ref() {
            names.insert(service.clone());
        }
    }
    for child in e.child_exprs() {
        invoked_services_in(child, names);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_field_of_a_std_value_type_has_the_type_an_entity_field_of_it_has() {
        for (word, ty) in [("orbital", OirType::Orbital), ("behavior", OirType::Behavior), ("trait", OirType::Trait)] {
            assert_eq!(payload_type_shape(word), ty, "{word}");
            assert_eq!(payload_type_shape(&format!("[{word}]")), OirType::array(ty.clone()), "[{word}]");
            let field: crate::schema::FieldDefinition =
                serde_json::from_value(serde_json::json!({ "name": "f", "type": word })).unwrap();
            assert_eq!(payload_type_shape(word), field_shape(&field), "payload and entity agree on {word}");
        }
        assert_eq!(
            payload_type_shape("Order"),
            OirType::Reference { entity: "Order".to_string() },
            "control: a program name stays a reference"
        );
    }

    #[test]
    fn a_program_type_spelled_like_a_builtin_in_another_case_stays_the_programs_type() {
        for name in ["Orbital", "Trait", "Node", "File", "String", "Event"] {
            assert_eq!(payload_type_shape(name), OirType::Reference { entity: name.to_string() }, "{name}");
        }
        assert_eq!(payload_type_shape("CallKey"), OirType::CallKey, "control: a mixed-case builtin is spelled as the language spells it");
        assert_eq!(payload_type_shape("SExpr"), OirType::SExpr, "control");
        assert_eq!(payload_type_shape("orbital"), OirType::Orbital, "control: the builtin itself");
    }

    #[test]
    fn payload_array_item_type_distinguishes_lists_from_tuples() {
        assert_eq!(payload_array_item_type(" [ string ] "), Some(" string "));
        assert_eq!(payload_array_item_type("[Map<string,number>]"), Some("Map<string,number>"));
        assert_eq!(payload_array_item_type("[[string,number]]"), Some("[string,number]"));
        for non_array in ["[string,number]", "[string,[number]]", "Map<string,number>", "string"] {
            assert_eq!(payload_array_item_type(non_array), None, "{non_array}");
        }
        assert_eq!(payload_type_shape("[string]"), OirType::array(OirType::String));
        let tuple = OirType::Tuple { items: vec![OirType::String, OirType::Number] };
        assert_eq!(payload_type_shape("[string,number]"), tuple);
        assert_eq!(payload_type_shape("[[string,number]]"), OirType::array(tuple));
    }

    #[test]
    fn declared_shape_resolver_keeps_nullable_recursive_aliases_and_excludes_unrelated_types() {
        let reference = |name: &str| OirType::TypeRef { name: name.into() };
        let definitions = std::collections::BTreeMap::from([
            ("Alias", reference("V")),
            ("V", OirType::array(OirType::optional(reference("V")))),
            ("Unused", OirType::Boolean),
        ]);
        let shape = DeclaredShape::with_resolver(Some("Alias".into()), reference("Alias"), |name| {
            definitions.get(name).cloned()
        });
        assert_eq!(
            shape.named.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["Alias", "V"]
        );
        assert_eq!(
            shape.named["V"],
            OirType::array(OirType::optional(reference("V")))
        );
        assert_eq!(
            DeclaredShape::parse(&serde_json::to_string(&shape).unwrap()),
            Some(shape)
        );
    }

    #[test]
    fn type_var_substitution_visits_nested_types_once_and_preserves_metadata() {
        let var = |name: &str| OirType::Var { name: name.into() };
        let mut field = OirField::new("slot", OirType::optional(var("t")));
        field.required = false;
        field.description = Some("keep".into());
        let mut shape = OirType::Tuple { items: vec![
            OirType::map(var("t"), OirType::array(var("t"))),
            OirType::Struct { fields: vec![field.clone()], additional: None },
            OirType::Union { variants: vec![OirField::new("case", var("t"))] },
            OirType::TypeRef { name: "t".into() },
            OirType::reference("t"),
            var("unbound"),
        ] };
        let bindings = std::collections::BTreeMap::from([
            ("t".into(), var("next")),
            ("next".into(), OirType::Number),
        ]);
        assert!(shape.substitute_type_vars(&bindings));
        let OirType::Tuple { items } = &shape else { panic!("tuple preserved") };
        assert_eq!(items[0], OirType::map(var("next"), OirType::array(var("next"))));
        let OirType::Struct { fields, .. } = &items[1] else { panic!("record preserved") };
        field.field_type = OirType::optional(var("next"));
        assert_eq!(fields, &[field]);
        assert_eq!(items[2], OirType::Union { variants: vec![OirField::new("case", var("next"))] });
        assert_eq!(items[3], OirType::TypeRef { name: "t".into() });
        assert_eq!(items[4], OirType::reference("t"));
        assert_eq!(items[5], var("unbound"));
        assert!(!shape.substitute_type_vars(&std::collections::BTreeMap::new()));
        let mut cycle = var("t");
        assert!(cycle.substitute_type_vars(&std::collections::BTreeMap::from([
            ("t".into(), OirType::array(var("t"))),
        ])));
        assert_eq!(cycle, OirType::array(var("t")), "new nodes are not revisited");
        let mut explicit = OirType::map(OirType::String, OirType::reference("Before"));
        assert!(explicit.rewrite(&|ty| match ty {
            OirType::Reference { entity } if entity == "Before" => Some(OirType::reference("After")),
            _ => None,
        }));
        assert_eq!(explicit, OirType::map(OirType::String, OirType::reference("After")));
        assert!(!explicit.rewrite(&|ty| Some(ty.clone())));
    }

    #[test]
    fn declared_shape_contract_version_is_explicit_and_legacy_remains_unmarked() {
        let old = DeclaredShape::parse(r#"{"type":{"kind":"string"}}"#).unwrap();
        assert!(serde_json::to_value(&old).unwrap().get("version").is_none());
        let current = DeclaredShape::parse(r#"{"version":1,"type":{"kind":"string"}}"#).unwrap();
        assert_eq!(serde_json::to_value(&current).unwrap()["version"], 1);
        assert!(DeclaredShape::parse(r#"{"version":2,"type":{"kind":"string"}}"#).is_none());
        let aliases = std::collections::BTreeMap::new();
        let built = DeclaredShape::with_named(None, OirType::String, &aliases);
        assert!(serde_json::to_value(&built).unwrap().get("version").is_none());
        let legacy: crate::schema::OrbitalSchema = serde_json::from_value(serde_json::json!({"name":"old","orbitals":[]})).unwrap();
        assert!(serde_json::to_value(legacy).unwrap().get("typeContractVersion").is_none());
        let current: crate::schema::OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name":"new","orbitals":[],"typeContractVersion":1,
            "types":{"Row":[{"name":"", "type":"object", "nullable":false, "additionalProperties":{"type":"string","nullable":false}}]}
        })).unwrap();
        assert_eq!(current.type_contract_version, Some(1));
        assert!(current.types.as_ref().unwrap().contains_key("Row"));
        assert!(serde_json::from_value::<crate::schema::OrbitalSchema>(serde_json::json!({"name":"bad","orbitals":[],"typeContractVersion":2})).is_err());
    }

    #[test]
    fn payload_container_items_preserve_explicit_nullable_value_contract() {
        for (container, key) in [("Map<string,string>", "value"), ("[string]", "inner")] {
            let field: crate::schema::PayloadField = serde_json::from_value(serde_json::json!({
                "type":container, "nullable":false,
                "items":{"type":"string","nullable":true}
            })).unwrap();
            let shape = serde_json::to_value(payload_field_shape(&field)).unwrap();
            assert_eq!(shape[key]["kind"], "optional", "{container}: {shape}");
            assert_eq!(shape[key]["inner"]["kind"], "string");
            if key == "value" { assert_eq!(shape["key"]["kind"], "string"); }
        }
    }

    #[test]
    fn explicit_nullable_and_additional_values_survive_schema_and_shape_readers() {
        let value = serde_json::json!({
            "type": "object", "nullable": false,
            "properties": {
                "id": {"type":"string", "required":false, "nullable":false},
                "token": {"type":"string", "required":true, "nullable":true}
            },
            "additionalProperties": {"type":"number", "nullable":true}
        });
        let field: FieldDefinition = serde_json::from_value(value).unwrap();
        let wire = serde_json::to_value(&field).unwrap();
        assert_eq!(wire["nullable"], false);
        assert_eq!(wire["additionalProperties"]["nullable"], true);
        let shape = serde_json::to_value(field_shape(&field)).unwrap();
        assert_eq!(shape["kind"], "struct");
        assert_eq!(shape["additional"]["kind"], "optional");
        assert_eq!(shape["additional"]["inner"]["kind"], "number");
        let fields = shape["fields"].as_array().unwrap();
        assert_eq!(fields[0]["required"], false);
        assert_eq!(fields[0]["fieldType"]["kind"], "string");
        assert_eq!(fields[1]["required"], true);
        assert_eq!(fields[1]["fieldType"]["kind"], "optional");
        let payload: crate::schema::PayloadField = serde_json::from_value(serde_json::json!({
            "name":"row", "type":"object", "nullable":false,
            "properties":[{"name":"id", "type":"string", "required":false, "nullable":false}],
            "additionalProperties":{"type":"number", "nullable":true}
        })).unwrap();
        let payload_wire = serde_json::to_value(&payload).unwrap();
        assert_eq!(payload_wire["nullable"], false);
        assert_eq!(payload_wire["additionalProperties"]["nullable"], true);
        let payload_shape = serde_json::to_value(payload_field_shape(&payload)).unwrap();
        assert_eq!(payload_shape["additional"]["kind"], "optional");
        let catchall = OirType::Struct { fields: Vec::new(), additional: Some(Box::new(OirType::TypeRef { name: "Cell".into() })) };
        let closed = DeclaredShape::with_resolver(None, catchall, |name| (name == "Cell").then_some(OirType::String));
        assert_eq!(closed.named.get("Cell"), Some(&OirType::String));
        let mut variable = OirType::Struct { fields: Vec::new(), additional: Some(Box::new(OirType::Var { name: "cell".into() })) };
        assert!(variable.substitute_type_vars(&std::collections::BTreeMap::from([("cell".into(), OirType::String)])));
        assert_eq!(serde_json::to_value(variable).unwrap()["additional"]["kind"], "string");
    }

    fn transition(
        from: OirTransitionFrom,
        event: &str,
        renders: Vec<AwaitRender>,
    ) -> OirTransition {
        OirTransition {
            from,
            to: "done".into(),
            event: event.into(),
            guard: None,
            effects: vec![],
            await_render: renders,
        }
    }

    fn render(trait_name: &str, variant: &str) -> AwaitRender {
        AwaitRender {
            trait_name: trait_name.into(),
            slot: "main".into(),
            skeleton: SkeletonNode::Shape {
                shape: SkeletonShape {
                    variant: variant.into(),
                    rows: Some(3),
                    columns: None,
                    fields: None,
                },
            },
        }
    }

    #[test]
    fn resolved_schema_rows_read_each_transitions_await_render() {
        let list = serde_json::json!({ "shape": { "variant": "list", "rows": 3 } });
        let resolved = serde_json::json!({ "orbitals": [{ "traits": [{
            "name": "Browse",
            "stateMachine": { "transitions": [
                { "from": "idle", "event": "LOAD", "awaitRender": [{ "trait": "Browse", "slot": "main", "skeleton": list }] },
                { "from": ["a", "b"], "event": "SAVE", "awaitRender": [{ "trait": "Browse", "slot": "drawer", "skeleton": list }] },
                { "from": "*", "event": "REFRESH", "awaitRender": [{ "trait": "Browse", "slot": "main", "skeleton": list }] },
                { "from": "idle", "event": "CLOSE" },
                { "from": "idle", "event": "BROKEN", "awaitRender": [{ "slot": "main" }] }
            ] }
        }] }] });
        let rows = await_render_rows_from_resolved(&resolved);
        let keys: Vec<(&str, Option<Vec<String>>, &str)> = rows
            .iter()
            .map(|r| (r.event.as_str(), r.from.clone(), r.slot.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("LOAD", Some(vec!["idle".to_string()]), "main"),
                (
                    "SAVE",
                    Some(vec!["a".to_string(), "b".to_string()]),
                    "drawer"
                ),
                ("REFRESH", None, "main"),
            ],
            "control: a transition with no predictions, or an unreadable one, adds no row"
        );
        assert!(rows.iter().all(|r| r.request_trait == "Browse"));
        assert!(await_render_rows_from_resolved(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn await_render_rows_key_each_prediction_by_the_dispatch_that_leads_to_it() {
        let mut module = OirModule::new("App", "1.0.0");
        let mut browse = OirTrait::new("Browse");
        browse.transitions = vec![
            transition(
                OirTransitionFrom::Single("idle".into()),
                "LOAD",
                vec![render("Browse", "list")],
            ),
            transition(
                OirTransitionFrom::Any,
                "SAVE",
                vec![render("Detail", "detail"), render("Browse", "list")],
            ),
            transition(OirTransitionFrom::Single("idle".into()), "CLOSE", vec![]),
        ];
        module.traits.push(browse);
        let rows = module.await_render_rows();
        let keys: Vec<(&str, &str, Option<Vec<String>>, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.request_trait.as_str(),
                    r.event.as_str(),
                    r.from.clone(),
                    r.trait_name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                ("Browse", "LOAD", Some(vec!["idle".to_string()]), "Browse"),
                ("Browse", "SAVE", None, "Detail"),
                ("Browse", "SAVE", None, "Browse"),
            ],
            "control: a transition with no prediction contributes no row"
        );
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(json["requestTrait"], "Browse");
        assert_eq!(json["trait"], "Browse");
        assert_eq!(json["skeleton"]["shape"]["variant"], "list");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("Task"), "task");
        assert_eq!(to_snake_case("TaskList"), "task_list");
        assert_eq!(to_snake_case("HTTPServer"), "h_t_t_p_server");
    }

    #[test]
    fn test_oir_module_new() {
        let module = OirModule::new("TestApp", "1.0.0");
        assert_eq!(module.name, "TestApp");
        assert_eq!(module.version, "1.0.0");
        assert!(module.entities.is_empty());
    }

    #[test]
    fn test_oir_entity_new() {
        let entity = OirEntity::new("Task");
        assert_eq!(entity.name, "Task");
        assert_eq!(entity.collection, "task");
        assert_eq!(entity.persistence, OirPersistence::Persistent);
    }

    #[test]
    fn test_oir_type_array() {
        let arr_type = OirType::array(OirType::String);
        assert!(matches!(arr_type, OirType::Array { .. }));
        assert_eq!(arr_type.inner_type(), Some(&OirType::String));
    }

    fn bare_field(field_type: FieldType) -> FieldDefinition {
        FieldDefinition {
            nullable: None,
            additional_properties: None,
            name: None,
            field_type,
            required: None,
            primary_key: None,
            unique: None,
            default: None,
            values: None,
            relation: None,
            items: None,
            properties: None,
            intrinsic: None,
            description: None,
            synonyms: None,
            mock: None,
            merged_from: None,
            projected_from: None,
            record: None,
            derived: None,
        }
    }

    #[test]
    fn distinct_entities_keeps_first_of_each_name_in_order() {
        let mut m = OirModule::new("M", "1.0.0".to_string());
        let mut first = OirEntity::new("A");
        first.fields.push(OirField::new("x", OirType::String));
        m.entities.push(first);
        m.entities.push(OirEntity::new("B"));
        m.entities.push(OirEntity::new("A"));
        let names: Vec<_> = m.distinct_entities().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["A", "B"]);
        assert_eq!(m.distinct_entities()[0].fields.len(), 1);
    }

    #[test]
    fn distinct_entities_leaves_unique_names_untouched() {
        let mut m = OirModule::new("M", "1.0.0".to_string());
        m.entities.push(OirEntity::new("A"));
        m.entities.push(OirEntity::new("B"));
        assert_eq!(m.distinct_entities().len(), 2);
        assert_eq!(m.entities.len(), 2);
    }

    #[test]
    fn a_field_knob_shapes_as_a_field_ref_of_its_value_type() {
        let mut typed = bare_field(FieldType::Field);
        typed.record = Some("@entity".to_string());
        typed.items = Some(Box::new(bare_field(FieldType::String)));
        let entity = OirType::Reference { entity: "@entity".to_string() };
        assert_eq!(
            field_shape(&typed),
            OirType::FieldRef { record: Box::new(entity.clone()), value: Some(Box::new(OirType::String)) }
        );
        // control: a field knob that names any field has no value type.
        typed.items = None;
        assert_eq!(
            field_shape(&typed),
            OirType::FieldRef { record: Box::new(entity), value: None }
        );
        // a type variable record is a variable; an alias record is its shape.
        typed.record = Some("$r".to_string());
        assert_eq!(
            field_shape(&typed),
            OirType::FieldRef { record: Box::new(OirType::Var { name: "r".to_string() }), value: None }
        );
        let mut alias = bare_field(FieldType::Field);
        alias.record = Some("Point".to_string());
        alias.properties = Some(HashMap::from([("x".to_string(), bare_field(FieldType::Number))]));
        let OirType::FieldRef { record, .. } = field_shape(&alias) else { panic!("a field reference") };
        assert!(matches!(*record, OirType::Struct { ref fields, .. } if fields.len() == 1 && fields[0].name == "x"));
        assert_eq!(serde_json::to_string(&FieldType::Field).unwrap(), "\"field\"");
        assert_eq!(serde_json::from_str::<FieldType>("\"field\"").unwrap(), FieldType::Field);
        // control: the neighbouring name-carrying tag keeps its own spelling.
        assert_eq!(serde_json::to_string(&FieldType::Event).unwrap(), "\"event\"");
    }

    #[test]
    fn a_bare_payload_object_has_a_record_shape_while_json_stays_unconstrained() {
        let object: crate::schema::types::PayloadField = serde_json::from_value(serde_json::json!({"name": "data", "type": "object"})).unwrap();
        let json: crate::schema::types::PayloadField = serde_json::from_value(serde_json::json!({"name": "data", "type": "json"})).unwrap();
        assert_eq!(payload_field_shape(&object), OirType::Struct { fields: Vec::new(), additional: None });
        assert_eq!(payload_field_shape(&json), OirType::Json);
    }

    #[test]
    fn field_shape_retains_closed_string_and_enum_values_through_nullable_containers() {
        let expected = OirType::enum_type("\"ready\" | \"done\"");
        for kind in [FieldType::String, FieldType::Enum] {
            let mut field = bare_field(kind);
            field.values = Some(vec!["ready".into(), "done".into()]);
            field.nullable = Some(false);
            assert_eq!(field_shape(&field), expected);
            let OirType::Enum { name } = field_shape(&field) else { panic!("closed values retained") };
            let members = crate::type_compat::literal_union_members(&name).unwrap();
            assert!(members.iter().any(|member| member == "ready"));
            assert!(!members.iter().any(|member| member == "wrong"));
            field.nullable = Some(true);
            let nullable = OirType::optional(expected.clone());
            assert_eq!(field_shape(&field), nullable);
            let mut map = bare_field(FieldType::Object);
            map.items = Some(Box::new(field.clone()));
            assert_eq!(field_shape(&map), OirType::map(OirType::String, nullable.clone()));
            let mut record = bare_field(FieldType::Object);
            record.properties = Some(std::collections::HashMap::new());
            record.additional_properties = Some(Box::new(field));
            assert_eq!(field_shape(&record), OirType::Struct { fields: Vec::new(), additional: Some(Box::new(nullable)) });
        }
        assert_eq!(field_shape(&bare_field(FieldType::String)), OirType::String);
        assert_eq!(field_shape(&bare_field(FieldType::Enum)), OirType::enum_type("Enum"));
    }

    #[test]
    fn field_shape_primitive() {
        assert_eq!(field_shape(&bare_field(FieldType::String)), OirType::String);
        assert_eq!(field_shape(&bare_field(FieldType::Number)), OirType::Number);
        assert_eq!(field_shape(&bare_field(FieldType::Money)), OirType::Money);
    }

    #[test]
    fn field_shape_int_is_integer_and_number_stays_number() {
        assert_eq!(field_shape(&bare_field(FieldType::Int)), OirType::Integer);
        let mut nullable = bare_field(FieldType::Int);
        nullable.nullable = Some(true);
        assert_eq!(field_shape(&nullable), OirType::optional(OirType::Integer));
        assert_eq!(field_shape(&bare_field(FieldType::Number)), OirType::Number);
    }

    #[test]
    fn field_shape_array_recurses_into_items() {
        let mut f = bare_field(FieldType::Array);
        f.items = Some(Box::new(bare_field(FieldType::String)));
        assert_eq!(
            field_shape(&f),
            OirType::Array {
                inner: Box::new(OirType::String)
            }
        );
    }

    #[test]
    fn field_shape_bare_array_is_array_of_json() {
        // No `items` at all — the honest "no element information" shape.
        assert_eq!(
            field_shape(&bare_field(FieldType::Array)),
            OirType::Array {
                inner: Box::new(OirType::Json)
            }
        );
    }

    #[test]
    fn field_shape_object_with_properties_is_the_struct_of_them() {
        let mut f = bare_field(FieldType::Object);
        let mut props = std::collections::HashMap::new();
        props.insert("label".to_string(), bare_field(FieldType::String));
        props.insert("count".to_string(), bare_field(FieldType::Number));
        f.properties = Some(props);
        let OirType::Struct { fields, .. } = field_shape(&f) else {
            panic!("an object with properties is a struct");
        };
        let named: Vec<(&str, &OirType)> = fields.iter().map(|f| (f.name.as_str(), &f.field_type)).collect();
        assert_eq!(named, vec![("count", &OirType::Number), ("label", &OirType::String)]);
    }

    #[test]
    fn field_shape_object_with_items_is_a_map_of_the_item_type() {
        let mut f = bare_field(FieldType::Object);
        f.items = Some(Box::new(bare_field(FieldType::Number)));
        assert_eq!(field_shape(&f), OirType::map(OirType::String, OirType::Number));
    }

    #[test]
    fn field_shape_keeps_a_recursive_back_reference_as_the_type_it_names() {
        // `type Node = { children : [Node] }`: the inner `Node` is the by-name form.
        let mut back = bare_field(FieldType::Union);
        back.values = Some(vec!["Node".to_string()]);
        let mut children = bare_field(FieldType::Array);
        children.items = Some(Box::new(back.clone()));
        assert_eq!(back.by_name_reference(), Some("Node"));
        assert_eq!(
            field_shape(&children),
            OirType::array(OirType::TypeRef { name: "Node".to_string() })
        );
    }

    #[test]
    fn field_shape_of_a_union_carries_its_variants_sorted() {
        let mut union = bare_field(FieldType::Union);
        let mut props = std::collections::HashMap::new();
        props.insert("string".to_string(), bare_field(FieldType::String));
        props.insert("number".to_string(), bare_field(FieldType::Number));
        union.properties = Some(props);
        let OirType::Union { variants } = field_shape(&union) else {
            panic!("a union with variants is a union");
        };
        let named: Vec<(&str, &OirType)> = variants.iter().map(|v| (v.name.as_str(), &v.field_type)).collect();
        assert_eq!(named, vec![("number", &OirType::Number), ("string", &OirType::String)]);
        assert_eq!(field_shape(&bare_field(FieldType::Union)), OirType::Json, "no variants, no shape");
    }

    #[test]
    fn a_union_with_variant_shapes_is_not_a_by_name_reference() {
        let mut tagged = bare_field(FieldType::Union);
        tagged.values = Some(vec!["A".to_string()]);
        let mut props = std::collections::HashMap::new();
        props.insert("A".to_string(), bare_field(FieldType::Object));
        tagged.properties = Some(props);
        assert_eq!(tagged.by_name_reference(), None);
        assert_eq!(bare_field(FieldType::Union).by_name_reference(), None);
        let mut two = bare_field(FieldType::Union);
        two.values = Some(vec!["A".to_string(), "B".to_string()]);
        assert_eq!(two.by_name_reference(), None, "a name list is not one reference");
    }

    #[test]
    fn field_shape_relation_with_definition_is_reference() {
        let mut f = bare_field(FieldType::Relation);
        f.relation = Some(crate::schema::types::RelationDefinition {
            entity: "Article".to_string(),
            entity_id: None,
            cardinality: None,
            on_delete: None,
        });
        assert_eq!(
            field_shape(&f),
            OirType::Reference {
                entity: "Article".to_string()
            }
        );
    }

    fn relation_field(cardinality: Option<crate::schema::types::Cardinality>) -> FieldDefinition {
        let mut f = bare_field(FieldType::Relation);
        f.relation = Some(crate::schema::types::RelationDefinition {
            entity: "Revision".to_string(),
            entity_id: None,
            cardinality,
            on_delete: None,
        });
        f
    }

    #[test]
    fn field_shape_to_many_relation_is_array_of_reference() {
        use crate::schema::types::Cardinality::*;
        for c in [Many, OneToMany, ManyToMany] {
            assert_eq!(
                field_shape(&relation_field(Some(c))),
                OirType::array(OirType::reference("Revision"))
            );
        }
    }

    #[test]
    fn field_shape_intrinsic_to_many_relation_is_array_of_row() {
        use crate::schema::types::Cardinality::*;
        for c in [Many, OneToMany, ManyToMany] {
            let mut f = relation_field(Some(c));
            f.intrinsic = Some(true);
            assert_eq!(field_shape(&f), OirType::array(OirType::row("Revision")));
        }
    }

    #[test]
    fn field_shape_explicitly_non_intrinsic_to_many_relation_stays_ids() {
        let mut f = relation_field(Some(crate::schema::types::Cardinality::OneToMany));
        f.intrinsic = Some(false);
        assert_eq!(
            field_shape(&f),
            OirType::array(OirType::reference("Revision"))
        );
    }

    /// `loadedRow : RecordItem @intrinsic` holds the fetched row (LOLO reference), not its id.
    #[test]
    fn field_shape_intrinsic_to_one_relation_is_a_row() {
        let mut f = relation_field(Some(crate::schema::types::Cardinality::ManyToOne));
        f.intrinsic = Some(true);
        assert_eq!(field_shape(&f), OirType::row("Revision"));
    }

    #[test]
    fn row_serializes_under_its_own_kind() {
        let v = serde_json::to_value(OirType::row("Revision")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "kind": "row", "entity": "Revision" })
        );
        assert_ne!(
            v,
            serde_json::to_value(OirType::reference("Revision")).unwrap()
        );
    }

    #[test]
    fn field_shape_to_one_relation_stays_reference() {
        use crate::schema::types::Cardinality::*;
        for c in [None, Some(One), Some(ManyToOne)] {
            assert_eq!(
                field_shape(&relation_field(c)),
                OirType::reference("Revision")
            );
        }
    }

    #[test]
    fn field_shape_relation_without_definition_degrades_to_json() {
        assert_eq!(field_shape(&bare_field(FieldType::Relation)), OirType::Json);
    }

    #[test]
    fn test_oir_value_conversions() {
        let v: OirValue = true.into();
        assert_eq!(v.as_bool(), Some(true));

        let v: OirValue = 42.0.into();
        assert_eq!(v.as_number(), Some(42.0));

        let v: OirValue = "hello".into();
        assert_eq!(v.as_str(), Some("hello"));
    }

    #[test]
    fn test_transition_from_matches() {
        let single = OirTransitionFrom::Single("idle".to_string());
        assert!(single.matches("idle"));
        assert!(!single.matches("running"));

        let multiple = OirTransitionFrom::Multiple(vec!["a".to_string(), "b".to_string()]);
        assert!(multiple.matches("a"));
        assert!(multiple.matches("b"));
        assert!(!multiple.matches("c"));

        let any = OirTransitionFrom::Any;
        assert!(any.matches("anything"));
    }

    #[test]
    fn test_oir_entity_shared_round_trips_true() {
        let mut entity = OirEntity::new("Session");
        entity.shared = true;

        let serialized = serde_json::to_string(&entity).unwrap();
        assert!(serialized.contains("\"shared\":true"));

        let deserialized: OirEntity = serde_json::from_str(&serialized).unwrap();
        assert!(deserialized.shared);
    }

    #[test]
    fn test_oir_entity_shared_defaults_false_when_absent() {
        let json = r#"{
            "name": "Task",
            "collection": "task",
            "persistence": "persistent",
            "fields": []
        }"#;
        let entity: OirEntity = serde_json::from_str(json).unwrap();
        assert!(!entity.shared);
    }

    #[test]
    fn test_oir_entity_shared_false_omitted_from_serialization() {
        let entity = OirEntity::new("Task");
        assert!(!entity.shared);
        let serialized = serde_json::to_string(&entity).unwrap();
        assert!(!serialized.contains("shared"));
    }

    #[test]
    fn test_oir_field_intrinsic_round_trips_true() {
        let mut field = OirField::new("viewState", OirType::String);
        field.intrinsic = true;
        let serialized = serde_json::to_string(&field).unwrap();
        assert!(serialized.contains("\"intrinsic\":true"));
        let deserialized: OirField = serde_json::from_str(&serialized).unwrap();
        assert!(deserialized.intrinsic);
    }

    #[test]
    fn test_oir_field_intrinsic_defaults_false_when_absent() {
        let json = r#"{ "name": "x", "fieldType": { "kind": "string" } }"#;
        let field: OirField = serde_json::from_str(json).unwrap();
        assert!(!field.intrinsic);
    }

    #[test]
    fn test_oir_field_intrinsic_false_omitted_from_serialization() {
        let field = OirField::new("x", OirType::String);
        let serialized = serde_json::to_string(&field).unwrap();
        assert!(!serialized.contains("intrinsic"));
    }

    #[test]
    fn test_oir_entity_frame_fields_lists_only_intrinsic_names() {
        let mut entity = OirEntity::new("Wizard");
        entity.fields.push(OirField::new("id", OirType::String));
        let mut frame_field = OirField::new("currentStepIndex", OirType::Integer);
        frame_field.intrinsic = true;
        entity.fields.push(frame_field);
        assert_eq!(entity.frame_fields(), vec!["currentStepIndex"]);
    }

    #[test]
    fn test_oir_entity_required_create_field_names_excludes_stamped_intrinsic_and_defaulted() {
        let mut entity = OirEntity::new("Note");
        entity.fields.push(OirField::new("id", OirType::String)); // FRAMEWORK_STAMPED_FIELDS
        entity
            .fields
            .push(OirField::new("content", OirType::String)); // the real required column
        entity
            .fields
            .push(OirField::optional("summary", OirType::String)); // not required at all
        let mut frame_field = OirField::new("workingCopy", OirType::String);
        frame_field.intrinsic = true;
        entity.fields.push(frame_field); // required but @intrinsic
        let mut defaulted = OirField::new("priority", OirType::String);
        defaulted.default = Some(OirValue::String("normal".to_string()));
        entity.fields.push(defaulted); // required but declares a default
        assert_eq!(entity.required_create_field_names(), vec!["content"]);
    }

    #[test]
    fn test_oir_entity_required_create_field_names_excludes_merged_from() {
        // ChatMessage.content is host-authored and required; ChatMessage.name
        // was added by the inline phase's rebind merge from BrowseItem and
        // must NOT count as a required create column — BrowseItem's `!` is
        // its own write contract, never ChatMessage's.
        let mut entity = OirEntity::new("ChatMessage");
        entity
            .fields
            .push(OirField::new("content", OirType::String));
        let mut merged = OirField::new("name", OirType::String);
        merged.merged_from = Some("BrowseItem".to_string());
        entity.fields.push(merged);
        assert_eq!(entity.required_create_field_names(), vec!["content"]);
    }

    #[test]
    fn test_oir_trait_dispatch_round_trips_declared_mode() {
        let mut t = OirTrait::new("Toggle");
        t.dispatch = Some(crate::schema::types::DispatchMode::Client);
        let serialized = serde_json::to_string(&t).unwrap();
        assert!(serialized.contains("\"dispatch\":\"client\""));
        let deserialized: OirTrait = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.dispatch, Some(crate::schema::types::DispatchMode::Client));
    }

    #[test]
    fn test_oir_trait_dispatch_defaults_absent() {
        let t = OirTrait::new("Toggle");
        assert_eq!(t.dispatch, None);
        let serialized = serde_json::to_string(&t).unwrap();
        assert!(!serialized.contains("\"dispatch\""));
        let deserialized: OirTrait = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.dispatch, None);
    }

    #[test]
    fn test_dispatch_mode_runtime_entity_without_declaration_is_server() {
        let mut entity = OirEntity::new("Cursor");
        entity.persistence = OirPersistence::Runtime;
        let t = OirTrait::new("Move");
        assert_eq!(
            t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
            OirDispatchMode::PersistedAwaited
        );
    }

    #[test]
    fn test_dispatch_mode_client_is_client_only_whatever_the_entity() {
        let mut runtime = OirEntity::new("Cursor");
        runtime.persistence = OirPersistence::Runtime;
        let persistent = OirEntity::new("Draft");
        let mut t = OirTrait::new("Move");
        t.dispatch = Some(crate::schema::types::DispatchMode::Client);
        for entity in [Some(&runtime), Some(&persistent), None] {
            assert_eq!(
                t.dispatch_mode(entity, &crate::runs_on::Residence::none()),
                OirDispatchMode::HybridClientOnly
            );
        }
    }

    #[test]
    fn test_dispatch_mode_optimistic_is_declared() {
        let entity = OirEntity::new("Draft");
        let mut t = OirTrait::new("Edit");
        t.dispatch = Some(crate::schema::types::DispatchMode::Optimistic);
        assert_eq!(
            t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
            OirDispatchMode::RuntimeOptimistic
        );
    }

    #[test]
    fn test_dispatch_mode_persisted_awaited_by_default() {
        let entity = OirEntity::new("Order");
        let t = OirTrait::new("Checkout");
        assert_eq!(
            t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
            OirDispatchMode::PersistedAwaited
        );
        assert_eq!(
            t.dispatch_mode(None, &crate::runs_on::Residence::none()),
            OirDispatchMode::PersistedAwaited
        );
    }

    #[test]
    fn optional_constructor_is_idempotent() {
        let once = OirType::optional(OirType::String);
        let twice = OirType::optional(once.clone());
        assert_eq!(once, twice);
        assert_eq!(
            twice,
            OirType::Optional {
                inner: Box::new(OirType::String)
            }
        );
    }

    #[test]
    fn effective_type_unwraps_exactly_one_optional_layer() {
        // Via the `OirField::optional` constructor (required:false + Optional wrapper).
        let via_constructor = OirField::optional("x", OirType::String);
        assert_eq!(via_constructor.effective_type(), (&OirType::String, true));

        // Via a struct literal recording optionality only as `required:false`
        // (the shape an event-payload field carries).
        let via_required_flag = OirField {
            declared: None,
            field_type: OirType::String,
            required: false,
            ..OirField::new("x", OirType::String)
        };
        assert_eq!(via_required_flag.effective_type(), (&OirType::String, true));

        let required = OirField::new("x", OirType::String);
        assert_eq!(required.effective_type(), (&OirType::String, false));
    }
}

#[cfg(test)]
mod tuple_field_tests {
    use super::*;
    use crate::schema::types::FieldDefinition;

    fn tuple_field() -> FieldDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "position",
            "type": "tuple",
            "properties": { "2": { "type": "string" }, "0": { "type": "number" }, "10": { "type": "boolean" }, "1": { "type": "number" } }
        }))
        .unwrap()
    }

    /// A tuple's slots ride `properties` keyed by index; they read back in slot order
    /// (numeric, so `10` follows `2`) and shape as `OirType::Tuple`.
    #[test]
    fn a_tuple_field_reads_its_slots_in_order() {
        let f = tuple_field();
        let kinds: Vec<FieldType> = f
            .tuple_slots()
            .iter()
            .map(|s| s.field_type.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                FieldType::Number,
                FieldType::Number,
                FieldType::String,
                FieldType::Boolean
            ]
        );
        assert_eq!(
            field_shape(&f),
            OirType::Tuple {
                items: vec![
                    OirType::Number,
                    OirType::Number,
                    OirType::String,
                    OirType::Boolean
                ]
            }
        );
    }
}

#[cfg(test)]
mod interval_wire_tests {
    use super::OirInterval;

    /// G-ORBITAL-CORE-001: `orb` sends the module to its shell program as JSON; a cron tick
    /// must survive that trip (an internally tagged newtype variant cannot be serialized).
    #[test]
    fn a_cron_interval_round_trips_as_json() {
        let cron = OirInterval::Cron { expression: "0 9 * * *".to_string() };
        let json = serde_json::to_value(&cron).expect("a cron interval serializes");
        assert_eq!(json, serde_json::json!({ "type": "cron", "expression": "0 9 * * *" }));
        assert_eq!(serde_json::from_value::<OirInterval>(json).unwrap(), cron);
    }

    #[test]
    fn control_the_fixed_intervals_keep_their_wire_form() {
        for (interval, json) in [
            (OirInterval::Milliseconds { value: 5 }, serde_json::json!({ "type": "milliseconds", "value": 5 })),
            (OirInterval::Seconds { value: 2 }, serde_json::json!({ "type": "seconds", "value": 2 })),
            (OirInterval::Minutes { value: 1 }, serde_json::json!({ "type": "minutes", "value": 1 })),
            (OirInterval::Frame, serde_json::json!({ "type": "frame" })),
        ] {
            assert_eq!(serde_json::to_value(&interval).unwrap(), json);
            assert_eq!(serde_json::from_value::<OirInterval>(json).unwrap(), interval);
        }
    }
}
