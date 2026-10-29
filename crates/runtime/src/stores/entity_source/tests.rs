use super::*;
use orbital_core_base::value::ValueMap;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::default();
    for (k, v) in pairs {
        m.insert((*k).to_string().into(), v.clone());
    }
    Value::object(m)
}

/// Entities bound to one `persistent:` collection share ONE store (JS
/// `MockPersistenceAdapter`, the TS shell): a row created through one is the
/// row the other lists, reads, writes and deletes.
#[test]
fn collection_mates_share_one_store() {
    let cache = InMemoryEntityCache::new();
    cache.bind_collection("Deal", "deals");
    cache.bind_collection("PipelineDeal", "deals");
    cache
        .create(
            "Deal",
            &obj(&[
                ("id", Value::string("d1")),
                ("owner", Value::string("u1")),
            ]),
        )
        .expect("create");
    let listed = cache.list("PipelineDeal").expect("list");
    assert_eq!(listed.len(), 1, "the mate sees the row");
    assert_eq!(
        cache
            .read("PipelineDeal", "d1")
            .expect("read")
            .as_object()
            .and_then(|o| o.get("owner")),
        Some(&Value::string("u1"))
    );
    cache
        .write(
            "PipelineDeal",
            "d1",
            &obj(&[("stage", Value::string("won"))]),
        )
        .expect("write");
    assert_eq!(
        cache
            .read("Deal", "d1")
            .expect("read")
            .as_object()
            .and_then(|o| o.get("stage")),
        Some(&Value::string("won"))
    );
    cache.delete("PipelineDeal", "d1").expect("delete");
    assert!(
        cache.list("Deal").expect("list").is_empty(),
        "deleted through the mate"
    );
}

#[test]
fn control_an_unbound_entity_keeps_its_own_store() {
    let cache = InMemoryEntityCache::new();
    cache.bind_collection("Deal", "deals");
    cache
        .create("Deal", &obj(&[("id", Value::string("d1"))]))
        .expect("create");
    cache
        .create("Note", &obj(&[("id", Value::string("n1"))]))
        .expect("create");
    assert_eq!(cache.list("Note").expect("list").len(), 1);
    assert_eq!(cache.list("Deal").expect("list").len(), 1);
    assert!(cache.list("Other").expect("list").is_empty());
    assert_eq!(cache.count(), 2);
}

/// Partial writes must preserve untouched fields. The replace-write bug dropped
/// every field a `set`/`persist` didn't name, breaking the next render's
/// `@entity.<untouched>` bindings.
#[test]
fn write_merges_into_existing_record() {
    let cache = InMemoryEntityCache::new();
    cache
        .write(
            "BoardView",
            "singleton",
            &obj(&[
                ("title", Value::string("Activity")),
                ("filterKind", Value::string("all")),
            ]),
        )
        .unwrap();

    // Partial update naming only `boards`.
    cache
        .write(
            "BoardView",
            "singleton",
            &obj(&[("boards", Value::array(vec![Value::Number(1.0)]))]),
        )
        .unwrap();

    let got = cache.read("BoardView", "singleton").unwrap();
    let m = got.as_object().unwrap();
    assert_eq!(m["boards"], Value::array(vec![Value::Number(1.0)])); // new field
    assert_eq!(m["title"], Value::string("Activity")); // preserved
    assert_eq!(m["filterKind"], Value::string("all")); // preserved
    assert_eq!(m["id"], Value::string("singleton")); // stamped
}

#[test]
fn write_incoming_wins_on_conflict() {
    let cache = InMemoryEntityCache::new();
    cache
        .write("E", "singleton", &obj(&[("x", Value::Number(1.0))]))
        .unwrap();
    cache
        .write("E", "singleton", &obj(&[("x", Value::Number(2.0))]))
        .unwrap();
    let got = cache.read("E", "singleton").unwrap();
    assert_eq!(got.as_object().unwrap()["x"], Value::Number(2.0));
}

#[test]
fn write_first_record_inserts() {
    let cache = InMemoryEntityCache::new();
    cache
        .write("E", "singleton", &obj(&[("a", Value::Bool(true))]))
        .unwrap();
    let got = cache.read("E", "singleton").unwrap();
    let m = got.as_object().unwrap();
    assert_eq!(m["a"], Value::Bool(true));
    assert_eq!(m["id"], Value::string("singleton"));
}

/// Rows list in insertion order, as the JS store's `Map` does: an update keeps
/// the row's place, a delete closes the gap (parity fixture `fetch-payload`).
#[test]
fn list_keeps_insertion_order_through_update_and_delete() {
    let cache = InMemoryEntityCache::new();
    let ids = ["e", "a", "d", "b", "c", "f", "h", "g"];
    for id in ids {
        let mut row = ValueMap::default();
        row.insert("id".into(), Value::string(id.to_string()));
        cache.create("Item", &Value::object(row)).unwrap();
    }
    let listed = |cache: &InMemoryEntityCache| -> Vec<String> {
        cache
            .list("Item")
            .unwrap()
            .iter()
            .map(|r| r.as_object().unwrap()["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(listed(&cache), ids);

    let mut patch = ValueMap::default();
    patch.insert("n".into(), Value::Number(1.0));
    cache.write("Item", "d", &Value::object(patch)).unwrap();
    cache.delete("Item", "b").unwrap();
    assert_eq!(listed(&cache), ["e", "a", "d", "c", "f", "h", "g"]);
}

#[test]
fn a_refusing_store_fails_a_create_on_a_taken_id() {
    let cache = InMemoryEntityCache::refusing_duplicate_ids();
    cache.bind_collection("PipelineDeal", "deals");
    cache.bind_collection("Deal", "deals");
    cache
        .create(
            "Deal",
            &obj(&[
                ("id", Value::string("d1")),
                ("n", Value::Number(1.0)),
            ]),
        )
        .expect("first create");
    let err = cache
        .create("PipelineDeal", &obj(&[("id", Value::string("d1"))]))
        .expect_err("the id is taken through the collection mate");
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        cache.read("Deal", "d1").unwrap().as_object().unwrap()["n"],
        Value::Number(1.0),
        "the refused create wrote nothing"
    );
    let minted = cache
        .create("Deal", &obj(&[]))
        .expect("an id-less create mints");
    assert!(minted.as_object().unwrap()["id"]
        .as_str()
        .is_some_and(|id| id != "d1"));
}

#[test]
fn control_the_client_cache_merges_a_create_on_a_taken_id() {
    let cache = InMemoryEntityCache::new();
    cache
        .create(
            "Deal",
            &obj(&[
                ("id", Value::string("d1")),
                ("n", Value::Number(1.0)),
            ]),
        )
        .unwrap();
    cache
        .create(
            "Deal",
            &obj(&[
                ("id", Value::string("d1")),
                ("m", Value::Number(2.0)),
            ]),
        )
        .expect("the client cache upserts");
    let row = cache.read("Deal", "d1").unwrap();
    assert_eq!(row.as_object().unwrap()["n"], Value::Number(1.0));
    assert_eq!(row.as_object().unwrap()["m"], Value::Number(2.0));
}

#[test]
fn entity_types_names_each_store_once() {
    let cache = InMemoryEntityCache::new();
    assert!(
        cache.entity_types().is_empty(),
        "control: an empty cache holds no store"
    );
    cache.bind_collection("Deal", "deals");
    cache.bind_collection("PipelineDeal", "deals");
    for (entity, id) in [("Deal", "d1"), ("PipelineDeal", "d2"), ("Note", "n1")] {
        cache
            .create(entity, &obj(&[("id", Value::string(id))]))
            .unwrap();
    }
    let mut types = cache.entity_types();
    types.sort();
    assert_eq!(types, ["Note", "deals"]);
}

/// G-ORBITAL-CORE-070: rows a store keeps share one allocation per field name (R3, interned `ValueMap` keys).
#[test]
fn rows_a_store_keeps_share_their_field_names() {
    let cache = InMemoryEntityCache::new();
    for id in ["a", "b", "c"] {
        cache
            .create("Deal", &obj(&[("id", Value::string(id)), ("owner", Value::string("u1"))]))
            .expect("create");
    }
    let rows = cache.list("Deal").expect("list");
    let key_of = |row: &Arc<Value>, name: &str| {
        row.as_object().unwrap().get_key_value(name).map(|(k, _)| k.clone()).unwrap()
    };
    for name in ["id", "owner"] {
        let first = key_of(&rows[0], name);
        for row in &rows[1..] {
            assert!(
                orbital_core_base::value::Shared::ptr_eq(&first, &key_of(row, name)),
                "`{name}` is one allocation across the rows"
            );
        }
    }
}

/// Control: names that differ are not merged, and the same text in another entity's rows is the same name.
#[test]
fn control_distinct_field_names_stay_distinct_and_equal_ones_are_shared_across_entities() {
    let cache = InMemoryEntityCache::new();
    cache.create("Deal", &obj(&[("id", Value::string("a")), ("owner", Value::string("u1"))])).unwrap();
    cache.create("Task", &obj(&[("id", Value::string("t")), ("title", Value::string("x"))])).unwrap();
    let deal = cache.list("Deal").unwrap().remove(0);
    let task = cache.list("Task").unwrap().remove(0);
    let deal_fields = deal.as_object().unwrap();
    let task_fields = task.as_object().unwrap();
    let (id_a, _) = deal_fields.get_key_value("id").unwrap();
    let (id_b, _) = task_fields.get_key_value("id").unwrap();
    assert!(orbital_core_base::value::Shared::ptr_eq(id_a, id_b));
    assert!(deal_fields.contains_key("owner") && !deal_fields.contains_key("title"));
    assert_eq!(deal_fields.len(), 2);
    assert_eq!(task_fields.len(), 2);
}

fn key_ptr(row: &Value, name: &str) -> orbital_core_base::value::Shared<String> {
    row.as_object().unwrap().get_key_value(name).map(|(k, _)| k.clone()).unwrap()
}

/// R3: the batch path of a store shares field names with the rows the store already keeps.
#[test]
fn rows_written_by_a_batch_share_field_names_with_the_stores_other_rows() {
    let cache = InMemoryEntityCache::new();
    cache.create("Deal", &obj(&[("id", Value::string("a")), ("owner", Value::string("u1"))])).unwrap();
    let batch: RowWriteBatch = serde_json::from_value(serde_json::json!({
        "ops": [{"kind": "create", "entityType": "Deal", "data": {"id": "b", "owner": "u2"}}],
        "onFailure": "stop"
    }))
    .unwrap();
    assert!(cache.write_batch(&batch).is_some());
    let rows = cache.list("Deal").unwrap();
    assert_eq!(rows.len(), 2);
    for name in ["id", "owner"] {
        assert!(orbital_core_base::value::Shared::ptr_eq(&key_ptr(&rows[0], name), &key_ptr(&rows[1], name)), "`{name}`");
    }
}

/// The batch helper the host-answered store uses: one vocabulary shares names across tables; a fresh one does not.
#[test]
fn apply_write_batch_across_shares_names_through_the_vocabulary_it_is_given() {
    use orbital_core_base::intern::KeyVocabulary;
    let make = || -> RowWriteBatch {
        serde_json::from_value(serde_json::json!({
            "ops": [
                {"kind": "create", "entityType": "A", "data": {"id": "1", "title": "x"}},
                {"kind": "create", "entityType": "B", "data": {"id": "2", "title": "y"}}
            ],
            "onFailure": "stop"
        }))
        .unwrap()
    };
    let run = |vocab: &mut KeyVocabulary| {
        let mut tables: HashMap<String, RowTable> = HashMap::new();
        super::super::row_query::apply_write_batch_across(&mut tables, &make(), &mut |_| CreateIds::Strict, vocab);
        (tables["A"]["1"].clone(), tables["B"]["2"].clone())
    };
    let (a, b) = run(&mut KeyVocabulary::default());
    assert!(orbital_core_base::value::Shared::ptr_eq(&key_ptr(&a, "title"), &key_ptr(&b, "title")));
    let (a2, _) = run(&mut KeyVocabulary::default());
    assert!(!orbital_core_base::value::Shared::ptr_eq(&key_ptr(&a, "title"), &key_ptr(&a2, "title")), "control: separate vocabularies do not share");
}
