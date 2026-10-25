#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::entity_source::{EntitySource, InMemoryEntityCache};
use crate::row_query::{QueryCapabilities, RowWriteOp, WriteCapabilities};
use orbital_core_base::value::Value;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeMap;

fn parse_checked<T: DeserializeOwned>(text: &str, contract: &str) -> Result<T, String> {
    let raw: serde_json::Value = parse(text)?;
    match contract {
        "EntityRow" => validate_entity_row(&raw)?,
        "RowWriteOp" => validate_row_write_op(&raw)?,
        "RowWriteBatch" => {
            let ops = raw.get("ops").and_then(serde_json::Value::as_array)
                .ok_or_else(|| "invalid RowWriteBatch: `ops` must be an array".to_string())?;
            for op in ops { validate_row_write_op(op)?; }
        }
        _ => return Err(format!("unknown memory store input type: {contract}")),
    }
    serde_json::from_value(raw).map_err(|error| format!("parse memory store input: {error}"))
}

fn validate_entity_row(row: &serde_json::Value) -> Result<(), String> {
    let object = row.as_object().ok_or_else(|| "EntityRow must be an object".to_string())?;
    if let Some(id) = object.get("id") {
        if !id.is_string() {
            return Err("EntityRow.id must be a string when present".to_string());
        }
    }
    Ok(())
}

fn validate_row_write_op(op: &serde_json::Value) -> Result<(), String> {
    let object = op.as_object().ok_or_else(|| "RowWriteOp must be an object".to_string())?;
    if object.get("guard").is_some_and(serde_json::Value::is_null) {
        return Err("RowWriteOp.guard must be omitted or a predicate, not null".to_string());
    }
    if let Some(data) = object.get("data") { validate_entity_row(data)?; }
    Ok(())
}

#[derive(Default)]
pub struct MemoryStoreApi {
    source: InMemoryEntityCache,
}

fn parse<T: DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("parse memory store input: {e}"))
}

fn json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize memory store result: {e}"))
}

impl MemoryStoreApi {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get_by_id(&self, entity_type: &str, id: &str) -> Result<String, String> {
        json(&self.source.find(entity_type, id)?.as_deref())
    }
    pub fn list(&self, entity_type: &str) -> Result<String, String> {
        let rows = self.source.list(entity_type)?;
        json(&rows.iter().map(|row| row.as_ref()).collect::<Vec<_>>())
    }
    pub fn create(&self, entity_type: &str, data_json: &str) -> Result<String, String> {
        json(
            &self
                .source
                .create(entity_type, &parse_checked::<Value>(data_json, "EntityRow")?)?,
        )
    }
    pub fn update(&self, entity_type: &str, id: &str, data_json: &str) -> Result<String, String> {
        let op = RowWriteOp::Update {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: parse_checked(data_json, "EntityRow")?,
            guard: None,
        };
        json(
            &self
                .source
                .write_where(&op)
                .ok_or("memory store does not support conditional writes")??,
        )
    }
    pub fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        self.source.delete(entity_type, id)
    }
    pub fn query_page(&self, entity_type: &str, query_json: &str) -> Result<String, String> {
        json(
            &self
                .source
                .query_page(entity_type, &parse(query_json)?)
                .ok_or("memory store does not support row queries")??,
        )
    }
    pub fn query_ids(&self, entity_type: &str, query_json: &str) -> Result<String, String> {
        json(
            &self
                .source
                .query_ids(entity_type, &parse(query_json)?)
                .ok_or("memory store does not support id queries")??,
        )
    }
    pub fn write_where(&self, op_json: &str) -> Result<String, String> {
        json(
            &self
                .source
                .write_where(&parse_checked(op_json, "RowWriteOp")?)
                .ok_or("memory store does not support conditional writes")??,
        )
    }
    pub fn write_batch(&self, batch_json: &str) -> Result<String, String> {
        json(
            &self
                .source
                .write_batch(&parse_checked(batch_json, "RowWriteBatch")?)
                .ok_or("memory store does not support write batches")??,
        )
    }
    pub fn query_capabilities(&self) -> Result<String, String> {
        json(&QueryCapabilities::full())
    }
    pub fn write_capabilities(&self) -> Result<String, String> {
        let mut capabilities = WriteCapabilities::full();
        capabilities.rekey = Some(true);
        json(&capabilities)
    }
    pub fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.source.bind_collection(entity_type, collection);
    }
    pub fn clear(&self) {
        self.source.clear();
    }
    pub fn snapshot(&self) -> Result<String, String> {
        let rows: Result<BTreeMap<_, _>, String> = self
            .source
            .entity_types()
            .into_iter()
            .map(|name| {
                self.source.list(&name).map(|rows| {
                    (
                        name,
                        rows.into_iter()
                            .map(|row| (*row).clone())
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .collect();
        json(&rows?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value as Json};

    fn decoded(result: Result<String, String>) -> Json {
        serde_json::from_str(&result.unwrap()).unwrap()
    }

    #[test]
    fn crud_aliases_and_missing_updates_preserve_native_rows() {
        let store = MemoryStoreApi::new();
        store.bind_collection("Note", "notes");
        store.bind_collection("SharedNote", "notes");
        assert_eq!(decoded(store.get_by_id("Note", "absent")), Json::Null);
        assert_eq!(
            decoded(store.update("Note", "absent", r#"{"body":"ignored"}"#)),
            json!({"kind":"not-found"})
        );
        assert_eq!(decoded(store.list("Note")), json!([]));
        assert_eq!(
            decoded(store.create("Note", r#"{"body":"first"}"#)),
            json!({"id":"Note-1","body":"first"})
        );
        assert_eq!(
            decoded(store.update("SharedNote", "Note-1", r#"{"title":"kept","id":"wrong"}"#))
                ["kind"],
            "updated"
        );
        assert_eq!(
            decoded(store.get_by_id("Note", "Note-1")),
            json!({"id":"Note-1","body":"first","title":"kept"})
        );
        assert_eq!(
            decoded(store.snapshot())["notes"].as_array().unwrap().len(),
            1
        );
        store.delete("SharedNote", "Note-1").unwrap();
        assert_eq!(decoded(store.get_by_id("Note", "Note-1")), Json::Null);
        store.delete("Note", "absent").unwrap();
    }

    #[test]
    fn guarded_atomic_and_stop_writes_have_the_native_outcomes() {
        let store = MemoryStoreApi::new();
        store.create("Note", r#"{"id":"a","active":true}"#).unwrap();
        let refused = json!({"kind":"update","entityType":"Note","id":"a","data":{"active":false},"guard":{"kind":"compare","field":"active","op":"==","value":false}});
        assert_eq!(
            decoded(store.write_where(&refused.to_string())),
            json!({"kind":"refused"})
        );
        for (mode, first, retained) in [("atomic", "rolled-back", false), ("stop", "created", true)]
        {
            let batch = json!({"onFailure":mode,"ops":[{"kind":"create","entityType":"Note","data":{"id":"b"}}, refused, {"kind":"delete","entityType":"Note","id":"a"}]});
            let outcomes = decoded(store.write_batch(&batch.to_string()));
            assert_eq!(outcomes.as_array().unwrap().len(), 2);
            assert_eq!(outcomes[0]["kind"], first);
            assert_eq!(outcomes[1]["kind"], "refused");
            assert_eq!(!decoded(store.get_by_id("Note", "b")).is_null(), retained);
            assert!(!decoded(store.get_by_id("Note", "a")).is_null());
        }
        assert_eq!(
            decoded(
                store.write_where(r#"{"kind":"create","entityType":"Note","data":{"id":"a"}}"#)
            ),
            json!({"kind":"duplicate-id"})
        );
    }

    #[test]
    fn queries_share_policy_filter_window_ids_and_nearest_semantics() {
        let store = MemoryStoreApi::new();
        for row in [
            json!({"id":"a","rank":1,"visible":true,"vector":[1,0]}),
            json!({"id":"b","rank":2,"visible":true,"vector":[0,1]}),
            json!({"id":"c","rank":3,"visible":false,"vector":[1,0]}),
        ] {
            store.create("Note", &row.to_string()).unwrap();
        }
        let query = json!({"policy":{"kind":"compare","field":"visible","op":"==","value":true},"filter":{"kind":"compare","field":"rank","op":">=","value":1},"order":{"field":"rank","direction":"desc"},"limit":1});
        let page = decoded(store.query_page("Note", &query.to_string()));
        assert_eq!(page["total"], 2);
        assert_eq!(page["rows"][0]["id"], "b");
        assert_eq!(
            decoded(store.query_ids("Note", &query.to_string())),
            json!({"ids":["b"],"total":2})
        );
        let nearest = json!({"policy":{"kind":"compare","field":"visible","op":"==","value":true},"nearest":{"field":"vector","vector":[1,0],"k":1}});
        let page = decoded(store.query_page("Note", &nearest.to_string()));
        assert_eq!(page["rows"][0]["id"], "a");
        assert_eq!(page["total"], 1);
        assert!(store
            .query_page(
                "Note",
                r#"{"nearest":{"field":"vector","vector":[0,0],"k":1}}"#
            )
            .is_err());
        assert_eq!(decoded(store.query_capabilities())["nearest"], true);
        assert_eq!(decoded(store.write_capabilities())["atomicBatch"], true);
    }

    #[test]
    fn malformed_inputs_do_not_mutate_rows_and_clear_keeps_aliases() {
        let store = MemoryStoreApi::new();
        store.bind_collection("Note", "notes");
        assert!(store.create("Note", "{").is_err());
        assert!(store.write_where(r#"{"kind":"bad"}"#).is_err());
        assert!(store.write_batch(r#"{"ops":[]}"#).is_err());
        assert_eq!(decoded(store.snapshot()), json!({}));
        store.create("Note", "{}").unwrap();
        store.clear();
        assert_eq!(decoded(store.snapshot()), json!({}));
        assert_eq!(decoded(store.create("Note", "{}"))["id"], "Note-2");
        assert_eq!(
            decoded(store.snapshot())["notes"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn raw_native_type_null_guards_refuse_before_any_batch_prefix_write() {
        for kind in ["update", "delete", "rekey"] {
            for mode in ["atomic", "stop"] {
                let store = MemoryStoreApi::new();
                store.create("Note", r#"{"id":"a","body":"kept"}"#).unwrap();
                let before = decoded(store.snapshot());
                let op = json!({"kind":kind,"entityType":"Note","id":"a",
                    "newId":"b","data":{"body":"changed"},"guard":null});
                assert!(store.write_where(&op.to_string()).is_err(), "{kind}: null guard");
                assert_eq!(decoded(store.snapshot()), before);
                let batch = json!({"onFailure":mode,"ops":[
                    {"kind":"create","entityType":"Note","data":{"id":"prefix"}},op]});
                assert!(store.write_batch(&batch.to_string()).is_err(), "{kind}: {mode}");
                assert_eq!(decoded(store.snapshot()), before);
            }
        }
    }

    #[test]
    fn raw_native_type_nonstring_row_ids_refuse_before_any_batch_prefix_write() {
        for id in [Json::Null, json!(7), json!(true), json!([]), json!({})] {
            for kind in ["create", "update"] {
                for mode in ["atomic", "stop"] {
                    let store = MemoryStoreApi::new();
                    store.create("Note", r#"{"id":"a","body":"kept"}"#).unwrap();
                    let before = decoded(store.snapshot());
                    let op = json!({"kind":kind,"entityType":"Note","id":"a",
                        "data":{"id":id,"body":"changed"}});
                    assert!(store.write_where(&op.to_string()).is_err(), "{kind}: {id}");
                    assert_eq!(decoded(store.snapshot()), before);
                    let batch = json!({"onFailure":mode,"ops":[
                        {"kind":"create","entityType":"Note","data":{"id":"prefix"}},op]});
                    assert!(store.write_batch(&batch.to_string()).is_err(), "{kind}: {id}: {mode}");
                    assert_eq!(decoded(store.snapshot()), before);
                }
            }
        }
    }

    #[test]
    fn raw_native_type_convenience_writes_refuse_present_nonstring_ids() {
        for id in [Json::Null, json!(7), json!(true), json!([]), json!({})] {
            let store = MemoryStoreApi::new();
            store.create("Note", r#"{"id":"a","body":"kept"}"#).unwrap();
            let before = decoded(store.snapshot());
            let data = json!({"id":id,"body":"changed"}).to_string();
            assert!(store.create("Note", &data).is_err(), "create: {id}");
            assert_eq!(decoded(store.snapshot()), before);
            assert!(store.update("Note", "a", &data).is_err(), "update: {id}");
            assert_eq!(decoded(store.snapshot()), before);
        }
    }

    #[test]
    fn raw_native_type_missing_guards_and_null_comparison_values_keep_existing_outcomes() {
        let store = MemoryStoreApi::new();
        assert_eq!(decoded(store.create("Note", r#"{"active":true}"#))["id"], "Note-1");
        let update = json!({"kind":"update","entityType":"Note","id":"Note-1",
            "data":{"body":"kept"}});
        assert_eq!(decoded(store.write_where(&update.to_string()))["kind"], "updated");
        let null_value = json!({"kind":"compare","field":"active","op":"==","value":null});
        let mut guarded = update.clone();
        guarded["guard"] = null_value;
        assert_eq!(decoded(store.write_where(&guarded.to_string())), json!({"kind":"refused"}));
        let before = decoded(store.snapshot());
        let missing_value = json!({"kind":"compare","field":"active","op":"=="});
        for guard in [missing_value.clone(), json!({"kind":"not","of":missing_value})] {
            guarded["guard"] = guard;
            assert!(store.write_where(&guarded.to_string()).is_err());
            let batch = json!({"onFailure":"stop","ops":[
                {"kind":"create","entityType":"Note","data":{"id":"prefix"}},guarded]});
            assert!(store.write_batch(&batch.to_string()).is_err());
            assert_eq!(decoded(store.snapshot()), before);
        }
    }
}
