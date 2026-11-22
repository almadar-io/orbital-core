//! Guest sessions (EXEC-CONC-7): `program/open` runs a program value as a guest kernel this kernel keeps, with its own
//! rows; `program/dispatch { guest }` and `program/inputs { guest }` reach it by its handle; `program/close` ends it.
//!
//! A guest is a server-role kernel over an in-memory entity source: its fetches and persists answer from its own rows.
//! Opened with `grants` (and any shared `arenas`), which the opener's kernel `spawn` grant and its own grants must cover,
//! its asks are answered by a host holding only those grants. The key decides the lane (EXEC-CONC-0a): an unkeyed dispatch
//! runs in the asking dispatch and answers in place (in the kernel for a guest without grants, through the opener's host
//! for one with them); a keyed dispatch runs on the guest's own lane, a host thread, and answers as a completion event.
use orbital_core_base::json_value::{from_json_str, from_json_value};
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
use orbital_core_base::os_grants::{covers, narrow_program_grants, union, KernelKind, OsGrants};
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
        "program/inputs" | "program/self" | "program/tick" => args.first(),
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
    /// The session that opened the guest; the guest ends with it (EXEC-CONC-7).
    pub(crate) opener_session: Option<String>,
    /// The opener's viewer runs the guest's `[client]` traits and mounts its pages; the guest answers its server legs.
    pub(crate) split: bool,
    /// The guest's lazy programs: a split guest's viewer asks for one with `program/lazy`.
    lazy: Arc<GuestLazy>,
    arenas: Vec<(i64, Value)>,
    route: Arc<RwLock<GuestRoute>>,
    /// When a viewer's stream last ticked the guest ([`guest_ticks_now_ms`]); its host ticks it while none has lately.
    viewer_ticked_ms: Arc<std::sync::atomic::AtomicU64>,
    /// The tick clock of a guest with no host of its own: `program/tick { guest }` passes it on the asking step's clock.
    ticks: Arc<orbital_core_base::sync::Mutex<crate::tick_clock::TickClock>>,
}

/// How often a hosted split guest's host ticks it while no viewer streams it (G-CROSS-708, EXEC-CONC-7).
const UNWATCHED_TICK_EVERY: std::time::Duration = std::time::Duration::from_millis(50);
/// How long after a viewer's last tick the host still leaves the ticking to the viewer.
const VIEWER_TICKS_FOR_MS: u64 = 250;

/// Milliseconds on one monotonic clock, for comparing a viewer's last tick with now.
fn guest_ticks_now_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

/// A served program's server ticks with no viewer, so a hosted split guest does too: its host ticks it every
/// [`UNWATCHED_TICK_EVERY`] while no viewer's stream has ticked it for [`VIEWER_TICKS_FOR_MS`], dropping the paints and pushes
/// no viewer is there to take; the thread ends once the guest is closed.
fn tick_while_unwatched(
    guests: std::sync::Weak<RwLock<std::collections::HashMap<String, GuestSession>>>,
    handle: String,
    host: Arc<dyn super::guest_hosting::GuestHost>,
    viewer_ticked_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    let spawned = std::thread::Builder::new().name(format!("guest-ticks-{handle}")).spawn(move || loop {
        std::thread::sleep(UNWATCHED_TICK_EVERY);
        let Some(open) = guests.upgrade() else { return };
        if !open.read().contains_key(&handle) {
            return;
        }
        drop(open);
        let last = viewer_ticked_ms.load(std::sync::atomic::Ordering::Relaxed);
        if last != 0 && guest_ticks_now_ms().saturating_sub(last) < VIEWER_TICKS_FOR_MS {
            continue;
        }
        if let Err(error) = host.tick_pass() {
            tracing::error!(guest = %handle, %error, "an unwatched guest's tick pass failed");
        }
        drop(host.take_host_dispatches());
    });
    if let Err(error) = spawned {
        tracing::error!(%error, "the unwatched-guest ticker could not start");
    }
}

type OpenedProgram = (
    OrbitalSchema,
    Option<std::collections::HashMap<String, OrbitalSchema>>,
    Option<String>,
);

/// A `program/open` program value: the resolved program, the lazy programs it carries (`lazyPrograms`) or the installed
/// behavior it names for them (`lazySource`).
fn parse_open_program(program: &Value) -> Result<OpenedProgram, AgentPortFailure> {
    use orbital_core_base::value_deserialize::from_value;
    let field = |name: &str| match program {
        Value::Object(fields) => fields.get(name).filter(|v| !matches!(v, Value::Null)),
        _ => None,
    };
    let carried = match field("lazyPrograms") {
        None => None,
        Some(carried) => Some(from_value::<std::collections::HashMap<String, OrbitalSchema>>(carried).map_err(|e| {
            failure(format!("program/open: `lazyPrograms` is a map of file to resolved program: {e}"))
        })?),
    };
    let lazy_behavior = match field("lazySource") {
        None => None,
        Some(source) => match source {
            Value::Object(source) => match source.get("behavior") {
                Some(Value::String(behavior)) => Some(behavior.to_string()),
                _ => return Err(failure("program/open: `lazySource` names the installed behavior its lazy programs are read from: `{ behavior }`")),
            },
            _ => return Err(failure("program/open: `lazySource` names the installed behavior its lazy programs are read from: `{ behavior }`")),
        },
    };
    if carried.is_some() && lazy_behavior.is_some() {
        return Err(guest_refusal(
            orbital_core_base::os::OsErrorKind::Invalid,
            "program/open: a program carries its lazy programs (`lazyPrograms`) or names where they are read (`lazySource`), not both",
        ));
    }
    // Read straight from the value: a `serde_json` copy of a resolved program is ~10x the value (P-049).
    let schema: OrbitalSchema = from_value(program).map_err(|e| failure(format!("program/open: the program is not a resolved program: {e}")))?;
    if schema.orbitals.is_empty() {
        return Err(failure("program/open: the program has no orbitals"));
    }
    Ok((schema, carried, lazy_behavior))
}

/// What a guest has mounted and which of those traits the routed page embeds, so a later route mounts only what is new.
#[derive(Default)]
struct GuestRoute {
    embedded: std::collections::HashSet<String>,
    mounted: std::collections::HashSet<String>,
}

/// How a guest's programs are made ready before it runs them: a mocked guest's stores unbound, and the grants it holds.
#[derive(Clone)]
struct GuestPrepare {
    mock: bool,
    apply_grants: bool,
    keep_declared: bool,
    live: bool,
    opener_held: Option<OsGrants>,
    granted: Option<OsGrants>,
    withhold: Vec<GuestWithhold>,
}

/// A grant kind an opener withholds from its guest whatever the guest declares (`program/open { withhold }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuestWithhold {
    /// `net … [listen]`: a hosted guest is served through its opener's endpoint and binds no port of its own.
    Listen,
    /// `sys: [self-signal]`: a guest shares its host's process and may not end it.
    SelfSignal,
}

fn withhold_grants(grants: &mut OsGrants, withhold: &[GuestWithhold]) {
    if withhold.contains(&GuestWithhold::Listen) {
        for net in grants.net.iter_mut() {
            net.kinds
                .retain(|kind| *kind != orbital_core_base::os_grants::NetKind::Listen);
        }
        grants.net.retain(|net| !net.kinds.is_empty());
    }
    if withhold.contains(&GuestWithhold::SelfSignal) {
        grants
            .sys
            .retain(|kind| *kind != orbital_core_base::os_grants::SysKind::SelfSignal);
    }
}

impl GuestPrepare {
    fn apply(&self, schema: &mut OrbitalSchema) {
        if self.mock {
            unbind_stores(schema);
        }
        if self.apply_grants {
            self.apply_grants(schema);
        }
        if !self.withhold.is_empty() {
            if let Some(grants) = schema.os_grants.as_mut() {
                withhold_grants(grants, &self.withhold);
            }
            for orbital in schema.orbitals.iter_mut() {
                if let Some(grants) = orbital.os_grants.as_mut() {
                    withhold_grants(grants, &self.withhold);
                }
            }
        }
    }

    fn apply_grants(&self, schema: &mut OrbitalSchema) {
        // A guest holds exactly the grants it was passed, plus with `declared` its own declared grants narrowed to the
        // opener's: none when neither was asked, even under a host.
        match (self.keep_declared, &self.opener_held) {
            (true, Some(held)) if self.live && held.kernel_allows(KernelKind::Declared, None) => {
                let passed = self.granted.clone().unwrap_or_default();
                schema.os_grants = Some(union(
                    &passed,
                    &schema.os_grants.clone().unwrap_or_default(),
                ));
            }
            (true, Some(held)) => narrow_program_grants(
                schema,
                held,
                self.granted.as_ref().unwrap_or(&OsGrants::default()),
            ),
            _ => {
                schema.os_grants = self.granted.clone();
                schema
                    .orbitals
                    .iter_mut()
                    .for_each(|orbital| orbital.os_grants = None);
            }
        }
    }
}

/// Where a guest reads its lazy programs: none, the programs its opened value carried (`lazyPrograms`), or the installed
/// behavior its value names (`lazySource`), which the host resolves one file at a time.
enum LazyFrom {
    None,
    Carried(std::collections::HashMap<String, OrbitalSchema>),
    Behavior(Arc<dyn super::guest_hosting::GuestHosting>, String),
}

/// A guest's lazy programs: each read once, when the guest or its viewer first needs it, and prepared as its own program is.
struct GuestLazy {
    from: LazyFrom,
    prepare: GuestPrepare,
    read: orbital_core_base::sync::Mutex<std::collections::HashMap<String, OrbitalSchema>>,
}

impl GuestLazy {
    fn program(
        &self,
        file: &str,
    ) -> Result<OrbitalSchema, crate::effects::executor::SubstrateFailure> {
        if let Some(program) = self.read.lock().get(file) {
            return Ok(program.clone());
        }
        let carried_none = || {
            crate::agent_ports::guest_failure(
                orbital_core_base::os::OsErrorKind::NotFound,
                format!("the guest carries no lazy program `{file}`"),
            )
        };
        let mut program = match &self.from {
            LazyFrom::None => return Err(carried_none()),
            LazyFrom::Carried(programs) => programs.get(file).cloned().ok_or_else(carried_none)?,
            LazyFrom::Behavior(hosting, behavior) => hosting.lazy_program(behavior, file)?,
        };
        self.prepare.apply(&mut program);
        self.read.lock().insert(file.to_string(), program.clone());
        Ok(program)
    }
}

/// The guest kernel's own lazy loads (EXEC-LOAD-2), read through its [`GuestLazy`].
struct GuestLazySource(Arc<GuestLazy>);

impl LazySource for GuestLazySource {
    fn read(&self, file: &str) -> Result<LazyProgram, String> {
        self.0
            .program(file)
            .map(|schema| LazyProgram { schema })
            .map_err(|failure| failure.message)
    }
}

/// The part of `program` a split guest's viewer runs: its `[client]` traits and pages.
fn client_program_of(program: &OrbitalSchema) -> Value {
    orbital_core_base::runs_on::client_program(
        orbital_core_base::value_serialize::to_canonical_value(program).unwrap_or(Value::Null),
    )
}

/// `client` with the orbitals of the resolved client-kernel `program` added (EXEC-ENDPOINT-3a): each keeps its program's
/// app-level grants as its own, its protocol pages stay behind (the browser mounts its traits from `dispatchKernel`), and
/// its named types join `client`'s. A trait or type name `client` already holds otherwise is refused.
fn with_client_kernel(client: &mut Value, program: &Value) -> Result<(), String> {
    let taken: std::collections::HashSet<String> = client
        .get("orbitals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|orbital| orbital.get("traits").and_then(Value::as_array).cloned().unwrap_or_default())
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let app_grants = program.get("osGrants").filter(|g| !matches!(g, Value::Null)).cloned();
    let mut joined = Vec::new();
    for orbital in program.get("orbitals").and_then(Value::as_array).cloned().unwrap_or_default() {
        let Value::Object(mut fields) = orbital else {
            return Err("an orbital is not a record".to_string());
        };
        for t in fields.get("traits").and_then(Value::as_array).cloned().unwrap_or_default() {
            if let Some(name) = t.get("name").and_then(Value::as_str) {
                if taken.contains(name) {
                    return Err(format!("trait {name} is already the program's"));
                }
            }
        }
        if let Some(app) = &app_grants {
            let mut grants: orbital_core_base::os_grants::OsGrants = orbital_core_base::json_value::from_json_value(&orbital_core_base::json_value::value_to_json(app)).map_err(|e| e.to_string())?;
            if let Some(own) = fields.get("osGrants").filter(|g| !matches!(g, Value::Null)) {
                let own: orbital_core_base::os_grants::OsGrants = orbital_core_base::json_value::from_json_value(&orbital_core_base::json_value::value_to_json(own)).map_err(|e| e.to_string())?;
                grants.union(&own);
            }
            fields.insert("osGrants".into(), orbital_core_base::value_serialize::to_canonical_value(&grants).map_err(|e| e.to_string())?);
        }
        if let Some(Value::Array(pages)) = fields.get("pages").cloned() {
            let kept: Vec<Value> = pages.iter().filter(|page| page.get("protocol").is_none()).cloned().collect();
            fields.insert("pages".into(), Value::array(kept));
        }
        joined.push(Value::object(fields));
    }
    let Value::Object(root) = client else {
        return Err("the client program is not a record".to_string());
    };
    if let Some(Value::Object(types)) = program.get("types") {
        let mut held = match root.get("types") {
            Some(Value::Object(held)) => (**held).clone(),
            _ => ValueMap::default(),
        };
        for (name, shape) in types.iter() {
            match held.get(name.as_str()) {
                Some(existing) if existing != shape => return Err(format!("type {name} is already the program's, with another shape")),
                _ => {
                    held.insert(name.clone(), shape.clone());
                }
            }
        }
        root.insert("types".into(), Value::object(held));
    }
    let mut orbitals = match root.get("orbitals") {
        Some(Value::Array(orbitals)) => orbitals.to_vec(),
        _ => Vec::new(),
    };
    orbitals.extend(joined);
    root.insert("orbitals".into(), Value::array(orbitals));
    Ok(())
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
    leg: Option<&Value>,
    reply: Option<&crate::resumable::GuestReply>,
    answer: &(dyn Fn(&crate::resumable::IoRequest) -> crate::resumable::IoAnswer + Sync),
) -> Result<Value, crate::effects::executor::SubstrateFailure> {
    if let Some(leg) = leg {
        return serve_leg(session, payload, leg).map_err(|refused| crate::effects::executor::SubstrateFailure {
            message: refused.error,
            errors: refused.errors,
        });
    }
    let io = |message: String| crate::agent_ports::guest_failure(orbital_core_base::os::OsErrorKind::Io, message);
    // Registered before the dispatch runs, so a reply its own cascade emits is not missed.
    let waiting = reply.map(|wanted| session.kernel.await_reply(wanted));
    let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
    for trait_name in targets {
        if let Some(host) = &session.host {
            outcomes.extend(
                host.dispatch(trait_name, event, payload.clone())
                    .map_err(|e| io(format!("program/dispatch: {e}")))?,
            );
            continue;
        }
        let progress = session
            .kernel
            .start_dispatch(trait_name, None, event, payload.clone(), None, None)
            .map_err(|e| io(format!("program/dispatch: {e}")))?;
        let ran = crate::resumable::drive_concurrent(&session.kernel, progress, answer)
            .map_err(|e| io(format!("program/dispatch: {e}")))?;
        outcomes.extend(ran);
    }
    let replied = match (reply, &waiting) {
        (Some(wanted), Some(waiting)) => Some(
            waiting
                .wait(std::time::Duration::from_millis(wanted.timeout_ms))
                .ok_or_else(|| {
                    let value = orbital_core_base::json_value::value_to_json(&wanted.value);
                    io(format!(
                        "program/dispatch: no {} for {} {} within {} ms",
                        wanted.events.join(" or "),
                        wanted.field,
                        value,
                        wanted.timeout_ms
                    ))
                })?,
        ),
        _ => None,
    };
    let seed_trait = targets.first().cloned().unwrap_or_default();
    let embedded = session.route.read().embedded.clone();
    let orbitals = painter_orbitals(&session.kernel, &outcomes);
    let mut answer = guest_answer(
        &session.kernel,
        &orbitals,
        None,
        None,
        &embedded,
        &outcomes,
        &seed_trait,
        event,
        None,
        &[],
    );
    if let (Some((event, payload)), Value::Object(fields)) = (replied, &mut answer) {
        let mut reply = ValueMap::default();
        reply.insert("event".into(), Value::string(event));
        reply.insert("payload".into(), payload);
        fields.insert("reply".into(), Value::object(reply));
    }
    Ok(answer)
}

/// `options.reply` of a `program/dispatch` into a guest: `{ events, match, timeoutMs }`, every part required, the match
/// field read from the dispatched payload. Only a keyed call may wait: an unkeyed one would hold its caller's dispatch.
fn guest_reply(
    args: &[Value],
    payload: &Value,
    keyed: bool,
) -> Result<Option<crate::resumable::GuestReply>, AgentPortFailure> {
    let Some(Value::Object(options)) = args.get(2) else {
        return Ok(None);
    };
    let reply = match options.get("reply") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(reply)) => reply,
        Some(_) => {
            return Err(failure(
                "program/dispatch: options.reply is a record { events, match, timeoutMs }",
            ))
        }
    };
    if !keyed {
        return Err(failure("program/dispatch: a reply is waited for only on a keyed call (give the call a key and routes)"));
    }
    let events: Vec<String> = match reply.get("events") {
        Some(Value::Array(events)) if !events.is_empty() => events
            .iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect(),
        _ => {
            return Err(failure(
                "program/dispatch: reply.events names the events that answer the call",
            ))
        }
    };
    let field = match reply.get("match") {
        Some(Value::String(field)) if !field.is_empty() => field.to_string(),
        _ => {
            return Err(failure(
                "program/dispatch: reply.match names the payload field the reply carries back",
            ))
        }
    };
    let timeout_ms = match reply.get("timeoutMs") {
        Some(Value::Int(ms)) if *ms > 0 => *ms as u64,
        Some(Value::Number(ms)) if *ms > 0.0 => *ms as u64,
        _ => {
            return Err(failure(
                "program/dispatch: reply.timeoutMs is how long to wait, in milliseconds",
            ))
        }
    };
    let value = match payload.get(field.as_str()) {
        Some(value) if !matches!(value, Value::Null) => value.clone(),
        _ => {
            return Err(failure(format!(
                "program/dispatch: the payload carries no {field} for the reply to match"
            )))
        }
    };
    Ok(Some(crate::resumable::GuestReply {
        events,
        field,
        value,
        timeout_ms,
    }))
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

/// A local guest's environment (EXEC-CONC-7): it runs with no host and no OS, so `os/getenv` reads the `env` it was opened
/// with (a name it was not given is unset) and `os/environ` lists those names; every other `os/` ask stays unanswered.
struct LocalGuestEnv(std::collections::BTreeMap<String, String>);

impl LocalGuestEnv {
    fn of(env: &[(String, Option<String>)]) -> Self {
        Self(
            env.iter()
                .filter_map(|(name, value)| Some((name.clone(), value.clone()?)))
                .collect(),
        )
    }
}

impl crate::resumable::DirectOs for LocalGuestEnv {
    fn ask(
        &self,
        request: crate::resumable::IoRequest,
        _site: Option<crate::resumable::CallSite>,
    ) -> Option<crate::resumable::IoAnswer> {
        use orbital_core_base::os::OsOp;
        let crate::resumable::IoRequest::Os { op, args, .. } = request else {
            return None;
        };
        let mut fields = ValueMap::default();
        match op {
            OsOp::Getenv => {
                let name = args.first().and_then(Value::as_str)?;
                if let Some(value) = self.0.get(name) {
                    fields.insert("value".into(), Value::string(value.clone()));
                }
            }
            OsOp::Environ => {
                fields.insert(
                    "names".into(),
                    Value::array(
                        self.0
                            .keys()
                            .cloned()
                            .map(Value::string)
                            .collect::<Vec<_>>(),
                    ),
                );
            }
            _ => return None,
        }
        Some(crate::resumable::IoAnswer::Os(Ok(
            Value::object(fields).into()
        )))
    }
}

/// A split guest's server leg, answered by its host as a served events request (EXEC-CONC-7). Only a split guest takes
/// one: its viewer's kernel runs its `[client]` traits, so the host answers it as a served program's server.
/// A leg carrying `inbound` is a hook delivery, which any hosted guest takes (EXEC-INBOUND-1).
fn serve_leg(
    session: &GuestSession,
    payload: &Value,
    leg: &Value,
) -> Result<Value, AgentPortFailure> {
    if let (Value::Object(ask), Some(host)) = (leg, &session.host) {
        if ask.contains_key("inbound") {
            return host.inbound(ask).map_err(|refused| AgentPortFailure {
                error: refused.message,
                errors: refused.errors,
            });
        }
    }
    let (Some(host), true) = (&session.host, session.split) else {
        return Err(guest_refusal(
            orbital_core_base::os::OsErrorKind::Invalid,
            "program/dispatch: an events request goes only to a guest opened with `split: true`",
        ));
    };
    let Value::Object(options) = leg else {
        return Err(failure(
            "program/dispatch: a wire leg's options are a record",
        ));
    };
    match options.get("lolo").and_then(Value::as_str) {
        Some("begin") => {
            let mut options = options.clone();
            options.shift_remove("lolo");
            return host.begin_leg(payload, &options).map_err(|refused| AgentPortFailure { error: refused.message, errors: refused.errors });
        }
        Some("finish") => {
            let leg = options.get("leg").and_then(Value::as_str).ok_or_else(|| failure("program/dispatch: { lolo: \"finish\" } names the leg begin opened"))?;
            return host.finish_leg(leg).map_err(|refused| AgentPortFailure { error: refused.message, errors: refused.errors });
        }
        Some("abandon") => return abandon_leg(host.as_ref(), options),
        Some("settle") => {
            let leg = options.get("leg").and_then(Value::as_str).ok_or_else(|| failure("program/dispatch: { lolo: \"settle\" } names the leg begin opened"))?;
            return host.settle_leg(leg).map_err(|e| failure(format!("program/dispatch: {e}")));
        }
        Some(other) => return Err(failure(format!("program/dispatch: lolo is \"begin\", \"finish\", \"settle\" or \"abandon\", not \"{other}\""))),
        None => {}
    }
    host.request(payload, options)
        .map_err(|refused| AgentPortFailure {
            error: refused.message,
            errors: refused.errors,
        })
}

/// `{ lolo: "abandon", leg, error }`: close a leg its `.lolo` kernel failed; answers `{ abandoned: true }`.
fn abandon_leg(host: &dyn crate::kernel::guest_hosting::GuestHost, options: &ValueMap) -> Result<Value, AgentPortFailure> {
    let leg = options.get("leg").and_then(Value::as_str).ok_or_else(|| failure("program/dispatch: { lolo: \"abandon\" } names the leg begin opened"))?;
    let error = options.get("error").and_then(Value::as_str).unwrap_or_default();
    host.abandon_leg(leg, error).map_err(|e| failure(format!("program/dispatch: {e}")))?;
    Ok(Value::object(ValueMap::from_iter([("abandoned".into(), Value::Bool(true))])))
}

fn json_value(json: serde_json::Value) -> Value {
    from_json_value(&json).unwrap_or(Value::Null)
}

/// The orbital declaring each trait that painted in `outcomes`: a paint names its orbital, so a host scopes the paint's
/// controls to the trait that drew them, as it scopes its own.
pub(super) fn painter_orbitals(
    kernel: &RuntimeKernel,
    outcomes: &[(String, ProcessEventResult)],
) -> std::collections::HashMap<String, String> {
    EventResponse::from_process_results(outcomes, "", "")
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|paint| {
            kernel
                .orbital_of_trait(&paint.trait_name)
                .map(|orbital| (paint.trait_name.clone(), orbital))
        })
        .collect()
}

/// What a guest's run answers: each trait's state, what it painted (with its trait's orbital), what it emitted and every
/// trait it moved.
/// The paints (`{ slot, trait, orbital, embedded, content }`) and navigations (`{ trait, path }`) a response's client
/// effects carry, as a guest answer reports them.
pub(super) fn paints_of(
    response: &EventResponse,
    orbitals: &std::collections::HashMap<String, String>,
    embedded: &std::collections::HashSet<String>,
) -> (Vec<Value>, Vec<Value>) {
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
                (
                    "orbital",
                    orbitals
                        .get(&paint.trait_name)
                        .map_or(Value::Null, |o| Value::string(o.clone())),
                ),
                (
                    "embedded",
                    Value::Bool(embedded.contains(&paint.trait_name)),
                ),
                ("content", config.clone().unwrap_or(Value::Null)),
            ])),
            _ => None,
        })
        .collect();
    let navigations: Vec<Value> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|step| match &step.effect {
            ClientEffect::Navigate { path, .. } => Some(record(vec![
                ("trait", Value::string(step.trait_name.clone())),
                ("path", Value::string(path.clone())),
            ])),
            _ => None,
        })
        .collect();
    (rendered, navigations)
}

fn guest_answer(
    kernel: &RuntimeKernel,
    orbitals: &std::collections::HashMap<String, String>,
    guest: Option<&str>,
    page: Option<(&[String], Option<&str>)>,
    embedded: &std::collections::HashSet<String>,
    outcomes: &[(String, ProcessEventResult)],
    seed_trait: &str,
    event: &str,
    access: Option<&super::page_circuit::PageAccessVerdict>,
    rerenders: &[super::Rerender],
) -> Value {
    let mut response = EventResponse::from_process_results(outcomes, seed_trait, event);
    kernel.stamp_emitted_sources(&mut response);
    response.fold_rerenders(rerenders);
    let record = |pairs: Vec<(&str, Value)>| {
        let mut fields = ValueMap::default();
        for (key, value) in pairs {
            fields.insert(key.into(), value);
        }
        Value::object(fields)
    };
    let (rendered, navigations) = paints_of(&response, orbitals, embedded);
    let host_asks: Vec<Value> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter_map(|step| match &step.effect {
            ClientEffect::HostAsk {
                op,
                args,
                completion,
            } => Some(record(vec![
                ("trait", Value::string(step.trait_name.clone())),
                ("op", Value::string(op.clone())),
                (
                    "ask",
                    Value::string(orbital_core_base::host_ask::write(
                        op,
                        args,
                        completion.as_ref(),
                    )),
                ),
            ])),
            _ => None,
        })
        .collect();
    let emitted: Vec<Value> = response
        .emitted_events
        .iter()
        .map(|e| {
            record(vec![
                ("event", Value::string(e.event.clone())),
                ("payload", e.payload.clone().unwrap_or(Value::Null)),
            ])
        })
        .collect();
    let moved: Vec<Value> = outcomes
        .iter()
        .map(|(trait_name, result)| {
            let mut fields = vec![
                ("trait", Value::string(trait_name.clone())),
                (
                    "event",
                    result
                        .event
                        .clone()
                        .map(Value::string)
                        .unwrap_or(Value::Null),
                ),
                ("from", Value::string(result.previous_state.clone())),
                ("to", Value::string(result.new_state.clone())),
                ("fired", Value::Bool(result.executed)),
            ];
            if let Some(cost) = result.cost {
                fields.push(("cost", cost_record(&cost)));
            }
            record(fields)
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
    if !navigations.is_empty() {
        answer.push(("navigations", Value::array(navigations)));
    }
    if !host_asks.is_empty() {
        answer.push(("hostAsks", Value::array(host_asks)));
    }
    if let Some(guest) = guest {
        answer.push(("guest", Value::string(guest.to_string())));
    }
    if let Some((page, route)) = page {
        answer.push((
            "page",
            Value::array(
                page.iter()
                    .cloned()
                    .map(Value::string)
                    .collect::<Vec<Value>>(),
            ),
        ));
        if let Some(route) = route {
            answer.push(("route", Value::string(route.to_string())));
        }
    }
    if let Some(rejections) = response.rejections.as_ref().filter(|r| !r.is_empty()) {
        answer.push((
            "rejections",
            serde_json::to_value(rejections)
                .map(json_value)
                .unwrap_or(Value::Null),
        ));
    }
    if let Some(access) = access {
        answer.push((
            "access",
            serde_json::to_value(access)
                .map(json_value)
                .unwrap_or(Value::Null),
        ));
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
        for entity in std::iter::once(&mut orbital.entity)
            .chain(orbital.auxiliary_entities.iter_mut().flatten())
        {
            if let orbital_core_base::schema::types::EntityRef::Inline(def) = entity {
                def.store = None;
            }
        }
    }
}

fn view_page(guest: &RuntimeKernel, page: &[String], embedded: &std::collections::HashSet<String>) {
    guest.release_client(GUEST_VIEWER);
    let mut by_orbital: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
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
    access: super::page_circuit::PageAccessVerdict,
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
    let denied = matches!(
        circuit.access,
        super::page_circuit::PageAccessVerdict::Denied { .. }
    );
    Ok(RoutedPage {
        route: if denied {
            None
        } else {
            circuit.page.as_ref().and(circuit.route)
        },
        access: circuit.access,
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
        None => guest
            .dispatch_route_mount(route, seeds, None, payload)
            .map_err(|e| e.to_string()),
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
                guest_refusal(
                    orbital_core_base::os::OsErrorKind::Closed,
                    format!("{op}: no open guest '{}' (it was never opened or is closed)", name.as_str()),
                )
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
                format!(
                    "{op}: no kernel guest grant covers it (asked from orbital {}, trait {})",
                    asker.as_deref().unwrap_or("none"),
                    self.caller_position().1
                ),
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
        let path = match options.as_ref().and_then(|o| o.get("path")) {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) if matches!(program, Value::Null) => Some(path.to_string()),
            Some(Value::String(_)) => return Err(failure("program/open: name the program or its `path`, not both: the program is null when `path` is given")),
            Some(_) => return Err(failure("program/open: `path` is a file path")),
        };
        // `program: "declared"` gives the program port only to a guest whose own program grants declare `kernel: guest [program]`.
        let (give_program, program_if_declared) = match options.as_ref().and_then(|o| o.get("program").cloned()) {
            None => (false, false),
            Some(Value::Bool(give)) => (give, false),
            Some(Value::String(word)) if word.as_str() == "declared" => (false, true),
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `program` is true, false or \"declared\"",
                ))
            }
        };
        let split = match options.as_ref().and_then(|o| o.get("split")) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(split)) => *split,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `split` is true or false",
                ))
            }
        };
        let mount = match options.as_ref().and_then(|o| o.get("mount")) {
            None | Some(Value::Null) => true,
            Some(Value::Bool(mount)) => *mount,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `mount` is true or false",
                ))
            }
        };
        let lolo = match options.as_ref().and_then(|o| o.get("kernel")) {
            None | Some(Value::Null) => false,
            Some(Value::String(kernel)) if kernel.as_str() == "native" => false,
            Some(Value::String(kernel)) if kernel.as_str() == "lolo" => true,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `kernel` is \"native\" or \"lolo\"",
                ))
            }
        };
        let client_kernel = match options.as_ref().and_then(|o| o.get("clientKernel")) {
            None | Some(Value::Null) => None,
            Some(named) if lolo => Some((
                orbital_core_base::json_value::from_json_value::<orbital_core_base::schema::DispatchKernel>(&orbital_core_base::json_value::value_to_json(named))
                    .map_err(|e| guest_refusal(orbital_core_base::os::OsErrorKind::Invalid, format!("program/open: `clientKernel` is {{ trait, mount, program? }}: {e}")))?,
                named.get("program").filter(|program| !matches!(program, Value::Null)).cloned(),
            )),
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `clientKernel` names the .lolo kernel a client program dispatches through; it needs kernel: \"lolo\"",
                ))
            }
        };
        let seed_writer = match options.as_ref().and_then(|o| o.get("seed")) {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if !name.as_str().is_empty() => Some(name.to_string()),
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `seed` names the guest's seed-writer trait",
                ))
            }
        };
        let local = match options.as_ref().and_then(|o| o.get("local")) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(local)) => *local,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `local` is true or false",
                ))
            }
        };
        if local {
            let held = ["grants", "arenas", "declared", "split", "program", "cwd"]
                .into_iter()
                .filter(|name| {
                    options.as_ref().is_some_and(|o| {
                        o.get(*name)
                            .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
                    })
                })
                .collect::<Vec<_>>();
            if !held.is_empty() {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    format!("program/open: a `local` guest runs in this kernel with no host, so it takes no {}", held.join(", ")),
                ));
            }
        }
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
        let withhold = match options.as_ref().and_then(|o| o.get("withhold")) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .map(|kind| match kind {
                    Value::String(word) if word.as_str() == "listen" => Ok(GuestWithhold::Listen),
                    Value::String(word) if word.as_str() == "self-signal" => {
                        Ok(GuestWithhold::SelfSignal)
                    }
                    _ => Err(()),
                })
                .collect::<Result<Vec<_>, ()>>()
                .map_err(|()| {
                    guest_refusal(
                        orbital_core_base::os::OsErrorKind::Invalid,
                        "program/open: `withhold` is a list of \"listen\" and \"self-signal\"",
                    )
                })?,
            Some(_) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    "program/open: `withhold` is a list of \"listen\" and \"self-signal\"",
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
        let cwd = match options.as_ref().and_then(|o| o.get("cwd")) {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) if integrations == GuestIntegrations::Live => {
                Some(path.to_string())
            }
            Some(Value::String(_)) => return Err(failure(
                "program/open: `cwd` is for a live guest; a mocked guest has no files of its own",
            )),
            Some(_) => return Err(failure("program/open: `cwd` is a directory path")),
        };
        let env: Vec<(String, Option<String>)> = match options.as_ref().and_then(|o| o.get("env")) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Object(fields)) => fields
                .iter()
                .map(|(name, value)| match value {
                    Value::Null => Ok((name.to_string(), None)),
                    Value::String(text) => Ok((name.to_string(), Some(text.to_string()))),
                    _ => Err(failure(
                        "program/open: an `env` value is text, or null for unset",
                    )),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(failure("program/open: `env` is a record of variable names")),
        };
        let mut granted: Option<OsGrants> = None;
        let mut opener_held: Option<OsGrants> = None;
        if asked_grants.is_some() || asked_arenas.is_some() || keep_declared || give_program {
            let held = self.may_spawn_guest("program/open")?;
            if give_program && !held.kernel_allows(KernelKind::Program, None) {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    "program/open: `program` needs the grant kernel: { \"guest\": [spawn, program] }",
                ));
            }
            let guest: OsGrants = from_json_value(
                &(value_to_json(
                    asked_grants
                        .as_ref()
                        .unwrap_or(&Value::object(ValueMap::default())),
                )),
            )
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
            if let Some(dir) = &cwd {
                let asked: OsGrants = from_json_value(&(serde_json::json!({
                    "fs": [{ "glob": format!("{dir}/**"), "modes": ["read", "write", "create", "remove"] }]
                })))
                .map_err(|e| failure(format!("program/open: `cwd` is not a grantable path: {e}")))?;
                covers(&held, &asked).map_err(|missing| {
                    guest_refusal(
                        orbital_core_base::os::OsErrorKind::Denied,
                        format!("program/open: the guest's `cwd` needs grants the opening orbital does not hold: {}", missing.join(", ")),
                    )
                })?;
            }
            granted = Some(guest);
            opener_held = Some(held);
        }
        if local && !env.is_empty() {
            opener_held = Some(self.may_spawn_guest("program/open")?);
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
        if !env.is_empty() {
            let held = opener_held.as_ref().ok_or_else(|| {
                failure("program/open: `env` needs the guest grant `declared` or `grants`")
            })?;
            if let Some((name, _)) = env.iter().find(|(name, _)| !held.env_allows(name)) {
                return Err(failure(format!(
                    "program/open: `env` names {name}, which the opener holds no grant for"
                )));
            }
        }
        let hosting = if local { None } else { self.guest_hosting() };
        if split && hosting.is_none() {
            return Err(guest_refusal(
                orbital_core_base::os::OsErrorKind::Unsupported,
                "program/open: a split guest needs a host that offers guest hosting; this one runs guests in its own kernel",
            ));
        }
        let cost_meter = self.cost_meter();
        let guest_seq = Arc::clone(&self.guest_seq);
        let opener_session = self.thread_session().map(|session| session.to_string());
        let guests = Arc::clone(&self.guests);
        let settler = self.memory_settler.read().clone();
        let finish = move |mut schema: OrbitalSchema,
                           carried: Option<std::collections::HashMap<String, OrbitalSchema>>,
                           lazy_behavior: Option<String>|
              -> Result<Value, AgentPortFailure> {
            let declares_program = schema.os_grants.as_ref().is_some_and(|grants| grants.kernel_allows(KernelKind::Program, None));
            let give_program = give_program || (program_if_declared && declares_program);
            if program_if_declared && declares_program && !opener_held.as_ref().is_some_and(|held| held.kernel_allows(KernelKind::Program, None)) {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    "program/open: the guest declares `kernel: guest [program]`, which needs the opener's grant kernel: { \"guest\": [spawn, program] }",
                ));
            }
            let host = hosting
                .as_ref()
                .map(|hosting| hosting.open(integrations, cwd.as_deref(), &env, give_program));
            if lolo {
                let Some(host) = &host else {
                    return Err(guest_refusal(
                        orbital_core_base::os::OsErrorKind::Unsupported,
                        "program/open: a guest the .lolo kernel runs needs a host of its own (kernel: \"lolo\")",
                    ));
                };
                host.drive_by_lolo().map_err(|refused| guest_refusal(orbital_core_base::os::OsErrorKind::Unsupported, format!("program/open: {refused}")))?;
            }
            let prepare = GuestPrepare {
                mock: integrations == GuestIntegrations::Mock,
                apply_grants: granted.is_some() || host.is_some(),
                keep_declared,
                live: integrations == GuestIntegrations::Live,
                opener_held: opener_held.clone(),
                granted: granted.clone(),
                withhold,
            };
            prepare.apply(&mut schema);
            let from = match (carried, lazy_behavior, &hosting) {
            (Some(programs), _, _) => LazyFrom::Carried(programs),
            (None, Some(behavior), Some(hosting)) => LazyFrom::Behavior(Arc::clone(hosting), behavior),
            (None, Some(behavior), None) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Unsupported,
                    format!("program/open: `lazySource` names `{behavior}`, but this host offers no guest hosting to read its lazy programs"),
                ))
            }
            (None, None, _) => LazyFrom::None,
        };
            let has_lazy = !matches!(from, LazyFrom::None);
            let lazy = Arc::new(GuestLazy {
                from,
                prepare,
                read: orbital_core_base::sync::Mutex::new(Default::default()),
            });
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
                    guest.set_cost_meter(cost_meter);
                    if local && !env.is_empty() {
                        guest.set_direct_os(Some(Arc::new(LocalGuestEnv::of(&env))));
                    }
                    (Arc::new(guest), Some(source))
                }
            };
            guest.set_runs_client_traits(!split);
            let client = match split {
                false => None,
                true => {
                    let mut client = client_program_of(&schema);
                    if let Some((kernel, program)) = &client_kernel {
                        if let Some(program) = program {
                            with_client_kernel(&mut client, program).map_err(|e| guest_refusal(orbital_core_base::os::OsErrorKind::Invalid, format!("program/open: `clientKernel.program`: {e}")))?;
                        }
                        if let Value::Object(fields) = &mut client {
                            fields.insert("dispatchKernel".into(), orbital_core_base::value_serialize::to_canonical_value(kernel).unwrap_or(Value::Null));
                        }
                    }
                    Some(client)
                }
            };
            if has_lazy {
                guest.set_lazy_source(Arc::new(GuestLazySource(Arc::clone(&lazy))));
            }
            match &host {
                Some(host) => host
                    .register(schema)
                    .map_err(|e| failure(format!("program/open: {e}")))?,
                None => guest
                    .register_schema(schema)
                    .map_err(|e| failure(format!("program/open: {e}")))?,
            }
            if let Some(name) = seed_writer {
                if !guest.registry.trait_definitions.read().contains_key(&name) {
                    return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Invalid,
                    format!("program/open: `seed` names `{name}`, which is not a trait of the program"),
                ));
                }
                guest.set_seed_writer(Some(name));
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
                access,
            } = routed_mounts(&guest, route.as_deref(), "program/open")?;
            let page: Vec<String> = routed.iter().map(|seed| seed.r#trait.clone()).collect();
            if !split {
                for seed in routed {
                    if !seeds.iter().any(|held| held.r#trait == seed.r#trait) {
                        seeds.push(seed);
                    }
                }
            }
            let outcomes = if mount {
                mount_seeds(&guest, host.as_deref(), resolved.as_deref(), &seeds, "program/open")?
            } else {
                Vec::new()
            };
            if !split {
                view_page(&guest, &page, &embedded);
            }
            let orbitals = painter_orbitals(&guest, &outcomes);
            let handle = format!("guest-{}", guest_seq.fetch_add(1, Ordering::SeqCst) + 1);
            let mounted = seeds.iter().map(|s| s.r#trait.clone()).collect();
            let opened_kernel = guest.clone();
            let session = GuestSession {
                kernel: guest,
                source,
                host,
                grants: granted,
                opener_session,
                split,
                lazy,
                arenas,
                route: Arc::new(RwLock::new(GuestRoute {
                    embedded: embedded.clone(),
                    mounted,
                })),
                viewer_ticked_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                ticks: Arc::new(orbital_core_base::sync::Mutex::new(crate::tick_clock::TickClock::from_programs(&opened_kernel.registered_programs()))),
            };
            let ticker = match (&session.host, split) {
                (Some(host), true) => Some((Arc::clone(host), Arc::clone(&session.viewer_ticked_ms))),
                _ => None,
            };
            guests.write().insert(handle.clone(), session);
            if let Some((host, viewer_ticked_ms)) = ticker {
                tick_while_unwatched(Arc::downgrade(&guests), handle.clone(), host, viewer_ticked_ms);
            }
            let seed_trait = seeds.first().map(|s| s.r#trait.clone()).unwrap_or_default();
            let mut answer = guest_answer(
                &opened_kernel,
                &orbitals,
                Some(&handle),
                Some((&page, resolved.as_deref())),
                &embedded,
                &outcomes,
                &seed_trait,
                "INIT",
                Some(&access),
                &[],
            );
            if let (Some(client), Value::Object(fields)) = (client, &mut answer) {
                fields.insert("client".into(), client);
            }
            if let (true, Value::Object(fields)) = (split, &mut answer) {
                let mut runs: Vec<String> = opened_kernel.browser_run_traits().into_iter().collect();
                runs.sort();
                fields.insert("browserRuns".into(), Value::array(runs.into_iter().map(Value::string).collect::<Vec<_>>()));
            }
            if let (false, Value::Object(fields)) = (mount, &mut answer) {
                let unmounted = seeds
                    .iter()
                    .map(|seed| {
                        Value::object(ValueMap::from_iter([
                            ("trait".to_string().into(), Value::string(seed.r#trait.clone())),
                            ("event".to_string().into(), Value::string(seed.event.as_str())),
                        ]))
                    })
                    .collect::<Vec<_>>();
                fields.insert("seeds".into(), Value::array(unmounted));
                fields.insert("route".into(), resolved.clone().map_or(Value::Null, Value::string));
            }
            if let Some(settle) = settler {
                settle();
            }
            Ok(answer)
        };
        match path {
            None => {
                let (schema, carried, lazy_behavior) = parse_open_program(program)?;
                finish(schema, carried, lazy_behavior)
            }
            Some(path) => {
                let hosting = self.guest_hosting().ok_or_else(|| guest_refusal(
                    orbital_core_base::os::OsErrorKind::Unsupported,
                    "program/open: `path` needs a host that offers guest hosting to read the program",
                ))?;
                let work = crate::resumable::GuestOpenWork::of(Box::new(move || {
                    let (schema, lazy) = hosting.read_program(&path)?;
                    let carried = (!lazy.is_empty()).then_some(lazy);
                    finish(schema, carried, None).map_err(|refused| {
                        crate::effects::executor::SubstrateFailure {
                            message: refused.error,
                            errors: refused.errors,
                        }
                    })
                }));
                self.io
                    .guest_open(work, args.to_vec())
                    .map(|answer| answer.unwrap_or(Value::Null))
                    .map_err(|refused| AgentPortFailure {
                        error: refused.message,
                        errors: refused.errors,
                    })
            }
        }
    }

    /// `program/lazy { guest, file }`: the client program of the lazy program the split guest carries under `file`, for
    /// its viewer's kernel to load when a route first needs it (EXEC-CONC-7, EXEC-LOAD-2).
    pub(super) fn lazy_guest_page(&self, args: &[Value]) -> Result<Value, AgentPortFailure> {
        let Some(Value::Object(options)) = args.first() else {
            return Err(failure("program/lazy: name `{ guest, file }`"));
        };
        let handle = options
            .get("guest")
            .ok_or_else(|| failure("program/lazy: name the `guest` program/open answered"))?;
        let Some(Value::String(file)) = options.get("file") else {
            return Err(failure("program/lazy: `file` is the lazy page's file"));
        };
        let session = self.guest(handle, "program/lazy")?;
        if !session.split {
            return Err(guest_refusal(
                orbital_core_base::os::OsErrorKind::Invalid,
                "program/lazy: the guest is not split, so its viewer loads no page",
            ));
        }
        let child = session
            .lazy
            .program(file.as_str())
            .map_err(|refused| AgentPortFailure {
                error: format!("program/lazy: {}", refused.message),
                errors: refused.errors,
            })?;
        Ok(Value::object(
            orbital_core_base::value::ValueMap::from_iter([
                ("file".into(), Value::String(file.as_str().into())),
                ("program".into(), client_program_of(&child)),
            ]),
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
        let mount = match options.get("mount") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(mount)) => *mount,
            Some(_) => return Err(failure("program/navigate: `mount` is true or false")),
        };
        if remount {
            // A remount enters the page afresh: each of its mounted traits starts again from its initial state with no
            // frame, so its lifecycle event is the one it handles there.
            let traits = guest
                .page_circuit(None, Some(route.as_str()))
                .map_err(|e| failure(format!("program/navigate: {e}")))?
                .traits;
            let held = session.route.read();
            for name in traits.iter().filter(|name| held.mounted.contains(*name)) {
                let initial = guest
                    .registry
                    .trait_definitions
                    .read()
                    .get(name)
                    .and_then(|def| {
                        let states = &def.state_machine.states;
                        states
                            .iter()
                            .find(|s| s.is_initial == Some(true))
                            .or_else(|| states.first())
                            .map(|s| s.name.clone())
                    });
                if let Some(state) = initial {
                    guest.restore_trait(
                        name,
                        None,
                        super::TraitSnapshot {
                            state,
                            entity: Value::Null,
                        },
                    );
                }
            }
        }
        let RoutedPage {
            route: resolved,
            mounts: routed,
            embedded,
            access,
        } = routed_mounts(guest, Some(route.as_str()), "program/navigate")?;
        if matches!(
            access,
            super::page_circuit::PageAccessVerdict::Denied { .. }
        ) {
            let name = match handle {
                Value::String(name) => Some(name.as_str()),
                _ => None,
            };
            return Ok(guest_answer(
                guest,
                &std::collections::HashMap::new(),
                name,
                Some((&[], None)),
                &embedded,
                &[],
                "",
                "INIT",
                Some(&access),
                &[],
            ));
        }
        let page: Vec<String> = routed.iter().map(|seed| seed.r#trait.clone()).collect();
        let fresh: Vec<MountSeed> = {
            let held = session.route.read();
            routed
                .into_iter()
                .filter(|seed| remount || !held.mounted.contains(&seed.r#trait))
                .collect()
        };
        let outcomes = if mount {
            mount_seeds(guest, session.host.as_deref(), resolved.as_deref(), &fresh, "program/navigate")?
        } else {
            Vec::new()
        };
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
        let mut answer = guest_answer(
            guest,
            &orbitals,
            name,
            Some((&page, resolved.as_deref())),
            &embedded,
            &outcomes,
            &seed_trait,
            "INIT",
            Some(&access),
            &[],
        );
        if let (false, Value::Object(fields)) = (mount, &mut answer) {
            let unmounted = fresh
                .iter()
                .map(|seed| {
                    Value::object(ValueMap::from_iter([
                        ("trait".to_string().into(), Value::string(seed.r#trait.clone())),
                        ("event".to_string().into(), Value::string(seed.event.as_str())),
                    ]))
                })
                .collect::<Vec<_>>();
            fields.insert("seeds".into(), Value::array(unmounted));
        }
        Ok(answer)
    }

    /// `program/live { kind: "guest", guest }`, polled by the host's stream: one tick pass of the hosted guest `handle`,
    /// with what its deferred completions moved, painted and emitted since the last pass (EXEC-DIST-14, EXEC-CONC-7). A guest without a host of its own has no clock to tick on and is refused.
    pub fn guest_tick_pass(&self, handle: &str) -> Result<GuestTicks, String> {
        let Some(session) = self.guests.read().get(handle).cloned() else {
            return Ok(GuestTicks::Closed);
        };
        session.viewer_ticked_ms.store(guest_ticks_now_ms(), std::sync::atomic::Ordering::Relaxed);
        // A stream keeps its session alive, so the guest's own must end for a closed tab's session to be freed.
        if session
            .opener_session
            .as_deref()
            .is_some_and(|opener| self.session_lives.streams_dropped(opener))
        {
            return Ok(GuestTicks::Closed);
        }
        let Some(host) = &session.host else {
            return Err(format!(
                "program/live: guest {handle} runs in this kernel with no host of its own, so nothing ticks it"
            ));
        };
        let outcomes = host.tick_pass()?;
        let dispatches = host.take_host_dispatches();
        let runs = host.take_parked_runs();
        if outcomes.is_empty() && dispatches.is_empty() && runs.is_empty() {
            return Ok(GuestTicks::Idle);
        }
        let embedded = session.route.read().embedded.clone();
        let orbitals = painter_orbitals(&session.kernel, &outcomes);
        let seed_trait = outcomes.first().map(|(t, _)| t.clone()).unwrap_or_default();
        // A tick that changed a field an earlier paint reads repaints it: the guest stands in for the browser
        // (EXEC-ENDPOINT-3a), so its kernel resolves those paints (owner 2026-11-12, G-ORBITAL-CORE-148).
        let rerenders = session.kernel.rerender_stale();
        let mut answer = guest_answer(
            &session.kernel,
            &orbitals,
            Some(handle),
            None,
            &embedded,
            &outcomes,
            &seed_trait,
            "",
            None,
            &rerenders,
        );
        if let Value::Object(fields) = &mut answer {
            fields.insert("dispatches".into(), Value::Array(dispatches.into()));
            if !runs.is_empty() {
                fields.insert("runs".into(), Value::Array(runs.into()));
            }
        }
        // The opener reacts to an item; a pass that painted and emitted nothing (a paused clock accumulating) is no item.
        let empty = |field: &str| {
            answer
                .get(field)
                .and_then(Value::as_array)
                .is_none_or(|items| items.is_empty())
        };
        if empty("rendered") && empty("emitted") && empty("hostAsks") && empty("dispatches") && empty("runs") {
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
            let closed = self.guests.write().remove(name.as_str());
            drop(closed);
        }
        self.settle_memory();
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
        if let Some(Value::Object(form)) = args.first() {
            if form.contains_key("ask") {
                return self.answer_guest_ask(&session, handle, form, args);
            }
            if form.contains_key("inbound") {
                let ask = orbital_core_base::agent_ports::inbound_ask(form).map_err(failure)?;
                return self.run_in_guest(
                    &session,
                    handle,
                    Vec::new(),
                    "",
                    Value::Null,
                    Some(Value::object(ask)),
                    args,
                );
            }
        }
        // A `.lolo` leg's begin and finish name the leg or held completion they open or answer, not an address.
        if let Some(Value::Object(options)) = args.get(2) {
            if options.get("plain").and_then(Value::as_bool) == Some(true) && options.contains_key("lolo") {
                return self.plain_leg(&session, args, options);
            }
            if options.contains_key("lolo") {
                let mut leg = options.clone();
                leg.shift_remove("guest");
                let given = orbital_core_base::agent_ports::dispatch_arguments(args.get(1)).map_err(failure)?;
                let payload = self.copy_into(&session, &given)?;
                return serve_leg(&session, &payload, &Value::object(leg));
            }
        }
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
        let given =
            orbital_core_base::agent_ports::dispatch_arguments(args.get(1)).map_err(failure)?;
        let payload = self.copy_into(&session, &given)?;
        let leg = match args.get(2) {
            // An events request names its orbital, as a served program/dispatch request does; `wire` picks its shape.
            Some(Value::Object(options)) if options.contains_key("orbital") => {
                let mut leg = options.clone();
                leg.shift_remove("guest");
                Some(Value::object(leg))
            }
            _ => None,
        };
        self.run_in_guest(&session, handle, targets, event, payload, leg, args)
    }

    /// A plain guest dispatch run by a `.lolo` kernel: `lolo: "begin"` opens it as a leg on the guest's host for the
    /// address's targets, `lolo: "finish", leg` answers what its steps did as the plain dispatch answers.
    fn plain_leg(&self, session: &GuestSession, args: &[Value], options: &ValueMap) -> Result<Value, AgentPortFailure> {
        let Some(host) = &session.host else {
            return Err(failure("program/dispatch: a plain .lolo leg runs on a guest with a host of its own"));
        };
        match options.get("lolo").and_then(Value::as_str) {
            Some("begin") => {
                let Some(Value::String(address)) = args.first() else {
                    return Err(failure("program/dispatch: a guest address is `Orbital.Trait.EVENT` or `Trait.EVENT`"));
                };
                let parts: Vec<&str> = address.as_str().split('.').collect();
                let (targets, event): (Vec<String>, &str) = match parts.as_slice() {
                    [_, trait_name, event] | [trait_name, event] => (vec![trait_name.to_string()], *event),
                    [event] => (session.kernel.traits_handling(event), *event),
                    _ => return Err(failure(format!("program/dispatch: '{}' is not `Orbital.Trait.EVENT`, `Trait.EVENT` or `EVENT`", address.as_str()))),
                };
                let given = orbital_core_base::agent_ports::dispatch_arguments(args.get(1)).map_err(failure)?;
                let payload = self.copy_into(session, &given)?;
                host.begin_plain(&targets, event, payload).map_err(failure)
            }
            Some("finish") => {
                let leg = options.get("leg").and_then(Value::as_str).ok_or_else(|| failure("program/dispatch: { lolo: \"finish\" } names the leg begin opened"))?;
                let (outcomes, seed_trait, event) = host.finish_plain(leg).map_err(failure)?;
                let embedded = session.route.read().embedded.clone();
                let orbitals = painter_orbitals(&session.kernel, &outcomes);
                Ok(guest_answer(&session.kernel, &orbitals, None, None, &embedded, &outcomes, &seed_trait, &event, None, &[]))
            }
            Some("abandon") => abandon_leg(host.as_ref(), options),
            other => Err(failure(format!("program/dispatch: a plain leg's lolo is \"begin\", \"finish\" or \"abandon\", not {other:?}"))),
        }
    }

    /// `program/dispatch { ask, answer | refuse } { guest }`: return a host's answer to a client ask the guest made, as a
    /// browser host returns it (EXEC-DIST-13): the ask's success or failure route fires on the trait that asked.
    /// `refuse` is an opener with no viewer to ask: the failure route fires with kind `unavailable`.
    fn answer_guest_ask(
        &self,
        session: &GuestSession,
        handle: &Value,
        form: &ValueMap,
        args: &[Value],
    ) -> Result<Value, AgentPortFailure> {
        use orbital_core_base::host_ask::{answer, route, RelayedAnswer, RelayedAsk};
        let text = |name: &str| form.get(name).and_then(Value::as_str);
        let ask: RelayedAsk = text("ask")
            .ok_or_else(|| failure("program/dispatch: ask is the text a guest answer carried"))
            .and_then(|t| {
                from_json_str(t).map_err(|e| {
                    failure(format!(
                        "program/dispatch: the ask is not one a guest wrote: {e}"
                    ))
                })
            })?;
        let outcome = match (text("answer"), text("refuse")) {
            (Some(given), None) => answer(given).map_err(failure)?,
            (None, Some(reason)) => RelayedAnswer::Refused {
                error: reason.to_string(),
                kind: "unavailable".to_string(),
            },
            _ => {
                return Err(failure(
                    "program/dispatch: an ask is returned with exactly one of answer or refuse",
                ))
            }
        };
        if ask
            .completion
            .as_ref()
            .is_some_and(|c| c.entity_id.is_some())
        {
            return Err(failure("program/dispatch: the ask was made on a row frame, which a guest's viewer cannot return to yet"));
        }
        let Some((event, payload)) = route(&ask, &outcome) else {
            return Ok(guest_answer(
                &session.kernel,
                &std::collections::HashMap::new(),
                None,
                None,
                &session.route.read().embedded.clone(),
                &[],
                "",
                "",
                None,
                &[],
            ));
        };
        let target = ask.completion.map(|c| c.trait_name).unwrap_or_default();
        let payload = self.copy_into(session, &payload)?;
        self.run_in_guest(session, handle, vec![target], &event, payload, None, args)
    }

    /// Dispatch `event` to `targets` in the guest as its client would, on the lane the call's key decides.
    fn run_in_guest(
        &self,
        session: &GuestSession,
        handle: &Value,
        targets: Vec<String>,
        event: &str,
        payload: Value,
        leg: Option<Value>,
        args: &[Value],
    ) -> Result<Value, AgentPortFailure> {
        let guest = &session.kernel;
        let keyed = self.io.keyed_call();
        let reply = guest_reply(args, &payload, keyed)?;
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
                    leg,
                    reply,
                    args.to_vec(),
                )
                .map(|answer| answer.unwrap_or(Value::Null))
                .map_err(|refused| AgentPortFailure {
                    error: refused.message,
                    errors: refused.errors,
                });
        }
        if let Some(leg) = &leg {
            return serve_leg(session, &payload, leg);
        }
        let cost = match args.get(2) {
            Some(Value::Object(options)) => match options.get("cost") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(cost)) => *cost,
                Some(_) => return Err(failure("program/dispatch: options.cost must be a boolean")),
            },
            _ => false,
        };
        let _cost = cost.then(RuntimeKernel::cost_scope);
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        for trait_name in &targets {
            let ran = match &session.host {
                Some(host) => host.dispatch(trait_name, event, payload.clone()),
                None => guest
                    .dispatch(trait_name, None, event, payload.clone())
                    .map_err(|e| e.to_string()),
            }
            .map_err(|e| failure(format!("program/dispatch: {e}")))?;
            outcomes.extend(ran);
        }
        let seed_trait = targets.first().cloned().unwrap_or_default();
        let embedded = session.route.read().embedded.clone();
        let orbitals = painter_orbitals(guest, &outcomes);
        Ok(guest_answer(
            &session.kernel,
            &orbitals,
            None,
            None,
            &embedded,
            &outcomes,
            &seed_trait,
            event,
            None,
            &[],
        ))
    }

    /// `program/tick { guest }`: one tick pass of a guest with no host of its own, on the asking step's clock: each tick
    /// its clock finds due and its kernel runs ([`RuntimeKernel::tick_runs`]) runs over its rows as a host's tick does,
    /// its asks failing in place. The first pass only starts the clock, as a host's does. A hosted guest's ticks are
    /// its host's (`program/live { kind: "guest" }`), so it is refused.
    pub(super) fn tick_guest(&self, handle: &Value) -> Result<Value, AgentPortFailure> {
        let session = self.guest(handle, "program/tick")?;
        if session.host.is_some() {
            return Err(failure(
                "program/tick: a hosted guest is ticked by its host (program/live { kind: \"guest\" }); { guest } ticks a guest with no host of its own",
            ));
        }
        let now_ms = (*self.step.dispatch_now.read()).unwrap_or_else(|| self.clock_now()).max(0.0) as u64;
        let due = session.ticks.lock().due(now_ms, crate::tick_clock::Calendar::local_now);
        let guest = &session.kernel;
        let mut ran = Vec::new();
        let mut outcomes: Vec<(String, ProcessEventResult)> = Vec::new();
        for (trait_name, tick) in due {
            if !guest.tick_runs(&trait_name, &tick) {
                continue;
            }
            // The rows this firing covers, listed before it runs as its run lists them, so a `.lolo` kernel fans out over the same rows.
            let listed = guest
                .entity_source
                .list(&guest.tick_entity_type(&trait_name))
                .map_err(|error| failure(format!("program/tick: listing the rows tick {tick} of {trait_name} covers: {error}")))?;
            let rows = guest
                .tick_rows(&trait_name, super::RUNTIME_SINGLETON, &listed)
                .into_iter()
                .map(|row| Value::string(row.entity_id.unwrap_or_default()))
                .collect::<Vec<_>>();
            match guest.start_dispatch_tick(&trait_name, None, &tick) {
                Ok(super::resumable::Progress::Done(done)) => outcomes.extend(done),
                Ok(super::resumable::Progress::Await { .. }) => {
                    return Err(failure(format!(
                        "program/tick: tick {tick} of {trait_name} asked a host, and this guest has none"
                    )))
                }
                Err(error) => return Err(failure(format!("program/tick: tick {tick} of {trait_name}: {error}"))),
            }
            ran.push(Value::object(ValueMap::from_iter([
                ("trait".to_string().into(), Value::string(trait_name)),
                ("tick".to_string().into(), Value::string(tick)),
                ("rows".to_string().into(), Value::array(rows)),
            ])));
        }
        let embedded = session.route.read().embedded.clone();
        let orbitals = painter_orbitals(guest, &outcomes);
        let seed_trait = outcomes.first().map(|(t, _)| t.clone()).unwrap_or_default();
        let mut answer = guest_answer(guest, &orbitals, None, None, &embedded, &outcomes, &seed_trait, "", None, &[]);
        if let Value::Object(fields) = &mut answer {
            fields.insert("ran".into(), Value::array(ran));
        }
        Ok(answer)
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

    /// `program/self { guest }`: the program the guest was opened from, as its kernel registered it, so an opener reads
    /// it by handle instead of holding a copy.
    pub(super) fn program_of_guest(&self, handle: &Value) -> Result<Value, AgentPortFailure> {
        let programs = self.guest(handle, "program/self")?.kernel.registered_programs();
        match programs.as_slice() {
            [program] => orbital_core_base::value_serialize::to_value(program.as_ref())
                .map_err(|e| e.to_string())
                .map_err(|e| AgentPortFailure::plain(format!("program/self: the guest's program does not serialize: {e}"))),
            other => Err(AgentPortFailure::plain(format!("program/self: the guest holds {} programs, not one", other.len()))),
        }
    }

    /// `program/inputs { guest, … }`: the guest's own inputs and runtime reads.
    pub(super) fn inputs_of_guest(
        &self,
        args: &[Value],
        handle: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let session = self.guest(handle, "program/inputs")?;
        let guest = session.kernel.clone();
        let asked = |key: &str| match args.first() {
            Some(Value::Object(map)) => map
                .get(key)
                .filter(|value| !matches!(value, Value::Null))
                .cloned(),
            _ => None,
        };
        let (environment, credentials, denials) =
            (asked("environment"), asked("credentials"), asked("denials"));
        let own: Vec<Value> = args
            .iter()
            .enumerate()
            .map(|(at, arg)| match arg {
                Value::Object(map) if at == 0 => {
                    let mut rest = map.clone();
                    rest.shift_remove("guest");
                    rest.shift_remove("environment");
                    rest.shift_remove("credentials");
                    rest.shift_remove("denials");
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
        let mut answer = guest.with_runtime_reads(reflection, &own)?;
        if let Some(asked) = environment {
            answer = self.with_guest_environment(answer, &session, &asked)?;
        }
        if let Some(asked) = credentials {
            answer = self.with_guest_credentials(answer, &session, &asked)?;
        }
        match denials {
            None | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => answer = self.with_guest_denials(answer, &session)?,
            Some(_) => return Err(failure("program/inputs: `denials` is true or false")),
        }
        Ok(answer)
    }

    /// The opener may read this guest's declared values, and the guest has a host to read them from.
    fn declared_reader<'a>(
        &self,
        session: &'a GuestSession,
    ) -> Result<&'a Arc<dyn super::guest_hosting::GuestHost>, AgentPortFailure> {
        let held = self.may_spawn_guest("program/inputs")?;
        if !held.kernel_allows(KernelKind::Declared, None) {
            return Err(guest_refusal(
                orbital_core_base::os::OsErrorKind::Denied,
                "program/inputs: reading a guest's environment, credentials or denials needs the grant kernel: { guest: [declared] }".to_string(),
            ));
        }
        session
            .host
            .as_ref()
            .ok_or_else(|| failure("program/inputs: this guest has no host to hold them"))
    }

    /// `answer` with the credentials the guest's program names: each masked, or the value of the one named.
    fn with_guest_credentials(
        &self,
        answer: Value,
        session: &GuestSession,
        asked: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let host = self.declared_reader(session)?;
        let mut declared = std::collections::BTreeSet::new();
        let mut computed = std::collections::BTreeSet::new();
        for schema in session.kernel.registry.schemas.read().values() {
            for orbital in &schema.orbitals {
                for reference in &orbital.traits {
                    let orbital_core_base::schema::types::TraitReference::Inline(definition) =
                        reference
                    else {
                        computed.insert(format!(
                            "{}.{}",
                            orbital.name,
                            reference.trait_name().unwrap_or_default()
                        ));
                        continue;
                    };
                    let value = serde_json::to_value(definition)
                        .map_err(|error| failure(format!("program/inputs: {error}")))?;
                    let (named, slots) =
                        orbital_core_base::secrets::declared_credentials_in(&value);
                    declared.extend(named);
                    computed.extend(
                        slots
                            .into_iter()
                            .map(|slot| format!("{}.{}", orbital.name, slot)),
                    );
                }
            }
        }
        let credentials = match asked {
            Value::Bool(true) => Value::array(
                declared
                    .iter()
                    .map(|name| masked_entry(name, host.credential_of(name)))
                    .collect::<Vec<Value>>(),
            ),
            Value::String(name) if declared.contains(name.as_str()) => record(vec![
                ("name", Value::string(name.to_string())),
                (
                    "value",
                    host.credential_of(name).map_or(Value::Null, Value::string),
                ),
            ]),
            Value::String(name) => {
                return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    format!(
                        "program/inputs: the guest's program does not name the credential {}",
                        name.as_str()
                    ),
                ))
            }
            _ => {
                return Err(failure(
                    "program/inputs: `credentials` is true or a credential's name",
                ))
            }
        };
        let mut fields = fields_of(answer);
        fields.insert("credentials".into(), credentials);
        fields.insert(
            "computedCredentials".into(),
            Value::array(
                computed
                    .into_iter()
                    .map(Value::string)
                    .collect::<Vec<Value>>(),
            ),
        );
        Ok(Value::object(fields))
    }

    /// `answer` with every `denied` answer the guest was given, the ones its program handled included.
    fn with_guest_denials(
        &self,
        answer: Value,
        session: &GuestSession,
    ) -> Result<Value, AgentPortFailure> {
        let host = self.declared_reader(session)?;
        let mut merged = session.kernel.grants.denials.entries();
        for (message, count) in host.deferred_denials() {
            match merged.iter_mut().find(|(seen, _)| *seen == message) {
                Some((_, total)) => *total += count,
                None => merged.push((message, count)),
            }
        }
        let denials = merged
            .into_iter()
            .map(|(message, count)| {
                record(vec![
                    ("message", Value::string(message)),
                    ("count", Value::Number(count as f64)),
                ])
            })
            .collect::<Vec<Value>>();
        let mut fields = fields_of(answer);
        fields.insert("denials".into(), Value::array(denials));
        Ok(Value::object(fields))
    }

    /// `answer` with what the guest really holds (`grants`) and its environment: every declared variable masked, or the
    /// value of the one named. Only an opener holding `kernel: { guest: [declared] }` may read it.
    fn with_guest_environment(
        &self,
        answer: Value,
        session: &GuestSession,
        asked: &Value,
    ) -> Result<Value, AgentPortFailure> {
        let host = self.declared_reader(session)?;
        let declared = session.kernel.grants.os_grants.read().declared();
        let environment =
            match asked {
                Value::Bool(true) => Value::array(
                    declared
                        .env
                        .iter()
                        .map(|name| masked_entry(name, host.environment_of(name)))
                        .collect::<Vec<Value>>(),
                ),
                Value::String(name) if declared.env.iter().any(|n| n.as_str() == name.as_str()) => {
                    record(vec![
                        ("name", Value::string(name.to_string())),
                        (
                            "value",
                            host.environment_of(name).map_or(Value::Null, Value::string),
                        ),
                    ])
                }
                Value::String(name) => return Err(guest_refusal(
                    orbital_core_base::os::OsErrorKind::Denied,
                    format!(
                        "program/inputs: the guest does not declare the environment variable {}",
                        name.as_str()
                    ),
                )),
                _ => {
                    return Err(failure(
                        "program/inputs: `environment` is true or a variable's name",
                    ))
                }
            };
        let grants: Value = serde_json::to_value(&declared)
            .ok()
            .and_then(|json| from_json_value(&json).ok())
            .unwrap_or(Value::Null);
        let mut fields = fields_of(answer);
        fields.insert("environment".into(), environment);
        fields.insert("grants".into(), grants);
        Ok(Value::object(fields))
    }
}

fn fields_of(answer: Value) -> ValueMap {
    match answer {
        Value::Object(map) => (*map).clone(),
        _ => ValueMap::default(),
    }
}

fn record(pairs: Vec<(&str, Value)>) -> Value {
    let mut fields = ValueMap::default();
    for (key, value) in pairs {
        fields.insert(key.into(), value);
    }
    Value::object(fields)
}

/// `{ name, set, tail }`: whether `value` is held and, past 8 characters, its last 4.
fn masked_entry(name: &str, value: Option<String>) -> Value {
    let tail: String = match &value {
        Some(text) if text.chars().count() > 8 => {
            text.chars().skip(text.chars().count() - 4).collect()
        }
        _ => String::new(),
    };
    record(vec![
        ("name", Value::string(name.to_string())),
        ("set", Value::Bool(value.is_some())),
        ("tail", Value::string(tail)),
    ])
}

impl GuestSession {
    /// The traits the guest's page embeds, as its answers report them (`embedded` on each paint).
    pub(super) fn embedded_traits(&self) -> std::collections::HashSet<String> {
        self.route.read().embedded.clone()
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
            opener_session: None,
            split: false,
            arenas: Vec::new(),
            route: Arc::default(),
            lazy: Arc::new(GuestLazy {
                from: LazyFrom::None,
                prepare: GuestPrepare {
                    mock: false,
                    apply_grants: false,
                    keep_declared: false,
                    live: false,
                    opener_held: None,
                    granted: None,
                    withhold: Vec::new(),
                },
                read: orbital_core_base::sync::Mutex::new(Default::default()),
            }),
            viewer_ticked_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            ticks: Arc::new(orbital_core_base::sync::Mutex::new(crate::tick_clock::TickClock::from_programs(&[]))),
        }
    }
}

/// A step's cost as a guest answer carries it (EXEC-COST-1).
fn cost_record(cost: &orbital_core_base::cost::StepCost) -> Value {
    let mut fields = ValueMap::default();
    fields.insert("startNs".into(), Value::Number(cost.start_ns as f64));
    fields.insert("ns".into(), Value::Number(cost.ns as f64));
    fields.insert("allocs".into(), Value::Number(cost.allocs as f64));
    fields.insert("allocBytes".into(), Value::Number(cost.alloc_bytes as f64));
    Value::object(fields)
}

#[cfg(test)]
mod withhold_tests {
    use super::{withhold_grants, GuestWithhold};
    use orbital_core_base::json_value::from_json_value;
    use orbital_core_base::os_grants::OsGrants;
    use serde_json::json;

    fn grants(j: serde_json::Value) -> OsGrants {
        from_json_value(&j).expect("os grants")
    }

    #[test]
    fn withholding_listen_drops_only_the_listen_kind_and_keeps_other_net_targets() {
        let mut held = grants(
            json!({ "net": [{ "target": "0.0.0.0:*", "kinds": ["listen"] }, { "target": "api.example:443", "kinds": ["tcp"] }, { "target": "127.0.0.1:8080", "kinds": ["tcp", "listen"] }], "sys": ["self-signal"] }),
        );
        withhold_grants(&mut held, &[GuestWithhold::Listen]);
        assert_eq!(
            held,
            grants(
                json!({ "net": [{ "target": "api.example:443", "kinds": ["tcp"] }, { "target": "127.0.0.1:8080", "kinds": ["tcp"] }], "sys": ["self-signal"] })
            )
        );
    }

    #[test]
    fn withholding_self_signal_drops_only_that_sys_kind() {
        let mut held = grants(
            json!({ "sys": ["self-signal", "cpu"], "net": [{ "target": "0.0.0.0:*", "kinds": ["listen"] }] }),
        );
        withhold_grants(&mut held, &[GuestWithhold::SelfSignal]);
        assert_eq!(
            held,
            grants(
                json!({ "sys": ["cpu"], "net": [{ "target": "0.0.0.0:*", "kinds": ["listen"] }] })
            )
        );
    }

    #[test]
    fn control_withholding_nothing_keeps_every_grant() {
        let before = grants(
            json!({ "sys": ["self-signal"], "net": [{ "target": "0.0.0.0:*", "kinds": ["listen"] }] }),
        );
        let mut held = before.clone();
        withhold_grants(&mut held, &[]);
        assert_eq!(held, before);
    }
}
