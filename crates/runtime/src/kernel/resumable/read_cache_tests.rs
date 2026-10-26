use super::*;

fn port() -> IoPort {
    IoPort::new(Arc::new(HostAnswered), None, None)
}

fn row(id: &str) -> Value {
    serde_json::from_value(serde_json::json!({ "id": id })).unwrap()
}

fn read(id: &str) -> IoRequest {
    IoRequest::ReadRow { entity_type: "T".to_string(), id: id.to_string() }
}

fn read_answer(id: &str) -> Exchange {
    Exchange::new(read(id), IoAnswer::Row(Ok(row(id))))
}

#[test]
fn a_row_read_twice_in_a_dispatch_is_asked_once() {
    let port = port();
    let (found, end) = port.run_step(vec![read_answer("1")], || (port.find("T", "1"), port.find("T", "1")));
    assert!(matches!(&found, (Ok(Some(_)), Ok(Some(_)))), "{found:?}");
    assert!(end.pending.is_empty() && end.diverged.is_none());
}

#[test]
fn a_later_step_reads_what_an_earlier_one_was_answered_and_asks_only_for_a_new_row() {
    let port = port();
    let (_, first) = port.run_step(vec![read_answer("1")], || port.find("T", "1"));
    let (_, second) = port.run_step_reading(Vec::new(), first.reads, || (port.find("T", "1"), port.find("T", "2")));
    assert_eq!(second.pending.as_slice(), &[read("2")]);
}

#[test]
fn control_a_write_to_the_entity_makes_the_next_read_ask_again() {
    let port = port();
    let write = Exchange::new(
        IoRequest::WriteRow { entity_type: "T".to_string(), id: "1".to_string(), data: row("1") },
        IoAnswer::Row(Ok(row("1"))),
    );
    let (_, end) = port.run_step(vec![read_answer("1"), write], || {
        port.find("T", "1").unwrap();
        port.write("T", "1", &row("1")).unwrap();
        port.find("T", "1")
    });
    assert_eq!(end.pending.as_slice(), &[read("1")]);
}

#[test]
fn a_step_that_stops_for_the_host_keeps_none_of_its_reads() {
    let port = port();
    let (_, end) = port.run_step(vec![read_answer("1")], || (port.find("T", "1"), port.find("T", "2")));
    assert_eq!(end.pending.as_slice(), &[read("2")]);
    assert!(end.reads.is_empty(), "{:?}", end.reads.keys().collect::<Vec<_>>());
}
