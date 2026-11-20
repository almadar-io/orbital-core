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
        Self { enabled: AtomicBool::new(false), position: RwLock::new(Position::default()), io, grants, guests }
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
        const OP: &str = "kernel/step";
        let need = OsRequirement::Kernel { kind: KernelKind::Step, operator: None };
        if !(self.enabled() && self.grants.read().allows(self.position.read().orbital.as_deref(), &need)) {
            return Err(format!("{OP}: denied: no `kernel: {{ \"guest\": [step] }}` grant covers it"));
        }
        let Value::String(handle) = guest else {
            return Err(format!("{OP}: guest must be the handle program/open answered"));
        };
        let session = self
            .guests
            .upgrade()
            .and_then(|open| open.read().get(handle.as_str()).cloned())
            .ok_or_else(|| format!("{OP}: no open guest '{}' (it was never opened or is closed)", handle.as_str()))?;
        let text = |field: &str| {
            request
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("{OP}: the request's `{field}` must be a string"))
        };
        let delivery = match request.get("delivery") {
            None | Some(Value::Null) => None,
            Some(named) => Some(
                named
                    .get("at")
                    .and_then(Value::as_number)
                    .filter(|at| *at >= 0.0 && at.fract() == 0.0)
                    .map(|at| at as usize)
                    .ok_or_else(|| format!("{OP}: the request's `delivery.at` must be the listen's position, a whole number"))?,
            ),
        };
        let leg = match request.get("leg") {
            None | Some(Value::Null) => None,
            Some(Value::String(leg)) if session.host.is_some() => Some(leg.to_string()),
            Some(Value::String(_)) => return Err(format!("{OP}: a leg is a hosted split guest's; this guest has no host")),
            Some(_) => return Err(format!("{OP}: the request's `leg` must be the handle program/dispatch {{ lolo: \"begin\" }} answered")),
        };
        let trait_name = text("trait")?;
        let from = match (&leg, request.get("from")) {
            (Some(_), None | Some(Value::Null)) => None,
            _ => Some(text("from")?),
        };
        let tick = match request.get("tick") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if delivery.is_none() => Some(name.to_string()),
            Some(Value::String(_)) => return Err(format!("{OP}: a tick is not a delivery; name `tick` or `delivery`, not both")),
            Some(_) => return Err(format!("{OP}: the request's `tick` must be the tick's name")),
        };
        let event = match (delivery, &tick) {
            (Some(_), _) | (None, Some(_)) => request.get("event").and_then(Value::as_str).unwrap_or_default().to_string(),
            (None, None) => text("event")?,
        };
        let entry = match request.get("entry") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) if !*flag || delivery.is_none() => *flag,
            Some(Value::Bool(_)) => return Err(format!("{OP}: a delivery is not an entry; `entry` names a dispatch's own event")),
            Some(_) => return Err(format!("{OP}: the request's `entry` must be a boolean")),
        };
        let mount = match request.get("mount") {
            None | Some(Value::Null) => None,
            Some(_) if delivery.is_none() => return Err(format!("{OP}: `mount` names the route a `$MOUNT` delivery announces; it needs `delivery`")),
            Some(named) => Some((
                named.get("route").and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{OP}: the request's `mount.route` must be a string"))?,
                match named.get("crumb") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(crumb)) => Some(crumb.to_string()),
                    Some(_) => return Err(format!("{OP}: the request's `mount.crumb` must be a string")),
                },
            )),
        };
        let emitter = match request.get("emitter") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if delivery.is_some() => Some(name.to_string()),
            Some(Value::String(_)) => return Err(format!("{OP}: `emitter` names the trait a delivery's emit came from; it needs `delivery`")),
            Some(_) => return Err(format!("{OP}: the request's `emitter` must be a string")),
        };
        let entity_id = match request.get("entityId") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id.to_string()),
            Some(_) => return Err(format!("{OP}: the request's `entityId` must be a string")),
        };
        let now = match request.get("now") {
            None | Some(Value::Null) => self.position.read().now,
            Some(value) => value.as_number().ok_or_else(|| format!("{OP}: the request's `now` must be a number of ms"))?,
        };
        let payload = request.get("payload").cloned().unwrap_or(Value::Null);
        let entity = request.get("entity").cloned();
        let embedded = session.embedded_traits();
        let kernel = Arc::clone(&session.kernel);
        let host = session.host.clone();
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
        match (&session.host, request.get("leg").and_then(Value::as_str)) {
            (Some(host), Some(leg)) => host.in_leg(leg, Box::new(run)),
            _ => run(),
        }
        .map_err(|error| format!("{OP}: {error}"))
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
                    return Ok(not_delivered(&input, None));
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
        let commit = input.leg.is_some();
        let held = self.snapshot_trait(input.trait_name, entity_id);
        let held = if held.state.is_empty() {
            TraitSnapshot { state: self.get_initial_state(input.trait_name).unwrap_or_default(), entity: held.entity }
        } else {
            held
        };
        let before = match input.entity {
            Some(entity) => entity,
            None if commit => self.step_frame(input.trait_name, entity_id.unwrap_or(crate::process_result::RUNTIME_SINGLETON)).to_value(),
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
        if !commit {
            self.restore_trait(input.trait_name, entity_id, held);
            self.renders.write().forget_traits([input.trait_name]);
        }
        let result = ran.map_err(|fault| fault.into_kernel().to_string())?;
        let mut answer = step_answer(&result, keyed.as_deref(), before);
        let ran = [(input.trait_name.to_string(), result.clone())];
        let response = crate::effects::response::EventResponse::from_process_results(&ran, input.trait_name, event);
        let (rendered, navigations) = super::guests::paints_of(&response, &super::guests::painter_orbitals(self, &ran), &input.embedded);
        if let Value::Object(fields) = &mut answer {
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
