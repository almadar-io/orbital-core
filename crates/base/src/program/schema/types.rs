//! Core schema types for Orbital
//!
//! These types match the TypeScript OrbitalSchema definitions and can parse `.orb` files.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};

use crate::schema::sexpr::SExpression;
use crate::effect_row::EffectRow;

/// `ConfigField.default`'s deserializer: distinguishes an ABSENT `default`
/// key (→ `None`, via `#[serde(default)]` on the field, which short-circuits
/// before this ever runs) from a key PRESENT with JSON value `null` (→
/// `Some(SExpression::Null)`). Without this, deriving `Option<SExpression>`
/// directly hands `null` to `Option`'s own `Deserialize` impl, which treats
/// ANY `null` as `None` regardless of `SExpression`'s own `Null` variant —
/// collapsing "no default was ever declared" and "the default IS the
/// unbound sentinel" (a `trait`-typed knob's `none`, owner ruling
/// 2026-09-13) into the same on-disk shape. Every consumer that reads a
/// `.orb` registry file (a downstream `uses` load, `orb validate`'s
/// re-inline, the runtime) needs the distinction preserved through the
/// round trip, not just within one process's in-memory compile.
fn deserialize_present_default<'de, D>(deserializer: D) -> Result<Option<SExpression>, D::Error>
where
    D: Deserializer<'de>,
{
    SExpression::deserialize(deserializer).map(Some)
}

// =============================================================================
// Root Schema
// =============================================================================

/// The type contract version this build reads and writes.
pub const TYPE_CONTRACT_VERSION: u32 = 1;

/// Who resolved a program and under which type contract (`OrbitalSchema.resolved`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMark {
    /// The `orb` version that resolved the program.
    pub orb: String,
    /// The type contract version the resolved program was written under.
    #[serde(rename = "typeContractVersion")]
    pub type_contract_version: u32,
}

/// The root OrbitalSchema type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitalSchema {
    #[serde(default, rename = "typeContractVersion", skip_serializing_if = "Option::is_none", deserialize_with = "crate::ir::types::deserialize_type_contract_version")]
    pub type_contract_version: Option<u32>,
    /// Present exactly when the program is RESOLVED (inlined, composed, enriched): stamped by the compiler as the
    /// last step of resolution, never authored. A loader takes a program carrying it as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<std::collections::BTreeMap<String, Vec<PayloadField>>>,

    /// Application name
    pub name: String,

    /// Schema version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Application description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// App-level theme — the skin every orbital's `@currentTheme` seeds from
    /// unless the orbital declares its own `theme` (precedence: orbital →
    /// app → `DEFAULT_THEME_KEY`). Authored as `theme "<key>"` in the
    /// `.lolo` app header; a full `<base>-<light|dark>` registry key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeRef>,

    /// The registry themes the program switches to inside a page (a sub-frame's
    /// `data-theme`), beyond its app and orbital themes. Authored as
    /// `themes: [<key>, …]` in the `.lolo` app header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,

    /// The theme presets this program references plus the runtime default,
    /// written by `orb` when it compiles the program for a runtime, so a kernel
    /// with no installed `@almadar/core` (wasm, mobile) resolves `@currentTheme`
    /// from the program alone. Never authored.
    #[serde(
        rename = "themeDefinitions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub theme_definitions: Option<Vec<ThemeDefinition>>,

    /// Organism-wide declared config knobs (Almadar_Orbital_Import.md §4.5). Same
    /// `ConfigField` value type as `TraitDefinition.config` and `Orbital.config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// List of orbitals (feature units)
    pub orbitals: Vec<Orbital>,

    /// `locales [en, ar, sl]` — the locales this program ships. Every
    /// `(i18n/t …)` key must exist in the catalog of each one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locales: Option<Vec<String>>,

    /// `public "./public"` — a folder beside the program copied verbatim to
    /// the built site's root (logos, favicons, downloads).
    #[serde(rename = "publicDir", default, skip_serializing_if = "Option::is_none")]
    pub public_dir: Option<String>,

    /// Site modifiers (`origin`, `icon`, `siteName`, `signIn`) from the app bracket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteMeta>,

    /// `app … [store: Driver]` — the default store-driver orbital for persistent entities that name none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,

    /// `egress: [...]` — the exact `scheme://host[:port]` origins the `http` effect may reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<Vec<String>>,

    /// `inbound: [...]` — the hook names (`/api/hooks/<name>`) the program accepts deliveries on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbound: Option<Vec<String>>,

    /// The `os/` grants (`fs`, `exec`, `net`, `env`, `stdio`, `sys`, `desktop`): what the program's `os/` effects may touch.
    #[serde(rename = "osGrants", default, skip_serializing_if = "Option::is_none")]
    pub os_grants: Option<crate::os_grants::OsGrants>,

    /// `deployment: { unit: [Orbital, ...] }` — which deployment unit runs each orbital; absent = one unit holding every orbital.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<std::collections::BTreeMap<String, Vec<String>>>,

    /// Pages served by `uses lazy` behaviors, each loaded from its own `.orb`
    /// when opened.
    #[serde(rename = "lazyPages", default, skip_serializing_if = "Option::is_none")]
    pub lazy_pages: Option<Vec<LazyPage>>,

    /// Schema metadata
    #[serde(rename = "_metadata", skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SchemaMetadata>,

    /// V4 IR schema version — optional until the Phase-7 flip.
    #[serde(
        rename = "schemaVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub schema_version: Option<u32>,

    /// V4 identity ledger slice (id → name history) — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<crate::identity::IdentityLedger>,
}

impl OrbitalSchema {
    /// An orbital's effective `os/` grants: the app level plus its own.
    pub fn orbital_os_grants(&self, orbital: &Orbital) -> crate::os_grants::OsGrants {
        let mut grants = self.os_grants.clone().unwrap_or_default();
        if let Some(own) = &orbital.os_grants {
            grants.union(own);
        }
        grants.union(&self.egress_net_grants());
        grants
    }

    /// The raw TCP targets the app's `egress` declares (`redis://`, `rediss://`, `postgres://`, `postgresql://`
    /// origins) as `net` grants: the one declaration a protocol driver's `os/tcp-connect` is authorized by, so the
    /// host's net check, the validator and the io-gate manifest all see the target without a second `net` entry.
    pub fn egress_net_grants(&self) -> crate::os_grants::OsGrants {
        let net = self
            .egress
            .iter()
            .flatten()
            .filter_map(|entry| crate::http_wire::parse_egress_entry(entry))
            .filter(|origin| crate::http_wire::tcp_default_port(origin.scheme()).is_some())
            .map(|origin| crate::os_grants::NetGrant {
                target: format!("{}:{}", origin.host(), origin.port()),
                kinds: vec![crate::os_grants::NetKind::Tcp],
            })
            .collect();
        crate::os_grants::OsGrants { net, ..Default::default() }
    }

    /// The `os/` grants of a set of orbitals: the explicit union of each one's effective grants.
    pub fn os_grants_of<'a>(
        &self,
        orbitals: impl IntoIterator<Item = &'a Orbital>,
    ) -> crate::os_grants::OsGrants {
        let mut grants = crate::os_grants::OsGrants::default();
        for orbital in orbitals {
            grants.union(&self.orbital_os_grants(orbital));
        }
        grants
    }

    /// The program's `os/` grants: the app level plus every orbital's own.
    pub fn program_os_grants(&self) -> crate::os_grants::OsGrants {
        let mut grants = self.os_grants.clone().unwrap_or_default();
        grants.union(&self.os_grants_of(&self.orbitals));
        grants.union(&self.egress_net_grants());
        grants
    }

    /// An orbital's inline trait with no `linkedEntity` binds its host orbital's
    /// entity (name + id) — the default `@almadar/runtime`'s `buildTraitIndex`
    /// applies, settled once so the kernel, every shell and every validator
    /// read one declared binding.
    pub fn bind_unlinked_traits_to_host_entity(&mut self) {
        for orbital in &mut self.orbitals {
            let EntityRef::Inline(host) = &orbital.entity else {
                continue;
            };
            let (name, id) = (host.name.clone(), host.id.clone());
            for t in &mut orbital.traits {
                if let TraitReference::Inline(td) = t {
                    if td.linked_entity.is_none() {
                        td.linked_entity = Some(name.clone());
                        td.linked_entity_id = id.clone();
                    }
                }
            }
        }
    }
}

/// Schema metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,

    /// Config→knob provenance for `@config.X` bindings that the inline phase
    /// substituted into render-ui props, keyed by orbital name. After
    /// substitution the `@config.X` string is gone, so a consumer (Studio
    /// contextual edit) reads this to trace a resolved render-ui prop back to
    /// the config knob it came from. See [`ConfigProvenanceRecord`].
    #[serde(rename = "configProvenance", skip_serializing_if = "Option::is_none")]
    pub config_provenance: Option<HashMap<String, Vec<ConfigProvenanceRecord>>>,

    /// For a program that declares `locales`: each loaded behavior's name →
    /// the folder of its source file, relative to the program's own folder.
    /// Its message catalogs live at `<folder>/locales/<locale>/<name>.json`.
    #[serde(
        rename = "messageSources",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub message_sources: Option<std::collections::BTreeMap<String, String>>,

    /// Where each effect operator the client's traits run executes (`server`, `client`, `any`), written into a
    /// compiled client program so a browser kernel with no installed operator table places every effect.
    #[serde(rename = "operatorSites", default, skip_serializing_if = "Option::is_none")]
    pub operator_sites: Option<std::collections::BTreeMap<String, String>>,

    /// The operator table's dispatch facts, written into a compiled client program for a browser kernel with no table.
    #[serde(rename = "dispatchFacts", default, skip_serializing_if = "Option::is_none")]
    pub dispatch_facts: Option<crate::language::registry::DispatchFacts>,

    /// The endpoint protocol table, written into a compiled client program for a browser kernel with no installed core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<crate::protocols::ProtocolSpec>>,
}

/// One `@config.<knob>` → render-ui prop substitution recorded at inline time.
/// Emitted under [`SchemaMetadata::config_provenance`] so a resolved literal
/// can be traced back to its originating config knob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProvenanceRecord {
    /// Resolved trait name (post call-site rename) owning the render-ui effect.
    #[serde(rename = "trait")]
    pub trait_name: String,
    /// Pattern path within the render-ui tree (e.g. `root.children.0`), matching
    /// the runtime `UISlotRenderer` `${parentPath}.children.${index}` convention.
    #[serde(rename = "patternPath", skip_serializing_if = "Option::is_none")]
    pub pattern_path: Option<String>,
    /// The pattern-node prop key whose value was a `@config.X` binding.
    pub prop: String,
    /// The config knob name (`X` in `@config.X`).
    pub knob: String,
}

mod theme;
mod services;
mod orbital;
mod entity;
mod traits;
mod events;
mod state_machine;
mod page;
pub use entity::{
    compose_nullability, tuple_slots_of, AccessWaivers, AssetAspect, AssetDimension, Cardinality,
    EntityDefinition, EntityIndex, FieldDefinition, FieldProjection, FieldType, OnDelete,
    Persistence, RelationDefinition, SemanticAssetRef, TagReading, TypeVarName, UnionVariantChoice,
    VisualStyle, FRAMEWORK_STAMPED_FIELDS,
};
pub use events::{
    callsite_payload_path, capture_callsite_payload, collect_callsite_payload_paths,
    config_ref_event_knob, contains_callsite_payload, definer_knobs_for_trait, join_event_address,
    resolve_config_ref_event_name, resolve_payload_type_when, retag_callsite_payload,
    split_event_address, ConfigRefEventError, EmitDefinition, EventScope, ListenDefinition,
    ListenSource, PayloadField, PayloadTypeWhen, CALLSITE_PAYLOAD_PREFIX, PAYLOAD_PREFIX,
};
pub use orbital::{
    EntityRef, EntityRefExtends, ExpectDeclaration, LazyPage, Orbital, OrbitalRefListen,
    OrbitalRefObject, OrbitalRefTraitOverride, PageRef, PageRefObject, UseDeclaration,
};
pub use page::{
    is_host_sign_in_route, DesignHints, DomainContext, I18nHead, PageAccess, PageDefinition,
    PageIndexing, PageMessage, PageMeta, PageModifiers, PatternDefinition, SectionDefinition,
    SiteMeta, HOST_SIGN_IN_ROUTE,
};
pub use services::{
    AuthKeyLocation, McpServiceDef, McpServiceDefInner, RestAuthConfig, RestAuthType,
    RestServiceDef, RestServiceDefInner, ServiceDefinition, ServiceRef, ServiceRefObject,
    SocketEvents, SocketReconnect, SocketServiceDef, SocketServiceDefInner,
};
pub use state_machine::{
    EventDefinition, InboundDeclaration, InboundVerification, StateDefinition, StateMachine,
    StringOrArray, StringOrNumber, TickDefinition, TransitionDefinition,
};
pub use theme::{
    css_time_ms, ColorTokens, DensityTokens, DiagramFillStyle, DiagramLabelCase, DiagramLabelFont,
    DiagramLineCap, DiagramLineJoin, DiagramMarker, DiagramTokens, ElevationTokens, EnterAnimation,
    GeometryTokens, IconFamily, IconographyTokens, IllustrationStyle, IllustrationTokens,
    MotionDurationKey, MotionDurationPalette, MotionEasingKey, MotionEasingPalette,
    MotionEntryTokens, MotionIntent, MotionIntentMap, MotionShapeTokens, MotionTokens, SceneTokens,
    SpacingScale, SurfaceTokens, ThemeDefinition, ThemeMap, ThemeRef, ThemeTokens, ThemeVariant,
    TypeIntent, TypeIntentMap, TypeScale, TypeScaleEntry, TypeScaleTokens, TypeSizeKey, TypeSlot,
    TypeWeight, BASE_BUSY_DELAY_MS,
};
pub use traits::{
    ConfigField, DispatchMode, EntityFieldContract, SourceBehaviorMetadata,
    SourceDeclarationMetadata, TraitDefinition, TraitReference, TraitTypeParamDef,
};

#[cfg(test)]
mod tests;
