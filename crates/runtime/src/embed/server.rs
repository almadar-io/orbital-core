//! The server-role embedding: a `Server`-environment [`RuntimeKernel`] whose every read, write,
//! service call and substrate operator is an `Await` the host answers. A JavaScript host cannot block
//! on a promise, so it starts a dispatch, answers the requests it is handed (in any order, all at
//! once) and resumes. The continuation never leaves this module; only requests and answers cross.

use super::wire::{CallSiteWire, Settled, IoReplyWire, PendingRequestWire, ResolveProgressWire, ServerProgressWire};
use crate::effects::response::{RunTick, TraitFrom, TransitionHop};
use crate::effects::executor::{ServiceHandler, SubstrateHandler};
use crate::resumable::{completion_event, CallSite, DispatchContinuation, DispatchError, DispatchOutcomes, HostAnswered, IoReply, PendingRequest, Progress, ResolveContinuation, ResolveProgress};
use crate::row_query::RowPage;
use orbital_core_base::row_set::RowSetRef;
use crate::config::KernelConfig;
use crate::kernel::{KernelError, RuntimeKernel};
use crate::entity_source::EntitySource;
use crate::effects::response::{EventRequest, EventResponse};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::os::{OsError, OsOp};
use orbital_core_base::os_grants::{FsMode, OsGrants, OsRequirement};
use orbital_core_base::value::Value;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Where a server dispatch stands.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
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
    dispatch_stream: Mutex<Vec<crate::effects::live_push::StreamItem>>,
}

/// The row source and config of a server-role kernel whose host answers every ask.
fn host_answered() -> (Arc<dyn EntitySource>, KernelConfig) {
    let host = Arc::new(HostAnswered);
    let config = KernelConfig {
        environment: RuntimeEnvironment::Server,
        service_handler: Some(host.clone() as Arc<dyn ServiceHandler>),
        substrate_handler: Some(host.clone() as Arc<dyn SubstrateHandler>),
        runtime_rows_in_kernel: true,
        ..KernelConfig::default()
    };
    (host as Arc<dyn EntitySource>, config)
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
        let (host, config) = host_answered();
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, host, config);
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
            dispatch_stream: Mutex::new(Vec::new()),
        })
    }

    /// A server holding this one's registered program and none of its session ([`RuntimeKernel::fork_session`]):
    /// its viewer, mounted ticks and locales carry over, nothing a dispatch left does.
    pub fn fork_session(&self) -> Result<Self, String> {
        let (host, config) = host_answered();
        let kernel = self.kernel.fork_session(host, config).map_err(|e| format!("fork_session: {e}"))?;
        Ok(Self {
            kernel,
            orbital_traits: self.orbital_traits.clone(),
            default_user: Mutex::new(self.default_user.lock().clone()),
            tick_mount: Mutex::new(self.tick_mount.lock().clone()),
            flights: Mutex::new(HashMap::new()),
            resolves: Mutex::new(HashMap::new()),
            next_flight: Mutex::new(0),
            dispatch_stream: Mutex::new(Vec::new()),
        })
    }

    pub fn kernel(&self) -> &RuntimeKernel {
        &self.kernel
    }


    /// Mock-seed the `[runtime]` entities this kernel holds itself, owner columns stamped with `viewer_id`
    /// ([`RuntimeKernel::seed_runtime_mock`]). A preview host calls it on a fresh kernel.
    pub fn seed_runtime_mock(&self, viewer_id: Option<String>) {
        self.kernel.seed_runtime_mock(viewer_id);
    }
    /// Name the program whose declared credentials this server's credential effects manage
    /// ([`RuntimeKernel::set_managed_program`]).
    pub fn set_managed_program(&self, program: &serde_json::Value) {
        self.kernel.set_managed_program(program);
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
        let query: Option<crate::row_query::QueryCapabilities> =
            serde_json::from_str(query_json).map_err(|e| format!("parse query_json: {e}"))?;
        let write: Option<crate::row_query::WriteCapabilities> =
            serde_json::from_str(write_json).map_err(|e| format!("parse write_json: {e}"))?;
        if let Some(q) = query {
            self.kernel.set_query_capabilities(entity_type, q);
        }
        if let Some(w) = write {
            self.kernel.set_write_capabilities(entity_type, w);
        }
        Ok(())
    }

    /// Put `rows` into the kernel's own store for `entity_type`, each under its `id`: how a host or a test
    /// gives a `[runtime]` entity rows, which are never a host ask.
    pub fn seed_rows(&self, entity_type: &str, rows: &[Value]) -> Result<(), String> {
        for row in rows {
            let id = row
                .get_path("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("seed_rows: a {entity_type} row has no string id"))?;
            self.kernel.entity_source().write(entity_type, id, row)?;
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

    /// The state a trait instance holds: its current one, or its initial one while nothing has touched it (a trait that has
    /// not mounted yet is at its initial state, as the whole-orbital `states` of a response says); `""` for a trait the
    /// program does not declare.
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> String {
        self.kernel
            .held_states(&[trait_name.to_string()], entity_id)
            .remove(trait_name)
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
        self.kernel
            .payload_rejection(targets.iter().map(|(t, e)| (t.as_str(), e.as_str())), payload)
            .map(EventResponse::error)
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

    /// The program's `os/` grants the kernel holds.
    pub fn os_grants(&self) -> OsGrants {
        self.kernel.os_grants()
    }

    /// Whether the program's grants cover `requirement_json` (an `OsRequirement`).
    pub fn os_allows_json(&self, requirement_json: &str) -> Result<bool, String> {
        let need: OsRequirement = serde_json::from_str(requirement_json)
            .map_err(|e| format!("parse requirement_json: {e}"))?;
        Ok(self.os_grants().allows(&need))
    }

    pub fn live_item_delivered(&self, item_json: &str, viewer_json: &str, client_id: Option<&str>) -> Result<bool, String> {
        let item: crate::effects::LiveBroadcastItem = serde_json::from_str(item_json).map_err(|e| format!("parse item_json: {e}"))?;
        let viewer: Value = serde_json::from_str(viewer_json).map_err(|e| format!("parse viewer_json: {e}"))?;
        Ok(item.delivered_to(client_id, |item| viewer.is_null() || self.kernel.live_item_visible_to(item, &viewer)))
    }

    /// The last settled dispatch's ordered items; read before another dispatch starts.
    pub fn dispatch_stream_json(&self) -> Result<String, String> {
        serde_json::to_string(&*self.dispatch_stream.lock()).map_err(|e| format!("serialize: {e}"))
    }

    /// [`Self::start`] over JSON: the request in, a `ServerProgressWire` out.
    pub fn start_json(&self, orbital: &str, request_json: &str) -> Result<String, String> {
        let request: EventRequest =
            serde_json::from_str(request_json).map_err(|e| format!("parse request_json: {e}"))?;
        self.dispatch_stream.lock().clear();
        let progress = self.start(orbital, request)?;
        if let ServerProgress::Done(response) = &progress {
            let mut items = self.dispatch_stream.lock();
            if items.is_empty() { items.push(crate::effects::live_push::StreamItem::complete(response.clone())); }
        }
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
                *self.dispatch_stream.lock() = crate::effects::live_push::StreamItem::of_outcomes(&outcomes, &response);
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

/// The `os/` operations that may defer their completion (`OsOp::defers_completion`), as a JSON array of names.
pub fn os_deferring_ops_json() -> Result<String, String> {
    let names: Vec<&str> = OsOp::ALL.iter().filter(|op| op.defers_completion()).map(|op| op.name()).collect();
    serde_json::to_string(&names).map_err(|e| format!("serialize: {e}"))
}

/// The event a deferred keyed `os/` call's outcome fires on its call site (`completion_event`), as `{ event, payload }`
/// JSON, or `null` when the site declares no route for the outcome. `outcome_json` is a `Settled<_, OsError>`.
pub fn os_completion_event_json(site_json: &str, key: &str, outcome_json: &str) -> Result<String, String> {
    let site: CallSite = serde_json::from_str::<CallSiteWire>(site_json)
        .map_err(|e| format!("parse site_json: {e}"))?
        .into();
    let outcome: Result<Value, OsError> = serde_json::from_str::<Settled<Value, OsError>>(outcome_json)
        .map_err(|e| format!("parse outcome_json: {e}"))?
        .into();
    let fired = completion_event(&site, key, outcome).map(|(event, payload)| EventFired { event, payload });
    serde_json::to_string(&fired).map_err(|e| format!("serialize: {e}"))
}

pub fn substrate_completion_event_json(site_json: &str, key: &str, op: &str, outcome_json: &str) -> Result<String, String> {
    let site: CallSite = serde_json::from_str::<CallSiteWire>(site_json).map_err(|e| format!("parse site_json: {e}"))?.into();
    let outcome: Result<Value, super::wire::SubstrateFailureWire> = serde_json::from_str::<Settled<Value, super::wire::SubstrateFailureWire>>(outcome_json).map_err(|e| format!("parse outcome_json: {e}"))?.into();
    let outcome = outcome.map(|value| crate::agent_ports::completion_result(op, value)).map_err(|failure| crate::agent_ports::failure_payload(&crate::effects::executor::SubstrateFailure { message: failure.message, errors: failure.errors }));
    let fired = crate::resumable::substrate_completion_event(&site, key, outcome).map(|(event, payload)| EventFired { event, payload });
    serde_json::to_string(&fired).map_err(|e| format!("serialize: {e}"))
}

#[derive(serde::Serialize)]
struct EventFired {
    event: String,
    payload: Value,
}

/// Whether the one glob `glob` matches `path` lexically, as `os/glob` filters its results.
pub fn os_glob_matches(glob: &str, path: &str) -> bool {
    OsGrants::read_matcher(glob).fs_allows(std::path::Path::new(path), FsMode::Read)
}

/// Whether the Chrome match pattern `pattern` matches `url`: the one rule `browser/tab-list` filters by, and a browser
/// host attaches a `browser/tab-watch` to the tabs it names with.
pub fn tab_url_matches(pattern: &str, url: &str) -> bool {
    orbital_core_base::tab_call::matches_pattern(pattern, url)
}
