//! Entity source abstraction for the runtime kernel.
//!
//! An `EntitySource` hides the difference between a client-side in-memory cache
//! and a server-side persistence adapter so the kernel can read and write entity
//! data without knowing which environment it is running in.

use crate::evaluator::Value;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Abstract source for entity data.
pub trait EntitySource: Send + Sync {
    /// Read an entity by type and ID.
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String>;

    /// The row, or `None` when no such row exists. A store that tracks absence says so here; the default
    /// reads the row and takes `Ok(Null)` as absent, so an `Err` stays a failure rather than a miss.
    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        match self.read(entity_type, id)? {
            Value::Null => Ok(None),
            row => Ok(Some(row)),
        }
    }

    /// Write entity data, creating or updating as needed.
    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String>;

    /// Create a new entity.
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String>;

    /// Delete an entity.
    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String>;

    /// Clear all stored entities.
    fn clear(&self);

    /// Count stored entities across all types.
    fn count(&self) -> usize;

    /// List all entities of a given type (empty vec if none).
    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String>;

    /// Bind `entity_type` to its `persistent:` collection. Entities bound to one
    /// collection are one table (JS `MockPersistenceAdapter`, the TS shell); a
    /// source that already keys its storage by collection ignores this.
    fn bind_collection(&self, _entity_type: &str, _collection: &str) {}
}

/// In-memory entity cache used by the client runtime.
#[derive(Debug, Default)]
pub struct InMemoryEntityCache {
    /// Rows per entity in insertion order, as the JS store's `Map`.
    data: RwLock<HashMap<String, indexmap::IndexMap<String, Value>>>,
    /// Mints ids for id-less creates, one counter across entity types (JS `InMemoryPersistence`).
    id_counter: std::sync::atomic::AtomicUsize,
    /// Entity name -> its bound collection: the store key it reads and writes.
    collections: RwLock<HashMap<String, String>>,
}

impl InMemoryEntityCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The store an entity's rows live in: its bound collection, else its name.
    fn store_key(&self, entity_type: &str) -> String {
        self.collections
            .read()
            .get(entity_type)
            .cloned()
            .unwrap_or_else(|| entity_type.to_string())
    }
}

impl EntitySource for InMemoryEntityCache {
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        let cache = self.data.read();
        cache
            .get(&self.store_key(entity_type))
            .and_then(|type_cache| type_cache.get(id))
            .cloned()
            .ok_or_else(|| format!("Entity not found in cache: {}:{}", entity_type, id))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        Ok(self
            .data
            .read()
            .get(&self.store_key(entity_type))
            .and_then(|type_cache| type_cache.get(id))
            .cloned())
    }

    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let key = self.store_key(entity_type);
        let mut cache = self.data.write();
        let type_cache = cache.entry(key).or_default();

        // Read-modify-write: merge `data` into the EXISTING record so a partial write
        // (a `set` of one field, a `persist` of a slice, a re-fetch of a projection)
        // can never drop untouched fields. A full replace here was the root cause of
        // `@entity.<untouched-field>` reverting to a literal token on the next render
        // (e.g. board `current*` / tabs `activeTab`). Incoming wins on conflict; the
        // canonical `id` is stamped last. With no existing record — or a non-object on
        // either side — this degrades to a plain insert (first write / scalar entity).
        let merged = match (type_cache.get(id), data) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                let mut obj = existing.clone();
                for (k, v) in incoming {
                    obj.insert(k.clone(), v.clone());
                }
                Value::Object(obj)
            }
            _ => data.clone(),
        };
        let mut record = merged;
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.to_string()));
        }
        type_cache.insert(id.to_string(), record.clone());
        Ok(record)
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let id = data
            .as_object()
            .and_then(|obj| obj.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let n = self
                    .id_counter
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                format!("{entity_type}-{n}")
            });
        let data = match data {
            Value::Object(obj) if !obj.contains_key("id") => {
                let mut obj = obj.clone();
                obj.insert("id".to_string(), Value::String(id.clone()));
                Value::Object(obj)
            }
            other => other.clone(),
        };
        let data = &data;
        let field_count = data.as_object().map(|o| o.len()).unwrap_or(0);
        tracing::debug!(target: crate::logging::ns::SERVER_DATA, entity = %entity_type, id = %id, fields = field_count, "data:create");
        self.write(entity_type, &id, data)
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let key = self.store_key(entity_type);
        let mut cache = self.data.write();
        if let Some(type_cache) = cache.get_mut(&key) {
            type_cache.shift_remove(id);
        }
        Ok(())
    }

    fn clear(&self) {
        self.data.write().clear();
    }

    fn count(&self) -> usize {
        self.data
            .read()
            .values()
            .map(|type_cache| type_cache.len())
            .sum()
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        let key = self.store_key(entity_type);
        let cache = self.data.read();
        let rows: Vec<Value> = cache
            .get(&key)
            .map(|type_cache| type_cache.values().cloned().collect())
            .unwrap_or_default();
        tracing::debug!(
            target: crate::logging::ns::SERVER_DATA,
            entity = %entity_type,
            count = rows.len(),
            known_types = ?cache.keys().collect::<Vec<_>>(),
            "data:list",
        );
        Ok(rows)
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections
            .write()
            .insert(entity_type.to_string(), collection.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = HashMap::new();
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
    /// the row's place, a delete closes the gap (`tools/parity-cascade` `fetch-payload`).
    #[test]
    fn list_keeps_insertion_order_through_update_and_delete() {
        let cache = InMemoryEntityCache::new();
        let ids = ["e", "a", "d", "b", "c", "f", "h", "g"];
        for id in ids {
            let mut row = HashMap::new();
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

        let mut patch = HashMap::new();
        patch.insert("n".to_string(), Value::Number(1.0));
        cache.write("Item", "d", &Value::Object(patch)).unwrap();
        cache.delete("Item", "b").unwrap();
        assert_eq!(listed(&cache), ["e", "a", "d", "c", "f", "h", "g"]);
    }
}
