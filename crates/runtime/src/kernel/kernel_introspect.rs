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
}

impl std::fmt::Debug for StepKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepKernel").finish_non_exhaustive()
    }
}

impl StepKernel {
    pub(super) fn new(io: Arc<IoPort>, grants: Arc<RwLock<OsGrantTable>>) -> Self {
        Self { enabled: AtomicBool::new(false), position: RwLock::new(Position::default()), io, grants }
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
