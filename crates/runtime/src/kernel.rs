//! Runtime kernel — canonical runtime lifecycle for Orbital applications.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::dispatch_memory::{DispatchMemory, DispatchView};
use super::embed_index::EmbedIndex;
use super::entity_access::{
    apply_read_access, scope_fetched_rows, EntityAccessPolicies, EntityAccessTable,
};
use super::entity_source::{EntitySource, QueryAnswer};
use super::listener::{ListenerManager, TriggerCallback};
use super::process_result::{
    DeliveryEntry, EffectRun, EffectRunStatus, EntitySummary, OrbitalDetail, OrbitalInfo,
    null_payload, store_row_id, ProcessEventResult, RuntimeStats, RUNTIME_SINGLETON,
};
use super::render_store::RenderStore;
use super::resumable::{CallRoutes, CallSite, CallVerdict, DeferredCall, IoPort};
use orbital_core_base::dispatch_mode::DispatchMode;

use super::row_query::{
    and_predicates, compile_predicate, default_row_order, query_unsupported_reason,
    resolve_row_set, NotExpressible, PredicateRole, QueryCapabilities, RowPage, RowWriteBatch,
    RowWriteOp, RowWriteOutcome, WriteCapabilities,
};
use super::server_leg::ServerLegCollector;
use super::shared_entity::SharedEntity;
use crate::config::KernelConfig;
use crate::effects::executor::page_ordered_rows;
use crate::effects::executor::ServiceCaller;
use crate::effects::executor::{EffectExecutor, FetchHandler, FetchPage, PersistenceHandler};
use crate::effects::response::{delivery_record_value, DeliveryRecord, DispatchLog};
use crate::effects::response::{EventRequest, EventResponse, LegResidence, TraitFrom, TransitionRejectionCode};
use orbital_core_base::bus_event::BusEventSource;
use orbital_core_base::codec;
use orbital_core_base::effect_types::PersistOperation;
use orbital_core_base::effect_types::{
    Effect, EffectOutcome, EffectResult, FetchEmit, RuntimeEnvironment,
};
use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::events::bus::{shared_bus, SharedEventBus};
use orbital_core_base::os::{OsError, OsErrorKind};
use orbital_core_base::os_grants::OsGrants;
use orbital_core_base::row_set::{
    nearest_rows, RowNearest, RowOrder, RowPredicate, RowQuery, RowSetRef, RowTotalMode,
};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::{
    EntityDefinition, EntityRef, OrbitalSchema, Persistence, TraitDefinition, TraitReference,
};
use orbital_core_base::theme::{
    resolve_theme, resolve_theme_default, ResolvedTheme, ThemeMode, ThemePresets,
};
use orbital_core_base::value::{Shared, Value};
use orbital_core_base::value::ValueMap;
use orbital_core_eval::evaluator::context::{BindingContext, BindingContextBuilder, EntropySource, SealedOutputs};
use orbital_core_eval::evaluator::Evaluator;
use orbital_core_eval::state_machine::manager::StateMachineManager;
use orbital_core_eval::state_machine::types::{EventPayload, TraitState};
use orbital_core_eval::util::visit_key::DispatchVisits;
use orbital_core_base::sync::RwLock;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, info};

/// The most bytes one `random-bytes` may ask for.
const RANDOM_BYTES_MAX: f64 = 1024.0;

/// Why a shell entry constructor (`EmbeddedServer::new`, `EmbeddedKernel::new`, `KernelBridge::new`,
/// `ClientKernel::new`, the forks) refused its program.
#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    #[error("resolved schema declares no orbitals")]
    NoOrbitals,
    /// The kernel evaluates through the installed language; a host with no packages must install its bundle first.
    #[error("{}: {}", .0.code(), .0)]
    Language(#[source] orbital_core_base::language_data::LanguageDataError),
    #[error("register_schema: {0}")]
    Register(#[source] KernelError),
    #[error("fork_session: {0}")]
    ForkSession(#[source] KernelError),
    #[error("fork_program: {0}")]
    ForkProgram(#[source] KernelError),
}

/// Errors returned by the runtime kernel.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("Trait not found: {0}")]
    TraitNotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),

    /// The event's payload does not satisfy the payload its trait declares; the message is the API response's text.
    #[error("{0}")]
    PayloadInvalid(String),

    /// A call carrying the turn of a dispatch parked holding the lane (EXEC-CONC-0): it would wait on itself
    /// forever, so it is refused until that dispatch is resumed or its continuation dropped.
    #[error("dispatch #{ticket} ({event}) is parked holding the dispatch lane under this turn; resume or drop it before starting another dispatch")]
    LaneHeldByParkedDispatch { ticket: u64, event: String },

    /// The program carries no type-contract stamp (`resolved` / `typeContractVersion`), so it was never resolved
    /// under the contract this kernel checks values by.
    #[error("program '{program}' carries no type-contract stamp; resolve it again from source")]
    UnresolvedProgram { program: String },

    /// A lazy orbital could not be loaded when a request or event reached it (EXEC-LOAD-2).
    #[error("lazy orbital `{orbital}` could not be loaded: {reason}")]
    LazyLoad { orbital: String, reason: String },
}

pub mod agent_ports;
pub mod dispatch_run;
pub(crate) mod envelope;
pub mod inbound;
pub(crate) mod lane;
pub use lane::LaneTurn;
pub mod resolve_run;
pub(crate) use dispatch_run::Fault;

/// Circuit breaker for one dispatch's cross-trait worklist; twin of the JS `CROSS_TRAIT_CASCADE_CAP`.
pub const CROSS_TRAIT_CASCADE_CAP: usize = 2000;

/// The hard bound on one dispatch's deliveries in total, across every settle pass (EXEC-QUEUE-4, EXEC-CONC-2).
pub const CROSS_TRAIT_DISPATCH_CAP: usize = 1_000_000;

/// One queued delivery: the trait it reaches, the event it triggers there, and what `@event` reads.
struct CascadeItem {
    trait_name: String,
    event: String,
    /// Shared across one fan-out's targets (G-ORBITAL-CORE-051); a delivery copies it only when processed.
    payload: Arc<Value>,
    /// The digest of `payload` shared by one fan-out's targets, filled by the first delivery that visits.
    payload_digest: Option<Arc<std::sync::OnceLock<u128>>>,
    delivery: Arc<Value>,
    /// The emit's own payload, which a listens `when` reads as `@payload`.
    source_payload: Arc<Value>,
    /// Evaluated when the delivery is processed, so it sees the listener's log.
    listen_guard: Option<Arc<SExpression>>,
    /// Whether processing this item offers its event to the listeners of that event. A listener's
    /// delivery runs its `triggers` event privately: only the seed and `emit`s reach listeners.
    fans_out: bool,
    /// Set for a re-render of an embedded child under its composer's payload, not a delivery.
    repaint: Option<RepaintJob>,
    /// The reply or failure an effect's completion fired: a distinct delivery, never skipped as a repeat.
    completion: bool,
    /// A listener's `per` clause, evaluated against `source_payload` when the delivery is processed.
    listen_key: Option<ListenKey>,
    /// The row this delivery steps: the evaluated `per` key, or the row of the step that queued it.
    row: Option<String>,
}

impl CascadeItem {
    fn payload_digest(&self) -> u128 {
        match &self.payload_digest {
            Some(cell) => *cell.get_or_init(|| orbital_core_eval::util::visit_key::payload_digest(&self.payload)),
            None => orbital_core_eval::util::visit_key::payload_digest(&self.payload),
        }
    }
}

/// A `per` clause awaiting evaluation, with the trait whose emit the listener heard (who hears a refusal).
#[derive(Clone)]
struct ListenKey {
    expr: Arc<SExpression>,
    emitter: String,
}

/// An embedded child to re-render under the payload of the transition that composed it
/// ([`callsite_capture`](super::callsite_capture)); `CascadeItem::trait_name` is the child.
#[derive(Clone)]
struct RepaintJob {
    /// The composing transition's payload.
    composed_with: Value,
    /// Walking through a child that was not repainted: its children keep the payload they were last composed with.
    keep_composed: bool,
    /// The children this walk has repainted, shared by every job of one walk.
    visited: Arc<orbital_core_base::sync::Mutex<HashSet<String>>>,
}

/// In which session a dispatch runs and who that session is: each request thread's session, each session's bound
/// identity, and the sessions that have ended.
struct Sessions {
    /// The session a request's thread dispatches in (set by [`SessionGuard`]): a frame belongs to the session whose
    /// dispatch made it (EXEC-DIST-9). A thread with none is in the program session.
    session_by_thread: Arc<RwLock<HashMap<orbital_core_base::sync::ThreadKey, Arc<str>>>>,
    scratch_sessions: std::sync::atomic::AtomicU64,
    /// `@user` of a session with no bound identity.
    anonymous: Value,
    /// Each session that took part in the identity effects, and who it is now (EXEC-IDENTITY-1). The program session
    /// is the empty key. Freed with the session.
    identities: Arc<RwLock<HashMap<String, identity_session::IdentityBinding>>>,
    /// Sessions whose states and frames were freed (EXEC-DIST-9); a request but a mount to one is refused. Never pruned.
    ended_sessions: Arc<RwLock<HashMap<String, session_life::EndedSession>>>,
}

/// A trait never receives a routed event before its own lifecycle event
/// (INIT/LOAD/$MOUNT) has run on the current mount: a delivery reaching a
/// mounted-but-not-yet-initialized trait is held and re-enqueued right after
/// that trait's lifecycle step. JS twin: `evaluation/mount-lifecycle.ts`.
#[derive(Default)]
struct MountLifecycle {
    awaiting: HashSet<String>,
    held: HashMap<String, Vec<CascadeItem>>,
}

impl MountLifecycle {
    fn holds(&self, item: &CascadeItem) -> bool {
        !orbital_core_base::events::is_bus_excluded_event(&item.event)
            && self.awaiting.contains(&item.trait_name)
    }

    fn hold(&mut self, item: CascadeItem) {
        self.held
            .entry(item.trait_name.clone())
            .or_default()
            .push(item);
    }

    /// A server host follows the client's mount: `awaiting` is what the client
    /// still awaits when it posts. Held deliveries for a trait the client has
    /// since initialized are released (its INIT may never reach this host),
    /// except the request's own lifecycle target, held until its step runs.
    fn sync(&mut self, awaiting: HashSet<String>, lifecycle_targets: &[&str]) -> Vec<CascadeItem> {
        let mut next = awaiting;
        for t in lifecycle_targets {
            if self.awaiting.contains(*t) {
                next.insert(t.to_string());
            }
        }
        let mut released = Vec::new();
        for t in self.awaiting.iter().filter(|t| !next.contains(*t)) {
            released.extend(self.held.remove(t).unwrap_or_default());
        }
        self.awaiting = next;
        released
    }

    /// `trait_name` just ran `event`; when that is its lifecycle step, its held deliveries go out in arrival order.
    fn release_after(&mut self, trait_name: &str, event: &str) -> Vec<CascadeItem> {
        if !orbital_core_base::events::is_bus_excluded_event(event)
            || !self.awaiting.remove(trait_name)
        {
            return Vec::new();
        }
        self.held.remove(trait_name).unwrap_or_default()
    }
}

/// Split a leg payload's `_awaitingInit` sidecar (the client's mount) from the payload proper.
fn take_awaiting_sidecar(payload: Value) -> (Value, Option<HashSet<String>>) {
    let Value::Object(mut map) = payload else {
        return (payload, None);
    };
    let awaiting = map.shift_remove("_awaitingInit").map(|v| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    });
    let payload = if map.is_empty() && awaiting.is_some() {
        Value::Null
    } else {
        Value::Object(map)
    };
    (payload, awaiting)
}

/// Put a client's mount back on a payload (the inverse of [`take_awaiting_sidecar`]).
fn with_awaiting_sidecar(payload: Value, awaiting: &HashSet<String>) -> Value {
    let mut map = match payload {
        Value::Object(map) => map,
        _ => ValueMap::default().into(),
    };
    let mut list: Vec<&String> = awaiting.iter().collect();
    list.sort();
    map.insert(
        "_awaitingInit".into(),
        Value::Array(list.into_iter().map(|t| Value::string(t.clone())).collect()),
    );
    Value::Object(map)
}

fn listen_guard_passes(
    guard: &SExpression,
    source_payload: &Value,
    from: &str,
    config: Value,
    view: &DispatchView,
) -> bool {
    let mut ctx = BindingContextBuilder::new()
        .payload(source_payload.clone())
        .state(Value::string(from.to_string()))
        .build();
    ctx.from_state = Some(std::sync::Arc::from(from));
    ctx.set_config(config);
    view.apply(&mut ctx);
    matches!(Evaluator::new().evaluate_guard(guard, &ctx), Ok(true))
}

/// What the in-flight dispatch and its running step hold: the one `now` stamp, the viewer locale, and the
/// trait, follow-on flag and frame its call sites are stamped with.
struct StepScope {
    /// The in-flight dispatch's one `now` stamp; set by the outermost entry, reused by nested steps.
    dispatch_now: Arc<RwLock<Option<f64>>>,
    /// The in-flight dispatch's viewer locale (`EventRequest.locale`), held by a [`LocaleGuard`].
    dispatch_locale: Arc<RwLock<Option<String>>>,
    /// The trait whose effects are running and whether its step is a follow-on of the dispatch:
    /// what a `call-service` is stamped with.
    step_site: Arc<RwLock<Option<(String, bool)>>>,
    /// The frame the step in `step_site` runs on, for the call sites it creates.
    step_frame: Arc<RwLock<Option<String>>>,
    /// The `let` scope each `render-ui` of the running step ran under, by its position in the step's results.
    render_scopes: Arc<RwLock<Vec<(usize, orbital_core_eval::evaluator::context::LocalScope)>>>,
    /// The identity change the running step made (session key, binding): applied when the step commits and dropped
    /// when it suspends or fails, so a replayed step starts from the identity the first attempt started from.
    identity_change: RwLock<Option<(String, identity_session::IdentityBinding)>>,
}

/// What the registered programs present with: their theme presets, the viewer's light/dark mode, and the root nav
/// entries `@pages` resolves to.
struct Presentation {
    /// The theme presets the registered programs carry (`themeDefinitions`,
    /// written by `orb` when it compiles a program) — the kernel has no
    /// filesystem access at runtime. Additive across registrations.
    theme_presets: Arc<RwLock<ThemePresets>>,
    /// The kernel's tracked light/dark toggle, independent of what a
    /// declared theme's key suffix says — `@currentTheme` resolution honors
    /// this over the declared suffix when EXPLICITLY set via
    /// `set_theme_mode`. `None` (never set) lets `resolve_theme` fall back
    /// to the declared theme key's own suffix instead of silently forcing
    /// Light (§157/C7 `theme-declaration`: a `-dark` key must not be
    /// overridden by an ambient default no one asked for).
    theme_mode: Arc<RwLock<Option<ThemeMode>>>,
    /// `@pages`: the app's root nav entries over every registered program; the empty list while none
    /// declares a labeled root page (LOLO "Nav entries").
    nav_pages: Arc<RwLock<Value>>,
}

/// The registered programs as the kernel looks them up: each program's schema, where each trait lives and its
/// definition, and each entity's definition, defaults, access policies and embedded rows.
struct Registry {
    /// Shared with every fork (EXEC-DIST-5): a registration replaces the entry, never mutates it.
    schemas: Arc<RwLock<HashMap<String, Arc<OrbitalSchema>>>>,
    /// Trait name to the schema and orbital index that declare it, rebuilt on every registration.
    trait_homes: Arc<RwLock<HashMap<String, (String, usize)>>>,
    trait_definitions: Arc<RwLock<HashMap<String, Arc<TraitDefinition>>>>,
    /// Every registered inline `EntityDefinition`, keyed by entity name —
    /// populated additively alongside `entity_policies` in `register_schema`
    /// (same loop, same inline-only/additive-across-registrations contract).
    /// Policy and residence readers resolve a trait's linked entity here;
    /// `DispatchMode` never does (it is the trait's declared `dispatch`).
    entity_definitions: Arc<RwLock<HashMap<String, Arc<EntityDefinition>>>>,
    /// Each entity type's declared field defaults as one object (`Null` when none), built when the
    /// schema registers; `read_trait_entity` layers a row and a frame over it.
    entity_defaults: Arc<RwLock<HashMap<String, SharedEntity>>>,
    /// Schema-derived `@read`/`@create`/`@update`/`@delete` directives, keyed
    /// by entity name — populated at `register_schema`, shared with the
    /// `EffectExecutor` (write side) and `EntitySourceHandler` (read side) so
    /// every entity mutation/fetch is checked no matter which trait issues it.
    entity_policies: Arc<RwLock<EntityAccessTable>>,
    /// Inverse of every registered trait's `trait_embed_ids` (§158), rebuilt
    /// from the full accumulated `trait_definitions` at the end of each
    /// `register_schema` call so it stays additive across registrations like
    /// the rest of the kernel's schema-derived tables. `dispatch` reads it to
    /// bubble a seed event up an embedded trait's host chain.
    embed_index: Arc<RwLock<EmbedIndex>>,
    /// Each entity's declared row shape, built on its first row check and dropped on every registration.
    row_shapes: RwLock<HashMap<String, Option<Arc<orbital_core_base::ir::types::DeclaredShape>>>>,
    /// Per emitting trait and event, the payload fields its contract declares as a sealed type (`Verified`) with that
    /// type's name, built when the trait registers: such a field reaches a host as `{ sealed: <type> }` and no more.
    sealed_emit_fields: Arc<RwLock<HashMap<String, HashMap<String, Vec<(String, String)>>>>>,
}

/// What the program may reach outside the kernel: its egress origins, its os grants, and the credentials of
/// the program a host names this kernel the console of.
struct Grants {
    /// The `egress` origins the registered programs declare: the only hosts the `http` effect may reach.
    egress: Arc<RwLock<Vec<String>>>,
    /// The `os/` grants the registered programs declare, unioned.
    os_grants: Arc<RwLock<OsGrants>>,
    /// The credentials of the program a host names this kernel the console of ([`Self::set_managed_program`]).
    managed_credentials: Arc<RwLock<Vec<orbital_core_base::secrets::ProgramCredential>>>,
}

/// Shared runtime kernel.
///
/// Owns schema registration, trait resolution, event processing, effect execution,
/// and listener wiring. `orbital-client` and `orbital-server` wrap this kernel and
/// inject environment-specific entity sources and handlers.
pub struct RuntimeKernel {
    grants: Grants,
    registry: Registry,
    presentation: Presentation,
    step: StepScope,
    config: KernelConfig,
    state_manager: Arc<RwLock<StateMachineManager>>,
    effect_executor: Arc<EffectExecutor>,
    event_bus: SharedEventBus,
    listener_manager: ListenerManager,
    dispatch_observer: RwLock<Option<Arc<crate::dispatch_trace::DispatchObserver>>>,
    entity_source: Arc<dyn EntitySource>,
    /// The door every read, write, service call and substrate operator goes through; `entity_source` is it.
    io: Arc<IoPort>,
    sessions: Sessions,
    /// Each client session's live streams and running requests, and what freeing one drops (EXEC-DIST-9).
    session_lives: Arc<session_life::SessionLives>,
    /// Which traits each client has mounted: the mount-scoped ticks a server-role kernel runs (EXEC-TICK-4).
    mount_ledger: Arc<RwLock<mount_ledger::MountLedger>>,
    /// Each session's client view record: the frames last sent and a version (K5).
    view_records: Arc<view_record::ViewRecords>,
    /// The server sessions' records as this client holds them (K5).
    inbound_views: view_record::InboundViews,
    /// The host runs an emit's listeners whenever it marks the emit `dispatched`, so a fold runs only the client-only ones.
    host_fans_out: std::sync::atomic::AtomicBool,
    /// Record each step's `@entity` after its effects ([`DeliveryEntry::entity_after`]); off unless a verifier asks.
    record_entity_after: std::sync::atomic::AtomicBool,
    /// Whether this host holds the circuit state itself ([`RuntimeKernel::set_stateful`]).
    stateful: std::sync::atomic::AtomicBool,
    /// The program's declared locales (the first is the default) and merged message catalogs.
    i18n: Arc<RwLock<KernelI18n>>,
    /// The program's placement, the envelope ids accepted per sender and the dispatch origin in flight (EXEC-QUEUE-9).
    deliveries: Arc<RwLock<envelope::Deliveries>>,
    /// Referrer trait -> the embedded children to re-render under its payload (`callsite_capture`).
    capture_children: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// The program's lazy orbitals not loaded yet and the source that loads them (EXEC-LOAD-2).
    lazy: lazy::LazyOrbitals,
    /// Trait -> the payload its composer last composed it with: what its own `@callsitePayload` reads.
    composed_callsite: Arc<RwLock<ValueMap>>,
    /// Trait frames (`(set @entity.X)` writes), keyed `(frame key, entity id)`: a `[shared]`
    /// entity's frame is `$shared::<Entity>` for every trait bound to it, any other trait's is
    /// its own name (Runtime Spec Clause 2.3). `@entity` is the row with the frame over it.
    frames: Arc<RwLock<HashMap<(String, String), SharedEntity>>>,
    /// The state a step committed as it suspended on I/O (G-CROSS-259), for the resumable run to carry.
    pub(crate) suspended_commit: Arc<RwLock<Option<ProvisionalCommit>>>,
    /// A store-bound entity's declared `instances`, written through its driver by the next resumable dispatch
    /// before its first step (EXEC-STORE-4): one create batch per entity.
    pub(crate) store_seeds:
        Arc<orbital_core_base::sync::Mutex<std::collections::VecDeque<crate::resumable::StoreAsk>>>,
    /// Frame slot -> the id of the row a carried row named: that trait reads that row under its frame,
    /// not the dispatch's address (G-CROSS-256).
    carried_rows: Arc<RwLock<HashMap<(String, String), String>>>,
    /// The current mount's lifecycle (see [`MountLifecycle`]).
    mount: Arc<RwLock<MountLifecycle>>,
    /// Keyed asks a row's step deferred and the host has not completed, per (trait, session-folded row).
    deferred_asks: RwLock<HashMap<(String, String), Vec<keyed_rows::DeferredAsk>>>,
    /// The order the kernel recorded its deferred asks in.
    deferred_seq: std::sync::atomic::AtomicU64,
    /// Keyed rows (trait, row) of this session whose last pending ask ended since the last step ended.
    settled_rows: RwLock<Vec<(String, String)>>,
    /// Keyed rows dropped and not yet reported to their session's client: (session, frame key).
    removed_rows: RwLock<Vec<(Option<String>, String)>>,
    /// Each mounted trait's last `render-ui`, kept to repaint it when the entity it reads changes.
    renders: Arc<RwLock<RenderStore>>,
    /// The fetch/persist handler over the entity source: where a row-set reference is resolved and the
    /// kernel's own writes are noted for EXEC-RENDER-3.
    rows: Arc<EntitySourceHandler>,
    prob_entropy: EntropySource,
    sealed_outputs: SealedOutputs,
    /// The one dispatch lane (EXEC-CONC-1): a dispatch holds it from entry to commit.
    lane: Arc<lane::DispatchLane>,
    /// Keys of the keyed calls whose asking dispatch is parked without the lane (EXEC-CONC-0/1).
    pub(crate) keyed_parks: Arc<lane::KeyedParks>,
    #[cfg(feature = "wire-drivers")]
    tls: Arc<tls_sockets::TlsSockets>,
}

impl std::fmt::Debug for RuntimeKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeKernel")
            .field("config", &self.config)
            .field("state_manager", &self.state_manager)
            .field("effect_executor", &self.effect_executor)
            .field("event_bus", &self.event_bus)
            .field("listener_manager", &self.listener_manager)
            .field("schemas", &self.registry.schemas)
            .field("trait_definitions", &self.registry.trait_definitions)
            .field("host_fans_out", &self.host_fans_out)
            .finish()
    }
}

/// One `dispatch_with_server_leg` call's outcome for a client-role kernel: the
/// merged client-facing response, the server legs to post (empty when the seed
/// trait's dispatch mode never produces one, or nothing server-only fired), every
/// delivery the cascade executed, the seed `(trait, event)` that started it, the
/// seed trait's resolved [`DispatchMode`], and — for `RuntimeOptimistic` — the
/// pre-dispatch snapshot a caller rolls back to on a disagreeing/erroring server
/// response.
///
/// A leg is addressed to the trait whose own delivery needs the server: the seed
/// itself when its own effects do, otherwise each listener that does (a listener
/// reached through another leg's server-side cascade rides that leg).
#[derive(Debug, Clone)]
pub struct ClientDispatch {
    pub response: EventResponse,
    pub server_legs: Vec<EventRequest>,
    pub outcomes: Vec<(String, ProcessEventResult)>,
    pub seed: (String, String),
    pub mode: DispatchMode,
    pub snapshot: Option<TraitSnapshot>,
    /// The traits awaiting this dispatch's server legs (empty when nothing is posted).
    pub awaiting: Vec<AwaitingTrait>,
    /// The plain leg a stateful host is posted when no effect was delegated.
    pub stateful_leg: Option<StatefulLeg>,
}

/// A step's transition committed while its effects await host I/O: `from` is the state the step is replayed from.
#[derive(Debug, Clone)]
pub(crate) struct ProvisionalCommit {
    pub(crate) trait_name: String,
    pub(crate) entity_id: String,
    pub(crate) from: TraitState,
    pub(crate) to: String,
}

/// The leg of a server-backed dispatch that delegated nothing: the host holds the circuit state,
/// so the event itself is posted for it to advance its copy (`postServerLeg`'s stateful branch).
#[derive(Debug, Clone)]
pub struct StatefulLeg {
    pub leg: EventRequest,
    /// The traits awaiting the host's answer.
    pub awaiting: Vec<AwaitingTrait>,
}

/// A trait awaiting a server round trip, keyed by the transition its local run took
/// (`@almadar/core` `AwaitingTrait`): with `event`, `from` selects that transition's `awaitRender`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AwaitingTrait {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    pub from: String,
}

/// A trait's `render-ui` repainted because the `@entity` it reads changed: only the slots whose
/// content differs, resolved against the entity as it is now. Pure binding resolution, no
/// transition, guard, emit or other effect runs ([`RuntimeKernel::rerender_stale`]).
#[derive(Debug, Clone)]
pub struct Rerender {
    pub trait_name: String,
    pub state: String,
    pub entity: Value,
    pub effects: Vec<Effect>,
    /// The slots of `effects` repainted because the rows a reference in them points at changed
    /// (EXEC-RENDER-3); their content may be unchanged.
    pub rows_changed: Vec<String>,
    /// The slots whose repaint failed to resolve; each keeps its last paint.
    pub faults: Vec<crate::render_store::RepaintFault>,
}

/// One scheduled tick's outcome for a client-role kernel ([`RuntimeKernel::dispatch_tick`]):
/// the tick's own effects, the count of its own server-bound effects that were discarded (a tick
/// has no event to replay them under, as on web), and one [`TickEmit`] per `emit` it ran.
#[derive(Debug, Clone)]
pub struct TickDispatch {
    pub outcome: ProcessEventResult,
    pub discarded_server_effects: usize,
    /// The `runTick` leg for the tick's own data effects on browser-stored entities: the data
    /// authority runs the tick once (a tick has no event for an event leg to replay under).
    pub browser_leg: Option<EventRequest>,
    pub emits: Vec<TickEmit>,
    /// Traits repainted for the entity the tick and its cascade left behind.
    pub rerenders: Vec<Rerender>,
}

/// One `emit` a tick ran: the event and payload, and the [`ClientDispatch`] of its listener
/// cascade (with the leg to post when that cascade reached a server-bound trait).
#[derive(Debug, Clone)]
pub struct TickEmit {
    pub event: String,
    pub payload: Value,
    pub dispatch: ClientDispatch,
}

/// What a processed step queues next: the same-trait cascade items, the listener deliveries, and the
/// positions of the `emit` results those consumed.
pub(super) struct StepFollowups {
    own: Vec<CascadeItem>,
    fanned: Vec<CascadeItem>,
    consumed_emits: Vec<usize>,
}

/// What [`RuntimeKernel::finish_client_dispatch`] needs to build a dispatch's server leg.
struct LegSeed<'a> {
    trait_name: &'a str,
    event: &'a str,
    payload: Value,
    /// The entity id as the caller gave it.
    entity_id: Option<&'a str>,
    client_id: Option<&'a str>,
    tick: Option<&'a str>,
    mode: DispatchMode,
    snapshot: Option<TraitSnapshot>,
    seed_row: &'a Value,
    outcomes: &'a [(String, ProcessEventResult)],
    response: EventResponse,
}

/// A trait instance's state + entity frame at a point in time. Captured by
/// [`RuntimeKernel::snapshot_trait`] before a `RuntimeOptimistic` dispatch
/// commits locally, handed back to [`RuntimeKernel::restore_trait`] to roll
/// back. `Serialize`/`Deserialize` so it can cross the `orbital-ffi` C ABI
/// boundary as JSON (`orbital_kernel_dispatch`'s `snapshot` field,
/// `orbital_kernel_restore`'s input).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitSnapshot {
    pub state: String,
    pub entity: Value,
}


impl RuntimeKernel {
    /// Create a new kernel for the given environment with the supplied entity source.
    pub fn new(
        environment: RuntimeEnvironment,
        entity_source: Arc<dyn EntitySource>,
        config: KernelConfig,
    ) -> Self {
        let event_bus = shared_bus();
        let state_manager = Arc::new(RwLock::new(StateMachineManager::new()));
        let entity_policies: Arc<RwLock<EntityAccessTable>> = Arc::new(RwLock::new(HashMap::new()));
        let io = Arc::new(
            IoPort::new(
                entity_source,
                config.service_handler.clone(),
                config.substrate_handler.clone(),
            )
            .with_clock(config.clock.clone())
            .with_runtime_rows(config.runtime_rows_in_kernel),
        );
        let entity_source: Arc<dyn EntitySource> = io.clone();
        let prob_entropy = entropy_from(Arc::clone(&io));
        let sealed_outputs = sealed_from(Arc::clone(&io));
        let entity_definitions: Arc<RwLock<HashMap<String, Arc<EntityDefinition>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let entity_handler = Arc::new(EntitySourceHandler {
            source: entity_source.clone(),
            io: io.clone(),
            entity_policies: entity_policies.clone(),
            entity_definitions: entity_definitions.clone(),
            written: Arc::default(),
        });
        let mut executor = EffectExecutor::new(environment)
            .with_fetch_handler(entity_handler.clone())
            .with_persistence(entity_handler.clone())
            .with_entity_policies(entity_policies.clone());
        // Threaded BEFORE the `Arc` wrap (`EffectExecutor::with_delegate` is a
        // consuming builder) — every client-role kernel is configured with the
        // ONE `ServerLegCollector` via `KernelConfig.delegate`.
        if let Some(delegate) = config.delegate.clone() {
            executor = executor.with_delegate(delegate);
        }
        if io.has_service() {
            executor = executor.with_service_handler(io.clone());
        } else {
            executor = executor.with_call_canceller(io.clone());
        }
        if io.has_substrate() {
            executor = executor.with_substrate_handler(io.clone());
        }
        let effect_executor = Arc::new(executor);

        let listener_manager = ListenerManager::new(event_bus.clone());

        let frames: Arc<RwLock<HashMap<(String, String), SharedEntity>>> = Arc::new(RwLock::new(HashMap::new()));
        let carried_rows: Arc<RwLock<HashMap<(String, String), String>>> = Arc::new(RwLock::new(HashMap::new()));
        let ended_sessions: Arc<RwLock<HashMap<String, session_life::EndedSession>>> = Arc::default();
        let identities: Arc<RwLock<HashMap<String, identity_session::IdentityBinding>>> = Arc::default();
        let view_records: Arc<view_record::ViewRecords> = Arc::default();
        let mount_ledger: Arc<RwLock<mount_ledger::MountLedger>> = Arc::default();
        let session_lives = Arc::new(session_life::SessionLives::new(
            Arc::clone(&mount_ledger),
            Arc::clone(&view_records),
            Arc::clone(&frames),
            Arc::clone(&carried_rows),
            Arc::clone(&state_manager),
            Arc::clone(&ended_sessions),
            Arc::clone(&identities),
        ));
        Self {
            config,
            state_manager,
            effect_executor,
            event_bus,
            listener_manager,
            registry: Registry {
                schemas: Arc::new(RwLock::new(HashMap::new())),
                trait_homes: Arc::new(RwLock::new(HashMap::new())),
                trait_definitions: Arc::new(RwLock::new(HashMap::new())),
                entity_definitions,
                entity_defaults: Arc::new(RwLock::new(HashMap::new())),
                entity_policies,
                embed_index: Arc::new(RwLock::new(EmbedIndex::default())),
                row_shapes: RwLock::new(HashMap::new()),
                sealed_emit_fields: Arc::default(),
            },
            dispatch_observer: RwLock::new(None),
            entity_source,
            io,
            rows: entity_handler,
            prob_entropy,
            sealed_outputs,
            sessions: Sessions {
                session_by_thread: Arc::default(),
                scratch_sessions: std::sync::atomic::AtomicU64::new(0),
                anonymous: identity_session::anonymous_user(),
                identities,
                ended_sessions,
            },
            session_lives,
            mount_ledger,
            view_records,
            inbound_views: Default::default(),
            lane: Arc::default(),
            keyed_parks: Arc::default(),
            #[cfg(feature = "wire-drivers")]
            tls: Arc::default(),
            host_fans_out: std::sync::atomic::AtomicBool::new(false),
            record_entity_after: std::sync::atomic::AtomicBool::new(false),
            stateful: std::sync::atomic::AtomicBool::new(false),
            i18n: Arc::new(RwLock::new(KernelI18n::default())),
            step: StepScope {
                dispatch_now: Arc::new(RwLock::new(None)),
                dispatch_locale: Arc::new(RwLock::new(None)),
                step_site: Arc::new(RwLock::new(None)),
                step_frame: Arc::new(RwLock::new(None)),
                render_scopes: Arc::new(RwLock::new(Vec::new())),
                identity_change: RwLock::new(None),
            },
            grants: Grants {
                egress: Arc::new(RwLock::new(Vec::new())),
                os_grants: Arc::new(RwLock::new(OsGrants::default())),
                managed_credentials: Arc::new(RwLock::new(Vec::new())),
            },
            deliveries: Arc::default(),
            capture_children: Arc::new(RwLock::new(HashMap::new())),
            lazy: lazy::LazyOrbitals::default(),
            composed_callsite: Arc::new(RwLock::new(ValueMap::default())),
            frames,
            carried_rows,
            suspended_commit: Arc::new(RwLock::new(None)),
            store_seeds: Arc::new(orbital_core_base::sync::Mutex::new(std::collections::VecDeque::new())),
            presentation: Presentation {
                theme_presets: Arc::new(RwLock::new(ThemePresets::new())),
                theme_mode: Arc::new(RwLock::new(None)),
                nav_pages: Arc::new(RwLock::new(Value::array(Vec::new()))),
            },
            mount: Arc::new(RwLock::new(MountLifecycle::default())),
            deferred_asks: RwLock::new(HashMap::new()),
            deferred_seq: std::sync::atomic::AtomicU64::new(0),
            settled_rows: RwLock::new(Vec::new()),
            removed_rows: RwLock::new(Vec::new()),
            renders: Arc::new(RwLock::new(RenderStore::default())),
        }
    }

    /// Tell the render store which entity types the kernel has written since it last heard, so the slots
    /// holding a row-set reference to them are repainted (EXEC-RENDER-3). Called before a render is recorded
    /// and before a repaint, so a slot painted after a write is not marked by it.
    fn flush_row_writes(&self) {
        let written = std::mem::take(&mut *self.rows.written.lock());
        if !written.is_empty() {
            self.renders.write().mark_rows_changed(&written);
        }
    }

    /// `reference` resolved against the rows as they are now for the viewer of this thread's session: the ids that still match the
    /// reference's query and the viewer's `@read`, in the reference's order, a missing or hidden row omitted and
    /// `total` re-counted (Q6). The one resolver every renderer and the embedded API reach, so `@read` is applied
    /// here and nowhere else. A store that answers through the host (the server role) is reached in a dispatch.
    pub fn resolve_rows(&self, reference: &RowSetRef) -> Result<RowPage, String> {
        let _lane = self.lane.enter(None).map_err(|e| e.to_string())?;
        let viewer = self.session_viewer().map_err(|e| e.to_string())?;
        let mut ctx = BindingContext::new();
        ctx.set_user(viewer);
        self.rows.resolve_rows(reference, &ctx)
    }

    /// Set the current user.
    /// Declare that the host runs the listeners of every emit it marks `dispatched` (a worklist host:
    /// the JS runtime, `orbital-server`). Default `false`: a generated server delivers no listener, so
    /// the fold runs them all.
    pub fn set_host_fans_out(&self, fans_out: bool) {
        self.host_fans_out
            .store(fans_out, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_dispatch_observer(
        &self,
        observer: Option<Arc<crate::dispatch_trace::DispatchObserver>>,
    ) {
        *self.dispatch_observer.write() = observer;
    }

    /// Record every step's `@entity` after its effects next to the one it read at its start (propositions are judged
    /// on the pair). Off by default: it reads the row once more per step, and has no effect on what a step does.
    pub fn set_record_entity_after(&self, record: bool) {
        self.record_entity_after
            .store(record, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get runtime statistics.
    pub fn stats(&self) -> RuntimeStats {
        let manager_stats = self.state_manager.read().stats();
        RuntimeStats {
            registered_schemas: self.registry.schemas.read().len(),
            registered_traits: manager_stats.registered_traits,
            active_instances: manager_stats.active_instances,
            cached_entities: self.entity_source.count(),
        }
    }

    /// Access the event bus.
    pub fn event_bus(&self) -> &SharedEventBus {
        &self.event_bus
    }

    /// Declare what the host's query push-down honours for `entity_type`. A host that declares nothing is
    /// never sent a query: the kernel lists the table and runs the query itself.
    pub fn set_query_capabilities(&self, entity_type: &str, capabilities: QueryCapabilities) {
        self.io.set_query_capabilities(entity_type, capabilities);
    }

    /// Declare what the host's conditional writes honour for `entity_type`. A host that declares nothing is
    /// never sent `WriteRows`: the kernel reads the row, tests the policy and writes it itself.
    pub fn set_write_capabilities(&self, entity_type: &str, capabilities: WriteCapabilities) {
        self.io.set_write_capabilities(entity_type, capabilities);
    }

    /// Access the entity source.
    pub fn entity_source(&self) -> &Arc<dyn EntitySource> {
        &self.entity_source
    }

    /// Access the effect executor.
    pub fn effect_executor(&self) -> &Arc<EffectExecutor> {
        &self.effect_executor
    }

    /// Access the config.
    /// Block until no dispatch parked without the lane is waiting on the keyed call `key`: a completion for `key`
    /// runs only after the dispatch that asked it has ended (EXEC-CONC-1).
    pub fn wait_for_parked_key(&self, key: &str) {
        self.keyed_parks.wait_settled(key);
    }

    pub fn config(&self) -> &KernelConfig {
        &self.config
    }
}

/// Build the completion `emit` for a `fetch` effect: on success, fire `emit.success`
/// carrying the fetched rows as `{ "data": [...] }` (so a `@payload.data`-bound table
/// renders); on failure, fire `emit.failure`. This is what advances a data-loading
/// trait from its `loading` state to its `loaded` state. Returns `None` when the fetch
/// was delegated/no-op (the real fetch — and its emit — happens in the other environment).
/// The `render-ui` effects among `results`, as `(slot, unresolved content, let scope)` in run order.
fn render_sources(results: &[EffectResult], scopes: &[(usize, orbital_core_eval::evaluator::context::LocalScope)]) -> Vec<RenderSource> {
    results
        .iter()
        .enumerate()
        .filter_map(|(index, er)| match &er.effect {
            Effect::RenderUi { slot, content } => Some((slot.clone(), content.clone(), scope_at(scopes, index).cloned())),
            _ => None,
        })
        .collect()
}

/// The `let` scope the result at `index` ran under, when it ran inside one.
fn scope_at(scopes: &[(usize, orbital_core_eval::evaluator::context::LocalScope)], index: usize) -> Option<&orbital_core_eval::evaluator::context::LocalScope> {
    scopes.iter().find(|(at, _)| *at == index).map(|(_, scope)| scope)
}

/// A `render-ui` as it ran: its slot, its unresolved content and the `let` scope it ran under.
type RenderSource = (String, Value, Option<orbital_core_eval::evaluator::context::LocalScope>);

/// `ctx` with `scope`'s `let` bindings in scope, for resolving an effect that ran under them.
fn scoped<'c>(ctx: &'c BindingContext, scope: Option<&orbital_core_eval::evaluator::context::LocalScope>) -> std::borrow::Cow<'c, BindingContext> {
    match scope {
        Some(scope) => std::borrow::Cow::Owned(ctx.with_local_scope(scope)),
        None => std::borrow::Cow::Borrowed(ctx),
    }
}

/// The unresolved `render-ui` effects of the transition that enters `state_name`.
fn state_render_sources(trait_def: &TraitDefinition, state_name: &str) -> Vec<(String, Value)> {
    let Some(entering) = trait_def
        .state_machine
        .transitions
        .iter()
        .find(|t| t.to == state_name)
    else {
        return Vec::new();
    };
    entering
        .effects
        .iter()
        .flatten()
        .filter_map(
            |expr| match orbital_core_base::effect_types::parse_effect(expr) {
                Some(Effect::RenderUi { slot, content }) => Some((slot, content)),
                _ => None,
            },
        )
        .collect()
}

/// A data effect on a browser-stored entity: a browser-run tick sends it as its `runTick` leg.
fn dispatch_log_of(entry: &DeliveryEntry) -> Option<DispatchLog> {
    (!entry.view.prev_events.is_empty() || !entry.view.prev_states.is_empty()).then(|| {
        DispatchLog {
            prev_events: entry
                .view
                .prev_events
                .iter()
                .filter_map(|d| DeliveryRecord::from_value(d))
                .collect(),
            prev_states: entry.view.prev_states.to_vec(),
        }
    })
}

fn browser_data_effect(effect: &Effect, residence: &orbital_core_base::runs_on::Residence) -> bool {
    !orbital_core_base::runs_on::LegTouch::of_effect(effect, residence).server_work
}

/// Merge the Set-effect `set` fields into the `base` entity (read-modify-write).
/// Set fields override; untouched base fields are preserved. Used to PERSIST the
/// post-Set entity so a later re-render still sees every field (a partial replace-
/// write of only the Set-touched fields drops the rest and breaks
/// `@entity.<untouched-field>` on the next frame). Empty/Null `set` ⇒ `base`;
/// a non-object `set` with no base ⇒ `set` (first write).
/// The transition's `(set @entity.…)` writes so far, and `@entity` with them applied, kept in step
/// on every write so a later read in the same transition sees them without rebuilding the map
/// (JS twin: `EffectExecutor`'s bindings mirror).
struct PendingSets {
    sets: ValueMap,
    view: SharedEntity,
}

impl PendingSets {
    fn new(ctx: &BindingContext) -> Self {
        Self {
            sets: ValueMap::default(),
            view: ctx.entity.clone(),
        }
    }

    /// `ctx` with the earlier writes visible in `@entity`.
    fn apply<'a>(&self, ctx: &'a BindingContext) -> std::borrow::Cow<'a, BindingContext> {
        if self.sets.is_empty() {
            return std::borrow::Cow::Borrowed(ctx);
        }
        let mut c = ctx.clone();
        c.entity = self.view.clone();
        std::borrow::Cow::Owned(c)
    }

    fn insert(&mut self, field: Shared<String>, value: Value) {
        self.sets.insert(field.clone(), value.clone());
        if !self.view.set_field(field, value) {
            self.view = SharedEntity::from_value(Value::object(self.sets.clone()));
        }
    }
}

/// The declared field defaults of `def` as one object, `Null` when no field declares one.
fn declared_defaults(def: &EntityDefinition) -> Value {
    let defaults: ValueMap = def
        .fields
        .iter()
        .filter_map(|f| Some((f.name.clone()?.into(), sexpr_to_value(f.default.as_ref()?))))
        .collect();
    if defaults.is_empty() {
        Value::Null
    } else {
        Value::object(defaults)
    }
}

#[cfg(test)]
fn merge_entity(base: &Value, set: &Value) -> Value {
    SharedEntity::from_value_ref(base)
        .merged(&SharedEntity::from_value_ref(set))
        .to_value()
}

/// `@pages`: the root pages of a program's orbitals that declare a nav `@label`, deduped by path, in
/// orbital order, as `{ href, label, icon?, roles? }`. A page reference (resolved before it reaches a
/// stage) and a `:param` page are not entries. Twin of `appNavItems` in `@almadar/runtime`
/// `render-sigils.ts`; empty when there is none.
fn app_nav_items(schema: &OrbitalSchema) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for orbital in &schema.orbitals {
        for page in orbital
            .pages
            .iter()
            .flatten()
            .filter_map(orbital_core_base::schema::types::PageRef::as_inline)
        {
            let Some(label) = &page.label else { continue };
            if page.path.contains(':') || !seen.insert(page.path.clone()) {
                continue;
            }
            let mut item = ValueMap::from_iter([
                ("href".into(), Value::string(page.path.clone())),
                ("label".into(), Value::string(label.clone())),
            ]);
            if let Some(icon) = &page.icon {
                item.insert("icon".into(), Value::string(icon.clone()));
            }
            if let Some(roles) = &page.roles {
                item.insert(
                    "roles".into(),
                    Value::Array(roles.iter().cloned().map(Value::string).collect()),
                );
            }
            items.push(Value::object(item));
        }
    }
    items
}

/// The app-wide `@pages` after registering another program: earlier entries first, a later entry whose
/// path is already listed dropped. The empty list while no program declares one.
fn merge_nav_items(current: &Value, added: Vec<Value>) -> Value {
    let mut items = current.as_array().cloned().unwrap_or_default();
    let href = |item: &Value| {
        item.get_path("href")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut listed: std::collections::HashSet<String> = items.iter().filter_map(href).collect();
    items.extend(
        added
            .into_iter()
            .filter(|item| href(item).is_some_and(|h| listed.insert(h))),
    );
    Value::array(items)
}

use super::callsite_capture;
use super::entity_access;
use super::relations;
use super::resumable;
use super::row_query;

mod source_handler;
mod registration;
#[cfg(feature = "wire-drivers")]
mod tls_sockets;
#[cfg(feature = "wire-drivers")]
pub use tls_sockets::TlsCompletion;
pub mod lazy;
mod dispatch;
mod ticks;
mod client_leg;
mod frames;
mod cascade;
mod keyed_rows;
mod step_effects;
mod host_effects;
mod repaint;
mod viewer_scope;
mod session_life;
mod identity_session;
mod mount_ledger;
mod view_record;
pub use view_record::{FrameWrite, FramesApplied, FramesResync, PROGRAM_SESSION};
pub use client_leg::MountPlan;
pub use session_life::{RequestSession, SessionActive, SessionEnded, SessionPending, SessionStream};
mod program_queries;
pub mod page_circuit;
pub mod contract_query;
mod dispatch_scope;
mod values;
mod config_forward;
mod effect_bindings;
mod completion;
mod row_intake;
use source_handler::*;
use dispatch_scope::*;
use values::*;
use config_forward::*;
use effect_bindings::*;
use completion::*;
pub use dispatch_scope::{LocaleGuard, SessionGuard};
pub use values::already_delivered_from;
pub(crate) use values::sexpr_to_value;

#[cfg(test)]
mod navigate_options_tests;

#[cfg(test)]
mod call_key_tests;

#[cfg(test)]
mod merge_entity_tests;

#[cfg(test)]
mod config_forward_tests;

/// `EmbedIndex::build` in isolation — no kernel/dispatch involved, just the
/// `trait_embed_ids` (child id) -> ancestor-chain (names) inversion.
#[cfg(test)]
mod embed_index_tests;

/// §158 reproduction + regression: the kernel-side twin of the JS runtime's
/// `TraitScopeProvider` embed-emit bubble (R-EMBEDDED-EMIT-BUBBLE), verified
/// through real `RuntimeKernel::dispatch` calls (not just `EmbedIndex` in
/// isolation).
#[cfg(test)]
mod embed_bubble_dispatch_tests;

/// §B3 client role: `dispatch_with_server_leg` / `snapshot_trait` /
/// `restore_trait` / `apply_server_response` / `already_delivered_from`.
#[cfg(test)]
mod client_dispatch_tests;

#[cfg(test)]
mod theme_mode_tests;

#[cfg(test)]
mod mount_lifecycle_tests;

#[cfg(test)]
mod host_entity_tests;

#[cfg(test)]
mod fetch_payload_tests;

#[cfg(test)]
mod set_eval_failure_tests;

#[cfg(test)]
mod suspend_log_tests;

#[cfg(test)]
mod set_object_literal_tests;

#[cfg(test)]
mod circuit_step_tests;

#[cfg(test)]
mod os_grants_tests;

/// A stateless host forks the registered program for every request (EXEC-DIST-5), so a fork shares the program
/// rather than copying it: eight concurrent forks of project-friday each deep-copied it, ~90 MB of wasm heap a fork,
/// and the first such burst took minutes (G-UI-107).
#[cfg(test)]
mod fork_shares_program_tests;

#[cfg(test)]
mod program_context_metadata_tests;

#[cfg(test)]
mod endpoint_mount_tests;
