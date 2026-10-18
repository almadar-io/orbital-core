//! The one embedding of the client-role kernel every host drives: the C ABI and JNI
//! (`orbital-ffi`), the WASM package and the Rust renderers. A host marshals arguments in and the
//! [`wire`] types out; every decision (leg routing, merged responses, the tick clock's frame,
//! repaints) is made here.

pub mod eval_wire;
pub mod server;
pub mod wire;

pub use server::{os_completion_event_json, os_deferring_ops_json, os_glob_matches, tab_url_matches, EmbeddedServer, ServerProgress, ServerResolve};

use orbital_core_base::value::ValueMap;
use crate::effects::response::{MountSeed, TransitionHop};
use crate::kernel::{already_delivered_from, ClientDispatch, RuntimeKernel, TraitSnapshot};
use crate::tick_clock::{Calendar, TickClock};
use orbital_core_base::dispatch_mode::DispatchMode;
use crate::entity_source::InMemoryEntityCache;
use crate::config::KernelConfig;
use crate::server_leg::ServerLegCollector;
use orbital_core_base::theme::ThemeMode;
use crate::trait_owners::TraitOwners;
use orbital_core_base::effect_types::{Effect, RuntimeEnvironment};
use crate::entity_source::EntitySource;
use crate::effects::response::EventResponse;
use orbital_core_base::schema::types::OrbitalSchema;
use crate::process_result::ProcessEventResult;
use orbital_core_base::value::Value;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use wire::{
    AmbiguousTrait, AppliedWire, DispatchWire, FiredTick, FrameDispatch, FrameWire, MountFailure,
    MountLegWire, MountWire, RoutedLeg, SeedWire, SnapshotWire, StatefulLegWire,
};

/// Watches what the embedded kernel does, for a host that records it (the `orb verify` bridge).
/// Every hook defaults to nothing.
pub trait EmbedObserver: Send + Sync {
    /// A dispatch ran (`tick` names the tick whose `emit` began it).
    fn dispatched(
        &self,
        _trait_name: &str,
        _event: &str,
        _payload: &Value,
        _dispatch: &ClientDispatch,
        _tick: Option<&str>,
    ) {
    }
    /// A server response was folded.
    fn applied(
        &self,
        _owners: &TraitOwners,
        _response: &EventResponse,
        _outcomes: &[(String, ProcessEventResult)],
    ) {
    }
    /// A trait was rolled back to a snapshot.
    fn restored(&self) {}
    /// A tick ran during a frame.
    fn tick_fired(&self, _trait_name: &str, _tick: &str) {}
}

/// `payload` with the client's mount as `_awaitingInit`.
fn with_awaiting_init(payload: Value, awaiting: Value) -> Value {
    match payload {
        Value::Object(mut map) => {
            map.insert("_awaitingInit".to_string(), awaiting);
            Value::Object(map)
        }
        Value::Null => Value::Object(ValueMap::from([("_awaitingInit".to_string(), awaiting)])),
        other => other,
    }
}

/// The rows a posted leg is folded against, by trait ([`EmbeddedKernel::fold_base`]).
pub type FoldBase = ValueMap;

/// One `mount` leg for `seeds`: the seeds in order, and the union of the carried legs' stateless
/// addressing (`traits`, `entityByTrait`), each trait at its first occurrence. The payload is that
/// of `payload_from` (it carries the client's mount, `_awaitingInit`).
pub fn mount_leg_request(
    seeds: Vec<MountSeed>,
    carried: &[&crate::effects::response::EventRequest],
    payload_from: Option<&crate::effects::response::EventRequest>,
    entity_id: Option<String>,
    client_id: &str,
) -> crate::effects::response::EventRequest {
    let mut traits: Vec<crate::effects::response::TraitFrom> = Vec::new();
    let mut entity_by_trait: ValueMap = ValueMap::new();
    for leg in carried {
        for t in leg.traits.iter().flatten() {
            if !traits.iter().any(|seen| seen.r#trait == t.r#trait) {
                traits.push(t.clone());
            }
        }
        for (name, row) in leg.entity_by_trait.iter().flatten() {
            entity_by_trait
                .entry(name.clone())
                .or_insert_with(|| row.clone());
        }
    }
    crate::effects::response::EventRequest {
        event: seeds
            .first()
            .map(|seed| seed.event.as_str().to_string())
            .unwrap_or_default(),
        payload: payload_from.and_then(|leg| leg.payload.clone()),
        entity_id,
        client_id: Some(client_id.to_string()),
        mount: Some(seeds),
        traits: (!traits.is_empty()).then_some(traits),
        entity_by_trait: (!entity_by_trait.is_empty()).then_some(entity_by_trait),
        ..Default::default()
    }
}

/// A `Client`-environment kernel with its entity cache, trait index, client id and tick clock.
pub struct EmbeddedKernel {
    kernel: RuntimeKernel,
    entities: Arc<InMemoryEntityCache>,
    trait_owners: TraitOwners,
    client_id: String,
    ticks: Mutex<TickClock>,
    /// The mounted traits whose ticks run; `None` until a host declares a mount (the whole schema).
    mounted: Mutex<Option<BTreeSet<String>>>,
    observer: Option<Arc<dyn EmbedObserver>>,
}

/// Parse the host's light/dark toggle.
pub fn parse_theme_mode(mode: &str) -> Result<ThemeMode, String> {
    match mode {
        "light" => Ok(ThemeMode::Light),
        "dark" => Ok(ThemeMode::Dark),
        other => Err(format!("unknown theme mode: {other}")),
    }
}

impl EmbeddedKernel {
    /// Register an already-resolved schema (what `orb compile` embeds) in a fresh client kernel
    /// whose delegate is the one `ServerLegCollector`.
    pub fn new(schema: OrbitalSchema, client_id: impl Into<String>) -> Result<Self, String> {
        if schema.orbitals.is_empty() {
            return Err("resolved schema declares no orbitals".to_string());
        }
        let ticks = TickClock::from_schema(&schema);
        let entities = Arc::new(InMemoryEntityCache::new());
        let collector = Arc::new(ServerLegCollector::new());
        let config = KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn crate::effects::executor::EffectDelegate>),
            ..KernelConfig::default()
        };
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Client,
            Arc::clone(&entities) as Arc<dyn EntitySource>,
            config,
        );
        kernel
            .register_schema(schema)
            .map_err(|e| format!("register_schema: {e}"))?;
        let trait_owners = TraitOwners::from_kernel(&kernel);
        Ok(Self {
            kernel,
            entities,
            trait_owners,
            client_id: client_id.into(),
            ticks: Mutex::new(ticks),
            mounted: Mutex::new(None),
            observer: None,
        })
    }

    pub fn set_observer(&mut self, observer: Arc<dyn EmbedObserver>) {
        self.observer = Some(observer);
    }

    pub fn kernel(&self) -> &RuntimeKernel {
        &self.kernel
    }

    pub fn entities(&self) -> &Arc<InMemoryEntityCache> {
        &self.entities
    }

    pub fn trait_owners(&self) -> &TraitOwners {
        &self.trait_owners
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The orbital declaring `trait_name`; an error for an unknown or ambiguous trait.
    pub fn owning_orbital(&self, trait_name: &str) -> Result<&str, String> {
        self.trait_owners.owning_orbital(trait_name)
    }

    fn dispatch_wire(
        &self,
        orbital: &str,
        mut dispatch: ClientDispatch,
    ) -> Result<DispatchWire, String> {
        let mut already_delivered: Vec<(String, String)> =
            already_delivered_from(&dispatch).into_iter().collect();
        already_delivered.sort();
        let hops = TransitionHop::of_outcomes(&dispatch.outcomes);
        dispatch.response.transitions = (!hops.is_empty()).then_some(hops);
        let fold_seeded = if matches!(dispatch.mode, DispatchMode::HybridClientOnly) {
            vec![dispatch.seed.0.clone()]
        } else {
            Vec::new()
        };
        let stateful_leg = dispatch
            .stateful_leg
            .map(|stateful| {
                let target = stateful.leg.target_trait.as_deref().unwrap_or_default();
                self.trait_owners
                    .owning_orbital(target)
                    .map(|o| StatefulLegWire {
                        orbital: o.to_string(),
                        leg: stateful.leg,
                        awaiting: stateful.awaiting,
                    })
            })
            .transpose()?;
        let server_legs = dispatch
            .server_legs
            .into_iter()
            .map(|leg| {
                let target = leg.target_trait.as_deref().unwrap_or_default();
                self.trait_owners.owning_orbital(target).map(|o| RoutedLeg {
                    orbital: o.to_string(),
                    leg,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(DispatchWire {
            orbital: orbital.to_string(),
            seed: SeedWire {
                trait_name: dispatch.seed.0,
                event: dispatch.seed.1,
            },
            response: dispatch.response,
            server_legs,
            mode: dispatch.mode,
            snapshot: dispatch.snapshot,
            already_delivered,
            awaiting: dispatch.awaiting,
            stateful_leg,
            fold_seeded,
        })
    }

    /// Dispatch one event through the cross-trait cascade.
    pub fn dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: Value,
    ) -> Result<DispatchWire, String> {
        let orbital = self.owning_orbital(trait_name)?;
        let observed = self.observer.as_ref().map(|_| payload.clone());
        let dispatch = self
            .kernel
            .dispatch_with_server_leg(
                orbital,
                trait_name,
                entity_id,
                event,
                payload,
                Some(&self.client_id),
            )
            .map_err(|e| e.to_string())?;
        if let (Some(observer), Some(payload)) = (&self.observer, &observed) {
            observer.dispatched(trait_name, event, payload, &dispatch, None);
        }
        self.dispatch_wire(orbital, dispatch)
    }

    /// Dispatch a mount batch: every seed's lifecycle event runs locally, in order, and the legs to
    /// post are one `mount` leg per owning orbital (JS twin: `postMountLeg`). `legs` is for a host
    /// that carries the circuit state; `stateful_legs` for one that holds it, which is also posted the
    /// seeds that delegated nothing. A seed that fails stops the batch and is named in `failed`.
    pub fn dispatch_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<MountWire, String> {
        let mut dispatched: Vec<(&MountSeed, DispatchWire)> = Vec::new();
        let mut failed = None;
        for seed in seeds {
            match self.dispatch(
                &seed.r#trait,
                entity_id,
                seed.event.as_str(),
                payload.clone(),
            ) {
                Ok(wire) => dispatched.push((seed, wire)),
                Err(message) => {
                    failed = Some(MountFailure {
                        trait_name: seed.r#trait.clone(),
                        event: seed.event.as_str().to_string(),
                        message,
                    });
                    break;
                }
            }
        }
        let (legs, stateful_legs) = if failed.is_some() {
            (Vec::new(), Vec::new())
        } else {
            (
                self.mount_legs(&dispatched, entity_id, false),
                self.mount_legs(&dispatched, entity_id, true),
            )
        };
        Ok(MountWire {
            seeds: dispatched.into_iter().map(|(_, wire)| wire).collect(),
            legs,
            stateful_legs,
            failed,
        })
    }

    fn mount_legs(
        &self,
        dispatched: &[(&MountSeed, DispatchWire)],
        entity_id: Option<&str>,
        stateful: bool,
    ) -> Vec<MountLegWire> {
        let mut groups: Vec<(&str, Vec<&(&MountSeed, DispatchWire)>)> = Vec::new();
        for item in dispatched {
            let wire = &item.1;
            let posts = !wire.server_legs.is_empty() || (stateful && wire.stateful_leg.is_some());
            if !posts {
                continue;
            }
            match groups
                .iter_mut()
                .find(|(orbital, _)| *orbital == wire.orbital)
            {
                Some((_, group)) => group.push(item),
                None => groups.push((wire.orbital.as_str(), vec![item])),
            }
        }
        groups
            .into_iter()
            .map(|(orbital, group)| {
                // A client-only seed's leg carries its state on every topology: no host holds it.
                let carried: Vec<&crate::effects::response::EventRequest> = group
                    .iter()
                    .filter(|(_, wire)| {
                        !stateful || matches!(wire.mode, DispatchMode::HybridClientOnly)
                    })
                    .flat_map(|(_, wire)| wire.server_legs.iter().map(|routed| &routed.leg))
                    .collect();
                let payload_from = carried.first().copied().or_else(|| {
                    group
                        .iter()
                        .find_map(|(_, wire)| wire.stateful_leg.as_ref().map(|s| &s.leg))
                });
                let mut already_delivered: Vec<(String, String)> = group
                    .iter()
                    .flat_map(|(_, wire)| wire.already_delivered.iter().cloned())
                    .collect();
                already_delivered.sort();
                already_delivered.dedup();
                let awaiting = group
                    .iter()
                    .flat_map(
                        |(_, wire)| match (&wire.stateful_leg, wire.server_legs.is_empty()) {
                            (Some(plain), true) => plain.awaiting.clone(),
                            _ => wire.awaiting.clone(),
                        },
                    )
                    .collect();
                MountLegWire {
                    orbital: orbital.to_string(),
                    seeds: group
                        .iter()
                        .map(|(_, wire)| SeedWire {
                            trait_name: wire.seed.trait_name.clone(),
                            event: wire.seed.event.clone(),
                        })
                        .collect(),
                    leg: {
                        let mut leg = mount_leg_request(
                            group.iter().map(|(seed, _)| (*seed).clone()).collect(),
                            &carried,
                            payload_from,
                            entity_id.map(str::to_string),
                            &self.client_id,
                        );
                        // Every seed has run by now: what the host must still hold is what is left.
                        leg.payload = Some(with_awaiting_init(
                            leg.payload.take().unwrap_or(Value::Null),
                            self.kernel.awaiting_init_value(),
                        ));
                        leg
                    },
                    already_delivered,
                    awaiting,
                    fold_seeded: group
                        .iter()
                        .flat_map(|(_, wire)| wire.fold_seeded.iter().cloned())
                        .collect(),
                }
            })
            .collect()
    }

    /// Fold a server response, skipping every `(trait, event)` in `already_delivered`.
    pub fn apply_server_response(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
    ) -> Result<AppliedWire, String> {
        self.apply_server_response_since(response, already_delivered, None)
    }

    /// The rows a posted leg is folded against: the rows the leg carried, with each `seeded`
    /// trait's row as the client shows it now (its leg carries the row it was dispatched from, yet
    /// the client already shows the arm's result). `None` when the leg carried no rows.
    pub fn fold_base(
        &self,
        carried: Option<&ValueMap>,
        seeded: &[String],
        entity_id: Option<&str>,
    ) -> Option<FoldBase> {
        let carried = carried?;
        let mut base = carried.clone();
        for trait_name in seeded {
            if !carried.contains_key(trait_name) {
                continue;
            }
            let frame = self.kernel.snapshot_trait(trait_name, entity_id).entity;
            if !frame.is_null() {
                base.insert(trait_name.clone(), frame);
            }
        }
        Some(base)
    }

    /// [`Self::apply_server_response`] against a [`FoldBase`]: a returned field equal to what the leg
    /// carried is the host echoing the client, not a write, and would clobber a newer client value.
    /// Only what the host changed (and the row id, which addresses the frame) is folded; a row with
    /// no carried counterpart folds whole.
    pub fn apply_server_response_since(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
        base: Option<&FoldBase>,
    ) -> Result<AppliedWire, String> {
        let pruned;
        let response = match base {
            Some(base) => {
                pruned = self.changed_since(response, base);
                &pruned
            }
            None => response,
        };
        let outcomes = self
            .kernel
            .apply_server_response(response, already_delivered)
            .map_err(|e| e.to_string())?;
        if let Some(observer) = &self.observer {
            observer.applied(&self.trait_owners, response, &outcomes);
        }
        let mut merged = EventResponse::from_process_results(&outcomes, "", "")
            .with_server_effect_results(response);
        merged.fold_rerenders(&self.kernel.rerender_stale());
        let hops = TransitionHop::of_outcomes(&outcomes);
        merged.transitions = (!hops.is_empty()).then_some(hops);
        let server_legs = self
            .kernel
            .fold_follow_on_legs(&outcomes, Some(&self.client_id))
            .into_iter()
            .map(|leg| {
                let target = leg.target_trait.as_deref().unwrap_or_default();
                self.trait_owners.owning_orbital(target).map(|o| RoutedLeg {
                    orbital: o.to_string(),
                    leg,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let applied = outcomes
            .into_iter()
            .map(|(trait_name, result)| (trait_name, result.new_state))
            .collect();
        Ok(AppliedWire {
            applied,
            response: merged,
            server_legs,
        })
    }

    fn changed_since(&self, response: &EventResponse, base: &FoldBase) -> EventResponse {
        let Some(rows) = &response.entity_by_trait else {
            return response.clone();
        };
        let by_frame: HashMap<String, &Value> = base
            .iter()
            .map(|(trait_name, row)| (self.kernel.trait_frame_key(trait_name), row))
            .collect();
        let delta = |trait_name: &String, row: &Value| -> Value {
            let (Some(Value::Object(carried)), Value::Object(fields)) = (
                by_frame
                    .get(&self.kernel.trait_frame_key(trait_name))
                    .copied(),
                row,
            ) else {
                return row.clone();
            };
            Value::Object(
                fields
                    .iter()
                    .filter(|(field, value)| *field == "id" || carried.get(*field) != Some(*value))
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect(),
            )
        };
        let mut pruned = response.clone();
        pruned.entity_by_trait = Some(
            rows.iter()
                .map(|(trait_name, row)| (trait_name.clone(), delta(trait_name, row)))
                .collect(),
        );
        pruned
    }

    /// Roll a trait instance back to `snapshot`; the response carries the repaint it owes.
    pub fn restore(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        snapshot: TraitSnapshot,
    ) -> EventResponse {
        self.kernel.restore_trait(trait_name, entity_id, snapshot);
        if let Some(observer) = &self.observer {
            observer.restored();
        }
        let mut repaint = EventResponse::from_process_results(&[], "", "");
        repaint.fold_rerenders(&self.kernel.rerender_stale());
        repaint
    }

    /// Run one named tick.
    pub fn tick(
        &self,
        trait_name: &str,
        tick_name: &str,
        entity_id: Option<&str>,
    ) -> Result<EventResponse, String> {
        let outcome = self
            .kernel
            .process_tick(trait_name, entity_id, tick_name)
            .map_err(|e| e.to_string())?;
        let mut response = EventResponse::from_process_result(trait_name, &outcome);
        response.fold_rerenders(&self.kernel.rerender_stale());
        Ok(response)
    }

    /// One host frame of the tick clock at monotonic `now_ms`: every due tick runs at most once.
    /// `None` when nothing fired and nothing failed.
    pub fn frame(&self, now_ms: u64) -> Result<Option<FrameWire>, String> {
        let due = {
            let mounted = self.mounted.lock();
            self.ticks
                .lock()
                .due_among(now_ms, Calendar::local_now, mounted.as_ref())
        };
        if due.is_empty() {
            return Ok(None);
        }
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        let mut rerenders = Vec::new();
        let mut last_repaint: HashMap<(String, String), usize> = HashMap::new();
        let mut dispatch_tick_at: Vec<usize> = Vec::new();
        let mut fired = Vec::new();
        let mut dispatches = Vec::new();
        let mut browser_legs = Vec::new();
        let mut errors = Vec::new();
        let mut dropped_server_effects = 0;
        for (trait_name, tick) in due {
            let orbital = self.owning_orbital(&trait_name)?;
            match self.kernel.dispatch_tick_unrepainted(
                orbital,
                &trait_name,
                None,
                &tick,
                Some(&self.client_id),
            ) {
                Ok(dispatched) => {
                    dropped_server_effects += dispatched.discarded_server_effects;
                    if !dispatched.outcome.executed {
                        continue;
                    }
                    if let Some(observer) = &self.observer {
                        observer.tick_fired(&trait_name, &tick);
                    }
                    if let Some(leg) = dispatched.browser_leg {
                        browser_legs.push(RoutedLeg {
                            orbital: orbital.to_string(),
                            leg,
                        });
                    }
                    for emit in dispatched.emits {
                        if let Some(observer) = &self.observer {
                            observer.dispatched(
                                &trait_name,
                                &emit.event,
                                &emit.payload,
                                &emit.dispatch,
                                Some(&tick),
                            );
                        }
                        match self.dispatch_wire(orbital, emit.dispatch) {
                            Ok(dispatch) => {
                                dispatch_tick_at.push(fired.len());
                                dispatches.push(FrameDispatch {
                                    trait_name: trait_name.clone(),
                                    dispatch,
                                });
                            }
                            Err(e) => errors.push(format!("tick {trait_name}.{tick}: {e}")),
                        }
                    }
                    fired.push(FiredTick {
                        trait_name: trait_name.clone(),
                        tick,
                    });
                    outcomes.push((trait_name, dispatched.outcome));
                    for rerender in &dispatched.rerenders {
                        for effect in &rerender.effects {
                            if let Effect::RenderUi { slot, .. } = effect {
                                last_repaint.insert(
                                    (rerender.trait_name.clone(), slot.clone()),
                                    fired.len() - 1,
                                );
                            }
                        }
                    }
                    rerenders.extend(dispatched.rerenders);
                }
                Err(e) => errors.push(format!("tick {trait_name}.{tick}: {e}")),
            }
        }
        if fired.is_empty() && errors.is_empty() {
            return Ok(None);
        }
        // One repaint pass for the whole frame: it sees the newest content of every slot, so it
        // supersedes the dispatch renders of every tick fired before it.
        if !fired.is_empty() {
            let tail = self.kernel.rerender_stale();
            for rerender in &tail {
                for effect in &rerender.effects {
                    if let Effect::RenderUi { slot, .. } = effect {
                        last_repaint.insert(
                            (rerender.trait_name.clone(), slot.clone()),
                            fired.len() - 1,
                        );
                    }
                }
            }
            rerenders.extend(tail);
        }
        // The host folds `response` before the dispatches, so a dispatch render of a slot that the
        // same or a later tick repainted would land last with older content: the repaint is the
        // slot's newest paint, and the dispatch's stale copy is dropped from the envelope.
        for (dispatch, tick_at) in dispatches.iter_mut().zip(&dispatch_tick_at) {
            let superseded: HashSet<(String, String)> = last_repaint
                .iter()
                .filter(|(_, repainted_at)| *repainted_at >= tick_at)
                .map(|(key, _)| key.clone())
                .collect();
            dispatch.dispatch.response.drop_renders_of(&superseded);
        }
        let mut response = EventResponse::from_process_results(&outcomes, "", "");
        response.fold_rerenders(&rerenders);
        Ok(Some(FrameWire {
            fired,
            response,
            dispatches,
            browser_legs,
            dropped_server_effects,
            errors,
        }))
    }

    /// Stop the tick clock (host backgrounded); declared ticks and their phase are kept.
    pub fn frame_pause(&self) {
        self.ticks.lock().pause();
    }

    /// Restart the tick clock; the paused span is never counted.
    pub fn frame_resume(&self) {
        self.ticks.lock().resume();
    }

    /// How many `(trait, tick)` pairs the schema declares.
    pub fn tick_count(&self) -> usize {
        self.ticks.lock().len()
    }

    /// The current state of a trait instance, `""` when unset.
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> String {
        self.kernel
            .get_state(trait_name, entity_id)
            .map(|s| s.current_state)
            .unwrap_or_default()
    }

    /// Every unambiguous trait's state plus the resolved theme, canvas theme and motion.
    pub fn snapshot(&self) -> SnapshotWire {
        let mut states = serde_json::Map::new();
        let mut ambiguous_traits = Vec::new();
        for (trait_name, orbitals) in self.trait_owners.iter() {
            if orbitals.len() > 1 {
                ambiguous_traits.push(AmbiguousTrait {
                    trait_name: trait_name.clone(),
                    orbitals: orbitals.iter().cloned().collect(),
                });
                continue;
            }
            states.insert(
                trait_name.clone(),
                serde_json::Value::String(self.get_state(trait_name, None)),
            );
        }
        let theme = self.kernel.resolved_theme();
        let canvas_theme = crate::canvas_theme::canvas_theme(&theme.tokens);
        let motion = crate::motion::resolve_motion(&theme.tokens);
        SnapshotWire {
            states,
            ambiguous_traits,
            theme,
            canvas_theme,
            motion,
        }
    }

    pub fn set_theme_mode(&self, mode: ThemeMode) {
        self.kernel.set_theme_mode(mode);
    }

    pub fn set_current_user(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// Declare that the host runs the listeners of every emit it marks `dispatched` (see
    /// [`RuntimeKernel::set_host_fans_out`]).
    pub fn set_host_fans_out(&self, fans_out: bool) {
        self.kernel.set_host_fans_out(fans_out);
    }

    /// A row-set reference resolved against this client's rows for its current user: the one kernel resolver
    /// ([`RuntimeKernel::resolve_rows`]) every renderer reaches. A store the client does not hold answers
    /// nothing here; the host resolves those through the server leg.
    pub fn resolve_rows(
        &self,
        reference: &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, String> {
        self.kernel
            .resolve_rows(reference, &self.kernel.get_current_user())
    }

    /// Seed `entity_type` rows into the entity cache; each row's own `id` is honored.
    pub fn seed(&self, entity_type: &str, rows: Vec<Value>) -> Result<(), String> {
        for row in rows {
            self.entities
                .create(entity_type, &row)
                .map_err(|e| format!("seed create: {e}"))?;
        }
        Ok(())
    }

    /// Declare a new mount: a routed delivery reaches none of `traits` before its own INIT, and
    /// the tick clock runs the ticks of `traits` only.
    pub fn remount(&self, traits: Vec<String>) {
        *self.mounted.lock() = Some(traits.iter().cloned().collect());
        self.kernel.remount_traits(traits);
    }

    /// Add traits to the current mount without dropping those already mounted. The first mount a
    /// host declares scopes the tick clock to it.
    pub fn mount(&self, traits: Vec<String>) {
        self.mounted
            .lock()
            .get_or_insert_with(BTreeSet::new)
            .extend(traits.iter().cloned());
        self.kernel.mount_traits(traits);
    }

    /// Traits leaving the mount: their ticks stop and nothing stays held for them.
    pub fn unmount(&self, traits: Vec<String>) {
        if let Some(mounted) = self.mounted.lock().as_mut() {
            for t in &traits {
                mounted.remove(t);
            }
        }
        self.kernel.unmount_traits(traits);
    }

    /// The host's report of whether `trait_name`'s rendered output is on screen (its `[visible]` ticks).
    pub fn set_trait_on_screen(&self, trait_name: &str, on_screen: bool) {
        self.ticks.lock().set_on_screen(trait_name, on_screen);
    }
}
