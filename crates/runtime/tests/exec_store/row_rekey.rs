use orbital_core_base::value::Value;
use orbital_core_runtime::embed::memory::MemoryStoreApi;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::row_query::{
    apply_write, apply_write_batch, CreateIds, RowTable, RowWriteBatch, RowWriteFailureMode,
    RowWriteOp, RowWriteOutcome, WriteCapabilities,
};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn table(rows: Json) -> RowTable {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let value: Value = serde_json::from_value(row.clone()).unwrap();
            (row["id"].as_str().unwrap().to_string(), Arc::new(value))
        })
        .collect()
}

fn rows(table: &RowTable) -> Json {
    serde_json::to_value(table.values().map(|row| &**row).collect::<Vec<_>>()).unwrap()
}

fn op(value: Json) -> RowWriteOp {
    serde_json::from_value(value).expect("canonical rekey wire")
}
fn rekey(id: &str, new_id: &str) -> RowWriteOp {
    op(json!({"kind":"rekey", "entityType":"Item", "id":id, "newId":new_id}))
}
fn apply(table: &mut RowTable, op: &RowWriteOp) -> RowWriteOutcome {
    apply_write(table, op, &mut CreateIds::Strict, &mut orbital_core_base::intern::KeyVocabulary::default())
}

#[test]
fn rekey_keeps_order_whole_winner_timestamps_and_unrelated_arcs() {
    for initial in [
        json!([{"id":"a","createdAt":"a0","updatedAt":"a1","source":true},{"id":"middle","manual":7.0},{"id":"c"}]),
        json!([{"id":"middle","manual":7.0},{"id":"a","createdAt":"a0","updatedAt":"a1","source":true},{"id":"c"}]),
        json!([{"id":"middle","manual":7.0},{"id":"c"},{"id":"a","createdAt":"a0","updatedAt":"a1","source":true}]),
    ] {
        let mut data = table(initial.clone());
        let middle = data["middle"].clone();
        let outcome = apply(&mut data, &rekey("a", "b"));
        let mut expected = initial;
        expected
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["id"] == "a")
            .unwrap()["id"] = json!("b");
        assert_eq!(rows(&data), expected);
        assert!(Arc::ptr_eq(&middle, &data["middle"]));
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            json!({"kind":"updated","row":{"id":"b","createdAt":"a0","updatedAt":"a1","source":true}})
        );
    }
    for (initial, expected) in [
        (
            json!([{"id":"a","createdAt":"a0","updatedAt":"a1","loserOnly":1.0},{"id":"middle","manual":7.0},{"id":"b","createdAt":"b0","updatedAt":"b1","winnerOnly":2.0}]),
            json!([{"id":"b","createdAt":"b0","updatedAt":"b1","winnerOnly":2.0},{"id":"middle","manual":7.0}]),
        ),
        (
            json!([{"id":"b","createdAt":"b0","updatedAt":"b1","loserOnly":2.0},{"id":"middle","manual":7.0},{"id":"a","createdAt":"a0","updatedAt":"a1","winnerOnly":1.0}]),
            json!([{"id":"b","createdAt":"a0","updatedAt":"a1","winnerOnly":1.0},{"id":"middle","manual":7.0}]),
        ),
    ] {
        let mut data = table(initial);
        let middle = data["middle"].clone();
        let outcome = apply(&mut data, &rekey("a", "b"));
        assert_eq!(rows(&data), expected);
        assert!(Arc::ptr_eq(&middle, &data["middle"]));
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            json!({"kind":"updated","row":expected[0]})
        );
    }
}

#[test]
fn rekey_guards_missing_empty_identity_and_invalid_wire_do_not_mutate() {
    let initial = json!([{"id":"a","enabled":true,"createdAt":"exact"},{"id":"b"}]);
    let mut data = table(initial.clone());
    assert_eq!(
        apply(&mut data, &rekey("missing", "b")),
        RowWriteOutcome::NotFound
    );
    assert!(matches!(
        apply(&mut data, &rekey("a", "")),
        RowWriteOutcome::Failed { .. }
    ));
    let guard = json!({"kind":"compare","field":"enabled","op":"==","value":false});
    let refused =
        op(json!({"kind":"rekey","entityType":"Item","id":"a","newId":"b","guard":guard}));
    assert_eq!(apply(&mut data, &refused), RowWriteOutcome::Refused);
    let same = apply(&mut data, &rekey("a", "a"));
    assert_eq!(
        serde_json::to_value(same).unwrap(),
        json!({"kind":"updated","row":initial[0]})
    );
    let guarded = op(
        json!({"kind":"rekey","entityType":"Item","id":"a","newId":"a","guard":{"kind":"compare","field":"enabled","op":"==","value":true}}),
    );
    assert_eq!(
        serde_json::to_value(apply(&mut data, &guarded)).unwrap(),
        json!({"kind":"updated","row":initial[0],"before":initial[0]})
    );
    for malformed in [
        json!({"kind":"rekey","entityType":"Item","id":"a","newId":3}),
        json!({"kind":"rekey","entityType":"Item","id":"a"}),
        json!({"kind":"other","entityType":"Item"}),
    ] {
        assert!(serde_json::from_value::<RowWriteOp>(malformed).is_err());
    }
    assert_eq!(rows(&data), initial);
}

#[test]
fn rekey_batches_preserve_ordered_prefix_and_restore_exact_atomic_snapshot() {
    let initial = json!([{"id":"a","createdAt":"old","manual":9.0},{"id":"middle"}]);
    let ops = vec![
        op(json!({"kind":"update","entityType":"Item","id":"a","data":{"owner":"first"}})),
        rekey("a", "b"),
        op(json!({"kind":"update","entityType":"Item","id":"b","data":{"owner":"second"}})),
        rekey("missing", "nope"),
    ];
    let mut atomic = table(initial.clone());
    let batch = RowWriteBatch {
        ops: ops.clone(),
        on_failure: RowWriteFailureMode::Atomic,
    };
    assert_eq!(
        apply_write_batch(&mut atomic, &batch, &mut CreateIds::Strict, &mut orbital_core_base::intern::KeyVocabulary::default()),
        vec![
            RowWriteOutcome::RolledBack,
            RowWriteOutcome::RolledBack,
            RowWriteOutcome::RolledBack,
            RowWriteOutcome::NotFound
        ]
    );
    assert_eq!(rows(&atomic), initial);
    let mut stop = table(initial);
    let outcomes = apply_write_batch(
        &mut stop,
        &RowWriteBatch {
            ops,
            on_failure: RowWriteFailureMode::Stop,
        },
        &mut CreateIds::Strict,
        &mut orbital_core_base::intern::KeyVocabulary::default(),
    );
    assert_eq!(outcomes.len(), 4);
    assert_eq!(outcomes[3], RowWriteOutcome::NotFound);
    assert_eq!(
        rows(&stop),
        json!([{"id":"b","createdAt":"old","manual":9.0,"owner":"second"},{"id":"middle"}])
    );
}

#[test]
fn cache_rekey_aliases_share_table_and_atomic_rollback_crosses_tables() {
    let cache = InMemoryEntityCache::new();
    cache.bind_collection("Item", "items");
    cache.bind_collection("Alias", "items");
    cache
        .write(
            "Item",
            "a",
            &serde_json::from_value(json!({"id":"a","manual":9.0})).unwrap(),
        )
        .unwrap();
    let moved = cache.write_where(&rekey("a", "b")).unwrap().unwrap();
    assert!(moved.applied());
    assert_eq!(
        serde_json::to_value(&*cache.read("Alias", "b").unwrap()).unwrap(),
        json!({"id":"b","manual":9.0})
    );
    let batch = RowWriteBatch {
        ops: vec![
            rekey("b", "c"),
            op(json!({"kind":"rekey","entityType":"Other","id":"missing","newId":"x"})),
        ],
        on_failure: RowWriteFailureMode::Atomic,
    };
    assert_eq!(
        cache.write_batch(&batch).unwrap().unwrap(),
        vec![RowWriteOutcome::RolledBack, RowWriteOutcome::NotFound]
    );
    assert!(cache.find("Item", "c").unwrap().is_none());
    assert!(cache.find("Alias", "b").unwrap().is_some());
    assert_eq!(cache.entity_types(), vec!["items"]);
}

#[test]
fn failed_writes_do_not_create_an_observable_empty_table() {
    let store = MemoryStoreApi::new();
    let before = store.snapshot().unwrap();
    for (operation, outcome) in [
        (
            json!({"kind":"delete","entityType":"Missing","id":"x"}),
            "not-found",
        ),
        (
            json!({"kind":"rekey","entityType":"Missing","id":"x","newId":"y"}),
            "not-found",
        ),
        (
            json!({"kind":"rekey","entityType":"Missing","id":"x","newId":""}),
            "failed",
        ),
    ] {
        let result = store.write_where(&operation.to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<Json>(&result).unwrap()["kind"],
            outcome
        );
        assert_eq!(store.snapshot().unwrap(), before);
    }
}

#[test]
fn only_native_memory_advertises_rekey_capability() {
    let historical = serde_json::to_value(WriteCapabilities::full()).unwrap();
    assert!(historical.get("rekey").is_none());
    let restored: WriteCapabilities = serde_json::from_value(historical).unwrap();
    assert_eq!(restored.rekey, None);
    let native: Json =
        serde_json::from_str(&MemoryStoreApi::new().write_capabilities().unwrap()).unwrap();
    assert_eq!(native["rekey"], true);
}
