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
