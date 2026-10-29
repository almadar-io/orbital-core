//! Orbitals, their use/expect declarations and inline-or-reference entities, pages and orbitals.

use super::*;

// =============================================================================
// Use Declaration (Import System)
// =============================================================================

/// UseDeclaration - Import an external Orbital as a namespace.
///
/// After importing, access components via:
/// - Alias.entity - The entity definition
/// - Alias.traits.TraitName - A specific trait
/// - Alias.pages.PageName - A specific page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseDeclaration {
    /// Import source path:
    /// - "std/behaviors/game-core" - Standard library
    /// - "./shared/health.orb" - Local .orb file (relative path)
    /// - "../common/physics.orb" - Parent directory .orb file
    /// - "@game-lib/enemies.orb" - Scoped package (configured base)
    pub from: String,

    /// Alias for accessing imported components.
    /// Used as namespace: Alias.entity, Alias.traits.X, Alias.pages.X
    #[serde(rename = "as")]
    pub alias: String,

    /// `uses A from "…" { config { … } }` — call-site override of the imported
    /// organism's app-level declared config knobs (Almadar_Orbital_Import.md §4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::collections::IndexMap<String, ConfigField>>,

    /// `uses lazy A from "…"` — the imported behavior is compiled to its own
    /// `.orb` and loaded when one of its pages is opened (see [`LazyPage`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy: Option<bool>,
}

/// A page served by a lazily-used behavior. The importer lists the page so
/// routing and prerender know it; the behavior lives in the `.orb` at
/// `orb_ref`, relative to the importer's `.orb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazyPage {
    pub path: String,
    pub orbital: String,
    #[serde(rename = "orbRef")]
    pub orb_ref: String,
    /// The link and navigate targets inside this page's resolved behavior, sorted and unique, recorded at split time
    /// so page reachability follows links between lazy pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The page's protocol (`http`, `stdio`, `boot`); absent for a route. A lazy orbital with an endpoint page is loaded when the host starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// The eager orbital that preceded the lazy one in the importer (`None`: it was first): where its listeners sit in registration and fan-out order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The traits the page places, by name: with a trait's dispatch mode, whether a `[client]` trait is mounted by a browser route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<String>,
    /// The traits of the page's orbital as the importer's stub knows them: what the kernel registers before the behavior is
    /// loaded (EXEC-LOAD-2). Every page of one orbital records the same list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<LazyTrait>,
}

/// A trait of a lazily-used orbital, as recorded on its importer: its name, the events it listens to and whether it has a background tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazyTrait {
    pub name: String,
    /// The trait's declared dispatch mode: a `[client]` trait never triggers a server load.
    #[serde(rename = "dispatchMode")]
    pub dispatch_mode: crate::dispatch_mode::DispatchMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listens: Vec<ListenDefinition>,
    /// Whether the trait declares a `[background]` tick: the host runs it with no client mounted, so the orbital loads at boot.
    #[serde(default, rename = "backgroundTicks", skip_serializing_if = "std::ops::Not::not")]
    pub background_ticks: bool,
}

// =============================================================================
// Expect Declaration (Consumer-Side Requirements)
// =============================================================================

/// ExpectDeclaration — a consumer-side requirement: "some sibling orbital
/// provides this, and here is the minimal typed slice of it I rely on."
///
/// The dual of a provider: the entity `[identity]` tag declares "I am the
/// roster"; `expects entity [identity]` declares "I read the roster". Standalone
/// validation trusts the declaration and uses its shape as the local type
/// environment; composed-program validation link-checks it against the real
/// provider. The shape reuses the canonical `FieldDefinition` so all type
/// compatibility flows through `type_compat`. See §3-§5 of
/// docs/Almadar_LOLO_Expects_Proposal.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; the Entity variant carries the access policies and propositions inline")]
pub enum ExpectDeclaration {
    /// `expects entity Customer [identity] { role : "admin" | "member" }` — the `@user`
    /// fields this orbital reads, typed. Bare `expects entity [identity]` (no name, no
    /// shape) is the hand-authoring escape hatch: existence-only, open shape.
    Identity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<Vec<FieldDefinition>>,
        /// Set when the obligation was written in an expects-only orbital (a spec): unprovided is an error, not deferred.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects entity OrderRecord { totalAmount : number }` — a sibling
    /// entity this orbital touches (relation field target, persist/fetch
    /// effect target, linkedEntity rebind). Shape omitted = opaque reference.
    Entity {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<Vec<FieldDefinition>>,
        /// `expects entity X [persistent]`: the storage mode the provider must declare (the collection name is the provider's).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persistence: Option<Persistence>,
        /// `[shared]`: the provider must be a shared entity.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shared: bool,
        /// `[browser]`: the provider's rows must live in the browser.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        browser: bool,
        /// `[self]`: the providing entity must be declared by this same orbital body, under this name; checked in every validate.
        #[serde(default, rename = "self", skip_serializing_if = "std::ops::Not::not")]
        own: bool,
        /// `@read|@create|@update|@delete <expr>`: the provider's policy must be this expression, structurally.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_policy: Option<SExpression>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_policy: Option<SExpression>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_policy: Option<SExpression>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delete_policy: Option<SExpression>,
        /// `@<op> none "<reason>"`: the provider must carry a declared access waiver for each op named (reason text is not compared).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_waivers: Option<AccessWaivers>,
        /// `@invariant <expr>` (spec only): every stored row of the provider satisfies each, proved by `orb validate`
        /// over the program's writes. No runtime effect.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invariants: Option<Vec<SExpression>>,
        /// Set when the obligation was written in an expects-only orbital (a spec): unprovided is an error, not deferred.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects trait Checkout { emits { ORDER_PLACED } }` — a sibling trait produces this
    /// event. Phase 2: feeds escape analysis / listens-audit.
    Event {
        #[serde(rename = "traitName")]
        trait_name: String,
        event: String,
        /// The payload fields the event must carry (names, types and requiredness checked against the
        /// provider's `emits`); absent = the event's name only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Vec<PayloadField>>,
        /// Set when the obligation was written in an expects-only orbital (a spec): unprovided is an error, not deferred.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects page "/articles/:slug"` — a route this orbital navigates to
    /// but does not declare. The one cross-boundary edge family a `navigate`
    /// effect cannot state for itself: the effect carries a static PREFIX
    /// (`/articles/`, all `extract_static_path_prefix` can recover from
    /// `(str/concat "/articles/" @payload.data.slug)`), while the provider
    /// declares a PATTERN (`/articles/:slug`). The parameterisation is real
    /// information the usage does not contain, which is why this is a
    /// declaration rather than a restatement of the effect.
    ///
    /// Derived, not hand-authored, in the normal path: `deriveExpectations`
    /// resolves each navigate target against the whole organism's pages and
    /// emits the matched pattern when it belongs to a sibling. A target that
    /// matches no page derives nothing, so a genuine typo stays an error.
    Page {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        path: String,
        /// `expects page boot -> A`: a protocol endpoint instead of a route (`path` empty); absent = route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<String>,
        /// `expects page "/p" -> A, B`: the traits the provider page must mount.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        traits: Option<Vec<String>>,
        /// Set when the obligation was written in an expects-only orbital (a spec): unprovided is an error, not deferred.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects trait T { state paid }` (the real `state` keyword, no body): the provider trait declares this state.
    State {
        #[serde(rename = "traitName")]
        trait_name: String,
        state: String,
        /// `state paid @holds <expr>` (spec only): the trait's `@entity` satisfies it whenever the trait is in the
        /// state, proved by `orb validate`. No runtime effect.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holds: Option<SExpression>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects trait T { state idle { PAY -> paid } }` (the real transition form, no guard or effects): the
    /// provider trait has a transition `from --event--> to`.
    Transition {
        #[serde(rename = "traitName")]
        trait_name: String,
        from: String,
        event: String,
        to: String,
        /// `@ensures <expr>`: the outcome contract the provider's transition owes (`@fromEntity` is the row before it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ensures: Option<Vec<SExpression>>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
    /// `expects config { stdio : string = "pipe" }`: a trait of the program declares this knob, of this type and,
    /// when one is stated, with this default.
    Config {
        name: String,
        knob: ConfigField,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        strict: bool,
    },
}

impl ExpectDeclaration {
    /// True for an obligation that came from a spec orbital.
    pub fn is_strict(&self) -> bool {
        match self {
            ExpectDeclaration::Identity { strict, .. }
            | ExpectDeclaration::Entity { strict, .. }
            | ExpectDeclaration::Event { strict, .. }
            | ExpectDeclaration::Page { strict, .. }
            | ExpectDeclaration::State { strict, .. }
            | ExpectDeclaration::Transition { strict, .. }
            | ExpectDeclaration::Config { strict, .. } => *strict,
        }
    }
}

impl Orbital {
    /// True for a spec orbital (see the `spec` field).
    pub fn is_spec(&self) -> bool {
        self.spec
    }
}

// =============================================================================
// Entity Reference (Inline OR Reference)
// =============================================================================

/// EntityRefExtends — Inherit an imported entity, optionally renamed,
/// with additional fields appended.
///
/// Mirrors the pattern of `PageRefObject` and `ServiceRefObject`: a
/// dedicated struct that carries a `reference` (the upstream alias.entity
/// path) plus the override fields. The `extends` JSON key is what
/// disambiguates this variant from `EntityRef::Inline` under serde's
/// untagged enum dispatch.
///
/// Semantic: "resolve `reference` to an entity, clone it, optionally rename
/// it via `name`, append `fields` (deduplicate by name where the user's
/// `fields` win on conflict), and apply optional `persistence` and
/// `collection` overrides. The result is the molecule's entity, with the
/// inherited fields available."
///
/// After the inline phase processes this variant, it is rewritten to a
/// regular `EntityRef::Inline(merged_entity)` so downstream phases see no
/// surprises.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRefExtends {
    /// Reference to imported entity: "Alias.entity"
    #[serde(rename = "extends")]
    pub reference: String,

    /// V4 dual-carry id sibling of `extends` — optional until the Phase-7 flip.
    #[serde(rename = "extendsId", default, skip_serializing_if = "Option::is_none")]
    pub extends_id: Option<String>,
    /// Optional rename for the resulting entity. When omitted the merged
    /// entity keeps the upstream entity's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Additional fields to append. Merged after inherited fields and
    /// dedup'd by `name`: when an extra field shares a name with an
    /// inherited field, the extra field replaces the inherited one (the
    /// molecule wins).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldDefinition>>,
    /// Optional persistence override that replaces the upstream value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,
    /// Optional collection override that replaces the upstream value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

/// EntityRef - Entity can be inline definition OR reference to imported entity.
///
/// Reference formats:
/// - extends form `{ extends: "Alias.entity", name?, fields?, ... }`
/// - inline `EntityDefinition` (a literal `name`/`fields`/...)
/// - simple string reference `"Alias.entity"`
///
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub enum EntityRef {
    /// Extends form: inherit an imported entity and append fields
    Extends(EntityRefExtends),
    /// Inline entity definition
    Inline(EntityDefinition),
    /// Reference to imported entity: "Alias.entity"
    Reference(String),
}

impl<'de> Deserialize<'de> for EntityRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(reference) => Ok(Self::Reference(reference.clone())),
            serde_json::Value::Object(object) if object.contains_key("extends") => {
                serde_json::from_value(value).map(Self::Extends).map_err(D::Error::custom)
            }
            serde_json::Value::Object(_) => {
                serde_json::from_value(value).map(Self::Inline).map_err(D::Error::custom)
            }
            _ => Err(D::Error::custom("entity must be an inline object, an extends object, or a string reference")),
        }
    }
}

impl EntityRef {
    /// Check if this is a reference (string or extends form)
    pub fn is_reference(&self) -> bool {
        !matches!(self, EntityRef::Inline(_))
    }

    /// Get the reference string if this is a reference. For the extends
    /// form returns the upstream `extends` value; for `Reference` returns
    /// the bare string.
    pub fn as_reference(&self) -> Option<&str> {
        match self {
            EntityRef::Reference(s) => Some(s),
            EntityRef::Extends(e) => Some(&e.reference),
            _ => None,
        }
    }

    /// Get the inline entity if this is inline
    pub fn as_inline(&self) -> Option<&EntityDefinition> {
        match self {
            EntityRef::Inline(e) => Some(e),
            _ => None,
        }
    }
}

// =============================================================================
// Page Reference (Inline OR Reference)
// =============================================================================

/// PageRefObject - Reference to imported page with optional override fields.
///
/// Phase F: Carries `linked_entity` and `traits` overrides in addition to `path`.
/// The inline phase applies these by rewriting identifiers in the imported page
/// (linked_entity) and replacing its trait list (traits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRefObject {
    /// Reference to imported page: "Alias.pages.PageName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// V4 dual-carry id sibling of `ref` — optional until the Phase-7 flip.
    #[serde(rename = "refId", default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,

    /// Optional path override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// The call site's protocol word; it must equal the referenced page's protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,

    /// Call-site address override for a protocol page (`page http { … } = Alias.pages.X`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<SExpression>,

    /// Phase F: rebind the page's entity to the caller-supplied entity.
    /// The identifier-substitution pass rewrites references to the original
    /// entity inside the imported page.
    #[serde(
        rename = "linkedEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity: Option<String>,

    /// V4 dual-carry id sibling of `linkedEntity` — optional until the Phase-7 flip.
    #[serde(
        rename = "linkedEntityId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity_id: Option<String>,

    /// Phase F: override the page's trait set with the caller-supplied list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<TraitReference>>,

    /// V4 dual-carry id siblings of `traits` — optional until the Phase-7 flip.
    #[serde(
        rename = "traitRefIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub trait_ref_ids: Option<Vec<String>>,

    /// Explicit page-modifier overrides on the reference form. Absent fields
    /// inherit the upstream page's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
    /// The page's nav entry (`@label`/`@icon`/`@roles` on the reference form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

/// PageRef - Page can be inline definition OR reference to imported page.
///
/// Reference formats:
/// - "Alias.pages.PageName" - Simple reference (keeps original path)
/// - { ref: "Alias.pages.PageName", path: "/custom" } - Reference with path override
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PageRef {
    /// Inline page definition (must be first - more specific pattern)
    Inline(PageDefinition),
    /// Reference object with optional path override
    Object(PageRefObject),
    /// Simple reference string: "Alias.pages.PageName"
    Reference(String),
}

impl PageRef {
    /// Check if this is a reference (string or object)
    pub fn is_reference(&self) -> bool {
        !matches!(self, PageRef::Inline(_))
    }

    /// Get the reference string
    pub fn reference_string(&self) -> Option<&str> {
        match self {
            PageRef::Reference(s) => Some(s),
            PageRef::Object(o) => Some(&o.reference),
            _ => None,
        }
    }

    /// Get the inline page if this is inline
    pub fn as_inline(&self) -> Option<&PageDefinition> {
        match self {
            PageRef::Inline(p) => Some(p),
            _ => None,
        }
    }

    /// Get path override (only for PageRefObject)
    pub fn path_override(&self) -> Option<&str> {
        match self {
            PageRef::Object(o) => o.path.as_deref(),
            _ => None,
        }
    }
}

// =============================================================================
// Orbital Reference (Orbital Import)
// =============================================================================

/// OrbitalRefObject - Reference to an imported orbital with optional override fields.
///
/// Field-for-field mirror of `PageRefObject`'s serde style (Almadar_Orbital_Import.md §4).
/// Transient: set by lowering on an unresolved `orbital X = Alias.orbitals.Y { … }`
/// declaration, cleared by the inline phase's orbital-reference resolution — must be
/// `None` in any schema reaching validation, codegen, or a committed `.orb`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrbitalRefObject {
    /// Reference to imported orbital: "Alias.orbitals.UpstreamName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// V4 dual-carry id sibling of `ref` — optional until the Phase-7 flip.
    #[serde(rename = "refId", default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,

    /// Rebind the primary entity to a caller-supplied bare name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,

    /// Primary-entity field renames: upstream field name → local field name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<crate::collections::IndexMap<String, String>>,

    /// Route path remap: upstream path → local path. An unmapped path keeps
    /// its upstream value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<crate::collections::IndexMap<String, String>>,

    /// Page modifiers a `pages {}` remap entry declares, keyed by the
    /// UPSTREAM path: they replace the upstream page's, absent ones inherit.
    #[serde(
        rename = "pageModifiers",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub page_modifiers: Option<std::collections::BTreeMap<String, PageModifiers>>,

    /// Trait names to drop from the imported set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit: Option<Vec<String>>,

    /// Trait names to keep from the imported set (all others dropped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,

    /// Call-site overrides of the upstream orbital's declared config knobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::collections::IndexMap<String, ConfigField>>,

    /// Orbital-scope event rename: upstream event key → local event key,
    /// applied to every trait in the imported set (emits and listen keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<crate::collections::IndexMap<String, String>>,

    /// Role-vocabulary remap (Stage B): upstream role name → one or more
    /// consumer roster role names. A single target lowers to a one-element
    /// Vec; the formatter prints the single form back when len == 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<crate::collections::IndexMap<String, Vec<String>>>,

    /// Out-of-orbital entity remap (Stage B): upstream entity name →
    /// consumer entity name, applied to relation targets, `@Entity` tokens,
    /// fetch/persist targets, policy comparisons, and page
    /// `primaryEntity`/`sourceEntityDefinition`. Identity (`X: X`) is legal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<crate::collections::IndexMap<String, String>>,

    /// Sibling-trait page mounts (Stage B): upstream page path → the
    /// sibling trait names (declared in this reference's body) to mount on
    /// that page, in addition to the traits it already imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<crate::collections::IndexMap<String, Vec<String>>>,

    /// `extend { <field-decl>+ }` (Stage C): full entity-field declarations
    /// appended to the materialized primary entity, AFTER `fields {}`
    /// renames apply — unlike `fields {}` (rename-only), each entry is a
    /// whole new field the upstream entity never declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend: Option<Vec<FieldDefinition>>,

    /// `mock { field: "a, b" }` — sample data for fields of the materialized
    /// primary entity (named after `fields {}` renames), replacing the
    /// upstream field's `@mock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<crate::collections::IndexMap<String, String>>,

    /// `retype { name : Type = default }`: replaces the type/default of fields the
    /// upstream primary entity already declares (after `fields {}` renames). Unlike
    /// `extend {}`, every name MUST already exist (`ORB_O_RETYPE_UNKNOWN_FIELD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retype: Option<Vec<FieldDefinition>>,

    /// `entity Name [persistent: tbl]`: overrides the materialized primary entity's
    /// persistence mode. Refused when the upstream entity is not `persistent`
    /// (`ORB_O_ENTITY_PERSISTENCE_LOCKED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,

    /// `entity Name [persistent: tbl]`: overrides the primary entity's collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,

    /// Entity-header flags of the rebind line (`[shared]`, `[identity]`, `[browser]`);
    /// each one switches the flag ON for the materialized primary entity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,

    /// `traits { UpstreamTrait { ... } }`: per-imported-trait call-site overrides,
    /// keyed by the trait's UPSTREAM name. Same surface as a trait reference's body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<crate::collections::IndexMap<String, OrbitalRefTraitOverride>>,

    /// Declared routes INTO the imported set: each entry appends one listen
    /// to the named imported trait (upstream name). The source is a host
    /// sibling or another imported trait; the imported set's own wiring stays
    /// upstream-owned. Events reach another trait only through `listens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<OrbitalRefListen>>,
}

/// One `traits { UpstreamTrait { ... } }` entry of an orbital import: the §8
/// trait-reference override surface, applied to a single imported trait.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrbitalRefTraitOverride {
    /// `-> Entity` on the entry: rebinds the trait's linked entity.
    #[serde(
        rename = "linkedEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::collections::IndexMap<String, ConfigField>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<crate::collections::IndexMap<String, String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<crate::collections::IndexMap<String, String>>,

    /// Replaces the trait's whole `listens` array when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<ListenDefinition>>,

    #[serde(
        rename = "emitsScope",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub emits_scope: Option<EventScope>,

    /// `traits { Up [client|optimistic|server] }`: replaces the imported trait's
    /// declared dispatch mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<DispatchMode>,
}

/// One `listens { Source.EVENT -> Target.TRIGGER }` entry of an orbital
/// import: `trait` names the imported trait (upstream name) that gains the
/// route; the rest is an ordinary listen definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitalRefListen {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(flatten)]
    pub listen: ListenDefinition,
}

// =============================================================================
// Orbital (Feature Unit)
// =============================================================================

/// An Orbital is a self-contained feature unit: Entity + Traits + Pages
///
/// Use `uses` to import other orbitals and access their components via:
/// - Alias.entity - The entity definition
/// - Alias.traits.TraitName - A specific trait
/// - Alias.pages.PageName - A specific page
/// - Alias.theme - The theme definition
/// - Alias.services.ServiceName - A specific service
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orbital {
    /// Orbital name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Import declarations for external orbitals
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<Vec<UseDeclaration>>,

    /// Consumer-side requirement declarations (`.lolo` `expects ...`).
    /// Promises about sibling orbitals this unit relies on: trusted as the
    /// local type environment when the orbital is validated standalone,
    /// link-checked against real providers at compose. Compile-time only —
    /// runtime semantics are unchanged. See docs/Almadar_LOLO_Expects_Proposal.md.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expects: Option<Vec<ExpectDeclaration>>,

    /// An orbital of only `expects` lines: a spec. It provides nothing, so its own obligations bind
    /// the orbital that imports it and the validators skip its shell entity, trait and page rules.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spec: bool,

    /// Theme definition (inline OR reference via "Alias.theme")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeRef>,

    /// Service definitions (inline OR reference via "Alias.services.ServiceName")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceRef>>,

    /// Entity definition (inline OR reference via "Alias.entity")
    ///
    /// Entity sharing depends on persistence type:
    /// - persistent: Shared (same DB collection)
    /// - runtime: Isolated (each orbital gets own instances)
    /// - singleton: Shared (single global instance)
    pub entity: EntityRef,

    /// Imported atom entities surfaced into this orbital's resolution
    /// scope WITHOUT modifying the primary entity. Populated by the
    /// inline phase when a trait reference imports an atom and OMITS
    /// the `-> Entity` linkedEntity rebind: the atom keeps its own
    /// entity (e.g. `SearchResult` from std-search), and that entity
    /// is registered here so validators / codegen / runtime can
    /// resolve `linkedEntity = "SearchResult"` lookups, register
    /// persistence for it, and emit its TypeScript type alongside
    /// the primary entity. The orbital's per-trait entity arity stays
    /// 1:1 — this is purely a NAME-RESOLUTION side channel for cross
    /// import linkedEntity references.
    #[serde(rename = "auxiliaryEntities", skip_serializing_if = "Option::is_none")]
    pub auxiliary_entities: Option<Vec<EntityRef>>,

    /// Traits attached to this orbital (local or via "Alias.traits.TraitName")
    pub traits: Vec<TraitReference>,

    /// Pages for this orbital (inline OR reference)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<PageRef>>,

    /// Events this orbital emits (for cross-orbital communication)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emits: Option<Vec<String>>,

    /// Events this orbital listens to (for cross-orbital communication)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<ListenDefinition>>,

    /// Filter which trait events are exposed externally.
    /// If omitted, all external-scoped trait events are exposed.
    /// Format: ["TraitName.EVENT_NAME", ...]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposes: Option<Vec<String>>,

    /// Present ONLY on an unresolved `orbital X = Alias.orbitals.Y { … }` declaration.
    /// Set by lowering, cleared by the inline phase's orbital-reference resolution; must be
    /// `None` in any schema reaching validation, codegen, or a committed `.orb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<OrbitalRefObject>,

    /// This orbital's own declared config knobs (§4.5) — mirrors `TraitDefinition.config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::collections::IndexMap<String, ConfigField>>,

    /// This orbital's own `os/` grants (`orbital Name [ fs: …, exec: … ]`); its effective grants are the app level plus these.
    #[serde(rename = "osGrants", default, skip_serializing_if = "Option::is_none")]
    pub os_grants: Option<crate::os_grants::OsGrants>,

    /// Domain context metadata
    #[serde(rename = "domainContext", skip_serializing_if = "Option::is_none")]
    pub domain_context: Option<DomainContext>,

    /// Design hints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design: Option<DesignHints>,

    /// The orbital's own named STRUCT aliases (`type X = { ... }`), each
    /// flattened to the same `PayloadField` tree emit/listen payloads use.
    /// `.orb`'s only record of a struct alias post-L1 — a Type-kind trait
    /// type-param arg (`:: p SomeStructAlias`) resolves against this map.
    /// Enum/primitive/entity aliases are NOT carried here (not legal `::`
    /// args). `BTreeMap` for deterministic serialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<std::collections::BTreeMap<String, Vec<PayloadField>>>,
}
