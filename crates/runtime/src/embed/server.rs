//! The server-role embedding: a `Server`-environment [`RuntimeKernel`] whose every read, write,
//! service call and substrate operator is an `Await` the host answers. A JavaScript host cannot block
//! on a promise, so it starts a dispatch, answers the requests it is handed (in any order, all at
//! once) and resumes. The continuation never leaves this module; only requests and answers cross.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::ticket_queue::TicketQueue;
use super::wire::{CallSiteWire, FiredTick, Settled, IoReplyWire, NextWire, PendingRequestWire, ResolveProgressWire, ServerProgressWire};
use crate::effects::response::{RunTick, TraitFrom, TransitionHop};
use crate::effects::executor::{ServiceHandler, SubstrateHandler};
use crate::resumable::{completion_event, CallSite, DispatchContinuation, DispatchError, DispatchOutcomes, HostAnswered, IoReply, PendingRequest, Progress, ResolveContinuation, ResolveProgress};
use crate::row_query::RowPage;
use orbital_core_base::row_set::RowSetRef;
use crate::config::KernelConfig;
use crate::tick_clock::{Calendar, TickClock};
use crate::kernel::{EntryError, FrameWrite, KernelError, RequestSession, RuntimeKernel, SessionActive, SessionStream};
use crate::entity_source::EntitySource;
use crate::effects::response::{EventRequest, EventResponse, LifecycleEvent};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::os::{OsError, OsOp};
#[cfg(feature = "std")]
use orbital_core_base::os_grants::{resolve_anchored_args, EnvSnapshot, FsMode, OsAnchors};
use orbital_core_base::os_grants::{OsGrants, OsRequirement};
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::sync::Mutex;
use std::collections::{BTreeMap, HashMap};
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
    /// A dispatch holds the lane, so the kernel queued the request in arrival order (EXEC-CONC-2); [`EmbeddedServer::next`]
    /// starts it, under this ticket, once the lane frees.
    Queued {
        ticket: u64,
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
            ServerProgress::Queued { ticket } => ServerProgressWire::Queued { ticket: *ticket },
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
    /// A `request.mount` dispatch: its rows are whole and seed the viewer's frames (EXEC-DIST-7).
    mount: bool,
    /// The rows a reviving mount carried, echoed as the client sent them (EXEC-DIST-9).
    carried_rows: Option<ValueMap>,
}

struct Flight {
    continuation: DispatchContinuation,
    seed: Seed,
    user: Value,
    /// The client session the dispatch runs in, held active until it ends (EXEC-DIST-9).
    session: Option<SessionActive>,
    /// Set for an untargeted request: the traits it was delivered to and the held state of the orbital's.
    discovery: Option<Discovery>,
}

type Discovery = (Vec<String>, HashMap<String, String>);

/// A request's session as admitted when it arrived: the client's hold, and whether the request revived it.
type Admitted = (Option<SessionActive>, bool);

/// Work that arrived while a dispatch held the lane, waiting its turn: a new request, or the answers of a dispatch
/// parked without the lane (a keyed ask), whose resumption enters the lane like any other dispatch.
enum Waiting {
    Start { orbital: String, request: Box<EventRequest>, admitted: Admitted },
    Resume { flight: u64, replies: Vec<IoReply> },
}

fn clock_of(kernel: &RuntimeKernel) -> Mutex<TickClock> {
    Mutex::new(TickClock::from_programs(&kernel.registered_programs()))
}

/// A server-role kernel and the dispatches it has in flight.
pub struct EmbeddedServer {
    kernel: RuntimeKernel,
    orbital_traits: HashMap<String, Vec<String>>,
    default_user: Mutex<Value>,
    /// The coalesced relay of tick-stamped dispatches to the other clients.
    tick_relay: Mutex<super::tick_relay::TickRelay>,
    /// Which declared ticks are due on each pass ([`Self::tick_pass`]), the clock every host runs.
    ticks: Mutex<TickClock>,
    flights: Mutex<HashMap<u64, Flight>>,
    /// The live streams the host reported open, by the id it closes them with: while one is open its session lives.
    streams: Mutex<HashMap<u64, SessionStream>>,
    /// The requests waiting for the lane, in arrival order: the kernel, not the host, decides what runs next.
    queued: Mutex<TicketQueue<Waiting>>,
    /// Row-set resolves waiting on the host; they share `next_flight`'s ids with the dispatches.
    resolves: Mutex<HashMap<u64, ResolveContinuation>>,
    next_flight: Mutex<u64>,
    dispatch_stream: Mutex<Vec<crate::effects::live_push::StreamItem>>,
    /// Where the host places the program's anchored grants and arguments; none until it says.
    #[cfg(feature = "std")]
    os_anchors: Mutex<OsAnchors>,
}

#[cfg(feature = "std")]
/// A host's anchors as it sends them: its home and start directories, its executable's directory and its boot
/// environment.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OsAnchorsWire {
    #[serde(default)]
    home: Option<std::path::PathBuf>,
    #[serde(default)]
    start: Option<std::path::PathBuf>,
    #[serde(default)]
    exe_dir: Option<std::path::PathBuf>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
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
    pub fn new(schema: OrbitalSchema) -> Result<Self, EntryError> {
        Self::with_lazy_source(schema, None)
    }

    /// [`Self::new`] for a program whose `uses lazy` orbitals the kernel loads through `lazy` on the first dispatch that
    /// reaches them (EXEC-LOAD-2). Without a source a lazy page is the client's to open.
    pub fn with_lazy_source(
        schema: OrbitalSchema,
        lazy: Option<Arc<dyn crate::kernel::lazy::LazySource>>,
    ) -> Result<Self, EntryError> {
        if schema.orbitals.is_empty() {
            return Err(EntryError::NoOrbitals);
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
        if let Some(lazy) = lazy {
            kernel.set_lazy_source(lazy);
        }
        kernel
            .register_schema(schema)
            .map_err(EntryError::Register)?;
        let default_user = kernel.get_current_user();
        Ok(Self {
            ticks: clock_of(&kernel),
            kernel,
            orbital_traits,
            default_user: Mutex::new(default_user),
            tick_relay: Mutex::new(Default::default()),
            flights: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            queued: Mutex::new(TicketQueue::default()),
            resolves: Mutex::new(HashMap::new()),
            next_flight: Mutex::new(0),
            dispatch_stream: Mutex::new(Vec::new()),
            #[cfg(feature = "std")]
            os_anchors: Mutex::new(OsAnchors::default()),
        })
    }

    /// A server holding this one's registered program and none of its session ([`RuntimeKernel::fork_session`]):
    /// its viewer, mounted ticks and locales carry over, nothing a dispatch left does.
    pub fn fork_session(&self) -> Result<Self, EntryError> {
        let (host, config) = host_answered();
        let kernel = self.kernel.fork_session(host, config).map_err(EntryError::ForkSession)?;
        Ok(Self {
            ticks: clock_of(&kernel),
            kernel,
            orbital_traits: self.orbital_traits.clone(),
            default_user: Mutex::new(self.default_user.lock().clone()),
            tick_relay: Mutex::new(Default::default()),
            flights: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            queued: Mutex::new(TicketQueue::default()),
            resolves: Mutex::new(HashMap::new()),
            next_flight: Mutex::new(0),
            dispatch_stream: Mutex::new(Vec::new()),
            #[cfg(feature = "std")]
            os_anchors: Mutex::new(OsAnchors::default()),
        })
    }

    /// A fresh control program using this app's entity policies; app routes, sessions and grants are not inherited.
    pub fn fork_program(&self, schema: OrbitalSchema) -> Result<Self, EntryError> {
        if schema.orbitals.is_empty() {
            return Err(EntryError::NoOrbitals);
        }
        let orbital_traits = schema
            .orbitals
            .iter()
            .map(|orbital| {
                (
                    orbital.name.clone(),
                    orbital
                        .traits
                        .iter()
                        .filter_map(|t| t.trait_name().map(str::to_string))
                        .collect(),
                )
            })
            .collect();
        let (host, config) = host_answered();
        let kernel = self
            .kernel
            .program_context(host, config, schema)
            .map_err(EntryError::ForkProgram)?;
        Ok(Self {
            ticks: clock_of(&kernel),
            kernel,
            orbital_traits,
            default_user: Mutex::new(self.default_user.lock().clone()),
            tick_relay: Mutex::new(Default::default()),
            flights: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            queued: Mutex::new(TicketQueue::default()),
            resolves: Mutex::new(HashMap::new()),
            next_flight: Mutex::new(0),
            dispatch_stream: Mutex::new(Vec::new()),
            #[cfg(feature = "std")]
            os_anchors: Mutex::new(OsAnchors::default()),
        })
    }

    pub fn kernel(&self) -> &RuntimeKernel {
        &self.kernel
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
        if let Some(misfit) = self.kernel.rows_misfit(entity_type, rows) {
            return Err(format!("seed_rows: {misfit}"));
        }
        for row in rows {
            let id = row
                .get_path("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("seed_rows: a {entity_type} row has no string id"))?;
            self.kernel.entity_source().write(entity_type, id, row)?;
        }
        Ok(())
    }

    /// One pass of the tick clock at monotonic `now_ms`: the `(orbital, trait, tick)` triples due now that this server
    /// runs, the `[background]` ticks and those of a trait some client has mounted ([`Self::mount_client`]). The host
    /// starts each as a `runTick` request to the orbital named. Same selection as the native host's tick pass for
    /// `[background]` ticks.
    pub fn tick_pass(&self, now_ms: u64) -> BTreeMap<String, Vec<FiredTick>> {
        let mut clock = self.ticks.lock();
        let due = clock.due(now_ms, Calendar::local_now);
        let mut by_orbital: BTreeMap<String, Vec<FiredTick>> = BTreeMap::new();
        for (trait_name, tick) in due {
            if !(self.kernel.trait_mounted(&trait_name) || self.kernel.tick_of(&trait_name, &tick).is_some_and(|t| t.background)) {
                continue;
            }
            let Some(orbital) = clock.orbital_of(&trait_name, &tick) else { continue };
            let entry = FiredTick { trait_name, tick };
            match by_orbital.get_mut(orbital) {
                Some(ticks) => ticks.push(entry),
                None => {
                    by_orbital.insert(orbital.to_string(), vec![entry]);
                }
            }
        }
        by_orbital
    }

    /// Whether the program declares any tick: a host runs its timer only then.
    pub fn declares_ticks(&self) -> bool {
        !self.ticks.lock().is_empty()
    }

    /// Stop the tick clock; declared ticks and their phase are kept.
    pub fn tick_pause(&self) {
        self.ticks.lock().pause();
    }

    /// Restart the tick clock; the paused span is never counted.
    pub fn tick_resume(&self) {
        self.ticks.lock().resume();
    }

    /// Whether the tick clock is stopped.
    pub fn tick_paused(&self) -> bool {
        self.ticks.lock().is_paused()
    }

    /// This host holds the circuit state ([`RuntimeKernel::set_stateful`]).
    pub fn set_stateful(&self, stateful: bool) {
        self.kernel.set_stateful(stateful);
    }

    /// `client` has mounted `traits` of `orbital`: their mount-scoped ticks fire while any client holds them. A request
    /// derives this itself ([`Self::start`]); a host calls it for a mount it makes outside a request.
    pub fn mount_client(&self, client: &str, orbital: &str, traits: Vec<String>) {
        self.kernel.mount_client(client, orbital, traits);
    }

    /// `client` has unmounted `traits` of `orbital`.
    pub fn unmount_client(&self, client: &str, orbital: &str, traits: Vec<String>) {
        self.kernel.unmount_client(client, orbital, traits);
    }

    /// `client` is gone: everything it had mounted is released. A session the kernel frees (EXEC-DIST-9) releases its
    /// client the same way.
    pub fn release_client(&self, client: &str) {
        self.kernel.release_client(client);
    }

    /// Queue the tick-stamped dispatch `request` of `orbital` for the coalesced peer relay: newest per (origin client,
    /// orbital, event) wins. `Some(ms)` when it opened a window, which the host flushes after that long
    /// ([`Self::flush_tick_relay`]); `None` for a request with no tick or one joining an open window.
    pub fn relay_tick(&self, orbital: &str, request: &EventRequest) -> Option<u64> {
        self.tick_relay.lock().queue(orbital, request)
    }

    /// The window's cadence in ms; the default is 50 (≈20 Hz).
    pub fn set_tick_relay_interval(&self, interval_ms: u64) {
        self.tick_relay.lock().set_interval(interval_ms);
    }

    /// Every item queued since the window opened, in first-arrival order; the window closes.
    pub fn flush_tick_relay(&self) -> Vec<crate::effects::LiveBroadcastItem> {
        self.tick_relay.lock().flush()
    }

    /// The kernel's mounts for a host start on `side` (EXEC-ENDPOINT-3).
    pub fn boot_mounts(&self, side: orbital_core_base::protocols::ProtocolSide) -> Vec<crate::effects::response::MountSeed> {
        self.kernel.boot_mounts(side)
    }

    /// The answer to one [`crate::kernel::contract_query::ContractQuery`] about the registered program, as JSON.
    pub fn contract_query(&self, query: crate::kernel::contract_query::ContractQuery) -> Result<serde_json::Value, String> {
        self.kernel.contract_query(query)
    }

    /// The kernel's mounts for a client opening `route_path` (EXEC-ENDPOINT-3).
    pub fn page_circuit(&self, page: Option<&str>, route: Option<&str>) -> Result<crate::kernel::page_circuit::PageCircuit, String> {
        self.kernel.page_circuit(page, route)
    }

    pub fn route_mounts(&self, route_path: &str) -> Vec<crate::effects::response::MountSeed> {
        self.kernel.route_mounts(route_path)
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
        self.queued.lock().retain(|work| !matches!(work, Waiting::Resume { flight: waiting, .. } if *waiting == flight));
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
                let flight = self.next_id();
                self.resolves.lock().insert(flight, continuation);
                ServerResolve::Await { flight, requests }
            }
        }
    }

    /// Start `request` against `orbital`. A delegated leg (`traits` / `entityByTrait`) first seeds the client's
    /// states and rows. A mount batch seeds each lifecycle event; otherwise the target is `targetTrait`, else
    /// every declared trait of `traits`, else every trait of the orbital whose held state handles the event.
    ///
    /// While a parked dispatch holds the lane, or earlier requests still wait for it, the request is queued behind
    /// them and answered `Queued`; the host starts nothing itself (EXEC-CONC-2).
    pub fn start(&self, orbital: &str, request: EventRequest) -> Result<ServerProgress, String> {
        if let Some(refused) = self.kernel.refusal(&request) {
            return Ok(ServerProgress::Done(refused));
        }
        if self.record_mount(orbital, &request) {
            return Ok(ServerProgress::Done(EventResponse::success("")));
        }
        let admitted = self.admit(&request)?;
        let mut queued = self.queued.lock();
        if self.lane_held() || !queued.is_empty() {
            let ticket = self.next_id();
            queued.push(ticket, Waiting::Start { orbital: orbital.to_string(), request: Box::new(request), admitted });
            return Ok(ServerProgress::Queued { ticket });
        }
        drop(queued);
        self.start_now(orbital, request, admitted)
    }

    /// The client's mount ledger follows its requests (EXEC-TICK-4): a mount batch or a lifecycle event mounts the traits
    /// it names for the client, `$UNMOUNT` releases them. A request naming no client is no client's mount (the host's own
    /// boot mount). Answers whether the request was an unmount, which dispatches nothing.
    fn record_mount(&self, orbital: &str, request: &EventRequest) -> bool {
        let unmount = request.event == orbital_core_base::bus_event::UNMOUNT_EVENT;
        let Some(client) = request.client_id.as_deref() else {
            return unmount;
        };
        let traits: Vec<String> = match request.mount.as_ref() {
            Some(seeds) => seeds.iter().map(|seed| seed.r#trait.clone()).collect(),
            None => request.target_trait.iter().cloned().collect(),
        };
        if unmount {
            self.kernel.unmount_client(client, orbital, traits);
        } else if request.mount.is_some() || LifecycleEvent::parse(&request.event).is_some() {
            self.kernel.mount_client(client, orbital, traits);
        }
        unmount
    }

    /// Admit `request` into its session as it arrives (EXEC-DIST-9), so a request waiting for the lane holds it.
    fn admit(&self, request: &EventRequest) -> Result<Admitted, String> {
        let mounting = request.mount.as_ref().is_some_and(|seeds| !seeds.is_empty());
        match self
            .kernel
            .admit_request(request.client_id.as_deref(), mounting, false)
            .map_err(|ended| format!("SESSION_ENDED: {ended}"))?
        {
            RequestSession::Client { active, revived } => Ok((Some(active), revived)),
            RequestSession::Program | RequestSession::Scratch(_) => Ok((None, false)),
        }
    }

    /// The oldest queued request, started now that no dispatch holds the lane, with the ticket it was queued under;
    /// `None` while the lane is held or nothing waits. The host calls it after each dispatch ends until it is `None`.
    pub fn next(&self) -> Option<(u64, Result<ServerProgress, String>)> {
        if self.lane_held() {
            return None;
        }
        let (ticket, work) = self.queued.lock().pop_front()?;
        Some((ticket, match work {
            Waiting::Start { orbital, request, admitted } => self.start_now(&orbital, *request, admitted),
            Waiting::Resume { flight, replies } => self.resume_now(flight, replies),
        }))
    }

    /// A live stream of `client`'s session opened (EXEC-DIST-9): the session is not freed while one is open. Answers the
    /// id the host closes it with.
    pub fn open_session_stream(&self, client: &str) -> u64 {
        let id = self.next_id();
        self.streams.lock().insert(id, self.kernel.open_session_stream(client));
        id
    }

    /// The stream `id` dropped; once a session's streams have all dropped and it is quiet, the kernel frees it.
    pub fn close_session_stream(&self, id: u64) -> bool {
        let stream = self.streams.lock().remove(&id);
        stream.is_some()
    }

    /// Whether `flight` is a dispatch parked holding the lane: a dispatch started on its behalf would wait on itself,
    /// so the host refuses it (EXEC-CONC-0).
    pub fn holds_lane(&self, flight: u64) -> bool {
        self.flights.lock().get(&flight).is_some_and(|f| f.continuation.turn().is_some())
    }

    fn lane_held(&self) -> bool {
        self.flights.lock().values().any(|f| f.continuation.turn().is_some())
    }

    fn next_id(&self) -> u64 {
        let mut next = self.next_flight.lock();
        let id = *next;
        *next += 1;
        id
    }

    fn start_now(&self, orbital: &str, request: EventRequest, (session, revived): Admitted) -> Result<ServerProgress, String> {
        self.dispatch_stream.lock().clear();
        let lazy_traits;
        let traits = match self.orbital_traits.get(orbital) {
            Some(traits) => traits,
            None => {
                self.kernel.ensure_orbital_loaded(orbital).map_err(|e| e.to_string())?;
                lazy_traits = self
                    .kernel
                    .get_orbital(orbital)
                    .ok_or_else(|| format!("Orbital not found: {orbital}"))?
                    .traits;
                &lazy_traits
            }
        };
        let _in_session = session.as_ref().map(|active| self.kernel.enter_session(active.session()));
        if let Some(since) = request.frames_since {
            let frames = self.kernel.frames_since(since);
            return Ok(ServerProgress::Done(match frames {
                Some(frames) => EventResponse { success: true, frames: Some(frames), ..Default::default() },
                None => EventResponse::error("this host keeps no frames for the session: mount again"),
            }));
        }
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
            return self.start_tick(run, entity_id, &request.event, user, session);
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
            let carried = if revived {
                RuntimeKernel::carried_traits(request.traits.as_deref(), request.entity_by_trait.as_ref())
            } else {
                Default::default()
            };
            let carried_rows = revived.then(|| {
                request
                    .entity_by_trait
                    .iter()
                    .flatten()
                    .filter(|(name, _)| seeds.iter().any(|seed| &seed.r#trait == *name))
                    .map(|(name, row)| (name.clone(), row.clone()))
                    .collect::<ValueMap>()
            });
            let unseeded: Vec<_> = seeds.iter().filter(|seed| !carried.contains(&seed.r#trait)).cloned().collect();
            let progress = if unseeded.is_empty() {
                Progress::Done(Vec::new())
            } else {
                self.kernel
                    .start_dispatch_mount(&unseeded, entity_id, payload)
                    .map_err(dispatch_error)?
            };
            let seed = Seed {
                trait_name: seed_trait,
                event: seed_event,
                tick: false,
                entity_id: entity_id.map(str::to_string),
                mount: true,
                carried_rows,
            };
            return self.settle(progress, seed, user, None, session);
        }
        if request.target_trait.is_none() {
            if let Some(declared) = request.traits.as_ref() {
                return self.start_declared(declared, entity_id, &request, payload, user, session);
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
                mount: false,
                carried_rows: None,
            };
            return self.settle(progress, seed, user, Some((handlers, held)), session);
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
            mount: false,
            carried_rows: None,
        };
        self.settle(progress, seed, user, None, session)
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
        session: Option<SessionActive>,
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
            mount: false,
            carried_rows: None,
        };
        self.settle(progress, seed, user, None, session)
    }

    fn start_tick(
        &self,
        run: &RunTick,
        entity_id: Option<&str>,
        event: &str,
        user: Value,
        session: Option<SessionActive>,
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
        if !background && !self.kernel.trait_mounted(&run.trait_name) {
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
            mount: false,
            carried_rows: None,
        };
        self.settle(progress, seed, user, None, session)
    }

    /// Go on with `flight`, given an answer to each request it is waiting on. A refused set of
    /// answers ends the dispatch: the host asked for something it cannot supply.
    ///
    /// A dispatch parked without the lane resumes only in its turn: while another holds the lane, or earlier work
    /// waits, its answers are queued and the kernel resumes it from [`Self::next`] (EXEC-CONC-1).
    pub fn resume(&self, flight: u64, replies: Vec<IoReply>) -> Result<ServerProgress, String> {
        let mut queued = self.queued.lock();
        if !self.holds_lane(flight) && (self.lane_held() || !queued.is_empty()) {
            if !self.flights.lock().contains_key(&flight) {
                return Err(format!("no dispatch is waiting as flight {flight}"));
            }
            let ticket = self.next_id();
            queued.push(ticket, Waiting::Resume { flight, replies });
            return Ok(ServerProgress::Queued { ticket });
        }
        drop(queued);
        self.resume_now(flight, replies)
    }

    fn resume_now(&self, flight: u64, replies: Vec<IoReply>) -> Result<ServerProgress, String> {
        let Flight {
            continuation,
            seed,
            user,
            session,
            discovery,
        } = self
            .flights
            .lock()
            .remove(&flight)
            .ok_or_else(|| format!("no dispatch is waiting as flight {flight}"))?;
        self.kernel.set_current_user(user.clone());
        let _in_session = session.as_ref().map(|active| self.kernel.enter_session(active.session()));
        let progress = self
            .kernel
            .resume(continuation, replies)
            .map_err(dispatch_error)?;
        self.settle(progress, seed, user, discovery, session)
    }

    /// [`Self::resume`] with the wire's answers.
    pub fn resume_wire(
        &self,
        flight: u64,
        replies: Vec<IoReplyWire>,
    ) -> Result<ServerProgress, String> {
        self.resume(flight, replies.into_iter().map(IoReply::from).collect())
    }

    /// Finish a TLS operation the kernel parked on a deferred raw ask (EXEC-HTTP-7): `outcome_json` is the host's
    /// `Settled<_, OsError>` for the raw ask under `key`. Answers `null` when `key` parks nothing, `{ "ask": { op,
    /// args } }` when the host must answer one more raw ask (call again with its outcome), or `{ "done": Settled }`
    /// with the program's outcome.
    pub fn tls_complete_json(&self, key: &str, outcome_json: &str) -> Result<String, String> {
        let outcome: Result<Value, OsError> = serde_json::from_str::<Settled<Value, OsError>>(outcome_json)
            .map_err(|e| format!("parse outcome_json: {e}"))?
            .into();
        #[cfg(feature = "wire-drivers")]
        let step = self.kernel.tls_complete(key, outcome).map(|completion| match completion {
            crate::kernel::TlsCompletion::Ask { op, args } => serde_json::json!({ "ask": { "op": op, "args": args } }),
            crate::kernel::TlsCompletion::Done(done) => serde_json::json!({ "done": Settled::from(done) }),
        });
        #[cfg(not(feature = "wire-drivers"))]
        let step: Option<serde_json::Value> = {
            let _ = (key, outcome);
            None
        };
        serde_json::to_string(&step).map_err(|e| format!("serialize: {e}"))
    }

    /// The program's `os/` grants the kernel holds.
    /// The program's grants with every anchored entry resolved under the host's anchors.
    #[cfg(feature = "std")]
    pub fn os_grants(&self) -> OsGrants {
        self.kernel.os_grants().resolve_anchors(&self.os_anchors.lock()).0
    }

    /// The program's grants as declared: a bare-metal host has no filesystem anchors to place.
    #[cfg(not(feature = "std"))]
    pub fn os_grants(&self) -> OsGrants {
        self.kernel.os_grants()
    }

    #[cfg(feature = "std")]
    /// Place the program's anchored grants and arguments under the host's anchors (`OsAnchorsWire` JSON); answers the
    /// entries that grant nothing under them, as `[{ path, message }]`.
    pub fn set_os_anchors_json(&self, anchors_json: &str) -> Result<String, String> {
        let wire: OsAnchorsWire = serde_json::from_str(anchors_json).map_err(|e| format!("parse anchors_json: {e}"))?;
        let anchors = OsAnchors { home: wire.home, start: wire.start, exe_dir: wire.exe_dir, env: EnvSnapshot::from_pairs(wire.env) };
        let (_, problems) = self.kernel.os_grants().resolve_anchors(&anchors);
        *self.os_anchors.lock() = anchors;
        let problems: Vec<serde_json::Value> =
            problems.into_iter().map(|p| serde_json::json!({ "path": p.path, "message": p.message })).collect();
        serde_json::to_string(&problems).map_err(|e| format!("serialize: {e}"))
    }

    #[cfg(feature = "std")]
    /// The `op` call's arguments with every anchored path, `cwd` and `env` host or port replaced by what it names under
    /// the host's anchors, as `Settled<args, OsError>` JSON.
    pub fn os_resolve_args_json(&self, op: &str, args_json: &str) -> Result<String, String> {
        let op = OsOp::from_name(op).ok_or_else(|| format!("{op} is not an os operation"))?;
        let args: Vec<Value> = serde_json::from_str(args_json).map_err(|e| format!("parse args_json: {e}"))?;
        let resolved: Settled<Vec<Value>, OsError> = resolve_anchored_args(op, &args, &self.os_anchors.lock()).into();
        serde_json::to_string(&resolved).map_err(|e| format!("serialize: {e}"))
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

    /// The last settled dispatch's ordered event and effect items; read before another dispatch starts. The response
    /// crosses once, in the dispatch's own answer: the stream carries no `complete`.
    pub fn dispatch_stream_json(&self) -> Result<String, String> {
        serde_json::to_string(&*self.dispatch_stream.lock()).map_err(|e| format!("serialize: {e}"))
    }

    /// [`Self::start`] over JSON: the request in, a `ServerProgressWire` out.
    pub fn start_json(&self, orbital: &str, request_json: &str) -> Result<String, String> {
        let request: EventRequest =
            serde_json::from_str(request_json).map_err(|e| format!("parse request_json: {e}"))?;
        let progress = self.start(orbital, request)?;
        serde_json::to_string(&self.started_wire(&progress)).map_err(|e| format!("serialize: {e}"))
    }

    /// [`Self::next`] over JSON: `null` while nothing can start, else a `NextWire` naming the ticket.
    pub fn next_json(&self) -> Result<String, String> {
        let next = self.next().map(|(ticket, started)| match started {
            Ok(progress) => NextWire::Started { ticket, progress: self.started_wire(&progress) },
            Err(error) => NextWire::Failed { ticket, error },
        });
        serde_json::to_string(&next).map_err(|e| format!("serialize: {e}"))
    }

    /// The wire form of a dispatch that just started.
    fn started_wire(&self, progress: &ServerProgress) -> ServerProgressWire {
        ServerProgressWire::from(progress)
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
        let traits = match self.orbital_traits.get(&orbital) {
            Some(traits) => traits.clone(),
            None => match self.kernel.get_orbital(&orbital) {
                Some(detail) => detail.traits,
                None => return,
            },
        };
        for (name, state) in self.kernel.held_states(&traits, entity_id) {
            response.states.entry(name).or_insert(state);
        }
    }

    fn settle(
        &self,
        progress: Progress<DispatchOutcomes>,
        seed: Seed,
        user: Value,
        discovery: Option<Discovery>,
        session: Option<SessionActive>,
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
                let mut writes = self.kernel.frame_writes(&outcomes, seed.entity_id.as_deref(), seed.mount);
                writes.extend(seed.carried_rows.iter().flatten().filter_map(|(trait_name, row)| {
                    Some((self.kernel.trait_frame_key(trait_name), FrameWrite::Fields(row.as_object()?.clone())))
                }));
                self.kernel.seal_frames(&mut response, writes);
                self.echo_held_states(&mut response, &seed.trait_name, seed.entity_id.as_deref());
                *self.dispatch_stream.lock() = crate::effects::live_push::StreamItem::of_outcomes(&outcomes);
                Ok(ServerProgress::Done(response))
            }
            Progress::Await {
                continuation,
                requests,
            } => {
                let flight = self.next_id();
                self.flights.lock().insert(
                    flight,
                    Flight {
                        continuation,
                        seed,
                        user,
                        session,
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

/// The `os/` operations whose keyed call is a stream (`OsOp::is_stream`): answered once in place, then each message
/// delivered to its call site's `onMessage` route (EXEC-QUEUE-12), as a JSON array of names.
pub fn os_stream_ops_json() -> Result<String, String> {
    let names: Vec<&str> = OsOp::ALL.iter().filter(|op| op.is_stream()).map(|op| op.name()).collect();
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

pub fn substrate_completion_event_json(site_json: &str, key: &str, op: &str, args_json: &str, outcome_json: &str) -> Result<String, String> {
    let site: CallSite = serde_json::from_str::<CallSiteWire>(site_json).map_err(|e| format!("parse site_json: {e}"))?.into();
    let args: Vec<Value> = serde_json::from_str(args_json).map_err(|e| format!("parse args_json: {e}"))?;
    let outcome: Result<Value, super::wire::SubstrateFailureWire> = serde_json::from_str::<Settled<Value, super::wire::SubstrateFailureWire>>(outcome_json).map_err(|e| format!("parse outcome_json: {e}"))?.into();
    let outcome = outcome
        .map_err(|failure| crate::effects::executor::SubstrateFailure { message: failure.message, errors: failure.errors })
        .and_then(|value| crate::agent_ports::program_answer(op, &args, value))
        .map(|value| crate::agent_ports::completion_result(op, value))
        .map_err(|failure| crate::agent_ports::failure_payload(&failure));
    let fired = crate::resumable::substrate_completion_event(&site, key, outcome).map(|(event, payload)| EventFired { event, payload });
    serde_json::to_string(&fired).map_err(|e| format!("serialize: {e}"))
}

#[derive(serde::Serialize)]
struct EventFired {
    event: String,
    payload: Value,
}

#[cfg(feature = "std")]
/// Whether the one glob `glob` matches `path` lexically, as `os/glob` filters its results.
pub fn os_glob_matches(glob: &str, path: &str) -> bool {
    OsGrants::read_matcher(glob).fs_allows(std::path::Path::new(path), FsMode::Read)
}

/// Whether the Chrome match pattern `pattern` matches `url`: the one rule `browser/tab-list` filters by, and a browser
/// host attaches a `browser/tab-watch` to the tabs it names with.
pub fn tab_url_matches(pattern: &str, url: &str) -> bool {
    orbital_core_base::tab_call::matches_pattern(pattern, url)
}

#[cfg(test)]
mod program_context_tests {
    use super::*;
    use serde_json::{json, Value as Json};

    fn program(
        name: &str,
        orbital: &str,
        entity: &str,
        trait_name: &str,
        effect: Json,
    ) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name":name, "typeContractVersion": 1, "orbitals":[{"name":orbital,"entity":{"name":entity,"persistence":"runtime","fields":[{"name":"id","type":"string"}]},
            "traits":[{"name":trait_name,"linkedEntity":entity,"stateMachine":{"states":[{"name":"idle","isInitial":true},{"name":"done"}],"transitions":[{"from":"idle","to":"done","event":"GO","effects":[effect]}]}}]}]
        })).unwrap()
    }

    fn app() -> EmbeddedServer {
        let mut schema = program(
            "App",
            "AppOrbital",
            "Note",
            "AppTrait",
            json!(["emit", "APP_DONE", {}]),
        );
        let orbital_core_base::schema::types::EntityRef::Inline(entity) =
            &mut schema.orbitals[0].entity
        else {
            panic!("inline entity")
        };
        entity.collection = Some("notes".to_string());
        let policy = serde_json::from_value(json!([
            "=",
            ["object/get", "@entity", "ownerId"],
            "@user.id"
        ]))
        .unwrap();
        entity.read_policy = Some(policy);
        entity.create_policy = entity.read_policy.clone();
        schema.os_grants = Some(serde_json::from_value(json!({"exec":["parent-only"]})).unwrap());
        schema.egress = Some(vec!["https://parent.example".to_string()]);
        EmbeddedServer::new(schema).unwrap()
    }

    fn request(trait_name: &str) -> EventRequest {
        EventRequest {
            event: "GO".to_string(),
            target_trait: Some(trait_name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn control_checks_app_policies_without_inheriting_routes_or_grants() {
        let parent = app();
        for action in ["create", "read"] {
            for (viewer, allowed) in [
                (json!({"id":"owner"}), true),
                (json!({"id":"other"}), false),
                (Json::Null, false),
            ] {
                let effect = json!(["access-check",{"entity":"Note","action":action,"row":{"ownerId":"owner"},"viewer":viewer},{"emit":{"success":"CHECKED","failure":"FAILED"}}]);
                let mut control = program(
                    "Control",
                    "ControlOrbital",
                    "ControlRow",
                    "ControlTrait",
                    effect,
                );
                control.os_grants =
                    Some(serde_json::from_value(json!({"exec":["child-only"]})).unwrap());
                let child = parent.fork_program(control).unwrap();
                let ServerProgress::Done(response) = child
                    .start("ControlOrbital", request("ControlTrait"))
                    .unwrap()
                else {
                    panic!("access-check must not ask host")
                };
                let checked = response
                    .emitted_events
                    .iter()
                    .find(|e| e.event == "CHECKED")
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&checked.payload).unwrap(),
                    json!({"allowed":allowed})
                );
                assert!(child.kernel.os_grants().exec_allows("child-only"));
                assert!(!child.kernel.os_grants().exec_allows("parent-only"));
                assert!(child.kernel.egress().is_empty());
                assert!(child.kernel.get_orbital("AppOrbital").is_none());
                assert!(child.kernel.get_initial_state("AppTrait").is_none());
            }
        }
        assert!(parent.kernel.os_grants().exec_allows("parent-only"));
        assert!(!parent.kernel.os_grants().exec_allows("child-only"));
        assert!(parent.kernel.get_orbital("ControlOrbital").is_none());
        assert_eq!(parent.get_state("AppTrait", None), "idle");
        assert_eq!(parent.in_flight(), 0);
        assert!(parent.dispatch_stream.lock().is_empty());
    }

    #[test]
    fn collisions_are_refused_without_mutating_parent() {
        let parent = app();
        let plain = || {
            program(
                "Control",
                "ControlOrbital",
                "ControlRow",
                "ControlTrait",
                json!(["emit", "DONE", {}]),
            )
        };
        let mut cases = Vec::new();
        let mut schema = plain();
        schema.name = "App".to_string();
        cases.push((schema, "schema"));
        let mut schema = plain();
        schema.orbitals[0].name = "AppOrbital".to_string();
        cases.push((schema, "orbital"));
        let mut schema = plain();
        let orbital_core_base::schema::types::TraitReference::Inline(def) =
            &mut schema.orbitals[0].traits[0]
        else {
            panic!("inline trait")
        };
        std::sync::Arc::make_mut(def).name = "AppTrait".to_string();
        cases.push((schema, "trait"));
        let mut schema = plain();
        let orbital_core_base::schema::types::EntityRef::Inline(def) =
            &mut schema.orbitals[0].entity
        else {
            panic!("inline entity")
        };
        def.name = "Note".to_string();
        cases.push((schema, "entity"));
        let mut schema = plain();
        let orbital_core_base::schema::types::EntityRef::Inline(def) =
            &mut schema.orbitals[0].entity
        else {
            panic!("inline entity")
        };
        def.collection = Some("notes".to_string());
        cases.push((schema, "collection"));
        for (schema, kind) in cases {
            let error = match parent.fork_program(schema) {
                Ok(_) => panic!("collision accepted"),
                Err(error) => error,
            };
            assert!(
                matches!(&error, EntryError::ForkProgram(KernelError::Internal(m)) if m.contains(&format!("{kind} collision"))),
                "{error}"
            );
            assert!(parent.kernel.get_orbital("ControlOrbital").is_none());
            assert_eq!(parent.get_state("AppTrait", None), "idle");
        }
        assert!(parent.fork_program(plain()).is_ok());
    }

    #[test]
    fn plain_fork_keeps_existing_program_and_resets_session() {
        let parent = app();
        let ServerProgress::Done(_) = parent.start("AppOrbital", request("AppTrait")).unwrap()
        else {
            panic!("emit is local")
        };
        assert_eq!(parent.get_state("AppTrait", None), "done");
        let child = parent.fork_session().unwrap();
        assert_eq!(child.get_state("AppTrait", None), "idle");
        assert!(child.kernel.get_orbital("AppOrbital").is_some());
        assert!(child.kernel.os_grants().exec_allows("parent-only"));
        assert_eq!(parent.get_state("AppTrait", None), "done");
    }
}

#[cfg(test)]
mod os_anchor_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_stream_ops_are_the_kernel_s_streams_and_no_single_answer_op() {
        let streams: Vec<String> = serde_json::from_str(&os_stream_ops_json().unwrap()).unwrap();
        assert!(streams.contains(&"os/tcp-listen".to_string()), "{streams:?}");
        assert!(!streams.contains(&"os/read-file".to_string()), "control: a single answer is no stream");
        assert!(streams.iter().all(|name| OsOp::from_name(name).is_some_and(OsOp::is_stream)));
    }

    fn listener() -> EmbeddedServer {
        let mut schema: OrbitalSchema = serde_json::from_value(json!({
            "name":"L", "typeContractVersion": 1, "orbitals":[{"name":"L","entity":{"name":"S","persistence":"runtime","fields":[{"name":"id","type":"string"}]},
            "traits":[{"name":"T","linkedEntity":"S","stateMachine":{"states":[{"name":"idle","isInitial":true}],"transitions":[]}}]}]
        }))
        .unwrap();
        schema.os_grants = Some(
            serde_json::from_value(json!({
                "net": [{ "target": "127.0.0.1:3000", "kinds": ["listen"] }],
                "anchored": {
                    "net": [{ "host": { "literal": "127.0.0.1" }, "port": { "anchor": { "env": "PORT" } }, "kinds": ["listen"] }],
                    "fs": [{ "anchor": "cwd", "glob": "dist/**", "modes": ["read"] }]
                }
            }))
            .unwrap(),
        );
        EmbeddedServer::new(schema).unwrap()
    }

    fn listen_on(port: u16) -> String {
        json!({ "need": "net", "host": "127.0.0.1", "port": port, "kinds": ["listen"] }).to_string()
    }

    fn read(path: &str) -> String {
        json!({ "need": "fs", "path": path, "mode": "read" }).to_string()
    }

    #[test]
    fn a_host_s_anchors_resolve_the_anchored_grants_and_arguments() {
        let server = listener();
        let problems = server
            .set_os_anchors_json(&json!({ "start": "/srv/app", "env": { "PORT": "8080" } }).to_string())
            .unwrap();
        assert_eq!(problems, "[]");
        assert!(server.os_allows_json(&listen_on(8080)).unwrap(), "the env PORT grant resolves");
        assert!(server.os_allows_json(&read("/srv/app/dist/index.html")).unwrap(), "the cwd grant resolves under start");
        assert!(server.os_allows_json(&listen_on(3000)).unwrap(), "control: the literal grant stays");
        assert!(!server.os_allows_json(&listen_on(8081)).unwrap(), "control: another port");
        let args = server
            .os_resolve_args_json("os/tcp-listen", &json!(["127.0.0.1", { "anchor": { "env": "PORT" } }]).to_string())
            .unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&args).unwrap(), json!({ "ok": ["127.0.0.1", 8080.0] }));
    }

    #[test]
    fn control_without_anchors_an_anchored_grant_grants_nothing() {
        let server = listener();
        assert!(!server.os_allows_json(&listen_on(8080)).unwrap());
        assert!(server.os_allows_json(&listen_on(3000)).unwrap());
    }

    #[test]
    fn edge_an_unset_env_anchor_is_reported_and_its_argument_is_absent() {
        let server = listener();
        let problems = server.set_os_anchors_json(&json!({ "start": "/srv/app", "env": {} }).to_string()).unwrap();
        assert!(problems.contains("PORT is unset"), "{problems}");
        let args = server
            .os_resolve_args_json("os/tcp-listen", &json!(["127.0.0.1", { "anchor": { "env": "PORT" } }]).to_string())
            .unwrap();
        let answer: serde_json::Value = serde_json::from_str(&args).unwrap();
        assert_eq!(answer["error"]["kind"], "not-found", "{answer}");
        assert!(server.os_resolve_args_json("os/nope", "[]").is_err(), "an unknown op is refused");
    }
}
