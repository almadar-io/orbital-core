//! Traits, their states and the events they take.

use super::*;

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
