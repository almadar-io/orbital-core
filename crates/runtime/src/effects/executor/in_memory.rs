//! `InMemoryPersistence`: the in-memory persistence and fetch handler.

use super::*;

/// In-memory persistence handler for testing
#[derive(Debug, Default)]
pub struct InMemoryPersistence {
    /// Rows per entity in insertion order, as the JS store's `Map`.
    pub(super) data: orbital_core_base::sync::RwLock<HashMap<String, orbital_core_base::collections::IndexMap<String, Value>>>,
    pub(super) next_id: std::sync::atomic::AtomicU64,
    pub(super) entity_policies: Option<Arc<RwLock<EntityAccessTable>>>,
}

impl InMemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_entity_policies(mut self, table: Arc<RwLock<EntityAccessTable>>) -> Self {
        self.entity_policies = Some(table);
        self
    }

    pub(super) fn generate_id(&self) -> String {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("id-{}", id)
    }
}

impl PersistenceHandler for InMemoryPersistence {
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let mut store = self.data.write();
        let collection = store.entry(entity_type.to_string()).or_default();

        let id = if let Some(obj) = data.as_object() {
            obj.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
        .unwrap_or_else(|| self.generate_id());

        let mut record = data.clone();
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".into(), Value::string(id.clone()));
        }

        collection.insert(id.clone(), record.clone());
        Ok(record)
    }

    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        let store = self.data.read();
        store
            .get(entity_type)
            .and_then(|c| c.get(id))
            .cloned()
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        Ok(self
            .data
            .read()
            .get(entity_type)
            .and_then(|c| c.get(id))
            .cloned())
    }

    fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let mut store = self.data.write();
        let collection = store
            .get_mut(entity_type)
            .ok_or_else(|| format!("Collection not found: {}", entity_type))?;

        let existing = collection
            .get_mut(id)
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))?;

        // Merge data
        if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), data.as_object()) {
            for (k, v) in new_obj {
                existing_obj.insert(k.clone(), v.clone());
            }
        }

        Ok(existing.clone())
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let mut store = self.data.write();
        let collection = store
            .get_mut(entity_type)
            .ok_or_else(|| format!("Collection not found: {}", entity_type))?;

        collection
            .shift_remove(id)
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))?;

        Ok(())
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        let store = self.data.read();
        Ok(store
            .get(entity_type)
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default())
    }
}

impl FetchHandler for InMemoryPersistence {
    fn fetch(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        _include: Option<&Vec<String>>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let mut entities: Vec<Value> = {
            let store = self.data.read();
            let collection = store.get(entity_type);
            match id {
                Some(id) => collection
                    .and_then(|c| c.get(id))
                    .cloned()
                    .into_iter()
                    .collect(),
                None => collection
                    .map(|c| c.values().cloned().collect())
                    .unwrap_or_default(),
            }
        };
        let policy = self
            .entity_policies
            .as_ref()
            .and_then(|table| table.read().get(entity_type).and_then(|p| p.read.clone()));
        let total = scope_fetched_rows(
            &mut entities,
            policy.as_ref(),
            filter,
            limit,
            offset,
            ctx,
            entity_type,
        );
        Ok(FetchPage {
            rows: entities,
            total,
        })
    }
}

/// Interpolate `@entity`/`@entity.*` string leaves in a fetch filter against
/// the ISSUING trait's context; the row-scoped `["object/get", "@entity", …]`
/// form is preserved verbatim (mirrors JS `interpolateFilterTraitRefs`).
pub(super) fn interpolate_filter_trait_refs(value: &Value, ctx: &BindingContext) -> Value {
    match value {
        Value::String(s) => {
            if s == "@entity" || s.starts_with("@entity.") {
                ctx.resolve_binding(s).unwrap_or(Value::Null)
            } else if s.starts_with("@config.") {
                // A knob whose value IS an `@entity` binding (a list-detail pair's `initialFilterValue`)
                // reads the issuing trait's entity, not the candidate row the filter runs over.
                match ctx.resolve_binding(s) {
                    Some(Value::String(inner))
                        if inner == "@entity" || inner.starts_with("@entity.") =>
                    {
                        ctx.resolve_binding(&inner).unwrap_or(Value::Null)
                    }
                    _ => value.clone(),
                }
            } else {
                value.clone()
            }
        }
        Value::Array(arr) => {
            if arr.len() >= 2
                && arr[0] == Value::string("object/get")
                && arr[1] == Value::string("@entity")
            {
                return value.clone();
            }
            Value::Array(
                arr.iter()
                    .map(|v| interpolate_filter_trait_refs(v, ctx))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}
