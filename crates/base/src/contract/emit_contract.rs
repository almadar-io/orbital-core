//! What an effect puts on the bus when its `emit.success` fires.
//!
//! **The payload invariant:** an event's declared payload type IS its access
//! path. Declare `Survey` and the payload is the row, read as `?name`; declare
//! `{ row : Survey }` and it is read as `?row.name`. Wrapping that is not in
//! the type is a lie, and a field in the type that is not in the payload reads
//! `undefined`. Reading FEWER fields than are emitted is fine, so the relation
//! is `declared ⊆ produced`.
//!
//! This module is the ONE place that answers "what does operator X emit?" for
//! both compiler layers. It lives in `orbital-core` because
//! `orbital-lolo` (L1) and `orbital-compiler` (L2) are siblings — L1 dev-depends
//! on L2, so it cannot reach the baked operator registry, and CLAUDE.md's rule
//! is that a check both layers need lives in the crate they share.
//!
//! The two layers see different amounts of the program, and the split is
//! deliberate:
//!
//! - **L1** knows one file. It can decide a call site whose entity is a literal
//!   in the same file, and it catches the common authoring mistake at parse
//!   time.
//! - **L2** knows the whole program post-inline, including `@rebindable` traits
//!   whose entity is only fixed once a consumer binds it. Those are inherently
//!   L2 and L1 stays silent on them.
//!
//! The contracts below are LANGUAGE invariants, not registry trivia, but the
//! registry (`canonical-operators.json`) states them too. To keep the two from
//! drifting, `orbital-compiler` carries a test that asserts this table agrees
//! with the baked registry — drift fails a build rather than waiting to be
//! noticed in a corpus sweep.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::language::registry::{get_operator, OperatorTypeRef, OperatorTypeRefKind};
use crate::os::OsOp;
use crate::sync::OnceLock;

/// The shape an operator's success emit carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducedPayload {
    /// The persisted row itself, FLAT — its own fields at the payload root.
    /// `persist create` / `persist update`: both executors emit the row with
    /// no wrapper (`EffectExecutor.ts`'s `successPayload = persisted ?? data`;
    /// `orbital-core`'s `do_persist` → `executed_with_value`).
    EntityRow,
    /// A fixed set of field names, e.g. `persist delete`'s `{ id, deleted }`
    /// or `fetch`'s `{ data, totalCount }` wrapper.
    Fields(&'static [&'static str]),
    /// A fixed set of typed fields (a host call's reply and failure),
    /// each possibly carrying nested typed fields.
    Shape(&'static [ShapeField]),
    /// Not decidable here — no contract, an opaque `any`, or a call site this
    /// layer cannot narrow. Callers MUST treat this as "do not check" rather
    /// than as "produces nothing".
    Opaque,
}

/// One typed field of a [`ProducedPayload::Shape`]. `field_type` speaks the
/// payload-declaration vocabulary (`string`, `number`, `object`, `array`);
/// `fields` types an object's members, empty when it has none to state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeField {
    pub name: &'static str,
    pub field_type: &'static str,
    pub fields: &'static [ShapeField],
}

const fn field(name: &'static str, field_type: &'static str) -> ShapeField {
    ShapeField { name, field_type, fields: &[] }
}

/// What distinguishes a call site within one operator's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVariant<'a> {
    /// The operator has one contract, or the call site carries no literal
    /// action (the legacy 3-element `persist`).
    Plain,
    /// A keyed stdin read with an onMessage route completes after streaming its chunks.
    StdinStream,
    /// `persist`'s literal action keyword (`create`, `update`, `delete`, ...).
    PersistAction(&'a str),
}

/// `persist delete` hands back the deleted row's id plus a tombstone flag —
/// never the row.
pub const PERSIST_DELETE_FIELDS: &[&str] = &["id", "deleted"];

/// `fetch` wraps its rows: authors read `?data`, and `totalCount` is
/// the pre-pagination count so a paginating consumer needs no second call.
pub const FETCH_WRAPPER_FIELDS: &[&str] = &["data", "totalCount"];

/// `browser/page-facts` hands back the page facts flat: whether there is a page, and each fact (a fact that was
/// not requested keeps its default and is not readable).
pub const PAGE_FACTS_SHAPE: &[ShapeField] = &[
    field("available", "boolean"),
    field("hostname", "string"),
    field("referrer", "string"),
    field("screenWidth", "number"),
];

/// `browser/tab-list` hands back the open tabs (`{ tab, url, title, active }` each) and the call's key.
pub const TAB_LIST_SHAPE: &[ShapeField] = &[field("tabs", "array"), field("key", "string")];

/// `browser/tab-open` hands back the new tab's number and the call's key.
pub const TAB_OPEN_SHAPE: &[ShapeField] = &[field("tab", "number"), field("key", "string")];

/// `browser/tab-close` hands back `closed` and the call's key.
pub const TAB_CLOSE_SHAPE: &[ShapeField] = &[field("closed", "boolean"), field("key", "string")];

/// `browser/tab-navigate` hands back the URL the tab was pointed at and the call's key.
pub const TAB_NAVIGATE_SHAPE: &[ShapeField] = &[field("url", "string"), field("key", "string")];

/// `browser/tab-query` hands back the tab and its elements (`{ elementId, fields }` each) and the call's key.
pub const TAB_QUERY_SHAPE: &[ShapeField] =
    &[field("tab", "number"), field("elements", "array"), field("key", "string")];

/// `browser/tab-capture` hands back the tab, the image format, the image as bytes, and the call's key.
pub const TAB_CAPTURE_SHAPE: &[ShapeField] =
    &[field("tab", "number"), field("format", "string"), field("image", "bytes"), field("key", "string")];

/// `browser/tab-watch` hands back the watch's id and the call's key.
pub const TAB_WATCH_SHAPE: &[ShapeField] = &[field("watchId", "string"), field("key", "string")];

/// `browser/tab-unwatch` hands back whether the watch was kept and the call's key.
pub const TAB_UNWATCH_SHAPE: &[ShapeField] = &[field("stopped", "boolean"), field("key", "string")];

/// `browser/tab-apply` hands back whether the element took the treatment, where, and the call's key.
pub const TAB_APPLY_SHAPE: &[ShapeField] = &[
    field("applied", "boolean"),
    field("tab", "number"),
    field("elementId", "string"),
    field("treatment", "string"),
    field("key", "string"),
];

/// `browser/tab-apply-many` hands back the treated and the missing elements (`{ tab, elementId }` each) and the key.
pub const TAB_APPLY_MANY_SHAPE: &[ShapeField] =
    &[field("applied", "array"), field("missing", "array"), field("key", "string")];

/// `browser/tab-act` hands back whether the interaction was done, where, and the call's key.
pub const TAB_ACT_SHAPE: &[ShapeField] = &[
    field("done", "boolean"),
    field("tab", "number"),
    field("elementId", "string"),
    field("key", "string"),
];

/// A `browser/tab-*` failure: the message, its kind (`invalid`: the request was refused before any ask; `unavailable`:
/// the host has no browser tabs; `not-ready`: the tab has no page script running; `not-found`: the element left the
/// page; `failed`) and the call's key (absent unkeyed).
pub const TAB_FAILURE_SHAPE: &[ShapeField] =
    &[field("error", "string"), field("kind", "string"), field("key", "string")];

/// The success shape of a `browser/tab-<op>` effect.
pub fn tab_success_shape(op: crate::tab::TabOp) -> &'static [ShapeField] {
    use crate::tab::TabOp;
    match op {
        TabOp::List => TAB_LIST_SHAPE,
        TabOp::Open => TAB_OPEN_SHAPE,
        TabOp::Close => TAB_CLOSE_SHAPE,
        TabOp::Navigate => TAB_NAVIGATE_SHAPE,
        TabOp::Query => TAB_QUERY_SHAPE,
        TabOp::Watch => TAB_WATCH_SHAPE,
        TabOp::Unwatch => TAB_UNWATCH_SHAPE,
        TabOp::Apply => TAB_APPLY_SHAPE,
        TabOp::ApplyMany => TAB_APPLY_MANY_SHAPE,
        TabOp::Act => TAB_ACT_SHAPE,
        TabOp::Capture => TAB_CAPTURE_SHAPE,
    }
}

/// `browser/prompt` hands back the on-device model's reply text (JSON satisfying the schema the call gave) and the
/// call's key (absent unkeyed).
pub const PROMPT_SHAPE: &[ShapeField] = &[field("reply", "string"), field("key", "string")];

/// `browser/embed` hands back one vector per text, in input order, and the call's key (absent unkeyed).
pub const EMBED_SHAPE: &[ShapeField] = &[field("embeddings", "array"), field("key", "string")];

/// A `browser/prompt` or `browser/embed` failure: the message, its kind (`unconfigured`: the host wires no model or
/// embedder; `unavailable`: it has one that cannot answer now; `failed`: it answered with an error) and the call's key.
pub const MODEL_FAILURE_SHAPE: &[ShapeField] =
    &[field("error", "string"), field("kind", "string"), field("key", "string")];

/// `random-bytes` hands back the bytes as base64url without padding; a keyed call adds its `key`.
pub const RANDOM_BYTES_SHAPE: &[ShapeField] = &[field("value", "string")];

/// `access-check` hands back the kernel's verdict on one access policy.
pub const ACCESS_CHECK_SHAPE: &[ShapeField] = &[field("allowed", "boolean")];

/// `credential-status` and `credential-forget` hand back whether the credential is connected and the non-secret account
/// label stored beside it (empty text when it has none, always empty after a forget).
pub const CREDENTIAL_SHAPE: &[ShapeField] = &[field("connected", "boolean"), field("label", "string")];

/// `credential-list`: the store's state, the integrations mode, each service's status and the masked inventory.
pub const CREDENTIAL_LIST_SHAPE: &[ShapeField] =
    &[field("enabled", "boolean"), field("mode", "string"), field("services", "array"), field("entries", "array")];

/// `credential-set`: the masked tail of what was stored.
pub const CREDENTIAL_SET_SHAPE: &[ShapeField] =
    &[field("saved", "boolean"), field("service", "string"), field("envVar", "string"), field("last4", "string")];

/// `credential-remove`: whether a stored row was deleted.
pub const CREDENTIAL_REMOVE_SHAPE: &[ShapeField] = &[field("removed", "boolean")];

/// `credential-test`: the presence check, never a live probe.
pub const CREDENTIAL_TEST_SHAPE: &[ShapeField] = &[
    field("service", "string"),
    field("configured", "boolean"),
    field("missing", "array"),
    field("probed", "boolean"),
    field("ok", "boolean"),
    field("message", "string"),
];

/// `credential-mode`: the mode in force after the switch.
pub const CREDENTIAL_MODE_SHAPE: &[ShapeField] = &[field("mode", "string")];

/// `credential-rotate`: how many rows were re-encrypted, were already current, or could not be read.
pub const CREDENTIAL_ROTATE_SHAPE: &[ShapeField] =
    &[field("rotated", "number"), field("alreadyCurrent", "number"), field("unreadable", "number")];

/// A credential administration failure: the message and its code (`AUTH_ERROR` for a refused role, `VALIDATION_ERROR`
/// for a request outside its contract; absent for any other cause).
pub const CREDENTIAL_ADMIN_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("code", "string")];

/// A host effect's failure: the message.
pub const HOST_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string")];

/// A keyed `call-service` failure: the message and the call's key.
pub const KEYED_CALL_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("key", "string")];

/// A `persist` failure: the message and the store's code (`duplicate-id`, `not-found`, `no-row-key`; absent for
/// any other cause, such as an access denial).
pub const PERSIST_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("code", "string")];

/// A `fetch` failure: the message and the code (`not-found` for a by-id miss; absent for any other cause, until
/// the read contract gains a typed refusal, `G-ORB-334`).
pub const FETCH_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("code", "string")];

/// An agent-port or program-host failure: the message, and a program's validator issues when it has any.
pub const SUBSTRATE_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("errors", "array")];

/// A keyed agent-port or program op's failure: the substrate failure with the call's `key` beside it.
pub const KEYED_SUBSTRATE_FAILURE_SHAPE: &[ShapeField] =
    &[field("error", "string"), field("errors", "array"), field("key", "string")];

/// A `persist batch` failure: the persist failure (message and the failing operation's store code, absent for a cause
/// that has none), the failing operation's position in the batch (absent when no single operation is at fault, such
/// as operations that do not evaluate) and the number of operations that completed before it (rolled back by a store
/// that runs the batch atomically).
pub const PERSIST_BATCH_FAILURE_SHAPE: &[ShapeField] = &[
    field("error", "string"),
    field("code", "string"),
    field("index", "number"),
    field("completedCount", "number"),
];

/// `persist batch` hands back a summary of the attempted operations.
pub const PERSIST_BATCH_FIELDS: &[&str] = &["operations", "completedCount", "totalCount"];

/// An `os/` failure: the message and its closed kind (`denied`, `not-found`, `exists`, `invalid`, `io`,
/// `timeout`, `unsupported`, `closed`).
pub const OS_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("kind", "string")];

/// What one `os/` or `kv/` operation emits, read from the operator table's `effect` (`produces`, `events`,
/// `unrouted`): its final answer on `emit.success`, each event of a stream on `emit.onMessage` and, for a stream op,
/// its answer when called with no `onMessage` route. A payload the table does not declare is opaque.
struct OsContract {
    success: ProducedPayload,
    event: ProducedPayload,
    unrouted: ProducedPayload,
}

fn table_payload(op: OsOp, declared: Option<&OperatorTypeRef>) -> ProducedPayload {
    let Some(declared) = declared else {
        return ProducedPayload::Opaque;
    };
    let OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields, .. }) = declared else {
        panic!("the operator table declares a payload for `{}` that is not an object", op.name());
    };
    let mut names: Vec<&String> = fields.keys().collect();
    names.sort_unstable();
    let shape: Vec<ShapeField> = names
        .into_iter()
        .map(|name| ShapeField {
            name: Box::leak(name.clone().into_boxed_str()),
            field_type: Box::leak(fields[name].kind_name().to_string().into_boxed_str()),
            fields: &[],
        })
        .collect();
    ProducedPayload::Shape(Box::leak(shape.into_boxed_slice()))
}

fn os_contract(op: OsOp) -> &'static OsContract {
    static CONTRACTS: OnceLock<Vec<OsContract>> = OnceLock::new();
    let contracts = CONTRACTS.get_or_init(|| {
        OsOp::ALL
            .iter()
            .map(|op| {
                let effect = get_operator(op.name()).and_then(|operator| operator.effect.as_ref());
                OsContract {
                    success: table_payload(*op, effect.and_then(|e| e.produces.as_ref())),
                    event: table_payload(*op, effect.and_then(|e| e.events.as_ref())),
                    unrouted: match effect.and_then(|e| e.unrouted.as_ref()) {
                        Some(unrouted) => table_payload(*op, Some(unrouted)),
                        None => table_payload(*op, effect.and_then(|e| e.produces.as_ref())),
                    },
                }
            })
            .collect()
    });
    let at = OsOp::ALL.iter().position(|candidate| *candidate == op).expect("every OsOp is in ALL");
    &contracts[at]
}

/// What `operator` emits on success at a call site.
///
/// `variant` carries what the call site adds to the operator's contract:
/// `persist`'s literal action keyword (the legacy 3-element form has no action
/// keyword; pass [`CallVariant::Plain`] and this returns
/// [`ProducedPayload::Opaque`] rather than guessing "create" -- a wrong guess
/// would report against the wrong contract).
pub fn success_payload(operator: &str, variant: CallVariant) -> ProducedPayload {
    if let Some(op) = OsOp::from_name(operator) {
        let contract = os_contract(op);
        return if variant == CallVariant::StdinStream { contract.success.clone() } else { contract.unrouted.clone() };
    }
    if let Some(op) = crate::tab::TabOp::from_name(operator) {
        return ProducedPayload::Shape(tab_success_shape(op));
    }
    match operator {
        "persist" => match variant {
            CallVariant::PersistAction("create" | "upsert" | "update") => ProducedPayload::EntityRow,
            CallVariant::PersistAction("delete") => ProducedPayload::Fields(PERSIST_DELETE_FIELDS),
            CallVariant::PersistAction("batch") => ProducedPayload::Fields(PERSIST_BATCH_FIELDS),
            _ => ProducedPayload::Opaque,
        },
        "fetch" => ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
        "browser/page-facts" => ProducedPayload::Shape(PAGE_FACTS_SHAPE),
        "browser/prompt" => ProducedPayload::Shape(PROMPT_SHAPE),
        "browser/embed" => ProducedPayload::Shape(EMBED_SHAPE),
        "random-bytes" => ProducedPayload::Shape(RANDOM_BYTES_SHAPE),
        "credential-status" | "credential-forget" => ProducedPayload::Shape(CREDENTIAL_SHAPE),
        "access-check" => ProducedPayload::Shape(ACCESS_CHECK_SHAPE),
        "credential-list" => ProducedPayload::Shape(CREDENTIAL_LIST_SHAPE),
        "credential-set" => ProducedPayload::Shape(CREDENTIAL_SET_SHAPE),
        "credential-remove" => ProducedPayload::Shape(CREDENTIAL_REMOVE_SHAPE),
        "credential-test" => ProducedPayload::Shape(CREDENTIAL_TEST_SHAPE),
        "credential-mode" => ProducedPayload::Shape(CREDENTIAL_MODE_SHAPE),
        "credential-rotate" => ProducedPayload::Shape(CREDENTIAL_ROTATE_SHAPE),
        // `call-service` and `set` declare `any`; nothing to compare against.
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.failure`. `keyed` = the call site carries a
/// `key`: only a keyed `call-service` failure has a known shape (`{ error, key }`);
/// an unkeyed one stays opaque.
pub fn failure_payload(operator: &str, variant: CallVariant, keyed: bool) -> ProducedPayload {
    if OsOp::from_name(operator).is_some() {
        return ProducedPayload::Shape(OS_FAILURE_SHAPE);
    }
    if crate::tab::TabOp::is_tab_effect(operator) {
        return ProducedPayload::Shape(TAB_FAILURE_SHAPE);
    }
    match operator {
        "browser/prompt" | "browser/embed" => ProducedPayload::Shape(MODEL_FAILURE_SHAPE),
        "browser/page-facts" | "random-bytes" if keyed => ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE),
        "random-bytes" | "credential-status" | "credential-forget" | "access-check" | "browser/page-facts" => {
            ProducedPayload::Shape(HOST_FAILURE_SHAPE)
        }
        "credential-list" | "credential-set" | "credential-remove" | "credential-test" | "credential-mode"
        | "credential-rotate" => ProducedPayload::Shape(CREDENTIAL_ADMIN_FAILURE_SHAPE),
        "persist" => match variant {
            CallVariant::PersistAction("batch") => ProducedPayload::Shape(PERSIST_BATCH_FAILURE_SHAPE),
            _ => ProducedPayload::Shape(PERSIST_FAILURE_SHAPE),
        },
        "fetch" => ProducedPayload::Shape(FETCH_FAILURE_SHAPE),
        _ if crate::agent_ports::is_agent_port_operator(operator)
            || crate::language::registry::is_program_host_effect(operator) =>
        {
            ProducedPayload::Shape(if keyed { KEYED_SUBSTRATE_FAILURE_SHAPE } else { SUBSTRATE_FAILURE_SHAPE })
        }
        "call-service" if keyed => ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE),
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.onMessage`: each event of a streaming `os/` op.
pub fn message_payload(operator: &str) -> ProducedPayload {
    if let Some(op) = OsOp::from_name(operator) {
        return os_contract(op).event.clone();
    }
    match operator {
        _ => ProducedPayload::Opaque,
    }
}

/// The declared field names that the produced payload does NOT carry — i.e.
/// every field whose read would be `undefined` at runtime.
///
/// This is the whole rule, and it is width subtyping on purpose: declaring
/// FEWER fields than are emitted is legal (you only read what you declared),
/// so only the declared side is walked. Equality would reject the many corpus
/// fetch events that legitimately omit `totalCount`.
///
/// `entity_field_names` supplies the row's own fields for
/// [`ProducedPayload::EntityRow`]; pass an empty slice when the entity cannot
/// be resolved, which yields no findings rather than false ones.
pub fn undeclared_reads<'a>(
    declared_field_names: &[&'a str],
    produced: &ProducedPayload,
    entity_field_names: &[&str],
) -> Vec<&'a str> {
    let shape_names: Vec<&str>;
    let produced_names: &[&str] = match produced {
        ProducedPayload::Opaque => return Vec::new(),
        ProducedPayload::Fields(f) => f,
        ProducedPayload::Shape(fields) => {
            shape_names = fields.iter().map(|f| f.name).collect();
            &shape_names
        }
        ProducedPayload::EntityRow => {
            if entity_field_names.is_empty() {
                return Vec::new();
            }
            entity_field_names
        }
    };
    declared_field_names
        .iter()
        .copied()
        .filter(|d| !produced_names.contains(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Pairs(&'static [(&'static str, &'static str)]);

    fn pairs(payload: ProducedPayload) -> Pairs {
        let ProducedPayload::Shape(shape) = payload else { panic!("a shape: {payload:?}") };
        Pairs(Box::leak(shape.iter().map(|f| (f.name, f.field_type)).collect::<Vec<_>>().into_boxed_slice()))
    }

    #[test]
    fn os_binary_reads_declare_bytes_while_text_facts_stay_strings() {
        for operator in ["os/read-file", "os/read", "os/recv", "os/recvfrom"] {
            let ProducedPayload::Shape(shape) = success_payload(operator, CallVariant::Plain) else {
                panic!("{operator} must declare its payload");
            };
            assert_eq!(shape.iter().find(|f| f.name == "data").unwrap().field_type, "bytes", "{operator}");
        }
        let ProducedPayload::Shape(shape) = message_payload("os/stdin-read") else {
            panic!("stdin chunks must declare their payload");
        };
        assert_eq!(pairs(ProducedPayload::Shape(shape)), Pairs(&[("data", "bytes"), ("key", "string")]));
        assert_eq!(pairs(success_payload("os/getenv", CallVariant::Plain)), Pairs(&[("key", "string"), ("value", "string")]));
    }

    #[test]
    fn device_calls_declare_an_int_register_value_and_an_irq_line() {
        assert_eq!(pairs(success_payload("os/mmio-read", CallVariant::Plain)), Pairs(&[("key", "string"), ("value", "int")]));
        assert_eq!(pairs(success_payload("os/irq-wait", CallVariant::Plain)), Pairs(&[("irq", "int"), ("key", "string")]));
        for operator in ["os/mmio-write", "os/irq-ack"] {
            assert_eq!(pairs(success_payload(operator, CallVariant::Plain)), Pairs(&[("key", "string")]), "{operator}");
        }
        assert_eq!(message_payload("os/irq-wait"), ProducedPayload::Opaque, "control: a single answer has no stream event");
    }

    #[test]
    fn stdin_success_is_narrowed_between_single_read_and_stream_end() {
        assert_eq!(pairs(success_payload("os/stdin-read", CallVariant::Plain)), pairs(success_payload("os/read", CallVariant::Plain)));
        assert_eq!(pairs(success_payload("os/stdin-read", CallVariant::StdinStream)), Pairs(&[("eof", "boolean"), ("key", "string")]));
        assert_eq!(pairs(message_payload("os/stdin-read")), Pairs(&[("data", "bytes"), ("key", "string")]));
    }

    #[test]
    fn every_tab_effect_emits_its_shape_and_the_one_typed_failure() {
        for op in crate::tab::TabOp::ALL {
            let shape = tab_success_shape(op);
            assert_eq!(success_payload(op.name(), CallVariant::Plain), ProducedPayload::Shape(shape), "{}", op.name());
            assert!(shape.iter().any(|f| f.name == "key"), "{} echoes the key", op.name());
            for keyed in [false, true] {
                assert_eq!(failure_payload(op.name(), CallVariant::Plain, keyed), ProducedPayload::Shape(TAB_FAILURE_SHAPE));
            }
        }
        assert_eq!(undeclared_reads(&["tab", "elements", "key"], &success_payload("browser/tab-query", CallVariant::Plain), &[]), Vec::<&str>::new());
        assert_eq!(undeclared_reads(&["tabs"], &success_payload("browser/tab-query", CallVariant::Plain), &[]), vec!["tabs"]);
        assert_eq!(undeclared_reads(&["tab", "format", "image", "key"], &success_payload("browser/tab-capture", CallVariant::Plain), &[]), Vec::<&str>::new());
        assert_eq!(undeclared_reads(&["elements"], &success_payload("browser/tab-capture", CallVariant::Plain), &[]), vec!["elements"]);
        assert_eq!(success_payload("browser/tab-nope", CallVariant::Plain), ProducedPayload::Opaque);
    }

    #[test]
    fn keyed_socket_answers_and_accepted_connections_declare_the_call_key() {
        for (operator, event) in [("os/send", false), ("os/recv", false), ("os/tcp-listen", false), ("os/tcp-listen", true)] {
            let payload = if event { message_payload(operator) } else { success_payload(operator, CallVariant::Plain) };
            let ProducedPayload::Shape(shape) = payload else { panic!("{operator} declares its payload") };
            assert!(shape.iter().any(|f| f.name == "key" && f.field_type == "string"), "{operator} event={event}");
        }
        assert_eq!(message_payload("os/send"), ProducedPayload::Opaque, "control: a single answer has no event");
    }

    #[test]
    fn a_file_event_carries_its_instant_as_text() {
        let ProducedPayload::Shape(shape) = message_payload("os/watch-files") else { panic!("file events declare a payload") };
        assert_eq!(shape.iter().find(|f| f.name == "timestamp").map(|f| f.field_type), Some("string"));
    }

    #[test]
    fn every_os_payload_is_the_one_the_table_declares() {
        for op in OsOp::ALL {
            let declared = get_operator(op.name()).and_then(|o| o.effect.as_ref()).and_then(|e| e.produces.as_ref());
            match (declared, success_payload(op.name(), CallVariant::StdinStream)) {
                (None, payload) => assert_eq!(payload, ProducedPayload::Opaque, "{}", op.name()),
                (Some(_), ProducedPayload::Shape(shape)) => assert!(shape.iter().any(|f| f.name == "key"), "{}", op.name()),
                (Some(_), other) => panic!("{}: {other:?}", op.name()),
            }
        }
    }

    #[test]
    fn current_exe_answers_a_path_like_cwd() {
        assert_eq!(pairs(success_payload("os/current-exe", CallVariant::Plain)), pairs(success_payload("os/cwd", CallVariant::Plain)));
        assert_eq!(undeclared_reads(&["path"], &success_payload("os/current-exe", CallVariant::Plain), &[]), Vec::<&str>::new());
        assert_eq!(undeclared_reads(&["exe"], &success_payload("os/current-exe", CallVariant::Plain), &[]), vec!["exe"]);
    }

    #[test]
    fn host_effects_emit_their_closed_payloads() {
        assert_eq!(
            success_payload("random-bytes", CallVariant::Plain),
            ProducedPayload::Shape(RANDOM_BYTES_SHAPE)
        );
        assert_eq!(success_payload("access-check", CallVariant::Plain), ProducedPayload::Shape(ACCESS_CHECK_SHAPE));
        assert_eq!(failure_payload("access-check", CallVariant::Plain, false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        for op in ["credential-status", "credential-forget"] {
            assert_eq!(success_payload(op, CallVariant::Plain), ProducedPayload::Shape(CREDENTIAL_SHAPE));
            assert_eq!(failure_payload(op, CallVariant::Plain, false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        }
        for (op, shape) in [
            ("credential-list", CREDENTIAL_LIST_SHAPE),
            ("credential-set", CREDENTIAL_SET_SHAPE),
            ("credential-remove", CREDENTIAL_REMOVE_SHAPE),
            ("credential-test", CREDENTIAL_TEST_SHAPE),
            ("credential-mode", CREDENTIAL_MODE_SHAPE),
            ("credential-rotate", CREDENTIAL_ROTATE_SHAPE),
        ] {
            assert_eq!(success_payload(op, CallVariant::Plain), ProducedPayload::Shape(shape), "{op}");
            assert_eq!(failure_payload(op, CallVariant::Plain, false), ProducedPayload::Shape(CREDENTIAL_ADMIN_FAILURE_SHAPE), "{op}");
        }
        assert_eq!(undeclared_reads(&["last4", "saved"], &success_payload("credential-set", CallVariant::Plain), &[]), Vec::<&str>::new());
        assert_eq!(undeclared_reads(&["value"], &success_payload("credential-set", CallVariant::Plain), &[]), vec!["value"]);
        assert_eq!(failure_payload("random-bytes", CallVariant::Plain, false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        assert_eq!(failure_payload("random-bytes", CallVariant::Plain, true), ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE));
        assert_eq!(
            undeclared_reads(&["value", "bytes"], &success_payload("random-bytes", CallVariant::Plain), &[]),
            vec!["bytes"]
        );
        assert_eq!(
            undeclared_reads(&["connected", "label"], &success_payload("credential-status", CallVariant::Plain), &[]),
            Vec::<&str>::new()
        );
        assert_eq!(
            undeclared_reads(&["connected", "value"], &success_payload("credential-status", CallVariant::Plain), &[]),
            vec!["value"],
            "the credential value is never a field of the status"
        );
    }

    #[test]
    fn on_device_model_effects_emit_their_closed_payloads() {
        assert_eq!(success_payload("browser/prompt", CallVariant::Plain), ProducedPayload::Shape(PROMPT_SHAPE));
        assert_eq!(success_payload("browser/embed", CallVariant::Plain), ProducedPayload::Shape(EMBED_SHAPE));
        for keyed in [false, true] {
            for op in ["browser/prompt", "browser/embed"] {
                assert_eq!(failure_payload(op, CallVariant::Plain, keyed), ProducedPayload::Shape(MODEL_FAILURE_SHAPE));
            }
        }
        assert!(undeclared_reads(&["reply", "key"], &success_payload("browser/prompt", CallVariant::Plain), &[]).is_empty());
        assert_eq!(undeclared_reads(&["embeddings", "reply"], &success_payload("browser/embed", CallVariant::Plain), &[]), vec!["reply"]);
        assert_eq!(undeclared_reads(&["kind", "status"], &failure_payload("browser/prompt", CallVariant::Plain, false), &[]), vec!["status"]);
    }

    #[test]
    fn persist_create_and_update_emit_the_row_flat() {
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("create")),
            ProducedPayload::EntityRow
        );
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("update")),
            ProducedPayload::EntityRow
        );
    }

    #[test]
    fn persist_delete_emits_a_tombstone_not_the_row() {
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("delete")),
            ProducedPayload::Fields(PERSIST_DELETE_FIELDS)
        );
    }

    /// The legacy 3-element persist form carries no action keyword. Guessing
    /// "create" there would compare against the wrong contract.
    #[test]
    fn an_actionless_persist_is_not_guessed_at() {
        assert_eq!(success_payload("persist", CallVariant::Plain), ProducedPayload::Opaque);
    }

    #[test]
    fn fetch_emits_the_data_wrapper() {
        assert_eq!(
            success_payload("fetch", CallVariant::Plain),
            ProducedPayload::Fields(FETCH_WRAPPER_FIELDS)
        );
    }

    /// The defect that started this: `persist create` emits the row flat, so a
    /// declared `row` wrapper names a field nothing emits.
    #[test]
    fn a_row_wrapper_on_a_created_entity_is_reported() {
        let found = undeclared_reads(
            &["row"],
            &ProducedPayload::EntityRow,
            &["id", "title", "content"],
        );
        assert_eq!(found, vec!["row"]);
    }

    /// Omission is legal — the rule is `declared ⊆ produced`.
    #[test]
    fn declaring_a_subset_is_legal() {
        assert!(undeclared_reads(
            &["data"],
            &ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
            &[]
        )
        .is_empty());
        assert!(
            undeclared_reads(&["id"], &ProducedPayload::EntityRow, &["id", "title"]).is_empty()
        );
    }

    /// An unresolvable entity must produce NO finding rather than claim every
    /// declared field is missing.
    #[test]
    fn an_unresolvable_entity_yields_no_findings() {
        assert!(undeclared_reads(&["anything"], &ProducedPayload::EntityRow, &[]).is_empty());
    }

    #[test]
    fn a_persist_failure_carries_the_message_and_the_store_code_and_nothing_else() {
        let failure = failure_payload("persist", CallVariant::Plain, false);
        assert_eq!(failure, ProducedPayload::Shape(PERSIST_FAILURE_SHAPE));
        assert!(undeclared_reads(&["error", "code"], &failure, &[]).is_empty());
        assert_eq!(undeclared_reads(&["entityType", "id"], &failure, &[]), vec!["entityType", "id"]);
    }

    #[test]
    fn a_persist_batch_failure_names_the_failing_operation() {
        let failure = failure_payload("persist", CallVariant::PersistAction("batch"), false);
        assert_eq!(failure, ProducedPayload::Shape(PERSIST_BATCH_FAILURE_SHAPE));
        assert!(undeclared_reads(&["error", "code", "index", "completedCount"], &failure, &[]).is_empty());
        assert_eq!(undeclared_reads(&["totalCount"], &failure, &[]), vec!["totalCount"]);
        assert_eq!(
            failure_payload("persist", CallVariant::PersistAction("update"), false),
            ProducedPayload::Shape(PERSIST_FAILURE_SHAPE),
            "control: a single persist keeps the two-field failure"
        );
        assert_eq!(undeclared_reads(&["index"], &failure_payload("persist", CallVariant::Plain, false), &[]), vec!["index"]);
    }

    #[test]
    fn a_fetch_failure_carries_no_payload_and_an_agent_port_failure_carries_error_and_errors() {
        let fetch = failure_payload("fetch", CallVariant::Plain, false);
        assert_eq!(fetch, ProducedPayload::Shape(FETCH_FAILURE_SHAPE));
        assert_eq!(undeclared_reads(&["message", "requestId"], &fetch, &[]), vec!["message", "requestId"]);
        assert!(undeclared_reads(&["error", "code"], &fetch, &[]).is_empty());
        for op in crate::agent_ports::AGENT_PORT_OPERATORS.iter().copied().chain(["program/eval", "program/validate"]) {
            let failure = failure_payload(op, CallVariant::Plain, false);
            assert_eq!(failure, ProducedPayload::Shape(SUBSTRATE_FAILURE_SHAPE), "{op}");
            assert!(undeclared_reads(&["error", "errors"], &failure, &[]).is_empty());
            assert_eq!(undeclared_reads(&["message"], &failure, &[]), vec!["message"]);
        }
    }

    #[test]
    fn a_keyed_agent_port_or_program_failure_echoes_its_key() {
        for op in crate::agent_ports::AGENT_PORT_OPERATORS.iter().copied().chain(["program/eval", "program/validate"]) {
            let keyed = failure_payload(op, CallVariant::Plain, true);
            assert_eq!(keyed, ProducedPayload::Shape(KEYED_SUBSTRATE_FAILURE_SHAPE), "{op}");
            assert!(undeclared_reads(&["error", "errors", "key"], &keyed, &[]).is_empty(), "{op}");
            assert_eq!(
                undeclared_reads(&["key"], &failure_payload(op, CallVariant::Plain, false), &[]),
                vec!["key"],
                "control: an unkeyed {op} failure carries no key"
            );
        }
    }

    #[test]
    fn a_keyed_call_service_failure_is_error_and_key_and_an_unkeyed_one_is_opaque() {
        assert_eq!(
            failure_payload("call-service", CallVariant::Plain, true),
            ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE)
        );
        assert_eq!(failure_payload("call-service", CallVariant::Plain, false), ProducedPayload::Opaque);
        assert_eq!(message_payload("call-service"), ProducedPayload::Opaque);
        assert_eq!(failure_payload("fetch", CallVariant::Plain, true), ProducedPayload::Shape(FETCH_FAILURE_SHAPE));
    }

    #[test]
    fn opaque_operators_are_never_checked() {
        assert_eq!(
            success_payload("call-service", CallVariant::Plain),
            ProducedPayload::Opaque
        );
        assert!(undeclared_reads(&["whatever"], &ProducedPayload::Opaque, &[]).is_empty());
    }

}
