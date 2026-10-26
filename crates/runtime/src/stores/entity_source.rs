//! Entity source abstraction for the runtime kernel.
//!
//! An `EntitySource` hides the difference between a client-side in-memory cache
//! and a server-side persistence adapter so the kernel can read and write entity
//! data without knowing which environment it is running in.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::row_query::{
    apply_write, touched_ids, CreateIds, QueryRun, RowIdPage, RowPage, RowTable, RowWriteBatch,
    RowWriteFailureMode, RowWriteOp, RowWriteOutcome, ScanControl,
};
use orbital_core_base::row_set::{row_field, RowCompareOp, RowPredicate, RowQuery};
use orbital_core_base::schema::types::EntityIndex;
use orbital_core_base::value::Value;
use orbital_core_base::sync::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What a store answers to a pushed-down row query.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryAnswer<T> {
    /// The store ran the query.
    Rows(T),
    /// The store cannot carry this query, so the kernel reads rows instead. The reason is the word the std
    /// row-store drivers answer in `STORE_UNSUPPORTED`: the store's status word or the planner's decline word.
    Declined(String),
    /// The store tried and failed.
    Failed(String),
}

/// The decline reason of a store that pushes no query down.
pub const NO_QUERY_PUSHDOWN: &str = "the store pushes no query down";

impl<T> QueryAnswer<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> QueryAnswer<U> {
        match self {
            QueryAnswer::Rows(rows) => QueryAnswer::Rows(f(rows)),
            QueryAnswer::Declined(reason) => QueryAnswer::Declined(reason),
            QueryAnswer::Failed(message) => QueryAnswer::Failed(message),
        }
    }

    /// The rows. Panics on a decline or a failure; for tests that expect the store to run the query.
    pub fn unwrap_rows(self) -> T {
        match self {
            QueryAnswer::Rows(rows) => rows,
            QueryAnswer::Declined(reason) => panic!("the query was declined: {reason}"),
            QueryAnswer::Failed(message) => panic!("the query failed: {message}"),
        }
    }

    /// The failure message. Panics on rows or a decline; for tests that expect a failure.
    pub fn unwrap_failure(self) -> String {
        match self {
            QueryAnswer::Failed(message) => message,
            QueryAnswer::Rows(_) => panic!("the query ran, expected a failure"),
            QueryAnswer::Declined(reason) => panic!("the query was declined: {reason}"),
        }
    }

    /// The decline reason. Panics on rows or a failure; for tests that expect a decline.
    pub fn unwrap_declined(self) -> String {
        match self {
            QueryAnswer::Declined(reason) => reason,
            QueryAnswer::Rows(_) => panic!("the query ran, expected a decline"),
            QueryAnswer::Failed(message) => panic!("the query failed: {message}"),
        }
    }
}

impl<T> From<Result<T, String>> for QueryAnswer<T> {
    fn from(run: Result<T, String>) -> Self {
        match run {
            Ok(rows) => QueryAnswer::Rows(rows),
            Err(message) => QueryAnswer::Failed(message),
        }
    }
}

/// Abstract source for entity data. Rows travel as `Arc<Value>`, so a read hands out a pointer, not a copy.
pub trait EntitySource: Send + Sync {
    /// Read an entity by type and ID.
    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String>;

    /// The row, or `None` when no such row exists. A store that tracks absence says so here; the default
    /// reads the row and takes `Ok(Null)` as absent, so an `Err` stays a failure rather than a miss.
    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        let row = self.read(entity_type, id)?;
        Ok(if row.is_null() { None } else { Some(row) })
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
    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String>;

    /// Offer each row of `entity_type` to `visit` until it says stop. The default lists the table and
    /// wraps each row; a store that holds `Arc` rows overrides it and hands out pointers.
    fn scan(
        &self,
        entity_type: &str,
        visit: &mut dyn FnMut(&Arc<Value>) -> ScanControl,
    ) -> Result<(), String> {
        for row in self.list(entity_type)? {
            if visit(&row) == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    /// The page of rows matching `query.policy AND query.filter`, ordered and windowed. The default runs the
    /// query over [`Self::scan`]; a store with a native query overrides it. A source that cannot answer declines with its reason.
    fn query_page(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowPage> {
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish().into()
    }

    /// The ids of the page `query_page` would return, no row cloned.
    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowIdPage> {
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish_ids().into()
    }

    /// One conditional write (the row is tested and written in one step). `None` = not supported here.
    fn write_where(&self, _op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        None
    }

    /// A batch of conditional writes: `atomic` all-or-nothing, `stop` ending at the first failure. `None` = not
    /// supported here; nothing was written.
    fn write_batch(&self, _batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        None
    }

    /// Bind `entity_type` to its `persistent:` collection. Entities bound to one
    /// collection are one table (JS `MockPersistenceAdapter`, the TS shell); a
    /// source that already keys its storage by collection ignores this.
    fn bind_collection(&self, _entity_type: &str, _collection: &str) {}

    /// Declare the access paths of `entity_type`'s rows (`@index`). A path changes a query's cost, never its
    /// result; a source with no use for them ignores the call.
    fn declare_indexes(&self, _entity_type: &str, _indexes: &[EntityIndex]) {}

    /// The stores this source holds rows in (a bound collection, else an entity name); a remote store does not
    /// enumerate its collections and answers none.
    fn entity_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// In-memory entity cache used by the client runtime.
#[derive(Debug, Default)]
pub struct InMemoryEntityCache {
    /// Rows per entity in insertion order, as the JS store's `Map`.
    data: RwLock<HashMap<String, RowTable>>,
    /// Mints ids for id-less creates, one counter across entity types (JS `InMemoryPersistence`).
    id_counter: std::sync::atomic::AtomicUsize,
    /// Entity name -> its bound collection: the store key it reads and writes.
    collections: RwLock<HashMap<String, String>>,
    /// A create naming a taken id fails (JS `MockPersistenceAdapter`, the database drivers) instead of merging.
    refuse_duplicate_ids: bool,
    /// The declared access paths per store key. Always locked after `data`.
    indexes: RwLock<HashMap<String, StoreIndexes>>,
    /// Rows offered to a query so far.
    visited: std::sync::atomic::AtomicUsize,
}

/// One index key component: a scalar the equality comparison can match (`same_value`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum IndexKey {
    Text(String),
    Flag(bool),
    Number(u64),
}

fn index_key(value: &Value) -> Option<IndexKey> {
    match value {
        Value::String(s) => Some(IndexKey::Text(s.clone())),
        Value::Bool(b) => Some(IndexKey::Flag(*b)),
        Value::Number(n) if !n.is_nan() => {
            let canonical = if *n == 0.0 { 0.0 } else { *n };
            Some(IndexKey::Number(f64::to_bits(canonical)))
        }
        _ => None,
    }
}

/// The access paths of one store: for each declared path and each prefix length of its fields, the ids of the
/// rows by their values on that prefix. A row whose value on a field is not a matchable scalar is on no
/// entry from that field on, so an equality lookup never needs it.
#[derive(Debug, Default)]
struct StoreIndexes {
    paths: Vec<Vec<String>>,
    levels: Vec<Vec<HashMap<Vec<IndexKey>, HashSet<String>>>>,
}

impl StoreIndexes {
    fn new(declared: &[EntityIndex]) -> Self {
        let mut paths: Vec<Vec<String>> = Vec::new();
        for index in declared.iter().filter(|i| !i.fields.is_empty()) {
            if !paths.contains(&index.fields) {
                paths.push(index.fields.clone());
            }
        }
        let levels = paths.iter().map(|p| vec![HashMap::new(); p.len()]).collect();
        Self { paths, levels }
    }

    fn prefixes(path: &[String], row: &Value) -> Vec<Vec<IndexKey>> {
        let mut key: Vec<IndexKey> = Vec::new();
        let mut out = Vec::new();
        for field in path {
            match row_field(row, field).and_then(index_key) {
                Some(k) => key.push(k),
                None => break,
            }
            out.push(key.clone());
        }
        out
    }

    fn add(&mut self, id: &str, row: &Value) {
        for (path, levels) in self.paths.iter().zip(self.levels.iter_mut()) {
            for (prefix, level) in Self::prefixes(path, row).into_iter().zip(levels.iter_mut()) {
                level.entry(prefix).or_default().insert(id.to_string());
            }
        }
    }

    fn remove(&mut self, id: &str, row: &Value) {
        for (path, levels) in self.paths.iter().zip(self.levels.iter_mut()) {
            for (prefix, level) in Self::prefixes(path, row).into_iter().zip(levels.iter_mut()) {
                if let Some(ids) = level.get_mut(&prefix) {
                    ids.remove(id);
                    if ids.is_empty() {
                        level.remove(&prefix);
                    }
                }
            }
        }
    }

    fn clear(&mut self) {
        for level in self.levels.iter_mut().flatten() {
            level.clear();
        }
    }

    /// The smallest set of ids that holds every row `query` can admit, from an equality its policy or filter
    /// requires on the leading fields of a path; `None` when no path is covered.
    fn candidates(&self, query: &RowQuery) -> Option<Vec<String>> {
        let mut required: Vec<(&str, &Value)> = Vec::new();
        for predicate in [&query.policy, &query.filter].into_iter().flatten() {
            required_equalities(predicate, &mut required);
        }
        let mut best: Option<Vec<String>> = None;
        for (path, levels) in self.paths.iter().zip(&self.levels) {
            let mut key: Vec<IndexKey> = Vec::new();
            for field in path {
                let Some(k) = required
                    .iter()
                    .find(|(f, _)| *f == field.as_str())
                    .and_then(|(_, v)| index_key(v))
                else {
                    break;
                };
                key.push(k);
            }
            if key.is_empty() {
                continue;
            }
            let ids: Vec<String> = levels[key.len() - 1]
                .get(&key)
                .map(|ids| ids.iter().cloned().collect())
                .unwrap_or_default();
            if best.as_ref().is_none_or(|b| ids.len() < b.len()) {
                best = Some(ids);
            }
        }
        best
    }
}

/// The `field == constant` comparisons every admitted row must satisfy: the predicate itself, or a conjunct of it.
fn required_equalities<'p>(predicate: &'p RowPredicate, out: &mut Vec<(&'p str, &'p Value)>) {
    match predicate {
        RowPredicate::Compare { field, op: RowCompareOp::Eq, value } if !value.is_null() => {
            out.push((field.as_str(), value));
        }
        RowPredicate::All { of } => {
            for p in of {
                required_equalities(p, out);
            }
        }
        _ => {}
    }
}

impl InMemoryEntityCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server's mock store: a create naming a taken id fails, as `MockPersistenceAdapter` and the database
    /// drivers refuse it.
    pub fn refusing_duplicate_ids() -> Self {
        Self {
            refuse_duplicate_ids: true,
            ..Self::default()
        }
    }

    /// The store an entity's rows live in: its bound collection, else its name.
    fn store_key(&self, entity_type: &str) -> String {
        self.collections
            .read()
            .get(entity_type)
            .cloned()
            .unwrap_or_else(|| entity_type.to_string())
    }

    fn next_id(&self, entity_type: &str) -> String {
        let n = self
            .id_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        format!("{entity_type}-{n}")
    }

    /// Move a row between the index entries of `key`: out of `before`'s, into `after`'s.
    fn reindex(&self, key: &str, id: &str, before: Option<&Arc<Value>>, after: Option<&Arc<Value>>) {
        if matches!((before, after), (Some(b), Some(a)) if Arc::ptr_eq(b, a)) {
            return;
        }
        if let Some(store) = self.indexes.write().get_mut(key) {
            if let Some(before) = before {
                store.remove(id, before);
            }
            if let Some(after) = after {
                store.add(id, after);
            }
        }
    }

    /// Offer `visit` the rows `query` can admit: those a declared path names for it, else every row. The query
    /// still tests each one, so an index narrows the scan and nothing else.
    fn offer_candidates(
        &self,
        entity_type: &str,
        query: &RowQuery,
        visit: &mut dyn FnMut(&Arc<Value>) -> ScanControl,
    ) -> Result<(), String> {
        let key = self.store_key(entity_type);
        let narrowed: Option<Vec<Arc<Value>>> = {
            let data = self.data.read();
            let indexes = self.indexes.read();
            indexes.get(&key).and_then(|store| store.candidates(query)).map(|ids| {
                ids.iter()
                    .filter_map(|id| data.get(&key).and_then(|table| table.get(id)).cloned())
                    .collect()
            })
        };
        let Some(rows) = narrowed else {
            return self.scan(entity_type, visit);
        };
        let mut offered = 0;
        for row in &rows {
            offered += 1;
            if visit(row) == ScanControl::Stop {
                break;
            }
        }
        self.visited.fetch_add(offered, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// How many rows queries have been offered so far (a scan counts every row it hands out).
    pub fn rows_visited(&self) -> usize {
        self.visited.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn indexed(&self, key: &str) -> bool {
        self.indexes.read().contains_key(key)
    }

    /// Apply `ops` under one write lock; `atomic` restores every table on a failure. The index entries of every
    /// row the batch touched are brought to the rows' final state.
    fn apply_batch(&self, batch: &RowWriteBatch) -> Vec<RowWriteOutcome> {
        let mut cache = self.data.write();
        let mut before: Vec<(String, String, Option<Arc<Value>>)> = Vec::new();
        for op in &batch.ops {
            let key = self.store_key(op.entity_type());
            if !self.indexed(&key) {
                continue;
            }
            for id in touched_ids(op) {
                if !before.iter().any(|(k, i, _)| *k == key && *i == id) {
                    let row = cache.get(&key).and_then(|t| t.get(&id)).cloned();
                    before.push((key.clone(), id, row));
                }
            }
        }
        let outcomes = self.apply_batch_on(&mut cache, batch);
        if !batch.ops.iter().any(|op| self.indexed(&self.store_key(op.entity_type()))) {
            return outcomes;
        }
        for (op, outcome) in batch.ops.iter().zip(&outcomes) {
            let key = self.store_key(op.entity_type());
            if let RowWriteOutcome::Created { row } = outcome {
                if let Some(id) = row.get_path("id").and_then(|v| v.as_str()) {
                    if !before.iter().any(|(k, i, _)| *k == key && i.as_str() == id) {
                        before.push((key, id.to_string(), None));
                    }
                }
            }
        }
        for (key, id, was) in &before {
            let now = cache.get(key).and_then(|t| t.get(id)).cloned();
            self.reindex(key, id, was.as_ref(), now.as_ref());
        }
        outcomes
    }

    fn apply_batch_on(&self, cache: &mut HashMap<String, RowTable>, batch: &RowWriteBatch) -> Vec<RowWriteOutcome> {
        let snapshot = (batch.on_failure == RowWriteFailureMode::Atomic && batch.ops.len() > 1)
            .then(|| cache.clone());
        let mut outcomes: Vec<RowWriteOutcome> = Vec::with_capacity(batch.ops.len());
        for op in &batch.ops {
            let entity = op.entity_type().to_string();
            let key = self.store_key(&entity);
            let existed = cache.contains_key(&key);
            let table = cache.entry(key.clone()).or_default();
            let mut mint = || self.next_id(&entity);
            let outcome = apply_write(table, op, &mut CreateIds::Mint(&mut mint));
            let applied = outcome.applied();
            if !applied && !existed {
                cache.remove(&key);
            }
            outcomes.push(outcome);
            if !applied {
                if batch.on_failure == RowWriteFailureMode::Atomic {
                    if let Some(snapshot) = snapshot {
                        *cache = snapshot;
                    }
                    let failing = outcomes.pop().unwrap_or(RowWriteOutcome::RolledBack);
                    let mut undone: Vec<RowWriteOutcome> = outcomes
                        .iter()
                        .map(|_| RowWriteOutcome::RolledBack)
                        .collect();
                    undone.push(failing);
                    return undone;
                }
                break;
            }
        }
        outcomes
    }
}

impl EntitySource for InMemoryEntityCache {
    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        let cache = self.data.read();
        cache
            .get(&self.store_key(entity_type))
            .and_then(|type_cache| type_cache.get(id))
            .cloned()
            .ok_or_else(|| format!("Entity not found in cache: {}:{}", entity_type, id))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
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
        let type_cache = cache.entry(key.clone()).or_default();

        // Read-modify-write: merge `data` into the EXISTING record so a partial write
        // (a `set` of one field, a `persist` of a slice, a re-fetch of a projection)
        // can never drop untouched fields. A full replace here was the root cause of
        // `@entity.<untouched-field>` reverting to a literal token on the next render
        // (e.g. board `current*` / tabs `activeTab`). Incoming wins on conflict; the
        // canonical `id` is stamped last. With no existing record — or a non-object on
        // either side — this degrades to a plain insert (first write / scalar entity).
        let merged = match (type_cache.get(id).map(|row| &**row), data) {
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
        let row = Arc::new(record.clone());
        let was = type_cache.insert(id.to_string(), row.clone());
        self.reindex(&key, id, was.as_ref(), Some(&row));
        Ok(record)
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let id = data
            .as_object()
            .and_then(|obj| obj.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.next_id(entity_type));
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
        tracing::debug!(target: orbital_core_base::logging::ns::SERVER_DATA, entity = %entity_type, id = %id, fields = field_count, "data:create");
        if !self.refuse_duplicate_ids {
            return self.write(entity_type, &id, data);
        }
        let key = self.store_key(entity_type);
        let mut cache = self.data.write();
        let table = cache.entry(key.clone()).or_default();
        if table.contains_key(&id) {
            return Err(format!("Entity {entity_type} with id {id} already exists"));
        }
        let mut record = data.clone();
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.clone()));
        }
        let row = Arc::new(record.clone());
        table.insert(id.clone(), row.clone());
        self.reindex(&key, &id, None, Some(&row));
        Ok(record)
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let key = self.store_key(entity_type);
        let mut cache = self.data.write();
        if let Some(removed) = cache.get_mut(&key).and_then(|type_cache| type_cache.shift_remove(id)) {
            self.reindex(&key, id, Some(&removed), None);
        }
        Ok(())
    }

    fn clear(&self) {
        let mut data = self.data.write();
        data.clear();
        for store in self.indexes.write().values_mut() {
            store.clear();
        }
    }

    fn declare_indexes(&self, entity_type: &str, indexes: &[EntityIndex]) {
        let key = self.store_key(entity_type);
        let data = self.data.read();
        let mut store = StoreIndexes::new(indexes);
        for (id, row) in data.get(&key).into_iter().flatten() {
            store.add(id, row);
        }
        self.indexes.write().insert(key, store);
    }

    fn count(&self) -> usize {
        self.data
            .read()
            .values()
            .map(|type_cache| type_cache.len())
            .sum()
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        let key = self.store_key(entity_type);
        let cache = self.data.read();
        let rows: Vec<Arc<Value>> = cache
            .get(&key)
            .map(|type_cache| type_cache.values().cloned().collect())
            .unwrap_or_default();
        tracing::debug!(
            target: orbital_core_base::logging::ns::SERVER_DATA,
            entity = %entity_type,
            count = rows.len(),
            known_types = ?cache.keys().collect::<Vec<_>>(),
            "data:list",
        );
        Ok(rows)
    }

    /// Pointer copies under the read lock; the lock is released before any predicate runs.
    fn scan(
        &self,
        entity_type: &str,
        visit: &mut dyn FnMut(&Arc<Value>) -> ScanControl,
    ) -> Result<(), String> {
        let rows: Vec<Arc<Value>> = self
            .data
            .read()
            .get(&self.store_key(entity_type))
            .map(|type_cache| type_cache.values().cloned().collect())
            .unwrap_or_default();
        let mut offered = 0;
        for row in &rows {
            offered += 1;
            if visit(row) == ScanControl::Stop {
                break;
            }
        }
        self.visited.fetch_add(offered, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowPage> {
        let mut run = QueryRun::new(query);
        if let Err(e) = self.offer_candidates(entity_type, query, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish().into()
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowIdPage> {
        let mut run = QueryRun::new(query);
        if let Err(e) = self.offer_candidates(entity_type, query, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish_ids().into()
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        let batch = RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop,
        };
        Some(Ok(self
            .apply_batch(&batch)
            .pop()
            .unwrap_or(RowWriteOutcome::RolledBack)))
    }

    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        Some(Ok(self.apply_batch(batch)))
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections
            .write()
            .insert(entity_type.to_string(), collection.to_string());
    }

    fn entity_types(&self) -> Vec<String> {
        self.data.read().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
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
}
