//! `persist create` with a REQUIRED column missing must fail the write
//! (never a partial row) and route the declared `emit.failure`, the same
//! `EffectOutcome::Failed` shape a denied mutation already produces.
//!
//! "Required column": an entity field declaring `required: true`, excluding
//! `FRAMEWORK_STAMPED_FIELDS` (`id`/`createdAt`/`updatedAt`), any `@intrinsic`
//! field, and any field declaring a `default` — shared with `@almadar/runtime`'s
//! `missingRequiredFields`.

use orbital_core_base::value::ValueMap;
use orbital_core_base::effect_types::{Effect, EffectOutcome};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::object(m)
}

/// `content` is a plain required column. `priority` is required but declares
/// a `default` (the store fills it in). `workingCopy` is required but
/// `@intrinsic` (the trait's own frame, never a persisted column). Neither
/// of the latter two should ever be reported missing.
fn note_create_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Note",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "content", "type": "string", "required": true },
                    { "name": "priority", "type": "string", "required": true, "default": "normal" },
                    { "name": "workingCopy", "type": "string", "required": true, "intrinsic": true }
                ]
            },
            "traits": [{
                "name": "NoteManage",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [
                        { "name": "browsing", "isInitial": true },
                        { "name": "creating" }
                    ],
                    "transitions": [
                        { "from": "browsing", "to": "creating", "event": "CREATE",
                          "effects": [
                            ["persist", "create", "Note", "@payload.data",
                             {"emit": {"success": "NOTE_CREATED", "failure": "NOTE_CREATE_FAILED"}}]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// `content` is host-authored and required. `name` is `required: true` AND
/// carries `mergedFrom: "BrowseItem"` — the inline phase's rebind merge added
/// it from an imported atom's entity, so it must never gate `ChatMessage`'s
/// own `persist create` even though it is omitted.
fn chat_message_create_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "ChatMessage",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "content", "type": "string", "required": true },
                    { "name": "name", "type": "string", "required": true, "mergedFrom": "BrowseItem" }
                ]
            },
            "traits": [{
                "name": "ChatCompose",
                "linkedEntity": "ChatMessage",
                "stateMachine": {
                    "states": [
                        { "name": "browsing", "isInitial": true },
                        { "name": "creating" }
                    ],
                    "transitions": [
                        { "from": "browsing", "to": "creating", "event": "CREATE",
                          "effects": [
                            ["persist", "create", "ChatMessage", "@payload.data",
                             {"emit": {"success": "MESSAGE_CREATED", "failure": "MESSAGE_CREATE_FAILED"}}]
                          ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn kernel_with(schema: OrbitalSchema, cache: Arc<InMemoryEntityCache>) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn persist_result(effect_results: &[orbital_core_base::effect_types::EffectResult]) -> &orbital_core_base::effect_types::EffectResult {
    effect_results
        .iter()
        .find(|r| matches!(r.effect, Effect::Persist { .. }))
        .expect("the persist effect must be reported")
}

#[test]
fn persist_create_missing_required_field_fails_and_routes_emit_failure() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(note_create_schema(), cache.clone());

    // `content` omitted entirely.
    let result = kernel
        .process_event(
            "NoteManage",
            Some("singleton"),
            "CREATE",
            obj(&[("data", obj(&[]))]),
        )
        .unwrap();
    assert!(result.executed, "the transition must still fire");

    let persist = persist_result(&result.effect_results);
    assert!(
        matches!(persist.outcome, EffectOutcome::Failed),
        "a required-column-missing create must be Failed, never Executed: {persist:?}"
    );
    assert_eq!(
        persist.error.as_deref(),
        Some("persist create Note: required field(s) content missing"),
    );
    assert_eq!(persist.code, None, "a missing required column is not an access denial");

    let failure_emit = result.effect_results.iter().find(|r| {
        matches!(&r.effect, Effect::Emit { event, .. } if event == "NOTE_CREATE_FAILED")
    });
    assert!(
        failure_emit.is_some(),
        "the declared emit.failure must fire: {:?}",
        result.effect_results
    );

    assert!(
        cache.list("Note").unwrap().is_empty(),
        "no partial row may land in the store"
    );
}

#[test]
fn persist_create_with_all_required_fields_succeeds() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(note_create_schema(), cache.clone());

    let result = kernel
        .process_event(
            "NoteManage",
            Some("singleton"),
            "CREATE",
            obj(&[(
                "data",
                obj(&[("content", Value::string("hello"))]),
            )]),
        )
        .unwrap();
    assert!(result.executed);

    let persist = persist_result(&result.effect_results);
    assert!(
        matches!(persist.outcome, EffectOutcome::Executed),
        "a complete row must persist: {persist:?}"
    );

    let rows = cache.list("Note").unwrap();
    assert_eq!(rows.len(), 1, "the row must actually land in the store");
}

#[test]
fn persist_create_default_and_intrinsic_required_fields_are_never_reported_missing() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(note_create_schema(), cache.clone());

    // `priority` (has a `default`) and `workingCopy` (`@intrinsic`) are both
    // declared `required: true` and both omitted here — neither should block
    // the write, only `content` is a real required column.
    let result = kernel
        .process_event(
            "NoteManage",
            Some("singleton"),
            "CREATE",
            obj(&[(
                "data",
                obj(&[("content", Value::string("hello again"))]),
            )]),
        )
        .unwrap();

    let persist = persist_result(&result.effect_results);
    assert!(
        matches!(persist.outcome, EffectOutcome::Executed),
        "default-bearing and intrinsic required fields must never gate the write: {persist:?}"
    );
}

#[test]
fn persist_create_merged_from_required_field_is_never_reported_missing() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = kernel_with(chat_message_create_schema(), cache.clone());

    // `name` is `required: true` but carries `mergedFrom` — the field the
    // inline rebind merge added from BrowseItem, never authored by
    // ChatMessage's own writer. Omitted here; only `content` is supplied.
    let result = kernel
        .process_event(
            "ChatCompose",
            Some("singleton"),
            "CREATE",
            obj(&[(
                "data",
                obj(&[("content", Value::string("hi"))]),
            )]),
        )
        .unwrap();

    let persist = persist_result(&result.effect_results);
    assert!(
        matches!(persist.outcome, EffectOutcome::Executed),
        "a mergedFrom required field must never gate the host's own create: {persist:?}"
    );

    let rows = cache.list("ChatMessage").unwrap();
    assert_eq!(rows.len(), 1, "the row must actually land in the store");
}
