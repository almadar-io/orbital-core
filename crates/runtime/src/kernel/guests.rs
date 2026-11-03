//! Guest sessions (EXEC-CONC-7): `program/open` runs a program value as a guest kernel this kernel keeps, with its own
//! rows; `program/dispatch { guest }` and `program/inputs { guest }` reach it by its handle; `program/close` ends it.
//!
//! A guest is a server-role kernel over an in-memory entity source: its fetches and persists answer from its own rows.
//! It has no host ports yet, so its `os/*`, `http` and service asks fail, and a `grants` option is refused with that
//! reason until guest grants are passed through the opener's port (G-CROSS-400).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::RuntimeKernel;
use crate::config::KernelConfig;
use crate::effects::response::EventResponse;
use crate::entity_source::InMemoryEntityCache;
use crate::process_result::ProcessEventResult;
use orbital_core_base::agent_ports::AgentPortFailure;
use orbital_core_base::client_effect::ClientEffect;
use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::json_value::value_to_json;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::{Value, ValueMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// The guest handle `op`'s options record names (`{ guest }`), if any: `program/dispatch address payload options` and
/// `program/inputs options`. A payload is never read for it.
pub(super) fn guest_option<'a>(op: &str, args: &'a [Value]) -> Option<&'a Value> {
    let options = match op {
        "program/dispatch" => args.get(2),
        "program/inputs" => args.first(),
        _ => None,
    };
    match options {
        Some(Value::Object(map)) => map.get("guest"),
        _ => None,
    }
}

fn failure(message: impl Into<String>) -> AgentPortFailure {
    AgentPortFailure {
        error: message.into(),
        errors: Vec::new(),
    }
}

fn json_value(json: serde_json::Value) -> Value {
    serde_json::from_value(json).unwrap_or(Value::Null)
}

/// What a guest's run answers: each trait's state, what it painted, what it emitted and every trait it moved.
fn guest_answer(
    guest: Option<&str>,
    outcomes: &[(String, ProcessEventResult)],
    seed_trait: &str,
    event: &str,
) -> Value {
    let response = EventResponse::from_process_results(outcomes, seed_trait, event);
    let rendered: Vec<serde_json::Value> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|paint| match &paint.effect {
            ClientEffect::RenderUi { slot, config } => Some(serde_json::json!({
                "slot": slot,
                "trait": paint.trait_name,
                "content": config.as_ref().map(value_to_json).unwrap_or(serde_json::Value::Null),
            })),
            _ => None,
        })
        .collect();
    let emitted: Vec<serde_json::Value> = response
        .emitted_events
        .iter()
        .map(|e| serde_json::json!({ "event": e.event, "payload": e.payload.as_ref().map(value_to_json).unwrap_or(serde_json::Value::Null) }))
        .collect();
    let moved: Vec<serde_json::Value> = outcomes
        .iter()
        .map(|(trait_name, result)| {
            serde_json::json!({
                "trait": trait_name,
                "event": result.event,
                "from": result.previous_state,
                "to": result.new_state,
                "fired": result.executed,
            })
        })
        .collect();
    let mut answer = serde_json::json!({
        "states": response.states,
        "rendered": rendered,
        "emitted": emitted,
        "outcomes": moved,
    });
    if let Some(guest) = guest {
        answer["guest"] = serde_json::Value::String(guest.to_string());
    }
    if let Some(rejections) = response.rejections.as_ref().filter(|r| !r.is_empty()) {
        answer["rejections"] = serde_json::to_value(rejections).unwrap_or(serde_json::Value::Null);
    }
    json_value(answer)
}

impl RuntimeKernel {
    /// The guest `handle` names, or the failure naming it.
    fn guest(&self, handle: &Value, op: &str) -> Result<Arc<RuntimeKernel>, AgentPortFailure> {
        let Value::String(name) = handle else {
            return Err(failure(format!(
                "{op}: guest must be the handle program/open answered"
            )));
        };
        self.guests
            .read()
            .get(name.as_str())
            .cloned()
            .ok_or_else(|| {
                failure(format!(
                    "{op}: no open guest '{}' (it was never opened or is closed)",
                    name.as_str()
                ))
            })
    }

    /// `program/open program options`: register the program in a guest kernel, mount its first route page and answer
    /// `{ guest, states, rendered, emitted, outcomes }`.
    pub(super) fn open_guest(&self, args: &[Value]) -> Result<Value, AgentPortFailure> {
        let program = args
            .first()
            .ok_or_else(|| failure("program/open: name the program to open"))?;
        if let Some(Value::Object(options)) = args.get(1) {
            if options.contains_key("grants") {
                return Err(failure(
                    "program/open: a guest has no host ports yet, so it takes no grants (G-CROSS-400); open it without `grants`",
                ));
            }
        }
        let schema: OrbitalSchema =
            serde_json::from_value(value_to_json(program)).map_err(|e| {
                failure(format!(
                    "program/open: the program is not a resolved program: {e}"
                ))
            })?;
        if schema.orbitals.is_empty() {
            return Err(failure("program/open: the program has no orbitals"));
        }
        let guest = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()),
            KernelConfig {
                runtime_rows_in_kernel: true,
                ..KernelConfig::default()
            },
        );
        guest
            .register_schema(schema)
            .map_err(|e| failure(format!("program/open: {e}")))?;
        let first_page: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
        let seeds = guest.mounts_of(|page| {
            if !page.is_route() {
                return false;
            }
            let mut first = first_page.borrow_mut();
            first.get_or_insert_with(|| page.path.clone()) == &page.path
        });
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        for seed in &seeds {
            let ran = guest
                .dispatch(&seed.r#trait, None, seed.event.as_str(), Value::Null)
                .map_err(|e| failure(format!("program/open: mounting {}: {e}", seed.r#trait)))?;
            outcomes.extend(ran);
        }
        let handle = format!(
            "guest-{}",
            self.guest_seq.fetch_add(1, Ordering::SeqCst) + 1
        );
        self.guests.write().insert(handle.clone(), Arc::new(guest));
        let seed_trait = seeds.first().map(|s| s.r#trait.clone()).unwrap_or_default();
        Ok(guest_answer(Some(&handle), &outcomes, &seed_trait, "INIT"))
    }

    /// `program/close guest`: end the guest; `{ closed: true }`.
    pub(super) fn close_guest(&self, args: &[Value]) -> Result<Value, AgentPortFailure> {
        let handle = args
            .first()
            .ok_or_else(|| failure("program/close: name the guest to close"))?;
        self.guest(handle, "program/close")?;
        if let Value::String(name) = handle {
            self.guests.write().remove(name.as_str());
        }
        let mut answer = ValueMap::default();
        answer.insert("closed".into(), Value::Bool(true));
        Ok(Value::object(answer))
    }

    /// `program/dispatch address payload { guest }`: dispatch the addressed event into the guest as its client would,
    /// and answer what it moved, painted and emitted.
    pub(super) fn dispatch_guest(
        &self,
        args: &[Value],
        handle: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let guest = self.guest(handle, "program/dispatch")?;
        let Some(Value::String(address)) = args.first() else {
            return Err(failure(
                "program/dispatch: a guest address is `Orbital.Trait.EVENT` or `Trait.EVENT`",
            ));
        };
        let payload = args.get(1).cloned().unwrap_or(Value::Null);
        let parts: Vec<&str> = address.as_str().split('.').collect();
        let (targets, event): (Vec<String>, &str) = match parts.as_slice() {
            [_, trait_name, event] | [trait_name, event] => (vec![trait_name.to_string()], *event),
            // A bare event goes to every guest trait whose current state handles it, as a bridged event did.
            [event] => (guest.traits_handling(event), *event),
            _ => {
                return Err(failure(format!(
                    "program/dispatch: '{}' is not `Orbital.Trait.EVENT`, `Trait.EVENT` or `EVENT`",
                    address.as_str()
                )))
            }
        };
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        for trait_name in &targets {
            let ran = guest
                .dispatch(trait_name, None, event, payload.clone())
                .map_err(|e| failure(format!("program/dispatch: {e}")))?;
            outcomes.extend(ran);
        }
        let seed_trait = targets.first().cloned().unwrap_or_default();
        Ok(guest_answer(None, &outcomes, &seed_trait, event))
    }

    /// The registered traits whose current state handles `event`, in name order.
    fn traits_handling(&self, event: &str) -> Vec<String> {
        let definitions: Vec<(
            String,
            Arc<orbital_core_base::schema::types::TraitDefinition>,
        )> = self
            .registry
            .trait_definitions
            .read()
            .iter()
            .map(|(name, def)| (name.clone(), Arc::clone(def)))
            .collect();
        let mut out: Vec<String> = definitions
            .into_iter()
            .filter(|(name, def)| {
                let state = self.current_state_name(name, Some(super::RUNTIME_SINGLETON));
                def.state_machine
                    .transitions
                    .iter()
                    .any(|t| t.event == event && t.from.matches(&state))
            })
            .map(|(name, _)| name)
            .collect();
        out.sort();
        out
    }

    /// `program/inputs { guest, … }`: the guest's own inputs and runtime reads.
    pub(super) fn inputs_of_guest(
        &self,
        args: &[Value],
        handle: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let guest = self.guest(handle, "program/inputs")?;
        let own: Vec<Value> = args
            .iter()
            .enumerate()
            .map(|(at, arg)| match arg {
                Value::Object(map) if at == 0 => {
                    let mut rest = map.clone();
                    rest.shift_remove("guest");
                    Value::Object(rest)
                }
                other => other.clone(),
            })
            .collect();
        let reflection = orbital_core_base::agent_ports::plan_inputs(
            &own,
            &guest.external_inputs(),
            &guest.readable_entities(),
            None,
            "",
        )?;
        guest.with_runtime_reads(reflection, &own)
    }
}
