//! Runtime kernel — canonical runtime lifecycle for Orbital applications.

use crate::evaluator::ValueMap;
use super::{
    dispatch_memory::{DispatchMemory, DispatchView},
    dispatch_mode::DispatchMode,
    embed_index::EmbedIndex,
    entity_access::{
        apply_read_access, scope_fetched_rows, EntityAccessPolicies, EntityAccessTable,
    },
    entity_source::EntitySource,
    listener::{ListenerManager, TriggerCallback},
    process_result::{
        DeliveryEntry, EffectRun, EffectRunStatus, OrbitalDetail, OrbitalInfo, ProcessEventResult,
        RuntimeStats,
    },
    render_store::RenderStore,
    resumable::{CallRoutes, CallSite, CallVerdict, IoPort},
    server_leg::ServerLegCollector,
    theme::{resolve_theme, resolve_theme_default, ResolvedTheme, ThemeMode, ThemePresets},
    KernelConfig,
};
use crate::effects::{
    delivery_record_value, BusEventSource, DeliveryRecord, DispatchLog, PersistOperation,
};
use crate::error::{EvalError, OrbitalError};
use crate::util::DispatchVisits;
use crate::{
    effects::{
        Effect, EffectExecutor, EffectOutcome, EffectResult, EventRequest, EventResponse,
        FetchEmit, FetchHandler, FetchPage, PersistenceHandler, RuntimeEnvironment, TraitFrom,
    },
    evaluator::{BindingContext, BindingContextBuilder, Evaluator, Value},
    events::{shared_bus, SharedEventBus},
    schema::{
        EntityDefinition, EntityRef, OrbitalSchema, SExpression, TraitDefinition, TraitReference,
    },
    state_machine::{EventPayload, StateMachineManager, TraitState},
};
use parking_lot::RwLock;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, info};

/// Errors returned by the runtime kernel.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("Trait not found: {0}")]
    TraitNotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

mod dispatch_run;
pub(crate) use dispatch_run::{DispatchRun, Fault};

/// Circuit breaker for one dispatch's cross-trait worklist; twin of the JS `CROSS_TRAIT_CASCADE_CAP`.
pub const CROSS_TRAIT_CASCADE_CAP: usize = 2000;

/// One queued delivery: the trait it reaches, the event it triggers there, and what `@event` reads.
struct CascadeItem {
    trait_name: String,
    event: String,
    payload: Value,
    delivery: Value,
    /// The emit's own payload, which a listens `when` reads as `@payload`.
    source_payload: Value,
    /// Evaluated when the delivery is processed, so it sees the listener's log.
    listen_guard: Option<SExpression>,
    /// Whether processing this item offers its event to the listeners of that event. A listener's
    /// delivery runs its `triggers` event privately: only the seed and `emit`s reach listeners.
    fans_out: bool,
    /// Set for a re-render of an embedded child under its composer's payload, not a delivery.
    repaint: Option<RepaintJob>,
}

/// An embedded child to re-render under the payload of the transition that composed it
/// ([`callsite_capture`](super::callsite_capture)); `CascadeItem::trait_name` is the child.
#[derive(Clone)]
struct RepaintJob {
    /// The composing transition's payload.
    composed_with: Value,
    /// Walking through a child that was not repainted: its children keep the payload they were last composed with.
    keep_composed: bool,
    /// The children the firing transition composes; `None` = every capture child.
    composed_by: Option<Vec<String>>,
    /// The children this walk has repainted, shared by every job of one walk.
    visited: Arc<parking_lot::Mutex<HashSet<String>>>,
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
        !crate::events::is_bus_excluded_event(&item.event)
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
        if !crate::events::is_bus_excluded_event(event) || !self.awaiting.remove(trait_name) {
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
        _ => ValueMap::new(),
    };
    let mut list: Vec<&String> = awaiting.iter().collect();
    list.sort();
    map.insert(
        "_awaitingInit".to_string(),
        Value::Array(list.into_iter().map(|t| Value::String(t.clone())).collect()),
    );
    Value::Object(map)
}

fn listen_guard_passes(
    guard: &SExpression,
    source_payload: &Value,
    from: &str,
    view: &DispatchView,
) -> bool {
    let mut ctx = BindingContextBuilder::new()
        .payload(source_payload.clone())
        .state(Value::String(from.to_string()))
        .build();
    ctx.from_state = Some(from.to_string());
    view.apply(&mut ctx);
    matches!(Evaluator::new().evaluate_guard(guard, &ctx), Ok(true))
}

/// Shared runtime kernel.
///
/// Owns schema registration, trait resolution, event processing, effect execution,
/// and listener wiring. `orbital-client` and `orbital-server` wrap this kernel and
/// inject environment-specific entity sources and handlers.
pub struct RuntimeKernel {
    config: KernelConfig,
    state_manager: Arc<RwLock<StateMachineManager>>,
    effect_executor: Arc<EffectExecutor>,
    event_bus: SharedEventBus,
    listener_manager: ListenerManager,
    schemas: Arc<RwLock<HashMap<String, OrbitalSchema>>>,
    trait_definitions: Arc<RwLock<HashMap<String, Arc<TraitDefinition>>>>,
    entity_source: Arc<dyn EntitySource>,
    /// The door every read, write, service call and substrate operator goes through; `entity_source` is it.
    io: Arc<IoPort>,
    current_user: Arc<RwLock<Value>>,
    /// The host runs an emit's listeners whenever it marks the emit `dispatched`, so a fold runs only the client-only ones.
    host_fans_out: std::sync::atomic::AtomicBool,
    /// The in-flight dispatch's one `now` stamp; set by the outermost entry, reused by nested steps.
    dispatch_now: Arc<RwLock<Option<f64>>>,
    /// The program's declared locales (the first is the default) and merged message catalogs.
    i18n: Arc<RwLock<KernelI18n>>,
    /// The in-flight dispatch's viewer locale (`EventRequest.locale`), held by a [`LocaleGuard`].
    dispatch_locale: Arc<RwLock<Option<String>>>,
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
    /// Every registered inline `EntityDefinition`, keyed by entity name —
    /// populated additively alongside `entity_policies` in `register_schema`
    /// (same loop, same inline-only/additive-across-registrations contract).
    /// `dispatch_with_server_leg`/`TraitDefinition::dispatch_mode` reads a
    /// trait's linked entity here to decide `DispatchMode` (declared
    /// `[runtime]` persistence, never inferred).
    entity_definitions: Arc<RwLock<HashMap<String, EntityDefinition>>>,
    /// The trait whose effects are running and whether its step is a follow-on of the dispatch:
    /// what a `call-service` is stamped with.
    step_site: Arc<RwLock<Option<(String, bool)>>>,
    /// Referrer trait -> the embedded children to re-render under its payload (`callsite_capture`).
    capture_children: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Trait -> the payload its composer last composed it with: what its own `@callsitePayload` reads.
    composed_callsite: Arc<RwLock<ValueMap>>,
    /// Trait frames (`(set @entity.X)` writes), keyed `(frame key, entity id)`: a `[shared]`
    /// entity's frame is `$shared::<Entity>` for every trait bound to it, any other trait's is
    /// its own name (Runtime Spec Clause 2.3). `@entity` is the row with the frame over it.
    frames: Arc<RwLock<HashMap<(String, String), Value>>>,
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
    /// `@pages`: the app's root nav entries over every registered program (`Value::Null` while none
    /// declares a labeled root page, which leaves the token unresolved, as the JS stage does).
    nav_pages: Arc<RwLock<Value>>,
    /// The current mount's lifecycle (see [`MountLifecycle`]).
    mount: Arc<RwLock<MountLifecycle>>,
    /// Each mounted trait's last `render-ui`, kept to repaint it when the entity it reads changes.
    renders: Arc<RwLock<RenderStore>>,
}

impl std::fmt::Debug for RuntimeKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeKernel")
            .field("config", &self.config)
            .field("state_manager", &self.state_manager)
            .field("effect_executor", &self.effect_executor)
            .field("event_bus", &self.event_bus)
            .field("listener_manager", &self.listener_manager)
            .field("schemas", &self.schemas)
            .field("trait_definitions", &self.trait_definitions)
            .field("current_user", &self.current_user)
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

/// Bridges the executor's Fetch and Persist effects to the kernel's entity
/// source so list views resolve real data and mutations actually land.
struct EntitySourceHandler {
    source: Arc<dyn EntitySource>,
    io: Arc<IoPort>,
    entity_policies: Arc<RwLock<EntityAccessTable>>,
    entity_definitions: Arc<RwLock<HashMap<String, EntityDefinition>>>,
}

impl PersistenceHandler for EntitySourceHandler {
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        self.source.create(entity_type, data)
    }
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        self.source.read(entity_type, id)
    }
    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        self.source.find(entity_type, id)
    }
    fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        self.source.write(entity_type, id, data)
    }
    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        self.source.delete(entity_type, id)
    }
    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        self.source.list(entity_type)
    }
}

impl FetchHandler for EntitySourceHandler {
    #[allow(clippy::too_many_arguments)]
    fn fetch(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let mut entities = match id {
            Some(id) => match self.source.read(entity_type, id) {
                Ok(v) if !v.is_null() => vec![v],
                _ => Vec::new(),
            },
            None => self.source.list(entity_type)?,
        };
        tracing::debug!(
            target: crate::logging::ns::RUNTIME_FETCH,
            entity = %entity_type,
            id = ?id,
            has_filter = filter.is_some(),
            limit = ?limit,
            returned = entities.len(),
            "fetch",
        );
        // The env-agnostic entity source lists unfiltered, so the declared
        // predicate is applied here, through the shared owner every FetchHandler
        // uses (C-FETCH-FILTER-NOT-APPLIED-BY-ENTITY-SOURCE-HANDLER). The
        // entity's declared `@read` policy (if any) is ANDed in alongside the
        // call-site filter — the un-bypassable half of row visibility.
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        let total = scope_fetched_rows(
            &mut entities,
            policy.as_ref(),
            filter,
            limit,
            offset,
            ctx,
            entity_type,
        );
        if let Some(include) = include.filter(|i| !i.is_empty()) {
            super::relations::populate_relations(
                &*self.source,
                &self.entity_definitions.read(),
                &mut entities,
                entity_type,
                include,
                0,
                &mut HashSet::new(),
            );
            if self.io.suspended() {
                return Err(super::resumable::AWAITING_HOST.to_string());
            }
        }
        Ok(FetchPage {
            rows: entities,
            total,
        })
    }
}

/// Clears the dispatch stamp when the outermost entry that set it returns (or unwinds).
struct DispatchStamp {
    slot: Option<Arc<RwLock<Option<f64>>>>,
    now: f64,
}

#[derive(Debug, Default)]
struct KernelI18n {
    locales: Vec<String>,
    catalogs: HashMap<String, HashMap<String, String>>,
}

/// Holds a dispatch's viewer locale; dropping it clears the slot. JS twin: `OrbitalEventRequest.locale`.
pub struct LocaleGuard {
    slot: Arc<RwLock<Option<String>>>,
}

impl Drop for LocaleGuard {
    fn drop(&mut self) {
        *self.slot.write() = None;
    }
}

impl Drop for DispatchStamp {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            *slot.write() = None;
        }
    }
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
        let io = Arc::new(IoPort::new(
            entity_source,
            config.service_handler.clone(),
            config.substrate_handler.clone(),
        ));
        let entity_source: Arc<dyn EntitySource> = io.clone();
        let entity_definitions: Arc<RwLock<HashMap<String, EntityDefinition>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let entity_handler = Arc::new(EntitySourceHandler {
            source: entity_source.clone(),
            io: io.clone(),
            entity_policies: entity_policies.clone(),
            entity_definitions: entity_definitions.clone(),
        });
        let mut executor = EffectExecutor::new(environment)
            .with_fetch_handler(entity_handler.clone())
            .with_persistence(entity_handler)
            .with_entity_policies(entity_policies.clone());
        // Threaded BEFORE the `Arc` wrap (`EffectExecutor::with_delegate` is a
        // consuming builder) — every client-role kernel is configured with the
        // ONE `ServerLegCollector` via `KernelConfig.delegate`.
        if let Some(delegate) = config.delegate.clone() {
            executor = executor.with_delegate(delegate);
        }
        if io.has_service() {
            executor = executor.with_service_handler(io.clone());
        }
        if io.has_substrate() {
            executor = executor.with_substrate_handler(io.clone());
        }
        let effect_executor = Arc::new(executor);

        let listener_manager = ListenerManager::new(event_bus.clone());

        // No HTTP request stands behind a headless run, so the viewer comes from
        // `ALMADAR_PERSONA`. Without it `@user.*` is Null and every role gate and
        // ownership filter silently takes its empty branch (C-KERNEL-USER-NEVER-SET).
        // A JSON-form spec is self-contained and resolves here; a bare id/role
        // names a row of the app's OWN [identity] roster, which exists only once
        // a schema registers — `register_schema` finishes that resolution.
        let current_user = match crate::runtime::persona::persona_from_env_spec() {
            Ok(crate::runtime::persona::EnvPersona::Resolved(user)) => user,
            Ok(_) => Value::Null,
            Err(err) => {
                // A developer who asked to be somebody must never be shown the app
                // as nobody — report and refuse the bad spec rather than degrade.
                tracing::error!(error = %err, "invalid {}", crate::runtime::persona::PERSONA_ENV);
                panic!("{}", err);
            }
        };

        Self {
            config,
            state_manager,
            effect_executor,
            event_bus,
            listener_manager,
            schemas: Arc::new(RwLock::new(HashMap::new())),
            trait_definitions: Arc::new(RwLock::new(HashMap::new())),
            entity_source,
            io,
            current_user: Arc::new(RwLock::new(current_user)),
            host_fans_out: std::sync::atomic::AtomicBool::new(false),
            dispatch_now: Arc::new(RwLock::new(None)),
            i18n: Arc::new(RwLock::new(KernelI18n::default())),
            dispatch_locale: Arc::new(RwLock::new(None)),
            entity_policies,
            embed_index: Arc::new(RwLock::new(EmbedIndex::default())),
            entity_definitions,
            step_site: Arc::new(RwLock::new(None)),
            capture_children: Arc::new(RwLock::new(HashMap::new())),
            composed_callsite: Arc::new(RwLock::new(ValueMap::new())),
            frames: Arc::new(RwLock::new(HashMap::new())),
            theme_presets: Arc::new(RwLock::new(ThemePresets::new())),
            theme_mode: Arc::new(RwLock::new(None)),
            nav_pages: Arc::new(RwLock::new(Value::Null)),
            mount: Arc::new(RwLock::new(MountLifecycle::default())),
            renders: Arc::new(RwLock::new(RenderStore::default())),
        }
    }

    /// Traits entering the mount: routed deliveries to them wait for their own lifecycle event.
    pub fn mount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        for t in traits {
            mount.awaiting.insert(t.into());
        }
    }

    /// A new mount replacing the previous one (a page switch): nothing held for
    /// the old mount survives, and `traits` await their lifecycle event.
    pub fn remount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        mount.awaiting.clear();
        mount.held.clear();
        self.renders.write().clear();
        for t in traits {
            mount.awaiting.insert(t.into());
        }
    }

    /// Traits leaving the mount: nothing stays held for them.
    pub fn unmount_traits<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mount = self.mount.write();
        let mut renders = self.renders.write();
        for t in traits {
            let t = t.into();
            mount.awaiting.remove(&t);
            mount.held.remove(&t);
            renders.forget_traits([t.as_str()]);
        }
    }

    /// Create a kernel with a trigger callback for listener events.
    pub fn with_trigger_callback(mut self, callback: TriggerCallback) -> Self {
        self.listener_manager =
            ListenerManager::new(self.event_bus.clone()).with_trigger_callback(callback);
        self
    }

    /// Register a schema with the kernel.
    pub fn register_schema(&self, mut schema: OrbitalSchema) -> Result<(), KernelError> {
        schema.bind_unlinked_traits_to_host_entity();
        let schema_name = schema.name.clone();
        info!(schema = %schema_name, "Registering schema");
        self.theme_presets
            .write()
            .merge(ThemePresets::from_definitions(
                schema.theme_definitions.iter().flatten(),
            ));

        // A bare `ALMADAR_PERSONA` (id/role) resolves against this app's declared
        // roster — its [identity] entity's seeded rows — which construction could
        // not see. Only fills a still-unset viewer: a later registration must not
        // clobber a viewer somebody already resolved or set.
        if let Ok(crate::runtime::persona::EnvPersona::Bare(spec)) =
            crate::runtime::persona::persona_from_env_spec()
        {
            if matches!(self.get_current_user(), Value::Null) {
                let roster = crate::runtime::persona::identity_roster(
                    &schema,
                    crate::runtime::persona::DEFAULT_IDENTITY_ROSTER_COUNT,
                );
                let user = crate::runtime::persona::resolve_persona_spec(&spec, &roster)
                    .map_err(KernelError::Internal)?;
                self.set_current_user(user);
            }
        }

        // Populate the entity access table from every declared entity's four
        // directives (additive across registrations — a later schema never
        // clobbers an earlier one's policies for an entity it doesn't itself
        // redeclare).
        {
            let mut table = self.entity_policies.write();

            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        table.insert(
                            def.name.clone(),
                            EntityAccessPolicies {
                                read: def.read_policy.as_ref().map(sexpr_to_value),
                                create: def.create_policy.as_ref().map(sexpr_to_value),
                                update: def.update_policy.as_ref().map(sexpr_to_value),
                                delete: def.delete_policy.as_ref().map(sexpr_to_value),
                                collection: def.collection.clone(),
                                inherited: false,
                                required_create_fields:
                                    crate::runtime::entity_access::required_create_field_names(def),
                                intrinsic_fields:
                                    crate::runtime::entity_access::intrinsic_field_names(def),
                            },
                        );
                    }
                }
            }

            // Access is a property of the DATA, so a collection's declared
            // policy governs every entity bound to it. Re-run over the whole
            // accumulated table, not just this schema: an organism's entity and
            // the std-atom entity that shadows it on the same collection arrive
            // in different registrations
            // (`R-SHADOW-ENTITY-SHARES-COLLECTION-DIRECTIVES-DONT-APPLY`).
            crate::runtime::entity_access::resolve_collection_inheritance(&mut table);
        }

        // A `persistent:` collection is one table: every entity bound to it reads
        // and writes the same rows (JS `MockPersistenceAdapter`, the TS shell).
        for orbital in &schema.orbitals {
            for entity_ref in
                std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten())
            {
                if let EntityRef::Inline(def) = entity_ref {
                    if let Some(collection) = &def.collection {
                        self.entity_source.bind_collection(&def.name, collection);
                    }
                }
            }
        }

        // Additive across registrations, same inline-only contract as the access
        // table above. A SEPARATE lock/loop (never nested inside the
        // `entity_policies` write guard above) so `entity_policies` and
        // `entity_definitions` stay two independent leaf locks, not a new
        // nested pair to add to the documented order.
        {
            let mut defs = self.entity_definitions.write();
            for orbital in &schema.orbitals {
                for entity_ref in std::iter::once(&orbital.entity)
                    .chain(orbital.auxiliary_entities.iter().flatten())
                {
                    if let EntityRef::Inline(def) = entity_ref {
                        defs.insert(def.name.clone(), def.clone());
                    }
                }
            }
        }

        for orbital in &schema.orbitals {
            let mut registered = Vec::new();
            for trait_ref in &orbital.traits {
                if let Some(trait_def) = self.resolve_trait_definition(trait_ref, &schema) {
                    self.register_trait(&trait_def)?;
                    self.register_trait_listeners(&trait_def, &orbital.name)?;
                    registered.push(trait_def);
                }
            }
            self.capture_children
                .write()
                .extend(super::callsite_capture::capture_children(
                    &registered.iter().collect::<Vec<_>>(),
                ));

            // Seed entity instances from inline entity definitions.
            if let EntityRef::Inline(entity_def) = &orbital.entity {
                if let Some(instances) = &entity_def.instances {
                    let now = crate::runtime::mock_random::iso_from_ms(
                        crate::time_util::now_millis_f64(),
                    );
                    for entity in
                        crate::runtime::seed::instance_rows(&entity_def.name, instances, &now)
                    {
                        let _ = self.entity_source.create(&entity_def.name, &entity);
                    }
                }
            }
        }

        self.schemas
            .write()
            .insert(schema_name.clone(), schema.clone());
        {
            let mut nav = self.nav_pages.write();
            *nav = merge_nav_items(&nav, app_nav_items(&schema));
        }

        // Additive rebuild over the FULL accumulated trait set, not just this
        // schema's — an embed spanning two registrations must still resolve
        // (mirrors the entity-policy table's additive re-run above).
        *self.embed_index.write() = EmbedIndex::build(&self.trait_definitions.read());

        info!(schema = %schema_name, "Schema registered successfully");
        Ok(())
    }

    /// Resolve a trait reference to its definition.
    fn resolve_trait_definition(
        &self,
        trait_ref: &TraitReference,
        schema: &OrbitalSchema,
    ) -> Option<TraitDefinition> {
        match trait_ref {
            TraitReference::Inline(def) => Some(def.clone()),
            TraitReference::Reference {
                reference, name, ..
            } => {
                let lookup_name = reference.as_ref().or(name.as_ref())?;
                for orbital in &schema.orbitals {
                    for other_trait_ref in &orbital.traits {
                        if let TraitReference::Inline(trait_def) = other_trait_ref {
                            if &trait_def.name == lookup_name {
                                return Some(trait_def.clone());
                            }
                        }
                    }
                }
                None
            }
        }
    }

    /// Register a trait definition with the state manager.
    fn register_trait(&self, trait_def: &TraitDefinition) -> Result<(), KernelError> {
        let trait_name = &trait_def.name;
        debug!(trait_name = %trait_name, "Registering trait");

        self.state_manager.write().register_trait(trait_def.clone());
        self.trait_definitions
            .write()
            .insert(trait_name.clone(), Arc::new(trait_def.clone()));

        Ok(())
    }

    /// Register listeners declared on a trait.
    fn register_trait_listeners(
        &self,
        trait_def: &TraitDefinition,
        orbital: &str,
    ) -> Result<(), KernelError> {
        if let Some(listens) = &trait_def.listens {
            for listen in listens {
                self.listener_manager
                    .register(&trait_def.name, orbital, listen)
                    .map_err(KernelError::Internal)?;
            }
        }
        Ok(())
    }

    /// Process an event for a specific entity.
    pub fn process_event(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<ProcessEventResult, KernelError> {
        self.process_event_in_dispatch(trait_name, entity_id, event_name, payload, None, false)
            .map_err(Fault::into_kernel)
    }

    /// `process_event` inside a dispatch: `view` supplies `@event`/`@prevEvents`/`@prevStates`.
    /// Guards read the source state; effects read the target (Runtime Spec Clause 3.3).
    fn process_event_in_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        view: Option<&DispatchView>,
        follow_on: bool,
    ) -> Result<ProcessEventResult, Fault> {
        self.process_event_composed(
            trait_name, entity_id, event_name, payload, view, follow_on, None,
        )
    }

    /// [`Self::process_event_in_dispatch`] with an explicit `@callsitePayload` (a repaint of an embedded
    /// child under its composer's payload); `None` reads the payload the trait was last composed with.
    #[allow(clippy::too_many_arguments)]
    fn process_event_composed(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        view: Option<&DispatchView>,
        follow_on: bool,
        callsite: Option<&Value>,
    ) -> Result<ProcessEventResult, Fault> {
        // An id-less event (`entity_id: None`) still needs somewhere to keep
        // `@entity` writes across transitions, so the kernel keys a per-trait
        // frame under the literal id "singleton" — one frame per (trait,
        // "singleton") pair, not per row. The compiled TS server reproduces
        // the same view via `@almadar/core`'s `resolveEntityView` (`frame <
        // row < id` precedence), with `OirEntity::frame_fields()` (the
        // `@intrinsic` fields) as the one allow-list for what belongs in the
        // frame tier on both paths.
        let entity_id = entity_id.unwrap_or("singleton");
        let stamp = self.begin_dispatch();

        debug!(
            trait_name = %trait_name,
            entity_id = %entity_id,
            event = %event_name,
            "Processing event"
        );

        let trait_def = self
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;

        let mut state_manager = self.state_manager.write();

        let current_state = state_manager
            .get_or_create_state(trait_name, Some(entity_id))
            .map_err(|e| KernelError::Internal(e.to_string()))?
            .clone();

        // `@payload` is this event's own payload; an absent one reads `{}` (drift D-2).
        let effective_payload = if payload.is_null() {
            Value::Object(ValueMap::new())
        } else {
            payload.clone()
        };

        let ctx = BindingContextBuilder::new()
            .payload(effective_payload.clone())
            .state(Value::String(current_state.current_state.clone()))
            .user(self.current_user.read().clone())
            .now(stamp.now)
            .current_theme(Value::String(
                self.resolve_current_theme_for_trait(trait_name).key,
            ))
            .pages(self.nav_pages.read().clone());
        let mut ctx = self.with_viewer_i18n(ctx).build();
        ctx.set_config(bind_user_forwards(
            config_defaults_value(&trait_def, &self.trait_definitions.read()),
            &self.current_user.read(),
        ));
        if let Some(view) = view {
            view.apply(&mut ctx);
        }
        let capture = callsite
            .cloned()
            .or_else(|| self.composed_callsite.read().get(trait_name).cloned());
        if let Some(capture) = &capture {
            ctx.set_callsite_payload(capture.clone());
        }

        let entity_type = trait_def.linked_entity.as_deref().unwrap_or(trait_name);
        let entity_data = self.read_trait_entity(trait_name, entity_type, entity_id);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        if !entity_data.is_null() {
            ctx.set_entity(entity_data.clone());
        }
        debug!(
            target: "almadar::kernel",
            entity_type = %entity_type,
            entity_id = %entity_id,
            resolved = !entity_data.is_null(),
            entity_keys = ?entity_data.as_object().map(|o| o.keys().collect::<Vec<_>>()),
            "kernel: entity read for @entity bindings"
        );

        let event_payload = EventPayload {
            event: event_name.to_string(),
            data: payload.clone(),
            source: Some(entity_id.to_string()),
            timestamp: crate::time_util::now_millis(),
        };

        // Guard/transition evaluation runs against `ctx` — the SAME context
        // already built above with payload/state/entity/user/config/currentTheme
        // — instead of a second, state+entity-only context the manager would
        // otherwise build from scratch (§157 `identity`: this is what makes
        // `(when ["=", "@user.role", ...])` / `@config.*` guards see real values
        // on the Rust path).
        let result = state_manager
            .process_event_with_ctx(trait_name, Some(entity_id), &event_payload, &ctx)
            .map_err(|e| KernelError::Internal(e.to_string()))?;

        if result.executed {
            let new_state = result
                .new_state
                .clone()
                .unwrap_or(current_state.current_state.clone());
            ctx.set_state(Value::String(new_state.clone()));
            ctx.from_state = Some(current_state.current_state.clone());
            ctx.to_state = Some(new_state.clone());
            *self.step_site.write() = Some((trait_name.to_string(), follow_on));
            let (effect_results, captured_entity_data, effect_runs, failure) =
                match self.server_leg_collector() {
                    Some(collector) => collector.attribute(trait_name, || {
                        self.execute_effects_traced(&result.effects, &ctx)
                    }),
                    None => self.execute_effects_traced(&result.effects, &ctx),
                };
            *self.step_site.write() = None;

            // Effective entity = read entity with Set-effect fields merged in. Persist
            // the MERGED entity (read-modify-write), never the partial
            // `captured_entity_data` (only the Set-touched fields): a replace-write of
            // the partial would drop every untouched field, so a later re-render's
            // `@entity.<untouched-field>` (tabs `activeTab: @entity.filterKind`, board
            // `current*` card fields, …) reverts to a literal token. The merged value is
            // also what render-ui resolves against below and what the client receives.
            let effective_entity = merge_entity(&entity_data, &captured_entity_data);

            // Resolve EVERY binding (@payload.X / @entity.X / @config.X / @state / @user)
            // in the client-facing effects against the full context, through the ONE
            // canonical resolver — `@payload` is a typed payload so `X` is arbitrary; no
            // per-field special-cases. The entity is the post-Set effective entity.
            ctx.set_entity(effective_entity.clone());
            let sources = render_sources(&effect_results);
            let mut effect_results = effect_results
                .into_iter()
                .map(|er| resolve_effect_result(er, &ctx))
                .collect::<Vec<_>>();
            let mut failure = failure;
            if failure.is_none() {
                failure = effect_results
                    .iter()
                    .find(|r| is_thrown_failure(r))
                    .map(|thrown| thrown.error.clone().unwrap_or_default());
                if failure.is_some() {
                    effect_results.retain(is_thrown_failure);
                }
            }
            self.inject_relation_options(trait_name, &mut effect_results);
            if self.io.suspended() {
                if let Some(state) = state_manager.get_state_mut(trait_name, Some(entity_id)) {
                    *state = current_state;
                }
                return Err(Fault::Suspended);
            }

            if !captured_entity_data.is_null()
                && captured_entity_data
                    .as_object()
                    .map(|o| !o.is_empty())
                    .unwrap_or(false)
            {
                self.write_trait_frame(trait_name, entity_type, entity_id, &captured_entity_data);
            }

            info!(
                trait_name = %trait_name,
                entity_id = %entity_id,
                event = %event_name,
                new_state = %new_state,
                effects_count = effect_results.len(),
                "Event processed successfully"
            );

            self.record_renders(trait_name, entity_id, sources, &effect_results, &ctx);

            Ok(ProcessEventResult {
                executed: true,
                previous_state: current_state.current_state.clone(),
                new_state,
                effects_executed: effect_results.len(),
                effect_results,
                entity_data: effective_entity,
                linked_entity: entity_type.to_string(),
                event: Some(event_name.to_string()),
                payload: payload.clone(),
                entry: Some(DeliveryEntry {
                    entity_before: entity_data,
                    view: view.cloned().unwrap_or_default(),
                }),
                fired_arm: result.fired_arm,
                guards: result.guards.clone(),
                effect_runs,
                cascade_dropped: Vec::new(),
                consumed_emits: Vec::new(),
                failure,
                callsite_payload: capture,
            })
        } else {
            debug!(
                trait_name = %trait_name,
                entity_id = %entity_id,
                event = %event_name,
                "Event not processed (guard blocked or no matching transition)"
            );

            let entity_data = if ctx.entity.is_null() {
                Value::Object(ValueMap::new())
            } else {
                (*ctx.entity).clone()
            };

            let state_effects = self.get_state_render_effects(
                &trait_def,
                &current_state.current_state,
                &ctx,
                &entity_data,
            );
            self.record_renders(
                trait_name,
                entity_id,
                state_render_sources(&trait_def, &current_state.current_state),
                &state_effects,
                &ctx,
            );

            Ok(ProcessEventResult {
                event: None,
                executed: false,
                previous_state: current_state.current_state.clone(),
                new_state: current_state.current_state.clone(),
                effects_executed: 0,
                effect_results: state_effects,
                entity_data,
                linked_entity: entity_type.to_string(),
                payload: payload.clone(),
                entry: None,
                fired_arm: None,
                guards: result.guards.clone(),
                effect_runs: Vec::new(),
                cascade_dropped: Vec::new(),
                consumed_emits: Vec::new(),
                failure: None,
                callsite_payload: None,
            })
        }
    }

    /// The payload `payloadSchema` `trait_name` declares for `event`, when it declares one.
    pub fn event_payload_schema(
        &self,
        trait_name: &str,
        event: &str,
    ) -> Option<Vec<crate::schema::PayloadField>> {
        self.trait_definitions
            .read()
            .get(trait_name)?
            .state_machine
            .events
            .as_ref()?
            .iter()
            .find(|e| e.key == event)?
            .payload_schema
            .clone()
    }

    /// The declared tick `tick_name` of `trait_name`.
    pub fn tick_of(
        &self,
        trait_name: &str,
        tick_name: &str,
    ) -> Option<crate::schema::TickDefinition> {
        self.trait_definitions
            .read()
            .get(trait_name)?
            .ticks
            .as_ref()?
            .iter()
            .find(|tick| tick.name == tick_name)
            .cloned()
    }

    /// Run one scheduled tick of `trait_name` against `entity_id` through the **same**
    /// effect path as a transition (`execute_effects_with_entity`) — no parallel walker.
    /// Mirrors the canonical JS runtime's `executeTick`: build the entity/state context,
    /// evaluate the tick's guard (skip when it fails), run the tick's effects, and persist
    /// any `set` fields back to the entity. A tick is not an event/transition, so this is a
    /// sibling entry point to `process_event` that shares its effect execution + binding
    /// resolution, returning the same [`ProcessEventResult`] shape so the existing
    /// `EventResponse` conversion folds tick effects into the client response unchanged.
    pub fn process_tick(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
    ) -> Result<ProcessEventResult, KernelError> {
        let entity_id = entity_id.unwrap_or("singleton");
        self.process_tick_in_dispatch(trait_name, entity_id, entity_id, tick_name)
            .map_err(Fault::into_kernel)
    }

    /// [`Self::process_tick`] inside a dispatch: the tick reads the row `entity_id` while the trait's
    /// state is held under `state_entity` (a server tick runs over every row against the one trait state).
    fn process_tick_in_dispatch(
        &self,
        trait_name: &str,
        entity_id: &str,
        state_entity: &str,
        tick_name: &str,
    ) -> Result<ProcessEventResult, Fault> {
        let stamp = self.begin_dispatch();

        let trait_def = self
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;

        let tick = trait_def
            .ticks
            .as_ref()
            .and_then(|ticks| ticks.iter().find(|t| t.name == tick_name))
            .cloned()
            .ok_or_else(|| {
                KernelError::Internal(format!("tick '{tick_name}' not found on '{trait_name}'"))
            })?;

        let current_state = {
            let mut state_manager = self.state_manager.write();
            state_manager
                .get_or_create_state(trait_name, Some(state_entity))
                .map_err(|e| KernelError::Internal(e.to_string()))?
                .current_state
                .clone()
        };

        // `appliesTo` lists the trait states the tick runs in; none listed = every state.
        if tick
            .applies_to
            .as_ref()
            .is_some_and(|states| !states.is_empty() && !states.contains(&current_state))
        {
            return Ok(ProcessEventResult {
                event: None,
                executed: false,
                previous_state: current_state.clone(),
                new_state: current_state,
                effects_executed: 0,
                effect_results: Vec::new(),
                entity_data: Value::Null,
                linked_entity: trait_def
                    .linked_entity
                    .clone()
                    .unwrap_or_else(|| trait_name.to_string()),
                payload: Value::Null,
                entry: None,
                fired_arm: None,
                guards: Vec::new(),
                effect_runs: Vec::new(),
                cascade_dropped: Vec::new(),
                consumed_emits: Vec::new(),
                failure: None,
                callsite_payload: None,
            });
        }

        // A tick has no payload (drift D-2).
        let tick_payload = Value::Object(ValueMap::new());

        let ctx = BindingContextBuilder::new()
            .payload(tick_payload)
            .state(Value::String(current_state.clone()))
            .user(self.current_user.read().clone())
            .now(stamp.now)
            .current_theme(Value::String(
                self.resolve_current_theme_for_trait(trait_name).key,
            ))
            .pages(self.nav_pages.read().clone());
        let mut ctx = self.with_viewer_i18n(ctx).build();
        ctx.set_config(bind_user_forwards(
            config_defaults_value(&trait_def, &self.trait_definitions.read()),
            &self.current_user.read(),
        ));

        let entity_type = trait_def.linked_entity.as_deref().unwrap_or(trait_name);
        let entity_data = self.read_trait_entity(trait_name, entity_type, entity_id);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        if !entity_data.is_null() {
            ctx.set_entity(entity_data.clone());
        }

        // Guard gates the tick exactly like the JS runtime — evaluated through the one
        // canonical evaluator, never a re-implemented predicate.
        if let Some(guard) = &tick.guard {
            let evaluator = Evaluator::new();
            if !matches!(evaluator.evaluate_guard(guard, &ctx), Ok(true)) {
                return Ok(ProcessEventResult {
                    event: None,
                    executed: false,
                    previous_state: current_state.clone(),
                    new_state: current_state,
                    effects_executed: 0,
                    effect_results: Vec::new(),
                    entity_data,
                    linked_entity: entity_type.to_string(),
                    payload: Value::Null,
                    entry: None,
                    fired_arm: None,
                    guards: Vec::new(),
                    effect_runs: Vec::new(),
                    cascade_dropped: Vec::new(),
                    consumed_emits: Vec::new(),
                    failure: None,
                    callsite_payload: None,
                });
            }
        }

        *self.step_site.write() = Some((trait_name.to_string(), false));
        let (effect_results, captured_entity_data) =
            self.execute_effects_with_entity(&tick.effects, &ctx);
        *self.step_site.write() = None;

        // Effective entity = base entity with the tick's `set` fields merged in.
        // Persist the MERGED entity (read-modify-write), not the partial — same
        // reasoning as process_event: a replace-write of only the tick's Set fields
        // would drop every untouched field on the next render.
        let effective_entity = merge_entity(&entity_data, &captured_entity_data);

        ctx.set_entity(effective_entity.clone());
        let sources = render_sources(&effect_results);
        let mut effect_results = effect_results
            .into_iter()
            .map(|er| resolve_effect_result(er, &ctx))
            .collect::<Vec<_>>();
        self.inject_relation_options(trait_name, &mut effect_results);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }

        if captured_entity_data
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false)
        {
            self.write_trait_frame(trait_name, entity_type, entity_id, &captured_entity_data);
        }

        self.record_renders(trait_name, entity_id, sources, &effect_results, &ctx);

        Ok(ProcessEventResult {
            event: None,
            executed: true,
            previous_state: current_state.clone(),
            new_state: current_state,
            effects_executed: effect_results.len(),
            effect_results,
            entity_data: effective_entity,
            linked_entity: entity_type.to_string(),
            payload: Value::Null,
            entry: None,
            fired_arm: None,
            guards: Vec::new(),
            effect_runs: Vec::new(),
            cascade_dropped: Vec::new(),
            consumed_emits: Vec::new(),
            failure: None,
            callsite_payload: None,
        })
    }

    /// Dispatch through the SEED trait's declared [`DispatchMode`], collecting
    /// whatever server-only effects (`persist`/`fetch`/`call-service`) the
    /// `EffectExecutor` delegated instead of executing — the `RuntimeEnvironment::
    /// Client` path (`executor.rs` `do_persist`/`do_fetch`/`do_call_service`)
    /// routes those to `self.effect_executor`'s configured [`EffectDelegate`]
    /// (installed from `KernelConfig.delegate` at construction — see
    /// [`RuntimeKernel::new`]), which for every client role is the ONE
    /// [`ServerLegCollector`]. `dispatch_with_server_leg` drains it right after
    /// this call's `dispatch`, so effects from an unrelated concurrent dispatch
    /// on another thread must never land here — see the lock-order note on
    /// [`Self::snapshot_trait`]; this method takes no kernel lock itself besides
    /// what `dispatch`/`process_event` already take internally, so two threads
    /// calling `dispatch_with_server_leg` concurrently race on the SAME
    /// collector and may attribute each other's delegated effects to the wrong
    /// leg — acceptable for now (client-role kernels are driven from one
    /// thread; `kernel_concurrent_dispatch_and_apply_no_deadlock` only proves
    /// the LOCKS never deadlock under concurrency, not per-call attribution).
    ///
    /// `orbital` is carried through for tracing/attribution only (no field on
    /// the wire [`EventRequest`] needs it yet — multiple orbitals sharing one
    /// client kernel is future work).
    pub fn dispatch_with_server_leg(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: Value,
        client_id: Option<&str>,
    ) -> Result<ClientDispatch, KernelError> {
        let eid = entity_id.unwrap_or("singleton").to_string();

        let trait_def = self
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;
        let mode = self.dispatch_mode_of(&trait_def);

        // RuntimeOptimistic commits locally before any server round trip, so the
        // snapshot must be taken BEFORE `dispatch` runs — it's what the caller
        // rolls back to on a disagreeing/erroring server response.
        let snapshot = if matches!(mode, DispatchMode::RuntimeOptimistic) {
            Some(self.snapshot_trait(trait_name, Some(&eid)))
        } else {
            None
        };
        // The leg replays the event from the pre-dispatch state, so it carries
        // the pre-dispatch row too.
        let seed_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

        debug!(
            target: "almadar::kernel",
            orbital = %orbital, trait_name = %trait_name, event = %event, mode = ?mode,
            "dispatch_with_server_leg",
        );

        let outcomes = self.dispatch(trait_name, Some(&eid), event, payload.clone())?;
        let mut response = EventResponse::from_process_results(&outcomes, trait_name, event);
        response.fold_rerenders(&self.rerender_stale());

        Ok(self.finish_client_dispatch(LegSeed {
            trait_name,
            event,
            payload,
            entity_id,
            client_id,
            tick: None,
            mode,
            snapshot,
            seed_row: &seed_row,
            outcomes: &outcomes,
            response,
        }))
    }

    /// Drain the delegated server effects the cascade collected and, for a client-role kernel,
    /// wrap them in the leg that replays `seed.event` on the server. The one body behind
    /// [`Self::dispatch_with_server_leg`] and [`Self::dispatch_tick`].
    fn finish_client_dispatch(&self, seed: LegSeed<'_>) -> ClientDispatch {
        let LegSeed {
            trait_name,
            event,
            payload,
            entity_id: raw_entity_id,
            client_id,
            tick,
            mode,
            snapshot,
            seed_row,
            outcomes,
            response,
        } = seed;
        let (drained, delegating_traits) = self.drain_server_leg_collector();
        let hybrid = matches!(mode, DispatchMode::HybridClientOnly);
        // A local seed never needs the server for its OWN effects, but the
        // traits its cascade reached (a persistor, a refetching list) may.
        let downstream_needs_server = delegating_traits.iter().any(|t| t != trait_name);

        // The client's mount rides every leg so the host holds a seed
        // for a trait this client has not initialized yet.
        let awaiting = self.awaiting_init_value();
        let with_awaiting = |payload: Value| match payload {
            Value::Object(mut map) => {
                map.insert("_awaitingInit".to_string(), awaiting.clone());
                Some(Value::Object(map))
            }
            Value::Null => Some(Value::Object(ValueMap::from([(
                "_awaitingInit".to_string(),
                awaiting.clone(),
            )]))),
            other => Some(other),
        };
        let server_legs = if self.effect_executor.environment().is_client() && !drained.is_empty() {
            if hybrid && !downstream_needs_server && !self.all_browser_data(&drained) {
                // A `local` trait's OWN effects should be client-only by construction;
                // a server-only effect the seed itself delegated is a validator bug
                // (§7 declares `local` legal only where nothing needs the server), not
                // something to silently ship as a leg. Report and drop. Data in the
                // browser store is the exception: the host runs that leg against the store.
                tracing::error!(
                    trait_name = %trait_name, event = %event, drained = drained.len(),
                    "HybridClientOnly trait delegated server-only effects — dropping the server leg",
                );
                Vec::new()
            } else {
                let traits: Vec<TraitFrom> = outcomes
                    .iter()
                    .filter(|(t, r)| r.executed || t == trait_name)
                    .map(|(t, r)| TraitFrom {
                        r#trait: t.clone(),
                        from: r.previous_state.clone(),
                        event: r.event.clone(),
                    })
                    .collect();
                let traits = (!traits.is_empty()).then_some(traits);
                if delegating_traits.contains(trait_name) {
                    // The seed's own arm needs the server: it replays from its
                    // pre-dispatch row and the server cascades the rest itself.
                    let entity_by_trait = (!seed_row.is_null())
                        .then(|| ValueMap::from([(trait_name.to_string(), seed_row.clone())]));
                    vec![EventRequest {
                        event: event.to_string(),
                        tick: tick.map(str::to_string),
                        payload: with_awaiting(payload.clone()),
                        entity_id: raw_entity_id.map(str::to_string),
                        target_trait: Some(trait_name.to_string()),
                        client_id: client_id.map(|s| s.to_string()),
                        source_trait: Some(trait_name.to_string()),
                        traits,
                        entity_by_trait,
                        ..Default::default()
                    }]
                } else {
                    // The seed has nothing to do on the server (the generated handler
                    // would refuse an event it has no arm for): address each
                    // listener whose delivery does.
                    let mut covered: HashSet<&str> = HashSet::new();
                    let mut legs = Vec::new();
                    for (t, r) in outcomes {
                        let Some(entry) = r.entry.as_ref().filter(|_| r.executed) else {
                            continue;
                        };
                        let delivery = DeliveryRecord::from_value(&entry.view.event);
                        let emitter = delivery.as_ref().and_then(|d| d.source.r#trait.as_deref());
                        if emitter.is_some_and(|e| covered.contains(e)) {
                            covered.insert(t.as_str());
                            continue;
                        }
                        if !delegating_traits.contains(t) {
                            continue;
                        }
                        covered.insert(t.as_str());
                        let Some(delivered) = r.event.clone() else {
                            continue;
                        };
                        let dispatch_log = dispatch_log_of(entry);
                        legs.push(EventRequest {
                            event: delivered,
                            tick: tick.map(str::to_string),
                            payload: with_awaiting(r.payload.clone()),
                            entity_id: raw_entity_id.map(str::to_string),
                            target_trait: Some(t.clone()),
                            client_id: client_id.map(|s| s.to_string()),
                            source_trait: emitter.map(str::to_string),
                            traits: traits.clone(),
                            entity_by_trait: (!entry.entity_before.is_null())
                                .then(|| ValueMap::from([(t.clone(), entry.entity_before.clone())])),
                            delivery,
                            dispatch_log,
                            ..Default::default()
                        });
                    }
                    legs
                }
            }
        } else {
            Vec::new()
        };

        let stateful_leg =
            (server_legs.is_empty() && !hybrid && self.effect_executor.environment().is_client())
                .then(|| StatefulLeg {
                    leg: EventRequest {
                        event: event.to_string(),
                        payload: with_awaiting(payload.clone()),
                        entity_id: raw_entity_id.map(str::to_string),
                        target_trait: Some(trait_name.to_string()),
                        client_id: client_id.map(str::to_string),
                        source_trait: Some(trait_name.to_string()),
                        ..Default::default()
                    },
                    awaiting: if tick.is_some() {
                        Vec::new()
                    } else {
                        self.awaiting_of(outcomes)
                    },
                });
        let awaiting = if tick.is_some() || server_legs.is_empty() {
            Vec::new()
        } else {
            self.awaiting_of(outcomes)
        };
        ClientDispatch {
            response,
            server_legs,
            outcomes: outcomes.to_vec(),
            seed: (trait_name.to_string(), event.to_string()),
            mode,
            snapshot,
            awaiting,
            stateful_leg,
        }
    }

    /// The server-backed traits of the client's mount still awaiting their lifecycle event, as the
    /// `_awaitingInit` a leg carries: a client-only trait's INIT never posts, so the host would hold its
    /// deliveries forever (twin of `withMountedSet`, `almadar-runtime/src/evaluation/client-role.ts`).
    pub fn awaiting_init_value(&self) -> Value {
        let defs = self.trait_definitions.read();
        let mut names: Vec<String> = self
            .mount
            .read()
            .awaiting
            .iter()
            .filter(|name| {
                defs.get(name.as_str()).is_none_or(|def| {
                    !matches!(self.dispatch_mode_of(def), DispatchMode::HybridClientOnly)
                })
            })
            .cloned()
            .collect();
        names.sort();
        Value::Array(names.into_iter().map(Value::String).collect())
    }

    /// The legs a fold's own arms owe the host, one per trait whose arm delegated an effect: a trait
    /// continuing its own unconsumed emit, or one whose delegated data lives in the browser store. A
    /// listener's server work is the host's own cascade, so it is dropped, as the JS fold drops it
    /// (`applyOrbitalEventResponse`). Drains the collector, so the fold's effects never ride a later dispatch.
    pub fn fold_follow_on_legs(
        &self,
        outcomes: &[(String, ProcessEventResult)],
        client_id: Option<&str>,
    ) -> Vec<EventRequest> {
        let _ = self.drain_server_leg_collector();
        let residence =
            crate::effects::Residence::of_entities(self.entity_definitions.read().values());
        outcomes
            .iter()
            .filter_map(|(trait_name, result)| {
                let entry = result.entry.as_ref().filter(|_| result.executed)?;
                let delegated: Vec<&Effect> = result
                    .effect_results
                    .iter()
                    .filter(|r| matches!(r.outcome, EffectOutcome::Delegated(_)))
                    .map(|r| &r.effect)
                    .collect();
                if delegated.is_empty() {
                    return None;
                }
                let delivery = DeliveryRecord::from_value(&entry.view.event);
                let emitter = delivery.as_ref().and_then(|d| d.source.r#trait.as_deref());
                let own = emitter == Some(trait_name.as_str());
                if !own && !delegated.iter().all(|e| browser_data_effect(e, &residence)) {
                    return None;
                }
                let event = result.event.clone()?;
                Some(EventRequest {
                    event: event.clone(),
                    payload: (!result.payload.is_null()).then(|| result.payload.clone()),
                    target_trait: Some(trait_name.clone()),
                    source_trait: Some(emitter.unwrap_or(trait_name).to_string()),
                    client_id: client_id.map(str::to_string),
                    traits: Some(vec![TraitFrom {
                        r#trait: trait_name.clone(),
                        from: result.previous_state.clone(),
                        event: Some(event),
                    }]),
                    entity_by_trait: (!entry.entity_before.is_null()).then(|| {
                        ValueMap::from([(trait_name.clone(), entry.entity_before.clone())])
                    }),
                    delivery,
                    dispatch_log: dispatch_log_of(entry),
                    ..Default::default()
                })
            })
            .collect()
    }

    /// Whether every effect is a data effect on a browser-stored entity (the host's store holds the data).
    fn all_browser_data(&self, effects: &[Effect]) -> bool {
        let residence =
            crate::effects::Residence::of_entities(self.entity_definitions.read().values());
        effects.iter().all(|e| browser_data_effect(e, &residence))
    }

    /// Each non-local trait a dispatch's local run executed, by its first firing (the web
    /// client kernel's `awaitingEntries`).
    fn awaiting_of(&self, outcomes: &[(String, ProcessEventResult)]) -> Vec<AwaitingTrait> {
        let defs = self.trait_definitions.read();
        let mut out: Vec<AwaitingTrait> = Vec::new();
        for (name, result) in outcomes {
            let Some(event) = result.event.as_ref().filter(|_| result.executed) else {
                continue;
            };
            if out.iter().any(|a| &a.trait_name == name) {
                continue;
            }
            let local = defs.get(name).is_some_and(|def| {
                matches!(self.dispatch_mode_of(def), DispatchMode::HybridClientOnly)
            });
            if local {
                continue;
            }
            out.push(AwaitingTrait {
                trait_name: name.clone(),
                event: event.clone(),
                from: result.previous_state.clone(),
            });
        }
        out
    }

    /// The tick twin of [`Self::dispatch_with_server_leg`]. Runs the tick's effects through
    /// [`Self::process_tick`], then takes each `emit` it ran through the normal cascade (its
    /// declared listeners, and the emitter's own arm for the event when it has one) WITHOUT
    /// running the emitter as a seed, so its last render is not repainted and no `guardFailed`
    /// is reported for an event it never received. Each emit yields one [`ClientDispatch`], with
    /// the tick's name stamped on the event's source and on the leg (`EventRequest.tick`).
    ///
    /// Web parity (`useClientTicks` -> `runCircuitEffects`): a tick's OWN `persist`/`fetch`/
    /// `call-service` is a documented no-op there (`persistDelegated`, `callServiceDelegated`;
    /// no event exists to replay it under), so it is discarded here too and must not ride the
    /// next dispatch's leg. Server work done by the emit's listeners does produce a leg, as a
    /// listener's dispatch does on web.
    pub fn dispatch_tick(
        &self,
        orbital: &str,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
        client_id: Option<&str>,
    ) -> Result<TickDispatch, KernelError> {
        let eid = entity_id.unwrap_or("singleton").to_string();
        let trait_def = self
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
            .ok_or_else(|| KernelError::TraitNotFound(trait_name.to_string()))?;
        let mode = self.dispatch_mode_of(&trait_def);
        // The leg replays the tick from the pre-tick row, so the store applies its writes once.
        let pre_tick_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

        let outcome = self.process_tick(trait_name, Some(&eid), tick_name)?;
        let (tick_own, _) = self.drain_server_leg_collector();
        // The tick's own delegated effects, by where their data lives: browser-stored data
        // leaves as one `runTick` leg; server-owned work in a browser-run tick is an
        // `orb validate` error (ORB_T_CLIENT_TICK_SERVER_EFFECT), so reaching here is a bug.
        let residence =
            crate::effects::Residence::of_entities(self.entity_definitions.read().values());
        let (browser_own, server_own): (Vec<Effect>, Vec<Effect>) = tick_own
            .into_iter()
            .partition(|effect| browser_data_effect(effect, &residence));
        let discarded_server_effects = server_own.len();
        if !server_own.is_empty() {
            tracing::error!(
                target: crate::logging::ns::RUNTIME_KERNEL,
                trait_name = %trait_name, tick = %tick_name, discarded = server_own.len(),
                "browser-run tick reached the server — orb validate should have rejected it (ORB_T_CLIENT_TICK_SERVER_EFFECT)",
            );
        }
        let browser_leg = (!browser_own.is_empty()).then(|| EventRequest {
            event: crate::effects::tick_event_key(tick_name),
            run_tick: Some(crate::effects::RunTick {
                trait_name: trait_name.to_string(),
                tick: tick_name.to_string(),
            }),
            tick: Some(tick_name.to_string()),
            entity_id: Some(eid.clone()),
            target_trait: Some(trait_name.to_string()),
            client_id: client_id.map(str::to_string),
            source_trait: Some(trait_name.to_string()),
            traits: Some(vec![TraitFrom {
                r#trait: trait_name.to_string(),
                from: outcome.previous_state.clone(),
                event: None,
            }]),
            entity_by_trait: (!pre_tick_row.is_null())
                .then(|| ValueMap::from([(trait_name.to_string(), pre_tick_row.clone())])),
            ..Default::default()
        });
        if !outcome.executed {
            return Ok(TickDispatch {
                outcome,
                discarded_server_effects,
                browser_leg,
                emits: Vec::new(),
                rerenders: Vec::new(),
            });
        }

        debug!(
            target: "almadar::kernel",
            orbital = %orbital, trait_name = %trait_name, tick = %tick_name, mode = ?mode,
            "dispatch_tick",
        );
        let mut source = self.emit_source(trait_name);
        source.tick = Some(tick_name.to_string());
        let mut emits = Vec::new();
        for er in &outcome.effect_results {
            let Effect::Emit {
                event: emitted,
                payload: ep,
            } = &er.effect
            else {
                continue;
            };
            let snapshot = matches!(mode, DispatchMode::RuntimeOptimistic)
                .then(|| self.snapshot_trait(trait_name, Some(&eid)));
            let seed_row = self.snapshot_trait(trait_name, Some(&eid)).entity;

            let _stamp = self.begin_dispatch();
            let mut own = Vec::new();
            let mut fanned = Vec::new();
            let handled = self.trait_handles(trait_name, &outcome.new_state, emitted);
            self.emit_followup(
                trait_name,
                &source,
                handled,
                emitted,
                ep,
                &mut own,
                &mut fanned,
            );
            let queue: VecDeque<CascadeItem> = own.into_iter().chain(fanned).collect();
            let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
            self.run_cascade(
                &eid,
                queue,
                &mut DispatchVisits::default(),
                &HashSet::new(),
                &mut DispatchMemory::default(),
                &mut outcomes,
            )?;
            let response = EventResponse::from_process_results(&outcomes, trait_name, emitted);
            let dispatch = self.finish_client_dispatch(LegSeed {
                trait_name,
                event: emitted,
                payload: ep.clone(),
                entity_id,
                client_id,
                tick: Some(tick_name),
                mode,
                snapshot,
                seed_row: &seed_row,
                outcomes: &outcomes,
                response,
            });
            emits.push(TickEmit {
                event: emitted.clone(),
                payload: ep.clone(),
                dispatch,
            });
        }
        Ok(TickDispatch {
            outcome,
            discarded_server_effects,
            browser_leg,
            emits,
            rerenders: self.rerender_stale(),
        })
    }

    /// Downcast the configured [`EffectDelegate`] to the concrete
    /// [`ServerLegCollector`] every client-role kernel is configured with
    /// (`KernelConfig.delegate`) and take everything it collected since the
    /// last drain. Empty when no delegate is configured (e.g. a Server-env
    /// kernel, or a Client-env kernel under test with no delegate wired).
    fn drain_server_leg_collector(&self) -> (Vec<Effect>, BTreeSet<String>) {
        self.server_leg_collector()
            .map(ServerLegCollector::drain_attributed)
            .unwrap_or_default()
    }

    fn server_leg_collector(&self) -> Option<&ServerLegCollector> {
        self.effect_executor
            .delegate()
            .and_then(|d| d.as_any())
            .and_then(|a| a.downcast_ref::<ServerLegCollector>())
    }

    /// The key of the frame `trait_name` reads: its own name, or the shared entity's group key.
    pub fn trait_frame_key(&self, trait_name: &str) -> String {
        let entity_type = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        self.frame_slot(trait_name, &entity_type, "").0
    }

    /// `(frame key, entity id)` of `trait_name`'s frame (Runtime Spec Clause 2.3).
    fn frame_slot(&self, trait_name: &str, entity_type: &str, entity_id: &str) -> (String, String) {
        let shared = self
            .entity_definitions
            .read()
            .get(entity_type)
            .is_some_and(|d| d.shared);
        let key = if shared {
            format!("$shared::{entity_type}")
        } else {
            trait_name.to_string()
        };
        (key, entity_id.to_string())
    }

    /// A trait's `@entity`: declared field defaults, then its row, then its frame.
    fn read_trait_entity(&self, trait_name: &str, entity_type: &str, entity_id: &str) -> Value {
        let defaults: ValueMap = self
            .entity_definitions
            .read()
            .get(entity_type)
            .map(|d| {
                d.fields
                    .iter()
                    .filter_map(|f| Some((f.name.clone()?, sexpr_to_value(f.default.as_ref()?))))
                    .collect()
            })
            .unwrap_or_default();
        let base = if defaults.is_empty() {
            Value::Null
        } else {
            Value::Object(defaults)
        };
        let row = self
            .entity_source
            .read(entity_type, entity_id)
            .unwrap_or(Value::Null);
        let with_row = merge_entity(&base, &row);
        let slot = self.frame_slot(trait_name, entity_type, entity_id);
        match self.frames.read().get(&slot) {
            Some(frame) => merge_entity(&with_row, frame),
            None => with_row,
        }
    }

    /// The `@entity` a step of `trait_name` would read now, for the cascade's visit key.
    fn step_entity_view(&self, trait_name: &str, entity_id: &str) -> Value {
        let entity_type = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        self.read_trait_entity(trait_name, &entity_type, entity_id)
    }

    /// Merge `set` writes into a trait's frame; the row store is never touched.
    fn write_trait_frame(
        &self,
        trait_name: &str,
        entity_type: &str,
        entity_id: &str,
        writes: &Value,
    ) {
        let slot = self.frame_slot(trait_name, entity_type, entity_id);
        let mut frames = self.frames.write();
        let merged = merge_entity(frames.get(&slot).unwrap_or(&Value::Null), writes);
        frames.insert(slot, merged);
    }

    /// Capture a trait instance's current state + entity frame, for
    /// `RuntimeOptimistic` rollback (`restore_trait`) when a caller's
    /// optimistic local commit disagrees with (or errors against) the
    /// server's response.
    ///
    /// Lock order (documented once, honored by every kernel method that reads
    /// more than one of these): **`state_manager` before `trait_definitions`
    /// before `entity_source`** — the order `process_event` already
    /// establishes (its `state_manager.write()` guard is held across the
    /// `trait_definitions.read()` and `entity_source.read()`/`write()` calls
    /// inside it). `snapshot_trait`/`restore_trait` never hold more than one
    /// of these at a time (each `.read()`/`.write()` call is a temporary,
    /// dropped before the next is taken), so they trivially satisfy the order
    /// and can never deadlock against a concurrent `dispatch`/`process_event`.
    pub fn snapshot_trait(&self, trait_name: &str, entity_id: Option<&str>) -> TraitSnapshot {
        let eid = entity_id.unwrap_or("singleton");
        let state = self
            .state_manager
            .read()
            .get_state(trait_name, Some(eid))
            .map(|s| s.current_state.clone())
            .unwrap_or_default();
        let entity_type = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        let slot = self.frame_slot(trait_name, &entity_type, eid);
        let entity = self
            .frames
            .read()
            .get(&slot)
            .cloned()
            .unwrap_or(Value::Null);
        TraitSnapshot { state, entity }
    }

    /// Restore a trait instance to a prior [`TraitSnapshot`] — the
    /// `RuntimeOptimistic` rollback path. Write-through in the SAME lock order
    /// `snapshot_trait` reads them (`state_manager` then `entity_source`,
    /// `trait_definitions` read only to resolve the entity type). A REPLACE of
    /// the entity, not the usual merge `EntitySource::write` performs — the
    /// optimistic commit may have ADDED fields since the snapshot that the
    /// rollback must remove, which a merge would silently keep.
    ///
    /// A bound trait's render of the rolled-back entity is stale from here: the next
    /// [`Self::rerender_stale`] repaints it (the web repaints from the restored store snapshot).
    /// Restore itself consumes nothing, so a caller with no response to carry it loses no repaint.
    pub fn restore_trait(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        snapshot: TraitSnapshot,
    ) {
        let eid = entity_id.unwrap_or("singleton");
        if !snapshot.state.is_empty() {
            let mut state_manager = self.state_manager.write();
            if state_manager
                .get_or_create_state(trait_name, Some(eid))
                .is_ok()
            {
                if let Some(state) = state_manager.get_state_mut(trait_name, Some(eid)) {
                    state.transition_to(&snapshot.state);
                }
            }
        }
        let entity_type = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        // REPLACE the frame, never merge: the optimistic commit may have added
        // fields a merge would keep. A `Null` snapshot means there was no frame.
        let slot = self.frame_slot(trait_name, &entity_type, eid);
        let mut frames = self.frames.write();
        if snapshot.entity.is_null() {
            frames.remove(&slot);
        } else {
            frames.insert(slot, snapshot.entity);
        }
    }

    /// Fold a server [`EventResponse`] into this kernel: `data`/`entity_by_trait`
    /// merge into the entity source (server wins), `states{}` write through as a
    /// RECONCILE (no guard evaluation — the server already decided the
    /// transition fired), and every `emitted_events` entry NOT in
    /// `already_delivered` fans out through the SAME cascade [`Self::dispatch`]
    /// uses (`run_cascade`) — the Rust twin of `useTraitStateMachine.ts`'s
    /// `stampLocallyDeliveredEchoes`, so a live-push echo of an event this
    /// client already realized locally doesn't re-run it.
    ///
    /// An emit whose source trait has an arm for it from its reconciled state and
    /// that the host did not already run (`source.dispatched`) is delivered to that
    /// trait first; its listeners fan out from that delivery.
    ///
    /// `already_delivered` also seeds the cascade's own visited-set, so a
    /// listener loop that cycles back onto an already-delivered pair is
    /// skipped too, not just the top-level entries.
    pub fn apply_server_response(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        if let Some(data) = &response.data {
            for (entity_type, rows) in data {
                for row in rows {
                    let id = row
                        .as_object()
                        .and_then(|o| o.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("singleton");
                    let _ = self.entity_source.write(entity_type, id, row);
                }
            }
        }

        // The host's paint of a slot is newer than the kernel's own render of it.
        {
            let mut renders = self.renders.write();
            for painted in response.client_effects_by_trait.iter().flatten() {
                if let crate::effects::ClientEffect::RenderUi { slot, .. } = &painted.effect {
                    renders.forget_slot(&painted.trait_name, slot);
                }
            }
        }

        let client_only: HashSet<String> = {
            let defs: Vec<Arc<TraitDefinition>> =
                self.trait_definitions.read().values().cloned().collect();
            defs.iter()
                .filter(|d| self.dispatch_mode_of(d) == DispatchMode::HybridClientOnly)
                .map(|d| d.name.clone())
                .collect()
        };

        let mut trait_entity_ids: HashMap<String, String> = HashMap::new();
        if let Some(entity_by_trait) = &response.entity_by_trait {
            let trait_defs = self.trait_definitions.read();
            for (trait_name, entity_val) in entity_by_trait {
                if client_only.contains(trait_name) {
                    continue;
                }
                let entity_type = trait_defs
                    .get(trait_name)
                    .and_then(|d| d.linked_entity.clone())
                    .unwrap_or_else(|| trait_name.clone());
                let id = entity_val
                    .as_object()
                    .and_then(|o| o.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| "singleton".to_string());
                self.write_trait_frame(trait_name, &entity_type, &id, entity_val);
                trait_entity_ids.insert(trait_name.clone(), id);
            }
        }

        {
            let mut state_manager = self.state_manager.write();
            for (trait_name, state_name) in &response.states {
                if client_only.contains(trait_name) {
                    continue;
                }
                let entity_id = Some(
                    trait_entity_ids
                        .get(trait_name)
                        .map_or("singleton", String::as_str),
                );
                if state_manager
                    .get_or_create_state(trait_name, entity_id)
                    .is_ok()
                {
                    if let Some(state) = state_manager.get_state_mut(trait_name, entity_id) {
                        state.transition_to(state_name);
                    }
                }
            }
        }

        let mut visited = DispatchVisits::default();
        let mut fanned: Vec<CascadeItem> = Vec::new();
        for ev in &response.emitted_events {
            let source = ev.source.clone().unwrap_or_default();
            let source_trait = source.r#trait.clone().unwrap_or_default();
            if already_delivered.contains(&(source_trait.clone(), ev.event.clone())) {
                continue;
            }
            let payload = ev.payload.clone().unwrap_or(Value::Null);
            let host_ran = source.dispatched == Some(true);
            if !host_ran
                && self.trait_handles(
                    &source_trait,
                    &self.current_state_name(&source_trait, Some("singleton")),
                    &ev.event,
                )
            {
                fanned.push(CascadeItem {
                    trait_name: source_trait.clone(),
                    event: ev.event.clone(),
                    delivery: delivery_record_value(&ev.event, &payload, &source),
                    source_payload: payload.clone(),
                    payload,
                    listen_guard: None,
                    fans_out: true,
                    repaint: None,
                });
                continue;
            }
            let before = fanned.len();
            self.collect_listener_targets(
                &source,
                &source_trait,
                &ev.event,
                &payload,
                &[],
                &mut fanned,
            );
            // The host's own fan-out already ran this emit's listeners (their rows and renders are in
            // this response); a client-only listener is the exception, its host run being dropped.
            if host_ran
                && self
                    .host_fans_out
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                let mut reached = fanned.split_off(before);
                reached.retain(|item| client_only.contains(&item.trait_name));
                fanned.extend(reached);
            }
        }

        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        // No single entity id is carried on the wire per emitted event (mirrors
        // `dispatch`'s own single-`eid`-per-call model) — cascaded processing uses
        // the same id-less convention `process_event` already falls back to.
        self.run_cascade(
            "singleton",
            fanned.into(),
            &mut visited,
            already_delivered,
            &mut DispatchMemory::default(),
            &mut outcomes,
        )?;
        Ok(outcomes)
    }

    /// Find every listener reacting to `event` from `source_trait`, evaluate each guard +
    /// payload mapping against `source_payload`, and push `(listener_trait, triggers, payload)`
    /// targets for the cascade queue.
    /// What a processed step queues next: the trait's own emits it handles from its new
    /// state (the same-trait cascade, Runtime Spec 5.3, which runs before any fan-out)
    /// and every listener target of the dispatched event and of each emit.
    fn step_followups(
        &self,
        trait_name: &str,
        event: &str,
        payload: &Value,
        result: Option<&ProcessEventResult>,
        direct_scopes: &[String],
        fans_out: bool,
    ) -> StepFollowups {
        let source = self.emit_source(trait_name);
        let mut own: Vec<CascadeItem> = Vec::new();
        let mut fanned = Vec::new();
        let mut consumed_emits = Vec::new();
        if fans_out {
            self.collect_listener_targets(
                &source,
                trait_name,
                event,
                payload,
                direct_scopes,
                &mut fanned,
            );
        }
        for (index, er) in result
            .map_or(&[][..], |r| &r.effect_results[..])
            .iter()
            .enumerate()
        {
            if let Effect::Emit {
                event: emitted,
                payload: ep,
            } = &er.effect
            {
                let handled = result.is_some_and(|r| {
                    r.executed && self.trait_handles(trait_name, &r.new_state, emitted)
                });
                let (own_before, fanned_before) = (own.len(), fanned.len());
                self.emit_followup(
                    trait_name,
                    &source,
                    handled,
                    emitted,
                    ep,
                    &mut own,
                    &mut fanned,
                );
                if own.len() > own_before || fanned.len() > fanned_before {
                    consumed_emits.push(index);
                }
            }
        }
        if let Some(result) = result.filter(|r| r.executed) {
            let mut repaints = self.first_repaints(trait_name, result);
            repaints.append(&mut own);
            own = repaints;
        }
        StepFollowups {
            own,
            fanned,
            consumed_emits,
        }
    }

    /// The re-renders a step's composition calls for: each capture child of `trait_name` that the fired
    /// arm's effects compose, to be re-fired under the step's payload (server role only: a client renders
    /// its own children).
    fn first_repaints(&self, trait_name: &str, result: &ProcessEventResult) -> Vec<CascadeItem> {
        if !self.config.environment.is_server() {
            return Vec::new();
        }
        let Some(children) = self.capture_children.read().get(trait_name).cloned() else {
            return Vec::new();
        };
        let Some(def) = self.trait_definitions.read().get(trait_name).cloned() else {
            return Vec::new();
        };
        let authored: Vec<&SExpression> =
            result.effect_runs.iter().map(|run| &run.effect).collect();
        let composed = super::callsite_capture::embedded_by_effects(&def, &authored);
        let payload = match &result.payload {
            Value::Null => Value::Object(ValueMap::new()),
            other => other.clone(),
        };
        let visited = Arc::new(parking_lot::Mutex::new(HashSet::new()));
        children
            .into_iter()
            .filter(|child| composed.contains(child))
            .map(|child| {
                Self::repaint_item(
                    child,
                    RepaintJob {
                        composed_with: payload.clone(),
                        keep_composed: false,
                        composed_by: None,
                        visited: Arc::clone(&visited),
                    },
                )
            })
            .collect()
    }

    fn repaint_item(child: String, job: RepaintJob) -> CascadeItem {
        CascadeItem {
            trait_name: child,
            event: String::new(),
            payload: Value::Null,
            delivery: Value::Null,
            source_payload: Value::Null,
            listen_guard: None,
            fans_out: false,
            repaint: Some(job),
        }
    }

    /// What one `emit` queues: the emitter's own arm for it (when `handled`, the same-trait
    /// cascade) and every listener target.
    #[allow(clippy::too_many_arguments)]
    fn emit_followup(
        &self,
        trait_name: &str,
        source: &BusEventSource,
        handled: bool,
        emitted: &str,
        payload: &Value,
        own: &mut Vec<CascadeItem>,
        fanned: &mut Vec<CascadeItem>,
    ) {
        if handled {
            own.push(CascadeItem {
                trait_name: trait_name.to_string(),
                event: emitted.to_string(),
                payload: payload.clone(),
                delivery: delivery_record_value(emitted, payload, source),
                source_payload: payload.clone(),
                listen_guard: None,
                fans_out: false,
                repaint: None,
            });
        }
        self.collect_listener_targets(source, trait_name, emitted, payload, &[], fanned);
    }

    /// Whether `trait_name` has an arm for `event` from `state`.
    fn trait_handles(&self, trait_name: &str, state: &str, event: &str) -> bool {
        self.trait_definitions
            .read()
            .get(trait_name)
            .is_some_and(|d| {
                d.state_machine
                    .transitions
                    .iter()
                    .any(|tr| tr.event == event && tr.from.matches(state))
            })
    }

    fn collect_listener_targets(
        &self,
        source: &BusEventSource,
        source_trait: &str,
        event: &str,
        source_payload: &Value,
        direct_scopes: &[String],
        out: &mut Vec<CascadeItem>,
    ) {
        let listeners = self.listener_manager.matching(event, source);
        if listeners.is_empty() {
            return;
        }
        let evaluator = Evaluator::new();
        let ctx = BindingContextBuilder::new()
            .payload(source_payload.clone())
            .build();
        let delivery = delivery_record_value(event, source_payload, source);
        for l in listeners {
            // `direct_scopes` already received this very event as a seed scope on the embed
            // chain; a same-name route back to one of them is that delivery again.
            if l.triggers == event && direct_scopes.contains(&l.trait_name) {
                continue;
            }
            let mapped = match &l.payload_mapping {
                Some(mapping) => {
                    let mut m = ValueMap::new();
                    for (k, expr) in mapping {
                        if let Ok(v) = evaluator.evaluate(expr, &ctx) {
                            m.insert(k.clone(), v);
                        }
                    }
                    Value::Object(m)
                }
                None => source_payload.clone(),
            };
            debug!(
                source_trait = %source_trait, event = %event,
                listener_trait = %l.trait_name, triggers = %l.triggers,
                "listener fan-out",
            );
            out.push(CascadeItem {
                trait_name: l.trait_name,
                event: l.triggers,
                payload: mapped,
                delivery: delivery.clone(),
                source_payload: source_payload.clone(),
                listen_guard: l.guard,
                fans_out: false,
                repaint: None,
            });
        }
    }

    /// Bring the response's rows to the frames as they stand when the dispatch ends: a trait's row is its
    /// FINAL row (a sibling sharing its frame may have written after it stepped), and every non-empty frame
    /// is echoed to every trait bound to it, so a bound trait that never ran still converges its row.
    /// JS twin: the final-row and frame-echo passes at the end of `evaluateOrbitalEvent`.
    pub fn echo_frames(&self, response: &mut EventResponse, entity_id: Option<&str>) {
        let eid = entity_id.unwrap_or("singleton");
        let defs = self.trait_definitions.read();
        let frames = self.frames.read();
        let mut rows = response.entity_by_trait.take().unwrap_or_default();
        for ((key, frame_eid), frame) in frames.iter() {
            let Some(frame_fields) = frame.as_object().filter(|f| !f.is_empty()) else {
                continue;
            };
            if frame_eid != eid {
                continue;
            }
            let bound: Vec<&str> = match key.strip_prefix("$shared::") {
                Some(entity) => defs
                    .values()
                    .filter(|def| def.linked_entity.as_deref() == Some(entity))
                    .map(|def| def.name.as_str())
                    .collect(),
                None => defs
                    .get(key)
                    .map(|def| def.name.as_str())
                    .into_iter()
                    .collect(),
            };
            for trait_name in bound {
                let row = rows
                    .entry(trait_name.to_string())
                    .or_insert_with(|| Value::Object(ValueMap::new()));
                if let Value::Object(fields) = row {
                    fields.extend(frame_fields.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
        }
        response.entity_by_trait = (!rows.is_empty()).then_some(rows);
    }

    /// The orbital that declares `trait_name`.
    pub fn orbital_of_trait(&self, trait_name: &str) -> Option<String> {
        self.emit_source(trait_name).orbital
    }

    /// Complete the source of each event `response` carries with the emitter's identity: orbital, the ids
    /// the schema carries, and the id of the emit contract. A client matches a `listens` source on these.
    pub fn stamp_emitted_sources(&self, response: &mut EventResponse) {
        for emitted in &mut response.emitted_events {
            let Some(source) = emitted.source.as_mut() else {
                continue;
            };
            let Some(trait_name) = source.r#trait.clone() else {
                continue;
            };
            let identity = self.emit_source(&trait_name);
            source.orbital = identity.orbital;
            source.orbital_id = identity.orbital_id;
            source.trait_id = identity.trait_id;
            source.event_id = self
                .trait_definitions
                .read()
                .get(&trait_name)
                .and_then(|def| def.emits.as_ref())
                .and_then(|emits| emits.iter().find(|e| e.event == emitted.event))
                .and_then(|contract| contract.event_id.clone());
        }
    }

    /// The envelope this trait stamps on its emits: `{ orbital, trait }` plus the ids the schema carries.
    fn emit_source(&self, trait_name: &str) -> BusEventSource {
        let mut source = BusEventSource::from_trait(trait_name);
        for schema in self.schemas.read().values() {
            for orbital in &schema.orbitals {
                let found = orbital
                    .traits
                    .iter()
                    .find(|r| r.trait_name() == Some(trait_name));
                if let Some(trait_ref) = found {
                    source.orbital = Some(orbital.name.clone());
                    source.orbital_id = orbital.id.clone();
                    if let TraitReference::Inline(def) = trait_ref {
                        source.trait_id = def.id.clone();
                    }
                    return source;
                }
            }
        }
        source
    }

    /// Execute effects and capture Set-effect data.
    fn execute_effects_with_entity(
        &self,
        effects: &[SExpression],
        ctx: &BindingContext,
    ) -> (Vec<EffectResult>, Value) {
        let (effect_results, entity_data, _, _) = self.execute_effects_traced(effects, ctx);
        (effect_results, entity_data)
    }

    /// `execute_effects_with_entity`, also reporting how each authored effect ran.
    fn execute_effects_traced(
        &self,
        effects: &[SExpression],
        ctx: &BindingContext,
    ) -> (Vec<EffectResult>, Value, Vec<EffectRun>, Option<String>) {
        let evaluator = Evaluator::new();
        let mut effect_results = Vec::new();
        let mut entity_data: ValueMap = ValueMap::new();
        let mut runs = Vec::with_capacity(effects.len());
        let mut failure = None;

        for effect_expr in effects {
            if failure.is_some() {
                runs.push(EffectRun {
                    effect: effect_expr.clone(),
                    status: EffectRunStatus::Skipped,
                    error: None,
                });
                continue;
            }
            let before = effect_results.len();
            self.exec_effect_expr(
                effect_expr,
                ctx,
                &evaluator,
                &mut effect_results,
                &mut entity_data,
            );
            let ran = &effect_results[before..];
            let failed = ran.iter().find(|r| r.is_failed());
            let status = if failed.is_some() {
                EffectRunStatus::Failed
            } else if ran.iter().any(|r| r.is_executed() || r.is_delegated()) {
                EffectRunStatus::Executed
            } else {
                EffectRunStatus::Skipped
            };
            runs.push(EffectRun {
                effect: effect_expr.clone(),
                status,
                error: failed.and_then(|r| r.error.clone()),
            });
            if failure.is_none() {
                failure = ran
                    .iter()
                    .find(|r| is_thrown_failure(r))
                    .map(|thrown| thrown.error.clone().unwrap_or_default());
            }
        }

        if failure.is_some() {
            effect_results.retain(is_thrown_failure);
        }

        (effect_results, Value::Object(entity_data), runs, failure)
    }

    /// Execute a single top-level effect expression.
    ///
    /// Leaf effects (`set`/`render-ui`/`emit`/`fetch`/…) go through the normal
    /// effect handler so entity writes + render flushing happen. The compound
    /// forms `let`/`if`/`do`/`when` are control flow, not effects: their parts
    /// are evaluated through the canonical `Evaluator` (the same `LetOp`/`IfOp`
    /// scope rules `eval_expr` uses) and their body effects are dispatched
    /// recursively, so a top-level `(let ((a v)…) (do (set …) (render-ui …)))`
    /// — a physics tick — actually runs instead of being dropped as a no-op
    /// `Effect::Custom`.
    fn exec_effect_expr(
        &self,
        effect_expr: &SExpression,
        ctx: &BindingContext,
        evaluator: &Evaluator,
        effect_results: &mut Vec<EffectResult>,
        entity_data: &mut ValueMap,
    ) {
        let mirrored = with_pending_sets(ctx, entity_data);
        let ctx: &BindingContext = &mirrored;
        if let SExpression::Array(items) = effect_expr {
            if let Some(SExpression::String(head)) = items.first() {
                match head.as_str() {
                    "let" => {
                        // [ "let", [[name, valExpr], …], body… ]
                        let mut child_ctx = ctx.child();
                        if let Some(SExpression::Array(bindings)) = items.get(1) {
                            for binding in bindings {
                                if let SExpression::Array(pair) = binding {
                                    if let (Some(SExpression::String(name)), Some(value_expr)) =
                                        (pair.first(), pair.get(1))
                                    {
                                        if let Ok(value) =
                                            evaluator.evaluate(value_expr, &child_ctx)
                                        {
                                            child_ctx.set_local(name, value);
                                        }
                                    }
                                }
                            }
                        }
                        for body in items.iter().skip(2) {
                            self.exec_effect_expr(
                                body,
                                &child_ctx,
                                evaluator,
                                effect_results,
                                entity_data,
                            );
                            if effect_results.iter().any(is_thrown_failure) {
                                break;
                            }
                        }
                    }
                    "do" => {
                        for body in items.iter().skip(1) {
                            self.exec_effect_expr(
                                body,
                                ctx,
                                evaluator,
                                effect_results,
                                entity_data,
                            );
                            if effect_results.iter().any(is_thrown_failure) {
                                break;
                            }
                        }
                    }
                    "if" => {
                        // [ "if", cond, then, else? ]
                        if let Some(cond) = items.get(1) {
                            let truthy = evaluator
                                .evaluate(cond, ctx)
                                .map(|v| v.is_truthy())
                                .unwrap_or(false);
                            let branch = if truthy { items.get(2) } else { items.get(3) };
                            if let Some(branch) = branch {
                                self.exec_effect_expr(
                                    branch,
                                    ctx,
                                    evaluator,
                                    effect_results,
                                    entity_data,
                                );
                            }
                        }
                    }
                    "when" => {
                        // [ "when", cond, body… ]
                        if let Some(cond) = items.get(1) {
                            let truthy = evaluator
                                .evaluate(cond, ctx)
                                .map(|v| v.is_truthy())
                                .unwrap_or(false);
                            if truthy {
                                for body in items.iter().skip(2) {
                                    self.exec_effect_expr(
                                        body,
                                        ctx,
                                        evaluator,
                                        effect_results,
                                        entity_data,
                                    );
                                    if effect_results.iter().any(is_thrown_failure) {
                                        break;
                                    }
                                }
                            }
                        }
                        return;
                    }
                    _ => {
                        self.exec_leaf_effect(
                            effect_expr,
                            ctx,
                            evaluator,
                            effect_results,
                            entity_data,
                        );
                    }
                }
                return;
            }
        }

        self.exec_leaf_effect(effect_expr, ctx, evaluator, effect_results, entity_data);
    }

    /// Execute a single leaf effect (`set`/`render-ui`/`emit`/`fetch`/…) via the
    /// canonical effect handler, capturing Set-effect data.
    fn exec_leaf_effect(
        &self,
        effect_expr: &SExpression,
        ctx: &BindingContext,
        evaluator: &Evaluator,
        effect_results: &mut Vec<EffectResult>,
        entity_data: &mut ValueMap,
    ) {
        // `ctx` arrives with this transition's earlier `(set @entity.…)` writes already mirrored
        // into `@entity` by `exec_effect_expr` (the same `entity_data`, unchanged since).
        if let Some(effect) = crate::effects::parse_effect(effect_expr) {
            match &effect {
                Effect::Set { path, value } => {
                    let evaluated_value = if value.as_array().is_some() {
                        let sexpr = value_to_sexpr(value);
                        match evaluator.evaluate(&sexpr, ctx) {
                            Ok(v) => v,
                            Err(err) => {
                                // A value that fails to evaluate is a failed effect, never stored raw.
                                tracing::error!(path = %path, error = %err, "kernel: set value failed to evaluate");
                                effect_results
                                    .push(EffectResult::failed(effect.clone(), &err.to_string()));
                                return;
                            }
                        }
                    } else if let Some(s) = value.as_str() {
                        if s.starts_with('@') {
                            // `resolve` expects a prefix-less path; a `@`-token must go
                            // through `resolve_binding` (strips `@`, then resolves). The
                            // old `ctx.resolve(s)` always returned None and wrote the
                            // literal token into the entity field.
                            ctx.resolve_binding(s).unwrap_or_else(|| value.clone())
                        } else {
                            value.clone()
                        }
                    } else {
                        value.clone()
                    };

                    let field_name = path.strip_prefix("@entity.").unwrap_or(path);
                    entity_data.insert(field_name.to_string(), evaluated_value.clone());

                    effect_results.push(EffectResult::executed(Effect::Set {
                        path: path.clone(),
                        value: evaluated_value,
                    }));
                }
                Effect::Fetch {
                    emit: Some(fetch_emit),
                    ..
                } if !fetch_emit.is_empty() => {
                    let result = self.effect_executor.execute(&effect, ctx);
                    let completion = fetch_completion_emit(fetch_emit, &result);
                    effect_results.push(result);
                    effect_results.extend(completion);
                }
                Effect::Persist {
                    emit: Some(persist_emit),
                    ..
                } if !persist_emit.is_empty() => {
                    let result = self.effect_executor.execute(&effect, ctx);
                    let completion = persist_completion_emit(persist_emit, &effect, &result);
                    effect_results.push(result);
                    effect_results.extend(completion);
                }
                Effect::PersistBatch {
                    emit: Some(batch_emit),
                    ..
                } if !batch_emit.is_empty() => {
                    let result = self.effect_executor.execute(&effect, ctx);
                    let completion = persist_completion_emit(batch_emit, &effect, &result);
                    effect_results.push(result);
                    effect_results.extend(completion);
                }
                Effect::CallService { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    self.io.set_call_site(self.call_site_of(emit.as_ref()));
                    let mut result = self.effect_executor.execute(&resolved, ctx);
                    let verdict = self.io.take_call_verdict();
                    self.io.set_call_site(None);
                    let key = match &resolved {
                        Effect::CallService { key, .. } => key.clone(),
                        _ => None,
                    };
                    if verdict == Some(CallVerdict::Cancelled) {
                        result = EffectResult::executed(resolved);
                    } else if let Effect::CallService { emit: routes, .. } = &mut result.effect {
                        routes.clone_from(emit);
                    }
                    let completion = emit.as_ref().and_then(|e| {
                        call_service_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion);
                }
                Effect::Custom {
                    emit: Some(op_emit),
                    ..
                } if !op_emit.is_empty() => {
                    let mut result = self.effect_executor.execute(&effect, ctx);
                    if let Effect::Custom { emit: routes, .. } = &mut result.effect {
                        *routes = Some(op_emit.clone());
                    }
                    let completion = substrate_completion_emit(op_emit, &result);
                    effect_results.push(result);
                    effect_results.extend(completion);
                }
                Effect::CancelCall { .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    effect_results.push(self.effect_executor.execute(&resolved, ctx));
                }
                Effect::Emit { .. } => {
                    // Resolve now, against this leaf's scope (`let` locals, earlier sets), as JS does at emit time.
                    if let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) {
                        effect_results.push(self.effect_executor.execute(&resolved, ctx));
                    }
                }
                _ => {
                    effect_results.push(self.effect_executor.execute(&effect, ctx));
                }
            }
        }
    }

    /// Give each form/detail/column-bearing node of a server-role `render-ui` the options of its relation
    /// fields (`relationsData`); the browser role reads its own store.
    fn inject_relation_options(&self, trait_name: &str, results: &mut [EffectResult]) {
        if !self.config.environment.is_server() {
            return;
        }
        let linked = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|def| def.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        let defs = self.entity_definitions.read();
        for result in results {
            if let Effect::RenderUi { content, .. } = &mut result.effect {
                super::relations::inject_relation_options(
                    &*self.entity_source,
                    &defs,
                    &linked,
                    content,
                );
            }
        }
    }

    /// The call site of a `call-service` made by the step in flight.
    fn call_site_of(&self, emit: Option<&FetchEmit>) -> Option<CallSite> {
        let (trait_name, follow_on) = self.step_site.read().clone()?;
        let orbital = self.emit_source(&trait_name).orbital;
        let routes = emit.map_or_else(CallRoutes::default, |e| CallRoutes {
            success: e.success.clone(),
            failure: e.failure.clone(),
            cancelled: e.cancelled.clone(),
            on_message: e.on_message.clone(),
        });
        Some(CallSite {
            orbital,
            trait_name,
            follow_on,
            emit: routes,
        })
    }

    /// Get render-ui effects for a state to show UI when no transition happens.
    fn get_state_render_effects(
        &self,
        trait_def: &TraitDefinition,
        state_name: &str,
        ctx: &BindingContext,
        _entity_data: &Value,
    ) -> Vec<EffectResult> {
        // Resolve every binding through the one canonical walker
        // (`ctx.entity` already carries the current entity here).
        state_render_sources(trait_def, state_name)
            .into_iter()
            .map(|(slot, content)| {
                resolve_effect_result(
                    EffectResult::executed(Effect::RenderUi { slot, content }),
                    ctx,
                )
            })
            .collect()
    }

    /// Keep a client trait's just-run `render-ui` effects (`sources`: unresolved, in run order;
    /// `resolved`: the same effects after binding resolution) so [`Self::rerender_stale`] can
    /// repaint them when the entity they read changes.
    fn record_renders(
        &self,
        trait_name: &str,
        entity_id: &str,
        sources: Vec<(String, Value)>,
        resolved: &[EffectResult],
        ctx: &BindingContext,
    ) {
        if sources.is_empty() || !self.effect_executor.environment().is_client() {
            return;
        }
        let resolved = resolved.iter().filter_map(|er| match &er.effect {
            Effect::RenderUi { content, .. } => Some(content.clone()),
            _ => None,
        });
        let renders = sources
            .into_iter()
            .zip(resolved)
            .map(|((slot, source), content)| (slot, source, content))
            .collect();
        self.renders
            .write()
            .record(trait_name, entity_id, renders, ctx);
    }

    /// Repaint every mounted trait whose last `render-ui` reads an entity that has changed since
    /// it was resolved. Which leaves read the entity is declared in the source: a leaf holding
    /// `@entity` and no `@payload` (the web's `RenderBindingMarker`); everything else stays at
    /// the value it was flushed with. Only a client-role kernel repaints, and only for an entity
    /// the client owns (`[runtime]`, `[shared]`, browser-stored or unbound; the web's
    /// `clientResolvesRenderBindings`), since a persistent entity's bindings are server-resolved.
    /// A trait changed by several writes appears once; a second call with nothing new is empty.
    pub fn rerender_stale(&self) -> Vec<Rerender> {
        if !self.effect_executor.environment().is_client() {
            return Vec::new();
        }
        let stale = self.renders.write().stale(
            |trait_name| {
                let entity_type = self
                    .trait_definitions
                    .read()
                    .get(trait_name)
                    .and_then(|d| d.linked_entity.clone())
                    .unwrap_or_else(|| trait_name.to_string());
                self.entity_definitions
                    .read()
                    .get(&entity_type)
                    .is_none_or(|d| d.shared || d.is_client_resident())
            },
            |trait_name, entity_id| self.step_entity_view(trait_name, entity_id),
        );
        stale
            .into_iter()
            .map(|(trait_name, entity_id, repaints)| Rerender {
                state: self.current_state_name(&trait_name, Some(&entity_id)),
                entity: self.step_entity_view(&trait_name, &entity_id),
                effects: repaints
                    .into_iter()
                    .map(|r| Effect::RenderUi {
                        slot: r.slot,
                        content: r.content,
                    })
                    .collect(),
                trait_name,
            })
            .collect()
    }

    /// The state a trait instance is in now, or the one it will be created in; empty for an unknown trait.
    fn current_state_name(&self, trait_name: &str, entity_id: Option<&str>) -> String {
        if let Some(state) = self.state_manager.read().get_state(trait_name, entity_id) {
            return state.current_state.clone();
        }
        self.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| crate::state_machine::StateMachineCore::get_trait_initial_state(d))
            .map(str::to_string)
            .unwrap_or_default()
    }

    /// A registered trait's derived dispatch mode (`dispatch_mode::compute`).
    pub fn dispatch_mode_of(&self, trait_def: &TraitDefinition) -> DispatchMode {
        let entity_def = trait_def
            .linked_entity
            .as_deref()
            .and_then(|name| self.entity_definitions.read().get(name).cloned());
        let residence =
            crate::effects::Residence::of_entities(self.entity_definitions.read().values());
        trait_def.dispatch_mode(entity_def.as_ref(), &residence)
    }

    /// Every registered trait's state (its initial one while untouched) and derived dispatch mode.
    pub fn trait_snapshot(
        &self,
        entity_id: Option<&str>,
    ) -> BTreeMap<String, (String, DispatchMode)> {
        let defs: Vec<Arc<TraitDefinition>> =
            self.trait_definitions.read().values().cloned().collect();
        defs.iter()
            .map(|def| {
                let state = self.current_state_name(&def.name, entity_id);
                (def.name.clone(), (state, self.dispatch_mode_of(def)))
            })
            .collect()
    }

    /// Get the current state for an entity.
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> Option<TraitState> {
        self.state_manager
            .read()
            .get_state(trait_name, Some(entity_id.unwrap_or("singleton")))
            .cloned()
    }

    /// Seed a delegated server leg with the client's circuit state: each carried trait is held at its
    /// `from` state, and each carried row is folded into its trait's frame (`@entity`), so the leg replays
    /// from where the client was. JS twin: `evaluateOrbitalEvent`'s `manager.seedState` + frame fold.
    pub fn seed_leg(
        &self,
        traits: &[TraitFrom],
        entity_by_trait: Option<&ValueMap>,
        entity_id: Option<&str>,
    ) {
        let eid = entity_id.unwrap_or("singleton");
        for carried in traits {
            if !self.trait_definitions.read().contains_key(&carried.r#trait) {
                continue;
            }
            let mut state_manager = self.state_manager.write();
            if state_manager
                .get_or_create_state(&carried.r#trait, Some(eid))
                .is_ok()
            {
                if let Some(state) = state_manager.get_state_mut(&carried.r#trait, Some(eid)) {
                    state.transition_to(&carried.from);
                }
            }
        }
        for (trait_name, row) in entity_by_trait.into_iter().flatten() {
            let entity_type = match self.trait_definitions.read().get(trait_name) {
                Some(def) => def
                    .linked_entity
                    .clone()
                    .unwrap_or_else(|| trait_name.clone()),
                None => continue,
            };
            if row.as_object().is_some_and(|o| !o.is_empty()) {
                self.write_trait_frame(trait_name, &entity_type, eid, row);
            }
        }
    }

    /// Reset a trait instance to its initial state.
    pub fn reset_state(&self, trait_name: &str, entity_id: Option<&str>) {
        let _ = self
            .state_manager
            .write()
            .reset_trait(trait_name, entity_id);
    }

    /// Set the current user.
    /// Declare that the host runs the listeners of every emit it marks `dispatched` (a worklist host:
    /// the JS runtime, `orbital-server`). Default `false`: a generated server delivers no listener, so
    /// the fold runs them all.
    pub fn set_host_fans_out(&self, fans_out: bool) {
        self.host_fans_out
            .store(fans_out, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_current_user(&self, user: Value) {
        *self.current_user.write() = user;
    }

    /// Get the current user.
    /// Stamp `now` once for the dispatch this call starts; a nested call reuses the outer stamp.
    fn begin_dispatch(&self) -> DispatchStamp {
        let mut slot = self.dispatch_now.write();
        match *slot {
            Some(now) => DispatchStamp { slot: None, now },
            None => {
                let now = crate::time_util::now_millis_f64();
                *slot = Some(now);
                DispatchStamp {
                    slot: Some(Arc::clone(&self.dispatch_now)),
                    now,
                }
            }
        }
    }

    pub fn get_current_user(&self) -> Value {
        self.current_user.read().clone()
    }

    /// Install the program's declared locales and merged catalogs (`orb resolve`'s `<stem>.<locale>.json`).
    pub fn set_messages(
        &self,
        locales: Vec<String>,
        catalogs: HashMap<String, HashMap<String, String>>,
    ) {
        *self.i18n.write() = KernelI18n { locales, catalogs };
    }

    /// Run the next dispatches as the viewer's `locale` until the guard drops. A program
    /// that declares locales refuses one it does not declare; one without accepts any.
    pub fn enter_locale(&self, locale: Option<&str>) -> Result<LocaleGuard, KernelError> {
        if let Some(locale) = locale {
            let i18n = self.i18n.read();
            if !i18n.locales.is_empty() && !i18n.locales.iter().any(|l| l == locale) {
                return Err(KernelError::Internal(format!(
                    "locale \"{locale}\" is not one of the program's declared locales [{}]",
                    i18n.locales.join(", ")
                )));
            }
        }
        *self.dispatch_locale.write() = locale.map(str::to_string);
        Ok(LocaleGuard {
            slot: Arc::clone(&self.dispatch_locale),
        })
    }

    /// `@locale` and its catalog for `i18n/t`: the dispatch's viewer locale, else the first declared.
    fn with_viewer_i18n(&self, builder: BindingContextBuilder) -> BindingContextBuilder {
        let i18n = self.i18n.read();
        let locale = self
            .dispatch_locale
            .read()
            .clone()
            .or_else(|| i18n.locales.first().cloned());
        match locale {
            Some(locale) => {
                let messages = i18n.catalogs.get(&locale).cloned().unwrap_or_default();
                builder.locale(Value::String(locale)).messages(messages)
            }
            None => builder,
        }
    }

    /// Set the kernel's tracked light/dark toggle. Overrides the mode
    /// encoded in a declared theme's key suffix on every subsequent
    /// `@currentTheme` resolution (`resolve_theme`'s `requested_mode`).
    pub fn set_theme_mode(&self, mode: ThemeMode) {
        *self.theme_mode.write() = Some(mode);
    }

    /// Get the kernel's tracked light/dark toggle, defaulting to `Light`
    /// when no explicit mode was ever set. External-facing convenience
    /// (status/introspection); theme RESOLUTION uses the raw `Option`
    /// directly (below) so an unset toggle still lets `resolve_theme` honor
    /// a declared theme key's own `-light`/`-dark` suffix instead of this
    /// default silently overriding it.
    pub fn get_theme_mode(&self) -> ThemeMode {
        self.theme_mode.read().unwrap_or_default()
    }

    /// Resolve the app-level `@currentTheme` (orbital precedence not
    /// applied — see [`Self::resolve_current_theme_for_trait`] for the
    /// per-trait resolution `process_event`/`process_tick` bind into
    /// render-ui). Snapshot-facing: the FFI's `orbital_kernel_snapshot`
    /// surfaces this as `"theme"`.
    pub fn resolved_theme(&self) -> ResolvedTheme {
        let mode = *self.theme_mode.read();
        match self.schemas.read().values().next() {
            Some(schema) => resolve_theme(schema, None, &self.theme_presets.read(), mode),
            None => resolve_theme_default(&self.theme_presets.read(), mode.unwrap_or_default()),
        }
    }

    /// Resolve `@currentTheme` for the orbital that owns `trait_name`,
    /// applying orbital → app → default precedence (`resolve_theme`). Walks
    /// the registered schemas the same way [`Self::get_initial_state`] does
    /// — no separate trait→orbital index to keep in sync.
    fn resolve_current_theme_for_trait(&self, trait_name: &str) -> ResolvedTheme {
        let mode = *self.theme_mode.read();
        let schemas = self.schemas.read();
        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                if orbital
                    .traits
                    .iter()
                    .any(|t| t.trait_name() == Some(trait_name))
                {
                    return resolve_theme(
                        schema,
                        Some(&orbital.name),
                        &self.theme_presets.read(),
                        mode,
                    );
                }
            }
        }
        resolve_theme_default(&self.theme_presets.read(), mode.unwrap_or_default())
    }

    /// Narrow `rows` to the ones the CURRENT USER may read, using the entity's
    /// declared `@read` policy.
    ///
    /// Exists so a caller measuring "how much does this persona see" measures the
    /// same thing a fetch would. Counting straight off the entity store answers a
    /// different question — the unfiltered store size, identical for every persona —
    /// which is how `--all-personas` came to report symmetric results for correctly
    /// ownership-scoped apps (`S-C2-ASYMMETRY-BLIND-TO-OWNERSHIP-ONLY-SCOPING`).
    ///
    /// Reuses `apply_read_access`, the one owner of read filtering, rather than
    /// re-deriving the predicate.
    pub fn retain_readable(&self, entity_type: &str, rows: &mut Vec<Value>) {
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        if policy.is_none() {
            return;
        }
        let mut ctx = BindingContext::new();
        ctx.set_user(self.get_current_user());
        apply_read_access(rows, policy.as_ref(), None, &ctx, entity_type);
    }

    /// List all registered orbitals across all schemas.
    pub fn list_orbitals(&self) -> Vec<OrbitalInfo> {
        let schemas = self.schemas.read();
        let mut orbitals = Vec::new();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                let entity_name = match &orbital.entity {
                    EntityRef::Inline(e) => e.name.clone(),
                    EntityRef::Reference(r) => r.clone(),
                    EntityRef::Extends(e) => e.reference.clone(),
                };
                orbitals.push(OrbitalInfo {
                    name: orbital.name.clone(),
                    entity_name,
                    trait_count: orbital.traits.len(),
                    page_count: orbital.pages.as_ref().map(|p| p.len()).unwrap_or(0),
                });
            }
        }

        orbitals
    }

    /// Get detailed information for a single orbital.
    pub fn get_orbital(&self, orbital_name: &str) -> Option<OrbitalDetail> {
        let schemas = self.schemas.read();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                if orbital.name == orbital_name {
                    return Some(OrbitalDetail {
                        name: orbital.name.clone(),
                        entity: orbital.entity.clone(),
                        traits: orbital
                            .traits
                            .iter()
                            .filter_map(|t| t.trait_name().map(|s| s.to_string()))
                            .collect(),
                        pages: orbital.pages.clone().unwrap_or_default(),
                    });
                }
            }
        }

        None
    }

    /// Declared external inputs across every registered schema.
    pub fn external_inputs(&self) -> Vec<super::external_inputs::ExternalInput> {
        self.schemas
            .read()
            .values()
            .flat_map(super::external_inputs::external_inputs_of)
            .collect()
    }

    /// Get the initial state for a trait.
    pub fn get_initial_state(&self, trait_name: &str) -> Option<String> {
        let schemas = self.schemas.read();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                for trait_ref in &orbital.traits {
                    if let Some(name) = trait_ref.trait_name() {
                        if name == trait_name {
                            if let TraitReference::Inline(trait_def) = trait_ref {
                                let sm = &trait_def.state_machine;
                                for state in &sm.states {
                                    if state.is_initial.unwrap_or(false) {
                                        return Some(state.name.clone());
                                    }
                                }
                                if let Some(first_state) = sm.states.first() {
                                    return Some(first_state.clone().name);
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }

    /// Get runtime statistics.
    pub fn stats(&self) -> RuntimeStats {
        let manager_stats = self.state_manager.read().stats();
        RuntimeStats {
            registered_schemas: self.schemas.read().len(),
            registered_traits: manager_stats.registered_traits,
            active_instances: manager_stats.active_instances,
            cached_entities: self.entity_source.count(),
        }
    }

    /// Access the event bus.
    pub fn event_bus(&self) -> &SharedEventBus {
        &self.event_bus
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
    pub fn config(&self) -> &KernelConfig {
        &self.config
    }
}

/// The `(trait, event)` pairs a [`ClientDispatch`] already realized LOCALLY —
/// every event its own outcomes emitted (`response.emitted_events`), plus the
/// seed dispatch itself, read off `server_leg` (a leg's `target_trait`/`event`
/// ARE the seed trait's own dispatch — see `dispatch_with_server_leg`; `None`
/// when no leg was produced, so only the local emits are known). Pass the
/// result to [`RuntimeKernel::apply_server_response`] so a live-push echo of
/// an event this client already ran doesn't run it again — the Rust twin of
/// `useTraitStateMachine.ts`'s `stampLocallyDeliveredEchoes`.
pub fn already_delivered_from(dispatch: &ClientDispatch) -> HashSet<(String, String)> {
    let mut set: HashSet<(String, String)> = dispatch
        .response
        .emitted_events
        .iter()
        .map(|e| {
            (
                e.source
                    .as_ref()
                    .and_then(|s| s.r#trait.clone())
                    .unwrap_or_default(),
                e.event.clone(),
            )
        })
        .collect();
    if !dispatch.server_legs.is_empty() {
        set.insert(dispatch.seed.clone());
    }
    for leg in &dispatch.server_legs {
        if let Some(target) = &leg.target_trait {
            set.insert((target.clone(), leg.event.clone()));
        }
    }
    set
}

/// A runtime value as the expression it denotes. Equals the JSON round trip this replaces:
/// JSON has no NaN or infinity, so a non-finite number reads back as `null`.
fn value_to_sexpr(value: &Value) -> SExpression {
    fn finite(expr: &mut SExpression) {
        match expr {
            SExpression::Number(n) if !n.is_finite() => *expr = SExpression::Null,
            SExpression::Array(items) => items.iter_mut().for_each(finite),
            SExpression::Object(obj) => obj.values_mut().for_each(finite),
            _ => {}
        }
    }
    let mut expr: SExpression = value.clone().into();
    finite(&mut expr);
    expr
}

/// Convert an SExpression to a runtime Value for instance seeding. Shared with
/// `persona::identity_roster`, which materializes the same authored `instances`.
pub(crate) fn sexpr_to_value(sexpr: &SExpression) -> Value {
    match sexpr {
        SExpression::String(s) => Value::String(s.clone()),
        SExpression::Number(n) => Value::Number(*n),
        SExpression::Bool(b) => Value::Bool(*b),
        SExpression::Null => Value::Null,
        SExpression::Array(items) => Value::Array(items.iter().map(sexpr_to_value).collect()),
        SExpression::Object(obj) => {
            let map: ValueMap = obj
                .iter()
                .map(|(k, v)| (k.clone(), sexpr_to_value(v)))
                .collect();
            Value::Object(map)
        }
    }
}

/// Build the `@config` root for the live render context from the trait's declared
/// config field defaults. Mirrors the JS runtime, where `@config.X` resolves against
/// the trait's config defaults (and instance overrides). Std behaviors rendered
/// standalone rely on defaults only — instance/call-site overrides are a deeper
/// composition concern (follow-up). A field with no `default` is left out: its `@config.<knob>` is unset.
fn config_defaults_value<T: std::borrow::Borrow<TraitDefinition>>(
    trait_def: &TraitDefinition,
    all_traits: &HashMap<String, T>,
) -> Value {
    let mut map: ValueMap = ValueMap::new();
    if let Some(cfg) = &trait_def.config {
        for (k, field) in cfg {
            if let Some(default) = field.default.as_ref() {
                map.insert(k.clone(), sexpr_to_value(default));
            }
        }
    }
    resolve_config_forwards(&mut map, all_traits);
    Value::Object(map)
}

/// A config value that is exactly a `@user.<path>` binding reads the viewer one hop; a field the viewer
/// lacks, or one that is not a string, number or boolean, becomes `""`, never the sigil (twin of
/// `buildConfigBinding`, `almadar-runtime/src/traits/config-defaults.ts`).
fn bind_user_forwards(config: Value, user: &Value) -> Value {
    let Value::Object(mut map) = config else {
        return config;
    };
    for value in map.values_mut() {
        let Value::String(s) = value else { continue };
        let Some(path) = s.strip_prefix("@user.") else {
            continue;
        };
        if path.is_empty()
            || !path.split('.').all(|seg| {
                !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            continue;
        }
        *value = match user.get_path(path) {
            Some(read @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => read.clone(),
            _ => Value::String(String::new()),
        };
    }
    Value::Object(map)
}

/// Chain wrapper-forward config defaults to the enclosing/host trait's knob.
///
/// A peer trait inlined from a wrapper (e.g. `std-export`'s `Button1`, inlined
/// from `ui-button`) can carry a config default that is itself a `@config.<key>`
/// forward — `Button1.label = @config.buttonLabel` — where `<key>` is a knob on
/// the HOST trait that composes it (`ExportMenu.buttonLabel = "Export"`), not a
/// knob of the peer. The inline phase collapses the peer's own `@config.label`
/// render binding to the forward token `@config.buttonLabel`, so the peer's
/// render-ui reads `buttonLabel` — a key ABSENT from its own config root, which
/// would otherwise paint the literal token.
///
/// For each of the trait's config fields whose default is a `@config.<key>`
/// forward, expose `<key>` in the live config root, resolved against the host
/// trait that declares it (its `default` already carries any folded call-site
/// override, so a host-level override flows to the child). The local field is
/// collapsed too, so both `@config.<key>` and `@config.<field>` resolve. The
/// forward stays in the trait's declared config + render-ui; only the LIVE render
/// context is chained, so the knob remains overridable.
fn resolve_config_forwards<T: std::borrow::Borrow<TraitDefinition>>(
    map: &mut ValueMap,
    all_traits: &HashMap<String, T>,
) {
    let forwards: Vec<(String, String)> = map
        .iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => forward_key(s).map(|fk| (k.clone(), fk)),
            _ => None,
        })
        .collect();
    for (field, fwd_key) in forwards {
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(field.clone());
        if let Some(resolved) = resolve_forward_key(&fwd_key, all_traits, &mut visited) {
            // Expose the host knob under its own name (the token the render-ui
            // reads) without clobbering a same-named local concrete knob…
            map.entry(fwd_key).or_insert_with(|| resolved.clone());
            // …and collapse the forwarding field itself.
            map.insert(field, resolved);
        }
    }
}

/// The single-segment key of a `@config.<key>` wrapper-forward string. Nested
/// paths (`@config.x.y`) are not host-knob forwards and return `None`.
fn forward_key(s: &str) -> Option<String> {
    s.strip_prefix("@config.")
        .filter(|rest| !rest.contains('.'))
        .map(|rest| rest.to_string())
}

/// Resolve a host-knob `key` to its effective value by searching the orbital's
/// traits (deterministic name order) for the one that declares it. Prefers a
/// concrete default; if the only declarations are themselves forwards, chains
/// transitively. `visited` guards against forward cycles.
fn resolve_forward_key<T: std::borrow::Borrow<TraitDefinition>>(
    key: &str,
    all_traits: &HashMap<String, T>,
    visited: &mut HashSet<String>,
) -> Option<Value> {
    if !visited.insert(key.to_string()) {
        return None;
    }
    let mut names: Vec<&String> = all_traits.keys().collect();
    names.sort();
    let mut chained_next: Option<String> = None;
    for name in names {
        let Some(cfg) = &all_traits[name].borrow().config else {
            continue;
        };
        let Some(field) = cfg.get(key) else { continue };
        let Some(default) = &field.default else {
            continue;
        };
        let v = sexpr_to_value(default);
        if let Value::String(s) = &v {
            if let Some(next) = forward_key(s) {
                if next != key {
                    chained_next.get_or_insert(next);
                }
                continue;
            }
        }
        return Some(v);
    }
    chained_next.and_then(|next| resolve_forward_key(&next, all_traits, visited))
}

/// Build the completion `emit` for a `fetch` effect: on success, fire `emit.success`
/// carrying the fetched rows as `{ "data": [...] }` (so a `@payload.data`-bound table
/// renders); on failure, fire `emit.failure`. This is what advances a data-loading
/// trait from its `loading` state to its `loaded` state. Returns `None` when the fetch
/// was delegated/no-op (the real fetch — and its emit — happens in the other environment).
/// The `render-ui` effects among `results`, as `(slot, unresolved content)` in run order.
fn render_sources(results: &[EffectResult]) -> Vec<(String, Value)> {
    results
        .iter()
        .filter_map(|er| match &er.effect {
            Effect::RenderUi { slot, content } => Some((slot.clone(), content.clone())),
            _ => None,
        })
        .collect()
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
        .filter_map(|expr| match crate::effects::parse_effect(expr) {
            Some(Effect::RenderUi { slot, content }) => Some((slot, content)),
            _ => None,
        })
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
                .filter_map(DeliveryRecord::from_value)
                .collect(),
            prev_states: entry.view.prev_states.to_vec(),
        }
    })
}

fn browser_data_effect(effect: &Effect, residence: &crate::effects::Residence) -> bool {
    match effect {
        Effect::Persist { entity_type, .. } | Effect::Fetch { entity_type, .. } => {
            residence.is_client(entity_type)
        }
        _ => false,
    }
}

/// Merge the Set-effect `set` fields into the `base` entity (read-modify-write).
/// Set fields override; untouched base fields are preserved. Used to PERSIST the
/// post-Set entity so a later re-render still sees every field (a partial replace-
/// write of only the Set-touched fields drops the rest and breaks
/// `@entity.<untouched-field>` on the next frame). Empty/Null `set` ⇒ `base`;
/// a non-object `set` with no base ⇒ `set` (first write).
/// `ctx` with the transition's earlier `(set @entity.…)` writes merged into `@entity`, so
/// every read inside the transition sees them: leaf effects, `if`/`when` conditions,
/// `let` values (JS twin: `EffectExecutor`'s bindings mirror).
fn with_pending_sets<'a>(
    ctx: &'a BindingContext,
    entity_data: &ValueMap,
) -> std::borrow::Cow<'a, BindingContext> {
    if entity_data.is_empty() {
        return std::borrow::Cow::Borrowed(ctx);
    }
    let mut c = ctx.clone();
    c.entity = Arc::new(match ctx.entity.as_object() {
        Some(base) => {
            let mut merged = base.clone();
            merged.extend(entity_data.iter().map(|(k, v)| (k.clone(), v.clone())));
            Value::Object(merged)
        }
        None => Value::Object(entity_data.clone()),
    });
    std::borrow::Cow::Owned(c)
}

fn merge_entity(base: &Value, set: &Value) -> Value {
    match (base.as_object(), set.as_object()) {
        (Some(b), Some(s)) if !s.is_empty() => {
            let mut merged = b.clone();
            for (k, v) in s {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        }
        _ if set.as_object().map(|o| o.is_empty()).unwrap_or(true) => base.clone(),
        _ => set.clone(),
    }
}

/// Resolve every `@`-binding in a client-facing effect's content/payload against `ctx`,
/// via the canonical `BindingContext::resolve_value`. Non-client effects pass through.
fn resolve_effect_bindings(effect: Effect, ctx: &BindingContext) -> Result<Effect, EvalError> {
    Ok(match effect {
        Effect::RenderUi { slot, content } => {
            let resolved = ctx.resolve_value_checked(&content)?.into_owned();
            debug!(
                target: "almadar::kernel",
                slot = %slot,
                before = %content,
                after = %resolved,
                "kernel: resolve render-ui bindings"
            );
            Effect::RenderUi {
                slot,
                content: resolved,
            }
        }
        Effect::Emit { event, payload } => Effect::Emit {
            event,
            payload: ctx.resolve_dropping_unset_checked(&payload)?,
        },
        Effect::Navigate {
            path,
            params,
            options,
        } => Effect::Navigate {
            path,
            params: params
                .map(|p| {
                    p.iter()
                        .map(|(k, v)| Ok((k.clone(), ctx.resolve_value_checked(v)?.into_owned())))
                        .collect::<Result<_, EvalError>>()
                })
                .transpose()?,
            options: options.map(|o| crate::effects::NavigateOptions {
                crumb: o.crumb.map(|c| {
                    ctx.resolve_value(&Value::String(c))
                        .into_owned()
                        .to_string_value()
                }),
            }),
        },
        Effect::CallService {
            service,
            method,
            args,
            key,
            emit,
        } => Effect::CallService {
            service,
            method,
            args,
            key: key.map(|k| resolve_key(&k, ctx)),
            emit,
        },
        Effect::CancelCall { key } => Effect::CancelCall {
            key: resolve_key(&key, ctx),
        },
        other => other,
    })
}

/// `effect` with its bindings resolved, or `None` after recording the typed argument mismatch as the effect's failure.
fn resolved_or_failed(
    effect: &Effect,
    ctx: &BindingContext,
    effect_results: &mut Vec<EffectResult>,
) -> Option<Effect> {
    match resolve_effect_bindings(effect.clone(), ctx) {
        Ok(resolved) => Some(resolved),
        Err(fault) => {
            effect_results.push(EffectResult::failed(
                effect.clone(),
                &OrbitalError::Eval(fault).to_string(),
            ));
            None
        }
    }
}

/// The effect with its bindings resolved; a typed argument mismatch in an embedded operator call is the
/// effect's failure, as it is when the JS executor throws.
fn resolve_effect_result(result: EffectResult, ctx: &BindingContext) -> EffectResult {
    let mut result = result;
    match resolve_effect_bindings(result.effect.clone(), ctx) {
        Ok(effect) => {
            result.effect = effect;
            result
        }
        Err(fault) => EffectResult::failed(result.effect, &OrbitalError::Eval(fault).to_string()),
    }
}

fn resolve_key(key: &str, ctx: &BindingContext) -> String {
    ctx.resolve_value(&Value::String(key.to_string()))
        .into_owned()
        .to_string_value()
}

#[cfg(test)]
mod navigate_options_tests {
    use super::*;

    /// §157 `navigate-route-params`: the third `navigate` argument's `crumb` must
    /// resolve through the same `BindingContext` as `params`, not stay the
    /// unresolved `@payload.title` literal.
    #[test]
    fn navigate_carries_crumb_options() {
        let mut payload = ValueMap::new();
        payload.insert("title".to_string(), Value::String("My Note".to_string()));
        let ctx = BindingContextBuilder::new()
            .payload(Value::Object(payload))
            .build();

        let effect = Effect::Navigate {
            path: "/notes/:id".to_string(),
            params: None,
            options: Some(crate::effects::NavigateOptions {
                crumb: Some("@payload.title".to_string()),
            }),
        };

        let resolved = resolve_effect_bindings(effect, &ctx).expect("resolves");
        let Effect::Navigate { options, .. } = resolved else {
            panic!("expected Navigate effect");
        };
        assert_eq!(
            options.and_then(|o| o.crumb),
            Some("My Note".to_string()),
            "the crumb must resolve from @payload.title, not stay the literal token"
        );
    }
}

#[cfg(test)]
mod call_key_tests {
    use super::*;

    fn ctx_with_batch() -> BindingContext {
        let mut entity = ValueMap::new();
        entity.insert("batchId".to_string(), Value::String("b-7".to_string()));
        BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .build()
    }

    #[test]
    fn call_service_key_resolves_from_the_entity_binding() {
        let effect = Effect::CallService {
            service: "llm".into(),
            method: "classifyMany".into(),
            args: Value::Null,
            key: Some("@entity.batchId".into()),
            emit: None,
        };
        let Ok(Effect::CallService { key, .. }) =
            resolve_effect_bindings(effect, &ctx_with_batch())
        else {
            panic!("expected CallService");
        };
        assert_eq!(key.as_deref(), Some("b-7"));
    }

    #[test]
    fn cancel_call_key_resolves_from_the_entity_binding() {
        let effect = Effect::CancelCall {
            key: "@entity.batchId".into(),
        };
        let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
        else {
            panic!("expected CancelCall");
        };
        assert_eq!(key, "b-7");
    }

    #[test]
    fn a_literal_key_passes_through_unchanged() {
        let effect = Effect::CancelCall {
            key: "fixed".into(),
        };
        let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
        else {
            panic!("expected CancelCall");
        };
        assert_eq!(key, "fixed");
    }
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
            .filter_map(crate::schema::PageRef::as_inline)
        {
            let Some(label) = &page.label else { continue };
            if page.path.contains(':') || !seen.insert(page.path.clone()) {
                continue;
            }
            let mut item = ValueMap::from([
                ("href".to_string(), Value::String(page.path.clone())),
                ("label".to_string(), Value::String(label.clone())),
            ]);
            if let Some(icon) = &page.icon {
                item.insert("icon".to_string(), Value::String(icon.clone()));
            }
            if let Some(roles) = &page.roles {
                item.insert(
                    "roles".to_string(),
                    Value::Array(roles.iter().cloned().map(Value::String).collect()),
                );
            }
            items.push(Value::Object(item));
        }
    }
    items
}

/// The app-wide `@pages` after registering another program: earlier entries first, a later entry whose
/// path is already listed dropped. `Value::Null` while no program declares one.
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
    if items.is_empty() {
        Value::Null
    } else {
        Value::Array(items)
    }
}

/// Whether a failed effect is one the JS executor throws, the failure a response reports as `effect-failed`.
/// A `set` that cannot evaluate, and a `call-service` or substrate operator with no declared `emit.failure`
/// route, are; a `persist` refusal (policy, required column) and a `fetch` miss are handled outcomes. A thrown
/// failure stops its transition: no later effect runs and the step reports no client effect or emit.
fn is_thrown_failure(result: &EffectResult) -> bool {
    if !result.is_failed() {
        return false;
    }
    let routed = |emit: &Option<FetchEmit>| emit.as_ref().is_some_and(|e| e.failure.is_some());
    match &result.effect {
        Effect::Set { .. }
        | Effect::CancelCall { .. }
        | Effect::RenderUi { .. }
        | Effect::Emit { .. } => true,
        Effect::CallService { emit, .. } | Effect::Custom { emit, .. } => !routed(emit),
        _ => false,
    }
}

/// A `call-service`'s declared completion — JS `EffectExecutor.ts`: `emit.success` carries the
/// service's result as the payload, `emit.failure` `{ error }`. A delegated or no-op call fires nothing.
fn call_service_completion_emit(
    emit: &FetchEmit,
    result: &EffectResult,
    verdict: Option<CallVerdict>,
    key: Option<&str>,
) -> Option<EffectResult> {
    let (event, payload) = match (verdict, &result.outcome) {
        (Some(CallVerdict::Deferred), _) => return None,
        (Some(CallVerdict::Cancelled), _) => {
            let key = key?;
            (
                emit.cancelled.clone()?,
                Value::Object(ValueMap::from([(
                    "key".to_string(),
                    Value::String(key.to_string()),
                )])),
            )
        }
        (None, EffectOutcome::Executed) => (
            emit.success.clone()?,
            result.return_value.clone().unwrap_or(Value::Null),
        ),
        (None, EffectOutcome::Failed) => (emit.failure.clone()?, error_payload(result, None)),
        (None, EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_)) => return None,
    };
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

/// A substrate op's declared completion — JS `runSubstrate`/`runProgramEffectArm`: `emit.success`
/// carries `{ result }`, `emit.failure` `{ error }` plus `errors` (a program's validator issues).
fn substrate_completion_emit(emit: &FetchEmit, result: &EffectResult) -> Option<EffectResult> {
    let (event, payload) = match &result.outcome {
        EffectOutcome::Executed => {
            let mut payload = ValueMap::new();
            payload.insert(
                "result".to_string(),
                result.return_value.clone().unwrap_or(Value::Null),
            );
            (emit.success.clone()?, Value::Object(payload))
        }
        EffectOutcome::Failed => (
            emit.failure.clone()?,
            error_payload(result, result.return_value.clone()),
        ),
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) => return None,
    };
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

fn error_payload(result: &EffectResult, errors: Option<Value>) -> Value {
    let mut payload = ValueMap::new();
    payload.insert(
        "error".to_string(),
        Value::String(result.error.clone().unwrap_or_default()),
    );
    if let Some(errors) = errors {
        payload.insert("errors".to_string(), errors);
    }
    Value::Object(payload)
}

fn fetch_completion_emit(emit: &FetchEmit, result: &EffectResult) -> Option<EffectResult> {
    let (event, payload) = match &result.outcome {
        EffectOutcome::Executed => {
            let event = emit.success.clone()?;
            let rows = result
                .return_value
                .clone()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            // The TS wire (`EffectExecutor.ts`): a by-id hit's `data` is the row itself.
            let by_id = matches!(&result.effect, Effect::Fetch { id: Some(_), .. });
            let data = match rows {
                Value::Array(mut r) if by_id && r.len() == 1 => r.remove(0),
                other => other,
            };
            let mut payload = ValueMap::new();
            payload.insert("data".to_string(), data);
            payload.insert(
                "totalCount".to_string(),
                Value::Number(result.total_count.unwrap_or_default() as f64),
            );
            (event, Value::Object(payload))
        }
        EffectOutcome::Failed => (emit.failure.clone()?, Value::Object(ValueMap::new())),
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) => return None,
    };
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

/// A `persist`'s declared completion: `emit.failure` with `{ error }` when it failed
/// (access denial, missing required column), `emit.success` when it ran, carrying the
/// stored row for create/update and `{ id, deleted: true }` for delete (the JS runtime's
/// payloads, `emit_contract::success_payload`). A delegated or no-op persist fires
/// nothing here; the leg that runs it does.
fn persist_completion_emit(
    emit: &FetchEmit,
    effect: &Effect,
    result: &EffectResult,
) -> Option<EffectResult> {
    let (event, payload) = match result.outcome {
        EffectOutcome::Failed => {
            let mut payload = ValueMap::new();
            if let Some(err) = &result.error {
                payload.insert("error".to_string(), Value::String(err.clone()));
            }
            (emit.failure.clone()?, Value::Object(payload))
        }
        EffectOutcome::Executed => {
            let event = emit.success.clone()?;
            let payload = match effect {
                Effect::Persist {
                    operation: PersistOperation::Delete,
                    id,
                    ..
                } => {
                    let mut payload = ValueMap::new();
                    payload.insert(
                        "id".to_string(),
                        id.clone().map_or(Value::Null, Value::String),
                    );
                    payload.insert("deleted".to_string(), Value::Bool(true));
                    Value::Object(payload)
                }
                _ => result.return_value.clone().unwrap_or(Value::Null),
            };
            (event, payload)
        }
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) => return None,
    };
    let envelope = EffectResult::executed(Effect::Emit { event, payload });
    Some(match result.outcome {
        EffectOutcome::Executed => envelope.as_persist_success(),
        _ => envelope,
    })
}

#[cfg(test)]
mod merge_entity_tests {
    use super::*;
    

    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = ValueMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    /// Non-empty incoming payload always wins (and is what gets persisted by the
    /// caller) — even when a prior payload exists.
    /// Empty incoming payload falls back to the last meaningful payload — the
    /// render-ui re-render case (no-op event / tick) that used to revert
    /// `@payload.data` to a literal token.
    /// With nothing persisted, an empty incoming payload stays empty (no
    /// fabrication — there is genuinely nothing to bind).
    /// Payloads accumulate across events: a fetch/emit carries `{data}`, a later
    /// gesture carries `{row}`; a view referencing `@payload.data` AND `@payload.row`
    /// on the same frame must see BOTH. Replacement semantics would drop `data` when
    /// the gesture fires.
    /// Read-modify-write: Set fields override, untouched base fields survive. This
    /// is the field-retention fix — persisting only the Set-touched partial would
    /// drop `filterKind`/`current*`/etc. and break the next re-render.
    /// Drift D-2: an empty `@payload` (no event payload, nothing remembered) reads `{}` on every
    /// path, as in the JS runtime, never `null`.
    #[test]
    fn merge_entity_preserves_untouched_and_overrides_set() {
        let base = obj(&[
            ("id", Value::String("singleton".to_string())),
            ("filterKind", Value::String("all".to_string())),
            ("title", Value::String("Activity".to_string())),
        ]);
        let set = obj(&[("filterKind", Value::String("created".to_string()))]);
        let merged = merge_entity(&base, &set);
        let m = merged.as_object().unwrap();
        assert_eq!(m["filterKind"], Value::String("created".to_string())); // overridden
        assert_eq!(m["title"], Value::String("Activity".to_string())); // preserved
        assert_eq!(m["id"], Value::String("singleton".to_string())); // preserved
    }

    #[test]
    fn merge_entity_empty_set_returns_base() {
        let base = obj(&[("a", Value::Number(1.0))]);
        assert_eq!(merge_entity(&base, &Value::Object(ValueMap::new())), base);
        assert_eq!(merge_entity(&base, &Value::Null), base);
    }

    #[test]
    fn merge_entity_first_write_uses_set_when_no_base() {
        let set = obj(&[("x", Value::Bool(true))]);
        assert_eq!(merge_entity(&Value::Null, &set), set);
    }
}

#[cfg(test)]
mod config_forward_tests {
    use super::*;

    /// Build a minimal `TraitDefinition` with a `config` map of `key → default
    /// string` from JSON — the kernel only reads `config[k].default` for the
    /// forward chain, so an empty state machine suffices.
    fn trait_with_config(name: &str, cfg: &[(&str, &str)]) -> TraitDefinition {
        let config: serde_json::Map<String, serde_json::Value> = cfg
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    serde_json::json!({ "type": "string", "default": v }),
                )
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "name": name,
            "stateMachine": { "states": [], "transitions": [] },
            "config": config,
        }))
        .expect("trait fixture")
    }

    fn traits(defs: Vec<TraitDefinition>) -> HashMap<String, TraitDefinition> {
        defs.into_iter().map(|t| (t.name.clone(), t)).collect()
    }

    /// A peer's `@config.<key>` forward chains to the host trait's knob default.
    #[test]
    fn wrapper_forward_chains_to_host_default() {
        let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
        let host = trait_with_config("ExportMenu", &[("buttonLabel", "Export")]);
        let all = traits(vec![button.clone(), host]);
        let cfg = config_defaults_value(&button, &all);
        let o = cfg.as_object().unwrap();
        // The render-ui reads `@config.buttonLabel` (the collapsed forward token),
        // so the host knob must be exposed under its own name…
        assert_eq!(
            o.get("buttonLabel"),
            Some(&Value::String("Export".to_string()))
        );
        // …and the forwarding field is collapsed too.
        assert_eq!(o.get("label"), Some(&Value::String("Export".to_string())));
    }

    /// A host call-site override of the knob flows through to the child — the
    /// override is folded into the host's `default` by the inline phase, so the
    /// chain resolves to it, not the atom's own default.
    #[test]
    fn wrapper_forward_picks_up_host_override() {
        let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
        let host = trait_with_config("ExportMenu", &[("buttonLabel", "Download CSV")]);
        let all = traits(vec![button.clone(), host]);
        let cfg = config_defaults_value(&button, &all);
        assert_eq!(
            cfg.as_object().unwrap().get("buttonLabel"),
            Some(&Value::String("Download CSV".to_string())),
        );
    }

    /// No host declares the knob → the forward is left live for the binding
    /// validator to flag (no fabrication).
    #[test]
    fn wrapper_forward_unresolved_when_no_host_knob() {
        let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
        let all = traits(vec![button.clone()]);
        let cfg = config_defaults_value(&button, &all);
        assert_eq!(
            cfg.as_object().unwrap().get("label"),
            Some(&Value::String("@config.buttonLabel".to_string())),
        );
    }

    /// A self-referential forward (`X = @config.X`) terminates instead of looping.
    #[test]
    fn self_forward_terminates() {
        let t = trait_with_config("Loop", &[("x", "@config.x")]);
        let all = traits(vec![t.clone()]);
        let cfg = config_defaults_value(&t, &all);
        assert_eq!(
            cfg.as_object().unwrap().get("x"),
            Some(&Value::String("@config.x".to_string())),
        );
    }
}

/// `EmbedIndex::build` in isolation — no kernel/dispatch involved, just the
/// `trait_embed_ids` (child id) -> ancestor-chain (names) inversion.
#[cfg(test)]
mod embed_index_tests {
    use super::*;

    /// A trait fixture carrying only what `EmbedIndex::build` reads: its own
    /// `id` and (optionally) the embed-token -> child-id map a host declares.
    fn trait_def(name: &str, id: &str, embeds: &[(&str, &str)]) -> TraitDefinition {
        let mut json = serde_json::json!({
            "name": name,
            "id": id,
            "stateMachine": { "states": [], "transitions": [] },
        });
        if !embeds.is_empty() {
            let map: serde_json::Map<String, serde_json::Value> = embeds
                .iter()
                .map(|(token, child_id)| {
                    (
                        (*token).to_string(),
                        serde_json::Value::String((*child_id).to_string()),
                    )
                })
                .collect();
            json["traitEmbedIds"] = serde_json::Value::Object(map);
        }
        serde_json::from_value(json).expect("trait fixture")
    }

    fn defs(list: Vec<TraitDefinition>) -> HashMap<String, TraitDefinition> {
        list.into_iter().map(|t| (t.name.clone(), t)).collect()
    }

    #[test]
    fn embed_index_inverts_trait_embed_ids() {
        let child = trait_def("Child", "child-1", &[]);
        let host = trait_def("Host", "host-1", &[("slot", "child-1")]);
        let idx = EmbedIndex::build(&defs(vec![child, host]));
        assert_eq!(idx.ancestors("Child"), &["Host".to_string()]);
        assert!(idx.ancestors("Host").is_empty());
        assert!(idx.ancestors("Unknown").is_empty());
    }

    #[test]
    fn embed_index_transitive_chain_innermost_first() {
        let child = trait_def("Child", "child-1", &[]);
        let host = trait_def("Host", "host-1", &[("slot", "child-1")]);
        let grandhost = trait_def("Grandhost", "grand-1", &[("slot2", "host-1")]);
        let idx = EmbedIndex::build(&defs(vec![child, host, grandhost]));
        assert_eq!(
            idx.ancestors("Child"),
            &["Host".to_string(), "Grandhost".to_string()],
            "innermost host first, outward to the root"
        );
        assert_eq!(idx.ancestors("Host"), &["Grandhost".to_string()]);
    }

    /// A embeds B, B embeds A — malformed input no authoring path produces, but
    /// the walk must terminate defensively rather than loop forever. Simply
    /// returning (rather than hanging the test run) is the assertion.
    #[test]
    fn embed_index_cycle_guard_terminates() {
        let a = trait_def("A", "a-1", &[("slot", "b-1")]);
        let b = trait_def("B", "b-1", &[("slot", "a-1")]);
        let idx = EmbedIndex::build(&defs(vec![a, b]));
        assert!(idx.ancestors("A").len() <= 2);
        assert!(idx.ancestors("B").len() <= 2);
    }
}

/// §158 reproduction + regression: the kernel-side twin of the JS runtime's
/// `TraitScopeProvider` embed-emit bubble (R-EMBEDDED-EMIT-BUBBLE), verified
/// through real `RuntimeKernel::dispatch` calls (not just `EmbedIndex` in
/// isolation).
#[cfg(test)]
mod embed_bubble_dispatch_tests {
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;
    use crate::schema::ListenSource;

    /// Mirrors the real std-notes reproduction named in the §158 gap:
    /// `InlineButtonRender3` embedded by `NoteCatalog` (which declares
    /// `emits CREATE_DRAFT`); `NotePersistor` `listens CREATE_DRAFT` from
    /// source `NoteCatalog`. Generic names so the fixture stands on its own,
    /// same shape as the real corpus.
    fn notes_like_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    {
                        "name": "InlineButtonRender3",
                        "id": "child-btn",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": []
                        }
                    },
                    {
                        "name": "NoteCatalog",
                        "id": "host-catalog",
                        "traitEmbedIds": { "createButton": "child-btn" },
                        "emits": [{ "event": "CREATE_DRAFT" }],
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                        }
                    },
                    {
                        "name": "NotePersistor",
                        "linkedEntity": "Note",
                        "listens": [{
                            "event": "CREATE_DRAFT",
                            "triggers": "DO_CREATE_DRAFT",
                            "source": { "kind": "trait", "trait": "NoteCatalog" }
                        }],
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true },
                                { "name": "created" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "created", "event": "DO_CREATE_DRAFT" }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    /// A Child embedded by a Host that has its own scoped listener firing off
    /// the SAME seed event, plus two listeners distinguishing "sourced from
    /// the emitting trait itself" vs "sourced from its host" — used to prove
    /// an `Effect::Emit` produced inside a transition does NOT bubble past the
    /// trait that produced it, only the seed event does.
    fn internal_emit_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    {
                        "name": "Child",
                        "id": "child-1",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "SEED",
                                  "effects": [["emit", "INTERNAL", {}]] }
                            ]
                        }
                    },
                    {
                        "name": "Host",
                        "id": "host-1",
                        "traitEmbedIds": { "slot": "child-1" },
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                        }
                    },
                    {
                        "name": "ListenerOnChild",
                        "linkedEntity": "Note",
                        "listens": [{
                            "event": "INTERNAL", "triggers": "FIRED_FROM_CHILD",
                            "source": { "kind": "trait", "trait": "Child" }
                        }],
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true }, { "name": "fired" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "fired", "event": "FIRED_FROM_CHILD" }
                            ]
                        }
                    },
                    {
                        "name": "ListenerOnHost",
                        "linkedEntity": "Note",
                        "listens": [{
                            "event": "INTERNAL", "triggers": "FIRED_FROM_HOST",
                            "source": { "kind": "trait", "trait": "Host" }
                        }],
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true }, { "name": "fired" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "fired", "event": "FIRED_FROM_HOST" }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    fn kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    /// The §158 reproduction, pre-fix: `dispatch` only fanned out from the
    /// seed trait itself, so `CREATE_DRAFT` sent to the embedded
    /// `InlineButtonRender3` never reached `NotePersistor` (source
    /// `NoteCatalog`) — this assertion fails on the pre-bubble `dispatch`.
    #[test]
    fn dispatch_bubbles_embedded_emit_to_host_listener() {
        let kernel = kernel_with(notes_like_schema());
        let outcomes = kernel
            .dispatch(
                "InlineButtonRender3",
                None,
                "CREATE_DRAFT",
                Value::Object(Default::default()),
            )
            .unwrap();
        let persistor = outcomes
            .iter()
            .find(|(t, _)| t == "NotePersistor")
            .expect("NotePersistor (listens CREATE_DRAFT from NoteCatalog) must fire when the embedded button dispatches CREATE_DRAFT");
        assert_eq!(persistor.1.new_state, "created");
    }

    /// A trait with no ancestors must dispatch byte-identically to the
    /// pre-bubble single-scope path: one outcome, the target trait only.
    #[test]
    fn dispatch_unembedded_trait_is_single_scope() {
        let kernel = kernel_with(notes_like_schema());
        let outcomes = kernel
            .dispatch("NoteCatalog", None, "INIT", Value::Null)
            .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "NoteCatalog");
    }

    #[test]
    fn dispatch_does_not_bubble_internal_emit_effects() {
        let kernel = kernel_with(internal_emit_schema());
        let outcomes = kernel
            .dispatch("Child", None, "SEED", Value::Object(Default::default()))
            .unwrap();
        assert!(
            outcomes
                .iter()
                .any(|(t, r)| t == "ListenerOnChild" && r.new_state == "fired"),
            "the emitting trait's own scope must still receive its Effect::Emit: {:?}",
            outcomes
                .iter()
                .map(|(t, r)| (t.clone(), r.new_state.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !outcomes
                .iter()
                .any(|(t, r)| t == "ListenerOnHost" && r.new_state == "fired"),
            "an internal Effect::Emit must not bubble past its emitting trait's own scope: {:?}",
            outcomes
                .iter()
                .map(|(t, r)| (t.clone(), r.new_state.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// The Rust twin of `audit_listens` "via host-transition": for every
    /// declared `listens` whose source S declares (or whose descendant
    /// declares, transitively — but here checked at S itself, the common
    /// authoring shape where the composing host carries the `emits`
    /// metadata for a primitive it embeds) `emits` of that event, dispatching
    /// the event from EVERY trait embedded (directly or transitively) under S
    /// must realize the listener in `dispatch`'s outcomes.
    #[test]
    fn embed_bubble_realizes_every_declared_host_transition_listener() {
        let schema = notes_like_schema();
        let kernel = kernel_with(schema.clone());

        let trait_definitions: HashMap<String, TraitDefinition> = schema.orbitals[0]
            .traits
            .iter()
            .filter_map(|t| match t {
                TraitReference::Inline(def) => Some((def.name.clone(), def.clone())),
                _ => None,
            })
            .collect();
        let embed_index = EmbedIndex::build(&trait_definitions);

        let mut checked = 0;
        for listener_def in trait_definitions.values() {
            let Some(listens) = &listener_def.listens else {
                continue;
            };
            for l in listens {
                let Some(ListenSource::Trait {
                    trait_name: source, ..
                }) = &l.source
                else {
                    continue;
                };
                let source_emits_event = trait_definitions
                    .get(source)
                    .and_then(|d| d.emits.as_ref())
                    .is_some_and(|es| es.iter().any(|e| e.event == l.event));
                if !source_emits_event {
                    continue;
                }
                for candidate in trait_definitions.keys() {
                    if !embed_index.ancestors(candidate).iter().any(|a| a == source) {
                        continue;
                    }
                    checked += 1;
                    let outcomes = kernel
                        .dispatch(candidate, None, &l.event, Value::Null)
                        .unwrap();
                    assert!(
                        outcomes.iter().any(|(t, _)| t == &listener_def.name),
                        "{} (listens {} from {source}) must fire when embedded emitter {candidate} dispatches {}",
                        listener_def.name, l.event, l.event
                    );
                }
            }
        }
        assert!(
            checked > 0,
            "fixture produced no host-transition-listener pairs to check"
        );
    }

    /// `Child` is embedded by `Host` (directly, or through `Mid` when `via_mid`); `Host` handles
    /// `REQUEST` from `idle` AND from `open`, so a second delivery is a real second transition
    /// (the seed run moves `idle -> open`, which changes the cascade's visit key). `route` is the
    /// host's `listens` array; `Other` is an unembedded sibling that can emit too.
    fn double_delivery_schema(route: serde_json::Value, via_mid: bool) -> OrbitalSchema {
        let host_embed = if via_mid { "mid-1" } else { "child-1" };
        let mut traits = vec![
            serde_json::json!({
                "name": "Child", "id": "child-1",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
            }),
            serde_json::json!({
                "name": "Other", "id": "other-1",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
            }),
            serde_json::json!({
                "name": "Host", "id": "host-1",
                "traitEmbedIds": { "slot": host_embed },
                "listens": route,
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "open" }],
                    "transitions": [
                        { "from": "idle", "to": "open", "event": "REQUEST" },
                        { "from": "open", "to": "open", "event": "REQUEST" },
                        { "from": "open", "to": "open", "event": "ADMIT" }
                    ]
                }
            }),
        ];
        if via_mid {
            traits.push(serde_json::json!({
                "name": "Mid", "id": "mid-1",
                "traitEmbedIds": { "slot": "child-1" },
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] }
            }));
        }
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": traits
            }]
        }))
        .unwrap()
    }

    fn route(source: &str, event: &str, triggers: &str) -> serde_json::Value {
        serde_json::json!([{
            "event": event, "triggers": triggers,
            "source": { "kind": "trait", "trait": source }
        }])
    }

    fn deliveries(outcomes: &[(String, ProcessEventResult)], trait_name: &str) -> usize {
        outcomes.iter().filter(|(t, _)| t == trait_name).count()
    }

    /// G-RUNTIME-053: the embedded child's action reaches `Host` as its own scope on the embed
    /// chain AND through `Host`'s same-name listen route; one physical event must run `Host` once.
    #[test]
    fn same_name_route_from_embedded_child_delivers_once() {
        let kernel = kernel_with(double_delivery_schema(
            route("Child", "REQUEST", "REQUEST"),
            false,
        ));
        let outcomes = kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
        let host = outcomes.iter().find(|(t, _)| t == "Host").unwrap();
        assert_eq!(host.1.new_state, "open");
    }

    #[test]
    fn same_name_route_through_a_transitive_embed_delivers_once() {
        let kernel = kernel_with(double_delivery_schema(
            route("Child", "REQUEST", "REQUEST"),
            true,
        ));
        let outcomes = kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
    }

    /// Control: no route, the host transition alone is one delivery.
    #[test]
    fn embedded_child_without_route_delivers_once() {
        let kernel = kernel_with(double_delivery_schema(serde_json::json!([]), false));
        let outcomes = kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 1);
    }

    /// Control: a route from a trait the host does NOT embed is the only path, so it must keep firing.
    #[test]
    fn route_from_unembedded_source_still_delivers() {
        let kernel = kernel_with(double_delivery_schema(
            route("Other", "REQUEST", "REQUEST"),
            false,
        ));
        let outcomes = kernel
            .dispatch("Other", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 1, "{outcomes:?}");
    }

    /// Control: a route to a DIFFERENT trigger name is a distinct delivery, not a duplicate.
    #[test]
    fn differently_named_route_from_embedded_child_is_a_second_delivery() {
        let kernel = kernel_with(double_delivery_schema(
            route("Child", "REQUEST", "ADMIT"),
            false,
        ));
        let outcomes = kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 2, "{outcomes:?}");
    }

    /// Edge: the dedup is per physical event — a second dispatch delivers once more.
    #[test]
    fn dedup_does_not_leak_across_dispatches() {
        let kernel = kernel_with(double_delivery_schema(
            route("Child", "REQUEST", "REQUEST"),
            false,
        ));
        kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        let second = kernel
            .dispatch("Child", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&second, "Host"), 1);
    }

    /// Edge: a dispatch seeded on the host itself is unaffected by a route from its own child.
    #[test]
    fn host_seeded_dispatch_delivers_once() {
        let kernel = kernel_with(double_delivery_schema(
            route("Child", "REQUEST", "REQUEST"),
            false,
        ));
        let outcomes = kernel
            .dispatch("Host", None, "REQUEST", Value::Null)
            .unwrap();
        assert_eq!(deliveries(&outcomes, "Host"), 1);
    }
}

/// §B3 client role: `dispatch_with_server_leg` / `snapshot_trait` /
/// `restore_trait` / `apply_server_response` / `already_delivered_from`.
#[cfg(test)]
mod client_dispatch_tests {
    use super::*;
    use crate::effects::{BusEventSource, ClientEffect, EmittedEvent};
    use crate::runtime::entity_source::InMemoryEntityCache;
    use crate::runtime::server_leg::ServerLegCollector;

    /// `Persistor` (PersistedAwaited: not `local`, linked entity `Persistent`)
    /// transitions `idle -[CREATE]-> created` via a real `persist` effect —
    /// the server-only effect that must reach the `ServerLegCollector`.
    fn persisted_awaited_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Persistor",
                    "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "created" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "created", "event": "CREATE",
                                "effects": [
                                    ["set", "@entity.title", "Hello"],
                                    ["persist", "create", "Note", {}]
                                ]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// `LocalToggle` declares `local: true` on a Persistent (non-runtime)
    /// entity but STILL carries a `persist` effect — a schema no validator
    /// should ever let through (§7: `local` is only legal where nothing needs
    /// the server), reproduced here deliberately to prove the kernel's
    /// defensive drop holds even when the collector has something to drain.
    fn hybrid_with_stray_persist_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "LocalToggle",
                    "linkedEntity": "Note",
                    "local": true,
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "created" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "created", "event": "CREATE",
                                "effects": [["persist", "create", "Note", {}]]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// `Move` is linked to `Cursor`, a `[runtime]`-persistence entity —
    /// RuntimeOptimistic. Its `MOVE` transition is a plain `Set`, no
    /// server-only effect, so it never produces a leg either (nothing to
    /// delegate) but DOES snapshot for optimistic rollback.
    fn runtime_optimistic_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": {
                    "name": "Cursor", "persistence": "runtime",
                    "fields": [{ "name": "id", "type": "string" }]
                },
                "traits": [{
                    "name": "Move",
                    "linkedEntity": "Cursor",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "moved" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "moved", "event": "MOVE",
                                // Its own server effect makes it optimistic
                                // (a `[runtime]` trait with none is client-only).
                                "effects": [["set", "@entity.x", 5], ["call-service", "telemetry", "move", {}]]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// `RenderTrait` has NO linked entity, but its `SHOW` transition renders
    /// `@payload.title` into `main` — proves render-ui resolves live in a
    /// Client-env kernel (native shells depend on this).
    fn render_ui_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "RenderTrait",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "shown" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "shown", "event": "SHOW",
                                "effects": [["render-ui", "main", { "title": "@payload.title" }]]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// A listener trait, used by the `apply_server_response` tests: fires
    /// `LISTENED` when a server-emitted `PING` (source `Emitter`) is folded.
    fn server_response_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    {
                        "name": "Emitter",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                        }
                    },
                    {
                        "name": "Listener",
                        "linkedEntity": "Note",
                        "listens": [{
                            "event": "PING", "triggers": "PONG",
                            "source": { "kind": "trait", "trait": "Emitter" }
                        }],
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true },
                                { "name": "ponged" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "ponged", "event": "PONG" }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    fn client_kernel_with(schema: OrbitalSchema) -> (RuntimeKernel, Arc<ServerLegCollector>) {
        let collector = Arc::new(ServerLegCollector::new());
        let config = KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector.clone() as Arc<dyn crate::effects::EffectDelegate>),
            ..KernelConfig::default()
        };
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Client,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            config,
        );
        kernel.register_schema(schema).unwrap();
        (kernel, collector)
    }

    fn server_kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    #[test]
    fn client_env_hybrid_local_trait_never_produces_server_leg() {
        let (kernel, _collector) = client_kernel_with(hybrid_with_stray_persist_schema());
        let dispatch = kernel
            .dispatch_with_server_leg("App", "LocalToggle", None, "CREATE", Value::Null, None)
            .unwrap();
        assert_eq!(dispatch.mode, DispatchMode::HybridClientOnly);
        assert!(
            dispatch.server_legs.is_empty(),
            "a HybridClientOnly trait must never produce a server leg, even when its \
             (deliberately malformed) schema delegates a persist effect"
        );
    }

    #[test]
    fn client_env_delegates_persist_and_collects_server_leg() {
        let (kernel, _collector) = client_kernel_with(persisted_awaited_schema());
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Persistor", None, "CREATE", Value::Null, None)
            .unwrap();
        assert_eq!(dispatch.mode, DispatchMode::PersistedAwaited);
        assert_eq!(dispatch.server_legs.len(), 1);
        let leg = dispatch.server_legs[0].clone();
        assert_eq!(leg.event, "CREATE");
        assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
        let traits = leg.traits.expect("executed transitions");
        assert!(traits
            .iter()
            .any(|t| t.r#trait == "Persistor" && t.from == "idle"));
        // No row before CREATE: the server replays from its own, nothing carried.
        assert!(leg.entity_by_trait.is_none());
    }

    #[test]
    fn a_cascade_leg_names_the_event_each_trait_fired_on() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Composer", "local": true, "linkedEntity": "Note", "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [{ "from": "ready", "to": "ready", "event": "SEND", "effects": [["emit", "SAVE", {}]] }] } },
                    { "name": "Persistor", "linkedEntity": "Note",
                      "listens": [{ "event": "SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Composer" } }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE", "effects": [["persist", "create", "Note", {}]] }] } }
                ]
            }]
        }))
        .unwrap();
        let (kernel, _collector) = client_kernel_with(schema);
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
            .unwrap();
        let leg = dispatch
            .server_legs
            .first()
            .expect("the persistor's write owes a leg");
        assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
        assert_eq!(leg.event, "DO_CREATE");
        let traits = leg.traits.clone().expect("executed transitions");
        let event_of = |name: &str| {
            traits
                .iter()
                .find(|t| t.r#trait == name)
                .and_then(|t| t.event.clone())
        };
        assert_eq!(event_of("Composer").as_deref(), Some("SEND"));
        assert_eq!(event_of("Persistor").as_deref(), Some("DO_CREATE"));
    }

    #[test]
    fn a_posted_dispatch_marks_each_non_local_trait_it_fired_as_awaiting() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Composer", "local": true, "linkedEntity": "Note", "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [{ "from": "ready", "to": "ready", "event": "SEND", "effects": [["emit", "SAVE", {}]] }] } },
                    { "name": "Persistor", "linkedEntity": "Note",
                      "listens": [{ "event": "SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Composer" } }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "saved" }],
                        "transitions": [{ "from": "idle", "to": "saved", "event": "DO_CREATE", "effects": [["persist", "create", "Note", {}]] }] } }
                ]
            }]
        }))
        .unwrap();
        let (kernel, _collector) = client_kernel_with(schema);
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
            .unwrap();
        assert_eq!(
            dispatch.awaiting,
            vec![AwaitingTrait {
                trait_name: "Persistor".into(),
                event: "DO_CREATE".into(),
                from: "idle".into(),
            }],
            "the local Composer never awaits; the Persistor awaits from the state it left"
        );
    }

    #[test]
    fn control_a_local_dispatch_with_no_leg_awaits_nothing() {
        let (kernel, _collector) = client_kernel_with(hybrid_with_stray_persist_schema());
        let dispatch = kernel
            .dispatch_with_server_leg("App", "LocalToggle", None, "CREATE", Value::Null, None)
            .unwrap();
        assert!(dispatch.awaiting.is_empty());
    }

    #[test]
    fn control_a_trait_entry_without_event_still_deserializes() {
        let entry: TraitFrom =
            serde_json::from_value(serde_json::json!({ "trait": "A", "from": "idle" })).unwrap();
        assert_eq!(entry.event, None);
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({ "trait": "A", "from": "idle" })
        );
    }

    #[test]
    fn client_env_runtime_optimistic_snapshot_and_restore() {
        let (kernel, _collector) = client_kernel_with(runtime_optimistic_schema());
        kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
        assert_eq!(
            kernel
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "idle"
        );

        let dispatch = kernel
            .dispatch_with_server_leg("App", "Move", None, "MOVE", Value::Null, None)
            .unwrap();
        assert_eq!(dispatch.mode, DispatchMode::RuntimeOptimistic);
        let snapshot = dispatch
            .snapshot
            .expect("RuntimeOptimistic dispatch must snapshot");
        assert_eq!(
            snapshot.state, "idle",
            "snapshot captured BEFORE the dispatch committed"
        );
        assert!(
            snapshot.entity.is_null(),
            "no entity existed yet before MOVE"
        );
        assert!(
            !dispatch.server_legs.is_empty(),
            "MOVE's own call-service is the server work that makes it optimistic"
        );

        assert_eq!(
            kernel
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "moved"
        );
        let after = kernel.snapshot_trait("Move", None).entity;
        assert_eq!(
            after.as_object().unwrap().get("x"),
            Some(&Value::Number(5.0))
        );

        kernel.restore_trait("Move", None, snapshot);
        assert_eq!(
            kernel
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "idle"
        );
        assert!(
            kernel.snapshot_trait("Move", None).entity.is_null(),
            "rollback must leave the frame absent, matching the pre-dispatch snapshot"
        );
    }

    #[test]
    fn render_ui_runs_locally_in_client_env() {
        let (kernel, _collector) = client_kernel_with(render_ui_schema());
        let mut payload = ValueMap::new();
        payload.insert("title".to_string(), Value::String("Hello".to_string()));
        let outcomes = kernel
            .dispatch("RenderTrait", None, "SHOW", Value::Object(payload))
            .unwrap();
        let response = EventResponse::from_process_results(&outcomes, "RenderTrait", "SHOW");
        let effects = response
            .client_effects
            .expect("render-ui must surface as a client effect");
        let render = effects
            .iter()
            .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
            .expect("a render-ui client effect");
        let ClientEffect::RenderUi { config, .. } = render else {
            unreachable!()
        };
        let title = config
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|o| o.get("title"))
            .cloned();
        assert_eq!(
            title,
            Some(Value::String("Hello".to_string())),
            "the binding must be RESOLVED (not the literal '@payload.title' token), \
             even though this kernel is Client-env with no render handler configured"
        );
    }

    /// App-level `theme: "clay-light"`, no orbital-level override — the
    /// `std-notes` shape the live `orb dev` gate checks. `ThemeTrait`'s `SHOW`
    /// transition renders the bare sigil directly, same as an atom's
    /// `data-theme: @currentTheme` binding (`std-app-layout.lolo`).
    fn render_ui_theme_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "theme": "clay-light",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "ThemeTrait",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "shown" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "shown", "event": "SHOW",
                                "effects": [["render-ui", "main", { "data-theme": "@currentTheme" }]]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn current_theme_binding_resolves_in_render_ui() {
        let kernel = server_kernel_with(render_ui_theme_schema());
        let outcomes = kernel
            .dispatch("ThemeTrait", None, "SHOW", Value::Null)
            .unwrap();
        let response = EventResponse::from_process_results(&outcomes, "ThemeTrait", "SHOW");
        let effects = response
            .client_effects
            .expect("render-ui must surface as a client effect");
        let render = effects
            .iter()
            .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
            .expect("a render-ui client effect");
        let ClientEffect::RenderUi { config, .. } = render else {
            unreachable!()
        };
        let theme = config
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|o| o.get("data-theme"))
            .cloned();
        assert_eq!(
            theme,
            Some(Value::String("clay-light".to_string())),
            "the @currentTheme sigil must resolve to the declared app-level theme key, \
             not round-trip as the literal '@currentTheme' token"
        );
    }

    fn pages_schema(extra_shell_pages: serde_json::Value) -> OrbitalSchema {
        let mut shell_pages = vec![
            serde_json::json!({ "name": "HomePage", "path": "/", "traits": [{ "ref": "Layout" }] }),
            serde_json::json!({ "name": "DetailPage", "path": "/items/:id", "label": "Detail", "traits": [] }),
        ];
        shell_pages.extend(extra_shell_pages.as_array().cloned().unwrap_or_default());
        serde_json::from_value(serde_json::json!({
            "name": "SigilApp",
            "orbitals": [
                {
                    "name": "ShellOrbital",
                    "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                    "traits": [{
                        "name": "Layout",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                                "effects": [["render-ui", "main", { "type": "stack", "items": "@pages" }]] }]
                        }
                    }],
                    "pages": shell_pages
                },
                {
                    "name": "ItemsOrbital",
                    "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                    "traits": [],
                    "pages": [
                        { "name": "ItemsPage", "path": "/items", "label": "Items", "icon": "list", "roles": ["admin", "staff"], "traits": [] },
                        { "name": "DuplicatePage", "path": "/items", "label": "Items again", "traits": [] }
                    ]
                }
            ]
        }))
        .unwrap()
    }

    fn rendered_items(schema: OrbitalSchema) -> Option<Value> {
        let kernel = server_kernel_with(schema);
        let outcomes = kernel
            .dispatch("Layout", None, "INIT", Value::Null)
            .unwrap();
        let response = EventResponse::from_process_results(&outcomes, "Layout", "INIT");
        response.client_effects?.iter().find_map(|e| match e {
            ClientEffect::RenderUi { config, .. } => config
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|o| o.get("items"))
                .cloned(),
            _ => None,
        })
    }

    #[test]
    fn pages_binding_resolves_to_the_app_wide_root_nav_entries() {
        let items = rendered_items(pages_schema(serde_json::json!([])));
        let expected: Value = serde_json::from_value(serde_json::json!([
            { "href": "/items", "label": "Items", "icon": "list", "roles": ["admin", "staff"] }
        ]))
        .unwrap();
        assert_eq!(items, Some(expected));
    }

    #[test]
    fn pages_binding_lists_entries_in_orbital_order_without_duplicates_params_or_unlabeled_pages() {
        let items = rendered_items(pages_schema(serde_json::json!([
            { "name": "AboutPage", "path": "/about", "label": "About", "traits": [] }
        ])));
        let hrefs: Vec<String> = items
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        e.get_path("href")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(hrefs, ["/about", "/items"]);
    }

    #[test]
    fn a_second_registered_program_adds_its_entries_after_the_first_and_skips_a_listed_path() {
        let kernel = server_kernel_with(pages_schema(serde_json::json!([])));
        let second: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "Second",
            "orbitals": [{
                "name": "ExtraOrbital",
                "entity": { "name": "Extra", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [],
                "pages": [
                    { "name": "Again", "path": "/items", "label": "Again", "traits": [] },
                    { "name": "Zeta", "path": "/zeta", "label": "Zeta", "traits": [] }
                ]
            }]
        }))
        .unwrap();
        kernel.register_schema(second).unwrap();
        let outcomes = kernel
            .dispatch("Layout", None, "INIT", Value::Null)
            .unwrap();
        let response = EventResponse::from_process_results(&outcomes, "Layout", "INIT");
        let hrefs: Vec<String> = response
            .client_effects
            .unwrap()
            .iter()
            .find_map(|e| match e {
                ClientEffect::RenderUi { config, .. } => config
                    .as_ref()
                    .and_then(Value::as_object)
                    .and_then(|o| o.get("items"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|i| {
                                i.get_path("href")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            })
                            .collect()
                    }),
                _ => None,
            })
            .unwrap();
        assert_eq!(hrefs, ["/items", "/zeta"]);
    }

    #[test]
    fn control_a_program_with_no_labeled_root_page_leaves_the_pages_token() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "NoNav",
            "orbitals": [{
                "name": "ShellOrbital",
                "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Layout",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                            "effects": [["render-ui", "main", { "type": "stack", "items": "@pages" }]] }]
                    }
                }],
                "pages": [{ "name": "HomePage", "path": "/", "traits": [] }]
            }]
        }))
        .unwrap();
        assert_eq!(
            rendered_items(schema),
            Some(Value::String("@pages".to_string()))
        );
    }

    fn i18n_schema(locales: bool) -> OrbitalSchema {
        let mut schema = serde_json::json!({
            "name": "Site",
            "orbitals": [{
                "name": "Home",
                "entity": { "name": "HomeView", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "HomePage",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{
                            "from": "idle", "to": "idle", "event": "SHOW",
                            "effects": [["render-ui", "main", {
                                "title": ["i18n/t", "site:hero.title"],
                                "count": ["i18n/t", "site:posts.count", { "n": 3 }],
                                "className": ["if", ["==", ["i18n/t", "site:cta"], ""], "hidden", ""],
                                "lang": "@locale"
                            }]]
                        }]
                    }
                }]
            }]
        });
        if locales {
            schema["locales"] = serde_json::json!(["en", "ar"]);
        }
        serde_json::from_value(schema).unwrap()
    }

    fn i18n_catalogs() -> HashMap<String, HashMap<String, String>> {
        let catalog = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        HashMap::from([
            (
                "en".to_string(),
                catalog(&[
                    ("site:hero.title", "Hello"),
                    ("site:posts.count", "{{n}} posts"),
                    ("site:cta", "Start"),
                ]),
            ),
            (
                "ar".to_string(),
                catalog(&[
                    ("site:hero.title", "مرحبا"),
                    ("site:posts.count", "{{n}} منشورات"),
                    ("site:cta", "ابدأ"),
                ]),
            ),
        ])
    }

    fn field(config: &Value, key: &str) -> Value {
        config
            .as_object()
            .and_then(|o| o.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn rendered_config(kernel: &RuntimeKernel) -> Value {
        let outcomes = kernel
            .dispatch("HomePage", None, "SHOW", Value::Null)
            .unwrap();
        let response = EventResponse::from_process_results(&outcomes, "HomePage", "SHOW");
        let effects = response.client_effects.expect("render-ui effect");
        let Some(ClientEffect::RenderUi { config, .. }) = effects
            .iter()
            .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
        else {
            panic!("no render-ui effect");
        };
        config.clone().unwrap_or(Value::Null)
    }

    #[test]
    fn i18n_t_renders_the_dispatch_viewers_locale() {
        let kernel = server_kernel_with(i18n_schema(true));
        kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
        let _locale = kernel.enter_locale(Some("ar")).unwrap();
        let config = rendered_config(&kernel);
        assert_eq!(field(&config, "title"), Value::String("مرحبا".into()));
        assert_eq!(field(&config, "count"), Value::String("3 منشورات".into()));
        assert_eq!(field(&config, "className"), Value::String("".into()));
        assert_eq!(field(&config, "lang"), Value::String("ar".into()));
    }

    #[test]
    fn control_i18n_t_without_a_viewer_locale_renders_the_first_declared() {
        let kernel = server_kernel_with(i18n_schema(true));
        kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
        let config = rendered_config(&kernel);
        assert_eq!(field(&config, "title"), Value::String("Hello".into()));
        assert_eq!(field(&config, "lang"), Value::String("en".into()));
    }

    #[test]
    fn edge_a_locale_the_program_does_not_declare_is_refused() {
        let kernel = server_kernel_with(i18n_schema(true));
        kernel.set_messages(vec!["en".into(), "ar".into()], i18n_catalogs());
        let err = kernel
            .enter_locale(Some("fr"))
            .err()
            .expect("undeclared locale");
        assert!(err.to_string().contains("\"fr\""), "{err}");
    }

    #[test]
    fn edge_a_program_without_locales_accepts_any_viewer_locale() {
        let kernel = server_kernel_with(i18n_schema(false));
        assert!(kernel.enter_locale(Some("en")).is_ok());
    }

    #[test]
    fn apply_server_response_merges_data_and_states() {
        let kernel = server_kernel_with(server_response_schema());
        let mut row = ValueMap::new();
        row.insert("id".to_string(), Value::String("n-1".to_string()));
        row.insert(
            "title".to_string(),
            Value::String("From server".to_string()),
        );
        let mut data = HashMap::new();
        data.insert("Note".to_string(), vec![Value::Object(row)]);

        let mut entity_by_trait = ValueMap::new();
        let mut listener_entity = ValueMap::new();
        listener_entity.insert("id".to_string(), Value::String("singleton".to_string()));
        listener_entity.insert("note".to_string(), Value::String("n-1".to_string()));
        entity_by_trait.insert("Listener".to_string(), Value::Object(listener_entity));

        let mut states = HashMap::new();
        states.insert("Listener".to_string(), "ponged".to_string());

        let response = EventResponse {
            success: true,
            transitioned: true,
            states,
            emitted_events: Vec::new(),
            data: Some(data),
            entity_by_trait: Some(entity_by_trait),
            ..Default::default()
        };

        kernel
            .apply_server_response(&response, &HashSet::new())
            .unwrap();

        let note = kernel.entity_source().read("Note", "n-1").unwrap();
        assert_eq!(
            note.as_object().unwrap().get("title"),
            Some(&Value::String("From server".to_string()))
        );
        let listener_frame = kernel.snapshot_trait("Listener", None).entity;
        assert_eq!(
            listener_frame.as_object().unwrap().get("note"),
            Some(&Value::String("n-1".to_string()))
        );
        assert_eq!(
            kernel
                .get_state("Listener", Some("singleton"))
                .unwrap()
                .current_state,
            "ponged"
        );
    }

    #[test]
    fn apply_server_response_dedupes_locally_delivered_echo() {
        let kernel = server_kernel_with(server_response_schema());
        let response = EventResponse {
            success: true,
            transitioned: true,
            states: HashMap::new(),
            emitted_events: vec![EmittedEvent {
                event: "PING".to_string(),
                payload: None,
                source: Some(BusEventSource {
                    orbital: Some("Main".to_string()),
                    ..BusEventSource::from_trait("Emitter")
                }),
            }],
            ..Default::default()
        };

        // Already delivered locally — must NOT re-fire the listener.
        let mut already_delivered = HashSet::new();
        already_delivered.insert(("Emitter".to_string(), "PING".to_string()));
        let outcomes = kernel
            .apply_server_response(&response, &already_delivered)
            .unwrap();
        assert!(
            !outcomes.iter().any(|(t, _)| t == "Listener"),
            "an echo of a locally-delivered event must not re-run its listener"
        );

        // Nothing delivered locally — the listener must fire.
        let outcomes = kernel
            .apply_server_response(&response, &HashSet::new())
            .unwrap();
        assert!(
            outcomes
                .iter()
                .any(|(t, r)| t == "Listener" && r.new_state == "ponged"),
            "an event not already delivered must still cascade through the local listener"
        );
    }

    #[test]
    fn apply_server_response_rolls_back_on_error() {
        let (kernel, _collector) = client_kernel_with(runtime_optimistic_schema());
        kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Move", None, "MOVE", Value::Null, None)
            .unwrap();
        let snapshot = dispatch.snapshot.expect("RuntimeOptimistic snapshot");
        assert_eq!(
            kernel
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "moved"
        );

        // Caller pattern: the (simulated) server round trip errors/disagrees →
        // roll back to the pre-dispatch snapshot.
        kernel.restore_trait("Move", None, snapshot);

        assert_eq!(
            kernel
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "idle"
        );
        assert!(kernel.entity_source().read("Cursor", "singleton").is_err());
    }

    #[test]
    fn already_delivered_from_includes_local_emits_and_seed() {
        let mut result = ProcessEventResult {
            event: None,
            executed: true,
            previous_state: "idle".to_string(),
            new_state: "created".to_string(),
            effects_executed: 1,
            effect_results: vec![EffectResult::executed(Effect::Emit {
                event: "DRAFT_CREATED".to_string(),
                payload: Value::Null,
            })],
            entity_data: Value::Null,
            linked_entity: "Note".to_string(),
            payload: Value::Null,
            entry: None,
            fired_arm: None,
            guards: Vec::new(),
            effect_runs: Vec::new(),
            cascade_dropped: Vec::new(),
            consumed_emits: Vec::new(),
            failure: None,
            callsite_payload: None,
        };
        result.previous_state = "idle".to_string();
        let outcomes = vec![("Persistor".to_string(), result)];
        let response = EventResponse::from_process_results(&outcomes, "Persistor", "CREATE");
        let dispatch = ClientDispatch {
            response,
            server_legs: vec![EventRequest {
                event: "CREATE".to_string(),
                target_trait: Some("Persistor".to_string()),
                ..Default::default()
            }],
            outcomes,
            seed: ("Persistor".to_string(), "CREATE".to_string()),
            mode: DispatchMode::PersistedAwaited,
            snapshot: None,
            awaiting: Vec::new(),
            stateful_leg: None,
        };

        let delivered = already_delivered_from(&dispatch);
        assert!(delivered.contains(&("Persistor".to_string(), "DRAFT_CREATED".to_string())));
        assert!(delivered.contains(&("Persistor".to_string(), "CREATE".to_string())));
    }

    #[test]
    fn kernel_concurrent_dispatch_and_apply_no_deadlock() {
        let kernel = Arc::new(server_kernel_with(server_response_schema()));
        kernel
            .dispatch("Emitter", None, "INIT", Value::Null)
            .unwrap();
        kernel
            .dispatch("Listener", None, "INIT", Value::Null)
            .unwrap();

        let dispatch_kernel = kernel.clone();
        let dispatcher = std::thread::spawn(move || {
            for _ in 0..200 {
                let _ = dispatch_kernel.dispatch("Emitter", None, "PING", Value::Null);
            }
        });

        let apply_kernel = kernel.clone();
        let applier = std::thread::spawn(move || {
            for _ in 0..200 {
                let response = EventResponse {
                    success: true,
                    emitted_events: vec![EmittedEvent {
                        event: "PING".to_string(),
                        payload: None,
                        source: Some(BusEventSource::from_trait("Emitter")),
                    }],
                    ..Default::default()
                };
                let _ = apply_kernel.apply_server_response(&response, &HashSet::new());
            }
        });

        dispatcher
            .join()
            .expect("dispatcher thread must not panic or deadlock");
        applier
            .join()
            .expect("applier thread must not panic or deadlock");
    }

    /// std-realtime-chat's shape: a `local` composer emits SAVE; a persistor
    /// listening to it runs the server-bound `persist create`.
    fn local_seed_downstream_persist_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Message", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "content", "type": "string" }
                ] },
                "traits": [
                    {
                        "name": "Composer",
                        "linkedEntity": "Message",
                        "local": true,
                        "stateMachine": {
                            "states": [{ "name": "ready", "isInitial": true }],
                            "transitions": [
                                { "from": "ready", "to": "ready", "event": "DRAFT",
                                  "effects": [["set", "@entity.content", "@payload.value"]] },
                                { "from": "ready", "to": "ready", "event": "SEND",
                                  "effects": [["emit", "SAVE", { "content": "@entity.content" }]] }
                            ]
                        }
                    },
                    {
                        "name": "Persistor",
                        "linkedEntity": "Message",
                        "listens": [{ "event": "SAVE", "source": { "kind": "trait", "trait": "Composer" }, "triggers": "DO_CREATE" }],
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "DO_CREATE",
                                  "effects": [["persist", "create", "Message", { "content": "@payload.content" }]] }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    /// A `local` seed never needs the server for its OWN effects, but the
    /// traits its cascade reached may; that work is posted, not dropped.
    #[test]
    fn client_env_local_seed_posts_its_cascade_server_work() {
        let (kernel, _collector) = client_kernel_with(local_seed_downstream_persist_schema());
        kernel
            .dispatch_with_server_leg(
                "App",
                "Composer",
                None,
                "DRAFT",
                Value::Object(ValueMap::from([("value".to_string(), Value::from("hi"))])),
                None,
            )
            .unwrap();
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
            .unwrap();
        assert_eq!(dispatch.mode, DispatchMode::HybridClientOnly);
        assert_eq!(dispatch.server_legs.len(), 1);
        let leg = &dispatch.server_legs[0];
        assert_eq!(leg.event, "DO_CREATE");
        assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
    }

    /// A local `Ticker` whose `beat` tick emits BEAT; `Listener` hears it; `Persistor` hears it
    /// and persists. `Ticker` renders on INIT and has `ownArm` (an own BEAT arm) when asked.
    fn tick_emit_schema(own_arm: bool) -> OrbitalSchema {
        let mut ticker_transitions = vec![serde_json::json!(
            { "from": "running", "to": "running", "event": "INIT",
              "effects": [["render-ui", "main", { "type": "typography", "content": "tick" }]] })];
        if own_arm {
            ticker_transitions.push(serde_json::json!(
                { "from": "running", "to": "running", "event": "BEAT", "effects": [] }));
        }
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "beats", "type": "number" }
                ] },
                "traits": [
                    {
                        "name": "Ticker", "linkedEntity": "Note", "local": true,
                        "stateMachine": {
                            "states": [{ "name": "running", "isInitial": true }],
                            "transitions": ticker_transitions
                        },
                        "ticks": [
                            { "name": "beat", "interval": 1000,
                              "effects": [["set", "@entity.beats", 1], ["emit", "BEAT", { "n": 1 }]] },
                            { "name": "quiet", "interval": 1000,
                              "effects": [["set", "@entity.beats", 2]] },
                            { "name": "saver", "interval": 1000,
                              "effects": [["persist", "create", "Note", { "beats": 3 }]] },
                            { "name": "never", "interval": 1000,
                              "guard": ["=", 1, 2],
                              "effects": [["emit", "BEAT", {}]] }
                        ]
                    },
                    {
                        "name": "Listener", "linkedEntity": "Note", "local": true,
                        "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                            "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }]
                        }
                    },
                    {
                        "name": "Persistor", "linkedEntity": "Note",
                        "listens": [{ "event": "BEAT", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Ticker" } }],
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE",
                                "effects": [["persist", "create", "Note", { "beats": "@payload.n" }]] }]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    fn booted_ticker(own_arm: bool) -> (RuntimeKernel, Arc<ServerLegCollector>) {
        let (kernel, collector) = client_kernel_with(tick_emit_schema(own_arm));
        kernel
            .dispatch_with_server_leg("App", "Ticker", None, "INIT", Value::Null, None)
            .unwrap();
        collector.drain();
        (kernel, collector)
    }

    #[test]
    fn a_tick_emit_reaches_its_listener_through_the_cascade() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "beat", Some("c1"))
            .unwrap();
        assert!(tick.outcome.executed);
        assert_eq!(tick.emits.len(), 1, "one emit, one cascade");
        let emit = &tick.emits[0].dispatch;
        assert_eq!(
            emit.response.states.get("Listener").map(String::as_str),
            Some("heard")
        );
        assert!(emit.response.transitioned);
    }

    #[test]
    fn the_emitter_is_not_re_seeded_by_its_own_tick_emit() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "beat", None)
            .unwrap();
        let response = &tick.emits[0].dispatch.response;
        assert!(
            response.guard_failed.is_none(),
            "no guardFailed: {:?}",
            response.guard_failed
        );
        assert!(
            !response.states.contains_key("Ticker"),
            "the emitter is not an outcome of its own emit"
        );
        assert!(
            response
                .client_effects
                .as_ref()
                .is_none_or(|e| e.is_empty()),
            "no repaint of the emitter's last render"
        );
    }

    #[test]
    fn control_dispatching_the_emitted_event_as_a_seed_does_reseed_the_emitter() {
        let (kernel, _collector) = booted_ticker(false);
        let seeded = kernel
            .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
            .unwrap();
        assert_eq!(
            seeded.response.guard_failed.as_deref(),
            Some("Ticker.BEAT"),
            "the old re-entry path reports the spurious guardFailed"
        );
        assert!(
            seeded
                .response
                .client_effects
                .as_ref()
                .is_some_and(|e| !e.is_empty()),
            "and repaints the emitter"
        );
    }

    #[test]
    fn an_emitter_with_its_own_arm_for_the_event_still_runs_it() {
        let (kernel, _collector) = booted_ticker(true);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "beat", None)
            .unwrap();
        let response = &tick.emits[0].dispatch.response;
        assert_eq!(
            response.states.get("Ticker").map(String::as_str),
            Some("running")
        );
        assert!(
            response.transitioned,
            "the same-trait cascade ran the own BEAT arm"
        );
    }

    #[test]
    fn a_listener_s_persist_yields_a_tick_stamped_server_leg() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "beat", Some("c1"))
            .unwrap();
        let emit = &tick.emits[0].dispatch;
        assert_eq!(emit.server_legs.len(), 1);
        let leg = &emit.server_legs[0];
        assert_eq!(leg.event, "DO_CREATE");
        assert_eq!(leg.tick.as_deref(), Some("beat"));
        assert_eq!(leg.target_trait.as_deref(), Some("Persistor"));
        assert_eq!(leg.client_id.as_deref(), Some("c1"));
        assert_eq!(emit.mode, DispatchMode::HybridClientOnly);
    }

    #[test]
    fn a_tick_cascade_leg_awaits_nothing_but_the_same_cascade_dispatched_does() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "beat", None)
            .unwrap();
        assert!(
            tick.emits[0].dispatch.awaiting.is_empty(),
            "tick legs are latest-state broadcasts"
        );
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
            .unwrap();
        assert!(
            dispatch
                .awaiting
                .iter()
                .any(|a| a.trait_name == "Persistor" && a.event == "DO_CREATE"),
            "control: {:?}",
            dispatch.awaiting
        );
    }

    #[test]
    fn control_a_dispatch_leg_carries_no_tick_stamp() {
        let (kernel, _collector) = booted_ticker(false);
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
            .unwrap();
        let leg = dispatch
            .server_legs
            .first()
            .expect("same cascade, same leg");
        assert!(leg.tick.is_none());
    }

    #[test]
    fn a_tick_without_an_emit_has_no_cascade_and_no_leg() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "quiet", None)
            .unwrap();
        assert!(tick.outcome.executed);
        assert!(tick.emits.is_empty());
        assert_eq!(
            tick.discarded_server_effects, 0,
            "control: nothing server-bound"
        );
    }

    #[test]
    fn a_ticks_own_server_effect_is_discarded_and_never_rides_the_next_leg() {
        let (kernel, collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "saver", None)
            .unwrap();
        assert!(tick.emits.is_empty(), "no event to replay it under");
        assert_eq!(tick.discarded_server_effects, 1, "counted, not silent");
        assert!(
            collector.drain().is_empty(),
            "the collector is clean afterwards"
        );
        let next = kernel
            .dispatch_with_server_leg("App", "Ticker", None, "BEAT", Value::Null, None)
            .unwrap();
        assert_eq!(
            next.server_legs.first().map(|leg| leg.event.as_str()),
            Some("DO_CREATE"),
            "the next dispatch's leg is its own"
        );
    }

    #[test]
    fn a_guard_blocked_tick_runs_no_effects_and_no_cascade() {
        let (kernel, _collector) = booted_ticker(false);
        let tick = kernel
            .dispatch_tick("Main", "Ticker", None, "never", None)
            .unwrap();
        assert!(!tick.outcome.executed);
        assert!(tick.emits.is_empty());
    }

    #[test]
    fn dispatching_an_unknown_tick_or_trait_is_an_error() {
        let (kernel, _collector) = booted_ticker(false);
        assert!(kernel
            .dispatch_tick("Main", "Ticker", None, "nope", None)
            .is_err());
        assert!(kernel
            .dispatch_tick("Main", "Ghost", None, "beat", None)
            .is_err());
    }

    /// std-realtime-chat's composer: SEND persists the draft, then clears it.
    fn clearing_composer_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "content", "type": "string" }
                ] },
                "traits": [{
                    "name": "Composer",
                    "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [
                            { "from": "ready", "to": "ready", "event": "DRAFT",
                              "effects": [["set", "@entity.content", "@payload.value"]] },
                            {
                                "from": "ready", "to": "ready", "event": "SEND",
                                "guard": ["!=", ["str/default", "@entity.content", ""], ""],
                                "effects": [
                                    ["persist", "create", "Note", { "content": "@entity.content" }],
                                    ["set", "@entity.content", ""]
                                ]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// The leg replays SEND from the pre-dispatch state, so it must carry the
    /// pre-dispatch row: the post-dispatch one has the draft already cleared
    /// and fails the replayed guard.
    #[test]
    fn client_env_leg_carries_the_row_the_event_was_dispatched_against() {
        let (kernel, _collector) = client_kernel_with(clearing_composer_schema());
        kernel
            .dispatch_with_server_leg(
                "App",
                "Composer",
                None,
                "DRAFT",
                Value::Object(ValueMap::from([("value".to_string(), Value::from("hi"))])),
                None,
            )
            .unwrap();
        let dispatch = kernel
            .dispatch_with_server_leg("App", "Composer", None, "SEND", Value::Null, None)
            .unwrap();
        let leg = dispatch
            .server_legs
            .first()
            .expect("persist produces a server leg");
        let row = leg
            .entity_by_trait
            .as_ref()
            .and_then(|m| m.get("Composer"))
            .expect("seed row carried");
        assert_eq!(row.get_path("content").and_then(Value::as_str), Some("hi"));
    }
}

#[cfg(test)]
mod theme_mode_tests {
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn schema_with_theme(theme_key: &str) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "theme": theme_key,
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "PanelView",
                    "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// §157/C7 `theme-declaration`: a declared `-dark` theme key must resolve
    /// as Dark when `set_theme_mode` was NEVER called — the kernel's default
    /// (unset) toggle must not silently force Light over the key's own
    /// suffix. `ClientKernel::set_theme_mode` is the passthrough this proves
    /// exists; here it is exercised directly against `RuntimeKernel`.
    #[test]
    fn theme_key_suffix_selects_mode() {
        let kernel = kernel_with(schema_with_theme("clay-dark"));
        let resolved = kernel.resolved_theme();
        assert_eq!(
            resolved.key, "clay-dark",
            "an unset toggle must honor the declared key's own -dark suffix, not force Light"
        );
        assert_eq!(resolved.mode, ThemeMode::Dark);
    }

    /// An explicit `set_theme_mode` call still overrides the declared key's
    /// suffix, exactly as documented on `RuntimeKernel::set_theme_mode`.
    #[test]
    fn explicit_theme_mode_overrides_key_suffix() {
        let kernel = kernel_with(schema_with_theme("clay-dark"));
        kernel.set_theme_mode(ThemeMode::Light);
        let resolved = kernel.resolved_theme();
        assert_eq!(resolved.key, "clay-light");
        assert_eq!(resolved.mode, ThemeMode::Light);
    }
}

#[cfg(test)]
mod mount_lifecycle_tests {
    //! A trait never receives a routed event before its own lifecycle event has
    //! run on the current mount (riya, 2026-09-24: SineLevelData's INIT seeded
    //! the body's skate curve before SineBody's own INIT reset it). JS twin:
    //! `evaluation/mount-lifecycle.ts`.
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn kernel() -> RuntimeKernel {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "Boot",
            "orbitals": [{
                "name": "Level",
                "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    {
                        "name": "Seeder",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT",
                                  "effects": [["emit", "SEED", { "v": 1, "echo": "@payload" }]] },
                                { "from": "idle", "to": "idle", "event": "PING" }
                            ]
                        }
                    },
                    {
                        "name": "Body",
                        "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }, { "name": "seeded" }],
                            "transitions": [
                                { "from": "idle", "to": "ready", "event": "INIT" },
                                { "from": "ready", "to": "seeded", "event": "SET" }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn body_state(kernel: &RuntimeKernel) -> String {
        // No row yet = never touched = its initial state.
        kernel
            .get_state("Body", Some("singleton"))
            .map_or_else(|| "idle".to_string(), |s| s.current_state)
    }

    fn seeds(list: &[(&str, &str)]) -> Vec<crate::effects::MountSeed> {
        list.iter()
            .map(|(t, e)| crate::effects::MountSeed {
                r#trait: t.to_string(),
                event: crate::effects::LifecycleEvent::parse(e).expect("a lifecycle event"),
            })
            .collect()
    }

    /// JS twin: `evaluate-mount-batch.test.ts` — one mount request runs each seed's own
    /// lifecycle event in order; the held SEED reaches Body once Body's own INIT ran.
    #[test]
    fn a_mount_batch_runs_every_seed_in_order_and_keeps_the_lifecycle_hold() {
        let kernel = kernel();
        kernel.mount_traits(["Seeder", "Body"]);
        let outcomes = kernel
            .dispatch_mount(
                &seeds(&[("Seeder", "INIT"), ("Body", "INIT")]),
                None,
                Value::Null,
            )
            .unwrap();
        let ran: Vec<String> = outcomes
            .iter()
            .filter(|(_, r)| r.executed)
            .map(|(t, r)| format!("{t}:{}", r.event.clone().unwrap_or_default()))
            .collect();
        assert_eq!(ran, vec!["Seeder:INIT", "Body:INIT", "Body:SET"]);
        assert_eq!(body_state(&kernel), "seeded");
    }

    /// A server leg's `_awaitingInit` is synced once against EVERY seed: a later seed's
    /// held delivery is not released before its own INIT runs here.
    #[test]
    fn a_mount_batch_syncs_the_clients_mount_against_every_seed() {
        let kernel = kernel();
        kernel.mount_traits(["Seeder", "Body"]);
        kernel
            .dispatch_mount(
                &seeds(&[("Seeder", "INIT"), ("Body", "INIT")]),
                None,
                leg(&[]),
            )
            .unwrap();
        assert_eq!(body_state(&kernel), "seeded");
    }

    #[test]
    fn control_an_unknown_seed_is_skipped_and_the_others_still_run() {
        let kernel = kernel();
        let outcomes = kernel
            .dispatch_mount(
                &seeds(&[("Nowhere", "INIT"), ("Body", "INIT")]),
                None,
                Value::Null,
            )
            .unwrap();
        assert!(outcomes.iter().any(|(t, r)| t == "Body" && r.executed));
        assert!(!outcomes.iter().any(|(t, _)| t == "Nowhere"));
    }

    #[test]
    fn a_seed_waits_for_the_receivers_own_init() {
        let kernel = kernel();
        kernel.mount_traits(["Seeder", "Body"]);
        kernel
            .dispatch("Seeder", None, "INIT", Value::Null)
            .unwrap();
        assert_eq!(
            body_state(&kernel),
            "idle",
            "held, not delivered before Body's INIT"
        );
        kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
        assert_eq!(body_state(&kernel), "seeded");
    }

    /// A server host follows the client's mount from each leg's
    /// `_awaitingInit` (JS twin: `riya-level-seed-reaches-the-body.test.ts`).
    fn leg(awaiting: &[&str]) -> Value {
        Value::Object(ValueMap::from([(
            "_awaitingInit".to_string(),
            Value::Array(
                awaiting
                    .iter()
                    .map(|t| Value::String(t.to_string()))
                    .collect(),
            ),
        )]))
    }

    #[test]
    fn a_server_leg_holds_a_seed_for_a_trait_the_client_still_awaits() {
        let kernel = kernel();
        kernel
            .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
            .unwrap();
        assert_eq!(body_state(&kernel), "idle");
        kernel
            .dispatch_continuing("Body", None, "INIT", leg(&[]), None, None)
            .unwrap();
        assert_eq!(body_state(&kernel), "seeded");
    }

    #[test]
    fn a_server_leg_releases_what_the_client_has_since_initialized() {
        let kernel = kernel();
        kernel
            .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
            .unwrap();
        // Body's INIT stayed client-side; the next leg says nothing awaits.
        kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
        kernel
            .dispatch_continuing("Seeder", None, "PING", leg(&[]), None, None)
            .unwrap();
        assert_eq!(body_state(&kernel), "seeded");
    }

    #[test]
    fn the_sidecar_never_reaches_payload_bindings() {
        let kernel = kernel();
        let out = kernel
            .dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None)
            .unwrap();
        let blob = format!(
            "{:?}",
            out.iter()
                .map(|(_, r)| &r.effect_results)
                .collect::<Vec<_>>()
        );
        assert!(
            blob.contains("echo"),
            "the seed emit echoes @payload: {blob}"
        );
        assert!(!blob.contains("_awaitingInit"), "{blob}");
    }

    #[test]
    fn control_without_a_declared_mount_nothing_is_held() {
        let kernel = kernel();
        kernel
            .dispatch("Seeder", None, "INIT", Value::Null)
            .unwrap();
        kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
        assert_eq!(body_state(&kernel), "ready");
    }

    #[test]
    fn an_unmounted_trait_keeps_nothing_held() {
        let kernel = kernel();
        kernel.mount_traits(["Seeder", "Body"]);
        kernel
            .dispatch("Seeder", None, "INIT", Value::Null)
            .unwrap();
        kernel.unmount_traits(["Body"]);
        kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
        assert_eq!(body_state(&kernel), "ready");
    }
}

#[cfg(test)]
mod host_entity_tests {
    //! A schema registered straight into the kernel (no compiler resolve) binds
    //! an unlinked trait to its host orbital's entity, as the JS runtime does.
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn kernel_with(entity_persistence: &str, linked: Option<&str>) -> RuntimeKernel {
        let mut t = serde_json::json!({
            "name": "Pure",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["set", "@entity.n", 1]] }]
            }
        });
        if let Some(e) = linked {
            t["linkedEntity"] = serde_json::json!(e);
        }
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "Host",
            "orbitals": [
                { "name": "Frames",
                  "entity": { "name": "Frame", "persistence": entity_persistence,
                              "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
                  "traits": [t] },
                { "name": "Rows",
                  "entity": { "name": "Row", "fields": [{ "name": "id", "type": "string" }] },
                  "traits": [] }
            ]
        }))
        .unwrap();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn mode(kernel: &RuntimeKernel) -> DispatchMode {
        kernel.trait_snapshot(Some("singleton"))["Pure"].1
    }

    #[test]
    fn an_unlinked_trait_on_a_runtime_orbital_is_client_only() {
        assert_eq!(
            mode(&kernel_with("runtime", None)),
            DispatchMode::HybridClientOnly
        );
    }

    #[test]
    fn an_unlinked_trait_on_a_persisted_orbital_awaits_the_server() {
        assert_eq!(
            mode(&kernel_with("persistent", None)),
            DispatchMode::PersistedAwaited
        );
    }

    #[test]
    fn a_declared_binding_to_another_orbital_wins() {
        assert_eq!(
            mode(&kernel_with("runtime", Some("Row"))),
            DispatchMode::PersistedAwaited
        );
    }
}

#[cfg(test)]
mod fetch_payload_tests {
    //! A fetch's `emit.success` payload is the TS wire's (`EffectExecutor.ts`):
    //! `{ data, totalCount }`, `totalCount` counted after `@read` + `filter:` and
    //! before offset/limit; a by-id hit's `data` is the row itself.
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn loaded(fetch: serde_json::Value) -> Option<Value> {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "Fetches",
            "orbitals": [{
                "name": "Items",
                "entity": {
                    "name": "Item",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "kind", "type": "string" }],
                    "instances": [
                        { "id": "a", "kind": "x" },
                        { "id": "b", "kind": "y" },
                        { "id": "c", "kind": "x" }
                    ]
                },
                "traits": [{
                    "name": "Loader",
                    "linkedEntity": "Item",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [fetch] }]
                    }
                }]
            }]
        }))
        .unwrap();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        let out = kernel.dispatch("Loader", None, "GO", Value::Null).unwrap();
        out.iter().find_map(|(_, r)| {
            r.effect_results.iter().find_map(|er| match &er.effect {
                crate::effects::Effect::Emit { event, payload } if event == "LOADED" => {
                    Some(payload.clone())
                }
                _ => None,
            })
        })
    }

    fn field<'a>(payload: &'a Option<Value>, key: &str) -> Option<&'a Value> {
        match payload {
            Some(Value::Object(m)) => m.get(key),
            _ => None,
        }
    }

    fn rows(payload: &Option<Value>) -> usize {
        field(payload, "data")
            .and_then(Value::as_array)
            .map_or(usize::MAX, Vec::len)
    }

    #[test]
    fn a_collection_fetch_reports_the_total_before_the_page() {
        let p = loaded(
            serde_json::json!(["fetch", "Item", { "limit": 2, "emit": { "success": "LOADED" } }]),
        );
        assert_eq!(rows(&p), 2);
        assert_eq!(field(&p, "totalCount"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn the_total_counts_the_filtered_rows() {
        let p = loaded(serde_json::json!(["fetch", "Item", {
            "filter": ["=", ["object/get", "@entity", "kind"], "x"], "emit": { "success": "LOADED" }
        }]));
        assert_eq!(rows(&p), 2);
        assert_eq!(field(&p, "totalCount"), Some(&Value::Number(2.0)));
    }

    #[test]
    fn an_offset_past_the_end_still_reports_the_total() {
        let p = loaded(
            serde_json::json!(["fetch", "Item", { "offset": 5, "emit": { "success": "LOADED" } }]),
        );
        assert_eq!(rows(&p), 0);
        assert_eq!(field(&p, "totalCount"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn a_by_id_hit_carries_the_row_itself() {
        let p = loaded(
            serde_json::json!(["fetch", "Item", { "id": "b", "emit": { "success": "LOADED" } }]),
        );
        let row = field(&p, "data").and_then(|d| match d {
            Value::Object(m) => m.get("id").cloned(),
            _ => None,
        });
        assert_eq!(row, Some(Value::String("b".to_string())));
        assert_eq!(field(&p, "totalCount"), Some(&Value::Number(1.0)));
    }

    #[test]
    fn control_a_by_id_miss_emits_no_success() {
        let p = loaded(
            serde_json::json!(["fetch", "Item", { "id": "zz", "emit": { "success": "LOADED" } }]),
        );
        assert_eq!(p, None);
    }
}

#[cfg(test)]
mod set_eval_failure_tests {
    //! A `set` whose value fails to evaluate writes nothing, records a failed
    //! effect and stops its transition: no later effect runs, and the step reports
    //! none of its client effects or emits. Writes made before the failure stand.
    //! JS twin: `effect-failure-contained.test.ts`.
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn kernel() -> RuntimeKernel {
        let broken = serde_json::json!(["set", "@entity.app", ["behavior/ref", "Indigo Pioneer"]]);
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "SetFailure",
            "orbitals": [{
                "name": "Page",
                "entity": { "name": "View", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Preview",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT",
                              "effects": [
                                ["set", "@entity.before", true],
                                ["render-ui", "main", { "type": "typography", "content": "loading" }],
                                broken.clone(),
                                ["set", "@entity.ok", true],
                                ["render-ui", "main", { "type": "typography", "content": "after" }],
                                ["emit", "AFTER"]
                              ] },
                            { "from": "idle", "to": "idle", "event": "NESTED",
                              "effects": [["do", broken.clone(), ["set", "@entity.ok", true]]] },
                            { "from": "idle", "to": "idle", "event": "PLAIN",
                              "effects": [
                                ["set", "@entity.before", true],
                                ["render-ui", "main", { "type": "typography", "content": "loading" }],
                                ["set", "@entity.ok", true],
                                ["render-ui", "main", { "type": "typography", "content": "after" }],
                                ["emit", "AFTER"]
                              ] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn run(event: &str) -> ProcessEventResult {
        let outcomes = kernel()
            .dispatch("Preview", None, event, Value::Null)
            .unwrap();
        outcomes
            .into_iter()
            .find(|(t, _)| t == "Preview")
            .expect("Preview ran")
            .1
    }

    fn has_set(result: &ProcessEventResult, field: &str) -> bool {
        let path = format!("@entity.{field}");
        result
            .effect_results
            .iter()
            .any(|r| matches!(&r.effect, Effect::Set { path: p, .. } if *p == path))
    }

    #[test]
    fn a_set_that_fails_to_evaluate_writes_nothing_and_records_the_failure() {
        let result = run("INIT");
        let row = result.entity_data.as_object().expect("an entity row");
        assert!(!row.contains_key("app"), "{row:?}");
        let failed = result
            .effect_results
            .iter()
            .find(|r| matches!(r.effect, Effect::Set { ref path, .. } if path == "@entity.app"))
            .expect("the set is recorded");
        assert!(
            matches!(failed.outcome, EffectOutcome::Failed),
            "{:?}",
            failed.outcome
        );
        assert!(
            failed
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("specifier"),
            "{:?}",
            failed.error
        );
    }

    #[test]
    fn a_failed_effect_stops_the_remaining_effects_of_its_transition() {
        let result = run("INIT");
        let row = result.entity_data.as_object().expect("an entity row");
        assert!(!row.contains_key("ok"), "{row:?}");
        assert!(!has_set(&result, "ok"));
        assert_eq!(result.effect_runs.len(), 6, "one run per authored effect");
        assert!(
            result.effect_runs[3..]
                .iter()
                .all(|r| r.status == EffectRunStatus::Skipped),
            "{:?}",
            result.effect_runs
        );
    }

    #[test]
    fn writes_before_the_failure_stand() {
        let row = run("INIT").entity_data;
        assert_eq!(
            row.as_object().and_then(|o| o.get("before")),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn the_failing_step_reports_no_client_effect_and_no_emit() {
        let result = run("INIT");
        assert!(
            result
                .effect_results
                .iter()
                .all(|r| !matches!(r.effect, Effect::RenderUi { .. } | Effect::Emit { .. })),
            "{:?}",
            result.effect_results
        );
    }

    #[test]
    fn a_failure_inside_a_do_stops_the_rest_of_the_block() {
        let result = run("NESTED");
        assert!(!has_set(&result, "ok"));
        assert!(result.failure.is_some());
    }

    #[test]
    fn control_a_transition_with_no_failure_runs_every_effect() {
        let result = run("PLAIN");
        assert!(result.failure.is_none());
        let row = result.entity_data.as_object().expect("an entity row");
        assert_eq!(row.get("ok"), Some(&Value::Bool(true)));
        let renders = result
            .effect_results
            .iter()
            .filter(|r| matches!(r.effect, Effect::RenderUi { .. }))
            .count();
        assert_eq!(renders, 2);
        assert!(result
            .effect_results
            .iter()
            .any(|r| matches!(r.effect, Effect::Emit { .. })));
        assert!(result
            .effect_runs
            .iter()
            .all(|r| r.status != EffectRunStatus::Failed));
    }
}

#[cfg(test)]
mod circuit_step_tests {
    //! One dispatch reports the circuit it ran: which `from --event-->` arm fired, every
    //! guard tried, and one status per authored effect of the fired arm (what
    //! `orb execute --steps` and the cosmic zoom's Step read).
    use super::*;
    use crate::runtime::entity_source::InMemoryEntityCache;

    fn kernel() -> RuntimeKernel {
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "EditorApp",
            "orbitals": [{
                "name": "Editor",
                "entity": { "name": "Doc", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "mode", "type": "string", "default": "normal" }] },
                "traits": [{
                    "name": "Modes",
                    "linkedEntity": "Doc",
                    "stateMachine": {
                        "states": [{ "name": "NORMAL", "isInitial": true }, { "name": "INSERT" }, { "name": "VISUAL" }],
                        "transitions": [
                            { "from": "NORMAL", "to": "INSERT", "event": "KEY", "guard": ["=", "@payload.key", "i"], "effects": [["set", "@entity.mode", "insert"]] },
                            { "from": "NORMAL", "to": "VISUAL", "event": "KEY", "guard": ["=", "@payload.key", "v"],
                              "effects": [["set", "@entity.mode", "visual"], ["if", true, ["set", "@entity.mode", "visual"], ["set", "@entity.mode", "never"]]] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        kernel.register_schema(schema).unwrap();
        kernel
    }

    fn key(k: &str) -> Value {
        let mut data = ValueMap::new();
        data.insert("key".to_string(), Value::String(k.to_string()));
        Value::Object(data)
    }

    #[test]
    fn a_fired_dispatch_reports_its_arm_its_guards_and_one_status_per_authored_effect() {
        let result = kernel()
            .process_event("Modes", None, "KEY", key("v"))
            .unwrap();
        assert!(result.executed);
        assert_eq!(result.fired_arm, Some(1));
        assert_eq!(
            result
                .guards
                .iter()
                .map(|g| (g.arm, g.guard.passed))
                .collect::<Vec<_>>(),
            vec![(0, false), (1, true)]
        );
        assert_eq!(
            result.effect_runs.len(),
            2,
            "one run per authored effect, the compound `if` included"
        );
        assert!(result
            .effect_runs
            .iter()
            .all(|r| r.status == EffectRunStatus::Executed));
    }

    #[test]
    fn control_a_blocked_dispatch_has_no_arm_and_no_effect_runs_but_keeps_its_guards() {
        let result = kernel()
            .process_event("Modes", None, "KEY", key("x"))
            .unwrap();
        assert!(!result.executed);
        assert_eq!(result.fired_arm, None);
        assert_eq!(result.guards.len(), 2);
        assert!(result.effect_runs.is_empty());
    }
}
