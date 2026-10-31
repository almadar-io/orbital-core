//! A store-bound fetch or persist whose step suspends on host I/O logs a suspend at debug, never an ERROR;
//! a real store failure still logs ERROR.
use super::*;
use crate::resumable::{Exchange, IoAnswer, IoRequest};
use crate::row_query::{RowWriteBatch, RowWriteFailureMode, RowWriteOp};
use orbital_core_base::effect_types::Effect;

fn bound_port() -> IoPort {
    let port = IoPort::new(Arc::new(crate::resumable::HostAnswered), None, None);
    port.bind_store("Ticket", "Rows");
    port
}

fn take() -> IoRequest {
    IoRequest::WriteRows {
        batch: RowWriteBatch {
            ops: vec![RowWriteOp::Delete {
                entity_type: "Ticket".into(),
                id: "t1".into(),
                guard: None,
            }],
            on_failure: RowWriteFailureMode::Stop,
        },
    }
}

fn bound_executor(port: &Arc<IoPort>) -> EffectExecutor {
    let rows = Arc::new(EntitySourceHandler {
        source: port.clone(),
        io: port.clone(),
        entity_policies: Arc::default(),
        entity_definitions: Arc::default(),
        written: Arc::default(),
    });
    EffectExecutor::server()
        .with_fetch_handler(rows.clone())
        .with_persistence(rows)
}

fn fetch_of(id: Option<&str>) -> Effect {
    Effect::Fetch {
        entity_type: "Ticket".into(),
        id: id.map(Value::string),
        filter: None,
        limit: None,
        offset: None,
        order_by: None,
        include: None,
        deliver: orbital_core_base::effect_types::FetchDeliver::Rows,
        total: None,
        nearest: None,
        emit: None,
    }
}

fn delete_of(id: &str) -> Effect {
    Effect::Persist {
        operation: orbital_core_base::effect_types::PersistOperation::Delete,
        entity_type: "Ticket".into(),
        data: Value::Null,
        id: Some(id.into()),
        emit: None,
        guard: None,
    }
}

fn run_logged(port: &Arc<IoPort>, journal: Vec<Exchange>, effect: &Effect) -> Vec<String> {
    let executor = bound_executor(port);
    let lines = super::set_eval_failure_tests::capture_lines(|| {
        port.run_step(journal, || {
            executor.execute(
                effect,
                &orbital_core_eval::evaluator::context::BindingContext::new(),
            )
        });
    });
    lines
}

fn errors(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("ERROR")).collect()
}

#[test]
fn a_store_bound_fetch_that_suspends_logs_no_error() {
    let port = Arc::new(bound_port());
    let lines = run_logged(&port, Vec::new(), &fetch_of(None));
    assert!(errors(&lines).is_empty(), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("DEBUG") && l.contains("Fetch effect suspended")),
        "{lines:?}"
    );
}

#[test]
fn a_store_bound_persist_that_suspends_logs_no_error() {
    let port = Arc::new(bound_port());
    let lines = run_logged(&port, Vec::new(), &delete_of("t1"));
    assert!(errors(&lines).is_empty(), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("DEBUG") && l.contains("Persist effect suspended")),
        "{lines:?}"
    );
}

#[test]
fn control_a_real_store_failure_on_a_fetch_still_logs_error() {
    let port = Arc::new(bound_port());
    let read = IoRequest::ReadRow {
        entity_type: "Ticket".into(),
        id: "t1".into(),
    };
    let down = Exchange::new(read, IoAnswer::Row(Err("store down".into())));
    let lines = run_logged(&port, vec![down], &fetch_of(Some("t1")));
    assert_eq!(errors(&lines).len(), 1, "{lines:?}");
    assert!(
        errors(&lines)[0].contains("Fetch effect failed")
            && errors(&lines)[0].contains("store down"),
        "{lines:?}"
    );
}

#[test]
fn control_a_real_store_failure_on_a_persist_still_logs_error() {
    let port = Arc::new(bound_port());
    let down = Exchange::new(take(), IoAnswer::Wrote(Err("store down".into())));
    let lines = run_logged(&port, vec![down], &delete_of("t1"));
    assert_eq!(errors(&lines).len(), 1, "{lines:?}");
    assert!(
        errors(&lines)[0].contains("Persist effect failed")
            && errors(&lines)[0].contains("store down"),
        "{lines:?}"
    );
}

#[test]
fn a_diverged_replay_still_logs_error_never_a_suspend() {
    let port = Arc::new(bound_port());
    let other = IoRequest::WriteRows {
        batch: RowWriteBatch {
            ops: vec![RowWriteOp::Delete {
                entity_type: "Ticket".into(),
                id: "t9".into(),
                guard: None,
            }],
            on_failure: RowWriteFailureMode::Stop,
        },
    };
    let recorded = Exchange::new(other, IoAnswer::Wrote(Ok(Vec::new())));
    let lines = run_logged(&port, vec![recorded], &delete_of("t1"));
    assert_eq!(errors(&lines).len(), 1, "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("suspended")), "{lines:?}");
}

fn batch_of(ids: &[&str]) -> Effect {
    let ops: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!(["create", "Note", { "id": id }]))
        .collect();
    Effect::PersistBatch {
        ops: serde_json::from_value(serde_json::Value::Array(ops)).unwrap(),
        emit: None,
    }
}

fn replayed_to_the_end(
    port: &Arc<IoPort>,
    effect: &Effect,
) -> (orbital_core_base::effect_types::EffectResult, Vec<IoRequest>) {
    let executor = bound_executor(port);
    let mut journal: Vec<Exchange> = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..8 {
        let (result, end) = port.run_step(journal.clone(), || {
            executor.execute(
                effect,
                &orbital_core_eval::evaluator::context::BindingContext::new(),
            )
        });
        if end.pending.is_empty() {
            return (result, seen);
        }
        for request in end.pending {
            let answer = match &request {
                IoRequest::CreateRow { data, .. } => IoAnswer::Row(Ok(data.clone())),
                IoRequest::ReadRow { .. } => IoAnswer::Row(Ok(Value::Null)),
                IoRequest::WriteRows { batch } => IoAnswer::Wrote(Ok(batch
                    .ops
                    .iter()
                    .map(|op| match op {
                        RowWriteOp::Create { data, .. } => RowWriteOutcome::Created { row: data.clone() },
                        _ => RowWriteOutcome::NotFound,
                    })
                    .collect())),
                other => panic!("unexpected ask {other:?}"),
            };
            seen.push(request.clone());
            journal.push(Exchange::new(request, answer));
        }
    }
    panic!("the replay never settled: {seen:?}");
}

#[test]
fn control_a_one_op_batch_replayed_with_each_answer_succeeds() {
    let port = Arc::new(IoPort::new(Arc::new(crate::resumable::HostAnswered), None, None));
    let (result, seen) = replayed_to_the_end(&port, &batch_of(&["a"]));
    assert!(result.is_executed(), "{result:?} after {seen:?}");
}

#[test]
fn a_two_op_batch_replayed_with_each_answer_succeeds() {
    let port = Arc::new(IoPort::new(Arc::new(crate::resumable::HostAnswered), None, None));
    let (result, seen) = replayed_to_the_end(&port, &batch_of(&["a", "b"]));
    assert!(result.is_executed(), "{result:?} after {seen:?}");
}

fn bound_batch_of(ids: &[&str]) -> Effect {
    let ops: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!(["create", "Ticket", { "id": id }]))
        .collect();
    Effect::PersistBatch {
        ops: serde_json::from_value(serde_json::Value::Array(ops)).unwrap(),
        emit: None,
    }
}

fn replayed_store_to_the_end(
    port: &Arc<IoPort>,
    effect: &Effect,
) -> (orbital_core_base::effect_types::EffectResult, Vec<IoRequest>) {
    let executor = bound_executor(port);
    let mut journal: Vec<Exchange> = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..8 {
        let (result, end) = port.run_step(journal.clone(), || {
            executor.execute(
                effect,
                &orbital_core_eval::evaluator::context::BindingContext::new(),
            )
        });
        let asks: Vec<IoRequest> = end
            .pending
            .iter()
            .cloned()
            .chain(end.store_asks.iter().map(|a| a.request.clone()))
            .collect();
        if asks.is_empty() {
            return (result, seen);
        }
        for request in asks {
            let answer = match &request {
                IoRequest::ReadRow { .. } => IoAnswer::Row(Ok(Value::Null)),
                IoRequest::WriteRows { batch } => IoAnswer::Wrote(Ok(batch
                    .ops
                    .iter()
                    .map(|op| match op {
                        RowWriteOp::Create { data, .. } => RowWriteOutcome::Created { row: data.clone() },
                        _ => RowWriteOutcome::NotFound,
                    })
                    .collect())),
                other => panic!("unexpected ask {other:?}"),
            };
            seen.push(request.clone());
            journal.push(Exchange::new(request, answer));
        }
    }
    panic!("the replay never settled: {seen:?}");
}

#[test]
fn control_a_one_op_batch_on_a_store_bound_entity_replays_to_success() {
    let port = Arc::new(bound_port());
    let (result, seen) = replayed_store_to_the_end(&port, &bound_batch_of(&["a"]));
    assert!(result.is_executed(), "{result:?} after {seen:?}");
}

#[test]
fn a_two_op_batch_on_a_store_bound_entity_replays_to_success() {
    let port = Arc::new(bound_port());
    let (result, seen) = replayed_store_to_the_end(&port, &bound_batch_of(&["a", "b"]));
    assert!(result.is_executed(), "{result:?} after {seen:?}");
}

#[test]
fn a_two_op_batch_on_a_store_bound_entity_with_full_capabilities_replays_to_success() {
    let port = Arc::new(bound_port());
    port.set_write_capabilities("Ticket", crate::row_query::WriteCapabilities::full());
    let (result, seen) = replayed_store_to_the_end(&port, &bound_batch_of(&["a", "b"]));
    assert!(result.is_executed(), "{result:?} after {seen:?}");
}

#[test]
fn a_batch_op_that_only_awaits_host_io_is_never_compensated_or_logged_as_an_error() {
    let port = Arc::new(IoPort::new(Arc::new(crate::resumable::HostAnswered), None, None));
    let mut outcome = None;
    let lines = super::set_eval_failure_tests::capture_lines(|| {
        outcome = Some(replayed_to_the_end(&port, &batch_of(&["a", "b"])));
    });
    let (result, _) = outcome.unwrap();
    assert!(result.is_executed(), "{result:?}");
    assert!(errors(&lines).is_empty(), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("batch-compensated")), "{lines:?}");
}

#[test]
fn control_a_batch_op_the_host_really_refuses_is_compensated_and_answers_its_failure() {
    let port = Arc::new(IoPort::new(Arc::new(crate::resumable::HostAnswered), None, None));
    let executor = bound_executor(&port);
    let effect = batch_of(&["a", "b"]);
    let read = |id: &str| Exchange::new(IoRequest::ReadRow { entity_type: "Note".into(), id: id.into() }, IoAnswer::Row(Ok(Value::Null)));
    let create = |id: &str, answer: Result<Value, String>| {
        let data: Value = serde_json::from_value(serde_json::json!({ "id": id })).unwrap();
        Exchange::new(IoRequest::CreateRow { entity_type: "Note".into(), data: data.clone() }, IoAnswer::Row(answer.map(|_| data)))
    };
    let journal = vec![read("a"), create("a", Ok(Value::Null)), read("b"), create("b", Err("disk full".into()))];
    let (result, _) = port.run_step(journal, || {
        executor.execute(&effect, &orbital_core_eval::evaluator::context::BindingContext::new())
    });
    assert!(result.is_failed(), "{result:?}");
    assert!(result.error.unwrap().contains("disk full"));
}
