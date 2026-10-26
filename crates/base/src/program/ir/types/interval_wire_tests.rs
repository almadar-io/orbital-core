use super::OirInterval;

/// G-ORBITAL-CORE-001: `orb` sends the module to its shell program as JSON; a cron tick
/// must survive that trip (an internally tagged newtype variant cannot be serialized).
#[test]
fn a_cron_interval_round_trips_as_json() {
    let cron = OirInterval::Cron { expression: "0 9 * * *".to_string() };
    let json = serde_json::to_value(&cron).expect("a cron interval serializes");
    assert_eq!(json, serde_json::json!({ "type": "cron", "expression": "0 9 * * *" }));
    assert_eq!(serde_json::from_value::<OirInterval>(json).unwrap(), cron);
}

#[test]
fn control_the_fixed_intervals_keep_their_wire_form() {
    for (interval, json) in [
        (OirInterval::Milliseconds { value: 5 }, serde_json::json!({ "type": "milliseconds", "value": 5 })),
        (OirInterval::Seconds { value: 2 }, serde_json::json!({ "type": "seconds", "value": 2 })),
        (OirInterval::Minutes { value: 1 }, serde_json::json!({ "type": "minutes", "value": 1 })),
        (OirInterval::Frame, serde_json::json!({ "type": "frame" })),
    ] {
        assert_eq!(serde_json::to_value(&interval).unwrap(), json);
        assert_eq!(serde_json::from_value::<OirInterval>(json).unwrap(), interval);
    }
}
