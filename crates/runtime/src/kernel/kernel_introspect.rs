//! The kernel's view of its own step (EXEC-METAL-5/6): the pending worklist, the step's frame and stamp that a pure
//! `kernel/*` read answers, and the `kernel/queue-front|back` directives a step stages and the cascade applies after it.

use super::*;
use orbital_core_base::os::OsOp;
use orbital_core_base::os_grants::{KernelKind, OsRequirement};
use orbital_core_eval::evaluator::context::KernelIntrospect;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
struct WorklistEntry {
    event: String,
    trait_name: String,
    orbital: String,
}

/// Where the running step stands, recorded when it begins and only while a loaded program declares a `kernel` grant.
#[derive(Debug, Default)]
struct Position {
    trait_name: String,
    state: String,
    event: String,
    entity_id: String,
    orbital: Option<String>,
    now: f64,
    dispatch: u64,
    step: u64,
    worklist: Arc<Vec<WorklistEntry>>,
}

/// What a step's pure `kernel/*` reads see; one instance serves every step.
pub(super) struct StepKernel {
    enabled: AtomicBool,
    position: RwLock<Position>,
    io: Arc<IoPort>,
    grants: Arc<RwLock<OsGrantTable>>,
    /// The kernel's open guests, which `kernel/step` steps (EXEC-METAL-7).
    guests: std::sync::Weak<RwLock<HashMap<String, super::guests::GuestSession>>>,
    /// The kernel this is the step reader of, which `kernel/step "self"` steps; set once the kernel is shared.
    owner: std::sync::OnceLock<std::sync::Weak<RuntimeKernel>>,
}

impl std::fmt::Debug for StepKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepKernel").finish_non_exhaustive()
    }
}

impl StepKernel {
    pub(super) fn new(
        io: Arc<IoPort>,
        grants: Arc<RwLock<OsGrantTable>>,
        guests: std::sync::Weak<RwLock<HashMap<String, super::guests::GuestSession>>>,
    ) -> Self {
        Self { enabled: AtomicBool::new(false), position: RwLock::new(Position::default()), io, grants, guests, owner: std::sync::OnceLock::new() }
    }

    pub(super) fn set_owner(&self, owner: std::sync::Weak<RuntimeKernel>) {
        let _ = self.owner.set(owner);
    }

    pub(super) fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

}

fn read_need() -> OsRequirement {
    OsRequirement::Kernel { kind: KernelKind::Read, operator: None }
}

fn record<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::object(fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
}

fn count(value: u64) -> Value {
    Value::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

impl KernelIntrospect for StepKernel {
    fn may_read(&self) -> bool {
        self.enabled() && self.grants.read().allows(self.position.read().orbital.as_deref(), &read_need())
    }

    fn worklist(&self) -> Value {
        let position = self.position.read();
        Value::Array(
            position
                .worklist
                .iter()
                .map(|entry| {
                    record([
                        ("event", Value::string(entry.event.as_str())),
                        ("trait", Value::string(entry.trait_name.as_str())),
                        ("orbital", Value::string(entry.orbital.as_str())),
                    ])
                })
                .collect::<Vec<Value>>()
                .into(),
        )
    }

    fn frame(&self) -> Value {
        let position = self.position.read();
        record([
            ("trait", Value::string(position.trait_name.as_str())),
            ("state", Value::string(position.state.as_str())),
            ("event", Value::string(position.event.as_str())),
            ("entityId", Value::string(position.entity_id.as_str())),
        ])
    }

    fn stamp(&self) -> Value {
        let position = self.position.read();
        record([
            ("now", Value::Int(position.now as i64)),
            ("dispatch", count(position.dispatch)),
            ("step", count(position.step)),
            ("journal", count(self.io.claimed_prefix() as u64)),
        ])
    }

    fn may_apply(&self, operator: &str) -> bool {
        let need = OsRequirement::Kernel { kind: KernelKind::Call, operator: Some(operator.to_string()) };
        self.grants.read().allows(self.position.read().orbital.as_deref(), &need)
    }

    fn step(&self, guest: &Value, request: &Value) -> Result<Value, String> {
        Ok(self.stepped(guest, request, false).unwrap_or_else(|failure| refused_step(request, failure)))
    }
}

impl StepKernel {
    /// `kernel/commit`: `kernel/step`'s request, the step kept.
    pub(super) fn commit(&self, guest: &Value, request: &Value) -> Result<Value, String> {
        self.stepped(guest, request, true)
    }

    fn stepped(&self, guest: &Value, request: &Value, committing: bool) -> Result<Value, String> {
        let op = if committing { "kernel/commit" } else { "kernel/step" };
        let need = OsRequirement::Kernel { kind: KernelKind::Step, operator: None };
        if !(self.enabled() && self.grants.read().allows(self.position.read().orbital.as_deref(), &need)) {
            return Err(format!("{op}: denied: no `kernel: {{ \"guest\": [step] }}` grant covers it"));
        }
        let Value::String(handle) = guest else {
            return Err(format!("{op}: guest must be the handle program/open answered"));
        };
        // "self" names the kernel the step runs on: a client kernel's .lolo kernel stepping its own traits inside the
        // client dispatch it runs, each step committed (EXEC-METAL-7).
        let stepping_self = handle.as_str() == "self";
        if stepping_self && !committing {
            return Err(format!("{op}: a step of \"self\" always commits; step it with kernel/commit"));
        }
        let (step_kernel, step_host, embedded): (Arc<RuntimeKernel>, Option<Arc<dyn super::guest_hosting::GuestHost>>, _) = if stepping_self {
            let owner = self
                .owner
                .get()
                .and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| format!("{op}: \"self\" names the kernel the step runs on, and this one is not shared"))?;
            let host: Arc<dyn super::guest_hosting::GuestHost> = Arc::new(SelfHost(Arc::downgrade(&owner)));
            (owner, Some(host), Default::default())
        } else {
            let session = self
                .guests
                .upgrade()
                .and_then(|open| open.read().get(handle.as_str()).cloned())
                .ok_or_else(|| format!("{op}: no open guest '{}' (it was never opened or is closed)", handle.as_str()))?;
            (Arc::clone(&session.kernel), session.host.clone(), session.embedded_traits())
        };
        let text = |field: &str| {
            request
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("{op}: the request's `{field}` must be a string"))
        };
        let delivery = match request.get("delivery") {
            None | Some(Value::Null) => None,
            Some(named) => Some(
                named
                    .get("at")
                    .and_then(Value::as_number)
                    .filter(|at| *at >= 0.0 && at.fract() == 0.0)
                    .map(|at| at as usize)
                    .ok_or_else(|| format!("{op}: the request's `delivery.at` must be the listen's position, a whole number"))?,
            ),
        };
        let leg = match request.get("leg") {
            None | Some(Value::Null) => None,
            Some(Value::String(_)) if !committing => return Err(format!("{op}: a leg's steps commit; step it with kernel/commit")),
            Some(Value::String(leg)) if step_host.is_some() && !stepping_self => Some(leg.to_string()),
            Some(Value::String(_)) => return Err(format!("{op}: a leg is a hosted split guest's; this guest has no host")),
            Some(_) => return Err(format!("{op}: the request's `leg` must be the handle program/dispatch {{ lolo: \"begin\" }} answered")),
        };
        let held_run = match request.get("run") {
            None | Some(Value::Null) => None,
            Some(Value::String(run)) if leg.is_some() => Some(run.to_string()),
            Some(Value::String(_)) => return Err(format!("{op}: a held completion's `run` is stepped inside the leg settle handed it to; name `leg`")),
            Some(_) => return Err(format!("{op}: the request's `run` must be the handle program/dispatch {{ lolo: \"settle\" }} answered")),
        };
        if !matches!(request.get("commit"), None | Some(Value::Null)) {
            return Err(format!("{op}: a request names no `commit`: kernel/step never keeps a step and kernel/commit always does"));
        }
        let commit = committing;
        let trait_name = text("trait")?;
        let from = match (&leg, request.get("from")) {
            (Some(_), None | Some(Value::Null)) => None,
            (None, None | Some(Value::Null)) if stepping_self => None,
            _ => Some(text("from")?),
        };
        let tick = match request.get("tick") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if delivery.is_none() => Some(name.to_string()),
            Some(Value::String(_)) => return Err(format!("{op}: a tick is not a delivery; name `tick` or `delivery`, not both")),
            Some(_) => return Err(format!("{op}: the request's `tick` must be the tick's name")),
        };
        let event = match (delivery, &tick) {
            (Some(_), _) | (None, Some(_)) => request.get("event").and_then(Value::as_str).unwrap_or_default().to_string(),
            (None, None) => text("event")?,
        };
        let entry = match request.get("entry") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) if !*flag || delivery.is_none() => *flag,
            Some(Value::Bool(_)) => return Err(format!("{op}: a delivery is not an entry; `entry` names a dispatch's own event")),
            Some(_) => return Err(format!("{op}: the request's `entry` must be a boolean")),
        };
        let mount = match request.get("mount") {
            None | Some(Value::Null) => None,
            Some(_) if delivery.is_none() => return Err(format!("{op}: `mount` names the route a `$MOUNT` delivery announces; it needs `delivery`")),
            Some(named) => Some((
                named.get("route").and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{op}: the request's `mount.route` must be a string"))?,
                match named.get("crumb") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(crumb)) => Some(crumb.to_string()),
                    Some(_) => return Err(format!("{op}: the request's `mount.crumb` must be a string")),
                },
            )),
        };
        let emitter = match request.get("emitter") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if delivery.is_some() => Some(name.to_string()),
            Some(Value::String(_)) => return Err(format!("{op}: `emitter` names the trait a delivery's emit came from; it needs `delivery`")),
            Some(_) => return Err(format!("{op}: the request's `emitter` must be a string")),
        };
        let entity_id = match request.get("entityId") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id.to_string()),
            Some(_) => return Err(format!("{op}: the request's `entityId` must be a string")),
        };
        let now = match request.get("now") {
            None | Some(Value::Null) => self.position.read().now,
            Some(value) => value.as_number().ok_or_else(|| format!("{op}: the request's `now` must be a number of ms"))?,
        };
        let payload = request.get("payload").cloned().unwrap_or(Value::Null);
        let entity = request.get("entity").cloned();
        let kernel = step_kernel;
        let host = step_host;
        let in_leg_host = host.clone();
        let run = move || {
            let from = match from {
                Some(from) => from,
                None => kernel.current_state_name(&trait_name, entity_id.as_deref()),
            };
            let input = IsolatedStep {
                contained: host.is_none(),
                host: host.clone(),
                embedded,
                delivery,
                entry,
                mount,
                tick: tick.as_deref(),
                emitter: emitter.as_deref(),
                leg: leg.as_deref(),
                commit,
                trait_name: &trait_name,
                entity_id: entity_id.as_deref(),
                from: &from,
                event: &event,
                payload,
                entity,
                now,
            };
            kernel.step_isolated(input)
        };
        match (in_leg_host, request.get("leg").and_then(Value::as_str), held_run.as_deref()) {
            (Some(host), Some(leg), Some(held)) => host.in_leg_run(leg, held, Box::new(run)),
            (Some(host), Some(leg), None) => host.in_leg(leg, Box::new(run)),
            _ => run(),
        }
        .map_err(|error| format!("{op}: {error}"))
    }
}

/// The kernel a step runs on, as the host of its own `kernel/step "self"`: each step runs isolated on it, committed,
/// and joins the client dispatch its `.lolo` kernel is running.
struct SelfHost(std::sync::Weak<RuntimeKernel>);

impl SelfHost {
    fn owner(&self) -> Result<Arc<RuntimeKernel>, String> {
        self.0.upgrade().ok_or_else(|| "the kernel has ended".to_string())
    }
}

impl super::guest_hosting::GuestHost for SelfHost {
    fn kernel(&self) -> Arc<RuntimeKernel> {
        self.0.upgrade().expect("a step of \"self\" runs while its kernel lives")
    }
    fn environment_of(&self, _: &str) -> Option<String> {
        None
    }
    fn credential_of(&self, _: &str) -> Option<String> {
        None
    }
    fn deferred_denials(&self) -> Vec<(String, u64)> {
        Vec::new()
    }
    fn register(&self, _: OrbitalSchema) -> Result<(), String> {
        Err("a kernel stepping itself registers nothing".to_string())
    }
    fn mount(&self, _: Option<&str>, _: &[crate::effects::MountSeed], _: Value) -> Result<crate::resumable::DispatchOutcomes, String> {
        Err("a kernel stepping itself mounts nothing".to_string())
    }
    fn dispatch(&self, _: &str, _: &str, _: Value) -> Result<crate::resumable::DispatchOutcomes, String> {
        Err("a kernel stepping itself runs no whole dispatch".to_string())
    }
    fn request(&self, _: &Value, _: &orbital_core_base::value::ValueMap) -> Result<Value, crate::effects::executor::SubstrateFailure> {
        Err(crate::agent_ports::guest_failure(orbital_core_base::os::OsErrorKind::Unsupported, "a kernel stepping itself takes no events request"))
    }
    fn tick_pass(&self) -> Result<crate::resumable::DispatchOutcomes, String> {
        Ok(Vec::new())
    }
    fn step(&self, trait_name: &str, row: Option<&str>, event: &str, payload: Value, tick: Option<&str>, _leg: Option<&str>) -> Result<crate::resumable::DispatchOutcomes, String> {
        self.owner()?.step_self_run(trait_name, row, event, payload, tick)
    }
}

/// One `kernel/step` request, read from its record.
pub(super) struct IsolatedStep<'a> {
    /// The guest has no host: its own rows answer its fetches and persists and every other ask fails in place, as a
    /// native dispatch into it does, so a step may run anything. A hosted guest's asks reach its host and are refused.
    contained: bool,
    /// The host of a hosted guest: it runs the step, I/O included (`GuestHost::step`).
    host: Option<Arc<dyn super::guest_hosting::GuestHost>>,
    /// The traits the guest's page embeds: each paint says whether its trait is one.
    embedded: std::collections::HashSet<String>,
    /// The position of the listen of `trait_name` this step is a delivery through: its `when`, `per` and `with` run on
    /// the raw `payload` first, and its trigger is the event fired.
    delivery: Option<usize>,
    /// The step is a dispatch's own event entering the kernel: its payload is checked against the event the trait
    /// declares first, as a dispatch's entry does (EXEC-EVAL-10), and a misfit runs nothing.
    entry: bool,
    /// The delivery is a route mount's `$MOUNT` (EXEC-SEED-7): its payload is the announcement of this route (and crumb),
    /// built by the guest as a native route mount builds it, in place of `payload`.
    mount: Option<(String, Option<String>)>,
    /// The tick of `trait_name` this step runs (its `appliesTo`, `afterEntryMs` and guard judged as a tick pass judges
    /// them, no payload), in place of an event.
    tick: Option<&'a str>,
    /// The open server leg this step commits into (`program/dispatch { lolo: "begin" }`): the step runs in the leg's
    /// session, its writes stay and its outcome joins the leg's.
    leg: Option<&'a str>,
    /// The step's writes and moves stay (`commit: true`, or a `leg`): nothing is restored after it.
    commit: bool,
    /// The trait whose emit this delivery carries: a refused `per` key is answered to it as `DELIVERY_REJECTED`, as a
    /// native delivery's is, or refuses the dispatch when it declares no listener for it.
    emitter: Option<&'a str>,
    trait_name: &'a str,
    entity_id: Option<&'a str>,
    from: &'a str,
    event: &'a str,
    payload: Value,
    /// The row the step reads; absent, the trait's (or keyed row's) row before any frame: declared defaults, then its
    /// stored row, as a native step reads a trait nothing has touched.
    entity: Option<Value>,
    now: f64,
}

/// The effects a step may run as data; every other effect with a host or a side effect is an ask.
const STEP_EFFECTS: [&str; 7] = ["emit", "set", "render-ui", "navigate", "navigate-back", "log", "notify"];


/// The asks in `expr`, in walk order: each call whose head the operator table gives a host or a side effect and that
/// is not a step effect.
fn asks_in(expr: &SExpression, out: &mut Vec<String>) {
    match expr {
        SExpression::Array(items) => {
            if let Some(head) = items.first().and_then(SExpression::as_str) {
                let asks = !STEP_EFFECTS.contains(&head)
                    && matches!(
                        orbital_core_base::language::registry::lookup_operator(head),
                        Ok(Some(declared)) if declared.has_side_effects || declared.host.is_some()
                    );
                if asks && !out.iter().any(|seen| seen == head) {
                    out.push(head.to_string());
                }
            }
            for item in items.iter() {
                asks_in(item, out);
            }
        }
        SExpression::Object(fields) => {
            for value in fields.values() {
                asks_in(value, out);
            }
        }
        _ => {}
    }
}

impl RuntimeKernel {
    /// `kernel/step` on this (guest) kernel: the trait set to `from` and `entity`, one `event` processed with no cascade,
    /// then the trait restored and its renders dropped, so the guest holds nothing a step left. A transition whose
    /// `from --event-->` arms hold an ask is refused before anything runs.
    pub(super) fn step_isolated(&self, mut input: IsolatedStep<'_>) -> Result<Value, String> {
        let trait_def = self
            .registry
            .trait_definitions
            .read()
            .get(input.trait_name)
            .cloned()
            .ok_or_else(|| format!("the guest has no trait \"{}\"", input.trait_name))?;
        if let Some((route, crumb)) = input.mount.take() {
            match self.route_mount_announcement(&route, crumb.as_deref()).map_err(|error| error.to_string())? {
                Some((_, announced)) => input.payload = announced,
                None => return Ok(not_delivered(&input, None)),
            }
        }
        let mut keyed: Option<String> = None;
        let trigger: String;
        if let Some(at) = input.delivery {
            let listen = trait_def
                .listens
                .as_ref()
                .and_then(|all| all.get(at).cloned())
                .ok_or_else(|| format!("trait \"{}\" has no listen at position {at}", input.trait_name))?;
            trigger = listen.triggers.clone().unwrap_or_else(|| listen.event.clone());
            let passes = |guard: &SExpression| -> Result<bool, String> {
                let config = self.listener_config(input.trait_name).map_err(|fault| fault.into_kernel().to_string())?;
                Ok(listen_guard_passes(guard, &input.payload, input.from, config, &DispatchView::default()))
            };
            if let Some(expr) = &listen.key {
                let user = self.session_user().map_err(|fault| fault.into_kernel().to_string())?;
                let key = ListenKey { expr: Arc::new(expr.clone()), emitter: String::new() };
                match self.resolve_listen_key(&key, &input.payload, user) {
                    Ok(row) => match input.entity_id {
                        None => return Ok(key_required(&input, &row)),
                        Some(named) if named != row => {
                            return Err(format!("the delivery's per key names row \"{row}\", not the row \"{named}\" the request names"))
                        }
                        Some(_) => keyed = Some(row),
                    },
                    Err(reason) => {
                        let blocked = match &listen.guard {
                            Some(guard) => {
                                let config = self.listener_config(input.trait_name).map_err(|fault| fault.into_kernel().to_string())?;
                                let initial = self.get_initial_state(input.trait_name).unwrap_or_default();
                                !listen_guard_passes(guard, &input.payload, &initial, config, &DispatchView::default())
                            }
                            None => false,
                        };
                        if blocked {
                            return Ok(not_delivered(&input, None));
                        }
                        let Some(emitter) = input.emitter else {
                            return Ok(not_delivered(&input, Some(reason)));
                        };
                        return Ok(match self.refuse_key_of(input.trait_name, &trigger, emitter, &reason, &mut Vec::new()) {
                            Err(refusal) => rejected(&input, refusal.to_string()),
                            Ok(payload) => {
                                let mut answer = not_delivered(&input, Some(reason));
                                if let Value::Object(fields) = &mut answer {
                                    fields.insert("deliveryRejected".into(), payload);
                                }
                                answer
                            }
                        });
                    }
                }
            }
            if let Some(guard) = &listen.guard {
                if !passes(guard)? {
                    let mut answer = not_delivered(&input, None);
                    if let (Some(row), Value::Object(fields)) = (keyed.as_deref(), &mut answer) {
                        let entity_type = trait_def.linked_entity.as_deref().unwrap_or(input.trait_name);
                        let held = match &input.entity {
                            Some(entity) => SharedEntity::from_value(entity.clone()),
                            None => self.unframed_trait_entity(input.trait_name, entity_type, row),
                        };
                        fields.insert("quiet".into(), Value::Bool(self.row_quiet(&trait_def, entity_type, row, input.from, &held)));
                    }
                    return Ok(answer);
                }
            }
            if let Some(mapping) = listen.payload_mapping.as_ref() {
                let ctx = BindingContextBuilder::new().payload(input.payload.clone()).build();
                input.payload = super::envelope::mapped_listen_payload(&Evaluator::new(), &ctx, Some(mapping), &input.payload);
            }
        } else {
            trigger = input.event.to_string();
        }
        let event = trigger.as_str();
        if input.entry {
            if let Some(message) = self.payload_rejection([(input.trait_name, event)], &input.payload) {
                return Ok(rejected(&input, message));
            }
        }
        let entity_id = keyed.as_deref().or(input.entity_id);
        let mut asks = Vec::new();
        for arm in trait_def
            .state_machine
            .transitions
            .iter()
            .filter(|t| input.tick.is_none() && t.event == event && t.from.states().contains(&input.from))
        {
            if input.contained {
                continue;
            }
            for effect in arm.effects.iter().flatten() {
                asks_in(effect, &mut asks);
            }
        }
        for declared in trait_def.ticks.iter().flatten().filter(|t| Some(t.name.as_str()) == input.tick) {
            if input.contained {
                continue;
            }
            for effect in &declared.effects {
                asks_in(effect, &mut asks);
            }
        }
        if !asks.is_empty() && input.host.is_none() {
            return Ok(record([
                ("fired", Value::Bool(false)),
                ("from", Value::string(input.from)),
                ("to", Value::string(input.from)),
                ("arm", Value::Null),
                ("entity", input.entity.unwrap_or(Value::Null)),
                ("emits", Value::array(Vec::<Value>::new())),
                ("effects", Value::array(Vec::<Value>::new())),
                ("guards", Value::array(Vec::<Value>::new())),
                ("failure", Value::Null),
                ("refused", Value::string("io")),
                ("asks", Value::array(asks.into_iter().map(Value::string).collect::<Vec<Value>>())),
                ("delivered", Value::Bool(true)),
                ("key", keyed.as_deref().map_or(Value::Null, Value::string)),
                ("keyRefused", Value::Null),
                ("keyRequired", Value::Bool(false)),
                ("rejected", Value::Null),
            ]));
        }
        let _lane = self.lane.enter(None).map_err(|e| e.to_string())?;
        let commit = input.commit;
        let held = self.snapshot_trait(input.trait_name, entity_id);
        let unmounted = held.state.is_empty();
        let held = if held.state.is_empty() {
            TraitSnapshot { state: self.get_initial_state(input.trait_name).unwrap_or_default(), entity: held.entity }
        } else {
            held
        };
        let before = match input.entity {
            Some(entity) => entity,
            None if commit => {
                let row = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
                // A guest with no host keeps the step in this kernel: its keyed row is made on first use, as a native delivery makes it.
                if input.host.is_none() && self.is_keyed_row(input.trait_name, row) && self.keyed_instance_missing(input.trait_name, row) {
                    self.create_keyed_row(input.trait_name, row).map_err(|fault| fault.into_kernel().to_string())?;
                }
                self.step_frame(input.trait_name, row).to_value()
            }
            None => {
                let row = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
                if row != crate::process_result::RUNTIME_SINGLETON {
                    self.create_keyed_row(input.trait_name, row).map_err(|fault| fault.into_kernel().to_string())?;
                }
                let entity_type = trait_def.linked_entity.as_deref().unwrap_or(input.trait_name);
                self.unframed_trait_entity(input.trait_name, entity_type, row).to_value()
            }
        };
        if !commit {
            self.restore_trait(input.trait_name, entity_id, TraitSnapshot { state: input.from.to_string(), entity: before.clone() });
            self.renders.write().forget_traits([input.trait_name]);
        }
        *self.step.dispatch_now.write() = Some(input.now);
        self.step.queue_directives.write().clear();
        self.step.isolated_directives.write().clear();
        let row = entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON);
        let ran = match (&input.host, input.tick) {
            (Some(host), tick) => host
                .step(input.trait_name, if tick.is_some() { Some(row) } else { entity_id }, event, input.payload, tick, input.leg)
                .and_then(|outcomes| {
                    outcomes
                        .into_iter()
                        .find(|(name, _)| name == input.trait_name)
                        .map(|(_, result)| result)
                        .ok_or_else(|| format!("the host ran no step of {}", input.trait_name))
                })
                .map_err(|error| Fault::Kernel(KernelError::Internal(error))),
            (None, Some(tick)) => self.process_tick_in_dispatch(input.trait_name, row, row, tick),
            (None, None) => self.process_event_in_dispatch(input.trait_name, entity_id, event, Arc::new(input.payload), None, false),
        };
        *self.step.dispatch_now.write() = None;
        let mut directed = self.take_queue_directives();
        directed.extend(std::mem::take(&mut *self.step.isolated_directives.write()));
        if !commit {
            self.restore_trait(input.trait_name, entity_id, held);
            if unmounted {
                let state_key = self.state_key(Some(row));
                self.state_manager.write().remove_trait_instance(input.trait_name, state_key.as_deref());
            }
            self.renders.write().forget_traits([input.trait_name]);
        }
        let result = ran.map_err(|fault| fault.into_kernel().to_string())?;
        let mut answer = step_answer(&result, keyed.as_deref(), before);
        let ran = [(input.trait_name.to_string(), result.clone())];
        let response = crate::effects::response::EventResponse::from_process_results(&ran, input.trait_name, event);
        let (rendered, navigations) = super::guests::paints_of(&response, &super::guests::painter_orbitals(self, &ran), &input.embedded);
        if let Value::Object(fields) = &mut answer {
            let seen = |emit: &Value| -> Value {
                let Value::Object(emit) = emit else { return emit.clone() };
                let mut shown = emit.get("payload").cloned();
                if let Some(Value::String(event)) = emit.get("event") {
                    self.redact_sealed_payload(input.trait_name, event.as_str(), &mut shown);
                }
                let mut emit = (**emit).clone();
                emit.insert("seen".into(), shown.unwrap_or(Value::Null));
                Value::object(emit)
            };
            if let Some(Value::Array(emits)) = fields.get("emits") {
                let emits = emits.iter().map(seen).collect::<Vec<Value>>();
                fields.insert("emits".into(), Value::array(emits));
            }
            let queue = directed.into_iter().map(|(event, front)| record([("event", Value::string(event)), ("front", Value::Bool(front))]));
            fields.insert("queue".into(), Value::array(queue.collect::<Vec<Value>>()));
            fields.insert("rendered".into(), Value::array(rendered));
            fields.insert("navigations".into(), Value::array(navigations));
        }
        if let (Some(row), Value::Object(fields)) = (entity_id.filter(|row| self.is_keyed_row(input.trait_name, row)), &mut answer) {
            let entity_type = trait_def.linked_entity.as_deref().unwrap_or(input.trait_name);
            fields.insert("quiet".into(), Value::Bool(self.row_quiet(&trait_def, entity_type, row, &result.new_state, &result.entity_data)));
        }
        if let (Some(message), Value::Object(fields)) = (self.emit_rejection(input.trait_name, Some(&result)), &mut answer) {
            fields.insert("rejected".into(), Value::string(message));
        }
        Ok(answer)
    }
}

/// The answer of a keyed delivery that named no row: its `per` key, for the caller to step that row (EXEC-STEP-1a).
/// The answer of an entry whose payload does not fit its declared event: nothing ran, and `rejected` carries the
/// refusal a native dispatch answers.
fn rejected(input: &IsolatedStep<'_>, message: String) -> Value {
    let mut answer = not_delivered(input, None);
    if let Value::Object(fields) = &mut answer {
        fields.insert("rejected".into(), Value::string(message));
    }
    answer
}

/// The answer of a `kernel/step` that could not run (G-STD-446): nothing ran, and `failure` carries the reason.
fn refused_step(request: &Value, failure: String) -> Value {
    let from = request.get("from").and_then(Value::as_str).map_or(Value::Null, Value::string);
    record([
        ("fired", Value::Bool(false)),
        ("delivered", Value::Bool(false)),
        ("from", from.clone()),
        ("to", from),
        ("arm", Value::Null),
        ("entity", request.get("entity").cloned().unwrap_or(Value::Null)),
        ("emits", Value::array(Vec::<Value>::new())),
        ("effects", Value::array(Vec::<Value>::new())),
        ("guards", Value::array(Vec::<Value>::new())),
        ("failure", Value::string(failure)),
        ("refused", Value::Null),
        ("asks", Value::array(Vec::<Value>::new())),
        ("key", Value::Null),
        ("keyRefused", Value::Null),
        ("keyRequired", Value::Bool(false)),
        ("rejected", Value::Null),
    ])
}

fn key_required(input: &IsolatedStep<'_>, row: &str) -> Value {
    let mut answer = not_delivered(input, None);
    if let Value::Object(fields) = &mut answer {
        fields.insert("key".into(), Value::string(row));
        fields.insert("keyRequired".into(), Value::Bool(true));
    }
    answer
}

/// The answer of a delivery its listen held back: by its `when` guard (`refused` none) or by a `per` key that did not name
/// a row (`refused` the reason). Nothing ran.
fn not_delivered(input: &IsolatedStep<'_>, key_refused: Option<String>) -> Value {
    record([
        ("fired", Value::Bool(false)),
        ("delivered", Value::Bool(false)),
        ("from", Value::string(input.from)),
        ("to", Value::string(input.from)),
        ("arm", Value::Null),
        ("entity", input.entity.clone().unwrap_or(Value::Null)),
        ("emits", Value::array(Vec::<Value>::new())),
        ("effects", Value::array(Vec::<Value>::new())),
        ("guards", Value::array(Vec::<Value>::new())),
        ("failure", Value::Null),
        ("refused", Value::Null),
        ("asks", Value::array(Vec::<Value>::new())),
        ("key", Value::Null),
        ("keyRefused", key_refused.map_or(Value::Null, Value::string)),
        ("keyRequired", Value::Bool(false)),
        ("rejected", Value::Null),
    ])
}

fn step_answer(result: &ProcessEventResult, key: Option<&str>, before: Value) -> Value {
    let emits = result
        .effect_results
        .iter()
        .filter(|ran| !ran.is_failed())
        .filter_map(|ran| match &ran.effect {
            Effect::Emit { event, payload } => Some(record([
                ("event", Value::string(event.as_str())),
                ("payload", payload.clone()),
                ("completion", Value::Bool(ran.completion)),
            ])),
            _ => None,
        })
        .collect::<Vec<Value>>();
    let effects = result
        .effect_results
        .iter()
        .filter_map(|ran| match &ran.effect {
            Effect::Emit { .. } | Effect::Set { .. } => None,
            Effect::RenderUi { slot, content } => Some(record([
                ("type", Value::string("render-ui")),
                ("slot", Value::string(slot.as_str())),
                ("content", content.clone()),
            ])),
            other => serde_json::to_value(other)
                .ok()
                .and_then(|json| orbital_core_base::json_value::from_json_value::<Value>(&json).ok()),
        })
        .collect::<Vec<Value>>();
    let guards = result
        .guards
        .iter()
        .map(|tried| record([("arm", count(tried.arm as u64)), ("passed", Value::Bool(tried.guard.passed))]))
        .collect::<Vec<Value>>();
    record([
        ("fired", Value::Bool(result.executed)),
        ("from", Value::string(result.previous_state.as_str())),
        ("to", Value::string(result.new_state.as_str())),
        ("arm", result.fired_arm.map_or(Value::Null, |arm| count(arm as u64))),
        ("before", before),
        ("entity", result.entity_data.to_value()),
        ("emits", Value::array(emits)),
        ("effects", Value::array(effects)),
        ("guards", Value::array(guards)),
        ("failure", result.failure.as_deref().map_or(Value::Null, Value::string)),
        ("refused", Value::Null),
        ("asks", Value::array(Vec::<Value>::new())),
        ("delivered", Value::Bool(true)),
        ("key", key.map_or(Value::Null, Value::string)),
        ("keyRefused", Value::Null),
        ("keyRequired", Value::Bool(false)),
        ("rejected", Value::Null),
    ])
}

/// `queue` with every item whose event is `event` moved to the front (or back), keeping their relative order.
fn moved(queue: VecDeque<CascadeItem>, event: &str, front: bool) -> VecDeque<CascadeItem> {
    let (moving, rest): (VecDeque<CascadeItem>, VecDeque<CascadeItem>) = queue.into_iter().partition(|item| item.event == event);
    let (first, second) = if front { (moving, rest) } else { (rest, moving) };
    first.into_iter().chain(second).collect()
}

impl RuntimeKernel {
    /// What a step's pure `kernel/*` reads see.
    pub(super) fn kernel_introspect(&self) -> Arc<dyn KernelIntrospect> {
        self.introspect.clone()
    }

    /// A loaded program declares a `kernel` grant: from now on each step records the position its reads answer from.
    pub(super) fn note_kernel_grants(&self, schema: &OrbitalSchema) {
        let declared = |grants: &Option<OsGrants>| grants.as_ref().is_some_and(|grants| !grants.kernel.is_empty());
        if declared(&schema.os_grants) || schema.orbitals.iter().any(|orbital| declared(&orbital.os_grants)) {
            self.introspect.enable();
        }
    }

    pub(super) fn copy_kernel_grants(&self, from: &Self) {
        if from.introspect.enabled() {
            self.introspect.enable();
        }
    }

    /// The step about to run is the `step`th delivery of its dispatch, with `pending` still on the worklist. Only an
    /// orbital whose grants cover `kernel/*` reads gets the worklist copied.
    pub(super) fn note_step<'a>(&self, trait_name: &str, step: usize, pending: impl Iterator<Item = &'a CascadeItem>) {
        if !self.introspect.enabled() {
            return;
        }
        let orbital = self.emit_source(trait_name).orbital;
        let worklist: Vec<WorklistEntry> = if self.grants.os_grants.read().allows(orbital.as_deref(), &read_need()) {
            pending
                .map(|item| WorklistEntry {
                    event: item.event.clone(),
                    trait_name: item.trait_name.clone(),
                    orbital: self.emit_source(&item.trait_name).orbital.unwrap_or_default(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut position = self.introspect.position.write();
        position.step = step as u64;
        position.worklist = Arc::new(worklist);
    }

    /// The frame the step about to run stands on.
    pub(super) fn note_frame(&self, trait_name: &str, state: &str, event: &str, entity_id: &str, now: f64) {
        if !self.introspect.enabled() {
            return;
        }
        let orbital = self.emit_source(trait_name).orbital;
        let dispatch = self.held_dispatch().unwrap_or(0);
        let mut position = self.introspect.position.write();
        position.trait_name = trait_name.to_string();
        position.state = state.to_string();
        position.event = event.to_string();
        position.entity_id = if entity_id == crate::process_result::RUNTIME_SINGLETON { String::new() } else { entity_id.to_string() };
        position.orbital = orbital;
        position.now = now;
        position.dispatch = dispatch;
    }

    /// A step ended, committed or not: what it staged for the worklist is dropped and its snapshot cleared.
    pub(super) fn settle_kernel_step(&self) {
        self.step.queue_directives.write().clear();
        if self.introspect.enabled() {
            let mut position = self.introspect.position.write();
            if !position.worklist.is_empty() {
                position.worklist = Arc::default();
            }
        }
    }

    /// Answer a `kernel/*` op the kernel owns. `None` for an op outside the family.
    pub(super) fn kernel_os(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Option<Result<Value, OsError>> {
        let front = match op {
            OsOp::QueueFront => true,
            OsOp::QueueBack => false,
            OsOp::KernelCommit => {
                let (Some(guest), Some(request)) = (args.first(), args.get(1)) else {
                    return Some(Err(OsError::new(OsErrorKind::Invalid, "kernel/commit: takes the guest handle and the request")));
                };
                // The answer goes under `step`: its own `key` (a keyed delivery's row) is not the call's key.
                return Some(
                    self.introspect
                        .commit(guest, request)
                        .map(|step| record([("step", step)]))
                        .map_err(|failure| OsError::new(OsErrorKind::Invalid, failure)),
                );
            }
            _ => return None,
        };
        let stage = || -> Result<Value, OsError> {
            let need = OsRequirement::Kernel { kind: KernelKind::Reorder, operator: None };
            if !self.grants.os_grants.read().allows(orbital, &need) {
                return Err(OsError::new(OsErrorKind::Denied, "no kernel grant covers it"));
            }
            let Some(Value::String(event)) = args.first() else {
                return Err(OsError::new(OsErrorKind::Invalid, "the event must be text"));
            };
            self.step.queue_directives.write().push((event.to_string(), front));
            Ok(Value::object(ValueMap::default()))
        };
        Some(stage().map_err(|error| OsError::new(error.kind, format!("{}: {}", op.name(), error.message))))
    }

    /// Apply what the step that just ran asked of the worklist, after its own emits and listeners are queued.
    pub(super) fn apply_queue_directives(&self, queue: &mut VecDeque<CascadeItem>) {
        let directives = self.take_queue_directives();
        self.reorder_queue(queue, directives);
    }

    pub(super) fn take_queue_directives(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut *self.step.queue_directives.write())
    }

    pub(super) fn reorder_queue(&self, queue: &mut VecDeque<CascadeItem>, directives: Vec<(String, bool)>) {
        for (event, front) in &directives {
            *queue = moved(std::mem::take(queue), event, *front);
        }
    }
}
