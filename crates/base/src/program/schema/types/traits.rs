//! Trait definitions, trait references and config fields.

use super::*;

// =============================================================================
// Trait Definition
// =============================================================================

/// Increment 3-II (`docs/Almadar_LOLO.md`): one trait-level type parameter,
/// declared `(V : Type)` in `trait Name (V : Type, R : Type) -> Entity { ... }`.
/// `kind` is one of the same six kind names Increment 3-I's alias params
/// accept (`Type`, `Entity`, `Trait`, `Page`, `Orbital`, `Event`) — in
/// practice `Type` and `Entity` are the two meaningful kinds for a trait
/// param today. Carried on the DECLARING trait's `TraitDefinition` (never on
/// a `Reference`) so the compiler's inline phase can tell, for a surviving
/// `"$<name>"` payload sentinel, whether it's Entity-kind (defaults to the
/// trait's `linkedEntity` when no call-site arg was supplied — the 3-I
/// behavior) or Type-kind (no default; an unresolved sentinel is blocking).
///
/// Gap C: `default` carries an optional `(r : Type = <name>)` declaration
/// default — a primitive name, an entity name, or a
/// struct-alias name declared in the atom's own file. Resolved via the same
/// vocabulary a call-site `::` arg uses, consulted only when no call-site
/// arg was supplied (and, for Entity-kind, only when `linkedEntity` also
/// gave no default) — see `resolve_type_param_sentinels`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraitTypeParamDef {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Trait reference in an orbital
///
/// Both `Serialize` and `Deserialize` are hand-written so the wire form is FLAT
/// (the inner trait/reference fields at the top level), identical to the bytes
/// the old `#[serde(untagged)]` produced — NOT externally tagged. Deserialization
/// dispatches on the `stateMachine` discriminator (present only on an inline
/// `TraitDefinition`, never on a reference) rather than serde's untagged
/// trial-and-error: dispatching to ONE concrete variant means serde surfaces the
/// variant's real field-level error (`unknown field`, `missing field`, type
/// mismatch) instead of the opaque "did not match any variant of untagged enum".
#[derive(Debug, Clone)]
pub enum TraitReference {
    /// Inline trait definition
    Inline(Arc<TraitDefinition>),
    /// Reference to a named trait. Serde attributes for these fields live on the
    /// `TraitReferenceFields` (deserialize) and `TraitReferenceFieldsRef`
    /// (serialize) mirror structs below; the enum's `Serialize`/`Deserialize`
    /// are hand-written, so no derive consumes per-field `#[serde]` here.
    Reference {
        reference: Option<String>,
        /// V4 dual-carry id sibling of `reference` — optional until the Phase-7 flip.
        ref_id: Option<String>,
        /// V4 LOCAL DECLARATION id — the identity of this trait AS DECLARED in
        /// its orbital (`trait X = Alias.traits.Y` declares node X; `ref_id`
        /// carries Y's id). Minted by `stamp_identity` so page trait refs and
        /// `@trait.X` embeds resolve by id across `name:` renames — the
        /// composed-organism surface was the one declaration class without an
        /// id (battery 2026-07-20 FACTORY-PAGE-TRAIT-NAME-COUPLING).
        id: Option<String>,
        name: Option<String>,
        linked_entity: Option<String>,
        /// V4 dual-carry id sibling of `linked_entity` — optional until the Phase-7 flip.
        linked_entity_id: Option<String>,
        /// Call-site config overrides. Each entry may be a plain wiring
        /// (`ConfigField` with only `default` set, `field_type` = `"unknown"`)
        /// or a fully-annotated re-declaration (`field_type`, `label`,
        /// `description`, `synonyms`, `tier` populated). The inline phase
        /// folds both into the atom's declared config schema: the `default`
        /// is always applied; annotations are merged (organism wins) onto the
        /// inlined trait's config field so emitted artifacts carry the
        /// organism's wording.
        ///
        /// Serialization: a plain wiring value serializes as
        /// `{ "key": { "type": "unknown", "default": <SExpr> } }` — the
        /// same shape the atom's config block uses, so round-trip is clean.
        config: Option<crate::collections::IndexMap<String, ConfigField>>,
        /// Phase F: caller-supplied event-key renames, e.g. {"OPEN": "ADD_ITEM"}.
        /// The inline phase rewrites every occurrence of the atom-declared event
        /// key (StateMachine.events[*].key, transitions[*].event, emits[*].event,
        /// listens[*].event/triggers, and any SExpression event references) to
        /// the caller-supplied value.
        events: Option<crate::collections::IndexMap<String, String>>,
        /// V4 dual-carry id siblings of `events` keys (baked event name → id) —
        /// optional until the Phase-7 flip.
        event_ids: Option<crate::collections::IndexMap<String, String>>,
        /// Per-key entity-field rename map, e.g. {"title": "dishName"}. Mirrors
        /// `events`: the inline phase rewrites every `@entity.<old>` read inside
        /// the resolved trait to `@entity.<new>` at the call site, so a DATA
        /// field the trait reads can be satisfied by a differently-named field
        /// on the rebind target. Keys are the trait's declared field names;
        /// values are the target entity's field names.
        fields: Option<crate::collections::IndexMap<String, String>>,
        /// Phase F.7: replace the imported trait's `listens` array with the
        /// caller-supplied list. Empty array clears all upstream listens.
        /// Applied AFTER the substitution pass, overwriting whatever the
        /// upstream declared. Use sparingly: this drops external event wiring.
        listens: Option<Vec<ListenDefinition>>,
        /// Phase F.7: set every emit's `scope` to the caller-supplied value.
        /// `Internal` confines emits to the orbital's internal bus; `External`
        /// broadcasts them. Applied after substitution and listens replacement.
        emits_scope: Option<EventScope>,
        /// Increment 3-II: call-site type-parameter arguments, e.g.
        /// `{"V": "string"}` from `trait X :: V string = Alias.traits.Y { ... }`.
        /// Reinterpretation of the pre-existing `::` type-annotation surface —
        /// `orbital-lolo` populates this ONLY when the annotation's `type_name`
        /// does NOT resolve as a known local type alias (preserving the
        /// original alias-conformance meaning of `::` untouched whenever it
        /// does resolve). The inline phase consumes this against the
        /// resolved source trait's `type_params` to substitute each
        /// `"$<param>"` payload sentinel; unconsumed keys (source declares no
        /// matching param) are a harmless no-op.
        type_args: Option<crate::collections::IndexMap<String, String>>,
        /// Call-site dispatch mode (`trait X = Alias.traits.Y -> E [client]`):
        /// replaces the mode the atom declared when present.
        dispatch: Option<DispatchMode>,
    },
}

impl TraitReference {
    /// Get the trait name (either from ref or inline name)
    pub fn trait_name(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => Some(&def.name),
            TraitReference::Reference {
                reference, name, ..
            } => reference.as_deref().or(name.as_deref()),
        }
    }

    /// V4 dual-carry id of the referenced trait (inline node id, or the
    /// reference's `refId`). `None` until the Phase-7 flip mints ids everywhere.
    pub fn trait_ref_id(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => def.id.as_deref(),
            TraitReference::Reference { ref_id, .. } => ref_id.as_deref(),
        }
    }

    /// V4 LOCAL DECLARATION id — the identity of this trait as declared in its
    /// orbital (for a `Reference` this is the declared node's own id, NOT the
    /// referenced target's `refId`).
    pub fn trait_decl_id(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => def.id.as_deref(),
            TraitReference::Reference { id, .. } => id.as_deref(),
        }
    }
}

/// Fields of the `Reference` variant, mirrored as a struct so the manual
/// `Deserialize` for `TraitReference` can deserialize the named-reference shape
/// in one concrete pass (producing real field-level errors).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TraitReferenceFields {
    #[serde(rename = "ref", default)]
    pub(super) reference: Option<String>,
    #[serde(rename = "refId", default)]
    pub(super) ref_id: Option<String>,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(rename = "linkedEntity", default)]
    pub(super) linked_entity: Option<String>,
    #[serde(rename = "linkedEntityId", default)]
    pub(super) linked_entity_id: Option<String>,
    #[serde(default)]
    pub(super) config: Option<crate::collections::IndexMap<String, ConfigField>>,
    #[serde(default)]
    pub(super) events: Option<crate::collections::IndexMap<String, String>>,
    #[serde(rename = "eventIds", default)]
    pub(super) event_ids: Option<crate::collections::IndexMap<String, String>>,
    #[serde(default)]
    pub(super) fields: Option<crate::collections::IndexMap<String, String>>,
    #[serde(default)]
    pub(super) listens: Option<Vec<ListenDefinition>>,
    #[serde(rename = "emitsScope", default)]
    pub(super) emits_scope: Option<EventScope>,
    #[serde(rename = "typeArgs", default)]
    pub(super) type_args: Option<crate::collections::IndexMap<String, String>>,
    #[serde(default)]
    pub(super) dispatch: Option<DispatchMode>,
}

/// Borrowing mirror of the `Reference` variant used only by the hand-written
/// `Serialize`. Field names, `rename`s, and `skip_serializing_if` rules match
/// the variant exactly so the FLAT wire output is byte-identical to the old
/// `#[serde(untagged)]` derive (which serialized the reference's fields at the
/// top level, NOT under a `"Reference"` tag).
#[derive(Serialize)]
pub(super) struct TraitReferenceFieldsRef<'a> {
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub(super) reference: &'a Option<String>,
    #[serde(rename = "refId", skip_serializing_if = "Option::is_none")]
    pub(super) ref_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: &'a Option<String>,
    #[serde(rename = "linkedEntity", skip_serializing_if = "Option::is_none")]
    pub(super) linked_entity: &'a Option<String>,
    #[serde(rename = "linkedEntityId", skip_serializing_if = "Option::is_none")]
    pub(super) linked_entity_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) config: &'a Option<crate::collections::IndexMap<String, ConfigField>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) events: &'a Option<crate::collections::IndexMap<String, String>>,
    #[serde(rename = "eventIds", skip_serializing_if = "Option::is_none")]
    pub(super) event_ids: &'a Option<crate::collections::IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fields: &'a Option<crate::collections::IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) listens: &'a Option<Vec<ListenDefinition>>,
    #[serde(rename = "emitsScope", skip_serializing_if = "Option::is_none")]
    pub(super) emits_scope: &'a Option<EventScope>,
    #[serde(rename = "typeArgs", skip_serializing_if = "Option::is_none")]
    pub(super) type_args: &'a Option<crate::collections::IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) dispatch: &'a Option<DispatchMode>,
}

impl Serialize for TraitReference {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // FLAT output (no external `"Inline"`/`"Reference"` tag): the inline
        // variant serializes as the bare `TraitDefinition`; the reference
        // variant serializes its fields at the top level via the borrowing
        // mirror. Matches the old untagged derive byte-for-byte.
        match self {
            TraitReference::Inline(t) => (**t).serialize(serializer),
            TraitReference::Reference {
                reference,
                ref_id,
                id,
                name,
                linked_entity,
                linked_entity_id,
                config,
                events,
                event_ids,
                fields,
                listens,
                emits_scope,
                type_args,
                dispatch,
            } => TraitReferenceFieldsRef {
                reference,
                ref_id,
                id,
                name,
                linked_entity,
                linked_entity_id,
                config,
                events,
                event_ids,
                fields,
                listens,
                emits_scope,
                type_args,
                dispatch,
            }
            .serialize(serializer),
        }
    }
}

/// Validation code of a trait that carries a retired key.
pub const RETIRED_TRAIT_KEY_CODE: &str = "ORB_SCHEMA_RETIRED_TRAIT_KEY";

const RETIRED_TRAIT_KEYS: &[(&str, &str)] = &[
    (
        "local",
        "retired; `\"local\": true` is `\"dispatch\": \"client\"`, and `\"local\": false` is deleted",
    ),
    (
        "category",
        "retired with P-009; declare `dispatch` (`\"client\"` or `\"optimistic\"`) where the trait runs client-side, otherwise delete the key (a trait without `dispatch` is server-awaited)",
    ),
    (
        "scope",
        "retired when `TraitScope` became `TraitDispatch`; declare `dispatch` (`\"client\"` or `\"optimistic\"`) where the trait runs client-side, otherwise delete the key",
    ),
];

/// One retired key found on a trait of a parsed `.orb` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredTraitKey {
    pub path: String,
    pub trait_name: Option<String>,
    pub key: &'static str,
    pub replacement: &'static str,
}

impl RetiredTraitKey {
    pub fn detail(&self) -> String {
        let owner = self
            .trait_name
            .as_deref()
            .map(|n| format!("trait `{n}`"))
            .unwrap_or_else(|| "a trait".to_string());
        format!(
            "{owner} carries the retired key `{}` ({})",
            self.key, self.replacement
        )
    }

    pub fn message(&self) -> String {
        format!("{RETIRED_TRAIT_KEY_CODE}: {}", self.detail())
    }
}

fn retired_keys_of(
    trait_obj: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Vec<RetiredTraitKey> {
    let trait_name = trait_obj
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    RETIRED_TRAIT_KEYS
        .iter()
        .filter(|(key, _)| trait_obj.contains_key(*key))
        .map(|(key, replacement)| RetiredTraitKey {
            path: format!("{path}.{key}"),
            trait_name: trait_name.clone(),
            key,
            replacement,
        })
        .collect()
}

/// Every retired key on the traits of `orbitals[*].traits[*]`, in document order.
pub fn retired_trait_keys(doc: &serde_json::Value) -> Vec<RetiredTraitKey> {
    let mut found = Vec::new();
    let Some(orbitals) = doc.get("orbitals").and_then(serde_json::Value::as_array) else {
        return found;
    };
    for (oi, orbital) in orbitals.iter().enumerate() {
        let Some(traits) = orbital.get("traits").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for (ti, trait_value) in traits.iter().enumerate() {
            if let Some(obj) = trait_value.as_object() {
                found.extend(retired_keys_of(
                    obj,
                    &format!("orbitals[{oi}].traits[{ti}]"),
                ));
            }
        }
    }
    found
}

impl<'de> Deserialize<'de> for TraitReference {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        // Buffer the value, then dispatch on the discriminator. An inline trait
        // is the only shape that carries a `stateMachine` (it's a required field
        // on `TraitDefinition`); a named reference never does. Anything with a
        // `ref` key but no `stateMachine` is also a reference. Deserializing
        // exactly ONE concrete variant (not serde's untagged trial of both)
        // makes the resulting error name the offending field (`unknown field`,
        // `missing field`, type mismatch) rather than the opaque "did not match
        // any variant of untagged enum".
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value.as_object().ok_or_else(|| {
            D::Error::custom("trait reference must be a JSON object (inline trait or { ref: ... })")
        })?;

        if let Some(retired) = retired_keys_of(obj, "").into_iter().next() {
            return Err(D::Error::custom(retired.message()));
        }

        let is_reference = obj.contains_key("ref") || !obj.contains_key("stateMachine");
        if is_reference {
            let f: TraitReferenceFields =
                serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(TraitReference::Reference {
                reference: f.reference,
                ref_id: f.ref_id,
                id: f.id,
                name: f.name,
                linked_entity: f.linked_entity,
                linked_entity_id: f.linked_entity_id,
                config: f.config,
                events: f.events,
                event_ids: f.event_ids,
                fields: f.fields,
                listens: f.listens,
                emits_scope: f.emits_scope,
                type_args: f.type_args,
                dispatch: f.dispatch,
            })
        } else {
            let td: TraitDefinition = serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(TraitReference::Inline(Arc::new(td)))
        }
    }
}

/// Inferred field contract for a `@rebindable` trait binding. `requires`
/// = entity fields the trait READS via `@entity.<field>`; `provides` =
/// fields it WRITES via `(set @entity.<field> ...)`. A consumer that
/// rebinds the trait (`linkedEntity`) must point at an entity whose
/// fields ⊇ `requires`. Inferred by the lolo lowerer from the trait's own
/// SExpr usage; the L2 validator enforces it (`ORB_T_REBIND_MISSING_FIELDS`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityFieldContract {
    // Always emit both arrays (no skip_serializing_if) — the @almadar/core
    // zod validator (EntityFieldContractSchema) requires BOTH `requires` and
    // `provides` keys. Skipping an empty array produces a partial object that
    // fails strict validation downstream even when the field is semantically
    // empty.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
}

/// Full trait definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraitDefinition {
    /// Trait name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Opt-in marker authored as `-> @rebindable Entity` in `.lolo`. When
    /// `Some(true)`, a consuming molecule/organism may rebind this trait's
    /// entity via `linkedEntity`; otherwise the binding is fixed and the
    /// validator rejects any rebind (`ORB_T_ENTITY_NOT_REBINDABLE`). Absent
    /// on traits that don't bind an entity.
    #[serde(
        rename = "entityRebindable",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_rebindable: Option<bool>,

    /// Inferred field contract a rebind target must satisfy. Emitted only
    /// alongside `entity_rebindable: Some(true)`.
    #[serde(
        rename = "entityContract",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_contract: Option<EntityFieldContract>,

    /// Inferred effect surface — every effect this trait produces, tagged with
    /// the resource it acts on and whether it sits in a transition or a `ticks`
    /// block. Derived by the lolo lowerer from the trait's own effects (never
    /// authored, the way `entity_contract` is never authored), then re-resolved
    /// post-inline once call-site config makes `@config.*` resources concrete.
    /// Absent on traits that produce no effects at all.
    #[serde(rename = "effectRow", default, skip_serializing_if = "Option::is_none")]
    pub effect_row: Option<EffectRow>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Linked entity name
    #[serde(rename = "linkedEntity", skip_serializing_if = "Option::is_none")]
    pub linked_entity: Option<String>,

    /// V4 dual-carry id sibling of `linkedEntity` — optional until the Phase-7 flip.
    #[serde(
        rename = "linkedEntityId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity_id: Option<String>,

    /// V4 leverage-ids — id side-map for `@entity.<Name>` binding tokens + positional
    /// entity args (`fetch`/`ref`/`persist`/`spawn`) appearing in this trait's guards /
    /// effects / ticks / listens. Maps the referenced entity NAME → its stable entity id,
    /// minted at stamp from the orbital's entity id map. Readers resolve entity refs by
    /// this id (else the name), so a rename of the entity declaration never strands these
    /// bare-string tokens — this is what lets the `rewriteEntityInInlineTrait` name-rewrite
    /// be deleted. Present only when the trait references an entity in an expression.
    #[serde(
        rename = "entityRefIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_ref_ids: Option<crate::collections::IndexMap<String, String>>,

    /// V4 leverage-ids — id side-map for `@trait.<Name>` embed tokens (render-ui embeds /
    /// effect-tree sibling references). Maps the embed token NAME → the embedded trait's
    /// stable id, minted at stamp from the orbital's trait id map. Readers splice embeds by
    /// this id (else the name), which is what lets the `@trait.` name-rewrite be deleted.
    /// Present only when the trait embeds a sibling render trait.
    #[serde(
        rename = "traitEmbedIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub trait_embed_ids: Option<crate::collections::IndexMap<String, String>>,

    /// Events this trait emits
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emits: Option<crate::value::Shared<Vec<EmitDefinition>>>,

    /// Events this trait listens to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listens: Option<crate::value::Shared<Vec<ListenDefinition>>>,

    /// State machine definition
    #[serde(rename = "stateMachine")]
    pub state_machine: crate::value::Shared<StateMachine>,

    /// Scheduled ticks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticks: Option<Vec<TickDefinition>>,

    /// Configuration schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::collections::IndexMap<String, ConfigField>>,

    /// Data entities created by this trait
    #[serde(rename = "dataEntities", skip_serializing_if = "Option::is_none")]
    pub data_entities: Option<Vec<EntityDefinition>>,

    /// Declared dispatch mode (`[client]` / `[optimistic]` in the `.lolo` trait
    /// header). Absent = server: transitions await the server round trip.
    /// Nothing about dispatch is inferred from effects or entity residence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<DispatchMode>,

    /// Set by the inline phase when this trait was cloned from a `uses[]`
    /// import. Absent for traits authored directly on the orbital.
    /// Viewport-only metadata — runtime + codegen ignore it. Studio's L2
    /// canvas buckets cards by alias to give imported behaviors their own
    /// drill-down level. See `docs/Almadar_Studio_Gaps.md` STUDIO-1.
    #[serde(rename = "sourceBehavior", skip_serializing_if = "Option::is_none")]
    pub source_behavior: Option<SourceBehaviorMetadata>,

    #[serde(
        rename = "sourceDeclaration",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_declaration: Option<SourceDeclarationMetadata>,

    /// Set by the inline phase alongside `source_behavior`. Carries the
    /// resolved `EntityDefinition` of the imported behavior so the
    /// client-side mock-data generator can produce rows for the trait's
    /// `linked_entity` without re-walking the import graph at runtime.
    /// See `docs/Almadar_Studio_Gaps.md` STUDIO-2.
    #[serde(
        rename = "sourceEntityDefinition",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_entity_definition: Option<Arc<EntityDefinition>>,

    /// Increment 3-II: this trait's own declared type parameters —
    /// `(V : Type, R : Type)` in `trait Name (V : Type, R : Type) -> Entity
    /// { ... }`. Present only on an atom's DECLARING inline trait (never
    /// authored on a `Reference`); survives cloning into a consumer's
    /// composed schema so the inline phase can read each param's `kind`
    /// when deciding how to resolve a surviving `"$<name>"` sentinel.
    #[serde(
        rename = "typeParams",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub type_params: Option<Vec<TraitTypeParamDef>>,

    /// Increment 3-II: TRANSIENT carrier for the call-site type-parameter
    /// arguments a `Reference`'s `type_args` supplied, stashed here by the
    /// inline phase the moment the reference is resolved to this cloned
    /// `TraitDefinition` (`phases::inline::trait::inline_traits`) so they
    /// survive to the later per-orbital sentinel-resolution pass
    /// (`phases::inline::rewrite::resolve_orbital_type_param_sentinels`),
    /// which needs the orbital's FULLY composed entity set to resolve an
    /// entity-valued argument. That pass clears this field after consuming
    /// it — it never appears in a final emitted `.orb`.
    #[serde(rename = "typeArgs", default, skip_serializing_if = "Option::is_none")]
    pub type_args: Option<crate::collections::IndexMap<String, String>>,
}

impl TraitDefinition {
    /// Every proposition the trait states: each state's `@holds` and each transition's `@ensures`. They read the same
    /// roots as a guard, so every rewrite of the trait's guards (renames, config fold) applies to them too.
    pub fn propositions_mut(&mut self) -> impl Iterator<Item = &mut SExpression> {
        self.state_machine.propositions_mut()
    }
}

impl TraitDefinition {
    /// Client dispatch strategy for this trait — the schema-side entry point
    /// to the same [`crate::dispatch_mode::compute`] rule
    /// `ir::types::OirTrait::dispatch_mode` calls, so the compiled path and
    /// the runtime kernel can never disagree.
    pub fn dispatch_mode(
        &self,
        _entity: Option<&EntityDefinition>,
        _residence: &crate::runs_on::Residence,
    ) -> crate::dispatch_mode::DispatchMode {
        crate::dispatch_mode::compute(self.dispatch)
    }

    /// Whether this trait's own effects reach the server: any transition, or
    /// any client tick (a `[background]` tick runs on the host, not a dispatch).
    /// A data effect on a client-resident entity (`residence`) is client work.
    pub fn touches_server(&self, residence: &crate::runs_on::Residence) -> bool {
        use crate::runs_on::sexpr_runs_server_effect;
        let server = |e: &SExpression| sexpr_runs_server_effect(e, residence);
        self.state_machine
            .transitions
            .iter()
            .filter(|t| !t.guard.as_ref().is_some_and(|g| g.is_statically_false()))
            .any(|t| t.effects.iter().flatten().any(server))
            || self
                .ticks
                .iter()
                .flatten()
                .filter(|t| !t.background)
                .any(|t| t.effects.iter().any(server))
    }

    /// Every entity field NAME this trait's own body reads or writes — the
    /// union of `@entity.<field>` refs and `set @entity.<field>` writers
    /// across every guard, transition effect, tick guard/effect, and
    /// config-field default. Reuses the single canonical walker
    /// `SExpression::collect_entity_field_refs` (the same one
    /// `phases/validation/entity_mutation.rs`'s `collect_entity_field_reads`
    /// and `binding.rs` use in the compiler) — no second S-expression
    /// walker. Entity-agnostic: this says which field NAMES the trait's own
    /// logic touches, not which entity they belong to — a caller matches the
    /// result against a specific `EntityDefinition`.
    pub fn referenced_entity_field_names(&self) -> HashSet<String> {
        let mut referenced: HashMap<String, String> = HashMap::new();
        let mut set_fields: HashSet<String> = HashSet::new();
        for transition in &self.state_machine.transitions {
            if let Some(guard) = &transition.guard {
                guard.collect_entity_field_refs(&mut referenced, &mut set_fields, "GUARD");
            }
            if let Some(effects) = &transition.effects {
                for effect in effects {
                    effect.collect_entity_field_refs(&mut referenced, &mut set_fields, "EFFECT");
                }
            }
        }
        if let Some(ticks) = &self.ticks {
            for tick in ticks {
                if let Some(guard) = &tick.guard {
                    guard.collect_entity_field_refs(&mut referenced, &mut set_fields, "TICK_GUARD");
                }
                for effect in &tick.effects {
                    effect.collect_entity_field_refs(&mut referenced, &mut set_fields, "TICK");
                }
            }
        }
        if let Some(config) = &self.config {
            for field in config.values() {
                if let Some(default) = &field.default {
                    default.collect_entity_field_refs(
                        &mut referenced,
                        &mut set_fields,
                        "CONFIG_DEFAULT",
                    );
                }
            }
        }
        let mut out: HashSet<String> = referenced.into_keys().collect();
        out.extend(set_fields);
        out
    }
}

/// Provenance metadata attached to a trait that was cloned from a `uses[]`
/// import during the inline phase. Serialised on the resolved `.orb` JSON
/// so Studio can group L2 cards by source behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBehaviorMetadata {
    /// Behavior name (e.g. `"std-stat-card"`) — the resolved `from:` value.
    pub behavior: String,

    /// Alias used at the import site (e.g. `"Stat"`). Multiple imports of
    /// the same atom under different aliases group separately.
    pub alias: String,

    /// Original trait name inside the imported behavior, before any
    /// call-site `name:` rename.
    pub original_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeclarationMetadata {
    pub trait_id: String,
    pub fingerprint: String,
}

/// Declared dispatch mode of a trait. Absent on the trait = server; `Server`
/// is the explicit spelling, so a call site can override an atom's mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DispatchMode {
    /// Transitions run client-side with no server round trip.
    Client,
    /// Transitions commit locally, then post to the server and fold its reply.
    Optimistic,
    /// Transitions await the server round trip (`[server]`).
    Server,
}

/// Configuration field definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    /// `field R T`: the record `R` as a type string (`@entity`, `@entity(<knob>)`, `@payload(<knob>)`, an entity or
    /// alias name, `$r`). A record alias carries its shape in `properties`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(
        default,
        rename = "additionalProperties",
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<Box<FieldDefinition>>,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_default"
    )]
    pub default: Option<SExpression>,
    /// Human-readable question prompt for the studio's questionnaire.
    /// Authored in `.lolo` as `@label "..."` on the line below the
    /// config field declaration. When unset, the questionnaire falls
    /// back to a humanized version of the key name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Long-form description / help text shown under the label in the
    /// studio's questionnaire. Authored as `@description "..."` on the
    /// line below the config field declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// User-vocabulary synonyms — comma-separated free text authored
    /// in `.lolo` as `@synonyms "..."` on the line below the config
    /// field declaration. Threaded through the .orb pipeline so the
    /// agent's catalog-summary prompt + the knob-embeddings bake can
    /// widen recall on prompts that name the knob via synonym
    /// ("taller" → `height`, "highest to lowest" → `sortDir=desc`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,
    /// Visibility tier — authored in `.lolo` as `@tier "..."` on the
    /// line below the config field declaration. One of `essential`,
    /// `customization`, `advanced`, `internal`. Drives the studio
    /// Questionnaire's filter + disclosure: `internal` is hidden
    /// entirely, `advanced` collapses behind a "Show advanced" toggle.
    /// Validated by the L2 `ORB_F_TIER_INVALID` rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Literal-union variants for fields declared as `"a" | "b" | "c"` in
    /// the .lolo source. Mirrors the entity-side `FieldDefinition.values`
    /// shape — preserving the closed set of legal values through the .orb
    /// pipeline so downstream consumers (the std-functions regenerator,
    /// validators, agent tool schemas) can produce literal-union TS types
    /// instead of falling back to bare `string`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// Array item schema for `[T]` config slots — mirrors
    /// `FieldDefinition.items`. Populated by lowering named array types
    /// (e.g. `metrics : [MetricSpec]`) so consumers see the full
    /// item shape, not just the `"[MetricSpec]"` type tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<FieldDefinition>>,
    /// Object property schema for inline / named object config slots
    /// — mirrors `FieldDefinition.properties`. Populated when the config
    /// type resolves to a `TypeExpr::Object` (struct alias or literal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<crate::collections::IndexMap<String, FieldDefinition>>,
    /// `!` required marker — authored in `.lolo` as `fields : [FieldSpec]!`.
    /// Declares that call sites referencing this trait MUST supply this
    /// knob's key in their `config` block (any value form). Coexists with
    /// `default`: the default serves the atom's own demo page binding,
    /// which is exempt from the requirement. Validated by the L2
    /// call-site config rules (unknown key / missing required key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// V4-W5 dual-carry id — set by the stamp when `field_type` is a reference
    /// config type (`entity`/`trait`/`event`): the stable id of the node the
    /// field's `default` value names. Readers (compiler `identity_normalize` +
    /// runtime resolver) resolve the value to that node's CURRENT name by id, so
    /// a declaration-only rename flows through a config value. Absent otherwise.
    #[serde(rename = "refId", skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    /// The declared field this knob's type was projected from — `.lolo`
    /// `T.f`; see [`FieldDefinition::projected_from`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
    /// The original `@config.<knob>` forward token this field's `default`
    /// collapsed FROM, when the inline phase resolved a forward against the
    /// orbital/schema rung (`forwarded_sibling_config_from`,
    /// `orbital-compiler`'s `phases::inline`). Set only at the moment of
    /// collapse, only when the value truly came from a forward — never
    /// stamped on a field whose default was always concrete. Carries the
    /// provenance the RESOLVED `.orb`'s `default` alone can no longer show
    /// (the literal token is gone, replaced by its resolved value), so
    /// `ORB_O_CONFIG_DEAD_KNOB`'s forwarded-anywhere check — which runs on
    /// the resolved schema, in both the Rust and JS (`lintWiring`
    /// `orbital-config-knob-unforwarded`) compilers — can still see that the
    /// declaring knob was consumed. One owner, in the IR, so the provenance
    /// crosses the wire instead of living in a side channel per language.
    #[serde(rename = "forwardedFrom", skip_serializing_if = "Option::is_none")]
    pub forwarded_from: Option<String>,
    /// The `@config.<knob>` tokens nested INSIDE this field's value (an
    /// array element or object member, e.g. `metrics[].target`) that the
    /// inline phase resolved to their values — the list twin of
    /// `forwarded_from`, so `ORB_O_CONFIG_DEAD_KNOB` still sees those knobs
    /// as read once the tokens are gone. Empty when nothing nested resolved.
    #[serde(
        rename = "nestedForwardedFrom",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub nested_forwarded_from: Vec<String>,
    /// The definer knob name this field is a payload COMPANION of, when
    /// this field's declared type was `@payload(<knob>)` — set by
    /// `resolve_sigil_types` at the moment it rewrites `field_type` from
    /// the sentinel to the resolved payload type name (`GeneratePayload`),
    /// the same way `forwarded_from` preserves a provenance the resolved
    /// `default`/`field_type` can no longer show on their own. `None` for
    /// any config field that was never a `@payload(<knob>)` sentinel.
    #[serde(rename = "payloadFor", skip_serializing_if = "Option::is_none")]
    pub payload_for: Option<String>,
    /// A call-site entry that wires a value into a knob the trait declares (never a declaration of its own).
    /// `field_type` then carries the SOURCE knob's declared type for an `@config.<knob>` wiring, and `"unknown"`
    /// for a literal value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wired: bool,
}

impl ConfigField {
    /// True when this entry is a fully-annotated DECLARATION (a NEW typed
    /// knob) rather than a plain call-site wiring/override of an already
    /// declared knob. A wiring entry is marked `wired` by the lowerer
    /// (`orbital-lolo/src/lower/trait.rs::lower_trait_reference`); an unmarked
    /// entry of type `"unknown"` with no label/description/synonyms/tier is
    /// the same wiring in hand-written IR. Anything else — a real type tag or
    /// any annotation — only comes from a `.lolo` declaration
    /// (`key : Type = default @label "..."`).
    pub fn is_declaration_form(&self) -> bool {
        !(self.wired
            || (self.field_type == "unknown"
                && self.label.is_none()
                && self.description.is_none()
                && self.synonyms.is_none()
                && self.tier.is_none()))
    }
}
