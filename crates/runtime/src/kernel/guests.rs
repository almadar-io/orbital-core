//! Guest sessions (EXEC-CONC-7): `program/open` runs a program value as a guest kernel this kernel keeps, with its own
//! rows; `program/dispatch { guest }` and `program/inputs { guest }` reach it by its handle; `program/close` ends it.
//!
//! A guest is a server-role kernel over an in-memory entity source: its fetches and persists answer from its own rows.
//! Opened with `grants` (and any shared `arenas`), which the opener's kernel `spawn` grant and its own grants must cover,
//! its asks are answered by a host holding only those grants. The key decides the lane (EXEC-CONC-0a): an unkeyed dispatch
//! runs in the asking dispatch and answers in place (in the kernel for a guest without grants, through the opener's host
//! for one with them); a keyed dispatch runs on the guest's own lane, a host thread, and answers as a completion event.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::RuntimeKernel;
use crate::config::KernelConfig;
use crate::effects::response::EventResponse;
use crate::entity_source::InMemoryEntityCache;
use crate::process_result::ProcessEventResult;
use orbital_core_base::agent_ports::AgentPortFailure;
use orbital_core_base::os_grants::{covers, KernelKind, OsGrants};
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

/// One open guest: its kernel and rows, the grants a host answers its asks under (`None`: no host ports), and each shared
/// arena of the opener it was handed, as `(opener's arena id, the guest's handle)`.
#[derive(Clone)]
pub struct GuestSession {
    pub(crate) kernel: Arc<RuntimeKernel>,
    pub(crate) source: Arc<InMemoryEntityCache>,
    pub(crate) grants: Option<OsGrants>,
    arenas: Vec<(i64, Value)>,
}

fn failure(message: impl Into<String>) -> AgentPortFailure {
    AgentPortFailure {
        error: message.into(),
        errors: Vec::new(),
    }
}

/// A guest refusal carrying its error kind, as the os and guest asks report one.
fn guest_refusal(kind: orbital_core_base::os::OsErrorKind, message: impl Into<String>) -> AgentPortFailure {
    let refused = crate::agent_ports::guest_failure(kind, message);
    AgentPortFailure {
        error: refused.message,
        errors: refused.errors,
    }
}

/// Run `event` with `payload` to each of `targets` in the guest `session`, its asks answered by `answer`, and answer what
/// it moved, painted and emitted: the host side of a guest dispatch.
pub(crate) fn run_session(
    session: &GuestSession,
    targets: &[String],
    event: &str,
    payload: &Value,
    answer: &(dyn Fn(&crate::resumable::IoRequest) -> crate::resumable::IoAnswer + Sync),
) -> Result<Value, String> {
    let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
    for trait_name in targets {
        let progress = session
            .kernel
            .start_dispatch(trait_name, None, event, payload.clone(), None, None)
            .map_err(|e| format!("program/dispatch: {e}"))?;
        let ran = crate::resumable::drive_concurrent(&session.kernel, progress, answer)
            .map_err(|e| format!("program/dispatch: {e}"))?;
        outcomes.extend(ran);
    }
    let seed_trait = targets.first().cloned().unwrap_or_default();
    Ok(guest_answer(None, &outcomes, &seed_trait, event))
}

/// `program/open`'s `rows`, `[{ entity, rows }]`: each row checked against its entity's declaration (EXEC-EVAL-10) and
/// written into the guest's store before its page mounts. An undeclared entity or a misfit row refuses the open.
fn seed_guest_rows(guest: &RuntimeKernel, rows: &Value) -> Result<(), AgentPortFailure> {
    let Value::Array(sets) = rows else {
        return Err(failure("program/open: `rows` is a list of { entity, rows }"));
    };
    for set in sets.iter() {
        let entity = set
            .get("entity")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("program/open: each `rows` entry names its `entity`"))?;
        let Some(Value::Array(list)) = set.get("rows") else {
            return Err(failure(format!("program/open: the `rows` of {entity} are a list of rows")));
        };
        if !guest.registry.entity_definitions.read().contains_key(entity) {
            return Err(failure(format!("program/open: `rows` names {entity}, which the program does not declare")));
        }
        if let Some(misfit) = guest.rows_misfit(entity, list.iter()) {
            return Err(failure(format!("program/open: a seeded row does not fit: {misfit}")));
        }
        for row in list.iter() {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| failure(format!("program/open: a seeded {entity} row has no string id")))?;
            guest
                .entity_source
                .write(entity, id, row)
                .map_err(|e| failure(format!("program/open: seeding {entity}: {e}")))?;
        }
        guest.rows.note_write(entity);
    }
    Ok(())
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
    fn guest(&self, handle: &Value, op: &str) -> Result<GuestSession, AgentPortFailure> {
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

    /// The grants the calling orbital holds, after checking they let it give a guest its own lane or host ports.
    fn may_spawn_guest(&self, op: &str) -> Result<OsGrants, AgentPortFailure> {
        let (asker, _) = self.caller_position();
        let held = self.grants.os_grants.read().for_orbital(asker.as_deref()).clone();
        if !held.kernel_allows(KernelKind::Spawn, None) {
            return Err(guest_refusal(
                orbital_core_base::os::OsErrorKind::Denied,
                format!("{op}: no kernel guest grant covers it"),
            ));
        }
        Ok(held)
    }

    /// A copy of `value` for the guest `session`: data is rebuilt, an Arena handed over at `program/open` becomes the
    /// guest's handle for it, and any other sealed value (or a lambda) is refused.
    fn copy_into(&self, session: &GuestSession, value: &Value) -> Result<Value, AgentPortFailure> {
        match value {
            Value::Sealed(_) if value.is_sealed_as("Arena") => value
                .get("id")
                .and_then(Value::as_int)
                .and_then(|id| session.arenas.iter().find(|(held, _)| *held == id))
                .map(|(_, handle)| handle.clone())
                .ok_or_else(|| {
                    guest_refusal(
                        orbital_core_base::os::OsErrorKind::Invalid,
                        "program/dispatch: an Arena crosses to a guest only when program/open handed it over in `arenas`",
                    )
                }),
            Value::Sealed(_) | Value::Lambda(_) => Err(guest_refusal(
                orbital_core_base::os::OsErrorKind::Invalid,
                format!("program/dispatch: a {} cannot cross to a guest; only data and handed-over arenas can", value.type_name()),
            )),
            Value::Array(items) => items
                .iter()
                .map(|item| self.copy_into(session, item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::array),
            Value::Object(fields) => fields
                .iter()
                .map(|(key, item)| Ok((key.clone(), self.copy_into(session, item)?)))
                .collect::<Result<ValueMap, AgentPortFailure>>()
                .map(Value::object),
            other => Ok(other.clone()),
        }
    }

    /// `program/open program options`: register the program in a guest kernel, mount its first route page and answer
    /// `{ guest, states, rendered, emitted, outcomes }`.
    pub(super) fn open_guest(&self, args: &[Value]) -> Result<Value, AgentPortFailure> {
        let program = args
            .first()
            .ok_or_else(|| failure("program/open: name the program to open"))?;
        let options = match args.get(1) {
            Some(Value::Object(options)) => Some(options.clone()),
            _ => None,
        };
        let asked_grants = options.as_ref().and_then(|o| o.get("grants").cloned());
        let asked_arenas = options.as_ref().and_then(|o| o.get("arenas").cloned());
        let mut granted: Option<OsGrants> = None;
        if asked_grants.is_some() || asked_arenas.is_some() {
            let held = self.may_spawn_guest("program/open")?;
            let guest: OsGrants = serde_json::from_value(value_to_json(asked_grants.as_ref().unwrap_or(&Value::object(ValueMap::default()))))
                .map_err(|e| guest_refusal(orbital_core_base::os::OsErrorKind::Invalid, format!("program/open: the guest's grants are not os grants: {e}")))?;
            covers(&held, &guest).map_err(|missing| {
                guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    format!("program/open: the guest asks for grants the opening orbital does not hold: {}", missing.join(", ")),
                )
            })?;
            granted = Some(guest);
        }
        let mut handed = Vec::new();
        match &asked_arenas {
            None => {}
            Some(Value::Array(arenas)) => {
                for arena in arenas.iter() {
                    let id = arena.get("id").and_then(Value::as_int);
                    match (arena.is_sealed_as("Arena"), id, self.shared_arena_for(arena)) {
                        (true, Some(id), Some(shared)) => handed.push((id, shared)),
                        _ => {
                            return Err(guest_refusal(
                                orbital_core_base::os::OsErrorKind::Invalid,
                                "program/open: `arenas` holds only Arenas of shared arenas of the opener",
                            ))
                        }
                    }
                }
            }
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `arenas` is a list of shared Arenas",
                ))
            }
        }
        let mut schema: OrbitalSchema =
            serde_json::from_value(value_to_json(program)).map_err(|e| {
                failure(format!(
                    "program/open: the program is not a resolved program: {e}"
                ))
            })?;
        if schema.orbitals.is_empty() {
            return Err(failure("program/open: the program has no orbitals"));
        }
        if let Some(grants) = &granted {
            schema.os_grants = Some(grants.clone());
            schema.orbitals.iter_mut().for_each(|orbital| orbital.os_grants = None);
        }
        let source = Arc::new(InMemoryEntityCache::new());
        let guest = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            source.clone(),
            KernelConfig {
                runtime_rows_in_kernel: true,
                ..KernelConfig::default()
            },
        );
        guest
            .register_schema(schema)
            .map_err(|e| failure(format!("program/open: {e}")))?;
        if let Some(rows) = options.as_ref().and_then(|o| o.get("rows")) {
            seed_guest_rows(&guest, rows)?;
        }
        let mut arenas = Vec::with_capacity(handed.len());
        for (id, shared) in handed {
            let handle = guest
                .adopt_shared_arena(shared)
                .map_err(|e| guest_refusal(e.kind, e.message))?;
            arenas.push((id, handle));
        }
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
        self.guests.write().insert(handle.clone(), GuestSession { kernel: Arc::new(guest), source, grants: granted, arenas });
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

    /// `program/dispatch address payload { guest }`: dispatch the addressed event into the guest as its client would. The
    /// key decides the lane (EXEC-CONC-0a): unkeyed, it runs in the asking dispatch and answers what it moved, painted and
    /// emitted (in this kernel for a guest without grants, through the host for one with them); keyed, the host runs it
    /// on the guest's own lane and the answer arrives as the call site's completion event.
    pub(super) fn dispatch_guest(
        &self,
        args: &[Value],
        handle: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let session = self.guest(handle, "program/dispatch")?;
        if let Some(Value::Object(options)) = args.get(2) {
            if options.contains_key("grants") {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/dispatch: a guest's grants are given once, at program/open",
                ));
            }
        }
        let guest = &session.kernel;
        let Some(Value::String(address)) = args.first() else {
            return Err(failure(
                "program/dispatch: a guest address is `Orbital.Trait.EVENT` or `Trait.EVENT`",
            ));
        };
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
        let payload = self.copy_into(&session, &args.get(1).cloned().unwrap_or(Value::Null))?;
        let keyed = self.io.keyed_call();
        if keyed || session.grants.is_some() {
            if keyed {
                self.may_spawn_guest("program/dispatch")?;
            }
            let Value::String(name) = handle else {
                return Err(failure("program/dispatch: guest must be the handle program/open answered"));
            };
            let grants = session.grants.clone().unwrap_or_default();
            return self
                .io
                .guest(
                    name.to_string(),
                    crate::resumable::GuestKernel(Some(session.clone())),
                    targets,
                    event.to_string(),
                    payload,
                    grants,
                    args.to_vec(),
                )
                .map(|answer| answer.unwrap_or(Value::Null))
                .map_err(|refused| AgentPortFailure {
                    error: refused.message,
                    errors: refused.errors,
                });
        }
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
        let guest = self.guest(handle, "program/inputs")?.kernel;
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

#[cfg(test)]
impl GuestSession {
    /// A session over `kernel` and its rows `source`, answered under `grants`, with no arenas handed over.
    pub(crate) fn fresh(kernel: Arc<RuntimeKernel>, source: Arc<InMemoryEntityCache>, grants: Option<OsGrants>) -> Self {
        Self { kernel, source, grants, arenas: Vec::new() }
    }
}
