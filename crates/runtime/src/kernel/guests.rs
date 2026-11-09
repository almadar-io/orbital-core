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

use super::guest_hosting::GuestIntegrations;
use super::lazy::{LazyProgram, LazySource};
use super::RuntimeKernel;
use crate::config::KernelConfig;
use crate::effects::response::{EventResponse, MountSeed};
use crate::entity_source::InMemoryEntityCache;
use crate::process_result::ProcessEventResult;
use orbital_core_base::agent_ports::AgentPortFailure;
use orbital_core_base::client_effect::ClientEffect;
use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::json_value::value_to_json;
use orbital_core_base::os_grants::{covers, narrow_program_grants, KernelKind, OsGrants};
use orbital_core_base::protocols::ProtocolSide;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::sync::RwLock;
use orbital_core_base::value::{Value, ValueMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// The guest handle `op`'s options record names (`{ guest }`), if any: `program/dispatch address payload options` and
/// `program/inputs options` and `program/self options`. A payload is never read for it.
pub(super) fn guest_option<'a>(op: &str, args: &'a [Value]) -> Option<&'a Value> {
    let options = match op {
        "program/dispatch" => args.get(2),
        "program/inputs" | "program/self" => args.first(),
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
    /// The rows of a guest the opener's kernel runs itself; a hosted guest's rows are its host's.
    pub(crate) source: Option<Arc<InMemoryEntityCache>>,
    /// The host a hosted guest runs under (EXEC-CONC-7): its dispatches and mounts go through it.
    pub(crate) host: Option<Arc<dyn super::guest_hosting::GuestHost>>,
    pub(crate) grants: Option<OsGrants>,
    arenas: Vec<(i64, Value)>,
    route: Arc<RwLock<GuestRoute>>,
}

/// What a guest has mounted and which of those traits the routed page embeds, so a later route mounts only what is new.
#[derive(Default)]
struct GuestRoute {
    embedded: std::collections::HashSet<String>,
    mounted: std::collections::HashSet<String>,
}

/// The lazy programs an opened program value carries (`lazyPrograms`), by the file the kernel asks for.
struct CarriedLazy(std::collections::HashMap<String, OrbitalSchema>);

impl LazySource for CarriedLazy {
    fn read(&self, file: &str) -> Result<LazyProgram, String> {
        self.0
            .get(file)
            .map(|schema| LazyProgram {
                schema: schema.clone(),
            })
            .ok_or_else(|| {
                format!("the opened program carries no lazy program `{file}` in `lazyPrograms`")
            })
    }
}

fn failure(message: impl Into<String>) -> AgentPortFailure {
    AgentPortFailure {
        error: message.into(),
        errors: Vec::new(),
    }
}

/// A guest refusal carrying its error kind, as the os and guest asks report one.
fn guest_refusal(
    kind: orbital_core_base::os::OsErrorKind,
    message: impl Into<String>,
) -> AgentPortFailure {
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
        if let Some(host) = &session.host {
            outcomes.extend(host.dispatch(trait_name, event, payload.clone()).map_err(|e| format!("program/dispatch: {e}"))?);
            continue;
        }
        let progress = session
            .kernel
            .start_dispatch(trait_name, None, event, payload.clone(), None, None)
            .map_err(|e| format!("program/dispatch: {e}"))?;
        let ran = crate::resumable::drive_concurrent(&session.kernel, progress, answer)
            .map_err(|e| format!("program/dispatch: {e}"))?;
        outcomes.extend(ran);
    }
    let seed_trait = targets.first().cloned().unwrap_or_default();
    let embedded = session.route.read().embedded.clone();
    let orbitals = painter_orbitals(&session.kernel, &outcomes);
    Ok(guest_answer(&orbitals, None, None, &embedded, &outcomes, &seed_trait, event))
}

/// `program/open`'s and `program/navigate`'s `rows`, `[{ entity, rows }]`: each row checked against its entity's declaration (EXEC-EVAL-10) and
/// written into the guest's store before its page mounts. An undeclared entity or a misfit row refuses the open.
fn seed_guest_rows(guest: &RuntimeKernel, rows: &Value, op: &str) -> Result<(), AgentPortFailure> {
    let Value::Array(sets) = rows else {
        return Err(failure(format!(
            "{op}: `rows` is a list of {{ entity, rows }}"
        )));
    };
    for set in sets.iter() {
        let entity = set
            .get("entity")
            .and_then(Value::as_str)
            .ok_or_else(|| failure(format!("{op}: each `rows` entry names its `entity`")))?;
        let Some(Value::Array(list)) = set.get("rows") else {
            return Err(failure(format!(
                "{op}: the `rows` of {entity} are a list of rows"
            )));
        };
        if !guest
            .registry
            .entity_definitions
            .read()
            .contains_key(entity)
        {
            return Err(failure(format!(
                "{op}: `rows` names {entity}, which the program does not declare"
            )));
        }
        if let Some(misfit) = guest.rows_misfit(entity, list.iter()) {
            return Err(failure(format!(
                "{op}: a seeded row does not fit: {misfit}"
            )));
        }
        for row in list.iter() {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| failure(format!("{op}: a seeded {entity} row has no string id")))?;
            guest
                .entity_source
                .write(entity, id, row)
                .map_err(|e| failure(format!("{op}: seeding {entity}: {e}")))?;
        }
        guest.rows.note_write(entity);
    }
    Ok(())
}

fn json_value(json: serde_json::Value) -> Value {
    serde_json::from_value(json).unwrap_or(Value::Null)
}

/// The orbital declaring each trait that painted in `outcomes`: a paint names its orbital, so a host scopes the paint's
/// controls to the trait that drew them, as it scopes its own.
fn painter_orbitals(kernel: &RuntimeKernel, outcomes: &[(String, ProcessEventResult)]) -> std::collections::HashMap<String, String> {
    EventResponse::from_process_results(outcomes, "", "")
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|paint| kernel.orbital_of_trait(&paint.trait_name).map(|orbital| (paint.trait_name.clone(), orbital)))
        .collect()
}

/// What a guest's run answers: each trait's state, what it painted (with its trait's orbital), what it emitted and every
/// trait it moved.
fn guest_answer(
    orbitals: &std::collections::HashMap<String, String>,
    guest: Option<&str>,
    page: Option<(&[String], Option<&str>)>,
    embedded: &std::collections::HashSet<String>,
    outcomes: &[(String, ProcessEventResult)],
    seed_trait: &str,
    event: &str,
) -> Value {
    let response = EventResponse::from_process_results(outcomes, seed_trait, event);
    let record = |pairs: Vec<(&str, Value)>| {
        let mut fields = ValueMap::default();
        for (key, value) in pairs {
            fields.insert(key.into(), value);
        }
        Value::object(fields)
    };
    let rendered: Vec<Value> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|paint| match &paint.effect {
            ClientEffect::RenderUi { slot, config } => Some(record(vec![
                ("slot", Value::string(slot.to_string())),
                ("trait", Value::string(paint.trait_name.clone())),
                ("orbital", orbitals.get(&paint.trait_name).map_or(Value::Null, |o| Value::string(o.clone()))),
                ("embedded", Value::Bool(embedded.contains(&paint.trait_name))),
                ("content", config.clone().unwrap_or(Value::Null)),
            ])),
            _ => None,
        })
        .collect();
    let emitted: Vec<Value> = response
        .emitted_events
        .iter()
        .map(|e| record(vec![("event", Value::string(e.event.clone())), ("payload", e.payload.clone().unwrap_or(Value::Null))]))
        .collect();
    let moved: Vec<Value> = outcomes
        .iter()
        .map(|(trait_name, result)| {
            record(vec![
                ("trait", Value::string(trait_name.clone())),
                ("event", result.event.clone().map(Value::string).unwrap_or(Value::Null)),
                ("from", Value::string(result.previous_state.clone())),
                ("to", Value::string(result.new_state.clone())),
                ("fired", Value::Bool(result.executed)),
            ])
        })
        .collect();
    let mut states = ValueMap::default();
    for (name, state) in &response.states {
        states.insert(name.as_str().into(), Value::string(state.clone()));
    }
    let mut answer = vec![
        ("states", Value::object(states)),
        ("rendered", Value::array(rendered)),
        ("emitted", Value::array(emitted)),
        ("outcomes", Value::array(moved)),
    ];
    if let Some(guest) = guest {
        answer.push(("guest", Value::string(guest.to_string())));
    }
    if let Some((page, route)) = page {
        answer.push(("page", Value::array(page.iter().cloned().map(Value::string).collect::<Vec<Value>>())));
        if let Some(route) = route {
            answer.push(("route", Value::string(route.to_string())));
        }
    }
    if let Some(rejections) = response.rejections.as_ref().filter(|r| !r.is_empty()) {
        answer.push(("rejections", serde_json::to_value(rejections).map(json_value).unwrap_or(Value::Null)));
    }
    record(answer)
}

/// The client a guest's page is mounted under in the guest's own mount ledger (EXEC-TICK-6): the opener paints that page,
/// so its traits and the traits it embeds hold their ticks while it is the guest's page, and no others.
const GUEST_VIEWER: &str = "guest-viewer";

/// `page` (and every trait it embeds) is now the guest's viewed page: the ledger holds exactly those traits.
/// A guest opened with mocked integrations binds no store driver: every row it reads or writes is its own (EXEC-STORE-4).
fn unbind_stores(schema: &mut OrbitalSchema) {
    schema.store = None;
    for orbital in schema.orbitals.iter_mut() {
        for entity in std::iter::once(&mut orbital.entity).chain(orbital.auxiliary_entities.iter_mut().flatten()) {
            if let orbital_core_base::schema::types::EntityRef::Inline(def) = entity {
                def.store = None;
            }
        }
    }
}

fn view_page(guest: &RuntimeKernel, page: &[String], embedded: &std::collections::HashSet<String>) {
    guest.release_client(GUEST_VIEWER);
    let mut by_orbital: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for name in page.iter().chain(embedded.iter()) {
        if let Some(orbital) = guest.orbital_of_trait(name) {
            by_orbital.entry(orbital).or_default().push(name.clone());
        }
    }
    for (orbital, traits) in by_orbital {
        guest.mount_client(GUEST_VIEWER, &orbital, traits);
    }
}

/// What one tick pass of a guest found (`program/live { kind: "guest" }`).
pub enum GuestTicks {
    /// The pass painted something: the answer, shaped as a `program/dispatch` answer.
    Painted(Value),
    /// Nothing was due, or what ran painted nothing.
    Idle,
    /// The guest is closed.
    Closed,
}

/// The page a guest route resolves to: the path it resolved to (none when no page matched), its mounts and every trait
/// another trait on it embeds (`@trait.X`), listed on the page or not: those paint into the embedding trait, never a slot.
struct RoutedPage {
    route: Option<String>,
    mounts: Vec<MountSeed>,
    embedded: std::collections::HashSet<String>,
}

/// The page `route` resolves to in `guest` (`page_circuit`: the `[initial]` or first route page for none).
fn routed_mounts(
    guest: &RuntimeKernel,
    route: Option<&str>,
    op: &str,
) -> Result<RoutedPage, AgentPortFailure> {
    let circuit = guest
        .page_circuit(None, route)
        .map_err(|e| failure(format!("{op}: {e}")))?;
    Ok(RoutedPage {
        route: circuit.page.as_ref().and(circuit.route),
        mounts: circuit.mounts,
        embedded: circuit.embedded_by.into_iter().collect(),
    })
}

/// Mount `seeds` in `guest` as the route mount of `route` (EXEC-SEED-7: its `$MOUNT` listeners run first) and collect what moved.
fn mount_seeds(
    guest: &RuntimeKernel,
    host: Option<&dyn super::guest_hosting::GuestHost>,
    route: Option<&str>,
    seeds: &[MountSeed],
    op: &str,
) -> Result<Vec<(String, ProcessEventResult)>, AgentPortFailure> {
    let payload = route.map_or(Value::Null, |route| guest.route_mount_payload(route));
    // The guest declares what it mounts, as a browser kernel does, so a delivery to a page trait waits for its lifecycle
    // event (EXEC-SEED-2).
    guest.mount_traits(seeds.iter().map(|seed| seed.r#trait.clone()));
    let mounted = match host {
        Some(host) => host.mount(route, seeds, payload),
        None => guest.dispatch_route_mount(route, seeds, None, payload).map_err(|e| e.to_string()),
    };
    mounted.map_err(|e| failure(format!("{op}: mounting {}: {e}", route.unwrap_or_default())))
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
        let held = self
            .grants
            .os_grants
            .read()
            .for_orbital(asker.as_deref())
            .clone();
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

    /// `program/open program options`: register the program in a guest kernel, mount its boot traits and its routed page and answer
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
        let keep_declared = match options.as_ref().and_then(|o| o.get("declared").cloned()) {
            None => false,
            Some(Value::Bool(keep)) => keep,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `declared` is true or false",
                ))
            }
        };
        let integrations = match options.as_ref().and_then(|o| o.get("integrations")) {
            None | Some(Value::Null) => GuestIntegrations::Live,
            Some(Value::String(word)) if word.as_str() == "live" => GuestIntegrations::Live,
            Some(Value::String(word)) if word.as_str() == "mock" => GuestIntegrations::Mock,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `integrations` is \"live\" or \"mock\"",
                ))
            }
        };
        let mut granted: Option<OsGrants> = None;
        let mut opener_held: Option<OsGrants> = None;
        if asked_grants.is_some() || asked_arenas.is_some() || keep_declared {
            let held = self.may_spawn_guest("program/open")?;
            let guest: OsGrants = serde_json::from_value(value_to_json(
                asked_grants
                    .as_ref()
                    .unwrap_or(&Value::object(ValueMap::default())),
            ))
            .map_err(|e| {
                guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    format!("program/open: the guest's grants are not os grants: {e}"),
                )
            })?;
            covers(&held, &guest).map_err(|missing| {
                guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    format!("program/open: the guest asks for grants the opening orbital does not hold: {}", missing.join(", ")),
                )
            })?;
            granted = Some(guest);
            opener_held = Some(held);
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
        let mut program_json = value_to_json(program);
        let carried = match program_json.as_object_mut().and_then(|object| object.remove("lazyPrograms")) {
            None | Some(serde_json::Value::Null) => None,
            Some(carried) => Some(
                serde_json::from_value::<std::collections::HashMap<String, OrbitalSchema>>(carried).map_err(|e| {
                    failure(format!("program/open: `lazyPrograms` is a map of file to resolved program: {e}"))
                })?,
            ),
        };
        let mut schema: OrbitalSchema = serde_json::from_value(program_json).map_err(|e| {
            failure(format!(
                "program/open: the program is not a resolved program: {e}"
            ))
        })?;
        if schema.orbitals.is_empty() {
            return Err(failure("program/open: the program has no orbitals"));
        }
        let carries_lazy = carried.is_some();
        let mut lazy_programs = carried.unwrap_or_default();
        if integrations == GuestIntegrations::Mock {
            std::iter::once(&mut schema).chain(lazy_programs.values_mut()).for_each(unbind_stores);
        }
        let host = self.guest_hosting().map(|hosting| hosting.open(integrations));
        // A guest holds exactly the grants it was passed, plus with `declared` its own declared grants narrowed to the
        // opener's: none when neither was asked, even under a host.
        if granted.is_some() || host.is_some() {
            for carried_schema in std::iter::once(&mut schema).chain(lazy_programs.values_mut()) {
                match (keep_declared, &opener_held) {
                    (true, Some(held)) => narrow_program_grants(carried_schema, held, granted.as_ref().unwrap_or(&OsGrants::default())),
                    _ => {
                        carried_schema.os_grants = granted.clone();
                        carried_schema
                            .orbitals
                            .iter_mut()
                            .for_each(|orbital| orbital.os_grants = None);
                    }
                }
            }
        }
        let (guest, source) = match &host {
            Some(host) => (host.kernel(), None),
            None => {
                let source = Arc::new(InMemoryEntityCache::new());
                let guest = RuntimeKernel::new(
                    RuntimeEnvironment::Server,
                    source.clone(),
                    KernelConfig {
                        runtime_rows_in_kernel: true,
                        ..KernelConfig::default()
                    },
                );
                (Arc::new(guest), Some(source))
            }
        };
        guest.set_runs_client_traits(true);
        if carries_lazy {
            guest.set_lazy_source(Arc::new(CarriedLazy(lazy_programs)));
        }
        match &host {
            Some(host) => host.register(schema).map_err(|e| failure(format!("program/open: {e}")))?,
            None => guest
                .register_schema(schema)
                .map_err(|e| failure(format!("program/open: {e}")))?,
        }
        if let Some(rows) = options.as_ref().and_then(|o| o.get("rows")) {
            seed_guest_rows(&guest, rows, "program/open")?;
        }
        let mut arenas = Vec::with_capacity(handed.len());
        for (id, shared) in handed {
            let handle = guest
                .adopt_shared_arena(shared)
                .map_err(|e| guest_refusal(e.kind, e.message))?;
            arenas.push((id, handle));
        }
        let route = match options.as_ref().and_then(|o| o.get("route")) {
            None | Some(Value::Null) => None,
            Some(Value::String(route)) => Some(route.to_string()),
            Some(_) => return Err(failure("program/open: `route` is a path string")),
        };
        let mut seeds = guest.boot_mounts(ProtocolSide::Server);
        let RoutedPage {
            route: resolved,
            mounts: routed,
            embedded,
        } = routed_mounts(&guest, route.as_deref(), "program/open")?;
        let page: Vec<String> = routed.iter().map(|seed| seed.r#trait.clone()).collect();
        for seed in routed {
            if !seeds.iter().any(|held| held.r#trait == seed.r#trait) {
                seeds.push(seed);
            }
        }
        let outcomes = mount_seeds(&guest, host.as_deref(), resolved.as_deref(), &seeds, "program/open")?;
        view_page(&guest, &page, &embedded);
        let orbitals = painter_orbitals(&guest, &outcomes);
        let handle = format!(
            "guest-{}",
            self.guest_seq.fetch_add(1, Ordering::SeqCst) + 1
        );
        let mounted = seeds.iter().map(|s| s.r#trait.clone()).collect();
        let session = GuestSession {
            kernel: guest,
            source,
            host,
            grants: granted,
            arenas,
            route: Arc::new(RwLock::new(GuestRoute {
                embedded: embedded.clone(),
                mounted,
            })),
        };
        self.guests.write().insert(handle.clone(), session);
        let seed_trait = seeds.first().map(|s| s.r#trait.clone()).unwrap_or_default();
        Ok(guest_answer(
            &orbitals,
            Some(&handle),
            Some((&page, resolved.as_deref())),
            &embedded,
            &outcomes,
            &seed_trait,
            "INIT",
        ))
    }

    /// `program/navigate { guest, route, rows?, remount? }`: load the lazy page `route` names, seed its `rows`, mount the
    /// traits of the route the guest has not mounted yet (every trait of it with `remount: true`) and answer as
    /// `program/open` does.
    pub(super) fn navigate_guest(&self, args: &[Value]) -> Result<Value, AgentPortFailure> {
        let Some(Value::Object(options)) = args.first() else {
            return Err(failure("program/navigate: name `{ guest, route }`"));
        };
        let handle = options
            .get("guest")
            .ok_or_else(|| failure("program/navigate: name the `guest` program/open answered"))?;
        let Some(Value::String(route)) = options.get("route") else {
            return Err(failure("program/navigate: `route` is a path string"));
        };
        let session = self.guest(handle, "program/navigate")?;
        let guest = &session.kernel;
        guest
            .ensure_route_loaded(route.as_str())
            .map_err(|e| failure(format!("program/navigate: {e}")))?;
        if let Some(rows) = options
            .get("rows")
            .filter(|rows| !matches!(rows, Value::Null))
        {
            seed_guest_rows(guest, rows, "program/navigate")?;
        }
        let remount = matches!(options.get("remount"), Some(Value::Bool(true)));
        if remount {
            // A remount enters the page afresh: each of its mounted traits starts again from its initial state with no
            // frame, so its lifecycle event is the one it handles there.
            let traits = guest.page_circuit(None, Some(route.as_str())).map_err(|e| failure(format!("program/navigate: {e}")))?.traits;
            let held = session.route.read();
            for name in traits.iter().filter(|name| held.mounted.contains(*name)) {
                let initial = guest.registry.trait_definitions.read().get(name).and_then(|def| {
                    let states = &def.state_machine.states;
                    states.iter().find(|s| s.is_initial == Some(true)).or_else(|| states.first()).map(|s| s.name.clone())
                });
                if let Some(state) = initial {
                    guest.restore_trait(name, None, super::TraitSnapshot { state, entity: Value::Null });
                }
            }
        }
        let RoutedPage {
            route: resolved,
            mounts: routed,
            embedded,
        } = routed_mounts(guest, Some(route.as_str()), "program/navigate")?;
        let page: Vec<String> = routed.iter().map(|seed| seed.r#trait.clone()).collect();
        let fresh: Vec<MountSeed> = {
            let held = session.route.read();
            routed
                .into_iter()
                .filter(|seed| remount || !held.mounted.contains(&seed.r#trait))
                .collect()
        };
        let outcomes = mount_seeds(guest, session.host.as_deref(), resolved.as_deref(), &fresh, "program/navigate")?;
        view_page(guest, &page, &embedded);
        let orbitals = painter_orbitals(guest, &outcomes);
        {
            let mut held = session.route.write();
            held.mounted.extend(fresh.iter().map(|s| s.r#trait.clone()));
            held.embedded = embedded.clone();
        }
        let seed_trait = fresh.first().map(|s| s.r#trait.clone()).unwrap_or_default();
        let name = match handle {
            Value::String(name) => Some(name.as_str()),
            _ => None,
        };
        Ok(guest_answer(
            &orbitals,
            name,
            Some((&page, resolved.as_deref())),
            &embedded,
            &outcomes,
            &seed_trait,
            "INIT",
        ))
    }

    /// `program/live { kind: "guest", guest }`, polled by the host's stream: one tick pass of the hosted guest `handle`
    /// (EXEC-DIST-14). A guest without a host of its own has no clock to tick on and is refused.
    pub fn guest_tick_pass(&self, handle: &str) -> Result<GuestTicks, String> {
        let Some(session) = self.guests.read().get(handle).cloned() else {
            return Ok(GuestTicks::Closed);
        };
        let Some(host) = &session.host else {
            return Err(format!(
                "program/live: guest {handle} runs in this kernel with no host of its own, so nothing ticks it"
            ));
        };
        let outcomes = host.tick_pass()?;
        if outcomes.is_empty() {
            return Ok(GuestTicks::Idle);
        }
        let embedded = session.route.read().embedded.clone();
        let orbitals = painter_orbitals(&session.kernel, &outcomes);
        let seed_trait = outcomes.first().map(|(t, _)| t.clone()).unwrap_or_default();
        let answer = guest_answer(&orbitals, Some(handle), None, &embedded, &outcomes, &seed_trait, "");
        // The opener repaints from an item; a pass that painted nothing (a paused clock accumulating) is no item.
        if answer.get("rendered").and_then(Value::as_array).is_none_or(|rendered| rendered.is_empty()) {
            return Ok(GuestTicks::Idle);
        }
        Ok(GuestTicks::Painted(answer))
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
                return Err(failure(
                    "program/dispatch: guest must be the handle program/open answered",
                ));
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
            let ran = match &session.host {
                Some(host) => host.dispatch(trait_name, event, payload.clone()),
                None => guest.dispatch(trait_name, None, event, payload.clone()).map_err(|e| e.to_string()),
            }
            .map_err(|e| failure(format!("program/dispatch: {e}")))?;
            outcomes.extend(ran);
        }
        let seed_trait = targets.first().cloned().unwrap_or_default();
        let embedded = session.route.read().embedded.clone();
        let orbitals = painter_orbitals(guest, &outcomes);
        Ok(guest_answer(&orbitals, None, None, &embedded, &outcomes, &seed_trait, event))
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

    /// `program/self { mounted: true, guest }`: the guest's mounted orbital, answered by the guest's own kernel from the
    /// page its route mount opened (EXEC-DIST-15).
    pub(super) fn mounted_of_guest(&self, handle: &Value) -> Result<Value, AgentPortFailure> {
        let guest = self.guest(handle, "program/self")?.kernel;
        guest.mounted_tree().map_err(AgentPortFailure::plain)
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
    pub(crate) fn fresh(
        kernel: Arc<RuntimeKernel>,
        source: Arc<InMemoryEntityCache>,
        grants: Option<OsGrants>,
    ) -> Self {
        Self {
            kernel,
            source: Some(source),
            host: None,
            grants,
            arenas: Vec::new(),
            route: Arc::default(),
        }
    }
}
