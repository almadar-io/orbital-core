use super::*;
use crate::entity_source::InMemoryEntityCache;

fn row(id: &str) -> Value {
    serde_json::from_value(serde_json::json!({ "id": id })).unwrap()
}

fn create(entity: &str, id: &str) -> RowWriteOp {
    RowWriteOp::Create { entity_type: entity.to_string(), data: row(id) }
}

fn batch(ops: Vec<RowWriteOp>) -> RowWriteBatch {
    RowWriteBatch { ops, on_failure: RowWriteFailureMode::Stop }
}

fn ids(port: &IoPort, entity: &str) -> Vec<String> {
    let mut ids: Vec<String> = port
        .list(entity)
        .unwrap()
        .iter()
        .map(|r| r.get_path("id").and_then(|v| v.as_str().map(str::to_string)).unwrap())
        .collect();
    ids.sort();
    ids
}

fn ports() -> (IoPort, Arc<InMemoryEntityCache>) {
    let host = Arc::new(InMemoryEntityCache::new());
    let port = IoPort::new(host.clone() as Arc<dyn EntitySource>, None, None).with_runtime_rows(true);
    port.claim_runtime("Card");
    (port, host)
}

#[test]
fn a_runtime_row_written_with_write_where_is_listed_and_never_reaches_the_host_source() {
    let (port, host) = ports();
    assert!(matches!(port.write_where(&create("Card", "c1")), Some(Ok(_))));
    assert_eq!(ids(&port, "Card"), ["c1"]);
    assert_eq!(host.count(), 0);
}

#[test]
fn control_a_persistent_row_written_with_write_where_is_listed() {
    let (port, host) = ports();
    assert!(matches!(port.write_where(&create("Note", "n1")), Some(Ok(_))));
    assert_eq!(ids(&port, "Note"), ["n1"]);
    assert_eq!(host.count(), 1);
}

#[test]
fn a_runtime_batch_is_listed_and_never_reaches_the_host_source() {
    let (port, host) = ports();
    let written = port.write_batch(&batch(vec![create("Card", "c1"), create("Card", "c2")]));
    assert!(matches!(written, Some(Ok(ref outcomes)) if outcomes.len() == 2), "{written:?}");
    assert_eq!(ids(&port, "Card"), ["c1", "c2"]);
    assert_eq!(host.count(), 0);
}

#[test]
fn a_runtime_row_written_in_a_step_is_listed_in_the_same_step_and_in_a_replay() {
    let (port, host) = ports();
    let step = |port: &IoPort| (port.write_where(&create("Card", "c1")), ids(port, "Card"));
    let (first, end) = port.run_step(Vec::new(), || step(&port));
    assert!(matches!(first.0, Some(Ok(_))), "{:?}", first.0);
    assert_eq!(first.1, ["c1"]);
    assert!(end.pending.is_empty() && end.diverged.is_none());
    assert_eq!(host.count(), 0);
    let (replayed, end) = port.run_step(end.journal.clone(), || step(&port));
    assert!(matches!(replayed.0, Some(Ok(_))), "{:?}", replayed.0);
    assert_eq!(replayed.1, ["c1"]);
    assert!(end.pending.is_empty() && end.diverged.is_none());
}

#[test]
fn a_batch_mixing_runtime_and_host_rows_is_refused_and_writes_nothing() {
    let (port, host) = ports();
    let written = port.write_batch(&batch(vec![create("Card", "c1"), create("Note", "n1")]));
    assert!(matches!(written, Some(Err(_))), "{written:?}");
    assert!(ids(&port, "Card").is_empty());
    assert_eq!(host.count(), 0);
}
