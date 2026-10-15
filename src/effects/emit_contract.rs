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
    /// A fixed set of typed fields (`http`'s reply, failure and stream event),
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
    /// `persist`'s literal action keyword (`create`, `update`, `delete`, ...).
    PersistAction(&'a str),
    /// An `http` call site that carries `paginate`.
    HttpPaginate,
}

/// `persist delete` hands back the deleted row's id plus a tombstone flag —
/// never the row.
pub const PERSIST_DELETE_FIELDS: &[&str] = &["id", "deleted"];

/// `fetch` / `ref` wrap their rows: authors read `?data`, and `totalCount` is
/// the pre-pagination count so a paginating consumer needs no second call.
pub const FETCH_WRAPPER_FIELDS: &[&str] = &["data", "totalCount"];

/// The reply an `http` call hands back, as `last` carries it inside a paged payload.
const HTTP_REPLY_BODY_SHAPE: &[ShapeField] = &[
    field("status", "number"),
    field("headers", "object"),
    field("body", "string"),
];

/// `http` hands back the reply: its status, its lower-cased headers, its body and the call's key (`null` unkeyed).
pub const HTTP_REPLY_SHAPE: &[ShapeField] = &[
    field("status", "number"),
    field("headers", "object"),
    field("body", "string"),
    field("key", "string"),
];

/// `http` with `paginate` hands back the pages it walked, the collected items and the last reply.
pub const HTTP_PAGED_SHAPE: &[ShapeField] = &[
    field("pages", "number"),
    field("items", "array"),
    ShapeField { name: "last", field_type: "object", fields: HTTP_REPLY_BODY_SHAPE },
    field("key", "string"),
];

/// An `http` failure: the message, its kind (`timeout`, `failed`, `refused`, `unconfigured`), the credential a
/// missing one names (`null` for every other kind) and the call's key.
pub const HTTP_FAILURE_SHAPE: &[ShapeField] = &[
    field("error", "string"),
    field("kind", "string"),
    field("credential", "string"),
    field("key", "string"),
];

/// One `http` `stream` event handed to `emit.onMessage`.
pub const HTTP_STREAM_EVENT_SHAPE: &[ShapeField] = &[
    field("data", "string"),
    field("event", "string"),
    field("id", "string"),
    field("key", "string"),
];

/// `random-bytes` hands back the bytes as base64url without padding.
pub const RANDOM_BYTES_SHAPE: &[ShapeField] = &[field("value", "string")];

/// `credential-status` and `credential-forget` hand back whether the credential is connected.
pub const CREDENTIAL_SHAPE: &[ShapeField] = &[field("connected", "boolean")];

/// A host effect's failure: the message.
pub const HOST_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string")];

/// A keyed `call-service` failure: the message and the call's key.
pub const KEYED_CALL_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("key", "string")];

/// `persist batch` hands back a summary of the attempted operations.
pub const PERSIST_BATCH_FIELDS: &[&str] = &["operations", "completedCount", "totalCount"];

/// What `operator` emits on success at a call site.
///
/// `variant` carries what the call site adds to the operator's contract:
/// `persist`'s literal action keyword (the legacy 3-element form has no action
/// keyword; pass [`CallVariant::Plain`] and this returns
/// [`ProducedPayload::Opaque`] rather than guessing "create" -- a wrong guess
/// would report against the wrong contract), or an `http` call's `paginate`.
pub fn success_payload(operator: &str, variant: CallVariant) -> ProducedPayload {
    match operator {
        "persist" => match variant {
            CallVariant::PersistAction("create" | "upsert" | "update") => ProducedPayload::EntityRow,
            CallVariant::PersistAction("delete") => ProducedPayload::Fields(PERSIST_DELETE_FIELDS),
            CallVariant::PersistAction("batch") => ProducedPayload::Fields(PERSIST_BATCH_FIELDS),
            _ => ProducedPayload::Opaque,
        },
        // `ref` emits the same wrapper as `fetch` but fires it on `on_change`;
        // see `success_emit_key`.
        "fetch" | "ref" => ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
        "http" => match variant {
            CallVariant::HttpPaginate => ProducedPayload::Shape(HTTP_PAGED_SHAPE),
            _ => ProducedPayload::Shape(HTTP_REPLY_SHAPE),
        },
        "random-bytes" => ProducedPayload::Shape(RANDOM_BYTES_SHAPE),
        "credential-status" | "credential-forget" => ProducedPayload::Shape(CREDENTIAL_SHAPE),
        // `call-service` and `set` declare `any`; nothing to compare against.
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.failure`. `keyed` = the call site carries a
/// `key`: only a keyed `call-service` failure has a known shape (`{ error, key }`);
/// an unkeyed one stays opaque. `http` always echoes its key.
pub fn failure_payload(operator: &str, keyed: bool) -> ProducedPayload {
    match operator {
        "http" => ProducedPayload::Shape(HTTP_FAILURE_SHAPE),
        "random-bytes" | "credential-status" | "credential-forget" => ProducedPayload::Shape(HOST_FAILURE_SHAPE),
        "call-service" | "call_service" if keyed => ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE),
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.onMessage`: only an `http` `stream` has one.
pub fn message_payload(operator: &str) -> ProducedPayload {
    match operator {
        "http" => ProducedPayload::Shape(HTTP_STREAM_EVENT_SHAPE),
        _ => ProducedPayload::Opaque,
    }
}

/// The bus key an operator fires its success payload on.
///
/// `ref` uses `on_change` in both executors, so reading `emit.success` for it
/// inspects a key the runtime never fires.
pub fn success_emit_key(operator: &str) -> &'static str {
    match operator {
        "ref" => "on_change",
        _ => "success",
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

    #[test]
    fn host_effects_emit_their_closed_payloads() {
        assert_eq!(
            success_payload("random-bytes", CallVariant::Plain),
            ProducedPayload::Shape(RANDOM_BYTES_SHAPE)
        );
        for op in ["credential-status", "credential-forget"] {
            assert_eq!(success_payload(op, CallVariant::Plain), ProducedPayload::Shape(CREDENTIAL_SHAPE));
            assert_eq!(failure_payload(op, false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        }
        assert_eq!(failure_payload("random-bytes", false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        assert_eq!(
            undeclared_reads(&["value", "bytes"], &success_payload("random-bytes", CallVariant::Plain), &[]),
            vec!["bytes"]
        );
        assert_eq!(
            undeclared_reads(&["connected"], &success_payload("credential-status", CallVariant::Plain), &[]),
            Vec::<&str>::new()
        );
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
    fn fetch_and_ref_emit_the_data_wrapper_and_ref_uses_on_change() {
        assert_eq!(
            success_payload("fetch", CallVariant::Plain),
            ProducedPayload::Fields(FETCH_WRAPPER_FIELDS)
        );
        assert_eq!(success_emit_key("fetch"), "success");
        assert_eq!(success_emit_key("ref"), "on_change");
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
    fn http_emits_the_reply_and_a_paginating_call_emits_the_pages() {
        assert_eq!(
            success_payload("http", CallVariant::Plain),
            ProducedPayload::Shape(HTTP_REPLY_SHAPE)
        );
        assert_eq!(
            success_payload("http", CallVariant::HttpPaginate),
            ProducedPayload::Shape(HTTP_PAGED_SHAPE)
        );
        assert_eq!(success_emit_key("http"), "success");
        assert_eq!(
            undeclared_reads(&["status", "body"], &success_payload("http", CallVariant::Plain), &[]),
            Vec::<&str>::new()
        );
        assert_eq!(
            undeclared_reads(&["status"], &success_payload("http", CallVariant::HttpPaginate), &[]),
            vec!["status"],
            "control: a paged payload has no status at its root"
        );
    }

    #[test]
    fn a_paged_payload_types_its_last_reply() {
        let last = HTTP_PAGED_SHAPE.iter().find(|f| f.name == "last").unwrap();
        let names: Vec<&str> = last.fields.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["status", "headers", "body"]);
    }

    #[test]
    fn http_failure_and_stream_events_echo_the_key() {
        let failure = failure_payload("http", false);
        assert_eq!(failure, ProducedPayload::Shape(HTTP_FAILURE_SHAPE));
        assert!(undeclared_reads(&["error", "kind", "credential", "key"], &failure, &[]).is_empty());
        assert_eq!(undeclared_reads(&["status"], &failure, &[]), vec!["status"]);
        let event = message_payload("http");
        assert!(undeclared_reads(&["data", "event", "id", "key"], &event, &[]).is_empty());
        assert_eq!(undeclared_reads(&["error"], &event, &[]), vec!["error"]);
    }

    #[test]
    fn a_keyed_call_service_failure_is_error_and_key_and_an_unkeyed_one_is_opaque() {
        assert_eq!(
            failure_payload("call-service", true),
            ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE)
        );
        assert_eq!(failure_payload("call-service", false), ProducedPayload::Opaque);
        assert_eq!(message_payload("call-service"), ProducedPayload::Opaque);
        assert_eq!(failure_payload("fetch", true), ProducedPayload::Opaque);
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
