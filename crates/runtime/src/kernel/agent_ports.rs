//! `program/inputs`, `program/dispatch` and `persist/read-named` (EXEC-AGENT-1): the abilities an LLM tool loop
//! is written with. The kernel owns what each one means; the pure half (reflection, JSON Schema, argument
//! checks, address resolution, the dispatch plan) is `orbital_core_base::agent_ports`, shared with the JS twin.
//!
//! - `program/inputs` reflects the registered programs' declared external inputs, and on request the running states
//!   of one orbital, the kernel's counters and the host's last trace steps. Only `trace` asks the host.
//! - `program/dispatch { inbound, body, headers }` asks the host to deliver to an inbound handler (EXEC-INBOUND-1).
//! - `persist/read-named` reads a persisted entity of the program by a name chosen at run time, through the
//!   ordinary `fetch` (read policy for the dispatch's viewer, default row order).
//! - `program/dispatch` asks the host, once and journaled, to dispatch a declared input AS THE DISPATCH'S
//!   VIEWER through its ordinary input channel (guards, grants and entity policies apply, never a bypass). The
//!   kernel sends the qualified address and the checked payload only, never a principal: the host runs the
//!   dispatch as the viewer it started this dispatch for, and answers the outcome the model is told.

use super::RuntimeKernel;
use crate::effects::executor::SubstrateHandler;
use orbital_core_base::agent_ports::{
    inputs_reads, plan_dispatch, plan_inputs, read_named_result, read_named_target,
    readable_entities_of, AgentPortFailure, DispatchPlan, InputsReads,
};
use orbital_core_base::effect_types::{
    Effect, EffectOutcome, EffectResult, FetchDeliver, FetchEmit,
};
use orbital_core_base::error::OrbitalError;
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_eval::evaluator::context::BindingContext;

/// Every string of `value` that is, whole, one binding token (`@payload.x`): what the program wrote.
fn binding_tokens(value: &Value, out: &mut std::collections::HashSet<String>) {
    match value {
        Value::String(s) if BindingContext::binding_token_end(s, 0) == Some(s.len()) => {
            out.insert(s.clone());
        }
        Value::Array(items) => items.iter().for_each(|item| binding_tokens(item, out)),
        Value::Object(map) => map.values().for_each(|item| binding_tokens(item, out)),
        _ => {}
    }
}

/// Whether a resolved argument still holds a binding token the program wrote and the context cannot resolve.
/// The JS interpreter hands such an argument over as `undefined`, which is not JSON data and fails the call.
fn leaves_unresolved(
    value: &Value,
    written: &std::collections::HashSet<String>,
    ctx: &BindingContext,
) -> bool {
    match value {
        Value::String(s) => written.contains(s) && ctx.resolve_binding(s).is_none(),
        Value::Array(items) => items
            .iter()
            .any(|item| leaves_unresolved(item, written, ctx)),
        Value::Object(map) => map
            .values()
            .any(|item| leaves_unresolved(item, written, ctx)),
        _ => false,
    }
}

fn failure_result(effect: &Effect, failure: AgentPortFailure) -> EffectResult {
    let mut result = EffectResult::failed(effect.clone(), &failure.error);
    if !failure.errors.is_empty() {
        result.return_value = Some(Value::Array(failure.errors));
    }
    result
}

impl RuntimeKernel {
    /// The persisted entities of every registered program, once, in schema order.
    pub fn readable_entities(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for schema in self.schemas.read().values() {
            for name in readable_entities_of(schema) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// The orbital and trait whose step is running: the position a relative address resolves from.
    pub(super) fn caller_position(&self) -> (Option<String>, String) {
        let (caller_trait, _) = self.step_site.read().clone().unwrap_or_default();
        let caller_orbital = self.emit_source(&caller_trait).orbital;
        (caller_orbital, caller_trait)
    }

    /// Run one agent-port effect (a `Custom` effect named by `is_agent_port_operator`), its arguments resolved
    /// against the step's bindings.
    pub(super) fn run_agent_port(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        let Effect::Custom { name, args, .. } = effect else {
            return EffectResult::noop(effect.clone(), "not an agent-port effect");
        };
        let mut resolved: Vec<Value> = Vec::with_capacity(args.len());
        for arg in args {
            match ctx.resolve_value_checked(arg) {
                Ok(value) => resolved.push(value.into_owned()),
                Err(fault) => {
                    return EffectResult::failed(
                        effect.clone(),
                        &OrbitalError::Eval(fault).to_string(),
                    );
                }
            }
        }
        let mut written = std::collections::HashSet::new();
        for arg in args {
            binding_tokens(arg, &mut written);
        }
        if resolved
            .iter()
            .any(|arg| leaves_unresolved(arg, &written, ctx))
        {
            return EffectResult::failed(
                effect.clone(),
                &format!("{name}: arguments must be JSON data"),
            );
        }
        match name.as_str() {
            "program/inputs" => {
                let (caller_orbital, caller_trait) = self.caller_position();
                match plan_inputs(
                    &resolved,
                    &self.external_inputs(),
                    &self.readable_entities(),
                    caller_orbital.as_deref(),
                    &caller_trait,
                ) {
                    Ok(reflection) => match self.with_runtime_reads(reflection, &resolved) {
                        Ok(answer) => EffectResult::executed_with_value(effect.clone(), answer),
                        Err(failure) => failure_result(effect, failure),
                    },
                    Err(failure) => failure_result(effect, failure),
                }
            }
            "persist/read-named" => self.run_read_named(effect, &resolved, ctx),
            "program/dispatch" => self.run_dispatch(effect, &resolved),
            other => EffectResult::noop(
                effect.clone(),
                &format!("{other} is not an agent-port effect"),
            ),
        }
    }

    /// `reflection` with the runtime reads the options ask for: `state` (each running instance of the orbital's traits,
    /// its state and entity), `stats` (the kernel's counters) and `trace` (the host's last n dispatch steps, asked of
    /// the host, which keeps the trace).
    fn with_runtime_reads(
        &self,
        reflection: Value,
        args: &[Value],
    ) -> Result<Value, AgentPortFailure> {
        let reads: InputsReads = inputs_reads(args).map_err(AgentPortFailure::plain)?;
        let Value::Object(mut answer) = reflection else {
            return Ok(reflection);
        };
        if let Some(orbital) = reads
            .state
            .as_deref()
            .filter(|o| self.get_orbital(o).is_none())
        {
            return Err(AgentPortFailure::plain(format!(
                "program/inputs: '{orbital}' is not a registered orbital"
            )));
        }
        if let Some(orbital) = &reads.state {
            let detail = self.get_orbital(orbital).ok_or_else(|| {
                AgentPortFailure::plain(format!(
                    "program/inputs: '{orbital}' is not a registered orbital"
                ))
            })?;
            let mut traits = Vec::new();
            for trait_name in &detail.traits {
                let instances: Vec<(Option<String>, String)> = self
                    .state_manager
                    .read()
                    .get_instances_for_trait(trait_name)
                    .into_iter()
                    .map(|s| (s.entity_id.clone(), s.current_state.clone()))
                    .collect();
                for (entity_id, state) in instances {
                    let snapshot = self.snapshot_trait(trait_name, entity_id.as_deref());
                    let mut row = ValueMap::new();
                    row.insert("trait".to_string(), Value::String(trait_name.clone()));
                    row.insert(
                        "entityId".to_string(),
                        Value::String(entity_id.unwrap_or_else(|| "singleton".to_string())),
                    );
                    row.insert("state".to_string(), Value::String(state));
                    row.insert("entity".to_string(), snapshot.entity);
                    traits.push(Value::Object(row));
                }
            }
            let mut state = ValueMap::new();
            state.insert("orbital".to_string(), Value::String(detail.name));
            state.insert("traits".to_string(), Value::Array(traits));
            answer.insert("state".to_string(), Value::Object(state));
        }
        if reads.stats {
            let stats = self.stats();
            let mut counters = ValueMap::new();
            for (name, count) in [
                ("registeredSchemas", stats.registered_schemas),
                ("registeredTraits", stats.registered_traits),
                ("activeInstances", stats.active_instances),
                ("cachedEntities", stats.cached_entities),
            ] {
                counters.insert(name.to_string(), Value::Number(count as f64));
            }
            answer.insert("stats".to_string(), Value::Object(counters));
        }
        if let Some(steps) = reads.trace {
            let mut ask = ValueMap::new();
            ask.insert("trace".to_string(), Value::Number(steps as f64));
            let trace = self
                .io
                .run("program/inputs", &[Value::Object(ask)])
                .map_err(|failure| AgentPortFailure {
                    error: failure.message,
                    errors: failure.errors,
                })?;
            answer.insert("trace".to_string(), trace);
        }
        Ok(Value::Object(answer))
    }

    fn run_read_named(
        &self,
        effect: &Effect,
        args: &[Value],
        ctx: &BindingContext,
    ) -> EffectResult {
        let entity = match read_named_target(args, &self.readable_entities()) {
            Ok(entity) => entity,
            Err(failure) => return failure_result(effect, failure),
        };
        let fetch = Effect::Fetch {
            entity_type: entity.clone(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            nearest: None,
            emit: None,
        };
        let fetched = self.effect_executor.execute(&fetch, ctx);
        match (&fetched.outcome, fetched.return_value) {
            (EffectOutcome::Executed, Some(Value::Array(rows))) => {
                EffectResult::executed_with_value(effect.clone(), read_named_result(&entity, rows))
            }
            (EffectOutcome::Executed, _) => EffectResult::failed(
                effect.clone(),
                &format!("persist/read-named: reading '{entity}' returned no rows"),
            ),
            (EffectOutcome::Failed, _) => EffectResult::failed(
                effect.clone(),
                &fetched
                    .error
                    .unwrap_or_else(|| format!("persist/read-named: reading '{entity}' failed")),
            ),
            (_, _) => EffectResult::failed(
                effect.clone(),
                &format!(
                    "persist/read-named: '{entity}' cannot be read here: no store is configured"
                ),
            ),
        }
    }

    fn run_dispatch(&self, effect: &Effect, args: &[Value]) -> EffectResult {
        let (caller_orbital, caller_trait) = self.caller_position();
        let plan = plan_dispatch(
            args,
            &self.external_inputs(),
            caller_orbital.as_deref(),
            &caller_trait,
        );
        let (input, payload) = match plan {
            DispatchPlan::Failed(failure) => return failure_result(effect, failure),
            DispatchPlan::Refused(answer) => {
                return EffectResult::executed_with_value(effect.clone(), answer)
            }
            DispatchPlan::Dispatch { input, payload } => (input, payload),
            DispatchPlan::Request { payload, options } => {
                let address = args.first().cloned().unwrap_or(Value::Null);
                return match self.io.run(
                    "program/dispatch",
                    &[address, payload, Value::Object(options)],
                ) {
                    Ok(answer) => EffectResult::executed_with_value(effect.clone(), answer),
                    Err(failure) => failure_result(
                        effect,
                        AgentPortFailure {
                            error: failure.message,
                            errors: failure.errors,
                        },
                    ),
                };
            }
            DispatchPlan::Inbound {
                name,
                body,
                headers,
            } => {
                let mut ask = ValueMap::new();
                ask.insert("inbound".to_string(), Value::String(name));
                ask.insert("body".to_string(), Value::String(body));
                ask.insert("headers".to_string(), Value::Object(headers));
                return match self.io.run("program/dispatch", &[Value::Object(ask)]) {
                    Ok(answer) => EffectResult::executed_with_value(effect.clone(), answer),
                    Err(failure) => failure_result(
                        effect,
                        AgentPortFailure {
                            error: failure.message,
                            errors: failure.errors,
                        },
                    ),
                };
            }
        };
        let address = format!("{}.{}.{}", input.orbital, input.trait_name, input.event);
        let ask = [Value::String(address), payload];
        match self.io.run("program/dispatch", &ask) {
            Ok(answer) => EffectResult::executed_with_value(effect.clone(), answer),
            Err(failure) => failure_result(
                effect,
                AgentPortFailure {
                    error: failure.message,
                    errors: failure.errors,
                },
            ),
        }
    }
}

/// An agent-port effect's declared completion: `emit.success` carries `{ result, text }` (`text` is `result` as
/// JSON text, the one writing both paths share), `emit.failure` `{ error, errors? }`.
pub(super) fn agent_port_completion_emit(
    emit: &FetchEmit,
    result: &EffectResult,
    key: Option<&str>,
) -> Option<EffectResult> {
    match &result.outcome {
        EffectOutcome::Executed => {
            let op = match &result.effect {
                Effect::Custom { name, .. } => name.as_str(),
                _ => "",
            };
            let value = result.return_value.clone().unwrap_or(Value::Null);
            Some(EffectResult::executed(Effect::Emit {
                event: emit.success.clone()?,
                payload: super::with_key(crate::agent_ports::completion_result(op, value), key),
            }))
        }
        EffectOutcome::Failed => super::substrate_completion_emit(emit, result, key),
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) => None,
    }
}
