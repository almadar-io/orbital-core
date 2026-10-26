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
    assert!(crate::store_driver::verb_for(&end.store_asks[0].request, &end.store_asks[0].key).is_some());
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
