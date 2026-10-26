use super::*;
use orbital_core_base::value::ValueMap;

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = ValueMap::default();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
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
                ("id", Value::String("d1".into())),
                ("owner", Value::String("u1".into())),
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
        Some(&Value::String("u1".into()))
    );
    cache
        .write(
            "PipelineDeal",
            "d1",
            &obj(&[("stage", Value::String("won".into()))]),
        )
        .expect("write");
    assert_eq!(
        cache
            .read("Deal", "d1")
            .expect("read")
            .as_object()
            .and_then(|o| o.get("stage")),
        Some(&Value::String("won".into()))
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
        .create("Deal", &obj(&[("id", Value::String("d1".into()))]))
        .expect("create");
    cache
        .create("Note", &obj(&[("id", Value::String("n1".into()))]))
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
                ("title", Value::String("Activity".to_string())),
                ("filterKind", Value::String("all".to_string())),
            ]),
        )
        .unwrap();

    // Partial update naming only `boards`.
    cache
        .write(
            "BoardView",
            "singleton",
            &obj(&[("boards", Value::Array(vec![Value::Number(1.0)]))]),
        )
        .unwrap();

    let got = cache.read("BoardView", "singleton").unwrap();
    let m = got.as_object().unwrap();
    assert_eq!(m["boards"], Value::Array(vec![Value::Number(1.0)])); // new field
    assert_eq!(m["title"], Value::String("Activity".to_string())); // preserved
    assert_eq!(m["filterKind"], Value::String("all".to_string())); // preserved
    assert_eq!(m["id"], Value::String("singleton".to_string())); // stamped
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
    assert_eq!(m["id"], Value::String("singleton".to_string()));
}

/// Rows list in insertion order, as the JS store's `Map` does: an update keeps
/// the row's place, a delete closes the gap (parity fixture `fetch-payload`).
#[test]
fn list_keeps_insertion_order_through_update_and_delete() {
    let cache = InMemoryEntityCache::new();
    let ids = ["e", "a", "d", "b", "c", "f", "h", "g"];
    for id in ids {
        let mut row = ValueMap::default();
        row.insert("id".to_string(), Value::String(id.to_string()));
        cache.create("Item", &Value::Object(row)).unwrap();
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
    patch.insert("n".to_string(), Value::Number(1.0));
    cache.write("Item", "d", &Value::Object(patch)).unwrap();
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
                ("id", Value::String("d1".into())),
                ("n", Value::Number(1.0)),
            ]),
        )
        .expect("first create");
    let err = cache
        .create("PipelineDeal", &obj(&[("id", Value::String("d1".into()))]))
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
                ("id", Value::String("d1".into())),
                ("n", Value::Number(1.0)),
            ]),
        )
        .unwrap();
    cache
        .create(
            "Deal",
            &obj(&[
                ("id", Value::String("d1".into())),
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
            .create(entity, &obj(&[("id", Value::String(id.into()))]))
            .unwrap();
    }
    let mut types = cache.entity_types();
    types.sort();
    assert_eq!(types, ["Note", "deals"]);
}
