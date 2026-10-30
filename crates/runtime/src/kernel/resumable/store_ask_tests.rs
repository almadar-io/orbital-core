use super::*;

fn bound_port() -> IoPort {
    let port = IoPort::new(Arc::new(HostAnswered), None, None);
    port.bind_store("Ticket", "Rows");
    port
}

#[test]
fn rekey_requires_explicit_capability_and_never_falls_back_after_host_refusal() {
    let op = RowWriteOp::Rekey {
        entity_type: "Ticket".into(),
        id: "a".into(),
        new_id: "b".into(),
        guard: None,
    };
    let direct = IoPort::new(Arc::new(HostAnswered), None, None);
    assert!(matches!(direct.write_where(&op), Some(Err(_))));
    assert!(matches!(
        direct.write_batch(&RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop
        }),
        Some(Err(_))
    ));
    for capability in [None, Some(false)] {
        let port = bound_port();
        let mut capabilities = WriteCapabilities::full();
        capabilities.rekey = capability;
        port.set_write_capabilities("Ticket", capabilities);
        let (outcome, end) = port.run_step(Vec::new(), || port.write_where(&op));
        assert!(
            matches!(outcome, Some(Err(ref message)) if message == "rekey is unsupported by this store")
        );
        assert!(end.store_asks.is_empty() && end.pending.is_empty());
    }
    let port = bound_port();
    let (outcome, end) = port.run_step(Vec::new(), || port.write_where(&op));
    assert!(matches!(outcome, Some(Err(_))));
    assert!(end.store_asks.is_empty() && end.pending.is_empty());
    let mut capabilities = WriteCapabilities::full();
    capabilities.rekey = Some(true);
    port.set_write_capabilities("Ticket", capabilities);
    let (_, end) = port.run_step(Vec::new(), || port.write_where(&op));
    assert_eq!(end.store_asks.len(), 1);
    let request = end.store_asks[0].request.clone();
    let refusal = Exchange::new(request, IoAnswer::WriteUnsupported("driver refused".into()));
    let (outcome, end) = port.run_step(vec![refusal], || port.write_where(&op));
    assert!(matches!(outcome, Some(Err(ref message)) if message.contains("driver refused")));
    assert!(end.store_asks.is_empty() && end.pending.is_empty());
}

fn take(guard: Option<crate::row_query::RowPredicate>) -> IoRequest {
    IoRequest::WriteRows {
        batch: RowWriteBatch {
            ops: vec![RowWriteOp::Delete { entity_type: "Ticket".into(), id: "t1".into(), guard }],
            on_failure: RowWriteFailureMode::Stop,
        },
    }
}

#[test]
fn a_row_ask_on_a_store_bound_entity_is_a_store_ask_never_a_host_request() {
    let port = bound_port();
    let (found, end) = port.run_step(Vec::new(), || port.find("Ticket", "t1"));
    assert_eq!(found, Err(AWAITING_HOST.to_string()));
    assert!(end.pending.is_empty(), "{:?}", end.pending);
    assert_eq!(end.store_asks.len(), 1);
    assert_eq!(end.store_asks[0].driver, "Rows");
    assert_eq!(end.store_asks[0].request, IoRequest::ReadRow { entity_type: "Ticket".into(), id: "t1".into() });
    assert!(crate::store_driver::verb_for(&end.store_asks[0].request, &end.store_asks[0].key, &|_| None).is_some());
}

#[test]
fn control_an_unbound_entity_still_asks_the_host() {
    let port = bound_port();
    let (_, end) = port.run_step(Vec::new(), || port.find("Note", "n1"));
    assert!(end.store_asks.is_empty());
    assert_eq!(end.pending.as_slice(), &[IoRequest::ReadRow { entity_type: "Note".into(), id: "n1".into() }]);
    let (_, end) = port.run_step(Vec::new(), || port.delete("Note", "n1"));
    assert_eq!(end.pending.as_slice(), &[IoRequest::DeleteRow { entity_type: "Note".into(), id: "n1".into() }]);
}

#[test]
fn a_single_delete_on_a_bound_entity_is_one_conditional_write_op() {
    let port = bound_port();
    let (_, end) = port.run_step(Vec::new(), || port.delete("Ticket", "t1"));
    assert_eq!(end.store_asks.len(), 1);
    assert_eq!(end.store_asks[0].request, take(None));
}

#[test]
fn a_replayed_take_succeeds_once_and_a_missing_row_fails() {
    let port = bound_port();
    let deleted = Exchange::new(take(None), IoAnswer::Wrote(Ok(vec![RowWriteOutcome::Deleted { before: None }])));
    let (done, end) = port.run_step(vec![deleted], || port.delete("Ticket", "t1"));
    assert_eq!(done, Ok(()));
    assert!(end.store_asks.is_empty() && end.pending.is_empty());
    let gone = Exchange::new(take(None), IoAnswer::Wrote(Ok(vec![RowWriteOutcome::NotFound])));
    let (done, _) = port.run_step(vec![gone], || port.delete("Ticket", "t1"));
    assert_eq!(done, Err("Entity not found: Ticket:t1".to_string()));
}

#[test]
fn a_single_update_on_a_bound_entity_is_one_update_op_and_a_missing_row_fails() {
    let port = bound_port();
    let data: Value = serde_json::from_value(serde_json::json!({"used": true})).unwrap();
    let (_, end) = port.run_step(Vec::new(), || port.write("Ticket", "t1", &data));
    let update = IoRequest::WriteRows {
        batch: RowWriteBatch {
            ops: vec![RowWriteOp::Update { entity_type: "Ticket".into(), id: "t1".into(), data: data.clone(), guard: None }],
            on_failure: RowWriteFailureMode::Stop,
        },
    };
    assert_eq!(end.store_asks[0].request, update);
    let gone = Exchange::new(update, IoAnswer::Wrote(Ok(vec![RowWriteOutcome::NotFound])));
    let (done, _) = port.run_step(vec![gone], || port.write("Ticket", "t1", &data));
    assert_eq!(done, Err("Entity not found: Ticket:t1".to_string()));
}

fn row(fields: serde_json::Value) -> Value {
    serde_json::from_value(fields).unwrap()
}

fn create_of(request: &IoRequest) -> &Value {
    let IoRequest::WriteRows { batch } = request else { panic!("expected a write, got {request:?}") };
    let [RowWriteOp::Create { data, .. }] = batch.ops.as_slice() else { panic!("expected one create") };
    data
}

fn draw(fill: u8) -> Exchange {
    Exchange::new(IoRequest::Random { bytes: 16 }, IoAnswer::Random(Ok((vec![fill; 16]).into())))
}

fn id_of(request: &IoRequest) -> String {
    create_of(request).as_object().unwrap().get("id").and_then(Value::as_str).unwrap().to_string()
}

fn is_uuid_v4(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        && parts[2].starts_with('4')
        && matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b')
}

#[test]
fn a_create_without_an_id_on_a_bound_entity_is_minted_a_uuid_from_the_journaled_draw() {
    let port = bound_port();
    let data = row(serde_json::json!({"title": "a"}));
    let (asked, end) = port.run_step(Vec::new(), || port.create("Ticket", &data));
    assert_eq!(asked, Err(AWAITING_HOST.to_string()));
    assert_eq!(end.pending.as_slice(), &[IoRequest::Random { bytes: 16 }]);
    assert!(end.store_asks.is_empty());
    let (_, end) = port.run_step(vec![draw(7)], || port.create("Ticket", &data));
    let id = id_of(&end.store_asks[0].request);
    assert!(is_uuid_v4(&id), "{id}");
    assert_ne!(id, "Ticket-1");
    let verb = crate::store_driver::verb_for(&end.store_asks[0].request, &end.store_asks[0].key, &|_| None).unwrap();
    assert_eq!(orbital_core_base::json_value::value_to_json(&verb.payload)["batch"]["ops"][0]["data"]["id"], id.as_str());
    let created = Exchange::new(
        end.store_asks[0].request.clone(),
        IoAnswer::Wrote(Ok(vec![RowWriteOutcome::Created { row: create_of(&end.store_asks[0].request).clone() }])),
    );
    let (done, end) = port.run_step(vec![draw(7), created], || port.create("Ticket", &data));
    assert_eq!(done.unwrap().as_object().unwrap().get("id").and_then(Value::as_str), Some(id.as_str()));
    assert!(end.store_asks.is_empty() && end.diverged.is_none(), "{:?}", end.diverged);
}

#[test]
fn two_kernels_minting_for_the_same_entity_draw_different_ids() {
    let data = row(serde_json::json!({"title": "a"}));
    let minted = |fill: u8| {
        let port = bound_port();
        let (_, end) = port.run_step(vec![draw(fill)], || port.create("Ticket", &data));
        id_of(&end.store_asks[0].request)
    };
    assert_ne!(minted(1), minted(2));
}

#[test]
fn control_a_create_with_an_id_passes_it_through_unchanged() {
    let port = bound_port();
    let data = row(serde_json::json!({"id": "t9", "title": "a"}));
    let (_, end) = port.run_step(Vec::new(), || port.create("Ticket", &data));
    assert_eq!(create_of(&end.store_asks[0].request), &data);
}

#[test]
fn two_id_less_creates_in_one_step_mint_distinct_ids_and_replay_keeps_each() {
    let port = bound_port();
    let data = row(serde_json::json!({"title": "a"}));
    let both = |port: &IoPort| (port.create("Ticket", &data), port.create("Ticket", &data));
    let created = |request: &IoRequest| {
        Exchange::new(request.clone(), IoAnswer::Wrote(Ok(vec![RowWriteOutcome::Created { row: create_of(request).clone() }])))
    };
    let (_, end) = port.run_step(vec![draw(1)], || both(&port));
    let first = end.store_asks[0].request.clone();
    let (_, end) = port.run_step(vec![draw(1), created(&first), draw(2)], || both(&port));
    assert!(end.diverged.is_none(), "{:?}", end.diverged);
    let second = end.store_asks[0].request.clone();
    assert_ne!(id_of(&first), id_of(&second));
    assert!(is_uuid_v4(&id_of(&first)) && is_uuid_v4(&id_of(&second)));
    let journal = vec![draw(1), created(&first), draw(2), created(&second)];
    let (_, end) = port.run_step(journal, || both(&port));
    assert!(end.diverged.is_none() && end.store_asks.is_empty() && end.pending.is_empty(), "{:?}", end.diverged);
}

#[test]
fn a_journal_that_recorded_the_create_without_a_draw_keeps_its_id() {
    let port = bound_port();
    let data = row(serde_json::json!({"title": "a"}));
    let (_, end) = port.run_step(vec![draw(3)], || port.create("Ticket", &data));
    let request = end.store_asks[0].request.clone();
    let created = Exchange::new(
        request.clone(),
        IoAnswer::Wrote(Ok(vec![RowWriteOutcome::Created { row: create_of(&request).clone() }])),
    );
    let (done, end) = port.run_step(vec![created], || port.create("Ticket", &data));
    assert_eq!(done.unwrap().as_object().unwrap().get("id").and_then(Value::as_str), Some(id_of(&request).as_str()));
    assert!(end.pending.is_empty() && end.diverged.is_none(), "{:?}", end.diverged);
}

#[test]
fn the_kernel_store_mints_the_same_id_for_an_id_less_create() {
    let memory = crate::entity_source::InMemoryEntityCache::new();
    let data = row(serde_json::json!({"title": "a"}));
    let made = memory.create("Ticket", &data).unwrap();
    assert_eq!(made.as_object().unwrap().get("id").and_then(Value::as_str), Some("Ticket-1"));
}

#[test]
fn a_bound_entity_s_declared_collection_is_what_the_kernel_sends_its_driver() {
    let port = bound_port();
    port.bind_collection("Ticket", "graph_nodes");
    assert_eq!(port.collection_of("Ticket").as_deref(), Some("graph_nodes"));
    assert_eq!(port.collection_of("Note"), None);
}
