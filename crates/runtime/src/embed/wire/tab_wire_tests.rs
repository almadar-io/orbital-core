use super::*;
use orbital_core_base::tab::{TabAsk, TabCommand, TabFailure, TabFailureKind};

#[test]
fn a_tab_ask_and_its_answer_cross_the_wire_in_the_shape_a_browser_host_reads() {
    let request = IoRequest::Tab {
        ask: TabAsk::Command {
            tab: 4,
            command: TabCommand::Act {
                element_id: "el-1".to_string(),
                action: orbital_core_base::tab::TabAction::Click,
                text: None,
            },
        },
        key: Some("job".to_string()),
    };
    let wire = IoRequestWire::from(&request);
    let json = serde_json::to_value(&wire).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "kind": "tab",
            "ask": { "kind": "command", "tab": 4, "command": { "kind": "act", "elementId": "el-1", "action": "click" } },
            "key": "job"
        })
    );
    let back: IoRequestWire = serde_json::from_value(json).expect("parses");
    assert_eq!(IoRequest::from(back), request);
}

#[test]
fn a_tab_answer_round_trips_whether_it_settles_or_fails() {
    for answer in [
        IoAnswer::Tab(Ok(Value::Bool(true))),
        IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::NotReady, "Tab 3 has no page script"))),
    ] {
        let json = serde_json::to_value(IoAnswerWire::from(&answer)).expect("serializes");
        let back: IoAnswerWire = serde_json::from_value(json).expect("parses");
        assert_eq!(IoAnswer::from(back), answer);
    }
    let failed = serde_json::to_value(IoAnswerWire::from(&IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::NotFound, "gone"))))).unwrap();
    assert_eq!(failed, serde_json::json!({ "kind": "tab", "result": { "error": { "kind": "not-found", "message": "gone" } } }));
}
