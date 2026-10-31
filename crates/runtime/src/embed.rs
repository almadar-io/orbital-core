//! The one embedding of the client-role kernel every host drives: the C ABI and JNI
//! (`orbital-ffi`), the WASM package and the Rust renderers. A host marshals arguments in and the
//! [`wire`] types out; every decision (leg routing, merged responses, the tick clock's frame,
//! repaints) is made here.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

pub mod eval_wire;
mod fold_order;
mod tick_relay;
pub(crate) mod ticket_queue;
pub mod memory;
pub mod server;
pub mod wire;

pub use crate::kernel::contract_query::contract_query_without_program;
pub use fold_order::{FoldProgress, FoldedNext};
pub use server::{
    substrate_completion_event_json, os_completion_event_json, os_deferring_ops_json, os_stream_ops_json, tab_url_matches,
    EmbeddedServer, ServerProgress, ServerResolve,
};
#[cfg(feature = "std")]
pub use server::os_glob_matches;

use crate::config::KernelConfig;
use crate::effects::response::{EventRequest, EventResponse};
use crate::effects::response::{MountSeed, TransitionHop};
use orbital_core_base::bus_event::EmittedEvent;
use crate::entity_source::EntitySource;
use crate::entity_source::InMemoryEntityCache;
use crate::kernel::{already_delivered_from, ClientDispatch, EntryError, FrameWrite, RuntimeKernel, TraitSnapshot};
use crate::process_result::ProcessEventResult;
use crate::server_leg::ServerLegCollector;
use crate::tick_clock::{Calendar, TickClock};
use crate::trait_owners::TraitOwners;
use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_base::effect_types::{Effect, RuntimeEnvironment};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::theme::ThemeMode;
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;
use orbital_core_base::sync::Mutex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use wire::{
    AmbiguousTrait, AppliedWire, Entered, Left, DispatchWire, FiredTick, FrameDispatch, FrameWire, MountFailure,
    MountLegWire, MountWire, RoutedLeg, SeedWire, SnapshotWire, StatefulLegWire,
};

/// One host frame, before it is given a wire form: each emit cascade is the `D` the caller asked for.
pub struct FrameParts<D> {
    pub fired: BTreeMap<String, Vec<FiredTick>>,
    pub response: EventResponse,
    pub dispatches: Vec<(String, D)>,
    pub browser_legs: Vec<RoutedLeg>,
    pub dropped_server_effects: usize,
    pub errors: Vec<String>,
}

/// `payload` with the client's mount as `_awaitingInit`.
fn with_awaiting_init(payload: Value, awaiting: Value) -> Value {
    match payload {
        Value::Object(mut map) => {
            map.insert("_awaitingInit".into(), awaiting);
            Value::Object(map)
        }
        Value::Null => Value::object(ValueMap::from_iter([("_awaitingInit".into(), awaiting)])),
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
    let mut entity_by_trait: ValueMap = ValueMap::default();
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
        residence: carried.first().and_then(|leg| leg.residence),
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
    /// A frame carries its ticks' `effectResults` only for a host that asked ([`Self::set_frame_effect_results`]).
    frame_effect_results: std::sync::atomic::AtomicBool,
    /// The flights of the dispatches that owe the host a leg, and the answers waiting for their turn (EXEC-CONC-8).
    fold_order: fold_order::FoldOrder,
    /// The traits whose lifecycle seed the host has been answered, by owning orbital: what the page entered and has not left.
    entered: Mutex<std::collections::BTreeMap<String, String>>,
    /// The identity of the page the host last entered: another key is a new mount.
    mount_key: Mutex<Option<String>>,
    /// The page the host just left and the traits it had entered: a re-enter under the same key before any other page is
    /// that mount continuing (a strict-mode remount), not a new one.
    departed: Mutex<Option<(String, std::collections::BTreeMap<String, String>)>>,
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
    pub fn new(schema: OrbitalSchema, client_id: impl Into<String>) -> Result<Self, EntryError> {
        if schema.orbitals.is_empty() {
            return Err(EntryError::NoOrbitals);
        }
        orbital_core_eval::language_loader::preflight().map_err(EntryError::Language)?;
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
            .map_err(EntryError::Register)?;
        let trait_owners = TraitOwners::from_kernel(&kernel);
        Ok(Self {
            kernel,
            entities,
            trait_owners,
            client_id: client_id.into(),
            ticks: Mutex::new(ticks),
            mounted: Mutex::new(None),
            frame_effect_results: std::sync::atomic::AtomicBool::new(false),
            fold_order: fold_order::FoldOrder::default(),
            entered: Mutex::new(std::collections::BTreeMap::new()),
            mount_key: Mutex::new(None),
            departed: Mutex::new(None),
        })
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
        let flight = (!server_legs.is_empty() || stateful_leg.is_some()).then(|| self.fold_order.open());
        Ok(DispatchWire {
            orbital: orbital.to_string(),
            flight,
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
        self.dispatch_wire(orbital, dispatch)
    }

    /// The kernel's mounts for a host start on `side` (EXEC-ENDPOINT-3).
    pub fn boot_mounts(&self, side: orbital_core_base::protocols::ProtocolSide) -> Vec<MountSeed> {
        self.kernel.boot_mounts(side)
    }

    /// The kernel's mounts for a client opening `route_path` (EXEC-ENDPOINT-3).
    pub fn page_circuit(&self, page: Option<&str>, route: Option<&str>) -> Result<crate::kernel::page_circuit::PageCircuit, String> {
        self.kernel.page_circuit(page, route)
    }

    /// The answer to one [`crate::kernel::contract_query::ContractQuery`] about the registered program, as JSON.
    pub fn contract_query(&self, query: crate::kernel::contract_query::ContractQuery) -> Result<serde_json::Value, String> {
        self.kernel.contract_query(query)
    }

    pub fn route_mounts(&self, route_path: &str) -> Vec<MountSeed> {
        self.kernel.route_mounts(route_path)
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
                        // A stateful host replays each seed from the shared frame it ran from (EXEC-DIST-2).
                        if stateful {
                            let shared = group
                                .iter()
                                .flat_map(|(_, wire)| {
                                    wire.stateful_leg
                                        .iter()
                                        .map(|stateful| &stateful.leg)
                                        .chain(wire.server_legs.iter().map(|routed| &routed.leg))
                                })
                                .flat_map(|leg| leg.entity_by_trait.iter().flatten())
                                .filter(|(name, _)| self.kernel.reads_shared_frame(name));
                            for (name, row) in shared {
                                leg.entity_by_trait
                                    .get_or_insert_with(ValueMap::default)
                                    .entry(name.clone())
                                    .or_insert_with(|| row.clone());
                            }
                        }
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

    /// Fold a server response, skipping every `(trait, event)` in `already_delivered`, for a host that posts and folds
    /// synchronously: it has no overlapping dispatch, so the answer ends every flight its dispatches opened. A host
    /// that posts concurrently folds through [`Self::fold_in_order`] and ends flights itself.
    pub fn apply_server_response(
        &self,
        response: &EventResponse,
        already_delivered: &HashSet<(String, String)>,
    ) -> Result<AppliedWire, String> {
        let applied = self.apply_server_response_since(response, already_delivered, None);
        self.fold_order.end_all();
        applied
    }

    /// The mount a reconnecting client posts to rebuild its session (EXEC-DIST-9): `seeds` with the state and the frame this
    /// client holds for each seeded trait, as the host trusts them. A trait the client holds nothing for is left out of
    /// the carried copies, so the host runs its lifecycle event from defaults.
    pub fn reseed_leg(&self, seeds: &[MountSeed], entity_id: Option<&str>) -> EventRequest {
        let mut traits: Vec<crate::effects::response::TraitFrom> = Vec::new();
        let mut rows = ValueMap::default();
        for seed in seeds {
            let held = self.kernel.snapshot_trait(&seed.r#trait, entity_id);
            if !held.state.is_empty() {
                traits.push(crate::effects::response::TraitFrom { r#trait: seed.r#trait.clone(), from: held.state, event: None });
            }
            if !held.entity.is_null() {
                rows.insert(seed.r#trait.clone().into(), held.entity);
            }
        }
        EventRequest {
            event: seeds.first().map(|seed| seed.event.as_str().to_string()).unwrap_or_default(),
            entity_id: entity_id.map(str::to_string),
            client_id: Some(self.client_id.clone()),
            mount: Some(seeds.to_vec()),
            traits: (!traits.is_empty()).then_some(traits),
            entity_by_trait: (!rows.is_empty()).then_some(rows),
            ..Default::default()
        }
    }

    /// `leg` as a host that holds the circuit state takes it: no carried states, and only the rows of the `[shared]`
    /// frames its traits read, since a client-side write into a shared frame is the one piece of state that host lacks.
    pub fn stateful_leg(&self, mut leg: EventRequest) -> EventRequest {
        leg.traits = None;
        leg.entity_by_trait = leg
            .entity_by_trait
            .take()
            .map(|rows| {
                rows.into_iter()
                    .filter(|(trait_name, _)| self.kernel.reads_shared_frame(trait_name))
                    .collect::<ValueMap>()
            })
            .filter(|rows| !rows.is_empty());
        leg
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
                base.insert(trait_name.clone().into(), frame);
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
        let by_frame: Option<HashMap<String, ValueMap>> = base.map(|base| {
            base.iter()
                .filter_map(|(trait_name, row)| {
                    Some((self.kernel.trait_frame_key(trait_name), row.as_object()?.clone()))
                })
                .collect()
        });
        let (outcomes, frames) = self
            .kernel
            .apply_server_response_with(response, already_delivered, by_frame.as_ref())
            .map_err(|e| e.to_string())?;
        let mut merged = EventResponse::from_process_results(&outcomes, "", "")
            .with_server_effect_results(response);
        let painted = self.kernel.diff_frames(frames.writes);
        self.kernel.seal_outcomes(&mut merged, &outcomes, None, false);
        if let Some(mut painted) = painted {
            if let Some(cascade) = merged.frames.take() {
                painted.ops.extend(cascade.ops);
                painted.to = cascade.to;
            }
            merged.frames = Some(painted);
        }
        self.applied_wire(outcomes, merged, frames.resync)
    }

    /// The wire a fold answers with: its outcomes merged into `merged` (already sealed), the legs its arms owe the host.
    fn applied_wire(
        &self,
        outcomes: Vec<(String, ProcessEventResult)>,
        mut merged: EventResponse,
        resync: Option<crate::kernel::FramesResync>,
    ) -> Result<AppliedWire, String> {
        merged.fold_rerenders(&self.kernel.rerender_stale());
        let hops = TransitionHop::of_outcomes(&outcomes);
        merged.transitions = (!hops.is_empty()).then_some(hops);
        let server_legs = self
            .kernel
            .fold_follow_on_legs(&outcomes, Some(&self.client_id))
            .map_err(|e| e.to_string())?
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
            resync,
        })
    }

    /// [`Self::fold_in_order`] for an emit that entered from outside the kernel (another client's push): it is delivered
    /// to its declared listeners only, guards and payload mapping included, whatever `host_fans_out` and the emit's
    /// `dispatched` stamp say. Folded now or queued behind older flights, exactly as an answer is.
    pub fn deliver_emit_to_listeners(&self, flight: u64, emitted: EmittedEvent) -> Result<FoldProgress, String> {
        self.deliver_emit(flight, emitted, fold_order::EmitOrigin::Peer)
    }

    /// [`Self::deliver_emit_to_listeners`] for a live message of this client's own running call, whose emitter ran its own
    /// arm as that call's progress dispatch: a listener route back to the emitter under the emitted name is that arm again
    /// and does not run twice; every other listener hears the emit.
    pub fn deliver_origin_emit(&self, flight: u64, emitted: EmittedEvent) -> Result<FoldProgress, String> {
        self.deliver_emit(flight, emitted, fold_order::EmitOrigin::Own)
    }

    fn deliver_emit(&self, flight: u64, emitted: EmittedEvent, origin: fold_order::EmitOrigin) -> Result<FoldProgress, String> {
        match self.fold_order.admit(flight, fold_order::Work::Emit(emitted, origin))? {
            Ok(ready) => self.fold_ready(ready.work).map(|applied| FoldProgress::Applied(Box::new(applied))),
            Err(ticket) => Ok(FoldProgress::Queued { ticket }),
        }
    }

    fn fold_ready(&self, work: fold_order::Work) -> Result<AppliedWire, String> {
        match work {
            fold_order::Work::Answer { response, already_delivered, base } => {
                self.apply_server_response_since(&response, &already_delivered, base.as_ref())
            }
            fold_order::Work::Emit(emitted, origin) => {
                let outcomes = match origin {
                    fold_order::EmitOrigin::Peer => self.kernel.deliver_emit_to_listeners(&emitted),
                    fold_order::EmitOrigin::Own => self.kernel.deliver_origin_emit_to_listeners(&emitted),
                }
                .map_err(|e| e.to_string())?;
                let mut merged = EventResponse::from_process_results(&outcomes, "", "");
                self.kernel.seal_outcomes(&mut merged, &outcomes, None, false);
                self.applied_wire(outcomes, merged, None)
            }
        }
    }

    /// Open a flight for work the kernel did not wire (a dispatch the host ran on its own, a frame's browser leg).
    pub fn open_flight(&self) -> u64 {
        self.fold_order.open()
    }

    /// [`Self::apply_server_response_since`] in flight order (EXEC-CONC-8): folded now when `flight` is the oldest
    /// flight still open, else queued behind the older ones and folded by [`Self::next_fold`] once they end.
    pub fn fold_in_order(
        &self,
        flight: u64,
        response: EventResponse,
        already_delivered: HashSet<(String, String)>,
        base: Option<FoldBase>,
    ) -> Result<FoldProgress, String> {
        match self.fold_order.admit(flight, fold_order::Work::Answer { response, already_delivered, base })? {
            Ok(ready) => self.fold_ready(ready.work).map(|applied| FoldProgress::Applied(Box::new(applied))),
            Err(ticket) => Ok(FoldProgress::Queued { ticket }),
        }
    }

    /// The oldest queued answer that can fold now, folded, with the ticket [`Self::fold_in_order`] queued it under.
    /// The host calls it after each flight ends until it is `None`.
    pub fn next_fold(&self) -> Option<FoldedNext> {
        let (ticket, ready) = self.fold_order.take_next()?;
        let outcome = self.fold_ready(ready.work);
        Some(FoldedNext { ticket, outcome })
    }

    /// `flight` is done (its answers are folded, or the host gave up on them): the next flight's answers may fold.
    pub fn end_flight(&self, flight: u64) -> bool {
        self.fold_order.end(flight)
    }

    /// What a painter at version `since` of this kernel's frames is missing: nothing when current, else the snapshot.
    pub fn frames_since(&self, since: u64) -> Result<crate::effects::FrameDelta, String> {
        self.kernel.frames_since(since).ok_or_else(|| "this kernel keeps no frame record".to_string())
    }

    /// Roll a trait instance back to `snapshot`; the response carries the repaint it owes.
    pub fn restore(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        snapshot: TraitSnapshot,
    ) -> EventResponse {
        let restored_entity = snapshot.entity.as_object().cloned();
        self.kernel.restore_trait(trait_name, entity_id, snapshot);
        let mut repaint = EventResponse::from_process_results(&[], "", "");
        let write = match restored_entity {
            Some(fields) => FrameWrite::Replaced(fields),
            None => FrameWrite::Removed,
        };
        self.kernel.seal_frames(&mut repaint, vec![(self.kernel.trait_frame_key(trait_name), write)]);
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
        let outcomes = [(trait_name.to_string(), outcome)];
        let mut response = self.kernel.response_for(&outcomes, "", "");
        response.fold_rerenders(&self.kernel.rerender_stale());
        Ok(response)
    }

    /// One host frame of the tick clock at monotonic `now_ms`: every due tick runs at most once.
    /// `None` when nothing fired and nothing failed.
    pub fn frame(&self, now_ms: u64) -> Result<Option<FrameWire>, String> {
        Ok(self
            .frame_with(now_ms, |orbital, dispatch| self.dispatch_wire(orbital, dispatch), |wire| &mut wire.response)?
            .map(|parts| FrameWire {
                fired: parts.fired,
                response: parts.response,
                dispatches: parts
                    .dispatches
                    .into_iter()
                    .map(|(trait_name, dispatch)| FrameDispatch { trait_name, dispatch })
                    .collect(),
                browser_legs: parts.browser_legs,
                dropped_server_effects: parts.dropped_server_effects,
                errors: parts.errors,
            }))
    }

    /// [`Self::frame`] for the client role: the emit cascades stay [`ClientDispatch`]es, which the client kernel posts
    /// and folds itself (no wire flight is opened).
    pub fn frame_client(&self, now_ms: u64) -> Result<Option<FrameParts<(String, ClientDispatch)>>, String> {
        self.frame_with(now_ms, |orbital, dispatch| Ok((orbital.to_string(), dispatch)), |(_, dispatch)| &mut dispatch.response)
    }

    fn frame_with<D>(
        &self,
        now_ms: u64,
        mut wrap: impl FnMut(&str, ClientDispatch) -> Result<D, String>,
        response_of: impl Fn(&mut D) -> &mut EventResponse,
    ) -> Result<Option<FrameParts<D>>, String> {
        let due = {
            let mounted = self.mounted.lock();
            self.ticks
                .lock()
                .due_among(now_ms, Calendar::local_now, mounted.as_ref())
        };
        if due.is_empty() {
            return Ok(None);
        }
        let due_count = due.len();
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        let mut rerenders = Vec::new();
        let mut last_repaint: HashMap<(String, String), usize> = HashMap::new();
        let mut dispatch_tick_at: Vec<usize> = Vec::new();
        let mut fired: BTreeMap<String, Vec<FiredTick>> = BTreeMap::new();
        let mut dispatches = Vec::new();
        let mut browser_legs = Vec::new();
        let mut errors = Vec::new();
        let mut dropped_server_effects = 0;
        // Each tick keeps the server host's order: its own effects, sealed against the view record, then its emit
        // cascades (whose deltas continue from the record), then the next tick. The frame's response carries the
        // concatenation of those deltas in order, so no absolute field value from before a cascade is written after it (K5).
        let mut frames: Option<crate::effects::FrameDelta> = None;
        let mut sealed = 0;
        let absorb = |frames: &mut Option<crate::effects::FrameDelta>, delta: crate::effects::FrameDelta| match frames {
            None => *frames = Some(delta),
            Some(held) => {
                held.to = delta.to;
                held.ops.extend(delta.ops);
            }
        };
        for (trait_name, tick) in due {
            let orbital = self.owning_orbital(&trait_name)?;
            match self.kernel.dispatch_tick_own(&trait_name, None, &tick, Some(&self.client_id)) {
                Ok(dispatched) => {
                    dropped_server_effects += dispatched.discarded_server_effects;
                    if !dispatched.outcome.executed {
                        continue;
                    }
                    if let Some(leg) = dispatched.browser_leg {
                        browser_legs.push(RoutedLeg {
                            orbital: orbital.to_string(),
                            leg,
                        });
                    }
                    let at = outcomes.len();
                    outcomes.push((trait_name.clone(), dispatched.outcome));
                    // Own writes are sealed only when a cascade is about to read them, in tick order; ticks with no
                    // cascade between them seal together.
                    if outcomes[at].1.effect_results.iter().any(|r| matches!(r.effect, Effect::Emit { .. })) {
                        let mut own = EventResponse::default();
                        self.kernel.seal_outcomes(&mut own, &outcomes[sealed..], None, false);
                        sealed = outcomes.len();
                        if let Some(delta) = own.frames {
                            absorb(&mut frames, delta);
                        }
                    }
                    let emitted = self.kernel.dispatch_tick_emits(orbital, &trait_name, None, &tick, Some(&self.client_id), &outcomes[at].1);
                    match emitted {
                        Ok(emits) => {
                            for emit in emits {
                                if let Some(delta) = emit.dispatch.response.frames.clone() {
                                    absorb(&mut frames, delta);
                                }
                                match wrap(orbital, emit.dispatch) {
                                    Ok(dispatch) => {
                                        dispatch_tick_at.push(at);
                                        dispatches.push((trait_name.clone(), dispatch));
                                    }
                                    Err(e) => errors.push(format!("tick {trait_name}.{tick}: {e}")),
                                }
                            }
                        }
                        Err(e) => errors.push(format!("tick {trait_name}.{tick}: {e}")),
                    }
                    let entry = FiredTick { trait_name, tick };
                    match fired.get_mut(orbital) {
                        Some(ticks) => ticks.push(entry),
                        None => {
                            let mut ticks = Vec::with_capacity(due_count);
                            ticks.push(entry);
                            fired.insert(orbital.to_string(), ticks);
                        }
                    }
                }
                Err(e) => errors.push(format!("tick {trait_name}.{tick}: {e}")),
            }
        }
        let mut response = EventResponse::from_process_results(&outcomes, "", "");
        if sealed < outcomes.len() {
            let mut rest = EventResponse::default();
            self.kernel.seal_outcomes(&mut rest, &outcomes[sealed..], None, false);
            if let Some(delta) = rest.frames {
                absorb(&mut frames, delta);
            }
        }
        response.frames = frames;
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
                        last_repaint
                            .insert((rerender.trait_name.clone(), slot.clone()), fired.len() - 1);
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
            response_of(&mut dispatch.1).drop_renders_of(&superseded);
        }
        response.fold_rerenders(&rerenders);
        // A frame names only the traits a tick moved; the host holds every other state (P-015).
        response
            .states
            .retain(|name, _| outcomes.iter().any(|(t, r)| t == name && r.previous_state != r.new_state));
        if !self.frame_effect_results.load(std::sync::atomic::Ordering::Relaxed) {
            response.effect_results = None;
        }
        Ok(Some(FrameParts {
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

    /// The kernel's dispatch mode for a registered trait (the client reads it, never derives it).
    pub fn dispatch_mode(&self, trait_name: &str) -> Result<DispatchMode, String> {
        self.kernel
            .trait_dispatch_mode(trait_name)
            .ok_or_else(|| format!("dispatch mode: '{trait_name}' is not a registered trait"))
    }

    /// The config the kernel binds as `@config` for a registered trait (render markers resolve against it).
    pub fn trait_config(&self, trait_name: &str) -> Result<Value, String> {
        self.kernel
            .trait_config(trait_name)
            .ok_or_else(|| format!("trait config: '{trait_name}' is not a registered trait"))
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

    /// Install the viewer's locales (the first is the one `i18n/t` paints) and their catalogs, keyed locale then message
    /// key: the client role paints locally what the server role renders (twin: [`EmbeddedServer::set_messages`]).
    pub fn set_messages(
        &self,
        locales: Vec<String>,
        catalogs: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    ) {
        self.kernel.set_messages(locales, catalogs);
    }

    pub fn set_current_user(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// Declare that the host runs the listeners of every emit it marks `dispatched` (see
    /// [`RuntimeKernel::set_host_fans_out`]).
    pub fn set_host_fans_out(&self, fans_out: bool) {
        self.kernel.set_host_fans_out(fans_out);
    }

    /// Record every step's `@entity` after its effects and carry it on its `TransitionHop` (`observed`), for a
    /// verifier judging `@ensures` ([`RuntimeKernel::set_record_entity_after`], EXEC-STEP-10).
    pub fn set_record_entity_after(&self, record: bool) {
        self.kernel.set_record_entity_after(record);
    }

    /// Carry each tick's `effectResults` on the frame response, for a host that verifies them (P-015).
    pub fn set_frame_effect_results(&self, carry: bool) {
        self.frame_effect_results.store(carry, std::sync::atomic::Ordering::Relaxed);
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
        if let Some(misfit) = self.kernel.created_rows_misfit(entity_type, &rows) {
            return Err(format!("seed: {misfit}"));
        }
        for row in rows {
            self.entities
                .create(entity_type, &row)
                .map_err(|e| format!("seed create: {e}"))?;
        }
        Ok(())
    }

    /// Declare a new mount: a routed delivery reaches none of `traits` before its own INIT, and
    /// the tick clock runs the ticks of `traits` only.
    /// The page it replaces is gone: the answer is the traits it had entered that the new mount does not hold, with the
    /// legs that release them on their hosts. The new mount has entered none yet ([`Self::enter`]).
    pub fn remount(&self, traits: Vec<String>) -> Vec<Left> {
        *self.mounted.lock() = Some(traits.iter().cloned().collect());
        self.kernel.remount_traits(traits.clone());
        let mut entered = self.entered.lock();
        let gone = std::mem::take(&mut *entered);
        gone.into_iter()
            .filter(|(name, _)| !traits.contains(name))
            .map(|(name, orbital)| self.unmount_leg(name, orbital))
            .collect()
    }

    /// The page identified by `mount_key` now holds `traits`, opened at `route_path` (`None`: no route, nothing seeded).
    /// A key other than the one held is a new mount: [`Self::remount`] first, so every trait enters afresh and the old
    /// page's traits that are gone leave. Otherwise the traits not entered yet are added to the mount and answered with the
    /// seeds the kernel mounts them by (route order, a child awaiting its composer left out), and the traits entered
    /// earlier that are no longer in the set leave. Every leave carries the leg that releases the trait on its host.
    /// A key re-entered right after [`Self::leave`] with no other page between is the same mount continuing: the traits it
    /// had entered are entered again without their seeds, so a lifecycle event never runs twice for one mount.
    pub fn enter(&self, route_path: Option<&str>, mount_key: &str, traits: Vec<String>) -> Result<Entered, String> {
        let mut owned = Vec::with_capacity(traits.len());
        for name in &traits {
            owned.push((name.clone(), self.owning_orbital(name)?.to_string()));
        }
        let resumed = match self.departed.lock().take() {
            Some((key, held)) if key == mount_key => held,
            _ => std::collections::BTreeMap::new(),
        };
        let fresh = self.mount_key.lock().replace(mount_key.to_string()).as_deref() != Some(mount_key);
        let mut left = if fresh { self.remount(traits.clone()) } else { Vec::new() };
        let resumed: Vec<String> = resumed.into_keys().filter(|name| traits.contains(name)).collect();
        if !resumed.is_empty() {
            self.entered.lock().extend(owned.iter().filter(|(name, _)| resumed.contains(name)).cloned());
            self.kernel.unmount_traits(resumed.clone());
        }
        let (entering, leaving) = {
            let mut entered = self.entered.lock();
            let leaving: Vec<String> = entered.keys().filter(|name| !traits.contains(name)).cloned().collect();
            let entering: Vec<String> = traits.iter().filter(|name| !entered.contains_key(*name)).cloned().collect();
            entered.extend(owned);
            (entering, leaving)
        };
        left.extend(self.unmount(leaving));
        if !entering.is_empty() {
            self.mount(entering.clone());
        }
        let seeds = match route_path {
            Some(path) => self.route_mounts(path).into_iter().filter(|seed| entering.contains(&seed.r#trait)).collect(),
            None => Vec::new(),
        };
        Ok(Entered { seeds, left })
    }

    /// Every trait the page entered leaves (the page closes): the legs that release them on their hosts.
    pub fn leave(&self) -> Vec<Left> {
        let key = self.mount_key.lock().take();
        let held = self.entered.lock().clone();
        *self.departed.lock() = key.map(|key| (key, held));
        let all: Vec<String> = self.entered.lock().keys().cloned().collect();
        self.unmount(all)
    }

    fn unmount_leg(&self, trait_name: String, orbital: String) -> Left {
        let leg = EventRequest {
            event: orbital_core_base::bus_event::UNMOUNT_EVENT.to_string(),
            target_trait: Some(trait_name.clone()),
            client_id: Some(self.client_id.clone()),
            ..Default::default()
        };
        Left { trait_name, orbital, leg }
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

    /// Traits leaving the mount: their ticks stop and nothing stays held for them. The answer is the leg that releases
    /// each trait the page had entered on its host.
    pub fn unmount(&self, traits: Vec<String>) -> Vec<Left> {
        if let Some(mounted) = self.mounted.lock().as_mut() {
            for t in &traits {
                mounted.remove(t);
            }
        }
        let left = {
            let mut entered = self.entered.lock();
            traits.iter().filter_map(|name| entered.remove_entry(name)).collect::<Vec<_>>()
        };
        self.kernel.unmount_traits(traits);
        left.into_iter().map(|(name, orbital)| self.unmount_leg(name, orbital)).collect()
    }

    /// The host's report of whether `trait_name`'s rendered output is on screen (its `[visible]` ticks).
    pub fn set_trait_on_screen(&self, trait_name: &str, on_screen: bool) {
        self.ticks.lock().set_on_screen(trait_name, on_screen);
    }
}
