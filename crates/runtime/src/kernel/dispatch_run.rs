//! The one dispatch loop. A dispatch is a [`DispatchRun`]: the seeds to deliver, and for the seed in
//! flight its worklist, visited keys, dispatch memory and place in the seed's stages. The loop moves it
//! one unit at a time (one delivery of the worklist, or one seed scope), and a unit commits to the run
//! only once all its I/O is answered. A blocking host runs the units straight through; an async host
//! runs each with the answers recorded so far and gets [`Progress::Await`] at the first one missing.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::envelope::Origin;
use super::*;
use crate::effects::response::MountSeed;
use crate::resumable::{
    accept_into, number_requests, DispatchContinuation, DispatchError, DispatchOutcomes, Exchange,
    IoAnswer, IoReply, IoRequest, PendingRequest, Progress, StoreAsk, StoreLeg,
};
use crate::row_query::RowWriteOutcome;
use orbital_core_base::envelope::{DeliverRefusal, DeliverRefusalKind, Envelope, EnvelopeVerdict};
use orbital_core_base::value::ValueMap;
use orbital_core_eval::util::visit_key::VisitFrame;

/// Why a unit stopped short: a kernel error, or an I/O answer the unit does not have yet (the unit
/// has put the run and the kernel back as they were before it).
pub(crate) enum Fault {
    Kernel(KernelError),
    Suspended,
}

impl From<KernelError> for Fault {
    fn from(e: KernelError) -> Self {
        Fault::Kernel(e)
    }
}

impl Fault {
    pub(crate) fn into_kernel(self) -> KernelError {
        match self {
            Fault::Kernel(e) => e,
            Fault::Suspended => {
                KernelError::Internal("a dispatch suspended outside a resumable run".to_string())
            }
        }
    }
}

struct Cascade {
    queue: VecDeque<CascadeItem>,
    steps: usize,
}

enum Stage {
    Released(Cascade),
    Scopes {
        idx: usize,
        own: Vec<CascadeItem>,
        fanned: Vec<CascadeItem>,
    },
    Main(Cascade),
}

/// A seed that fires a trait's tick instead of delivering an event.
#[derive(Clone)]
struct TickSeed {
    name: String,
    /// The entity id the trait's state is held under.
    state_entity: String,
    /// Run over every row of the trait's entity (the plan's `entity_id` is the placeholder).
    all_rows: bool,
}

struct SeedPlan {
    trait_name: String,
    entity_id: Option<String>,
    event_name: String,
    payload: Value,
    delivery: Option<DeliveryRecord>,
    log: Option<DispatchLog>,
    lifecycle_targets: Vec<String>,
    tick: Option<TickSeed>,
}

struct SeedRun {
    eid: String,
    event_name: String,
    payload: Arc<Value>,
    now: f64,
    seed_scopes: Vec<String>,
    seed_delivery: Arc<Value>,
    visited: DispatchVisits,
    memory: DispatchMemory,
    stage: Stage,
    tick: Option<TickSeed>,
    /// One event's delivery to one trait, open until its step is done.
    span: tracing::Span,
}

pub(crate) struct DispatchRun {
    trace_seeds: Vec<crate::dispatch_trace::TraceSeed>,
    plans: VecDeque<SeedPlan>,
    seed: Option<SeedRun>,
    outcomes: DispatchOutcomes,
    locale: Option<String>,
    /// The viewer the dispatch was started as (see `enter_viewer`); `None` leaves the kernel's `@user` as it is.
    user: Option<Arc<Value>>,
    origin: Origin,
    /// The transition the step in flight committed when it suspended (G-CROSS-259).
    provisional: Option<Box<ProvisionalCommit>>,
    /// Open from the run's start to its end, across every ask it parks on.
    span: tracing::Span,
}

impl DispatchRun {
    pub(crate) fn dispatch_id(&self) -> u64 {
        self.origin.seq
    }

    /// The event of the seed in flight, which names a dispatch parked on its ask.
    fn parked_event(&self) -> String {
        self.seed
            .as_ref()
            .map(|seed| seed.event_name.clone())
            .unwrap_or_default()
    }
}

enum Flow {
    More,
    Done,
}

impl RuntimeKernel {
    /// Dispatch an event with full cross-trait listener fan-out, mirroring the canonical
    /// runtime's EventBus. The target trait processes the event; then every trait whose
    /// `listens` matches `(event, source-trait)` processes its triggered event, and any
    /// `emit` effect fans out the same way. Returns one outcome per processed trait, in
    /// dispatch order, so the caller merges their effects into a single response. A
    /// `(trait, event, from, payload, frame)` visited-set bounds cycles (`DispatchVisits`).
    ///
    /// This is the single dispatch entry point the client bridge and server route call —
    /// `process_event` is the single-trait primitive it builds on. Without this, a user
    /// event sent to one trait (e.g. a "New Item" button's `CREATE`) never reaches the
    /// modal trait that *listens* for it, so nothing happens (RS-G20).
    pub fn dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_continuing(trait_name, entity_id, event_name, payload, None, None)
    }

    /// [`Self::dispatch`] for a request that continues a client-begun dispatch: `delivery` is the
    /// target's `@event` and `log` its `@prevEvents`/`@prevStates` so far (`EventRequest.delivery`
    /// / `.dispatch_log`). `None` for both is a direct dispatch.
    pub fn dispatch_continuing(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_entered(
            None, trait_name, entity_id, event_name, payload, delivery, log,
        )
    }

    /// [`Self::dispatch`] on behalf of the dispatch holding `turn` (EXEC-CONC-0): it re-enters that dispatch while
    /// it runs and is refused while it is parked on an ask.
    pub fn dispatch_in_turn(
        &self,
        turn: super::LaneTurn,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        self.dispatch_entered(
            Some(turn),
            trait_name,
            entity_id,
            event_name,
            payload,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_entered(
        &self,
        turn: Option<super::LaneTurn>,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(turn)?;
        self.require_payload([(trait_name, event_name)], &payload)?;
        let run = self.run_of_dispatch(trait_name, entity_id, event_name, payload, delivery, log);
        self.run_straight_through(run)
    }

    /// The traits of `candidates` whose held state has an arm for `event`, in the order given: who a
    /// request that names no trait is delivered to. JS twin: `evaluateOrbitalEvent`'s discovery
    /// (`canHandleEvent`).
    pub fn discover_handlers(
        &self,
        candidates: &[String],
        entity_id: Option<&str>,
        event_name: &str,
    ) -> Vec<String> {
        let eid = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
        candidates
            .iter()
            .filter(|t| self.trait_handles(t, &self.current_state_name(t, Some(eid)), event_name))
            .cloned()
            .collect()
    }

    /// The held state of each of `traits` (its initial one while untouched).
    pub fn held_states(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
    ) -> HashMap<String, String> {
        let eid = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
        traits
            .iter()
            .map(|t| (t.clone(), self.current_state_name(t, Some(eid))))
            .collect()
    }

    /// [`Self::dispatch`] to each of `traits` as its own seed, in order, with the shared `payload`.
    pub fn dispatch_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(None)?;
        self.require_payload(traits.iter().map(|t| (t.as_str(), event_name)), &payload)?;
        let run = self.run_of_each(traits, entity_id, event_name, payload, None, None);
        self.run_straight_through(run)
    }

    /// [`Self::dispatch_each`] as a resumable dispatch.
    pub fn start_dispatch_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload(traits.iter().map(|t| (t.as_str(), event_name)), &payload)?;
        let run = self.run_of_each(traits, entity_id, event_name, payload, delivery, log);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// A mount batch (`EventRequest.mount`): each seed runs its own lifecycle event, in
    /// order, with the shared `payload`. The client's mount (`_awaitingInit`) is synced
    /// once against every seed, so no seed's held delivery leaves before its own step.
    /// A seed naming no registered trait is skipped. JS twin: `evaluateOrbitalEvent`'s
    /// `request.mount` branch.
    pub fn dispatch_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<Vec<(String, ProcessEventResult)>, KernelError> {
        let _lane = self.lane.enter(None)?;
        self.require_payload(
            seeds.iter().map(|s| (s.r#trait.as_str(), s.event.as_str())),
            &payload,
        )?;
        let run = self.run_of_mount(seeds, entity_id, payload);
        self.run_straight_through(run)
    }

    /// [`Self::dispatch_continuing`] as a resumable dispatch: it runs until it needs I/O it has no
    /// answer for and returns [`Progress::Await`] with the requests, or [`Progress::Done`] with the outcomes.
    pub fn start_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload([(trait_name, event_name)], &payload)?;
        let run = self.run_of_dispatch(trait_name, entity_id, event_name, payload, delivery, log);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// [`Self::dispatch_mount`] as a resumable dispatch.
    pub fn start_dispatch_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        self.require_payload(
            seeds.iter().map(|s| (s.r#trait.as_str(), s.event.as_str())),
            &payload,
        )?;
        let run = self.run_of_mount(seeds, entity_id, payload);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    /// Go on with a dispatch that returned [`Progress::Await`], given an answer to each of its requests.
    pub fn resume(
        &self,
        mut continuation: DispatchContinuation,
        replies: Vec<IoReply>,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = match continuation.lane.take() {
            Some(hold) => hold.resume(),
            None => self.lane.enter(None)?,
        };
        accept_into(&mut continuation, replies)?;
        self.drive_entry(lane, continuation)
    }

    /// Drive a dispatch entered on `lane`: a dispatch parked on an ask keeps the lane until it commits.
    fn drive_entry(
        &self,
        lane: super::lane::LaneGuard,
        continuation: DispatchContinuation,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        match self.drive_resumable(continuation)? {
            Progress::Await {
                mut continuation,
                requests,
            } => {
                if Self::parked_on_keyed_calls_only(&continuation, &requests) {
                    let keys = requests
                        .iter()
                        .filter_map(|pending| match &pending.request {
                            IoRequest::CallService { key, .. } => key.clone(),
                            _ => None,
                        })
                        .collect();
                    continuation.keyed = Some(Box::new(self.keyed_parks.hold(keys)));
                    drop(lane);
                } else {
                    continuation.keyed = None;
                    let event = continuation.run.parked_event();
                    continuation.lane = lane.park(event);
                }
                Ok(Progress::Await {
                    continuation,
                    requests,
                })
            }
            done => Ok(done),
        }
    }

    /// A dispatch parked only on keyed, routed `call-service` asks has reached the declared async boundary: it
    /// commits and frees the lane, and its answer is handled as a dispatch queued behind the lane (EXEC-CONC-0). Any
    /// unkeyed ask, or a store leg in flight, keeps the lane until the dispatch commits (EXEC-CONC-1).
    fn parked_on_keyed_calls_only(continuation: &DispatchContinuation, requests: &[PendingRequest]) -> bool {
        continuation.store.is_none()
            && !requests.is_empty()
            && requests.iter().all(|pending| match &pending.request {
                IoRequest::CallService { key: Some(_), site: Some(site), .. } => {
                    let routes = &site.emit;
                    routes.success.is_some() || routes.failure.is_some() || routes.cancelled.is_some() || routes.on_message.is_some()
                }
                _ => false,
            })
    }

    fn continuation_of(run: DispatchRun) -> DispatchContinuation {
        DispatchContinuation {
            run,
            journal: Vec::new(),
            pending: Vec::new(),
            next_id: 0,
            reads: Default::default(),
            store: None,
            seeds: None,
            lane: None,
            keyed: None,
        }
    }

    fn run_of_dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> DispatchRun {
        let lifecycle_targets = orbital_core_base::events::is_bus_excluded_event(event_name)
            .then(|| trait_name.to_string())
            .into_iter()
            .collect();
        self.run_of(vec![SeedPlan {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(str::to_string),
            event_name: event_name.to_string(),
            payload,
            delivery: delivery.cloned(),
            log: log.cloned(),
            lifecycle_targets,
            tick: None,
        }])
    }

    /// One firing of `tick_name` on `trait_name` as a resumable dispatch. With no `entity_id` it runs over
    /// every row of the trait's entity (the kernel asks the host to list them); the trait's state is held
    /// under `entity_id`, else the singleton. JS twin: `OrbitalServerRuntime.executeTick`.
    pub fn start_dispatch_tick(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        tick_name: &str,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let lane = self.lane.enter(None)?;
        let defs = self.trait_definitions.read();
        let declared = defs.get(trait_name).ok_or_else(|| {
            DispatchError::Kernel(KernelError::TraitNotFound(trait_name.to_string()))
        })?;
        if !declared
            .ticks
            .iter()
            .flatten()
            .any(|tick| tick.name == tick_name)
        {
            return Err(DispatchError::Kernel(KernelError::Internal(format!(
                "tick '{tick_name}' not found on '{trait_name}'"
            ))));
        }
        drop(defs);
        let plan = SeedPlan {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(str::to_string),
            event_name: orbital_core_base::bus_event::tick_event_key(tick_name),
            payload: Value::Null,
            delivery: None,
            log: None,
            lifecycle_targets: Vec::new(),
            tick: Some(TickSeed {
                name: tick_name.to_string(),
                state_entity: entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON).to_string(),
                all_rows: entity_id.is_none(),
            }),
        };
        let run = self.run_of(vec![plan]);
        self.drive_entry(lane, Self::continuation_of(run))
    }

    fn run_of_each(
        &self,
        traits: &[String],
        entity_id: Option<&str>,
        event_name: &str,
        payload: Value,
        delivery: Option<&DeliveryRecord>,
        log: Option<&DispatchLog>,
    ) -> DispatchRun {
        let lifecycle_targets: Vec<String> =
            if orbital_core_base::events::is_bus_excluded_event(event_name) {
                traits.to_vec()
            } else {
                Vec::new()
            };
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let plans = traits
            .iter()
            .enumerate()
            .map(|(i, trait_name)| SeedPlan {
                trait_name: trait_name.clone(),
                entity_id: entity_id.map(str::to_string),
                event_name: event_name.to_string(),
                payload: match (&awaiting, i) {
                    (Some(list), 0) => with_awaiting_sidecar(payload.clone(), list),
                    _ => payload.clone(),
                },
                delivery: delivery.cloned(),
                log: log.cloned(),
                lifecycle_targets: lifecycle_targets.clone(),
                tick: None,
            })
            .collect();
        self.run_of(plans)
    }

    fn run_of_mount(
        &self,
        seeds: &[MountSeed],
        entity_id: Option<&str>,
        payload: Value,
    ) -> DispatchRun {
        let targets: Vec<String> = seeds.iter().map(|s| s.r#trait.clone()).collect();
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let mut first = true;
        let mut plans = Vec::new();
        for seed in seeds {
            if !self.trait_definitions.read().contains_key(&seed.r#trait) {
                continue;
            }
            // Only the first seed carries the client's mount, synced against all seeds.
            let seed_payload = match (&awaiting, first) {
                (Some(list), true) => with_awaiting_sidecar(payload.clone(), list),
                _ => payload.clone(),
            };
            first = false;
            plans.push(SeedPlan {
                trait_name: seed.r#trait.clone(),
                entity_id: entity_id.map(str::to_string),
                event_name: seed.event.as_str().to_string(),
                payload: seed_payload,
                delivery: None,
                log: None,
                lifecycle_targets: targets.clone(),
                tick: None,
            });
        }
        self.run_of(plans)
    }

    fn run_of(&self, plans: Vec<SeedPlan>) -> DispatchRun {
        let span = match plans.first() {
            Some(first) => tracing::debug_span!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                "dispatch",
                event = %first.event_name,
                target_trait = %first.trait_name,
                targets = plans.len(),
            ),
            None => tracing::debug_span!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, "dispatch", targets = 0),
        };
        DispatchRun {
            trace_seeds: plans
                .iter()
                .map(|p| crate::dispatch_trace::TraceSeed {
                    trait_name: p.trait_name.clone(),
                    entity_id: p.entity_id.clone(),
                    event: p.event_name.clone(),
                })
                .collect(),
            plans: plans.into(),
            seed: None,
            outcomes: Vec::new(),
            locale: self.dispatch_locale.read().clone(),
            user: self.thread_viewer(),
            origin: self.next_dispatch_origin(),
            provisional: None,
            span,
        }
    }

    fn observe_dispatch(&self, run: &DispatchRun, error: Option<String>) {
        let Some(observer) = self.dispatch_observer.read().clone() else {
            return;
        };
        if !(observer.enabled)() {
            return;
        }
        let defs = self.trait_definitions.read();
        let outcomes = run
            .outcomes
            .iter()
            .map(|(name, result)| {
                let state_key = self.state_key(Some(&result.state_entity_id));
                let observed_state = self
                    .state_manager
                    .read()
                    .get_state(name, state_key.as_deref())
                    .map(|state| state.current_state.clone());
                let mut trace = crate::dispatch_trace::transition_of(name, result, observed_state);
                let source = self.emit_source(name);
                trace.trait_id = source.trait_id;
                trace.orbital_id = source.orbital_id;
                if let Some(def) = defs.get(name) {
                    trace.definition_fingerprint = Some(
                        orbital_core_base::schema::trait_execution_fingerprint(def)
                            .expect("registered trait definition serializes"),
                    );
                    if let Some(metadata) = def.source_declaration.as_ref() {
                        trace.source_trait_id = Some(metadata.trait_id.clone());
                        trace.source_declaration_fingerprint = Some(metadata.fingerprint.clone());
                    }
                }
                trace
            })
            .collect();
        drop(defs);
        (observer.completed)(crate::dispatch_trace::DispatchTraceRecord {
            dispatch_id: run.origin.seq,
            seeds: run.trace_seeds.clone(),
            outcomes,
            error,
        });
    }

    /// Run every unit with the host's handlers answering at the I/O point: no suspension, no replay.
    fn run_straight_through(&self, mut run: DispatchRun) -> Result<DispatchOutcomes, KernelError> {
        loop {
            match self.unit(&mut run) {
                Ok(Flow::More) => {}
                Ok(Flow::Done) => {
                    self.observe_dispatch(&run, None);
                    return Ok(std::mem::take(&mut run.outcomes));
                }
                Err(fault) => {
                    let error = fault.into_kernel();
                    self.observe_dispatch(&run, Some(error.to_string()));
                    return Err(error);
                }
            }
        }
    }

    /// Run units until the dispatch is done or one needs an answer the journal lacks.
    pub(crate) fn drive_resumable(
        &self,
        mut c: DispatchContinuation,
    ) -> Result<Progress<DispatchOutcomes>, DispatchError> {
        let _dispatch = c.run.span.clone().entered();
        loop {
            let seeds = c.seeds.get_or_insert_with(|| self.pending_store_seeds());
            if c.store.is_none() {
                if let Some((origin, ask)) = seeds.pop_front() {
                    c.store = self.seed_leg_for(origin, ask);
                    continue;
                }
            }
            if let Some(leg) = c.store.take() {
                let StoreLeg { ask, driver, seed } = *leg;
                let driven = match self.drive_resumable(driver) {
                    Ok(progress) => progress,
                    Err(error) => {
                        self.observe_dispatch(&c.run, Some(error.to_string()));
                        return Err(error);
                    }
                };
                match driven {
                    Progress::Await {
                        continuation,
                        requests,
                    } => {
                        c.pending = requests.clone();
                        c.store = Some(Box::new(StoreLeg {
                            ask,
                            driver: continuation,
                            seed,
                        }));
                        return Ok(Progress::Await {
                            continuation: c,
                            requests,
                        });
                    }
                    Progress::Done(outcomes) => {
                        let answer = self.store_reply(&ask, &outcomes);
                        if let Some(origin) = seed {
                            self.seed_landed(&ask, &answer, &origin);
                            continue;
                        }
                        c.journal.push(Exchange::new(ask.request, answer));
                    }
                }
            }
            *self.suspended_commit.write() = None;
            // The step committed its transition when it suspended: it replays from the state it left, and a state
            // another dispatch moved the trait to meanwhile stands once the replay is done.
            let replay = c.run.provisional.take().map(|p| {
                let moved = self.rewind_provisional(&p);
                (p, moved)
            });
            let journal = std::mem::take(&mut c.journal);
            let (result, end) =
                self.io
                    .run_step_reading(journal, std::mem::take(&mut c.reads), || {
                        self.unit(&mut c.run)
                    });
            let committed = self.suspended_commit.write().take();
            let suspended = !end.pending.is_empty() || !end.store_asks.is_empty();
            match replay {
                Some((p, Some(moved))) => {
                    self.set_held_state(&p.trait_name, &p.entity_id, moved);
                    if suspended {
                        c.run.provisional = Some(committed.map(Box::new).unwrap_or(p));
                    }
                }
                Some((p, None)) if suspended => match committed {
                    Some(again) => c.run.provisional = Some(Box::new(again)),
                    None => {
                        // Suspended before reaching its transition again: hold the state it had entered.
                        let mut entered = p.from.clone();
                        entered.transition_to(&p.to);
                        self.set_held_state(&p.trait_name, &p.entity_id, entered);
                        c.run.provisional = Some(p);
                    }
                },
                _ if suspended => c.run.provisional = committed.map(Box::new),
                _ => {}
            }
            c.reads = end.reads;
            if let Some(diverged) = end.diverged {
                self.observe_dispatch(&c.run, Some(diverged.to_string()));
                return Err(DispatchError::Resume(diverged));
            }
            if suspended {
                if result.is_ok() {
                    let error = DispatchError::Kernel(KernelError::Internal(
                        "a dispatch step committed while it awaited host I/O".to_string(),
                    ));
                    self.observe_dispatch(&c.run, Some(error.to_string()));
                    return Err(error);
                }
                c.journal = end.journal;
                if let Some(ask) = end.store_asks.into_iter().next() {
                    match self.store_driver_run(&ask) {
                        Ok(driver) => {
                            c.store = Some(Box::new(StoreLeg {
                                ask,
                                driver,
                                seed: None,
                            }))
                        }
                        Err(message) => {
                            let answer = Self::store_failed(&ask, message);
                            c.journal.push(Exchange::new(ask.request, answer));
                        }
                    }
                    continue;
                }
                c.pending = number_requests(&mut c.next_id, end.pending);
                tracing::debug!(target: orbital_core_base::logging::ns::RUNTIME_KERNEL, asks = c.pending.len(), "parked on host asks");
                let requests = c.pending.clone();
                return Ok(Progress::Await {
                    continuation: c,
                    requests,
                });
            }
            match result {
                Ok(Flow::More) => {}
                Ok(Flow::Done) => {
                    self.observe_dispatch(&c.run, None);
                    return Ok(Progress::Done(std::mem::take(&mut c.run.outcomes)));
                }
                Err(fault) => {
                    let error = fault.into_kernel();
                    self.observe_dispatch(&c.run, Some(error.to_string()));
                    return Err(DispatchError::Kernel(error));
                }
            }
        }
    }

    /// Every registered seed not yet landed, each under a fresh ask key: a run writes them before its first step,
    /// even while another run's leg for one awaits the host (seeds are creates; `duplicate-id` is their no-op).
    pub(crate) fn pending_store_seeds(&self) -> std::collections::VecDeque<(String, StoreAsk)> {
        self.store_seeds
            .lock()
            .iter()
            .map(|ask| {
                (
                    ask.key.clone(),
                    StoreAsk {
                        key: self.io.next_store_key(),
                        ..ask.clone()
                    },
                )
            })
            .collect()
    }

    pub(crate) fn seed_leg_for(&self, origin: String, ask: StoreAsk) -> Option<Box<StoreLeg>> {
        match self.store_driver_run(&ask) {
            Ok(driver) => Some(Box::new(StoreLeg {
                ask,
                driver,
                seed: Some(origin),
            })),
            Err(message) => {
                tracing::warn!(driver = %ask.driver, %message, "kernel: store seed not sent");
                if let Some(ensure) = &ask.ensure {
                    self.io.set_index_failure(
                        &ensure.entity_type,
                        Some(format!("store driver {} could not ensure the indexes of {}: {message}", ask.driver, ensure.entity_type)),
                    );
                }
                None
            }
        }
    }

    pub(crate) fn seed_landed(&self, ask: &StoreAsk, answer: &IoAnswer, origin: &str) {
        if let Some(ensure) = &ask.ensure {
            match answer {
                IoAnswer::Wrote(Err(message)) => {
                    tracing::warn!(driver = %ask.driver, entity = %ensure.entity_type, %message, "kernel: store indexes not ensured");
                    self.io.set_index_failure(
                        &ensure.entity_type,
                        Some(format!(
                            "store driver {} could not ensure the indexes of {}: {message}",
                            ask.driver, ensure.entity_type
                        )),
                    );
                    return;
                }
                _ => self.io.set_index_failure(&ensure.entity_type, None),
            }
        } else {
            Self::report_seed(ask, answer);
        }
        self.store_seeds
            .lock()
            .retain(|registered| registered.key != origin);
    }

    /// The driver orbital's dispatch of the verb `ask` maps to, in a frame of its own keyed by the ask.
    pub(crate) fn store_driver_run(&self, ask: &StoreAsk) -> Result<DispatchContinuation, String> {
        let verb = match &ask.ensure {
            Some(ensure) => crate::store_driver::ensure_verb(&ensure.entity_type, &ensure.indexes, &ask.key),
            None => crate::store_driver::verb_for(&ask.request, &ask.key).ok_or_else(|| {
                format!(
                    "store driver {}: {:?} is not a row ask a driver answers",
                    ask.driver, ask.request
                )
            })?,
        };
        let candidates: Vec<String> = {
            let schemas = self.schemas.read();
            self.trait_homes
                .read()
                .iter()
                .filter(|(_, (schema, at))| {
                    schemas
                        .get(schema)
                        .and_then(|s| s.orbitals.get(*at))
                        .is_some_and(|o| o.name == ask.driver)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        let handlers = self.discover_handlers(&candidates, Some(&ask.key), verb.event);
        let [target] = handlers.as_slice() else {
            return Err(format!(
                "store driver {} must have exactly one trait handling {}, has {:?}",
                ask.driver, verb.event, handlers
            ));
        };
        let mut driver = Self::continuation_of(self.run_of_dispatch(
            target,
            Some(&ask.key),
            verb.event,
            verb.payload,
            None,
            None,
        ));
        driver.seeds = Some(Default::default());
        Ok(driver)
    }

    /// A seed row the driver did not create is logged; a row already present (`duplicate-id`) is the seed's no-op.
    fn report_seed(ask: &StoreAsk, answer: &IoAnswer) {
        match answer {
            IoAnswer::Wrote(Ok(outcomes)) => {
                for outcome in outcomes {
                    if !matches!(
                        outcome,
                        RowWriteOutcome::Created { .. } | RowWriteOutcome::DuplicateId
                    ) {
                        tracing::warn!(driver = %ask.driver, key = %ask.key, ?outcome, "kernel: store seed row not created");
                    }
                }
            }
            other => {
                tracing::warn!(driver = %ask.driver, key = %ask.key, answer = ?other, "kernel: store seed failed")
            }
        }
    }

    pub(crate) fn store_failed(ask: &StoreAsk, message: String) -> IoAnswer {
        crate::store_driver::failed_answer(&ask.request, message).unwrap_or_else(|| {
            IoAnswer::Wrote(Err(format!("store driver {}: not a row ask", ask.driver)))
        })
    }

    /// The answer the driver's keyed reply gives `ask`: a driver that sent none, or a malformed one, failed it.
    pub(crate) fn store_reply(&self, ask: &StoreAsk, outcomes: &DispatchOutcomes) -> IoAnswer {
        use crate::store_driver::*;
        if ask.ensure.is_some() {
            return Self::ensure_reply(ask, outcomes);
        }
        const REPLIES: [&str; 7] = [
            ROW_FOUND,
            ROWS_LISTED,
            ROWS_PAGE,
            ROWS_IDS,
            ROWS_WRITTEN,
            STORE_UNSUPPORTED,
            STORE_FAILED,
        ];
        let reply = outcomes
            .iter()
            .flat_map(|(_, r)| r.effect_results.iter())
            .find_map(|er| match &er.effect {
                Effect::Emit { event, payload }
                    if REPLIES.contains(&event.as_str())
                        && reply_key(payload) == Some(ask.key.as_str()) =>
                {
                    Some((event.as_str(), payload))
                }
                _ => None,
            });
        let failed = |message: String| Self::store_failed(ask, message);
        match reply {
            Some((event, payload)) => answer_for(&ask.request, event, payload)
                .unwrap_or_else(|e| failed(format!("store driver {}: {e}", ask.driver))),
            None => failed(format!(
                "store driver {} sent no reply for ask {}",
                ask.driver, ask.key
            )),
        }
    }

    /// `Wrote(Ok)` when the driver confirmed the ensure, else `Wrote(Err)` with its message.
    fn ensure_reply(ask: &StoreAsk, outcomes: &DispatchOutcomes) -> IoAnswer {
        use crate::store_driver::*;
        let reply = outcomes
            .iter()
            .flat_map(|(_, r)| r.effect_results.iter())
            .find_map(|er| match &er.effect {
                Effect::Emit { event, payload }
                    if [ROWS_INDEXES_ENSURED, STORE_FAILED].contains(&event.as_str())
                        && reply_key(payload) == Some(ask.key.as_str()) =>
                {
                    Some((event.as_str(), payload))
                }
                _ => None,
            });
        match reply {
            Some((ROWS_INDEXES_ENSURED, _)) => IoAnswer::Wrote(Ok(Vec::new())),
            Some((_, payload)) => IoAnswer::Wrote(Err(payload
                .as_object()
                .and_then(|o| o.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("the driver failed without a message")
                .to_string())),
            None => IoAnswer::Wrote(Err(format!("the driver sent no reply for ask {}", ask.key))),
        }
    }

    /// One unit of the run: begin the next seed, or advance the seed in flight by one delivery.
    fn unit(&self, run: &mut DispatchRun) -> Result<Flow, Fault> {
        let DispatchRun {
            plans,
            seed,
            outcomes,
            locale,
            user,
            origin,
            ..
        } = run;
        let Some(in_flight) = seed.as_mut() else {
            if plans
                .front()
                .is_some_and(|plan| plan.tick.as_ref().is_some_and(|tick| tick.all_rows))
            {
                self.expand_tick_rows(plans)?;
                return Ok(Flow::More);
            }
            return match plans.pop_front() {
                Some(plan) => {
                    *seed = Some(self.begin_seed(plan));
                    Ok(Flow::More)
                }
                None => Ok(Flow::Done),
            };
        };
        let _stamp = self.hold_stamp(in_flight.now);
        let _locale = self.hold_locale(locale.as_deref());
        let _user = self.hold_user(user.as_deref());
        let committed_from = origin.emits;
        self.hold_origin(origin);
        let _deliver = in_flight.span.clone().entered();
        let stepped = self.step_seed(in_flight, outcomes);
        self.release_origin(origin, committed_from, stepped.is_ok());
        match stepped? {
            Flow::More => Ok(Flow::More),
            Flow::Done => {
                *seed = None;
                Ok(Flow::More)
            }
        }
    }

    /// The dispatch's one `now`: the outer stamp when one is held, else the clock.
    fn hold_stamp(&self, now: f64) -> DispatchStamp {
        let mut slot = self.dispatch_now.write();
        match *slot {
            Some(held) => DispatchStamp {
                slot: None,
                now: held,
            },
            None => {
                *slot = Some(now);
                DispatchStamp {
                    slot: Some(Arc::clone(&self.dispatch_now)),
                    now,
                }
            }
        }
    }

    /// The run's viewer as the kernel's `@user` for one unit.
    fn hold_user(&self, user: Option<&Value>) -> Option<UserHold> {
        let user = user?;
        let previous = std::mem::replace(&mut *self.current_user.write(), user.clone());
        Some(UserHold {
            slot: Arc::clone(&self.current_user),
            previous,
        })
    }

    /// The run's viewer locale, held for one unit unless the caller already holds one.
    fn hold_locale(&self, locale: Option<&str>) -> Option<LocaleGuard> {
        let mut slot = self.dispatch_locale.write();
        match (locale, slot.is_none()) {
            (Some(locale), true) => {
                *slot = Some(locale.to_string());
                Some(LocaleGuard {
                    slot: Arc::clone(&self.dispatch_locale),
                })
            }
            _ => None,
        }
    }

    /// Replace the front tick plan that runs over every row by one plan per row of its trait's entity.
    fn expand_tick_rows(&self, plans: &mut VecDeque<SeedPlan>) -> Result<(), Fault> {
        let Some(front) = plans.front() else {
            return Ok(());
        };
        let entity_type = self
            .trait_definitions
            .read()
            .get(&front.trait_name)
            .and_then(|def| def.linked_entity.clone())
            .unwrap_or_else(|| front.trait_name.clone());
        let singleton_entity = self
            .entity_definitions
            .read()
            .get(&entity_type)
            .is_some_and(|def| {
                matches!(
                    def.persistence,
                    Some(orbital_core_base::schema::Persistence::Runtime)
                )
            });
        let listed = self.entity_source.list(&entity_type);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        let rows = listed.map_err(KernelError::Internal)?;
        let Some(plan) = plans.pop_front() else {
            return Ok(());
        };
        let ids: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                row.get_path("id")
                    .and_then(|id| id.as_str().map(str::to_string))
            })
            .collect();
        // A `[runtime]` entity with no rows ticks once on the `singleton` frame (EXEC-TICK-5).
        if ids.is_empty() && singleton_entity {
            plans.push_front(SeedPlan {
                tick: plan.tick.clone().map(|tick| TickSeed {
                    all_rows: false,
                    ..tick
                }),
                ..plan
            });
            return Ok(());
        }
        for id in ids.into_iter().rev() {
            plans.push_front(SeedPlan {
                entity_id: Some(id),
                trait_name: plan.trait_name.clone(),
                event_name: plan.event_name.clone(),
                payload: plan.payload.clone(),
                delivery: plan.delivery.clone(),
                log: plan.log.clone(),
                lifecycle_targets: plan.lifecycle_targets.clone(),
                tick: plan.tick.clone().map(|tick| TickSeed {
                    all_rows: false,
                    ..tick
                }),
            });
        }
        Ok(())
    }

    fn begin_seed(&self, plan: SeedPlan) -> SeedRun {
        let now = self.dispatch_now.read().unwrap_or_else(|| self.clock_now());
        let SeedPlan {
            trait_name,
            entity_id,
            event_name,
            payload,
            delivery,
            log,
            lifecycle_targets,
            tick,
        } = plan;
        let eid = entity_id.unwrap_or_else(|| crate::process_result::RUNTIME_SINGLETON.to_string());
        // A server leg carries the client's mount (`_awaitingInit`, never a
        // payload binding): deliveries held for a trait the client has since
        // initialized go out first, ahead of this request's seed.
        let (payload, awaiting) = take_awaiting_sidecar(payload);
        let released = match awaiting {
            Some(awaiting) => {
                let targets: Vec<&str> = lifecycle_targets.iter().map(String::as_str).collect();
                self.mount.write().sync(awaiting, &targets)
            }
            None => Vec::new(),
        };

        // Seed bubble (§158): the JS runtime fans a UI-originated emit to
        // every scope on the embed chain, innermost first (`TraitScopeProvider`
        // R-EMBEDDED-EMIT-BUBBLE) — a host with no transition for the event is
        // a tolerated no-op, same as `TraitNotFound` below, but it still serves
        // as a listener source. Only the SEED event bubbles this way; anything
        // this loop queues afterwards (cascaded listens/emits) keeps the
        // single-scope semantics `useEventBus.ts` reserves for machine-
        // originated emits. A trait with no ancestors runs this loop exactly
        // once, byte-identical to the pre-bubble single-scope dispatch.
        let mut seed_scopes = vec![trait_name.clone()];
        if tick.is_none() {
            seed_scopes.extend(
                self.embed_index
                    .read()
                    .ancestors(&trait_name)
                    .iter()
                    .cloned(),
            );
        }
        let mut memory = DispatchMemory::default();
        if let Some(log) = &log {
            memory.seed(&trait_name, log);
        }
        let seed_delivery = Arc::new(match &delivery {
            Some(d) => d.value(),
            None => delivery_record_value(&event_name, &payload, &BusEventSource::default()),
        });
        let stage = if released.is_empty() {
            Stage::Scopes {
                idx: 0,
                own: Vec::new(),
                fanned: Vec::new(),
            }
        } else {
            Stage::Released(Cascade {
                queue: released.into(),
                steps: 0,
            })
        };
        let span = tracing::trace_span!(
            target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
            "deliver",
            event = %event_name,
            entity = %eid,
        );
        SeedRun {
            eid,
            event_name,
            payload: Arc::new(payload),
            now,
            seed_scopes,
            seed_delivery,
            visited: DispatchVisits::default(),
            memory,
            stage,
            tick,
            span,
        }
    }

    fn step_seed(
        &self,
        seed: &mut SeedRun,
        outcomes: &mut DispatchOutcomes,
    ) -> Result<Flow, Fault> {
        let SeedRun {
            eid,
            event_name,
            payload,
            seed_scopes,
            seed_delivery,
            visited,
            memory,
            stage,
            tick,
            ..
        } = seed;
        match stage {
            Stage::Released(cascade) => {
                if !self.cascade_unit(eid, cascade, visited, memory, outcomes)? {
                    *stage = Stage::Scopes {
                        idx: 0,
                        own: Vec::new(),
                        fanned: Vec::new(),
                    };
                }
                Ok(Flow::More)
            }
            Stage::Scopes { idx, own, fanned } => {
                let Some(scope) = seed_scopes.get(*idx) else {
                    let queue: VecDeque<CascadeItem> = std::mem::take(own)
                        .into_iter()
                        .chain(std::mem::take(fanned))
                        .collect();
                    *stage = Stage::Main(Cascade { queue, steps: 0 });
                    return Ok(Flow::More);
                };
                let from = self.current_state_name(scope, Some(eid));
                let frame = self.step_frame(scope, eid);
                if self.io.suspended() {
                    return Err(Fault::Suspended);
                }
                let visit = visited.visit(scope, event_name, &from, payload, &VisitFrame::of(&frame));
                if visited.seen(&visit) {
                    debug!(trait_name = %scope, event = %event_name, from = %from, "seed:visited-skip");
                    *idx += 1;
                    return Ok(Flow::More);
                }
                let view = memory.view(scope, seed_delivery);
                let ran = match tick {
                    Some(tick) => {
                        self.process_tick_in_dispatch(scope, eid, &tick.state_entity, &tick.name)
                    }
                    None => self.process_event_in_dispatch(
                        scope,
                        Some(eid),
                        event_name,
                        Arc::clone(payload),
                        Some(&view),
                        false,
                    ),
                };
                let result = match ran {
                    Ok(r) => Some(r),
                    Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
                    Err(fault) => return Err(fault),
                };
                visited.record(visit);
                if result.is_some() {
                    fanned.extend(self.mount.write().release_after(scope, event_name));
                }
                let followups = self.step_followups(
                    scope,
                    event_name,
                    payload,
                    result.as_ref(),
                    seed_scopes,
                    tick.is_none(),
                );
                own.extend(followups.own);
                fanned.extend(followups.fanned);
                let mut result = result;
                if let Some(result) = result.as_mut() {
                    result.consumed_emits = followups.consumed_emits;
                }
                if let (Some(result), None) = (&result, tick.as_ref()) {
                    memory.record_delivery(scope, seed_delivery, view);
                    if result.executed {
                        memory.record_transition(scope, &result.previous_state, &result.new_state);
                    }
                }
                if let Some(result) = result {
                    outcomes.push((scope.clone(), result.released()));
                }
                *idx += 1;
                Ok(Flow::More)
            }
            Stage::Main(cascade) => {
                if self.cascade_unit(eid, cascade, visited, memory, outcomes)? {
                    Ok(Flow::More)
                } else {
                    Ok(Flow::Done)
                }
            }
        }
    }

    /// Shared BFS body: drain `queue`, processing each `(trait, event, payload)`
    /// through `process_event` and fanning out its own emits + declared listeners.
    /// The single owner of cross-trait cascade fan-out — `dispatch` (seeded from the
    /// target trait's own bubble scopes) and `apply_server_response` (seeded from
    /// un-delivered server-emitted events) both build on this, so there is ONE BFS,
    /// not two independently-maintained copies that could drift apart.
    pub(super) fn run_cascade(
        &self,
        eid: &str,
        queue: VecDeque<CascadeItem>,
        visited: &mut DispatchVisits,
        memory: &mut DispatchMemory,
        outcomes: &mut Vec<(String, ProcessEventResult)>,
    ) -> Result<(), KernelError> {
        let mut cascade = Cascade { queue, steps: 0 };
        loop {
            match self.cascade_unit(
                eid,
                &mut cascade,
                visited,
                memory,
                outcomes,
            ) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(fault) => return Err(fault.into_kernel()),
            }
        }
    }

    /// Re-render one embedded child under the payload its composer gave it. The child's lifecycle arm runs
    /// only when it stays in its state and renders; otherwise the walk goes through it to its own capture
    /// children, which keep the payload they were last composed with. JS twin:
    /// `OrbitalServerRuntime.rerenderCallsiteCaptureChildren`.
    fn repaint_unit(
        &self,
        eid: &str,
        cascade: &mut Cascade,
        item: CascadeItem,
        job: RepaintJob,
        outcomes: &mut DispatchOutcomes,
    ) -> Result<(), Fault> {
        let child = item.trait_name.clone();
        if job.visited.lock().contains(&child) {
            return Ok(());
        }
        let composed_with = if job.keep_composed {
            self.composed_callsite
                .read()
                .get(&child)
                .cloned()
                .unwrap_or_else(|| job.composed_with.clone())
        } else {
            job.composed_with.clone()
        };
        let def = self.trait_definitions.read().get(&child).cloned();
        let lifecycle = def.as_ref().and_then(|def| {
            crate::callsite_capture::repaint_lifecycle_event(
                def,
                &self.current_state_name(&child, Some(eid)),
            )
        });
        let result = match lifecycle {
            Some(event) => match self.process_event_composed(
                &child,
                Some(eid),
                event,
                Arc::new(Value::Object(ValueMap::default())),
                None,
                false,
                Some(&composed_with),
            ) {
                Ok(result) if result.executed => Some(result),
                Ok(_) => None,
                Err(Fault::Suspended) => {
                    cascade.queue.push_front(item);
                    return Err(Fault::Suspended);
                }
                Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
                Err(fault) => return Err(fault),
            },
            None => None,
        };
        job.visited.lock().insert(child.clone());
        let (keep_composed, composed_by) = match (&result, &def) {
            (Some(result), Some(def)) => {
                self.composed_callsite
                    .write()
                    .insert(child.clone(), composed_with.clone());
                let authored: Vec<&SExpression> =
                    result.effect_runs.iter().map(|run| &run.effect).collect();
                (
                    job.keep_composed,
                    Some(crate::callsite_capture::embedded_by_effects(def, &authored)),
                )
            }
            _ => (true, None),
        };
        let grandchildren = self
            .capture_children
            .read()
            .get(&child)
            .cloned()
            .unwrap_or_default();
        for name in grandchildren.into_iter().rev().filter(|name| {
            composed_by
                .as_ref()
                .is_none_or(|composed| composed.contains(name))
        }) {
            cascade.queue.push_front(Self::repaint_item(
                name,
                RepaintJob {
                    composed_with: composed_with.clone(),
                    keep_composed,
                    visited: Arc::clone(&job.visited),
                },
            ));
        }
        if let Some(result) = result {
            outcomes.push((child, result.released()));
        }
        Ok(())
    }

    /// Deliver the next item of the worklist. `Ok(true)` when the worklist has more, `Ok(false)` once
    /// it is drained or cut at the cap. A delivery that needs I/O the journal lacks is put back at the
    /// front of the worklist and the unit returns `Fault::Suspended` with nothing committed.
    fn cascade_unit(
        &self,
        eid: &str,
        cascade: &mut Cascade,
        visited: &mut DispatchVisits,
        memory: &mut DispatchMemory,
        outcomes: &mut Vec<(String, ProcessEventResult)>,
    ) -> Result<bool, Fault> {
        let Some(item) = cascade.queue.pop_front() else {
            return Ok(false);
        };
        if let Some(job) = item.repaint.clone() {
            self.repaint_unit(eid, cascade, item, job, outcomes)?;
            return Ok(true);
        }
        {
            let mut mount = self.mount.write();
            if mount.holds(&item) {
                debug!(trait_name = %item.trait_name, event = %item.event, "fanout:held-until-init");
                mount.hold(item);
                return Ok(true);
            }
        }
        let from = self.current_state_name(&item.trait_name, Some(eid));
        let frame = self.step_frame(&item.trait_name, eid);
        if self.io.suspended() {
            cascade.queue.push_front(item);
            return Err(Fault::Suspended);
        }
        let visit = (!item.completion)
            .then(|| visited.visit(&item.trait_name, &item.event, &from, &item.payload, &VisitFrame::of(&frame)));
        if visit.as_ref().is_some_and(|v| visited.seen(v)) {
            debug!(trait_name = %item.trait_name, event = %item.event, from = %from, "fanout:visited-skip");
            return Ok(true);
        }
        let view = memory.view(&item.trait_name, &item.delivery);
        if let Some(guard) = &item.listen_guard {
            if !listen_guard_passes(
                guard,
                &item.source_payload,
                &from,
                self.listener_config(&item.trait_name),
                &view,
            ) {
                debug!(trait_name = %item.trait_name, event = %item.event, "fanout:listen-guard-blocked");
                return Ok(true);
            }
        }
        if cascade.steps >= CROSS_TRAIT_CASCADE_CAP {
            let at_cap = match visit {
                Some(v) => v,
                None => visited.visit(&item.trait_name, &item.event, &from, &item.payload, &VisitFrame::of(&frame)),
            };
            visited.record(at_cap);
            let cut = crate::process_result::CascadeCut {
                cap: CROSS_TRAIT_CASCADE_CAP,
                trait_name: item.trait_name.clone(),
                event: item.event.clone(),
            };
            let mut dropped: Vec<String> = Vec::new();
            for name in std::iter::once(item.trait_name).chain(
                std::mem::take(&mut cascade.queue)
                    .into_iter()
                    .map(|i| i.trait_name),
            ) {
                if !dropped.contains(&name) {
                    dropped.push(name);
                }
            }
            tracing::warn!(
                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                cap = CROSS_TRAIT_CASCADE_CAP,
                dropped = ?dropped,
                "cascade:truncated",
            );
            if let Some((_, last)) = outcomes.last_mut() {
                last.cascade_dropped = dropped;
                last.cascade_cut = Some(cut);
            }
            return Ok(false);
        }
        // A source trait may not exist as a processable trait (e.g. a render-only
        // layout that only emits) — skip its own processing but still fan out.
        let result = match self.process_event_in_dispatch(
            &item.trait_name,
            Some(eid),
            &item.event,
            Arc::clone(&item.payload),
            Some(&view),
            true,
        ) {
            Ok(r) => Some(r),
            Err(Fault::Kernel(KernelError::TraitNotFound(_))) => None,
            Err(Fault::Suspended) => {
                cascade.queue.push_front(item);
                return Err(Fault::Suspended);
            }
            Err(fault) => return Err(fault),
        };
        cascade.steps += 1;
        if let Some(v) = visit {
            visited.record(v);
        }
        let CascadeItem {
            trait_name: t,
            event: ev,
            payload: pl,
            delivery,
            fans_out,
            ..
        } = item;

        // Fan out from the trait that just handled this event (source = `t`): the dispatched
        // event itself (unless it is a listener's private trigger), plus any events it emitted.
        let StepFollowups {
            own,
            fanned,
            consumed_emits,
        } = self.step_followups(&t, &ev, &pl, result.as_ref(), &[], fans_out);
        let mut result = result;
        if let Some(result) = result.as_mut() {
            result.consumed_emits = consumed_emits;
        }
        if let Some(result) = &result {
            memory.record_delivery(&t, &delivery, view);
            if result.executed {
                memory.record_transition(&t, &result.previous_state, &result.new_state);
            }
        }
        if let Some(result) = result {
            cascade
                .queue
                .extend(self.mount.write().release_after(&t, &ev));
            outcomes.push((t, result.released()));
        }
        for item in own.into_iter().rev() {
            cascade.queue.push_front(item);
        }
        cascade.queue.extend(fanned);
        Ok(true)
    }
}

/// How an inbound envelope was received: judged and refused, or admitted and started as a dispatch.
pub enum InboundStart {
    Declined(EnvelopeVerdict),
    Admitted(Box<Result<Progress<DispatchOutcomes>, DispatchError>>),
}

impl RuntimeKernel {
    /// Run an inbound [`Envelope`] as a fresh dispatch of `envelope.event` with `envelope.payload` at its target,
    /// carrying the envelope's hop and causal id so the emits it makes continue the chain (EXEC-QUEUE-9). The
    /// verdict is `Accepted` once the dispatch has run, `Duplicate` for an id already accepted from the sender,
    /// `Rejected` for `hop-cap`, `unknown-instance`, or `down` when the dispatch itself failed.
    pub fn dispatch_envelope(&self, envelope: Envelope) -> EnvelopeVerdict {
        let _lane = match self.lane.enter(None) {
            Ok(lane) => lane,
            Err(error) => return Self::envelope_down(&envelope, &error),
        };
        let admitted = match self.admit_envelope(&envelope) {
            Ok(admitted) => admitted,
            Err(verdict) => return verdict,
        };
        let run = self.run_of_envelope(envelope.event.clone(), envelope.payload.clone(), admitted);
        match self.run_straight_through(run) {
            Ok(_) => EnvelopeVerdict::Accepted,
            Err(error) => Self::envelope_down(&envelope, &error),
        }
    }

    fn envelope_down(envelope: &Envelope, error: &KernelError) -> EnvelopeVerdict {
        tracing::error!(
            target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
            envelope = %envelope.id,
            error = %error,
            "envelope:dispatch-failed",
        );
        EnvelopeVerdict::Rejected(DeliverRefusal {
            kind: DeliverRefusalKind::Down,
            id: envelope.id.clone(),
        })
    }

    /// [`Self::dispatch_envelope`] as a resumable dispatch: the host answers the dispatch's asks, and `Accepted`
    /// is its `Progress::Done`.
    pub fn start_dispatch_envelope(&self, envelope: Envelope) -> InboundStart {
        let lane = match self.lane.enter(None) {
            Ok(lane) => lane,
            Err(error) => return InboundStart::Admitted(Box::new(Err(error.into()))),
        };
        let admitted = match self.admit_envelope(&envelope) {
            Ok(admitted) => admitted,
            Err(verdict) => return InboundStart::Declined(verdict),
        };
        let run = self.run_of_envelope(envelope.event, envelope.payload, admitted);
        InboundStart::Admitted(Box::new(self.drive_entry(lane, Self::continuation_of(run))))
    }

    fn run_of_envelope(
        &self,
        event: String,
        payload: Value,
        admitted: super::envelope::Admitted,
    ) -> DispatchRun {
        let mut run = self.run_of_each(&admitted.targets, None, &event, payload, None, None);
        run.origin.inbound = Some(admitted.inbound);
        run
    }
}
