//! OIR Type Definitions
//!
//! Core types for the Orbital Intermediate Representation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{FieldDefinition, FieldType, ServiceRef, ThemeDefinition};
use crate::str_utils::to_snake_case;
use crate::collections::IndexMap;
use serde::{Deserialize, Serialize};
use crate::value::Shared;
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

mod entity;
mod oir_type;
mod shape;
mod traits;
mod transition;
mod page;
mod value;
pub use entity::{OirCardinality, OirConstraint, OirEntity, OirField, OirPersistence, OirRelation};
pub use oir_type::{no_fields, OirType};
pub use page::{
    OirEntityBinding, OirNavigation, OirPage, OirPattern, OirSection, OirSectionEvent,
    OirTraitBinding, OirViewType,
};
pub(crate) use shape::deserialize_type_contract_version;
pub use shape::{
    declared_alias_shape, field_shape, names_in, payload_array_item_type, payload_field_shape,
    payload_field_shape_with_entity, payload_tuple_slots, payload_type_shape, record_shape,
    DeclaredShape,
};
pub use traits::{OirDispatchMode, OirEvent, OirState, OirTrait, OirTraitSource};
pub use transition::{
    await_render_rows_from_resolved, AwaitRender, AwaitRenderRow, OirEmit, OirEventScope, OirGuard,
    OirInterval, OirListener, OirPayloadField, OirTick, OirTransition, OirTransitionFrom,
    SkeletonNode, SkeletonShape, SKELETON_VARIANTS,
};
pub use value::{
    invoked_services_in, OirConfigSchema, OirIdMap, OirMetadata, OirObject, OirTypeMap, OirValue,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tuple_field_tests;

#[cfg(test)]
mod interval_wire_tests;
