use super::*;
use orbital_core_base::value::ValueMap;
use orbital_core_base::events::bus::shared_bus;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn persist_prep_stays_pointer_sized() {
    let size = std::mem::size_of::<PersistPrep>();
    let maximum = 3 * std::mem::size_of::<usize>();
    assert!(
        size <= maximum,
        "PersistPrep uses {size} bytes; expected at most {maximum} bytes"
    );
}

#[derive(Default)]
struct RecordingService {
    calls: orbital_core_base::sync::Mutex<Vec<(String, Option<String>)>>,
    cancelled: orbital_core_base::sync::Mutex<Vec<String>>,
}

impl ServiceHandler for RecordingService {
    fn call(
        &self,
        service: &str,
        method: &str,
        _args: &Value,
        key: Option<&str>,
        _caller: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        self.calls
            .lock()
            .push((format!("{service}.{method}"), key.map(str::to_string)));
        Ok(Value::Null)
    }

    fn cancel(&self, key: &str) -> Result<(), String> {
        self.cancelled.lock().push(key.to_string());
        Ok(())
    }
}

#[test]
fn keyed_call_service_hands_its_key_to_the_service_handler() {
    let svc = Arc::new(RecordingService::default());
    let executor = EffectExecutor::server().with_service_handler(svc.clone());
    let effect = Effect::CallService {
        service: "llm".into(),
        method: "classifyMany".into(),
        args: Value::Null,
        key: Some("batch-1".into()),
        emit: None,
    };
    assert!(executor
        .execute(&effect, &BindingContext::new())
        .is_executed());
    assert_eq!(
        *svc.calls.lock(),
        vec![("llm.classifyMany".to_string(), Some("batch-1".to_string()))]
    );
}

#[test]
fn cancel_call_asks_the_service_handler_to_abort_that_key() {
    let svc = Arc::new(RecordingService::default());
    let executor = EffectExecutor::server().with_service_handler(svc.clone());
    let effect = Effect::CancelCall {
        key: "batch-1".into(),
    };
    assert!(executor
        .execute(&effect, &BindingContext::new())
        .is_executed());
    assert_eq!(*svc.cancelled.lock(), vec!["batch-1".to_string()]);
}

#[test]
fn cancel_call_on_a_list_asks_the_service_handler_to_abort_each_key_in_order() {
    let svc = Arc::new(RecordingService::default());
    let executor = EffectExecutor::server().with_service_handler(svc.clone());
    let effect = Effect::CancelCall {
        key: Value::array(vec!["a".into(), "b".into()]),
    };
    assert!(executor.execute(&effect, &BindingContext::new()).is_executed());
    assert_eq!(*svc.cancelled.lock(), vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn cancel_call_on_a_list_with_a_non_string_member_fails_before_cancelling_any() {
    let svc = Arc::new(RecordingService::default());
    let executor = EffectExecutor::server().with_service_handler(svc.clone());
    let effect = Effect::CancelCall {
        key: Value::array(vec!["a".into(), Value::Number(3.0)]),
    };
    let result = executor.execute(&effect, &BindingContext::new());
    assert!(!result.is_executed(), "{result:?}");
    assert!(svc.cancelled.lock().is_empty());
}

#[test]
fn cancel_call_without_a_service_handler_is_a_noop_on_a_server() {
    let executor = EffectExecutor::server();
    let effect = Effect::CancelCall { key: "k".into() };
    assert!(executor.execute(&effect, &BindingContext::new()).is_noop());
}

#[test]
fn a_client_delegates_cancel_call_instead_of_running_it() {
    let svc = Arc::new(RecordingService::default());
    let executor = EffectExecutor::client().with_service_handler(svc.clone());
    let effect = Effect::CancelCall { key: "k".into() };
    let result = executor.execute(&effect, &BindingContext::new());
    assert!(svc.cancelled.lock().is_empty());
    assert!(!result.is_executed(), "{result:?}");
}

#[test]
fn test_emit_effect() {
    let bus = shared_bus();
    let executor = EffectExecutor::server().with_event_bus(bus.clone());

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    bus.on("TEST_EVENT", move |_| {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    let effect = Effect::Emit {
        event: "TEST_EVENT".to_string(),
        payload: Value::Null,
    };

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_executed());
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn test_persist_server_only() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let server_executor = EffectExecutor::server().with_persistence(persistence.clone());
    let client_executor = EffectExecutor::client();

    let effect = Effect::Persist {
        operation: PersistOperation::Create,
        entity_type: "User".to_string(),
        data: Value::Object({
            let mut m = ValueMap::default();
            m.insert("name".to_string(), Value::string("John"));
            m.into()
        }),
        id: None,
        emit: None,
        guard: None,
    };

    // Server executes
    let server_result = server_executor.execute(&effect, &BindingContext::new());
    assert!(server_result.is_executed());

    // Client no-ops
    let client_result = client_executor.execute(&effect, &BindingContext::new());
    assert!(client_result.is_noop());
}

fn row_of(pairs: &[(&str, &str)]) -> Value {
    Value::Object({
        let mut m = ValueMap::default();
        for (k, v) in pairs {
            m.insert((*k).to_string(), Value::string((*v).to_string()));
        }
        m.into()
    })
}

fn persist_effect(operation: PersistOperation, data: Value) -> Effect {
    Effect::Persist {
        operation,
        entity_type: "Mix".to_string(),
        data,
        id: None,
        emit: None,
        guard: None,
    }
}

/// EXEC-STEP-8: a create on a taken id fails `duplicate-id` and changes nothing, on a store with no
/// conditional write; a create naming no id is minted one (control).
#[test]
fn test_persist_create_on_a_taken_id_fails_duplicate_id() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let executor = EffectExecutor::server().with_persistence(persistence.clone());
    let ctx = BindingContext::new();
    let first = persist_effect(
        PersistOperation::Create,
        row_of(&[("id", "mix"), ("title", "one")]),
    );
    assert!(executor.execute(&first, &ctx).is_executed());
    let again = persist_effect(
        PersistOperation::Create,
        row_of(&[("id", "mix"), ("title", "two")]),
    );
    let result = executor.execute(&again, &ctx);
    assert!(result.is_failed());
    assert_eq!(result.code, Some(ServerEffectCode::DuplicateId));
    let stored = persistence.read("Mix", "mix").unwrap();
    assert_eq!(stored.as_object().unwrap()["title"], Value::string("one"));
    let minted = persist_effect(PersistOperation::Create, row_of(&[("title", "free")]));
    assert!(executor.execute(&minted, &ctx).is_executed());
    assert!(executor.execute(&minted, &ctx).is_executed());
}

/// EXEC-STEP-8: an upsert creates the row, then replaces it whole keeping the stored `createdAt`.
#[test]
fn test_persist_upsert_creates_then_replaces_the_whole_row() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let executor = EffectExecutor::server().with_persistence(persistence.clone());
    let ctx = BindingContext::new();
    let created = persist_effect(
        PersistOperation::Upsert,
        row_of(&[("id", "mix"), ("title", "one"), ("tone", "warm"), ("createdAt", "2026-01-01")]),
    );
    assert!(executor.execute(&created, &ctx).is_executed());
    let replaced = persist_effect(
        PersistOperation::Upsert,
        row_of(&[("id", "mix"), ("title", "two"), ("createdAt", "2030-01-01")]),
    );
    let result = executor.execute(&replaced, &ctx);
    assert!(result.is_executed(), "{result:?}");
    let stored = persistence.read("Mix", "mix").unwrap();
    let fields = stored.as_object().unwrap();
    assert_eq!(fields["title"], Value::string("two"));
    assert!(!fields.contains_key("tone"), "replaced whole: {fields:?}");
    assert_eq!(fields["createdAt"], Value::string("2026-01-01"));
    let keyless = persist_effect(PersistOperation::Upsert, row_of(&[("title", "x")]));
    let failed = executor.execute(&keyless, &ctx);
    assert!(failed.is_failed());
    assert_eq!(failed.code, Some(ServerEffectCode::NoRowKey), "control: an upsert needs an id");
}

/// C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED, end to end: `(persist update
/// Entity ?data {emit:{...}})` — no separate id arg, the data carries its
/// own `id` field, and a trailing `{emit:...}` options object rides
/// along. Before the fix, `parse_persist` misread the emit config as
/// `data` and the real data token as `id`; the update landed on a
/// nonexistent row (or silently no-op'd). This proves the real row is
/// found and updated.
#[test]
fn test_persist_update_data_carries_its_own_id_with_trailing_emit_options() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let created = persistence
        .create(
            "Ticket",
            &Value::Object({
                let mut m = ValueMap::default();
                m.insert("subject".to_string(), Value::string("original"));
                m.into()
            }),
        )
        .unwrap();
    let ticket_id = created
        .as_object()
        .unwrap()
        .get("id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();

    let executor = EffectExecutor::server().with_persistence(persistence.clone());

    // Simulates the compiled effect array `["persist", "update", "Ticket",
    // "@payload.data", {"emit": {...}}]` after `parse_persist` — id is
    // None (no separate id arg), data carries its own `id` field.
    let effect = Effect::Persist {
        operation: PersistOperation::Update,
        entity_type: "Ticket".to_string(),
        data: Value::Object({
            let mut m = ValueMap::default();
            m.insert("id".to_string(), Value::string(ticket_id.clone()));
            m.insert("subject".to_string(), Value::string("edited"));
            m.into()
        }),
        id: None,
        emit: None,
        guard: None,
    };

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(
        result.is_executed(),
        "update must find the real row, not fail: {result:?}"
    );

    let updated = persistence.read("Ticket", &ticket_id).unwrap();
    assert_eq!(
        updated
            .as_object()
            .unwrap()
            .get("subject")
            .unwrap()
            .as_str(),
        Some("edited"),
        "the real row must have been updated, not a nonexistent placeholder id"
    );
}

/// C-PERSIST-DATA-CALL-EXPR-NEVER-EVALUATED: `(persist create Entity
/// (object/merge ?data {field: @user.id}) {...})` — the DOMINANT
/// server-side-enrichment idiom (`TicketReplyPersistor`, both
/// `TicketPersistor`/`ChatMessagePersistor` DO_CREATE arms). `data` here
/// is a CALL expression, not a plain object — `parse_persist` converts it
/// straight from `SExpression` to `Value` (a 3-element array literally
/// `["object/merge", "@payload.data", {field: "@user.id"}]`), and
/// `do_persist` only ran it through `resolve_value_bindings`, which
/// substitutes leaf `@`-sigils but never DISPATCHES a call — so the
/// merge itself never ran and the stored row was the raw call array,
/// not an object. Invisible until something read a field back out of
/// it (an access-policy check is the first thing that ever did).
/// Reproduced via the REAL parser (`parse_effect`), not a hand-built
/// `Effect::Persist{data: Value::Object(...)}` — those bypass the bug
/// entirely since they skip the call-expression shape.
#[test]
fn test_persist_create_data_call_expression_is_evaluated_not_left_literal() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let executor = EffectExecutor::server().with_persistence(persistence.clone());

    let raw = SExpression::Array(vec![
        SExpression::string("persist"),
        SExpression::string("create"),
        SExpression::string("ChatMessage"),
        SExpression::Array(vec![
            SExpression::string("object/merge"),
            SExpression::string("@payload.data"),
            SExpression::Object({
                let mut m = orbital_core_base::schema::sexpr::SObject::default();
                m.insert(
                    "sender".to_string(),
                    SExpression::string("@user.id"),
                );
                m
            }),
        ]),
    ]);
    let effect =
        orbital_core_base::effect_types::parse_effect(&raw).expect("parse_persist must recognize this shape");

    let mut payload_data = ValueMap::default();
    payload_data.insert("content".to_string(), Value::string("hello"));
    let mut payload = ValueMap::default();
    payload.insert("data".to_string(), Value::object(payload_data));
    let mut user = ValueMap::default();
    user.insert("id".to_string(), Value::string("user-42"));

    let ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
        .payload(Value::object(payload))
        .user(Value::object(user))
        .build();

    let result = executor.execute(&effect, &ctx);
    assert!(result.is_executed(), "create must execute: {result:?}");

    let all = persistence.list("ChatMessage").unwrap();
    assert_eq!(all.len(), 1, "exactly one row must have been created");
    let row = all[0]
        .as_object()
        .expect("the stored row must be a real object, not the raw call array");
    assert_eq!(
        row.get("content").and_then(|v| v.as_str()),
        Some("hello"),
        "the ?data payload's own fields must survive the merge"
    );
    assert_eq!(
        row.get("sender").and_then(|v| v.as_str()),
        Some("user-42"),
        "object/merge's stamped field must actually be evaluated, not left as a literal call array"
    );
}

/// B4-V3 (Rust twin): the JS runtime's persist handler used to fall
/// back to the submitted data when the store denied a write, so a
/// denied delete logged `persist:success` and fired the declared
/// success event for a row the store never touched. `do_persist` here
/// already routes a `check_mutation_access` denial through the SAME
/// `Err` arm as any other persist failure — `EffectResult::failed`, not
/// `executed_with_value` — so there is no row for a caller to
/// mistakenly treat as written. This pins that the kernel never had
/// the JS-side bug: a denied delete/update is `EffectOutcome::Failed`
/// with no `return_value`, never `Executed`.
#[test]
fn test_persist_denied_delete_is_failed_not_executed() {
    let persistence = Arc::new(InMemoryPersistence::new());
    persistence
        .create(
            "Note",
            &Value::Object({
                let mut m = ValueMap::default();
                m.insert("id".to_string(), Value::string("note-1"));
                m.insert(
                    "authorId".to_string(),
                    Value::string("author-1"),
                );
                m.into()
            }),
        )
        .unwrap();

    let mut policies = EntityAccessTable::new();
    policies.insert(
        "Note".to_string(),
        crate::entity_access::EntityAccessPolicies {
            delete: Some(Value::array(vec![
                Value::String(("=".to_string()).into()),
                Value::String(("@entity.authorId".to_string()).into()),
                Value::String(("@user.id".to_string()).into()),
            ])),
            ..Default::default()
        },
    );

    let executor = EffectExecutor::server()
        .with_persistence(persistence.clone())
        .with_entity_policies(Arc::new(RwLock::new(policies)));

    // The viewer is NOT the row's author — the declared `@delete`
    // policy must reject this row.
    let viewer = Value::Object({
        let mut m = ValueMap::default();
        m.insert("id".to_string(), Value::string("someone-else"));
        m.into()
    });
    let ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
        .user(viewer)
        .build();

    let effect = Effect::Persist {
        operation: PersistOperation::Delete,
        entity_type: "Note".to_string(),
        data: Value::Null,
        id: Some("note-1".to_string()),
        emit: None,
        guard: None,
    };

    let result = executor.execute(&effect, &ctx);
    assert!(
        result.is_failed(),
        "a denied delete must be Failed, never Executed: {result:?}"
    );
    assert!(result.return_value.is_none());
    assert_eq!(result.code, Some(ServerEffectCode::Denied));

    // The row must genuinely survive — the denial is real, not cosmetic.
    assert!(persistence.read("Note", "note-1").is_ok());
}

#[test]
fn test_render_ui_client_only() {
    let server_executor = EffectExecutor::server();
    let client_executor = EffectExecutor::client();

    let effect = Effect::RenderUi {
        slot: "main".to_string(),
        content: Value::object(ValueMap::default()),
    };

    // Server no-ops
    let server_result = server_executor.execute(&effect, &BindingContext::new());
    assert!(server_result.is_noop());

    // Client would execute if handler configured, otherwise no-ops
    let client_result = client_executor.execute(&effect, &BindingContext::new());
    assert!(client_result.is_noop()); // No handler configured
}

#[test]
fn test_navigate_client_only() {
    let server_executor = EffectExecutor::server();

    let effect = Effect::Navigate {
        path: "/users".to_string(),
        params: None,
        options: None,
    };

    let result = server_executor.execute(&effect, &BindingContext::new());
    assert!(result.is_noop());
}

#[test]
fn test_log_effect() {
    let executor = EffectExecutor::server();

    let effect = Effect::Log {
        level: LogLevel::Info,
        message: "Test message".to_string(),
        data: None,
    };

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_executed());
}

#[test]
fn test_in_memory_persistence() {
    let persistence = InMemoryPersistence::new();

    // Create
    let data = Value::Object({
        let mut m = ValueMap::default();
        m.insert("name".to_string(), Value::string("John"));
        m.into()
    });
    let created = persistence.create("User", &data).unwrap();
    let id = created
        .as_object()
        .unwrap()
        .get("id")
        .unwrap()
        .as_str()
        .unwrap();

    // Read
    let read = persistence.read("User", id).unwrap();
    assert_eq!(
        read.as_object().unwrap().get("name").unwrap(),
        &Value::string("John")
    );

    // Update
    let update_data = Value::Object({
        let mut m = ValueMap::default();
        m.insert("name".to_string(), Value::string("Jane"));
        m.into()
    });
    let updated = persistence.update("User", id, &update_data).unwrap();
    assert_eq!(
        updated.as_object().unwrap().get("name").unwrap(),
        &Value::string("Jane")
    );

    // Delete
    persistence.delete("User", id).unwrap();
    assert!(persistence.read("User", id).is_err());
}

#[test]
fn test_execute_all_from_sexpr() {
    let bus = shared_bus();
    let executor = EffectExecutor::server().with_event_bus(bus.clone());

    let effects: Vec<SExpression> = vec![
        serde_json::from_str(r#"["emit", "EVENT_1", null]"#).unwrap(),
        serde_json::from_str(r#"["log", "Test message", "info"]"#).unwrap(),
    ];

    let ctx = BindingContext::new();
    let results = executor.execute_all(&effects, &ctx);

    assert_eq!(results.len(), 2);
    assert!(results[0].is_executed());
    assert!(results[1].is_executed());
}

#[test]
fn test_custom_effect() {
    let executor = EffectExecutor::server();

    let effect = Effect::Custom {
        name: "my-custom-effect".to_string(),
        args: vec![Value::Number(42.0)],
        key: None,
        emit: None,
    };

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_noop()); // Custom effects are not handled
}

#[test]
fn test_fetch_effect_server() {
    let persistence = Arc::new(InMemoryPersistence::new());

    // Create some test data
    persistence
        .create(
            "User",
            &Value::Object({
                let mut m = ValueMap::default();
                m.insert("id".to_string(), Value::string("user-1"));
                m.insert("name".to_string(), Value::string("John"));
                m.into()
            }),
        )
        .unwrap();
    persistence
        .create(
            "User",
            &Value::Object({
                let mut m = ValueMap::default();
                m.insert("id".to_string(), Value::string("user-2"));
                m.insert("name".to_string(), Value::string("Jane"));
                m.into()
            }),
        )
        .unwrap();

    let executor = EffectExecutor::server()
        .with_persistence(persistence.clone())
        .with_fetch_handler(persistence);

    // Test fetch all
    let effect = Effect::Fetch {
        entity_type: "User".to_string(),
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

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_executed());

    // Verify entity_type is tracked
    assert_eq!(result.entity_type, Some("User".to_string()));

    let value = result.return_value.unwrap();
    let arr = value.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn test_fetch_effect_by_id() {
    let persistence = Arc::new(InMemoryPersistence::new());

    persistence
        .create(
            "User",
            &Value::Object({
                let mut m = ValueMap::default();
                m.insert("id".to_string(), Value::string("user-123"));
                m.insert("name".to_string(), Value::string("John"));
                m.into()
            }),
        )
        .unwrap();

    let executor = EffectExecutor::server()
        .with_persistence(persistence.clone())
        .with_fetch_handler(persistence);

    // Test fetch by ID
    let effect = Effect::Fetch {
        entity_type: "User".to_string(),
        id: Some(Value::string("user-123")),
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

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_executed());
    let value = result.return_value.unwrap();
    let arr = value.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0].as_object().unwrap().get("name").unwrap(),
        &Value::string("John")
    );
}

#[test]
fn test_fetch_effect_client_noop() {
    let executor = EffectExecutor::client();

    let effect = Effect::Fetch {
        entity_type: "User".to_string(),
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

    let result = executor.execute(&effect, &BindingContext::new());
    assert!(result.is_noop()); // Fetch is server-only
}

#[test]
fn test_fetch_filter_resolves_frame_ref_against_issuing_context() {
    struct RowScopedFetchHandler;
    impl FetchHandler for RowScopedFetchHandler {
        fn fetch(
            &self,
            entity_type: &str,
            _id: Option<&str>,
            filter: Option<&Value>,
            _limit: Option<usize>,
            _offset: Option<usize>,
            _include: Option<&Vec<String>>,
            ctx: &BindingContext,
        ) -> Result<FetchPage, String> {
            let mut rows = vec![
                Value::Object({
                    let mut m = ValueMap::default();
                    m.insert("id".to_string(), Value::string("r1"));
                    m.insert("channel".to_string(), Value::string("general"));
                    m.into()
                }),
                Value::Object({
                    let mut m = ValueMap::default();
                    m.insert("id".to_string(), Value::string("r2"));
                    m.insert("channel".to_string(), Value::string("random"));
                    m.into()
                }),
            ];
            crate::entity_access::apply_read_access(
                &mut rows,
                None,
                filter,
                ctx,
                entity_type,
            );
            Ok(FetchPage {
                total: rows.len(),
                rows,
            })
        }
    }

    // ["=", ["object/get", "@entity", "channel"], "@entity.activeChannel"] —
    // row-scoped LHS stays verbatim, issuing-frame RHS resolves before the
    // per-row evaluation. Previously both sides re-scoped to the row and
    // this self-comparison never matched.
    let filter = Value::array(vec![
        Value::String(("=".to_string()).into()),
        Value::Array(vec![
            Value::String(("object/get".to_string()).into()),
            Value::String(("@entity".to_string()).into()),
            Value::String(("channel".to_string()).into()),
        ].into()),
        Value::String(("@entity.activeChannel".to_string()).into()),
    ]);

    let issuing_entity = Value::Object({
        let mut m = ValueMap::default();
        m.insert(
            "activeChannel".to_string(),
            Value::string("general"),
        );
        m.into()
    });
    let ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
        .entity(issuing_entity)
        .build();

    let executor = EffectExecutor::server().with_fetch_handler(Arc::new(RowScopedFetchHandler));
    let effect = Effect::Fetch {
        entity_type: "Message".to_string(),
        id: None,
        filter: Some(filter),
        limit: None,
        offset: None,
        order_by: None,
        include: None,
        deliver: FetchDeliver::Rows,
        total: None,
        nearest: None,
        emit: None,
    };

    let result = executor.execute(&effect, &ctx);
    assert!(result.is_executed());
    let arr = result.return_value.unwrap().as_array().unwrap().clone();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0].get_path("id"),
        Some(&Value::string("r1"))
    );
}

#[test]
fn test_fetch_policy_predicate_stays_row_scoped() {
    // A declared @read policy with a bare-string leaf (`@entity.ownerId`)
    // must keep evaluating against each ROW — the frame-ref rewrite in
    // `interpolate_filter_trait_refs` only ever touches the call-site
    // `filter:` passed into `do_fetch`, never the policy predicate a
    // FetchHandler applies internally.
    struct PolicyScopedFetchHandler;
    impl FetchHandler for PolicyScopedFetchHandler {
        fn fetch(
            &self,
            entity_type: &str,
            _id: Option<&str>,
            filter: Option<&Value>,
            _limit: Option<usize>,
            _offset: Option<usize>,
            _include: Option<&Vec<String>>,
            ctx: &BindingContext,
        ) -> Result<FetchPage, String> {
            let mut rows = vec![
                Value::Object({
                    let mut m = ValueMap::default();
                    m.insert("id".to_string(), Value::string("r1"));
                    m.insert("ownerId".to_string(), Value::string("user-1"));
                    m.into()
                }),
                Value::Object({
                    let mut m = ValueMap::default();
                    m.insert("id".to_string(), Value::string("r2"));
                    m.insert("ownerId".to_string(), Value::string("user-2"));
                    m.into()
                }),
            ];
            let policy = Value::array(vec![
                Value::String(("=".to_string()).into()),
                Value::String(("@entity.ownerId".to_string()).into()),
                Value::String(("@user.id".to_string()).into()),
            ]);
            crate::entity_access::apply_read_access(
                &mut rows,
                Some(&policy),
                filter,
                ctx,
                entity_type,
            );
            Ok(FetchPage {
                total: rows.len(),
                rows,
            })
        }
    }

    let viewer = Value::Object({
        let mut m = ValueMap::default();
        m.insert("id".to_string(), Value::string("user-1"));
        m.into()
    });
    let ctx = orbital_core_eval::evaluator::context::BindingContextBuilder::new()
        .user(viewer)
        .build();

    let executor =
        EffectExecutor::server().with_fetch_handler(Arc::new(PolicyScopedFetchHandler));
    let effect = Effect::Fetch {
        entity_type: "Ticket".to_string(),
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

    let result = executor.execute(&effect, &ctx);
    assert!(result.is_executed());
    let arr = result.return_value.unwrap().as_array().unwrap().clone();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0].get_path("id"),
        Some(&Value::string("r1"))
    );
}

/// `@derived` fields: the kernel computes them on every write path, from the row being written, before the
/// policies and the required-column check; an expression that fails fails the write.
mod derived_fields {
    use super::*;
    use crate::entity_access::EntityAccessPolicies;
    use crate::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_base::row_set::RowQuery;
    use serde_json::json;

    fn v(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn s(j: serde_json::Value) -> SExpression {
        serde_json::from_value(j).unwrap()
    }

    fn lower_email() -> SExpression {
        s(json!(["str/lower", ["object/get", "@entity", "email"]]))
    }

    fn executor(
        derived: Vec<(&str, SExpression)>,
        create: Option<serde_json::Value>,
        required: &[&str],
    ) -> (EffectExecutor, Arc<InMemoryPersistence>) {
        let persistence = Arc::new(InMemoryPersistence::new());
        let mut table = EntityAccessTable::new();
        table.insert(
            "Person".to_string(),
            EntityAccessPolicies {
                create: create.map(v),
                required_create_fields: required.iter().map(|r| r.to_string()).collect(),
                derived_fields: derived.into_iter().map(|(n, e)| (n.to_string(), e)).collect(),
                ..Default::default()
            },
        );
        let executor = EffectExecutor::server()
            .with_persistence(persistence.clone())
            .with_entity_policies(Arc::new(RwLock::new(table)));
        (executor, persistence)
    }

    fn persist(operation: PersistOperation, entity: &str, id: Option<&str>, data: serde_json::Value) -> Effect {
        Effect::Persist {
            operation,
            entity_type: entity.to_string(),
            data: v(data),
            id: id.map(str::to_string),
            emit: None,
            guard: None,
        }
    }

    fn stored(persistence: &InMemoryPersistence, id: &str) -> serde_json::Value {
        serde_json::to_value(persistence.read("Person", id).unwrap()).unwrap()
    }

    #[test]
    fn create_computes_the_derived_field_and_an_entity_without_one_stores_nothing_extra() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let ctx = BindingContext::new();
        let made = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C" })), &ctx);
        assert!(made.is_executed(), "{made:?}");
        assert_eq!(stored(&persistence, "p1")["emailKey"], json!("a@b.c"));
        let plain = executor.execute(&persist(PersistOperation::Create, "Other", None, json!({ "id": "o1", "email": "A@B.C" })), &ctx);
        assert!(plain.is_executed(), "{plain:?}");
        assert!(persistence.read("Other", "o1").unwrap().as_object().unwrap().get("emailKey").is_none(), "control: no derived field, none computed");
    }

    #[test]
    fn a_value_the_caller_supplies_for_a_derived_field_is_replaced_by_the_computed_one() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let made = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C", "emailKey": "forged" })), &BindingContext::new());
        assert!(made.is_executed(), "{made:?}");
        assert_eq!(stored(&persistence, "p1")["emailKey"], json!("a@b.c"));
    }

    #[test]
    fn update_recomputes_from_the_stored_row_with_the_patch_laid_over_it() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let ctx = BindingContext::new();
        assert!(executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C", "nick": "a" })), &ctx).is_executed());
        let changed = executor.execute(&persist(PersistOperation::Update, "Person", Some("p1"), json!({ "email": "New@X.Y" })), &ctx);
        assert!(changed.is_executed(), "{changed:?}");
        assert_eq!(stored(&persistence, "p1")["emailKey"], json!("new@x.y"));
        let other = executor.execute(&persist(PersistOperation::Update, "Person", Some("p1"), json!({ "nick": "z" })), &ctx);
        assert!(other.is_executed(), "{other:?}");
        let row = stored(&persistence, "p1");
        assert_eq!((row["emailKey"].clone(), row["nick"].clone()), (json!("new@x.y"), json!("z")), "a patch that leaves email alone recomputes from the stored email");
    }

    #[test]
    fn upsert_computes_on_both_the_create_and_the_replace() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let ctx = BindingContext::new();
        assert!(executor.execute(&persist(PersistOperation::Upsert, "Person", None, json!({ "id": "p1", "email": "A@B.C" })), &ctx).is_executed());
        assert_eq!(stored(&persistence, "p1")["emailKey"], json!("a@b.c"));
        assert!(executor.execute(&persist(PersistOperation::Upsert, "Person", None, json!({ "id": "p1", "email": "Z@B.C" })), &ctx).is_executed());
        assert_eq!(stored(&persistence, "p1")["emailKey"], json!("z@b.c"));
    }

    #[test]
    fn every_op_of_a_batch_computes_and_a_batch_of_an_entity_without_one_is_unchanged() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let ctx = BindingContext::new();
        let batch = Effect::PersistBatch {
            ops: v(json!([
                ["create", "Person", { "id": "b1", "email": "ONE@X.Y" }],
                ["create", "Person", { "id": "b2", "email": "TWO@X.Y" }],
                ["update", "Person", "b1", { "email": "Uno@X.Y" }]
            ])),
            emit: None,
        };
        let result = executor.execute(&batch, &ctx);
        assert!(result.is_executed(), "{result:?}");
        assert_eq!(stored(&persistence, "b2")["emailKey"], json!("two@x.y"));
        assert!(stored(&persistence, "b1").get("email").is_some());
        let plain = Effect::PersistBatch { ops: v(json!([["create", "Other", { "id": "o1", "email": "A@B.C" }]])), emit: None };
        assert!(executor.execute(&plain, &ctx).is_executed());
        assert!(persistence.read("Other", "o1").unwrap().as_object().unwrap().get("emailKey").is_none());
    }

    #[test]
    fn a_derived_field_may_read_another_field_and_an_earlier_derived_field() {
        let (executor, persistence) = executor(
            vec![
                ("emailKey", lower_email()),
                ("slug", s(json!(["str/concat", ["object/get", "@entity", "emailKey"], "-", ["object/get", "@entity", "nick"]]))),
            ],
            None,
            &[],
        );
        let made = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C", "nick": "x" })), &BindingContext::new());
        assert!(made.is_executed(), "{made:?}");
        assert_eq!(stored(&persistence, "p1")["slug"], json!("a@b.c-x"));
    }

    #[test]
    fn the_create_policy_and_the_required_check_see_the_computed_value() {
        let policy = json!(["=", "@entity.emailKey", "a@b.c"]);
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], Some(policy), &["emailKey"]);
        let ctx = BindingContext::new();
        let allowed = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C" })), &ctx);
        assert!(allowed.is_executed(), "required emailKey is not missing and the policy reads the computed key: {allowed:?}");
        let denied = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p2", "email": "Other@B.C" })), &ctx);
        assert!(denied.is_failed());
        assert_eq!(denied.code, Some(ServerEffectCode::Denied));
        assert!(persistence.find("Person", "p2").unwrap().is_none(), "control: the denied row was not stored");
    }

    #[test]
    fn an_expression_that_fails_fails_the_write_by_name_and_stores_nothing() {
        let (executor, persistence) = executor(vec![("emailKey", s(json!(["no/such-operator", "@entity.email"])))], None, &[]);
        let ctx = BindingContext::new();
        for effect in [
            persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "a" })),
            persist(PersistOperation::Upsert, "Person", None, json!({ "id": "p2", "email": "a" })),
        ] {
            let result = executor.execute(&effect, &ctx);
            assert!(result.is_failed(), "{result:?}");
            let message = result.error.clone().unwrap_or_default();
            assert!(message.contains("derived field `emailKey`"), "named error: {message}");
            assert_eq!(result.code, Some(ServerEffectCode::DerivedFailed), "{message}");
        }
        assert!(persistence.find("Person", "p1").unwrap().is_none() && persistence.find("Person", "p2").unwrap().is_none(), "nothing stored, no null");
        let batch = Effect::PersistBatch { ops: v(json!([["create", "Person", { "id": "p3", "email": "a" }]])), emit: None };
        let failed = executor.execute(&batch, &ctx);
        assert!(failed.is_failed(), "{failed:?}");
        assert_eq!(failed.code, Some(ServerEffectCode::DerivedFailed), "a batch op reports the same code");
        assert!(persistence.find("Person", "p3").unwrap().is_none());
    }

    #[test]
    fn control_a_valid_expression_reports_no_code() {
        let (executor, _) = executor(vec![("emailKey", lower_email())], None, &[]);
        let made = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C" })), &BindingContext::new());
        assert!(made.is_executed() && made.code.is_none(), "{made:?}");
    }

    /// A store that runs conditional batches: the pushed path, where every op is prepared before any lands.
    struct Pushing(Arc<InMemoryPersistence>);

    impl PersistenceHandler for Pushing {
        fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
            self.0.create(entity_type, data)
        }
        fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
            self.0.read(entity_type, id)
        }
        fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
            self.0.find(entity_type, id)
        }
        fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
            self.0.update(entity_type, id, data)
        }
        fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
            self.0.delete(entity_type, id)
        }
        fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
            self.0.list(entity_type)
        }
        fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
            let outcomes = batch
                .ops
                .iter()
                .map(|op| match op {
                    RowWriteOp::Create { entity_type, data } => match self.0.create(entity_type, data) {
                        Ok(row) => RowWriteOutcome::Created { row },
                        Err(message) => RowWriteOutcome::Failed { message },
                    },
                    RowWriteOp::Update { entity_type, id, data, .. } => match self.0.update(entity_type, id, data) {
                        Ok(row) => RowWriteOutcome::Updated { row, before: None },
                        Err(message) => RowWriteOutcome::Failed { message },
                    },
                    _ => RowWriteOutcome::Failed { message: "unsupported by this test store".to_string() },
                })
                .collect();
            Some(Ok(outcomes))
        }
    }

    fn create_then_update_batch() -> Effect {
        Effect::PersistBatch {
            ops: v(json!([
                ["create", "Person", { "id": "b1", "email": "ONE@X.Y", "nick": "n" }],
                ["update", "Person", "b1", { "nick": "m" }],
                ["update", "Person", "b1", { "email": "Uno@X.Y" }]
            ])),
            emit: None,
        }
    }

    fn over(persistence: Arc<dyn PersistenceHandler>) -> EffectExecutor {
        let mut table = EntityAccessTable::new();
        table.insert(
            "Person".to_string(),
            EntityAccessPolicies { derived_fields: vec![("emailKey".to_string(), lower_email())], ..Default::default() },
        );
        EffectExecutor::server().with_persistence(persistence).with_entity_policies(Arc::new(RwLock::new(table)))
    }

    #[test]
    fn a_create_then_updates_in_one_pushed_batch_recompute_from_the_batchs_own_rows() {
        let store = Arc::new(InMemoryPersistence::new());
        let result = over(Arc::new(Pushing(store.clone()))).execute(&create_then_update_batch(), &BindingContext::new());
        assert!(result.is_executed(), "{result:?}");
        let row = stored(&store, "b1");
        assert_eq!(row["emailKey"], json!("uno@x.y"), "the last update recomputed from the created row with the earlier patches laid over it");
        assert_eq!(row["nick"], json!("m"));
    }

    #[test]
    fn control_the_per_op_path_gives_the_same_rows_as_the_pushed_one() {
        let pushed = Arc::new(InMemoryPersistence::new());
        let per_op = Arc::new(InMemoryPersistence::new());
        let ctx = BindingContext::new();
        assert!(over(Arc::new(Pushing(pushed.clone()))).execute(&create_then_update_batch(), &ctx).is_executed());
        assert!(over(per_op.clone()).execute(&create_then_update_batch(), &ctx).is_executed());
        let (a, b) = (stored(&pushed, "b1"), stored(&per_op, "b1"));
        assert_eq!((a["emailKey"].clone(), a["nick"].clone(), a["email"].clone()), (b["emailKey"].clone(), b["nick"].clone(), b["email"].clone()));
    }

    #[test]
    fn a_run_of_persists_in_one_transition_recomputes_from_its_own_earlier_write() {
        let store = Arc::new(InMemoryPersistence::new());
        let effects = vec![
            persist(PersistOperation::Create, "Person", None, json!({ "id": "r1", "email": "ONE@X.Y" })),
            persist(PersistOperation::Update, "Person", Some("r1"), json!({ "email": "Two@X.Y" })),
        ];
        let results = over(Arc::new(Pushing(store.clone()))).execute_persist_run(&effects, &BindingContext::new());
        assert!(results.iter().all(|r| !r.is_failed()), "{results:?}");
        assert_eq!(stored(&store, "r1")["emailKey"], json!("two@x.y"));
    }

    #[test]
    fn a_working_expression_beside_the_failing_one_still_writes_control() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let made = executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": "p1", "email": "A@B.C" })), &BindingContext::new());
        assert!(made.is_executed() && persistence.read("Person", "p1").is_ok());
    }

    #[test]
    fn an_index_on_a_derived_field_answers_a_covered_query_from_the_stored_values() {
        let (executor, persistence) = executor(vec![("emailKey", lower_email())], None, &[]);
        let ctx = BindingContext::new();
        for (id, email) in [("p1", "A@B.C"), ("p2", "x@y.z"), ("p3", "a@B.c"), ("p4", "q@y.z")] {
            assert!(executor.execute(&persist(PersistOperation::Create, "Person", None, json!({ "id": id, "email": email })), &ctx).is_executed());
        }
        let indexed = InMemoryEntityCache::new();
        let plain = InMemoryEntityCache::new();
        indexed.declare_indexes(
            "Person",
            &[orbital_core_base::schema::types::EntityIndex { fields: vec!["emailKey".to_string()], order_by: None }],
        );
        for id in ["p1", "p2", "p3", "p4"] {
            let row = persistence.read("Person", id).unwrap();
            indexed.write("Person", id, &row).unwrap();
            plain.write("Person", id, &row).unwrap();
        }
        let query: RowQuery = serde_json::from_value(json!({ "filter": { "kind": "compare", "field": "emailKey", "op": "==", "value": "a@b.c" } })).unwrap();
        let want = plain.query_page("Person", &query).unwrap_rows();
        assert_eq!(plain.rows_visited(), 4, "an undeclared cache scans every row");
        let got = indexed.query_page("Person", &query).unwrap_rows();
        assert_eq!(got, want);
        assert_eq!(want.rows.len(), 2, "the two spellings of one address share the computed key");
        assert_eq!(indexed.rows_visited(), 2, "the index on the derived field named the two candidates");
    }
}

mod in_memory_read_access {
    use super::*;
    use crate::entity_access::EntityAccessPolicies;
    use serde_json::json;

    fn v(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn store(
        rows: &[(&str, &str, &str)],
        read: Option<serde_json::Value>,
    ) -> Arc<InMemoryPersistence> {
        let mut table: EntityAccessTable = HashMap::new();
        table.insert(
            "Note".to_string(),
            EntityAccessPolicies {
                read: read.map(v),
                ..Default::default()
            },
        );
        let persistence =
            InMemoryPersistence::new().with_entity_policies(Arc::new(RwLock::new(table)));
        for (id, owner, status) in rows {
            persistence
                .create(
                    "Note",
                    &v(json!({ "id": id, "ownerId": owner, "status": status })),
                )
                .unwrap();
        }
        Arc::new(persistence)
    }

    fn owner_only() -> Option<serde_json::Value> {
        Some(json!([
            "=",
            ["object/get", "@entity", "ownerId"],
            "@user.id"
        ]))
    }

    fn viewer(user: Option<serde_json::Value>) -> BindingContext {
        match user {
            Some(u) => orbital_core_eval::evaluator::context::BindingContextBuilder::new()
                .user(v(u))
                .build(),
            None => BindingContext::new(),
        }
    }

    fn run(
        handler: Arc<InMemoryPersistence>,
        id: Option<&str>,
        filter: Option<serde_json::Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let executor = EffectExecutor::server().with_fetch_handler(handler);
        let effect = Effect::Fetch {
            entity_type: "Note".to_string(),
            id: id.map(|s| Value::string(s.to_string())),
            filter: filter.map(v),
            limit: limit.map(|n| Value::Number(n as f64)),
            offset: offset.map(|n| Value::Number(n as f64)),
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            nearest: None,
            emit: None,
        };
        executor.execute(&effect, ctx)
    }

    fn fetch(
        handler: Arc<InMemoryPersistence>,
        id: Option<&str>,
        filter: Option<serde_json::Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        ctx: &BindingContext,
    ) -> Vec<(String, String)> {
        let result = run(handler, id, filter, limit, offset, ctx);
        assert!(
            result.is_executed(),
            "fetch must execute, got {:?}",
            result.outcome
        );
        let rows = result
            .return_value
            .map(|r| serde_json::to_value(r).unwrap());
        let mut out: Vec<(String, String)> = rows
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|r| {
                (
                    r["id"].as_str().unwrap().to_string(),
                    r["ownerId"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        out.sort();
        out
    }

    fn ids(rows: &[(String, String)]) -> Vec<&str> {
        rows.iter().map(|(id, _)| id.as_str()).collect()
    }

    const ROWS: &[(&str, &str, &str)] = &[
        ("a1", "alice", "open"),
        ("a2", "alice", "closed"),
        ("b1", "bob", "open"),
    ];

    #[test]
    fn collection_is_scoped_to_the_viewers_rows() {
        let rows = fetch(
            store(ROWS, owner_only()),
            None,
            None,
            None,
            None,
            &viewer(Some(json!({ "id": "alice" }))),
        );
        assert_eq!(ids(&rows), vec!["a1", "a2"]);
    }

    #[test]
    fn anonymous_viewer_sees_nothing_under_an_owner_policy() {
        assert!(fetch(
            store(ROWS, owner_only()),
            None,
            None,
            None,
            None,
            &viewer(None)
        )
        .is_empty());
    }

    #[test]
    fn by_id_of_an_unreadable_row_fails_as_not_found() {
        let alice = viewer(Some(json!({ "id": "alice" })));
        let r = run(
            store(ROWS, owner_only()),
            Some("b1"),
            None,
            None,
            None,
            &alice,
        );
        assert!(!r.is_executed());
        assert_eq!(r.error.as_deref(), Some("Note b1 not found"));
    }

    #[test]
    fn by_id_of_a_readable_row_is_served() {
        let rows = fetch(
            store(ROWS, owner_only()),
            Some("a1"),
            None,
            None,
            None,
            &viewer(Some(json!({ "id": "alice" }))),
        );
        assert_eq!(ids(&rows), vec!["a1"]);
    }

    #[test]
    fn by_id_of_a_missing_row_fails_exactly_like_a_refused_one() {
        let alice = viewer(Some(json!({ "id": "alice" })));
        let missing = run(
            store(ROWS, owner_only()),
            Some("zz"),
            None,
            None,
            None,
            &alice,
        );
        assert!(!missing.is_executed());
        assert_eq!(missing.error.as_deref(), Some("Note zz not found"));
        let refused = run(
            store(ROWS, owner_only()),
            Some("b1"),
            None,
            None,
            None,
            &alice,
        );
        assert_eq!(
            refused.error.as_deref().map(|e| e.replace("b1", "zz")),
            missing.error,
            "a refused row must be indistinguishable from a missing one"
        );
    }

    #[test]
    fn call_site_filter_narrows_within_the_policy() {
        let filter = json!(["=", ["object/get", "@entity", "status"], "open"]);
        let rows = fetch(
            store(ROWS, owner_only()),
            None,
            Some(filter),
            None,
            None,
            &viewer(Some(json!({ "id": "alice" }))),
        );
        assert_eq!(ids(&rows), vec!["a1"]);
    }

    #[test]
    fn filter_cannot_widen_past_the_policy() {
        let filter = json!(["=", ["object/get", "@entity", "ownerId"], "bob"]);
        let rows = fetch(
            store(ROWS, owner_only()),
            None,
            Some(filter),
            None,
            None,
            &viewer(Some(json!({ "id": "alice" }))),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn pagination_applies_after_the_policy() {
        let many: Vec<(String, String)> = (0..6)
            .map(|i| {
                (
                    format!("r{i}"),
                    if i % 2 == 0 {
                        "alice".into()
                    } else {
                        "bob".into()
                    },
                )
            })
            .collect();
        let rows_ref: Vec<(&str, &str, &str)> = many
            .iter()
            .map(|(id, o)| (id.as_str(), o.as_str(), "open"))
            .collect();
        let alice = viewer(Some(json!({ "id": "alice" })));
        let page = fetch(
            store(&rows_ref, owner_only()),
            None,
            None,
            Some(2),
            Some(1),
            &alice,
        );
        assert_eq!(page.len(), 2);
        assert!(page.iter().all(|(_, owner)| owner == "alice"));
        let past_end = fetch(
            store(&rows_ref, owner_only()),
            None,
            None,
            None,
            Some(3),
            &alice,
        );
        assert!(past_end.is_empty());
    }

    #[test]
    fn no_read_policy_is_allow_all() {
        let rows = fetch(
            store(ROWS, None),
            None,
            None,
            None,
            None,
            &viewer(Some(json!({ "id": "alice" }))),
        );
        assert_eq!(ids(&rows), vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn no_policy_table_is_allow_all() {
        let persistence = Arc::new(InMemoryPersistence::new());
        for (id, owner, status) in ROWS {
            persistence
                .create(
                    "Note",
                    &v(json!({ "id": id, "ownerId": owner, "status": status })),
                )
                .unwrap();
        }
        let rows = fetch(persistence, None, None, None, None, &viewer(None));
        assert_eq!(ids(&rows), vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn role_or_owner_policy_admits_both_kinds_of_viewer() {
        let policy = Some(json!([
            "or",
            ["=", "@user.role", "admin"],
            ["=", ["object/get", "@entity", "ownerId"], "@user.id"]
        ]));
        let admin = fetch(
            store(ROWS, policy.clone()),
            None,
            None,
            None,
            None,
            &viewer(Some(json!({ "id": "carol", "role": "admin" }))),
        );
        assert_eq!(ids(&admin), vec!["a1", "a2", "b1"]);
        let bob = fetch(
            store(ROWS, policy),
            None,
            None,
            None,
            None,
            &viewer(Some(json!({ "id": "bob", "role": "member" }))),
        );
        assert_eq!(ids(&bob), vec!["b1"]);
    }
}

mod fetch_option_evaluation {
    use super::*;
    use serde_json::json;

    fn v(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn run(
        options: serde_json::Value,
        payload: serde_json::Value,
        entity: serde_json::Value,
    ) -> EffectResult {
        let persistence = Arc::new(InMemoryPersistence::new());
        for i in 0..5 {
            persistence
                .create("Row", &v(json!({ "id": format!("r{i}"), "n": i })))
                .unwrap();
        }
        let executor = EffectExecutor::server().with_fetch_handler(persistence);
        let raw: SExpression =
            serde_json::from_value(json!(["fetch", "Row", options])).unwrap();
        let effect = orbital_core_base::effect_types::parse_effect(&raw).expect("fetch parses");
        let mut ctx = BindingContext::new();
        ctx.payload = Arc::new(v(payload));
        ctx.set_entity(v(entity));
        executor.execute(&effect, &ctx)
    }

    fn count(result: &EffectResult) -> usize {
        assert!(
            result.is_executed(),
            "fetch must execute: {:?}",
            result.error
        );
        result
            .return_value
            .as_ref()
            .and_then(|r| r.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    }

    #[test]
    fn literal_limit_applies() {
        assert_eq!(count(&run(json!({ "limit": 2 }), json!({}), json!({}))), 2);
    }

    #[test]
    fn payload_bound_limit_and_offset_apply() {
        let r = run(
            json!({ "limit": "@payload.limit", "offset": "@payload.offset" }),
            json!({ "limit": 2, "offset": 4 }),
            json!({}),
        );
        assert_eq!(count(&r), 1);
    }

    #[test]
    fn expression_limit_is_evaluated() {
        let opts = json!({ "limit": ["default", "@payload.limit", 3] });
        assert_eq!(count(&run(opts.clone(), json!({}), json!({}))), 3);
        assert_eq!(count(&run(opts, json!({ "limit": 1 }), json!({}))), 1);
    }

    #[test]
    fn a_limit_or_offset_that_is_not_a_whole_number_fails_the_fetch() {
        for limit in [json!(-1), json!(1.5), json!("abc"), json!("@payload.missing")] {
            let r = run(json!({ "limit": limit }), json!({}), json!({}));
            assert!(!r.is_executed(), "limit {limit} must fail the fetch");
        }
        for offset in [json!(-1), json!(1.5), json!("abc")] {
            let r = run(json!({ "offset": offset }), json!({}), json!({}));
            assert!(!r.is_executed(), "offset {offset} must fail the fetch");
        }
    }

    #[test]
    fn a_limit_is_at_least_one_and_an_offset_may_be_zero() {
        let executor = EffectExecutor::server();
        let ctx = BindingContext::new();
        assert!(executor.resolve_limit(Some(&v(json!(0))), &ctx).is_err());
        assert_eq!(executor.resolve_limit(Some(&v(json!(3))), &ctx), Ok(Some(3)));
        assert_eq!(executor.resolve_limit(None, &ctx), Ok(None));
        assert_eq!(executor.resolve_page_option(Some(&v(json!(0))), &ctx), Ok(None));
        assert_eq!(executor.resolve_page_option(Some(&v(json!(2))), &ctx), Ok(Some(2)));
        assert_eq!(count(&run(json!({ "offset": 0 }), json!({}), json!({}))), 5);
    }

    fn ids(result: &EffectResult) -> Vec<String> {
        result
            .return_value
            .as_ref()
            .and_then(|r| r.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| match row.get_path("id") {
                        Some(Value::String(id)) => Some(id.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn order_by_field_and_direction_applies_before_paging() {
        assert_eq!(
            ids(&run(json!({ "orderBy": "n:desc" }), json!({}), json!({}))),
            ["r4", "r3", "r2", "r1", "r0"]
        );
        assert_eq!(
            ids(&run(
                json!({ "orderBy": "n:desc", "limit": 2 }),
                json!({}),
                json!({})
            )),
            ["r4", "r3"]
        );
    }

    #[test]
    fn a_bare_order_field_is_ascending_and_a_payload_spec_is_evaluated() {
        assert_eq!(
            ids(&run(
                json!({ "orderBy": "@payload.order", "limit": 2 }),
                json!({ "order": "n:desc" }),
                json!({})
            )),
            ["r4", "r3"]
        );
        assert_eq!(
            ids(&run(
                json!({ "orderBy": "n", "offset": 3 }),
                json!({}),
                json!({})
            )),
            ["r3", "r4"]
        );
    }

    #[test]
    fn without_order_by_rows_lacking_created_at_follow_id_order() {
        assert_eq!(
            ids(&run(json!({ "limit": 2 }), json!({}), json!({}))),
            ["r0", "r1"]
        );
    }

    /// EXEC-FETCH-1: an unordered fetch is `createdAt` ascending, a row without one last, ties by id; the
    /// insertion order of the store is not the answer. Twin: `almadar-runtime/test/fetch-default-order.test.ts`.
    #[test]
    fn without_order_by_the_default_order_is_created_at_then_id() {
        let persistence = Arc::new(InMemoryPersistence::new());
        for (id, created) in [
            ("a", Some("2026-03-02T00:00:00.000Z")),
            ("b", None),
            ("c", Some("2026-03-01T00:00:00.000Z")),
            ("d", Some("2026-03-02T00:00:00.000Z")),
        ] {
            let mut row = json!({ "id": id });
            if let Some(created) = created {
                row["createdAt"] = json!(created);
            }
            persistence.create("Row", &v(row)).unwrap();
        }
        let executor = EffectExecutor::server().with_fetch_handler(persistence);
        let fetch = |options: serde_json::Value| {
            let raw: SExpression =
                serde_json::from_value(json!(["fetch", "Row", options])).unwrap();
            let effect = orbital_core_base::effect_types::parse_effect(&raw).expect("fetch parses");
            let mut ctx = BindingContext::new();
            ctx.payload = Arc::new(v(json!({})));
            ctx.set_entity(v(json!({})));
            ids(&executor.execute(&effect, &ctx))
        };
        assert_eq!(fetch(json!({})), ["c", "a", "d", "b"]);
        assert_eq!(fetch(json!({ "offset": 1, "limit": 2 })), ["a", "d"]);
        // control: an authored order replaces the default
        assert_eq!(fetch(json!({ "orderBy": "id:desc" })), ["d", "c", "b", "a"]);
    }

    #[test]
    fn offset_past_the_end_is_empty() {
        assert_eq!(count(&run(json!({ "offset": 9 }), json!({}), json!({}))), 0);
    }

    #[test]
    fn expression_id_fetches_one_row_not_the_collection() {
        let r = run(
            json!({ "id": ["object/get", "@payload", "rowId"] }),
            json!({ "rowId": "r3" }),
            json!({}),
        );
        assert_eq!(count(&r), 1);
        let row = r.return_value.unwrap().as_array().unwrap()[0].clone();
        assert_eq!(row.get_path("id"), Some(&Value::string("r3")));
    }

    #[test]
    fn expression_id_resolving_to_a_non_id_fails_loudly() {
        let r = run(
            json!({ "id": ["object/get", "@payload", "rowId"] }),
            json!({ "rowId": true }),
            json!({}),
        );
        assert!(!r.is_executed());
        assert!(r.error.unwrap().contains("not a string or integer"));
    }

    #[test]
    fn by_id_miss_fails_instead_of_succeeding_empty() {
        let r = run(
            json!({ "id": "@payload.rowId" }),
            json!({ "rowId": "zz" }),
            json!({}),
        );
        assert!(!r.is_executed());
        assert_eq!(r.error.as_deref(), Some("Row zz not found"));
    }

    #[test]
    fn unresolved_id_binding_still_fails_loudly() {
        let r = run(json!({ "id": "@payload.nope" }), json!({}), json!({}));
        assert!(!r.is_executed());
    }
}
