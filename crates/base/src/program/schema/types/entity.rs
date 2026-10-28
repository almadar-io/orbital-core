//! Entity definitions, fields, field types and relations.

use super::*;

// =============================================================================
// Entity Definition
// =============================================================================

/// Row columns the store mints itself on `persist create` — never authored
/// input, so never a candidate REQUIRED column regardless of a `required:
/// true` declaration. The single owner of this list: before this constant,
/// `runtime/seed.rs`, `orbital-shell-typescript`'s `backend/shared.rs`, and
/// the JS interpreter (`packages/almadar-core/src/mock/sampleValue.ts`,
/// `packages/almadar-runtime/src/{EffectExecutor,MockPersistenceAdapter}.ts`)
/// each carried their own literal `["id", "createdAt", "updatedAt"]` copy —
/// consume this one instead of adding another.
pub const FRAMEWORK_STAMPED_FIELDS: &[&str] = &["id", "createdAt", "updatedAt"];

/// Entity definition - the data schema for an orbital
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDefinition {
    /// Entity name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Database collection name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,

    /// Persistence type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,

    /// `[store: Driver]` — the store-driver orbital that answers this entity's row asks (EXEC-STORE-4);
    /// wins over the app's `store`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,

    /// Field definitions
    pub fields: Vec<FieldDefinition>,

    /// Pre-authored instances (seed data / static reference data)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<crate::schema::sexpr::SObject>>,

    /// Shared across all bound traits (vs per-trait copy); orthogonal to persistence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,

    /// Types the ambient `@user` viewer. Orthogonal to both persistence and
    /// `shared`: `[persistent: people, identity]` is an app-owned user
    /// directory, `[runtime, identity]` the provider-supplied current viewer.
    /// At most one per composed program.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity: bool,

    /// `[persistent: x, browser]` — the rows live in the browser (IndexedDB), so
    /// data effects on this entity run client-side. Twin of `@almadar/core`
    /// `OrbitalEntity.browser`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,

    /// A browser-stored entity's first rows per declared locale (`instances ar [ … ]`);
    /// a locale with no block takes `instances`.
    #[serde(
        default,
        rename = "localeInstances",
        skip_serializing_if = "Option::is_none"
    )]
    pub locale_instances:
        Option<std::collections::BTreeMap<String, Vec<crate::schema::sexpr::SObject>>>,

    /// `@read` — a per-row predicate (`@entity` = candidate row, `@user` =
    /// viewer) ANDed with any call-site `filter:`. The un-bypassable twin of
    /// `filter:`, enforced once for every trait that fetches this entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_policy: Option<SExpression>,

    /// `@create` — `@entity` binds the incoming data about to become a row
    /// (no row exists yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_policy: Option<SExpression>,

    /// `@update` — `@entity` binds the existing row before mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<SExpression>,

    /// `@delete` — same binding shape as `@update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_policy: Option<SExpression>,

    /// `@<op> none "<reason>"` — the omission of a directive, DECLARED.
    ///
    /// Runtime behaviour is identical to simply leaving the directive out
    /// (an absent policy is allow-all). The point is to record that the author
    /// weighed it and concluded no per-row predicate can express the rule —
    /// typically because visibility depends on a DIFFERENT entity, which a
    /// per-row predicate cannot join across. Without this, a lint has no way to
    /// tell a considered omission from a forgotten one, so it must flag both;
    /// flagging correct code forever is how a signal gets ignored.
    ///
    /// Never a way to silence a policy that IS expressible — the reason is
    /// mandatory and gets read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_waivers: Option<AccessWaivers>,

    /// `@invariant <expr>`: every stored row satisfies each, proved by `orb validate` over the program's writes. No
    /// runtime effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invariants: Option<Vec<SExpression>>,

    /// `@index { fields, orderBy? }`: declared access paths. A path changes a query's cost, never its result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexes: Option<Vec<EntityIndex>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<bool>,
    #[serde(default, rename = "softDelete", skip_serializing_if = "Option::is_none")]
    pub soft_delete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_prompt: Option<String>,
    #[serde(default, rename = "assetRef", skip_serializing_if = "Option::is_none")]
    pub asset_ref: Option<SemanticAssetRef>,
}

/// One declared access path: the equality `fields` in order, and the `orderBy` string a `fetch` takes
/// (`"field"` / `"field:desc"`); absent means EXEC-FETCH-1's default order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EntityIndex {
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<String>,
}

/// Twin of `@almadar/core`'s entity asset metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticAssetRef {
    pub role: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<VisualStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension: Option<AssetDimension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<AssetAspect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisualStyle {
    Pixel,
    Vector,
    Hd,
    #[serde(rename = "1-bit")]
    OneBit,
    Isometric,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AssetDimension {
    #[serde(rename = "2d")]
    TwoDimensional,
    #[serde(rename = "3d")]
    ThreeDimensional,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AssetAspect {
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "16:9")]
    Widescreen,
    #[serde(rename = "5:7")]
    Card,
    #[serde(rename = "8:1")]
    EffectStrip,
    #[serde(rename = "4:1")]
    WideBar,
    #[serde(rename = "5:3")]
    WideScene,
    #[serde(rename = "1:2")]
    TallBanner,
}

impl EntityDefinition {
    /// Names of this entity's `persist create`-required columns: a field declaring `required: true`, excluding
    /// [`FRAMEWORK_STAMPED_FIELDS`], any `@intrinsic` field, any field declaring a `default` (the store fills it in),
    /// and any field carrying `merged_from` (added by the inline phase's rebind merge from an imported atom's entity,
    /// never authored by this entity's own writer). The one definition; the kernel and the validator both read it.
    pub fn required_create_field_names(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|f| f.required == Some(true))
            .filter(|f| f.intrinsic != Some(true))
            .filter(|f| f.default.is_none())
            .filter(|f| f.merged_from.is_none())
            .filter_map(|f| f.name.as_ref())
            .filter(|name| !FRAMEWORK_STAMPED_FIELDS.contains(&name.as_str()))
            .cloned()
            .collect()
    }

    /// The rows the entity starts with for a viewer's locale: its locale block, else
    /// `instances`. Twin of `@almadar/core` `instancesForLocale`.
    pub fn instances_for_locale(&self, locale: Option<&str>) -> &[crate::schema::sexpr::SObject] {
        locale
            .and_then(|l| self.locale_instances.as_ref()?.get(l))
            .or(self.instances.as_ref())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Rows live in the client: `[runtime]` (in memory) or `[persistent: x, browser]`
    /// (the browser's IndexedDB). Twin of `@almadar/core` `isClientResident`.
    pub fn is_client_resident(&self) -> bool {
        self.browser || matches!(self.persistence, Some(Persistence::Runtime))
    }
}

/// Why a declared access directive is deliberately absent, per operation.
/// Each value is the author's stated reason; `None` means "not waived".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct AccessWaivers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

/// Persistence type for entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Persistence {
    /// Stored in database
    #[default]
    Persistent,
    /// In-memory only
    Runtime,
}

/// Field definition within an entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    /// `field R T`: the record `R` as a type string (`@entity`, `@entity(<knob>)`, `@payload(<knob>)`, an entity or
    /// alias name, `$r`). A record alias carries its shape in `properties`. On an `object` with no `properties`, the
    /// record is the object's own shape: `@payload(.<sibling>)` is the declared payload of the event the sibling
    /// member of the same struct names (`.lolo` `payload : @payload event`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(default, rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<Box<FieldDefinition>>,
    /// Field name (optional for nested items/properties)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Field type
    #[serde(rename = "type")]
    pub field_type: FieldType,

    /// Whether the field is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,

    /// Whether this is a primary key
    #[serde(rename = "primaryKey", skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<bool>,

    /// Whether this field must be unique
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,

    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<SExpression>,

    /// Enum values (for enum type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,

    /// Relation definition (for relation type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<RelationDefinition>,

    /// Array item type (for array type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<FieldDefinition>>,

    /// Object properties (for object type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<crate::collections::IndexMap<String, FieldDefinition>>,

    /// Field-level rebindability designation. `Some(true)` marks the field as
    /// INTRINSIC machinery/view-state the owning trait computes itself (e.g. a
    /// step-flow's `currentStepIndex`, a board's `boards`/`currentId`). Intrinsic
    /// fields travel WITH the trait onto whatever entity it binds and are NOT
    /// required to pre-exist on a rebind target. Absent / `None` = DATA field:
    /// the consumer supplies it; a rebind target must satisfy it (present, or
    /// remapped via the `fields` override). Authored in `.lolo` as a trailing
    /// `@intrinsic` annotation on the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intrinsic: Option<bool>,

    /// `@derived <expr>`: a stored computed field. The kernel evaluates the expression on every write (create,
    /// update, upsert, batch) after the row's own fields are set and before policies and invariants check, with
    /// `@entity` bound to the row; the store keeps the value like any field. Never written by `set` or `persist`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<SExpression>,

    /// Human/semantic description — authored in `.lolo` as a trailing
    /// `@description "..."` annotation. Authoring/build-time metadata that flows
    /// to the factory-signature catalog, embeddings, and the curation field
    /// matcher; the runtime ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (free text) — authored in `.lolo` as a trailing
    /// `@synonyms "..."` annotation. Feeds catalog search / curation matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Declared mock value(s) — authored in `.lolo` as a trailing
    /// `@mock "..."` annotation, used LITERALLY by the seeder (`seed.rs`,
    /// `sampleValue.ts`), never mapped through a domain-name lookup table —
    /// the generator has no built-in idea what "person-name" or any other
    /// word means, only the author does. Comma-separated is an explicit
    /// candidate list rotated by row index (`@mock "Acme Corp, Globex Inc"`);
    /// no comma is a single literal value, index-suffixed per row
    /// (`@mock "person-name"` -> "person-name-1", "person-name-2", ...).
    /// Adding support for a new kind of mock data is never a generator change
    /// on either side — the author just writes different literal values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,

    /// Name of the imported entity this field was copied FROM by the inline
    /// phase's rebind merge (`merge_imported_entity_fields`), when this field
    /// was added to the host rather than authored by it. `None` for every
    /// field the host itself declares, even if an atom also declares a field
    /// of the same name (the host's own declaration always wins on collision
    /// and is never merged). A merged field's `required: true` is the
    /// IMPORTED atom's own write contract, never the host writer's — see
    /// `required_create_field_names`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "mergedFrom"
    )]
    pub merged_from: Option<String>,

    /// The declared field this field's type was projected from — `.lolo`
    /// `T.f` in any type position (`tagId : NoteTagRef.id`). The field
    /// carries `T.f`'s declared type itself; this records where it came from,
    /// so a value of it can be drawn from `T`'s own `f` (the mock seeder draws
    /// from `T`'s seeded rows). `None` for a field typed any other way.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
}

impl FieldDefinition {
    /// The sibling member whose value names the event this field is the payload of (`@payload(.<sibling>)`).
    pub fn payload_sibling(&self) -> Option<&str> {
        if self.field_type != FieldType::Object || self.properties.is_some() {
            return None;
        }
        self.record.as_deref()?.strip_prefix("@payload(.")?.strip_suffix(')')
    }

    /// The type name this field refers to by name: the recursive back-reference the lolo lowerer leaves where a
    /// self-referential alias would otherwise expand forever (`type Node = { children : [Node] }`). It is a
    /// `union` carrying the one name in `values` and no `properties`; a tagged union carries its variants'
    /// shapes in `properties`.
    pub fn by_name_reference(&self) -> Option<&str> {
        if self.field_type != FieldType::Union || self.properties.is_some() {
            return None;
        }
        match self.values.as_deref() {
            Some([name]) => Some(name.as_str()),
            _ => None,
        }
    }

    /// A `tuple` field's slots, in slot order (`properties` keyed `"0"`, `"1"`, …).
    pub fn tuple_slots(&self) -> Vec<&FieldDefinition> {
        tuple_slots_of(self.properties.as_ref())
    }

    /// The variant of this tagged union a value belongs to (the LOLO tagged-union rule). A variant is chosen by its
    /// tags (each single-valued field that is not a by-name reference) when the value carries them all; a value
    /// carrying no variant's tags selects the one untagged variant whose closed shape it fits (every key declared;
    /// on a tie, every required member present). None or several is never a guess. `keys` are the value's keys;
    /// `read_tag(field, tag)` reads the value's `field` against `tag`.
    pub fn select_union_variant<'d>(
        &'d self,
        keys: &[&str],
        read_tag: impl Fn(&str, &str) -> TagReading,
    ) -> UnionVariantChoice<'d> {
        let (Some(names), Some(variants)) = (&self.values, &self.properties) else {
            return UnionVariantChoice::Malformed(
                "a union definition declares its variants (values + properties)".to_string(),
            );
        };
        let mut untagged: Vec<(&str, &FieldDefinition)> = Vec::new();
        let mut deferred = false;
        for name in names {
            let Some(variant) = variants.get(name) else {
                return UnionVariantChoice::Malformed(format!(
                    "the union names variant {name} but does not declare it"
                ));
            };
            let Some(props) = &variant.properties else {
                continue;
            };
            let tags: Vec<(&str, &str)> = props
                .iter()
                .filter_map(|(field, d)| match d.values.as_deref() {
                    Some([only]) if d.by_name_reference().is_none() => {
                        Some((field.as_str(), only.as_str()))
                    }
                    _ => None,
                })
                .collect();
            if tags.is_empty() {
                untagged.push((name, variant));
                continue;
            }
            let mut agreed = 0usize;
            for (field, tag) in &tags {
                match read_tag(field, tag) {
                    TagReading::Equal => agreed += 1,
                    TagReading::LateBound => deferred = true,
                    TagReading::Differs => {}
                }
            }
            if agreed == tags.len() {
                return UnionVariantChoice::Matched(variant);
            }
        }
        if deferred {
            return UnionVariantChoice::Deferred;
        }
        let declares_every_key = |v: &FieldDefinition| {
            v.properties
                .as_ref()
                .is_some_and(|props| keys.iter().all(|k| props.contains_key(*k)))
        };
        let mut fits: Vec<(&str, &FieldDefinition)> = untagged
            .into_iter()
            .filter(|(_, v)| declares_every_key(v))
            .collect();
        if fits.len() > 1 {
            let complete: Vec<(&str, &FieldDefinition)> = fits
                .iter()
                .copied()
                .filter(|(_, v)| {
                    v.properties.as_ref().is_some_and(|props| {
                        props
                            .iter()
                            .all(|(k, d)| d.required != Some(true) || keys.contains(&k.as_str()))
                    })
                })
                .collect();
            if !complete.is_empty() {
                fits = complete;
            }
        }
        match fits.as_slice() {
            [(_, one)] => UnionVariantChoice::Matched(one),
            [] => UnionVariantChoice::NoMatch,
            many => {
                let mut names: Vec<String> = many.iter().map(|(n, _)| n.to_string()).collect();
                names.sort_unstable();
                UnionVariantChoice::Ambiguous(names)
            }
        }
    }
}

/// How a value's discriminator field reads against one variant tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagReading {
    Equal,
    Differs,
    /// The field holds a late-bound reference (`@config.shape`): which variant applies is not decidable yet.
    LateBound,
}

/// The outcome of [`FieldDefinition::select_union_variant`].
#[derive(Debug)]
pub enum UnionVariantChoice<'d> {
    Matched(&'d FieldDefinition),
    /// A tag is late-bound, so the variant is decided later, never guessed now.
    Deferred,
    NoMatch,
    /// Several untagged variants fit the value equally (sorted names); the union needs a discriminator.
    Ambiguous(Vec<String>),
    /// The union definition itself is ill-formed.
    Malformed(String),
}

/// Tuple slots carried in a `properties` map keyed by slot index, in slot order — the
/// one reader for a tuple field and a tuple-typed config knob alike.
pub fn tuple_slots_of(
    properties: Option<&crate::collections::IndexMap<String, FieldDefinition>>,
) -> Vec<&FieldDefinition> {
    let mut slots: Vec<(usize, &FieldDefinition)> = properties
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
        .collect();
    slots.sort_by_key(|(i, _)| *i);
    slots.into_iter().map(|(_, v)| v).collect()
}

/// Authored type `T`, or its field `T.f` when `field` is present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldProjection {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_projected_field")]
    pub field: Option<String>,
}

pub(super) fn deserialize_projected_field<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let field = String::deserialize(deserializer)?;
    if field.is_empty() {
        return Err(serde::de::Error::custom("a projected field must be nonempty; omit field for a named type"));
    }
    Ok(Some(field))
}

/// Field type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Number,
    /// Whole number. Distinct from `Number` in the type relation: a number does
    /// not narrow into an `int` slot. `float` stays `Number`.
    Int,
    Boolean,
    Date,
    Datetime,
    Timestamp,
    /// Epoch milliseconds the clock produced; a number at rest.
    Instant,
    /// Semantic string domains. `.lolo` already types `date`/`datetime` rather
    /// than annotating them, so a field's domain belongs in the type system for
    /// the same reason. These replaced the TS-only `format` property, which had
    /// no Rust counterpart at all and therefore no way to reach `.lolo`.
    Email,
    Url,
    Phone,
    Uuid,
    Image,
    /// Monetary amount. Numeric at rest (plain number); the type — not the
    /// field name — drives currency-formatted display and currency inputs.
    /// Was a grammar token that silently collapsed to `String` (the schema
    /// lied about the value's shape); promoted 2026-08-20.
    Money,
    /// Uploaded file/attachment. A STRUCT value at rest —
    /// `{ name, url, mimeType, sizeBytes }` — not a validated string like
    /// the semantic domains above: it names a stored artifact. `url` is a
    /// `data:` URI in mock/playground mode (size-ceilinged) or an
    /// object-storage URL in production.
    File,
    /// Binary value. A standard padded base64 string at run time; the type, not
    /// the representation, keeps it from mixing with `string`.
    Bytes,
    Array,
    Object,
    Enum,
    Relation,
    /// `@trait.X` reference. Allowed only on config fields. Runtime + compiled
    /// codegen substitute the value with embedded trait UI at render time.
    Trait,
    /// An orbital value held as data (language trio): `<spec>.orbitals.<Orbital>`
    /// plus a recorded §8b import body, or `none` (unbound).
    Orbital,
    /// A whole behavior held as data: `{ behavior }` (`behavior/ref`), or `none`.
    /// Embedding one in a render tree runs the program in place.
    Behavior,
    /// UI slot name. Validated against `VALID_SLOTS` at compile time.
    Slot,
    /// Pattern type name. Validated against `PATTERN_TYPES` at compile time.
    Pattern,
    /// Renderable UI content — a pattern node (or array of them), the same
    /// shape `render-ui` effects carry. First-class so view-fragment fields
    /// stop hiding behind erased `object`/`json` (type-integrity campaign).
    Node,
    /// Event-name reference — a string at rest, but the TYPE marks it as an
    /// event key rather than free text (Phase B / SCAN-EVENT-KNOB-2). Legal
    /// on struct-alias fields consumed by config knobs (e.g. `ItemAction.event`);
    /// forbidden directly on entity fields by the parser gate
    /// (`ELOLO_CONFIG_TYPE_FORBIDDEN_IN_ENTITY`). Acceptance is
    /// closed-circuit parity: own transitions ∪ orbital-wide transitions ∪
    /// own emits (`config_type.rs::check_field_def_shape`).
    Event,
    /// A value that ADDRESSES a declared external input — `Trait.EVENT` within
    /// the orbital, `Orbital.Trait.EVENT` across the program; never a bare name.
    /// The `@almadar/core` `EventAddress` contract (what `llm call-tools` reads).
    #[serde(rename = "EventAddress")]
    EventAddress,
    /// A call key — a string at rest, nominal in the type relation: a bare string is
    /// not assignable to it, only the explicit `(as v CallKey)` cast converts. Names
    /// the keyed call a `cancel-call` aborts.
    #[serde(rename = "CallKey")]
    CallKey,
    /// A fixed-length tuple (`.lolo` `[T1, T2, …]`). Its slots ride `properties`, keyed
    /// by slot index (`"0"`, `"1"`, …).
    Tuple,
    /// The name of a field of a record (`.lolo` `field R T`): a string at rest. `record` is `R`, `items` the type the
    /// named field must have (absent: any field). Config-only.
    Field,
    /// An S-expression as data — an atom (string/number/boolean/null), a list
    /// of S-expressions, or an options object: what a transition's guard and
    /// effect arguments are. `SExpr`-typed struct members (`args : [SExpr]`)
    /// lowered to a shapeless `Object` before, which rejected every atom.
    #[serde(rename = "SExpr")]
    SExpr,
    /// A TAGGED union of struct variants: `type DrawItem = DrawShape | DrawText`.
    /// `values` carries the variant NAMES in declaration order; `properties`
    /// carries each variant's shape keyed by that name. The discriminator is
    /// the variant's own literal-typed tag field (`type : "draw-shape"`), so
    /// selection is data-driven, not positional.
    ///
    /// Distinct from a bare `Union` of names, which lowers to `String` +
    /// `values` (a string enum). That collapse is why `DrawShape | DrawText`
    /// used to declare a field whose only legal values were the literal words
    /// "DrawShape"/"DrawText" — it accepted the NAME and rejected the VALUE.
    ///
    /// Recursion (`DrawGroup.items : [DrawItem]`) is carried BY NAME: a
    /// variant that refers back to the union being lowered gets `values` with
    /// no `properties`, so the shape is a reference rather than an infinite
    /// inline expansion.
    Union,
    /// A type variable (`t` in `type Page t = { items : [t]! }` or a trait's
    /// `(t : Type)`), written `"$t"` on the wire, the same spelling as a payload
    /// field's sentinel. Bound by [`FieldDefinition::substitute_type_vars`].
    #[serde(untagged)]
    TypeVar(TypeVarName),
}

/// A type variable's name; serializes as `"$<name>"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeVarName(pub String);

impl Serialize for TypeVarName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("${}", self.0))
    }
}

impl<'de> Deserialize<'de> for TypeVarName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        match raw.strip_prefix('$') {
            Some(name) if !name.is_empty() => Ok(TypeVarName(name.to_string())),
            _ => Err(serde::de::Error::custom(format!("unknown field type `{raw}`"))),
        }
    }
}

impl FieldType {
    pub fn type_var(name: &str) -> Self {
        FieldType::TypeVar(TypeVarName(name.to_string()))
    }

    /// The `.lolo` primitive type names and their tags; `None` for a non-primitive
    /// (an alias, entity or typo). One table for lowering, checking and the
    /// compiler's type-argument binding.
    pub fn from_primitive_name(name: &str) -> Option<Self> {
        Some(match name {
            // `icon`/`component` are refinement types over string (validity enforced
            // at the render-ui layer); they lower to a plain string field.
            "string" | "any" | "icon" | "component" => FieldType::String,
            // Promoted 2026-08-20 (was collapsed to String — the schema lied about
            // the value's shape): money is numeric at rest; file is the canonical
            // {name, url, mimeType, sizeBytes} struct. The declared type — never a
            // field-name heuristic — drives currency inputs/formatting and the
            // upload dropzone + download-chip rendering on both paths.
            "money" => FieldType::Money,
            "file" => FieldType::File,
            "bytes" => FieldType::Bytes,
            "CallKey" => FieldType::CallKey,
            // Semantic string domains, now first-class field types rather than
            // subtypes that erased to `string`. Closes the "no format validation yet"
            // half of Gap G-LOLO-TYPENAME-SILENT-FALLBACK: the declared domain
            // survives lowering, so the validator can check a literal default and
            // codegen can emit the matching zod refinement.
            "email" => FieldType::Email,
            "url" => FieldType::Url,
            "phone" => FieldType::Phone,
            "uuid" => FieldType::Uuid,
            "image" => FieldType::Image,
            // Renderable content is first-class (type-integrity campaign): the
            // `.orb` carries the honest `node` tag instead of lying `string`.
            "node" => FieldType::Node,
            // De-facto accepted config/field types in the std corpus — `json` (free
            // value), `render-ui` (a render-ui pattern object config), `relation`
            // (a foreign-key reference). They lowered to a string field via the old
            // unknown-name fallback; recognized here to preserve that (`json` and
            // `render-ui` are being ELIMINATED, not re-typed — Gate C tracks them)
            // while still rejecting genuine typos.
            "json" | "render-ui" | "relation" => FieldType::String,
            "int" => FieldType::Int,
            "float" | "number" | "duration" => FieldType::Number,
            "bool" | "boolean" => FieldType::Boolean,
            "date" => FieldType::Date,
            "datetime" => FieldType::Datetime,
            "timestamp" => FieldType::Timestamp,
            "instant" => FieldType::Instant,
            "object" => FieldType::Object,
            _ => return None,
        })
    }
}

/// A nullable occurrence or binding makes the substituted value nullable.
pub fn compose_nullability(occurrence: Option<bool>, binding: Option<bool>) -> Option<bool> {
    match (occurrence, binding) {
        (None, None) => None,
        (occurrence, binding) => Some(occurrence.unwrap_or(false) || binding.unwrap_or(false)),
    }
}

impl FieldDefinition {
    /// Replaces every type variable in this shape (items, properties, map
    /// values) by its binding, keeping the occurrence's own name, `required`
    /// and description. An unbound variable stays a variable.
    pub fn substitute_type_vars(&mut self, bindings: &crate::collections::IndexMap<String, FieldDefinition>) {
        if let FieldType::TypeVar(TypeVarName(var)) = &self.field_type {
            if let Some(bound) = bindings.get(var) {
                let name = self.name.take();
                let required = self.required;
                let nullable = self.nullable;
                let description = self.description.take();
                *self = bound.clone();
                self.name = name;
                self.required = required.or(self.required);
                self.nullable = compose_nullability(nullable, self.nullable);
                self.description = description.or(self.description.take());
            }
            return;
        }
        if let Some(items) = self.items.as_deref_mut() {
            items.substitute_type_vars(bindings);
        }
        if let Some(additional) = self.additional_properties.as_deref_mut() {
            additional.substitute_type_vars(bindings);
        }
        if let Some(props) = self.properties.as_mut() {
            for prop in props.values_mut() {
                prop.substitute_type_vars(bindings);
            }
        }
    }
}

/// What deleting a row does to the rows that reference it (`@almadar/core` `RelationConfig.onDelete`).
/// Undeclared means `restrict` on every path that enforces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnDelete {
    Cascade,
    Nullify,
    Restrict,
}

/// Relation definition for entity references
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationDefinition {
    /// Target entity name
    pub entity: String,

    /// V4 dual-carry id sibling of `entity` — optional until the Phase-7 flip.
    #[serde(rename = "entityId", default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,

    /// Cardinality
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<Cardinality>,

    /// Delete behavior of the referencing rows
    #[serde(rename = "onDelete", default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<OnDelete>,
}

impl RelationDefinition {
    pub fn is_to_many(&self) -> bool {
        matches!(
            self.cardinality,
            Some(Cardinality::Many | Cardinality::OneToMany | Cardinality::ManyToMany)
        )
    }
}

/// Relation cardinality
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    One,
    Many,
    #[serde(rename = "many-to-one")]
    ManyToOne,
    #[serde(rename = "one-to-many")]
    OneToMany,
    #[serde(rename = "many-to-many")]
    ManyToMany,
}
