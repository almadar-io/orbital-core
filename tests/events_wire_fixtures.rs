//! §165 wire-mirror gate: every case of the canonical `events-wire` fixture set
//! (`@almadar/core` `fixtures/events-wire/<case>/{request,response}.json`, generated
//! from core's types, read from the installed core by `orbital_core::fixtures`) must
//! deserialize into `EventRequest`/`EventResponse` and re-serialize to the same JSON
//! value (byte-equal modulo key order). A core field or discriminant the Rust mirror
//! does not carry goes red here, not in a native shell's `apply_server_response`.

use orbital_core::fixtures::event_wire_cases;
use orbital_core::{EventRequest, EventResponse};

#[test]
fn every_fixture_case_round_trips_through_the_rust_mirror() {
    assert!(
        !event_wire_cases().is_empty(),
        "no baked events-wire cases — run `almadar-sync rust`"
    );
    for case in event_wire_cases() {
        for (file, raw, parse) in [
            (
                "request.json",
                case.request(),
                (|v: serde_json::Value| {
                    serde_json::from_value::<EventRequest>(v)
                        .map(|r| serde_json::to_value(r).unwrap())
                })
                    as fn(serde_json::Value) -> serde_json::Result<serde_json::Value>,
            ),
            ("response.json", case.response(), |v| {
                serde_json::from_value::<EventResponse>(v).map(|r| serde_json::to_value(r).unwrap())
            }),
        ] {
            let original: serde_json::Value = serde_json::from_str(raw).unwrap();
            let back = parse(original.clone()).unwrap_or_else(|e| {
                panic!(
                    "{}/{file}: does not deserialize into the Rust mirror: {e}",
                    case.name
                )
            });
            assert_eq!(
                back, original,
                "{}/{file}: Rust mirror re-serializes differently",
                case.name
            );
        }
    }
}
