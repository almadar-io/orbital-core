//! The server-role embedding: a `Server`-environment [`RuntimeKernel`] whose every read, write,
//! service call and substrate operator is an `Await` the host answers. A JavaScript host cannot block
//! on a promise, so it starts a dispatch, answers the requests it is handed (in any order, all at
//! once) and resumes. The continuation never leaves this module; only requests and answers cross.

use super::wire::{IoReplyWire, PendingRequestWire, ResolveProgressWire, ServerProgressWire};
use crate::effects::{RunTick, TraitFrom, TransitionHop};
use crate::effects::{ServiceHandler, SubstrateHandler};
use crate::runtime::payload_validation::{format_payload_failures, validate_event_payload};
use crate::runtime::resumable::{
    DispatchContinuation, DispatchError, DispatchOutcomes, HostAnswered, IoReply, PendingRequest,
    Progress, ResolveContinuation, ResolveProgress,
};
use crate::runtime::row_query::{RowPage, RowSetRef};
use crate::runtime::{KernelConfig, KernelError, RuntimeKernel};
use crate::{EntitySource, EventRequest, EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Where a server dispatch stands.
#[derive(Debug)]
pub enum ServerProgress {
    Done(EventResponse),
    Await {
        flight: u64,
        requests: Vec<PendingRequest>,
    },
}

impl From<&ServerProgress> for ServerProgressWire {
    fn from(progress: &ServerProgress) -> Self {
        match progress {
            ServerProgress::Done(response) => ServerProgressWire::Done {
                response: response.clone(),
            },
            ServerProgress::Await { flight, requests } => ServerProgressWire::Await {
                flight: *flight,
                requests: requests.iter().map(PendingRequestWire::from).collect(),
            },
        }
    }
}

/// Where a row-set resolve stands: the page, or the reads the host answers next.
#[derive(Debug)]
pub enum ServerResolve {
    Done(RowPage),
    Await {
        flight: u64,
        requests: Vec<PendingRequest>,
    },
}

impl From<&ServerResolve> for ResolveProgressWire {
    fn from(progress: &ServerResolve) -> Self {
        match progress {
            ServerResolve::Done(page) => ResolveProgressWire::Done {
                rows: page.rows.clone(),
                total: page.total,
            },
            ServerResolve::Await { flight, requests } => ResolveProgressWire::Await {
                flight: *flight,
                requests: requests.iter().map(PendingRequestWire::from).collect(),
            },
        }
    }
}

/// What a dispatch was begun with, kept to build its response.
struct Seed {
    trait_name: String,
    event: String,
    /// A tick firing: its response never reports a rejected seed.
    tick: bool,
    /// The row the request addressed; its frames are the ones the response echoes.
    entity_id: Option<String>,
}

struct Flight {
    continuation: DispatchContinuation,
    seed: Seed,
    user: Value,
    /// Set for an untargeted request: the traits it was delivered to and the held state of the orbital's.
    discovery: Option<Discovery>,
}

type Discovery = (Vec<String>, HashMap<String, String>);

/// A server-role kernel and the dispatches it has in flight.
pub struct EmbeddedServer {
    kernel: RuntimeKernel,
    orbital_traits: HashMap<String, Vec<String>>,
    default_user: Mutex<Value>,
    /// The traits some client has mounted: a tick that is not `[background]` fires only for these.
    tick_mount: Mutex<BTreeSet<String>>,
    flights: Mutex<HashMap<u64, Flight>>,
    /// Row-set resolves waiting on the host; they share `next_flight`'s ids with the dispatches.
    resolves: Mutex<HashMap<u64, ResolveContinuation>>,
    next_flight: Mutex<u64>,
}

impl EmbeddedServer {
    /// Register an already-resolved schema in a fresh server-role kernel.
    pub fn new(schema: OrbitalSchema) -> Result<Self, String> {
        if schema.orbitals.is_empty() {
            return Err("resolved schema declares no orbitals".to_string());
        }
        let orbital_traits = schema
            .orbitals
            .iter()
            .map(|orbital| {
                let traits = orbital
                    .traits
                    .iter()
                    .filter_map(|t| t.trait_name().map(str::to_string))
                    .collect();
                (orbital.name.clone(), traits)
            })
            .collect();
        let host = Arc::new(HostAnswered);
        let config = KernelConfig {
            environment: RuntimeEnvironment::Server,
            service_handler: Some(host.clone() as Arc<dyn ServiceHandler>),
            substrate_handler: Some(host.clone() as Arc<dyn SubstrateHandler>),
            ..KernelConfig::default()
        };
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            host as Arc<dyn EntitySource>,
            config,
        );
        kernel
            .register_schema(schema)
            .map_err(|e| format!("register_schema: {e}"))?;
        let default_user = kernel.get_current_user();
        Ok(Self {
            kernel,
            orbital_traits,
            default_user: Mutex::new(default_user),
            tick_mount: Mutex::new(BTreeSet::new()),
            flights: Mutex::new(HashMap::new()),
            resolves: Mutex::new(HashMap::new()),
            next_flight: Mutex::new(0),
        })
    }

    pub fn kernel(&self) -> &RuntimeKernel {
        &self.kernel
    }

    /// The viewer a request that names none runs as.
    pub fn set_default_user(&self, user: Value) {
        *self.default_user.lock() = user;
    }

    /// Install the program's declared locales (the first is the default) and merged message catalogs,
    /// keyed locale then message key; a lazy page's behavior contributes its own namespaced keys.
    pub fn set_messages(
        &self,
        locales: Vec<String>,
        catalogs: HashMap<String, HashMap<String, String>>,
    ) {
        self.kernel.set_messages(locales, catalogs);
    }

    /// Declare what the host's row store honours for `entity_type`, as JSON: `query` is a
    /// `QueryCapabilities`, `write` a `WriteCapabilities`; either may be `null` to declare nothing.
    pub fn set_capabilities_json(
        &self,
        entity_type: &str,
        query_json: &str,
        write_json: &str,
    ) -> Result<(), String> {
        let query: Option<crate::runtime::row_query::QueryCapabilities> =
            serde_json::from_str(query_json).map_err(|e| format!("parse query_json: {e}"))?;
        let write: Option<crate::runtime::row_query::WriteCapabilities> =
            serde_json::from_str(write_json).map_err(|e| format!("parse write_json: {e}"))?;
        if let Some(q) = query {
            self.kernel.set_query_capabilities(entity_type, q);
        }
        if let Some(w) = write {
            self.kernel.set_write_capabilities(entity_type, w);
        }
        Ok(())
    }

    /// The traits whose mount-scoped ticks fire: the host's union of what its clients have mounted.
    pub fn set_tick_mount(&self, traits: Vec<String>) {
        *self.tick_mount.lock() = traits.into_iter().collect();
    }

    /// Add traits to the current mount without dropping those already mounted.
    pub fn mount(&self, traits: Vec<String>) {
        self.kernel.mount_traits(traits);
    }

    /// A new mount replacing the previous one.
    pub fn remount(&self, traits: Vec<String>) {
        self.kernel.remount_traits(traits);
    }

    /// The current state of a trait instance, `""` when unset.
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> String {
        self.kernel
            .get_state(trait_name, entity_id)
            .map(|s| s.current_state)
            .unwrap_or_default()
    }

    /// How many dispatches are waiting on the host.
    pub fn in_flight(&self) -> usize {
        self.flights.lock().len() + self.resolves.lock().len()
    }

    /// Forget a dispatch the host gave up on.
    pub fn abandon(&self, flight: u64) -> bool {
        self.flights.lock().remove(&flight).is_some() || self.resolves.lock().remove(&flight).is_some()
    }

    /// `reference` resolved for `viewer` against the host-answered store (EXEC-RENDER-3): the ids that still
    /// match its query and the viewer's `@read`, a missing or hidden row omitted and `total` re-counted. Each read
    /// is a request the host answers, as in a dispatch; resume the flight with [`Self::resume_resolve`].
    pub fn resolve_rows(
        &self,
        reference: &RowSetRef,
        viewer: &Value,
    ) -> Result<ServerResolve, String> {
        let progress = self
            .kernel
            .start_resolve_rows(reference, viewer)
            .map_err(dispatch_error)?;
        Ok(self.settle_resolve(progress))
    }

    /// Go on with a resolve `flight`, given an answer to each request it is waiting on.
    pub fn resume_resolve(
        &self,
        flight: u64,
        replies: Vec<IoReply>,
    ) -> Result<ServerResolve, String> {
        let continuation = self
            .resolves
            .lock()
            .remove(&flight)
            .ok_or_else(|| format!("no resolve is waiting as flight {flight}"))?;
        let progress = self
            .kernel
            .resume_resolve_rows(continuation, replies)
            .map_err(dispatch_error)?;
        Ok(self.settle_resolve(progress))
    }

    /// [`Self::resolve_rows`] over JSON: the reference and viewer in, a `ResolveProgressWire` out.
    pub fn resolve_rows_json(&self, ref_json: &str, viewer_json: &str) -> Result<String, String> {
        let reference: RowSetRef =
            serde_json::from_str(ref_json).map_err(|e| format!("parse ref_json: {e}"))?;
        let viewer: Value =
            serde_json::from_str(viewer_json).map_err(|e| format!("parse viewer_json: {e}"))?;
        let progress = self.resolve_rows(&reference, &viewer)?;
        serde_json::to_string(&ResolveProgressWire::from(&progress))
            .map_err(|e| format!("serialize: {e}"))
    }

    /// [`Self::resume_resolve`] over JSON: the answers (`IoReplyWire[]`) in, a `ResolveProgressWire` out.
    pub fn resume_resolve_json(&self, flight: u64, replies_json: &str) -> Result<String, String> {
        let replies: Vec<IoReplyWire> =
            serde_json::from_str(replies_json).map_err(|e| format!("parse replies_json: {e}"))?;
        let progress =
            self.resume_resolve(flight, replies.into_iter().map(IoReply::from).collect())?;
        serde_json::to_string(&ResolveProgressWire::from(&progress))
            .map_err(|e| format!("serialize: {e}"))
    }

    fn settle_resolve(&self, progress: ResolveProgress) -> ServerResolve {
        match progress {
            ResolveProgress::Done(page) => ServerResolve::Done(page),
            ResolveProgress::Await {
                continuation,
                requests,
            } => {
                let flight = {
                    let mut next = self.next_flight.lock();
                    let id = *next;
                    *next += 1;
                    id
                };
                self.resolves.lock().insert(flight, continuation);
                ServerResolve::Await { flight, requests }
            }
        }
    }

    /// Start `request` against `orbital`. A delegated leg (`traits` / `entityByTrait`) first seeds the client's
    /// states and rows. A mount batch seeds each lifecycle event; otherwise the target is `targetTrait`, else
    /// every declared trait of `traits`, else every trait of the orbital whose held state handles the event.
    pub fn start(&self, orbital: &str, request: EventRequest) -> Result<ServerProgress, String> {
        let traits = self
            .orbital_traits
            .get(orbital)
            .ok_or_else(|| format!("Orbital not found: {orbital}"))?;
        let user = request
            .user
            .clone()
            .unwrap_or_else(|| self.default_user.lock().clone());
        let _locale = self
            .kernel
            .enter_locale(request.locale.as_deref())
            .map_err(|e| e.to_string())?;
        let payload = request.payload.clone().unwrap_or(Value::Null);
        let carried_id = request.target_trait.as_ref().and_then(|target| {
            request
                .entity_by_trait
                .as_ref()?
                .get(target)?
                .get_path("id")?
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        });
        let entity_id = request.entity_id.clone().or(carried_id);
        let entity_id = entity_id.as_deref();
        self.kernel.set_current_user(user.clone());
        let carries_state = request.traits.is_some() || request.entity_by_trait.is_some();
        if carries_state {
            self.kernel.seed_leg(
                request.traits.as_deref().unwrap_or_default(),
                request.entity_by_trait.as_ref(),
                entity_id,
            );
        }
        if let Some(run) = request.run_tick.as_ref() {
            return self.start_tick(run, entity_id, &request.event, user);
        }
        if let Some(seeds) = request.mount.as_ref().filter(|seeds| !seeds.is_empty()) {
            let first = &seeds[0];
            let seed_trait = first.r#trait.clone();
            let seed_event = first.event.as_str().to_string();
            let targets: Vec<(String, String)> = seeds
                .iter()
                .map(|seed| (seed.r#trait.clone(), seed.event.as_str().to_string()))
                .collect();
            if let Some(rejected) = self.payload_rejection(&targets, &payload) {
                return Ok(ServerProgress::Done(rejected));
            }
            let progress = self
                .kernel
                .start_dispatch_mount(seeds, entity_id, payload)
                .map_err(dispatch_error)?;
            let seed = Seed {
                trait_name: seed_trait,
                event: seed_event,
                tick: false,
                entity_id: entity_id.map(str::to_string),
            };
            return self.settle(progress, seed, user, None);
        }
        if request.target_trait.is_none() {
            if let Some(declared) = request.traits.as_ref() {
                return self.start_declared(declared, entity_id, &request, payload, user);
            }
            let handlers = self
                .kernel
                .discover_handlers(traits, entity_id, &request.event);
            let held = self.kernel.held_states(traits, entity_id);
            if handlers.is_empty() {
                return Ok(ServerProgress::Done(EventResponse::from_discovery(
                    &[],
                    &handlers,
                    &request.event,
                    held,
                )));
            }
            let targets: Vec<(String, String)> = handlers
                .iter()
                .map(|t| (t.clone(), request.event.clone()))
                .collect();
            if let Some(rejected) = self.payload_rejection(&targets, &payload) {
                return Ok(ServerProgress::Done(rejected));
            }
            let progress = self
                .kernel
                .start_dispatch_each(
                    &handlers,
                    entity_id,
                    &request.event,
                    payload,
                    request.delivery.as_ref(),
                    request.dispatch_log.as_ref(),
                )
                .map_err(dispatch_error)?;
            let seed_trait = handlers[0].clone();
            let seed = Seed {
                trait_name: seed_trait,
                event: request.event,
                tick: false,
                entity_id: entity_id.map(str::to_string),
            };
            return self.settle(progress, seed, user, Some((handlers, held)));
        }
        let seed_trait = request
            .target_trait
            .clone()
            .ok_or_else(|| "No trait specified and orbital has no traits".to_string())?;
        let targets = [(seed_trait.clone(), request.event.clone())];
        if let Some(rejected) = self.payload_rejection(&targets, &payload) {
            return Ok(ServerProgress::Done(rejected));
        }
        let progress = self
            .kernel
            .start_dispatch(
                &seed_trait,
                entity_id,
                &request.event,
                payload,
                request.delivery.as_ref(),
                request.dispatch_log.as_ref(),
            )
            .map_err(dispatch_error)?;
        let seed = Seed {
            trait_name: seed_trait,
            event: request.event,
            tick: false,
            entity_id: entity_id.map(str::to_string),
        };
        self.settle(progress, seed, user, None)
    }

    /// The payload check at the API boundary: `Some(error response)` when any of `targets` (trait, the
    /// event it receives) declares a payload the request does not satisfy.
    fn payload_rejection(
        &self,
        targets: &[(String, String)],
        payload: &Value,
    ) -> Option<EventResponse> {
        let mut failures = Vec::new();
        for (trait_name, event) in targets {
            if let Some(schema) = self.kernel.event_payload_schema(trait_name, event) {
                failures.extend(validate_event_payload(event, payload, &schema));
            }
        }
        (!failures.is_empty()).then(|| EventResponse::error(format_payload_failures(&failures)))
    }

    /// A request that names the traits it dispatches (`traits`) and no `targetTrait`: every declared
    /// trait runs, each from its carried state, and an empty list is an inert rejection.
    fn start_declared(
        &self,
        declared: &[TraitFrom],
        entity_id: Option<&str>,
        request: &EventRequest,
        payload: Value,
        user: Value,
    ) -> Result<ServerProgress, String> {
        let names: Vec<String> = declared.iter().map(|t| t.r#trait.clone()).collect();
        let Some(first) = names.first().cloned() else {
            return Ok(ServerProgress::Done(EventResponse::from_discovery(
                &[],
                &[],
                &request.event,
                HashMap::new(),
            )));
        };
        let targets: Vec<(String, String)> = names
            .iter()
            .map(|t| (t.clone(), request.event.clone()))
            .collect();
        if let Some(rejected) = self.payload_rejection(&targets, &payload) {
            return Ok(ServerProgress::Done(rejected));
        }
        let progress = self
            .kernel
            .start_dispatch_each(
                &names,
                entity_id,
                &request.event,
                payload,
                request.delivery.as_ref(),
                request.dispatch_log.as_ref(),
            )
            .map_err(dispatch_error)?;
        let seed = Seed {
            trait_name: first,
            event: request.event.clone(),
            tick: false,
            entity_id: entity_id.map(str::to_string),
        };
        self.settle(progress, seed, user, None)
    }

    fn start_tick(
        &self,
        run: &RunTick,
        entity_id: Option<&str>,
        event: &str,
        user: Value,
    ) -> Result<ServerProgress, String> {
        let background = match self.kernel.tick_of(&run.trait_name, &run.tick) {
            Some(tick) => tick.background,
            None => {
                return Ok(ServerProgress::Done(EventResponse::from_discovery(
                    &[],
                    &[],
                    event,
                    HashMap::new(),
                )))
            }
        };
        if !background && !self.tick_mount.lock().contains(&run.trait_name) {
            return Ok(ServerProgress::Done(EventResponse::success("")));
        }
        let progress = self
            .kernel
            .start_dispatch_tick(&run.trait_name, entity_id, &run.tick)
            .map_err(dispatch_error)?;
        let seed = Seed {
            trait_name: run.trait_name.clone(),
            event: event.to_string(),
            tick: true,
            entity_id: entity_id.map(str::to_string),
        };
        self.settle(progress, seed, user, None)
    }

    /// Go on with `flight`, given an answer to each request it is waiting on. A refused set of
    /// answers ends the dispatch: the host asked for something it cannot supply.
    pub fn resume(&self, flight: u64, replies: Vec<IoReply>) -> Result<ServerProgress, String> {
        let Flight {
            continuation,
            seed,
            user,
            discovery,
        } = self
            .flights
            .lock()
            .remove(&flight)
            .ok_or_else(|| format!("no dispatch is waiting as flight {flight}"))?;
        self.kernel.set_current_user(user.clone());
        let progress = self
            .kernel
            .resume(continuation, replies)
            .map_err(dispatch_error)?;
        self.settle(progress, seed, user, discovery)
    }

    /// [`Self::resume`] with the wire's answers.
    pub fn resume_wire(
        &self,
        flight: u64,
        replies: Vec<IoReplyWire>,
    ) -> Result<ServerProgress, String> {
        self.resume(flight, replies.into_iter().map(IoReply::from).collect())
    }

    /// [`Self::start`] over JSON: the request in, a `ServerProgressWire` out.
    pub fn start_json(&self, orbital: &str, request_json: &str) -> Result<String, String> {
        let request: EventRequest =
            serde_json::from_str(request_json).map_err(|e| format!("parse request_json: {e}"))?;
        let progress = self.start(orbital, request)?;
        serde_json::to_string(&ServerProgressWire::from(&progress))
            .map_err(|e| format!("serialize: {e}"))
    }

    /// [`Self::resume`] over JSON: the answers (`IoReplyWire[]`) in, a `ServerProgressWire` out.
    pub fn resume_json(&self, flight: u64, replies_json: &str) -> Result<String, String> {
        let replies: Vec<IoReplyWire> =
            serde_json::from_str(replies_json).map_err(|e| format!("parse replies_json: {e}"))?;
        let progress = self.resume_wire(flight, replies)?;
        serde_json::to_string(&ServerProgressWire::from(&progress))
            .map_err(|e| format!("serialize: {e}"))
    }

    /// The whole orbital's held states under `states`, the dispatched traits' new ones over them (the
    /// stateful host's contract: the response is the full picture, not only who ran).
    fn echo_held_states(
        &self,
        response: &mut EventResponse,
        seed_trait: &str,
        entity_id: Option<&str>,
    ) {
        let Some(orbital) = self.kernel.orbital_of_trait(seed_trait) else {
            return;
        };
        let Some(traits) = self.orbital_traits.get(&orbital) else {
            return;
        };
        for (name, state) in self.kernel.held_states(traits, entity_id) {
            response.states.entry(name).or_insert(state);
        }
    }

    fn settle(
        &self,
        progress: Progress<DispatchOutcomes>,
        seed: Seed,
        user: Value,
        discovery: Option<Discovery>,
    ) -> Result<ServerProgress, String> {
        match progress {
            Progress::Done(outcomes) => {
                let mut response = match discovery {
                    Some((handlers, held)) => {
                        EventResponse::from_discovery(&outcomes, &handlers, &seed.event, held)
                    }
                    None if seed.tick => {
                        EventResponse::from_process_results(&outcomes, "", &seed.event)
                    }
                    None => EventResponse::from_process_results(
                        &outcomes,
                        &seed.trait_name,
                        &seed.event,
                    ),
                };
                let hops = TransitionHop::of_outcomes(&outcomes);
                response.transitions = (!hops.is_empty()).then_some(hops);
                self.kernel.stamp_emitted_sources(&mut response);
                self.kernel
                    .echo_frames(&mut response, seed.entity_id.as_deref());
                self.echo_held_states(&mut response, &seed.trait_name, seed.entity_id.as_deref());
                Ok(ServerProgress::Done(response))
            }
            Progress::Await {
                continuation,
                requests,
            } => {
                let flight = {
                    let mut next = self.next_flight.lock();
                    let id = *next;
                    *next += 1;
                    id
                };
                self.flights.lock().insert(
                    flight,
                    Flight {
                        continuation,
                        seed,
                        user,
                        discovery,
                    },
                );
                Ok(ServerProgress::Await { flight, requests })
            }
        }
    }
}

fn dispatch_error(error: DispatchError) -> String {
    match error {
        DispatchError::Kernel(KernelError::TraitNotFound(name)) => {
            format!("Trait not found: {name}")
        }
        DispatchError::Kernel(other) => other.to_string(),
        DispatchError::Resume(resume) => resume.to_string(),
    }
}
